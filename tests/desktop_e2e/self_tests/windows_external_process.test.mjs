import assert from "node:assert/strict";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import test from "node:test";
import { spawn } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm } from "node:fs/promises";

import {
  WindowsExternalProcessError,
  runWindowsExternalProcess,
} from "../drivers/windows_external_process.mjs";

const windowsTest = process.platform === "win32" ? test : test.skip;

async function temporaryExecution(context) {
  const parent = await mkdtemp(path.join(os.tmpdir(), "moyai-e2e-job-process-"));
  context.after(() => rm(parent, { recursive: true, force: true }));
  const root = path.join(parent, "execution root");
  const logs = path.join(root, "logs");
  await mkdir(logs, { recursive: true });
  return { parent, root, logs };
}

function rootWithGrandchildProgram(mode) {
  const grandchildProgram = mode === "normal"
    ? "setTimeout(() => process.exit(0), 150);"
    : "setInterval(() => {}, 1000);";
  return `
    import { spawn } from "node:child_process";
    import { writeFileSync } from "node:fs";
    const marker = process.argv[1];
    const child = spawn(process.execPath, ["--eval", ${JSON.stringify(grandchildProgram)}], {
      stdio: "ignore",
      windowsHide: true,
    });
    writeFileSync(marker, JSON.stringify({ root_pid: process.pid, grandchild_pid: child.pid, wrapper_pid: process.ppid }) + "\\n", { flag: "wx" });
    process.stdout.write("root-started\\n");
    process.stderr.write("root-stderr\\n");
    ${mode === "normal"
      ? "child.once(\"exit\", () => process.exit(0));"
      : "setInterval(() => {}, 1000);"}
  `;
}

function pidExists(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error?.code !== "ESRCH";
  }
}

async function waitForPidsAbsent(pids, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (pids.every((pid) => !pidExists(pid))) return true;
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  return pids.every((pid) => !pidExists(pid));
}

async function waitForJsonFile(candidate, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try { return JSON.parse(await readFile(candidate, "utf8")); }
    catch (error) {
      if (error?.code !== "ENOENT" && !(error instanceof SyntaxError)) throw error;
    }
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  throw new Error(`timed out waiting for JSON marker: ${candidate}`);
}

function supervisorRunnerProgram() {
  const driverUrl = new URL("../drivers/windows_external_process.mjs", import.meta.url).href;
  return `
    import { runWindowsExternalProcess } from ${JSON.stringify(driverUrl)};
    const input = JSON.parse(Buffer.from(process.argv[1], "base64").toString("utf8"));
    await runWindowsExternalProcess({
      ...input,
      executable: process.execPath,
      env: process.env,
    });
  `;
}

function outputOverflowProgram() {
  return `
    import { spawn } from "node:child_process";
    import { writeFileSync } from "node:fs";
    const marker = process.argv[1];
    const child = spawn(process.execPath, ["--eval", "setInterval(() => {}, 1000)"], {
      stdio: "ignore",
      windowsHide: true,
    });
    writeFileSync(marker, JSON.stringify({ root_pid: process.pid, grandchild_pid: child.pid }) + "\\n", { flag: "wx" });
    const chunk = Buffer.alloc(256 * 1024, 120);
    for (let index = 0; index < 16; index += 1) process.stdout.write(chunk);
    setInterval(() => {}, 1000);
  `;
}

windowsTest("Windows external runner owns a normal root-to-grandchild tree until exact zero", async (context) => {
  const execution = await temporaryExecution(context);
  const marker = path.join(execution.logs, "normal-owner.json");
  const stdout = path.join(execution.logs, "normal.stdout.log");
  const stderr = path.join(execution.logs, "normal.stderr.log");
  const result = await runWindowsExternalProcess({
    executionRoot: execution.root,
    executable: process.execPath,
    args: ["--input-type=module", "--eval", rootWithGrandchildProgram("normal"), marker],
    cwd: execution.root,
    env: process.env,
    stdoutPath: stdout,
    stderrPath: stderr,
    timeoutMs: 10_000,
    cleanupTimeoutMs: 5_000,
    label: "self-test-normal-tree",
  });
  const owners = JSON.parse(await readFile(marker, "utf8"));

  assert.equal(result.outcome.timed_out, false);
  assert.equal(result.outcome.output_limit_exceeded, false);
  assert.equal(result.outcome.tree_termination_requested, false);
  assert.equal(result.outcome.root_exit_code, 0);
  assert.equal(result.outcome.job_assigned_at_creation, true);
  assert.equal(result.outcome.supervisor_lost, false);
  assert.equal(result.job.assigned_before_resume, true);
  assert.equal(result.job.assigned_at_creation, true);
  assert.equal(result.job.kill_on_close, true);
  assert.equal(result.job.descendant_zero, true);
  assert.deepEqual(result.job.residual_process_ids_after_cleanup, []);
  assert.equal(result.job.observed_process_ids.includes(owners.root_pid), true);
  assert.equal(result.job.observed_process_ids.includes(owners.grandchild_pid), true);
  assert.equal(result.owner.process_id, owners.root_pid);
  assert.equal(result.owner.parent_process_id, result.wrapper.process_id);
  assert.equal(result.supervisor.process_id, process.pid);
  assert.match(result.supervisor.process_start_time_utc_ticks, /^\d+$/);
  assert.equal(path.resolve(result.supervisor.executable_path).toLowerCase(), path.resolve(process.execPath).toLowerCase());
  assert.match(result.owner.process_start_time_utc_ticks, /^\d+$/);
  assert.equal(path.resolve(result.owner.executable_path).toLowerCase(), path.resolve(process.execPath).toLowerCase());
  assert.equal(pidExists(owners.root_pid), false);
  assert.equal(pidExists(owners.grandchild_pid), false);
  assert.equal(await readFile(stdout, "utf8"), "root-started\n");
  assert.equal(await readFile(stderr, "utf8"), "root-stderr\n");
});

windowsTest("Windows external runner times out and terminates the exact root-to-grandchild Job tree", async (context) => {
  const execution = await temporaryExecution(context);
  const marker = path.join(execution.logs, "timeout-owner.json");
  const stdout = path.join(execution.logs, "timeout.stdout.log");
  const stderr = path.join(execution.logs, "timeout.stderr.log");
  const result = await runWindowsExternalProcess({
    executionRoot: execution.root,
    executable: process.execPath,
    args: ["--input-type=module", "--eval", rootWithGrandchildProgram("timeout"), marker],
    cwd: execution.root,
    env: process.env,
    stdoutPath: stdout,
    stderrPath: stderr,
    timeoutMs: 1_500,
    cleanupTimeoutMs: 10_000,
    label: "self-test-timeout-tree",
  });
  const owners = JSON.parse(await readFile(marker, "utf8"));

  assert.equal(result.outcome.timed_out, true);
  assert.equal(result.outcome.output_limit_exceeded, false);
  assert.equal(result.outcome.tree_termination_requested, true);
  assert.equal(result.outcome.descendant_zero, true);
  assert.equal(result.outcome.active_processes_after_cleanup, 0);
  assert.equal(result.outcome.residual_process_ids_before_termination.includes(owners.root_pid), true);
  assert.equal(result.outcome.residual_process_ids_before_termination.includes(owners.grandchild_pid), true);
  assert.deepEqual(result.outcome.residual_process_ids_after_cleanup, []);
  assert.equal(result.job.observed_process_ids.includes(owners.root_pid), true);
  assert.equal(result.job.observed_process_ids.includes(owners.grandchild_pid), true);
  assert.equal(pidExists(owners.root_pid), false);
  assert.equal(pidExists(owners.grandchild_pid), false);
});

windowsTest("Windows external runner returns a nonzero root exit without leaking its Job", async (context) => {
  const execution = await temporaryExecution(context);
  const result = await runWindowsExternalProcess({
    executionRoot: execution.root,
    executable: process.execPath,
    args: ["--eval", "process.exit(23)"],
    cwd: execution.root,
    env: process.env,
    stdoutPath: path.join(execution.logs, "nonzero.stdout.log"),
    stderrPath: path.join(execution.logs, "nonzero.stderr.log"),
    timeoutMs: 10_000,
    cleanupTimeoutMs: 5_000,
    label: "self-test-nonzero",
  });

  assert.equal(result.outcome.root_exit_code, 23);
  assert.equal(result.outcome.timed_out, false);
  assert.equal(result.outcome.output_limit_exceeded, false);
  assert.equal(result.job.assigned_at_creation, true);
  assert.equal(result.job.descendant_zero, true);
  assert.equal(pidExists(result.owner.process_id), false);
});

windowsTest("Windows external runner preserves exact arguments and a target-only environment block", async (context) => {
  const execution = await temporaryExecution(context);
  const values = ["", "plain", "space value", "quote\"and\\slash", "unicode-モヤイ", "line-one\nline-two", "trailing\\"];
  const targetPath = "Z:\\moyai-target-only-path";
  const targetEnvironment = {
    ...Object.fromEntries(Object.entries(process.env).filter(([key]) => key.toLowerCase() !== "path")),
    PATH: targetPath,
    MOYAI_EXTERNAL_UNICODE: "環境-✓",
  };
  const result = await runWindowsExternalProcess({
    executionRoot: execution.root,
    executable: process.execPath,
    args: [
      "--input-type=module",
      "--eval",
      "process.stdout.write(JSON.stringify({ args: process.argv.slice(1), path: process.env.PATH, unicode: process.env.MOYAI_EXTERNAL_UNICODE }));",
      ...values,
    ],
    cwd: execution.root,
    env: targetEnvironment,
    stdoutPath: path.join(execution.logs, "arguments-env.stdout.log"),
    stderrPath: path.join(execution.logs, "arguments-env.stderr.log"),
    timeoutMs: 10_000,
    cleanupTimeoutMs: 5_000,
    label: "self-test-arguments-env",
  });
  const observed = JSON.parse(await readFile(result.output.stdout.path, "utf8"));

  assert.deepEqual(observed.args, values);
  assert.equal(observed.path, targetPath);
  assert.equal(observed.unicode, "環境-✓");
  assert.equal(result.command.environment.key_count, Object.keys(targetEnvironment).length);
  assert.match(result.command.environment.sha256, /^[a-f0-9]{64}$/);
  assert.equal(result.job.descendant_zero, true);
});

windowsTest("Windows external runner bounds output overflow and collects the root-to-grandchild tree", async (context) => {
  const execution = await temporaryExecution(context);
  const marker = path.join(execution.logs, "overflow-owner.json");
  const started = Date.now();
  const result = await runWindowsExternalProcess({
    executionRoot: execution.root,
    executable: process.execPath,
    args: ["--input-type=module", "--eval", outputOverflowProgram(), marker],
    cwd: execution.root,
    env: process.env,
    stdoutPath: path.join(execution.logs, "overflow.stdout.log"),
    stderrPath: path.join(execution.logs, "overflow.stderr.log"),
    timeoutMs: 60_000,
    cleanupTimeoutMs: 10_000,
    maxOutputBytes: 1024,
    label: "self-test-output-overflow",
  });
  const owners = JSON.parse(await readFile(marker, "utf8"));

  assert.equal(result.outcome.output_limit_exceeded, true);
  assert.equal(result.outcome.output_limit_streams.includes("stdout"), true);
  assert.equal(result.outcome.tree_termination_requested, true);
  assert.equal(result.outcome.descendant_zero, true);
  assert.equal(result.output.stdout.size_bytes > 1024, true);
  assert.equal(Date.now() - started < 15_000, true);
  assert.equal(pidExists(owners.root_pid), false);
  assert.equal(pidExists(owners.grandchild_pid), false);
});

windowsTest("supervisor Node death terminates the wrapper and its creation-time Job tree", async (context) => {
  const execution = await temporaryExecution(context);
  const marker = path.join(execution.logs, "supervisor-death-owner.json");
  const input = Buffer.from(JSON.stringify({
    executionRoot: execution.root,
    args: ["--input-type=module", "--eval", rootWithGrandchildProgram("timeout"), marker],
    cwd: execution.root,
    stdoutPath: path.join(execution.logs, "supervisor-death.stdout.log"),
    stderrPath: path.join(execution.logs, "supervisor-death.stderr.log"),
    timeoutMs: 60_000,
    cleanupTimeoutMs: 10_000,
    label: "self-test-supervisor-death",
  }), "utf8").toString("base64");
  const supervisor = spawn(process.execPath, [
    "--input-type=module",
    "--eval",
    supervisorRunnerProgram(),
    input,
  ], {
    windowsHide: true,
    stdio: "ignore",
  });
  context.after(() => {
    if (supervisor.exitCode === null) supervisor.kill("SIGKILL");
  });
  const owners = await waitForJsonFile(marker);
  assert.equal(pidExists(owners.wrapper_pid), true);
  assert.equal(pidExists(owners.root_pid), true);
  assert.equal(pidExists(owners.grandchild_pid), true);

  assert.equal(supervisor.kill("SIGKILL"), true);
  assert.equal(await waitForPidsAbsent([
    supervisor.pid,
    owners.wrapper_pid,
    owners.root_pid,
    owners.grandchild_pid,
  ], 15_000), true);
});

windowsTest("killing the bounded PowerShell wrapper closes its Job and collects the owned tree", async (context) => {
  const execution = await temporaryExecution(context);
  const marker = path.join(execution.logs, "wrapper-kill-owner.json");
  const stdout = path.join(execution.logs, "wrapper-kill.stdout.log");
  const stderr = path.join(execution.logs, "wrapper-kill.stderr.log");
  let observed = null;
  await assert.rejects(
    runWindowsExternalProcess({
      executionRoot: execution.root,
      executable: process.execPath,
      args: ["--input-type=module", "--eval", rootWithGrandchildProgram("timeout"), marker],
      cwd: execution.root,
      env: process.env,
      stdoutPath: stdout,
      stderrPath: stderr,
      timeoutMs: 60_000,
      cleanupTimeoutMs: 10_000,
      wrapperTimeoutMs: 4_000,
      label: "self-test-wrapper-kill",
    }),
    (error) => {
      observed = error;
      return error instanceof WindowsExternalProcessError
        && error.code === "external-wrapper-timeout"
        && error.evidence?.owner?.job?.assigned_before_resume === true
        && error.evidence?.owner?.job?.kill_on_close === true
        && error.evidence?.root_absent_after_wrapper_exit === true;
    },
  );
  const owners = JSON.parse(await readFile(marker, "utf8"));
  assert.equal(observed.evidence.owner.owner.process_id, owners.root_pid);
  assert.equal(pidExists(owners.root_pid), false);
  assert.equal(pidExists(owners.grandchild_pid), false);
});

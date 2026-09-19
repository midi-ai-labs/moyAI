import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { createManagedExecutionRunner } from "../drivers/managed_execution_runner.mjs";
import { releaseResourcesThenAuditClosedStore } from "../drivers/windows_tauri_host.mjs";
import { waitForObservation } from "../core/deadline.mjs";

function fixture({ forceShutdown = false, changedOwner = false, identityErrors = [], identityOutput, captureError,
  shutdownErrors = [], closedIdentityErrors = [] } = {}) {
  const root = path.resolve("managed-runner-fixture"), events = [], commands = [];
  const context = { root, binary: path.join(root, "moyai-desktop.exe"), desktopIsolation: "fixture", paths: {
    config: path.join(root, "config"), config_file: path.join(root, "config/config.toml"),
    data: path.join(root, "data"), database: path.join(root, "data/moyai.sqlite3"),
    prefs: path.join(root, "prefs"), prefs_file: path.join(root, "prefs/desktop.toml"), webview: path.join(root, "webview"),
  } };
  const runnerBinary = path.join(root, "moyai-runner.exe"), runnerTestBinary = path.join(root, "runner-test.exe");
  const identity = { runner_id: "runner-incarnation", process_id: 22 };
  const owner = { process_id: 22, parent_process_id: 11, process_start_time_utc_ticks: "123456", executable_path: runnerTestBinary };
  let exited = false;
  const sink = { root: path.join(root, "evidence"), async writeJson(name, value) {
    events.push(["owner-saved", name, value]); return { relative_path: name };
  } };
  const dependencies = {
    async execute(executable, args, options) {
      commands.push({ executable, args, options }); events.push(["cli", ...args]);
      if (args[0] === "shutdown") {
        if (forceShutdown) throw new Error("shutdown failed");
        if (shutdownErrors.length) throw shutdownErrors.shift();
        exited = true; return { stdout: JSON.stringify({ stopped: true }) };
      }
      if (args[0] === "identity" && exited) throw closedIdentityErrors.shift() ?? new Error("IPC closed");
      if (args[0] === "identity" && identityErrors.length) throw identityErrors.shift();
      return { stdout: identityOutput ?? JSON.stringify({ identity }) };
    },
    async processCommand(action, params) {
      events.push([action, params]);
      if (action === "Capture") { if (exited) throw new Error("owner exited"); if (captureError) throw captureError; return structuredClone(owner); }
      assert.equal(action, "StopOwner");
      if (changedOwner) throw new Error("Process start identity changed");
      const stopped = !exited; exited = true; return { stopped, process_id: owner.process_id };
    },
    async observe({ sample, accept, timeoutMs, retrySampleErrors, ...options }) {
      assert.equal(timeoutMs, 25000); assert.equal(retrySampleErrors, false);
      let now = 0;
      return waitForObservation({ ...options, sample, accept, timeoutMs, retrySampleErrors,
        now: () => now, sleep: async milliseconds => { now += milliseconds; } });
    },
  };
  const options = { context, sink, runnerBinary, runnerTestBinary, expectedParentProcessId: 11 };
  return { ...options, events, commands, dependencies, driver: createManagedExecutionRunner(options, dependencies) };
}

test("managed Runner capture uses the same fixture IPC scope and records an exact process owner", async () => {
  const f = fixture(), captured = await f.driver.capture();
  assert.deepEqual(captured.identity, { runner_id: "runner-incarnation", process_id: 22 });
  assert.deepEqual(f.events[1], ["Capture", { ProcessId: 22, ExpectedExecutable: f.runnerTestBinary, ExpectedParentProcessId: 11 }]);
  const { executable, args, options } = f.commands[0];
  assert.equal(executable, f.runnerBinary); assert.deepEqual(args, ["identity"]);
  assert.equal(options.env.MOYAI_CONFIG_PATH, f.context.paths.config_file);
  assert.equal(options.env.MOYAI_DATA_DIR, f.context.paths.data);
  assert.equal(options.env.MOYAI_DESKTOP_E2E_ROOT, f.context.root);
  assert.equal(options.env.MOYAI_TEST_RESOURCE_REGISTRY, path.join(f.context.root, "resource-admission"));
  assert.equal(options.env.TEMP, path.join(f.context.root, "temp"));
  assert.equal(options.windowsHide, true); assert.equal(options.timeout, 12000);
  f.driver.identity.runner_id = "changed-by-observer";
  assert.equal(f.driver.identity.runner_id, "runner-incarnation");
});

function ipcError(code) {
  return Object.assign(new Error(`Runner IPC failed (${code})`), { stderr: `Runner IPC failed (os error ${code})\r\n` });
}

test("managed Runner capture retries a busy Windows pipe and captures only the authenticated result", async () => {
  const f = fixture({ identityErrors: [ipcError(231), ipcError(231)] });
  const captured = await f.driver.capture();
  assert.equal(captured.identity.runner_id, "runner-incarnation");
  assert.deepEqual(f.events.map(row => row[0]), ["cli", "cli", "cli", "Capture", "owner-saved"]);
  assert.deepEqual(f.events[3][1], { ProcessId: 22, ExpectedExecutable: f.runnerTestBinary, ExpectedParentProcessId: 11 });
});

test("busy Runner capture has a deadline and cleanup can later acquire only the same expected owner", async () => {
  const errors = Array.from({ length: 250 }, () => ipcError(231));
  const f = fixture({ identityErrors: errors });
  await assert.rejects(f.driver.capture(33), error => error.code === "observation-timeout" && error.evidence.attempts === 250);
  assert.equal(f.driver.identity, null);
  assert.equal(f.events.some(row => row[0] === "Capture" || row[0] === "owner-saved"), false);
  f.events.length = 0;
  const result = await f.driver.quiesce();
  assert.deepEqual(result, { pass: true, normal_shutdown: true, forced: false, process_id: 22 });
  assert.deepEqual(f.events[1], ["Capture", { ProcessId: 22, ExpectedExecutable: f.runnerTestBinary, ExpectedParentProcessId: 33 }]);
  assert.equal(f.events.filter(row => row[0] === "owner-saved").length, 1);
});

test("capture does not retry absence, access denial, malformed identity or a different process owner", async () => {
  for (const error of [ipcError(2), ipcError(5), new Error("unexpected error 231")]) {
    const f = fixture({ identityErrors: [error] });
    await assert.rejects(f.driver.capture(), candidate => candidate === error);
    assert.equal(f.commands.length, 1);
    assert.equal(f.events.some(row => row[0] === "Capture"), false);
  }
  for (const identityOutput of ["invalid JSON", "null", JSON.stringify({ identity: null })]) {
    const f = fixture({ identityOutput });
    await assert.rejects(f.driver.capture());
    assert.equal(f.commands.length, 1);
    assert.equal(f.events.some(row => row[0] === "Capture"), false);
  }
  const captureError = new Error("Captured process parent does not match the expected runner");
  const f = fixture({ captureError });
  await assert.rejects(f.driver.capture(), candidate => candidate === captureError);
  assert.equal(f.commands.length, 1);
  await assert.rejects(f.driver.quiesce(), candidate => candidate === captureError);
  assert.equal(f.driver.identity, null);
  assert.equal(f.events.some(row => row[0] === "StopOwner" || row[0] === "owner-saved"), false);
});

test("failed capture followed by missing IPC cannot report not-started or successful cleanup", async () => {
  const f = fixture({ identityErrors: [ipcError(2), ipcError(2)] });
  await assert.rejects(f.driver.capture());
  await assert.rejects(f.driver.quiesce());
  assert.equal(f.events.some(row => row[0] === "Capture" || row[0] === "StopOwner"), false);
});

test("cleanup retries only undelivered busy shutdown and does not mistake a busy identity for closed IPC", async () => {
  const f = fixture({ shutdownErrors: [ipcError(231)], closedIdentityErrors: [ipcError(231), ipcError(231)] });
  await f.driver.capture(); f.events.length = 0;
  assert.deepEqual(await f.driver.quiesce(), { pass: true, normal_shutdown: true, forced: false, process_id: 22 });
  assert.deepEqual(f.events.filter(row => row[0] === "cli").map(row => row[1]), ["shutdown", "shutdown", "identity", "identity", "identity"]);
});

test("managed Runner settles once through IPC and exact owner before the common SQLite audit", async () => {
  const f = fixture(); await f.driver.capture(11); f.events.length = 0;
  const result = await releaseResourcesThenAuditClosedStore({ context: f.context, scenario: { databaseRequired: true }, inputs: { acquisition: "pass" },
    desktopExited: true, profileRows: [],
    releaseScenarioResources: async () => { const result = await f.driver.quiesce(); return { input: result.pass ? "pass" : "fail", resources: [result] }; },
    auditSqlite: async () => { f.events.push(["sqlite-audit"]); return { pass: true }; },
  });
  assert.equal(result.sqlite.pass, true);
  assert.deepEqual(f.events.map(row => row[0]), ["cli", "cli", "Capture", "StopOwner", "sqlite-audit"]);
  assert.deepEqual(f.events[0], ["cli", "shutdown", "--runner", "runner-incarnation"]);
  assert.deepEqual(f.events[3][1], { ExecutionRoot: f.context.root, OwnerPath: path.join(f.sink.root, "owners/managed-execution-runner-incarnation.json") });
  const count = f.events.length;
  assert.deepEqual(await f.driver.quiesce(), { pass: true, normal_shutdown: true, forced: false, process_id: 22 });
  assert.equal(f.events.length, count);
});

test("failed normal shutdown uses only the captured owner and cannot pass the closed-store audit", async () => {
  const f = fixture({ forceShutdown: true }); await f.driver.capture(); f.events.length = 0;
  const result = await releaseResourcesThenAuditClosedStore({ context: f.context, scenario: { databaseRequired: true }, inputs: { acquisition: "pass" },
    desktopExited: true, profileRows: [],
    releaseScenarioResources: async () => { const result = await f.driver.quiesce(); return { input: result.pass ? "pass" : "fail", resources: [result] }; },
    auditSqlite: async () => { assert.fail("audit ran before successful resource settlement"); },
  });
  assert.equal(result.sqlite.pass, false);
  assert.deepEqual(f.events.map(row => row[0]), ["cli", "StopOwner"]);
  assert.deepEqual(result.scenarioQuiesce.resources[0], { pass: false, normal_shutdown: false, forced: true, process_id: 22 });
});

test("changed process identity is propagated instead of reporting successful cleanup", async () => {
  const f = fixture({ changedOwner: true }); await f.driver.capture();
  await assert.rejects(f.driver.quiesce(), /Process start identity changed/);
});

test("an uncaptured observer does not discover or stop other hosts", async () => {
  const f = fixture();
  assert.equal(f.driver.identity, null);
  assert.deepEqual(await f.driver.quiesce(), { pass: true, not_started: true });
  assert.deepEqual(f.events, []);
});

test("a cross-PC IPC configuration is rejected by the existing fixture owner", () => {
  const f = fixture();
  f.context.paths.data = path.join(f.context.root, "other-pc", "data");
  assert.throws(() => createManagedExecutionRunner(f, f.dependencies), /does not match its root/);
  assert.deepEqual(f.events, []);
});

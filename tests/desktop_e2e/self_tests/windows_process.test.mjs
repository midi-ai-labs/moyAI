import assert from "node:assert/strict";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawn } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";

import { invokeWindowsProcess, waitForChildExit } from "../drivers/windows_process.mjs";

test("Windows adapter captures and stops only an exact test-owned process", { skip: process.platform !== "win32" }, async (context) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "moyai-e2e-process-"));
  const child = spawn("pwsh.exe", ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "Wait-Event"], {
    windowsHide: true,
    stdio: "ignore",
  });
  context.after(async () => {
    if (child.exitCode === null) child.kill();
    await waitForChildExit(child, 5_000);
    await rm(root, { recursive: true, force: true });
  });
  await new Promise((resolve, reject) => {
    child.once("spawn", resolve);
    child.once("error", reject);
  });
  const owner = await invokeWindowsProcess("Capture", { ProcessId: child.pid });
  assert.equal(owner.process_id, child.pid);
  assert.match(owner.process_start_time_utc_ticks, /^\d+$/);
  const constrained = await invokeWindowsProcess("Capture", {
    ProcessId: child.pid,
    ExpectedExecutable: owner.executable_path,
    ExpectedParentProcessId: process.pid,
  });
  assert.equal(constrained.process_start_time_utc_ticks, owner.process_start_time_utc_ticks);
  await assert.rejects(
    () => invokeWindowsProcess("Capture", { ProcessId: child.pid, ExpectedExecutable: process.execPath }),
    /executable does not match/,
  );
  await assert.rejects(
    () => invokeWindowsProcess("Capture", { ProcessId: child.pid, ExpectedParentProcessId: process.pid + 1 }),
    /parent does not match/,
  );
  const ownerPath = path.join(root, "owner.json");
  await writeFile(ownerPath, JSON.stringify(owner), { flag: "wx" });
  const stopped = await invokeWindowsProcess("StopOwner", { ExecutionRoot: root, OwnerPath: ownerPath });
  assert.equal(stopped.process_id, child.pid);
  assert.equal(stopped.stopped, true);
  assert.equal(await waitForChildExit(child, 10_000), true);
});

test("WebView profile matching uses an exact user-data-dir path boundary", { skip: process.platform !== "win32" }, async (context) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "moyai-e2e-profile-"));
  context.after(() => rm(root, { recursive: true, force: true }));
  const profile = path.join(root, "webview");
  const childProfile = path.join(profile, "EBWebView");
  const exact = await invokeWindowsProcess("MatchProfile", {
    ExecutionRoot: root,
    ProfilePath: profile,
    CommandLine: `msedgewebview2.exe --user-data-dir="${childProfile}" --type=renderer`,
  });
  assert.equal(exact.matches, true);
  const prefixOnly = await invokeWindowsProcess("MatchProfile", {
    ExecutionRoot: root,
    ProfilePath: profile,
    CommandLine: `msedgewebview2.exe --user-data-dir="${profile}-foreign" --type=renderer`,
  });
  assert.equal(prefixOnly.matches, false);
});

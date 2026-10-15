import assert from "node:assert/strict";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawn, execFile } from "node:child_process";
import { createServer } from "node:net";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";
import { mkdir, mkdtemp, rm, rmdir, unlink, writeFile } from "node:fs/promises";

import { invokeWindowsProcess, waitForChildExit } from "../drivers/windows_process.mjs";

test("listener observation binds an actual loopback port to the exact captured owner", { skip: process.platform !== "win32" }, async (context) => {
  const parent = fileURLToPath(new URL("../../../../project_sandbox/four-device-unit/", import.meta.url));
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(path.join(parent, "listener-")), ownerPath = path.join(root, "owner.json");
  const server = createServer();
  await new Promise((resolve, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", resolve); });
  const child = spawn(process.execPath, ["-e", "const server=require('node:net').createServer();server.listen(0,'127.0.0.1',()=>process.send({port:server.address().port}));process.on('message',()=>server.close(()=>process.exit(0)));"], { windowsHide: true, stdio: ["ignore", "ignore", "ignore", "ipc"] });
  const childPort = await new Promise((resolve, reject) => { child.once("error", reject); child.once("message", value => resolve(value.port)); });
  context.after(async () => {
    await new Promise(resolve => server.close(resolve));
    if (child.connected) child.send("close");
    if (!await waitForChildExit(child, 5000)) { child.kill(); await waitForChildExit(child, 5000); }
    await unlink(ownerPath); await rmdir(root);
  });
  const owner = await invokeWindowsProcess("Capture", { ProcessId: process.pid });
  await writeFile(ownerPath, JSON.stringify(owner), { flag: "wx" });
  const listeners = await invokeWindowsProcess("ObserveListeners", { ExecutionRoot: root, OwnerPath: ownerPath });
  assert.ok(listeners.some(row => row.process_id === process.pid && row.port === server.address().port && row.address === "127.0.0.1"));
  assert.ok(listeners.every(row => row.process_id === process.pid));
  const tree = await invokeWindowsProcess("ObserveTreeListeners", { ExecutionRoot: root, OwnerPath: ownerPath });
  assert.ok(tree.some(row => row.process_id === process.pid && row.port === server.address().port));
  assert.ok(tree.some(row => row.process_id === child.pid && row.port === childPort));
});

test("exact exit observation rejects query errors and confirms disappearance across a query race", { skip: process.platform !== "win32" }, async (context) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "moyai-e2e-owner-query-"));
  context.after(() => rm(root, { recursive: true, force: true }));
  const ownerPath = path.join(root, "owner.json"), scriptPath = path.join(root, "query-fixture.ps1");
  await writeFile(ownerPath, JSON.stringify({ process_id: 17, process_start_time_utc_ticks: "639261936000000000", executable_path: "C:\\owned\\runner.exe" }), { flag: "wx" });
  await writeFile(scriptPath, `param($Adapter, $Root, $OwnerPath, $Mode)
$global:probeReads = 0
$global:probeMode = $Mode
function global:Get-Process {
  [CmdletBinding()]param([int]$Id)
  $global:probeReads++
  if ($global:probeMode -eq 'process-error') { Write-Error 'Process access denied'; return }
  if ($global:probeMode -eq 'absent' -or ($global:probeMode -eq 'exit-race' -and $global:probeReads -gt 1)) {
    $PSCmdlet.WriteError([System.Management.Automation.ErrorRecord]::new([ArgumentException]::new('No process'), 'NoProcessFoundForGivenId', [System.Management.Automation.ErrorCategory]::ObjectNotFound, $Id)); return
  }
  [pscustomobject]@{ Path = 'C:\\owned\\runner.exe'; StartTime = [DateTime]::new(639261936000000000, [DateTimeKind]::Utc) }
}
function global:Get-CimInstance {
  [CmdletBinding()]param($ClassName, $Filter)
  if ($global:probeMode -eq 'cim-error') { Write-Error 'CIM access denied'; return }
  return $null
}
& $Adapter -Action ObserveOwner -ExecutionRoot $Root -OwnerPath $OwnerPath
`, { flag: "wx" });
  const adapter = fileURLToPath(new URL("../drivers/windows_process.ps1", import.meta.url));
  const invoke = mode => promisify(execFile)("pwsh.exe", ["-NoLogo", "-NoProfile", "-NonInteractive", "-File", scriptPath, adapter, root, ownerPath, mode], { windowsHide: true, timeout: 10000 });
  for (const [mode, message] of [["process-error", /Process access denied/], ["cim-error", /CIM access denied/], ["inconsistent", /observation did not settle/]]) {
    await assert.rejects(invoke(mode), error => message.test(error.stderr));
  }
  for (const mode of ["absent", "exit-race"]) {
    const result = await invoke(mode);
    assert.deepEqual(JSON.parse(result.stdout), { live: false, process_id: 17 });
  }
});

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
  assert.deepEqual(await invokeWindowsProcess("ObserveOwner", { ExecutionRoot: root, OwnerPath: ownerPath }), { live: true, process_id: child.pid });
  const changedPath = path.join(root, "changed-owner.json");
  await writeFile(changedPath, JSON.stringify({ ...owner, process_start_time_utc_ticks: "1" }), { flag: "wx" });
  await assert.rejects(() => invokeWindowsProcess("ObserveOwner", { ExecutionRoot: root, OwnerPath: changedPath }), /start identity changed/);
  await assert.rejects(() => invokeWindowsProcess("ObserveOwner", { ExecutionRoot: path.join(root, "narrower"), OwnerPath: ownerPath }), /escaped execution root/i);
  assert.equal(child.exitCode, null);
  const stopped = await invokeWindowsProcess("StopOwner", { ExecutionRoot: root, OwnerPath: ownerPath });
  assert.equal(stopped.process_id, child.pid);
  assert.equal(stopped.stopped, true);
  assert.equal(await waitForChildExit(child, 10_000), true);
  assert.deepEqual(await invokeWindowsProcess("ObserveOwner", { ExecutionRoot: root, OwnerPath: ownerPath }), { live: false, process_id: child.pid });
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

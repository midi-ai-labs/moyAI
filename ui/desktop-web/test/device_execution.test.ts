import assert from "node:assert/strict";
import test from "node:test";
import type { ActionContext } from "../src/actions.ts";
import { bindProjectFolder, deviceExecutionAction, deviceExecutionActionEnabled, projectFolderBindingEnabled, refreshDeviceExecution, renderDeviceExecution, type DeviceExecutionProjection } from "../src/device_execution.ts";
import { acceptDeviceNetworkProjection, editDeviceNetworkField } from "../src/device_network_state.ts";
import { deviceProjection, deviceUiFixture } from "./device_network_fixture.ts";

function projection(overrides: Partial<DeviceExecutionProjection> = {}): DeviceExecutionProjection {
  return { revision: "1", state: "needs_setup", projects: [], review: null, directory: null, access_mode: null,
    accepting: false, can_pause: false, can_resume: false, unknown_attempts: [], error: null, ...overrides };
}
function unknown(attempt = "attempt-a", generation = 1) {
  return { attempt_id: attempt, generation, job_id: `job-${attempt}`, environment_id: "env-a", run_id: "run-a", state: "unknown" };
}
function fixture(p = projection()) {
  const local = deviceUiFixture(); local.execution = p;
  const view = { overlay: "hub" };
  const context = { uiState: { deviceNetwork: local }, getViewState: () => view, rerender() {} } as unknown as ActionContext;
  return { local, view, context };
}
test("local consent hands off to the administrator without claiming AI or project readiness", () => {
  const { local } = fixture(projection({ state: "not_selected" }));
  let html = renderDeviceExecution(local);
  assert.match(html, /data-details-key="device-execution-setup" open/);
  assert.doesNotMatch(html, /このPCの実行設定は保存済みです/);
  local.execution!.review = { id: "review-a", directory: "C:/NotYetConsented", access_mode: "default" };
  assert.doesNotMatch(renderDeviceExecution(local), /このPCの実行設定は保存済みです/);
  local.execution = projection({ state: "not_selected", directory: "C:/Approved", access_mode: "default" });
  html = renderDeviceExecution(local);
  assert.match(html, /このPCの実行設定は保存済みです/);
  assert.match(html, /次はHub管理者の操作です/);
  assert.ok(html.indexOf("次はHub管理者") < html.indexOf('data-details-key="device-execution-setup"'));
  assert.doesNotMatch(html, /data-details-key="device-execution-setup" open/);
  assert.doesNotMatch(html, /実行するPCとしての割り当てはありません|実行機能が動作中/);
  local.execution.state = "unavailable";
  local.execution.error = "起動を確認できません";
  assert.doesNotMatch(renderDeviceExecution(local), /次はHub管理者の操作です/);
});
test("sign-in autostart is explicit, limited to a consented PC and sent with its revision", async () => {
  const { local, context } = fixture();
  assert.equal(deviceExecutionActionEnabled(local, "install-autostart"), false);
  local.execution = projection({ directory: "C:/Approved", autostart: false, state: "ready" });
  assert.equal(deviceExecutionActionEnabled(local, "install-autostart"), true);
  const calls: unknown[] = [];
  await withInvoke(async (_name, args) => { calls.push(args); return projection({ revision: "2", directory: "C:/Approved", autostart: true, state: "ready" }); }, async () => {
    await deviceExecutionAction(context, "install-autostart");
  });
  assert.deepEqual(calls, [{ expectedRevision: "1", request: { kind: "install_autostart" } }]);
  assert.equal(deviceExecutionActionEnabled(local, "install-autostart"), false);
  assert.equal(deviceExecutionActionEnabled(local, "remove-autostart"), true);
  assert.match(renderDeviceExecution(local), /サインアウト中/);
});
async function withInvoke(invoke: (name: string, args: Record<string, unknown>) => Promise<unknown>, run: () => Promise<void>) {
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke } } });
  try { await run(); }
  finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
}

test("execution setup sends one native review and enables only that review with its displayed access", async () => {
  const { local, context } = fixture(), calls: Record<string, unknown>[] = [];
  const review = { id: "review-a", directory: "C:/Approved", access_mode: "default" as const };
  assert.match(renderDeviceExecution(local), /フォルダーの作成先を選ぶ/);
  assert.doesNotMatch(renderDeviceExecution(local), /Runnerを起動|ひな形をHubに公開|templateId/);
  await withInvoke(async (_name, args) => { calls.push(args); return projection({ revision: String(calls.length + 1), review }); }, async () => {
    await deviceExecutionAction(context, "prepare");
    editDeviceNetworkField(local, "execution_access", "full_access", false);
    await deviceExecutionAction(context, "enable"); assert.equal(calls.length, 1);
    editDeviceNetworkField(local, "execution_access", "default", false);
    await deviceExecutionAction(context, "enable");
    assert.deepEqual(calls, [
      { expectedRevision: "1", request: { kind: "prepare", access_mode: "default" } },
      { expectedRevision: "2", request: { kind: "enable", review_id: "review-a" } },
    ]);
  });
});

test("new execution after a connection reset requires a separate local stop and effects confirmation", async () => {
  const {local, context} = fixture(projection({reset_review_required:true, review:{id:"new-review",directory:"C:/New",access_mode:"default"}}));
  assert.equal(deviceExecutionActionEnabled(local,"enable"),false);
  assert.match(renderDeviceExecution(local), /以前の処理の停止と影響を確認した/);
  editDeviceNetworkField(local,"execution_reset_confirmed","",true);
  assert.equal(deviceExecutionActionEnabled(local,"enable"),true);
  const calls: unknown[]=[];
  await withInvoke(async (_name,args)=>{calls.push(args);return projection({revision:"2",state:"ready",reset_review_required:false});}, async()=>{await deviceExecutionAction(context,"enable");});
  assert.deepEqual(calls,[{expectedRevision:"1",request:{kind:"enable",review_id:"new-review",previous_execution_confirmed:true}}]);
  assert.equal(local.executionResetConfirmed,false);
});

test("execution settings point to per-project folders and missing capabilities do not invent Resume", () => {
  const { local } = fixture(projection({ state: "ready", directory: "C:/Approved", access_mode: "default" }));
  const html = renderDeviceExecution(local);
  assert.match(html, /この後プロジェクトごとに選びます/);
  assert.match(html, /既存プロジェクトの場所を変える場合は、下の「2\. プロジェクトの作業フォルダー」で選び直してください/);
  assert.doesNotMatch(html, /今後追加されるプロジェクト/);
  assert.doesNotMatch(html, /data-action="device-execution-resume"|data-action="device-execution-pause"/);
});

test("a missing project mapping gives its own reselection action without blocking another prepared project", () => {
  for (const preparation_state of ["ready", "failed"] as const) {
    const { local } = fixture(projection({ state: "ready", directory: "C:/NewFolders", projects: [
      { id: "needs-folder", label: "時計アプリ", can_control: false, can_execute: true,
        environment_id: "clock-env", directory: null, preparation_state,
        error: preparation_state === "failed" ? "登録した作業フォルダーが見つかりません。" : null },
      { id: "prepared", label: "TODOアプリ", can_control: true, can_execute: true,
        environment_id: "todo-env", directory: "C:/ExistingTODO", preparation_state: "ready", error: null },
    ] }));
    const html = renderDeviceExecution(local);
    assert.match(html, /このプロジェクトの新しい仕事を実行できません/);
    assert.match(html, /data-action="bind-project-folder" data-value="needs-folder" >作業フォルダーを選び直す/);
    assert.match(html, /data-action="bind-project-folder" data-value="prepared" >作業フォルダーを変更/);
    assert.match(html, /作業フォルダー: C:\/ExistingTODO/);
    assert.equal(projectFolderBindingEnabled(local, "needs-folder"), true);
  }
  const { local } = fixture(projection({ state: "unavailable", directory: "C:/NewFolders", error: "状態を取得できません。", projects: [
    { id: "first-use", label: "初回設定", can_control: false, can_execute: true,
      environment_id: "new-env", directory: null, preparation_state: "waiting_setup", error: null },
  ] }));
  const html = renderDeviceExecution(local);
  assert.match(html, /data-value="first-use" >作業フォルダーを選ぶ/);
  assert.doesNotMatch(html, /作業フォルダーを選び直す|削除|見つかりません/);
  assert.match(html, /状態を取得できません/);
});

test("reselecting an unbound folder sends the project mapping target independently of the creation root", async () => {
  const project = { id: "project-a", label: "時計アプリ", can_control: true, can_execute: true,
    environment_id: "clock-env", directory: null, preparation_state: "failed" as const, error: "登録した作業フォルダーが見つかりません。" };
  const { local, context } = fixture(projection({ state: "ready", directory: "C:/NewFolders", access_mode: "default", projects: [project] }));
  const calls: { name: string; args: Record<string, unknown> }[] = [];
  let polls = 0;
  await withInvoke(async (name, args) => {
    calls.push({ name, args });
    if (name === "browse_shared_project_folder") return "C:/RecoveredClock";
    if (name === "device_execution_projection") return projection({ revision: String(++polls + 1), state: "ready", directory: "C:/NewFolders", access_mode: "default",
      projects: polls === 1 ? [project] : [{ ...project, directory: "C:/RecoveredClock", preparation_state: "ready", error: null }] });
    if (name === "shared_work_projection") return { generation: "7" };
    if (name === "shared_work_command") return { error: null };
    throw new Error(`Unexpected command: ${name}`);
  }, async () => { await bindProjectFolder(context, project.id); });
  assert.deepEqual(calls.find(call => call.name === "shared_work_command")?.args, {
    expectedGeneration: "7", request: { kind: "bind_project_folder", project_id: "project-a", environment_id: "clock-env",
      directory: "C:/RecoveredClock", access_mode: "default", expected_directory: null },
  });
  assert.equal(local.execution!.directory, "C:/NewFolders");
  assert.equal(local.execution!.projects[0].directory, "C:/RecoveredClock");
  assert.equal(local.executionError, "");
});

test("first-use folder selection follows PC consent and does not open an unusable native picker", async () => {
  const project = { id: "project-a", label: "TODOアプリ", can_control: true, can_execute: true,
    environment_id: "environment-a", directory: null, preparation_state: "failed" as const, error: "実行設定がありません" };
  const { local, context } = fixture(projection({ projects: [project] }));
  const before = renderDeviceExecution(local);
  assert.ok(before.indexOf("1. このPCの実行許可") < before.indexOf("2. プロジェクトの作業フォルダー"));
  assert.match(before, /実行許可」を保存すると選べます/);
  assert.doesNotMatch(before, /実行設定がありません/);
  assert.match(before, /data-action="bind-project-folder" data-value="project-a" disabled/);
  assert.equal(projectFolderBindingEnabled(local, "project-a"), false);
  let calls = 0;
  await withInvoke(async () => { calls++; throw new Error("no picker before consent"); }, async () => {
    await bindProjectFolder(context, "project-a");
  });
  assert.equal(calls, 0);
  local.execution = projection({ state: "ready", directory: "C:/CreatedFolders", projects: [project] });
  assert.equal(projectFolderBindingEnabled(local, "project-a"), true);
  const after = renderDeviceExecution(local);
  assert.match(after, /フォルダーの作成先: C:\/CreatedFolders/);
  assert.match(after, /実行設定がありません/); // Once consented, preserve an actual preparation failure.
  assert.equal(projectFolderBindingEnabled(local, "another-project"), false);
  local.execution.state = "starting";
  assert.equal(projectFolderBindingEnabled(local, "project-a"), false);
  local.execution.state = "unconnected";
  assert.equal(projectFolderBindingEnabled(local, "project-a"), false);
  const disconnected = renderDeviceExecution(local);
  assert.match(disconnected, /先に上の「Hubへの接続」/);
  assert.match(disconnected, /data-action="device-execution-prepare" disabled/);
});

test("execution controls follow capabilities even while a running task temporarily occupies the PC", () => {
  const { local } = fixture(projection({ state: "ready", directory: "C:/Approved", can_pause: true }));
  assert.equal(local.execution!.accepting, false);
  assert.equal(deviceExecutionActionEnabled(local, "pause"), true);
  assert.equal(deviceExecutionActionEnabled(local, "resume"), false);
  assert.match(renderDeviceExecution(local), /data-action="device-execution-pause"/);
  local.execution = projection({ state: "paused", directory: "C:/Approved", can_resume: true });
  assert.equal(deviceExecutionActionEnabled(local, "resume"), true);
  local.executionPending = "resume";
  assert.equal(deviceExecutionActionEnabled(local, "resume"), false);
});

test("execution polling ignores an older response and a response after the Hub view closes", async () => {
  const { local, context, view } = fixture();
  const resolves: ((value: DeviceExecutionProjection) => void)[] = [];
  await withInvoke(async () => new Promise(resolve => resolves.push(resolve)), async () => {
    const first = refreshDeviceExecution(context), second = refreshDeviceExecution(context);
    resolves[1](projection({ revision: "3", state: "ready" })); await second;
    resolves[0](projection({ revision: "2", state: "unconnected" })); await first;
    assert.equal(local.execution!.revision, "3");
    const closing = refreshDeviceExecution(context); view.overlay = "none";
    resolves[2](projection({ revision: "4", state: "unconnected" })); await closing;
    assert.equal(local.execution!.revision, "3");
  });
});

test("stop evidence belongs to the selected attempt and cannot be reused for another unknown", async () => {
  const a = unknown(), b = unknown("attempt-b");
  const { local, context } = fixture(projection({ state: "unavailable", unknown_attempts: [a, b] }));
  editDeviceNetworkField(local, "execution_recovery_target", JSON.stringify([a.attempt_id, a.generation]), false);
  editDeviceNetworkField(local, "execution_recovery_reason", "現地で停止を確認", false);
  editDeviceNetworkField(local, "execution_recovery_effects", "", true);
  editDeviceNetworkField(local, "execution_recovery_stopped", "", true);
  assert.equal(deviceExecutionActionEnabled(local, "reconcile", a.attempt_id), true);
  assert.equal(deviceExecutionActionEnabled(local, "reconcile", b.attempt_id), false);
  const calls: Record<string, unknown>[] = [];
  await withInvoke(async (_name, args) => { calls.push(args); return projection({ revision: "2", unknown_attempts: [b] }); }, async () => {
    await deviceExecutionAction(context, "reconcile", b.attempt_id); assert.equal(calls.length, 0);
    await deviceExecutionAction(context, "reconcile", a.attempt_id);
    assert.deepEqual(calls[0], { expectedRevision: "1", request: { kind: "reconcile", attempt_id: a.attempt_id, generation: 1,
      reason: "現地で停止を確認", evidence: { kind: "operator_confirmed_stopped", effects_reviewed: true, processes_stopped: true } } });
    editDeviceNetworkField(local, "execution_recovery_target", JSON.stringify([b.attempt_id, b.generation]), false);
    assert.equal(deviceExecutionActionEnabled(local, "reconcile", b.attempt_id), false);
    assert.equal(local.executionEffectsReviewed, false); assert.equal(local.executionProcessesStopped, false);
  });
});

test("changed unknown generation and connection identity retire recovery evidence and stale execution responses", async () => {
  const a = unknown(), { local, context } = fixture(projection({ unknown_attempts: [a] }));
  editDeviceNetworkField(local, "execution_recovery_target", JSON.stringify([a.attempt_id, a.generation]), false);
  editDeviceNetworkField(local, "execution_recovery_reason", "checked", false);
  editDeviceNetworkField(local, "execution_recovery_effects", "", true);
  editDeviceNetworkField(local, "execution_recovery_stopped", "", true);
  let resolve!: (value: DeviceExecutionProjection) => void;
  await withInvoke(async () => new Promise(done => { resolve = done; }), async () => {
    const refresh = refreshDeviceExecution(context);
    resolve(projection({ revision: "2", unknown_attempts: [unknown("attempt-a", 2)] })); await refresh;
    assert.equal(deviceExecutionActionEnabled(local, "reconcile", a.attempt_id), false);
    assert.equal(local.executionRecoveryReason, "");
    const stale = refreshDeviceExecution(context);
    acceptDeviceNetworkProjection(local, deviceProjection({ revision: "4", generation: "8", device_id: "other-device" }));
    resolve(projection({ revision: "3", directory: "C:/Previous-PC", unknown_attempts: [a] })); await stale;
    assert.equal(local.execution, null);
    assert.equal(local.executionEffectsReviewed, false); assert.equal(local.executionProcessesStopped, false);
  });
});

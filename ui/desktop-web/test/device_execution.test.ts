import assert from "node:assert/strict";
import test from "node:test";
import type { ActionContext } from "../src/actions.ts";
import { deviceExecutionAction, deviceExecutionActionEnabled, refreshDeviceExecution, renderDeviceExecution, type DeviceExecutionProjection } from "../src/device_execution.ts";
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
  assert.match(renderDeviceExecution(local), /初回の実行設定|保存先フォルダーを選ぶ/);
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

test("changing execution defaults describes future projects and missing capabilities do not invent Resume", () => {
  const { local } = fixture(projection({ state: "ready", directory: "C:/Approved", access_mode: "default" }));
  const html = renderDeviceExecution(local);
  assert.match(html, /今後追加されるプロジェクト/);
  assert.match(html, /作成済みの作業フォルダーと実行権限は変更しません/);
  assert.doesNotMatch(html, /data-action="device-execution-resume"|data-action="device-execution-pause"/);
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

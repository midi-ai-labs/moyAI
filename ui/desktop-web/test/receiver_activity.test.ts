import assert from "node:assert/strict";
import test from "node:test";
import type { ActionContext } from "../src/actions.ts";
import { createDeviceNetworkUiState, deviceNetworkPresentation } from "../src/device_network_state.ts";
import { receiverAttemptKey, receiverBlocksLocalSend, receiverServiceKey, receiverServiceStopEnabled, receiverStopEnabled, renderReceiverActivity, stopReceiverAttempt, stopReceiverService, type ReceiverActivityProjection } from "../src/receiver_activity.ts";
import { sharedUiFixture } from "./shared_work_fixture.ts";

const attempt = { attempt_id: "attempt-a", generation: "9007199254740993", job_id: "job-a", project_id: "project-a", environment_id: "env-a", run_id: "01K5RSWRDNYVZ5QNS5FD6C0FTK", state: "executing" as const, local_state: "running" as const };
const activity: ReceiverActivityProjection = { runner_id: "01K5RSWRDNYVZ5QNS5FD6C0FTM", attempts: [attempt], retained_services: [], observed_at_ms: 1000, unavailable: false };

test("local Runner work is visible in either conversation and private details stay hidden", () => {
  const local = createDeviceNetworkUiState(); local.receiverActivity = activity;
  const key = receiverAttemptKey(attempt);
  assert.equal(receiverStopEnabled(deviceNetworkPresentation(local), key), true);
  local.receiverActivity = { ...activity, attempts: [{ ...attempt, local_state: "stopping" }] };
  assert.equal(receiverStopEnabled(deviceNetworkPresentation(local), key), false);
  local.receiverActivity = activity;
  let html = renderReceiverActivity(deviceNetworkPresentation(local));
  assert.match(html, /このPCは使用中/);
  assert.match(html, /このPCで仕事を実行中です/);
  assert.doesNotMatch(html, /別の端末からの仕事/);
  assert.match(html, /同じ実行枠を使う新しい仕事は待機または拒否/);
  assert.match(html, /詳細を表示できない仕事/);
  assert.doesNotMatch(html, /job-a|project-a/);
  const shared = sharedUiFixture();
  shared.projection!.status!.jobs[0].title = "共有できる仕事";
  html = renderReceiverActivity(deviceNetworkPresentation(local), shared);
  assert.match(html, /共有できる仕事/);
  shared.projection!.status!.project_id = "other-project";
  assert.doesNotMatch(renderReceiverActivity(deviceNetworkPresentation(local), shared), /共有できる仕事/);
  local.receiverActivity = { ...activity, unavailable: true };
  html = renderReceiverActivity(deviceNetworkPresentation(local), shared);
  assert.match(html, /実行状態を確認できません/);
  assert.doesNotMatch(html, /data-action="receiver-stop"/);
});

test("configured receiver with unavailable status conservatively blocks local Send", () => {
  const local = createDeviceNetworkUiState();
  local.execution = { revision: "1", state: "ready", projects: [], review: null, directory: "C:/runner",
    access_mode: "default", accepting: true, can_pause: true, can_resume: false, error: null, unknown_attempts: [] };
  local.receiverActivity = { ...activity, attempts: [], unavailable: true };
  assert.equal(receiverBlocksLocalSend(deviceNetworkPresentation(local)), true);
  assert.match(renderReceiverActivity(deviceNetworkPresentation(local)), /このPCの実行状態を確認できません/);
  local.receiverActivity = { ...activity, attempts: [], unavailable: false };
  assert.equal(receiverBlocksLocalSend(deviceNetworkPresentation(local)), false);
});

test("retained app stays visible and locally stoppable without Hub access", async () => {
  const service = { service_id: "service-a", attempt_id: "attempt-a", generation: "9007199254740993", project_id: "project-hidden", conversation_id: "conversation-hidden", environment_id: "environment-hidden", expires_at_ms: Date.now() + 60_000, local_state: "running" as const, uncertain: false };
  const local = createDeviceNetworkUiState();
  local.receiverActivity = { ...activity, attempts: [], retained_services: [service] };
  const key = receiverServiceKey(service);
  assert.equal(receiverServiceStopEnabled(deviceNetworkPresentation(local), key), true);
  const html = renderReceiverActivity(deviceNetworkPresentation(local));
  assert.match(html, /このPCでアプリを起動中/);
  assert.match(html, /詳細を表示できないアプリ/);
  assert.doesNotMatch(html, /project-hidden|conversation-hidden|environment-hidden/);
  local.receiverActivity.retained_services[0] = { ...service, local_state: "stopped" };
  assert.match(renderReceiverActivity(deviceNetworkPresentation(local)), /停止済み・Hubへの報告待ち/);
  assert.equal(receiverServiceStopEnabled(deviceNetworkPresentation(local), key), false);
  local.receiverActivity.retained_services[0] = service;
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  let sent: unknown;
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (name: string, args: unknown) => {
    assert.equal(name, "receiver_service_stop"); sent = args;
    return { ...activity, attempts: [], retained_services: [] };
  } } } });
  try {
    const context = { uiState: { deviceNetwork: local }, rerender() {} } as unknown as ActionContext;
    await stopReceiverService(context, key);
    assert.deepEqual(sent, { target: { runner_id: activity.runner_id, service_id: service.service_id, attempt_id: service.attempt_id, generation: service.generation } });
    assert.equal(local.receiverActivity?.retained_services.length, 0);
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});

test("local stop sends exact Runner incarnation, attempt, generation and run identity", async () => {
  const local = createDeviceNetworkUiState(); local.receiverActivity = activity;
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  let sent: unknown;
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (name: string, args: unknown) => {
    assert.equal(name, "receiver_activity_stop"); sent = args;
    return { ...activity, attempts: [] };
  } } } });
  try {
    const context = { uiState: { deviceNetwork: local }, rerender() {} } as unknown as ActionContext;
    await stopReceiverAttempt(context, receiverAttemptKey(attempt));
    assert.deepEqual(sent, { target: { runner_id: activity.runner_id, attempt_id: attempt.attempt_id, generation: attempt.generation, run_id: attempt.run_id } });
    assert.equal(local.receiverActivity?.attempts.length, 0);
    await stopReceiverAttempt(context, receiverAttemptKey(attempt));
    assert.equal(local.receiverError, "");
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});

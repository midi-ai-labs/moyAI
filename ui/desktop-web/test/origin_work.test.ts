import assert from "node:assert/strict";
import test from "node:test";
import type { ActionContext } from "../src/actions.ts";
import { createDeviceNetworkUiState, deviceNetworkPresentation } from "../src/device_network_state.ts";
import { originAllStopEnabled, originAppsStopEnabled, refreshOriginWork, renderOriginWork, stopOriginAll, stopOriginApps, type OriginWorkProjection } from "../src/origin_work.ts";

const target = (sessionId: string) => ({ workspacePath: "C:/workspace", sessionId, ownerGeneration: "4" });
const origin = (sessionId: string): OriginWorkProjection => ({ origin_session_ref: sessionId,
  jobs: [{ project_id: "project-a", job: { id: "job-a", title: "WinBでアプリ", state: "succeeded", environment_label: "WinB", device_label: "WinB", updated_at_ms: 1 } }],
  retained_services: [{ project_id: "project-a", service: { service_id: "service-a", conversation_id: "chat-a", environment_id: "env-b", expires_at_ms: Date.now() + 60_000,
    stop_requested: false, uncertain: false, can_stop: true } }], observed_at_ms: Date.now(), admission_revision: "7" });

function contextFixture() {
  const local = createDeviceNetworkUiState();
  local.projection = { hub_url: "https://hub.test/", device_id: "device-a", enrollment: "active" } as typeof local.projection;
  let current = target("session-a");
  let renders = 0;
  const context = { uiState: { deviceNetwork: local }, getProjection: () => ({ draft_target: current, hub_project_open: false }),
    rerender: () => { renders++; } } as unknown as ActionContext;
  return { local, context, select: (id: string) => { current = target(id); }, renders: () => renders };
}

test("late origin lookup cannot attach another chat's apps or overwrite its draft", async () => {
  const { local, context, select } = contextFixture();
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  let resolveA!: (value: OriginWorkProjection) => void;
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (_name: string, args: { expectedTarget: ReturnType<typeof target> }) =>
    args.expectedTarget.sessionId === "session-a" ? new Promise<OriginWorkProjection>(resolve => { resolveA = resolve; }) : origin("session-b") } } });
  try {
    const a = refreshOriginWork(context);
    select("session-b");
    await refreshOriginWork(context);
    resolveA(origin("session-a"));
    await a;
    assert.equal(local.originWork?.origin_session_ref, "session-b");
    assert.equal(renderOriginWork(deviceNetworkPresentation(local), "session-a"), "");
    assert.match(renderOriginWork(deviceNetworkPresentation(local), "session-b"), /WinBでアプリ/);
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});

test("only exact authorized retained apps can be stopped from the current ordinary chat", async () => {
  const { local, context } = contextFixture();
  local.originOwner = JSON.stringify(["C:/workspace", "session-a", "4", "https://hub.test/", "device-a"]);
  local.originWork = origin("session-a");
  assert.equal(originAppsStopEnabled(deviceNetworkPresentation(local)), true);
  const visible = renderOriginWork(deviceNetworkPresentation(local), "session-a");
  assert.match(visible, /起動中のアプリを停止/);
  assert.match(visible, /アプリの停止では、現在の回答や実行中の仕事は止まりません/);
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  let sent: unknown;
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (name: string, args: unknown) => {
    assert.equal(name, "origin_work_stop_apps"); sent = args; return { ...origin("session-a"), retained_services: [] };
  } } } });
  try {
    await stopOriginApps(context);
    assert.deepEqual(sent, { expectedTarget: target("session-a"), expectedServiceIds: ["service-a"] });
    assert.equal(local.originWork?.retained_services.length, 0);
    local.originWork = origin("session-a");
    local.originWork.retained_services[0].service.can_stop = false;
    assert.equal(originAppsStopEnabled(deviceNetworkPresentation(local)), false);
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});

test("a pending remote Stop stays visible in its ordinary chat even before Hub lists work", () => {
  const { local } = contextFixture();
  local.originOwner = JSON.stringify(["C:/workspace", "session-a", "4", "https://hub.test/", "device-a"]);
  local.originWork = { origin_session_ref: "session-a", jobs: [], retained_services: [],
    observed_at_ms: Date.now(), admission_revision: "7", stop_pending: true, stop_error: "transport uncertain" };
  const visible = renderOriginWork(deviceNetworkPresentation(local), "session-a");
  assert.match(visible, /Hubが停止要求を受け付けたか確認しています/);
  assert.match(visible, /接続が戻ると再確認/);
  assert.equal(renderOriginWork(deviceNetworkPresentation(local), "session-b"), "");
});

test("a local-only running turn keeps the ordinary Stop without a misleading PC card", () => {
  const { local } = contextFixture();
  local.originOwner = JSON.stringify(["C:/workspace", "session-a", "4", "https://hub.test/", "device-a"]);
  local.originWork = { origin_session_ref: "session-a", jobs: [], retained_services: [],
    observed_at_ms: Date.now(), admission_revision: "7" };
  assert.equal(originAllStopEnabled(deviceNetworkPresentation(local)), false);
  assert.equal(renderOriginWork(deviceNetworkPresentation(local), "session-a", true), "");
});

test("assigned and child-waiting origin jobs keep their actual status and whole-chat Stop", () => {
  const { local } = contextFixture();
  local.originOwner = JSON.stringify(["C:/workspace", "session-a", "4", "https://hub.test/", "device-a"]);
  local.originWork = { ...origin("session-a"), retained_services: [] };
  for (const [state, label] of [["assigned", "実行準備中"], ["waiting_child", "サブエージェントの結果待ち"]]) {
    local.originWork.jobs[0].job.state = state;
    const presentation = deviceNetworkPresentation(local);
    const visible = renderOriginWork(presentation, "session-a");
    assert.ok(visible.includes(label), `${state} must display its current Hub state`);
    assert.doesNotMatch(visible, /状態不明/);
    assert.equal(originAllStopEnabled(presentation), true, `${state} is still active`);
    assert.match(visible, /data-action="origin-stop-all"/);
  }
  local.originWork.jobs[0].job.state = "succeeded";
  assert.equal(originAllStopEnabled(deviceNetworkPresentation(local)), false);
});

test("all-stop forwards the exact chat, revision and live local Stop target", async () => {
  const { local, context } = contextFixture();
  local.originOwner = JSON.stringify(["C:/workspace", "session-a", "4", "https://hub.test/", "device-a"]);
  local.originWork = origin("session-a");
  const stopTarget = { kind: "turn", workspacePath: "C:/workspace", sessionId: "session-a",
    turnId: "turn-a", admissionRevision: "7", rootEpoch: "2" };
  context.getProjection = () => ({ draft_target: target("session-a"), hub_project_open: false,
    can_cancel_run: true, stop_target: stopTarget }) as ReturnType<ActionContext["getProjection"]>;
  assert.equal(originAllStopEnabled(deviceNetworkPresentation(local)), true);
  const visible = renderOriginWork(deviceNetworkPresentation(local), "session-a", true);
  assert.match(visible, /この会話の実行をすべて停止/);
  assert.match(visible, /現在の回答、別PCの仕事、起動中のアプリに停止を要求します/);
  assert.match(visible, /Hubの受付後も、停止したことを確認してください/);
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  let sent: unknown;
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (name: string, args: unknown) => {
    assert.equal(name, "origin_work_stop_all"); sent = args;
    return { ...origin("session-a"), stop_pending: true };
  } } } });
  try {
    await stopOriginAll(context);
    assert.deepEqual(sent, { expectedTarget: target("session-a"), expectedAdmissionRevision: "7", expectedStopTarget: stopTarget });
    assert.equal(local.originWork?.stop_pending, true);
    assert.equal(originAllStopEnabled(deviceNetworkPresentation(local)), false);
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});

import assert from "node:assert/strict";
import test from "node:test";
import type { ActionContext } from "../src/actions.ts";
import { refreshDeviceExecution } from "../src/device_execution.ts";
import { refreshDeviceNetworkJobs } from "../src/device_network_actions.ts";
import { deviceNetworkPresentation } from "../src/device_network_state.ts";
import { InteractionLifecycle } from "../src/interaction_lifecycle.ts";
import { refreshMcpHistory } from "../src/mcp_history_actions.ts";
import { mcpHistoryPresentation } from "../src/mcp_history_state.ts";
import { refreshOriginWork, type OriginWorkProjection } from "../src/origin_work.ts";
import { refreshReceiverActivity, type ReceiverActivityProjection } from "../src/receiver_activity.ts";
import { createDesktopRenderModel, desktopRenderRequired, DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION } from "../src/render_projection.ts";
import { refreshSharedWork } from "../src/shared_work_actions.ts";
import { sharedWorkPresentation } from "../src/shared_work_state.ts";
import type { DesktopViewState, DesktopWebState } from "../src/types.ts";
import { createUiLocalState } from "../src/ui_state.ts";
import { deviceProjection } from "./device_network_fixture.ts";
import { sharedProjection, sharedUiFixture } from "./shared_work_fixture.ts";

const idleReceiver = (): ReceiverActivityProjection => ({ runner_id: "runner-a", attempts: [], retained_services: [], observed_at_ms: 1, unavailable: false });
const origin = (): OriginWorkProjection => ({ origin_session_ref: "session-a", jobs: [], retained_services: [], observed_at_ms: 1, admission_revision: "7" });

function fixture() {
  const local = createUiLocalState();
  local.sharedWork = sharedUiFixture();
  local.deviceNetwork.receiverActivity = idleReceiver();
  const view = { projection_revision: "1", hub_project_open: false, overlay: "none", confirmation_visible: false,
    draft_target: { workspacePath: "C:/workspace", sessionId: "session-a", ownerGeneration: "1" },
    transcript_rows: [{ row_kind: "assistant", stable_history_identity: "history:1", title: "Assistant", body: "unchanged", file_changes: [] }] } as unknown as DesktopViewState;
  const model = () => createDesktopRenderModel(view, { ...DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION,
    sharedWork: sharedWorkPresentation(local.sharedWork), deviceNetwork: deviceNetworkPresentation(local.deviceNetwork),
    mcpHistory: mcpHistoryPresentation(local.mcpHistory) });
  let previous = model();
  let renders = 0;
  const forces: boolean[] = [];
  const context = { uiState: local, getProjection: () => view as DesktopWebState, getViewState: () => view,
    rerender(force = true) {
      forces.push(force);
      const next = model();
      if (desktopRenderRequired(previous, next, force)) { renders++; previous = next; }
    } } as unknown as ActionContext;
  return { local, view, model, context, forces, renders: () => renders };
}

async function withInvoke(invoke: (name: string) => Promise<unknown>, run: () => Promise<void>) {
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke } } });
  try { await run(); }
  finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
}

test("idle shared and receiver polls do not redraw unchanged content as observation counters advance", async () => {
  const { local, context, forces, renders } = fixture();
  let revision = 1;
  await withInvoke(async name => {
    if (name === "receiver_activity_projection") return { ...idleReceiver(), observed_at_ms: revision * 600 };
    assert.ok(["shared_work_projection", "shared_work_command"].includes(name));
    return sharedProjection({ revision: String(++revision), observed_at_ms: revision * 600 });
  }, async () => {
    for (let poll = 0; poll < 6; poll++) {
      await refreshSharedWork(context);
      await refreshReceiverActivity(context);
    }
  });
  assert.equal(renders(), 0);
  assert.deepEqual(forces, Array(12).fill(false));
  assert.equal(local.sharedWork.projection?.revision, String(revision), "freshness owner still adopts each projection");
  assert.equal(local.deviceNetwork.receiverActivity?.observed_at_ms, revision * 600, "raw observation remains available");
});

test("passive receiver refresh still displays work, observation failure and recovery", async () => {
  const { context, renders } = fixture();
  let unavailable = false;
  let attempts: ReceiverActivityProjection["attempts"] = [];
  await withInvoke(async () => {
    if (unavailable) throw new Error("fixture unavailable");
    return { ...idleReceiver(), attempts, observed_at_ms: 600 };
  }, async () => {
    attempts = [{ attempt_id: "attempt-a", generation: "1", job_id: "job-a", project_id: "project-a",
      environment_id: "env-a", run_id: "run-a", state: "executing", local_state: "running" }];
    await refreshReceiverActivity(context); assert.equal(renders(), 1);
    unavailable = true;
    await refreshReceiverActivity(context); assert.equal(renders(), 2);
    await refreshReceiverActivity(context); assert.equal(renders(), 2, "unchanged failures retain the displayed DOM");
    unavailable = false; attempts = [];
    await refreshReceiverActivity(context); assert.equal(renders(), 3);
  });
});

test("other periodic readers request normal invalidation and retain their results", async () => {
  const { local, view, context, forces } = fixture();
  local.deviceNetwork.projection = deviceProjection();
  local.hub.tab = "devices";
  const execution = { revision: "1", state: "needs_setup", projects: [], review: null, directory: null, access_mode: null,
    accepting: false, can_pause: false, can_resume: false, unknown_attempts: [], error: null };
  await withInvoke(async name => {
    if (name === "device_network_jobs") return { incoming: [], outgoing: [] };
    if (name === "device_execution_projection") return execution;
    if (name === "origin_work_projection") return origin();
    if (name === "mcp_history_list") return { rows: [], next_offset: null, anchor: null };
    throw new Error(`unexpected fixture command ${name}`);
  }, async () => {
    view.overlay = "hub";
    await refreshDeviceNetworkJobs(context); await refreshDeviceExecution(context);
    view.overlay = "none";
    await refreshOriginWork(context);
    view.overlay = "mcp_history";
    await refreshMcpHistory(context, true);
  });
  assert.ok(forces.length >= 5);
  assert.ok(forces.every(force => force === false));
  assert.equal(local.deviceNetwork.execution?.state, "needs_setup");
  assert.equal(local.deviceNetwork.originWork?.origin_session_ref, "session-a");
  assert.equal(local.mcpHistory.loaded, true);
});

test("error timestamps and actual shared activity updates remain display-relevant", () => {
  const { local, model } = fixture();
  local.deviceNetwork.originWork = origin();
  const initial = model();
  local.deviceNetwork.originWork.observed_at_ms = 2000;
  local.deviceNetwork.receiverActivity!.observed_at_ms = 2000;
  assert.equal(desktopRenderRequired(initial, model(), false), false);
  assert.equal(model().local.deviceNetwork.originWork?.observed_at_ms, 2000, "model keeps full observations");
  local.deviceNetwork.originError = "cannot observe origin";
  local.deviceNetwork.receiverActivity!.unavailable = true;
  const failed = model();
  assert.equal(desktopRenderRequired(initial, failed, false), true);
  local.deviceNetwork.originWork.observed_at_ms = 3000;
  assert.equal(desktopRenderRequired(failed, model(), false), true, "origin error's last-confirmed time is visible");
  const originChanged = model();
  local.deviceNetwork.receiverActivity!.observed_at_ms = 3000;
  assert.equal(desktopRenderRequired(originChanged, model(), false), true, "receiver error's last-confirmed time is visible");
  const beforeActivity = model();
  local.sharedWork.projection!.transcript = { items: [], next_after: null, activity: {
    job_id: "job-a", attempt_id: "attempt-a", generation: 1, environment_id: "env-a", runner_id: "runner-a",
    revision: 1, observed_at_ms: 3000, items: [{ position: 1, kind: "assistant_message", payload: { text: "working" } }], truncated: false } };
  const activity = model();
  assert.equal(desktopRenderRequired(beforeActivity, activity, false), true);
  local.sharedWork.projection!.transcript!.activity!.observed_at_ms = 4000;
  assert.equal(desktopRenderRequired(activity, model(), false), true, "live activity's displayed update time is preserved");
  const beforeGeneration = model();
  local.sharedWork.projection!.generation = "2";
  assert.equal(desktopRenderRequired(beforeGeneration, model(), false), true, "command ownership is never masked");
});

test("passive projections settle after pointer, keyboard and IME without turning into forced renders", () => {
  const { model, local } = fixture();
  for (const kind of ["pointer", "key", "composition"] as const) {
    const previous = model();
    const lifecycle = new InteractionLifecycle<{ model: ReturnType<typeof model>; forceRender: boolean }>(() => true);
    let finish;
    if (kind === "pointer") { lifecycle.beginPointer(1); finish = lifecycle.capturePointerEnd(1); }
    else if (kind === "key") { lifecycle.beginKey("KeyA"); finish = lifecycle.captureKeyEnd("KeyA"); }
    else { lifecycle.beginComposition(); finish = lifecycle.captureCompositionEnd(); }
    const next = model();
    assert.equal(lifecycle.defer({ model: next, forceRender: false }, false, false), true);
    const settled = finish!()!;
    assert.equal(desktopRenderRequired(previous, settled.deferred!.model, settled.deferred!.forceRender), false);
    assert.equal(settled.renderCurrent, false);

    lifecycle.beginComposition();
    local.deviceNetwork.receiverError = `changed during ${kind}`;
    const changed = model();
    lifecycle.defer({ model: changed, forceRender: false }, true, desktopRenderRequired(previous, changed, false));
    assert.equal(lifecycle.captureCompositionEnd()!()!.renderCurrent, true, "changed local status settles once interaction ends");
    assert.equal(desktopRenderRequired(changed, changed, true), true, "explicit render work remains imperative");
  }
});

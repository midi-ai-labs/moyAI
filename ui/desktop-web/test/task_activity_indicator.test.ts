import assert from "node:assert/strict";
import test from "node:test";

import {
  classifyTaskActivity,
  reconcileTaskActivityAnimationEpoch,
  renderTaskActivityIndicator,
  taskActivityAnimationDelay,
  taskActivityStateForSessionRow,
  taskActivityStateForView,
  type TaskActivityAnimationEpoch,
  type TaskActivityAnimationInput,
} from "../src/task_activity_indicator.ts";
import { renderRunStatusStrip } from "../src/render.ts";
import type { DesktopWebState, TaskActivityState } from "../src/types.ts";
import { turnStopTarget } from "./stop_target_fixture.ts";

function input(
  nowMs: number,
  overrides: Partial<TaskActivityAnimationInput> = {},
): TaskActivityAnimationInput {
  return {
    activityState: "running",
    workspacePath: "C:/workspace",
    sessionId: "session-a",
    runtimeOwnerToken: "root:8",
    runStartMutationPending: false,
    nowMs,
    ...overrides,
  };
}

test("task activity presentation is typed, accessible, and idle-safe", () => {
  const expected = new Map<TaskActivityState, string>([
    ["running", "実行中"],
    ["finalizing", "最終反映中"],
    ["attention", "確認待ち"],
  ]);
  for (const [state, label] of expected) {
    assert.equal(classifyTaskActivity(state)?.label, label);
    const html = renderTaskActivityIndicator(state, { small: true });
    assert.match(html, new RegExp(`data-task-activity="${state}"`));
    assert.match(html, /class="task-activity-indicator small"/);
    assert.match(html, /role="img"/);
    assert.match(html, new RegExp(`aria-label="${label}"`));
  }
  assert.equal(classifyTaskActivity("idle"), null);
  assert.equal(renderTaskActivityIndicator("idle"), "");
  assert.equal(taskActivityStateForView("idle", true), "running");
  assert.equal(taskActivityStateForView("finalizing", true), "finalizing");
});

test("session rows derive background activity while selected lifecycle state remains authoritative", () => {
  const row = (
    loaded_status: "not_loaded" | "idle" | "active" | "system_error",
    pending_permission_requests = 0,
    pending_user_input_requests = 0,
  ) => ({ loaded_status, pending_permission_requests, pending_user_input_requests });

  assert.equal(taskActivityStateForSessionRow(row("active"), "idle"), "running");
  assert.equal(taskActivityStateForSessionRow(row("active", 1), "idle"), "attention");
  assert.equal(taskActivityStateForSessionRow(row("active", 0, 1), "idle"), "attention");
  assert.equal(taskActivityStateForSessionRow(row("active", 1, 1), "finalizing"), "finalizing");
  assert.equal(taskActivityStateForSessionRow(row("idle"), "running"), "running");
  assert.equal(taskActivityStateForSessionRow(row("system_error"), "attention"), "attention");
  for (const loadedStatus of ["not_loaded", "idle", "system_error"] as const) {
    assert.equal(taskActivityStateForSessionRow(row(loadedStatus), "idle"), "idle");
  }
});

test("run status strip exposes one typed state label", () => {
  for (const [state, label] of [
    ["running", "実行中"],
    ["finalizing", "最終反映中"],
    ["attention", "確認待ち"],
  ] as const) {
    const html = renderRunStatusStrip({
      can_cancel_run: true,
      stop_target: turnStopTarget(),
      task_activity_state: state,
      run_phase: "provider",
      run_active_step: "応答待ち",
      status_message: "running",
      latest_tool_summary: "ツール待機中",
    } as DesktopWebState);
    assert.match(html, new RegExp(`data-task-activity="${state}"`));
    assert.match(html, new RegExp(`<strong>${label}</strong>`));
    assert.match(html, /aria-hidden="true"/);
    assert.doesNotMatch(html, new RegExp(`aria-label="${label}"`));
    assert.match(html, /class="run-strip has-stop"/);
    assert.match(html, /data-action="cancel-run"/);
  }
});

test("activity status remains visible when the exact Stop capability is absent", () => {
  for (const [state, label] of [
    ["running", "実行中"],
    ["finalizing", "最終反映中"],
    ["attention", "確認待ち"],
  ] as const) {
    const html = renderRunStatusStrip({
      can_cancel_run: false,
      stop_target: null,
      task_activity_state: state,
      run_phase: "settlement",
      run_active_step: "反映中",
      status_message: "settling",
      latest_tool_summary: "結果を同期中",
    } as DesktopWebState);
    assert.match(html, /class="run-strip"/);
    assert.match(html, new RegExp(`data-task-activity="${state}"`));
    assert.match(html, new RegExp(`<strong>${label}</strong>`));
    assert.doesNotMatch(html, /data-action="cancel-run"/);
  }

  assert.equal(renderRunStatusStrip({
    can_cancel_run: true,
    stop_target: turnStopTarget(),
    task_activity_state: "idle",
  } as DesktopWebState), "");
});

test("animation epoch survives more than five ordinary 600ms poll projections", () => {
  let epoch: TaskActivityAnimationEpoch | null = null;
  for (const nowMs of [0, 600, 1200, 1800, 2400, 3000]) {
    epoch = reconcileTaskActivityAnimationEpoch(epoch, input(nowMs));
    assert.equal(epoch?.startedAtMs, 0);
    assert.equal(taskActivityAnimationDelay(epoch, nowMs), nowMs === 0 ? "0ms" : `-${nowMs}ms`);
  }
});

test("running, attention, finalizing, and root-to-tree retain one lifecycle epoch", () => {
  let epoch = reconcileTaskActivityAnimationEpoch(null, input(0));
  epoch = reconcileTaskActivityAnimationEpoch(epoch, input(600, { activityState: "attention" }));
  epoch = reconcileTaskActivityAnimationEpoch(epoch, input(1200, { activityState: "finalizing" }));
  epoch = reconcileTaskActivityAnimationEpoch(epoch, input(1800, {
    activityState: "running",
    runtimeOwnerToken: "tree:8",
  }));

  assert.equal(epoch?.startedAtMs, 0);
  assert.equal(epoch?.runtimeOwnerToken, "tree:8");
  assert.equal(taskActivityAnimationDelay(epoch, 1800), "-1800ms");
});

test("a pending start adopts its admitted root and newly bound session", () => {
  let epoch = reconcileTaskActivityAnimationEpoch(null, input(0, {
    activityState: "idle",
    sessionId: null,
    runtimeOwnerToken: "idle:7",
    runStartMutationPending: true,
  }));
  assert.equal(epoch?.provisionalRunStart, true);

  epoch = reconcileTaskActivityAnimationEpoch(epoch, input(600, {
    sessionId: "created-session",
    runtimeOwnerToken: "root:8",
    runStartMutationPending: true,
  }));
  epoch = reconcileTaskActivityAnimationEpoch(epoch, input(1200, {
    sessionId: "created-session",
    runtimeOwnerToken: "root:8",
  }));

  assert.equal(epoch?.startedAtMs, 0);
  assert.equal(epoch?.sessionId, "created-session");
  assert.equal(epoch?.provisionalRunStart, false);
});

test("true owner changes reset and inactive state clears the epoch", () => {
  const rootEight = reconcileTaskActivityAnimationEpoch(null, input(0));
  const rootNine = reconcileTaskActivityAnimationEpoch(rootEight, input(2400, {
    runtimeOwnerToken: "root:9",
  }));
  assert.equal(rootNine?.startedAtMs, 2400);

  const otherSession = reconcileTaskActivityAnimationEpoch(rootNine, input(3000, {
    sessionId: "session-b",
  }));
  assert.equal(otherSession?.startedAtMs, 3000);
  assert.equal(reconcileTaskActivityAnimationEpoch(otherSession, input(3600, {
    activityState: "idle",
  })), null);
});

test("non-finite and backward clocks produce a finite non-positive delay", () => {
  const invalidStart = reconcileTaskActivityAnimationEpoch(null, input(Number.NaN));
  assert.equal(invalidStart?.startedAtMs, 0);
  assert.equal(taskActivityAnimationDelay(invalidStart, Number.POSITIVE_INFINITY), "0ms");

  const laterStart = reconcileTaskActivityAnimationEpoch(null, input(1000));
  assert.equal(taskActivityAnimationDelay(laterStart, 500), "0ms");
  assert.equal(taskActivityAnimationDelay(laterStart, 1600), "-600ms");
});

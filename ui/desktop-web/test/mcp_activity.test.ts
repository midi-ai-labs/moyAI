import assert from "node:assert/strict";
import test from "node:test";
import { mcpActivityPresentation, renderMcpActivityStrip, type McpActivityProjection } from "../src/mcp_activity.ts";
import { renderRunStatusStrip } from "../src/render.ts";
import { reconcileTaskActivityAnimationEpoch, taskActivityAnimationDelay } from "../src/task_activity_indicator.ts";
import type { DesktopWebState } from "../src/types.ts";
import { createDesktopRenderModel, DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION, desktopRenderModelChanged } from "../src/render_projection.ts";
import type { DesktopViewState } from "../src/types.ts";

const activity = (overrides: Partial<McpActivityProjection> = {}): McpActivityProjection => ({
  running: 0, waiting: 0, awaiting_approval: 0, cancelling: 0, unavailable: false, ...overrides,
});

test("receiver-only work is visible without claiming or stopping the selected Main task", () => {
  const html = renderRunStatusStrip({
    task_activity_state: "idle", mcp_activity: activity({ running: 1 }),
  } as DesktopWebState);
  assert.match(html, /MCP実行中/);
  assert.match(html, /mcp-activity-badge/);
  assert.match(html, /data-task-activity="running"/);
  assert.match(html, /実行中 1件/);
  assert.match(html, /data-action="show-mcp-execution-history"/);
  assert.doesNotMatch(html, /data-action="cancel-run"/);
});

test("Main and multiple incoming jobs retain distinct labels and one combined receiver count", () => {
  const html = renderRunStatusStrip({
    task_activity_state: "running", can_cancel_run: false, stop_target: null,
    run_active_step: "応答待ち", status_message: "", latest_tool_summary: "",
    mcp_activity: activity({ running: 2, waiting: 1, awaiting_approval: 1, cancelling: 1 }),
  } as DesktopWebState);
  assert.match(html, /<strong>実行中<\/strong>/);
  assert.match(html, /<strong>MCP承認待ち<\/strong>/);
  assert.match(html, /実行中 2件 · 待機 1件 · 承認待ち 1件 · 停止処理中 1件/);
  assert.equal(mcpActivityPresentation(activity({ running: 2, waiting: 1, awaiting_approval: 1, cancelling: 1 }))?.total, 5);
});

test("waiting, approval and stopping are distinct; completion clears loading and errors do not imply idle", () => {
  assert.equal(mcpActivityPresentation(activity({ waiting: 1 }))?.label, "MCP待機中");
  assert.equal(mcpActivityPresentation(activity({ awaiting_approval: 1 }))?.state, "attention");
  assert.equal(mcpActivityPresentation(activity({ cancelling: 1 }))?.label, "MCP停止処理中");
  assert.match(renderMcpActivityStrip(activity({ unavailable: true })), /MCP状態を確認できません/);
  assert.equal(renderMcpActivityStrip(activity()), "");
  assert.equal(renderMcpActivityStrip(null), "");
});

test("receiver animation survives Main navigation, polling, count and state changes, then stops at zero", () => {
  let epoch = null;
  for (let i = 0; i < 10; i++) {
    epoch = reconcileTaskActivityAnimationEpoch(epoch, {
      activityState: mcpActivityPresentation(activity(i === 3 ? { awaiting_approval: 1 } : { running: i + 1 }))!.state,
      workspacePath: "mcp-receiver", sessionId: null, runtimeOwnerToken: "root:1",
      runStartMutationPending: false, nowMs: 1000 + i * 600,
    });
    assert.equal(epoch?.startedAtMs, 1000);
  }
  assert.equal(taskActivityAnimationDelay(epoch, 7000), "-6000ms");
  assert.equal(reconcileTaskActivityAnimationEpoch(epoch, {
    activityState: "idle", workspacePath: "mcp-receiver", sessionId: null,
    runtimeOwnerToken: "root:1", runStartMutationPending: false, nowMs: 7000,
  }), null);
});

test("unchanged receiver snapshots do not redraw while count and terminal changes invalidate the view", () => {
  const model = (revision: string, incoming: McpActivityProjection) => createDesktopRenderModel({
    projection_revision: revision, mcp_activity: incoming,
  } as DesktopViewState, DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION);
  const running = model("1", activity({ running: 1 }));
  assert.equal(desktopRenderModelChanged(running, model("2", activity({ running: 1 }))), false);
  assert.equal(desktopRenderModelChanged(running, model("3", activity({ running: 2 }))), true);
  assert.equal(desktopRenderModelChanged(running, model("4", activity())), true);
});

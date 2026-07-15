import assert from "node:assert/strict";
import test from "node:test";

import {
  agentActivityCounts,
  agentActivityRowsChanged,
  agentActivitySummary,
  orderedAgentActivityRows,
  stableAgentVisual,
} from "../src/agent_activity.ts";
import {
  renderAgentInspector,
  renderInlineAgentActivity,
  renderSubAgentSummaryTrigger,
} from "../src/render_agent_activity.ts";
import { actionById, type ActionContext } from "../src/actions.ts";
import { renderArtifactPane, setRenderContext } from "../src/render.ts";
import { renderConfirmation } from "../src/render_overlays.ts";
import { runCanBeCancelled, runSurfaceActive } from "../src/run_control.ts";
import type { AgentActivityRow, AgentStatus, DesktopWebState } from "../src/types.ts";
import {
  createUiLocalState,
  openAgentPane,
  reconcileAgentPaneState,
} from "../src/ui_state.ts";

test("agent rows retain spawn order and derive all counts from status", () => {
  const rows = [
    agentRow("/root/second", 2, "completed"),
    agentRow("/root/first", 1, "running", true),
    agentRow("/root/pending", 3, "pending_init"),
    agentRow("/root/error", 4, "errored"),
    agentRow("/root/interrupted", 5, "interrupted"),
    agentRow("/root/missing", 6, "not_found"),
    agentRow("/root/stopped", 7, "shutdown"),
  ];
  const originalOrder = rows.map((row) => row.agent_path);

  assert.deepEqual(
    orderedAgentActivityRows(rows).map((row) => row.agent_path),
    [
      "/root/first",
      "/root/second",
      "/root/pending",
      "/root/error",
      "/root/interrupted",
      "/root/missing",
      "/root/stopped",
    ],
  );
  assert.deepEqual(rows.map((row) => row.agent_path), originalOrder, "projection rows are not mutated");
  assert.deepEqual(agentActivityCounts(rows), {
    total: 7,
    active: 2,
    completed: 1,
    attention: 3,
    stopped: 1,
    updated: 1,
  });
  assert.equal(agentActivitySummary(rows, true), "2件作業中 · 1件完了 · 3件要確認 · 1件停止");
  assert.equal(agentActivitySummary([], true), "Sub Agentを準備中");
});

test("agent visual identity is stable and activity comparison observes visible changes", () => {
  const row = agentRow("/root/runtime", 1, "running");
  const same = { ...row };
  assert.deepEqual(stableAgentVisual(row.agent_path), stableAgentVisual(row.agent_path));
  assert.equal(agentActivityRowsChanged([row], [same]), false);

  same.current_activity = "テストを実行中";
  assert.equal(agentActivityRowsChanged([row], [same]), true);
  assert.equal(agentActivityRowsChanged([same], [{ ...same, updated: true }]), true);
});

test("inline and output renderers show projected activity in spawn order without raw reasoning", () => {
  const later = agentRow("/root/later", 2, "completed");
  later.task_name = "Later Agent";
  later.result_preview = "実装を完了";
  const first = agentRow("/root/first", 1, "running", true) as AgentActivityRow & { reasoning: string };
  first.task_name = "<First Agent>";
  first.current_activity = "型を <確認> 中";
  first.reasoning = "INTERNAL_CHAIN_OF_THOUGHT";
  const state = {
    agent_tree_active: true,
    agent_activity_rows: [later, first],
  } as DesktopWebState;

  const inline = renderInlineAgentActivity(state);
  assert.ok(inline.indexOf("&lt;First Agent&gt;") < inline.indexOf("Later Agent"));
  assert.match(inline, /1件作業中 · 1件完了/);
  assert.match(inline, /1件のSub Agentが更新しました/);
  assert.match(inline, /型を &lt;確認&gt; 中/);
  assert.match(inline, /<button[^>]+data-action="show-agent-pane"[^>]+data-agent-path="\/root\/first"/);
  assert.match(inline, /data-focus-key="agent-current:\/root\/first"/);
  assert.match(inline, /aria-controls="sub-agent-inspector" aria-expanded="false"/);
  assert.doesNotMatch(inline, /class="agent-inline-activity"[^>]*aria-live/);
  assert.match(inline, /class="agent-update-summary" role="status" aria-live="polite" aria-atomic="true"/);
  assert.doesNotMatch(inline, /INTERNAL_CHAIN_OF_THOUGHT/);

  const output = renderAgentInspector(state, null);
  assert.ok(output.indexOf("sub-agent:/root/first") < output.indexOf("sub-agent:/root/later"));
  assert.match(output, /data-details-key="sub-agent:\/root\/first"/);
  assert.match(output, /data-focus-key="sub-agent-summary:\/root\/first"/);
  assert.match(output, /実装を完了/);
  assert.doesNotMatch(output, /INTERNAL_CHAIN_OF_THOUGHT/);
  assert.equal(renderInlineAgentActivity({ agent_tree_active: false, agent_activity_rows: [] } as DesktopWebState), "");
});

test("terminal inline activity collapses to one history trigger and output keeps only a summary trigger", () => {
  const first = agentRow("/root/first", 1, "completed");
  first.result_preview = "完了しました";
  const second = agentRow("/root/second", 2, "errored");
  const state = { agent_tree_active: false, agent_activity_rows: [second, first] } as DesktopWebState;

  const inline = renderInlineAgentActivity(state);
  assert.match(inline, /class="agent-history-trigger"[^>]+data-action="show-agent-pane"/);
  assert.match(inline, /aria-controls="sub-agent-inspector" aria-expanded="false"/);
  assert.match(inline, /2件のSub Agentが作業しました/);
  assert.doesNotMatch(inline, /class="agent-chip/);
  assert.doesNotMatch(inline, /class="agent-inline-current/);

  const output = renderSubAgentSummaryTrigger(state);
  assert.match(output, /class="output-agent-trigger"[^>]+data-action="show-agent-pane"/);
  assert.match(output, /1件完了 · 1件要確認/);
  assert.doesNotMatch(output, /<details/);
});

test("agent inspector groups terminal states, preserves spawn order, and expands the selected row", () => {
  const completedLater = agentRow("/root/completed-later", 3, "completed");
  completedLater.result_preview = "later result";
  const completedFirst = agentRow("/root/completed-first", 1, "completed");
  const active = agentRow("/root/active", 2, "running");
  const attention = agentRow("/root/attention", 4, "interrupted");
  const state = {
    agent_tree_active: true,
    agent_activity_rows: [completedLater, attention, active, completedFirst],
  } as DesktopWebState;

  const inspector = renderAgentInspector(state, "/root/completed-later");
  assert.ok(inspector.indexOf('id="sub-agent-group-active"') < inspector.indexOf('id="sub-agent-group-attention"'));
  assert.ok(inspector.indexOf('id="sub-agent-group-attention"') < inspector.indexOf('id="sub-agent-group-completed"'));
  assert.ok(inspector.indexOf("/root/completed-first") < inspector.indexOf("/root/completed-later"));
  assert.match(
    inspector,
    /data-agent-path="\/root\/completed-later" open>[\s\S]*?<summary[^>]+aria-current="true"/,
  );
  assert.match(inspector, /依頼/);
  assert.match(inspector, /現在/);
  assert.match(inspector, /結果/);
  assert.match(inspector, /Session session-3/);

  setRenderContext({
    artifactPaneCollapsed: false,
    artifactPaneMode: "agents",
    selectedAgentPath: "/root/completed-later",
    attachmentTrayOpen: false,
    configDirty: false,
    configMutationPending: false,
    configOwnerMutationOpen: true,
    configDraftEditOpen: true,
    configDraftDiscardOpen: false,
    configDraftCommitOpen: false,
  });
  const pane = renderArtifactPane(state);
  assert.match(pane, /data-pane-mode="sub-agents"/);
  assert.match(pane, /id="sub-agent-inspector"/);
  assert.match(pane, /data-action="show-output-pane" aria-label="出力ペインに戻る"/);
  assert.match(pane, /data-action="toggle-artifact-pane"[^>]+aria-label="Sub Agentペインを閉じる"/);
});

test("agent pane selection is frontend-local and resets at owner or row boundaries", async () => {
  const first = agentRow("/root/first", 1, "completed");
  const active = agentRow("/root/active", 2, "running");
  const state = {
    workspace_path: "C:/workspace",
    draft_target: { workspacePath: "C:/workspace", sessionId: "root-session" },
    agent_activity_rows: [first, active],
  } as DesktopWebState;
  const ui = createUiLocalState();
  reconcileAgentPaneState(ui, state);
  assert.equal(openAgentPane(ui, state, "/root/first"), true);
  assert.equal(ui.artifactPaneMode, "agents");
  assert.equal(ui.selectedAgentPath, "/root/first");
  assert.equal(ui.focusSelectedAgentAfterRender, true);

  let rerenders = 0;
  let mutations = 0;
  await actionById("show-agent-pane")?.run(
    state,
    {
      uiState: ui,
      rerender: () => { rerenders += 1; },
      mutate: async () => { mutations += 1; },
    } as unknown as ActionContext,
    { index: -1, value: "/root/active" },
  );
  assert.equal(ui.selectedAgentPath, "/root/active");
  assert.equal(rerenders, 1);
  assert.equal(mutations, 0, "inspecting a child never invokes Rust or navigates to its session");

  await actionById("toggle-artifact-pane")?.run(
    state,
    {
      uiState: ui,
      rerender: () => { rerenders += 1; },
    } as unknown as ActionContext,
    { index: -1, value: "" },
  );
  assert.equal(ui.artifactPaneCollapsed, true);
  assert.equal(ui.artifactPaneMode, "output", "closing the inspector returns the collapsed button to Output");

  reconcileAgentPaneState(ui, { ...state, agent_activity_rows: [first] });
  assert.equal(ui.artifactPaneMode, "output");
  assert.equal(ui.selectedAgentPath, null);

  openAgentPane(ui, state, "/root/first");
  reconcileAgentPaneState(ui, {
    ...state,
    draft_target: { workspacePath: "C:/workspace", sessionId: "other-root-session" },
  });
  assert.equal(ui.artifactPaneMode, "output");
  assert.equal(ui.selectedAgentPath, null);
});

test("permission confirmation identifies the requesting Sub Agent and stays compatible when absent", () => {
  const state = {
    confirmation_visible: true,
    confirmation_id: "request-42",
    confirmation_text: "",
    confirmation: {
      summary: "shellを実行します",
      details: ["npm test"],
      targets: ["workspace"],
      outside_workspace: false,
      risks: [],
      agent_path: "/root/review",
      agent_task_name: "<Review Agent>",
    },
  } as DesktopWebState;
  const rendered = renderConfirmation(state);
  assert.match(rendered, /要求元/);
  assert.match(rendered, /&lt;Review Agent&gt;/);
  assert.match(rendered, /\/root\/review/);
  assert.doesNotMatch(rendered, /<Review Agent>/);
  assert.match(rendered, /data-action="abort-permission"[^>]+autofocus>実行せず、指示を変更する/);
  assert.match(rendered, /data-action="approve-permission"[^>]*>実行する/);
  assert.match(rendered, /現在のタスクを停止し、次の指示を待ちます/);
  assert.match(rendered, /data-permission-id="request-42"/);

  state.confirmation = {
    summary: "従来の確認",
    details: [],
    targets: [],
    outside_workspace: false,
    risks: [],
  };
  assert.doesNotMatch(renderConfirmation(state), /要求元/);
});

test("permission rendering is declarative, request-owned, and gives each new request a safe focus target", () => {
  const state = {
    confirmation_visible: true,
    confirmation_id: "B",
    confirmation_text: "確認",
    confirmation: {
      summary: "shellを実行します",
      details: ["npm test"],
      targets: ["workspace"],
      outside_workspace: false,
      risks: [],
    },
  } as DesktopWebState;

  const submitting = renderConfirmation(state, {
    phase: "submitting",
    requestId: "B",
    submissionId: 7,
    decision: "abort",
  });
  assert.match(submitting, /data-permission-id="B"[^>]+aria-busy="true"/);
  assert.equal(submitting.match(/data-permission-action[^>]+disabled/g)?.length, 2);
  assert.match(submitting, /現在のタスクを停止しています/);
  assert.match(submitting, /停止しています…/);
  assert.doesNotMatch(submitting, /data-permission-action[^>]+autofocus/);

  const failed = renderConfirmation(state, {
    phase: "failed",
    requestId: "B",
    error: "再試行 <B>",
  });
  assert.match(failed, /再試行 &lt;B&gt;/);
  assert.match(failed, /data-focus-key="permission:B:abort" autofocus/);
  assert.doesNotMatch(failed, /aria-busy="true"/);

  const newRequest = renderConfirmation(state, {
    phase: "failed",
    requestId: "A",
    error: "stale A error",
  });
  assert.doesNotMatch(newRequest, /stale A error/);
  assert.match(newRequest, /data-focus-key="permission:B:abort" autofocus/);
  assert.match(newRequest, /現在のタスクを停止し、次の指示を待ちます/);
});

test("permission actions send typed approve and abort decisions", async () => {
  const state = { confirmation_visible: true } as DesktopWebState;
  const decisions: string[] = [];
  const context = {
    submitPermissionDecision: async (decision: string) => {
      decisions.push(decision);
    },
  } as unknown as ActionContext;

  await actionById("approve-permission")?.run(state, context, { index: -1, value: "" });
  await actionById("abort-permission")?.run(state, context, { index: -1, value: "" });

  assert.deepEqual(decisions, ["approved", "abort"]);
});

test("run cancellation remains available while only the child agent tree is active", () => {
  assert.equal(runCanBeCancelled({ can_cancel_run: false }), false);
  assert.equal(runCanBeCancelled({ can_cancel_run: true }), true);
  assert.equal(runSurfaceActive({ busy: false, agent_tree_active: true }), true);
  assert.equal(runSurfaceActive({ busy: false, agent_tree_active: false }), false);
});

function agentRow(
  agentPath: string,
  startedOrder: number,
  status: AgentStatus,
  updated = false,
): AgentActivityRow {
  return {
    agent_path: agentPath,
    session_id: `session-${startedOrder}`,
    task_name: agentPath.split("/").pop() ?? "agent",
    task_preview: `${agentPath} の担当作業`,
    status,
    current_activity: status === "running" ? `${agentPath} を調査中` : "",
    result_preview: "",
    started_order: startedOrder,
    updated,
  };
}

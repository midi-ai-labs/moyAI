import assert from "node:assert/strict";
import test from "node:test";

import {
  agentActivityCounts,
  agentActivityRowsChanged,
  agentActivitySummary,
  orderedAgentActivityRows,
  selectedAgentActivityChanged,
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
import type {
  AgentActivityRow,
  AgentExecutionProjection,
  AgentStatus,
  DesktopWebState,
} from "../src/types.ts";
import {
  agentExecutionSnapshotOwnerIdentity,
  agentExecutionRequestNeedsRefresh,
  beginAgentExecutionLoad,
  beginPreviousAgentExecutionPageLoad,
  createUiLocalState,
  finishAgentExecutionLoad,
  openAgentPane,
  reconcileAgentPaneState,
  selectedAgentExecution,
  shouldPreserveAgentExecutionSnapshots,
} from "../src/ui_state.ts";

test("agent rows retain spawn order and derive all counts from status", () => {
  const rows = [
    agentRow("/root/second", 2, "completed"),
    agentRow("/root/first", 1, "running", true),
    agentRow("/root/pending", 3, "pending_init"),
    agentRow("/root/error", 4, "errored"),
    agentRow("/root/interrupted", 5, "interrupted"),
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
      "/root/stopped",
    ],
  );
  assert.deepEqual(rows.map((row) => row.agent_path), originalOrder, "projection rows are not mutated");
  assert.deepEqual(agentActivityCounts(rows), {
    total: 6,
    active: 2,
    completed: 1,
    attention: 2,
    stopped: 1,
    updated: 1,
  });
  assert.equal(agentActivitySummary(rows, true), "2件作業中 · 1件完了 · 2件要確認 · 1件停止");
  assert.equal(agentActivitySummary([], true), "Sub Agentを準備中");
});

test("agent visual identity is stable and activity comparison observes visible changes", () => {
  const row = agentRow("/root/runtime", 1, "running");
  const same = { ...row };
  assert.deepEqual(stableAgentVisual(row.agent_path), stableAgentVisual(row.agent_path));
  assert.equal(agentActivityRowsChanged([row], [same]), false);
  assert.equal(selectedAgentActivityChanged([row], [same], row.agent_path), false);

  same.current_activity = "テストを実行中";
  assert.equal(agentActivityRowsChanged([row], [same]), true);
  assert.equal(selectedAgentActivityChanged([row], [same], row.agent_path), true);
  assert.equal(selectedAgentActivityChanged([row], [same], "/root/unrelated"), false);
  assert.equal(agentActivityRowsChanged([same], [{ ...same, updated: true }]), true);
});

test("inline and output renderers show projected activity in spawn order without raw reasoning", () => {
  const later = agentRow("/root/later", 2, "completed");
  later.task_name = "Later Agent";
  later.result_preview = "**実装を完了** [詳細](https://example.invalid)";
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
  assert.match(inline, /data-focus-key="agent-job:\/root\/first"/);
  assert.match(inline, /class="agent-job-group"[^>]*open/);
  assert.match(inline, /data-details-key="sub-agent-inline-group:active"/);
  assert.match(inline, /class="agent-job-card[^>]+agent-status-completed/);
  assert.match(inline, /aria-controls="sub-agent-inspector" aria-expanded="false"/);
  assert.match(inline, /実装を完了 詳細/);
  assert.doesNotMatch(inline, /\*\*|https:\/\/example\.invalid/);
  assert.doesNotMatch(inline, /class="agent-inline-activity"[^>]*aria-live/);
  assert.match(inline, /class="agent-update-summary" role="status" aria-live="polite" aria-atomic="true"/);
  assert.doesNotMatch(inline, /INTERNAL_CHAIN_OF_THOUGHT/);

  const selectedInline = renderInlineAgentActivity(state, "/root/first");
  assert.match(
    selectedInline,
    /data-agent-path="\/root\/first"[^>]+aria-controls="sub-agent-inspector" aria-expanded="true"/,
  );
  assert.match(
    selectedInline,
    /data-agent-path="\/root\/later"[^>]+aria-controls="sub-agent-inspector" aria-expanded="false"/,
  );

  const output = renderAgentInspector(state, null);
  assert.ok(output.indexOf("sub-agent-card:/root/first") < output.indexOf("sub-agent-card:/root/later"));
  assert.match(output, /data-focus-key="sub-agent-card:\/root\/first"/);
  assert.match(output, /実装を完了/);
  assert.doesNotMatch(output, /INTERNAL_CHAIN_OF_THOUGHT/);
  assert.equal(renderInlineAgentActivity({ agent_tree_active: false, agent_activity_rows: [] } as DesktopWebState), "");
});

test("terminal inline activity keeps individual stable-icon jobs inside a collapsed group", () => {
  const first = agentRow("/root/first", 1, "completed");
  first.result_preview = "完了しました";
  const second = agentRow("/root/second", 2, "errored");
  const state = { agent_tree_active: false, agent_activity_rows: [second, first] } as DesktopWebState;

  const inline = renderInlineAgentActivity(state);
  assert.match(inline, /class="agent-job-group"/);
  assert.match(inline, /data-details-key="sub-agent-inline-group:terminal"/);
  assert.doesNotMatch(inline, /class="agent-job-group"[^>]*open/);
  assert.equal(inline.match(/class="agent-job-card/g)?.length, 2);
  assert.match(inline, /data-agent-path="\/root\/first"/);
  assert.match(inline, /data-agent-path="\/root\/second"/);
  assert.match(inline, /完了しました/);

  const output = renderSubAgentSummaryTrigger(state);
  assert.match(output, /class="output-agent-trigger"[^>]+data-action="show-agent-pane"/);
  assert.match(output, /1件完了 · 1件要確認/);
  assert.doesNotMatch(output, /<details/);
});

test("agent execution snapshots persist only for the same root, path, and child session", () => {
  const first = agentRow("/root/first", 1, "running");
  const second = agentRow("/root/second", 2, "running");
  const state = {
    workspace_path: "C:/workspace",
    draft_target: { workspacePath: "C:/workspace", sessionId: "root-session", ownerGeneration: 1 },
    agent_activity_rows: [first, second],
  } as DesktopWebState;
  const firstOwner = agentExecutionSnapshotOwnerIdentity(state, first.agent_path);

  assert.ok(firstOwner);
  assert.equal(
    shouldPreserveAgentExecutionSnapshots(
      firstOwner,
      agentExecutionSnapshotOwnerIdentity({ ...state, agent_activity_rows: [{ ...first }, second] }, first.agent_path),
    ),
    true,
    "polling the same child preserves its execution scroll and disclosures",
  );
  assert.equal(
    shouldPreserveAgentExecutionSnapshots(firstOwner, agentExecutionSnapshotOwnerIdentity(state, second.agent_path)),
    false,
    "selecting another child cannot inherit the previous execution state",
  );
  assert.equal(
    shouldPreserveAgentExecutionSnapshots(
      firstOwner,
      agentExecutionSnapshotOwnerIdentity({
        ...state,
        agent_activity_rows: [{ ...first, session_id: "replacement-child" }, second],
      }, first.agent_path),
    ),
    false,
    "reusing a path for a different child session resets execution state",
  );
  assert.equal(
    shouldPreserveAgentExecutionSnapshots(
      firstOwner,
      agentExecutionSnapshotOwnerIdentity({
        ...state,
        draft_target: { ...state.draft_target, sessionId: "other-root" },
      }, first.agent_path),
    ),
    false,
    "changing the root session resets execution state",
  );
  assert.equal(shouldPreserveAgentExecutionSnapshots(firstOwner, null), false);
});

test("agent inspector separates ordered list and selected execution detail", () => {
  const completedLater = agentRow("/root/completed-later", 3, "completed");
  completedLater.result_preview = "later result";
  const completedFirst = agentRow("/root/completed-first", 1, "completed");
  const active = agentRow("/root/active", 2, "running");
  const attention = agentRow("/root/attention", 4, "interrupted");
  const state = {
    agent_tree_active: true,
    agent_activity_rows: [completedLater, attention, active, completedFirst],
  } as DesktopWebState;

  const list = renderAgentInspector(state, null);
  assert.ok(list.indexOf('id="sub-agent-group-active"') < list.indexOf('id="sub-agent-group-attention"'));
  assert.ok(list.indexOf('id="sub-agent-group-attention"') < list.indexOf('id="sub-agent-group-completed"'));
  assert.ok(list.indexOf("/root/completed-first") < list.indexOf("/root/completed-later"));
  assert.match(list, /data-action="show-agent-pane" data-agent-path="\/root\/completed-later"/);

  const projection = executionProjection(completedLater, [{
    row_kind: "assistant",
    step: "1",
    title: "Assistant",
    body: "later result",
    file_changes: [],
  }]);
  const detail = renderAgentInspector(state, "/root/completed-later", {
    status: "ready",
    generation: 1,
    expectedTarget: executionTarget(completedLater),
    projection,
    error: "",
  });
  assert.match(detail, /class="agent-execution"[^>]+data-agent-path="\/root\/completed-later"/);
  assert.match(detail, /data-focus-key="agent-execution:\/root\/completed-later"/);
  assert.match(detail, /later result/);
  assert.match(detail, /1件の履歴/);
  assert.doesNotMatch(detail, />Assistant</);

  projection.turn_page_offset = 80;
  projection.turn_page_end = 160;
  projection.turn_page_total = 160;
  projection.turn_page_has_previous = true;
  const boundedDetail = renderAgentInspector(state, "/root/completed-later", {
    status: "ready",
    generation: 1,
    expectedTarget: executionTarget(completedLater),
    projection,
    error: "",
  });
  assert.match(boundedDetail, /1件を表示 · 以前の実行履歴あり/);
  assert.match(
    boundedDetail,
    /data-action="load-previous-agent-execution-page"[\s\S]*?data-agent-path="\/root\/completed-later"/,
  );
  assert.match(boundedDetail, />\s*以前の実行履歴\s*</);
  assert.doesNotMatch(boundedDetail, /1\/160件/);

  setRenderContext({
    artifactPaneCollapsed: false,
    artifactPaneMode: "agents",
    selectedAgentPath: "/root/completed-later",
    selectedAgentExecution: {
      status: "ready",
      generation: 1,
      expectedTarget: executionTarget(completedLater),
      projection,
      error: "",
    },
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
  assert.match(pane, /data-action="show-agent-list"[\s\S]*?aria-label="Sub Agent一覧に戻る"/);
  assert.match(pane, /data-focus-key="agent-pane-back" aria-label="Sub Agent一覧に戻る"/);
  assert.match(pane, /completed-later/);
  assert.match(pane, /data-action="toggle-artifact-pane"[^>]+aria-label="Sub Agentペインを閉じる"/);

  setRenderContext({
    artifactPaneCollapsed: false,
    artifactPaneMode: "agents",
    selectedAgentPath: null,
    selectedAgentExecution: null,
    attachmentTrayOpen: false,
    configDirty: false,
    configMutationPending: false,
    configOwnerMutationOpen: true,
    configDraftEditOpen: true,
    configDraftDiscardOpen: true,
    configDraftCommitOpen: true,
  });
  const listPane = renderArtifactPane(state);
  assert.match(listPane, /data-action="show-output-pane"/);
  assert.match(listPane, /data-focus-key="agent-pane-back" aria-label="出力ペインに戻る"/);
});

test("agent pane selection is frontend-local and resets at owner or row boundaries", async () => {
  const first = agentRow("/root/first", 1, "completed");
  const active = agentRow("/root/active", 2, "running");
  const state = {
    workspace_path: "C:/workspace",
    draft_target: { workspacePath: "C:/workspace", sessionId: "root-session", ownerGeneration: 1 },
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
  let executionLoads = 0;
  await actionById("show-agent-pane")?.run(
    state,
    {
      uiState: ui,
      rerender: () => { rerenders += 1; },
      mutate: async () => { mutations += 1; },
      loadAgentExecution: async () => { executionLoads += 1; },
    } as unknown as ActionContext,
    { index: -1, value: "/root/active" },
  );
  assert.equal(ui.selectedAgentPath, "/root/active");
  assert.equal(rerenders, 1);
  assert.equal(mutations, 0, "inspecting a child never navigates or mutates its session");
  assert.equal(executionLoads, 1, "inspecting a child invokes the dedicated read-only loader");

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
    draft_target: { workspacePath: "C:/workspace", sessionId: "other-root-session", ownerGeneration: 2 },
  });
  assert.equal(ui.artifactPaneMode, "output");
  assert.equal(ui.selectedAgentPath, null);
});

test("agent pane back actions request stable focus targets across each rerender", async () => {
  const first = agentRow("/root/first", 1, "completed");
  const state = {
    workspace_path: "C:/workspace",
    draft_target: { workspacePath: "C:/workspace", sessionId: "root-session", ownerGeneration: 1 },
    agent_activity_rows: [first],
  } as DesktopWebState;
  const ui = createUiLocalState();
  reconcileAgentPaneState(ui, state);
  openAgentPane(ui, state, first.agent_path);
  let rerenders = 0;
  const context = {
    uiState: ui,
    rerender: () => { rerenders += 1; },
  } as unknown as ActionContext;

  await actionById("show-agent-list")?.run(state, context, { index: -1, value: "" });
  assert.equal(ui.selectedAgentPath, null);
  assert.equal(ui.agentPaneFocusAfterRender, "agent-pane-back");

  await actionById("show-output-pane")?.run(state, context, { index: -1, value: "" });
  assert.equal(ui.artifactPaneMode, "output");
  assert.equal(ui.agentPaneFocusAfterRender, "output-agent-trigger");
  assert.equal(rerenders, 2);
});

test("agent execution cache accepts only the selected generation and current owner", () => {
  const first = agentRow("/root/first", 1, "completed");
  const second = agentRow("/root/second", 2, "running");
  const state = {
    workspace_path: "C:/workspace",
    draft_target: { workspacePath: "C:/workspace", sessionId: "root-session", ownerGeneration: 1 },
    agent_activity_rows: [first, second],
  } as DesktopWebState;
  const ui = createUiLocalState();
  reconcileAgentPaneState(ui, state);

  openAgentPane(ui, state, first.agent_path);
  const firstRequest = beginAgentExecutionLoad(ui, state, first);
  openAgentPane(ui, state, second.agent_path);
  const secondRequest = beginAgentExecutionLoad(ui, state, second);

  assert.equal(finishAgentExecutionLoad(ui, firstRequest, executionProjection(first)), false);
  assert.equal(selectedAgentExecution(ui, state)?.status, "loading");
  assert.equal(finishAgentExecutionLoad(ui, secondRequest, executionProjection(second)), true);
  assert.equal(selectedAgentExecution(ui, state)?.projection?.agent_path, second.agent_path);

  const mismatchedRequest = beginAgentExecutionLoad(ui, state, second);
  const mismatched = executionProjection(second);
  mismatched.agent_path = first.agent_path;
  assert.equal(finishAgentExecutionLoad(ui, mismatchedRequest, mismatched), true);
  assert.equal(selectedAgentExecution(ui, state)?.status, "error");
  assert.equal(selectedAgentExecution(ui, state)?.projection?.agent_path, second.agent_path);

  openAgentPane(ui, state, first.agent_path);
  const ownerStaleRequest = beginAgentExecutionLoad(ui, state, first);
  const nextOwner = {
    ...state,
    draft_target: { workspacePath: "C:/workspace", sessionId: "other-root", ownerGeneration: 2 },
  };
  reconcileAgentPaneState(ui, nextOwner);
  assert.equal(finishAgentExecutionLoad(ui, ownerStaleRequest, executionProjection(first)), false);
  assert.equal(ui.agentExecutionCache.size, 0);
});

test("a selected agent activity change during a read requires an immediate latest refresh", () => {
  const running = agentRow("/root/reviewer", 1, "running");
  const state = {
    workspace_path: "C:/workspace",
    draft_target: { workspacePath: "C:/workspace", sessionId: "root-session", ownerGeneration: 1 },
    agent_activity_rows: [running],
  } as DesktopWebState;
  const ui = createUiLocalState();
  reconcileAgentPaneState(ui, state);
  openAgentPane(ui, state, running.agent_path);

  const request = beginAgentExecutionLoad(ui, state, running);
  assert.equal(agentExecutionRequestNeedsRefresh(request, state), false);

  const terminalState = {
    ...state,
    agent_activity_rows: [{
      ...running,
      status: "completed" as const,
      current_activity: "",
      result_preview: "final review",
      updated: true,
    }],
  };
  assert.equal(
    agentExecutionRequestNeedsRefresh(request, terminalState),
    true,
    "a response started before the terminal activity cannot be the final pane snapshot",
  );
});

test("agent execution previous pages replace with one contiguous reprojected range", () => {
  const row = agentRow("/root/history", 1, "completed");
  const state = {
    workspace_path: "C:/workspace",
    draft_target: { workspacePath: "C:/workspace", sessionId: "root-session", ownerGeneration: 1 },
    agent_activity_rows: [row],
  } as DesktopWebState;
  const ui = createUiLocalState();
  reconcileAgentPaneState(ui, state);
  openAgentPane(ui, state, row.agent_path);

  const latestRequest = beginAgentExecutionLoad(ui, state, row);
  const latest = executionProjection(row, [{
    row_kind: "assistant",
    step: "newer",
    title: "Assistant",
    body: "newer row",
    file_changes: [],
  }]);
  latest.turn_page_offset = 80;
  latest.turn_page_end = 160;
  latest.turn_page_total = 160;
  latest.turn_page_has_previous = true;
  assert.equal(finishAgentExecutionLoad(ui, latestRequest, latest), true);

  const previousRequest = beginPreviousAgentExecutionPageLoad(ui, state, row);
  assert.ok(previousRequest);
  assert.equal(previousRequest.expectedOffset, 80);
  assert.equal(previousRequest.expectedEnd, 160);
  assert.equal(
    beginPreviousAgentExecutionPageLoad(ui, state, row),
    null,
    "the previous-page cache owner is single-flight",
  );
  const previous = executionProjection(row, [
    {
      row_kind: "user",
      step: "older",
      title: "User",
      body: "older row",
      file_changes: [],
    },
    {
      row_kind: "assistant",
      step: "newer",
      title: "Assistant",
      body: "newer row",
      file_changes: [],
    },
  ]);
  previous.turn_page_offset = 0;
  previous.turn_page_end = 160;
  previous.turn_page_total = 160;
  previous.turn_page_has_previous = false;
  assert.equal(finishAgentExecutionLoad(ui, previousRequest, previous), true);

  const merged = selectedAgentExecution(ui, state);
  assert.equal(merged?.status, "ready");
  assert.equal(merged?.projection?.turn_page_offset, 0);
  assert.equal(merged?.projection?.turn_page_total, 160);
  assert.deepEqual(
    merged?.projection?.transcript_rows.map((transcriptRow) => transcriptRow.body),
    ["older row", "newer row"],
  );
  assert.equal(beginPreviousAgentExecutionPageLoad(ui, state, row), null);
});

test("agent execution rejects a non-contiguous previous page and preserves the loaded suffix", () => {
  const row = agentRow("/root/history", 1, "completed");
  const state = {
    workspace_path: "C:/workspace",
    draft_target: { workspacePath: "C:/workspace", sessionId: "root-session", ownerGeneration: 1 },
    agent_activity_rows: [row],
  } as DesktopWebState;
  const ui = createUiLocalState();
  reconcileAgentPaneState(ui, state);
  openAgentPane(ui, state, row.agent_path);

  const latestRequest = beginAgentExecutionLoad(ui, state, row);
  const latest = executionProjection(row, [{
    row_kind: "assistant",
    step: "newer",
    title: "Assistant",
    body: "newer row",
    file_changes: [],
  }]);
  latest.turn_page_offset = 80;
  latest.turn_page_end = 160;
  latest.turn_page_total = 160;
  latest.turn_page_has_previous = true;
  finishAgentExecutionLoad(ui, latestRequest, latest);

  const previousRequest = beginPreviousAgentExecutionPageLoad(ui, state, row);
  assert.ok(previousRequest);
  const nonContiguous = executionProjection(row, [{
    row_kind: "user",
    step: "wrong",
    title: "User",
    body: "wrong page",
    file_changes: [],
  }]);
  nonContiguous.turn_page_offset = 80;
  nonContiguous.turn_page_end = 160;
  nonContiguous.turn_page_total = 160;
  nonContiguous.turn_page_has_previous = true;
  assert.equal(finishAgentExecutionLoad(ui, previousRequest, nonContiguous), true);

  const cached = selectedAgentExecution(ui, state);
  assert.equal(cached?.status, "error");
  assert.equal(cached?.projection?.turn_page_offset, 80);
  assert.deepEqual(
    cached?.projection?.transcript_rows.map((transcriptRow) => transcriptRow.body),
    ["newer row"],
  );
  assert.match(cached?.error ?? "", /連続していません/);
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

test("run cancellation remains available while only the child agent tree is active and carries its owner", async () => {
  assert.equal(runCanBeCancelled({ can_cancel_run: false }), false);
  assert.equal(runCanBeCancelled({ can_cancel_run: true }), true);
  assert.equal(runSurfaceActive({ busy: false, agent_tree_active: true }), true);
  assert.equal(runSurfaceActive({ busy: false, agent_tree_active: false }), false);

  const expectedTarget = {
    workspacePath: "C:/workspace",
    sessionId: "root-session",
    runtimeOwnerToken: "tree:17",
  };
  let dispatched: { name: string; args?: Record<string, unknown> } | null = null;
  await actionById("cancel-run")?.run(
    { can_cancel_run: true, run_target: expectedTarget } as DesktopWebState,
    {
      mutate: async (name: string, args?: Record<string, unknown>) => {
        dispatched = { name, args };
      },
    } as unknown as ActionContext,
    { index: -1, value: "" },
  );
  assert.deepEqual(dispatched, {
    name: "cancel_run",
    args: { expectedTarget },
  });
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

function executionTarget(row: AgentActivityRow) {
  return {
    workspacePath: "C:/workspace",
    rootSessionId: "root-session",
    agentPath: row.agent_path,
    childSessionId: row.session_id,
  };
}

function executionProjection(
  row: AgentActivityRow,
  transcriptRows: AgentExecutionProjection["transcript_rows"] = [],
): AgentExecutionProjection {
  return {
    workspace_path: "C:/workspace",
    root_session_id: "root-session",
    agent_path: row.agent_path,
    session_id: row.session_id,
    task_name: row.task_name,
    transcript_rows: transcriptRows,
    turn_page_offset: 0,
    turn_page_end: transcriptRows.length,
    turn_page_total: transcriptRows.length,
    turn_page_has_previous: false,
  };
}

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
  renderInlineAgentActivity,
  renderSubAgentSection,
} from "../src/render_agent_activity.ts";
import { renderConfirmation } from "../src/render_overlays.ts";
import { runCanBeCancelled, runSurfaceActive } from "../src/run_control.ts";
import type { AgentActivityRow, AgentStatus, DesktopWebState } from "../src/types.ts";

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
  assert.doesNotMatch(inline, /class="agent-inline-activity"[^>]*aria-live/);
  assert.match(inline, /class="agent-update-summary" role="status" aria-live="polite" aria-atomic="true"/);
  assert.doesNotMatch(inline, /INTERNAL_CHAIN_OF_THOUGHT/);

  const output = renderSubAgentSection(state);
  assert.ok(output.indexOf("sub-agent:/root/first") < output.indexOf("sub-agent:/root/later"));
  assert.match(output, /data-details-key="sub-agent:\/root\/first"/);
  assert.match(output, /data-focus-key="sub-agent-summary:\/root\/first"/);
  assert.match(output, /実装を完了/);
  assert.doesNotMatch(output, /INTERNAL_CHAIN_OF_THOUGHT/);
  assert.equal(renderInlineAgentActivity({ agent_tree_active: false, agent_activity_rows: [] } as DesktopWebState), "");
});

test("permission confirmation identifies the requesting Sub Agent and stays compatible when absent", () => {
  const state = {
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

  state.confirmation = {
    summary: "従来の確認",
    details: [],
    targets: [],
    outside_workspace: false,
    risks: [],
  };
  assert.doesNotMatch(renderConfirmation(state), /要求元/);
});

test("run cancellation remains available while only the child agent tree is active", () => {
  assert.equal(runCanBeCancelled({ busy: false, confirmation_visible: false, agent_tree_active: false }), false);
  assert.equal(runCanBeCancelled({ busy: true, confirmation_visible: false, agent_tree_active: false }), true);
  assert.equal(runCanBeCancelled({ busy: false, confirmation_visible: true, agent_tree_active: false }), true);
  assert.equal(runCanBeCancelled({ busy: false, confirmation_visible: false, agent_tree_active: true }), true);
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

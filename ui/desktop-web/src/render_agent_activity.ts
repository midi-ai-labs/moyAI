import {
  agentActivityCounts,
  agentActivitySummary,
  agentDisplayName,
  agentIsActive,
  agentStatusLabel,
  orderedAgentActivityRows,
  stableAgentVisual,
} from "./agent_activity.ts";
import type { AgentActivityRow, DesktopWebState } from "./types.ts";
import { escapeHtml } from "./utils.ts";

export function renderInlineAgentActivity(state: DesktopWebState, inspectorOpen = false): string {
  const rows = orderedAgentActivityRows(state.agent_activity_rows ?? []);
  if (!state.agent_tree_active && rows.length === 0) return "";
  if (!state.agent_tree_active) {
    return `
      <section class="agent-inline-activity agent-inline-history" aria-label="Sub Agentの履歴">
        <button type="button" class="agent-history-trigger" data-action="show-agent-pane" data-focus-key="agent-history-trigger"
          aria-controls="sub-agent-inspector" aria-expanded="${inspectorOpen ? "true" : "false"}">
          <span><strong>${rows.length}件のSub Agentが作業しました</strong><small>${escapeHtml(agentActivitySummary(rows, false))}</small></span>
          <span aria-hidden="true">›</span>
        </button>
      </section>
    `;
  }
  const counts = agentActivityCounts(rows);
  const updateText = counts.updated > 0 ? `${counts.updated}件のSub Agentが更新しました` : "";
  const currentRows = rows.filter((row) => activityPreview(row).length > 0);
  return `
    <section class="agent-inline-activity" aria-label="Sub Agentの活動">
      <div class="agent-inline-heading">
        <strong>${escapeHtml(agentActivitySummary(rows, state.agent_tree_active))}</strong>
        ${updateText ? `<span class="agent-update-summary" role="status" aria-live="polite" aria-atomic="true">${escapeHtml(updateText)}</span>` : ""}
      </div>
      ${rows.length > 0 ? `<div class="agent-chip-list">${rows.map((row) => renderAgentChip(row, inspectorOpen)).join("")}</div>` : renderAgentTreePending()}
      ${
        currentRows.length > 0
          ? `<div class="agent-current-list">${currentRows.map((row) => renderInlineCurrentActivity(row, inspectorOpen)).join("")}</div>`
          : ""
      }
    </section>
  `;
}

export function renderSubAgentSummaryTrigger(state: DesktopWebState): string {
  const rows = orderedAgentActivityRows(state.agent_activity_rows ?? []);
  if (!state.agent_tree_active && rows.length === 0) return "";
  return `
    <section class="output-agent-section" aria-label="Sub Agent">
      ${rows.length > 0
        ? `<button type="button" class="output-agent-trigger" data-action="show-agent-pane" data-focus-key="output-agent-trigger"
             aria-controls="sub-agent-inspector" aria-expanded="false">
            <span class="output-agent-symbols" aria-hidden="true">${rows.slice(0, 4).map(renderSummarySymbol).join("")}</span>
            <span class="output-agent-trigger-label"><strong>Sub Agent</strong><small>${escapeHtml(agentActivitySummary(rows, state.agent_tree_active))}</small></span>
            <span aria-hidden="true">›</span>
          </button>`
        : renderAgentTreePending()}
    </section>
  `;
}

export function renderAgentInspector(state: DesktopWebState, selectedAgentPath: string | null): string {
  const rows = orderedAgentActivityRows(state.agent_activity_rows ?? []);
  if (rows.length === 0) return renderAgentTreePending();
  const groups = [
    { key: "active", label: "作業中", rows: rows.filter((row) => agentIsActive(row.status)) },
    { key: "attention", label: "要確認", rows: rows.filter((row) => ["interrupted", "errored", "not_found"].includes(row.status)) },
    { key: "completed", label: "完了", rows: rows.filter((row) => row.status === "completed") },
    { key: "stopped", label: "停止", rows: rows.filter((row) => row.status === "shutdown") },
  ].filter((group) => group.rows.length > 0);
  return `
    <div class="agent-inspector-summary">${escapeHtml(agentActivitySummary(rows, state.agent_tree_active))}</div>
    <div class="sub-agent-list agent-inspector-list">
      ${groups.map((group) => `
        <section class="sub-agent-group" aria-labelledby="sub-agent-group-${group.key}">
          <h3 id="sub-agent-group-${group.key}">${group.label}<small>${group.rows.length}</small></h3>
          ${group.rows.map((row) => renderAgentDetail(row, row.agent_path === selectedAgentPath)).join("")}
        </section>
      `).join("")}
    </div>
  `;
}

export function renderPermissionAgentIdentity(agentPath: string, taskName: string): string {
  const visual = stableAgentVisual(agentPath);
  const label = taskName.trim() || agentPath.split("/").filter(Boolean).pop() || "Sub Agent";
  return `
    <span class="permission-agent agent-tone-${visual.tone}">
      <span class="agent-symbol" aria-hidden="true">${visual.glyph}</span>
      <span><strong>${escapeHtml(label)}</strong><small>${escapeHtml(agentPath)}</small></span>
    </span>
  `;
}

function renderAgentChip(row: AgentActivityRow, inspectorOpen: boolean): string {
  const visual = stableAgentVisual(row.agent_path);
  const label = agentDisplayName(row);
  const status = agentStatusLabel(row.status);
  return `
    <button type="button" class="agent-chip agent-tone-${visual.tone} agent-status-${row.status} ${row.updated ? "updated" : ""}"
      data-action="show-agent-pane" data-agent-path="${escapeHtml(row.agent_path)}"
      data-focus-key="agent-chip:${escapeHtml(row.agent_path)}" title="${escapeHtml(`${row.agent_path} · ${status}`)}"
      aria-label="${escapeHtml(`${label}のSub Agent履歴を表示 · ${status}`)}"
      aria-controls="sub-agent-inspector" aria-expanded="${inspectorOpen ? "true" : "false"}">
      <span class="agent-symbol" aria-hidden="true">${visual.glyph}</span>
      <span class="agent-chip-label">${escapeHtml(label)}</span>
      <small>${escapeHtml(status)}</small>
    </button>
  `;
}

function renderInlineCurrentActivity(row: AgentActivityRow, inspectorOpen: boolean): string {
  const visual = stableAgentVisual(row.agent_path);
  return `
    <button type="button" class="agent-inline-current agent-tone-${visual.tone}"
      data-action="show-agent-pane" data-agent-path="${escapeHtml(row.agent_path)}"
      data-focus-key="agent-current:${escapeHtml(row.agent_path)}"
      aria-label="${escapeHtml(`${agentDisplayName(row)}のSub Agent履歴を表示`)}"
      aria-controls="sub-agent-inspector" aria-expanded="${inspectorOpen ? "true" : "false"}">
      <span class="agent-symbol" aria-hidden="true">${visual.glyph}</span>
      <span><strong>${escapeHtml(agentDisplayName(row))}</strong><small>${escapeHtml(activityPreview(row))}</small></span>
    </button>
  `;
}

function renderAgentDetail(row: AgentActivityRow, selected = false): string {
  const visual = stableAgentVisual(row.agent_path);
  const open = selected || agentIsActive(row.status) || row.updated ? "open" : "";
  const activity = row.current_activity.trim();
  const task = row.task_preview.trim();
  const result = row.result_preview.trim();
  return `
    <details class="sub-agent-detail agent-tone-${visual.tone} agent-status-${row.status} ${row.updated ? "updated" : ""} ${selected ? "selected" : ""}"
      data-details-key="sub-agent:${escapeHtml(row.agent_path)}" data-agent-path="${escapeHtml(row.agent_path)}" ${open}>
      <summary data-focus-key="sub-agent-summary:${escapeHtml(row.agent_path)}"${selected ? ' aria-current="true"' : ""}>
        <span class="agent-symbol" aria-hidden="true">${visual.glyph}</span>
        <span class="sub-agent-title">
          <strong>${escapeHtml(agentDisplayName(row))}</strong>
          <small>${escapeHtml(row.agent_path)}</small>
        </span>
        <span class="agent-status-label">${escapeHtml(agentStatusLabel(row.status))}</span>
      </summary>
      <div class="sub-agent-detail-body">
        ${task ? renderDetailLine("依頼", task) : ""}
        ${activity ? renderDetailLine("現在", activity) : ""}
        ${result ? renderDetailLine("結果", result) : ""}
        <small class="sub-agent-session">Session ${escapeHtml(row.session_id)}</small>
      </div>
    </details>
  `;
}

function renderSummarySymbol(row: AgentActivityRow): string {
  const visual = stableAgentVisual(row.agent_path);
  return `<span class="agent-symbol agent-tone-${visual.tone}">${visual.glyph}</span>`;
}

function renderDetailLine(label: string, value: string): string {
  return `<div class="sub-agent-detail-line"><span>${escapeHtml(label)}</span><p>${escapeHtml(value)}</p></div>`;
}

function renderAgentTreePending(): string {
  return '<div class="sub-agent-pending"><span class="busy-spinner small" title="準備中"></span><span>Sub Agentを準備しています</span></div>';
}

function activityPreview(row: AgentActivityRow): string {
  const current = row.current_activity.trim();
  if (current.length > 0) return current;
  if (!agentIsActive(row.status)) return row.result_preview.trim();
  return "";
}

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

export function renderInlineAgentActivity(state: DesktopWebState): string {
  const rows = orderedAgentActivityRows(state.agent_activity_rows ?? []);
  if (!state.agent_tree_active && rows.length === 0) return "";
  const counts = agentActivityCounts(rows);
  const updateText = counts.updated > 0 ? `${counts.updated}件のSub Agentが更新しました` : "";
  const currentRows = rows.filter((row) => activityPreview(row).length > 0);
  return `
    <section class="agent-inline-activity" aria-label="Sub Agentの活動">
      <div class="agent-inline-heading">
        <strong>${escapeHtml(agentActivitySummary(rows, state.agent_tree_active))}</strong>
        ${updateText ? `<span class="agent-update-summary" role="status" aria-live="polite" aria-atomic="true">${escapeHtml(updateText)}</span>` : ""}
      </div>
      ${rows.length > 0 ? `<div class="agent-chip-list">${rows.map(renderAgentChip).join("")}</div>` : renderAgentTreePending()}
      ${
        currentRows.length > 0
          ? `<div class="agent-current-list">${currentRows.map(renderInlineCurrentActivity).join("")}</div>`
          : ""
      }
    </section>
  `;
}

export function renderSubAgentSection(state: DesktopWebState): string {
  const rows = orderedAgentActivityRows(state.agent_activity_rows ?? []);
  if (!state.agent_tree_active && rows.length === 0) return "";
  return `
    <section class="output-agent-section" aria-label="Sub Agent">
      <div class="output-section-heading">
        <strong>Sub Agent</strong>
        <small>${escapeHtml(agentActivitySummary(rows, state.agent_tree_active))}</small>
      </div>
      <div class="sub-agent-list">
        ${rows.length > 0 ? rows.map(renderAgentDetail).join("") : renderAgentTreePending()}
      </div>
    </section>
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

function renderAgentChip(row: AgentActivityRow): string {
  const visual = stableAgentVisual(row.agent_path);
  const label = agentDisplayName(row);
  const status = agentStatusLabel(row.status);
  return `
    <span class="agent-chip agent-tone-${visual.tone} agent-status-${row.status} ${row.updated ? "updated" : ""}"
      title="${escapeHtml(`${row.agent_path} · ${status}`)}">
      <span class="agent-symbol" aria-hidden="true">${visual.glyph}</span>
      <span class="agent-chip-label">${escapeHtml(label)}</span>
      <small>${escapeHtml(status)}</small>
    </span>
  `;
}

function renderInlineCurrentActivity(row: AgentActivityRow): string {
  const visual = stableAgentVisual(row.agent_path);
  return `
    <div class="agent-inline-current agent-tone-${visual.tone}">
      <span class="agent-symbol" aria-hidden="true">${visual.glyph}</span>
      <span><strong>${escapeHtml(agentDisplayName(row))}</strong><small>${escapeHtml(activityPreview(row))}</small></span>
    </div>
  `;
}

function renderAgentDetail(row: AgentActivityRow): string {
  const visual = stableAgentVisual(row.agent_path);
  const open = agentIsActive(row.status) || row.updated ? "open" : "";
  const activity = row.current_activity.trim();
  const task = row.task_preview.trim();
  const result = row.result_preview.trim();
  return `
    <details class="sub-agent-detail agent-tone-${visual.tone} agent-status-${row.status} ${row.updated ? "updated" : ""}"
      data-details-key="sub-agent:${escapeHtml(row.agent_path)}" ${open}>
      <summary data-focus-key="sub-agent-summary:${escapeHtml(row.agent_path)}">
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

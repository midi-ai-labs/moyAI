import {
  agentActivityCounts,
  agentActivitySummary,
  agentDisplayName,
  agentIsActive,
  agentStatusLabel,
  orderedAgentActivityRows,
  plainAgentPreview,
  stableAgentVisual,
} from "./agent_activity.ts";
import type { AgentActivityRow, DesktopWebState } from "./types.ts";
import type { AgentExecutionCacheEntry } from "./ui_state.ts";
import { renderTranscriptRows } from "./render_transcript.ts";
import { escapeHtml } from "./utils.ts";

export function renderInlineAgentActivity(
  state: DesktopWebState,
  selectedAgentPath: string | null = null,
): string {
  const rows = orderedAgentActivityRows(state.agent_activity_rows ?? []);
  if (!state.agent_tree_active && rows.length === 0) return "";
  const counts = agentActivityCounts(rows);
  const updateText = counts.updated > 0 ? `${counts.updated}件のSub Agentが更新しました` : "";
  const groupPhase = state.agent_tree_active ? "active" : "terminal";
  return `
    <section class="agent-inline-activity" aria-label="Sub Agentの活動">
      <details class="agent-job-group" data-details-key="sub-agent-inline-group:${groupPhase}" ${state.agent_tree_active ? "open" : ""}>
        <summary>
          <span class="agent-inline-heading"><strong>Sub Agent</strong><small>${escapeHtml(agentActivitySummary(rows, state.agent_tree_active))}</small></span>
          ${updateText ? `<span class="agent-update-summary" role="status" aria-live="polite" aria-atomic="true">${escapeHtml(updateText)}</span>` : ""}
        </summary>
        ${rows.length > 0
          ? `<div class="agent-job-list">${rows.map((row) => renderAgentJobCard(row, selectedAgentPath)).join("")}</div>`
          : renderAgentTreePending()}
      </details>
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

export function renderAgentInspector(
  state: DesktopWebState,
  selectedAgentPath: string | null,
  execution: AgentExecutionCacheEntry | null = null,
): string {
  const rows = orderedAgentActivityRows(state.agent_activity_rows ?? []);
  if (rows.length === 0) return renderAgentTreePending();
  const selected = rows.find((row) => row.agent_path === selectedAgentPath);
  if (selected) return renderAgentExecution(selected, execution);
  const groups = [
    { key: "active", label: "作業中", rows: rows.filter((row) => agentIsActive(row.status)) },
    { key: "attention", label: "要確認", rows: rows.filter((row) => ["interrupted", "errored"].includes(row.status)) },
    { key: "completed", label: "完了", rows: rows.filter((row) => row.status === "completed") },
    { key: "stopped", label: "停止", rows: rows.filter((row) => row.status === "shutdown") },
  ].filter((group) => group.rows.length > 0);
  return `
    <div class="agent-inspector-summary">${escapeHtml(agentActivitySummary(rows, state.agent_tree_active))}</div>
    <div class="sub-agent-list agent-inspector-list">
      ${groups.map((group) => `
        <section class="sub-agent-group" aria-labelledby="sub-agent-group-${group.key}">
          <h3 id="sub-agent-group-${group.key}">${group.label}<small>${group.rows.length}</small></h3>
          ${group.rows.map((row) => renderInspectorListCard(row)).join("")}
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

function renderAgentJobCard(row: AgentActivityRow, selectedAgentPath: string | null): string {
  const visual = stableAgentVisual(row.agent_path);
  const label = agentDisplayName(row);
  const status = agentStatusLabel(row.status);
  const preview = plainAgentPreview(activityPreview(row) || row.task_preview.trim());
  const selected = selectedAgentPath === row.agent_path;
  return `
    <button type="button" class="agent-job-card agent-tone-${visual.tone} agent-status-${row.status} ${row.updated ? "updated" : ""}"
      data-action="show-agent-pane" data-agent-path="${escapeHtml(row.agent_path)}"
      data-focus-key="agent-job:${escapeHtml(row.agent_path)}" title="${escapeHtml(`${row.agent_path} · ${status}`)}"
      aria-label="${escapeHtml(`${label}のSub Agent履歴を表示 · ${status}`)}"
      aria-controls="sub-agent-inspector" aria-expanded="${selected ? "true" : "false"}">
      <span class="agent-symbol" aria-hidden="true">${visual.glyph}</span>
      <span class="agent-job-copy"><strong>${escapeHtml(label)}</strong><small>${escapeHtml(preview)}</small></span>
      <span class="agent-status-label">${escapeHtml(status)}</span>
      <span class="agent-job-chevron" aria-hidden="true">›</span>
    </button>
  `;
}

function renderInspectorListCard(row: AgentActivityRow): string {
  const visual = stableAgentVisual(row.agent_path);
  const preview = plainAgentPreview(activityPreview(row) || row.task_preview.trim());
  return `
    <button type="button" class="sub-agent-list-card agent-tone-${visual.tone} agent-status-${row.status} ${row.updated ? "updated" : ""}"
      data-action="show-agent-pane" data-agent-path="${escapeHtml(row.agent_path)}"
      data-focus-key="sub-agent-card:${escapeHtml(row.agent_path)}"
      aria-label="${escapeHtml(`${agentDisplayName(row)}のSub Agent履歴を表示`)}"
      aria-controls="sub-agent-inspector">
      <span class="agent-symbol" aria-hidden="true">${visual.glyph}</span>
      <span class="agent-job-copy"><strong>${escapeHtml(agentDisplayName(row))}</strong><small>${escapeHtml(preview)}</small></span>
      <span class="agent-status-label">${escapeHtml(agentStatusLabel(row.status))}</span>
      <span class="agent-job-chevron" aria-hidden="true">›</span>
    </button>
  `;
}

function renderAgentExecution(row: AgentActivityRow, execution: AgentExecutionCacheEntry | null): string {
  const projection = execution?.projection;
  const transcript = projection
    ? renderTranscriptRows(projection.transcript_rows, { anchorPrefix: "agent-execution" })
    : "";
  const count = projection
    ? projection.turn_page_offset > 0
      ? `${projection.transcript_rows.length}件を表示 · 以前の実行履歴あり`
      : `${projection.transcript_rows.length}件の履歴`
    : "";
  const previousHistoryAction = projection?.turn_page_has_previous
    ? `<button type="button" class="agent-execution-previous" data-action="load-previous-agent-execution-page"
         data-agent-path="${escapeHtml(row.agent_path)}" ${execution?.status === "loading" ? "disabled" : ""}>
         ${execution?.status === "loading" ? '<span class="busy-spinner small"></span><span>読み込み中</span>' : "以前の実行履歴"}
       </button>`
    : "";
  return `
    <section class="agent-execution" data-agent-path="${escapeHtml(row.agent_path)}"
      data-focus-key="agent-execution:${escapeHtml(row.agent_path)}" tabindex="-1">
      <div class="agent-execution-meta">
        <span>${escapeHtml(agentStatusLabel(row.status))}</span>
        <small>${escapeHtml(count || `Session ${row.session_id}`)}</small>
      </div>
      ${execution?.status === "loading" && !projection ? '<div class="agent-execution-state"><span class="busy-spinner small"></span><span>実行履歴を読み込んでいます</span></div>' : ""}
      ${execution?.status === "error" ? `<div class="agent-execution-error" role="status"><span>${escapeHtml(execution.error)}</span><button type="button" data-action="show-agent-pane" data-agent-path="${escapeHtml(row.agent_path)}">再試行</button></div>` : ""}
      ${previousHistoryAction ? `<div class="agent-execution-history-controls">${previousHistoryAction}</div>` : ""}
      ${transcript
        ? `<div class="agent-execution-scroll">${transcript}</div>`
        : execution?.status === "loading"
          ? ""
          : '<div class="empty agent-execution-empty">表示できる実行履歴はありません</div>'}
    </section>
  `;
}

function renderSummarySymbol(row: AgentActivityRow): string {
  const visual = stableAgentVisual(row.agent_path);
  return `<span class="agent-symbol agent-tone-${visual.tone}">${visual.glyph}</span>`;
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

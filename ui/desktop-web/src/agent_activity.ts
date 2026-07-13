import type { AgentActivityRow, AgentStatus } from "./types.ts";

export interface AgentActivityCounts {
  total: number;
  active: number;
  completed: number;
  attention: number;
  stopped: number;
  updated: number;
}

export interface AgentVisual {
  tone: string;
  glyph: string;
}

const AGENT_VISUALS: AgentVisual[] = [
  { tone: "rose", glyph: "✿" },
  { tone: "violet", glyph: "◆" },
  { tone: "amber", glyph: "✦" },
  { tone: "blue", glyph: "●" },
  { tone: "green", glyph: "⬢" },
  { tone: "cyan", glyph: "✧" },
];

const ACTIVE_STATUSES = new Set<AgentStatus>(["pending_init", "running"]);
const ATTENTION_STATUSES = new Set<AgentStatus>(["interrupted", "errored", "not_found"]);

export function orderedAgentActivityRows(rows: readonly AgentActivityRow[]): AgentActivityRow[] {
  return [...rows].sort((left, right) => {
    if (left.started_order !== right.started_order) return left.started_order - right.started_order;
    if (left.agent_path < right.agent_path) return -1;
    if (left.agent_path > right.agent_path) return 1;
    return 0;
  });
}

export function agentActivityCounts(rows: readonly AgentActivityRow[]): AgentActivityCounts {
  const counts: AgentActivityCounts = {
    total: rows.length,
    active: 0,
    completed: 0,
    attention: 0,
    stopped: 0,
    updated: 0,
  };
  for (const row of rows) {
    if (ACTIVE_STATUSES.has(row.status)) counts.active += 1;
    if (row.status === "completed") counts.completed += 1;
    if (ATTENTION_STATUSES.has(row.status)) counts.attention += 1;
    if (row.status === "shutdown") counts.stopped += 1;
    if (row.updated) counts.updated += 1;
  }
  return counts;
}

export function agentActivitySummary(rows: readonly AgentActivityRow[], treeActive: boolean): string {
  const counts = agentActivityCounts(rows);
  const parts: string[] = [];
  if (counts.active > 0) parts.push(`${counts.active}件作業中`);
  if (counts.completed > 0) parts.push(`${counts.completed}件完了`);
  if (counts.attention > 0) parts.push(`${counts.attention}件要確認`);
  if (counts.stopped > 0) parts.push(`${counts.stopped}件停止`);
  if (parts.length === 0 && treeActive) return "Sub Agentを準備中";
  return parts.join(" · ");
}

export function agentStatusLabel(status: AgentStatus): string {
  switch (status) {
    case "pending_init":
      return "準備中";
    case "running":
      return "作業中";
    case "interrupted":
      return "中断";
    case "completed":
      return "完了";
    case "errored":
      return "エラー";
    case "shutdown":
      return "停止";
    case "not_found":
      return "不明";
  }
}

export function agentIsActive(status: AgentStatus): boolean {
  return ACTIVE_STATUSES.has(status);
}

export function agentDisplayName(row: Pick<AgentActivityRow, "agent_path" | "task_name">): string {
  const taskName = row.task_name.trim();
  if (taskName.length > 0) return taskName;
  const pathName = row.agent_path.split("/").filter(Boolean).pop()?.trim();
  return pathName || "Sub Agent";
}

export function stableAgentVisual(agentPath: string): AgentVisual {
  let hash = 2_166_136_261;
  for (let index = 0; index < agentPath.length; index += 1) {
    hash ^= agentPath.charCodeAt(index);
    hash = Math.imul(hash, 16_777_619) >>> 0;
  }
  return AGENT_VISUALS[hash % AGENT_VISUALS.length];
}

export function agentActivityRowsChanged(
  previous: readonly AgentActivityRow[],
  current: readonly AgentActivityRow[],
): boolean {
  const previousOrdered = orderedAgentActivityRows(previous);
  const currentOrdered = orderedAgentActivityRows(current);
  if (previousOrdered.length !== currentOrdered.length) return true;
  return previousOrdered.some((row, index) => agentActivityRowKey(row) !== agentActivityRowKey(currentOrdered[index]));
}

function agentActivityRowKey(row: AgentActivityRow): string {
  return [
    row.agent_path,
    row.session_id,
    row.task_name,
    row.task_preview,
    row.status,
    row.current_activity,
    row.result_preview,
    String(row.started_order),
    String(row.updated),
  ].join("\u0000");
}

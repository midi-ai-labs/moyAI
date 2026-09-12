import { renderTaskActivityIndicator } from "./task_activity_indicator.ts";
import type { TaskActivityState } from "./types.ts";

/** Receiver-wide Rust projection; independent of the chat selected in this window. */
export interface McpActivityProjection {
  running: number;
  waiting: number;
  awaiting_approval: number;
  cancelling: number;
  unavailable: boolean;
}

export function mcpActivityPresentation(activity: McpActivityProjection | null | undefined): {
  state: TaskActivityState; label: string; detail: string; total: number;
} | null {
  if (!activity) return null;
  if (activity.unavailable) return { state: "attention", label: "MCP状態を確認できません", detail: "MCP履歴で作業状況を確認してください", total: 0 };
  const total = activity.running + activity.waiting + activity.awaiting_approval + activity.cancelling;
  if (total === 0) return null;
  const counts = [
    ["実行中", activity.running], ["待機", activity.waiting],
    ["承認待ち", activity.awaiting_approval], ["停止処理中", activity.cancelling],
  ] as const;
  return {
    state: activity.awaiting_approval > 0 ? "attention" : "running",
    label: activity.awaiting_approval > 0 ? "MCP承認待ち"
      : activity.cancelling > 0 ? "MCP停止処理中"
      : activity.running > 0 ? "MCP実行中" : "MCP待機中",
    detail: counts.filter(([, count]) => count > 0).map(([label, count]) => `${label} ${count}件`).join(" · "),
    total,
  };
}

export function renderMcpActivityStrip(activity: McpActivityProjection | null | undefined): string {
  const presentation = mcpActivityPresentation(activity);
  if (!presentation) return "";
  return `<section class="run-strip mcp-run-strip" aria-label="この端末のMCP受付状況">
    <span class="task-activity-badge mcp-activity-badge" data-mcp-activity="${presentation.state}" role="status" aria-live="polite" aria-atomic="true">
      ${renderTaskActivityIndicator(presentation.state, { decorative: true })}<strong>${presentation.label}</strong>
    </span><span>${presentation.detail}</span>
    <button data-action="show-mcp-execution-history" data-focus-key="mcp-activity-history" title="MCP実行の詳細・承認・停止を確認">MCP履歴を開く</button>
  </section>`;
}

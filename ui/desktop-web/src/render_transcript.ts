import {
  agentDisplayName,
  agentStatusLabel,
  plainAgentPreview,
  stableAgentVisual,
} from "./agent_activity.ts";
import { transcriptAnchors, type TranscriptAnchor } from "./history_navigation.ts";
import { renderMarkdown } from "./markdown.ts";
import type { AgentActivityRow, FileChangeRow, TranscriptRow } from "./types.ts";
import { escapeHtml } from "./utils.ts";

interface TranscriptRenderOptions {
  anchorPrefix?: string;
  includeRail?: boolean;
  agentActivityRows?: readonly AgentActivityRow[];
  currentTurnAgentActivityRows?: readonly AgentActivityRow[];
  includeLiveAgentFallback?: boolean;
  selectedAgentPath?: string | null;
  stableLatestAssistant?: boolean;
}

interface AgentHistoryEvent {
  agentPath: string;
  rowKind: "sub_agent_started" | "sub_agent_updated" | "sub_agent_interrupted";
}

interface WorkHistoryItem {
  label: string;
  detail: string;
  status: string;
}

const MAX_HISTORY_RAIL_MARKERS = 96;
const HISTORY_RAIL_RECENT_MARKERS = 4;

export function renderTranscriptRows(
  rows: readonly TranscriptRow[],
  options: TranscriptRenderOptions = {},
): string {
  const anchors = transcriptAnchors(rows, {
    stableLatestAssistant: options.stableLatestAssistant,
  });
  const prefix = options.anchorPrefix?.trim() ?? "";
  let lastWorkSummaryIndex = -1;
  anchors.forEach((anchor, index) => {
    if (anchor.row.row_kind.startsWith("work_summary")) lastWorkSummaryIndex = index;
  });
  const lastAgentSummaryIndexes = lastAgentSummaryIndexByPath(anchors);
  const pendingAgentEvents: AgentHistoryEvent[] = [];
  const rendered: string[] = [];

  anchors.forEach((anchor, index) => {
    const agentEvent = agentHistoryEvent(anchor);
    if (agentEvent) {
      pendingAgentEvents.push(agentEvent);
      return;
    }
    if (anchor.row.row_kind === "user" && pendingAgentEvents.length > 0) {
      rendered.push(renderOrphanAgentEvents(pendingAgentEvents.splice(0), options));
    }
    if (anchor.row.row_kind.startsWith("work_summary")) {
      const events = pendingAgentEvents.splice(0);
      const activityRows = activityRowsForSummary(
        index,
        lastWorkSummaryIndex,
        lastAgentSummaryIndexes,
        options,
      );
      if (index === lastWorkSummaryIndex && options.includeLiveAgentFallback) {
        appendLiveAgentFallback(events, options.currentTurnAgentActivityRows ?? []);
      }
      rendered.push(renderWorkSummary(
        anchor.row,
        prefixedAnchorId(prefix, anchor.id),
        prefixedAnchorId(prefix, anchor.detailsId),
        events,
        activityRows,
        options,
      ));
      return;
    }
    rendered.push(renderTranscriptRow(
      anchor.row,
      prefixedAnchorId(prefix, anchor.id),
    ));
  });
  if (pendingAgentEvents.length > 0) {
    rendered.push(renderOrphanAgentEvents(pendingAgentEvents, options));
  }
  const railAnchors = anchors.filter((anchor) => !isAgentHistoryRow(anchor.row));
  return `${options.includeRail ? renderHistoryRail(railAnchors) : ""}${rendered.join("")}`;
}

export function renderEarlierHistoryTrigger(offset: number, loading: boolean): string {
  if (!(offset > 0)) return "";
  return `
    <div class="history-load-earlier">
      <button type="button" data-action="load-previous-turn-page" data-focus-key="load-previous-turn-page"
        ${loading ? 'disabled aria-disabled="true"' : ""}>
        <span aria-hidden="true">↑</span>
        <span><strong>以前の履歴</strong><small>現在位置を保ったまま読み込みます</small></span>
      </button>
    </div>
  `;
}

function renderHistoryRail(anchors: readonly TranscriptAnchor[]): string {
  const markers = railAnchors(anchors);
  if (markers.length < 2) return "";
  return `
    <nav class="history-rail" aria-label="会話履歴">
      <div class="history-rail-markers" style="--history-marker-count: ${markers.length}">
        ${markers.map((anchor) => `
          <button type="button" class="history-rail-marker history-kind-${escapeHtml(anchor.row.row_kind)}"
            data-action="jump-history-anchor" data-history-target="${escapeHtml(anchor.id)}"
            data-focus-key="history-rail:${escapeHtml(anchor.id)}"
            aria-label="${escapeHtml(`${anchor.label}: ${anchor.preview}`)}">
            <span class="history-marker-tick" aria-hidden="true"></span>
            <span class="history-marker-preview" role="tooltip"><strong>${escapeHtml(anchor.label)}</strong>${escapeHtml(anchor.preview)}</span>
          </button>
        `).join("")}
      </div>
    </nav>
  `;
}

function railAnchors(anchors: readonly TranscriptAnchor[]): TranscriptAnchor[] {
  const conversational = anchors.filter((anchor) => (
    anchor.row.row_kind === "user"
    || anchor.row.row_kind === "assistant"
    || anchor.row.row_kind.startsWith("work_summary")
  ));
  const candidates = conversational.length >= 2 ? conversational : [...anchors];
  if (candidates.length <= MAX_HISTORY_RAIL_MARKERS) return [...candidates];

  // Keep the newest markers stable for live updates and distribute the remaining
  // slots over the older history. The bounded rail always fits in the viewport,
  // while its first and last anchors still span the complete loaded conversation.
  const recentStart = candidates.length - HISTORY_RAIL_RECENT_MARKERS;
  const olderSlotCount = MAX_HISTORY_RAIL_MARKERS - HISTORY_RAIL_RECENT_MARKERS;
  const olderEnd = recentStart - 1;
  const sampled: TranscriptAnchor[] = [];
  for (let slot = 0; slot < olderSlotCount; slot += 1) {
    const index = Math.floor((slot * olderEnd) / Math.max(1, olderSlotCount - 1));
    const candidate = candidates[index];
    if (candidate && sampled.at(-1)?.id !== candidate.id) sampled.push(candidate);
  }
  sampled.push(...candidates.slice(recentStart));
  return sampled;
}

function renderTranscriptRow(row: TranscriptRow, anchorId: string): string {
  if (row.row_kind === "file_changes") {
    return renderFileChanges(row, anchorId);
  }
  const routineConversation = row.row_kind === "user" || row.row_kind === "assistant";
  return `
    <article class="message ${escapeHtml(row.row_kind)}" data-history-anchor="${escapeHtml(anchorId)}">
      <div class="message-body">
        ${routineConversation ? "" : `<h2>${escapeHtml(row.title)}</h2>`}
        <div class="markdown-body">${renderMarkdown(row.body)}</div>
      </div>
    </article>
  `;
}

function renderFileChanges(row: TranscriptRow, anchorId: string): string {
  const body = row.file_changes.length === 0
    ? `<div class="markdown-body">${renderMarkdown(row.body)}</div>`
    : renderFileChangeTable(row.file_changes);
  return `
    <article class="message file_changes" data-history-anchor="${escapeHtml(anchorId)}">
      <div class="message-body">
        <h2>${escapeHtml(row.title)}</h2>
        ${body}
      </div>
    </article>
  `;
}

function renderFileChangeTable(changes: readonly FileChangeRow[]): string {
  const rows = changes.map((change) => `
    <div class="transcript-change-row">
      <span class="transcript-change-action">${escapeHtml(change.action)}</span>
      <strong title="${escapeHtml(change.path)}">${escapeHtml(change.path)}</strong>
      <small>${escapeHtml(change.summary || change.label || change.path)}</small>
    </div>
  `).join("");
  return `
    <div class="transcript-change-table" role="table" aria-label="ファイル変更結果">
      <div class="transcript-change-row transcript-change-head" role="row">
        <span>操作</span><span>ファイル</span><span>内容</span>
      </div>
      ${rows}
    </div>
  `;
}

function renderWorkSummary(
  row: TranscriptRow,
  anchorId: string,
  detailsId: string,
  agentEvents: readonly AgentHistoryEvent[],
  agentActivityRows: readonly AgentActivityRow[],
  options: TranscriptRenderOptions,
): string {
  const running = row.row_kind === "work_summary_running";
  const incomplete = row.row_kind === "work_summary_incomplete";
  const open = running || incomplete ? "open" : "";
  const statusText = running
    ? "実行中"
    : incomplete
      ? "状態未確定"
      : row.row_kind === "work_summary_failed"
        ? "失敗"
        : row.row_kind === "work_summary_cancelled"
          ? "停止"
          : "";
  const body = renderStructuredWorkHistory(
    row.body,
    agentEvents,
    agentActivityRows,
    detailsId,
    options,
  );
  return `
    <article class="message work-summary ${escapeHtml(row.row_kind)}" data-history-anchor="${escapeHtml(anchorId)}">
      <div class="message-body">
        <details data-details-key="work-summary:${escapeHtml(detailsId)}" ${open}>
          <summary data-focus-key="work-summary:${escapeHtml(anchorId)}"><span>${escapeHtml(row.title)}</span>${statusText ? `<small>${escapeHtml(statusText)}</small>` : ""}</summary>
          <div class="work-summary-body">${body}</div>
        </details>
      </div>
    </article>
  `;
}

function renderStructuredWorkHistory(
  body: string,
  agentEvents: readonly AgentHistoryEvent[],
  agentActivityRows: readonly AgentActivityRow[],
  ownerId: string,
  options: TranscriptRenderOptions,
): string {
  const sections = parseWorkSummarySections(body);
  const agents = renderAgentHistoryEvents(agentEvents, agentActivityRows, ownerId, options);
  const historyItems = presentedWorkHistoryItems(sections.history, agentEvents.length > 0);
  const history = historyItems.length > 0
    ? `<div class="work-history-events">${historyItems.map(renderWorkHistoryItem).join("")}</div>`
    : "";
  const summary = sections.summary.length > 0
    ? `<div class="work-history-meta">${sections.summary.map((item) => `
        <span><strong>${escapeHtml(item.label)}</strong>${item.detail ? `<small>${escapeHtml(item.detail)}</small>` : ""}</span>
      `).join("")}</div>`
    : "";
  if (agents || history || summary) return `${agents}${history}${summary}`;
  return `<div class="markdown-body">${renderMarkdown(body)}</div>`;
}

function renderAgentHistoryEvents(
  events: readonly AgentHistoryEvent[],
  activityRows: readonly AgentActivityRow[],
  ownerId: string,
  options: TranscriptRenderOptions,
): string {
  if (events.length === 0) return "";
  const activityByPath = new Map(activityRows.map((row) => [row.agent_path, row]));
  const firstEventIndexes = new Map<string, number>();
  events.forEach((event, index) => {
    if (!firstEventIndexes.has(event.agentPath)) firstEventIndexes.set(event.agentPath, index);
  });
  const presentedEvents = coalesceAgentEvents(events).sort((left, right) => {
    const leftActivity = activityByPath.get(left.agentPath);
    const rightActivity = activityByPath.get(right.agentPath);
    if (
      leftActivity
      && rightActivity
      && leftActivity.started_order !== rightActivity.started_order
    ) {
      return leftActivity.started_order - rightActivity.started_order;
    }
    return (firstEventIndexes.get(left.agentPath) ?? 0)
      - (firstEventIndexes.get(right.agentPath) ?? 0);
  });
  return `
    <div class="work-summary-agent-events" aria-label="Sub Agentの作業履歴">
      ${presentedEvents.map((event) => {
        const activity = activityByPath.get(event.agentPath);
        const visual = stableAgentVisual(event.agentPath);
        const label = humanizeAgentName(activity
          ? agentDisplayName(activity)
          : agentNameFromPath(event.agentPath));
        const description = plainAgentPreview(activity?.task_preview ?? "");
        const status = agentEventStatus(event, activity, true);
        const statusKey = activity?.status
          ?? (event.rowKind === "sub_agent_interrupted"
            ? "interrupted"
            : event.rowKind === "sub_agent_started"
              ? "running"
              : "updated");
        const selected = options.selectedAgentPath === event.agentPath;
        const ariaLabel = [
          `${label}のSub Agent履歴を表示`,
          description,
          status,
        ].filter(Boolean).join(" · ");
        return `
          <button type="button"
            class="agent-job-card work-summary-agent-card agent-tone-${visual.tone} agent-status-${escapeHtml(statusKey)}"
            data-action="show-agent-pane" data-agent-path="${escapeHtml(event.agentPath)}"
            data-focus-key="agent-history:${escapeHtml(ownerId)}:${escapeHtml(event.agentPath)}"
            title="${escapeHtml(`${event.agentPath} · ${status}`)}"
            aria-label="${escapeHtml(ariaLabel)}"
            aria-controls="sub-agent-inspector" aria-expanded="${selected ? "true" : "false"}">
            <span class="agent-symbol" aria-hidden="true">${visual.glyph}</span>
            <span class="agent-job-copy">
              <strong>${escapeHtml(label)}</strong>
              ${description ? `<small>${escapeHtml(description)}</small>` : ""}
            </span>
            <span class="agent-status-label">${escapeHtml(status)}</span>
            <span class="agent-job-chevron" aria-hidden="true">›</span>
          </button>
        `;
      }).join("")}
    </div>
  `;
}

function coalesceAgentEvents(
  events: readonly AgentHistoryEvent[],
): AgentHistoryEvent[] {
  const coalesced = new Map<string, AgentHistoryEvent>();
  for (const event of events) {
    coalesced.set(event.agentPath, event);
  }
  return [...coalesced.values()];
}

function renderOrphanAgentEvents(
  events: readonly AgentHistoryEvent[],
  options: TranscriptRenderOptions,
): string {
  if (events.length === 0) return "";
  return `
    <article class="message agent-history-orphan">
      <div class="message-body">${renderAgentHistoryEvents(
        events,
        options.agentActivityRows ?? [],
        "orphan",
        options,
      )}</div>
    </article>
  `;
}

function renderWorkHistoryItem(item: WorkHistoryItem): string {
  return `
    <div class="work-history-event">
      <span class="work-history-event-icon" aria-hidden="true">${workHistoryGlyph(item.status)}</span>
      <span class="work-history-event-copy">
        <strong>${escapeHtml(item.label)}</strong>
        ${item.detail ? `<small>${escapeHtml(item.detail)}</small>` : ""}
      </span>
      ${item.status ? `<span class="work-history-event-status">${escapeHtml(item.status)}</span>` : ""}
    </div>
  `;
}

function parseWorkSummarySections(body: string): { summary: WorkHistoryItem[]; history: WorkHistoryItem[] } {
  const sections: Record<"summary" | "history", string[]> = { summary: [], history: [] };
  let section: "summary" | "history" = "summary";
  for (const rawLine of body.replace(/\r\n/g, "\n").split("\n")) {
    const heading = rawLine.trim().match(/^###\s+(.+)$/);
    if (heading) {
      section = heading[1].includes("履歴") ? "history" : "summary";
      continue;
    }
    sections[section].push(rawLine);
  }
  return {
    summary: parseWorkHistoryLines(sections.summary),
    history: parseWorkHistoryLines(sections.history),
  };
}

function parseWorkHistoryLines(lines: readonly string[]): WorkHistoryItem[] {
  const items: WorkHistoryItem[] = [];
  for (const rawLine of lines) {
    const trimmed = rawLine.trim();
    if (!trimmed) continue;
    const bullet = trimmed.match(/^[-*]\s+(.+)$/);
    if (bullet) {
      items.push(workHistoryItem(bullet[1]));
      continue;
    }
    const plain = plainMarkdownText(trimmed);
    if (!plain) continue;
    const latest = items.at(-1);
    if (latest) {
      latest.detail = [latest.detail, plain].filter(Boolean).join(" · ");
    } else {
      items.push({ label: plain, detail: "", status: "" });
    }
  }
  return items;
}

function workHistoryItem(value: string): WorkHistoryItem {
  const plain = plainMarkdownText(value);
  const statusMatch = plain.match(/^\[([^\]]+)\]\s*(.*)$/);
  const withoutStatus = statusMatch?.[2]?.trim() || plain;
  const separator = withoutStatus.indexOf(":");
  const label = separator > 0 && separator < 18
    ? withoutStatus.slice(0, separator).trim()
    : withoutStatus;
  const detail = separator > 0 && separator < 18
    ? withoutStatus.slice(separator + 1).trim()
    : "";
  return { label, detail, status: statusMatch?.[1]?.trim() ?? "" };
}

function presentedWorkHistoryItems(
  items: readonly WorkHistoryItem[],
  hasAgentEvents: boolean,
): WorkHistoryItem[] {
  const presented: WorkHistoryItem[] = [];
  for (const item of items) {
    if (item.status === "待機") continue;
    if (hasAgentEvents && /^(?:spawn_agent|Agent spawned)$/i.test(item.label)) continue;
    const waitCompleted = /^(?:wait_agent|Agent wait completed)$/i.test(item.label);
    const detail = normalizedWorkHistoryDetail(item.detail);
    const candidate = waitCompleted
      ? { label: "Sub Agentの完了を待ちました", detail: "", status: item.status || "完了" }
      : { ...item, detail };
    const previous = presented.at(-1);
    if (
      previous
      && previous.label === candidate.label
      && previous.detail === candidate.detail
      && previous.status === candidate.status
    ) {
      continue;
    }
    presented.push(candidate);
  }
  return presented;
}

function normalizedWorkHistoryDetail(value: string): string {
  const detail = value.replace(/^出力:\s*/, "").trim();
  if (/^[{[]/.test(detail)) return "";
  return detail;
}

function agentHistoryEvent(anchor: TranscriptAnchor): AgentHistoryEvent | null {
  if (!isAgentHistoryRow(anchor.row)) return null;
  return {
    agentPath: anchor.row.title.trim(),
    rowKind: anchor.row.row_kind,
  };
}

function isAgentHistoryRow(row: TranscriptRow): row is TranscriptRow & {
  row_kind: AgentHistoryEvent["rowKind"];
} {
  return row.row_kind === "sub_agent_started"
    || row.row_kind === "sub_agent_updated"
    || row.row_kind === "sub_agent_interrupted";
}

function lastAgentSummaryIndexByPath(
  anchors: readonly TranscriptAnchor[],
): ReadonlyMap<string, number> {
  const result = new Map<string, number>();
  const pendingPaths = new Set<string>();
  anchors.forEach((anchor, index) => {
    const event = agentHistoryEvent(anchor);
    if (event) {
      pendingPaths.add(event.agentPath);
      return;
    }
    if (anchor.row.row_kind === "user") pendingPaths.clear();
    if (!anchor.row.row_kind.startsWith("work_summary")) return;
    pendingPaths.forEach((path) => result.set(path, index));
    pendingPaths.clear();
  });
  return result;
}

function activityRowsForSummary(
  summaryIndex: number,
  lastWorkSummaryIndex: number,
  lastAgentSummaryIndexes: ReadonlyMap<string, number>,
  options: TranscriptRenderOptions,
): AgentActivityRow[] {
  const currentRows = options.currentTurnAgentActivityRows ?? [];
  const currentPaths = new Set(currentRows.map((row) => row.agent_path));
  const rows = (options.agentActivityRows ?? []).filter((row) => (
    !currentPaths.has(row.agent_path)
    && lastAgentSummaryIndexes.get(row.agent_path) === summaryIndex
  ));
  if (summaryIndex !== lastWorkSummaryIndex) return rows;
  currentRows.forEach((row) => {
    if (!rows.some((candidate) => candidate.agent_path === row.agent_path)) rows.push(row);
  });
  return rows;
}

function appendLiveAgentFallback(
  events: AgentHistoryEvent[],
  rows: readonly AgentActivityRow[],
): void {
  const represented = new Set(events.map((event) => event.agentPath));
  rows.forEach((row) => {
    if (represented.has(row.agent_path)) return;
    represented.add(row.agent_path);
    events.push({
      agentPath: row.agent_path,
      rowKind: row.status === "interrupted" ? "sub_agent_interrupted" : "sub_agent_updated",
    });
  });
}

function agentEventStatus(
  event: AgentHistoryEvent,
  activity: AgentActivityRow | undefined,
  latestForAgent: boolean,
): string {
  if (event.rowKind === "sub_agent_interrupted") return "中断しました";
  const terminalStatus = latestForAgent && activity?.status === "completed"
    ? "完了しました"
    : latestForAgent && activity?.status === "errored"
      ? "エラーになりました"
      : latestForAgent && activity?.status === "interrupted"
        ? "中断しました"
        : latestForAgent && activity?.status === "shutdown"
          ? "停止しました"
          : null;
  if (event.rowKind === "sub_agent_started" && !terminalStatus) return "作業を開始しました";
  if (terminalStatus) return terminalStatus;
  if (activity?.status === "running" || activity?.status === "pending_init") {
    return agentStatusLabel(activity.status);
  }
  return "更新しました";
}

function agentNameFromPath(path: string): string {
  const name = path.split("/").filter(Boolean).pop()?.trim() || "Sub Agent";
  return name.replace(/[_-]+/g, " ");
}

function humanizeAgentName(value: string): string {
  const spaced = value.trim().replace(/[_-]+/g, " ").replace(/\s+/g, " ");
  if (!spaced) return "Sub Agent";
  return `${spaced.charAt(0).toUpperCase()}${spaced.slice(1)}`;
}

function plainMarkdownText(value: string): string {
  return value
    .replace(/```[\s\S]*?```/g, " コード ")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/\*\*([^*]+)\*\*/g, "$1")
    .replace(/__([^_]+)__/g, "$1")
    .replace(/~~([^~]+)~~/g, "$1")
    .replace(/^#{1,6}\s+/gm, "")
    .replace(/[*~]/g, "")
    .replace(/\s+/g, " ")
    .trim();
}

function workHistoryGlyph(status: string): string {
  if (status === "完了") return "✓";
  if (status === "失敗" || status === "拒否") return "!";
  if (status === "実行中") return "•";
  return "›";
}

function prefixedAnchorId(prefix: string, id: string): string {
  return prefix.length > 0 ? `${prefix}-${id}` : id;
}

import { convertFileSrc } from "@tauri-apps/api/core";
import { actionById, menuActions, paletteActions, shortcutActions, type ActionDefinition, type ActionMenu } from "./actions.ts";
import { icon } from "./icons.ts";
import { renderMarkdown } from "./markdown.ts";
import { navigationIsIdle, quickChatDeleteAction, sessionRowCapabilities } from "./navigation_state.ts";
import {
  renderAgentInspector,
  renderInlineAgentActivity,
  renderSubAgentSummaryTrigger,
} from "./render_agent_activity.ts";
import { agentActivitySummary } from "./agent_activity.ts";
import { runCanBeCancelled, runSurfaceActive } from "./run_control.ts";
import type {
  ConfigFieldProjection,
  DesktopViewState,
  DesktopWebState,
  FileChangeRow,
  ProjectRow,
  SessionRow,
  TranscriptRow,
} from "./types.ts";
import type { ArtifactPaneMode } from "./ui_state.ts";
import { displayAccessLabel, escapeHtml, fileName, goalSlashCommandHint, shortenPath } from "./utils.ts";
import { providerCapabilities } from "./view_state.ts";

export type { LocalConfirmation } from "./render_overlays.ts";
export { renderConfirmation, renderLocalConfirmation } from "./render_overlays.ts";

const splashLogoUrl = new URL("../../../logo/fabicon/android-chrome-512x512.png", import.meta.url).href;

interface RenderContext {
  artifactPaneCollapsed: boolean;
  artifactPaneMode?: ArtifactPaneMode;
  selectedAgentPath?: string | null;
  attachmentTrayOpen: boolean;
  configDirty: boolean;
  configMutationPending: boolean;
  configOwnerMutationOpen: boolean;
  configDraftEditOpen: boolean;
  configDraftDiscardOpen: boolean;
  configDraftCommitOpen: boolean;
}

let artifactPaneCollapsed = false;
let artifactPaneMode: ArtifactPaneMode = "output";
let selectedAgentPath: string | null = null;
let attachmentTrayOpen = false;
let configDirty = false;
let configMutationInFlight = false;
let configOwnerMutationIsOpen = true;
let configDraftEditingIsOpen = true;
let configDraftDiscardIsOpen = false;
let configDraftCommitIsOpen = false;

const TYPED_CONFIG_KEYS = new Set([
  "model.base_url",
  "model.model",
  "model.provider_metadata_mode",
  "model.context_window",
  "model.max_output_tokens",
  "model.temperature",
  "model.top_p",
  "model.supports_tools",
  "model.supports_reasoning",
  "model.supports_images",
  "model.parallel_tool_calls",
  "permissions.access_mode",
  "multi_agent.enabled",
  "multi_agent.mode",
  "multi_agent.max_concurrent_agents",
  "multi_agent.max_concurrent_model_requests",
  "shell.hide_windows",
  "inspection.default_max_depth",
  "inspection.default_max_entries_per_dir",
  "inspection.max_extensions_reported",
  "inspection.include_hidden_by_default",
  "file_guard.max_inline_read_bytes",
  "file_guard.large_file_warning_bytes",
  "file_guard.blocked_read_extensions",
  "file_guard.structured_document_extensions",
  "docling.enabled",
  "docling.base_url",
  "docling.timeout_ms",
  "docling.api_key_env",
  "docling.headers_json",
  "mcp.enabled",
  "mcp.servers_json",
]);

export function setRenderContext(context: RenderContext): void {
  artifactPaneCollapsed = context.artifactPaneCollapsed;
  artifactPaneMode = context.artifactPaneMode ?? "output";
  selectedAgentPath = context.selectedAgentPath ?? null;
  attachmentTrayOpen = context.attachmentTrayOpen;
  configDirty = context.configDirty;
  configMutationInFlight = context.configMutationPending;
  configOwnerMutationIsOpen = context.configOwnerMutationOpen;
  configDraftEditingIsOpen = context.configDraftEditOpen;
  configDraftDiscardIsOpen = context.configDraftDiscardOpen;
  configDraftCommitIsOpen = context.configDraftCommitOpen;
}

export function renderStartupSplash(state: DesktopWebState, elapsedMs: number, minVisibleMs: number): string {
  const remainingMs = Math.max(0, minVisibleMs - elapsedMs);
  const progressLabel =
    remainingMs > 0
      ? "起動中"
      : state.startup.status === "ready"
        ? "準備完了"
        : "確認が必要";
  return `
    <div class="splash-screen">
      <div class="splash-core">
        <img class="splash-logo" src="${splashLogoUrl}" alt="moyAI" />
        <div class="splash-title">${escapeHtml(state.startup.title)}</div>
        <div class="splash-message">${escapeHtml(state.startup.message)}</div>
        <div class="splash-progress" aria-label="${escapeHtml(progressLabel)}">
          <span></span>
        </div>
        <div class="splash-detail">${escapeHtml(state.startup.detail)}</div>
        <div class="splash-checks">
          ${state.startup.checks
            .map(
              (check) => `
                <div class="splash-check ${check.status}">
                  <span class="splash-check-status">${startupCheckMark(check.status)}</span>
                  <span class="splash-check-label">${escapeHtml(check.label)}</span>
                  <span class="splash-check-message">${escapeHtml(check.message)}</span>
                </div>
              `,
            )
            .join("")}
        </div>
      </div>
    </div>
  `;
}

function startupCheckMark(status: string): string {
  if (status === "pass") return "OK";
  if (status === "warning") return "!";
  if (status === "fail") return "NG";
  return "…";
}

export function renderTitlebar(maximized = false, applicationCommandsInert = false): string {
  const maximizeLabel = maximized ? "元のサイズに戻す" : "最大化";
  const applicationCommandsState = applicationCommandsInert ? ' inert aria-hidden="true"' : "";
  return `
    <header class="app-titlebar">
      <div class="titlebar-left">
        <span class="app-brand" data-drag-region>moyAI</span>
        <nav class="titlebar-menu"${applicationCommandsState}>
          <button data-action="show-file-menu" aria-label="ファイルメニュー">ファイル</button>
          <button data-action="show-edit-menu" aria-label="編集メニュー">編集</button>
          <button data-action="show-view-menu" aria-label="表示メニュー">表示</button>
          <button data-action="show-help-menu" aria-label="ヘルプメニュー">ヘルプ</button>
        </nav>
      </div>
      <div class="titlebar-drag" data-drag-region></div>
      <div class="titlebar-controls">
        <button type="button" data-window-control data-action="minimize-window" title="最小化" aria-label="最小化"><span class="window-control-icon minimize-icon" aria-hidden="true"></span></button>
        <button type="button" data-window-control data-action="toggle-maximize-window" title="${maximizeLabel}" aria-label="${maximizeLabel}" aria-pressed="${maximized}"><span class="window-control-icon maximize-icon ${maximized ? "restore" : ""}" aria-hidden="true"></span></button>
        <button type="button" data-window-control data-action="close-window" title="閉じる" aria-label="閉じる"><span class="window-control-icon close-icon" aria-hidden="true"></span></button>
      </div>
    </header>
  `;
}

function renderProjectRowWithSessions(state: DesktopWebState, row: ProjectRow, index: number): string {
  const selected = index === state.selected_project_index;
  const projectRow = renderProjectRow(
    row,
    selected,
    index,
    !navigationIsIdle(state),
  );
  if (!selected) {
    return projectRow;
  }
  const sessionRows = renderProjectSessionRows(state);
  return `${projectRow}${sessionRows}`;
}

function renderProjectRow(row: ProjectRow, selected: boolean, index: number, actionsDisabled: boolean): string {
  const disabled = actionsDisabled ? ' disabled aria-disabled="true"' : "";
  return `
    <div class="nav-row-wrap project-row ${selected ? "selected" : ""}">
      <button class="nav-row" data-action="project" data-index="${index}" data-focus-key="project:${escapeHtml(row.project_id)}:select"${selected ? ' aria-current="page"' : ""}${disabled}>
        <span class="nav-title">${escapeHtml(row.label)}</span>
        <small>${escapeHtml(row.path)}</small>
      </button>
      <button class="row-action add-session" data-action="new-project-session" data-index="${index}" data-focus-key="project:${escapeHtml(row.project_id)}:new-session" title="このプロジェクトで新しい開発チャット" aria-label="このプロジェクトで新しい開発チャット"${disabled}>${icon("plus")}</button>
      <button class="row-action danger" data-action="delete-project" data-index="${index}" data-focus-key="project:${escapeHtml(row.project_id)}:delete" title="削除" aria-label="削除"${disabled}>${icon("x")}</button>
    </div>
  `;
}

function renderProjectSessionRows(state: DesktopWebState): string {
  if (state.selected_project_index < 0) {
    return "";
  }
  const searchDisabled = !navigationIsIdle(state)
    ? ' disabled aria-disabled="true"'
    : "";
  const search = `
    <div class="session-search">
      <input id="session-search" value="${escapeHtml(state.session_search_text)}" placeholder="セッション検索" aria-label="セッション検索"${searchDisabled} />
      <button class="${state.session_search_include_archived ? "selected" : ""}" data-action="toggle-session-archived-search" title="アーカイブ済みを含める" aria-label="アーカイブ済みを含める"${searchDisabled}>${icon("archive")}</button>
    </div>
  `;
  const rows = state.session_rows
    .map((row, index) => {
      const capabilities = sessionRowCapabilities(
        row.loaded_status,
        row.archived,
      );
      return renderNavRow(
        row.label,
        sessionRowSubtitle(row, "開発チャット"),
        index === state.selected_session_index,
        "session",
        index,
        capabilities.rejoinAction,
        capabilities.secondaryAction,
        capabilities.rollbackAction,
        capabilities.deleteAction,
        runSurfaceActive(state) && index === state.selected_session_index,
        !navigationIsIdle(state),
        `session:${row.session_id}`,
      );
    })
    .join("");
  const activeFallback = rows.length === 0 ? renderActiveProjectSessionPlaceholder(state) : "";
  return rows.length > 0 || activeFallback.length > 0 || state.session_search_text.trim().length > 0
    ? `<div class="project-session-list">${search}${rows}${activeFallback}</div>`
    : `<div class="project-session-list">${search}</div>`;
}

function renderActiveProjectSessionPlaceholder(state: DesktopWebState): string {
  if (!runSurfaceActive(state)) {
    return "";
  }
  const label = activeSessionLabel(state);
  if (!label) {
    return "";
  }
  return `
    <div class="nav-row-wrap selected project-session-placeholder">
      <div class="nav-row">
        <span class="nav-title"><span class="busy-spinner" title="実行中"></span><span>${escapeHtml(label)}</span></span>
        <small>開発チャット</small>
      </div>
    </div>
  `;
}

function activeSessionLabel(state: DesktopWebState): string {
  const candidates = [state.current_session_label, state.selected_session_title];
  for (const candidate of candidates) {
    const label = candidate.trim();
    if (label.length > 0 && label !== "新規チャット" && label !== "セッション未選択") {
      return label;
    }
  }
  return "";
}

function renderChatRows(state: DesktopWebState): string {
  if (state.chat_session_rows.length === 0) {
    return '<div class="empty">チャットはありません</div>';
  }
  const selectedChatSessionId =
    state.selected_project_index < 0 && state.selected_session_index >= 0
      ? state.session_rows[state.selected_session_index]?.session_id
      : undefined;
  return state.chat_session_rows
    .map((row, index) =>
      renderNavRow(
        row.label,
        sessionRowSubtitle(row, "通常チャット"),
        row.session_id === selectedChatSessionId,
        "chat-session",
        index,
        "",
        "",
        "",
        quickChatDeleteAction(row.loaded_status),
        state.selected_project_index < 0 && runSurfaceActive(state) && row.session_id === selectedChatSessionId,
        !navigationIsIdle(state),
        `chat-session:${row.session_id}`,
      )
    )
    .join("");
}

function sessionRowSubtitle(row: SessionRow, fallback: string): string {
  if (row.loaded_status === "active") {
    const pending = row.pending_user_input_requests + row.pending_permission_requests;
    const prefix = pending > 0 ? "確認待ち" : "実行中";
    const turn =
      typeof row.active_turn_sequence_no === "number"
        ? `turn ${row.active_turn_sequence_no}`
        : row.active_turn_id
          ? `turn ${row.active_turn_id.slice(0, 8)}`
          : "active turn";
    return `${prefix} · ${turn}`;
  }
  if (row.loaded_status === "system_error") {
    return "状態取得エラー";
  }
  return fallback;
}

export function renderSidebar(state: DesktopWebState): string {
  const chatRunning = state.selected_project_index < 0 && runSurfaceActive(state);
  const navigationDisabled = !navigationIsIdle(state);
  return `
    <aside class="sidebar">
      <div class="window-actions">
        <button class="icon-button" data-action="show-shortcuts" title="ショートカット" aria-label="ショートカット">${icon("keyboard")}</button>
        <button class="icon-button" data-action="refresh" title="更新" aria-label="更新">${icon("refresh")}</button>
      </div>
      <button class="rail-item" data-action="show-provider" title="LLM URL">
        <span class="rail-icon">${icon("plug")}</span><span>LLM URL</span>
      </button>
      <div class="rail-section row-heading">
        <span>プロジェクト</span>
        <button class="tiny-button icon-only" data-action="create-project-from-picker" title="プロジェクトを作成" aria-label="プロジェクトを作成" ${navigationDisabled ? "disabled" : ""}>${icon("folder-plus")}</button>
      </div>
      <div class="row-list project-list">
        ${state.project_rows
          .map((row, index) => renderProjectRowWithSessions(state, row, index))
          .join("")}
      </div>
      <div class="rail-section row-heading">
        <span class="section-label">チャット${chatRunning ? '<span class="busy-spinner small" title="実行中"></span>' : ""}</span>
        <button class="tiny-button icon-only" data-action="new-chat" title="新しい通常チャット" aria-label="新しい通常チャット" ${navigationDisabled ? "disabled" : ""}>${icon("plus")}</button>
      </div>
      <div class="row-list chat-list">${renderChatRows(state)}</div>
      <button class="settings" data-action="show-config" title="設定"><span class="rail-icon">${icon("settings")}</span><span>設定</span></button>
    </aside>
  `;
}

export function renderTopbar(state: DesktopViewState): string {
  const workspaceLabel = state.selected_project_index >= 0 ? shortenPath(state.workspace_path) : "プロジェクトなし";
  const projectContextAction = state.selected_project_index >= 0 ? "open-workspace-folder" : "create-project-from-picker";
  const exportDisabled = !state.history_export_enabled || !navigationIsIdle(state);
  const exportTitle = exportDisabled ? "保存できる表示中の履歴がありません" : "表示中の履歴をMarkdown保存";
  const turnPageVisible = state.turn_page_total > state.turn_page_limit && state.turn_page_limit > 0;
  const turnPageStart = state.turn_page_total === 0 ? 0 : state.turn_page_offset + 1;
  const turnPageEnd = Math.min(state.turn_page_total, state.turn_page_offset + state.turn_page_limit);
  const navigationBlocked = !navigationIsIdle(state);
  const previousDisabled = state.turn_page_offset === 0 || navigationBlocked;
  const nextDisabled = !state.turn_page_has_more || navigationBlocked;
  return `
    <header class="topbar">
      <div class="title-row">
        <div class="title-copy">
          <h1>${escapeHtml(state.selected_session_title)}</h1>
          <div class="status-line ${state.status_detail.trim().length > 0 ? "has-detail" : ""}">
            <span>${escapeHtml(state.status_message)}</span>
            ${
              state.status_detail.trim().length > 0
                ? `<details data-details-key="status-detail">
                    <summary>詳細</summary>
                    <pre>${escapeHtml(state.status_detail)}</pre>
                  </details>`
                : ""
            }
          </div>
        </div>
        <div class="chips">
          <button data-action="${projectContextAction}" title="${escapeHtml(state.workspace_path)}">${escapeHtml(workspaceLabel)}</button>
          <button data-action="show-provider" title="${escapeHtml(state.provider_label)}">
            <span>${escapeHtml(state.model_label)}</span><small>${escapeHtml(state.provider_label)}</small>
          </button>
           <button data-action="toggle-access" title="アクセス権限" aria-disabled="${state.config_draft.access_mode_mutation_enabled ? "false" : "true"}" ${state.config_draft.access_mode_mutation_enabled ? "" : "disabled"}>${escapeHtml(displayAccessLabel(state.access_label))}</button>
          ${
            turnPageVisible
              ? `<span class="turn-page-chip">${turnPageStart}-${turnPageEnd}/${state.turn_page_total}</span>
                 <button class="icon-button" data-action="load-previous-turn-page" title="前の履歴ページ" aria-label="前の履歴ページ" ${previousDisabled ? "disabled" : ""}>${icon("chevron-left")}</button>
                 <button class="icon-button" data-action="load-next-turn-page" title="次の履歴ページ" aria-label="次の履歴ページ" ${nextDisabled ? "disabled" : ""}>${icon("chevron-right")}</button>`
              : ""
          }
          <button class="icon-button" data-action="export-transcript" title="${exportTitle}" aria-label="${exportTitle}" ${exportDisabled ? "disabled" : ""}>${icon("download")}</button>
        </div>
      </div>
    </header>
  `;
}

export function renderRunStatusStrip(state: DesktopWebState): string {
  if (!runCanBeCancelled(state)) {
    return "";
  }
  const agentTreeOnly = state.agent_tree_active && !state.busy && !state.confirmation_visible;
  const phase = agentTreeOnly ? "Sub Agent" : state.run_phase.trim() || "running";
  const step = agentTreeOnly
    ? agentActivitySummary(state.agent_activity_rows ?? [], true)
    : state.run_active_step.trim() || state.status_message;
  const toolLine = agentTreeOnly ? "子Agentの完了を待機中" : state.latest_tool_summary.trim() || "ツール待機中";
  const statusLabel = state.confirmation_visible ? "確認待ち" : agentTreeOnly ? "Sub Agent実行中" : "実行中";
  return `
    <section class="run-strip" aria-live="polite">
      <span class="busy-spinner" title="${statusLabel}"></span>
      <strong>${statusLabel}</strong>
      <span>${escapeHtml(phase)}</span>
      <span>${escapeHtml(step)}</span>
      <small>${escapeHtml(toolLine)}</small>
      <button class="icon-only danger" data-action="cancel-run" title="実行停止" aria-label="実行停止">${icon("square")}</button>
    </section>
  `;
}

export function renderThreadContent(state: DesktopWebState): string {
  const agentActivity = renderInlineAgentActivity(state, artifactPaneMode === "agents");
  if ((state.thread_empty || state.selected_session_index < 0) && state.file_change_rows.length === 0) {
    return `${renderEmptyThread(state)}${agentActivity}`;
  }
  return `${state.transcript_rows.map(renderTranscriptCard).join("")}${agentActivity}`;
}

function renderEmptyThread(state: DesktopWebState): string {
  const projectText =
    state.selected_project_index >= 0
      ? "このプロジェクトで最初のセッションを作成します"
      : "通常チャットとして新しいセッションを作成します";
  const llmText = state.model_label.trim().length > 0 ? `LLM: ${state.model_label}` : "LLM URL を設定してください";
  return `
    <div class="empty-thread">
      <h2>${state.selected_project_index >= 0 ? "このプロジェクトで何を作りますか？" : "何に取り組みますか？"}</h2>
      <div class="empty-status">
        ${state.selected_project_index >= 0 ? `<span>${escapeHtml(shortenPath(state.workspace_path))}</span>` : ""}
        <span>${escapeHtml(projectText)}</span>
        <span>${escapeHtml(llmText)}</span>
        <span>${escapeHtml(displayAccessLabel(state.access_label))}</span>
      </div>
    </div>
  `;
}

function renderTranscriptCard(row: TranscriptRow): string {
  const rowKind = row.row_kind;
  if (rowKind.startsWith("work_summary")) {
    return renderWorkSummaryCard(row);
  }
  if (rowKind === "file_changes") {
    return renderFileChangesTranscriptCard(row);
  }
  return `
    <article class="message ${escapeHtml(rowKind)}">
      <div class="message-step">${escapeHtml(row.step)}</div>
      <div class="message-body">
        <h2>${escapeHtml(row.title)}</h2>
        <div class="markdown-body">${renderMarkdown(row.body)}</div>
      </div>
    </article>
  `;
}

function renderFileChangesTranscriptCard(row: TranscriptRow): string {
  const changes = row.file_changes;
  const body = changes.length === 0 ? `<div class="markdown-body">${renderMarkdown(row.body)}</div>` : renderTranscriptFileChangeTable(changes);
  return `
    <article class="message file_changes">
      <div class="message-step">${escapeHtml(row.step)}</div>
      <div class="message-body">
        <h2>${escapeHtml(row.title)}</h2>
        ${body}
      </div>
    </article>
  `;
}

function renderTranscriptFileChangeTable(changes: FileChangeRow[]): string {
  const rows = changes
    .map(
      (change) => `
        <div class="transcript-change-row">
          <span class="transcript-change-action">${escapeHtml(change.action)}</span>
          <strong title="${escapeHtml(change.path)}">${escapeHtml(change.path)}</strong>
          <small>${escapeHtml(change.summary || change.label || change.path)}</small>
        </div>
      `,
    )
    .join("");
  return `
    <div class="transcript-change-table" role="table" aria-label="ファイル変更結果">
      <div class="transcript-change-row transcript-change-head" role="row">
        <span>操作</span>
        <span>ファイル</span>
        <span>内容</span>
      </div>
      ${rows}
    </div>
  `;
}

function renderWorkSummaryCard(row: TranscriptRow): string {
  const rowKind = row.row_kind;
  const running = rowKind === "work_summary_running";
  const incomplete = rowKind === "work_summary_incomplete";
  const open = running || incomplete ? "open" : "";
  const statusText = running ? "実行中" : incomplete ? "状態未確定" : "作業サマリ";
  const summary = extractMarkdownSections(row.body, ["作業サマリ", "完了"]);
  const history = removeMarkdownSections(row.body, ["作業サマリ", "完了"]);
  const visibleSummary = !running && summary.trim().length > 0 ? summary : "";
  const detailBody = visibleSummary.length > 0 && history.trim().length > 0 ? history : row.body;
  return `
    <article class="message work-summary ${escapeHtml(rowKind)}">
      <div class="message-step">${escapeHtml(row.step)}</div>
      <div class="message-body">
        ${
          visibleSummary.length > 0
            ? `<h2>${escapeHtml(row.title)}</h2><div class="markdown-body work-summary-visible">${renderMarkdown(visibleSummary)}</div>`
            : ""
        }
        <details data-details-key="work-summary:${escapeHtml(`${row.step}:${rowKind}:${row.title}`)}" ${open}>
          <summary>
            <span>${escapeHtml(visibleSummary.length > 0 ? "作業履歴" : row.title)}</span>
            <small>${escapeHtml(statusText)}</small>
          </summary>
          <div class="markdown-body">${renderMarkdown(detailBody)}</div>
        </details>
      </div>
    </article>
  `;
}

function extractMarkdownSections(body: string, headings: string[]): string {
  const wanted = new Set(headings);
  const lines = body.replace(/\r\n/g, "\n").split("\n");
  const result: string[] = [];
  let taking = false;
  for (const line of lines) {
    const heading = line.match(/^###\s+(.+)$/);
    if (heading) {
      taking = wanted.has(heading[1].trim());
    }
    if (taking) result.push(line);
  }
  return result.join("\n").trim();
}

function removeMarkdownSections(body: string, headings: string[]): string {
  const unwanted = new Set(headings);
  const lines = body.replace(/\r\n/g, "\n").split("\n");
  const result: string[] = [];
  let dropping = false;
  for (const line of lines) {
    const heading = line.match(/^###\s+(.+)$/);
    if (heading) {
      dropping = unwanted.has(heading[1].trim());
    }
    if (!dropping) result.push(line);
  }
  return result.join("\n").trim();
}

export function renderComposer(state: DesktopWebState): string {
  const projectContextAction = state.selected_project_index >= 0 ? "open-workspace-folder" : "create-project-from-picker";
  const sendTitle = state.navigation_loading
    ? "画面の切り替え完了後に送信できます"
    : state.agent_tree_active
      ? "Sub Agentの完了または停止後に送信できます"
    : state.busy
      ? "実行中は送信できません"
      : state.draft_prompt.trim().length === 0
        ? "依頼文を入力してください"
        : "送信";
  const enhanceTitle = state.navigation_loading
    ? "画面の切り替え完了後にEnhanceできます"
    : state.agent_tree_active
      ? "Sub Agentの完了または停止後にEnhanceできます"
    : state.busy
      ? "実行中はEnhanceできません"
      : state.draft_prompt.trim().length === 0
        ? "依頼文を入力してください"
        : "Enhance";
  const controlsVisible = attachmentTrayOpen || state.image_input.trim().length > 0;
  const trayVisible = controlsVisible || state.attached_images.length > 0;
  const goalHint = goalSlashCommandHint(state.draft_prompt);
  return `
    <section class="composer ${goalHint ? "goal-command" : ""}">
      ${trayVisible ? renderAttachmentTray(state, controlsVisible) : ""}
      <textarea id="prompt" placeholder="moyAI に依頼する" aria-describedby="goal-command-hint" ${state.navigation_loading ? "disabled" : ""}>${escapeHtml(state.draft_prompt)}</textarea>
      <div class="goal-command-hint" id="goal-command-hint" ${goalHint ? "" : "hidden"}>
        <span class="goal-command-badge">/goal</span>
        <span data-goal-command-help>${escapeHtml(goalHint ?? "")}</span>
      </div>
      <div class="composer-actions">
        <button class="add-button icon-only" data-action="toggle-attachment-tray" title="画像添付" aria-label="画像添付" ${state.image_input_enabled ? "" : "disabled"}>${icon("plus")}</button>
        <button class="icon-only" data-action="show-command-palette" title="検索 / コマンド" aria-label="検索 / コマンド">${icon("more")}</button>
        <button class="icon-only" data-action="enhance-prompt" title="${enhanceTitle}" aria-label="${enhanceTitle}" ${state.enhance_enabled ? "" : "disabled"}>${icon("sparkles")}</button>
        <button class="send icon-only" data-action="send" title="${sendTitle}" aria-label="${sendTitle}" ${state.can_submit ? "" : "disabled"}>${icon("send")}</button>
      </div>
      <div class="composer-meta">
          <button data-action="${projectContextAction}" title="${escapeHtml(state.workspace_path)}">${state.selected_project_index >= 0 ? "プロジェクトで作業" : "プロジェクトを選択"}</button>
        ${renderTokenMeter(state)}
      </div>
    </section>
  `;
}

function renderTokenMeter(state: DesktopWebState): string {
  const label = state.token_meter_label.trim();
  if (label.length === 0) {
    return "";
  }
  const level = state.token_meter_level.trim() || "unknown";
  return `
    <span class="token-meter ${escapeHtml(level)}" title="${escapeHtml(state.token_meter_title)}" aria-label="${escapeHtml(state.token_meter_title)}">
      <span class="token-meter-dot"></span>
      <span>${escapeHtml(label)}</span>
    </span>
  `;
}

function renderAttachmentTray(state: DesktopWebState, controlsVisible: boolean): string {
  return `
    <div class="attachment-tray ${controlsVisible ? "expanded" : "compact"}">
      <div class="attachment-row">
        ${renderAttachedImages(state)}
      </div>
      ${
        controlsVisible
          ? `<div class="attachment-controls">
              <input id="image-input" value="${escapeHtml(state.image_input)}" placeholder="画像ファイルのパス" ${state.image_input_enabled ? "" : "disabled"} />
              <button class="icon-only" data-action="set-image" title="画像を添付" aria-label="画像を添付" ${state.image_input_enabled ? "" : "disabled"}>${icon("upload")}</button>
              <button class="icon-only" data-action="browse-image" title="画像を参照" aria-label="画像を参照" ${state.image_input_enabled ? "" : "disabled"}>${icon("folder")}</button>
              <button class="icon-only" data-action="clear-images" title="添付を解除" aria-label="添付を解除" ${state.attached_images.length > 0 ? "" : "disabled"}>${icon("x")}</button>
            </div>`
          : ""
      }
    </div>
  `;
}

function renderAttachedImages(state: DesktopWebState): string {
  if (state.attached_images.length === 0) {
    return '<span class="attachment-empty">画像は未添付です</span>';
  }
  return state.attached_images
    .map((path, index) => {
      const thumbnail = attachmentThumbnailSrc(path);
      return `
        <button class="thumb image-thumb" data-action="remove-image" data-index="${index}" data-focus-key="attachment:${escapeHtml(path)}" title="${escapeHtml(path)}" aria-label="添付画像を削除: ${escapeHtml(fileName(path))}">
          ${
            thumbnail
              ? `<img src="${escapeHtml(thumbnail)}" alt="" loading="lazy" />`
              : `<span class="thumb-fallback">${icon("image")}</span>`
          }
          <span>${escapeHtml(fileName(path))}</span><b>×</b>
        </button>`;
    })
    .join("");
}

function attachmentThumbnailSrc(path: string): string {
  try {
    return convertFileSrc(path);
  } catch {
    return "";
  }
}

function planStepStatusLabel(status: "pending" | "in_progress" | "completed"): string {
  if (status === "completed") return "完了";
  if (status === "in_progress") return "進行中";
  return "未着手";
}

export function renderPlanProjection(state: DesktopWebState): string {
  const plan = state.plan;
  if (!plan || (plan.steps.length === 0 && !(plan.explanation ?? "").trim())) return "";
  return `
    <section class="output-file-section" aria-label="作業計画">
      <div class="output-section-heading">
        <strong>計画</strong>
        <small>${plan.steps.length}件</small>
      </div>
      ${plan.explanation?.trim() ? `<p>${escapeHtml(plan.explanation.trim())}</p>` : ""}
      <ol class="plan-list">
        ${plan.steps
          .map(
            (step) => `<li data-plan-status="${step.status}"><span>${escapeHtml(planStepStatusLabel(step.status))}</span> ${escapeHtml(step.step)}</li>`,
          )
          .join("")}
      </ol>
    </section>
  `;
}

export function renderArtifactPane(state: DesktopWebState): string {
  if (artifactPaneCollapsed) {
    return `
      <aside class="artifact-pane collapsed">
        <button class="pin" data-action="toggle-artifact-pane" title="出力を表示" aria-label="出力を表示">${icon("folder")}</button>
      </aside>
    `;
  }
  if (artifactPaneMode === "agents") {
    return `
      <aside id="sub-agent-inspector" class="artifact-pane agent-inspector-pane" data-pane-mode="sub-agents" aria-label="Sub Agent履歴">
        <div class="pane-title agent-pane-title">
          <button class="agent-pane-back" data-action="show-output-pane" aria-label="出力ペインに戻る">‹ <span>出力</span></button>
          <strong>サブエージェント</strong>
          <button class="pin" data-action="toggle-artifact-pane" title="Sub Agentペインを閉じる" aria-label="Sub Agentペインを閉じる">${icon("x")}</button>
        </div>
        ${renderAgentInspector(state, selectedAgentPath)}
      </aside>
    `;
  }
  const hasPreview = state.artifact_preview_available;
  const artifactNavigationBlocked = !navigationIsIdle(state);
  const artifactFolderDisabled = state.selected_artifact_index < 0
    || state.artifact_rows[state.selected_artifact_index] === undefined
    || artifactNavigationBlocked;
  const artifactFolderDisabledAttrs = artifactFolderDisabled
    ? ` disabled aria-disabled="true" title="${artifactNavigationBlocked ? "画面の切り替え完了後に開けます" : "アーティファクトを選択してください"}"`
    : ' title="アーティファクトのフォルダーを開く"';
  const hasActivity = state.busy && (state.progress_text.trim().length > 0 || state.tool_status_text.trim().length > 0);
  return `
    <aside class="artifact-pane" data-pane-mode="output">
      <div class="pane-title">
        <strong>出力</strong>
        <div class="pane-actions">
          <button class="pin" data-action="toggle-artifact-pane" title="出力ペインを閉じる" aria-label="出力ペインを閉じる">${icon("x")}</button>
          <button class="pin" data-action="open-artifact-folder"${artifactFolderDisabledAttrs} aria-label="アーティファクトのフォルダーを開く">${icon("folder")}</button>
        </div>
      </div>
      ${renderSubAgentSummaryTrigger(state)}
      ${renderPlanProjection(state)}
      <section class="output-file-section" aria-label="ファイル出力">
        <div class="output-section-heading">
          <strong>ファイル</strong>
          <small>${state.artifact_rows.length}件</small>
        </div>
        <div class="artifact-list">
          ${
            state.artifact_rows.length === 0
              ? '<div class="empty artifact-empty">生成ファイル、開いたファイル、変更履歴がここに表示されます</div>'
              : state.artifact_rows
                  .map(
                    (row, index) => `
                      <button class="artifact-row ${index === state.selected_artifact_index ? "selected" : ""}"
                        data-action="artifact" data-index="${index}" data-focus-key="artifact:${escapeHtml(row.path)}"${index === state.selected_artifact_index ? ' aria-current="true"' : ""} ${artifactNavigationBlocked ? 'disabled aria-disabled="true"' : ""}>
                        <span class="file-icon">▣</span>
                        <span><b>${escapeHtml(row.label)}</b><small>${escapeHtml(row.path)}</small></span>
                      </button>`
                  )
                  .join("")
          }
        </div>
      </section>
      ${
        hasPreview
          ? `<div class="preview">
              <div class="preview-tabs">
                <span>プレビュー</span>
                <button data-action="open-artifact-folder" ${artifactFolderDisabled ? "disabled aria-disabled=\"true\"" : ""}>開く</button>
              </div>
              <pre>${escapeHtml(state.artifact_preview_text)}</pre>
            </div>`
          : ""
      }
      ${
        hasActivity
          ? `<div class="activity">
              <h3>進捗</h3>
              <pre>${escapeHtml(state.progress_text)}</pre>
              <h3>ツール</h3>
              <pre>${escapeHtml(state.tool_status_text)}</pre>
            </div>`
          : ""
      }
    </aside>
  `;
}

export function renderOverlay(state: DesktopViewState): string {
  if (state.overlay === "provider") return renderProviderOverlay(state);
  if (state.overlay === "config") return renderConfigOverlay(state);
  if (state.overlay === "workspace") return renderWorkspaceOverlay(state);
  if (state.overlay === "prompt_review") return renderPromptReviewOverlay(state);
  if (state.overlay === "command_palette") return renderCommandPalette(state);
  if (state.overlay === "shortcuts") return renderShortcuts();
  if (state.overlay === "project_menu") return "";
  if (state.overlay === "file_menu") return renderMenuPopover("file", menuActions("file", state));
  if (state.overlay === "edit_menu") return renderMenuPopover("edit", menuActions("edit", state));
  if (state.overlay === "view_menu") {
    return renderMenuPopover(
      "view",
      menuActions("view", state),
      `
        <div class="menu-slider" data-modal>
          <label class="field-label">ウィンドウ透過率</label>
          <input id="opacity-input" type="range" min="50" max="100" value="${state.window_opacity_percent}" />
        </div>
      `
    );
  }
  if (state.overlay === "help_menu") return renderMenuPopover("help", menuActions("help", state));
  return "";
}

function renderProviderOverlay(state: DesktopViewState): string {
  const selectedSummary = state.provider_selected_model_summary.length > 0 ? state.provider_selected_model_summary : ["モデル metadata は未取得です。"];
  const providerStatus = providerStatusView(state);
  const setupRequired = startupSetupRequired(state);
  const providerModeOptions = [
    ["lm_studio_native_required", "LM Studio native"],
    ["openai_compatible_only", "OpenAI互換のみ"],
  ] as const;
  return `
    <div class="modal-backdrop">
      <section class="modal wide ${setupRequired ? "setup-modal" : ""}" data-modal role="dialog" aria-modal="true" aria-labelledby="provider-dialog-title" tabindex="-1">
        <div class="modal-header">
          <h2 id="provider-dialog-title">${setupRequired ? "初期設定" : "LLM URL"}</h2>
          ${setupRequired ? "" : `<button class="icon-button" data-action="close-overlay" title="閉じる" aria-label="閉じる">${icon("x")}</button>`}
        </div>
        ${setupRequired ? `<p class="setup-message">${escapeHtml(state.startup.message)} ${escapeHtml(state.startup.detail)}</p>` : ""}
        <label class="field-label" for="provider-url">ベースURL</label>
        <input id="provider-url" value="${escapeHtml(state.provider_base_url)}" />
        <label class="field-label">Provider mode</label>
        <div class="segmented-control provider-mode-control">
          ${providerModeOptions
            .map(
              ([mode, label]) => `
                <button class="${state.provider_metadata_mode === mode ? "selected" : ""}" data-action="set-provider-mode" data-mode="${mode}" aria-pressed="${state.provider_metadata_mode === mode}">
                  ${escapeHtml(label)}
                </button>`
            )
            .join("")}
        </div>
        <div class="provider-limit-grid">
          <div>
            <label class="field-label" for="provider-context-window">Context window</label>
            <input id="provider-context-window" inputmode="numeric" value="${escapeHtml(state.provider_context_window)}" />
          </div>
          <div>
            <label class="field-label" for="provider-max-output-tokens">Max output tokens</label>
            <input id="provider-max-output-tokens" inputmode="numeric" value="${escapeHtml(state.provider_max_output_tokens)}" />
          </div>
        </div>
        <div class="split-actions">
          <button data-action="load-provider-models" ${providerCapabilities(state).canLoadProviderModels ? "" : "disabled"}>${state.provider_loading ? "読込中" : "モデル読込"}</button>
          <button data-action="apply-provider-session" ${state.provider_apply_enabled ? "" : "disabled"}>UIセッションに適用</button>
          <button data-action="save-provider-global" ${state.provider_apply_enabled ? "" : "disabled"}>設定ファイルに保存</button>
          ${setupRequired ? `<button data-action="import-config-toml" ${configOwnerMutationIsOpen ? "" : "disabled"}>config.tomlをImport</button>` : ""}
        </div>
        <div class="select-list">
          ${state.provider_models
            .map(
              (model, index) => `
                <button class="${index === state.provider_selected_index ? "selected" : ""}" data-action="select-provider-model" data-index="${index}" data-focus-key="provider-model:${escapeHtml(state.provider_model_ids[index] ?? model)}" aria-pressed="${index === state.provider_selected_index}">
                  ${escapeHtml(model)}
                </button>`
            )
            .join("")}
        </div>
        <details class="provider-details" data-details-key="provider-model-details">
          <summary>モデル詳細</summary>
          <div class="provider-summary">
            ${selectedSummary
              .map((line) => {
                const [label, ...rest] = line.split(": ");
                return `<div><span>${escapeHtml(label)}</span><strong>${escapeHtml(rest.join(": ") || line)}</strong></div>`;
              })
              .join("")}
          </div>
        </details>
        <div class="provider-status ${providerStatus.kind}">
          <strong>${escapeHtml(providerStatus.title)}</strong>
          <p>${escapeHtml(providerStatus.hint)}</p>
          ${
            providerStatus.details.trim().length > 0
              ? `<details data-details-key="provider-status-details"><summary>技術詳細</summary><pre>${escapeHtml(providerStatus.details)}</pre></details>`
              : ""
          }
        </div>
      </section>
    </div>
  `;
}

function providerStatusView(state: DesktopWebState): { kind: string; title: string; hint: string; details: string } {
  const typed = state.provider_status;
  return {
    kind: typed.kind === "success" ? "ok" : typed.kind,
    title: typed.title,
    hint: typed.hint,
    details: typed.details,
  };
}

function renderConfigOverlay(state: DesktopWebState): string {
  const setupRequired = startupSetupRequired(state);
  const title = setupRequired ? "初期設定" : "Preferences";
  const configCommitDisabled = !configDraftCommitIsOpen;
  return `
    <div class="modal-backdrop">
      <section class="modal settings-modal ${setupRequired ? "setup-modal" : ""}" data-modal role="dialog" aria-modal="true" aria-labelledby="config-dialog-title" tabindex="-1">
        <div class="settings-header">
          <div>
            <h2 id="config-dialog-title">${escapeHtml(title)}</h2>
            <p>${setupRequired ? "起動に必要な設定を確認します。" : "永続設定を編集します。セッションだけの変更は LLM URL または上部チップから行います。"}</p>
          </div>
          <div class="settings-header-actions">
            <span class="dirty-badge ${configDirty ? "visible" : ""}">変更あり</span>
            <button data-action="discard-config-draft" ${configDirty ? "" : "hidden"} ${configDraftDiscardIsOpen ? "" : "disabled"}>変更を破棄</button>
            <button data-action="apply-session-config" ${configCommitDisabled ? "disabled" : ""}>UIセッションに適用</button>
            <button data-action="save-global-config" ${configCommitDisabled ? "disabled" : ""}>設定ファイルに保存</button>
            ${setupRequired ? `<button data-action="import-config-toml" ${configOwnerMutationIsOpen ? "" : "disabled"}>config.tomlをImport</button>` : `<button class="icon-button" data-action="close-overlay" title="閉じる" aria-label="閉じる">${icon("x")}</button>`}
          </div>
        </div>
        ${setupRequired ? `<p class="setup-message">${escapeHtml(state.startup.message)} ${escapeHtml(state.startup.detail)}</p>` : ""}
        <div id="settings-validation" class="validation ok">${configDirty ? "未保存の設定があります。Apply、保存、または変更を破棄するまで別画面からの設定変更は停止します。" : "入力形式は問題ありません。"}</div>
        <div class="settings-layout">
          <nav class="settings-nav" aria-label="設定カテゴリ">
            <a href="#settings-provider">Provider</a>
            <a href="#settings-model">Model</a>
            <a href="#settings-permissions">Permissions</a>
            <a href="#settings-agents">Agents</a>
            <a href="#settings-tools">Tools</a>
            <a href="#settings-files">Files</a>
            <a href="#settings-advanced">Advanced</a>
            <button data-action="open-global-config-folder">設定フォルダーを開く</button>
          </nav>
          <div class="settings-content">
            <section id="settings-provider" class="settings-section">
              <div class="settings-section-head">
                <h3>Provider</h3>
                <button data-action="show-provider">LLM URL 画面を開く</button>
              </div>
              <div class="settings-grid-two">
                ${renderConfigTextField(state, "model.base_url", "Base URL", "url")}
                ${renderConfigTextField(state, "model.model", "Model")}
              </div>
              ${renderConfigEnumField(state, "model.provider_metadata_mode", "Provider mode", {
                lm_studio_native_required: "LM Studio metadata API",
                openai_compatible_only: "OpenAI compatible",
              })}
            </section>
            <section id="settings-model" class="settings-section">
              <h3>Model</h3>
              <div class="settings-grid-two">
                ${renderConfigTextField(state, "model.context_window", "Context window", "number")}
                ${renderConfigTextField(state, "model.max_output_tokens", "Max output tokens", "number")}
                ${renderConfigTextField(state, "model.temperature", "Temperature", "number")}
                ${renderConfigTextField(state, "model.top_p", "Top P", "number")}
              </div>
              <div class="settings-toggle-grid">
                ${renderConfigToggleField(state, "model.supports_tools", "Tools")}
                ${renderConfigToggleField(state, "model.supports_reasoning", "Reasoning")}
                ${renderConfigToggleField(state, "model.supports_images", "Images")}
                ${renderConfigToggleField(state, "model.parallel_tool_calls", "Parallel tool calls")}
              </div>
            </section>
            <section id="settings-permissions" class="settings-section">
              <h3>Permissions</h3>
              ${renderConfigEnumField(state, "permissions.access_mode", "Access mode", {
                default: "標準",
                full_access: "フルアクセス",
              })}
            </section>
            <section id="settings-agents" class="settings-section">
              <div class="settings-section-head">
                <div>
                  <h3>Agents</h3>
                  <p>通常のmoyAIセッションをSub Agentとして共有workspace上で実行します。変更は次回runから有効です。</p>
                </div>
                ${renderConfigToggleField(state, "multi_agent.enabled", "Multi-Agentを有効化")}
              </div>
              ${renderConfigEnumField(state, "multi_agent.mode", "起動モード", {
                explicit_request_only: "明示依頼時のみ",
                proactive: "必要に応じて自動",
              })}
              <div class="settings-grid-two">
                ${renderConfigTextField(state, "multi_agent.max_concurrent_agents", "同時Agent数（root込み）", "number")}
                ${renderConfigTextField(state, "multi_agent.max_concurrent_model_requests", "同時model request数", "number")}
              </div>
              <p class="settings-hint">ローカルLLMではmodel requestを1本に保つ設定を推奨します。Agentごとのcontextと独立レビューは並列推論なしでも維持されます。</p>
            </section>
            <section id="settings-tools" class="settings-section">
              <h3>Tools</h3>
              <div class="settings-subsection">
                <div class="settings-section-head compact">
                  <div>
                    <h4>Shell</h4>
                    <p>Windows の PowerShell / taskkill helper window 表示を制御します。</p>
                  </div>
                  ${renderConfigToggleField(state, "shell.hide_windows", "PowerShell window を隠す")}
                </div>
              </div>
              <div class="settings-subsection">
                <div class="settings-section-head compact">
                  <div>
                    <h4>Docling</h4>
                    <p>PDF / DOCX などの構造化 document 変換に使います。無効時は agent tool surface から外れます。</p>
                  </div>
                  ${renderConfigToggleField(state, "docling.enabled", "有効")}
                </div>
                <div class="settings-grid-two">
                  ${renderConfigTextField(state, "docling.base_url", "Docling base URL", "url")}
                  ${renderConfigTextField(state, "docling.timeout_ms", "Timeout ms", "number")}
                  ${renderConfigTextField(state, "docling.api_key_env", "API key env")}
                </div>
                ${renderConfigJsonField(state, "docling.headers_json", "Headers JSON")}
              </div>
              <div class="settings-subsection">
                <div class="settings-section-head compact">
                  <div>
                    <h4>MCP</h4>
                    <p>明示設定した HTTP MCP server だけを使います。</p>
                  </div>
                  ${renderConfigToggleField(state, "mcp.enabled", "有効")}
                </div>
                ${renderConfigJsonField(state, "mcp.servers_json", "MCP servers JSON")}
              </div>
            </section>
            <section id="settings-files" class="settings-section">
              <h3>Files</h3>
              <div class="settings-grid-two">
                ${renderConfigTextField(state, "inspection.default_max_depth", "Inspection depth", "number")}
                ${renderConfigTextField(state, "inspection.default_max_entries_per_dir", "Entries per dir", "number")}
                ${renderConfigTextField(state, "inspection.max_extensions_reported", "Extensions reported", "number")}
                ${renderConfigTextField(state, "file_guard.max_inline_read_bytes", "Max inline read bytes", "number")}
                ${renderConfigTextField(state, "file_guard.large_file_warning_bytes", "Large file warning bytes", "number")}
                ${renderConfigTextField(state, "file_guard.blocked_read_extensions", "Blocked extensions")}
                ${renderConfigTextField(state, "file_guard.structured_document_extensions", "Structured document extensions")}
              </div>
              ${renderConfigToggleField(state, "inspection.include_hidden_by_default", "Hidden files を inspection に含める")}
            </section>
            <section id="settings-advanced" class="settings-section">
              <details data-details-key="settings-advanced-fields">
                <summary>Advanced raw fields</summary>
                <div class="settings-raw-grid">
                  ${state.config_fields
                    .map((field, index) => ({ field, index }))
                    .filter(({ field }) => !TYPED_CONFIG_KEYS.has(field.key))
                    .map(({ field, index }) => renderRawConfigField(field, index))
                    .join("")}
                </div>
              </details>
            </section>
          </div>
        </div>
      </section>
    </div>
  `;
}

function configField(state: DesktopWebState, key: string): { field: ConfigFieldProjection; index: number } | null {
  const index = state.config_fields.findIndex((field) => field.key === key);
  if (index < 0) return null;
  return { field: state.config_fields[index], index };
}

function renderMissingConfigField(key: string): string {
  return `<div class="settings-field missing"><label>${escapeHtml(key)}</label><small>未対応の設定項目です。</small></div>`;
}

function renderConfigTextField(state: DesktopWebState, key: string, label: string, type = "text"): string {
  const found = configField(state, key);
  if (!found) return renderMissingConfigField(key);
  const inputMode = type === "number" ? ' inputmode="numeric"' : "";
  return `
    <label class="settings-field">
      <span>${escapeHtml(label)}${renderEnvBadge(found.field)}</span>
      <input class="settings-control" data-config-index="${found.index}" data-config-key="${escapeHtml(key)}" type="${type === "number" ? "text" : type}"${inputMode} value="${escapeHtml(found.field.value)}" ${configDraftEditingIsOpen ? "" : "disabled"} />
    </label>
  `;
}

function renderConfigJsonField(state: DesktopWebState, key: string, label: string): string {
  const found = configField(state, key);
  if (!found) return renderMissingConfigField(key);
  return `
    <label class="settings-field wide">
      <span>${escapeHtml(label)}${renderEnvBadge(found.field)}</span>
      <textarea class="settings-control settings-json" data-config-index="${found.index}" data-config-key="${escapeHtml(key)}" ${configDraftEditingIsOpen ? "" : "disabled"}>${escapeHtml(found.field.value)}</textarea>
    </label>
  `;
}

function renderConfigToggleField(state: DesktopWebState, key: string, label: string): string {
  const found = configField(state, key);
  if (!found) return renderMissingConfigField(key);
  const checked = found.field.value.trim().toLowerCase() === "true" ? "checked" : "";
  return `
    <label class="settings-toggle">
      <input class="settings-control" data-config-index="${found.index}" data-config-key="${escapeHtml(key)}" type="checkbox" ${checked} ${configDraftEditingIsOpen ? "" : "disabled"} />
      <span class="toggle-ui"></span>
      <span>${escapeHtml(label)}${renderEnvBadge(found.field)}</span>
    </label>
  `;
}

function renderConfigEnumField(
  state: DesktopWebState,
  key: string,
  label: string,
  optionLabels: Record<string, string>,
): string {
  const found = configField(state, key);
  if (!found) return renderMissingConfigField(key);
  const options = found.field.options.length > 0 ? found.field.options : [found.field.value];
  return `
    <label class="settings-field wide">
      <span>${escapeHtml(label)}${renderEnvBadge(found.field)}</span>
      <select class="settings-control" data-config-index="${found.index}" data-config-key="${escapeHtml(key)}" ${configDraftEditingIsOpen ? "" : "disabled"}>
        ${options.map((value) => `<option value="${escapeHtml(value)}" ${found.field.value === value ? "selected" : ""}>${escapeHtml(optionLabels[value] ?? value)}</option>`).join("")}
      </select>
    </label>
  `;
}

function renderRawConfigField(field: ConfigFieldProjection, index: number): string {
  return `
    <label class="settings-field raw">
      <span>${escapeHtml(field.key)}${renderEnvBadge(field)}</span>
      <textarea class="settings-control settings-raw-value" data-config-index="${index}" data-config-key="${escapeHtml(field.key)}" ${configDraftEditingIsOpen ? "" : "disabled"}>${escapeHtml(field.value)}</textarea>
    </label>
  `;
}

function renderEnvBadge(field: ConfigFieldProjection): string {
  if (!field.env_override) return "";
  return ` <small class="env-badge">${escapeHtml(field.env_override)}</small>`;
}

function startupSetupRequired(state: DesktopWebState): boolean {
  return state.startup.initial_setup_required && state.startup.action_overlay === state.overlay;
}

function renderWorkspaceOverlay(state: DesktopWebState): string {
  return `
    <div class="modal-backdrop" data-action="close-overlay">
      <section class="modal wide" data-modal role="dialog" aria-modal="true" aria-labelledby="workspace-dialog-title" tabindex="-1">
        <h2 id="workspace-dialog-title">ワークスペース</h2>
        <label class="field-label">パス</label>
        <input id="workspace-input" value="${escapeHtml(state.workspace_input)}" />
        <div class="split-actions">
          <button data-action="switch-workspace">切り替え</button>
          <button data-action="browse-workspace">参照</button>
          <button data-action="open-typed-path">入力パスを開く</button>
          <button data-action="open-workspace-folder">現在の場所を開く</button>
        </div>
      </section>
    </div>
  `;
}

function renderPromptReviewOverlay(state: DesktopWebState): string {
  return `
    <div class="modal-backdrop" data-action="close-overlay">
      <section class="modal wide" data-modal role="dialog" aria-modal="true" aria-labelledby="prompt-review-dialog-title" tabindex="-1">
        <h2 id="prompt-review-dialog-title">Enhance</h2>
        <div class="review-grid">
          <pre>${escapeHtml(state.review_raw_text)}</pre>
          <textarea id="review-draft">${escapeHtml(state.review_draft_text)}</textarea>
        </div>
        <pre class="feedback">${escapeHtml(state.review_status_text)}</pre>
        <div class="modal-actions">
          <button data-action="cancel-review">キャンセル</button>
          <button data-action="send-review-raw" ${state.send_raw_enabled ? "" : "disabled"}>原文で送信</button>
          <button class="send wide-send" data-action="send-review-enhanced" ${state.send_enhanced_enabled ? "" : "disabled"}>推敲文で送信</button>
        </div>
      </section>
    </div>
  `;
}

function renderCommandPalette(state: DesktopViewState): string {
  const actions = paletteActions(state, configDirty && !configMutationInFlight);
  return `
    <div class="modal-backdrop" data-action="close-overlay">
      <section class="modal command" data-modal role="dialog" aria-modal="true" aria-labelledby="command-palette-dialog-title" tabindex="-1">
        <h2 id="command-palette-dialog-title">コマンドパレット</h2>
        <input id="local-search" value="${escapeHtml(state.local_search_text)}" placeholder="アクション、セッション、/コマンドを検索" />
        <pre class="feedback">${escapeHtml(state.local_search_results_text)}</pre>
        <div class="select-list compact">
          ${
            actions.length === 0
              ? '<div class="empty">実行できるアクションはありません</div>'
              : actions
                  .map(
                    (action) => `
                      <button data-action="${escapeHtml(action.id)}" data-focus-key="palette-action:${escapeHtml(action.id)}">
                        <span>${escapeHtml(action.label)}</span>${action.shortcut ? `<small>${escapeHtml(action.shortcut)}</small>` : ""}
                      </button>`
                  )
                  .join("")
          }
          ${state.command_rows
            .map(
              (row, index) => `
                <button data-action="insert-command" data-index="${index}" data-focus-key="palette-command:${escapeHtml(row.path)}">
                  <span>/${escapeHtml(row.name)}</span><small>${escapeHtml(row.path)}</small>
                </button>`
            )
            .join("")}
        </div>
      </section>
    </div>
  `;
}

function renderShortcuts(): string {
  const rows: Array<[string, string]> = [["close-overlay", "Esc  閉じる"]];
  rows.push(...shortcutActions().map((action) => [action.id, `${action.shortcut ?? ""}  ${action.label}`] as [string, string]));
  return renderMenuOverlay("ショートカット", rows);
}

function renderMenuPopover(menu: ActionMenu, items: ActionDefinition[], extra = ""): string {
  return `
    <div class="menu-scrim" data-action="close-overlay">
      <section class="titlebar-popover ${menu}" data-modal role="menu">
        ${items
          .map(
            (action) => `
              <button data-action="${escapeHtml(action.id)}" role="menuitem">
                <span>${escapeHtml(action.label)}</span>
                ${action.shortcut ? `<small>${escapeHtml(action.shortcut)}</small>` : ""}
              </button>`
          )
          .join("")}
        ${extra}
      </section>
    </div>
  `;
}

function renderMenuOverlay(title: string, items: Array<[string, string]>): string {
  return `
    <div class="modal-backdrop" data-action="close-overlay">
      <section class="modal side" data-modal role="dialog" aria-modal="true" aria-labelledby="shortcuts-dialog-title" tabindex="-1">
        <h2 id="shortcuts-dialog-title">${escapeHtml(title)}</h2>
        <div class="select-list">
          ${items.map(([action, label]) => `<button data-action="${action}">${escapeHtml(label)}</button>`).join("")}
        </div>
      </section>
    </div>
  `;
}

function renderNavRow(
  label: string,
  detail: string,
  selected: boolean,
  kind: string,
  index: number,
  rejoinAction: string,
  secondaryAction: string,
  rollbackAction: string,
  deleteAction: string,
  running = false,
  mutationDisabled = false,
  focusKey = `${kind}:${index}`,
): string {
  const actionClass = `${rejoinAction ? "has-rejoin" : ""} ${secondaryAction ? "has-archive" : ""} ${rollbackAction ? "has-rollback" : ""}`.trim();
  const rejoinLabel = actionLabel(rejoinAction, "実行中セッションに再参加");
  const secondaryLabel = actionLabel(
    secondaryAction,
    secondaryAction === "interrupt-session"
      ? "実行中セッションを interrupt"
      : secondaryAction === "unarchive-session"
        ? "復元"
        : "アーカイブ",
  );
  const secondaryIcon = secondaryAction === "interrupt-session" ? "square" : "archive";
  const rollbackLabel = actionLabel(rollbackAction, "最新turnを戻す");
  const deleteLabel = actionLabel(deleteAction, "削除");
  const disabled = mutationDisabled ? ' disabled aria-disabled="true"' : "";
  return `
    <div class="nav-row-wrap ${actionClass} ${selected ? "selected" : ""}">
      <button class="nav-row" data-action="${kind}" data-index="${index}" data-focus-key="${escapeHtml(focusKey)}:select"${selected ? ' aria-current="page"' : ""}${disabled}>
        <span class="nav-title">${running ? '<span class="busy-spinner" title="実行中"></span>' : ""}<span>${escapeHtml(label)}</span></span>
        <small>${escapeHtml(detail)}</small>
      </button>
      ${
        rejoinAction
          ? `<button class="row-action row-rejoin" data-action="${rejoinAction}" data-index="${index}" data-focus-key="${escapeHtml(focusKey)}:${escapeHtml(rejoinAction)}" title="${escapeHtml(rejoinLabel)}" aria-label="${escapeHtml(rejoinLabel)}"${disabled}>${icon("refresh")}</button>`
          : ""
      }
      ${
        secondaryAction
          ? `<button class="row-action ${secondaryAction === "interrupt-session" ? "row-interrupt" : "row-archive"}" data-action="${secondaryAction}" data-index="${index}" data-focus-key="${escapeHtml(focusKey)}:${escapeHtml(secondaryAction)}" title="${escapeHtml(secondaryLabel)}" aria-label="${escapeHtml(secondaryLabel)}"${disabled}>${icon(secondaryIcon)}</button>`
          : ""
      }
      ${
        rollbackAction
          ? `<button class="row-action row-rollback" data-action="${rollbackAction}" data-index="${index}" data-focus-key="${escapeHtml(focusKey)}:${escapeHtml(rollbackAction)}" title="${escapeHtml(rollbackLabel)}" aria-label="${escapeHtml(rollbackLabel)}"${disabled}>${icon("undo")}</button>`
          : ""
      }
      ${deleteAction ? `<button class="row-delete" data-action="${deleteAction}" data-index="${index}" data-focus-key="${escapeHtml(focusKey)}:${escapeHtml(deleteAction)}" title="${escapeHtml(deleteLabel)}" aria-label="${escapeHtml(deleteLabel)}"${disabled}>${icon("x")}</button>` : ""}
    </div>
  `;
}

function actionLabel(actionId: string, fallback: string): string {
  return actionId ? (actionById(actionId)?.label ?? fallback) : fallback;
}



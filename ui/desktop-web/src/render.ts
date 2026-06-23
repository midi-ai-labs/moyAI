import { convertFileSrc } from "@tauri-apps/api/core";
import { actionById, menuActions, paletteActions, shortcutActions, type ActionDefinition, type ActionMenu } from "./actions";
import { icon } from "./icons";
import { renderMarkdown } from "./markdown";
import type { DesktopWebState, FileChangeRow, ProjectRow, SessionRow, TranscriptRow } from "./types";
import { displayAccessLabel, escapeHtml, fileName, goalSlashCommandHint, humanizeError, shortenPath, validateConfigInput } from "./utils";

export type { LocalConfirmation } from "./render_overlays";
export { renderConfirmation, renderLocalConfirmation } from "./render_overlays";

const splashLogoUrl = new URL("../../../logo/fabicon/android-chrome-512x512.png", import.meta.url).href;

interface RenderContext {
  artifactPaneCollapsed: boolean;
  attachmentTrayOpen: boolean;
  configFilterText: string;
  configDirty: boolean;
}

let artifactPaneCollapsed = false;
let attachmentTrayOpen = false;
let configFilterText = "";
let configDirty = false;

export function setRenderContext(context: RenderContext): void {
  artifactPaneCollapsed = context.artifactPaneCollapsed;
  attachmentTrayOpen = context.attachmentTrayOpen;
  configFilterText = context.configFilterText;
  configDirty = context.configDirty;
}

export function renderStartupSplash(state: DesktopWebState, elapsedMs: number, minVisibleMs: number): string {
  const remainingMs = Math.max(0, minVisibleMs - elapsedMs);
  const progressLabel =
    state.startup.status === "loading"
      ? "確認中"
      : remainingMs > 0
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

export function renderTitlebar(): string {
  return `
    <header class="app-titlebar" data-drag-region data-tauri-drag-region>
      <div class="titlebar-left" data-drag-region data-tauri-drag-region>
        <span class="app-brand" data-drag-region data-tauri-drag-region>moyAI</span>
        <nav class="titlebar-menu">
          <button data-action="show-file-menu" aria-label="ファイルメニュー">ファイル</button>
          <button data-action="show-edit-menu" aria-label="編集メニュー">編集</button>
          <button data-action="show-view-menu" aria-label="表示メニュー">表示</button>
          <button data-action="show-help-menu" aria-label="ヘルプメニュー">ヘルプ</button>
        </nav>
      </div>
      <div class="titlebar-drag" data-drag-region data-tauri-drag-region></div>
      <div class="titlebar-controls">
        <button data-action="minimize-window" title="最小化" aria-label="最小化">−</button>
        <button data-action="toggle-maximize-window" title="最大化" aria-label="最大化">□</button>
        <button data-action="close-window" title="閉じる" aria-label="閉じる">×</button>
      </div>
    </header>
  `;
}

function renderProjectRowWithSessions(state: DesktopWebState, row: ProjectRow, index: number): string {
  const selected = index === state.selected_project_index;
  const projectRow = renderProjectRow(row, selected, index);
  if (!selected) {
    return projectRow;
  }
  const sessionRows = renderProjectSessionRows(state);
  return `${projectRow}${sessionRows}`;
}

function renderProjectRow(row: ProjectRow, selected: boolean, index: number): string {
  return `
    <div class="nav-row-wrap project-row ${selected ? "selected" : ""}">
      <button class="nav-row" data-action="project" data-index="${index}">
        <span class="nav-title">${escapeHtml(row.label)}</span>
        <small>${escapeHtml(row.path)}</small>
      </button>
      <button class="row-action add-session" data-action="new-project-session" data-index="${index}" title="このプロジェクトで新しい開発チャット" aria-label="このプロジェクトで新しい開発チャット">${icon("plus")}</button>
      <button class="row-action danger" data-action="delete-project" data-index="${index}" title="削除" aria-label="削除">${icon("x")}</button>
    </div>
  `;
}

function renderProjectSessionRows(state: DesktopWebState): string {
  if (state.selected_project_index < 0) {
    return "";
  }
  const search = `
    <div class="session-search">
      <input id="session-search" value="${escapeHtml(state.session_search_text)}" placeholder="セッション検索" aria-label="セッション検索" />
      <button class="${state.session_search_include_archived ? "selected" : ""}" data-action="toggle-session-archived-search" title="アーカイブ済みを含める" aria-label="アーカイブ済みを含める">${icon("archive")}</button>
    </div>
  `;
  const archiveAction = state.session_search_include_archived ? "unarchive-session" : "archive-session";
  const rows = state.session_rows
    .map((row, index) =>
      renderNavRow(
        row.label,
        sessionRowSubtitle(row, "開発チャット"),
        index === state.selected_session_index,
        "session",
        index,
        row.loaded_status === "active" ? "rejoin-session" : "",
        archiveAction,
        row.loaded_status === "active" ? "" : "rollback-session",
        "delete-session",
        state.busy && index === state.selected_session_index
      )
    )
    .join("");
  const activeFallback = rows.length === 0 ? renderActiveProjectSessionPlaceholder(state) : "";
  return rows.length > 0 || activeFallback.length > 0 || state.session_search_text.trim().length > 0
    ? `<div class="project-session-list">${search}${rows}${activeFallback}</div>`
    : `<div class="project-session-list">${search}</div>`;
}

function renderActiveProjectSessionPlaceholder(state: DesktopWebState): string {
  if (!state.busy) {
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
        row.loaded_status === "active" ? "" : "rollback-session",
        "delete-chat-session",
        state.selected_project_index < 0 && state.busy && row.session_id === selectedChatSessionId
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
  const chatRunning = state.selected_project_index < 0 && state.busy;
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
        <button class="tiny-button icon-only" data-action="create-project-from-picker" title="プロジェクトを作成" aria-label="プロジェクトを作成">${icon("folder-plus")}</button>
      </div>
      <div class="row-list project-list">
        ${state.project_rows
          .map((row, index) => renderProjectRowWithSessions(state, row, index))
          .join("")}
      </div>
      <div class="rail-section row-heading">
        <span class="section-label">チャット${chatRunning ? '<span class="busy-spinner small" title="実行中"></span>' : ""}</span>
        <button class="tiny-button icon-only" data-action="new-chat" title="新しいチャット" aria-label="新しいチャット">${icon("edit")}</button>
      </div>
      <div class="row-list chat-list">${renderChatRows(state)}</div>
      <button class="settings" data-action="show-config" title="設定"><span class="rail-icon">${icon("settings")}</span><span>設定</span></button>
    </aside>
  `;
}

export function renderTopbar(state: DesktopWebState): string {
  const workspaceLabel = state.selected_project_index >= 0 ? shortenPath(state.workspace_path) : "プロジェクトなし";
  const projectContextAction = state.selected_project_index >= 0 ? "open-workspace-folder" : "create-project-from-picker";
  const exportDisabled = !state.history_export_enabled;
  const exportTitle = exportDisabled ? "保存できる表示中の履歴がありません" : "表示中の履歴をMarkdown保存";
  const turnPageVisible = state.turn_page_total > state.turn_page_limit && state.turn_page_limit > 0;
  const turnPageStart = state.turn_page_total === 0 ? 0 : state.turn_page_offset + 1;
  const turnPageEnd = Math.min(state.turn_page_total, state.turn_page_offset + state.turn_page_limit);
  const previousDisabled = state.turn_page_offset === 0 || state.busy;
  const nextDisabled = !state.turn_page_has_more || state.busy;
  return `
    <header class="topbar">
      <div class="title-row">
        <div class="title-copy">
          <h1>${escapeHtml(state.selected_session_title)}</h1>
          <div class="status-line ${state.status_detail.trim().length > 0 ? "has-detail" : ""}">
            <span>${escapeHtml(state.status_message)}</span>
            ${
              state.status_detail.trim().length > 0
                ? `<details>
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
          <button data-action="toggle-access" title="アクセス権限">${escapeHtml(displayAccessLabel(state.access_label))}</button>
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
  if (!state.busy && !state.confirmation_visible) {
    return "";
  }
  const phase = state.run_phase.trim() || "running";
  const step = state.run_active_step.trim() || state.status_message;
  const toolLine = state.latest_tool_summary.trim() || "ツール待機中";
  return `
    <section class="run-strip" aria-live="polite">
      <span class="busy-spinner" title="実行中"></span>
      <strong>${state.confirmation_visible ? "確認待ち" : "実行中"}</strong>
      <span>${escapeHtml(phase)}</span>
      <span>${escapeHtml(step)}</span>
      <small>${escapeHtml(toolLine)}</small>
      <button class="icon-only danger" data-action="cancel-run" title="実行停止" aria-label="実行停止">${icon("square")}</button>
    </section>
  `;
}

export function renderThreadContent(state: DesktopWebState): string {
  if ((state.thread_empty || state.selected_session_index < 0) && state.file_change_rows.length === 0) {
    return renderEmptyThread(state);
  }
  return state.transcript_rows.map(renderTranscriptCard).join("");
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
  if (row.kind.startsWith("work_summary")) {
    return renderWorkSummaryCard(row);
  }
  if (row.kind === "file_changes") {
    return renderFileChangesTranscriptCard(row);
  }
  return `
    <article class="message ${escapeHtml(row.kind)}">
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
  const running = row.kind === "work_summary_running";
  const open = running ? "open" : "";
  const statusText = row.kind === "work_summary_running" ? "実行中" : "作業サマリ";
  const summary = extractMarkdownSections(row.body, ["作業サマリ", "完了"]);
  const history = removeMarkdownSections(row.body, ["作業サマリ", "完了"]);
  const visibleSummary = !running && summary.trim().length > 0 ? summary : "";
  const detailBody = visibleSummary.length > 0 && history.trim().length > 0 ? history : row.body;
  return `
    <article class="message work-summary ${escapeHtml(row.kind)}">
      <div class="message-step">${escapeHtml(row.step)}</div>
      <div class="message-body">
        ${
          visibleSummary.length > 0
            ? `<h2>${escapeHtml(row.title)}</h2><div class="markdown-body work-summary-visible">${renderMarkdown(visibleSummary)}</div>`
            : ""
        }
        <details ${open}>
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
  const sendTitle = state.busy ? "実行中は送信できません" : state.draft_prompt.trim().length === 0 ? "依頼文を入力してください" : "送信";
  const enhanceTitle = state.busy ? "実行中はEnhanceできません" : state.draft_prompt.trim().length === 0 ? "依頼文を入力してください" : "Enhance";
  const controlsVisible = attachmentTrayOpen || state.image_input.trim().length > 0;
  const trayVisible = controlsVisible || state.attached_images.length > 0;
  const goalHint = goalSlashCommandHint(state.draft_prompt);
  return `
    <section class="composer ${goalHint ? "goal-command" : ""}">
      ${trayVisible ? renderAttachmentTray(state, controlsVisible) : ""}
      <textarea id="prompt" placeholder="moyAI に依頼する" aria-describedby="goal-command-hint">${escapeHtml(state.draft_prompt)}</textarea>
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
      </div>
    </section>
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
        <button class="thumb image-thumb" data-action="remove-image" data-index="${index}" title="${escapeHtml(path)}" aria-label="添付画像を削除: ${escapeHtml(fileName(path))}">
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

export function renderArtifactPane(state: DesktopWebState): string {
  if (artifactPaneCollapsed) {
    return `
      <aside class="artifact-pane collapsed">
        <button class="pin" data-action="toggle-artifact-pane" title="アーティファクトを表示" aria-label="アーティファクトを表示">${icon("folder")}</button>
      </aside>
    `;
  }
  const hasPreview = state.artifact_preview_available;
  const hasActivity = state.busy && (state.progress_text.trim().length > 0 || state.tool_status_text.trim().length > 0);
  return `
    <aside class="artifact-pane">
      <div class="pane-title">
        <strong>アーティファクト</strong>
        <div class="pane-actions">
          <button class="pin" data-action="toggle-artifact-pane" title="アーティファクトを折りたたむ" aria-label="アーティファクトを折りたたむ">${icon("x")}</button>
          <button class="pin" data-action="open-artifact-folder" title="アーティファクトのフォルダーを開く" aria-label="アーティファクトのフォルダーを開く">${icon("folder")}</button>
        </div>
      </div>
      <div class="artifact-list">
        ${
          state.artifact_rows.length === 0
            ? '<div class="empty artifact-empty">生成ファイル、開いたファイル、変更履歴がここに表示されます</div>'
            : state.artifact_rows
                .map(
                  (row, index) => `
                    <button class="artifact-row ${index === state.selected_artifact_index ? "selected" : ""}"
                      data-action="artifact" data-index="${index}">
                      <span class="file-icon">▣</span>
                      <span><b>${escapeHtml(row.label)}</b><small>${escapeHtml(row.path)}</small></span>
                    </button>`
                )
                .join("")
        }
      </div>
      ${
        hasPreview
          ? `<div class="preview">
              <div class="preview-tabs">
                <span>プレビュー</span>
                <button data-action="open-artifact-folder">開く</button>
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

export function renderOverlay(state: DesktopWebState): string {
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

function renderProviderOverlay(state: DesktopWebState): string {
  const selectedSummary = state.provider_selected_model_summary.length > 0 ? state.provider_selected_model_summary : ["モデル metadata は未取得です。"];
  const providerStatus = providerStatusView(state.provider_status_text);
  const providerModeOptions = [
    ["lm_studio_native_required", "LM Studio native"],
    ["openai_compatible_only", "OpenAI互換のみ"],
  ] as const;
  return `
    <div class="modal-backdrop" data-action="close-overlay">
      <section class="modal wide" data-modal>
        <h2>LLM URL</h2>
        <label class="field-label">ベースURL</label>
        <input id="provider-url" value="${escapeHtml(state.provider_base_url)}" />
        <label class="field-label">Provider mode</label>
        <div class="segmented-control provider-mode-control">
          ${providerModeOptions
            .map(
              ([mode, label]) => `
                <button class="${state.provider_metadata_mode === mode ? "selected" : ""}" data-action="set-provider-mode" data-mode="${mode}">
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
          <button data-action="load-provider-models" ${state.provider_loading ? "disabled" : ""}>${state.provider_loading ? "読込中" : "モデル読込"}</button>
          <button data-action="apply-provider-session" ${state.provider_apply_enabled ? "" : "disabled"}>UIセッションに適用</button>
          <button data-action="save-provider-global" ${state.provider_apply_enabled ? "" : "disabled"}>設定ファイルに保存</button>
        </div>
        <div class="select-list">
          ${state.provider_models
            .map(
              (model, index) => `
                <button class="${index === state.provider_selected_index ? "selected" : ""}" data-action="select-provider-model" data-index="${index}">
                  ${escapeHtml(model)}
                </button>`
            )
            .join("")}
        </div>
        <details class="provider-details">
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
              ? `<details><summary>技術詳細</summary><pre>${escapeHtml(providerStatus.details)}</pre></details>`
              : ""
          }
        </div>
      </section>
    </div>
  `;
}

function providerStatusView(message: string): { kind: string; title: string; hint: string; details: string } {
  const text = message.trim();
  if (text.length === 0) {
    return {
      kind: "idle",
      title: "Provider 設定を確認できます",
      hint: "Base URL、mode、model を選択してセッションへ適用できます。",
      details: "",
    };
  }
  const lower = text.toLowerCase();
  if (lower.includes("error") || lower.includes("failed")) {
    const error = humanizeError(text);
    return { kind: "error", title: error.title, hint: error.hint, details: error.details };
  }
  if (lower.includes("selected") || lower.includes("loaded") || lower.includes("managed request")) {
    return {
      kind: "ok",
      title: "Provider 設定を読み込みました",
      hint: "選択したモデルとBase URLをセッションまたは設定ファイルへ適用できます。",
      details: text,
    };
  }
  return {
    kind: "idle",
    title: "Provider 状態",
    hint: "詳細は必要な場合だけ展開してください。",
    details: text,
  };
}

function renderConfigOverlay(state: DesktopWebState): string {
  const normalizedFilter = configFilterText.trim().toLowerCase();
  const selectedValidation = validateConfigInput(state.config_field_title, state.config_value_text);
  return `
    <div class="modal-backdrop" data-action="close-overlay">
      <section class="modal wide" data-modal>
        <div class="modal-header">
          <h2>設定</h2>
          <button class="icon-button" data-action="close-overlay" title="閉じる" aria-label="閉じる">${icon("x")}</button>
        </div>
        <div class="config-grid">
          <div class="config-list-pane">
            <input id="config-filter" value="${escapeHtml(configFilterText)}" placeholder="設定項目を検索" aria-label="設定項目を検索" />
            <div class="select-list config-list">
              ${renderConfigItems(state, normalizedFilter)}
            </div>
          </div>
          <div class="config-editor-pane">
            <div class="config-title-row">
              <label class="field-label" for="config-value">${escapeHtml(state.config_field_title)}</label>
              <span class="dirty-badge ${configDirty ? "visible" : ""}">変更あり</span>
            </div>
            <textarea id="config-value">${escapeHtml(state.config_value_text)}</textarea>
            <div id="config-validation" class="validation ${selectedValidation.ok ? "ok" : "error"}">${escapeHtml(
              selectedValidation.message
            )}</div>
            <pre class="feedback">${escapeHtml(state.config_feedback_text)}</pre>
            <div class="split-actions config-actions">
              <button data-action="apply-session-config">このUIセッションに適用</button>
              <button data-action="save-global-config">設定ファイルに保存</button>
              <button data-action="open-global-config-folder">設定フォルダーを開く</button>
              <button data-action="close-overlay">閉じる</button>
            </div>
          </div>
        </div>
      </section>
    </div>
  `;
}

function renderConfigItems(state: DesktopWebState, normalizedFilter: string): string {
  const filtered = state.config_items
    .map((item, index) => ({ item, index, key: item.split(" = ")[0] ?? item }))
    .filter(({ item }) => normalizedFilter.length === 0 || item.toLowerCase().includes(normalizedFilter));
  if (filtered.length === 0) {
    return '<div class="empty">一致する設定項目はありません</div>';
  }
  const grouped = new Map<string, Array<{ item: string; index: number; key: string }>>();
  for (const entry of filtered) {
    const group = configGroupLabel(entry.key);
    const rows = grouped.get(group) ?? [];
    rows.push(entry);
    grouped.set(group, rows);
  }
  return Array.from(grouped.entries())
    .map(
      ([group, rows]) => `
        <div class="config-group-label">${escapeHtml(group)}</div>
        ${rows
          .map(({ item, index }) => {
            const [key, value = ""] = item.split(" = ");
            return `
              <button class="${index === state.selected_config_index ? "selected" : ""}" data-action="select-config" data-index="${index}" title="${escapeHtml(item)}">
                <span>${escapeHtml(key)}</span>
                <small>${escapeHtml(value)}</small>
              </button>`;
          })
          .join("")}`
    )
    .join("");
}

function configGroupLabel(key: string): string {
  const prefix = key.split(".")[0] ?? "other";
  const labels: Record<string, string> = {
    model: "Model",
    permissions: "Permissions",
    inspection: "Files",
    file_guard: "Files",
    docling: "Tools",
    mcp: "Tools",
    session: "Session",
    shell: "Tools",
    desktop: "Desktop",
    logging: "Harness",
    tool_output: "Tools",
    instructions: "Instructions",
    workspace: "Workspace",
  };
  return labels[prefix] ?? "Other";
}

function renderWorkspaceOverlay(state: DesktopWebState): string {
  return `
    <div class="modal-backdrop" data-action="close-overlay">
      <section class="modal wide" data-modal>
        <h2>ワークスペース</h2>
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
      <section class="modal wide" data-modal>
        <h2>Enhance</h2>
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

function renderCommandPalette(state: DesktopWebState): string {
  const actions = paletteActions(state);
  return `
    <div class="modal-backdrop" data-action="close-overlay">
      <section class="modal command" data-modal>
        <h2>コマンドパレット</h2>
        <input id="local-search" value="${escapeHtml(state.local_search_text)}" placeholder="アクション、セッション、/コマンドを検索" />
        <pre class="feedback">${escapeHtml(state.local_search_results_text)}</pre>
        <div class="select-list compact">
          ${
            actions.length === 0
              ? '<div class="empty">実行できるアクションはありません</div>'
              : actions
                  .map(
                    (action) => `
                      <button data-action="${escapeHtml(action.id)}">
                        <span>${escapeHtml(action.label)}</span>${action.shortcut ? `<small>${escapeHtml(action.shortcut)}</small>` : ""}
                      </button>`
                  )
                  .join("")
          }
          ${state.command_rows
            .map(
              (row, index) => `
                <button data-action="insert-command" data-index="${index}">
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
      <section class="modal side" data-modal>
        <h2>${escapeHtml(title)}</h2>
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
  archiveAction: string,
  rollbackAction: string,
  deleteAction: string,
  running = false
): string {
  const actionClass = `${rejoinAction ? "has-rejoin" : ""} ${archiveAction ? "has-archive" : ""} ${rollbackAction ? "has-rollback" : ""}`.trim();
  const rejoinLabel = actionLabel(rejoinAction, "実行中セッションに再参加");
  const archiveLabel = actionLabel(archiveAction, archiveAction === "unarchive-session" ? "復元" : "アーカイブ");
  const rollbackLabel = actionLabel(rollbackAction, "最新turnを戻す");
  const deleteLabel = actionLabel(deleteAction, "削除");
  return `
    <div class="nav-row-wrap ${actionClass} ${selected ? "selected" : ""}">
      <button class="nav-row" data-action="${kind}" data-index="${index}">
        <span class="nav-title">${running ? '<span class="busy-spinner" title="実行中"></span>' : ""}<span>${escapeHtml(label)}</span></span>
        <small>${escapeHtml(detail)}</small>
      </button>
      ${
        rejoinAction
          ? `<button class="row-action row-rejoin" data-action="${rejoinAction}" data-index="${index}" title="${escapeHtml(rejoinLabel)}" aria-label="${escapeHtml(rejoinLabel)}">${icon("refresh")}</button>`
          : ""
      }
      ${
        archiveAction
          ? `<button class="row-action row-archive" data-action="${archiveAction}" data-index="${index}" title="${escapeHtml(archiveLabel)}" aria-label="${escapeHtml(archiveLabel)}">${icon("archive")}</button>`
          : ""
      }
      ${
        rollbackAction
          ? `<button class="row-action row-rollback" data-action="${rollbackAction}" data-index="${index}" title="${escapeHtml(rollbackLabel)}" aria-label="${escapeHtml(rollbackLabel)}">${icon("undo")}</button>`
          : ""
      }
      <button class="row-delete" data-action="${deleteAction}" data-index="${index}" title="${escapeHtml(deleteLabel)}" aria-label="${escapeHtml(deleteLabel)}">${icon("x")}</button>
    </div>
  `;
}

function actionLabel(actionId: string, fallback: string): string {
  return actionId ? (actionById(actionId)?.label ?? fallback) : fallback;
}



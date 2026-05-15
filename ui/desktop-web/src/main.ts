import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import "./styles.css";

type RowId = string;

interface TranscriptRow {
  kind: string;
  step: string;
  title: string;
  body: string;
}

interface ProjectRow {
  project_id: RowId;
  label: string;
  path: string;
}

interface SessionRow {
  session_id: RowId;
  label: string;
}

interface ArtifactRow {
  label: string;
  path: string;
  kind: string;
  action: string;
}

interface FileChangeRow {
  label: string;
  path: string;
  action: string;
  summary: string;
}

interface DesktopWebState {
  workspace_path: string;
  provider_label: string;
  model_label: string;
  access_label: string;
  current_session_label: string;
  selected_session_title: string;
  status_message: string;
  run_status_text: string;
  progress_text: string;
  tool_status_text: string;
  confirmation_visible: boolean;
  confirmation_text: string;
  draft_prompt: string;
  image_input: string;
  attached_images: string[];
  can_submit: boolean;
  busy: boolean;
  overlay: string;
  project_rows: ProjectRow[];
  selected_project_index: number;
  session_rows: SessionRow[];
  chat_session_rows: SessionRow[];
  selected_session_index: number;
  transcript_rows: TranscriptRow[];
  artifact_rows: ArtifactRow[];
  selected_artifact_index: number;
  artifact_preview_text: string;
  file_change_rows: FileChangeRow[];
  file_change_summary_text: string;
  local_search_text: string;
  local_search_results_text: string;
  command_rows: Array<{ name: string; label: string; path: string }>;
  provider_base_url: string;
  provider_models: string[];
  provider_selected_index: number;
  provider_status_text: string;
  provider_selected_model_summary: string[];
  provider_loading: boolean;
  provider_apply_enabled: boolean;
  config_items: string[];
  selected_config_index: number;
  config_field_title: string;
  config_value_text: string;
  config_feedback_text: string;
  workspace_input: string;
  review_raw_text: string;
  review_draft_text: string;
  review_status_text: string;
  send_enhanced_enabled: boolean;
  send_raw_enabled: boolean;
  history_export_enabled: boolean;
  enhance_enabled: boolean;
  image_input_enabled: boolean;
  window_opacity_percent: number;
}

const app = document.querySelector<HTMLDivElement>("#app");
const desktopWindow = getCurrentWindow();
let currentState: DesktopWebState | null = null;
let lastRenderedState: DesktopWebState | null = null;
let polling = false;
let previousSessionKey = "";
const THREAD_END_THRESHOLD_PX = 96;
let pendingLocalConfirmation: { kind: "project" | "session" | "chat_session"; index: number; title: string; detail: string } | null = null;
let configFilterText = "";
let configDirty = false;
let lastFocusedOverlay = "none";
let artifactPaneCollapsed = window.localStorage.getItem("moyai.artifactPaneCollapsed") === "true";
let attachmentTrayOpen = false;

if (!app) {
  throw new Error("app root missing");
}
const appRoot = app;

void refresh();
window.setInterval(() => {
  if (currentState?.busy || currentState?.confirmation_visible || currentState?.provider_loading) {
    void refresh();
  }
}, 600);

document.addEventListener("keydown", (event) => {
  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
    event.preventDefault();
    void mutate("show_command_palette");
  }
  if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "n") {
    event.preventDefault();
    void mutate("new_chat");
  }
  if ((event.ctrlKey || event.metaKey) && event.key === "Enter" && currentState?.can_submit) {
    event.preventDefault();
    void mutate("submit_prompt");
  }
  if (event.key === "Escape" && currentState?.overlay !== "none") {
    event.preventDefault();
    void mutate("close_overlay");
  }
});

async function command<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  return invoke<T>(name, args ?? {});
}

async function refresh(): Promise<void> {
  if (polling) {
    return;
  }
  polling = true;
  try {
    currentState = await command<DesktopWebState>("desktop_state");
    render(currentState);
  } catch (error) {
    renderError(String(error));
  } finally {
    polling = false;
  }
}

async function mutate(name: string, args?: Record<string, unknown>): Promise<void> {
  try {
    currentState = await command<DesktopWebState>(name, args);
    render(currentState);
  } catch (error) {
    renderError(String(error));
  }
}

function render(state: DesktopWebState): void {
  const previous = lastRenderedState;
  const previousThread = document.querySelector<HTMLElement>("#thread");
  const previousThreadScrollTop = previousThread?.scrollTop ?? 0;
  const previousThreadWasNearEnd = previousThread ? isThreadNearEnd(previousThread) : true;
  const nextSessionKey = state.session_rows[state.selected_session_index]?.session_id ?? state.selected_session_title;
  const previousTranscriptCount = previous?.transcript_rows.length ?? 0;
  const previousChangeCount = previous?.file_change_rows.length ?? 0;
  const sessionChanged = nextSessionKey !== previousSessionKey;
  const contentAdvanced = state.transcript_rows.length > previousTranscriptCount || state.file_change_rows.length > previousChangeCount;
  const runCompleted = Boolean(previous?.busy && !state.busy) || state.run_status_text.includes("実行完了") || state.selected_session_title.includes("[完了]");
  const shouldRevealEnd = sessionChanged || (previousThreadWasNearEnd && (state.busy || contentAdvanced || runCompleted));
  const hasArtifactContext = state.artifact_rows.length > 0 || state.file_change_rows.length > 0 || state.busy;
  if (hasArtifactContext && artifactPaneCollapsed) {
    artifactPaneCollapsed = false;
    window.localStorage.setItem("moyai.artifactPaneCollapsed", "false");
  }
  appRoot.innerHTML = `
    <div class="app-frame ${artifactPaneCollapsed ? "artifact-collapsed" : ""}" style="--window-opacity: ${state.window_opacity_percent / 100}">
      ${renderTitlebar()}
      <div class="shell">
        ${renderSidebar(state)}
        <main class="conversation">
          ${renderTopbar(state)}
          ${renderRunStatusStrip(state)}
          <section class="thread" id="thread">
            ${renderThreadContent(state)}
          </section>
          ${renderComposer(state)}
        </main>
        ${renderArtifactPane(state)}
      </div>
    </div>
    ${pendingLocalConfirmation ? renderLocalConfirmation(pendingLocalConfirmation) : ""}
    ${state.confirmation_visible ? renderConfirmation(state) : ""}
    ${state.overlay !== "none" ? renderOverlay(state) : ""}
  `;
  const thread = document.querySelector<HTMLElement>("#thread");
  if (thread && shouldRevealEnd) {
    revealThreadEnd(thread);
  } else if (thread && previousThread) {
    restoreThreadPosition(thread, previousThreadScrollTop);
  }
  previousSessionKey = nextSessionKey;
  lastRenderedState = state;
  wire(state);
}

function isThreadNearEnd(thread: HTMLElement): boolean {
  return thread.scrollHeight - thread.scrollTop - thread.clientHeight <= THREAD_END_THRESHOLD_PX;
}

function revealThreadEnd(thread: HTMLElement): void {
  const scroll = () => {
    thread.scrollTop = thread.scrollHeight;
  };
  requestAnimationFrame(scroll);
  window.setTimeout(scroll, 50);
}

function restoreThreadPosition(thread: HTMLElement, scrollTop: number): void {
  const scroll = () => {
    thread.scrollTop = Math.min(scrollTop, Math.max(0, thread.scrollHeight - thread.clientHeight));
  };
  requestAnimationFrame(scroll);
}

function renderTitlebar(): string {
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
  const rows = state.session_rows
    .map((row, index) =>
      renderNavRow(
        row.label,
        "開発チャット",
        index === state.selected_session_index,
        "session",
        index,
        "delete-session",
        state.busy && index === state.selected_session_index
      )
    )
    .join("");
  const activeFallback = rows.length === 0 ? renderActiveProjectSessionPlaceholder(state) : "";
  return rows.length > 0 || activeFallback.length > 0 ? `<div class="project-session-list">${rows}${activeFallback}</div>` : "";
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
        "通常チャット",
        row.session_id === selectedChatSessionId,
        "chat-session",
        index,
        "delete-chat-session",
        state.selected_project_index < 0 && state.busy && row.session_id === selectedChatSessionId
      )
    )
    .join("");
}

function renderSidebar(state: DesktopWebState): string {
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
      <div class="row-list">
        ${state.project_rows
          .map((row, index) => renderProjectRowWithSessions(state, row, index))
          .join("")}
      </div>
      <div class="rail-section row-heading">
        <span class="section-label">チャット${chatRunning ? '<span class="busy-spinner small" title="実行中"></span>' : ""}</span>
        <button class="tiny-button icon-only" data-action="new-chat" title="新しいチャット" aria-label="新しいチャット">${icon("edit")}</button>
      </div>
      <div class="row-list">${renderChatRows(state)}</div>
      <button class="settings" data-action="show-config" title="設定"><span class="rail-icon">${icon("settings")}</span><span>設定</span></button>
    </aside>
  `;
}

function renderTopbar(state: DesktopWebState): string {
  const workspaceLabel = state.selected_project_index >= 0 ? shortenPath(state.workspace_path) : "プロジェクトなし";
  const projectContextAction = state.selected_project_index >= 0 ? "open-workspace-folder" : "create-project-from-picker";
  const exportDisabled = !state.history_export_enabled;
  const exportTitle = exportDisabled ? "保存できる表示中の履歴がありません" : "表示中の履歴をMarkdown保存";
  return `
    <header class="topbar">
      <div class="title-row">
        <div>
          <h1>${escapeHtml(state.selected_session_title)}</h1>
          <p>${escapeHtml(state.status_message)}</p>
        </div>
        <div class="chips">
          <button data-action="${projectContextAction}" title="${escapeHtml(state.workspace_path)}">${escapeHtml(workspaceLabel)}</button>
          <button data-action="show-provider" title="${escapeHtml(state.provider_label)}">${escapeHtml(state.model_label)}</button>
          <button data-action="toggle-access" title="アクセス権限">${escapeHtml(displayAccessLabel(state.access_label))}</button>
          <button class="icon-button" data-action="export-transcript" title="${exportTitle}" aria-label="${exportTitle}" ${exportDisabled ? "disabled" : ""}>${icon("download")}</button>
        </div>
      </div>
    </header>
  `;
}

function renderRunStatusStrip(state: DesktopWebState): string {
  if (!state.busy && !state.confirmation_visible) {
    return "";
  }
  const phase = lineValue(state.progress_text, "フェーズ") || lineValue(state.run_status_text, "フェーズ") || "running";
  const step = lineValue(state.progress_text, "手順") || lineValue(state.run_status_text, "状態") || state.status_message;
  const toolLine = state.tool_status_text.trim().split("\n").find((line) => line.trim().length > 0) ?? "ツール待機中";
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

function renderThreadContent(state: DesktopWebState): string {
  const onlyEmptyPlaceholder =
    state.transcript_rows.length === 1 &&
    state.transcript_rows[0]?.title.includes("チャット") &&
    state.transcript_rows[0]?.body.includes("下の入力欄");
  if ((state.transcript_rows.length === 0 || onlyEmptyPlaceholder || state.selected_session_index < 0) && state.file_change_rows.length === 0) {
    return renderEmptyThread(state);
  }
  return `${state.transcript_rows.map(renderTranscriptCard).join("")}${renderChangeCard(state)}`;
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

function renderWorkSummaryCard(row: TranscriptRow): string {
  const open = row.kind === "work_summary_running" ? "open" : "";
  const statusText = row.kind === "work_summary_running" ? "実行中" : "作業サマリ";
  return `
    <article class="message work-summary ${escapeHtml(row.kind)}">
      <div class="message-step">${escapeHtml(row.step)}</div>
      <div class="message-body">
        <details ${open}>
          <summary>
            <span>${escapeHtml(row.title)}</span>
            <small>${escapeHtml(statusText)}</small>
          </summary>
          <div class="markdown-body">${renderMarkdown(row.body)}</div>
        </details>
      </div>
    </article>
  `;
}

function renderChangeCard(state: DesktopWebState): string {
  if (state.file_change_rows.length === 0) {
    return "";
  }
  return `
    <article class="change-card">
      <div class="change-header">
        <strong>${escapeHtml(state.file_change_summary_text.split("\n")[0] ?? "ファイル変更")}</strong>
      </div>
      ${state.file_change_rows
        .map(
          (row) => `
            <div class="change-row">
              <span class="change-action">${escapeHtml(row.action)}</span>
              <span>${escapeHtml(row.path)}</span>
              <small>${escapeHtml(row.summary)}</small>
            </div>`
        )
        .join("")}
    </article>
  `;
}

function renderComposer(state: DesktopWebState): string {
  const projectContextAction = state.selected_project_index >= 0 ? "open-workspace-folder" : "create-project-from-picker";
  const sendTitle = state.busy ? "実行中は送信できません" : state.draft_prompt.trim().length === 0 ? "依頼文を入力してください" : "送信";
  const enhanceTitle = state.busy ? "実行中はEnhanceできません" : state.draft_prompt.trim().length === 0 ? "依頼文を入力してください" : "Enhance";
  const trayVisible = attachmentTrayOpen || state.attached_images.length > 0 || state.image_input.trim().length > 0;
  return `
    <section class="composer">
      ${trayVisible ? renderAttachmentTray(state) : ""}
      <textarea id="prompt" placeholder="moyAI に依頼する">${escapeHtml(state.draft_prompt)}</textarea>
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

function renderAttachmentTray(state: DesktopWebState): string {
  return `
    <div class="attachment-tray">
      <div class="attachment-row">
        ${state.attached_images
          .map(
            (path, index) => `
              <button class="thumb" data-action="remove-image" data-index="${index}" title="${escapeHtml(path)}">
                <span>${escapeHtml(fileName(path))}</span><b>×</b>
              </button>`
          )
          .join("") || '<span class="attachment-empty">画像は未添付です</span>'}
      </div>
      <div class="attachment-controls">
        <input id="image-input" value="${escapeHtml(state.image_input)}" placeholder="画像ファイルのパス" ${state.image_input_enabled ? "" : "disabled"} />
        <button class="icon-only" data-action="set-image" title="画像を添付" aria-label="画像を添付" ${state.image_input_enabled ? "" : "disabled"}>${icon("upload")}</button>
        <button class="icon-only" data-action="browse-image" title="画像を参照" aria-label="画像を参照" ${state.image_input_enabled ? "" : "disabled"}>${icon("folder")}</button>
        <button class="icon-only" data-action="clear-images" title="添付を解除" aria-label="添付を解除" ${state.attached_images.length > 0 ? "" : "disabled"}>${icon("x")}</button>
      </div>
    </div>
  `;
}

function renderArtifactPane(state: DesktopWebState): string {
  if (artifactPaneCollapsed) {
    return `
      <aside class="artifact-pane collapsed">
        <button class="pin" data-action="toggle-artifact-pane" title="アーティファクトを表示" aria-label="アーティファクトを表示">${icon("folder")}</button>
      </aside>
    `;
  }
  const previewText = state.artifact_preview_text.trim();
  const hasPreview = previewText.length > 0 && !previewText.includes("選択されていません");
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

function renderOverlay(state: DesktopWebState): string {
  if (state.overlay === "provider") return renderProviderOverlay(state);
  if (state.overlay === "config") return renderConfigOverlay(state);
  if (state.overlay === "workspace") return renderWorkspaceOverlay(state);
  if (state.overlay === "prompt_review") return renderPromptReviewOverlay(state);
  if (state.overlay === "command_palette") return renderCommandPalette(state);
  if (state.overlay === "shortcuts") return renderShortcuts();
  if (state.overlay === "project_menu") return "";
  if (state.overlay === "file_menu") return renderMenuPopover("file", [
    ["new-chat", "新しいチャット", "Ctrl+N"],
    ["create-project-from-picker", "プロジェクトを追加...", ""],
    ["open-workspace-folder", "現在のフォルダーを開く", ""],
  ]);
  if (state.overlay === "edit_menu") return renderMenuPopover("edit", [
    ["show-command-palette", "検索 / コマンド", "Ctrl+K"],
    ["enhance-prompt", "Enhance", ""],
  ]);
  if (state.overlay === "view_menu") {
    return renderMenuPopover(
      "view",
      [["refresh", "更新", ""], ["show-provider", "LLM URL", ""], ["show-config", "設定", ""]],
      `
        <div class="menu-slider" data-modal>
          <label class="field-label">ウィンドウ透過率</label>
          <input id="opacity-input" type="range" min="70" max="100" value="${state.window_opacity_percent}" />
        </div>
      `
    );
  }
  if (state.overlay === "help_menu") return renderMenuPopover("help", [["show-shortcuts", "ショートカット", ""]]);
  return "";
}

function renderProviderOverlay(state: DesktopWebState): string {
  const selectedSummary = state.provider_selected_model_summary.length > 0 ? state.provider_selected_model_summary : ["モデル metadata は未取得です。"];
  return `
    <div class="modal-backdrop" data-action="close-overlay">
      <section class="modal wide" data-modal>
        <h2>LLM URL</h2>
        <label class="field-label">ベースURL</label>
        <input id="provider-url" value="${escapeHtml(state.provider_base_url)}" />
        <div class="split-actions">
          <button data-action="load-provider-models" ${state.provider_loading ? "disabled" : ""}>${state.provider_loading ? "読込中" : "モデル読込"}</button>
          <button data-action="apply-provider-session" ${state.provider_apply_enabled ? "" : "disabled"}>セッションに適用</button>
          <button data-action="save-provider-project" ${state.provider_apply_enabled ? "" : "disabled"}>プロジェクトに保存</button>
          <button data-action="save-provider-global" ${state.provider_apply_enabled ? "" : "disabled"}>全体に保存</button>
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
        <div class="provider-summary">
          ${selectedSummary
            .map((line) => {
              const [label, ...rest] = line.split(": ");
              return `<div><span>${escapeHtml(label)}</span><strong>${escapeHtml(rest.join(": ") || line)}</strong></div>`;
            })
            .join("")}
        </div>
        <pre class="feedback">${escapeHtml(state.provider_status_text)}</pre>
      </section>
    </div>
  `;
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
              <button data-action="apply-session-config">このセッションに適用</button>
              <button data-action="save-project-config">プロジェクトに保存</button>
              <button data-action="save-global-config">全体に保存</button>
              <button data-action="open-project-config-folder">プロジェクト設定を開く</button>
              <button data-action="open-global-config-folder">全体設定を開く</button>
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
  return `
    <div class="modal-backdrop" data-action="close-overlay">
      <section class="modal command" data-modal>
        <h2>検索 / コマンド</h2>
        <input id="local-search" value="${escapeHtml(state.local_search_text)}" placeholder="ローカル状態を検索" />
        <pre class="feedback">${escapeHtml(state.local_search_results_text)}</pre>
        <div class="select-list compact">
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
  return renderMenuOverlay("ショートカット", [
    ["close-overlay", "Esc  閉じる"],
    ["show-command-palette", "Ctrl+K  検索 / コマンド"],
    ["new-chat", "Ctrl+N  新しいチャット"],
    ["send", "Ctrl+Enter  送信"],
  ]);
}

function renderMenuPopover(
  menu: "file" | "edit" | "view" | "help",
  items: Array<[string, string, string]>,
  extra = ""
): string {
  return `
    <div class="menu-scrim" data-action="close-overlay">
      <section class="titlebar-popover ${menu}" data-modal role="menu">
        ${items
          .map(
            ([action, label, shortcut]) => `
              <button data-action="${action}" role="menuitem">
                <span>${escapeHtml(label)}</span>
                ${shortcut ? `<small>${escapeHtml(shortcut)}</small>` : ""}
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

function renderConfirmation(state: DesktopWebState): string {
  const parts = parsePermissionText(state.confirmation_text);
  return `
    <div class="modal-backdrop">
      <section class="modal confirmation" role="alertdialog" aria-modal="true">
        <h2>確認が必要です</h2>
        <div class="confirm-summary">${escapeHtml(parts.summary)}</div>
        <dl class="confirm-details">
          <dt>対象</dt><dd>${escapeHtml(parts.targets)}</dd>
          <dt>ワークスペース外</dt><dd>${escapeHtml(parts.outside)}</dd>
          <dt>リスク</dt><dd>${escapeHtml(parts.risks)}</dd>
        </dl>
        <div class="modal-actions">
          <button data-action="deny" autofocus>拒否</button>
          <button class="send wide-send" data-action="allow">許可</button>
        </div>
      </section>
    </div>
  `;
}

function renderLocalConfirmation(confirm: { kind: "project" | "session" | "chat_session"; index: number; title: string; detail: string }): string {
  const target = confirm.kind === "project" ? "プロジェクト" : "チャット";
  const consequence =
    confirm.kind === "project"
      ? "履歴とセッション情報を削除します。ワークスペース内の実ファイルは削除しません。"
      : "このチャット履歴を削除します。ワークスペース内の実ファイルは削除しません。";
  return `
    <div class="modal-backdrop">
      <section class="modal confirmation" role="alertdialog" aria-modal="true">
        <h2>${target}を削除しますか？</h2>
        <div class="confirm-summary">${escapeHtml(confirm.title)}</div>
        <dl class="confirm-details">
          <dt>対象</dt><dd>${escapeHtml(confirm.detail)}</dd>
          <dt>影響</dt><dd>${escapeHtml(consequence)}</dd>
        </dl>
        <div class="modal-actions">
          <button data-action="cancel-local-confirm" autofocus>キャンセル</button>
          <button class="danger-button" data-action="confirm-local-delete">削除</button>
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
  deleteAction: string,
  running = false
): string {
  return `
    <div class="nav-row-wrap ${selected ? "selected" : ""}">
      <button class="nav-row" data-action="${kind}" data-index="${index}">
        <span class="nav-title">${running ? '<span class="busy-spinner" title="実行中"></span>' : ""}<span>${escapeHtml(label)}</span></span>
        <small>${escapeHtml(detail)}</small>
      </button>
      <button class="row-delete" data-action="${deleteAction}" data-index="${index}" title="削除" aria-label="削除">${icon("x")}</button>
    </div>
  `;
}

function wire(state: DesktopWebState): void {
  document.querySelectorAll<HTMLElement>('[data-action="close-window"]').forEach((node) => {
    node.addEventListener("pointerdown", (event) => {
      if (event.button !== 0) return;
      event.preventDefault();
      event.stopPropagation();
      void command("hide_to_tray").catch(() => desktopWindow.hide());
    });
  });
  document.querySelector<HTMLTextAreaElement>("#prompt")?.addEventListener("input", (event) => {
    const text = (event.currentTarget as HTMLTextAreaElement).value;
    if (currentState) {
      currentState.draft_prompt = text;
      currentState.can_submit = text.trim().length > 0 && !currentState.busy;
      const send = document.querySelector<HTMLButtonElement>('[data-action="send"]');
      if (send) send.disabled = !currentState.can_submit;
    }
    void command<DesktopWebState>("set_prompt", { text }).catch((error) => renderError(String(error)));
  });
  document.querySelector<HTMLInputElement>("#image-input")?.addEventListener("input", (event) => {
    const text = (event.currentTarget as HTMLInputElement).value;
    if (currentState) currentState.image_input = text;
    void command<DesktopWebState>("set_image_input", { text }).catch((error) => renderError(String(error)));
  });
  document.querySelector<HTMLInputElement>("#provider-url")?.addEventListener("input", (event) => {
    void command<DesktopWebState>("set_provider_base_url", { text: (event.currentTarget as HTMLInputElement).value }).catch((error) =>
      renderError(String(error))
    );
  });
  document.querySelector<HTMLTextAreaElement>("#config-value")?.addEventListener("input", (event) => {
    const text = (event.currentTarget as HTMLTextAreaElement).value;
    configDirty = true;
    updateConfigValidation(state.config_field_title, text);
    void command<DesktopWebState>("set_config_value", { text }).catch((error) => renderError(String(error)));
  });
  document.querySelector<HTMLInputElement>("#config-filter")?.addEventListener("input", (event) => {
    configFilterText = (event.currentTarget as HTMLInputElement).value;
    render(state);
  });
  document.querySelector<HTMLInputElement>("#workspace-input")?.addEventListener("input", (event) => {
    void command<DesktopWebState>("set_workspace_input", { text: (event.currentTarget as HTMLInputElement).value }).catch((error) =>
      renderError(String(error))
    );
  });
  document.querySelector<HTMLInputElement>("#local-search")?.addEventListener("input", (event) => {
    void mutate("set_local_search", { text: (event.currentTarget as HTMLInputElement).value });
  });
  document.querySelector<HTMLTextAreaElement>("#review-draft")?.addEventListener("input", (event) => {
    void command<DesktopWebState>("set_review_draft", { text: (event.currentTarget as HTMLTextAreaElement).value }).catch((error) =>
      renderError(String(error))
    );
  });
  document.querySelector<HTMLInputElement>("#opacity-input")?.addEventListener("input", (event) => {
    void mutate("set_window_opacity", { percent: Number((event.currentTarget as HTMLInputElement).value) });
  });
  document.querySelectorAll<HTMLElement>("[data-action]").forEach((node) => {
    node.addEventListener("click", (event) => {
      if (
        (event.target as HTMLElement).closest("[data-modal]") &&
        (node.classList.contains("modal-backdrop") || node.classList.contains("menu-scrim"))
      ) {
        return;
      }
      const action = node.dataset.action ?? "";
      const index = Number(node.dataset.index ?? "-1");
      dispatchAction(action, index, state);
    });
  });
  document.querySelectorAll<HTMLElement>("[data-drag-region], [data-tauri-drag-region]").forEach((node) => {
    node.addEventListener("mousedown", (event) => {
      if (event.button !== 0 || (event.target as HTMLElement).closest("button")) return;
      void command("start_window_drag").catch(() => desktopWindow.startDragging());
    });
  });
  focusOverlayPrimary(state);
}

function dispatchAction(action: string, index: number, state: DesktopWebState): void {
  if (action === "minimize-window") void desktopWindow.minimize();
  if (action === "toggle-maximize-window") void desktopWindow.toggleMaximize();
  if (action === "close-window") void command("hide_to_tray").catch(() => desktopWindow.hide());
  if (action === "send" && state.can_submit) void mutate("submit_prompt");
  if (action === "toggle-attachment-tray") {
    attachmentTrayOpen = !attachmentTrayOpen;
    return render(state);
  }
  if (action === "toggle-artifact-pane") {
    artifactPaneCollapsed = !artifactPaneCollapsed;
    window.localStorage.setItem("moyai.artifactPaneCollapsed", String(artifactPaneCollapsed));
    return render(state);
  }
  if (action === "cancel-run" && (state.busy || state.confirmation_visible)) void mutate("cancel_run");
  if (action === "refresh") void mutate("refresh_desktop");
  if (action === "new-chat") void mutate("new_chat");
  if (action === "new-project-session") void mutate("new_project_session", { index });
  if (action === "project") void mutate("select_project", { index });
  if (action === "session") void mutate("select_session", { index });
  if (action === "chat-session") void mutate("select_chat_session", { index });
  if (action === "delete-project") return requestLocalDelete("project", index, state);
  if (action === "delete-session") return requestLocalDelete("session", index, state);
  if (action === "delete-chat-session") return requestLocalDelete("chat_session", index, state);
  if (action === "cancel-local-confirm") {
    pendingLocalConfirmation = null;
    return render(state);
  }
  if (action === "confirm-local-delete") return confirmLocalDelete();
  if (action === "artifact") void mutate("select_artifact", { index });
  if (action === "export-transcript" && state.history_export_enabled) void mutate("export_transcript_markdown");
  if (action === "export-history") void mutate("export_history_markdown");
  if (action === "set-image") void mutate("attach_image");
  if (action === "browse-image") void mutate("browse_image");
  if (action === "clear-images") void mutate("clear_images");
  if (action === "remove-image") void mutate("remove_image", { index });
  if (action === "enhance-prompt") void mutate("enhance_prompt");
  if (action === "send-review-enhanced") void mutate("send_prompt_review", { enhanced: true });
  if (action === "send-review-raw") void mutate("send_prompt_review", { enhanced: false });
  if (action === "cancel-review") void mutate("cancel_prompt_review");
  if (action === "show-file-menu") void mutate("show_file_menu");
  if (action === "show-edit-menu") void mutate("show_edit_menu");
  if (action === "show-view-menu") void mutate("show_view_menu");
  if (action === "show-help-menu") void mutate("show_help_menu");
  if (action === "create-project-from-picker") void mutate("create_project_from_picker");
  if (action === "show-provider") void mutate("show_provider_editor");
  if (action === "show-config") void mutate("show_config_editor");
  if (action === "show-command-palette") void mutate("show_command_palette");
  if (action === "show-shortcuts") void mutate("show_shortcuts");
  if (action === "close-overlay") {
    configDirty = false;
    void mutate("close_overlay");
  }
  if (action === "switch-workspace") void mutate("switch_workspace");
  if (action === "browse-workspace") void mutate("browse_workspace");
  if (action === "open-workspace-folder") void mutate("open_workspace_folder");
  if (action === "open-project-config-folder") void mutate("open_project_config_folder");
  if (action === "open-global-config-folder") void mutate("open_global_config_folder");
  if (action === "open-typed-path") void mutate("open_typed_path");
  if (action === "open-artifact-folder") void mutate("open_artifact_folder");
  if (action === "load-provider-models") void mutate("load_provider_models");
  if (action === "select-provider-model") void mutate("select_provider_model", { index });
  if (action === "apply-provider-session") void mutate("apply_provider_session");
  if (action === "save-provider-project") void mutate("save_provider_project");
  if (action === "save-provider-global") void mutate("save_provider_global");
  if (action === "select-config") {
    configDirty = false;
    void mutate("set_config_selection", { index });
  }
  if (action === "apply-session-config") submitConfigAction("apply_session_config", state);
  if (action === "save-project-config") submitConfigAction("save_project_config", state);
  if (action === "save-global-config") submitConfigAction("save_global_config", state);
  if (action === "toggle-access") void mutate("toggle_access_mode");
  if (action === "insert-command") void mutate("insert_command", { index });
  if (action === "allow") void mutate("answer_permission", { allow: true });
  if (action === "deny") void mutate("answer_permission", { allow: false });
}

function requestLocalDelete(kind: "project" | "session" | "chat_session", index: number, state: DesktopWebState): void {
  if (state.busy) {
    return;
  }
  const row =
    kind === "project" ? state.project_rows[index] : kind === "chat_session" ? state.chat_session_rows[index] : state.session_rows[index];
  if (!row) {
    return;
  }
  pendingLocalConfirmation = {
    kind,
    index,
    title: kind === "project" ? row.label : row.label,
    detail: kind === "project" ? (row as ProjectRow).path : (row as SessionRow).session_id,
  };
  render(state);
}

function confirmLocalDelete(): void {
  const pending = pendingLocalConfirmation;
  if (!pending) {
    return;
  }
  pendingLocalConfirmation = null;
  if (pending.kind === "project") {
    void mutate("delete_project", { index: pending.index });
  } else if (pending.kind === "chat_session") {
    void mutate("delete_chat_session", { index: pending.index });
  } else {
    void mutate("delete_session", { index: pending.index });
  }
}

function submitConfigAction(commandName: string, state: DesktopWebState): void {
  const value = document.querySelector<HTMLTextAreaElement>("#config-value")?.value ?? state.config_value_text;
  const result = validateConfigInput(state.config_field_title, value);
  if (!result.ok) {
    updateConfigValidation(state.config_field_title, value);
    document.querySelector<HTMLTextAreaElement>("#config-value")?.focus();
    return;
  }
  configDirty = false;
  void mutate(commandName);
}

function renderError(message: string): void {
  appRoot.innerHTML = `<div class="fatal"><h1>moyAI Desktop</h1><pre>${escapeHtml(message)}</pre></div>`;
}

function renderMarkdown(value: string): string {
  const lines = value.replace(/\r\n/g, "\n").split("\n");
  let html = "";
  let paragraph: string[] = [];
  let listItems: string[] = [];
  let orderedItems: string[] = [];
  let quoteLines: string[] = [];
  let codeLines: string[] = [];
  let inCode = false;

  const flushParagraph = () => {
    if (paragraph.length === 0) return;
    html += `<p>${renderInlineMarkdown(paragraph.join(" "))}</p>`;
    paragraph = [];
  };
  const flushList = () => {
    if (listItems.length > 0) {
      html += `<ul>${listItems.map((item) => `<li>${renderInlineMarkdown(item)}</li>`).join("")}</ul>`;
      listItems = [];
    }
    if (orderedItems.length > 0) {
      html += `<ol>${orderedItems.map((item) => `<li>${renderInlineMarkdown(item)}</li>`).join("")}</ol>`;
      orderedItems = [];
    }
  };
  const flushQuote = () => {
    if (quoteLines.length === 0) return;
    html += `<blockquote>${quoteLines.map((line) => `<p>${renderInlineMarkdown(line)}</p>`).join("")}</blockquote>`;
    quoteLines = [];
  };
  const flushTextBlocks = () => {
    flushParagraph();
    flushList();
    flushQuote();
  };

  for (const line of lines) {
    const fence = line.match(/^```/);
    if (fence) {
      if (inCode) {
        html += `<pre class="md-code"><code>${escapeHtml(codeLines.join("\n"))}</code></pre>`;
        codeLines = [];
        inCode = false;
      } else {
        flushTextBlocks();
        inCode = true;
      }
      continue;
    }
    if (inCode) {
      codeLines.push(line);
      continue;
    }
    const trimmed = line.trim();
    if (trimmed.length === 0) {
      flushTextBlocks();
      continue;
    }
    const heading = trimmed.match(/^(#{1,3})\s+(.+)$/);
    if (heading) {
      flushTextBlocks();
      const level = heading[1].length + 2;
      html += `<h${level}>${renderInlineMarkdown(heading[2])}</h${level}>`;
      continue;
    }
    const bullet = trimmed.match(/^[-*]\s+(.+)$/);
    if (bullet) {
      flushParagraph();
      flushQuote();
      listItems.push(bullet[1]);
      continue;
    }
    const ordered = trimmed.match(/^\d+[.)]\s+(.+)$/);
    if (ordered) {
      flushParagraph();
      flushQuote();
      orderedItems.push(ordered[1]);
      continue;
    }
    const quote = trimmed.match(/^>\s?(.*)$/);
    if (quote) {
      flushParagraph();
      flushList();
      quoteLines.push(quote[1]);
      continue;
    }
    flushList();
    flushQuote();
    paragraph.push(line);
  }

  if (inCode) {
    html += `<pre class="md-code"><code>${escapeHtml(codeLines.join("\n"))}</code></pre>`;
  }
  flushTextBlocks();
  return html || `<p>${escapeHtml(value)}</p>`;
}

function renderInlineMarkdown(value: string): string {
  return value
    .split(/(`[^`]*`)/g)
    .map((part) => {
      if (part.startsWith("`") && part.endsWith("`")) {
        return `<code>${escapeHtml(part.slice(1, -1))}</code>`;
      }
      return escapeHtml(part)
        .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
        .replace(/__([^_]+)__/g, "<strong>$1</strong>");
    })
    .join("");
}

function fileName(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

function icon(name: string): string {
  const paths: Record<string, string> = {
    keyboard: '<path d="M3 5.5h18v13H3z"/><path d="M6 9h.01M9 9h.01M12 9h.01M15 9h.01M18 9h.01M7 13h10M6 16h12"/>',
    refresh: '<path d="M20 6v5h-5"/><path d="M4 18v-5h5"/><path d="M19 11a7 7 0 0 0-12-4l-3 3"/><path d="M5 13a7 7 0 0 0 12 4l3-3"/>',
    plug: '<path d="M12 22v-5"/><path d="M9 8V2"/><path d="M15 8V2"/><path d="M7 8h10v4a5 5 0 0 1-10 0z"/>',
    "folder-plus": '<path d="M3 6h6l2 2h10v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/><path d="M12 12v6M9 15h6"/>',
    edit: '<path d="M12 20h9"/><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z"/>',
    settings: '<path d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7z"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1-2.8 2.8-.1-.1a1.7 1.7 0 0 0-1.9-.3 1.7 1.7 0 0 0-1 1.6V21h-4v-.1a1.7 1.7 0 0 0-1-1.6 1.7 1.7 0 0 0-1.9.3l-.1.1L4.2 17l.1-.1a1.7 1.7 0 0 0 .3-1.9 1.7 1.7 0 0 0-1.6-1H3v-4h.1a1.7 1.7 0 0 0 1.6-1 1.7 1.7 0 0 0-.3-1.9l-.1-.1L7 4.2l.1.1a1.7 1.7 0 0 0 1.9.3 1.7 1.7 0 0 0 1-1.6V3h4v.1a1.7 1.7 0 0 0 1 1.6 1.7 1.7 0 0 0 1.9-.3l.1-.1L19.8 7l-.1.1a1.7 1.7 0 0 0-.3 1.9 1.7 1.7 0 0 0 1.6 1h.1v4H21a1.7 1.7 0 0 0-1.6 1z"/>',
    download: '<path d="M12 3v12"/><path d="m7 10 5 5 5-5"/><path d="M5 21h14"/>',
    plus: '<path d="M12 5v14M5 12h14"/>',
    upload: '<path d="M12 21V9"/><path d="m7 14 5-5 5 5"/><path d="M5 3h14"/>',
    more: '<path d="M5 12h.01M12 12h.01M19 12h.01"/>',
    x: '<path d="M6 6l12 12M18 6 6 18"/>',
    sparkles: '<path d="M12 3l1.4 4.6L18 9l-4.6 1.4L12 15l-1.4-4.6L6 9l4.6-1.4z"/><path d="M19 14l.7 2.3L22 17l-2.3.7L19 20l-.7-2.3L16 17l2.3-.7z"/>',
    send: '<path d="M22 2 11 13"/><path d="m22 2-7 20-4-9-9-4z"/>',
    square: '<rect x="7" y="7" width="10" height="10" rx="1"/>',
    folder: '<path d="M3 6h6l2 2h10v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
  };
  return `<svg class="ui-icon" viewBox="0 0 24 24" aria-hidden="true">${paths[name] ?? paths.plus}</svg>`;
}

function lineValue(text: string, label: string): string {
  const prefix = `${label}:`;
  const line = text
    .split("\n")
    .map((value) => value.trim())
    .find((value) => value.startsWith(prefix));
  return line ? line.slice(prefix.length).trim() : "";
}

function parsePermissionText(text: string): { summary: string; targets: string; outside: string; risks: string } {
  const lines = text.split("\n").map((line) => line.trim()).filter(Boolean);
  return {
    summary: lines.find((line) => !line.startsWith("対象:") && !line.startsWith("ワークスペース外:") && !line.startsWith("リスク:")) ?? text,
    targets: lines.find((line) => line.startsWith("対象:"))?.replace("対象:", "").trim() || "(なし)",
    outside: lines.find((line) => line.startsWith("ワークスペース外:"))?.replace("ワークスペース外:", "").trim() || "不明",
    risks: lines.find((line) => line.startsWith("リスク:"))?.replace("リスク:", "").trim() || "なし",
  };
}

function validateConfigInput(field: string, rawValue: string): { ok: boolean; message: string } {
  const value = rawValue.trim();
  if (value.length === 0) {
    return { ok: true, message: "空欄は継承または削除として扱います。" };
  }
  if (field.endsWith("base_url")) {
    try {
      const url = new URL(value);
      if (url.protocol !== "http:" && url.protocol !== "https:") {
        return { ok: false, message: "URL は http:// または https:// で始めてください。" };
      }
    } catch {
      return { ok: false, message: "URL として解釈できません。" };
    }
  }
  if (field.endsWith("_json") || field.endsWith("servers_json")) {
    try {
      JSON.parse(value);
    } catch (error) {
      return { ok: false, message: `JSON として解釈できません: ${String(error)}` };
    }
  }
  if (field.includes("enabled") || field.includes("supports_") || field.includes("include_hidden") || field.includes("parallel_tool_calls")) {
    if (!["true", "false"].includes(value.toLowerCase())) {
      return { ok: false, message: "true または false を入力してください。" };
    }
  }
  if (
    field.includes("timeout_ms") ||
    field.includes("retries") ||
    field.includes("tokens") ||
    field.includes("context_window") ||
    field.includes("max_") ||
    field.includes("top_k") ||
    field.includes("seed")
  ) {
    if (!Number.isFinite(Number(value)) || Number(value) < 0) {
      return { ok: false, message: "0 以上の数値を入力してください。" };
    }
  }
  if (field === "permissions.access_mode" && !["default", "auto_review", "full_access"].includes(value)) {
    return { ok: false, message: "default / auto_review / full_access のいずれかを入力してください。" };
  }
  return { ok: true, message: "入力形式は問題ありません。" };
}

function updateConfigValidation(field: string, value: string): void {
  const node = document.querySelector<HTMLElement>("#config-validation");
  const result = validateConfigInput(field, value);
  if (node) {
    node.textContent = result.message;
    node.classList.toggle("ok", result.ok);
    node.classList.toggle("error", !result.ok);
  }
  document.querySelector<HTMLElement>(".dirty-badge")?.classList.toggle("visible", configDirty);
}

function focusOverlayPrimary(state: DesktopWebState): void {
  const overlayKey = pendingLocalConfirmation ? "local-confirm" : state.confirmation_visible ? "permission" : state.overlay;
  if (overlayKey === lastFocusedOverlay) {
    return;
  }
  lastFocusedOverlay = overlayKey;
  const selector =
    overlayKey === "command_palette"
      ? "#local-search"
      : overlayKey === "provider"
        ? "#provider-url"
        : overlayKey === "config"
          ? "#config-filter"
          : overlayKey === "workspace"
            ? "#workspace-input"
            : overlayKey === "prompt_review"
              ? "#review-draft"
              : overlayKey === "permission" || overlayKey === "local-confirm"
                ? ".modal-actions button"
                : "";
  if (!selector) {
    return;
  }
  requestAnimationFrame(() => {
    document.querySelector<HTMLElement>(selector)?.focus();
  });
}

function shortenPath(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts.slice(-2).join(" / ") || path;
}

function displayAccessLabel(label: string): string {
  if (label === "default") return "標準";
  if (label === "auto_review") return "自動レビュー";
  if (label === "full_access") return "フルアクセス";
  return label;
}

function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

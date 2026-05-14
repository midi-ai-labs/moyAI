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
  const nextSessionKey = state.session_rows[state.selected_session_index]?.session_id ?? state.selected_session_title;
  const previousTranscriptCount = previous?.transcript_rows.length ?? 0;
  const previousChangeCount = previous?.file_change_rows.length ?? 0;
  const shouldRevealEnd =
    nextSessionKey !== previousSessionKey ||
    state.transcript_rows.length > previousTranscriptCount ||
    state.file_change_rows.length > previousChangeCount ||
    Boolean(previous?.busy && !state.busy) ||
    state.run_status_text.includes("実行完了") ||
    state.selected_session_title.includes("[完了]");
  appRoot.innerHTML = `
    <div class="app-frame" style="--window-opacity: ${state.window_opacity_percent / 100}">
      ${renderTitlebar()}
      <div class="shell">
        ${renderSidebar(state)}
        <main class="conversation">
          ${renderTopbar(state)}
          <section class="thread" id="thread">
            ${state.transcript_rows.map(renderTranscriptCard).join("")}
            ${renderChangeCard(state)}
          </section>
          ${renderComposer(state)}
        </main>
        ${renderArtifactPane(state)}
      </div>
    </div>
    ${state.confirmation_visible ? renderConfirmation(state) : ""}
    ${state.overlay !== "none" ? renderOverlay(state) : ""}
  `;
  const thread = document.querySelector<HTMLElement>("#thread");
  if (thread && shouldRevealEnd) {
    revealThreadEnd(thread);
  }
  previousSessionKey = nextSessionKey;
  lastRenderedState = state;
  wire(state);
}

function revealThreadEnd(thread: HTMLElement): void {
  const scroll = () => {
    thread.scrollTop = thread.scrollHeight;
  };
  requestAnimationFrame(scroll);
  window.setTimeout(scroll, 50);
}

function renderTitlebar(): string {
  return `
    <header class="app-titlebar" data-drag-region>
      <div class="titlebar-left" data-drag-region>
        <span class="app-brand" data-drag-region>moyAI</span>
        <nav class="titlebar-menu">
          <button data-action="show-file-menu">ファイル</button>
          <button data-action="show-edit-menu">編集</button>
          <button data-action="show-view-menu">表示</button>
          <button data-action="show-help-menu">ヘルプ</button>
        </nav>
      </div>
      <div class="titlebar-drag" data-drag-region></div>
      <div class="titlebar-controls">
        <button data-action="minimize-window" title="最小化">−</button>
        <button data-action="toggle-maximize-window" title="最大化">□</button>
        <button data-action="close-window" title="閉じる">×</button>
      </div>
    </header>
  `;
}

function renderSidebar(state: DesktopWebState): string {
  return `
    <aside class="sidebar">
      <div class="window-actions">
        <button class="icon-button" data-action="show-shortcuts" title="ショートカット">⌘</button>
        <button class="icon-button" data-action="refresh" title="更新">↻</button>
      </div>
      <button class="rail-item" data-action="show-provider" title="LLM URL">
        <span class="rail-icon">⌘</span><span>LLM URL</span>
      </button>
      <div class="rail-section row-heading">
        <span>プロジェクト</span>
        <button class="tiny-button icon-only" data-action="create-project-from-picker" title="プロジェクトを作成">⊞</button>
      </div>
      <div class="row-list">
        ${state.project_rows
          .map((row, index) =>
            renderNavRow(row.label, row.path, index === state.selected_project_index, "project", index, "delete-project")
          )
          .join("")}
      </div>
      <div class="rail-section row-heading">
        <span>チャット</span>
        <button class="tiny-button icon-only" data-action="new-chat" title="新しいチャット">✎</button>
      </div>
      <div class="row-list">
        ${
          state.session_rows.length === 0
            ? '<div class="empty">チャットはありません</div>'
            : state.session_rows
                .map((row, index) =>
                  renderNavRow(row.label, row.session_id, index === state.selected_session_index, "session", index, "delete-session")
                )
                .join("")
        }
      </div>
      <button class="settings" data-action="show-config" title="設定"><span class="rail-icon">⚙</span><span>設定</span></button>
    </aside>
  `;
}

function renderTopbar(state: DesktopWebState): string {
  const workspaceLabel = state.selected_project_index >= 0 ? shortenPath(state.workspace_path) : "プロジェクトなし";
  const projectContextAction = state.selected_project_index >= 0 ? "open-workspace-folder" : "create-project-from-picker";
  return `
    <header class="topbar">
      <div class="title-row">
        <div>
          <h1>${escapeHtml(state.selected_session_title)}</h1>
          <p>${escapeHtml(state.status_message)}</p>
        </div>
        <div class="chips">
          <button data-action="${projectContextAction}" title="${escapeHtml(state.workspace_path)}">${escapeHtml(workspaceLabel)}</button>
          <button data-action="show-provider">${escapeHtml(state.model_label)}</button>
          <button data-action="toggle-access">${escapeHtml(displayAccessLabel(state.access_label))}</button>
          <button class="icon-button" data-action="export-transcript" title="表示中の履歴をMarkdown保存">⇩</button>
        </div>
      </div>
    </header>
  `;
}

function renderTranscriptCard(row: TranscriptRow): string {
  return `
    <article class="message ${escapeHtml(row.kind)}">
      <div class="message-step">${escapeHtml(row.step)}</div>
      <div class="message-body">
        <h2>${escapeHtml(row.title)}</h2>
        <p>${formatMultiline(row.body)}</p>
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
  return `
    <section class="composer">
      <div class="attachment-row">
        ${state.attached_images
          .map(
            (path, index) => `
              <button class="thumb" data-action="remove-image" data-index="${index}" title="${escapeHtml(path)}">
                <span>${escapeHtml(fileName(path))}</span><b>×</b>
              </button>`
          )
          .join("")}
      </div>
      <textarea id="prompt" placeholder="moyAI に依頼する">${escapeHtml(state.draft_prompt)}</textarea>
      <div class="composer-actions">
        <button class="add-button icon-only" data-action="show-command-palette" title="検索 / コマンド">＋</button>
        <input id="image-input" value="${escapeHtml(state.image_input)}" placeholder="画像パス" ${state.image_input_enabled ? "" : "disabled"} />
        <button class="icon-only" data-action="set-image" title="画像を添付" ${state.image_input_enabled ? "" : "disabled"}>↥</button>
        <button class="icon-only" data-action="browse-image" title="画像を参照" ${state.image_input_enabled ? "" : "disabled"}>…</button>
        <button class="icon-only" data-action="clear-images" title="添付を解除" ${state.attached_images.length > 0 ? "" : "disabled"}>×</button>
        <button class="icon-only" data-action="enhance-prompt" title="Enhance" ${state.enhance_enabled ? "" : "disabled"}>◇</button>
        <button class="send icon-only" data-action="send" title="送信" ${state.can_submit ? "" : "disabled"}>↑</button>
      </div>
      <div class="composer-meta">
        <button data-action="${projectContextAction}" title="${escapeHtml(state.workspace_path)}">${state.selected_project_index >= 0 ? "プロジェクトで作業" : "プロジェクトを選択"}</button>
      </div>
    </section>
  `;
}

function renderArtifactPane(state: DesktopWebState): string {
  const previewText = state.artifact_preview_text.trim();
  const hasPreview = previewText.length > 0 && !previewText.includes("選択されていません");
  const hasActivity = state.busy && (state.progress_text.trim().length > 0 || state.tool_status_text.trim().length > 0);
  return `
    <aside class="artifact-pane">
      <div class="pane-title">
        <strong>アーティファクト</strong>
        <button class="pin" data-action="open-artifact-folder" title="アーティファクトのフォルダーを開く">◆</button>
      </div>
      <div class="artifact-list">
        ${
          state.artifact_rows.length === 0
            ? '<div class="empty">アーティファクトはありません</div>'
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
  if (state.overlay === "file_menu") return renderMenuOverlay("ファイル", [
    ["open-workspace-folder", "プロジェクトフォルダーを開く"],
  ]);
  if (state.overlay === "edit_menu") return renderMenuOverlay("編集", [["show-command-palette", "検索 / コマンド"]]);
  if (state.overlay === "view_menu") {
    return `
      <div class="modal-backdrop" data-action="close-overlay">
        <section class="modal side" data-modal>
          <h2>表示</h2>
          <label class="field-label">ウィンドウ透過率</label>
          <input id="opacity-input" type="range" min="70" max="100" value="${state.window_opacity_percent}" />
          <div class="modal-actions"><button data-action="close-overlay">閉じる</button></div>
        </section>
      </div>
    `;
  }
  if (state.overlay === "help_menu") return renderMenuOverlay("ヘルプ", [["show-shortcuts", "ショートカット"]]);
  return "";
}

function renderProviderOverlay(state: DesktopWebState): string {
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
        <pre class="feedback">${escapeHtml(state.provider_status_text)}</pre>
      </section>
    </div>
  `;
}

function renderConfigOverlay(state: DesktopWebState): string {
  return `
    <div class="modal-backdrop" data-action="close-overlay">
      <section class="modal wide" data-modal>
        <h2>設定</h2>
        <div class="config-grid">
          <div class="select-list">
            ${state.config_items
              .map(
                (item, index) => `
                  <button class="${index === state.selected_config_index ? "selected" : ""}" data-action="select-config" data-index="${index}">
                    ${escapeHtml(item)}
                  </button>`
              )
              .join("")}
          </div>
          <div>
            <label class="field-label">${escapeHtml(state.config_field_title)}</label>
            <textarea id="config-value">${escapeHtml(state.config_value_text)}</textarea>
            <pre class="feedback">${escapeHtml(state.config_feedback_text)}</pre>
            <div class="split-actions">
              <button data-action="apply-session-config">セッションに適用</button>
              <button data-action="save-project-config">プロジェクトに保存</button>
              <button data-action="save-global-config">全体に保存</button>
              <button data-action="open-project-config-folder">プロジェクト設定を開く</button>
              <button data-action="open-global-config-folder">全体設定を開く</button>
            </div>
          </div>
        </div>
      </section>
    </div>
  `;
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
  return `
    <div class="modal-backdrop">
      <section class="modal">
        <h2>確認が必要です</h2>
        <pre>${escapeHtml(state.confirmation_text)}</pre>
        <div class="modal-actions">
          <button data-action="deny">拒否</button>
          <button class="send wide-send" data-action="allow">許可</button>
        </div>
      </section>
    </div>
  `;
}

function renderNavRow(label: string, detail: string, selected: boolean, kind: string, index: number, deleteAction: string): string {
  return `
    <div class="nav-row-wrap ${selected ? "selected" : ""}">
      <button class="nav-row" data-action="${kind}" data-index="${index}">
        <span>${escapeHtml(label)}</span>
        <small>${escapeHtml(detail)}</small>
      </button>
      <button class="row-delete" data-action="${deleteAction}" data-index="${index}" title="削除">×</button>
    </div>
  `;
}

function wire(state: DesktopWebState): void {
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
    void command<DesktopWebState>("set_config_value", { text: (event.currentTarget as HTMLTextAreaElement).value }).catch((error) =>
      renderError(String(error))
    );
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
      if ((event.target as HTMLElement).closest("[data-modal]") && node.classList.contains("modal-backdrop")) return;
      const action = node.dataset.action ?? "";
      const index = Number(node.dataset.index ?? "-1");
      dispatchAction(action, index, state);
    });
  });
  document.querySelectorAll<HTMLElement>("[data-drag-region]").forEach((node) => {
    node.addEventListener("pointerdown", (event) => {
      if (event.button !== 0 || (event.target as HTMLElement).closest("button")) return;
      void desktopWindow.startDragging();
    });
  });
}

function dispatchAction(action: string, index: number, state: DesktopWebState): void {
  if (action === "minimize-window") void desktopWindow.minimize();
  if (action === "toggle-maximize-window") void desktopWindow.toggleMaximize();
  if (action === "close-window") void command("exit_app");
  if (action === "send" && state.can_submit) void mutate("submit_prompt");
  if (action === "refresh") void mutate("refresh_desktop");
  if (action === "new-chat") void mutate("new_chat");
  if (action === "project") void mutate("select_project", { index });
  if (action === "session") void mutate("select_session", { index });
  if (action === "delete-project") void mutate("delete_project", { index });
  if (action === "delete-session") void mutate("delete_session", { index });
  if (action === "artifact") void mutate("select_artifact", { index });
  if (action === "export-transcript") void mutate("export_transcript_markdown");
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
  if (action === "close-overlay") void mutate("close_overlay");
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
  if (action === "select-config") void mutate("set_config_selection", { index });
  if (action === "apply-session-config") void mutate("apply_session_config");
  if (action === "save-project-config") void mutate("save_project_config");
  if (action === "save-global-config") void mutate("save_global_config");
  if (action === "toggle-access") void mutate("toggle_access_mode");
  if (action === "insert-command") void mutate("insert_command", { index });
  if (action === "allow") void mutate("answer_permission", { allow: true });
  if (action === "deny") void mutate("answer_permission", { allow: false });
}

function renderError(message: string): void {
  appRoot.innerHTML = `<div class="fatal"><h1>moyAI Desktop</h1><pre>${escapeHtml(message)}</pre></div>`;
}

function formatMultiline(value: string): string {
  return escapeHtml(value).replace(/\n/g, "<br />");
}

function fileName(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
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

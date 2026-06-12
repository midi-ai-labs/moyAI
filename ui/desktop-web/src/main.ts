import { getCurrentWindow } from "@tauri-apps/api/window";
import { command } from "./api";
import { installGlobalKeyboardShortcuts, wireEvents } from "./events";
import {
  renderArtifactPane,
  renderComposer,
  renderConfirmation,
  renderLocalConfirmation,
  renderOverlay,
  renderRunStatusStrip,
  renderSidebar,
  renderStartupSplash,
  renderThreadContent,
  renderTitlebar,
  renderTopbar,
  setRenderContext,
} from "./render";
import type { DesktopWebState } from "./types";
import { createUiLocalState } from "./ui_state";
import { escapeHtml } from "./utils";
import "./styles.css";

const app = document.querySelector<HTMLDivElement>("#app");
const desktopWindow = getCurrentWindow();
let currentState: DesktopWebState | null = null;
let lastRenderedState: DesktopWebState | null = null;
let polling = false;
let previousSessionKey = "";
let splashDismissed = false;
let splashTimer: number | null = null;
const splashStartedAt = performance.now();
const SPLASH_MIN_VISIBLE_MS = 5000;
const THREAD_END_THRESHOLD_PX = 96;
const uiState = createUiLocalState();

if (!app) {
  throw new Error("app root missing");
}
const appRoot = app;
const eventContext = {
  desktopWindow,
  uiState,
  getCurrentState: () => currentState,
  setCurrentState: (state: DesktopWebState) => {
    currentState = state;
  },
  render,
  mutate,
  renderError,
};

void refresh();
window.setInterval(() => {
  if (currentState?.async_polling_required && !shouldDeferAutoRefresh()) {
    void refresh();
  }
}, 600);

installGlobalKeyboardShortcuts(eventContext);

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
    const previous = currentState;
    currentState = await command<DesktopWebState>(name, args);
    reconcileUiLocalState(previous, currentState, name);
    render(currentState);
  } catch (error) {
    renderError(String(error));
  }
}

function render(state: DesktopWebState): void {
  const elapsedSplashMs = performance.now() - splashStartedAt;
  if (!splashDismissed && shouldShowSplash(state, elapsedSplashMs)) {
    appRoot.innerHTML = renderStartupSplash(state, elapsedSplashMs, SPLASH_MIN_VISIBLE_MS);
    scheduleSplashReveal(state, elapsedSplashMs);
    lastRenderedState = state;
    return;
  }
  if (!splashDismissed) {
    splashDismissed = true;
    if (splashTimer !== null) {
      window.clearTimeout(splashTimer);
      splashTimer = null;
    }
  }
  const previous = lastRenderedState;
  reconcileUiLocalState(previous, state, null);
  const previousThread = document.querySelector<HTMLElement>("#thread");
  const previousThreadScrollTop = previousThread?.scrollTop ?? 0;
  const previousThreadWasNearEnd = previousThread ? isThreadNearEnd(previousThread) : true;
  const nextSessionKey = state.session_rows[state.selected_session_index]?.session_id ?? state.selected_session_title;
  const previousTranscriptCount = previous?.transcript_rows.length ?? 0;
  const previousChangeCount = previous?.file_change_rows.length ?? 0;
  const sessionChanged = nextSessionKey !== previousSessionKey;
  const contentAdvanced = state.transcript_rows.length > previousTranscriptCount || state.file_change_rows.length > previousChangeCount;
  const runCompleted = Boolean(previous?.busy && !state.busy) || isTerminalRunStatus(state.run_status_key);
  const shouldRevealEnd = sessionChanged || (previousThreadWasNearEnd && (state.busy || contentAdvanced || runCompleted));
  const hasArtifactContext = state.artifact_rows.length > 0 || state.file_change_rows.length > 0 || state.busy;
  if (hasArtifactContext && uiState.artifactPaneCollapsed) {
    uiState.artifactPaneCollapsed = false;
    window.localStorage.setItem("moyai.artifactPaneCollapsed", "false");
  }
  setRenderContext({
    artifactPaneCollapsed: uiState.artifactPaneCollapsed,
    attachmentTrayOpen: uiState.attachmentTrayOpen,
    configFilterText: uiState.configFilterText,
    configDirty: uiState.configDirty,
  });
  appRoot.innerHTML = `
    <div class="app-frame ${uiState.artifactPaneCollapsed ? "artifact-collapsed" : ""}" style="--window-opacity: ${state.window_opacity_percent / 100}">
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
    ${uiState.pendingLocalConfirmation ? renderLocalConfirmation(uiState.pendingLocalConfirmation) : ""}
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
  wireEvents(state, eventContext);
}

function shouldShowSplash(state: DesktopWebState, elapsedMs: number): boolean {
  return state.startup.status === "loading" || elapsedMs < SPLASH_MIN_VISIBLE_MS;
}

function scheduleSplashReveal(state: DesktopWebState, elapsedMs: number): void {
  if (state.startup.status === "loading" || elapsedMs >= SPLASH_MIN_VISIBLE_MS || splashTimer !== null) {
    return;
  }
  splashTimer = window.setTimeout(() => {
    splashTimer = null;
    if (currentState) {
      render(currentState);
    }
  }, Math.max(0, SPLASH_MIN_VISIBLE_MS - elapsedMs));
}

function reconcileUiLocalState(previous: DesktopWebState | null, state: DesktopWebState, mutationName: string | null): void {
  const nextSessionKey = state.session_rows[state.selected_session_index]?.session_id ?? state.selected_session_title;
  const previousSessionKey = previous?.session_rows[previous.selected_session_index]?.session_id ?? previous?.selected_session_title ?? "";
  const sessionChanged = previous !== null && nextSessionKey !== previousSessionKey;
  const overlayChanged = previous !== null && previous.overlay !== state.overlay;
  const imagesCleared = state.attached_images.length === 0 && state.image_input.trim().length === 0;

  if (sessionChanged || operationInvalidatesComposerTray(mutationName)) {
    uiState.attachmentTrayOpen = false;
  }
  if ((mutationName === "attach_image" || mutationName === "browse_image") && state.image_input.trim().length === 0) {
    uiState.attachmentTrayOpen = false;
  }
  if ((mutationName === "clear_images" || mutationName === "remove_image") && imagesCleared) {
    uiState.attachmentTrayOpen = false;
  }
  if (overlayChanged && state.overlay !== "config") {
    uiState.configDirty = false;
  }
  if (uiState.pendingLocalConfirmation && !localConfirmationStillTargetsRow(uiState.pendingLocalConfirmation, state)) {
    uiState.pendingLocalConfirmation = null;
  }
}

function operationInvalidatesComposerTray(name: string | null): boolean {
  if (
    name === "submit_prompt" ||
    name === "send_prompt_review" ||
    name === "select_project" ||
    name === "select_session" ||
    name === "select_chat_session" ||
    name === "new_chat" ||
    name === "new_project_session" ||
    name === "switch_workspace"
  ) {
    return true;
  }
  return false;
}

function localConfirmationStillTargetsRow(
  confirmation: NonNullable<typeof uiState.pendingLocalConfirmation>,
  state: DesktopWebState
): boolean {
  if (confirmation.kind === "project") {
    const row = state.project_rows[confirmation.index];
    return row?.label === confirmation.title && row?.path === confirmation.detail;
  }
  const rows = confirmation.kind === "chat_session" ? state.chat_session_rows : state.session_rows;
  const row = rows[confirmation.index];
  return row?.label === confirmation.title && row?.session_id === confirmation.detail;
}

function isTerminalRunStatus(status: DesktopWebState["run_status_key"]): boolean {
  return status === "completed" || status === "awaiting_user" || status === "cancelled" || status === "failed";
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

function shouldDeferAutoRefresh(): boolean {
  const active = document.activeElement;
  const editingText =
    active instanceof HTMLInputElement ||
    active instanceof HTMLTextAreaElement ||
    active instanceof HTMLSelectElement;
  return editingText || currentState?.overlay === "provider" || currentState?.overlay === "config";
}

function renderError(message: string): void {
  appRoot.innerHTML = `<div class="fatal"><h1>moyAI Desktop</h1><pre>${escapeHtml(message)}</pre></div>`;
}

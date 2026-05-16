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
  if (currentState?.busy || currentState?.confirmation_visible || currentState?.provider_loading || currentState?.navigation_loading) {
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

function renderError(message: string): void {
  appRoot.innerHTML = `<div class="fatal"><h1>moyAI Desktop</h1><pre>${escapeHtml(message)}</pre></div>`;
}

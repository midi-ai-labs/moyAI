import { getCurrentWindow } from "@tauri-apps/api/window";
import { command } from "./api";
import { agentActivityRowsChanged } from "./agent_activity";
import { commandConflictState } from "./command_error";
import type { ActionContext } from "./actions";
import {
  installGlobalKeyboardShortcuts,
  prepareConfigMutation,
  wireEvents,
} from "./events";
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
import { createUiLocalState, reconcileAgentPaneState } from "./ui_state";
import {
  InteractionLifecycle,
  type InteractionRelease,
  shouldBeginKeyboardInteraction,
  shouldBeginPointerInteraction,
} from "./interaction_lifecycle";
import {
  appliedProjectionRevision,
  deferredProjectionCandidatePreferred,
  projectionUpdateAccepted,
} from "./projection_state";
import { modalIsOpen } from "./modal_state";
import { autoRefreshAllowed, runtimePollingRequired } from "./polling_state";
import {
  beginPermissionDecision,
  failPermissionDecision,
  finishLocalDecision,
  finishPermissionDecision,
  permissionDecisionResponseAccepted,
} from "./decision_state";
import { escapeHtml, humanizeError } from "./utils";
import {
  configDraftAppliesTo,
  configMutationPending,
} from "./config_mutation";
import { rowMutationTargetStillMatches } from "./row_target";
import {
  acknowledgeDraftMutation,
  captureDraftMutation,
  configDraftEditOpen,
  configDraftCommitOpen,
  configDraftDiscardOpen,
  configOwnerMutationOpen,
  type DraftMutationSnapshot,
  mutationAdmissionOpen,
  mutationChangesConfigOwner,
  mutationStartsRun,
  operationInvalidatesComposer,
  projectViewState,
  reconcileUiDrafts,
  rejectDraftMutation,
} from "./view_state";
import "./styles.css";

const app = document.querySelector<HTMLDivElement>("#app");
const desktopWindow = getCurrentWindow();
let currentState: DesktopWebState | null = null;
let lastRenderedState: DesktopWebState | null = null;
let polling = false;
let previousSessionKey = "";
let lastRenderedLocalConfirmationPending = false;
let splashDismissed = false;
let splashTimer: number | null = null;
const splashStartedAt = performance.now();
const SPLASH_MIN_VISIBLE_MS = 5000;
const THREAD_END_THRESHOLD_PX = 96;
const INTERACTION_INACTIVITY_RECOVERY_MS = 5 * 60_000;
const uiState = createUiLocalState();

interface StateUpdate {
  state: DesktopWebState;
  render: boolean;
  mutationName: string | null;
  scheduleNavigation: boolean;
  sequence: number;
  draftSnapshot: DraftMutationSnapshot | null;
}

const interactionLifecycle = new InteractionLifecycle<StateUpdate>((current, candidate) =>
  deferredProjectionCandidatePreferred(
    current.state.projection_revision,
    candidate.state.projection_revision,
    current.sequence,
    candidate.sequence,
  ),
);
let nextStateSequence = 1;
let lastAppliedStateSequence = 0;
let lastAppliedProjectionRevision = "0";
let interactionWatchdog: number | null = null;
const modalScrollReturnStack: ScrollSnapshot[][] = [];
const modalDetailsReturnStack: DetailSnapshot[][] = [];
const modalFocusReturnStack: Array<FocusSnapshot | null> = [];

if (!app) {
  throw new Error("app root missing");
}
const appRoot = app;
const eventContext: ActionContext = {
  desktopWindow,
  uiState,
  getProjection: () => currentState,
  getViewState: () => currentState ? projectViewState(currentState, uiState) : null,
  acceptProjection: (state: DesktopWebState, shouldRender = true) => acceptState(state, shouldRender),
  rerender: () => {
    if (currentState) acceptState(currentState, true);
  },
  mutate,
  recoverCommandConflict,
  reportError,
  prepareConfigMutation: (target) => prepareConfigMutation(eventContext, target),
  submitPermissionDecision,
  setWindowMaximized,
};

installPointerRenderGate();
installKeyboardStateGate();
installCompositionStateGate();
installWindowMaximizedSync();
void refresh();
window.setInterval(() => {
  if (
    currentState
    && runtimePollingRequired(currentState.async_polling_required, uiState.runStartMutationPending)
    && shouldAutoRefresh(currentState)
  ) {
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
    render(await command<DesktopWebState>("desktop_state"));
  } catch (error) {
    reportError(String(error));
  } finally {
    polling = false;
  }
}

async function mutate(name: string, args?: Record<string, unknown>): Promise<void> {
  const startsRun = mutationStartsRun(name);
  const changesConfigOwner = mutationChangesConfigOwner(name);
  if (!mutationAdmissionOpen(uiState, name)) return;
  if (startsRun) {
    uiState.runStartMutationPending = true;
    if (currentState) acceptState(currentState, true);
    window.setTimeout(() => void refresh(), 0);
  }
  if (changesConfigOwner) {
    uiState.externalConfigMutationPending = true;
    if (currentState) acceptState(currentState, true);
  }
  const draftSnapshot = captureDraftMutation(uiState, name);
  try {
    const state = await command<DesktopWebState>(name, args);
    acknowledgeDraftMutation(uiState, state, name, draftSnapshot);
    acceptState(state, true, name, true, draftSnapshot);
    if (currentState && currentState !== state) acceptState(currentState, true);
  } catch (error) {
    rejectDraftMutation(uiState, name, draftSnapshot);
    if (!recoverCommandConflict(error)) reportError(String(error));
  } finally {
    if (startsRun) {
      uiState.runStartMutationPending = false;
      if (currentState) acceptState(currentState, true);
    }
    if (changesConfigOwner) {
      uiState.externalConfigMutationPending = false;
      if (currentState) acceptState(currentState, true);
    }
  }
}

function recoverCommandConflict(error: unknown): boolean {
  const state = commandConflictState(error);
  if (!state) return false;
  acceptState(state, true, "command_conflict");
  return true;
}

function render(state: DesktopWebState): void {
  acceptState(state, true);
}

function acceptState(
  state: DesktopWebState,
  shouldRender: boolean,
  mutationName: string | null = null,
  scheduleNavigation = false,
  draftSnapshot: DraftMutationSnapshot | null = null,
): void {
  const stateChangeRequiresRender = currentState !== null && requiresRenderForStateChange(currentState, state);
  const update: StateUpdate = {
    state,
    render: shouldRender || stateChangeRequiresRender,
    mutationName,
    scheduleNavigation,
    sequence: nextStateSequence++,
    draftSnapshot,
  };
  applyStateUpdate(update);
}

function requiresRenderForStateChange(previous: DesktopWebState, state: DesktopWebState): boolean {
  if (
    previous.provider_label !== state.provider_label ||
    previous.model_label !== state.model_label ||
    previous.access_label !== state.access_label ||
    previous.current_session_label !== state.current_session_label ||
    previous.selected_session_title !== state.selected_session_title ||
    previous.status_message !== state.status_message ||
    previous.status_detail !== state.status_detail ||
    previous.run_status_text !== state.run_status_text ||
    previous.run_phase !== state.run_phase ||
    previous.run_active_step !== state.run_active_step ||
    previous.latest_tool_summary !== state.latest_tool_summary ||
    previous.progress_text !== state.progress_text ||
    previous.tool_status_text !== state.tool_status_text ||
    previous.token_meter_label !== state.token_meter_label ||
    previous.confirmation_visible !== state.confirmation_visible ||
    previous.confirmation_id !== state.confirmation_id ||
    previous.confirmation?.agent_path !== state.confirmation?.agent_path ||
    previous.confirmation?.agent_task_name !== state.confirmation?.agent_task_name ||
    previous.composer_commit_generation !== state.composer_commit_generation ||
    previous.agent_tree_active !== state.agent_tree_active ||
    previous.async_polling_required !== state.async_polling_required ||
    previous.provider_loading !== state.provider_loading ||
    previous.navigation_loading !== state.navigation_loading ||
    previous.busy !== state.busy ||
    previous.post_run_refresh_pending !== state.post_run_refresh_pending ||
    previous.background_mutation_pending !== state.background_mutation_pending ||
    previous.overlay !== state.overlay ||
    previous.startup.status !== state.startup.status ||
    previous.run_status_key !== state.run_status_key ||
    previous.can_submit !== state.can_submit ||
    previous.selected_project_index !== state.selected_project_index ||
    previous.selected_session_index !== state.selected_session_index ||
    previous.selected_artifact_index !== state.selected_artifact_index ||
    previous.thread_empty !== state.thread_empty ||
    previous.turn_page_offset !== state.turn_page_offset ||
    previous.turn_page_total !== state.turn_page_total ||
    previous.turn_page_has_more !== state.turn_page_has_more ||
    previous.transcript_rows.length !== state.transcript_rows.length ||
    previous.artifact_preview_available !== state.artifact_preview_available ||
    previous.artifact_preview_text !== state.artifact_preview_text ||
    previous.provider_metadata_mode !== state.provider_metadata_mode ||
    previous.provider_selected_index !== state.provider_selected_index ||
    previous.provider_status?.kind !== state.provider_status?.kind ||
    previous.provider_status?.title !== state.provider_status?.title ||
    previous.provider_status?.hint !== state.provider_status?.hint ||
    previous.provider_status?.details !== state.provider_status?.details ||
    previous.provider_selected_model_summary.join("\u0000") !== state.provider_selected_model_summary.join("\u0000") ||
    previous.provider_model_ids.join("\u0000") !== state.provider_model_ids.join("\u0000") ||
    previous.provider_apply_enabled !== state.provider_apply_enabled ||
    previous.config_target.workspacePath !== state.config_target.workspacePath ||
    previous.config_target.sessionId !== state.config_target.sessionId ||
    previous.config_target.configGeneration !== state.config_target.configGeneration ||
    previous.config_feedback_text !== state.config_feedback_text ||
    previous.review_status_text !== state.review_status_text ||
    previous.send_enhanced_enabled !== state.send_enhanced_enabled ||
    previous.send_raw_enabled !== state.send_raw_enabled ||
    previous.history_export_enabled !== state.history_export_enabled ||
    previous.enhance_enabled !== state.enhance_enabled ||
    previous.image_input_enabled !== state.image_input_enabled
  ) {
    return true;
  }
  return (
    keyedRowsChanged(previous.project_rows, state.project_rows, (row) => `${row.project_id}:${row.path}:${row.label}`) ||
    keyedRowsChanged(
      previous.session_rows,
      state.session_rows,
      (row) => `${row.session_id}:${row.label}:${row.status}:${row.loaded_status}:${row.archived}:${row.pending_permission_requests}:${row.pending_user_input_requests}`,
    ) ||
    keyedRowsChanged(
      previous.chat_session_rows,
      state.chat_session_rows,
      (row) => `${row.session_id}:${row.label}:${row.status}:${row.loaded_status}:${row.archived}:${row.pending_permission_requests}:${row.pending_user_input_requests}`,
    ) ||
    keyedRowsChanged(previous.artifact_rows, state.artifact_rows, (row) => `${row.path}:${row.action}:${row.label}`) ||
    keyedRowsChanged(previous.file_change_rows, state.file_change_rows, (row) => `${row.path}:${row.action}:${row.summary}`) ||
    agentActivityRowsChanged(previous.agent_activity_rows ?? [], state.agent_activity_rows ?? []) ||
    keyedRowsChanged(previous.provider_models, state.provider_models, (model) => model) ||
    keyedRowsChanged(previous.config_fields, state.config_fields, (field) => `${field.key}:${field.value}:${field.env_override ?? ""}`) ||
    keyedRowsChanged(previous.attached_images, state.attached_images, (imagePath) => imagePath)
  );
}

function keyedRowsChanged<T>(previous: T[], state: T[], key: (value: T) => string): boolean {
  return previous.length !== state.length || previous.some((value, index) => key(value) !== key(state[index]));
}

function deferredStateUpdateStillAccepted(update: StateUpdate): boolean {
  return projectionUpdateAccepted(
    lastAppliedProjectionRevision,
    update.state.projection_revision,
    update.state === currentState,
  );
}

function applyStateUpdate(update: StateUpdate): void {
  if (update.sequence <= lastAppliedStateSequence) return;
  if (
    !projectionUpdateAccepted(
      lastAppliedProjectionRevision,
      update.state.projection_revision,
      update.state === currentState,
    )
  ) {
    return;
  }
  if (interactionLifecycle.defer({ ...update, render: true }, update.state === currentState, update.render)) return;
  const previousProjection = currentState;
  reconcileUiDrafts(uiState, previousProjection, update.state, update.draftSnapshot);
  reconcileAgentPaneState(uiState, update.state);
  currentState = update.state;
  lastAppliedStateSequence = update.sequence;
  lastAppliedProjectionRevision = appliedProjectionRevision(
    lastAppliedProjectionRevision,
    update.state.projection_revision,
  );
  if (update.render) {
    renderCommitted(projectViewState(update.state, uiState), update.mutationName);
  }
  if (update.scheduleNavigation) {
    scheduleNavigationRefresh(update.state);
  }
}

function renderCommitted(state: DesktopWebState, mutationName: string | null): void {
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
  reconcileUiLocalState(previous, state, mutationName);
  const localConfirmationPending = uiState.pendingLocalConfirmation !== null;
  const backgroundInert = modalIsOpen(state, localConfirmationPending);
  const localConfirmationOpening = !lastRenderedLocalConfirmationPending && localConfirmationPending;
  const localConfirmationClosing = lastRenderedLocalConfirmationPending && !localConfirmationPending;
  const modalOpening = (previous !== null && isModalOpening(previous, state)) || localConfirmationOpening;
  const modalClosing = (previous !== null && isModalClosing(previous, state)) || localConfirmationClosing;
  if (modalOpening) {
    modalScrollReturnStack.push(captureSelectorScrollSnapshots(MODAL_SCROLL_SELECTORS));
    modalDetailsReturnStack.push(captureCurrentDetailSnapshots());
    modalFocusReturnStack.push(captureCurrentFocusSnapshot());
  }
  const focusSnapshot = modalClosing
    ? (modalFocusReturnStack.pop() ?? null)
    : modalOpening
      ? null
      : captureFocusSnapshot(previous, state);
  const scrollSnapshots = captureScrollSnapshots(previous, state);
  if (modalClosing) scrollSnapshots.push(...(modalScrollReturnStack.pop() ?? []));
  const detailSnapshots = captureDetailSnapshots(previous, state);
  if (modalClosing) detailSnapshots.push(...(modalDetailsReturnStack.pop() ?? []));
  const previousThread = document.querySelector<HTMLElement>("#thread");
  const previousThreadScrollTop = previousThread?.scrollTop ?? 0;
  const previousThreadWasNearEnd = previousThread ? isThreadNearEnd(previousThread) : true;
  const nextSessionKey = state.session_rows[state.selected_session_index]?.session_id ?? state.selected_session_title;
  const previousTranscriptCount = previous?.transcript_rows.length ?? 0;
  const previousChangeCount = previous?.file_change_rows.length ?? 0;
  const previousAgentRows = previous?.agent_activity_rows ?? [];
  const agentRows = state.agent_activity_rows ?? [];
  const sessionChanged = nextSessionKey !== previousSessionKey;
  const contentAdvanced = state.transcript_rows.length > previousTranscriptCount || state.file_change_rows.length > previousChangeCount;
  const agentActivityAdvanced = previous
    ? agentActivityRowsChanged(previousAgentRows, agentRows) || (!previous.agent_tree_active && state.agent_tree_active)
    : agentRows.length > 0;
  const runCompleted = Boolean(previous?.busy && !state.busy) || isTerminalRunStatus(state.run_status_key);
  const shouldRevealEnd = sessionChanged
    || (previousThreadWasNearEnd && (state.busy || state.agent_tree_active || contentAdvanced || agentActivityAdvanced || runCompleted));
  const previousOutputCount = (previous?.artifact_rows.length ?? 0) + (previous?.file_change_rows.length ?? 0) + previousAgentRows.length;
  const outputCount = state.artifact_rows.length + state.file_change_rows.length + agentRows.length;
  if (previous && outputCount > 0 && previousOutputCount === 0 && uiState.artifactPaneCollapsed) {
    uiState.artifactPaneCollapsed = false;
    window.localStorage.setItem("moyai.artifactPaneCollapsed", "false");
  }
  setRenderContext({
    artifactPaneCollapsed: uiState.artifactPaneCollapsed,
    artifactPaneMode: uiState.artifactPaneMode,
    selectedAgentPath: uiState.selectedAgentPath,
    attachmentTrayOpen: uiState.attachmentTrayOpen,
    configDirty: configDraftAppliesTo(uiState, state.config_target),
    configMutationPending: configMutationPending(uiState),
    configOwnerMutationOpen: configOwnerMutationOpen(uiState),
    configDraftEditOpen: configDraftEditOpen(uiState),
    configDraftDiscardOpen: configDraftDiscardOpen(uiState),
    configDraftCommitOpen: configDraftCommitOpen(uiState, state.startup.initial_setup_required),
  });
  const preservedTitlebar = document.querySelector<HTMLElement>(".app-titlebar");
  preservedTitlebar?.remove();
  appRoot.innerHTML = `
    <div class="app-frame ${uiState.artifactPaneCollapsed ? "artifact-collapsed" : ""}" style="--window-opacity: ${state.window_opacity_percent / 100}">
      ${renderTitlebar(uiState.windowMaximized, backgroundInert)}
      <div class="shell" ${backgroundInert ? 'inert aria-hidden="true"' : ""}>
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
    ${
      !state.confirmation_visible && uiState.pendingLocalConfirmation
        ? renderLocalConfirmation(
            uiState.pendingLocalConfirmation,
            uiState.localConfirmationDecisionPending,
            uiState.localConfirmationDecisionError,
          )
        : ""
    }
    ${state.confirmation_visible ? renderConfirmation(state) : ""}
    ${!state.confirmation_visible && !localConfirmationPending && state.overlay !== "none" ? renderOverlay(state) : ""}
    ${backgroundInert ? "" : renderRecoverableError()}
  `;
  const nextTitlebar = document.querySelector<HTMLElement>(".app-titlebar");
  if (preservedTitlebar && nextTitlebar) {
    nextTitlebar.replaceWith(preservedTitlebar);
    const applicationCommands = preservedTitlebar.querySelector<HTMLElement>(".titlebar-menu");
    applicationCommands?.toggleAttribute("inert", backgroundInert);
    if (backgroundInert) {
      applicationCommands?.setAttribute("aria-hidden", "true");
    } else {
      applicationCommands?.removeAttribute("aria-hidden");
    }
  }
  const thread = document.querySelector<HTMLElement>("#thread");
  if (thread && shouldRevealEnd) {
    revealThreadEnd(thread);
  } else if (thread && previousThread) {
    restoreThreadPosition(thread, previousThreadScrollTop);
  }
  previousSessionKey = nextSessionKey;
  lastRenderedLocalConfirmationPending = localConfirmationPending;
  lastRenderedState = state;
  restoreScrollSnapshots(scrollSnapshots);
  restoreDetailSnapshots(detailSnapshots);
  restoreFocusSnapshot(focusSnapshot);
  wireEvents(state, eventContext);
  focusSelectedAgentAfterRender();
  if (state.confirmation_visible && uiState.permissionDecisionPending && uiState.permissionDecisionAllow !== null) {
    setPermissionDecisionPendingUi(uiState.permissionDecisionAllow);
  } else if (state.confirmation_visible && uiState.permissionDecisionError) {
    const status = document.querySelector<HTMLElement>(".permission-decision-status");
    if (status) status.textContent = uiState.permissionDecisionError;
  }
  if (
    modalClosing &&
    !focusSnapshot &&
    !state.confirmation_visible &&
    !localConfirmationPending &&
    state.overlay === "none"
  ) {
    requestAnimationFrame(() => document.querySelector<HTMLTextAreaElement>("#prompt")?.focus());
  }
  focusPromptIfRequested(state);
}

function focusSelectedAgentAfterRender(): void {
  if (!uiState.focusSelectedAgentAfterRender || !uiState.selectedAgentPath) return;
  const agentPath = uiState.selectedAgentPath;
  uiState.focusSelectedAgentAfterRender = false;
  requestAnimationFrame(() => {
    const summary = document.querySelector<HTMLElement>(
      `[data-focus-key="sub-agent-summary:${CSS.escape(agentPath)}"]`,
    );
    const detail = summary?.closest<HTMLDetailsElement>("details[data-details-key]");
    if (detail) detail.open = true;
    summary?.focus({ preventScroll: true });
    summary?.scrollIntoView({ block: "nearest", inline: "nearest" });
  });
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
  const imagesCleared = state.attached_images.length === 0 && state.image_input.trim().length === 0;

  if (sessionChanged || operationInvalidatesComposer(mutationName)) {
    uiState.attachmentTrayOpen = false;
  }
  if (mutationName === "new_chat" || mutationName === "new_project_session") {
    uiState.focusPromptAfterRender = true;
  }
  if ((mutationName === "attach_image" || mutationName === "browse_image") && state.image_input.trim().length === 0) {
    uiState.attachmentTrayOpen = false;
  }
  if ((mutationName === "clear_images" || mutationName === "remove_image") && imagesCleared) {
    uiState.attachmentTrayOpen = false;
  }
  if (uiState.pendingLocalConfirmation && !localConfirmationStillTargetsRow(uiState.pendingLocalConfirmation, state)) {
    uiState.pendingLocalConfirmation = null;
    finishLocalDecision(uiState);
  }
  if (
    !state.confirmation_visible
    || (uiState.permissionDecisionPending
      && state.confirmation_id !== uiState.permissionDecisionConfirmationId)
  ) {
    finishPermissionDecision(uiState);
  }
}

function focusPromptIfRequested(state: DesktopWebState): void {
  const shouldFocusInitialPrompt =
    !uiState.initialPromptFocusDone &&
    state.selected_session_index < 0 &&
    !state.busy &&
    state.overlay === "none" &&
    !state.confirmation_visible;
  if (!uiState.focusPromptAfterRender && !shouldFocusInitialPrompt) {
    return;
  }
  if (state.busy || state.overlay !== "none" || state.confirmation_visible) {
    return;
  }
  uiState.initialPromptFocusDone = true;
  uiState.focusPromptAfterRender = false;
  requestAnimationFrame(() => {
    document.querySelector<HTMLTextAreaElement>("#prompt")?.focus();
  });
}

interface FocusSnapshot {
  selector: string;
  occurrence: number;
  selectionStart: number | null;
  selectionEnd: number | null;
}

interface ScrollSnapshot {
  selector: string;
  occurrence: number;
  scrollLeft: number;
  scrollTop: number;
}

interface DetailSnapshot {
  key: string;
  open: boolean;
}

const STABLE_LIST_SCROLL_SELECTORS = [".project-list", ".chat-list", ".artifact-list", ".sub-agent-list"];
const MODAL_SCROLL_SELECTORS = [".modal", ".settings-content", ".settings-nav", ".select-list"];

function captureFocusSnapshot(previous: DesktopWebState | null, state: DesktopWebState): FocusSnapshot | null {
  if (!previous || previous.overlay !== state.overlay || previous.confirmation_visible !== state.confirmation_visible) {
    return null;
  }
  return captureCurrentFocusSnapshot();
}

function captureCurrentFocusSnapshot(): FocusSnapshot | null {
  const active = document.activeElement;
  if (!(active instanceof HTMLElement)) {
    return null;
  }
  const parentDetailsKey = active.tagName === "SUMMARY"
    ? active.closest<HTMLDetailsElement>("details[data-details-key]")?.dataset.detailsKey
    : undefined;
  const stableHref = active instanceof HTMLAnchorElement && active.getAttribute("href")?.startsWith("#")
    ? active.getAttribute("href")
    : null;
  const selector = active.id
    ? `#${CSS.escape(active.id)}`
    : active.dataset.configKey
      ? `[data-config-key="${CSS.escape(active.dataset.configKey)}"]`
      : active.dataset.focusKey
        ? `[data-focus-key="${CSS.escape(active.dataset.focusKey)}"]`
      : active.dataset.action
        ? `[data-action="${CSS.escape(active.dataset.action)}"]`
      : parentDetailsKey
        ? `details[data-details-key="${CSS.escape(parentDetailsKey)}"] > summary`
      : stableHref
        ? `.settings-nav a[href="${CSS.escape(stableHref)}"]`
      : "";
  if (!selector) return null;
  const occurrence = Array.from(document.querySelectorAll(selector)).indexOf(active);
  if (occurrence < 0) return null;
  return {
    selector,
    occurrence,
    selectionStart: active instanceof HTMLInputElement || active instanceof HTMLTextAreaElement ? active.selectionStart : null,
    selectionEnd: active instanceof HTMLInputElement || active instanceof HTMLTextAreaElement ? active.selectionEnd : null,
  };
}

function restoreFocusSnapshot(snapshot: FocusSnapshot | null): void {
  if (!snapshot) return;
  const target = document.querySelectorAll<HTMLElement>(snapshot.selector)[snapshot.occurrence];
  if (!target || (target instanceof HTMLButtonElement && target.disabled)) return;
  target.focus({ preventScroll: true });
  if (
    (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement) &&
    snapshot.selectionStart !== null &&
    snapshot.selectionEnd !== null
  ) {
    target.setSelectionRange(snapshot.selectionStart, snapshot.selectionEnd);
  }
}

function captureScrollSnapshots(previous: DesktopWebState | null, state: DesktopWebState): ScrollSnapshot[] {
  const selectors = [...STABLE_LIST_SCROLL_SELECTORS];
  if (previous && selectedSessionIdentity(previous) === selectedSessionIdentity(state)) selectors.push(".activity");
  if (previous && selectedArtifactIdentity(previous) === selectedArtifactIdentity(state)) selectors.push(".preview");
  if (previous && modalIdentity(previous) === modalIdentity(state) && modalIdentity(state) !== "none") {
    selectors.push(...MODAL_SCROLL_SELECTORS);
  }
  return captureSelectorScrollSnapshots(selectors);
}

function captureSelectorScrollSnapshots(selectors: string[]): ScrollSnapshot[] {
  const snapshots: ScrollSnapshot[] = [];
  for (const selector of selectors) {
    document.querySelectorAll<HTMLElement>(selector).forEach((node, occurrence) => {
      snapshots.push({ selector, occurrence, scrollLeft: node.scrollLeft, scrollTop: node.scrollTop });
    });
  }
  return snapshots;
}

function restoreScrollSnapshots(snapshots: ScrollSnapshot[]): void {
  for (const snapshot of snapshots) {
    const target = document.querySelectorAll<HTMLElement>(snapshot.selector)[snapshot.occurrence];
    if (!target) continue;
    target.scrollLeft = snapshot.scrollLeft;
    target.scrollTop = snapshot.scrollTop;
  }
}

function captureDetailSnapshots(previous: DesktopWebState | null, state: DesktopWebState): DetailSnapshot[] {
  if (
    !previous ||
    selectedSessionIdentity(previous) !== selectedSessionIdentity(state) ||
    modalIdentity(previous) !== modalIdentity(state)
  ) {
    return [];
  }
  return captureCurrentDetailSnapshots();
}

function captureCurrentDetailSnapshots(): DetailSnapshot[] {
  return Array.from(document.querySelectorAll<HTMLDetailsElement>("details[data-details-key]"), (detail) => ({
    key: detail.dataset.detailsKey ?? "",
    open: detail.open,
  })).filter((snapshot) => snapshot.key.length > 0);
}

function restoreDetailSnapshots(snapshots: DetailSnapshot[]): void {
  const details = Array.from(document.querySelectorAll<HTMLDetailsElement>("details[data-details-key]"));
  for (const snapshot of snapshots) {
    const detail = details.find((candidate) => candidate.dataset.detailsKey === snapshot.key);
    if (detail) detail.open = snapshot.open;
  }
}

function selectedSessionIdentity(state: DesktopWebState): string {
  return state.session_rows[state.selected_session_index]?.session_id ?? state.selected_session_title;
}

function selectedArtifactIdentity(state: DesktopWebState): string {
  return state.artifact_rows[state.selected_artifact_index]?.path ?? "none";
}

function modalIdentity(state: DesktopWebState): string {
  return state.confirmation_visible ? "permission" : state.overlay;
}

function isModalOpening(previous: DesktopWebState, state: DesktopWebState): boolean {
  return (
    (!previous.confirmation_visible && state.confirmation_visible) ||
    (!state.confirmation_visible && previous.overlay === "none" && state.overlay !== "none")
  );
}

function isModalClosing(previous: DesktopWebState, state: DesktopWebState): boolean {
  return (
    (previous.confirmation_visible && !state.confirmation_visible) ||
    (!previous.confirmation_visible && previous.overlay !== "none" && state.overlay === "none")
  );
}

function installPointerRenderGate(): void {
  document.addEventListener(
    "pointerdown",
    (event) => {
      const target = event.target;
      if (!(target instanceof Element) || !shouldBeginPointerInteraction(event.button, appRoot.contains(target))) return;
      const directOwner = target.closest<HTMLElement>(
        'input, textarea, select, summary, [contenteditable="true"]',
      );
      const action = target.closest<HTMLElement>("[data-action]");
      const owner = directOwner ?? action;
      if (!interactionLifecycle.beginPointer(event.pointerId)) return;
      if (owner && !owner.matches(":disabled")) {
        try {
          owner.setPointerCapture(event.pointerId);
        } catch {
          // Text selection, native scrollbars, and synthetic pointer events may not support capture.
        }
      }
      armInteractionWatchdog();
    },
    true,
  );
  document.addEventListener(
    "pointerup",
    (event) => window.setTimeout(() => finishInteraction(interactionLifecycle.endPointer(event.pointerId)), 0),
    true,
  );
  document.addEventListener("pointermove", () => {
    if (interactionLifecycle.active) armInteractionWatchdog();
  }, true);
  document.addEventListener("pointercancel", (event) => finishInteraction(interactionLifecycle.endPointer(event.pointerId)), true);
  document.addEventListener(
    "lostpointercapture",
    (event) => window.setTimeout(() => finishInteraction(interactionLifecycle.endPointer(event.pointerId)), 0),
    true,
  );
  window.addEventListener("blur", cancelInteractionLifecycle);
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) cancelInteractionLifecycle();
  });
}

function installKeyboardStateGate(): void {
  document.addEventListener(
    "keydown",
    (event) => {
      const target = event.target;
      if (!(target instanceof Element)) return;
      const disabledOwner = target.closest<HTMLElement>(":disabled");
      if (!shouldBeginKeyboardInteraction(
        event.isComposing,
        event.code,
        appRoot.contains(target),
        disabledOwner !== null,
      )) return;
      interactionLifecycle.beginKey(event.code);
      armInteractionWatchdog();
    },
    true,
  );
  document.addEventListener(
    "keyup",
    (event) => {
      window.setTimeout(() => finishInteraction(interactionLifecycle.endKey(event.code)), 0);
    },
    true,
  );
  window.addEventListener("blur", cancelInteractionLifecycle);
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) cancelInteractionLifecycle();
  });
}

function installCompositionStateGate(): void {
  document.addEventListener("compositionstart", () => {
    interactionLifecycle.beginComposition();
    armInteractionWatchdog();
  }, true);
  document.addEventListener("compositionend", () => {
    window.setTimeout(() => finishInteraction(interactionLifecycle.endComposition()), 0);
  }, true);
  document.addEventListener("compositionupdate", armInteractionWatchdog, true);
  document.addEventListener("input", (event) => {
    if ((event as InputEvent).isComposing) armInteractionWatchdog();
  }, true);
  window.addEventListener("blur", cancelInteractionLifecycle);
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) cancelInteractionLifecycle();
  });
}

function armInteractionWatchdog(): void {
  if (interactionWatchdog !== null) window.clearTimeout(interactionWatchdog);
  interactionWatchdog = window.setTimeout(cancelInteractionLifecycle, INTERACTION_INACTIVITY_RECOVERY_MS);
}

function cancelInteractionLifecycle(): void {
  finishInteraction(interactionLifecycle.cancel());
}

function finishInteraction(release: InteractionRelease<StateUpdate> | null): void {
  if (!release) return;
  if (interactionWatchdog !== null) {
    window.clearTimeout(interactionWatchdog);
    interactionWatchdog = null;
  }
  if (release.deferred && deferredStateUpdateStillAccepted(release.deferred)) {
    applyStateUpdate({ ...release.deferred, render: release.deferred.render || release.renderCurrent });
  } else if (release.renderCurrent && currentState) {
    acceptState(currentState, true);
  }
}

async function submitPermissionDecision(allow: boolean): Promise<void> {
  const confirmationId = currentState?.confirmation_id ?? null;
  if (confirmationId === null || !beginPermissionDecision(uiState, confirmationId, allow)) return;
  setPermissionDecisionPendingUi(allow);
  try {
    const state = await command<DesktopWebState>("answer_permission", { allow, confirmationId });
    if (!permissionDecisionResponseAccepted(confirmationId, state.confirmation_visible, state.confirmation_id)) {
      failPermissionDecision(uiState, "決定を反映できませんでした。もう一度お試しください。");
    } else {
      finishPermissionDecision(uiState);
    }
    acceptState(state, true, "answer_permission", true);
  } catch {
    failPermissionDecision(uiState, "決定を反映できませんでした。もう一度お試しください。");
    if (currentState) render(currentState);
  }
}

function setPermissionDecisionPendingUi(allow: boolean): void {
  const dialog = document.querySelector<HTMLElement>(".confirmation[role='alertdialog']");
  dialog?.setAttribute("aria-busy", "true");
  document.querySelectorAll<HTMLButtonElement>("[data-permission-action]").forEach((button) => {
    button.disabled = true;
  });
  const selected = document.querySelector<HTMLButtonElement>(`[data-action="${allow ? "allow" : "deny"}"]`);
  if (selected) selected.textContent = allow ? "許可しています…" : "拒否しています…";
  const status = document.querySelector<HTMLElement>(".permission-decision-status");
  if (status) {
    status.textContent = allow ? "許可を反映しています。" : "拒否を反映しています。";
    status.tabIndex = 0;
    status.focus({ preventScroll: true });
  }
}

function installWindowMaximizedSync(): void {
  let frame: number | null = null;
  const sync = () => {
    if (frame !== null) window.cancelAnimationFrame(frame);
    frame = window.requestAnimationFrame(() => {
      frame = null;
      void command<boolean>("is_window_maximized").then(setWindowMaximized).catch(() => undefined);
    });
  };
  window.addEventListener("resize", sync);
  sync();
}

function setWindowMaximized(maximized: boolean): void {
  uiState.windowMaximized = maximized;
  const button = document.querySelector<HTMLButtonElement>('[data-action="toggle-maximize-window"]');
  if (!button) return;
  const label = maximized ? "元のサイズに戻す" : "最大化";
  button.title = label;
  button.setAttribute("aria-label", label);
  button.setAttribute("aria-pressed", String(maximized));
  button.querySelector<HTMLElement>(".maximize-icon")?.classList.toggle("restore", maximized);
}

function localConfirmationStillTargetsRow(
  confirmation: NonNullable<typeof uiState.pendingLocalConfirmation>,
  state: DesktopWebState
): boolean {
  if (confirmation.kind === "project") {
    const row = state.project_rows[confirmation.index];
    return row?.label === confirmation.title
      && row?.path === confirmation.detail
      && rowMutationTargetStillMatches(state, confirmation.expectedTarget, row?.project_id);
  }
  const rows = confirmation.kind === "chat_session" ? state.chat_session_rows : state.session_rows;
  const row = rows[confirmation.index];
  return row?.label === confirmation.title
    && row?.session_id === confirmation.detail
    && rowMutationTargetStillMatches(state, confirmation.expectedTarget, row?.session_id);
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

function shouldAutoRefresh(state: DesktopWebState): boolean {
  return autoRefreshAllowed(state, shouldDeferAutoRefresh());
}

function scheduleNavigationRefresh(state: DesktopWebState): void {
  if (!state.async_polling_required || !state.navigation_loading) {
    return;
  }
  window.setTimeout(() => {
    if (currentState?.async_polling_required && currentState.navigation_loading) {
      void refresh();
    }
  }, 80);
}

function shouldDeferAutoRefresh(): boolean {
  return interactionLifecycle.active;
}

function reportError(message: string): void {
  const error = humanizeError(message);
  if (currentState) {
    uiState.recoverableError = error;
    acceptState(currentState, true);
    return;
  }
  appRoot.innerHTML = `
    <div class="fatal">
      <h1>moyAI Desktop</h1>
      <h2>${escapeHtml(error.title)}</h2>
      <p>${escapeHtml(error.hint)}</p>
      <details>
        <summary>技術詳細</summary>
        <pre>${escapeHtml(error.details)}</pre>
      </details>
    </div>`;
}

function renderRecoverableError(): string {
  const error = uiState.recoverableError;
  if (!error) return "";
  return `
    <aside class="ui-error-notice" role="status" aria-live="polite">
      <div>
        <strong>${escapeHtml(error.title)}</strong>
        <span>${escapeHtml(error.hint)}</span>
        ${error.details.trim().length > 0 ? `<details data-details-key="recoverable-error-details"><summary data-focus-key="recoverable-error-summary">技術詳細</summary><pre>${escapeHtml(error.details)}</pre></details>` : ""}
      </div>
      <button class="icon-button" data-action="dismiss-ui-error" title="閉じる" aria-label="閉じる">×</button>
    </aside>`;
}

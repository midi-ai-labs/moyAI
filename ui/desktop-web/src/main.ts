import { getCurrentWindow } from "@tauri-apps/api/window";
import { command } from "./api";
import { acceptHubProjection, hubPresentation } from "./hub_state.ts";
import { acceptDeviceNetworkProjection, deviceNetworkPresentation } from "./device_network_state.ts";
import { synchronizeDeviceNetworkControls } from "./device_network_dom.ts";
import { refreshDeviceNetworkJobs } from "./device_network_actions.ts";
import { acceptPublishProjection, publishPresentation } from "./mcp_publish_state.ts";
import { clearPublishSecret, synchronizePublishControlValues } from "./mcp_publish_dom.ts";
import { refreshPublishJobs } from "./mcp_publish_actions.ts";
import { clearMcpPeerToken, mcpPeerPresentation, refreshMcpPeers } from "./mcp_peer.ts";
import { synchronizeHubControlValues } from "./hub_dom.ts";
import { cancelRunCommand, interruptSessionCommand } from "./stop_contract";
import { agentActivityRowsChanged, selectedAgentActivityChanged } from "./agent_activity";
import {
  beginCommandPaletteInsertion,
  CommandPaletteInsertionAsyncOwner,
  commandPaletteInsertionFocusCandidates,
  commandPaletteInsertionFocusStillCurrent,
  dispatchCommandPaletteInsertion,
  settleCommandPaletteInsertion,
} from "./command_palette_insertion";
import {
  dispatchNewSessionMutation,
  mutationStartsNewSession,
} from "./new_session_mutation";
import {
  beginAttachmentFocusContinuation,
  attachmentFocusCandidates,
  reconcileAttachmentFocusContinuation,
  type AttachmentFocusDecision,
} from "./attachment_focus_continuation";
import {
  agentExecutionPrependOwnerMatches,
  beginAgentExecutionPrependContinuation,
  agentExecutionPrependFocusCandidates,
  reconcileAgentExecutionPrependContinuation,
  restoreAgentExecutionPrependViewport,
} from "./agent_execution_prepend_continuation";
import {
  acknowledgePendingHistoryPrepend,
  advancePendingHistoryPrepend,
  captureViewportAnchor,
  createPendingHistoryPrepend,
  historyPrependFocusCandidates,
  historyPrependFocusContinuationIsCurrent,
  pinResolvedThreadToEnd,
  rejectPendingHistoryPrepend,
  restoreViewportAnchor,
  runCompletionEdge,
  shouldRevealThreadEnd,
  syncResolvedInactiveThreadViewport,
  ThreadTailFollowAffinity,
  type PendingHistoryPrepend,
} from "./history_navigation";
import { commandConflictState, commandInternalState } from "./command_error";
import type { ActionContext } from "./actions";
import {
  focusOverlayPrimary,
  installGlobalKeyboardShortcuts,
  prepareConfigMutation,
  prepareConfigSnapshot,
  wireEvents,
} from "./events";
import {
  PostRenderFocusArbiter,
  animationFrameFocusScheduler,
  type FocusArbiterResult,
  type PostRenderFocusIntent,
} from "./focus_arbiter";
import {
  renderDesktopMarkup,
  renderStartupSplash,
  synchronizeTitlebarMenuState,
} from "./render";
import type {
  AgentExecutionProjection,
  CommandPaletteInsertionResult,
  DesktopViewState,
  DesktopWebState,
  SideChatCatalogResult,
} from "./types";
import {
  beginAgentExecutionLoad,
  beginPreviousAgentExecutionPageLoad,
  createUiLocalState,
  doclingReadinessRequestPending,
  agentExecutionRequestNeedsRefresh,
  agentExecutionSnapshotOwnerIdentity,
  failAgentExecutionLoad,
  finishAgentExecutionLoad,
  reconcileAgentPaneState,
  selectedAgentExecution,
  sessionSettingsDraftFromProjection,
  sessionSettingsMutationAvailability,
  sideChatCatalogLoadOpen,
  sideChatCatalogViewForState,
  sideChatDeleteConfirmationStillTargets,
  sideChatDraftForState,
  sideChatMutationPending,
  sideChatOperationsOpen,
  sideChatOwnerSessionId,
  shouldPreserveAgentExecutionSnapshots,
  type SessionInteractionSnapshot,
} from "./ui_state";
import {
  initialSetupDiffSummary,
  initialSetupFinishPending,
  reconcileInitialSetupOwner,
} from "./initial_setup_state";
import {
  initialSetupAuxiliaryPendingKind,
  initialSetupDoclingReadinessVisible,
  initialSetupImportedSourcePath,
  reconcileInitialSetupAuxiliaryState,
} from "./initial_setup_auxiliary_state";
import {
  clearSessionSettings,
  reconcileSessionSettings,
  sameSessionSettingsRootOwner,
  sameSessionSettingsTarget,
  sessionSettingsMutationPending,
} from "./session_settings_state";
import {
  InteractionLifecycle,
  type InteractionRelease,
  installInteractionEventGate,
} from "./interaction_lifecycle";
import {
  appliedProjectionRevision,
  deferredProjectionCandidatePreferred,
  projectionUpdateAccepted,
} from "./projection_state";
import {
  createDesktopRenderModel,
  desktopRenderRequired,
  type DesktopRenderModel,
} from "./render_projection";
import { isRegularModalOverlay, localModalIdentity, modalIdentity, modalIsOpen } from "./modal_state";
import { autoRefreshAllowed, createSnapshotRefresh, installRuntimePolling, runtimePollingRequired } from "./polling_state";
import {
  reconcileTaskActivityAnimationEpoch,
  taskActivityAnimationDelay,
} from "./task_activity_indicator";
import {
  quickChatDeleteFocusCandidates,
  reconcileQuickChatDeleteFocusContinuation,
} from "./quick_chat_delete_focus_continuation";
import { restoreScrollPosition } from "./scroll_state";
import {
  beginNewChatFocusContinuation,
  beginNewProjectSessionFocusContinuation,
  captureSessionInteractionSnapshot,
  newSessionRetryFocusTarget,
  reconcileNewSessionFocusContinuation,
  restoreSessionPromptInteraction,
  restoreSessionThreadInteraction,
  sameNewSessionFocusRequest,
  settledNewSessionFocusContinuationIsCurrent,
  sessionPromptInteractionForRender,
  sessionSelectionRequestsComposerFocus,
  type NewSessionFocusContinuation,
  type SettledNewSessionFocusContinuation,
} from "./session_interaction_state";
import {
  createRefreshPromptFocusIntent,
  refreshPromptFocusContinuationAccepted,
  retainConnectedMainPrompt,
  takePendingRefreshPromptFocus,
  type RefreshPromptFocusContinuation,
} from "./main_prompt_continuity";
import {
  applyTitlebarMenuRovingTabIndex,
  titlebarMenuFromOverlay,
  titlebarMenuTriggerAction,
  titlebarMenuUsesRovingFocus,
} from "./titlebar_interaction";
import {
  settingsActionFocusCandidates,
  settingsActionFocusStillTargets,
  restoreSettingsActionViewport,
  type SettingsActionFocusContinuation,
  settingsCloseTargetStillMatches,
  settingsRecoverableErrorOwnerIdentity,
  sameSettingsSurface,
  settingsSurfaceIdentity,
  shouldRetainConnectedSettingsSurface,
  synchronizeRetainedSettingsSurface,
} from "./settings_surface";
import {
  mainRunFocusSurface,
  reconcileMainRunFocusContinuation,
} from "./run_focus_continuation";
import {
  reconcileSideChatFocusContinuation,
  sideChatFocusSurface,
  sideChatFocusTargetStillMatches,
} from "./side_chat_focus_continuation";
import {
  beginPermissionDecision,
  beginPermissionStop,
  failPermissionDecision,
  finishLocalDecision,
  finishPermissionDecision,
  permissionDecisionShouldFocusComposer,
  permissionDecisionResponseAccepted,
  reconcilePermissionDecision,
  recoverPermissionDecisionFromConflict,
  type PermissionReviewDecision,
} from "./decision_state";
import { escapeHtml, humanizeError } from "./utils";
import {
  configMutationPending,
} from "./config_mutation";
import { rowMutationTargetStillMatches } from "./row_target";
import {
  acknowledgeDraftMutation,
  captureDraftMutation,
  type DraftMutationSnapshot,
  mutationAdmissionOpen,
  mutationChangesConfigOwner,
  mutationStartsRun,
  operationInvalidatesComposer,
  composerOwner,
  composerSessionOwner,
  projectViewState,
  reconcileUiDrafts,
  rejectDraftMutation,
} from "./view_state";
import "./styles.css";
import "./hub_surface.css";
import "./device_network_surface.css";
import "./mcp_publish_surface.css";

const app = document.querySelector<HTMLDivElement>("#app");
const desktopWindow = getCurrentWindow();
let currentState: DesktopWebState | null = null;
let lastRenderedState: DesktopViewState | null = null;
let lastRenderedModel: DesktopRenderModel | null = null;
const refresh = createSnapshotRefresh(async () => {
  try {
    acceptState(await command<DesktopWebState>("desktop_state"), false);
    await refreshPublishJobs(eventContext);
    await refreshDeviceNetworkJobs(eventContext);
  } catch (error) {
    reportError(error);
  }
});
let previousSessionKey = "";
let lastRenderedLocalModalIdentity: string | null = null;
let splashDismissed = false;
let splashTimer: number | null = null;
const splashStartedAt = performance.now();
const SPLASH_MIN_VISIBLE_MS = 5000;
const THREAD_END_THRESHOLD_PX = 96;
const uiState = createUiLocalState();
let pendingHistoryPrepend: PendingHistoryPrepend | null = null;
let nextHistoryPrependGeneration = 1;
let lastRenderedAgentExecutionOwner: string | null = null;
const threadTailFollow = new ThreadTailFollowAffinity();
let threadEndRevealGeneration = 0;
const commandPaletteInsertionOwner = new CommandPaletteInsertionAsyncOwner();
let commandPaletteInsertionInteractionGeneration = 0n;
let nextCommandPaletteInsertionRequestId = 1;
let composerFocusInteractionGeneration = 0n;
let postRenderFocusInteractionEpoch = 0n;
let pendingNewSessionInitiatingFocus: NewSessionFocusContinuation | null = null;
let pendingNewSessionPromptFocus: SettledNewSessionFocusContinuation | null = null;

interface StateUpdate {
  state: DesktopWebState;
  forceRender: boolean;
  mutationName: string | null;
  scheduleNavigation: boolean;
  sequence: number;
  draftSnapshot: DraftMutationSnapshot | null;
  refreshPromptFocusContinuation: RefreshPromptFocusContinuation | null;
  settingsAction: SettingsActionFocusContinuation | null;
}

const interactionLifecycle = new InteractionLifecycle<StateUpdate>((current, candidate) =>
  deferredProjectionCandidatePreferred(
    current.state.projection_revision,
    candidate.state.projection_revision,
    current.sequence,
    candidate.sequence,
  ),
);
const postRenderFocusArbiter = new PostRenderFocusArbiter(
  animationFrameFocusScheduler(window),
  {
    currentRenderCommit: () => threadEndRevealGeneration,
    currentInteractionEpoch: () => postRenderFocusInteractionEpoch,
    interactionActive: () => interactionLifecycle.active,
    activeElement: () => document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null,
    bodyElement: () => document.body,
    documentElement: () => document.documentElement,
  },
);
let nextStateSequence = 1;
let lastAppliedStateSequence = 0;
let lastAppliedProjectionRevision = "0";
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
  getRenderModel: () => currentState
    ? buildDesktopRenderModel(projectViewState(currentState, uiState))
    : null,
  acceptProjection: (state: DesktopWebState, forceRender = true, settingsAction) => acceptState(state, forceRender, null, false, null, null, settingsAction ?? null),
  waitForInteractionIdle: () => interactionLifecycle.whenIdle(),
  rerender: () => {
    if (currentState) acceptState(currentState, true);
  },
  mutate,
  insertCommandFromPalette,
  invalidateCommandPaletteInsertion,
  recoverCommandConflict,
  reportError,
  prepareConfigMutation: (target) => prepareConfigMutation(eventContext, target),
  prepareConfigSnapshot: (target) => prepareConfigSnapshot(eventContext, target),
  submitPermissionDecision,
  submitRunStop,
  setWindowMaximized,
  loadAgentExecution,
  loadPreviousAgentExecutionPage,
  loadSideChatModels: (args) => command<SideChatCatalogResult>("load_side_chat_models", args),
  jumpToHistoryAnchor,
};

installInteractionEventGate({
  documentTarget: document,
  windowTarget: window,
  appRoot,
  lifecycle: interactionLifecycle,
  finish: finishInteraction,
});
installComposerFocusInteractionInvalidation();
installWindowMaximizedSync();
void refresh();
installRuntimePolling(window, document, () => Boolean(
    currentState
    && (currentState.overlay === "mcp_publish" || currentState.overlay === "hub" || uiState.deviceNetwork.projection?.enrollment === "active" || runtimePollingRequired(currentState.async_polling_required, uiState.runStartMutationPending, uiState.hub.projection, uiState.mcpPublish.projection))
    && shouldAutoRefresh(currentState)
), refresh);

installGlobalKeyboardShortcuts(eventContext);

async function mutate(
  name: string,
  args?: Record<string, unknown>,
  activationSource?: "shortcut",
): Promise<void> {
  const startsRun = mutationStartsRun(name);
  const changesConfigOwner = mutationChangesConfigOwner(name);
  const refreshPromptFocusContinuation = takePendingRefreshPromptFocus(uiState, name);
  if (!mutationAdmissionOpen(uiState, name)) return;
  if (
    mutationStartsNewSession(name)
    && (!currentState || !projectViewState(currentState, uiState).navigation_admission_open)
  ) {
    return;
  }
  const activeElement = document.activeElement instanceof HTMLElement
    ? document.activeElement
    : null;
  const requestOwner = currentState ? composerOwner(currentState) : null;
  const newSessionFocusContinuation = name === "new_chat"
    ? beginNewChatFocusContinuation(
      activationSource === "shortcut" ? "shortcut" : "element",
      activeElement,
      composerFocusInteractionGeneration,
      activeElement?.dataset.action ?? null,
      activeElement?.dataset.focusKey ?? null,
      requestOwner,
    )
    : name === "new_project_session"
      ? beginNewProjectSessionFocusContinuation(
        activeElement,
        composerFocusInteractionGeneration,
        activeElement?.dataset.action ?? null,
        activeElement?.dataset.focusKey ?? null,
        requestOwner,
        newProjectSessionTargetProjectId(currentState, args),
      )
      : null;
  const attachmentFocusContinuation = currentState
    ? beginAttachmentFocusContinuation(projectViewState(currentState, uiState), name, args)
    : null;
  if (attachmentFocusContinuation) {
    uiState.attachmentFocusContinuation = attachmentFocusContinuation;
  }
  let historyPrependRequest: PendingHistoryPrepend | null = null;
  if (name === "load_previous_turn_page") {
    historyPrependRequest = beginHistoryPrepend();
    if (!historyPrependRequest) return;
  }
  if (startsRun) {
    if (currentState) {
      threadTailFollow.armRun({
        workspacePath: currentState.run_target.workspacePath,
        sessionId: currentState.run_target.sessionId,
        runtimeOwnerToken: currentState.run_target.runtimeOwnerToken,
      });
    }
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
    const acceptMutationResponse = (state: DesktopWebState): void => {
      if (
        historyPrependRequest
        && pendingHistoryPrepend?.generation === historyPrependRequest.generation
      ) {
        pendingHistoryPrepend = acknowledgePendingHistoryPrepend(pendingHistoryPrepend);
      }
      acknowledgeDraftMutation(uiState, state, name, draftSnapshot);
      acceptState(
        state,
        true,
        name,
        true,
        draftSnapshot,
        refreshPromptFocusContinuation,
      );
      if (currentState && currentState !== state) acceptState(currentState, true);
    };
    if (mutationStartsNewSession(name)) {
      pendingNewSessionInitiatingFocus = newSessionFocusContinuation;
      pendingNewSessionPromptFocus = null;
      const dispatched = await dispatchNewSessionMutation(
        uiState,
        name,
        interactionLifecycle,
        () => {
          if (currentState) acceptState(currentState, true);
        },
        () => command<DesktopWebState>(name, args),
        acceptMutationResponse,
        () => {
          if (currentState) acceptState(currentState, true);
        },
      );
      if (!dispatched) {
        cancelNewSessionFocusRequest(newSessionFocusContinuation);
      }
    } else {
      acceptMutationResponse(
        name === "interrupt_session"
          ? await interruptSessionCommand<DesktopWebState>(args ?? {})
          : await command<DesktopWebState>(name, args),
      );
    }
  } catch (error) {
    cancelNewSessionFocusRequest(newSessionFocusContinuation);
    if (uiState.attachmentFocusContinuation === attachmentFocusContinuation) {
      uiState.attachmentFocusContinuation = null;
    }
    if (historyPrependRequest) {
      pendingHistoryPrepend = rejectPendingHistoryPrepend(
        pendingHistoryPrepend,
        historyPrependRequest.generation,
      );
      scheduleInactiveThreadViewportSync();
    }
    if (startsRun) threadTailFollow.cancelRun();
    rejectDraftMutation(uiState, name, draftSnapshot);
    const conflictRecovered = recoverCommandConflict(error, draftSnapshot);
    if (!conflictRecovered) reportError(error);
    scheduleNewSessionRetryFocus(newSessionFocusContinuation);
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

function invalidateCommandPaletteInsertion(): void {
  commandPaletteInsertionInteractionGeneration += 1n;
}

async function insertCommandFromPalette(
  state: DesktopViewState,
  index: number,
): Promise<void> {
  const projection = currentState;
  if (
    !projection
    || state.projection_revision !== projection.projection_revision
    || state.command_rows[index]?.path !== projection.command_rows[index]?.path
  ) {
    return;
  }
  const active = document.activeElement;
  const focusOwner = active instanceof Element
    ? active.closest<HTMLElement>('[data-action="insert-command"]')
    : null;
  if (!focusOwner || focusOwner.dataset.index !== String(index)) return;
  const prompt = document.querySelector<HTMLTextAreaElement>("#prompt");
  if (!prompt) return;
  const request = beginCommandPaletteInsertion(
    projection,
    uiState.drafts,
    prompt,
    focusOwner,
    index,
    nextCommandPaletteInsertionRequestId++,
    commandPaletteInsertionInteractionGeneration,
  );
  if (!request) return;

  try {
    const dispatched = await dispatchCommandPaletteInsertion(
      commandPaletteInsertionOwner,
      request,
      interactionLifecycle,
      () => command<CommandPaletteInsertionResult>("insert_command", {
        index,
        expectedTarget: request.expectedTarget,
        expectedDraftTarget: request.expectedDraftTarget,
      }),
      (response) => {
        const current = currentState;
        return current
          ? settleCommandPaletteInsertion({
              request,
              activeRequest: commandPaletteInsertionOwner.activeRequest,
              interactionGeneration: commandPaletteInsertionInteractionGeneration,
              interactionIdle: !interactionLifecycle.active,
              projectionAccepted: projectionUpdateAccepted(
                lastAppliedProjectionRevision,
                response.state.projection_revision,
                response.state === currentState,
              ),
              currentState: current,
              response,
              drafts: uiState.drafts,
              prompt: document.querySelector<HTMLTextAreaElement>("#prompt"),
              focusOwned: document.activeElement === request.focusOwner,
            })
          : null;
      },
    );
    if (!dispatched) return;
    const { response, settlement } = dispatched;
    if (settlement) {
      uiState.drafts.prompt = settlement.value;
      uiState.drafts.composerRevision = settlement.focusContinuation.composerRevision;
    }
    acceptState(response.state, true, "insert_command", true);
    if (!settlement || currentState !== response.state) return;
    const continuation = settlement.focusContinuation;
    const paletteFocusOwners = Array.from(
      document.querySelectorAll<HTMLElement>('[data-action="show-command-palette"]'),
    );
    schedulePostRenderFocus([{
      source: "command-palette",
      priority: "explicit-transfer",
      claim: { kind: "yield-from", owners: paletteFocusOwners },
      candidates: commandPaletteInsertionFocusCandidates(document, continuation),
      isCurrent: () => Boolean(
        currentState
        && commandPaletteInsertionFocusStillCurrent(
          continuation,
          commandPaletteInsertionInteractionGeneration,
          currentState,
          uiState.drafts,
        )
      ),
    }]);
  } catch (error) {
    if (!recoverCommandConflict(error)) reportError(error);
  }
}

function cancelNewSessionFocusRequest(request: NewSessionFocusContinuation | null): void {
  if (request === null) return;
  request.requestToken.rejected = true;
  if (sameNewSessionFocusRequest(pendingNewSessionInitiatingFocus, request)) {
    pendingNewSessionInitiatingFocus = null;
  }
  if (sameNewSessionFocusRequest(pendingNewSessionPromptFocus, request)) {
    pendingNewSessionPromptFocus = null;
  }
}

function scheduleNewSessionRetryFocus(request: NewSessionFocusContinuation | null): void {
  if (request === null) return;
  schedulePostRenderFocus([{
    source: "new-session",
    priority: "explicit-transfer",
    claim: { kind: "unowned" },
    candidates: [{
      resolve: () => {
        if (!currentState) return null;
        const initiatingElement = request.initiatingElement instanceof HTMLElement
          && request.initiatingElement !== document.body
          && request.initiatingElement !== document.documentElement
          && request.initiatingElement.isConnected
          && !request.initiatingElement.matches(":disabled")
            ? request.initiatingElement
            : null;
        const exactRouteTarget = request.activeAction && request.activeFocusKey
          ? document.querySelector<HTMLElement>(
            `[data-action="${CSS.escape(request.activeAction)}"]`
            + `[data-focus-key="${CSS.escape(request.activeFocusKey)}"]:not(:disabled)`,
          )
          : null;
        const target = newSessionRetryFocusTarget(request, {
          currentOwner: composerOwner(currentState),
          currentInteractionGeneration: composerFocusInteractionGeneration,
          focusUnclaimed: true,
          initiatingElementConnected: initiatingElement !== null,
          exactRouteTarget,
          promptTarget: document.querySelector<HTMLTextAreaElement>("#prompt:not(:disabled)"),
        });
        return target instanceof HTMLElement ? target : null;
      },
    }],
    isCurrent: () => Boolean(
      currentState
      && request.requestToken.rejected
      && request.requestOwner === composerOwner(currentState)
      && request.interactionGeneration === composerFocusInteractionGeneration
    ),
  }]);
}

function newProjectSessionTargetProjectId(
  state: DesktopWebState | null,
  args: Record<string, unknown> | undefined,
): string | null {
  if (!state || !args || typeof args.index !== "number" || !Number.isInteger(args.index)) return null;
  const expectedTarget = args.expectedTarget;
  if (!expectedTarget || typeof expectedTarget !== "object") return null;
  const rowId = (expectedTarget as { rowId?: unknown }).rowId;
  if (typeof rowId !== "string" || rowId.length === 0) return null;
  return state.project_rows[args.index]?.project_id === rowId ? rowId : null;
}

async function loadAgentExecution(state: DesktopWebState, agentPath: string): Promise<void> {
  const row = state.agent_activity_rows.find((candidate) => candidate.agent_path === agentPath);
  if (!row || state.draft_target.sessionId === null) return;
  const request = beginAgentExecutionLoad(uiState, state, row);
  if (currentState) acceptState(currentState, true);
  try {
    const projection = await command<AgentExecutionProjection>("load_agent_execution", {
      expectedTarget: request.expectedTarget,
    });
    if (finishAgentExecutionLoad(uiState, request, projection)) {
      renderAgentExecutionSettlement(request);
    }
  } catch (error) {
    recoverCommandConflict(error);
    const message = humanizeError(error);
    if (failAgentExecutionLoad(uiState, request, `${message.title}: ${message.hint}`)) {
      renderAgentExecutionSettlement(request);
    }
  }
}

async function loadPreviousAgentExecutionPage(
  state: DesktopWebState,
  agentPath: string,
): Promise<void> {
  const row = state.agent_activity_rows.find((candidate) => candidate.agent_path === agentPath);
  if (!row || state.draft_target.sessionId === null) return;
  const request = beginPreviousAgentExecutionPageLoad(uiState, state, row);
  if (!request || request.expectedOffset === null || request.expectedEnd === null) return;
  uiState.agentExecutionPrependContinuation = beginAgentExecutionPrependContinuation(
    document,
    request,
  );
  if (currentState) acceptState(currentState, true);
  try {
    const projection = await command<AgentExecutionProjection>(
      "load_previous_agent_execution_page",
      {
        expectedTarget: request.expectedTarget,
        expectedOffset: request.expectedOffset,
        expectedEnd: request.expectedEnd,
      },
    );
    if (finishAgentExecutionLoad(uiState, request, projection)) {
      renderAgentExecutionSettlement(request);
    }
  } catch (error) {
    recoverCommandConflict(error);
    const message = humanizeError(error);
    if (failAgentExecutionLoad(uiState, request, `${message.title}: ${message.hint}`)) {
      renderAgentExecutionSettlement(request);
    }
  }
}

function renderAgentExecutionSettlement(request: ReturnType<typeof beginAgentExecutionLoad>): void {
  if (!currentState) return;
  const settledState = currentState;
  const refreshAfterSettlement = agentExecutionRequestNeedsRefresh(request, settledState);
  acceptState(settledState, true);
  if (
    refreshAfterSettlement
    && uiState.selectedAgentPath === request.expectedTarget.agentPath
    && uiState.agentExecutionTransaction.active === null
  ) {
    void loadAgentExecution(settledState, request.expectedTarget.agentPath);
  }
}

function beginHistoryPrepend(): PendingHistoryPrepend | null {
  if (!currentState || pendingHistoryPrepend) return null;
  const request = createPendingHistoryPrepend(
    currentState,
    nextHistoryPrependGeneration,
    document,
  );
  if (!request) return null;
  nextHistoryPrependGeneration += 1;
  pendingHistoryPrepend = request;
  noteUserThreadScrollAway();
  return request;
}

function jumpToHistoryAnchor(anchorId: string): void {
  if (!anchorId) return;
  const thread = document.querySelector<HTMLElement>("#thread");
  const target = thread?.querySelector<HTMLElement>(
    `[data-history-anchor="${CSS.escape(anchorId)}"]`,
  );
  if (!thread || !target) return;
  noteUserThreadScrollAway();
  const top = thread.scrollTop
    + target.getBoundingClientRect().top
    - thread.getBoundingClientRect().top
    - 18;
  const targetTop = Math.min(
    Math.max(0, top),
    Math.max(0, thread.scrollHeight - thread.clientHeight),
  );
  const alreadyAtTarget = Math.abs(thread.scrollTop - targetTop) <= 1;
  thread.scrollTo({ top: targetTop, behavior: "smooth" });
  if (alreadyAtTarget) scheduleInactiveThreadViewportSync();
}

function recoverCommandConflict(
  error: unknown,
  draftSnapshot: DraftMutationSnapshot | null = null,
): boolean {
  const state = commandConflictState(error);
  if (!state) return false;
  acceptState(state, true, "command_conflict", true, draftSnapshot);
  return true;
}

/**
 * Accepts one ordered Rust projection. Visual changes are detected from the
 * reconciled DesktopRenderModel; `forceRender` only requests imperative
 * render-phase work for an otherwise identical model.
 */
function acceptState(
  state: DesktopWebState,
  forceRender: boolean,
  mutationName: string | null = null,
  scheduleNavigation = false,
  draftSnapshot: DraftMutationSnapshot | null = null,
  refreshPromptFocusContinuation: RefreshPromptFocusContinuation | null = null,
  settingsAction: SettingsActionFocusContinuation | null = null,
): void {
  const update: StateUpdate = {
    state,
    forceRender,
    mutationName,
    scheduleNavigation,
    sequence: nextStateSequence++,
    draftSnapshot,
    refreshPromptFocusContinuation,
    settingsAction,
  };
  applyStateUpdate(update);
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
  if (
    interactionLifecycle.defer(
      { ...update, forceRender: true },
      update.state === currentState,
      update.forceRender,
    )
  ) return;
  const previousProjection = currentState;
  if (update.state !== previousProjection && update.state.hub) {
    acceptHubProjection(uiState.hub, update.state.hub);
  }
  if (update.state !== previousProjection && update.state.device_network) {
    acceptDeviceNetworkProjection(uiState.deviceNetwork, update.state.device_network);
  }
  if (previousProjection?.overlay === "hub" && update.state.overlay !== "hub") ++uiState.deviceNetwork.jobsSerial;
  if (update.state !== previousProjection && update.state.mcp_publish) {
    acceptPublishProjection(uiState.mcpPublish, update.state.mcp_publish);
  }
  if (previousProjection?.overlay === "mcp_publish" && update.state.overlay !== "mcp_publish") {
    clearPublishSecret();
    ++uiState.mcpPublish.jobsSerial;
  }
  if (previousProjection?.overlay === "config" && update.state.overlay !== "config") {
    clearMcpPeerToken();
    ++uiState.mcpPeers.serial;
    uiState.mcpPeers.pending = null;
  }
  reconcileUiDrafts(uiState, previousProjection, update.state, update.draftSnapshot);
  reconcileSettingsFlowState(update.state);
  reconcileAgentPaneState(uiState, update.state);
  const viewState = projectViewState(update.state, uiState);
  reconcileRecoverableErrorOwner(viewState);
  const attachmentFocusDecision = reconcileUiLocalState(
    lastRenderedState,
    viewState,
    update.mutationName,
  );
  const renderModel = buildDesktopRenderModel(viewState);
  currentState = update.state;
  lastAppliedStateSequence = update.sequence;
  lastAppliedProjectionRevision = appliedProjectionRevision(
    lastAppliedProjectionRevision,
    update.state.projection_revision,
  );
  if (desktopRenderRequired(lastRenderedModel, renderModel, update.forceRender)) {
    renderCommitted(
      renderModel,
      update.mutationName,
      update.refreshPromptFocusContinuation,
      attachmentFocusDecision,
      update.settingsAction,
    );
  }
  if (update.scheduleNavigation) {
    scheduleNavigationRefresh(update.state);
  }
  if (previousProjection?.overlay !== "config" && update.state.overlay === "config") void refreshMcpPeers(eventContext);
}

function buildDesktopRenderModel(state: DesktopViewState): DesktopRenderModel {
  const sideChatDraft = sideChatDraftForState(uiState, state);
  const sideChatOwner = sideChatOwnerSessionId(state);
  const sideChatDeleteConfirmation = sideChatDeleteConfirmationStillTargets(
    uiState.sideChatDeleteConfirmation,
    state,
  ) ? uiState.sideChatDeleteConfirmation : null;
  const configDraftValues = state.config_fields.map((field) => ({
    key: field.key,
    text: field.value,
  }));
  const configBaselineValues = state.config_fields.map((field) => ({
    key: field.key,
    text: uiState.configDraftBaselineValues.get(field.key) ?? field.value,
  }));
  return createDesktopRenderModel(state, {
    hub: hubPresentation(uiState.hub),
    deviceNetwork: deviceNetworkPresentation(uiState.deviceNetwork),
    mcpPublish: publishPresentation(uiState.mcpPublish),
    mcpPeers: mcpPeerPresentation(uiState.mcpPeers),
    artifactPane: {
      collapsed: uiState.artifactPaneCollapsed,
      mode: uiState.artifactPaneMode,
      selectedAgentPath: uiState.selectedAgentPath,
      selectedAgentExecution: selectedAgentExecution(uiState, state),
    },
    attachmentTrayOpen: uiState.attachmentTrayOpen,
    configMutationPending: configMutationPending(uiState)
      || (
        state.overlay === "config"
        && uiState.localConfirmationDecisionPending
        && uiState.pendingLocalConfirmation === null
      ),
    doclingReadinessRequestPending: doclingReadinessRequestPending(uiState),
    initialSetup: {
      step: uiState.initialSetup.step,
      finishPending: initialSetupFinishPending(uiState.initialSetup),
      auxiliaryPendingKind: initialSetupAuxiliaryPendingKind(
        uiState.initialSetupAuxiliary,
      ),
      importedSourcePath: initialSetupImportedSourcePath(
        uiState.initialSetupAuxiliary,
        state.startup.setup_target,
        state.config_target,
      ),
      doclingReadinessVisible: initialSetupDoclingReadinessVisible(
        uiState.initialSetupAuxiliary,
        state.startup.setup_target,
        state.config_target,
        uiState.configDraftRevision,
        state.docling_readiness.endpoint,
      ),
      differences: initialSetupDiffSummary(
        state.config_fields,
        configBaselineValues,
        configDraftValues,
      ),
    },
    sessionSettings: {
      draft: uiState.sessionSettings.draft,
      dirty: uiState.sessionSettings.dirty,
      validation: uiState.sessionSettings.validation,
      mutationPending: sessionSettingsMutationPending(uiState.sessionSettings),
      availability: sessionSettingsMutationAvailability(
        uiState.sessionSettings,
        state.session_settings,
      ),
    },
    sideChat: {
      draft: sideChatDraft?.text ?? "",
      pendingQuote: sideChatDraft?.pendingQuote ?? null,
      catalog: sideChatCatalogViewForState(uiState, state),
      catalogLoadEnabled: sideChatCatalogLoadOpen(uiState, state),
      mutationPending: sideChatMutationPending(uiState, sideChatOwner),
      operationsOpen: sideChatOperationsOpen(uiState),
      deleteConfirmation: sideChatDeleteConfirmation,
    },
    modal: {
      localConfirmation: uiState.pendingLocalConfirmation,
      localDecisionPending: uiState.localConfirmationDecisionPending,
      localDecisionError: uiState.localConfirmationDecisionError,
      permissionDecision: uiState.permissionDecision,
    },
    recoverableError: uiState.recoverableErrorOwner === settingsRecoverableErrorOwnerIdentity(
      state,
      uiState.initialSetup.step,
    ) ? uiState.recoverableError : null,
    windowMaximized: uiState.windowMaximized,
  });
}

function reconcileSettingsFlowState(state: DesktopWebState): void {
  const setupTarget = state.startup.setup_target;
  reconcileInitialSetupAuxiliaryState(
    uiState.initialSetupAuxiliary,
    setupTarget,
    state.config_target,
  );
  if (state.startup.initial_setup_required && setupTarget !== null) {
    reconcileInitialSetupOwner(uiState.initialSetup, setupTarget);
  }

  const sessionProjection = state.session_settings;
  if (
    state.overlay === "session_settings"
    && sessionProjection.available
    && sessionProjection.target !== null
  ) {
    reconcileSessionSettings(
      uiState.sessionSettings,
      sessionProjection.target,
      sessionSettingsDraftFromProjection(sessionProjection),
    );
  } else if (
    state.overlay !== "session_settings"
    && (
      uiState.sessionSettings.owner !== null
      || uiState.sessionSettings.draft !== null
      || uiState.sessionSettings.activeMutation !== null
    )
  ) {
    clearSessionSettings(uiState.sessionSettings);
  }

}

function reconcileRecoverableErrorOwner(state: DesktopViewState): void {
  if (
    uiState.recoverableError !== null
    && uiState.recoverableErrorOwner !== settingsRecoverableErrorOwnerIdentity(
      state,
      uiState.initialSetup.step,
    )
  ) {
    uiState.recoverableError = null;
    uiState.recoverableErrorOwner = null;
  }
}

function renderCommitted(
  model: DesktopRenderModel,
  mutationName: string | null,
  refreshPromptFocusContinuation: RefreshPromptFocusContinuation | null,
  attachmentFocusDecision: AttachmentFocusDecision,
  settingsAction: SettingsActionFocusContinuation | null,
): void {
  const state = model.view;
  const revealGeneration = ++threadEndRevealGeneration;
  const postRenderFocusIntents: PostRenderFocusIntent[] = [];
  const postRenderFocusResultHandlers: Array<(result: FocusArbiterResult) => void> = [];
  const renderNowMs = performance.now();
  const elapsedSplashMs = renderNowMs - splashStartedAt;
  if (!splashDismissed && shouldShowSplash(elapsedSplashMs)) {
    appRoot.innerHTML = renderStartupSplash(state, elapsedSplashMs, SPLASH_MIN_VISIBLE_MS);
    scheduleSplashReveal(elapsedSplashMs);
    lastRenderedState = state;
    lastRenderedModel = model;
    lastRenderedAgentExecutionOwner = null;
    return;
  }
  if (!splashDismissed) {
    splashDismissed = true;
    if (splashTimer !== null) {
      window.clearTimeout(splashTimer);
      splashTimer = null;
    }
  }
  uiState.taskActivityAnimationEpoch = reconcileTaskActivityAnimationEpoch(
    uiState.taskActivityAnimationEpoch,
    {
      activityState: state.task_activity_state,
      workspacePath: state.run_target.workspacePath,
      sessionId: state.run_target.sessionId,
      runtimeOwnerToken: state.run_target.runtimeOwnerToken,
      runStartMutationPending: uiState.runStartMutationPending,
      nowMs: renderNowMs,
    },
  );
  const taskActivityDelay = taskActivityAnimationDelay(
    uiState.taskActivityAnimationEpoch,
    renderNowMs,
  );
  const previous = lastRenderedState;
  const runFocusDecision = reconcileMainRunFocusContinuation(
    uiState.mainRunFocusContinuation,
    previous,
    state,
    mainRunFocusSurface(document),
  );
  uiState.mainRunFocusContinuation = runFocusDecision.continuation;
  const nextAgentExecutionOwner = agentExecutionSnapshotOwnerIdentity(
    state,
    uiState.selectedAgentPath,
  );
  const selectedExecution = model.local.artifactPane.selectedAgentExecution;
  const agentExecutionPrependDecision = reconcileAgentExecutionPrependContinuation(
    uiState.agentExecutionPrependContinuation,
    state,
    uiState.selectedAgentPath,
    selectedExecution,
  );
  uiState.agentExecutionPrependContinuation = agentExecutionPrependDecision.continuation;
  const localConfirmationPending = model.local.modal.localConfirmation !== null;
  const sideChatDeleteTarget = model.local.sideChat.deleteConfirmation;
  const renderedLocalModalIdentity = localModalIdentity(localConfirmationPending, sideChatDeleteTarget);
  const localModalPending = renderedLocalModalIdentity !== null;
  const quickChatDeleteFocusDecision = reconcileQuickChatDeleteFocusContinuation(
    uiState.quickChatDeleteFocusContinuation,
    state,
    localModalPending,
  );
  uiState.quickChatDeleteFocusContinuation = quickChatDeleteFocusDecision.continuation;
  const sideChatOwner = sideChatOwnerSessionId(state);
  const sideChatFocusDecision = reconcileSideChatFocusContinuation(
    uiState.sideChatFocusContinuation,
    previous,
    state,
    sideChatFocusSurface(document),
    {
      paneVisible: uiState.artifactPaneMode === "side_chat" && !uiState.artifactPaneCollapsed,
      localModalOpen: localModalPending,
      mutation: sideChatOwner === null
        ? null
        : (uiState.sideChatMutations.get(sideChatOwner) ?? null),
    },
  );
  uiState.sideChatFocusContinuation = sideChatFocusDecision.continuation;
  const backgroundInert = modalIsOpen(state, localModalPending);
  const localModalOpening = lastRenderedLocalModalIdentity === null && renderedLocalModalIdentity !== null;
  const localModalClosing = lastRenderedLocalModalIdentity !== null && renderedLocalModalIdentity === null;
  const modalOpening = (previous !== null && isModalOpening(previous, state)) || localModalOpening;
  const modalClosing = (previous !== null && isModalClosing(previous, state)) || localModalClosing;
  const titlebarMenuClosing = Boolean(
    previous
    && titlebarMenuFromOverlay(previous.overlay)
    && state.overlay === "none"
    && !state.confirmation_visible
    && !localModalPending,
  );
  const titlebarMenuFocusAction = titlebarMenuClosing
    && previous
    && uiState.titlebarMenuFocusContinuation?.overlay === previous.overlay
      ? uiState.titlebarMenuFocusContinuation.action
      : null;
  const previousInitialSetupStep = lastRenderedModel?.local.initialSetup.step;
  const nextInitialSetupStep = model.local.initialSetup.step;
  const previousSettingsOwner = settingsSurfaceIdentity(previous, previousInitialSetupStep);
  const nextSettingsOwner = settingsSurfaceIdentity(state, nextInitialSetupStep);
  if (modalOpening) {
    modalScrollReturnStack.push(captureSelectorScrollSnapshots(
      MODAL_SCROLL_SELECTORS,
      lastRenderedAgentExecutionOwner,
      previousSettingsOwner,
    ));
    modalDetailsReturnStack.push(captureCurrentDetailSnapshots(
      lastRenderedAgentExecutionOwner,
      previousSettingsOwner,
    ));
    modalFocusReturnStack.push(captureModalReturnFocusSnapshot(previous, previousSettingsOwner));
  }
  const activeElement = document.activeElement instanceof HTMLElement
    ? document.activeElement
    : null;
  const selectedProjectId = state.project_rows[state.selected_project_index]?.project_id ?? null;
  const newSessionFocusAtRenderStart = pendingNewSessionInitiatingFocus;
  const newSessionPromptAtRenderStart = pendingNewSessionPromptFocus;
  const newSessionFocusDecision = reconcileNewSessionFocusContinuation(
    newSessionFocusAtRenderStart,
    {
      mutationName,
      currentActiveElement: activeElement,
      currentFocusUnclaimed: activeElement === null
        || activeElement === document.body
        || activeElement === document.documentElement,
      currentInteractionGeneration: composerFocusInteractionGeneration,
      selectedProjectId,
      selectedSessionIndex: state.selected_session_index,
      currentOwner: composerOwner(state),
      currentSessionId: state.draft_target.sessionId,
      navigationLoading: state.navigation_loading,
    },
  );
  pendingNewSessionInitiatingFocus = newSessionFocusDecision.continuation;
  if (newSessionFocusDecision.settled) {
    pendingNewSessionPromptFocus = newSessionFocusDecision.settled;
    uiState.focusPromptAfterRender = true;
  } else if (mutationName !== null && !mutationStartsNewSession(mutationName)) {
    pendingNewSessionPromptFocus = null;
  }
  const newSessionFocusRejected = newSessionFocusDecision.rejected
    || (mutationName !== null
      && mutationStartsNewSession(mutationName)
      && newSessionFocusAtRenderStart === null
      && newSessionPromptAtRenderStart === null);
  const independentComposerFocusRequested = sessionSelectionRequestsComposerFocus(mutationName);
  if (newSessionFocusRejected && !independentComposerFocusRequested) {
    pendingNewSessionPromptFocus = null;
    uiState.focusPromptAfterRender = false;
  }
  if (
    pendingNewSessionPromptFocus
    && !settledNewSessionFocusContinuationIsCurrent(
      pendingNewSessionPromptFocus,
      composerFocusInteractionGeneration,
      selectedProjectId,
      state.selected_session_index,
      composerOwner(state),
      state.draft_target.sessionId,
    )
  ) {
    pendingNewSessionPromptFocus = null;
    if (!independentComposerFocusRequested) {
      uiState.focusPromptAfterRender = false;
    }
  }
  const initiatingTriggerYieldsPromptFocus = newSessionFocusDecision.yieldsInitiatingFocus
    || newSessionFocusDecision.settled !== null
    || newSessionFocusDecision.continuation !== null
    || pendingNewSessionPromptFocus !== null;
  const modalReturnFocusSnapshot = modalClosing
    ? (modalFocusReturnStack.pop() ?? null)
    : null;
  const focusSnapshot = initiatingTriggerYieldsPromptFocus
    ? null
    : modalClosing
      ? modalReturnFocusSnapshot
      : modalOpening
        ? null
        : captureFocusSnapshot(previous, state);
  if (
    previous
    && titlebarMenuFromOverlay(previous.overlay)
    && previous.overlay !== state.overlay
  ) {
    uiState.titlebarMenuFocusContinuation = null;
  }
  const scrollSnapshots = captureScrollSnapshots(previous, state, lastRenderedAgentExecutionOwner);
  if (modalClosing) scrollSnapshots.push(...(modalScrollReturnStack.pop() ?? []));
  const detailSnapshots = captureDetailSnapshots(previous, state, lastRenderedAgentExecutionOwner);
  if (modalClosing) detailSnapshots.push(...(modalDetailsReturnStack.pop() ?? []));
  const previousThread = document.querySelector<HTMLElement>("#thread");
  const previousPrompt = document.querySelector<HTMLTextAreaElement>("#prompt");
  const previousThreadScrollTop = previousThread?.scrollTop ?? 0;
  const previousThreadWasNearEnd = previousThread ? isThreadNearEnd(previousThread) : true;
  const previousSessionInteractionOwner = previous ? composerSessionOwner(previous) : null;
  const nextSessionInteractionOwner = composerSessionOwner(state);
  const restorePromptFocusAfterRefresh = refreshPromptFocusContinuationAccepted(
    refreshPromptFocusContinuation,
    uiState.refreshPromptFocusInteractionGeneration,
    mutationName,
    composerOwner(state),
    document.activeElement instanceof Element
      && document.activeElement.closest('[data-action="refresh"]') !== null,
  );
  if (previousSessionInteractionOwner && previousThread && previousPrompt) {
    uiState.sessionInteractionSnapshots.set(
      previousSessionInteractionOwner,
      captureSessionInteractionSnapshot(previousThread, previousPrompt),
    );
  }
  const sessionInteractionOwnerChanged = previousSessionInteractionOwner !== null
    && previousSessionInteractionOwner !== nextSessionInteractionOwner;
  const rememberedSessionInteraction = sessionInteractionOwnerChanged
    && state.draft_target.sessionId !== null
    ? (uiState.sessionInteractionSnapshots.get(nextSessionInteractionOwner) ?? null)
    : null;
  const promptSessionInteraction = sessionPromptInteractionForRender(
    uiState.sessionInteractionSnapshots,
    {
      owner: nextSessionInteractionOwner,
      ownerChanged: sessionInteractionOwnerChanged,
      durableSession: state.draft_target.sessionId !== null,
      focusPending: uiState.focusPromptAfterRender,
    },
  );
  const nextSessionKey = state.session_rows[state.selected_session_index]?.session_id ?? state.selected_session_title;
  const previousTranscriptCount = previous?.transcript_rows.length ?? 0;
  const previousPendingInputCount = previous?.pending_turn_inputs.length ?? 0;
  const previousChangeCount = previous?.file_change_rows.length ?? 0;
  const previousAgentRows = previous?.agent_activity_rows ?? [];
  const agentRows = state.agent_activity_rows ?? [];
  const sessionChanged = nextSessionKey !== previousSessionKey;
  const historyPrependTransition = advancePendingHistoryPrepend(pendingHistoryPrepend, state);
  pendingHistoryPrepend = historyPrependTransition.pending;
  const prependViewportAnchor = historyPrependTransition.disposition === "consume" && previousThread
    ? captureViewportAnchor(previousThread)
    : null;
  const contentAdvanced = state.transcript_rows.length > previousTranscriptCount
    || state.pending_turn_inputs.length > previousPendingInputCount
    || state.file_change_rows.length > previousChangeCount;
  const agentActivityAdvanced = previous
    ? agentActivityRowsChanged(previousAgentRows, agentRows) || (!previous.agent_tree_active && state.agent_tree_active)
    : agentRows.length > 0;
  const selectedAgentNeedsRefresh = previous !== null
    && uiState.artifactPaneMode === "agents"
    && selectedAgentActivityChanged(previousAgentRows, agentRows, uiState.selectedAgentPath);
  const runCompleted = previous !== null && runCompletionEdge(
    {
      busy: previous.busy,
      terminal: isTerminalRunStatus(previous.run_status_key),
    },
    {
      busy: state.busy,
      terminal: isTerminalRunStatus(state.run_status_key),
    },
  );
  const tailFollowDecision = threadTailFollow.reconcile({
    workspacePath: state.run_target.workspacePath,
    sessionId: state.run_target.sessionId,
    runtimeOwnerToken: state.run_target.runtimeOwnerToken,
    runActive: state.busy || state.agent_tree_active || state.post_run_refresh_pending,
    terminal: isTerminalRunStatus(state.run_status_key),
  });
  const shouldRevealEnd = shouldRevealThreadEnd({
    sessionChanged: sessionChanged && rememberedSessionInteraction === null,
    runStartRequested: tailFollowDecision.follow,
    previouslyNearEnd: previousThreadWasNearEnd,
    updateWantsEnd: state.busy
      || state.agent_tree_active
      || contentAdvanced
      || agentActivityAdvanced
      || runCompleted,
  });
  const preserveConnectedSettings = shouldRetainConnectedSettingsSurface(
    previous,
    state,
    lastRenderedLocalModalIdentity,
    renderedLocalModalIdentity,
    previousInitialSetupStep,
    nextInitialSetupStep,
  );
  const currentSettingsModal = preserveConnectedSettings
    ? document.querySelector<HTMLElement>(".settings-modal, .initial-setup-shell")
    : null;
  const retainingInitialSetup = currentSettingsModal?.matches(".initial-setup-shell") === true;
  const currentFrame = preserveConnectedSettings && !retainingInitialSetup
    ? appRoot.querySelector<HTMLElement>(".app-frame")
    : null;
  const preservedTitlebar = document.querySelector<HTMLElement>(".app-titlebar");
  const renderedMarkup = renderDesktopMarkup(model, {
    backgroundInert,
    taskActivityDelay,
  });
  let retainedConnectedSettings = false;
  let retainedConnectedPrompt = false;
  if (currentSettingsModal && retainingInitialSetup) {
    const template = document.createElement("template");
    template.innerHTML = renderedMarkup;
    const nextSettingsModal = template.content.querySelector<HTMLElement>(".initial-setup-shell");
    if (nextSettingsModal) {
      synchronizeRetainedSettingsSurface(
        currentSettingsModal,
        nextSettingsModal,
        model.local.configMutationPending
          || model.local.initialSetup.finishPending
          || model.local.initialSetup.auxiliaryPendingKind !== null,
        !uiState.configDirty,
      );
      retainedConnectedSettings = true;
    }
  }
  if (currentSettingsModal && currentFrame) {
    const template = document.createElement("template");
    template.innerHTML = renderedMarkup;
    const nextFrame = template.content.querySelector<HTMLElement>(".app-frame");
    const nextSettingsModal = template.content.querySelector<HTMLElement>(".settings-modal");
    if (nextFrame && nextSettingsModal) {
      retainedConnectedPrompt = retainConnectedMainPrompt(
        previousPrompt,
        nextFrame.querySelector<HTMLTextAreaElement>("#prompt"),
        previousSessionInteractionOwner,
        nextSessionInteractionOwner,
      );
      const nextTitlebar = nextFrame.querySelector<HTMLElement>(".app-titlebar");
      if (preservedTitlebar && nextTitlebar) nextTitlebar.replaceWith(preservedTitlebar);
      currentFrame.replaceWith(nextFrame);
      const currentStatus = currentSettingsModal.querySelector<HTMLElement>(".settings-status-stack");
      const nextStatus = nextSettingsModal.querySelector<HTMLElement>(".settings-status-stack");
      if (currentStatus && nextStatus && !currentStatus.contains(document.activeElement)) {
        currentStatus.replaceWith(nextStatus);
      }
      synchronizeRetainedSettingsSurface(
        currentSettingsModal,
        nextSettingsModal,
        model.local.configMutationPending || model.local.sessionSettings.mutationPending
          || (state.overlay === "hub" && (model.local.hub.pending !== null || model.local.deviceNetwork.pending !== null))
          || (state.overlay === "mcp_publish" && model.local.mcpPublish.pending !== null),
        state.overlay === "hub" || state.overlay === "mcp_publish" ? false : state.overlay === "session_settings"
          ? !model.local.sessionSettings.dirty
          : !uiState.configDirty,
      );
      if (state.overlay === "hub") {
        synchronizeHubControlValues(currentSettingsModal, nextSettingsModal);
        synchronizeDeviceNetworkControls(currentSettingsModal, nextSettingsModal);
      }
      if (state.overlay === "mcp_publish") synchronizePublishControlValues(currentSettingsModal, nextSettingsModal);
      retainedConnectedSettings = true;
    }
  }
  if (!retainedConnectedSettings) {
    preservedTitlebar?.remove();
    if (
      previousPrompt?.isConnected
      && previousSessionInteractionOwner === nextSessionInteractionOwner
    ) {
      const template = document.createElement("template");
      template.innerHTML = renderedMarkup;
      retainedConnectedPrompt = retainConnectedMainPrompt(
        previousPrompt,
        template.content.querySelector<HTMLTextAreaElement>("#prompt"),
        previousSessionInteractionOwner,
        nextSessionInteractionOwner,
      );
      appRoot.replaceChildren(template.content);
    } else {
      appRoot.innerHTML = renderedMarkup;
    }
    const nextTitlebar = document.querySelector<HTMLElement>(".app-titlebar");
    if (preservedTitlebar && nextTitlebar) nextTitlebar.replaceWith(preservedTitlebar);
  }
  if (preservedTitlebar?.isConnected) {
    synchronizeTitlebarMenuState(preservedTitlebar, state.overlay, backgroundInert);
  }
  if (prependViewportAnchor) {
    restoreDetailSnapshots(detailSnapshots, nextAgentExecutionOwner, nextSettingsOwner);
  }
  const thread = document.querySelector<HTMLElement>("#thread");
  let sessionThreadInteractionAfterLayout: SessionInteractionSnapshot | null = null;
  let pinnedToEnd = false;
  if (thread && prependViewportAnchor && restoreViewportAnchor(thread, prependViewportAnchor)) {
    // Preserve the visible message while an older bounded history chunk is prepended.
  } else if (thread && rememberedSessionInteraction) {
    // wireEvents autosizes the composer and changes the thread's available scroll range. Defer
    // this session-owned viewport until that final geometry exists, or a short Quick Chat thread
    // can clamp the offset against the default composer reserve and persist it on the next poll.
    sessionThreadInteractionAfterLayout = rememberedSessionInteraction;
  } else if (thread && shouldRevealEnd) {
    revealThreadEnd(revealGeneration);
    pinnedToEnd = true;
  } else if (thread && previousThread) {
    restoreThreadPosition(thread, previousThreadScrollTop);
  }
  previousSessionKey = nextSessionKey;
  lastRenderedLocalModalIdentity = renderedLocalModalIdentity;
  lastRenderedState = state;
  lastRenderedModel = model;
  lastRenderedAgentExecutionOwner = nextAgentExecutionOwner;
  if (!prependViewportAnchor) {
    restoreDetailSnapshots(detailSnapshots, nextAgentExecutionOwner, nextSettingsOwner);
  }
  restoreScrollSnapshots(scrollSnapshots, nextAgentExecutionOwner, nextSettingsOwner);
  const acceptedSettingsAction = restoreSettingsActionViewport(
    settingsAction, previous, state, (selector) => document.querySelector<HTMLElement>(selector),
  ) ? settingsAction : null;
  if (
    agentExecutionPrependDecision.restoreViewport
    && agentExecutionPrependOwnerMatches(
      agentExecutionPrependDecision.restoreViewport,
      state,
      uiState.selectedAgentPath,
      selectedExecution,
    )
  ) {
    restoreAgentExecutionPrependViewport(
      document,
      agentExecutionPrependDecision.restoreViewport,
    );
  }
  const sessionInteractionOwnsPromptSelection = state.draft_target.sessionId !== null
    || sessionInteractionOwnerChanged;
  const focusSnapshotIntent = createFocusSnapshotIntent(
    focusSnapshot,
    nextSettingsOwner,
    sessionInteractionOwnsPromptSelection,
    modalClosing ? "modal-return" : "focus-snapshot",
  );
  const focusSnapshotReserved = focusSnapshotIntent !== null;
  if (focusSnapshotIntent) postRenderFocusIntents.push(focusSnapshotIntent);
  const settingsActionFocusContinuation = acceptedSettingsAction ?? uiState.settingsActionFocusContinuation;
  uiState.settingsActionFocusContinuation = null;
  const settingsActionModal = settingsActionFocusContinuation
    && settingsActionFocusStillTargets(settingsActionFocusContinuation, state)
    ? document.querySelector<HTMLElement>(".settings-modal")
    : null;
  if (settingsActionFocusContinuation) {
    postRenderFocusIntents.push({
      source: "settings-action",
      priority: "explicit-transfer",
      claim: { kind: "unowned" },
      candidates: settingsActionFocusCandidates(
        settingsActionFocusContinuation,
        (selector) => selector === ".settings-modal"
          ? settingsActionModal
          : settingsActionModal?.querySelector<HTMLElement>(selector) ?? null,
      ),
      isCurrent: () => settingsActionFocusStillTargets(
        settingsActionFocusContinuation,
        state,
      ),
    });
  }
  const titlebarForContinuation = titlebarMenuFocusAction
    ? document.querySelector<HTMLElement>(".app-titlebar")
    : null;
  const titlebarMenuFocusIntent: PostRenderFocusIntent | null =
    !initiatingTriggerYieldsPromptFocus
    && titlebarForContinuation
    && titlebarMenuFocusAction
      ? {
          source: "titlebar-menu",
          priority: "explicit-transfer",
          claim: { kind: "unowned" },
          candidates: [{
            resolve: () => Array.from(
              titlebarForContinuation.querySelectorAll<HTMLElement>(
                "button[data-action]:not(:disabled):not([aria-disabled='true'])",
              ),
            ).find((candidate) => candidate.dataset.action === titlebarMenuFocusAction) ?? null,
          }],
          isCurrent: () => (
            lastRenderedState === state
            && state.overlay === "none"
            && !state.confirmation_visible
            && uiState.pendingLocalConfirmation === null
            && uiState.sideChatDeleteConfirmation === null
          ),
        }
      : null;
  const titlebarMenuFocusReserved = titlebarMenuFocusIntent !== null;
  if (titlebarMenuFocusIntent) postRenderFocusIntents.push(titlebarMenuFocusIntent);
  wireEvents(state, eventContext);
  // A current Settings continuation is already constrained to the exact modal owner and ends
  // with that dialog as its fallback. Do not let the generic modal-primary intent replace the
  // requested return target with the first Settings field after a nested dialog is rebuilt.
  const overlayPrimaryFocusIntent = settingsActionModal
    ? null
    : focusOverlayPrimary(state, uiState);
  if (overlayPrimaryFocusIntent) postRenderFocusIntents.push(overlayPrimaryFocusIntent);
  if (
    historyPrependTransition.focusContinuation
    && historyPrependTransition.focusPhase
  ) {
    const settledState = state;
    const settledRenderGeneration = revealGeneration;
    const continuation = historyPrependTransition.focusContinuation;
    const phase = historyPrependTransition.focusPhase;
    postRenderFocusIntents.push({
      source: "history-prepend",
      priority: "operation-return",
      claim: { kind: "unowned" },
      candidates: historyPrependFocusCandidates(document, continuation),
      isCurrent: () => (
        lastRenderedState === settledState
        && threadEndRevealGeneration === settledRenderGeneration
        && historyPrependFocusContinuationIsCurrent(
          continuation,
          settledState,
          pendingHistoryPrepend,
          nextHistoryPrependGeneration - 1,
          phase,
        )
      ),
    });
  }
  if (agentExecutionPrependDecision.restoreFocus) {
    const settledState = state;
    const settledRenderGeneration = revealGeneration;
    const continuation = agentExecutionPrependDecision.restoreFocus;
    postRenderFocusIntents.push({
      source: "agent-execution-prepend",
      priority: "operation-return",
      claim: { kind: "unowned" },
      candidates: agentExecutionPrependFocusCandidates(document, continuation),
      isCurrent: () => {
      const execution = selectedAgentExecution(uiState, settledState);
      return (
        lastRenderedState === settledState
        && threadEndRevealGeneration === settledRenderGeneration
        && agentExecutionPrependOwnerMatches(
          continuation,
          settledState,
          uiState.selectedAgentPath,
          execution,
        )
      );
      },
    });
  }
  if (thread && sessionThreadInteractionAfterLayout) {
    restoreSessionThreadInteraction(sessionThreadInteractionAfterLayout, thread);
  }
  if (promptSessionInteraction) {
    const prompt = document.querySelector<HTMLTextAreaElement>("#prompt");
    if (prompt) restoreSessionPromptInteraction(promptSessionInteraction, prompt);
  }
  if (
    restorePromptFocusAfterRefresh
    && retainedConnectedPrompt
    && previousPrompt
    && refreshPromptFocusContinuation
  ) {
    const settledState = state;
    const settledContinuation = refreshPromptFocusContinuation;
    const refreshFocusOwners = Array.from(
      document.querySelectorAll<HTMLElement>('[data-action="refresh"]'),
    );
    postRenderFocusIntents.push(createRefreshPromptFocusIntent({
      prompt: previousPrompt,
      resolvePrompt: () => document.querySelector<HTMLTextAreaElement>("#prompt"),
      refreshOwners: refreshFocusOwners,
      settle: (target) => {
        if (promptSessionInteraction && target instanceof HTMLTextAreaElement) {
          restoreSessionPromptInteraction(promptSessionInteraction, target);
        }
      },
      isCurrent: () => (
        lastRenderedState === settledState
        && uiState.refreshPromptFocusInteractionGeneration === settledContinuation.interactionGeneration
        && composerOwner(settledState) === settledContinuation.owner
        && previousPrompt.isConnected
        && !previousPrompt.disabled
      ),
    }));
  }
  if (quickChatDeleteFocusDecision.focusTarget && !focusSnapshotReserved) {
    const settledState = state;
    const settledRenderGeneration = revealGeneration;
    const continuation = quickChatDeleteFocusDecision.continuation;
    const focusTarget = quickChatDeleteFocusDecision.focusTarget;
    if (continuation) {
      postRenderFocusIntents.push({
        source: "quick-chat-delete",
        priority: "operation-return",
        claim: { kind: "unowned" },
        candidates: quickChatDeleteFocusCandidates(document, focusTarget),
        isCurrent: () => (
        continuation
        && uiState.quickChatDeleteFocusContinuation === continuation
        && lastRenderedState === settledState
        && threadEndRevealGeneration === settledRenderGeneration
        && uiState.pendingLocalConfirmation === null
        && uiState.sideChatDeleteConfirmation === null
        ),
      });
      postRenderFocusResultHandlers.push((result) => {
        if (
          result.source === "quick-chat-delete"
          && result.kind !== "unavailable"
          && result.kind !== "stale-render"
          && result.kind !== "stale-interaction"
          && result.kind !== "interaction-active"
          && result.kind !== "stale-intent"
          && result.kind !== "superseded"
          && uiState.quickChatDeleteFocusContinuation === continuation
        ) {
          uiState.quickChatDeleteFocusContinuation = null;
        }
      });
    }
  } else if (
    quickChatDeleteFocusDecision.focusTarget
    && focusSnapshotReserved
    && uiState.quickChatDeleteFocusContinuation === quickChatDeleteFocusDecision.continuation
  ) {
    uiState.quickChatDeleteFocusContinuation = null;
  }
  if (attachmentFocusDecision.focusTarget) {
    const settledState = state;
    const focusTarget = attachmentFocusDecision.focusTarget;
    postRenderFocusIntents.push({
      source: "attachment",
      priority: "operation-return",
      claim: { kind: "unowned" },
      candidates: attachmentFocusCandidates(document, focusTarget),
      isCurrent: () => lastRenderedState === settledState,
    });
  }
  if (thread) wireThreadTailFollowEvents(thread);
  if (pinnedToEnd) {
    pinResolvedThreadToEnd(resolveCurrentThread);
    threadTailFollow.completeRender(tailFollowDecision, true);
  } else if (previous === null || sessionChanged) {
    syncInactiveThreadViewport();
  }
  if (historyPrependTransition.disposition === "consume"
    || historyPrependTransition.disposition === "discard") {
    scheduleInactiveThreadViewportSync();
  }
  const agentPaneFocusIntent = takeAgentPaneFocusIntent(state);
  if (agentPaneFocusIntent) postRenderFocusIntents.push(agentPaneFocusIntent);
  const artifactPaneFocusIntent = takeArtifactPaneFocusIntent(state);
  if (artifactPaneFocusIntent) postRenderFocusIntents.push(artifactPaneFocusIntent);
  if (runFocusDecision.focusPrompt && !focusSnapshotReserved) {
    const settledState = state;
    postRenderFocusIntents.push({
      source: "main-run",
      priority: "operation-return",
      claim: { kind: "unowned" },
      candidates: [{
        resolve: () => document.querySelector<HTMLTextAreaElement>("#prompt"),
      }],
      isCurrent: () => lastRenderedState === settledState,
    });
  }
  if (sideChatFocusDecision.focusTarget && !focusSnapshotReserved) {
    const settledState = state;
    const settledRenderGeneration = revealGeneration;
    const settledInteractionGeneration = uiState.sideChatFocusInteractionGeneration;
    const focusTarget = sideChatFocusDecision.focusTarget;
    postRenderFocusIntents.push({
      source: "side-chat",
      priority: "operation-return",
      claim: { kind: "unowned" },
      candidates: [{
        resolve: () => document.querySelector<HTMLTextAreaElement>("#side-chat-prompt"),
      }],
      isCurrent: () => (
        lastRenderedState === settledState
        && threadEndRevealGeneration === settledRenderGeneration
        && uiState.sideChatFocusInteractionGeneration === settledInteractionGeneration
        && uiState.artifactPaneMode === "side_chat"
        && !uiState.artifactPaneCollapsed
        && uiState.pendingLocalConfirmation === null
        && uiState.sideChatDeleteConfirmation === null
        && sideChatFocusTargetStillMatches(focusTarget, settledState)
      ),
    });
  }
  if (
    (modalClosing || (titlebarMenuFocusAction !== null && !titlebarMenuFocusReserved)) &&
    !initiatingTriggerYieldsPromptFocus &&
    !newSessionFocusRejected &&
    !focusSnapshotReserved &&
    !quickChatDeleteFocusDecision.ownsModalCloseFallback &&
    !state.confirmation_visible &&
    !localModalPending &&
    state.overlay === "none"
  ) {
    postRenderFocusIntents.push({
      source: "composer-request",
      priority: "fallback",
      claim: { kind: "unowned" },
      candidates: [{
        resolve: () => document.querySelector<HTMLTextAreaElement>("#prompt"),
      }],
      isCurrent: () => (
        lastRenderedState === state
        && !state.confirmation_visible
        && uiState.pendingLocalConfirmation === null
        && uiState.sideChatDeleteConfirmation === null
        && state.overlay === "none"
      ),
    });
  }
  const requestedPromptFocus = focusPromptIfRequested(
    state,
    promptSessionInteraction,
    pendingNewSessionPromptFocus,
  );
  if (requestedPromptFocus) postRenderFocusIntents.push(requestedPromptFocus);
  if (
    selectedAgentNeedsRefresh
    && uiState.selectedAgentPath
    && uiState.agentExecutionTransaction.active === null
  ) {
    void loadAgentExecution(state, uiState.selectedAgentPath);
  }
  schedulePostRenderFocus(
    postRenderFocusIntents,
    revealGeneration,
    postRenderFocusResultHandlers.length > 0
      ? (result) => postRenderFocusResultHandlers.forEach((handler) => handler(result))
      : undefined,
  );
}

function schedulePostRenderFocus(
  intents: readonly PostRenderFocusIntent[],
  renderCommit = threadEndRevealGeneration,
  onResult?: (result: FocusArbiterResult) => void,
): void {
  postRenderFocusArbiter.schedule({
    renderCommit,
    interactionEpoch: postRenderFocusInteractionEpoch,
    intents,
    onResult,
  });
}

function takeArtifactPaneFocusIntent(state: DesktopWebState): PostRenderFocusIntent | null {
  const target = uiState.artifactPaneFocusAfterRender;
  if (!target) return null;
  uiState.artifactPaneFocusAfterRender = null;
  const selector = target === "content"
    ? '[data-focus-key="artifact-pane-content"]'
    : '[data-focus-key="artifact-pane-toggle"]';
  return {
    source: "artifact-pane",
    priority: "pane-navigation",
    claim: { kind: "unowned" },
    candidates: [{ resolve: () => document.querySelector<HTMLElement>(selector) }],
    isCurrent: () => lastRenderedState === state,
  };
}

function takeAgentPaneFocusIntent(state: DesktopWebState): PostRenderFocusIntent | null {
  if (uiState.focusSelectedAgentAfterRender && uiState.selectedAgentPath) {
    const agentPath = uiState.selectedAgentPath;
    uiState.focusSelectedAgentAfterRender = false;
    return {
      source: "agent-pane",
      priority: "pane-navigation",
      claim: { kind: "unowned" },
      candidates: [{
        resolve: () => document.querySelector<HTMLElement>(
          `[data-focus-key="agent-execution:${CSS.escape(agentPath)}"]`,
        ),
        settle: (target) => {
          if (target instanceof HTMLElement) {
            target.scrollIntoView({ block: "nearest", inline: "nearest" });
          }
        },
      }],
      isCurrent: () => (
        lastRenderedState === state
        && uiState.selectedAgentPath === agentPath
        && uiState.artifactPaneMode === "agents"
        && !uiState.artifactPaneCollapsed
      ),
    };
  }
  const focusTarget = uiState.agentPaneFocusAfterRender;
  if (!focusTarget) return null;
  uiState.agentPaneFocusAfterRender = null;
  return {
    source: "agent-pane",
    priority: "pane-navigation",
    claim: { kind: "unowned" },
    candidates: [{
      resolve: () => document.querySelector<HTMLElement>(
        `[data-focus-key="${CSS.escape(focusTarget)}"]`,
      ),
    }],
    isCurrent: () => lastRenderedState === state,
  };
}

function shouldShowSplash(elapsedMs: number): boolean {
  return elapsedMs < SPLASH_MIN_VISIBLE_MS;
}

function scheduleSplashReveal(elapsedMs: number): void {
  if (elapsedMs >= SPLASH_MIN_VISIBLE_MS || splashTimer !== null) {
    return;
  }
  splashTimer = window.setTimeout(() => {
    splashTimer = null;
    if (currentState) {
      acceptState(currentState, true);
    }
  }, Math.max(0, SPLASH_MIN_VISIBLE_MS - elapsedMs));
}

function reconcileUiLocalState(
  previous: DesktopWebState | null,
  state: DesktopWebState,
  mutationName: string | null,
): AttachmentFocusDecision {
  const nextSessionKey = state.session_rows[state.selected_session_index]?.session_id ?? state.selected_session_title;
  const previousSessionKey = previous?.session_rows[previous.selected_session_index]?.session_id ?? previous?.selected_session_title ?? "";
  const sessionChanged = previous !== null && nextSessionKey !== previousSessionKey;
  const imagesCleared = state.attached_images.length === 0 && state.image_input.trim().length === 0;

  if (sessionChanged || operationInvalidatesComposer(mutationName)) {
    uiState.attachmentTrayOpen = false;
  }
  if (
    mutationName === "new_chat"
    || mutationName === "new_project_session"
    || sessionSelectionRequestsComposerFocus(mutationName)
  ) {
    uiState.focusPromptAfterRender = true;
  }
  if ((mutationName === "attach_image" || mutationName === "browse_image") && state.image_input.trim().length === 0) {
    uiState.attachmentTrayOpen = false;
  }
  if ((mutationName === "clear_images" || mutationName === "remove_image") && imagesCleared) {
    uiState.attachmentTrayOpen = false;
  }
  const attachmentFocusDecision = reconcileAttachmentFocusContinuation(
    uiState.attachmentFocusContinuation,
    state,
    mutationName,
  );
  uiState.attachmentFocusContinuation = attachmentFocusDecision.continuation;
  if (attachmentFocusDecision.trayOpen !== null) {
    uiState.attachmentTrayOpen = attachmentFocusDecision.trayOpen;
  }
  if (uiState.pendingLocalConfirmation && !localConfirmationStillTargetsOwner(uiState.pendingLocalConfirmation, state)) {
    uiState.pendingLocalConfirmation = null;
    finishLocalDecision(uiState);
  }
  if (
    uiState.sideChatDeleteConfirmation
    && !sideChatDeleteConfirmationStillTargets(uiState.sideChatDeleteConfirmation, state)
  ) {
    uiState.sideChatDeleteConfirmation = null;
  }
  reconcilePermissionDecision(
    uiState,
    state.confirmation_visible ? state.confirmation_id : null,
  );
  const previousOutputCount = (previous?.artifact_rows.length ?? 0)
    + (previous?.file_change_rows.length ?? 0)
    + (previous?.agent_activity_rows.length ?? 0);
  const outputCount = state.artifact_rows.length
    + state.file_change_rows.length
    + state.agent_activity_rows.length;
  if (previous && outputCount > 0 && previousOutputCount === 0 && uiState.artifactPaneCollapsed) {
    uiState.artifactPaneCollapsed = false;
    window.localStorage.setItem("moyai.artifactPaneCollapsed", "false");
  }
  return attachmentFocusDecision;
}

function focusPromptIfRequested(
  state: DesktopWebState,
  interactionSnapshot: SessionInteractionSnapshot | null,
  newSessionFocusContinuation: SettledNewSessionFocusContinuation | null,
): PostRenderFocusIntent | null {
  const shouldFocusInitialPrompt =
    !uiState.initialPromptFocusDone &&
    state.selected_session_index < 0 &&
    !state.busy &&
    state.overlay === "none" &&
    !state.confirmation_visible;
  if (!uiState.focusPromptAfterRender && !shouldFocusInitialPrompt) {
    return null;
  }
  if (state.busy || state.navigation_loading || state.overlay !== "none" || state.confirmation_visible) {
    return null;
  }
  uiState.initialPromptFocusDone = true;
  uiState.focusPromptAfterRender = false;
  pendingNewSessionPromptFocus = null;
  const expectedOwner = composerOwner(state);
  const expectedNewSessionInteractionGeneration = newSessionFocusContinuation?.interactionGeneration ?? null;
  const expectedNewSessionRequestToken = newSessionFocusContinuation?.requestToken ?? null;
  return {
    source: newSessionFocusContinuation ? "new-session" : shouldFocusInitialPrompt
      ? "initial-composer"
      : "composer-request",
    priority: newSessionFocusContinuation ? "explicit-transfer" : "fallback",
    claim: { kind: "unowned" },
    candidates: [{
      resolve: () => document.querySelector<HTMLTextAreaElement>("#prompt"),
      settle: (target) => {
        if (!(target instanceof HTMLTextAreaElement)) return;
        if (interactionSnapshot) {
          restoreSessionPromptInteraction(interactionSnapshot, target);
        }
      },
    }],
    isCurrent: () => {
    const settledState = lastRenderedState;
    if (
      !settledState
      || composerOwner(settledState) !== expectedOwner
      || settledState.busy
      || settledState.navigation_loading
      || settledState.overlay !== "none"
      || settledState.confirmation_visible
      || interactionLifecycle.active
      || expectedNewSessionRequestToken?.rejected === true
      || (
        expectedNewSessionInteractionGeneration !== null
        && composerFocusInteractionGeneration !== expectedNewSessionInteractionGeneration
      )
    ) {
      return false;
    }
    const prompt = document.querySelector<HTMLTextAreaElement>("#prompt");
    return Boolean(prompt && !prompt.disabled);
    },
  };
}

interface FocusSnapshot {
  selector: string;
  occurrence: number;
  selectionStart: number | null;
  selectionEnd: number | null;
  settingsSurfaceOwner: string | null;
}

interface ScrollSnapshot {
  selector: string;
  occurrence: number;
  scrollLeft: number;
  scrollTop: number;
  agentExecutionOwner: string | null;
  settingsSurfaceOwner: string | null;
}

interface DetailSnapshot {
  key: string;
  open: boolean;
  scope: "global" | "agent-execution" | "settings";
  agentExecutionOwner: string | null;
  settingsSurfaceOwner: string | null;
}

const STABLE_LIST_SCROLL_SELECTORS = [
  ".project-list",
  ".chat-list",
  ".sub-agent-list",
  ".agent-execution-scroll",
];
const MODAL_SCROLL_SELECTORS = [
  ".modal",
  ".settings-content",
  ".settings-nav",
  ".settings-json",
  ".settings-raw-value",
  ".select-list",
];

function captureFocusSnapshot(previous: DesktopViewState | null, state: DesktopViewState): FocusSnapshot | null {
  if (
    !previous
    || modalIdentity(previous) !== modalIdentity(state)
    || (
      (previous.overlay === "config" || previous.overlay === "session_settings")
      && !sameSettingsSurface(previous, state)
    )
  ) {
    return null;
  }
  if (
    document.activeElement instanceof Element
    && document.activeElement.closest(".side-chat-pane")
    && sideChatIdentity(previous) !== sideChatIdentity(state)
  ) {
    return null;
  }
  return captureCurrentFocusSnapshot(settingsSurfaceIdentity(previous));
}

function captureCurrentFocusSnapshot(settingsSurfaceOwner: string | null = null): FocusSnapshot | null {
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
    settingsSurfaceOwner: active.closest(".settings-modal") ? settingsSurfaceOwner : null,
  };
}

function captureModalReturnFocusSnapshot(
  previous: DesktopViewState | null,
  settingsSurfaceOwner: string | null,
): FocusSnapshot | null {
  const menuTriggerAction = previous ? titlebarMenuTriggerAction(previous.overlay) : null;
  if (menuTriggerAction) {
    const selector = `[data-action="${menuTriggerAction}"]`;
    const target = document.querySelector<HTMLElement>(selector);
    if (target) {
      return {
        selector,
        occurrence: Array.from(document.querySelectorAll(selector)).indexOf(target),
        selectionStart: null,
        selectionEnd: null,
        settingsSurfaceOwner: null,
      };
    }
  }
  return captureCurrentFocusSnapshot(settingsSurfaceOwner);
}

function createFocusSnapshotIntent(
  snapshot: FocusSnapshot | null,
  settingsSurfaceOwner: string | null,
  sessionInteractionOwnsPromptSelection = false,
  source: "modal-return" | "focus-snapshot" = "focus-snapshot",
): PostRenderFocusIntent | null {
  if (!snapshot) return null;
  if (
    snapshot.settingsSurfaceOwner !== null
    && snapshot.settingsSurfaceOwner !== settingsSurfaceOwner
  ) return null;
  const resolve = (): HTMLElement | null => (
    document.querySelectorAll<HTMLElement>(snapshot.selector)[snapshot.occurrence] ?? null
  );
  if (!resolve()) return null;
  return {
    source,
    priority: "exact-restore",
    claim: { kind: "unowned" },
    candidates: [{
      resolve,
      settle: (candidate) => {
        if (!(candidate instanceof HTMLElement)) return;
        const titlebarMenu = candidate.closest<HTMLElement>(
          ".titlebar-popover[data-titlebar-menu]",
        );
        if (
          titlebarMenu
          && titlebarMenuUsesRovingFocus(titlebarMenu.getAttribute("role"))
          && candidate.matches("button[data-titlebar-menu-action]")
        ) {
          const actions = Array.from(
            titlebarMenu.querySelectorAll<HTMLElement>(
              "button[data-titlebar-menu-action]:not(:disabled):not([aria-disabled='true'])",
            ),
          );
          const actionIndex = actions.indexOf(candidate);
          if (actionIndex >= 0) applyTitlebarMenuRovingTabIndex(actions, actionIndex);
        }
        if (
          (candidate instanceof HTMLInputElement || candidate instanceof HTMLTextAreaElement)
          && snapshot.selectionStart !== null
          && snapshot.selectionEnd !== null
          && !(sessionInteractionOwnsPromptSelection && candidate.id === "prompt")
        ) {
          candidate.setSelectionRange(snapshot.selectionStart, snapshot.selectionEnd);
        }
      },
    }],
    isCurrent: () => (
      snapshot.settingsSurfaceOwner === null
      || snapshot.settingsSurfaceOwner === settingsSurfaceIdentity(lastRenderedState)
    ),
  };
}

function captureScrollSnapshots(
  previous: DesktopViewState | null,
  state: DesktopViewState,
  agentExecutionOwner: string | null,
): ScrollSnapshot[] {
  const selectors = [...STABLE_LIST_SCROLL_SELECTORS];
  if (previous && selectedSessionIdentity(previous) === selectedSessionIdentity(state)) selectors.push(".output-scroll");
  if (previous && sideChatIdentity(previous) === sideChatIdentity(state)) selectors.push(".side-chat-scroll");
  if (previous && modalIdentity(previous) === modalIdentity(state) && modalIdentity(state) !== "none") {
    selectors.push(...MODAL_SCROLL_SELECTORS);
  }
  return captureSelectorScrollSnapshots(selectors, agentExecutionOwner, settingsSurfaceIdentity(previous));
}

function captureSelectorScrollSnapshots(
  selectors: string[],
  agentExecutionOwner: string | null = null,
  settingsSurfaceOwner: string | null = null,
): ScrollSnapshot[] {
  const snapshots: ScrollSnapshot[] = [];
  for (const selector of selectors) {
    document.querySelectorAll<HTMLElement>(selector).forEach((node, occurrence) => {
      snapshots.push({
        selector,
        occurrence,
        scrollLeft: node.scrollLeft,
        scrollTop: node.scrollTop,
        agentExecutionOwner: selector === ".agent-execution-scroll" ? agentExecutionOwner : null,
        settingsSurfaceOwner: node.closest(".settings-modal") ? settingsSurfaceOwner : null,
      });
    });
  }
  return snapshots;
}

function restoreScrollSnapshots(
  snapshots: ScrollSnapshot[],
  agentExecutionOwner: string | null,
  settingsSurfaceOwner: string | null,
): void {
  for (const snapshot of snapshots) {
    if (
      snapshot.selector === ".agent-execution-scroll"
      && !shouldPreserveAgentExecutionSnapshots(snapshot.agentExecutionOwner, agentExecutionOwner)
    ) {
      continue;
    }
    if (
      snapshot.settingsSurfaceOwner !== null
      && snapshot.settingsSurfaceOwner !== settingsSurfaceOwner
    ) {
      continue;
    }
    const target = document.querySelectorAll<HTMLElement>(snapshot.selector)[snapshot.occurrence];
    if (!target) continue;
    restoreScrollPosition(target, snapshot.scrollLeft, snapshot.scrollTop);
  }
}

function captureDetailSnapshots(
  previous: DesktopViewState | null,
  state: DesktopViewState,
  agentExecutionOwner: string | null,
): DetailSnapshot[] {
  if (
    !previous ||
    selectedSessionIdentity(previous) !== selectedSessionIdentity(state) ||
    modalIdentity(previous) !== modalIdentity(state) ||
    (
      (previous.overlay === "config" || previous.overlay === "session_settings")
      && !sameSettingsSurface(previous, state)
    )
  ) {
    return [];
  }
  return captureCurrentDetailSnapshots(agentExecutionOwner, settingsSurfaceIdentity(previous));
}

function captureCurrentDetailSnapshots(
  agentExecutionOwner: string | null,
  settingsSurfaceOwner: string | null = null,
): DetailSnapshot[] {
  return Array.from(document.querySelectorAll<HTMLDetailsElement>("details[data-details-key]"), (detail) => {
    const inAgentExecution = detail.closest(".agent-execution") !== null;
    const inSettings = detail.closest(".settings-modal") !== null;
    return {
      key: detail.dataset.detailsKey ?? "",
      open: detail.open,
      scope: inAgentExecution ? "agent-execution" as const : inSettings ? "settings" as const : "global" as const,
      agentExecutionOwner: inAgentExecution ? agentExecutionOwner : null,
      settingsSurfaceOwner: inSettings ? settingsSurfaceOwner : null,
    };
  }).filter((snapshot) => snapshot.key.length > 0);
}

function restoreDetailSnapshots(
  snapshots: DetailSnapshot[],
  agentExecutionOwner: string | null,
  settingsSurfaceOwner: string | null,
): void {
  const details = Array.from(document.querySelectorAll<HTMLDetailsElement>("details[data-details-key]"));
  for (const snapshot of snapshots) {
    if (
      snapshot.scope === "agent-execution"
      && !shouldPreserveAgentExecutionSnapshots(snapshot.agentExecutionOwner, agentExecutionOwner)
    ) {
      continue;
    }
    if (
      snapshot.scope === "settings"
      && snapshot.settingsSurfaceOwner !== settingsSurfaceOwner
    ) {
      continue;
    }
    const detail = details.find((candidate) => candidate.dataset.detailsKey === snapshot.key);
    if (detail) detail.open = snapshot.open;
  }
}

function selectedSessionIdentity(state: DesktopWebState): string {
  return state.session_rows[state.selected_session_index]?.session_id ?? state.selected_session_title;
}

function sideChatIdentity(state: DesktopWebState): string {
  return `${sideChatOwnerSessionId(state) ?? ""}\u0000${state.side_chat.chat_id ?? ""}`;
}

function isModalOpening(previous: DesktopWebState, state: DesktopWebState): boolean {
  return (
    (!previous.confirmation_visible && state.confirmation_visible) ||
    (!state.confirmation_visible
      && !isRegularModalOverlay(previous.overlay)
      && isRegularModalOverlay(state.overlay))
  );
}

function isModalClosing(previous: DesktopWebState, state: DesktopWebState): boolean {
  return (
    (previous.confirmation_visible && !state.confirmation_visible) ||
    (!previous.confirmation_visible
      && isRegularModalOverlay(previous.overlay)
      && !isRegularModalOverlay(state.overlay))
  );
}

function finishInteraction(release: InteractionRelease<StateUpdate> | null): void {
  if (!release) return;
  if (release.deferred && deferredStateUpdateStillAccepted(release.deferred)) {
    applyStateUpdate({
      ...release.deferred,
      forceRender: release.deferred.forceRender || release.renderCurrent,
    });
  } else if (release.renderCurrent && currentState) {
    acceptState(currentState, true);
  }
}

function installComposerFocusInteractionInvalidation(): void {
  const invalidate = (): void => {
    composerFocusInteractionGeneration += 1n;
    postRenderFocusInteractionEpoch += 1n;
    postRenderFocusArbiter.cancel();
  };
  document.addEventListener("pointerdown", invalidate, true);
  document.addEventListener("keydown", (event) => {
    if (!event.repeat) invalidate();
  }, true);
  document.addEventListener("compositionstart", invalidate, true);
  document.addEventListener("input", invalidate, true);
  document.addEventListener("wheel", invalidate, { capture: true, passive: true });
  window.addEventListener("blur", invalidate);
  window.addEventListener("pagehide", invalidate);
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) invalidate();
  });
}

async function submitPermissionDecision(decision: PermissionReviewDecision): Promise<void> {
  const confirmationId = currentState?.confirmation_visible
    ? currentState.confirmation_id
    : null;
  const submission = beginPermissionDecision(uiState, confirmationId, decision);
  if (submission === null) return;
  if (currentState) acceptState(currentState, true);
  try {
    const state = await command<DesktopWebState>("answer_permission", {
      decision,
      confirmationId: submission.requestId,
    });
    let settlementApplied = false;
    if (
      !permissionDecisionResponseAccepted(
        submission.requestId,
        state.confirmation_visible,
        state.confirmation_id,
      )
    ) {
      failPermissionDecision(
        uiState,
        submission,
        "決定を反映できませんでした。もう一度お試しください。",
      );
    } else {
      settlementApplied = finishPermissionDecision(uiState, submission);
    }
    if (permissionDecisionShouldFocusComposer(submission, settlementApplied, state.confirmation_visible)) {
      uiState.focusPromptAfterRender = true;
    }
    acceptState(state, true, "answer_permission", true);
  } catch (error) {
    const conflictState = commandConflictState(error);
    if (conflictState) {
      const recovered = recoverPermissionDecisionFromConflict(
        uiState,
        submission,
        conflictState.confirmation_visible ? conflictState.confirmation_id : null,
      );
      if (recovered && conflictState.confirmation_visible) {
        const active = document.activeElement;
        if (active instanceof HTMLElement) active.blur();
        uiState.lastFocusedOverlay = "none";
      }
      acceptState(conflictState, true, "command_conflict");
      return;
    }
    const failureState = commandInternalState(error);
    if (failureState) {
      finishPermissionDecision(uiState, submission);
      acceptState(failureState, true, "answer_permission_failure", true);
      return;
    }
    if (failPermissionDecision(
      uiState,
      submission,
      "決定を反映できませんでした。もう一度お試しください。",
    )) {
      if (currentState) acceptState(currentState, true);
    }
  }
}

async function submitRunStop(state: DesktopViewState): Promise<void> {
  const stopTarget = state.stop_target;
  if (stopTarget === null) return;
  const confirmationId = state.confirmation_visible ? state.confirmation_id : null;
  if (confirmationId === null) {
    try {
      acceptState(await cancelRunCommand<DesktopWebState>(stopTarget), true, "cancel_run", true);
    } catch (error) {
      if (!recoverCommandConflict(error)) reportError(error);
    }
    return;
  }
  const submission = beginPermissionStop(uiState, confirmationId);
  if (submission === null) return;
  if (currentState) acceptState(currentState, true);
  try {
    const nextState = await cancelRunCommand<DesktopWebState>(stopTarget);
    acceptState(nextState, true, "cancel_run", true);
  } catch (error) {
    const conflictState = commandConflictState(error);
    if (conflictState) {
      const recovered = recoverPermissionDecisionFromConflict(
        uiState,
        submission,
        conflictState.confirmation_visible ? conflictState.confirmation_id : null,
      );
      if (recovered && conflictState.confirmation_visible) {
        const active = document.activeElement;
        if (active instanceof HTMLElement) active.blur();
        uiState.lastFocusedOverlay = "none";
      }
      acceptState(conflictState, true, "command_conflict");
      return;
    }
    const failureState = commandInternalState(error);
    if (failureState) {
      if (
        failureState.confirmation_visible
        && failureState.confirmation_id === submission.requestId
      ) {
        failPermissionDecision(
          uiState,
          submission,
          "実行停止を要求できませんでした。もう一度お試しください。",
        );
      } else {
        finishPermissionDecision(uiState, submission);
      }
      acceptState(failureState, true, "cancel_run_failure", true);
      return;
    }
    if (failPermissionDecision(
      uiState,
      submission,
      "実行停止を要求できませんでした。もう一度お試しください。",
    )) {
      if (currentState) acceptState(currentState, true);
    }
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

function localConfirmationStillTargetsOwner(
  confirmation: NonNullable<typeof uiState.pendingLocalConfirmation>,
  state: DesktopWebState
): boolean {
  if (confirmation.kind === "settings_close") {
    return uiState.configDirty
      && settingsCloseTargetStillMatches(confirmation.expectedTarget, state);
  }
  if (confirmation.kind === "session_settings_close") {
    const target = state.session_settings.target;
    const owner = uiState.sessionSettings.owner;
    return state.overlay === "session_settings"
      && uiState.sessionSettings.dirty
      && owner !== null
      && sameSessionSettingsTarget(confirmation.expectedTarget, owner)
      && (
        target === null
        || (
          sameSessionSettingsRootOwner(confirmation.expectedTarget, target)
          && sameSessionSettingsRootOwner(owner, target)
        )
      );
  }
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
  return status === "completed" || status === "cancelled" || status === "failed";
}

function isThreadNearEnd(thread: HTMLElement): boolean {
  return thread.scrollHeight - thread.scrollTop - thread.clientHeight <= THREAD_END_THRESHOLD_PX;
}

function resolveCurrentThread(): HTMLElement | null {
  return document.querySelector<HTMLElement>("#thread");
}

function syncInactiveThreadViewport(): boolean {
  return syncResolvedInactiveThreadViewport(
    threadTailFollow,
    resolveCurrentThread,
    isThreadNearEnd,
  );
}

function scheduleInactiveThreadViewportSync(): void {
  window.requestAnimationFrame(() => {
    syncInactiveThreadViewport();
  });
}

function revealThreadEnd(generation: number): void {
  const scroll = () => {
    if (generation !== threadEndRevealGeneration) return;
    pinResolvedThreadToEnd(resolveCurrentThread);
  };
  scroll();
  requestAnimationFrame(scroll);
  window.setTimeout(scroll, 50);
}

function noteUserThreadScrollAway(): void {
  threadEndRevealGeneration += 1;
  threadTailFollow.noteUserScrollAway();
}

function wireThreadTailFollowEvents(thread: HTMLElement): void {
  let observationFrame: number | null = null;
  const observeUserViewport = () => {
    if (observationFrame !== null) window.cancelAnimationFrame(observationFrame);
    observationFrame = window.requestAnimationFrame(() => {
      observationFrame = null;
      const currentThread = resolveCurrentThread();
      if (!currentThread) return;
      threadTailFollow.noteUserViewport(isThreadNearEnd(currentThread));
    });
  };
  const moveAway = () => {
    noteUserThreadScrollAway();
    observeUserViewport();
  };
  thread.addEventListener("scroll", syncInactiveThreadViewport, { passive: true });
  thread.addEventListener("wheel", (event) => {
    if (event.deltaY < 0) {
      moveAway();
    } else {
      observeUserViewport();
    }
  }, { passive: true });
  thread.addEventListener("touchmove", moveAway, { passive: true });
  thread.addEventListener("touchend", observeUserViewport, { passive: true });
  thread.addEventListener("touchcancel", observeUserViewport, { passive: true });
  thread.addEventListener("pointerdown", (event) => {
    const bounds = thread.getBoundingClientRect();
    if (event.clientX < bounds.right - 20) return;
    moveAway();
    const pointerId = event.pointerId;
    const finishPointerScroll = (finishedEvent: PointerEvent) => {
      if (finishedEvent.pointerId !== pointerId) return;
      document.removeEventListener("pointerup", finishPointerScroll, true);
      document.removeEventListener("pointercancel", finishPointerScroll, true);
      observeUserViewport();
    };
    document.addEventListener("pointerup", finishPointerScroll, true);
    document.addEventListener("pointercancel", finishPointerScroll, true);
  });
  thread.addEventListener("keydown", (event) => {
    if (["ArrowUp", "PageUp", "Home"].includes(event.key)) {
      moveAway();
    } else if (["ArrowDown", "PageDown", "End"].includes(event.key)) {
      observeUserViewport();
    }
  });
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

function reportError(value: unknown): void {
  const error = humanizeError(value);
  if (currentState) {
    uiState.recoverableError = error;
    uiState.recoverableErrorOwner = settingsRecoverableErrorOwnerIdentity(
      projectViewState(currentState, uiState),
      uiState.initialSetup.step,
    );
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

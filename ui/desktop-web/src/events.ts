import { command } from "./api.ts";
import { editHubField } from "./hub_state.ts";
import { editDeviceNetworkField } from "./device_network_state.ts";
import { editMcpPeerField } from "./mcp_peer.ts";
import {
  actionEnabledById,
  dispatchAction,
  persistSideChatDraft,
  type ActionContext,
  type ActionPayload,
} from "./actions.ts";
import {
  configDraftAppliesTo,
  configMutationValues,
  reconcileConfigDraftTarget,
  type ConfigValueInput,
  updateConfigDraftValue,
} from "./config_mutation.ts";
import {
  permissionDecisionForEscape,
} from "./decision_state.ts";
import { validateInitialSetupStep } from "./initial_setup_state.ts";
import {
  confirmationFocusIsMeaningful,
  confirmationFocusSelectors,
  isRegularModalOverlay,
  localModalIdentity,
  modalIdentity,
  modalIsOpen,
  overlayPrimaryFocusRequired,
  overlayPrimaryFocusSelectors,
} from "./modal_state.ts";
import { containDialogFocus } from "./dialog_focus.ts";
import type { PostRenderFocusIntent } from "./focus_arbiter.ts";
import {
  globalShortcutAction,
  modalShortcutShouldPreventDefault,
  type KeyboardShortcutSample,
} from "./keyboard_shortcut.ts";
import { repeatedNewSessionPointerActivation } from "./new_session_mutation.ts";
import {
  abandonQuickChatDeleteFocusContinuation,
} from "./quick_chat_delete_focus_continuation.ts";
import { composerSendTitle, sideChatCatalogStatusText } from "./render.ts";
import {
  pendingSideChatQuoteFromDomSelection,
  sideChatQuoteKeyboardActivation,
  sideChatQuoteOwnerSessionIdFromTrigger,
} from "./side_chat_quote.ts";
import {
  sameSessionSettingsTarget,
  updateSessionSettingsDraft,
  type SessionSettingsDraftField,
} from "./session_settings_state.ts";
import {
  applyTitlebarMenuRovingTabIndex,
  TitlebarDragGesture,
  titlebarMenuFromOverlay,
  titlebarMenuKeyboardDecision,
  titlebarMenuTabContinuationAction,
  titlebarMenuTriggerAction,
  titlebarMenuUsesRovingFocus,
  windowControlKeyboardActivation,
} from "./titlebar_interaction.ts";
import type {
  ConfigFieldProjection,
  ConfigMutationTarget,
  DesktopViewState,
  DesktopWebState,
  ProviderProfile,
  SideChatPendingQuote,
} from "./types.ts";
import {
  recordSideChatCatalogConfigEdit,
  sideChatCatalogViewForState,
  sideChatDeleteConfirmationStillTargets,
  sideChatDraftForState,
  sideChatModelOptionLabel,
  sideChatModelOptions,
  sideChatMutationPending,
  sideChatOperationsOpen,
  sessionSettingsMutationAvailability,
  updateSideChatDraftFromManualEdit,
  type UiLocalState,
} from "./ui_state.ts";
import {
  goalSlashCommandHint,
  providerOverlayFeedback,
  validateConfigFieldValues,
  validateConfigInput,
} from "./utils.ts";
import {
  activateSettingsSectionNavigation,
  settingsSurfaceIdentity,
} from "./settings_surface.ts";
import {
  beginMainRunFocusContinuation,
  pointerTargetsMainRunControl,
} from "./run_focus_continuation.ts";
import {
  beginSideChatFocusContinuation,
  invalidateSideChatFocusInteraction,
  pointerTargetsSideChatRunControl,
} from "./side_chat_focus_continuation.ts";
import {
  invalidateRefreshPromptFocus,
  recordRefreshPointerInteraction,
  wireMainPromptInputOnce,
} from "./main_prompt_continuity.ts";
import {
  composerOwner,
  configDraftEditOpen,
  draftMutationTarget,
  localSearchOwner,
  normalizeProviderBaseUrl,
  sessionSearchMutationTarget,
  sessionSearchOwner,
  synchronizeInitialSetupProviderDraft,
} from "./view_state.ts";

let pendingOpacityPreviewPercent: number | null = null;
let opacityPreviewFrame: number | null = null;
let opacityPreviewInFlight = false;
let delegatedEventsInstalled = false;
const wiredOpacityInputs = new WeakSet<HTMLInputElement>();
interface CapturedSideChatPointerQuote {
  ownerSessionId: string;
  quote: SideChatPendingQuote;
}

const sideChatPointerQuotes = new WeakMap<Element, CapturedSideChatPointerQuote>();
const TEXT_MUTATION_DEBOUNCE_MS = 180;
const SIDE_CHAT_DRAFT_DEBOUNCE_MS = 450;
const MIN_WINDOW_OPACITY_PERCENT = 50;
const MAX_WINDOW_OPACITY_PERCENT = 100;

type SettingsControl = HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement;

interface PendingTextMutation {
  name: string;
  args: Record<string, unknown> | null;
  timer: number | null;
  inFlight: Promise<void> | null;
  renderResult: boolean;
  target: string;
  generation: number;
}

export interface TextMutationSettlementOwner {
  readonly hasQueuedValue: boolean;
  readonly currentGeneration: number;
  readonly requestGeneration: number;
  readonly targetStillMatches: boolean;
}

const pendingTextMutations = new Map<string, PendingTextMutation>();
let nextTextMutationGeneration = 1;
const titlebarDragGesture = new TitlebarDragGesture();
const NON_TEXT_INPUT_TYPES = new Set([
  "button",
  "checkbox",
  "color",
  "file",
  "hidden",
  "image",
  "radio",
  "range",
  "reset",
  "submit",
]);
const TEXT_SELECTION_INPUT_TYPES = new Set(["text", "search", "tel", "url", "password"]);

interface SettingsSelectAllShortcutSample {
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
}

export function installGlobalKeyboardShortcuts(context: ActionContext): void {
  document.addEventListener("keydown", (event) => {
    if (event.isComposing || event.keyCode === 229) return;
    const currentState = context.getViewState();
    const target = event.target;
    const sideChatDeleteTarget = currentState && sideChatDeleteConfirmationStillTargets(
      context.uiState.sideChatDeleteConfirmation,
      currentState,
    ) ? context.uiState.sideChatDeleteConfirmation : null;
    const activeLocalModalIdentity = localModalIdentity(
      context.uiState.pendingLocalConfirmation !== null,
      sideChatDeleteTarget,
    );
    if (
      event.key === "Escape"
      && currentState
      && !currentState.confirmation_visible
      && activeLocalModalIdentity?.startsWith("side-chat-delete:")
      && sideChatDeleteTarget
    ) {
      event.preventDefault();
      if (!sideChatMutationPending(context.uiState, sideChatDeleteTarget.ownerSessionId)) {
        void dispatchAction(
          "cancel-delete-side-chat",
          context,
          { index: -1, value: "" },
        ).catch((error) => context.reportError(error));
      }
      return;
    }
    if (currentState && handleTitlebarMenuKeyboard(event, currentState, context)) return;
    if (
      currentState &&
      modalIsOpen(currentState, activeLocalModalIdentity !== null)
    ) {
      if (handleSettingsSelectAllShortcut(event, currentState)) {
        event.preventDefault();
      } else if (event.key === "Tab") {
        trapDialogFocus(event);
      } else if (event.key === "Escape" && currentState.confirmation_visible) {
        event.preventDefault();
        if (permissionDecisionForEscape(currentState.confirmation_visible, event.repeat, Boolean(currentState.confirmation?.remote)) === "abort") {
          void dispatchAction("abort-permission", context, { index: -1, value: "" });
        }
      } else if (event.key === "Escape" && context.uiState.pendingLocalConfirmation) {
        event.preventDefault();
        void dispatchAction("cancel-local-confirm", context, { index: -1, value: "" })
          .catch((error) => context.reportError(error));
      } else if (event.key === "Escape" && currentState.overlay === "initial_setup") {
        event.preventDefault();
      } else if (event.key === "Escape" && isRegularModalOverlay(currentState.overlay)) {
        event.preventDefault();
        if (!startupSetupRequired(currentState)) dismissOverlayForState(currentState, context);
      } else if (modalShortcutShouldPreventDefault(
        event,
        isRegularModalOverlay(currentState.overlay) && (
          isNativeTextEditingTarget(target)
          || (event.key.toLowerCase() === "c"
            && !currentState.confirmation_visible
            && activeLocalModalIdentity === null
            && nativeModalCopySelectionAvailable(target))
        ),
      )) {
        event.preventDefault();
      }
      return;
    }
    const shortcutAction = shortcutActionForComposer(
      event,
      target instanceof Element && target.closest("#side-chat-prompt") !== null,
    );
    if (shortcutAction && currentState) {
      event.preventDefault();
      void dispatchAction(shortcutAction, context, {
        index: -1,
        value: "",
        activationSource: "shortcut",
      });
    }
    if (event.key === "Escape" && currentState && currentState.overlay !== "none") {
      event.preventDefault();
      if (startupSetupRequired(currentState)) return;
      dismissOverlayForState(currentState, context);
    }
  });
}

export function overlayDismissAction(overlay: string): "cancel-review" | "close-overlay" {
  return overlay === "prompt_review" ? "cancel-review" : "close-overlay";
}

function startupSetupRequired(state: DesktopWebState): boolean {
  return state.startup.initial_setup_required && state.startup.action_overlay === state.overlay;
}

function dismissOverlayForState(state: DesktopViewState, context: ActionContext): void {
  void dispatchAction(overlayDismissAction(state.overlay), context, { index: -1, value: "" }).catch((error) =>
    context.reportError(error),
  );
}

function handleTitlebarMenuKeyboard(
  event: KeyboardEvent,
  state: DesktopViewState,
  context: ActionContext,
): boolean {
  if (!titlebarMenuFromOverlay(state.overlay)) return false;
  const target = event.target;
  if (!(target instanceof Element)) return false;
  const popover = target.closest<HTMLElement>(".titlebar-popover[data-titlebar-menu]");
  if (!popover) return false;
  if (event.key === "Escape" && event.repeat) {
    event.preventDefault();
    event.stopPropagation();
    return true;
  }
  const actions = Array.from(
    popover.querySelectorAll<HTMLElement>(
      "button[data-titlebar-menu-action]:not(:disabled):not([aria-disabled='true'])",
    ),
  );
  const currentAction = target.closest<HTMLElement>("button[data-titlebar-menu-action]");
  const nativeWidget = !titlebarMenuUsesRovingFocus(popover.getAttribute("role"))
    || target.closest("input, select, textarea, [contenteditable='true']") !== null;
  const decision = titlebarMenuKeyboardDecision(
    event.key,
    currentAction ? actions.indexOf(currentAction) : -1,
    actions.length,
    nativeWidget,
  );
  if (decision.kind === "native") return false;
  if (decision.kind === "close-natural") {
    if (event.repeat) {
      event.preventDefault();
      event.stopPropagation();
      return true;
    }
    const triggerAction = titlebarMenuTriggerAction(state.overlay);
    const titlebarActions = Array.from(
      document.querySelectorAll<HTMLElement>(
        ".app-titlebar button[data-action]:not(:disabled):not([aria-disabled='true'])",
      ),
      (candidate) => candidate.dataset.action ?? "",
    ).filter((action) => action.length > 0);
    const continuationAction = triggerAction
      ? titlebarMenuTabContinuationAction(titlebarActions, triggerAction, event.shiftKey) ?? triggerAction
      : null;
    context.uiState.titlebarMenuFocusContinuation = continuationAction
      ? { overlay: state.overlay, action: continuationAction }
      : null;
    event.preventDefault();
    event.stopPropagation();
    if (!startupSetupRequired(state)) {
      void dispatchAction("close-overlay", context, { index: -1, value: "" })
        .catch((error) => context.reportError(error));
    }
    return true;
  }
  event.preventDefault();
  event.stopPropagation();
  if (decision.kind === "close") {
    const triggerAction = titlebarMenuTriggerAction(state.overlay);
    context.uiState.titlebarMenuFocusContinuation = triggerAction
      ? { overlay: state.overlay, action: triggerAction }
      : null;
    if (!startupSetupRequired(state)) {
      void dispatchAction("close-overlay", context, { index: -1, value: "" })
        .catch((error) => context.reportError(error));
    }
  } else {
    applyTitlebarMenuRovingTabIndex(actions, decision.index);
    actions[decision.index]?.focus({ preventScroll: true });
  }
  return true;
}

function isNativeTextEditingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  if (target instanceof HTMLTextAreaElement) return !target.disabled && !target.readOnly;
  if (target instanceof HTMLInputElement) {
    return !target.disabled
      && !target.readOnly
      && !NON_TEXT_INPUT_TYPES.has(target.type.toLowerCase());
  }
  return target.closest('[contenteditable="true"], [contenteditable="plaintext-only"]') !== null;
}

function nativeModalCopySelectionAvailable(target: EventTarget | null): boolean {
  if (!(target instanceof Element) || !target.isConnected) return false;
  const modal = target.closest('[data-modal][role="dialog"]');
  if (!modal) return false;
  if (target instanceof HTMLTextAreaElement) return !target.disabled;
  if (target instanceof HTMLInputElement) {
    return !target.disabled && !NON_TEXT_INPUT_TYPES.has(target.type.toLowerCase());
  }
  // Read-only text such as the original prompt uses the document's native selection,
  // even when focus stays on the containing dialog rather than an editable control.
  const selection = target.ownerDocument.getSelection();
  return selection !== null && selection.rangeCount > 0 && !selection.isCollapsed
    && modal.contains(selection.anchorNode) && modal.contains(selection.focusNode);
}

/**
 * Keeps Select All owned by the current Settings editor even when WebView2 temporarily routes
 * the native Ctrl+A default action to the document while a Tauri command is pending. The current
 * connected active element is the only eligible owner, so a stale async completion cannot select
 * text in a detached or replacement Settings surface.
 */
export function handleSettingsSelectAllShortcut(
  sample: SettingsSelectAllShortcutSample,
  state: Pick<DesktopViewState, "overlay" | "confirmation_visible">,
  activeElement: Element | null = document.activeElement,
): boolean {
  if (
    !["config", "session_settings", "initial_setup"].includes(state.overlay)
    || state.confirmation_visible
    || !(sample.ctrlKey || sample.metaKey)
    || sample.altKey
    || sample.key.toLowerCase() !== "a"
    || !activeElement?.isConnected
    || (
      activeElement.closest(".settings-modal") === null
      && activeElement.closest(".initial-setup-shell") === null
    )
  ) return false;

  if (activeElement instanceof HTMLTextAreaElement) {
    if (activeElement.disabled || activeElement.readOnly) return false;
    activeElement.setSelectionRange(0, activeElement.value.length);
    return true;
  }
  if (
    activeElement instanceof HTMLInputElement
    && !activeElement.disabled
    && !activeElement.readOnly
    && TEXT_SELECTION_INPUT_TYPES.has(activeElement.type.toLowerCase())
  ) {
    activeElement.setSelectionRange(0, activeElement.value.length);
    return true;
  }
  return false;
}

export function wireEvents(state: DesktopViewState, context: ActionContext): void {
  installDelegatedActionEvents(context);
  const prompt = document.querySelector<HTMLTextAreaElement>("#prompt");
  if (prompt) {
    resizePromptComposer(prompt);
    wireMainPromptInputOnce(prompt, (event) => {
      const prompt = event.currentTarget as HTMLTextAreaElement;
      const text = prompt.value;
      resizePromptComposer(prompt);
      context.uiState.drafts.prompt = text;
      context.uiState.drafts.composerRevision += 1;
      const projection = context.getProjection();
      if (projection) {
        updateGoalCommandHint(text);
        const send = document.querySelector<HTMLButtonElement>('[data-action="send"]');
        if (send) {
          synchronizeActionButtonAvailability(send, context);
          const title = composerSendTitle(projection, text);
          send.title = title;
          send.setAttribute("aria-label", title);
        }
        const enhance = document.querySelector<HTMLButtonElement>('[data-action="enhance-prompt"]');
        if (enhance) {
          synchronizeActionButtonAvailability(enhance, context);
          const title = projection.navigation_loading
            ? "画面の切り替え完了後にEnhanceできます"
            : projection.busy
              ? "実行中はEnhanceできません"
            : text.trim().length === 0
              ? "依頼文を入力してください"
              : "Enhance";
          enhance.title = title;
          enhance.setAttribute("aria-label", title);
        }
      }
      resizePromptComposer(prompt);
    });
  }
  const sideChatDraft = sideChatDraftForState(context.uiState, state);
  document.querySelector<HTMLTextAreaElement>("#side-chat-prompt")?.addEventListener("input", (event) => {
    if (!sideChatDraft || !sideChatOperationsOpen(context.uiState)) return;
    const pendingQuoteCleared = sideChatDraft.pendingQuote !== null;
    updateSideChatDraftFromManualEdit(
      sideChatDraft,
      (event.currentTarget as HTMLTextAreaElement).value,
    );
    if (pendingQuoteCleared) {
      document.querySelector(".side-chat-pending-quote")?.remove();
    }
    if (sideChatDraft.saveTimer !== null) window.clearTimeout(sideChatDraft.saveTimer);
    sideChatDraft.saveTimer = window.setTimeout(() => {
      sideChatDraft.saveTimer = null;
      const projection = context.getProjection();
      if (projection) void persistSideChatDraft(projection, context);
    }, SIDE_CHAT_DRAFT_DEBOUNCE_MS);
    updateSideChatActionButtons(state, context);
  });
  document.querySelector<HTMLTextAreaElement>("#side-chat-prompt")?.addEventListener("change", () => {
    if (!sideChatDraft) return;
    if (sideChatDraft.saveTimer !== null) window.clearTimeout(sideChatDraft.saveTimer);
    sideChatDraft.saveTimer = null;
    const projection = context.getProjection();
    if (projection) void persistSideChatDraft(projection, context);
  });
  document.querySelector<HTMLInputElement>("#image-input")?.addEventListener("input", (event) => {
    context.uiState.drafts.imageInput = (event.currentTarget as HTMLInputElement).value;
    context.uiState.drafts.imageRevision += 1;
  });
  document.querySelector<HTMLInputElement>("#provider-url")?.addEventListener("input", (event) => {
    const next = (event.currentTarget as HTMLInputElement).value;
    if (
      normalizeProviderBaseUrl(next)
      !== normalizeProviderBaseUrl(context.uiState.drafts.provider.baseUrl)
    ) context.uiState.drafts.providerCatalogIdentityRevision += 1;
    context.uiState.drafts.provider.baseUrl = next;
    context.uiState.drafts.providerRevision += 1;
    updateProviderActionButtons(context);
  });
  document.querySelector<HTMLSelectElement>("#provider-profile")?.addEventListener("change", (event) => {
    const next = (event.currentTarget as HTMLSelectElement).value;
    if (!isProviderProfile(next)) return;
    if (context.uiState.drafts.provider.providerProfile !== next) {
      context.uiState.drafts.providerCatalogIdentityRevision += 1;
    }
    context.uiState.drafts.provider.providerProfile = next;
    context.uiState.drafts.providerRevision += 1;
    updateProviderActionButtons(context);
  });
  document.querySelector<HTMLInputElement>("#provider-api-key-env")?.addEventListener("input", (event) => {
    const next = (event.currentTarget as HTMLInputElement).value;
    if (next.trim() !== context.uiState.drafts.provider.apiKeyEnv.trim()) {
      context.uiState.drafts.providerCatalogIdentityRevision += 1;
    }
    context.uiState.drafts.provider.apiKeyEnv = next;
    context.uiState.drafts.providerRevision += 1;
    updateProviderActionButtons(context);
  });
  document.querySelector<HTMLInputElement>("#provider-context-window")?.addEventListener("input", (event) => {
    context.uiState.drafts.provider.contextWindow = (event.currentTarget as HTMLInputElement).value;
    context.uiState.drafts.providerRevision += 1;
    updateProviderActionButtons(context);
  });
  const settingsControls = collectSettingsControls();
  if (settingsControls.length > 0) {
    validateSettingsForm(context, state.config_fields, false);
  }
  if (state.overlay === "session_settings") updateSessionSettingsControls(state, context);
  updateSideChatActionButtons(state, context);
  document.querySelector<HTMLInputElement>("#workspace-input")?.addEventListener("input", (event) => {
    context.uiState.drafts.workspaceInput = (event.currentTarget as HTMLInputElement).value;
    context.uiState.drafts.workspaceRevision += 1;
  });
  const localSearch = document.querySelector<HTMLInputElement>("#local-search");
  const updateLocalSearch = (input: HTMLInputElement, commit: boolean) => {
    const text = input.value;
    context.uiState.drafts.localSearch = text;
    if (commit) scheduleTextMutation(
      "local-search",
      "set_local_search",
      { text, expectedTarget: draftMutationTarget(state) },
      context,
      localSearchOwner(state),
      true,
    );
  };
  localSearch?.addEventListener("input", (event) => {
    updateLocalSearch(event.currentTarget as HTMLInputElement, !(event as InputEvent).isComposing);
  });
  localSearch?.addEventListener("compositionend", (event) => {
    updateLocalSearch(event.currentTarget as HTMLInputElement, true);
  });
  const sessionSearch = document.querySelector<HTMLInputElement>("#session-search");
  const updateSessionSearch = (input: HTMLInputElement, commit: boolean) => {
    const text = input.value;
    context.uiState.drafts.sessionSearch = text;
    if (commit) scheduleTextMutation(
      "session-search",
      "set_session_search",
      { text, expectedTarget: sessionSearchMutationTarget(state) },
      context,
      sessionSearchOwner(state),
      true,
    );
  };
  sessionSearch?.addEventListener("input", (event) => {
    updateSessionSearch(event.currentTarget as HTMLInputElement, !(event as InputEvent).isComposing);
  });
  sessionSearch?.addEventListener("compositionend", (event) => {
    updateSessionSearch(event.currentTarget as HTMLInputElement, true);
  });
  document.querySelector<HTMLTextAreaElement>("#review-draft")?.addEventListener("input", (event) => {
    context.uiState.drafts.reviewDraft = (event.currentTarget as HTMLTextAreaElement).value;
    context.uiState.drafts.reviewRevision += 1;
    updateReviewActionButtons(context);
  });
  const opacityInput = document.querySelector<HTMLInputElement>("#opacity-input");
  if (opacityInput && !wiredOpacityInputs.has(opacityInput)) {
    wiredOpacityInputs.add(opacityInput);
    opacityInput.addEventListener("input", (event) => {
      const input = event.currentTarget as HTMLInputElement;
      const percent = clampOpacityPercent(Number(input.value));
      input.setAttribute("aria-valuetext", `${percent}%`);
      scheduleOpacityPreview(percent, context);
    });
    opacityInput.addEventListener("change", (event) => {
      void context.mutate("set_window_opacity", {
        percent: clampOpacityPercent(Number((event.currentTarget as HTMLInputElement).value)),
      });
    });
  }
}

function actionPayloadValue(node: HTMLElement): string {
  return node.dataset.agentPath
    ?? node.dataset.historyTarget
    ?? node.dataset.providerProfile
    ?? node.dataset.mode
    ?? node.dataset.value
    ?? "";
}

function installDelegatedActionEvents(context: ActionContext): void {
  if (delegatedEventsInstalled) return;
  delegatedEventsInstalled = true;
  const invalidateSideChatFocusContinuation = () => {
    invalidateSideChatFocusInteraction(context.uiState);
  };
  const invalidateQuickChatDeleteFocusContinuation = () => {
    context.uiState.quickChatDeleteFocusContinuation = abandonQuickChatDeleteFocusContinuation(
      context.uiState.quickChatDeleteFocusContinuation,
    );
  };
  const invalidateRefreshFocusContinuation = () => {
    invalidateRefreshPromptFocus(context.uiState);
  };
  const invalidateCommandPaletteInsertion = () => {
    context.invalidateCommandPaletteInsertion();
  };
  document.addEventListener("pointerdown", invalidateCommandPaletteInsertion, true);
  document.addEventListener("keydown", (event) => {
    if (shouldInvalidateCommandPaletteInsertionForKeydown(event.repeat)) {
      invalidateCommandPaletteInsertion();
    }
  }, true);
  document.addEventListener("compositionstart", invalidateCommandPaletteInsertion, true);
  document.addEventListener("input", invalidateCommandPaletteInsertion, true);
  document.addEventListener("wheel", invalidateCommandPaletteInsertion, { capture: true, passive: true });
  window.addEventListener("blur", invalidateCommandPaletteInsertion);
  window.addEventListener("pagehide", invalidateCommandPaletteInsertion);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") invalidateCommandPaletteInsertion();
  });
  document.addEventListener("pointerdown", invalidateSideChatFocusContinuation, true);
  document.addEventListener("keydown", invalidateSideChatFocusContinuation, true);
  document.addEventListener("compositionstart", invalidateSideChatFocusContinuation, true);
  document.addEventListener("wheel", invalidateSideChatFocusContinuation, { capture: true, passive: true });
  document.addEventListener("pointerdown", invalidateQuickChatDeleteFocusContinuation, true);
  document.addEventListener("keydown", invalidateQuickChatDeleteFocusContinuation, true);
  document.addEventListener("compositionstart", invalidateQuickChatDeleteFocusContinuation, true);
  document.addEventListener("wheel", invalidateQuickChatDeleteFocusContinuation, { capture: true, passive: true });
  document.addEventListener("keydown", invalidateRefreshFocusContinuation, true);
  document.addEventListener("compositionstart", invalidateRefreshFocusContinuation, true);
  document.addEventListener("wheel", invalidateRefreshFocusContinuation, { capture: true, passive: true });
  window.addEventListener("blur", invalidateRefreshFocusContinuation);
  window.addEventListener("pagehide", invalidateRefreshFocusContinuation);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") invalidateRefreshFocusContinuation();
  });
  const updateSettingsControl = (event: Event) => {
    const target = event.target;
    if (
      !(target instanceof HTMLInputElement)
      && !(target instanceof HTMLTextAreaElement)
      && !(target instanceof HTMLSelectElement)
    ) {
      return;
    }
    if (target.dataset.mcpPeerField !== undefined) {
      if (context.getViewState()?.overlay !== "config") return;
      editMcpPeerField(context.uiState.mcpPeers, target.dataset.mcpPeerField, target.value);
      context.rerender();
      return;
    }
    if (target.dataset.networkField !== undefined) {
      if (context.getViewState()?.overlay !== "hub") return;
      editDeviceNetworkField(context.uiState.deviceNetwork, target.dataset.networkField, target.value, target instanceof HTMLInputElement && target.checked);
      context.rerender();
      return;
    }
    if (target.dataset.hubField !== undefined) {
      if (context.getViewState()?.overlay !== "hub") return;
      editHubField(context.uiState.hub, target.dataset.hubField ?? "", target.value, target instanceof HTMLInputElement && target.checked);
      context.uiState.hub.error = "";
      context.rerender();
      return;
    }
    if (target.matches(".session-settings-control")) {
      const currentState = context.getViewState();
      const currentTarget = currentState?.session_settings.target ?? null;
      if (
        !currentState
        || currentState.overlay !== "session_settings"
        || currentTarget === null
        || !sameSessionSettingsTarget(context.uiState.sessionSettings.owner, currentTarget)
      ) return;
      const field = sessionSettingsDraftField(target.dataset.sessionSetting ?? "");
      if (
        field !== null
        && updateSessionSettingsDraft(
          context.uiState.sessionSettings,
          currentTarget,
          field,
          target.value,
        )
      ) {
        updateSessionSettingsControls(currentState, context);
      }
      return;
    }
    if (!target.matches(".settings-control") || !updateSettingsControlDraft(target, context)) return;
    synchronizeProviderModelControls(target);
    const currentState = context.getViewState();
    if (currentState) {
      synchronizeInitialSetupProviderDraft(currentState, context.uiState);
      validateSettingsForm(context, currentState.config_fields, false);
      if (target.dataset.configKey?.startsWith("side_chat.")) {
        synchronizeSideChatCatalogControls(currentState, context, configDraftEditOpen(context.uiState));
      }
      if (target.dataset.configKey?.startsWith("model.")) {
        // Keep the connected URL input, but retire options belonging to its previous target.
        context.rerender();
      }
    }
    if (event.type === "change" && target.dataset.configKey === "docling.enabled") {
      context.rerender();
    }
  };
  document.addEventListener("input", updateSettingsControl);
  document.addEventListener("change", updateSettingsControl);
  document.addEventListener("focusout", (event) => {
    if (event.target instanceof Element && event.target.closest(".hub-modal")) {
      // Apply a deferred passive model-list refresh only after the user leaves its controls.
      queueMicrotask(() => { if (context.getViewState()?.overlay === "hub") context.rerender(); });
    }
  });
  document.addEventListener("pointerdown", (event) => {
    const target = event.target;
    if (!(target instanceof Element)) return;
    const trigger = target.closest<HTMLElement>('[data-action="quote-selection-to-side-chat"]');
    const currentState = context.getViewState();
    if (!trigger || !currentState) return;
    sideChatPointerQuotes.delete(trigger);
    const quote = pendingSideChatQuoteFromDomSelection(
      trigger,
      window.getSelection(),
      currentState.side_chat.context_as_of_append_position,
    );
    const ownerSessionId = sideChatQuoteOwnerSessionIdFromTrigger(trigger);
    if (quote && ownerSessionId) {
      sideChatPointerQuotes.set(trigger, { ownerSessionId, quote });
      window.setTimeout(() => sideChatPointerQuotes.delete(trigger), 0);
    }
  }, true);
  document.addEventListener("click", (event) => {
    const target = event.target;
    if (!(target instanceof Element)) return;
    const currentState = context.getViewState();
    if (!currentState) return;
    if (handleSettingsNavigationClick(event, currentState)) return;
    const node = target.closest<HTMLElement>("[data-action]");
    if (!node || (node instanceof HTMLButtonElement && node.disabled)) return;
    if (
      node.hasAttribute("data-window-control")
      && titlebarDragGesture.consumeWindowControlClickSuppression(event.detail > 0)
    ) {
      event.preventDefault();
      event.stopPropagation();
      return;
    }
    if (
      target.closest("[data-modal]") &&
      (node.classList.contains("modal-backdrop") || node.classList.contains("menu-scrim"))
    ) {
      return;
    }
    const action = node.dataset.action ?? "";
    if (repeatedNewSessionPointerActivation(action, event.detail)) return;
    context.uiState.mainRunFocusContinuation = beginMainRunFocusContinuation(
      currentState,
      action,
    );
    context.uiState.sideChatFocusContinuation = beginSideChatFocusContinuation(
      currentState,
      action,
    );
    const index = Number(node.dataset.index ?? "-1");
    const value = actionPayloadValue(node);
    const capturedSideChatQuote = action === "quote-selection-to-side-chat"
      ? sideChatPointerQuotes.get(node) ?? null
      : null;
    const sideChatQuote = action === "quote-selection-to-side-chat"
      ? pendingSideChatQuoteFromDomSelection(
        node,
        window.getSelection(),
        currentState.side_chat.context_as_of_append_position,
      ) ?? capturedSideChatQuote?.quote ?? null
      : undefined;
    const sideChatQuoteOwnerSessionId = action === "quote-selection-to-side-chat"
      ? sideChatQuoteOwnerSessionIdFromTrigger(node) ?? capturedSideChatQuote?.ownerSessionId ?? null
      : undefined;
    if (action === "quote-selection-to-side-chat") sideChatPointerQuotes.delete(node);
    void dispatchAction(action, context, {
      index,
      value,
      sideChatQuote,
      sideChatQuoteOwnerSessionId,
    })
      .catch((error) => context.reportError(error));
  });
  document.addEventListener("keydown", (event) => {
    const target = event.target;
    if (!(target instanceof Element)) return;
    const quoteNode = target.closest<HTMLElement>('[data-action="quote-selection-to-side-chat"]');
    if (
      quoteNode
      && !(quoteNode instanceof HTMLButtonElement && quoteNode.disabled)
      && sideChatQuoteKeyboardActivation(event.key, event.repeat)
    ) {
      const currentState = context.getViewState();
      if (!currentState) return;
      const sideChatQuote = pendingSideChatQuoteFromDomSelection(
        quoteNode,
        window.getSelection(),
        currentState.side_chat.context_as_of_append_position,
      );
      const sideChatQuoteOwnerSessionId = sideChatQuoteOwnerSessionIdFromTrigger(quoteNode);
      if (!sideChatQuote || !sideChatQuoteOwnerSessionId) return;
      event.preventDefault();
      event.stopPropagation();
      void dispatchAction("quote-selection-to-side-chat", context, {
        index: Number(quoteNode.dataset.index ?? "-1"),
        value: "",
        sideChatQuote,
        sideChatQuoteOwnerSessionId,
      }).catch((error) => context.reportError(error));
      return;
    }
    if (event.repeat || (event.key !== "Enter" && event.key !== " ")) return;
    const node = target.closest<HTMLElement>(
      '[data-action="show-agent-pane"], [data-action="show-agent-list"], [data-action="show-output-pane"], [data-action="jump-history-anchor"]',
    );
    if (!node || (node instanceof HTMLButtonElement && node.disabled)) return;
    if (!shouldDispatchDelegatedKeyboardAction(node.tagName, node.hasAttribute("href"))) return;
    const currentState = context.getViewState();
    if (!currentState) return;
    event.preventDefault();
    event.stopPropagation();
    const action = node.dataset.action ?? "";
    const index = Number(node.dataset.index ?? "-1");
    const value = actionPayloadValue(node);
    void dispatchAction(action, context, { index, value }).catch((error) => context.reportError(error));
  });
  document.addEventListener("pointerdown", (event) => {
    const target = event.target;
    if (!(target instanceof Element)) return;
    const projection = context.getProjection();
    const prompt = document.querySelector<HTMLTextAreaElement>("#prompt");
    recordRefreshPointerInteraction(context.uiState, {
      owner: projection ? composerOwner(projection) : "",
      targetsRefresh: target.closest('[data-action="refresh"]') !== null,
      promptFocused: prompt !== null && document.activeElement === prompt,
    });
    if (
      context.uiState.mainRunFocusContinuation
      && !pointerTargetsMainRunControl(target)
    ) {
      context.uiState.mainRunFocusContinuation = null;
    }
    if (
      context.uiState.sideChatFocusContinuation
      && !pointerTargetsSideChatRunControl(target)
    ) {
      context.uiState.sideChatFocusContinuation = null;
    }
    titlebarDragGesture.pointerDown(titlebarPointerSample(event, target));
  });
  document.addEventListener("pointermove", (event) => {
    const target = event.target;
    if (!(target instanceof Element)) return;
    if (!titlebarDragGesture.pointerMove(titlebarPointerSample(event, target))) return;
    event.preventDefault();
    event.stopPropagation();
    void command("start_window_drag").catch(() => context.desktopWindow.startDragging());
  });
  document.addEventListener("pointerup", (event) => titlebarDragGesture.pointerUp(event.pointerId));
  document.addEventListener("pointercancel", () => {
    titlebarDragGesture.cancel();
    invalidateRefreshFocusContinuation();
  });
  window.addEventListener("blur", () => titlebarDragGesture.cancel());
  document.addEventListener("dblclick", (event) => {
    const target = event.target;
    if (!(target instanceof Element)) return;
    if (
      !titlebarDragGesture.doubleClick({
        button: event.button,
        inDragRegion: target.closest("[data-drag-region]") !== null,
        inWindowControl: target.closest("[data-window-control]") !== null,
      })
    ) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    const currentState = context.getViewState();
    if (currentState) {
      void dispatchAction("toggle-maximize-window", context, { index: -1, value: "" }).catch((error) =>
        context.reportError(error),
      );
    }
  });
  document.addEventListener("keydown", (event) => {
    const target = event.target;
    if (!(target instanceof Element)) return;
    const control = target.closest<HTMLElement>("button[data-window-control]");
    if (!control || !windowControlKeyboardActivation(event.key, event.repeat)) return;
    event.preventDefault();
    event.stopPropagation();
    const currentState = context.getViewState();
    const action = control.dataset.action ?? "";
    if (currentState && action) {
      void dispatchAction(action, context, { index: -1, value: "" }).catch((error) =>
        context.reportError(error),
      );
    }
  });
}

export function shouldInvalidateCommandPaletteInsertionForKeydown(repeat: boolean): boolean {
  return !repeat;
}

export function handleSettingsNavigationClick(
  event: MouseEvent,
  state: DesktopViewState,
): boolean {
  if (state.overlay !== "config" || state.confirmation_visible) return false;
  const target = event.target;
  if (!(target instanceof Element)) return false;
  const anchor = target.closest<HTMLAnchorElement>(".settings-nav a[href^='#']");
  if (!anchor || !activateSettingsSectionNavigation(anchor)) return false;
  event.preventDefault();
  return true;
}

export function shouldDispatchDelegatedKeyboardAction(tagName: string, hasHref = false): boolean {
  const normalized = tagName.toUpperCase();
  if (["BUTTON", "INPUT", "SELECT", "TEXTAREA", "SUMMARY"].includes(normalized)) return false;
  return normalized !== "A" || !hasHref;
}

function titlebarPointerSample(event: PointerEvent, target: Element) {
  return {
    pointerId: event.pointerId,
    button: event.button,
    buttons: event.buttons,
    clientX: event.clientX,
    clientY: event.clientY,
    inDragRegion: target.closest("[data-drag-region]") !== null,
    inWindowControl: target.closest("[data-window-control]") !== null,
  };
}

function trapDialogFocus(event: KeyboardEvent): void {
  const dialog = Array.from(document.querySelectorAll<HTMLElement>(
    ".modal[role='dialog'], .modal[role='alertdialog'], .initial-setup-shell",
  )).reverse().find((candidate) => candidate.closest("[inert], [aria-hidden='true']") === null) ?? null;
  if (!dialog) return;
  event.preventDefault();
  containDialogFocus(dialog, document.activeElement, event.shiftKey);
}

function resizePromptComposer(prompt: HTMLTextAreaElement): void {
  prompt.style.height = "auto";
  const style = window.getComputedStyle(prompt);
  const maxHeight = Number.parseFloat(style.maxHeight);
  const nextHeight = Number.isFinite(maxHeight) ? Math.min(prompt.scrollHeight, maxHeight) : prompt.scrollHeight;
  prompt.style.height = `${Math.ceil(nextHeight)}px`;
  prompt.style.overflowY = Number.isFinite(maxHeight) && prompt.scrollHeight > maxHeight + 1 ? "auto" : "hidden";
  updateComposerReserve();
}

function updateComposerReserve(): void {
  const conversation = document.querySelector<HTMLElement>(".conversation");
  const composer = document.querySelector<HTMLElement>(".composer");
  if (!conversation || !composer) return;
  const reserve = Math.max(188, Math.ceil(composer.getBoundingClientRect().height + 42));
  conversation.style.setProperty("--composer-reserve", `${reserve}px`);
}

function updateGoalCommandHint(text: string): void {
  const hint = goalSlashCommandHint(text);
  document.querySelector<HTMLElement>(".composer")?.classList.toggle("goal-command", hint !== null);
  const hintNode = document.querySelector<HTMLElement>("#goal-command-hint");
  if (!hintNode) return;
  hintNode.hidden = hint === null;
  const helpNode = hintNode.querySelector<HTMLElement>("[data-goal-command-help]");
  if (helpNode) helpNode.textContent = hint ?? "";
}

function scheduleTextMutation(
  key: string,
  name: string,
  args: Record<string, unknown>,
  context: ActionContext,
  target: string,
  renderResult = false,
): void {
  const existing = pendingTextMutations.get(key);
  const entry = existing ?? {
    name,
    args: null,
    timer: null,
    inFlight: null,
    renderResult,
    target,
    generation: 0,
  };
  entry.name = name;
  entry.args = args;
  entry.renderResult = renderResult;
  entry.target = target;
  entry.generation = nextTextMutationGeneration++;
  if (entry.timer !== null) window.clearTimeout(entry.timer);
  entry.timer = window.setTimeout(() => {
    entry.timer = null;
    void flushTextMutation(key, context);
  }, TEXT_MUTATION_DEBOUNCE_MS);
  pendingTextMutations.set(key, entry);
}

async function flushTextMutation(key: string, context: ActionContext): Promise<void> {
  const entry = pendingTextMutations.get(key);
  if (!entry) return;
  // The invocation that owns the current request also drains the latest queued
  // value. Timer callbacks that arrive meanwhile must not build an await chain.
  if (entry.inFlight) return;
  while (entry.args !== null) {
    if (entry.timer !== null) {
      window.clearTimeout(entry.timer);
      entry.timer = null;
    }
    const name = entry.name;
    const args = entry.args;
    const renderResult = entry.renderResult;
    const target = entry.target;
    const generation = entry.generation;
    entry.args = null;
    // A debounce can outlive its palette or navigation owner. Reject it before IPC;
    // ignoring the eventual receipt cannot undo a stale command's backend status.
    if (!searchTargetStillMatches(key, target, context)) continue;
    const settlementOwner = (): TextMutationSettlementOwner => ({
      hasQueuedValue: entry.args !== null,
      currentGeneration: entry.generation,
      requestGeneration: generation,
      targetStillMatches: searchTargetStillMatches(key, target, context),
    });
    const request = command<DesktopWebState>(name, args)
      .then((state) => {
        if (textMutationSettlementOwnerIsCurrent(settlementOwner())) {
          context.acceptProjection(state, renderResult);
        }
      })
      .catch((error) => {
        settleTextMutationFailure(
          settlementOwner(),
          error,
          context.recoverCommandConflict,
          context.reportError,
        );
      });
    entry.inFlight = request;
    try {
      await request;
    } finally {
      if (entry.inFlight === request) entry.inFlight = null;
    }
  }
  if (pendingTextMutations.get(key) === entry && entry.timer === null && entry.inFlight === null) {
    pendingTextMutations.delete(key);
  }
}

function searchTargetStillMatches(key: string, target: string, context: ActionContext): boolean {
  const state = context.getProjection();
  if (!state) return false;
  return key === "session-search"
    ? sessionSearchOwner(state) === target
    : state.overlay === "command_palette" && localSearchOwner(state) === target;
}

function updateProviderActionButtons(context: ActionContext): void {
  const load = document.querySelector<HTMLButtonElement>('[data-action="load-provider-models"]');
  if (load) synchronizeActionButtonAvailability(load, context);
  document
    .querySelectorAll<HTMLButtonElement>('[data-action="apply-provider-session"], [data-action="save-provider-global"]')
    .forEach((button) => synchronizeActionButtonAvailability(button, context));
  const view = context.getViewState();
  if (view) {
    synchronizeProviderOverlayFeedback(view);
    document.querySelectorAll<HTMLButtonElement>('[data-action="select-provider-model"]').forEach((button) => {
      const index = Number(button.dataset.index);
      button.hidden = view.provider_model_ids[index] === undefined;
      synchronizeActionButtonAvailability(button, context);
    });
  }
}

export function textMutationSettlementOwnerIsCurrent(
  owner: TextMutationSettlementOwner,
): boolean {
  return !owner.hasQueuedValue
    && owner.currentGeneration === owner.requestGeneration
    && owner.targetStillMatches;
}

export function settleTextMutationFailure(
  owner: TextMutationSettlementOwner,
  error: unknown,
  recoverCommandConflict: (error: unknown) => boolean,
  reportError: (error: unknown) => void,
): boolean {
  if (!textMutationSettlementOwnerIsCurrent(owner)) return false;
  if (!recoverCommandConflict(error)) reportError(error);
  return true;
}

export function synchronizeProviderOverlayFeedback(state: DesktopViewState): void {
  const feedback = providerOverlayFeedback(state.provider_base_url, state.provider_status);
  const input = document.querySelector<HTMLInputElement>("#provider-url");
  input?.setAttribute("aria-invalid", String(!feedback.baseUrl.ok));

  const status = document.querySelector<HTMLElement>("#provider-status");
  if (!status) return;
  status.className = `provider-status ${feedback.status.kind === "success" ? "ok" : feedback.status.kind}`;
  const title = status.querySelector<HTMLElement>("[data-provider-status-title]");
  const hint = status.querySelector<HTMLElement>("[data-provider-status-hint]");
  if (title) title.textContent = feedback.status.title;
  if (hint) hint.textContent = feedback.status.hint;
  const details = status.querySelector<HTMLDetailsElement>("[data-details-key='provider-status-details']");
  const detailsText = status.querySelector<HTMLElement>("[data-provider-status-details]");
  if (detailsText) detailsText.textContent = feedback.status.details;
  if (details) details.hidden = feedback.status.details.trim().length === 0;
}

function updateReviewActionButtons(context: ActionContext): void {
  const enhanced = document.querySelector<HTMLButtonElement>('[data-action="send-review-enhanced"]');
  if (enhanced) synchronizeActionButtonAvailability(enhanced, context);
  const raw = document.querySelector<HTMLButtonElement>('[data-action="send-review-raw"]');
  if (raw) synchronizeActionButtonAvailability(raw, context);
}

function synchronizeActionButtonAvailability(
  button: HTMLButtonElement,
  context: ActionContext,
): void {
  const action = button.dataset.action ?? "";
  const model = context.getRenderModel();
  const payload: ActionPayload = {
    index: Number(button.dataset.index ?? "-1"),
    value: actionPayloadValue(button),
  };
  button.disabled = !model || !actionEnabledById(action, model, payload);
  button.setAttribute("aria-disabled", String(button.disabled));
}

export function prepareConfigMutation(
  context: ActionContext,
  target: ConfigMutationTarget,
): ConfigValueInput[] | null {
  reconcileConfigDraftTarget(context.uiState, target);
  const currentState = context.getViewState();
  if (!currentState) return null;
  const values = prepareConfigSnapshot(context, target);
  if (!values || !validateConfigValues(values, currentState.config_fields, true)) return null;
  return values;
}

export function prepareConfigSnapshot(
  context: ActionContext,
  target: ConfigMutationTarget,
): ConfigValueInput[] | null {
  reconcileConfigDraftTarget(context.uiState, target);
  const currentState = context.getViewState();
  if (!currentState) return null;
  return configMutationValues(context.uiState, target)
    ?? currentState.config_fields.map((field) => ({ key: field.key, text: field.value }));
}

function collectSettingsControls(): SettingsControl[] {
  return Array.from(document.querySelectorAll<SettingsControl>(".settings-control"));
}

function settingsControlValue(control: SettingsControl): string {
  if (control instanceof HTMLInputElement && control.type === "checkbox") {
    return control.checked ? "true" : "false";
  }
  return control.value;
}

function updateSettingsControlDraft(control: SettingsControl, context: ActionContext): boolean {
  if (!configDraftEditOpen(context.uiState)) return false;
  const currentState = context.getViewState();
  const text = settingsControlValue(control);
  const key = control.dataset.configKey ?? "";
  if (!currentState || !key) return false;
  const index = currentState.config_fields.findIndex((field) => field.key === key);
  if (index < 0) return false;
  recordSideChatCatalogConfigEdit(
    context.uiState,
    key,
    currentState.config_fields[index].value,
    text,
  );
  updateConfigDraftValue(
    context.uiState,
    currentState.config_target,
    currentState.config_fields.map((field) => ({ key: field.key, text: field.value })),
    key,
    text,
  );
  return true;
}

function synchronizeProviderModelControls(source: SettingsControl): void {
  const selector = source.matches("[data-main-provider-model-control]")
    ? "[data-main-provider-model-control]"
    : source.matches("[data-side-chat-model-control]")
      ? "[data-side-chat-model-control]"
      : null;
  if (!selector) return;
  const value = source.value;
  document.querySelectorAll<SettingsControl>(selector).forEach((control) => {
    if (control === source) return;
    if (control instanceof HTMLSelectElement) {
      control.querySelectorAll<HTMLOptionElement>("option[data-manual-option]").forEach((option) => option.remove());
      const existing = Array.from(control.options).some((option) => option.value === value);
      if (!existing && value.length > 0 && !source.matches("[data-main-provider-model-control]")) {
        const option = document.createElement("option");
        option.value = value;
        option.textContent = `${value}（手入力）`;
        option.dataset.manualOption = "true";
        control.prepend(option);
      }
      control.value = value;
      return;
    }
    control.value = value;
  });
}

function validateSettingsForm(
  context: ActionContext,
  fields: ConfigFieldProjection[],
  focusInvalid: boolean,
): boolean {
  const controls = collectSettingsControls();
  const validation = document.querySelector<HTMLElement>("#settings-validation");
  const values = controls.flatMap((control) => {
    const key = control.dataset.configKey ?? "";
    return key ? [{ key, text: settingsControlValue(control) }] : [];
  });
  const initialSetup = document.querySelector("[data-surface='initial-setup']") !== null;
  const currentValues = context.getViewState()?.config_fields.map((field) => ({
    key: field.key,
    text: field.value,
  })) ?? values;
  const visibleValidation = initialSetup
    ? validateInitialSetupStep(
      context.uiState.initialSetup.step,
      fields,
      currentValues,
    )
    : validateConfigFieldValues(fields, currentValues);
  for (const control of controls) {
    const key = control.dataset.configKey ?? "";
    if (!key) continue;
    const field = fields.find((candidate) => candidate.key === key);
    if (!field) continue;
    const value = control.matches("select[data-main-provider-model-control]")
      ? currentValues.find((entry) => entry.key === key)?.text ?? ""
      : settingsControlValue(control);
    const result = validateConfigInput(field, value, currentValues);
    if (result.ok) control.removeAttribute("aria-invalid");
    else control.setAttribute("aria-invalid", "true");
  }
  if (validation) {
    validation.textContent = visibleValidation.ok
      ? initialSetup
        ? "入力形式は問題ありません。"
        : context.uiState.configDirty
          ? "未保存の設定があります。Apply、保存、または変更を破棄するまで別画面からの設定変更は停止します。"
          : "入力形式は問題ありません。"
      : `${visibleValidation.invalidKey}: ${visibleValidation.message}`;
    validation.classList.toggle("ok", visibleValidation.ok);
    validation.classList.toggle("error", !visibleValidation.ok);
  }
  updateDirtyBadges(context, visibleValidation.ok);
  if (focusInvalid && !visibleValidation.ok) {
    controls.find((control) => control.dataset.configKey === visibleValidation.invalidKey)?.focus();
  }
  return visibleValidation.ok;
}

function validateConfigValues(
  values: ConfigValueInput[],
  fields: ConfigFieldProjection[],
  focusInvalid: boolean,
): boolean {
  const validation = validateConfigFieldValues(fields, values);
  if (!validation.ok && focusInvalid && validation.invalidKey) {
    document.querySelector<SettingsControl>(
      `[data-config-key="${CSS.escape(validation.invalidKey)}"]`,
    )?.focus();
  }
  return validation.ok;
}

function updateDirtyBadges(context: ActionContext, _validationOk: boolean): void {
  const uiState = context.uiState;
  document.querySelectorAll<HTMLElement>(".dirty-badge").forEach((node) => {
    node.classList.toggle("visible", uiState.configDirty);
  });
  document
    .querySelectorAll<HTMLButtonElement>(".settings-modal [data-action='discard-config-draft']")
    .forEach((button) => {
      button.hidden = !uiState.configDirty;
    });
  document
    .querySelectorAll<HTMLButtonElement>(".settings-modal button[data-action]")
    .forEach((button) => synchronizeActionButtonAvailability(button, context));
  document
    .querySelectorAll<HTMLButtonElement>(".initial-setup-shell button[data-action]")
    .forEach((button) => synchronizeActionButtonAvailability(button, context));
}

function sessionSettingsDraftField(value: string): SessionSettingsDraftField | null {
  if (value === "base-url") return "baseUrl";
  if (value === "model") return "model";
  if (value === "provider-profile") return "providerProfile";
  if (value === "api-key-env") return "apiKeyEnv";
  if (value === "context-window") return "contextWindow";
  if (value === "access-mode") return "accessMode";
  return null;
}

function isProviderProfile(value: string): value is ProviderProfile {
  return value === "lm_studio"
    || value === "openai_compatible"
    || value === "openai_responses"
    || value === "lm_studio_chat_completions";
}

function updateSessionSettingsControls(
  state: DesktopViewState,
  context: ActionContext,
): void {
  const local = context.uiState.sessionSettings;
  const validation = local.validation;
  const availability = sessionSettingsMutationAvailability(local, state.session_settings);
  document.querySelectorAll<SettingsControl>(".session-settings-control").forEach((control) => {
    const field = sessionSettingsDraftField(control.dataset.sessionSetting ?? "");
    if (field !== null && validation?.fields[field].ok === false) {
      control.setAttribute("aria-invalid", "true");
    } else {
      control.removeAttribute("aria-invalid");
    }
  });
  document.querySelectorAll<HTMLElement>(".session-settings-dirty").forEach((badge) => {
    badge.classList.toggle("visible", local.dirty);
  });
  const status = document.querySelector<HTMLElement>("#session-settings-status");
  if (status) {
    status.textContent = local.activeMutation
      ? "Session Settingsを適用しています…"
      : availability.reason;
    status.classList.toggle("ok", validation?.ok !== false && (availability.enabled || !local.dirty));
    status.classList.toggle("error", validation?.ok === false);
    status.classList.toggle(
      "warning",
      validation?.ok !== false && local.dirty && !availability.enabled,
    );
  }
  document
    .querySelectorAll<HTMLButtonElement>(".session-settings-modal button[data-action]")
    .forEach((button) => {
      if (button.dataset.action === "discard-session-settings") button.hidden = !local.dirty;
      if (button.dataset.action === "close-overlay") {
        button.setAttribute("aria-haspopup", local.dirty ? "alertdialog" : "false");
      }
      synchronizeActionButtonAvailability(button, context);
    });
}

export function shortcutActionForComposer(
  sample: KeyboardShortcutSample,
  sideChatComposerActive: boolean,
): string | null {
  const action = globalShortcutAction(sample);
  return action === "send" && sideChatComposerActive ? "send-side-chat" : action;
}

function updateSideChatActionButtons(_state: DesktopWebState, context: ActionContext): void {
  const send = document.querySelector<HTMLButtonElement>('[data-action="send-side-chat"]');
  if (send) synchronizeActionButtonAvailability(send, context);
  const cancel = document.querySelector<HTMLButtonElement>('[data-action="cancel-side-chat"]');
  if (cancel) synchronizeActionButtonAvailability(cancel, context);
  const remove = document.querySelector<HTMLButtonElement>('[data-action="request-delete-side-chat"]');
  if (remove) synchronizeActionButtonAvailability(remove, context);
  const view = context.getViewState();
  if (view?.overlay === "config") {
    synchronizeSideChatCatalogControls(view, context, configDraftEditOpen(context.uiState));
  }
}

function synchronizeSideChatCatalogControls(
  state: DesktopViewState,
  context: ActionContext,
  editingOpen: boolean,
): void {
  const catalog = sideChatCatalogViewForState(context.uiState, state);
  const draftApplies = configDraftAppliesTo(context.uiState, state.config_target);
  const activeFields = state.config_fields.map((field) => ({
    ...field,
    value: draftApplies
      ? (context.uiState.configDraftValues.get(field.key) ?? field.value)
      : field.value,
  }));
  const model = activeFields.find((field) => field.key === "side_chat.model")?.value ?? "";
  const baseUrlField = activeFields.find((field) => field.key === "side_chat.base_url");
  const baseUrlValidation = baseUrlField
    ? validateConfigInput(baseUrlField, baseUrlField.value, activeFields.map((field) => ({
      key: field.key,
      text: field.value,
    })))
    : { ok: false, message: "Side ChatのLLM URL設定が見つかりません。" };
  const options = sideChatModelOptions(catalog, model);
  const select = document.querySelector<HTMLSelectElement>("#side-chat-model");
  if (select) {
    const desired = [
      ...(model.trim() ? [] : [{ value: "", label: "モデルを選択してください", disabled: true }]),
      ...options.map((option) => ({
        value: option.id,
        label: sideChatModelOptionLabel(option),
        disabled: false,
      })),
    ];
    const currentSignature = Array.from(select.options)
      .map((option) => `${option.value}\u0000${option.textContent ?? ""}`)
      .join("\u0001");
    const desiredSignature = desired
      .map((option) => `${option.value}\u0000${option.label}`)
      .join("\u0001");
    if (currentSignature !== desiredSignature) {
      select.replaceChildren(...desired.map((candidate) => {
        const option = document.createElement("option");
        option.value = candidate.value;
        option.textContent = candidate.label;
        option.disabled = candidate.disabled;
        return option;
      }));
    }
    select.value = model.trim();
    select.disabled = !editingOpen || options.length === 0;
    select.setAttribute("aria-disabled", String(select.disabled));
  }
  const manual = document.querySelector<HTMLInputElement>("#side-chat-model-manual");
  if (manual && manual !== document.activeElement) manual.value = model;

  const load = document.querySelector<HTMLButtonElement>('[data-action="load-side-chat-models"]');
  if (load) {
    synchronizeActionButtonAvailability(load, context);
    load.textContent = catalog.status === "loading" ? "読込中…" : "モデル読込";
  }
  const status = document.querySelector<HTMLElement>("#side-chat-model-catalog-status");
  if (status) {
    status.textContent = !baseUrlValidation.ok
      ? baseUrlValidation.message
      : sideChatCatalogStatusText(catalog);
    status.classList.toggle("error", catalog.status === "error" || !baseUrlValidation.ok);
  }
  document.querySelector<HTMLElement>("#settings-side-chat")
    ?.setAttribute("aria-busy", String(catalog.status === "loading"));
}

function scheduleOpacityPreview(percent: number, context: ActionContext): void {
  percent = clampOpacityPercent(percent);
  pendingOpacityPreviewPercent = percent;
  if (opacityPreviewFrame !== null) return;
  opacityPreviewFrame = window.requestAnimationFrame(() => {
    opacityPreviewFrame = null;
    void flushOpacityPreview(context);
  });
}

function clampOpacityPercent(percent: number): number {
  if (!Number.isFinite(percent)) return MAX_WINDOW_OPACITY_PERCENT;
  return Math.min(MAX_WINDOW_OPACITY_PERCENT, Math.max(MIN_WINDOW_OPACITY_PERCENT, Math.round(percent)));
}

async function flushOpacityPreview(context: ActionContext): Promise<void> {
  if (opacityPreviewInFlight || pendingOpacityPreviewPercent === null) return;
  const percent = pendingOpacityPreviewPercent;
  pendingOpacityPreviewPercent = null;
  opacityPreviewInFlight = true;
  try {
    await command<void>("preview_window_opacity", { percent });
  } catch (error) {
    context.reportError(error);
  } finally {
    opacityPreviewInFlight = false;
    if (pendingOpacityPreviewPercent !== null) void flushOpacityPreview(context);
  }
}

export function focusOverlayPrimary(
  state: DesktopViewState,
  uiState: UiLocalState,
): PostRenderFocusIntent | null {
  // The initiating modal/menu may be rerendered while a new-session command is pending. Its
  // detached trigger deliberately leaves focus unclaimed for the typed composer continuation.
  if (uiState.activeNewSessionMutation !== null) return null;
  const sideChatDeleteTarget = sideChatDeleteConfirmationStillTargets(
    uiState.sideChatDeleteConfirmation,
    state,
  ) ? uiState.sideChatDeleteConfirmation : null;
  const activeLocalModalIdentity = localModalIdentity(
    uiState.pendingLocalConfirmation !== null,
    sideChatDeleteTarget,
  );
  const overlayKey = state.confirmation_visible
    ? modalIdentity(state)
    : activeLocalModalIdentity
      ? activeLocalModalIdentity
      : state.overlay;
  const focusOwnerKey = overlayKey === "config" || overlayKey === "session_settings"
    ? `${overlayKey}:${settingsSurfaceIdentity(state) ?? "unavailable"}`
    : overlayKey;
  const confirmationOverlay = overlayKey.startsWith("permission:") || activeLocalModalIdentity !== null;
  const confirmationPending = overlayKey.startsWith("permission:")
    ? uiState.permissionDecision?.phase === "submitting"
    : overlayKey === "local-confirm"
      ? uiState.localConfirmationDecisionPending
      : sideChatDeleteTarget !== null
        && sideChatMutationPending(uiState, sideChatDeleteTarget.ownerSessionId);
  const active = document.activeElement;
  const activeModal = active instanceof Element
    ? active.closest<HTMLElement>(
      ".modal[role='dialog'], .modal[role='alertdialog'], .modal[data-modal], .titlebar-popover[data-modal], .initial-setup-shell",
    )
    : null;
  const hasMeaningfulActiveElement = Boolean(
    active instanceof HTMLElement &&
    active !== document.body &&
    active !== document.documentElement &&
    (overlayKey === "none" || (activeModal !== null && activeModal.closest("[inert], [aria-hidden='true']") === null)) &&
    (!confirmationOverlay || confirmationFocusIsMeaningful(
      confirmationPending,
      active.matches(".permission-decision-status"),
    ))
  );
  if (hasMeaningfulActiveElement) {
    uiState.lastFocusedOverlay = focusOwnerKey;
    return null;
  }
  if (!overlayPrimaryFocusRequired(
    overlayKey,
    focusOwnerKey,
    uiState.lastFocusedOverlay,
    confirmationOverlay,
    hasMeaningfulActiveElement,
  )) {
    return null;
  }
  const scheduledFocusOwner = focusOwnerKey;
  const selectors =
    titlebarMenuFromOverlay(overlayKey)
      ? [".titlebar-popover button[data-titlebar-menu-action]:not(:disabled):not([aria-disabled='true'])"]
      : confirmationOverlay
        ? []
        : overlayPrimaryFocusSelectors(overlayKey);
  if (selectors.length === 0 && !confirmationOverlay) {
    return null;
  }
  const focusSelectors = confirmationOverlay
    ? confirmationFocusSelectors(confirmationPending)
    : selectors;
  const titlebarPopup = titlebarMenuFromOverlay(overlayKey) !== null;
  return {
    source: titlebarPopup ? "titlebar-menu" : "modal-primary",
    // Menu entry is a fallback after rerender: an exact action/range snapshot must keep
    // ownership. Actual modal entry retains priority over every background continuation.
    priority: titlebarPopup ? "fallback" : "modal-containment",
    claim: { kind: "force" },
    candidates: focusSelectors.map((selector) => ({
      resolve: () => document.querySelector<HTMLElement>(selector),
      settle: (target) => {
        uiState.lastFocusedOverlay = scheduledFocusOwner;
        if (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement) {
          const end = target.value.length;
          target.setSelectionRange(end, end);
        }
      },
    })),
    isCurrent: () => uiState.activeNewSessionMutation === null,
  };
}

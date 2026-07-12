import { command } from "./api";
import { dispatchRegisteredAction, type ActionContext } from "./actions";
import {
  beginConfigMutation,
  configMutationValues,
  finishConfigMutation,
  reconcileConfigDraftTarget,
  type ConfigValueInput,
  updateConfigDraftValue,
} from "./config_mutation";
import { beginLocalDecision, failLocalDecision, finishLocalDecision } from "./decision_state";
import { isRegularModalOverlay, modalIsOpen, nextDialogFocusIndex } from "./modal_state";
import { globalShortcutAction } from "./keyboard_shortcut";
import { navigationIsIdle } from "./navigation_state";
import { rowMutationArgs } from "./row_target";
import { TitlebarDragGesture, windowControlKeyboardActivation } from "./titlebar_interaction";
import type { ConfigFieldProjection, ConfigMutationTarget, DesktopWebState, RowMutationTarget } from "./types";
import type { UiLocalState } from "./ui_state";
import { goalSlashCommandHint, validateConfigInput } from "./utils";
import {
  deriveUiCapabilities,
  configDraftEditOpen,
  configDraftCommitOpen,
  configDraftDiscardOpen,
  configOwnerMutationOpen,
  draftMutationTarget,
  localSearchOwner,
  sessionSearchMutationTarget,
  sessionSearchOwner,
} from "./view_state";

let pendingOpacityPreviewPercent: number | null = null;
let opacityPreviewFrame: number | null = null;
let opacityPreviewInFlight = false;
let delegatedEventsInstalled = false;
const TEXT_MUTATION_DEBOUNCE_MS = 180;
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

const pendingTextMutations = new Map<string, PendingTextMutation>();
let nextTextMutationGeneration = 1;
const titlebarDragGesture = new TitlebarDragGesture();

export function installGlobalKeyboardShortcuts(context: ActionContext): void {
  document.addEventListener("keydown", (event) => {
    if (event.isComposing || event.keyCode === 229) return;
    const currentState = context.getViewState();
    if (
      currentState &&
      modalIsOpen(currentState, context.uiState.pendingLocalConfirmation !== null)
    ) {
      if (event.key === "Tab") {
        trapDialogFocus(event);
      } else if (event.key === "Escape" && currentState.confirmation_visible) {
        event.preventDefault();
        if (event.repeat) return;
        void dispatchRegisteredAction("deny", currentState, context, { index: -1, value: "" });
      } else if (event.key === "Escape" && context.uiState.pendingLocalConfirmation) {
        event.preventDefault();
        if (!context.uiState.localConfirmationDecisionPending) {
          context.uiState.pendingLocalConfirmation = null;
          context.uiState.localConfirmationDecisionError = "";
          context.rerender();
        }
      } else if (event.key === "Escape" && isRegularModalOverlay(currentState.overlay)) {
        event.preventDefault();
        if (!startupSetupRequired(currentState)) void context.mutate("close_overlay");
      } else if (event.ctrlKey || event.metaKey || event.altKey || /^F\d+$/.test(event.key)) {
        event.preventDefault();
      }
      return;
    }
    const shortcutAction = globalShortcutAction(event);
    if (shortcutAction && currentState) {
      event.preventDefault();
      void dispatchRegisteredAction(shortcutAction, currentState, context, { index: -1, value: "" });
    }
    if (event.key === "Escape" && currentState && currentState.overlay !== "none") {
      event.preventDefault();
      if (startupSetupRequired(currentState)) return;
      void context.mutate("close_overlay");
    }
  });
}

export function wireEvents(state: DesktopWebState, context: ActionContext): void {
  installDelegatedActionEvents(context);
  const prompt = document.querySelector<HTMLTextAreaElement>("#prompt");
  if (prompt) {
    resizePromptComposer(prompt);
  }
  prompt?.addEventListener("input", (event) => {
    const prompt = event.currentTarget as HTMLTextAreaElement;
    const text = prompt.value;
    resizePromptComposer(prompt);
    context.uiState.drafts.prompt = text;
    context.uiState.drafts.composerRevision += 1;
    const projection = context.getProjection();
    if (projection) {
      const capabilities = deriveUiCapabilities(projection, context.uiState);
      updateGoalCommandHint(text);
      const send = document.querySelector<HTMLButtonElement>('[data-action="send"]');
      if (send) send.disabled = !capabilities.canSubmit;
      const enhance = document.querySelector<HTMLButtonElement>('[data-action="enhance-prompt"]');
      if (enhance) {
        enhance.disabled = !capabilities.canEnhance;
        const title = projection.navigation_loading
          ? "画面の切り替え完了後にEnhanceできます"
          : projection.agent_tree_active
            ? "Sub Agentの完了または停止後にEnhanceできます"
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
  document.querySelector<HTMLInputElement>("#image-input")?.addEventListener("input", (event) => {
    context.uiState.drafts.imageInput = (event.currentTarget as HTMLInputElement).value;
    context.uiState.drafts.imageRevision += 1;
  });
  document.querySelector<HTMLInputElement>("#provider-url")?.addEventListener("input", (event) => {
    context.uiState.drafts.provider.baseUrl = (event.currentTarget as HTMLInputElement).value;
    context.uiState.drafts.providerRevision += 1;
    updateProviderActionButtons(context);
  });
  document.querySelector<HTMLInputElement>("#provider-context-window")?.addEventListener("input", (event) => {
    context.uiState.drafts.provider.contextWindow = (event.currentTarget as HTMLInputElement).value;
    context.uiState.drafts.providerRevision += 1;
    updateProviderActionButtons(context);
  });
  document.querySelector<HTMLInputElement>("#provider-max-output-tokens")?.addEventListener("input", (event) => {
    context.uiState.drafts.provider.maxOutputTokens = (event.currentTarget as HTMLInputElement).value;
    context.uiState.drafts.providerRevision += 1;
    updateProviderActionButtons(context);
  });
  const settingsControls = collectSettingsControls();
  settingsControls.forEach((control) => {
    const update = () => {
      if (!updateSettingsControlDraft(control, context)) return;
      updateDirtyBadges(context.uiState);
      validateSettingsForm(context.uiState, state.config_fields, false);
    };
    control.addEventListener("input", update);
    control.addEventListener("change", update);
  });
  if (settingsControls.length > 0) {
    validateSettingsForm(context.uiState, state.config_fields, false);
  }
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
  opacityInput?.addEventListener("input", (event) => {
    scheduleOpacityPreview(Number((event.currentTarget as HTMLInputElement).value), context);
  });
  opacityInput?.addEventListener("change", (event) => {
    void context.mutate("set_window_opacity", {
      percent: clampOpacityPercent(Number((event.currentTarget as HTMLInputElement).value)),
    });
  });
  focusOverlayPrimary(state, context.uiState);
}

function installDelegatedActionEvents(context: ActionContext): void {
  if (delegatedEventsInstalled) return;
  delegatedEventsInstalled = true;
  document.addEventListener("click", (event) => {
    const target = event.target;
    if (!(target instanceof Element)) return;
    const node = target.closest<HTMLElement>("[data-action]");
    if (!node || (node instanceof HTMLButtonElement && node.disabled)) return;
    if (
      target.closest("[data-modal]") &&
      (node.classList.contains("modal-backdrop") || node.classList.contains("menu-scrim"))
    ) {
      return;
    }
    const currentState = context.getViewState();
    if (!currentState) return;
    const action = node.dataset.action ?? "";
    const index = Number(node.dataset.index ?? "-1");
    const value = node.dataset.mode ?? "";
    void dispatchAction(action, index, value, currentState, context).catch((error) => context.reportError(String(error)));
  });
  document.addEventListener("pointerdown", (event) => {
    const target = event.target;
    if (!(target instanceof Element)) return;
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
  document.addEventListener("pointercancel", () => titlebarDragGesture.cancel());
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
      void dispatchRegisteredAction("toggle-maximize-window", currentState, context, { index: -1, value: "" }).catch((error) =>
        context.reportError(String(error)),
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
      void dispatchRegisteredAction(action, currentState, context, { index: -1, value: "" }).catch((error) =>
        context.reportError(String(error)),
      );
    }
  });
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
  const dialog = document.querySelector<HTMLElement>(".modal[role='dialog'], .modal[role='alertdialog']");
  if (!dialog) return;
  const focusable = Array.from(
    dialog.querySelectorAll<HTMLElement>(
      "button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), a[href], [tabindex]:not([tabindex='-1'])",
    ),
  ).filter((element) => !element.hidden && element.getAttribute("aria-hidden") !== "true");
  event.preventDefault();
  if (focusable.length === 0) {
    (dialog.querySelector<HTMLElement>(".permission-decision-status") ?? dialog).focus();
    return;
  }
  const currentIndex = focusable.indexOf(document.activeElement as HTMLElement);
  const nextIndex = nextDialogFocusIndex(currentIndex, focusable.length, event.shiftKey);
  focusable[nextIndex]?.focus();
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
  if (entry.timer !== null) {
    window.clearTimeout(entry.timer);
    entry.timer = null;
  }
  if (entry.inFlight) {
    await entry.inFlight;
  }
  if (!entry.args) {
    return;
  }
  const name = entry.name;
  const args = entry.args;
  const renderResult = entry.renderResult;
  const target = entry.target;
  const generation = entry.generation;
  entry.args = null;
  entry.inFlight = command<DesktopWebState>(name, args)
    .then((state) => {
      if (entry.args === null && entry.generation === generation && searchTargetStillMatches(key, target, context)) {
        context.acceptProjection(state, renderResult);
      }
    })
    .catch((error) => {
      if (!context.recoverCommandConflict(error)) context.reportError(String(error));
    })
    .finally(() => {
      entry.inFlight = null;
    });
  await entry.inFlight;
  if (entry.args !== null) {
    await flushTextMutation(key, context);
  } else {
    pendingTextMutations.delete(key);
  }
}

function searchTargetStillMatches(key: string, target: string, context: ActionContext): boolean {
  const state = context.getProjection();
  if (!state) return false;
  return key === "session-search"
    ? sessionSearchOwner(state) === target
    : localSearchOwner(state) === target;
}

function updateProviderActionButtons(context: ActionContext): void {
  const projection = context.getProjection();
  if (!projection) return;
  const capabilities = deriveUiCapabilities(projection, context.uiState);
  const load = document.querySelector<HTMLButtonElement>('[data-action="load-provider-models"]');
  if (load) load.disabled = !capabilities.canLoadProviderModels;
  document
    .querySelectorAll<HTMLButtonElement>('[data-action="apply-provider-session"], [data-action="save-provider-global"]')
    .forEach((button) => {
      button.disabled = !capabilities.canApplyProvider;
    });
}

function updateReviewActionButtons(context: ActionContext): void {
  const projection = context.getProjection();
  if (!projection) return;
  const capabilities = deriveUiCapabilities(projection, context.uiState);
  const enhanced = document.querySelector<HTMLButtonElement>('[data-action="send-review-enhanced"]');
  if (enhanced) enhanced.disabled = !capabilities.canSendEnhancedReview;
  const raw = document.querySelector<HTMLButtonElement>('[data-action="send-review-raw"]');
  if (raw) raw.disabled = !capabilities.canSendRawReview;
}

export function prepareConfigMutation(
  context: ActionContext,
  target: ConfigMutationTarget,
): ConfigValueInput[] | null {
  reconcileConfigDraftTarget(context.uiState, target);
  const currentState = context.getViewState();
  if (!currentState) return null;
  const values = configMutationValues(context.uiState, target)
    ?? (currentState.overlay === "config"
      ? currentState.config_fields.map((field) => ({ key: field.key, text: field.value }))
      : null);
  if (!values || !validateConfigValues(values, currentState.config_fields, true)) return null;
  return values;
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
  updateConfigDraftValue(
    context.uiState,
    currentState.config_target,
    currentState.config_fields.map((field) => ({ key: field.key, text: field.value })),
    key,
    text,
  );
  return true;
}

function validateSettingsForm(
  uiState: UiLocalState,
  fields: ConfigFieldProjection[],
  focusInvalid: boolean,
): boolean {
  const controls = collectSettingsControls();
  const validation = document.querySelector<HTMLElement>("#settings-validation");
  for (const control of controls) {
    const key = control.dataset.configKey ?? "";
    if (!key) continue;
    const field = fields.find((candidate) => candidate.key === key);
    if (!field) continue;
    const result = validateConfigInput(field, settingsControlValue(control));
    if (!result.ok) {
      if (validation) {
        validation.textContent = `${key}: ${result.message}`;
        validation.classList.toggle("ok", false);
        validation.classList.toggle("error", true);
      }
      updateDirtyBadges(uiState);
      if (focusInvalid) control.focus();
      return false;
    }
  }
  if (validation && controls.length > 0) {
    validation.textContent = uiState.configDirty
      ? "未保存の設定があります。Apply、保存、または変更を破棄するまで別画面からの設定変更は停止します。"
      : "入力形式は問題ありません。";
    validation.classList.toggle("ok", true);
    validation.classList.toggle("error", false);
  }
  updateDirtyBadges(uiState);
  return true;
}

function validateConfigValues(
  values: ConfigValueInput[],
  fields: ConfigFieldProjection[],
  focusInvalid: boolean,
): boolean {
  for (const value of values) {
    const field = fields.find((candidate) => candidate.key === value.key);
    if (!field) return false;
    const result = validateConfigInput(field, value.text);
    if (result.ok) continue;
    if (focusInvalid) {
      document.querySelector<SettingsControl>(
        `[data-config-key="${CSS.escape(value.key)}"]`,
      )?.focus();
    }
    return false;
  }
  return true;
}

function updateDirtyBadges(uiState: UiLocalState): void {
  document.querySelectorAll<HTMLElement>(".dirty-badge").forEach((node) => {
    node.classList.toggle("visible", uiState.configDirty);
  });
  document
    .querySelectorAll<HTMLButtonElement>(".settings-modal [data-action='discard-config-draft']")
    .forEach((button) => {
      button.hidden = !uiState.configDirty;
      button.disabled = !configDraftDiscardOpen(uiState);
      button.setAttribute("aria-disabled", String(button.disabled));
    });
  const setupRequired = document.querySelector(".settings-modal.setup-modal") !== null;
  const commitEnabled = configDraftCommitOpen(uiState, setupRequired);
  document
    .querySelectorAll<HTMLButtonElement>(
      ".settings-modal [data-action='apply-session-config'], .settings-modal [data-action='save-global-config']",
    )
    .forEach((button) => {
      button.disabled = !commitEnabled;
      button.setAttribute("aria-disabled", String(!commitEnabled));
    });
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
    context.reportError(String(error));
  } finally {
    opacityPreviewInFlight = false;
    if (pendingOpacityPreviewPercent !== null) void flushOpacityPreview(context);
  }
}

async function dispatchAction(action: string, index: number, value: string, state: DesktopWebState, context: ActionContext): Promise<void> {
  if (await dispatchRegisteredAction(action, state, context, { index, value })) return;
  switch (action) {
    case "toggle-attachment-tray":
      context.uiState.attachmentTrayOpen = !context.uiState.attachmentTrayOpen;
      context.rerender();
      return;
    case "dismiss-ui-error":
      context.uiState.recoverableError = null;
      context.rerender();
      return;
    case "new-project-session":
      if (!navigationIsIdle(state)) return;
      await runIndexedMutation("new_project_session", index, state.project_rows[index]?.project_id, state, context);
      return;
    case "project":
      if (!navigationIsIdle(state)) return;
      await runIndexedMutation("select_project", index, state.project_rows[index]?.project_id, state, context);
      return;
    case "session":
      if (!navigationIsIdle(state)) return;
      await runIndexedMutation("select_session", index, state.session_rows[index]?.session_id, state, context);
      return;
    case "chat-session":
      if (!navigationIsIdle(state)) return;
      await runIndexedMutation("select_chat_session", index, state.chat_session_rows[index]?.session_id, state, context);
      return;
    case "cancel-local-confirm":
      if (context.uiState.localConfirmationDecisionPending) return;
      context.uiState.pendingLocalConfirmation = null;
      finishLocalDecision(context.uiState);
      context.rerender();
      return;
    case "confirm-local-delete":
      await confirmLocalDelete(context);
      return;
    case "confirm-local-archive-state":
      await confirmLocalArchiveState(context);
      return;
    case "confirm-local-rollback":
      await confirmLocalRollback(context);
      return;
    case "artifact":
      await runIndexedMutation("select_artifact", index, state.artifact_rows[index]?.path, state, context);
      return;
    case "remove-image":
      await runIndexedMutation("remove_image", index, state.attached_images[index], state, context);
      return;
    case "send-review-enhanced":
      if (!state.send_enhanced_enabled) return;
      await context.mutate("send_prompt_review", {
        enhanced: true,
        text: state.review_draft_text,
        expectedTarget: draftMutationTarget(state),
      });
      return;
    case "send-review-raw":
      if (!state.send_raw_enabled) return;
      await context.mutate("send_prompt_review", {
        enhanced: false,
        text: state.review_draft_text,
        expectedTarget: draftMutationTarget(state),
      });
      return;
    case "cancel-review":
      await context.mutate("cancel_prompt_review");
      return;
    case "show-file-menu":
      await context.mutate("show_file_menu");
      return;
    case "show-edit-menu":
      await context.mutate("show_edit_menu");
      return;
    case "show-view-menu":
      await context.mutate("show_view_menu");
      return;
    case "show-help-menu":
      await context.mutate("show_help_menu");
      return;
    case "close-overlay":
      if (startupSetupRequired(state)) return;
      await context.mutate("close_overlay");
      return;
    case "import-config-toml":
      {
        const ownerMutationOpen = configOwnerMutationOpen(context.uiState);
        if (!ownerMutationOpen) return;
        context.uiState.externalConfigMutationPending = true;
        try {
          context.rerender();
          const request = beginConfigMutation(context.uiState, state.config_target);
          let nextState: DesktopWebState;
          let imported: boolean;
          try {
            [nextState, imported] = await command<[DesktopWebState, boolean]>("import_global_config_toml", {
              expectedTarget: request.target,
              configOwnerMutationOpen: ownerMutationOpen,
            });
          } catch (error) {
            const finished = finishConfigMutation(context.uiState, request, false, context.getViewState()?.config_target ?? null);
            if (context.recoverCommandConflict(error)) return;
            if (!finished) return;
            context.rerender();
            context.reportError(String(error));
            return;
          }
          if (!finishConfigMutation(context.uiState, request, imported, context.getViewState()?.config_target ?? null)) return;
          context.acceptProjection(nextState);
        } finally {
          context.uiState.externalConfigMutationPending = false;
          context.rerender();
        }
      }
      return;
    case "insert-command":
      await runIndexedMutation("insert_command", index, state.command_rows[index]?.path, state, context);
      return;
    default:
      return;
  }
}

function startupSetupRequired(state: DesktopWebState): boolean {
  return state.startup.initial_setup_required && state.startup.action_overlay === state.overlay;
}

async function confirmLocalDelete(context: ActionContext): Promise<void> {
  const pending = context.uiState.pendingLocalConfirmation;
  if (!pending || context.uiState.localConfirmationDecisionPending) {
    return;
  }
  if (pending.kind === "project") {
    await runLocalConfirmationMutation(context, "delete_project", pending.index, pending.expectedTarget);
  } else if (pending.kind === "chat_session") {
    await runLocalConfirmationMutation(context, "delete_chat_session", pending.index, pending.expectedTarget);
  } else {
    await runLocalConfirmationMutation(context, "delete_session", pending.index, pending.expectedTarget);
  }
}

async function confirmLocalArchiveState(context: ActionContext): Promise<void> {
  const pending = context.uiState.pendingLocalConfirmation;
  if (
    !pending ||
    context.uiState.localConfirmationDecisionPending ||
    (pending.kind !== "archive_session" && pending.kind !== "unarchive_session")
  ) {
    return;
  }
  await runLocalConfirmationMutation(
    context,
    pending.kind === "archive_session" ? "archive_session" : "unarchive_session",
    pending.index,
    pending.expectedTarget,
  );
}

async function confirmLocalRollback(context: ActionContext): Promise<void> {
  const pending = context.uiState.pendingLocalConfirmation;
  if (!pending || context.uiState.localConfirmationDecisionPending || pending.kind !== "rollback_session") {
    return;
  }
  await runLocalConfirmationMutation(context, "rollback_session", pending.index, pending.expectedTarget);
}

async function runLocalConfirmationMutation(
  context: ActionContext,
  name: string,
  index: number,
  expectedTarget: RowMutationTarget,
): Promise<void> {
  if (!beginLocalDecision(context.uiState, context.uiState.pendingLocalConfirmation !== null)) return;
  context.rerender();
  try {
    const nextState = await command<DesktopWebState>(name, { index, expectedTarget });
    finishLocalDecision(context.uiState);
    context.uiState.pendingLocalConfirmation = null;
    context.acceptProjection(nextState);
  } catch (error) {
    failLocalDecision(context.uiState, "処理を開始できませんでした。もう一度お試しください。");
    if (context.recoverCommandConflict(error)) {
      context.rerender();
      return;
    }
    context.rerender();
    context.reportError(String(error));
  }
}

async function runIndexedMutation(
  name: string,
  index: number,
  rowId: string | null | undefined,
  state: DesktopWebState,
  context: ActionContext,
): Promise<void> {
  const args = rowMutationArgs(state, index, rowId);
  if (args) await context.mutate(name, args);
}

function focusOverlayPrimary(state: DesktopWebState, uiState: UiLocalState): void {
  const overlayKey = state.confirmation_visible ? "permission" : uiState.pendingLocalConfirmation ? "local-confirm" : state.overlay;
  const active = document.activeElement;
  const activeModal = document.querySelector<HTMLElement>(".modal[role='dialog'], .modal[role='alertdialog'], .modal[data-modal]");
  if (
    active instanceof HTMLElement &&
    active !== document.body &&
    active !== document.documentElement &&
    (overlayKey === "none" || activeModal?.contains(active))
  ) {
    uiState.lastFocusedOverlay = overlayKey;
    return;
  }
  if (overlayKey === uiState.lastFocusedOverlay) {
    if (active && active !== document.body && active !== document.documentElement) return;
  }
  uiState.lastFocusedOverlay = overlayKey;
  const confirmationOverlay = overlayKey === "permission" || overlayKey === "local-confirm";
  const selector =
    overlayKey === "command_palette"
      ? "#local-search"
      : overlayKey === "provider"
        ? "#provider-url"
        : overlayKey === "config"
          ? ".settings-control"
          : overlayKey === "workspace"
            ? "#workspace-input"
            : overlayKey === "prompt_review"
              ? "#review-draft"
              : confirmationOverlay
                ? ""
                : isRegularModalOverlay(overlayKey)
                  ? ".modal button:not(:disabled), .modal[role='dialog']"
                  : "";
  if (!selector && !confirmationOverlay) {
    return;
  }
  requestAnimationFrame(() => {
    const target = confirmationOverlay
      ? document.querySelector<HTMLElement>(".modal-actions button[autofocus]:not(:disabled)")
        ?? document.querySelector<HTMLElement>(".modal-actions button:not(:disabled)")
        ?? document.querySelector<HTMLElement>(".permission-decision-status")
      : document.querySelector<HTMLElement>(selector);
    target?.focus();
    if (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement) {
      const end = target.value.length;
      target.setSelectionRange(end, end);
    }
  });
}

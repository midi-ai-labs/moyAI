import { command } from "./api";
import { dispatchRegisteredAction, type ActionContext } from "./actions";
import {
  beginConfigMutation,
  configMutationPending,
  configMutationValues,
  finishConfigMutation,
  reconcileConfigDraftTarget,
  type ConfigValueInput,
  updateConfigDraftValue,
} from "./config_mutation";
import { beginLocalDecision, failLocalDecision, finishLocalDecision } from "./decision_state";
import { isRegularModalOverlay, modalIsOpen, nextDialogFocusIndex } from "./modal_state";
import { configCommitEnabled, navigationIsIdle } from "./navigation_state";
import { rowMutationArgs } from "./row_target";
import { TitlebarDragGesture, windowControlKeyboardActivation } from "./titlebar_interaction";
import type { ConfigMutationTarget, DesktopWebState, RowMutationTarget } from "./types";
import type { UiLocalState } from "./ui_state";
import { goalSlashCommandHint, validateConfigInput } from "./utils";

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
}

const pendingTextMutations = new Map<string, PendingTextMutation>();
const titlebarDragGesture = new TitlebarDragGesture();

export function textMutationPending(key: string): boolean {
  const entry = pendingTextMutations.get(key);
  return Boolean(entry && (entry.timer !== null || entry.inFlight !== null || entry.args !== null));
}

export function installGlobalKeyboardShortcuts(context: ActionContext): void {
  document.addEventListener("keydown", (event) => {
    if (event.isComposing || event.keyCode === 229) return;
    const currentState = context.getCurrentState();
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
          context.render(currentState);
        }
      } else if (event.key === "Escape" && isRegularModalOverlay(currentState.overlay)) {
        event.preventDefault();
        if (!startupSetupRequired(currentState)) void context.mutate("close_overlay");
      } else if (event.ctrlKey || event.metaKey || event.altKey || /^F\d+$/.test(event.key)) {
        event.preventDefault();
      }
      return;
    }
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
      event.preventDefault();
      if (currentState) void dispatchRegisteredAction("show-command-palette", currentState, context, { index: -1, value: "" });
    }
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "n") {
      event.preventDefault();
      if (currentState) void dispatchRegisteredAction("new-chat", currentState, context, { index: -1, value: "" });
    }
    if ((event.ctrlKey || event.metaKey) && event.key === "Enter" && currentState?.can_submit) {
      event.preventDefault();
      void dispatchRegisteredAction("send", currentState, context, { index: -1, value: "" });
    }
    if (event.key === "F8" && currentState) {
      event.preventDefault();
      void dispatchRegisteredAction("toggle-access", currentState, context, { index: -1, value: "" });
    }
    if (event.key === "F9" && currentState) {
      event.preventDefault();
      void dispatchRegisteredAction("export-transcript", currentState, context, { index: -1, value: "" });
    }
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "i" && currentState) {
      event.preventDefault();
      void dispatchRegisteredAction("toggle-session-archived-search", currentState, context, { index: -1, value: "" });
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
    const currentState = context.getCurrentState();
    if (currentState) {
      currentState.draft_prompt = text;
      currentState.can_submit = text.trim().length > 0 && !currentState.busy && !currentState.navigation_loading;
      currentState.enhance_enabled = text.trim().length > 0 && !currentState.busy && !currentState.navigation_loading;
      updateGoalCommandHint(text);
      const send = document.querySelector<HTMLButtonElement>('[data-action="send"]');
      if (send) send.disabled = !currentState.can_submit;
      const enhance = document.querySelector<HTMLButtonElement>('[data-action="enhance-prompt"]');
      if (enhance) {
        enhance.disabled = !currentState.enhance_enabled;
        const title = currentState.navigation_loading
          ? "画面の切り替え完了後にEnhanceできます"
          : currentState.busy
            ? "実行中はEnhanceできません"
          : text.trim().length === 0
            ? "依頼文を入力してください"
            : "Enhance";
        enhance.title = title;
        enhance.setAttribute("aria-label", title);
      }
    }
    resizePromptComposer(prompt);
    void command<DesktopWebState>("set_prompt", { text })
      .then(context.setCurrentState)
      .catch((error) => context.renderError(String(error)));
  });
  document.querySelector<HTMLInputElement>("#image-input")?.addEventListener("input", (event) => {
    const text = (event.currentTarget as HTMLInputElement).value;
    const currentState = context.getCurrentState();
    if (currentState) currentState.image_input = text;
    void command<DesktopWebState>("set_image_input", { text })
      .then(context.setCurrentState)
      .catch((error) => context.renderError(String(error)));
  });
  document.querySelector<HTMLInputElement>("#provider-url")?.addEventListener("input", (event) => {
    const text = (event.currentTarget as HTMLInputElement).value;
    const currentState = context.getCurrentState();
    if (currentState) currentState.provider_base_url = text;
    scheduleTextMutation("provider-url", "set_provider_base_url", { text }, context);
  });
  document.querySelector<HTMLInputElement>("#provider-context-window")?.addEventListener("input", (event) => {
    const text = (event.currentTarget as HTMLInputElement).value;
    const currentState = context.getCurrentState();
    if (currentState) currentState.provider_context_window = text;
    scheduleTextMutation("provider-context-window", "set_provider_context_window", { text }, context);
  });
  document.querySelector<HTMLInputElement>("#provider-max-output-tokens")?.addEventListener("input", (event) => {
    const text = (event.currentTarget as HTMLInputElement).value;
    const currentState = context.getCurrentState();
    if (currentState) currentState.provider_max_output_tokens = text;
    scheduleTextMutation("provider-max-output-tokens", "set_provider_max_output_tokens", { text }, context);
  });
  const settingsControls = collectSettingsControls();
  settingsControls.forEach((control) => {
    const update = () => {
      if (!updateSettingsControlDraft(control, context)) return;
      updateDirtyBadges(context.uiState);
      validateSettingsForm(context.uiState, false);
    };
    control.addEventListener("input", update);
    control.addEventListener("change", update);
  });
  if (settingsControls.length > 0) {
    validateSettingsForm(context.uiState, false);
  }
  document.querySelector<HTMLInputElement>("#workspace-input")?.addEventListener("input", (event) => {
    void command<DesktopWebState>("set_workspace_input", {
      text: (event.currentTarget as HTMLInputElement).value,
    })
      .then(context.setCurrentState)
      .catch((error) => context.renderError(String(error)));
  });
  const localSearch = document.querySelector<HTMLInputElement>("#local-search");
  const updateLocalSearch = (input: HTMLInputElement, commit: boolean) => {
    const text = input.value;
    const currentState = context.getCurrentState();
    if (currentState) currentState.local_search_text = text;
    if (commit) scheduleTextMutation("local-search", "set_local_search", { text }, context, true);
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
    const currentState = context.getCurrentState();
    if (currentState) currentState.session_search_text = text;
    if (commit) scheduleTextMutation("session-search", "set_session_search", { text }, context, true);
  };
  sessionSearch?.addEventListener("input", (event) => {
    updateSessionSearch(event.currentTarget as HTMLInputElement, !(event as InputEvent).isComposing);
  });
  sessionSearch?.addEventListener("compositionend", (event) => {
    updateSessionSearch(event.currentTarget as HTMLInputElement, true);
  });
  document.querySelector<HTMLTextAreaElement>("#review-draft")?.addEventListener("input", (event) => {
    void command<DesktopWebState>("set_review_draft", {
      text: (event.currentTarget as HTMLTextAreaElement).value,
    })
      .then(context.setCurrentState)
      .catch((error) => context.renderError(String(error)));
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
    const currentState = context.getCurrentState();
    if (!currentState) return;
    const action = node.dataset.action ?? "";
    const index = Number(node.dataset.index ?? "-1");
    const value = node.dataset.mode ?? "";
    void dispatchAction(action, index, value, currentState, context).catch((error) => context.renderError(String(error)));
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
    const currentState = context.getCurrentState();
    if (currentState) {
      void dispatchRegisteredAction("toggle-maximize-window", currentState, context, { index: -1, value: "" }).catch((error) =>
        context.renderError(String(error)),
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
    const currentState = context.getCurrentState();
    const action = control.dataset.action ?? "";
    if (currentState && action) {
      void dispatchRegisteredAction(action, currentState, context, { index: -1, value: "" }).catch((error) =>
        context.renderError(String(error)),
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
  renderResult = false,
): void {
  const existing = pendingTextMutations.get(key);
  const entry = existing ?? { name, args: null, timer: null, inFlight: null, renderResult };
  entry.name = name;
  entry.args = args;
  entry.renderResult = renderResult;
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
  entry.args = null;
  entry.inFlight = command<DesktopWebState>(name, args)
    .then((state) => {
      if (entry.args === null) {
        context.setCurrentState(state);
        if (renderResult) context.render(state);
      }
    })
    .catch((error) => context.renderError(String(error)))
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

export async function flushProviderInputMutations(context: ActionContext): Promise<void> {
  await Promise.all([
    flushTextMutation("provider-url", context),
    flushTextMutation("provider-context-window", context),
    flushTextMutation("provider-max-output-tokens", context),
  ]);
}

export function prepareConfigMutation(
  context: ActionContext,
  target: ConfigMutationTarget,
): ConfigValueInput[] | null {
  reconcileConfigDraftTarget(context.uiState, target);
  const currentState = context.getCurrentState();
  if (!currentState) return null;
  const values = configMutationValues(context.uiState, target)
    ?? (currentState.overlay === "config"
      ? currentState.config_fields.map((field) => ({ key: field.key, text: field.value }))
      : null);
  if (!values || !validateConfigValues(values, true)) return null;
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
  const currentState = context.getCurrentState();
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
  currentState.config_fields[index].value = text;
  if (index === currentState.selected_config_index) {
    currentState.config_value_text = text;
    currentState.config_field_title = key;
  }
  return true;
}

function validateSettingsForm(uiState: UiLocalState, focusInvalid: boolean): boolean {
  const controls = collectSettingsControls();
  const validation = document.querySelector<HTMLElement>("#settings-validation");
  for (const control of controls) {
    const key = control.dataset.configKey ?? "";
    if (!key) continue;
    const result = validateConfigInput(key, settingsControlValue(control));
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
    validation.textContent = "入力形式は問題ありません。";
    validation.classList.toggle("ok", true);
    validation.classList.toggle("error", false);
  }
  updateDirtyBadges(uiState);
  return true;
}

function validateConfigValues(values: ConfigValueInput[], focusInvalid: boolean): boolean {
  for (const value of values) {
    const result = validateConfigInput(value.key, value.text);
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
  const setupRequired = document.querySelector(".settings-modal.setup-modal") !== null;
  const commitEnabled = configCommitEnabled(
    setupRequired,
    uiState.configDirty,
    configMutationPending(uiState),
  );
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
  const currentState = context.getCurrentState();
  if (currentState) currentState.window_opacity_percent = percent;
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
    context.renderError(String(error));
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
      context.render(state);
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
      context.render(state);
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
      await context.mutate("send_prompt_review", { enhanced: true });
      return;
    case "send-review-raw":
      await context.mutate("send_prompt_review", { enhanced: false });
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
        const request = beginConfigMutation(context.uiState, state.config_target);
        context.render(state);
        let nextState: DesktopWebState;
        let imported: boolean;
        try {
          [nextState, imported] = await command<[DesktopWebState, boolean]>("import_global_config_toml", {
            expectedTarget: request.target,
          });
        } catch (error) {
          const finished = finishConfigMutation(context.uiState, request, false, context.getCurrentState()?.config_target ?? null);
          if (context.recoverCommandConflict(error)) return;
          if (!finished) return;
          const latest = context.getCurrentState();
          if (latest) context.render(latest);
          context.renderError(String(error));
          return;
        }
        if (!finishConfigMutation(context.uiState, request, imported, context.getCurrentState()?.config_target ?? null)) return;
        context.setCurrentState(nextState);
        context.render(nextState);
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
  const state = context.getCurrentState();
  if (state) context.render(state);
  try {
    const nextState = await command<DesktopWebState>(name, { index, expectedTarget });
    finishLocalDecision(context.uiState);
    context.uiState.pendingLocalConfirmation = null;
    context.render(nextState);
  } catch (error) {
    if (context.recoverCommandConflict(error)) return;
    failLocalDecision(context.uiState, "処理を開始できませんでした。もう一度お試しください。");
    const latest = context.getCurrentState();
    if (latest) context.render(latest);
    context.renderError(String(error));
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
              : overlayKey === "permission" || overlayKey === "local-confirm"
                ? ".modal-actions button:not(:disabled), .permission-decision-status"
                : isRegularModalOverlay(overlayKey)
                  ? ".modal button:not(:disabled), .modal[role='dialog']"
                  : "";
  if (!selector) {
    return;
  }
  requestAnimationFrame(() => {
    const target = document.querySelector<HTMLElement>(selector);
    target?.focus();
    if (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement) {
      const end = target.value.length;
      target.setSelectionRange(end, end);
    }
  });
}

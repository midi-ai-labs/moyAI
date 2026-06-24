import { command } from "./api";
import { dispatchRegisteredAction, type ActionContext } from "./actions";
import type { DesktopWebState } from "./types";
import type { UiLocalState } from "./ui_state";
import { goalSlashCommandHint, validateConfigInput } from "./utils";

let pendingOpacityPreviewPercent: number | null = null;
let opacityPreviewFrame: number | null = null;
let opacityPreviewInFlight = false;
const TEXT_MUTATION_DEBOUNCE_MS = 180;
const MIN_WINDOW_OPACITY_PERCENT = 50;
const MAX_WINDOW_OPACITY_PERCENT = 100;

type SettingsControl = HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement;

interface ConfigValueInput {
  index: number;
  text: string;
}

interface PendingTextMutation {
  name: string;
  args: Record<string, unknown> | null;
  timer: number | null;
  inFlight: Promise<void> | null;
}

const pendingTextMutations = new Map<string, PendingTextMutation>();

export function installGlobalKeyboardShortcuts(context: ActionContext): void {
  document.addEventListener("keydown", (event) => {
    const currentState = context.getCurrentState();
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
    if (event.key === "F8" && currentState && !currentState.busy) {
      event.preventDefault();
      void dispatchRegisteredAction("toggle-access", currentState, context, { index: -1, value: "" });
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
  document.querySelectorAll<HTMLElement>('[data-action="close-window"]').forEach((node) => {
    node.addEventListener("pointerdown", (event) => {
      if (event.button !== 0) return;
      event.preventDefault();
      event.stopPropagation();
      void command("hide_to_tray").catch(() => context.desktopWindow.hide());
    });
  });
  document.querySelector<HTMLTextAreaElement>("#prompt")?.addEventListener("input", (event) => {
    const text = (event.currentTarget as HTMLTextAreaElement).value;
    const currentState = context.getCurrentState();
    if (currentState) {
      currentState.draft_prompt = text;
      currentState.can_submit = text.trim().length > 0 && !currentState.busy;
      currentState.enhance_enabled = text.trim().length > 0 && !currentState.busy;
      updateGoalCommandHint(text);
      const send = document.querySelector<HTMLButtonElement>('[data-action="send"]');
      if (send) send.disabled = !currentState.can_submit;
      const enhance = document.querySelector<HTMLButtonElement>('[data-action="enhance-prompt"]');
      if (enhance) {
        enhance.disabled = !currentState.enhance_enabled;
        const title = currentState.busy
          ? "実行中はEnhanceできません"
          : text.trim().length === 0
            ? "依頼文を入力してください"
            : "Enhance";
        enhance.title = title;
        enhance.setAttribute("aria-label", title);
      }
    }
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
      updateSettingsControlDraft(control, context);
      markConfigDirty(context.uiState);
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
  document.querySelector<HTMLInputElement>("#local-search")?.addEventListener("input", (event) => {
    void context.mutate("set_local_search", { text: (event.currentTarget as HTMLInputElement).value });
  });
  document.querySelector<HTMLInputElement>("#session-search")?.addEventListener("input", (event) => {
    void context.mutate("set_session_search", { text: (event.currentTarget as HTMLInputElement).value });
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
      const value = node.dataset.mode ?? "";
      void dispatchAction(action, index, value, state, context);
    });
  });
  document.querySelectorAll<HTMLElement>("[data-drag-region], [data-tauri-drag-region]").forEach((node) => {
    node.addEventListener("mousedown", (event) => {
      if (event.button !== 0 || (event.target as HTMLElement).closest("button")) return;
      event.preventDefault();
      event.stopPropagation();
      void command("start_window_drag").catch(() => context.desktopWindow.startDragging());
    });
  });
  focusOverlayPrimary(state, context.uiState);
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

function scheduleTextMutation(key: string, name: string, args: Record<string, unknown>, context: ActionContext): void {
  const existing = pendingTextMutations.get(key);
  const entry = existing ?? { name, args: null, timer: null, inFlight: null };
  entry.name = name;
  entry.args = args;
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
  entry.args = null;
  entry.inFlight = command<DesktopWebState>(name, args)
    .then((state) => {
      if (entry.args === null) context.setCurrentState(state);
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

export async function flushConfigInputMutations(context: ActionContext): Promise<boolean> {
  const values = collectSettingsFormValues();
  if (values.length > 0) {
    if (!validateSettingsForm(context.uiState, true)) return false;
    try {
      const nextState = await command<DesktopWebState>("set_config_values", { values });
      context.setCurrentState(nextState);
      return true;
    } catch (error) {
      context.renderError(String(error));
      return false;
    }
  }
  await flushTextMutation("config-value", context);
  return true;
}

function collectSettingsControls(): SettingsControl[] {
  return Array.from(document.querySelectorAll<SettingsControl>(".settings-control"));
}

function collectSettingsFormValues(): ConfigValueInput[] {
  const valuesByIndex = new Map<number, ConfigValueInput>();
  for (const control of collectSettingsControls()) {
    const index = Number(control.dataset.configIndex ?? "-1");
    if (!Number.isInteger(index) || index < 0) continue;
    valuesByIndex.set(index, { index, text: settingsControlValue(control) });
  }
  return Array.from(valuesByIndex.values());
}

function settingsControlValue(control: SettingsControl): string {
  if (control instanceof HTMLInputElement && control.type === "checkbox") {
    return control.checked ? "true" : "false";
  }
  return control.value;
}

function updateSettingsControlDraft(control: SettingsControl, context: ActionContext): void {
  const index = Number(control.dataset.configIndex ?? "-1");
  if (!Number.isInteger(index) || index < 0) return;
  const currentState = context.getCurrentState();
  const text = settingsControlValue(control);
  if (!currentState || !currentState.config_fields[index]) return;
  currentState.config_fields[index].value = text;
  if (index === currentState.selected_config_index) {
    currentState.config_value_text = text;
    currentState.config_field_title = control.dataset.configKey ?? currentState.config_field_title;
  }
}

function markConfigDirty(uiState: UiLocalState): void {
  uiState.configDirty = true;
  updateDirtyBadges(uiState);
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

function updateDirtyBadges(uiState: UiLocalState): void {
  document.querySelectorAll<HTMLElement>(".dirty-badge").forEach((node) => {
    node.classList.toggle("visible", uiState.configDirty);
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
  if (action === "minimize-window") void context.desktopWindow.minimize();
  if (action === "toggle-maximize-window") void context.desktopWindow.toggleMaximize();
  if (action === "close-window") void command("hide_to_tray").catch(() => context.desktopWindow.hide());
  if (action === "send" && state.can_submit) void context.mutate("submit_prompt");
  if (action === "toggle-attachment-tray") {
    context.uiState.attachmentTrayOpen = !context.uiState.attachmentTrayOpen;
    return context.render(state);
  }
  if (action === "toggle-artifact-pane") return;
  if (action === "cancel-run" && (state.busy || state.confirmation_visible)) void context.mutate("cancel_run");
  if (action === "refresh") void context.mutate("refresh_desktop");
  if (action === "load-previous-turn-page") void context.mutate("load_previous_turn_page");
  if (action === "load-next-turn-page") void context.mutate("load_next_turn_page");
  if (action === "new-chat") void context.mutate("new_chat");
  if (action === "new-project-session") void context.mutate("new_project_session", { index });
  if (action === "project") void context.mutate("select_project", { index });
  if (action === "session") void context.mutate("select_session", { index });
  if (action === "rejoin-session") void context.mutate("rejoin_session", { index });
  if (action === "chat-session") void context.mutate("select_chat_session", { index });
  if (action === "toggle-session-archived-search") {
    void context.mutate("set_session_search_include_archived", { includeArchived: !state.session_search_include_archived });
  }
  if (action === "cancel-local-confirm") {
    context.uiState.pendingLocalConfirmation = null;
    return context.render(state);
  }
  if (action === "confirm-local-delete") return confirmLocalDelete(context);
  if (action === "confirm-local-archive-state") return confirmLocalArchiveState(context);
  if (action === "confirm-local-rollback") return confirmLocalRollback(context);
  if (action === "artifact") void context.mutate("select_artifact", { index });
  if (action === "export-transcript" && state.history_export_enabled) void context.mutate("export_transcript_markdown");
  if (action === "export-history") void context.mutate("export_history_markdown");
  if (action === "set-image") void context.mutate("attach_image");
  if (action === "browse-image") void context.mutate("browse_image");
  if (action === "clear-images") void context.mutate("clear_images");
  if (action === "remove-image") void context.mutate("remove_image", { index });
  if (action === "enhance-prompt") void context.mutate("enhance_prompt");
  if (action === "send-review-enhanced") void context.mutate("send_prompt_review", { enhanced: true });
  if (action === "send-review-raw") void context.mutate("send_prompt_review", { enhanced: false });
  if (action === "cancel-review") void context.mutate("cancel_prompt_review");
  if (action === "show-file-menu") void context.mutate("show_file_menu");
  if (action === "show-edit-menu") void context.mutate("show_edit_menu");
  if (action === "show-view-menu") void context.mutate("show_view_menu");
  if (action === "show-help-menu") void context.mutate("show_help_menu");
  if (action === "create-project-from-picker") void context.mutate("create_project_from_picker");
  if (action === "show-provider") void context.mutate("show_provider_editor");
  if (action === "show-config") void context.mutate("show_config_editor");
  if (action === "show-command-palette") void context.mutate("show_command_palette");
  if (action === "show-shortcuts") void context.mutate("show_shortcuts");
  if (action === "close-overlay") {
    if (startupSetupRequired(state)) return;
    context.uiState.configDirty = false;
    void context.mutate("close_overlay");
  }
  if (action === "switch-workspace") void context.mutate("switch_workspace");
  if (action === "browse-workspace") void context.mutate("browse_workspace");
  if (action === "open-workspace-folder") void context.mutate("open_workspace_folder");
  if (action === "open-global-config-folder") void context.mutate("open_global_config_folder");
  if (action === "import-config-toml") void context.mutate("import_global_config_toml");
  if (action === "open-typed-path") void context.mutate("open_typed_path");
  if (action === "open-artifact-folder") void context.mutate("open_artifact_folder");
  if (action === "load-provider-models") {
    await flushProviderInputMutations(context);
    void context.mutate("load_provider_models");
  }
  if (action === "set-provider-mode") {
    void context.mutate("set_provider_metadata_mode", { mode: value });
  }
  if (action === "select-provider-model") void context.mutate("select_provider_model", { index });
  if (action === "apply-provider-session") {
    await flushProviderInputMutations(context);
    void context.mutate("apply_provider_session");
  }
  if (action === "save-provider-global") {
    await flushProviderInputMutations(context);
    void context.mutate("save_provider_global");
  }
  if (action === "select-config") {
    context.uiState.configDirty = false;
    void context.mutate("set_config_selection", { index });
  }
  if (action === "apply-session-config") {
    if (await flushConfigInputMutations(context)) {
      submitConfigAction("apply_session_config", state, context);
    }
  }
  if (action === "save-global-config") {
    if (await flushConfigInputMutations(context)) {
      submitConfigAction("save_global_config", state, context);
    }
  }
  if (action === "toggle-access") void context.mutate("toggle_access_mode");
  if (action === "insert-command") void context.mutate("insert_command", { index });
  if (action === "allow") void context.mutate("answer_permission", { allow: true });
  if (action === "deny") void context.mutate("answer_permission", { allow: false });
}

function startupSetupRequired(state: DesktopWebState): boolean {
  return (
    (state.startup.status === "requires_config" || state.startup.status === "requires_provider") &&
    state.startup.action_overlay === state.overlay
  );
}

function confirmLocalDelete(context: ActionContext): void {
  const pending = context.uiState.pendingLocalConfirmation;
  if (!pending) {
    return;
  }
  context.uiState.pendingLocalConfirmation = null;
  if (pending.kind === "project") {
    void context.mutate("delete_project", { index: pending.index });
  } else if (pending.kind === "chat_session") {
    void context.mutate("delete_chat_session", { index: pending.index });
  } else {
    void context.mutate("delete_session", { index: pending.index });
  }
}

function confirmLocalArchiveState(context: ActionContext): void {
  const pending = context.uiState.pendingLocalConfirmation;
  if (!pending || (pending.kind !== "archive_session" && pending.kind !== "unarchive_session")) {
    return;
  }
  context.uiState.pendingLocalConfirmation = null;
  void context.mutate(pending.kind === "archive_session" ? "archive_session" : "unarchive_session", { index: pending.index });
}

function confirmLocalRollback(context: ActionContext): void {
  const pending = context.uiState.pendingLocalConfirmation;
  if (!pending || pending.kind !== "rollback_session") {
    return;
  }
  context.uiState.pendingLocalConfirmation = null;
  void context.mutate("rollback_session", { index: pending.index });
}

function submitConfigAction(commandName: string, _state: DesktopWebState, context: ActionContext): void {
  if (!validateSettingsForm(context.uiState, true)) return;
  context.uiState.configDirty = false;
  updateDirtyBadges(context.uiState);
  void context.mutate(commandName);
}

function focusOverlayPrimary(state: DesktopWebState, uiState: UiLocalState): void {
  const overlayKey = uiState.pendingLocalConfirmation ? "local-confirm" : state.confirmation_visible ? "permission" : state.overlay;
  if (overlayKey === uiState.lastFocusedOverlay) {
    return;
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
                ? ".modal-actions button"
                : "";
  if (!selector) {
    return;
  }
  requestAnimationFrame(() => {
    document.querySelector<HTMLElement>(selector)?.focus();
  });
}

import { command } from "./api";
import type { DesktopWebState, ProjectRow, SessionRow } from "./types";
import { setArtifactPaneCollapsed, type UiLocalState } from "./ui_state";
import { validateConfigInput } from "./utils";

interface EventContext {
  desktopWindow: {
    hide: () => Promise<void>;
    minimize: () => Promise<void>;
    toggleMaximize: () => Promise<void>;
    startDragging: () => Promise<void>;
  };
  uiState: UiLocalState;
  getCurrentState: () => DesktopWebState | null;
  setCurrentState: (state: DesktopWebState) => void;
  render: (state: DesktopWebState) => void;
  mutate: (name: string, args?: Record<string, unknown>) => Promise<void>;
  renderError: (message: string) => void;
}

let pendingOpacityPreviewPercent: number | null = null;
let opacityPreviewFrame: number | null = null;
let opacityPreviewInFlight = false;
const MIN_WINDOW_OPACITY_PERCENT = 50;
const MAX_WINDOW_OPACITY_PERCENT = 100;

export function installGlobalKeyboardShortcuts(context: EventContext): void {
  document.addEventListener("keydown", (event) => {
    const currentState = context.getCurrentState();
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
      event.preventDefault();
      void context.mutate("show_command_palette");
    }
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "n") {
      event.preventDefault();
      void context.mutate("new_chat");
    }
    if ((event.ctrlKey || event.metaKey) && event.key === "Enter" && currentState?.can_submit) {
      event.preventDefault();
      void context.mutate("submit_prompt");
    }
    if (event.key === "Escape" && currentState?.overlay !== "none") {
      event.preventDefault();
      void context.mutate("close_overlay");
    }
  });
}

export function wireEvents(state: DesktopWebState, context: EventContext): void {
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
      const send = document.querySelector<HTMLButtonElement>('[data-action="send"]');
      if (send) send.disabled = !currentState.can_submit;
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
    void command<DesktopWebState>("set_provider_base_url", {
      text: (event.currentTarget as HTMLInputElement).value,
    })
      .then(context.setCurrentState)
      .catch((error) => context.renderError(String(error)));
  });
  document.querySelector<HTMLTextAreaElement>("#config-value")?.addEventListener("input", (event) => {
    const text = (event.currentTarget as HTMLTextAreaElement).value;
    context.uiState.configDirty = true;
    updateConfigValidation(state.config_field_title, text, context.uiState);
    void command<DesktopWebState>("set_config_value", { text })
      .then(context.setCurrentState)
      .catch((error) => context.renderError(String(error)));
  });
  document.querySelector<HTMLInputElement>("#config-filter")?.addEventListener("input", (event) => {
    context.uiState.configFilterText = (event.currentTarget as HTMLInputElement).value;
    context.render(state);
  });
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
      dispatchAction(action, index, state, context);
    });
  });
  document.querySelectorAll<HTMLElement>("[data-drag-region], [data-tauri-drag-region]").forEach((node) => {
    node.addEventListener("mousedown", (event) => {
      if (event.button !== 0 || (event.target as HTMLElement).closest("button")) return;
      void command("start_window_drag").catch(() => context.desktopWindow.startDragging());
    });
  });
  focusOverlayPrimary(state, context.uiState);
}

function scheduleOpacityPreview(percent: number, context: EventContext): void {
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

async function flushOpacityPreview(context: EventContext): Promise<void> {
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

function dispatchAction(action: string, index: number, state: DesktopWebState, context: EventContext): void {
  if (action === "minimize-window") void context.desktopWindow.minimize();
  if (action === "toggle-maximize-window") void context.desktopWindow.toggleMaximize();
  if (action === "close-window") void command("hide_to_tray").catch(() => context.desktopWindow.hide());
  if (action === "send" && state.can_submit) void context.mutate("submit_prompt");
  if (action === "toggle-attachment-tray") {
    context.uiState.attachmentTrayOpen = !context.uiState.attachmentTrayOpen;
    return context.render(state);
  }
  if (action === "toggle-artifact-pane") {
    setArtifactPaneCollapsed(context.uiState, !context.uiState.artifactPaneCollapsed);
    return context.render(state);
  }
  if (action === "cancel-run" && (state.busy || state.confirmation_visible)) void context.mutate("cancel_run");
  if (action === "refresh") void context.mutate("refresh_desktop");
  if (action === "new-chat") void context.mutate("new_chat");
  if (action === "new-project-session") void context.mutate("new_project_session", { index });
  if (action === "project") void context.mutate("select_project", { index });
  if (action === "session") void context.mutate("select_session", { index });
  if (action === "chat-session") void context.mutate("select_chat_session", { index });
  if (action === "delete-project") return requestLocalDelete("project", index, state, context);
  if (action === "delete-session") return requestLocalDelete("session", index, state, context);
  if (action === "delete-chat-session") return requestLocalDelete("chat_session", index, state, context);
  if (action === "cancel-local-confirm") {
    context.uiState.pendingLocalConfirmation = null;
    return context.render(state);
  }
  if (action === "confirm-local-delete") return confirmLocalDelete(context);
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
    context.uiState.configDirty = false;
    void context.mutate("close_overlay");
  }
  if (action === "switch-workspace") void context.mutate("switch_workspace");
  if (action === "browse-workspace") void context.mutate("browse_workspace");
  if (action === "open-workspace-folder") void context.mutate("open_workspace_folder");
  if (action === "open-project-config-folder") void context.mutate("open_project_config_folder");
  if (action === "open-global-config-folder") void context.mutate("open_global_config_folder");
  if (action === "open-typed-path") void context.mutate("open_typed_path");
  if (action === "open-artifact-folder") void context.mutate("open_artifact_folder");
  if (action === "load-provider-models") void context.mutate("load_provider_models");
  if (action === "select-provider-model") void context.mutate("select_provider_model", { index });
  if (action === "apply-provider-session") void context.mutate("apply_provider_session");
  if (action === "save-provider-project") void context.mutate("save_provider_project");
  if (action === "save-provider-global") void context.mutate("save_provider_global");
  if (action === "select-config") {
    context.uiState.configDirty = false;
    void context.mutate("set_config_selection", { index });
  }
  if (action === "apply-session-config") submitConfigAction("apply_session_config", state, context);
  if (action === "save-project-config") submitConfigAction("save_project_config", state, context);
  if (action === "save-global-config") submitConfigAction("save_global_config", state, context);
  if (action === "toggle-access") void context.mutate("toggle_access_mode");
  if (action === "insert-command") void context.mutate("insert_command", { index });
  if (action === "allow") void context.mutate("answer_permission", { allow: true });
  if (action === "deny") void context.mutate("answer_permission", { allow: false });
}

function requestLocalDelete(
  kind: "project" | "session" | "chat_session",
  index: number,
  state: DesktopWebState,
  context: EventContext
): void {
  if (state.busy) {
    return;
  }
  const row =
    kind === "project" ? state.project_rows[index] : kind === "chat_session" ? state.chat_session_rows[index] : state.session_rows[index];
  if (!row) {
    return;
  }
  context.uiState.pendingLocalConfirmation = {
    kind,
    index,
    title: row.label,
    detail: kind === "project" ? (row as ProjectRow).path : (row as SessionRow).session_id,
  };
  context.render(state);
}

function confirmLocalDelete(context: EventContext): void {
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

function submitConfigAction(commandName: string, state: DesktopWebState, context: EventContext): void {
  const value = document.querySelector<HTMLTextAreaElement>("#config-value")?.value ?? state.config_value_text;
  const result = validateConfigInput(state.config_field_title, value);
  if (!result.ok) {
    updateConfigValidation(state.config_field_title, value, context.uiState);
    document.querySelector<HTMLTextAreaElement>("#config-value")?.focus();
    return;
  }
  context.uiState.configDirty = false;
  void context.mutate(commandName);
}

function updateConfigValidation(field: string, value: string, uiState: UiLocalState): void {
  const node = document.querySelector<HTMLElement>("#config-validation");
  const result = validateConfigInput(field, value);
  if (node) {
    node.textContent = result.message;
    node.classList.toggle("ok", result.ok);
    node.classList.toggle("error", !result.ok);
  }
  document.querySelector<HTMLElement>(".dirty-badge")?.classList.toggle("visible", uiState.configDirty);
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

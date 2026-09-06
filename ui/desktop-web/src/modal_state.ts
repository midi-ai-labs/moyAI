const REGULAR_MODAL_OVERLAYS = new Set([
  "provider",
  "config",
  "hub",
  "mcp_publish",
  "session_settings",
  "workspace",
  "prompt_review",
  "command_palette",
  "shortcuts",
  "about",
]);

export function isRegularModalOverlay(overlay: string): boolean {
  return REGULAR_MODAL_OVERLAYS.has(overlay);
}

export function modalIsOpen(
  state: { confirmation_visible: boolean; overlay: string },
  localModalOpen: boolean,
): boolean {
  return state.confirmation_visible
    || localModalOpen
    || state.overlay === "initial_setup"
    || isRegularModalOverlay(state.overlay);
}

export interface SideChatDeleteModalTarget {
  ownerSessionId: string;
  chatId: string;
  expectedGeneration: string;
}

export function localModalIdentity(
  localConfirmationOpen: boolean,
  sideChatDeleteTarget: SideChatDeleteModalTarget | null,
): string | null {
  if (localConfirmationOpen) return "local-confirm";
  if (!sideChatDeleteTarget) return null;
  return `side-chat-delete:${JSON.stringify([
    sideChatDeleteTarget.ownerSessionId,
    sideChatDeleteTarget.chatId,
    sideChatDeleteTarget.expectedGeneration,
  ])}`;
}

export function modalIdentity(state: {
  confirmation_visible: boolean;
  confirmation_id?: string | null;
  overlay: string;
}): string {
  return state.confirmation_visible
    ? `permission:${state.confirmation_id ?? "unknown"}`
    : state.overlay;
}

export function nextDialogFocusIndex(currentIndex: number, focusableCount: number, backwards: boolean): number {
  if (focusableCount <= 0) return -1;
  if (backwards) {
    return currentIndex <= 0 ? focusableCount - 1 : currentIndex - 1;
  }
  return currentIndex < 0 || currentIndex >= focusableCount - 1 ? 0 : currentIndex + 1;
}

export function overlayPrimaryFocusRequired(
  overlay: string,
  focusOwner: string,
  lastFocusedOverlay: string,
  confirmationOverlay: boolean,
  hasMeaningfulActiveElement: boolean,
): boolean {
  if (hasMeaningfulActiveElement) return false;
  if (!confirmationOverlay && overlay === "config" && focusOwner === lastFocusedOverlay) return false;
  return true;
}

export function overlayPrimaryFocusSelectors(overlay: string): readonly string[] {
  if (overlay === "command_palette") return ["#local-search"];
  if (overlay === "provider") return ["#provider-url"];
  if (overlay === "config") return [".settings-control"];
  if (overlay === "hub") return ["#hub-tab-devices:not(:disabled)", ".hub-modal"];
  if (overlay === "mcp_publish") return ["#mcp-publish-label:not(:disabled)", "#mcp-publish-add:not(:disabled)", ".mcp-publish-modal"];
  if (overlay === "session_settings") {
    return [
      ".session-settings-control:not(:disabled):not([aria-disabled='true'])",
      ".session-settings-modal",
    ];
  }
  if (overlay === "initial_setup") {
    return ["#initial-setup-primary", ".initial-setup-shell .settings-control"];
  }
  if (overlay === "workspace") return ["#workspace-input"];
  if (overlay === "prompt_review") return ["#review-draft"];
  if (!isRegularModalOverlay(overlay)) return [];
  return [
    ".modal button:not(:disabled)",
    ".modal[role='dialog']",
  ];
}

export function confirmationFocusSelectors(pending: boolean): readonly string[] {
  if (pending) return [".permission-decision-status"];
  return [
    ".modal-actions button[autofocus]:not(:disabled)",
    ".modal-actions button:not(:disabled)",
    ".permission-decision-status",
  ];
}

export function confirmationFocusIsMeaningful(pending: boolean, statusFocused: boolean): boolean {
  return !statusFocused || pending;
}

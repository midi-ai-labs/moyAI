const REGULAR_MODAL_OVERLAYS = new Set([
  "provider",
  "config",
  "workspace",
  "prompt_review",
  "command_palette",
  "shortcuts",
]);

export function isRegularModalOverlay(overlay: string): boolean {
  return REGULAR_MODAL_OVERLAYS.has(overlay);
}

export function modalIsOpen(
  state: { confirmation_visible: boolean; overlay: string },
  localConfirmationOpen: boolean,
): boolean {
  return state.confirmation_visible || localConfirmationOpen || isRegularModalOverlay(state.overlay);
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

export function confirmationFocusSelectors(pending: boolean): readonly string[] {
  if (pending) return [".permission-decision-status"];
  return [
    ".modal-actions button[autofocus]:not(:disabled)",
    ".modal-actions button:not(:disabled)",
    ".permission-decision-status",
  ];
}

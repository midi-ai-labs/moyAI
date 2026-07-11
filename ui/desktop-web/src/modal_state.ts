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

export function nextDialogFocusIndex(currentIndex: number, focusableCount: number, backwards: boolean): number {
  if (focusableCount <= 0) return -1;
  if (backwards) {
    return currentIndex <= 0 ? focusableCount - 1 : currentIndex - 1;
  }
  return currentIndex < 0 || currentIndex >= focusableCount - 1 ? 0 : currentIndex + 1;
}

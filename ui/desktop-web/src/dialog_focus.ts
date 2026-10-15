import { nextDialogFocusIndex } from "./modal_state.ts";

const DIALOG_FOCUSABLE_SELECTOR = [
  "button",
  "input",
  "select",
  "textarea",
  "a[href]",
  "summary",
  "[contenteditable]:not([contenteditable='false'])",
  "[tabindex]",
].join(", ");

const INACTIVE_FOCUS_ANCESTOR_SELECTOR =
  "[hidden], [aria-hidden='true'], [inert]";

/**
 * Returns the connected, visible controls that can actually receive focus in a dialog.
 *
 * The browser does not expose descendants of a closed `details` element in the focus order,
 * even though those descendants still match ordinary `input` / `button` selectors. Requiring a
 * layout box keeps those descendants (and controls under `display: none`) out while preserving
 * the visible `summary` that opens the disclosure. Inert and accessibility-hidden subtrees are
 * excluded explicitly because they can retain layout boxes.
 */
export function dialogFocusTargets(dialog: HTMLElement): HTMLElement[] {
  return Array.from(
    dialog.querySelectorAll<HTMLElement>(DIALOG_FOCUSABLE_SELECTOR),
  ).filter(dialogFocusTargetIsAvailable);
}

export function dialogFocusTargetIsAvailable(target: HTMLElement): boolean {
  if (
    !target.isConnected
    || target.hidden
    || target.getAttribute("aria-hidden") === "true"
    || target.getAttribute("aria-disabled") === "true"
    || target.closest(INACTIVE_FOCUS_ANCESTOR_SELECTOR) !== null
    || target.matches(":disabled")
  ) return false;

  const explicitTabIndex = target.getAttribute("tabindex");
  if (explicitTabIndex !== null && Number(explicitTabIndex) < 0) return false;

  const view = target.ownerDocument.defaultView;
  if (view) {
    const style = view.getComputedStyle(target);
    if (
      style.display === "none"
      || style.visibility === "hidden"
      || style.visibility === "collapse"
    ) return false;
  }

  return target.getClientRects().length > 0;
}

/**
 * Moves focus cyclically inside one dialog. If a candidate unexpectedly refuses focus, continue
 * in the requested direction instead of leaving the user trapped on the previous control.
 */
export function moveDialogFocus(
  dialog: HTMLElement,
  activeElement: Element | null,
  backwards: boolean,
): boolean {
  const targets = dialogFocusTargets(dialog);
  if (targets.length === 0) return false;

  let index = targets.indexOf(activeElement as HTMLElement);
  for (let attempt = 0; attempt < targets.length; attempt += 1) {
    index = nextDialogFocusIndex(index, targets.length, backwards);
    const target = targets[index];
    // Tab is an explicit navigation request. Let the browser reveal an offscreen control
    // within its scroll containers; passive render restoration still preserves scroll.
    target?.focus();
    if (target && target.ownerDocument.activeElement === target) return true;
  }
  return false;
}

/** Keeps a permission decision status (or the dialog itself) as the fail-safe owner. */
export function containDialogFocus(
  dialog: HTMLElement,
  activeElement: Element | null,
  backwards: boolean,
): boolean {
  if (moveDialogFocus(dialog, activeElement, backwards)) return true;
  const fallback = dialog.querySelector<HTMLElement>(".permission-decision-status") ?? dialog;
  fallback.focus({ preventScroll: true });
  return fallback.ownerDocument.activeElement === fallback;
}

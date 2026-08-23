export interface TitlebarPointerSample {
  pointerId: number;
  button: number;
  buttons: number;
  clientX: number;
  clientY: number;
  inDragRegion: boolean;
  inWindowControl: boolean;
}

export type TitlebarMenuName = "file" | "edit" | "view" | "help";
export type TitlebarMenuPopupRole = "menu" | "dialog";

export type TitlebarMenuKeyboardDecision =
  | { kind: "close" }
  | { kind: "close-natural" }
  | { kind: "move"; index: number }
  | { kind: "native" };

const TITLEBAR_MENU_OVERLAYS: Readonly<Record<string, TitlebarMenuName>> = {
  file_menu: "file",
  edit_menu: "edit",
  view_menu: "view",
  help_menu: "help",
};

export function titlebarMenuFromOverlay(overlay: string): TitlebarMenuName | null {
  return TITLEBAR_MENU_OVERLAYS[overlay] ?? null;
}

export function titlebarMenuPopupRole(menu: TitlebarMenuName): TitlebarMenuPopupRole {
  return menu === "view" ? "dialog" : "menu";
}

export function titlebarMenuTriggerAction(overlay: string): string | null {
  const menu = titlebarMenuFromOverlay(overlay);
  return menu ? `show-${menu}-menu` : null;
}

export function titlebarMenuKeyboardDecision(
  key: string,
  currentIndex: number,
  actionCount: number,
  nativeWidget: boolean,
): TitlebarMenuKeyboardDecision {
  if (key === "Escape") return { kind: "close" };
  if (key === "Tab") return nativeWidget ? { kind: "native" } : { kind: "close-natural" };
  if (nativeWidget || actionCount <= 0) return { kind: "native" };
  if (key === "Home") return { kind: "move", index: 0 };
  if (key === "End") return { kind: "move", index: actionCount - 1 };
  if (key === "ArrowDown") {
    return { kind: "move", index: currentIndex < 0 || currentIndex >= actionCount - 1 ? 0 : currentIndex + 1 };
  }
  if (key === "ArrowUp") {
    return { kind: "move", index: currentIndex <= 0 ? actionCount - 1 : currentIndex - 1 };
  }
  return { kind: "native" };
}

export function applyTitlebarMenuRovingTabIndex<T extends { tabIndex: number }>(
  actions: readonly T[],
  activeIndex: number,
): void {
  for (const [index, action] of actions.entries()) {
    action.tabIndex = index === activeIndex ? 0 : -1;
  }
}

export function titlebarMenuUsesRovingFocus(role: string | null): boolean {
  return role === "menu";
}

export function titlebarMenuTabContinuationAction(
  actions: readonly string[],
  triggerAction: string,
  backwards: boolean,
): string | null {
  if (actions.length === 0) return null;
  const triggerIndex = actions.indexOf(triggerAction);
  if (triggerIndex < 0) return null;
  const nextIndex = backwards
    ? (triggerIndex === 0 ? actions.length - 1 : triggerIndex - 1)
    : (triggerIndex >= actions.length - 1 ? 0 : triggerIndex + 1);
  return actions[nextIndex] ?? null;
}

export function focusTitlebarMenuContinuation(
  titlebar: Pick<ParentNode, "querySelectorAll">,
  action: string,
): boolean {
  const target = Array.from(
    titlebar.querySelectorAll<HTMLElement>("button[data-action]:not(:disabled):not([aria-disabled='true'])"),
  ).find((candidate) => candidate.dataset.action === action);
  if (!target) return false;
  target.focus({ preventScroll: true });
  return true;
}

interface PendingTitlebarPointer {
  pointerId: number;
  clientX: number;
  clientY: number;
  kind: "drag-region" | "window-control";
}

export class TitlebarDragGesture {
  private pending: PendingTitlebarPointer | null = null;
  private suppressNextWindowControlPointerClick = false;
  private readonly thresholdPx: number;

  constructor(thresholdPx = 4) {
    this.thresholdPx = thresholdPx;
  }

  pointerDown(sample: TitlebarPointerSample): boolean {
    this.pending = null;
    this.suppressNextWindowControlPointerClick = false;
    if (sample.button !== 0) return false;
    const kind = sample.inWindowControl
      ? "window-control"
      : sample.inDragRegion
        ? "drag-region"
        : null;
    if (!kind) return false;
    this.pending = {
      pointerId: sample.pointerId,
      clientX: sample.clientX,
      clientY: sample.clientY,
      kind,
    };
    return kind === "drag-region";
  }

  pointerMove(sample: TitlebarPointerSample): boolean {
    const pending = this.pending;
    if (!pending || pending.pointerId !== sample.pointerId) return false;
    if ((sample.buttons & 1) === 0) {
      this.pending = null;
      return false;
    }
    const distance = Math.hypot(
      sample.clientX - pending.clientX,
      sample.clientY - pending.clientY,
    );
    if (distance < this.thresholdPx) return false;
    this.pending = null;
    if (pending.kind === "window-control") {
      this.suppressNextWindowControlPointerClick = true;
      return false;
    }
    return true;
  }

  pointerUp(pointerId: number): void {
    if (this.pending?.pointerId === pointerId) this.pending = null;
  }

  cancel(): void {
    this.pending = null;
    this.suppressNextWindowControlPointerClick = false;
  }

  consumeWindowControlClickSuppression(pointerOrigin: boolean): boolean {
    if (!pointerOrigin || !this.suppressNextWindowControlPointerClick) return false;
    this.suppressNextWindowControlPointerClick = false;
    return true;
  }

  doubleClick(sample: Pick<TitlebarPointerSample, "button" | "inDragRegion" | "inWindowControl">): boolean {
    this.cancel();
    return sample.button === 0 && sample.inDragRegion && !sample.inWindowControl;
  }
}

export function windowControlKeyboardActivation(key: string, repeat: boolean): boolean {
  return !repeat && (key === "Enter" || key === " " || key === "Spacebar");
}

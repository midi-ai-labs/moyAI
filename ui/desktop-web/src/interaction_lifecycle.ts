export interface InteractionRelease<T> {
  deferred: T | null;
  renderCurrent: boolean;
}

export function shouldBeginPointerInteraction(button: number, withinApp: boolean): boolean {
  return button === 0 && withinApp;
}

export function shouldBeginKeyboardInteraction(
  isComposing: boolean,
  code: string,
  withinApp: boolean,
  disabledControl: boolean,
): boolean {
  return !isComposing && code !== "Unidentified" && withinApp && !disabledControl;
}

export class InteractionLifecycle<T> {
  private readonly pointers = new Set<number>();
  private readonly keys = new Set<string>();
  private compositionDepth = 0;
  private deferred: T | null = null;
  private renderCurrent = false;
  private readonly preferCandidate: (current: T, candidate: T) => boolean;

  constructor(preferCandidate: (current: T, candidate: T) => boolean) {
    this.preferCandidate = preferCandidate;
  }

  get active(): boolean {
    return this.pointers.size > 0 || this.keys.size > 0 || this.compositionDepth > 0;
  }

  beginPointer(pointerId: number): boolean {
    if (this.pointers.has(pointerId)) return false;
    this.pointers.add(pointerId);
    return true;
  }

  endPointer(pointerId: number): InteractionRelease<T> | null {
    this.pointers.delete(pointerId);
    return this.releaseIfIdle();
  }

  beginKey(code: string): void {
    this.keys.add(code);
  }

  endKey(code: string): InteractionRelease<T> | null {
    this.keys.delete(code);
    return this.releaseIfIdle();
  }

  beginComposition(): void {
    this.compositionDepth += 1;
  }

  endComposition(): InteractionRelease<T> | null {
    this.compositionDepth = Math.max(0, this.compositionDepth - 1);
    return this.releaseIfIdle();
  }

  defer(candidate: T, sameProjection: boolean, shouldRender: boolean): boolean {
    if (!this.active) return false;
    if (sameProjection) {
      this.renderCurrent ||= shouldRender;
      return true;
    }
    const renderCandidate = candidate;
    if (!this.deferred || this.preferCandidate(this.deferred, renderCandidate)) {
      this.deferred = renderCandidate;
    }
    return true;
  }

  cancel(): InteractionRelease<T> {
    this.pointers.clear();
    this.keys.clear();
    this.compositionDepth = 0;
    return this.takeRelease();
  }

  private releaseIfIdle(): InteractionRelease<T> | null {
    return this.active ? null : this.takeRelease();
  }

  private takeRelease(): InteractionRelease<T> {
    const release = { deferred: this.deferred, renderCurrent: this.renderCurrent };
    this.deferred = null;
    this.renderCurrent = false;
    return release;
  }
}

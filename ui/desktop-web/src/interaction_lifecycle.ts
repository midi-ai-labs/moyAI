export interface InteractionRelease<T> {
  deferred: T | null;
  renderCurrent: boolean;
}

export type InteractionEnd<T> = () => InteractionRelease<T> | null;

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
  private readonly pointers = new Map<number, bigint>();
  private readonly keys = new Map<string, bigint>();
  private compositionGeneration: bigint | null = null;
  private nextGeneration = 1n;
  private deferred: T | null = null;
  private renderCurrent = false;
  private readonly preferCandidate: (current: T, candidate: T) => boolean;

  constructor(preferCandidate: (current: T, candidate: T) => boolean) {
    this.preferCandidate = preferCandidate;
  }

  get active(): boolean {
    return this.pointers.size > 0 || this.keys.size > 0 || this.compositionGeneration !== null;
  }

  beginPointer(pointerId: number): void {
    this.pointers.set(pointerId, this.issueGeneration());
  }

  capturePointerEnd(pointerId: number): InteractionEnd<T> | null {
    const generation = this.pointers.get(pointerId);
    if (generation === undefined) return null;
    return () => {
      if (this.pointers.get(pointerId) !== generation) return null;
      this.pointers.delete(pointerId);
      return this.releaseIfIdle();
    };
  }

  beginKey(code: string): void {
    this.keys.set(code, this.issueGeneration());
  }

  captureKeyEnd(code: string): InteractionEnd<T> | null {
    const generation = this.keys.get(code);
    if (generation === undefined) return null;
    return () => {
      if (this.keys.get(code) !== generation) return null;
      this.keys.delete(code);
      return this.releaseIfIdle();
    };
  }

  beginComposition(): void {
    this.compositionGeneration = this.issueGeneration();
  }

  captureCompositionEnd(): InteractionEnd<T> | null {
    const generation = this.compositionGeneration;
    if (generation === null) return null;
    return () => {
      if (this.compositionGeneration !== generation) return null;
      this.compositionGeneration = null;
      return this.releaseIfIdle();
    };
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
    this.compositionGeneration = null;
    return this.takeRelease();
  }

  private issueGeneration(): bigint {
    const generation = this.nextGeneration;
    this.nextGeneration += 1n;
    return generation;
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

export interface InteractionEventGateOptions<T> {
  documentTarget: Document;
  windowTarget: Window;
  appRoot: Element;
  lifecycle: InteractionLifecycle<T>;
  finish: (release: InteractionRelease<T> | null) => void;
}

export function installInteractionEventGate<T>(options: InteractionEventGateOptions<T>): () => void {
  const { documentTarget, windowTarget, appRoot, lifecycle, finish } = options;
  const cleanup: Array<() => void> = [];
  const pendingEnds = new Set<number>();
  let disposed = false;

  const onDocument = <K extends keyof DocumentEventMap>(
    type: K,
    listener: (event: DocumentEventMap[K]) => void,
    capture = false,
  ): void => {
    documentTarget.addEventListener(type, listener, capture);
    cleanup.push(() => documentTarget.removeEventListener(type, listener, capture));
  };
  const onWindow = <K extends keyof WindowEventMap>(
    type: K,
    listener: (event: WindowEventMap[K]) => void,
  ): void => {
    windowTarget.addEventListener(type, listener);
    cleanup.push(() => windowTarget.removeEventListener(type, listener));
  };
  const clearPendingEnds = (): void => {
    for (const timer of pendingEnds) windowTarget.clearTimeout(timer);
    pendingEnds.clear();
  };
  const scheduleEnd = (end: InteractionEnd<T> | null): void => {
    if (!end || disposed) return;
    const timer = windowTarget.setTimeout(() => {
      pendingEnds.delete(timer);
      if (!disposed) finish(end());
    }, 0);
    pendingEnds.add(timer);
  };
  const recoverInteraction = (): void => {
    if (disposed) return;
    clearPendingEnds();
    finish(lifecycle.cancel());
  };

  onDocument("pointerdown", (event) => {
    const target = event.target;
    if (!(target instanceof Element) || !shouldBeginPointerInteraction(event.button, appRoot.contains(target))) return;
    const directOwner = target.closest<HTMLElement>(
      'input, textarea, select, summary, [contenteditable="true"]',
    );
    const action = target.closest<HTMLElement>("[data-action]");
    const owner = directOwner ?? action;
    lifecycle.beginPointer(event.pointerId);
    if (owner && !owner.matches(":disabled")) {
      try {
        owner.setPointerCapture(event.pointerId);
      } catch {
        // Text selection, native scrollbars, and synthetic pointer events may not support capture.
      }
    }
  }, true);
  onDocument("pointerup", (event) => {
    scheduleEnd(lifecycle.capturePointerEnd(event.pointerId));
  }, true);
  onDocument("pointercancel", (event) => finish(lifecycle.capturePointerEnd(event.pointerId)?.() ?? null), true);
  onDocument("lostpointercapture", (event) => {
    scheduleEnd(lifecycle.capturePointerEnd(event.pointerId));
  }, true);

  onDocument("keydown", (event) => {
    const target = event.target;
    if (!(target instanceof Element)) return;
    const disabledOwner = target.closest<HTMLElement>(":disabled");
    const belongsToApp = appRoot.contains(target)
      || target === documentTarget.body
      || target === documentTarget.documentElement;
    if (!shouldBeginKeyboardInteraction(
      event.isComposing,
      event.code,
      belongsToApp,
      disabledOwner !== null,
    )) return;
    lifecycle.beginKey(event.code);
  }, true);
  onDocument("keyup", (event) => {
    scheduleEnd(lifecycle.captureKeyEnd(event.code));
  }, true);

  onDocument("compositionstart", () => {
    lifecycle.beginComposition();
  }, true);
  onDocument("compositionend", () => {
    scheduleEnd(lifecycle.captureCompositionEnd());
  }, true);

  onWindow("blur", recoverInteraction);
  onWindow("pagehide", recoverInteraction);
  onDocument("visibilitychange", () => {
    if (documentTarget.hidden) recoverInteraction();
  });

  return () => {
    if (disposed) return;
    disposed = true;
    for (const remove of cleanup) remove();
    clearPendingEnds();
    lifecycle.cancel();
  };
}

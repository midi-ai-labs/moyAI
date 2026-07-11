export class PointerRenderGate<T> {
  private activePointerId: number | null = null;
  private deferredValue: T | null = null;
  private readonly candidatePreferred: (current: T, candidate: T) => boolean;

  constructor(
    candidatePreferred: (current: T, candidate: T) => boolean = () => true,
  ) {
    this.candidatePreferred = candidatePreferred;
  }

  get active(): boolean {
    return this.activePointerId !== null;
  }

  begin(pointerId: number): boolean {
    if (this.activePointerId !== null) {
      return false;
    }
    this.activePointerId = pointerId;
    return true;
  }

  defer(value: T): boolean {
    if (this.activePointerId === null) {
      return false;
    }
    if (this.deferredValue === null || this.candidatePreferred(this.deferredValue, value)) {
      this.deferredValue = value;
    }
    return true;
  }

  end(pointerId: number): T | null {
    if (this.activePointerId !== pointerId) {
      return null;
    }
    return this.release();
  }

  cancel(): T | null {
    if (this.activePointerId === null) {
      return null;
    }
    return this.release();
  }

  private release(): T | null {
    this.activePointerId = null;
    const deferred = this.deferredValue;
    this.deferredValue = null;
    return deferred;
  }
}

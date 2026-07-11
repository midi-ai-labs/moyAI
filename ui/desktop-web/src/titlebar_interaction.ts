export interface TitlebarPointerSample {
  pointerId: number;
  button: number;
  buttons: number;
  clientX: number;
  clientY: number;
  inDragRegion: boolean;
  inWindowControl: boolean;
}

interface PendingTitlebarPointer {
  pointerId: number;
  clientX: number;
  clientY: number;
}

export class TitlebarDragGesture {
  private pending: PendingTitlebarPointer | null = null;
  private readonly thresholdPx: number;

  constructor(thresholdPx = 4) {
    this.thresholdPx = thresholdPx;
  }

  pointerDown(sample: TitlebarPointerSample): boolean {
    this.pending = null;
    if (
      sample.button !== 0 ||
      !sample.inDragRegion ||
      sample.inWindowControl
    ) {
      return false;
    }
    this.pending = {
      pointerId: sample.pointerId,
      clientX: sample.clientX,
      clientY: sample.clientY,
    };
    return true;
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
    return true;
  }

  pointerUp(pointerId: number): void {
    if (this.pending?.pointerId === pointerId) this.pending = null;
  }

  cancel(): void {
    this.pending = null;
  }

  doubleClick(sample: Pick<TitlebarPointerSample, "button" | "inDragRegion" | "inWindowControl">): boolean {
    this.pending = null;
    return sample.button === 0 && sample.inDragRegion && !sample.inWindowControl;
  }
}

export function windowControlKeyboardActivation(key: string, repeat: boolean): boolean {
  return !repeat && (key === "Enter" || key === " " || key === "Spacebar");
}

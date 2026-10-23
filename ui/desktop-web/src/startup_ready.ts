export function createStartupReadyNotification(
  notify: () => void,
  requestFrame: (callback: FrameRequestCallback) => number = requestAnimationFrame,
): () => void {
  let scheduled = false;
  return () => {
    if (scheduled) return;
    scheduled = true;
    // A second frame allows the completed main render to reach the screen first.
    requestFrame(() => requestFrame(notify));
  };
}

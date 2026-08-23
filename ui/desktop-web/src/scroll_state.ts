export interface ScrollRestorationTarget {
  style: { scrollBehavior: string };
  scrollTo(options: ScrollToOptions): void;
}

/** Restore a captured position synchronously even when user navigation uses smooth scrolling. */
export function restoreScrollPosition(
  target: ScrollRestorationTarget,
  scrollLeft: number,
  scrollTop: number,
): void {
  const previousInlineBehavior = target.style.scrollBehavior;
  target.style.scrollBehavior = "auto";
  try {
    target.scrollTo({ left: scrollLeft, top: scrollTop, behavior: "auto" });
  } finally {
    target.style.scrollBehavior = previousInlineBehavior;
  }
}

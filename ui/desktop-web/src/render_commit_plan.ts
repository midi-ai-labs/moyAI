/**
 * Decisions that `renderCommitted` makes around one DOM update. They read only the
 * previous and next projections and plain facts captured from the page, so each rule can be
 * tested without a document. `main.ts` performs the matching DOM work.
 */
import type { DesktopViewState } from "./types.ts";

type ThreadRetentionView = Pick<DesktopViewState, "overlay" | "hub_project_open" | "confirmation_visible">;

/**
 * Whether the conversation thread may be updated in place (keeping unchanged rows and any
 * text selection) instead of being replaced. Only an ordinary conversation view qualifies.
 */
export function conversationThreadRetainable(
  previous: ThreadRetentionView | null,
  next: ThreadRetentionView,
  render: { backgroundInert: boolean; sessionChanged: boolean; localModalWasOpen: boolean },
): boolean {
  return previous !== null
    && previous.overlay === "none" && next.overlay === "none"
    && !previous.hub_project_open && !next.hub_project_open
    && !render.backgroundInert && !render.sessionChanged
    && !previous.confirmation_visible && !render.localModalWasOpen;
}

type ThreadContentView = Pick<DesktopViewState, "transcript_rows" | "pending_turn_inputs" | "file_change_rows">;

/** Whether the next projection adds conversation rows, queued inputs or file changes. */
export function threadContentAdvanced(previous: ThreadContentView | null, next: ThreadContentView): boolean {
  return next.transcript_rows.length > (previous?.transcript_rows.length ?? 0)
    || next.pending_turn_inputs.length > (previous?.pending_turn_inputs.length ?? 0)
    || next.file_change_rows.length > (previous?.file_change_rows.length ?? 0);
}

export type ThreadViewportPlan = "session-interaction" | "reveal-end" | "keep-scroll-top" | "none";

/**
 * How the thread viewport is restored after the update, once a prepended history anchor did
 * not apply. A remembered per-session position wins, then following the tail, then keeping the
 * previous offset.
 */
export function planThreadViewport(input: {
  threadPresent: boolean;
  rememberedSessionInteraction: boolean;
  revealEnd: boolean;
  previousThreadPresent: boolean;
}): ThreadViewportPlan {
  if (!input.threadPresent) return "none";
  if (input.rememberedSessionInteraction) return "session-interaction";
  if (input.revealEnd) return "reveal-end";
  return input.previousThreadPresent ? "keep-scroll-top" : "none";
}

/**
 * The scroll offset to restore after layout. A thread that was near its end keeps the same
 * distance from the end, so a reflow alone does not move the reader into older history.
 */
export function threadScrollTopAfterLayout(input: {
  previousScrollTop: number;
  previousWasNearEnd: boolean;
  sameSessionOwner: boolean;
  previousEndGap: number;
  scrollHeight: number;
  clientHeight: number;
}): number {
  return input.previousWasNearEnd && input.sameSessionOwner
    ? Math.max(0, input.scrollHeight - input.clientHeight - input.previousEndGap)
    : input.previousScrollTop;
}

const NAVIGATION_ROW_ACTIONS = ["project", "session", "chat-session"];

/**
 * Whether the focused sidebar row is the one just selected, so the composer may take focus
 * once navigation has settled.
 */
export function selectedNavigationRowYieldsFocus(input: {
  focusPromptAfterRender: boolean;
  navigationLoading: boolean;
  activeAction: string | undefined;
  activeFocusKey: string | undefined;
  selectedProjectId: string | null;
  selectedSessionId: string | null;
}): boolean {
  const selectedId = input.activeAction === "project" ? input.selectedProjectId : input.selectedSessionId;
  return input.focusPromptAfterRender && !input.navigationLoading && Boolean(selectedId)
    && NAVIGATION_ROW_ACTIONS.includes(input.activeAction ?? "")
    && input.activeFocusKey === `${input.activeAction}:${selectedId}:select`;
}

/**
 * Whether the composer receives focus as the last resort after a modal or titlebar menu
 * closes. Any more specific focus owner, or a still open dialog, takes precedence.
 */
export function composerFallbackFocusWanted(input: {
  modalClosing: boolean;
  titlebarMenuFocusAction: string | null;
  titlebarMenuFocusReserved: boolean;
  initiatingTriggerYieldsPromptFocus: boolean;
  newSessionFocusRejected: boolean;
  focusSnapshotReserved: boolean;
  quickChatOwnsModalCloseFallback: boolean;
  confirmationVisible: boolean;
  localModalPending: boolean;
  overlay: DesktopViewState["overlay"];
}): boolean {
  return (input.modalClosing || (input.titlebarMenuFocusAction !== null && !input.titlebarMenuFocusReserved))
    && !input.initiatingTriggerYieldsPromptFocus
    && !input.newSessionFocusRejected
    && !input.focusSnapshotReserved
    && !input.quickChatOwnsModalCloseFallback
    && !input.confirmationVisible
    && !input.localModalPending
    && input.overlay === "none";
}

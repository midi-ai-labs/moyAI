import type { FocusTargetCandidate } from "./focus_arbiter.ts";
import type { TranscriptRow } from "./types.ts";

export interface TranscriptAnchor {
  id: string;
  detailsId: string;
  label: string;
  preview: string;
  row: TranscriptRow;
}

export interface TranscriptAnchorOptions {
  stableLatestAssistant?: boolean;
}

export interface ViewportAnchorCandidate {
  id: string;
  offsetTop: number;
}

export interface ViewportAnchorSnapshot {
  candidates: ReadonlyArray<ViewportAnchorCandidate>;
  scrollTop: number;
  scrollHeight: number;
}

export interface ThreadEndRevealInput {
  sessionChanged: boolean;
  runStartRequested: boolean;
  previouslyNearEnd: boolean;
  updateWantsEnd: boolean;
}

export interface RunCompletionSample {
  busy: boolean;
  terminal: boolean;
}

export interface ThreadTailFollowOwner {
  workspacePath: string;
  sessionId: string | null;
  runtimeOwnerToken: string;
}

export interface ThreadTailFollowProjection extends ThreadTailFollowOwner {
  runActive: boolean;
  terminal: boolean;
}

export interface ThreadTailFollowDecision {
  follow: boolean;
  clearAfterPin: boolean;
}

interface ActiveThreadTailFollow {
  workspacePath: string;
  sessionId: string | null;
  initialRuntimeEpoch: string | null;
  runEpoch: string | null;
  observedRunOwner: boolean;
}

interface RuntimeOwnerIdentity {
  kind: "idle" | "root" | "tree";
  epoch: string;
}

export interface HistoryPrependProjection {
  workspace_path: string;
  selected_session_index: number;
  session_rows: ReadonlyArray<{ session_id: string }>;
  turn_page_offset: number;
  pending_async_operations: ReadonlyArray<string>;
}

export interface PendingHistoryPrepend {
  generation: number;
  ownerIdentity: string;
  startingOffset: number;
  commandAccepted: boolean;
  returnFocusRequested: boolean;
}

export type HistoryPrependDisposition = "none" | "wait" | "consume" | "discard";

export type HistoryPrependFocusPhase = "loading" | "settled";

export interface HistoryPrependTransition {
  pending: PendingHistoryPrepend | null;
  disposition: HistoryPrependDisposition;
  focusContinuation: PendingHistoryPrepend | null;
  focusPhase: HistoryPrependFocusPhase | null;
}

export type HistoryPrependFocusResult =
  | "focused-trigger"
  | "focused-thread"
  | "already-focused"
  | "not-requested"
  | "owned"
  | "unavailable";

export function turnPageLoadPending(
  state: { pending_async_operations?: ReadonlyArray<string> },
): boolean {
  return state.pending_async_operations?.includes("turn_page_load") ?? false;
}

export function shouldRevealThreadEnd(input: ThreadEndRevealInput): boolean {
  return input.sessionChanged
    || input.runStartRequested
    || (input.previouslyNearEnd && input.updateWantsEnd);
}

export function runCompletionEdge(
  previous: RunCompletionSample,
  current: RunCompletionSample,
): boolean {
  return (previous.busy && !current.busy)
    || (!previous.terminal && current.terminal);
}

export function pinThreadToEnd(
  thread: Pick<HTMLElement, "scrollTop" | "scrollHeight">,
): void {
  thread.scrollTop = thread.scrollHeight;
}

export function pinResolvedThreadToEnd(
  resolveThread: () => Pick<HTMLElement, "scrollTop" | "scrollHeight"> | null,
): boolean {
  const thread = resolveThread();
  if (!thread) return false;
  pinThreadToEnd(thread);
  return true;
}

export class ThreadTailFollowAffinity {
  private viewportNearEnd = true;
  private activeRun: ActiveThreadTailFollow | null = null;

  get followingRun(): boolean {
    return this.activeRun !== null;
  }

  get viewportIsNearEnd(): boolean {
    return this.viewportNearEnd;
  }

  armRun(owner: ThreadTailFollowOwner): boolean {
    if (!this.viewportNearEnd) return false;
    const runtimeOwner = parseRuntimeOwner(owner.runtimeOwnerToken);
    this.activeRun = {
      workspacePath: owner.workspacePath,
      sessionId: owner.sessionId,
      initialRuntimeEpoch: runtimeOwner?.epoch ?? null,
      runEpoch: runtimeOwner?.kind === "root" || runtimeOwner?.kind === "tree"
        ? runtimeOwner.epoch
        : null,
      observedRunOwner: runtimeOwner?.kind === "root" || runtimeOwner?.kind === "tree",
    };
    return true;
  }

  reconcile(projection: ThreadTailFollowProjection): ThreadTailFollowDecision {
    const activeRun = this.activeRun;
    if (!activeRun) return noTailFollow();
    if (activeRun.workspacePath !== projection.workspacePath) {
      this.activeRun = null;
      return noTailFollow();
    }
    if (activeRun.sessionId === null && projection.sessionId !== null) {
      activeRun.sessionId = projection.sessionId;
    } else if (
      activeRun.sessionId !== null
      && projection.sessionId !== activeRun.sessionId
    ) {
      this.activeRun = null;
      return noTailFollow();
    }

    const runtimeOwner = parseRuntimeOwner(projection.runtimeOwnerToken);
    if (!runtimeOwner) {
      this.activeRun = null;
      return noTailFollow();
    }
    if (runtimeOwner.kind === "root" || runtimeOwner.kind === "tree") {
      if (activeRun.runEpoch !== null && activeRun.runEpoch !== runtimeOwner.epoch) {
        this.activeRun = null;
        return noTailFollow();
      }
      activeRun.runEpoch = runtimeOwner.epoch;
      activeRun.observedRunOwner = true;
    } else if (activeRun.runEpoch !== null) {
      if (activeRun.runEpoch !== runtimeOwner.epoch) {
        this.activeRun = null;
        return noTailFollow();
      }
    } else if (
      activeRun.initialRuntimeEpoch !== null
      && activeRun.initialRuntimeEpoch !== runtimeOwner.epoch
    ) {
      // A very short run can settle before a root:<generation> projection is observed.
      activeRun.runEpoch = runtimeOwner.epoch;
      activeRun.observedRunOwner = true;
    }

    return {
      follow: true,
      clearAfterPin: activeRun.observedRunOwner
        && !projection.runActive
        && projection.terminal
        && runtimeOwner.kind === "idle",
    };
  }

  completeRender(decision: ThreadTailFollowDecision, pinnedToEnd: boolean): void {
    if (!pinnedToEnd) return;
    this.viewportNearEnd = true;
    if (decision.clearAfterPin) this.activeRun = null;
  }

  noteUserViewport(nearEnd: boolean): void {
    this.viewportNearEnd = nearEnd;
    if (!nearEnd) this.activeRun = null;
  }

  noteUserScrollAway(): void {
    this.noteUserViewport(false);
  }

  syncInactiveViewport(nearEnd: boolean): boolean {
    if (this.activeRun) return false;
    this.viewportNearEnd = nearEnd;
    return true;
  }

  cancelRun(): void {
    this.activeRun = null;
  }
}

export function syncResolvedInactiveThreadViewport<T>(
  affinity: ThreadTailFollowAffinity,
  resolveThread: () => T | null,
  isNearEnd: (thread: T) => boolean,
): boolean {
  const thread = resolveThread();
  if (!thread) return false;
  return affinity.syncInactiveViewport(isNearEnd(thread));
}

function noTailFollow(): ThreadTailFollowDecision {
  return { follow: false, clearAfterPin: false };
}

function parseRuntimeOwner(token: string): RuntimeOwnerIdentity | null {
  const match = /^(idle|root|tree):(.+)$/.exec(token);
  if (!match) return null;
  return {
    kind: match[1] as RuntimeOwnerIdentity["kind"],
    epoch: match[2]!,
  };
}

export function transcriptAnchors(
  rows: readonly TranscriptRow[],
  options: TranscriptAnchorOptions = {},
): TranscriptAnchor[] {
  let stableAssistantIndex = -1;
  if (options.stableLatestAssistant) {
    let currentTurnBoundary = -1;
    for (let index = rows.length - 1; index >= 0; index -= 1) {
      const kind = rows[index]?.row_kind;
      if (kind === "user" || kind === "work_summary_running" || kind === "work_summary_incomplete") {
        currentTurnBoundary = index;
        break;
      }
    }
    for (let index = rows.length - 1; index >= 0; index -= 1) {
      if (index <= currentTurnBoundary || rows[index]?.row_kind !== "assistant") continue;
      stableAssistantIndex = index;
      break;
    }
  }
  const baseIds = rows.map((row, index) => `history-${stableHash(
    index === stableAssistantIndex ? "assistant\u0000latest" : transcriptIdentity(row),
  )}`);
  const detailsBaseIds = rows.map((row) => `history-detail-${stableHash(transcriptDetailsIdentity(row))}`);
  const totals = new Map<string, number>();
  for (const baseId of baseIds) totals.set(baseId, (totals.get(baseId) ?? 0) + 1);
  const detailsTotals = new Map<string, number>();
  for (const baseId of detailsBaseIds) detailsTotals.set(baseId, (detailsTotals.get(baseId) ?? 0) + 1);
  const seen = new Map<string, number>();
  const detailsSeen = new Map<string, number>();
  return rows.map((row, index) => {
    const baseId = baseIds[index] ?? "history";
    const occurrence = (seen.get(baseId) ?? 0) + 1;
    seen.set(baseId, occurrence);
    // Count identical projected rows from the newest end. Prepending an older
    // bounded chunk therefore keeps every already-visible suffix anchor stable.
    const reverseOccurrence = (totals.get(baseId) ?? occurrence) - occurrence + 1;
    const detailsBaseId = detailsBaseIds[index] ?? "history-detail";
    const detailsOccurrence = (detailsSeen.get(detailsBaseId) ?? 0) + 1;
    detailsSeen.set(detailsBaseId, detailsOccurrence);
    const detailsReverseOccurrence = (detailsTotals.get(detailsBaseId) ?? detailsOccurrence)
      - detailsOccurrence
      + 1;
    return {
      id: `${baseId}-r${reverseOccurrence}`,
      detailsId: `${detailsBaseId}-r${detailsReverseOccurrence}`,
      label: transcriptAnchorLabel(row),
      preview: transcriptAnchorPreview(row),
      row,
    };
  });
}

export function captureViewportAnchor(thread: HTMLElement): ViewportAnchorSnapshot | null {
  const threadBounds = thread.getBoundingClientRect();
  const rows = Array.from(thread.querySelectorAll<HTMLElement>("[data-history-anchor]"))
    .map((candidate) => ({ candidate, bounds: candidate.getBoundingClientRect() }));
  const visibleRows = rows.filter(({ bounds }) => (
    bounds.bottom > threadBounds.top + 8 && bounds.top < threadBounds.bottom - 8
  ));
  const selectedRows = visibleRows.length > 0
    ? visibleRows
    : rows.filter(({ bounds }) => bounds.bottom > threadBounds.top + 8).slice(0, 1);
  const candidates = selectedRows.flatMap(({ candidate, bounds }) => {
    const id = candidate.dataset.historyAnchor;
    return id ? [{ id, offsetTop: bounds.top - threadBounds.top }] : [];
  });
  if (candidates.length === 0) return null;
  return {
    candidates,
    scrollTop: thread.scrollTop,
    scrollHeight: thread.scrollHeight,
  };
}

export function restoreViewportAnchor(thread: HTMLElement, snapshot: ViewportAnchorSnapshot): boolean {
  const rows = Array.from(thread.querySelectorAll<HTMLElement>("[data-history-anchor]"));
  const threadTop = thread.getBoundingClientRect().top;
  for (const candidate of snapshot.candidates) {
    const target = rows.find((row) => row.dataset.historyAnchor === candidate.id);
    if (!target) continue;
    const currentOffset = target.getBoundingClientRect().top - threadTop;
    thread.scrollTop += currentOffset - candidate.offsetTop;
    return true;
  }
  const heightDelta = thread.scrollHeight - snapshot.scrollHeight;
  thread.scrollTop = Math.max(0, snapshot.scrollTop + heightDelta);
  return true;
}

export function createPendingHistoryPrepend(
  state: HistoryPrependProjection,
  generation: number,
  documentTarget?: Document,
): PendingHistoryPrepend | null {
  const ownerIdentity = historyPrependOwnerIdentity(state);
  if (!ownerIdentity || !(state.turn_page_offset > 0)) return null;
  const trigger = documentTarget ? historyPrependTrigger(documentTarget) : null;
  return {
    generation,
    ownerIdentity,
    startingOffset: state.turn_page_offset,
    commandAccepted: false,
    returnFocusRequested: trigger !== null && documentTarget?.activeElement === trigger,
  };
}

export function acknowledgePendingHistoryPrepend(
  pending: PendingHistoryPrepend,
): PendingHistoryPrepend {
  return pending.commandAccepted ? pending : { ...pending, commandAccepted: true };
}

export function rejectPendingHistoryPrepend(
  pending: PendingHistoryPrepend | null,
  generation: number,
): PendingHistoryPrepend | null {
  return pending?.generation === generation ? null : pending;
}

export function advancePendingHistoryPrepend(
  pending: PendingHistoryPrepend | null,
  state: HistoryPrependProjection,
): HistoryPrependTransition {
  if (!pending) return emptyHistoryPrependTransition("none");
  if (historyPrependOwnerIdentity(state) !== pending.ownerIdentity) {
    return emptyHistoryPrependTransition("discard");
  }
  if (state.turn_page_offset < pending.startingOffset) {
    return {
      pending: null,
      disposition: "consume",
      focusContinuation: pending.returnFocusRequested ? pending : null,
      focusPhase: pending.returnFocusRequested ? "settled" : null,
    };
  }
  if (state.turn_page_offset > pending.startingOffset) {
    return emptyHistoryPrependTransition("discard");
  }
  if (
    !pending.commandAccepted
    || turnPageLoadPending(state)
  ) {
    return {
      pending,
      disposition: "wait",
      focusContinuation: pending.returnFocusRequested ? pending : null,
      focusPhase: pending.returnFocusRequested ? "loading" : null,
    };
  }
  return emptyHistoryPrependTransition("discard");
}

export function historyPrependOwnerIdentity(state: HistoryPrependProjection): string | null {
  const sessionId = state.session_rows[state.selected_session_index]?.session_id;
  return sessionId ? `${state.workspace_path}\u0000${sessionId}` : null;
}

export function historyPrependFocusContinuationIsCurrent(
  continuation: PendingHistoryPrepend,
  state: HistoryPrependProjection,
  pending: PendingHistoryPrepend | null,
  latestGeneration: number,
  phase: HistoryPrependFocusPhase,
): boolean {
  if (
    !continuation.returnFocusRequested
    || continuation.generation !== latestGeneration
    || historyPrependOwnerIdentity(state) !== continuation.ownerIdentity
  ) {
    return false;
  }
  if (phase === "loading") {
    return pending?.generation === continuation.generation
      && state.turn_page_offset === continuation.startingOffset;
  }
  return pending === null && state.turn_page_offset < continuation.startingOffset;
}

/**
 * Resolves the exact post-render focus fallback chain without moving focus.
 * Eligibility such as disabled/hidden/inert is intentionally left to the shared arbiter.
 */
export function historyPrependFocusCandidates(
  documentTarget: Document,
  continuation: PendingHistoryPrepend,
): readonly FocusTargetCandidate[] {
  if (!continuation.returnFocusRequested) return [];
  return [
    {
      resolve: () => {
        const thread = historyPrependThread(documentTarget);
        const trigger = historyPrependTrigger(documentTarget);
        return thread && trigger && elementFullyVisibleWithin(trigger, thread) ? trigger : null;
      },
    },
    { resolve: () => historyPrependThread(documentTarget) },
  ];
}

export function focusHistoryPrependReturnIfUnowned(
  documentTarget: Document,
  continuation: PendingHistoryPrepend,
): HistoryPrependFocusResult {
  if (!continuation.returnFocusRequested) return "not-requested";
  const thread = historyPrependThread(documentTarget);
  if (!thread || elementUnavailable(thread)) return "unavailable";
  const target = historyPrependFocusCandidates(documentTarget, continuation)
    .map((candidate) => candidate.resolve() as HTMLElement | null)
    .find((candidate) => candidate !== null && !elementUnavailable(candidate)) ?? null;
  if (!target) return "unavailable";
  const trigger = historyPrependTrigger(documentTarget);
  const active = documentTarget.activeElement;
  if (active === target) return "already-focused";
  if (
    active !== null
    && active !== documentTarget.body
    && active !== documentTarget.documentElement
    && !(active === thread && target === trigger)
    && !(active === trigger && target === thread)
  ) {
    return "owned";
  }
  target.focus({ preventScroll: true });
  if (documentTarget.activeElement !== target) return "unavailable";
  return target === trigger ? "focused-trigger" : "focused-thread";
}

function historyPrependThread(documentTarget: Document): HTMLElement | null {
  return documentTarget.querySelector<HTMLElement>("#thread");
}

function historyPrependTrigger(documentTarget: Document): HTMLElement | null {
  return historyPrependThread(documentTarget)?.querySelector<HTMLElement>(
    '[data-focus-key="load-previous-turn-page"]',
  ) ?? null;
}

function elementUnavailable(element: HTMLElement): boolean {
  return element.matches(":disabled")
    || element.getAttribute("aria-disabled") === "true"
    || element.hidden
    || element.closest("[inert], [aria-hidden='true']") !== null;
}

function elementFullyVisibleWithin(element: HTMLElement, owner: HTMLElement): boolean {
  const bounds = element.getBoundingClientRect();
  const ownerBounds = owner.getBoundingClientRect();
  return bounds.width > 0
    && bounds.height > 0
    && bounds.left >= ownerBounds.left
    && bounds.top >= ownerBounds.top
    && bounds.right <= ownerBounds.right
    && bounds.bottom <= ownerBounds.bottom;
}

function emptyHistoryPrependTransition(
  disposition: Extract<HistoryPrependDisposition, "none" | "discard">,
): HistoryPrependTransition {
  return {
    pending: null,
    disposition,
    focusContinuation: null,
    focusPhase: null,
  };
}

function transcriptIdentity(row: TranscriptRow): string {
  const stableIdentity = row.stable_history_identity?.trim();
  if (stableIdentity) return stableIdentity;
  // Polling changes both the elapsed label and the live work body. Keep the
  // rail/focus owner stable for the running phase just like its disclosure.
  if (row.row_kind === "work_summary_running" || row.row_kind === "work_summary_incomplete") {
    return row.row_kind;
  }
  const files = row.file_changes
    .map((change) => `${change.action}\u0000${change.path}\u0000${change.summary}`)
    .join("\u0001");
  return `${row.row_kind}\u0000${row.title}\u0000${row.body}\u0000${files}`;
}

function transcriptDetailsIdentity(row: TranscriptRow): string {
  const stableIdentity = row.stable_history_identity?.trim();
  if (stableIdentity) {
    // The rail/focus owner stays bound to the durable turn while its running
    // WorkSummary becomes terminal. The disclosure owner is phase-scoped so
    // an automatically-open running card renders with the terminal default
    // (collapsed), while a user-opened terminal card still survives later
    // title/body refreshes and history prepends.
    if (row.row_kind.startsWith("work_summary")) {
      return `${stableIdentity}\u0000${row.row_kind}`;
    }
    return stableIdentity;
  }
  // Work-summary titles can contain a live elapsed-time label. Keep the
  // disclosure owner stable while that visible label and body are refreshed;
  // the reverse occurrence still distinguishes multiple summaries.
  if (row.row_kind === "work_summary_running" || row.row_kind === "work_summary_incomplete") {
    return row.row_kind;
  }
  if (row.row_kind.startsWith("work_summary")) return transcriptIdentity(row);
  return `${row.row_kind}\u0000${row.title}`;
}

function transcriptAnchorLabel(row: TranscriptRow): string {
  if (row.row_kind === "user") return "依頼";
  if (row.row_kind === "assistant") return "応答";
  if (row.row_kind.startsWith("work_summary")) return "作業履歴";
  return row.title.trim() || "履歴";
}

function transcriptAnchorPreview(row: TranscriptRow): string {
  if (row.row_kind.startsWith("work_summary")) return row.title.trim() || "作業履歴";
  const plain = row.body
    .replace(/```[\s\S]*?```/g, " コード ")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/!?(?:\[([^\]]*)\])\([^)]*\)/g, "$1")
    .replace(/^#{1,6}\s+/gm, "")
    .replace(/^[-*+]\s+/gm, "")
    .replace(/\s+/g, " ")
    .trim();
  if (plain.length <= 84) return plain || transcriptAnchorLabel(row);
  return `${plain.slice(0, 81).trimEnd()}…`;
}

function stableHash(value: string): string {
  let hash = 2_166_136_261;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 16_777_619) >>> 0;
  }
  return hash.toString(36);
}

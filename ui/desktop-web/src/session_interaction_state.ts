import { restoreScrollPosition, type ScrollRestorationTarget } from "./scroll_state.ts";
import type { SessionInteractionSnapshot } from "./ui_state.ts";

interface ScrollSnapshotSource {
  scrollLeft: number;
  scrollTop: number;
}

interface PromptSnapshotSource extends ScrollSnapshotSource {
  value: string;
  selectionStart: number | null;
  selectionEnd: number | null;
}

interface PromptRestorationTarget extends PromptSnapshotSource, ScrollRestorationTarget {
  setSelectionRange(start: number, end: number): void;
}

interface FocusablePromptRestorationTarget extends PromptRestorationTarget {
  focus(options?: FocusOptions): void;
}

export interface SessionPromptInteractionRenderContext {
  owner: string;
  ownerChanged: boolean;
  durableSession: boolean;
  focusPending: boolean;
}

export function sessionSelectionRequestsComposerFocus(mutationName: string | null): boolean {
  return mutationName === "select_project"
    || mutationName === "select_session"
    || mutationName === "select_chat_session";
}

export interface ComposerFocusInitiatingTriggerContext {
  mutationName: string | null;
  activeAction: string | null;
  activeFocusKey: string | null;
  selectedProjectId: string | null;
}

export interface NewSessionFocusContinuation {
  phase: "initiating" | "admitted";
  mutationName: "new_chat" | "new_project_session";
  source: "element" | "shortcut";
  initiatingElement: object;
  interactionGeneration: bigint;
  activeAction: string | null;
  activeFocusKey: string | null;
  requestOwner: string;
  targetProjectId: string | null;
  initiatingPrompt: boolean;
  requestToken: NewSessionFocusRequestToken;
}

export interface SettledNewSessionFocusContinuation {
  owner: string;
  interactionGeneration: bigint;
  targetProjectId: string | null;
  requestToken: NewSessionFocusRequestToken;
}

export interface NewSessionFocusRequestToken {
  rejected: boolean;
}

export interface NewSessionFocusSettlement {
  mutationName: string | null;
  currentActiveElement: object | null;
  currentFocusUnclaimed: boolean;
  currentInteractionGeneration: bigint;
  selectedProjectId: string | null;
  selectedSessionIndex: number;
  currentOwner: string;
  currentSessionId: string | null;
  navigationLoading: boolean;
}

export interface NewSessionFocusDecision {
  continuation: NewSessionFocusContinuation | null;
  settled: SettledNewSessionFocusContinuation | null;
  yieldsInitiatingFocus: boolean;
  rejected: boolean;
}

export interface NewSessionRetryFocusContext {
  currentOwner: string;
  currentInteractionGeneration: bigint;
  focusUnclaimed: boolean;
  initiatingElementConnected: boolean;
  exactRouteTarget: object | null;
  promptTarget: object | null;
}

const NEW_CHAT_FOCUS_KEYS = new Set([
  "quick-chat:new-session",
  "titlebar-menu:file:new-chat",
  "palette-action:new-chat",
  "shortcut-action:new-chat",
]);

/**
 * A completed new-session mutation intentionally transfers focus from its exact initiating control
 * to the composer. Same-action controls from another surface remain meaningful focus owners and
 * are protected by the generic no-steal rule.
 */
export function initiatingTriggerYieldsToComposerFocus(
  context: ComposerFocusInitiatingTriggerContext,
): boolean {
  if (context.mutationName === "new_chat") {
    return context.selectedProjectId === null
      && context.activeAction === "new-chat"
      && context.activeFocusKey !== null
      && NEW_CHAT_FOCUS_KEYS.has(context.activeFocusKey);
  }
  return context.mutationName === "new_project_session"
    && context.activeAction === "new-project-session"
    && context.selectedProjectId !== null
    && context.activeFocusKey === `project:${context.selectedProjectId}:new-session`;
}

export function beginNewChatFocusContinuation(
  source: "element" | "shortcut",
  initiatingElement: object | null,
  interactionGeneration: bigint,
  activeAction: string | null,
  activeFocusKey: string | null,
  requestOwner: string | null,
): NewSessionFocusContinuation | null {
  if (initiatingElement === null || requestOwner === null) return null;
  if (
    source === "element"
    && (activeAction !== "new-chat" || !activeFocusKey || !NEW_CHAT_FOCUS_KEYS.has(activeFocusKey))
  ) {
    return null;
  }
  return {
    phase: "initiating",
    mutationName: "new_chat",
    source,
    initiatingElement,
    interactionGeneration,
    activeAction,
    activeFocusKey,
    requestOwner,
    targetProjectId: null,
    initiatingPrompt: elementIdOf(initiatingElement) === "prompt",
    requestToken: { rejected: false },
  };
}

export function beginNewProjectSessionFocusContinuation(
  initiatingElement: object | null,
  interactionGeneration: bigint,
  activeAction: string | null,
  activeFocusKey: string | null,
  requestOwner: string | null,
  expectedProjectId: string | null,
): NewSessionFocusContinuation | null {
  if (
    initiatingElement === null
    || requestOwner === null
    || expectedProjectId === null
    || activeAction !== "new-project-session"
    || activeFocusKey !== `project:${expectedProjectId}:new-session`
  ) {
    return null;
  }
  return {
    phase: "initiating",
    mutationName: "new_project_session",
    source: "element",
    initiatingElement,
    interactionGeneration,
    activeAction,
    activeFocusKey,
    requestOwner,
    targetProjectId: expectedProjectId,
    initiatingPrompt: false,
    requestToken: { rejected: false },
  };
}

export function reconcileNewSessionFocusContinuation(
  continuation: NewSessionFocusContinuation | null,
  settlement: NewSessionFocusSettlement,
): NewSessionFocusDecision {
  if (continuation === null) return noNewSessionFocusDecision(false);
  if (continuation.requestToken.rejected) return noNewSessionFocusDecision(true);
  if (
    continuation.interactionGeneration !== settlement.currentInteractionGeneration
    || (continuation.phase === "admitted" && settlement.mutationName !== null)
  ) {
    return noNewSessionFocusDecision(true);
  }
  if (
    continuation.phase === "initiating"
    && !initiatingRouteStillOwnsFocus(
      continuation,
      settlement.currentActiveElement,
      settlement.currentFocusUnclaimed,
    )
  ) {
    return noNewSessionFocusDecision(true);
  }
  if (canonicalNewSessionOwnerAdvanced(continuation, settlement)) {
    return settledNewSessionFocusDecision(continuation, settlement);
  }
  if (continuation.phase === "initiating") {
    if (settlement.mutationName === null) {
      return { continuation, settled: null, yieldsInitiatingFocus: false, rejected: false };
    }
    if (
      settlement.mutationName !== continuation.mutationName
    ) {
      return noNewSessionFocusDecision(true);
    }
  }
  if (settlement.navigationLoading) {
    return {
      continuation: { ...continuation, phase: "admitted" },
      settled: null,
      yieldsInitiatingFocus: continuation.phase === "initiating",
      rejected: false,
    };
  }
  return noNewSessionFocusDecision(true);
}

function canonicalNewSessionOwnerAdvanced(
  continuation: NewSessionFocusContinuation,
  settlement: NewSessionFocusSettlement,
): boolean {
  return settlement.selectedProjectId === continuation.targetProjectId
    && settlement.selectedSessionIndex === -1
    && settlement.currentSessionId === null
    && continuation.requestOwner !== settlement.currentOwner;
}

function settledNewSessionFocusDecision(
  continuation: NewSessionFocusContinuation,
  settlement: NewSessionFocusSettlement,
): NewSessionFocusDecision {
  return {
    continuation: null,
    settled: {
      owner: settlement.currentOwner,
      interactionGeneration: settlement.currentInteractionGeneration,
      targetProjectId: continuation.targetProjectId,
      requestToken: continuation.requestToken,
    },
    yieldsInitiatingFocus: true,
    rejected: false,
  };
}

function initiatingRouteStillOwnsFocus(
  continuation: NewSessionFocusContinuation,
  currentActiveElement: object | null,
  currentFocusUnclaimed: boolean,
): boolean {
  if (continuation.initiatingElement === currentActiveElement) return true;
  if (currentFocusUnclaimed) return true;
  if (continuation.source === "shortcut") return false;
  return continuation.activeAction === activeActionOf(currentActiveElement)
    && continuation.activeFocusKey !== null
    && continuation.activeFocusKey === activeFocusKeyOf(currentActiveElement)
    && (
      continuation.mutationName === "new_project_session"
        ? continuation.activeFocusKey === `project:${continuation.targetProjectId}:new-session`
        : NEW_CHAT_FOCUS_KEYS.has(continuation.activeFocusKey)
    );
}

function activeActionOf(element: object | null): string | null {
  return element && "dataset" in element
    ? ((element as { dataset?: { action?: string } }).dataset?.action ?? null)
    : null;
}

function activeFocusKeyOf(element: object | null): string | null {
  return element && "dataset" in element
    ? ((element as { dataset?: { focusKey?: string } }).dataset?.focusKey ?? null)
    : null;
}

function elementIdOf(element: object | null): string | null {
  return element && "id" in element
    ? ((element as { id?: string }).id ?? null)
    : null;
}

function noNewSessionFocusDecision(rejected: boolean): NewSessionFocusDecision {
  return { continuation: null, settled: null, yieldsInitiatingFocus: false, rejected };
}

export function settledNewSessionFocusContinuationIsCurrent(
  continuation: SettledNewSessionFocusContinuation | null,
  currentInteractionGeneration: bigint,
  selectedProjectId: string | null,
  selectedSessionIndex: number,
  currentOwner: string,
  currentSessionId: string | null,
): boolean {
  return continuation !== null
    && !continuation.requestToken.rejected
    && continuation.interactionGeneration === currentInteractionGeneration
    && continuation.owner === currentOwner
    && selectedProjectId === continuation.targetProjectId
    && selectedSessionIndex === -1
    && currentSessionId === null;
}

export function sameNewSessionFocusRequest(
  candidate: NewSessionFocusContinuation | SettledNewSessionFocusContinuation | null,
  request: NewSessionFocusContinuation | null,
): boolean {
  return candidate !== null
    && request !== null
    && candidate.requestToken === request.requestToken;
}

export function newSessionRetryFocusTarget(
  request: NewSessionFocusContinuation | null,
  context: NewSessionRetryFocusContext,
): object | null {
  if (
    request === null
    || !request.requestToken.rejected
    || request.requestOwner !== context.currentOwner
    || request.interactionGeneration !== context.currentInteractionGeneration
    || !context.focusUnclaimed
  ) {
    return null;
  }
  if (context.initiatingElementConnected) return request.initiatingElement;
  return request.initiatingPrompt ? context.promptTarget : context.exactRouteTarget;
}

export function captureSessionInteractionSnapshot(
  thread: ScrollSnapshotSource,
  prompt: PromptSnapshotSource,
): SessionInteractionSnapshot {
  const selectionStart = prompt.selectionStart ?? prompt.value.length;
  const selectionEnd = prompt.selectionEnd ?? selectionStart;
  return {
    threadScrollLeft: thread.scrollLeft,
    threadScrollTop: thread.scrollTop,
    promptScrollLeft: prompt.scrollLeft,
    promptScrollTop: prompt.scrollTop,
    promptSelectionStart: selectionStart,
    promptSelectionEnd: selectionEnd,
  };
}

export function restoreSessionThreadInteraction(
  snapshot: SessionInteractionSnapshot,
  thread: ScrollRestorationTarget,
): void {
  restoreScrollPosition(thread, snapshot.threadScrollLeft, snapshot.threadScrollTop);
}

export function restoreSessionPromptInteraction(
  snapshot: SessionInteractionSnapshot,
  prompt: PromptRestorationTarget,
): void {
  const start = Math.min(Math.max(0, snapshot.promptSelectionStart), prompt.value.length);
  const end = Math.min(Math.max(start, snapshot.promptSelectionEnd), prompt.value.length);
  prompt.setSelectionRange(start, end);
  restoreScrollPosition(prompt, snapshot.promptScrollLeft, snapshot.promptScrollTop);
}

/**
 * Select the exact durable-session snapshot for every DOM replacement. Owner changes and pending
 * focus still determine focus behavior in main.ts; this snapshot only owns selection and scroll.
 */
export function sessionPromptInteractionForRender(
  snapshots: ReadonlyMap<string, SessionInteractionSnapshot>,
  context: SessionPromptInteractionRenderContext,
): SessionInteractionSnapshot | null {
  if (!context.durableSession) return null;
  return snapshots.get(context.owner) ?? null;
}

/** Focus may reset a detached/recreated textarea's default caret in WebView, so restore after it. */
export function focusThenRestoreSessionPromptInteraction(
  snapshot: SessionInteractionSnapshot,
  prompt: FocusablePromptRestorationTarget,
): void {
  prompt.focus({ preventScroll: true });
  restoreSessionPromptInteraction(snapshot, prompt);
}

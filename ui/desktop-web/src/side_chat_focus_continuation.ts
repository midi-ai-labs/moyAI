import type { DesktopViewState, SideChatStatus } from "./types.ts";

export type SideChatRunAction = "send" | "cancel";

export type SideChatFocusSurface =
  | "unowned"
  | "side-prompt"
  | "side-send"
  | "side-stop"
  | "other";

export interface SideChatFocusMutation {
  kind: string;
  chatId: string | null;
  generation: string;
}

export interface SideChatFocusContinuation {
  action: SideChatRunAction;
  ownerSessionId: string;
  chatId: string;
  startingGeneration: string;
  requestGeneration: string | null;
}

export interface SideChatFocusTarget {
  ownerSessionId: string;
  chatId: string;
  generation: string;
}

export interface SideChatFocusEnvironment {
  paneVisible: boolean;
  localModalOpen: boolean;
  mutation: SideChatFocusMutation | null;
}

export interface SideChatFocusDecision {
  continuation: SideChatFocusContinuation | null;
  focusTarget: SideChatFocusTarget | null;
}

export interface SideChatFocusInteractionState {
  sideChatFocusContinuation: SideChatFocusContinuation | null;
  sideChatFocusInteractionGeneration: bigint;
}

type SideChatFocusState = Pick<
  DesktopViewState,
  | "confirmation_visible"
  | "draft_target"
  | "navigation_loading"
  | "overlay"
  | "side_chat"
>;

export function sideChatFocusSurface(documentTarget: Document): SideChatFocusSurface {
  const active = documentTarget.activeElement;
  if (
    !active
    || active === documentTarget.body
    || active === documentTarget.documentElement
  ) {
    return "unowned";
  }
  if (active.closest(".modal, [role='dialog'], [role='alertdialog']")) return "other";
  if (active.matches("#side-chat-prompt")) return "side-prompt";
  const action = active.closest<HTMLElement>("[data-action]")?.dataset.action;
  if (action === "send-side-chat") return "side-send";
  if (action === "cancel-side-chat") return "side-stop";
  return "other";
}

export function pointerTargetsSideChatRunControl(target: Element): boolean {
  const action = target.closest<HTMLElement>("[data-action]")?.dataset.action;
  return action === "send-side-chat" || action === "cancel-side-chat";
}

export function invalidateSideChatFocusInteraction(
  state: SideChatFocusInteractionState,
): void {
  state.sideChatFocusInteractionGeneration += 1n;
  state.sideChatFocusContinuation = null;
}

export function beginSideChatFocusContinuation(
  state: SideChatFocusState,
  action: string,
): SideChatFocusContinuation | null {
  const target = exactSideChatTarget(state);
  if (!target || !state.side_chat.configured || state.side_chat.deleting) return null;
  if (action === "send-side-chat" && state.side_chat.can_send) {
    return {
      action: "send",
      ...target,
      startingGeneration: state.side_chat.generation,
      requestGeneration: null,
    };
  }
  if (action === "cancel-side-chat" && state.side_chat.can_cancel) {
    return {
      action: "cancel",
      ...target,
      startingGeneration: state.side_chat.generation,
      requestGeneration: state.side_chat.generation,
    };
  }
  return null;
}

export function reconcileSideChatFocusContinuation(
  current: SideChatFocusContinuation | null,
  _previous: SideChatFocusState | null,
  next: SideChatFocusState,
  focusedSurface: SideChatFocusSurface,
  environment: SideChatFocusEnvironment,
): SideChatFocusDecision {
  let continuation = current;
  if (focusedSurface === "side-prompt" || focusedSurface === "other") {
    continuation = null;
  } else if (focusedSurface === "side-send") {
    continuation = continuation?.action === "send" ? continuation : null;
  } else if (focusedSurface === "side-stop") {
    continuation = continuation?.action === "cancel" ? continuation : null;
  }

  if (!continuation) return noSideChatFocus();
  if (!focusEnvironmentAllowsContinuation(next, environment)) return noSideChatFocus();
  if (!sameExactTarget(continuation, next)) return noSideChatFocus();
  if (!mutationBelongsToContinuation(continuation, environment.mutation)) {
    return noSideChatFocus();
  }

  const startingGeneration = parseGeneration(continuation.startingGeneration);
  const nextGeneration = parseGeneration(next.side_chat.generation);
  if (startingGeneration === null || nextGeneration === null) return noSideChatFocus();

  if (continuation.action === "send" && continuation.requestGeneration === null) {
    if (nextGeneration === startingGeneration + 1n) {
      continuation = {
        ...continuation,
        requestGeneration: next.side_chat.generation,
      };
    } else if (nextGeneration !== startingGeneration) {
      return noSideChatFocus();
    } else if (environment.mutation === null) {
      return retryFocusDecision(continuation, next);
    }
  }

  const requestGeneration = continuation.requestGeneration === null
    ? null
    : parseGeneration(continuation.requestGeneration);
  if (requestGeneration !== null && nextGeneration !== requestGeneration) {
    return noSideChatFocus();
  }
  if (requestGeneration === null || !terminalStatus(next.side_chat.status)) {
    return { continuation, focusTarget: null };
  }
  if (environment.mutation !== null) {
    return { continuation, focusTarget: null };
  }
  if (!next.side_chat.can_send || next.side_chat.can_cancel) {
    return { continuation, focusTarget: null };
  }
  return {
    continuation: null,
    focusTarget: targetFor(continuation, next.side_chat.generation),
  };
}

export function sideChatFocusTargetStillMatches(
  target: SideChatFocusTarget,
  state: SideChatFocusState,
): boolean {
  const current = exactSideChatTarget(state);
  return current !== null
    && current.ownerSessionId === target.ownerSessionId
    && current.chatId === target.chatId
    && state.side_chat.generation === target.generation
    && state.side_chat.configured
    && !state.side_chat.deleting
    && state.side_chat.can_send
    && !state.side_chat.can_cancel
    && terminalOrIdleStatus(state.side_chat.status)
    && state.overlay === "none"
    && !state.confirmation_visible
    && !state.navigation_loading;
}

function retryFocusDecision(
  continuation: SideChatFocusContinuation,
  state: SideChatFocusState,
): SideChatFocusDecision {
  if (
    !state.side_chat.can_send
    || state.side_chat.can_cancel
    || !terminalOrIdleStatus(state.side_chat.status)
  ) {
    return noSideChatFocus();
  }
  return {
    continuation: null,
    focusTarget: targetFor(continuation, state.side_chat.generation),
  };
}

function focusEnvironmentAllowsContinuation(
  state: SideChatFocusState,
  environment: SideChatFocusEnvironment,
): boolean {
  return environment.paneVisible
    && !environment.localModalOpen
    && state.overlay === "none"
    && !state.confirmation_visible
    && !state.navigation_loading
    && state.side_chat.configured
    && !state.side_chat.deleting;
}

function mutationBelongsToContinuation(
  continuation: SideChatFocusContinuation,
  mutation: SideChatFocusMutation | null,
): boolean {
  if (mutation === null) return true;
  return mutation.kind === continuation.action
    && mutation.chatId === continuation.chatId
    && mutation.generation === continuation.startingGeneration;
}

function exactSideChatTarget(state: SideChatFocusState): {
  ownerSessionId: string;
  chatId: string;
} | null {
  const selectedOwner = state.draft_target.sessionId;
  const projectedOwner = state.side_chat.owner_session_id;
  if (
    selectedOwner === null
    || (projectedOwner !== null && projectedOwner !== selectedOwner)
    || state.side_chat.chat_id === null
  ) {
    return null;
  }
  return {
    ownerSessionId: projectedOwner ?? selectedOwner,
    chatId: state.side_chat.chat_id,
  };
}

function sameExactTarget(
  continuation: SideChatFocusContinuation,
  state: SideChatFocusState,
): boolean {
  const target = exactSideChatTarget(state);
  return target !== null
    && target.ownerSessionId === continuation.ownerSessionId
    && target.chatId === continuation.chatId;
}

function targetFor(
  continuation: SideChatFocusContinuation,
  generation: string,
): SideChatFocusTarget {
  return {
    ownerSessionId: continuation.ownerSessionId,
    chatId: continuation.chatId,
    generation,
  };
}

function terminalStatus(status: SideChatStatus): boolean {
  return status === "completed" || status === "failed" || status === "cancelled";
}

function terminalOrIdleStatus(status: SideChatStatus): boolean {
  return status === "idle" || terminalStatus(status);
}

function parseGeneration(value: string): bigint | null {
  return /^(0|[1-9][0-9]*)$/.test(value) ? BigInt(value) : null;
}

function noSideChatFocus(): SideChatFocusDecision {
  return { continuation: null, focusTarget: null };
}

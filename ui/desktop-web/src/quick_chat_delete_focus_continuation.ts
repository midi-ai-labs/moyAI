import type { FocusTargetCandidate } from "./focus_arbiter.ts";
import type { DesktopViewState, RowMutationTarget } from "./types.ts";

type QuickChatDeleteFocusState = Pick<
  DesktopViewState,
  | "chat_session_rows"
  | "confirmation_visible"
  | "navigation_loading"
  | "overlay"
  | "project_rows"
  | "selected_project_index"
  | "selected_session_index"
  | "session_rows"
  | "workspace_path"
>;

interface QuickChatOwner {
  projectId: string | null;
  sessionId: string | null;
}

export interface QuickChatDeleteFocusContinuation {
  workspacePath: string;
  deletedSessionId: string;
  initialOwner: QuickChatOwner;
  settledOwner: QuickChatOwner;
  fallbackSessionId: string | null;
  abandoned: boolean;
}

export type QuickChatDeleteFocusTarget =
  | { kind: "chat-row"; sessionId: string }
  | { kind: "prompt" };

export interface QuickChatDeleteFocusDecision {
  continuation: QuickChatDeleteFocusContinuation | null;
  focusTarget: QuickChatDeleteFocusTarget | null;
  ownsModalCloseFallback: boolean;
}

export function beginQuickChatDeleteFocusContinuation(
  state: QuickChatDeleteFocusState,
  index: number,
  expectedTarget: RowMutationTarget,
): QuickChatDeleteFocusContinuation | null {
  const row = state.chat_session_rows[index];
  const initialOwner = quickChatOwner(state);
  if (
    !row
    || row.session_id !== expectedTarget.rowId
    || state.workspace_path !== expectedTarget.workspacePath
    || initialOwner.projectId !== expectedTarget.ownerProjectId
    || initialOwner.sessionId !== expectedTarget.ownerSessionId
  ) {
    return null;
  }

  const fallbackSessionId = state.chat_session_rows[index + 1]?.session_id
    ?? state.chat_session_rows[index - 1]?.session_id
    ?? null;
  const deletingSelectedQuickChat = initialOwner.projectId === null
    && initialOwner.sessionId === row.session_id;
  return {
    workspacePath: state.workspace_path,
    deletedSessionId: row.session_id,
    initialOwner,
    settledOwner: deletingSelectedQuickChat
      ? { projectId: null, sessionId: fallbackSessionId }
      : initialOwner,
    fallbackSessionId,
    abandoned: false,
  };
}

export function abandonQuickChatDeleteFocusContinuation(
  current: QuickChatDeleteFocusContinuation | null,
): QuickChatDeleteFocusContinuation | null {
  return current && !current.abandoned ? { ...current, abandoned: true } : current;
}

export function reconcileQuickChatDeleteFocusContinuation(
  current: QuickChatDeleteFocusContinuation | null,
  state: QuickChatDeleteFocusState,
  localModalOpen: boolean,
): QuickChatDeleteFocusDecision {
  if (!current) {
    return { continuation: null, focusTarget: null, ownsModalCloseFallback: false };
  }
  if (state.workspace_path !== current.workspacePath) {
    return { continuation: null, focusTarget: null, ownsModalCloseFallback: true };
  }

  const targetStillExists = state.chat_session_rows.some(
    (row) => row.session_id === current.deletedSessionId,
  );
  if (targetStillExists) {
    if (!sameQuickChatOwner(quickChatOwner(state), current.initialOwner)) {
      return {
        continuation: localModalOpen ? { ...current, abandoned: true } : null,
        focusTarget: null,
        ownsModalCloseFallback: !localModalOpen,
      };
    }
    return {
      continuation: current,
      focusTarget: null,
      ownsModalCloseFallback: false,
    };
  }

  if (state.navigation_loading || localModalOpen) {
    return {
      continuation: current,
      focusTarget: null,
      ownsModalCloseFallback: false,
    };
  }
  if (
    current.abandoned
    || state.confirmation_visible
    || state.overlay !== "none"
    || !sameQuickChatOwner(quickChatOwner(state), current.settledOwner)
    || (
      current.fallbackSessionId !== null
      && !state.chat_session_rows.some((row) => row.session_id === current.fallbackSessionId)
    )
  ) {
    return { continuation: null, focusTarget: null, ownsModalCloseFallback: true };
  }

  return {
    continuation: current,
    focusTarget: current.fallbackSessionId === null
      ? { kind: "prompt" }
      : { kind: "chat-row", sessionId: current.fallbackSessionId },
    ownsModalCloseFallback: true,
  };
}

/** Resolve the exact post-delete target; row scrolling is a separate post-focus settlement. */
export function quickChatDeleteFocusCandidates(
  documentTarget: Document,
  target: QuickChatDeleteFocusTarget,
): readonly FocusTargetCandidate[] {
  if (target.kind === "prompt") {
    return [{ resolve: () => documentTarget.querySelector<HTMLElement>("#prompt") }];
  }
  return [{
    resolve: () => Array.from(
      documentTarget.querySelectorAll<HTMLElement>("[data-focus-key]"),
    ).find(
      (candidate) => candidate.dataset.focusKey === `chat-session:${target.sessionId}:select`,
    ) ?? null,
    settle: (focusTarget) => {
      (focusTarget as HTMLElement).scrollIntoView({ block: "nearest", inline: "nearest" });
    },
  }];
}

function quickChatOwner(state: QuickChatDeleteFocusState): QuickChatOwner {
  return {
    projectId: state.project_rows[state.selected_project_index]?.project_id ?? null,
    sessionId: state.session_rows[state.selected_session_index]?.session_id ?? null,
  };
}

function sameQuickChatOwner(left: QuickChatOwner, right: QuickChatOwner): boolean {
  return left.projectId === right.projectId && left.sessionId === right.sessionId;
}

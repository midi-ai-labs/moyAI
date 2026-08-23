import type { DesktopViewState } from "./types.ts";

export type MainRunFocusSurface =
  | "unowned"
  | "main-prompt"
  | "main-send"
  | "main-stop"
  | "other";

export interface MainRunFocusContinuation {
  workspacePath: string;
  sessionId: string | null;
  mayAdoptCreatedSession: boolean;
  startingEpoch: string;
  runEpoch: string | null;
}

export interface MainRunFocusDecision {
  continuation: MainRunFocusContinuation | null;
  focusPrompt: boolean;
}

type MainRunFocusState = Pick<
  DesktopViewState,
  | "busy"
  | "can_submit"
  | "can_cancel_run"
  | "confirmation_visible"
  | "navigation_loading"
  | "overlay"
  | "post_run_refresh_pending"
  | "run_status_key"
  | "run_target"
>;

interface RuntimeOwner {
  phase: "idle" | "root" | "tree";
  epoch: bigint;
  epochText: string;
}

export function mainRunFocusSurface(documentTarget: Document): MainRunFocusSurface {
  const active = documentTarget.activeElement;
  if (
    !active
    || active === documentTarget.body
    || active === documentTarget.documentElement
  ) {
    return "unowned";
  }
  if (active.closest(".modal, [role='dialog'], [role='alertdialog'], .side-chat-pane")) {
    return "other";
  }
  if (active.matches("#prompt")) return "main-prompt";
  const action = active.closest<HTMLElement>("[data-action]")?.dataset.action;
  if (action === "send") return "main-send";
  if (action === "cancel-run") return "main-stop";
  return "other";
}

export function pointerTargetsMainRunControl(target: Element): boolean {
  const action = target.closest<HTMLElement>("[data-action]")?.dataset.action;
  return action === "send" || action === "cancel-run";
}

export function beginMainRunFocusContinuation(
  state: MainRunFocusState,
  action: string,
): MainRunFocusContinuation | null {
  const runtime = parseRuntimeOwner(state.run_target.runtimeOwnerToken);
  if (!runtime) return null;
  if (action === "send") {
    return state.can_submit ? continuationFor(state, runtime) : null;
  }
  if (action === "cancel-run") {
    return runtime.phase === "root" && state.can_cancel_run
      ? continuationFor(state, runtime)
      : null;
  }
  return null;
}

export function reconcileMainRunFocusContinuation(
  current: MainRunFocusContinuation | null,
  _previous: MainRunFocusState | null,
  next: MainRunFocusState,
  focusedSurface: MainRunFocusSurface,
): MainRunFocusDecision {
  let continuation = current;
  if (focusedSurface === "main-prompt" || focusedSurface === "other") {
    continuation = null;
  }

  if (!continuation) return { continuation: null, focusPrompt: false };
  if (
    next.overlay !== "none"
    || next.confirmation_visible
    || next.navigation_loading
  ) {
    return { continuation: null, focusPrompt: false };
  }

  continuation = bindExactOwner(continuation, next);
  if (!continuation) return { continuation: null, focusPrompt: false };
  continuation = advanceRuntimeOwner(continuation, next);
  if (!continuation) return { continuation: null, focusPrompt: false };

  const runtime = parseRuntimeOwner(next.run_target.runtimeOwnerToken);
  if (!runtime || !focusContinuationSettled(continuation, next, runtime)) {
    return { continuation, focusPrompt: false };
  }
  return { continuation: null, focusPrompt: true };
}

function continuationFor(
  state: MainRunFocusState,
  runtime: RuntimeOwner,
): MainRunFocusContinuation {
  return {
    workspacePath: state.run_target.workspacePath,
    sessionId: state.run_target.sessionId,
    mayAdoptCreatedSession: state.run_target.sessionId === null,
    startingEpoch: runtime.epochText,
    runEpoch: runtime.phase === "root" ? runtime.epochText : null,
  };
}

function bindExactOwner(
  continuation: MainRunFocusContinuation,
  state: MainRunFocusState,
): MainRunFocusContinuation | null {
  if (state.run_target.workspacePath !== continuation.workspacePath) return null;
  const nextSessionId = state.run_target.sessionId;
  if (nextSessionId === continuation.sessionId) return continuation;
  if (
    continuation.sessionId === null
    && nextSessionId !== null
    && continuation.mayAdoptCreatedSession
  ) {
    return {
      ...continuation,
      sessionId: nextSessionId,
      mayAdoptCreatedSession: false,
    };
  }
  return null;
}

function advanceRuntimeOwner(
  continuation: MainRunFocusContinuation,
  state: MainRunFocusState,
): MainRunFocusContinuation | null {
  const runtime = parseRuntimeOwner(state.run_target.runtimeOwnerToken);
  if (!runtime) return null;
  const startingEpoch = BigInt(continuation.startingEpoch);
  const runEpoch = continuation.runEpoch === null ? null : BigInt(continuation.runEpoch);

  if (runtime.phase === "root" || runtime.phase === "tree") {
    if (runEpoch !== null) {
      return runtime.epoch === runEpoch ? continuation : null;
    }
    if (runtime.epoch === startingEpoch && state.busy) return continuation;
    if (runtime.epoch === startingEpoch + 1n) {
      return { ...continuation, runEpoch: runtime.epochText };
    }
    return null;
  }
  if (runEpoch !== null) {
    return runtime.epoch === runEpoch ? continuation : null;
  }
  if (runtime.epoch === startingEpoch) return continuation;
  if (runtime.epoch === startingEpoch + 1n) {
    return { ...continuation, runEpoch: runtime.epochText };
  }
  return null;
}

function focusContinuationSettled(
  continuation: MainRunFocusContinuation,
  state: MainRunFocusState,
  runtime: RuntimeOwner,
): boolean {
  if (
    state.busy
    || state.post_run_refresh_pending
  ) {
    return false;
  }
  const terminal = state.run_status_key === "completed"
    || state.run_status_key === "cancelled"
    || state.run_status_key === "failed";
  if (continuation.runEpoch !== null) {
    return terminal
      && runtime.phase !== "root"
      && runtime.epoch === BigInt(continuation.runEpoch);
  }
  return runtime.epoch === BigInt(continuation.startingEpoch);
}

function parseRuntimeOwner(token: string): RuntimeOwner | null {
  const match = /^(idle|root|tree):([0-9]+)$/.exec(token);
  if (!match) return null;
  return {
    phase: match[1] as RuntimeOwner["phase"],
    epoch: BigInt(match[2]),
    epochText: match[2],
  };
}

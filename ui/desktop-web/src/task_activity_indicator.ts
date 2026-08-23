import type { SessionRow, TaskActivityState } from "./types.ts";

export interface TaskActivityPresentation {
  state: Exclude<TaskActivityState, "idle">;
  label: "実行中" | "最終反映中" | "確認待ち";
}

export interface TaskActivityAnimationEpoch {
  workspacePath: string;
  sessionId: string | null;
  runtimeOwnerToken: string;
  startedAtMs: number;
  provisionalRunStart: boolean;
}

export interface TaskActivityIndicatorOptions {
  small?: boolean;
  decorative?: boolean;
}

export interface TaskActivityAnimationInput {
  activityState: TaskActivityState;
  workspacePath: string;
  sessionId: string | null;
  runtimeOwnerToken: string;
  runStartMutationPending: boolean;
  nowMs: number;
}

interface RuntimeOwner {
  kind: "root" | "tree" | "idle";
  epoch: string;
}

export function taskActivityStateForView(
  state: TaskActivityState,
  runStartMutationPending: boolean,
): TaskActivityState {
  return state === "idle" && runStartMutationPending ? "running" : state;
}

export function taskActivityStateForSessionRow(
  row: Pick<
    SessionRow,
    "loaded_status" | "pending_permission_requests" | "pending_user_input_requests"
  >,
  selectedActivityState: TaskActivityState,
): TaskActivityState {
  if (selectedActivityState !== "idle") return selectedActivityState;
  if (row.loaded_status !== "active") return "idle";
  return row.pending_permission_requests > 0 || row.pending_user_input_requests > 0
    ? "attention"
    : "running";
}

export function classifyTaskActivity(state: TaskActivityState): TaskActivityPresentation | null {
  switch (state) {
    case "running":
      return { state, label: "実行中" };
    case "finalizing":
      return { state, label: "最終反映中" };
    case "attention":
      return { state, label: "確認待ち" };
    case "idle":
      return null;
  }
}

export function renderTaskActivityIndicator(
  state: TaskActivityState,
  options: TaskActivityIndicatorOptions = {},
): string {
  const presentation = classifyTaskActivity(state);
  if (!presentation) return "";
  const accessibility = options.decorative
    ? 'aria-hidden="true"'
    : `role="img" aria-label="${presentation.label}" title="${presentation.label}"`;
  return `<span class="task-activity-indicator${options.small ? " small" : ""}" data-task-activity="${presentation.state}" ${accessibility}></span>`;
}

export function renderTaskActivityBadge(state: TaskActivityState): string {
  const presentation = classifyTaskActivity(state);
  if (!presentation) return "";
  return `<span class="task-activity-badge" data-task-activity-badge="${presentation.state}" role="status" aria-live="polite" aria-atomic="true">${renderTaskActivityIndicator(presentation.state, { decorative: true })}<strong>${presentation.label}</strong></span>`;
}

export function reconcileTaskActivityAnimationEpoch(
  previous: TaskActivityAnimationEpoch | null,
  input: TaskActivityAnimationInput,
): TaskActivityAnimationEpoch | null {
  const activityState = taskActivityStateForView(
    input.activityState,
    input.runStartMutationPending,
  );
  if (activityState === "idle") return null;

  const nextOwner = parseRuntimeOwner(input.runtimeOwnerToken);
  const nextStartedAtMs = safeNow(input.nowMs);
  const nextProvisional = input.runStartMutationPending && nextOwner?.kind === "idle";
  if (!previous || !animationOwnerContinues(previous, input, nextOwner)) {
    return {
      workspacePath: input.workspacePath,
      sessionId: input.sessionId,
      runtimeOwnerToken: input.runtimeOwnerToken,
      startedAtMs: nextStartedAtMs,
      provisionalRunStart: nextProvisional,
    };
  }

  return {
    ...previous,
    sessionId: input.sessionId,
    runtimeOwnerToken: input.runtimeOwnerToken,
    provisionalRunStart: nextProvisional,
  };
}

export function taskActivityAnimationDelay(
  epoch: TaskActivityAnimationEpoch | null,
  nowMs: number,
): string {
  if (!epoch || !Number.isFinite(nowMs)) return "0ms";
  const elapsedMs = Math.max(0, nowMs - epoch.startedAtMs);
  if (!Number.isFinite(elapsedMs) || elapsedMs === 0) return "0ms";
  return `-${elapsedMs}ms`;
}

function animationOwnerContinues(
  previous: TaskActivityAnimationEpoch,
  input: TaskActivityAnimationInput,
  nextOwner: RuntimeOwner | null,
): boolean {
  if (previous.workspacePath !== input.workspacePath) return false;
  const sessionContinues = previous.sessionId === input.sessionId
    || (previous.provisionalRunStart && previous.sessionId === null && input.sessionId !== null);
  if (!sessionContinues) return false;
  if (previous.runtimeOwnerToken === input.runtimeOwnerToken) return true;

  const previousOwner = parseRuntimeOwner(previous.runtimeOwnerToken);
  if (previous.provisionalRunStart && runtimeOwnerIsActive(nextOwner)) return true;
  return runtimeOwnerIsActive(previousOwner)
    && runtimeOwnerIsActive(nextOwner)
    && previousOwner.epoch === nextOwner.epoch;
}

function runtimeOwnerIsActive(
  owner: RuntimeOwner | null,
): owner is RuntimeOwner & { kind: "root" | "tree" } {
  return owner !== null && (owner.kind === "root" || owner.kind === "tree");
}

function parseRuntimeOwner(token: string): RuntimeOwner | null {
  const match = /^(root|tree|idle):(\d+)$/.exec(token);
  if (!match) return null;
  return {
    kind: match[1] as RuntimeOwner["kind"],
    epoch: match[2],
  };
}

function safeNow(nowMs: number): number {
  return Number.isFinite(nowMs) && nowMs >= 0 ? nowMs : 0;
}

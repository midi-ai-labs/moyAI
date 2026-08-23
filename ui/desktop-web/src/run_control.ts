import type { DesktopWebState } from "./types.ts";

export type RunControlState = Pick<DesktopWebState, "can_cancel_run" | "stop_target">;

export function runCanBeCancelled(state: RunControlState): boolean {
  return state.can_cancel_run && state.stop_target != null;
}

export function runSurfaceActive(state: Pick<DesktopWebState, "task_activity_state">): boolean {
  return state.task_activity_state !== "idle";
}

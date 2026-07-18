import type { DesktopWebState } from "./types.ts";

export type RunControlState = Pick<DesktopWebState, "can_cancel_run">;

export function runCanBeCancelled(state: RunControlState): boolean {
  return state.can_cancel_run;
}

export function runSurfaceActive(state: Pick<DesktopWebState, "busy" | "agent_tree_active">): boolean {
  return state.busy || state.agent_tree_active;
}

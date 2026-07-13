import type { DesktopWebState } from "./types.ts";

export type RunControlState = Pick<DesktopWebState, "busy" | "confirmation_visible" | "agent_tree_active">;

export function runCanBeCancelled(state: RunControlState): boolean {
  return state.busy || state.confirmation_visible || state.agent_tree_active;
}

export function runSurfaceActive(state: Pick<RunControlState, "busy" | "agent_tree_active">): boolean {
  return state.busy || state.agent_tree_active;
}

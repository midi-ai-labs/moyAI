import type { HubProjection } from "./hub_state.ts";
import type { PublishProjection } from "./mcp_publish_state.ts";

export interface AutoRefreshState {
  navigation_loading: boolean;
  confirmation_visible: boolean;
}

export function autoRefreshAllowed(state: AutoRefreshState, interactionActive: boolean): boolean {
  return state.navigation_loading || !interactionActive;
}

export function createSnapshotRefresh(readSnapshot: () => Promise<void>): (afterInFlight?: boolean) => Promise<void> {
  let inFlight: Promise<void> | null = null;
  let refreshAfterCurrent = false;
  return (afterInFlight = false) => {
    if (inFlight) {
      refreshAfterCurrent ||= afterInFlight;
      return inFlight;
    }
    inFlight = Promise.resolve().then(async () => {
      try {
        do {
          refreshAfterCurrent = false;
          await readSnapshot();
        } while (refreshAfterCurrent);
      } finally {
        inFlight = null;
      }
    });
    return inFlight;
  };
}

export function installRuntimePolling(
  windowTarget: Pick<Window, "setInterval" | "clearInterval" | "addEventListener" | "removeEventListener">,
  documentTarget: Pick<Document, "hidden" | "addEventListener" | "removeEventListener">,
  shouldPoll: () => boolean,
  refresh: (afterInFlight?: boolean) => void,
): () => void {
  const interval = windowTarget.setInterval(() => {
    if (shouldPoll()) refresh();
  }, 600);
  // Native hide/show can change capabilities after the last runtime has stopped.
  // Reuse the ordinary snapshot owner even when periodic polling is idle.
  const refreshVisible = () => {
    if (!documentTarget.hidden) refresh(true);
  };
  windowTarget.addEventListener("focus", refreshVisible);
  documentTarget.addEventListener("visibilitychange", refreshVisible);
  return () => {
    windowTarget.clearInterval(interval);
    windowTarget.removeEventListener("focus", refreshVisible);
    documentTarget.removeEventListener("visibilitychange", refreshVisible);
  };
}

export function runtimePollingRequired(
  projectionRequiresPolling: boolean,
  runStartMutationPending: boolean,
  hub: Pick<HubProjection, "status" | "active_main" | "active_side_chat"> | null = null,
  publish: Pick<PublishProjection, "profiles"> | null = null,
): boolean {
  // A Hub command can return its owner before the next ordinary Desktop snapshot.
  // Keep the existing poll alive across that handoff; both views come from Rust.
  return projectionRequiresPolling || runStartMutationPending
    || hub?.status === "connecting" || hub?.status === "connected"
    || Boolean(hub?.active_main || hub?.active_side_chat)
    || Boolean(publish?.profiles.some((row) => ["starting", "running", "stopping"].includes(row.status)));
}

export interface NavigationAdmissionState {
  navigation_admission_open: boolean;
}

export function navigationIsIdle(state: NavigationAdmissionState): boolean {
  return state.navigation_admission_open;
}

export function sessionActionIndex(selectedIndex: number, payloadIndex: number): number {
  return payloadIndex >= 0 ? payloadIndex : selectedIndex;
}

export function sessionRowActionAvailable(
  sessionCount: number,
  selectedIndex: number,
  payloadIndex: number,
): boolean {
  const index = sessionActionIndex(selectedIndex, payloadIndex);
  return index >= 0 && index < sessionCount;
}

export interface SessionRowCapabilities {
  rejoinAction: string;
  secondaryAction: string;
  rollbackAction: string;
  deleteAction: string;
}

export function sessionRowCapabilities(
  loadedStatus: string,
  archived: boolean,
): SessionRowCapabilities {
  if (loadedStatus === "active") {
    return {
      rejoinAction: "rejoin-session",
      secondaryAction: archived ? "unarchive-session" : "interrupt-session",
      rollbackAction: "",
      deleteAction: "",
    };
  }
  return {
    rejoinAction: "",
    secondaryAction: archived ? "unarchive-session" : "archive-session",
    rollbackAction: "rollback-session",
    deleteAction: "delete-session",
  };
}

export function quickChatDeleteAction(loadedStatus: string): string {
  return loadedStatus === "active" ? "" : "delete-chat-session";
}

export function configCommitEnabled(
  setupRequired: boolean,
  dirty: boolean,
  mutationPending: boolean,
): boolean {
  return !mutationPending && (setupRequired || dirty);
}

export interface NavigationAdmissionState {
  busy: boolean;
  background_mutation_pending: boolean;
  navigation_loading: boolean;
}

export function navigationIsIdle(state: NavigationAdmissionState): boolean {
  return !state.busy && !state.background_mutation_pending && !state.navigation_loading;
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

export function sessionMemoryActions(
  loadedStatus: string,
  memoryMode: "enabled" | "disabled",
): { enable: boolean; disable: boolean } {
  if (loadedStatus === "active") {
    return { enable: false, disable: false };
  }
  return {
    enable: memoryMode === "disabled",
    disable: memoryMode === "enabled",
  };
}

export function configCommitEnabled(
  setupRequired: boolean,
  dirty: boolean,
  mutationPending: boolean,
): boolean {
  return !mutationPending && (setupRequired || dirty);
}

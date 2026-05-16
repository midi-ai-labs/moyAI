import type { LocalConfirmation } from "./render";

export interface UiLocalState {
  pendingLocalConfirmation: LocalConfirmation | null;
  configFilterText: string;
  configDirty: boolean;
  lastFocusedOverlay: string;
  artifactPaneCollapsed: boolean;
  attachmentTrayOpen: boolean;
}

export function createUiLocalState(): UiLocalState {
  return {
    pendingLocalConfirmation: null,
    configFilterText: "",
    configDirty: false,
    lastFocusedOverlay: "none",
    artifactPaneCollapsed: window.localStorage.getItem("moyai.artifactPaneCollapsed") === "true",
    attachmentTrayOpen: false,
  };
}

export function setArtifactPaneCollapsed(uiState: UiLocalState, collapsed: boolean): void {
  uiState.artifactPaneCollapsed = collapsed;
  window.localStorage.setItem("moyai.artifactPaneCollapsed", String(collapsed));
}

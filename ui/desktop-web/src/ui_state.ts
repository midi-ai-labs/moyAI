import type { LocalConfirmation } from "./render";

export interface UiLocalState {
  pendingLocalConfirmation: LocalConfirmation | null;
  configFilterText: string;
  configDirty: boolean;
  lastFocusedOverlay: string;
  focusPromptAfterRender: boolean;
  initialPromptFocusDone: boolean;
  artifactPaneCollapsed: boolean;
  attachmentTrayOpen: boolean;
}

export function createUiLocalState(): UiLocalState {
  return {
    pendingLocalConfirmation: null,
    configFilterText: "",
    configDirty: false,
    lastFocusedOverlay: "none",
    focusPromptAfterRender: false,
    initialPromptFocusDone: false,
    artifactPaneCollapsed: window.localStorage.getItem("moyai.artifactPaneCollapsed") === "true",
    attachmentTrayOpen: false,
  };
}

export function setArtifactPaneCollapsed(uiState: UiLocalState, collapsed: boolean): void {
  uiState.artifactPaneCollapsed = collapsed;
  window.localStorage.setItem("moyai.artifactPaneCollapsed", String(collapsed));
}

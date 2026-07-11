import type { LocalConfirmation } from "./render";
import type { ConfigMutationTarget } from "./types";

export interface UiLocalState {
  pendingLocalConfirmation: LocalConfirmation | null;
  configDirty: boolean;
  configDraftValues: Map<string, string>;
  configDraftTarget: ConfigMutationTarget | null;
  configDraftRevision: number;
  nextConfigMutationGeneration: number;
  activeConfigMutationGeneration: number | null;
  lastFocusedOverlay: string;
  focusPromptAfterRender: boolean;
  promptDraftPreservationBlocked: boolean;
  promptInvalidationCommandPending: boolean;
  initialPromptFocusDone: boolean;
  artifactPaneCollapsed: boolean;
  attachmentTrayOpen: boolean;
  permissionDecisionPending: boolean;
  permissionDecisionAllow: boolean | null;
  permissionDecisionConfirmationId: number | null;
  permissionDecisionError: string;
  localConfirmationDecisionPending: boolean;
  localConfirmationDecisionError: string;
  windowMaximized: boolean;
}

export function createUiLocalState(): UiLocalState {
  return {
    pendingLocalConfirmation: null,
    configDirty: false,
    configDraftValues: new Map(),
    configDraftTarget: null,
    configDraftRevision: 0,
    nextConfigMutationGeneration: 1,
    activeConfigMutationGeneration: null,
    lastFocusedOverlay: "none",
    focusPromptAfterRender: false,
    promptDraftPreservationBlocked: false,
    promptInvalidationCommandPending: false,
    initialPromptFocusDone: false,
    artifactPaneCollapsed: window.localStorage.getItem("moyai.artifactPaneCollapsed") === "true",
    attachmentTrayOpen: false,
    permissionDecisionPending: false,
    permissionDecisionAllow: null,
    permissionDecisionConfirmationId: null,
    permissionDecisionError: "",
    localConfirmationDecisionPending: false,
    localConfirmationDecisionError: "",
    windowMaximized: false,
  };
}

export function setArtifactPaneCollapsed(uiState: UiLocalState, collapsed: boolean): void {
  uiState.artifactPaneCollapsed = collapsed;
  window.localStorage.setItem("moyai.artifactPaneCollapsed", String(collapsed));
}

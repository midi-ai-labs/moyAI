import type { LocalConfirmation } from "./render.ts";
import type { ConfigMutationTarget, DesktopWebState } from "./types.ts";

export interface ProviderDraft {
  baseUrl: string;
  metadataMode: DesktopWebState["provider_metadata_mode"];
  contextWindow: string;
  maxOutputTokens: string;
  selectedModelId: string;
}

export interface UiDraftState {
  initialized: boolean;
  composerOwner: string;
  composerCommitGeneration: string;
  sessionSearchOwner: string;
  providerOwner: string;
  prompt: string;
  imageInput: string;
  workspaceInput: string;
  reviewDraft: string;
  localSearch: string;
  sessionSearch: string;
  provider: ProviderDraft;
  composerRevision: number;
  imageRevision: number;
  workspaceRevision: number;
  reviewRevision: number;
  providerRevision: number;
  pendingRunSubmission: {
    owner: string;
    workspacePath: string;
    composerRevision: number;
    imageRevision: number;
    reviewRevision: number | null;
    baseCommitGeneration: string;
    commandAccepted: boolean;
  } | null;
}

export interface UiRecoverableError {
  title: string;
  hint: string;
  details: string;
}

export interface UiLocalState {
  drafts: UiDraftState;
  runStartMutationPending: boolean;
  externalConfigMutationPending: boolean;
  pendingLocalConfirmation: LocalConfirmation | null;
  configDirty: boolean;
  configDraftValues: Map<string, string>;
  configDraftBaselineValues: Map<string, string>;
  configDraftTarget: ConfigMutationTarget | null;
  configDraftRevision: number;
  nextConfigMutationGeneration: number;
  activeConfigMutationGeneration: number | null;
  lastFocusedOverlay: string;
  focusPromptAfterRender: boolean;
  initialPromptFocusDone: boolean;
  artifactPaneCollapsed: boolean;
  attachmentTrayOpen: boolean;
  permissionDecisionPending: boolean;
  permissionDecisionAllow: boolean | null;
  permissionDecisionConfirmationId: number | null;
  permissionDecisionError: string;
  localConfirmationDecisionPending: boolean;
  localConfirmationDecisionError: string;
  recoverableError: UiRecoverableError | null;
  windowMaximized: boolean;
}

export function createUiLocalState(): UiLocalState {
  return {
    drafts: {
      initialized: false,
      composerOwner: "",
      composerCommitGeneration: "0",
      sessionSearchOwner: "",
      providerOwner: "",
      prompt: "",
      imageInput: "",
      workspaceInput: "",
      reviewDraft: "",
      localSearch: "",
      sessionSearch: "",
      provider: {
        baseUrl: "",
        metadataMode: "openai_compatible_only",
        contextWindow: "",
        maxOutputTokens: "",
        selectedModelId: "",
      },
      composerRevision: 0,
      imageRevision: 0,
      workspaceRevision: 0,
      reviewRevision: 0,
      providerRevision: 0,
      pendingRunSubmission: null,
    },
    runStartMutationPending: false,
    externalConfigMutationPending: false,
    pendingLocalConfirmation: null,
    configDirty: false,
    configDraftValues: new Map(),
    configDraftBaselineValues: new Map(),
    configDraftTarget: null,
    configDraftRevision: 0,
    nextConfigMutationGeneration: 1,
    activeConfigMutationGeneration: null,
    lastFocusedOverlay: "none",
    focusPromptAfterRender: false,
    initialPromptFocusDone: false,
    artifactPaneCollapsed: typeof window !== "undefined"
      && window.localStorage.getItem("moyai.artifactPaneCollapsed") === "true",
    attachmentTrayOpen: false,
    permissionDecisionPending: false,
    permissionDecisionAllow: null,
    permissionDecisionConfirmationId: null,
    permissionDecisionError: "",
    localConfirmationDecisionPending: false,
    localConfirmationDecisionError: "",
    recoverableError: null,
    windowMaximized: false,
  };
}

export function setArtifactPaneCollapsed(uiState: UiLocalState, collapsed: boolean): void {
  uiState.artifactPaneCollapsed = collapsed;
  if (typeof window !== "undefined") {
    window.localStorage.setItem("moyai.artifactPaneCollapsed", String(collapsed));
  }
}

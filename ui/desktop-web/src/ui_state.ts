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

export type ArtifactPaneMode = "output" | "agents";

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
  artifactPaneMode: ArtifactPaneMode;
  selectedAgentPath: string | null;
  agentPaneOwnerIdentity: string;
  focusSelectedAgentAfterRender: boolean;
  attachmentTrayOpen: boolean;
  permissionDecisionPending: boolean;
  permissionDecisionAllow: boolean | null;
  permissionDecisionConfirmationId: string | null;
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
    artifactPaneMode: "output",
    selectedAgentPath: null,
    agentPaneOwnerIdentity: "",
    focusSelectedAgentAfterRender: false,
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

export function agentPaneOwnerIdentity(
  state: Pick<DesktopWebState, "workspace_path" | "draft_target">,
): string {
  return `${state.workspace_path}\u0000${state.draft_target.sessionId ?? ""}`;
}

export function reconcileAgentPaneState(
  uiState: UiLocalState,
  state: Pick<DesktopWebState, "workspace_path" | "draft_target" | "agent_activity_rows">,
): void {
  const ownerIdentity = agentPaneOwnerIdentity(state);
  if (uiState.agentPaneOwnerIdentity !== ownerIdentity) {
    uiState.artifactPaneMode = "output";
    uiState.selectedAgentPath = null;
    uiState.agentPaneOwnerIdentity = ownerIdentity;
    uiState.focusSelectedAgentAfterRender = false;
    return;
  }
  if (
    uiState.selectedAgentPath !== null
    && !state.agent_activity_rows.some((row) => row.agent_path === uiState.selectedAgentPath)
  ) {
    uiState.artifactPaneMode = "output";
    uiState.selectedAgentPath = null;
    uiState.focusSelectedAgentAfterRender = false;
  }
  if (state.agent_activity_rows.length === 0 && uiState.artifactPaneMode === "agents") {
    uiState.artifactPaneMode = "output";
    uiState.selectedAgentPath = null;
    uiState.focusSelectedAgentAfterRender = false;
  }
}

export function openAgentPane(
  uiState: UiLocalState,
  state: Pick<DesktopWebState, "workspace_path" | "draft_target" | "agent_activity_rows">,
  requestedAgentPath: string,
): boolean {
  const rows = [...state.agent_activity_rows].sort((left, right) => {
    if (left.started_order !== right.started_order) return left.started_order - right.started_order;
    return left.agent_path.localeCompare(right.agent_path);
  });
  if (rows.length === 0) return false;
  const selected = rows.find((row) => row.agent_path === requestedAgentPath)
    ?? rows.find((row) => row.agent_path === uiState.selectedAgentPath)
    ?? rows.find((row) => row.status === "pending_init" || row.status === "running")
    ?? rows[0];
  uiState.artifactPaneMode = "agents";
  uiState.selectedAgentPath = selected.agent_path;
  uiState.agentPaneOwnerIdentity = agentPaneOwnerIdentity(state);
  uiState.focusSelectedAgentAfterRender = true;
  setArtifactPaneCollapsed(uiState, false);
  return true;
}

export function showOutputPane(uiState: UiLocalState): void {
  uiState.artifactPaneMode = "output";
  uiState.focusSelectedAgentAfterRender = false;
}

import type { LocalConfirmation } from "./render.ts";
import { agentActivityRowIdentity } from "./agent_activity.ts";
import type {
  AgentActivityRow,
  AgentExecutionExpectedTarget,
  AgentExecutionProjection,
  ConfigMutationTarget,
  DesktopWebState,
} from "./types.ts";
import type { PermissionDecisionState } from "./decision_state.ts";

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
  reviewSyncedRevision: number;
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
export type AgentPaneFocusTarget = "agent-pane-back" | "output-agent-trigger";

export interface AgentExecutionCacheEntry {
  status: "loading" | "ready" | "error";
  generation: number;
  expectedTarget: AgentExecutionExpectedTarget;
  projection: AgentExecutionProjection | null;
  error: string;
}

export interface AgentExecutionRequest {
  cacheKey: string;
  generation: number;
  ownerIdentity: string;
  expectedTarget: AgentExecutionExpectedTarget;
  activityIdentity: string;
  operation: "replace" | "prepend";
  expectedOffset: number | null;
  expectedEnd: number | null;
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
  configDraftRevision: bigint;
  nextConfigMutationGeneration: bigint;
  activeConfigMutationGeneration: bigint | null;
  lastFocusedOverlay: string;
  focusPromptAfterRender: boolean;
  initialPromptFocusDone: boolean;
  artifactPaneCollapsed: boolean;
  artifactPaneMode: ArtifactPaneMode;
  selectedAgentPath: string | null;
  agentPaneOwnerIdentity: string;
  focusSelectedAgentAfterRender: boolean;
  agentPaneFocusAfterRender: AgentPaneFocusTarget | null;
  agentExecutionCache: Map<string, AgentExecutionCacheEntry>;
  activeAgentExecutionRequest: AgentExecutionRequest | null;
  nextAgentExecutionGeneration: number;
  attachmentTrayOpen: boolean;
  permissionDecision: PermissionDecisionState | null;
  nextPermissionSubmissionId: number;
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
      reviewSyncedRevision: 0,
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
    configDraftRevision: 0n,
    nextConfigMutationGeneration: 1n,
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
    agentPaneFocusAfterRender: null,
    agentExecutionCache: new Map(),
    activeAgentExecutionRequest: null,
    nextAgentExecutionGeneration: 1,
    attachmentTrayOpen: false,
    permissionDecision: null,
    nextPermissionSubmissionId: 1,
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

export function agentExecutionSnapshotOwnerIdentity(
  state: Pick<DesktopWebState, "workspace_path" | "draft_target" | "agent_activity_rows">,
  selectedAgentPath: string | null,
): string | null {
  if (!selectedAgentPath) return null;
  const row = state.agent_activity_rows.find((candidate) => candidate.agent_path === selectedAgentPath);
  if (!row || !state.draft_target.sessionId) return null;
  return [
    state.workspace_path,
    state.draft_target.sessionId,
    row.agent_path,
    row.session_id,
  ].join("\u0000");
}

export function shouldPreserveAgentExecutionSnapshots(
  previousOwnerIdentity: string | null,
  nextOwnerIdentity: string | null,
): boolean {
  return previousOwnerIdentity !== null && previousOwnerIdentity === nextOwnerIdentity;
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
    uiState.agentPaneFocusAfterRender = null;
    uiState.agentExecutionCache.clear();
    uiState.activeAgentExecutionRequest = null;
    return;
  }
  if (
    uiState.selectedAgentPath !== null
    && !state.agent_activity_rows.some((row) => row.agent_path === uiState.selectedAgentPath)
  ) {
    uiState.artifactPaneMode = "output";
    uiState.selectedAgentPath = null;
    uiState.focusSelectedAgentAfterRender = false;
    uiState.agentPaneFocusAfterRender = null;
    uiState.activeAgentExecutionRequest = null;
  }
  if (state.agent_activity_rows.length === 0 && uiState.artifactPaneMode === "agents") {
    uiState.artifactPaneMode = "output";
    uiState.selectedAgentPath = null;
    uiState.focusSelectedAgentAfterRender = false;
    uiState.agentPaneFocusAfterRender = null;
    uiState.activeAgentExecutionRequest = null;
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
  uiState.artifactPaneMode = "agents";
  const selected = requestedAgentPath
    ? rows.find((row) => row.agent_path === requestedAgentPath) ?? null
    : null;
  uiState.selectedAgentPath = selected?.agent_path ?? null;
  uiState.agentPaneOwnerIdentity = agentPaneOwnerIdentity(state);
  uiState.focusSelectedAgentAfterRender = selected !== null;
  uiState.agentPaneFocusAfterRender = null;
  setArtifactPaneCollapsed(uiState, false);
  return true;
}

export function showOutputPane(uiState: UiLocalState, focusOutputTrigger = false): void {
  uiState.artifactPaneMode = "output";
  uiState.selectedAgentPath = null;
  uiState.focusSelectedAgentAfterRender = false;
  uiState.agentPaneFocusAfterRender = focusOutputTrigger ? "output-agent-trigger" : null;
  uiState.activeAgentExecutionRequest = null;
}

export function showAgentList(uiState: UiLocalState): void {
  uiState.artifactPaneMode = "agents";
  uiState.selectedAgentPath = null;
  uiState.focusSelectedAgentAfterRender = false;
  uiState.agentPaneFocusAfterRender = "agent-pane-back";
  uiState.activeAgentExecutionRequest = null;
}

export function beginAgentExecutionLoad(
  uiState: UiLocalState,
  state: Pick<DesktopWebState, "workspace_path" | "draft_target">,
  row: AgentActivityRow,
): AgentExecutionRequest {
  const rootSessionId = state.draft_target.sessionId ?? "";
  const expectedTarget: AgentExecutionExpectedTarget = {
    workspacePath: state.workspace_path,
    rootSessionId,
    agentPath: row.agent_path,
    childSessionId: row.session_id,
  };
  const cacheKey = agentExecutionCacheKey(expectedTarget);
  const generation = uiState.nextAgentExecutionGeneration++;
  const request: AgentExecutionRequest = {
    cacheKey,
    generation,
    ownerIdentity: agentPaneOwnerIdentity(state),
    expectedTarget,
    activityIdentity: agentActivityRowIdentity(row),
    operation: "replace",
    expectedOffset: null,
    expectedEnd: null,
  };
  const cached = uiState.agentExecutionCache.get(cacheKey);
  uiState.agentExecutionCache.set(cacheKey, {
    status: "loading",
    generation,
    expectedTarget,
    projection: cached?.projection ?? null,
    error: "",
  });
  uiState.activeAgentExecutionRequest = request;
  return request;
}

export function beginPreviousAgentExecutionPageLoad(
  uiState: UiLocalState,
  state: Pick<DesktopWebState, "workspace_path" | "draft_target">,
  row: AgentActivityRow,
): AgentExecutionRequest | null {
  if (uiState.activeAgentExecutionRequest !== null) return null;
  const rootSessionId = state.draft_target.sessionId ?? "";
  const expectedTarget: AgentExecutionExpectedTarget = {
    workspacePath: state.workspace_path,
    rootSessionId,
    agentPath: row.agent_path,
    childSessionId: row.session_id,
  };
  const cacheKey = agentExecutionCacheKey(expectedTarget);
  const cached = uiState.agentExecutionCache.get(cacheKey);
  const expectedOffset = cached?.projection?.turn_page_offset ?? 0;
  const expectedEnd = cached?.projection?.turn_page_end ?? 0;
  if (expectedOffset <= 0 || expectedEnd <= expectedOffset) return null;

  const generation = uiState.nextAgentExecutionGeneration++;
  const request: AgentExecutionRequest = {
    cacheKey,
    generation,
    ownerIdentity: agentPaneOwnerIdentity(state),
    expectedTarget,
    activityIdentity: agentActivityRowIdentity(row),
    operation: "prepend",
    expectedOffset,
    expectedEnd,
  };
  uiState.agentExecutionCache.set(cacheKey, {
    status: "loading",
    generation,
    expectedTarget,
    projection: cached?.projection ?? null,
    error: "",
  });
  uiState.activeAgentExecutionRequest = request;
  return request;
}

export function finishAgentExecutionLoad(
  uiState: UiLocalState,
  request: AgentExecutionRequest,
  projection: AgentExecutionProjection,
): boolean {
  if (!agentExecutionRequestIsCurrent(uiState, request)) return false;
  if (!agentExecutionProjectionMatches(request.expectedTarget, projection)) {
    const cached = uiState.agentExecutionCache.get(request.cacheKey);
    uiState.agentExecutionCache.set(request.cacheKey, {
      status: "error",
      generation: request.generation,
      expectedTarget: request.expectedTarget,
      projection: cached?.projection ?? null,
      error: "読み込み結果の対象が現在のSub Agentと一致しませんでした。",
    });
    uiState.activeAgentExecutionRequest = null;
    return true;
  }
  if (request.operation === "prepend") {
    const cached = uiState.agentExecutionCache.get(request.cacheKey);
    const newer = cached?.projection;
    const expectedOffset = request.expectedOffset;
    const expectedEnd = request.expectedEnd;
    if (
      !newer
      || expectedOffset === null
      || expectedEnd === null
      || newer.turn_page_offset !== expectedOffset
      || newer.turn_page_end !== expectedEnd
      || projection.turn_page_offset >= expectedOffset
      || projection.turn_page_end !== expectedEnd
      || projection.turn_page_total < projection.turn_page_end
    ) {
      uiState.agentExecutionCache.set(request.cacheKey, {
        status: "error",
        generation: request.generation,
        expectedTarget: request.expectedTarget,
        projection: newer ?? null,
        error: "以前の実行履歴が現在の表示範囲と連続していませんでした。",
      });
      uiState.activeAgentExecutionRequest = null;
      return true;
    }
    uiState.agentExecutionCache.set(request.cacheKey, {
      status: "ready",
      generation: request.generation,
      expectedTarget: request.expectedTarget,
      projection,
      error: "",
    });
    uiState.activeAgentExecutionRequest = null;
    return true;
  }
  uiState.agentExecutionCache.set(request.cacheKey, {
    status: "ready",
    generation: request.generation,
    expectedTarget: request.expectedTarget,
    projection,
    error: "",
  });
  uiState.activeAgentExecutionRequest = null;
  return true;
}

export function failAgentExecutionLoad(
  uiState: UiLocalState,
  request: AgentExecutionRequest,
  error: string,
): boolean {
  if (!agentExecutionRequestIsCurrent(uiState, request)) return false;
  const cached = uiState.agentExecutionCache.get(request.cacheKey);
  uiState.agentExecutionCache.set(request.cacheKey, {
    status: "error",
    generation: request.generation,
    expectedTarget: request.expectedTarget,
    projection: cached?.projection ?? null,
    error,
  });
  uiState.activeAgentExecutionRequest = null;
  return true;
}

export function selectedAgentExecution(
  uiState: UiLocalState,
  state: Pick<DesktopWebState, "workspace_path" | "draft_target" | "agent_activity_rows">,
): AgentExecutionCacheEntry | null {
  if (!uiState.selectedAgentPath) return null;
  const row = state.agent_activity_rows.find((candidate) => candidate.agent_path === uiState.selectedAgentPath);
  if (!row) return null;
  return uiState.agentExecutionCache.get(agentExecutionCacheKey({
    workspacePath: state.workspace_path,
    rootSessionId: state.draft_target.sessionId ?? "",
    agentPath: row.agent_path,
    childSessionId: row.session_id,
  })) ?? null;
}

export function agentExecutionRequestNeedsRefresh(
  request: AgentExecutionRequest,
  state: Pick<DesktopWebState, "agent_activity_rows">,
): boolean {
  const current = state.agent_activity_rows.find(
    (row) => row.agent_path === request.expectedTarget.agentPath,
  );
  return current !== undefined
    && agentActivityRowIdentity(current) !== request.activityIdentity;
}

function agentExecutionRequestIsCurrent(
  uiState: UiLocalState,
  request: AgentExecutionRequest,
): boolean {
  const active = uiState.activeAgentExecutionRequest;
  return active?.generation === request.generation
    && active.cacheKey === request.cacheKey
    && uiState.agentPaneOwnerIdentity === request.ownerIdentity
    && uiState.artifactPaneMode === "agents"
    && uiState.selectedAgentPath === request.expectedTarget.agentPath;
}

function agentExecutionCacheKey(target: AgentExecutionExpectedTarget): string {
  return [
    target.workspacePath,
    target.rootSessionId,
    target.agentPath,
    target.childSessionId,
  ].join("\u0000");
}

function agentExecutionProjectionMatches(
  target: AgentExecutionExpectedTarget,
  projection: AgentExecutionProjection,
): boolean {
  return projection.workspace_path === target.workspacePath
    && projection.root_session_id === target.rootSessionId
    && projection.agent_path === target.agentPath
    && projection.session_id === target.childSessionId;
}

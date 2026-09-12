import type { LocalConfirmation } from "./render_overlays.ts";
import { createHubUiState, type HubUiState } from "./hub_state.ts";
import { createDeviceNetworkUiState, type DeviceNetworkUiState } from "./device_network_state.ts";
import { createMcpHistoryUiState, type McpHistoryUiState } from "./mcp_history_state.ts";
import { createMcpPeerState, type McpPeerState } from "./mcp_peer.ts";
import { agentActivityRowIdentity } from "./agent_activity.ts";
import {
  asyncTransactionIsCurrent,
  beginAsyncTransaction,
  clearAsyncTransaction,
  createAsyncTransactionSlot,
  type AsyncTransactionSlot,
} from "./async_transaction.ts";
import type {
  AgentActivityRow,
  AgentExecutionExpectedTarget,
  AgentExecutionProjection,
  ConfigMutationTarget,
  DesktopWebState,
  PromptReviewMutationTarget,
  ProviderProfile,
  SideChatCatalogModel,
  SideChatCatalogResult,
  SideChatDraftQuoteProjection,
  SideChatPendingQuote,
} from "./types.ts";
import { replaceReadableSideChatQuote } from "./side_chat_quote.ts";
import type { AttachmentFocusContinuation } from "./attachment_focus_continuation.ts";
import type { PermissionDecisionState } from "./decision_state.ts";
import type { MainRunFocusContinuation } from "./run_focus_continuation.ts";
import type { SideChatFocusContinuation } from "./side_chat_focus_continuation.ts";
import type { SettingsActionFocusContinuation } from "./settings_surface.ts";
import type { QuickChatDeleteFocusContinuation } from "./quick_chat_delete_focus_continuation.ts";
import type { AgentExecutionPrependContinuation } from "./agent_execution_prepend_continuation.ts";
import type { RefreshPromptFocusContinuation } from "./main_prompt_continuity.ts";
import type { NewSessionMutationRequest } from "./new_session_mutation.ts";
import type { TaskActivityAnimationEpoch } from "./task_activity_indicator.ts";
import {
  createInitialSetupState,
  type InitialSetupState,
} from "./initial_setup_state.ts";
import {
  createInitialSetupAuxiliaryState,
  type InitialSetupAuxiliaryState,
} from "./initial_setup_auxiliary_state.ts";
import {
  createSessionSettingsState,
  sameSessionSettingsTarget,
  sessionSettingsApplyEnabled,
  type SessionSettingsDraft,
  type SessionSettingsState,
} from "./session_settings_state.ts";
import {
  configDraftAppliesTo,
  sameConfigMutationTarget,
} from "./config_mutation.ts";
import { validateProviderBaseUrl } from "./utils.ts";

export interface ProviderDraft {
  baseUrl: string;
  providerProfile: DesktopWebState["provider_profile"];
  apiKeyEnv: string;
  contextWindow: string;
  selectedModelId: string;
}

export interface ProviderCatalogTarget {
  readonly providerOwner: string;
  readonly providerRevision: number;
  readonly baseUrl: string;
  readonly providerProfile: ProviderProfile;
  readonly apiKeyEnv: string | null;
}

export interface ProviderCatalogRequest extends ProviderCatalogTarget {
  readonly token: number;
  admitted: boolean;
}

export interface DoclingReadinessRequest {
  readonly token: number;
  readonly expectedTarget: Readonly<ConfigMutationTarget>;
}

export interface UiDraftState {
  initialized: boolean;
  composerOwner: string;
  composerSessionOwner: string;
  composerCommitGeneration: string;
  sessionSearchOwner: string;
  providerOwner: string;
  prompt: string;
  imageInput: string;
  workspaceInput: string;
  reviewTarget: PromptReviewMutationTarget | null;
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
  providerCatalogIdentityRevision: number;
  pendingRunSubmission: {
    owner: string;
    workspacePath: string;
    composerRevision: number;
    imageRevision: number;
    reviewTarget: PromptReviewMutationTarget | null;
    reviewRevision: number | null;
    baseCommitGeneration: string;
    commandAccepted: boolean;
  } | null;
}

export interface MainComposerLocalDraft {
  prompt: string;
  imageInput: string;
}

export interface SessionInteractionSnapshot {
  threadScrollLeft: number;
  threadScrollTop: number;
  promptScrollLeft: number;
  promptScrollTop: number;
  promptSelectionStart: number;
  promptSelectionEnd: number;
}

export interface UiRecoverableError {
  title: string;
  hint: string;
  details: string;
}

export type ArtifactPaneMode = "output" | "agents" | "side_chat";
export type AgentPaneFocusTarget = "agent-pane-back" | "output-agent-trigger";
export type ArtifactPaneFocusTarget = "content" | "trigger";

export interface SideChatLocalDraft {
  chatId: string | null;
  text: string;
  revision: number;
  persistedText: string;
  persistedQuote: SideChatPendingQuote | null;
  persistedRevision: string;
  saveTimer: number | null;
  saveInFlight: boolean;
  savePromise: Promise<void> | null;
  saveQueued: boolean;
  pendingQuote: SideChatPendingQuote | null;
}

export interface SideChatMutationState {
  kind: "ensure" | "capture" | "send" | "cancel" | "delete";
  chatId: string | null;
  generation: string;
}

export type SideChatCatalogStatus = "idle" | "loading" | "ready" | "error";

export interface SideChatCatalogEntry {
  configTarget: Readonly<ConfigMutationTarget>;
  identityRevision: number;
  baseUrl: string;
  providerProfile: ProviderProfile;
  configGeneration: string;
  models: SideChatCatalogModel[];
  status: Exclude<SideChatCatalogStatus, "idle">;
  error: string;
  requestToken: number;
}

export interface SideChatCatalogTarget {
  readonly key: string;
  readonly configTarget: Readonly<ConfigMutationTarget>;
  readonly identityRevision: number;
  readonly baseUrl: string;
  readonly providerProfile: ProviderProfile;
  readonly configGeneration: string;
}

export interface SideChatCatalogRequest extends SideChatCatalogTarget {
  readonly token: number;
}

export interface SideChatCatalogSettlement {
  catalogAccepted: boolean;
  localStateChanged: boolean;
}

export interface SideChatCatalogView {
  status: SideChatCatalogStatus;
  source: "none" | "main" | "global";
  baseUrl: string;
  models: SideChatCatalogModel[];
  error: string;
}

export interface SideChatModelOption extends SideChatCatalogModel {
  currentOnly: boolean;
}

export interface SideChatDeleteConfirmation {
  ownerSessionId: string;
  chatId: string;
  expectedGeneration: string;
}

export interface AgentExecutionCacheEntry {
  status: "loading" | "ready" | "error";
  generation: number;
  expectedTarget: AgentExecutionExpectedTarget;
  projection: AgentExecutionProjection | null;
  error: string;
}

export interface AgentExecutionTarget {
  readonly cacheKey: string;
  readonly ownerIdentity: string;
  readonly expectedTarget: Readonly<AgentExecutionExpectedTarget>;
  readonly activityIdentity: string;
  readonly operation: "replace" | "prepend";
  readonly expectedOffset: number | null;
  readonly expectedEnd: number | null;
}

export interface AgentExecutionRequest extends AgentExecutionTarget {
  readonly generation: number;
}

export interface UiLocalState {
  hub: HubUiState;
  deviceNetwork: DeviceNetworkUiState;
  mcpHistory: McpHistoryUiState;
  mcpPeers: McpPeerState;
  drafts: UiDraftState;
  initialSetup: InitialSetupState;
  initialSetupAuxiliary: InitialSetupAuxiliaryState;
  sessionSettings: SessionSettingsState;
  mainComposerDrafts: Map<string, MainComposerLocalDraft>;
  sessionInteractionSnapshots: Map<string, SessionInteractionSnapshot>;
  runStartMutationPending: boolean;
  taskActivityAnimationEpoch: TaskActivityAnimationEpoch | null;
  mcpActivityAnimationEpoch: TaskActivityAnimationEpoch | null;
  externalConfigMutationPending: boolean;
  activeNewSessionMutation: NewSessionMutationRequest | null;
  pendingLocalConfirmation: LocalConfirmation | null;
  configDirty: boolean;
  configDraftValues: Map<string, string>;
  configDraftBaselineValues: Map<string, string>;
  configDraftTarget: ConfigMutationTarget | null;
  configDraftRevision: bigint;
  nextConfigMutationGeneration: bigint;
  activeConfigMutationGeneration: bigint | null;
  lastFocusedOverlay: string;
  settingsActionFocusContinuation: SettingsActionFocusContinuation | null;
  titlebarMenuFocusContinuation: { overlay: string; action: string } | null;
  mainRunFocusContinuation: MainRunFocusContinuation | null;
  sideChatFocusContinuation: SideChatFocusContinuation | null;
  quickChatDeleteFocusContinuation: QuickChatDeleteFocusContinuation | null;
  sideChatFocusInteractionGeneration: bigint;
  refreshPromptFocusInteractionGeneration: bigint;
  pendingRefreshPromptFocus: RefreshPromptFocusContinuation | null;
  focusPromptAfterRender: boolean;
  initialPromptFocusDone: boolean;
  artifactPaneCollapsed: boolean;
  artifactPaneFocusAfterRender: ArtifactPaneFocusTarget | null;
  artifactPaneMode: ArtifactPaneMode;
  selectedAgentPath: string | null;
  agentPaneOwnerIdentity: string;
  focusSelectedAgentAfterRender: boolean;
  agentPaneFocusAfterRender: AgentPaneFocusTarget | null;
  agentExecutionCache: Map<string, AgentExecutionCacheEntry>;
  agentExecutionTransaction: AsyncTransactionSlot<AgentExecutionRequest>;
  agentExecutionPrependContinuation: AgentExecutionPrependContinuation | null;
  sideChatDrafts: Map<string, SideChatLocalDraft>;
  sideChatMutations: Map<string, SideChatMutationState>;
  sideChatCatalogs: Map<string, SideChatCatalogEntry>;
  sideChatCatalogTransaction: AsyncTransactionSlot<SideChatCatalogRequest>;
  sideChatCatalogIdentityRevision: number;
  providerCatalogTransaction: AsyncTransactionSlot<ProviderCatalogRequest>;
  providerCatalogRevision: number | null;
  doclingReadinessTransaction: AsyncTransactionSlot<DoclingReadinessRequest>;
  rejectedProviderCatalogRequest: ProviderCatalogRequest | null;
  sideChatDeleteConfirmation: SideChatDeleteConfirmation | null;
  attachmentTrayOpen: boolean;
  attachmentFocusContinuation: AttachmentFocusContinuation | null;
  permissionDecision: PermissionDecisionState | null;
  nextPermissionSubmissionId: number;
  localConfirmationDecisionPending: boolean;
  localConfirmationDecisionError: string;
  recoverableError: UiRecoverableError | null;
  recoverableErrorOwner: string | null;
  windowMaximized: boolean;
}

export function createUiLocalState(): UiLocalState {
  return {
    hub: createHubUiState(),
    deviceNetwork: createDeviceNetworkUiState(),
    mcpHistory: createMcpHistoryUiState(),
    mcpPeers: createMcpPeerState(),
    drafts: {
      initialized: false,
      composerOwner: "",
      composerSessionOwner: "",
      composerCommitGeneration: "0",
      sessionSearchOwner: "",
      providerOwner: "",
      prompt: "",
      imageInput: "",
      workspaceInput: "",
      reviewTarget: null,
      reviewDraft: "",
      localSearch: "",
      sessionSearch: "",
      provider: {
        baseUrl: "",
        providerProfile: "openai_compatible",
        apiKeyEnv: "",
        contextWindow: "",
        selectedModelId: "",
      },
      composerRevision: 0,
      imageRevision: 0,
      workspaceRevision: 0,
      reviewRevision: 0,
      reviewSyncedRevision: 0,
      providerRevision: 0,
      providerCatalogIdentityRevision: 0,
      pendingRunSubmission: null,
    },
    initialSetup: createInitialSetupState(),
    initialSetupAuxiliary: createInitialSetupAuxiliaryState(),
    sessionSettings: createSessionSettingsState(),
    mainComposerDrafts: new Map(),
    sessionInteractionSnapshots: new Map(),
    runStartMutationPending: false,
    taskActivityAnimationEpoch: null,
    mcpActivityAnimationEpoch: null,
    externalConfigMutationPending: false,
    activeNewSessionMutation: null,
    pendingLocalConfirmation: null,
    configDirty: false,
    configDraftValues: new Map(),
    configDraftBaselineValues: new Map(),
    configDraftTarget: null,
    configDraftRevision: 0n,
    nextConfigMutationGeneration: 1n,
    activeConfigMutationGeneration: null,
    lastFocusedOverlay: "none",
    settingsActionFocusContinuation: null,
    titlebarMenuFocusContinuation: null,
    mainRunFocusContinuation: null,
    sideChatFocusContinuation: null,
    quickChatDeleteFocusContinuation: null,
    sideChatFocusInteractionGeneration: 0n,
    refreshPromptFocusInteractionGeneration: 0n,
    pendingRefreshPromptFocus: null,
    focusPromptAfterRender: false,
    initialPromptFocusDone: false,
    artifactPaneCollapsed: typeof window !== "undefined"
      && window.localStorage.getItem("moyai.artifactPaneCollapsed") === "true",
    artifactPaneFocusAfterRender: null,
    artifactPaneMode: "output",
    selectedAgentPath: null,
    agentPaneOwnerIdentity: "",
    focusSelectedAgentAfterRender: false,
    agentPaneFocusAfterRender: null,
    agentExecutionCache: new Map(),
    agentExecutionTransaction: createAsyncTransactionSlot(),
    agentExecutionPrependContinuation: null,
    sideChatDrafts: new Map(),
    sideChatMutations: new Map(),
    sideChatCatalogs: new Map(),
    sideChatCatalogTransaction: createAsyncTransactionSlot(),
    sideChatCatalogIdentityRevision: 0,
    providerCatalogTransaction: createAsyncTransactionSlot(),
    providerCatalogRevision: null,
    doclingReadinessTransaction: createAsyncTransactionSlot(),
    rejectedProviderCatalogRequest: null,
    sideChatDeleteConfirmation: null,
    attachmentTrayOpen: false,
    attachmentFocusContinuation: null,
    permissionDecision: null,
    nextPermissionSubmissionId: 1,
    localConfirmationDecisionPending: false,
    localConfirmationDecisionError: "",
    recoverableError: null,
    recoverableErrorOwner: null,
    windowMaximized: false,
  };
}

export function doclingReadinessRequestPending(
  uiState: Pick<UiLocalState, "doclingReadinessTransaction">,
): boolean {
  return uiState.doclingReadinessTransaction.active !== null;
}

export function beginDoclingReadinessRequest(
  uiState: UiLocalState,
  expectedTarget: ConfigMutationTarget,
): DoclingReadinessRequest | null {
  return beginAsyncTransaction(
    uiState.doclingReadinessTransaction,
    expectedTarget,
    "single-flight",
    (token, target) => ({ token, expectedTarget: target }),
  );
}

export function finishDoclingReadinessRequest(
  uiState: UiLocalState,
  request: DoclingReadinessRequest,
): boolean {
  return clearAsyncTransaction(uiState.doclingReadinessTransaction, request);
}

export function setArtifactPaneCollapsed(uiState: UiLocalState, collapsed: boolean): void {
  uiState.artifactPaneCollapsed = collapsed;
  if (typeof window !== "undefined") {
    window.localStorage.setItem("moyai.artifactPaneCollapsed", String(collapsed));
  }
}

type SideChatOwnerState = Pick<
  DesktopWebState,
  | "draft_target"
  | "provider_base_url"
  | "provider_effective_base_url"
  | "provider_effective_profile"
  | "side_chat"
>;

export function sideChatOwnerSessionId(state: SideChatOwnerState): string | null {
  const selectedOwner = state.draft_target.sessionId;
  const projectedOwner = state.side_chat.owner_session_id;
  if (selectedOwner === null || (projectedOwner !== null && projectedOwner !== selectedOwner)) {
    return null;
  }
  return projectedOwner ?? selectedOwner;
}

export function sideChatDeleteConfirmationStillTargets(
  confirmation: SideChatDeleteConfirmation | null,
  state: SideChatOwnerState,
): boolean {
  return confirmation !== null
    && !state.side_chat.deleting
    && confirmation.ownerSessionId === sideChatOwnerSessionId(state)
    && confirmation.chatId === state.side_chat.chat_id
    && confirmation.expectedGeneration === state.side_chat.generation;
}

export function sideChatDraftForState(
  uiState: UiLocalState,
  state: SideChatOwnerState,
): SideChatLocalDraft | null {
  const ownerSessionId = sideChatOwnerSessionId(state);
  if (!ownerSessionId) return null;
  const chatId = state.side_chat.chat_id;
  const existing = uiState.sideChatDrafts.get(ownerSessionId);
  if (existing && existing.chatId === chatId) {
    const locallyDirty = sideChatDraftIsDirty(existing);
    if (
      !locallyDirty
      && !existing.saveInFlight
      && existing.persistedRevision !== state.side_chat.draft_revision
    ) {
      const projectedQuote = sideChatPendingQuoteFromProjection(state.side_chat.draft_quote);
      existing.text = state.side_chat.draft_text;
      existing.pendingQuote = projectedQuote;
      existing.persistedText = state.side_chat.draft_text;
      existing.persistedQuote = projectedQuote;
      existing.persistedRevision = state.side_chat.draft_revision;
    }
    return existing;
  }
  const projectedQuote = sideChatPendingQuoteFromProjection(state.side_chat.draft_quote);
  const draft: SideChatLocalDraft = {
    chatId,
    text: state.side_chat.draft_text,
    revision: 0,
    persistedText: state.side_chat.draft_text,
    persistedQuote: projectedQuote,
    persistedRevision: state.side_chat.draft_revision,
    saveTimer: null,
    saveInFlight: false,
    savePromise: null,
    saveQueued: false,
    pendingQuote: projectedQuote,
  };
  uiState.sideChatDrafts.set(ownerSessionId, draft);
  return draft;
}

export function sideChatPendingQuoteFromProjection(
  quote: SideChatDraftQuoteProjection | null,
): SideChatPendingQuote | null {
  return quote === null ? null : {
    sourceKind: quote.source_kind,
    sourceHistoryItemId: quote.source_history_item_id,
    sourceAppendPosition: quote.source_append_position,
    selectedText: quote.selected_text,
  };
}

export function sameSideChatPendingQuote(
  left: SideChatPendingQuote | null,
  right: SideChatPendingQuote | null,
): boolean {
  return left === right || (
    left !== null
    && right !== null
    && left.sourceKind === right.sourceKind
    && left.sourceHistoryItemId === right.sourceHistoryItemId
    && left.sourceAppendPosition === right.sourceAppendPosition
    && left.selectedText === right.selectedText
  );
}

export function sideChatDraftIsDirty(draft: SideChatLocalDraft): boolean {
  return draft.text !== draft.persistedText
    || !sameSideChatPendingQuote(draft.pendingQuote, draft.persistedQuote);
}

export function appendQuoteToSideChatDraft(
  draft: SideChatLocalDraft,
  quote: SideChatPendingQuote,
): void {
  draft.text = replaceReadableSideChatQuote(
    draft.text,
    draft.pendingQuote?.selectedText ?? null,
    quote.selectedText,
  );
  draft.pendingQuote = quote;
  draft.revision += 1;
}

export function updateSideChatDraftFromManualEdit(
  draft: SideChatLocalDraft,
  text: string,
): void {
  draft.text = text;
  draft.pendingQuote = null;
  draft.revision += 1;
}

export function sideChatMutationPending(
  uiState: UiLocalState,
  ownerSessionId: string | null,
): boolean {
  return ownerSessionId !== null && uiState.sideChatMutations.has(ownerSessionId);
}

export function sideChatOperationsOpen(
  uiState: Pick<UiLocalState, "activeConfigMutationGeneration" | "externalConfigMutationPending">,
): boolean {
  // Conversation mutations and global Side model discovery share the config transaction fence.
  return uiState.activeConfigMutationGeneration === null
    && !uiState.externalConfigMutationPending;
}

export function canonicalSideChatProviderBaseUrl(input: string): string {
  return validateProviderBaseUrl(input).canonicalBaseUrl;
}

export function canonicalSideChatCatalogBaseUrl(input: string): string {
  const providerBaseUrl = canonicalSideChatProviderBaseUrl(input);
  if (!providerBaseUrl) return "";
  const url = new URL(providerBaseUrl);
  const path = url.pathname.replace(/\/+$/, "");
  const catalogPath = path.endsWith("/v1") ? path.slice(0, -3) : path;
  url.pathname = catalogPath || "/";
  return url.toString().replace(/\/+$/, "");
}

export function sideChatCatalogUrlValid(input: string): boolean {
  return canonicalSideChatCatalogBaseUrl(input).length > 0;
}

export function sideChatCatalogKey(
  target: ConfigMutationTarget,
  baseUrl: string,
  providerProfile: ProviderProfile,
): string {
  return [
    target.workspacePath,
    target.sessionId ?? "",
    target.configGeneration,
    canonicalSideChatCatalogBaseUrl(baseUrl),
    providerProfile,
  ].join("\u0000");
}

export function sideChatCatalogViewForState(
  uiState: UiLocalState,
  state: DesktopWebState,
): SideChatCatalogView {
  const settings = globalSideChatCatalogSettings(uiState, state);
  if (!settings) return emptySideChatCatalogView();
  const baseUrl = canonicalSideChatCatalogBaseUrl(settings.baseUrl);
  const { providerProfile } = settings;
  const key = sideChatCatalogKey(state.config_target, baseUrl, providerProfile);
  const local = uiState.sideChatCatalogs.get(key);
  if (
    local
    && sameConfigMutationTarget(local.configTarget, state.config_target)
    && local.identityRevision === uiState.sideChatCatalogIdentityRevision
    && local.baseUrl === baseUrl
    && local.providerProfile === providerProfile
    && local.configGeneration === state.config_target.configGeneration
  ) {
    return {
      status: local.status,
      source: "global",
      baseUrl,
      models: local.models,
      error: local.error,
    };
  }
  const seeded = mainProviderCatalogSeed(state, baseUrl, providerProfile);
  if (seeded.length > 0) {
    return {
      status: "ready",
      source: "main",
      baseUrl,
      models: seeded,
      error: "",
    };
  }
  return {
    ...emptySideChatCatalogView(),
    baseUrl,
  };
}

export function sessionSettingsDraftFromProjection(
  projection: DesktopWebState["session_settings"],
): SessionSettingsDraft {
  return {
    baseUrl: projection.base_url,
    model: projection.model,
    providerProfile: projection.provider_profile,
    apiKeyEnv: projection.api_key_env,
    contextWindow: projection.context_window,
    accessMode: projection.access_mode,
  };
}

export interface SessionSettingsMutationAvailability {
  readonly enabled: boolean;
  readonly staleTarget: boolean;
  readonly providerChanged: boolean;
  readonly accessChanged: boolean;
  readonly reason: string;
}

/**
 * Combines the browser-owned draft with Rust's semantic mutation lanes.
 * During an active tree, Rust may deliberately keep only the access lane open;
 * provider/model/limit edits must remain visible but cannot be submitted.
 */
export function sessionSettingsMutationAvailability(
  local: SessionSettingsState,
  projection: DesktopWebState["session_settings"],
): SessionSettingsMutationAvailability {
  const draft = local.draft;
  const baseline = local.baseline;
  if (!projection.available || projection.target === null || !draft || !baseline) {
    return {
      enabled: false,
      staleTarget: false,
      providerChanged: false,
      accessChanged: false,
      reason: projection.unavailable_reason || "root sessionを選択すると変更できます。",
    };
  }
  if (!sameSessionSettingsTarget(local.owner, projection.target)) {
    return {
      enabled: false,
      staleTarget: true,
      providerChanged: false,
      accessChanged: false,
      reason: "保存済みSession Settingsが別の操作で更新されました。変更を破棄するか、panelを開き直してください。",
    };
  }
  const providerChanged = draft.baseUrl !== baseline.baseUrl
    || draft.model !== baseline.model
    || draft.providerProfile !== baseline.providerProfile
    || draft.apiKeyEnv !== baseline.apiKeyEnv
    || draft.contextWindow !== baseline.contextWindow;
  const accessChanged = draft.accessMode !== baseline.accessMode;
  if (providerChanged && !projection.provider_mutation_enabled) {
    return {
      enabled: false,
      staleTarget: false,
      providerChanged,
      accessChanged,
      reason: "実行中はProvider・Model・moyAI local context budgetを変更できません。Access modeだけを適用できます。",
    };
  }
  if (accessChanged && !projection.access_mutation_enabled) {
    return {
      enabled: false,
      staleTarget: false,
      providerChanged,
      accessChanged,
      reason: "現在のruntime ownerではAccess modeを変更できません。",
    };
  }
  if (!local.dirty) {
    return {
      enabled: false,
      staleTarget: false,
      providerChanged,
      accessChanged,
      reason: "未適用の変更はありません。",
    };
  }
  if (local.validation?.ok !== true) {
    return {
      enabled: false,
      staleTarget: false,
      providerChanged,
      accessChanged,
      reason: local.validation?.message || "入力内容を確認してください。",
    };
  }
  if (!sessionSettingsApplyEnabled(local)) {
    return {
      enabled: false,
      staleTarget: false,
      providerChanged,
      accessChanged,
      reason: local.activeMutation ? "Session Settingsを適用しています…" : "現在は適用できません。",
    };
  }
  return {
    enabled: true,
    staleTarget: false,
    providerChanged,
    accessChanged,
    reason: "このroot sessionへ適用できます。",
  };
}

export function sideChatModelOptions(
  catalog: SideChatCatalogView,
  currentModel: string,
): SideChatModelOption[] {
  const current = currentModel.trim();
  const seen = new Set<string>();
  const options: SideChatModelOption[] = catalog.models.flatMap((model): SideChatModelOption[] => {
    const id = model.id.trim();
    if (!id || seen.has(id)) return [];
    seen.add(id);
    return [{
      id,
      label: model.label.trim() || id,
      loadState: model.loadState,
      currentOnly: false,
    } satisfies SideChatModelOption];
  });
  if (current && !seen.has(current)) {
    options.unshift({
      id: current,
      label: `${current}（現在の設定）`,
      loadState: "unknown",
      currentOnly: true,
    });
  }
  return options;
}

export function sideChatModelOptionLabel(option: SideChatModelOption): string {
  if (option.currentOnly) return option.label;
  if (option.loadState === "loaded") return `${option.label}（ロード済み）`;
  if (option.loadState === "not_loaded") return `${option.label}（未ロード）`;
  return option.label;
}

export function sideChatCatalogLoadOpen(
  uiState: UiLocalState,
  state: DesktopWebState,
): boolean {
  const settings = globalSideChatCatalogSettings(uiState, state);
  const configCapability = uiState.configDirty
    ? state.config_draft_capabilities.dirty
    : state.config_draft_capabilities.clean;
  if (
    state.overlay !== "config"
    || !configCapability.edit_enabled
    || !settings
    || !sideChatOperationsOpen(uiState)
    || !sideChatCatalogUrlValid(settings.baseUrl)
  ) return false;
  return sideChatCatalogViewForState(uiState, state).status !== "loading";
}

export function beginSideChatCatalogLoad(
  uiState: UiLocalState,
  state: DesktopWebState,
): SideChatCatalogRequest | null {
  if (!sideChatCatalogLoadOpen(uiState, state)) return null;
  const superseded = uiState.sideChatCatalogTransaction.active;
  if (superseded) deleteLoadingSideChatCatalogEntry(uiState, superseded);
  const settings = globalSideChatCatalogSettings(uiState, state);
  if (!settings) return null;
  const baseUrl = canonicalSideChatCatalogBaseUrl(settings.baseUrl);
  const key = sideChatCatalogKey(state.config_target, baseUrl, settings.providerProfile);
  const request = beginAsyncTransaction(uiState.sideChatCatalogTransaction, {
    key,
    configTarget: { ...state.config_target },
    identityRevision: uiState.sideChatCatalogIdentityRevision,
    baseUrl,
    providerProfile: settings.providerProfile,
    configGeneration: state.config_target.configGeneration,
  } satisfies SideChatCatalogTarget, "supersede", (token, target) => ({ token, ...target }));
  const previous = sideChatCatalogViewForState(uiState, state);
  uiState.sideChatCatalogs.set(key, {
    configTarget: { ...request.configTarget },
    identityRevision: request.identityRevision,
    baseUrl,
    providerProfile: request.providerProfile,
    configGeneration: request.configGeneration,
    models: previous.models,
    status: "loading",
    error: "",
    requestToken: request.token,
  });
  return request;
}

export function finishSideChatCatalogLoad(
  uiState: UiLocalState,
  state: DesktopWebState | null,
  request: SideChatCatalogRequest,
  result: SideChatCatalogResult,
): SideChatCatalogSettlement {
  if (!sideChatCatalogRequestIsCurrent(uiState, request)) {
    return { catalogAccepted: false, localStateChanged: false };
  }
  clearAsyncTransaction(uiState.sideChatCatalogTransaction, request);
  if (!state || !sideChatCatalogRequestStillTargets(uiState, state, request)) {
    rejectStaleSideChatCatalogEntry(uiState, request);
    return { catalogAccepted: false, localStateChanged: true };
  }
  const responseMatches = canonicalSideChatCatalogBaseUrl(result.baseUrl) === request.baseUrl
    && result.providerProfile === request.providerProfile
    && result.configGeneration === request.configGeneration;
  if (!responseMatches) {
    uiState.sideChatCatalogs.set(request.key, {
      configTarget: { ...request.configTarget },
      identityRevision: request.identityRevision,
      baseUrl: request.baseUrl,
      providerProfile: request.providerProfile,
      configGeneration: request.configGeneration,
      models: [],
      status: "error",
      error: "モデル一覧の応答対象が、現在のGlobal Side Chat設定と一致しませんでした。",
      requestToken: request.token,
    });
    return { catalogAccepted: false, localStateChanged: true };
  }
  uiState.sideChatCatalogs.set(request.key, {
    configTarget: { ...request.configTarget },
    identityRevision: request.identityRevision,
    baseUrl: request.baseUrl,
    providerProfile: request.providerProfile,
    configGeneration: request.configGeneration,
    models: result.models,
    status: "ready",
    error: "",
    requestToken: request.token,
  });
  return { catalogAccepted: true, localStateChanged: true };
}

export function failSideChatCatalogLoad(
  uiState: UiLocalState,
  state: DesktopWebState | null,
  request: SideChatCatalogRequest,
  error: string,
): SideChatCatalogSettlement {
  if (!sideChatCatalogRequestIsCurrent(uiState, request)) {
    return { catalogAccepted: false, localStateChanged: false };
  }
  clearAsyncTransaction(uiState.sideChatCatalogTransaction, request);
  if (!state || !sideChatCatalogRequestStillTargets(uiState, state, request)) {
    rejectStaleSideChatCatalogEntry(uiState, request);
    return { catalogAccepted: false, localStateChanged: true };
  }
  const previous = uiState.sideChatCatalogs.get(request.key);
  uiState.sideChatCatalogs.set(request.key, {
    configTarget: { ...request.configTarget },
    identityRevision: request.identityRevision,
    baseUrl: request.baseUrl,
    providerProfile: request.providerProfile,
    configGeneration: request.configGeneration,
    models: previous?.models ?? [],
    status: "error",
    error: error.trim() || "モデル一覧を読み込めませんでした。",
    requestToken: request.token,
  });
  return { catalogAccepted: false, localStateChanged: true };
}

function sideChatCatalogRequestIsCurrent(
  uiState: UiLocalState,
  request: SideChatCatalogRequest,
): boolean {
  return asyncTransactionIsCurrent(uiState.sideChatCatalogTransaction, request);
}

function sideChatCatalogRequestStillTargets(
  uiState: UiLocalState,
  state: DesktopWebState,
  request: SideChatCatalogRequest,
): boolean {
  const settings = globalSideChatCatalogSettings(uiState, state);
  return sideChatOperationsOpen(uiState)
    && sameConfigMutationTarget(request.configTarget, state.config_target)
    && request.identityRevision === uiState.sideChatCatalogIdentityRevision
    && settings !== null
    && canonicalSideChatCatalogBaseUrl(settings.baseUrl) === request.baseUrl
    && settings.providerProfile === request.providerProfile;
}

function deleteLoadingSideChatCatalogEntry(
  uiState: UiLocalState,
  request: SideChatCatalogRequest,
): void {
  const entry = uiState.sideChatCatalogs.get(request.key);
  if (entry?.requestToken === request.token && entry.status === "loading") {
    uiState.sideChatCatalogs.delete(request.key);
  }
}

function rejectStaleSideChatCatalogEntry(
  uiState: UiLocalState,
  request: SideChatCatalogRequest,
): void {
  uiState.sideChatCatalogs.set(request.key, {
    configTarget: { ...request.configTarget },
    identityRevision: request.identityRevision,
    baseUrl: request.baseUrl,
    providerProfile: request.providerProfile,
    configGeneration: request.configGeneration,
    models: [],
    status: "error",
    error: "モデル一覧の読込中にGlobal Side Chat設定が変更されました。現在の設定で、もう一度モデル一覧を読み込んでください。",
    requestToken: request.token,
  });
}

function mainProviderCatalogSeed(
  state: DesktopWebState,
  baseUrl: string,
  providerProfile: ProviderProfile,
): SideChatCatalogModel[] {
  if (
    !state.provider_catalog_base_url
    || canonicalSideChatCatalogBaseUrl(state.provider_catalog_base_url) !== baseUrl
    || state.provider_catalog_profile !== providerProfile
    || state.provider_effective_profile !== providerProfile
  ) return [];
  return state.provider_model_ids.flatMap((id, index) => {
    const modelId = id.trim();
    if (!modelId) return [];
    return [{
      id: modelId,
      label: state.provider_models[index]?.trim() || modelId,
      loadState: "unknown" as const,
    }];
  });
}

function emptySideChatCatalogView(): SideChatCatalogView {
  return {
    status: "idle",
    source: "none",
    baseUrl: "",
    models: [],
    error: "",
  };
}

function globalSideChatCatalogSettings(
  uiState: UiLocalState,
  state: Pick<DesktopWebState, "config_fields" | "config_target">,
): { baseUrl: string; providerProfile: ProviderProfile; model: string } | null {
  const draftApplies = configDraftAppliesTo(uiState, state.config_target);
  const value = (key: string): string | null => {
    const projected = state.config_fields.find((field) => field.key === key)?.value;
    if (projected === undefined) return null;
    return draftApplies ? (uiState.configDraftValues.get(key) ?? projected) : projected;
  };
  const baseUrl = value("side_chat.base_url");
  const model = value("side_chat.model");
  const providerProfile = value("side_chat.provider_profile");
  if (baseUrl === null || model === null || !isProviderProfile(providerProfile)) return null;
  return { baseUrl, model, providerProfile };
}

function isProviderProfile(value: string | null): value is ProviderProfile {
  return value === "lm_studio"
    || value === "openai_compatible"
    || value === "openai_responses"
    || value === "lm_studio_chat_completions";
}

export function recordSideChatCatalogConfigEdit(
  uiState: Pick<UiLocalState, "sideChatCatalogIdentityRevision">,
  key: string,
  previous: string,
  next: string,
): void {
  const identityChanged = key === "side_chat.base_url"
    ? canonicalSideChatCatalogBaseUrl(previous) !== canonicalSideChatCatalogBaseUrl(next)
    : key === "side_chat.provider_profile" && previous !== next;
  if (identityChanged) uiState.sideChatCatalogIdentityRevision += 1;
}

export function openSideChatPane(
  uiState: UiLocalState,
  state: SideChatOwnerState,
): boolean {
  if (!sideChatOwnerSessionId(state)) return false;
  uiState.artifactPaneMode = "side_chat";
  uiState.selectedAgentPath = null;
  uiState.focusSelectedAgentAfterRender = false;
  uiState.agentPaneFocusAfterRender = null;
  uiState.agentExecutionTransaction.active = null;
  setArtifactPaneCollapsed(uiState, false);
  uiState.artifactPaneFocusAfterRender = "content";
  return true;
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
    uiState.agentExecutionTransaction.active = null;
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
    uiState.agentExecutionTransaction.active = null;
  }
  if (state.agent_activity_rows.length === 0 && uiState.artifactPaneMode === "agents") {
    uiState.artifactPaneMode = "output";
    uiState.selectedAgentPath = null;
    uiState.focusSelectedAgentAfterRender = false;
    uiState.agentPaneFocusAfterRender = null;
    uiState.agentExecutionTransaction.active = null;
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
  uiState.agentExecutionTransaction.active = null;
}

export function showAgentList(uiState: UiLocalState): void {
  uiState.artifactPaneMode = "agents";
  uiState.selectedAgentPath = null;
  uiState.focusSelectedAgentAfterRender = false;
  uiState.agentPaneFocusAfterRender = "agent-pane-back";
  uiState.agentExecutionTransaction.active = null;
}

export function beginAgentExecutionLoad(
  uiState: UiLocalState,
  state: Pick<DesktopWebState, "workspace_path" | "draft_target">,
  row: AgentActivityRow,
): AgentExecutionRequest {
  const rootSessionId = state.draft_target.sessionId ?? "";
  const expectedTarget = Object.freeze({
    workspacePath: state.workspace_path,
    rootSessionId,
    agentPath: row.agent_path,
    childSessionId: row.session_id,
  } satisfies AgentExecutionExpectedTarget);
  const cacheKey = agentExecutionCacheKey(expectedTarget);
  const request = beginAsyncTransaction(uiState.agentExecutionTransaction, {
    cacheKey,
    ownerIdentity: agentPaneOwnerIdentity(state),
    expectedTarget,
    activityIdentity: agentActivityRowIdentity(row),
    operation: "replace",
    expectedOffset: null,
    expectedEnd: null,
  } satisfies AgentExecutionTarget, "supersede", (generation, target) => ({ generation, ...target }));
  const cached = uiState.agentExecutionCache.get(cacheKey);
  uiState.agentExecutionCache.set(cacheKey, {
    status: "loading",
    generation: request.generation,
    expectedTarget,
    projection: cached?.projection ?? null,
    error: "",
  });
  return request;
}

export function beginPreviousAgentExecutionPageLoad(
  uiState: UiLocalState,
  state: Pick<DesktopWebState, "workspace_path" | "draft_target">,
  row: AgentActivityRow,
): AgentExecutionRequest | null {
  const rootSessionId = state.draft_target.sessionId ?? "";
  const expectedTarget = Object.freeze({
    workspacePath: state.workspace_path,
    rootSessionId,
    agentPath: row.agent_path,
    childSessionId: row.session_id,
  } satisfies AgentExecutionExpectedTarget);
  const cacheKey = agentExecutionCacheKey(expectedTarget);
  const cached = uiState.agentExecutionCache.get(cacheKey);
  const expectedOffset = cached?.projection?.turn_page_offset ?? 0;
  const expectedEnd = cached?.projection?.turn_page_end ?? 0;
  if (expectedOffset <= 0 || expectedEnd <= expectedOffset) return null;

  const request = beginAsyncTransaction(uiState.agentExecutionTransaction, {
    cacheKey,
    ownerIdentity: agentPaneOwnerIdentity(state),
    expectedTarget,
    activityIdentity: agentActivityRowIdentity(row),
    operation: "prepend",
    expectedOffset,
    expectedEnd,
  } satisfies AgentExecutionTarget, "single-flight", (generation, target) => ({ generation, ...target }));
  if (!request) return null;
  uiState.agentExecutionCache.set(cacheKey, {
    status: "loading",
    generation: request.generation,
    expectedTarget,
    projection: cached?.projection ?? null,
    error: "",
  });
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
    clearAsyncTransaction(uiState.agentExecutionTransaction, request);
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
      clearAsyncTransaction(uiState.agentExecutionTransaction, request);
      return true;
    }
    uiState.agentExecutionCache.set(request.cacheKey, {
      status: "ready",
      generation: request.generation,
      expectedTarget: request.expectedTarget,
      projection,
      error: "",
    });
    clearAsyncTransaction(uiState.agentExecutionTransaction, request);
    return true;
  }
  uiState.agentExecutionCache.set(request.cacheKey, {
    status: "ready",
    generation: request.generation,
    expectedTarget: request.expectedTarget,
    projection,
    error: "",
  });
  clearAsyncTransaction(uiState.agentExecutionTransaction, request);
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
  clearAsyncTransaction(uiState.agentExecutionTransaction, request);
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
  return asyncTransactionIsCurrent(uiState.agentExecutionTransaction, request)
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

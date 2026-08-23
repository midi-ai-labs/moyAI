import {
  configDraftAppliesTo,
  configMutationPending,
  reconcileConfigDraftTarget,
  type ConfigValueInput,
} from "./config_mutation.ts";
import {
  beginAsyncTransaction,
  clearAsyncTransaction,
} from "./async_transaction.ts";
import type {
  ConfigMutationTarget,
  ConfigDraftCapabilityProjection,
  DesktopWebState,
  DesktopViewState,
  DraftActionTarget,
  PromptReviewMutationTarget,
  ProviderProfile,
  RunExpectedState,
  SessionSearchTarget,
} from "./types.ts";
import type {
  ProviderCatalogRequest,
  ProviderCatalogTarget,
  ProviderDraft,
  UiLocalState,
} from "./ui_state.ts";
import { mutationStartsNewSession } from "./new_session_mutation.ts";
import { taskActivityStateForView } from "./task_activity_indicator.ts";
import { validateConfigFieldValues, validateProviderBaseUrl } from "./utils.ts";
import {
  snapshotDraftActionTarget,
  snapshotPromptReviewMutationTarget,
} from "./composer_target_contract.ts";

const COMPOSER_INVALIDATING_MUTATIONS = new Set([
  "submit_prompt",
  "review_uncommitted",
  "send_prompt_review",
]);

const PROVIDER_COMMIT_MUTATIONS = new Set([
  "load_provider_models",
  "apply_provider_session",
  "save_provider_global",
]);

const RUN_START_MUTATIONS = new Set([
  "submit_prompt",
  "review_uncommitted",
  "send_prompt_review",
]);

const RUN_OWNER_CONSUMER_MUTATIONS = new Set([
  ...RUN_START_MUTATIONS,
  "enhance_prompt",
]);

const EXTERNAL_CONFIG_OWNER_MUTATIONS = new Set([
  "toggle_access_mode",
  "apply_provider_session",
  "save_provider_global",
]);

export function mutationStartsRun(mutationName: string): boolean {
  return RUN_START_MUTATIONS.has(mutationName);
}

export function mutationConsumesRunOwner(mutationName: string): boolean {
  return RUN_OWNER_CONSUMER_MUTATIONS.has(mutationName);
}

export function mutationChangesConfigOwner(mutationName: string): boolean {
  return EXTERNAL_CONFIG_OWNER_MUTATIONS.has(mutationName);
}

export function runOwnerMutationOpen(
  uiState: Pick<
    UiLocalState,
    "runStartMutationPending" | "externalConfigMutationPending" | "activeNewSessionMutation"
  >,
): boolean {
  return !uiState.runStartMutationPending
    && !uiState.externalConfigMutationPending
    && uiState.activeNewSessionMutation === null;
}

export function mutationAdmissionOpen(
  uiState: UiLocalState,
  mutationName: string,
): boolean {
  if (uiState.activeNewSessionMutation !== null) return false;
  if (mutationStartsNewSession(mutationName)) {
    return runOwnerMutationOpen(uiState) && configOwnerMutationOpen(uiState);
  }
  if (mutationConsumesRunOwner(mutationName)) return runOwnerMutationOpen(uiState);
  if (mutationChangesConfigOwner(mutationName)) return configOwnerMutationOpen(uiState);
  return true;
}

export interface UiCapabilities {
  canSubmit: boolean;
  canEnhance: boolean;
  canReviewUncommitted: boolean;
  canUseImageInput: boolean;
  canSendEnhancedReview: boolean;
  canSendRawReview: boolean;
  canLoadProviderModels: boolean;
  canApplyProvider: boolean;
}

export function configOwnerMutationOpen(uiState: UiLocalState): boolean {
  return !configMutationPending(uiState)
    && !uiState.externalConfigMutationPending
    && !uiState.runStartMutationPending
    && uiState.activeNewSessionMutation === null;
}

export function configDraftEditOpen(uiState: UiLocalState): boolean {
  return !configMutationPending(uiState)
    && !uiState.externalConfigMutationPending;
}

export function configDraftDiscardOpen(uiState: UiLocalState): boolean {
  return uiState.configDirty
    && configDraftEditOpen(uiState);
}

export function configDraftCommitOpen(
  uiState: UiLocalState,
  initialSetupRequired: boolean,
): boolean {
  return configDraftEditOpen(uiState)
    && !uiState.runStartMutationPending
    && (uiState.configDirty || initialSetupRequired);
}

export function composerCapabilities(
  state: Pick<DesktopWebState, "can_submit" | "enhance_enabled">,
  prompt: string,
): Pick<UiCapabilities, "canSubmit" | "canEnhance" | "canReviewUncommitted"> {
  const hasPrompt = prompt.trim().length > 0;
  return {
    canSubmit: state.can_submit && hasPrompt,
    canEnhance: state.enhance_enabled && hasPrompt,
    canReviewUncommitted: state.can_submit && hasPrompt,
  };
}

export function providerCapabilities(
  state: Pick<
    DesktopViewState,
    | "provider_loading"
    | "provider_base_url"
    | "provider_profile"
    | "provider_api_key_env"
    | "provider_catalog_base_url"
    | "provider_catalog_profile"
    | "provider_catalog_api_key_env"
    | "provider_context_window"
    | "provider_max_output_tokens"
    | "provider_selected_index"
    | "provider_apply_enabled"
    | "config_draft"
  >,
  options: {
    currentProviderLimitDraftDirty?: boolean;
  } = {},
): Pick<UiCapabilities, "canLoadProviderModels" | "canApplyProvider"> {
  const urlValid = validateProviderBaseUrl(state.provider_base_url).ok;
  const limitsValid = positiveInteger(state.provider_context_window)
    && positiveInteger(state.provider_max_output_tokens);
  return {
    canLoadProviderModels: !state.provider_loading && urlValid && limitsValid,
    canApplyProvider: state.config_draft.external_owner_mutation_open
      && !state.provider_loading
      && urlValid
      && limitsValid
      && state.provider_selected_index >= 0
      && (state.provider_apply_enabled || options.currentProviderLimitDraftDirty === true),
  };
}

export function normalizeProviderBaseUrl(input: string): string {
  const canonical = validateProviderBaseUrl(input).canonicalBaseUrl;
  return canonical.endsWith("/v1") && canonical.length > 3
    ? canonical.slice(0, -3)
    : canonical;
}

function normalizeApiKeyEnv(input: string): string | null {
  const value = input.trim();
  return value.length > 0 ? value : null;
}

function isProviderProfile(value: string | undefined): value is ProviderProfile {
  return value === "lm_studio"
    || value === "openai_compatible"
    || value === "openai_responses"
    || value === "lm_studio_chat_completions";
}

export interface DraftMutationSnapshot {
  runStart?: boolean;
  runSettlement?: "pending" | "accepted" | "rejected";
  composerRevision?: number;
  composerOwner?: string;
  imageRevision?: number;
  workspaceRevision?: number;
  reviewRevision?: number;
  reviewTarget?: PromptReviewMutationTarget | null;
  providerRevision?: number;
  providerCatalogRequestToken?: number;
}

export function captureDraftMutation(
  uiState: UiLocalState,
  mutationName: string,
): DraftMutationSnapshot | null {
  const drafts = uiState.drafts;
  const snapshot: DraftMutationSnapshot = {};
  if (COMPOSER_INVALIDATING_MUTATIONS.has(mutationName)) {
    snapshot.composerRevision = drafts.composerRevision;
    snapshot.composerOwner = drafts.composerOwner;
    snapshot.imageRevision = drafts.imageRevision;
  }
  if (mutationName === "attach_image" || mutationName === "browse_image" || mutationName === "clear_images") {
    snapshot.imageRevision = drafts.imageRevision;
  }
  if (mutationName === "switch_workspace" || mutationName === "open_typed_path") snapshot.workspaceRevision = drafts.workspaceRevision;
  if (mutationName === "send_prompt_review" || mutationName === "cancel_prompt_review") {
    snapshot.composerOwner = drafts.composerOwner;
    snapshot.reviewRevision = drafts.reviewRevision;
    snapshot.reviewTarget = snapshotPromptReviewTarget(drafts.reviewTarget);
  }
  if (PROVIDER_COMMIT_MUTATIONS.has(mutationName)) snapshot.providerRevision = drafts.providerRevision;
  if (mutationName === "load_provider_models") {
    snapshot.providerCatalogRequestToken = uiState.providerCatalogTransaction.active?.token;
  }
  if (RUN_START_MUTATIONS.has(mutationName)) {
    snapshot.runStart = true;
    snapshot.runSettlement = "pending";
    drafts.pendingRunSubmission = {
      owner: drafts.composerOwner,
      workspacePath: drafts.composerOwner.slice(0, drafts.composerOwner.indexOf("\u0000")),
      composerRevision: drafts.composerRevision,
      imageRevision: drafts.imageRevision,
      reviewTarget: snapshot.reviewTarget ?? null,
      reviewRevision: snapshot.reviewRevision ?? null,
      baseCommitGeneration: drafts.composerCommitGeneration,
      commandAccepted: false,
    };
  }
  return Object.keys(snapshot).length > 0 ? snapshot : null;
}

export function rejectDraftMutation(
  uiState: UiLocalState,
  mutationName: string,
  snapshot: DraftMutationSnapshot | null,
): void {
  const providerCatalogRequest = uiState.providerCatalogTransaction.active;
  if (
    mutationName === "load_provider_models"
    && providerCatalogRequest !== null
    && snapshot?.providerCatalogRequestToken !== undefined
    && providerCatalogRequest.token === snapshot.providerCatalogRequestToken
  ) {
    clearAsyncTransaction(uiState.providerCatalogTransaction, providerCatalogRequest);
  }
  if (!RUN_START_MUTATIONS.has(mutationName) || !snapshot?.composerOwner) return;
  snapshot.runSettlement = "rejected";
  if (
    uiState.drafts.pendingRunSubmission?.owner === snapshot.composerOwner
    && samePromptReviewTarget(
      uiState.drafts.pendingRunSubmission.reviewTarget,
      snapshot.reviewTarget ?? null,
    )
  ) {
    uiState.drafts.pendingRunSubmission = null;
  }
}

export function acknowledgeDraftMutation(
  uiState: UiLocalState,
  state: DesktopWebState,
  mutationName: string,
  snapshot: DraftMutationSnapshot | null,
): void {
  if (!snapshot) return;
  const drafts = uiState.drafts;
  const providerCatalogRequest = uiState.providerCatalogTransaction.active;
  if (
    mutationName === "load_provider_models"
    && providerCatalogRequest !== null
    && snapshot.providerCatalogRequestToken !== undefined
    && providerCatalogRequest.token === snapshot.providerCatalogRequestToken
  ) {
    if (state.provider_loading) {
      providerCatalogRequest.admitted = true;
      uiState.rejectedProviderCatalogRequest = null;
    } else {
      const completionAccepted = providerCatalogRequestTargetsCurrentDraft(
        providerCatalogRequest,
        state,
        uiState,
      ) && providerCatalogResultTargetsRequest(providerCatalogRequest, state);
      uiState.rejectedProviderCatalogRequest = completionAccepted
        ? null
        : providerCatalogRequest;
      clearAsyncTransaction(uiState.providerCatalogTransaction, providerCatalogRequest);
    }
  }
  const startsRun = RUN_START_MUTATIONS.has(mutationName);
  if (startsRun) {
    const pending = drafts.pendingRunSubmission;
    if (
      pending
      && pending.owner === snapshot.composerOwner
      && pending.composerRevision === snapshot.composerRevision
      && pending.imageRevision === snapshot.imageRevision
      && samePromptReviewTarget(pending.reviewTarget, snapshot.reviewTarget ?? null)
    ) {
      pending.commandAccepted = true;
      snapshot.runSettlement = "accepted";
    }
  } else {
    if (snapshot.composerRevision === drafts.composerRevision) drafts.prompt = state.draft_prompt;
    if (snapshot.imageRevision === drafts.imageRevision) drafts.imageInput = state.image_input;
  }
  if (snapshot.workspaceRevision === drafts.workspaceRevision) drafts.workspaceInput = state.workspace_input;
  if (
    !startsRun
    && snapshot.reviewRevision === drafts.reviewRevision
    && snapshot.composerOwner === drafts.composerOwner
    && snapshot.composerOwner === composerOwner(state)
    && snapshot.reviewTarget !== undefined
    && samePromptReviewTarget(snapshot.reviewTarget, drafts.reviewTarget)
    && samePromptReviewTarget(snapshot.reviewTarget, state.review_target)
  ) {
    synchronizeReviewDraft(drafts, state.review_draft_text);
  }
  if (snapshot.providerRevision === drafts.providerRevision) hydrateProviderDraft(drafts.provider, state);
}

export function composerOwner(state: DesktopWebState): string {
  return `${state.draft_target.workspacePath}\u0000${state.draft_target.ownerGeneration}\u0000${state.draft_target.sessionId ?? "new"}`;
}

/**
 * Stable in-memory owner for an unsent composer draft.
 *
 * `ownerGeneration` intentionally remains part of `composerOwner` for command conflict
 * detection, but it cannot identify a local draft that the user expects to find again after
 * switching away from and back to the same durable session.
 */
export function composerSessionOwner(state: DesktopWebState): string {
  const selectedProjectId = state.project_rows[state.selected_project_index]?.project_id ?? "quick-chat";
  return `${state.draft_target.workspacePath}\u0000${selectedProjectId}\u0000${state.draft_target.sessionId ?? "new"}`;
}

export function sessionSearchOwner(state: DesktopWebState): string {
  const target = sessionSearchMutationTarget(state);
  return `${target.workspacePath}\u0000${target.projectId ?? "quick-chat"}`;
}

export function sessionSearchMutationTarget(state: DesktopWebState): SessionSearchTarget {
  return {
    workspacePath: state.workspace_path,
    projectId: state.project_rows[state.selected_project_index]?.project_id ?? null,
  };
}

export function localSearchOwner(state: DesktopWebState): string {
  return `${state.workspace_path}\u0000${composerOwner(state)}`;
}

export function providerOwner(state: DesktopWebState): string {
  const target = state.config_target;
  return `${target.workspacePath}\u0000${target.sessionId ?? "global"}\u0000${target.configGeneration}`;
}

export function beginProviderCatalogRequest(
  uiState: UiLocalState,
  state: DesktopWebState,
): ProviderCatalogRequest | null {
  return beginAsyncTransaction(uiState.providerCatalogTransaction, {
    providerOwner: providerOwner(state),
    providerRevision: uiState.drafts.providerCatalogIdentityRevision,
    baseUrl: normalizeProviderBaseUrl(uiState.drafts.provider.baseUrl),
    providerProfile: uiState.drafts.provider.providerProfile,
    apiKeyEnv: normalizeApiKeyEnv(uiState.drafts.provider.apiKeyEnv),
  } satisfies ProviderCatalogTarget, "single-flight", (token, target) => ({
    token,
    ...target,
    admitted: false,
  }));
}

/**
 * Adopts the complete Initial Setup config draft as the provider-catalog owner.
 * The local revision is advanced for every identity change so an A -> B -> A
 * edit cannot accidentally accept a response started for the first A.
 */
export function synchronizeInitialSetupProviderDraft(
  state: DesktopWebState,
  uiState: UiLocalState,
): boolean {
  if (!state.startup.initial_setup_required || state.overlay !== "initial_setup") return false;
  const next = providerDraftFromConfigFields(state.config_fields, uiState.drafts.provider);
  if (sameProviderDraft(next, uiState.drafts.provider)) return false;
  const catalogIdentityChanged = !sameProviderCatalogIdentity(next, uiState.drafts.provider);
  uiState.drafts.provider = next;
  uiState.drafts.providerRevision += 1;
  if (catalogIdentityChanged) uiState.drafts.providerCatalogIdentityRevision += 1;
  return true;
}

export function draftMutationTarget(state: DesktopWebState): DraftActionTarget {
  return snapshotDraftActionTarget(state.draft_target);
}

export function providerDraftPayload(
  draft: ProviderDraft,
  expectedTarget: ConfigMutationTarget,
  draftValues?: ConfigValueInput[],
): Record<string, unknown> {
  const payload: Record<string, unknown> = {
    input: {
      baseUrl: draft.baseUrl,
      providerProfile: draft.providerProfile,
      apiKeyEnv: draft.apiKeyEnv,
      contextWindow: draft.contextWindow,
      maxOutputTokens: draft.maxOutputTokens,
      selectedModelId: draft.selectedModelId,
    },
    expectedTarget,
  };
  if (draftValues) payload.draftValues = draftValues;
  return payload;
}

export function reconcileUiDrafts(
  uiState: UiLocalState,
  previous: DesktopWebState | null,
  state: DesktopWebState,
  mutationSnapshot: DraftMutationSnapshot | null = null,
): void {
  reconcileConfigDraftTarget(uiState, state.config_target);
  const drafts = uiState.drafts;
  const nextComposerOwner = composerOwner(state);
  const nextComposerSessionOwner = composerSessionOwner(state);
  const nextSearchOwner = sessionSearchOwner(state);
  const nextProviderOwner = providerOwner(state);
  const nextReviewTarget = snapshotPromptReviewTarget(state.review_target);
  const firstProjection = !drafts.initialized;
  const composerOwnerChanged = !firstProjection && drafts.composerOwner !== nextComposerOwner;
  const composerSessionOwnerChanged = !firstProjection
    && drafts.composerSessionOwner !== nextComposerSessionOwner;
  const commitGenerationChanged = !firstProjection
    && drafts.composerCommitGeneration !== state.composer_commit_generation;
  const pendingRun = drafts.pendingRunSubmission;
  const bindsCreatedSession = pendingRun !== null
    && pendingRun.owner === drafts.composerOwner
    && pendingRun.owner.endsWith("\u0000new")
    && pendingRun.workspacePath === state.draft_target.workspacePath
    && state.draft_target.sessionId !== null;
  const reviewTargetChanged = !firstProjection
    && !samePromptReviewTarget(drafts.reviewTarget, nextReviewTarget);
  const providerOwnerChanged = drafts.providerOwner !== nextProviderOwner;
  const providerCommitHasNewerDraft = mutationSnapshot?.providerRevision !== undefined
    && mutationSnapshot.providerRevision !== drafts.providerRevision;
  const providerScopeUnchanged = previous !== null
    && previous.config_target.workspacePath === state.config_target.workspacePath
    && previous.config_target.sessionId === state.config_target.sessionId;
  const rejectedRunMutationOwnsCurrentDraft = mutationSnapshot?.runSettlement === "rejected"
    && mutationSnapshot.composerOwner === drafts.composerOwner
    && !composerSessionOwnerChanged;

  if (firstProjection) {
    drafts.composerOwner = nextComposerOwner;
    drafts.composerSessionOwner = nextComposerSessionOwner;
    drafts.composerCommitGeneration = state.composer_commit_generation;
    drafts.prompt = state.draft_prompt;
    drafts.imageInput = state.image_input;
  } else {
    if (composerOwnerChanged) {
      rememberCurrentComposerDraft(uiState);
    }
    if (commitGenerationChanged) {
      drafts.composerCommitGeneration = state.composer_commit_generation;
      if (pendingRun) {
        if (!rejectedRunMutationOwnsCurrentDraft) {
          if (pendingRun.composerRevision === drafts.composerRevision) drafts.prompt = state.draft_prompt;
          if (pendingRun.imageRevision === drafts.imageRevision) drafts.imageInput = state.image_input;
          if (
            pendingRun.reviewRevision === drafts.reviewRevision
            && samePromptReviewTarget(pendingRun.reviewTarget, drafts.reviewTarget)
            && samePromptReviewTarget(pendingRun.reviewTarget, nextReviewTarget)
          ) {
            synchronizeReviewDraft(drafts, state.review_draft_text);
          }
        }
        drafts.pendingRunSubmission = null;
      } else if (!rejectedRunMutationOwnsCurrentDraft) {
        drafts.prompt = state.draft_prompt;
        drafts.imageInput = state.image_input;
      }
    }
    if (drafts.composerOwner !== nextComposerOwner) {
      const previousComposerSessionOwner = drafts.composerSessionOwner;
      drafts.composerOwner = nextComposerOwner;
      drafts.composerSessionOwner = nextComposerSessionOwner;
      if (bindsCreatedSession) {
        const adopted = uiState.mainComposerDrafts.get(previousComposerSessionOwner);
        if (adopted) {
          uiState.mainComposerDrafts.set(nextComposerSessionOwner, adopted);
          uiState.mainComposerDrafts.delete(previousComposerSessionOwner);
        }
      } else if (
        state.draft_target.sessionId === null
        && !rejectedRunMutationOwnsCurrentDraft
      ) {
        // There is no row to navigate back to an unowned draft. Re-entering a project/quick-chat
        // new-session surface therefore means an explicit reset even when an older local `new`
        // key exists for that workspace.
        uiState.mainComposerDrafts.delete(nextComposerSessionOwner);
        drafts.prompt = state.draft_prompt;
        drafts.imageInput = state.image_input;
        drafts.composerRevision += 1;
        drafts.imageRevision += 1;
      } else if (composerSessionOwnerChanged) {
        const remembered = uiState.mainComposerDrafts.get(nextComposerSessionOwner);
        drafts.prompt = remembered?.prompt ?? state.draft_prompt;
        drafts.imageInput = remembered?.imageInput ?? state.image_input;
        // A response captured for the previous screen must not match the restored screen's
        // local revision, even when both happen to contain the same text.
        drafts.composerRevision += 1;
        drafts.imageRevision += 1;
      }
    }
    const pending = drafts.pendingRunSubmission;
    if (
      pending?.commandAccepted
      && pending.baseCommitGeneration === state.composer_commit_generation
      && state.can_submit
      && !state.busy
    ) {
      drafts.pendingRunSubmission = null;
    }
  }
  if (firstProjection || drafts.sessionSearchOwner !== nextSearchOwner) {
    drafts.sessionSearchOwner = nextSearchOwner;
    drafts.sessionSearch = state.session_search_text;
  }
  if (providerOwnerChanged && providerCommitHasNewerDraft && providerScopeUnchanged) {
    drafts.providerOwner = nextProviderOwner;
  } else if (firstProjection || providerOwnerChanged || providerOverlayOpened(previous, state)) {
    drafts.providerOwner = nextProviderOwner;
    hydrateProviderDraft(drafts.provider, state);
  } else if (mutationSnapshot?.providerRevision === drafts.providerRevision) {
    hydrateProviderDraft(drafts.provider, state);
  }
  if (firstProjection || providerOwnerChanged) {
    uiState.providerCatalogRevision = state.provider_catalog_base_url === null
      ? null
      : drafts.providerCatalogIdentityRevision;
  }
  rememberCurrentComposerDraft(uiState);
  reconcileProviderCatalogRequest(uiState, state);
  if (
    drafts.provider.selectedModelId.length > 0
    && providerCatalogOwnsCurrentDraft(state, uiState)
    && !state.provider_model_ids.includes(drafts.provider.selectedModelId)
  ) {
    drafts.provider.selectedModelId = selectedProviderModelId(state);
  }
  if (firstProjection || workspaceOverlayOpened(previous, state)) drafts.workspaceInput = state.workspace_input;
  const reviewProjectionAdvanced = previous !== null
    && samePromptReviewTarget(previous.review_target, nextReviewTarget)
    && previous.review_draft_text !== state.review_draft_text;
  if (firstProjection) {
    drafts.reviewTarget = nextReviewTarget;
    synchronizeReviewDraft(drafts, state.review_draft_text);
  } else if (reviewTargetChanged) {
    drafts.reviewTarget = nextReviewTarget;
    drafts.reviewRevision += 1;
    synchronizeReviewDraft(drafts, state.review_draft_text);
  } else if (reviewProjectionAdvanced && drafts.reviewRevision === drafts.reviewSyncedRevision) {
    synchronizeReviewDraft(drafts, state.review_draft_text);
  }
  if (firstProjection || commandPaletteOpened(previous, state)) drafts.localSearch = state.local_search_text;
  if (!mutationSnapshot?.runStart && mutationSnapshot?.imageRevision === drafts.imageRevision) {
    drafts.imageInput = state.image_input;
  }
  if (mutationSnapshot?.workspaceRevision === drafts.workspaceRevision) drafts.workspaceInput = state.workspace_input;
  if (
    !mutationSnapshot?.runStart
    && mutationSnapshot?.reviewRevision === drafts.reviewRevision
    && mutationSnapshot.composerOwner === drafts.composerOwner
    && mutationSnapshot.composerOwner === nextComposerOwner
    && mutationSnapshot.reviewTarget !== undefined
    && samePromptReviewTarget(mutationSnapshot.reviewTarget, drafts.reviewTarget)
    && samePromptReviewTarget(mutationSnapshot.reviewTarget, nextReviewTarget)
  ) {
    synchronizeReviewDraft(drafts, state.review_draft_text);
  }
  drafts.initialized = true;
}

function rememberCurrentComposerDraft(uiState: UiLocalState): void {
  const owner = uiState.drafts.composerSessionOwner;
  if (!owner) return;
  uiState.mainComposerDrafts.set(owner, {
    prompt: uiState.drafts.prompt,
    imageInput: uiState.drafts.imageInput,
  });
}

export function deriveUiCapabilities(state: DesktopWebState, uiState: UiLocalState): UiCapabilities {
  const composer = composerCapabilities(state, uiState.drafts.prompt);
  const runOwnerOpen = runOwnerMutationOpen(uiState);
  if (!runOwnerOpen) {
    composer.canSubmit = false;
    composer.canEnhance = false;
    composer.canReviewUncommitted = false;
  }
  const provider = uiState.drafts.provider;
  const configDraft = activeConfigDraftProjection(state, uiState);
  const selectedModelIndex = state.provider_model_ids.indexOf(provider.selectedModelId);
  const currentProviderLimitDraftDirty = providerLimitDraftTargetsEffectiveProvider(
    state,
    provider,
  );
  const providerActions = providerCapabilities({
    provider_loading: state.provider_loading || uiState.providerCatalogTransaction.active !== null,
    provider_base_url: provider.baseUrl,
    provider_profile: provider.providerProfile,
    provider_api_key_env: provider.apiKeyEnv,
    provider_catalog_base_url: state.provider_catalog_base_url,
    provider_catalog_profile: state.provider_catalog_profile,
    provider_catalog_api_key_env: state.provider_catalog_api_key_env,
    provider_context_window: provider.contextWindow,
    provider_max_output_tokens: provider.maxOutputTokens,
    provider_selected_index: selectedModelIndex,
    provider_apply_enabled: state.provider_apply_enabled,
    config_draft: configDraft,
  }, {
    currentProviderLimitDraftDirty,
  });
  return {
    ...composer,
    canUseImageInput: state.image_input_enabled,
    canSendEnhancedReview: runOwnerOpen
      && state.send_enhanced_enabled
      && !state.navigation_loading
      && uiState.drafts.reviewDraft.trim().length > 0,
    canSendRawReview: runOwnerOpen
      && state.send_raw_enabled
      && !state.navigation_loading,
    ...providerActions,
  };
}

export function projectViewState(state: DesktopWebState, uiState: UiLocalState): DesktopViewState {
  const capabilities = deriveUiCapabilities(state, uiState);
  const configDraft = activeConfigDraftProjection(state, uiState);
  const runStartPending = uiState.runStartMutationPending;
  const newSessionPending = uiState.activeNewSessionMutation !== null;
  const providerDraft = providerDraftForCurrentSurface(state, uiState);
  const providerCatalogAccepted = providerCatalogOwnsCurrentDraft(state, uiState);
  const providerLoading = state.provider_loading || uiState.providerCatalogTransaction.active !== null;
  const providerModelIds = providerCatalogAccepted ? state.provider_model_ids : [];
  const providerModels = providerCatalogAccepted ? state.provider_models : [];
  const providerIndex = providerModelIds.indexOf(providerDraft.selectedModelId);
  const providerTargetChangedDuringLoad = providerCatalogTargetChangedDuringLoad(state, uiState);
  const providerCompletionRejected = uiState.rejectedProviderCatalogRequest !== null;
  const providerCatalogMismatch = state.provider_catalog_base_url !== null && !providerCatalogAccepted;
  const providerStatus = providerTargetChangedDuringLoad || providerCompletionRejected || providerCatalogMismatch
    ? {
      kind: "warning" as const,
      title: "モデル一覧の対象が変更されました",
      hint: "現在のBase URLとConnection typeで、もう一度モデル一覧を読み込んでください。",
      details: "編集中の接続先と一致しないモデル一覧は表示・適用されません。",
    }
    : state.provider_status;
  const configFields = activeConfigFields(state, uiState);
  return {
    ...state,
    draft_prompt: uiState.drafts.prompt,
    image_input: uiState.drafts.imageInput,
    workspace_input: uiState.drafts.workspaceInput,
    review_draft_text: uiState.drafts.reviewDraft,
    local_search_text: uiState.drafts.localSearch,
    session_search_text: uiState.drafts.sessionSearch,
    provider_base_url: providerDraft.baseUrl,
    provider_profile: providerDraft.providerProfile,
    provider_api_key_env: providerDraft.apiKeyEnv,
    provider_context_window: providerDraft.contextWindow,
    provider_max_output_tokens: providerDraft.maxOutputTokens,
    provider_loading: providerLoading,
    provider_catalog_base_url: providerCatalogAccepted ? state.provider_catalog_base_url : null,
    provider_catalog_profile: providerCatalogAccepted ? state.provider_catalog_profile : null,
    provider_catalog_api_key_env: providerCatalogAccepted
      ? state.provider_catalog_api_key_env
      : null,
    provider_models: providerModels,
    provider_model_ids: providerModelIds,
    provider_selected_index: providerCatalogAccepted && providerIndex >= 0 ? providerIndex : -1,
    provider_status: providerStatus,
    provider_selected_model_summary: providerCatalogAccepted
      ? state.provider_selected_model_summary
      : [],
    config_draft: configDraft,
    access_target: state.access_target,
    navigation_loading: state.navigation_loading || newSessionPending,
    navigation_admission_open: state.navigation_admission_open && !runStartPending && !newSessionPending,
    background_mutation_pending: state.background_mutation_pending || runStartPending || newSessionPending,
    busy: state.busy || runStartPending,
    task_activity_state: taskActivityStateForView(state.task_activity_state, runStartPending),
    async_polling_required: state.async_polling_required || runStartPending,
    can_submit: capabilities.canSubmit,
    enhance_enabled: capabilities.canEnhance,
    image_input_enabled: capabilities.canUseImageInput,
    send_enhanced_enabled: capabilities.canSendEnhancedReview,
    send_raw_enabled: capabilities.canSendRawReview,
    provider_apply_enabled: capabilities.canApplyProvider,
    config_fields: configFields,
  };
}

function providerCatalogRequestTargetsCurrentDraft(
  request: ProviderCatalogRequest,
  state: DesktopWebState,
  uiState: UiLocalState,
): boolean {
  const draft = providerDraftForCurrentSurface(state, uiState);
  return request.providerOwner === providerOwner(state)
    && request.providerRevision === uiState.drafts.providerCatalogIdentityRevision
    && request.baseUrl === normalizeProviderBaseUrl(draft.baseUrl)
    && request.providerProfile === draft.providerProfile
    && request.apiKeyEnv === normalizeApiKeyEnv(draft.apiKeyEnv);
}

function providerCatalogResultTargetsRequest(
  request: ProviderCatalogRequest,
  state: DesktopWebState,
): boolean {
  return state.provider_catalog_base_url === null
    || (
      normalizeProviderBaseUrl(state.provider_catalog_base_url) === request.baseUrl
      && state.provider_catalog_profile === request.providerProfile
      && state.provider_catalog_api_key_env === request.apiKeyEnv
    );
}

function reconcileProviderCatalogRequest(uiState: UiLocalState, state: DesktopWebState): void {
  const request = uiState.providerCatalogTransaction.active;
  if (!request?.admitted || state.provider_loading) return;
  const completionAccepted = providerCatalogRequestTargetsCurrentDraft(request, state, uiState)
    && providerCatalogResultTargetsRequest(request, state);
  uiState.providerCatalogRevision = completionAccepted ? request.providerRevision : null;
  uiState.rejectedProviderCatalogRequest = completionAccepted ? null : request;
  clearAsyncTransaction(uiState.providerCatalogTransaction, request);
}

function providerCatalogTargetChangedDuringLoad(
  state: DesktopWebState,
  uiState: UiLocalState,
): boolean {
  const request = uiState.providerCatalogTransaction.active;
  return Boolean(
    request?.admitted
    && state.provider_loading
    && !providerCatalogRequestTargetsCurrentDraft(request, state, uiState),
  );
}

function providerCatalogOwnsCurrentDraft(
  state: DesktopWebState,
  uiState: UiLocalState,
): boolean {
  const draft = providerDraftForCurrentSurface(state, uiState);
  return uiState.drafts.providerOwner === providerOwner(state)
    && uiState.providerCatalogRevision === uiState.drafts.providerCatalogIdentityRevision
    && uiState.rejectedProviderCatalogRequest === null
    && state.provider_catalog_base_url !== null
    && normalizeProviderBaseUrl(state.provider_catalog_base_url)
      === normalizeProviderBaseUrl(draft.baseUrl)
    && state.provider_catalog_profile === draft.providerProfile
    && state.provider_catalog_api_key_env === normalizeApiKeyEnv(draft.apiKeyEnv);
}

function providerDraftForCurrentSurface(
  state: DesktopWebState,
  uiState: UiLocalState,
): ProviderDraft {
  if (!state.startup.initial_setup_required || state.overlay !== "initial_setup") {
    return uiState.drafts.provider;
  }
  return providerDraftFromConfigFields(activeConfigFields(state, uiState), uiState.drafts.provider);
}

function providerDraftFromConfigFields(
  fields: ReadonlyArray<DesktopWebState["config_fields"][number]>,
  fallback: ProviderDraft,
): ProviderDraft {
  const values = new Map(fields.map((field) => [field.key, field.value]));
  const providerProfile = values.get("model.provider_profile");
  return {
    baseUrl: values.get("model.base_url") ?? fallback.baseUrl,
    providerProfile: isProviderProfile(providerProfile)
      ? providerProfile
      : fallback.providerProfile,
    apiKeyEnv: values.get("model.api_key_env") ?? fallback.apiKeyEnv,
    contextWindow: values.get("model.context_window") ?? fallback.contextWindow,
    maxOutputTokens: values.get("model.max_output_tokens") ?? fallback.maxOutputTokens,
    selectedModelId: values.get("model.model") ?? fallback.selectedModelId,
  };
}

function sameProviderDraft(left: ProviderDraft, right: ProviderDraft): boolean {
  return left.baseUrl === right.baseUrl
    && left.providerProfile === right.providerProfile
    && left.apiKeyEnv === right.apiKeyEnv
    && left.contextWindow === right.contextWindow
    && left.maxOutputTokens === right.maxOutputTokens
    && left.selectedModelId === right.selectedModelId;
}

function sameProviderCatalogIdentity(left: ProviderDraft, right: ProviderDraft): boolean {
  return normalizeProviderBaseUrl(left.baseUrl) === normalizeProviderBaseUrl(right.baseUrl)
    && left.providerProfile === right.providerProfile
    && normalizeApiKeyEnv(left.apiKeyEnv) === normalizeApiKeyEnv(right.apiKeyEnv);
}

export function activeConfigDraftProjection(
  state: DesktopWebState,
  uiState: UiLocalState,
): ConfigDraftCapabilityProjection {
  const projected = uiState.configDirty
    ? state.config_draft_capabilities.dirty
    : state.config_draft_capabilities.clean;
  const editOpen = configDraftEditOpen(uiState);
  const ownerMutationOpen = configOwnerMutationOpen(uiState);
  const completeDraftValid = validateConfigFieldValues(activeConfigFields(state, uiState)).ok;
  return {
    ...projected,
    edit_enabled: projected.edit_enabled && editOpen,
    discard_enabled: projected.discard_enabled && configDraftDiscardOpen(uiState),
    commit_enabled: projected.commit_enabled
      && configDraftCommitOpen(uiState, state.startup.initial_setup_required)
      && completeDraftValid,
    external_owner_mutation_open: projected.external_owner_mutation_open
      && ownerMutationOpen
      && completeDraftValid,
    access_mode_mutation_enabled: projected.access_mode_mutation_enabled
      && ownerMutationOpen
      && completeDraftValid,
  };
}

function activeConfigFields(state: DesktopWebState, uiState: UiLocalState) {
  const draftApplies = configDraftAppliesTo(uiState, state.config_target);
  return state.config_fields.map((field) => ({
    ...field,
    value: draftApplies ? (uiState.configDraftValues.get(field.key) ?? field.value) : field.value,
  }));
}

export function operationInvalidatesComposer(name: string | null): boolean {
  return name !== null && COMPOSER_INVALIDATING_MUTATIONS.has(name);
}

function hydrateProviderDraft(draft: ProviderDraft, state: DesktopWebState): void {
  draft.baseUrl = state.provider_base_url;
  draft.providerProfile = state.provider_profile;
  draft.apiKeyEnv = state.provider_api_key_env;
  draft.contextWindow = state.provider_context_window;
  draft.maxOutputTokens = state.provider_max_output_tokens;
  draft.selectedModelId = selectedProviderModelId(state);
}

function providerLimitDraftTargetsEffectiveProvider(
  state: DesktopWebState,
  draft: ProviderDraft,
): boolean {
  const targetMatches = normalizeProviderBaseUrl(draft.baseUrl)
      === normalizeProviderBaseUrl(state.provider_effective_base_url)
    && draft.providerProfile === state.provider_effective_profile
    && normalizeApiKeyEnv(draft.apiKeyEnv)
      === normalizeApiKeyEnv(state.provider_effective_api_key_env)
    && draft.selectedModelId === state.provider_effective_model_id;
  if (!targetMatches) return false;
  return draft.contextWindow.trim() !== state.provider_effective_context_window.trim()
    || draft.maxOutputTokens.trim() !== state.provider_effective_max_output_tokens.trim();
}

function selectedProviderModelId(state: DesktopWebState): string {
  return state.provider_model_ids[state.provider_selected_index] ?? "";
}

function synchronizeReviewDraft(
  drafts: UiLocalState["drafts"],
  reviewDraft: string,
): void {
  drafts.reviewDraft = reviewDraft;
  drafts.reviewSyncedRevision = drafts.reviewRevision;
}

function snapshotPromptReviewTarget(
  target: PromptReviewMutationTarget | null | undefined,
): PromptReviewMutationTarget | null {
  return snapshotPromptReviewMutationTarget(target);
}

function samePromptReviewTarget(
  left: PromptReviewMutationTarget | null | undefined,
  right: PromptReviewMutationTarget | null | undefined,
): boolean {
  if (left === null || left === undefined || right === null || right === undefined) {
    return (left === null || left === undefined) && (right === null || right === undefined);
  }
  return left.workspacePath === right.workspacePath
    && left.sessionId === right.sessionId
    && left.ownerGeneration === right.ownerGeneration
    && left.requestId === right.requestId
    && sameRunExpectedState(left.expectedState, right.expectedState);
}

function sameRunExpectedState(left: RunExpectedState, right: RunExpectedState): boolean {
  if (left.kind !== right.kind) return false;
  if (left.admissionRevision !== right.admissionRevision) return false;
  return left.kind === "turn"
    ? right.kind === "turn" && left.turnId === right.turnId
    : right.kind === "idle" && left.latestTurnId === right.latestTurnId;
}

function positiveInteger(value: string): boolean {
  return /^[1-9]\d*$/.test(value.trim());
}

function providerOverlayOpened(previous: DesktopWebState | null, state: DesktopWebState): boolean {
  return state.overlay === "provider" && previous?.overlay !== "provider";
}

function workspaceOverlayOpened(previous: DesktopWebState | null, state: DesktopWebState): boolean {
  return state.overlay === "workspace" && previous?.overlay !== "workspace";
}

function commandPaletteOpened(previous: DesktopWebState | null, state: DesktopWebState): boolean {
  return state.overlay === "command_palette" && previous?.overlay !== "command_palette";
}

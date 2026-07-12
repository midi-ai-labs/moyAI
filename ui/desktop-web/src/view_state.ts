import {
  configDraftAppliesTo,
  configMutationPending,
  reconcileConfigDraftTarget,
} from "./config_mutation.ts";
import type {
  ConfigMutationTarget,
  DesktopWebState,
  DraftActionTarget,
  SessionSearchTarget,
} from "./types.ts";
import type { ProviderDraft, UiLocalState } from "./ui_state.ts";

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

const EXTERNAL_CONFIG_OWNER_MUTATIONS = new Set([
  "toggle_access_mode",
  "apply_provider_session",
  "save_provider_global",
]);

export function mutationStartsRun(mutationName: string): boolean {
  return RUN_START_MUTATIONS.has(mutationName);
}

export function mutationChangesConfigOwner(mutationName: string): boolean {
  return EXTERNAL_CONFIG_OWNER_MUTATIONS.has(mutationName);
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
  return !uiState.configDirty
    && !configMutationPending(uiState)
    && !uiState.externalConfigMutationPending
    && !uiState.runStartMutationPending;
}

export function configDraftEditOpen(uiState: UiLocalState): boolean {
  return !configMutationPending(uiState)
    && !uiState.externalConfigMutationPending;
}

export function configDraftDiscardOpen(uiState: UiLocalState): boolean {
  return uiState.configDirty && configDraftEditOpen(uiState);
}

export function configDraftCommitOpen(
  uiState: UiLocalState,
  initialSetupRequired: boolean,
): boolean {
  return configDraftEditOpen(uiState)
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
    DesktopWebState,
    | "provider_loading"
    | "provider_base_url"
    | "provider_metadata_mode"
    | "provider_catalog_base_url"
    | "provider_catalog_metadata_mode"
    | "provider_context_window"
    | "provider_max_output_tokens"
    | "provider_selected_index"
    | "provider_apply_enabled"
    | "config_owner_mutation_open"
  >,
): Pick<UiCapabilities, "canLoadProviderModels" | "canApplyProvider"> {
  const urlValid = /^https?:\/\/\S+$/i.test(state.provider_base_url.trim());
  const limitsValid = positiveInteger(state.provider_context_window)
    && positiveInteger(state.provider_max_output_tokens);
  const catalogOwnerMatches = state.provider_catalog_base_url !== null
    && normalizeProviderBaseUrl(state.provider_base_url) === state.provider_catalog_base_url
    && state.provider_metadata_mode === state.provider_catalog_metadata_mode;
  return {
    canLoadProviderModels: !state.provider_loading && urlValid && limitsValid,
    canApplyProvider: state.provider_apply_enabled
      && state.config_owner_mutation_open
      && !state.provider_loading
      && urlValid
      && limitsValid
      && state.provider_selected_index >= 0
      && catalogOwnerMatches,
  };
}

function normalizeProviderBaseUrl(input: string): string {
  const trimmed = input.trim().replace(/\/+$/, "");
  return trimmed.endsWith("/v1") && trimmed.length > 3
    ? trimmed.slice(0, -3)
    : trimmed;
}

export interface DraftMutationSnapshot {
  runStart?: boolean;
  composerRevision?: number;
  composerOwner?: string;
  imageRevision?: number;
  workspaceRevision?: number;
  reviewRevision?: number;
  providerRevision?: number;
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
  if (mutationName === "send_prompt_review" || mutationName === "cancel_prompt_review") snapshot.reviewRevision = drafts.reviewRevision;
  if (PROVIDER_COMMIT_MUTATIONS.has(mutationName)) snapshot.providerRevision = drafts.providerRevision;
  if (RUN_START_MUTATIONS.has(mutationName)) {
    snapshot.runStart = true;
    drafts.pendingRunSubmission = {
      owner: drafts.composerOwner,
      workspacePath: drafts.composerOwner.slice(0, drafts.composerOwner.lastIndexOf("\u0000")),
      composerRevision: drafts.composerRevision,
      imageRevision: drafts.imageRevision,
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
  if (!RUN_START_MUTATIONS.has(mutationName) || !snapshot?.composerOwner) return;
  if (uiState.drafts.pendingRunSubmission?.owner === snapshot.composerOwner) {
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
  const startsRun = RUN_START_MUTATIONS.has(mutationName);
  if (startsRun) {
    const pending = drafts.pendingRunSubmission;
    if (
      pending
      && pending.owner === snapshot.composerOwner
      && pending.composerRevision === snapshot.composerRevision
      && pending.imageRevision === snapshot.imageRevision
    ) {
      pending.commandAccepted = true;
    }
  } else {
    if (snapshot.composerRevision === drafts.composerRevision) drafts.prompt = state.draft_prompt;
    if (snapshot.imageRevision === drafts.imageRevision) drafts.imageInput = state.image_input;
  }
  if (snapshot.workspaceRevision === drafts.workspaceRevision) drafts.workspaceInput = state.workspace_input;
  if (!startsRun && snapshot.reviewRevision === drafts.reviewRevision) {
    drafts.reviewDraft = state.review_draft_text;
  }
  if (snapshot.providerRevision === drafts.providerRevision) hydrateProviderDraft(drafts.provider, state);
}

export function composerOwner(state: DesktopWebState): string {
  return `${state.draft_target.workspacePath}\u0000${state.draft_target.sessionId ?? "new"}`;
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

export function draftMutationTarget(state: DesktopWebState): DraftActionTarget {
  return state.draft_target;
}

export function providerDraftPayload(
  draft: ProviderDraft,
  expectedTarget: ConfigMutationTarget,
  configOwnerMutationOpen: boolean,
): Record<string, unknown> {
  return {
    input: {
      baseUrl: draft.baseUrl,
      metadataMode: draft.metadataMode,
      contextWindow: draft.contextWindow,
      maxOutputTokens: draft.maxOutputTokens,
      selectedModelId: draft.selectedModelId,
      configOwnerMutationOpen,
    },
    expectedTarget,
  };
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
  const nextSearchOwner = sessionSearchOwner(state);
  const nextProviderOwner = providerOwner(state);
  const firstProjection = !drafts.initialized;
  const commitGenerationChanged = !firstProjection
    && drafts.composerCommitGeneration !== state.composer_commit_generation;
  const pendingRun = drafts.pendingRunSubmission;
  const bindsCreatedSession = pendingRun !== null
    && pendingRun.owner === drafts.composerOwner
    && pendingRun.owner.endsWith("\u0000new")
    && pendingRun.workspacePath === state.draft_target.workspacePath
    && state.draft_target.sessionId !== null;
  const providerOwnerChanged = drafts.providerOwner !== nextProviderOwner;
  const providerCommitHasNewerDraft = mutationSnapshot?.providerRevision !== undefined
    && mutationSnapshot.providerRevision !== drafts.providerRevision;
  const providerScopeUnchanged = previous !== null
    && previous.config_target.workspacePath === state.config_target.workspacePath
    && previous.config_target.sessionId === state.config_target.sessionId;

  if (firstProjection) {
    drafts.composerOwner = nextComposerOwner;
    drafts.composerCommitGeneration = state.composer_commit_generation;
    drafts.prompt = state.draft_prompt;
    drafts.imageInput = state.image_input;
  } else {
    if (commitGenerationChanged) {
      drafts.composerCommitGeneration = state.composer_commit_generation;
      if (pendingRun) {
        if (pendingRun.composerRevision === drafts.composerRevision) drafts.prompt = state.draft_prompt;
        if (pendingRun.imageRevision === drafts.imageRevision) drafts.imageInput = state.image_input;
        if (pendingRun.reviewRevision === drafts.reviewRevision) {
          drafts.reviewDraft = state.review_draft_text;
        }
        drafts.pendingRunSubmission = null;
      } else {
        drafts.prompt = state.draft_prompt;
        drafts.imageInput = state.image_input;
      }
    }
    if (drafts.composerOwner !== nextComposerOwner) {
      drafts.composerOwner = nextComposerOwner;
      if (!bindsCreatedSession) {
        drafts.prompt = state.draft_prompt;
        drafts.imageInput = state.image_input;
      }
    }
    const pending = drafts.pendingRunSubmission;
    if (
      pending?.commandAccepted
      && pending.baseCommitGeneration === state.composer_commit_generation
      && state.can_submit
      && !state.busy
      && !state.agent_tree_active
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
  } else if (
    drafts.provider.selectedModelId.length > 0
    && !state.provider_model_ids.includes(drafts.provider.selectedModelId)
  ) {
    drafts.provider.selectedModelId = selectedProviderModelId(state);
  }
  if (firstProjection || workspaceOverlayOpened(previous, state)) drafts.workspaceInput = state.workspace_input;
  if (firstProjection || promptReviewOpened(previous, state)) drafts.reviewDraft = state.review_draft_text;
  if (firstProjection || commandPaletteOpened(previous, state)) drafts.localSearch = state.local_search_text;
  if (!mutationSnapshot?.runStart && mutationSnapshot?.imageRevision === drafts.imageRevision) {
    drafts.imageInput = state.image_input;
  }
  if (mutationSnapshot?.workspaceRevision === drafts.workspaceRevision) drafts.workspaceInput = state.workspace_input;
  if (!mutationSnapshot?.runStart && mutationSnapshot?.reviewRevision === drafts.reviewRevision) {
    drafts.reviewDraft = state.review_draft_text;
  }
  drafts.initialized = true;
}

export function deriveUiCapabilities(state: DesktopWebState, uiState: UiLocalState): UiCapabilities {
  const composer = composerCapabilities(state, uiState.drafts.prompt);
  if (uiState.runStartMutationPending) {
    composer.canSubmit = false;
    composer.canEnhance = false;
    composer.canReviewUncommitted = false;
  }
  const provider = uiState.drafts.provider;
  const selectedModelIndex = state.provider_model_ids.indexOf(provider.selectedModelId);
  const providerActions = providerCapabilities({
    provider_loading: state.provider_loading,
    provider_base_url: provider.baseUrl,
    provider_metadata_mode: provider.metadataMode,
    provider_catalog_base_url: state.provider_catalog_base_url,
    provider_catalog_metadata_mode: state.provider_catalog_metadata_mode,
    provider_context_window: provider.contextWindow,
    provider_max_output_tokens: provider.maxOutputTokens,
    provider_selected_index: selectedModelIndex,
    provider_apply_enabled: state.provider_apply_enabled,
    config_owner_mutation_open: configOwnerMutationOpen(uiState),
  });
  return {
    ...composer,
    canUseImageInput: state.image_input_enabled,
    canSendEnhancedReview: !uiState.runStartMutationPending
      && state.send_enhanced_enabled
      && !state.navigation_loading
      && uiState.drafts.reviewDraft.trim().length > 0,
    canSendRawReview: !uiState.runStartMutationPending
      && state.send_raw_enabled
      && !state.navigation_loading,
    ...providerActions,
  };
}

export function projectViewState(state: DesktopWebState, uiState: UiLocalState): DesktopWebState {
  const capabilities = deriveUiCapabilities(state, uiState);
  const configMutationOpen = configOwnerMutationOpen(uiState);
  const providerIndex = state.provider_model_ids.indexOf(uiState.drafts.provider.selectedModelId);
  const configFields = state.config_fields.map((field) => ({
    ...field,
    value: configDraftAppliesTo(uiState, state.config_target)
      ? (uiState.configDraftValues.get(field.key) ?? field.value)
      : field.value,
  }));
  return {
    ...state,
    draft_prompt: uiState.drafts.prompt,
    image_input: uiState.drafts.imageInput,
    workspace_input: uiState.drafts.workspaceInput,
    review_draft_text: uiState.drafts.reviewDraft,
    local_search_text: uiState.drafts.localSearch,
    session_search_text: uiState.drafts.sessionSearch,
    provider_base_url: uiState.drafts.provider.baseUrl,
    provider_metadata_mode: uiState.drafts.provider.metadataMode,
    provider_context_window: uiState.drafts.provider.contextWindow,
    provider_max_output_tokens: uiState.drafts.provider.maxOutputTokens,
    provider_selected_index: providerIndex >= 0 ? providerIndex : state.provider_selected_index,
    config_owner_mutation_open: configMutationOpen,
    config_draft_dirty: uiState.configDirty,
    config_draft_discard_enabled: configDraftDiscardOpen(uiState),
    config_draft_commit_enabled: configDraftCommitOpen(
      uiState,
      state.startup.initial_setup_required,
    ),
    access_target: {
      ...state.access_target,
      configOwnerMutationOpen: configMutationOpen,
    },
    access_mode_mutation_enabled: state.access_mode_mutation_enabled
      && configMutationOpen,
    navigation_admission_open: state.navigation_admission_open && !uiState.runStartMutationPending,
    background_mutation_pending: state.background_mutation_pending || uiState.runStartMutationPending,
    can_submit: capabilities.canSubmit,
    enhance_enabled: capabilities.canEnhance,
    image_input_enabled: capabilities.canUseImageInput,
    send_enhanced_enabled: capabilities.canSendEnhancedReview,
    send_raw_enabled: capabilities.canSendRawReview,
    provider_apply_enabled: capabilities.canApplyProvider,
    config_fields: configFields,
    config_value_text: configFields[state.selected_config_index]?.value ?? state.config_value_text,
  };
}

export function operationInvalidatesComposer(name: string | null): boolean {
  return name !== null && COMPOSER_INVALIDATING_MUTATIONS.has(name);
}

function hydrateProviderDraft(draft: ProviderDraft, state: DesktopWebState): void {
  draft.baseUrl = state.provider_base_url;
  draft.metadataMode = state.provider_metadata_mode;
  draft.contextWindow = state.provider_context_window;
  draft.maxOutputTokens = state.provider_max_output_tokens;
  draft.selectedModelId = selectedProviderModelId(state);
}

function selectedProviderModelId(state: DesktopWebState): string {
  return state.provider_model_ids[state.provider_selected_index] ?? "";
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

function promptReviewOpened(previous: DesktopWebState | null, state: DesktopWebState): boolean {
  return state.overlay === "prompt_review" && previous?.overlay !== "prompt_review";
}

function commandPaletteOpened(previous: DesktopWebState | null, state: DesktopWebState): boolean {
  return state.overlay === "command_palette" && previous?.overlay !== "command_palette";
}

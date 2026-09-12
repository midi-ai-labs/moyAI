import assert from "node:assert/strict";
import test from "node:test";

import {
  actionById as registryActionById,
  beginConfigImportMutation,
  dispatchAction,
  paletteActions as registryPaletteActions,
  type ActionContext,
  type ActionPayload,
} from "../src/actions.ts";
import {
  discardConfigDraft,
  sameConfigMutationTarget,
  updateConfigDraftValue,
} from "../src/config_mutation.ts";
import {
  CommandPaletteInsertionAsyncOwner,
  dispatchCommandPaletteInsertion,
  replaceUtf16Selection,
  type CommandPaletteInsertionRequest,
} from "../src/command_palette_insertion.ts";
import {
  InteractionLifecycle,
  installInteractionEventGate,
  shouldBeginKeyboardInteraction,
  shouldBeginPointerInteraction,
} from "../src/interaction_lifecycle.ts";
import { dispatchNewSessionMutation } from "../src/new_session_mutation.ts";
import {
  shouldDispatchDelegatedKeyboardAction,
  shouldInvalidateCommandPaletteInsertionForKeydown,
  synchronizeProviderOverlayFeedback,
  wireEvents,
} from "../src/events.ts";
import { transcriptAnchors } from "../src/history_navigation.ts";
import { recordInitialSetupImportedSource } from "../src/initial_setup_auxiliary_state.ts";
import {
  globalShortcutAction,
  modalShortcutShouldPreventDefault,
} from "../src/keyboard_shortcut.ts";
import { autoRefreshAllowed, runtimePollingRequired } from "../src/polling_state.ts";
import { PostRenderFocusArbiter } from "../src/focus_arbiter.ts";
import {
  settingsActionFocusCandidates,
  settingsActionFocusStillTargets,
} from "../src/settings_surface.ts";
import {
  renderArtifactPane,
  renderComposer,
  renderLocalConfirmation,
  renderOverlay,
  renderSidebar,
  renderThreadContent,
  renderTopbar,
} from "../src/render.ts";
import {
  createDesktopRenderModel,
  DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION,
  type DesktopRenderLocalPresentation,
} from "../src/render_projection.ts";
import type {
  ConfigFieldProjection,
  DesktopViewState,
  DesktopWebState,
  PromptReviewMutationTarget,
  RunMutationTarget,
} from "../src/types.ts";
import { createUiLocalState, type UiLocalState } from "../src/ui_state.ts";
import {
  agentInterruptTarget,
  childSessionIdForOrder,
  childTurnIdForOrder,
  QUICK_A,
  SESSION_A,
  SESSION_B,
  TURN_A,
  TURN_B,
} from "./canonical_wire_fixture.ts";
import { rootStopTarget, turnStopTarget } from "./stop_target_fixture.ts";
import {
  configCommitControlState,
  displayAccessLabel,
  providerOverlayFeedback,
  validateConfigFieldValues,
  validateConfigInput,
  validateProviderBaseUrl,
  USER_CONFIGURED_SYSTEM_PROMPT_MAX_CHARS,
} from "../src/utils.ts";
import {
  acknowledgeDraftMutation,
  beginProviderCatalogRequest,
  captureDraftMutation,
  composerOwner,
  composerSessionOwner,
  configDraftEditOpen,
  deriveUiCapabilities,
  mutationAdmissionOpen,
  projectViewState,
  reconcileUiDrafts,
  rejectDraftMutation,
  sessionSearchMutationTarget,
  synchronizeInitialSetupProviderDraft,
} from "../src/view_state.ts";

test("Initial Setup projects imported sensitive configured metadata without exposing its value", () => {
  const setupTarget = {
    workspacePath: "C:/workspace",
    globalConfigPath: "C:/config/config.toml",
    setupGeneration: "3",
  };
  const rustProjection = projection({
    overlay: "initial_setup",
    startup: {
      ...projection().startup,
      action_overlay: "initial_setup",
      initial_setup_required: true,
      initial_setup_reason: "config_missing",
      setup_target: setupTarget,
    },
    config_fields: [{
      key: "model.extra_headers_json",
      value: "",
      sensitive: true,
      configured: false,
      env_override: "MOYAI_EXTRA_HEADERS",
      value_type: "json",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    }],
  });
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, rustProjection, null);
  recordInitialSetupImportedSource(
    ui.initialSetupAuxiliary,
    setupTarget,
    rustProjection.config_target,
    "C:/workspace/import.toml",
    "1",
    ["model.extra_headers_json"],
  );

  const imported = projectViewState(rustProjection, ui).config_fields[0];
  assert.equal(imported.value, "");
  assert.equal(imported.configured, true);
  assert.equal(rustProjection.config_fields[0]?.configured, false, "Rust projection remains immutable");

  const drifted = projection({
    ...rustProjection,
    config_target: { ...rustProjection.config_target, configGeneration: "2" },
  });
  assert.equal(projectViewState(drifted, ui).config_fields[0]?.configured, false);
});

for (const overlay of ["initial_setup", "config"] as const) {
test(`${overlay} catalog evidence is invalidated by A to B to A config-draft edits`, () => {
  const providerFields: ConfigFieldProjection[] = [
    {
      key: "model.base_url",
      value: "http://provider-a.test/v1",
      env_override: null,
      value_type: "string",
      required: true,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "model.provider_profile",
      value: "openai_compatible",
      env_override: null,
      value_type: "enum",
      required: true,
      min_value: null,
      max_value: null,
      options: ["lm_studio", "openai_compatible", "openai_responses", "lm_studio_chat_completions"],
    },
    {
      key: "model.api_key_env",
      value: "",
      env_override: null,
      value_type: "string",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "model.context_window",
      value: "65536",
      env_override: null,
      value_type: "integer",
      required: true,
      min_value: 1,
      max_value: null,
      options: [],
    },
    {
      key: "model.max_output_tokens",
      value: "1024",
      env_override: null,
      value_type: "integer",
      required: true,
      min_value: 0,
      max_value: null,
      options: [],
    },
    {
      key: "model.model",
      value: "model-a",
      env_override: null,
      value_type: "string",
      required: true,
      min_value: null,
      max_value: null,
      options: [],
    },
  ];
  const state = projection({
    confirmation_visible: false,
    overlay,
    startup: {
      status: "requires_config",
      title: "Initial Setup",
      message: "Configure",
      detail: "",
      action_overlay: "initial_setup",
      initial_setup_required: true,
      initial_setup_reason: "config_missing",
      global_config_path: "C:/config/config.toml",
      setup_target: {
        workspacePath: "C:/workspace",
        globalConfigPath: "C:/config/config.toml",
        setupGeneration: "3",
      },
      checks: [],
    },
    provider_base_url: "http://provider-a.test/v1",
    provider_profile: "openai_compatible",
    provider_api_key_env: "",
    provider_context_window: "65536",
    provider_max_output_tokens: "1024",
    provider_catalog_base_url: "http://provider-a.test",
    provider_catalog_profile: "openai_compatible",
    provider_catalog_api_key_env: null,
    provider_model_ids: ["model-a", "model-a-alt"],
    provider_models: ["Model A", "Model A alt"],
    provider_selected_index: 0,
    config_fields: providerFields,
  });
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, state);
  assert.deepEqual(projectViewState(state, ui).provider_model_ids, ["model-a", "model-a-alt"]);
  const catalogIdentityRevision = ui.drafts.providerCatalogIdentityRevision;
  updateConfigDraftValue(
    ui,
    state.config_target,
    providerFields.map(({ key, value }) => ({ key, text: value })),
    "model.model",
    "model-a-alt",
  );
  const selectedModel = projectViewState(state, ui);
  assert.equal(synchronizeInitialSetupProviderDraft(selectedModel, ui), true);
  assert.equal(ui.drafts.providerCatalogIdentityRevision, catalogIdentityRevision);
  assert.deepEqual(
    projectViewState(state, ui).provider_model_ids,
    ["model-a", "model-a-alt"],
    "model selection updates the complete draft without invalidating URL/mode catalog evidence",
  );
  const request = beginProviderCatalogRequest(ui, state);
  assert.ok(request);
  const originalRevision = ui.drafts.providerRevision;

  updateConfigDraftValue(
    ui,
    state.config_target,
    providerFields.map(({ key, value }) => ({ key, text: value })),
    "model.base_url",
    "http://provider-b.test/v1",
  );
  const providerB = projectViewState(state, ui);
  assert.equal(synchronizeInitialSetupProviderDraft(providerB, ui), true);
  assert.equal(ui.drafts.providerRevision, originalRevision + 1);
  assert.deepEqual(projectViewState(state, ui).provider_model_ids, []);

  updateConfigDraftValue(
    ui,
    state.config_target,
    projectViewState(state, ui).config_fields.map(({ key, value }) => ({ key, text: value })),
    "model.base_url",
    "http://provider-a.test/v1",
  );
  const providerAAgain = projectViewState(state, ui);
  assert.equal(synchronizeInitialSetupProviderDraft(providerAAgain, ui), true);
  assert.equal(ui.drafts.providerRevision, originalRevision + 2);
  assert.equal(request.providerRevision, catalogIdentityRevision);
  assert.deepEqual(
    projectViewState(state, ui).provider_model_ids,
    [],
    "the old A catalog cannot become current evidence after an ABA draft edit",
  );
});

}

function actionTestModel(
  state: DesktopViewState,
  uiState: UiLocalState = createUiLocalState(),
) {
  return createDesktopRenderModel(state, {
    ...DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION,
    artifactPane: {
      collapsed: uiState.artifactPaneCollapsed,
      mode: uiState.artifactPaneMode,
      selectedAgentPath: uiState.selectedAgentPath,
      selectedAgentExecution: null,
    },
    attachmentTrayOpen: uiState.attachmentTrayOpen,
    configMutationPending: uiState.activeConfigMutationGeneration !== null
      || uiState.externalConfigMutationPending
      || (
        state.overlay === "config"
        && uiState.localConfirmationDecisionPending
        && uiState.pendingLocalConfirmation === null
      ),
    doclingReadinessRequestPending: uiState.doclingReadinessTransaction.active !== null,
    modal: {
      localConfirmation: uiState.pendingLocalConfirmation,
      localDecisionPending: uiState.localConfirmationDecisionPending,
      localDecisionError: uiState.localConfirmationDecisionError,
      permissionDecision: uiState.permissionDecision,
    },
    recoverableError: uiState.recoverableError,
    windowMaximized: uiState.windowMaximized,
  });
}

function actionById(id: string) {
  const action = registryActionById(id);
  return action
    ? {
      ...action,
      enabled: (state: DesktopViewState, payload: ActionPayload) =>
        action.enabled(actionTestModel(state), payload),
    }
    : undefined;
}

function paletteActions(state: DesktopViewState, _configDraftAvailable = false) {
  return registryPaletteActions(actionTestModel(state));
}

async function dispatchGuiAction(
  action: string,
  index: number,
  value: string,
  state: DesktopViewState,
  context: ActionContext,
): Promise<boolean> {
  const currentContext = {
    ...context,
    getRenderModel: () => actionTestModel(context.getViewState?.() ?? state, context.uiState),
  } as ActionContext;
  return dispatchAction(action, currentContext, { index, value });
}

function projection(overrides: Partial<DesktopViewState> = {}): DesktopViewState {
  return {
    projection_revision: "1",
    workspace_path: "C:/workspace",
    selected_session_title: "Session A",
    current_session_label: "Session A",
    status_message: "Ready",
    status_detail: "",
    status_code: "plain",
    access_label: "default",
    access_target: {
      workspacePath: "C:/workspace",
      sessionId: SESSION_A,
      configGeneration: "1",
      accessMode: "default",
      runtimeOwnerToken: "idle:0",
    },
    session_settings: {
      available: false,
      base_url: "http://127.0.0.1:1234",
      model: "model-a",
      provider_profile: "openai_compatible",
      api_key_env: "",
      access_mode: "default",
      context_window: "",
      max_output_tokens: "",
      context_window_inherited: true,
      max_output_tokens_inherited: true,
      provider_mutation_enabled: false,
      access_mutation_enabled: false,
      unavailable_reason: "root sessionを選択すると変更できます。",
      target: null,
    },
    config_draft_capabilities: {
      clean: {
        dirty: false,
        edit_enabled: true,
        discard_enabled: false,
        commit_enabled: false,
        external_owner_mutation_open: true,
        access_mode_mutation_enabled: true,
      },
      dirty: {
        dirty: true,
        edit_enabled: true,
        discard_enabled: true,
        commit_enabled: true,
        external_owner_mutation_open: false,
        access_mode_mutation_enabled: false,
      },
    },
    config_draft: {
      dirty: false,
      edit_enabled: true,
      discard_enabled: false,
      commit_enabled: false,
      external_owner_mutation_open: true,
      access_mode_mutation_enabled: true,
    },
    model_label: "model-a",
    provider_label: "Local",
    selected_project_index: 0,
    selected_session_index: 0,
    project_rows: [{ project_id: "project-a", label: "Project A", path: "C:/workspace" }],
    session_rows: [{
      session_id: SESSION_A,
      label: "Session A",
      title: "Session A",
      status: "idle",
      loaded_status: "idle",
      archived: false,
      pending_permission_requests: 0,
      pending_user_input_requests: 0,
      admission_revision: "4",
      short_id: SESSION_A,
    }],
    chat_session_rows: [],
    draft_prompt: "server prompt",
    image_input: "server.png",
    workspace_input: "C:/workspace",
    review_target: null,
    review_draft_text: "server review",
    local_search_text: "",
    session_search_text: "",
    overlay: "none",
    about: {
      product_name: "moyAI",
      version: "test-version",
      codename: "test-codename",
      license_identifier: "test-license",
      copyright_notice: "Copyright test notice",
    },
    side_chat: {
      configured: false,
      deleting: false,
      chat_id: null,
      owner_session_id: SESSION_A,
      model: "",
      system_prompt: "",
      base_url: "",
      provider_profile: "",
      status: "idle",
      phase: "idle",
      last_error: "",
      generation: "0",
      draft_text: "",
      draft_quote: null,
      draft_revision: "0",
      context_scope: "owner_session",
      context_as_of_append_position: null,
      context_truncated: false,
      messages: [],
      can_send: false,
      can_cancel: false,
    },
    startup: {
      status: "ready",
      title: "Ready",
      message: "",
      detail: "",
      action_overlay: "none",
      initial_setup_required: false,
      checks: [],
    },
    composer_commit_generation: "0",
    draft_target: { workspacePath: "C:/workspace", sessionId: SESSION_A, ownerGeneration: "1" },
    busy: false,
    task_activity_state: "idle",
    navigation_loading: false,
    navigation_admission_open: true,
    turn_page_admission_open: true,
    background_mutation_pending: false,
    agent_tree_active: false,
    composer_submit_mode: "new_request",
    can_submit: true,
    can_cancel_run: false,
    run_target: {
      workspacePath: "C:/workspace",
      sessionId: SESSION_A,
      runtimeOwnerToken: "idle:0",
      permissionConfirmationId: null,
      expectedState: { kind: "idle", latestTurnId: TURN_A, admissionRevision: "4" },
    } satisfies RunMutationTarget,
    stop_target: null,
    enhance_enabled: true,
    image_input_enabled: true,
    send_enhanced_enabled: true,
    send_raw_enabled: true,
    provider_base_url: "http://127.0.0.1:1234",
    provider_profile: "openai_compatible",
    provider_api_key_env: "",
    provider_effective_base_url: "http://127.0.0.1:1234",
    provider_effective_profile: "openai_compatible",
    provider_effective_api_key_env: "",
    provider_effective_context_window: "131072",
    provider_effective_max_output_tokens: "8192",
    provider_effective_model_id: "model-a",
    provider_catalog_base_url: "http://127.0.0.1:1234",
    provider_catalog_profile: "openai_compatible",
    provider_catalog_api_key_env: null,
    provider_context_window: "131072",
    provider_max_output_tokens: "8192",
    provider_model_ids: ["model-a"],
    provider_models: ["Model A"],
    provider_selected_index: 0,
    provider_status: { kind: "idle", title: "Typed idle", hint: "Typed hint", details: "" },
    provider_selected_model_summary: [],
    provider_loading: false,
    provider_apply_enabled: true,
    docling_readiness: {
      status: "idle",
      endpoint: "",
      httpStatus: null,
      message: "Docling readiness has not been checked.",
    },
    config_target: {
      workspacePath: "C:/workspace",
      sessionId: SESSION_A,
      configGeneration: "1",
    },
    config_fields: [{
      key: "model.model",
      value: "model-a",
      env_override: null,
      value_type: "string",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    }],
    attached_images: [],
    transcript_rows: [],
    pending_turn_inputs: [],
    thread_empty: true,
    artifact_rows: [],
    selected_artifact_index: -1,
    artifact_preview_available: false,
    artifact_preview_text: "",
    file_change_rows: [],
    agent_activity_rows: [],
    current_turn_agent_activity_rows: [],
    progress_text: "",
    tool_status_text: "",
    plan: null,
    session_search_include_archived: false,
    history_export_enabled: true,
    ...overrides,
  } as DesktopViewState;
}

function reviewTarget(
  overrides: Partial<PromptReviewMutationTarget> = {},
): PromptReviewMutationTarget {
  return {
    workspacePath: "C:/workspace",
    sessionId: SESSION_A,
    ownerGeneration: "1",
    requestId: "9007199254740993",
    expectedState: { kind: "idle", latestTurnId: TURN_A, admissionRevision: "4" },
    ...overrides,
  };
}

function renderLocal(overrides: {
  configMutationPending?: boolean;
  doclingReadinessRequestPending?: boolean;
  sideChat?: Partial<DesktopRenderLocalPresentation["sideChat"]>;
} = {}): DesktopRenderLocalPresentation {
  return {
    ...DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION,
    configMutationPending: overrides.configMutationPending ?? false,
    doclingReadinessRequestPending: overrides.doclingReadinessRequestPending ?? false,
    sideChat: {
      ...DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION.sideChat,
      ...overrides.sideChat,
    },
  };
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

test("local drafts produce a view without mutating the Rust projection", () => {
  const state = projection();
  const original = structuredClone(state);
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, state, null);

  ui.drafts.prompt = "local prompt";
  ui.drafts.imageInput = "local.png";
  ui.drafts.provider.contextWindow = "65536";
  const view = projectViewState(state, ui);

  assert.equal(view.draft_prompt, "local prompt");
  assert.equal(view.image_input, "local.png");
  assert.equal(view.provider_context_window, "65536");
  assert.deepEqual(state, original);
});

test("child-only activity preserves the authoritative Rust new-request gate", () => {
  const ui = createUiLocalState();
  const state = projection({ draft_prompt: "" });
  reconcileUiDrafts(ui, null, state, null);
  ui.drafts.prompt = "work";

  assert.equal(deriveUiCapabilities(state, ui).canSubmit, true);
  assert.equal(deriveUiCapabilities(state, ui).canEnhance, true);
  assert.equal(mutationAdmissionOpen(ui, "submit_prompt"), true);
  assert.equal(mutationAdmissionOpen(ui, "enhance_prompt"), true);

  const treeActive = projection({
    busy: false,
    agent_tree_active: true,
    can_submit: true,
    enhance_enabled: true,
    review_uncommitted_enabled: true,
  });
  const capabilities = deriveUiCapabilities(treeActive, ui);
  assert.equal(capabilities.canSubmit, true);
  assert.equal(capabilities.canEnhance, true);
  assert.equal(capabilities.canReviewUncommitted, true);
  assert.equal(capabilities.canUseImageInput, true, "draft attachments remain available for the next prompt");

  const baseSession = projection().session_rows[0]!;
  const navigationState = projection({
    busy: false,
    agent_tree_active: true,
    navigation_admission_open: true,
    can_cancel_run: false,
    session_rows: [
      baseSession,
      {
        ...baseSession,
        session_id: SESSION_B,
        label: "Session B",
        title: "Session B",
        loaded_status: "active",
        active_turn_id: TURN_B,
        active_turn_sequence_no: 2,
      },
    ],
  });
  const sidebar = renderSidebar(navigationState);
  for (const [label, pattern] of [
    ["session search", /<input id="session-search"[^>]*>/],
    ["new root chat", /<button class="row-action add-session" data-action="new-project-session"[^>]*>/],
    ["new quick chat", /<button class="tiny-button icon-only" data-action="new-chat" data-focus-key="quick-chat:new-session"[^>]*>/],
    ["open session", /<button class="nav-row" data-action="session" data-index="1"[^>]*>/],
    ["rejoin session", /<button class="row-action row-rejoin" data-action="rejoin-session" data-index="1"[^>]*>/],
  ] as const) {
    const tag = sidebar.match(pattern)?.[0];
    assert.ok(tag, `${label} control is rendered`);
    assert.doesNotMatch(tag, /\sdisabled(?:\s|>|=)/, `${label} stays enabled`);
  }
  assert.equal(
    actionById("rejoin-session")?.enabled?.(navigationState, { index: 1, value: "" }),
    true,
  );
  assert.equal(
    actionById("toggle-session-archived-search")?.enabled?.(
      navigationState,
      { index: -1, value: "" },
    ),
    true,
  );
  assert.equal(
    actionById("cancel-run")?.enabled?.(navigationState, { index: -1, value: "" }),
    false,
    "ordinary Stop is not inferred from detached child activity",
  );
});

test("active root steering keeps an enabled send control labeled as additional instruction", () => {
  for (const agentTreeActive of [false, true]) {
    const rendered = renderComposer(projection({
      busy: true,
      agent_tree_active: agentTreeActive,
      composer_submit_mode: "steer",
      can_submit: true,
      enhance_enabled: false,
      draft_prompt: "追加指示",
      token_meter_label: "",
      token_meter_title: "",
    }));

    assert.match(
      rendered,
      /data-action="send" title="実行中のタスクへ追加指示を送信" aria-label="実行中のタスクへ追加指示を送信"/,
      agentTreeActive ? "active child retention" : "active root",
    );
    assert.doesNotMatch(rendered, /実行中は送信できません|Sub Agentの完了または停止後に送信できます/);
  }
});

test("active root steering with an empty draft asks for input instead of claiming steering is blocked", () => {
  for (const agentTreeActive of [false, true]) {
    const rendered = renderComposer(projection({
      busy: true,
      agent_tree_active: agentTreeActive,
      composer_submit_mode: "steer",
      can_submit: false,
      enhance_enabled: false,
      draft_prompt: "",
      token_meter_label: "",
      token_meter_title: "",
    }));

    assert.match(
      rendered,
      /data-action="send" title="依頼文を入力してください" aria-label="依頼文を入力してください"[^>]*disabled/,
      agentTreeActive ? "active child retention" : "active root",
    );
    assert.doesNotMatch(rendered, /実行中は送信できません|Sub Agentの完了または停止後に送信できます/);
  }
});

test("installed prompt and delegated Settings handlers update capability state synchronously", () => {
  class FakeHtmlElement {
    hidden = false;
  }
  class FakeInput extends FakeHtmlElement {
    value: string;
    type: string;
    checked = false;
    disabled = false;
    dataset: { configKey: string };
    readonly attributes = new Map<string, string>();
    private readonly listeners = new Map<string, Array<(event: { currentTarget: FakeInput }) => void>>();

    constructor(
      configKey = "model.request_timeout_ms",
      value = "3600000",
      type = "text",
    ) {
      super();
      this.dataset = { configKey };
      this.value = value;
      this.type = type;
    }

    matches(selector: string): boolean {
      return selector === ".settings-control";
    }

    setAttribute(name: string, value: string): void {
      this.attributes.set(name, value);
    }

    removeAttribute(name: string): void {
      this.attributes.delete(name);
    }

    getAttribute(name: string): string | null {
      return this.attributes.get(name) ?? null;
    }

    addEventListener(name: string, listener: (event: { currentTarget: FakeInput }) => void): void {
      const listeners = this.listeners.get(name) ?? [];
      listeners.push(listener);
      this.listeners.set(name, listeners);
    }

    dispatch(name: string): void {
      for (const listener of this.listeners.get(name) ?? []) listener({ currentTarget: this });
    }

    focus(): void {}
  }
  class FakePrompt extends FakeHtmlElement {
    value = "";
    scrollHeight = 24;
    style = { height: "", overflowY: "" };
    private readonly listeners = new Map<string, (event: { currentTarget: FakePrompt }) => void>();

    addEventListener(name: string, listener: (event: { currentTarget: FakePrompt }) => void): void {
      this.listeners.set(name, listener);
    }

    input(value: string): void {
      this.value = value;
      this.listeners.get("input")?.({ currentTarget: this });
    }
  }
  class FakeButton extends FakeHtmlElement {
    disabled = true;
    title = "依頼文を入力してください";
    readonly attributes = new Map<string, string>([["aria-label", this.title]]);
    readonly dataset: Record<string, string>;

    constructor(action: string) {
      super();
      this.dataset = { action };
    }

    setAttribute(name: string, value: string): void {
      this.attributes.set(name, value);
    }

    getAttribute(name: string): string | null {
      return this.attributes.get(name) ?? null;
    }
  }

  class FakeValidation extends FakeHtmlElement {
    textContent = "";
    readonly classes = new Map<string, boolean>();
    classList = { toggle: (name: string, active: boolean) => this.classes.set(name, active) };
  }

  const prompt = new FakePrompt();
  const send = new FakeButton("send");
  const settingsInput = new FakeInput();
  const doclingToggle = new FakeInput("docling.enabled", "", "checkbox");
  doclingToggle.checked = true;
  const doclingUrl = new FakeInput("docling.base_url", "http://127.0.0.1:5001", "url");
  const opacityInput = new FakeInput("", "80", "range");
  const apply = new FakeButton("apply-session-config");
  const save = new FakeButton("save-global-config");
  const doclingReadiness = new FakeButton("check-docling-readiness");
  doclingReadiness.disabled = false;
  const validation = new FakeValidation();
  const documentListeners = new Map<string, Array<(event: { target: unknown }) => void>>();
  const fakeDocument = {
    activeElement: null,
    body: {},
    documentElement: {},
    addEventListener: (name: string, listener: (event: { target: unknown }) => void) => {
      const listeners = documentListeners.get(name) ?? [];
      listeners.push(listener);
      documentListeners.set(name, listeners);
    },
    querySelector: (selector: string) => {
      if (selector === "#prompt") return prompt;
      if (selector === '[data-action="send"]') return send;
      if (selector === "#settings-validation") return validation;
      if (selector === "#opacity-input") return opacityInput;
      return null;
    },
    querySelectorAll: (selector: string) => {
      if (selector === ".settings-control") return [settingsInput, doclingToggle, doclingUrl];
      if (selector === ".settings-modal button[data-action]") {
        return [apply, save, doclingReadiness];
      }
      return [];
    },
  };
  const fakeWindow = {
    getComputedStyle: () => ({ maxHeight: "200" }),
    localStorage: { getItem: () => null, setItem: () => undefined },
    addEventListener: () => undefined,
  };
  const previousGlobals = new Map(
    ["document", "window", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLSelectElement", "HTMLButtonElement", "Element"]
      .map((name) => [name, Object.getOwnPropertyDescriptor(globalThis, name)] as const),
  );
  const defineGlobal = (name: string, value: unknown) => {
    Object.defineProperty(globalThis, name, { configurable: true, writable: true, value });
  };

  try {
    defineGlobal("document", fakeDocument);
    defineGlobal("window", fakeWindow);
    defineGlobal("HTMLElement", FakeHtmlElement);
    defineGlobal("HTMLInputElement", FakeInput);
    defineGlobal("HTMLTextAreaElement", FakePrompt);
    defineGlobal("HTMLSelectElement", FakeHtmlElement);
    defineGlobal("HTMLButtonElement", FakeButton);
    defineGlobal("Element", FakeHtmlElement);

    const rustProjection = projection({
      busy: true,
      agent_tree_active: true,
      composer_submit_mode: "steer",
      can_submit: true,
      enhance_enabled: false,
      draft_prompt: "",
      config_fields: [
        {
          key: "model.request_timeout_ms",
          value: "3600000",
          env_override: "MOYAI_REQUEST_TIMEOUT_MS",
          value_type: "integer",
          required: true,
          min_value: 1,
          max_value: 3600000,
          options: [],
        },
        {
          key: "docling.enabled",
          value: "true",
          env_override: null,
          value_type: "boolean",
          required: false,
          min_value: null,
          max_value: null,
          options: [],
        },
        {
          key: "docling.base_url",
          value: "http://127.0.0.1:5001",
          env_override: null,
          value_type: "string",
          required: true,
          min_value: null,
          max_value: null,
          options: [],
        },
      ],
    });
    const ui = createUiLocalState();
    reconcileUiDrafts(ui, null, rustProjection, null);
    const view = projectViewState(rustProjection, ui);
    const opacityMutations: Array<Record<string, unknown> | undefined> = [];
    const context = {
      uiState: ui,
      getProjection: () => rustProjection,
      getViewState: () => projectViewState(rustProjection, ui),
      getRenderModel: () => actionTestModel(projectViewState(rustProjection, ui), ui),
      invalidateCommandPaletteInsertion: () => undefined,
      mutate: async (name: string, args?: Record<string, unknown>) => {
        if (name === "set_window_opacity") opacityMutations.push(args);
        return rustProjection;
      },
      rerender: () => undefined,
    } as unknown as ActionContext;

    wireEvents(view, context);
    wireEvents(view, context);
    prompt.input("追加指示");

    assert.equal(send.disabled, false);
    assert.equal(send.title, "実行中のタスクへ追加指示を送信");
    assert.equal(send.getAttribute("aria-label"), "実行中のタスクへ追加指示を送信");
    opacityInput.dispatch("change");
    assert.deepEqual(
      opacityMutations,
      [{ percent: 80 }],
      "a retained Settings range keeps exactly one persistence listener across rerenders",
    );

    const dispatchSettings = (name: "input" | "change", value: string) => {
      settingsInput.value = value;
      for (const listener of documentListeners.get(name) ?? []) listener({ target: settingsInput });
    };
    dispatchSettings("input", "3599000");
    assert.equal(apply.disabled, false);
    assert.equal(save.disabled, false);
    assert.equal(apply.getAttribute("aria-disabled"), "false");
    assert.equal(doclingReadiness.disabled, true, "every Settings action consumes the fresh dirty model");

    dispatchSettings("input", "0");
    assert.equal(apply.disabled, true);
    assert.equal(save.disabled, true);
    assert.equal(settingsInput.getAttribute("aria-invalid"), "true");
    assert.match(validation.textContent, /model\.request_timeout_ms: 1 以上/);

    dispatchSettings("change", "3598000");
    assert.equal(apply.disabled, false);
    assert.equal(settingsInput.getAttribute("aria-invalid"), null);

    doclingUrl.value = "https://docling.example.test/convert?token=hidden";
    for (const listener of documentListeners.get("input") ?? []) listener({ target: doclingUrl });
    assert.equal(doclingUrl.getAttribute("aria-invalid"), "true");
    assert.match(validation.textContent, /docling\.base_url: URL にquery string/);

    doclingToggle.checked = false;
    for (const listener of documentListeners.get("change") ?? []) listener({ target: doclingToggle });
    assert.equal(
      doclingUrl.getAttribute("aria-invalid"),
      null,
      "the same complete draft context removes inline URL errors while Docling is disabled",
    );

    doclingToggle.checked = true;
    for (const listener of documentListeners.get("change") ?? []) listener({ target: doclingToggle });
    assert.equal(doclingUrl.getAttribute("aria-invalid"), "true");
  } finally {
    for (const [name, descriptor] of previousGlobals) {
      if (descriptor) Object.defineProperty(globalThis, name, descriptor);
      else delete (globalThis as Record<string, unknown>)[name];
    }
  }
});

test("local typing cannot reopen the hidden Rust finalizing gate", () => {
  const ui = createUiLocalState();
  const finalizing = projection({
    busy: false,
    agent_tree_active: false,
    can_submit: false,
    enhance_enabled: false,
  });
  reconcileUiDrafts(ui, null, finalizing, null);
  ui.drafts.prompt = "typed while the previous run is finalizing";

  const capabilities = deriveUiCapabilities(finalizing, ui);
  assert.equal(capabilities.canSubmit, false);
  assert.equal(capabilities.canEnhance, false);
  assert.equal(capabilities.canReviewUncommitted, false);
});

test("a run-start command is single-flight in local capability projection", () => {
  const ui = createUiLocalState();
  const state = projection();
  reconcileUiDrafts(ui, null, state, null);
  ui.drafts.prompt = "submit once";
  ui.runStartMutationPending = true;

  const capabilities = deriveUiCapabilities(state, ui);
  assert.equal(capabilities.canSubmit, false);
  assert.equal(capabilities.canEnhance, false);
  assert.equal(capabilities.canReviewUncommitted, false);
  assert.equal(capabilities.canSendEnhancedReview, false);
  assert.equal(capabilities.canSendRawReview, false);
  assert.equal(mutationAdmissionOpen(ui, "enhance_prompt"), false);
  const view = projectViewState(state, ui);
  assert.equal(view.background_mutation_pending, true);
  assert.equal(view.busy, true, "the local request remains visibly pending");
  assert.equal(
    actionById("cancel-run")?.enabled?.(view, { index: -1, value: "" }),
    false,
    "only Rust may publish the cancel capability",
  );
  const admitted = projectViewState(projection({
    can_cancel_run: true,
    stop_target: turnStopTarget(),
  }), ui);
  assert.equal(actionById("cancel-run")?.enabled?.(admitted, { index: -1, value: "" }), true);
  const targetless = projectViewState(projection({ can_cancel_run: true, stop_target: null }), ui);
  assert.equal(
    actionById("cancel-run")?.enabled?.(targetless, { index: -1, value: "" }),
    false,
    "the exact Stop target is part of the Rust-published cancellation capability",
  );
});

test("an external config owner mutation immediately rejects submit and review dispatch", () => {
  const ui = createUiLocalState();
  const state = projection();
  reconcileUiDrafts(ui, null, state, null);
  ui.drafts.prompt = "must wait for the access owner";
  ui.drafts.reviewDraft = "review must wait too";
  ui.externalConfigMutationPending = true;

  const capabilities = deriveUiCapabilities(state, ui);
  assert.equal(capabilities.canSubmit, false);
  assert.equal(capabilities.canEnhance, false);
  assert.equal(capabilities.canReviewUncommitted, false);
  assert.equal(capabilities.canSendEnhancedReview, false);
  assert.equal(capabilities.canSendRawReview, false);
  for (const mutation of [
    "submit_prompt",
    "review_uncommitted",
    "send_prompt_review",
    "enhance_prompt",
  ]) {
    assert.equal(mutationAdmissionOpen(ui, mutation), false, mutation);
  }
  assert.equal(
    mutationAdmissionOpen(ui, "cancel_run"),
    true,
    "Stop is independent from access/config persistence",
  );
  assert.equal(
    mutationAdmissionOpen(ui, "desktop_state"),
    true,
    "polling remains available while the config owner settles",
  );
});

test("durable sessions across projects restore their isolated unsent composer drafts", () => {
  const ui = createUiLocalState();
  const initial = projection({
    selected_session_title: "Session A",
    selected_session_index: 0,
    session_rows: [{ session_id: SESSION_A, label: "Session A" }],
    draft_target: { workspacePath: "C:/workspace", sessionId: SESSION_A, ownerGeneration: "1" },
    config_target: { workspacePath: "C:/workspace", sessionId: SESSION_A, configGeneration: 1 },
  });
  reconcileUiDrafts(ui, null, initial, null);
  ui.drafts.prompt = "session A draft";
  ui.drafts.imageInput = "session-a.png";
  ui.drafts.composerRevision += 1;
  ui.drafts.imageRevision += 1;

  const poll = projection({ projection_revision: "2", status_message: "poll" });
  const sameOwnerPoll = {
    ...poll,
    selected_session_title: "Session A",
    selected_session_index: 0,
    session_rows: [{ session_id: SESSION_A, label: "Session A" }],
    draft_target: { workspacePath: "C:/workspace", sessionId: SESSION_A, ownerGeneration: "1" },
    config_target: { workspacePath: "C:/workspace", sessionId: SESSION_A, configGeneration: "1" },
  };
  reconcileUiDrafts(ui, initial, sameOwnerPoll, null);
  assert.equal(ui.drafts.prompt, "session A draft");

  const switched = projection({
    projection_revision: "3",
    workspace_path: "C:/project-b",
    selected_session_title: "Session B",
    selected_project_index: 0,
    selected_session_index: 0,
    project_rows: [{ project_id: "project-b", label: "Project B", path: "C:/project-b" }],
    session_rows: [{ session_id: SESSION_B, label: "Session B" }],
    draft_target: { workspacePath: "C:/project-b", sessionId: SESSION_B, ownerGeneration: "2" },
    draft_prompt: "",
    image_input: "",
    config_target: { workspacePath: "C:/project-b", sessionId: SESSION_B, configGeneration: 1 },
  });
  reconcileUiDrafts(ui, sameOwnerPoll, switched, null);
  assert.equal(ui.drafts.prompt, "");
  assert.equal(ui.drafts.imageInput, "");

  ui.drafts.prompt = "session B draft!";
  ui.drafts.imageInput = "session-b.png";
  ui.drafts.composerRevision += 1;
  ui.drafts.imageRevision += 1;

  const returnedA = projection({
    projection_revision: "4",
    selected_session_title: "Session A",
    selected_project_index: 0,
    selected_session_index: 0,
    project_rows: [{ project_id: "project-a", label: "Project A", path: "C:/workspace" }],
    session_rows: [{ session_id: SESSION_A, label: "Session A" }],
    draft_target: { workspacePath: "C:/workspace", sessionId: SESSION_A, ownerGeneration: "3" },
    draft_prompt: "",
    image_input: "",
    config_target: { workspacePath: "C:/workspace", sessionId: SESSION_A, configGeneration: 1 },
  });
  reconcileUiDrafts(ui, switched, returnedA, null);
  assert.equal(ui.drafts.prompt, "session A draft");
  assert.equal(ui.drafts.imageInput, "session-a.png");
  assert.equal(projectViewState(returnedA, ui).draft_prompt, "session A draft");

  const returnedB = projection({
    projection_revision: "5",
    workspace_path: "C:/project-b",
    selected_session_title: "Session B",
    selected_project_index: 0,
    selected_session_index: 0,
    project_rows: [{ project_id: "project-b", label: "Project B", path: "C:/project-b" }],
    session_rows: [{ session_id: SESSION_B, label: "Session B" }],
    draft_target: { workspacePath: "C:/project-b", sessionId: SESSION_B, ownerGeneration: "4" },
    draft_prompt: "",
    image_input: "",
    config_target: { workspacePath: "C:/project-b", sessionId: SESSION_B, configGeneration: 1 },
  });
  reconcileUiDrafts(ui, returnedA, returnedB, null);
  assert.equal(ui.drafts.prompt, "session B draft!");
  assert.equal(ui.drafts.imageInput, "session-b.png");
  assert.equal(projectViewState(returnedB, ui).draft_prompt, "session B draft!");
});

test("one local new-session owner closes stale navigation, run, and config admission", () => {
  const rustProjection = projection({
    navigation_admission_open: true,
    background_mutation_pending: false,
  });
  const ui = createUiLocalState();
  const request = { mutationName: "new_chat" as const, token: {} };
  ui.activeNewSessionMutation = request;

  const pending = projectViewState(rustProjection, ui);
  assert.equal(pending.navigation_admission_open, false);
  assert.equal(pending.background_mutation_pending, true);
  assert.equal(actionById("new-chat")?.enabled?.(pending, { index: -1, value: "" }), false);
  for (const mutation of [
    "new_chat",
    "new_project_session",
    "select_session",
    "submit_prompt",
    "toggle_access_mode",
  ]) {
    assert.equal(mutationAdmissionOpen(ui, mutation), false, mutation);
  }

  ui.activeNewSessionMutation = null;
  assert.equal(projectViewState(rustProjection, ui).navigation_admission_open, true);
  assert.equal(mutationAdmissionOpen(ui, "new_project_session"), true);
});

test("slow pointer and Enter new-session commands disable the old composer after release and reopen on error", async () => {
  for (const activation of ["pointer", "Enter"] as const) {
    const rustProjection = projection({
      draft_prompt: "keep this draft",
      token_meter_label: "",
    });
    const ui = createUiLocalState();
    reconcileUiDrafts(ui, null, rustProjection, null);
    const lifecycle = new InteractionLifecycle<string>(() => true);
    const end = activation === "pointer"
      ? (lifecycle.beginPointer(41), lifecycle.capturePointerEnd(41))
      : (lifecycle.beginKey("Enter"), lifecycle.captureKeyEnd("Enter"));
    assert.notEqual(end, null);

    let composerHtml = renderComposer(projectViewState(rustProjection, ui));
    const renderCurrent = (): void => {
      composerHtml = renderComposer(projectViewState(rustProjection, ui));
    };
    let rejectCommand!: (reason?: unknown) => void;
    const slowFailure = new Promise<string>((_resolve, reject) => {
      rejectCommand = reject;
    });
    let commandCount = 0;
    const pending = dispatchNewSessionMutation(
      ui,
      "new_chat",
      lifecycle,
      () => {
        if (!lifecycle.defer("pending-render", true, true)) renderCurrent();
      },
      () => {
        commandCount += 1;
        return slowFailure;
      },
      () => renderCurrent(),
      () => renderCurrent(),
    );
    await Promise.resolve();
    assert.equal(commandCount, 1);
    assert.doesNotMatch(
      composerHtml,
      /<textarea id="prompt"[^>]* disabled>/,
      `${activation} keeps its initiating DOM connected until release`,
    );

    assert.equal(await dispatchNewSessionMutation(
      ui,
      "new_project_session",
      lifecycle,
      () => assert.fail("a repeated activation must not publish another owner"),
      async () => {
        commandCount += 1;
        return "duplicate";
      },
      () => assert.fail("a repeated activation has no response"),
      () => assert.fail("a repeated activation has no error owner"),
    ), false);
    assert.equal(commandCount, 1);

    const release = end!();
    if (release?.renderCurrent) renderCurrent();
    assert.match(
      composerHtml,
      /<textarea id="prompt"[^>]* disabled>/,
      `${activation} release renders the pending navigation owner before command settlement`,
    );
    assert.equal(ui.drafts.prompt, "keep this draft", "a disabled old textarea cannot edit the local draft");
    assert.equal(projectViewState(rustProjection, ui).navigation_admission_open, false);

    rejectCommand(new Error(`${activation} slow failure`));
    await assert.rejects(pending, /slow failure/);
    assert.equal(ui.activeNewSessionMutation, null);
    assert.doesNotMatch(
      composerHtml,
      /<textarea id="prompt"[^>]* disabled>/,
      `${activation} exact error cleanup re-enables the composer`,
    );
    assert.equal(projectViewState(rustProjection, ui).navigation_admission_open, true);
  }
});

test("Quick Chat interaction owners use exact durable session identity", () => {
  const quickChatA = projection({
    workspace_path: "C:/quick-chat",
    selected_project_index: -1,
    draft_target: {
      workspacePath: "C:/quick-chat",
      sessionId: QUICK_A,
      ownerGeneration: "17",
    },
  });
  const sameQuickChatAfterPoll = projection({
    workspace_path: "C:/quick-chat",
    selected_project_index: -1,
    draft_target: {
      workspacePath: "C:/quick-chat",
      sessionId: QUICK_A,
      ownerGeneration: "18",
    },
  });
  const quickChatB = projection({
    workspace_path: "C:/quick-chat",
    selected_project_index: -1,
    draft_target: {
      workspacePath: "C:/quick-chat",
      sessionId: "quick-b",
      ownerGeneration: "19",
    },
  });

  assert.equal(
    composerSessionOwner(quickChatA),
    composerSessionOwner(sameQuickChatAfterPoll),
    "command-owner generations do not split the same durable Quick Chat viewport",
  );
  assert.notEqual(
    composerSessionOwner(quickChatA),
    composerSessionOwner(quickChatB),
    "neighboring Quick Chat rows cannot share a viewport owner",
  );
});

test("composer command owner rejects an owner-generation ABA on the same new-session surface", () => {
  const ownerGenerationOne = projection({
    selected_session_index: -1,
    session_rows: [],
    draft_target: { workspacePath: "C:/workspace", sessionId: null, ownerGeneration: "1" },
  });
  const ownerGenerationThree = projection({
    selected_session_index: -1,
    session_rows: [],
    draft_target: { workspacePath: "C:/workspace", sessionId: null, ownerGeneration: "3" },
  });

  assert.equal(
    composerSessionOwner(ownerGenerationOne),
    composerSessionOwner(ownerGenerationThree),
    "the stable interaction owner still identifies the same new-session surface",
  );
  assert.notEqual(
    composerOwner(ownerGenerationOne),
    composerOwner(ownerGenerationThree),
    "a deferred focus request must reject the newer command owner after A-B-A navigation",
  );
});

test("synchronous new Quick Chat settlement exposes one canonical empty unowned composer", () => {
  const quickRow = {
    session_id: QUICK_A,
    label: "Quick A",
    title: "Quick A",
    status: "completed" as const,
    loaded_status: "idle" as const,
    archived: false,
    pending_permission_requests: 0,
    pending_user_input_requests: 0,
    short_id: QUICK_A,
  };
  const previous = projection({
    workspace_path: "C:/quick-chat",
    selected_project_index: -1,
    selected_session_index: 0,
    session_rows: [quickRow],
    chat_session_rows: [quickRow],
    draft_prompt: "previous quick draft",
    image_input: "",
    draft_target: { workspacePath: "C:/quick-chat", sessionId: QUICK_A, ownerGeneration: "7" },
  });
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, previous, null);
  ui.drafts.prompt = "unsent previous quick edit";
  ui.drafts.composerRevision += 1;

  const emptyRow = {
    row_kind: "empty_placeholder" as const,
    step: "00",
    title: "チャットはありません",
    body: "下の入力欄から依頼を送ると、通常チャットが作成されます。",
    file_changes: [],
  };
  const settled = projection({
    projection_revision: "2",
    workspace_path: "C:/quick-chat",
    selected_project_index: -1,
    selected_session_index: -1,
    selected_session_title: "セッション未選択",
    current_session_label: "新規チャット",
    session_rows: [],
    chat_session_rows: [quickRow],
    draft_prompt: "",
    image_input: "",
    draft_target: { workspacePath: "C:/quick-chat", sessionId: null, ownerGeneration: "8" },
    thread_empty: true,
    transcript_rows: [emptyRow],
    navigation_loading: false,
  });
  reconcileUiDrafts(ui, previous, settled, null);
  const view = projectViewState(settled, ui);

  assert.equal(view.selected_project_index, -1);
  assert.equal(view.selected_session_index, -1);
  assert.deepEqual(view.draft_target, {
    workspacePath: "C:/quick-chat",
    sessionId: null,
    ownerGeneration: "8",
  });
  assert.equal(ui.drafts.composerOwner, "C:/quick-chat\u00008\u0000new");
  assert.equal(view.draft_prompt, "");
  assert.equal(view.thread_empty, true);
  assert.deepEqual(view.transcript_rows, [emptyRow]);
  assert.notEqual(
    composerOwner(previous),
    composerOwner(settled),
    "the scheduled focus request must reject the previous durable Quick Chat owner",
  );
});

test("an action response does not clear text entered after dispatch", () => {
  const ui = createUiLocalState();
  const initial = projection({ draft_prompt: "first" });
  reconcileUiDrafts(ui, null, initial, null);
  const snapshot = captureDraftMutation(ui, "submit_prompt");

  ui.drafts.prompt = "next request";
  ui.drafts.composerRevision += 1;
  const response = projection({ projection_revision: "2", draft_prompt: "" });
  acknowledgeDraftMutation(ui, response, "submit_prompt", snapshot);
  reconcileUiDrafts(ui, initial, response, snapshot);

  assert.equal(ui.drafts.prompt, "next request");
});

test("run command acknowledgement waits for durable admission before clearing the draft", () => {
  const ui = createUiLocalState();
  const initial = projection({ draft_prompt: "sent request" });
  reconcileUiDrafts(ui, null, initial, null);
  const snapshot = captureDraftMutation(ui, "submit_prompt");
  const response = projection({
    projection_revision: "2",
    draft_prompt: "sent request",
    busy: true,
    can_submit: false,
  });

  acknowledgeDraftMutation(ui, response, "submit_prompt", snapshot);
  reconcileUiDrafts(ui, initial, response, snapshot);
  assert.equal(ui.drafts.prompt, "sent request");

  const newerPoll = projection({
    projection_revision: "3",
    composer_commit_generation: "1",
    draft_prompt: "",
    image_input: "",
    busy: true,
    can_submit: false,
  });
  reconcileUiDrafts(ui, initial, newerPoll, null);

  assert.equal(ui.drafts.prompt, "");
  assert.equal(ui.drafts.pendingRunSubmission, null);
});

test("new-session binding preserves follow-up text typed after send", () => {
  const ui = createUiLocalState();
  const initial = projection({
    draft_prompt: "first request",
    draft_target: { workspacePath: "C:/workspace", sessionId: null, ownerGeneration: "1" },
  });
  reconcileUiDrafts(ui, null, initial, null);
  const snapshot = captureDraftMutation(ui, "submit_prompt");
  ui.drafts.prompt = "follow-up typed while running";
  ui.drafts.composerRevision += 1;

  const response = projection({
    projection_revision: "2",
    draft_prompt: "",
    draft_target: { workspacePath: "C:/workspace", sessionId: null, ownerGeneration: "1" },
    busy: true,
  });
  acknowledgeDraftMutation(ui, response, "submit_prompt", snapshot);
  reconcileUiDrafts(ui, initial, response, snapshot);
  const bound = projection({
    projection_revision: "3",
    composer_commit_generation: "1",
    draft_prompt: "",
    draft_target: { workspacePath: "C:/workspace", sessionId: "created-session", ownerGeneration: "1" },
    busy: true,
  });
  reconcileUiDrafts(ui, response, bound, null);

  assert.equal(ui.drafts.prompt, "follow-up typed while running");
  assert.equal(ui.drafts.composerOwner, "C:/workspace\u00001\u0000created-session");
  assert.equal(ui.drafts.pendingRunSubmission, null);
});

test("new-session binding is registered before a newer poll can beat the command response", () => {
  const ui = createUiLocalState();
  const initial = projection({
    draft_prompt: "first request",
    draft_target: { workspacePath: "C:/workspace", sessionId: null, ownerGeneration: "1" },
  });
  reconcileUiDrafts(ui, null, initial, null);
  const snapshot = captureDraftMutation(ui, "submit_prompt");
  ui.drafts.prompt = "follow-up before command response";
  ui.drafts.composerRevision += 1;

  const boundPoll = projection({
    projection_revision: "3",
    composer_commit_generation: "1",
    draft_prompt: "",
    draft_target: { workspacePath: "C:/workspace", sessionId: "created-before-response", ownerGeneration: "1" },
    busy: true,
  });
  reconcileUiDrafts(ui, initial, boundPoll, null);
  const olderResponse = projection({
    projection_revision: "2",
    draft_prompt: "",
    draft_target: { workspacePath: "C:/workspace", sessionId: null, ownerGeneration: "2" },
    busy: true,
  });
  acknowledgeDraftMutation(ui, olderResponse, "submit_prompt", snapshot);

  assert.equal(ui.drafts.prompt, "follow-up before command response");
  assert.equal(ui.drafts.composerOwner, "C:/workspace\u00001\u0000created-before-response");
});

test("same-owner generation reset discards stale local composer text", () => {
  const ui = createUiLocalState();
  const initial = projection({
    draft_prompt: "server draft",
    draft_target: { workspacePath: "C:/workspace", sessionId: null, ownerGeneration: "4" },
  });
  reconcileUiDrafts(ui, null, initial, null);
  ui.drafts.prompt = "stale local draft";
  ui.drafts.composerRevision += 1;

  const reset = projection({
    projection_revision: "2",
    draft_prompt: "",
    draft_target: { workspacePath: "C:/workspace", sessionId: null, ownerGeneration: "5" },
  });
  reconcileUiDrafts(ui, initial, reset, null);

  assert.equal(ui.drafts.prompt, "");
});

test("an explicit new-session surface never resurrects an abandoned unowned draft", () => {
  const ui = createUiLocalState();
  const firstNew = projection({
    draft_prompt: "",
    image_input: "",
    selected_session_index: -1,
    session_rows: [],
    draft_target: { workspacePath: "C:/workspace", sessionId: null, ownerGeneration: "1" },
  });
  reconcileUiDrafts(ui, null, firstNew, null);
  ui.drafts.prompt = "abandoned new-session draft";
  ui.drafts.composerRevision += 1;

  const durable = projection({
    projection_revision: "2",
    draft_prompt: "",
    image_input: "",
    draft_target: { workspacePath: "C:/workspace", sessionId: SESSION_A, ownerGeneration: "2" },
  });
  reconcileUiDrafts(ui, firstNew, durable, null);
  const explicitNew = projection({
    projection_revision: "3",
    draft_prompt: "",
    image_input: "",
    selected_session_index: -1,
    session_rows: [],
    draft_target: { workspacePath: "C:/workspace", sessionId: null, ownerGeneration: "3" },
  });
  reconcileUiDrafts(ui, durable, explicitNew, null);

  assert.equal(ui.drafts.prompt, "");
});

test("failed run start releases an unconsumed pending submission", () => {
  const ui = createUiLocalState();
  const initial = projection({
    draft_target: { workspacePath: "C:/workspace", sessionId: null, ownerGeneration: "1" },
  });
  reconcileUiDrafts(ui, null, initial, null);
  const snapshot = captureDraftMutation(ui, "submit_prompt");
  assert.notEqual(ui.drafts.pendingRunSubmission, null);

  rejectDraftMutation(ui, "submit_prompt", snapshot);
  assert.equal(ui.drafts.pendingRunSubmission, null);
  assert.equal(snapshot?.runSettlement, "rejected");
});

test("deferred run-target conflict retains drafts across exact owner-generation drift", () => {
  const target = reviewTarget();
  const initial = projection({
    overlay: "prompt_review",
    review_target: target,
    draft_prompt: "rust prompt before dispatch",
    image_input: "rust-image-before.png",
    review_draft_text: "rust review before dispatch",
    composer_commit_generation: "0",
  });
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, initial, null);
  ui.drafts.prompt = "local prompt A";
  ui.drafts.imageInput = "local-image-a.png";
  ui.drafts.reviewDraft = "local review A";
  ui.drafts.composerRevision += 1;
  ui.drafts.imageRevision += 1;
  ui.drafts.reviewRevision += 1;
  const snapshot = captureDraftMutation(ui, "send_prompt_review");
  assert.notEqual(snapshot, null);

  const conflict = projection({
    projection_revision: "2",
    overlay: "prompt_review",
    review_target: target,
    draft_prompt: "rust replacement B",
    image_input: "rust-image-b.png",
    review_draft_text: "rust review B",
    composer_commit_generation: "1",
    draft_target: {
      workspacePath: "C:/workspace",
      sessionId: SESSION_A,
      ownerGeneration: "2",
    },
  });
  const lifecycle = new InteractionLifecycle<{
    previous: DesktopWebState;
    state: DesktopWebState;
    snapshot: typeof snapshot;
  }>(() => true);
  lifecycle.beginKey("Enter");
  const end = lifecycle.captureKeyEnd("Enter");
  assert.notEqual(end, null);
  assert.equal(lifecycle.defer({ previous: initial, state: conflict, snapshot }, false, true), true);

  rejectDraftMutation(ui, "send_prompt_review", snapshot);
  assert.equal(snapshot?.runSettlement, "rejected");
  assert.equal(ui.drafts.pendingRunSubmission, null);

  const release = end?.();
  assert.notEqual(release?.deferred, null);
  const deferred = release?.deferred;
  assert.ok(deferred);
  reconcileUiDrafts(ui, deferred.previous, deferred.state, deferred.snapshot);

  assert.equal(ui.drafts.prompt, "local prompt A");
  assert.equal(ui.drafts.imageInput, "local-image-a.png");
  assert.equal(ui.drafts.reviewDraft, "local review A");
  assert.equal(ui.drafts.composerCommitGeneration, "1");
  assert.equal(ui.drafts.composerOwner, `C:/workspace\u00002\u0000${SESSION_A}`);
});

test("synchronous run-target conflict retains same-owner local drafts", () => {
  const initial = projection({
    draft_prompt: "rust prompt A",
    image_input: "rust-image-a.png",
    composer_commit_generation: "0",
  });
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, initial, null);
  ui.drafts.prompt = "local prompt A";
  ui.drafts.imageInput = "local-image-a.png";
  ui.drafts.composerRevision += 1;
  ui.drafts.imageRevision += 1;
  const snapshot = captureDraftMutation(ui, "submit_prompt");

  rejectDraftMutation(ui, "submit_prompt", snapshot);
  const conflict = projection({
    projection_revision: "2",
    draft_prompt: "rust prompt B",
    image_input: "rust-image-b.png",
    composer_commit_generation: "1",
  });
  reconcileUiDrafts(ui, initial, conflict, snapshot);

  assert.equal(ui.drafts.prompt, "local prompt A");
  assert.equal(ui.drafts.imageInput, "local-image-a.png");
  assert.equal(ui.drafts.composerCommitGeneration, "1");
});

test("rejected new-session conflict retains the unowned draft across owner-generation drift", () => {
  const initial = projection({
    draft_prompt: "rust new-session draft",
    image_input: "rust-new.png",
    selected_session_index: -1,
    session_rows: [],
    draft_target: {
      workspacePath: "C:/workspace",
      sessionId: null,
      ownerGeneration: "7",
    },
  });
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, initial, null);
  ui.drafts.prompt = "local unowned request";
  ui.drafts.imageInput = "local-unowned.png";
  ui.drafts.composerRevision += 1;
  ui.drafts.imageRevision += 1;
  const snapshot = captureDraftMutation(ui, "submit_prompt");
  rejectDraftMutation(ui, "submit_prompt", snapshot);

  const conflict = projection({
    projection_revision: "2",
    composer_commit_generation: "1",
    draft_prompt: "rust replacement",
    image_input: "rust-replacement.png",
    selected_session_index: -1,
    session_rows: [],
    draft_target: {
      workspacePath: "C:/workspace",
      sessionId: null,
      ownerGeneration: "8",
    },
  });
  reconcileUiDrafts(ui, initial, conflict, snapshot);

  assert.equal(ui.drafts.prompt, "local unowned request");
  assert.equal(ui.drafts.imageInput, "local-unowned.png");
  assert.equal(ui.drafts.composerOwner, "C:/workspace\u00008\u0000new");
});

test("pre-admission runtime failure preserves the retryable prompt and image", () => {
  const ui = createUiLocalState();
  const initial = projection({ draft_prompt: "retry me", image_input: "diagram.png" });
  reconcileUiDrafts(ui, null, initial, null);
  const snapshot = captureDraftMutation(ui, "submit_prompt");
  const launched = projection({
    projection_revision: "2",
    draft_prompt: "retry me",
    image_input: "diagram.png",
    busy: true,
    can_submit: false,
  });
  acknowledgeDraftMutation(ui, launched, "submit_prompt", snapshot);
  reconcileUiDrafts(ui, initial, launched, snapshot);

  const failed = projection({
    projection_revision: "3",
    draft_prompt: "retry me",
    image_input: "diagram.png",
    busy: false,
    can_submit: true,
    run_status_key: "failed",
  });
  reconcileUiDrafts(ui, launched, failed, null);

  assert.equal(ui.drafts.prompt, "retry me");
  assert.equal(ui.drafts.imageInput, "diagram.png");
  assert.equal(ui.drafts.pendingRunSubmission, null);
  assert.equal(ui.drafts.composerCommitGeneration, "0");
});

test("successful admission clears only the dispatched image and prompt revisions", () => {
  const ui = createUiLocalState();
  const initial = projection({ draft_prompt: "sent", image_input: "sent.png" });
  reconcileUiDrafts(ui, null, initial, null);
  captureDraftMutation(ui, "submit_prompt");
  ui.drafts.prompt = "next";
  ui.drafts.imageInput = "next.png";
  ui.drafts.composerRevision += 1;
  ui.drafts.imageRevision += 1;

  const admitted = projection({
    projection_revision: "2",
    composer_commit_generation: "1",
    draft_prompt: "",
    image_input: "",
    busy: true,
    can_submit: false,
  });
  reconcileUiDrafts(ui, initial, admitted, null);

  assert.equal(ui.drafts.prompt, "next");
  assert.equal(ui.drafts.imageInput, "next.png");
  assert.equal(ui.drafts.pendingRunSubmission, null);
});

test("review draft is retained until the reviewed run is durably admitted", () => {
  const ui = createUiLocalState();
  const target = reviewTarget();
  const initial = projection({
    overlay: "prompt_review",
    review_target: target,
    review_draft_text: "edited enhanced request",
  });
  reconcileUiDrafts(ui, null, initial, null);
  const snapshot = captureDraftMutation(ui, "send_prompt_review");
  const launchAccepted = projection({
    projection_revision: "2",
    overlay: "prompt_review",
    review_target: target,
    review_draft_text: "edited enhanced request",
    busy: true,
    can_submit: false,
  });
  acknowledgeDraftMutation(ui, launchAccepted, "send_prompt_review", snapshot);
  reconcileUiDrafts(ui, initial, launchAccepted, snapshot);
  assert.equal(ui.drafts.reviewDraft, "edited enhanced request");

  const admitted = projection({
    projection_revision: "3",
    composer_commit_generation: "1",
    overlay: "none",
    review_target: null,
    review_draft_text: "",
    busy: true,
    can_submit: false,
  });
  reconcileUiDrafts(ui, launchAccepted, admitted, null);
  assert.equal(ui.drafts.reviewDraft, "");
});

test("same-overlay prompt enhancement completion hydrates an untouched local review draft", () => {
  const ui = createUiLocalState();
  const target = reviewTarget();
  const enhancing = projection({
    overlay: "prompt_review",
    review_target: target,
    review_draft_text: "",
    review_status_text: "Enhancing",
  });
  reconcileUiDrafts(ui, null, enhancing, null);

  const reviewing = projection({
    projection_revision: "2",
    overlay: "prompt_review",
    review_target: target,
    review_draft_text: "enhanced request",
    review_status_text: "Reviewing",
  });
  reconcileUiDrafts(ui, enhancing, reviewing, null);

  assert.equal(ui.drafts.reviewDraft, "enhanced request");
  assert.equal(projectViewState(reviewing, ui).review_draft_text, "enhanced request");
});

test("late same-owner enhancement cannot overwrite a local review edit", () => {
  const ui = createUiLocalState();
  const target = reviewTarget();
  const enhancing = projection({
    overlay: "prompt_review",
    review_target: target,
    review_draft_text: "",
    review_status_text: "Enhancing",
  });
  reconcileUiDrafts(ui, null, enhancing, null);
  ui.drafts.reviewDraft = "user refinement";
  ui.drafts.reviewRevision += 1;

  const staleCompletion = projection({
    projection_revision: "2",
    overlay: "prompt_review",
    review_target: target,
    review_draft_text: "late enhanced request",
    review_status_text: "Reviewing",
  });
  reconcileUiDrafts(ui, enhancing, staleCompletion, null);

  assert.equal(ui.drafts.reviewDraft, "user refinement");
  assert.equal(projectViewState(staleCompletion, ui).review_draft_text, "user refinement");
});

test("prompt review owner change replaces an old owner's local edit", () => {
  const ui = createUiLocalState();
  const ownerA = projection({
    overlay: "prompt_review",
    review_target: reviewTarget(),
    review_draft_text: "owner A review",
    draft_target: { workspacePath: "C:/workspace", sessionId: SESSION_A, ownerGeneration: "1" },
  });
  reconcileUiDrafts(ui, null, ownerA, null);
  ui.drafts.reviewDraft = "owner A local edit";
  ui.drafts.reviewRevision += 1;

  const ownerB = projection({
    projection_revision: "2",
    overlay: "prompt_review",
    review_target: reviewTarget({ sessionId: SESSION_B, ownerGeneration: "2", requestId: "9007199254740994" }),
    review_draft_text: "owner B review",
    draft_target: { workspacePath: "C:/workspace", sessionId: SESSION_B, ownerGeneration: "2" },
  });
  reconcileUiDrafts(ui, ownerA, ownerB, null);

  assert.equal(ui.drafts.reviewDraft, "owner B review");
  assert.equal(projectViewState(ownerB, ui).review_draft_text, "owner B review");
});

test("every exact prompt-review target field rebases a dirty local draft and its revisions", async () => {
  const idleTargetA = reviewTarget();
  const turnTargetA = reviewTarget({
    expectedState: { kind: "turn", turnId: TURN_A, admissionRevision: "4" },
  });
  const targetVariants: Array<[
    string,
    PromptReviewMutationTarget,
    PromptReviewMutationTarget,
  ]> = [
    ["workspacePath", idleTargetA, reviewTarget({ workspacePath: "D:/other-workspace" })],
    ["sessionId", idleTargetA, reviewTarget({ sessionId: SESSION_B })],
    ["ownerGeneration", idleTargetA, reviewTarget({ ownerGeneration: "2" })],
    ["requestId", idleTargetA, reviewTarget({ requestId: "9007199254740994" })],
    [
      "expectedState.kind",
      idleTargetA,
      reviewTarget({
        expectedState: { kind: "turn", turnId: TURN_A, admissionRevision: "4" },
      }),
    ],
    [
      "expectedState.latestTurnId",
      idleTargetA,
      reviewTarget({
        expectedState: { kind: "idle", latestTurnId: TURN_B, admissionRevision: "4" },
      }),
    ],
    [
      "expectedState.turnId",
      turnTargetA,
      reviewTarget({
        expectedState: { kind: "turn", turnId: TURN_B, admissionRevision: "4" },
      }),
    ],
    [
      "expectedState.idle.admissionRevision",
      idleTargetA,
      reviewTarget({
        expectedState: { kind: "idle", latestTurnId: TURN_A, admissionRevision: "5" },
      }),
    ],
    [
      "expectedState.turn.admissionRevision",
      turnTargetA,
      reviewTarget({
        expectedState: { kind: "turn", turnId: TURN_A, admissionRevision: "5" },
      }),
    ],
  ];

  for (const [changedField, targetA, targetB] of targetVariants) {
    const ui = createUiLocalState();
    const ownerA = projection({
      overlay: "prompt_review",
      review_target: targetA,
      review_draft_text: "canonical A review",
    });
    reconcileUiDrafts(ui, null, ownerA, null);
    ui.drafts.reviewDraft = "dirty local A review";
    ui.drafts.reviewRevision += 1;
    const dirtyRevision = ui.drafts.reviewRevision;
    const staleSnapshot = captureDraftMutation(ui, "cancel_prompt_review");

    const ownerB = projection({
      projection_revision: "2",
      workspace_path: targetB.workspacePath,
      draft_target: {
        workspacePath: targetB.workspacePath,
        sessionId: targetB.sessionId,
        ownerGeneration: targetB.ownerGeneration,
      },
      overlay: "prompt_review",
      review_target: targetB,
      review_draft_text: `canonical B review: ${changedField}`,
    });
    reconcileUiDrafts(ui, ownerA, ownerB, null);

    assert.deepEqual(ui.drafts.reviewTarget, targetB, changedField);
    assert.notEqual(ui.drafts.reviewTarget, targetB, changedField);
    assert.notEqual(ui.drafts.reviewTarget?.expectedState, targetB.expectedState, changedField);
    assert.equal(ui.drafts.reviewDraft, `canonical B review: ${changedField}`, changedField);
    assert.ok(ui.drafts.reviewRevision > dirtyRevision, changedField);
    assert.equal(ui.drafts.reviewSyncedRevision, ui.drafts.reviewRevision, changedField);
    assert.equal(
      projectViewState(ownerB, ui).review_draft_text,
      `canonical B review: ${changedField}`,
      changedField,
    );
    if (changedField === "requestId") {
      assert.equal(
        composerOwner(ownerA),
        composerOwner(ownerB),
        "request replacement is distinct even when the composer owner is unchanged",
      );
    }

    const staleResponse = projection({
      projection_revision: "1",
      overlay: "prompt_review",
      review_target: targetA,
      review_draft_text: "stale A acknowledgement",
    });
    acknowledgeDraftMutation(ui, staleResponse, "cancel_prompt_review", staleSnapshot);
    assert.equal(ui.drafts.reviewDraft, `canonical B review: ${changedField}`, changedField);

    if (changedField === "requestId") {
      const sends: Array<{ name: string; args?: Record<string, unknown> }> = [];
      const currentView = projectViewState(ownerB, ui);
      await dispatchGuiAction(
        "send-review-enhanced",
        -1,
        "",
        currentView,
        {
          uiState: ui,
          mutate: async (name, args) => { sends.push({ name, args }); },
        } as unknown as ActionContext,
      );
      assert.deepEqual(sends, [{
        name: "send_prompt_review",
        args: {
          enhanced: true,
          text: "canonical B review: requestId",
          expectedTarget: targetB,
          expectedRunTarget: currentView.run_target,
        },
      }]);
    }
  }
});

test("stale prompt review acknowledgement cannot cross an owner change", () => {
  const ui = createUiLocalState();
  const ownerA = projection({
    overlay: "prompt_review",
    review_target: reviewTarget(),
    review_draft_text: "owner A review",
    draft_target: { workspacePath: "C:/workspace", sessionId: SESSION_A, ownerGeneration: "1" },
  });
  reconcileUiDrafts(ui, null, ownerA, null);
  const staleSnapshot = captureDraftMutation(ui, "cancel_prompt_review");

  const ownerB = projection({
    projection_revision: "3",
    overlay: "prompt_review",
    review_target: reviewTarget({ sessionId: SESSION_B, ownerGeneration: "2", requestId: "9007199254740994" }),
    review_draft_text: "owner B review",
    draft_target: { workspacePath: "C:/workspace", sessionId: SESSION_B, ownerGeneration: "2" },
  });
  reconcileUiDrafts(ui, ownerA, ownerB, null);

  const staleResponse = projection({
    projection_revision: "2",
    overlay: "none",
    review_target: null,
    review_draft_text: "",
    draft_target: { workspacePath: "C:/workspace", sessionId: SESSION_A, ownerGeneration: "1" },
  });
  acknowledgeDraftMutation(ui, staleResponse, "cancel_prompt_review", staleSnapshot);

  assert.equal(ui.drafts.reviewDraft, "owner B review");
  assert.equal(projectViewState(ownerB, ui).review_draft_text, "owner B review");
});

test("same-owner navigation acknowledgement cannot clear an unsaved composer draft", () => {
  const ui = createUiLocalState();
  const initial = projection({ draft_prompt: "server" });
  reconcileUiDrafts(ui, null, initial, null);
  ui.drafts.prompt = "unsaved local";
  ui.drafts.composerRevision += 1;

  const snapshot = captureDraftMutation(ui, "select_project");
  const response = projection({ projection_revision: "2", draft_prompt: "server" });
  acknowledgeDraftMutation(ui, response, "select_project", snapshot);
  reconcileUiDrafts(ui, initial, response, snapshot);

  assert.equal(snapshot, null);
  assert.equal(ui.drafts.prompt, "unsaved local");
});

class FakeInteractionEventTarget {
  private readonly listeners = new Map<string, Set<EventListenerOrEventListenerObject>>();

  addEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject | null,
    _options?: boolean | AddEventListenerOptions,
  ): void {
    if (!listener) return;
    const listeners = this.listeners.get(type) ?? new Set<EventListenerOrEventListenerObject>();
    listeners.add(listener);
    this.listeners.set(type, listeners);
  }

  removeEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject | null,
    _options?: boolean | EventListenerOptions,
  ): void {
    if (listener) this.listeners.get(type)?.delete(listener);
  }

  dispatch(type: string, values: Record<string, unknown> = {}): void {
    const event = { type, ...values } as unknown as Event;
    for (const listener of Array.from(this.listeners.get(type) ?? [])) {
      if (typeof listener === "function") listener.call(this, event);
      else listener.handleEvent(event);
    }
  }
}

class FakeInteractionWindow extends FakeInteractionEventTarget {
  private now = 0;
  private nextTimerId = 1;
  private readonly timers = new Map<number, { at: number; handler: TimerHandler; args: unknown[] }>();

  setTimeout(handler: TimerHandler, timeout = 0, ...args: unknown[]): number {
    const id = this.nextTimerId++;
    this.timers.set(id, { at: this.now + Math.max(0, timeout), handler, args });
    return id;
  }

  clearTimeout(id: number | undefined): void {
    if (id !== undefined) this.timers.delete(id);
  }

  advanceBy(milliseconds: number): void {
    const target = this.now + milliseconds;
    while (true) {
      const next = Array.from(this.timers.entries())
        .filter(([, timer]) => timer.at <= target)
        .sort((left, right) => left[1].at - right[1].at || left[0] - right[0])[0];
      if (!next) break;
      const [id, timer] = next;
      this.timers.delete(id);
      this.now = timer.at;
      if (typeof timer.handler !== "function") throw new Error("string timers are not supported by the fake clock");
      timer.handler(...timer.args);
    }
    this.now = target;
  }
}

class FakeInteractionDocument extends FakeInteractionEventTarget {
  hidden = false;
  body: FakeInteractionElement | null = null;
  documentElement: FakeInteractionElement | null = null;
}

class FakeInteractionElement extends FakeInteractionEventTarget {
  disabled = false;
  action = false;
  modal = false;
  readonly capturedPointers: number[] = [];
  projection = "revision-1";
  replacements = 0;
  readonly kind: "html" | "body" | "div" | "input";
  readonly parentElement: FakeInteractionElement | null;

  constructor(
    kind: "html" | "body" | "div" | "input",
    parentElement: FakeInteractionElement | null = null,
  ) {
    super();
    this.kind = kind;
    this.parentElement = parentElement;
  }

  contains(target: Node | null): boolean {
    let current = target as unknown as FakeInteractionElement | null;
    while (current) {
      if (current === this) return true;
      current = current.parentElement;
    }
    return false;
  }

  closest<E extends Element = Element>(selectors: string): E | null {
    let candidate: FakeInteractionElement | null = this;
    while (candidate) {
      const matches = selectors === ":disabled" ? candidate.disabled
        : selectors === "[data-action]" ? candidate.action
        : selectors === "[data-modal]" ? candidate.modal
        : candidate.kind === "input";
      if (matches) return candidate as unknown as E;
      candidate = candidate.parentElement;
    }
    return null;
  }

  matches(selectors: string): boolean {
    return selectors === ":disabled" && this.disabled;
  }

  setPointerCapture(pointerId: number): void { this.capturedPointers.push(pointerId); }

  applyProjection(revision: number): void {
    this.projection = `revision-${revision}`;
    this.replacements += 1;
  }
}

interface InteractionGateHarness {
  documentTarget: FakeInteractionDocument;
  windowTarget: FakeInteractionWindow;
  documentElement: FakeInteractionElement;
  body: FakeInteractionElement;
  appRoot: FakeInteractionElement;
  input: FakeInteractionElement;
  unrelated: FakeInteractionElement;
  disabledInput: FakeInteractionElement;
  lifecycle: InteractionLifecycle<number>;
  applied: number[];
  queueProjection: (revision: number) => void;
  dispose: () => void;
}

function openInteractionGate(): { harness: InteractionGateHarness; close: () => void } {
  const elementDescriptor = Object.getOwnPropertyDescriptor(globalThis, "Element");
  Object.defineProperty(globalThis, "Element", {
    configurable: true,
    writable: true,
    value: FakeInteractionElement,
  });
  const documentTarget = new FakeInteractionDocument();
  const windowTarget = new FakeInteractionWindow();
  const documentElement = new FakeInteractionElement("html");
  const body = new FakeInteractionElement("body", documentElement);
  const appRoot = new FakeInteractionElement("div", body);
  const input = new FakeInteractionElement("input", appRoot);
  const unrelated = new FakeInteractionElement("div", body);
  const disabledInput = new FakeInteractionElement("input", appRoot);
  disabledInput.disabled = true;
  documentTarget.documentElement = documentElement;
  documentTarget.body = body;
  const lifecycle = new InteractionLifecycle<number>((current, candidate) => candidate > current);
  const applied: number[] = [];
  const applyProjection = (revision: number): void => {
    appRoot.applyProjection(revision);
    applied.push(revision);
  };
  const dispose = installInteractionEventGate({
    documentTarget: documentTarget as unknown as Document,
    windowTarget: windowTarget as unknown as Window,
    appRoot: appRoot as unknown as Element,
    lifecycle,
    finish: (release) => {
      if (release?.deferred !== null && release?.deferred !== undefined) {
        applyProjection(release.deferred);
      }
    },
  });

  const harness = {
    documentTarget,
    windowTarget,
    documentElement,
    body,
    appRoot,
    input,
    unrelated,
    disabledInput,
    lifecycle,
    applied,
    queueProjection: (revision: number) => {
      if (!lifecycle.defer(revision, false, true)) applyProjection(revision);
    },
    dispose,
  };
  return {
    harness,
    close: () => {
    dispose();
    if (elementDescriptor) Object.defineProperty(globalThis, "Element", elementDescriptor);
    else delete (globalThis as Record<string, unknown>).Element;
    },
  };
}

function withInteractionGate(run: (harness: InteractionGateHarness) => void): void {
  const gate = openInteractionGate();
  try {
    run(gate.harness);
  } finally {
    gate.close();
  }
}

async function withInteractionGateAsync(
  run: (harness: InteractionGateHarness) => Promise<void>,
): Promise<void> {
  const gate = openInteractionGate();
  try {
    await run(gate.harness);
  } finally {
    gate.close();
  }
}

function createFastCommandPaletteActivation(
  lifecycle: InteractionLifecycle<number>,
  interactionGeneration: () => bigint,
  failure: Error | null = null,
): {
  activate: () => Promise<void>;
  readonly commandCalls: number;
  readonly insertions: number;
  readonly value: string;
} {
  const source = "左😀選択右";
  const prefix = "左😀";
  const selected = "選択";
  const insertionText = "/case ";
  let value = source;
  let commandCalls = 0;
  let insertions = 0;
  let nextRequestId = 1;
  let completion: Promise<void> | null = null;
  const owner = new CommandPaletteInsertionAsyncOwner();
  return {
    activate(): Promise<void> {
      const request = { requestId: nextRequestId++ } as CommandPaletteInsertionRequest;
      const capturedGeneration = interactionGeneration();
      const dispatched = dispatchCommandPaletteInsertion(
        owner,
        request,
        lifecycle,
        async () => {
          commandCalls += 1;
          if (failure) throw failure;
          return insertionText;
        },
        (canonicalInsertionText) => {
          if (interactionGeneration() !== capturedGeneration) return null;
          const replacement = replaceUtf16Selection(
            value,
            prefix.length,
            prefix.length + selected.length,
            canonicalInsertionText,
          );
          if (!replacement) return null;
          value = replacement.value;
          insertions += 1;
          return replacement;
        },
      ).then(() => undefined);
      if (owner.activeRequest === request) completion = dispatched;
      return completion ?? dispatched;
    },
    get commandCalls(): number {
      return commandCalls;
    },
    get insertions(): number {
      return insertions;
    },
    get value(): string {
      return value;
    },
  };
}

test("selecting dialog text keeps pointer events inside the dialog while projections wait for release", () => {
  withInteractionGate(({ documentTarget, windowTarget, appRoot, lifecycle, queueProjection, applied }) => {
    const backdrop = new FakeInteractionElement("div", appRoot);
    backdrop.action = true;
    const dialog = new FakeInteractionElement("div", backdrop);
    dialog.modal = true;
    const text = new FakeInteractionElement("div", dialog);

    documentTarget.dispatch("pointerdown", { target: text, button: 0, pointerId: 21 });
    assert.equal(lifecycle.active, true);
    assert.deepEqual(backdrop.capturedPointers, [], "dialog text must not redirect pointerup/click to the dismiss action");
    queueProjection(2);
    assert.deepEqual(applied, []);
    documentTarget.dispatch("pointerup", { target: text, pointerId: 21 });
    windowTarget.advanceBy(0);
    assert.deepEqual(applied, [2]);
  });
});

test("dialog buttons, text inputs, and genuine backdrop clicks retain their pointer capture owner", () => {
  withInteractionGate(({ documentTarget, appRoot }) => {
    const backdrop = new FakeInteractionElement("div", appRoot);
    backdrop.action = true;
    const dialog = new FakeInteractionElement("div", backdrop);
    dialog.modal = true;
    const button = new FakeInteractionElement("div", dialog);
    button.action = true;
    const icon = new FakeInteractionElement("div", button);
    const input = new FakeInteractionElement("input", dialog);
    documentTarget.dispatch("pointerdown", { target: icon, button: 0, pointerId: 22 });
    documentTarget.dispatch("pointerdown", { target: input, button: 0, pointerId: 23 });
    documentTarget.dispatch("pointerdown", { target: backdrop, button: 0, pointerId: 24 });
    button.disabled = true;
    documentTarget.dispatch("pointerdown", { target: icon, button: 0, pointerId: 25 });
    assert.deepEqual(button.capturedPointers, [22]);
    assert.deepEqual(input.capturedPointers, [23]);
    assert.deepEqual(backdrop.capturedPointers, [24]);
  });
});

test("pointer activation starts one command before the deferred release and settles a fast response once", async () => {
  await withInteractionGateAsync(async ({ documentTarget, windowTarget, input, lifecycle }) => {
    let interactionGeneration = 0n;
    documentTarget.addEventListener("pointerdown", () => { interactionGeneration += 1n; }, true);
    const activation = createFastCommandPaletteActivation(lifecycle, () => interactionGeneration);

    documentTarget.dispatch("pointerdown", { target: input, button: 0, pointerId: 7 });
    documentTarget.dispatch("pointerup", { target: input, pointerId: 7 });
    assert.equal(lifecycle.active, true, "pointerup defers release until its zero-delay callback");
    const completion = activation.activate();
    void activation.activate();
    assert.equal(activation.commandCalls, 1);
    await Promise.resolve();
    await Promise.resolve();
    assert.equal(activation.insertions, 0, "the fast response cannot settle during capture ownership");

    windowTarget.advanceBy(0);
    assert.equal(lifecycle.active, false);
    await completion;
    assert.equal(activation.commandCalls, 1);
    assert.equal(activation.insertions, 1);
    assert.equal(activation.value, "左😀/case 右");
  });
});

for (const code of ["Enter", "Space"] as const) {
  test(`${code} activation starts one command while key capture is active and settles after release`, async () => {
    await withInteractionGateAsync(async ({ documentTarget, windowTarget, input, lifecycle }) => {
      let interactionGeneration = 0n;
      documentTarget.addEventListener("keydown", (event) => {
        if (shouldInvalidateCommandPaletteInsertionForKeydown(
          (event as unknown as { repeat?: boolean }).repeat ?? false,
        )) interactionGeneration += 1n;
      }, true);
      const activation = createFastCommandPaletteActivation(lifecycle, () => interactionGeneration);

      documentTarget.dispatch("keydown", { target: input, code, isComposing: false, repeat: false });
      assert.equal(lifecycle.active, true);
      let completion: Promise<void>;
      if (code === "Enter") {
        completion = activation.activate();
        await Promise.resolve();
        await Promise.resolve();
      }
      documentTarget.dispatch("keyup", { target: input, code });
      assert.equal(lifecycle.active, true, "keyup keeps capture until its zero-delay callback");
      if (code === "Space") {
        completion = activation.activate();
        await Promise.resolve();
        await Promise.resolve();
      }
      void activation.activate();
      assert.equal(activation.commandCalls, 1);
      assert.equal(activation.insertions, 0);

      windowTarget.advanceBy(0);
      assert.equal(lifecycle.active, false);
      await completion!;
      assert.equal(activation.commandCalls, 1);
      assert.equal(activation.insertions, 1);
      assert.equal(activation.value, "左😀/case 右");
    });
  });
}

test("a new unrelated interaction invalidates a fast response waiting on the initiating release", async () => {
  await withInteractionGateAsync(async ({ documentTarget, windowTarget, input, lifecycle }) => {
    let interactionGeneration = 0n;
    documentTarget.addEventListener("pointerdown", () => { interactionGeneration += 1n; }, true);
    documentTarget.addEventListener("keydown", (event) => {
      if (shouldInvalidateCommandPaletteInsertionForKeydown(
        (event as unknown as { repeat?: boolean }).repeat ?? false,
      )) interactionGeneration += 1n;
    }, true);
    const activation = createFastCommandPaletteActivation(lifecycle, () => interactionGeneration);

    documentTarget.dispatch("pointerdown", { target: input, button: 0, pointerId: 7 });
    const completion = activation.activate();
    await Promise.resolve();
    await Promise.resolve();
    documentTarget.dispatch("keydown", { target: input, code: "KeyA", isComposing: false, repeat: false });
    documentTarget.dispatch("pointerup", { target: input, pointerId: 7 });
    windowTarget.advanceBy(0);
    assert.equal(lifecycle.active, true, "the unrelated key still owns the lifecycle");
    assert.equal(activation.insertions, 0);

    documentTarget.dispatch("keyup", { target: input, code: "KeyA" });
    windowTarget.advanceBy(0);
    assert.equal(lifecycle.active, false);
    await completion;
    assert.equal(activation.commandCalls, 1);
    assert.equal(activation.insertions, 0);
    assert.equal(activation.value, "左😀選択右");
  });
});

test("held Enter repeats stay within one activation and produce one insertion", async () => {
  await withInteractionGateAsync(async ({ documentTarget, windowTarget, input, lifecycle }) => {
    let interactionGeneration = 0n;
    documentTarget.addEventListener("keydown", (event) => {
      if (shouldInvalidateCommandPaletteInsertionForKeydown(
        (event as unknown as { repeat?: boolean }).repeat ?? false,
      )) interactionGeneration += 1n;
    }, true);
    const activation = createFastCommandPaletteActivation(lifecycle, () => interactionGeneration);

    documentTarget.dispatch("keydown", {
      target: input,
      code: "Enter",
      isComposing: false,
      repeat: false,
    });
    const completion = activation.activate();
    await Promise.resolve();
    await Promise.resolve();
    for (let repeat = 0; repeat < 3; repeat += 1) {
      documentTarget.dispatch("keydown", {
        target: input,
        code: "Enter",
        isComposing: false,
        repeat: true,
      });
      void activation.activate();
    }
    assert.equal(interactionGeneration, 1n, "repeat keydowns belong to the initiating key hold");
    assert.equal(activation.commandCalls, 1, "native repeat clicks remain single-flight");

    documentTarget.dispatch("keyup", { target: input, code: "Enter" });
    windowTarget.advanceBy(0);
    await completion;
    assert.equal(activation.commandCalls, 1);
    assert.equal(activation.insertions, 1);
    assert.equal(activation.value, "左😀/case 右");
  });
});

test("a fast command error stays single-flight through held Enter release", async () => {
  await withInteractionGateAsync(async ({ documentTarget, windowTarget, input, lifecycle }) => {
    let interactionGeneration = 0n;
    documentTarget.addEventListener("keydown", (event) => {
      if (shouldInvalidateCommandPaletteInsertionForKeydown(
        (event as unknown as { repeat?: boolean }).repeat ?? false,
      )) interactionGeneration += 1n;
    }, true);
    const activation = createFastCommandPaletteActivation(
      lifecycle,
      () => interactionGeneration,
      new Error("fast command failure"),
    );

    documentTarget.dispatch("keydown", {
      target: input,
      code: "Enter",
      isComposing: false,
      repeat: false,
    });
    const completion = activation.activate();
    const rejected = assert.rejects(completion, /fast command failure/);
    await Promise.resolve();
    await Promise.resolve();
    for (let repeat = 0; repeat < 3; repeat += 1) {
      documentTarget.dispatch("keydown", {
        target: input,
        code: "Enter",
        isComposing: false,
        repeat: true,
      });
      void activation.activate();
    }
    assert.equal(activation.commandCalls, 1);

    documentTarget.dispatch("keyup", { target: input, code: "Enter" });
    windowTarget.advanceBy(0);
    await rejected;
    assert.equal(activation.commandCalls, 1);
    assert.equal(activation.insertions, 0);
  });
});

for (const recovery of ["blur", "pagehide", "hidden"] as const) {
  test(`${recovery} invalidates a fast response after lifecycle recovery resolves its idle wait`, async () => {
    await withInteractionGateAsync(async ({ documentTarget, windowTarget, input, lifecycle }) => {
      let interactionGeneration = 0n;
      documentTarget.addEventListener("pointerdown", () => { interactionGeneration += 1n; }, true);
      if (recovery === "hidden") {
        documentTarget.addEventListener("visibilitychange", () => {
          if (documentTarget.hidden) interactionGeneration += 1n;
        });
      } else {
        windowTarget.addEventListener(recovery, () => { interactionGeneration += 1n; });
      }
      const activation = createFastCommandPaletteActivation(lifecycle, () => interactionGeneration);

      documentTarget.dispatch("pointerdown", { target: input, button: 0, pointerId: 7 });
      const completion = activation.activate();
      await Promise.resolve();
      await Promise.resolve();
      assert.equal(lifecycle.active, true);
      assert.equal(activation.insertions, 0);

      if (recovery === "hidden") {
        documentTarget.hidden = true;
        documentTarget.dispatch("visibilitychange");
      } else {
        windowTarget.dispatch(recovery);
      }
      assert.equal(lifecycle.active, false, "the recovery listener releases capture ownership first");
      await completion;
      assert.equal(activation.commandCalls, 1);
      assert.equal(activation.insertions, 0, "the later invalidator in the same event wins before settlement");
      assert.equal(activation.value, "左😀選択右");
    });
  });
}

test("interaction lifecycle holds one newest projection across pointer, keyboard, and IME", () => {
  const lifecycle = new InteractionLifecycle<number>((current, candidate) => candidate > current);
  lifecycle.beginPointer(7);
  lifecycle.beginKey("Enter");
  lifecycle.beginComposition();
  const endPointer = lifecycle.capturePointerEnd(7);
  const endKey = lifecycle.captureKeyEnd("Enter");
  const endComposition = lifecycle.captureCompositionEnd();

  assert.equal(lifecycle.defer(2, false, true), true);
  assert.equal(lifecycle.defer(1, false, true), true);
  assert.equal(endPointer?.(), null);
  assert.equal(endKey?.(), null);
  assert.deepEqual(endComposition?.(), { deferred: 2, renderCurrent: false });
  assert.equal(lifecycle.active, false);
});

test("a paused IME keeps the DOM stable until compositionend releases only the newest projection", () => {
  withInteractionGate(({ documentTarget, windowTarget, appRoot, lifecycle, applied, queueProjection }) => {
    documentTarget.dispatch("compositionstart");
    queueProjection(2);
    queueProjection(3);

    windowTarget.advanceBy(60_000);
    assert.equal(lifecycle.active, true);
    assert.equal(appRoot.projection, "revision-1");
    assert.equal(appRoot.replacements, 0);

    documentTarget.dispatch("compositionend");
    assert.equal(appRoot.projection, "revision-1", "the compositionend event settles after its input event turn");
    windowTarget.advanceBy(0);
    assert.equal(lifecycle.active, false);
    assert.equal(appRoot.projection, "revision-3");
    assert.deepEqual(applied, [3]);
    windowTarget.advanceBy(60_000);
    assert.deepEqual(applied, [3]);
  });
});

test("a stationary pointer keeps the DOM stable until lost capture releases only the newest projection", () => {
  withInteractionGate(({ documentTarget, windowTarget, appRoot, input, lifecycle, applied, queueProjection }) => {
    documentTarget.dispatch("pointerdown", { target: input, button: 0, pointerId: 7 });
    queueProjection(2);
    queueProjection(4);

    windowTarget.advanceBy(60_000);
    assert.equal(lifecycle.active, true);
    assert.equal(appRoot.projection, "revision-1");
    assert.equal(appRoot.replacements, 0);

    documentTarget.dispatch("lostpointercapture", { target: input, pointerId: 7 });
    windowTarget.advanceBy(0);
    assert.equal(lifecycle.active, false);
    assert.equal(appRoot.projection, "revision-4");
    assert.deepEqual(applied, [4]);
    windowTarget.advanceBy(60_000);
    assert.deepEqual(applied, [4]);
  });
});

test("a held key keeps the DOM stable until window blur explicitly recovers the lifecycle", () => {
  withInteractionGate(({ documentTarget, windowTarget, appRoot, input, lifecycle, applied, queueProjection }) => {
    documentTarget.dispatch("keydown", {
      target: input,
      code: "ArrowDown",
      isComposing: false,
    });
    queueProjection(5);

    windowTarget.advanceBy(60_000);
    assert.equal(lifecycle.active, true);
    assert.equal(appRoot.projection, "revision-1");

    windowTarget.dispatch("blur");
    assert.equal(lifecycle.active, false);
    assert.equal(appRoot.projection, "revision-5");
    assert.deepEqual(applied, [5]);
  });
});

test("document visibility loss explicitly recovers a missing compositionend", () => {
  withInteractionGate(({ documentTarget, appRoot, lifecycle, applied, queueProjection }) => {
    documentTarget.dispatch("compositionstart");
    queueProjection(6);
    documentTarget.hidden = true;

    documentTarget.dispatch("visibilitychange");

    assert.equal(lifecycle.active, false);
    assert.equal(appRoot.projection, "revision-6");
    assert.deepEqual(applied, [6]);
  });
});

test("installed keyboard events admit BODY and HTML reading operations but reject unrelated and disabled targets", () => {
  withInteractionGate(({
    documentTarget,
    windowTarget,
    documentElement,
    body,
    unrelated,
    disabledInput,
    lifecycle,
  }) => {
    documentTarget.dispatch("keydown", { target: body, code: "PageDown", isComposing: false });
    assert.equal(lifecycle.active, true, "BODY owns document-level reading keys");
    documentTarget.dispatch("keyup", { target: body, code: "PageDown" });
    windowTarget.advanceBy(0);
    assert.equal(lifecycle.active, false);

    documentTarget.dispatch("keydown", { target: documentElement, code: "ArrowDown", isComposing: false });
    assert.equal(lifecycle.active, true, "HTML owns document-level reading keys");
    windowTarget.dispatch("blur");
    assert.equal(lifecycle.active, false);

    documentTarget.dispatch("keydown", { target: unrelated, code: "ArrowDown", isComposing: false });
    assert.equal(lifecycle.active, false, "an unrelated element outside #app is not a document owner");
    documentTarget.dispatch("keydown", { target: disabledInput, code: "Enter", isComposing: false });
    assert.equal(lifecycle.active, false, "disabled controls remain outside keyboard admission");
  });
});

test("delayed pointer termination cannot end a newer generation with the same pointer id", () => {
  for (const termination of ["pointerup", "lostpointercapture"] as const) {
    withInteractionGate(({ documentTarget, windowTarget, appRoot, input, lifecycle, applied, queueProjection }) => {
      documentTarget.dispatch("pointerdown", { target: input, button: 0, pointerId: 9 });
      queueProjection(2);
      documentTarget.dispatch(termination, { target: input, pointerId: 9 });
      documentTarget.dispatch("pointerdown", { target: input, button: 0, pointerId: 9 });
      queueProjection(3);

      windowTarget.advanceBy(0);
      assert.equal(lifecycle.active, true, termination);
      assert.equal(appRoot.projection, "revision-1", termination);
      assert.deepEqual(applied, [], termination);

      documentTarget.dispatch("pointerup", { target: input, pointerId: 9 });
      windowTarget.advanceBy(0);
      assert.equal(lifecycle.active, false, termination);
      assert.equal(appRoot.projection, "revision-3", termination);
      assert.deepEqual(applied, [3], termination);
    });
  }
});

test("delayed keyup cannot end a newer generation with the same key code", () => {
  withInteractionGate(({ documentTarget, windowTarget, appRoot, input, lifecycle, applied, queueProjection }) => {
    documentTarget.dispatch("keydown", { target: input, code: "ArrowDown", isComposing: false });
    queueProjection(2);
    documentTarget.dispatch("keyup", { target: input, code: "ArrowDown" });
    documentTarget.dispatch("keydown", { target: input, code: "ArrowDown", isComposing: false });
    queueProjection(4);

    windowTarget.advanceBy(0);
    assert.equal(lifecycle.active, true);
    assert.equal(appRoot.projection, "revision-1");
    assert.deepEqual(applied, []);

    documentTarget.dispatch("keyup", { target: input, code: "ArrowDown" });
    windowTarget.advanceBy(0);
    assert.equal(lifecycle.active, false);
    assert.equal(appRoot.projection, "revision-4");
    assert.deepEqual(applied, [4]);
  });
});

test("duplicate delayed compositionend callbacks cannot end a newer composition", () => {
  withInteractionGate(({ documentTarget, windowTarget, appRoot, lifecycle, applied, queueProjection }) => {
    documentTarget.dispatch("compositionstart");
    queueProjection(2);
    documentTarget.dispatch("compositionend");
    documentTarget.dispatch("compositionend");
    documentTarget.dispatch("compositionstart");
    queueProjection(5);

    windowTarget.advanceBy(0);
    assert.equal(lifecycle.active, true);
    assert.equal(appRoot.projection, "revision-1");
    assert.deepEqual(applied, []);

    documentTarget.dispatch("compositionend");
    windowTarget.advanceBy(0);
    assert.equal(lifecycle.active, false);
    assert.equal(appRoot.projection, "revision-5");
    assert.deepEqual(applied, [5]);
  });
});

test("double termination releases a deferred projection exactly once", () => {
  withInteractionGate(({ documentTarget, windowTarget, input, lifecycle, applied, queueProjection }) => {
    documentTarget.dispatch("pointerdown", { target: input, button: 0, pointerId: 11 });
    queueProjection(6);
    documentTarget.dispatch("pointerup", { target: input, pointerId: 11 });
    documentTarget.dispatch("lostpointercapture", { target: input, pointerId: 11 });

    windowTarget.advanceBy(0);
    assert.equal(lifecycle.active, false);
    assert.deepEqual(applied, [6]);
    windowTarget.dispatch("blur");
    assert.deepEqual(applied, [6]);
  });
});

test("blur invalidates a queued end before the same owner begins again", () => {
  withInteractionGate(({ documentTarget, windowTarget, appRoot, input, lifecycle, applied, queueProjection }) => {
    documentTarget.dispatch("pointerdown", { target: input, button: 0, pointerId: 13 });
    queueProjection(2);
    documentTarget.dispatch("pointerup", { target: input, pointerId: 13 });
    windowTarget.dispatch("blur");
    assert.deepEqual(applied, [2]);

    documentTarget.dispatch("pointerdown", { target: input, button: 0, pointerId: 13 });
    queueProjection(7);
    windowTarget.advanceBy(0);
    assert.equal(lifecycle.active, true);
    assert.equal(appRoot.projection, "revision-2");
    assert.deepEqual(applied, [2]);

    documentTarget.dispatch("pointercancel", { target: input, pointerId: 13 });
    assert.equal(lifecycle.active, false);
    assert.equal(appRoot.projection, "revision-7");
    assert.deepEqual(applied, [2, 7]);
  });
});

test("disposing the installed gate drops deferred state and cancels queued end callbacks", () => {
  withInteractionGate(({ documentTarget, windowTarget, appRoot, input, lifecycle, applied, queueProjection, dispose }) => {
    documentTarget.dispatch("pointerdown", { target: input, button: 0, pointerId: 15 });
    queueProjection(8);
    documentTarget.dispatch("pointerup", { target: input, pointerId: 15 });

    dispose();
    windowTarget.advanceBy(0);

    assert.equal(lifecycle.active, false);
    assert.equal(appRoot.projection, "revision-1");
    assert.deepEqual(applied, []);
  });
});

test("non-control text selection, scrolling, and document keyboard reading start the lifecycle", () => {
  assert.equal(shouldBeginPointerInteraction(0, true), true);
  assert.equal(shouldBeginPointerInteraction(2, true), false);
  assert.equal(shouldBeginKeyboardInteraction(false, "PageDown", true, false), true);
  assert.equal(shouldBeginKeyboardInteraction(false, "ArrowDown", true, false), true);
  assert.equal(shouldBeginKeyboardInteraction(true, "KeyA", true, false), false);
  assert.equal(shouldBeginKeyboardInteraction(false, "Enter", true, true), false);
});

test("frontend config validation matches integer and floating-point field shapes", () => {
  const integer = {
    key: "multi_agent.max_concurrent_agents",
    value: "4",
    env_override: null,
    value_type: "integer",
    required: false,
    min_value: 1,
    max_value: null,
    options: [],
  };
  const number = { ...integer, key: "model.temperature", value_type: "number", min_value: null };
  const responseTimeout = {
    ...integer,
    key: "model.request_timeout_ms",
    value: "3600000",
    max_value: 3600000,
  };
  const requiredModel = {
    ...integer,
    key: "model.model",
    value_type: "string",
    required: true,
    min_value: null,
  };
  assert.equal(validateConfigInput(integer, "1.5").ok, false);
  assert.equal(validateConfigInput(integer, "0").ok, false);
  assert.equal(validateConfigInput(integer, "4").ok, true);
  for (const valid of ["0.2", "+1", "-1.25", ".5", "1.", "6.02e23", "-2E-3"]) {
    assert.equal(validateConfigInput(number, valid).ok, true, valid);
  }
  for (const invalid of ["0x10", "0b10", "0o10", "Infinity", "-Infinity", "NaN"]) {
    assert.equal(validateConfigInput(number, invalid).ok, false, invalid);
  }
  assert.equal(validateConfigInput(responseTimeout, "0").ok, false);
  assert.equal(validateConfigInput(responseTimeout, "3600000").ok, true);
  assert.equal(validateConfigInput(responseTimeout, "3600001").ok, false);
  assert.equal(validateConfigInput(requiredModel, "").ok, false);
  assert.equal(validateConfigInput(number, "").ok, true, "optional floating values may be cleared");
  assert.deepEqual(
    validateConfigFieldValues([responseTimeout], [{ key: responseTimeout.key, text: "0" }]),
    {
      ok: false,
      invalidKey: "model.request_timeout_ms",
      message: "1 以上の数値を入力してください。",
    },
  );
  assert.deepEqual(
    configCommitControlState(true, false),
    { disabled: true, ariaDisabled: "true" },
    "local validation closes an otherwise-open Rust commit capability",
  );
  assert.deepEqual(configCommitControlState(true, true), { disabled: false, ariaDisabled: "false" });
  assert.deepEqual(configCommitControlState(false, true), { disabled: true, ariaDisabled: "true" });
});

test("frontend provider URL validation mirrors the Rust ProviderEndpoint boundary", () => {
  for (const valid of [
    "http://localhost:1234",
    "HTTP://LOCALHOST:80/v1/",
    "https://provider.example/proxy/openai/v1",
  ]) {
    assert.equal(validateProviderBaseUrl(valid).ok, true, valid);
    const raw = projection({ provider_base_url: valid });
    const ui = createUiLocalState();
    reconcileUiDrafts(ui, null, raw, null);
    assert.equal(deriveUiCapabilities(raw, ui).canLoadProviderModels, true, valid);
  }
  for (const invalid of [
    "",
    "file:///tmp/provider.sock",
    "https://user:secret@provider.example/v1",
    "https://provider.example/v1?api_key=hidden",
    "https://provider.example/v1#hidden",
  ]) {
    const validation = validateProviderBaseUrl(invalid);
    assert.equal(validation.ok, false, invalid);
    assert.equal(validation.canonicalBaseUrl, "", invalid);
    assert.equal(validation.message.includes("secret"), false);
    assert.equal(validation.message.includes("hidden"), false);
    const raw = projection({ provider_base_url: invalid });
    const ui = createUiLocalState();
    reconcileUiDrafts(ui, null, raw, null);
    const view = projectViewState(raw, ui);
    assert.equal(deriveUiCapabilities(raw, ui).canLoadProviderModels, false, invalid);
    assert.equal(view.provider_apply_enabled, false, invalid);
    assert.equal(paletteActions(view).some((action) => action.id === "load-provider-models"), false, invalid);
  }
  const systemPromptField = {
    key: "model.system_prompt",
    value: "",
    env_override: null,
    value_type: "string",
    required: false,
    min_value: null,
    max_value: null,
    options: [],
  };
  for (const key of ["model.system_prompt", "side_chat.system_prompt"]) {
    const field = { ...systemPromptField, key };
    assert.equal(
      validateConfigInput(
        field,
        `  ${"😀".repeat(USER_CONFIGURED_SYSTEM_PROMPT_MAX_CHARS)}  `,
      ).ok,
      true,
    );
    assert.equal(
      validateConfigInput(
        field,
        "😀".repeat(USER_CONFIGURED_SYSTEM_PROMPT_MAX_CHARS + 1),
      ).ok,
      false,
    );
  }
});

test("Docling base URL validation follows the enabled value in the same complete draft", () => {
  const field = (
    key: string,
    value: string,
    valueType: ConfigFieldProjection["value_type"] = "string",
  ): ConfigFieldProjection => ({
    key,
    value,
    env_override: null,
    value_type: valueType,
    required: true,
    min_value: null,
    max_value: null,
    options: [],
  });
  const enabled = field("docling.enabled", "true", "boolean");
  const baseUrl = field("docling.base_url", "https://docling.example.test/api");
  const values = (enabledValue: string, baseUrlValue: string) => [
    { key: enabled.key, text: enabledValue },
    { key: baseUrl.key, text: baseUrlValue },
  ];
  const fields = [enabled, baseUrl];

  for (const retained of [
    "",
    "inactive-draft",
    "https://user:secret@docling.example.test/api",
    "https://docling.example.test/api?token=hidden",
    "https://docling.example.test/api#hidden",
  ]) {
    const disabledValues = values("false", retained);
    assert.equal(validateConfigInput(baseUrl, retained, disabledValues).ok, true, retained);
    assert.equal(validateConfigFieldValues(fields, disabledValues).ok, true, retained);
  }

  const validEnabled = values("true", "HTTPS://DOCLING.EXAMPLE.TEST/api/");
  assert.equal(validateConfigInput(baseUrl, validEnabled[1].text, validEnabled).ok, true);
  assert.equal(validateConfigFieldValues(fields, validEnabled).ok, true);

  for (const rejected of [
    "https://user:secret@docling.example.test/api",
    "https://docling.example.test/api?token=hidden",
    "https://docling.example.test/api#hidden",
  ]) {
    const enabledValues = values("true", rejected);
    assert.equal(validateConfigInput(baseUrl, rejected, enabledValues).ok, false, rejected);
    assert.deepEqual(validateConfigFieldValues(fields, enabledValues).invalidKey, "docling.base_url");
  }

  const modelBaseUrl = field("model.base_url", "https://provider.example/v1?token=hidden");
  const modelValues = [
    { key: enabled.key, text: "false" },
    { key: baseUrl.key, text: "inactive-draft" },
    { key: modelBaseUrl.key, text: modelBaseUrl.value },
  ];
  assert.deepEqual(
    validateConfigFieldValues([enabled, baseUrl, modelBaseUrl], modelValues).invalidKey,
    "model.base_url",
    "disabling Docling must not change the Main provider URL boundary",
  );
});

test("local full-draft validation owns Settings actions and command-palette admission", () => {
  const timeoutField: ConfigFieldProjection = {
    key: "model.request_timeout_ms",
    value: "3600000",
    env_override: "MOYAI_REQUEST_TIMEOUT_MS",
    value_type: "integer",
    required: true,
    min_value: 1,
    max_value: 3600000,
    options: [],
  };
  const rustProjection = projection({ config_fields: [timeoutField] });
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, rustProjection, null);
  const setDraft = (text: string) => updateConfigDraftValue(
    ui,
    rustProjection.config_target,
    rustProjection.config_fields.map((field) => ({ key: field.key, text: field.value })),
    timeoutField.key,
    text,
  );

  setDraft("0");
  const invalid = projectViewState(rustProjection, ui);
  assert.equal(invalid.config_draft.commit_enabled, false);
  assert.equal(invalid.config_draft.external_owner_mutation_open, false);
  assert.equal(invalid.config_draft.access_mode_mutation_enabled, false);
  assert.equal(invalid.provider_apply_enabled, false);
  for (const id of [
    "apply-session-config",
    "save-global-config",
    "toggle-access",
    "apply-provider-session",
    "save-provider-global",
  ]) {
    assert.equal(actionById(id)?.enabled?.(invalid, { index: -1, value: "" }), false, id);
    assert.equal(paletteActions(invalid, true).some((action) => action.id === id), false, id);
  }

  setDraft("3599000");
  const valid = projectViewState(rustProjection, ui);
  assert.equal(valid.config_draft.commit_enabled, true);
  assert.equal(valid.config_draft.external_owner_mutation_open, false);
  assert.equal(valid.config_draft.access_mode_mutation_enabled, false);
  assert.equal(valid.provider_apply_enabled, false);
  for (const id of ["apply-session-config", "save-global-config"]) {
    assert.equal(actionById(id)?.enabled?.(valid, { index: -1, value: "" }), true, id);
    assert.equal(paletteActions(valid, true).some((action) => action.id === id), true, id);
  }
  for (const id of ["toggle-access", "apply-provider-session", "save-provider-global"]) {
    assert.equal(actionById(id)?.enabled?.(valid, { index: -1, value: "" }), false, id);
    assert.equal(paletteActions(valid, true).some((action) => action.id === id), false, id);
  }

  setDraft("3600000");
  const revertedClean = projectViewState(rustProjection, ui);
  assert.equal(revertedClean.config_draft.commit_enabled, false);
  assert.equal(revertedClean.config_draft.external_owner_mutation_open, true);
  assert.equal(revertedClean.config_draft.access_mode_mutation_enabled, true);
  assert.equal(revertedClean.provider_apply_enabled, true);
  for (const id of ["toggle-access", "apply-provider-session", "save-provider-global"]) {
    assert.equal(actionById(id)?.enabled?.(revertedClean, { index: -1, value: "" }), true, id);
    assert.equal(paletteActions(revertedClean, true).some((action) => action.id === id), true, id);
  }

  const invalidRustBaseline = projection({
    config_fields: [{ ...timeoutField, value: "0" }],
  });
  const invalidCleanUi = createUiLocalState();
  reconcileUiDrafts(invalidCleanUi, null, invalidRustBaseline, null);
  const invalidClean = projectViewState(invalidRustBaseline, invalidCleanUi);
  assert.equal(invalidClean.config_draft.dirty, false);
  assert.equal(invalidClean.config_draft.external_owner_mutation_open, false);
  assert.equal(invalidClean.config_draft.access_mode_mutation_enabled, false);
  assert.equal(invalidClean.provider_apply_enabled, false);
  for (const id of ["toggle-access", "apply-provider-session", "save-provider-global"]) {
    assert.equal(actionById(id)?.enabled?.(invalidClean, { index: -1, value: "" }), false, id);
    assert.equal(paletteActions(invalidClean).some((action) => action.id === id), false, id);
  }

  ui.activeConfigMutationGeneration = 1n;
  const pending = projectViewState(rustProjection, ui);
  assert.equal(pending.config_draft.commit_enabled, false);
});

test("global action shortcuts ignore key-repeat activation", () => {
  const f8 = { key: "F8", ctrlKey: false, metaKey: false, repeat: false };
  assert.equal(globalShortcutAction(f8), "toggle-access");
  assert.equal(globalShortcutAction({ ...f8, repeat: true }), null);
  assert.equal(globalShortcutAction({ key: "Enter", ctrlKey: true, metaKey: false, repeat: false }), "send");
  assert.equal(globalShortcutAction({ key: "Enter", ctrlKey: true, metaKey: false, repeat: true }), null);
  const ctrlN = { key: "n", ctrlKey: true, metaKey: false, repeat: false };
  assert.equal(globalShortcutAction(ctrlN), "new-chat");
  assert.equal(globalShortcutAction({ ...ctrlN, repeat: true }), null);
});

test("every visible new-chat route exposes one exact typed focus identity", () => {
  assert.match(
    renderSidebar(projection()),
    /data-action="new-chat" data-focus-key="quick-chat:new-session"/,
  );
  assert.match(
    renderOverlay(projection({ overlay: "file_menu" })),
    /data-action="new-chat" data-focus-key="titlebar-menu:file:new-chat" data-titlebar-menu-action/,
  );
  assert.match(
    renderOverlay(projection({
      overlay: "command_palette",
      local_search_text: "",
      local_search_results_text: "",
      command_rows: [],
    })),
    /data-action="new-chat" data-focus-key="palette-action:new-chat"/,
  );
  assert.match(
    renderOverlay(projection({ overlay: "shortcuts" })),
    /data-action="new-chat" data-focus-key="shortcut-action:new-chat"/,
  );
});

test("access modes use the Codex-aligned Japanese labels", () => {
  assert.deepEqual(
    ["default", "auto_review", "full_access"].map(displayAccessLabel),
    ["承認を求める", "代理で承認", "フルアクセス"],
  );
});

test("permission visibility does not stop runtime polling", () => {
  assert.equal(autoRefreshAllowed({ navigation_loading: false, confirmation_visible: true }, false), true);
  assert.equal(autoRefreshAllowed({ navigation_loading: false, confirmation_visible: true }, true), false);
  assert.equal(autoRefreshAllowed({ navigation_loading: true, confirmation_visible: true }, true), true);
});

test("run admission polls for the Rust owner before the start command responds", () => {
  assert.equal(runtimePollingRequired(false, false), false);
  assert.equal(runtimePollingRequired(true, false), true);
  assert.equal(runtimePollingRequired(false, true), true);
});

test("a Hub command starts ordinary polling before the Desktop snapshot advertises its connection", () => {
  const idle = { status: "disconnected" as const, active_main: null, active_side_chat: null };
  assert.equal(runtimePollingRequired(false, false, idle), false);
  assert.equal(runtimePollingRequired(false, false, { ...idle, status: "connecting" }), true);
  assert.equal(runtimePollingRequired(false, false, { ...idle, status: "connected" }), true);
  assert.equal(runtimePollingRequired(false, false, { ...idle, status: "stale" }), false);
  assert.equal(runtimePollingRequired(false, false, { ...idle, status: "error" }), false);
  assert.equal(runtimePollingRequired(false, false, {
    ...idle, active_side_chat: { turn_id: "side-turn", phase: "running", logical_model_id: "model" },
  }), true);
});

test("workspace browser submits its local draft with the authoritative draft owner", async () => {
  const state = projection({ workspace_input: "D:/next-workspace" });
  let invocation: { name: string; args?: Record<string, unknown> } | null = null;
  const context = {
    mutate: async (name: string, args?: Record<string, unknown>) => {
      invocation = { name, args };
    },
  } as unknown as ActionContext;

  await actionById("browse-workspace")?.run(state, context, { index: -1, value: "" });

  assert.deepEqual(invocation, {
    name: "browse_workspace",
    args: {
      text: "D:/next-workspace",
      expectedTarget: {
        workspacePath: "C:/workspace",
        sessionId: SESSION_A,
        ownerGeneration: "1",
      },
    },
  });
});

test("search and attachment actions carry their authoritative owners", async () => {
  const state = projection({ attached_images: ["C:/workspace/reference.png"] });
  const invocations: Array<{ name: string; args?: Record<string, unknown> }> = [];
  const context = {
    mutate: async (name: string, args?: Record<string, unknown>) => {
      invocations.push({ name, args });
    },
    prepareConfigSnapshot: () => state.config_fields.map((field) => ({
      key: field.key,
      text: field.value,
    })),
  } as unknown as ActionContext;

  await actionById("toggle-session-archived-search")?.run(state, context, { index: -1, value: "" });
  await actionById("clear-images")?.run(state, context, { index: -1, value: "" });
  await actionById("toggle-access")?.run(state, context, { index: -1, value: "" });

  assert.deepEqual(sessionSearchMutationTarget(state), {
    workspacePath: "C:/workspace",
    projectId: "project-a",
  });
  assert.deepEqual(invocations, [
    {
      name: "set_session_search_include_archived",
      args: {
        includeArchived: true,
        expectedTarget: { workspacePath: "C:/workspace", projectId: "project-a" },
      },
    },
    {
      name: "clear_images",
      args: {
        expectedTarget: {
          workspacePath: "C:/workspace",
          sessionId: SESSION_A,
          ownerGeneration: "1",
        },
      },
    },
    {
      name: "toggle_access_mode",
      args: {
        expectedTarget: {
          workspacePath: "C:/workspace",
          sessionId: SESSION_A,
          configGeneration: "1",
          accessMode: "default",
          runtimeOwnerToken: "idle:0",
        },
        draftValues: [{ key: "model.model", text: "model-a" }],
      },
    },
  ]);
  assert.equal(
    actionById("toggle-access")?.enabled?.(
      projection({
        config_draft: {
          ...projection().config_draft,
          access_mode_mutation_enabled: false,
        },
      }),
      { index: -1, value: "" },
    ),
    false,
  );
  const exportDisabled = projection({ history_export_enabled: false });
  assert.equal(
    actionById("export-history")?.enabled(exportDisabled, { index: -1, value: "" }),
    false,
  );
});

test("session-row interrupt carries the exact active turn and fails closed without it", async () => {
  const base = projection();
  const activeRow = {
    ...base.session_rows[0]!,
    loaded_status: "active" as const,
    active_turn_id: TURN_A,
    active_turn_sequence_no: 4,
    interrupt_target: rootStopTarget(),
  };
  const state = projection({ session_rows: [activeRow] });
  const invocations: Array<{ name: string; args?: Record<string, unknown> }> = [];
  const context = {
    mutate: async (name: string, args?: Record<string, unknown>) => {
      invocations.push({ name, args });
    },
  } as unknown as ActionContext;

  await actionById("interrupt-session")?.run(state, context, { index: 0, value: "" });
  assert.deepEqual(invocations, [{
    name: "interrupt_session",
    args: {
      index: 0,
      expectedTarget: {
        workspacePath: "C:/workspace",
        ownerProjectId: "project-a",
        ownerSessionId: SESSION_A,
        rowId: SESSION_A,
      },
      expectedStopTarget: activeRow.interrupt_target,
    },
  }]);

  invocations.length = 0;
  const missingTarget = projection({
    session_rows: [{ ...activeRow, interrupt_target: null }],
  });
  await actionById("interrupt-session")?.run(missingTarget, context, { index: 0, value: "" });
  assert.deepEqual(invocations, []);
});

test("stable row identity and selected-state semantics survive list reordering", () => {
  const state = projection();
  const sidebar = renderSidebar(state);
  assert.match(sidebar, /data-focus-key="project:project-a:select" aria-current="page"/);
  assert.match(sidebar, new RegExp(`data-focus-key="session:${SESSION_A}:select" aria-current="page"`));
  const childOnlyActive = renderSidebar(projection({
    busy: false,
    agent_tree_active: true,
    task_activity_state: "running",
  }));
  assert.match(
    childOnlyActive,
    new RegExp(`data-focus-key="session:${SESSION_A}:select"[^>]*>[\\s\\S]*?task-activity-indicator`),
  );
  assert.doesNotMatch(childOnlyActive, /busy-spinner/);

  const artifact = renderArtifactPane(projection({
    artifact_rows: [{ label: "report", path: "C:/workspace/report.md", kind: "file", action: "created" }],
    selected_artifact_index: 0,
  }));
  assert.match(artifact, /data-focus-key="artifact:C:\/workspace\/report\.md" aria-current="true"/);
});

test("project and quick-chat rows expose selected and background task activity", () => {
  const buttonFor = (html: string, focusKey: string): string => {
    const marker = `data-focus-key="${focusKey}:select"`;
    const start = html.indexOf(marker);
    assert.notEqual(start, -1, `missing ${focusKey}`);
    const end = html.indexOf("</button>", start);
    assert.notEqual(end, -1, `unterminated ${focusKey}`);
    return html.slice(start, end + "</button>".length);
  };
  const base = projection().session_rows[0]!;
  const selected = {
    ...base,
    loaded_status: "active" as const,
    pending_permission_requests: 1,
  };
  const background = {
    ...base,
    session_id: SESSION_B,
    short_id: SESSION_B,
    label: "Session B",
    title: "Session B",
    loaded_status: "active" as const,
  };
  const attention = {
    ...background,
    session_id: "session-c",
    short_id: "session-c",
    label: "Session C",
    title: "Session C",
    pending_user_input_requests: 1,
  };
  const inactive = {
    ...base,
    session_id: "session-d",
    short_id: "session-d",
    label: "Session D",
    title: "Session D",
  };
  const projectSidebar = renderSidebar(projection({
    agent_tree_active: true,
    task_activity_state: "finalizing",
    session_rows: [selected, background, attention, inactive],
  }));

  assert.match(buttonFor(projectSidebar, `session:${SESSION_A}`), /data-task-activity="finalizing"/);
  assert.match(buttonFor(projectSidebar, `session:${SESSION_B}`), /data-task-activity="running"/);
  assert.match(buttonFor(projectSidebar, "session:session-c"), /data-task-activity="attention"/);
  assert.doesNotMatch(buttonFor(projectSidebar, "session:session-d"), /task-activity-indicator/);

  const quickSidebar = renderSidebar(projection({
    selected_project_index: -1,
    selected_session_index: 0,
    agent_tree_active: true,
    task_activity_state: "running",
    session_rows: [selected],
    chat_session_rows: [selected, attention, inactive],
  }));
  const selectedQuickChat = buttonFor(quickSidebar, `chat-session:${SESSION_A}`);
  assert.match(selectedQuickChat, /data-task-activity="running"/);
  assert.match(selectedQuickChat, /<small>実行中 · active turn<\/small>/);
  assert.match(buttonFor(quickSidebar, "chat-session:session-c"), /data-task-activity="attention"/);
  assert.doesNotMatch(buttonFor(quickSidebar, "chat-session:session-d"), /task-activity-indicator/);
});

test("access-mode control consumes the Rust mutation capability", () => {
  const enabled = projection();
  const disabled = projection({
    config_draft: {
      ...projection().config_draft,
      access_mode_mutation_enabled: false,
    },
  });
  assert.match(
    renderTopbar(enabled),
    /data-action="toggle-access"[^>]*aria-disabled="false"(?![^>]*\sdisabled(?:\s|>|=))[^>]*>/,
  );
  assert.match(
    renderTopbar(disabled),
    /data-action="toggle-access"[^>]*aria-disabled="true"[^>]*\sdisabled>/,
  );
});

test("topbar omits page replacement controls even when bounded history has more rows", () => {
  const html = renderTopbar(projection({
    turn_page_offset: 80,
    turn_page_limit: 80,
    turn_page_total: 240,
    turn_page_has_more: true,
  }));
  assert.doesNotMatch(html, /turn-page-chip/);
  assert.doesNotMatch(html, /data-action="load-previous-turn-page"/);
  assert.doesNotMatch(html, /data-action="load-next-turn-page"/);
});

test("drive-root projects use the selected Rust display label without changing path authority", () => {
  const state = projection({
    workspace_path: "R:/",
    selected_project_index: 0,
    selected_session_index: -1,
    project_rows: [{
      project_id: "project-r",
      label: "MappedProjectFolder",
      path: "R:/",
    }],
    session_rows: [],
    transcript_rows: [],
  });

  const sidebar = renderSidebar(state);
  assert.match(sidebar, /<span class="nav-title">MappedProjectFolder<\/span>/);
  assert.match(sidebar, /<small>R:\/<\/small>/);

  const topbar = renderTopbar(state);
  assert.match(
    topbar,
    /data-action="open-workspace-folder" title="R:\/">MappedProjectFolder<\/button>/,
  );

  const thread = renderThreadContent(state);
  assert.match(thread, /<div class="empty-status">[\s\S]*?<span>MappedProjectFolder<\/span>/);
  assert.doesNotMatch(thread, /<div class="empty-status">[\s\S]*?<span>R:<\/span>/);
});

test("config commit capability never gates unrelated workspace or window actions", () => {
  const clean = projection();
  for (const id of ["browse-workspace", "toggle-maximize-window"]) {
    assert.equal(actionById(id)?.enabled?.(clean, { index: -1, value: "" }), true, id);
  }
  assert.equal(
    actionById("apply-session-config")?.enabled?.(clean, { index: -1, value: "" }),
    false,
  );
  assert.equal(
    actionById("save-global-config")?.enabled?.(clean, { index: -1, value: "" }),
    false,
  );
});

test("a closed dirty settings draft blocks every external config owner mutation", () => {
  const ui = createUiLocalState();
  const fields = [
    {
      key: "model.model",
      value: "model-a",
      env_override: null,
      value_type: "string",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "permissions.access_mode",
      value: "default",
      env_override: null,
      value_type: "enum",
      required: false,
      min_value: null,
      max_value: null,
      options: ["default", "auto_review", "full_access"],
    },
  ];
  const before = projection({ config_fields: fields });
  reconcileUiDrafts(ui, null, before, null);
  updateConfigDraftValue(
    ui,
    before.config_target,
    before.config_fields.map((field) => ({ key: field.key, text: field.value })),
    "model.model",
    "unsaved-model-draft",
  );

  const dirtyView = projectViewState(before, ui);

  assert.equal(dirtyView.config_target.configGeneration, before.config_target.configGeneration);
  assert.equal(ui.configDirty, true);
  assert.equal(dirtyView.config_draft.external_owner_mutation_open, false);
  assert.equal(dirtyView.config_draft.access_mode_mutation_enabled, false);
  assert.equal(dirtyView.provider_apply_enabled, false);
  assert.equal(
    dirtyView.config_fields.find((field) => field.key === "model.model")?.value,
    "unsaved-model-draft",
  );
  assert.equal(
    dirtyView.config_fields.find((field) => field.key === "permissions.access_mode")?.value,
    "default",
    "the settings draft remains authoritative until its own Apply/Save",
  );

  for (const id of ["toggle-access", "apply-provider-session", "save-provider-global"]) {
    assert.equal(actionById(id)?.enabled?.(dirtyView, { index: -1, value: "" }), false, id);
    assert.equal(paletteActions(dirtyView).some((action) => action.id === id), false, id);
  }
  assert.equal(
    paletteActions(dirtyView).some((action) => action.id === "load-provider-models"),
    true,
    "catalog loading does not replace the config owner",
  );

  const setup = {
    ...dirtyView,
    overlay: "config",
    startup: {
      ...dirtyView.startup,
      initial_setup_required: true,
      action_overlay: "config",
    },
  };
  const dirtySettings = renderOverlay(setup, renderLocal());
  assert.match(dirtySettings, /data-action="import-config-toml" disabled>/);
  assert.match(dirtySettings, /data-action="discard-config-draft"(?![^>]*hidden)/);
  const after = projection({
    access_label: "full_access",
    access_target: { ...before.access_target, accessMode: "full_access" },
    config_target: { ...before.config_target },
    config_fields: fields.map((field) => field.key === "permissions.access_mode"
      ? { ...field, value: "full_access" }
      : field),
  });

  const cleanUi = createUiLocalState();
  reconcileUiDrafts(cleanUi, null, after, null);
  assert.equal(
    projectViewState(after, cleanUi).config_fields
      .find((field) => field.key === "permissions.access_mode")?.value,
    "full_access",
    "a clean settings view consumes the new Rust baseline",
  );
});

test("local config and run-start mutations close external config owner admission", async () => {
  const state = projection();
  const configPending = createUiLocalState();
  reconcileUiDrafts(configPending, null, state, null);
  updateConfigDraftValue(
    configPending,
    state.config_target,
    state.config_fields.map((field) => ({ key: field.key, text: field.value })),
    "model.model",
    "pending-model",
  );
  configPending.activeConfigMutationGeneration = 1n;
  const configPendingView = projectViewState(state, configPending);
  assert.equal(configPendingView.config_draft.access_mode_mutation_enabled, false);
  assert.equal(configPendingView.provider_apply_enabled, false);
  assert.equal(configPendingView.config_draft.external_owner_mutation_open, false);
  assert.equal(configPendingView.config_draft.discard_enabled, false);
  assert.equal(configPendingView.config_draft.commit_enabled, false);
  assert.equal(configDraftEditOpen(configPending), false);
  for (const id of ["discard-config-draft", "apply-session-config", "save-global-config"]) {
    assert.equal(actionById(id)?.enabled?.(
      configPendingView,
      { index: -1, value: "" },
    ), false, id);
  }
  let rerenders = 0;
  await actionById("discard-config-draft")?.run(
    configPendingView,
    { uiState: configPending, rerender: () => { rerenders += 1; } } as unknown as ActionContext,
    { index: -1, value: "" },
  );
  assert.equal(configPending.configDirty, true);
  assert.equal(rerenders, 0);

  let configCommands = 0;
  await actionById("apply-session-config")?.run(
    configPendingView,
    {
      uiState: configPending,
      mutate: async () => { configCommands += 1; },
    } as unknown as ActionContext,
    { index: -1, value: "" },
  );
  assert.equal(configCommands, 0, "direct repeated dispatch is rejected before a second request");

  const pendingHtml = renderOverlay(
    { ...configPendingView, overlay: "config" },
    renderLocal({ configMutationPending: true }),
  );
  assert.match(pendingHtml, /data-action="discard-config-draft"[^>]*disabled/);
  assert.match(pendingHtml, /data-action="apply-session-config" disabled/);
  assert.match(pendingHtml, /data-action="save-global-config" disabled/);
  assert.match(pendingHtml, /class="settings-control"[^>]*disabled/);
  configPending.activeConfigMutationGeneration = null;
  const rustOwnedDirty = projection();
  const resumed = projectViewState(rustOwnedDirty, configPending);
  assert.equal(configDraftEditOpen(configPending), true);
  assert.equal(resumed.config_draft.discard_enabled, true);
  assert.equal(resumed.config_draft.commit_enabled, true);

  const runPending = createUiLocalState();
  reconcileUiDrafts(runPending, null, rustOwnedDirty, null);
  updateConfigDraftValue(
    runPending,
    rustOwnedDirty.config_target,
    rustOwnedDirty.config_fields.map((field) => ({ key: field.key, text: field.value })),
    "model.model",
    "run-pending-model",
  );
  runPending.runStartMutationPending = true;
  const runPendingView = projectViewState(rustOwnedDirty, runPending);
  assert.equal(runPendingView.config_draft.access_mode_mutation_enabled, false);
  assert.equal(runPendingView.provider_apply_enabled, false);
  assert.equal(runPendingView.config_draft.external_owner_mutation_open, false);
  assert.equal(runPendingView.config_draft.commit_enabled, false);
});

test("a successful Discard click keeps an owner-fenced focus continuation through async settlement", async () => {
  const timeoutField = {
    key: "model.request_timeout_ms",
    value: "3600000",
    env_override: null,
    value_type: "integer",
    required: false,
    min_value: 1,
    max_value: 3600000,
    options: [],
  };
  const rustProjection = projection({
    overlay: "config",
    config_fields: [timeoutField],
  });
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, rustProjection, null);
  updateConfigDraftValue(
    ui,
    rustProjection.config_target,
    [{ key: timeoutField.key, text: timeoutField.value }],
    timeoutField.key,
    "3599000",
  );
  const dirtyView = projectViewState(rustProjection, ui);

  let activeElement: FakeFocusTarget | null = null;
  class FakeFocusTarget {
    readonly isConnected = true;
    hidden = false;
    disabled = false;
    inert = false;
    readonly id: string;
    constructor(id: string) { this.id = id; }
    getAttribute(): string | null { return null; }
    matches(selector: string): boolean { return selector === ":disabled" && this.disabled; }
    closest(): null { return null; }
    focus(): void { activeElement = this; }
  }
  class ManualFocusScheduler {
    private callback: (() => void) | null = null;

    schedule(callback: () => void): number {
      this.callback = callback;
      return 1;
    }

    cancel(): void {
      this.callback = null;
    }

    flush(): void {
      const callback = this.callback;
      this.callback = null;
      callback?.();
    }
  }
  const body = new FakeFocusTarget("body");
  const documentElement = new FakeFocusTarget("html");
  const discard = new FakeFocusTarget("discard");
  const close = new FakeFocusTarget("close");
  const dialog = new FakeFocusTarget("dialog");
  activeElement = discard;
  const focusScheduler = new ManualFocusScheduler();
  const focusArbiter = new PostRenderFocusArbiter(focusScheduler, {
    currentRenderCommit: () => 1,
    currentInteractionEpoch: () => 1n,
    interactionActive: () => false,
    activeElement: () => activeElement,
    bodyElement: () => body,
    documentElement: () => documentElement,
  });

  let resolveReset!: (value: DesktopViewState) => void;
  const resetResponse = new Promise<DesktopViewState>((resolve) => { resolveReset = resolve; });
  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    writable: true,
    value: {
      __TAURI_INTERNALS__: {
        invoke: (name: string, args: Record<string, unknown>) => {
          assert.equal(name, "reset_config_draft");
          assert.deepEqual(args.expectedTarget, rustProjection.config_target);
          return resetResponse;
        },
      },
    },
  });

  let rerenders = 0;
  let accepted = false;
  const context = {
    uiState: ui,
    getProjection: () => rustProjection,
    getViewState: () => projectViewState(rustProjection, ui),
    prepareConfigSnapshot: () => [{ key: timeoutField.key, text: "3599000" }],
    rerender: () => { rerenders += 1; },
    recoverCommandConflict: () => false,
    reportError: (error: unknown) => { throw error; },
    acceptProjection: (settled: DesktopViewState) => {
      accepted = true;
      discard.hidden = true;
      discard.disabled = true;
      const pendingFocus = ui.settingsActionFocusContinuation;
      assert.ok(pendingFocus);
      const cleanView = projectViewState(settled, ui);
      activeElement = body;
      ui.settingsActionFocusContinuation = null;
      const focusResults: unknown[] = [];
      focusArbiter.schedule({
        renderCommit: 1,
        interactionEpoch: 1n,
        intents: [{
          source: "settings-action",
          priority: "explicit-transfer",
          claim: { kind: "unowned" },
          candidates: settingsActionFocusCandidates(pendingFocus, (selector) => {
          if (selector === '[data-action="discard-config-draft"]') return discard as unknown as HTMLElement;
          if (selector === '[data-action="close-overlay"]') return close as unknown as HTMLElement;
          if (selector === ".settings-modal") return dialog as unknown as HTMLElement;
          return null;
          }),
          isCurrent: () => settingsActionFocusStillTargets(pendingFocus, cleanView),
        }],
        onResult: (result) => focusResults.push(result),
      });
      assert.equal(activeElement, body, "candidate generation does not write focus");
      focusScheduler.flush();
      assert.deepEqual(focusResults, [{ kind: "focused", source: "settings-action" }]);
    },
  } as unknown as ActionContext;

  try {
    const action = actionById("discard-config-draft");
    assert.ok(action);
    const pending = Promise.resolve(action.run(dirtyView, context, { index: -1, value: "" }));

    assert.equal(activeElement, discard, "the pointer-clicked Discard remains the interaction origin");
    assert.equal(rerenders, 1, "the pending projection disables Settings immediately");
    assert.notEqual(ui.activeConfigMutationGeneration, null);

    resolveReset({ ...rustProjection, projection_revision: "2" });
    await pending;

    assert.equal(accepted, true);
    assert.equal(ui.configDirty, false);
    assert.equal(activeElement, close, "clean Settings continues at the adjacent safe Close action");
    assert.equal(ui.settingsActionFocusContinuation, null);
  } finally {
    if (previousWindow) Object.defineProperty(globalThis, "window", previousWindow);
    else delete (globalThis as Record<string, unknown>).window;
  }
});

test("hidden settings draft can be reopened, discarded, and release external mutations", async () => {
  const raw = projection({ overlay: "none" });
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, raw, null);
  updateConfigDraftValue(
    ui,
    raw.config_target,
    raw.config_fields.map((field) => ({ key: field.key, text: field.value })),
    "model.model",
    "invalid-hidden-draft",
  );
  assert.equal(projectViewState(raw, ui).config_draft.external_owner_mutation_open, false);

  const reopened = {
    ...raw,
    overlay: "config",
  };
  reconcileUiDrafts(ui, raw, reopened, null);
  const reopenedView = projectViewState(reopened, ui);
  assert.equal(reopenedView.config_draft.dirty, true);
  assert.equal(actionById("discard-config-draft")?.enabled?.(
    reopenedView,
    { index: -1, value: "" },
  ), true);
  discardConfigDraft(ui);

  const rustOwnedClean = {
    ...reopened,
  };
  const cleanView = projectViewState(rustOwnedClean, ui);
  assert.equal(ui.configDirty, false);
  assert.equal(cleanView.config_draft.external_owner_mutation_open, true);
  assert.equal(cleanView.config_draft.access_mode_mutation_enabled, true);
  assert.equal(cleanView.provider_apply_enabled, true);
});

test("external config mutation roundtrip prevents a settings draft from starting", async () => {
  const state = projection({ overlay: "config" });
  state.startup.initial_setup_required = true;
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, state, null);
  ui.externalConfigMutationPending = true;

  const pending = projectViewState(state, ui);
  assert.equal(configDraftEditOpen(ui), false);
  assert.equal(pending.config_draft.external_owner_mutation_open, false);
  assert.equal(pending.config_draft.access_mode_mutation_enabled, false);
  assert.equal(pending.provider_apply_enabled, false);
  assert.equal(pending.config_draft.commit_enabled, false);
  assert.equal(ui.configDirty, false);
  let configCommands = 0;
  await actionById("apply-session-config")?.run(
    pending,
    {
      uiState: ui,
      getViewState: () => pending,
      mutate: async () => { configCommands += 1; },
    } as unknown as ActionContext,
    { index: -1, value: "" },
  );
  assert.equal(configCommands, 0);

  assert.match(
    renderOverlay(pending, renderLocal()),
    /class="settings-control"[^>]*disabled/,
  );
});

test("provider overlay consumes typed status and exposes control selection semantics", () => {
  const html = renderOverlay(projection({ overlay: "provider" }));
  assert.match(html, /<label class="field-label" for="provider-url">/);
  assert.match(html, /id="provider-url"[^>]*aria-describedby="provider-url-help provider-status"[^>]*aria-invalid="false"/);
  assert.match(html, /id="provider-url-help"/);
  assert.match(html, /id="provider-status"[^>]*role="status"[^>]*aria-live="polite"/);
  assert.match(html, /id="provider-profile"[^>]*aria-describedby="provider-profile-help"/);
  assert.match(html, /<option value="openai_compatible" selected>OpenAI-compatible \(Chat Completions\)<\/option>/);
  assert.match(html, /data-focus-key="provider-model:model-a" aria-pressed="true"/);
  assert.match(html, /Typed idle/);
  assert.doesNotMatch(html, /処理に失敗しました/);

  const invalid = renderOverlay(projection({
    overlay: "provider",
    provider_base_url: "",
    provider_context_window: "0",
  }));
  assert.match(invalid, /id="provider-url"[^>]*aria-invalid="true"/);
  assert.match(invalid, /provider-status error/);
  assert.match(invalid, /ベースURLを確認してください/);
  assert.match(invalid, /URL を入力してください。/);
  assert.match(invalid, /data-action="load-provider-models" disabled>モデル読込<\/button>/);
});

test("provider overlay renders actionable URL feedback without replacing valid typed status", () => {
  const typedStatus = projection().provider_status;
  const cases = [
    ["", "URL を入力してください。"],
    ["file:///tmp/provider.sock", "http:// または https://"],
    ["https://user:secret@provider.example/v1", "認証情報を含めず"],
    ["https://provider.example/v1?api_key=hidden", "query stringは指定できません"],
    ["https://provider.example/v1#hidden", "fragmentは指定できません"],
  ] as const;
  for (const [baseUrl, reason] of cases) {
    const feedback = providerOverlayFeedback(baseUrl, typedStatus);
    assert.equal(feedback.baseUrl.ok, false, baseUrl);
    assert.equal(feedback.status.kind, "error", baseUrl);
    assert.match(feedback.status.hint, new RegExp(reason), baseUrl);

    const html = renderOverlay(projection({ overlay: "provider", provider_base_url: baseUrl }));
    assert.match(html, /id="provider-url"[^>]*aria-describedby="provider-url-help provider-status"[^>]*aria-invalid="true"/, baseUrl);
    assert.match(html, /id="provider-status" class="provider-status error"/, baseUrl);
    assert.match(html, new RegExp(reason), baseUrl);
  }

  for (const baseUrl of [
    "http://localhost:1234/v1/",
    "https://provider.example/proxy/openai/v1",
  ]) {
    const feedback = providerOverlayFeedback(baseUrl, typedStatus);
    assert.equal(feedback.baseUrl.ok, true, baseUrl);
    assert.equal(feedback.status, typedStatus, baseUrl);
    const html = renderOverlay(projection({ overlay: "provider", provider_base_url: baseUrl }));
    assert.match(html, /id="provider-url"[^>]*aria-invalid="false"/, baseUrl);
    assert.match(html, /Typed idle/, baseUrl);
    assert.doesNotMatch(html, /ベースURLを確認してください/, baseUrl);
  }
});

test("provider URL typing updates accessible feedback and restores the current typed status", () => {
  const attributes = new Map<string, string>();
  const input = {
    setAttribute: (name: string, value: string) => attributes.set(name, value),
  };
  const title = { textContent: "" };
  const hint = { textContent: "" };
  const details = { hidden: false };
  const detailsText = { textContent: "" };
  const status = {
    className: "",
    querySelector: (selector: string) => {
      if (selector === "[data-provider-status-title]") return title;
      if (selector === "[data-provider-status-hint]") return hint;
      if (selector === "[data-details-key='provider-status-details']") return details;
      if (selector === "[data-provider-status-details]") return detailsText;
      return null;
    },
  };
  const fakeDocument = {
    querySelector: (selector: string) => {
      if (selector === "#provider-url") return input;
      if (selector === "#provider-status") return status;
      return null;
    },
  };
  const previousDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
  Object.defineProperty(globalThis, "document", {
    configurable: true,
    writable: true,
    value: fakeDocument,
  });

  try {
    synchronizeProviderOverlayFeedback(projection({
      provider_base_url: "https://user:secret@provider.example/v1",
      provider_status: { kind: "loading", title: "読込中", hint: "接続中", details: "pending" },
    }));
    assert.equal(attributes.get("aria-invalid"), "true");
    assert.equal(status.className, "provider-status error");
    assert.equal(title.textContent, "ベースURLを確認してください");
    assert.match(hint.textContent, /認証情報を含めず/);
    assert.equal(details.hidden, true);
    assert.equal(detailsText.textContent, "");

    synchronizeProviderOverlayFeedback(projection({
      provider_base_url: "https://provider.example/proxy/openai/v1",
      provider_status: { kind: "error", title: "接続できません", hint: "Providerを確認", details: "timeout" },
    }));
    assert.equal(attributes.get("aria-invalid"), "false");
    assert.equal(status.className, "provider-status error");
    assert.equal(title.textContent, "接続できません");
    assert.equal(hint.textContent, "Providerを確認");
    assert.equal(details.hidden, false);
    assert.equal(detailsText.textContent, "timeout");
  } finally {
    if (previousDocument) Object.defineProperty(globalThis, "document", previousDocument);
    else delete (globalThis as Record<string, unknown>).document;
  }
});

test("provider manual target and local context edits can be committed without loading a catalog", () => {
  const currentWithoutCatalog = projection({
    provider_apply_enabled: true,
    provider_catalog_base_url: null,
    provider_catalog_profile: null,
  });
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, currentWithoutCatalog, null);
  assert.equal(
    projectViewState(currentWithoutCatalog, ui).provider_apply_enabled,
    true,
    "a complete hand-entered connection remains independently applicable",
  );

  ui.drafts.provider.contextWindow = "65536";
  assert.equal(projectViewState(currentWithoutCatalog, ui).provider_apply_enabled, true);
  ui.drafts.provider.contextWindow = currentWithoutCatalog.provider_context_window;
  assert.equal(
    projectViewState(currentWithoutCatalog, ui).provider_apply_enabled,
    true,
    "catalog diagnostics do not own a complete connection",
  );

  ui.drafts.provider.baseUrl = "http://127.0.0.1:4321";
  assert.equal(projectViewState(currentWithoutCatalog, ui).provider_apply_enabled, true);
  ui.drafts.provider.baseUrl = currentWithoutCatalog.provider_base_url;
  ui.drafts.provider.providerProfile = "lm_studio";
  assert.equal(projectViewState(currentWithoutCatalog, ui).provider_apply_enabled, true);
  ui.drafts.provider.apiKeyEnv = "OPENAI_API_KEY";
  assert.equal(projectViewState(currentWithoutCatalog, ui).provider_apply_enabled, true);
  ui.drafts.provider.apiKeyEnv = currentWithoutCatalog.provider_api_key_env;
  ui.drafts.provider.providerProfile = currentWithoutCatalog.provider_profile;
  ui.drafts.provider.selectedModelId = "model-b";
  assert.equal(projectViewState(currentWithoutCatalog, ui).provider_apply_enabled, false);

  const failedSave = projection({
    provider_context_window: "65536",
    provider_apply_enabled: true,
    provider_catalog_base_url: null,
    provider_catalog_profile: null,
  });
  const failedSaveUi = createUiLocalState();
  reconcileUiDrafts(failedSaveUi, null, failedSave, null);
  assert.equal(
    projectViewState(failedSave, failedSaveUi).provider_apply_enabled,
    true,
    "a failed command keeps the local context budget dirty against the effective provider baseline",
  );

  const failedCatalogSwitch = projection({
    provider_base_url: "http://127.0.0.1:4321",
    provider_context_window: "65536",
    provider_apply_enabled: false,
    provider_catalog_base_url: null,
    provider_catalog_profile: null,
  });
  const failedCatalogSwitchUi = createUiLocalState();
  reconcileUiDrafts(failedCatalogSwitchUi, null, failedCatalogSwitch, null);
  failedCatalogSwitchUi.drafts.provider.baseUrl =
    failedCatalogSwitch.provider_effective_base_url;
  assert.equal(
    projectViewState(failedCatalogSwitch, failedCatalogSwitchUi).provider_apply_enabled,
    true,
    "returning from a failed catalog target can commit the local context budget against the effective provider",
  );

  const rustRejected = projection({
    provider_apply_enabled: false,
    provider_catalog_base_url: null,
    provider_catalog_profile: null,
  });
  const rustRejectedUi = createUiLocalState();
  reconcileUiDrafts(rustRejectedUi, null, rustRejected, null);
  rustRejectedUi.drafts.provider.contextWindow = "65536";
  rustRejectedUi.drafts.provider.baseUrl = "http://127.0.0.1:4321";
  assert.equal(
    projectViewState(rustRejected, rustRejectedUi).provider_apply_enabled,
    false,
    "frontend dirty state cannot replace catalog evidence for a new provider target",
  );
});

test("provider Apply and Save submit a complete hand-entered connection without catalog evidence", async () => {
  const projected = projection({
    overlay: "provider",
    provider_apply_enabled: true,
    provider_catalog_base_url: null,
    provider_catalog_profile: null,
    provider_catalog_api_key_env: null,
  });
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, projected, null);
  ui.drafts.provider.baseUrl = "http://provider.example:8119/v1";
  ui.drafts.provider.providerProfile = "openai_compatible";
  ui.drafts.provider.apiKeyEnv = "OPENAI_API_KEY";
  ui.drafts.provider.contextWindow = "65536";
  ui.drafts.provider.selectedModelId = "model-a";
  const view = projectViewState(projected, ui);
  assert.equal(view.provider_apply_enabled, true);
  for (const id of ["apply-provider-session", "save-provider-global"]) {
    assert.equal(actionById(id)?.enabled?.(view, { index: -1, value: "" }), true, id);
  }

  const invocations: Array<{ name: string; args?: Record<string, unknown> }> = [];
  const context = {
    uiState: ui,
    mutate: async (name: string, args?: Record<string, unknown>) => {
      invocations.push({ name, args });
    },
    prepareConfigSnapshot: () => projected.config_fields.map((field) => ({
      key: field.key,
      text: field.value,
    })),
  } as unknown as ActionContext;

  await actionById("apply-provider-session")?.run(view, context, { index: -1, value: "" });
  await actionById("save-provider-global")?.run(view, context, { index: -1, value: "" });

  const expectedArgs = {
    input: {
      baseUrl: "http://provider.example:8119/v1",
      providerProfile: "openai_compatible",
      apiKeyEnv: "OPENAI_API_KEY",
      contextWindow: "65536",
      selectedModelId: "model-a",
    },
    expectedTarget: projected.config_target,
    draftValues: [{ key: "model.model", text: "model-a" }],
  };
  assert.deepEqual(invocations, [
    { name: "apply_provider_session", args: expectedArgs },
    { name: "save_provider_global", args: expectedArgs },
  ]);
});

test("provider catalog evidence remains bound to the local URL and connection profile", () => {
  const loaded = projection({
    provider_apply_enabled: true,
    provider_catalog_base_url: "http://127.0.0.1:1234",
    provider_catalog_profile: "openai_compatible",
  });
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, loaded, null);
  assert.equal(projectViewState(loaded, ui).provider_apply_enabled, true);

  ui.drafts.provider.baseUrl = "http://127.0.0.1:1234/v1/";
  ui.drafts.provider.contextWindow = "65536";
  assert.equal(
    projectViewState(loaded, ui).provider_apply_enabled,
    true,
    "normalized /v1 URLs and local limit changes keep ownership of the loaded catalog",
  );
  ui.drafts.provider.baseUrl = "http://127.0.0.1:4321";
  assert.equal(projectViewState(loaded, ui).provider_apply_enabled, true);
  ui.drafts.provider.baseUrl = "http://127.0.0.1:1234";
  ui.drafts.provider.providerProfile = "lm_studio";
  assert.equal(projectViewState(loaded, ui).provider_apply_enabled, true);
});

test("provider catalog completion is rejected after a mid-flight URL edit", () => {
  const initial = projection({
    overlay: "provider",
    provider_base_url: "http://127.0.0.1:9763/slow",
    provider_catalog_base_url: null,
    provider_catalog_profile: null,
    provider_model_ids: ["model-a"],
    provider_models: ["Model A"],
    provider_selected_index: 0,
    provider_loading: false,
  });
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, initial, null);
  beginProviderCatalogRequest(ui, initial);
  const dispatched = captureDraftMutation(ui, "load_provider_models");
  const loading = projection({
    ...initial,
    projection_revision: "2",
    provider_loading: true,
    provider_status: { kind: "loading", title: "Loading", hint: "Waiting", details: "" },
  });
  acknowledgeDraftMutation(ui, loading, "load_provider_models", dispatched);
  reconcileUiDrafts(ui, initial, loading, dispatched);

  ui.drafts.provider.baseUrl = "http://192.168.10.101:1234";
  ui.drafts.providerRevision += 1;
  ui.drafts.providerCatalogIdentityRevision += 1;
  const completion = projection({
    ...initial,
    projection_revision: "3",
    provider_catalog_base_url: "http://127.0.0.1:9763/slow",
    provider_catalog_profile: "openai_compatible",
    provider_catalog_api_key_env: null,
    provider_model_ids: ["uat/fast-model", "model-a"],
    provider_models: ["uat/fast-model", "Model A"],
    provider_selected_index: 1,
    provider_selected_model_summary: ["Model: uat/fast-model"],
    provider_status: { kind: "success", title: "Loaded", hint: "Apply", details: "1 model" },
    provider_loading: false,
  });
  reconcileUiDrafts(ui, loading, completion, null);
  const view = projectViewState(completion, ui);

  assert.equal(view.provider_base_url, "http://192.168.10.101:1234");
  assert.deepEqual(view.provider_model_ids, []);
  assert.deepEqual(view.provider_models, []);
  assert.equal(view.provider_selected_index, -1);
  assert.equal(view.provider_catalog_base_url, null);
  assert.equal(view.provider_apply_enabled, true);
  assert.equal(view.provider_status.kind, "warning");
  assert.equal(ui.drafts.provider.baseUrl, "http://192.168.10.101:1234");
});

test("provider catalog completion is rejected after an ABA draft edit", () => {
  const initial = projection({
    overlay: "provider",
    provider_base_url: "http://127.0.0.1:9763/slow",
    provider_catalog_base_url: null,
    provider_catalog_profile: null,
    provider_model_ids: ["configured-model"],
    provider_models: ["Configured model"],
    provider_selected_index: 0,
    provider_loading: false,
    provider_apply_enabled: false,
  });
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, initial, null);
  const request = beginProviderCatalogRequest(ui, initial);
  assert.ok(request);
  assert.equal(request.providerRevision, ui.drafts.providerRevision);
  const dispatched = captureDraftMutation(ui, "load_provider_models");
  const loading = projection({
    ...initial,
    projection_revision: "2",
    provider_loading: true,
    provider_status: { kind: "loading", title: "Loading", hint: "Waiting", details: "" },
  });
  acknowledgeDraftMutation(ui, loading, "load_provider_models", dispatched);
  reconcileUiDrafts(ui, initial, loading, dispatched);

  ui.drafts.provider.baseUrl = "http://192.168.10.101:1234";
  ui.drafts.providerRevision += 1;
  ui.drafts.providerCatalogIdentityRevision += 1;
  ui.drafts.provider.baseUrl = "http://127.0.0.1:9763/slow";
  ui.drafts.providerRevision += 1;
  ui.drafts.providerCatalogIdentityRevision += 1;
  const loadingView = projectViewState(loading, ui);
  assert.equal(loadingView.provider_loading, true, "the Rust-owned request remains in flight");
  assert.equal(loadingView.provider_base_url, "http://127.0.0.1:9763/slow");
  assert.equal(loadingView.provider_status.kind, "warning");

  const completion = projection({
    ...initial,
    projection_revision: "3",
    provider_catalog_base_url: "http://127.0.0.1:9763/slow",
    provider_catalog_profile: "openai_compatible",
    provider_model_ids: ["configured-model", "stale-model"],
    provider_models: ["Configured model", "Stale model"],
    provider_selected_index: 0,
    provider_loading: false,
    provider_apply_enabled: true,
  });
  reconcileUiDrafts(ui, loading, completion, null);
  const view = projectViewState(completion, ui);

  assert.deepEqual(view.provider_model_ids, []);
  assert.equal(view.provider_selected_index, -1);
  assert.equal(view.provider_catalog_base_url, null);
  assert.equal(view.provider_status.kind, "warning");
  assert.equal(ui.rejectedProviderCatalogRequest, request);
  assert.equal(ui.drafts.provider.baseUrl, "http://127.0.0.1:9763/slow");
  assert.equal(ui.drafts.provider.selectedModelId, "configured-model");
  assert.equal(deriveUiCapabilities(completion, ui).canApplyProvider, true);
  assert.equal(view.provider_apply_enabled, true);
  const rejectedHtml = renderOverlay(view);
  assert.match(
    rejectedHtml,
    /data-action="apply-provider-session" >UIセッションに適用/,
  );
  assert.match(
    rejectedHtml,
    /data-action="save-provider-global" >設定ファイルに保存/,
  );

  const reloadRequest = beginProviderCatalogRequest(ui, completion);
  assert.ok(reloadRequest);
  const reloadDispatched = captureDraftMutation(ui, "load_provider_models");
  const reloadLoading = projection({
    ...completion,
    projection_revision: "4",
    provider_catalog_base_url: null,
    provider_catalog_profile: null,
    provider_loading: true,
    provider_apply_enabled: false,
    provider_status: { kind: "loading", title: "Loading", hint: "Waiting", details: "" },
  });
  acknowledgeDraftMutation(ui, reloadLoading, "load_provider_models", reloadDispatched);
  reconcileUiDrafts(ui, completion, reloadLoading, reloadDispatched);

  const reloadCompletion = projection({
    ...completion,
    projection_revision: "5",
    provider_model_ids: ["configured-model", "fresh-model"],
    provider_models: ["Configured model", "Fresh model"],
    provider_selected_index: 0,
    provider_loading: false,
    provider_apply_enabled: true,
    provider_status: { kind: "success", title: "Loaded", hint: "Apply", details: "2 models" },
  });
  reconcileUiDrafts(ui, reloadLoading, reloadCompletion, null);
  const reloadedView = projectViewState(reloadCompletion, ui);

  assert.equal(ui.rejectedProviderCatalogRequest, null);
  assert.deepEqual(
    reloadedView.provider_model_ids,
    ["configured-model", "fresh-model"],
    "a successful reload retains the configured/manual model fallback beside returned models",
  );
  assert.equal(ui.drafts.provider.baseUrl, "http://127.0.0.1:9763/slow");
  assert.equal(ui.drafts.provider.selectedModelId, "configured-model");
  assert.equal(deriveUiCapabilities(reloadCompletion, ui).canApplyProvider, true);
  assert.equal(reloadedView.provider_apply_enabled, true);
  const reloadedHtml = renderOverlay(reloadedView);
  assert.match(
    reloadedHtml,
    /data-action="apply-provider-session" >UIセッションに適用/,
  );
  assert.match(
    reloadedHtml,
    /data-action="save-provider-global" >設定ファイルに保存/,
  );
});

test("provider catalog rapid double dispatch preserves one owner through ABA settlement", async () => {
  const initial = projection({
    overlay: "provider",
    provider_base_url: "http://127.0.0.1:9763/slow",
    provider_catalog_base_url: null,
    provider_catalog_profile: null,
    provider_model_ids: [],
    provider_models: [],
    provider_selected_index: -1,
    provider_loading: false,
  });
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, initial, null);

  let current: DesktopViewState = initial;
  let currentView = projectViewState(current, ui);
  let commandCount = 0;
  let releaseCommand!: () => void;
  const commandDeferred = new Promise<void>((resolve) => { releaseCommand = resolve; });
  const context = {
    uiState: ui,
    getProjection: () => current,
    getViewState: () => currentView,
    rerender: () => { currentView = projectViewState(current, ui); },
    mutate: async (name: string) => {
      assert.equal(name, "load_provider_models");
      commandCount += 1;
      const dispatched = captureDraftMutation(ui, name);
      await commandDeferred;
      const loading = projection({
        ...initial,
        projection_revision: "2",
        provider_loading: true,
        provider_status: { kind: "loading", title: "Loading", hint: "Waiting", details: "" },
      });
      acknowledgeDraftMutation(ui, loading, name, dispatched);
      reconcileUiDrafts(ui, current, loading, dispatched);
      current = loading;
      currentView = projectViewState(current, ui);
    },
  } as unknown as ActionContext;
  const staleEnabledView = currentView;

  const firstDispatch = dispatchGuiAction(
    "load-provider-models",
    -1,
    "",
    staleEnabledView,
    context,
  );
  const request = ui.providerCatalogTransaction.active;
  assert.ok(request);
  const secondDispatch = dispatchGuiAction(
    "load-provider-models",
    -1,
    "",
    staleEnabledView,
    context,
  );
  await secondDispatch;

  assert.equal(commandCount, 1, "a stale queued activation cannot submit a second command");
  assert.equal(ui.providerCatalogTransaction.active, request, "the first request token remains the owner");
  assert.equal(ui.providerCatalogTransaction.nextId, request.token + 1);
  assert.equal(currentView.provider_loading, true, "local pending projects before the Rust response");
  assert.equal(deriveUiCapabilities(current, ui).canLoadProviderModels, false);
  assert.match(
    renderOverlay(currentView),
    /data-action="load-provider-models" disabled>読込中<\/button>/,
  );

  releaseCommand();
  await firstDispatch;
  assert.equal(ui.providerCatalogTransaction.active, request);
  assert.equal(request.admitted, true);

  ui.drafts.provider.baseUrl = "http://192.168.10.101:1234";
  ui.drafts.providerRevision += 1;
  ui.drafts.providerCatalogIdentityRevision += 1;
  ui.drafts.provider.baseUrl = "http://127.0.0.1:9763/slow";
  ui.drafts.providerRevision += 1;
  ui.drafts.providerCatalogIdentityRevision += 1;
  const completion = projection({
    ...initial,
    projection_revision: "3",
    provider_catalog_base_url: "http://127.0.0.1:9763/slow",
    provider_catalog_profile: "openai_compatible",
    provider_model_ids: ["stale-model"],
    provider_models: ["Stale model"],
    provider_selected_index: 0,
    provider_loading: false,
  });
  reconcileUiDrafts(ui, current, completion, null);
  current = completion;
  currentView = projectViewState(current, ui);

  assert.equal(ui.providerCatalogTransaction.active, null);
  assert.equal(ui.rejectedProviderCatalogRequest, request);
  assert.equal(currentView.provider_loading, false);
  assert.deepEqual(currentView.provider_model_ids, []);
  assert.equal(currentView.provider_catalog_base_url, null);
  assert.equal(currentView.provider_status.kind, "warning");
});

test("provider catalog completion is accepted for the exact dispatched draft target", () => {
  const initial = projection({
    overlay: "provider",
    provider_base_url: "http://127.0.0.1:9763/slow",
    provider_catalog_base_url: null,
    provider_catalog_profile: null,
    provider_model_ids: ["model-a"],
    provider_models: ["Model A"],
    provider_selected_index: 0,
    provider_loading: false,
  });
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, initial, null);
  const request = beginProviderCatalogRequest(ui, initial);
  assert.ok(request);
  assert.equal(request.providerRevision, ui.drafts.providerRevision);
  const dispatched = captureDraftMutation(ui, "load_provider_models");
  const loading = projection({
    ...initial,
    projection_revision: "2",
    provider_loading: true,
  });
  acknowledgeDraftMutation(ui, loading, "load_provider_models", dispatched);
  reconcileUiDrafts(ui, initial, loading, dispatched);

  const completion = projection({
    ...initial,
    projection_revision: "3",
    provider_catalog_base_url: "http://127.0.0.1:9763/slow",
    provider_catalog_profile: "openai_compatible",
    provider_model_ids: ["uat/fast-model", "model-a"],
    provider_models: ["uat/fast-model", "Model A"],
    provider_selected_index: 1,
    provider_loading: false,
  });
  reconcileUiDrafts(ui, loading, completion, null);
  const view = projectViewState(completion, ui);

  assert.deepEqual(view.provider_model_ids, ["uat/fast-model", "model-a"]);
  assert.equal(view.provider_selected_index, 1);
  assert.equal(view.provider_catalog_base_url, "http://127.0.0.1:9763/slow");
  assert.equal(ui.rejectedProviderCatalogRequest, null);
});

test("provider catalog completion is rejected after a mid-flight profile edit", () => {
  const initial = projection({
    overlay: "provider",
    provider_base_url: "http://127.0.0.1:9763/slow",
    provider_profile: "openai_compatible",
    provider_catalog_base_url: null,
    provider_catalog_profile: null,
    provider_model_ids: [],
    provider_models: [],
    provider_selected_index: -1,
    provider_loading: false,
  });
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, initial, null);
  beginProviderCatalogRequest(ui, initial);
  const dispatched = captureDraftMutation(ui, "load_provider_models");
  const loading = projection({
    ...initial,
    projection_revision: "2",
    provider_loading: true,
    provider_status: { kind: "loading", title: "Loading", hint: "Waiting", details: "" },
  });
  acknowledgeDraftMutation(ui, loading, "load_provider_models", dispatched);
  reconcileUiDrafts(ui, initial, loading, dispatched);

  ui.drafts.provider.providerProfile = "lm_studio";
  ui.drafts.providerRevision += 1;
  ui.drafts.providerCatalogIdentityRevision += 1;
  const completion = projection({
    ...initial,
    projection_revision: "3",
    provider_catalog_base_url: "http://127.0.0.1:9763/slow",
    provider_catalog_profile: "openai_compatible",
    provider_model_ids: ["uat/fast-model"],
    provider_models: ["uat/fast-model"],
    provider_selected_index: 0,
    provider_selected_model_summary: ["Model: uat/fast-model"],
    provider_status: { kind: "success", title: "Loaded", hint: "Apply", details: "1 model" },
    provider_loading: false,
  });
  reconcileUiDrafts(ui, loading, completion, null);
  const view = projectViewState(completion, ui);

  assert.equal(view.provider_profile, "lm_studio");
  assert.deepEqual(view.provider_model_ids, []);
  assert.deepEqual(view.provider_models, []);
  assert.equal(view.provider_selected_index, -1);
  assert.equal(view.provider_catalog_profile, null);
  assert.equal(view.provider_apply_enabled, false);
  assert.equal(view.provider_status.kind, "warning");
  assert.equal(ui.drafts.provider.providerProfile, "lm_studio");
});

test("provider catalog completion is rejected after its config target changes", () => {
  const initial = projection({
    overlay: "provider",
    provider_base_url: "http://127.0.0.1:9763/slow",
    provider_catalog_base_url: null,
    provider_catalog_profile: null,
    provider_model_ids: [],
    provider_models: [],
    provider_selected_index: -1,
    provider_loading: false,
  });
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, initial, null);
  beginProviderCatalogRequest(ui, initial);
  const dispatched = captureDraftMutation(ui, "load_provider_models");
  const loading = projection({
    ...initial,
    projection_revision: "2",
    provider_loading: true,
  });
  acknowledgeDraftMutation(ui, loading, "load_provider_models", dispatched);
  reconcileUiDrafts(ui, initial, loading, dispatched);

  const completion = projection({
    ...initial,
    projection_revision: "3",
    config_target: {
      workspacePath: "C:/workspace",
      sessionId: SESSION_A,
      configGeneration: "2",
    },
    provider_catalog_base_url: "http://127.0.0.1:9763/slow",
    provider_catalog_profile: "openai_compatible",
    provider_model_ids: ["uat/fast-model"],
    provider_models: ["uat/fast-model"],
    provider_selected_index: 0,
    provider_loading: false,
  });
  reconcileUiDrafts(ui, loading, completion, null);
  const view = projectViewState(completion, ui);

  assert.deepEqual(view.provider_model_ids, []);
  assert.equal(view.provider_selected_index, -1);
  assert.equal(view.provider_catalog_base_url, null);
  assert.equal(view.provider_status.kind, "warning");
});

test("settings exposes separate config and user data folder actions", () => {
  const state = projection({ overlay: "config" });
  const html = renderOverlay(state);

  assert.match(
    html,
    /data-action="open-global-config-folder">設定フォルダーを開く<\/button>/,
  );
  assert.match(
    html,
    /data-action="open-user-data-folder">データフォルダーを開く<\/button>/,
  );
  assert.equal(actionById("open-user-data-folder")?.label, "データフォルダーを開く");
  assert.equal(
    actionById("open-user-data-folder")?.enabled?.(state, { index: -1, value: "" }),
    true,
  );
});

test("initial setup renders typed config import failures inside the active modal", () => {
  for (const overlay of ["config", "provider"] as const) {
    const state = projection({
      overlay,
      status_code: "config_import_failed",
      status_message: "設定ファイルをImportできませんでした。",
      status_detail: "invalid <model.request_timeout_ms>",
    });
    state.startup = {
      ...state.startup,
      status: "requires_config",
      action_overlay: overlay,
      initial_setup_required: true,
    };

    const html = renderOverlay(state);
    assert.match(html, /role="alert" aria-live="assertive"/);
    assert.match(html, /設定ファイルをImportできませんでした。/);
    assert.match(html, /invalid &lt;model\.request_timeout_ms&gt;/);
    assert.match(html, /TOML設定をImport/);
    assert.doesNotMatch(html, /invalid <model\.request_timeout_ms>/);
  }

  const preferences = projection({
    overlay: "config",
    status_code: "config_import_failed",
    status_message: "stale import failure",
  });
  assert.doesNotMatch(renderOverlay(preferences), /role="alert"/);
});

test("initial setup makes durable save primary and explains its blocking alternatives", () => {
  const local = renderLocal();
  for (const overlay of ["config", "provider"] as const) {
    const state = projection({ overlay });
    state.config_draft = { ...state.config_draft, commit_enabled: true };
    state.startup = {
      ...state.startup,
      status: "requires_config",
      action_overlay: overlay,
      initial_setup_required: true,
    };

    const html = renderOverlay(state, local);
    const saveAction = overlay === "config" ? "save-global-config" : "save-provider-global";
    const applyAction = overlay === "config" ? "apply-session-config" : "apply-provider-session";
    assert.match(
      html,
      new RegExp(`class="setup-primary-action" data-action="${saveAction}"(?![^>]*\\sdisabled(?:\\s|>))[^>]*>設定を保存して開始</button>`),
    );
    assert.match(
      html,
      new RegExp(`class="setup-secondary-action" data-action="${applyAction}"[^>]*>この起動中だけ適用</button>`),
    );
    assert.match(html, /role="group" aria-label="初期設定の完了方法" aria-describedby="initial-setup-action-help"/);
    assert.match(html, /TOMLのファイル選択をキャンセルしても設定は変わりません。/);
    assert.match(html, /保存または一時適用が完了するまで閉じません。/);
    assert.doesNotMatch(html, /data-action="close-overlay"/);
  }

  const preferences = renderOverlay(projection({ overlay: "config" }), local);
  assert.match(preferences, /data-action="apply-session-config"[^>]*>UIセッションに適用</);
  assert.match(preferences, /data-action="save-global-config"[^>]*>設定ファイルに保存</);
  assert.doesNotMatch(preferences, /setup-primary-action/);

  const provider = renderOverlay(projection({ overlay: "provider" }), local);
  assert.match(provider, /data-action="apply-provider-session"[^>]*>UIセッションに適用</);
  assert.match(provider, /data-action="save-provider-global"[^>]*>設定ファイルに保存</);
  assert.doesNotMatch(provider, /setup-primary-action/);

  const blocked = projection({ overlay: "config" });
  blocked.startup = {
    ...blocked.startup,
    status: "requires_config",
    action_overlay: "config",
    initial_setup_required: true,
  };
  assert.match(
    renderOverlay(blocked, local),
    /class="setup-primary-action" data-action="save-global-config" disabled aria-disabled="true">設定を保存して開始</,
  );
});

test("initial setup replaces an old import failure with current mutation progress", () => {
  const state = projection({
    overlay: "config",
    status_code: "config_import_failed",
    status_message: "stale import failure",
  });
  state.startup = {
    ...state.startup,
    status: "requires_config",
    action_overlay: "config",
    initial_setup_required: true,
  };
  const html = renderOverlay(state, renderLocal({ configMutationPending: true }));
  assert.match(html, /role="status" aria-live="polite">設定を確認しています…/);
  assert.doesNotMatch(html, /stale import failure/);

});

test("config import begins its mutation owner before the pending rerender", () => {
  const ui = createUiLocalState();
  const state = projection({ overlay: "config" });
  const observations: Array<{ external: boolean; generation: bigint | null }> = [];

  const request = beginConfigImportMutation(ui, state.config_target, () => {
    observations.push({
      external: ui.externalConfigMutationPending,
      generation: ui.activeConfigMutationGeneration,
    });
  });

  assert.deepEqual(observations, [{ external: true, generation: request.generation }]);
});

test("cancelled config file selection settles as a no-op and keeps initial setup open", async () => {
  const state = projection({ overlay: "config" });
  state.startup = {
    ...state.startup,
    status: "requires_config",
    action_overlay: "config",
    initial_setup_required: true,
  };
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, state, null);
  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    writable: true,
    value: {
      __TAURI_INTERNALS__: {
        invoke: (name: string) => {
          assert.equal(name, "import_global_config_toml");
          return Promise.resolve([state, false]);
        },
      },
    },
  });
  let accepted: DesktopViewState | null = null;
  let errors = 0;
  const context = {
    uiState: ui,
    getProjection: () => state,
    getViewState: () => state,
    prepareConfigSnapshot: () => state.config_fields.map((field) => ({
      key: field.key,
      text: field.value,
    })),
    acceptProjection: (next: DesktopViewState) => { accepted = next; },
    rerender: () => {},
    recoverCommandConflict: () => false,
    reportError: () => { errors += 1; },
  } as unknown as ActionContext;

  try {
    assert.equal(await dispatchGuiAction("import-config-toml", -1, "", state, context), true);
    assert.equal(accepted?.overlay, "config");
    assert.equal(accepted?.startup.initial_setup_required, true);
    assert.equal(ui.externalConfigMutationPending, false);
    assert.equal(ui.activeConfigMutationGeneration, null);
    assert.equal(ui.configDirty, false);
    assert.equal(errors, 0);
  } finally {
    if (previousWindow) Object.defineProperty(globalThis, "window", previousWindow);
    else delete (globalThis as Record<string, unknown>).window;
  }
});

test("settings enum controls render only Rust-projected option values", () => {
  const html = renderOverlay(projection({
    overlay: "config",
    config_fields: [{
      key: "multi_agent.mode",
      value: "future_mode",
      env_override: null,
      value_type: "enum",
      required: false,
      min_value: null,
      max_value: null,
      options: ["explicit_request_only", "future_mode"],
    }],
  }));
  assert.match(html, /<option value="future_mode" selected>future_mode<\/option>/);
  assert.doesNotMatch(html, /<option value="proactive"/);
});

test("regular modals preserve native text editing shortcuts only for editable controls", () => {
  const sample = {
    key: "c",
    ctrlKey: true,
    metaKey: false,
    altKey: false,
    repeat: false,
  };
  for (const key of ["a", "c", "v", "x", "y", "z", "Backspace", "Delete", "ArrowLeft", "ArrowRight", "Home", "End"]) {
    assert.equal(modalShortcutShouldPreventDefault({ ...sample, key }, true), false, key);
    assert.equal(modalShortcutShouldPreventDefault({ ...sample, key }, false), true, key);
  }
  for (const key of ["k", "n", "i", "Enter"]) {
    assert.equal(modalShortcutShouldPreventDefault({ ...sample, key }, true), true, key);
  }
  assert.equal(modalShortcutShouldPreventDefault({ ...sample, key: "v", altKey: true }, true), true);
  assert.equal(modalShortcutShouldPreventDefault({ ...sample, key: "F8", ctrlKey: false }, true), true);
  assert.equal(modalShortcutShouldPreventDefault({ ...sample, key: "Tab", ctrlKey: false }, true), false);
});

test("Help exposes a Rust-owned accessible About dialog", () => {
  const state = projection({
    overlay: "about",
    about: {
      product_name: "moyAI Next",
      version: "9.8.7-test",
      codename: "Test Codename",
      license_identifier: "License-ID",
      copyright_notice: "Copyright notice from the bundled license",
    },
  });
  const html = renderOverlay(state);

  assert.equal(actionById("show-about")?.menu, "help");
  assert.match(html, /role="dialog"/);
  assert.match(html, /aria-labelledby="about-dialog-title"/);
  assert.match(html, /aria-describedby="about-dialog-description"/);
  assert.match(html, /moyAI Nextについて/);
  assert.match(html, /9\.8\.7-test/);
  assert.match(html, /License-ID/);
  assert.match(html, /Copyright notice from the bundled license/);
  assert.match(html, /class="about-logo"[^>]*alt=""[^>]*aria-hidden="true"/);
  assert.equal((html.match(/data-action="close-overlay"/g) ?? []).length, 3);
});

test("settings renders one typed LLM response inactivity timeout with host-neutral help", () => {
  const html = renderOverlay(projection({
    overlay: "config",
    config_fields: [{
      key: "model.request_timeout_ms",
      value: "3600000",
      env_override: "MOYAI_REQUEST_TIMEOUT_MS",
      value_type: "integer",
      required: false,
      min_value: 1,
      max_value: 3600000,
      options: [],
    }],
  }));

  assert.match(html, /応答の進捗を待つ時間（ms）/);
  assert.match(html, /応答開始までと、受信中に進捗が止まった場合の待ち時間です。応答が続いている間の総時間は制限せず、モデル側の設定も変えません。/);
  assert.match(html, /data-config-key="model\.request_timeout_ms"[^>]+value="3600000"/);
  assert.doesNotMatch(html, /model\.stream_idle_timeout_ms/);
  assert.doesNotMatch(html, /settings-raw-value[^>]+model\.request_timeout_ms/);
});

test("Docling dependencies expose one visible toggle owner and disable connection editors while off", () => {
  const doclingField = (
    key: string,
    value: string,
    valueType: ConfigFieldProjection["value_type"] = "string",
  ): ConfigFieldProjection => ({
    key,
    value,
    env_override: null,
    value_type: valueType,
    required: false,
    min_value: null,
    max_value: null,
    options: [],
  });
  const fields = (
    enabled: boolean,
    baseUrl = "http://127.0.0.1:5001",
  ): ConfigFieldProjection[] => [
    doclingField("docling.enabled", String(enabled), "boolean"),
    { ...doclingField("docling.base_url", baseUrl, "string"), required: true },
    doclingField("docling.timeout_ms", "120000", "integer"),
    doclingField("docling.api_key_env", "DOCLING_API_KEY", "string"),
    doclingField("docling.headers_json", "{}", "json"),
  ];

  const off = renderOverlay(projection({ overlay: "config", config_fields: fields(false) }));
  assert.match(off, /<label class="settings-toggle"[^>]*data-config-key="docling\.enabled"[^>]*>/);
  assert.match(off, />Docling を有効化/);
  assert.match(off, /id="docling-disabled-help"[^>]*(?<!hidden)>Doclingがオフのため/);
  assert.match(off, /data-docling-dependent aria-disabled="true"/);
  for (const key of ["docling.base_url", "docling.timeout_ms", "docling.api_key_env", "docling.headers_json"]) {
    assert.match(off, new RegExp(`data-config-key="${key.replace(".", "\\.")}"[^>]*aria-describedby="[^"]*docling-disabled-help[^"]*"[^>]*disabled`));
  }
  assert.match(off, /<summary>Doclingの接続ヘッダー（詳細）<\/summary>/);

  const on = renderOverlay(projection({ overlay: "config", config_fields: fields(true) }));
  assert.match(on, /id="docling-disabled-help"[^>]*hidden/);
  assert.match(on, /data-docling-dependent aria-disabled="false"/);
  assert.match(on, /data-config-key="docling\.base_url"(?![^>]*disabled)[^>]*>/);

  const invalidUrl = "https://docling.example.test/api?token=hidden";
  const offInvalid = renderOverlay(projection({ overlay: "config", config_fields: fields(false, invalidUrl) }));
  const onInvalid = renderOverlay(projection({ overlay: "config", config_fields: fields(true, invalidUrl) }));
  const baseUrlControl = (html: string) => html.match(
    /<input[^>]*data-config-key="docling\.base_url"[^>]*>/,
  )?.[0] ?? "";
  assert.doesNotMatch(baseUrlControl(offInvalid), /aria-invalid="true"/);
  assert.match(baseUrlControl(onInvalid), /aria-invalid="true"/);
});

test("Docling readiness stays an explicit clean-config action with typed section status", () => {
  const fields: ConfigFieldProjection[] = [
    {
      key: "docling.enabled",
      value: "true",
      env_override: null,
      value_type: "boolean",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "docling.base_url",
      value: "http://127.0.0.1:5001",
      env_override: null,
      value_type: "string",
      required: true,
      min_value: null,
      max_value: null,
      options: [],
    },
  ];
  const ready = projection({
    overlay: "config",
    config_fields: fields,
    docling_readiness: {
      status: "ready",
      endpoint: "http://127.0.0.1:5001/ready",
      httpStatus: 200,
      message: "Docling /ready returned HTTP 200.",
    },
  });
  const action = registryActionById("check-docling-readiness");
  assert.ok(action);
  assert.equal(action.enabled(actionTestModel(ready), { index: -1, value: "" }), true);
  const readyHtml = renderOverlay(ready, renderLocal());
  assert.match(readyHtml, /data-action="check-docling-readiness"[^>]*aria-controls="docling-readiness-status"/);
  assert.match(readyHtml, /data-settings-live-region="docling-readiness"[^>]*data-docling-readiness-status="ready"/);
  assert.match(readyHtml, /Docling を利用できます/);
  assert.match(readyHtml, /HTTP 200/);

  const localPending = renderLocal({ doclingReadinessRequestPending: true });
  assert.equal(
    action.enabled(createDesktopRenderModel(ready, localPending), { index: -1, value: "" }),
    false,
  );
  const localPendingHtml = renderOverlay(ready, localPending);
  assert.match(
    localPendingHtml,
    /data-action="check-docling-readiness"[^>]*aria-busy="true"[^>]*disabled/,
  );
  assert.match(localPendingHtml, /data-docling-readiness-status="checking"[^>]*aria-busy="true"/);
  assert.match(localPendingHtml, /接続確認を開始しています/);
  assert.doesNotMatch(localPendingHtml, /HTTP 200/);

  const checking = {
    ...ready,
    docling_readiness: { ...ready.docling_readiness, status: "checking" as const },
  };
  assert.equal(action.enabled(actionTestModel(checking), { index: -1, value: "" }), false);
  assert.match(renderOverlay(checking, renderLocal()), /data-docling-readiness-status="checking"[^>]*aria-busy="true"/);

  const dirty = {
    ...ready,
    config_draft: {
      ...ready.config_draft,
      dirty: true,
      discard_enabled: true,
      commit_enabled: true,
      external_owner_mutation_open: true,
    },
  };
  assert.equal(
    action.enabled(actionTestModel(dirty), { index: -1, value: "" }),
    false,
    "dirty draft alone closes the saved-effective-config readiness action",
  );
  const dirtyHtml = renderOverlay(dirty, renderLocal());
  assert.match(dirtyHtml, /未保存の設定があります/);
  assert.match(dirtyHtml, /保存してから Test Docling/);
});

test("dirty Settings close confirmation is modal, target-scoped, and has no backdrop action", () => {
  const html = renderLocalConfirmation({
    kind: "settings_close",
    expectedTarget: projection().config_target,
  });
  assert.match(html, /class="modal confirmation settings-close-confirmation" role="alertdialog"/);
  assert.match(html, /aria-labelledby="settings-close-confirm-title"/);
  assert.match(html, /aria-describedby="settings-close-confirm-summary"/);
  assert.match(html, /data-action="cancel-local-confirm"/);
  assert.match(html, /data-action="confirm-settings-discard-close"/);
  assert.doesNotMatch(html, /modal-backdrop"[^>]*data-action/);
});

test("invalid local Settings values close Apply and Save while valid dirty values reopen them", () => {
  const timeoutField: ConfigFieldProjection = {
    key: "model.request_timeout_ms",
    value: "0",
    env_override: "MOYAI_REQUEST_TIMEOUT_MS",
    value_type: "integer",
    required: false,
    min_value: 1,
    max_value: 3600000,
    options: [],
  };
  const renderSettings = (field: ConfigFieldProjection, commitOpen: boolean) => {
    const state = projection({ overlay: "config", config_fields: [field] });
    state.config_draft = {
      ...state.config_draft,
      dirty: true,
      edit_enabled: commitOpen,
      discard_enabled: commitOpen,
      commit_enabled: commitOpen,
      external_owner_mutation_open: commitOpen,
    };
    return renderOverlay(state, renderLocal({
      configMutationPending: !commitOpen,
    }));
  };

  const invalid = renderSettings(timeoutField, true);
  assert.match(invalid, /id="settings-validation" class="validation error"[^>]*>model\.request_timeout_ms: 1 以上の数値を入力してください。/);
  assert.match(invalid, /data-config-key="model\.request_timeout_ms"[^>]*aria-invalid="true"/);
  for (const action of ["apply-session-config", "save-global-config"]) {
    assert.match(invalid, new RegExp(`data-action="${action}"[^>]*disabled[^>]*aria-disabled="true"`));
  }

  const valid = renderSettings({ ...timeoutField, value: "3600000" }, true);
  assert.match(valid, /id="settings-validation" class="validation ok"[^>]*>未保存の設定があります。/);
  assert.doesNotMatch(valid, /data-config-key="model\.request_timeout_ms"[^>]*aria-invalid="true"/);
  for (const action of ["apply-session-config", "save-global-config"]) {
    assert.match(valid, new RegExp(`data-action="${action}"(?![^>]*\\sdisabled(?:\\s|>))[^>]*aria-disabled="false"`));
  }

  const pending = renderSettings({ ...timeoutField, value: "3600000" }, false);
  for (const action of ["apply-session-config", "save-global-config"]) {
    assert.match(pending, new RegExp(`data-action="${action}"[^>]*disabled[^>]*aria-disabled="true"`));
  }
});

test("every Settings field has unique connected help, validation, and explicit label ownership", () => {
  const configFields: ConfigFieldProjection[] = [
    {
      key: "model.base_url",
      value: "http://127.0.0.1:1234",
      env_override: "MOYAI_BASE_URL",
      value_type: "string",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "model.model",
      value: "model-a",
      env_override: "MOYAI_MODEL",
      value_type: "string",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "model.provider_profile",
      value: "openai_compatible",
      env_override: "MOYAI_PROVIDER_PROFILE",
      value_type: "enum",
      required: false,
      min_value: null,
      max_value: null,
      options: ["lm_studio", "openai_compatible", "openai_responses", "lm_studio_chat_completions"],
    },
    {
      key: "model.api_key_env",
      value: "OPENAI_API_KEY",
      env_override: "MOYAI_API_KEY_ENV",
      value_type: "string",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "model.system_prompt",
      value: "Answer with evidence.",
      env_override: null,
      value_type: "string",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "model.context_window",
      value: "131072",
      env_override: "MOYAI_CONTEXT_WINDOW",
      value_type: "integer",
      required: false,
      min_value: 0,
      max_value: 4294967295,
      options: [],
    },
    {
      key: "model.temperature",
      value: "0.2",
      env_override: null,
      value_type: "number",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "model.top_p",
      value: "0.9",
      env_override: null,
      value_type: "number",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "model.top_k",
      value: "40",
      env_override: "MOYAI_TOP_K",
      value_type: "integer",
      required: false,
      min_value: 0,
      max_value: null,
      options: [],
    },
    {
      key: "model.presence_penalty",
      value: "0.1",
      env_override: null,
      value_type: "number",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "model.frequency_penalty",
      value: "0.2",
      env_override: null,
      value_type: "number",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "model.seed",
      value: "42",
      env_override: "MOYAI_SEED",
      value_type: "integer",
      required: false,
      min_value: 0,
      max_value: null,
      options: [],
    },
    {
      key: "model.stop_sequences",
      value: "END",
      env_override: "MOYAI_STOP_SEQUENCES",
      value_type: "string",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "model.extra_body_json",
      value: "{}",
      env_override: "MOYAI_EXTRA_BODY_JSON",
      value_type: "json",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "model.max_output_tokens",
      value: "4096",
      env_override: "MOYAI_MAX_OUTPUT_TOKENS",
      value_type: "integer",
      required: false,
      min_value: 0,
      max_value: 4294967295,
      options: [],
    },
    {
      key: "model.request_timeout_ms",
      value: "120000",
      env_override: "MOYAI_REQUEST_TIMEOUT_MS",
      value_type: "integer",
      required: false,
      min_value: 1,
      max_value: 3600000,
      options: [],
    },
    {
      key: "model.supports_tools",
      value: "true",
      env_override: "MOYAI_SUPPORTS_TOOLS",
      value_type: "boolean",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "model.supports_reasoning",
      value: "true",
      env_override: "MOYAI_SUPPORTS_REASONING",
      value_type: "boolean",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "model.supports_images",
      value: "true",
      env_override: "MOYAI_SUPPORTS_IMAGES",
      value_type: "boolean",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "model.parallel_tool_calls",
      value: "false",
      env_override: "MOYAI_PARALLEL_TOOL_CALLS",
      value_type: "boolean",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "side_chat.base_url",
      value: "http://127.0.0.1:1234/v1",
      env_override: null,
      value_type: "string",
      required: true,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "side_chat.model",
      value: "side-model",
      env_override: null,
      value_type: "string",
      required: true,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "side_chat.provider_profile",
      value: "openai_compatible",
      env_override: null,
      value_type: "enum",
      required: true,
      min_value: null,
      max_value: null,
      options: ["lm_studio", "openai_compatible", "openai_responses", "lm_studio_chat_completions"],
    },
    {
      key: "side_chat.system_prompt",
      value: "Reply briefly.",
      env_override: null,
      value_type: "string",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "side_chat.context_window",
      value: "65536",
      env_override: null,
      value_type: "integer",
      required: true,
      min_value: 1,
      max_value: 4294967295,
      options: [],
    },
    {
      key: "side_chat.request_timeout_ms",
      value: "120000",
      env_override: null,
      value_type: "integer",
      required: true,
      min_value: 1,
      max_value: 3600000,
      options: [],
    },
    {
      key: "side_chat.connect_timeout_ms",
      value: "10000",
      env_override: null,
      value_type: "integer",
      required: true,
      min_value: 0,
      max_value: null,
      options: [],
    },
    {
      key: "side_chat.max_retries",
      value: "2",
      env_override: null,
      value_type: "integer",
      required: true,
      min_value: 0,
      max_value: 255,
      options: [],
    },
    {
      key: "permissions.access_mode",
      value: "default",
      env_override: "MOYAI_ACCESS_MODE",
      value_type: "enum",
      required: false,
      min_value: null,
      max_value: null,
      options: ["default", "auto_review", "full_access"],
    },
    {
      key: "docling.headers_json",
      value: "{}",
      env_override: "MOYAI_DOCLING_HEADERS",
      value_type: "json",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: 'future.<unsafe>&"',
      value: "raw value",
      env_override: "ENV<unsafe>",
      value_type: "future<type>",
      required: true,
      min_value: null,
      max_value: null,
      options: [],
    },
  ];
  const settingsState = projection({ overlay: "config", config_fields: configFields });
  settingsState.config_draft = {
    ...settingsState.config_draft,
    dirty: true,
    discard_enabled: true,
    commit_enabled: true,
  };
  const local = renderLocal({
    sideChat: {
      catalog: {
        status: "ready",
        source: "global",
        baseUrl: "http://127.0.0.1:1234",
        models: [{ id: "side-model", label: "Side Model" }],
        error: "",
      },
      catalogLoadEnabled: true,
      mutationPending: false,
      operationsOpen: true,
    },
  });
  const html = renderOverlay(settingsState, local);

  const idCounts = new Map<string, number>();
  for (const match of html.matchAll(/\bid="([^"]+)"/g)) {
    idCounts.set(match[1], (idCounts.get(match[1]) ?? 0) + 1);
  }
  assert.deepEqual(
    [...idCounts.entries()].filter(([, count]) => count !== 1),
    [],
    "every Settings id must be unique",
  );

  const controls = Array.from(
    html.matchAll(/<(?:input|select|textarea)\b[^>]*class="[^"]*settings-control[^"]*"[^>]*>/g),
    (match) => match[0],
  );
  assert.equal(controls.length, 27, "twenty-three config controls plus four remote peer connection fields");
  for (const control of controls) {
    const id = /\bid="([^"]+)"/.exec(control)?.[1];
    const describedBy = /\baria-describedby="([^"]+)"/.exec(control)?.[1];
    assert.ok(id, `Settings control lacks a stable id: ${control}`);
    assert.ok(describedBy, `Settings control lacks described help: ${control}`);
    assert.match(html, new RegExp(`<label[^>]+for="${escapeRegExp(id)}"`));
    for (const reference of describedBy.split(/\s+/)) {
      assert.equal(idCounts.get(reference), 1, `${id} references one existing unique #${reference}`);
    }
  }

  const configControls = controls.filter((control) => control.includes("data-config-key="));
  assert.equal(configControls.length, 23);
  for (const control of configControls) assert.match(control, /aria-describedby="[^"]*settings-validation/);
  const modelHelpReferences = configControls
    .filter((control) => control.includes('data-config-key="model.model"'))
    .map((control) => /aria-describedby="([^"]+)"/.exec(control)?.[1].split(/\s+/)
      .find((id) => id.startsWith("settings-config-help-")));
  assert.equal(modelHelpReferences.length, 2);
  assert.equal(modelHelpReferences[0], modelHelpReferences[1]);

  const contextControl = configControls.find((control) => control.includes('data-config-key="model.context_window"'))!;
  const contextHelpId = /aria-describedby="([^"]+)"/.exec(contextControl)![1]
    .split(/\s+/)
    .find((id) => id.startsWith("settings-config-help-"))!;
  const contextHelp = new RegExp(`<small id="${escapeRegExp(contextHelpId)}"[^>]*>([^<]+)</small>`)
    .exec(html)?.[1] ?? "";
  assert.match(contextHelp, /範囲: 0以上4294967295以下/);
  assert.doesNotMatch(contextHelp, /設定キー:|形式:|環境変数:/);
  const technicalDetails = Array.from(html.matchAll(/(<details\b[^>]*class="settings-field-technical"[^>]*>)([\s\S]*?)<\/details>/g))
    .find((match) => match[2].includes("設定キー: model.context_window。"));
  assert.ok(technicalDetails, "technical metadata remains available without expanding the primary field help");
  assert.doesNotMatch(technicalDetails[1], /\sopen(?:\s|>)/, "technical metadata is collapsed initially");
  assert.match(technicalDetails[2], /形式: 整数/);
  assert.match(technicalDetails[2], /環境変数: MOYAI_CONTEXT_WINDOW/);
  assert.doesNotMatch(technicalDetails[2], /<(?:input|select|textarea)\b/, "collapsing metadata must not hide an editor");
  for (const key of [
    "model.temperature",
    "model.top_p",
    "model.top_k",
    "model.presence_penalty",
    "model.frequency_penalty",
    "model.seed",
    "model.stop_sequences",
    "model.extra_body_json",
    "model.supports_reasoning",
    "model.max_output_tokens",
    "model.reasoning_effort",
    "model.reasoning_summary",
    "model.chat_completions_reasoning_parameters",
  ]) {
    assert.doesNotMatch(html, new RegExp(`data-config-key="${escapeRegExp(key)}"`));
  }
  for (const key of [
    "model.context_window",
    "model.system_prompt",
    "model.request_timeout_ms",
    "model.supports_tools",
    "model.supports_images",
    "model.parallel_tool_calls",
    "side_chat.base_url",
    "side_chat.model",
    "side_chat.provider_profile",
    "side_chat.system_prompt",
    "side_chat.context_window",
    "side_chat.request_timeout_ms",
    "side_chat.connect_timeout_ms",
    "side_chat.max_retries",
  ]) {
    assert.match(html, new RegExp(`data-config-key="${escapeRegExp(key)}"`));
  }
  assert.match(html, /id="config-dialog-title">設定</);
  assert.match(html, /class="settings-nav-group" role="heading" aria-level="3">共通設定</);
  assert.match(html, /class="settings-nav-group" role="heading" aria-level="3">チャットごとの設定</);
  assert.match(html, /class="settings-nav-group" role="heading" aria-level="3">画面設定</);
  assert.match(html, /サイドチャットは、開いた時点のモデルとプロンプトを保持します。/);
  assert.doesNotMatch(html, /data-action="configure-side-chat"/);
  assert.match(html, /入力の整理と利用する機能を設定します。回答の長さや思考設定はモデルのホスト側で管理します。/);
  assert.match(html, /id="settings-validation"[^>]*role="status"[^>]*aria-live="polite"/);
  for (const section of ["provider", "side-chat", "permissions", "agents", "tools", "files", "advanced"]) {
    assert.match(
      html,
      new RegExp(`<section id="settings-${section}"[^>]*aria-labelledby="settings-${section}-title"[^>]*aria-describedby="[^"]+"`),
    );
  }
  assert.match(
    html,
    /<div id="settings-model" class="settings-subsection"[^>]*aria-labelledby="settings-model-title"[^>]*aria-describedby="settings-model-help"/,
  );
  assert.doesNotMatch(html, /<unsafe>|future<type>|ENV<unsafe>/);
  assert.match(html, /future\.&lt;unsafe&gt;&amp;&quot;/);
  assert.match(html, /future&lt;type&gt;/);
  assert.match(html, /ENV&lt;unsafe&gt;/);
});

test("canonical row_kind selects specialized transcript rendering", () => {
  const stableHistoryIdentity = "turn:01STABLE:work-summary";
  const running = renderThreadContent(projection({
    thread_empty: false,
    transcript_rows: [{
      row_kind: "work_summary_running",
      step: "1",
      title: "Work",
      body: "running",
      file_changes: [],
      stable_history_identity: stableHistoryIdentity,
    }],
  }));
  const completed = renderThreadContent(projection({
    thread_empty: false,
    transcript_rows: [{
      row_kind: "work_summary_completed",
      step: "1",
      title: "1分作業しました",
      body: "completed",
      file_changes: [],
      stable_history_identity: stableHistoryIdentity,
    }],
  }));

  assert.match(running, /message work-summary work_summary_running/);
  assert.match(running, new RegExp(`data-history-identity="${stableHistoryIdentity}"`));
  assert.match(completed, new RegExp(`data-history-identity="${stableHistoryIdentity}"`));
  assert.match(running, /<details[^>]+open>/);
  assert.doesNotMatch(completed, /<details[^>]+open>/);
  const runningFocusKey = running.match(/<summary data-focus-key="([^"]+)"/)?.[1];
  const completedFocusKey = completed.match(/<summary data-focus-key="([^"]+)"/)?.[1];
  const runningDetailsKey = running.match(/<details data-details-key="([^"]+)"/)?.[1];
  const completedDetailsKey = completed.match(/<details data-details-key="([^"]+)"/)?.[1];
  assert.ok(runningFocusKey);
  assert.equal(completedFocusKey, runningFocusKey, "phase changes retain keyboard focus ownership");
  assert.ok(runningDetailsKey);
  assert.notEqual(completedDetailsKey, runningDetailsKey, "phase changes reset automatic disclosure state");
});

test("authoritative pending steer is separate from canonical transcript and keeps its durable identity", () => {
  const id = "01PENDINGINPUT00000000000000";
  const html = renderThreadContent(projection({
    thread_empty: true,
    transcript_rows: [],
    pending_turn_inputs: [{
      id,
      turn_id: "01TURN000000000000000000000",
      text: "追加で境界条件も確認してください",
      image_count: 1,
      accepted_at_ms: 42,
    }],
  }));

  assert.match(html, /モデルへの送信待ち/);
  assert.match(html, new RegExp(`data-pending-input-id="${id}"`));
  assert.match(html, new RegExp(`data-history-identity="${id}"`));
  assert.match(html, /追加で境界条件も確認してください/);
  assert.match(html, /添付画像 1件/);
  assert.doesNotMatch(html, /history-rail-marker/);
  assert.doesNotMatch(html, /履歴はまだありません/);
});

test("pending steer transitions to one delivered transcript row by exact identity", () => {
  const id = "01DELIVEREDINPUT000000000000";
  const delivered = {
    row_kind: "user" as const,
    stable_history_identity: id,
    step: "",
    title: "ユーザー依頼",
    body: "同じ本文",
    file_changes: [],
  };
  const html = renderThreadContent(projection({
    thread_empty: false,
    transcript_rows: [delivered],
    // A delayed acknowledgement must not resurrect a pending card when the
    // same canonical identity has already crossed into history.
    pending_turn_inputs: [{
      id,
      turn_id: "01TURN000000000000000000000",
      text: "同じ本文",
      image_count: 0,
      accepted_at_ms: 43,
    }],
  }));

  assert.equal((html.match(new RegExp(`data-history-identity="${id}"`, "g")) ?? []).length, 1);
  assert.doesNotMatch(html, /data-pending-input-id=/);
  assert.match(html, /同じ本文/);
});

test("identical pending steer text remains distinct when durable identities differ", () => {
  const html = renderThreadContent(projection({
    thread_empty: true,
    transcript_rows: [],
    pending_turn_inputs: [
      {
        id: "01PENDINGA000000000000000000",
        turn_id: "01TURN000000000000000000000",
        text: "重複して見える本文",
        image_count: 0,
        accepted_at_ms: 44,
      },
      {
        id: "01PENDINGB000000000000000000",
        turn_id: "01TURN000000000000000000000",
        text: "重複して見える本文",
        image_count: 0,
        accepted_at_ms: 45,
      },
    ],
  }));

  assert.equal((html.match(/data-pending-input-id=/g) ?? []).length, 2);
  assert.match(html, /01PENDINGA000000000000000000/);
  assert.match(html, /01PENDINGB000000000000000000/);
  assert.equal((html.match(/重複して見える本文/g) ?? []).length, 2);
});

test("conversation rows render naturally with stable rail anchors and inline earlier history", () => {
  const user = {
    row_kind: "user" as const,
    step: "01",
    title: "ユーザー依頼",
    body: "自然な依頼です",
    file_changes: [],
  };
  const assistant = {
    row_kind: "assistant" as const,
    step: "02",
    title: "Assistant",
    body: "自然な応答です",
    file_changes: [],
  };
  const completed = {
    row_kind: "work_summary_completed" as const,
    step: "03",
    title: "1分作業しました",
    body: "### 作業履歴\n- ファイルを確認しました",
    file_changes: [],
  };
  const html = renderThreadContent(projection({
    thread_empty: false,
    turn_page_offset: 80,
    navigation_loading: false,
    transcript_rows: [user, assistant, completed],
  }));

  assert.match(html, /data-action="load-previous-turn-page"/);
  assert.match(html, /<nav class="history-rail"/);
  assert.equal(html.match(/data-action="jump-history-anchor"/g)?.length, 3);
  assert.match(html, /class="message user"/);
  assert.match(html, /class="message assistant"/);
  assert.doesNotMatch(html, /message-step|<h2>ユーザー依頼<\/h2>|<h2>Assistant<\/h2>/);
  assert.match(html, /work_summary_completed[^>]+data-history-anchor=[\s\S]*?<details[^>]+>/);
  assert.doesNotMatch(html, /work_summary_completed[\s\S]*?<details[^>]+open>/);

  const activeRun = projection({
    thread_empty: false,
    turn_page_offset: 80,
    navigation_admission_open: false,
    turn_page_admission_open: true,
    transcript_rows: [user, assistant, completed],
  });
  assert.equal(
    actionById("load-previous-turn-page")?.enabled?.(activeRun, { index: -1, value: "" }),
    true,
    "read-only history prepend remains available while the owned run is active",
  );

  const loading = projection({
    thread_empty: false,
    turn_page_offset: 80,
    turn_page_has_more: true,
    pending_async_operations: ["turn_page_load"],
    transcript_rows: [user, assistant, completed],
  });
  const loadingHtml = renderThreadContent(loading);
  assert.match(loadingHtml, /data-action="load-previous-turn-page"[^>]+disabled/);
  assert.equal(actionById("load-previous-turn-page")?.enabled?.(loading, { index: -1, value: "" }), false);
  assert.equal(actionById("load-next-turn-page")?.enabled?.(loading, { index: -1, value: "" }), false);

  const renumbered = { ...user, step: "99" };
  assert.equal(transcriptAnchors([user])[0]?.id, transcriptAnchors([renumbered])[0]?.id);
  assert.deepEqual(
    transcriptAnchors([user, user, user]).slice(1).map((anchor) => anchor.id),
    transcriptAnchors([user, user]).map((anchor) => anchor.id),
    "prepending an identical projected row preserves the visible suffix anchors",
  );
  const runningUpdated = {
    row_kind: "work_summary_running" as const,
    step: "04",
    title: "作業中",
    body: "新しい進捗",
    file_changes: [],
  };
  assert.equal(
    transcriptAnchors([{ ...runningUpdated, body: "以前の進捗" }])[0]?.detailsId,
    transcriptAnchors([runningUpdated])[0]?.detailsId,
    "running work disclosure identity survives visible progress updates",
  );
  assert.equal(
    transcriptAnchors([{ ...runningUpdated, title: "12s 作業中" }])[0]?.detailsId,
    transcriptAnchors([{ ...runningUpdated, title: "14s 作業中" }])[0]?.detailsId,
    "running work disclosure identity survives elapsed-title updates",
  );

  const longRail = renderThreadContent(projection({
    thread_empty: false,
    transcript_rows: Array.from({ length: 60 }, (_, index) => ({
      ...user,
      body: `依頼 ${index}`,
    })),
  }));
  assert.equal(
    longRail.match(/data-action="jump-history-anchor"/g)?.length,
    60,
    "ordinary histories keep every conversational marker mounted",
  );

  const boundedRail = renderThreadContent(projection({
    thread_empty: false,
    transcript_rows: Array.from({ length: 240 }, (_, index) => ({
      ...user,
      body: `長い依頼 ${index}`,
    })),
  }));
  assert.equal(
    boundedRail.match(/data-action="jump-history-anchor"/g)?.length,
    96,
    "very long histories remain bounded to the viewport-scale rail capacity",
  );
  const boundedTargets = [...boundedRail.matchAll(/data-history-target="([^"]+)"/g)]
    .map((match) => match[1]);
  assert.equal(new Set(boundedTargets).size, 96, "sampled rail targets remain unique");
  assert.match(boundedRail, /aria-label="依頼: 長い依頼 0"/);
  assert.match(boundedRail, /aria-label="依頼: 長い依頼 239"/);
});

test("durable Sub Agent events stay inside their turn history and the root final summary stays visible", () => {
  const rows = [
    {
      row_kind: "user" as const,
      step: "01",
      title: "ユーザー依頼",
      body: "2つのSub Agentで確認してください",
      file_changes: [],
    },
    {
      row_kind: "sub_agent_started" as const,
      step: "02",
      title: "/root/navigation_review",
      body: "",
      file_changes: [],
    },
    {
      row_kind: "sub_agent_updated" as const,
      step: "03",
      title: "/root/navigation_review",
      body: "**RAW CHILD REPORT** https://example.invalid/private",
      file_changes: [],
    },
    {
      row_kind: "work_summary_completed" as const,
      step: "04",
      title: "12s作業しました",
      body: "### 作業サマリ\n- 結果: セッションは完了しました。\n\n### 作業履歴\n- [完了] wait_agent\n  出力: 2件完了",
      file_changes: [],
    },
    {
      row_kind: "assistant" as const,
      step: "05",
      title: "応答",
      body: "---\n\n**`navigation_review` の最終結果**\n\n- 問題ありません。",
      file_changes: [],
    },
    {
      row_kind: "user" as const,
      step: "06",
      title: "ユーザー依頼",
      body: "次の依頼",
      file_changes: [],
    },
    {
      row_kind: "work_summary_completed" as const,
      step: "07",
      title: "2s作業しました",
      body: "### 作業サマリ\n- 結果: セッションは完了しました。",
      file_changes: [],
    },
    {
      row_kind: "assistant" as const,
      step: "08",
      title: "応答",
      body: "次の依頼も完了しました。",
      file_changes: [],
    },
  ];
  const html = renderThreadContent(projection({
    thread_empty: false,
    agent_tree_active: false,
    transcript_rows: rows,
    agent_activity_rows: [{
      agent_path: "/root/navigation_review",
      session_id: "child-navigation",
      task_name: "Navigation review",
      task_preview: "**履歴を確認** [内部リンク](https://secret.invalid/task) <script>alert('x')</script> `cargo test`",
      status: "completed",
      current_activity: "",
      result_preview: "RAW CHILD REPORT",
      started_order: 1,
      updated: false,
      active_turn_id: null,
      interrupt_target: null,
    }],
  }));

  const firstSummary = html.indexOf("12s作業しました");
  const firstFinal = html.indexOf("navigation_review</code> の最終結果");
  const secondUser = html.indexOf("次の依頼", firstFinal);
  const secondFinal = html.indexOf("次の依頼も完了しました。", secondUser);
  assert.ok(firstSummary >= 0 && firstSummary < firstFinal);
  assert.ok(firstFinal < secondUser && secondUser < secondFinal);
  const firstSummaryHtml = html.slice(firstSummary, firstFinal);
  assert.match(
    firstSummaryHtml,
    /class="agent-job-card work-summary-agent-card[^\"]*"[\s\S]*data-agent-path="\/root\/navigation_review"[\s\S]*Navigation review[\s\S]*履歴を確認 内部リンク[\s\S]*cargo test[\s\S]*完了しました/,
  );
  assert.equal(firstSummaryHtml.match(/data-agent-path="\/root\/navigation_review"/g)?.length, 1);
  assert.equal(firstSummaryHtml.match(/data-focus-key="agent-history:[^"]+:\/root\/navigation_review"/g)?.length, 1);
  assert.match(firstSummaryHtml, /data-action="show-agent-pane"[\s\S]*aria-controls="sub-agent-inspector" aria-expanded="false"/);
  assert.match(firstSummaryHtml, /&lt;script&gt;alert\(&#0?39;x&#0?39;\)&lt;\/script&gt;/);
  assert.doesNotMatch(firstSummaryHtml, /https:\/\/secret\.invalid|<script>/);
  assert.match(html, /<strong><code>navigation_review<\/code> の最終結果<\/strong>/);
  assert.match(html, /class="work-history-event"[\s\S]*Sub Agentの完了を待ちました/);
  assert.doesNotMatch(html, /出力: 2件完了/);
  assert.doesNotMatch(html, /RAW CHILD REPORT|example\.invalid|message sub_agent_|agent-inline-activity/);
  assert.equal(html.match(/data-action="jump-history-anchor"/g)?.length, 6);
});

test("Sub Agent lifecycle events coalesce to one described card per path in spawn order", () => {
  const activity = (path: string, order: number, preview: string) => ({
    agent_path: path,
    session_id: `child-${order}`,
    task_name: path.endsWith("alpha") ? "Alpha security review" : "Beta navigation review",
    task_preview: preview,
    status: "completed" as const,
    current_activity: "",
    result_preview: `RAW ${path} RESULT`,
    started_order: order,
    updated: true,
    active_turn_id: null,
    interrupt_target: null,
  });
  const html = renderThreadContent(projection({
    thread_empty: false,
    transcript_rows: [
      { row_kind: "sub_agent_started", step: "1", title: "/root/alpha", body: "", file_changes: [] },
      { row_kind: "sub_agent_started", step: "2", title: "/root/beta", body: "", file_changes: [] },
      { row_kind: "sub_agent_updated", step: "3", title: "/root/beta", body: "RAW BETA REPORT", file_changes: [] },
      { row_kind: "sub_agent_updated", step: "4", title: "/root/alpha", body: "RAW ALPHA REPORT", file_changes: [] },
      { row_kind: "sub_agent_updated", step: "5", title: "/root/beta", body: "RAW BETA REPORT 2", file_changes: [] },
      { row_kind: "work_summary_completed", step: "6", title: "history", body: "", file_changes: [] },
    ],
    agent_activity_rows: [
      activity("/root/alpha", 1, "Sandbox boundaryを確認"),
      activity("/root/beta", 2, "履歴navigationを確認"),
    ],
    current_turn_agent_activity_rows: [],
  }));

  assert.equal(html.match(/class="agent-job-card work-summary-agent-card/g)?.length, 2);
  assert.equal(html.match(/data-agent-path="\/root\/alpha"/g)?.length, 1);
  assert.equal(html.match(/data-agent-path="\/root\/beta"/g)?.length, 1);
  assert.equal(html.match(/class="agent-status-label">完了しました/g)?.length, 2);
  assert.match(html, /Alpha security review[\s\S]*Sandbox boundaryを確認/);
  assert.match(html, /Beta navigation review[\s\S]*履歴navigationを確認/);
  assert.ok(html.indexOf("/root/alpha") < html.indexOf("/root/beta"));
  assert.doesNotMatch(html, /RAW (?:ALPHA|BETA|\/root)/);
});

test("live Sub Agent fallback is owned by the current turn and survives child quiescence", () => {
  const agent = (path: string, status: "running" | "completed", order: number) => {
    const childSessionId = childSessionIdForOrder(order);
    const expectedTurnId = childTurnIdForOrder(order);
    return {
      agent_path: path,
      session_id: childSessionId,
      task_name: path.split("/").at(-1) ?? "agent",
      task_preview: "bounded review",
      status,
      current_activity: "",
      result_preview: status === "completed" ? "done" : "",
      started_order: order,
      updated: false,
      active_turn_id: status === "running" ? expectedTurnId : null,
      interrupt_target: status === "running"
        ? agentInterruptTarget({ agentPath: path, childSessionId, expectedTurnId })
        : null,
    };
  };
  const oldAgent = agent("/root/old_agent", "completed", 1);
  const currentAgent = agent("/root/current_agent", "completed", 2);
  const html = renderThreadContent(projection({
    thread_empty: false,
    agent_tree_active: false,
    transcript_rows: [
      { row_kind: "user", step: "1", title: "ユーザー依頼", body: "first", file_changes: [] },
      { row_kind: "sub_agent_started", step: "2", title: oldAgent.agent_path, body: "", file_changes: [] },
      { row_kind: "sub_agent_updated", step: "3", title: oldAgent.agent_path, body: "done", file_changes: [] },
      { row_kind: "work_summary_completed", step: "4", title: "first history", body: "", file_changes: [] },
      { row_kind: "assistant", step: "5", title: "応答", body: "first final", file_changes: [] },
      { row_kind: "user", step: "6", title: "ユーザー依頼", body: "second", file_changes: [] },
      { row_kind: "work_summary_running", step: "7", title: "second history", body: "", file_changes: [] },
    ],
    agent_activity_rows: [oldAgent, currentAgent],
    current_turn_agent_activity_rows: [currentAgent],
  }));

  const firstHistory = html.lastIndexOf("first history");
  const firstFinal = html.lastIndexOf("first final");
  const secondHistory = html.lastIndexOf("second history");
  assert.match(html.slice(firstHistory, firstFinal), /Old agent[\s\S]*完了しました/);
  assert.doesNotMatch(html.slice(firstHistory, firstFinal), /Current agent/);
  assert.match(html.slice(secondHistory), /Current agent[\s\S]*完了しました/);
  assert.doesNotMatch(html.slice(secondHistory), /Old agent/);
});

test("a current Sub Agent remains visible before the live WorkSummary is projected", () => {
  const childSessionId = childSessionIdForOrder(3);
  const expectedTurnId = childTurnIdForOrder(3);
  const currentAgent = {
    agent_path: "/root/early_agent",
    session_id: childSessionId,
    task_name: "Early review",
    task_preview: "review while the turn starts",
    status: "running" as const,
    current_activity: "checking",
    result_preview: "",
    started_order: 1,
    updated: false,
    active_turn_id: expectedTurnId,
    interrupt_target: agentInterruptTarget({
      agentPath: "/root/early_agent",
      childSessionId,
      expectedTurnId,
    }),
  };
  const html = renderThreadContent(projection({
    thread_empty: false,
    transcript_rows: [
      { row_kind: "user", step: "1", title: "ユーザー依頼", body: "start", file_changes: [] },
    ],
    agent_tree_active: true,
    agent_activity_rows: [currentAgent],
    current_turn_agent_activity_rows: [currentAgent],
  }));

  assert.match(html, /class="agent-inline-activity"/);
  assert.match(html, /Early review/);
});

test("interleaved legacy communication markers coalesce to one card per Sub Agent", () => {
  const activity = (path: string, order: number) => ({
    agent_path: path,
    session_id: `child-${order}`,
    task_name: path.endsWith("alpha") ? "Alpha review" : "Beta review",
    task_preview: "review",
    status: "completed" as const,
    current_activity: "",
    result_preview: "done",
    started_order: order,
    updated: true,
    active_turn_id: null,
    interrupt_target: null,
  });
  const html = renderThreadContent(projection({
    thread_empty: false,
    transcript_rows: [
      { row_kind: "sub_agent_updated", step: "1", title: "/root/alpha", body: "", file_changes: [] },
      { row_kind: "sub_agent_updated", step: "2", title: "/root/beta", body: "", file_changes: [] },
      { row_kind: "sub_agent_updated", step: "3", title: "/root/alpha", body: "", file_changes: [] },
      { row_kind: "sub_agent_updated", step: "4", title: "/root/beta", body: "", file_changes: [] },
      { row_kind: "work_summary_completed", step: "5", title: "history", body: "", file_changes: [] },
    ],
    agent_activity_rows: [activity("/root/alpha", 1), activity("/root/beta", 2)],
    current_turn_agent_activity_rows: [],
  }));

  assert.equal(html.match(/data-agent-path="\/root\/alpha"/g)?.length, 1);
  assert.equal(html.match(/data-agent-path="\/root\/beta"/g)?.length, 1);
});

test("a started-only detached Sub Agent uses terminal status only at its latest turn", () => {
  const currentAgent = {
    agent_path: "/root/reused_agent",
    session_id: "child-reused",
    task_name: "reused_agent",
    task_preview: "follow-up",
    status: "completed" as const,
    current_activity: "",
    result_preview: "done",
    started_order: 1,
    updated: true,
    active_turn_id: null,
    interrupt_target: null,
  };
  const html = renderThreadContent(projection({
    thread_empty: false,
    transcript_rows: [
      { row_kind: "user", step: "1", title: "ユーザー依頼", body: "first", file_changes: [] },
      { row_kind: "sub_agent_started", step: "2", title: currentAgent.agent_path, body: "", file_changes: [] },
      { row_kind: "work_summary_completed", step: "3", title: "old history", body: "", file_changes: [] },
      { row_kind: "assistant", step: "4", title: "応答", body: "old final", file_changes: [] },
      { row_kind: "user", step: "5", title: "ユーザー依頼", body: "follow-up", file_changes: [] },
      { row_kind: "sub_agent_updated", step: "6", title: currentAgent.agent_path, body: "done", file_changes: [] },
      { row_kind: "work_summary_completed", step: "7", title: "new history", body: "", file_changes: [] },
    ],
    agent_activity_rows: [currentAgent],
    current_turn_agent_activity_rows: [currentAgent],
  }));

  const oldHistory = html.lastIndexOf("old history");
  const oldFinal = html.lastIndexOf("old final");
  const newHistory = html.lastIndexOf("new history");
  const oldHistoryHtml = html.slice(oldHistory, oldFinal);
  const newHistoryHtml = html.slice(newHistory);
  assert.equal(html.match(/data-agent-path="\/root\/reused_agent"/g)?.length, 2);
  assert.match(oldHistoryHtml, /Reused agent[\s\S]*作業を開始しました/);
  assert.doesNotMatch(oldHistoryHtml, /follow-up/);
  assert.match(newHistoryHtml, /Reused agent[\s\S]*follow-up[\s\S]*完了しました/);
});

test("a bounded canonical suffix keeps unmatched durable Sub Agents visible as compact cards", () => {
  const boundaryAgent = {
    agent_path: "/root/boundary_agent",
    session_id: "child-boundary",
    task_name: "Boundary security review",
    task_preview: "review the truncated turn boundary",
    status: "completed" as const,
    current_activity: "",
    result_preview: "done",
    started_order: 1,
    updated: false,
    active_turn_id: null,
    interrupt_target: null,
  };
  const html = renderThreadContent(projection({
    thread_empty: false,
    transcript_rows: [
      { row_kind: "work_summary_completed", step: "1", title: "bounded history", body: "", file_changes: [] },
      { row_kind: "assistant", step: "2", title: "応答", body: "root final", file_changes: [] },
    ],
    agent_activity_rows: [boundaryAgent],
    current_turn_agent_activity_rows: [],
  }));

  assert.match(html, /class="agent-inline-activity"/);
  assert.match(html, /Boundary security review/);
  assert.match(html, /data-action="show-agent-pane" data-agent-path="\/root\/boundary_agent"/);
  assert.ok(html.indexOf("Boundary security review") < html.lastIndexOf("root final"));
});

test("delegated keyboard activation skips native controls to avoid a second native click", () => {
  assert.equal(shouldDispatchDelegatedKeyboardAction("BUTTON"), false);
  assert.equal(shouldDispatchDelegatedKeyboardAction("A", true), false);
  assert.equal(shouldDispatchDelegatedKeyboardAction("SUMMARY"), false);
  assert.equal(shouldDispatchDelegatedKeyboardAction("DIV"), true);
});

test("config owner generation remains exact beyond JavaScript's safe integer range", () => {
  const current = {
    ...projection().config_target,
    configGeneration: "9007199254740993",
  };
  const newerFence = {
    ...current,
    configGeneration: "9007199254740994",
  };

  assert.equal(sameConfigMutationTarget(current, { ...current }), true);
  assert.equal(sameConfigMutationTarget(current, newerFence), false);
});

test("Stop target is a distinct tagged Rust owner projection", () => {
  const target = projection({
    can_cancel_run: true,
    stop_target: rootStopTarget({
      rootGeneration: "9007199254740993",
      admissionRevision: "9007199254740994",
      permissionConfirmationId: "41",
    }),
  }).stop_target!;

  assert.deepEqual(
    Object.keys(target).sort(),
    [
      "admissionRevision",
      "kind",
      "latestTurnId",
      "permissionConfirmationId",
      "rootGeneration",
      "sessionId",
      "workspacePath",
    ],
  );
  assert.equal(target.kind, "root");
  assert.equal(target.rootGeneration, "9007199254740993");
  assert.equal(target.admissionRevision, "9007199254740994");
  assert.equal("runtimeOwnerToken" in target, false);
});

test("incomplete canonical turn is rendered as nonterminal evidence", () => {
  const html = renderThreadContent(projection({
    thread_empty: false,
    transcript_rows: [{
      row_kind: "work_summary_incomplete",
      step: "1",
      title: "状態未確定の作業履歴",
      body: "### 作業サマリ\n- 結果: この turn の完了状態は未確定です。",
      file_changes: [],
    }],
  }));
  assert.match(html, /message work-summary work_summary_incomplete/);
  assert.match(html, /状態未確定/);
  assert.match(html, /<details[^>]+open>/);
});

test("Settings Main and Provider present typed model-load failure and recover on retry", () => {
  const failure = { kind: "error" as const, title: "Providerモデル一覧を読み込めません", hint: "接続先を確認して再試行してください。", details: "connection refused <diagnostic>" };
  for (const overlay of ["config", "provider"] as const) {
    const current = projection({ overlay, provider_status: failure, provider_catalog_base_url: null, provider_catalog_profile: null });
    const id = overlay === "config" ? "main-provider-model-catalog-status" : "provider-status";
    const html = renderOverlay(current);
    assert.match(html, new RegExp(`id="${id}" class="provider-status error"[^>]*data-settings-passive="${id}"[^>]*data-settings-preserve-focused-region`));
    assert.match(html, /<strong data-provider-status-title>Providerモデル一覧を読み込めません<\/strong>/);
    assert.match(html, /<p data-provider-status-hint>接続先を確認して再試行してください。<\/p>/);
    assert.match(html, /<pre data-provider-status-details>connection refused &lt;diagnostic&gt;<\/pre>/);
    assert.doesNotMatch(html, new RegExp(`data-details-key="${id}-details"[^>]*\\bopen`));
    assert.doesNotMatch(renderOverlay({ ...current, provider_loading: true, provider_status: { kind: "loading", title: "読込中", hint: "", details: "" } }), /connection refused|Providerモデル一覧を読み込めません/);
    assert.doesNotMatch(renderOverlay({ ...current, provider_status: { kind: "success", title: "読み込みました", hint: "選択できます", details: "" } }), /connection refused|Providerモデル一覧を読み込めません/);
  }
});

test("a completed provider failure cannot describe a subsequently edited connection target", () => {
  const initial = projection({ overlay: "provider", provider_catalog_base_url: null, provider_catalog_profile: null });
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, initial, null);
  beginProviderCatalogRequest(ui, initial);
  const dispatched = captureDraftMutation(ui, "load_provider_models");
  const loading = projection({ ...initial, projection_revision: "2", provider_loading: true,
    provider_status: { kind: "loading", title: "Loading", hint: "", details: "" } });
  acknowledgeDraftMutation(ui, loading, "load_provider_models", dispatched);
  reconcileUiDrafts(ui, initial, loading, dispatched);
  const failed = projection({ ...initial, projection_revision: "3", provider_loading: false,
    provider_status: { kind: "error", title: "読み込めません", hint: "接続先を確認", details: "old connection failure" } });
  reconcileUiDrafts(ui, loading, failed, null);
  assert.equal(projectViewState(failed, ui).provider_status.kind, "error");
  ui.drafts.provider.baseUrl = "http://127.0.0.1:45678";
  ui.drafts.providerRevision += 1;
  ui.drafts.providerCatalogIdentityRevision += 1;
  const changed = projectViewState(failed, ui);
  assert.equal(changed.provider_status.kind, "warning");
  assert.equal(changed.provider_status.title, "モデル一覧の対象が変更されました");
  assert.doesNotMatch(changed.provider_status.details, /old connection failure/);
});

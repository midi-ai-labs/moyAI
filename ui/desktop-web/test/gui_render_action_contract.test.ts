import assert from "node:assert/strict";
import test from "node:test";
import { deviceUiFixture } from "./device_network_fixture.ts";
import { deviceNetworkPresentation } from "../src/device_network_state.ts";

import { ACTIONS, actionById, actionEnabledById } from "../src/actions.ts";
import {
  renderArtifactPane,
  renderComposer,
  renderConfirmation,
  renderDesktopMarkup,
  renderLocalConfirmation,
  renderOverlay,
  renderPendingTurnInputs,
  renderPlanProjection,
  renderRunStatusStrip,
  renderSideChatDeleteConfirmation,
  renderSidebar,
  renderStartupSplash,
  renderThreadContent,
  renderTitlebar,
  renderTopbar,
  type LocalConfirmation,
} from "../src/render.ts";
import {
  createDesktopRenderModel,
  DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION,
  type DesktopRenderLocalPresentation,
} from "../src/render_projection.ts";
import { validateSessionSettingsDraft } from "../src/session_settings_state.ts";
import type {
  AgentActivityRow,
  ConfigFieldProjection,
  DesktopViewState,
  SessionRow,
} from "../src/types.ts";
import type { AgentExecutionCacheEntry } from "../src/ui_state.ts";
import { rootStopTarget, turnStopTarget } from "./stop_target_fixture.ts";

interface RenderedSurface {
  name: string;
  html: string;
}

const SESSION_IDLE = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const SESSION_ACTIVE = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const SESSION_ARCHIVED = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const ROOT_SESSION = SESSION_IDLE;
const AGENT_SESSION = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";
const IDLE_TURN = "01ARZ3NDEKTSV4RRFFQ69G5FB0";
const ACTIVE_TURN = "01ARZ3NDEKTSV4RRFFQ69G5FB1";
const AGENT_TURN = "01ARZ3NDEKTSV4RRFFQ69G5FB2";
const PENDING_TURN = "01ARZ3NDEKTSV4RRFFQ69G5FB3";
const SIDE_CHAT = "01ARZ3NDEKTSV4RRFFQ69G5FB4";
const QUICK_SESSION = "01ARZ3NDEKTSV4RRFFQ69G5FB5";
const PROJECT_ID = "01ARZ3NDEKTSV4RRFFQ69G5FB6";
const SIDE_MESSAGE_USER = "01ARZ3NDEKTSV4RRFFQ69G5FB7";
const SIDE_MESSAGE_ASSISTANT = "01ARZ3NDEKTSV4RRFFQ69G5FB8";
const HISTORY_USER = "01ARZ3NDEKTSV4RRFFQ69G5FB9";
const HISTORY_ASSISTANT = "01ARZ3NDEKTSV4RRFFQ69G5FBA";
const PENDING_INPUT = "01ARZ3NDEKTSV4RRFFQ69G5FBB";
const PERMISSION_ID = "41";

const idleSession: SessionRow = {
  session_id: SESSION_IDLE,
  title: "Idle session",
  status: "idle",
  loaded_status: "idle",
  archived: false,
  pending_permission_requests: 0,
  pending_user_input_requests: 0,
  admission_revision: "4",
  short_id: "idle",
  label: "Idle session",
};

const activeSession: SessionRow = {
  ...idleSession,
  session_id: SESSION_ACTIVE,
  title: "Active session",
  status: "running",
  loaded_status: "active",
  active_turn_id: ACTIVE_TURN,
  active_turn_sequence_no: 4,
  interrupt_target: turnStopTarget({
    sessionId: SESSION_ACTIVE,
    turnId: ACTIVE_TURN,
    rootEpoch: "5",
  }),
  short_id: "active",
  label: "Active session",
};

const archivedSession: SessionRow = {
  ...idleSession,
  session_id: SESSION_ARCHIVED,
  title: "Archived session",
  archived: true,
  short_id: "archived",
  label: "Archived session",
};

const runningAgent: AgentActivityRow = {
  agent_path: "/root/gui-audit",
  session_id: AGENT_SESSION,
  task_name: "GUI audit",
  task_preview: "Inspect controls",
  status: "running",
  current_activity: "Checking actions",
  result_preview: "",
  started_order: 1,
  updated: true,
  active_turn_id: AGENT_TURN,
  interrupt_target: {
    workspacePath: "C:/workspace",
    rootSessionId: ROOT_SESSION,
    agentPath: "/root/gui-audit",
    childSessionId: AGENT_SESSION,
    expectedTurnId: AGENT_TURN,
    admissionRevision: "1",
  },
};

function representativeState(overrides: Partial<DesktopViewState> = {}): DesktopViewState {
  return {
    projection_revision: "11",
    workspace_path: "C:/workspace",
    provider_label: "Local provider",
    model_label: "model-a",
    access_label: "default",
    access_target: {
      workspacePath: "C:/workspace",
      sessionId: SESSION_IDLE,
      configGeneration: "9",
      accessMode: "default",
      runtimeOwnerToken: "tree:5",
    },
    session_settings: {
      available: false,
      base_url: "http://127.0.0.1:1234/v1",
      model: "model-a",
      provider_profile: "openai_compatible",
      api_key_env: "",
      access_mode: "default",
      context_window: "131072",
      max_output_tokens: "8192",
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
    current_session_label: "Idle session",
    selected_session_title: "Idle session",
    status_message: "Ready",
    status_detail: "",
    status_code: "plain",
    run_status_key: "running",
    run_status_text: "Running",
    run_phase: "provider",
    run_active_step: "Generating",
    latest_tool_summary: "Reading files",
    plan: {
      explanation: "Representative UI state",
      steps: [{ step: "Inspect controls", status: "in_progress" }],
    },
    progress_text: "1/2",
    tool_status_text: "inspect_directory",
    token_meter_label: "12%",
    token_meter_title: "Context use 12%",
    token_meter_level: "low",
    session_usage_label: "セッション累計: 1.2k token（一部）",
    session_usage_title: "canonical terminal telemetry: 2 / 3 turn計測",
    session_usage_state: "partial",
    confirmation_visible: true,
    confirmation_id: PERMISSION_ID,
    confirmation_text: "Allow this action?",
    confirmation: {
      summary: "Write a test result",
      details: ["write RESULTS.md"],
      targets: ["C:/workspace/RESULTS.md"],
      outside_workspace: false,
      risks: ["file mutation"],
      agent_path: "/root/gui-audit",
      agent_task_name: "GUI audit",
    },
    startup: {
      status: "ready",
      title: "Ready",
      message: "Configuration ready",
      detail: "",
      action_overlay: "none",
      initial_setup_required: false,
      initial_setup_reason: null,
      global_config_path: null,
      setup_target: null,
      checks: [{ status: "pass", label: "Storage", message: "Ready" }],
    },
    composer_commit_generation: "4",
    draft_target: {
      workspacePath: "C:/workspace",
      sessionId: SESSION_IDLE,
      ownerGeneration: "2",
    },
    draft_prompt: "Check every control",
    image_input: "C:/workspace/image.png",
    attached_images: ["C:/workspace/image.png"],
    composer_submit_mode: "new_request",
    can_submit: true,
    can_cancel_run: true,
    run_target: {
      workspacePath: "C:/workspace",
      sessionId: SESSION_IDLE,
      runtimeOwnerToken: "tree:5",
      permissionConfirmationId: PERMISSION_ID,
      expectedState: { kind: "idle", latestTurnId: IDLE_TURN, admissionRevision: "4" },
    },
    stop_target: rootStopTarget({
      sessionId: SESSION_IDLE,
      rootGeneration: "5",
      latestTurnId: IDLE_TURN,
      admissionRevision: "4",
      permissionConfirmationId: PERMISSION_ID,
    }),
    busy: false,
    task_activity_state: "attention",
    async_polling_required: true,
    pending_async_operations: ["run"],
    navigation_loading: false,
    navigation_admission_open: true,
    turn_page_admission_open: true,
    post_run_refresh_pending: false,
    background_mutation_pending: false,
    overlay: "none",
    about: {
      product_name: "moyAI",
      version: "test",
      codename: "test-codename",
      license_identifier: "MIT",
      copyright_notice: "Copyright test",
    },
    side_chat: {
      configured: true,
      deleting: false,
      chat_id: SIDE_CHAT,
      owner_session_id: SESSION_IDLE,
      model: "side-model",
      system_prompt: "",
      base_url: "http://127.0.0.1:1234/v1",
      provider_profile: "openai_compatible",
      status: "running",
      phase: "generating",
      last_error: "",
      generation: "6",
      draft_text: "Side question",
      draft_quote: null,
      draft_revision: "3",
      context_scope: "owner_session",
      context_as_of_append_position: "42",
      context_truncated: false,
      messages: [
        { id: SIDE_MESSAGE_USER, sequence_no: 1, role: "user", content: "Question" },
        { id: SIDE_MESSAGE_ASSISTANT, sequence_no: 2, role: "assistant", content: "Answer" },
      ],
      can_send: false,
      can_cancel: true,
    },
    project_rows: [{ project_id: PROJECT_ID, label: "Project A", path: "C:/workspace" }],
    selected_project_index: 0,
    session_rows: [idleSession, activeSession, archivedSession],
    chat_session_rows: [{
      ...idleSession,
      session_id: QUICK_SESSION,
      title: "Quick chat",
      short_id: "quick",
      label: "Quick chat",
    }],
    selected_session_index: 0,
    session_search_text: "",
    session_search_include_archived: true,
    thread_empty: false,
    transcript_rows: [
      {
        row_kind: "user",
        stable_history_identity: HISTORY_USER,
        step: "1",
        title: "User",
        body: "Please check the GUI",
        file_changes: [],
      },
      {
        row_kind: "assistant",
        stable_history_identity: HISTORY_ASSISTANT,
        step: "2",
        title: "Assistant",
        body: "Checking",
        file_changes: [],
      },
    ],
    pending_turn_inputs: [{
      id: PENDING_INPUT,
      turn_id: PENDING_TURN,
      text: "Also check keyboard use",
      image_count: 0,
      accepted_at_ms: 10,
    }],
    turn_page_offset: 80,
    turn_page_limit: 80,
    turn_page_total: 160,
    turn_page_has_more: true,
    artifact_rows: [{
      label: "RESULTS.md",
      path: "C:/workspace/RESULTS.md",
      kind: "file",
      action: "modified",
    }],
    selected_artifact_index: 0,
    artifact_preview_available: true,
    artifact_preview_text: "Results",
    file_change_rows: [{
      label: "RESULTS.md",
      path: "C:/workspace/RESULTS.md",
      action: "modified",
      summary: "Added GUI evidence",
    }],
    file_change_summary_text: "1 file changed",
    agent_activity_rows: [runningAgent],
    current_turn_agent_activity_rows: [runningAgent],
    agent_tree_active: true,
    local_search_text: "",
    local_search_results_text: "",
    command_rows: [{ name: "case1", label: "Case 1", path: "/case1" }],
    provider_base_url: "http://127.0.0.1:1234/v1",
    provider_profile: "openai_compatible",
    provider_api_key_env: "",
    provider_effective_base_url: "http://127.0.0.1:1234/v1",
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
    provider_models: ["Model A"],
    provider_model_ids: ["model-a"],
    provider_selected_index: 0,
    provider_status: {
      kind: "success",
      title: "Loaded",
      hint: "Choose a model",
      details: "",
    },
    provider_selected_model_summary: ["Model: model-a"],
    provider_loading: false,
    provider_apply_enabled: true,
    config_fields: [
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
        value: "",
        env_override: null,
        value_type: "string",
        required: false,
        min_value: null,
        max_value: null,
        options: [],
      },
      {
        key: "side_chat.context_window",
        value: "131072",
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
    ],
    config_target: {
      workspacePath: "C:/workspace",
      sessionId: SESSION_IDLE,
      configGeneration: "9",
    },
    docling_readiness: {
      status: "idle",
      endpoint: "",
      httpStatus: null,
      message: "Docling readiness has not been checked.",
    },
    workspace_input: "C:/workspace",
    review_raw_text: "Raw prompt",
    review_draft_text: "Improved prompt",
    review_target: {
      workspacePath: "C:/workspace",
      sessionId: SESSION_IDLE,
      ownerGeneration: "2",
      requestId: "41",
      expectedState: { kind: "idle", latestTurnId: IDLE_TURN, admissionRevision: "4" },
    },
    review_status_text: "Ready",
    send_enhanced_enabled: true,
    send_raw_enabled: true,
    history_export_enabled: true,
    enhance_enabled: true,
    image_input_enabled: true,
    window_opacity_percent: 90,
    ...overrides,
  };
}

function defaultRenderLocal(overrides: {
  artifactPane?: Partial<DesktopRenderLocalPresentation["artifactPane"]>;
  attachmentTrayOpen?: boolean;
  configMutationPending?: boolean;
  initialSetup?: Partial<DesktopRenderLocalPresentation["initialSetup"]>;
  modal?: Partial<DesktopRenderLocalPresentation["modal"]>;
  sessionSettings?: Partial<DesktopRenderLocalPresentation["sessionSettings"]>;
  sideChat?: Partial<DesktopRenderLocalPresentation["sideChat"]>;
} = {}): DesktopRenderLocalPresentation {
  return {
    ...DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION,
    artifactPane: {
      ...DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION.artifactPane,
      collapsed: false,
      mode: "output",
      ...overrides.artifactPane,
    },
    attachmentTrayOpen: overrides.attachmentTrayOpen ?? true,
    configMutationPending: overrides.configMutationPending ?? false,
    initialSetup: {
      ...DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION.initialSetup,
      ...overrides.initialSetup,
    },
    modal: {
      ...DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION.modal,
      ...overrides.modal,
    },
    sessionSettings: {
      ...DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION.sessionSettings,
      ...overrides.sessionSettings,
    },
    sideChat: {
      ...DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION.sideChat,
      draft: "Side question",
      catalog: {
        status: "ready",
        source: "global",
        baseUrl: "http://127.0.0.1:1234",
        models: [{ id: "side-model", label: "Side model", loadState: "loaded" }],
        error: "",
      },
      catalogLoadEnabled: true,
      mutationPending: false,
      deleteConfirmation: null,
      ...overrides.sideChat,
    },
  };
}

function representativeSurfaces(): RenderedSurface[] {
  const base = representativeState();
  const local = defaultRenderLocal();
  local.mcpHistory = { ...local.mcpHistory, loaded: true, rows: [{ id: "ref-a", direction: "instruction",
    created_at_ms: 1_700_000_000_000, updated_at_ms: null, title: "Investigate WinB", peer_label: "WinB", target_label: "temp",
    state: "running", stop_status: "none", session_id: "session-a", job_id: "job-b", profile_id: "profile-b",
    root_task_id: "session-a", device_path: ["WinA", "WinB"], can_stop: true, state_source: "last_observed", result_received: false }] };
  local.deviceNetwork = deviceNetworkPresentation(deviceUiFixture());
  local.deviceNetwork.jobs.outgoing[0].state = "completed";
  local.deviceNetwork.jobs.outgoing[0].can_stop = false;
  local.mcpPeers = { ...local.mcpPeers, rows: [{ id: "WinB", base_url: "https://192.168.10.22/mcp", enabled: true, credential_configured: true, certificate_sha256: null }] };
  const surfaces: RenderedSurface[] = [
    { name: "startup", html: renderStartupSplash(base, 1_000, 500) },
    { name: "titlebar", html: renderTitlebar(true, false, "file_menu") },
    { name: "sidebar", html: renderSidebar(base) },
    { name: "topbar", html: renderTopbar(base, local) },
    { name: "run-status", html: renderRunStatusStrip(base) },
    { name: "mcp-run-status", html: renderRunStatusStrip({ ...base, task_activity_state: "idle", mcp_activity: {
      running: 1, waiting: 0, awaiting_approval: 0, cancelling: 0, unavailable: false,
    } }) },
    { name: "thread", html: renderThreadContent(base, local) },
    { name: "pending-turn-input", html: renderPendingTurnInputs(base.pending_turn_inputs) },
    { name: "composer", html: renderComposer(base, local) },
    { name: "plan", html: renderPlanProjection(base) },
    { name: "artifact-output", html: renderArtifactPane(base, local) },
    { name: "permission", html: renderConfirmation(base) },
    { name: "receiver-permission", html: renderConfirmation({ ...base, confirmation: { ...base.confirmation!, remote: {
      job_id: "job-b", profile_id: "profile-b", session_id: "session-b", requester_label: "WinA", target_label: "Temp",
    } } }) },
  ];

  for (const overlay of [
    "provider",
    "config",
    "hub",
    "mcp_history",
    "workspace",
    "prompt_review",
    "command_palette",
    "shortcuts",
    "about",
    "file_menu",
    "edit_menu",
    "view_menu",
    "help_menu",
  ]) {
    surfaces.push({
      name: `overlay-${overlay}`,
      html: renderOverlay(representativeState({ overlay }), local),
    });
  }

  const setup = representativeState({
    overlay: "config",
    startup: {
      ...base.startup,
      action_overlay: "config",
      initial_setup_required: true,
    },
  });
  surfaces.push({ name: "overlay-initial-setup", html: renderOverlay(setup, local) });

  const initialSetupState = representativeState({
    confirmation_visible: false,
    overlay: "initial_setup",
    startup: {
      ...base.startup,
      action_overlay: "initial_setup",
      initial_setup_required: true,
      initial_setup_reason: "config_missing",
      global_config_path: "C:/config/config.toml",
      setup_target: {
        workspacePath: "C:/workspace",
        globalConfigPath: "C:/config/config.toml",
        setupGeneration: "3",
      },
    },
  });
  surfaces.push({
    name: "initial-setup-start",
    html: renderDesktopMarkup(
      createDesktopRenderModel(initialSetupState, defaultRenderLocal()),
      { backgroundInert: false, taskActivityDelay: "0ms" },
    ),
  });
  surfaces.push({
    name: "initial-setup-finish",
    html: renderDesktopMarkup(
      createDesktopRenderModel(
        initialSetupState,
        defaultRenderLocal({ initialSetup: { step: "finish" } }),
      ),
      { backgroundInert: false, taskActivityDelay: "0ms" },
    ),
  });
  surfaces.push({
    name: "initial-setup-tools",
    html: renderDesktopMarkup(
      createDesktopRenderModel(
        initialSetupState,
        defaultRenderLocal({ initialSetup: { step: "tools" } }),
      ),
      { backgroundInert: false, taskActivityDelay: "0ms" },
    ),
  });

  const sessionDraft = {
    baseUrl: "http://127.0.0.1:1234/v1",
    model: "model-a",
    providerProfile: "openai_compatible" as const,
    apiKeyEnv: "",
    accessMode: "default" as const,
    contextWindow: "131072",
  };
  const sessionTarget = {
    workspacePath: "C:/workspace",
    rootSessionId: SESSION_IDLE,
    settingsRevision: "4",
    configGeneration: "9",
    runtimeOwnerToken: "tree:5",
  };
  const sessionSettingsState = representativeState({
    confirmation_visible: false,
    overlay: "session_settings",
    session_settings: {
      available: true,
      base_url: sessionDraft.baseUrl,
      model: sessionDraft.model,
      provider_profile: sessionDraft.providerProfile,
      api_key_env: sessionDraft.apiKeyEnv,
      access_mode: sessionDraft.accessMode,
      context_window: sessionDraft.contextWindow,
      max_output_tokens: "8192",
      context_window_inherited: true,
      max_output_tokens_inherited: true,
      provider_mutation_enabled: true,
      access_mutation_enabled: true,
      unavailable_reason: "",
      target: sessionTarget,
    },
  });
  const sessionSettingsLocal = defaultRenderLocal({
    sessionSettings: {
      draft: sessionDraft,
      dirty: false,
      validation: validateSessionSettingsDraft(sessionDraft),
      mutationPending: false,
      availability: {
        enabled: false,
        providerChanged: false,
        accessChanged: false,
        reason: "未適用の変更はありません。",
      },
    },
  });
  surfaces.push({
    name: "topbar-session-settings",
    html: renderTopbar(sessionSettingsState, sessionSettingsLocal),
  });
  surfaces.push({
    name: "overlay-session_settings",
    html: renderOverlay(sessionSettingsState, sessionSettingsLocal),
  });

  for (const kind of ["session", "archive_session", "rollback_session"] as const) {
    const confirmation: LocalConfirmation = {
      kind,
      index: 0,
      title: "Idle session",
      detail: SESSION_IDLE,
      expectedTarget: {
        workspacePath: "C:/workspace",
        ownerProjectId: PROJECT_ID,
        ownerSessionId: SESSION_IDLE,
        rowId: SESSION_IDLE,
      },
    };
    surfaces.push({ name: `local-confirm-${kind}`, html: renderLocalConfirmation(confirmation) });
  }
  surfaces.push({
    name: "local-confirm-settings-close",
    html: renderLocalConfirmation({
      kind: "settings_close",
      expectedTarget: base.config_target,
    }),
  });
  surfaces.push({
    name: "local-confirm-session-settings-close",
    html: renderLocalConfirmation({
      kind: "session_settings_close",
      expectedTarget: sessionTarget,
    }),
  });

  const agentListLocal = defaultRenderLocal({ artifactPane: { mode: "agents" } });
  surfaces.push({ name: "artifact-agent-list", html: renderArtifactPane(base, agentListLocal) });

  const execution: AgentExecutionCacheEntry = {
    status: "ready",
    generation: 3,
    expectedTarget: {
      workspacePath: "C:/workspace",
      rootSessionId: SESSION_IDLE,
      agentPath: runningAgent.agent_path,
      childSessionId: runningAgent.session_id,
    },
    projection: {
      workspace_path: "C:/workspace",
      root_session_id: SESSION_IDLE,
      agent_path: runningAgent.agent_path,
      session_id: runningAgent.session_id,
      task_name: runningAgent.task_name,
      transcript_rows: base.transcript_rows,
      turn_page_offset: 80,
      turn_page_end: 160,
      turn_page_total: 160,
      turn_page_has_previous: true,
    },
    error: "",
  };
  const agentDetailLocal = defaultRenderLocal({
    artifactPane: {
      mode: "agents",
      selectedAgentPath: runningAgent.agent_path,
      selectedAgentExecution: execution,
    },
  });
  surfaces.push({
    name: "artifact-agent-detail",
    html: renderArtifactPane(base, agentDetailLocal),
  });

  const sideChatLocal = defaultRenderLocal({
    artifactPane: { mode: "side_chat" },
    sideChat: {
      deleteConfirmation: {
        ownerSessionId: SESSION_IDLE,
        chatId: SIDE_CHAT,
        expectedGeneration: "6",
      },
    },
  });
  surfaces.push({ name: "artifact-side-chat", html: renderArtifactPane(base, sideChatLocal) });
  surfaces.push({ name: "artifact-side-chat-first-direct", html: renderArtifactPane({
    ...base,
    side_chat: { ...base.side_chat, can_send: false, direct_provider_capture: {
      base_url: "http://direct.test:1234/v1", model: "direct-model",
      provider_profile: "openai_compatible", enabled: true,
      reason: "会話を保持し、最初のDirect設定を適用します。",
    } },
  }, defaultRenderLocal({ artifactPane: { mode: "side_chat" } })) });
  surfaces.push({
    name: "side-chat-delete-confirmation",
    html: renderSideChatDeleteConfirmation(base, sideChatLocal),
  });

  return surfaces;
}

function actionIds(html: string): string[] {
  const ids: string[] = [];
  const attribute = /\bdata-action=(?:"([^"]+)"|'([^']+)')/g;
  for (const match of html.matchAll(attribute)) {
    ids.push(match[1] ?? match[2] ?? "");
  }
  return ids;
}

function buttonActionIds(html: string): Array<string | null> {
  const actions: Array<string | null> = [];
  for (const match of html.matchAll(/<button\b([^>]*)>/gi)) {
    const attribute = /\bdata-action=(?:"([^"]*)"|'([^']*)')/i.exec(match[1] ?? "");
    actions.push(attribute ? (attribute[1] ?? attribute[2] ?? "").trim() : null);
  }
  return actions;
}

function attribute(tag: string, name: string): string | null {
  const match = new RegExp(`\\b${name}=(?:"([^"]*)"|'([^']*)')`, "i").exec(tag);
  return match?.[1] ?? match?.[2] ?? null;
}

function decodeHtmlAttribute(value: string): string {
  return value
    .replaceAll("&quot;", '"')
    .replaceAll("&#39;", "'")
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&amp;", "&");
}

function actionButtons(html: string): Array<{
  action: string;
  index: number;
  value: string;
  disabled: boolean;
}> {
  return Array.from(html.matchAll(/<button\b[^>]*>/gi)).flatMap(([tag]) => {
    const action = attribute(tag, "data-action");
    if (!action) return [];
    return [{
      action,
      index: Number(attribute(tag, "data-index") ?? "-1"),
      value: attribute(tag, "data-agent-path")
        ?? attribute(tag, "data-history-target")
        ?? attribute(tag, "data-provider-profile")
        ?? attribute(tag, "data-mode")
        ?? attribute(tag, "data-value")
        ?? "",
      disabled: /\sdisabled(?:\s|>)/i.test(tag),
    }];
  });
}

test("every rendered button has one non-empty GUI action owner", () => {
  const unwired: Array<{ surface: string; button: number }> = [];
  for (const surface of representativeSurfaces()) {
    buttonActionIds(surface.html).forEach((action, index) => {
      if (!action) unwired.push({ surface: surface.name, button: index + 1 });
    });
  }

  // Native window controls also use data-action so keyboard/pointer dispatch shares the same
  // exact-once owner. There are currently no intentional action-less button exceptions.
  assert.deepEqual(unwired, []);
});

test("composer exposes the exact frontend-owned run target for settlement checks", () => {
  const state = representativeState({
    workspace_path: 'C:/workspace/&"owner"',
    run_target: {
      ...representativeState().run_target,
      workspacePath: 'C:/workspace/&"owner"',
    },
  });
  const section = /<section\b[^>]*class="composer[^"]*"[^>]*>/i.exec(renderComposer(state))?.[0];
  assert.ok(section, "composer section must be rendered");
  const encoded = attribute(section, "data-run-target");
  assert.notEqual(encoded, null, "composer must expose its rendered run owner");
  assert.deepEqual(JSON.parse(decodeHtmlAttribute(encoded ?? "")), state.run_target);
});

test("initial setup is a six-step blocking shell and session settings exposes stable root-only locators", () => {
  const surfaces = new Map(representativeSurfaces().map((surface) => [surface.name, surface.html]));
  const setup = surfaces.get("initial-setup-start") ?? "";
  assert.match(setup, /data-surface="initial-setup" data-current-step="start"/);
  assert.equal(Array.from(setup.matchAll(/\bdata-step="(?:start|provider|model|permissions|tools|finish)"/g)).length, 6);
  assert.equal(
    Array.from(setup.matchAll(/<li data-step="(?:start|provider|model|permissions|tools|finish)"[^>]*aria-label="[1-6]\. [^"]+"/g)).length,
    6,
  );
  assert.doesNotMatch(setup, /<div class="shell"/);
  assert.doesNotMatch(setup, /class="modal-backdrop"/);
  assert.match(setup, /data-action="import-config-toml"(?![^>]*\sdisabled(?:\s|>))[^>]*>/);
  assert.match(setup, /ディスクへの保存とruntime reloadはFinishまで行いません/);
  assert.match(surfaces.get("initial-setup-tools") ?? "", /data-action="check-docling-readiness"/);
  assert.match(surfaces.get("initial-setup-finish") ?? "", /data-action="finish-initial-setup"/);

  const session = surfaces.get("overlay-session_settings") ?? "";
  assert.match(session, /class="modal settings-modal session-settings-modal" data-modal="session-settings" data-surface="session-settings"/);
  assert.match(session, /data-session-scope="root-only">このセッションだけ/);
  assert.equal(Array.from(session.matchAll(/placeholder="Global Settingsを継承"/g)).length, 1);
  assert.match(session, /保存後の次のpermission decisionからrootと子Agentへ反映/);
  for (const field of [
    "base-url",
    "model",
    "access-mode",
    "context-window",
  ]) {
    assert.match(session, new RegExp(`data-session-setting="${field}"`));
  }
  assert.doesNotMatch(session, /class="modal-backdrop"[^>]*data-action/);

  const triggers = surfaces.get("topbar-session-settings") ?? "";
  assert.equal(Array.from(triggers.matchAll(/data-action="show-session-settings"/g)).length, 2);
  assert.match(triggers, /data-session-settings-trigger="model"/);
  assert.match(triggers, /data-session-settings-trigger="access"/);
  assert.match(
    surfaces.get("local-confirm-session-settings-close") ?? "",
    /data-modal="session-settings-close-confirmation"/,
  );
});

test("Initial Setup exposes both system prompts as multiline editors without losing draft line breaks", () => {
  const baseline = representativeState();
  const text = "日本語の追加指示\nsecond <line> & detail";
  const promptKeys = ["model.system_prompt", "side_chat.system_prompt"];
  const fields: ConfigFieldProjection[] = promptKeys.map(key => ({
    key, value: text, value_type: "string", required: false, min_value: null,
    max_value: null, options: [], sensitive: false, configured: true, env_override: null,
  }));
  const state = representativeState({
    confirmation_visible: false,
    overlay: "initial_setup",
    startup: { ...baseline.startup, action_overlay: "initial_setup", initial_setup_required: true },
    config_fields: [...baseline.config_fields.filter(field => !promptKeys.includes(field.key)), ...fields],
  });
  for (const [step, key] of [["model", "model.system_prompt"], ["finish", "side_chat.system_prompt"]] as const) {
    const html = renderDesktopMarkup(createDesktopRenderModel(state, defaultRenderLocal({ initialSetup: { step } })),
      { backgroundInert: false, taskActivityDelay: "0ms" });
    const escapedKey = key.replaceAll(".", "\\.");
    const controls = [...html.matchAll(new RegExp(`<textarea\\b[^>]*data-config-key="${escapedKey}"[^>]*>([\\s\\S]*?)<\\/textarea>`, "g"))];
    assert.equal(controls.length, 1, `${key} must be one multiline control in ${step}`);
    assert.equal(controls[0][1], "日本語の追加指示\nsecond &lt;line&gt; &amp; detail");
    assert.doesNotMatch(html, new RegExp(`<input\\b[^>]*data-config-key="${escapedKey}"`));
  }
});

test("initial setup owns a retained dismissible live error inside its blocking shell", () => {
  const base = representativeState();
  const state = representativeState({
    confirmation_visible: false,
    overlay: "initial_setup",
    startup: {
      ...base.startup,
      action_overlay: "initial_setup",
      initial_setup_required: true,
      initial_setup_reason: "config_missing",
      global_config_path: "C:/config/config.toml",
      setup_target: {
        workspacePath: "C:/workspace",
        globalConfigPath: "C:/config/config.toml",
        setupGeneration: "3",
      },
    },
  });
  const error = {
    title: "設定を読み込めませんでした",
    hint: "TOMLのschemaを確認して、もう一度Importしてください。",
    details: "invalid complete draft",
  };
  const errorHtml = renderDesktopMarkup(
    createDesktopRenderModel(state, { ...defaultRenderLocal(), recoverableError: error }),
    { backgroundInert: true, taskActivityDelay: "0ms" },
  );
  assert.equal(
    Array.from(errorHtml.matchAll(/data-settings-passive="initial-setup-recoverable-error"/g)).length,
    1,
  );
  assert.match(errorHtml, /id="initial-setup-recoverable-error"[^>]*role="alert"[^>]*aria-live="assertive"/);
  assert.match(errorHtml, /設定を読み込めませんでした/);
  assert.match(errorHtml, /TOMLのschemaを確認して、もう一度Importしてください/);
  assert.match(errorHtml, /invalid complete draft/);
  assert.match(errorHtml, /data-action="dismiss-ui-error"(?![^>]*\sdisabled(?:\s|>))[^>]*>/);
  assert.doesNotMatch(errorHtml, /<div class="shell"/);

  const cleanHtml = renderDesktopMarkup(
    createDesktopRenderModel(state, defaultRenderLocal()),
    { backgroundInert: true, taskActivityDelay: "0ms" },
  );
  assert.match(
    cleanHtml,
    /id="initial-setup-recoverable-error"[^>]*hidden aria-hidden="true"/,
  );
  assert.doesNotMatch(cleanHtml, /設定を読み込めませんでした/);
});

test("Session Settings keeps an Apply failure visible inside the exact dirty panel", () => {
  const target = {
    workspacePath: "C:/workspace",
    rootSessionId: SESSION_IDLE,
    settingsRevision: "4",
    configGeneration: "9",
    runtimeOwnerToken: "tree:5",
  };
  const draft = {
    baseUrl: "http://127.0.0.1:1234/v1",
    model: "locally-edited-model",
    providerProfile: "openai_compatible" as const,
    apiKeyEnv: "",
    accessMode: "default" as const,
    contextWindow: "",
  };
  const state = representativeState({
    confirmation_visible: false,
    overlay: "session_settings",
    session_settings: {
      available: true,
      base_url: "http://127.0.0.1:1234/v1",
      model: "model-a",
      provider_profile: "openai_compatible",
      api_key_env: "",
      access_mode: "default",
      context_window: "",
      max_output_tokens: "",
      context_window_inherited: true,
      max_output_tokens_inherited: true,
      provider_mutation_enabled: true,
      access_mutation_enabled: true,
      unavailable_reason: "",
      target,
    },
  });
  const local = {
    ...defaultRenderLocal({
      sessionSettings: {
        draft,
        dirty: true,
        validation: validateSessionSettingsDraft(draft),
        availability: {
          enabled: true,
          providerChanged: true,
          accessChanged: false,
          reason: "適用できます。",
        },
      },
    }),
    recoverableError: {
      title: "Session Settingsを適用できませんでした",
      hint: "入力内容を保持したまま、もう一度お試しください。",
      details: "storage transaction failed",
    },
  };
  const html = renderDesktopMarkup(
    createDesktopRenderModel(state, local),
    { backgroundInert: true, taskActivityDelay: "0ms" },
  );

  assert.match(html, /data-settings-passive="session-settings-recoverable-error"/);
  assert.match(html, /Session Settingsを適用できませんでした/);
  assert.match(html, /storage transaction failed/);
  assert.match(html, /data-session-setting="model"[^>]*value="locally-edited-model"/);
  assert.match(html, /data-action="apply-session-settings"(?![^>]*\sdisabled(?:\s|>))[^>]*>/);
  assert.match(html, /data-action="dismiss-ui-error"(?![^>]*\sdisabled(?:\s|>))[^>]*>/);
});

test("Wizard Advanced sections enumerate typed hidden fields and open at the repair owner", () => {
  const field = (
    key: string,
    value: string,
    valueType: ConfigFieldProjection["value_type"],
    required = true,
    options: string[] = [],
  ): ConfigFieldProjection => ({
    key,
    value,
    env_override: null,
    value_type: valueType,
    required,
    min_value: valueType === "integer" || valueType === "number" ? 0 : null,
    max_value: null,
    options,
  });
  const base = representativeState();
  const state = representativeState({
    confirmation_visible: false,
    overlay: "initial_setup",
    startup: {
      ...base.startup,
      action_overlay: "initial_setup",
      initial_setup_required: true,
      initial_setup_reason: "config_missing",
      global_config_path: "C:/config/config.toml",
      setup_target: {
        workspacePath: "C:/workspace",
        globalConfigPath: "C:/config/config.toml",
        setupGeneration: "3",
      },
    },
    config_fields: [
      field("model.base_url", "http://127.0.0.1:1234/v1", "string"),
      field("model.provider_profile", "openai_compatible", "enum", true, [
        "lm_studio",
        "openai_compatible",
        "openai_responses",
        "lm_studio_chat_completions",
      ]),
      field("model.api_key_env", "", "string", false),
      field("model.context_window", "65536", "integer"),
      field("model.max_output_tokens", "1024", "integer"),
      field("model.model", "model-a", "string"),
      field("model.supports_tools", "true", "boolean"),
      field("model.supports_reasoning", "true", "boolean"),
      field("model.supports_images", "false", "boolean"),
      field("model.parallel_tool_calls", "false", "boolean"),
      field("model.request_timeout_ms", "120000", "integer"),
      field("model.temperature", "0.2", "number"),
      field("model.top_p", "0.9", "number"),
      field("model.top_k", "40", "integer"),
      field("model.presence_penalty", "0.1", "number"),
      field("model.frequency_penalty", "0.2", "number"),
      field("model.seed", "42", "integer"),
      field("model.stop_sequences", "END", "string", false),
      field("model.extra_body_json", "{}", "json", false),
      field("permissions.access_mode", "default", "enum", true, ["default"]),
      field("docling.enabled", "true", "boolean"),
      field("docling.base_url", "http://127.0.0.1:5001", "string"),
      field("docling.timeout_ms", "30000", "integer"),
      field("docling.api_key_env", "", "string", false),
      field("docling.headers_json", "{}", "json", false),
      field("mcp.enabled", "false", "boolean"),
      field("mcp.servers_json", "", "json", false),
      field("shell.hide_windows", "true", "boolean"),
      field("inspection.default_max_depth", "", "integer"),
    ],
  });
  const renderStep = (step: "model" | "tools" | "finish") => renderDesktopMarkup(
    createDesktopRenderModel(state, defaultRenderLocal({ initialSetup: { step } })),
    { backgroundInert: true, taskActivityDelay: "0ms" },
  );
  const model = renderStep("model");
  const tools = renderStep("tools");
  const finish = renderStep("finish");

  assert.match(model, /data-config-key="model\.request_timeout_ms"/);
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
    assert.doesNotMatch(model, new RegExp(`data-config-key="${key.replace(".", "\\.")}"`));
  }
  for (const key of ["model.supports_tools", "model.supports_images", "model.parallel_tool_calls"]) {
    assert.match(model, new RegExp(`data-config-key="${key.replace(".", "\\.")}"`));
  }
  assert.match(model, /sampling \/ thinking \/ 出力量はホスティング側の設定をそのまま使用します。/);
  assert.match(tools, /data-config-key="docling\.headers_json"/);
  assert.match(
    finish,
    /<details class="initial-setup-advanced" data-details-key="initial-setup-finish-advanced" open>/,
  );
  assert.match(finish, /inspection\.default_max_depth: 値を入力してください/);
  assert.match(finish, /data-config-key="inspection\.default_max_depth"[^>]*aria-invalid="true"/);
  assert.match(finish, /data-config-key="shell\.hide_windows"[^>]*type="checkbox"/);
  assert.doesNotMatch(`${model}${tools}${finish}`, /settings-raw-value/);
  assert.doesNotMatch(
    `${model}${tools}${finish}`,
    /aria-describedby="[^"]*(?:main-provider-settings-help|settings-model-help|settings-permissions-help|settings-tools-help|settings-files-help)/,
  );
});

test("a stale session-settings owner disables editors while keeping Discard and Close available", () => {
  const target = {
    workspacePath: "C:/workspace",
    rootSessionId: SESSION_IDLE,
    settingsRevision: "8",
    configGeneration: "10",
    runtimeOwnerToken: "tree:6",
  };
  const draft = {
    baseUrl: "http://127.0.0.1:1234/v1",
    model: "locally-edited-model",
    providerProfile: "openai_compatible" as const,
    apiKeyEnv: "",
    accessMode: "default" as const,
    contextWindow: "131072",
  };
  const state = representativeState({
    confirmation_visible: false,
    overlay: "session_settings",
    session_settings: {
      available: true,
      base_url: "http://127.0.0.1:1234/v1",
      model: "canonical-model",
      provider_profile: "openai_compatible",
      api_key_env: "",
      access_mode: "default",
      context_window: "131072",
      max_output_tokens: "8192",
      context_window_inherited: true,
      max_output_tokens_inherited: true,
      provider_mutation_enabled: true,
      access_mutation_enabled: true,
      unavailable_reason: "",
      target,
    },
  });
  const local = defaultRenderLocal({
    sessionSettings: {
      draft,
      dirty: true,
      validation: validateSessionSettingsDraft(draft),
      availability: {
        enabled: false,
        staleTarget: true,
        providerChanged: false,
        accessChanged: false,
        reason: "保存済みSession Settingsが別の操作で更新されました。",
      },
    },
  });
  const html = renderDesktopMarkup(
    createDesktopRenderModel(state, local),
    { backgroundInert: true, taskActivityDelay: "0ms" },
  );
  for (const field of [
    "base-url",
    "provider-profile",
    "api-key-env",
    "model",
    "access-mode",
    "context-window",
  ]) {
    assert.match(html, new RegExp(`data-session-setting="${field}"[^>]*disabled`));
  }
  assert.match(html, /data-action="apply-session-settings"[^>]*disabled/);
  assert.match(html, /data-action="discard-session-settings"(?![^>]*\sdisabled(?:\s|>))[^>]*>/);
  assert.match(html, /data-action="close-overlay"(?![^>]*\sdisabled(?:\s|>))[^>]*>/);
});

test("an unavailable Session Settings underlay becomes inert below its dirty-close confirmation", () => {
  const owner = {
    workspacePath: "C:/workspace",
    rootSessionId: SESSION_IDLE,
    settingsRevision: "7",
    configGeneration: "9",
    runtimeOwnerToken: "tree:5",
  };
  const draft = {
    baseUrl: "http://127.0.0.1:1234/v1",
    model: "locally-edited-model",
    providerProfile: "openai_compatible" as const,
    apiKeyEnv: "",
    accessMode: "default" as const,
    contextWindow: "131072",
  };
  const state = representativeState({
    confirmation_visible: false,
    overlay: "session_settings",
  });
  const local = defaultRenderLocal({
    modal: {
      localConfirmation: {
        kind: "session_settings_close",
        expectedTarget: owner,
      },
    },
    sessionSettings: {
      draft,
      dirty: true,
      validation: validateSessionSettingsDraft(draft),
      availability: {
        enabled: false,
        staleTarget: true,
        providerChanged: false,
        accessChanged: false,
        reason: "root sessionを選択すると変更できます。",
      },
    },
  });

  const html = renderDesktopMarkup(
    createDesktopRenderModel(state, local),
    { backgroundInert: true, taskActivityDelay: "0ms" },
  );
  assert.match(
    html,
    /<div class="modal-backdrop" inert aria-hidden="true">\s*<section class="modal settings-modal session-settings-modal"/,
  );
  assert.match(html, /data-modal="session-settings-close-confirmation"/);
});

test("each primary GUI surface retains its required action routes", () => {
  const requiredActionsBySurface = {
    titlebar: [
      "show-file-menu",
      "show-edit-menu",
      "show-view-menu",
      "show-help-menu",
      "minimize-window",
      "toggle-maximize-window",
      "close-window",
    ],
    sidebar: [
      "show-shortcuts",
      "refresh",
      "create-project-from-picker",
      "project",
      "new-project-session",
      "delete-project",
      "toggle-session-archived-search",
      "session",
      "archive-session",
      "rollback-session",
      "delete-session",
      "rejoin-session",
      "interrupt-session",
      "unarchive-session",
      "new-chat",
      "chat-session",
      "delete-chat-session",
      "show-config",
    ],
    topbar: [
      "open-workspace-folder",
      "show-provider",
      "toggle-access",
      "export-transcript",
      "toggle-artifact-pane",
    ],
    composer: [
      "toggle-attachment-tray",
      "show-command-palette",
      "enhance-prompt",
      "send",
      "open-workspace-folder",
      "set-image",
      "browse-image",
      "clear-images",
      "remove-image",
    ],
    "run-status": ["cancel-run"],
    "mcp-run-status": ["show-mcp-execution-history"],
    permission: ["abort-permission", "cancel-run", "approve-permission"],
    "local-confirm-session": ["cancel-local-confirm", "confirm-local-delete"],
    "local-confirm-archive_session": ["cancel-local-confirm", "confirm-local-archive-state"],
    "local-confirm-rollback_session": ["cancel-local-confirm", "confirm-local-rollback"],
    "local-confirm-settings-close": ["cancel-local-confirm", "confirm-settings-discard-close"],
    "local-confirm-session-settings-close": [
      "cancel-local-confirm",
      "confirm-session-settings-discard-close",
    ],
    "initial-setup-start": [
      "initial-setup-next",
      "initial-setup-back",
      "import-config-toml",
    ],
    "initial-setup-finish": ["initial-setup-back", "finish-initial-setup"],
    "topbar-session-settings": ["show-session-settings"],
    "overlay-session_settings": [
      "close-overlay",
      "discard-session-settings",
      "apply-session-settings",
      "open-preferences-from-session-settings",
    ],
    "overlay-provider": [
      "close-overlay",
      "load-provider-models",
      "apply-provider-session",
      "save-provider-global",
      "select-provider-model",
    ],
    "overlay-config": [
      "discard-config-draft",
      "apply-session-config",
      "save-global-config",
      "close-overlay",
      "open-global-config-folder",
      "open-user-data-folder",
      "load-provider-models",
      "load-side-chat-models",
      "show-session-settings",
    ],
    "overlay-hub": ["close-overlay", "hub-connect", "hub-refresh", "hub-disconnect", "hub-save-main", "hub-save-side"],
    "overlay-mcp_history": ["close-overlay", "mcp-history-direction", "mcp-history-refresh", "mcp-history-export", "mcp-history-stop"],
    "overlay-workspace": [
      "close-overlay",
      "switch-workspace",
      "browse-workspace",
      "open-typed-path",
      "open-workspace-folder",
    ],
    "overlay-prompt_review": [
      "close-overlay",
      "cancel-review",
      "send-review-raw",
      "send-review-enhanced",
    ],
    "artifact-output": [
      "toggle-artifact-pane",
      "open-artifact-folder",
      "artifact",
      "show-agent-pane",
      "show-side-chat-pane",
    ],
    "artifact-side-chat": [
      "show-output-pane",
      "request-delete-side-chat",
      "toggle-artifact-pane",
      "cancel-side-chat",
      "send-side-chat",
    ],
    "side-chat-delete-confirmation": [
      "cancel-delete-side-chat",
      "confirm-delete-side-chat",
    ],
  } as const;
  const surfaces = new Map(
    representativeSurfaces().map((surface) => [surface.name, new Set(actionIds(surface.html))]),
  );
  const missing: Array<{ surface: string; action: string }> = [];

  for (const [surface, requiredActions] of Object.entries(requiredActionsBySurface)) {
    const rendered = surfaces.get(surface);
    assert.ok(rendered, `representative surface is missing: ${surface}`);
    for (const action of requiredActions) {
      if (!rendered.has(action)) missing.push({ surface, action });
    }
  }
  assert.deepEqual(missing, []);
});

test("the sidebar separates Hub preparation from ordinary Settings", () => {
  const html = renderSidebar(representativeState());

  assert.match(html, /class="rail-item" data-action="show-hub"/);
  assert.match(html, />moyAI Hub<\/span>/);
  assert.match(html, /class="rail-item" data-action="show-mcp-history"/);
  assert.doesNotMatch(html, /data-action="show-mcp-publish"/);
  assert.match(html, /class="settings" data-action="show-config"/);
  assert.doesNotMatch(html, /class="rail-item" data-action="show-provider"/);
});

test("Settings shows configured sensitive fields without exposing their values", () => {
  const base = representativeState();
  const state = representativeState({
    confirmation_visible: false,
    overlay: "config",
    config_fields: [
      ...base.config_fields,
      {
        key: "model.extra_headers_json",
        value: "",
        sensitive: true,
        configured: true,
        env_override: null,
        value_type: "json",
        required: false,
        min_value: null,
        max_value: null,
        options: [],
      },
    ],
  });

  const html = renderOverlay(state, defaultRenderLocal());

  assert.match(html, /data-config-key="model\.extra_headers_json"[^>]*data-sensitive-config="true"/);
  assert.match(html, /placeholder="設定済み（値は非表示）"/);
  assert.match(html, />設定済み・値は非表示</);
  assert.doesNotMatch(html, /model-header-super-secret/);
});

test("composer shows canonical session usage separately from the context meter", () => {
  const html = renderComposer(representativeState(), defaultRenderLocal());

  assert.match(html, /class="token-meter low"/);
  assert.match(html, /class="session-usage partial"/);
  assert.match(html, /セッション累計: 1\.2k token（一部）/);
  assert.match(html, /canonical terminal telemetry: 2 \/ 3 turn計測/);
});

test("Initial Setup Finish and Session Settings Apply render as text-sized primary actions", () => {
  const surfaces = new Map(
    representativeSurfaces().map((surface) => [surface.name, surface.html]),
  );
  const finish = surfaces.get("initial-setup-finish");
  const sessionSettings = surfaces.get("overlay-session_settings");

  assert.ok(finish);
  assert.match(
    finish,
    /<button id="initial-setup-primary" class="send wide-send" data-action="finish-initial-setup"[^>]*>設定を保存してmoyAIを開く<\/button>/,
  );
  assert.ok(sessionSettings);
  assert.match(
    sessionSettings,
    /<button class="send wide-send" data-action="apply-session-settings"[^>]*>このセッションに適用<\/button>/,
  );
});

test("workspace validation feedback is readable inside the active dialog", () => {
  for (const message of ["workspace path is empty", "workspace path is not accessible: <missing>", "workspace path is not a directory"]) {
    const state = representativeState({ overlay: "workspace", confirmation_visible: false, status_message: message });
    const html = renderOverlay(state, defaultRenderLocal());
    const dialog = html.match(/<section[^>]*aria-labelledby="workspace-dialog-title"[\s\S]*?<\/section>/)?.[0];
    assert.ok(dialog);
    const feedback = dialog.match(/<pre[^>]*id="workspace-feedback"[^>]*role="status"[^>]*>([\s\S]*?)<\/pre>/)?.[1];
    assert.equal(feedback, message.replaceAll("<", "&lt;").replaceAll(">", "&gt;"));
  }
});

test("every action rendered by public GUI surfaces resolves through the single registry", () => {
  const unresolved: Array<{ surface: string; action: string }> = [];
  for (const surface of representativeSurfaces()) {
    for (const action of actionIds(surface.html)) {
      if (!actionById(action)) unresolved.push({ surface: surface.name, action });
    }
  }
  assert.deepEqual(unresolved, []);
});

test("every registry action is render-reachable or has one explicit host contract", () => {
  const hostOwnedExceptions = {
    "load-next-turn-page": "the inline history UI intentionally exposes prepend-only paging and no page-replacement control",
  } as const;
  const rendered = new Set(representativeSurfaces().flatMap((surface) => actionIds(surface.html)));
  const notRendered = ACTIONS
    .map((action) => action.id)
    .filter((id) => !rendered.has(id))
    .sort();

  assert.deepEqual(notRendered, Object.keys(hostOwnedExceptions).sort());
  assert.equal(rendered.size, ACTIONS.length - Object.keys(hostOwnedExceptions).length);
  for (const [id, reason] of Object.entries(hostOwnedExceptions)) {
    assert.notEqual(actionById(id), undefined, id);
    assert.ok(reason.length > 20, id);
  }
});

test("production render button availability matches the shared action resolver", () => {
  const local = defaultRenderLocal();
  const base = representativeState({ confirmation_visible: false, overlay: "none" });
  const states = [
    base,
    ...[
      "provider",
      "config",
      "hub",
      "mcp_history",
      "workspace",
      "prompt_review",
      "command_palette",
      "shortcuts",
      "about",
      "file_menu",
      "edit_menu",
      "view_menu",
      "help_menu",
    ].map((overlay) => representativeState({ confirmation_visible: false, overlay })),
  ];
  let checked = 0;
  for (const state of states) {
    const model = createDesktopRenderModel(state, local);
    const html = renderDesktopMarkup(model, { backgroundInert: false, taskActivityDelay: "0ms" });
    for (const button of actionButtons(html)) {
      assert.equal(
        button.disabled,
        !actionEnabledById(button.action, model, { index: button.index, value: button.value }),
        `${state.overlay}:${button.action}`,
      );
      checked += 1;
    }
  }
  assert.ok(checked > 100, `expected broad render coverage, checked ${checked} action buttons`);
});

test("dirty Settings close layers an inert retained dialog below the alertdialog", () => {
  const state = representativeState({
    overlay: "config",
    confirmation_visible: false,
    confirmation_id: null,
    confirmation: null,
    config_draft: {
      dirty: true,
      edit_enabled: true,
      discard_enabled: true,
      commit_enabled: true,
      external_owner_mutation_open: false,
      access_mode_mutation_enabled: false,
    },
  });
  const local: DesktopRenderLocalPresentation = {
    ...defaultRenderLocal(),
    modal: {
      ...DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION.modal,
      localConfirmation: {
        kind: "settings_close",
        expectedTarget: state.config_target,
      },
    },
  };
  const html = renderDesktopMarkup(
    createDesktopRenderModel(state, local),
    { backgroundInert: false, taskActivityDelay: "0ms" },
  );

  const settingsIndex = html.indexOf('class="modal settings-modal');
  const confirmationIndex = html.indexOf(
    'class="modal confirmation settings-close-confirmation" role="alertdialog"',
  );
  assert.ok(settingsIndex >= 0, "the underlying Settings dialog remains rendered");
  assert.ok(confirmationIndex > settingsIndex, "the close alertdialog is layered after Settings");

  const backdropTags = Array.from(
    html.matchAll(/<div class="modal-backdrop"[^>]*>/g),
    (match) => match[0],
  );
  assert.equal(backdropTags.length, 2);
  assert.match(backdropTags[0], /\binert\b/);
  assert.match(backdropTags[0], /aria-hidden="true"/);
  for (const backdrop of backdropTags) assert.doesNotMatch(backdrop, /data-action=/);
});

test("Main and Side Stop are named visibly and an idle Side has no misleading Stop control", () => {
  const running = representativeState();
  assert.match(renderRunStatusStrip(running), />Mainを停止<\/span>/);
  const local = defaultRenderLocal({ artifactPane: { mode: "side_chat" } });
  assert.match(renderArtifactPane(running, local), />Sideを停止<\/span>/);
  const completed = representativeState({
    status_code: "user_stopped",
    status_message: "run stopped by user",
    side_chat: { ...running.side_chat, status: "completed", can_cancel: false, can_send: true },
  });
  assert.doesNotMatch(renderArtifactPane(completed, local), /data-action="cancel-side-chat"|停止不可/);
  assert.match(renderTopbar(completed), /実行を停止しました/);
  assert.doesNotMatch(renderTopbar(completed), /run stopped by user/);
});

test("production MCP history keeps both direction buttons and a saved task selectable", () => {
  const state = representativeState({ confirmation_visible: false, overlay: "mcp_history" });
  const local = defaultRenderLocal();
  local.mcpHistory = { ...local.mcpHistory, loaded: true, rows: [{ id: "history-row-a", direction: "instruction",
    created_at_ms: 1_700_000_000_000, updated_at_ms: null, title: "WinBのCPU使用率", peer_label: "WinB", target_label: "temp",
    state: "completed", stop_status: "none", session_id: "session-a", job_id: "job-b", profile_id: "profile-b",
    root_task_id: "session-a", device_path: ["WinA", "WinB"], can_stop: false, state_source: "last_observed", result_received: true }] };
  const html = renderDesktopMarkup(createDesktopRenderModel(state, local), { backgroundInert: false, taskActivityDelay: "0ms" });
  for (const id of ["mcp-history-instruction", "mcp-history-execution", "mcp-history-row-history-row-a"]) {
    const tag = html.match(new RegExp(`<button\\b[^>]*id="${id}"[^>]*>`))?.[0];
    assert.ok(tag, `${id} is visible`);
    assert.doesNotMatch(tag, /\sdisabled(?:\s|>)/, `${id} must remain usable after the final action-availability pass`);
    assert.match(tag, /aria-disabled="false"/);
  }
});

test("activity remains visible while Stop is neither rendered nor activatable without its exact Rust target", () => {
  const targetless = representativeState({ can_cancel_run: true, stop_target: null });

  const strip = renderRunStatusStrip(targetless);
  assert.match(strip, /data-task-activity="attention"/);
  assert.match(strip, /<strong>確認待ち<\/strong>/);
  assert.doesNotMatch(strip, /data-action="cancel-run"/);
  assert.doesNotMatch(renderConfirmation(targetless), /data-action="cancel-run"/);
  assert.equal(
    actionById("cancel-run")?.enabled(
      createDesktopRenderModel(targetless, defaultRenderLocal()),
      { index: -1, value: "" },
    ),
    false,
  );
});

test("quick-chat activity uses the selected chat identity and keeps background rows compact", () => {
  const selectedQuickChat: SessionRow = {
    ...activeSession,
    session_id: QUICK_SESSION,
    title: "Selected quick chat",
    short_id: "selected-quick",
    label: "Selected quick chat [実行中] selected-quick",
  };
  const backgroundQuickChat: SessionRow = {
    ...activeSession,
    session_id: SESSION_ACTIVE,
    title: "Background quick chat",
    short_id: "background-quick",
    label: "Background quick chat [実行中] background-quick",
  };
  const html = renderSidebar(representativeState({
    selected_project_index: -1,
    selected_session_index: 0,
    session_rows: [idleSession],
    chat_session_rows: [selectedQuickChat, backgroundQuickChat],
    task_activity_state: "finalizing",
  }));
  const selectedRow = /<button class="nav-row" data-action="chat-session" data-index="0"[\s\S]*?<\/button>/.exec(html)?.[0];
  const backgroundRow = /<button class="nav-row" data-action="chat-session" data-index="1"[\s\S]*?<\/button>/.exec(html)?.[0];

  assert.ok(selectedRow);
  assert.match(selectedRow, /aria-current="page"/);
  assert.match(selectedRow, /class="task-activity-indicator" data-task-activity="finalizing" aria-hidden="true"/);
  assert.doesNotMatch(selectedRow, /class="task-activity-indicator small"/);
  assert.match(selectedRow, /<small>最終反映中 · turn 4<\/small>/);
  assert.doesNotMatch(selectedRow, /\[実行中\]/);
  assert.ok(backgroundRow);
  assert.doesNotMatch(backgroundRow, /aria-current="page"/);
  assert.match(backgroundRow, /class="task-activity-indicator small" data-task-activity="running" aria-hidden="true"/);
  assert.match(backgroundRow, /<small>実行中 · turn 4<\/small>/);
  assert.doesNotMatch(backgroundRow, /\[実行中\]/);
  assert.match(html, /data-task-activity-row="finalizing"/);
  assert.match(html, /data-task-activity-row="running"/);
  assert.doesNotMatch(html, /class="task-activity-indicator[^>]*aria-label=/);
  assert.match(html, /<span class="section-label">チャット<\/span>/);
  assert.equal(Array.from(html.matchAll(/class="task-activity-indicator/g)).length, 2);
});

test("a selected active row keeps the Rust state and removes its duplicate durable status", () => {
  const pendingInputSession: SessionRow = {
    ...activeSession,
    pending_user_input_requests: 1,
    label: "Active session [実行中] active",
  };
  const html = renderSidebar(representativeState({
    selected_project_index: 0,
    selected_session_index: 0,
    session_rows: [pendingInputSession],
    task_activity_state: "running",
  }));

  assert.match(html, /data-task-activity-row="running"/);
  assert.match(
    html,
    /class="task-activity-indicator" data-task-activity="running" aria-hidden="true"/,
  );
  assert.match(html, /<span>Active session active<\/span>/);
  assert.match(html, /<small>実行中 · turn 4<\/small>/);
  assert.doesNotMatch(html, /\[実行中\]/);
  assert.doesNotMatch(html, /class="task-activity-indicator[^>]*aria-label=/);
});

test("Desktop markup is pure across A to B to A local-presentation renders", () => {
  const state = representativeState({
    confirmation_visible: false,
    confirmation_id: null,
    confirmation: null,
    overlay: "none",
  });
  const localA = defaultRenderLocal({
    artifactPane: { collapsed: true, mode: "output" },
    attachmentTrayOpen: true,
  });
  const localB = defaultRenderLocal({
    artifactPane: { collapsed: false, mode: "side_chat" },
    attachmentTrayOpen: false,
    sideChat: { draft: "B-only draft marker" },
  });
  const options = { backgroundInert: false, taskActivityDelay: "0ms" };

  const firstA = renderDesktopMarkup(createDesktopRenderModel(state, localA), options);
  const onlyB = renderDesktopMarkup(createDesktopRenderModel(state, localB), options);
  const secondA = renderDesktopMarkup(createDesktopRenderModel(state, localA), options);

  assert.equal(secondA, firstA);
  assert.notEqual(onlyB, firstA);
  assert.match(firstA, /class="app-frame artifact-collapsed /);
  assert.doesNotMatch(firstA, /B-only draft marker/);
  assert.match(onlyB, /B-only draft marker/);
});

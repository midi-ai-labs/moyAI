import assert from "node:assert/strict";
import test from "node:test";

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
import type {
  AgentActivityRow,
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
      license_identifier: "MIT",
      copyright_notice: "Copyright test",
    },
    side_chat: {
      configured: true,
      deleting: false,
      chat_id: SIDE_CHAT,
      owner_session_id: SESSION_IDLE,
      model: "side-model",
      base_url: "http://127.0.0.1:1234/v1",
      status: "running",
      phase: "generating",
      last_error: "",
      generation: "6",
      draft_text: "Side question",
      draft_revision: "3",
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
    provider_metadata_mode: "openai_compatible_only",
    provider_effective_base_url: "http://127.0.0.1:1234/v1",
    provider_effective_metadata_mode: "openai_compatible_only",
    provider_effective_context_window: "131072",
    provider_effective_max_output_tokens: "8192",
    provider_effective_model_id: "model-a",
    provider_catalog_base_url: "http://127.0.0.1:1234",
    provider_catalog_metadata_mode: "openai_compatible_only",
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
    config_target: {
      workspacePath: "C:/workspace",
      sessionId: SESSION_IDLE,
      configGeneration: "9",
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
    sideChat: {
      ...DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION.sideChat,
      draft: "Side question",
      setupBaseUrl: "http://127.0.0.1:1234/v1",
      setupModel: "side-model",
      catalog: {
        status: "ready",
        source: "side",
        ownerSessionId: SESSION_IDLE,
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
  const surfaces: RenderedSurface[] = [
    { name: "startup", html: renderStartupSplash(base, 1_000, 500) },
    { name: "titlebar", html: renderTitlebar(true, false, "file_menu") },
    { name: "sidebar", html: renderSidebar(base) },
    { name: "topbar", html: renderTopbar(base, local) },
    { name: "run-status", html: renderRunStatusStrip(base) },
    { name: "thread", html: renderThreadContent(base, local) },
    { name: "pending-turn-input", html: renderPendingTurnInputs(base.pending_turn_inputs) },
    { name: "composer", html: renderComposer(base, local) },
    { name: "plan", html: renderPlanProjection(base) },
    { name: "artifact-output", html: renderArtifactPane(base, local) },
    { name: "permission", html: renderConfirmation(base) },
  ];

  for (const overlay of [
    "provider",
    "config",
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
        ?? attribute(tag, "data-mode")
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
      "show-provider",
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
    permission: ["abort-permission", "cancel-run", "approve-permission"],
    "local-confirm-session": ["cancel-local-confirm", "confirm-local-delete"],
    "local-confirm-archive_session": ["cancel-local-confirm", "confirm-local-archive-state"],
    "local-confirm-rollback_session": ["cancel-local-confirm", "confirm-local-rollback"],
    "overlay-provider": [
      "close-overlay",
      "set-provider-mode",
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
      "show-provider",
      "load-side-chat-models",
      "configure-side-chat",
    ],
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
    "dismiss-ui-error": "the transient recoverable-error host is outside the public render-function surface",
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
    label: "Selected quick chat",
  };
  const backgroundQuickChat: SessionRow = {
    ...activeSession,
    session_id: SESSION_ACTIVE,
    title: "Background quick chat",
    short_id: "background-quick",
    label: "Background quick chat",
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
  assert.match(selectedRow, /class="task-activity-indicator" data-task-activity="finalizing"/);
  assert.doesNotMatch(selectedRow, /class="task-activity-indicator small"/);
  assert.ok(backgroundRow);
  assert.doesNotMatch(backgroundRow, /aria-current="page"/);
  assert.match(backgroundRow, /class="task-activity-indicator small" data-task-activity="running"/);
  assert.match(html, /<span class="section-label">チャット<\/span>/);
  assert.equal(Array.from(html.matchAll(/class="task-activity-indicator/g)).length, 2);
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

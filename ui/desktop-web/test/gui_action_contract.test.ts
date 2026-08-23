import assert from "node:assert/strict";
import test from "node:test";

import {
  ACTION_IDS,
  ACTIONS,
  ACTION_BY_ID,
  actionById,
  dispatchAction,
  type ActionContext,
} from "../src/actions.ts";
import {
  overlayDismissAction,
  shouldDispatchDelegatedKeyboardAction,
} from "../src/events.ts";
import { InteractionLifecycle, type InteractionEnd } from "../src/interaction_lifecycle.ts";
import {
  dispatchNewSessionMutation,
  type NewSessionMutationOwnerState,
} from "../src/new_session_mutation.ts";
import type { DesktopViewState } from "../src/types.ts";
import { createUiLocalState } from "../src/ui_state.ts";
import {
  initialSetupDoclingReadinessVisible,
  initialSetupImportedSourcePath,
} from "../src/initial_setup_auxiliary_state.ts";
import {
  createDesktopRenderModel,
  DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION,
} from "../src/render_projection.ts";
import { rootStopTarget, turnStopTarget } from "./stop_target_fixture.ts";

const SESSION_A = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const SESSION_B = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const QUICK_A = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const TURN_A = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
const TURN_B = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";

interface MutationCall {
  name: string;
  args?: Record<string, unknown>;
}

function state(overrides: Partial<DesktopViewState> = {}): DesktopViewState {
  return {
    workspace_path: "C:/workspace",
    project_rows: [{ project_id: "project-a", label: "Project A", path: "C:/workspace" }],
    selected_project_index: 0,
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
    chat_session_rows: [{
      session_id: QUICK_A,
      label: "Quick A",
      title: "Quick A",
      status: "idle",
      loaded_status: "idle",
      archived: false,
      pending_permission_requests: 0,
      pending_user_input_requests: 0,
      admission_revision: "4",
      short_id: QUICK_A,
    }],
    selected_session_index: 0,
    artifact_rows: [{ path: "C:/workspace/result.md", label: "result.md", kind: "file" }],
    selected_artifact_index: 0,
    attached_images: ["C:/workspace/image.png"],
    command_rows: [{ name: "case", label: "Case", path: "/case" }],
    navigation_admission_open: true,
    draft_prompt: "Run the task",
    review_draft_text: "Reviewed task",
    draft_target: {
      workspacePath: "C:/workspace",
      sessionId: SESSION_A,
      ownerGeneration: "3",
    },
    review_target: {
      workspacePath: "C:/workspace",
      sessionId: SESSION_A,
      ownerGeneration: "3",
      requestId: "41",
      expectedState: { kind: "idle", latestTurnId: TURN_A, admissionRevision: "4" },
    },
    can_submit: true,
    enhance_enabled: true,
    can_cancel_run: true,
    task_activity_state: "running",
    run_target: {
      workspacePath: "C:/workspace",
      sessionId: SESSION_A,
      runtimeOwnerToken: "tree:9",
      permissionConfirmationId: null,
      expectedState: { kind: "idle", latestTurnId: TURN_A, admissionRevision: "4" },
    },
    stop_target: turnStopTarget(),
    send_enhanced_enabled: true,
    send_raw_enabled: true,
    startup: {
      status: "ready",
      title: "Ready",
      message: "",
      detail: "",
      action_overlay: "none",
      initial_setup_required: false,
      checks: [],
    },
    overlay: "none",
    config_target: {
      workspacePath: "C:/workspace",
      sessionId: SESSION_A,
      configGeneration: "7",
    },
    config_draft: {
      dirty: false,
      edit_enabled: true,
      discard_enabled: false,
      commit_enabled: false,
      external_owner_mutation_open: true,
      access_mode_mutation_enabled: true,
    },
    provider_apply_enabled: false,
    provider_model_ids: [],
    docling_readiness: {
      status: "idle",
      endpoint: "",
      httpStatus: null,
      message: "Docling readiness has not been checked.",
    },
    config_fields: [],
    agent_activity_rows: [],
    ...overrides,
  } as DesktopViewState;
}

function context(
  current: DesktopViewState,
  calls: MutationCall[],
  rerender: () => void = () => {},
): ActionContext {
  const uiState = createUiLocalState();
  return {
    uiState,
    getProjection: () => current,
    getViewState: () => current,
    getRenderModel: () => createDesktopRenderModel(current, {
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
          current.overlay === "config"
          && uiState.localConfirmationDecisionPending
          && uiState.pendingLocalConfirmation === null
        ),
      doclingReadinessRequestPending: uiState.doclingReadinessTransaction.active !== null,
      initialSetup: {
        ...DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION.initialSetup,
        step: uiState.initialSetup.step,
        finishPending: uiState.initialSetup.activeFinish !== null,
        auxiliaryPendingKind: uiState.initialSetupAuxiliary.active?.kind ?? null,
      },
      modal: {
        localConfirmation: uiState.pendingLocalConfirmation,
        localDecisionPending: uiState.localConfirmationDecisionPending,
        localDecisionError: uiState.localConfirmationDecisionError,
        permissionDecision: uiState.permissionDecision,
      },
      recoverableError: uiState.recoverableError,
      windowMaximized: uiState.windowMaximized,
    }),
    mutate: async (name, args) => {
      calls.push({ name, args });
    },
    insertCommandFromPalette: async (state, index) => {
      const row = state.command_rows[index];
      if (!row) return;
      calls.push({
        name: "insert_command",
        args: {
          index,
          expectedTarget: {
            workspacePath: state.workspace_path,
            ownerProjectId: state.project_rows[state.selected_project_index]?.project_id ?? null,
            ownerSessionId: state.session_rows[state.selected_session_index]?.session_id ?? null,
            rowId: row.path,
          },
          expectedDraftTarget: state.draft_target,
        },
      });
    },
    invalidateCommandPaletteInsertion: () => {},
    rerender,
    prepareConfigSnapshot: () => [],
    prepareConfigMutation: () => [],
    recoverCommandConflict: () => false,
    reportError: () => {},
    acceptProjection: () => {},
    waitForInteractionIdle: async () => {},
    submitPermissionDecision: async () => {},
    submitRunStop: async (state) => {
      calls.push({
        name: "cancel_run",
        args: { expectedTarget: state.stop_target },
      });
    },
    setWindowMaximized: () => {},
    loadAgentExecution: async () => {},
    loadPreviousAgentExecutionPage: async () => {},
    loadSideChatModels: async () => ({ models: [] }),
    jumpToHistoryAnchor: () => {},
    desktopWindow: {
      hide: async () => {},
      minimize: async () => {},
      toggleMaximize: async () => {},
      startDragging: async () => {},
    },
  } as unknown as ActionContext;
}

async function dispatchGuiAction(
  action: string,
  index: number,
  value: string,
  _state: DesktopViewState,
  actionContext: ActionContext,
): Promise<boolean> {
  return dispatchAction(action, actionContext, { index, value });
}

const EXACT_DELIVERY_ACTION_IDS = [
    "toggle-attachment-tray",
    "dismiss-ui-error",
    "new-project-session",
    "project",
    "session",
    "chat-session",
    "cancel-local-confirm",
    "confirm-local-delete",
    "confirm-local-archive-state",
    "confirm-local-rollback",
    "artifact",
    "remove-image",
    "send-review-enhanced",
    "send-review-raw",
    "cancel-review",
    "show-file-menu",
    "show-edit-menu",
    "show-view-menu",
    "show-help-menu",
    "close-overlay",
    "check-docling-readiness",
    "import-config-toml",
    "insert-command",
  ] as const;

test("the single GUI action registry owns all 97 actions without duplicates", () => {
  const actionIds = ACTIONS.map((action) => action.id);

  assert.equal(actionIds.length, 97);
  assert.deepEqual(ACTION_IDS, actionIds);
  assert.equal(new Set(actionIds).size, actionIds.length);
  assert.equal(ACTION_BY_ID.size, actionIds.length);
  for (const id of actionIds) {
    assert.equal(actionById(id)?.id, id, id);
  }
  assert.equal(actionById("not-a-gui-action"), undefined);
});

test("delivery-sensitive GUI actions preserve their exact native boundaries", async () => {
  const rowOwner = {
    workspacePath: "C:/workspace",
    ownerProjectId: "project-a",
    ownerSessionId: SESSION_A,
  };
  const draftTarget = state().draft_target;
  const reviewTarget = state().review_target;
  const cases: Array<{
    id: string;
    expectedMutation?: MutationCall;
    state?: () => DesktopViewState;
    prepare?: (actionContext: ActionContext) => void;
    verify?: (actionContext: ActionContext, rerenders: number) => void;
  }> = [
    {
      id: "toggle-attachment-tray",
      verify: (actionContext, rerenders) => {
        assert.equal(actionContext.uiState.attachmentTrayOpen, true);
        assert.equal(rerenders, 1);
      },
    },
    {
      id: "dismiss-ui-error",
      prepare: (actionContext) => {
        actionContext.uiState.recoverableError = {
          title: "Provider error",
          hint: "Retry",
          details: "offline",
        };
      },
      verify: (actionContext, rerenders) => {
        assert.equal(actionContext.uiState.recoverableError, null);
        assert.equal(rerenders, 1);
      },
    },
    {
      id: "new-project-session",
      expectedMutation: {
        name: "new_project_session",
        args: { index: 0, expectedTarget: { ...rowOwner, rowId: "project-a" } },
      },
    },
    {
      id: "project",
      expectedMutation: {
        name: "select_project",
        args: { index: 0, expectedTarget: { ...rowOwner, rowId: "project-a" } },
      },
    },
    {
      id: "session",
      expectedMutation: {
        name: "select_session",
        args: { index: 0, expectedTarget: { ...rowOwner, rowId: SESSION_A } },
      },
    },
    {
      id: "chat-session",
      expectedMutation: {
        name: "select_chat_session",
        args: { index: 0, expectedTarget: { ...rowOwner, rowId: QUICK_A } },
      },
    },
    {
      id: "cancel-local-confirm",
      prepare: (actionContext) => {
        actionContext.uiState.pendingLocalConfirmation = {
          kind: "session",
          index: 0,
          title: "Session A",
          detail: SESSION_A,
          expectedTarget: { ...rowOwner, rowId: SESSION_A },
        };
      },
      verify: (actionContext, rerenders) => {
        assert.equal(actionContext.uiState.pendingLocalConfirmation, null);
        assert.equal(rerenders, 1);
      },
    },
    { id: "confirm-local-delete" },
    { id: "confirm-local-archive-state" },
    { id: "confirm-local-rollback" },
    {
      id: "artifact",
      expectedMutation: {
        name: "select_artifact",
        args: { index: 0, expectedTarget: { ...rowOwner, rowId: "C:/workspace/result.md" } },
      },
    },
    {
      id: "remove-image",
      expectedMutation: {
        name: "remove_image",
        args: { index: 0, expectedTarget: { ...rowOwner, rowId: "C:/workspace/image.png" } },
      },
    },
    {
      id: "send-review-enhanced",
      expectedMutation: {
        name: "send_prompt_review",
        args: {
          enhanced: true,
          text: "Reviewed task",
          expectedTarget: reviewTarget,
          expectedRunTarget: state().run_target,
        },
      },
    },
    {
      id: "send-review-raw",
      expectedMutation: {
        name: "send_prompt_review",
        args: {
          enhanced: false,
          text: "Reviewed task",
          expectedTarget: reviewTarget,
          expectedRunTarget: state().run_target,
        },
      },
    },
    {
      id: "cancel-review",
      expectedMutation: {
        name: "cancel_prompt_review",
        args: { expectedTarget: reviewTarget },
      },
    },
    { id: "show-file-menu", expectedMutation: { name: "show_file_menu", args: undefined } },
    { id: "show-edit-menu", expectedMutation: { name: "show_edit_menu", args: undefined } },
    { id: "show-view-menu", expectedMutation: { name: "show_view_menu", args: undefined } },
    { id: "show-help-menu", expectedMutation: { name: "show_help_menu", args: undefined } },
    { id: "close-overlay", expectedMutation: { name: "close_overlay", args: undefined } },
    {
      id: "check-docling-readiness",
      state: () => state({
        overlay: "config",
        config_fields: [{
          key: "docling.enabled",
          value: "true",
          env_override: null,
          value_type: "boolean",
          required: false,
          min_value: null,
          max_value: null,
          options: [],
        }],
      }),
      expectedMutation: {
        name: "check_docling_readiness",
        args: { expectedTarget: state().config_target },
      },
    },
    {
      id: "import-config-toml",
      state: () => {
        const current = state();
        return state({
          config_draft: {
            ...current.config_draft,
            external_owner_mutation_open: false,
          },
        });
      },
    },
    {
      id: "insert-command",
      expectedMutation: {
        name: "insert_command",
        args: {
          index: 0,
          expectedTarget: { ...rowOwner, rowId: "/case" },
          expectedDraftTarget: draftTarget,
        },
      },
    },
  ];

  assert.deepEqual(new Set(cases.map((candidate) => candidate.id)), new Set(EXACT_DELIVERY_ACTION_IDS));
  for (const candidate of cases) {
    const current = candidate.state?.() ?? state();
    const calls: MutationCall[] = [];
    let rerenders = 0;
    const actionContext = context(current, calls, () => { rerenders += 1; });
    candidate.prepare?.(actionContext);

    assert.equal(
      await dispatchGuiAction(candidate.id, 0, "", current, actionContext),
      true,
      candidate.id,
    );

    assert.deepEqual(
      calls,
      candidate.expectedMutation ? [candidate.expectedMutation] : [],
      candidate.id,
    );
    candidate.verify?.(actionContext, rerenders);
    if (candidate.id === "import-config-toml") {
      assert.equal(actionContext.uiState.externalConfigMutationPending, false);
    }
  }
});

test("the single registry dispatcher recognizes disabled actions and runs enabled actions exactly once", async () => {
  const calls: MutationCall[] = [];
  const disabled = state({ can_submit: false, can_cancel_run: false });
  const disabledContext = context(disabled, calls);

  assert.equal(await dispatchGuiAction("send", -1, "", disabled, disabledContext), true);
  assert.equal(await dispatchGuiAction("cancel-run", -1, "", disabled, disabledContext), true);
  assert.deepEqual(calls, []);

  const enabled = state();
  const enabledContext = context(enabled, calls);
  assert.equal(await dispatchGuiAction("send", -1, "", enabled, enabledContext), true);
  assert.equal(await dispatchGuiAction("enhance-prompt", -1, "", enabled, enabledContext), true);
  assert.equal(await dispatchGuiAction("review-uncommitted", -1, "", enabled, enabledContext), true);
  assert.equal(await dispatchGuiAction("cancel-run", -1, "", enabled, enabledContext), true);
  assert.deepEqual(calls, [
    {
      name: "submit_prompt",
      args: {
        text: "Run the task",
        expectedTarget: enabled.draft_target,
        expectedRunTarget: enabled.run_target,
      },
    },
    {
      name: "enhance_prompt",
      args: {
        text: "Run the task",
        expectedTarget: enabled.draft_target,
        expectedRunTarget: enabled.run_target,
      },
    },
    {
      name: "review_uncommitted",
      args: {
        text: "Run the task",
        expectedTarget: enabled.draft_target,
        expectedRunTarget: enabled.run_target,
      },
    },
    {
      name: "cancel_run",
      args: { expectedTarget: enabled.stop_target },
    },
  ]);

  assert.equal(await dispatchGuiAction("not-a-gui-action", -1, "", enabled, enabledContext), false);
  assert.equal(calls.length, 4);

  const targetless = state({ can_cancel_run: true, stop_target: null });
  const targetlessCalls: MutationCall[] = [];
  assert.equal(
    await dispatchGuiAction("cancel-run", -1, "", targetless, context(targetless, targetlessCalls)),
    true,
  );
  assert.deepEqual(targetlessCalls, [], "a capability without its exact Stop target must fail closed");
});

test("run-opening actions resolve availability and payloads from one fresh render model", async () => {
  const captured = state({
    draft_target: {
      workspacePath: "C:/workspace",
      sessionId: SESSION_A,
      ownerGeneration: "3",
    },
    run_target: {
      workspacePath: "C:/workspace",
      sessionId: SESSION_A,
      runtimeOwnerToken: "idle:9",
      permissionConfirmationId: null,
      expectedState: { kind: "idle", latestTurnId: TURN_A, admissionRevision: "4" },
    },
  });
  const advanced = state({
    draft_target: {
      workspacePath: "C:/workspace",
      sessionId: SESSION_A,
      ownerGeneration: "4",
    },
    run_target: {
      workspacePath: "C:/workspace",
      sessionId: SESSION_A,
      runtimeOwnerToken: "root:10",
      permissionConfirmationId: null,
      expectedState: { kind: "turn", turnId: TURN_B, admissionRevision: "5" },
    },
  });
  const calls: MutationCall[] = [];
  const actionContext = context(advanced, calls);

  for (const id of ["send", "enhance-prompt", "review-uncommitted"]) {
    assert.equal(await dispatchGuiAction(id, -1, "", captured, actionContext), true, id);
  }

  assert.deepEqual(calls, [
    {
      name: "submit_prompt",
      args: {
        text: "Run the task",
        expectedTarget: advanced.draft_target,
        expectedRunTarget: advanced.run_target,
      },
    },
    {
      name: "enhance_prompt",
      args: {
        text: "Run the task",
        expectedTarget: advanced.draft_target,
        expectedRunTarget: advanced.run_target,
      },
    },
    {
      name: "review_uncommitted",
      args: {
        text: "Run the task",
        expectedTarget: advanced.draft_target,
        expectedRunTarget: advanced.run_target,
      },
    },
  ]);
});

test("native and delegated keyboard boundaries preserve one action per activation", async () => {
  assert.equal(
    shouldDispatchDelegatedKeyboardAction("BUTTON"),
    false,
    "native buttons use the browser-generated click and must not also dispatch on keydown",
  );
  assert.equal(shouldDispatchDelegatedKeyboardAction("A", true), false);
  assert.equal(shouldDispatchDelegatedKeyboardAction("DIV"), true);

  let rerenders = 0;
  const calls: MutationCall[] = [];
  const current = state();
  const actionContext = context(current, calls, () => { rerenders += 1; });
  assert.equal(
    await dispatchGuiAction("toggle-attachment-tray", -1, "", current, actionContext),
    true,
  );
  assert.equal(actionContext.uiState.attachmentTrayOpen, true);
  assert.equal(rerenders, 1);
  assert.deepEqual(calls, []);
});

test("fast new-chat settlements wait for pointer, Enter, and Space activation release", async () => {
  type Settlement = { activation: string; ownerGeneration: string };
  for (const activation of ["pointer", "Enter", "Space"] as const) {
    const lifecycle = new InteractionLifecycle<Settlement>(() => true);
    let end: InteractionEnd<Settlement> | null;
    if (activation === "pointer") {
      lifecycle.beginPointer(17);
      end = lifecycle.capturePointerEnd(17);
    } else {
      lifecycle.beginKey(activation);
      end = lifecycle.captureKeyEnd(activation);
    }
    assert.notEqual(end, null);

    const current = state();
    const actionContext = context(current, []);
    const owner: NewSessionMutationOwnerState = { activeNewSessionMutation: null };
    let commandCount = 0;
    const accepted: Settlement[] = [];
    actionContext.mutate = async (name) => {
      assert.equal(name, "new_chat");
      await dispatchNewSessionMutation(
        owner,
        "new_chat",
        lifecycle,
        () => undefined,
        async () => {
          commandCount += 1;
          assert.equal(lifecycle.active, true, `${activation} still owns the synchronous response`);
          return { activation, ownerGeneration: "4" };
        },
        (response) => accepted.push(response),
        () => assert.fail("successful settlement must not release through the error path"),
      );
    };

    const first = dispatchGuiAction("new-chat", -1, "", current, actionContext);
    await Promise.resolve();
    assert.equal(commandCount, 1, `${activation} dispatches one command`);
    assert.equal(
      await dispatchGuiAction("new-chat", -1, "", current, actionContext),
      true,
      `${activation} repeat still resolves at the registered action boundary`,
    );
    assert.equal(commandCount, 1, `${activation} repeat cannot cross the single-flight owner`);
    assert.deepEqual(accepted, []);
    const release = end!();
    assert.deepEqual(release, { deferred: null, renderCurrent: false });
    assert.equal(lifecycle.active, false);
    assert.equal(await first, true);
    assert.deepEqual(accepted, [{ activation, ownerGeneration: "4" }]);
    assert.equal(owner.activeNewSessionMutation, null);
  }
});

test("row actions keep the clicked row identity in the command payload", async () => {
  const current = state();
  const calls: MutationCall[] = [];
  const actionContext = context(current, calls);
  const expectedOwner = {
    workspacePath: "C:/workspace",
    ownerProjectId: "project-a",
    ownerSessionId: SESSION_A,
  };

  for (const [id, expectedName, rowId] of [
    ["new-project-session", "new_project_session", "project-a"],
    ["project", "select_project", "project-a"],
    ["session", "select_session", SESSION_A],
    ["chat-session", "select_chat_session", QUICK_A],
    ["artifact", "select_artifact", "C:/workspace/result.md"],
    ["remove-image", "remove_image", "C:/workspace/image.png"],
    ["insert-command", "insert_command", "/case"],
  ] as const) {
    assert.equal(await dispatchGuiAction(id, 0, "", current, actionContext), true, id);
    const expectedDraftTarget = id === "insert-command"
      ? { expectedDraftTarget: current.draft_target }
      : {};
    assert.deepEqual(calls.pop(), {
      name: expectedName,
      args: {
        index: 0,
        expectedTarget: { ...expectedOwner, rowId },
        ...expectedDraftTarget,
      },
    }, id);
  }

  const navigationBlocked = state({ navigation_admission_open: false });
  const blockedCalls: MutationCall[] = [];
  assert.equal(
    await dispatchGuiAction("project", 0, "", navigationBlocked, context(navigationBlocked, blockedCalls)),
    true,
  );
  assert.deepEqual(blockedCalls, []);

  assert.equal(await dispatchGuiAction("remove-image", 9, "", current, actionContext), true);
  assert.deepEqual(calls, []);
});

test("closed image admission blocks new attachments but keeps existing attachment removal available", () => {
  const current = state({ image_input_enabled: false });
  const model = context(current, []).getRenderModel();
  assert.ok(model);
  for (const id of ["toggle-attachment-tray", "set-image", "browse-image"]) {
    assert.equal(actionById(id)?.enabled(model, { index: -1, value: "" }), false, id);
  }
  assert.equal(
    actionById("clear-images")?.enabled(model, { index: -1, value: "" }),
    true,
  );
  assert.equal(
    actionById("remove-image")?.enabled(model, { index: 0, value: "" }),
    true,
  );
});

test("session-row Stop forwards the captured typed target without a legacy turn argument", async () => {
  const base = state();
  const expectedStopTarget = rootStopTarget();
  const current = state({
    session_rows: [{
      ...base.session_rows[0]!,
      loaded_status: "active",
      active_turn_id: TURN_A,
      interrupt_target: expectedStopTarget,
    }],
  });
  const calls: MutationCall[] = [];

  assert.equal(
    await dispatchGuiAction("interrupt-session", 0, "", current, context(current, calls)),
    true,
  );
  assert.deepEqual(calls, [{
    name: "interrupt_session",
    args: {
      index: 0,
      expectedTarget: {
        workspacePath: "C:/workspace",
        ownerProjectId: "project-a",
        ownerSessionId: SESSION_A,
        rowId: SESSION_A,
      },
      expectedStopTarget,
    },
  }]);
  assert.equal(Object.hasOwn(calls[0]!.args!, "expectedTurnId"), false);

  const missingTarget = state({
    session_rows: [{ ...current.session_rows[0]!, interrupt_target: null }],
  });
  const rejectedCalls: MutationCall[] = [];
  const missingTargetContext = context(missingTarget, rejectedCalls);
  const missingTargetModel = missingTargetContext.getRenderModel();
  assert.ok(missingTargetModel);
  assert.equal(
    actionById("interrupt-session")?.enabled(
      missingTargetModel,
      { index: 0, value: "" },
    ),
    false,
  );
  assert.equal(
    actionById("rejoin-session")?.enabled(
      missingTargetModel,
      { index: 0, value: "" },
    ),
    true,
    "rejoin remains available for the same active row without a Stop target",
  );
  assert.equal(
    await dispatchGuiAction(
      "interrupt-session",
      0,
      "",
      missingTarget,
      missingTargetContext,
    ),
    true,
  );
  assert.deepEqual(rejectedCalls, []);
});

test("review, menu, and overlay actions enforce their gates and exact payloads", async () => {
  const blocked = state({ send_enhanced_enabled: false, send_raw_enabled: false });
  const calls: MutationCall[] = [];
  const blockedContext = context(blocked, calls);
  await dispatchGuiAction("send-review-enhanced", -1, "", blocked, blockedContext);
  await dispatchGuiAction("send-review-raw", -1, "", blocked, blockedContext);
  assert.deepEqual(calls, []);

  const current = state();
  const actionContext = context(current, calls);
  for (const id of [
    "send-review-enhanced",
    "send-review-raw",
    "cancel-review",
    "show-file-menu",
    "show-edit-menu",
    "show-view-menu",
    "show-help-menu",
    "close-overlay",
  ]) {
    assert.equal(await dispatchGuiAction(id, -1, "", current, actionContext), true, id);
  }
  assert.deepEqual(calls, [
    {
      name: "send_prompt_review",
      args: {
        enhanced: true,
        text: "Reviewed task",
        expectedTarget: current.review_target,
        expectedRunTarget: current.run_target,
      },
    },
    {
      name: "send_prompt_review",
      args: {
        enhanced: false,
        text: "Reviewed task",
        expectedTarget: current.review_target,
        expectedRunTarget: current.run_target,
      },
    },
    { name: "cancel_prompt_review", args: { expectedTarget: current.review_target } },
    { name: "show_file_menu", args: undefined },
    { name: "show_edit_menu", args: undefined },
    { name: "show_view_menu", args: undefined },
    { name: "show_help_menu", args: undefined },
    { name: "close_overlay", args: undefined },
  ]);

  const setup = state({
    overlay: "config",
    startup: {
      ...current.startup,
      initial_setup_required: true,
      action_overlay: "config",
    },
  });
  const setupCalls: MutationCall[] = [];
  assert.equal(await dispatchGuiAction("close-overlay", -1, "", setup, context(setup, setupCalls)), true);
  assert.deepEqual(setupCalls, []);

  const promptReview = state({ overlay: "prompt_review" });
  const reviewCloseCalls: MutationCall[] = [];
  assert.equal(
    await dispatchGuiAction("close-overlay", -1, "", promptReview, context(promptReview, reviewCloseCalls)),
    true,
  );
  assert.deepEqual(reviewCloseCalls, [{
    name: "cancel_prompt_review",
    args: { expectedTarget: promptReview.review_target },
  }]);
  assert.equal(overlayDismissAction("prompt_review"), "cancel-review");
  assert.equal(overlayDismissAction("config"), "close-overlay");

  const targetlessReview = state({ overlay: "prompt_review", review_target: null });
  const targetlessCalls: MutationCall[] = [];
  for (const action of ["send-review-enhanced", "send-review-raw", "cancel-review", "close-overlay"]) {
    await dispatchGuiAction(action, -1, "", targetlessReview, context(targetlessReview, targetlessCalls));
  }
  assert.deepEqual(targetlessCalls, [], "a review action without an exact target fails closed");
});

test("review submission preserves mismatched review and current run expectations for Rust to reject", async () => {
  const cases = [
    {
      label: "active turn A to active turn B",
      reviewExpectedState: {
        kind: "turn",
        turnId: TURN_A,
        admissionRevision: "4",
      } as const,
      currentRunTarget: {
        ...state().run_target,
        runtimeOwnerToken: "root:10",
        expectedState: { kind: "turn", turnId: TURN_B, admissionRevision: "5" } as const,
      },
    },
    {
      label: "idle latest-turn generation drift",
      reviewExpectedState: {
        kind: "idle",
        latestTurnId: TURN_A,
        admissionRevision: "4",
      } as const,
      currentRunTarget: {
        ...state().run_target,
        runtimeOwnerToken: "idle:11",
        expectedState: {
          kind: "idle",
          latestTurnId: TURN_B,
          admissionRevision: "5",
        } as const,
      },
    },
    {
      label: "idle admission revision drift with the same latest turn",
      reviewExpectedState: {
        kind: "idle",
        latestTurnId: TURN_A,
        admissionRevision: "4",
      } as const,
      currentRunTarget: {
        ...state().run_target,
        runtimeOwnerToken: "idle:11",
        expectedState: {
          kind: "idle",
          latestTurnId: TURN_A,
          admissionRevision: "5",
        } as const,
      },
    },
  ];

  for (const candidate of cases) {
    const current = state({
      review_target: {
        ...state().review_target!,
        expectedState: candidate.reviewExpectedState,
      },
      run_target: candidate.currentRunTarget,
    });
    const calls: MutationCall[] = [];
    await dispatchGuiAction("send-review-enhanced", -1, "", current, context(current, calls));

    assert.deepEqual(calls, [{
      name: "send_prompt_review",
      args: {
        enhanced: true,
        text: "Reviewed task",
        expectedTarget: current.review_target,
        expectedRunTarget: current.run_target,
      },
    }], candidate.label);
    assert.notDeepEqual(
      current.review_target?.expectedState,
      current.run_target.expectedState,
      candidate.label,
    );
  }
});

test("destructive actions create an owner-bound confirmation before mutation", async () => {
  const current = state();
  const calls: MutationCall[] = [];
  let rerenders = 0;
  const actionContext = context(current, calls, () => { rerenders += 1; });

  assert.equal(await dispatchGuiAction("delete-session", 0, "", current, actionContext), true);
  assert.deepEqual(actionContext.uiState.pendingLocalConfirmation, {
    kind: "session",
    index: 0,
    title: "Session A",
    detail: SESSION_A,
    expectedTarget: {
      workspacePath: "C:/workspace",
      ownerProjectId: "project-a",
      ownerSessionId: SESSION_A,
      rowId: SESSION_A,
    },
  });
  assert.equal(rerenders, 1);
  assert.deepEqual(calls, [], "the destructive command is deferred until explicit confirmation");

  const blocked = state({ navigation_admission_open: false });
  const blockedContext = context(blocked, calls);
  assert.equal(await dispatchGuiAction("delete-project", 0, "", blocked, blockedContext), true);
  assert.equal(blockedContext.uiState.pendingLocalConfirmation, null);
  assert.deepEqual(calls, []);
});

test("Docling readiness admits only one invoke across rapid double activation", async () => {
  const ready = state({
    overlay: "config",
    config_fields: [{
      key: "docling.enabled",
      value: "true",
      env_override: null,
      value_type: "boolean",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    }],
  });
  const calls: MutationCall[] = [];
  let rerenders = 0;
  let release!: () => void;
  const inFlight = new Promise<void>((resolve) => { release = resolve; });
  const actionContext = context(ready, calls, () => { rerenders += 1; });
  actionContext.mutate = async (name, args) => {
    calls.push({ name, args });
    await inFlight;
  };

  const first = dispatchGuiAction("check-docling-readiness", -1, "", ready, actionContext);
  assert.notEqual(actionContext.uiState.doclingReadinessTransaction.active, null);
  await dispatchGuiAction("check-docling-readiness", -1, "", ready, actionContext);
  assert.deepEqual(calls, [{
    name: "check_docling_readiness",
    args: { expectedTarget: ready.config_target },
  }]);
  assert.equal(rerenders, 1, "the local pending owner is rendered before invoke settlement");

  release();
  await first;
  assert.equal(actionContext.uiState.doclingReadinessTransaction.active, null);
  assert.equal(rerenders, 2, "exact settlement releases and rerenders the local pending owner");

  const readinessFailure = new Error("readiness invoke failed");
  actionContext.mutate = async (name, args) => {
    calls.push({ name, args });
    throw readinessFailure;
  };
  await assert.rejects(
    dispatchGuiAction("check-docling-readiness", -1, "", ready, actionContext),
    readinessFailure,
  );
  assert.equal(actionContext.uiState.doclingReadinessTransaction.active, null);
  assert.equal(rerenders, 4, "error settlement also releases and rerenders the local pending owner");
  assert.equal(calls.length, 2);
});

test("Initial Setup Import is read-only, exact-targeted, complete, and single-flight", async () => {
  const setupTarget = {
    workspacePath: "C:/workspace",
    globalConfigPath: "C:/config/config.toml",
    setupGeneration: "3",
  };
  const wizard = state({
    overlay: "initial_setup",
    startup: {
      ...state().startup,
      action_overlay: "initial_setup",
      initial_setup_required: true,
      initial_setup_reason: "config_missing",
      global_config_path: setupTarget.globalConfigPath,
      setup_target: setupTarget,
    },
    config_fields: [
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
      {
        key: "docling.enabled",
        value: "false",
        env_override: null,
        value_type: "boolean",
        required: false,
        min_value: null,
        max_value: null,
        options: [],
      },
    ],
  });
  const actionContext = context(wizard, []);
  const nativeCalls: MutationCall[] = [];
  let release!: (result: unknown) => void;
  const blockedResult = new Promise((resolve) => { release = resolve; });
  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    writable: true,
    value: {
      __TAURI_INTERNALS__: {
        invoke: async (name: string, args?: Record<string, unknown>) => {
          nativeCalls.push({ name, args });
          return blockedResult;
        },
      },
    },
  });

  try {
    const first = dispatchGuiAction("import-config-toml", -1, "", wizard, actionContext);
    assert.equal(actionContext.uiState.initialSetupAuxiliary.active?.kind, "import");
    await dispatchGuiAction("import-config-toml", -1, "", wizard, actionContext);
    assert.equal(nativeCalls.length, 1, "the file picker is owned by one local request");
    assert.deepEqual(nativeCalls[0], {
      name: "load_initial_setup_config_toml",
      args: {
        expectedConfigTarget: wizard.config_target,
        expectedSetupTarget: setupTarget,
      },
    });
    release({
      sourcePath: "C:/existing/config.toml",
      values: [
        { key: "model.model", text: "model-b" },
        { key: "docling.enabled", text: "true" },
      ],
    });
    await first;

    assert.equal(actionContext.uiState.configDirty, true);
    assert.deepEqual(Array.from(actionContext.uiState.configDraftValues), [
      ["model.model", "model-b"],
      ["docling.enabled", "true"],
    ]);
    assert.equal(
      initialSetupImportedSourcePath(
        actionContext.uiState.initialSetupAuxiliary,
        setupTarget,
        wizard.config_target,
      ),
      "C:/existing/config.toml",
    );
  } finally {
    if (previousWindow) Object.defineProperty(globalThis, "window", previousWindow);
    else delete (globalThis as Record<string, unknown>).window;
  }
});

test("Initial Setup Docling check sends the complete draft and admits only its endpoint result", async () => {
  const setupTarget = {
    workspacePath: "C:/workspace",
    globalConfigPath: "C:/config/config.toml",
    setupGeneration: "4",
  };
  const fields = [
    {
      key: "docling.enabled",
      value: "true",
      env_override: null,
      value_type: "boolean" as const,
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "docling.base_url",
      value: "http://127.0.0.1:5001/",
      env_override: null,
      value_type: "string" as const,
      required: true,
      min_value: null,
      max_value: null,
      options: [],
    },
  ];
  const wizard = state({
    overlay: "initial_setup",
    startup: {
      ...state().startup,
      action_overlay: "initial_setup",
      initial_setup_required: true,
      initial_setup_reason: "optional_tool_invalid",
      global_config_path: setupTarget.globalConfigPath,
      setup_target: setupTarget,
    },
    config_fields: fields,
  });
  const actionContext = context(wizard, []);
  actionContext.uiState.initialSetup.step = "tools";
  const pending = state({
    ...wizard,
    docling_readiness: {
      status: "checking",
      endpoint: "http://127.0.0.1:5001/ready",
      httpStatus: null,
      message: "Checking Docling readiness.",
    },
  });
  const nativeCalls: MutationCall[] = [];
  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    writable: true,
    value: {
      __TAURI_INTERNALS__: {
        invoke: async (name: string, args?: Record<string, unknown>) => {
          nativeCalls.push({ name, args });
          return pending;
        },
      },
    },
  });

  try {
    await dispatchGuiAction("check-docling-readiness", -1, "", wizard, actionContext);
    assert.deepEqual(nativeCalls, [{
      name: "check_initial_setup_docling_readiness",
      args: {
        values: fields.map((field) => ({ key: field.key, text: field.value })),
        expectedConfigTarget: wizard.config_target,
        expectedSetupTarget: setupTarget,
      },
    }]);
    assert.equal(actionContext.uiState.initialSetupAuxiliary.active, null);
    assert.equal(initialSetupDoclingReadinessVisible(
      actionContext.uiState.initialSetupAuxiliary,
      setupTarget,
      wizard.config_target,
      actionContext.uiState.configDraftRevision,
      pending.docling_readiness.endpoint,
    ), true);
    assert.equal(initialSetupDoclingReadinessVisible(
      actionContext.uiState.initialSetupAuxiliary,
      setupTarget,
      wizard.config_target,
      actionContext.uiState.configDraftRevision,
      "http://127.0.0.1:5002/ready",
    ), false);
  } finally {
    if (previousWindow) Object.defineProperty(globalThis, "window", previousWindow);
    else delete (globalThis as Record<string, unknown>).window;
  }
});

test("dirty Settings X or Escape route requests one exact-target confirmation and Cancel restores X focus", async () => {
  const clean = state();
  const dirty = state({
    overlay: "config",
    config_draft: {
      ...clean.config_draft,
      dirty: true,
      discard_enabled: true,
      commit_enabled: true,
      external_owner_mutation_open: false,
    },
  });
  const calls: MutationCall[] = [];
  let rerenders = 0;
  const actionContext = context(dirty, calls, () => { rerenders += 1; });

  assert.equal(overlayDismissAction("config"), "close-overlay");
  assert.equal(await dispatchGuiAction("close-overlay", -1, "", dirty, actionContext), true);
  assert.deepEqual(actionContext.uiState.pendingLocalConfirmation, {
    kind: "settings_close",
    expectedTarget: dirty.config_target,
  });
  assert.deepEqual(calls, [], "dirty close must not reach close_overlay before a decision");
  assert.equal(rerenders, 1);

  assert.equal(await dispatchGuiAction("cancel-local-confirm", -1, "", dirty, actionContext), true);
  assert.equal(actionContext.uiState.pendingLocalConfirmation, null);
  assert.deepEqual(actionContext.uiState.settingsActionFocusContinuation, {
    target: dirty.config_target,
    primaryAction: "close-overlay",
    fallbackAction: null,
  });
  assert.deepEqual(calls, []);
  assert.equal(rerenders, 2);
});

test("discard-and-close validates the exact config owner with its Rust baseline before issuing close_overlay", async () => {
  const clean = state();
  const dirty = state({
    overlay: "config",
    config_fields: [{
      key: "model.model",
      value: "draft-model",
      env_override: null,
      value_type: "string",
      required: true,
      min_value: null,
      max_value: null,
      options: [],
    }],
    config_draft: {
      ...clean.config_draft,
      dirty: true,
      discard_enabled: true,
      commit_enabled: true,
      external_owner_mutation_open: false,
    },
  });
  const calls: MutationCall[] = [];
  const actionContext = context(dirty, calls);
  actionContext.uiState.configDirty = true;
  actionContext.uiState.configDraftTarget = { ...dirty.config_target };
  actionContext.uiState.configDraftValues.set("model.model", "draft-model");
  actionContext.uiState.configDraftBaselineValues.set("model.model", "saved-model");
  actionContext.uiState.configDraftRevision = 1n;
  const baseline = state({
    overlay: "config",
    config_target: dirty.config_target,
    config_fields: [{ ...dirty.config_fields[0], value: "saved-model" }],
  });
  actionContext.getProjection = () => baseline;
  const values = [{ key: "model.model", text: "saved-model" }];
  let currentView = dirty;
  actionContext.getViewState = () => currentView;
  const accepted: Array<{
    overlay: DesktopViewState["overlay"];
    localConfirmationOpen: boolean;
    settingsPending: boolean;
  }> = [];
  const sequence: string[] = [];
  let markInteractionBarrierReached!: () => void;
  const interactionBarrierReached = new Promise<void>((resolve) => {
    markInteractionBarrierReached = resolve;
  });
  let releaseInteraction!: () => void;
  const interactionIdle = new Promise<void>((resolve) => {
    releaseInteraction = resolve;
  });
  actionContext.waitForInteractionIdle = async () => {
    sequence.push("wait:interaction-idle");
    markInteractionBarrierReached();
    await interactionIdle;
    sequence.push("settled:interaction-idle");
  };
  actionContext.acceptProjection = (next) => {
    currentView = next as DesktopViewState;
    accepted.push({
      overlay: currentView.overlay,
      localConfirmationOpen: actionContext.uiState.pendingLocalConfirmation !== null,
      settingsPending: actionContext.getRenderModel()?.local.configMutationPending ?? false,
    });
    sequence.push(`accept:${currentView.overlay}`);
  };
  const nativeCalls: MutationCall[] = [];
  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    writable: true,
    value: {
      __TAURI_INTERNALS__: {
        invoke: async (name: string, args?: Record<string, unknown>) => {
          nativeCalls.push({ name, args });
          sequence.push(`invoke:${name}`);
          return name === "close_overlay" ? state({ overlay: "none" }) : baseline;
        },
      },
    },
  });

  try {
    await dispatchGuiAction("close-overlay", -1, "", dirty, actionContext);
    const confirmation = dispatchGuiAction(
      "confirm-settings-discard-close",
      -1,
      "",
      dirty,
      actionContext,
    );
    await interactionBarrierReached;
    assert.deepEqual(nativeCalls.map((call) => call.name), ["reset_config_draft"]);
    assert.deepEqual(accepted, [
      { overlay: "config", localConfirmationOpen: false, settingsPending: true },
    ], "close_overlay waits until the alertdialog-close render has committed");
    releaseInteraction();
    await confirmation;

    assert.deepEqual(nativeCalls, [
      {
        name: "reset_config_draft",
        args: { values, expectedTarget: dirty.config_target },
      },
      { name: "close_overlay", args: {} },
    ]);
    assert.deepEqual(calls, []);
    assert.equal(actionContext.uiState.configDirty, false);
    assert.equal(actionContext.uiState.pendingLocalConfirmation, null);
    assert.deepEqual(accepted, [
      { overlay: "config", localConfirmationOpen: false, settingsPending: true },
      { overlay: "none", localConfirmationOpen: false, settingsPending: false },
    ]);
    assert.deepEqual(sequence, [
      "invoke:reset_config_draft",
      "accept:config",
      "wait:interaction-idle",
      "settled:interaction-idle",
      "invoke:close_overlay",
      "accept:none",
    ], "the local modal and Settings overlay settle in separate renders");
  } finally {
    if (previousWindow) Object.defineProperty(globalThis, "window", previousWindow);
    else delete (globalThis as Record<string, unknown>).window;
  }
});

test("discard-and-close never crosses a config target change that wins the reset settlement race", async () => {
  const clean = state();
  const dirty = state({
    overlay: "config",
    config_fields: [{
      key: "model.model",
      value: "draft-model",
      env_override: null,
      value_type: "string",
      required: true,
      min_value: null,
      max_value: null,
      options: [],
    }],
    config_draft: {
      ...clean.config_draft,
      dirty: true,
      discard_enabled: true,
      commit_enabled: true,
      external_owner_mutation_open: false,
    },
  });
  let currentView = dirty;
  const actionContext = context(dirty, []);
  actionContext.getViewState = () => currentView;
  actionContext.getProjection = () => state({
    overlay: "config",
    config_target: dirty.config_target,
    config_fields: [{ ...dirty.config_fields[0], value: "saved-model" }],
  });
  actionContext.uiState.configDirty = true;
  actionContext.uiState.configDraftTarget = { ...dirty.config_target };
  actionContext.uiState.configDraftValues.set("model.model", "draft-model");
  actionContext.uiState.configDraftBaselineValues.set("model.model", "saved-model");
  const nativeCalls: MutationCall[] = [];
  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    writable: true,
    value: {
      __TAURI_INTERNALS__: {
        invoke: async (name: string, args?: Record<string, unknown>) => {
          nativeCalls.push({ name, args });
          currentView = state({
            overlay: "config",
            config_target: { ...dirty.config_target, configGeneration: "8" },
          });
          return currentView;
        },
      },
    },
  });

  try {
    await dispatchGuiAction("close-overlay", -1, "", dirty, actionContext);
    await dispatchGuiAction("confirm-settings-discard-close", -1, "", dirty, actionContext);
    assert.deepEqual(nativeCalls.map((call) => call.name), ["reset_config_draft"]);
    assert.equal(actionContext.uiState.configDirty, true);
    assert.equal(actionContext.uiState.localConfirmationDecisionPending, false);
    assert.match(actionContext.uiState.localConfirmationDecisionError, /設定対象が変更/);
  } finally {
    if (previousWindow) Object.defineProperty(globalThis, "window", previousWindow);
    else delete (globalThis as Record<string, unknown>).window;
  }
});

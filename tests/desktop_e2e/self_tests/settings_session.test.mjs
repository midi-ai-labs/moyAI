import assert from "node:assert/strict";
import test from "node:test";

import {
  SESSION_CONTEXT_AFTER,
  SESSION_CONTEXT_BEFORE,
  SESSION_PROVIDER_API_KEY_ENV,
  SESSION_PROVIDER_PROFILE,
  SESSION_PROVIDER_PROFILE_OPTIONS,
  SESSION_SETTINGS_MODEL_TRIGGER,
  advancedSessionSettingsTarget,
  completedSessionRootReady,
  createSettingsSessionScenario,
  createStableRestoredSessionSettingsDecision,
  exactSessionProviderLedger,
  expectedSessionSettingsApplyCommand,
  hoveredProjectActionDecision,
  projectNewSessionLocator,
  projectRowHoverLocator,
  projectRootNavigationDecision,
  projectSessionSelectionLocator,
  restartedSessionSelectionDecision,
  restartedSessionSettingsCloseDecision,
  restartedSessionSettingsOpenDecision,
  restartedSessionSettingsTriggerDecision,
  restoredSessionSettingsPanelDecision,
  sessionSettingsDirtyGuardReady,
  sessionSettingsPanelReady,
} from "../scenarios/settings_session.mjs";
import { assertExactSemanticTarget } from "../drivers/webview_input.mjs";

const target = Object.freeze({
  workspacePath: "C:\\e2e\\workspace",
  rootSessionId: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
  settingsRevision: "3",
  configGeneration: "9",
  runtimeOwnerToken: "root:01ARZ3NDEKTSV4RRFFQ69G5FAV:9",
});
const projectId = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const betaRootSessionId = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const restoredTarget = Object.freeze({
  ...target,
  settingsRevision: "4",
  configGeneration: "1",
  runtimeOwnerToken: "idle:0",
});
const betaTarget = Object.freeze({
  ...target,
  rootSessionId: betaRootSessionId,
  settingsRevision: "0",
  configGeneration: "1",
  runtimeOwnerToken: "idle:0",
});
const restoredExpected = Object.freeze({
  expectedWorkspacePath: target.workspacePath,
  projectId,
  rootSessionIds: [target.rootSessionId, betaRootSessionId],
  expectedTargetRoot: target.rootSessionId,
  expectedSettingsRevision: restoredTarget.settingsRevision,
  expectedBaseUrl: "http://127.0.0.1:43111",
  expectedModel: "e2e/scripted-responses",
  expectedProviderProfile: SESSION_PROVIDER_PROFILE,
  expectedApiKeyEnv: SESSION_PROVIDER_API_KEY_ENV,
  expectedAccessMode: "default",
  expectedPrompt: "create root alpha",
  expectedResponse: "ALPHA_OK",
  expectedResponseCount: 2,
});

function panelField(value, options = []) {
  return { count: 1, visible: true, enabled: true, value, options };
}

function trigger(triggerName) {
  return {
    count: 1,
    visible: true,
    enabled: true,
    identity: {
      tag: "BUTTON",
      action: "show-session-settings",
      session_settings_trigger: triggerName,
    },
  };
}

function surface({
  contextWindow = SESSION_CONTEXT_BEFORE,
  inherited = true,
  dirty = false,
  targetValue = target,
  overrides = {},
} = {}) {
  return {
    projection: {
      overlay: "session_settings",
      run_status_key: "completed",
      task_activity_state: "idle",
      busy: false,
      agent_tree_active: false,
      post_run_refresh_pending: false,
      background_mutation_pending: false,
      async_polling_required: false,
      navigation_loading: false,
      transcript_rows: [],
      session_settings: {
        available: true,
        base_url: "http://127.0.0.1:43111",
        model: "e2e/scripted-responses",
        provider_profile: SESSION_PROVIDER_PROFILE,
        api_key_env: SESSION_PROVIDER_API_KEY_ENV,
        access_mode: "default",
        context_window: inherited ? SESSION_CONTEXT_BEFORE : contextWindow,
        context_window_inherited: inherited,
        target: { ...targetValue },
      },
    },
    panel: {
      count: 1,
      visible: true,
      inert: false,
      scope_count: 1,
      scope_visible: true,
      scope_text: "このセッションだけ",
      base_url: panelField("http://127.0.0.1:43111"),
      model: panelField("e2e/scripted-responses"),
      provider_profile: panelField(SESSION_PROVIDER_PROFILE, [...SESSION_PROVIDER_PROFILE_OPTIONS]),
      api_key_env: panelField(SESSION_PROVIDER_API_KEY_ENV),
      access_mode: panelField("default"),
      context_window: panelField(contextWindow),
      context_inherited_badge: { count: 1, visible: inherited },
      max_output_tokens: { count: 0, visible: false, enabled: false, value: null, options: [] },
      apply: { count: 1, visible: true, enabled: dirty },
      discard: { count: dirty ? 1 : 0, visible: dirty, enabled: dirty },
      preferences: { count: 1, visible: true, enabled: true },
      close: { count: 1, visible: true, enabled: true },
      save_global_count: 0,
    },
    confirmation: {
      count: 0,
      visible: false,
      cancel: { count: 0, enabled: false },
      discard_close: { count: 0, enabled: false },
    },
    triggers: {
      model: trigger("model"),
      access: trigger("access"),
    },
    prompt: { count: 1, value: "", enabled: true },
    visible_dialog_count: 1,
    visible_backdrop_count: 1,
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
    visible_validation_error_count: 0,
    ...overrides,
  };
}

function closedPanel(panel) {
  return {
    ...panel,
    count: 0,
    visible: false,
    inert: false,
    scope_count: 0,
    scope_visible: false,
    scope_text: null,
    base_url: { count: 0, visible: false, enabled: false, value: null },
    model: { count: 0, visible: false, enabled: false, value: null },
    provider_profile: { count: 0, visible: false, enabled: false, value: null, options: [] },
    api_key_env: { count: 0, visible: false, enabled: false, value: null, options: [] },
    access_mode: { count: 0, visible: false, enabled: false, value: null },
    context_window: { count: 0, visible: false, enabled: false, value: null },
    max_output_tokens: { count: 0, visible: false, enabled: false, value: null },
    apply: { count: 0, visible: false, enabled: false },
    discard: { count: 0, visible: false, enabled: false },
    preferences: { count: 0, visible: false, enabled: false },
    close: { count: 0, visible: false, enabled: false },
  };
}

function restartedSurface({
  selectedRoot = target.rootSessionId,
  targetValue = restoredTarget,
  overlay = "session_settings",
  panelOpen = overlay === "session_settings",
  inherited = false,
  projectionOverrides = {},
  surfaceOverrides = {},
} = {}) {
  const base = surface({
    contextWindow: inherited ? SESSION_CONTEXT_BEFORE : SESSION_CONTEXT_AFTER,
    inherited,
    targetValue,
  });
  const selectedSessionIndex = selectedRoot === target.rootSessionId ? 0 : 1;
  return {
    ...base,
    projection: {
      ...base.projection,
      projection_revision: "10",
      workspace_path: target.workspacePath,
      overlay,
      pending_async_operations: [],
      selected_project_index: 0,
      selected_session_index: selectedSessionIndex,
      project_rows: [{ project_id: projectId }],
      session_rows: [
        { session_id: target.rootSessionId },
        { session_id: betaRootSessionId },
      ],
      transcript_rows: selectedRoot === target.rootSessionId ? [
        { row_kind: "user", body: "create root alpha" },
        { row_kind: "work_summary_completed", body: "done" },
        { row_kind: "assistant", body: "ALPHA_OK" },
      ] : [
        { row_kind: "user", body: "create root beta" },
        { row_kind: "work_summary_completed", body: "done" },
        { row_kind: "assistant", body: "BETA_OK" },
      ],
      ...projectionOverrides,
    },
    panel: panelOpen ? base.panel : closedPanel(base.panel),
    project_session_navigation: [
      {
        action: "session",
        focus_key: `session:${target.rootSessionId}:select`,
        aria_current: selectedRoot === target.rootSessionId ? "page" : null,
        visible: true,
        enabled: !panelOpen,
      },
      {
        action: "session",
        focus_key: `session:${betaRootSessionId}:select`,
        aria_current: selectedRoot === betaRootSessionId ? "page" : null,
        visible: true,
        enabled: !panelOpen,
      },
    ],
    visible_dialog_count: panelOpen ? 1 : 0,
    visible_backdrop_count: panelOpen ? 1 : 0,
    ...surfaceOverrides,
  };
}

function acceptedLedger() {
  return [1, 2].map((sequence) => ({
    sequence,
    method: "POST",
    pathname: "/v1/responses",
    route: "responses",
    contract: { pass: true },
    response_phase: "completed",
    response_status: 200,
  }));
}

function commandSnapshot(command, { count = 1, afterSequence = 0 } = {}) {
  return {
    found: true,
    sequence: afterSequence + count,
    dropped_through: 0,
    calls: Array.from({ length: count }, (_, index) => ({
      sequence: afterSequence + index + 1,
      command,
      args: {},
    })),
  };
}

test("Session Settings model trigger locator is placement-independent and exact", () => {
  assert.deepEqual(SESSION_SETTINGS_MODEL_TRIGGER, {
    selector: 'button[data-action="show-session-settings"][data-session-settings-trigger="model"]',
    identity: {
      tag: "BUTTON",
      action: "show-session-settings",
      sessionSettingsTrigger: "model",
    },
  });
  assert.doesNotMatch(SESSION_SETTINGS_MODEL_TRIGGER.selector, /\.topbar|\.chips/);

  const identity = {
    tag: "BUTTON",
    action: "show-session-settings",
    sessionSettingsTrigger: "model",
  };
  const observation = {
    count: 1,
    connected: true,
    visible: true,
    enabled: true,
    identity,
    hit_identity: identity,
    center_hit: true,
    center: { x: 100, y: 40 },
    rect: { left: 80, top: 30, right: 120, bottom: 50, width: 40, height: 20 },
  };
  assert.doesNotThrow(() => assertExactSemanticTarget(observation, SESSION_SETTINGS_MODEL_TRIGGER));
  assert.throws(
    () => assertExactSemanticTarget({ ...observation, count: 2 }, SESSION_SETTINGS_MODEL_TRIGGER),
    (error) => error.code === "semantic-target-cardinality",
  );
  const accessIdentity = { ...identity, sessionSettingsTrigger: "access" };
  assert.throws(
    () => assertExactSemanticTarget({
      ...observation,
      identity: accessIdentity,
      hit_identity: accessIdentity,
    }, SESSION_SETTINGS_MODEL_TRIGGER),
    (error) => error.code === "semantic-target-identity",
  );
});

test("project-root navigation locators bind stable action and focus identity without placement or index", () => {
  assert.deepEqual(projectRowHoverLocator(projectId), {
    selector: `button[data-action="project"][data-focus-key="project:${projectId}:select"]`,
    identity: {
      tag: "BUTTON",
      action: "project",
      focusKey: `project:${projectId}:select`,
    },
  });
  assert.deepEqual(projectNewSessionLocator(projectId), {
    selector: `button[data-action="new-project-session"][data-focus-key="project:${projectId}:new-session"]`,
    identity: {
      tag: "BUTTON",
      action: "new-project-session",
      focusKey: `project:${projectId}:new-session`,
    },
  });
  assert.deepEqual(projectSessionSelectionLocator(target.rootSessionId), {
    selector: `button[data-action="session"][data-focus-key="session:${target.rootSessionId}:select"]`,
    identity: {
      tag: "BUTTON",
      action: "session",
      focusKey: `session:${target.rootSessionId}:select`,
    },
  });
  for (const locator of [
    projectRowHoverLocator(projectId),
    projectNewSessionLocator(projectId),
    projectSessionSelectionLocator(target.rootSessionId),
  ]) {
    assert.doesNotMatch(locator.selector, /\.nav-row|\[data-index=/);
  }
  assert.throws(() => projectRowHoverLocator("not-a-project"), /canonical navigation identity/);
  assert.throws(() => projectSessionSelectionLocator("not-a-session"), /canonical navigation identity/);
});

test("project hover reveal waits only for one hidden exact action and fails closed on owner drift", () => {
  const locator = projectNewSessionLocator(projectId);
  const hidden = {
    count: 1,
    connected: true,
    visible: false,
    enabled: true,
    identity: locator.identity,
    hit_identity: { tag: "", action: null, focusKey: null },
    center_hit: false,
    center_in_viewport: true,
    center_in_scroll_clip: true,
    center: { x: 239, y: 208 },
    rect: { left: 225, top: 194, right: 253, bottom: 222, width: 28, height: 28 },
  };
  const beforeHover = hoveredProjectActionDecision(hidden, locator);
  assert.equal(beforeHover.decision, "pending", "the intentional no-hover opacity state is the only retryable state");
  assert.equal(beforeHover.error.code, "semantic-target-hidden");

  const visible = {
    ...hidden,
    visible: true,
    hit_identity: locator.identity,
    center_hit: true,
  };
  const revealed = hoveredProjectActionDecision(visible, locator);
  assert.equal(revealed.decision, "pass");
  assert.equal(revealed.acquired.identity.focusKey, `project:${projectId}:new-session`);

  const ambiguous = hoveredProjectActionDecision({ ...visible, count: 2 }, locator);
  assert.equal(ambiguous.decision, "fail");
  assert.equal(ambiguous.error.code, "semantic-target-cardinality");

  const wrongIdentity = {
    tag: "BUTTON",
    action: "delete-project",
    focusKey: `project:${projectId}:delete`,
  };
  const wrong = hoveredProjectActionDecision({
    ...visible,
    identity: wrongIdentity,
    hit_identity: wrongIdentity,
  }, locator);
  assert.equal(wrong.decision, "fail");
  assert.equal(wrong.error.code, "semantic-target-identity");

  const occluded = hoveredProjectActionDecision({ ...visible, center_hit: false }, locator);
  assert.equal(occluded.decision, "fail");
  assert.equal(occluded.error.code, "semantic-target-hit-test");
});

test("project-root navigation waits for absence and rejects duplicate semantic owners", () => {
  const exact = surface({ overrides: {
    projection: {
      ...surface().projection,
      overlay: "none",
      selected_project_index: 0,
      selected_session_index: 1,
      project_rows: [{ project_id: projectId }],
      session_rows: [
        { session_id: target.rootSessionId },
        { session_id: betaRootSessionId },
      ],
    },
    project_session_navigation: [
      {
        action: "session",
        focus_key: `session:${target.rootSessionId}:select`,
        aria_current: null,
        visible: true,
        enabled: true,
      },
      {
        action: "session",
        focus_key: `session:${betaRootSessionId}:select`,
        aria_current: "page",
        visible: true,
        enabled: true,
      },
    ],
    visible_dialog_count: 0,
  } });
  const expected = {
    projectId,
    rootSessionIds: [target.rootSessionId, betaRootSessionId],
    selectedRootSessionId: betaRootSessionId,
  };
  assert.equal(projectRootNavigationDecision(exact, expected), "pass");
  assert.equal(projectRootNavigationDecision({
    ...exact,
    projection: {
      ...exact.projection,
      session_rows: [{ session_id: betaRootSessionId }],
    },
    project_session_navigation: exact.project_session_navigation.slice(1),
  }, expected), "pending", "a temporarily absent prior root must not choose a different row");
  assert.equal(projectRootNavigationDecision({
    ...exact,
    project_session_navigation: [
      ...exact.project_session_navigation,
      { ...exact.project_session_navigation[0] },
    ],
  }, expected), "fail", "duplicate stable identities are ambiguous");
  assert.equal(projectRootNavigationDecision({
    ...exact,
    projection: { ...exact.projection, selected_project_index: -1 },
  }, expected), "pending", "quick-chat navigation is not the expected project owner");
});

test("Session Settings panel predicate binds root target, local badge, fields, and no global save", () => {
  assert.equal(SESSION_CONTEXT_BEFORE, "");
  assert.equal(sessionSettingsPanelReady(surface(), {
    expectedTarget: target,
    contextWindow: SESSION_CONTEXT_BEFORE,
    dirty: false,
    inherited: true,
  }), true);
  assert.equal(sessionSettingsPanelReady(surface({
    overrides: { panel: { ...surface().panel, scope_text: "グローバル" } },
  }), {
    expectedTarget: target,
    contextWindow: SESSION_CONTEXT_BEFORE,
    dirty: false,
    inherited: true,
  }), false);
  assert.equal(sessionSettingsPanelReady(surface({
    overrides: { panel: { ...surface().panel, save_global_count: 1 } },
  }), {
    contextWindow: SESSION_CONTEXT_BEFORE,
  }), false);
  assert.equal(sessionSettingsPanelReady(surface({
    overrides: {
      panel: {
        ...surface().panel,
        provider_profile: { ...surface().panel.provider_profile, options: [SESSION_PROVIDER_PROFILE] },
      },
    },
  }), {
    contextWindow: SESSION_CONTEXT_BEFORE,
  }), false, "Connection type requires the complete four-profile selector");
});

test("Session Settings rejects a visible inheritance badge for an applied session override", () => {
  const overridden = surface({ contextWindow: SESSION_CONTEXT_AFTER, inherited: false });
  const expected = { contextWindow: SESSION_CONTEXT_AFTER, inherited: false };
  assert.equal(sessionSettingsPanelReady(overridden, expected), true);
  overridden.panel.context_inherited_badge.visible = true;
  assert.equal(sessionSettingsPanelReady(overridden, expected), false);
  const inherited = surface();
  inherited.panel.context_inherited_badge.visible = false;
  assert.equal(sessionSettingsPanelReady(inherited, { contextWindow: SESSION_CONTEXT_BEFORE, inherited: true }), false);
});

test("dirty close guard preserves the exact root draft behind one inert panel", () => {
  const dirty = surface({ contextWindow: SESSION_CONTEXT_AFTER, dirty: true });
  const dirtyPanel = {
    expectedTarget: target,
    contextWindow: SESSION_CONTEXT_AFTER,
    dirty: true,
    inherited: true,
  };
  assert.equal(sessionSettingsPanelReady(dirty, dirtyPanel), true);
  const guarded = {
    ...dirty,
    panel: { ...dirty.panel, inert: true },
    confirmation: {
      count: 1,
      visible: true,
      cancel: { count: 1, enabled: true },
      discard_close: { count: 1, enabled: true },
    },
    visible_dialog_count: 2,
  };
  assert.equal(sessionSettingsDirtyGuardReady(guarded, target), true);
  assert.equal(
    sessionSettingsPanelReady(guarded, dirtyPanel),
    false,
    "the inert underlying panel is never a discard-action owner while confirmation is open",
  );
  assert.equal(sessionSettingsPanelReady({
    ...dirty,
    panel: {
      ...dirty.panel,
      discard: { count: 1, visible: false, enabled: false },
    },
  }, dirtyPanel), false, "cancel settlement requires the exact visible and enabled panel discard action");
  assert.equal(sessionSettingsPanelReady({
    ...dirty,
    panel: {
      ...dirty.panel,
      discard: { count: 2, visible: false, enabled: false },
    },
  }, dirtyPanel), false, "an ambiguous discard owner remains fail-closed");
  assert.equal(sessionSettingsDirtyGuardReady({
    ...guarded,
    confirmation: { ...guarded.confirmation, count: 2 },
  }, target), false, "an ambiguous confirmation owner remains fail-closed");
  assert.equal(sessionSettingsDirtyGuardReady({ ...guarded, panel: { ...guarded.panel, inert: false } }, target), false);
  assert.equal(sessionSettingsDirtyGuardReady({
    ...guarded,
    projection: {
      ...guarded.projection,
      session_settings: {
        ...guarded.projection.session_settings,
        target: { ...target, settingsRevision: "4" },
      },
    },
  }, target), false);
});

test("Session Settings apply command captures the complete host-neutral connection and exact target", () => {
  const applyable = surface({
    contextWindow: SESSION_CONTEXT_AFTER,
    dirty: true,
  });
  assert.deepEqual(expectedSessionSettingsApplyCommand(applyable), {
    command: "apply_session_settings",
    args: {
      input: {
        baseUrl: "http://127.0.0.1:43111",
        model: "e2e/scripted-responses",
        providerProfile: SESSION_PROVIDER_PROFILE,
        apiKeyEnv: SESSION_PROVIDER_API_KEY_ENV,
        accessMode: "default",
        contextWindow: SESSION_CONTEXT_AFTER,
      },
      expectedTarget: target,
    },
  });
  assert.equal(advancedSessionSettingsTarget({
    ...target,
    settingsRevision: "4",
    configGeneration: "10",
  }, target), true);
  assert.equal(advancedSessionSettingsTarget({
    ...target,
    rootSessionId: "01ARZ3NDEKTSV4RRFFQ69G5FAW",
    settingsRevision: "4",
  }, target), false);
});

test("completed root predicate requires exact ordered provider response and a durable target", () => {
  const completed = surface({ overrides: {
    projection: {
      ...surface().projection,
      overlay: "none",
      transcript_rows: [
        { row_kind: "user", body: "create root alpha" },
        { row_kind: "assistant", body: "ALPHA_OK" },
      ],
    },
    panel: { ...surface().panel, count: 0, visible: false },
    visible_dialog_count: 0,
  } });
  const ledger = [{
    method: "POST",
    pathname: "/v1/responses",
    route: "responses",
    contract: { pass: true },
    response_phase: "completed",
    response_status: 200,
  }];
  assert.equal(completedSessionRootReady(completed, ledger, {
    prompt: "create root alpha",
    response: "ALPHA_OK",
    responseCount: 1,
  }), true);
  assert.equal(completedSessionRootReady(completed, [...ledger, { ...ledger[0] }], {
    prompt: "create root alpha",
    response: "ALPHA_OK",
    responseCount: 1,
  }), false);
  assert.equal(completedSessionRootReady(completed, [...ledger, {
    method: "GET",
    pathname: "/v1/models",
    route: "models",
    response_status: 200,
  }], {
    prompt: "create root alpha",
    response: "ALPHA_OK",
    responseCount: 1,
  }), false, "unexpected catalog or diagnostic traffic must fail the Session Settings oracle");
  assert.equal(completedSessionRootReady({
    ...completed,
    triggers: {
      ...completed.triggers,
      model: { count: 0, visible: false, enabled: false, identity: null },
    },
  }, ledger, {
    prompt: "create root alpha",
    response: "ALPHA_OK",
    responseCount: 1,
  }), false, "Rust completion must not race ahead of the rendered Session Settings trigger");
  assert.equal(exactSessionProviderLedger(ledger, 1), true);
  assert.equal(exactSessionProviderLedger([...ledger, {
    method: "GET",
    pathname: "/ready",
    route: "docling_ready",
    response_status: 200,
  }], 1), false);
});

test("sealed Beta-shaped restart observation selects exact Alpha instead of blaming healthy triggers", () => {
  const sealedLikeBeta = restartedSurface({
    selectedRoot: betaRootSessionId,
    targetValue: betaTarget,
    overlay: "none",
    panelOpen: false,
    inherited: true,
  });
  const expectedNavigation = {
    projectId,
    rootSessionIds: restoredExpected.rootSessionIds,
    expectedRootSessionId: target.rootSessionId,
  };
  const selection = restartedSessionSelectionDecision(sealedLikeBeta, expectedNavigation);
  assert.equal(selection.status, "pass");
  assert.equal(selection.route, "select-exact-root");
  assert.equal(selection.selected.projection_root_session_id, betaRootSessionId);

  const prematureTrigger = restartedSessionSettingsTriggerDecision(sealedLikeBeta, expectedNavigation);
  assert.equal(prematureTrigger.status, "fail");
  assert.deepEqual(prematureTrigger.terminal_failures, [
    "restart-root-not-selected",
    "restart-session-settings-target-mismatch",
  ]);
  assert.equal(prematureTrigger.gates.find((gate) => gate.id === "model-trigger").pass, true);
  assert.equal(prematureTrigger.gates.find((gate) => gate.id === "access-trigger").pass, true);

  const inFlightTrigger = restartedSessionSettingsTriggerDecision(sealedLikeBeta, {
    ...expectedNavigation,
    afterProjectionRevision: "10",
  });
  assert.equal(inFlightTrigger.status, "pending", "the exact pre-click projection is an observable navigation transition baseline");
  assert.deepEqual(inFlightTrigger.terminal_failures, []);

  const selectedAlpha = restartedSurface({
    overlay: "none",
    panelOpen: false,
    projectionOverrides: { projection_revision: "11" },
  });
  const trigger = restartedSessionSettingsTriggerDecision(selectedAlpha, {
    ...expectedNavigation,
    afterProjectionRevision: "10",
  });
  assert.equal(trigger.status, "pass");
  assert.equal(trigger.accepted, true);

  const missingModel = restartedSessionSettingsTriggerDecision({
    ...selectedAlpha,
    triggers: {
      ...selectedAlpha.triggers,
      model: { count: 0, visible: false, enabled: false, identity: null },
    },
  }, expectedNavigation);
  assert.equal(missingModel.status, "pending", "an exact trigger may still be in a render transition");
  const duplicateModel = restartedSessionSettingsTriggerDecision({
    ...selectedAlpha,
    triggers: {
      ...selectedAlpha.triggers,
      model: { ...selectedAlpha.triggers.model, count: 2 },
    },
  }, expectedNavigation);
  assert.equal(duplicateModel.status, "fail");
  assert.ok(duplicateModel.terminal_failures.includes("restart-model-trigger-not-ready"));

  const duplicateAlpha = {
    ...sealedLikeBeta,
    projection: {
      ...sealedLikeBeta.projection,
      session_rows: [
        ...sealedLikeBeta.projection.session_rows,
        { session_id: target.rootSessionId },
      ],
    },
  };
  const ambiguous = restartedSessionSelectionDecision(duplicateAlpha, expectedNavigation);
  assert.equal(ambiguous.status, "fail");
  assert.ok(ambiguous.terminal_failures.includes("restart-root-owners-not-exact"));
});

test("restart open decision distinguishes render transition from wrong or duplicate command owners", () => {
  const opened = restartedSurface();
  const exact = restartedSessionSettingsOpenDecision({
    surface: opened,
    commandSnapshot: commandSnapshot("show_session_settings"),
  }, {
    expectedTargetRoot: target.rootSessionId,
    afterCommandSequence: 0,
  });
  assert.equal(exact.status, "pass");
  assert.deepEqual(exact.command.calls, [{
    sequence: 1,
    command: "show_session_settings",
    args: {},
  }]);

  const pending = restartedSessionSettingsOpenDecision({
    surface: restartedSurface({ overlay: "none", panelOpen: false }),
    commandSnapshot: commandSnapshot("show_session_settings", { count: 0 }),
  }, {
    expectedTargetRoot: target.rootSessionId,
    afterCommandSequence: 0,
  });
  assert.equal(pending.status, "pending");
  assert.ok(pending.failures.includes("restart-open-command-not-exact"));

  const wrongCommand = restartedSessionSettingsOpenDecision({
    surface: opened,
    commandSnapshot: commandSnapshot("close_overlay"),
  }, {
    expectedTargetRoot: target.rootSessionId,
    afterCommandSequence: 0,
  });
  assert.equal(wrongCommand.status, "fail");
  assert.deepEqual(wrongCommand.terminal_failures, ["restart-open-command-not-exact"]);

  const duplicatePanel = restartedSessionSettingsOpenDecision({
    surface: { ...opened, panel: { ...opened.panel, count: 2 } },
    commandSnapshot: commandSnapshot("show_session_settings"),
  }, {
    expectedTargetRoot: target.rootSessionId,
    afterCommandSequence: 0,
  });
  assert.equal(duplicatePanel.status, "fail");
  assert.ok(duplicatePanel.terminal_failures.includes("restart-session-settings-panel-not-open"));
});

test("restored root panel decision exposes every durable owner, value, control, and ledger gate", () => {
  const restored = restartedSurface();
  const ledger = acceptedLedger();
  const exact = restoredSessionSettingsPanelDecision({ surface: restored, ledger }, restoredExpected);
  assert.equal(exact.status, "pass");
  assert.equal(exact.gates.length, 32);
  assert.equal(exact.gates.every((gate) => gate.pass), true);
  assert.equal(exact.gates.find((gate) => gate.id === "target-owner").actual.target.configGeneration, "1");
  assert.equal(exact.gates.find((gate) => gate.id === "target-owner").actual.target.runtimeOwnerToken, "idle:0");
  const visibleInheritance = structuredClone(restored);
  visibleInheritance.panel.context_inherited_badge.visible = true;
  const wrongBadge = restoredSessionSettingsPanelDecision({ surface: visibleInheritance, ledger }, restoredExpected);
  assert.equal(wrongBadge.status, "fail");
  assert.ok(wrongBadge.terminal_failures.includes("restart-restored-inheritance-badge-mismatch"));

  const wrongRevision = restartedSurface({
    targetValue: { ...restoredTarget, settingsRevision: "5" },
  });
  const revisionDecision = restoredSessionSettingsPanelDecision({
    surface: wrongRevision,
    ledger,
  }, restoredExpected);
  assert.equal(revisionDecision.status, "fail");
  assert.ok(revisionDecision.terminal_failures.includes("restart-restored-target-owner-mismatch"));

  const extraTraffic = restoredSessionSettingsPanelDecision({
    surface: restored,
    ledger: [...ledger, {
      method: "GET",
      pathname: "/v1/models",
      route: "models",
      response_status: 200,
    }],
  }, restoredExpected);
  assert.equal(extraTraffic.status, "fail");
  assert.deepEqual(extraTraffic.terminal_failures, ["restart-provider-ledger-not-exact"]);

  const disabledModel = {
    ...restored,
    panel: {
      ...restored.panel,
      model: { ...restored.panel.model, enabled: false },
    },
  };
  const controlDecision = restoredSessionSettingsPanelDecision({
    surface: disabledModel,
    ledger,
  }, restoredExpected);
  assert.equal(controlDecision.status, "fail");
  assert.ok(controlDecision.terminal_failures.includes("restart-panel-model-mismatch"));

  const incompleteProfiles = {
    ...restored,
    panel: {
      ...restored.panel,
      provider_profile: {
        ...restored.panel.provider_profile,
        options: [SESSION_PROVIDER_PROFILE],
      },
    },
  };
  const profileDecision = restoredSessionSettingsPanelDecision({
    surface: incompleteProfiles,
    ledger,
  }, restoredExpected);
  assert.equal(profileDecision.status, "fail");
  assert.ok(profileDecision.terminal_failures.includes("restart-panel-provider-profile-mismatch"));

  const ambiguousBackdrop = restoredSessionSettingsPanelDecision({
    surface: { ...restored, visible_backdrop_count: 2 },
    ledger,
  }, restoredExpected);
  assert.equal(ambiguousBackdrop.status, "fail");
  assert.ok(ambiguousBackdrop.terminal_failures.includes("restart-restored-dialog-topology-invalid"));
});

test("restored root panel must remain canonical and stable", () => {
  const restored = restartedSurface();
  const times = [0, 250, 500];
  const decision = createStableRestoredSessionSettingsDecision({
    ...restoredExpected,
    minimumStableMs: 500,
    now: () => times.shift(),
  });
  const ledger = acceptedLedger();
  assert.equal(decision({ surface: restored, ledger }).status, "pending");
  assert.equal(decision({ surface: restored, ledger }).status, "pending");
  const stable = decision({ surface: restored, ledger });
  assert.equal(stable.status, "pass");
  assert.equal(stable.stable_for_ms, 500);
  const rejected = createStableRestoredSessionSettingsDecision({
    ...restoredExpected,
  });
  const irreversible = rejected({ surface: restored, ledger: [...ledger, {
    method: "GET",
    pathname: "/v1/models",
    route: "models",
    response_status: 200,
  }] });
  assert.equal(irreversible.status, "fail");
  assert.ok(irreversible.terminal_failures.includes("restart-provider-ledger-not-exact"));
  assert.ok(irreversible.failures.includes("restart-restored-panel-not-stable"));
});

test("restart close decision preserves Alpha and fails closed on command, error, or owner drift", () => {
  const ledger = acceptedLedger();
  const closed = restartedSurface({ overlay: "none", panelOpen: false });
  const exact = restartedSessionSettingsCloseDecision({
    surface: closed,
    commandSnapshot: commandSnapshot("close_overlay"),
    ledger,
  }, {
    ...restoredExpected,
    afterCommandSequence: 0,
  });
  assert.equal(exact.status, "pass");
  assert.equal(exact.selected.projection_root_session_id, target.rootSessionId);

  const pending = restartedSessionSettingsCloseDecision({
    surface: restartedSurface(),
    commandSnapshot: commandSnapshot("close_overlay", { count: 0 }),
    ledger,
  }, {
    ...restoredExpected,
    afterCommandSequence: 0,
  });
  assert.equal(pending.status, "pending");

  const duplicateCommand = restartedSessionSettingsCloseDecision({
    surface: closed,
    commandSnapshot: commandSnapshot("close_overlay", { count: 2 }),
    ledger,
  }, {
    ...restoredExpected,
    afterCommandSequence: 0,
  });
  assert.equal(duplicateCommand.status, "fail");
  assert.ok(duplicateCommand.terminal_failures.includes("restart-close-command-not-exact"));

  const betaClosed = restartedSurface({
    selectedRoot: betaRootSessionId,
    targetValue: betaTarget,
    overlay: "none",
    panelOpen: false,
    inherited: true,
  });
  const ownerDrift = restartedSessionSettingsCloseDecision({
    surface: betaClosed,
    commandSnapshot: commandSnapshot("close_overlay"),
    ledger,
  }, {
    ...restoredExpected,
    afterCommandSequence: 0,
  });
  assert.equal(ownerDrift.status, "fail");
  assert.ok(ownerDrift.terminal_failures.includes("restart-close-root-owner-drift"));
  assert.ok(ownerDrift.terminal_failures.includes("restart-close-target-owner-drift"));

  const errorSurface = {
    ...closed,
    visible_recoverable_error_count: 1,
  };
  const surfacedError = restartedSessionSettingsCloseDecision({
    surface: errorSurface,
    commandSnapshot: commandSnapshot("close_overlay"),
    ledger,
  }, {
    ...restoredExpected,
    afterCommandSequence: 0,
  });
  assert.equal(surfacedError.status, "fail");
  assert.ok(surfacedError.terminal_failures.includes("restart-close-surface-error"));
});

test("Session Settings scenario is a fresh common-runner contract", () => {
  const first = createSettingsSessionScenario();
  const second = createSettingsSessionScenario();
  assert.notEqual(first, second);
  assert.equal(first.id, "settings.session");
  assert.equal(first.databaseRequired, true);
  for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) {
    assert.equal(typeof first[method], "function", method);
  }
});

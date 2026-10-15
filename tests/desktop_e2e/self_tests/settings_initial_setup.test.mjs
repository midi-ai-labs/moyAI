import assert from "node:assert/strict";
import test from "node:test";
import { assertExactDesktopCommandSequence } from "../drivers/desktop_command_probe.mjs";

import {
  INITIAL_SETUP_HOST_OWNED_CONFIG_KEYS,
  INITIAL_SETUP_PROVIDER_API_KEY_ENV,
  INITIAL_SETUP_PROVIDER_PROFILE,
  INITIAL_SETUP_PROVIDER_PROFILE_OPTIONS,
  INITIAL_SETUP_IMPORT_GENERATION,
  INITIAL_SETUP_SECRET_SENTINEL,
  INITIAL_SETUP_STEPS,
  createSettingsInitialSetupScenario,
  createStableInitialSetupClosedDecision,
  expectedInitialSetupFinishCommand,
  importedSecretEditorReady,
  importedSecretStagedReady,
  initialSetupImportConfig,
  initialSetupImportedPublicOverrides,
  initialSetupImportedApproveAbsent,
  initialSetupStepReady,
  initialSetupHostingUnavailableReady,
  initialSetupPurposeButtonsReady,
} from "../scenarios/settings_initial_setup.mjs";

const workspace = "C:\\e2e\\workspace";
const setupTarget = Object.freeze({
  workspacePath: workspace,
  globalConfigPath: "C:\\e2e\\config\\config.toml",
  setupGeneration: "7",
});
const configTarget = Object.freeze({
  workspacePath: workspace,
  sessionId: null,
  configGeneration: "11",
});

function surface(step = "start", overrides = {}) {
  return {
    projection: {
      projection_revision: "91",
      workspace_path: workspace,
      overlay: "initial_setup",
      startup: {
        status: "requires_config",
        initial_setup_required: true,
        initial_setup_reason: "config_missing",
        action_overlay: "initial_setup",
        setup_target: { ...setupTarget },
      },
      config_target: { ...configTarget },
      config_fields: [
        { key: "model.base_url", value: "http://127.0.0.1:43111" },
        { key: "model.model", value: "e2e/scripted-responses" },
        { key: "model.provider_profile", value: INITIAL_SETUP_PROVIDER_PROFILE },
        { key: "model.api_key_env", value: INITIAL_SETUP_PROVIDER_API_KEY_ENV },
      ],
    },
    wizard: {
      count: 1,
      visible: true,
      current_step: step,
      rect: { left: 0, top: 0, width: 1440, height: 900 },
      step_rows: (step === "start" ? ["start"] : INITIAL_SETUP_STEPS).map((row) => ({
        step: row,
        visible: true,
        current: row === step ? "step" : null,
      })),
      next: { count: step === "finish" ? 0 : 1, visible: step !== "finish", enabled: step !== "finish", label_contained: true, label_line_count: 1 },
      back: { count: step === "start" ? 0 : 1, visible: step !== "start", enabled: step !== "start" },
      finish: { count: step === "finish" ? 1 : 0, visible: step === "finish", enabled: step === "finish", label_contained: true, label_line_count: 1 },
      import_config: { count: step === "start" ? 1 : 0, visible: step === "start", enabled: step === "start" },
      import_source: { count: step === "start" ? 1 : 0, visible: step === "start", text: "キャンセルした場合、現在のdraftは変わりません。" },
      provider: {
        profile: {
          count: step === "provider" ? 1 : 0,
          visible: step === "provider",
          enabled: step === "provider",
          value: step === "provider" ? INITIAL_SETUP_PROVIDER_PROFILE : null,
          options: step === "provider" ? [...INITIAL_SETUP_PROVIDER_PROFILE_OPTIONS] : [],
        },
        api_key_env: {
          count: step === "provider" ? 1 : 0,
          visible: step === "provider",
          enabled: step === "provider",
          value: step === "provider" ? INITIAL_SETUP_PROVIDER_API_KEY_ENV : null,
        },
      },
      host_owned_config_key_counts: Object.fromEntries(
        INITIAL_SETUP_HOST_OWNED_CONFIG_KEYS.map((key) => [key, 0]),
      ),
      sensitive_extra_headers: {
        count: 0,
        visible: false,
        value: null,
        configured: null,
        placeholder: null,
        status_text: null,
        status_visible: false,
      },
    },
    secret_exposure: { projection: false, dom: false },
    viewport: { width: 1440, height: 900 },
    visible_shell_count: 0,
    visible_dialog_count: 0,
    visible_backdrop_count: 0,
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
    visible_validation_error_count: 0,
    ...overrides,
  };
}

test("Initial Setup predicate requires the exact fullscreen welcome or settings steps and zero-network owner", () => {
  for (const step of INITIAL_SETUP_STEPS) {
    assert.equal(initialSetupStepReady(surface(step), [], step, workspace), true, step);
  }
  const staleWelcome = surface("start");
  staleWelcome.wizard.step_rows = INITIAL_SETUP_STEPS.map(step => ({ step, visible: true, current: step === "start" ? "step" : null }));
  assert.equal(initialSetupStepReady(staleWelcome, [], "start", workspace), false, "welcome must display only its current purpose step");
  assert.equal(initialSetupStepReady(surface("provider"), [{ pathname: "/v1/models" }], "provider", workspace), false);
  assert.equal(initialSetupStepReady(surface("provider", { visible_shell_count: 1 }), [], "provider", workspace), false);
  assert.equal(initialSetupStepReady(surface("provider", {
    wizard: { ...surface("provider").wizard, current_step: "model" },
  }), [], "provider", workspace), false);
  assert.equal(initialSetupStepReady(surface("provider", {
    projection: {
      ...surface("provider").projection,
      startup: { ...surface("provider").projection.startup, setup_target: { ...setupTarget, setupGeneration: "8" } },
    },
  }), [], "provider", workspace), true, "a fresh owner is accepted when all exact projected and DOM state agrees");
  assert.equal(initialSetupStepReady(surface("provider", {
    wizard: {
      ...surface("provider").wizard,
      provider: {
        ...surface("provider").wizard.provider,
        profile: { ...surface("provider").wizard.provider.profile, options: ["openai_responses"] },
      },
    },
  }), [], "provider", workspace), false, "the provider step requires the complete four-profile selector");
  assert.equal(initialSetupStepReady(surface("provider", {
    wizard: {
      ...surface("provider").wizard,
      provider: {
        ...surface("provider").wizard.provider,
        api_key_env: { ...surface("provider").wizard.provider.api_key_env, enabled: false },
      },
    },
  }), [], "provider", workspace), false, "the optional API-key env field remains directly editable");
  for (const key of INITIAL_SETUP_HOST_OWNED_CONFIG_KEYS) {
    assert.equal(initialSetupStepReady(surface("model", {
      wizard: {
        ...surface("model").wizard,
        host_owned_config_key_counts: {
          ...surface("model").wizard.host_owned_config_key_counts,
          [key]: 1,
        },
      },
    }), [], "model", workspace), false, `${key} must remain absent from Initial Setup`);
  }
});

test("Initial Setup rejects a visible enabled primary action with wrapped or overflowing text", () => {
  for (const [step, action] of [["start", "next"], ["finish", "finish"]]) {
    for (const change of [{ label_contained: false }, { label_line_count: 2 }]) {
      const candidate = surface(step);
      Object.assign(candidate.wizard[action], change);
      assert.equal(initialSetupStepReady(candidate, [], step, workspace), false);
    }
  }
});

test("missing Hub guidance must be normally visible while every initial purpose remains actionable", () => {
  const value = {
    surface: { projection: { overlay: "initial_setup", startup: { initial_setup_required: true, onboarding_intent: "hosting" } }, wizard: { current_step: "start" }, visible_fatal_count: 0 },
    notice: { visible: true, details_open: false, text: "Hub同梱版を導入するか、既存の管理PCでHubを起動してください。", purposes: ["personal", "team", "hosting"].map(kind => ({ action: `initial-setup-${kind}`, enabled: true })) },
  };
  assert.equal(initialSetupHostingUnavailableReady(value), true);
  assert.equal(initialSetupHostingUnavailableReady({ ...value, notice: { ...value.notice, text: "技術詳細を参照" } }), false);
  assert.equal(initialSetupHostingUnavailableReady({ ...value, notice: { ...value.notice, details_open: true } }), false);
  value.notice.purposes[0].enabled = false;
  assert.equal(initialSetupHostingUnavailableReady(value), false);
});

test("all three purpose labels fit the visible action without wrapping or clipping", () => {
  const button = { count: 1, visible: true, enabled: true, label_contained: true, label_line_count: 1 };
  const value = { wizard: { current_step: "start", purposes: [{ ...button }, { ...button }, { ...button }] } };
  assert.equal(initialSetupPurposeButtonsReady(value), true);
  value.wizard.purposes[0].label_line_count = 6;
  assert.equal(initialSetupPurposeButtonsReady(value), false);
  value.wizard.purposes[0].label_line_count = 1;
  value.wizard.purposes[0].label_contained = false;
  assert.equal(initialSetupPurposeButtonsReady(value), false);
});

test("Initial Setup Finish expectation carries ordinary values and both exact targets", () => {
  assert.deepEqual(expectedInitialSetupFinishCommand(surface("finish")), {
    command: "finish_initial_setup",
    args: {
      values: [
        { key: "model.base_url", text: "http://127.0.0.1:43111" },
        { key: "model.model", text: "e2e/scripted-responses" },
        { key: "model.provider_profile", text: INITIAL_SETUP_PROVIDER_PROFILE },
      ],
      expectedConfigTarget: configTarget,
      expectedSetupTarget: setupTarget,
      importGeneration: null,
    },
  });
  assert.equal(
    expectedInitialSetupFinishCommand(surface("finish"), INITIAL_SETUP_IMPORT_GENERATION).args.importGeneration,
    INITIAL_SETUP_IMPORT_GENERATION,
  );
  const importedPublic = initialSetupImportedPublicOverrides("http://127.0.0.1:43111");
  const beforeImport = surface("finish");
  beforeImport.projection.config_fields = beforeImport.projection.config_fields.map(field =>
    field.key === "model.api_key_env" ? { ...field, value: "" } : field);
  const imported = expectedInitialSetupFinishCommand(
    beforeImport,
    INITIAL_SETUP_IMPORT_GENERATION,
    {
      "model.base_url": importedPublic["model.base_url"],
      "model.model": importedPublic["model.model"],
      "model.provider_profile": importedPublic["model.provider_profile"],
      "model.api_key_env": importedPublic["model.api_key_env"],
    },
  );
  assert.equal(
    imported.args.values.find((field) => field.key === "model.api_key_env")?.text,
    INITIAL_SETUP_PROVIDER_API_KEY_ENV,
  );
  assert.equal(importedPublic["docling.enabled"], "false");
  assert.throws(
    () => expectedInitialSetupFinishCommand({ projection: { config_target: configTarget, startup: { setup_target: null } } }),
    /both exact mutation targets/,
  );
});

test("Initial Setup import keeps a configured secret blank across public projection and editor state", () => {
  const sourcePath = "C:\\e2e\\workspace\\E2E_INITIAL_SETUP_IMPORT.toml";
  const importedStart = surface("start", {
    projection: {
      ...surface("start").projection,
      config_fields: [
        ...surface("start").projection.config_fields,
        { key: "model.extra_headers_json", value: "", sensitive: true, configured: false },
      ],
    },
    wizard: {
      ...surface("start").wizard,
      import_source: { count: 1, visible: true, text: `読込元: ${sourcePath}。内容はまだ保存されていません。` },
    },
  });
  assert.equal(importedSecretStagedReady(importedStart, sourcePath), true);
  assert.equal(JSON.stringify(importedStart).includes(INITIAL_SETUP_SECRET_SENTINEL), false);

  const editor = surface("model", {
    wizard: {
      ...surface("model").wizard,
      sensitive_extra_headers: {
        count: 1,
        visible: true,
        value: "",
        configured: "true",
        placeholder: "設定済み（値は非表示）",
        status_text: "設定済み・値は非表示",
        status_visible: true,
      },
    },
  });
  assert.equal(importedSecretEditorReady(editor), true);
  const fixture = initialSetupImportConfig("http://127.0.0.1:43111");
  assert.equal((fixture.match(new RegExp(INITIAL_SETUP_SECRET_SENTINEL, "g")) ?? []).length, 1);
});

test("Main-only import sends inherited Approve differences while omitting unchanged Approve fields", () => {
  const baseUrl = "http://127.0.0.1:43111";
  const importedPublic = initialSetupImportedPublicOverrides(baseUrl);
  const beforeImport = surface("finish");
  const savedApprove = {
    "approve.base_url": baseUrl,
    "approve.model": "e2e/scripted-responses",
    "approve.provider_profile": INITIAL_SETUP_PROVIDER_PROFILE,
    "approve.api_key_env": "",
    "approve.context_window": "65536",
    "approve.request_timeout_ms": "3600000",
    "approve.connect_timeout_ms": "10000",
    "approve.max_retries": "2",
  };
  beforeImport.projection.config_fields = Object.entries(importedPublic).map(([key, text]) => ({
    key,
    value: Object.hasOwn(savedApprove, key) ? savedApprove[key] : text,
  }));
  const command = expectedInitialSetupFinishCommand(beforeImport, INITIAL_SETUP_IMPORT_GENERATION, importedPublic);
  assert.deepEqual(command.args.values.filter(field => field.key.startsWith("approve.")), [
    { key: "approve.api_key_env", text: INITIAL_SETUP_PROVIDER_API_KEY_ENV },
    { key: "approve.request_timeout_ms", text: "120000" },
    { key: "approve.max_retries", text: "0" },
  ]);
  for (const suffix of ["base_url", "model", "provider_profile", "api_key_env", "context_window", "request_timeout_ms", "connect_timeout_ms", "max_retries"]) {
    assert.equal(importedPublic[`approve.${suffix}`], importedPublic[`model.${suffix}`]);
  }
  const snapshot = { found: true, sequence: 1, dropped_through: 0, calls: [{ sequence: 1, ...command }] };
  assertExactDesktopCommandSequence(snapshot, { expected: [command] });
  const obsolete = structuredClone(command);
  obsolete.args.values = obsolete.args.values.filter(field => !field.key.startsWith("approve."));
  assert.throws(() => assertExactDesktopCommandSequence(snapshot, { expected: [obsolete] }), { code: "desktop-command-probe-call-mismatch" });
  const completeApprove = structuredClone(command);
  completeApprove.args.values.push({ key: "approve.model", text: "e2e/scripted-responses" });
  assert.throws(() => assertExactDesktopCommandSequence(snapshot, { expected: [completeApprove] }), { code: "desktop-command-probe-call-mismatch" });
  assert.equal(JSON.stringify(command).includes(INITIAL_SETUP_SECRET_SENTINEL), false);
});

test("Main-only import requires the absent Approve role in both saved TOML and reopened public state", () => {
  const persistedText = initialSetupImportConfig("http://127.0.0.1:43111");
  assert.equal(initialSetupImportedApproveAbsent({ approve_model_configured: false }, persistedText), true);
  assert.equal(initialSetupImportedApproveAbsent({ approve_model_configured: true }, persistedText), false);
  assert.equal(initialSetupImportedApproveAbsent({}, persistedText), false);
  assert.equal(initialSetupImportedApproveAbsent({ approve_model_configured: false }, `${persistedText}\n[approve]\nmodel = "guardian"\n`), false);
  assert.equal(initialSetupImportedApproveAbsent({ approve_model_configured: false }, `${persistedText}\n[approve.extra_headers]\nAuthorization = "example"\n`), false);
});

test("Initial Setup restart predicate requires continuous closed wizard and zero network", () => {
  const closed = surface("finish", {
    projection: {
      ...surface("finish").projection,
      overlay: "none",
      startup: {
        status: "ready",
        initial_setup_required: false,
        initial_setup_reason: null,
        action_overlay: "none",
        setup_target: null,
      },
    },
    wizard: { ...surface("finish").wizard, count: 0, visible: false },
    visible_shell_count: 1,
  });
  const times = [0, 250, 500];
  const decision = createStableInitialSetupClosedDecision({
    expectedWorkspace: workspace,
    minimumStableMs: 500,
    now: () => times.shift(),
  });
  assert.equal(decision({ surface: closed, ledger: [] }), "pending");
  assert.equal(decision({ surface: closed, ledger: [] }), "pending");
  assert.equal(decision({ surface: closed, ledger: [] }), "pass");
  const failing = createStableInitialSetupClosedDecision({ expectedWorkspace: workspace });
  assert.equal(failing({ surface: closed, ledger: [{ pathname: "/ready" }] }), "fail");
});

test("Initial Setup scenario is a fresh common-runner contract with late-bound safe environment", () => {
  const first = createSettingsInitialSetupScenario();
  const second = createSettingsInitialSetupScenario();
  assert.notEqual(first, second);
  assert.equal(first.id, "settings.initial-setup");
  assert.deepEqual(first.environment, {});
  for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) {
    assert.equal(typeof first[method], "function", method);
  }
});

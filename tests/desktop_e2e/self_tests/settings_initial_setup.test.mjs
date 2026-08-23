import assert from "node:assert/strict";
import test from "node:test";

import {
  INITIAL_SETUP_PROVIDER_API_KEY_ENV,
  INITIAL_SETUP_PROVIDER_PROFILE,
  INITIAL_SETUP_PROVIDER_PROFILE_OPTIONS,
  INITIAL_SETUP_STEPS,
  createSettingsInitialSetupScenario,
  createStableInitialSetupClosedDecision,
  expectedInitialSetupFinishCommand,
  initialSetupStepReady,
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
      step_rows: INITIAL_SETUP_STEPS.map((row) => ({
        step: row,
        visible: true,
        current: row === step ? "step" : null,
      })),
      next: { count: step === "finish" ? 0 : 1, visible: step !== "finish", enabled: step !== "finish" },
      back: { count: step === "start" ? 0 : 1, visible: step !== "start", enabled: step !== "start" },
      finish: { count: step === "finish" ? 1 : 0, visible: step === "finish", enabled: step === "finish" },
      import_config: { count: step === "start" ? 1 : 0, visible: step === "start", enabled: step === "start" },
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
    },
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

test("Initial Setup predicate requires the exact fullscreen six-step zero-network owner", () => {
  for (const step of INITIAL_SETUP_STEPS) {
    assert.equal(initialSetupStepReady(surface(step), [], step, workspace), true, step);
  }
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
});

test("Initial Setup Finish expectation carries all values and both exact targets", () => {
  assert.deepEqual(expectedInitialSetupFinishCommand(surface("finish")), {
    command: "finish_initial_setup",
    args: {
      values: [
        { key: "model.base_url", text: "http://127.0.0.1:43111" },
        { key: "model.model", text: "e2e/scripted-responses" },
        { key: "model.provider_profile", text: INITIAL_SETUP_PROVIDER_PROFILE },
        { key: "model.api_key_env", text: INITIAL_SETUP_PROVIDER_API_KEY_ENV },
      ],
      expectedConfigTarget: configTarget,
      expectedSetupTarget: setupTarget,
    },
  });
  assert.throws(
    () => expectedInitialSetupFinishCommand({ projection: { config_target: configTarget, startup: { setup_target: null } } }),
    /both exact mutation targets/,
  );
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

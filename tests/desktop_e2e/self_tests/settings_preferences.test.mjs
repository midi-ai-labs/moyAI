import assert from "node:assert/strict";
import test from "node:test";

import {
  PROVIDER_CONTEXT_AFTER,
  PROVIDER_CONTEXT_BEFORE,
  PROVIDER_PROFILE_OPTIONS,
  SETTINGS_PROVIDER_API_KEY_ENV,
  SETTINGS_PROVIDER_PROFILE,
  createSettingsPreferencesScenario,
  createStablePreferencesDecision,
  dirtyCloseGuardReady,
  dirtyDoclingPreferencesReady,
  expectedGlobalSave,
  expectedProviderGlobalSave,
  expectedResetThenClose,
  preferencesReady,
  providerEditorReady,
  savedPreferencesReady,
  settingsTriggerRestoredShellReady,
  settingsPreferencesFixtureConfig,
  shellReadyForSettingsDrag,
  trustedClickProbeEvents,
} from "../scenarios/settings_preferences.mjs";

const target = Object.freeze({
  workspacePath: "C:\\workspace",
  sessionId: null,
  configGeneration: "41",
});

function projection(overrides = {}) {
  return {
    overlay: "config",
    config_target: { ...target },
    config_fields: [
      { key: "model.base_url", value: "http://127.0.0.1:43111" },
      { key: "model.model", value: "moyai-e2e-scripted" },
      { key: "model.provider_profile", value: SETTINGS_PROVIDER_PROFILE },
      { key: "model.api_key_env", value: SETTINGS_PROVIDER_API_KEY_ENV },
      { key: "model.context_window", value: PROVIDER_CONTEXT_AFTER },
      { key: "model.max_output_tokens", value: "1024" },
      { key: "docling.enabled", value: "false" },
      { key: "docling.base_url", value: "http://127.0.0.1:43111" },
    ],
    provider_base_url: "http://127.0.0.1:43111",
    provider_profile: SETTINGS_PROVIDER_PROFILE,
    provider_api_key_env: SETTINGS_PROVIDER_API_KEY_ENV,
    provider_context_window: PROVIDER_CONTEXT_BEFORE,
    provider_max_output_tokens: "1024",
    provider_model_ids: ["moyai-e2e-scripted"],
    provider_selected_index: 0,
    ...overrides,
  };
}

function cleanSurface(overrides = {}) {
  return {
    projection: projection(),
    settings: {
      dialog_count: 1,
      dialog_visible: true,
      profile: { count: 1, visible: true, enabled: true, value: SETTINGS_PROVIDER_PROFILE, options: [...PROVIDER_PROFILE_OPTIONS] },
      api_key_env: { count: 1, visible: true, enabled: true, value: SETTINGS_PROVIDER_API_KEY_ENV },
      context: { count: 1, value: PROVIDER_CONTEXT_AFTER },
      docling: { count: 1, checked: false },
      docling_label: { count: 1, visible: false, text: "Docling を有効化" },
      dirty_badge_visible: false,
      save: { count: 1, visible: true, enabled: false },
      discard: { count: 0, visible: false, enabled: false },
      close: { count: 1, visible: true, enabled: true },
    },
    close_confirmation: {
      count: 0,
      visible: false,
      cancel: { count: 0, enabled: false },
      discard_close: { count: 0, enabled: false },
    },
    titlebar: {
      drag_count: 1,
      drag_visible: true,
      drag_rect: { left: 240, top: 0, width: 500, height: 32 },
    },
    visible_dialog_count: 1,
    visible_backdrop_count: 1,
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
    visible_validation_error_count: 0,
    ...overrides,
  };
}

function dirtySurface(overrides = {}) {
  const clean = cleanSurface();
  return {
    ...clean,
    settings: {
      ...clean.settings,
      docling: { count: 1, checked: true },
      docling_label: { count: 1, visible: true, text: "Docling を有効化" },
      dirty_badge_visible: true,
      save: { count: 1, visible: true, enabled: true },
      discard: { count: 1, visible: true, enabled: true },
    },
    ...overrides,
  };
}

test("fixture binds provider and Docling to one loopback zero-request ledger", () => {
  const config = settingsPreferencesFixtureConfig("http://127.0.0.1:43111");
  assert.equal((config.match(/base_url = "http:\/\/127\.0\.0\.1:43111"/g) ?? []).length, 2);
  assert.match(config, /context_window = 65536/);
  assert.match(config, /provider_profile = "openai_responses"/);
  assert.doesNotMatch(config, /provider_(?:metadata|api)_mode/);
  assert.match(config, /\[docling\]\nenabled = false/);
  assert.match(config, /timeout_ms = 1000/);
});

test("Docling label activation includes the browser-forwarded trusted checkbox click", () => {
  const expected = trustedClickProbeEvents({
    identity: { tag: "LABEL", configKey: "docling.enabled" },
    forwardedClickIdentity: { tag: "INPUT", configKey: "docling.enabled" },
  });
  assert.deepEqual(expected.map((event) => [event.type, event.identity.tag, event.identity.configKey]), [
    ["pointerdown", "LABEL", "docling.enabled"],
    ["pointerup", "LABEL", "docling.enabled"],
    ["click", "LABEL", "docling.enabled"],
    ["click", "INPUT", "docling.enabled"],
  ]);
});

test("shell, provider, and clean Preferences predicates reject network and visible errors", () => {
  const shell = cleanSurface({
    projection: projection({ overlay: "none" }),
    visible_dialog_count: 0,
    visible_backdrop_count: 0,
  });
  assert.equal(shellReadyForSettingsDrag(shell, []), true);
  assert.equal(shellReadyForSettingsDrag(shell, [{ method: "GET" }]), false);
  assert.equal(shellReadyForSettingsDrag({ ...shell, visible_fatal_count: 1 }, []), false);

  const provider = cleanSurface({
    projection: projection({ overlay: "provider" }),
    provider: {
      dialog_count: 1,
      dialog_visible: true,
      profile: { count: 1, visible: true, enabled: true, value: SETTINGS_PROVIDER_PROFILE, options: [...PROVIDER_PROFILE_OPTIONS] },
      api_key_env: { count: 1, visible: true, enabled: true, value: SETTINGS_PROVIDER_API_KEY_ENV },
      context: { count: 1, visible: true, enabled: true, value: PROVIDER_CONTEXT_BEFORE },
      save: { count: 1, visible: true, enabled: true },
      load_models: { count: 1, visible: true, enabled: true },
      close: { count: 1, visible: true, enabled: true },
    },
  });
  assert.equal(providerEditorReady(provider, []), true);
  assert.equal(providerEditorReady({
    ...provider,
    provider: { ...provider.provider, profile: { ...provider.provider.profile, options: ["openai_responses"] } },
  }, []), false);
  assert.equal(providerEditorReady({ ...provider, visible_recoverable_error_count: 1 }, []), false);

  const settings = cleanSurface();
  assert.equal(preferencesReady(settings, [], { contextWindow: PROVIDER_CONTEXT_AFTER, doclingEnabled: false }), true);
  assert.equal(preferencesReady({ ...settings, visible_dialog_count: 2 }, [], {
    contextWindow: PROVIDER_CONTEXT_AFTER,
    doclingEnabled: false,
  }), false);
});

test("dirty close predicates preserve the exact target and distinguish guard from ordinary dirty state", () => {
  const dirty = dirtySurface();
  assert.equal(dirtyDoclingPreferencesReady(dirty, [], target), true);
  assert.equal(dirtyDoclingPreferencesReady(dirty, [], { ...target, configGeneration: "42" }), false);
  assert.equal(dirtyCloseGuardReady(dirty, [], target), false);

  const guarded = {
    ...dirty,
    settings: { ...dirty.settings, dialog_inert: true },
    close_confirmation: {
      count: 1,
      visible: true,
      cancel: { count: 1, enabled: true },
      discard_close: { count: 1, enabled: true },
    },
    visible_dialog_count: 2,
  };
  assert.equal(dirtyCloseGuardReady(guarded, [], target), true);
  assert.equal(dirtyCloseGuardReady({ ...guarded, settings: { ...guarded.settings, dialog_inert: false } }, [], target), false);
  assert.equal(dirtyCloseGuardReady({ ...guarded, visible_validation_error_count: 1 }, [], target), false);

  const restoredShell = {
    ...cleanSurface({
      projection: projection({ overlay: "none" }),
      visible_dialog_count: 0,
      visible_backdrop_count: 0,
    }),
    active: { tag: "BUTTON", action: "show-config" },
  };
  assert.equal(settingsTriggerRestoredShellReady(restoredShell, []), true);
  assert.equal(settingsTriggerRestoredShellReady({ ...restoredShell, active: { tag: "BODY", action: null } }, []), false);
});

test("command expectations preserve complete ordered values and exact config targets", () => {
  const providerSurface = cleanSurface({ projection: projection({ overlay: "provider" }) });
  const providerCommand = expectedProviderGlobalSave(providerSurface);
  assert.equal(providerCommand.command, "save_provider_global");
  assert.equal(providerCommand.args.input.contextWindow, PROVIDER_CONTEXT_AFTER);
  assert.equal(providerCommand.args.input.providerProfile, SETTINGS_PROVIDER_PROFILE);
  assert.equal(providerCommand.args.input.apiKeyEnv, SETTINGS_PROVIDER_API_KEY_ENV);
  assert.equal(Object.hasOwn(providerCommand.args.input, "metadataMode"), false);
  assert.deepEqual(providerCommand.args.expectedTarget, target);
  assert.equal(providerCommand.args.draftValues.find((row) => row.key === "docling.enabled").text, "false");

  const dirty = dirtySurface();
  const reset = expectedResetThenClose(dirty);
  assert.deepEqual(reset.map((row) => row.command), ["reset_config_draft", "close_overlay"]);
  assert.equal(reset[0].args.values.find((row) => row.key === "docling.enabled").text, "false");
  assert.deepEqual(reset[0].args.expectedTarget, target);

  const save = expectedGlobalSave(dirty);
  assert.equal(save.command, "save_global_config");
  assert.equal(save.args.values.find((row) => row.key === "docling.enabled").text, "true");
  assert.deepEqual(save.args.expectedTarget, target);
});

test("saved and restored predicates require a generation advance, clean state, and continuous zero-network stability", () => {
  const saved = cleanSurface({
    projection: projection({
      config_target: { ...target, configGeneration: "42" },
      config_fields: projection().config_fields.map((field) => field.key === "docling.enabled" ? { ...field, value: "true" } : field),
    }),
    settings: {
      ...cleanSurface().settings,
      docling: { count: 1, checked: true },
    },
  });
  assert.equal(savedPreferencesReady(saved, [], target), true);
  assert.equal(savedPreferencesReady({ ...saved, projection: projection() }, [], target), false);

  const times = [0, 200, 500];
  const decide = createStablePreferencesDecision({ now: () => times.shift(), minimumStableMs: 500 });
  assert.equal(decide({ surface: saved, ledger: [] }), "pending");
  assert.equal(decide({ surface: saved, ledger: [] }), "pending");
  assert.equal(decide({ surface: saved, ledger: [] }), "pass");
  const failing = createStablePreferencesDecision({ now: () => 0 });
  assert.equal(failing({ surface: saved, ledger: [{ pathname: "/v1/models" }] }), "fail");
});

test("scenario factory returns fresh common-runner contracts", () => {
  const first = createSettingsPreferencesScenario();
  const second = createSettingsPreferencesScenario();
  assert.notEqual(first, second);
  assert.equal(first.id, "settings.preferences");
  assert.equal(first.databaseRequired, true);
  for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) {
    assert.equal(typeof first[method], "function");
  }
});

import assert from "node:assert/strict";
import test from "node:test";

import {
  PROVIDER_CONTEXT_AFTER,
  PROVIDER_CONTEXT_BEFORE,
  PROVIDER_PROFILE_OPTIONS,
  MAIN_SYSTEM_PROMPT_MARKER,
  SETTINGS_PROVIDER_API_KEY_ENV,
  SETTINGS_PROVIDER_PROFILE,
  createSettingsPreferencesScenario,
  createSettingsPreferencesConfigScenario,
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
  shellReadyForPreferences,
  smallSettingsEditorWithinViewport,
  settingsToggleLabelAssociated,
  tabToSettingsControl,
  trustedClickProbeEvents,
  trustedReplaceFocusedSettingsDigits,
  trustedToggleFocusedSettingsCheckbox,
  trustedTypeFocusedSettingsText,
} from "../scenarios/settings_preferences.mjs";

test("category navigation requires complete compact editors inside the actual scroll viewport", () => {
  const editor = {
    count: 1, connected: true, visible: true, enabled: true,
    identity: { tag: "INPUT", configKey: "model.context_window" },
    rect: { left: 100, top: 120, right: 240, bottom: 160, width: 140, height: 40 },
    scroll_clip: { left: 90, top: 100, right: 700, bottom: 400 },
    viewport: { width: 800, height: 600 },
    center_in_viewport: true, center_in_scroll_clip: true, center_hit: true,
  };
  const ready = (observation, key = "model.context_window") => smallSettingsEditorWithinViewport({ observation }, key);
  assert.equal(ready(editor), true);
  for (const rect of [
    { left: 89 }, { top: 99 }, { right: 701 }, { bottom: 401 },
    { top: 390, bottom: 430 }, // Center/hit flags alone cannot prove the entire input is visible.
    { top: 700, bottom: 740 }, { height: 0 }, { left: NaN },
  ]) assert.equal(ready({ ...editor, rect: { ...editor.rect, ...rect } }), false);
  for (const invalid of [
    { count: 0 }, { count: 2 }, { connected: false }, { visible: false }, { enabled: false },
    { identity: { tag: "TEXTAREA", configKey: "model.context_window" } },
    { identity: { tag: "INPUT", configKey: "other" } },
    { viewport: { width: 220, height: 600 } }, { viewport: { width: 800, height: 150 } },
    { scroll_clip: null },
  ]) assert.equal(ready({ ...editor, ...invalid }), false);
  assert.equal(ready({ ...editor, center_hit: false }), true, "visibility is not a pointer-center hit-test");
  assert.equal(ready({ ...editor, identity: { tag: "INPUT", configKey: "docling.enabled" } }, "docling.enabled"), true);
  const visualLabel = { observation: { ...editor, identity: { tag: "LABEL", configKey: "docling.enabled" } } };
  assert.equal(smallSettingsEditorWithinViewport(visualLabel, "docling.enabled", "LABEL"), true);
  assert.equal(smallSettingsEditorWithinViewport(visualLabel, "docling.enabled"), false);
  assert.equal(smallSettingsEditorWithinViewport(visualLabel, "docling.enabled", "TEXTAREA"), false);
  assert.equal(smallSettingsEditorWithinViewport(null, "model.context_window"), false);
});

test("custom toggle visibility binds its unique label to the enabled semantic checkbox", () => {
  const label = { count: 1, config_key: "docling.enabled", associated_input: true };
  const checkbox = { count: 1, config_key: "docling.enabled", type: "checkbox", enabled: true, checked: false, visible: false };
  const accepted = (labelValue = label, checkboxValue = checkbox) => settingsToggleLabelAssociated(labelValue, checkboxValue, "docling.enabled");
  assert.equal(accepted(), true, "a CSS-hidden native input is represented by its visible associated label");
  assert.equal(accepted(label, { ...checkbox, checked: true }), true);
  for (const invalid of [{ count: 0 }, { count: 2 }, { config_key: "other" }, { associated_input: false }]) {
    assert.equal(accepted({ ...label, ...invalid }), false);
  }
  for (const invalid of [{ count: 0 }, { count: 2 }, { config_key: "other" }, { type: "text" }, { enabled: false }, { checked: null }]) {
    assert.equal(accepted(label, { ...checkbox, ...invalid }), false);
  }
});

test("Settings reaches an editor with trusted Tab before keyboard typing", async () => {
  const locator = { selector: "textarea", identity: { tag: "TEXTAREA", configKey: "model.system_prompt" } };
  const ready = { count: 1, available: true, focus_in_dialog: true, focused: false };
  function fixture(observations, untrusted = false) {
    let sequence = 0;
    const events = [], keys = [];
    return {
      keys,
      cdp: { evaluate: async () => { assert.ok(observations.length); return observations.shift(); } },
      input: {
        snapshotProbe: async (after = 0) => ({ found: true, sequence, dropped_through: 0, events: events.filter(e => e.sequence > after) }),
        pressKey: async (key) => {
          keys.push(key);
          for (const type of ["keydown", "keyup"]) events.push({ sequence: ++sequence, type, key, code: key, isTrusted: !untrusted });
        },
      },
    };
  }
  const successful = fixture([{ ...ready }, { ...ready }, { ...ready, focused: true }]);
  const result = await tabToSettingsControl(successful.input, successful.cdp, locator);
  assert.equal(result.steps, 2);
  assert.deepEqual(successful.keys, ["Tab", "Tab"]);
  assert.equal(result.probe.events.length, 4);
  const focused = fixture([{ ...ready, focused: true }]);
  assert.equal((await tabToSettingsControl(focused.input, focused.cdp, locator)).steps, 0);
  assert.deepEqual(focused.keys, []);
  for (const invalid of [{ count: 0 }, { count: 2 }, { available: false }, { focus_in_dialog: false }]) {
    const unavailable = fixture([{ ...ready, ...invalid }]);
    await assert.rejects(tabToSettingsControl(unavailable.input, unavailable.cdp, locator), error => error.code === "settings-tab-target-unavailable");
    assert.deepEqual(unavailable.keys, []);
  }
  const unreachable = fixture([{ ...ready }, { ...ready }, { ...ready }]);
  await assert.rejects(tabToSettingsControl(unreachable.input, unreachable.cdp, locator, { maxSteps: 2 }), error => error.code === "settings-tab-target-unreachable");
  assert.deepEqual(unreachable.keys, ["Tab", "Tab"]);
  const synthetic = fixture([{ ...ready }, { ...ready, focused: true }], true);
  await assert.rejects(tabToSettingsControl(synthetic.input, synthetic.cdp, locator), error => error.code === "event-probe-untrusted");
});

test("Settings keyboard typing binds every character to the already-focused textarea without a pointer", async () => {
  const identity = { tag: "TEXTAREA", configKey: "model.system_prompt" };
  const locator = { selector: "textarea", identity };
  function fixture({ active = identity, changeEvents = () => {} } = {}) {
    const calls = [], events = [];
    return { calls, input: {
      snapshotProbe: async () => ({ found: true, sequence: events.length, dropped_through: 0, active, events }),
      typeText: async (text) => {
        calls.push(text);
        for (const character of text) {
          events.push({ sequence: events.length + 1, isTrusted: true, ...identity, type: "keydown", key: character });
          events.push({ sequence: events.length + 1, isTrusted: true, ...identity, type: "input", inputType: "insertText", data: character });
          events.push({ sequence: events.length + 1, isTrusted: true, ...identity, type: "keyup", key: character });
        }
        changeEvents(events);
        return { text, character_count: text.length };
      },
    } };
  }
  const accepted = fixture();
  const typed = await trustedTypeFocusedSettingsText(accepted.input, locator, "e2e-prompt");
  assert.deepEqual(accepted.calls, ["e2e-prompt"]);
  assert.equal(typed.probe.events.length, 30);
  const wrongFocus = fixture({ active: { ...identity, configKey: "model.api_key_env" } });
  await assert.rejects(trustedTypeFocusedSettingsText(wrongFocus.input, locator, "e2e-prompt"), error => error.code === "settings-text-focus-owner");
  assert.deepEqual(wrongFocus.calls, []);
  for (const changeEvents of [
    events => { events[1].configKey = "model.api_key_env"; },
    events => { events[1].isTrusted = false; },
    events => { events[1].data = "other"; },
    events => { events.pop(); },
  ]) {
    const invalid = fixture({ changeEvents });
    await assert.rejects(trustedTypeFocusedSettingsText(invalid.input, locator, "e2e-prompt"));
    assert.deepEqual(invalid.calls, ["e2e-prompt"], "failed delivery is not retried");
  }
});

test("remaining Settings controls use focused keyboard replacement and one native checkbox activation", async () => {
  const contextIdentity = { tag: "INPUT", configKey: "model.context_window" };
  const checkboxIdentity = { tag: "INPUT", configKey: "docling.enabled" };
  const keyEvent = (type, key, code) => ({ type, key, code });
  const digits = [
    keyEvent("keydown", "Control", "ControlLeft"), keyEvent("keydown", "a", "KeyA"),
    keyEvent("keyup", "a", "KeyA"), keyEvent("keyup", "Control", "ControlLeft"),
    ...Array.from("65537").flatMap(key => [keyEvent("keydown", key, `Digit${key}`),
      { type: "input", inputType: "insertText", data: key }, keyEvent("keyup", key, `Digit${key}`)]),
  ];
  const checkbox = [keyEvent("keydown", " ", "Space"), keyEvent("keyup", " ", "Space"),
    { type: "click" }, { type: "input" }, { type: "change" }];
  function fixture(identity, eventRows, active = identity) {
    const calls = [];
    return { calls, input: {
      snapshotProbe: async () => ({ found: true, sequence: calls.length ? eventRows.length : 0, dropped_through: 0,
        active, events: calls.length ? eventRows.map((row, index) => ({ sequence: index + 1, isTrusted: true, ...identity, ...row })) : [] }),
      keyDown: async key => { calls.push(["down", key]); },
      pressKey: async key => { calls.push(["press", key]); },
      keyUp: async key => { calls.push(["up", key]); },
      typeText: async text => { calls.push(["text", text]); },
    } };
  }
  const numeric = fixture(contextIdentity, digits);
  await trustedReplaceFocusedSettingsDigits(numeric.input, { identity: contextIdentity }, "65537");
  assert.deepEqual(numeric.calls, [["down", "Control"], ["press", "a"], ["up", "Control"], ["text", "65537"]]);
  const toggle = fixture(checkboxIdentity, checkbox);
  await trustedToggleFocusedSettingsCheckbox(toggle.input, { identity: checkboxIdentity });
  assert.deepEqual(toggle.calls, [["press", " "]]);
  for (const [identity, rows, operate] of [
    [contextIdentity, digits, input => trustedReplaceFocusedSettingsDigits(input, { identity: contextIdentity }, "65537")],
    [checkboxIdentity, checkbox, input => trustedToggleFocusedSettingsCheckbox(input, { identity: checkboxIdentity })],
  ]) {
    const wrongFocus = fixture(identity, rows, { ...identity, configKey: "other" });
    await assert.rejects(operate(wrongFocus.input), error => error.code === "settings-text-focus-owner");
    assert.deepEqual(wrongFocus.calls, []);
    for (const invalidRows of [
      rows.slice(0, -1),
      rows.map((row, index) => index === 0 ? { ...row, configKey: "other" } : row),
      rows.map((row, index) => index === 0 ? { ...row, isTrusted: false } : row),
      [...rows, rows.at(-1)],
    ]) await assert.rejects(operate(fixture(identity, invalidRows).input));
  }
});

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
      { key: "model.system_prompt", value: MAIN_SYSTEM_PROMPT_MARKER },
      { key: "model.max_output_tokens", value: "32768" },
      { key: "docling.enabled", value: "false" },
      { key: "docling.base_url", value: "http://127.0.0.1:43111" },
    ],
    provider_base_url: "http://127.0.0.1:43111",
    provider_profile: SETTINGS_PROVIDER_PROFILE,
    provider_api_key_env: SETTINGS_PROVIDER_API_KEY_ENV,
    provider_context_window: PROVIDER_CONTEXT_BEFORE,
    provider_max_output_tokens: "32768",
    provider_model_ids: ["moyai-e2e-scripted"],
    provider_selected_index: 0,
    ...overrides,
  };
}

function cleanSurface(overrides = {}) {
  return {
    projection: projection(),
    settings_entry: {
      count: 1,
      visible: true,
      enabled: true,
      text: "設定",
      title: "設定",
    },
    settings: {
      dialog_count: 1,
      dialog_visible: true,
      profile: { count: 1, visible: true, enabled: true, value: SETTINGS_PROVIDER_PROFILE, options: [...PROVIDER_PROFILE_OPTIONS] },
      api_key_env: { count: 1, visible: true, enabled: true, value: SETTINGS_PROVIDER_API_KEY_ENV },
      context: { count: 1, value: PROVIDER_CONTEXT_AFTER },
      system_prompt: {
        count: 1,
        visible: true,
        enabled: true,
        value: MAIN_SYSTEM_PROMPT_MARKER,
      },
      max_output_tokens: { count: 0, visible: false, enabled: false, value: null },
      docling: { count: 1, checked: false },
      docling_label: { count: 1, visible: false, text: "Docling を有効化" },
      dirty_badge_visible: false,
      save: { count: 1, visible: true, enabled: false },
      discard: { count: 0, visible: false, enabled: false },
      close: { count: 1, visible: true, enabled: true },
      navigation: {
        groups: {
          count: 3,
          visible_count: 3,
          texts: ["共通設定", "チャットごとの設定", "画面設定"],
        },
        provider: { count: 1, visible: true, text: "メインチャット" },
        side_chat: { count: 1, visible: true, text: "サイドチャット" },
        tools: { count: 1, visible: true, text: "ツール" },
        session_overrides: { count: 1, visible: true, text: "現在のチャット" },
        window: { count: 1, visible: true, text: "ウィンドウ" },
      },
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
  assert.doesNotMatch(config, /max_(?:output_)?tokens|reasoning_(?:effort|summary)|supports_reasoning|temperature|top_p|top_k|presence_penalty|frequency_penalty|seed\s*=|stop(?:_sequences)?\s*=|\[model\.extra_body_json\]/);
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
      max_output_tokens: { count: 0, visible: false, enabled: false, value: null },
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

  for (const [name, mutate] of [
    ["legacy Main label", (value) => { value.settings.navigation.provider.text = "メインLLM"; }],
    ["legacy Side label", (value) => { value.settings.navigation.side_chat.text = "サイドチャットLLM"; }],
    ["missing hierarchy group", (value) => { value.settings.navigation.groups.count = 2; }],
    ["reordered hierarchy groups", (value) => { value.settings.navigation.groups.texts.reverse(); }],
    ["missing Session Overrides", (value) => { value.settings.navigation.session_overrides.count = 0; }],
    ["missing Window", (value) => { value.settings.navigation.window.visible = false; }],
  ]) {
    const drifted = structuredClone(settings);
    mutate(drifted);
    assert.equal(
      preferencesReady(drifted, [], { contextWindow: PROVIDER_CONTEXT_AFTER, doclingEnabled: false }),
      false,
      name,
    );
  }
});

test("an enabled Docling fixture without a custom prompt uses an explicit empty prompt expectation", () => {
  const settings = cleanSurface();
  settings.projection.config_fields.find(row => row.key === "model.system_prompt").value = "";
  settings.projection.config_fields.find(row => row.key === "docling.enabled").value = "true";
  settings.settings.system_prompt.value = "";
  settings.settings.docling.checked = true;
  const options = { contextWindow: PROVIDER_CONTEXT_AFTER, doclingEnabled: true, systemPrompt: "" };
  assert.equal(preferencesReady(settings, [], options), true);
  assert.equal(preferencesReady(settings, [], { ...options, systemPrompt: MAIN_SYSTEM_PROMPT_MARKER }), false);
  settings.settings.system_prompt.value = "unexpected override";
  assert.equal(preferencesReady(settings, [], options), false);
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
  assert.equal(Object.hasOwn(providerCommand.args.input, "maxOutputTokens"), false);
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
  assert.equal(
    save.args.values.find((row) => row.key === "model.system_prompt").text,
    MAIN_SYSTEM_PROMPT_MARKER,
  );
  assert.deepEqual(save.args.expectedTarget, target);

  const contextSave = expectedGlobalSave(cleanSurface(), {
    "model.context_window": PROVIDER_CONTEXT_AFTER,
    "model.system_prompt": MAIN_SYSTEM_PROMPT_MARKER,
  });
  assert.equal(contextSave.command, "save_global_config");
  assert.equal(contextSave.args.values.find((row) => row.key === "model.context_window").text, PROVIDER_CONTEXT_AFTER);
  assert.equal(contextSave.args.values.find((row) => row.key === "docling.enabled").text, "false");
  assert.equal(
    contextSave.args.values.find((row) => row.key === "model.system_prompt").text,
    MAIN_SYSTEM_PROMPT_MARKER,
  );
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
  assert.equal(first.coverage.native_titlebar_drag, "required");
  for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) {
    assert.equal(typeof first[method], "function");
  }
});

test("config-only scenario declares excluded native drag and retains isolated settings lifecycle", async () => {
  const config = createSettingsPreferencesConfigScenario();
  assert.notEqual(config, createSettingsPreferencesConfigScenario());
  assert.equal(config.id, "settings.preferences-config");
  assert.equal(config.databaseRequired, true);
  assert.equal(config.coverage.settings_save_and_restart, "required");
  assert.equal(config.coverage.native_titlebar_drag, "not_tested");
  assert.match(config.coverage.native_drag_note, /native drag未確認/);
  for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) {
    assert.equal(typeof config[method], "function");
  }
  const cleanup = await config.cleanup();
  assert.equal(cleanup.input, "fail");
  assert.equal(cleanup.resources[0].coverage.native_titlebar_drag, "not_tested");
  assert.equal(cleanup.resources[0].native_drag_release_verified, null);
});

test("config shell admission does not claim native drag geometry", () => {
  const shell = cleanSurface({
    projection: projection({ overlay: "none" }),
    visible_dialog_count: 0,
    visible_backdrop_count: 0,
    settings_entry: { count: 1, visible: true, enabled: true, text: "設定", title: "設定" },
    titlebar: null,
  });
  assert.equal(shellReadyForPreferences(shell, []), true);
  assert.equal(shellReadyForSettingsDrag(shell, []), false);
  assert.equal(shellReadyForPreferences(shell, [{ pathname: "/v1/models" }]), false);
  assert.equal(shellReadyForPreferences({ ...shell, visible_dialog_count: 1 }, []), false);
});

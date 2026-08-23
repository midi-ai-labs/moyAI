import assert from "node:assert/strict";
import test from "node:test";

import {
  PROVIDER_OPENAI_COMPATIBLE_PROMPT,
  assertTrustedProviderProfileSelection,
  createProviderConnectionLiveScenario,
  expectedProviderConnectionGlobalSave,
  liveCurrentTimeTerminalAccepted,
  liveCurrentTimeTerminalDecision,
  normalizeProviderConnectionLiveOptions,
  parseCurrentTimeToolStatus,
  parseCurrentTimeWorkSummary,
  providerConnectionLiveFixtureConfig,
  restoredProviderConnectionReady,
  savedProviderConnectionReady,
} from "../scenarios/provider_connection_live.mjs";

const RAW_OPTIONS = Object.freeze({
  provider_base_url: "http://192.0.2.10:8119/v1/",
  model: " example/Qwen-27B ",
});

const OPTIONS = Object.freeze({
  providerBaseUrl: "http://192.0.2.10:8119/v1",
  model: "example/Qwen-27B",
});

const CONFIG_TARGET = Object.freeze({
  workspacePath: "C:\\workspace",
  sessionId: null,
  configGeneration: "42",
});

const CONFIG_FIELDS = Object.freeze([
  { key: "model.base_url", value: OPTIONS.providerBaseUrl },
  { key: "model.model", value: OPTIONS.model },
  { key: "model.provider_profile", value: "openai_compatible" },
  { key: "model.api_key_env", value: "" },
  { key: "model.context_window", value: "32768" },
  { key: "model.max_output_tokens", value: "1024" },
  { key: "model.supports_tools", value: "true" },
  { key: "docling.enabled", value: "false" },
]);

const TOOL_STATUS = "ツール:\n- Current time [completed] local: 2026-08-24T12:34:56+09:00\nutc: 2026-08-24T03:34:56Z\ntimezone: +09:00\nunix_ms: 1787542496000";
const WORK_SUMMARY = "### 作業サマリ\n- 結果: セッションは完了しました。\n- コマンド/ツール: 2件\n\n### 作業履歴\n- [待機] current_time\n- [完了] Current time\n  出力: local: 2026-08-24T12:34:56+09:00 utc: 2026-08-24T03:34:56Z timezone: +09:00 unix_ms: 1787542496000";
const ASSISTANT = "現在時刻は local=2026-08-24T12:34:56+09:00 / utc=2026-08-24T03:34:56Z / timezone=+09:00 です。";

function restoredSurface(overrides = {}) {
  const projection = {
    overlay: "config",
    config_target: structuredClone(CONFIG_TARGET),
    config_fields: structuredClone(CONFIG_FIELDS),
    provider_effective_base_url: OPTIONS.providerBaseUrl,
    provider_effective_model_id: OPTIONS.model,
    provider_effective_profile: "openai_compatible",
    provider_effective_api_key_env: "",
    ...overrides.projection,
  };
  return {
    projection,
    settings: {
      dialog: { count: 1, visible: true },
      base_url: { count: 1, visible: true, enabled: true, value: OPTIONS.providerBaseUrl },
      profile: {
        count: 1,
        visible: true,
        enabled: true,
        value: "openai_compatible",
        options: ["lm_studio", "openai_compatible", "openai_responses", "lm_studio_chat_completions"],
      },
      model_details: { count: 1, visible: true, open: true },
      model: { count: 1, visible: true, enabled: true, value: OPTIONS.model },
      api_key_env: { count: 1, visible: true, enabled: true, value: "" },
      dirty: false,
      save: { count: 1, visible: true, enabled: false },
      close: { count: 1, visible: true, enabled: true },
      ...overrides.settings,
    },
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
    visible_validation_error_count: 0,
    visible_transcript_error_count: 0,
    ...overrides.surface,
  };
}

function terminalSurface({ projection = {}, surface = {} } = {}) {
  return {
    projection: {
      run_status_key: "completed",
      task_activity_state: "idle",
      busy: false,
      agent_tree_active: false,
      post_run_refresh_pending: false,
      background_mutation_pending: false,
      async_polling_required: false,
      pending_async_operations: [],
      overlay: "none",
      confirmation_visible: false,
      confirmation_id: null,
      confirmation: null,
      draft_prompt: "",
      can_submit: true,
      tool_status_text: "ツール: 実行履歴はまだありません。",
      transcript_rows: [
        { row_kind: "user", body: PROVIDER_OPENAI_COMPATIBLE_PROMPT },
        { row_kind: "work_summary_completed", body: WORK_SUMMARY },
        { row_kind: "assistant", body: ASSISTANT },
      ],
      ...projection,
    },
    assistants: [{ text: ASSISTANT, visible: true }],
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
    visible_validation_error_count: 0,
    visible_transcript_error_count: 0,
    ...surface,
  };
}

test("live provider config accepts exactly one credential-free endpoint and model pair", () => {
  assert.deepEqual(normalizeProviderConnectionLiveOptions(RAW_OPTIONS), OPTIONS);
  assert.deepEqual(normalizeProviderConnectionLiveOptions({
    provider_base_url: "https://provider.example.test",
    model: "model-a",
  }), {
    providerBaseUrl: "https://provider.example.test",
    model: "model-a",
  });

  for (const invalid of [
    null,
    [],
    {},
    { provider_base_url: "ftp://provider.test/v1", model: "model-a" },
    { provider_base_url: "http://user@provider.test/v1", model: "model-a" },
    { provider_base_url: "http://provider.test/v1?key=secret", model: "model-a" },
    { provider_base_url: "http://provider.test/v1#fragment", model: "model-a" },
    { provider_base_url: "http://provider.test/v1", model: "" },
    { provider_base_url: "http://provider.test/v1", model: "bad\nmodel" },
    { provider_base_url: "http://provider.test/v1", model: "model-a", api_key: "secret" },
  ]) {
    assert.throws(() => normalizeProviderConnectionLiveOptions(invalid), TypeError);
  }
});

test("fixture is deterministic, tool-capable, and starts from a distinct credential-name baseline", () => {
  const config = providerConnectionLiveFixtureConfig();
  assert.match(config, /base_url = "http:\/\/127\.0\.0\.1:9\/v1"/);
  assert.match(config, /model = "moyai-e2e-provider-before-save"/);
  assert.match(config, /provider_profile = "lm_studio"/);
  assert.match(config, /api_key_env = "MOYAI_E2E_UNUSED_PROVIDER_KEY"/);
  assert.match(config, /supports_tools = true/);
  assert.match(config, /max_retries = 0/);
  assert.doesNotMatch(config, /provider_(?:metadata|api)_mode/);
  assert.doesNotMatch(config, /192\.0\.2\.10|example\/Qwen-27B/);
});

test("native provider select accepts the trusted committed change across its controlled rerender", () => {
  const identity = { tag: "SELECT", configKey: "model.provider_profile" };
  const snapshot = {
    found: true,
    sequence: 13,
    dropped_through: 0,
    events: [
      { sequence: 11, type: "input", isTrusted: true, ...identity },
      { sequence: 12, type: "change", isTrusted: true, ...identity },
      { sequence: 13, type: "keyup", isTrusted: true, key: "Enter", code: "Enter", ...identity },
    ],
  };

  assert.deepEqual(assertTrustedProviderProfileSelection(snapshot, 10), {
    after_sequence: 10,
    last_sequence: 12,
    events: [snapshot.events[1]],
  });
  assert.throws(
    () => assertTrustedProviderProfileSelection({
      ...snapshot,
      events: [{ ...snapshot.events[1], isTrusted: false }],
    }, 10),
    /not browser-trusted/,
  );
});

test("global Save expectation carries every ordered config value and the exact target", () => {
  const surface = restoredSurface();
  surface.projection.config_fields = surface.projection.config_fields.map((field) => {
    if (field.key === "model.base_url") return { ...field, value: "http://127.0.0.1:9/v1" };
    if (field.key === "model.model") return { ...field, value: "before" };
    if (field.key === "model.provider_profile") return { ...field, value: "lm_studio" };
    if (field.key === "model.api_key_env") return { ...field, value: "OLD_KEY" };
    return field;
  });
  assert.deepEqual(expectedProviderConnectionGlobalSave(surface, OPTIONS), {
    command: "save_global_config",
    args: {
      values: CONFIG_FIELDS.map((field) => ({ key: field.key, text: field.value })),
      expectedTarget: CONFIG_TARGET,
    },
  });
});

test("restart persistence requires the exact saved and effective atomic connection", () => {
  assert.equal(restoredProviderConnectionReady(restoredSurface(), OPTIONS), true);
  assert.equal(restoredProviderConnectionReady(restoredSurface({
    projection: { provider_effective_profile: "openai_responses" },
  }), OPTIONS), false);
  assert.equal(restoredProviderConnectionReady(restoredSurface({
    projection: {
      config_fields: CONFIG_FIELDS.map((field) => field.key === "model.api_key_env"
        ? { ...field, value: "CURRENT_GLOBAL_KEY" }
        : field),
    },
  }), OPTIONS), false);
  assert.equal(restoredProviderConnectionReady(restoredSurface({
    settings: { dirty: true, save: { count: 1, visible: true, enabled: true } },
  }), OPTIONS), false);
});

test("global save accepts the product's collapsed model details after the exact atomic commit", () => {
  const surface = restoredSurface({
    projection: {
      config_target: { ...CONFIG_TARGET, configGeneration: "43" },
    },
    settings: {
      model_details: { count: 1, visible: true, open: false },
      model: { count: 1, visible: true, enabled: true, value: OPTIONS.model },
    },
  });
  assert.equal(savedProviderConnectionReady(surface, OPTIONS, CONFIG_TARGET), true);
  assert.equal(savedProviderConnectionReady(surface, OPTIONS, surface.projection.config_target), false);
  assert.equal(savedProviderConnectionReady(restoredSurface(), OPTIONS, CONFIG_TARGET), false);
});

test("current_time parser accepts exactly one completed four-field result", () => {
  assert.deepEqual(parseCurrentTimeToolStatus(TOOL_STATUS), {
    local: "2026-08-24T12:34:56+09:00",
    utc: "2026-08-24T03:34:56Z",
    timezone: "+09:00",
    unixMs: "1787542496000",
  });
  assert.equal(parseCurrentTimeToolStatus(`${TOOL_STATUS}\n- Current time [completed] local: duplicate`), null);
  assert.equal(parseCurrentTimeToolStatus(TOOL_STATUS.replace("[completed]", "[failed]")), null);
  assert.equal(parseCurrentTimeToolStatus(TOOL_STATUS.replace("timezone: +09:00\n", "")), null);
  assert.equal(parseCurrentTimeToolStatus(null), null);
  assert.deepEqual(parseCurrentTimeWorkSummary(WORK_SUMMARY), {
    local: "2026-08-24T12:34:56+09:00",
    utc: "2026-08-24T03:34:56Z",
    timezone: "+09:00",
    unixMs: "1787542496000",
  });
  assert.equal(parseCurrentTimeWorkSummary(`${WORK_SUMMARY}\n- [完了] Other`), null);
  assert.equal(parseCurrentTimeWorkSummary(WORK_SUMMARY.replace("[完了]", "[失敗]")), null);
});

test("live terminal helper classifies deterministic pass, pending, and product failure without network", () => {
  const accepted = terminalSurface();
  assert.equal(liveCurrentTimeTerminalAccepted(accepted), true);
  assert.equal(liveCurrentTimeTerminalDecision(accepted), "pass");
  assert.equal(liveCurrentTimeTerminalAccepted(terminalSurface({
    surface: { assistants: [{ text: ASSISTANT.replace("現在時刻は", "確認結果は"), visible: true }] },
  })), true);

  assert.equal(liveCurrentTimeTerminalDecision(terminalSurface({
    projection: { run_status_key: "running", task_activity_state: "running", busy: true },
  })), "pending");
  assert.equal(liveCurrentTimeTerminalDecision(terminalSurface({
    projection: {
      post_run_refresh_pending: true,
      async_polling_required: true,
      pending_async_operations: ["terminal_run_refresh", "snapshot_refresh"],
      tool_status_text: "ツール: 実行履歴はまだありません。",
    },
  })), "pending");
  assert.equal(liveCurrentTimeTerminalDecision(terminalSurface({
    surface: { visible_recoverable_error_count: 1 },
  })), "fail");
  assert.equal(liveCurrentTimeTerminalDecision(terminalSurface({
    projection: {
      transcript_rows: [
        { row_kind: "work_summary_completed", body: `${WORK_SUMMARY}\n- [完了] Other` },
        { row_kind: "assistant", body: ASSISTANT },
      ],
    },
  })), "fail");
  assert.equal(liveCurrentTimeTerminalDecision(terminalSurface({
    projection: {
      transcript_rows: [
        { row_kind: "work_summary_completed", body: WORK_SUMMARY },
        { row_kind: "assistant", body: "日本語ですが時刻値は含みません。" },
      ],
    },
  })), "fail");
  assert.equal(liveCurrentTimeTerminalDecision(terminalSurface({
    projection: { run_status_key: "failed" },
  })), "fail");
});

test("scenario factory exposes the shared-runner live manual contract without contacting the provider", () => {
  const first = createProviderConnectionLiveScenario(RAW_OPTIONS);
  const second = createProviderConnectionLiveScenario(RAW_OPTIONS);
  assert.notEqual(first, second);
  assert.equal(first.id, "manual.provider-openai-compatible");
  assert.equal(first.productOracle, "pass");
  assert.equal(first.manualGate, "not_required");
  assert.equal(first.databaseRequired, true);
  for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) {
    assert.equal(typeof first[method], "function");
  }
});

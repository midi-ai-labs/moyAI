import assert from "node:assert/strict";
import test from "node:test";

import {
  PROVIDER_OPENAI_COMPATIBLE_PROMPT,
  assertTrustedProviderProfileSelection,
  createProviderConnectionLiveScenario,
  currentTimeFromCompletedProjection,
  expectedProviderConnectionGlobalSave,
  liveCurrentTimeTerminalAccepted,
  liveCurrentTimeTerminalDecision,
  normalizeProviderConnectionLiveOptions,
  parseCurrentTimeWorkSummary,
  providerConnectionLiveSideChatAnswerAccepted,
  providerConnectionLiveSideChatMainPreserved,
  providerConnectionLiveSideChatQuestion,
  providerConnectionLiveFixtureConfig,
  providerConnectionLiveControlTokenLeaks,
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
  sideChatAfterCompletion: false,
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
  { key: "model.max_output_tokens", value: "32768" },
  { key: "model.supports_tools", value: "true" },
  { key: "docling.enabled", value: "false" },
]);

const WORK_SUMMARY = "### 作業サマリ\n- 結果: セッションは完了しました。\n- コマンド/ツール: 2件\n\n### 作業履歴\n- [待機] current_time\n- [完了] Current time\n  出力: local: 2026-08-24T12:34:56+09:00 utc: 2026-08-24T03:34:56Z timezone: +09:00 unix_ms: 1787542496000";
const ASSISTANT = "接続確認完了：local=2026-08-24T12:34:56+09:00 / utc=2026-08-24T03:34:56Z / timezone=+09:00 です。";

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
      selected_session_title: "接続確認 [完了] 01M0TEST",
      status_message: "実行完了",
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
      tool_status_text: "ツール: 1件中1件を表示（要確認を優先・新しい順）\n- [完了] Current time: local: 2026-08-24T12:34:56+09:00 utc: 2026-08-24T03:34:56Z timezone: +09:00 unix_ms: 1787542496000",
      latest_tool_summary: "完了: 時刻の確認",
      progress_text: "Completed\nフェーズ: 終了処理\n手順: completed\nモデル要求: 2\nツール: 1件開始 / 1件完了 / 0件拒否 / 0件キャンセル / 0件失敗\n圧縮: 0",
      transcript_rows: [
        { row_kind: "user", body: PROVIDER_OPENAI_COMPATIBLE_PROMPT },
        { row_kind: "work_summary_completed", title: "1s作業しました", body: WORK_SUMMARY },
        { row_kind: "assistant", body: ASSISTANT },
      ],
      ...projection,
    },
    assistants: [{ text: ASSISTANT, visible: true }],
    completed_summaries: [{
      title: "1s作業しました",
      body: "Current time local: 2026-08-24T12:34:56+09:00 utc: 2026-08-24T03:34:56Z timezone: +09:00 unix_ms: 1787542496000 完了",
      visible: true,
      summary_visible: true,
    }],
    terminal_dom: {
      topbar_title: { count: 1, visible: true, text: "接続確認 [完了] 01M0TEST" },
      topbar_status: { count: 1, visible: true, text: "実行完了" },
      visible_run_strip_count: 0,
      visible_task_activity_indicator_count: 0,
      visible_selected_activity_row_count: 0,
    },
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
    visible_validation_error_count: 0,
    visible_transcript_error_count: 0,
    ...surface,
  };
}

test("live provider config accepts one credential-free endpoint, model, and optional Side Chat gate", () => {
  assert.deepEqual(normalizeProviderConnectionLiveOptions(RAW_OPTIONS), OPTIONS);
  assert.deepEqual(normalizeProviderConnectionLiveOptions({
    provider_base_url: "https://provider.example.test",
    model: "model-a",
    side_chat_after_completion: true,
  }), {
    providerBaseUrl: "https://provider.example.test",
    model: "model-a",
    sideChatAfterCompletion: true,
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
    { provider_base_url: "http://provider.test/v1", model: "model-a", side_chat_after_completion: "true" },
    { provider_base_url: "http://provider.test/v1", model: "model-a", side_chat_after_completion: 1 },
  ]) {
    assert.throws(() => normalizeProviderConnectionLiveOptions(invalid), TypeError);
  }
});

test("post-task Side Chat asks from owner evidence and accepts only the exact tool and time answer", () => {
  const time = {
    local: "2026-08-24T12:34:56+09:00",
    utc: "2026-08-24T03:34:56Z",
    timezone: "+09:00",
    unixMs: "1787542496000",
  };
  const question = providerConnectionLiveSideChatQuestion(time);
  assert.match(question, /unix_msが1787542496000/u);
  assert.doesNotMatch(question, /current_time|2026-08-24T12:34:56\+09:00|2026-08-24T03:34:56Z|\+09:00/u);
  const answer = "Side Chat確認完了：tool=current_time / local=2026-08-24T12:34:56+09:00 / utc=2026-08-24T03:34:56Z / timezone=+09:00 です。";
  assert.equal(providerConnectionLiveSideChatAnswerAccepted(answer, time), true);
  assert.equal(providerConnectionLiveSideChatAnswerAccepted(` \r\n${answer}\n\t`, time), true);
  for (const answer of [
    "Side Chat確認完了：tool=current_time / local=2026-08-24T12:34:56+09:00 / utc=2026-08-24T03:34:56Z / timezone=UTC+09:00 です。",
    "Side Chat確認完了：tool=Current time / local=2026-08-24T12:34:56+09:00 / utc=2026-08-24T03:34:56Z / timezone=+09:00 です。",
    "Side Chat確認完了：tool=current_time / local=2026-08-24T12:34:56+09:00\n/ utc=2026-08-24T03:34:56Z / timezone=+09:00 です。",
    "Side Chat確認完了：tool=current_time / timezone=+09:00 です。",
  ]) {
    assert.equal(providerConnectionLiveSideChatAnswerAccepted(answer, time), false);
  }
  assert.equal(providerConnectionLiveSideChatAnswerAccepted("", null), false);
  assert.throws(() => providerConnectionLiveSideChatQuestion({ ...time, unixMs: "not-a-number" }), TypeError);
});

test("post-task Side Chat preserves one immutable Main snapshot through global save, ensure, and completion", () => {
  const transcriptRows = [
    { row_kind: "user", stable_history_identity: "history-user", body: PROVIDER_OPENAI_COMPATIBLE_PROMPT },
    { row_kind: "assistant", stable_history_identity: "history-assistant", body: ASSISTANT },
  ];
  const visibleRows = transcriptRows.map((row) => ({ id: row.stable_history_identity, kind: row.row_kind, body: row.body }));
  const surface = {
    projection: {
      draft_target: { sessionId: "session-main" },
      run_status_key: "completed",
      task_activity_state: "idle",
      busy: false,
      agent_tree_active: false,
      post_run_refresh_pending: false,
      navigation_loading: false,
      selected_project_index: 0,
      selected_session_index: 0,
      session_rows: [{
        session_id: "session-main",
        active_turn_id: null,
        admission_revision: "1",
        latest_turn_id: "turn-main",
      }],
      transcript_rows: transcriptRows,
      turn_page_total: 2,
      turn_page_limit: 256,
    },
    main: { primary_rows: visibleRows, prompt_value: "" },
  };
  const baseline = {
    session_id: "session-main",
    selected_session_id: "session-main",
    primary_rows: visibleRows,
    canonical_rows: transcriptRows,
    visible_primary_rows: visibleRows,
    main_draft: "",
    active_turn_id: null,
    admission_revision: "1",
    latest_turn_id: "turn-main",
    turn_page_total: 2,
    turn_page_limit: 256,
  };
  const sideChat = { mainBaseline: structuredClone(baseline), completedSurface: structuredClone(surface) };
  assert.equal(providerConnectionLiveSideChatMainPreserved(baseline, surface, sideChat), true);
  assert.equal(providerConnectionLiveSideChatMainPreserved(
    baseline,
    { ...surface, main: { ...surface.main, prompt_value: "changed" } },
    sideChat,
  ), false);
  assert.equal(providerConnectionLiveSideChatMainPreserved(
    baseline,
    surface,
    { ...sideChat, mainBaseline: { ...baseline, latest_turn_id: "turn-other" } },
  ), false);
  assert.equal(providerConnectionLiveSideChatMainPreserved(
    baseline,
    surface,
    {
      ...sideChat,
      completedSurface: {
        ...surface,
        projection: { ...surface.projection, transcript_rows: [...transcriptRows, { row_kind: "error", body: "drift" }] },
      },
    },
  ), false);
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
  assert.doesNotMatch(config, /max_(?:output_)?tokens|reasoning_(?:effort|summary)|supports_reasoning|temperature|top_p|top_k|presence_penalty|frequency_penalty|seed\s*=|stop(?:_sequences)?\s*=|\[model\.extra_body_json\]/);
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

test("completed work summary is the single current_time evidence owner", () => {
  assert.deepEqual(parseCurrentTimeWorkSummary(WORK_SUMMARY), {
    local: "2026-08-24T12:34:56+09:00",
    utc: "2026-08-24T03:34:56Z",
    timezone: "+09:00",
    unixMs: "1787542496000",
  });
  assert.equal(parseCurrentTimeWorkSummary(`${WORK_SUMMARY}\n- [完了] Other`), null);
  assert.equal(parseCurrentTimeWorkSummary(WORK_SUMMARY.replace("[完了]", "[失敗]")), null);
  assert.deepEqual(currentTimeFromCompletedProjection(terminalSurface().projection), {
    local: "2026-08-24T12:34:56+09:00",
    utc: "2026-08-24T03:34:56Z",
    timezone: "+09:00",
    unixMs: "1787542496000",
  });
  assert.equal(currentTimeFromCompletedProjection(terminalSurface({
    projection: {
      transcript_rows: [
        { row_kind: "work_summary_completed", body: WORK_SUMMARY },
        { row_kind: "work_summary_completed", body: WORK_SUMMARY },
        { row_kind: "assistant", body: ASSISTANT },
      ],
    },
  }).projection), null);
});

test("live provider smoke rejects exact chat-template tokens in every assistant transcript row", () => {
  for (const marker of ["<|im_start|>", "<|im_end|>"]) {
    const leaked = terminalSurface({
      projection: {
        transcript_rows: [
          { row_kind: "assistant", body: `intermediate tool call ${marker}assistant` },
          { row_kind: "work_summary_completed", title: "1s作業しました", body: WORK_SUMMARY },
          { row_kind: "assistant", body: ASSISTANT },
        ],
      },
    });
    assert.deepEqual(providerConnectionLiveControlTokenLeaks(leaked.projection), [{
      row_index: 0,
      markers: [marker],
    }]);
    assert.equal(liveCurrentTimeTerminalAccepted(leaked), false);
    assert.equal(liveCurrentTimeTerminalDecision(leaked), "fail");
  }
});

test("live provider smoke allows lookalike tokens and exact markers outside assistant rows", () => {
  const accepted = terminalSurface({
    projection: {
      transcript_rows: [
        { row_kind: "assistant", body: "intermediate <|tool_call|> payload" },
        { row_kind: "tool", body: "tool output containing <|im_start|> and <|im_end|>" },
        { row_kind: "work_summary_completed", title: "1s作業しました", body: WORK_SUMMARY },
        { row_kind: "assistant", body: ASSISTANT },
      ],
    },
  });
  assert.deepEqual(providerConnectionLiveControlTokenLeaks(accepted.projection), []);
  assert.equal(liveCurrentTimeTerminalAccepted(accepted), true);
  assert.equal(liveCurrentTimeTerminalDecision(accepted), "pass");
});

test("Hub live terminal permits idle connection polling only with an explicit route scope", () => {
  const hub = { status: "connected", active_main: null, active_side_chat: null };
  const surface = terminalSurface({ projection: { async_polling_required: true, hub } });
  assert.equal(liveCurrentTimeTerminalDecision(surface), "pending");
  assert.equal(liveCurrentTimeTerminalDecision(surface, { allowIdleHubPolling: true }), "pass");
  for (const changed of [{ active_main: {} }, { active_side_chat: {} }, { status: "connecting" }]) {
    assert.equal(liveCurrentTimeTerminalDecision(terminalSurface({ projection: {
      async_polling_required: true, hub: { ...hub, ...changed },
    } }), { allowIdleHubPolling: true }), "pending");
  }
  assert.equal(liveCurrentTimeTerminalDecision(terminalSurface({ projection: {
    async_polling_required: true, hub, pending_async_operations: [{}],
  } }), { allowIdleHubPolling: true }), "pending");
  assert.equal(liveCurrentTimeTerminalDecision(terminalSurface({ projection: {
    async_polling_required: true, hub, busy: true,
  } }), { allowIdleHubPolling: true }), "fail");
});

test("live terminal helper classifies deterministic pass, pending, and product failure without network", () => {
  const accepted = terminalSurface();
  assert.equal(liveCurrentTimeTerminalAccepted(accepted), true);
  assert.equal(liveCurrentTimeTerminalDecision(accepted), "pass");
  assert.equal(liveCurrentTimeTerminalAccepted(terminalSurface({
    surface: { assistants: [{ text: ASSISTANT.replace("接続確認完了：", ""), visible: true }] },
  })), false);
  for (const unexpectedAssistant of [
    ASSISTANT.replace("接続確認完了：", "接続確認完了：余計な文 "),
    ASSISTANT.replace(" です。", " 余計 です。"),
    ASSISTANT.replace(" / utc=", "/utc="),
  ]) {
    const unexpected = terminalSurface({
      projection: {
        transcript_rows: [
          { row_kind: "work_summary_completed", title: "1s作業しました", body: WORK_SUMMARY },
          { row_kind: "assistant", body: unexpectedAssistant },
        ],
      },
      surface: { assistants: [{ text: unexpectedAssistant, visible: true }] },
    });
    assert.equal(liveCurrentTimeTerminalAccepted(unexpected), false);
    assert.equal(liveCurrentTimeTerminalDecision(unexpected), "fail");
  }
  const corruptLocal = ASSISTANT.replace(" / utc=", "-CORRUPT / utc=");
  assert.equal(liveCurrentTimeTerminalDecision(terminalSurface({
    projection: {
      transcript_rows: [
        { row_kind: "work_summary_completed", title: "1s作業しました", body: WORK_SUMMARY },
        { row_kind: "assistant", body: corruptLocal },
      ],
    },
    surface: { assistants: [{ text: corruptLocal, visible: true }] },
  })), "fail");
  assert.equal(liveCurrentTimeTerminalDecision(terminalSurface({
    surface: { assistants: [{ text: corruptLocal, visible: true }] },
  })), "pending");
  assert.equal(liveCurrentTimeTerminalDecision(terminalSurface({
    surface: {
      assistants: [{
        text: "接続確認完了：local=2026-08-24T12:34:56+09:00 / utc=2026-08-24T03:34:56Z /",
        visible: true,
      }],
    },
  })), "pending");

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
      tool_status_text: "ツール: 実行履歴はまだありません。",
      latest_tool_summary: "ツール: 実行履歴はまだありません。",
      progress_text: "Completed\nツール: 1件開始 / 0件完了 / 0件拒否 / 0件キャンセル / 0件失敗",
    },
  })), "fail");
  assert.equal(liveCurrentTimeTerminalDecision(terminalSurface({
    surface: {
      terminal_dom: {
        visible_run_strip_count: 1,
        visible_task_activity_indicator_count: 2,
        visible_selected_activity_row_count: 1,
      },
    },
  })), "pending");
  assert.equal(liveCurrentTimeTerminalDecision(terminalSurface({
    projection: {
      selected_session_title: "接続確認 [実行中] 01M0TEST",
      status_message: "Provider応答受信中",
    },
    surface: {
      terminal_dom: {
        topbar_title: { count: 1, visible: true, text: "接続確認 [実行中] 01M0TEST" },
        topbar_status: { count: 1, visible: true, text: "Provider応答受信中" },
      },
    },
  })), "fail");
  assert.equal(liveCurrentTimeTerminalDecision(terminalSurface({
    surface: {
      completed_summaries: [{
        title: "1s作業しました",
        body: "Current time local: 2026-08-24T12:34:56+09:00-CORRUPT utc: 2026-08-24T03:34:56Z timezone: +09:00 unix_ms: 1787542496000 完了",
        visible: true,
        summary_visible: true,
      }],
    },
  })), "pending");
  assert.equal(liveCurrentTimeTerminalDecision(terminalSurface({
    surface: {
      terminal_dom: {
        topbar_title: { count: 1, visible: true, text: "接続確認 [実行中] 01M0TEST" },
        topbar_status: { count: 1, visible: true, text: "Provider応答受信中" },
        visible_run_strip_count: 1,
        visible_task_activity_indicator_count: 2,
        visible_selected_activity_row_count: 1,
      },
    },
  })), "pending");
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

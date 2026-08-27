import assert from "node:assert/strict";
import test from "node:test";

import {
  SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT,
  SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_RESPONSE,
} from "../drivers/scripted_provider.mjs";
import {
  chatToolContinuationHeldFailures,
  chatToolContinuationTerminalFailures,
  createProviderChatToolContinuationScenario,
  exactChatToolContinuationLedger,
  providerChatToolContinuationFixtureConfig,
} from "../scenarios/provider_chat_tool_continuation.mjs";

const SESSION_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const TURN_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const USER_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const HASH = "a".repeat(64);
const TIME = Object.freeze({
  local: "2026-08-26T12:34:56+09:00",
  utc: "2026-08-26T03:34:56Z",
  timezone: "+09:00",
  unixMs: "1787715296000",
});

function toolStatus() {
  return `ツール:\n- Current time [completed] local: ${TIME.local}\nutc: ${TIME.utc}\ntimezone: ${TIME.timezone}\nunix_ms: ${TIME.unixMs}`;
}

function workSummary() {
  return `### 作業サマリ
- 結果: セッションは完了しました。
- コマンド/ツール: 2件

### 作業履歴
- [待機] current_time
- [完了] Current time
  出力: local: ${TIME.local} utc: ${TIME.utc} timezone: ${TIME.timezone} unix_ms: ${TIME.unixMs}`;
}

function contract(role, continuation) {
  return {
    pass: true,
    role,
    model_matches: true,
    top_level_keys_match: true,
    stream_true: true,
    include_usage_true: true,
    n_one: true,
    client_generation_fields_absent: true,
    client_generation_fields_present: [],
    max_tokens_absent: true,
    parallel_tool_calls_false: true,
    tools: {
      pass: true,
      unique_tool_names: true,
      current_time_present: true,
      current_time_schema_matches: true,
    },
    role_evidence: continuation ? {
      message_count: 4,
      message_roles: ["system", "user", "assistant", "tool"],
      system_content_sha256: HASH,
      system_content_non_empty: true,
      user_content_sha256: HASH,
      user_prompt_matches: true,
      assistant_content_absent: true,
      current_time_call_matches: true,
      tool_output_shape_matches: true,
      tool_output_size_bytes: 143,
      tool_output_sha256: HASH,
    } : {
      message_count: 2,
      message_roles: ["system", "user"],
      system_content_sha256: HASH,
      system_content_non_empty: true,
      user_content_sha256: HASH,
      user_prompt_matches: true,
      assistant_content_absent: false,
      current_time_call_matches: false,
      tool_output_shape_matches: false,
      tool_output_size_bytes: null,
      tool_output_sha256: null,
    },
  };
}

function ledger(phases = ["completed", "held"]) {
  return ["chat_tool_initial", "chat_continuation"].map((role, index) => ({
    route: "chat_completions",
    method: "POST",
    pathname: "/v1/chat/completions",
    query_present: false,
    contract: contract(role, index === 1),
    response_phase: phases[index],
    response_status: phases[index] === "completed" ? 200 : null,
  }));
}

function baseProjection(overrides = {}) {
  return {
    run_status_key: "running",
    task_activity_state: "running",
    busy: true,
    agent_tree_active: false,
    tool_status_text: "ツール: 実行履歴はまだありません。",
    latest_tool_summary: "ツール: 実行履歴はまだありません。",
    progress_text: "Running\nモデル要求: 0\nツール: 0件開始 / 0件完了 / 0件拒否 / 0件キャンセル / 0件失敗",
    run_target: {
      sessionId: SESSION_ID,
      expectedState: { kind: "turn", turnId: TURN_ID, admissionRevision: "1" },
    },
    draft_target: { sessionId: SESSION_ID },
    transcript_rows: [
      {
        row_kind: "user",
        stable_history_identity: USER_ID,
        body: SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT,
      },
      {
        row_kind: "work_summary_running",
        stable_history_identity: `turn:${TURN_ID}:work-summary`,
        body: "Current time completed",
      },
    ],
    ...overrides,
  };
}

function baseSurface(overrides = {}) {
  return {
    projection: baseProjection(),
    thread_count: 1,
    thread_text: `${SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT}\nCurrent time completed`,
    users: [{ history_identity: USER_ID, text: SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT, visible: true }],
    assistants: [],
    errors: [],
    running_summaries: [{ history_identity: `turn:${TURN_ID}:work-summary`, text: "Current time completed", visible: true }],
    completed_summaries: [],
    prompt: { count: 1, value: "", visible: true, enabled: true },
    send: { count: 0, visible: false, enabled: false, title: null, aria_label: null },
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
    visible_validation_error_count: 0,
    visible_transcript_error_count: 0,
    ...overrides,
  };
}

function terminalProjection(overrides = {}) {
  return baseProjection({
    run_status_key: "completed",
    task_activity_state: "idle",
    busy: false,
    tool_status_text: toolStatus(),
    latest_tool_summary: "ツール:",
    progress_text: "Completed\nモデル要求: 2\nツール: 1件開始 / 1件完了 / 0件拒否 / 0件キャンセル / 0件失敗",
    post_run_refresh_pending: false,
    background_mutation_pending: false,
    async_polling_required: false,
    pending_async_operations: [],
    navigation_loading: false,
    provider_loading: false,
    overlay: "none",
    confirmation_visible: false,
    confirmation_id: null,
    confirmation: null,
    draft_prompt: "",
    composer_submit_mode: "new_request",
    can_submit: true,
    run_target: {
      sessionId: SESSION_ID,
      expectedState: { kind: "idle", latestTurnId: TURN_ID, admissionRevision: "1" },
    },
    transcript_rows: [
      {
        row_kind: "user",
        stable_history_identity: USER_ID,
        body: SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT,
      },
      {
        row_kind: "work_summary_completed",
        stable_history_identity: `turn:${TURN_ID}:work-summary`,
        body: workSummary(),
      },
      {
        row_kind: "assistant",
        body: SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_RESPONSE,
      },
    ],
    ...overrides,
  });
}

function terminalSurface(overrides = {}) {
  return baseSurface({
    projection: terminalProjection(),
    thread_text: [
      SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT,
      "Current time completed",
      SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_RESPONSE,
    ].join("\n"),
    assistants: [{
      history_identity: null,
      text: SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_RESPONSE,
      visible: true,
    }],
    running_summaries: [],
    completed_summaries: [{
      history_identity: `turn:${TURN_ID}:work-summary`,
      text: "Current time completed",
      visible: true,
    }],
    prompt: { count: 1, value: "", visible: true, enabled: true },
    send: {
      count: 1,
      visible: true,
      enabled: false,
      title: "依頼文を入力してください",
      aria_label: "依頼文を入力してください",
    },
    ...overrides,
  });
}

test("Chat tool-continuation fixture uses the canonical credential-free profile", () => {
  const config = providerChatToolContinuationFixtureConfig("http://127.0.0.1:43123");
  assert.match(config, /base_url = "http:\/\/127\.0\.0\.1:43123"/u);
  assert.match(config, /model = "e2e\/scripted-responses"/u);
  assert.match(config, /provider_profile = "openai_compatible"/u);
  assert.match(config, /supports_tools = true/u);
  assert.match(config, /parallel_tool_calls = false/u);
  assert.doesNotMatch(config, /provider_(?:metadata|api)_mode/u);
  assert.doesNotMatch(config, /api_key(?:_env)? =/u);
  assert.doesNotMatch(config, /max_(?:output_)?tokens|reasoning_(?:effort|summary)|supports_reasoning|temperature|top_p|top_k|presence_penalty|frequency_penalty|seed\s*=|stop(?:_sequences)?\s*=|\[model\.extra_body_json\]/u);
});

test("Chat continuation ledger requires the exact two roles and absent replay content", () => {
  assert.equal(exactChatToolContinuationLedger(ledger(), ["completed", "held"]), true);
  assert.equal(exactChatToolContinuationLedger(ledger(["completed", "completed"]), [
    "completed",
    "completed",
  ]), true);
  assert.equal(exactChatToolContinuationLedger(ledger().slice(0, 1), ["completed", "held"]), false);
  const contentReplay = ledger();
  contentReplay[1].contract.role_evidence.assistant_content_absent = false;
  assert.equal(exactChatToolContinuationLedger(contentReplay, ["completed", "held"]), false);
  const wrongRoute = ledger();
  wrongRoute[1].pathname = "/v1/responses";
  assert.equal(exactChatToolContinuationLedger(wrongRoute, ["completed", "held"]), false);
  const wrongRole = ledger();
  wrongRole[1].contract.role = "chat_tool_initial";
  assert.equal(exactChatToolContinuationLedger(wrongRole, ["completed", "held"]), false);
});

test("held continuation requires exact tool replay and exposes no Assistant or ChatML marker", () => {
  const sample = { surface: baseSurface(), ledger: ledger() };
  assert.deepEqual(chatToolContinuationHeldFailures(sample), []);

  const projectedLeak = structuredClone(sample);
  projectedLeak.surface.projection.transcript_rows.push({
    row_kind: "assistant",
    stable_history_identity: null,
    body: "<|im_start|>",
  });
  assert.match(chatToolContinuationHeldFailures(projectedLeak).join(","), /assistant-row|control-token/u);

  const domLeak = structuredClone(sample);
  domLeak.surface.thread_text += "\n<|im_end|>";
  assert.match(chatToolContinuationHeldFailures(domLeak).join(","), /control-token/u);

  const ordinaryAssistant = structuredClone(sample);
  ordinaryAssistant.surface.assistants.push({ history_identity: null, text: "unexpected", visible: true });
  assert.match(chatToolContinuationHeldFailures(ordinaryAssistant).join(","), /assistant-row/u);

  const replayContent = structuredClone(sample);
  replayContent.ledger[1].contract.role_evidence.assistant_content_absent = false;
  assert.match(chatToolContinuationHeldFailures(replayContent).join(","), /continuation-request/u);
});

test("terminal continuation keeps one canonical tool result and one exact final Assistant", () => {
  const sample = { surface: terminalSurface(), ledger: ledger(["completed", "completed"]) };
  assert.deepEqual(chatToolContinuationTerminalFailures(sample, TIME), []);

  const leaked = structuredClone(sample);
  leaked.surface.projection.transcript_rows.at(-1).body = `<|im_start|>${SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_RESPONSE}`;
  assert.match(chatToolContinuationTerminalFailures(leaked, TIME).join(","), /assistant-not-exact|control-token/u);

  const extraAssistant = structuredClone(sample);
  extraAssistant.surface.projection.transcript_rows.push({
    row_kind: "assistant",
    stable_history_identity: null,
    body: "extra",
  });
  assert.match(chatToolContinuationTerminalFailures(extraAssistant, TIME).join(","), /history-order|assistant-not-exact/u);

  const durableError = structuredClone(sample);
  durableError.surface.projection.transcript_rows.splice(2, 0, {
    row_kind: "error",
    stable_history_identity: null,
    body: "unexpected",
  });
  assert.match(chatToolContinuationTerminalFailures(durableError, TIME).join(","), /error-present/u);

  const identityDrift = structuredClone(sample);
  identityDrift.surface.projection.transcript_rows[1].stable_history_identity = "not-canonical";
  assert.match(chatToolContinuationTerminalFailures(identityDrift, TIME).join(","), /work-summary-not-canonical/u);

  const toolDrift = structuredClone(sample);
  toolDrift.surface.projection.tool_status_text = toolDrift.surface.projection.tool_status_text.replace(
    TIME.unixMs,
    "1787715296001",
  );
  assert.match(chatToolContinuationTerminalFailures(toolDrift, TIME).join(","), /time-evidence/u);
});

test("provider.chat-tool-continuation factory binds common terminal and SQLite owners", () => {
  const scenario = createProviderChatToolContinuationScenario();
  assert.equal(scenario.id, "provider.chat-tool-continuation");
  assert.equal(scenario.productOracle, "pass");
  assert.equal(scenario.manualGate, "not_required");
  assert.equal(scenario.databaseRequired, true);
  for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) {
    assert.equal(typeof scenario[method], "function", method);
  }
});

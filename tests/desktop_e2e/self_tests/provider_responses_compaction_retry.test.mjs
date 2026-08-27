import assert from "node:assert/strict";
import test from "node:test";

import {
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_CHECKPOINT,
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES,
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_PROMPT,
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RAW_REASONING_SENTINEL,
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RESPONSE,
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT,
} from "../drivers/scripted_provider.mjs";
import {
  PROVIDER_RESPONSES_COMPACTION_CONTEXT_WINDOW,
  PROVIDER_RESPONSES_COMPACTION_INPUT_BYTE_THRESHOLD,
  PROVIDER_RESPONSES_COMPACTION_SENTINEL_LINE_BYTES,
  createProviderResponsesCompactionRetryScenario,
  exactResponsesCompactionLedger,
  providerResponsesCompactionFixtureConfig,
  providerResponsesCompactionSentinelText,
  responsesCompactionReasoningHeldDecision,
  responsesCompactionReasoningHeldFailures,
  responsesCompactionTerminalDecision,
  responsesCompactionTerminalFailures,
} from "../scenarios/provider_responses_compaction_retry.mjs";

const SESSION_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const TURN_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const USER_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const HASH = "a".repeat(64);
const OUTPUT_HASHES = Object.freeze(Array.from(
  { length: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT },
  (_, index) => String(index + 1).repeat(64),
));
const OUTPUT_SIZES = Object.freeze(Array.from(
  { length: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT },
  (_, index) => PROVIDER_RESPONSES_COMPACTION_SENTINEL_LINE_BYTES
    + (index + 1 === SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT ? 3 : 94),
));

function outputEvidence(start, count) {
  return {
    read_output_size_bytes: OUTPUT_SIZES.slice(start, start + count),
    read_output_sha256: OUTPUT_HASHES.slice(start, start + count),
  };
}

function tools() {
  return {
    pass: true,
    unique_tool_names: true,
    read_present: true,
    read_schema_matches: true,
  };
}

function commonContract(role, evidence, toolBearing) {
  return {
    pass: true,
    role,
    model_matches: true,
    instructions_non_empty: true,
    instructions_sha256: HASH,
    top_level_keys_match: true,
    stream_true: true,
    store_false: true,
    max_output_tokens_absent: true,
    client_generation_fields_absent: true,
    client_generation_fields_present: [],
    tool_choice_auto: toolBearing ? true : null,
    parallel_tool_calls_false: toolBearing ? true : null,
    compaction_tools_absent: toolBearing ? null : true,
    tools: toolBearing ? tools() : null,
    role_evidence: {
      input_size_bytes: 1_000,
      input_byte_threshold: PROVIDER_RESPONSES_COMPACTION_INPUT_BYTE_THRESHOLD,
      ...evidence,
    },
  };
}

function readContract(index) {
  return commonContract(`read_${index + 1}`, {
    matches: true,
    first_prompt_matches: true,
    terminal_prompt_matches: true,
    terminal_prompt_sha256: null,
    source_read_count: index,
    source_unit_count: index + 1,
    input_count: 1 + index * 2,
    input_item_types: [
      "message",
      ...Array.from({ length: index }, () => ["function_call", "function_call_output"]).flat(),
    ],
    source_unit_signatures: Array(index + 1).fill(HASH),
    ...outputEvidence(0, index),
  }, true);
}

function row(contract) {
  return {
    route: "responses",
    method: "POST",
    pathname: "/v1/responses",
    query_present: false,
    response_phase: "completed",
    response_status: 200,
    contract,
  };
}

function reasoningStream(held) {
  const eventTypes = [
    "response.output_item.added",
    "response.reasoning_text.delta",
    "response.reasoning_text.done",
    "response.output_item.done",
    "response.completed",
  ];
  const selected = held ? eventTypes.slice(0, 2) : eventTypes;
  return {
    schema_version: "desktop-e2e.scripted-provider-held-stream.v1",
    hold_after_event_type: "response.reasoning_text.delta",
    events: selected.map((eventType, index) => ({
      sequence: index + 1,
      event_type: eventType,
      elapsed_ms: index,
      size_bytes: 100 + index,
    })),
    release_observed: !held,
    release_elapsed_ms: held ? null : 2,
    terminal_sent: !held,
    response_finished: !held,
    peer_close_observed: false,
  };
}

function exactLedger() {
  const readRows = Array.from(
    { length: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT },
    (_, index) => row(readContract(index)),
  );
  const firstEvidence = {
    matches: true,
    first_prompt_matches: true,
    terminal_prompt_matches: true,
    terminal_prompt_sha256: HASH,
    input_exceeds_threshold: true,
    bounded_prefix_candidate: true,
    input_size_bytes: PROVIDER_RESPONSES_COMPACTION_INPUT_BYTE_THRESHOLD + 5_000,
    source_read_count: 3,
    source_unit_count: 4,
    source_unit_signatures: Array(4).fill(HASH),
    ...outputEvidence(0, 3),
  };
  const retryEvidence = {
    matches: true,
    first_prompt_matches: true,
    terminal_prompt_matches: true,
    terminal_prompt_sha256: HASH,
    input_exceeds_threshold: false,
    bounded_prefix_candidate: true,
    input_size_bytes: PROVIDER_RESPONSES_COMPACTION_INPUT_BYTE_THRESHOLD - 5_000,
    source_read_count: 1,
    source_unit_count: 2,
    source_unit_signatures: Array(2).fill(HASH),
    ...outputEvidence(0, 1),
  };
  const first = commonContract("compaction_empty", firstEvidence, false);
  const retry = commonContract("compaction_valid", retryEvidence, false);
  retry.retry_alignment = {
    pass: true,
    strict_prefix: true,
    first_source_unit_count: firstEvidence.source_unit_count,
    retry_source_unit_count: retryEvidence.source_unit_count,
    first_input_size_bytes: firstEvidence.input_size_bytes,
    retry_input_size_bytes: retryEvidence.input_size_bytes,
  };
  const final = commonContract("resumed_final", {
    matches: true,
    first_prompt_matches: true,
    checkpoint_matches: true,
    checkpoint_sha256: HASH,
    first_remaining_read_index: 2,
    remaining_read_count: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT - 1,
    ...outputEvidence(1, SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT - 1),
  }, true);
  final.compaction_replay = {
    pass: true,
    summarized_read_count: 1,
    first_remaining_read_index: 2,
    remaining_read_count: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT - 1,
  };
  const firstRow = row(first);
  firstRow.response_stream = reasoningStream(false);
  return [...readRows, firstRow, row(retry), row(final)];
}

function heldLedger() {
  const ledger = exactLedger().slice(0, SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT + 1);
  ledger.at(-1).response_phase = "held";
  ledger.at(-1).response_stream = reasoningStream(true);
  return ledger;
}

function heldResource() {
  const heldCount = SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT + 1;
  return {
    script_kind: "responses_compaction_retry",
    request_count: heldCount,
    active_request_count: 1,
    open_connection_count: 1,
    accepted_response_count: heldCount,
    successful_response_count: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT,
    scripted_responses_request_count: heldCount,
    scripted_responses_maximum: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES,
    scripted_response_roles: [
      ...Array.from(
        { length: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT },
        (_, index) => `read_${index + 1}`,
      ),
      "compaction_empty",
    ],
    response_release_controlled: true,
    response_release_count: 0,
    response_release_cleanup_count: 0,
    script_role_release: {
      role: "compaction_empty",
      released: false,
      released_by_cleanup: false,
    },
  };
}

function terminalResource() {
  return {
    script_kind: "responses_compaction_retry",
    request_count: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES,
    active_request_count: 0,
    accepted_response_count: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES,
    successful_response_count: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES,
    scripted_responses_request_count: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES,
    scripted_responses_maximum: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES,
    scripted_response_roles: [
      ...Array.from(
        { length: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT },
        (_, index) => `read_${index + 1}`,
      ),
      "compaction_empty",
      "compaction_valid",
      "resumed_final",
    ],
    response_release_controlled: true,
    response_release_count: 0,
    response_release_cleanup_count: 0,
    script_role_release: {
      role: "compaction_empty",
      released: true,
      released_by_cleanup: false,
    },
  };
}

function projection(overrides = {}) {
  return {
    run_status_key: "completed",
    task_activity_state: "idle",
    busy: false,
    agent_tree_active: false,
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
    draft_target: { sessionId: SESSION_ID },
    selected_project_index: -1,
    selected_session_index: 0,
    project_rows: [],
    chat_session_rows: [{ session_id: SESSION_ID }],
    transcript_rows: [
      {
        row_kind: "user",
        stable_history_identity: USER_ID,
        body: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_PROMPT,
      },
      {
        row_kind: "system",
        title: "システム - Context Compaction",
        body: `圧縮しました\n\n${SCRIPTED_PROVIDER_RESPONSES_COMPACTION_CHECKPOINT}`,
      },
      {
        row_kind: "work_summary_completed",
        stable_history_identity: `turn:${TURN_ID}:work-summary`,
        body: "eight reads completed",
      },
      {
        row_kind: "assistant",
        body: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RESPONSE,
      },
    ],
    progress_text: `Completed\nモデル要求: ${SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES}\nツール: ${SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT}件開始 / ${SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT}件完了 / 0件拒否 / 0件キャンセル / 0件失敗\n圧縮: 1`,
    ...overrides,
  };
}

function surface(overrides = {}) {
  return {
    projection: projection(),
    thread_count: 1,
    users: [{ history_identity: USER_ID, text: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_PROMPT, visible: true }],
    assistants: [{ history_identity: null, text: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RESPONSE, visible: true }],
    completed_summaries: [{ history_identity: `turn:${TURN_ID}:work-summary`, text: "eight reads completed", visible: true }],
    all_transcript_rows: [
      { classes: ["message", "user"], text: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_PROMPT, visible: true },
      {
        classes: ["message", "system"],
        text: `システム - Context Compaction\n圧縮しました\n\n${SCRIPTED_PROVIDER_RESPONSES_COMPACTION_CHECKPOINT}`,
        visible: true,
      },
      { classes: ["message", "work-summary"], text: "eight reads completed", visible: true },
      { classes: ["assistant", "message"], text: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RESPONSE, visible: true },
    ],
    selected_navigation: [{ action: "chat-session", focus_key: `chat-session:${SESSION_ID}:select`, visible: true }],
    prompt: { count: 1, value: "", visible: true, enabled: true },
    send: { count: 1, visible: true, enabled: false },
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
    ...overrides,
  };
}

function heldSurface() {
  const value = surface();
  value.projection.run_status_key = "running";
  value.projection.task_activity_state = "running";
  value.projection.busy = true;
  value.projection.run_phase = "Provider応答受信中";
  value.projection.post_run_refresh_pending = false;
  value.projection.async_polling_required = true;
  value.projection.pending_async_operations = ["run_poll"];
  value.projection.composer_submit_mode = "stop";
  value.projection.can_submit = false;
  return value;
}

test("Responses compaction GUI fixture is host-owned and reaches the 32K stress boundary", () => {
  const config = providerResponsesCompactionFixtureConfig("http://127.0.0.1:43123");
  assert.match(config, /provider_profile = "openai_responses"/u);
  assert.match(config, new RegExp(`context_window = ${PROVIDER_RESPONSES_COMPACTION_CONTEXT_WINDOW}`, "u"));
  assert.match(config, /supports_tools = true/u);
  assert.match(config, /parallel_tool_calls = false/u);
  assert.match(config, /max_retries = 0/u);
  assert.doesNotMatch(config, /api_key(?:_env)? =/u);
  assert.doesNotMatch(config, /max_(?:output_)?tokens|reasoning_(?:effort|summary)|supports_reasoning|temperature|top_p|top_k|presence_penalty|frequency_penalty|seed\s*=|stop(?:_sequences)?\s*=|\[model\.extra_body_json\]/u);

  const sentinel = providerResponsesCompactionSentinelText();
  const lines = sentinel.trimEnd().split("\n");
  assert.equal(lines.length, SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT);
  assert.equal(lines.every((line) => Buffer.byteLength(line, "utf8") === PROVIDER_RESPONSES_COMPACTION_SENTINEL_LINE_BYTES), true);
});

test("Responses compaction ledger requires exact bounded-prefix retry and host-owned wire", () => {
  const ledger = exactLedger();
  assert.equal(ledger.length, SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES);
  assert.equal(exactResponsesCompactionLedger(ledger), true);

  const generation = structuredClone(ledger);
  generation[0].contract.client_generation_fields_absent = false;
  generation[0].contract.client_generation_fields_present = ["temperature"];
  assert.equal(exactResponsesCompactionLedger(generation), false);

  const compactionTools = structuredClone(ledger);
  compactionTools[8].contract.compaction_tools_absent = false;
  assert.equal(exactResponsesCompactionLedger(compactionTools), false);

  const initialInstructionDrift = structuredClone(ledger);
  initialInstructionDrift[8].contract.instructions_sha256 = "b".repeat(64);
  assert.equal(exactResponsesCompactionLedger(initialInstructionDrift), false);

  const retryInstructionDrift = structuredClone(ledger);
  retryInstructionDrift[9].contract.instructions_sha256 = "b".repeat(64);
  assert.equal(exactResponsesCompactionLedger(retryInstructionDrift), false);

  const allHistory = structuredClone(ledger);
  allHistory[8].contract.role_evidence.bounded_prefix_candidate = false;
  assert.equal(exactResponsesCompactionLedger(allHistory), false);

  const unaligned = structuredClone(ledger);
  unaligned[9].contract.retry_alignment.strict_prefix = false;
  assert.equal(exactResponsesCompactionLedger(unaligned), false);

  const duplicate = [...ledger, structuredClone(ledger.at(-1))];
  assert.equal(exactResponsesCompactionLedger(duplicate), false);
});

test("Responses compaction ledger rejects tool-output identity mutation across every replay surface", () => {
  const ledger = exactLedger();
  const normalOutputMutation = structuredClone(ledger);
  normalOutputMutation[4].contract.role_evidence.read_output_sha256[2] = "e".repeat(64);
  assert.equal(exactResponsesCompactionLedger(normalOutputMutation), false);

  const initialCompactionMutation = structuredClone(ledger);
  initialCompactionMutation[8].contract.role_evidence.read_output_size_bytes[1] += 1;
  assert.equal(exactResponsesCompactionLedger(initialCompactionMutation), false);

  const retryOutputMutation = structuredClone(ledger);
  retryOutputMutation[9].contract.role_evidence.read_output_sha256[0] = "f".repeat(64);
  assert.equal(exactResponsesCompactionLedger(retryOutputMutation), false);

  const resumedOverlapMutation = structuredClone(ledger);
  resumedOverlapMutation[10].contract.role_evidence.read_output_sha256[0] = "e".repeat(64);
  assert.equal(exactResponsesCompactionLedger(resumedOverlapMutation), false);
});

test("Responses compaction ledger rejects a duplicated or truncated final read identity", () => {
  const ledger = exactLedger();
  const duplicatedLastRead = structuredClone(ledger);
  duplicatedLastRead[10].contract.role_evidence.read_output_sha256[
    duplicatedLastRead[10].contract.role_evidence.read_output_sha256.length - 1
  ] = OUTPUT_HASHES.at(-2);
  assert.equal(exactResponsesCompactionLedger(duplicatedLastRead), false);

  const truncatedSuffix = structuredClone(ledger);
  truncatedSuffix[10].contract.role_evidence.read_output_size_bytes.pop();
  truncatedSuffix[10].contract.role_evidence.read_output_sha256.pop();
  assert.equal(exactResponsesCompactionLedger(truncatedSuffix), false);
});

test("Responses compaction reasoning hold requires an active private delta and exact release owner", () => {
  const sample = {
    ledger: heldLedger(),
    resource: heldResource(),
    surface: heldSurface(),
  };
  assert.deepEqual(responsesCompactionReasoningHeldFailures(sample), []);
  assert.equal(responsesCompactionReasoningHeldDecision(sample), "pass");

  const prematureCompletion = structuredClone(sample);
  prematureCompletion.ledger.at(-1).response_phase = "completed";
  assert.equal(responsesCompactionReasoningHeldDecision(prematureCompletion), "fail");

  const releasedWithoutAcquisition = structuredClone(sample);
  releasedWithoutAcquisition.resource.script_role_release.released = true;
  assert.match(
    responsesCompactionReasoningHeldFailures(releasedWithoutAcquisition).join(","),
    /reasoning-release-owner/u,
  );
  assert.equal(responsesCompactionReasoningHeldDecision(releasedWithoutAcquisition), "fail");
});

test("Responses compaction reasoning hold rejects transient projection and DOM disclosure", () => {
  const projectionLeak = {
    ledger: heldLedger(),
    resource: heldResource(),
    surface: heldSurface(),
  };
  projectionLeak.surface.projection.transcript_rows.push({
    row_kind: "reasoning_summary",
    body: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RAW_REASONING_SENTINEL,
  });
  assert.match(
    responsesCompactionReasoningHeldFailures(projectionLeak).join(","),
    /raw-reasoning-visible-in-flight/u,
  );
  assert.equal(responsesCompactionReasoningHeldDecision(projectionLeak), "fail");

  const domLeak = {
    ledger: heldLedger(),
    resource: heldResource(),
    surface: heldSurface(),
  };
  domLeak.surface.all_transcript_rows.push({
    classes: ["message", "reasoning_summary"],
    text: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RAW_REASONING_SENTINEL,
    visible: true,
  });
  assert.match(
    responsesCompactionReasoningHeldFailures(domLeak).join(","),
    /raw-reasoning-visible-in-flight/u,
  );
  assert.equal(responsesCompactionReasoningHeldDecision(domLeak), "fail");
});

test("Responses compaction terminal requires one canonical checkpoint and visible final answer", () => {
  const sample = { ledger: exactLedger(), resource: terminalResource(), surface: surface() };
  assert.deepEqual(responsesCompactionTerminalFailures(sample), []);
  assert.equal(responsesCompactionTerminalDecision(sample), "pass");

  const missingCompaction = structuredClone(sample);
  missingCompaction.surface.projection.transcript_rows.splice(1, 1);
  assert.match(responsesCompactionTerminalFailures(missingCompaction).join(","), /canonical-compaction/u);

  const wrongProgress = structuredClone(sample);
  wrongProgress.surface.projection.progress_text = wrongProgress.surface.projection.progress_text.replace(
    "圧縮: 1",
    "圧縮: 0",
  );
  assert.match(responsesCompactionTerminalFailures(wrongProgress).join(","), /progress-counts/u);

  const cleanupRelease = structuredClone(sample);
  cleanupRelease.resource.script_role_release.released_by_cleanup = true;
  assert.match(
    responsesCompactionTerminalFailures(cleanupRelease).join(","),
    /reasoning-release-not-exact/u,
  );

  const fatal = structuredClone(sample);
  fatal.surface.visible_fatal_count = 1;
  assert.equal(responsesCompactionTerminalDecision(fatal), "fail");

  const pending = structuredClone(sample);
  pending.surface.projection.run_status_key = "running";
  pending.surface.projection.task_activity_state = "running";
  pending.surface.projection.busy = true;
  assert.equal(responsesCompactionTerminalDecision(pending), "pending");
});

test("Responses compaction terminal rejects raw reasoning in projection or any DOM transcript row", () => {
  const projectionLeak = {
    ledger: exactLedger(),
    resource: terminalResource(),
    surface: surface(),
  };
  projectionLeak.surface.projection.transcript_rows.push({
    row_kind: "reasoning_summary",
    body: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RAW_REASONING_SENTINEL,
  });
  assert.match(responsesCompactionTerminalFailures(projectionLeak).join(","), /raw-reasoning-visible/u);
  assert.equal(responsesCompactionTerminalDecision(projectionLeak), "fail");

  const domLeak = { ledger: exactLedger(), resource: terminalResource(), surface: surface() };
  domLeak.surface.all_transcript_rows.push({
    classes: ["message", "reasoning_summary"],
    text: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RAW_REASONING_SENTINEL,
    visible: true,
  });
  assert.match(responsesCompactionTerminalFailures(domLeak).join(","), /raw-reasoning-visible/u);
  assert.equal(responsesCompactionTerminalDecision(domLeak), "fail");
});

test("provider.responses-compaction-retry factory binds common GUI, terminal, and SQLite owners", () => {
  const scenario = createProviderResponsesCompactionRetryScenario();
  assert.equal(scenario.id, "provider.responses-compaction-retry");
  assert.equal(scenario.productOracle, "pass");
  assert.equal(scenario.manualGate, "not_required");
  assert.equal(scenario.databaseRequired, true);
  for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) {
    assert.equal(typeof scenario[method], "function", method);
  }
});

import assert from "node:assert/strict";
import test from "node:test";

import {
  PROVIDER_RESPONSES_PROGRESS_CADENCE_MS,
  PROVIDER_RESPONSES_PROGRESS_DELTA_COUNT,
  PROVIDER_RESPONSES_PROGRESS_RESPONSE,
  PROVIDER_RESPONSES_PROGRESS_TIMEOUT_MS,
  providerResponsesProgressDecision,
  providerResponsesProgressFixtureConfig,
  providerResponsesProgressTerminalDecision,
} from "../scenarios/provider_responses_progress.mjs";
import { PROVIDER_RESTART_PROMPT } from "../scenarios/provider_restart.mjs";

const SESSION_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const EVENT_TYPES = [
  ...Array(PROVIDER_RESPONSES_PROGRESS_DELTA_COUNT).fill("response.output_text.delta"),
  "response.output_item.done",
  "response.completed",
];
const EVENT_TIMES = EVENT_TYPES.map((_, index) => 5 + (index * PROVIDER_RESPONSES_PROGRESS_CADENCE_MS));

function events(count = EVENT_TYPES.length) {
  return EVENT_TYPES.slice(0, count).map((eventType, index) => ({
    sequence: index + 1,
    event_type: eventType,
    elapsed_ms: EVENT_TIMES[index],
    size_bytes: 64,
  }));
}

function stream({ eventCount = EVENT_TYPES.length, terminal = true } = {}) {
  return {
    schema_version: "desktop-e2e.scripted-provider-response-stream.v1",
    cadence_ms: PROVIDER_RESPONSES_PROGRESS_CADENCE_MS,
    delta_count: PROVIDER_RESPONSES_PROGRESS_DELTA_COUNT,
    configured_total_duration_ms: 6_800,
    expected_event_count: EVENT_TYPES.length,
    headers_sent_elapsed_ms: 1,
    events: events(eventCount),
    terminal_sent: terminal,
    terminal_elapsed_ms: terminal ? EVENT_TIMES.at(-1) : null,
    response_finished: terminal,
    response_finished_elapsed_ms: terminal ? EVENT_TIMES.at(-1) + 1 : null,
    peer_close_observed: false,
    peer_close_elapsed_ms: null,
    peer_closed_before_terminal: false,
  };
}

function row({ eventCount = EVENT_TYPES.length, terminal = true } = {}) {
  return {
    method: "POST",
    pathname: "/v1/responses",
    query_present: false,
    response_status: 200,
    response_phase: terminal ? "completed" : "streaming",
    contract: {
      pass: true,
      client_generation_fields_absent: true,
      client_generation_fields_present: [],
    },
    response_stream: stream({ eventCount, terminal }),
  };
}

function runningSurface() {
  return {
    projection: {
      startup: { status: "ready" },
      run_status_key: "running",
      task_activity_state: "running",
      busy: true,
      run_phase: "Provider応答受信中",
      run_active_step: "Provider request req-1 first_progress via http://127.0.0.1 (attempt 1, 1205 ms)",
      transcript_rows: [{ row_kind: "user", body: PROVIDER_RESTART_PROMPT }],
    },
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
  };
}

function terminalSurface() {
  const userIdentity = `turn:${SESSION_ID}:user`;
  const summaryIdentity = `turn:${SESSION_ID}:work-summary`;
  const assistantIdentity = `turn:${SESSION_ID}:assistant`;
  const transcriptRows = [
    { row_kind: "user", body: PROVIDER_RESTART_PROMPT, stable_history_identity: userIdentity },
    { row_kind: "work_summary_completed", body: "completed", stable_history_identity: summaryIdentity },
    { row_kind: "assistant", body: PROVIDER_RESPONSES_PROGRESS_RESPONSE, stable_history_identity: assistantIdentity },
  ];
  return {
    projection: {
      startup: { status: "ready" },
      run_status_key: "completed",
      task_activity_state: "idle",
      busy: false,
      agent_tree_active: false,
      post_run_refresh_pending: false,
      background_mutation_pending: false,
      async_polling_required: false,
      pending_async_operations: [],
      navigation_loading: false,
      navigation_admission_open: true,
      provider_loading: false,
      overlay: "none",
      confirmation_visible: false,
      confirmation_id: null,
      confirmation: null,
      draft_prompt: "",
      composer_submit_mode: "new_request",
      can_submit: true,
      transcript_rows: transcriptRows,
      selected_project_index: -1,
      selected_session_index: 0,
      project_rows: [],
      session_rows: [],
      chat_session_rows: [{ session_id: SESSION_ID }],
      workspace_path: null,
    },
    thread_count: 1,
    users: [{ history_identity: userIdentity, text: PROVIDER_RESTART_PROMPT, visible: true }],
    assistants: [{ history_identity: assistantIdentity, text: PROVIDER_RESPONSES_PROGRESS_RESPONSE, visible: true }],
    completed_summaries: [{ history_identity: summaryIdentity, text: "completed", visible: true }],
    selected_navigation: [{
      action: "chat-session",
      focus_key: `chat-session:${SESSION_ID}:select`,
      visible: true,
    }],
    prompt: { count: 1, value: "", visible: true, enabled: true },
    send: { count: 1, visible: true, enabled: true },
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
    visible_dialog_count: 0,
    visible_modal_backdrop_count: 0,
  };
}

test("progress fixture config uses one short client liveness interval without host generation controls", () => {
  const config = providerResponsesProgressFixtureConfig("http://127.0.0.1:4444");
  assert.match(config, /provider_profile = "openai_responses"/);
  assert.match(config, new RegExp(`request_timeout_ms = ${PROVIDER_RESPONSES_PROGRESS_TIMEOUT_MS}`));
  assert.doesNotMatch(config, /temperature|top_p|top_k|min_p|reasoning|thinking|effort|stop|max_output_tokens|max_tokens|extra_body/);
});

test("progress oracle accepts a live GUI stream beyond total timeout but rejects replay and early close", () => {
  const sample = { surface: runningSurface(), ledger: [row({ eventCount: 4, terminal: false })] };
  assert.equal(providerResponsesProgressDecision(sample), "pass");

  const beforeBoundary = structuredClone(sample);
  beforeBoundary.ledger[0].response_stream.events[3].elapsed_ms = 899;
  assert.equal(providerResponsesProgressDecision(beforeBoundary), "pending");

  const wrongVisiblePhase = structuredClone(sample);
  wrongVisiblePhase.surface.projection.run_phase = "Provider応答ヘッダー受信";
  wrongVisiblePhase.surface.projection.run_active_step = "Provider応答受信中";
  assert.equal(providerResponsesProgressDecision(wrongVisiblePhase), "pending");

  const replayed = structuredClone(sample);
  replayed.ledger.push(row({ eventCount: 1, terminal: false }));
  assert.equal(providerResponsesProgressDecision(replayed), "fail");

  const closed = structuredClone(sample);
  closed.ledger[0].response_stream.peer_closed_before_terminal = true;
  assert.equal(providerResponsesProgressDecision(closed), "fail");
});

test("terminal oracle requires the exact paced wire, settled GUI, and one host-neutral request", () => {
  const sample = { surface: terminalSurface(), ledger: [row()] };
  assert.equal(providerResponsesProgressTerminalDecision(sample), "pass");

  const missingEvent = structuredClone(sample);
  missingEvent.ledger[0].response_stream.events.pop();
  assert.equal(providerResponsesProgressTerminalDecision(missingEvent), "pending");

  const failedGui = structuredClone(sample);
  failedGui.surface.projection.run_status_key = "failed";
  assert.equal(providerResponsesProgressTerminalDecision(failedGui), "fail");
});

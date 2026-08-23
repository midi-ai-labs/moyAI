import assert from "node:assert/strict";
import test from "node:test";

import { DesktopE2eError } from "../core/execution.mjs";
import { SCRIPTED_PROVIDER_MODEL_ID, SCRIPTED_PROVIDER_RESPONSE } from "../drivers/scripted_provider.mjs";
import {
  PROVIDER_RESTART_PROMPT,
  classifyAcquiredObservationFailure,
  createStableRestartDecision,
  exactFreshProviderHistory,
  exactProviderCatalogLedger,
  exactProviderTurnLedger,
  providerRestartFixtureConfig,
  providerTurnDomAccepted,
  quiesceProviderResource,
  relevantProviderHistory,
  settledCompletedProviderTurn,
} from "../scenarios/provider_restart.mjs";

function catalogRow() {
  return { method: "GET", pathname: "/v1/models", response_status: 200 };
}

function responseRow() {
  return { method: "POST", pathname: "/v1/responses", response_status: 200, contract: { pass: true } };
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
    navigation_admission_open: true,
    provider_loading: false,
    overlay: "none",
    confirmation_visible: false,
    confirmation_id: null,
    confirmation: null,
    draft_prompt: "",
    composer_submit_mode: "new_request",
    can_submit: true,
    workspace_path: null,
    selected_project_index: -1,
    selected_session_index: 0,
    project_rows: [],
    chat_session_rows: [{ session_id: "session-1" }],
    transcript_rows: [
      { row_kind: "user", stable_history_identity: "history-user-1", title: "ユーザー依頼", body: PROVIDER_RESTART_PROMPT },
      { row_kind: "work_summary_completed", stable_history_identity: "turn-1:work-summary", title: "完了", body: "" },
      { row_kind: "assistant", stable_history_identity: null, title: "応答", body: SCRIPTED_PROVIDER_RESPONSE },
    ],
    ...overrides,
  };
}

function surface(overrides = {}) {
  return {
    thread_count: 1,
    users: [{ history_identity: "history-user-1", text: PROVIDER_RESTART_PROMPT, visible: true }],
    assistants: [{ history_identity: null, text: SCRIPTED_PROVIDER_RESPONSE, visible: true }],
    completed_summaries: [{ history_identity: "turn-1:work-summary", text: "", visible: true }],
    selected_navigation: [{ action: "chat-session", focus_key: "chat-session:session-1:select", visible: true }],
    prompt: { count: 1, value: "", visible: true, enabled: true },
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
    visible_dialog_count: 0,
    visible_modal_backdrop_count: 0,
    ...overrides,
  };
}

test("provider restart fixture intentionally covers legacy split-mode normalization and clears the default generation body", () => {
  const config = providerRestartFixtureConfig("http://127.0.0.1:19454");
  assert.match(config, /base_url = "http:\/\/127\.0\.0\.1:19454"/);
  assert.match(config, /provider_metadata_mode = "openai_compatible_only"/);
  assert.match(config, /provider_api_mode = "responses"/);
  assert.doesNotMatch(config, /provider_profile\s*=/);
  assert.match(config, /\[model\.extra_body_json\]\r?\n\r?\n\[permissions\]/);
});

test("fresh provider terminal requires exact transcript, idle projection, and rendered DOM", () => {
  const value = projection();
  const history = relevantProviderHistory(value);
  const identity = { project_id: null, session_id: "session-1" };
  assert.equal(exactFreshProviderHistory(history), true);
  assert.equal(settledCompletedProviderTurn(value), true);
  assert.equal(providerTurnDomAccepted(surface(), history, identity), true);

  const extra = projection({ transcript_rows: [...value.transcript_rows, { row_kind: "user", stable_history_identity: "extra", body: "extra" }] });
  assert.equal(exactFreshProviderHistory(relevantProviderHistory(extra)), false);
  const reordered = projection({ transcript_rows: [value.transcript_rows[0], value.transcript_rows[2], value.transcript_rows[1]] });
  assert.equal(exactFreshProviderHistory(relevantProviderHistory(reordered)), false);
  assert.equal(settledCompletedProviderTurn(projection({ overlay: "shortcuts" })), false);
  assert.equal(providerTurnDomAccepted(surface({ prompt: { count: 1, value: "stale", visible: true, enabled: true } }), history, identity), false);
});

test("restart restore must remain continuously accepted across a later observation window", () => {
  let nowMs = 1_000;
  const expected = {
    identity: {
      workspace_path: null,
      project_id: null,
      project_path: null,
      session_id: "session-1",
      project_row_ids: [],
      session_row_ids: ["session-1"],
    },
    history: relevantProviderHistory(projection()),
  };
  const accepted = { surface: { ...surface(), projection: projection() }, ledger: [catalogRow(), responseRow()] };
  const pending = {
    surface: { ...surface(), projection: projection({ navigation_loading: true }) },
    ledger: [catalogRow(), responseRow()],
  };
  const decide = createStableRestartDecision({ expected, minimumStableMs: 500, now: () => nowMs });

  assert.equal(decide(accepted), "pending");
  nowMs += 400;
  assert.equal(decide(accepted), "pending");
  assert.equal(decide(pending), "pending");
  nowMs += 500;
  assert.equal(decide(accepted), "pending");
  nowMs += 500;
  assert.equal(decide(accepted), "pass");
});

test("provider ledger contracts reject missing, duplicate, wrong-status, and malformed requests", () => {
  const catalog = [catalogRow()];
  const turn = [catalogRow(), responseRow()];
  assert.equal(exactProviderCatalogLedger(catalog), true);
  assert.equal(exactProviderTurnLedger(turn), true);
  assert.equal(exactProviderCatalogLedger([]), false);
  assert.equal(exactProviderTurnLedger([...turn, responseRow()]), false);
  assert.equal(exactProviderTurnLedger([catalogRow(), { ...responseRow(), response_status: 422 }]), false);
  assert.equal(exactProviderTurnLedger([catalogRow(), { ...responseRow(), contract: { pass: false } }]), false);
});

test("only a reachable final observation without a transport error becomes product failure", () => {
  const timeout = new Error("timed out");
  timeout.code = "observation-timeout";
  timeout.evidence = { last_value: { projection: { overlay: "none" } }, last_error: null };
  const classified = classifyAcquiredObservationFailure(timeout, { code: "predicate-failed", message: "predicate failed" });
  assert.ok(classified instanceof DesktopE2eError);
  assert.equal(classified.owner, "product");
  assert.equal(classified.code, "predicate-failed");

  const disconnected = new Error("timed out after disconnect");
  disconnected.code = "observation-timeout";
  disconnected.evidence = { last_value: { projection: {} }, last_error: "cdp disconnected" };
  assert.equal(classifyAcquiredObservationFailure(disconnected, { code: "unused", message: "unused" }), disconnected);
});

test("provider quiesce detects late replay after Desktop zero and closes the resource once", async () => {
  const accepted = [catalogRow(), responseRow()];
  const ledger = structuredClone(accepted);
  let closeCount = 0;
  const provider = {
    get requestLedger() { return structuredClone(ledger); },
    async close() {
      closeCount += 1;
      ledger.push({ method: "POST", pathname: "/v1/responses", response_status: 409, contract: { pass: true } });
      return { pass: true, forced_connection_count: 0 };
    },
  };
  const outcome = await quiesceProviderResource({ provider, acceptedLedger: accepted, inputs: { oracle: "pass" } });
  assert.equal(closeCount, 1);
  assert.equal(outcome.input, "pass");
  assert.equal(outcome.productFailure.code, "provider-post-terminal-wire-drift");
  assert.equal(outcome.resources[0].final_ledger.length, 3);
});

test("provider resource or accepted-ledger failure is cleanup-owned and never fakes a product pass", async () => {
  const provider = {
    get requestLedger() { return [catalogRow(), responseRow()]; },
    async close() { return { pass: false, forced_connection_count: 1 }; },
  };
  const failedClose = await quiesceProviderResource({ provider, acceptedLedger: [catalogRow(), responseRow()], inputs: { oracle: "pass" } });
  assert.equal(failedClose.input, "fail");
  assert.equal(failedClose.productFailure, null);

  const missingAccepted = await quiesceProviderResource({
    provider: { get requestLedger() { return []; }, async close() { return { pass: true, forced_connection_count: 0 }; } },
    acceptedLedger: null,
    inputs: { oracle: "pass" },
  });
  assert.equal(missingAccepted.input, "fail");
});

import assert from "node:assert/strict";
import test from "node:test";

import { SCRIPTED_PROVIDER_MAX_TURNS } from "../drivers/scripted_provider.mjs";
import { classifyRestartTurnPage } from "../core/history_restart_contract.mjs";
import {
  createHistoryRestartPrependScenario,
  exactHistoryFixtureLedger,
  expectedPreviousTurnPageCommand,
  historyConversationRows,
  historyFixtureConversationFailures,
  historyFixtureThresholdReached,
  historyFixtureTurnDecision,
  historyPrependTranscriptFailures,
  historyRestartFixtureTurns,
  historyRestartOwner,
} from "../scenarios/history_restart_prepend.mjs";

const SESSION_ID = "01K3HISTORYSESSION0000000000";
const TURN_ID = "01K3HISTORYTURN000000000000";

function projection(overrides = {}) {
  const prompt = "history fixture prompt 01";
  const response = "HISTORY_FIXTURE_RESPONSE_01";
  return {
    workspace_path: "C:\\workspace",
    selected_project_index: 0,
    project_rows: [{ project_id: "01K3HISTORYPROJECT000000000" }],
    selected_session_index: 0,
    session_rows: [{ session_id: SESSION_ID, admission_revision: "8" }],
    chat_session_rows: [],
    run_target: {
      expectedState: {
        kind: "idle",
        latestTurnId: TURN_ID,
        admissionRevision: "8",
      },
    },
    draft_target: { workspacePath: "C:\\workspace", sessionId: SESSION_ID, ownerGeneration: "4" },
    run_status_key: "completed",
    task_activity_state: "idle",
    busy: false,
    agent_tree_active: false,
    post_run_refresh_pending: false,
    background_mutation_pending: false,
    async_polling_required: false,
    pending_async_operations: [],
    navigation_loading: false,
    turn_page_admission_open: true,
    provider_loading: false,
    overlay: "none",
    confirmation_visible: false,
    draft_prompt: "",
    composer_submit_mode: "new_request",
    can_submit: true,
    turn_page_offset: 81,
    turn_page_limit: 80,
    turn_page_total: 161,
    turn_page_has_more: false,
    transcript_rows: [
      { row_kind: "user", body: prompt },
      { row_kind: "work_summary_completed", body: "done" },
      { row_kind: "assistant", body: response },
    ],
    ...overrides,
  };
}

function ledger(count = 1) {
  return Array.from({ length: count }, (_, index) => ({
    sequence: index + 1,
    method: "POST",
    pathname: "/v1/responses",
    response_phase: "completed",
    response_status: 200,
    contract: { pass: true },
  }));
}

test("history fixture is bounded and stops only after two previous pages are available", () => {
  const turns = historyRestartFixtureTurns();
  assert.equal(turns.length, SCRIPTED_PROVIDER_MAX_TURNS);
  assert.equal(new Set(turns.map((turn) => turn.prompt)).size, turns.length);
  assert.equal(new Set(turns.map((turn) => turn.responseText)).size, turns.length);
  assert.equal(historyFixtureThresholdReached(projection()), true);
  assert.equal(historyFixtureThresholdReached(projection({ turn_page_offset: 0 })), true);
  assert.equal(historyFixtureThresholdReached(projection({ turn_page_total: 160, turn_page_offset: 0 })), false);
  assert.equal(historyFixtureThresholdReached(projection({ turn_page_offset: 0, turn_page_has_more: true })), false);
  assert.equal(historyFixtureThresholdReached(projection({ turn_page_limit: 0 })), false);
});

test("live fixture range and fresh restart suffix keep distinct offset contracts", () => {
  const live = projection({ turn_page_offset: 0, turn_page_total: 256 });
  const expected = {
    expectedSessionId: SESSION_ID,
    expectedTurnId: TURN_ID,
    expectedAdmissionRevision: "8",
    expectedTotal: 256,
    expectedLimit: 80,
    requireLatestSuffix: true,
  };
  assert.equal(historyFixtureThresholdReached(live), true);
  assert.equal(classifyRestartTurnPage(live, expected).decision, "fail");
  assert.equal(classifyRestartTurnPage(
    projection({ turn_page_offset: 176, turn_page_total: 256 }),
    expected,
  ).decision, "page_needed");
});

test("history fixture terminal requires the exact scripted response and durable owner", () => {
  const expected = {
    prompt: "history fixture prompt 01",
    responseText: "HISTORY_FIXTURE_RESPONSE_01",
    expectedResponseCount: 1,
    expectedSessionId: SESSION_ID,
  };
  assert.equal(exactHistoryFixtureLedger(ledger(), 1), true);
  assert.equal(historyFixtureTurnDecision({ projection: projection(), ledger: ledger() }, expected), "pass");
  assert.equal(historyFixtureTurnDecision({
    projection: projection({ busy: true, run_status_key: "running" }),
    ledger: [],
  }, expected), "pending");
  assert.equal(historyFixtureTurnDecision({
    projection: projection({ transcript_rows: [{ row_kind: "assistant", body: "wrong" }] }),
    ledger: ledger(),
  }, expected), "fail");
  assert.equal(historyFixtureTurnDecision({
    projection: projection(),
    ledger: [{ ...ledger()[0], pathname: "/v1/chat/completions" }],
  }, expected), "fail");
});

test("history restart owner and previous-page command use one exact selected row", () => {
  assert.deepEqual(historyRestartOwner(projection()), {
    sessionId: SESSION_ID,
    turnId: TURN_ID,
    admissionRevision: "8",
    total: 161,
    limit: 80,
  });
  assert.deepEqual(expectedPreviousTurnPageCommand(projection()), {
    command: "load_previous_turn_page",
    args: {
      index: 0,
      expectedTarget: {
        workspacePath: "C:\\workspace",
        ownerProjectId: "01K3HISTORYPROJECT000000000",
        ownerSessionId: SESSION_ID,
        rowId: SESSION_ID,
      },
      expectedOffset: 81,
    },
  });
  assert.equal(expectedPreviousTurnPageCommand(projection({ turn_page_offset: 0 })), null);
  assert.equal(historyRestartOwner(projection({
    session_rows: [{ session_id: SESSION_ID, admission_revision: "9" }],
  })), null);
});

test("history prepend requires real transcript growth and an unchanged durable suffix", () => {
  const first = { row_kind: "user", stable_history_identity: "user-1", body: "first" };
  const second = { row_kind: "assistant", stable_history_identity: null, body: "FIRST" };
  const before = [first, second];
  const after = [
    { row_kind: "user", stable_history_identity: "user-0", body: "earlier" },
    { row_kind: "assistant", stable_history_identity: null, body: "EARLIER" },
    ...before,
  ];
  assert.deepEqual(historyPrependTranscriptFailures({ before, after }), []);
  assert.deepEqual(historyPrependTranscriptFailures({ before, after: before }), [
    "history-prepend-transcript-not-expanded",
  ]);
  assert.deepEqual(historyPrependTranscriptFailures({
    before,
    after: [...after.slice(0, -1), { ...second, body: "rewritten" }],
  }), ["history-prepend-transcript-suffix-drift"]);
});

test("history fixture final conversation is complete, ordered, and identity-stable", () => {
  const turns = [
    { prompt: "first", responseText: "FIRST" },
    { prompt: "second", responseText: "SECOND" },
  ];
  const transcript_rows = turns.flatMap((turn, index) => [
    {
      row_kind: "user",
      stable_history_identity: `user-${index + 1}`,
      body: turn.prompt,
    },
    { row_kind: "work_summary_completed", stable_history_identity: `summary-${index + 1}`, body: "" },
    { row_kind: "assistant", stable_history_identity: null, body: turn.responseText },
  ]);
  const value = projection({ transcript_rows });
  assert.equal(historyConversationRows(value).length, 4);
  assert.deepEqual(historyFixtureConversationFailures(value, turns), []);
  assert.deepEqual(historyFixtureConversationFailures(
    projection({ transcript_rows: transcript_rows.slice(0, -1) }),
    turns,
  ), [
    "history-fixture-conversation-count-mismatch",
    "history-fixture-conversation-order-mismatch",
  ]);
  const duplicateIdentity = structuredClone(transcript_rows);
  duplicateIdentity[3].stable_history_identity = "user-1";
  assert.deepEqual(historyFixtureConversationFailures(
    projection({ transcript_rows: duplicateIdentity }),
    turns,
  ), ["history-fixture-user-identity-invalid"]);
});

test("history.restart-prepend uses the common bounded Desktop execution contract", () => {
  const scenario = createHistoryRestartPrependScenario();
  assert.equal(scenario.id, "history.restart-prepend");
  assert.equal(scenario.productOracle, "pass");
  assert.equal(scenario.manualGate, "not_required");
  assert.equal(scenario.databaseRequired, true);
  for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) {
    assert.equal(typeof scenario[method], "function");
  }
});

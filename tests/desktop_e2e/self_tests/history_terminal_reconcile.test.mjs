import assert from "node:assert/strict";
import test from "node:test";

import {
  HISTORY_TERMINAL_RECONCILE_PROMPT,
  HISTORY_TERMINAL_RECONCILE_RESPONSE,
  exactWaitingPollBarrier,
  exactToolErrorRecoveryLedger,
  heldInitialProviderDecision,
  terminalReconcileDomFailures,
  terminalReconcileOwner,
  terminalReconcilePrimaryRows,
  terminalReconcileProjectionFailures,
  terminalReconcileRestartFailures,
} from "../scenarios/history_terminal_reconcile.mjs";

const SESSION_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const TURN_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

function ledger() {
  return ["tool_error_initial", "tool_error_continuation"].map((role) => ({
    method: "POST",
    pathname: "/v1/responses",
    query_present: false,
    contract: { pass: true, role },
    response_phase: "completed",
    response_status: 200,
  }));
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
    turn_page_admission_open: true,
    provider_loading: false,
    overlay: "none",
    confirmation_visible: false,
    draft_prompt: "",
    composer_submit_mode: "new_request",
    can_submit: true,
    selected_project_index: 0,
    selected_session_index: 0,
    session_rows: [{ session_id: SESSION_ID, admission_revision: "1" }],
    chat_session_rows: [],
    run_target: {
      sessionId: SESSION_ID,
      expectedState: {
        kind: "idle",
        latestTurnId: TURN_ID,
        admissionRevision: "1",
      },
    },
    turn_page_offset: 0,
    turn_page_limit: 80,
    turn_page_total: 5,
    turn_page_has_more: false,
    transcript_rows: [
      {
        row_kind: "user",
        stable_history_identity: "history-user-1",
        body: HISTORY_TERMINAL_RECONCILE_PROMPT,
      },
      {
        row_kind: "error",
        stable_history_identity: null,
        body: "missing fixture path",
      },
      {
        row_kind: "work_summary_completed",
        stable_history_identity: `turn:${TURN_ID}:work-summary`,
        body: "completed with one failed tool",
      },
      {
        row_kind: "assistant",
        stable_history_identity: null,
        body: HISTORY_TERMINAL_RECONCILE_RESPONSE,
      },
    ],
    ...overrides,
  };
}

function surface(overrides = {}) {
  return {
    users: [HISTORY_TERMINAL_RECONCILE_PROMPT],
    errors: ["missing fixture path"],
    assistants: [HISTORY_TERMINAL_RECONCILE_RESPONSE],
    primary_visible: true,
    prompt: { count: 1, value: "", visible: true, enabled: true },
    send: {
      count: 1,
      visible: true,
      enabled: false,
      title: "依頼文を入力してください",
      aria_label: "依頼文を入力してください",
    },
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
    ...overrides,
  };
}

test("tool-error recovery ledger requires the exact two accepted provider roles", () => {
  assert.equal(exactToolErrorRecoveryLedger(ledger()), true);
  assert.equal(exactToolErrorRecoveryLedger(ledger().reverse()), false);
  assert.equal(exactToolErrorRecoveryLedger(ledger().slice(0, 1)), false);
  assert.equal(exactToolErrorRecoveryLedger([
    ...ledger(),
    { method: "POST", pathname: "/v1/responses" },
  ]), false);
  const rejected = ledger();
  rejected[1].contract.pass = false;
  assert.equal(exactToolErrorRecoveryLedger(rejected), false);
});

test("initial provider hold treats an empty ledger as pending and only exact held state as pass", () => {
  const heldRow = {
    method: "POST",
    pathname: "/v1/responses",
    query_present: false,
    route: "responses",
    contract: { pass: true, role: "tool_error_initial" },
    response_phase: "held",
    response_status: null,
  };
  const heldResource = {
    active_request_count: 1,
    request_count: 1,
    accepted_response_count: 1,
    successful_response_count: 0,
    script_kind: "tool_error_recovery",
    scripted_responses_request_count: 1,
    scripted_responses_maximum: 2,
    scripted_response_roles: ["tool_error_initial"],
    response_release_controlled: true,
    response_release_count: 0,
    response_release_cleanup_count: 0,
  };
  assert.equal(heldInitialProviderDecision([], heldResource), "pending");
  assert.equal(heldInitialProviderDecision(null, heldResource), "pending");
  assert.equal(heldInitialProviderDecision([{
    ...heldRow,
    response_phase: "accepted",
  }], heldResource), "pending");
  assert.equal(heldInitialProviderDecision([heldRow], heldResource), "pass");
  assert.equal(heldInitialProviderDecision([{
    ...heldRow,
    contract: { pass: false, role: null },
    response_phase: "rejected",
    response_status: 422,
  }], heldResource), "fail");
  assert.equal(heldInitialProviderDecision([{
    ...heldRow,
    contract: { pass: true, role: "wrong" },
  }], heldResource), "fail");
  assert.equal(heldInitialProviderDecision([{}, {}], heldResource), "fail");
  assert.equal(heldInitialProviderDecision([heldRow], {
    ...heldResource,
    successful_response_count: 1,
  }), "fail");
});

test("frontend poll barrier requires one untouched held Desktop state request", () => {
  const waiting = {
    found: true,
    barrier_id: "history-terminal-reconcile",
    phase: "waiting",
    intercepted_count: 1,
    bypass_count: 0,
    captured: null,
  };
  assert.equal(exactWaitingPollBarrier(waiting), true);
  assert.equal(exactWaitingPollBarrier({ ...waiting, intercepted_count: 2 }), false);
  assert.equal(exactWaitingPollBarrier({ ...waiting, bypass_count: 1 }), false);
  assert.equal(exactWaitingPollBarrier({ ...waiting, phase: "idle" }), false);
});

test("terminal reconciliation requires canonical durable Error and full Assistant before restart", () => {
  const value = projection();
  assert.deepEqual(terminalReconcileProjectionFailures(value), []);
  assert.deepEqual(terminalReconcileDomFailures(surface(), value), []);
  assert.deepEqual(terminalReconcilePrimaryRows(value).map((row) => row.kind), [
    "user",
    "error",
    "assistant",
  ]);
  assert.deepEqual(terminalReconcileOwner(value), {
    sessionId: SESSION_ID,
    turnId: TURN_ID,
    admissionRevision: "1",
    page: { offset: 0, limit: 80, total: 5, hasMore: false },
  });

  const withoutError = projection({
    transcript_rows: value.transcript_rows.filter((row) => row.row_kind !== "error"),
  });
  assert.match(terminalReconcileProjectionFailures(withoutError).join(","), /primary-order/);
  const partialAssistant = structuredClone(value);
  partialAssistant.transcript_rows.at(-1).body = "TOOL_ERROR_RECOVERY_";
  assert.match(terminalReconcileProjectionFailures(partialAssistant).join(","), /assistant-not-canonical/);
  assert.match(
    terminalReconcileProjectionFailures(projection({ turn_page_offset: 1 })).join(","),
    /page-owner-invalid/,
  );
  assert.match(terminalReconcileDomFailures(surface({ errors: [] }), value).join(","), /error-dom-missing/);
  assert.match(
    terminalReconcileDomFailures(surface({ errors: ["different"] }), value).join(","),
    /error-dom-mismatch/,
  );
  assert.match(
    terminalReconcileDomFailures(surface({ assistants: ["TOOL_ERROR_RECOVERY_"] }), value).join(","),
    /assistant-dom-mismatch/,
  );
});

test("restart parity preserves the same owner and exact primary failed-tool conversation", () => {
  const before = projection();
  assert.deepEqual(terminalReconcileRestartFailures(before, structuredClone(before)), []);

  const changedError = structuredClone(before);
  changedError.transcript_rows.find((row) => row.row_kind === "error").body = "rewritten";
  assert.match(terminalReconcileRestartFailures(before, changedError).join(","), /primary-history-drift/);

  const changedOwner = structuredClone(before);
  changedOwner.run_target.expectedState.admissionRevision = "2";
  changedOwner.session_rows[0].admission_revision = "2";
  assert.match(terminalReconcileRestartFailures(before, changedOwner).join(","), /owner-drift/);
});

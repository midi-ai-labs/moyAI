import assert from "node:assert/strict";
import test from "node:test";

import {
  RUN_NEXT_TURN_FIRST_PROMPT,
  RUN_NEXT_TURN_FIRST_RESPONSE,
  RUN_NEXT_TURN_SECOND_PROMPT,
  RUN_NEXT_TURN_SECOND_RESPONSE,
  RUN_NEXT_TURN_THIRD_PROMPT,
  RUN_NEXT_TURN_THIRD_RESPONSE,
  exactRunNextTurnLedger,
  firstHeldDecision,
  firstTerminalSettlementAccepted,
  firstTurnAcquisitionFailures,
  nextTurnAcquisitionFailures,
  nextTurnTerminalFailures,
  nextHeldDecision,
  pendingOwnerRaceFailures,
} from "../scenarios/run_next_turn.mjs";

const SESSION_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const FIRST_TURN_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const SECOND_TURN_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const THIRD_TURN_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
const STALE_TURN_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";

function responseRow(phase) {
  return {
    method: "POST",
    pathname: "/v1/responses",
    query_present: false,
    contract: { pass: true },
    response_phase: phase,
    response_status: phase === "completed" ? 200 : null,
  };
}

function runTarget(expectedState) {
  return {
    workspacePath: "C:\\e2e\\run-next-turn",
    sessionId: SESSION_ID,
    runtimeOwnerToken: "idle:1",
    permissionConfirmationId: null,
    expectedState,
  };
}

function draftTarget() {
  return {
    workspacePath: "C:\\e2e\\run-next-turn",
    sessionId: SESSION_ID,
    ownerGeneration: "1",
  };
}

function firstTerminalProjection() {
  return {
    run_target: runTarget({
      kind: "idle",
      latestTurnId: FIRST_TURN_ID,
      admissionRevision: "7",
    }),
    draft_target: draftTarget(),
  };
}

function secondRunningProjection(overrides = {}) {
  return {
    run_status_key: "running",
    busy: true,
    run_target: runTarget({
      kind: "turn",
      turnId: SECOND_TURN_ID,
      admissionRevision: "8",
    }),
    draft_target: draftTarget(),
    transcript_rows: [{
      row_kind: "work_summary_running",
      stable_history_identity: "turn:" + SECOND_TURN_ID + ":work-summary",
      body: "### 現在\n- フェーズ: Provider応答受信中\n- 手順: 応答を待っています\n- モデル要求: 1",
    }],
    ...overrides,
  };
}

function runningSurface(projection = secondRunningProjection()) {
  return {
    projection,
    running_summaries: [{
      history_identity: projection.transcript_rows.find((row) => row.row_kind === "work_summary_running").stable_history_identity,
      visible: true,
      body_visible: true,
      metadata: [
        { label: "フェーズ", detail: "Provider応答受信中", visible: true },
        { label: "手順", detail: "応答を待っています", visible: true },
        { label: "モデル要求", detail: "1", visible: true },
      ],
      history_event_count: 0,
    }],
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
  };
}

function historyRows(turnCount = 2) {
  return [
    [FIRST_TURN_ID, RUN_NEXT_TURN_FIRST_PROMPT, RUN_NEXT_TURN_FIRST_RESPONSE],
    [SECOND_TURN_ID, RUN_NEXT_TURN_SECOND_PROMPT, RUN_NEXT_TURN_SECOND_RESPONSE],
    [THIRD_TURN_ID, RUN_NEXT_TURN_THIRD_PROMPT, RUN_NEXT_TURN_THIRD_RESPONSE],
  ].slice(0, turnCount).flatMap(([turnId, prompt, response]) => [
    { row_kind: "user", body: prompt, stable_history_identity: "user:" + turnId },
    { row_kind: "work_summary_completed", body: "", stable_history_identity: "turn:" + turnId + ":work-summary" },
    { row_kind: "assistant", body: response, stable_history_identity: "assistant:" + turnId },
  ]);
}

function terminalSample(overrides = {}) {
  const projection = {
    run_status_key: "completed",
    task_activity_state: "idle",
    busy: false,
    post_run_refresh_pending: false,
    background_mutation_pending: false,
    async_polling_required: false,
    pending_async_operations: [],
    composer_submit_mode: "new_request",
    can_submit: true,
    draft_prompt: "",
    run_target: runTarget({
      kind: "idle",
      latestTurnId: SECOND_TURN_ID,
      admissionRevision: "8",
    }),
    draft_target: draftTarget(),
    transcript_rows: historyRows(),
    ...(overrides.projection ?? {}),
  };
  return {
    surface: {
      projection,
      composer: { count: 1, visible: true, run_target: structuredClone(projection.run_target) },
      prompt: { count: 1, value: "", visible: true, enabled: true },
      send: { count: 1, visible: true, enabled: false },
      visible_fatal_count: 0,
      visible_recoverable_error_count: 0,
      ...(overrides.surface ?? {}),
    },
    ledger: [responseRow("completed"), responseRow("completed")],
    ...(overrides.sample ?? {}),
  };
}

function thirdRunningProjection() {
  const projection = secondRunningProjection();
  projection.run_target.expectedState = { kind: "turn", turnId: THIRD_TURN_ID, admissionRevision: "9" };
  projection.transcript_rows[0].stable_history_identity = "turn:" + THIRD_TURN_ID + ":work-summary";
  projection.transcript_rows = [...historyRows(), ...projection.transcript_rows];
  return projection;
}

function thirdTerminalSample() {
  return terminalSample({
    projection: {
      run_target: runTarget({ kind: "idle", latestTurnId: THIRD_TURN_ID, admissionRevision: "9" }),
      transcript_rows: historyRows(3),
    },
    sample: { ledger: Array.from({ length: 3 }, () => responseRow("completed")) },
  });
}

function pendingRaceSample({ latestTurnId = STALE_TURN_ID, admissionRevision = "6" } = {}) {
  const pendingProjection = {
    projection_revision: "21",
    post_run_refresh_pending: true,
    run_status_key: "completed",
    composer_submit_mode: "blocked",
    can_submit: false,
    run_target: runTarget({
      kind: "idle",
      latestTurnId,
      admissionRevision,
    }),
    draft_target: draftTarget(),
  };
  const settledProjection = {
    projection_revision: "22",
    post_run_refresh_pending: false,
    run_status_key: "completed",
    composer_submit_mode: "new_request",
    can_submit: true,
    run_target: runTarget({
      kind: "idle",
      latestTurnId: FIRST_TURN_ID,
      admissionRevision: "7",
    }),
    draft_target: draftTarget(),
  };
  return {
    pendingProjection,
    settledProjection,
    dom: {
      composer: { count: 1, visible: true, run_target: structuredClone(pendingProjection.run_target) },
      prompt: { count: 1, value: RUN_NEXT_TURN_SECOND_PROMPT, visible: true, enabled: true },
      send: { count: 1, visible: true, enabled: false, action: "send" },
      visible_fatal_count: 0,
      visible_recoverable_error_count: 0,
    },
    commandSnapshot: {
      found: true,
      probe_id: "run-next-turn-commands",
      sequence: 0,
      dropped_through: 0,
      calls: [],
    },
  };
}

test("run.next-turn ledger requires the exact ordered held/completed response phases", () => {
  assert.equal(exactRunNextTurnLedger([responseRow("completed"), responseRow("held")], ["completed", "held"]), true);
  assert.equal(exactRunNextTurnLedger([responseRow("completed"), responseRow("completed")], ["completed", "completed"]), true);
  assert.equal(exactRunNextTurnLedger([responseRow("held"), responseRow("completed")], ["completed", "held"]), false);
  assert.equal(exactRunNextTurnLedger([responseRow("completed")], ["completed", "completed"]), false);
});

test("run.next-turn first acquisition requires a concrete held Turn owner", () => {
  const valid = {
    surface: runningSurface(),
    ledger: [responseRow("held")],
  };
  assert.deepEqual(firstTurnAcquisitionFailures(valid), []);

  const staleIdleOwner = structuredClone(valid);
  staleIdleOwner.surface.projection.run_target.expectedState = {
    kind: "idle",
    latestTurnId: null,
    admissionRevision: "0",
  };
  assert.ok(firstTurnAcquisitionFailures(staleIdleOwner).includes("first-turn-owner-invalid"));

  const unboundSession = structuredClone(valid);
  unboundSession.surface.projection.run_target.sessionId = null;
  unboundSession.surface.projection.draft_target.sessionId = null;
  assert.ok(firstTurnAcquisitionFailures(unboundSession).includes("first-turn-owner-invalid"));

  const completedTooEarly = structuredClone(valid);
  completedTooEarly.ledger = [responseRow("completed")];
  assert.ok(firstTurnAcquisitionFailures(completedTooEarly).includes("provider-first-request-not-held"));
});

test("run.next-turn pending race oracle rejects open Send, session drift, and command delivery", () => {
  const valid = pendingRaceSample();
  assert.deepEqual(pendingOwnerRaceFailures(valid), []);

  const initialIdleFence = pendingRaceSample({ latestTurnId: null, admissionRevision: "0" });
  assert.deepEqual(pendingOwnerRaceFailures(initialIdleFence), []);

  const invalidNullFence = pendingRaceSample({ latestTurnId: null, admissionRevision: "6" });
  assert.ok(pendingOwnerRaceFailures(invalidNullFence).includes("pending-projection-did-not-close-new-request"));

  const openProjection = structuredClone(valid);
  openProjection.pendingProjection.composer_submit_mode = "new_request";
  openProjection.pendingProjection.can_submit = true;
  assert.ok(pendingOwnerRaceFailures(openProjection).includes("pending-projection-did-not-close-new-request"));

  const openDom = structuredClone(valid);
  openDom.dom.send.enabled = true;
  assert.ok(pendingOwnerRaceFailures(openDom).includes("pending-projection-was-not-rendered-as-a-blocked-composer"));

  const noOwnerDrift = structuredClone(valid);
  noOwnerDrift.settledProjection.run_target = structuredClone(noOwnerDrift.pendingProjection.run_target);
  assert.deepEqual(pendingOwnerRaceFailures(noOwnerDrift), []);

  const otherSession = structuredClone(valid);
  otherSession.settledProjection.run_target.sessionId = THIRD_TURN_ID;
  otherSession.settledProjection.draft_target.sessionId = THIRD_TURN_ID;
  assert.ok(pendingOwnerRaceFailures(otherSession).includes("pending-and-settled-session-drift"));

  const commandDelivered = structuredClone(valid);
  commandDelivered.commandSnapshot.sequence = 1;
  commandDelivered.commandSnapshot.calls = [{ sequence: 1, command: "submit_prompt", args: {} }];
  assert.ok(pendingOwnerRaceFailures(commandDelivered).includes("pending-composer-emitted-a-run-command"));
});

// Settled fields/rows from e2e-20261013t191415z-02e6546c event11.
// The workspace path is normalized; pending owner/revision are a controlled fixture,
// since that execution did not save the pending capture.
function acquiredFirstTerminalSample() {
  return {
    projection: {
      projection_revision: "1012",
      run_status_key: "completed",
      task_activity_state: "idle",
      busy: false,
      post_run_refresh_pending: false,
      background_mutation_pending: false,
      async_polling_required: false,
      pending_async_operations: [],
      composer_submit_mode: "new_request",
      can_submit: true,
      draft_prompt: "",
      run_target: {
        workspacePath: "C:\\e2e\\run-next-turn",
        sessionId: "01M4VB006YBMCYCN4E0DY3MEEW",
        runtimeOwnerToken: "idle:1",
        permissionConfirmationId: null,
        expectedState: {
          kind: "idle", latestTurnId: "01M4VB0072EQEYEPP3J720KKJZ", admissionRevision: "1",
        },
      },
      draft_target: {
        workspacePath: "C:\\e2e\\run-next-turn",
        sessionId: "01M4VB006YBMCYCN4E0DY3MEEW",
        ownerGeneration: "1",
      },
      transcript_rows: [
        { row_kind: "user", stable_history_identity: "01M4VB0073GM1YD8AJGN63K9Q6", body: "complete first turn" },
        { row_kind: "work_summary_completed", stable_history_identity: "turn:01M4VB0072EQEYEPP3J720KKJZ:work-summary",
          body: "### 作業サマリ\n- 結果: セッションは完了しました。" },
        { row_kind: "assistant", stable_history_identity: "01M4VB01HREVF4BTKFA4FWVXBW", body: "FIRST_TURN_OK" },
      ],
    },
    ledger: [responseRow("completed")],
  };
}

function controlledPendingProjection(sample) {
  return {
    ...structuredClone(sample.projection),
    projection_revision: "1011",
    post_run_refresh_pending: true,
    composer_submit_mode: "blocked",
    can_submit: false,
  };
}

test("run.next-turn accepts acquired first terminal history when only its pending projection is older", () => {
  const sample = acquiredFirstTerminalSample();
  const pending = controlledPendingProjection(sample);
  assert.equal(firstTerminalSettlementAccepted(sample, pending), true);
  const wrongHistory = structuredClone(sample);
  wrongHistory.projection.transcript_rows[2].body = "not the acquired response";
  assert.equal(firstTerminalSettlementAccepted(wrongHistory, pending), false);
});

test("run.next-turn first settlement rejects equal/older revisions and another session", () => {
  const sample = acquiredFirstTerminalSample();
  const pending = controlledPendingProjection(sample);
  pending.run_target.expectedState.latestTurnId = STALE_TURN_ID;
  for (const revision of ["1012", "1013", "not-a-revision"]) {
    pending.projection_revision = revision;
    assert.equal(firstTerminalSettlementAccepted(sample, pending), false);
  }
  pending.projection_revision = "1011";
  pending.run_target.sessionId = SESSION_ID;
  assert.equal(firstTerminalSettlementAccepted(sample, pending), false);
});

test("run.next-turn acquisition binds the same session to a distinct revision+1 Turn", () => {
  const first = firstTerminalProjection();
  const valid = {
    surface: runningSurface(),
    ledger: [responseRow("completed"), responseRow("held")],
  };
  assert.deepEqual(nextTurnAcquisitionFailures(valid, first), []);

  const staleRevision = structuredClone(valid);
  staleRevision.surface.projection.run_target.expectedState.admissionRevision = "7";
  assert.ok(nextTurnAcquisitionFailures(staleRevision, first).includes("second-turn-owner-not-next-revision"));

  const reusedTurn = structuredClone(valid);
  reusedTurn.surface.projection.run_target.expectedState.turnId = FIRST_TURN_ID;
  assert.ok(nextTurnAcquisitionFailures(reusedTurn, first).includes("second-turn-owner-not-next-revision"));

  const otherSession = structuredClone(valid);
  otherSession.surface.projection.run_target.sessionId = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
  assert.ok(nextTurnAcquisitionFailures(otherSession, first).includes("second-turn-session-drift"));
});

for (const [name, mutate, failure] of [
  ["zero canonical model requests", (sample) => {
    sample.surface.projection.transcript_rows[0].body = sample.surface.projection.transcript_rows[0].body.replace("モデル要求: 1", "モデル要求: 0");
  }, "running-summary-model-count-not-one"],
  ["a summary owned by an older turn", (sample) => {
    sample.surface.projection.transcript_rows[0].stable_history_identity = "turn:" + FIRST_TURN_ID + ":work-summary";
  }, "running-summary-owner-mismatch"],
  ["a stale DOM model count", (sample) => {
    sample.surface.running_summaries[0].metadata[2].detail = "0";
  }, "running-summary-dom-mismatch"],
  ["unexpected tool work", (sample) => {
    sample.surface.projection.transcript_rows[0].body += "\n\n### ツール\n- [完了] Current time";
  }, "running-summary-unexpected-tools"],
  ["a hidden DOM summary body", (sample) => {
    sample.surface.running_summaries[0].body_visible = false;
  }, "running-summary-dom-mismatch"],
]) {
  test("run.next-turn held acquisition rejects " + name, () => {
    const sample = { surface: runningSurface(), ledger: [responseRow("held")] };
    mutate(sample);
    assert.ok(firstTurnAcquisitionFailures(sample).includes(failure));
  });
}

test("run.next-turn terminal requires exact two-turn history and composer/backend owner settlement", () => {
  const first = firstTerminalProjection();
  const second = secondRunningProjection();
  const valid = terminalSample();
  assert.deepEqual(nextTurnTerminalFailures(valid, first, second), []);

  const staleComposer = terminalSample();
  staleComposer.surface.composer.run_target.expectedState.latestTurnId = FIRST_TURN_ID;
  assert.ok(nextTurnTerminalFailures(staleComposer, first, second).includes("second-terminal-not-settled"));

  const wrongHistory = terminalSample();
  wrongHistory.surface.projection.transcript_rows.at(-1).body = RUN_NEXT_TURN_FIRST_RESPONSE;
  assert.ok(nextTurnTerminalFailures(wrongHistory, first, second).includes("second-terminal-not-settled"));

  const wrongTerminalOwner = terminalSample();
  wrongTerminalOwner.surface.projection.run_target.expectedState.admissionRevision = "9";
  wrongTerminalOwner.surface.composer.run_target = structuredClone(wrongTerminalOwner.surface.projection.run_target);
  assert.ok(nextTurnTerminalFailures(wrongTerminalOwner, first, second).includes("second-terminal-owner-drift"));
});

test("run.next-turn count delivery waits within the observation budget without accepting zero", () => {
  const first = { surface: runningSurface(), ledger: [responseRow("held")] };
  assert.equal(firstHeldDecision(first), "pass");
  first.surface.projection.transcript_rows[0].body = first.surface.projection.transcript_rows[0].body.replace("モデル要求: 1", "モデル要求: 0");
  assert.equal(firstHeldDecision(first), "pending");
  const second = {
    surface: runningSurface(), ledger: [responseRow("completed"), responseRow("held")],
  };
  second.surface.running_summaries[0].metadata[2].detail = "0";
  assert.equal(nextHeldDecision(second, firstTerminalProjection()), "pending");
  second.surface.running_summaries[0].metadata[2].detail = "1";
  assert.equal(nextHeldDecision(second, firstTerminalProjection()), "pass");
  second.ledger.push(responseRow("held"));
  assert.equal(nextHeldDecision(second, firstTerminalProjection()), "fail");
});

test("run.next-turn third acquisition requires next revision and excludes every previous summary owner", () => {
  const previous = terminalSample().surface.projection;
  const valid = {
    surface: runningSurface(thirdRunningProjection()),
    ledger: [responseRow("completed"), responseRow("completed"), responseRow("held")],
  };
  assert.deepEqual(nextTurnAcquisitionFailures(valid, previous, 3), []);
  assert.equal(nextHeldDecision(valid, previous, 3), "pass");
  const staleRevision = structuredClone(valid);
  staleRevision.surface.projection.run_target.expectedState.admissionRevision = "8";
  assert.ok(nextTurnAcquisitionFailures(staleRevision, previous, 3).includes("third-turn-owner-not-next-revision"));
  const reusedFirst = structuredClone(valid);
  reusedFirst.surface.projection.run_target.expectedState.turnId = FIRST_TURN_ID;
  assert.ok(nextTurnAcquisitionFailures(reusedFirst, previous, 3).includes("third-turn-reused-history-owner"));
  const wrongSession = structuredClone(valid);
  wrongSession.surface.projection.run_target.sessionId = STALE_TURN_ID;
  assert.ok(nextTurnAcquisitionFailures(wrongSession, previous, 3).includes("third-turn-session-drift"));
  const staleDom = structuredClone(valid);
  staleDom.surface.running_summaries[0].history_identity = "turn:" + SECOND_TURN_ID + ":work-summary";
  assert.equal(nextHeldDecision(staleDom, previous, 3), "pending");
});

test("run.next-turn third terminal requires exact three-turn history with unique durable rows", () => {
  const first = firstTerminalProjection();
  const third = thirdRunningProjection();
  const valid = thirdTerminalSample();
  assert.deepEqual(nextTurnTerminalFailures(valid, first, third, 3), []);
  const duplicate = thirdTerminalSample();
  duplicate.surface.projection.transcript_rows[6].stable_history_identity = duplicate.surface.projection.transcript_rows[0].stable_history_identity;
  assert.ok(nextTurnTerminalFailures(duplicate, first, third, 3).includes("third-terminal-not-settled"));
  const missingThird = thirdTerminalSample();
  missingThird.surface.projection.transcript_rows = historyRows();
  assert.ok(nextTurnTerminalFailures(missingThird, first, third, 3).includes("third-terminal-not-settled"));
  const replay = thirdTerminalSample();
  replay.ledger.push(responseRow("completed"));
  assert.ok(nextTurnTerminalFailures(replay, first, third, 3).includes("provider-3-turn-ledger-not-complete"));
});

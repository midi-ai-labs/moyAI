import assert from "node:assert/strict";
import test from "node:test";

import {
  RUN_NEXT_TURN_FIRST_PROMPT,
  RUN_NEXT_TURN_FIRST_RESPONSE,
  RUN_NEXT_TURN_SECOND_PROMPT,
  RUN_NEXT_TURN_SECOND_RESPONSE,
  exactRunNextTurnLedger,
  firstTurnAcquisitionFailures,
  nextTurnAcquisitionFailures,
  nextTurnTerminalFailures,
  pendingOwnerRaceFailures,
} from "../scenarios/run_next_turn.mjs";

const SESSION_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const FIRST_TURN_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const SECOND_TURN_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
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
    ...overrides,
  };
}

function historyRows() {
  return [
    { row_kind: "user", body: RUN_NEXT_TURN_FIRST_PROMPT },
    { row_kind: "work_summary_completed", body: "" },
    { row_kind: "assistant", body: RUN_NEXT_TURN_FIRST_RESPONSE },
    { row_kind: "user", body: RUN_NEXT_TURN_SECOND_PROMPT },
    { row_kind: "work_summary_completed", body: "" },
    { row_kind: "assistant", body: RUN_NEXT_TURN_SECOND_RESPONSE },
  ];
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
    surface: {
      projection: secondRunningProjection(),
      visible_fatal_count: 0,
      visible_recoverable_error_count: 0,
    },
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

test("run.next-turn pending race oracle fails open Send, owner non-drift, and command delivery", () => {
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
  assert.ok(pendingOwnerRaceFailures(noOwnerDrift).includes("pending-and-settled-run-owners-did-not-drift-in-one-session"));

  const commandDelivered = structuredClone(valid);
  commandDelivered.commandSnapshot.sequence = 1;
  commandDelivered.commandSnapshot.calls = [{ sequence: 1, command: "submit_prompt", args: {} }];
  assert.ok(pendingOwnerRaceFailures(commandDelivered).includes("pending-composer-emitted-a-run-command"));
});

test("run.next-turn acquisition binds the same session to a distinct revision+1 Turn", () => {
  const first = firstTerminalProjection();
  const valid = {
    surface: {
      projection: secondRunningProjection(),
      visible_fatal_count: 0,
      visible_recoverable_error_count: 0,
    },
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

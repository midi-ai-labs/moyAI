import assert from "node:assert/strict";
import test from "node:test";

import {
  PERMISSION_RESTART_GUARDIAN_RESPONSE,
  PERMISSION_RESTART_GUARDIAN_SEED_PROMPT,
  PERMISSION_RESTART_GUARDIAN_SEED_RESPONSE,
  PERMISSION_RESTART_GUARDIAN_TASK_PROMPT,
  createPermissionRestartGuardianStableRestartDecision,
  exactPermissionRestartGuardianLedger,
  permissionRestartGuardianFixtureConfig,
  permissionRestartGuardianPressureOverlapFailures,
  permissionRestartGuardianPressureFailures,
  permissionRestartGuardianReviewObserved,
  permissionRestartGuardianTerminalFailures,
} from "../scenarios/permission_restart_guardian.mjs";

const SESSION_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const SEED_TURN_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const TASK_TURN_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const ROLES = Object.freeze([
  "guardian_seed",
  "guardian_tool_initial",
  "guardian_review",
  "guardian_continuation",
]);

function responseRow(role, overrides = {}) {
  return {
    route: "responses",
    method: "POST",
    pathname: "/v1/responses",
    query_present: false,
    contract: { pass: true, role },
    response_phase: "completed",
    response_status: 200,
    ...overrides,
  };
}

function ledger() {
  return [
    {
      route: "lm_studio_models",
      method: "GET",
      response_phase: "completed",
      response_status: 200,
    },
    ...ROLES.map((role) => responseRow(role)),
  ];
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
    composer_submit_mode: "new_request",
    can_submit: true,
    status_message: "完了",
    draft_target: { sessionId: SESSION_ID },
    run_target: {
      sessionId: SESSION_ID,
      expectedState: {
        kind: "idle",
        latestTurnId: TASK_TURN_ID,
        admissionRevision: "7",
      },
    },
    transcript_rows: [
      { row_kind: "user", body: PERMISSION_RESTART_GUARDIAN_SEED_PROMPT },
      { row_kind: "work_summary_completed", body: "seed completed" },
      { row_kind: "assistant", body: PERMISSION_RESTART_GUARDIAN_SEED_RESPONSE },
      { row_kind: "user", body: PERMISSION_RESTART_GUARDIAN_TASK_PROMPT },
      { row_kind: "work_summary_completed", body: "task completed" },
      { row_kind: "assistant", body: PERMISSION_RESTART_GUARDIAN_RESPONSE },
    ],
    ...overrides,
  };
}

function surface(projectionOverrides = {}, surfaceOverrides = {}) {
  return {
    projection: projection(projectionOverrides),
    prompt: { visible: true, enabled: true, value: "" },
    send: { count: 1, visible: true, enabled: false },
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
    ...surfaceOverrides,
  };
}

const seedOwner = Object.freeze({
  sessionId: SESSION_ID,
  turnId: SEED_TURN_ID,
  admissionRevision: "6",
});

test("permission restart Guardian fixture selects LM Studio Responses, tools, and AutoReview", () => {
  const config = permissionRestartGuardianFixtureConfig("http://127.0.0.1:43123");
  assert.match(config, /base_url = "http:\/\/127\.0\.0\.1:43123"/);
  assert.match(config, /provider_profile = "lm_studio"/);
  assert.match(config, /supports_tools = true/);
  assert.match(config, /\[permissions\][\s\S]*access_mode = "auto_review"/);
  assert.match(config, /max_retries = 0/);
  assert.doesNotMatch(config, /max_(?:output_)?tokens|reasoning_(?:effort|summary)|supports_reasoning|temperature|top_p|top_k|presence_penalty|frequency_penalty|seed\s*=|stop(?:_sequences)?\s*=|\[model\.extra_body_json\]/);
});

test("permission restart Guardian ledger accepts only the exact four ordered provider roles", () => {
  const exact = ledger();
  assert.equal(exactPermissionRestartGuardianLedger(exact, ROLES), true);
  assert.equal(permissionRestartGuardianReviewObserved(exact.slice(0, 4)), true);
  assert.equal(permissionRestartGuardianReviewObserved(exact), true);

  const reordered = ledger();
  [reordered[2], reordered[3]] = [reordered[3], reordered[2]];
  assert.equal(exactPermissionRestartGuardianLedger(reordered, ROLES), false);
  assert.equal(permissionRestartGuardianReviewObserved(reordered), false);

  assert.equal(exactPermissionRestartGuardianLedger([
    ...ledger(),
    responseRow("guardian_continuation"),
  ], ROLES), false);

  const rejected = ledger();
  rejected[3] = responseRow("guardian_review", {
    contract: { pass: false, role: "guardian_review" },
    response_phase: "rejected",
    response_status: 503,
  });
  assert.equal(exactPermissionRestartGuardianLedger(rejected, ROLES), false);

  const held = ledger();
  held[2] = responseRow("guardian_tool_initial", {
    response_phase: "held",
    response_status: null,
  });
  assert.equal(exactPermissionRestartGuardianLedger(held, ROLES), false);
  assert.equal(exactPermissionRestartGuardianLedger(held, ROLES, {
    heldRole: "guardian_tool_initial",
  }), true);

  assert.equal(exactPermissionRestartGuardianLedger([
    { route: "unexpected", method: "GET", response_phase: "completed", response_status: 200 },
    ...ledger(),
  ], ROLES), false);
});

test("permission restart Guardian terminal requires settled status, exact history, and revision+1 owner", () => {
  assert.deepEqual(permissionRestartGuardianTerminalFailures(surface(), seedOwner), []);

  assert.ok(permissionRestartGuardianTerminalFailures(
    surface({ busy: true }),
    seedOwner,
  ).includes("terminal-surface-not-settled"));

  assert.ok(permissionRestartGuardianTerminalFailures(surface({
    status_message: "guardian request failed: canonical user authority storage is busy",
  }), seedOwner).includes("guardian-storage-failure-visible"));

  const historyDrift = surface();
  historyDrift.projection.transcript_rows[3].body = "different task";
  assert.ok(permissionRestartGuardianTerminalFailures(
    historyDrift,
    seedOwner,
  ).includes("canonical-user-authority-conversation-mismatch"));

  const canonicalError = surface();
  canonicalError.projection.transcript_rows.splice(5, 0, {
    row_kind: "error",
    body: "guardian request failed",
  });
  assert.ok(permissionRestartGuardianTerminalFailures(
    canonicalError,
    seedOwner,
  ).includes("canonical-error-row-present"));

  assert.ok(permissionRestartGuardianTerminalFailures(surface({
    run_target: {
      sessionId: SESSION_ID,
      expectedState: {
        kind: "idle",
        latestTurnId: TASK_TURN_ID,
        admissionRevision: "8",
      },
    },
  }), seedOwner).includes("post-restart-turn-owner-mismatch"));
});

test("permission restart Guardian requires one continuously stable restart owner", () => {
  let observedAt = 1_000;
  const decide = createPermissionRestartGuardianStableRestartDecision(
    seedOwner,
    300,
    () => observedAt,
  );
  const restored = surface({
    run_target: {
      sessionId: SESSION_ID,
      expectedState: {
        kind: "idle",
        latestTurnId: SEED_TURN_ID,
        admissionRevision: "6",
      },
    },
    transcript_rows: [
      { row_kind: "user", body: PERMISSION_RESTART_GUARDIAN_SEED_PROMPT },
      { row_kind: "work_summary_completed", body: "seed completed" },
      { row_kind: "assistant", body: PERMISSION_RESTART_GUARDIAN_SEED_RESPONSE },
    ],
  });
  const seedLedger = ledger().slice(0, 2);
  assert.equal(decide({ surface: restored, ledger: seedLedger }), "pending");
  observedAt += 299;
  assert.equal(decide({ surface: restored, ledger: seedLedger }), "pending");
  observedAt += 1;
  assert.equal(decide({ surface: restored, ledger: seedLedger }), "pass");

  const drift = structuredClone(restored);
  drift.projection.run_target.expectedState.latestTurnId = TASK_TURN_ID;
  assert.equal(decide({ surface: drift, ledger: seedLedger }), "fail");
});

test("permission restart Guardian pressure overlaps review and then settles every started poll", () => {
  const exact = {
    found: true,
    workers: 8,
    minimumCompleted: 512,
    maxIterationsPerWorker: 4096,
    started: 640,
    completed: 640,
    inflight: 0,
    errors: [],
    stopRequested: true,
    exhausted: false,
    settled: true,
  };
  assert.deepEqual(permissionRestartGuardianPressureFailures(exact), []);
  assert.ok(permissionRestartGuardianPressureFailures({
    ...exact,
    completed: 639,
  }).includes("pressure-cardinality-mismatch"));
  assert.ok(permissionRestartGuardianPressureFailures({
    ...exact,
    errors: ["desktop_state rejected"],
  }).includes("pressure-command-error"));
  assert.ok(permissionRestartGuardianPressureFailures({
    ...exact,
    settled: false,
    inflight: 1,
  }).includes("pressure-not-settled"));

  const overlap = {
    ...exact,
    started: 648,
    completed: 640,
    inflight: 8,
    stopRequested: false,
    settled: false,
  };
  assert.deepEqual(permissionRestartGuardianPressureOverlapFailures(overlap), []);
  assert.ok(permissionRestartGuardianPressureOverlapFailures({
    ...overlap,
    inflight: 0,
  }).includes("pressure-not-active-at-guardian-review"));
});

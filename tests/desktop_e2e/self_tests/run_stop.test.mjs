import assert from "node:assert/strict";
import test from "node:test";

import {
  RUN_STOP_PROMPT,
  exactHeldRunStopLedger,
  exactStoppedRunStopLedger,
  exactTurnStopTarget,
  runStopInFlightFailures,
  runStopTerminalFailures,
} from "../scenarios/run_stop.mjs";

const WORKSPACE = "C:\\e2e\\run-stop";
const SESSION_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const OTHER_SESSION_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const TURN_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

function stopTarget(overrides = {}) {
  return {
    kind: "turn",
    workspacePath: WORKSPACE,
    sessionId: SESSION_ID,
    turnId: TURN_ID,
    admissionRevision: "7",
    rootEpoch: "11",
    ...overrides,
  };
}

function providerRow(phase) {
  return {
    method: "POST",
    pathname: "/v1/responses",
    query_present: false,
    contract: { pass: true },
    response_phase: phase,
    response_status: null,
  };
}

function runningProjection(overrides = {}) {
  const target = stopTarget();
  return {
    stop_target: target,
    run_target: {
      expectedState: {
        kind: "turn",
        turnId: TURN_ID,
        admissionRevision: "7",
      },
    },
    session_rows: [],
    chat_session_rows: [{
      session_id: SESSION_ID,
      status: "running",
      loaded_status: "active",
      admission_revision: "7",
      active_turn_id: TURN_ID,
      interrupt_target: target,
    }],
    transcript_rows: [{ row_kind: "user", body: RUN_STOP_PROMPT }],
    run_status_key: "running",
    status_code: "plain",
    busy: true,
    task_activity_state: "running",
    can_cancel_run: true,
    agent_tree_active: false,
    post_run_refresh_pending: false,
    background_mutation_pending: false,
    async_polling_required: true,
    pending_async_operations: ["run"],
    navigation_loading: false,
    overlay: "none",
    confirmation_visible: false,
    ...overrides,
  };
}

function runningIndicator(overrides = {}) {
  const unpaintedPseudo = {
    display: "inline",
    opacity: "1",
    color: "rgb(229, 233, 240)",
    background_color: "rgba(0, 0, 0, 0)",
    border_top_width: "0px",
    border_top_style: "none",
    border_top_color: "rgb(229, 233, 240)",
    border_right_width: "0px",
    border_right_style: "none",
    border_right_color: "rgb(229, 233, 240)",
    border_bottom_width: "0px",
    border_bottom_style: "none",
    border_bottom_color: "rgb(229, 233, 240)",
    border_left_width: "0px",
    border_left_style: "none",
    border_left_color: "rgb(229, 233, 240)",
    content: "none",
    width: "auto",
    height: "auto",
  };
  const base = {
    count: 1,
    state: "running",
    visible: true,
    width: 18,
    height: 18,
    css_width: "18px",
    css_height: "18px",
    small: false,
    opacity: "1",
    color: "rgb(229, 233, 240)",
    background_color: "rgba(85, 142, 236, 0.18)",
    border_radius: "999px",
    border_top_width: "2px",
    border_top_style: "solid",
    border_top_color: "rgb(155, 193, 255)",
    border_right_width: "2px",
    border_right_style: "solid",
    border_right_color: "rgb(121, 170, 255)",
    border_bottom_width: "2px",
    border_bottom_style: "solid",
    border_bottom_color: "rgba(121, 170, 255, 0.38)",
    border_left_width: "2px",
    border_left_style: "solid",
    border_left_color: "rgba(121, 170, 255, 0.38)",
    animation_name: "moyai-task-activity-running",
    animation_duration: "0.9s",
    animation_iteration_count: "infinite",
    animation_play_state: "running",
    before: unpaintedPseudo,
    after: {
      ...unpaintedPseudo,
      display: "block",
      background_color: "rgb(220, 234, 255)",
      content: '""',
      width: "4px",
      height: "4px",
    },
    aria_hidden: null,
    aria_label: null,
    row_action: null,
    row_focus_key: null,
    row_session_id: null,
    row_selected: false,
    row_aria_current: null,
    row_task_activity: null,
    row_subtitle: "",
  };
  return {
    ...base,
    ...overrides,
    before: { ...base.before, ...(overrides.before ?? {}) },
    after: { ...base.after, ...(overrides.after ?? {}) },
  };
}

function eraseIndicatorPaint(indicator) {
  indicator.background_color = "rgba(0, 0, 0, 0)";
  for (const side of ["top", "right", "bottom", "left"]) {
    indicator[`border_${side}_color`] = "rgba(0, 0, 0, 0)";
  }
  for (const pseudo of [indicator.before, indicator.after]) {
    pseudo.color = "rgba(0, 0, 0, 0)";
    pseudo.background_color = "rgba(0, 0, 0, 0)";
    for (const side of ["top", "right", "bottom", "left"]) {
      pseudo[`border_${side}_color`] = "rgba(0, 0, 0, 0)";
    }
  }
}

function runningSample(overrides = {}) {
  return {
    surface: {
      projection: runningProjection(),
      stop_button: { count: 1, visible: true, enabled: true, aria_label: "Mainを停止", text: "Mainを停止" },
      task_activity: {
        total_count: 2,
        visible_count: 2,
        activity_row_count: 1,
        prefers_reduced_motion: false,
        central_badge: {
          count: 1,
          state: "running",
          visible: true,
          role: "status",
          aria_live: "polite",
          aria_atomic: "true",
          label: "実行中",
          label_visible: true,
        },
        run_strip: runningIndicator({
          aria_hidden: "true",
          css_width: "20px",
          css_height: "20px",
        }),
        selected_sidebar: runningIndicator({
          aria_hidden: "true",
          row_action: "chat-session",
          row_focus_key: `chat-session:${SESSION_ID}:select`,
          row_session_id: SESSION_ID,
          row_selected: true,
          row_aria_current: "page",
          row_task_activity: "running",
          row_subtitle: "実行中 · turn 1",
        }),
      },
      visible_fatal_count: 0,
      visible_recoverable_error_count: 0,
    },
    ledger: [providerRow("held")],
    provider: {
      active_request_count: 1,
      accepted_response_count: 1,
      successful_response_count: 0,
    },
    ...overrides,
  };
}

function terminalProjection(overrides = {}) {
  return {
    stop_target: null,
    run_target: {
      expectedState: {
        kind: "idle",
        latestTurnId: TURN_ID,
        admissionRevision: "7",
      },
    },
    session_rows: [],
    chat_session_rows: [{
      session_id: SESSION_ID,
      status: "cancelled",
      loaded_status: "idle",
      admission_revision: "7",
      active_turn_id: null,
      interrupt_target: null,
    }],
    transcript_rows: [
      { row_kind: "user", body: RUN_STOP_PROMPT },
      { row_kind: "work_summary_cancelled", body: "" },
    ],
    run_status_key: "cancelled",
    status_code: "user_stopped",
    busy: false,
    task_activity_state: "idle",
    can_cancel_run: false,
    agent_tree_active: false,
    post_run_refresh_pending: false,
    background_mutation_pending: false,
    async_polling_required: false,
    pending_async_operations: [],
    navigation_loading: false,
    overlay: "none",
    confirmation_visible: false,
    ...overrides,
  };
}

function terminalSample(overrides = {}) {
  return {
    surface: {
      projection: terminalProjection(),
      stop_button: { count: 0, visible: false, enabled: false },
      task_activity: {
        total_count: 0,
        visible_count: 0,
        activity_row_count: 0,
        prefers_reduced_motion: false,
        central_badge: { count: 0 },
        run_strip: { count: 0 },
        selected_sidebar: { count: 0 },
      },
      user_rows: [{ text: RUN_STOP_PROMPT, visible: true }],
      cancelled_rows: [{ text: "", visible: true }],
      assistant_count: 0,
      visible_fatal_count: 0,
      visible_recoverable_error_count: 0,
      visible_dialog_count: 0,
      visible_modal_backdrop_count: 0,
    },
    ledger: [providerRow("peer_closed")],
    provider: {
      active_request_count: 0,
      accepted_response_count: 1,
      successful_response_count: 0,
    },
    ...overrides,
  };
}

test("run.stop Turn target requires concrete canonical session, turn, epoch, and admission revision", () => {
  assert.equal(exactTurnStopTarget(stopTarget()), true);
  assert.equal(exactTurnStopTarget(stopTarget({ admissionRevision: "18446744073709551615" })), true);
  assert.equal(exactTurnStopTarget(stopTarget(), stopTarget()), true);
  for (const invalid of [
    stopTarget({ sessionId: null }),
    stopTarget({ sessionId: "session-1" }),
    stopTarget({ turnId: TURN_ID.toLowerCase() }),
    stopTarget({ admissionRevision: "07" }),
    stopTarget({ admissionRevision: -1 }),
    stopTarget({ admissionRevision: "18446744073709551616" }),
    stopTarget({ rootEpoch: "011" }),
    { ...stopTarget(), compatibilityFlag: false },
    (() => { const value = { ...stopTarget() }; delete value.admissionRevision; return value; })(),
  ]) {
    assert.equal(exactTurnStopTarget(invalid), false, JSON.stringify(invalid));
  }
});

test("in-flight oracle binds one held provider request to the projected and row Turn Stop owner", () => {
  assert.deepEqual(runStopInFlightFailures(runningSample()), []);
  assert.equal(exactHeldRunStopLedger([providerRow("held")]), true);
  assert.equal(exactHeldRunStopLedger([providerRow("peer_closed")]), false);

  const missingRevision = stopTarget();
  delete missingRevision.admissionRevision;
  assert.ok(runStopInFlightFailures(runningSample({
    surface: {
      ...runningSample().surface,
      projection: runningProjection({ stop_target: missingRevision }),
    },
  })).includes("turn-stop-target-not-canonical"));
  assert.ok(runStopInFlightFailures(runningSample({
    ledger: [providerRow("held"), providerRow("held")],
  })).includes("provider-request-not-held-exactly-once"));
  assert.ok(runStopInFlightFailures(runningSample({
    surface: { ...runningSample().surface, stop_button: { count: 2, visible: true, enabled: true } },
  })).includes("semantic-stop-not-interactable"));

  const centralHidden = runningSample();
  centralHidden.surface.task_activity.run_strip.visible = false;
  assert.ok(runStopInFlightFailures(centralHidden).includes("central-running-indicator-not-visible"));

  const centralWrongSize = runningSample();
  centralWrongSize.surface.task_activity.run_strip.css_width = "18px";
  assert.ok(runStopInFlightFailures(centralWrongSize).includes("central-running-indicator-size-mismatch"));

  const selectedWrongSize = runningSample();
  selectedWrongSize.surface.task_activity.selected_sidebar.css_height = "20px";
  assert.ok(runStopInFlightFailures(selectedWrongSize).includes("selected-running-indicator-size-mismatch"));

  for (const [field, value] of [
    ["count", 2],
    ["state", "attention"],
    ["visible", false],
    ["role", "img"],
    ["aria_live", "assertive"],
    ["aria_atomic", "false"],
    ["label", ""],
    ["label_visible", false],
  ]) {
    const invalidBadge = runningSample();
    invalidBadge.surface.task_activity.central_badge[field] = value;
    assert.ok(
      runStopInFlightFailures(invalidBadge).includes("central-running-badge-not-semantic"),
      field,
    );
  }

  const selectedWrongState = runningSample();
  selectedWrongState.surface.task_activity.selected_sidebar.state = "finalizing";
  assert.ok(runStopInFlightFailures(selectedWrongState).includes("selected-running-indicator-not-visible"));

  const animationMissing = runningSample();
  animationMissing.surface.task_activity.run_strip.animation_name = "none";
  assert.ok(runStopInFlightFailures(animationMissing).includes("running-indicator-animation-mismatch"));

  const wrongSelectedOwner = runningSample();
  wrongSelectedOwner.surface.task_activity.selected_sidebar.row_focus_key = `chat-session:${OTHER_SESSION_ID}:select`;
  wrongSelectedOwner.surface.task_activity.selected_sidebar.row_session_id = OTHER_SESSION_ID;
  assert.ok(runStopInFlightFailures(wrongSelectedOwner).includes("selected-running-indicator-owner-mismatch"));

  const selectedMarkerNamed = runningSample();
  selectedMarkerNamed.surface.task_activity.selected_sidebar.aria_hidden = null;
  selectedMarkerNamed.surface.task_activity.selected_sidebar.aria_label = "実行中";
  assert.ok(runStopInFlightFailures(selectedMarkerNamed).includes("selected-running-indicator-not-decorative"));

  const selectedRowStateMissing = runningSample();
  selectedRowStateMissing.surface.task_activity.selected_sidebar.row_task_activity = null;
  assert.ok(runStopInFlightFailures(selectedRowStateMissing).includes("selected-running-row-state-mismatch"));

  const selectedRowLabelWrong = runningSample();
  selectedRowLabelWrong.surface.task_activity.selected_sidebar.row_subtitle = "確認待ち · turn 1";
  assert.ok(runStopInFlightFailures(selectedRowLabelWrong).includes("selected-running-row-label-mismatch"));

  const duplicateActivityRow = runningSample();
  duplicateActivityRow.surface.task_activity.activity_row_count = 2;
  assert.ok(runStopInFlightFailures(duplicateActivityRow).includes("running-task-activity-cardinality-mismatch"));

  const transparentCentral = runningSample();
  eraseIndicatorPaint(transparentCentral.surface.task_activity.run_strip);
  assert.ok(runStopInFlightFailures(transparentCentral).includes("central-running-indicator-not-painted"));
  assert.ok(runStopInFlightFailures(transparentCentral).includes("central-running-indicator-shape-mismatch"));

  const transparentSelected = runningSample();
  eraseIndicatorPaint(transparentSelected.surface.task_activity.selected_sidebar);
  assert.ok(runStopInFlightFailures(transparentSelected).includes("selected-running-indicator-not-painted"));
  assert.ok(runStopInFlightFailures(transparentSelected).includes("selected-running-indicator-shape-mismatch"));

  const missingCenterDot = runningSample();
  missingCenterDot.surface.task_activity.run_strip.after.width = "0px";
  missingCenterDot.surface.task_activity.run_strip.after.height = "0px";
  assert.ok(runStopInFlightFailures(missingCenterDot).includes("central-running-indicator-shape-mismatch"));

  const squareRunningIndicator = runningSample();
  squareRunningIndicator.surface.task_activity.selected_sidebar.border_radius = "0px";
  assert.ok(runStopInFlightFailures(squareRunningIndicator).includes("selected-running-indicator-shape-mismatch"));

  const zeroDuration = runningSample();
  zeroDuration.surface.task_activity.run_strip.animation_duration = "0s";
  assert.ok(runStopInFlightFailures(zeroDuration).includes("running-indicator-animation-mismatch"));

  const reducedMotion = runningSample();
  reducedMotion.surface.task_activity.prefers_reduced_motion = true;
  reducedMotion.surface.task_activity.run_strip.animation_name = "none";
  reducedMotion.surface.task_activity.selected_sidebar.animation_name = "none";
  assert.deepEqual(runStopInFlightFailures(reducedMotion), []);
});

test("Main Stop is acquired by action while its visible and accessible purpose are checked separately", () => {
  for (const change of [
    (v) => { v.surface.stop_button.aria_label = "実行停止"; },
    (v) => { v.surface.stop_button.text = "MCPを停止"; },
    (v) => { v.surface.stop_button.aria_label = null; },
  ]) {
    const sample = runningSample(); change(sample);
    const failures = runStopInFlightFailures(sample);
    assert.ok(failures.includes("main-stop-label-mismatch"));
    assert.equal(failures.includes("semantic-stop-not-interactable"), false,
      "a label regression must remain observable rather than hiding the acquired button");
  }
});

test("terminal oracle requires durable UserStop Idle, one cancelled history, no replay, and no error overlay", () => {
  const target = stopTarget();
  assert.deepEqual(runStopTerminalFailures(terminalSample(), target), []);
  assert.equal(exactStoppedRunStopLedger([providerRow("peer_closed")]), true);
  assert.equal(exactStoppedRunStopLedger([providerRow("completed")]), false);

  assert.ok(runStopTerminalFailures(terminalSample({
    surface: {
      ...terminalSample().surface,
      projection: terminalProjection({ status_code: "plain" }),
    },
  }), target).includes("durable-user-stop-terminal-missing"));
  assert.ok(runStopTerminalFailures(terminalSample({
    ledger: [providerRow("peer_closed"), { ...providerRow("rejected"), response_status: 409 }],
  }), target).includes("provider-request-replayed-or-completed"));
  assert.ok(runStopTerminalFailures(terminalSample({
    surface: { ...terminalSample().surface, visible_recoverable_error_count: 1 },
  }), target).includes("error-or-blocking-overlay-visible"));
  assert.ok(runStopTerminalFailures(terminalSample({
    surface: {
      ...terminalSample().surface,
      projection: terminalProjection({
        run_target: { expectedState: { kind: "idle", latestTurnId: TURN_ID, admissionRevision: "8" } },
      }),
    },
  }), target).includes("terminal-idle-owner-drift"));

  const lingeringIndicator = terminalSample();
  lingeringIndicator.surface.task_activity.total_count = 1;
  lingeringIndicator.surface.task_activity.visible_count = 1;
  lingeringIndicator.surface.task_activity.selected_sidebar = { count: 1 };
  assert.ok(runStopTerminalFailures(lingeringIndicator, target).includes("terminal-task-indicator-not-cleared"));
});

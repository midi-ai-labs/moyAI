import assert from "node:assert/strict";
import test from "node:test";
import { OUTPUT_HISTORY_DRAFT, createOutputHistoryNavigationScenario, outputHistoryDraftBaseline, outputHistoryDraftFailures,
  outputHistoryDestinationIdentity, outputHistoryKeyboardProof, outputHistoryNavigationFailures, outputHistoryOwner } from "../scenarios/output_history_navigation.mjs";

const owner = { workspace: "C:\\fixture", session: "session-a", turn: "turn-a", admission: "3" };
const baseline = { target: { workspacePath: owner.workspace, sessionId: owner.session, ownerGeneration: "2" },
  commitGeneration: "1", committedText: "" };
const destinationIdentity = { anchor: "current-summary", historyIdentity: `turn:${owner.turn}:work-summary`, focusKey: "work-summary:current-summary" };
const options = { owner, phase: "running", draft: OUTPUT_HISTORY_DRAFT, baseline, destinationIdentity,
  selection: [0, OUTPUT_HISTORY_DRAFT.length], focus: "summary" };
function surface(phase = "running") {
  return {
    projection: {
      run_target: { workspacePath: owner.workspace, sessionId: owner.session,
        expectedState: phase === "running" ? { kind: "turn", turnId: owner.turn, admissionRevision: owner.admission }
          : { kind: "idle", latestTurnId: owner.turn, admissionRevision: owner.admission } },
      draft_target: structuredClone(baseline.target), composer_commit_generation: baseline.commitGeneration,
      run_status_key: phase, busy: phase === "running", task_activity_state: phase === "running" ? "running" : "idle",
      post_run_refresh_pending: false, overlay: "none", draft_prompt: baseline.committedText,
    },
    errors: 0,
    route: phase === "running" ? { count: 1, target: "current-summary", enabled: true }
      : { count: 0, target: null, enabled: false },
    destination: { count: 1, anchor: "current-summary", history_identity: `turn:${owner.turn}:work-summary`,
      summary_focus_key: destinationIdentity.focusKey, details_open: true, summary_visible: true, summary_focused: true, same_summary: true },
    prompt: { count: 1, visible: true, value: OUTPUT_HISTORY_DRAFT, selection: options.selection, same_node: true, disabled: false, focused: false },
  };
}

test("completed native disclosure needs one trusted Enter activation click on the same summary", () => {
  const identity = { tag: "SUMMARY", focusKey: destinationIdentity.focusKey };
  const snapshot = () => ({ found: true, sequence: 3, dropped_through: 0, events: [
    { sequence: 1, type: "keydown", isTrusted: true, ...identity, key: "Enter", code: "Enter" },
    { sequence: 2, type: "click", isTrusted: true, ...identity },
    { sequence: 3, type: "keyup", isTrusted: true, ...identity, key: "Enter", code: "Enter" },
  ] });
  assert.equal(outputHistoryKeyboardProof(snapshot(), 0, { identity }).events.length, 3);
  for (const mutate of [
    (value) => { value.events.splice(1, 1); },
    (value) => { value.events[1].focusKey = "wrong-summary"; },
    (value) => { value.events[1].isTrusted = false; },
    (value) => { value.events.splice(2, 0, { ...value.events[1], sequence: 3 }); value.events[3].sequence = 4; value.sequence = 4; },
  ]) {
    const invalid = snapshot(); mutate(invalid);
    assert.throws(() => outputHistoryKeyboardProof(invalid, 0, { identity }));
  }
});

test("running and completed history navigation require an open visible focused destination", () => {
  assert.deepEqual(outputHistoryNavigationFailures(surface(), options), []);
  assert.deepEqual(outputHistoryNavigationFailures(surface("completed"), { ...options, phase: "completed" }), []);
  assert.deepEqual(outputHistoryOwner(surface().projection), outputHistoryOwner(surface("completed").projection));
});

test("completed history requires the canonical disclosure after the running-only route retires", () => {
  const completed = surface("completed"), final = { ...options, phase: "completed" };
  assert.deepEqual(outputHistoryNavigationFailures(completed, final), []);
  const stale = structuredClone(completed); stale.route = surface().route;
  assert.ok(outputHistoryNavigationFailures(stale, final).includes("completed-activity-route-not-retired"));
  for (const change of [
    (v) => { v.destination.count = 0; },
    (v) => { v.destination.history_identity = "turn:other:work-summary"; },
    (v) => { v.destination.summary_focus_key = "other-summary"; },
    (v) => { v.destination.details_open = null; },
  ]) {
    const invalid = structuredClone(completed); change(invalid);
    assert.ok(outputHistoryNavigationFailures(invalid, final).includes("history-destination-mismatch"));
  }
  completed.destination.details_open = false;
  assert.deepEqual(outputHistoryNavigationFailures(completed, { ...final, revealed: false }), []);
  assert.ok(outputHistoryNavigationFailures(completed, final).includes("history-disclosure-state-mismatch"));
  completed.destination.details_open = true; completed.destination.summary_focused = false;
  assert.ok(outputHistoryNavigationFailures(completed, final).includes("destination-focus-lost"));
});

test("a click with no reveal or focus movement fails even when the destination already occupies the viewport", () => {
  for (const [field, expected] of [["details_open", "history-disclosure-state-mismatch"], ["summary_focused", "destination-focus-lost"],
    ["summary_visible", "history-destination-not-visible"]]) {
    const value = surface(); value.destination[field] = false;
    assert.ok(outputHistoryNavigationFailures(value, options).includes(expected), field);
  }
});

test("passive transcript rerender may replace a card but must retain the same live canonical focused disclosure", () => {
  const value = surface(); value.destination.same_summary = false;
  assert.deepEqual(outputHistoryDestinationIdentity(value), destinationIdentity);
  assert.deepEqual(outputHistoryNavigationFailures(value, options), [], "node replacement alone is not focus loss");
  value.destination.summary_focused = false;
  assert.ok(outputHistoryNavigationFailures(value, options).includes("destination-focus-lost"));
  value.destination.summary_focused = true;
  value.destination.summary_focus_key = "other-disclosure";
  assert.ok(outputHistoryNavigationFailures(value, options).includes("history-destination-mismatch"));
  value.destination.summary_focus_key = destinationIdentity.focusKey;
  value.route.target = "other-anchor"; value.destination.anchor = "other-anchor";
  assert.ok(outputHistoryNavigationFailures(value, options).includes("history-destination-mismatch"),
    "the route and destination agreeing is insufficient if the observed canonical anchor changed");
});

test("navigation must keep the unsent value, selection and original editor through refresh", () => {
  for (const change of [
    (v) => { v.prompt.value = ""; },
    (v) => { v.prompt.count = 2; },
    (v) => { v.prompt.visible = false; },
    (v) => { v.prompt.same_node = false; },
    (v) => { v.prompt.disabled = true; },
  ]) {
    const value = surface(); change(value);
    assert.ok(outputHistoryNavigationFailures(value, options).includes("draft-or-editor-changed"));
  }
  const selection = surface(); selection.prompt.selection = [OUTPUT_HISTORY_DRAFT.length, OUTPUT_HISTORY_DRAFT.length];
  assert.ok(outputHistoryNavigationFailures(selection, options).includes("draft-selection-changed"));
});

test("local typing is ready while Rust retains its committed draft rather than mirroring each keystroke", () => {
  const value = surface(); value.prompt.focused = true; value.prompt.same_node = false;
  assert.equal(value.projection.draft_prompt, "");
  assert.notEqual(value.prompt.value, value.projection.draft_prompt);
  assert.deepEqual(outputHistoryDraftBaseline(value.projection), baseline);
  assert.deepEqual(outputHistoryDraftFailures(value, { ...options, focus: "prompt", sameNode: false }), []);
  assert.ok(outputHistoryDraftFailures(value, { ...options, focus: "prompt" }).includes("draft-or-editor-changed"),
    "node preservation is still required once the navigation phase remembers the actual editor");
  value.prompt.value = "";
  assert.ok(outputHistoryDraftFailures(value, { ...options, sameNode: false }).includes("draft-or-editor-changed"));
});

test("local draft preservation never permits an unexpected Rust commit or draft owner change", () => {
  for (const change of [
    (v) => { v.projection.draft_prompt = OUTPUT_HISTORY_DRAFT; },
    (v) => { v.projection.composer_commit_generation = "2"; },
    (v) => { v.projection.draft_target.ownerGeneration = "3"; },
    (v) => { v.projection.draft_target.sessionId = "session-b"; },
  ]) {
    const value = surface(); change(value);
    assert.ok(outputHistoryDraftFailures(value, options).includes("committed-draft-owner-changed"));
    assert.ok(outputHistoryNavigationFailures(value, options).includes("committed-draft-owner-changed"));
  }
  assert.ok(outputHistoryDraftFailures(surface(), { ...options, baseline: null }).includes("committed-draft-owner-changed"));
});

test("history target and its canonical owner cannot silently change", () => {
  for (const change of [
    (v) => { v.projection.run_target.workspacePath = "C:\\other"; },
    (v) => { v.projection.run_target.sessionId = "session-b"; },
    (v) => { v.projection.run_target.expectedState.turnId = "turn-b"; },
    (v) => { v.projection.run_target.expectedState.admissionRevision = "4"; },
    (v) => { v.projection.draft_target.sessionId = "session-b"; },
  ]) {
    const value = surface(); change(value);
    assert.ok(outputHistoryNavigationFailures(value, options).includes("run-or-session-owner-changed"));
  }
  for (const change of [
    (v) => { v.destination.history_identity = "turn:turn-b:work-summary"; },
    (v) => { v.destination.count = 0; },
  ]) {
    const value = surface(); change(value);
    assert.ok(outputHistoryNavigationFailures(value, options).includes("history-destination-mismatch"));
  }
  for (const change of [
    (v) => { v.route.count = 0; },
    (v) => { v.route.count = 2; },
    (v) => { v.route.target = "other-summary"; },
    (v) => { v.route.enabled = false; },
  ]) {
    const value = surface(); change(value);
    assert.ok(outputHistoryNavigationFailures(value, options).includes("history-route-mismatch"));
  }
});

test("draft focus before navigation and summary focus after navigation are separate expectations", () => {
  const value = surface(); value.prompt.focused = true; value.destination.summary_focused = false;
  assert.deepEqual(outputHistoryNavigationFailures(value, { ...options, focus: "prompt" }), []);
  assert.ok(outputHistoryNavigationFailures(value, options).includes("destination-focus-lost"));
  value.prompt.focused = false;
  assert.ok(outputHistoryNavigationFailures(value, { ...options, focus: "prompt" }).includes("draft-focus-lost"));
});

test("unfinished completion and error overlays cannot satisfy successful history navigation", () => {
  const value = surface("completed"); value.projection.post_run_refresh_pending = true;
  assert.ok(outputHistoryNavigationFailures(value, { ...options, phase: "completed" }).includes("completion-not-settled"));
  value.projection.overlay = "config";
  assert.ok(outputHistoryNavigationFailures(value, { ...options, phase: "completed" }).includes("error-or-overlay-present"));
  assert.notDeepEqual(outputHistoryNavigationFailures(null, options), []);
});

test("scenario uses the common lifecycle and leaves visual acceptance explicitly pending", () => {
  const scenario = createOutputHistoryNavigationScenario();
  assert.equal(scenario.id, "output.history-navigation");
  assert.equal(scenario.manualGate, "pending");
  assert.equal(scenario.databaseRequired, true);
  for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) assert.equal(typeof scenario[method], "function");
});

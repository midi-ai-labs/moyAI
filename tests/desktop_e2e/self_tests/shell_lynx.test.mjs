import assert from "node:assert/strict";
import test from "node:test";
import { createShellLynxScenario, lynxDraftFailures, lynxEditorAcquisitionReady, lynxLayoutFailures, lynxReplacementKeyEvents, lynxRunningComposerReady, lynxSettingsDraftReady } from "../scenarios/shell_lynx.mjs";
import { assertTrustedProbeSequence } from "../drivers/webview_input.mjs";

test("LYNX replacement accounts for release of every trusted key, including Backspace", () => {
  const identity = { tag: "TEXTAREA", id: "prompt" };
  const events = [
    ["keydown", "Control", "ControlLeft"], ["keydown", "a", "KeyA"],
    ["keyup", "a", "KeyA"], ["keyup", "Control", "ControlLeft"],
    ["keydown", "Backspace", "Backspace"], ["keyup", "Backspace", "Backspace"],
  ].map(([type, key, code], index) => ({ ...identity, sequence: index + 1, type, key, code, isTrusted: true }));
  const snapshot = { found: true, sequence: 6, dropped_through: 0, events };
  const expected = { afterSequence: 0, expected: lynxReplacementKeyEvents(identity) };
  assert.equal(assertTrustedProbeSequence(snapshot, expected).events.length, 6);
  assert.throws(() => assertTrustedProbeSequence({ ...snapshot, events: events.slice(0, -1) }, expected), /exact event sequence/);
  const untrusted = structuredClone(snapshot);
  untrusted.events[5].isTrusted = false;
  assert.throws(() => assertTrustedProbeSequence(untrusted, expected), /browser-trusted/);
});

function rect(left, top, width, height, extra = {}) {
  return { left, top, width, height, right: left + width, bottom: top + height,
    visible: true, center_hit: true, ...extra };
}
function surface() {
  return {
    viewport: { width: 1100, height: 720 },
    conversation: rect(260, 34, 840, 686), topbar: rect(260, 34, 840, 130),
    thread: rect(260, 164, 840, 556, { row: "3", padding_bottom: 232 }),
    composer: rect(300, 510, 400, 190), run_strip: null,
    run_stack: rect(260, 164, 840, 0, { row: "2", visible: false }),
    prompt: { ...rect(310, 524, 380, 70), value: "draft", active: true, same_node: true, selection_start: 5, selection_end: 5 },
    send: { ...rect(600, 602, 80, 34), text: "送信", enabled: true },
    hero: rect(450, 280, 310, 40), errors: 0, projection: { overlay: "none" },
  };
}

test("LYNX geometry rejects the observed idle auto-row bug and obscured composer", () => {
  assert.deepEqual(lynxLayoutFailures(surface(), { empty: true }), []);
  for (const [change, failure] of [
    [(value) => { value.thread.row = "2"; }, "thread-not-in-stretch-row"],
    [(value) => { value.thread.height = 120; value.thread.bottom = 284; }, "thread-viewport-collapsed"],
    [(value) => { value.thread.top = 300; }, "thread-detached-from-header"],
    [(value) => { value.thread.padding_bottom = 100; }, "composer-reserve-too-small"],
    [(value) => { value.send.center_hit = false; }, "composer-controls-occluded"],
    [(value) => { value.composer.right = 1102; }, "composer-outside-viewport"],
    [(value) => { value.hero.top = 170; }, "empty-state-placement"],
    [(value) => { value.hero.bottom = 515; }, "empty-state-placement"],
    [(value) => { value.send.text = ""; }, "send-has-no-visible-label"],
  ]) {
    const invalid = surface(); change(invalid);
    assert.ok(lynxLayoutFailures(invalid, { empty: true }).includes(failure), failure);
  }
});

test("LYNX running geometry requires the real strip above history and reachable Stop", () => {
  const running = surface();
  running.run_stack = rect(260, 164, 840, 40, { row: "2" });
  running.run_strip = rect(260, 164, 840, 40, { row: "auto" });
  running.thread = rect(260, 204, 840, 516, { row: "3", padding_bottom: 232 });
  running.stop = rect(640, 169, 30, 30);
  assert.deepEqual(lynxLayoutFailures(running, { running: true }), []);
  running.stop.center_hit = false;
  assert.ok(lynxLayoutFailures(running, { running: true }).includes("running-stop-occluded"));
});

test("LYNX running layout accepts a nested strip while rejecting detached status and clipped Stop", () => {
  const running = surface();
  running.run_stack = rect(260, 164, 840, 49, { row: "2" });
  running.run_strip = rect(260, 164, 840, 49, { row: "auto" });
  running.thread = rect(260, 213, 840, 507, { row: "3", padding_bottom: 232 });
  running.stop = rect(611, 170, 108, 36);
  assert.deepEqual(lynxLayoutFailures(running, { running: true }), []);
  for (const [change, failure] of [
    [(v) => { v.run_stack = null; }, "running-strip-detached"],
    [(v) => { v.run_stack.top += 20; }, "running-strip-detached"],
    [(v) => { v.run_stack.bottom -= 20; }, "thread-detached-from-header"],
    [(v) => { v.run_strip.bottom += 20; }, "running-strip-detached"],
    [(v) => { v.run_strip.visible = false; }, "running-strip-detached"],
    [(v) => { v.stop.bottom = 215; }, "running-stop-occluded"],
    [(v) => { v.stop.center_hit = false; }, "running-stop-occluded"],
  ]) {
    const invalid = structuredClone(running); change(invalid);
    assert.ok(lynxLayoutFailures(invalid, { running: true }).includes(failure), failure);
  }
  const stopped = surface(); stopped.run_stack = running.run_stack;
  assert.ok(lynxLayoutFailures(stopped).includes("unexpected-running-strip"));
});

test("LYNX draft evidence requires text, focus, selection and connected input identity", () => {
  assert.deepEqual(lynxDraftFailures(surface(), { value: "draft" }), []);
  for (const change of [
    (value) => { value.prompt.value = ""; },
    (value) => { value.prompt.active = false; },
    (value) => { value.prompt.same_node = false; },
    (value) => { value.prompt.selection_start = 0; },
    (value) => { value.projection.overlay = "config"; },
  ]) {
    const invalid = surface(); change(invalid);
    assert.notEqual(lynxDraftFailures(invalid, { value: "draft" }).length, 0);
  }
});

test("LYNX destructive keys require the connected focused editor and exact rendered owner", () => {
  const ready = surface();
  ready.prompt.disabled = false;
  ready.projection.run_target = { workspacePath: "C:\\e2e\\lynx", sessionId: null, runtimeOwnerToken: "idle:0" };
  ready.rendered_run_target = structuredClone(ready.projection.run_target);
  ready.run_target_parse_error = null;
  assert.equal(lynxEditorAcquisitionReady(ready, "prompt"), true);
  for (const change of [
    (value) => { value.prompt.active = false; },
    (value) => { value.prompt.same_node = false; },
    (value) => { value.prompt.disabled = true; },
    (value) => { value.rendered_run_target.runtimeOwnerToken = "root:1"; },
    (value) => { value.rendered_run_target = null; },
    (value) => { value.run_target_parse_error = "invalid JSON"; },
    (value) => { value.projection.overlay = "config"; },
  ]) {
    const invalid = structuredClone(ready); change(invalid);
    assert.equal(lynxEditorAcquisitionReady(invalid, "prompt"), false);
  }
  const settings = { projection: { overlay: "config" }, errors: 0,
    settings: { field_active: true, same_node: true, close_guard: false } };
  assert.equal(lynxEditorAcquisitionReady(settings, "settings"), true);
  settings.settings.field_active = false;
  assert.equal(lynxEditorAcquisitionReady(settings, "settings"), false);
});

test("LYNX settings requires the dirty visible focused editor and no close guard", () => {
  const ready = { projection: { overlay: "config" }, errors: 0,
    settings: { visible: true, field_value: "65537", field_active: true, same_node: true, dirty: true, close_guard: false } };
  assert.equal(lynxSettingsDraftReady(ready, "65537"), true);
  for (const change of [
    (value) => { value.settings.field_value = "65536"; },
    (value) => { value.settings.field_active = false; },
    (value) => { value.settings.same_node = false; },
    (value) => { value.settings.close_guard = true; },
  ]) {
    const invalid = structuredClone(ready); change(invalid);
    assert.equal(lynxSettingsDraftReady(invalid, "65537"), false);
  }
});

test("LYNX acquires the acknowledged rendered Running owner before testing same-owner polling", () => {
  const workspacePath = "C:\\e2e\\lynx";
  const sessionId = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
  const turnId = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
  const runTarget = {
    workspacePath, sessionId, runtimeOwnerToken: "root:1",
    permissionConfirmationId: null,
    expectedState: { kind: "turn", turnId, admissionRevision: "1" },
  };
  const ready = {
    projection: {
      task_activity_state: "running", async_polling_required: true, overlay: "none", draft_prompt: "",
      draft_target: { workspacePath, sessionId, ownerGeneration: "1" },
      run_target: runTarget,
      stop_target: { kind: "turn", workspacePath, sessionId, turnId, admissionRevision: "1", rootEpoch: "1" },
    },
    rendered_run_target: structuredClone(runTarget), run_target_parse_error: null,
    prompt: { value: "", disabled: false }, send: { enabled: false }, errors: 0,
  };
  assert.equal(lynxRunningComposerReady(ready), true);
  for (const change of [
    (value) => { value.prompt.value = "wait until user stop"; },
    (value) => { value.rendered_run_target.sessionId = null; },
    (value) => { value.rendered_run_target.runtimeOwnerToken = "idle:0"; },
    (value) => { value.rendered_run_target.expectedState.admissionRevision = "0"; },
    (value) => { value.projection.draft_target.sessionId = null; },
    (value) => { value.projection.stop_target.turnId = "turn-b"; },
    (value) => { value.run_target_parse_error = "invalid JSON"; },
    (value) => { value.prompt.disabled = true; },
    (value) => { value.send.enabled = true; },
  ]) {
    const invalid = structuredClone(ready); change(invalid);
    assert.equal(lynxRunningComposerReady(invalid), false);
  }
});

test("LYNX uses a fresh common-lifecycle scenario without external options", () => {
  const scenario = createShellLynxScenario();
  assert.equal(scenario.id, "shell.lynx");
  assert.equal(scenario.productOracle, "pass");
  for (const method of ["prepare", "execute", "quiesce", "cleanup", "requestGracefulExit"]) assert.equal(typeof scenario[method], "function");
  assert.notEqual(scenario, createShellLynxScenario());
});

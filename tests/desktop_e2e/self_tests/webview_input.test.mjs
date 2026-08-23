import assert from "node:assert/strict";
import test from "node:test";

import {
  WebviewInput,
  assertExactSemanticTarget,
  assertTrustedProbeSequence,
  normalizeSemanticLocator,
  normalizeWebviewKey,
} from "../drivers/webview_input.mjs";

const showShortcuts = Object.freeze({
  selector: 'button[data-action="show-shortcuts"]',
  identity: { tag: "BUTTON", action: "show-shortcuts" },
});

function targetObservation(overrides = {}) {
  return {
    count: 1,
    connected: true,
    visible: true,
    enabled: true,
    identity: { tag: "BUTTON", id: null, action: "show-shortcuts", focusKey: null, configKey: null, sideSetting: null },
    hit_identity: { tag: "BUTTON", id: null, action: "show-shortcuts", focusKey: null, configKey: null, sideSetting: null },
    center_hit: true,
    center: { x: 120.5, y: 48.25 },
    rect: { left: 100, top: 40, right: 141, bottom: 56.5, width: 41, height: 16.5 },
    ...overrides,
  };
}

class FakeCdp {
  constructor(evaluations = []) {
    this.evaluations = [...evaluations];
    this.evaluationExpressions = [];
    this.calls = [];
    this.failCall = null;
  }

  async evaluate(expression) {
    this.evaluationExpressions.push(expression);
    assert.ok(this.evaluations.length > 0, "unexpected evaluate call");
    const value = this.evaluations.shift();
    if (value instanceof Error) throw value;
    return structuredClone(value);
  }

  async call(method, params) {
    const call = { method, params: structuredClone(params) };
    this.calls.push(call);
    if (this.failCall?.(call)) throw new Error(`injected ${params.type} failure for ${params.code ?? params.button}`);
    return {};
  }
}

function event(sequence, type, identity, detail = {}) {
  return {
    sequence,
    type,
    isTrusted: true,
    ...identity,
    active: identity,
    key: null,
    code: null,
    button: null,
    buttons: null,
    pointerId: null,
    inputType: null,
    data: null,
    ...detail,
  };
}

test("semantic locators require stable identity and exact hit-tested ownership", () => {
  assert.deepEqual(normalizeSemanticLocator(showShortcuts), {
    ...showShortcuts,
    requireVisible: true,
    requireEnabled: true,
  });
  assert.throws(
    () => normalizeSemanticLocator({ selector: "button", identity: { tag: "BUTTON" } }),
    /requires id, action, focusKey, configKey, or sideSetting/,
  );

  const acquired = assertExactSemanticTarget(targetObservation(), showShortcuts);
  assert.deepEqual(acquired.center, { x: 120.5, y: 48.25 });
  assert.equal(acquired.identity.action, "show-shortcuts");
  assert.throws(
    () => assertExactSemanticTarget(targetObservation({ count: 2 }), showShortcuts),
    (error) => error.code === "semantic-target-cardinality",
  );
  assert.throws(
    () => assertExactSemanticTarget(targetObservation({ center_hit: false }), showShortcuts),
    (error) => error.code === "semantic-target-hit-test",
  );
  assert.throws(
    () => assertExactSemanticTarget(targetObservation({ hit_identity: { ...targetObservation().hit_identity, action: "refresh" } }), showShortcuts),
    (error) => error.code === "semantic-target-hit-identity",
  );
  assert.throws(
    () => assertExactSemanticTarget(targetObservation({ identity: { ...targetObservation().identity, action: "refresh" } }), showShortcuts),
    (error) => error.code === "semantic-target-identity",
  );
});

test("pointer press and release use the exact semantic center and remain separately controllable", async () => {
  const cdp = new FakeCdp([targetObservation()]);
  const input = new WebviewInput(cdp);

  const target = await input.pointerDown(showShortcuts);
  assert.equal(input.pointerPressed, true);
  assert.equal(target.identity.action, "show-shortcuts");
  assert.deepEqual(cdp.calls.map((call) => call.params.type), ["mouseMoved", "mousePressed"]);
  assert.deepEqual(cdp.calls[1].params, {
    type: "mousePressed",
    x: 120.5,
    y: 48.25,
    button: "left",
    buttons: 1,
    clickCount: 1,
    modifiers: 0,
  });
  await assert.rejects(input.pointerDown(showShortcuts), (error) => error.code === "pointer-already-pressed");

  await input.pointerUp();
  assert.equal(input.pointerPressed, false);
  assert.deepEqual(cdp.calls[2].params, {
    type: "mouseReleased",
    x: 120.5,
    y: 48.25,
    button: "left",
    buttons: 0,
    clickCount: 1,
    modifiers: 0,
  });
  await assert.rejects(input.pointerUp(), (error) => error.code === "pointer-not-pressed");
});

test("an ambiguous mousePressed remains cleanup-owned until mouseReleased is confirmed", async () => {
  const cdp = new FakeCdp([targetObservation()]);
  const input = new WebviewInput(cdp);
  cdp.failCall = (call) => call.params.type === "mousePressed";

  await assert.rejects(input.pointerDown(showShortcuts), /injected mousePressed failure/);
  assert.equal(input.pointerPressed, true);

  cdp.failCall = null;
  const cleanup = await input.releasePressedInputs();
  assert.equal(cleanup.pointer_released, true);
  assert.equal(input.pointerPressed, false);
  assert.deepEqual(cdp.calls.map((call) => call.params.type), ["mouseMoved", "mousePressed", "mouseReleased"]);
});

test("Escape, Tab, and printable keys expose distinct browser keyDown and keyUp calls", async () => {
  const cdp = new FakeCdp();
  const input = new WebviewInput(cdp);

  await input.keyDown("Escape");
  assert.deepEqual(input.pressedKeys, [{ key: "Escape", code: "Escape", delivery: "confirmed" }]);
  assert.equal(cdp.calls.length, 1, "keyDown does not imply release");
  await input.keyUp("Escape");
  await input.pressKey("Tab");
  await input.pressKey("a");

  assert.deepEqual(cdp.calls.map((call) => [call.params.type, call.params.code]), [
    ["keyDown", "Escape"],
    ["keyUp", "Escape"],
    ["keyDown", "Tab"],
    ["keyUp", "Tab"],
    ["keyDown", "KeyA"],
    ["keyUp", "KeyA"],
  ]);
  assert.equal(Object.hasOwn(cdp.calls[2].params, "text"), false, "Tab does not inject text");
  assert.equal(cdp.calls[4].params.text, "a");
  assert.equal(cdp.calls[4].params.unmodifiedText, "a");
  assert.deepEqual(input.pressedKeys, []);
  assert.throws(() => normalizeWebviewKey("A"), /unsupported WebView key/);
});

test("pressed-key cleanup attempts every key in reverse order and retains only failed releases", async () => {
  const cdp = new FakeCdp();
  const input = new WebviewInput(cdp);
  await input.keyDown("Control");
  await input.keyDown("Shift");
  await input.keyDown("Escape");
  cdp.failCall = (call) => call.params.type === "keyUp" && call.params.code === "ShiftLeft";

  await assert.rejects(
    input.releasePressedKeys(),
    (error) => error instanceof AggregateError && error.evidence.some((failure) => failure.owner === "key:ShiftLeft"),
  );
  assert.deepEqual(
    cdp.calls.slice(-3).map((call) => call.params.code),
    ["Escape", "ShiftLeft", "ControlLeft"],
    "a failed release does not prevent later cleanup attempts",
  );
  assert.deepEqual(input.pressedKeys, [{ key: "Shift", code: "ShiftLeft", delivery: "confirmed" }]);

  cdp.failCall = null;
  await input.releasePressedKeys();
  assert.deepEqual(input.pressedKeys, []);
});

test("an ambiguous keyDown remains cleanup-owned until a keyUp is confirmed", async () => {
  const cdp = new FakeCdp();
  const input = new WebviewInput(cdp);
  cdp.failCall = (call) => call.params.type === "keyDown" && call.params.code === "Escape";

  await assert.rejects(input.keyDown("Escape"), /injected keyDown failure/);
  assert.deepEqual(input.pressedKeys, [{ key: "Escape", code: "Escape", delivery: "ambiguous" }]);

  cdp.failCall = null;
  await input.releasePressedKeys();
  assert.deepEqual(input.pressedKeys, []);
  assert.deepEqual(cdp.calls.map((call) => call.params.type), ["keyDown", "keyUp"]);
});

test("trusted event acquisition enforces exact type order, trust, and semantic target", () => {
  const identity = { tag: "BUTTON", id: null, action: "show-shortcuts", focusKey: null, configKey: null, sideSetting: null };
  const snapshot = {
    found: true,
    probe_id: "input-probe",
    sequence: 4,
    dropped_through: 0,
    active: identity,
    events: [
      event(1, "pointerdown", identity, { pointerId: 1, button: 0, buttons: 1 }),
      event(2, "focusin", identity),
      event(3, "pointerup", identity, { pointerId: 1, button: 0, buttons: 0 }),
      event(4, "click", identity, { button: 0, buttons: 0 }),
    ],
  };
  const expected = [
    { type: "pointerdown", identity: { action: "show-shortcuts" }, button: 0, buttons: 1 },
    { type: "pointerup", identity: { action: "show-shortcuts" }, button: 0, buttons: 0 },
    { type: "click", identity: { action: "show-shortcuts" }, button: 0 },
  ];
  const acquired = assertTrustedProbeSequence(snapshot, { afterSequence: 0, expected });
  assert.deepEqual(acquired.events.map((row) => row.type), ["pointerdown", "pointerup", "click"]);

  const untrusted = structuredClone(snapshot);
  untrusted.events[2].isTrusted = false;
  assert.throws(
    () => assertTrustedProbeSequence(untrusted, { afterSequence: 0, expected }),
    (error) => error.code === "event-probe-untrusted",
  );
  const wrongTarget = structuredClone(snapshot);
  wrongTarget.events[3].action = "refresh";
  assert.throws(
    () => assertTrustedProbeSequence(wrongTarget, { afterSequence: 0, expected }),
    (error) => error.code === "event-probe-target",
  );
  const unordered = structuredClone(snapshot);
  unordered.events[2].sequence = 1;
  assert.throws(
    () => assertTrustedProbeSequence(unordered, { afterSequence: 0, expected }),
    (error) => error.code === "event-probe-order",
  );
});

test("event probe ownership is injected through CDP and cleanup removes it after releasing input", async () => {
  const cdp = new FakeCdp([
    { installed: true, probe_id: "input-probe", sequence: 0 },
    { found: true, probe_id: "input-probe", sequence: 0, dropped_through: 0, active: null, events: [] },
    { removed: true, probe_id: "input-probe", sequence: 0 },
  ]);
  const input = new WebviewInput(cdp, { probeId: "input-probe" });

  await input.installProbe();
  assert.equal(input.probeInstalled, true);
  const snapshot = await input.snapshotProbe();
  assert.equal(snapshot.sequence, 0);
  const cleanup = await input.cleanup();
  assert.equal(cleanup.probe.removed, true);
  assert.equal(input.probeInstalled, false);
  assert.equal(cdp.evaluationExpressions.length, 3);
});

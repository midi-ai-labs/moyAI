import assert from "node:assert/strict";
import test from "node:test";

import {
  WebviewInput,
  assertExactSemanticTarget,
  assertTrustedProbeSequence,
  assertTrustedTextInsertion,
  normalizeSemanticIdentity,
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
    center_in_viewport: true,
    center_in_scroll_clip: true,
    center: { x: 120.5, y: 48.25 },
    viewport: { width: 1440, height: 900 },
    scroll_clip: { left: 0, top: 0, right: 1440, bottom: 900 },
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
  assert.equal(
    normalizeSemanticIdentity({ tag: "SECTION", modal: "" }).modal,
    null,
    "boolean data attributes are presence markers, not empty semantic identities",
  );
  assert.throws(
    () => normalizeSemanticIdentity({ tag: "" }),
    /semantic identity tag must not be empty/,
  );
  assert.deepEqual(normalizeSemanticLocator(showShortcuts), {
    ...showShortcuts,
    requireVisible: true,
    requireEnabled: true,
  });
  assert.throws(
    () => normalizeSemanticLocator({ selector: "button", identity: { tag: "BUTTON" } }),
    /requires id, action, focusKey, configKey, sideSetting, sessionSetting, sessionSettingsTrigger, surface, modal, step, field, detailsKey, or href/,
  );
  assert.deepEqual(normalizeSemanticLocator({
    selector: 'a[href="#settings-tools"]',
    identity: { tag: "A", href: "#settings-tools" },
  }), {
    selector: 'a[href="#settings-tools"]',
    identity: { tag: "A", href: "#settings-tools" },
    requireVisible: true,
    requireEnabled: true,
  });
  assert.deepEqual(normalizeSemanticLocator({
    selector: 'details[data-details-key="side-chat-manual-model"] > summary',
    identity: { tag: "DETAILS", detailsKey: "side-chat-manual-model" },
  }), {
    selector: 'details[data-details-key="side-chat-manual-model"] > summary',
    identity: { tag: "DETAILS", detailsKey: "side-chat-manual-model" },
    requireVisible: true,
    requireEnabled: true,
  });
  assert.deepEqual(normalizeSemanticLocator({
    selector: '[data-action="show-session-settings"][data-session-settings-trigger="model"]',
    identity: { tag: "BUTTON", action: "show-session-settings", sessionSettingsTrigger: "model" },
  }), {
    selector: '[data-action="show-session-settings"][data-session-settings-trigger="model"]',
    identity: { tag: "BUTTON", action: "show-session-settings", sessionSettingsTrigger: "model" },
    requireVisible: true,
    requireEnabled: true,
  });
  assert.deepEqual(normalizeSemanticLocator({
    selector: '[data-modal="session-settings"] [data-session-setting="model"]',
    identity: { tag: "INPUT", sessionSetting: "model" },
  }), {
    selector: '[data-modal="session-settings"] [data-session-setting="model"]',
    identity: { tag: "INPUT", sessionSetting: "model" },
    requireVisible: true,
    requireEnabled: true,
  });

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

test("exact target observation is input-free and hover emits only a trusted-pointer-capable move", async () => {
  const observation = targetObservation();
  const cdp = new FakeCdp([observation, observation]);
  const input = new WebviewInput(cdp);

  const observed = await input.observeExactTarget(showShortcuts);
  assert.deepEqual(observed.locator, normalizeSemanticLocator(showShortcuts));
  assert.deepEqual(observed.observation, observation);
  assert.equal(cdp.calls.length, 0, "observing a hover-revealed child must not synthesize input");

  const target = await input.hover(showShortcuts);
  assert.equal(target.identity.action, "show-shortcuts");
  assert.equal(input.pointerPressed, false);
  assert.deepEqual(cdp.calls, [{
    method: "Input.dispatchMouseEvent",
    params: {
      type: "mouseMoved",
      x: 120.5,
      y: 48.25,
      button: "none",
      buttons: 0,
      modifiers: 0,
    },
  }]);
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
  await assert.rejects(input.hover(showShortcuts), (error) => error.code === "pointer-already-pressed");
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

test("semantic target acquisition waits without input for the product scroll to make the exact target hit-testable", async () => {
  const offscreen = targetObservation({
    hit_identity: { tag: "", id: null, action: null, focusKey: null, configKey: null, sideSetting: null },
    center_hit: false,
    center_in_viewport: false,
    center: { x: 1233.0625, y: 2273.515625 },
    rect: { left: 1179.125, top: 2255.015625, right: 1287, bottom: 2292.015625, width: 107.875, height: 37 },
  });
  const clipped = targetObservation({
    hit_identity: { tag: "DIV", id: null, action: null, focusKey: null, configKey: null, sideSetting: null },
    center_hit: false,
    center_in_viewport: true,
    center_in_scroll_clip: false,
    center: { x: 1233.0625, y: 851.5 },
    rect: { left: 1179.125, top: 833, right: 1287, bottom: 870, width: 107.875, height: 37 },
    scroll_clip: { left: 190, top: 90, right: 1370, bottom: 820 },
  });
  const moving = targetObservation({
    center: { x: 1233.0625, y: 513.5 },
    rect: { left: 1179.125, top: 495, right: 1287, bottom: 532, width: 107.875, height: 37 },
  });
  const settled = targetObservation({
    center: { x: 1233.0625, y: 413.5 },
    rect: { left: 1179.125, top: 395, right: 1287, bottom: 432, width: 107.875, height: 37 },
  });
  const cdp = new FakeCdp([offscreen, clipped, moving, settled, settled, settled]);
  const input = new WebviewInput(cdp, {
    targetAcquisitionTimeoutMs: 100,
    targetAcquisitionPollMs: 0,
  });

  const target = await input.pointerDown(showShortcuts);
  assert.equal(target.acquisition.kind, "existing-scroll-settled");
  assert.equal(target.acquisition.attempts, 6);
  assert.equal(target.acquisition.stable_hit_samples, 3);
  assert.equal(target.acquisition.initial_observation.center.y, 2273.515625);
  assert.equal(target.acquisition.final_observation.center.y, 413.5);
  assert.equal(cdp.evaluationExpressions.length, 6);
  assert.doesNotMatch(cdp.evaluationExpressions.join("\n"), /scrollIntoView/);
  assert.deepEqual(cdp.calls.map((call) => call.params.type), ["mouseMoved", "mousePressed"]);
  assert.equal(cdp.calls[0].params.y, 413.5);
  await input.pointerUp();
});

test("explicit stable-hit acquisition waits through an already hittable smooth scroll", async () => {
  const first = targetObservation({
    center: { x: 1233.0625, y: 513.5 },
    rect: { left: 1179.125, top: 495, right: 1287, bottom: 532, width: 107.875, height: 37 },
  });
  const moving = targetObservation({
    center: { x: 1233.0625, y: 463.5 },
    rect: { left: 1179.125, top: 445, right: 1287, bottom: 482, width: 107.875, height: 37 },
  });
  const settled = targetObservation({
    center: { x: 1233.0625, y: 413.5 },
    rect: { left: 1179.125, top: 395, right: 1287, bottom: 432, width: 107.875, height: 37 },
  });
  const cdp = new FakeCdp([first, moving, settled, settled, settled]);
  const input = new WebviewInput(cdp, {
    targetAcquisitionTimeoutMs: 100,
    targetAcquisitionPollMs: 0,
  });

  const target = await input.pointerDown(showShortcuts, { stableHitSamples: 3 });
  assert.equal(target.acquisition.kind, "stable-hit-settled");
  assert.equal(target.acquisition.attempts, 5);
  assert.equal(target.acquisition.stable_hit_samples, 3);
  assert.equal(target.center.y, 413.5);
  assert.deepEqual(cdp.calls.map((call) => call.params.type), ["mouseMoved", "mousePressed"]);
  await input.pointerUp();
});

test("semantic target acquisition never waits through viewport occlusion or identity drift", async () => {
  const occluded = targetObservation({
    hit_identity: { ...targetObservation().hit_identity, action: "covering-control" },
    center_hit: false,
    center_in_viewport: true,
  });
  const occludedCdp = new FakeCdp([occluded]);
  const occludedInput = new WebviewInput(occludedCdp);
  await assert.rejects(
    occludedInput.pointerDown(showShortcuts),
    (error) => error.code === "semantic-target-hit-test"
      && error.evidence.observation.center_in_viewport === true,
  );
  assert.equal(occludedCdp.evaluationExpressions.length, 1);
  assert.equal(occludedCdp.calls.length, 0);

  const offscreen = targetObservation({ center_hit: false, center_in_viewport: false });
  const drifted = targetObservation({
    identity: { ...targetObservation().identity, action: "different-action" },
  });
  const driftedCdp = new FakeCdp([offscreen, drifted]);
  const driftedInput = new WebviewInput(driftedCdp, {
    targetAcquisitionTimeoutMs: 100,
    targetAcquisitionPollMs: 0,
  });
  await assert.rejects(
    driftedInput.pointerDown(showShortcuts),
    (error) => error.code === "semantic-target-identity",
  );
  assert.equal(driftedCdp.evaluationExpressions.length, 2);
  assert.equal(driftedCdp.calls.length, 0);
});

test("semantic target viewport timeout preserves the last no-input observation", async () => {
  const first = targetObservation({
    center_hit: false,
    center_in_viewport: false,
    center: { x: 120, y: 2255 },
  });
  const last = targetObservation({
    center_hit: false,
    center_in_viewport: false,
    center: { x: 120, y: 1800 },
  });
  const cdp = new FakeCdp([first, last]);
  let currentTime = 0;
  const input = new WebviewInput(cdp, {
    targetAcquisitionTimeoutMs: 10,
    targetAcquisitionPollMs: 10,
    now: () => currentTime,
    wait: async (milliseconds) => { currentTime += milliseconds; },
  });

  await assert.rejects(
    input.pointerDown(showShortcuts),
    (error) => error.code === "semantic-target-viewport-timeout"
      && error.evidence.attempts === 2
      && error.evidence.elapsed_ms === 10
      && error.evidence.initial_observation.center.y === 2255
      && error.evidence.last_observation.center.y === 1800,
  );
  assert.equal(cdp.evaluationExpressions.length, 2);
  assert.equal(cdp.calls.length, 0);
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

test("navigation, Escape, Tab, and printable keys expose distinct browser keyDown and keyUp calls", async () => {
  const cdp = new FakeCdp();
  const input = new WebviewInput(cdp);

  await input.keyDown("Escape");
  assert.deepEqual(input.pressedKeys, [{ key: "Escape", code: "Escape", delivery: "confirmed" }]);
  assert.equal(cdp.calls.length, 1, "keyDown does not imply release");
  await input.keyUp("Escape");
  await input.pressKey("Home");
  await input.pressKey("ArrowDown");
  await input.pressKey("Tab");
  await input.pressKey("a");

  assert.deepEqual(cdp.calls.map((call) => [call.params.type, call.params.code]), [
    ["keyDown", "Escape"],
    ["keyUp", "Escape"],
    ["keyDown", "Home"],
    ["keyUp", "Home"],
    ["keyDown", "ArrowDown"],
    ["keyUp", "ArrowDown"],
    ["keyDown", "Tab"],
    ["keyUp", "Tab"],
    ["keyDown", "KeyA"],
    ["keyUp", "KeyA"],
  ]);
  for (const call of cdp.calls.slice(2, 8)) {
    assert.equal(Object.hasOwn(call.params, "text"), false, "navigation keys do not inject text");
  }
  assert.equal(cdp.calls[8].params.text, "a");
  assert.equal(cdp.calls[8].params.unmodifiedText, "a");
  assert.deepEqual(input.pressedKeys, []);
  assert.throws(() => normalizeWebviewKey("A"), /unsupported WebView key/);
});

test("exact focused text insertion supports byte-identical Unicode and multiline input without DOM assignment", async () => {
  const identity = {
    tag: "TEXTAREA",
    id: "prompt",
    action: null,
    focusKey: null,
    configKey: null,
    sideSetting: null,
    sessionSetting: null,
    sessionSettingsTrigger: null,
    surface: null,
    modal: null,
    step: null,
    field: null,
    href: null,
  };
  const prompt = {
    selector: "textarea#prompt",
    identity: { tag: "TEXTAREA", id: "prompt" },
  };
  const observation = targetObservation({ identity, hit_identity: identity });
  const text = "日本語の依頼です。\nsecond line\n";
  const cdp = new FakeCdp([observation, identity]);
  const input = new WebviewInput(cdp);

  const inserted = await input.insertText(prompt, text);
  assert.equal(inserted.delivery, "confirmed");
  assert.equal(inserted.text, text);
  assert.equal(inserted.character_count, Array.from(text).length);
  assert.equal(inserted.utf8_byte_count, Buffer.byteLength(text, "utf8"));
  assert.deepEqual(cdp.calls, [{ method: "Input.insertText", params: { text } }]);
  assert.doesNotMatch(cdp.evaluationExpressions.join("\n"), /\.value\s*=/);
});

test("trusted multiline insertion reconstructs WebView2 newline event segmentation exactly", () => {
  const identity = { tag: "TEXTAREA", id: "prompt" };
  const snapshot = {
    found: true,
    probe_id: "text-segments",
    sequence: 3,
    dropped_through: 0,
    active: identity,
    events: [
      event(1, "input", identity, { inputType: "insertText", data: "Unicode入力 😀" }),
      event(2, "input", identity, { inputType: "insertText", data: null }),
      event(3, "input", identity, { inputType: "insertText", data: "複数行" }),
    ],
  };
  const acquired = assertTrustedTextInsertion(snapshot, {
    afterSequence: 0,
    identity,
    text: "Unicode入力 😀\n複数行",
  });
  assert.equal(acquired.segment_count, 3);
  assert.equal(acquired.reconstructed_text, "Unicode入力 😀\n複数行");
  assert.throws(
    () => assertTrustedTextInsertion(snapshot, { afterSequence: 0, identity, text: "Unicode入力 😀複数行" }),
    (error) => error.code === "event-probe-text",
  );
});

test("text insertion fail-stops before delivery on focus drift and after ambiguous CDP delivery", async () => {
  const identity = {
    tag: "TEXTAREA",
    id: "prompt",
    action: null,
    focusKey: null,
    configKey: null,
    sideSetting: null,
    sessionSetting: null,
    sessionSettingsTrigger: null,
    surface: null,
    modal: null,
    step: null,
    field: null,
    href: null,
  };
  const prompt = { selector: "textarea#prompt", identity: { tag: "TEXTAREA", id: "prompt" } };
  const observation = targetObservation({ identity, hit_identity: identity });
  const drifted = { ...identity, id: "different" };
  const focusDrift = new FakeCdp([observation, drifted]);
  await assert.rejects(
    new WebviewInput(focusDrift).insertText(prompt, "request"),
    (error) => error.code === "text-insert-focus-owner",
  );
  assert.equal(focusDrift.calls.length, 0);

  const ambiguous = new FakeCdp([observation, identity]);
  ambiguous.failCall = (call) => call.method === "Input.insertText";
  await assert.rejects(
    new WebviewInput(ambiguous).insertText(prompt, "request"),
    (error) => error.code === "text-insert-delivery-ambiguous",
  );
  assert.equal(ambiguous.calls.length, 1, "ambiguous delivery is attempted exactly once");
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

test("native select acquisition binds the exact trusted committed change", () => {
  const identity = { tag: "SELECT", configKey: "model.provider_profile" };
  const snapshot = {
    found: true,
    probe_id: "input-probe",
    sequence: 4,
    dropped_through: 0,
    active: identity,
    events: [
      event(1, "input", identity),
      event(2, "change", identity),
      event(3, "focusin", identity),
      event(4, "keyup", identity, { key: "Enter", code: "Enter" }),
    ],
  };
  const expected = [{ type: "change", identity }];

  assert.deepEqual(
    assertTrustedProbeSequence(snapshot, { afterSequence: 0, expected }).events,
    [snapshot.events[1]],
  );
  for (const changed of [
    { events: snapshot.events.filter((row) => row.type !== "change"), code: "event-probe-cardinality" },
    { events: snapshot.events.map((row) => row.type === "change" ? { ...row, isTrusted: false } : row), code: "event-probe-untrusted" },
    { events: snapshot.events.map((row) => row.type === "change" ? { ...row, configKey: "model.model" } : row), code: "event-probe-target" },
  ]) {
    assert.throws(
      () => assertTrustedProbeSequence({ ...snapshot, events: changed.events }, { afterSequence: 0, expected }),
      (error) => error.code === changed.code,
    );
  }
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

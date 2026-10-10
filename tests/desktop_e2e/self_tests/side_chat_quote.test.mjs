import assert from "node:assert/strict";
import test from "node:test";
import vm from "node:vm";
import { assertExactSemanticTarget, assertTrustedProbeSequence } from "../drivers/webview_input.mjs";
import { sideChatStoppedSurfaceReady, trustedSideSettingsClick } from "../scenarios/side_chat_quote.mjs";

function settingsPromptFixture({ initiallyClipped = true, revealsAfterReentry = true, wheelReveals = true,
  focusScrollOffsets = [0], focusScrollReveals = false } = {}) {
  const locator = { selector: "textarea#side-chat-system-prompt", identity: { tag: "TEXTAREA", id: "side-chat-system-prompt", configKey: "side_chat.system_prompt" } };
  const controls = Array.from({ length: 3 }, () => ({ disabled: false, closest: () => null }));
  const node = controls[1];
  controls.forEach((control, index) => { control.compareDocumentPosition = other => other === node ? index > 1 ? 2 : index < 1 ? 4 : 0 : 0; });
  const document = { activeElement: initiallyClipped ? controls[0] : node, querySelectorAll: selector => selector === locator.selector ? [node] : controls };
  const cdp = { evaluate: async source => vm.runInNewContext(source, { document }) };
  const keys = [], clicks = [], events = [], wheels = [], records = [];
  let position = initiallyClipped ? 0 : 1, shift = false, wheelDelivered = false, focusSamples = 0, focusOffset = 0;
  function geometry() {
    const clipped = initiallyClipped && !(revealsAfterReentry && keys.length >= 3) && !(wheelDelivered && wheelReveals)
      && !(focusScrollReveals && focusOffset >= 140);
    const centerY = clipped ? 940 - focusOffset : 800;
    return {
      count: 1, connected: true, visible: true, enabled: true,
      identity: locator.identity, hit_identity: clipped ? { tag: "DIV" } : locator.identity,
      center_hit: !clipped, center_in_viewport: true, center_in_scroll_clip: !clipped,
      center: { x: 870, y: centerY }, viewport: { width: 1560, height: 960 },
      scroll_clip: { left: 431, top: 249, right: 1309, bottom: 906 },
      rect: { left: 453, top: centerY - 56, right: 1287, bottom: centerY + 56, width: 834, height: 112 },
    };
  }
  const input = {
    keyDown: async key => { if (key === "Shift") shift = true; },
    keyUp: async key => { if (key === "Shift") shift = false; },
    pressKey: async key => {
      keys.push(shift ? `Shift+${key}` : key);
      position = (position + (shift ? controls.length - 1 : 1)) % controls.length;
      document.activeElement = controls[position];
    },
    observeExactTarget: async () => ({ observation: geometry() }),
    observeScrollContainer: async () => {
      if (!wheelDelivered) focusOffset = focusScrollOffsets[Math.min(focusSamples++, focusScrollOffsets.length - 1)];
      return { observation: { target: geometry(), active_identity: locator.identity,
        container: { identity: { tag: "SECTION" } }, scroll_top: wheelDelivered && wheelReveals ? 360 : focusOffset, scroll_left: 0 } };
    },
    scrollContainer: async (target, parameters) => {
      const before = await input.observeScrollContainer();
      wheels.push({ target, parameters, before }); wheelDelivered = true;
      events.push({ sequence: events.length + 1, type: "wheel", isTrusted: true, tag: "SECTION",
        deltaX: parameters.deltaX, deltaY: parameters.deltaY, deltaMode: 0 });
      return { before, parameters, after: await input.observeScrollContainer() };
    },
    resolveExactTarget: async target => assertExactSemanticTarget(geometry(), target),
    snapshotProbe: async (after = 0) => ({ found: true, sequence: events.length, dropped_through: 0, events: events.filter(event => event.sequence > after) }),
    waitForTrustedProbeSequence: async options => assertTrustedProbeSequence(await input.snapshotProbe(options.afterSequence), options),
    click: async target => {
      const acquired = assertExactSemanticTarget(geometry(), target);
      assert.equal(document.activeElement, node);
      clicks.push(target);
      for (const [type, buttons] of [["pointerdown", 1], ["pointerup", 0], ["click", 0]])
        events.push({ sequence: events.length + 1, type, isTrusted: true, ...locator.identity, button: 0, buttons });
      return acquired;
    },
  };
  return { input, cdp, locator, keys, clicks, wheels, records,
    sink: { record: async (name, value, metadata) => records.push([name, structuredClone(value), metadata]) } };
}

test("Side settings acquire a visible textarea whose center is outside the scroll clip before clicking", async () => {
  const f = settingsPromptFixture();
  const result = await trustedSideSettingsClick(f.input, f.cdp, f.locator);
  assert.deepEqual(f.keys, ["Tab", "Tab", "Shift+Tab"]);
  assert.equal(f.clicks.length, 1);
  assert.equal(result.probe.events.length, 3);
  assert.equal(f.wheels.length, 0);
});

test("Side settings use one scoped trusted wheel when keyboard focus still exposes only a caret line", async () => {
  const f = settingsPromptFixture({ revealsAfterReentry: false });
  const result = await trustedSideSettingsClick(f.input, f.cdp, f.locator, { sink: f.sink });
  assert.deepEqual(f.keys, ["Tab", "Tab", "Shift+Tab"]);
  assert.equal(f.wheels.length, 1);
  assert.match(f.wheels[0].parameters.containerSelector, /config-dialog-title/);
  assert.equal(result.wheel_acquisition.dispatch.before.observation.target.center_in_scroll_clip, false);
  assert.equal(result.wheel_acquisition.settled.observation.target.center_in_scroll_clip, true);
  assert.equal(result.wheel_acquisition.probe.events[0].isTrusted, true);
  assert.equal(f.clicks.length, 1);
  assert.deepEqual(f.records.map(row => row[0]), ["side-chat-settings-focus-scroll-settled", "side-chat-settings-wheel-acquisition", "side-chat-settings-wheel-settled"]);
  assert.equal(f.records[1][1].probe, undefined, "dispatch must be saved before asynchronous event validation");
  assert.equal(f.records[2][1].probe.events.length, 1);
});

test("Side wheel dispatch evidence survives missing native event settlement", async () => {
  const f = settingsPromptFixture({ revealsAfterReentry: false });
  f.input.waitForTrustedProbeSequence = async () => { throw Object.assign(new Error("native event absent"), { code: "observation-timeout" }); };
  await assert.rejects(trustedSideSettingsClick(f.input, f.cdp, f.locator, { sink: f.sink }), error => error.code === "observation-timeout");
  assert.equal(f.wheels.length, 1);
  assert.equal(f.clicks.length, 0);
  assert.equal(f.records.length, 2);
  assert.equal(f.records[1][0], "side-chat-settings-wheel-acquisition");
  assert.equal(f.records[1][1].dispatch.before.observation.target.center_in_scroll_clip, false);
});

test("Side settings preserve exact pointer rejection if a single wheel does not expose the center", async () => {
  const f = settingsPromptFixture({ revealsAfterReentry: false, wheelReveals: false });
  await assert.rejects(trustedSideSettingsClick(f.input, f.cdp, f.locator, { sink: f.sink }), error => error.code === "semantic-target-hit-test");
  assert.equal(f.wheels.length, 1);
  assert.equal(f.clicks.length, 0);
  assert.equal(f.records[1][0], "side-chat-settings-wheel-acquisition", "failed settlement must still preserve the actual wheel comparison");
});

test("Side settings finish native focus movement before computing and sending the single wheel", async () => {
  const f = settingsPromptFixture({ revealsAfterReentry: false, focusScrollOffsets: [0, 2, 2, 3, 4, 4, 4] });
  const result = await trustedSideSettingsClick(f.input, f.cdp, f.locator, { sink: f.sink });
  assert.equal(f.wheels.length, 1); assert.equal(f.clicks.length, 1);
  assert.equal(f.wheels[0].before.observation.scroll_top, 4, "the intermediate stationary position must not dispatch a wheel");
  assert.equal(f.wheels[0].parameters.deltaY, 359, "the wheel must use the settled center936 and clip midpoint577.5");
  assert.deepEqual(result.wheel_acquisition.focus_scroll.samples.map(value => value.scroll_top), [0, 2, 2, 3, 4, 4, 4]);
  assert.equal(result.wheel_acquisition.focus_scroll.stable_samples, 3);
});

test("Side settings skip the wheel when native focus movement fully exposes the center", async () => {
  const f = settingsPromptFixture({ revealsAfterReentry: false, focusScrollOffsets: [0, 140, 140, 140], focusScrollReveals: true });
  await trustedSideSettingsClick(f.input, f.cdp, f.locator, { sink: f.sink });
  assert.equal(f.wheels.length, 0); assert.equal(f.clicks.length, 1);
  assert.deepEqual(f.records.map(row => row[0]), ["side-chat-settings-focus-scroll-settled"]);
});

test("Side settings refuse to dispatch input while native focus geometry keeps moving", async () => {
  const f = settingsPromptFixture({ revealsAfterReentry: false });
  let offset = 0;
  f.input.observeScrollContainer = async () => ({ observation: { scroll_top: ++offset, scroll_left: 0,
    target: { center: { x: 870, y: 940 - offset }, rect: { top: 884 - offset }, scroll_clip: { top: 249, bottom: 906 } } } });
  await assert.rejects(trustedSideSettingsClick(f.input, f.cdp, f.locator), error => error.code === "observation-timeout");
  assert.equal(f.wheels.length, 0); assert.equal(f.clicks.length, 0);
});

test("Side settings leave an already focused and acquired editor in place", async () => {
  const f = settingsPromptFixture({ initiallyClipped: false });
  await trustedSideSettingsClick(f.input, f.cdp, f.locator);
  assert.deepEqual(f.keys, []);
  assert.equal(f.clicks.length, 1);
  assert.equal(f.wheels.length, 0);
});

const TARGET = { ownerSessionId: "owner-a", chatId: "chat-a", expectedGeneration: "2" };
function stoppedSurface() {
  return {
    projection: { side_chat: {
      owner_session_id: TARGET.ownerSessionId, chat_id: TARGET.chatId, generation: TARGET.expectedGeneration,
      status: "cancelled", deleting: false, can_send: true, can_cancel: false,
      phase: "", last_error: "run stopped by user", draft_text: "", draft_quote: null,
    } },
    side: {
      pane_count: 1, pane_visible: true, setup_visible: false, owner_session_id: TARGET.ownerSessionId,
      status_count: 1, status_visible: true, status_text: "停止済み",
      notice_texts: ["サイドチャットの実行を停止しました。"],
      stop_count: 0, stop_visible: false, stop_enabled: false,
      prompt_visible: true, prompt_enabled: true, prompt_value: "",
      send_visible: true, send_enabled: false, pending_count: 0, error_count: 0,
    },
    visible_fatal_count: 0, visible_recoverable_error_count: 0,
  };
}

test("Side stop waits for the visible same-owner terminal UI after backend cancellation", () => {
  assert.equal(sideChatStoppedSurfaceReady(stoppedSurface(), TARGET), true);
  const early = stoppedSurface();
  Object.assign(early.side, {
    status_text: "実行中 · stop requested", stop_count: 1, stop_visible: true, stop_enabled: true,
    notice_texts: [],
  });
  assert.equal(sideChatStoppedSurfaceReady(early, TARGET), false, "the previously premature stopped screenshot must fail");
});

test("Side stop rejects wrong owners, leftover actions, unavailable input and non-terminal feedback", () => {
  for (const change of [
    value => { value.projection.side_chat.owner_session_id = "other"; },
    value => { value.projection.side_chat.chat_id = "other"; },
    value => { value.projection.side_chat.generation = "3"; },
    value => { value.projection.side_chat.status = "running"; },
    value => { value.projection.side_chat.deleting = true; },
    value => { value.projection.side_chat.can_cancel = true; },
    value => { value.projection.side_chat.phase = "stop requested"; },
    value => { value.projection.side_chat.last_error = "storage failure"; },
    value => { value.projection.side_chat.draft_text = "unexpected"; },
    value => { value.side.owner_session_id = "other"; },
    value => { value.side.pane_count = 2; },
    value => { value.side.pane_visible = false; },
    value => { value.side.status_count = 2; },
    value => { value.side.status_visible = false; },
    value => { value.side.status_text = "実行中"; },
    value => { value.side.notice_texts = []; },
    value => { value.side.stop_count = 1; },
    value => { value.side.stop_visible = true; },
    value => { value.side.prompt_enabled = false; },
    value => { value.side.prompt_value = "wrong draft"; },
    value => { value.side.send_visible = false; },
    value => { value.side.send_enabled = true; },
    value => { value.side.pending_count = 1; },
    value => { value.side.error_count = 1; },
    value => { value.visible_recoverable_error_count = 1; },
  ]) {
    const invalid = stoppedSurface(); change(invalid);
    assert.equal(sideChatStoppedSurfaceReady(invalid, TARGET), false);
  }
});

import assert from "node:assert/strict";
import test from "node:test";
import vm from "node:vm";
import { trustedClick, trustedFocus } from "../scenarios/hub_browser_enrollment.mjs";

function fixture({ targetIndex = 130, count = 140, reachable = true, trusted = true, focusAfterTabs = null, disabledChecks = 0 } = {}) {
  const target = { selector: "#older-versions > summary", identity: { tag: "DETAILS", detailsKey: "older-versions" } };
  const controls = Array.from({ length: count }, () => ({ disabled: false, closest: () => null }));
  const node = controls[targetIndex];
  let disabledReads = 0;
  Object.defineProperty(node, "disabled", { get: () => disabledReads++ < disabledChecks });
  const document = { activeElement: null, querySelectorAll: selector => selector === target.selector ? [node] : controls };
  const cdp = { evaluate: async source => vm.runInNewContext(source, { document }) };
  const keys = [], clicks = [], events = [], records = [];
  let position = -1;
  const input = {
    pressKey: async key => {
      keys.push(key); position = (position + 1) % count;
      document.activeElement = focusAfterTabs === null ? (reachable ? controls[position] : null) : (keys.length === focusAfterTabs ? node : null);
    },
    observeExactTarget: async () => ({ observation: { center_in_viewport: true, center_in_scroll_clip: true, center_hit: true } }),
    snapshotProbe: async (after = 0) => ({ found: true, sequence: events.length, dropped_through: 0, events: events.filter(event => event.sequence > after) }),
    click: async (locator, options) => {
      assert.equal(node.disabled, false, "pointer input must wait until the target is enabled");
      clicks.push({ locator, options });
      for (const [type, buttons] of [["pointerdown", 1], ["pointerup", 0], ["click", 0]])
        events.push({ sequence: events.length + 1, type, isTrusted: trusted, ...target.identity, button: 0, buttons });
    },
  };
  return { input, cdp, target, keys, clicks, records, sink: { record: async (...args) => records.push(args) } };
}

test("Hub keyboard navigation reaches a late control in a long completed history", async () => {
  const f = fixture();
  await trustedFocus(f.input, f.cdp, f.target);
  assert.equal(f.keys.length, 131);
  assert.deepEqual(new Set(f.keys), new Set(["Tab"]));
});

test("Hub keyboard navigation stays bounded when the available control never receives focus", async () => {
  const f = fixture({ reachable: false });
  await assert.rejects(trustedFocus(f.input, f.cdp, f.target), error => error.code === "hub-browser-focus-unreachable");
  assert.ok(f.keys.length >= 140 && f.keys.length <= 144);
});

test("Hub keyboard navigation observes focus after its final allowed Tab", async () => {
  const f = fixture({ focusAfterTabs: 144 });
  await trustedFocus(f.input, f.cdp, f.target);
  assert.equal(f.keys.length, 144);
});

test("Hub clicks only after native keyboard focus reaches the exact target", async () => {
  const f = fixture();
  await trustedClick(f.input, f.cdp, f.target, f.sink);
  assert.equal(f.keys.length, 131);
  assert.equal(f.clicks.length, 1);
  assert.deepEqual(f.clicks[0], { locator: f.target, options: { stableHitSamples: 3 } });
  assert.equal(f.records[0][0], "hub-browser-desktop-trusted-click");
  assert.equal(f.records[0][1].probe.events.length, 3);
});

test("Hub focused clicks still reject untrusted pointer evidence", async () => {
  const f = fixture({ trusted: false });
  await assert.rejects(trustedClick(f.input, f.cdp, f.target, f.sink), error => error.code === "event-probe-untrusted");
  assert.equal(f.records.length, 0);
});

test("Hub clicks wait for a temporarily disabled control before focus and pointer input", async () => {
  const f = fixture({ disabledChecks: 2 });
  await trustedClick(f.input, f.cdp, f.target, f.sink);
  assert.equal(f.keys.length, 131);
  assert.equal(f.clicks.length, 1);
});

import assert from "node:assert/strict";
import test from "node:test";

import {
  classifySemanticTargetSettlement,
  waitForSemanticTargetSettlement,
} from "../core/semantic_target_settlement.mjs";

const LOCATOR = Object.freeze({
  selector: 'button[data-action="load-previous-turn-page"]',
  identity: { tag: "BUTTON", action: "load-previous-turn-page" },
});

function target(overrides = {}) {
  return {
    observation: {
      count: 1,
      identity: structuredClone(LOCATOR.identity),
      connected: true,
      visible: true,
      enabled: true,
      ...overrides,
    },
  };
}

test("semantic target settlement separates rerender absence from ambiguous ownership", () => {
  assert.deepEqual(classifySemanticTargetSettlement({ observation: { count: 0 } }, {
    expectedIdentity: LOCATOR.identity,
  }), { decision: "pending", failures: [] });
  assert.equal(classifySemanticTargetSettlement(target({ enabled: false }), {
    expectedIdentity: LOCATOR.identity,
  }).decision, "pending");
  assert.equal(classifySemanticTargetSettlement(target(), {
    expectedIdentity: LOCATOR.identity,
  }).decision, "pass");
  assert.match(classifySemanticTargetSettlement(target({ count: 2 }), {
    expectedIdentity: LOCATOR.identity,
  }).failures.join(","), /cardinality/);
  assert.match(classifySemanticTargetSettlement(target({
    identity: { tag: "BUTTON", action: "different" },
  }), { expectedIdentity: LOCATOR.identity }).failures.join(","), /identity/);
});

test("semantic target settlement waits through 0 and disabled observations before exact readiness", async () => {
  const observations = [
    { observation: { count: 0 } },
    target({ enabled: false }),
    target(),
  ];
  let calls = 0;
  const observed = await waitForSemanticTargetSettlement({
    input: {
      async observeExactTarget(locator) {
        assert.deepEqual(locator, LOCATOR);
        return observations[Math.min(calls++, observations.length - 1)];
      },
    },
    locator: LOCATOR,
    label: "semantic target self-test",
    timeoutMs: 1_000,
    pollMs: 1,
  });
  assert.equal(calls, 3);
  assert.equal(observed.value.classified.decision, "pass");
});

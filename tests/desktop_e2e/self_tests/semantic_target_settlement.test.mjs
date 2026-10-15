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

test("stable semantic readiness resets after absent, hidden, detached, or disabled observations", async () => {
  for (const interrupted of [{ count: 0 }, { visible: false }, { connected: false }, { enabled: false }]) {
    const observations = [target(), target(interrupted), target(), target(), target()];
    let calls = 0;
    const observed = await waitForSemanticTargetSettlement({
      input: { async observeExactTarget() { return observations[Math.min(calls++, observations.length - 1)]; } },
      locator: LOCATOR,
      label: "stable target after transient readiness",
      timeoutMs: 1_000,
      pollMs: 1,
      consecutiveReadySamples: 3,
    });
    assert.equal(calls, 5);
    assert.equal(observed.value.classified.decision, "pass");
    assert.equal(observed.value.readySamples, 3);
  }
});

test("ambiguous or different semantic ownership fails immediately during stable readiness", async () => {
  for (const invalid of [{ count: 2 }, { identity: { tag: "BUTTON", action: "different" } }]) {
    const observations = [target(), target(invalid), target()];
    let calls = 0;
    const observed = await waitForSemanticTargetSettlement({
      input: { async observeExactTarget() { return observations[Math.min(calls++, observations.length - 1)]; } },
      locator: LOCATOR,
      label: "ambiguous target during stable readiness",
      timeoutMs: 1_000,
      pollMs: 1,
      consecutiveReadySamples: 3,
    });
    assert.equal(calls, 2);
    assert.equal(observed.value.classified.decision, "fail");
  }
});

test("semantic readiness sample count is bounded and validated before observation", async () => {
  for (const consecutiveReadySamples of [0, -1, 1.5, 11, Infinity, NaN, "3", null]) {
    let calls = 0;
    await assert.rejects(waitForSemanticTargetSettlement({
      input: { async observeExactTarget() { calls++; return target(); } },
      locator: LOCATOR,
      label: "invalid stable sample count",
      consecutiveReadySamples,
    }), /consecutiveReadySamples/);
    assert.equal(calls, 0);
  }
});

import assert from "node:assert/strict";
import test from "node:test";

import { executeDesktopScenario } from "../core/desktop_execution.mjs";
import { DesktopE2eError } from "../core/execution.mjs";

function clock() {
  let milliseconds = Date.parse("2026-08-22T00:00:00.000Z");
  return () => new Date(milliseconds++).toISOString();
}

function fakeSink() {
  const state = { records: [], seals: [] };
  return {
    state,
    async record(kind, payload, meta) { state.records.push({ kind, payload, meta }); },
    async seal(result) {
      state.seals.push(structuredClone(result));
      return { schema_version: "desktop-e2e.seal.v1", event_count: state.records.length };
    },
  };
}

function faultCase(point) {
  const calls = { preflight: 0, prepare: 0, launch: 0, attach: 0, execute: 0, cleanup: 0, scenarioQuiesce: 0, scenarioCleanup: 0 };
  const fail = (name, owner = "harness", code = `${name}-fault`) => {
    if (point === name) throw new DesktopE2eError(owner, code, `${name} injected fault`);
  };
  const scenario = {
    id: "fault.matrix",
    productOracle: "pass",
    manualGate: "not_required",
    async prepare() { calls.prepare += 1; fail("prepare"); },
    async execute() {
      calls.execute += 1;
      if (point === "ambiguous-action") throw new DesktopE2eError("harness", "ambiguous-action", "delivery was ambiguous");
      if (point === "product-assertion") throw new DesktopE2eError("product", "product-assertion", "acquired predicate failed");
      return { acquisition: "pass", oracle: "pass", manual: "not_required" };
    },
    async requestGracefulExit() { return { requested: true, reason: null }; },
    async quiesce() {
      calls.scenarioQuiesce += 1;
      if (point === "scenario-quiesce") throw new Error("scenario quiesce injected fault");
      return {
        input: "pass",
        resources: [],
        productFailure: point === "scenario-late-product"
          ? { code: "late-replay", message: "provider replayed after the terminal", evidence: { requests: 3 } }
          : null,
      };
    },
    async cleanup() {
      calls.scenarioCleanup += 1;
      if (point === "scenario-cleanup") throw new Error("scenario cleanup injected fault");
      return { input: "pass", resources: [] };
    },
  };
  const host = {
    async preflight() {
      calls.preflight += 1;
      if (point === "preflight-blocked") throw new DesktopE2eError("environment", "host-busy", "host is busy");
    },
    async launch() { calls.launch += 1; fail("startup"); return { process_id: 1 }; },
    async attach() { calls.attach += 1; fail("attach"); return { close() {} }; },
    async cleanup({ releaseScenarioResources }) {
      calls.cleanup += 1;
      await releaseScenarioResources();
      return {
        gracefulExit: { requested: true, reason: null },
        cleanup: {
          admission_released: true,
          desktop_exited: true,
          profile_webviews_remaining: 0,
          sqlite: { pass: true },
          forced_desktop: false,
          forced_profile_process_ids: [],
        },
        input: point === "cleanup" ? "fail" : "pass",
      };
    },
  };
  return { calls, scenario, host };
}

for (const [point, expected, productFailure] of [
  ["preflight-blocked", "environment_blocked", false],
  ["startup", "harness_ng", false],
  ["attach", "harness_ng", false],
  ["ambiguous-action", "harness_ng", false],
  ["product-assertion", "product_fail", true],
  ["cleanup", "harness_ng", false],
  ["scenario-quiesce", "harness_ng", false],
  ["scenario-late-product", "product_fail", true],
  ["scenario-cleanup", "harness_ng", false],
]) {
  test(`execution-level fault matrix: ${point}`, async () => {
    const injected = faultCase(point);
    const sink = fakeSink();
    const outcome = await executeDesktopScenario({
      context: { executionId: `e2e-20260822-${point}`, root: "C:\\fixture" },
      scenario: injected.scenario,
      host: injected.host,
      sink,
      now: clock(),
    });
    assert.equal(outcome.result.classification, expected);
    assert.equal(outcome.result.product_failure, productFailure);
    assert.equal(injected.calls.cleanup, 1, "cleanup must run exactly once");
    assert.equal(injected.calls.scenarioQuiesce, 1, "scenario quiesce must run exactly once");
    assert.equal(injected.calls.scenarioCleanup, 1, "scenario cleanup must run exactly once");
    assert.equal(sink.state.seals.length, 1, "result must seal exactly once");
    assert.equal(outcome.result.lifecycle.at(-1).phase, "sealed");
  });
}

test("host cleanup releases scenario resources after process zero and before storage audit", async () => {
  const order = [];
  const injected = faultCase("none");
  injected.scenario.quiesce = async () => {
    injected.calls.scenarioQuiesce += 1;
    order.push("scenario-resource-release");
    return { input: "pass", resources: [{ kind: "loopback-provider", closed: true }] };
  };
  injected.host.cleanup = async ({ releaseScenarioResources }) => {
    injected.calls.cleanup += 1;
    order.push("desktop-profile-zero");
    await releaseScenarioResources();
    order.push("sqlite-audit-admission-release");
    return {
      gracefulExit: { requested: true, reason: null },
      cleanup: {
        admission_released: true,
        desktop_exited: true,
        profile_webviews_remaining: 0,
        sqlite: { pass: true },
        forced_desktop: false,
        forced_profile_process_ids: [],
      },
      input: "pass",
    };
  };
  const outcome = await executeDesktopScenario({
    context: { executionId: "e2e-20260822-cleanup-order", root: "C:\\fixture" },
    scenario: injected.scenario,
    host: injected.host,
    sink: fakeSink(),
    now: clock(),
  });
  assert.equal(outcome.result.classification, "pass");
  assert.deepEqual(order, ["desktop-profile-zero", "scenario-resource-release", "sqlite-audit-admission-release"]);
  assert.equal(outcome.result.cleanup.scenario_quiesce_resources[0].closed, true);
});

test("a host that skips the ordered scenario release is harness NG but fallback releases it once", async () => {
  const injected = faultCase("none");
  injected.host.cleanup = async () => {
    injected.calls.cleanup += 1;
    return {
      gracefulExit: { requested: true, reason: null },
      cleanup: {
        admission_released: true,
        desktop_exited: true,
        profile_webviews_remaining: 0,
        sqlite: { pass: true },
        forced_desktop: false,
        forced_profile_process_ids: [],
      },
      input: "pass",
    };
  };
  const outcome = await executeDesktopScenario({
    context: { executionId: "e2e-20260822-cleanup-order-missing", root: "C:\\fixture" },
    scenario: injected.scenario,
    host: injected.host,
    sink: fakeSink(),
    now: clock(),
  });
  assert.equal(outcome.result.classification, "harness_ng");
  assert.equal(injected.calls.scenarioQuiesce, 1);
  assert.equal(outcome.result.diagnostics.some((row) => row.code === "scenario-quiesce-order-not-acquired"), true);
});

test("a host cannot release the same scenario resources twice", async () => {
  const injected = faultCase("none");
  injected.host.cleanup = async ({ releaseScenarioResources }) => {
    injected.calls.cleanup += 1;
    await releaseScenarioResources();
    await releaseScenarioResources();
    return {
      gracefulExit: { requested: true, reason: null },
      cleanup: {
        admission_released: true,
        desktop_exited: true,
        profile_webviews_remaining: 0,
        sqlite: { pass: true },
        forced_desktop: false,
        forced_profile_process_ids: [],
      },
      input: "pass",
    };
  };
  const outcome = await executeDesktopScenario({
    context: { executionId: "e2e-20260822-cleanup-repeat", root: "C:\\fixture" },
    scenario: injected.scenario,
    host: injected.host,
    sink: fakeSink(),
    now: clock(),
  });
  assert.equal(outcome.result.classification, "harness_ng");
  assert.equal(injected.calls.scenarioQuiesce, 1);
  assert.equal(outcome.result.diagnostics.some((row) => row.code === "scenario-quiesce-repeated"), true);
});

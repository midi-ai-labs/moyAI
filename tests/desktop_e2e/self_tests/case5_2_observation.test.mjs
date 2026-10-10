import assert from "node:assert/strict";
import test from "node:test";
import { ManualObservationBudget } from "../core/deadline.mjs";
import { normalizeCase52Options, case52FixtureConfig, settleCase52ObservationTimeout } from "../scenarios/case5_2.mjs";

const options = {
  fixture_source: "C:/fixture/RippleFish", provider_base_url: "http://127.0.0.1:8119/v1",
  provider_profile: "openai_compatible", provider_lifecycle: "external-unmanaged", main_model: "main",
};

test("Case5_2 accepts an optional case observation ceiling and preserves product timeout and omitted defaults", () => {
  const omitted = normalizeCase52Options(options);
  const bounded = normalizeCase52Options({ ...options, observation_timeout_ms: 7_200_000 });
  assert.equal(Object.hasOwn(omitted, "observationTimeoutMs"), false);
  assert.equal(bounded.observationTimeoutMs, 7_200_000);
  assert.equal(case52FixtureConfig(bounded), case52FixtureConfig(omitted));
  for (const value of [0, -1, 7_200_001, 1.5, "7200000", null, Number.MAX_SAFE_INTEGER + 1]) {
    assert.throws(() => normalizeCase52Options({ ...options, observation_timeout_ms: value }), TypeError);
  }
});

const mainTarget = { workspacePath: "C:/fixture/RippleFish", sessionId: "session", turnId: "turn", admissionRevision: "7", kind: "turn" };
const mainIdle = { workspacePath: mainTarget.workspacePath, sessionId: mainTarget.sessionId,
  expectedState: { kind: "idle", latestTurnId: mainTarget.turnId, admissionRevision: mainTarget.admissionRevision } };
const sideTarget = { owner_session_id: "session", chat_id: "side", generation: "9007199254740993", status: "running", can_cancel: true };

function execution({ main = false, side = false, duplicate = false, root = false, unbound = false } = {}) {
  const stopTarget = root ? { kind: "root", workspacePath: mainTarget.workspacePath,
    sessionId: unbound ? null : mainTarget.sessionId, latestTurnId: unbound ? null : mainTarget.turnId,
    admissionRevision: mainTarget.admissionRevision, rootGeneration: "8", permissionConfirmationId: null } : mainTarget;
  const idleTarget = unbound ? { ...mainIdle, sessionId: null,
    expectedState: { ...mainIdle.expectedState, latestTurnId: null } } : mainIdle;
  const before = { busy: main, agent_tree_active: main, task_activity_state: main ? "running" : "idle",
    run_status_key: main ? "running" : "completed", stop_target: main ? stopTarget : null,
    run_target: main && !root ? { ...mainIdle, expectedState: { kind: "turn", turnId: "turn", admissionRevision: "7" } } : idleTarget,
    side_chat: side ? sideTarget : { ...sideTarget, status: "idle", can_cancel: false } };
  const terminal = { ...before, busy: false, agent_tree_active: false, task_activity_state: "idle",
    run_status_key: root ? "idle" : main ? "cancelled" : "completed", run_target: idleTarget, stop_target: null,
    post_run_refresh_pending: false, background_mutation_pending: false, async_polling_required: false,
    pending_async_operations: [], side_chat: { ...before.side_chat, status: side ? "cancelled" : "idle", can_cancel: false } };
  const calls = [], clicks = [], records = [], waits = [];
  let removed = false, stopped = false;
  const probe = {
    async install() {},
    async snapshot(after = 0) { return { found: true, sequence: calls.length, dropped_through: 0,
      calls: calls.filter(row => row.sequence > after) }; },
    async remove() { removed = true; },
  };
  let now = 10;
  const budget = new ManualObservationBudget({ timeoutMs: 1, now: () => now });
  budget.beginOnce();
  now = 11;
  assert.equal(budget.expired, true);
  const run = () => settleCase52ObservationTimeout({ cdp: {}, input: {}, observationBudget: budget,
    sink: { async record(name, value) { records.push({ name, value }); } } }, {
    observe: async () => stopped ? terminal : before,
    createProbe: () => probe,
    click: async (locator) => {
      clicks.push(locator.identity.action);
      const command = locator.identity.action === "cancel-run" ? { command: "cancel_run", args: { expectedTarget: stopTarget } }
        : { command: "cancel_side_chat", args: { ownerSessionId: "session", chatId: "side", expectedGeneration: "9007199254740993" } };
      calls.push({ sequence: calls.length + 1, ...command });
      if (duplicate) calls.push({ sequence: calls.length + 1, ...command });
      stopped = true;
      return {};
    },
    wait: async ({ sample, accept, timeoutMs }) => {
      waits.push(timeoutMs);
      const value = await sample();
      assert.equal(accept(value), true);
      return { value };
    },
  });
  return { run, clicks, calls, records, waits, removed: () => removed };
}

test("Case5_2 observation timeout sends one exact Main Stop and settles its terminal owner", async () => {
  const current = execution({ main: true });
  const evidence = await current.run();
  assert.deepEqual(current.clicks, ["cancel-run"]);
  assert.deepEqual(current.calls[0].args.expectedTarget, mainTarget);
  assert.deepEqual(current.waits, [120_000, 10_000]);
  assert.equal(evidence.terminal.run_status_key, "cancelled");
  assert.equal(evidence.observation_budget.remaining_ms, 0);
  assert.equal(current.removed(), true);
});

test("Case5_2 observation timeout cancels only the active Side owner and retains its exact generation", async () => {
  const current = execution({ side: true });
  const evidence = await current.run();
  assert.deepEqual(current.clicks, ["cancel-side-chat"]);
  assert.equal(current.calls[0].args.expectedGeneration, "9007199254740993");
  assert.deepEqual(evidence.terminal.run_target, mainIdle);
  assert.equal(evidence.terminal.side_status, "cancelled");
  assert.equal(current.removed(), true);
});

test("Case5_2 timeout settles a Root owner before admission and after a completed turn", async () => {
  for (const unbound of [false, true]) {
    const current = execution({ main: true, root: true, unbound });
    const evidence = await current.run();
    assert.deepEqual(current.clicks, ["cancel-run"]);
    assert.equal(current.calls[0].args.expectedTarget.kind, "root");
    assert.equal(current.calls[0].args.expectedTarget.rootGeneration, "8");
    assert.equal(evidence.terminal.run_status_key, "idle");
    assert.deepEqual(evidence.terminal.run_target, unbound ? { ...mainIdle, sessionId: null,
      expectedState: { ...mainIdle.expectedState, latestTurnId: null } } : mainIdle);
    assert.equal(evidence.observation_budget.remaining_ms, 0);
    assert.equal(current.removed(), true);
  }
});

test("Case5_2 observation timeout emits no Stop after the runtime has already settled", async () => {
  const current = execution();
  const evidence = await current.run();
  assert.deepEqual(current.clicks, []);
  assert.equal(evidence.commands.calls.length, 0);
  assert.equal(current.records.length, 1);
  assert.equal(current.removed(), true);
});

test("Case5_2 timeout command mismatch still removes its probe and does not claim successful settlement", async () => {
  const current = execution({ main: true, duplicate: true });
  await assert.rejects(current.run, error => error.code === "desktop-command-probe-cardinality");
  assert.equal(current.records.length, 0);
  assert.equal(current.removed(), true);
});

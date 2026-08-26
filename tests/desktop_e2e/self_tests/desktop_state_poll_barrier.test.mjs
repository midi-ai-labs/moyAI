import assert from "node:assert/strict";
import test from "node:test";

import {
  DesktopStatePollBarrier,
} from "../drivers/desktop_state_poll_barrier.mjs";

const BARRIER_ID = "run-next-turn-terminal";
const PENDING = Object.freeze({
  projection_revision: "11",
  post_run_refresh_pending: true,
  composer_submit_mode: "blocked",
  can_submit: false,
  run_target: { expectedState: { kind: "idle", admissionRevision: "0" } },
});
const SETTLED = Object.freeze({
  projection_revision: "12",
  post_run_refresh_pending: false,
  composer_submit_mode: "new_request",
  can_submit: true,
  run_target: { expectedState: { kind: "idle", admissionRevision: "1" } },
});

class FakeCdp {
  constructor(results) {
    this.results = [...results];
    this.expressions = [];
  }

  async evaluate(expression) {
    this.expressions.push(expression);
    assert.ok(this.results.length > 0, "unexpected evaluate call");
    return structuredClone(this.results.shift());
  }
}

test("poll barrier owns one Tauri IPC fetch wrapper and exercises the pending-to-fresh lifecycle", async () => {
  const cdp = new FakeCdp([
    { installed: true, barrier_id: BARRIER_ID, phase: "idle" },
    { armed: true, barrier_id: BARRIER_ID, phase: "armed" },
    {
      found: true,
      barrier_id: BARRIER_ID,
      phase: "waiting",
      intercepted_count: 1,
      bypass_count: 0,
      captured: null,
    },
    {
      captured: true,
      barrier_id: BARRIER_ID,
      phase: "captured",
      projection: PENDING,
      bypass_count: 1,
    },
    { sampled: true, barrier_id: BARRIER_ID, projection: SETTLED, bypass_count: 2 },
    { released: true, barrier_id: BARRIER_ID, phase: "armed", projection_revision: "11" },
    { resumed: true, barrier_id: BARRIER_ID, phase: "idle", delivered: true, projection_revision: "12" },
    {
      removed: true,
      barrier_id: BARRIER_ID,
      settled_waiter: false,
      intercepted_count: 2,
      bypass_count: 3,
    },
  ]);
  const barrier = new DesktopStatePollBarrier(cdp, { barrierId: BARRIER_ID });

  await barrier.install();
  await barrier.arm();
  assert.equal((await barrier.snapshot()).phase, "waiting");
  assert.deepEqual((await barrier.capturePending()).projection, PENDING);
  assert.deepEqual(await barrier.sampleBackend(), SETTLED);
  assert.equal((await barrier.releaseCaptured({ rearm: true })).phase, "armed");
  assert.equal((await barrier.resumeFresh()).delivered, true);
  await barrier.remove();

  assert.equal(barrier.installed, false);
  assert.match(cdp.expressions[0], /window\.fetch = wrapped/);
  assert.match(cdp.expressions[0], /url\.hostname === 'ipc\.localhost'/);
  assert.match(cdp.expressions[0], /=== 'desktop_state'/);
  assert.match(cdp.expressions[3], /post_run_refresh_pending === true/);
  assert.match(cdp.expressions[3], /state\.captured = structuredClone\(lastProjection\)/);
  assert.match(cdp.expressions[5], /new Response\(JSON\.stringify\(projection\)/);
  assert.match(cdp.expressions[7], /window\.fetch = state\.originalFetch/);
});

test("poll barrier rejects invalid ownership and capture bounds before touching CDP", async () => {
  assert.throws(() => new DesktopStatePollBarrier({}, { barrierId: BARRIER_ID }), /CDP client is required/);
  assert.throws(
    () => new DesktopStatePollBarrier({ evaluate() {} }, { barrierId: "bad id" }),
    /invalid Desktop state poll barrier id/,
  );

  const cdp = new FakeCdp([{ installed: true, barrier_id: BARRIER_ID, phase: "idle" }]);
  const barrier = new DesktopStatePollBarrier(cdp, { barrierId: BARRIER_ID });
  await assert.rejects(() => barrier.arm(), (error) => error.code === "desktop-state-poll-barrier-not-owned");
  await barrier.install();
  await assert.rejects(
    () => barrier.capturePending({ timeoutMs: 99 }),
    /pending capture timeout is invalid/,
  );
  await assert.rejects(
    () => barrier.capturePending({ pollMs: 0 }),
    /pending capture poll interval is invalid/,
  );
  assert.equal(cdp.expressions.length, 1);
});

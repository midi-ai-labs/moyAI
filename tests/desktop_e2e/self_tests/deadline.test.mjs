import assert from "node:assert/strict";
import test from "node:test";

import { waitForObservation } from "../core/deadline.mjs";

test("bounded readiness waits for the accepted observation without mutation retry", async () => {
  let time = 0;
  let sampleCount = 0;
  const result = await waitForObservation({
    label: "ready",
    timeoutMs: 100,
    pollMs: 10,
    now: () => time,
    sleep: async (milliseconds) => { time += milliseconds; },
    sample: async () => ({ ready: ++sampleCount === 3 }),
    accept: (value) => value.ready,
  });
  assert.equal(result.attempts, 3);
  assert.equal(result.elapsed_ms, 20);
});

test("bounded readiness returns typed timeout evidence and can reject sampling errors immediately", async () => {
  let time = 0;
  await assert.rejects(
    () => waitForObservation({
      label: "never-ready",
      timeoutMs: 30,
      pollMs: 10,
      now: () => time,
      sleep: async (milliseconds) => { time += milliseconds; },
      sample: async () => ({ ready: false }),
      accept: (value) => value.ready,
    }),
    (error) => error.code === "observation-timeout" && error.evidence.attempts === 3,
  );

  await assert.rejects(
    () => waitForObservation({
      label: "fatal-sample",
      timeoutMs: 30,
      now: () => 0,
      sleep: async () => {},
      sample: async () => { throw new Error("fatal"); },
      accept: () => false,
      retrySampleErrors: false,
    }),
    /fatal/,
  );
});

test("a delayed sample cannot complete after the observation deadline and become a pass", async () => {
  const started = Date.now();
  await assert.rejects(
    () => waitForObservation({
      label: "delayed-ready",
      timeoutMs: 25,
      pollMs: 5,
      sample: () => new Promise((resolve) => setTimeout(() => resolve({ ready: true }), 100)),
      accept: (value) => value.ready,
    }),
    (error) => error.code === "observation-timeout"
      && error.evidence.attempts === 1
      && /remaining deadline/.test(error.evidence.last_error),
  );
  assert.ok(Date.now() - started < 80, "the wait must stop at its own deadline");
});

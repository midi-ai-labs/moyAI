import test from "node:test";
import assert from "node:assert/strict";

import { case52InitialReadinessPerformance } from "../scenarios/case5_2.mjs";

const appStarted = "2026-10-12T00:00:00.000Z";
const caseStarted = "2026-10-12T00:10:00.000Z";
const ready = Date.parse("2026-10-12T00:10:02.000Z");

test("single execution preserves the actual cold-launch metric and schema", () => {
  assert.deepEqual(case52InitialReadinessPerformance({}, { launch_started_at: appStarted }, ready), {
    launch_to_ready_ms: 602_000,
  });
});

test("a shared Desktop measures case entry without attributing earlier cases to launch", () => {
  assert.deepEqual(case52InitialReadinessPerformance({ manualSuite: {
    case_index: 5, started_at: caseStarted, shared_profile: true,
  } }, { launch_started_at: appStarted }, ready), {
    launch_to_ready_ms: null,
    case_entry_to_ready_ms: 2_000,
    comparability_deviations: ["shared-profile/continuous-desktop"],
  });
});

test("non-shared metadata does not alter a standalone comparison", () => {
  assert.deepEqual(case52InitialReadinessPerformance({ manualSuite: {
    started_at: caseStarted, shared_profile: false,
  } }, { launch_started_at: appStarted }, ready), { launch_to_ready_ms: 602_000 });
});

test("missing shared case time remains unknown instead of using the old app start", () => {
  const evidence = case52InitialReadinessPerformance({ manualSuite: {
    shared_profile: true,
  } }, { launch_started_at: appStarted }, ready);
  assert.equal(evidence.launch_to_ready_ms, null);
  assert.equal(evidence.case_entry_to_ready_ms, null);
  assert.deepEqual(evidence.comparability_deviations, ["shared-profile/continuous-desktop"]);
});

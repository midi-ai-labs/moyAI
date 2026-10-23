import assert from "node:assert/strict";
import test from "node:test";

import { startupReportDecision } from "../scenarios/shell_startup_report.mjs";

test("a startup acknowledgement requires the same live process and a rendered initial screen", () => {
  const ready = { schema: 1, status: "ready", process_id: 42, exit_code: 0, message: "ready" };
  assert.deepEqual(startupReportDecision(null, 42, true), { accepted: false, failure: null });
  assert.equal(startupReportDecision(ready, 42, true).accepted, true);
  assert.equal(startupReportDecision(ready, 42, false).failure, "startup-reported-ready-before-render");
  assert.equal(startupReportDecision(ready, 43, true).failure, "startup-report-process-mismatch");
  assert.equal(startupReportDecision({ ...ready, status: "error", exit_code: 1 }, 42, true).failure, "startup-reported-error");
  for (const report of [{}, { ...ready, schema: 2 }, { ...ready, status: "starting" }, { ...ready, exit_code: 1 }, { ...ready, message: null }]) {
    assert.equal(startupReportDecision(report, 42, true).failure, "startup-report-invalid");
  }
});

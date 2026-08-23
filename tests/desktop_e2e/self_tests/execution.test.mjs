import assert from "node:assert/strict";
import test from "node:test";

import { ExecutionLifecycle, classifyExecution, exactCleanupPassed } from "../core/execution.mjs";

test("execution lifecycle has one legal forward path and a cleanup escape", () => {
  let tick = 0;
  const lifecycle = new ExecutionLifecycle("e2e-20260822-foundation", "shell.baseline", () => `t${++tick}`);
  for (const phase of ["preflight", "prepared", "launching", "attached", "executing", "classifying", "cleaning", "sealed"]) {
    assert.equal(lifecycle.transition(phase), phase);
  }
  assert.equal(lifecycle.history.length, 9);
  assert.throws(() => lifecycle.transition("cleaning"), /invalid execution transition/);

  const failedLaunch = new ExecutionLifecycle("e2e-20260822-launch-fail", "shell.baseline");
  failedLaunch.transition("preflight");
  failedLaunch.transition("prepared");
  failedLaunch.transition("launching");
  failedLaunch.transition("cleaning", { owner: "desktop-launch" });
  failedLaunch.transition("sealed");
  assert.deepEqual(failedLaunch.history.map((row) => row.phase), ["created", "preflight", "prepared", "launching", "cleaning", "sealed"]);
});

test("verdict separates product, harness, environment, manual, and cleanup owners", () => {
  const pass = classifyExecution({ preflight: "pass", acquisition: "pass", oracle: "pass", manual: "not_required", cleanup: "pass" });
  assert.equal(pass.classification, "pass");
  assert.equal(pass.product_failure, false);

  const product = classifyExecution({ preflight: "pass", acquisition: "pass", oracle: "fail", manual: "not_required", cleanup: "pass" });
  assert.equal(product.classification, "product_fail");
  assert.equal(product.product_failure, true);
  assert.equal(product.harness_failure, false);

  const acquisition = classifyExecution({ preflight: "pass", acquisition: "fail", oracle: "not_run", manual: "not_run", cleanup: "pass" });
  assert.equal(acquisition.classification, "harness_ng");
  assert.equal(acquisition.product_failure, false);

  const blocked = classifyExecution({ preflight: "blocked", acquisition: "not_run", oracle: "not_run", manual: "not_run", cleanup: "pass" });
  assert.equal(blocked.classification, "environment_blocked");

  const pending = classifyExecution({ preflight: "pass", acquisition: "pass", oracle: "pass", manual: "pending", cleanup: "pass" });
  assert.equal(pending.classification, "manual_pending");

  const dirtyProduct = classifyExecution({ preflight: "pass", acquisition: "pass", oracle: "fail", manual: "not_required", cleanup: "fail" });
  assert.equal(dirtyProduct.classification, "harness_ng");
  assert.equal(dirtyProduct.product_failure, true, "cleanup must not erase an already acquired product failure");
});

test("qualification scenarios can explicitly declare no product oracle", () => {
  const result = classifyExecution({ preflight: "pass", acquisition: "pass", oracle: "not_required", manual: "not_required", cleanup: "pass" });
  assert.equal(result.classification, "pass");
  const missingRequiredOracle = classifyExecution({
    preflight: "pass",
    acquisition: "pass",
    oracle: "not_run",
    manual: "not_required",
    cleanup: "pass",
  });
  assert.equal(missingRequiredOracle.classification, "harness_ng");
  assert.match(missingRequiredOracle.reasons.join(","), /oracle was required/);
});

test("cleanup preserves a preflight block and requires graceful zero-state after acquisition", () => {
  const zero = {
    admission_released: true,
    desktop_exited: true,
    profile_webviews_remaining: 0,
    sqlite: { pass: true },
    forced_desktop: false,
    forced_profile_process_ids: [],
  };
  assert.equal(exactCleanupPassed({ acquisition: "not_run", gracefulExit: { requested: false }, cleanup: zero }), true);
  assert.equal(exactCleanupPassed({ acquisition: "fail", gracefulExit: { requested: false }, cleanup: { ...zero, forced_desktop: true } }), true);
  assert.equal(exactCleanupPassed({ acquisition: "pass", gracefulExit: { requested: true }, cleanup: zero }), true);
  assert.equal(exactCleanupPassed({ acquisition: "pass", gracefulExit: { requested: false }, cleanup: zero }), false);
  assert.equal(exactCleanupPassed({ acquisition: "pass", gracefulExit: { requested: true }, cleanup: { ...zero, forced_desktop: true } }), false);
  assert.equal(exactCleanupPassed({ acquisition: "pass", gracefulExit: { requested: true }, cleanup: { ...zero, profile_webviews_remaining: 1 } }), false);
});

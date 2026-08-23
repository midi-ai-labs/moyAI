const PHASE_TRANSITIONS = Object.freeze({
  created: new Set(["preflight"]),
  preflight: new Set(["prepared", "cleaning"]),
  prepared: new Set(["launching", "cleaning"]),
  launching: new Set(["attached", "cleaning"]),
  attached: new Set(["executing", "cleaning"]),
  executing: new Set(["classifying", "cleaning"]),
  classifying: new Set(["cleaning"]),
  cleaning: new Set(["sealed"]),
  sealed: new Set(),
});

export const EXECUTION_PHASES = Object.freeze(Object.keys(PHASE_TRANSITIONS));
export const EXECUTION_CLASSIFICATIONS = Object.freeze([
  "pass",
  "product_fail",
  "harness_ng",
  "environment_blocked",
  "manual_pending",
]);

const EXECUTION_ID = /^e2e-[a-z0-9][a-z0-9._-]{7,127}$/;
const SCENARIO_ID = /^[a-z0-9][a-z0-9._-]{2,127}$/;

function invariant(condition, message) {
  if (!condition) throw new TypeError(message);
}

export function assertExecutionIdentity(executionId, scenarioId) {
  invariant(EXECUTION_ID.test(executionId), `invalid execution id: ${executionId}`);
  invariant(SCENARIO_ID.test(scenarioId), `invalid scenario id: ${scenarioId}`);
}

export class ExecutionLifecycle {
  #phase = "created";
  #history;

  constructor(executionId, scenarioId, now = () => new Date().toISOString()) {
    assertExecutionIdentity(executionId, scenarioId);
    invariant(typeof now === "function", "now must be a function");
    this.executionId = executionId;
    this.scenarioId = scenarioId;
    this.now = now;
    this.#history = [{ phase: "created", at: now(), detail: null }];
  }

  get phase() {
    return this.#phase;
  }

  get history() {
    return this.#history.map((entry) => structuredClone(entry));
  }

  transition(next, detail = null) {
    invariant(EXECUTION_PHASES.includes(next), `unknown execution phase: ${next}`);
    invariant(PHASE_TRANSITIONS[this.#phase].has(next), `invalid execution transition: ${this.#phase} -> ${next}`);
    invariant(detail === null || (typeof detail === "object" && !Array.isArray(detail)), "transition detail must be an object or null");
    this.#phase = next;
    this.#history.push({ phase: next, at: this.now(), detail: detail === null ? null : structuredClone(detail) });
    return this.phase;
  }
}

const ALLOWED = Object.freeze({
  preflight: new Set(["pass", "fail", "blocked"]),
  acquisition: new Set(["pass", "fail", "not_run"]),
  oracle: new Set(["pass", "fail", "not_run", "not_required"]),
  manual: new Set(["pass", "fail", "pending", "not_required", "not_run"]),
  cleanup: new Set(["pass", "fail"]),
});

function normalizedInputs(value) {
  const inputs = {
    preflight: value?.preflight,
    acquisition: value?.acquisition,
    oracle: value?.oracle,
    manual: value?.manual,
    cleanup: value?.cleanup,
  };
  for (const [name, allowed] of Object.entries(ALLOWED)) {
    invariant(allowed.has(inputs[name]), `invalid ${name} result: ${inputs[name]}`);
  }
  return inputs;
}

export function classifyExecution(value) {
  const inputs = normalizedInputs(value);
  const reasons = [];
  let classification;

  if (inputs.cleanup === "fail") {
    classification = "harness_ng";
    reasons.push("exact cleanup did not settle cleanly");
  } else if (inputs.preflight === "blocked") {
    classification = "environment_blocked";
    reasons.push("host preflight blocked execution before product acquisition");
  } else if (inputs.preflight === "fail") {
    classification = "harness_ng";
    reasons.push("harness preflight failed");
  } else if (inputs.acquisition === "fail") {
    classification = "harness_ng";
    reasons.push("required action or observation was not acquired");
  } else if (inputs.acquisition === "not_run") {
    classification = "harness_ng";
    reasons.push("execution ended without required acquisition");
  } else if (inputs.oracle === "fail" || inputs.manual === "fail") {
    classification = "product_fail";
    reasons.push(inputs.oracle === "fail" ? "acquired product predicate failed" : "manual product predicate failed");
  } else if (inputs.oracle === "not_run") {
    classification = "harness_ng";
    reasons.push("product oracle was required but not evaluated");
  } else if (inputs.manual === "pending" || inputs.manual === "not_run") {
    classification = "manual_pending";
    reasons.push("machine acquisition completed but required manual verdict is pending");
  } else {
    classification = "pass";
    reasons.push("all required acquisition, product oracle, manual gate, and cleanup checks passed");
  }

  const productFailure = inputs.oracle === "fail" || inputs.manual === "fail";
  return {
    schema_version: "desktop-e2e.execution-result.v1",
    classification,
    product_failure: productFailure,
    harness_failure: classification === "harness_ng",
    environment_blocked: classification === "environment_blocked",
    manual_pending: classification === "manual_pending",
    inputs,
    reasons,
  };
}

export function exactCleanupPassed({ acquisition, gracefulExit, cleanup }) {
  invariant(ALLOWED.acquisition.has(acquisition), `invalid acquisition result for cleanup: ${acquisition}`);
  invariant(gracefulExit !== null && typeof gracefulExit === "object", "graceful exit result is required");
  invariant(cleanup !== null && typeof cleanup === "object", "cleanup result is required");
  const stoppedExactly = cleanup.desktop_exited === true
    && cleanup.profile_webviews_remaining === 0
    && cleanup.sqlite?.pass === true
    && cleanup.admission_released === true;
  if (!stoppedExactly) return false;
  if (acquisition !== "pass") return true;
  return gracefulExit.requested === true
    && cleanup.forced_desktop === false
    && Array.isArray(cleanup.forced_profile_process_ids)
    && cleanup.forced_profile_process_ids.length === 0;
}

export class DesktopE2eError extends Error {
  constructor(owner, code, message, evidence = null) {
    super(message);
    invariant(["harness", "environment", "product"].includes(owner), `invalid error owner: ${owner}`);
    invariant(typeof code === "string" && /^[a-z0-9][a-z0-9._-]+$/.test(code), `invalid error code: ${code}`);
    this.name = "DesktopE2eError";
    this.owner = owner;
    this.code = code;
    this.evidence = evidence === null ? null : structuredClone(evidence);
  }
}

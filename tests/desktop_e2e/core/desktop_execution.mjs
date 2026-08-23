import { DesktopE2eError, ExecutionLifecycle, classifyExecution } from "./execution.mjs";

function assertContract(value, name, methods) {
  if (value === null || typeof value !== "object") throw new TypeError(`${name} is required`);
  for (const method of methods) {
    if (typeof value[method] !== "function") throw new TypeError(`${name}.${method} must be a function`);
  }
}

function diagnostic(error) {
  return {
    owner: error instanceof DesktopE2eError ? error.owner : "harness",
    code: error?.code ?? "unclassified-error",
    message: error?.message ?? String(error),
    evidence: error?.evidence ?? null,
  };
}

function assertScenarioOutcome(outcome) {
  if (outcome === null || typeof outcome !== "object") throw new DesktopE2eError("harness", "scenario-outcome-invalid", "scenario outcome must be an object");
  if (!new Set(["pass", "fail", "not_run"]).has(outcome.acquisition ?? "pass")) {
    throw new DesktopE2eError("harness", "scenario-outcome-invalid", "scenario acquisition outcome is invalid");
  }
  if (!new Set(["pass", "fail", "not_run", "not_required"]).has(outcome.oracle)) {
    throw new DesktopE2eError("harness", "scenario-outcome-invalid", "scenario oracle outcome is invalid");
  }
  if (!new Set(["pass", "fail", "pending", "not_required", "not_run"]).has(outcome.manual)) {
    throw new DesktopE2eError("harness", "scenario-outcome-invalid", "scenario manual outcome is invalid");
  }
}

export async function executeDesktopScenario({ context, scenario, host, sink, now = () => new Date().toISOString() }) {
  assertContract(context, "context", []);
  assertContract(scenario, "scenario", ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]);
  assertContract(host, "host", ["preflight", "launch", "attach", "cleanup"]);
  assertContract(sink, "sink", ["record", "seal"]);
  const startedAt = now();
  const startedMs = Date.parse(startedAt);
  const lifecycle = new ExecutionLifecycle(context.executionId, scenario.id, now);
  const inputs = { preflight: "fail", acquisition: "not_run", oracle: "not_run", manual: "not_run", cleanup: "fail" };
  const diagnostics = [];
  let runtime = null;
  let driver = null;
  let gracefulExit = { requested: false, reason: "not-attached" };
  let cleanup = {
    admission_released: false,
    desktop_exited: false,
    profile_webviews_remaining: null,
    sqlite: null,
    forced_desktop: false,
    forced_profile_process_ids: [],
    scenario_quiesce_resources: [],
    scenario_resources: [],
  };

  try {
    lifecycle.transition("preflight");
    await host.preflight({ context, scenario, sink, phase: lifecycle.phase });
    inputs.preflight = "pass";
    lifecycle.transition("prepared");
    await scenario.prepare({ context, sink, phase: lifecycle.phase });
    lifecycle.transition("launching");
    runtime = await host.launch({ context, scenario, sink, phase: lifecycle.phase });
    driver = await host.attach({ context, scenario, sink, runtime, phase: lifecycle.phase });
    lifecycle.transition("attached");
    lifecycle.transition("executing");
    const outcome = await scenario.execute({ context, sink, runtime, driver, host, phase: lifecycle.phase });
    assertScenarioOutcome({
      acquisition: outcome?.acquisition ?? "pass",
      oracle: outcome?.oracle ?? scenario.productOracle,
      manual: outcome?.manual ?? scenario.manualGate,
    });
    inputs.acquisition = outcome?.acquisition ?? "pass";
    inputs.oracle = outcome?.oracle ?? scenario.productOracle;
    inputs.manual = outcome?.manual ?? scenario.manualGate;
    lifecycle.transition("classifying");
  } catch (error) {
    const row = diagnostic(error);
    if (row.owner === "environment") {
      inputs.preflight = "blocked";
    } else if (row.owner === "product") {
      inputs.acquisition = "pass";
      inputs.oracle = "fail";
      inputs.manual = scenario.manualGate;
    } else if (inputs.preflight !== "pass") {
      inputs.preflight = "fail";
    } else {
      inputs.acquisition = "fail";
    }
    diagnostics.push(row);
    try { await sink.record("execution-failure", row, { phase: lifecycle.phase, owner: row.owner }); }
    catch (recordError) { diagnostics.push({ owner: "harness", code: "failure-evidence-write-failed", message: recordError.message, evidence: null }); }
  } finally {
    if (lifecycle.phase !== "cleaning" && lifecycle.phase !== "sealed") {
      try { lifecycle.transition("cleaning"); }
      catch (error) { diagnostics.push({ owner: "harness", code: "cleanup-transition-failed", message: error.message, evidence: null }); }
    }
    let hostCleanupPassed = false;
    let scenarioQuiescePassed = false;
    let scenarioQuiesceInvoked = false;
    let scenarioCleanupPassed = false;
    let scenarioQuiesceResources = [];
    const releaseScenarioResources = async () => {
      if (scenarioQuiesceInvoked) {
        diagnostics.push({ owner: "harness", code: "scenario-quiesce-repeated", message: "scenario quiesce was invoked more than once", evidence: null });
        scenarioQuiescePassed = false;
        return { input: "fail", resources: scenarioQuiesceResources };
      }
      scenarioQuiesceInvoked = true;
      try {
        const outcome = await scenario.quiesce({
          context,
          sink,
          runtime,
          driver,
          host,
          inputs: structuredClone(inputs),
          phase: "cleaning",
        });
        if (outcome?.input !== "pass" && outcome?.input !== "fail") {
          throw new DesktopE2eError("harness", "scenario-quiesce-outcome-invalid", "scenario quiesce outcome is invalid");
        }
        let quiesceInput = outcome.input;
        if (outcome.productFailure !== undefined && outcome.productFailure !== null) {
          if (inputs.acquisition !== "pass") {
            throw new DesktopE2eError(
              "harness",
              "post-execution-product-failure-before-acquisition",
              "scenario quiesce cannot assert a product failure before action acquisition",
              { acquisition: inputs.acquisition },
            );
          }
          const failure = outcome.productFailure;
          const productError = new DesktopE2eError(
            "product",
            failure.code,
            failure.message,
            failure.evidence ?? null,
          );
          inputs.oracle = "fail";
          diagnostics.push(diagnostic(productError));
          try { await sink.record("post-execution-product-failure", diagnostic(productError), { phase: "cleaning", owner: "product" }); }
          catch (recordError) {
            diagnostics.push({ owner: "harness", code: "post-execution-failure-evidence-write-failed", message: recordError.message, evidence: null });
            quiesceInput = "fail";
          }
        }
        scenarioQuiesceResources = structuredClone(outcome.resources ?? []);
        scenarioQuiescePassed = quiesceInput === "pass";
        return { input: quiesceInput, resources: structuredClone(scenarioQuiesceResources) };
      } catch (error) {
        const row = diagnostic(error);
        diagnostics.push({
          ...row,
          owner: "harness",
          code: row.code === "unclassified-error" ? "scenario-quiesce-failed" : row.code,
        });
        scenarioQuiesceResources = structuredClone(error?.evidence?.resources ?? scenarioQuiesceResources);
        return { input: "fail", resources: structuredClone(scenarioQuiesceResources) };
      }
    };
    try {
      const outcome = await host.cleanup({
        context,
        scenario,
        sink,
        runtime,
        driver,
        inputs: structuredClone(inputs),
        phase: "cleaning",
        releaseScenarioResources,
      });
      if (outcome?.input !== "pass" && outcome?.input !== "fail") throw new DesktopE2eError("harness", "cleanup-outcome-invalid", "host cleanup outcome is invalid");
      gracefulExit = structuredClone(outcome.gracefulExit);
      cleanup = structuredClone(outcome.cleanup);
      hostCleanupPassed = outcome.input === "pass";
    } catch (error) {
      const row = diagnostic(error);
      diagnostics.push({ ...row, owner: "harness", code: row.code === "unclassified-error" ? "exact-cleanup-failed" : row.code });
      gracefulExit = structuredClone(error?.evidence?.graceful_exit ?? gracefulExit);
      cleanup = structuredClone(error?.evidence?.cleanup ?? cleanup);
    }
    if (!scenarioQuiesceInvoked) {
      diagnostics.push({
        owner: "harness",
        code: "scenario-quiesce-order-not-acquired",
        message: "host cleanup did not release scenario resources before storage audit",
        evidence: null,
      });
      await releaseScenarioResources();
      scenarioQuiescePassed = false;
    }
    cleanup.scenario_quiesce_resources = structuredClone(scenarioQuiesceResources);
    try {
      const outcome = await scenario.cleanup({
        context,
        sink,
        runtime,
        driver,
        host,
        inputs: structuredClone(inputs),
        phase: "cleaning",
      });
      if (outcome?.input !== "pass" && outcome?.input !== "fail") {
        throw new DesktopE2eError("harness", "scenario-cleanup-outcome-invalid", "scenario cleanup outcome is invalid");
      }
      cleanup.scenario_resources = structuredClone(outcome.resources ?? []);
      scenarioCleanupPassed = outcome.input === "pass";
    } catch (error) {
      const row = diagnostic(error);
      diagnostics.push({
        ...row,
        owner: "harness",
        code: row.code === "unclassified-error" ? "scenario-cleanup-failed" : row.code,
      });
      cleanup.scenario_resources = structuredClone(error?.evidence?.resources ?? cleanup.scenario_resources ?? []);
    }
    inputs.cleanup = hostCleanupPassed && scenarioQuiescePassed && scenarioCleanupPassed ? "pass" : "fail";
    try {
      await sink.record("exact-cleanup", { graceful_exit: gracefulExit, ...cleanup, pass: inputs.cleanup === "pass" }, { phase: "cleaning", owner: "process-ledger" });
    } catch (error) {
      diagnostics.push({ owner: "harness", code: "cleanup-evidence-write-failed", message: error.message, evidence: null });
      inputs.cleanup = "fail";
    }
  }

  const verdict = classifyExecution(inputs);
  const finishedAt = now();
  const finishedMs = Date.parse(finishedAt);
  const result = {
    ...verdict,
    execution_id: context.executionId,
    scenario_id: scenario.id,
    started_at: startedAt,
    finished_at: finishedAt,
    elapsed_ms: Number.isFinite(startedMs) && Number.isFinite(finishedMs) ? Math.max(0, finishedMs - startedMs) : null,
    lifecycle: lifecycle.history,
    graceful_exit: gracefulExit,
    cleanup,
    diagnostics,
  };
  lifecycle.transition("sealed");
  result.lifecycle = lifecycle.history;
  const seal = await sink.seal(result);
  return { execution_root: context.root, result, seal };
}

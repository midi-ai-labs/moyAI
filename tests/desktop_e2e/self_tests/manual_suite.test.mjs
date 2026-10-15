import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { mkdir, mkdtemp, readFile, rm } from "node:fs/promises";
import { PassThrough } from "node:stream";
import { createManualSuiteScenario, normalizeManualSuiteOptions, manualSuiteFixtureFields, manualSuiteExpectedFields, sameManualSuiteProcess, manualSuiteCaseHost } from "../scenarios/manual_suite.mjs";
import { EvidenceSink } from "../core/evidence_sink.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { executeDesktopScenario } from "../core/desktop_execution.mjs";
import { prepareDesktopFixture } from "../scenarios/fixture.mjs";
import { manualSuiteReviewFingerprint, validateManualSuiteReview, waitForManualSuiteReview } from "../drivers/manual_suite_review.mjs";

const parent = fileURLToPath(new URL("../../../../project_sandbox/gui-suite-self-tests/", import.meta.url));
const identity = (generation = 1) => ({ generation, desktop_process_id: 90 + generation, launch_started_at: "2026-10-13T00:00:00.000Z",
  desktop_owner: { process_id: 90 + generation, process_start_time_utc_ticks: String(1000 + generation), executable_path: "C:\\bin\\moyai-desktop.exe" } });

test("suite rejects duplicate cases, unknown options, and unreviewed dependent fixtures", () => {
  const one = { scenario: "manual.case1", options: {} };
  assert.throws(() => normalizeManualSuiteOptions({ cases: [one, one] }), /repeated/);
  assert.throws(() => normalizeManualSuiteOptions({ cases: [one], restart: true }), /invalid/);
  assert.throws(() => normalizeManualSuiteOptions({ cases: [one, { scenario: "manual.case3", options: {}, fixture_from: "manual.case1" }] }), /operator review/);
  assert.throws(() => normalizeManualSuiteOptions({ cases: [{ scenario: "manual.case5_2", options: {} }] }), /OpenAI-compatible/);
});

test("suite scalar fixtures preserve explicit values, restore baseline defaults and reject unsupported syntax", () => {
  const fields = manualSuiteFixtureFields('[model]\nmodel = "model-x"\ncontext_window = 131072\n[docling]\nenabled = true\n');
  const baseline = [{ key: "model.model", value: "old" }, { key: "model.context_window", value: "10" }, { key: "docling.enabled", value: "false" }, { key: "model.max_retries", value: "2" }];
  assert.deepEqual(manualSuiteExpectedFields(baseline, fields), [
    { key: "model.model", value: "model-x", explicit: true }, { key: "model.context_window", value: "131072", explicit: true }, { key: "docling.enabled", value: "true", explicit: true }, { key: "model.max_retries", value: "2", explicit: false }]);
  assert.throws(() => manualSuiteFixtureFields('[model]\nmodel = "a"\nmodel = "b"'), /duplicate/);
  assert.throws(() => manualSuiteFixtureFields('[model]\nvalues = []'), /unsupported/);
  assert.throws(() => manualSuiteExpectedFields(baseline, new Map([["private.setting", "true"]])), /not exposed/);
});

test("fixture provenance distinguishes explicit values from equal baseline values", () => {
  const baseline = [{ key: "model.request_timeout_ms", value: "30000" }, { key: "approve.request_timeout_ms", value: "30000" }];
  assert.deepEqual(manualSuiteExpectedFields(baseline, new Map([["model.request_timeout_ms", "30000"]])), [
    { key: "model.request_timeout_ms", value: "30000", explicit: true }, { key: "approve.request_timeout_ms", value: "30000", explicit: false },
  ]);
});

test("same PID with a different start time, executable, or generation is not continuous execution", () => {
  assert.equal(sameManualSuiteProcess(identity(), identity()), true);
  for (const change of [value => value.generation++, value => value.desktop_owner.process_start_time_utc_ticks = "2000", value => value.desktop_owner.executable_path = "C:\\other.exe"]) {
    const after = identity(); change(after);
    assert.equal(sameManualSuiteProcess(identity(), after), false);
  }
});

test("Case5_2 recovery keeps suite startup resources available to later cases", async () => {
  const scenario = { id: "manual.case5_2", environment: {} };
  const environment = { MOYAI_HTTP_REQUEST_CAPTURE_DIR: "C:\\suite\\logs\\case2-provider-requests" };
  let observed;
  const host = { restart: async args => { observed = args; return { runtime: identity(2) }; } };
  const context = { paths: { workspace: "C:\\suite\\case52", config: "C:\\suite\\config" } };
  await manualSuiteCaseHost(host, scenario, environment, { allowRestoration: true }).restart({ scenario, context });
  assert.deepEqual(observed.scenario.environment, environment);
  assert.equal(observed.context, context);
  await assert.rejects(manualSuiteCaseHost(host, { id: "manual.case1" }, environment, { allowRestoration: true }).restart({ scenario, context }), /only the deferred/);
  observed = null;
  await assert.rejects(manualSuiteCaseHost(host, scenario, environment).restart({ scenario, context }), /only the deferred/);
  assert.equal(observed, null);
});

async function exercise(t, { ids = ["manual.case1", "manual.case4"], fail = null, restart = null, cleanupFail = null, quiesceThrow = null, cleanupThrow = null,
  acquireFailureAt = null, reviewMode = "stop_on_failure", prepareFailure = null, fixtureFrom = false, fixtureAccepted = false,
  restoration = undefined, restorationFail = null, restorationExpired = false, reviewStop = null, restorationCleanupFail = false,
  restorationEvaluationNg = false } = {}) {
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(path.join(parent, "continuous-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const paths = Object.fromEntries(["workspace", "config", "data", "prefs", "webview", "logs"].map(name => [name, path.join(root, name)]));
  for (const directory of Object.values(paths)) await mkdir(directory);
  Object.assign(paths, { config_file: path.join(paths.config, "config.toml"), prefs_file: path.join(paths.prefs, "desktop.toml"), database: path.join(paths.data, "moyai.sqlite3") });
  const context = { root, executionId: "e2e-continuous-test", scenarioId: "manual.suite", paths, manifest: { started_at: "2026-10-13T00:00:00Z" } };
  const sink = await EvidenceSink.create(path.join(root, "evidence"));
  const calls = [], workspaces = [], owners = [], acquisitions = [], transitions = [];
  const normalOptions = [], reviewRequests = [], configRequests = [], fixtureSources = [];
  let restoring = false;
  let restorationStages = { restart: "not_run", stage4: "not_run", hidden_oracle: "not_run", stage5: "not_run" };
  let runtime = identity(), driver = { generation: 1, evaluate: async () => ({ config_fields: [{ key: "model.model", value: "fixture" }] }) };
  const createScenario = (id, caseOptions) => ({ id, prepareBeforeLaunch: id === "manual.case2", productOracle: "pass", manualGate: "pending",
    environment: {},
    async prepare({ context, sink, phase }) {
      calls.push(`prepare:${id}`);
      if (fixtureFrom && id === "manual.case3") fixtureSources.push(caseOptions.fixture_source);
      if (prepareFailure === id) throw new DesktopE2eError("environment", "missing-fixture", "missing fixture");
      await prepareDesktopFixture({ context, sink, phase, owner: id, configText: '[model]\nmodel = "fixture"\n', sentinelName: null, sentinelText: "" });
    },
    async execute({ context, driver, runtime: before, host: caseHost }) {
      calls.push(`execute:${id}`); workspaces.push(context.paths.workspace); owners.push({ driver: driver.generation, process: before.desktop_process_id, database: context.paths.database });
      if (restart === id) await caseHost.restart({ scenario: this, context });
      if (fail === id) throw new DesktopE2eError("product", "output-missing", "output missing");
      return { acquisition: "pass", oracle: "pass", manual: "pending" };
    },
    async executeNormal(args) { normalOptions.push({ scenario: id, restoration: args.restorationDisposition }); calls.push(`normal:${id}`); return this.execute(args); },
    restorationCheckpoint() { return { workspace: workspaces[ids.indexOf("manual.case5_2")], session_id: "01M4JZ789M94P1YM0A3GWH9EDD", turn_id: "01M4JZ789M94P1YM0A3GWH9EDE",
      config_fields: [{ key: "model.model", value: "fixture", explicit: false }, { key: "side_chat.model", value: "saved-stage3-side", explicit: false }] }; },
    restorationReadiness() { return { ready: !restorationExpired, reason: restorationExpired ? "observation-timeout" : null, stages: structuredClone(restorationStages) }; },
    async executeRestoration({ context, driver: current, host: caseHost }) {
      calls.push(`restore:${id}`); owners.push({ driver: current.generation, restoration: true, workspace: context.paths.workspace });
      restoring = true;
      if (restorationFail) throw new DesktopE2eError(restorationFail, "test-restore-failed", "restoration failed");
      await caseHost.restart({ scenario: this, context });
      if (restorationEvaluationNg) {
        restorationStages = { restart: "pass", stage4: "pass", hidden_oracle: "failed", stage5: "pass" };
        throw new DesktopE2eError("product", "case5_2-final-evaluation", "artifact evaluation failed after Stage5", {
          failures: ["hidden-oracle-failed"], summary: { path: "case5_2/summary.json" },
        });
      }
      return { acquisition: "pass", oracle: "pass", manual: "pending" };
    },
    async quiesce() { calls.push(`quiesce:${id}`); if (quiesceThrow === id) throw new DesktopE2eError("harness", "test-quiesce-failed", "quiesce failed"); return { input: "pass", resources: [] }; },
    async cleanup() { calls.push(`cleanup:${id}`); if (cleanupThrow === id) throw new DesktopE2eError("harness", "test-cleanup-failed", "cleanup failed"); return { input: cleanupFail === id || restoring && restorationCleanupFail ? "fail" : "pass", resources: [] }; },
  });
  const suite = createManualSuiteScenario({ review_mode: reviewMode, ...(restoration === undefined ? {} : { case5_2_restoration: restoration }), cases: ids.map(scenario => ({ scenario, options: scenario === "manual.case5_2" ? { provider_profile: "openai_compatible" } : {},
    ...(fixtureFrom && scenario === "manual.case3" ? { fixture_from: "manual.case1" } : {}) })) }, {
    createScenario,
    acquire: async ({ context, driver }, { screenshotStem }) => {
      calls.push(`acquire:${screenshotStem}`);
      acquisitions.push({ workspace: context.paths.workspace, driver: driver.generation, stem: screenshotStem });
      if (acquireFailureAt === screenshotStem) throw new DesktopE2eError("harness", "test-shell-not-idle", "predecessor is not idle");
    },
    configure: async args => { configRequests.push({ stem: args.stem, fields: structuredClone(args.expectedFields) }); },
    transition: async ({ context, previousContext, driver }) => { calls.push("transition"); transitions.push({ from: previousContext.paths.workspace, to: context.paths.workspace, driver: driver.generation }); },
    restore: async ({ context, previousContext }) => { calls.push("restore-selection"); transitions.push({ from: previousContext.paths.workspace, to: context.paths.workspace, restoration: true }); },
    review: async request => { reviewRequests.push(structuredClone(request)); return { action: request.scenario_id === reviewStop ? "stop" : "continue", fixture_accepted: fixtureAccepted }; },
  });
  const host = { driver,
    async restart({ scenario }) { runtime = identity(2); driver = { ...driver, generation: 2 }; this.driver = driver; calls.push(`restart:${scenario.id}`); return { runtime, driver }; },
    async preflight() { calls.push("preflight"); },
    async launch() { calls.push("launch"); return runtime; },
    async attach() { return driver; },
    async attachedRuntime() { return { driver: this.driver, runtime }; },
    async cleanup({ releaseScenarioResources }) {
      calls.push("desktop-exit"); await releaseScenarioResources(); calls.push("sqlite-audit");
      return { input: "pass", gracefulExit: { requested: true }, cleanup: { desktop_exited: true, sqlite: { pass: true } } };
    },
  };
  const outcome = await executeDesktopScenario({ context, scenario: suite, host, sink });
  const resultsPath = path.join(sink.root, "suite/cases.json");
  let results = []; try { results = JSON.parse(await readFile(resultsPath)); } catch (error) { if (error.code !== "ENOENT") throw error; }
  let restorationResult = null;
  try { restorationResult = JSON.parse(await readFile(path.join(sink.root, "suite/restoration.json"))); } catch (error) { if (error.code !== "ENOENT") throw error; }
  let status = null;
  try { status = JSON.parse(await readFile(path.join(root, "suite-status.json"))).complete_scope; } catch (error) { if (error.code !== "ENOENT") throw error; }
  return { calls, workspaces, owners, acquisitions, transitions, normalOptions, reviewRequests, configRequests, fixtureSources, initialWorkspace: context.paths.workspace, outcome, results, restorationResult, status };
}

test("Case52 normal work remains in order and restoration is deferred until every normal case ends", async t => {
  const value = await exercise(t, { ids: ["manual.case1", "manual.case2", "manual.case3", "manual.case5_2", "manual.case6", "manual.case7"],
    fixtureFrom: true, fixtureAccepted: true, reviewMode: "operator" });
  assert.equal(value.outcome.result.classification, "manual_pending");
  assert.ok(value.calls.indexOf("normal:manual.case5_2") < value.calls.indexOf("execute:manual.case6"));
  assert.ok(value.calls.indexOf("cleanup:manual.case7") < value.calls.indexOf("restore:manual.case5_2"));
  assert.equal(value.calls.filter(row => row === "restart:manual.case5_2").length, 1);
  assert.deepEqual(value.owners.slice(0, 6).map(row => row.process), [91, 91, 91, 91, 91, 91]);
  assert.equal(value.restorationResult.classification, "manual_pending");
  assert.equal(value.results[3].phase, "normal");
  assert.deepEqual(value.fixtureSources, [value.workspaces[0]]);
  assert.equal(value.reviewRequests.at(-1).next_phase, "restoration");
  assert.equal(value.reviewRequests.at(-1).next_scenario_id, "manual.case5_2");
  assert.equal(value.reviewRequests[0].next_phase, "normal");
  assert.deepEqual(value.configRequests.at(-1), { stem: "case5_2-restoration", fields: [
    { key: "model.model", value: "fixture", explicit: false }, { key: "side_chat.model", value: "saved-stage3-side", explicit: false }] });
});

test("explicit restoration skip keeps normal Case52 results and never claims a full Case52 pass", async t => {
  const value = await exercise(t, { ids: ["manual.case5_2", "manual.case7"], restoration: "skip" });
  assert.equal(value.outcome.result.classification, "manual_pending");
  assert.equal(value.calls.includes("normal:manual.case5_2"), true);
  assert.equal(value.calls.includes("restore:manual.case5_2"), false);
  assert.equal(value.restorationResult.status, "not_run");
  assert.equal(value.restorationResult.reason, "explicit-skip");
  assert.equal(value.restorationResult.full_case52_pass, false);
  assert.deepEqual(value.normalOptions, [{ scenario: "manual.case5_2", restoration: "skip" }]);
  assert.equal(value.status, "normal_cases");
});

test("restoration failures preserve all normal results and keep their own failure owner", async t => {
  for (const restorationFail of ["product", "harness", "environment"]) {
    const value = await exercise(t, { ids: ["manual.case5_2", "manual.case7"], restorationFail });
    assert.equal(value.results.length, 2);
    assert.ok(value.results.every(row => row.classification === "manual_pending"));
    assert.equal(value.restorationResult.status, "failed");
    assert.equal(value.restorationResult.diagnostics[0].owner, restorationFail);
    assert.equal(value.outcome.result.classification, { product: "product_fail", harness: "harness_ng", environment: "environment_blocked" }[restorationFail]);
  }
});

test("restoration cleanup contributes to global cleanup and is attempted only once per phase", async t => {
  const value = await exercise(t, { ids: ["manual.case5_2", "manual.case7"], restorationCleanupFail: true });
  assert.equal(value.outcome.result.classification, "harness_ng");
  assert.equal(value.outcome.result.inputs.cleanup, "fail");
  assert.equal(value.restorationResult.inputs.cleanup, "fail");
  assert.equal(value.calls.filter(row => row === "quiesce:manual.case5_2").length, 2);
  assert.equal(value.calls.filter(row => row === "cleanup:manual.case5_2").length, 2);
});

test("deferred artifact NG preserves Stage5 and its restoration verdict while cleanup retains precedence", async t => {
  for (const restorationCleanupFail of [false, true]) {
    const value = await exercise(t, { ids: ["manual.case5_2", "manual.case7"], restorationEvaluationNg: true, restorationCleanupFail });
    assert.equal(value.outcome.result.classification, restorationCleanupFail ? "harness_ng" : "product_fail");
    assert.equal(value.outcome.result.product_failure, !restorationCleanupFail);
    assert.equal(value.restorationResult.product_failure, true);
    assert.equal(value.restorationResult.full_case52_pass, false);
    assert.equal(value.restorationResult.status, "failed");
    assert.deepEqual(value.restorationResult.stages, { restart: "pass", stage4: "pass", hidden_oracle: "failed", stage5: "pass" });
    assert.equal(value.restorationResult.diagnostics[0].code, "case5_2-final-evaluation");
    assert.deepEqual(value.restorationResult.diagnostics[0].evidence.failures, ["hidden-oracle-failed"]);
    assert.equal(value.results.length, 2);
    assert.ok(value.results.every(row => row.classification === "manual_pending"));
  }
});

test("a stopped final operator boundary or an expired Case52 budget prevents restoration input", async t => {
  for (const options of [{ reviewStop: "manual.case7" }, { restorationExpired: true }]) {
    const value = await exercise(t, { ids: ["manual.case5_2", "manual.case7"], reviewMode: "operator", ...options });
    assert.equal(value.results.length, 2);
    assert.equal(value.calls.includes("restore-selection"), false);
    assert.equal(value.calls.includes("restore:manual.case5_2"), false);
    assert.equal(value.restorationResult.status, "not_run");
    assert.equal(value.restorationResult.full_case52_pass, false);
  }
});

test("two cases share one launch/DB/PID and close once after both per-case resources settle", async t => {
  const value = await exercise(t);
  assert.equal(value.outcome.result.classification, "manual_pending");
  assert.equal(value.calls.filter(row => row === "launch").length, 1);
  assert.equal(value.calls.filter(row => row === "desktop-exit").length, 1);
  assert.equal(new Set(value.workspaces).size, 2);
  assert.equal(new Set(value.owners.map(row => row.database)).size, 1);
  assert.deepEqual(value.owners.map(row => row.process), [91, 91]);
  assert.ok(value.calls.indexOf("cleanup:manual.case4") < value.calls.indexOf("desktop-exit"));
  assert.ok(value.calls.indexOf("desktop-exit") < value.calls.indexOf("sqlite-audit"));
});

test("normal Case52 cannot restart before later cases; only the deferred phase obtains generation two", async t => {
  const value = await exercise(t, { ids: ["manual.case1", "manual.case5_2", "manual.case4"] });
  assert.equal(value.outcome.result.classification, "manual_pending");
  assert.deepEqual(value.owners.slice(0, 3).map(row => [row.driver, row.process]), [[1, 91], [1, 91], [1, 91]]);
  assert.equal(value.restorationResult.runtime_after.generation, 2);
  assert.ok(value.calls.indexOf("cleanup:manual.case4") < value.calls.indexOf("restart:manual.case5_2"));
  assert.equal(value.calls.filter(row => row === "launch").length, 1);
  const bad = await exercise(t, { ids: ["manual.case5_2", "manual.case4"], restart: "manual.case5_2" });
  assert.equal(bad.outcome.result.classification, "harness_ng");
  assert.equal(bad.calls.includes("execute:manual.case4"), false);
});

test("unexpected restart or failed case-resource cleanup stops the suite", async t => {
  for (const variant of [{ restart: "manual.case1" }, { cleanupFail: "manual.case1" }]) {
    const value = await exercise(t, variant);
    assert.equal(value.outcome.result.classification, "harness_ng");
    assert.equal(value.owners.length, 1);
  }
});

test("quiesce failures still attempt cleanup once and preserve both failures before Desktop exit", async t => {
  for (const cleanupThrow of [null, "manual.case1"]) {
    const value = await exercise(t, { quiesceThrow: "manual.case1", cleanupThrow });
    assert.equal(value.outcome.result.classification, "harness_ng");
    assert.equal(value.owners.length, 1);
    assert.equal(value.calls.filter(row => row === "quiesce:manual.case1").length, 1);
    assert.equal(value.calls.filter(row => row === "cleanup:manual.case1").length, 1);
    assert.ok(value.calls.indexOf("quiesce:manual.case1") < value.calls.indexOf("cleanup:manual.case1"));
    assert.ok(value.calls.indexOf("cleanup:manual.case1") < value.calls.indexOf("desktop-exit"));
    assert.deepEqual(value.results[0].diagnostics.map(row => row.code), cleanupThrow ? ["test-quiesce-failed", "test-cleanup-failed"] : ["test-quiesce-failed"]);
    assert.equal(value.calls.filter(row => row === "desktop-exit").length, 1);
  }
});

test("case boundaries reacquire the prior workspace using the current driver before transition", async t => {
  const value = await exercise(t, { ids: ["manual.case1", "manual.case5_2", "manual.case4"], restoration: "skip" });
  assert.equal(value.outcome.result.classification, "manual_pending");
  assert.deepEqual(value.acquisitions, [
    { workspace: value.initialWorkspace, driver: 1, stem: "suite-initial" },
    { workspace: value.initialWorkspace, driver: 1, stem: "suite-predecessor-1" },
    { workspace: value.workspaces[0], driver: 1, stem: "suite-predecessor-2" },
    { workspace: value.workspaces[1], driver: 1, stem: "suite-predecessor-3" },
  ]);
  assert.deepEqual(value.transitions, value.workspaces.map((workspace, index) => ({ from: index === 0 ? value.initialWorkspace : value.workspaces[index - 1], to: workspace, driver: 1 })));
  for (let index = 0; index < value.transitions.length; index++) {
    const boundary = value.calls.indexOf(`acquire:suite-predecessor-${index + 1}`);
    assert.equal(value.calls[boundary + 1], "transition");
    if (index > 0) assert.ok(value.calls.indexOf(`cleanup:${["manual.case1", "manual.case5_2"][index - 1]}`) < boundary);
  }
});

test("an unsettled predecessor prevents workspace mutation and the next case from executing", async t => {
  const value = await exercise(t, { acquireFailureAt: "suite-predecessor-2" });
  assert.equal(value.outcome.result.classification, "harness_ng");
  assert.equal(value.owners.length, 1);
  assert.equal(value.transitions.length, 1);
  assert.equal(value.calls.includes("execute:manual.case4"), false);
  assert.equal(value.calls.filter(row => row === "cleanup:manual.case4").length, 1);
  assert.ok(value.results[1].diagnostics.some(row => row.code === "test-shell-not-idle"));
});

test("product failure stays visible when an operator continues an independent case", async t => {
  const value = await exercise(t, { fail: "manual.case1", reviewMode: "operator" });
  assert.equal(value.outcome.result.classification, "product_fail");
  assert.equal(value.owners.length, 2);
  assert.equal(value.results[0].classification, "product_fail");
});

test("Case3 cannot copy or execute before the preceding Case1 fixture is explicitly reviewed", async t => {
  for (const fixtureAccepted of [false, true]) {
    const value = await exercise(t, { ids: ["manual.case1", "manual.case3"], fixtureFrom: true, reviewMode: "operator", fixtureAccepted });
    assert.equal(value.calls.includes("prepare:manual.case3"), fixtureAccepted);
    assert.equal(value.calls.includes("execute:manual.case3"), fixtureAccepted);
    assert.equal(value.outcome.result.classification, fixtureAccepted ? "manual_pending" : "harness_ng");
  }
});

test("startup-owned capture preparation precedes launch; partial preparation still cleans up once", async t => {
  const value = await exercise(t, { ids: ["manual.case1", "manual.case2"] });
  assert.ok(value.calls.indexOf("prepare:manual.case2") < value.calls.indexOf("launch"));
  const failed = await exercise(t, { ids: ["manual.case1", "manual.case2"], prepareFailure: "manual.case2" });
  assert.equal(failed.outcome.result.classification, "environment_blocked");
  assert.equal(failed.calls.filter(row => row === "cleanup:manual.case2").length, 1);
  assert.equal(failed.calls.includes("launch"), false);
});

test("operator review binds exact case/evidence and EOF never accepts a fixture", async () => {
  const request = { scenario_id: "manual.case1", classification: "manual_pending", fixture_review_required: true, evidence: { sha256: "a" } };
  const answer = { scenario_id: request.scenario_id, request_sha256: manualSuiteReviewFingerprint(request), action: "continue", fixture_accepted: true };
  assert.deepEqual(validateManualSuiteReview(answer, request), answer);
  assert.throws(() => validateManualSuiteReview(answer, { ...request, evidence: { sha256: "b" } }), /must match/);
  const input = new PassThrough(), output = new PassThrough();
  const pending = waitForManualSuiteReview(request, { input, output, timeoutMs: 100 }); input.end();
  assert.deepEqual(await pending, { action: "stop", fixture_accepted: false, reason: "stdin-closed" });
});

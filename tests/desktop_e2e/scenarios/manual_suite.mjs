import path from "node:path";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { DesktopE2eError, classifyExecution } from "../core/execution.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import { transitionManualSuiteWorkspace, restoreManualSuiteSession } from "../drivers/manual_suite_transition.mjs";
import { applyManualSuiteConfig } from "../drivers/manual_suite_config.mjs";
import { waitForManualSuiteReview } from "../drivers/manual_suite_review.mjs";

const OWNER = "scenario:manual.suite";
const CASES = new Set(["manual.case1", "manual.case2", "manual.case3", "manual.case4", "manual.case5", "manual.case5_2", "manual.case6", "manual.case7"]);
const errorRow = error => ({ owner: error?.owner ?? "harness", code: error?.code ?? "suite-case-error", message: error?.message ?? String(error), evidence: error?.evidence ?? null });

export function normalizeManualSuiteOptions(raw) {
  if (!raw || typeof raw !== "object" || Array.isArray(raw) || Object.keys(raw).some(key => !["cases", "review_mode", "case5_2_restoration"].includes(key))) throw new TypeError("invalid manual suite options");
  if (!Array.isArray(raw.cases) || raw.cases.length < 1 || raw.cases.length > CASES.size) throw new TypeError("manual suite requires one to eight cases");
  const reviewMode = raw.review_mode ?? "stop_on_failure";
  if (!["operator", "stop_on_failure"].includes(reviewMode)) throw new TypeError("invalid manual suite review_mode");
  const restoration = raw.case5_2_restoration ?? "after_cases";
  if (!["after_cases", "skip"].includes(restoration)) throw new TypeError("invalid manual suite case5_2_restoration");
  const seen = new Set();
  const cases = raw.cases.map(row => {
    if (!row || typeof row !== "object" || Array.isArray(row) || Object.keys(row).some(key => !["scenario", "options", "fixture_from"].includes(key))
      || !CASES.has(row.scenario) || seen.has(row.scenario) || !row.options || typeof row.options !== "object" || Array.isArray(row.options)) throw new TypeError("invalid or repeated manual suite case");
    if (row.fixture_from !== undefined && (row.scenario !== "manual.case3" || row.fixture_from !== "manual.case1" || !seen.has(row.fixture_from)
      || row.options.fixture_source !== undefined || reviewMode !== "operator")) throw new TypeError("Case3 fixture_from requires an earlier Case1 and operator review");
    // Shared runtime comparisons cannot silently inherit a different private
    // provider configuration. The existing standalone LM Studio route is separate.
    if (row.scenario === "manual.case5_2" && (row.options.provider_profile !== "openai_compatible" || row.options.configure_main_via_gui !== undefined)) {
      throw new TypeError("continuous Case5_2 requires the external OpenAI-compatible profile");
    }
    seen.add(row.scenario);
    return structuredClone(row);
  });
  return { cases, reviewMode, restoration };
}

// These inputs are the harness's generated fixtures, not a general TOML reader.
// Refuse new syntax instead of silently dropping part of a case's configuration.
export function manualSuiteFixtureFields(text) {
  const fields = new Map();
  let section = null;
  for (const line of text.replaceAll("\r\n", "\n").split("\n")) {
    if (!line.trim() || line.trim().startsWith("#")) continue;
    const header = line.match(/^\[([a-z_]+)\]$/);
    if (header) { section = header[1]; continue; }
    const pair = line.match(/^([a-z_]+) = (.+)$/);
    if (!section || !pair) throw new TypeError("unsupported manual fixture syntax");
    const value = JSON.parse(pair[2]);
    if (!["string", "number", "boolean"].includes(typeof value) || typeof value === "number" && !Number.isSafeInteger(value)) throw new TypeError("unsupported manual fixture value");
    const key = `${section}.${pair[1]}`;
    if (fields.has(key)) throw new TypeError("duplicate manual fixture field");
    fields.set(key, String(value));
  }
  return fields;
}

export function manualSuiteExpectedFields(baseline, explicit) {
  const fields = new Map(baseline.map(row => [row.key, row.value]));
  if (fields.size !== baseline.length) throw new TypeError("duplicate public configuration field");
  for (const [key, value] of explicit) {
    if (!fields.has(key)) throw new TypeError(`manual fixture field is not exposed by Settings: ${key}`);
    fields.set(key, value);
  }
  return [...fields].map(([key, value]) => ({ key, value, explicit: explicit.has(key) }));
}

export function sameManualSuiteProcess(before, after) {
  const a = before?.desktop_owner, b = after?.desktop_owner;
  return before?.generation === after?.generation && a?.process_id === b?.process_id
    && typeof a?.process_start_time_utc_ticks === "string" && a.process_start_time_utc_ticks === b?.process_start_time_utc_ticks
    && typeof a.executable_path === "string" && a.executable_path.toLowerCase() === b?.executable_path?.toLowerCase();
}

function assertSameProcess(before, after, label) {
  if (!sameManualSuiteProcess(before, after)) throw new DesktopE2eError("harness", "manual-suite-unexpected-restart", `${label} changed Desktop process identity`, { before, after });
}

export function manualSuiteCaseHost(host, scenario, environment, { allowRestoration = false } = {}) {
  return Object.freeze({
    attachedRuntime: () => host.attachedRuntime(),
    restart: async args => {
      if (!allowRestoration || scenario.id !== "manual.case5_2" || args.scenario !== scenario) throw new DesktopE2eError("harness", "manual-suite-unexpected-restart", "only the deferred Case5_2 recovery phase may restart the suite Desktop");
      // The case owns the recovery action; startup-only resources such as the
      // image request capture directory still belong to the whole suite.
      return host.restart({ ...args, scenario: { ...scenario, environment: { ...environment } } });
    },
  });
}

export function createManualSuiteScenario(raw, { createScenario, transition = transitionManualSuiteWorkspace, restore = restoreManualSuiteSession,
  configure = applyManualSuiteConfig, review = waitForManualSuiteReview, acquire = acquireInteractiveShell } = {}) {
  const options = normalizeManualSuiteOptions(raw);
  const state = { cases: [], rows: [], environment: {}, baseline: null, quiesced: false, restoration: null, restorationRow: null };
  const resourceRows = () => [...state.cases, ...(state.restorationRow ? [state.restorationRow] : [])];
  const prepareCase = async (row, phase) => {
    row.prepareStarted = true;
    await row.scenario.prepare({ context: row.preparation, sink: row.sink, phase });
    row.explicit = manualSuiteFixtureFields(await readFile(row.preparation.paths.config_file, "utf8"));
    row.prepared = true;
  };
  const settleCase = async row => {
    if (row.settled || !row.prepareStarted) return;
    row.settled = true;
    try {
      row.quiesce = await row.scenario.quiesce({ context: row.context, sink: row.sink, phase: "cleaning" });
      if (row.quiesce?.productFailure) {
        row.inputs.oracle = "fail";
        row.diagnostics.push({ owner: "product", ...row.quiesce.productFailure });
      }
    } catch (error) { row.diagnostics.push(errorRow(error)); }
    // Quiescing can fail after acquiring a case resource. Always attempt its
    // cleanup before allowing the shared Desktop owner to close the suite.
    try { row.cleanup = await row.scenario.cleanup({ context: row.context, sink: row.sink, phase: "cleaning" }); }
    catch (error) { row.diagnostics.push(errorRow(error)); }
    row.inputs.cleanup = row.quiesce?.input === "pass" && row.cleanup?.input === "pass" ? "pass" : "fail";
  };
  return Object.freeze({
    id: "manual.suite", productOracle: "pass", manualGate: "pending", databaseRequired: true, requestGracefulExit,
    get environment() { return { ...state.environment }; },
    async prepare({ context, sink, phase }) {
      const paths = new Map(options.cases.map((row, index) => [row.scenario, path.join(context.root, "cases", `${index + 1}-${row.scenario.slice(7)}`, "workspace")]));
      for (const [index, entry] of options.cases.entries()) {
        const caseRoot = path.dirname(paths.get(entry.scenario));
        const directories = Object.fromEntries(["workspace", "config", "data", "prefs", "webview"].map(name => [name, path.join(caseRoot, name)]));
        for (const directory of Object.values(directories)) await mkdir(directory, { recursive: true });
        const caseOptions = { ...entry.options, ...(entry.fixture_from ? { fixture_source: paths.get(entry.fixture_from) } : {}) };
        const scenario = createScenario(entry.scenario, caseOptions);
        const preparation = { ...context, scenarioId: scenario.id, paths: { ...context.paths, ...directories,
          config_file: path.join(directories.config, "config.toml"), prefs_file: path.join(directories.prefs, "desktop.toml"), database: path.join(directories.data, "moyai.sqlite3") } };
        const runtimeContext = { ...context, scenarioId: scenario.id, paths: { ...context.paths, workspace: directories.workspace },
          manualSuite: { shared_profile: true, case_index: index + 1, started_at: null } };
        const row = { scenario, entry, preparation, context: runtimeContext, sink: sink.scope(`cases/${index + 1}-${scenario.id.slice(7)}`),
          prepared: false, prepareStarted: false, settled: false, diagnostics: [], inputs: { preflight: "pass", acquisition: "not_run", oracle: "not_run", manual: "not_run", cleanup: "fail" } };
        state.cases.push(row);
        if (index === 0 || scenario.prepareBeforeLaunch === true) await prepareCase(row, phase);
        for (const [key, value] of Object.entries(scenario.environment ?? {})) {
          if (state.environment[key] !== undefined && state.environment[key] !== value) throw new TypeError(`conflicting suite launch environment: ${key}`);
          state.environment[key] = value;
        }
      }
      const config = await readFile(state.cases[0].preparation.paths.config_file, "utf8");
      await prepareDesktopFixture({ context, sink, phase, owner: OWNER, configText: config, sentinelName: null, sentinelText: "" });
      await sink.writeJson("suite/plan.json", { cases: options.cases, review_mode: options.reviewMode, profile_scope: "suite", case_deadline: "case-specific; unbounded by default",
        case5_2_restoration: options.restoration, internal_restart: "Case5_2 restoration after all normal cases only",
        paths: state.cases.map(row => ({ scenario: row.scenario.id, workspace: row.context.paths.workspace, fixture_config: row.preparation.paths.config_file })) });
    },
    async execute({ context, driver, runtime, host, sink }) {
      await acquire({ context, driver, sink }, { evidenceOwner: OWNER, screenshotStem: "suite-initial" });
      state.baseline = (await driver.evaluate("window.__TAURI_INTERNALS__.invoke('desktop_state')")).config_fields.map(({ key, value }) => ({ key, value }));
      let previousContext = context;
      let previousRuntime = runtime;
      const restorationCase = state.cases.find(row => row.scenario.id === "manual.case5_2");
      let restorationBlocked = null;
      for (const [index, row] of state.cases.entries()) {
        row.context.manualSuite.started_at = new Date().toISOString();
        row.context.manifest = { ...context.manifest, started_at: row.context.manualSuite.started_at };
        let attachment;
        try {
          if (row.entry.fixture_from && state.rows.find(value => value.scenario_id === row.entry.fixture_from)?.review?.fixture_accepted !== true) {
            throw new DesktopE2eError("harness", "manual-suite-fixture-not-reviewed", "the Case1 fixture has not been accepted for Case3");
          }
          if (!row.prepared) await prepareCase(row, "executing");
          for (const [key, value] of Object.entries(row.scenario.environment ?? {})) if (state.environment[key] !== value) throw new TypeError("case requires an environment change after Desktop startup");
          attachment = await host.attachedRuntime();
          assertSameProcess(previousRuntime, attachment.runtime, "between-case boundary");
          await acquire({ context: previousContext, driver: attachment.driver, sink: row.sink },
            { evidenceOwner: OWNER, screenshotStem: `suite-predecessor-${index + 1}` });
          await transition({ context: row.context, previousContext, driver: attachment.driver, runtime: attachment.runtime, sink: row.sink });
          await configure({ driver: attachment.driver, sink: row.sink, stem: row.scenario.id.slice(7), expectedFields: manualSuiteExpectedFields(state.baseline, row.explicit) });
          const before = await host.attachedRuntime();
          assertSameProcess(attachment.runtime, before.runtime, "case preparation");
          row.before = before.runtime;
          await row.sink.record("manual-suite-case-start", { scenario: row.scenario.id, workspace: row.context.paths.workspace, runtime: row.before }, { phase: "executing", owner: OWNER });
          const execute = row.scenario.id === "manual.case5_2" ? row.scenario.executeNormal : row.scenario.execute;
          if (typeof execute !== "function") throw new TypeError("Case5_2 must expose its normal Stage1–3 execution");
          Object.assign(row.inputs, await execute.call(row.scenario, { context: row.context, driver: before.driver, runtime: before.runtime,
            host: manualSuiteCaseHost(host, row.scenario, state.environment), sink: row.sink, phase: "executing", restorationDisposition: options.restoration }));
        } catch (error) {
          const diagnostic = errorRow(error);
          row.diagnostics.push(diagnostic);
          if (diagnostic.owner === "product") Object.assign(row.inputs, { acquisition: "pass", oracle: "fail", manual: "pending" });
          else if (diagnostic.owner === "environment") row.inputs.preflight = "blocked";
          else row.inputs.acquisition = "fail";
        }
        // A completed case must release its probes/evaluators while the shared
        // Desktop stays alive. Closed SQLite audit belongs only to suite cleanup.
        await settleCase(row);
        try {
          const after = await host.attachedRuntime();
          row.after = after.runtime;
          if (row.before) assertSameProcess(row.before, row.after, "normal case execution");
          previousRuntime = after.runtime;
        } catch (error) { row.inputs.acquisition = "fail"; row.diagnostics.push(errorRow(error)); }
        const result = { scenario_id: row.scenario.id, phase: "normal", workspace: row.context.paths.workspace, started_at: row.context.manualSuite.started_at,
          finished_at: new Date().toISOString(), ...classifyExecution(row.inputs), diagnostics: row.diagnostics,
          runtime_before: row.before ?? null, runtime_after: row.after ?? null, resources: { quiesce: row.quiesce, cleanup: row.cleanup },
          shared_desktop_cleanup: "pending_suite_end", review: null };
        const evidence = await row.sink.writeJson("case-result.json", result);
        state.rows.push(result);
        await sink.record("manual-suite-case-result", result, { phase: "executing", owner: OWNER });
        if (["harness_ng", "environment_blocked"].includes(result.classification)) { restorationBlocked = "normal-case-failed"; break; }
        const hasRestoration = restorationCase && options.restoration === "after_cases";
        if ((index < state.cases.length - 1 || hasRestoration) && options.reviewMode === "operator") {
          result.review = await review({ execution_id: context.executionId, scenario_id: row.scenario.id, evidence,
            classification: result.classification, workspace: row.context.paths.workspace,
            next_phase: index < state.cases.length - 1 ? "normal" : "restoration",
            next_scenario_id: index < state.cases.length - 1 ? state.cases[index + 1].scenario.id : "manual.case5_2",
            fixture_review_required: options.cases.some(entry => entry.fixture_from === row.scenario.id) });
          await row.sink.writeJson("operator-review.json", result.review);
          if (result.review?.action !== "continue") { restorationBlocked = "operator-stop"; break; }
        } else if (result.classification === "product_fail") { restorationBlocked = "normal-case-failed"; break; }
        previousContext = row.context;
      }
      if (restorationCase) {
        state.restoration = { scenario_id: "manual.case5_2", phase: "restoration", selected: options.restoration === "after_cases",
          status: "not_run", reason: options.restoration === "skip" ? "explicit-skip" : restorationBlocked ?? "normal-cases-incomplete", full_case52_pass: false,
          stages: { restart: "not_run", stage4: "not_run", stage5: "not_run", hidden_oracle: "not_run" } };
        if (options.restoration === "after_cases" && restorationBlocked === null && state.rows.length === state.cases.length) {
          const row = { ...restorationCase, settled: false, diagnostics: [], quiesce: null, cleanup: null,
            inputs: { preflight: "pass", acquisition: "not_run", oracle: "not_run", manual: "not_run", cleanup: "fail" } };
          const readiness = row.scenario.restorationReadiness();
          state.restoration.eligibility = readiness;
          if (!readiness.ready) state.restoration.reason = readiness.reason;
          else {
            state.restorationRow = row;
            try {
              const before = await host.attachedRuntime();
              assertSameProcess(previousRuntime, before.runtime, "restoration entry");
              await acquire({ context: previousContext, driver: before.driver, sink: row.sink }, { evidenceOwner: OWNER, screenshotStem: "suite-predecessor-restoration" });
              const checkpoint = row.scenario.restorationCheckpoint();
              await restore({ context: row.context, previousContext, driver: before.driver, sink: row.sink, checkpoint });
              await configure({ driver: before.driver, sink: row.sink, stem: "case5_2-restoration", expectedFields: checkpoint.config_fields });
              row.before = before.runtime;
              await row.sink.record("manual-suite-restoration-start", { workspace: row.context.paths.workspace, runtime: before.runtime }, { phase: "executing", owner: OWNER });
              Object.assign(row.inputs, await row.scenario.executeRestoration({ context: row.context, driver: before.driver, runtime: before.runtime,
                host: manualSuiteCaseHost(host, row.scenario, state.environment, { allowRestoration: true }), sink: row.sink, phase: "executing" }));
            } catch (error) {
              const diagnostic = errorRow(error); row.diagnostics.push(diagnostic);
              if (diagnostic.owner === "product") Object.assign(row.inputs, { acquisition: "pass", oracle: "fail", manual: "pending" });
              else if (diagnostic.owner === "environment") row.inputs.preflight = "blocked";
              else row.inputs.acquisition = "fail";
            }
            await settleCase(row);
            try { row.after = (await host.attachedRuntime()).runtime; }
            catch (error) { row.inputs.acquisition = "fail"; row.diagnostics.push(errorRow(error)); }
            state.restoration = { ...state.restoration, status: "completed", reason: null, ...classifyExecution(row.inputs), diagnostics: row.diagnostics,
              full_case52_pass: false, runtime_before: row.before ?? null, runtime_after: row.after ?? null,
              resources: { quiesce: row.quiesce, cleanup: row.cleanup } };
            state.restoration.status = ["pass", "manual_pending"].includes(state.restoration.classification) ? "completed" : "failed";
            state.restoration.stages = row.scenario.restorationReadiness().stages ?? state.restoration.stages;
          }
        }
        await restorationCase.sink.writeJson("restoration-result.json", state.restoration);
        await sink.writeJson("suite/restoration.json", state.restoration);
        await sink.record("manual-suite-restoration-result", state.restoration, { phase: "executing", owner: OWNER });
      }
      await sink.writeJson("suite/cases.json", state.rows);
      const results = [...state.rows, ...(state.restoration?.classification ? [state.restoration] : [])];
      const failed = results.find(row => row.classification === "harness_ng") ?? results.find(row => row.classification === "environment_blocked");
      const complete = state.rows.length === state.cases.length;
      await writeFile(path.join(context.root, "suite-status.json"), `${JSON.stringify({ complete, complete_scope: "normal_cases", normal_cases_complete: complete,
        restoration_complete: state.restoration === null || state.restoration.status === "completed", cases: state.rows, restoration: state.restoration }, null, 2)}\n`, { flag: "wx" });
      if (failed) throw new DesktopE2eError(failed.classification === "harness_ng" ? "harness" : "environment", "manual-suite-case-failed", "a case could not safely complete in the shared Desktop", { cases: state.rows, restoration: state.restoration });
      return { acquisition: "pass", oracle: results.some(row => row.product_failure) ? "fail" : complete ? "pass" : "not_required", manual: "pending" };
    },
    async quiesce() {
      for (const row of resourceRows()) await settleCase(row);
      state.quiesced = true;
      return { input: resourceRows().filter(row => row.prepareStarted).every(row => row.inputs.cleanup === "pass") ? "pass" : "fail",
        resources: resourceRows().filter(row => row.prepareStarted).map(row => ({ scenario: row.scenario.id, phase: row === state.restorationRow ? "restoration" : "normal", ...row.quiesce })) };
    },
    async cleanup({ cleanup, sink }) {
      await sink.writeJson("suite/cleanup.json", { cases: state.rows, restoration: state.restoration, shared_cleanup: cleanup });
      return { input: state.quiesced && resourceRows().filter(row => row.prepareStarted).every(row => row.inputs.cleanup === "pass") ? "pass" : "fail", resources: [] };
    },
  });
}

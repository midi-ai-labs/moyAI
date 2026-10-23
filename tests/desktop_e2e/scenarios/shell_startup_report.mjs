import path from "node:path";
import { readFile } from "node:fs/promises";

import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";
import { initialSetupPurposeButtonsReady, initialSetupStepReady, observeInitialSetupSurface } from "./settings_initial_setup.mjs";
import { cleanupShellBaseline, quiesceShellBaseline, requestGracefulExit } from "./shell_baseline.mjs";

const OWNER = "scenario:shell.startup-report";

export function startupReportDecision(report, processId, surfaceReady) {
  if (report === null) return { accepted: false, failure: null };
  if (report?.schema !== 1 || !["ready", "error"].includes(report?.status)
    || !Number.isInteger(report?.exit_code) || typeof report?.message !== "string") {
    return { accepted: false, failure: "startup-report-invalid" };
  }
  if (report.process_id !== processId) return { accepted: false, failure: "startup-report-process-mismatch" };
  if (report.status === "error") return { accepted: false, failure: "startup-reported-error" };
  if (report.exit_code !== 0) return { accepted: false, failure: "startup-report-invalid" };
  if (!surfaceReady) return { accepted: false, failure: "startup-reported-ready-before-render" };
  return { accepted: true, failure: null };
}

async function readStartupReport(candidate) {
  try {
    const text = await readFile(candidate, "utf8");
    return text.trim() === "" ? null : JSON.parse(text);
  }
  catch (error) {
    if (error?.code === "ENOENT" || error instanceof SyntaxError) return null;
    throw error;
  }
}

export function createShellStartupReportScenario() {
  let reportPath = null;
  return Object.freeze({
    id: "shell.startup-report",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    startupTarget: "preferences",
    get startupReportPath() { return reportPath; },
    async prepare({ context, sink, phase }) {
      reportPath = path.join(context.paths.logs, "startup-report.json");
      await prepareDesktopFixture({ context, sink, phase, owner: OWNER, configMode: "absent", sentinelName: null, sentinelText: "" });
    },
    async execute({ context, runtime, driver, sink }) {
      const observation = await waitForObservation({
        label: "Fresh Desktop reports readiness after its initial setup has rendered",
        timeoutMs: 20_000,
        pollMs: 50,
        retrySampleErrors: false,
        sample: async () => {
          // Sample the DOM after the report, so an older splash snapshot
          // cannot turn a valid acknowledgement into an early-success failure.
          const report = await readStartupReport(reportPath);
          const surface = await observeInitialSetupSurface(driver);
          const dom = await driver.evaluate(`(() => ({ ready_state: document.readyState, splash_count: document.querySelectorAll('.splash-screen').length, visible: document.visibilityState === 'visible' }))()`);
          const surfaceReady = initialSetupStepReady(surface, [], "start")
            && surface.projection.startup.setup_target.globalConfigPath === context.paths.config_file
            && initialSetupPurposeButtonsReady(surface)
            && dom.ready_state === "complete" && dom.splash_count === 0 && dom.visible === true;
          const decision = startupReportDecision(report, runtime.desktop_process_id, surfaceReady);
          if (decision.failure !== null) {
            throw new DesktopE2eError("product", decision.failure, "Desktop startup report does not match its rendered initial screen", { report, surface, dom });
          }
          return { report, surface, dom, decision };
        },
        accept: value => value.decision.accepted,
      });
      await sink.record("startup-report-rendered", { ...observation.value, attempts: observation.attempts, elapsed_ms: observation.elapsed_ms }, { phase: "executing", owner: OWNER });
      await captureScenarioScreenshot({ cdp: driver, sink, name: "startup-report-initial-setup", owner: OWNER });
      return { acquisition: "pass", oracle: "pass", manual: "not_required" };
    },
    requestGracefulExit,
    quiesce: quiesceShellBaseline,
    cleanup: cleanupShellBaseline,
  });
}

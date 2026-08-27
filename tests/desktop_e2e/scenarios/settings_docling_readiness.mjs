import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import {
  DesktopCommandProbe,
  assertExactDesktopCommandSequence,
} from "../drivers/desktop_command_probe.mjs";
import {
  SCRIPTED_PROVIDER_MODEL_ID,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";
import { WebviewInput, assertTrustedProbeSequence } from "../drivers/webview_input.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";
import { quiesceProviderResource } from "./provider_restart.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import {
  observeSettingsPreferencesSurface,
  preferencesReady,
} from "./settings_preferences.mjs";

const OWNER = "scenario:settings.docling-readiness";
export const DOCLING_READINESS_HTTP_STATUS = 204;
export const DOCLING_READINESS_CONTEXT_WINDOW = "65536";

const SHOW_SETTINGS = Object.freeze({
  selector: 'aside.sidebar button.settings[data-action="show-config"][title="設定"]',
  identity: { tag: "BUTTON", action: "show-config" },
});
const SETTINGS_TOOLS = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] nav.settings-nav a[href="#settings-tools"]',
  identity: { tag: "A", href: "#settings-tools" },
});
const CHECK_DOCLING_READINESS = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] button[data-action="check-docling-readiness"][aria-controls="docling-readiness-status"]',
  identity: { tag: "BUTTON", action: "check-docling-readiness" },
});
const CLOSE_SETTINGS = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] button[data-action="close-overlay"]',
  identity: { tag: "BUTTON", action: "close-overlay" },
});

function sameValue(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

function errorFree(surface) {
  return surface?.visible_fatal_count === 0
    && surface?.visible_recoverable_error_count === 0
    && surface?.visible_validation_error_count === 0;
}

function configFieldValue(projection, key) {
  const matches = Array.isArray(projection?.config_fields)
    ? projection.config_fields.filter((field) => field?.key === key)
    : [];
  return matches.length === 1 ? matches[0].value : null;
}

function exactReadinessRequest(row) {
  return row?.sequence === 1
    && row?.method === "GET"
    && row?.pathname === "/ready"
    && row?.query_present === false
    && row?.route === "docling_readiness"
    && row?.contract?.pass === true;
}

export function exactHeldDoclingReadinessLedger(ledger) {
  return Array.isArray(ledger)
    && ledger.length === 1
    && exactReadinessRequest(ledger[0])
    && ledger[0].response_phase === "held"
    && ledger[0].response_status === null;
}

export function exactCompletedDoclingReadinessLedger(
  ledger,
  expectedHttpStatus = DOCLING_READINESS_HTTP_STATUS,
) {
  return Array.isArray(ledger)
    && ledger.length === 1
    && exactReadinessRequest(ledger[0])
    && ledger[0].response_phase === "completed"
    && ledger[0].response_status === expectedHttpStatus;
}

function readinessSurfaceBase(surface, expectedTarget) {
  const readiness = surface?.settings?.docling_readiness;
  return errorFree(surface)
    && surface?.projection?.overlay === "config"
    && sameValue(surface?.projection?.config_target, expectedTarget)
    && configFieldValue(surface.projection, "docling.enabled") === "true"
    && surface?.settings?.dialog_count === 1
    && surface.settings.dialog_visible === true
    && surface.settings.docling.count === 1
    && surface.settings.docling.checked === true
    && surface.settings.docling_label.count === 1
    && surface.settings.docling_label.visible === true
    && surface.settings.dirty_badge_visible === false
    && surface.settings.save.count === 1
    && surface.settings.save.enabled === false
    && surface.settings.discard.count === 0
    && surface.settings.close.count === 1
    && readiness?.button?.count === 1
    && readiness.button.visible === true
    && readiness.status_count === 1
    && readiness.status_visible === true
    && typeof readiness.text === "string"
    && readiness.text.length > 0
    && surface?.close_confirmation?.count === 0
    && surface?.visible_dialog_count === 1;
}

export function idleDoclingReadinessReady(surface, ledger, expectedTarget) {
  const readiness = surface?.settings?.docling_readiness;
  return Array.isArray(ledger)
    && ledger.length === 0
    && readinessSurfaceBase(surface, expectedTarget)
    && surface?.projection?.docling_readiness?.status === "idle"
    && surface.projection.docling_readiness.endpoint === ""
    && surface.projection.docling_readiness.httpStatus === null
    && readiness.button.enabled === true
    && readiness.status === "idle"
    && readiness.aria_busy === "false"
    && !surface.projection.pending_async_operations?.includes("docling_readiness_check");
}

export function checkingDoclingReadinessReady(surface, ledger, expectedTarget, expectedEndpoint) {
  const projection = surface?.projection?.docling_readiness;
  const readiness = surface?.settings?.docling_readiness;
  return exactHeldDoclingReadinessLedger(ledger)
    && readinessSurfaceBase(surface, expectedTarget)
    && projection?.status === "checking"
    && projection.endpoint === expectedEndpoint
    && projection.httpStatus === null
    && readiness.button.enabled === false
    && readiness.status === "checking"
    && readiness.aria_busy === "true"
    && surface.projection.pending_async_operations?.includes("docling_readiness_check");
}

export function terminalDoclingReadinessReady(
  surface,
  ledger,
  expectedTarget,
  expectedEndpoint,
  expectedHttpStatus = DOCLING_READINESS_HTTP_STATUS,
) {
  const expectedStatus = expectedHttpStatus >= 200 && expectedHttpStatus < 300 ? "ready" : "unavailable";
  const projection = surface?.projection?.docling_readiness;
  const readiness = surface?.settings?.docling_readiness;
  return exactCompletedDoclingReadinessLedger(ledger, expectedHttpStatus)
    && readinessSurfaceBase(surface, expectedTarget)
    && projection?.status === expectedStatus
    && projection.endpoint === expectedEndpoint
    && projection.httpStatus === expectedHttpStatus
    && typeof projection.message === "string"
    && projection.message.length > 0
    && readiness.button.enabled === true
    && readiness.status === expectedStatus
    && readiness.aria_busy === "false"
    && !surface.projection.pending_async_operations?.includes("docling_readiness_check");
}

export function expectedDoclingReadinessCommand(surface) {
  const target = surface?.projection?.config_target;
  if (target === null || typeof target !== "object" || Array.isArray(target)) {
    throw new TypeError("Docling readiness command expectation requires a config target");
  }
  return {
    command: "check_docling_readiness",
    args: { expectedTarget: structuredClone(target) },
  };
}

export function doclingReadinessFixtureConfig(baseUrl) {
  return `[model]
base_url = ${JSON.stringify(baseUrl)}
model = ${JSON.stringify(SCRIPTED_PROVIDER_MODEL_ID)}
provider_metadata_mode = "openai_compatible_only"
provider_api_mode = "responses"
connect_timeout_ms = 1000
request_timeout_ms = 30000
max_retries = 0
context_window = ${DOCLING_READINESS_CONTEXT_WINDOW}
supports_tools = false
supports_images = false
parallel_tool_calls = false

[permissions]
access_mode = "default"

[multi_agent]
enabled = false
mode = "explicit_request_only"
max_concurrent_agents = 2
max_concurrent_model_requests = 1

[docling]
enabled = true
base_url = ${JSON.stringify(baseUrl)}
timeout_ms = 5000

[mcp]
enabled = false
`;
}

function productFailure(code, message, evidence) {
  return new DesktopE2eError("product", code, message, evidence);
}

function classifyObservationFailure(error, code, message) {
  if (error?.code === "observation-timeout" && error?.evidence?.last_error === null) {
    return productFailure(code, message, error.evidence);
  }
  return error;
}

async function waitForProductStage({ label, timeoutMs = 10_000, sample, decide, code, message }) {
  let terminal = "pending";
  let observed;
  try {
    observed = await waitForObservation({
      label,
      timeoutMs,
      pollMs: 50,
      retrySampleErrors: false,
      sample,
      accept: (value) => {
        terminal = decide(value);
        return terminal !== "pending";
      },
    });
  } catch (error) {
    throw classifyObservationFailure(error, code, message);
  }
  if (terminal === "fail") throw productFailure(code, message, observed.value);
  return observed;
}

function invalidReadinessLedger(ledger) {
  if (!Array.isArray(ledger) || ledger.length > 1) return true;
  if (ledger.length === 0) return false;
  const [row] = ledger;
  return !exactReadinessRequest(row)
    || row.response_phase === "rejected"
    || (row.response_status !== null && row.response_status !== DOCLING_READINESS_HTTP_STATUS);
}

function readinessDecision(predicate) {
  return (sample) => {
    if ((sample?.surface && !errorFree(sample.surface)) || invalidReadinessLedger(sample?.ledger)) return "fail";
    return predicate(sample?.surface, sample?.ledger) ? "pass" : "pending";
  };
}

async function trustedClick(input, locator) {
  const start = (await input.snapshotProbe()).sequence;
  const target = await input.click(locator);
  const snapshot = await input.snapshotProbe(start);
  const probe = assertTrustedProbeSequence(snapshot, {
    afterSequence: start,
    expected: [
      { type: "pointerdown", identity: locator.identity, button: 0, buttons: 1 },
      { type: "pointerup", identity: locator.identity, button: 0, buttons: 0 },
      { type: "click", identity: locator.identity, button: 0, buttons: 0 },
    ],
  });
  return { target, probe };
}

async function waitForCommands(probe, afterSequence, expected, label) {
  const observed = await waitForObservation({
    label,
    timeoutMs: 10_000,
    pollMs: 50,
    retrySampleErrors: false,
    sample: () => probe.snapshot(afterSequence),
    accept: (snapshot) => snapshot.calls.length >= expected.length,
  });
  return assertExactDesktopCommandSequence(observed.value, { afterSequence, expected });
}

function errorObservation(error) {
  return {
    name: error?.name ?? "Error",
    code: error?.code ?? null,
    message: error?.message ?? String(error),
    evidence: error?.evidence ?? null,
  };
}

async function settleResources(state, input, commandProbe, primaryError) {
  const outcome = { input: null, command_probe: null, failures: [] };
  try { outcome.input = await input.cleanup(); }
  catch (error) { outcome.failures.push({ owner: "webview-input", error: errorObservation(error) }); }
  try { outcome.command_probe = await commandProbe.remove(); }
  catch (error) { outcome.failures.push({ owner: "desktop-command-probe", error: errorObservation(error) }); }
  state.resourceSettlement = outcome;
  if (outcome.failures.length > 0 && primaryError === null) {
    throw new DesktopE2eError(
      "harness",
      "docling-readiness-resource-cleanup-failed",
      "Docling readiness input/command probes did not settle",
      outcome,
    );
  }
  return outcome;
}

export function createSettingsDoclingReadinessScenario() {
  const state = {
    provider: null,
    acceptedLedger: null,
    quiesceOutcome: null,
    resourceSettlement: null,
  };
  return Object.freeze({
    id: "settings.docling-readiness",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    requestGracefulExit,
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({
        expectedPrompt: "docling-readiness-must-not-call-the-main-provider",
        doclingReadinessStatus: DOCLING_READINESS_HTTP_STATUS,
      });
      await prepareDesktopFixture({
        context,
        sink,
        phase,
        owner: OWNER,
        configText: doclingReadinessFixtureConfig(state.provider.baseUrl),
        sentinelName: "E2E_SETTINGS_DOCLING_READINESS.txt",
        sentinelText: "moyAI Desktop E2E explicit Docling readiness fixture.\n",
      });
      await sink.record("docling-readiness-fixture", state.provider.resourceObservation(), { phase, owner: OWNER });
    },
    async execute({ context, driver: cdp, sink }) {
      const provider = state.provider;
      if (provider === null) throw new Error("Docling readiness fixture was not prepared");
      await acquireInteractiveShell({ context, driver: cdp, sink }, {
        evidenceOwner: OWNER,
        screenshotStem: "docling-readiness-shell-ready",
      });
      const coldSurface = await observeSettingsPreferencesSurface(cdp);
      if (provider.requestLedger.length !== 0 || !errorFree(coldSurface)
        || coldSurface?.projection?.overlay !== "none"
        || coldSurface?.visible_dialog_count !== 0
        || coldSurface?.visible_backdrop_count !== 0) {
        throw productFailure(
          "docling-readiness-cold-start-contract-mismatch",
          "enabled Docling contacted the network or the Desktop shell was not clean before explicit activation",
          { surface: coldSurface, ledger: provider.requestLedger },
        );
      }

      const input = new WebviewInput(cdp, { probeId: "settings-docling-readiness" });
      const commands = new DesktopCommandProbe(cdp, {
        probeId: "settings-docling-readiness",
        commands: ["check_docling_readiness", "close_overlay"],
      });
      let settled = false;
      let primaryError = null;
      try {
        await input.installProbe();
        await commands.install();
        await trustedClick(input, SHOW_SETTINGS);
        const opened = await waitForProductStage({
          label: "clean enabled Docling Preferences",
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(cdp), ledger: provider.requestLedger }),
          decide: readinessDecision((surface, ledger) => preferencesReady(surface, ledger, {
            contextWindow: DOCLING_READINESS_CONTEXT_WINDOW,
            doclingEnabled: true,
          })),
          code: "docling-readiness-preferences-not-ready",
          message: "Preferences did not open with the clean enabled Docling fixture",
        });
        const expectedTarget = structuredClone(opened.value.surface.projection.config_target);
        await trustedClick(input, SETTINGS_TOOLS);
        const idle = await waitForProductStage({
          label: "idle explicit Docling readiness control",
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(cdp), ledger: provider.requestLedger }),
          decide: readinessDecision((surface, ledger) => idleDoclingReadinessReady(surface, ledger, expectedTarget)),
          code: "docling-readiness-control-not-ready",
          message: "Test Docling was not visibly actionable without cold-start traffic",
        });

        const expectedCommand = expectedDoclingReadinessCommand(idle.value.surface);
        const commandStart = (await commands.snapshot()).sequence;
        const activation = await trustedClick(input, CHECK_DOCLING_READINESS);
        const command = await waitForCommands(commands, commandStart, [expectedCommand], "exact Docling readiness command");
        const endpoint = `${provider.baseUrl}/ready`;
        const checking = await waitForProductStage({
          label: "held Docling readiness checking state",
          timeoutMs: 30_000,
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(cdp), ledger: provider.requestLedger }),
          decide: readinessDecision((surface, ledger) => checkingDoclingReadinessReady(
            surface,
            ledger,
            expectedTarget,
            endpoint,
          )),
          code: "docling-readiness-checking-not-observed",
          message: "the exact Test Docling activation did not produce one held GET and typed checking state",
        });
        const release = provider.releaseDoclingReadiness();
        const terminal = await waitForProductStage({
          label: "terminal Docling readiness result",
          timeoutMs: 30_000,
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(cdp), ledger: provider.requestLedger }),
          decide: readinessDecision((surface, ledger) => terminalDoclingReadinessReady(
            surface,
            ledger,
            expectedTarget,
            endpoint,
          )),
          code: "docling-readiness-terminal-mismatch",
          message: "the held readiness response did not settle to the exact typed ready result",
        });
        const terminalScreenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "settings-docling-readiness-ready",
          owner: OWNER,
        });

        const readinessCommandSettled = assertExactDesktopCommandSequence(
          await commands.snapshot(commandStart),
          { afterSequence: commandStart, expected: [expectedCommand] },
        );
        const closeStart = (await commands.snapshot()).sequence;
        await trustedClick(input, CLOSE_SETTINGS);
        await waitForProductStage({
          label: "clean Docling readiness Settings close",
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(cdp), ledger: provider.requestLedger }),
          decide: readinessDecision((surface, ledger) => exactCompletedDoclingReadinessLedger(ledger)
            && surface?.projection?.overlay === "none"
            && surface?.visible_dialog_count === 0
            && surface?.visible_backdrop_count === 0
            && surface?.active?.tag === "BUTTON"
            && surface.active.action === "show-config"),
          code: "docling-readiness-settings-close-failed",
          message: "clean Settings did not close and restore its trigger after the readiness result",
        });
        const closeCommand = await waitForCommands(
          commands,
          closeStart,
          [{ command: "close_overlay", args: {} }],
          "Docling readiness Settings close command",
        );
        state.acceptedLedger = structuredClone(provider.requestLedger);
        await sink.record("settings-docling-readiness-acquired", {
          cold_start_ledger: [],
          activation,
          command,
          command_settled: readinessCommandSettled,
          checking: checking.value,
          release,
          terminal: terminal.value,
          close_command: closeCommand,
          accepted_ledger: state.acceptedLedger,
          screenshot: terminalScreenshot,
        }, { phase: "executing", owner: OWNER });
        settled = true;
        await settleResources(state, input, commands, null);
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        if (!settled) {
          settled = true;
          try { await settleResources(state, input, commands, primaryError); }
          catch (error) { if (primaryError === null) throw error; }
        }
      }
    },
    async quiesce({ inputs }) {
      if (state.quiesceOutcome !== null) return structuredClone(state.quiesceOutcome);
      state.quiesceOutcome = await quiesceProviderResource({
        provider: state.provider,
        acceptedLedger: state.acceptedLedger,
        inputs,
      });
      return structuredClone(state.quiesceOutcome);
    },
    async cleanup() {
      const resourcesPass = state.resourceSettlement?.failures?.length === 0;
      const quiescePass = state.quiesceOutcome?.input === "pass";
      return {
        input: resourcesPass && quiescePass ? "pass" : "fail",
        resources: [{
          kind: "settings-docling-readiness-verification",
          resource_settlement: state.resourceSettlement,
          quiesce_input: state.quiesceOutcome?.input ?? null,
        }],
      };
    },
  });
}

import { isDeepStrictEqual } from "node:util";

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
import {
  WebviewInput,
  assertTrustedProbeSequence,
  assertTrustedTextInsertion,
} from "../drivers/webview_input.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import {
  PROVIDER_RESTART_PROMPT,
  classifyAcquiredObservationFailure,
  observeProviderTurnSurface,
  providerTurnDomAccepted,
  quiesceProviderResource,
  relevantProviderHistory,
  settledCompletedProviderTurn,
} from "./provider_restart.mjs";
import { captureScenarioScreenshot, selectedNavigationIdentity } from "./observations.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";

const OWNER = "scenario:provider.responses-progress";
export const PROVIDER_RESPONSES_PROGRESS_TIMEOUT_MS = 900;
export const PROVIDER_RESPONSES_PROGRESS_CADENCE_MS = 400;
export const PROVIDER_RESPONSES_PROGRESS_DELTA_COUNT = 16;
export const PROVIDER_RESPONSES_PROGRESS_RESPONSE = "PACED_STREAM_PROGRESS_OK";
const EXPECTED_EVENT_TYPES = Object.freeze([
  ...Array(PROVIDER_RESPONSES_PROGRESS_DELTA_COUNT).fill("response.output_text.delta"),
  "response.output_item.done",
  "response.completed",
]);

const PROMPT = Object.freeze({
  selector: "section.composer textarea#prompt",
  identity: { tag: "TEXTAREA", id: "prompt" },
});
const SEND = Object.freeze({
  selector: 'section.composer button[data-action="send"]',
  identity: { tag: "BUTTON", action: "send" },
});

function sameValue(left, right) {
  return isDeepStrictEqual(left, right);
}

function productFailure(code, message, evidence) {
  return new DesktopE2eError("product", code, message, evidence);
}

function errorObservation(error) {
  return {
    name: error?.name ?? "Error",
    code: error?.code ?? null,
    message: error?.message ?? String(error),
    evidence: error?.evidence ?? null,
  };
}

export function providerResponsesProgressFixtureConfig(baseUrl) {
  return `[model]
base_url = ${JSON.stringify(baseUrl)}
model = ${JSON.stringify(SCRIPTED_PROVIDER_MODEL_ID)}
provider_profile = "openai_responses"
connect_timeout_ms = 1000
request_timeout_ms = ${PROVIDER_RESPONSES_PROGRESS_TIMEOUT_MS}
max_retries = 0
context_window = 65536
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
enabled = false

[mcp]
enabled = false
`;
}

function responseRowPrefix(row) {
  return row?.method === "POST"
    && row?.pathname === "/v1/responses"
    && row?.query_present === false
    && row?.contract?.pass === true
    && row.contract.client_generation_fields_absent === true
    && sameValue(row.contract.client_generation_fields_present, []);
}

function timingPrefixAccepted(stream) {
  if (stream?.schema_version !== "desktop-e2e.scripted-provider-response-stream.v1"
    || stream?.cadence_ms !== PROVIDER_RESPONSES_PROGRESS_CADENCE_MS
    || stream?.delta_count !== PROVIDER_RESPONSES_PROGRESS_DELTA_COUNT
    || stream?.configured_total_duration_ms !== 6_800
    || stream?.expected_event_count !== EXPECTED_EVENT_TYPES.length
    || !Number.isFinite(stream?.headers_sent_elapsed_ms)
    || stream.headers_sent_elapsed_ms < 0
    || stream.headers_sent_elapsed_ms >= PROVIDER_RESPONSES_PROGRESS_TIMEOUT_MS
    || !Array.isArray(stream?.events)
    || stream.events.length > EXPECTED_EVENT_TYPES.length) return false;
  let previousElapsed = null;
  return stream.events.every((event, index) => {
    const elapsed = event?.elapsed_ms;
    const gapAccepted = previousElapsed === null
      ? Number.isFinite(elapsed) && elapsed >= 0 && elapsed < PROVIDER_RESPONSES_PROGRESS_TIMEOUT_MS
      : Number.isFinite(elapsed)
        && elapsed > previousElapsed
        && elapsed - previousElapsed < PROVIDER_RESPONSES_PROGRESS_TIMEOUT_MS;
    previousElapsed = elapsed;
    return event?.sequence === index + 1
      && event?.event_type === EXPECTED_EVENT_TYPES[index]
      && Number.isSafeInteger(event?.size_bytes)
      && event.size_bytes > 0
      && gapAccepted;
  });
}

function progressedBeyondAbsoluteTimeout(row) {
  const stream = row?.response_stream;
  const latest = stream?.events?.at(-1);
  return responseRowPrefix(row)
    && row.response_status === 200
    && row.response_phase === "streaming"
    && timingPrefixAccepted(stream)
    && latest?.elapsed_ms > PROVIDER_RESPONSES_PROGRESS_TIMEOUT_MS
    && stream.terminal_sent === false
    && stream.response_finished === false
    && stream.peer_close_observed === false
    && stream.peer_closed_before_terminal === false;
}

function blockingSurfaceFailure(surface) {
  const rows = Array.isArray(surface?.projection?.transcript_rows)
    ? surface.projection.transcript_rows
    : [];
  return surface?.visible_fatal_count > 0
    || surface?.visible_recoverable_error_count > 0
    || rows.some((row) => row?.row_kind === "error")
    || surface?.projection?.startup?.status === "failed"
    || ["failed", "cancelled", "incomplete"].includes(surface?.projection?.run_status_key);
}

export function providerResponsesProgressDecision(sample) {
  const ledger = sample?.ledger;
  if (!Array.isArray(ledger) || ledger.length > 1) return "fail";
  const row = ledger[0];
  if (row !== undefined && (!responseRowPrefix(row)
    || (row.response_status !== null && row.response_status !== 200)
    || row?.response_stream?.peer_closed_before_terminal === true)) return "fail";
  if (blockingSurfaceFailure(sample?.surface)) return "fail";
  const projection = sample?.surface?.projection;
  return progressedBeyondAbsoluteTimeout(row)
    && projection?.run_status_key === "running"
    && projection?.task_activity_state === "running"
    && projection?.busy === true
    && projection?.run_phase === "Provider応答受信中"
    ? "pass"
    : "pending";
}

function exactTerminalRow(row) {
  const stream = row?.response_stream;
  return responseRowPrefix(row)
    && row.response_status === 200
    && row.response_phase === "completed"
    && timingPrefixAccepted(stream)
    && sameValue(stream.events.map((event) => event.event_type), EXPECTED_EVENT_TYPES)
    && stream.events[PROVIDER_RESPONSES_PROGRESS_DELTA_COUNT - 1].elapsed_ms
      > PROVIDER_RESPONSES_PROGRESS_TIMEOUT_MS
    && stream.terminal_sent === true
    && stream.terminal_elapsed_ms > PROVIDER_RESPONSES_PROGRESS_TIMEOUT_MS
    && stream.response_finished === true
    && Number.isFinite(stream.response_finished_elapsed_ms)
    && stream.response_finished_elapsed_ms >= stream.terminal_elapsed_ms
    && stream.peer_close_observed === false
    && stream.peer_closed_before_terminal === false;
}

export function providerResponsesProgressTerminalDecision(sample) {
  const ledger = sample?.ledger;
  if (!Array.isArray(ledger) || ledger.length > 1) return "fail";
  const row = ledger[0];
  if (row !== undefined && (!responseRowPrefix(row)
    || (row.response_status !== null && row.response_status !== 200)
    || row?.response_stream?.peer_closed_before_terminal === true)) return "fail";
  if (blockingSurfaceFailure(sample?.surface)) return "fail";
  const projection = sample?.surface?.projection;
  const history = relevantProviderHistory(projection);
  const identity = selectedNavigationIdentity(projection);
  return exactTerminalRow(row)
    && settledCompletedProviderTurn(projection, {
      expectedPrompt: PROVIDER_RESTART_PROMPT,
      expectedResponse: PROVIDER_RESPONSES_PROGRESS_RESPONSE,
    })
    && providerTurnDomAccepted(sample.surface, history, identity, {
      expectedPrompt: PROVIDER_RESTART_PROMPT,
      expectedResponse: PROVIDER_RESPONSES_PROGRESS_RESPONSE,
    })
    ? "pass"
    : "pending";
}

async function waitForProductStage({ label, timeoutMs, sample, decide, code, message }) {
  let observed;
  let decision = "pending";
  try {
    observed = await waitForObservation({
      label,
      timeoutMs,
      pollMs: 25,
      sample,
      accept: (value) => {
        decision = decide(value);
        return decision !== "pending";
      },
    });
  } catch (error) {
    throw classifyAcquiredObservationFailure(error, { code, message });
  }
  if (decision === "fail") throw productFailure(code, message, { observation: observed });
  return observed;
}

async function trustedClick(input, locator) {
  const start = (await input.snapshotProbe()).sequence;
  const target = await input.click(locator);
  const probe = assertTrustedProbeSequence(await input.snapshotProbe(start), {
    afterSequence: start,
    expected: [
      { type: "pointerdown", identity: locator.identity, button: 0, buttons: 1 },
      { type: "pointerup", identity: locator.identity, button: 0, buttons: 0 },
      { type: "click", identity: locator.identity, button: 0, buttons: 0 },
    ],
  });
  return { target, probe };
}

async function trustedPromptInput(input) {
  const focus = await trustedClick(input, PROMPT);
  const start = (await input.snapshotProbe()).sequence;
  const insertion = await input.insertText(PROMPT, PROVIDER_RESTART_PROMPT);
  const probe = assertTrustedTextInsertion(await input.snapshotProbe(start), {
    afterSequence: start,
    identity: PROMPT.identity,
    text: PROVIDER_RESTART_PROMPT,
  });
  return { focus, insertion, probe };
}

async function settleResources(state, input, commands, primaryError) {
  const outcome = { input: null, command_probe: null, failures: [] };
  try { outcome.input = await input.cleanup(); }
  catch (error) { outcome.failures.push({ owner: "webview-input", error: errorObservation(error) }); }
  try { outcome.command_probe = await commands.remove(); }
  catch (error) { outcome.failures.push({ owner: "desktop-command-probe", error: errorObservation(error) }); }
  state.resourceOutcome = outcome;
  if (outcome.failures.length > 0 && primaryError === null) {
    throw new DesktopE2eError(
      "harness",
      "provider-responses-progress-resource-cleanup-failed",
      "Responses progress input and command probes did not settle",
      outcome,
    );
  }
}

export function createProviderResponsesProgressScenario() {
  const state = {
    provider: null,
    acceptedLedger: null,
    quiesceOutcome: null,
    resourceOutcome: null,
  };
  return Object.freeze({
    id: "provider.responses-progress",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    requestGracefulExit,
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({
        expectedPrompt: PROVIDER_RESTART_PROMPT,
        responseText: PROVIDER_RESPONSES_PROGRESS_RESPONSE,
        responsePacing: {
          cadenceMs: PROVIDER_RESPONSES_PROGRESS_CADENCE_MS,
          deltaCount: PROVIDER_RESPONSES_PROGRESS_DELTA_COUNT,
        },
      });
      await prepareDesktopFixture({
        context,
        sink,
        phase,
        owner: OWNER,
        configText: providerResponsesProgressFixtureConfig(state.provider.baseUrl),
        sentinelName: "E2E_PROVIDER_RESPONSES_PROGRESS.txt",
        sentinelText: "moyAI Desktop E2E rolling Responses progress fixture.\n",
      });
      await sink.record("scripted-provider-started", state.provider.resourceObservation(), { phase, owner: OWNER });
    },
    async execute({ context, driver: cdp, sink }) {
      const provider = state.provider;
      if (provider === null) throw new Error("paced Responses provider was not prepared");
      await acquireInteractiveShell({ context, driver: cdp, sink }, {
        evidenceOwner: OWNER,
        screenshotStem: "provider-responses-progress-shell-ready",
      });
      if (provider.requestLedger.length !== 0) {
        throw productFailure(
          "provider-responses-progress-cold-start-request",
          "Desktop contacted the provider before trusted Send",
          { ledger: provider.requestLedger },
        );
      }

      const input = new WebviewInput(cdp, { probeId: "provider-responses-progress" });
      const commands = new DesktopCommandProbe(cdp, {
        probeId: "provider-responses-progress-commands",
        commands: ["submit_prompt", "cancel_run"],
      });
      let primaryError = null;
      try {
        await input.installProbe();
        await commands.install();
        const typed = await trustedPromptInput(input);
        const ready = await observeProviderTurnSurface(cdp);
        if (ready.prompt.value !== PROVIDER_RESTART_PROMPT) {
          throw productFailure(
            "provider-responses-progress-prompt-drift",
            "trusted text insertion did not produce the exact progress prompt",
            { expected: PROVIDER_RESTART_PROMPT, surface: ready },
          );
        }
        const expectedCommand = {
          command: "submit_prompt",
          args: {
            text: PROVIDER_RESTART_PROMPT,
            expectedTarget: structuredClone(ready.projection.draft_target),
            expectedRunTarget: structuredClone(ready.projection.run_target),
          },
        };
        const commandStart = (await commands.snapshot()).sequence;
        const send = await trustedClick(input, SEND);
        const commandObservation = await waitForObservation({
          label: "provider.responses-progress exact submit command",
          timeoutMs: 10_000,
          pollMs: 25,
          retrySampleErrors: false,
          sample: () => commands.snapshot(commandStart),
          accept: (snapshot) => snapshot.calls.length >= 1,
        });
        const exactCommand = assertExactDesktopCommandSequence(commandObservation.value, {
          afterSequence: commandStart,
          expected: [expectedCommand],
        });

        const progressed = await waitForProductStage({
          label: "Responses progress beyond the former absolute timeout",
          timeoutMs: 10_000,
          sample: async () => ({
            surface: await observeProviderTurnSurface(cdp),
            ledger: provider.requestLedger,
          }),
          decide: providerResponsesProgressDecision,
          code: "provider-responses-progress-did-not-outlive-timeout",
          message: "a progressing Responses stream did not remain active beyond the configured inactivity interval",
        });
        const progressCommand = assertExactDesktopCommandSequence(await commands.snapshot(commandStart), {
          afterSequence: commandStart,
          expected: [expectedCommand],
        });
        const progressScreenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "provider-responses-progress-active",
          owner: OWNER,
        });

        const terminal = await waitForProductStage({
          label: "paced Responses terminal",
          timeoutMs: 30_000,
          sample: async () => ({
            surface: await observeProviderTurnSurface(cdp),
            ledger: provider.requestLedger,
          }),
          decide: providerResponsesProgressTerminalDecision,
          code: "provider-responses-progress-terminal-mismatch",
          message: "the paced Responses stream did not settle to the exact terminal GUI and wire contract",
        });
        const finalCommand = assertExactDesktopCommandSequence(await commands.snapshot(commandStart), {
          afterSequence: commandStart,
          expected: [expectedCommand],
        });
        const terminalScreenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "provider-responses-progress-completed",
          owner: OWNER,
        });
        state.acceptedLedger = structuredClone(terminal.value.ledger);
        await sink.record("provider-responses-progress-completed", {
          input_kind: "browser_trusted",
          typed,
          send,
          expected_command: expectedCommand,
          command: exactCommand,
          progress_command: progressCommand,
          final_command: finalCommand,
          progress: {
            projection_revision: progressed.value.surface.projection.projection_revision,
            run_status_key: progressed.value.surface.projection.run_status_key,
            run_active_step: progressed.value.surface.projection.run_active_step,
            response_stream: progressed.value.ledger[0].response_stream,
            screenshot: progressScreenshot,
          },
          terminal: {
            projection_revision: terminal.value.surface.projection.projection_revision,
            history: relevantProviderHistory(terminal.value.surface.projection),
            selected_navigation: selectedNavigationIdentity(terminal.value.surface.projection),
            response_stream: terminal.value.ledger[0].response_stream,
            screenshot: terminalScreenshot,
          },
          provider_ledger: state.acceptedLedger,
          provider_resource: provider.resourceObservation(),
        }, { phase: "executing", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        await settleResources(state, input, commands, primaryError);
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
      const quiesced = state.quiesceOutcome !== null;
      const pass = quiesced
        && state.quiesceOutcome.input === "pass"
        && state.resourceOutcome?.failures?.length === 0;
      return {
        input: pass ? "pass" : "fail",
        resources: [{
          kind: "provider-responses-progress-verification",
          quiesced,
          quiesce_input: state.quiesceOutcome?.input ?? null,
          interaction_resources: state.resourceOutcome,
        }],
      };
    },
  });
}

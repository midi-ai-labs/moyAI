import crypto from "node:crypto";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { lstat, readFile, readdir } from "node:fs/promises";

import { waitForObservation } from "../core/deadline.mjs";
import { canonicalU64, canonicalUlid } from "../core/canonical_identity.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import {
  WebviewInput, assertTrustedProbeSequence, assertTrustedTextInsertion,
} from "../drivers/webview_input.mjs";
import { runWindowsExternalProcess } from "../drivers/windows_external_process.mjs";
import { DesktopCommandProbe } from "../drivers/desktop_command_probe.mjs";
import { operatorRequestFingerprint, waitForOperatorReview } from "../drivers/operator_review.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { observeProviderTurnSurface } from "./provider_restart.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";

import {
  manualLivePrompt as manualCase1Prompt,
  normalizeManualLiveOptions as normalizeManualCase1Options,
  manualLiveFixtureConfig as manualCase1FixtureConfig,
  manualLiveTerminalDecision as manualCase1TerminalDecision,
  manualLiveUnittestResult as manualCase1UnittestResult,
  manualLiveHistory as manualCase1PublicHistory,
  manualLiveTranscriptResult as manualCase1TranscriptResult,
  manualLivePermissionRequest as manualCase1PermissionRequest,
  manualLivePermissionLocator as manualCase1PermissionLocator,
  manualLiveClick as click,
  manualLiveGeneratedFiles as generatedFiles,
  manualLiveExternalProcess as external,
  manualLiveExportTranscript as exportTranscript, ManualLiveSession
} from "../drivers/manual_live_session.mjs";
export {
  manualLivePrompt as manualCase1Prompt,
  normalizeManualLiveOptions as normalizeManualCase1Options,
  manualLiveFixtureConfig as manualCase1FixtureConfig,
  manualLiveTerminalDecision as manualCase1TerminalDecision,
  manualLiveUnittestResult as manualCase1UnittestResult,
  manualLiveHistory as manualCase1PublicHistory,
  manualLiveTranscriptResult as manualCase1TranscriptResult,
  manualLivePermissionRequest as manualCase1PermissionRequest,
  manualLivePermissionLocator as manualCase1PermissionLocator,
  manualLiveClick as click,
  manualLiveGeneratedFiles as generatedFiles,
  manualLiveExternalProcess as external,
  manualLiveExportTranscript as exportTranscript
} from "../drivers/manual_live_session.mjs";

const OWNER = "scenario:manual.case1";
const SPEC = fileURLToPath(new URL("../../manual_ST/case1/spec.md", import.meta.url));
const CONTEXT_WINDOW = 131_072;
const CASE_TIMEOUT_MS = 15 * 60 * 1000;
const MAX_OUTPUT_BYTES = 8 * 1024 * 1024;
const PROMPT = { selector: "section.composer textarea#prompt", identity: { tag: "TEXTAREA", id: "prompt" } };
const SEND = { selector: 'section.composer button[data-action="send"]', identity: { tag: "BUTTON", action: "send" } };
const EXPORT = { selector: 'header.topbar button[data-action="export-transcript"]', identity: { tag: "BUTTON", action: "export-transcript" } };
const MANUAL_REVIEW = Object.freeze([
  "Read calculator.py and test_calculator.py; inspect the callable API and CLI against all four operations, zero division, and invalid operators.",
  "Inspect canonical tool targets and generated files for references or mutations outside the workspace; no global filesystem isolation claim is made by this scenario.",
  "Read the intermediate outputs and final assistant answer; confirm it reports only demonstrated results and identifies unfinished work.",
  "Confirm agent-side unittest success, file creation and tool metrics from the public projection and transcript. Missing call/result or numeric detail remains unverified; do not open the sealed database.",
]);
const sha256 = bytes => crypto.createHash("sha256").update(bytes).digest("hex");
const failure = (code, message, evidence) => new DesktopE2eError("product", code, message, evidence);

export function manualCase1MachinePredicates({ terminal, approval, prompt, history, generated,
  incompleteReason = "unreviewed-approval-operator-stopped" }) {
  const decision = manualCase1TerminalDecision(terminal);
  const completed = !approval && decision === "completed";
  const failures = [];
  if (!approval) {
    if (decision !== "completed") failures.push("desktop-terminal-not-completed");
    if (history.users.length !== 1 || history.users[0] !== prompt) failures.push("canonical-user-request-mismatch");
    if (completed) for (const name of ["calculator.py", "test_calculator.py"]) if (!generated.some(row => row.path === name && row.source !== null)) failures.push(`missing-${name}`);
  }
  return { failures, run_external_unittest: completed,
    external_unittest: { status: "not_reached", reason: approval ? incompleteReason : "desktop-terminal-not-completed" },
    diagnostics: { terminal_decision: decision, run_status_key: terminal.projection?.run_status_key ?? null,
      status_message: terminal.projection?.status_message ?? null, status_detail: terminal.projection?.status_detail ?? null } };
}

export function createManualCase1Scenario(rawOptions = {}) {
  const options = normalizeManualCase1Options(rawOptions);
  const state = { prompt: null, live: null, inputCleanup: null, commandCleanup: null, summary: null, quiesced: false };
  return Object.freeze({
    id: "manual.case1", productOracle: "pass", manualGate: "pending", databaseRequired: true, requestGracefulExit,
    async prepare({ context, sink, phase }) {
      if ((await readdir(context.paths.workspace)).length !== 0 || (await readdir(context.paths.data)).length !== 0
        || (await readdir(context.paths.config)).length !== 0) throw new DesktopE2eError("harness", "case1-not-fresh", "Case1 requires empty workspace and fresh config/data", {});
      const spec = await readFile(SPEC);
      state.prompt = manualCase1Prompt(spec.toString("utf8"));
      await prepareDesktopFixture({ context, sink, phase, owner: OWNER, configText: manualCase1FixtureConfig(options), sentinelName: null, sentinelText: "" });
      await sink.record("case1-input", { options, spec: { path: SPEC, sha256: sha256(spec) }, prompt: state.prompt, context_window: CONTEXT_WINDOW,
        observation_timeout_ms: options.observationTimeoutMs ?? CASE_TIMEOUT_MS, provider_lifecycle: "external-unmanaged", provider_owned: false }, { phase, owner: OWNER });
    },
    async execute({ context, driver: cdp, sink }) {
      let primaryError = null;
      try {
        await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: OWNER, screenshotStem: "case1-ready" });
        const initial = await observeProviderTurnSurface(cdp);
        const p = initial.projection;
        if (p.provider_effective_profile !== "openai_compatible" || p.provider_effective_base_url !== options.providerBaseUrl
          || p.provider_effective_model_id !== options.model || p.provider_effective_api_key_env !== options.apiKeyEnv
          || Number(p.provider_effective_context_window) !== CONTEXT_WINDOW) throw failure("case1-provider-config", "Case1 did not use the explicit connection and context budget", { projection: p });
        state.live = new ManualLiveSession({ context, driver: cdp, sink, options, owner: OWNER, stem: "case1", observationTimeoutMs: CASE_TIMEOUT_MS });
        await state.live.open();
        const live = await state.live.send(state.prompt);
        const terminal = live.terminal;
        const approvals = live.approvals;
        const approval = live.incomplete;
        const reviewStopReason = live.incompleteReason;
        const terminalScreenshot = await captureScenarioScreenshot({ cdp, sink, name: "case1-final-state", owner: OWNER });
        const projectionEvidence = await sink.writeJson("case1/final-projection.json", terminal);
        const sessionId = terminal.projection.run_target?.sessionId;
        const turnId = terminal.projection.run_target?.expectedState?.latestTurnId;
        if (!canonicalUlid(sessionId) || !canonicalUlid(turnId)) throw failure("case1-terminal-owner", "Case1 final projection has no exact canonical owner", { sessionId, turnId });
        const history = manualCase1PublicHistory(terminal.projection);
        const historyEvidence = await sink.writeJson("case1/public-history.json", history);
        const generated = await generatedFiles(context, sink);
        const predicates = manualCase1MachinePredicates({ terminal, approval, prompt: state.prompt, history, generated,
          incompleteReason: options.approvalMode === "stop" ? "unreviewed-approval-operator-stopped" : reviewStopReason });
        let unittest = predicates.external_unittest;
        let transcript = null;
        const failures = predicates.failures;
        if (predicates.run_external_unittest) {
          const result = await external({ context, sink, options, label: "case1-unittest", args: ["-m", "unittest"], observationBudget: state.live.observationBudget });
          unittest = { status: "executed", ...manualCase1UnittestResult(result.result, result.stdout, result.stderr) };
          if (!unittest.pass) failures.push("external-unittest-failed-or-empty");
        }
        transcript = await exportTranscript({ context, input: state.live.input, sink, sessionId, prompt: state.prompt, observationBudget: state.live.observationBudget });
        await captureScenarioScreenshot({ cdp, sink, name: "case1-artifacts-and-transcript", owner: OWNER });
        state.live.observationBudget.assertRemaining("case1 case completion");
        state.summary = {
          schema_version: "desktop-e2e.manual-case1.v1", machine_gate: approval ? options.approvalMode === "stop" ? "unreviewed_approval_operator_stopped_incomplete" : "operator_review_incomplete" : failures.length === 0 ? "pass" : "fail",
          approval_mode: options.approvalMode, review_stop_reason: reviewStopReason, observation_budget: state.live.observationBudget.snapshot(),
          user_request: state.prompt, session_id: sessionId, turn_id: turnId,
          metrics: history.metrics, terminal: terminal.projection.run_target, diagnostics: predicates.diagnostics, tools: history.work_summaries, approvals,
          public_history: historyEvidence, agent_unittest: history.agent_unittest, exact_tool_call_result_pairs: history.exact_tool_call_result_pairs,
          transcript, generated, external_unittest: unittest,
          final_projection: projectionEvidence, terminal_screenshot: terminalScreenshot, failures,
          manual_review: MANUAL_REVIEW, manual_verdict: "pending", provider_cleanup: "none_external_unmanaged",
        };
        await sink.writeJson("case1/summary.json", state.summary);
        await sink.record("case1-machine-gate", state.summary, { phase: "executing", owner: OWNER });
        if (failures.length !== 0) throw failure("case1-machine-gate", "Case1 failed its acquired machine predicates; manual review remains separate", state.summary);
        return { acquisition: "pass", oracle: approval ? "not_required" : "pass", manual: "pending" };
      } catch (error) { primaryError = error; throw error; }
      finally {
        if (state.live !== null) {
          const cleanup = await state.live.close();
          state.inputCleanup = cleanup.input; state.commandCleanup = cleanup.commands;
          if (primaryError === null && (cleanup.input?.failure || cleanup.commands?.failure)) throw new DesktopE2eError("harness", "case1-input-cleanup", "manual interaction probes did not settle", cleanup);
        }
      }
    },
    async quiesce() {
      state.quiesced = true;
      return { input: state.inputCleanup?.failure || state.commandCleanup?.failure ? "fail" : "pass", resources: [{ kind: "external-provider", owned_by_scenario: false, cleanup_action: "none" }] };
    },
    async cleanup() {
      return { input: state.quiesced && !state.inputCleanup?.failure && !state.commandCleanup?.failure ? "pass" : "fail", resources: [{ kind: "case1-interaction", input_cleanup: state.inputCleanup, command_cleanup: state.commandCleanup }] };
    },
  });
}

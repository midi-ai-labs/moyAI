import crypto from "node:crypto";
import { readFile, readdir } from "node:fs/promises";
import { canonicalUlid } from "../core/canonical_identity.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";
import { observeProviderTurnSurface } from "./provider_restart.mjs";
import { ManualLiveSession, manualLiveFixtureConfig, manualLiveHistory, manualLiveGeneratedFiles,
  manualLiveExportTranscript, manualLiveTerminalDecision } from "../drivers/manual_live_session.mjs";

export function createManualTextCase({ id, options, specPath, stages, outputs, prepareWorkspace = async () => {},
  checkStage = async () => [], manualReview, fixtureConfig = manualLiveFixtureConfig(options), beforeStages = async () => {},
  observationTimeoutMs = 15 * 60 * 1000 }) {
  const stem = id.replace(/^manual\./, "");
  const owner = `scenario:${id}`;
  const caseTimeoutMs = options.observationTimeoutMs ?? observationTimeoutMs;
  const state = { steps: [], baseline: null, live: null, cleanup: null, quiesced: false };
  return Object.freeze({ id, productOracle: "pass", manualGate: "pending", databaseRequired: true, requestGracefulExit,
    async prepare({ context, sink, phase }) {
      for (const name of ["workspace", "config", "data"]) if ((await readdir(context.paths[name])).length) throw new DesktopE2eError("harness", `${stem}-not-fresh`, "manual case requires fresh workspace/config/data", {});
      const spec = await readFile(specPath);
      state.steps = stages(spec.toString("utf8"));
      await prepareDesktopFixture({ context, sink, phase, owner, configText: fixtureConfig, sentinelName: null, sentinelText: "" });
      await prepareWorkspace({ context, sink, options, owner, stem, spec: spec.toString("utf8"), phase });
      state.baseline = await manualLiveGeneratedFiles(context, sink, `${stem}/baseline`, outputs);
      await sink.record(`${stem}-input`, { options, spec: { path: specPath, sha256: crypto.createHash("sha256").update(spec).digest("hex") },
        requests: state.steps, baseline: state.baseline, observation_timeout_ms: caseTimeoutMs,
        provider_lifecycle: "external-unmanaged", provider_owned: false }, { phase, owner });
    },
    async execute({ context, driver, sink }) {
      let primaryError = null;
      try {
        await acquireInteractiveShell({ context, driver, sink }, { evidenceOwner: owner, screenshotStem: `${stem}-ready` });
        const initial = (await observeProviderTurnSurface(driver)).projection;
        if (initial.provider_effective_profile !== "openai_compatible" || initial.provider_effective_base_url !== options.providerBaseUrl
          || initial.provider_effective_model_id !== options.model || initial.provider_effective_api_key_env !== options.apiKeyEnv
          || Number(initial.provider_effective_context_window) !== 131072) throw new DesktopE2eError("product", `${stem}-provider-config`, "effective provider differs from explicit manual inputs", {});
        await beforeStages({ context, driver, sink, options, owner, stem });
        state.live = new ManualLiveSession({ context, driver, sink, options, owner, stem, capturePaths: outputs, observationTimeoutMs: caseTimeoutMs });
        await state.live.open();
        const acquired = [];
        const failures = [];
        let firstSession = null;
        let last = null;
        let previous = state.baseline;
        for (const step of state.steps) {
          const result = await state.live.send(step.prompt, { stage: step.name });
          last = result;
          const projection = result.terminal.projection;
          const sessionId = projection.run_target?.sessionId;
          const turnId = projection.run_target?.expectedState?.latestTurnId;
          if (!canonicalUlid(sessionId) || !canonicalUlid(turnId)) throw new DesktopE2eError("harness", `${stem}-terminal-owner`, "manual terminal has no exact owner", {});
          firstSession ??= sessionId;
          if (sessionId !== firstSession || acquired.some(row => row.turn_id === turnId)) failures.push(`${step.name}:session-or-turn-owner-changed`);
          const history = manualLiveHistory(projection);
          const expected = state.steps.slice(0, acquired.length + 1).map(row => row.prompt);
          if (JSON.stringify(history.users) !== JSON.stringify(expected)) failures.push(`${step.name}:canonical-user-requests-mismatch`);
          const generated = await manualLiveGeneratedFiles(context, sink, `${stem}/${step.name}/generated`, outputs);
          const evidence = await sink.writeJson(`${stem}/${step.name}/projection.json`, result.terminal);
          const screenshot = await captureScenarioScreenshot({ cdp: driver, sink, name: `${stem}-${step.name}-terminal`, owner });
          const row = { name: step.name, session_id: sessionId, turn_id: turnId, diagnostics: {
            status_message: projection.status_message, status_detail: projection.status_detail, run_status_key: projection.run_status_key },
            history, generated, approvals: result.approvals, incomplete: result.incomplete, incomplete_reason: result.incompleteReason, evidence, screenshot };
          acquired.push(row);
          if (result.incomplete) break;
          if (manualLiveTerminalDecision(result.terminal) !== "completed") { failures.push(`${step.name}:desktop-terminal-not-completed`); break; }
          if (failures.length) break;
          failures.push(...await checkStage({ context, sink, options, owner, stem, step, row, baseline: state.baseline, previous, generated,
            observationBudget: state.live.observationBudget }));
          state.live.observationBudget.assertRemaining(`${stem} ${step.name} verification`);
          previous = generated;
          if (failures.length) break;
        }
        const transcript = await manualLiveExportTranscript({ context, input: state.live.input, sink, owner, stem,
          sessionId: firstSession, prompt: state.steps[0].prompt, observationBudget: state.live.observationBudget });
        state.live.observationBudget.assertRemaining(`${stem} case completion`);
        const summary = { schema_version: "desktop-e2e.manual-text-case.v1", scenario: id, approval_mode: options.approvalMode,
          machine_gate: last.incomplete ? "operator_review_incomplete" : failures.length ? "fail" : "pass",
          stages: acquired, transcript, failures, observation_budget: state.live.observationBudget.snapshot(), manual_review: manualReview, manual_verdict: "pending",
          external_verification: last.incomplete || failures.some(value => value.includes("desktop-terminal")) ? "not_reached" : "see_stage_evidence",
          provider_cleanup: "none_external_unmanaged", workspace_outside_scope: "manual_public_evidence_review_pending" };
        await sink.writeJson(`${stem}/summary.json`, summary);
        await sink.record(`${stem}-machine-gate`, summary, { phase: "executing", owner });
        if (failures.length) throw new DesktopE2eError("product", `${stem}-machine-gate`, "manual case failed acquired predicates", summary);
        return { acquisition: "pass", oracle: last.incomplete ? "not_required" : "pass", manual: "pending" };
      } catch (error) { primaryError = error; throw error; }
      finally {
        if (state.live !== null) {
          state.cleanup = await state.live.close();
          if (primaryError === null && (state.cleanup.input?.failure || state.cleanup.commands?.failure)) throw new DesktopE2eError("harness", `${stem}-input-cleanup`, "manual interaction probes did not settle", state.cleanup);
        }
      }
    },
    async quiesce() { state.quiesced = true; return { input: state.cleanup?.input?.failure || state.cleanup?.commands?.failure ? "fail" : "pass", resources: [{ kind: "external-provider", owned_by_scenario: false, cleanup_action: "none" }] }; },
    async cleanup() { return { input: state.quiesced && !state.cleanup?.input?.failure && !state.cleanup?.commands?.failure ? "pass" : "fail", resources: [{ kind: "manual-interaction", cleanup: state.cleanup }] }; },
  });
}

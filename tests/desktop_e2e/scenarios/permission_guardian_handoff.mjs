import path from "node:path";
import { readFile } from "node:fs/promises";
import { DesktopE2eError } from "../core/execution.mjs";
import { canonicalU64 } from "../core/canonical_identity.mjs";
import { DesktopCommandProbe, assertExactDesktopCommandSequence } from "../drivers/desktop_command_probe.mjs";
import { WebviewInput } from "../drivers/webview_input.mjs";
import { createPermissionRestartGuardianProviderScript, startScriptedProvider } from "../drivers/scripted_provider.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { quiesceProviderResource } from "./provider_restart.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";
import { action, wait, trustedClick } from "./hub_browser_enrollment.mjs";
import { observeReceiverPermissionSurface } from "./mcp_receiver_permission.mjs";
import { observeRunNextTurnSurface, settledComposer } from "./run_next_turn.mjs";
import { exactPermissionRestartGuardianLedger, permissionRestartGuardianFixtureConfig, submitGuardianPrompt } from "./permission_restart_guardian.mjs";

const DIALOG = '[role="alertdialog"][data-permission-id]';
const SEED_PROMPT = "seed guardian authority";
const SEED_RESPONSE = "HANDOFF_AUTHORITY_READY";
const TASK_PROMPT = "review the fixture command";
const RESPONSE = "HUMAN_APPROVED_COMMAND_COMPLETE";
const RECEIPT = "HUMAN_APPROVED_EFFECT";
const ROLES = ["guardian_seed", "guardian_tool_initial", "guardian_review", "guardian_continuation"];
const fail = (message, evidence) => new DesktopE2eError("product", "guardian-handoff-mismatch", message, evidence);

export function guardianHandoffSeedReady({ surface, ledger }) {
  return exactPermissionRestartGuardianLedger(ledger, ROLES.slice(0, 1))
    && settledComposer(surface)
    && surface.prompt.value === "" && surface.send.enabled === false
    && surface.projection.transcript_rows.some(row => row.row_kind === "assistant" && row.body === SEED_RESPONSE);
}

export function guardianHandoffPlan(root, decision) {
  if (!path.isAbsolute(root) || !["approve", "abort"].includes(decision)) throw new TypeError("handoff requires an absolute fixture workspace and approve/abort");
  const receiptPath = path.join(root, "guardian-approved-effect.txt");
  const quoted = `'${receiptPath.replaceAll("'", "''")}'`;
  const command = [
    ...Array.from({ length: 64 }, (_, index) => `# Review line ${index + 1}: this fixture only appends one local receipt.`),
    `[System.IO.File]::AppendAllText(${quoted}, '${RECEIPT}' + [Environment]::NewLine)`,
    "Write-Output HUMAN_APPROVED_EFFECT_OK",
  ].join("\n");
  return { decision, id: `permission.guardian-handoff-${decision}`, receiptPath, command,
    responseText: RESPONSE, receiptText: `${RECEIPT}\n` };
}

async function receipt(pathname) {
  try { return (await readFile(pathname, "utf8")).replaceAll("\r\n", "\n"); }
  catch (error) { if (error?.code === "ENOENT") return null; throw error; }
}

export function guardianHandoffPendingFailures(sample, command) {
  const p = sample?.surface?.projection, d = sample?.surface?.dialog, failures = [];
  if (!exactPermissionRestartGuardianLedger(sample?.ledger, ROLES.slice(0, 3))) failures.push("provider-not-awaiting-human");
  if (sample?.receipt !== null) failures.push("effect-before-human-approval");
  if (!p?.confirmation_visible || !canonicalU64(p.confirmation_id) || p.confirmation?.remote) failures.push("local-confirmation-owner");
  if (d?.count !== 1 || !d.visible || d.busy || d.id !== p?.confirmation_id
    || !d.command?.includes(command)) failures.push("confirmation-content");
  const reasonPrefix = "代理承認からの確認: ";
  const rawReason = p?.confirmation?.details?.find(detail => typeof detail === "string" && detail.startsWith(reasonPrefix));
  if (!rawReason?.slice(reasonPrefix.length).trim() || !d?.text?.includes("確認が必要な理由")
    || !d.text.includes(rawReason.slice(reasonPrefix.length))) failures.push("confirmation-reason");
  for (const name of ["approve-permission", "abort-permission"]) {
    const rows = d?.buttons?.filter(row => row.action === name) ?? [];
    if (rows.length !== 1 || !rows[0].enabled || !rows[0].visible) failures.push(`confirmation-button:${name}`);
  }
  if (sample?.surface?.errors !== 0) failures.push("visible-error");
  return failures;
}

export function guardianHandoffTerminalFailures(sample, plan) {
  const p = sample?.surface?.projection, failures = [];
  const approved = plan.decision === "approve";
  if (p?.confirmation_visible || p?.confirmation_id !== null || sample?.surface?.dialog?.count !== 0
    || p?.busy !== false || p?.task_activity_state !== "idle") failures.push("not-settled");
  if (p?.run_status_key !== (approved ? "completed" : "cancelled")) failures.push("terminal-status");
  if (!exactPermissionRestartGuardianLedger(sample?.ledger, approved ? ROLES : ROLES.slice(0, 3))) failures.push("provider-replay-or-continuation-mismatch");
  if (approved ? sample?.receipt !== plan.receiptText : sample?.receipt !== null) failures.push("effect-count");
  const assistants = p?.transcript_rows?.filter(row => row.row_kind === "assistant").map(row => row.body) ?? [];
  if (approved ? assistants.at(-1) !== plan.responseText : assistants.includes(plan.responseText)) failures.push("assistant-result");
  if (sample?.surface?.errors !== 0) failures.push("visible-error");
  return failures;
}

async function footerObservation(cdp) {
  return cdp.evaluate(`(() => {
    const d=document.querySelector(${JSON.stringify(DIALOG)}), footer=d?.querySelector('.permission-footer');
    const rect=n=>{const r=n?.getBoundingClientRect();return r?{top:r.top,bottom:r.bottom,left:r.left,right:r.right}:null;};
    return {viewport:{width:innerWidth,height:innerHeight},footer:rect(footer),buttons:Array.from(footer?.querySelectorAll('button[data-action]')??[]).map(n=>{
      const r=n.getBoundingClientRect(), hit=document.elementFromPoint(r.left+r.width/2,r.top+r.height/2);
      return {action:n.dataset.action,rect:rect(n),reachable:n===hit||n.contains(hit),enabled:!n.disabled};
    })};
  })()`);
}

export function guardianHandoffFooterFailures(value) {
  const failures = [];
  if (!value?.footer || value.footer.top < 0 || value.footer.bottom > value.viewport?.height) failures.push("footer-outside-viewport");
  for (const action of ["approve-permission", "abort-permission"]) {
    const rows = value?.buttons?.filter(row => row.action === action) ?? [];
    if (rows.length !== 1 || !rows[0].enabled || !rows[0].reachable
      || rows[0].rect.top < 0 || rows[0].rect.bottom > value.viewport?.height) failures.push(`button-not-reachable:${action}`);
  }
  return failures;
}

export function createGuardianHandoffScenario(decision = "approve") {
  if (!["approve", "abort"].includes(decision)) throw new TypeError("unknown handoff decision");
  const id = `permission.guardian-handoff-${decision}`, owner = `scenario:${id}`;
  const state = { provider: null, plan: null, acceptedLedger: null, quiesceOutcome: null, cleanupFailures: [] };
  return Object.freeze({
    id, productOracle: "pass", manualGate: "not_required", databaseRequired: true, requestGracefulExit,
    async prepare({ context, sink, phase }) {
      state.plan = guardianHandoffPlan(context.paths.workspace, decision);
      state.provider = await startScriptedProvider({ script: createPermissionRestartGuardianProviderScript({
        seedPrompt: SEED_PROMPT, seedResponseText: SEED_RESPONSE, taskPrompt: TASK_PROMPT,
        command: state.plan.command, justification: "Confirm the exact local receipt operation with the user",
        responseText: RESPONSE, guardianDecision: "ask_user", expectedPermissionRisks: ["unclassified_shell"],
      }) });
      await prepareDesktopFixture({ context, sink, phase, owner,
        configText: permissionRestartGuardianFixtureConfig(state.provider.baseUrl),
        sentinelName: "E2E_GUARDIAN_HANDOFF.txt", sentinelText: "Bounded local permission handoff fixture.\n" });
      await sink.record("guardian-handoff-provider-started", state.provider.resourceObservation(), { phase, owner });
    },
    async execute({ context, driver: cdp, sink }) {
      await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: owner, screenshotStem: "guardian-handoff-shell" });
      const input = new WebviewInput(cdp, { probeId: "guardian-handoff-input" });
      const commands = new DesktopCommandProbe(cdp, { probeId: "guardian-handoff-commands", commands: ["submit_prompt", "answer_permission", "cancel_run"] });
      let primaryError = null;
      const sample = async () => {
        const value = { surface: await observeReceiverPermissionSurface(cdp), ledger: state.provider.requestLedger, receipt: await receipt(state.plan.receiptPath) };
        if (value.ledger.some(row => row.contract?.pass === false || row.response_phase === "rejected")) throw fail("scripted provider contract failed", value);
        return value;
      };
      try {
        await input.installProbe(); await commands.install();
        const seed = await submitGuardianPrompt({ cdp, input, commands, prompt: SEED_PROMPT });
        const seedReady = await wait("Guardian handoff seed owner settles in both backend and composer", async () => ({
          surface: await observeRunNextTurnSurface(cdp), ledger: state.provider.requestLedger,
        }), guardianHandoffSeedReady);
        await sink.record("guardian-handoff-seed-ready", seedReady, { phase: "executing", owner });
        const task = await submitGuardianPrompt({ cdp, input, commands, prompt: TASK_PROMPT });
        const pending = await wait("Guardian asks the real Desktop user without executing", sample,
          value => guardianHandoffPendingFailures(value, state.plan.command).length === 0);
        const footer = await footerObservation(cdp);
        if (guardianHandoffFooterFailures(footer).length) throw fail("long approval hides decision buttons", footer);
        const screenshot = await captureScenarioScreenshot({ cdp, sink, name: "guardian-handoff-long-approval", owner });
        const unanswered = assertExactDesktopCommandSequence(await commands.snapshot(), { expected: [seed.expected, task.expected] });
        await sink.record("guardian-handoff-awaiting-user", { pending, footer, unanswered, screenshot }, { phase: "executing", owner });
        await trustedClick(input, cdp, action(`${decision === "approve" ? "approve" : "abort"}-permission`, DIALOG), sink);
        const expected = [seed.expected, task.expected, { command: "answer_permission", args: {
          decision: decision === "approve" ? "approved" : "abort", confirmationId: pending.surface.projection.confirmation_id,
        } }];
        await wait("one exact human decision command", () => commands.snapshot(), value => value.calls.length >= expected.length);
        assertExactDesktopCommandSequence(await commands.snapshot(), { expected });
        const terminal = await wait("Guardian handoff outcome settles", sample,
          value => guardianHandoffTerminalFailures(value, state.plan).length === 0, 45_000);
        // Wait through additional ordinary polls: a denial must not start another route,
        // and an approval must never execute the same command twice.
        let observations = 0;
        const stable = await wait("Guardian handoff stays settled without replay", sample, value => {
          const failures = guardianHandoffTerminalFailures(value, state.plan);
          if (failures.length) throw fail("handoff outcome changed after terminal", { failures, value });
          return ++observations >= 4;
        });
        const commandProof = assertExactDesktopCommandSequence(await commands.snapshot(), { expected });
        state.acceptedLedger = structuredClone(stable.ledger);
        await sink.record("guardian-handoff-completed", { decision, terminal, stable, commandProof,
          receipt_path: state.plan.receiptPath, receipt: stable.receipt,
          screenshot: await captureScenarioScreenshot({ cdp, sink, name: "guardian-handoff-terminal", owner }),
        }, { phase: "executing", owner });
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) { primaryError = error; throw error; }
      finally {
        for (const [name, cleanup] of [["input", () => input.cleanup()], ["commands", () => commands.remove()]]) {
          try { await cleanup(); } catch (error) { state.cleanupFailures.push({ name, message: error.message }); }
        }
        if (!primaryError && state.cleanupFailures.length) throw new DesktopE2eError("harness", "guardian-handoff-probe-cleanup", "handoff probes did not settle", state.cleanupFailures);
      }
    },
    async quiesce({ inputs }) {
      state.quiesceOutcome ??= await quiesceProviderResource({ provider: state.provider, acceptedLedger: state.acceptedLedger, inputs });
      return structuredClone(state.quiesceOutcome);
    },
    async cleanup() {
      return { input: state.quiesceOutcome?.input === "pass" && !state.cleanupFailures.length ? "pass" : "fail",
        resources: [{ kind: "guardian-human-handoff", decision, quiesce: state.quiesceOutcome?.input, probe_failures: state.cleanupFailures }] };
    },
  });
}

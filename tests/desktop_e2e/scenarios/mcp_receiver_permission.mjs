import path from "node:path";
import { readFile } from "node:fs/promises";
import { isDeepStrictEqual } from "node:util";
import { canonicalUlid } from "../core/canonical_identity.mjs";
import { waitForSemanticTargetSettlement } from "../core/semantic_target_settlement.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { DesktopCommandProbe, assertExactDesktopCommandSequence } from "../drivers/desktop_command_probe.mjs";
import { assertTrustedProbeSequence } from "../drivers/webview_input.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";
import { action, wait, trustedClick } from "./hub_browser_enrollment.mjs";
import { trustedInsert } from "./side_chat_quote.mjs";

const DIALOG = '[role="alertdialog"][data-permission-id]';
const PROMPT = { selector: "textarea#prompt", identity: { tag: "TEXTAREA", id: "prompt" } };
const DRAFT = "Unsent Main draft survives receiver permission";
const RECEIPT = "REMOTE_APPROVAL_FILE";
const DECISIONS = Object.freeze({ approved: "approve", denied: "deny", abort: "abort" });
const fail = (message, evidence = {}) => new DesktopE2eError("product", "mcp-receiver-permission-mismatch", message, evidence);

export function receiverPermissionScenarioId(decision) {
  if (!Object.hasOwn(DECISIONS, decision)) throw new TypeError("Receiver permission decision must be approved, denied, or abort");
  return `mcp.receiver-${DECISIONS[decision]}`;
}

export function receiverPermissionPlan(root, decision) {
  receiverPermissionScenarioId(decision);
  if (!path.isAbsolute(root)) throw new TypeError("Receiver receipt root must be an absolute owned fixture path");
  const receiptPath = path.join(root, "remote-approval-receipt.txt");
  const quotedPath = `'${receiptPath.replaceAll("'", "''")}'`;
  return {
    decision, receiptPath, receiptText: RECEIPT,
    call: {
      prompt: `Receiver permission ${decision} GUI test`, name: "shell",
      arguments: {
        command: `[System.IO.File]::WriteAllText(${quotedPath}, '${RECEIPT}'); Write-Output 'REMOTE_APPROVAL_OK'`,
        timeout_ms: 5000, description: "Write the owned GUI test receipt once",
        sandbox_permissions: "require_escalated", justification: "One explicitly reviewed GUI test receipt in the owned test folder",
      },
      // Shell output also echoes Command, so the real receipt is independently
      // checked. The provider marker alone is never proof that the effect ran.
      outputMarker: decision === "denied" ? "permission denied by receiver user" : "Stdout:\nREMOTE_APPROVAL_OK",
      responseText: `REMOTE_PERMISSION_${decision.toUpperCase()}_COMPLETE`,
    },
  };
}

function mainIdentity(surface) {
  const p = surface.projection;
  return {
    workspace: p.workspace_path, draftTarget: p.draft_target, runTarget: p.run_target,
    transcript: p.transcript_rows.map(row => ({ kind: row.row_kind, identity: row.stable_history_identity, body: row.body })),
    draft: surface.prompt, runStatus: p.run_status_key,
  };
}

export function receiverPermissionMainUnchanged(surface, expected) {
  return !!surface?.projection && Array.isArray(surface.projection.transcript_rows)
    && isDeepStrictEqual(mainIdentity(surface), expected) && surface.projection.busy === false;
}

export function receiverPermissionLedgerReady(ledger, phase) {
  if (!Array.isArray(ledger) || !["pending", "held", "completed", "abort"].includes(phase)) return false;
  if (ledger.some(row => !["models", "chat_completions"].includes(row?.route))) return false;
  if (ledger.some(row => row.route === "models" && (row.method !== "GET" || row.pathname !== "/v1/models" || row.response_status !== 200))) return false;
  const rows = ledger.filter(row => row.route === "chat_completions");
  const roles = phase === "pending" || phase === "abort" ? ["chat_tool_initial"] : ["chat_tool_initial", "chat_continuation"];
  return rows.length === roles.length && rows.every((row, index) => {
    const held = phase === "held" && index === 1;
    return row.method === "POST" && row.pathname === "/v1/chat/completions" && row.query_present === false
      && row.contract?.pass === true && row.contract.role === roles[index]
      && row.response_phase === (held ? "held" : "completed") && row.response_status === (held ? null : 200);
  });
}

export function receiverPermissionPendingFailures(sample, expected) {
  const { surface, job, ledger } = sample ?? {};
  const p = surface?.projection, remote = p?.confirmation?.remote, failures = [];
  if (!receiverPermissionMainUnchanged(surface, expected.main)) failures.push("main-owner-or-draft-changed");
  if (job?.state !== "awaiting_approval" || !receiverPermissionLedgerReady(ledger, "pending")) failures.push("runtime-not-waiting-for-human");
  if (sample?.receipt !== null) failures.push("effect-before-approval");
  if (p?.confirmation_visible !== true || !canonicalUlid(p?.confirmation_id)
    || remote?.job_id !== expected.jobId || remote?.profile_id !== expected.profileId || !canonicalUlid(remote?.session_id)
    || remote?.target_label !== "temp" || remote?.requester_label !== expected.requesterLabel) failures.push("permission-owner");
  if (surface?.dialog?.count !== 1 || surface.dialog.id !== p?.confirmation_id || !surface.dialog.visible
    || surface.dialog.title !== "受入タスクの操作を確認" || surface.dialog.busy || !surface.shellInert
    || surface.dialog.focusedAction !== "deny-permission") failures.push("permission-dialog");
  if (!surface?.dialog?.command?.includes(expected.command) || !surface.dialog.text?.includes(expected.jobId)
    || !surface.dialog.text.includes(expected.requesterLabel)) failures.push("permission-description");
  const labels = { "deny-permission": "許可しない", "abort-permission": "タスクを停止", "approve-permission": "この操作を許可" };
  if (surface?.dialog?.buttons?.length !== 3) failures.push("permission-button-count");
  for (const [id, label] of Object.entries(labels)) {
    const matches = surface?.dialog?.buttons?.filter(button => button.action === id) ?? [];
    if (matches.length !== 1 || matches[0].label !== label || !matches[0].visible || !matches[0].enabled) failures.push(`permission-button:${id}`);
  }
  if (p?.mcp_activity?.awaiting_approval !== 1 || surface?.awaitingBadge !== 1) failures.push("awaiting-indicator");
  if (surface?.errors !== 0) failures.push("visible-error");
  return failures;
}

export function receiverPermissionTerminalFailures(sample, expected) {
  const { surface, job, ledger, receipt } = sample ?? {}, failures = [];
  if (!receiverPermissionMainUnchanged(surface, expected.main)) failures.push("main-owner-or-draft-changed");
  const aborted = expected.decision === "abort";
  if (job?.state !== (aborted ? "interrupted" : "completed") || (!aborted && job?.result !== expected.responseText)) failures.push("remote-result");
  if (!receiverPermissionLedgerReady(ledger, aborted ? "abort" : "completed")) failures.push("provider-sequence");
  if (expected.decision === "approved" ? receipt !== RECEIPT : receipt !== null) failures.push("receipt-effect");
  const p = surface?.projection;
  if (p?.confirmation_visible !== false || p?.confirmation_id !== null || p?.confirmation != null
    || surface?.dialog?.count !== 0 || surface?.shellInert || surface?.promptEnabled !== true) failures.push("confirmation-not-settled");
  if (surface?.errors !== 0) failures.push("visible-error");
  return failures;
}

export async function observeReceiverPermissionSurface(cdp) {
  const projection = await invokeDesktopCommand(cdp, "desktop_state");
  return { projection, ...await cdp.evaluate(`(() => {
    const visible=n=>{if(!(n instanceof HTMLElement))return false;const r=n.getBoundingClientRect(),s=getComputedStyle(n);return r.width>0&&r.height>0&&s.display!=='none'&&s.visibility!=='hidden'&&Number(s.opacity)!==0;};
    const dialogs=Array.from(document.querySelectorAll(${JSON.stringify(DIALOG)})),d=dialogs.length===1?dialogs[0]:null;
    const p=document.querySelector('textarea#prompt'),shell=document.querySelector('.app-frame > .shell');
    return {prompt:p?.value??null,promptEnabled:!!p&&!p.disabled&&!p.closest('[inert]'),
      shellInert:!!shell&&(shell.matches('[inert]')||shell.getAttribute('aria-hidden')==='true'),
      awaitingBadge:document.querySelectorAll('.mcp-run-strip [data-mcp-activity="attention"]').length,
      errors:Array.from(document.querySelectorAll('.fatal,.ui-error-notice')).filter(visible).length,
      dialog:{count:dialogs.length,id:d?.dataset.permissionId??null,visible:visible(d),busy:d?.getAttribute('aria-busy')==='true',focusedAction:d?.contains(document.activeElement)?document.activeElement?.dataset.action:null,
        title:d?.querySelector('#permission-title')?.textContent?.trim(),command:d?.querySelector('.confirm-command')?.textContent??null,text:d?.textContent??null,
        buttons:Array.from(d?.querySelectorAll('button[data-action]')??[]).map(n=>({action:n.dataset.action,label:n.textContent.trim(),visible:visible(n),enabled:!n.disabled&&n.getAttribute('aria-disabled')!=='true'}))}};
  })()`) };
}

export async function prepareReceiverPermissionMain({ cdp, input, sink, owner }) {
  await wait("Main composer is interactive after closing Hub settings", () => observeReceiverPermissionSurface(cdp),
    value => value.projection.overlay === "none" && value.promptEnabled && value.dialog.count === 0);
  const ready = await waitForSemanticTargetSettlement({ input, locator: PROMPT, label: "Receiver permission Main draft target" });
  if (ready.value.classified.decision !== "pass") throw fail("Main draft target did not settle", ready.value);
  const typed = await trustedInsert(input, PROMPT, DRAFT);
  const surface = await wait("Receiver permission fixture keeps an unsent Main draft", () => observeReceiverPermissionSurface(cdp),
    value => value.prompt === DRAFT && value.projection.overlay === "none" && value.projection.busy === false);
  await sink.record("receiver-permission-unsent-main", { typed, main: mainIdentity(surface) }, { phase: "executing", owner });
  return mainIdentity(surface);
}

async function readReceipt(file) {
  try { return await readFile(file, "utf8"); }
  catch (error) { if (error?.code === "ENOENT") return null; throw error; }
}

export async function executeReceiverPermissionDecision({ state, cdp, input, sink, owner, profileId, requesterLabel }) {
  const plan = state.permissionPlan;
  const expected = { main: state.permissionMain, jobId: state.jobId, profileId, requesterLabel,
    command: plan.call.arguments.command, decision: plan.decision, responseText: plan.call.responseText };
  const sample = async () => ({ surface: await observeReceiverPermissionSurface(cdp), job: await state.peer.status(state.jobId),
    ledger: state.provider.requestLedger, receipt: await readReceipt(plan.receiptPath) });
  const pending = await wait("Actual receiver requests an explicit decision on the exact remote job", sample,
    value => receiverPermissionPendingFailures(value, expected).length === 0, 45_000);
  if (pending.receipt !== null) throw fail("Receiver effect ran before approval", { receipt: pending.receipt });
  const commands = state.commands = new DesktopCommandProbe(cdp, { probeId: "mcp-receiver-permission", commands: ["answer_permission"] });
  await commands.install();
  const escapeStart = (await input.snapshotProbe()).sequence;
  await input.pressKey("Escape");
  const escape = assertTrustedProbeSequence(await input.snapshotProbe(escapeStart), { afterSequence: escapeStart, expected: [
    { type: "keydown", key: "Escape", identity: { tag: "BUTTON", action: "deny-permission" } },
    { type: "keyup", key: "Escape", identity: { tag: "BUTTON", action: "deny-permission" } },
  ] });
  const stillPending = await wait("Escape leaves the remote permission unanswered", sample,
    value => value.surface.projection.confirmation_id === pending.surface.projection.confirmation_id
      && receiverPermissionPendingFailures(value, expected).length === 0);
  const ignoredEscape = assertExactDesktopCommandSequence(await commands.snapshot(), { expected: [] });
  await sink.record("receiver-permission-escape-ignored", { expected, escape, ignoredEscape, pending: stillPending }, { phase: "executing", owner });
  await captureScenarioScreenshot({ cdp, sink, name: `mcp-receiver-${plan.decision}-confirmation`, owner });
  await trustedClick(input, cdp, action(`${DECISIONS[plan.decision]}-permission`, DIALOG), sink);
  const target = { decision: plan.decision, confirmationId: pending.surface.projection.confirmation_id,
    remoteJobId: state.jobId, remoteProfileId: profileId };
  await wait("GUI sends the exact remote permission answer", () => commands.snapshot(), value => value.calls.length > 0);
  const proof = assertExactDesktopCommandSequence(await commands.snapshot(), { expected: [{ command: "answer_permission", args: target }] });
  if (plan.decision !== "abort") {
    const held = await wait("Receiver applies only the chosen permission decision before continuing", sample,
      value => receiverPermissionLedgerReady(value.ledger, "held") && (plan.decision === "approved" ? value.receipt === RECEIPT : value.receipt === null));
    await sink.record("receiver-permission-effect-before-model-reply", { decision: plan.decision, held }, { phase: "executing", owner });
    state.provider.releaseScriptRole("chat_continuation"); state.released = true;
  }
  const terminal = await wait("Remote permission result settles without changing Main", sample,
    value => receiverPermissionTerminalFailures(value, expected).length === 0, 45_000);
  state.settled = true;
  const finalProof = assertExactDesktopCommandSequence(await commands.snapshot(), { expected: [{ command: "answer_permission", args: target }] });
  await sink.record("receiver-permission-decision-complete", { expected, proof, finalProof, terminal,
    fixture_effect: "The explicitly reviewed shell writes only this owned test receipt; denied/aborted variants must not create it",
    receipt_path: plan.receiptPath }, { phase: "executing", owner });
  await commands.remove(); state.commands = null;
  return { finished: terminal.job, pending, expected };
}

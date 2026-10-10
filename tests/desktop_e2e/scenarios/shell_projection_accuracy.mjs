import crypto from "node:crypto";
import path from "node:path";
import { lstat, readFile } from "node:fs/promises";
import { isDeepStrictEqual } from "node:util";
import { waitForObservation } from "../core/deadline.mjs";
import { canonicalU64, canonicalUlid } from "../core/canonical_identity.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { assertExactDesktopCommandSequence } from "../drivers/desktop_command_probe.mjs";
import { WebviewInput } from "../drivers/webview_input.mjs";
import { manualLiveClick, manualLiveExportTranscript, manualLivePermissionLocator,
  manualLivePermissionRequest, manualLiveTranscriptResult } from "../drivers/manual_live_session.mjs";
import { runReadOnlySqlite } from "./permission_temp_escalation.mjs";
import { captureScenarioScreenshot, observePermissionSurface } from "./observations.mjs";
import { acquireInteractiveShell } from "./shell_baseline.mjs";
import { createProviderChatToolContinuationScenario, exactChatToolContinuationLedger,
  exactTurnOwner, observeChatToolContinuationSurface, terminalSettled } from "./provider_chat_tool_continuation.mjs";

export const SHELL_PROJECTION_FIXTURE_NAME = "shell_sgr_fixture.py";
const OUTPUT_MAX_BYTES = 2048;
const EXPORT = { selector: 'header.topbar button[data-action="export-transcript"]', identity: { tag: "BUTTON", action: "export-transcript" } };
const sha256 = bytes => crypto.createHash("sha256").update(bytes).digest("hex");
const rows = (p, kind) => (p?.transcript_rows ?? []).filter(row => row.row_kind === kind);
const normalize = text => text.replace(/\s+/gu, " ").trim();
const fail = (code, message, evidence) => new DesktopE2eError("product", code, message, evidence);

function variant(mode) {
  if (!["success", "failure"].includes(mode)) throw new TypeError("unknown shell projection variant");
  const success = mode === "success";
  return { success, marker: success ? "SGR_STDOUT_OK" : "SGR_STDERR_FAIL", stream: success ? "Stdout" : "Stderr",
    code: success ? 0 : 1, color: success ? "32" : "31", status: success ? "完了" : "失敗",
    prompt: `Run the bounded ${mode} SGR fixture once, then report its observed result.`,
    response: success ? "SGR_SUCCESS_RECORDED" : "SGR_FAILURE_RECORDED" };
}

export function shellProjectionAccuracyFixture(mode) {
  const v = variant(mode);
  return `import sys\n\nsys.${v.success ? "stdout" : "stderr"}.write(chr(27) + chr(91) + "${v.color}m${v.marker}" + chr(27) + chr(91) + "0m" + "\\n")\nsys.exit(${v.code})\n`;
}

export function shellProjectionAccuracyCall(context, mode) {
  const v = variant(mode);
  // Keep the actual range operator and quoted Python data that exposed the two
  // classifier errors. The Python-c expression is still explicitly reviewed.
  return { prompt: v.prompt, name: "shell", arguments: {
    command: `1..${v.success ? 5 : 10}|Out-Null; python -c "'g2.move_player'"; python -B -m shell_sgr_fixture`,
    workdir: context.paths.workspace,
  }, outputMarker: v.success ? v.marker : "lifecycle_status: completed\nkind: process_exit_nonzero\nautomatic_retry: false\nexit_code: 1",
  outputMaxBytes: OUTPUT_MAX_BYTES, responseText: v.response };
}

export function shellProjectionHasSgr(text) {
  return typeof text === "string" && /\u001b|\u009b|\[(?:[0-9]{1,3};)*[0-9]{0,3}m/u.test(text);
}

export function shellProjectionPermissionFailures(sample, call, { includeDom = true } = {}) {
  const surface = sample?.surface, p = surface?.projection, d = surface?.dialog;
  const failures = [];
  if (exactTurnOwner(p, "turn") === null || p?.confirmation_visible !== true
    || !canonicalU64(p.confirmation_id) || p.confirmation?.remote) failures.push("permission-owner-mismatch");
  if (p?.confirmation?.outside_workspace !== false
    || !isDeepStrictEqual(p?.confirmation?.risks, ["unclassified dynamic/indirect shell construct"])) failures.push("permission-risk-mismatch");
  if (!p?.confirmation?.details?.includes(`Command: ${call.arguments.command}`)) failures.push("permission-command-mismatch");
  if (includeDom) {
    if (d?.count !== 1 || !d.visible || d.busy || d.id !== p?.confirmation_id
      || !d.command?.includes(call.arguments.command)) failures.push("permission-command-mismatch");
    for (const action of ["approve-permission", "abort-permission"]) {
      const buttons = d?.buttons?.filter(button => button.action === action) ?? [];
      if (buttons.length !== 1 || !buttons[0].visible || !buttons[0].enabled) failures.push(`permission-button:${action}`);
    }
  }
  const first = sample?.ledger?.[0];
  if (sample?.ledger?.length !== 1 || first?.method !== "POST" || first.route !== "chat_completions"
    || first.contract?.pass !== true || first.contract.role !== "chat_tool_initial"
    || first.response_phase !== "completed" || first.response_status !== 200) failures.push("effect-continuation-before-approval");
  if (surface?.errors !== 0) failures.push("visible-permission-error");
  return failures;
}

export function shellProjectionPermissionDecision(sample, call) {
  const surface = sample?.surface, p = surface?.projection, d = surface?.dialog;
  if (surface?.errors > 0) return "fail";
  if (p?.confirmation_visible !== true) return "pending";
  if (shellProjectionPermissionFailures(sample, call, { includeDom: false }).length) return "fail";
  // desktop_state may expose a new confirmation before the ordinary renderer
  // has inserted its dialog. Wait within the existing observation deadline.
  if (d?.count === 0) return "pending";
  if (d?.count !== 1 || d.id !== p.confirmation_id) return "fail";
  if (d.visible !== true || d.busy === true) return "pending";
  return shellProjectionPermissionFailures(sample, call).length === 0 ? "pass" : "fail";
}

function blockingFailure(surface) {
  return surface?.visible_fatal_count > 0 || surface?.visible_recoverable_error_count > 0
    || surface?.visible_validation_error_count > 0 || surface?.projection?.startup?.status === "failed"
    || ["failed", "cancelled", "incomplete"].includes(surface?.projection?.run_status_key)
    || surface?.projection?.confirmation_visible === true;
}

function progressHistoryText(projection) {
  if (typeof projection?.progress_text !== "string" || typeof projection.run_phase !== "string" || !projection.run_phase
    || typeof projection.run_active_step !== "string" || !projection.run_active_step) return null;
  const lines = projection.progress_text.split("\n");
  if (lines.length < 6 || lines[1] !== `フェーズ: ${projection.run_phase}` || lines[2] !== `手順: ${projection.run_active_step}`) return null;
  // Phase and step describe this run or load; every other progress line is history.
  return [lines[0], ...lines.slice(3)].join("\n");
}

export function shellProjectionAccuracyFailures(sample, { mode, phase = "held", heldProjection, previousProjection = null } = {}) {
  const v = variant(mode), held = phase === "held", surface = sample?.surface, p = surface?.projection;
  const owner = exactTurnOwner(p, held ? "turn" : "idle"), failures = [];
  const users = rows(p, "user"), assistants = rows(p, "assistant"), errors = rows(p, "error");
  const summaries = rows(p, held ? "work_summary_running" : "work_summary_completed");
  if (!exactChatToolContinuationLedger(sample?.ledger, ["completed", held ? "held" : "completed"], OUTPUT_MAX_BYTES)) failures.push("provider-ledger-not-exact");
  if (blockingFailure(surface) || owner === null || (held
    ? p?.busy !== true || p.run_status_key !== "running" || p.task_activity_state !== "running"
    : !terminalSettled(surface))) failures.push("run-owner-mismatch");
  if (heldProjection !== undefined && !isDeepStrictEqual(exactTurnOwner(heldProjection, "turn"), owner)) failures.push("held-terminal-owner-changed");
  if (users.length !== 1 || users[0].body !== v.prompt || !canonicalUlid(users[0].stable_history_identity)) failures.push("user-not-exact");
  if (summaries.length !== 1 || summaries[0].stable_history_identity !== `turn:${owner?.turnId}:work-summary`
    || rows(p, held ? "work_summary_completed" : "work_summary_running").length !== 0
    || !new RegExp(`Exit code: ${v.code}(?:\\s|$)`, "u").test(summaries[0]?.body ?? "")
    || !new RegExp(`${v.stream}:\\s*${v.marker}`, "u").test(summaries[0]?.body ?? "")) failures.push("summary-result-mismatch");
  const progress = `ツール: 1件開始 / ${v.success ? 1 : 0}件完了 / 0件拒否 / 0件キャンセル / ${v.success ? 0 : 1}件失敗`;
  if (!(p?.progress_text ?? "").includes(progress)) failures.push("progress-count-mismatch");
  const progressHistory = progressHistoryText(p), previousProgressHistory = previousProjection === null ? null : progressHistoryText(previousProjection);
  if (progressHistory === null || (previousProjection !== null && previousProgressHistory === null)) failures.push("progress-context-mismatch");
  const statuses = [...(p?.tool_status_text ?? "").matchAll(/^- \[([^\]]+)\] /gmu)].map(match => match[1]);
  if (!isDeepStrictEqual(statuses, [v.status])) failures.push("tool-list-result-mismatch");
  if ([p?.progress_text, p?.tool_status_text, surface?.thread_text, ...(p?.transcript_rows ?? []).map(row => row.body)].some(shellProjectionHasSgr)) failures.push("sgr-in-display-projection");
  if (held || v.success) {
    if (errors.length !== 0 || surface?.errors?.length !== 0) failures.push("unexpected-independent-error-row");
  } else {
    const error = errors[0], visible = surface?.errors?.[0], source = error?.stable_history_identity ?? null;
    if (errors.length !== 1 || (source !== null && !canonicalUlid(source))
      || !/Exit code: 1(?:\r?\n|$)/u.test(error?.body ?? "")
      || !/Stderr:\s*SGR_STDERR_FAIL/u.test(error?.body ?? "")
      || surface?.errors?.length !== 1 || visible?.visible !== true || visible.history_identity !== source
      || typeof visible.text !== "string" || normalize(visible.text) !== normalize(error?.body ?? "")) failures.push("failure-dom-result-mismatch");
  }
  if (held ? assistants.length !== 0 || surface?.assistants?.length !== 0
    : assistants.length !== 1 || assistants[0].body !== v.response || surface?.assistants?.length !== 1
      || surface.assistants[0].text !== v.response || surface.assistants[0].visible !== true
      || surface.assistants[0].history_identity !== assistants[0].stable_history_identity) failures.push("assistant-not-exact");
  if (previousProjection !== null && (!isDeepStrictEqual(exactTurnOwner(previousProjection, "idle"), owner)
    || !isDeepStrictEqual(previousProjection.transcript_rows.map(row => [row.row_kind, row.stable_history_identity, row.body]),
      p?.transcript_rows?.map(row => [row.row_kind, row.stable_history_identity, row.body]))
    || previousProjection.tool_status_text !== p?.tool_status_text || previousProgressHistory !== progressHistory)) failures.push("restart-display-history-changed");
  return failures;
}

export function shellProjectionCanonicalEvidence(rawRows, call, mode) {
  const v = variant(mode), failures = [], parsed = [];
  for (const row of Array.isArray(rawRows) ? rawRows : []) {
    try {
      const payload = JSON.parse(row.payload_json);
      if (payload === null || typeof payload !== "object" || Array.isArray(payload)) throw new TypeError("not a history payload");
      parsed.push({ ...row, payload });
    }
    catch { failures.push("canonical-payload-malformed"); }
  }
  const calls = parsed.filter(row => row.payload.kind === "tool_call"), outputs = parsed.filter(row => row.payload.kind === "tool_output");
  const c = calls[0]?.payload, o = outputs[0]?.payload, metadata = o?.metadata?.tool_metadata;
  if (!Array.isArray(rawRows) || parsed.length !== 2 || calls.length !== 1 || outputs.length !== 1) failures.push("canonical-call-output-cardinality");
  if (!canonicalUlid(c?.call_id) || c?.tool_name !== "shell" || c?.arguments_json !== JSON.stringify(call.arguments)) failures.push("canonical-call-mismatch");
  if (o?.call_id !== c?.call_id || o?.status !== "completed" || o?.success !== v.success
    || o?.metadata?.success !== v.success || metadata?.success !== v.success || metadata?.exit_code !== v.code
    || metadata?.effect_started !== true || metadata?.sandbox !== "unrestricted"
    || typeof o?.output_text !== "string" || !o.output_text.includes(`\u001b[${v.color}m${v.marker}\u001b[0m`)
    || !new RegExp(`${v.stream}:\\s*\\u001b\\[${v.color}m${v.marker}`, "u").test(o.output_text)
    || outputs[0]?.sequence_no <= calls[0]?.sequence_no) failures.push("canonical-raw-result-mismatch");
  return { read_only: true, rows: rawRows, failures, output_sha256: typeof o?.output_text === "string" ? sha256(o.output_text) : null,
    output_size_bytes: typeof o?.output_text === "string" ? Buffer.byteLength(o.output_text) : null };
}

async function readCanonical(context, owner, call, mode) {
  if (!canonicalUlid(owner?.sessionId) || !canonicalUlid(owner?.turnId)) throw fail("shell-projection-canonical-owner", "Canonical read has no exact owner", { owner });
  const result = await runReadOnlySqlite(context.paths.database, `SELECT id, session_id, turn_id, sequence_no, payload_json FROM protocol_history_items
WHERE session_id = '${owner.sessionId}' AND turn_id = '${owner.turnId}'
AND json_extract(payload_json, '$.kind') IN ('tool_call', 'tool_output') ORDER BY sequence_no ASC;`);
  if (result.exit_code !== 0 || result.stderr) throw new DesktopE2eError("harness", "shell-projection-canonical-read", "Read-only canonical query failed", result);
  const evidence = shellProjectionCanonicalEvidence(JSON.parse(result.stdout || "[]"), call, mode);
  if (evidence.rows.some(row => row.session_id !== owner.sessionId || row.turn_id !== owner.turnId)) evidence.failures.push("canonical-owner-mismatch");
  return evidence;
}

async function observeDisplay(cdp, turnId) {
  return cdp.evaluate(`(() => {
    const visible = n => { if (!(n instanceof HTMLElement)) return false; const r=n.getBoundingClientRect(),s=getComputedStyle(n);
      return r.width>0&&r.height>0&&s.display!=='none'&&s.visibility!=='hidden'&&Number(s.opacity)!==0; };
    const rows=[...document.querySelectorAll('#thread article.work-summary')].filter(n=>n.dataset.historyIdentity===${JSON.stringify(`turn:${turnId}:work-summary`)});
    const row=rows.length===1?rows[0]:null, details=row?.querySelector('.message-body > details'), body=row?.querySelector('.work-summary-body');
    const disclosures=row?[...row.querySelectorAll('.message-body > details > summary')]:[], disclosure=disclosures.length===1?disclosures[0]:null;
    const button=document.querySelector('header.topbar button[data-action="export-transcript"]'), status=document.querySelector('header.topbar .status-line > span');
    return {summary:{count:rows.length,history_identity:row?.dataset.historyIdentity??null,disclosure_count:disclosures.length,
      focus_key:disclosure?.dataset.focusKey??null,open:details?.open??false,
      visible:visible(body),text:body?.innerText??null}, export:{visible:visible(button),enabled:!!button&&!button.disabled,
      status_visible:visible(status),status:status?.innerText??null}};
  })()`);
}

export function shellProjectionSummaryTarget(display, turnId) {
  const summary = display?.summary, historyIdentity = `turn:${turnId}:work-summary`, key = summary?.focus_key;
  if (summary?.count !== 1 || summary.history_identity !== historyIdentity || summary.disclosure_count !== 1
    || typeof key !== "string" || key.trim().length === 0) throw new DesktopE2eError("harness", "shell-projection-summary-target",
      "The same-turn work summary has no unique observed disclosure identity", { display, turnId });
  return { selector: `#thread article.work-summary[data-history-identity=${JSON.stringify(historyIdentity)}] > .message-body > details > summary[data-focus-key=${JSON.stringify(key)}]`,
    identity: { tag: "SUMMARY", focusKey: key } };
}

export function shellProjectionVisibleFailures(display, { mode, turnId, exportPath = null } = {}) {
  const v = variant(mode), s = display?.summary, failures = [];
  if (s?.count !== 1 || s.history_identity !== `turn:${turnId}:work-summary` || !s.open || !s.visible
    || !new RegExp(`${v.stream}:\\s*${v.marker}`, "u").test(s.text ?? "")
    || !new RegExp(`Exit code: ${v.code}(?:\\s|$)`, "u").test(s.text ?? "") || shellProjectionHasSgr(s.text)) failures.push("visible-summary-result-mismatch");
  if (exportPath !== null && (!display?.export?.visible || !display.export.enabled || !display.export.status_visible
    || display.export.status !== `会話をMarkdownで保存しました: ${exportPath}`)) failures.push("visible-export-status-mismatch");
  return failures;
}

export function shellProjectionExportFailures(text, { sessionId, call, mode }) {
  const v = variant(mode), failures = [];
  if (!manualLiveTranscriptResult(text, { sessionId, prompt: call.prompt }).pass) failures.push("export-owner-mismatch");
  if (shellProjectionHasSgr(text) || !new RegExp(`${v.stream}:\\s*${v.marker}`, "u").test(text)
    || !text.includes(v.response)) failures.push("export-result-mismatch");
  return failures;
}

export function shellProjectionColdEvidenceFailures(previous, captured) {
  return captured.export.sha256 === previous.export.sha256 && captured.export.size_bytes === previous.export.size_bytes
    && captured.export.source_path === previous.export.source_path && isDeepStrictEqual(captured.fixture, previous.fixture)
    ? [] : ["cold-evidence-changed"];
}

async function openSummary(cdp, input, sink, turnId, owner, stem) {
  const display = await observeDisplay(cdp, turnId);
  if (!display.summary.open) await manualLiveClick(input, shellProjectionSummaryTarget(display, turnId), sink, "open-work-summary", { owner, stem });
}

async function captureTerminal({ context, cdp, sink, sample, call, mode, owner, stem, previous = null }) {
  const turnOwner = exactTurnOwner(sample.surface.projection, "idle"), input = new WebviewInput(cdp, { probeId: `${stem.replaceAll("/", "-")}-input` });
  await input.installProbe();
  try {
    await openSummary(cdp, input, sink, turnOwner.turnId, owner, stem);
    const fixturePath = path.join(context.paths.workspace, SHELL_PROJECTION_FIXTURE_NAME), info = await lstat(fixturePath);
    if (!info.isFile() || info.isSymbolicLink()) throw fail("shell-projection-fixture-changed", "The emission fixture is no longer a physical file", { fixturePath });
    const fixture = await readFile(fixturePath);
    if (!fixture.equals(Buffer.from(shellProjectionAccuracyFixture(mode)))) throw fail("shell-projection-fixture-changed", "The shell modified its immutable emission fixture", { fixturePath });
    let exported;
    if (previous === null) exported = await manualLiveExportTranscript({ context, input, sink, sessionId: turnOwner.sessionId, prompt: call.prompt, owner, stem });
    else {
      await manualLiveClick(input, EXPORT, sink, "cold-export-transcript", { owner, stem });
      exported = { source_path: previous.export.source_path };
    }
    const observation = await waitForObservation({ label: `${stem} visible summary and export status`, timeoutMs: 10000, retrySampleErrors: false,
      sample: () => observeDisplay(cdp, turnOwner.turnId), accept: display => shellProjectionVisibleFailures(display,
        { mode, turnId: turnOwner.turnId, exportPath: exported.source_path }).length === 0 });
    const bytes = await readFile(exported.source_path), text = bytes.toString("utf8");
    const exportFailures = shellProjectionExportFailures(text, { sessionId: turnOwner.sessionId, call, mode });
    if (exportFailures.length) throw fail("shell-projection-export-content", "Export did not preserve the clean displayed result", { path: exported.source_path, failures: exportFailures });
    const captured = { ...sample, display: observation.value, fixture: { path: fixturePath, sha256: sha256(fixture), size_bytes: fixture.length },
      export: { ...exported, sha256: sha256(bytes), size_bytes: bytes.length,
        evidence: exported.evidence ?? await sink.writeBytes(`${stem}/transcript/${path.basename(exported.source_path)}`, bytes) } };
    if (previous !== null && shellProjectionColdEvidenceFailures(previous, captured).length) throw fail("shell-projection-cold-evidence-changed", "Cold reopen changed fixture or visible export bytes", { previous, captured });
    await sink.writeJson(`${stem}/evidence.json`, captured);
    await captureScenarioScreenshot({ cdp, sink, name: stem.replaceAll("/", "-"), owner });
    return captured;
  } finally { await sink.record("shell-projection-input-cleanup", await input.cleanup(), { phase: "executing", owner }); }
}

export function createShellProjectionAccuracyScenario(mode) {
  const v = variant(mode), id = `permission.shell-projection-${mode}`, owner = `scenario:${id}`;
  let pendingEvidence = null, closedReference = null;
  return createProviderChatToolContinuationScenario({ profile: {
    id, owner, prompt: v.prompt, call: context => shellProjectionAccuracyCall(context, mode),
    sentinelName: SHELL_PROJECTION_FIXTURE_NAME, sentinelText: shellProjectionAccuracyFixture(mode),
    commandNames: ["submit_prompt", "answer_permission", "cancel_run"], blockingFailure,
    async beforeHeld({ context, cdp, input, commands, sink, provider, expectedCommand, waitForProductStage }) {
      const call = shellProjectionAccuracyCall(context, mode);
      const pending = await waitForProductStage({ label: "Exact shell range and quoted-data permission intent",
        sample: async () => ({ surface: await observePermissionSurface(cdp), ledger: provider.requestLedger }),
        decide: sample => shellProjectionPermissionDecision(sample, call),
        code: "shell-projection-permission-contract",
        message: "The exact public permission owner, metadata or same-ID visible dialog did not satisfy the bounded contract" });
      const permission = manualLivePermissionRequest(pending.value.surface.projection);
      pendingEvidence = pending.value;
      await sink.writeJson("shell-projection/permission.json", pendingEvidence);
      await captureScenarioScreenshot({ cdp, sink, name: "shell-projection-permission", owner });
      const answer = { command: "answer_permission", args: { decision: "approved", confirmationId: permission.confirmation_id } };
      await manualLiveClick(input, manualLivePermissionLocator(permission, "approve"), sink, "approve-exact-shell", { owner, stem: "shell-projection" });
      const observed = await waitForObservation({ label: "One exact shell approval command", timeoutMs: 10000, retrySampleErrors: false,
        sample: () => commands.snapshot(), accept: value => value.calls.length >= 2 });
      const proof = assertExactDesktopCommandSequence(observed.value, { expected: [expectedCommand, answer] });
      await sink.writeJson("shell-projection/permission-answer.json", proof);
      return [answer];
    },
    heldFailures: sample => shellProjectionAccuracyFailures(sample, { mode }),
    terminalFailures: (sample, _heldTime, held) => shellProjectionAccuracyFailures(sample,
      { mode, phase: "terminal", heldProjection: held.surface.projection }),
    async observeHeld({ cdp, input, sink, held }) {
      const heldOwner = exactTurnOwner(held.surface.projection, "turn");
      if (!isDeepStrictEqual(heldOwner, exactTurnOwner(pendingEvidence?.surface?.projection, "turn"))) throw fail("shell-projection-permission-owner-changed", "Shell completion changed the reviewed turn owner", { pendingEvidence, held });
      await openSummary(cdp, input, sink, heldOwner.turnId, owner, "shell-projection");
      const visible = await waitForObservation({ label: "Held visible clean shell result", timeoutMs: 10000, retrySampleErrors: false,
        sample: () => observeDisplay(cdp, heldOwner.turnId), accept: display => shellProjectionVisibleFailures(display, { mode, turnId: heldOwner.turnId }).length === 0 });
      await sink.writeJson("shell-projection/held.json", { ...held, display: visible.value });
    },
    async afterTerminal({ context, cdp, sink, host, scenario, provider, terminal, waitForProductStage }) {
      const call = shellProjectionAccuracyCall(context, mode);
      const before = await captureTerminal({ context, cdp, sink, sample: terminal, call, mode, owner, stem: "shell-projection/terminal" });
      const restarted = await host.restart({ context, scenario, sink, driver: cdp, phase: "executing" });
      await acquireInteractiveShell({ context, driver: restarted.driver, sink }, { evidenceOwner: owner, screenshotStem: "shell-projection-cold-shell" });
      const restored = await waitForProductStage({ label: "Cold shell display and canonical owner",
        sample: async () => ({ surface: await observeChatToolContinuationSurface(restarted.driver), ledger: provider.requestLedger }),
        decide: sample => blockingFailure(sample.surface) ? "fail" : shellProjectionAccuracyFailures(sample,
          { mode, phase: "restart", previousProjection: terminal.surface.projection }).length === 0 ? "pass" : "pending",
        code: "shell-projection-cold-history", message: "Cold reopen changed clean display history or its exact owner" });
      closedReference = await captureTerminal({ context, cdp: restarted.driver, sink, sample: restored.value, call, mode, owner, stem: "shell-projection/restart", previous: before });
    },
    async afterClosedStore({ context, sink, inputs, cleanup }) {
      if (inputs.oracle !== "pass" || closedReference === null) return { collection: "skipped", reason: "live-terminal-contract-not-passed" };
      const turnOwner = exactTurnOwner(closedReference.surface.projection, "idle"), call = shellProjectionAccuracyCall(context, mode);
      const canonical = await readCanonical(context, turnOwner, call, mode);
      const evidence = await sink.writeJson("shell-projection/closed-store.json", { owner: turnOwner, canonical, cleanup });
      if (canonical.failures.length) throw fail("shell-projection-closed-canonical-result",
        "Closed canonical history did not retain one exact reviewed call and its original SGR-bearing operation result", { owner: turnOwner, failures: canonical.failures, evidence });
      return { collection: "pass", owner: turnOwner, output_sha256: canonical.output_sha256, output_size_bytes: canonical.output_size_bytes, evidence };
    },
  } });
}

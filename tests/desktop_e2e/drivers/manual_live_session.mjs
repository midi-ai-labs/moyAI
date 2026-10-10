import crypto from "node:crypto";
import path from "node:path";
import { lstat, readFile, readdir } from "node:fs/promises";
import { waitForObservation } from "../core/deadline.mjs";
import { canonicalU64, canonicalUlid, canonicalWorkspace } from "../core/canonical_identity.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { WebviewInput, assertTrustedProbeSequence, assertTrustedTextInsertion } from "./webview_input.mjs";
import { DesktopCommandProbe } from "./desktop_command_probe.mjs";
import { runWindowsExternalProcess } from "./windows_external_process.mjs";
import { operatorRequestFingerprint, waitForOperatorReview } from "./operator_review.mjs";
import { observeProviderTurnSurface, providerRenderedTerminalFailures } from "../scenarios/provider_restart.mjs";
import { observeRunNextTurnSurface, settledComposer } from "../scenarios/run_next_turn.mjs";
import { captureScenarioScreenshot } from "../scenarios/observations.mjs";

const MAX_OUTPUT_BYTES = 8 * 1024 * 1024;
const PROMPT = { selector: "section.composer textarea#prompt", identity: { tag: "TEXTAREA", id: "prompt" } };
const SEND = { selector: 'section.composer button[data-action="send"]', identity: { tag: "BUTTON", action: "send" } };
const EXPORT = { selector: 'header.topbar button[data-action="export-transcript"]', identity: { tag: "BUTTON", action: "export-transcript" } };
const sha256 = bytes => crypto.createHash("sha256").update(bytes).digest("hex");
const failure = (code, message, evidence) => new DesktopE2eError("product", code, message, evidence);

export function manualLiveSection(spec, heading, language = "text") {
  const normalized = spec.replaceAll("\r\n", "\n");
  const marker = `## ${heading}\n`;
  const sections = normalized.split(marker);
  if (sections.length !== 2) throw new TypeError("spec must contain one requested section");
  const section = sections[1].split(/^## /m)[0];
  const fence = `\x60\x60\x60${language}\n`;
  const start = section.indexOf(fence);
  const end = start === -1 ? -1 : section.indexOf("\n\x60\x60\x60", start + fence.length);
  if (start === -1 || end === -1 || section.indexOf(fence, start + fence.length) !== -1) throw new TypeError("spec must contain one fenced request");
  const value = section.slice(start + fence.length, end);
  if (!value) throw new TypeError("spec request must not be empty");
  return value;
}

export function manualLivePrompt(spec) { return manualLiveSection(spec, "Canonical user request"); }

export function normalizeManualLiveOptions(raw) {
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) throw new TypeError("manual scenario requires explicit options");
  const allowed = new Set(["provider_base_url", "model", "api_key_env", "python_executable", "approval_mode",
    "side_model", "side_api_key_env", "approve_model", "approve_api_key_env", "access_mode"]);
  if (Object.keys(raw).some(key => !allowed.has(key))) throw new TypeError("unknown manual scenario option");
  const url = new URL(raw.provider_base_url);
  if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash
    || !["/v1", "/v1/"].includes(url.pathname)) throw new TypeError("manual scenario requires a credential-free HTTP(S) /v1 endpoint");
  if (typeof raw.model !== "string" || !raw.model || raw.model !== raw.model.trim()
    || /[\u0000-\u001f\u007f]/u.test(raw.model) || Buffer.byteLength(raw.model) > 1024) throw new TypeError("manual scenario requires an exact bounded model ID");
  const apiKeyEnv = raw.api_key_env ?? "";
  if (typeof apiKeyEnv !== "string" || (apiKeyEnv !== "" && !/^[A-Za-z_][A-Za-z0-9_]{0,127}$/.test(apiKeyEnv))) throw new TypeError("api_key_env must be an environment variable name");
  const roles = {};
  for (const role of ["side", "approve"]) {
    const model = raw[`${role}_model`];
    const key = raw[`${role}_api_key_env`];
    if (model !== undefined && (typeof model !== "string" || !model || model !== model.trim()
      || /[\u0000-\u001f\u007f]/u.test(model) || Buffer.byteLength(model) > 1024)) throw new TypeError(`${role}_model must be an exact bounded model ID`);
    if (key !== undefined && (typeof key !== "string" || (key !== "" && !/^[A-Za-z_][A-Za-z0-9_]{0,127}$/.test(key)))) throw new TypeError(`${role}_api_key_env must be an environment variable name`);
    if (model !== undefined || key !== undefined) {
      roles[`${role}Model`] = model ?? raw.model;
      roles[`${role}ApiKeyEnv`] = key ?? "";
    }
  }
  if (typeof raw.python_executable !== "string" || !path.isAbsolute(raw.python_executable)
    || raw.python_executable.includes("\0")) throw new TypeError("manual scenario requires an absolute python_executable");
  const approvalMode = raw.approval_mode ?? "stop";
  if (!["stop", "operator"].includes(approvalMode)) throw new TypeError("approval_mode must be stop or operator");
  const accessMode = raw.access_mode === undefined ? "default" : raw.access_mode;
  if (!["default", "auto_review", "full_access"].includes(accessMode)) throw new TypeError("access_mode must be default, auto_review, or full_access");
  return Object.freeze({ providerBaseUrl: url.href.replace(/\/$/, ""), model: raw.model, apiKeyEnv,
    pythonExecutable: raw.python_executable, approvalMode, accessMode, ...roles });
}

export function manualLiveFixtureConfig(options) {
  const roles = ["side", "approve"].filter(role => options[`${role}Model`] !== undefined).map(role =>
    `[${role === "side" ? "side_chat" : role}]\nbase_url = ${JSON.stringify(options.providerBaseUrl)}\nmodel = ${JSON.stringify(options[`${role}Model`])}\nprovider_profile = "openai_compatible"\napi_key_env = ${JSON.stringify(options[`${role}ApiKeyEnv`])}\n\n`).join("");
  return `[model]\nbase_url = ${JSON.stringify(options.providerBaseUrl)}\nmodel = ${JSON.stringify(options.model)}\nprovider_profile = "openai_compatible"\napi_key_env = ${JSON.stringify(options.apiKeyEnv)}\ncontext_window = 131072\n\n${roles}[permissions]\naccess_mode = ${JSON.stringify(options.accessMode ?? "default")}\n\n[multi_agent]\nenabled = false\n\n[docling]\nenabled = false\n\n[mcp]\nenabled = false\n`;
}

function manualLiveConfiguredFields(options) {
  const fields = [["permissions.access_mode", options.accessMode ?? "default"]];
  for (const [role, model, key] of [["model", options.model, options.apiKeyEnv],
    ["side_chat", options.sideModel, options.sideApiKeyEnv], ["approve", options.approveModel, options.approveApiKeyEnv]]) {
    if (model === undefined) continue;
    fields.push([`${role}.base_url`, options.providerBaseUrl], [`${role}.model`, model],
      [`${role}.provider_profile`, "openai_compatible"], [`${role}.api_key_env`, key ?? ""]);
  }
  return fields;
}

export function manualLiveConfigurationFailures(projection, options) {
  return manualLiveConfiguredFields(options).flatMap(([key, expected]) => {
    const rows = (projection?.config_fields ?? []).filter(field => field.key === key);
    const actual = rows.length === 1 ? rows[0].value : null;
    return actual === expected ? [] : [{ key, expected, actual }];
  });
}

function manualLiveProjectionTerminalDecision(surface) {
  const p = surface?.projection;
  if (p?.confirmation_visible === true) return "approval";
  if (surface?.visible_fatal_count > 0 || surface?.visible_recoverable_error_count > 0) return "failed";
  if (!p || p.busy !== false || p.post_run_refresh_pending !== false || p.background_mutation_pending !== false
    || p.async_polling_required !== false || !Array.isArray(p.pending_async_operations) || p.pending_async_operations.length !== 0) return "pending";
  if (["failed", "cancelled", "interrupted"].includes(p.run_status_key)) return "failed";
  if (p.run_status_key !== "completed") return "pending";
  return p.run_target?.expectedState?.kind === "idle"
    && canonicalUlid(p.run_target.sessionId) && canonicalUlid(p.run_target.expectedState.latestTurnId)
    && p.run_target.sessionId === p.draft_target?.sessionId && p.task_activity_state === "idle"
    && p.composer_submit_mode === "new_request" && p.can_submit === true ? "completed" : "pending";
}

export function manualLiveTerminalDecision(surface) {
  const decision = manualLiveProjectionTerminalDecision(surface);
  return decision === "completed" && providerRenderedTerminalFailures(surface).length > 0 ? "pending" : decision;
}

export function manualLiveTurnObservationReady(surface, { previousConfirmationId = null, previousTurnId = null } = {}) {
  const decision = manualLiveTerminalDecision(surface);
  return decision !== "pending"
    && !(surface.projection?.confirmation_visible && surface.projection.confirmation_id === previousConfirmationId)
    && !(decision === "completed" && previousTurnId !== null
      && surface.projection.run_target.expectedState.latestTurnId === previousTurnId);
}

function manualLiveStopOwnerReady(surface, request) {
  const projection = surface?.projection;
  const stopped = request?.stop_target;
  const target = projection?.run_target;
  return stopped?.kind === "turn" && canonicalWorkspace(stopped.workspacePath)
    && canonicalUlid(stopped.sessionId) && canonicalUlid(stopped.turnId)
    && canonicalU64(stopped.admissionRevision)
    && projection?.busy === false && projection.confirmation_visible === false
    && projection.post_run_refresh_pending === false && projection.background_mutation_pending === false
    && projection.async_polling_required === false && Array.isArray(projection.pending_async_operations)
    && projection.pending_async_operations.length === 0 && projection.task_activity_state === "idle"
    && target?.workspacePath === stopped.workspacePath && target?.sessionId === stopped.sessionId
    && target.expectedState?.kind === "idle" && target.expectedState.latestTurnId === stopped.turnId
    && target.expectedState.admissionRevision === stopped.admissionRevision
    // A fresh Rust terminal can precede the frontend's removal of the Stop dialog.
    && surface.visible_dialog_count === 0 && surface.visible_modal_backdrop_count === 0;
}

export function manualLiveStopObservationReady(surface, request) {
  return manualLiveStopOwnerReady(surface, request) && providerRenderedTerminalFailures(surface).length === 0;
}

export function manualLiveUnittestResult(processResult, stdout, stderr) {
  const text = `${stdout}\n${stderr}`;
  const runs = [...text.matchAll(/^Ran (\d+) tests? in .+$/gm)];
  const testCount = runs.length === 1 ? Number(runs[0][1]) : null;
  return { exit_code: processResult?.outcome?.root_exit_code ?? null, test_count: testCount,
    pass: processResult?.outcome?.root_exit_code === 0 && testCount > 0 && /^OK\s*$/m.test(text) };
}

export function manualLiveHistory(projection) {
  const rows = Array.isArray(projection?.transcript_rows) ? projection.transcript_rows : [];
  return { source: "canonical_desktop_projection",
    users: rows.filter(row => row.row_kind === "user").map(row => row.body),
    assistants: rows.filter(row => row.row_kind === "assistant").map(row => row.body),
    work_summaries: rows.filter(row => row.row_kind?.startsWith("work_summary")), file_changes: projection?.file_change_rows ?? [],
    metrics: { source: "desktop_display", session_usage_label: projection?.session_usage_label ?? null,
      session_usage_title: projection?.session_usage_title ?? null, token_meter_label: projection?.token_meter_label ?? null,
      token_meter_title: projection?.token_meter_title ?? null, numeric_runtime_metrics: "unverified_not_available_in_public_projection" },
    agent_unittest: "manual_public_evidence_review_pending", exact_tool_call_result_pairs: "unverified_not_available_in_public_projection" };
}

export function manualLiveTranscriptResult(markdown, { sessionId, prompt }) {
  const text = markdown.replaceAll("\r\n", "\n");
  const quote = text.match(/^# [^\n]*\n\n(> [^\n]*(?:\n> [^\n]*)*)\n\n/);
  const userPrompt = quote?.[1].split("\n").map(line => line.slice(2)).join("\n") ?? null;
  const metadataStart = text.lastIndexOf("<details><summary>実行情報</summary>\n\n");
  const metadata = metadataStart === -1 ? null : text.slice(metadataStart).match(/^<details><summary>実行情報<\/summary>\n\n([\s\S]*)\n<\/details>\n?$/);
  const sessions = metadata ? [...metadata[1].matchAll(/^- Session: `([^`\n]+)`$/gm)] : [];
  const exportedSessionId = sessions.length === 1 && canonicalUlid(sessions[0][1]) ? sessions[0][1] : null;
  return { pass: canonicalUlid(sessionId) && exportedSessionId === sessionId && userPrompt === prompt,
    session_id: exportedSessionId, user_prompt: userPrompt };
}

export function manualLivePermissionRequest(projection) {
  if (projection?.confirmation_visible !== true || projection.confirmation === null
    || typeof projection.confirmation !== "object" || Array.isArray(projection.confirmation)
    || (!canonicalU64(projection.confirmation_id) && !canonicalUlid(projection.confirmation_id))
    || !canonicalUlid(projection.run_target?.sessionId)) throw new DesktopE2eError("harness", "case1-review-owner", "permission review has no exact public owner", {});
  return structuredClone({ confirmation_id: projection.confirmation_id, request: projection.confirmation,
    run_target: projection.run_target, stop_target: projection.stop_target });
}

export function manualLivePermissionLocator(request, decision) {
  if (!["approve", "stop", "deny"].includes(decision)) throw new TypeError("unknown operator decision");
  const remote = Boolean(request.request?.remote);
  const action = decision === "approve" ? "approve-permission" : decision === "deny"
    ? remote ? "deny-permission" : "abort-permission" : remote ? "abort-permission" : "cancel-run";
  const focusAction = action === "cancel-run" ? "stop" : action.split("-")[0];
  return { selector: `.permission-confirmation[data-permission-id="${request.confirmation_id}"] button[data-action="${action}"][data-permission-action]`,
    identity: { tag: "BUTTON", action, focusKey: `permission:${request.confirmation_id}:${focusAction}` } };
}

export async function manualLiveClick(input, locator, sink, action, { owner = "scenario:manual.case1", stem = "case1" } = {}) {
  const before = await input.snapshotProbe();
  const target = await input.click(locator);
  const events = assertTrustedProbeSequence(await input.snapshotProbe(before.sequence), {
    afterSequence: before.sequence, expected: [{ type: "click", identity: locator.identity }] });
  const result = { action, target, events };
  await sink.record(`${stem}-trusted-action`, result, { phase: "executing", owner });
  return result;
}

export async function manualLiveGeneratedFiles(context, sink, evidenceStem = "case1/generated", capturePaths = ["calculator.py", "test_calculator.py"]) {
  const files = [];
  async function visit(directory) {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const candidate = path.join(directory, entry.name);
      const relative = path.relative(context.paths.workspace, candidate).replaceAll("\\", "/");
      if (entry.isSymbolicLink()) { files.push({ path: relative, symbolic_link: true }); continue; }
      if (entry.isDirectory()) await visit(candidate);
      else if (entry.isFile()) {
        const item = await lstat(candidate);
        if (item.size > MAX_OUTPUT_BYTES) { files.push({ path: relative, size_bytes: item.size, capture: "too_large" }); continue; }
        const bytes = await readFile(candidate);
        const source = capturePaths.includes(relative) ? await sink.writeBytes(`${evidenceStem}/${relative}`, bytes) : null;
        files.push({ path: relative, size_bytes: bytes.length, sha256: sha256(bytes), source });
      }
    }
  }
  await visit(context.paths.workspace);
  return files.sort((left, right) => left.path.localeCompare(right.path));
}

export async function manualLiveExternalProcess({ context, sink, options, label, args, owner = "scenario:manual.case1", stem = "case1", phase = "executing" }) {
  const stdoutPath = path.join(context.paths.logs, `${label}.stdout.log`);
  const stderrPath = path.join(context.paths.logs, `${label}.stderr.log`);
  const excluded = ["temp", "tmp", "tmpdir", options.apiKeyEnv, options.sideApiKeyEnv, options.approveApiKeyEnv]
    .filter(value => typeof value === "string").map(value => value.toLowerCase());
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !excluded.includes(key.toLowerCase())));
  Object.assign(env, { PYTHONUTF8: "1", PYTHONDONTWRITEBYTECODE: "1", TEMP: context.paths.logs, TMP: context.paths.logs, TMPDIR: context.paths.logs });
  const result = await runWindowsExternalProcess({ executionRoot: context.root, executable: options.pythonExecutable, args,
    cwd: context.paths.workspace, env, stdoutPath, stderrPath, timeoutMs: 120_000, maxOutputBytes: MAX_OUTPUT_BYTES, label });
  const captures = await Promise.all([[stdoutPath, result.output.stdout], [stderrPath, result.output.stderr]].map(async ([candidate, expected]) => {
    const bytes = await readFile(candidate);
    if (bytes.length !== expected.size_bytes || sha256(bytes) !== expected.sha256) throw new DesktopE2eError("harness", "case1-external-output-changed", "external output changed after Job settlement", { candidate });
    return { text: bytes.toString("utf8"), evidence: await sink.writeBytes(`${stem}/external/${path.basename(candidate)}`, bytes) };
  }));
  await sink.record(`${stem}-external-process`, { label, process: result, stdout: captures[0].evidence, stderr: captures[1].evidence }, { phase, owner });
  return { result, stdout: captures[0].text, stderr: captures[1].text };
}

export function manualLiveManifestDiff(before, after) {
  const old = new Map(before.filter(row => !row.path.startsWith(".moyai/") && !row.path.split("/").includes("__pycache__")).map(row => [row.path, row]));
  const next = new Map(after.filter(row => !row.path.startsWith(".moyai/") && !row.path.split("/").includes("__pycache__")).map(row => [row.path, row]));
  return [...new Set([...old.keys(), ...next.keys()])].filter(name =>
    !old.has(name) || !next.has(name) || old.get(name).sha256 !== next.get(name).sha256
      || old.get(name).size_bytes !== next.get(name).size_bytes || old.get(name).symbolic_link !== next.get(name).symbolic_link).sort();
}

export async function manualLiveExportTranscript({ context, input, sink, sessionId, prompt, owner = "scenario:manual.case1", stem = "case1" }) {
  const directory = path.join(context.paths.workspace, ".moyai", "transcript-exports");
  const list = async () => {
    try {
      const item = await lstat(directory);
      if (!item.isDirectory() || item.isSymbolicLink()) throw failure("case1-export-directory", "transcript export directory is not a physical directory", { directory });
      return (await readdir(directory, { withFileTypes: true })).filter(row => row.isFile() && !row.isSymbolicLink() && row.name.endsWith(".md"));
    } catch (error) { if (error.code === "ENOENT") return []; throw error; }
  };
  if ((await list()).length !== 0) throw failure("case1-export-not-fresh", "fresh workspace already contains transcript exports", {});
  await manualLiveClick(input, EXPORT, sink, "export-transcript", { owner, stem });
  const observed = await waitForObservation({ label: "Manual transcript export", timeoutMs: 10_000, pollMs: 100,
    retrySampleErrors: false, sample: list, accept: rows => rows.length === 1 });
  const candidate = path.join(directory, observed.value[0].name);
  const bytes = await readFile(candidate);
  if (!manualLiveTranscriptResult(bytes.toString("utf8"), { sessionId, prompt }).pass) throw failure("case1-export-content", "canonical transcript does not preserve this session and prompt", { candidate, sessionId });
  return { source_path: candidate, evidence: await sink.writeBytes(`${stem}/transcript/${path.basename(candidate)}`, bytes) };
}

export class ManualLiveSession {
  constructor({ context, driver, sink, options, owner, stem, capturePaths = ["calculator.py", "test_calculator.py"] }) {
    Object.assign(this, { context, driver, sink, options, owner, stem, capturePaths, input: null, commands: null, lastTerminal: null });
  }
  async open() {
    const initial = await observeProviderTurnSurface(this.driver);
    const failures = manualLiveConfigurationFailures(initial.projection, this.options);
    if (failures.length) throw failure(`${this.stem}-role-config`, "manual case role connections or access mode differ from explicit inputs", { failures });
    await this.sink.record(`${this.stem}-role-config`, { fields: manualLiveConfiguredFields(this.options).map(([key, expected]) => ({ key, value: expected })),
      config_target: structuredClone(initial.projection.config_target),
      observation: "public_config_projection", provider_traffic: "not_observed_by_this_check" }, { phase: "executing", owner: this.owner });
    this.input = new WebviewInput(this.driver, { probeId: `manual-${this.stem}-input` });
    await this.input.installProbe();
    this.commands = new DesktopCommandProbe(this.driver, { probeId: `manual-${this.stem}-permission`, commands: ["answer_permission", "cancel_run"] });
    await this.commands.install();
  }
  async send(prompt, { stage = null, timeoutMs = 15 * 60 * 1000 } = {}) {
    const { context, driver: cdp, sink, options, owner: OWNER } = this;
    const prefix = stage === null ? this.stem : `${this.stem}-${stage}`;
    const state = { input: this.input, commands: this.commands, prompt };
    const previousTurnId = this.lastTerminal?.run_target?.expectedState?.latestTurnId ?? null;
    const click = (...args) => manualLiveClick(...args, { owner: this.owner, stem: prefix });
    const generatedFiles = (...args) => manualLiveGeneratedFiles(...args, this.capturePaths);
    if (this.lastTerminal !== null) {
      await waitForObservation({ label: "Manual continuation composer settles", timeoutMs: 10000, retrySampleErrors: false,
        sample: () => observeRunNextTurnSurface(cdp), accept: value => settledComposer(value)
          && value.projection.run_target.sessionId === this.lastTerminal.run_target.sessionId });
    }
        await click(state.input, PROMPT, sink, "focus-canonical-prompt");
        const before = await state.input.snapshotProbe();
        await state.input.insertText(PROMPT, state.prompt);
        const typing = assertTrustedTextInsertion(await state.input.snapshotProbe(before.sequence), { afterSequence: before.sequence, identity: PROMPT.identity, text: state.prompt });
        await waitForObservation({ label: `${prefix} prompt ready`, timeoutMs: 10_000, retrySampleErrors: false, sample: () => observeProviderTurnSurface(cdp), accept: value => value.prompt?.value === state.prompt && value.send?.enabled === true });
        const send = await click(state.input, SEND, sink, "send-canonical-prompt");
        await captureScenarioScreenshot({ cdp, sink, name: `${prefix}-request-sent`, owner: OWNER });
        await sink.record(`${prefix}-prompt-sent`, { typing, send, prompt: state.prompt }, { phase: "executing", owner: OWNER });
        const turnDeadline = Date.now() + timeoutMs;
        let firstRenderDiscrepancyAt = null;
        const renderEvidence = surface => ({
          projection: { run_target: surface.projection.run_target,
            session_usage_label: surface.projection.session_usage_label,
            session_usage_title: surface.projection.session_usage_title,
            session_usage_state: surface.projection.session_usage_state },
          composer: surface.composer, session_usage: surface.session_usage,
          failures: providerRenderedTerminalFailures(surface),
        });
        const sampleTerminal = async ownerReady => {
          const surface = await observeProviderTurnSurface(cdp);
          if (firstRenderDiscrepancyAt === null && ownerReady(surface)
            && providerRenderedTerminalFailures(surface).length > 0) {
            firstRenderDiscrepancyAt = new Date().toISOString();
            await sink.record(`${prefix}-terminal-render`, { status: "discrepancy", ...renderEvidence(surface) },
              { phase: "executing", owner: OWNER, at: firstRenderDiscrepancyAt });
          }
          return surface;
        };
        const recordRenderedSettlement = surface => sink.record(`${prefix}-terminal-render`, {
          status: "settled", first_discrepancy_at: firstRenderDiscrepancyAt, ...renderEvidence(surface),
        }, { phase: "executing", owner: OWNER });
        const waitTerminal = async previousId => {
          const observed = await waitForObservation({ label: `${prefix} completion or review request`,
            timeoutMs: Math.max(1, turnDeadline - Date.now()), pollMs: 500, retrySampleErrors: false,
            sample: () => sampleTerminal(value => manualLiveProjectionTerminalDecision(value) === "completed"
              && value.projection.run_target.expectedState.latestTurnId !== previousTurnId),
            accept: value => manualLiveTurnObservationReady(value,
              { previousConfirmationId: previousId, previousTurnId }) });
          if (manualLiveTerminalDecision(observed.value) === "completed") await recordRenderedSettlement(observed.value);
          return observed;
        };
        let observed = await waitTerminal(null);
        let approval = false;
        let reviewStopReason = null;
        const approvals = [];
        while (manualLiveTerminalDecision(observed.value) === "approval") {
          const request = manualLivePermissionRequest(observed.value.projection);
          const number = approvals.length + 1;
          const requestSha256 = operatorRequestFingerprint(request);
          const screenshot = await captureScenarioScreenshot({ cdp, sink, name: `${prefix}-approval-${number}-request`, owner: OWNER });
          const evidence = await sink.writeJson(`${prefix}/approvals/${number}-request.json`, { ...request, request_sha256: requestSha256 });
          const commandsBefore = await state.commands.snapshot();
          if (commandsBefore.calls.some(row => row.command === "answer_permission" && row.args?.confirmationId === request.confirmation_id)) throw new DesktopE2eError("harness", "case1-review-replayed", "this confirmation already has a GUI decision", {});
          const pendingEvidence = await sink.writeJson(`${prefix}/approvals/${number}-pending.json`, {
            surface: observed.value, history: manualLiveHistory(observed.value.projection),
            generated: await generatedFiles(context, sink, `${prefix}/approvals/${number}/generated`), commands_before_decision: commandsBefore,
            observation: "permission pending; no GUI decision for this confirmation has been sent",
          });
          const item = { ...request, request_sha256: requestSha256, evidence, pending_evidence: pendingEvidence, screenshot };
          approvals.push(item);
          await sink.record("case1-approval-awaiting-review", item, { phase: "executing", owner: OWNER });
          const reviewed = options.approvalMode === "operator"
            ? await waitForOperatorReview(request, { timeoutMs: Math.min(5 * 60 * 1000, Math.max(1, turnDeadline - Date.now())),
              evidence: { request: evidence, pending: pendingEvidence, screenshot } })
            : { status: "not_decided", reason: "default-stop" };
          item.operator_review = reviewed;
          const current = await observeProviderTurnSurface(cdp);
          if (operatorRequestFingerprint(manualLivePermissionRequest(current.projection)) !== requestSha256) throw new DesktopE2eError("harness", "case1-review-stale", "reviewed permission request or owner changed before GUI decision", { request, current });
          const decision = reviewed.status === "decided" ? reviewed.decision : "stop";
          const locator = manualLivePermissionLocator(request, decision);
          await click(state.input, locator, sink, `operator-${decision}-permission`);
          const commandsAfter = await waitForObservation({ label: "Case1 exact permission GUI command", timeoutMs: 5000, retrySampleErrors: false,
            sample: () => state.commands.snapshot(commandsBefore.sequence), accept: value => value.calls.length === 1 });
          if (locator.identity.action !== "cancel-run") {
            const expectedDecision = decision === "approve" ? "approved" : decision === "deny" && request.request?.remote ? "denied" : "abort";
            const call = commandsAfter.value.calls[0];
            if (call.command !== "answer_permission" || call.args?.confirmationId !== request.confirmation_id || call.args?.decision !== expectedDecision) throw new DesktopE2eError("harness", "case1-review-command", "GUI permission command did not address the reviewed confirmation", { request, call });
          } else {
            const call = commandsAfter.value.calls[0];
            if (call.command !== "cancel_run" || JSON.stringify(call.args?.expectedTarget) !== JSON.stringify(request.stop_target)) throw new DesktopE2eError("harness", "case1-review-stop-owner", "GUI Stop did not address the reviewed runtime owner", { request, call });
          }
          item.commands_after_decision = commandsAfter.value;
          await sink.record("case1-approval-decision", item, { phase: "executing", owner: OWNER });
          if (decision === "approve") { observed = await waitTerminal(request.confirmation_id); continue; }
          approval = true;
          reviewStopReason = reviewed.status === "decided" ? `operator-${decision}` : reviewed.reason;
          observed = await waitForObservation({ label: `${prefix} operator Stop terminal`, timeoutMs: 30_000, pollMs: 100,
            retrySampleErrors: false, sample: () => sampleTerminal(value => manualLiveStopOwnerReady(value, request)),
            accept: value => manualLiveStopObservationReady(value, request) });
          await recordRenderedSettlement(observed.value);
          break;
        }
        const terminal = observed.value;

    this.lastTerminal = structuredClone(terminal.projection);
    return { terminal, approvals, incomplete: approval, incompleteReason: reviewStopReason };
  }
  async close() {
    const result = { input: null, commands: null };
    if (this.commands !== null) { try { result.commands = await this.commands.remove(); } catch (error) { result.commands = { failure: error.message }; } }
    if (this.input !== null) { try { result.input = await this.input.cleanup(); } catch (error) { result.input = { failure: error.message }; } }
    return result;
  }
}

import crypto from "node:crypto";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { lstat, readFile, readdir, realpath, writeFile } from "node:fs/promises";
import { canonicalUlid } from "../core/canonical_identity.mjs";
import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { runWindowsExternalProcess } from "../drivers/windows_external_process.mjs";
import { snapshotOwnedTopLevelWindows, selectFreshOwnedRootWindow, openFilePathInOwnedNativeDialog,
  probeExactOwnedWindow, closeOwnedWindowForCleanup, captureOwnedWindowPng } from "../drivers/windows_native_input.mjs";
import { ManualLiveSession, normalizeManualLiveOptions, manualLivePrompt, manualLiveFixtureConfig,
  manualLiveClick, manualLiveHistory, manualLiveGeneratedFiles, manualLiveExportTranscript,
  manualLiveTerminalDecision, manualLiveExternalProcess, manualLiveUnittestResult } from "../drivers/manual_live_session.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import { observeProviderTurnSurface } from "./provider_restart.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";

const OWNER = "scenario:manual.case2";
const SPEC = fileURLToPath(new URL("../../manual_ST/case2/spec.md", import.meta.url));
const CONTRACT = fileURLToPath(new URL("../../manual_ST/case2/scenario_contract.md", import.meta.url));
const ACL_SCRIPT = fileURLToPath(new URL("../drivers/manual_case2_capture.ps1", import.meta.url));
const OUTPUTS = ["space_invader.py", "test_space_invader.py", "README.md"];
const CASE_TIMEOUT_MS = 30 * 60 * 1000;
const MAX_CAPTURE_BYTES = 16 * 1024 * 1024;
const sha256 = bytes => crypto.createHash("sha256").update(bytes).digest("hex");
const fail = (code, message, evidence = {}) => new DesktopE2eError("product", code, message, evidence);
const action = name => ({ selector: `section.composer button[data-action="${name}"]`, identity: { tag: "BUTTON", action: name } });

export function normalizeManualCase2Options(raw = {}) {
  const { image_source, ...connection } = raw;
  if (typeof image_source !== "string" || !path.isAbsolute(image_source) || image_source.includes("\0")
    || ![".png", ".jpg", ".jpeg", ".webp", ".gif"].includes(path.extname(image_source).toLowerCase())) {
    throw new TypeError("case2 image_source must be an absolute supported image path");
  }
  return Object.freeze({ ...normalizeManualLiveOptions(connection), imageSource: path.resolve(image_source) });
}

/** Read only actual user image parts; tool arguments and text are not image transport evidence. */
export function manualCase2ImageDiagnostics(body) {
  const messages = Array.isArray(body?.messages) ? body.messages : Array.isArray(body?.input) ? body.input : [];
  const images = messages.filter(row => row?.role === "user").flatMap(row => Array.isArray(row.content) ? row.content : [])
    .filter(part => ["image_url", "input_image"].includes(part?.type)).map(part => {
      const url = part.type === "image_url" ? part.image_url?.url : part.image_url;
      const match = typeof url === "string" ? url.match(/^data:(image\/[a-z0-9.+-]+);base64,([A-Za-z0-9+/]*={0,2})$/i) : null;
      if (!match) return { valid_data_url: false };
      const bytes = Buffer.from(match[2], "base64");
      if (bytes.toString("base64") !== match[2] || bytes.length === 0) return { valid_data_url: false };
      return { valid_data_url: true, mime_type: match[1].toLowerCase(), size_bytes: bytes.length, sha256: sha256(bytes) };
    });
  return { model: body?.model ?? null, image_count: images.length, images };
}

export function manualCase2FixtureFailures(before, after, imageName) {
  return ["scenario_contract.md", imageName].flatMap(name => {
    const old = before.find(row => row.path === name);
    const next = after.find(row => row.path === name);
    return old && next && next.symbolic_link !== true && old.sha256 === next.sha256 && old.size_bytes === next.size_bytes
      ? [] : [`input-fixture-changed:${name}`];
  });
}

export function manualCase2CaptureAclValid(value, executionRoot, captureDirectory) {
  if (!path.isAbsolute(executionRoot) || !path.isAbsolute(captureDirectory)
    || path.resolve(captureDirectory) !== path.join(path.resolve(executionRoot), "logs", "case2-provider-requests")) return false;
  const sids = [value?.current_sid, "S-1-5-18", "S-1-5-32-544"];
  const exactRules = (acl, inherited) => acl?.owner_sid === value.current_sid && Array.isArray(acl.rules) && acl.rules.length === 3
    && new Set(acl.rules.map(row => row.sid)).size === 3 && acl.rules.every(row => sids.includes(row.sid)
      && row.type === "Allow" && row.full_control === true && row.inherited === inherited
      && (inherited || row.inheritance === "ContainerInherit, ObjectInherit" && row.propagation === "None"));
  return value?.schema_version === "desktop-e2e.manual-case2-capture-acl.v1"
    && typeof value.current_account === "string" && value.current_account.length > 0
    && /^S-1-5-21-(?:\d+-){3}\d+$/.test(value.current_sid) && new Set(sids).size === 3
    && path.resolve(value.execution_root ?? "") === path.resolve(executionRoot)
    && path.resolve(value.capture_directory ?? "") === path.resolve(captureDirectory)
    && Array.isArray(value.allowed_sids) && value.allowed_sids.length === 3
    && new Set(value.allowed_sids).size === 3 && value.allowed_sids.every(sid => sids.includes(sid))
    && value.directory_acl?.protected === true && exactRules(value.directory_acl, false)
    && exactRules(value.inherited_file_acl, true) && value.parent_acls_unchanged === true && value.probe_removed === true;
}

async function preparePrivateCapture({ context, sink, options, captureDirectory }) {
  const searchPath = Object.entries(process.env).find(([key]) => key.toLowerCase() === "path")?.[1] ?? "";
  let powershell = null;
  for (const directory of searchPath.split(path.delimiter).filter(value => path.isAbsolute(value))) {
    const candidate = path.join(directory, "pwsh.exe");
    try { if ((await lstat(candidate)).isFile()) { powershell = await realpath(candidate); break; } }
    catch (error) { if (error.code !== "ENOENT" && error.code !== "ENOTDIR") throw error; }
  }
  if (powershell === null) throw new DesktopE2eError("environment", "case2-powershell-unavailable", "existing PowerShell 7 executable is unavailable on PATH", {});
  const stdoutPath = path.join(context.paths.logs, "case2-capture-acl.stdout.log");
  const stderrPath = path.join(context.paths.logs, "case2-capture-acl.stderr.log");
  const excluded = [options.apiKeyEnv, options.sideApiKeyEnv, options.approveApiKeyEnv, "temp", "tmp", "tmpdir"]
    .filter(value => typeof value === "string").map(value => value.toLowerCase());
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !excluded.includes(key.toLowerCase())));
  Object.assign(env, { TEMP: context.paths.logs, TMP: context.paths.logs, TMPDIR: context.paths.logs });
  const result = await runWindowsExternalProcess({ executionRoot: context.root, executable: powershell,
    args: ["-NoLogo", "-NoProfile", "-NonInteractive", "-File", ACL_SCRIPT, "-ExecutionRoot", context.root, "-CaptureDirectory", captureDirectory],
    cwd: context.root, env, stdoutPath, stderrPath, timeoutMs: 30000, maxOutputBytes: 1024 * 1024, label: "case2-capture-acl" });
  const [stdout, stderr] = await Promise.all([readFile(stdoutPath), readFile(stderrPath)]);
  for (const [bytes, expected] of [[stdout, result.output.stdout], [stderr, result.output.stderr]]) {
    if (sha256(bytes) !== expected.sha256 || bytes.length !== expected.size_bytes) throw new DesktopE2eError("harness", "case2-acl-output-changed", "ACL process output changed after settlement", {});
  }
  const output = { stdout: await sink.writeBytes("case2/capture-acl.stdout.log", stdout), stderr: await sink.writeBytes("case2/capture-acl.stderr.log", stderr) };
  let acl = null;
  try { acl = JSON.parse(stdout.toString("utf8")); } catch { /* Preserve raw output and exact process result below. */ }
  const pass = result.outcome.root_exit_code === 0 && result.job.descendant_zero === true
    && manualCase2CaptureAclValid(acl, context.root, captureDirectory);
  await sink.writeJson("case2/capture-acl.json", { pass, acl, process: result, output });
  if (!pass) throw new DesktopE2eError("environment", "case2-capture-acl", "private capture ACL/account/readback was not established before launch", { acl, result, output });
}

async function preparedRequests(directory, sink) {
  const entries = await readdir(directory, { withFileTypes: true }).catch(error => {
    if (error.code === "ENOENT") return []; throw error;
  });
  const captures = [];
  for (const entry of entries.filter(row => row.name.endsWith(".metadata.json")).sort((a, b) => a.name.localeCompare(b.name))) {
    if (!entry.isFile() || entry.isSymbolicLink()) throw new DesktopE2eError("harness", "case2-capture-file", "request metadata is not a physical file", {});
    const metadata = JSON.parse(await readFile(path.join(directory, entry.name), "utf8"));
    const bodyName = entry.name.replace(/\.metadata\.json$/, ".request.json");
    if (metadata.schema_version !== 2 || metadata.capture_stage !== "prepared" || metadata.transport !== "http"
      || metadata.request_body_file !== bodyName || typeof metadata.request_id !== "string") {
      throw new DesktopE2eError("harness", "case2-capture-owner", "prepared request metadata does not own its body", {});
    }
    const candidate = path.join(directory, bodyName);
    const item = await lstat(candidate);
    if (!item.isFile() || item.isSymbolicLink() || item.size > MAX_CAPTURE_BYTES) throw new DesktopE2eError("harness", "case2-capture-size", "prepared request cannot be read as a bounded physical file", {});
    const bytes = await readFile(candidate);
    if (bytes.length !== metadata.request_body_bytes) throw new DesktopE2eError("harness", "case2-capture-bytes", "request body length differs from metadata", {});
    captures.push({ request_id: metadata.request_id, capture_stage: "prepared", api_mode: metadata.api_mode,
      endpoint_path: metadata.endpoint_path, request_body_sha256: sha256(bytes), request_body_bytes: bytes.length,
      ...manualCase2ImageDiagnostics(JSON.parse(bytes.toString("utf8"))) });
  }
  const summary = { source: "task_local_prepared_http_dto", requests: captures,
    provider_attempt_and_receipt: "unverified_not_proved_by_prepared_capture" };
  await sink.writeJson("case2/prepared-requests.json", summary);
  return summary;
}

async function settlePicker(state) {
  if (!state.pickerDispatched || !state.nativeOwner) return;
  let candidate = state.nativeCandidate;
  if (!candidate) {
    const windows = await snapshotOwnedTopLevelWindows(state.nativeOwner);
    try { candidate = selectFreshOwnedRootWindow(state.nativeBefore, windows, state.nativeOwner.expectedOwner, { expectedClassName: "#32770" }); }
    catch (error) { if (error?.code !== "native-window-cardinality" || error.evidence?.fresh_windows?.length !== 0) throw error; }
  }
  if (candidate && (await probeExactOwnedWindow({ ...state.nativeOwner, candidate })).live) {
    await closeOwnedWindowForCleanup({ ...state.nativeOwner, candidate });
    await waitForObservation({ label: "Case2 interrupted image picker closes", timeoutMs: 10000, retrySampleErrors: false,
      sample: () => probeExactOwnedWindow({ ...state.nativeOwner, candidate }), accept: value => !value.live });
  }
  state.nativeCandidate = null; state.pickerDispatched = false;
}

export function createManualCase2Scenario(raw = {}, { prepareCapture = preparePrivateCapture } = {}) {
  const options = normalizeManualCase2Options(raw);
  const state = { prompt: null, image: null, baseline: null, captureDirectory: null, live: null, cleanup: null,
    nativeOwner: null, nativeBefore: null, nativeCandidate: null, pickerDispatched: false, pickerCleanupFailure: null, quiesced: false };
  return Object.freeze({ id: "manual.case2", productOracle: "pass", manualGate: "pending", databaseRequired: true,
    get environment() { return state.captureDirectory === null ? {} : { MOYAI_HTTP_REQUEST_CAPTURE_DIR: state.captureDirectory }; },
    async requestGracefulExit(cdp) {
      try { await settlePicker(state); }
      catch (error) { state.pickerCleanupFailure = error.message; return { requested: false, reason: "case2-native-picker-cleanup-failed" }; }
      return requestGracefulExit(cdp);
    },
    async prepare({ context, sink, phase }) {
      for (const name of ["workspace", "config", "data"]) if ((await readdir(context.paths[name])).length) throw new DesktopE2eError("harness", "case2-not-fresh", "Case2 requires fresh workspace/config/data", {});
      const source = await lstat(options.imageSource);
      if (!source.isFile() || source.isSymbolicLink() || source.size > MAX_CAPTURE_BYTES) throw new DesktopE2eError("environment", "case2-image-source", "reference image must be a bounded physical file", {});
      const [spec, contract, image] = await Promise.all([readFile(SPEC), readFile(CONTRACT), readFile(options.imageSource)]);
      state.prompt = manualLivePrompt(spec.toString("utf8"));
      await prepareDesktopFixture({ context, sink, phase, owner: OWNER, configText: manualLiveFixtureConfig(options), sentinelName: null, sentinelText: "" });
      state.captureDirectory = path.join(context.paths.logs, "case2-provider-requests");
      await prepareCapture({ context, sink, options, captureDirectory: state.captureDirectory });
      const imageName = `reference-image${path.extname(options.imageSource).toLowerCase()}`;
      const imagePath = path.join(context.paths.workspace, imageName);
      await writeFile(path.join(context.paths.workspace, "scenario_contract.md"), contract, { flag: "wx" });
      await writeFile(imagePath, image, { flag: "wx" });
      state.image = { source_path: options.imageSource, source_filename: path.basename(options.imageSource), workspace_path: imagePath,
        workspace_filename: imageName, size_bytes: image.length, sha256: sha256(image) };
      state.baseline = await manualLiveGeneratedFiles(context, sink, "case2/baseline", ["scenario_contract.md", imageName]);
      await sink.record("case2-input", { options, prompt: state.prompt, image: state.image, baseline: state.baseline, observation_timeout_ms: options.observationTimeoutMs ?? CASE_TIMEOUT_MS,
        spec: { path: SPEC, sha256: sha256(spec) }, contract: { path: CONTRACT, sha256: sha256(contract) },
        model_capability: "unknown_unless_separate_provider_metadata_proves_support", runtime_image_policy: "existing_default_supports_images_true",
        provider_lifecycle: "external-unmanaged", provider_owned: false }, { phase, owner: OWNER });
    },
    async execute({ context, driver: cdp, sink, runtime }) {
      let primaryError = null;
      try {
        await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: OWNER, screenshotStem: "case2-ready" });
        const p = (await observeProviderTurnSurface(cdp)).projection;
        if (p.provider_effective_profile !== "openai_compatible" || p.provider_effective_base_url !== options.providerBaseUrl
          || p.provider_effective_model_id !== options.model || p.provider_effective_api_key_env !== options.apiKeyEnv
          || Number(p.provider_effective_context_window) !== 131072) throw fail("case2-provider-config", "effective provider differs from explicit inputs");
        state.live = new ManualLiveSession({ context, driver: cdp, sink, options, owner: OWNER, stem: "case2",
          capturePaths: [...OUTPUTS, "scenario_contract.md", state.image.workspace_filename], observationTimeoutMs: CASE_TIMEOUT_MS });
        await state.live.open();
        state.nativeOwner = { executionRoot: context.root, ownerPath: runtime.desktop_owner_path, expectedOwner: runtime.desktop_owner };
        await manualLiveClick(state.live.input, action("toggle-attachment-tray"), sink, "open-attachment-tray", { owner: OWNER, stem: "case2" });
        await waitForObservation({ label: "Case2 image picker action becomes ready", timeoutMs: 10000, retrySampleErrors: false,
          sample: () => cdp.evaluate(`(() => { const b = document.querySelector('section.composer button[data-action="browse-image"]'); return Boolean(b && !b.disabled && b.getClientRects().length && !b.closest('[hidden], [inert]')); })()`), accept: Boolean });
        state.nativeBefore = await snapshotOwnedTopLevelWindows(state.nativeOwner);
        state.pickerDispatched = true;
        await manualLiveClick(state.live.input, action("browse-image"), sink, "browse-reference-image", { owner: OWNER, stem: "case2" });
        const picked = await waitForObservation({ label: "Case2 exact owned image picker", timeoutMs: 10000, retrySampleErrors: false,
          sample: async () => {
            const windows = await snapshotOwnedTopLevelWindows(state.nativeOwner);
            try { return selectFreshOwnedRootWindow(state.nativeBefore, windows, runtime.desktop_owner, { expectedClassName: "#32770" }); }
            catch (error) { if (error?.code === "native-window-cardinality" && error.evidence?.fresh_windows?.length === 0) return null; throw error; }
          }, accept: Boolean });
        state.nativeCandidate = picked.value;
        const png = await captureOwnedWindowPng({ ...state.nativeOwner, candidate: picked.value });
        if (png.available) await sink.writeBytes("screenshots/case2-native-image-picker.png", png.bytes);
        const selection = await openFilePathInOwnedNativeDialog({ ...state.nativeOwner, candidate: picked.value, selectedPath: state.image.workspace_path });
        await waitForObservation({ label: "Case2 image picker closes", timeoutMs: 10000, retrySampleErrors: false,
          sample: () => probeExactOwnedWindow({ ...state.nativeOwner, candidate: picked.value }), accept: value => !value.live });
        state.nativeCandidate = null; state.pickerDispatched = false;
        const attached = await waitForObservation({ label: "Case2 reference image attached", timeoutMs: 10000, retrySampleErrors: false,
          sample: () => observeProviderTurnSurface(cdp), accept: value => value.projection.attached_images?.length === 1 && value.projection.image_input_enabled === true });
        const attachedPath = attached.value.projection.attached_images[0];
        if (await realpath(attachedPath) !== await realpath(state.image.workspace_path)) throw fail("case2-attached-image", "composer attachment differs from copied reference image");
        await sink.writeJson("case2/attachment.json", { selection, image: state.image, projection: attached.value.projection });
        await captureScenarioScreenshot({ cdp, sink, name: "case2-image-attached", owner: OWNER });
        const expectedUserBody = `${state.prompt}\n${attachedPath} (${state.image.size_bytes} bytes)`;
        const live = await state.live.send(state.prompt);
        const projection = live.terminal.projection;
        await sink.writeJson("case2/final-projection.json", live.terminal);
        await captureScenarioScreenshot({ cdp, sink, name: "case2-terminal", owner: OWNER });
        const diagnostics = { terminal_decision: manualLiveTerminalDecision(live.terminal), run_status_key: projection.run_status_key,
          status_message: projection.status_message, status_detail: projection.status_detail };
        if (!live.incomplete && diagnostics.terminal_decision !== "completed") {
          const stopped = { machine_gate: "fail", diagnostics, history: manualLiveHistory(projection),
            generated: await manualLiveGeneratedFiles(context, sink, "case2/generated", [...OUTPUTS, "scenario_contract.md", state.image.workspace_filename]),
            prepared_requests: await preparedRequests(state.captureDirectory, sink), approvals: live.approvals,
            external_verification: "not_reached", transcript: "not_reached_failed_terminal", manual_verdict: "pending" };
          await sink.writeJson("case2/summary.json", stopped);
          throw fail("case2-desktop-terminal", "Case2 did not complete; downstream artifact verification was not reached", stopped);
        }
        const sessionId = projection.run_target?.sessionId;
        if (!canonicalUlid(sessionId)) throw new DesktopE2eError("harness", "case2-terminal-owner", "Case2 has no exact terminal owner", {});
        const history = manualLiveHistory(projection);
        const generated = await manualLiveGeneratedFiles(context, sink, "case2/generated", [...OUTPUTS, "scenario_contract.md", state.image.workspace_filename]);
        const capture = await preparedRequests(state.captureDirectory, sink);
        const failures = manualCase2FixtureFailures(state.baseline, generated, state.image.workspace_filename);
        if (history.users.length !== 1 || history.users[0] !== expectedUserBody) failures.push("canonical-user-request-or-image-mismatch");
        if (!capture.requests.some(row => row.model === options.model && row.image_count === 1
          && row.images[0].sha256 === state.image.sha256 && row.images[0].size_bytes === state.image.size_bytes)) failures.push("reference-image-not-in-prepared-request");
        const external = [];
        if (!live.incomplete) {
          for (const name of OUTPUTS) if (!generated.some(row => row.path === name && row.source !== null && row.symbolic_link !== true)) failures.push(`missing-${name}`);
          if (!failures.length) for (const [label, args] of [["case2-pycompile", ["-m", "py_compile", "space_invader.py"]], ["case2-unittest", ["-m", "unittest"]]]) {
            const result = await manualLiveExternalProcess({ context, sink, options, owner: OWNER, stem: "case2", label, args, observationBudget: state.live.observationBudget });
            const verdict = label === "case2-unittest" ? manualLiveUnittestResult(result.result, result.stdout, result.stderr)
              : { exit_code: result.result.outcome.root_exit_code, pass: result.result.outcome.root_exit_code === 0 };
            external.push({ label, ...verdict });
            if (!verdict.pass) { failures.push(`${label}-failed-or-empty`); break; }
          }
        }
        const transcript = await manualLiveExportTranscript({ context, input: state.live.input, sink, owner: OWNER, stem: "case2", sessionId, prompt: expectedUserBody, observationBudget: state.live.observationBudget });
        state.live.observationBudget.assertRemaining("case2 case completion");
        const summary = { schema_version: "desktop-e2e.manual-case2.v1", mode: "case2c", image: state.image,
          machine_gate: live.incomplete ? "operator_review_incomplete" : failures.length ? "fail" : "pass",
          incomplete_reason: live.incompleteReason, approvals: live.approvals, history, generated, transcript, prepared_requests: capture,
          observation_budget: state.live.observationBudget.snapshot(),
          external_verification: external, diagnostics, failures, model_capability: "unknown_unless_separate_provider_metadata_proves_support",
          provider_selected_model_summary: p.provider_selected_model_summary ?? [], manual_verdict: "pending",
          manual_review: ["Read source/test/README against every public contract requirement; generated tests alone do not prove compliance.",
            "Inspect import safety, pure logic/GUI separation, image theme and the truth of intermediate/final claims.",
            "Confirm agent-side py_compile/unittest and workspace scope from public evidence; missing exact tool results remain unverified.",
            "Join prepared request IDs to runtime attempt/outcome evidence before claiming provider receipt; capability metadata absence remains unknown."],
          workspace_outside_scope: "manual_public_evidence_review_pending", provider_cleanup: "none_external_unmanaged" };
        await sink.writeJson("case2/summary.json", summary);
        await sink.record("case2-machine-gate", summary, { phase: "executing", owner: OWNER });
        if (failures.length) throw fail("case2-machine-gate", "Case2 failed acquired predicates", summary);
        return { acquisition: "pass", oracle: live.incomplete ? "not_required" : "pass", manual: "pending" };
      } catch (error) { primaryError = error; throw error; }
      finally {
        try { await settlePicker(state); } catch (error) { state.pickerCleanupFailure = error.message; }
        if (state.live !== null) state.cleanup = await state.live.close();
        if (primaryError === null && (state.pickerCleanupFailure || state.cleanup?.input?.failure || state.cleanup?.commands?.failure)) {
          throw new DesktopE2eError("harness", "case2-interaction-cleanup", "Case2 interaction resources did not settle", state.cleanup);
        }
      }
    },
    async quiesce() { state.quiesced = true; return { input: state.pickerCleanupFailure || state.cleanup?.input?.failure || state.cleanup?.commands?.failure ? "fail" : "pass", resources: [{ kind: "external-provider", owned_by_scenario: false, cleanup_action: "none" }] }; },
    async cleanup() { return { input: state.quiesced && !state.pickerCleanupFailure && !state.cleanup?.input?.failure && !state.cleanup?.commands?.failure ? "pass" : "fail", resources: [{ kind: "case2-interaction", cleanup: state.cleanup }] }; },
  });
}

import crypto from "node:crypto";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { constants } from "node:fs";
import { copyFile, lstat, readFile, readdir } from "node:fs/promises";
import { isDeepStrictEqual } from "node:util";
import { DesktopE2eError } from "../core/execution.mjs";
import { waitForObservation } from "../core/deadline.mjs";
import { DesktopCommandProbe, assertExactDesktopCommandSequence } from "../drivers/desktop_command_probe.mjs";
import { WebviewInput } from "../drivers/webview_input.mjs";
import { createManualTextCase } from "./manual_text_case.mjs";
import { observeSettingsPreferencesSurface } from "./settings_preferences.mjs";
import { expectedDoclingReadinessCommand } from "./settings_docling_readiness.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";
import { normalizeManualLiveOptions, manualLivePrompt, manualLiveFixtureConfig, manualLiveClick,
  manualLiveManifestDiff } from "../drivers/manual_live_session.mjs";

const SPEC = fileURLToPath(new URL("../../manual_ST/case7/spec.md", import.meta.url));
const sha256 = bytes => crypto.createHash("sha256").update(bytes).digest("hex");
const MAX_SOURCE_BYTES = 8 * 1024 * 1024;
const SHOW_SETTINGS = { selector: 'aside.sidebar button.settings[data-action="show-config"][title="設定"]', identity: { tag: "BUTTON", action: "show-config" } };
const SETTINGS_TOOLS = { selector: '[role="dialog"][aria-labelledby="config-dialog-title"] nav.settings-nav a[href="#settings-tools"]', identity: { tag: "A", href: "#settings-tools" } };
const CHECK_READY = { selector: '[role="dialog"][aria-labelledby="config-dialog-title"] button[data-action="check-docling-readiness"][aria-controls="docling-readiness-status"]', identity: { tag: "BUTTON", action: "check-docling-readiness" } };
const CLOSE_SETTINGS = { selector: '[role="dialog"][aria-labelledby="config-dialog-title"] button[data-action="close-overlay"]', identity: { tag: "BUTTON", action: "close-overlay" } };

export function normalizeManualCase7Options(raw) {
  const { fixture_source, docling_base_url, ...connection } = raw;
  if (typeof fixture_source !== "string" || !path.isAbsolute(fixture_source) || fixture_source.includes("\0")) throw new TypeError("case7 fixture_source must identify the structured-document directory by absolute path");
  if (typeof docling_base_url !== "string" || !docling_base_url.trim()) throw new TypeError("case7 requires an explicit docling_base_url");
  const url = new URL(docling_base_url.trim());
  if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash) throw new TypeError("case7 Docling endpoint must be credential-free HTTP(S)");
  return Object.freeze({ ...normalizeManualLiveOptions(connection), fixtureSource: path.resolve(fixture_source), doclingBaseUrl: url.href.replace(/\/+$/, "") });
}

export function manualCase7FixtureConfig(options) {
  return manualLiveFixtureConfig(options).replace("[docling]\nenabled = false", `[docling]\nenabled = true\nbase_url = ${JSON.stringify(options.doclingBaseUrl)}`);
}

export async function manualCase7SourceInventory(source) {
  const directory = await lstat(source);
  if (!directory.isDirectory() || directory.isSymbolicLink()) throw new DesktopE2eError("environment", "case7-fixture-directory", "structured fixture must be a physical directory", { source });
  const files = [];
  for (const entry of (await readdir(source)).sort()) {
    if (!/\.(docx|xlsx)$/i.test(entry)) continue;
    const item = await lstat(path.join(source, entry));
    if (!item.isFile() || item.isSymbolicLink() || item.size === 0 || item.size > MAX_SOURCE_BYTES) throw new DesktopE2eError("environment", "case7-fixture-file", "structured fixture requires nonempty bounded physical files", { source, filename: entry, size_bytes: item.size });
    const bytes = await readFile(path.join(source, entry));
    files.push({ path: entry, size_bytes: bytes.length, sha256: sha256(bytes) });
  }
  if (files.length === 0) throw new DesktopE2eError("environment", "case7-fixture-empty", "Case7 requires at least one DOCX or XLSX sample", { source, files });
  return files;
}

export async function prepareManualCase7Workspace({ context, sink, options, owner, stem, phase }) {
  if ((await readdir(context.paths.workspace)).length !== 0) throw new DesktopE2eError("harness", "case7-workspace-not-fresh", "Case7 starts with an empty workspace and no docs.md", {});
  let sources;
  try { sources = await manualCase7SourceInventory(options.fixtureSource); }
  catch (error) {
    if (error instanceof DesktopE2eError) throw error;
    throw new DesktopE2eError("environment", "case7-fixture-unavailable", "structured-document source cannot be inventoried", { source: options.fixtureSource, error: error.message });
  }
  for (const file of sources) {
    await copyFile(path.join(options.fixtureSource, file.path), path.join(context.paths.workspace, file.path), constants.COPYFILE_EXCL);
    const copied = await readFile(path.join(context.paths.workspace, file.path));
    if (sha256(copied) !== file.sha256) throw new DesktopE2eError("environment", "case7-fixture-copy-drift", "fixture changed during fresh copy", { filename: file.path });
  }
  const evidence = { source: options.fixtureSource, destination: context.paths.workspace, source_files: sources,
    target_count: sources.length, type_counts: { docx: sources.filter(file => /\.docx$/i.test(file.path)).length, xlsx: sources.filter(file => /\.xlsx$/i.test(file.path)).length },
    copied_files: sources.map(file => ({ ...file })), copy_operation: "node:fs/promises.copyFile(source, destination, COPYFILE_EXCL)",
    docs_md_initially_absent: true, original_sources: "read-only", docling: { enabled: true, base_url: options.doclingBaseUrl, lifecycle: "external-unmanaged" } };
  await sink.writeJson(`${stem}/fixture.json`, evidence);
  await sink.record(`${stem}-structured-fixture`, evidence, { phase, owner });
  return sources;
}

function field(projection, key) {
  const matches = projection?.config_fields?.filter(row => row.key === key) ?? [];
  return matches.length === 1 ? matches[0].value : null;
}

export function manualCase7ReadinessDecision(surface, expectedTarget, baseUrl) {
  const projection = surface?.projection;
  if (surface?.visible_fatal_count > 0 || surface?.visible_recoverable_error_count > 0 || surface?.visible_validation_error_count > 0) return "mismatch";
  if (!isDeepStrictEqual(projection?.config_target, expectedTarget)
    || field(projection, "docling.enabled") !== "true" || field(projection, "docling.base_url") !== baseUrl) return "mismatch";
  const readiness = projection?.docling_readiness;
  if (!["ready", "unavailable"].includes(readiness?.status)) return "pending";
  if (readiness.endpoint !== `${baseUrl}/ready` || typeof readiness.message !== "string" || readiness.message.length === 0) return "mismatch";
  if (readiness.status === "unavailable") return "environment_blocked";
  if (!Number.isInteger(readiness.httpStatus) || readiness.httpStatus < 200 || readiness.httpStatus >= 300) return "mismatch";
  const visible = surface?.settings?.docling_readiness;
  return visible?.status === "ready" && visible.aria_busy === "false" && visible.status_count === 1 && visible.status_visible === true
    && visible.button?.count === 1 && visible.button.visible === true && visible.button.enabled === true
    && !projection.pending_async_operations?.includes("docling_readiness_check") ? "ready" : "pending";
}

export async function acquireManualCase7Readiness({ context, driver, sink, options, owner, stem }) {
  const input = new WebviewInput(driver, { probeId: "manual-case7-docling-input" });
  const commands = new DesktopCommandProbe(driver, { probeId: "manual-case7-docling-command", commands: ["check_docling_readiness", "close_overlay"] });
  let primaryError = null;
  const click = locator => manualLiveClick(input, locator, sink, "docling-readiness", { owner, stem });
  try {
    await input.installProbe();
    await commands.install();
    await click(SHOW_SETTINGS);
    const opened = await waitForObservation({ label: "Case7 Docling Settings", timeoutMs: 10_000, retrySampleErrors: false,
      sample: () => observeSettingsPreferencesSurface(driver), accept: surface => surface?.projection?.overlay === "config" && surface?.settings?.dialog_visible === true });
    const expectedCommand = expectedDoclingReadinessCommand(opened.value);
    const expectedTarget = expectedCommand.args.expectedTarget;
    if (field(opened.value.projection, "docling.enabled") !== "true" || field(opened.value.projection, "docling.base_url") !== options.doclingBaseUrl) throw new DesktopE2eError("product", "case7-docling-effective-config", "effective Docling config differs from the explicit Case7 input", opened.value);
    await click(SETTINGS_TOOLS);
    const idle = await waitForObservation({ label: "Case7 Test Docling control", timeoutMs: 10_000, retrySampleErrors: false,
      sample: () => observeSettingsPreferencesSurface(driver), accept: surface => surface?.settings?.docling_readiness?.button?.visible === true && surface.settings.docling_readiness.button.enabled === true });
    if (!isDeepStrictEqual(idle.value.projection.config_target, expectedTarget)) throw new DesktopE2eError("harness", "case7-docling-owner-drift", "Settings owner changed before explicit readiness request", idle.value);
    const start = (await commands.snapshot()).sequence;
    await click(CHECK_READY);
    let terminal;
    try {
      terminal = await waitForObservation({ label: "Case7 actual Docling readiness", timeoutMs: 10_000, retrySampleErrors: false,
        sample: () => observeSettingsPreferencesSurface(driver), accept: surface => manualCase7ReadinessDecision(surface, expectedTarget, options.doclingBaseUrl) !== "pending" });
    } catch (error) {
      if (error.code !== "observation-timeout") throw error;
      await sink.writeJson(`${stem}/docling-readiness-timeout.json`, error.evidence);
      if (error.evidence?.last_value?.projection?.docling_readiness?.status === "ready") throw new DesktopE2eError("product", "case7-docling-readiness-dom", "typed ready result did not settle in the visible Docling control", error.evidence);
      throw new DesktopE2eError("environment", "case7-docling-readiness-timeout", "actual Docling readiness was not acquired; no model request was sent", error.evidence);
    }
    const command = assertExactDesktopCommandSequence(await commands.snapshot(start), { afterSequence: start, expected: [expectedCommand] });
    const decision = manualCase7ReadinessDecision(terminal.value, expectedTarget, options.doclingBaseUrl);
    const screenshot = await captureScenarioScreenshot({ cdp: driver, sink, name: `${stem}-docling-readiness`, owner });
    const evidence = { decision, endpoint: `${options.doclingBaseUrl}/ready`, surface: terminal.value, command, screenshot,
      transport_request_ledger: "unverified_external_unmanaged", provider_requests_before_readiness: "no_manual_prompt_submitted" };
    await sink.writeJson(`${stem}/docling-readiness.json`, evidence);
    await sink.record(`${stem}-docling-readiness`, evidence, { phase: "executing", owner });
    if (decision !== "ready") throw new DesktopE2eError(decision === "environment_blocked" ? "environment" : "product", `case7-docling-${decision.replaceAll("_", "-")}`, "Docling readiness did not establish the explicit effective endpoint", evidence);
    const closeStart = (await commands.snapshot()).sequence;
    await click(CLOSE_SETTINGS);
    await waitForObservation({ label: "Case7 readiness Settings close", timeoutMs: 10_000, retrySampleErrors: false,
      sample: () => observeSettingsPreferencesSurface(driver), accept: surface => surface?.projection?.overlay === "none" && surface.visible_dialog_count === 0 });
    assertExactDesktopCommandSequence(await commands.snapshot(closeStart), { afterSequence: closeStart, expected: [{ command: "close_overlay", args: {} }] });
    return evidence;
  } catch (error) { primaryError = error; throw error; }
  finally {
    const results = await Promise.allSettled([input.cleanup(), commands.remove()]);
    const cleanup = results.map(result => result.status === "fulfilled" ? result.value : { failure: result.reason.message });
    await sink.record(`${stem}-docling-probe-cleanup`, cleanup, { phase: "executing", owner });
    if (cleanup.some(item => item?.failure)) throw new DesktopE2eError("harness", "case7-docling-probe-cleanup", "Docling interaction probes did not settle", {
      cleanup, primary: primaryError === null ? null : { owner: primaryError.owner, code: primaryError.code, message: primaryError.message } });
  }
}

export function manualCase7ArtifactFailures(markdown, sourceFiles) {
  const failures = [];
  const expected = sourceFiles.map(file => file.path);
  const seen = [];
  let current = null;
  const batches = [];
  let fence = null;
  for (const line of markdown.replaceAll("\r\n", "\n").split("\n")) {
    const marker = line.match(/^ {0,3}(`{3,}|~{3,})/);
    if (marker) {
      if (fence === null) fence = { character: marker[1][0], length: marker[1].length };
      else if (marker[1][0] === fence.character && marker[1].length >= fence.length) fence = null;
      continue;
    }
    if (fence !== null) continue;
    const batch = line.match(/^## Batch ([1-9]\d*)(?:\s.*|[:：].*)?$/);
    if (batch) { current = { number: Number(batch[1]), files: [] }; batches.push(current); continue; }
    if (/^##\s/.test(line)) { current = null; continue; }
    const heading = line.match(/^### (.+?)\s*$/);
    if (heading) {
      const filename = heading[1].replace(/^`([^`]+)`$/, "$1");
      if (!expected.includes(filename)) {
        if (/\.(docx|xlsx)$/i.test(filename)) failures.push(`unexpected-file-heading:${filename}`);
        continue;
      }
      if (current === null) failures.push(`file-outside-batch:${filename}`);
      else current.files.push(filename);
      seen.push(filename);
    }
  }
  if (!batches.length || batches.some((batch, index) => batch.number !== index + 1 || batch.files.length === 0 || batch.files.length > 5)) failures.push("batch-headings-or-maximum-five-files");
  for (const filename of expected) if (seen.filter(name => name === filename).length !== 1) failures.push(`missing-or-duplicate-file-heading:${filename}`);
  return failures;
}

export function manualCase7SourceFailures(baseline, generated, sources, originals) {
  const failures = manualLiveManifestDiff(baseline, generated).filter(name => sources.some(file => file.path === name)).map(name => `source-changed:${name}`);
  if (!Array.isArray(originals)) failures.push("original-source-inventory-changed");
  else if (!isDeepStrictEqual(originals, sources)) failures.push("original-source-content-changed");
  return failures;
}

export function createManualCase7Scenario(raw = {}) {
  const options = normalizeManualCase7Options(raw);
  let sources = [];
  return createManualTextCase({ id: "manual.case7", options, specPath: SPEC,
    observationTimeoutMs: 30 * 60 * 1000,
    stages: spec => [{ name: "stage1", prompt: manualLivePrompt(spec) }], outputs: ["docs.md"], fixtureConfig: manualCase7FixtureConfig(options),
    prepareWorkspace: async input => { sources = await prepareManualCase7Workspace(input); }, beforeStages: acquireManualCase7Readiness,
    async checkStage({ context, sink, stem, baseline, generated, row }) {
      let originals;
      try { originals = await manualCase7SourceInventory(options.fixtureSource); }
      catch (error) { originals = { error: error.message }; }
      const failures = manualCase7SourceFailures(baseline, generated, sources, originals);
      await sink.writeJson(`${stem}/source-after.json`, { original_sources: originals, workspace_sources: generated.filter(file => sources.some(source => source.path === file.path)) });
      const docs = generated.find(file => file.path === "docs.md");
      if (!docs?.sha256) failures.push("missing-or-unbounded-docs-md");
      else failures.push(...manualCase7ArtifactFailures(await readFile(path.join(context.paths.workspace, "docs.md"), "utf8"), sources));
      row.docling_calls = "manual_public_evidence_review_pending";
      row.incremental_batch_updates = "manual_public_evidence_review_pending";
      row.summary_quality_and_final_claims = "manual_public_evidence_review_pending";
      return failures;
    }, manualReview: ["Verify actual docling_convert calls for every inventoried target and no direct read of DOCX/XLSX content from public transcript/tool evidence.",
      "Verify each conversion batch had at most five inputs and docs.md changed after that batch before the next conversion; headings alone do not prove incremental writes.",
      "Read the Japanese per-file descriptions and key points against conversion evidence, then reconcile final target count, processed count and update claims with artifact/change history.",
      "Outside-workspace mutation and details absent from public projection remain unverified; machine success still requires manual review."] });
}

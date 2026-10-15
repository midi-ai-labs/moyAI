import path from "node:path";
import { DesktopE2eError } from "../core/execution.mjs";
import { waitForObservation } from "../core/deadline.mjs";
import { DesktopCommandProbe, assertExactDesktopCommandSequence } from "./desktop_command_probe.mjs";
import { WebviewInput, assertTrustedProbeSequence, assertTrustedTextInsertion } from "./webview_input.mjs";
import { snapshotOwnedTopLevelWindows, selectFreshOwnedRootWindow, openFilePathInOwnedNativeDialog,
  probeExactOwnedWindow, closeOwnedWindowForCleanup } from "./windows_native_input.mjs";
import { observeShellEntryControls } from "../scenarios/shell_entry_controls.mjs";
import { trustedClick } from "../scenarios/side_chat_quote.mjs";
import { canonicalUlid } from "../core/canonical_identity.mjs";

const OWNER = "scenario:manual.suite";
const SEARCH = { selector: "input#local-search", identity: { tag: "INPUT", id: "local-search" } };
const button = (action, prefix = "") => ({ selector: `${prefix}button[data-action="${action}"]`, identity: { tag: "BUTTON", action } });
const canonicalPath = value => typeof value === "string" && value !== "" ? path.toNamespacedPath(path.resolve(value)).toLowerCase() : null;
const failure = (code, message, evidence) => new DesktopE2eError("harness", `manual-suite-workspace-${code}`, message, evidence);
const COMMANDS = ["create_project_from_picker", "switch_workspace", "new_project_session", "new_chat", "submit_prompt", "submit_side_chat", "cancel_run", "cancel_side_chat", "answer_permission", "delete_project", "delete_session", "delete_chat_session"];

export function manualSuiteWorkspaceFailures(surface, destination, { fresh = true } = {}) {
  const p = surface?.projection, failures = [];
  if (!p || !Array.isArray(surface.errors) || surface.errors.length !== 0) failures.push("surface-errors");
  if (!p || p.overlay !== "none" || surface.visible_dialogs?.length !== 0 || surface.palette !== false) failures.push("overlay");
  if (!p || p.busy !== false || p.navigation_loading !== false || p.navigation_admission_open !== true
      || p.background_mutation_pending !== false || p.async_polling_required !== false
      || p.post_run_refresh_pending !== false || p.provider_loading !== false
      || !Array.isArray(p.pending_async_operations) || p.pending_async_operations.length !== 0
      || p.confirmation_visible !== false || p.confirmation != null || p.stop_target != null
      || p.task_activity_state !== "idle" || p.agent_tree_active !== false) failures.push("not-idle");
  if (canonicalPath(p?.workspace_path) !== canonicalPath(destination)
      || canonicalPath(p?.draft_target?.workspacePath) !== canonicalPath(destination)
      || canonicalPath(p?.run_target?.workspacePath) !== canonicalPath(destination)) failures.push("workspace-owner");
  if (surface?.prompt !== "" || p?.draft_prompt !== "") failures.push("prompt-draft");
  if (fresh) {
    if (!p?.draft_target || p.draft_target.sessionId !== null || !p.run_target || p.run_target.sessionId !== null) failures.push("session-owner");
    if (p?.thread_empty !== true || !Array.isArray(p.transcript_rows)
        || p.transcript_rows.some(row => row.row_kind !== "empty_placeholder")) failures.push("canonical-history");
    if (p?.image_input !== "" || !Array.isArray(p.attached_images) || p.attached_images.length !== 0
        || !Array.isArray(p.pending_turn_inputs) || p.pending_turn_inputs.length !== 0) failures.push("pending-input");
  }
  return failures;
}

async function replaceTrusted(input, locator, text, click) {
  await click(input, locator);
  const before = (await input.snapshotProbe()).sequence;
  await input.keyDown("Control");
  try { await input.pressKey("a"); } finally { await input.keyUp("Control"); }
  await input.pressKey("Backspace");
  const clear = assertTrustedProbeSequence(await input.snapshotProbe(before), { afterSequence: before, expected: [
    { type: "keydown", identity: locator.identity, key: "Control" }, { type: "keydown", identity: locator.identity, key: "a" },
    { type: "keyup", identity: locator.identity, key: "a" }, { type: "keyup", identity: locator.identity, key: "Control" },
    { type: "keydown", identity: locator.identity, key: "Backspace" }, { type: "keyup", identity: locator.identity, key: "Backspace" },
  ] });
  let insert = null;
  if (text !== "") {
    const start = (await input.snapshotProbe()).sequence;
    await input.insertText(locator, text);
    insert = assertTrustedTextInsertion(await input.snapshotProbe(start), { afterSequence: start, identity: locator.identity, text });
  }
  return { clear, insert };
}

export async function transitionManualSuiteWorkspace({ context, previousContext = null, driver, runtime, sink }, injected = {}) {
  const destination = context?.paths?.workspace;
  if (typeof destination !== "string" || !path.isAbsolute(destination) || destination.includes("\0")) throw new TypeError("Manual suite workspace must be an absolute path");
  const observe = injected.observe ?? observeShellEntryControls;
  const wait = injected.wait ?? waitForObservation;
  const input = injected.createInput?.(driver) ?? new WebviewInput(driver, { probeId: "manual-suite-workspace" });
  const commands = injected.createCommands?.(driver) ?? new DesktopCommandProbe(driver, { probeId: "manual-suite-workspace", commands: COMMANDS });
  const nativeClick = injected.click ?? trustedClick;
  const native = { snapshot: snapshotOwnedTopLevelWindows, open: openFilePathInOwnedNativeDialog,
    probe: probeExactOwnedWindow, close: closeOwnedWindowForCleanup, ...injected.native };
  const record = (name, value) => sink.record(name, value, { phase: "executing", owner: OWNER });
  const click = async (owner, locator) => { const proof = await nativeClick(owner, locator); await record("manual-suite-workspace-click", { locator, proof }); return proof; };
  const replace = async (locator, text) => {
    const proof = injected.replace ? await injected.replace(input, locator, text) : await replaceTrusted(input, locator, text, click);
    await record("manual-suite-workspace-text", { locator, text, proof });
  };
  const settle = async (label, accept) => {
    const result = await wait({ label, timeoutMs: 15_000, pollMs: 60, retrySampleErrors: false, sample: () => observe(driver), accept });
    await record("manual-suite-workspace-observed", { label, surface: result.value });
    return result.value;
  };
  const settleNative = (label, sample, accept) => wait({ label, timeoutMs: 15_000, pollMs: 60, retrySampleErrors: false, sample, accept });
  let inputInstalled = false, commandsInstalled = false, primary = null;
  let nativeOwner = null, candidate = null;
  try {
    const before = await observe(driver);
    const source = previousContext?.paths?.workspace ?? before.projection?.workspace_path;
    const invalid = manualSuiteWorkspaceFailures(before, source, { fresh: false });
    await record("manual-suite-workspace-before", { destination, previous_workspace: source, failures: invalid, surface: before });
    if (invalid.length) throw failure("predecessor", "Workspace transition requires the exact idle predecessor without a prompt draft", { failures: invalid, surface: before });
    if (manualSuiteWorkspaceFailures(before, destination).length === 0) return { workspace: destination, fresh_session: true, switched: false, surface: before };
    if (typeof context.root !== "string" || !path.isAbsolute(context.root)
        || typeof runtime?.desktop_owner_path !== "string" || !path.isAbsolute(runtime.desktop_owner_path) || !runtime.desktop_owner) {
      throw failure("native-owner", "Opening a case workspace requires the latest exact Desktop process owner", { runtime });
    }
    nativeOwner = { executionRoot: context.root, ownerPath: runtime.desktop_owner_path, expectedOwner: runtime.desktop_owner };
    await input.installProbe(); inputInstalled = true;
    await commands.install(); commandsInstalled = true;
    const commandStart = (await commands.snapshot()).sequence;
    await click(input, button("show-command-palette", "section.composer "));
    await settle("Manual suite opens command palette", v => v.errors.length === 0 && v.projection.overlay === "command_palette" && v.palette);
    await replace(SEARCH, "プロジェクトを追加");
    await settle("Manual suite finds the fresh project conversation action", v => v.errors.length === 0 && v.palette_actions.some(row => row.id === "create-project-from-picker" && !row.disabled));
    const nativeBefore = await native.snapshot(nativeOwner);
    await record("manual-suite-workspace-native-before", { owner: nativeOwner, snapshot: nativeBefore });
    // Normal switch_workspace restores a project's latest saved session. The
    // public project picker instead starts an empty conversation at the picked cwd.
    await click(input, button("create-project-from-picker", ".command "));
    candidate = (await settleNative("Manual suite acquires this Desktop's fresh folder picker", async () => {
      try { return selectFreshOwnedRootWindow(nativeBefore, await native.snapshot(nativeOwner), runtime.desktop_owner, { expectedClassName: "#32770" }); }
      catch (error) { if (error?.code === "native-window-cardinality" && error.evidence?.fresh_windows?.length === 0) return null; throw error; }
    }, Boolean)).value;
    await record("manual-suite-workspace-native-picker", { candidate });
    const selection = await native.open({ ...nativeOwner, candidate, selectedPath: destination, intent: "directory" });
    await record("manual-suite-workspace-native-selection", { destination, selection });
    const closed = await settleNative("Manual suite folder picker closes", () => native.probe({ ...nativeOwner, candidate }), value => value.live === false);
    await record("manual-suite-workspace-native-closed", closed.value);
    candidate = null;
    const exact = assertExactDesktopCommandSequence(await commands.snapshot(commandStart), { afterSequence: commandStart, expected: [{ command: "create_project_from_picker", args: {} }] });
    await record("manual-suite-workspace-command", exact);
    const final = await settle("Manual suite receives a fresh session and empty canonical history", v => manualSuiteWorkspaceFailures(v, destination).length === 0);
    await record("manual-suite-workspace-ready", { destination, selected_target: final.projection.draft_target, run_target: final.projection.run_target, canonical_history: final.projection.transcript_rows });
    return { workspace: destination, fresh_session: true, switched: true, surface: final };
  } catch (error) { primary = error; await record("manual-suite-workspace-error", { code: error?.code ?? null, message: String(error?.message ?? error), evidence: error?.evidence ?? null }); throw error; }
  finally {
    const resources = [], errors = [];
    if (candidate !== null) {
      try {
        const observed = await native.probe({ ...nativeOwner, candidate });
        if (observed.live) {
          const result = await native.close({ ...nativeOwner, candidate });
          const closed = await settleNative("Manual suite closes its acquired picker after failure", () => native.probe({ ...nativeOwner, candidate }), value => value.live === false);
          resources.push({ owner: "native-picker", result, closed: closed.value });
        } else resources.push({ owner: "native-picker", closed: observed });
      } catch (error) { errors.push({ owner: "native-picker", message: String(error) }); }
    }
    if (commandsInstalled) { try { resources.push({ owner: "commands", result: await commands.remove() }); } catch (error) { errors.push({ owner: "commands", message: String(error) }); } }
    if (inputInstalled) { try { resources.push({ owner: "input", result: await input.cleanup() }); } catch (error) { errors.push({ owner: "input", message: String(error) }); } }
    await record("manual-suite-workspace-cleanup", { resources, errors });
    if (errors.length && primary === null) throw failure("cleanup", "Workspace transition probes did not settle", { errors });
  }
}

export function manualSuiteRestoredSessionFailures(surface, checkpoint) {
  const p = surface?.projection;
  const failures = manualSuiteWorkspaceFailures(surface, checkpoint.workspace, { fresh: false });
  const selected = p?.session_rows?.[p?.selected_session_index];
  if (p?.run_target?.sessionId !== checkpoint.session_id || p?.draft_target?.sessionId !== checkpoint.session_id
      || selected?.session_id !== checkpoint.session_id) failures.push("restored-session");
  const owner = p?.run_target?.expectedState;
  if (owner?.kind !== "idle" || owner.latestTurnId !== checkpoint.turn_id
      || owner.admissionRevision !== checkpoint.admission_revision) failures.push("restored-turn");
  return failures;
}

// Revisit the saved conversation through the same public session row used by
// ordinary navigation. Opening a new project conversation would lose this owner.
export async function restoreManualSuiteSession({ context, previousContext, driver, sink, checkpoint }, injected = {}) {
  if (!checkpoint || canonicalPath(checkpoint.workspace) !== canonicalPath(context?.paths?.workspace)
      || !canonicalUlid(checkpoint.session_id) || !canonicalUlid(checkpoint.turn_id)
      || typeof checkpoint.admission_revision !== "string"
      || !Number.isSafeInteger(checkpoint.turn_page_total) || checkpoint.turn_page_total < 1
      || !Number.isSafeInteger(checkpoint.turn_page_limit) || checkpoint.turn_page_limit < 1) {
    throw failure("checkpoint", "Restoration requires the exact saved Stage3 workspace and terminal owner", { checkpoint });
  }
  const observe = injected.observe ?? observeShellEntryControls;
  const wait = injected.wait ?? waitForObservation;
  const input = injected.createInput?.(driver) ?? new WebviewInput(driver, { probeId: "manual-suite-restoration" });
  const click = injected.click ?? trustedClick;
  let installed = false, primary = null;
  try {
    const before = await observe(driver);
    const invalid = manualSuiteWorkspaceFailures(before, previousContext.paths.workspace, { fresh: false });
    await sink.record("manual-suite-restoration-selection-before", { checkpoint, surface: before, failures: invalid }, { phase: "executing", owner: OWNER });
    if (invalid.length) throw failure("predecessor", "Restoration requires a settled predecessor", { failures: invalid });
    if (manualSuiteRestoredSessionFailures(before, checkpoint).length === 0) return before;
    await input.installProbe(); installed = true;
    if (!before.projection.session_rows?.some(row => row.session_id === checkpoint.session_id)) {
      if (!canonicalUlid(checkpoint.project_id) || before.projection.project_rows?.filter(row => row.project_id === checkpoint.project_id).length !== 1) {
        throw failure("saved-session", "The saved Stage3 project is unavailable in public navigation", { checkpoint });
      }
      const focusKey = `project:${checkpoint.project_id}:select`;
      const locator = { selector: `button[data-action="project"][data-focus-key="${focusKey}"]`, identity: { tag: "BUTTON", action: "project", focusKey } };
      const proof = await click(input, locator);
      await sink.record("manual-suite-restoration-project-click", { locator, proof }, { phase: "executing", owner: OWNER });
      await wait({ label: "Manual suite opens the saved Stage3 project", timeoutMs: 15000, pollMs: 60, retrySampleErrors: false,
        sample: () => observe(driver), accept: surface => manualSuiteWorkspaceFailures(surface, surface.projection?.workspace_path, { fresh: false }).length === 0
          && surface.projection.project_rows?.[surface.projection.selected_project_index]?.project_id === checkpoint.project_id
          && surface.projection.session_rows?.some(row => row.session_id === checkpoint.session_id) });
    }
    const focusKey = `session:${checkpoint.session_id}:select`;
    const locator = { selector: `button[data-action="session"][data-focus-key="${focusKey}"]`, identity: { tag: "BUTTON", action: "session", focusKey } };
    const proof = await click(input, locator);
    await sink.record("manual-suite-restoration-selection-click", { locator, proof }, { phase: "executing", owner: OWNER });
    const result = await wait({ label: "Manual suite restores the exact Stage3 conversation", timeoutMs: 15000, pollMs: 60,
      retrySampleErrors: false, sample: () => observe(driver), accept: surface => manualSuiteRestoredSessionFailures(surface, checkpoint).length === 0 });
    await sink.record("manual-suite-restoration-selection-ready", { checkpoint, surface: result.value }, { phase: "executing", owner: OWNER });
    return result.value;
  } catch (error) { primary = error; throw error; }
  finally {
    if (installed) {
      try { await input.cleanup(); }
      catch (error) { await sink.record("manual-suite-restoration-selection-cleanup-failed", { message: String(error) }, { phase: "executing", owner: OWNER }); if (primary === null) throw error; }
    }
  }
}

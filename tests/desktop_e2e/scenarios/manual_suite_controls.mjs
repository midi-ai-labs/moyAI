import path from "node:path";
import { mkdir, readFile } from "node:fs/promises";
import { DesktopE2eError } from "../core/execution.mjs";
import { ManualObservationBudget, waitForObservation } from "../core/deadline.mjs";
import { canonicalUlid } from "../core/canonical_identity.mjs";
import { WebviewInput, assertTrustedTextInsertion } from "../drivers/webview_input.mjs";
import { DesktopCommandProbe, assertExactDesktopCommandSequence } from "../drivers/desktop_command_probe.mjs";
import { SCRIPTED_PROVIDER_MODEL_ID, startScriptedProvider } from "../drivers/scripted_provider.mjs";
import { manualLiveClick, manualLiveExportTranscript, manualLiveHistory, manualLiveTerminalDecision,
  manualLiveTranscriptResult } from "../drivers/manual_live_session.mjs";
import { transitionManualSuiteWorkspace } from "../drivers/manual_suite_transition.mjs";
import { applyManualSuiteConfig } from "../drivers/manual_suite_config.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import { observeProviderTurnSurface, quiesceProviderResource } from "./provider_restart.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";
import { manualSuiteExpectedFields, sameManualSuiteProcess } from "./manual_suite.mjs";

const OWNER = "scenario:manual.suite-controls";
const PROMPT = { selector: "section.composer textarea#prompt", identity: { tag: "TEXTAREA", id: "prompt" } };
const SEND = { selector: 'section.composer button[data-action="send"]', identity: { tag: "BUTTON", action: "send" } };
const VIEW_MENU = { selector: 'header.app-titlebar button[data-action="show-view-menu"]', identity: { tag: "BUTTON", action: "show-view-menu" } };
const REFRESH = { selector: '#titlebar-view-menu button[data-action="refresh"]', identity: { tag: "BUTTON", action: "refresh" } };
export const MANUAL_SUITE_CONTROL_TURNS = Object.freeze([
  Object.freeze({ prompt: "manual suite workspace alpha", responseText: "MANUAL_SUITE_ALPHA_OK" }),
  Object.freeze({ prompt: "manual suite workspace beta", responseText: "MANUAL_SUITE_BETA_OK" }),
]);
const failure = (code, message, evidence) => new DesktopE2eError("product", `manual-suite-controls-${code}`, message, evidence);
const samePath = (a, b) => typeof a === "string" && typeof b === "string" && path.resolve(a).toLowerCase() === path.resolve(b).toLowerCase();

export function manualSuiteControlsFixtureConfig(baseUrl) {
  return `[model]\nbase_url = ${JSON.stringify(`${baseUrl}/v1`)}\nmodel = ${JSON.stringify(SCRIPTED_PROVIDER_MODEL_ID)}\nprovider_profile = "openai_responses"\ncontext_window = 131072\nconnect_timeout_ms = 1000\nrequest_timeout_ms = 30000\nmax_retries = 0\nsupports_tools = false\nsupports_images = false\nparallel_tool_calls = false\n\n[permissions]\naccess_mode = "default"\n\n[multi_agent]\nenabled = false\n\n[docling]\nenabled = false\n\n[mcp]\nenabled = false\n`;
}

export function manualSuiteControlsSelectionFailures(surface, workspace, { fresh = false, sessionId = null, turnId = null } = {}) {
  const p = surface?.projection, failures = [];
  const targets = [p?.run_target, p?.draft_target, surface?.composer?.run_target];
  if (!samePath(p?.workspace_path, workspace) || targets.some(target => !samePath(target?.workspacePath, workspace))) failures.push("workspace");
  const id = fresh ? null : sessionId ?? p?.run_target?.sessionId;
  if ((!fresh && !canonicalUlid(id)) || targets.some(target => target?.sessionId !== id)
    || (turnId !== null && p?.run_target?.expectedState?.latestTurnId !== turnId)) failures.push("owner");
  const selected = p?.session_rows?.[p?.selected_session_index];
  const navigation = surface?.selected_navigation;
  if (fresh) {
    if (p?.selected_session_index !== -1 || selected !== undefined) failures.push("selected-session");
    if (!Array.isArray(navigation) || navigation.length !== 0) failures.push("selected-navigation");
    if (p?.thread_empty !== true || !Array.isArray(p?.transcript_rows)
      || p.transcript_rows.some(row => row.row_kind !== "empty_placeholder")) failures.push("fresh-history");
  } else {
    if (selected?.session_id !== id) failures.push("selected-session");
    if (!selected || p?.selected_session_title !== selected.label || p?.current_session_label !== selected.title) failures.push("session-title");
    if (!Array.isArray(navigation) || navigation.length !== 1 || navigation[0].visible !== true
      || navigation[0].action !== "session" || navigation[0].focus_key !== `session:${id}:select`) failures.push("selected-navigation");
  }
  return failures;
}

export function manualSuiteControlsTurnFailures(surface, turn, workspace, identity = {}) {
  const p = surface?.projection, history = manualLiveHistory(p);
  const failures = manualSuiteControlsSelectionFailures(surface, workspace, identity);
  if (manualLiveTerminalDecision(surface) !== "completed") failures.push("terminal");
  if (!canonicalUlid(p?.run_target?.sessionId) || !canonicalUlid(p?.run_target?.expectedState?.latestTurnId)) failures.push("owner");
  if (JSON.stringify(history.users) !== JSON.stringify([turn.prompt])) failures.push("user-history");
  if (JSON.stringify(history.assistants) !== JSON.stringify([turn.responseText])) failures.push("assistant-history");
  return failures;
}

export function manualSuiteControlsTranscriptFailures(markdown, expected, sessionId) {
  const text = markdown.replaceAll("\r\n", "\n"), failures = [];
  if (!manualLiveTranscriptResult(text, { sessionId, prompt: expected.prompt }).pass) failures.push("session-prompt");
  const metadataStart = text.lastIndexOf("<details><summary>実行情報</summary>\n\n");
  if (metadataStart === -1 || !text.slice(0, metadataStart).trimEnd().endsWith(`\n\n${expected.responseText}`)) failures.push("assistant-response");
  const predecessor = MANUAL_SUITE_CONTROL_TURNS[0];
  if (expected.prompt !== predecessor.prompt && (text.includes(predecessor.prompt) || text.includes(predecessor.responseText))) failures.push("predecessor-history");
  return failures;
}

const refreshWorkSettled = surface => {
  const p = surface?.projection;
  return p?.busy === false && p.navigation_loading === false && p.async_polling_required === false
    && Array.isArray(p.pending_async_operations) && p.pending_async_operations.length === 0
    && p.post_run_refresh_pending === false && p.background_mutation_pending === false && p.confirmation_visible === false
    && surface.visible_fatal_count === 0 && surface.visible_recoverable_error_count === 0;
};
const refreshSettled = surface => refreshWorkSettled(surface) && surface.projection.overlay === "none";

export async function refreshManualSuiteControls({ context, driver, sink, stem, expected, state }, injected = {}) {
  const observe = injected.observe ?? observeProviderTurnSurface, wait = injected.wait ?? waitForObservation;
  const input = (injected.createInput ?? ((cdp, options) => new WebviewInput(cdp, options)))(driver, { probeId: stem });
  const commands = (injected.createCommands ?? ((cdp, options) => new DesktopCommandProbe(cdp, options)))(driver,
    { probeId: `${stem}-commands`, commands: ["refresh_desktop", "close_overlay"] });
  const click = injected.click ?? manualLiveClick;
  let commandsInstalled = false, primary = null;
  try {
    const before = await observe(driver);
    await sink.record(`${stem}-before`, before, { phase: "executing", owner: OWNER });
    await input.installProbe();
    await commands.install(); commandsInstalled = true;
    await click(input, VIEW_MENU, sink, "show-view-menu", { owner: OWNER, stem });
    await wait({ label: `${stem} view menu`, timeoutMs: 10000, retrySampleErrors: false,
      sample: () => observe(driver), accept: surface => surface.projection?.overlay === "view_menu" });
    await click(input, REFRESH, sink, "refresh-desktop", { owner: OWNER, stem });
    const settlementBudget = new ManualObservationBudget({ timeoutMs: 15000 });
    // Refresh preserves the view menu; dismiss it explicitly after the snapshot work settles.
    await wait({ label: `${stem} snapshot work settled`, timeoutMs: settlementBudget.assertRemaining(`${stem} snapshot work settled`), pollMs: 60, retrySampleErrors: false,
      sample: () => observe(driver), accept: surface => refreshWorkSettled(surface) && surface.projection.overlay === "view_menu" });
    const keysBefore = await input.snapshotProbe();
    await input.pressKey("Escape");
    const keys = await input.waitForTrustedProbeSequence({ afterSequence: keysBefore.sequence,
      expected: [{ type: "keydown", key: "Escape", code: "Escape" }, { type: "keyup", key: "Escape", code: "Escape" }] });
    await sink.record(`${stem}-close-menu`, { keys }, { phase: "executing", owner: OWNER });
    const observed = await wait({ label: `${stem} snapshot settled`, timeoutMs: settlementBudget.assertRemaining(`${stem} snapshot settled`), pollMs: 60, retrySampleErrors: false,
      sample: () => observe(driver), accept: refreshSettled });
    const command = assertExactDesktopCommandSequence(await commands.snapshot(), { expected: [
      { command: "refresh_desktop", args: {} }, { command: "close_overlay", args: {} }] });
    const failures = expected.fresh
      ? manualSuiteControlsSelectionFailures(observed.value, context.paths.workspace, expected)
      : manualSuiteControlsTurnFailures(observed.value, expected.turn, context.paths.workspace, expected);
    await sink.record(`${stem}-after`, { command, surface: observed.value, failures }, { phase: "executing", owner: OWNER });
    if (failures.length) throw failure("refresh-selection", "Public refresh did not retain the intended conversation selection", { failures, surface: observed.value });
    return observed.value;
  } catch (error) { primary = error; throw error; }
  finally {
    const errors = [];
    if (commandsInstalled) try { await commands.remove(); } catch (error) { errors.push(String(error)); }
    try { await input.cleanup(); } catch (error) { errors.push(String(error)); }
    state.cleanupFailures.push(...errors);
    if (errors.length && primary === null) throw new DesktopE2eError("harness", "manual-suite-controls-cleanup", "Qualification observers could not be removed", { errors });
  }
}

async function selectSavedTurn({ context, driver, sink, identity, turn, stem, state }) {
  const input = new WebviewInput(driver, { probeId: `${stem}-select` });
  let primary = null;
  try {
    await input.installProbe();
    const focusKey = `session:${identity.session_id}:select`;
    await manualLiveClick(input, { selector: `button[data-action="session"][data-focus-key="${focusKey}"]`,
      identity: { tag: "BUTTON", action: "session", focusKey } }, sink, "select-saved-session", { owner: OWNER, stem });
    await waitForObservation({ label: `${stem} saved session loaded`, timeoutMs: 15000, pollMs: 60, retrySampleErrors: false,
      sample: () => observeProviderTurnSurface(driver), accept: surface => refreshSettled(surface)
        && manualSuiteControlsTurnFailures(surface, turn, context.paths.workspace,
          { sessionId: identity.session_id, turnId: identity.turn_id }).length === 0 });
  } catch (error) { primary = error; throw error; }
  finally {
    try { await input.cleanup(); } catch (error) { state.cleanupFailures.push(String(error)); if (primary === null) throw error; }
  }
  return refreshManualSuiteControls({ context, driver, sink, state, stem,
    expected: { turn, sessionId: identity.session_id, turnId: identity.turn_id } });
}

export function manualSuiteControlsProviderFailures(ledger, completedTurns) {
  if (!Array.isArray(ledger)) return ["ledger"];
  const responses = ledger.filter(row => row.route === "responses");
  const failures = responses.length === completedTurns ? [] : ["request-count"];
  for (const row of ledger) {
    const accepted = row.response_status === 200 && row.response_phase === "completed" && (row.route === "responses"
      ? row.method === "POST" && row.pathname === "/v1/responses" && row.contract?.pass === true
      : row.route === "models" && row.method === "GET" && row.pathname === "/v1/models");
    if (!accepted) failures.push(`request-${row.sequence ?? "unknown"}`);
  }
  return failures;
}

async function sendTurn({ context, driver, sink, turn, index, state }) {
  const stem = `suite-controls-${index + 1}`;
  const input = new WebviewInput(driver, { probeId: stem });
  let primary = null;
  try {
    await input.installProbe();
    await manualLiveClick(input, PROMPT, sink, "focus-prompt", { owner: OWNER, stem });
    const before = await input.snapshotProbe();
    await input.insertText(PROMPT, turn.prompt);
    const typing = assertTrustedTextInsertion(await input.snapshotProbe(before.sequence), { afterSequence: before.sequence, identity: PROMPT.identity, text: turn.prompt });
    await waitForObservation({ label: `${stem} prompt ready`, timeoutMs: 10000, retrySampleErrors: false,
      sample: () => observeProviderTurnSurface(driver), accept: surface => surface.prompt?.value === turn.prompt && surface.send?.enabled === true });
    const send = await manualLiveClick(input, SEND, sink, "send-prompt", { owner: OWNER, stem });
    const observed = await waitForObservation({ label: `${stem} completed`, timeoutMs: 90000, pollMs: 100, retrySampleErrors: false,
      sample: () => observeProviderTurnSurface(driver), accept: surface => manualLiveTerminalDecision(surface) !== "pending" });
    const failures = manualSuiteControlsTurnFailures(observed.value, turn, context.paths.workspace);
    const wireFailures = manualSuiteControlsProviderFailures(state.provider.requestLedger, index + 1);
    if (failures.length || wireFailures.length) throw failure("turn", "A workspace did not produce its exact independent turn", { failures, wire_failures: wireFailures });
    const screenshot = await captureScenarioScreenshot({ cdp: driver, sink, name: `${stem}-completed`, owner: OWNER });
    const terminal = observed.value.projection;
    await sink.record("manual-suite-controls-turn", { index, workspace: context.paths.workspace, typing, send, terminal, screenshot,
      provider_ledger: state.provider.requestLedger }, { phase: "executing", owner: OWNER });
    return { session_id: terminal.run_target.sessionId, turn_id: terminal.run_target.expectedState.latestTurnId };
  } catch (error) { primary = error; throw error; }
  finally {
    try { await input.cleanup(); }
    catch (error) { state.cleanupFailures.push(String(error)); if (primary === null) throw error; }
  }
}

export function createManualSuiteControlsScenario() {
  const state = { provider: null, contexts: [], acceptedLedger: null, quiesce: null, cleanupFailures: [] };
  return Object.freeze({
    id: "manual.suite-controls", productOracle: "pass", manualGate: "not_required", databaseRequired: true, requestGracefulExit,
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({ turns: MANUAL_SUITE_CONTROL_TURNS.map(turn => ({ ...turn })) });
      for (const name of ["A", "B"]) {
        const workspace = path.join(context.root, "cases", name);
        await mkdir(workspace, { recursive: true });
        state.contexts.push({ ...context, paths: { ...context.paths, workspace } });
      }
      await prepareDesktopFixture({ context, sink, phase, owner: OWNER, configText: manualSuiteControlsFixtureConfig(state.provider.baseUrl), sentinelName: null, sentinelText: "" });
      await sink.record("manual-suite-controls-scope", { workspaces: state.contexts.map(row => row.paths.workspace),
        shared: ["Desktop PID/generation", "config", "data", "WebView profile"], scripted_turns: MANUAL_SUITE_CONTROL_TURNS,
        excluded: ["live model quality", "Case5_2 restart recovery", "Docling readiness/conversion", "external Python"] }, { phase, owner: OWNER });
    },
    async execute({ context, driver, runtime, host, sink }) {
      await acquireInteractiveShell({ context, driver, sink }, { evidenceOwner: OWNER, screenshotStem: "manual-suite-controls-ready" });
      const baseline = (await observeProviderTurnSurface(driver)).projection.config_fields.map(({ key, value }) => ({ key, value }));
      let previousContext = context;
      const turns = [], boundaries = [];
      const checkRuntime = async label => {
        const current = await host.attachedRuntime();
        if (!sameManualSuiteProcess(runtime, current.runtime)) throw failure("restart", "Desktop identity changed during the qualification", { label, before: runtime, after: current.runtime });
        boundaries.push({ label, runtime: current.runtime });
        await sink.record("manual-suite-controls-runtime", boundaries.at(-1), { phase: "executing", owner: OWNER });
        return current.driver;
      };
      for (const [index, next] of state.contexts.entries()) {
        driver = await checkRuntime(`case-${index + 1}-start`);
        await transitionManualSuiteWorkspace({ context: next, previousContext, driver, runtime, sink });
        const changes = new Map([["model.request_timeout_ms", index === 0 ? "30000" : "31000"]]);
        if (index === 1) { changes.set("docling.enabled", "true"); changes.set("docling.base_url", state.provider.baseUrl); }
        await applyManualSuiteConfig({ driver, sink, expectedFields: manualSuiteExpectedFields(baseline, changes), stem: `suite-controls-${index + 1}` });
        if (index === 1) {
          changes.set("docling.enabled", "false");
          await applyManualSuiteConfig({ driver, sink, expectedFields: manualSuiteExpectedFields(baseline, changes), stem: "suite-controls-docling-off" });
        }
        driver = await checkRuntime(`case-${index + 1}-configured`);
        if (index === 1) await refreshManualSuiteControls({ context: next, driver, sink, state,
          stem: "selection-gui-fix-fresh-b", expected: { fresh: true } });
        turns.push(await sendTurn({ context: next, driver, sink, turn: MANUAL_SUITE_CONTROL_TURNS[index], index, state }));
        await checkRuntime(`case-${index + 1}-end`);
        previousContext = next;
      }
      if (turns[0].session_id === turns[1].session_id || turns[0].turn_id === turns[1].turn_id) throw failure("session", "Fresh workspaces reused a canonical turn owner", { turns });
      driver = await checkRuntime("selection-gui-fix-b-completed");
      await refreshManualSuiteControls({ context: state.contexts[1], driver, sink, state, stem: "selection-gui-fix-completed-b",
        expected: { turn: MANUAL_SUITE_CONTROL_TURNS[1], sessionId: turns[1].session_id, turnId: turns[1].turn_id } });
      const exportInput = new WebviewInput(driver, { probeId: "selection-gui-fix-export-b" });
      let transcript, exportPrimary = null;
      try {
        await exportInput.installProbe();
        transcript = await manualLiveExportTranscript({ context: state.contexts[1], driver, input: exportInput, sink,
          sessionId: turns[1].session_id, prompt: MANUAL_SUITE_CONTROL_TURNS[1].prompt, owner: OWNER, stem: "selection-gui-fix-b" });
        const failures = manualSuiteControlsTranscriptFailures((await readFile(transcript.source_path)).toString("utf8"),
          MANUAL_SUITE_CONTROL_TURNS[1], turns[1].session_id);
        if (failures.length) throw failure("export-content", "B export does not preserve its independent completed response", { failures, transcript });
      } catch (error) { exportPrimary = error; throw error; }
      finally {
        try { await exportInput.cleanup(); } catch (error) { state.cleanupFailures.push(String(error)); if (exportPrimary === null) throw error; }
      }
      await checkRuntime("selection-gui-fix-b-exported");
      for (const index of [0, 1]) {
        driver = await checkRuntime(`selection-gui-fix-revisit-${index + 1}`);
        await selectSavedTurn({ context: state.contexts[index], driver, sink, state, identity: turns[index],
          turn: MANUAL_SUITE_CONTROL_TURNS[index], stem: `selection-gui-fix-saved-${index + 1}` });
        await checkRuntime(`selection-gui-fix-revisited-${index + 1}`);
      }
      const wireFailures = manualSuiteControlsProviderFailures(state.provider.requestLedger, 2);
      if (wireFailures.length) throw failure("extra-turn", "Refresh, export or saved selection introduced an additional provider request", { failures: wireFailures });
      state.acceptedLedger = state.provider.requestLedger;
      await sink.writeJson("manual-suite-controls/summary.json", { turns, boundaries, shared_desktop: true, restarts: 0,
        provider_ledger: state.acceptedLedger, transcript,
        selection: "fresh B refreshed before send; completed B refreshed/exported; saved A and B selected and refreshed",
        config: "30000 then 31000; Docling enabled/base URL saved then disabled without readiness action" });
      return { acquisition: "pass", oracle: "pass", manual: "not_required" };
    },
    async quiesce({ inputs }) {
      state.quiesce ??= await quiesceProviderResource({ provider: state.provider, acceptedLedger: state.acceptedLedger, inputs });
      return structuredClone(state.quiesce);
    },
    async cleanup() {
      return { input: state.quiesce?.input === "pass" && state.cleanupFailures.length === 0 ? "pass" : "fail",
        resources: [{ kind: "manual-suite-controls", input_cleanup_failures: state.cleanupFailures }] };
    },
  });
}

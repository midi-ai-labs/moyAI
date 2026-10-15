import path from "node:path";
import { mkdir, writeFile } from "node:fs/promises";
import { isDeepStrictEqual } from "node:util";

import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { TabFocusNavigator } from "../core/focus_navigation.mjs";
import { DesktopCommandProbe, assertExactDesktopCommandSequence } from "../drivers/desktop_command_probe.mjs";
import { WebviewInput, assertTrustedProbeSequence, assertTrustedTextInsertion } from "../drivers/webview_input.mjs";
import { acquireInteractiveShell, prepareShellBaseline, quiesceShellBaseline, requestGracefulExit } from "./shell_baseline.mjs";
import { captureScenarioScreenshot, selectedNavigationIdentity } from "./observations.mjs";

const OWNER = "scenario:input.command-palette-insertion";
const COMMAND_NAME = "e2e-palette";
const INITIAL_DRAFT = "prefix suffix";
const EXPECTED_DRAFT = "prefix /e2e-palette suffix";
const CARET = "prefix /e2e-palette ".length;
const PROMPT = { selector: "section.composer textarea#prompt", identity: { tag: "TEXTAREA", id: "prompt" } };
const SEARCH = { selector: "#local-search", identity: { tag: "INPUT", id: "local-search" } };

async function observe(cdp) {
  return cdp.evaluate(`(async () => {
    const projection = await window.__TAURI_INTERNALS__.invoke('desktop_state');
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    const visible = el => { const rect = el?.getBoundingClientRect(); return Boolean(el?.isConnected && rect.width > 0 && rect.height > 0 && getComputedStyle(el).visibility !== 'hidden'); };
    const prompts = [...document.querySelectorAll('section.composer textarea#prompt')];
    const prompt = prompts.length === 1 ? prompts[0] : null;
    const dialog = document.querySelector('[aria-labelledby="command-palette-dialog-title"]');
    const search = dialog?.querySelector('#local-search');
    return { projection,
      prompt: { count: prompts.length, visible: visible(prompt), enabled: Boolean(prompt && !prompt.disabled && !prompt.closest('[inert]')),
        value: prompt?.value, start: prompt?.selectionStart, end: prompt?.selectionEnd, active: document.activeElement === prompt },
      palette: { visible: visible(dialog), search: search?.value ?? null, search_active: document.activeElement === search,
        feedback: dialog?.querySelector('.feedback')?.textContent?.trim() ?? null,
        commands: [...(dialog?.querySelectorAll('[data-action="insert-command"]') ?? [])].map(el => ({index: Number(el.dataset.index), label: el.querySelector('span')?.textContent?.trim(), active: el === document.activeElement, visible: visible(el)})) },
      error_count: [...document.querySelectorAll('.fatal, .ui-error-notice')].filter(visible).length };
  })()`);
}

export function paletteDraftUnchanged(surface, initial, { value, caret, active = true }) {
  const state = surface?.projection;
  return surface?.prompt?.count === 1 && surface.prompt.visible === true && surface.prompt.enabled === true
    && surface.prompt.active === active && surface.prompt.value === value
    && surface.prompt.start === caret && surface.prompt.end === caret
    && state?.overlay === "none" && state.busy === false && state.confirmation_visible === false
    && state.draft_prompt === initial.draft_prompt
    && isDeepStrictEqual(state.draft_target, initial.draft_target)
    && isDeepStrictEqual(state.run_target, initial.run_target)
    && state.composer_commit_generation === initial.composer_commit_generation
    && isDeepStrictEqual(state.transcript_rows, initial.transcript_rows)
    && isDeepStrictEqual(selectedNavigationIdentity(state), selectedNavigationIdentity(initial))
    && surface.error_count === 0;
}

export function paletteNoMatchReady(surface, query) {
  return surface?.projection?.overlay === "command_palette"
    && surface.projection.local_search_text === query
    && surface.palette?.visible === true && surface.palette.search_active === true
    && surface.palette.search === query
    && surface.palette.feedback === "一致するプロジェクト・チャット・履歴・成果物・コマンドはありません。"
    && Array.isArray(surface.palette.commands) && surface.palette.commands.length === 0
    && surface.error_count === 0;
}

export function paletteInsertionCommand(projection, rowPath) {
  const rows = projection.command_rows.map((row, index) => ({ row, index })).filter(({ row }) => row.path === rowPath);
  if (rows.length !== 1 || rows[0].row.name !== COMMAND_NAME) {
    throw new DesktopE2eError("product", "palette-command-row-mismatch", "isolated command must appear exactly once", { rowPath, rows });
  }
  return { command: "insert_command", args: { index: rows[0].index,
    expectedTarget: { workspacePath: projection.workspace_path,
      ownerProjectId: projection.project_rows[projection.selected_project_index]?.project_id ?? null,
      ownerSessionId: projection.session_rows[projection.selected_session_index]?.session_id ?? null, rowId: rowPath },
    expectedDraftTarget: projection.draft_target } };
}

async function wait(cdp, label, accept) {
  try { return (await waitForObservation({ label, timeoutMs: 10_000, pollMs: 50, sample: () => observe(cdp), accept, retrySampleErrors: false })).value; }
  catch (error) {
    if (error?.code !== "observation-timeout" || error?.evidence?.last_error) throw error;
    throw new DesktopE2eError("product", "palette-state-mismatch", label, error.evidence);
  }
}

async function text(input, target, value, sink) {
  const start = (await input.snapshotProbe()).sequence;
  await input.insertText(target, value);
  const probe = assertTrustedTextInsertion(await input.snapshotProbe(start), { afterSequence: start, identity: target.identity, text: value });
  await sink.record("palette-trusted-text", { probe }, { phase: "executing", owner: OWNER });
}

async function chord(input, key) {
  await input.keyDown("Control");
  await input.pressKey(key);
  await input.keyUp("Control");
}

export function createCommandPaletteInsertionScenario() {
  let rowPath = null;
  let cleanup = { input: "pass", resources: [] };
  return Object.freeze({
    id: "input.command-palette-insertion", productOracle: "pass", manualGate: "not_required", databaseRequired: true,
    async prepare(args) {
      await prepareShellBaseline(args);
      // A declared isolated repository root prevents command discovery in a parent checkout.
      await mkdir(path.join(args.context.paths.workspace, ".git"));
      const directory = path.join(args.context.paths.workspace, ".moyai", "commands");
      await mkdir(directory, { recursive: true });
      rowPath = path.join(directory, `${COMMAND_NAME}.md`);
      await writeFile(rowPath, "# E2E palette command\nFixture only. Do not execute a task.\n", { flag: "wx" });
      await args.sink.record("palette-command-fixture", { workspace: args.context.paths.workspace, path: rowPath,
        command: `/${COMMAND_NAME}`, isolated_repository_marker: ".git", runtime_mutation: false }, { phase: args.phase, owner: OWNER });
    },
    requestGracefulExit, quiesce: quiesceShellBaseline,
    async cleanup() { return cleanup; },
    async execute({ context, driver: cdp, sink }) {
      await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: OWNER, screenshotStem: "palette-shell-ready" });
      const initial = (await observe(cdp)).projection;
      // Paths in the product projection may use slash normalization; discover by exact isolated row name first.
      const ownRows = initial.command_rows.filter(row => row.name === COMMAND_NAME);
      if (ownRows.length !== 1 || path.resolve(ownRows[0].path) !== path.resolve(rowPath)) {
        throw new DesktopE2eError("product", "palette-fixture-not-discovered", "Desktop did not discover the isolated command", { rowPath, rows: initial.command_rows });
      }
      rowPath = ownRows[0].path;
      const input = new WebviewInput(cdp, { probeId: "command-palette-insertion" });
      const commands = new DesktopCommandProbe(cdp, { probeId: "command-palette-insertion", commands: ["insert_command", "submit_prompt", "submit_side_chat", "cancel_run", "cancel_side_chat"] });
      let primary = null;
      try {
        await input.installProbe(); await commands.install();
        const pointerStart = (await input.snapshotProbe()).sequence;
        await input.click(PROMPT);
        assertTrustedProbeSequence(await input.snapshotProbe(pointerStart), { afterSequence: pointerStart, expected: [
          { type: "pointerdown", identity: PROMPT.identity, button: 0, buttons: 1 },
          { type: "pointerup", identity: PROMPT.identity, button: 0, buttons: 0 },
          { type: "click", identity: PROMPT.identity, button: 0, buttons: 0 },
        ] });
        await text(input, PROMPT, "suffix", sink);
        await input.pressKey("Home");
        await text(input, PROMPT, "prefix ", sink);
        await wait(cdp, "initial local draft and insertion caret", value => paletteDraftUnchanged(value, initial, { value: INITIAL_DRAFT, caret: 7 }));
        await chord(input, "k");
        await wait(cdp, "palette search receives focus", value => value.projection.overlay === "command_palette" && value.palette.search_active);
        await text(input, SEARCH, COMMAND_NAME, sink);
        await wait(cdp, "real command search settles", value => value.projection.local_search_text === COMMAND_NAME && value.palette.feedback?.includes(`コマンド: /${COMMAND_NAME}`));
        const before = await input.snapshotProbe();
        await input.pressKey("Tab");
        const after = await input.snapshotProbe(before.sequence);
        const navigation = new TabFocusNavigator().observe({ beforeSequence: before.sequence, beforeActive: before.active, afterActive: after.active, events: after.events });
        if (navigation.classification !== "acquired") throw new DesktopE2eError("harness", "palette-tab-acquisition", "trusted Tab could not be acquired", { navigation, after });
        const selected = await wait(cdp, "Tab selects the exact command", value => value.palette.commands.length === 1 && value.palette.commands[0].active && value.palette.commands[0].label === `/${COMMAND_NAME}`);
        const expected = paletteInsertionCommand(selected.projection, rowPath);
        const enterStart = (await input.snapshotProbe()).sequence;
        await input.pressKey("Enter");
        const enter = assertTrustedProbeSequence(await input.snapshotProbe(enterStart), { afterSequence: enterStart, expected: [
          { type: "keydown", key: "Enter", code: "Enter", identity: { tag: "BUTTON", action: "insert-command" } },
          { type: "keyup", key: "Enter", code: "Enter", identity: { tag: "BUTTON", action: "insert-command" } },
        ] });
        const inserted = await wait(cdp, "command inserts once into the local draft without sending", value => paletteDraftUnchanged(value, initial, { value: EXPECTED_DRAFT, caret: CARET }));
        const firstCommands = assertExactDesktopCommandSequence(await commands.snapshot(), { expected: [expected] });
        await sink.record("palette-insertion-verified", { navigation, enter, commands: firstCommands, observation: inserted }, { phase: "executing", owner: OWNER });
        await captureScenarioScreenshot({ cdp, sink, name: "palette-command-inserted-once", owner: OWNER });
        await chord(input, "k");
        await wait(cdp, "reopened palette search receives focus", value => value.projection.overlay === "command_palette" && value.palette.search_active);
        await chord(input, "a");
        await text(input, SEARCH, "e2e-no-such-result-99283", sink);
        const noMatch = await wait(cdp, "unmatched query reports no results and displays no command candidates", value => paletteNoMatchReady(value, "e2e-no-such-result-99283"));
        await captureScenarioScreenshot({ cdp, sink, name: "palette-no-match-before-cancel", owner: OWNER });
        const escapeStart = (await input.snapshotProbe()).sequence;
        await input.pressKey("Escape");
        const escape = assertTrustedProbeSequence(await input.snapshotProbe(escapeStart), { afterSequence: escapeStart, expected: [
          { type: "keydown", key: "Escape", code: "Escape", identity: SEARCH.identity },
          { type: "keyup", key: "Escape", code: "Escape", identity: SEARCH.identity },
        ] });
        const cancelled = await wait(cdp, "unmatched query cancel preserves inserted draft", value => paletteDraftUnchanged(value, initial, { value: EXPECTED_DRAFT, caret: CARET }));
        const finalCommands = assertExactDesktopCommandSequence(await commands.snapshot(), { expected: [expected] });
        await sink.record("palette-cancel-without-submit", { commands: finalCommands, no_match: noMatch.palette, escape, observation: cancelled,
          limitation: "Browser-trusted input; no OS keyboard/IME claim." }, { phase: "executing", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) { primary = error; throw error; }
      finally {
        const resources = [];
        for (const [owner, close] of [["desktop-command-probe", () => commands.remove()], ["webview-input", () => input.cleanup()]]) {
          try { resources.push({ owner, pass: true, result: await close() }); }
          catch (error) { resources.push({ owner, pass: false, message: error?.message ?? String(error) }); }
        }
        cleanup = { input: resources.every(resource => resource.pass) ? "pass" : "fail", resources };
        if (cleanup.input === "fail" && primary === null) throw new DesktopE2eError("harness", "palette-cleanup-failed", "palette input resources did not settle", cleanup);
      }
    },
  });
}

import assert from "node:assert/strict";
import test from "node:test";
import { createCommandPaletteInsertionScenario, paletteDraftUnchanged, paletteInsertionCommand, paletteNoMatchReady } from "../scenarios/command_palette_insertion.mjs";
import { assertExactDesktopCommandSequence } from "../drivers/desktop_command_probe.mjs";

function initial() { return { workspace_path: "/fixture", selected_project_index: 0, selected_session_index: 0,
  project_rows: [{ project_id: "p", path: "/fixture" }], session_rows: [{ session_id: "s" }],
  command_rows: [{ name: "e2e-palette", path: "/fixture/.moyai/commands/e2e-palette.md" }],
  draft_prompt: "", draft_target: { workspacePath: "/fixture", sessionId: "s", ownerGeneration: "1" },
  run_target: { run_id: null }, composer_commit_generation: "3", transcript_rows: [],
  overlay: "none", busy: false, confirmation_visible: false }; }

test("command palette scenario reuses common lifecycle without launching its own host", () => {
  const scenario = createCommandPaletteInsertionScenario();
  assert.equal(scenario.id, "input.command-palette-insertion");
  for (const key of ["prepare", "execute", "quiesce", "requestGracefulExit", "cleanup"]) assert.equal(typeof scenario[key], "function");
  assert.equal("launch" in scenario, false);
});

test("no-match oracle rejects stale candidates even when the feedback says there are no matches", () => {
  const query = "e2e-no-such-result-99283";
  const good = { projection: { overlay: "command_palette", local_search_text: query },
    palette: { visible: true, search_active: true, search: query,
      feedback: "一致するプロジェクト・チャット・履歴・成果物・コマンドはありません。", commands: [] }, error_count: 0 };
  assert.equal(paletteNoMatchReady(good, query), true);
  for (const change of [
    next => { next.palette.commands.push({ index: 0, label: "/e2e-palette", visible: true }); },
    next => { next.palette.commands.push({ index: 0, label: "/e2e-palette", visible: false }); },
    next => { next.palette.search = "stale"; },
    next => { next.projection.local_search_text = "stale"; },
    next => { next.palette.feedback = "コマンド: /e2e-palette"; },
    next => { next.palette.visible = false; },
    next => { next.palette.search_active = false; },
    next => { next.projection.overlay = "none"; },
    next => { next.error_count = 1; },
  ]) { const invalid = structuredClone(good); change(invalid); assert.equal(paletteNoMatchReady(invalid, query), false); }
});

test("palette draft oracle rejects partial insertion, changed target, hidden editor and accidental submission", () => {
  const baseline = initial();
  const expected = { value: "prefix /e2e-palette suffix", caret: 20 };
  const good = { projection: structuredClone(baseline), prompt: { count: 1, visible: true, enabled: true, active: true, value: expected.value, start: 20, end: 20 }, error_count: 0 };
  assert.equal(paletteDraftUnchanged(good, baseline, expected), true);
  for (const change of [
    next => { next.prompt.value = "/e2e-palette "; },
    next => { next.prompt.value += "/e2e-palette "; },
    next => { next.prompt.start = 0; },
    next => { next.prompt.active = false; },
    next => { next.prompt.enabled = false; },
    next => { next.projection.busy = true; },
    next => { next.projection.transcript_rows.push({ role: "user", text: "sent" }); },
    next => { next.projection.draft_target.ownerGeneration = "2"; },
    next => { next.projection.session_rows[0].session_id = "other"; },
    next => { next.projection.composer_commit_generation = "4"; },
    next => { next.projection.draft_prompt = expected.value; },
  ]) { const invalid = structuredClone(good); change(invalid); assert.equal(paletteDraftUnchanged(invalid, baseline, expected), false); }
});

test("exact command oracle rejects duplicate insertion, auto-send and changed command owner", () => {
  const state = initial();
  const expected = paletteInsertionCommand(state, state.command_rows[0].path);
  const snapshot = { found: true, sequence: 1, dropped_through: 0, calls: [{ sequence: 1, ...expected }] };
  assert.equal(assertExactDesktopCommandSequence(snapshot, { expected: [expected] }).calls.length, 1);
  for (const extra of [expected, { command: "submit_prompt", args: {} }]) {
    assert.throws(() => assertExactDesktopCommandSequence({ ...snapshot, sequence: 2, calls: [...snapshot.calls, { sequence: 2, ...extra }] }, { expected: [expected] }));
  }
  const wrongOwner = structuredClone(snapshot); wrongOwner.calls[0].args.expectedDraftTarget.sessionId = "wrong";
  assert.throws(() => assertExactDesktopCommandSequence(wrongOwner, { expected: [expected] }));
  assert.throws(() => paletteInsertionCommand({ ...state, command_rows: [...state.command_rows, ...state.command_rows] }, state.command_rows[0].path));
});

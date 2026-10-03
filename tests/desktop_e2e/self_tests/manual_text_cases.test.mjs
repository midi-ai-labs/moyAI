import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import { ManualLiveSession, manualLiveManifestDiff, manualLiveSection, manualLiveTurnObservationReady } from "../drivers/manual_live_session.mjs";
import { createManualCase3Scenario, normalizeManualCase3Options, manualCase3Stages, manualCase3DocsOnlyFailures, manualCase3CliResult } from "../scenarios/manual_case3.mjs";
import { createManualCase4Scenario, manualCase4Input } from "../scenarios/manual_case4.mjs";
import { createManualCase6Scenario, manualCase6ScopeFailures } from "../scenarios/manual_case6.mjs";

const RAW = { provider_base_url: "http://provider.invalid/v1", model: "exact-model", python_executable: process.execPath, approval_mode: "operator" };
const spec = name => readFile(new URL(`../../manual_ST/${name}/spec.md`, import.meta.url), "utf8");
const file = (name, hash = "same") => ({ path: name, sha256: hash, size_bytes: 3 });

test("manual cases preserve canonical requests/tasks and use common lifecycle contracts", async () => {
  const stages = manualCase3Stages(await spec("case3"));
  assert.deepEqual(stages.map(row => row.name), ["stage1", "stage2", "stage3"]);
  assert.match(stages[0].prompt, /実装コードと test は変更せず/);
  assert.match(stages[1].prompt, /この turn では文書だけ/);
  assert.match(stages[2].prompt, /calculate\(left, operator, right\)/);
  const input = manualCase4Input(await spec("case4"));
  assert.equal(input.prompt, "current directory の `task.md` を読み、記載された Step を順番に完了してください。\n作業対象は current directory 以下のみです。");
  assert.match(input.task, /python -m unittest test_integration -v/);
  assert.equal(manualLiveSection(await spec("case6"), "Canonical user request"), "このサーバが重い。何が起きているか調べて。");
  for (const [scenario, id] of [[createManualCase3Scenario({ ...RAW, fixture_source: process.cwd() }), "manual.case3"],
    [createManualCase4Scenario(RAW), "manual.case4"], [createManualCase6Scenario(RAW), "manual.case6"]]) {
    assert.equal(scenario.id, id); assert.equal(scenario.manualGate, "pending"); assert.equal(scenario.databaseRequired, true);
    assert.equal(typeof scenario.requestGracefulExit, "function");
  }
});

test("case3 requires an explicit accepted fixture and does not invent an initial API predicate", () => {
  assert.equal(normalizeManualCase3Options({ ...RAW, fixture_source: process.cwd() }).fixtureSource, process.cwd());
  assert.throws(() => normalizeManualCase3Options(RAW));
  assert.throws(() => normalizeManualCase3Options({ ...RAW, fixture_source: "relative" }));
});

test("docs-only and diagnostic cases compare substantive workspace changes while preserving runtime artifacts", () => {
  const before = [file("calculator.py"), file("test_calculator.py")];
  const after = [...before, file("docs/calculator-design.md"), file("__pycache__/test.pyc"), file(".moyai/state")];
  assert.deepEqual(manualLiveManifestDiff(before, after), ["docs/calculator-design.md"]);
  assert.deepEqual(manualCase3DocsOnlyFailures(before, after, "stage1"), []);
  assert.deepEqual(manualCase3DocsOnlyFailures(before, [file("calculator.py", "changed"), file("test_calculator.py")], "stage2"), ["stage2:docs-only-scope:calculator.py"]);
  assert.deepEqual(manualCase6ScopeFailures([], [file("diagnostic.md")]), ["read-only-workspace-change:diagnostic.md"]);
});

test("case3 CLI checks exact exits, result formatting and usage stderr", () => {
  const result = code => ({ outcome: { root_exit_code: code } });
  assert.equal(manualCase3CliResult(result(0), "5\n", "", { exit: 0, suffix: "5" }), true);
  for (const value of ["5.0", "0.5", "-5"]) assert.equal(manualCase3CliResult(result(0), value, "", { exit: 0, suffix: "5" }), false);
  assert.equal(manualCase3CliResult(result(1), "", "usage: calculator", { exit: 1 }), true);
  assert.equal(manualCase3CliResult(result(2), "", "usage: calculator", { exit: 1 }), false);
  assert.equal(manualCase3CliResult(result(1), "usage: calculator", "", { exit: 1 }), false);
});

test("same-session continuation does not accept the preceding turn or an approval still settling", () => {
  const session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
  const oldTurn = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
  const newTurn = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
  const surface = { projection: { confirmation_visible: false, busy: false, post_run_refresh_pending: false,
    background_mutation_pending: false, async_polling_required: false, pending_async_operations: [], run_status_key: "completed",
    task_activity_state: "idle", composer_submit_mode: "new_request", can_submit: true, draft_target: { sessionId: session },
    run_target: { sessionId: session, expectedState: { kind: "idle", latestTurnId: oldTurn } } } };
  assert.equal(manualLiveTurnObservationReady(surface, { previousTurnId: oldTurn }), false);
  surface.projection.run_target.expectedState.latestTurnId = newTurn;
  assert.equal(manualLiveTurnObservationReady(surface, { previousTurnId: oldTurn }), true);
  Object.assign(surface.projection, { confirmation_visible: true, confirmation_id: "42", busy: true });
  assert.equal(manualLiveTurnObservationReady(surface, { previousConfirmationId: "42" }), false);
  assert.equal(manualLiveTurnObservationReady(surface, { previousConfirmationId: "41" }), true);
});

test("manual send focuses the exact prompt after native attachment return and before each continuation", async () => {
  const sessionId = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
  const turns = ["01ARZ3NDEKTSV4RRFFQ69G5FAW", "01ARZ3NDEKTSV4RRFFQ69G5FAX", "01ARZ3NDEKTSV4RRFFQ69G5FAY"];
  const operations = [], records = [], events = [];
  let active = { tag: "BUTTON", action: "toggle-attachment-tray" }, value = "", turn = 0;
  const projection = () => ({ confirmation_visible: false, busy: false, post_run_refresh_pending: false,
    background_mutation_pending: false, async_polling_required: false, pending_async_operations: [], run_status_key: "completed",
    task_activity_state: "idle", composer_submit_mode: "new_request", can_submit: true, draft_target: { sessionId },
    run_target: { sessionId, expectedState: { kind: "idle", latestTurnId: turns[turn], admissionRevision: String(turn) } } });
  const driver = { screenshot: async () => Buffer.from("test screenshot"), evaluate: async () => {
    const p = projection();
    return { projection: p, composer: { count: 1, visible: true, run_target: p.run_target },
      prompt: { count: 1, visible: true, enabled: true, value }, send: { count: 1, visible: true, enabled: true },
      visible_fatal_count: 0, visible_recoverable_error_count: 0 };
  } };
  const emit = event => events.push({ sequence: events.length + 1, isTrusted: true, ...event });
  const input = {
    snapshotProbe: async (after = 0) => ({ found: true, sequence: events.length, dropped_through: 0,
      events: structuredClone(events.filter(event => event.sequence > after)) }),
    click: async locator => {
      operations.push(["click", locator.selector]); active = locator.identity;
      emit({ type: "click", ...locator.identity });
      if (locator.identity.action === "send") turn += 1;
      return { identity: locator.identity };
    },
    insertText: async (locator, text) => {
      assert.deepEqual(active, locator.identity, "insertText must retain its exact active-owner requirement");
      operations.push(["insert", text]); value = text;
      emit({ type: "input", ...locator.identity, inputType: "insertText", data: text });
    },
  };
  const sink = { writeBytes: async name => ({ path: name }), record: async (name, data) => records.push({ name, data }) };
  const live = new ManualLiveSession({ context: {}, driver, sink, options: {}, owner: "scenario:manual.case2", stem: "case2" });
  live.input = input;
  assert.equal((await live.send("画像から実装してください")).terminal.projection.run_target.expectedState.latestTurnId, turns[1]);
  assert.deepEqual(active, { tag: "BUTTON", action: "send" });
  assert.equal((await live.send("文書を追加してください", { stage: "stage2" })).terminal.projection.run_target.expectedState.latestTurnId, turns[2]);
  const prompt = "section.composer textarea#prompt", send = 'section.composer button[data-action="send"]';
  assert.deepEqual(operations, [["click", prompt], ["insert", "画像から実装してください"], ["click", send],
    ["click", prompt], ["insert", "文書を追加してください"], ["click", send]]);
  const focuses = records.filter(row => row.data.action === "focus-canonical-prompt");
  assert.deepEqual(focuses.map(row => row.name), ["case2-trusted-action", "case2-stage2-trusted-action"]);
  for (const row of focuses) assert.equal(row.data.events.events[0].isTrusted, true);
  const sent = records.filter(row => row.name.endsWith("-prompt-sent"));
  assert.deepEqual(sent.map(row => row.data.typing.reconstructed_text), ["画像から実装してください", "文書を追加してください"]);
  sent.forEach((row, index) => assert.equal(row.data.typing.after_sequence, focuses[index].data.events.last_sequence));
});

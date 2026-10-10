import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import { ManualLiveSession, manualLiveManifestDiff, manualLiveSection, manualLiveTurnObservationReady, manualLiveStopObservationReady } from "../drivers/manual_live_session.mjs";
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
  surface.composer = { count: 1, visible: true, run_target: structuredClone(surface.projection.run_target) };
  surface.session_usage = { count: 0, visible: false, text: null, title: null, state: null };
  assert.equal(manualLiveTurnObservationReady(surface, { previousTurnId: oldTurn }), false);
  surface.projection.run_target.expectedState.latestTurnId = newTurn;
  surface.composer.run_target.expectedState.latestTurnId = newTurn;
  assert.equal(manualLiveTurnObservationReady(surface, { previousTurnId: oldTurn }), true);
  Object.assign(surface.projection, { confirmation_visible: true, confirmation_id: "42", busy: true });
  assert.equal(manualLiveTurnObservationReady(surface, { previousConfirmationId: "42" }), false);
  assert.equal(manualLiveTurnObservationReady(surface, { previousConfirmationId: "41" }), true);
});

test("manual Stop waits for the reviewed terminal owner and the rendered approval dialog to close", () => {
  const session = "01ARZ3NDEKTSV4RRFFQ69G5FAV", turn = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
  const request = { stop_target: { kind: "turn", workspacePath: "C:/fixture", sessionId: session,
    turnId: turn, admissionRevision: "1" } };
  const surface = { visible_dialog_count: 0, visible_modal_backdrop_count: 0, projection: {
    confirmation_visible: false, busy: false, post_run_refresh_pending: false, background_mutation_pending: false,
    async_polling_required: false, pending_async_operations: [], task_activity_state: "idle", run_status_key: "cancelled",
    run_target: { workspacePath: "C:/fixture", sessionId: session,
      expectedState: { kind: "idle", latestTurnId: turn, admissionRevision: "1" } },
    session_usage_label: "未計測", session_usage_title: "使用量は未計測", session_usage_state: "missing",
  } };
  surface.composer = { count: 1, visible: true, run_target: structuredClone(surface.projection.run_target) };
  surface.session_usage = { count: 1, visible: true, text: "未計測", title: "使用量は未計測", state: "missing" };
  assert.equal(manualLiveStopObservationReady(surface, request), true);
  for (const patch of [{ visible_dialog_count: 1 }, { visible_modal_backdrop_count: 1 },
    { visible_dialog_count: undefined }, { visible_modal_backdrop_count: undefined },
    { composer: { ...surface.composer, run_target: { sessionId: turn } } },
    { session_usage: { ...surface.session_usage, text: "古い表示" } },
    { session_usage: { ...surface.session_usage, title: "古い説明" } },
    { session_usage: { ...surface.session_usage, state: "complete" } }]) {
    assert.equal(manualLiveStopObservationReady({ ...surface, ...patch }, request), false);
  }
  for (const patch of [{ confirmation_visible: true }, { busy: true }, { post_run_refresh_pending: true },
    { background_mutation_pending: true }, { async_polling_required: true },
    { pending_async_operations: ["cancel-run"] }, { pending_async_operations: null }, { task_activity_state: "running" }]) {
    assert.equal(manualLiveStopObservationReady({ ...surface, projection: { ...surface.projection, ...patch } }, request), false);
  }
  for (const patch of [{ workspacePath: "C:/other" }, { sessionId: "01ARZ3NDEKTSV4RRFFQ69G5FAX" },
    { expectedState: { ...surface.projection.run_target.expectedState, latestTurnId: "01ARZ3NDEKTSV4RRFFQ69G5FAX" } },
    { expectedState: { ...surface.projection.run_target.expectedState, admissionRevision: "2" } },
    { expectedState: { kind: "turn", turnId: turn, admissionRevision: "1" } }]) {
    const changed = structuredClone(surface);
    Object.assign(changed.projection.run_target, patch);
    assert.equal(manualLiveStopObservationReady(changed, request), false);
  }
  assert.equal(manualLiveStopObservationReady(surface, { stop_target: { ...request.stop_target, admissionRevision: "01" } }), false);
  assert.equal(manualLiveStopObservationReady(surface, { stop_target: { ...request.stop_target, workspacePath: undefined } }), false);
  assert.equal(manualLiveStopObservationReady(null, request), false);
});

test("manual send focuses the exact prompt after native attachment return and before each continuation", async () => {
  const sessionId = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
  const turns = ["01ARZ3NDEKTSV4RRFFQ69G5FAW", "01ARZ3NDEKTSV4RRFFQ69G5FAX", "01ARZ3NDEKTSV4RRFFQ69G5FAY"];
  const operations = [], records = [], events = [];
  let active = { tag: "BUTTON", action: "toggle-attachment-tray" }, value = "", turn = 0, pendingRender = false;
  const projection = () => ({ confirmation_visible: false, busy: false, post_run_refresh_pending: false,
    background_mutation_pending: false, async_polling_required: false, pending_async_operations: [], run_status_key: "completed",
    task_activity_state: "idle", composer_submit_mode: "new_request", can_submit: true, draft_target: { sessionId },
    session_usage_label: "累計 34 tokens", session_usage_title: "使用量の累計", session_usage_state: "complete",
    run_target: { sessionId, expectedState: { kind: "idle", latestTurnId: turns[turn], admissionRevision: String(turn) } } });
  const driver = { screenshot: async () => Buffer.from("test screenshot"), evaluate: async () => {
    const p = projection();
    const sessionUsage = pendingRender
      ? { count: 1, visible: true, text: "未計測", title: "使用量は未計測", state: "missing" }
      : { count: 1, visible: true, text: p.session_usage_label, title: p.session_usage_title, state: p.session_usage_state };
    pendingRender = false;
    return { projection: p, composer: { count: 1, visible: true, run_target: p.run_target },
      session_usage: sessionUsage,
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
      if (locator.identity.action === "send") { turn += 1; pendingRender = true; }
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
  const rendered = records.filter(row => row.name.endsWith("-terminal-render"));
  assert.deepEqual(rendered.map(row => [row.name, row.data.status]), [
    ["case2-terminal-render", "discrepancy"], ["case2-terminal-render", "settled"],
    ["case2-stage2-terminal-render", "discrepancy"], ["case2-stage2-terminal-render", "settled"],
  ]);
  for (const row of rendered.filter(row => row.data.status === "discrepancy")) {
    assert.equal(row.data.projection.session_usage_label, "累計 34 tokens");
    assert.equal(row.data.session_usage.text, "未計測");
    assert.deepEqual(row.data.failures.map(item => item.field), ["session_usage.text", "session_usage.title", "session_usage.state"]);
  }
  for (const row of rendered.filter(row => row.data.status === "settled")) {
    assert.deepEqual(row.data.failures, []);
    assert.equal(row.data.session_usage.text, row.data.projection.session_usage_label);
    assert.notEqual(row.data.first_discrepancy_at, null);
  }
});

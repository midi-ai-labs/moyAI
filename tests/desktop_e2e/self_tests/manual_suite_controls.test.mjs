import assert from "node:assert/strict";
import test from "node:test";
import { createManualSuiteControlsScenario, MANUAL_SUITE_CONTROL_TURNS, manualSuiteControlsFixtureConfig,
  manualSuiteControlsProviderFailures, manualSuiteControlsTurnFailures, manualSuiteControlsSelectionFailures,
  manualSuiteControlsTranscriptFailures, refreshManualSuiteControls } from "../scenarios/manual_suite_controls.mjs";
import { manualSuiteFixtureFields } from "../scenarios/manual_suite.mjs";
import { assertTrustedProbeSequence } from "../drivers/webview_input.mjs";

const session = "01ARZ3NDEKTSV4RRFFQ69G5FAV", turn = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
function terminal(expected) {
  const target = { workspacePath: "C:/fixture/A", sessionId: session, expectedState: { kind: "idle", latestTurnId: turn } };
  const row = { session_id: session, title: expected.prompt, label: `${expected.prompt} [完了] ${session.slice(0, 8)}` };
  return { projection: { busy: false, confirmation_visible: false, post_run_refresh_pending: false, background_mutation_pending: false,
    async_polling_required: false, pending_async_operations: [], run_status_key: "completed", task_activity_state: "idle",
    overlay: "none", navigation_loading: false,
    composer_submit_mode: "new_request", can_submit: true, workspace_path: "C:/fixture/A", run_target: target,
    draft_target: { workspacePath: "C:/fixture/A", sessionId: session }, session_rows: [row], selected_session_index: 0,
    selected_session_title: row.label, current_session_label: row.title,
    transcript_rows: [{ row_kind: "user", body: expected.prompt }, { row_kind: "assistant", body: expected.responseText }] },
    selected_navigation: [{ action: "session", focus_key: `session:${session}:select`, visible: true }],
    composer: { count: 1, visible: true, run_target: target }, session_usage: { count: 0, visible: false },
    visible_fatal_count: 0, visible_recoverable_error_count: 0 };
}

test("qualification has one lifecycle and a Responses fixture without tools or hidden prerequisite folders", () => {
  const scenario = createManualSuiteControlsScenario();
  assert.equal(scenario.id, "manual.suite-controls");
  for (const method of ["prepare", "execute", "quiesce", "cleanup", "requestGracefulExit"]) assert.equal(typeof scenario[method], "function");
  const fields = manualSuiteFixtureFields(manualSuiteControlsFixtureConfig("http://127.0.0.1:12345"));
  assert.equal(fields.get("model.provider_profile"), "openai_responses");
  assert.equal(fields.get("model.context_window"), "131072");
  assert.equal(fields.get("model.request_timeout_ms"), "30000");
  assert.equal(fields.get("model.supports_tools"), "false");
  assert.equal(fields.get("docling.enabled"), "false");
});

test("terminal predicate rejects history carried from the first workspace and wrong owner or response", () => {
  const expected = MANUAL_SUITE_CONTROL_TURNS[1], value = terminal(expected);
  assert.deepEqual(manualSuiteControlsTurnFailures(value, expected, "C:/fixture/A"), []);
  const contaminated = structuredClone(value);
  contaminated.projection.transcript_rows.unshift({ row_kind: "user", body: MANUAL_SUITE_CONTROL_TURNS[0].prompt });
  assert.deepEqual(manualSuiteControlsTurnFailures(contaminated, expected, "C:/fixture/A"), ["user-history"]);
  assert.ok(manualSuiteControlsTurnFailures(value, expected, "C:/fixture/B").includes("workspace"));
  value.projection.transcript_rows[1].body = "wrong response";
  assert.ok(manualSuiteControlsTurnFailures(value, expected, "C:/fixture/A").includes("assistant-history"));
});

test("exact two response requests reject extra turn, readiness request and rejected wire", () => {
  const row = sequence => ({ sequence, route: "responses", method: "POST", pathname: "/v1/responses", response_status: 200, response_phase: "completed", contract: { pass: true } });
  assert.deepEqual(manualSuiteControlsProviderFailures([row(1), row(2)], 2), []);
  assert.ok(manualSuiteControlsProviderFailures([row(1), row(2), row(3)], 2).includes("request-count"));
  assert.ok(manualSuiteControlsProviderFailures([row(1), row(2), { sequence: 3, route: "docling_readiness", method: "GET", pathname: "/ready", response_status: 200, response_phase: "completed" }], 2).includes("request-3"));
  assert.ok(manualSuiteControlsProviderFailures([{ ...row(1), contract: { pass: false } }], 1).includes("request-1"));
});

test("completed B rejects the predecessor selected row even with correct B run and history", () => {
  const value = terminal(MANUAL_SUITE_CONTROL_TURNS[1]);
  value.projection.session_rows[0].session_id = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
  assert.ok(manualSuiteControlsTurnFailures(value, MANUAL_SUITE_CONTROL_TURNS[1], "C:/fixture/A").includes("selected-session"));
});

test("completed B rejects stale selected title independently of its correct owner", () => {
  const value = terminal(MANUAL_SUITE_CONTROL_TURNS[1]);
  value.projection.selected_session_title = "old alpha title";
  assert.ok(manualSuiteControlsTurnFailures(value, MANUAL_SUITE_CONTROL_TURNS[1], "C:/fixture/A").includes("session-title"));
});

test("completed B rejects a stale rendered sidebar selection independently of projection selection", () => {
  const value = terminal(MANUAL_SUITE_CONTROL_TURNS[1]);
  value.selected_navigation[0].focus_key = "session:01ARZ3NDEKTSV4RRFFQ69G5FAX:select";
  assert.ok(manualSuiteControlsTurnFailures(value, MANUAL_SUITE_CONTROL_TURNS[1], "C:/fixture/A").includes("selected-navigation"));
});

test("completed B rejects a draft workspace left behind by the predecessor", () => {
  const value = terminal(MANUAL_SUITE_CONTROL_TURNS[1]);
  value.projection.draft_target.workspacePath = "C:/fixture/B";
  assert.ok(manualSuiteControlsTurnFailures(value, MANUAL_SUITE_CONTROL_TURNS[1], "C:/fixture/A").includes("workspace"));
});

function fresh() {
  const value = terminal(MANUAL_SUITE_CONTROL_TURNS[1]);
  for (const target of [value.projection.run_target, value.projection.draft_target, value.composer.run_target]) target.sessionId = null;
  value.projection.selected_session_index = -1;
  value.projection.selected_session_title = "unselected fallback";
  value.projection.current_session_label = "fresh draft fallback";
  value.projection.transcript_rows = [{ row_kind: "empty_placeholder" }];
  value.projection.thread_empty = true;
  value.selected_navigation = [];
  return value;
}

test("fresh refresh retains null owners and no selection without relying on translated fallback titles", () => {
  const value = fresh();
  assert.deepEqual(manualSuiteControlsSelectionFailures(value, "C:/fixture/A", { fresh: true }), []);
  value.projection.selected_session_index = 0;
  value.selected_navigation = [{ action: "session", focus_key: `session:${session}:select`, visible: true }];
  const failures = manualSuiteControlsSelectionFailures(value, "C:/fixture/A", { fresh: true });
  assert.ok(failures.includes("selected-session")); assert.ok(failures.includes("selected-navigation"));
  value.projection.run_target.sessionId = session;
  assert.ok(manualSuiteControlsSelectionFailures(value, "C:/fixture/A", { fresh: true }).includes("owner"));
});

test("explicit saved A stays selected and retains its exact completed turn owner", () => {
  const expected = MANUAL_SUITE_CONTROL_TURNS[0], value = terminal(expected);
  assert.deepEqual(manualSuiteControlsTurnFailures(value, expected, "C:/fixture/A", { sessionId: session, turnId: turn }), []);
  assert.ok(manualSuiteControlsTurnFailures(value, expected, "C:/fixture/A",
    { sessionId: "01ARZ3NDEKTSV4RRFFQ69G5FAX", turnId: turn }).includes("owner"));
  assert.ok(manualSuiteControlsTurnFailures(value, expected, "C:/fixture/A",
    { sessionId: session, turnId: "01ARZ3NDEKTSV4RRFFQ69G5FAX" }).includes("owner"));
});

function refreshExercise({ final = fresh(), extraCommand = false, commandArgs = {}, installError = false,
  cleanupError = null, neverSettle = false, keepsMenu = true, escapeTrusted = true, closeDispatch = true, closeSucceeds = true } = {}) {
  let current = fresh(), sequence = 0, pendingSamples = 0, keySequence = 0;
  const calls = [], actions = [], resources = [], records = [], keyEvents = [], state = { cleanupFailures: [] };
  const dependencies = {
    observe: async () => {
      if (current.projection.async_polling_required) {
        pendingSamples++;
        const pending = structuredClone(current);
        if (!neverSettle) { current = structuredClone(final); if (keepsMenu) current.projection.overlay = "view_menu"; }
        return pending;
      }
      return structuredClone(current);
    },
    createInput: () => ({ async installProbe() { resources.push("input-install"); },
      async snapshotProbe(after = 0) { return { found: true, sequence: keySequence, dropped_through: 0,
        events: keyEvents.filter(row => row.sequence > after) }; },
      async waitForTrustedProbeSequence(options) { return assertTrustedProbeSequence({ found: true,
        sequence: keySequence, dropped_through: 0, events: keyEvents.filter(row => row.sequence > options.afterSequence) }, options); },
      async pressKey(key) {
        assert.equal(key, "Escape"); actions.push("Escape");
        for (const type of ["keydown", "keyup"]) keyEvents.push({ sequence: ++keySequence, type, key, code: key, isTrusted: escapeTrusted });
        if (closeDispatch) calls.push({ sequence: ++sequence, command: "close_overlay", args: {} });
        if (closeSucceeds) current.projection.overlay = "none";
      },
      async cleanup() { resources.push("input-cleanup"); if (cleanupError === "input") throw new Error("input cleanup failed"); } }),
    createCommands: (_driver, options) => {
      assert.deepEqual(options.commands, ["refresh_desktop", "close_overlay"]);
      return { async install() { resources.push("commands-install"); if (installError) throw new Error("probe already owned"); },
        async snapshot() { return { found: true, sequence, dropped_through: 0, calls }; },
        async remove() { resources.push("commands-remove"); if (cleanupError === "commands") throw new Error("command cleanup failed"); } };
    },
    async click(_input, locator) {
      actions.push(locator.identity.action);
      if (locator.identity.action === "show-view-menu") current.projection.overlay = "view_menu";
      else if (locator.identity.action === "refresh") {
        calls.push({ sequence: ++sequence, command: "refresh_desktop", args: commandArgs });
        if (extraCommand) calls.push({ sequence: ++sequence, command: "refresh_desktop", args: {} });
        current.projection.overlay = "none";
        current.projection.async_polling_required = true;
        current.projection.pending_async_operations = ["snapshot_refresh"];
      } else throw new Error("unexpected GUI action");
      return { test: "trusted native input is covered by the common driver and real GUI qualification" };
    },
    async wait(options) {
      assert.equal(options.retrySampleErrors, false);
      for (let attempt = 0; attempt < 3; attempt++) {
        const value = await options.sample();
        if (options.accept(value)) return { value };
      }
      const error = new Error("snapshot did not settle"); error.code = "observation-timeout"; throw error;
    },
  };
  const run = () => refreshManualSuiteControls({ context: { paths: { workspace: "C:/fixture/A" } }, driver: {},
    sink: { async record(name, value) { records.push({ name, value: structuredClone(value) }); } },
    stem: "selection-gui-fix-test", expected: { fresh: true }, state }, dependencies);
  return { run, actions, calls, resources, records, state, pendingSamples: () => pendingSamples };
}

test("public refresh uses the native menu once and waits for snapshot async work before accepting fresh B", async () => {
  const value = refreshExercise(); await value.run();
  assert.deepEqual(value.actions, ["show-view-menu", "refresh", "Escape"]);
  assert.deepEqual(value.calls, [{ sequence: 1, command: "refresh_desktop", args: {} },
    { sequence: 2, command: "close_overlay", args: {} }]);
  assert.equal(value.pendingSamples(), 1);
  assert.deepEqual(value.resources, ["input-install", "commands-install", "commands-remove", "input-cleanup"]);
  assert.ok(value.records.some(row => row.name === "selection-gui-fix-test-after" && row.value.failures.length === 0));
});

test("view-menu refresh retaining its popup must explicitly close it before accepting fresh B", async () => {
  const value = refreshExercise({ keepsMenu: true }); await value.run();
  assert.deepEqual(value.actions, ["show-view-menu", "refresh", "Escape"]);
  assert.deepEqual(value.calls, [{ sequence: 1, command: "refresh_desktop", args: {} },
    { sequence: 2, command: "close_overlay", args: {} }]);
  assert.ok(value.records.some(row => row.name === "selection-gui-fix-test-close-menu"));
  assert.deepEqual(value.resources.slice(-2), ["commands-remove", "input-cleanup"]);
});

test("Escape closure cannot accept untrusted keys, missing dispatch or a still-open menu", async () => {
  const untrusted = refreshExercise({ escapeTrusted: false });
  await assert.rejects(untrusted.run(), error => error.name === "WebviewInputError" && /trusted/i.test(error.message));
  const missing = refreshExercise({ closeDispatch: false });
  await assert.rejects(missing.run(), error => error.code === "desktop-command-probe-cardinality");
  const open = refreshExercise({ closeSucceeds: false });
  await assert.rejects(open.run(), error => error.code === "observation-timeout");
  for (const value of [untrusted, missing, open]) assert.deepEqual(value.resources.slice(-2), ["commands-remove", "input-cleanup"]);
});

test("refresh rejects stale selection, duplicate dispatch and unexpected command arguments", async () => {
  const selected = fresh(); selected.projection.selected_session_index = 0;
  for (const [options, code] of [
    [{ final: selected }, "manual-suite-controls-refresh-selection"],
    [{ extraCommand: true }, "desktop-command-probe-cardinality"],
    [{ commandArgs: { sessionId: session } }, "desktop-command-probe-call-mismatch"],
    [{ neverSettle: true }, "observation-timeout"],
  ]) {
    const value = refreshExercise(options);
    await assert.rejects(value.run(), error => error.code === code);
    assert.deepEqual(value.resources.slice(-2), ["commands-remove", "input-cleanup"]);
  }
});

test("refresh observer ownership and cleanup failure cannot be hidden by a successful selection", async () => {
  const notOwned = refreshExercise({ installError: true });
  await assert.rejects(notOwned.run(), /probe already owned/);
  assert.deepEqual(notOwned.resources, ["input-install", "commands-install", "input-cleanup"]);
  assert.deepEqual(notOwned.actions, []);
  for (const cleanupError of ["input", "commands"]) {
    const value = refreshExercise({ cleanupError });
    await assert.rejects(value.run(), error => error.code === "manual-suite-controls-cleanup" && error.owner === "harness");
    assert.equal(value.state.cleanupFailures.length, 1);
  }
});

const transcript = (response = MANUAL_SUITE_CONTROL_TURNS[1].responseText) =>
  `# Conversation\n\n> ${MANUAL_SUITE_CONTROL_TURNS[1].prompt}\n\n${response}\n\n<details><summary>実行情報</summary>\n\n- Session: \`${session}\`\n</details>\n`;
test("B export requires its session, User prompt and final assistant content without A contamination", () => {
  const expected = MANUAL_SUITE_CONTROL_TURNS[1];
  assert.deepEqual(manualSuiteControlsTranscriptFailures(transcript(), expected, session), []);
  assert.ok(manualSuiteControlsTranscriptFailures(transcript(), expected, "01ARZ3NDEKTSV4RRFFQ69G5FAX").includes("session-prompt"));
  assert.ok(manualSuiteControlsTranscriptFailures(transcript().replace(expected.prompt, "wrong prompt"), expected, session).includes("session-prompt"));
  assert.ok(manualSuiteControlsTranscriptFailures(transcript(`<details><summary>tool</summary>\n${expected.responseText}\n</details>\n\nwrong final`), expected, session).includes("assistant-response"));
  assert.ok(manualSuiteControlsTranscriptFailures(transcript(`${MANUAL_SUITE_CONTROL_TURNS[0].responseText}\n\n${expected.responseText}`), expected, session).includes("predecessor-history"));
});

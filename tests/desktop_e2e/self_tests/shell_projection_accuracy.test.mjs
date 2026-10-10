import assert from "node:assert/strict";
import crypto from "node:crypto";
import test from "node:test";
import { executeDesktopScenario } from "../core/desktop_execution.mjs";
import { createChatToolContinuationProviderScript } from "../drivers/scripted_provider.mjs";
import { normalizeSemanticLocator } from "../drivers/webview_input.mjs";
import { chatToolContinuationClosedStoreReady, providerChatToolContinuationFixtureConfig } from "../scenarios/provider_chat_tool_continuation.mjs";
import { createShellProjectionAccuracyScenario, shellProjectionAccuracyCall, shellProjectionAccuracyFixture,
  shellProjectionAccuracyFailures, shellProjectionCanonicalEvidence, shellProjectionColdEvidenceFailures,
  shellProjectionExportFailures, shellProjectionHasSgr, shellProjectionPermissionDecision, shellProjectionPermissionFailures,
  shellProjectionSummaryTarget, shellProjectionVisibleFailures, SHELL_PROJECTION_FIXTURE_NAME } from "../scenarios/shell_projection_accuracy.mjs";

const SESSION = "01ARZ3NDEKTSV4RRFFQ69G5FAV", TURN = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const USER = "01ARZ3NDEKTSV4RRFFQ69G5FAX", CALL = "01ARZ3NDEKTSV4RRFFQ69G5FAY", ASSISTANT = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";
const HASH = "a".repeat(64), context = { paths: { workspace: "C:/fixture/workspace" } };
const sha256 = bytes => crypto.createHash("sha256").update(bytes).digest("hex");
const modes = ["success", "failure"];

function ledger(held) {
  return ["chat_tool_initial", "chat_continuation"].map((role, index) => ({ route: "chat_completions", method: "POST",
    pathname: "/v1/chat/completions", query_present: false, response_phase: index === 1 && held ? "held" : "completed",
    response_status: index === 1 && held ? null : 200, contract: {
      pass: true, role, model_matches: true, top_level_keys_match: true, stream_true: true, include_usage_true: true,
      n_one: true, client_generation_fields_absent: true, client_generation_fields_present: [], max_tokens_absent: true,
      parallel_tool_calls_false: true, tools: { pass: true, unique_tool_names: true, current_time_present: true, current_time_schema_matches: true },
      role_evidence: { message_count: index === 0 ? 2 : 4, message_roles: index === 0 ? ["system", "user"] : ["system", "user", "assistant", "tool"],
        system_content_non_empty: true, system_content_sha256: HASH, user_prompt_matches: true, user_content_sha256: HASH,
        assistant_content_absent: index === 1, current_time_call_matches: index === 1, tool_output_shape_matches: index === 1,
        tool_output_size_bytes: index === 1 ? 700 : null, tool_output_sha256: index === 1 ? HASH : null },
    } }));
}

function sample(mode, phase = "held") {
  const held = phase === "held", success = mode === "success", call = shellProjectionAccuracyCall(context, mode);
  const runPhase = held ? "fixture-live-phase" : phase === "restart" ? "fixture-load-phase" : "fixture-terminal-phase";
  const runStep = held ? "fixture live step" : phase === "restart" ? "fixture load step" : "fixture terminal step";
  const marker = success ? "SGR_STDOUT_OK" : "SGR_STDERR_FAIL", stream = success ? "Stdout" : "Stderr";
  const result = `Command: ${call.arguments.command}\n\nExit code: ${success ? 0 : 1}\n\n${stream}:\n${marker}`;
  const summary = { row_kind: held ? "work_summary_running" : "work_summary_completed", stable_history_identity: `turn:${TURN}:work-summary`,
    body: `### ${held ? "ツール" : "作業履歴"}\n- [${success ? "完了" : "失敗"}] Shell\n  出力: ${result.replaceAll("\n", " ")}` };
  const error = { row_kind: "error", body: result };
  const p = { run_status_key: held ? "running" : "completed", task_activity_state: held ? "running" : "idle", busy: held,
    agent_tree_active: false, startup: { status: "ready" }, post_run_refresh_pending: false, background_mutation_pending: false,
    async_polling_required: false, pending_async_operations: [], navigation_loading: false, provider_loading: false, overlay: "none",
    confirmation_visible: false, confirmation_id: null, confirmation: null, draft_prompt: "", composer_submit_mode: "new_request", can_submit: true,
    draft_target: { sessionId: SESSION }, run_target: { sessionId: SESSION,
      expectedState: { kind: held ? "turn" : "idle", ...(held ? { turnId: TURN } : { latestTurnId: TURN }), admissionRevision: "1" } },
    run_phase: runPhase, run_active_step: runStep,
    progress_text: `${held ? "Running" : "Completed"}\nフェーズ: ${runPhase}\n手順: ${runStep}\nモデル要求: ${held ? 1 : 2}\nツール: 1件開始 / ${success ? 1 : 0}件完了 / 0件拒否 / 0件キャンセル / ${success ? 0 : 1}件失敗\n圧縮: 0`,
    tool_status_text: `ツール: 1件中1件を表示（要確認を優先・新しい順）\n- [${success ? "完了" : "失敗"}] Shell: ${result.replaceAll("\n", " ")}`,
    transcript_rows: [{ row_kind: "user", stable_history_identity: USER, body: call.prompt },
      ...(!held && !success ? [error] : []), summary,
      ...(held ? [] : [{ row_kind: "assistant", stable_history_identity: ASSISTANT, body: call.responseText }])],
  };
  return { ledger: ledger(held), surface: { projection: p, thread_text: p.transcript_rows.map(row => row.body).join("\n"),
    errors: !held && !success ? [{ history_identity: null, text: result, visible: true }] : [],
    assistants: held ? [] : [{ history_identity: ASSISTANT, text: call.responseText, visible: true }],
    visible_fatal_count: 0, visible_recoverable_error_count: 0, visible_validation_error_count: 0 } };
}

function pending(mode) {
  const call = shellProjectionAccuracyCall(context, mode), p = sample(mode).surface.projection;
  Object.assign(p, { confirmation_visible: true, confirmation_id: "41", confirmation: {
    outside_workspace: false, risks: ["unclassified dynamic/indirect shell construct"],
    details: [`Command: ${call.arguments.command}`] } });
  return { ledger: ledger(true).slice(0, 1), surface: { projection: p, errors: 0,
    dialog: { count: 1, id: "41", visible: true, busy: false, command: call.arguments.command,
      buttons: ["approve-permission", "abort-permission"].map(action => ({ action, visible: true, enabled: true })) } } };
}

function canonical(mode) {
  const success = mode === "success", call = shellProjectionAccuracyCall(context, mode);
  const color = success ? 32 : 31, marker = success ? "SGR_STDOUT_OK" : "SGR_STDERR_FAIL", stream = success ? "Stdout" : "Stderr";
  return [{ id: USER, session_id: SESSION, turn_id: TURN, sequence_no: 1, payload_json: JSON.stringify({
    kind: "tool_call", call_id: CALL, tool_name: "shell", arguments_json: JSON.stringify(call.arguments) }) },
  { id: ASSISTANT, session_id: SESSION, turn_id: TURN, sequence_no: 2, payload_json: JSON.stringify({
    kind: "tool_output", call_id: CALL, status: "completed", success,
    output_text: `Command: ${call.arguments.command}\n\nExit code: ${success ? 0 : 1}\n\n${stream}:\n\u001b[${color}m${marker}\u001b[0m`,
    metadata: { success, tool_metadata: { success, exit_code: success ? 0 : 1, effect_started: true, sandbox: "unrestricted" } } }) }];
}

function display(mode) {
  const p = sample(mode, "terminal").surface.projection;
  return { summary: { count: 1, history_identity: `turn:${TURN}:work-summary`, disclosure_count: 1,
    focus_key: "observed-disclosure:opaque-key", open: true, visible: true,
    text: p.transcript_rows.find(row => row.row_kind === "work_summary_completed").body },
  export: { visible: true, enabled: true, status_visible: true, status: "会話をMarkdownで保存しました: C:/fixture/export.md" } };
}

function markdown(mode) {
  const call = shellProjectionAccuracyCall(context, mode), d = display(mode);
  return `# Transcript\n\n> ${call.prompt}\n\n${d.summary.text}\n\n${call.responseText}\n\n<details><summary>実行情報</summary>\n\n- Session: \`${SESSION}\`\n\n</details>\n`;
}

test("the two scripted fixtures retain exact ranges and quoted Move data without elevation or dependencies", () => {
  assert.equal(SHELL_PROJECTION_FIXTURE_NAME, "shell_sgr_fixture.py");
  for (const mode of modes) {
    const call = shellProjectionAccuracyCall(context, mode), success = mode === "success";
    assert.equal(call.arguments.command, `1..${success ? 5 : 10}|Out-Null; python -c "'g2.move_player'"; python -B -m shell_sgr_fixture`);
    assert.equal(call.arguments.workdir, context.paths.workspace);
    assert.equal(call.outputMaxBytes, 2048);
    assert.equal(shellProjectionHasSgr(call.arguments.command), false);
    assert.deepEqual(createChatToolContinuationProviderScript({ call }).call, call);
    const fixture = shellProjectionAccuracyFixture(mode);
    assert.match(fixture, /^import sys\n/u);
    assert.match(fixture, /chr\(27\) \+ chr\(91\)/u);
    assert.ok(fixture.includes(`sys.${success ? "stdout" : "stderr"}.write(`));
    assert.ok(fixture.includes(`sys.exit(${success ? 0 : 1})`));
    assert.equal(fixture.includes("open("), false);
    const raw = JSON.parse(canonical(mode)[1].payload_json).output_text;
    // Both markers remain within the existing short Desktop result preview.
    assert.ok(raw.indexOf(success ? "SGR_STDOUT_OK" : "SGR_STDERR_FAIL") < 200);
    const scenario = createShellProjectionAccuracyScenario(mode);
    assert.equal(scenario.id, `permission.shell-projection-${mode}`);
    assert.equal(scenario.productOracle, "pass");
    assert.equal(scenario.manualGate, "not_required");
    assert.equal(scenario.databaseRequired, true);
    for (const method of ["prepare", "execute", "quiesce", "cleanup", "requestGracefulExit"]) assert.equal(typeof scenario[method], "function");
  }
  assert.match(providerChatToolContinuationFixtureConfig("http://127.0.0.1:1234"), /access_mode = "default"/u);
  assert.throws(() => createShellProjectionAccuracyScenario("unknown"));
});

test("public permission intent drops false outside and Move risks while retaining the dynamic review boundary", () => {
  for (const mode of modes) {
    const call = shellProjectionAccuracyCall(context, mode);
    assert.deepEqual(shellProjectionPermissionFailures(pending(mode), call), []);
    for (const mutate of [
      value => { value.surface.projection.confirmation.outside_workspace = true; },
      value => { value.surface.projection.confirmation.risks.push("move"); },
      value => { value.surface.projection.confirmation.risks = []; },
      value => { value.surface.projection.confirmation.details = ["Command: some other operation"]; },
      value => { value.surface.dialog.id = "42"; },
      value => { value.surface.dialog.buttons[0].enabled = false; },
      value => { value.ledger.push(ledger(true)[1]); },
    ]) { const value = pending(mode); mutate(value); assert.ok(shellProjectionPermissionFailures(value, call).length); }
  }
});

test("valid Rust permission waits for same-ID DOM settlement without hiding bad metadata or approving a wrong dialog", () => {
  for (const mode of modes) {
    const call = shellProjectionAccuracyCall(context, mode);
    // The first actual GUI failure observed this gap: authoritative confirmation
    // already exists, while the ordinary renderer still has no dialog node.
    const lagging = pending(mode);
    lagging.surface.dialog = { count: 0, id: null, visible: false, busy: false, command: null, text: null, buttons: [] };
    assert.ok(shellProjectionPermissionFailures(lagging, call).length);
    assert.equal(shellProjectionPermissionDecision(lagging, call), "pending");
    const hidden = pending(mode);
    hidden.surface.dialog.visible = false;
    hidden.surface.dialog.buttons.forEach(button => { button.visible = false; });
    assert.equal(shellProjectionPermissionDecision(hidden, call), "pending");
    const busy = pending(mode); busy.surface.dialog.busy = true;
    assert.equal(shellProjectionPermissionDecision(busy, call), "pending");
    const settled = pending(mode), unchanged = structuredClone(settled);
    assert.equal(shellProjectionPermissionDecision(settled, call), "pass");
    assert.deepEqual(settled, unchanged);
    for (const mutate of [
      value => { value.surface.projection.confirmation.outside_workspace = true; },
      value => { value.surface.projection.confirmation.risks.push("move"); },
      value => { value.surface.projection.confirmation.details = ["Command: other operation"]; },
      value => { value.surface.projection.run_target.expectedState.admissionRevision = "invalid"; },
      value => { value.ledger.push(ledger(true)[1]); },
      value => { value.surface.errors = 1; },
    ]) { const value = structuredClone(lagging); mutate(value); assert.equal(shellProjectionPermissionDecision(value, call), "fail"); }
    for (const mutate of [
      value => { value.surface.dialog.id = "42"; value.surface.dialog.visible = false; },
      value => { value.surface.dialog.count = 2; },
      value => { value.surface.dialog.command = "other operation"; },
      value => { value.surface.dialog.buttons[0].enabled = false; },
    ]) { const value = pending(mode); mutate(value); assert.equal(shellProjectionPermissionDecision(value, call), "fail"); }
  }
});

test("held, terminal and cold projections require the correct result, count and owner without SGR remnants", () => {
  for (const mode of modes) for (const phase of ["held", "terminal", "restart"]) {
    const heldProjection = sample(mode).surface.projection;
    const options = { mode, phase, ...(phase !== "held" ? { heldProjection } : {}) };
    assert.deepEqual(shellProjectionAccuracyFailures(sample(mode, phase), options), []);
    for (const mutate of [
      value => { value.surface.projection.progress_text = "ツール: 2件開始"; },
      value => { value.surface.projection.tool_status_text += "\n- [完了] second execution"; },
      value => { value.surface.projection.transcript_rows.find(row => row.row_kind.startsWith("work_summary")).body += " \u001b[32mleaked\u001b[0m"; },
      value => { value.surface.projection.tool_status_text += " [31;1m orphan"; },
      value => { value.surface.thread_text += " \u009b0m leaked"; },
      value => { value.ledger[1].contract.role_evidence.tool_output_size_bytes = 2049; },
      value => { value.ledger.push(structuredClone(value.ledger[1])); },
    ]) { const value = sample(mode, phase); mutate(value); assert.ok(shellProjectionAccuracyFailures(value, options).length); }
    if (phase !== "held") {
      const value = sample(mode, phase); value.surface.projection.run_target.expectedState.admissionRevision = "2";
      assert.ok(shellProjectionAccuracyFailures(value, options).includes("held-terminal-owner-changed"));
    }
  }
});

test("cold progress permits its own typed context while every historical progress line and row stays exact", () => {
  for (const mode of modes) {
    const terminal = sample(mode, "terminal"), cold = sample(mode, "restart"), previousProjection = terminal.surface.projection;
    const options = { mode, phase: "restart", previousProjection }, unchanged = structuredClone(cold);
    assert.notEqual(cold.surface.projection.progress_text, previousProjection.progress_text);
    assert.notEqual(cold.surface.projection.run_phase, previousProjection.run_phase);
    assert.notEqual(cold.surface.projection.run_active_step, previousProjection.run_active_step);
    assert.deepEqual(shellProjectionAccuracyFailures(cold, options), []);
    assert.deepEqual(cold, unchanged);
    for (const mutate of [
      value => { value.surface.projection.run_phase = "another typed phase"; },
      value => { value.surface.projection.run_active_step = "another typed step"; },
      value => { delete value.surface.projection.run_phase; },
      value => { value.surface.projection.progress_text = value.surface.projection.progress_text.replace("Completed", "OtherStatus"); },
      value => { value.surface.projection.progress_text = value.surface.projection.progress_text.replace("モデル要求: 2", "モデル要求: 3"); },
      value => { value.surface.projection.progress_text = value.surface.projection.progress_text.replace("1件開始", "2件開始"); },
      value => { value.surface.projection.progress_text = value.surface.projection.progress_text.replace("圧縮: 0", "圧縮: 1"); },
      value => { value.surface.projection.transcript_rows.find(row => row.row_kind === "assistant").body += " changed"; },
    ]) { const value = sample(mode, "restart"); mutate(value); assert.ok(shellProjectionAccuracyFailures(value, options).length); }
    const badPrevious = structuredClone(previousProjection); badPrevious.run_active_step = "inconsistent previous context";
    assert.ok(shellProjectionAccuracyFailures(cold, { ...options, previousProjection: badPrevious }).includes("progress-context-mismatch"));
    const additionalPrevious = structuredClone(previousProjection), additionalCold = structuredClone(cold);
    additionalPrevious.progress_text += "\n追加の実績: 1"; additionalCold.surface.projection.progress_text += "\n追加の実績: 1";
    assert.deepEqual(shellProjectionAccuracyFailures(additionalCold, { ...options, previousProjection: additionalPrevious }), []);
    additionalCold.surface.projection.progress_text = additionalCold.surface.projection.progress_text.replace("追加の実績: 1", "追加の実績: 2");
    assert.ok(shellProjectionAccuracyFailures(additionalCold, { ...options, previousProjection: additionalPrevious }).includes("restart-display-history-changed"));
  }
});

test("terminal nonzero error matches its optional source identity and full visible text; held has no independent error", () => {
  for (const phase of ["terminal", "restart"]) {
    assert.deepEqual(shellProjectionAccuracyFailures(sample("failure", phase), { mode: "failure", phase }), []);
    for (const mutate of [
      value => { value.surface.errors[0].history_identity = CALL; },
      value => { value.surface.errors[0].visible = false; },
      value => { value.surface.errors[0].text += " additional text"; },
      value => { value.surface.projection.transcript_rows.find(row => row.row_kind === "error").body = "Exit code: 0\nOK"; },
    ]) { const value = sample("failure", phase); mutate(value); assert.ok(shellProjectionAccuracyFailures(value, { mode: "failure", phase }).length); }
  }
  const held = sample("failure"); held.surface.errors.push({ text: "premature independent error" });
  assert.ok(shellProjectionAccuracyFailures(held, { mode: "failure" }).includes("unexpected-independent-error-row"));
});

test("the canonical completed output retains original SGR bytes and truthful operation success across both fixtures", () => {
  for (const mode of modes) {
    const call = shellProjectionAccuracyCall(context, mode), raw = canonical(mode), evidence = shellProjectionCanonicalEvidence(raw, call, mode);
    assert.deepEqual(evidence.failures, []);
    assert.deepEqual(evidence.rows, raw);
    const text = JSON.parse(raw[1].payload_json).output_text;
    assert.equal(evidence.output_sha256, sha256(text));
    assert.equal(evidence.output_size_bytes, Buffer.byteLength(text));
    assert.equal(shellProjectionHasSgr(text), true);
    assert.ok(shellProjectionCanonicalEvidence(raw.slice(0, 1), call, mode).failures.includes("canonical-call-output-cardinality"));
    assert.ok(shellProjectionCanonicalEvidence([...raw, raw[1]], call, mode).failures.includes("canonical-call-output-cardinality"));
    for (const mutate of [
      p => { p.output_text = p.output_text.replace(/\u001b\[[0-9;]*m/gu, ""); },
      p => { p.success = !p.success; },
      p => { p.status = "failed"; },
      p => { p.metadata.tool_metadata.effect_started = false; },
      p => { p.metadata.tool_metadata.exit_code = 7; },
      p => { p.call_id = USER; },
    ]) { const value = canonical(mode), output = JSON.parse(value[1].payload_json); mutate(output); value[1].payload_json = JSON.stringify(output);
      assert.ok(shellProjectionCanonicalEvidence(value, call, mode).failures.length); }
    const malformed = canonical(mode); malformed[1].payload_json = "null";
    assert.ok(shellProjectionCanonicalEvidence(malformed, call, mode).failures.includes("canonical-payload-malformed"));
  }
});

test("summary clicks bind a unique same-turn disclosure to its observed public focus key", () => {
  const observed = display("success");
  observed.summary.open = false;
  const unchanged = structuredClone(observed);
  const target = shellProjectionSummaryTarget(observed, TURN);
  assert.deepEqual(target.identity, { tag: "SUMMARY", focusKey: observed.summary.focus_key });
  assert.equal(target.selector, `#thread article.work-summary[data-history-identity="turn:${TURN}:work-summary"] > .message-body > details > summary[data-focus-key="observed-disclosure:opaque-key"]`);
  assert.deepEqual(normalizeSemanticLocator(target).identity, target.identity);
  assert.throws(() => normalizeSemanticLocator({ selector: target.selector, identity: { tag: "SUMMARY" } }), /semantic locator requires/u);
  for (const mutate of [
    value => { value.summary.count = 0; },
    value => { value.summary.count = 2; },
    value => { value.summary.history_identity = `turn:${USER}:work-summary`; },
    value => { value.summary.disclosure_count = 0; },
    value => { value.summary.disclosure_count = 2; },
    value => { delete value.summary.focus_key; },
    value => { value.summary.focus_key = null; },
    value => { value.summary.focus_key = ""; },
    value => { value.summary.focus_key = " "; },
  ]) {
    const value = display("success"); mutate(value);
    assert.throws(() => shellProjectionSummaryTarget(value, TURN), error => error.owner === "harness" && error.code === "shell-projection-summary-target");
  }
  assert.deepEqual(observed, unchanged);
});

test("visible disclosure and export status require actual visible body, not hidden textContent", () => {
  for (const mode of modes) {
    const options = { mode, turnId: TURN, exportPath: "C:/fixture/export.md" };
    assert.deepEqual(shellProjectionVisibleFailures(display(mode), options), []);
    for (const mutate of [
      value => { value.summary.open = false; },
      value => { value.summary.visible = false; },
      value => { value.summary.history_identity = `turn:${USER}:work-summary`; },
      value => { value.summary.text = "Exit code: 0 Stdout: some other result"; },
      value => { value.summary.text += " \u001b[31mcolor\u001b[0m"; },
      value => { value.export.status = "会話をMarkdownで保存しました: C:/other/export.md"; },
      value => { value.export.status_visible = false; },
    ]) { const value = display(mode); mutate(value); assert.ok(shellProjectionVisibleFailures(value, options).length); }
  }
});

test("fresh and cold visible exports preserve session, prompt, clean result and exact bytes", () => {
  for (const mode of modes) {
    const call = shellProjectionAccuracyCall(context, mode), text = markdown(mode), options = { sessionId: SESSION, call, mode };
    assert.deepEqual(shellProjectionExportFailures(text, options), []);
    assert.ok(shellProjectionExportFailures(text.replace(SESSION, TURN), options).length);
    assert.ok(shellProjectionExportFailures(text + "\u001b[0m", options).includes("export-result-mismatch"));
    const before = { fixture: { path: "C:/fixture/workspace/shell_sgr_fixture.py", sha256: sha256(shellProjectionAccuracyFixture(mode)), size_bytes: Buffer.byteLength(shellProjectionAccuracyFixture(mode)) },
      export: { source_path: "C:/fixture/export.md", sha256: sha256(text), size_bytes: Buffer.byteLength(text) } };
    assert.deepEqual(shellProjectionColdEvidenceFailures(before, structuredClone(before)), []);
    for (const mutate of [
      value => { value.export.sha256 = "b".repeat(64); },
      value => { value.export.source_path = "C:/other/export.md"; },
      value => { value.fixture.sha256 = "b".repeat(64); },
    ]) { const after = structuredClone(before); mutate(after); assert.deepEqual(shellProjectionColdEvidenceFailures(before, after), ["cold-evidence-changed"]); }
    const terminal = sample(mode, "terminal"), cold = sample(mode, "restart");
    assert.deepEqual(shellProjectionAccuracyFailures(cold, { mode, phase: "restart", previousProjection: terminal.surface.projection }), []);
    cold.surface.projection.transcript_rows.find(row => row.row_kind === "assistant").body += "changed";
    assert.ok(shellProjectionAccuracyFailures(cold, { mode, phase: "restart", previousProjection: terminal.surface.projection }).includes("restart-display-history-changed"));
  }
});

test("closed canonical collection requires the host's exact zero, quiesce and SQLite audit facts", async () => {
  const safe = { desktop_exited: true, profile_webviews_remaining: 0, admission_released: true,
    forced_desktop: false, forced_profile_process_ids: [], scenario_quiesce: { input: "pass" }, sqlite: { pass: true, path: "C:/fixture/data/moyai.sqlite3" } };
  assert.equal(chatToolContinuationClosedStoreReady(safe), true);
  for (const mutate of [
    value => { value.desktop_exited = false; },
    value => { value.profile_webviews_remaining = 1; },
    value => { value.profile_webviews_remaining = null; },
    value => { value.scenario_quiesce.input = "fail"; },
    value => { value.sqlite.pass = false; },
  ]) { const value = structuredClone(safe); mutate(value); assert.equal(chatToolContinuationClosedStoreReady(value), false); }
  for (const profileCount of [0, 1]) {
    const facts = { ...structuredClone(safe), profile_webviews_remaining: profileCount }, trace = [];
    const scenario = { id: "permission.shell-projection-success", productOracle: "pass", manualGate: "not_required",
      prepare: async () => {}, execute: async () => ({ acquisition: "pass", oracle: "pass", manual: "not_required" }),
      requestGracefulExit: async () => ({ requested: true }),
      quiesce: async () => { trace.push("quiesce"); return { input: "pass", resources: [] }; },
      cleanup: async ({ cleanup, phase }) => {
        trace.push("scenario-cleanup"); assert.equal(phase, "cleaning");
        assert.equal(cleanup.profile_webviews_remaining, profileCount);
        if (chatToolContinuationClosedStoreReady(cleanup)) trace.push("read-closed-canonical");
        cleanup.sqlite.path = "local-copy-only";
        return { input: "pass", resources: [] };
      } };
    const host = { preflight: async () => {}, launch: async () => ({}), attach: async () => ({}),
      cleanup: async ({ releaseScenarioResources }) => {
        trace.push("host-settled"); await releaseScenarioResources(); trace.push("host-audit-finished");
        return { input: profileCount === 0 ? "pass" : "fail", gracefulExit: { requested: true }, cleanup: facts };
      } };
    const observed = await executeDesktopScenario({ context: { root: "C:/fixture", executionId: "e2e-shell-projection-selftest" },
      scenario, host, sink: { record: async () => {}, seal: async () => ({}) } });
    assert.deepEqual(trace, ["host-settled", "quiesce", "host-audit-finished", "scenario-cleanup", ...(profileCount === 0 ? ["read-closed-canonical"] : [])]);
    assert.equal(observed.result.classification, profileCount === 0 ? "pass" : "harness_ng");
    assert.equal(observed.result.cleanup.sqlite.path, safe.sqlite.path);
    assert.equal(facts.sqlite.path, safe.sqlite.path);
  }
});

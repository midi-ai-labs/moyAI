import assert from "node:assert/strict";
import test from "node:test";
import { createChatToolContinuationProviderScript } from "../drivers/scripted_provider.mjs";
import { exactChatToolContinuationLedger, providerChatToolContinuationFixtureConfig } from "../scenarios/provider_chat_tool_continuation.mjs";
import { createToolOperationFailureScenario, toolOperationFailureCall, toolOperationFailureFailures,
  toolOperationFailureVisibleFailures, TOOL_OPERATION_FAILURE_PROMPT, TOOL_OPERATION_FAILURE_RESPONSE,
  TOOL_OPERATION_FAILURE_PROGRESS, TOOL_OPERATION_FAILURE_FIXTURE } from "../scenarios/tool_operation_failure.mjs";

const SESSION = "01ARZ3NDEKTSV4RRFFQ69G5FAV", TURN = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const USER = "01ARZ3NDEKTSV4RRFFQ69G5FAX", ERROR = "01ARZ3NDEKTSV4RRFFQ69G5FAY", ASSISTANT = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";
const HASH = "a".repeat(64);
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
        tool_output_size_bytes: index === 1 ? 1400 : null, tool_output_sha256: index === 1 ? HASH : null },
    } }));
}
function sample(held = true, errorSource = null) {
  const summary = { row_kind: held ? "work_summary_running" : "work_summary_completed", stable_history_identity: `turn:${TURN}:work-summary`,
    body: held ? "### 現在\n- フェーズ: AIの応答待ち\n- モデル要求: 2\n\n### ツール\n0件のコマンドを実行, 1件失敗\n- [失敗] Run shell command: python -m unittest\n  出力: Command: python -m unittest Exit code: 1 Stdout: (empty) Stderr: F ..."
      : "### 作業履歴\n- [待機] shell\n- [失敗] Run unittest\n  出力: exit 1" };
  const p = { run_status_key: held ? "running" : "completed", task_activity_state: held ? "running" : "idle", busy: held,
    agent_tree_active: false, startup: { status: "ready" }, post_run_refresh_pending: false, background_mutation_pending: false,
    async_polling_required: false, pending_async_operations: [], navigation_loading: false, provider_loading: false, overlay: "none",
    confirmation_visible: false, confirmation_id: null, confirmation: null, draft_prompt: "", composer_submit_mode: "new_request", can_submit: true,
    draft_target: { sessionId: SESSION }, run_target: { sessionId: SESSION,
      expectedState: { kind: held ? "turn" : "idle", ...(held ? { turnId: TURN } : { latestTurnId: TURN }), admissionRevision: "1" } },
    progress_text: TOOL_OPERATION_FAILURE_PROGRESS, tool_status_text: "ツール: 1件中1件を表示（要確認を優先・新しい順）\n- [失敗] Run unittest: exit1",
    transcript_rows: [{ row_kind: "user", stable_history_identity: USER, body: TOOL_OPERATION_FAILURE_PROMPT },
      ...(held ? [] : [{ row_kind: "error", ...(errorSource === null ? {} : { stable_history_identity: errorSource }),
        body: "Command: python -m unittest\n\nExit code: 1\n\nStderr:\nF\r\n======\r\nFAIL: expected failure\r\n------\r\nRan 1 test in 0.001s\r\n------\r\nFAILED (failures=1)" }]), summary,
      ...(held ? [] : [{ row_kind: "assistant", stable_history_identity: ASSISTANT, body: TOOL_OPERATION_FAILURE_RESPONSE }])],
  };
  return { ledger: ledger(held), surface: { projection: p, errors: held ? [] : [{ history_identity: errorSource,
    text: "Command: python -m unittest\n\nExit code: 1\n\nStderr: F ====== FAIL: expected failure\n\nRan 1 test in 0.001s\n\nFAILED (failures=1)", visible: true }],
    running_summaries: held ? [{ history_identity: summary.stable_history_identity, text: "作業中 · 0件のコマンドを実行, 1件失敗 · Command: python -m unittest Exit code: 1 Stderr: F ...", visible: true }] : [],
    assistants: held ? [] : [{ history_identity: ASSISTANT, text: TOOL_OPERATION_FAILURE_RESPONSE, visible: true }],
    visible_fatal_count: 0, visible_recoverable_error_count: 0, visible_validation_error_count: 0 } };
}

test("one unittest failure uses the shared continuation lifecycle without elevated permissions or Python-c", () => {
  const call = toolOperationFailureCall({ paths: { workspace: "C:/fixture/workspace" } });
  assert.deepEqual(call.arguments, { command: "python -m unittest", workdir: "C:/fixture/workspace" });
  assert.match(call.outputMarker, /lifecycle_status: completed\nkind: process_exit_nonzero/u);
  assert.match(call.outputMarker, /exit_code: 1$/u);
  assert.equal(call.outputMaxBytes, 2048);
  assert.deepEqual(createChatToolContinuationProviderScript({ call }).call, call);
  assert.equal((TOOL_OPERATION_FAILURE_FIXTURE.match(/def test_/gu) ?? []).length, 1);
  assert.match(TOOL_OPERATION_FAILURE_FIXTURE, /self\.fail\("EXPECTED_UNITTEST_FAILURE"\)/u);
  assert.match(providerChatToolContinuationFixtureConfig("http://127.0.0.1:1234"), /access_mode = "default"/u);
  const scenario = createToolOperationFailureScenario();
  assert.equal(scenario.id, "history.tool-operation-failure");
  assert.equal(scenario.databaseRequired, true);
  assert.equal(scenario.manualGate, "not_required");
  for (const name of ["prepare", "execute", "quiesce", "cleanup", "requestGracefulExit"]) assert.equal(typeof scenario[name], "function");
});

test("custom shell output bound is explicit, bounded and leaves the current_time default unchanged", () => {
  const call = toolOperationFailureCall({ paths: { workspace: "C:/fixture/workspace" } });
  for (const outputMaxBytes of [0, -1, 2049, 1.5, "2048", null]) {
    assert.throws(() => createChatToolContinuationProviderScript({ call: { ...call, outputMaxBytes } }));
  }
  assert.deepEqual(createChatToolContinuationProviderScript(), { kind: createChatToolContinuationProviderScript().kind });
  assert.equal(exactChatToolContinuationLedger(ledger(true), ["completed", "held"]), false);
  assert.equal(exactChatToolContinuationLedger(ledger(true), ["completed", "held"], 2048), true);
  const oversized = ledger(true); oversized[1].contract.role_evidence.tool_output_size_bytes = 2049;
  assert.equal(exactChatToolContinuationLedger(oversized, ["completed", "held"], 2048), false);
});

test("held and terminal operation failures require the failure evidence of their current display phase", () => {
  for (const held of [true, false]) {
    const phase = held ? "held" : "terminal";
    assert.deepEqual(toolOperationFailureFailures(sample(held), { phase }), []);
    for (const mutate of [
      value => { value.surface.projection.progress_text = TOOL_OPERATION_FAILURE_PROGRESS.replace("0件完了", "1件完了").replace("1件失敗", "0件失敗"); },
      value => { value.surface.projection.tool_status_text = value.surface.projection.tool_status_text.replace("[失敗]", "[完了]"); },
      value => { value.surface.projection.transcript_rows[1].body = "Exit code: 0\nRan 1 test in 0.001s\nOK"; },
      value => { value.surface.projection.transcript_rows.push(structuredClone(value.surface.projection.transcript_rows[1])); },
      value => { value.ledger.push(structuredClone(value.ledger[1])); },
      value => { value.surface.projection.confirmation_visible = true; },
    ]) { const value = sample(held); mutate(value); assert.ok(toolOperationFailureFailures(value, { phase }).length > 0); }
  }
});

test("held failure is embedded in the current owner's running summary while terminal retains the full result", () => {
  const held = sample();
  assert.deepEqual(held.surface.errors, []);
  assert.deepEqual(toolOperationFailureFailures(held), []);
  for (const mutate of [
    value => { value.surface.projection.transcript_rows.push({ row_kind: "error", stable_history_identity: ERROR, body: "unexpected independent error" }); },
    value => { value.surface.projection.transcript_rows[1].body = value.surface.projection.transcript_rows[1].body.replace("Exit code: 1", "Exit code: 0"); },
    value => { value.surface.running_summaries[0].history_identity = `turn:${SESSION}:work-summary`; },
    value => { value.surface.running_summaries[0].text = "Command: python -m unittest Exit code: 0"; },
    value => { value.surface.running_summaries[0].visible = false; },
  ]) { const value = sample(); mutate(value); assert.ok(toolOperationFailureFailures(value).length > 0); }
  for (const phase of ["terminal", "restart"]) {
    const value = sample(false);
    assert.deepEqual(toolOperationFailureFailures(value, { phase }), []);
    value.surface.projection.transcript_rows[1].body = "Command: python -m unittest\nExit code: 1\nFAILED (failures=1)";
    assert.ok(toolOperationFailureFailures(value, { phase }).includes("unittest-failure-result-not-exact"));
  }
});

test("terminal and restart match the visible full failure with optional source provenance", () => {
  for (const phase of ["terminal", "restart"]) {
    for (const source of [null, ERROR]) {
      const value = sample(false, source);
      assert.deepEqual(toolOperationFailureFailures(value, { phase }), []);
      if (source === null) {
        value.surface.projection.transcript_rows[1].stable_history_identity = null;
        assert.deepEqual(toolOperationFailureFailures(value, { phase }), []);
      }
    }
    for (const mutate of [
      value => { value.surface.errors[0].history_identity = ERROR; },
      value => { value.surface.errors[0].visible = false; },
      value => { value.surface.errors[0].text += " extra output"; },
      value => { value.surface.projection.transcript_rows[1].body += " extra output"; },
      value => { value.surface.errors[0].text = value.surface.errors[0].text.replace("======", ""); },
      value => { value.surface.projection.transcript_rows[1].stable_history_identity = "unknown"; value.surface.errors[0].history_identity = "unknown"; },
    ]) { const value = sample(false); mutate(value); assert.ok(toolOperationFailureFailures(value, { phase }).length > 0); }
  }
});

test("terminal remains bound to the one held session, turn and admission revision", () => {
  const heldProjection = sample().surface.projection;
  assert.deepEqual(toolOperationFailureFailures(sample(false), { phase: "terminal", heldProjection }), []);
  assert.ok(toolOperationFailureFailures(sample(false), { phase: "terminal", heldProjection: null }).includes("held-terminal-owner-changed"));
  for (const mutate of [
    value => { value.surface.projection.run_target.sessionId = USER; value.surface.projection.draft_target.sessionId = USER; },
    value => { value.surface.projection.run_target.expectedState.latestTurnId = USER;
      value.surface.projection.transcript_rows.find(row => row.row_kind === "work_summary_completed").stable_history_identity = `turn:${USER}:work-summary`; },
    value => { value.surface.projection.run_target.expectedState.admissionRevision = "2"; },
  ]) { const value = sample(false); mutate(value);
    assert.ok(toolOperationFailureFailures(value, { phase: "terminal", heldProjection }).includes("held-terminal-owner-changed")); }
  const duplicated = sample(false);
  duplicated.ledger.push(structuredClone(duplicated.ledger[1]));
  assert.ok(toolOperationFailureFailures(duplicated, { phase: "terminal", heldProjection }).includes("provider-ledger-not-exact"));
});

test("active visible counts match Rust text while terminal and restart retain canonical failure identity", () => {
  const active = sample();
  const activity = { progress: { count: 1, visible: true, text: active.surface.projection.progress_text },
    tools: { count: 1, visible: true, text: active.surface.projection.tool_status_text } };
  assert.deepEqual(toolOperationFailureVisibleFailures(active.surface, activity), []);
  activity.tools.text = activity.tools.text.replace("[失敗]", "[完了]");
  assert.deepEqual(toolOperationFailureVisibleFailures(active.surface, activity), ["visible-tool-list-not-exact"]);
  const terminal = sample(false), restored = sample(false);
  assert.deepEqual(toolOperationFailureFailures(restored, { phase: "restart", previousProjection: terminal.surface.projection }), []);
  restored.surface.projection.transcript_rows[1].stable_history_identity = ASSISTANT;
  assert.ok(toolOperationFailureFailures(restored, { phase: "restart", previousProjection: terminal.surface.projection }).includes("restart-canonical-history-changed"));
});

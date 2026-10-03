import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";

import {
  createManualCase1Scenario, manualCase1PublicHistory, manualCase1FixtureConfig,
  manualCase1Prompt, manualCase1TerminalDecision, manualCase1UnittestResult,
  normalizeManualCase1Options,
} from "../scenarios/manual_case1.mjs";
import * as case1 from "../scenarios/manual_case1.mjs";

const SPEC = fileURLToPath(new URL("../../manual_ST/case1/spec.md", import.meta.url));
const RAW = { provider_base_url: "http://provider.invalid:1234/v1", model: "exact-model", python_executable: process.execPath };
const ID = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const TURN = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const TEST_OUTPUT = "......\n----------------------------------------------------------------------\nRan 6 tests in 0.001s\n\nOK\n";

function terminal(overrides = {}) {
  return { visible_fatal_count: 0, visible_recoverable_error_count: 0, projection: {
    confirmation_visible: false, busy: false, post_run_refresh_pending: false,
    background_mutation_pending: false, async_polling_required: false, pending_async_operations: [],
    run_status_key: "completed", task_activity_state: "idle", composer_submit_mode: "new_request", can_submit: true,
    run_target: { sessionId: ID, expectedState: { kind: "idle", latestTurnId: TURN } }, draft_target: { sessionId: ID },
    ...overrides,
  } };
}

test("Case1 reads the sole canonical prompt from the current spec without extra instructions", async () => {
  const spec = await readFile(SPEC, "utf8");
  const prompt = manualCase1Prompt(spec);
  assert.equal(prompt, "current directory に Python の CLI 電卓を作成してください。\n四則演算 (+, -, *, /) に対応し、`calculator.py` と `test_calculator.py` を current directory 直下へ作成してください。\n作業は current directory 以下のみで行い、最後に `python -m unittest` を実行して成功を確認してから終了してください。\nPython のテキスト入出力は UTF-8 前提で扱ってください。");
  assert.equal(manualCase1Prompt(spec.replaceAll("\r\n", "\n").replaceAll("\n", "\r\n")), prompt);
  assert.throws(() => manualCase1Prompt("## Canonical user request\nno fenced prompt"));
});

test("Case1 options require explicit endpoint/model/Python identity and carry only the secret environment name", () => {
  const options = normalizeManualCase1Options({ ...RAW, api_key_env: "CASE1_PROVIDER_KEY" });
  assert.equal(options.apiKeyEnv, "CASE1_PROVIDER_KEY");
  assert.equal(options.providerBaseUrl, RAW.provider_base_url);
  assert.equal(options.model, RAW.model);
  assert.equal(normalizeManualCase1Options(RAW).apiKeyEnv, "");
  assert.equal(normalizeManualCase1Options(RAW).approvalMode, "stop");
  assert.equal(normalizeManualCase1Options({ ...RAW, approval_mode: "operator" }).approvalMode, "operator");
  for (const change of [{ model: "" }, { provider_base_url: "http://user:secret@provider.invalid/v1" },
    { provider_base_url: "http://provider.invalid/v1?key=secret" }, { api_key_env: "secret value" },
    { python_executable: "python" }, { temperature: 0.1 }, { approval_mode: "always-approve" }]) assert.throws(() => normalizeManualCase1Options({ ...RAW, ...change }));
});

test("Case1 uses the current profile and local context budget with Default permissions and no generation overrides", () => {
  const config = manualCase1FixtureConfig(normalizeManualCase1Options(RAW));
  assert.match(config, /provider_profile = "openai_compatible"/);
  assert.match(config, /context_window = 131072/);
  assert.match(config, /access_mode = "default"/);
  assert.doesNotMatch(config, /temperature|top_p|top_k|max_tokens|thinking|reasoning|extra_body|request_timeout/);
});

test("Case1 terminal predicate waits for exact idle owner and does not classify approval as product failure", () => {
  assert.equal(manualCase1TerminalDecision(terminal()), "completed");
  assert.equal(manualCase1TerminalDecision(terminal({ confirmation_visible: true, busy: true })), "approval");
  for (const change of [{ busy: true }, { post_run_refresh_pending: true }, { pending_async_operations: ["refresh"] },
    { draft_target: { sessionId: TURN } }, { task_activity_state: "running" }]) assert.equal(manualCase1TerminalDecision(terminal(change)), "pending");
  assert.equal(manualCase1TerminalDecision(terminal({ run_status_key: "failed" })), "failed");
});

test("Case1 external unittest must execute at least one test and complete with exit zero", () => {
  assert.deepEqual(manualCase1UnittestResult({ outcome: { root_exit_code: 0 } }, "", TEST_OUTPUT), { exit_code: 0, test_count: 6, pass: true });
  assert.equal(manualCase1UnittestResult({ outcome: { root_exit_code: 0 } }, TEST_OUTPUT.replace("Ran 6", "Ran 0"), "").pass, false);
  assert.equal(manualCase1UnittestResult({ outcome: { root_exit_code: 1 } }, TEST_OUTPUT, "").pass, false);
  assert.equal(manualCase1UnittestResult({ outcome: { root_exit_code: 0 } }, "OK", "").pass, false);
});

test("Case1 public canonical projection preserves review evidence without inventing missing metrics or tool pairs", () => {
  const projection = { transcript_rows: [
    { row_kind: "user", body: "canonical request" },
    { row_kind: "work_summary_completed", body: "tool summary" },
    { row_kind: "assistant", body: "検証を終えました。" },
  ], file_change_rows: [{ path: "calculator.py" }], session_usage_label: "6 requests" };
  const history = manualCase1PublicHistory(projection);
  assert.deepEqual(history.users, ["canonical request"]);
  assert.deepEqual(history.assistants, ["検証を終えました。"]);
  assert.equal(history.work_summaries[0].body, "tool summary");
  assert.equal(history.metrics.session_usage_label, "6 requests");
  assert.equal(history.metrics.numeric_runtime_metrics, "unverified_not_available_in_public_projection");
  assert.equal(history.exact_tool_call_result_pairs, "unverified_not_available_in_public_projection");
  assert.equal(history.agent_unittest, "manual_public_evidence_review_pending");
  assert.deepEqual(manualCase1PublicHistory({}).users, []);
});

function transcript(prompt, sessionId = ID) {
  return `# CLI calculator\n\n> ${prompt.replaceAll("\n", "\n> ")}\n\n<details><summary>1 previous messages</summary>\n\n> Earlier assistant output.\n\n</details>\n\nConnection failed.\n\n<details><summary>実行情報</summary>\n\n- Workspace: \`C:/workspace\`\n- Session: \`${sessionId}\`\n- Provider: \`http://provider.invalid:1234/v1\`\n- Model: \`exact-model\`\n</details>\n`;
}

test("Case1 transcript restores the exported user quotation and matches the exact Session metadata", () => {
  const prompt = "Create the calculator.\n\n> Keep this literal quote.\n最後にテストしてください。";
  const expected = { pass: true, session_id: ID, user_prompt: prompt };
  assert.deepEqual(case1.manualCase1TranscriptResult(transcript(prompt), { sessionId: ID, prompt }), expected);
  assert.deepEqual(case1.manualCase1TranscriptResult(transcript(prompt).replaceAll("\n", "\r\n"), { sessionId: ID, prompt }), expected);
  for (const text of [
    transcript(prompt, TURN).replace("Connection failed.", `Connection failed for ${ID}.`),
    transcript(prompt, `prefix-${ID}-suffix`),
    transcript(prompt).replace(`- Session: \`${ID}\``, `- Session: \`${ID}\`\n- Session: \`${TURN}\``),
    transcript("Different request.").replace("Connection failed.", `> ${prompt.replaceAll("\n", "\n> ")}`),
    transcript(prompt).replace(`> ${prompt.replaceAll("\n", "\n> ")}`, prompt),
  ]) assert.equal(case1.manualCase1TranscriptResult(text, { sessionId: ID, prompt }).pass, false);
});

test("Case1 failed terminal preserves its first diagnostic and leaves downstream file/unittest checks unreached", () => {
  const value = case1.manualCase1MachinePredicates({
    terminal: terminal({ run_status_key: "failed", status_message: "provider connection failed", status_detail: "DNS returned no data" }),
    approval: false, prompt: "canonical request", history: { users: ["canonical request"] }, generated: [],
  });
  assert.deepEqual(value.failures, ["desktop-terminal-not-completed"]);
  assert.equal(value.run_external_unittest, false);
  assert.deepEqual(value.external_unittest, { status: "not_reached", reason: "desktop-terminal-not-completed" });
  assert.equal(value.diagnostics.status_message, "provider connection failed");
  assert.equal(value.diagnostics.status_detail, "DNS returned no data");
  const completed = case1.manualCase1MachinePredicates({ terminal: terminal(), approval: false,
    prompt: "canonical request", history: { users: ["canonical request"] }, generated: [] });
  assert.equal(completed.run_external_unittest, true);
  assert.deepEqual(completed.failures, ["missing-calculator.py", "missing-test_calculator.py"]);
  const approval = case1.manualCase1MachinePredicates({ terminal: terminal({ run_status_key: "cancelled" }), approval: true,
    prompt: "canonical request", history: { users: ["canonical request"] }, generated: [] });
  assert.deepEqual(approval.failures, []);
  assert.deepEqual(approval.external_unittest, { status: "not_reached", reason: "unreviewed-approval-operator-stopped" });
});

test("Case1 operator review retains the exact public owner and uses the existing local permission buttons", () => {
  const surface = terminal({ confirmation_visible: true, confirmation_id: "42", confirmation: {
    summary: "Run tests", details: ["python -m unittest"], remote: null,
  }, stop_target: { kind: "active_run", runtimeOwnerToken: "owner-a" } });
  const request = case1.manualCase1PermissionRequest(surface.projection);
  assert.equal(request.confirmation_id, "42");
  assert.equal(request.run_target.sessionId, ID);
  surface.projection.confirmation.details[0] = "changed";
  assert.deepEqual(request.request.details, ["python -m unittest"]);
  for (const [decision, action, focus] of [["approve", "approve-permission", "approve"], ["deny", "abort-permission", "abort"], ["stop", "cancel-run", "stop"]]) {
    const locator = case1.manualCase1PermissionLocator(request, decision);
    assert.equal(locator.identity.action, action);
    assert.equal(locator.identity.focusKey, `permission:42:${focus}`);
    assert.match(locator.selector, /data-permission-id="42"/);
  }
  assert.throws(() => case1.manualCase1PermissionRequest({ ...surface.projection, confirmation_id: "other" }));
  assert.throws(() => case1.manualCase1PermissionRequest({ ...surface.projection, run_target: { sessionId: "other" } }));
  assert.throws(() => case1.manualCase1PermissionLocator(request, "always-approve"));
});

test("Case1 prepare leaves workspace empty and uses fresh common fixture owners", async () => {
  const parent = fileURLToPath(new URL("../../../../project_sandbox/manual-case1-adapter-self-tests/", import.meta.url));
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(path.join(parent, "fixture-"));
  const paths = Object.fromEntries(["workspace", "config", "data", "prefs", "webview", "logs"].map(name => [name, path.join(root, name)]));
  await Promise.all(Object.values(paths).map(directory => mkdir(directory)));
  paths.config_file = path.join(paths.config, "config.toml");
  paths.prefs_file = path.join(paths.prefs, "desktop.toml");
  const records = [];
  const sink = { record: async (...args) => records.push(args) };
  try {
    const scenario = createManualCase1Scenario(RAW);
    assert.equal(scenario.id, "manual.case1");
    assert.equal(scenario.manualGate, "pending");
    assert.equal(scenario.databaseRequired, true);
    await scenario.prepare({ context: { root, paths }, sink, phase: "prepared" });
    assert.deepEqual(await readdir(paths.workspace), []);
    assert.deepEqual(await readdir(paths.data), []);
    assert.match(await readFile(paths.config_file, "utf8"), /access_mode = "default"/);
    assert.equal(records.find(row => row[0] === "case1-input")[1].provider_owned, false);
    assert.equal(records.find(row => row[0] === "fixture-prepared")[1].identities.sentinel, null);
    await writeFile(path.join(paths.workspace, "unexpected.txt"), "not empty");
    await assert.rejects(createManualCase1Scenario(RAW).prepare({ context: { root, paths }, sink, phase: "prepared" }), error => error.code === "case1-not-fresh");
  } finally {
    assert.equal(path.dirname(path.resolve(root)).toLowerCase(), path.resolve(parent).toLowerCase());
    await rm(root, { recursive: true });
  }
});

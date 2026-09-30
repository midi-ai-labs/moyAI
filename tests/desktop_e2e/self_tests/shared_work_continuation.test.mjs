import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { startSharedWorkflowProvider } from "../drivers/shared_work_runner_fixture.mjs";
import { sameFixtureFolder, sharedTurnSettled, sharedTurnIdentityMatches } from "../scenarios/shared_work_continuation.mjs";
import { waitForObservation } from "../core/deadline.mjs";
import { waitForSharedComposer } from "../scenarios/shared_work_navigation.mjs";
import { createScenario } from "../scenario_registry.mjs";

test("an observed turn must keep its accepted root, submitted input and conversation identity", () => {
  const expected = { id: "new-root", previousJobId: "old-root", projectId: "project", title: "Conversation", prompt: "Revised request",
    conversationId: "conversation", revisesJobId: "old-root" };
  const job = { id: "new-root", parent_id: null, project_id: "project", title: "Conversation", input: { prompt: "Revised request" },
    conversation_id: "conversation", revises_job_id: "old-root" };
  assert.equal(sharedTurnIdentityMatches(job, expected), true);
  for (const altered of [{ id: "other-root" }, { parent_id: "parent" }, { project_id: "other-project" }, { title: "Other title" },
    { input: { prompt: "Other input" } }, { conversation_id: "other-conversation" }, { revises_job_id: "other-predecessor" }]) {
    assert.equal(sharedTurnIdentityMatches({ ...job, ...altered }, expected), false);
  }
});

test("an admitted turn survives read backpressure until a successful terminal projection", async () => {
  const job = { id: "new-root", parent_id: null, state: "queued", uncertainty_reason: null };
  const busy = { detail: job, error: "temporary read admission failure" };
  const observations = [busy, { ...busy, detail: { ...job, state: "succeeded" } },
    { detail: { ...job, state: "succeeded" }, error: null }];
  let clock = 0, samples = 0;
  const result = await waitForObservation({ label: "admitted turn", sample: async () => observations[samples++],
    accept: p => sharedTurnSettled(p, "old-root"), timeoutMs: 1000, pollMs: 100,
    now: () => clock, sleep: async ms => { clock += ms; } });
  assert.equal(samples, 3);
  assert.equal(result.value.detail.state, "succeeded");
  assert.equal(result.value.error, null);
});

test("turn waits expose submission rejection and exact job failure without treating them as success", () => {
  for (const detail of [null, { id: "old-root", state: "succeeded" }]) {
    assert.equal(sharedTurnSettled({ detail, error: "submission rejected" }, "old-root"), true);
    assert.equal(sharedTurnSettled({ detail, error: null }, "old-root"), false);
  }
  for (const state of ["failed", "cancelled"]) {
    assert.equal(sharedTurnSettled({ detail: { id: "new-root", state }, error: null }, "old-root"), true);
  }
  assert.equal(sharedTurnSettled({ detail: { id: "new-root", state: "running", uncertainty_reason: "runner unavailable" }, error: null }, "old-root"), true);
  assert.equal(sharedTurnSettled({ detail: { id: "child", parent_id: "new-root", state: "succeeded" }, error: null }, "old-root"), false);
});

test("persistent read failure keeps the existing bounded wait and its final error evidence", async () => {
  let clock = 0;
  const projection = { detail: { id: "new-root", state: "queued" }, error: "read admission still unavailable" };
  await assert.rejects(waitForObservation({ label: "admitted turn", sample: async () => projection,
    accept: p => sharedTurnSettled(p, "old-root"), timeoutMs: 300, pollMs: 100,
    now: () => clock, sleep: async ms => { clock += ms; } }), error => {
    assert.equal(error.code, "observation-timeout");
    assert.equal(error.evidence.elapsed_ms, 300);
    assert.equal(error.evidence.last_value, projection);
    return true;
  });
});

test("shared composer waits past an old enabled frame and resets settling after busy or occluded frames", async () => {
  const ready = { count: 1, visible: true, enabled: true, center_hit: true };
  const observations = [ready, { ...ready, enabled: false }, ready, { ...ready, center_hit: false }, ready, ready, ready];
  let samples = 0;
  const target = { selector: "#shared-prompt", identity: { tag: "TEXTAREA", id: "shared-prompt" } };
  const input = { observeExactTarget: async observed => {
    assert.equal(observed, target);
    return { observation: observations[Math.min(samples++, observations.length - 1)] };
  } };
  await waitForSharedComposer(input, target);
  assert.equal(samples, 7);
});

test("selected and Runner-canonical Windows folders identify the same existing directory", () => {
  const chosen = path.resolve("shared-existing-folder");
  assert.equal(sameFixtureFolder(path.toNamespacedPath(chosen), chosen), true);
  assert.equal(sameFixtureFolder(path.resolve("another-folder"), chosen), false);
  assert.equal(sameFixtureFolder(null, chosen), false);
});

test("the combined scenario retains a real runner and an explicit actual GUI gate", () => {
  const scenario = createScenario("settings.shared-work-continuation", { runnerBinary: path.resolve("runner.exe"), runnerTestBinary: path.resolve("runner-test.exe") });
  assert.equal(scenario.id, "settings.shared-work-continuation");
  assert.equal(scenario.manualGate, "pending");
  assert.equal(scenario.databaseRequired, true);
  for (const method of ["prepare", "execute", "quiesce", "cleanup", "requestGracefulExit"]) assert.equal(typeof scenario[method], "function");
});
test("the controlled provider distinguishes parent tool arguments from the child task and checks canonical reuse", async () => {
  const provider = await startSharedWorkflowProvider();
  const post = async messages => {
    const response = await fetch(`${provider.baseUrl}/chat/completions`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ model: "shared-workflow", messages, tools: [{ function: { name: "shared_delegate" } }] }) });
    assert.equal(response.status, 200); return response.text();
  };
  try {
    assert.match(await post([{ role: "user", content: "desktop-transfer-parent" }]), /shared_delegate/);
    provider.releaseChild();
    assert.match(await post([{ role: "user", content: "desktop-transfer-child" }]), /require_escalated/);
    assert.match(await post([{ role: "user", content: "desktop-transfer-child" }, { role: "tool", tool_call_id: "child-approval", content: "approved" }]), /apply_patch/);
    const parent = [{ role: "user", content: "desktop-transfer-parent" }, { role: "assistant", content: "desktop-transfer-child is a delegated tool argument" }, { role: "tool", tool_call_id: "parent-child", content: "solver result" }];
    assert.match(await post(parent), /親は子の解析結果/);
    assert.match(await post([...parent, { role: "user", content: "desktop-followup" }]), /追加依頼でも/);
    assert.deepEqual(provider.failures, []);
  } finally { await provider.close(); }
});

test("the focused shared conversation provider answers two ordinary turns without a PC selector", async () => {
  const provider = await startSharedWorkflowProvider();
  const post = async messages => {
    const response = await fetch(`${provider.baseUrl}/chat/completions`, { method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify({ model: "shared-workflow", messages, tools: [] }) });
    assert.equal(response.status, 200); return response.text();
  };
  try {
    const first = { role: "user", content: "desktop-conversation-start: answer briefly" };
    const second = { role: "user", content: "desktop-conversation-followup: continue" };
    assert.match(await post([first]), /最初の依頼をこのプロジェクトで実行しました/);
    assert.match(await post([first, second]), /追加の依頼にも、同じ共有チャットで回答しました/);
    assert.match(await post([first, { role: "user", content: "desktop-conversation-revised: answer after edit" }]), /編集後の依頼をこのプロジェクトで実行しました/);
    assert.deepEqual(provider.failures, []);
  } finally { await provider.close(); }
});

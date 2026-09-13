import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { startSharedWorkflowProvider } from "../drivers/shared_work_runner_fixture.mjs";
import { createScenario } from "../scenario_registry.mjs";

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

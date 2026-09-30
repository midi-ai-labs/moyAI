import assert from "node:assert/strict";
import test from "node:test";
import { runInNewContext } from "node:vm";
import { createScenario } from "../scenario_registry.mjs";
import { managedExecutionReady, oneTimeExecutionSetup, observeConnectionDiagnosis, gatewayConnectionDiagnosed, saveExecutionProject, quiesceDeviceExecutionResources } from "../scenarios/device_execution.mjs";

test("execution scenario uses the common lifecycle and requires explicit isolated Runner input only at preparation", () => {
  const scenario = createScenario("settings.device-execution");
  assert.equal(scenario.manualGate, "pending"); assert.equal(scenario.databaseRequired, true);
  assert.deepEqual(scenario.environment, {});
  for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) assert.equal(typeof scenario[method], "function");
  assert.throws(() => createScenario("settings.device-execution", { runnerTestBinary:"relative.exe" }), /absolute/);
});
test("automatic setup completion requires the selected PC and prepared environment rather than a live UI alone", () => {
  const view = { state:"ready", accepting:true, can_pause:true, projects:[{id:"p", can_control:true, can_execute:true, preparation_state:"ready", environment_id:"env"}], access_mode:"default", directory:"C:/work", review:null };
  assert.equal(managedExecutionReady(view,"p"), true);
  for (const changed of [{accepting:false}, {review:{id:"unconfirmed"}}, {projects:[{...view.projects[0],preparation_state:"pending"}]}, {projects:[{...view.projects[0],can_execute:false}]}]) assert.equal(managedExecutionReady({...view,...changed},"p"), false);
  assert.equal(managedExecutionReady(view,"different-project"), false);
});
test("restart oracle rejects repeated consent or manual launch commands", () => {
  const call = kind => ({command:"device_execution_command",args:{request:{kind}}});
  assert.equal(oneTimeExecutionSetup([call("prepare"),call("enable")]), true);
  assert.equal(oneTimeExecutionSetup([call("prepare"),call("enable"),call("enable")]), false);
  assert.equal(oneTimeExecutionSetup([call("prepare"),call("enable"),call("start")]), false);
  assert.equal(oneTimeExecutionSetup([call("enable")]), false);
});

test("connection diagnosis reads the exact scope key including its JSON attribute characters", async () => {
  const rows = [
    ["device-network-diagnostic-hub", "obsolete locator"],
    ['device-network-diagnostic-["hub",""]', "共有仕事の接続"],
    ['device-network-diagnostic-["gateway",""]', "AI中継サーバーへの暗号化接続 確認済み"],
    ['device-network-diagnostic-["peer","hub"]', "different peer"],
  ].map(([key, textContent]) => ({ textContent, getAttribute: name => name === "data-settings-passive" ? key : null }));
  const cdp = { evaluate: async expression => runInNewContext(expression, { document: { querySelectorAll: () => rows } }) };
  assert.equal(await observeConnectionDiagnosis(cdp, "hub"), "共有仕事の接続");
  const gateway = await observeConnectionDiagnosis(cdp, "gateway");
  assert.equal(gateway, "AI中継サーバーへの暗号化接続 確認済み");
  assert.equal(gatewayConnectionDiagnosed(gateway), true);
  assert.equal(gatewayConnectionDiagnosed(await observeConnectionDiagnosis(cdp, "hub")), false);
  assert.equal(await observeConnectionDiagnosis(cdp, "receiver"), "");
});

test("AI connection scope waits for its own current public stage, not a certificate or unrelated diagnosis", () => {
  assert.equal(gatewayConnectionDiagnosed("診断日時: 本日 AI中継サーバーへの暗号化接続 確認できません"), true);
  for (const text of ["", "接続を診断しています…", "AI中継サーバーの証明書更新 確認済み", "共有仕事の接続 確認済み", "モデルGatewayへのTLS接続"]) {
    assert.equal(gatewayConnectionDiagnosed(text), false);
  }
});

function projectEditor({ conflict = false, error = "", changedDraft = false } = {}) {
  const calls = [], records = [], label = "実行試験", draft = [{ name: "label", value: label, checked: null },
    { name: "controller_device_ids", value: "pc-a", checked: true }, { name: "runner_device_ids", value: "pc-a", checked: true }];
  let saved = 0, compared = false;
  const page = { evaluate: async () => ({ closed: !conflict && !error, conflict, error }), locator(selector) {
    return {
      evaluate: async () => compared && changedDraft ? draft.slice(0, 2) : structuredClone(draft),
      innerText: async () => `今回の入力: ${label}`,
      waitFor: async ({ state }) => {
        if (selector === "#shared-admin-form") assert.equal(saved, conflict ? 2 : 1);
        else assert.equal(state, "visible");
      },
      click: async () => { calls.push(selector); if (selector === "#shared-admin-save") saved++; if (selector === "#shared-admin-accept-comparison") compared = true; },
    };
  } };
  return { page, label, calls, records, sink: { record: async (...args) => records.push(args) } };
}

test("execution project saves once or explicitly compares a preserved conflict before its second save", async () => {
  for (const conflict of [false, true]) {
    const f = projectEditor({ conflict });
    assert.deepEqual(await saveExecutionProject(f.page, f.sink, f.label), { saves: conflict ? 2 : 1, conflict_reviewed: conflict });
    assert.deepEqual(f.calls, conflict ? ["#shared-admin-save", "#shared-admin-review-conflict", "#shared-admin-accept-comparison", "#shared-admin-save"] : ["#shared-admin-save"]);
    assert.equal(f.records.length, conflict ? 1 : 0);
  }
});

test("execution project never retries validation errors or a changed PC selection", async () => {
  for (const [options, expected] of [[{ error: "PCの設定を確認してください" }, /PCの設定を確認/], [{ conflict: true, changedDraft: true }, /changed the project draft/]]) {
    const f = projectEditor(options);
    await assert.rejects(saveExecutionProject(f.page, f.sink, f.label), expected);
    assert.equal(f.calls.filter(value => value === "#shared-admin-save").length, 1);
    assert.equal(f.records.length, 0);
  }
});

test("execution cleanup closes every independent resource after a failure and preserves its reason", async () => {
  for (const failed of ["runner", "provider", "hub"]) {
    const events = [];
    const close = name => async () => {
      events.push(name);
      if (name === failed) throw Object.assign(new Error(`${name} exact owner could not settle`), { code: "owner-mismatch" });
      return { pass: true, resource: name };
    };
    const result = await quiesceDeviceExecutionResources({ runner: { quiesce: close("runner") }, provider: { close: close("provider") }, resource: { close: close("hub") } });
    assert.deepEqual(events, ["runner", "provider", "hub"]);
    assert.equal(result.pass, false);
    assert.deepEqual(result.failures, [{ resource: failed, code: "owner-mismatch", message: `${failed} exact owner could not settle` }]);
    assert.equal(result[failed].pass, false);
  }
});

test("an unsuccessful Runner settlement cannot be hidden by successful provider and Hub cleanup", async () => {
  let closed = 0;
  const result = await quiesceDeviceExecutionResources({
    runner: { quiesce: async () => ({ pass: false, forced: true, process_id: 12 }) },
    provider: { close: async () => { closed++; } }, resource: { close: async () => { closed++; return { pass: true }; } },
  });
  assert.equal(closed, 2); assert.equal(result.pass, false); assert.equal(result.runner.forced, true);
  assert.deepEqual(result.failures, [{ resource: "runner", code: "resource-not-settled" }]);
  const empty = await quiesceDeviceExecutionResources({});
  assert.equal(empty.pass, true); assert.equal(empty.runner.not_started, true);
});

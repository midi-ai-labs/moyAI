import assert from "node:assert/strict";
import test from "node:test";
import { runInNewContext } from "node:vm";
import { createScenario } from "../scenario_registry.mjs";
import { managedExecutionReady, oneTimeExecutionSetup, observeConnectionDiagnosis, quiesceDeviceExecutionResources } from "../scenarios/device_execution.mjs";

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
    ['device-network-diagnostic-["gateway",""]', "モデルGatewayへのTLS接続"],
    ['device-network-diagnostic-["peer","hub"]', "different peer"],
  ].map(([key, textContent]) => ({ textContent, getAttribute: name => name === "data-settings-passive" ? key : null }));
  const cdp = { evaluate: async expression => runInNewContext(expression, { document: { querySelectorAll: () => rows } }) };
  assert.equal(await observeConnectionDiagnosis(cdp, "hub"), "共有仕事の接続");
  assert.equal(await observeConnectionDiagnosis(cdp, "gateway"), "モデルGatewayへのTLS接続");
  assert.equal(await observeConnectionDiagnosis(cdp, "receiver"), "");
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

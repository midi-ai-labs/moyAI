import assert from "node:assert/strict";
import test from "node:test";
import { createScenario } from "../scenario_registry.mjs";
import { managedExecutionReady, oneTimeExecutionSetup } from "../scenarios/device_execution.mjs";

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

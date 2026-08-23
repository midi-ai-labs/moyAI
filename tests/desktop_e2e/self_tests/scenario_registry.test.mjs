import assert from "node:assert/strict";
import test from "node:test";

import { parseArguments } from "../run_scenario.mjs";
import { createScenario, scenarioIds } from "../scenario_registry.mjs";

test("one registry binds every reusable scenario to the common runner contract", () => {
  assert.deepEqual(scenarioIds, [
    "shell.baseline",
    "agent.interrupt",
    "input.pointer-keyboard",
    "native-dialog.cancel",
    "prompt-review.cancel",
    "provider.restart",
    "settings.docling-readiness",
    "settings.initial-setup",
    "settings.preferences",
    "settings.session",
    "run.stop",
  ]);
  for (const id of scenarioIds) {
    const scenario = createScenario(id);
    assert.equal(scenario.id, id);
    for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) {
      assert.equal(typeof scenario[method], "function", `${id}.${method}`);
    }
  }
  assert.notEqual(createScenario("provider.restart"), createScenario("provider.restart"));
  assert.notEqual(createScenario("settings.docling-readiness"), createScenario("settings.docling-readiness"));
  assert.notEqual(createScenario("settings.initial-setup"), createScenario("settings.initial-setup"));
  assert.notEqual(createScenario("settings.preferences"), createScenario("settings.preferences"));
  assert.notEqual(createScenario("settings.session"), createScenario("settings.session"));
  assert.notEqual(createScenario("prompt-review.cancel"), createScenario("prompt-review.cancel"));
  assert.notEqual(createScenario("run.stop"), createScenario("run.stop"));
  assert.notEqual(createScenario("agent.interrupt"), createScenario("agent.interrupt"));
  assert.throws(() => createScenario("run-95"), /unknown Desktop E2E scenario/);
});

test("generic runner accepts one scenario identity without runner-specific arguments", () => {
  assert.deepEqual(parseArguments([
    "--binary", "C:\\bin\\moyai-desktop.exe",
    "--artifact-parent", "C:\\evidence",
    "--scenario", "provider.restart",
  ]), {
    binary: "C:\\bin\\moyai-desktop.exe",
    "artifact-parent": "C:\\evidence",
    scenario: "provider.restart",
  });
  assert.throws(() => parseArguments(["--run-number", "95"]), /unknown argument/);
});

import assert from "node:assert/strict";
import test from "node:test";

import { parseArguments, readScenarioConfig } from "../run_scenario.mjs";
import { createScenario, scenarioIds } from "../scenario_registry.mjs";

const case52Options = Object.freeze({
  fixture_source: "C:\\fixture",
  provider_base_url: "http://192.0.2.1:1234",
  main_model: "example/main",
  side_model: "example/side",
  expected_main_variant: "example/main@q6",
  expected_side_variant: "example/side@q4",
});

test("one registry binds every reusable scenario to the common runner contract", () => {
  assert.deepEqual(scenarioIds, [
    "shell.baseline",
    "agent.interrupt",
    "input.pointer-keyboard",
    "manual.case5_2",
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
    const scenario = createScenario(id, id === "manual.case5_2" ? case52Options : {});
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
  assert.notEqual(createScenario("manual.case5_2", case52Options), createScenario("manual.case5_2", case52Options));
  assert.throws(() => createScenario("run-95"), /unknown Desktop E2E scenario/);
  assert.throws(
    () => createScenario("shell.baseline", { ignored: true }),
    /does not accept options/,
  );
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

test("generic runner binds one hashed scenario config without exposing scenario-specific CLI flags", async (context) => {
  const { mkdtemp, rm, writeFile } = await import("node:fs/promises");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const root = await mkdtemp(join(tmpdir(), "moyai-e2e-scenario-config-"));
  context.after(() => rm(root, { recursive: true, force: true }));
  const candidate = join(root, "scenario.json");
  const content = '{"fixture_source":"C:\\\\fixture"}\n';
  await writeFile(candidate, content, { flag: "wx" });

  assert.deepEqual(parseArguments([
    "--binary", "C:\\bin\\moyai-desktop.exe",
    "--artifact-parent", "C:\\evidence",
    "--scenario", "manual.case5_2",
    "--scenario-config", candidate,
  ]), {
    binary: "C:\\bin\\moyai-desktop.exe",
    "artifact-parent": "C:\\evidence",
    scenario: "manual.case5_2",
    "scenario-config": candidate,
  });
  const loaded = await readScenarioConfig(candidate);
  assert.deepEqual(loaded.options, { fixture_source: "C:\\fixture" });
  assert.match(loaded.identity.sha256, /^[a-f0-9]{64}$/);
  assert.equal(loaded.identity.size_bytes, Buffer.byteLength(content));
  await assert.rejects(readScenarioConfig(root), /not a file/);
});

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
const liveProviderOptions = Object.freeze({
  provider_base_url: "http://192.0.2.10:8119/v1",
  model: "example/Qwen-27B",
});
const lmStudioThinkingOptions = Object.freeze({
  provider_base_url: "http://127.0.0.1:1234",
  model: "qwen/qwen3.8-27b",
});
const case52OpenAiOptions = Object.freeze({
  fixture_source: "C:\\fixture",
  provider_profile: "openai_compatible",
  provider_base_url: "http://192.0.2.10:8119/v1",
  main_model: "Qwen3.8-27B-4bit",
});

test("one registry binds every reusable scenario to the common runner contract", () => {
  assert.deepEqual(scenarioIds, [
    "shell.baseline",
    "shell.about",
    "shell.lynx",
    "shell.managed-lifecycle",
    "hub.connection-settings",
    "hub.browser-enrollment",
    "hub.join-retry-controls",
    "hub.receiver-settings-controls",
    "hub.outgoing-controls",
    "agent.interrupt",
    "history.restart-prepend",
    "history.terminal-reconcile",
    "input.pointer-keyboard",
    "input.command-palette-insertion",
    "mcp.receiver-live",
    "mcp.receiver-stop",
    "mcp.history-pagination",
    "mcp.receiver-approve",
    "mcp.receiver-deny",
    "mcp.receiver-abort",
    "manual.case5_2",
    "manual.provider-openai-compatible",
    "manual.provider-lm-studio-thinking",
    "manual.permission-guardian-openai-compatible",
    "manual.permission-temp-escalation-lm-studio",
    "native-dialog.cancel",
    "navigation.session-management",
    "navigation.workspace-controls",
    "navigation.modal-keyboard-controls",
    "navigation.running-controls",
    "navigation.external-rejoin",
    "navigation.external-sidebar-stop",
    "navigation.external-palette-rejoin",
    "main.steer-controls",
    "history.rail-controls",
    "main.goal-query",
    "prompt-review.entries-enhanced",
    "navigation.shortcut-row-controls",
    "main.palette-run-controls",
    "output.history-navigation",
    "prompt-review.cancel",
    "prompt-review.raw-interaction",
    "review.uncommitted-controls",
    "prompt-review.submit-raw",
    "prompt-review.submit-enhanced",
    "permission.restart-guardian",
    "permission.restart-guardian-chat",
    "permission.temp-escalation",
    "provider.chat-tool-continuation",
    "provider.responses-compaction-retry",
    "provider.responses-progress",
    "provider.restart",
    "settings.docling-readiness",
    "settings.initial-setup",
    "settings.initial-setup-hub",
    "settings.preferences",
    "settings.preferences-config",
    "settings.session",
    "settings.field-controls",
    "settings.additional-controls",
    "settings.initial-additional-controls",
    "settings.session-discard-close",
    "settings.temporary-apply-controls",
    "settings.initial-field-controls",
    "settings.session-field-controls",
    "settings.mcp-peer-controls",
    "navigation.menu-entry-controls",
    "navigation.palette-entry-controls",
    "run.next-turn",
    "run.stop",
    "side-chat.quote",
    "side-chat.session",
  ]);
  for (const id of scenarioIds) {
    const options = id === "manual.case5_2"
      ? case52Options
      : id === "manual.provider-openai-compatible"
        ? liveProviderOptions
        : id === "manual.provider-lm-studio-thinking"
          ? lmStudioThinkingOptions
          : id === "manual.permission-guardian-openai-compatible"
            ? liveProviderOptions
            : id === "manual.permission-temp-escalation-lm-studio"
              ? lmStudioThinkingOptions
              : {};
    const scenario = createScenario(id, options);
    assert.equal(scenario.id, id);
    for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) {
      assert.equal(typeof scenario[method], "function", `${id}.${method}`);
    }
  }
  assert.notEqual(createScenario("provider.restart"), createScenario("provider.restart"));
  assert.notEqual(createScenario("history.restart-prepend"), createScenario("history.restart-prepend"));
  assert.notEqual(createScenario("history.terminal-reconcile"), createScenario("history.terminal-reconcile"));
  assert.notEqual(createScenario("settings.docling-readiness"), createScenario("settings.docling-readiness"));
  assert.notEqual(createScenario("settings.initial-setup"), createScenario("settings.initial-setup"));
  assert.notEqual(createScenario("settings.preferences"), createScenario("settings.preferences"));
  assert.notEqual(createScenario("settings.preferences-config"), createScenario("settings.preferences-config"));
  assert.notEqual(createScenario("settings.session"), createScenario("settings.session"));
  assert.notEqual(createScenario("navigation.session-management"), createScenario("navigation.session-management"));
  assert.notEqual(createScenario("input.command-palette-insertion"), createScenario("input.command-palette-insertion"));
  assert.notEqual(createScenario("prompt-review.cancel"), createScenario("prompt-review.cancel"));
  assert.notEqual(
    createScenario("permission.restart-guardian"),
    createScenario("permission.restart-guardian"),
  );
  assert.notEqual(
    createScenario("permission.restart-guardian-chat"),
    createScenario("permission.restart-guardian-chat"),
  );
  assert.notEqual(
    createScenario("permission.temp-escalation"),
    createScenario("permission.temp-escalation"),
  );
  assert.notEqual(
    createScenario("provider.chat-tool-continuation"),
    createScenario("provider.chat-tool-continuation"),
  );
  assert.notEqual(
    createScenario("provider.responses-compaction-retry"),
    createScenario("provider.responses-compaction-retry"),
  );
  assert.notEqual(
    createScenario("provider.responses-progress"),
    createScenario("provider.responses-progress"),
  );
  assert.notEqual(createScenario("run.next-turn"), createScenario("run.next-turn"));
  assert.notEqual(createScenario("run.stop"), createScenario("run.stop"));
  assert.notEqual(createScenario("side-chat.quote"), createScenario("side-chat.quote"));
  assert.notEqual(createScenario("side-chat.session"), createScenario("side-chat.session"));
  assert.notEqual(createScenario("agent.interrupt"), createScenario("agent.interrupt"));
  assert.notEqual(createScenario("manual.case5_2", case52Options), createScenario("manual.case5_2", case52Options));
  assert.notEqual(
    createScenario("manual.case5_2", case52OpenAiOptions),
    createScenario("manual.case5_2", case52OpenAiOptions),
  );
  assert.notEqual(
    createScenario("manual.provider-openai-compatible", liveProviderOptions),
    createScenario("manual.provider-openai-compatible", liveProviderOptions),
  );
  assert.notEqual(
    createScenario("manual.provider-lm-studio-thinking", lmStudioThinkingOptions),
    createScenario("manual.provider-lm-studio-thinking", lmStudioThinkingOptions),
  );
  assert.notEqual(
    createScenario("manual.permission-guardian-openai-compatible", liveProviderOptions),
    createScenario("manual.permission-guardian-openai-compatible", liveProviderOptions),
  );
  assert.notEqual(
    createScenario("manual.permission-temp-escalation-lm-studio", lmStudioThinkingOptions),
    createScenario("manual.permission-temp-escalation-lm-studio", lmStudioThinkingOptions),
  );
  assert.throws(
    () => createScenario("manual.provider-openai-compatible"),
    /provider_base_url must be a non-empty string/,
  );
  assert.throws(
    () => createScenario("manual.provider-lm-studio-thinking"),
    /provider_base_url must be a non-empty string/,
  );
  assert.throws(
    () => createScenario("manual.permission-guardian-openai-compatible"),
    /provider_base_url must be a non-empty string/,
  );
  assert.throws(
    () => createScenario("manual.permission-temp-escalation-lm-studio"),
    /provider_base_url must be a non-empty string/,
  );
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

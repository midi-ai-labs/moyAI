import assert from "node:assert/strict";
import test from "node:test";
import { expectedConfigCommandValues } from "../core/config_command_values.mjs";
import { assertExactDesktopCommandSequence } from "../drivers/desktop_command_probe.mjs";
import { expectedLmStudioThinkingGlobalSave } from "../scenarios/provider_lm_studio_thinking.mjs";
import { expectedProviderConnectionGlobalSave } from "../scenarios/provider_connection_live.mjs";
import { case52ExpectedMainGlobalSave, case52ExpectedSideGlobalSave } from "../scenarios/case5_2.mjs";
import { expectedInitialSetupFinishCommand } from "../scenarios/settings_initial_setup.mjs";
import { expectedGlobalSave } from "../scenarios/settings_preferences.mjs";

function surface() {
  return { projection: {
    config_target: { workspacePath: "C:\\fixture", sessionId: null, configGeneration: "7" },
    startup: { setup_target: { workspacePath: "C:\\fixture", globalConfigPath: "C:\\fixture\\config.toml", setupGeneration: "3" } },
    config_fields: [
      { key: "model.base_url", value: "http://127.0.0.1:9" },
      { key: "model.model", value: "before" },
      { key: "model.provider_profile", value: "lm_studio" },
      { key: "model.api_key_env", value: "" },
      { key: "model.extra_headers_json", value: "" },
      { key: "model.supports_tools", value: "true" },
      { key: "side_chat.base_url", value: "http://127.0.0.1:9" },
      { key: "side_chat.model", value: "before" },
      { key: "side_chat.provider_profile", value: "openai_responses" },
      { key: "side_chat.api_key_env", value: "OLD_SIDE_KEY" },
      { key: "side_chat.system_prompt", value: "" },
      { key: "approve.base_url", value: "http://127.0.0.1:10" },
      { key: "approve.model", value: "guardian" },
      { key: "approve.provider_profile", value: "openai_responses" },
      { key: "approve.api_key_env", value: "OLD_APPROVE_KEY" },
      { key: "permissions.access_mode", value: "default" },
    ],
  } };
}

test("ordinary edits omit unchanged Approve and private fields from exact Save expectations", () => {
  const baseline = surface().projection;
  const values = expectedConfigCommandValues(baseline, { "side_chat.system_prompt": "Consult the saved history" });
  assert.deepEqual(values.map(value => value.key), [
    "model.base_url", "model.model", "model.provider_profile", "model.supports_tools",
    "side_chat.base_url", "side_chat.model", "side_chat.provider_profile", "side_chat.system_prompt",
    "permissions.access_mode",
  ]);
  const command = { command: "save_global_config", args: { values, expectedTarget: baseline.config_target } };
  const snapshot = { found: true, sequence: 1, dropped_through: 0, calls: [{ sequence: 1, ...command }] };
  assertExactDesktopCommandSequence(snapshot, { expected: [command] });
  const obsolete = structuredClone(command);
  obsolete.args.values.push({ key: "approve.model", text: "guardian" });
  assert.throws(() => assertExactDesktopCommandSequence(snapshot, { expected: [obsolete] }), { code: "desktop-command-probe-call-mismatch" });
});

test("connection changes reuse a baseline credential only after explicit same-role input", () => {
  const projection = surface().projection;
  const overrides = { "side_chat.base_url": "http://127.0.0.1:20" };
  const omitted = expectedConfigCommandValues(projection, overrides);
  assert.ok(!omitted.some(value => value.key === "side_chat.api_key_env"));
  const explicit = expectedConfigCommandValues(projection, overrides, { editedKeys: ["side_chat.api_key_env"] });
  assert.deepEqual(explicit.find(value => value.key === "side_chat.api_key_env"), { key: "side_chat.api_key_env", text: "OLD_SIDE_KEY" });
  assert.ok(!explicit.some(value => value.key === "approve.api_key_env"));
  const cleared = expectedConfigCommandValues(projection, { ...overrides, "side_chat.api_key_env": "" });
  assert.deepEqual(cleared.find(value => value.key === "side_chat.api_key_env"), { key: "side_chat.api_key_env", text: "" });
});

test("current provider and Initial Setup command oracles share the private-field omission contract", () => {
  const baseline = surface();
  const baseUrl = "http://127.0.0.1:20";
  const commands = [
    expectedLmStudioThinkingGlobalSave(baseline, { providerBaseUrl: baseUrl, model: "after" }),
    expectedProviderConnectionGlobalSave(baseline, { providerBaseUrl: baseUrl, model: "after" }),
    case52ExpectedMainGlobalSave(baseline, { providerBaseUrl: baseUrl, mainModel: "after", providerProfile: "lm_studio" }),
    case52ExpectedSideGlobalSave(baseline, { providerBaseUrl: baseUrl, sideModel: "after", providerProfile: "openai_responses" }),
    expectedInitialSetupFinishCommand(baseline, null, { "model.base_url": baseUrl, "model.model": "after" }),
    expectedGlobalSave(baseline, { "model.model": "after" }),
  ];
  for (const command of commands) {
    assert.ok(!command.args.values.some(value => value.key.startsWith("approve.")), command.command);
    assert.ok(!command.args.values.some(value => ["model.api_key_env", "model.extra_headers_json"].includes(value.key)), command.command);
    assert.deepEqual(command.args.expectedTarget ?? command.args.expectedConfigTarget, baseline.projection.config_target);
  }
  const reused = case52ExpectedSideGlobalSave(baseline, {
    providerBaseUrl: baseUrl, sideModel: "after", providerProfile: "openai_responses", sideApiKeyEnv: "OLD_SIDE_KEY",
  });
  assert.deepEqual(reused.args.values.find(value => value.key === "side_chat.api_key_env"), { key: "side_chat.api_key_env", text: "OLD_SIDE_KEY" });
  const imported = expectedInitialSetupFinishCommand(baseline, "1", { "model.api_key_env": "NEW_KEY", "approve.model": "new-guardian" });
  assert.deepEqual(imported.args.values.find(value => value.key === "model.api_key_env"), { key: "model.api_key_env", text: "NEW_KEY" });
  assert.deepEqual(imported.args.values.find(value => value.key === "approve.model"), { key: "approve.model", text: "new-guardian" });
});

test("an imported connection snapshot explicitly preserves its supplied private values", () => {
  const baseline = surface();
  const command = expectedInitialSetupFinishCommand(baseline, "1", {
    "model.base_url": "http://127.0.0.1:20",
    "side_chat.base_url": "http://127.0.0.1:21",
  });
  for (const [key, text] of [["model.api_key_env", ""], ["model.extra_headers_json", ""], ["side_chat.api_key_env", "OLD_SIDE_KEY"]]) {
    assert.deepEqual(command.args.values.find(value => value.key === key), { key, text });
  }
  assert.ok(!command.args.values.some(value => value.key.startsWith("approve.")));
});

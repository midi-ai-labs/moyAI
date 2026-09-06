import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { normalizeHubRuntimeOptions, createHubRuntimeLiveScenario } from "../scenarios/hub_runtime_live.mjs";

const valid = () => ({ hub_binary: path.resolve("target/debug/moyai-hub.exe"), provider_base_url: "http://127.0.0.1:8119/v1", model: "hosted-model" });
test("two-app Hub live scenario requires explicit binary and unmanaged provider inputs", () => {
  const options = normalizeHubRuntimeOptions(valid());
  assert.equal(options.model, "hosted-model");
  assert.equal(options.providerBaseUrl, "http://127.0.0.1:8119/v1");
  const scenario = createHubRuntimeLiveScenario(valid());
  assert.equal(scenario.id, "manual.hub-runtime");
  assert.equal(scenario.databaseRequired, true);
  assert.equal(typeof scenario.quiesce, "function");
});
test("live Hub resource never accepts credentials or arbitrary launcher options through its scenario config", () => {
  for (const options of [
    { ...valid(), provider_base_url: "http://name:secret@127.0.0.1:8119/v1" },
    { ...valid(), provider_base_url: "file:///tmp/model" },
    { ...valid(), hub_binary: "relative.exe" },
    { ...valid(), model: "" },
    { ...valid(), token: "do-not-persist" },
  ]) assert.throws(() => normalizeHubRuntimeOptions(options));
});

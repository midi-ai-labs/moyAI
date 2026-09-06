import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { normalizeDeviceNetworkOptions, validateDeviceNetworkVerdict, createHubDeviceNetworkScenario } from "../scenarios/hub_device_network.mjs";

const valid = () => ({ hub_binary: path.resolve("hub.exe"), provider_base_url: "http://127.0.0.1:8119/v1", model: "fixture-model" });

test("native two-app scenario accepts only bounded unmanaged provider and binary inputs", () => {
  assert.equal(createHubDeviceNetworkScenario(valid()).id, "manual.hub-device-network");
  assert.equal(createHubDeviceNetworkScenario(valid()).manualGate, "pending");
  for (const value of [null, [], {}, { ...valid(), token: "private" }, { ...valid(), hub_binary: "relative" },
    { ...valid(), provider_base_url: "https://user:secret@host/v1" }, { ...valid(), provider_base_url: "http://host/?key=secret" },
    { ...valid(), provider_base_url: "file:///x" }, { ...valid(), model: "model\n[permissions]" }]) {
    assert.throws(() => normalizeDeviceNetworkOptions(value));
  }
});

test("manual verdict never silently converts missing, pending or physical-machine evidence to pass", () => {
  const valid = { oracle: "pass", manual: "pending", scope: "same-host-hub-desktop", observations: ["Enrollment observed; runtime acceptance pending"] };
  assert.equal(validateDeviceNetworkVerdict(valid).manual, "pending");
  for (const value of [{ ...valid, observations: [] }, { ...valid, scope: "two-physical-machines" }, { ...valid, manual: undefined },
    { ...valid, observations: [""] }, { ...valid, observations: ["x".repeat(2049)] }]) {
    assert.throws(() => validateDeviceNetworkVerdict(value));
  }
  assert.deepEqual(validateDeviceNetworkVerdict({ ...valid, manual: "fail", ignored: "not emitted" }), { ...valid, manual: "fail" });
});

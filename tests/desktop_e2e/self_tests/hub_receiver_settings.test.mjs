import assert from "node:assert/strict";
import test from "node:test";
import { createHubReceiverSettingsScenario, receiverSettingsMatch, receiverFormMatch, invalidReceiverDraftMatch, receiverSettingsOutcome } from "../scenarios/hub_receiver_settings.mjs";

const expected = { device_id: "device-a", profile_id: "profile-a", enrollment: "active", enabled: true, target: { kind: "temp" },
  access_mode: "auto_review", model_mode: "hub", start_on_launch: true, keep_when_hidden: false, bind_ip: null, port: null };
const projection = () => ({ device_id: expected.device_id, enrollment: expected.enrollment,
  receiver: { ...expected, confirmed: true, status: "receiving", endpoint: "https://127.0.0.1:17332/mcp" } });
const form = () => Object.fromEntries(["target", "access_mode", "model_mode", "start_on_launch", "keep_when_hidden", "bind_ip", "port"].map(key => [key, {
  count: 1, value: key === "target" ? "temp" : String(expected[key] ?? ""), checked: expected[key],
}]));

test("receiver settings scenario reuses existing Desktop and Hub lifecycles without a new launcher", () => {
  const scenario = createHubReceiverSettingsScenario();
  assert.equal(scenario.id, "hub.receiver-settings-controls");
  assert.equal(scenario.productOracle, "pass");
  assert.equal(scenario.manualGate, "not_required");
  assert.equal(scenario.databaseRequired, true);
  for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) assert.equal(typeof scenario[method], "function");
  assert.equal("launch" in scenario, false);
  assert.throws(() => createHubReceiverSettingsScenario({ ignoreHTTPSErrors: true }));
});

test("saved receiver settings require same device, authority, address, flags and actual receiving state", () => {
  assert.equal(receiverSettingsMatch(projection(), expected), true);
  for (const mutate of [
    p => { p.device_id = "other"; }, p => { p.enrollment = "pending"; },
    p => { p.receiver.enabled = false; }, p => { p.receiver.confirmed = false; }, p => { p.receiver.profile_id = "other"; },
    p => { p.receiver.status = "starting"; }, p => { p.receiver.endpoint = null; },
    p => { p.receiver.target = { kind: "project", project_id: "other" }; },
    p => { p.receiver.access_mode = "full_access"; }, p => { p.receiver.model_mode = "direct"; },
    p => { p.receiver.start_on_launch = false; }, p => { p.receiver.keep_when_hidden = true; },
    p => { p.receiver.bind_ip = "127.0.0.1"; }, p => { p.receiver.port = 7332; },
  ]) { const p = projection(); mutate(p); assert.equal(receiverSettingsMatch(p, expected), false); }
});

test("disconnect and reconnect acceptance retain configured start-on-launch while refusing accidental reception", () => {
  const p = projection(); p.enrollment = "disconnected"; p.receiver.enabled = false; p.receiver.status = "stopped"; p.receiver.endpoint = null;
  assert.equal(receiverSettingsMatch(p, { ...expected, enrollment: "disconnected", enabled: false }), true);
  p.enrollment = "active";
  assert.equal(receiverSettingsMatch(p, { ...expected, enabled: false }), true);
  p.receiver.status = "unknown";
  assert.equal(receiverSettingsMatch(p, { ...expected, enabled: false }), false);
  p.receiver.enabled = true; p.receiver.status = "receiving";
  assert.equal(receiverSettingsMatch(p, { ...expected, enabled: false }), false);
});

test("reopened form checks each concrete value and cardinality including hidden disclosure fields", () => {
  assert.equal(receiverFormMatch(form(), expected), true);
  for (const key of Object.keys(form())) {
    const duplicate = form(); duplicate[key].count = 2;
    assert.equal(receiverFormMatch(duplicate, expected), false);
    const changed = form();
    if (typeof expected[key] === "boolean") changed[key].checked = !expected[key]; else changed[key].value = "different";
    assert.equal(receiverFormMatch(changed, expected), false);
  }
  const fixed = { ...expected, target: { kind: "project", project_id: "selected", workspace_root: "C:/isolated" }, bind_ip: "127.0.0.1", port: 17001 };
  const displayed = form(); displayed.target.value = "project:selected"; displayed.bind_ip.value = "127.0.0.1"; displayed.port.value = "17001";
  assert.equal(receiverFormMatch(displayed, fixed), true);
});

test("invalid draft cannot pass through disabled-only or silently dispatched mutation", () => {
  const good = { count: 1, save_disabled: true, error: "入力エラー", commands: { calls: [], dropped_through: 0 } };
  assert.equal(invalidReceiverDraftMatch(good), true);
  for (const changed of [{ count: 0 }, { count: 2 }, { save_disabled: false }, { error: "" },
    { commands: { calls: [{ command: "device_network_receiver" }], dropped_through: 0 } },
    { commands: { calls: [], dropped_through: 1 } }]) assert.equal(invalidReceiverDraftMatch({ ...good, ...changed }), false);
});

test("visible successful controls do not hide an unhandled Hub page error", () => {
  assert.deepEqual(receiverSettingsOutcome([]), { acquisition: "pass", oracle: "pass", manual: "not_required" });
  assert.throws(() => receiverSettingsOutcome(["page failed"]), error => error.code === "hub-receiver-settings-mismatch");
});

import assert from "node:assert/strict";
import test from "node:test";
import { createScenario } from "../scenario_registry.mjs";
import { createSharedWorkIsolationScenario, isolatedDevicesAccepted, personalProjectAccepted, isolatedAccessRemovalAccepted, sharedMainFillsShell } from "../scenarios/shared_work_isolation.mjs";

const expected = { user_id: "bob", display_name: "Bob", project_id: "project-b", project_label: "Bob project" };
function personal() {
  return { desktop: { hub_project_open: true, overlay: "none", busy: false },
    shared: { connected: true, principal: { user_id: "bob", administrator: false }, projects: [{ id: "project-b" }], selected_project_id: "project-b", status: { jobs: [] }, observed_at_ms: 101 },
    visible_project_ids: ["project-b"], person_text: "Bob · project", project_heading: "Bob project", login_visible: false };
}
function accessRemoved() {
  return { desktop: { hub_project_open: true, overlay: "none", busy: false },
    shared: { connected: true, principal: { user_id: "alice" }, projects: [], status: null, detail: null }, visible_project_ids: [], login_visible: false };
}

test("Hub main geometry rejects a vacant third column and overflowing or missing surfaces", () => {
  const value = { shell: { left: 0, right: 1440, height: 800 }, sidebar: { left: 0, right: 260, height: 800 }, main: { left: 260, right: 1440, height: 800 } };
  assert.equal(sharedMainFillsShell(value), true);
  assert.equal(sharedMainFillsShell({ ...value, main: { ...value.main, right: 1439.5 } }), true);
  for (const main of [{ ...value.main, right: 1120 }, { ...value.main, right: 1442 }, { ...value.main, left: 300 }, { ...value.main, height: 0 }, null]) {
    assert.equal(sharedMainFillsShell({ ...value, main }), false);
  }
  assert.equal(sharedMainFillsShell(), false);
});

test("two simultaneous desktops use the common explicit-isolation lifecycle", async () => {
  const scenario = createScenario("settings.shared-work-isolation", { hubBinary: "C:/fixture/moyai-hub.exe", headed: false });
  for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) assert.equal(typeof scenario[method], "function");
  assert.equal(scenario.databaseRequired, true);
  assert.equal("launch" in scenario, false);
  await assert.rejects(scenario.prepare({ context: { desktopIsolation: "user-wide" } }), error => error.code === "fixture-isolation-required");
  assert.throws(() => createSharedWorkIsolationScenario({ desktopRoot: "C:/unowned" }));
});

test("simultaneous identity proof rejects sharing a process, certificate or underlying key", () => {
  const a = { process_id: 10, network: { enrollment: "active", device_id: "device-a" }, key_sha256: "a".repeat(64), certificate_sha256: "b".repeat(64) };
  const b = { process_id: 20, network: { enrollment: "active", device_id: "device-b" }, key_sha256: "c".repeat(64), certificate_sha256: "d".repeat(64) };
  assert.equal(isolatedDevicesAccepted(a, b), true);
  for (const changed of [{ ...b, process_id: a.process_id }, { ...b, network: a.network }, { ...b, key_sha256: a.key_sha256.toUpperCase() },
    { ...b, certificate_sha256: a.certificate_sha256 }, { ...b, key_sha256: undefined }, { ...b, network: { ...b.network, enrollment: "pending" } }]) {
    assert.equal(isolatedDevicesAccepted(a, changed), false);
  }
});

test("person isolation requires exact membership, selected project and visible sidebar", () => {
  assert.equal(personalProjectAccepted(personal(), expected), true);
  for (const mutate of [
    value => { value.shared.principal.user_id = "alice"; },
    value => { value.shared.principal.administrator = true; },
    value => { value.shared.projects.push({ id: "project-a" }); },
    value => { value.visible_project_ids.push("project-a"); },
    value => { value.person_text = "Alice · project"; },
    value => { value.project_heading = "Alice project"; },
    value => { value.shared.selected_project_id = "project-a"; },
    value => { value.shared.status = null; },
    value => { value.shared.connected = false; },
    value => { value.desktop.overlay = "hub"; },
    value => { value.login_visible = true; },
  ]) {
    const value = personal(); mutate(value); assert.equal(personalProjectAccepted(value, expected), false);
  }
});

test("project access removal needs a fresh unaffected B observation", () => {
  const value = { a: accessRemoved(), b: personal() };
  assert.equal(isolatedAccessRemovalAccepted(value, expected, 100), true);
  assert.equal(isolatedAccessRemovalAccepted(value, expected, 101), false);
  assert.equal(isolatedAccessRemovalAccepted(value, expected, 102), false);
  for (const mutate of [
    next => { next.a.shared.principal = null; },
    next => { next.a.shared.projects = [{ id: "project-a" }]; },
    next => { next.a.shared.status = { jobs: [] }; },
    next => { next.a.shared.detail = { id: "old-job" }; },
    next => { next.a.visible_project_ids = ["project-a"]; },
    next => { next.a.login_visible = true; },
    next => { next.b.shared.principal = null; },
    next => { next.b.shared.observed_at_ms = undefined; },
  ]) {
    const next = structuredClone(value); mutate(next); assert.equal(isolatedAccessRemovalAccepted(next, expected, 100), false);
  }
});

import assert from "node:assert/strict";
import test from "node:test";
import { sharedEntryReady, sharedSettingsClosed } from "../scenarios/shared_work_entry.mjs";
import { rememberedRestartAccepted, sharedActionTarget } from "../scenarios/shared_work_navigation.mjs";
import { action, hubSettingsCloseTarget } from "../scenarios/hub_browser_enrollment.mjs";
test("Hub projects occupy main while local model setup can remain unfinished", () => {
  const projection = { hub_project_open: true, overlay: "none", startup: { initial_setup_required: true }, busy: false };
  assert.equal(sharedEntryReady(projection), true);
  assert.equal(sharedEntryReady({ ...projection, startup: { initial_setup_required: false } }), false);
  assert.equal(sharedEntryReady({ ...projection, overlay: "hub" }), false);
  assert.equal(sharedEntryReady({ ...projection, hub_project_open: false }), false);
  assert.equal(sharedEntryReady({ ...projection, busy: true }), false);
});

test("closing settings preserves the current Hub-config readiness after restart", () => {
  const ready = { hub_project_open: true, overlay: "none", busy: false, startup: { status: "ready", initial_setup_required: false } };
  assert.equal(sharedSettingsClosed(ready, ready), true);
  assert.equal(sharedSettingsClosed({ ...ready, overlay: "hub" }, ready), false);
  assert.equal(sharedSettingsClosed({ ...ready, hub_project_open: false }, ready), false);
  assert.equal(sharedSettingsClosed({ ...ready, busy: true }, ready), false);
  const initial = { ...ready, startup: { status: "requires_config", initial_setup_required: true } };
  assert.equal(sharedSettingsClosed(initial, initial), true);
  assert.equal(sharedSettingsClosed(initial, ready), false);
  assert.equal(sharedSettingsClosed(ready, initial), false);
  assert.equal(sharedSettingsClosed({ ...ready, startup: null }, ready), false);
});
test("restart oracle needs the exact person/project/job without an interactive login", () => {
  const expected = { user_id: "alice", project_id: "analysis", job_id: "job-1" };
  const value = { desktop: { hub_project_open: true, overlay: "none", busy: false }, shared: { connected: true, principal: { user_id: "alice" }, selected_project_id: "analysis", status: { jobs: [{ id: "job-1" }] } }, login_visible: false, calls: [{ command: "shared_work_command", args: { request: { kind: "refresh" } } }] };
  assert.equal(rememberedRestartAccepted(value, expected), true);
  assert.equal(rememberedRestartAccepted({ ...value, login_visible: true }, expected), false);
  assert.equal(rememberedRestartAccepted({ ...value, calls: [{ command: "shared_work_command", args: { request: { kind: "login" } } }] }, expected), false);
  for (const shared of [{ ...value.shared, principal: { user_id: "bob" } }, { ...value.shared, selected_project_id: "other" }, { ...value.shared, status: { jobs: [] } }, { ...value.shared, connected: false }]) {
    assert.equal(rememberedRestartAccepted({ ...value, shared }, expected), false);
  }
});
test("job navigation uses the sidebar while explicit job mutation uses the conversation", () => {
  assert.match(sharedActionTarget("detail", "job-1").selector, /^\.sidebar /);
  assert.match(sharedActionTarget("cancel", "job-1").selector, /^\.shared-work /);
  assert.deepEqual(sharedActionTarget("approve", "job-1").identity, { tag: "BUTTON", action: "shared-approve" });
});

test("connection settings close uses the footer when header and footer expose the same action", () => {
  assert.equal(hubSettingsCloseTarget.selector, action("close-overlay", '[role="dialog"][data-modal="hub"] .hub-modal-footer').selector);
  assert.notEqual(hubSettingsCloseTarget.selector, action("close-overlay").selector);
  assert.deepEqual(hubSettingsCloseTarget.identity, { tag: "BUTTON", action: "close-overlay" });
});

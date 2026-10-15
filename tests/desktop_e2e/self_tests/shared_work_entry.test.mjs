import assert from "node:assert/strict";
import test from "node:test";
import { sharedEntryReady, sharedSettingsClosed, sharedApprovalParked, sharedRenameEditorRetained } from "../scenarios/shared_work_entry.mjs";
import { rememberedRestartAccepted, sharedActionTarget, sharedWorkSurfaceMatches, waitForSharedComposer } from "../scenarios/shared_work_navigation.mjs";
import { action, hubSettingsCloseTarget } from "../scenarios/hub_browser_enrollment.mjs";

test("a shared action waits through project-switch pending before dispatch becomes eligible", async () => {
  const target = sharedActionTarget("new-conversation");
  const ready = { count: 1, visible: true, enabled: true, center_hit: true };
  const frames = [ready, { ...ready, enabled: false }, ready, ready, ready];
  let samples = 0;
  const input = {
    observeExactTarget: async actual => {
      assert.equal(actual, target);
      return { observation: frames[Math.min(samples++, frames.length - 1)] };
    },
  };
  await waitForSharedComposer(input, target);
  assert.equal(samples, 5);
});

const parkedTarget = { jobId: "job-approval", approvalId: "permission-old", attemptId: "attempt-1", environmentId: "env-b" };
const parkedProjection = () => ({ observed_at_ms: 60_100, detail: { id: "job-approval", state: "running" },
  approval: { id: "permission-old", attempt_id: "attempt-1", context: { job_id: "job-approval" }, status: "expired", decision: null,
    expires_at_ms: 60_000, can_decide: false, can_reconfirm: true }, status: { environments: [{ id: "env-b", occupied: 1 }] } });

test("rename retention requires the original connected editor, focus, draft and exact selection", () => {
  const selected = { connected: true, same_node: true, focused: true, value: "名前変更の入力保持を確認", selection_start: 0, selection_end: 12, selection_direction: "forward" };
  assert.equal(sharedRenameEditorRetained(selected, selected), true);
  const pointer = { ...selected, selection_start: 5, selection_end: 5, selection_direction: "none" };
  assert.equal(sharedRenameEditorRetained(pointer, pointer), true);
  for (const patch of [{ connected: false }, { same_node: false }, { focused: false }, { value: "元の名前" },
    { selection_start: 1 }, { selection_end: 11 }, { selection_direction: "backward" }])
    assert.equal(sharedRenameEditorRetained({ ...selected, ...patch }, selected), false, JSON.stringify(patch));
  assert.equal(sharedRenameEditorRetained(null, selected), false);
});

test("permission expiry oracle requires the exact operation to stay running and occupy its environment", () => {
  assert.equal(sharedApprovalParked(parkedProjection(), parkedTarget), true);
  for (const patch of [{ state: "cancelled" }, { state: "succeeded" }, { id: "other-job" }]) {
    const p = parkedProjection(); Object.assign(p.detail, patch);
    assert.equal(sharedApprovalParked(p, parkedTarget), false);
  }
  for (const environments of [[], [{ id: "other-env", occupied: 1 }], [{ id: "env-b", occupied: 0 }]]) {
    const p = parkedProjection(); p.status.environments = environments;
    assert.equal(sharedApprovalParked(p, parkedTarget), false);
  }
});

test("permission expiry oracle rejects a still-valid, consumed, replaced, or actionable approval", () => {
  for (const patch of [{ id: "permission-new" }, { attempt_id: "attempt-2" }, { context: { job_id: "other-job" } },
    { status: "pending" }, { status: "consumed" }, { decision: "stop" }, { decision: "approve" },
    { can_decide: true }, { can_reconfirm: false }, { expires_at_ms: 70_000 }, { expires_at_ms: null }]) {
    const p = parkedProjection(); Object.assign(p.approval, patch);
    assert.equal(sharedApprovalParked(p, parkedTarget), false, JSON.stringify(patch));
  }
  assert.equal(sharedApprovalParked(null, parkedTarget), false);
});

test("reconfirm targets the exact expired approval inside the shared conversation", () => {
  const target = sharedActionTarget("reconfirm-approval", "permission-old");
  assert.match(target.selector, /^\.shared-work /);
  assert.match(target.selector, /permission-old/);
  assert.deepEqual(target.identity, { tag: "BUTTON", action: "shared-reconfirm-approval" });
});

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
  const value = { desktop: { hub_project_open: true, overlay: "none", busy: false }, shared: { connected: true, principal: { user_id: "alice", display_name: "Alice" }, projects: [{ id: "analysis", label: "Analysis" }], selected_project_id: "analysis", status: { jobs: [{ id: "job-1" }] } }, surface: { count: 1, splash_visible: false, heading: "Analysis", account_text: "Alice · Analysis", login_visible: false, login_enabled: false }, calls: [{ command: "shared_work_command", args: { request: { kind: "refresh" } } }] };
  assert.equal(rememberedRestartAccepted(value, expected), true);
  for (const patch of [{ count: 0 }, { splash_visible: true }, { login_visible: true }, { heading: "Other project" }, { account_text: "Bob" }]) {
    assert.equal(rememberedRestartAccepted({ ...value, surface: { ...value.surface, ...patch } }, expected), false);
  }
  assert.equal(rememberedRestartAccepted({ ...value, calls: [{ command: "shared_work_command", args: { request: { kind: "login" } } }] }, expected), false);
  for (const shared of [{ ...value.shared, principal: { user_id: "bob" } }, { ...value.shared, selected_project_id: "other" }, { ...value.shared, status: { jobs: [] } }, { ...value.shared, connected: false }]) {
    assert.equal(rememberedRestartAccepted({ ...value, shared }, expected), false);
  }
});

test("unavailable device access shows the connection step, never a login form", () => {
  const surface = { count: 1, splash_visible: false, login_visible: false, connection_visible: true };
  assert.equal(sharedWorkSurfaceMatches(surface, { principal: null }), true);
  for (const patch of [{ count: 0 }, { splash_visible: true }, { login_visible: true }, { connection_visible: false }]) {
    assert.equal(sharedWorkSurfaceMatches({ ...surface, ...patch }, { principal: null }), false);
  }
});
test("a job's detail and mutation controls remain inside its shared conversation", () => {
  assert.match(sharedActionTarget("detail", "job-1").selector, /^\.shared-work /);
  assert.match(sharedActionTarget("cancel", "job-1").selector, /^\.shared-work /);
  assert.deepEqual(sharedActionTarget("approve", "job-1").identity, { tag: "BUTTON", action: "shared-approve" });
});

test("connection settings close uses the footer when header and footer expose the same action", () => {
  assert.equal(hubSettingsCloseTarget.selector, action("close-overlay", '[role="dialog"][data-modal="hub"] .hub-modal-footer').selector);
  assert.notEqual(hubSettingsCloseTarget.selector, action("close-overlay").selector);
  assert.deepEqual(hubSettingsCloseTarget.identity, { tag: "BUTTON", action: "close-overlay" });
});

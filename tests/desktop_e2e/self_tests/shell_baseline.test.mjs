import assert from "node:assert/strict";
import test from "node:test";

import { shellReadinessAccepted, shellReadinessFailures } from "../scenarios/shell_baseline.mjs";

function ready(overrides = {}) {
  return {
    title: "moyAI",
    ready_state: "complete",
    body_connected: true,
    app_count: 1,
    app_connected: true,
    splash_count: 0,
    app_frame_count: 1,
    shell_count: 1,
    conversation_count: 1,
    composer_count: 1,
    shell_inert: false,
    visible_blocking_overlay_count: 0,
    visible_modal_backdrop_count: 0,
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
    prompt_count: 1,
    prompt_visible: true,
    prompt_enabled: true,
    prompt_center_hit: true,
    tauri_invoke_available: true,
    desktop_state_ok: true,
    projection_revision: "42",
    workspace_path: "C:\\fixture",
    startup_status: "ready",
    initial_setup_required: false,
    startup_action_overlay: "none",
    startup_fail_check_count: 0,
    projection_overlay: "none",
    confirmation_visible: false,
    confirmation_id: null,
    confirmation_present: false,
    run_status_key: "idle",
    task_activity_state: "idle",
    agent_tree_active: false,
    post_run_refresh_pending: false,
    provider_loading: false,
    composer_submit_mode: "new_request",
    can_submit: true,
    navigation_admission_open: true,
    navigation_loading: false,
    busy: false,
    background_mutation_pending: false,
    async_polling_required: false,
    pending_async_operation_count: 0,
    visibility_state: "visible",
    document_hidden: false,
    ...overrides,
  };
}

test("shell readiness requires an interactive canary and a settled Rust projection", () => {
  assert.equal(shellReadinessAccepted(ready(), { afterRevision: "41", expectedWorkspace: "C:\\fixture" }), true);
  assert.equal(shellReadinessFailures(ready({ projection_revision: "41" }), { afterRevision: "41" }).includes("ordinary-projection-not-fresh"), true);
  assert.equal(shellReadinessFailures(ready({ workspace_path: "C:\\other" }), { expectedWorkspace: "C:\\fixture" }).includes("workspace-owner-mismatch"), true);
  for (const [field, value, reason] of [
    ["splash_count", 1, "startup-splash-still-visible"],
    ["shell_inert", true, "interactive-shell-inert"],
    ["visible_blocking_overlay_count", 1, "blocking-overlay-visible"],
    ["visible_fatal_count", 1, "fatal-error-visible"],
    ["visible_recoverable_error_count", 1, "recoverable-error-visible"],
    ["prompt_visible", false, "main-prompt-not-interactable"],
    ["prompt_enabled", false, "main-prompt-not-interactable"],
    ["prompt_center_hit", false, "main-prompt-hit-test-failed"],
    ["desktop_state_ok", false, "desktop-state-command-unavailable"],
    ["async_polling_required", true, "projection-not-settled"],
    ["pending_async_operation_count", 1, "projection-not-settled"],
    ["startup_status", "requires_config", "startup-not-ready"],
    ["projection_overlay", "config", "projection-overlay-open"],
    ["task_activity_state", "running", "task-owner-not-idle"],
    ["composer_submit_mode", "blocked", "composer-admission-closed"],
  ]) {
    const failures = shellReadinessFailures(ready({ [field]: value }));
    assert.equal(failures.includes(reason), true, `${field} must fail as ${reason}`);
  }
});

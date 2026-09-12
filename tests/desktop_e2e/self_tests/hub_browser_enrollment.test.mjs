import assert from "node:assert/strict";
import test from "node:test";
import { normalizeHubBrowserOptions } from "../drivers/hub_browser_resource.mjs";
import { createHubBrowserEnrollmentScenario, enrollmentAccepted, independentSelectionsAccepted, connectedShellAccepted } from "../scenarios/hub_browser_enrollment.mjs";

test("combined browser enrollment uses the existing Desktop lifecycle and explicit isolated browser options", () => {
  const scenario = createHubBrowserEnrollmentScenario();
  assert.equal(scenario.id, "hub.browser-enrollment");
  for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) assert.equal(typeof scenario[method], "function");
  assert.equal("launch" in scenario, false);
  assert.equal(normalizeHubBrowserOptions().headed, true);
  for (const options of [{ hubBinary: "relative.exe" }, { hubRepository: "relative" }, { browserChannel: "unknown" },
    { headed: "false" }, { ignoreHTTPSErrors: true }]) assert.throws(() => normalizeHubBrowserOptions(options));
});

test("enrollment acceptance requires the same actual device at both endpoints", () => {
  const snapshot = { devices: [{ device_id: "same-device" }] };
  assert.equal(enrollmentAccepted({ enrollment: "active", device_id: "same-device" }, snapshot), true);
  for (const network of [{ enrollment: "pending", device_id: "same-device" }, { enrollment: "active", device_id: "other-device" }, {}]) {
    assert.equal(enrollmentAccepted(network, snapshot), false);
  }
  assert.equal(enrollmentAccepted({ enrollment: "active", device_id: "same-device" }, { devices: [...snapshot.devices, ...snapshot.devices] }), false);
});

test("independent model acceptance rejects route mismatch, stale review and leaked other-context selection", () => {
  const hub = { status: "connected", main_confirmation: "confirmed", side_chat_confirmation: "confirmed", main_mode: "hub", side_chat_mode: "hub",
    main_review: { selection: { preferred_model_id: "main", allowed_model_ids: ["main"] } },
    side_chat_review: { selection: { preferred_model_id: "side", allowed_model_ids: ["side"] } } };
  assert.equal(independentSelectionsAccepted(hub, "main", "side"), true);
  assert.equal(independentSelectionsAccepted({ ...hub, side_chat_mode: "direct" }, "main", "side"), false);
  assert.equal(independentSelectionsAccepted({ ...hub, main_confirmation: "review_required" }, "main", "side"), false);
  assert.equal(independentSelectionsAccepted({ ...hub, side_chat_review: hub.main_review }, "main", "side"), false);
  const additional = structuredClone(hub); additional.main_review.selection.allowed_model_ids.push("side");
  assert.equal(independentSelectionsAccepted(additional, "main", "side"), false);
});

test("enrolled idle shell permits heartbeat polling while rejecting jobs, dialogs and stale routes", () => {
  const value = { prompt_enabled: true, prompt_center_hit: true, blocking_dialogs: 0,
    state: { async_polling_required: true, device_network: { enrollment: "active" },
      hub: { status: "connected", main_confirmation: "confirmed", side_chat_confirmation: "confirmed", main_mode: "hub", side_chat_mode: "hub",
        main_review: { selection: { preferred_model_id: "main", allowed_model_ids: ["main"] } },
        side_chat_review: { selection: { preferred_model_id: "side", allowed_model_ids: ["side"] } } },
      overlay: "none", run_status_key: "idle", busy: false, provider_loading: false,
      confirmation_visible: false, background_mutation_pending: false, pending_async_operations: [],
      navigation_loading: false, navigation_admission_open: true, can_submit: true,
    } };
  assert.equal(connectedShellAccepted(value, "main", "side"), true);
  for (const mutate of [
    next => { next.state.pending_async_operations = ["remote_job"]; },
    next => { next.state.device_network.enrollment = "pending"; },
    next => { next.state.run_status_key = "running"; },
    next => { next.state.busy = true; },
    next => { next.state.hub.main_mode = "direct"; },
    next => { next.blocking_dialogs = 1; },
    next => { next.prompt_center_hit = false; },
  ]) {
    const next = structuredClone(value); mutate(next);
    assert.equal(connectedShellAccepted(next, "main", "side"), false);
  }
});

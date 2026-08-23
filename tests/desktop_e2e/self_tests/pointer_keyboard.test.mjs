import assert from "node:assert/strict";
import test from "node:test";

import { DesktopE2eError } from "../core/execution.mjs";
import {
  PointerKeyboardCleanupOwner,
  assertTrustedTabNavigation,
  composerQualificationFailures,
  shortcutsDialogReady,
} from "../scenarios/pointer_keyboard.mjs";

function composerSurface(overrides = {}) {
  return {
    projection: {
      overlay: "none",
      draft_prompt: "",
      draft_target: { workspace_path: "C:\\workspace", composer_generation: "4" },
      run_target: { kind: "idle", admission_revision: "9" },
      composer_commit_generation: "4",
      workspace_path: "C:\\workspace",
      selected_project_index: 0,
      selected_session_index: 0,
      project_rows: [{ project_id: "project-1", path: "C:\\workspace" }],
      session_rows: [{ session_id: "session-1" }],
    },
    prompt: {
      count: 1,
      value: "Unicode入力 😀\n複数行",
      active: true,
      visible: true,
      enabled: true,
      selection_start: 16,
      selection_end: 16,
    },
    fatal_count: 0,
    recoverable_error_count: 0,
    ...overrides,
  };
}

function composerExpected(surface) {
  return {
    active: true,
    domValue: surface.prompt.value,
    selectionOffset: surface.prompt.selection_end,
    draftTarget: surface.projection.draft_target,
    runTarget: surface.projection.run_target,
    composerCommitGeneration: surface.projection.composer_commit_generation,
    navigationIdentity: {
      workspace_path: "C:\\workspace",
      project_id: "project-1",
      project_path: "C:\\workspace",
      session_id: "session-1",
      project_row_ids: ["project-1"],
      session_row_ids: ["session-1"],
    },
  };
}

function dialogObservation(overrides = {}) {
  return {
    projection_overlay: "shortcuts",
    dialog_count: 1,
    dialog_connected: true,
    dialog_visible: true,
    active: { tag: "BUTTON", action: "close-overlay" },
    trigger_count: 1,
    fatal_count: 0,
    recoverable_error_count: 0,
    ...overrides,
  };
}

test("shortcuts readiness requires projection, DOM, exact trigger, focus, and clean errors together", () => {
  assert.equal(shortcutsDialogReady(dialogObservation()), true);
  for (const observation of [
    dialogObservation({ projection_overlay: "none" }),
    dialogObservation({ dialog_count: 0 }),
    dialogObservation({ dialog_visible: false }),
    dialogObservation({ active: { tag: "SECTION", action: null } }),
    dialogObservation({ trigger_count: 2 }),
    dialogObservation({ recoverable_error_count: 1 }),
  ]) {
    assert.equal(shortcutsDialogReady(observation), false);
  }
});

test("Tab acquisition failures remain harness-owned while acquired focus drift is product-owned", () => {
  assert.throws(
    () => assertTrustedTabNavigation(
      { classification: "harness_ng", transition: null, reasons: ["tab-event-not-trusted"] },
      { active: { tag: "BUTTON", action: "refresh" } },
    ),
    (error) => error instanceof DesktopE2eError
      && error.owner === "harness"
      && error.code === "trusted-tab-acquisition-failed",
  );
  assert.throws(
    () => assertTrustedTabNavigation(
      { classification: "acquired", transition: "in_document", reasons: [] },
      { active: { tag: "BUTTON", action: "show-shortcuts" } },
    ),
    (error) => error instanceof DesktopE2eError
      && error.owner === "product"
      && error.code === "trusted-tab-navigation-failed",
  );
  assert.equal(assertTrustedTabNavigation(
    { classification: "acquired", transition: "in_document", reasons: [] },
    { active: { tag: "BUTTON", action: "refresh" } },
  ).transition, "in_document");
});

test("composer qualification binds exact local text to an unchanged Rust owner projection", () => {
  const valid = composerSurface();
  const expected = composerExpected(valid);
  assert.deepEqual(composerQualificationFailures(valid, expected), []);

  const domDrift = composerSurface({ prompt: { ...valid.prompt, value: "different" } });
  assert.ok(composerQualificationFailures(domDrift, expected).includes("composer-dom-value-drift"));

  const projectedDraftDrift = composerSurface({
    projection: { ...valid.projection, draft_prompt: valid.prompt.value },
  });
  assert.ok(composerQualificationFailures(projectedDraftDrift, expected).includes("composer-projected-draft-drift"));

  const ownerDrift = composerSurface({
    projection: { ...valid.projection, draft_target: { workspace_path: "C:\\other", composer_generation: "5" } },
  });
  assert.ok(composerQualificationFailures(ownerDrift, expected).includes("composer-draft-target-drift"));
});

test("cleanup failure preserves a primary product failure and reports scenario cleanup failure", async () => {
  const owner = new PointerKeyboardCleanupOwner();
  const primary = new DesktopE2eError("product", "observed-product-failure", "product predicate failed");
  const input = { cleanup: async () => { throw new Error("injected input cleanup failure"); } };
  const cdp = { evaluate: async () => ({ had_reference: true, cleared: true }) };

  await owner.settle(input, cdp, primary);
  const outcome = owner.outcome;
  assert.equal(outcome.input, "fail");
  assert.equal(outcome.resources[0].failure.owner, "harness");
  assert.equal(outcome.resources[0].primary_failure.owner, "product");
  assert.equal(outcome.resources[0].primary_failure.code, "observed-product-failure");
});

test("cleanup failure without a primary error is surfaced immediately as harness failure", async () => {
  const owner = new PointerKeyboardCleanupOwner();
  const input = { cleanup: async () => ({ released: true }) };
  const cdp = { evaluate: async () => ({ had_reference: true, cleared: false }) };

  await assert.rejects(
    owner.settle(input, cdp),
    (error) => error instanceof DesktopE2eError
      && error.owner === "harness"
      && error.code === "pointer-keyboard-resource-cleanup-failed",
  );
  assert.equal(owner.outcome.input, "fail");
});

import assert from "node:assert/strict";
import test from "node:test";

import {
  isRegularModalOverlay,
  modalIsOpen,
  overlayPrimaryFocusSelectors,
} from "../src/modal_state.ts";

test("every current Desktop overlay has the intended modal containment and focus-entry policy", () => {
  // Public wire values of Rust DesktopOverlay, including menu-only and startup surfaces.
  const cases = [
    ["none", false, false],
    ["initial_setup", false, true],
    ["file_menu", false, false],
    ["edit_menu", false, false],
    ["view_menu", false, false],
    ["help_menu", false, false],
    ["project_menu", false, false],
    ["config", true, true],
    ["hub", true, true],
    ["mcp_history", true, true],
    ["session_settings", true, true],
    ["provider", true, true],
    ["workspace", true, true],
    ["prompt_review", true, true],
    ["command_palette", true, true],
    ["shortcuts", true, true],
    ["about", true, true],
  ] as const;
  for (const [overlay, dismissible, modal] of cases) {
    assert.equal(isRegularModalOverlay(overlay), dismissible, `${overlay}: regular modal lifecycle`);
    assert.equal(modalIsOpen({ confirmation_visible: false, overlay }, false), modal,
      `${overlay}: background inert and keyboard containment`);
    assert.equal(overlayPrimaryFocusSelectors(overlay).length > 0, modal, `${overlay}: focus entry`);
    assert.equal(modalIsOpen({ confirmation_visible: true, overlay }, false), true,
      `${overlay}: a permission decision takes modal precedence`);
    assert.equal(modalIsOpen({ confirmation_visible: false, overlay }, true), true,
      `${overlay}: local confirmation takes modal precedence`);
  }
  for (const overlay of ["mcp_publish", "unknown_overlay"]) {
    assert.equal(isRegularModalOverlay(overlay), false, `${overlay}: retired/unknown surface`);
    assert.equal(modalIsOpen({ confirmation_visible: false, overlay }, false), false);
    assert.deepEqual(overlayPrimaryFocusSelectors(overlay), []);
  }
});

test("initial setup blocks global shortcuts without becoming a dismissible regular modal", () => {
  assert.equal(isRegularModalOverlay("initial_setup"), false);
  assert.equal(modalIsOpen({ confirmation_visible: false, overlay: "initial_setup" }, false), true);
  assert.deepEqual(overlayPrimaryFocusSelectors("initial_setup"), [
    "#initial-setup-primary",
    ".initial-setup-shell .settings-control",
  ]);
});

test("session settings traps focus on the first enabled field and falls back to its dialog", () => {
  assert.equal(isRegularModalOverlay("session_settings"), true);
  assert.equal(modalIsOpen({ confirmation_visible: false, overlay: "session_settings" }, false), true);
  assert.deepEqual(overlayPrimaryFocusSelectors("session_settings"), [
    ".session-settings-control:not(:disabled):not([aria-disabled='true'])",
    ".session-settings-modal",
  ]);
});

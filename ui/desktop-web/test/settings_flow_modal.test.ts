import assert from "node:assert/strict";
import test from "node:test";

import {
  isRegularModalOverlay,
  modalIsOpen,
  overlayPrimaryFocusSelectors,
} from "../src/modal_state.ts";

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

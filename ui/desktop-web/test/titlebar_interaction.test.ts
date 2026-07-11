import assert from "node:assert/strict";
import test from "node:test";

import { TitlebarDragGesture, windowControlKeyboardActivation } from "../src/titlebar_interaction.ts";

function sample(overrides: Partial<Parameters<TitlebarDragGesture["pointerDown"]>[0]> = {}) {
  return {
    pointerId: 7,
    button: 0,
    buttons: 1,
    clientX: 20,
    clientY: 10,
    inDragRegion: true,
    inWindowControl: false,
    ...overrides,
  };
}

test("window controls remain independent hit targets", () => {
  const gesture = new TitlebarDragGesture();

  assert.equal(gesture.pointerDown(sample()), true);
  assert.equal(gesture.pointerDown(sample({ inWindowControl: true })), false);
  assert.equal(gesture.pointerMove(sample({ clientX: 40, inWindowControl: true })), false);
});

test("titlebar starts native drag only after pointer movement threshold", () => {
  const gesture = new TitlebarDragGesture(4);

  assert.equal(gesture.pointerDown(sample()), true);
  assert.equal(gesture.pointerMove(sample({ clientX: 22, clientY: 11 })), false);
  assert.equal(gesture.pointerMove(sample({ clientX: 25, clientY: 10 })), true);
  assert.equal(gesture.pointerMove(sample({ clientX: 30, clientY: 10 })), false);
});

test("titlebar clears a pending drag when the primary button is lost", () => {
  const gesture = new TitlebarDragGesture(4);

  assert.equal(gesture.pointerDown(sample()), true);
  assert.equal(gesture.pointerMove(sample({ buttons: 0, clientX: 30 })), false);
  assert.equal(gesture.pointerMove(sample({ buttons: 1, clientX: 40 })), false);
});

test("double-click toggles only on the drag region", () => {
  const gesture = new TitlebarDragGesture();

  assert.equal(gesture.doubleClick(sample()), true);
  assert.equal(gesture.doubleClick(sample({ inDragRegion: false })), false);
  assert.equal(gesture.doubleClick(sample({ inWindowControl: true })), false);
  assert.equal(gesture.doubleClick(sample({ button: 2 })), false);
});

test("window controls activate once from Enter or Space and ignore repeats", () => {
  assert.equal(windowControlKeyboardActivation("Enter", false), true);
  assert.equal(windowControlKeyboardActivation(" ", false), true);
  assert.equal(windowControlKeyboardActivation("Spacebar", false), true);
  assert.equal(windowControlKeyboardActivation("Enter", true), false);
  assert.equal(windowControlKeyboardActivation("Escape", false), false);
});

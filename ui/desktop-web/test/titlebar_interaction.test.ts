import assert from "node:assert/strict";
import test from "node:test";

import {
  applyTitlebarMenuRovingTabIndex,
  focusTitlebarMenuContinuation,
  TitlebarDragGesture,
  titlebarMenuFromOverlay,
  titlebarMenuKeyboardDecision,
  titlebarMenuPopupRole,
  titlebarMenuTabContinuationAction,
  titlebarMenuTriggerAction,
  titlebarMenuUsesRovingFocus,
  windowControlKeyboardActivation,
} from "../src/titlebar_interaction.ts";

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

test("window controls keep a below-threshold pointer click", () => {
  const gesture = new TitlebarDragGesture(4);

  assert.equal(gesture.pointerDown(sample({ inWindowControl: true })), false);
  assert.equal(gesture.pointerMove(sample({ clientX: 23, inWindowControl: true })), false);
  gesture.pointerUp(7);
  assert.equal(gesture.consumeWindowControlClickSuppression(true), false);
});

test("window controls suppress exactly one pointer click at or over the movement threshold", () => {
  for (const clientX of [24, 25]) {
    const gesture = new TitlebarDragGesture(4);

    assert.equal(gesture.pointerDown(sample({ inWindowControl: true })), false);
    assert.equal(gesture.pointerMove(sample({ clientX, inWindowControl: true })), false);
    gesture.pointerUp(7);
    assert.equal(gesture.consumeWindowControlClickSuppression(true), true);
    assert.equal(gesture.consumeWindowControlClickSuppression(true), false);
  }
});

test("window-control suppression ignores keyboard activation and survives pointerup until its click", () => {
  const gesture = new TitlebarDragGesture(4);

  gesture.pointerDown(sample({ inWindowControl: true }));
  gesture.pointerMove(sample({ clientX: 24, inWindowControl: true }));
  gesture.pointerUp(7);
  assert.equal(gesture.consumeWindowControlClickSuppression(false), false);
  assert.equal(gesture.consumeWindowControlClickSuppression(true), true);
});

test("lost primary buttons cannot arm a stale window-control suppression", () => {
  const gesture = new TitlebarDragGesture(4);

  gesture.pointerDown(sample({ inWindowControl: true }));
  assert.equal(gesture.pointerMove(sample({ buttons: 0, clientX: 30, inWindowControl: true })), false);
  assert.equal(gesture.pointerMove(sample({ buttons: 1, clientX: 40, inWindowControl: true })), false);
  assert.equal(gesture.consumeWindowControlClickSuppression(true), false);
});

test("a wrong pointer cannot arm or clear the tracked window-control transaction", () => {
  const gesture = new TitlebarDragGesture(4);

  gesture.pointerDown(sample({ inWindowControl: true }));
  assert.equal(
    gesture.pointerMove(sample({ pointerId: 8, clientX: 30, inWindowControl: true })),
    false,
  );
  gesture.pointerUp(8);
  assert.equal(gesture.consumeWindowControlClickSuppression(true), false);
  assert.equal(gesture.pointerMove(sample({ clientX: 24, inWindowControl: true })), false);
  assert.equal(gesture.consumeWindowControlClickSuppression(true), true);
});

test("the next pointerdown, cancellation, and double-click clear armed suppression", () => {
  const gesture = new TitlebarDragGesture(4);
  const arm = () => {
    gesture.pointerDown(sample({ inWindowControl: true }));
    gesture.pointerMove(sample({ clientX: 24, inWindowControl: true }));
  };

  arm();
  gesture.pointerDown(sample({ pointerId: 8, inDragRegion: false }));
  assert.equal(gesture.consumeWindowControlClickSuppression(true), false);

  arm();
  gesture.cancel();
  assert.equal(gesture.consumeWindowControlClickSuppression(true), false);

  arm();
  assert.equal(gesture.doubleClick(sample()), true);
  assert.equal(gesture.consumeWindowControlClickSuppression(true), false);
});

test("titlebar starts native drag only after pointer movement threshold", () => {
  const gesture = new TitlebarDragGesture(4);

  assert.equal(gesture.pointerDown(sample()), true);
  assert.equal(gesture.pointerMove(sample({ clientX: 22, clientY: 11 })), false);
  assert.equal(gesture.pointerMove(sample({ clientX: 24, clientY: 10 })), true);
  assert.equal(gesture.pointerMove(sample({ clientX: 30, clientY: 10 })), false);
  assert.equal(gesture.consumeWindowControlClickSuppression(true), false);
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

test("titlebar menu keyboard navigation wraps enabled actions and supports boundaries", () => {
  assert.deepEqual(titlebarMenuKeyboardDecision("ArrowDown", 0, 3, false), { kind: "move", index: 1 });
  assert.deepEqual(titlebarMenuKeyboardDecision("ArrowDown", 2, 3, false), { kind: "move", index: 0 });
  assert.deepEqual(titlebarMenuKeyboardDecision("ArrowUp", 0, 3, false), { kind: "move", index: 2 });
  assert.deepEqual(titlebarMenuKeyboardDecision("ArrowUp", -1, 3, false), { kind: "move", index: 2 });
  assert.deepEqual(titlebarMenuKeyboardDecision("Home", 2, 3, false), { kind: "move", index: 0 });
  assert.deepEqual(titlebarMenuKeyboardDecision("End", 0, 3, false), { kind: "move", index: 2 });
  assert.deepEqual(titlebarMenuKeyboardDecision("Escape", 1, 3, false), { kind: "close" });
});

test("menu Tab closes naturally while dialog and slider keys remain native", () => {
  assert.deepEqual(titlebarMenuKeyboardDecision("Tab", 0, 3, false), { kind: "close-natural" });
  for (const key of ["Enter", " "]) {
    assert.deepEqual(titlebarMenuKeyboardDecision(key, 0, 3, false), { kind: "native" });
  }
  for (const key of ["Tab", "ArrowDown", "ArrowUp", "Home", "End"]) {
    assert.deepEqual(titlebarMenuKeyboardDecision(key, -1, 3, true), { kind: "native" });
  }
  assert.deepEqual(titlebarMenuKeyboardDecision("Escape", -1, 3, true), { kind: "close" });
});

test("roving tabindex follows arrow focus and can be restored after a poll rerender", () => {
  const actions = [{ tabIndex: 0 }, { tabIndex: -1 }, { tabIndex: -1 }];

  applyTitlebarMenuRovingTabIndex(actions, 2);
  assert.deepEqual(actions.map((action) => action.tabIndex), [-1, -1, 0]);

  const rerendered = [{ tabIndex: 0 }, { tabIndex: -1 }, { tabIndex: -1 }];
  applyTitlebarMenuRovingTabIndex(rerendered, 2);
  assert.deepEqual(rerendered.map((action) => action.tabIndex), [-1, -1, 0]);
});

test("menu Tab continuation selects the next or previous titlebar control with wrapping", () => {
  const actions = [
    "show-file-menu",
    "show-edit-menu",
    "show-view-menu",
    "show-help-menu",
    "minimize-window",
    "toggle-maximize-window",
    "close-window",
  ];

  assert.equal(
    titlebarMenuTabContinuationAction(actions, "show-file-menu", false),
    "show-edit-menu",
  );
  assert.equal(
    titlebarMenuTabContinuationAction(actions, "show-file-menu", true),
    "close-window",
  );
  assert.equal(
    titlebarMenuTabContinuationAction(actions, "show-help-menu", false),
    "minimize-window",
  );
  assert.equal(titlebarMenuTabContinuationAction(actions, "missing", false), null);
});

test("async menu close continuation focuses its exact DOM action instead of BODY", () => {
  const body = { name: "body" };
  let activeElement: object = body;
  const targets = ["show-file-menu", "show-edit-menu", "close-window"].map((action) => ({
    dataset: { action },
    focus: () => {
      activeElement = targets.find((target) => target.dataset.action === action)!;
    },
  }));
  const titlebar = {
    querySelectorAll(selector: string) {
      assert.equal(
        selector,
        "button[data-action]:not(:disabled):not([aria-disabled='true'])",
      );
      return targets;
    },
  } as unknown as Pick<ParentNode, "querySelectorAll">;

  assert.equal(focusTitlebarMenuContinuation(titlebar, "show-edit-menu"), true);
  assert.equal(activeElement, targets[1]);
  assert.notEqual(activeElement, body);
  assert.equal(focusTitlebarMenuContinuation(titlebar, "missing"), false);
  assert.equal(activeElement, targets[1]);
});

test("titlebar overlay ownership maps to the exact trigger and mixed-widget popup role", () => {
  for (const menu of ["file", "edit", "help"] as const) {
    assert.equal(titlebarMenuFromOverlay(`${menu}_menu`), menu);
    assert.equal(titlebarMenuTriggerAction(`${menu}_menu`), `show-${menu}-menu`);
    assert.equal(titlebarMenuPopupRole(menu), "menu");
  }
  assert.equal(titlebarMenuUsesRovingFocus("menu"), true);
  assert.equal(titlebarMenuFromOverlay("view_menu"), "view");
  assert.equal(titlebarMenuTriggerAction("view_menu"), "show-view-menu");
  assert.equal(titlebarMenuPopupRole("view"), "dialog");
  assert.equal(titlebarMenuUsesRovingFocus("dialog"), false);
  assert.equal(titlebarMenuFromOverlay("provider"), null);
  assert.equal(titlebarMenuTriggerAction("none"), null);
});

import assert from "node:assert/strict";
import test from "node:test";
import { installGlobalKeyboardShortcuts } from "../src/events.ts";
import type { ActionContext } from "../src/actions.ts";
import type { DesktopViewState } from "../src/types.ts";
import { createUiLocalState } from "../src/ui_state.ts";

function withModalKeyboard(run: (fixture: {
  doc: DocumentFixture; context: ActionContext; state: DesktopViewState;
  press: (key: string, target: ElementFixture, modifiers?: Partial<KeyboardEvent>) => boolean;
}) => void): void {
  const doc = new DocumentFixture();
  const state = { overlay: "prompt_review", confirmation_visible: false } as DesktopViewState;
  const context = { uiState: createUiLocalState(), getViewState: () => state,
    getRenderModel: () => { assert.fail("native copying and blocked modal shortcuts must not dispatch product actions"); },
  } as unknown as ActionContext;
  const globals = { document: doc, Element: ElementFixture, HTMLElement: ElementFixture,
    HTMLInputElement: InputFixture, HTMLTextAreaElement: TextareaFixture };
  const previous = new Map(Object.keys(globals).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)]));
  try {
    for (const [key, value] of Object.entries(globals)) Object.defineProperty(globalThis, key, { configurable: true, value });
    installGlobalKeyboardShortcuts(context);
    run({ doc, context, state, press: (key, target, modifiers = {}) => {
      let prevented = false;
      doc.listener!({ key, target, keyCode: 0, isComposing: false, repeat: false, ctrlKey: true,
        metaKey: false, altKey: false, shiftKey: false, ...modifiers,
        preventDefault: () => { prevented = true; },
      } as unknown as KeyboardEvent);
      return prevented;
    } });
  } finally {
    for (const [key, descriptor] of previous) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else delete (globalThis as Record<string, unknown>)[key];
    }
  }
}

class ElementFixture {
  isConnected = true;
  readonly ownerDocument: DocumentFixture;
  readonly parent: ElementFixture | null;
  readonly role: string | null;
  constructor(ownerDocument: DocumentFixture, parent: ElementFixture | null = null, role: string | null = null) {
    this.ownerDocument = ownerDocument; this.parent = parent; this.role = role;
  }
  closest(selector: string): ElementFixture | null {
    if (selector.includes("contenteditable")) return null;
    if (selector === '[data-modal][role="dialog"]') return this.role === "dialog" ? this : this.parent?.closest(selector) ?? null;
    return null;
  }
  contains(node: unknown): boolean { return node === this || (node instanceof ElementFixture && this.contains(node.parent)); }
}
class InputFixture extends ElementFixture { disabled = false; readOnly = true; type = "text"; }
class TextareaFixture extends ElementFixture { disabled = false; readOnly = true; }
class DocumentFixture {
  readonly modal = new ElementFixture(this, null, "dialog");
  readonly raw = new ElementFixture(this, this.modal);
  readonly rawChild = new ElementFixture(this, this.raw);
  readonly outside = new ElementFixture(this);
  readonly readonlyInput = new InputFixture(this, this.modal);
  readonly readonlyTextarea = new TextareaFixture(this, this.modal);
  readonly selection = { rangeCount: 1, isCollapsed: false, anchorNode: this.rawChild, focusNode: this.raw };
  activeElement = this.modal;
  listener: ((event: KeyboardEvent) => void) | null = null;
  addEventListener(name: string, listener: (event: KeyboardEvent) => void) { assert.equal(name, "keydown"); this.listener = listener; }
  getSelection() { return this.selection; }
}

test("regular modal preserves native copy of selected raw/read-only text without product dispatch", () => {
  withModalKeyboard(({ doc, press }) => {
    for (const target of [doc.raw, doc.rawChild, doc.modal, doc.readonlyInput, doc.readonlyTextarea]) {
      assert.equal(press("c", target), false, "Ctrl+C must leave copying to the native WebView");
      assert.equal(press("C", target, { ctrlKey: false, metaKey: true }), false, "Cmd+C uses the same native selection");
    }
  });
});

test("native raw Copy exception requires selection within the current regular dialog", () => {
  withModalKeyboard(({ doc, state, context, press }) => {
    doc.selection.isCollapsed = true;
    assert.equal(press("c", doc.raw), true);
    doc.selection.isCollapsed = false;
    doc.selection.focusNode = doc.outside;
    assert.equal(press("c", doc.raw), true, "selection extending into the background is not the modal owner");
    doc.selection.focusNode = doc.raw;
    assert.equal(press("c", doc.outside), true, "a background event target is not the active modal");
    assert.equal(press("c", doc.raw, { altKey: true }), true);
    doc.readonlyInput.disabled = true;
    doc.readonlyTextarea.disabled = true;
    assert.equal(press("c", doc.readonlyInput), true);
    assert.equal(press("c", doc.readonlyTextarea), true);
    state.confirmation_visible = true;
    assert.equal(press("c", doc.raw), true, "permission decision handling is unchanged");
    state.confirmation_visible = false;
    context.uiState.pendingLocalConfirmation = {} as NonNullable<typeof context.uiState.pendingLocalConfirmation>;
    assert.equal(press("c", doc.raw), true, "a local confirmation supersedes the regular dialog");
  });
});

test("native raw copying does not allow Cut/Paste or background application shortcuts", () => {
  withModalKeyboard(({ doc, press }) => {
    for (const key of ["a", "x", "v", "n", "k", "Enter", "F8", "F9"]) {
      assert.equal(press(key, doc.raw, /^F/.test(key) ? { ctrlKey: false } : {}), true, key);
      assert.equal(press(key, doc.readonlyTextarea, /^F/.test(key) ? { ctrlKey: false } : {}), true, `readonly/${key}`);
    }
  });
});

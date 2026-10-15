import assert from "node:assert/strict";
import test from "node:test";
import { installGlobalKeyboardShortcuts } from "../src/events.ts";
import { createUiLocalState } from "../src/ui_state.ts";
import type { ActionContext } from "../src/actions.ts";
import type { DesktopViewState } from "../src/types.ts";

test("MCP history keyboard events remain in its dialog and never dispatch background shortcuts", () => {
  class ElementFixture {
    readonly isConnected = true;
    readonly hidden = false;
    readonly tagName = "BUTTON";
    readonly ownerDocument: DocumentFixture;
    constructor(ownerDocument: DocumentFixture) { this.ownerDocument = ownerDocument; }
    getAttribute(): string | null { return null; }
    closest(): null { return null; }
    matches(): boolean { return false; }
    getClientRects(): unknown[] { return [{}]; }
    focus(): void { this.ownerDocument.activeElement = this; }
    querySelectorAll(): ElementFixture[] { return this.ownerDocument.controls; }
  }
  class InputFixture extends ElementFixture {}
  class DocumentFixture {
    readonly defaultView = null;
    readonly body = new ElementFixture(this);
    readonly dialog = new ElementFixture(this);
    readonly controls = [new ElementFixture(this), new ElementFixture(this)];
    activeElement = this.body;
    listener: ((event: KeyboardEvent) => void) | null = null;
    addEventListener(type: string, listener: (event: KeyboardEvent) => void): void {
      assert.equal(type, "keydown"); this.listener = listener;
    }
    querySelectorAll(): ElementFixture[] { return [this.dialog]; }
  }
  const doc = new DocumentFixture();
  const globals = { document: doc, Element: ElementFixture, HTMLElement: ElementFixture,
    HTMLInputElement: InputFixture, HTMLTextAreaElement: InputFixture };
  const originals = new Map(Object.keys(globals).map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)]));
  let dispatchAttempts = 0;
  const state = { overlay: "mcp_history", confirmation_visible: false } as DesktopViewState;
  const context = { uiState: createUiLocalState(), getViewState: () => state,
    getRenderModel: () => { dispatchAttempts += 1; return null; } } as unknown as ActionContext;
  try {
    for (const [key, value] of Object.entries(globals)) Object.defineProperty(globalThis, key, { configurable: true, value });
    installGlobalKeyboardShortcuts(context);
    const press = (key: string, modifiers: Partial<KeyboardEvent> = {}) => {
      let prevented = false;
      const event = { key, keyCode: 0, isComposing: false, repeat: false, ctrlKey: false, metaKey: false,
        altKey: false, shiftKey: false, target: doc.activeElement, ...modifiers,
        preventDefault: () => { prevented = true; } } as unknown as KeyboardEvent;
      doc.listener!(event);
      return prevented;
    };
    assert.equal(press("Tab"), true);
    assert.equal(doc.activeElement, doc.controls[0], "Tab from outside must enter the dialog");
    assert.equal(press("Tab", { shiftKey: true }), true);
    assert.equal(doc.activeElement, doc.controls[1], "Shift+Tab wraps inside the dialog");
    assert.equal(press("Tab"), true);
    assert.equal(doc.activeElement, doc.controls[0], "Tab wraps instead of reaching the titlebar");
    for (const [key, modifiers] of [["Enter", { ctrlKey: true }], ["n", { ctrlKey: true }],
      ["k", { ctrlKey: true }], ["F8", {}], ["F9", {}]] as const) {
      assert.equal(press(key, modifiers), true, `${key}: background shortcut is suppressed`);
      assert.equal(dispatchAttempts, 0, `${key}: no background action dispatch`);
    }
  } finally {
    for (const [key, descriptor] of originals) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else delete (globalThis as Record<string, unknown>)[key];
    }
  }
});

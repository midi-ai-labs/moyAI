import assert from "node:assert/strict";
import test from "node:test";
import { focusOverlayPrimary } from "../src/events.ts";
import { PostRenderFocusArbiter, type PostRenderFocusIntent } from "../src/focus_arbiter.ts";
import { createUiLocalState } from "../src/ui_state.ts";
import type { DesktopViewState } from "../src/types.ts";

test("View popup rerenders restore the range owner before considering their entry control", () => {
  class ElementFixture {
    readonly isConnected = true;
    readonly hidden = false;
    focusCalls = 0;
    ownerDocument: DocumentFixture;
    constructor(ownerDocument: DocumentFixture) { this.ownerDocument = ownerDocument; }
    getAttribute(): string | null { return null; }
    matches(): boolean { return false; }
    closest(): ElementFixture | null { return null; }
    focus(): void { this.focusCalls += 1; this.ownerDocument.activeElement = this; }
  }
  class InputFixture extends ElementFixture {}
  class DocumentFixture {
    readonly body = new ElementFixture(this);
    readonly documentElement = new ElementFixture(this);
    readonly refresh = new ElementFixture(this);
    readonly range = new InputFixture(this);
    readonly modalButton = new ElementFixture(this);
    readonly titlebarTrigger = new ElementFixture(this);
    activeElement = this.body;
    querySelector(selector: string): ElementFixture | null {
      if (selector.startsWith(".titlebar-popover button")) return this.refresh;
      if (selector === ".modal button:not(:disabled)") return this.modalButton;
      return null;
    }
  }
  const doc = new DocumentFixture();
  const globals = { document: doc, Element: ElementFixture, HTMLElement: ElementFixture,
    HTMLInputElement: InputFixture, HTMLTextAreaElement: InputFixture };
  const originals = new Map(Object.keys(globals).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)]));
  const ui = createUiLocalState();
  const state = { overlay: "view_menu", confirmation_visible: false } as DesktopViewState;
  let scheduled: (() => void) | null = null;
  const arbiter = new PostRenderFocusArbiter({
    schedule(callback: () => void) { scheduled = callback; return 1; },
    cancel() { scheduled = null; },
  }, {
    currentRenderCommit: () => 1, currentInteractionEpoch: () => 1n, interactionActive: () => false,
    activeElement: () => doc.activeElement, bodyElement: () => doc.body, documentElement: () => doc.documentElement,
  });
  const flush = (intents: PostRenderFocusIntent[]) => {
    arbiter.schedule({ renderCommit: 1, interactionEpoch: 1n, intents });
    assert.ok(scheduled); (scheduled as () => void)(); scheduled = null;
  };
  const restoreRange: PostRenderFocusIntent = {
    source: "focus-snapshot", priority: "exact-restore", claim: { kind: "unowned" },
    candidates: [{ resolve: () => doc.range }], isCurrent: () => true,
  };
  try {
    for (const [key, value] of Object.entries(globals)) Object.defineProperty(globalThis, key, { configurable: true, value });
    doc.activeElement = doc.titlebarTrigger;
    const opening = focusOverlayPrimary(state, ui);
    assert.ok(opening); flush([opening]);
    assert.equal(doc.activeElement, doc.refresh, "first opening still enters at Refresh");
    const entryFocusCalls = doc.refresh.focusCalls;

    for (let adjustment = 0; adjustment < 2; adjustment += 1) {
      // The committed opacity response rebuilds the popup; the same-owner snapshot resolves
      // its new range node, while the detached old node leaves document.body active.
      doc.activeElement = doc.body;
      const fallback = focusOverlayPrimary(state, ui);
      assert.ok(fallback); flush([restoreRange, fallback]);
      assert.equal(doc.activeElement, doc.range, "another Arrow key can continue on the range");
      assert.equal(doc.refresh.focusCalls, entryFocusCalls, "a successful mutation does not reopen the menu");
    }

    doc.activeElement = doc.body;
    const modal = focusOverlayPrimary({ ...state, overlay: "about" }, ui);
    assert.ok(modal); flush([restoreRange, modal]);
    assert.equal(doc.activeElement, doc.modalButton, "a real modal still outranks background restoration");

    doc.activeElement = doc.body;
    const staleMenuFallback = focusOverlayPrimary(state, ui);
    assert.ok(staleMenuFallback); flush([{ ...restoreRange, isCurrent: () => false }, staleMenuFallback]);
    assert.equal(doc.activeElement, doc.body, "a stale winning owner never activates an unrelated fallback");
  } finally {
    for (const [key, descriptor] of originals) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else delete (globalThis as Record<string, unknown>)[key];
    }
  }
});

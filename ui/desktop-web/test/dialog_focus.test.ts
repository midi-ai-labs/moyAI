import assert from "node:assert/strict";
import test from "node:test";

import {
  containDialogFocus,
  dialogFocusTargets,
  moveDialogFocus,
} from "../src/dialog_focus.ts";

type InactiveAncestor = "hidden" | "aria-hidden" | "inert";

class FakeDocument {
  activeElement: FakeElement | null = null;

  readonly defaultView = {
    getComputedStyle: (target: FakeElement) => target.style,
  };
}

class FakeElement {
  readonly attributes = new Map<string, string>();
  readonly style = { display: "block", visibility: "visible" };
  readonly name: string;
  readonly ownerDocument: FakeDocument;
  isConnected = true;
  hidden = false;
  disabled = false;
  layoutVisible = true;
  acceptsFocus = true;
  offscreen = false;
  inactiveAncestor: InactiveAncestor | null = null;

  constructor(name: string, ownerDocument: FakeDocument) {
    this.name = name;
    this.ownerDocument = ownerDocument;
  }

  getAttribute(name: string): string | null {
    return this.attributes.get(name) ?? null;
  }

  closest(selector: string): FakeElement | null {
    if (!this.inactiveAncestor) return null;
    const marker = this.inactiveAncestor === "aria-hidden"
      ? "[aria-hidden='true']"
      : `[${this.inactiveAncestor}]`;
    return selector.includes(marker) ? this : null;
  }

  matches(selector: string): boolean {
    return selector === ":disabled" && this.disabled;
  }

  getClientRects(): readonly object[] {
    return this.layoutVisible ? [{}] : [];
  }

  focus(options?: { preventScroll?: boolean }): void {
    if (this.acceptsFocus) {
      this.ownerDocument.activeElement = this;
      if (!options?.preventScroll) this.offscreen = false;
    }
  }
}

class FakeDialog extends FakeElement {
  readonly candidates: FakeElement[];
  status: FakeElement | null = null;

  constructor(ownerDocument: FakeDocument, candidates: FakeElement[]) {
    super("dialog", ownerDocument);
    this.candidates = candidates;
  }

  querySelectorAll(): FakeElement[] {
    return this.candidates;
  }

  querySelector(selector: string): FakeElement | null {
    return selector === ".permission-decision-status" ? this.status : null;
  }
}

function asHtmlElement(value: FakeElement): HTMLElement {
  return value as unknown as HTMLElement;
}

function asDialog(value: FakeDialog): HTMLElement {
  return value as unknown as HTMLElement;
}

test("explicit dialog Tab reveals offscreen controls while keeping focus inside the dialog", () => {
  const document = new FakeDocument();
  const first = new FakeElement("first", document);
  const belowFold = new FakeElement("advanced-model-controls", document);
  belowFold.offscreen = true;
  const dialog = new FakeDialog(document, [first, belowFold]);
  document.activeElement = first;
  assert.equal(moveDialogFocus(asDialog(dialog), asHtmlElement(first), false), true);
  assert.equal(document.activeElement, belowFold);
  assert.equal(belowFold.offscreen, false);
  first.offscreen = true;
  assert.equal(moveDialogFocus(asDialog(dialog), asHtmlElement(belowFold), true), true);
  assert.equal(document.activeElement, first);
  assert.equal(first.offscreen, false);
});

test("dialog focus keeps a closed model disclosure summary in the Settings order", () => {
  const document = new FakeDocument();
  const modelSelect = new FakeElement("model-select", document);
  const summary = new FakeElement("closed-details-summary", document);
  const closedDetailsInput = new FakeElement("closed-details-input", document);
  closedDetailsInput.layoutVisible = false;
  const nextSettingsField = new FakeElement("next-settings-field", document);
  const disabled = new FakeElement("disabled", document);
  disabled.disabled = true;
  const ariaDisabled = new FakeElement("aria-disabled", document);
  ariaDisabled.attributes.set("aria-disabled", "true");
  const negativeTabIndex = new FakeElement("negative-tab-index", document);
  negativeTabIndex.attributes.set("tabindex", "-2");
  const hidden = new FakeElement("hidden", document);
  hidden.hidden = true;
  const hiddenAncestor = new FakeElement("hidden-ancestor", document);
  hiddenAncestor.inactiveAncestor = "hidden";
  const ariaHiddenAncestor = new FakeElement("aria-hidden-ancestor", document);
  ariaHiddenAncestor.inactiveAncestor = "aria-hidden";
  const inertAncestor = new FakeElement("inert-ancestor", document);
  inertAncestor.inactiveAncestor = "inert";
  const cssHidden = new FakeElement("css-hidden", document);
  cssHidden.style.display = "none";
  const cssInvisible = new FakeElement("css-invisible", document);
  cssInvisible.style.visibility = "hidden";
  const disconnected = new FakeElement("disconnected", document);
  disconnected.isConnected = false;
  const dialog = new FakeDialog(document, [
    modelSelect,
    summary,
    closedDetailsInput,
    nextSettingsField,
    disabled,
    ariaDisabled,
    negativeTabIndex,
    hidden,
    hiddenAncestor,
    ariaHiddenAncestor,
    inertAncestor,
    cssHidden,
    cssInvisible,
    disconnected,
  ]);

  assert.deepEqual(
    dialogFocusTargets(asDialog(dialog)),
    [asHtmlElement(modelSelect), asHtmlElement(summary), asHtmlElement(nextSettingsField)],
  );
});

test("dialog focus moves forward and backwards in DOM order with wrapping", () => {
  const document = new FakeDocument();
  const cancel = new FakeElement("cancel", document);
  const confirm = new FakeElement("confirm", document);
  const disclosure = new FakeElement("summary", document);
  const dialog = new FakeDialog(document, [cancel, confirm, disclosure]);

  document.activeElement = cancel;
  assert.equal(moveDialogFocus(asDialog(dialog), asHtmlElement(cancel), false), true);
  assert.equal(document.activeElement, confirm);
  assert.equal(moveDialogFocus(asDialog(dialog), asHtmlElement(confirm), false), true);
  assert.equal(document.activeElement, disclosure);
  assert.equal(moveDialogFocus(asDialog(dialog), asHtmlElement(disclosure), false), true);
  assert.equal(document.activeElement, cancel);

  assert.equal(moveDialogFocus(asDialog(dialog), asHtmlElement(cancel), true), true);
  assert.equal(document.activeElement, disclosure);
  assert.equal(moveDialogFocus(asDialog(dialog), null, false), true);
  assert.equal(document.activeElement, cancel);
  assert.equal(moveDialogFocus(asDialog(dialog), null, true), true);
  assert.equal(document.activeElement, disclosure);
});

test("dialog focus skips a candidate that unexpectedly refuses focus", () => {
  const document = new FakeDocument();
  const cancel = new FakeElement("cancel", document);
  const stale = new FakeElement("stale", document);
  stale.acceptsFocus = false;
  const confirm = new FakeElement("confirm", document);
  const dialog = new FakeDialog(document, [cancel, stale, confirm]);

  document.activeElement = cancel;
  assert.equal(moveDialogFocus(asDialog(dialog), asHtmlElement(cancel), false), true);
  assert.equal(document.activeElement, confirm);
});

test("permission fallback retains focus when every decision action is unavailable", () => {
  const document = new FakeDocument();
  const approve = new FakeElement("approve", document);
  approve.disabled = true;
  const abort = new FakeElement("abort", document);
  abort.inactiveAncestor = "inert";
  const status = new FakeElement("permission-status", document);
  status.attributes.set("tabindex", "-1");
  const dialog = new FakeDialog(document, [approve, abort, status]);
  dialog.status = status;

  assert.equal(containDialogFocus(asDialog(dialog), null, false), true);
  assert.equal(document.activeElement, status);
});

test("dialog itself is the final focus owner when it has no eligible control or status", () => {
  const document = new FakeDocument();
  const dialog = new FakeDialog(document, []);

  assert.equal(containDialogFocus(asDialog(dialog), null, false), true);
  assert.equal(document.activeElement, dialog);
});

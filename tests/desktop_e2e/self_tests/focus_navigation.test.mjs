import assert from "node:assert/strict";
import test from "node:test";

import { TabFocusNavigator } from "../core/focus_navigation.mjs";

const button = (id) => ({ tag: "BUTTON", id, action: null, focusKey: id, configKey: null, sideSetting: null });
const body = { tag: "BODY", id: null, action: null, focusKey: null, configKey: null, sideSetting: null };

function inDocumentObservation(before, after, beforeSequence = 10) {
  return {
    beforeSequence,
    dispatchError: null,
    beforeActive: before,
    afterActive: after,
    events: [
      { sequence: beforeSequence + 1, type: "keydown", key: "Tab", isTrusted: true, ...before },
      { sequence: beforeSequence + 2, type: "focusin", key: null, isTrusted: true, ...after },
      { sequence: beforeSequence + 3, type: "keyup", key: "Tab", isTrusted: true, ...after },
    ],
  };
}

test("ordinary in-document Tab transition is acquired", () => {
  const navigator = new TabFocusNavigator();
  const result = navigator.observe(inDocumentObservation(button("first"), button("second")));
  assert.equal(result.classification, "acquired");
  assert.equal(result.transition, "in_document");
  assert.equal(navigator.pendingBoundary, null);
});
test("DIV to BODY without focusin is a valid pending boundary and exact next Tab reenters", () => {
  const navigator = new TabFocusNavigator();
  const boundary = navigator.observe({
    beforeSequence: 470,
    dispatchError: null,
    beforeActive: { tag: "DIV", id: "artifact", action: null, focusKey: "artifact", configKey: null, sideSetting: null },
    afterActive: body,
    events: [
      { sequence: 471, type: "keydown", key: "Tab", isTrusted: true, tag: "DIV", id: "artifact", action: null, focusKey: "artifact", configKey: null, sideSetting: null },
      { sequence: 472, type: "keyup", key: "Tab", isTrusted: true, ...body },
    ],
  });
  assert.equal(boundary.classification, "acquired");
  assert.equal(boundary.transition, "document_boundary_pending");
  assert.equal(boundary.pending_boundary.sequence, 472);

  const reentry = navigator.observe(inDocumentObservation(body, button("file-menu"), 472));
  assert.equal(reentry.classification, "acquired");
  assert.equal(reentry.transition, "document_reentry");
  assert.equal(navigator.pendingBoundary, null);
});

test("settlement gaps and stale boundary ownership are harness failures, never product failures", () => {
  const navigator = new TabFocusNavigator();
  const missingFocus = inDocumentObservation(button("first"), button("second"));
  missingFocus.events = missingFocus.events.filter((event) => event.type !== "focusin");
  assert.deepEqual(navigator.observe(missingFocus).reasons, ["in-document-focusin-not-exact"]);

  const boundaryNavigator = new TabFocusNavigator();
  boundaryNavigator.observe({
    beforeSequence: 1,
    dispatchError: null,
    beforeActive: button("last"),
    afterActive: body,
    events: [
      { sequence: 2, type: "keydown", key: "Tab", isTrusted: true, ...button("last") },
      { sequence: 3, type: "keyup", key: "Tab", isTrusted: true, ...body },
    ],
  });
  const stale = boundaryNavigator.observe(inDocumentObservation(body, button("first"), 4));
  assert.equal(stale.classification, "harness_ng");
  assert.match(stale.reasons.join(","), /boundary-next-tab-not-contiguous/);
});

import assert from "node:assert/strict";
import test from "node:test";

import {
  settleTextMutationFailure,
  textMutationSettlementOwnerIsCurrent,
  type TextMutationSettlementOwner,
} from "../src/events.ts";

const currentOwner: TextMutationSettlementOwner = {
  hasQueuedValue: false,
  currentGeneration: 9,
  requestGeneration: 9,
  targetStillMatches: true,
};

test("stale debounced search failures cannot recover conflicts or report errors", () => {
  const staleOwners: Array<[string, TextMutationSettlementOwner]> = [
    ["a newer queued query", { ...currentOwner, hasQueuedValue: true }],
    ["a superseding generation", { ...currentOwner, currentGeneration: 10 }],
    ["a changed search target", { ...currentOwner, targetStillMatches: false }],
  ];

  for (const [label, owner] of staleOwners) {
    for (const recoverResult of [true, false]) {
      let recoverCalls = 0;
      let reportCalls = 0;
      const error = new Error(`${label}:${recoverResult ? "conflict" : "failure"}`);

      assert.equal(textMutationSettlementOwnerIsCurrent(owner), false, label);
      assert.equal(settleTextMutationFailure(
        owner,
        error,
        (received) => {
          recoverCalls += 1;
          assert.equal(received, error);
          return recoverResult;
        },
        (received) => {
          reportCalls += 1;
          assert.equal(received, error);
        },
      ), false, label);
      assert.equal(recoverCalls, 0, `${label} must not recover an obsolete conflict`);
      assert.equal(reportCalls, 0, `${label} must not surface an obsolete error`);
    }
  }
});

test("the exact current debounced search owner recovers conflicts and reports other errors", () => {
  assert.equal(textMutationSettlementOwnerIsCurrent(currentOwner), true);

  const conflict = new Error("conflict");
  let recovered: unknown = null;
  let reported: unknown = null;
  assert.equal(settleTextMutationFailure(
    currentOwner,
    conflict,
    (error) => {
      recovered = error;
      return true;
    },
    (error) => { reported = error; },
  ), true);
  assert.equal(recovered, conflict);
  assert.equal(reported, null);

  const failure = new Error("failure");
  recovered = null;
  assert.equal(settleTextMutationFailure(
    currentOwner,
    failure,
    (error) => {
      recovered = error;
      return false;
    },
    (error) => { reported = error; },
  ), true);
  assert.equal(recovered, failure);
  assert.equal(reported, failure);
});

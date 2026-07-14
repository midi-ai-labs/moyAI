import assert from "node:assert/strict";
import test from "node:test";

import {
  beginLocalDecision,
  beginPermissionDecision,
  failLocalDecision,
  failPermissionDecision,
  finishLocalDecision,
  finishPermissionDecision,
  permissionDecisionForEscape,
  permissionDecisionResponseAccepted,
  permissionDecisionShouldFocusComposer,
  reconcilePermissionDecision,
  recoverPermissionDecisionFromConflict,
  type PermissionDecisionState,
} from "../src/decision_state.ts";

test("permission decision dispatches once while pending and recovers after failure", () => {
  const owner = {
    permissionDecision: null as PermissionDecisionState | null,
    nextPermissionSubmissionId: 1,
  };
  const first = beginPermissionDecision(owner, "9007199254740993", "approved");
  assert.ok(first);
  assert.equal(beginPermissionDecision(owner, "9007199254740993", "approved"), null);
  assert.deepEqual(owner.permissionDecision, {
    phase: "submitting",
    requestId: "9007199254740993",
    submissionId: 1,
    decision: "approved",
  });

  assert.equal(failPermissionDecision(owner, first, "failed"), true);
  assert.deepEqual(owner.permissionDecision, {
    phase: "failed",
    requestId: "9007199254740993",
    error: "failed",
  });
  const retry = beginPermissionDecision(owner, "9007199254740993", "abort");
  assert.ok(retry);
  assert.equal(retry.submissionId, 2);
  assert.equal(finishPermissionDecision(owner, first), false);
  assert.equal(failPermissionDecision(owner, first, "late first failure"), false);
  assert.equal(owner.permissionDecision?.phase, "submitting");
  assert.equal(
    owner.permissionDecision?.phase === "submitting"
      ? owner.permissionDecision.submissionId
      : null,
    2,
  );
  assert.equal(finishPermissionDecision(owner, retry), true);
  assert.deepEqual(owner.permissionDecision, {
    phase: "ready",
    requestId: "9007199254740993",
  });
});

test("a new permission id owns fresh state and ignores every late settlement from the old id", () => {
  const owner = {
    permissionDecision: null as PermissionDecisionState | null,
    nextPermissionSubmissionId: 1,
  };
  reconcilePermissionDecision(owner, "A");
  const requestA = beginPermissionDecision(owner, "A", "approved");
  assert.ok(requestA);
  const sameIdState = owner.permissionDecision;
  reconcilePermissionDecision(owner, "A");
  assert.equal(owner.permissionDecision, sameIdState, "same-id polling preserves the in-flight owner");

  reconcilePermissionDecision(owner, "B");
  assert.deepEqual(owner.permissionDecision, { phase: "ready", requestId: "B" });
  assert.equal(finishPermissionDecision(owner, requestA), false);
  assert.equal(failPermissionDecision(owner, requestA, "stale A error"), false);
  assert.deepEqual(owner.permissionDecision, { phase: "ready", requestId: "B" });

  const requestB = beginPermissionDecision(owner, "B", "abort");
  assert.ok(requestB);
  assert.equal(failPermissionDecision(owner, requestA, "still stale"), false);
  assert.equal(owner.permissionDecision?.requestId, "B");
  assert.equal(owner.permissionDecision?.phase, "submitting");
  reconcilePermissionDecision(owner, null);
  assert.equal(owner.permissionDecision, null);
});

test("a command conflict reconciles the current submission to the backend permission owner", () => {
  const owner = {
    permissionDecision: null as PermissionDecisionState | null,
    nextPermissionSubmissionId: 1,
  };

  const sameId = beginPermissionDecision(owner, "A", "approved");
  assert.ok(sameId);
  assert.equal(recoverPermissionDecisionFromConflict(owner, sameId, "A"), true);
  assert.deepEqual(owner.permissionDecision, { phase: "ready", requestId: "A" });

  const changedId = beginPermissionDecision(owner, "A", "abort");
  assert.ok(changedId);
  assert.equal(recoverPermissionDecisionFromConflict(owner, changedId, "B"), true);
  assert.deepEqual(owner.permissionDecision, { phase: "ready", requestId: "B" });

  const closed = beginPermissionDecision(owner, "B", "approved");
  assert.ok(closed);
  assert.equal(recoverPermissionDecisionFromConflict(owner, closed, null), true);
  assert.equal(owner.permissionDecision, null);
});

test("a late conflict cannot overwrite the newer permission owner", () => {
  const owner = {
    permissionDecision: null as PermissionDecisionState | null,
    nextPermissionSubmissionId: 1,
  };
  const requestA = beginPermissionDecision(owner, "A", "approved");
  assert.ok(requestA);
  reconcilePermissionDecision(owner, "B");
  const requestB = beginPermissionDecision(owner, "B", "abort");
  assert.ok(requestB);

  assert.equal(recoverPermissionDecisionFromConflict(owner, requestA, "A"), false);
  assert.deepEqual(owner.permissionDecision, {
    phase: "submitting",
    requestId: "B",
    submissionId: 2,
    decision: "abort",
  });
});

test("permission decision reconciles only its own confirmation id", () => {
  assert.equal(permissionDecisionResponseAccepted("9007199254740993", true, "9007199254740993"), false);
  assert.equal(permissionDecisionResponseAccepted("9007199254740993", false, null), true);
  assert.equal(
    permissionDecisionResponseAccepted("9007199254740993", true, "9007199254740994"),
    true,
    "a newer confirmation must not be reported as failure of the completed decision",
  );
});

test("Escape maps a visible permission request to abort and ignores key repeat", () => {
  assert.equal(permissionDecisionForEscape(true, false), "abort");
  assert.equal(permissionDecisionForEscape(true, true), null);
  assert.equal(permissionDecisionForEscape(false, false), null);

  const owner = {
    permissionDecision: null as PermissionDecisionState | null,
    nextPermissionSubmissionId: 1,
  };
  const firstDecision = permissionDecisionForEscape(true, false);
  const first = firstDecision === null ? null : beginPermissionDecision(owner, "A", firstDecision);
  assert.ok(first);
  const repeatedDecision = permissionDecisionForEscape(true, false);
  assert.equal(
    repeatedDecision === null ? null : beginPermissionDecision(owner, "A", repeatedDecision),
    null,
    "a second Escape cannot dispatch while the first Abort is submitting",
  );
});

test("only the accepted current Abort settlement requests composer focus", () => {
  const owner = {
    permissionDecision: null as PermissionDecisionState | null,
    nextPermissionSubmissionId: 1,
  };
  const abort = beginPermissionDecision(owner, "A", "abort");
  assert.ok(abort);
  assert.equal(permissionDecisionShouldFocusComposer(abort, false, false), false);
  assert.equal(permissionDecisionShouldFocusComposer(abort, true, true), false);
  assert.equal(permissionDecisionShouldFocusComposer(abort, true, false), true);

  reconcilePermissionDecision(owner, "B");
  assert.equal(finishPermissionDecision(owner, abort), false);
  assert.equal(permissionDecisionShouldFocusComposer(abort, false, false), false);
});

test("local confirmation dispatches once while pending and restores retry after failure", () => {
  const owner = {
    localConfirmationDecisionPending: false,
    localConfirmationDecisionError: "",
  };
  let dispatches = 0;

  if (beginLocalDecision(owner, true)) dispatches += 1;
  if (beginLocalDecision(owner, true)) dispatches += 1;

  assert.equal(dispatches, 1);
  failLocalDecision(owner, "retry");
  assert.equal(owner.localConfirmationDecisionPending, false);
  assert.equal(owner.localConfirmationDecisionError, "retry");
  assert.equal(beginLocalDecision(owner, true), true);
  finishLocalDecision(owner);
  assert.equal(owner.localConfirmationDecisionPending, false);
  assert.equal(owner.localConfirmationDecisionError, "");
});

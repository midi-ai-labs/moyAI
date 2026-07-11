import assert from "node:assert/strict";
import test from "node:test";

import {
  beginLocalDecision,
  beginPermissionDecision,
  failLocalDecision,
  failPermissionDecision,
  finishLocalDecision,
  finishPermissionDecision,
  permissionDecisionResponseAccepted,
} from "../src/decision_state.ts";

test("permission decision dispatches once while pending and recovers after failure", () => {
  const owner = {
    permissionDecisionPending: false,
    permissionDecisionAllow: null as boolean | null,
    permissionDecisionConfirmationId: null as number | null,
    permissionDecisionError: "",
  };
  let dispatches = 0;

  if (beginPermissionDecision(owner, 41, true)) dispatches += 1;
  if (beginPermissionDecision(owner, 41, true)) dispatches += 1;

  assert.equal(dispatches, 1);
  assert.equal(owner.permissionDecisionPending, true);
  assert.equal(owner.permissionDecisionConfirmationId, 41);

  failPermissionDecision(owner, "failed");
  assert.equal(owner.permissionDecisionPending, false);
  assert.equal(owner.permissionDecisionError, "failed");
  assert.equal(beginPermissionDecision(owner, 41, false), true);
  finishPermissionDecision(owner);
  assert.equal(owner.permissionDecisionPending, false);
  assert.equal(owner.permissionDecisionAllow, null);
});

test("permission decision reconciles only its own confirmation id", () => {
  assert.equal(permissionDecisionResponseAccepted(41, true, 41), false);
  assert.equal(permissionDecisionResponseAccepted(41, false, null), true);
  assert.equal(
    permissionDecisionResponseAccepted(41, true, 42),
    true,
    "a newer confirmation must not be reported as failure of the completed decision",
  );
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

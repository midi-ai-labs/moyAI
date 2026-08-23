import assert from "node:assert/strict";
import test from "node:test";

import { classifyFreshProjectionSettlement, isSettledDesktopProjection } from "../core/projection_settlement.mjs";

function projection(revision, overrides = {}) {
  return {
    projection_revision: String(revision),
    navigation_loading: false,
    async_polling_required: false,
    pending_async_operations: [],
    busy: false,
    background_mutation_pending: false,
    ...overrides,
  };
}

function response(command, requestSequence, responseSequence, revision, id) {
  return {
    command,
    status: "returned",
    transportRequestId: id,
    requestSequence,
    responseSequence,
    responseRevision: String(revision),
  };
}

test("ordinary poll after probe configuration is admitted only when settled", () => {
  const result = classifyFreshProjectionSettlement({
    configuredSequence: 10,
    route: "ordinary_poll",
    ordinaryResponse: response("desktop_state", 11, 12, 20, "ordinary-11"),
    ordinaryProjection: projection(20),
  });
  assert.equal(result.classification, "acquired");
  assert.equal(result.normalized.ordinary_response_sequence, 12);

  assert.equal(isSettledDesktopProjection(projection(20, { pending_async_operations: ["refresh"] })), false);
});
test("refresh response is not settlement; a later distinct ordinary response is required", () => {
  const trigger = response("refresh_desktop", 11, 12, 20, "refresh-11");
  const pending = projection(20, {
    navigation_loading: true,
    async_polling_required: true,
    pending_async_operations: ["snapshot_refresh"],
  });
  const missing = classifyFreshProjectionSettlement({
    configuredSequence: 10,
    route: "explicit_refresh",
    triggerResponse: trigger,
    triggerProjection: pending,
    ordinaryResponse: null,
    ordinaryProjection: null,
  });
  assert.equal(missing.code, "projection-ordinary-response-missing");

  const acquired = classifyFreshProjectionSettlement({
    configuredSequence: 10,
    route: "explicit_refresh",
    triggerResponse: trigger,
    triggerProjection: pending,
    ordinaryResponse: response("desktop_state", 13, 14, 21, "ordinary-13"),
    ordinaryProjection: projection(21),
  });
  assert.equal(acquired.classification, "acquired");
  assert.equal(acquired.normalized.trigger_response_sequence, 12);
  assert.equal(acquired.normalized.ordinary_response_sequence, 14);
});

test("stale or still-pending ordinary responses remain harness acquisition failures", () => {
  const trigger = response("refresh_desktop", 11, 12, 20, "refresh-11");
  const pending = projection(20, { async_polling_required: true, pending_async_operations: ["snapshot_refresh"] });
  const stale = classifyFreshProjectionSettlement({
    configuredSequence: 10,
    route: "explicit_refresh",
    triggerResponse: trigger,
    triggerProjection: pending,
    ordinaryResponse: response("desktop_state", 11, 12, 20, "ordinary-11"),
    ordinaryProjection: projection(20),
  });
  assert.equal(stale.code, "projection-ordinary-response-stale");

  const notSettled = classifyFreshProjectionSettlement({
    configuredSequence: 10,
    route: "explicit_refresh",
    triggerResponse: trigger,
    triggerProjection: pending,
    ordinaryResponse: response("desktop_state", 13, 14, 21, "ordinary-13"),
    ordinaryProjection: projection(21, { busy: true }),
  });
  assert.equal(notSettled.code, "projection-not-settled");
});

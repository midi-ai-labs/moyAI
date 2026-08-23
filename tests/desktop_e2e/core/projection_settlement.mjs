function exactEnvelope(value, command) {
  return value !== null
    && typeof value === "object"
    && value.command === command
    && value.status === "returned"
    && typeof value.transportRequestId === "string"
    && value.transportRequestId.length > 0
    && Number.isInteger(value.requestSequence)
    && Number.isInteger(value.responseSequence)
    && value.requestSequence >= 0
    && value.responseSequence > value.requestSequence;
}
export function isSettledDesktopProjection(value) {
  return value !== null
    && typeof value === "object"
    && typeof value.projection_revision === "string"
    && /^\d+$/.test(value.projection_revision)
    && value.navigation_loading === false
    && value.async_polling_required === false
    && Array.isArray(value.pending_async_operations)
    && value.pending_async_operations.length === 0
    && value.busy === false
    && value.background_mutation_pending === false;
}

function reject(code, reason) {
  return { classification: "harness_ng", code, reason, normalized: null };
}

export function classifyFreshProjectionSettlement({ configuredSequence, route, triggerResponse = null, triggerProjection = null, ordinaryResponse, ordinaryProjection }) {
  if (!Number.isInteger(configuredSequence) || configuredSequence < 0) {
    return reject("projection-owner-invalid", "probe configuration sequence is invalid");
  }
  if (!new Set(["ordinary_poll", "explicit_refresh"]).has(route)) {
    return reject("projection-route-invalid", "projection acquisition route is unknown");
  }
  if (route === "explicit_refresh") {
    if (!exactEnvelope(triggerResponse, "refresh_desktop") || triggerResponse.responseSequence <= configuredSequence) {
      return reject("projection-trigger-invalid", "refresh response does not postdate probe configuration");
    }
    if (String(triggerResponse.responseRevision) !== String(triggerProjection?.projection_revision)) {
      return reject("projection-trigger-revision-mismatch", "refresh response and projection revisions differ");
    }
  }
  if (!exactEnvelope(ordinaryResponse, "desktop_state")) {
    return reject("projection-ordinary-response-missing", "an exact ordinary desktop_state response is required");
  }
  const lowerBound = route === "explicit_refresh" ? triggerResponse.responseSequence : configuredSequence;
  if (ordinaryResponse.requestSequence <= configuredSequence || ordinaryResponse.responseSequence <= lowerBound) {
    return reject("projection-ordinary-response-stale", "ordinary response does not postdate its owner and trigger");
  }
  if (String(ordinaryResponse.responseRevision) !== String(ordinaryProjection?.projection_revision)) {
    return reject("projection-ordinary-revision-mismatch", "ordinary response and projection revisions differ");
  }
  if (!isSettledDesktopProjection(ordinaryProjection)) {
    return reject("projection-not-settled", "ordinary desktop_state is still busy or has pending async work");
  }
  return {
    classification: "acquired",
    code: "fresh-settled-projection",
    reason: "a distinct settled ordinary desktop_state response postdates the probe owner and trigger",
    normalized: {
      route,
      configured_sequence: configuredSequence,
      trigger_response_sequence: triggerResponse?.responseSequence ?? null,
      ordinary_request_sequence: ordinaryResponse.requestSequence,
      ordinary_response_sequence: ordinaryResponse.responseSequence,
      projection_revision: ordinaryProjection.projection_revision,
    },
  };
}

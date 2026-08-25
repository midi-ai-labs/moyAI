import { waitForObservation } from "./deadline.mjs";

export function classifySemanticTargetSettlement(target, { expectedIdentity } = {}) {
  if (expectedIdentity === null
    || typeof expectedIdentity !== "object"
    || Array.isArray(expectedIdentity)) {
    throw new TypeError("semantic target settlement requires an expected identity");
  }
  const observation = target?.observation;
  if (observation === null || typeof observation !== "object" || Array.isArray(observation)) {
    return { decision: "fail", failures: ["semantic-target-observation-invalid"] };
  }
  if (!Number.isSafeInteger(observation.count) || observation.count < 0) {
    return { decision: "fail", failures: ["semantic-target-count-invalid"] };
  }
  if (observation.count === 0) return { decision: "pending", failures: [] };
  if (observation.count !== 1) {
    return { decision: "fail", failures: ["semantic-target-cardinality"] };
  }
  for (const [field, expected] of Object.entries(expectedIdentity)) {
    if (observation.identity?.[field] !== expected) {
      return { decision: "fail", failures: ["semantic-target-identity"] };
    }
  }
  if (observation.connected !== true
    || observation.visible !== true
    || observation.enabled !== true) {
    return { decision: "pending", failures: [] };
  }
  return { decision: "pass", failures: [] };
}

export async function waitForSemanticTargetSettlement({
  input,
  locator,
  label,
  expectedIdentity = locator?.identity,
  timeoutMs = 10_000,
  pollMs = 16,
}) {
  if (input === null || typeof input !== "object" || typeof input.observeExactTarget !== "function") {
    throw new TypeError("semantic target settlement requires a WebView input owner");
  }
  if (locator === null || typeof locator !== "object" || Array.isArray(locator)) {
    throw new TypeError("semantic target settlement requires a locator");
  }
  if (typeof label !== "string" || label.length === 0) {
    throw new TypeError("semantic target settlement requires a label");
  }
  return waitForObservation({
    label,
    timeoutMs,
    pollMs,
    sample: async () => {
      const target = await input.observeExactTarget(locator);
      return {
        target,
        classified: classifySemanticTargetSettlement(target, { expectedIdentity }),
      };
    },
    accept: ({ classified }) => classified.decision !== "pending",
    retrySampleErrors: false,
  });
}

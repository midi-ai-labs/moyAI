import crypto from "node:crypto";
import { createInterface } from "node:readline";

const DECISIONS = new Set(["approve", "stop", "deny"]);
const MAX_WAIT_MS = 5 * 60 * 1000;

export function operatorRequestFingerprint(request) {
  if (request === null || typeof request !== "object" || Array.isArray(request)
    || typeof request.confirmation_id !== "string" || !request.confirmation_id
    || request.confirmation_id.length > 128 || /[\u0000-\u001f\u007f]/u.test(request.confirmation_id)) {
    throw new TypeError("operator review requires one exact confirmation identity");
  }
  return crypto.createHash("sha256").update(JSON.stringify(request)).digest("hex");
}

export function validateOperatorDecision(value, request) {
  const fingerprint = operatorRequestFingerprint(request);
  if (value === null || typeof value !== "object" || Array.isArray(value)
    || Object.keys(value).length !== 3
    || value.confirmation_id !== request.confirmation_id || value.request_sha256 !== fingerprint
    || !DECISIONS.has(value.decision)) throw new TypeError("decision must match this confirmation and request SHA-256");
  return Object.freeze({ confirmation_id: value.confirmation_id, request_sha256: fingerprint, decision: value.decision });
}

export async function waitForOperatorReview(request, {
  input = process.stdin, output = process.stdout, timeoutMs = MAX_WAIT_MS, evidence = null,
} = {}) {
  if (!Number.isInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > MAX_WAIT_MS) throw new TypeError("operator review wait must be bounded by five minutes");
  const snapshot = structuredClone(request);
  const requestSha256 = operatorRequestFingerprint(snapshot);
  const wasPaused = input.isPaused();
  const lines = createInterface({ input, terminal: false, crlfDelay: Infinity });
  let timer;
  let onLine;
  let onClose;
  let onError;
  try {
    return await new Promise(resolve => {
      let settled = false;
      const finish = value => { if (!settled) { settled = true; resolve(value); } };
      onLine = line => {
        if (settled) return;
        try {
          if (Buffer.byteLength(line) > 4096) throw new TypeError("operator decision line exceeds 4096 bytes");
          const decision = validateOperatorDecision(JSON.parse(line), snapshot);
          finish({ status: "decided", ...decision });
        } catch (error) {
          output.write(`${JSON.stringify({ type: "operator-review-rejected", confirmation_id: snapshot.confirmation_id, message: error.message })}\n`);
        }
      };
      onClose = () => finish({ status: "not_decided", reason: "stdin-closed" });
      onError = error => finish({ status: "not_decided", reason: "stdin-error", message: error.message });
      lines.on("line", onLine);
      lines.on("close", onClose);
      lines.on("error", onError);
      timer = setTimeout(() => finish({ status: "not_decided", reason: "operator-timeout" }), timeoutMs);
      output.write(`${JSON.stringify({ type: "operator-review-request", confirmation_id: snapshot.confirmation_id,
        request_sha256: requestSha256, request: snapshot, evidence, timeout_ms: timeoutMs })}\n`);
      if (input.readableEnded || input.destroyed) onClose();
    });
  } finally {
    clearTimeout(timer);
    lines.removeListener("line", onLine);
    lines.removeListener("close", onClose);
    lines.removeListener("error", onError);
    lines.close();
    if (wasPaused) input.pause();
  }
}

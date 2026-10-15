import crypto from "node:crypto";
import { createInterface } from "node:readline";

export const manualSuiteReviewFingerprint = request => crypto.createHash("sha256").update(JSON.stringify(request)).digest("hex");

export function validateManualSuiteReview(value, request) {
  if (!value || typeof value !== "object" || Array.isArray(value) || Object.keys(value).length !== 4
    || value.scenario_id !== request.scenario_id || value.request_sha256 !== manualSuiteReviewFingerprint(request)
    || !["continue", "stop"].includes(value.action) || typeof value.fixture_accepted !== "boolean"
    || value.fixture_accepted && (!request.fixture_review_required || !["pass", "manual_pending"].includes(request.classification))) {
    throw new TypeError("suite review must match the current case and evidence; only a reviewed successful fixture can be accepted");
  }
  return structuredClone(value);
}

export async function waitForManualSuiteReview(request, { input = process.stdin, output = process.stdout, timeoutMs = 300_000 } = {}) {
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 300_000) throw new TypeError("suite review timeout must be within five minutes");
  const snapshot = structuredClone(request);
  const wasPaused = input.isPaused();
  const lines = createInterface({ input, terminal: false, crlfDelay: Infinity });
  let timer;
  try {
    return await new Promise(resolve => {
      let settled = false;
      const finish = result => { if (!settled) { settled = true; resolve(result); } };
      lines.on("line", line => {
        if (settled) return;
        try {
          if (Buffer.byteLength(line) > 4096) throw new TypeError("review line exceeds 4096 bytes");
          finish(validateManualSuiteReview(JSON.parse(line), snapshot));
        } catch (error) { output.write(`${JSON.stringify({ type: "manual-suite-review-rejected", message: error.message })}\n`); }
      });
      lines.on("close", () => finish({ action: "stop", fixture_accepted: false, reason: "stdin-closed" }));
      lines.on("error", () => finish({ action: "stop", fixture_accepted: false, reason: "stdin-error" }));
      timer = setTimeout(() => finish({ action: "stop", fixture_accepted: false, reason: "review-timeout" }), timeoutMs);
      output.write(`${JSON.stringify({ type: "manual-suite-review-request", ...snapshot, request_sha256: manualSuiteReviewFingerprint(snapshot), timeout_ms: timeoutMs })}\n`);
      if (input.readableEnded || input.destroyed) finish({ action: "stop", fixture_accepted: false, reason: "stdin-closed" });
    });
  } finally {
    clearTimeout(timer);
    lines.removeAllListeners();
    lines.close();
    if (wasPaused) input.pause();
  }
}

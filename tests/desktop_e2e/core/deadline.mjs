function defaultSleep(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

export function normalizeManualObservationTimeout(value) {
  if (value === undefined) return undefined;
  if (!Number.isSafeInteger(value) || value <= 0 || value > 7_200_000) {
    throw new TypeError("observation_timeout_ms must be a positive safe integer no greater than 7200000");
  }
  return value;
}

export class ManualObservationBudget {
  constructor({ timeoutMs, now = () => Date.now() }) {
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0) throw new TypeError("manual observation budget must be a positive safe integer");
    if (typeof now !== "function") throw new TypeError("manual observation clock is required");
    this.timeoutMs = timeoutMs;
    this.now = now;
    this.startedAt = null;
  }
  beginOnce() {
    this.startedAt ??= this.now();
    return this.snapshot();
  }
  remainingMs(ceiling = Infinity) {
    const remaining = this.startedAt === null ? this.timeoutMs : Math.max(0, this.startedAt + this.timeoutMs - this.now());
    return Math.min(ceiling, remaining);
  }
  get expired() { return this.startedAt !== null && this.remainingMs() <= 0; }
  assertRemaining(label, ceiling = Infinity) {
    const remaining = this.remainingMs(ceiling);
    if (remaining > 0) return remaining;
    const error = new Error(`${label} exhausted the case observation budget`);
    error.code = "manual-observation-timeout";
    error.evidence = { label, ...this.snapshot() };
    throw error;
  }
  snapshot() {
    return { timeout_ms: this.timeoutMs, started_at_ms: this.startedAt,
      deadline_ms: this.startedAt === null ? null : this.startedAt + this.timeoutMs, remaining_ms: this.remainingMs() };
  }
}

async function beforeHardDeadline(operation, milliseconds, label) {
  let timer;
  try {
    return await Promise.race([
      Promise.resolve().then(operation),
      new Promise((_, reject) => {
        timer = setTimeout(() => {
          const error = new Error(`${label} attempt exceeded its remaining deadline`);
          error.code = "observation-attempt-timeout";
          reject(error);
        }, milliseconds);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

export async function waitForObservation({
  label,
  sample,
  accept,
  timeoutMs,
  pollMs = 100,
  now = () => Date.now(),
  sleep = defaultSleep,
  retrySampleErrors = true,
}) {
  if (typeof label !== "string" || label.length === 0) throw new TypeError("wait label is required");
  if (typeof sample !== "function" || typeof accept !== "function") throw new TypeError("wait sample and accept functions are required");
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) throw new TypeError("wait timeout must be positive");
  if (!Number.isFinite(pollMs) || pollMs <= 0) throw new TypeError("wait poll interval must be positive");
  const started = now();
  const deadline = started + timeoutMs;
  let attempts = 0;
  let lastValue = null;
  let lastError = null;
  while (now() < deadline) {
    attempts += 1;
    try {
      const remaining = Math.max(1, deadline - now());
      const attempted = await beforeHardDeadline(async () => {
        const value = await sample();
        return { value, accepted: await accept(value) };
      }, remaining, label);
      lastValue = attempted.value;
      lastError = null;
      if (attempted.accepted) {
        return { value: lastValue, attempts, elapsed_ms: Math.max(0, now() - started) };
      }
    } catch (error) {
      lastError = error;
      if (error?.code === "observation-attempt-timeout") break;
      if (!retrySampleErrors) throw error;
    }
    const remaining = deadline - now();
    if (remaining > 0) await sleep(Math.min(pollMs, remaining));
  }
  const error = new Error(`${label} timed out after ${timeoutMs}ms`);
  error.code = "observation-timeout";
  error.evidence = {
    label,
    attempts,
    elapsed_ms: Math.max(0, now() - started),
    last_value: lastValue,
    last_error: lastError?.message ?? null,
  };
  throw error;
}

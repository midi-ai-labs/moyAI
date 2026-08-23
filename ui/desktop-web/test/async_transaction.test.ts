import assert from "node:assert/strict";
import test from "node:test";

import {
  asyncTransactionIsCurrent,
  beginAsyncTransaction,
  clearAsyncTransaction,
  createAsyncTransactionSlot,
} from "../src/async_transaction.ts";

interface RequestTarget {
  readonly owner: string;
  readonly revision: number;
}

interface Request {
  readonly token: number;
  readonly target: Readonly<RequestTarget>;
}

function begin(
  slot: ReturnType<typeof createAsyncTransactionSlot<Request>>,
  target: RequestTarget,
  policy: "single-flight" | "supersede",
): Request | null {
  if (policy === "supersede") {
    return beginAsyncTransaction(slot, target, policy, (token, immutableTarget) => ({
      token,
      target: immutableTarget,
    }));
  }
  return beginAsyncTransaction(slot, target, policy, (token, immutableTarget) => ({
    token,
    target: immutableTarget,
  }));
}

test("single-flight keeps the exact owner and does not consume another ID", () => {
  const slot = createAsyncTransactionSlot<Request>();
  const mutableTarget = { owner: "session-a", revision: 1 };
  const first = begin(slot, mutableTarget, "single-flight");
  assert.ok(first);

  mutableTarget.owner = "session-b";
  mutableTarget.revision = 2;
  assert.deepEqual(first.target, { owner: "session-a", revision: 1 });
  assert.equal(Object.isFrozen(first.target), true);
  assert.equal(begin(slot, { owner: "session-b", revision: 2 }, "single-flight"), null);
  assert.equal(slot.nextId, first.token + 1);
  assert.equal(slot.active, first);
});

test("supersede replaces the active owner and stale settlement cannot clear it", () => {
  const slot = createAsyncTransactionSlot<Request>();
  const first = begin(slot, { owner: "session-a", revision: 1 }, "supersede");
  const second = begin(slot, { owner: "session-b", revision: 1 }, "supersede");
  assert.ok(first && second);

  assert.equal(second.token, first.token + 1);
  assert.equal(asyncTransactionIsCurrent(slot, first), false);
  assert.equal(clearAsyncTransaction(slot, first), false);
  assert.equal(slot.active, second);
  assert.equal(clearAsyncTransaction(slot, second), true);
  assert.equal(slot.active, null);
});

test("request identity rejects an ABA clone with the same number and target", () => {
  const slot = createAsyncTransactionSlot<Request>();
  const request = begin(slot, { owner: "session-a", revision: 1 }, "single-flight");
  assert.ok(request);
  const clone = { ...request };

  assert.equal(asyncTransactionIsCurrent(slot, clone), false);
  assert.equal(clearAsyncTransaction(slot, clone), false);
  assert.equal(clearAsyncTransaction(slot, request), true);
});

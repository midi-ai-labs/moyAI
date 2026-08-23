export type AsyncTransactionPolicy = "single-flight" | "supersede";

/**
 * One feature-owned asynchronous request lane.
 *
 * The slot owns only monotonically increasing local request IDs and the exact
 * active request object. Feature adapters continue to own target comparison,
 * result validation, polling, cache updates, and interaction settlement.
 */
export interface AsyncTransactionSlot<TRequest> {
  nextId: number;
  active: TRequest | null;
}

export function createAsyncTransactionSlot<TRequest>(firstId = 1): AsyncTransactionSlot<TRequest> {
  return { nextId: firstId, active: null };
}

export function beginAsyncTransaction<TTarget extends object, TRequest>(
  slot: AsyncTransactionSlot<TRequest>,
  target: TTarget,
  policy: "supersede",
  createRequest: (id: number, target: Readonly<TTarget>) => TRequest,
): TRequest;
export function beginAsyncTransaction<TTarget extends object, TRequest>(
  slot: AsyncTransactionSlot<TRequest>,
  target: TTarget,
  policy: "single-flight",
  createRequest: (id: number, target: Readonly<TTarget>) => TRequest,
): TRequest | null;
export function beginAsyncTransaction<TTarget extends object, TRequest>(
  slot: AsyncTransactionSlot<TRequest>,
  target: TTarget,
  policy: AsyncTransactionPolicy,
  createRequest: (id: number, target: Readonly<TTarget>) => TRequest,
): TRequest | null {
  if (policy === "single-flight" && slot.active !== null) return null;

  const id = slot.nextId;
  const immutableTarget = Object.freeze({ ...target }) as Readonly<TTarget>;
  const request = createRequest(id, immutableTarget);
  slot.nextId = id + 1;
  slot.active = request;
  return request;
}

export function asyncTransactionIsCurrent<TRequest>(
  slot: Readonly<AsyncTransactionSlot<TRequest>>,
  request: TRequest,
): boolean {
  return slot.active === request;
}

export function clearAsyncTransaction<TRequest>(
  slot: AsyncTransactionSlot<TRequest>,
  request: TRequest,
): boolean {
  if (!asyncTransactionIsCurrent(slot, request)) return false;
  slot.active = null;
  return true;
}

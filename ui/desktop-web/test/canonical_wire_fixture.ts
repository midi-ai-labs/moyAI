import { isExactAgentInterruptTarget } from "../src/agent_interrupt_contract.ts";
import { isCanonicalU64, isCanonicalUlid } from "../src/canonical_identity.ts";
import type { AgentInterruptExpectedTarget } from "../src/types.ts";

const CROCKFORD_BASE32 = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const FIXTURE_TIMESTAMP_MS = 1_725_000_000_000n;
const FIXTURE_ENTROPY_BASE = 0x1234_5678_9abc_def0_1234n;
const MAX_ULID_ENTROPY = (1n << 80n) - 1n;

export const WORKSPACE_A = "C:/workspace";

export function canonicalFixtureUlid(sequence: bigint): string {
  const entropy = FIXTURE_ENTROPY_BASE + sequence;
  if (sequence < 0n || entropy > MAX_ULID_ENTROPY) {
    throw new RangeError("fixture ULID sequence is outside the canonical 80-bit entropy range");
  }
  let value = (FIXTURE_TIMESTAMP_MS << 80n) | entropy;
  let encoded = "";
  for (let index = 0; index < 26; index += 1) {
    encoded = CROCKFORD_BASE32[Number(value & 31n)]! + encoded;
    value >>= 5n;
  }
  if (value !== 0n || !isCanonicalUlid(encoded)) {
    throw new Error("fixture ULID encoder produced a non-canonical identity");
  }
  return encoded;
}

export function canonicalFixtureU64(value: bigint): string {
  const encoded = value.toString();
  if (!isCanonicalU64(encoded)) {
    throw new RangeError("fixture owner is outside the Rust u64 range");
  }
  return encoded;
}

export function runtimeOwnerToken(
  phase: "idle" | "root" | "tree",
  epoch: bigint,
): string {
  return `${phase}:${canonicalFixtureU64(epoch)}`;
}

export function turnIdForRuntimeEpoch(epoch: string): string {
  const value = BigInt(epoch);
  if (canonicalFixtureU64(value) !== epoch) {
    throw new Error("runtime owner epoch must use canonical Rust u64 spelling");
  }
  return canonicalFixtureUlid(10_000n + value);
}

export const SESSION_A = knownCanonicalUlid("01ARZ3NDEKTSV4RRFFQ69G5FAV");
export const SESSION_B = knownCanonicalUlid("01ARZ3NDEKTSV4RRFFQ69G5FAW");
export const QUICK_A = knownCanonicalUlid("01ARZ3NDEKTSV4RRFFQ69G5FAX");
export const TURN_A = knownCanonicalUlid("01ARZ3NDEKTSV4RRFFQ69G5FAY");
export const TURN_B = knownCanonicalUlid("01ARZ3NDEKTSV4RRFFQ69G5FAZ");

export function childSessionIdForOrder(order: number): string {
  if (!Number.isSafeInteger(order) || order < 0) {
    throw new RangeError("fixture child order must be a non-negative safe integer");
  }
  return canonicalFixtureUlid(1_000n + BigInt(order));
}

export function childTurnIdForOrder(order: number): string {
  if (!Number.isSafeInteger(order) || order < 0) {
    throw new RangeError("fixture child order must be a non-negative safe integer");
  }
  return canonicalFixtureUlid(2_000n + BigInt(order));
}

export function agentInterruptTarget(
  overrides: Partial<AgentInterruptExpectedTarget>
    & Pick<AgentInterruptExpectedTarget, "agentPath" | "childSessionId" | "expectedTurnId">,
): AgentInterruptExpectedTarget {
  const target = {
    workspacePath: WORKSPACE_A,
    rootSessionId: SESSION_A,
    admissionRevision: canonicalFixtureU64(1n),
    ...overrides,
  } satisfies AgentInterruptExpectedTarget;
  if (!isExactAgentInterruptTarget(target)) {
    throw new Error("AgentInterrupt fixture must satisfy the exact Rust wire contract");
  }
  return target;
}

function knownCanonicalUlid(value: string): string {
  if (!isCanonicalUlid(value)) throw new Error("known fixture identity is not a canonical ULID");
  return value;
}

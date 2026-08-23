import {
  isCanonicalOptionalUlid,
  isCanonicalU64,
  isCanonicalUlid,
  isCanonicalWorkspace,
} from "./canonical_identity.ts";
import type {
  DraftActionTarget,
  PromptReviewMutationTarget,
  RunExpectedState,
} from "./types.ts";

function exactKeys(value: Record<string, unknown>, expected: readonly string[]): boolean {
  return JSON.stringify(Object.keys(value).sort()) === JSON.stringify([...expected].sort());
}

export function isExactDraftActionTarget(value: unknown): value is DraftActionTarget {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return false;
  const target = value as Record<string, unknown>;
  return exactKeys(target, ["ownerGeneration", "sessionId", "workspacePath"])
    && isCanonicalWorkspace(target.workspacePath)
    && isCanonicalOptionalUlid(target.sessionId)
    && isCanonicalU64(target.ownerGeneration);
}

export function isExactRunExpectedState(value: unknown): value is RunExpectedState {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return false;
  const state = value as Record<string, unknown>;
  if (state.kind === "idle") {
    return exactKeys(state, ["admissionRevision", "kind", "latestTurnId"])
      && isCanonicalOptionalUlid(state.latestTurnId)
      && isCanonicalU64(state.admissionRevision);
  }
  if (state.kind === "turn") {
    return exactKeys(state, ["admissionRevision", "kind", "turnId"])
      && isCanonicalUlid(state.turnId)
      && isCanonicalU64(state.admissionRevision);
  }
  return false;
}

export function isExactPromptReviewMutationTarget(
  value: unknown,
): value is PromptReviewMutationTarget {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return false;
  const target = value as Record<string, unknown>;
  return exactKeys(target, [
    "expectedState",
    "ownerGeneration",
    "requestId",
    "sessionId",
    "workspacePath",
  ])
    && isExactDraftActionTarget({
      workspacePath: target.workspacePath,
      sessionId: target.sessionId,
      ownerGeneration: target.ownerGeneration,
    })
    && isCanonicalU64(target.requestId)
    && isExactRunExpectedState(target.expectedState);
}

export function snapshotDraftActionTarget(value: unknown): DraftActionTarget {
  if (!isExactDraftActionTarget(value)) {
    throw new TypeError("composer mutation requires one exact canonical draft owner");
  }
  return structuredClone(value);
}

export function snapshotPromptReviewMutationTarget(
  value: unknown,
): PromptReviewMutationTarget | null {
  return isExactPromptReviewMutationTarget(value) ? structuredClone(value) : null;
}

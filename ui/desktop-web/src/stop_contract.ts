import { command } from "./api.ts";
import {
  isCanonicalOptionalUlid,
  isCanonicalU64,
  isCanonicalUlid,
  isCanonicalWorkspace,
} from "./canonical_identity.ts";
import type { StopMutationTarget } from "./types.ts";

function exactKeys(value: Record<string, unknown>, expected: readonly string[]): boolean {
  return JSON.stringify(Object.keys(value).sort()) === JSON.stringify([...expected].sort());
}

function canonicalPermissionId(value: unknown): value is string | null {
  return value === null || isCanonicalU64(value);
}

export function isExactStopMutationTarget(value: unknown): value is StopMutationTarget {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return false;
  const target = value as Record<string, unknown>;
  if (target.kind === "root") {
    return exactKeys(target, [
      "admissionRevision",
      "kind",
      "latestTurnId",
      "permissionConfirmationId",
      "rootGeneration",
      "sessionId",
      "workspacePath",
    ])
      && isCanonicalWorkspace(target.workspacePath)
      && isCanonicalOptionalUlid(target.sessionId)
      && isCanonicalU64(target.rootGeneration)
      && isCanonicalOptionalUlid(target.latestTurnId)
      && isCanonicalU64(target.admissionRevision)
      && canonicalPermissionId(target.permissionConfirmationId);
  }
  if (target.kind === "turn") {
    return exactKeys(target, [
      "admissionRevision",
      "kind",
      "rootEpoch",
      "sessionId",
      "turnId",
      "workspacePath",
    ])
      && isCanonicalWorkspace(target.workspacePath)
      && isCanonicalUlid(target.sessionId)
      && isCanonicalUlid(target.turnId)
      && isCanonicalU64(target.admissionRevision)
      && isCanonicalU64(target.rootEpoch);
  }
  return false;
}

export function assertExactStopMutationTarget(value: unknown): asserts value is StopMutationTarget {
  if (!isExactStopMutationTarget(value)) {
    throw new TypeError("cancel_run requires one exact canonical Stop mutation target");
  }
}

export async function cancelRunCommand<T>(expectedTarget: StopMutationTarget): Promise<T> {
  assertExactStopMutationTarget(expectedTarget);
  return command<T>("cancel_run", {
    expectedTarget: structuredClone(expectedTarget),
  });
}

export async function interruptSessionCommand<T>(args: Record<string, unknown>): Promise<T> {
  const expectedTarget = args.expectedStopTarget;
  assertExactStopMutationTarget(expectedTarget);
  return command<T>("interrupt_session", {
    ...args,
    expectedStopTarget: structuredClone(expectedTarget),
  });
}

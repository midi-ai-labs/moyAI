import type { AgentInterruptExpectedTarget } from "./types.ts";
import {
  isCanonicalU64,
  isCanonicalUlid,
  isCanonicalWorkspace,
} from "./canonical_identity.ts";

export function isExactAgentInterruptTarget(
  value: unknown,
): value is AgentInterruptExpectedTarget {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return false;
  const target = value as Record<string, unknown>;
  const keys = Object.keys(target).sort();
  const expectedKeys = [
    "admissionRevision",
    "agentPath",
    "childSessionId",
    "expectedTurnId",
    "rootSessionId",
    "workspacePath",
  ].sort();
  return JSON.stringify(keys) === JSON.stringify(expectedKeys)
    && isCanonicalWorkspace(target.workspacePath)
    && isCanonicalUlid(target.rootSessionId)
    && typeof target.agentPath === "string"
    && target.agentPath.startsWith("/root/")
    && isCanonicalUlid(target.childSessionId)
    && isCanonicalUlid(target.expectedTurnId)
    && isCanonicalU64(target.admissionRevision);
}

export function snapshotAgentInterruptTarget(
  value: unknown,
): AgentInterruptExpectedTarget | null {
  return isExactAgentInterruptTarget(value) ? structuredClone(value) : null;
}

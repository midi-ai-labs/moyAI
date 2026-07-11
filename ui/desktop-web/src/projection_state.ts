export function projectionUpdateAccepted(
  lastAppliedRevision: string,
  candidateRevision: string,
  currentObject: boolean,
): boolean {
  const comparison = compareProjectionRevisions(candidateRevision, lastAppliedRevision);
  if (comparison === null) return false;
  return currentObject ? comparison >= 0 : comparison > 0;
}

export function appliedProjectionRevision(
  lastAppliedRevision: string,
  candidateRevision: string,
): string {
  const comparison = compareProjectionRevisions(candidateRevision, lastAppliedRevision);
  return comparison !== null && comparison > 0 ? canonicalProjectionRevision(candidateRevision) : lastAppliedRevision;
}

export function deferredProjectionCandidatePreferred(
  currentRevision: string,
  candidateRevision: string,
  currentSequence: number,
  candidateSequence: number,
): boolean {
  const comparison = compareProjectionRevisions(candidateRevision, currentRevision);
  if (comparison === null) return false;
  return comparison > 0 || (comparison === 0 && candidateSequence > currentSequence);
}

export function isProjectionRevision(value: unknown): value is string {
  if (typeof value !== "string" || !/^(0|[1-9]\d*)$/.test(value)) return false;
  try {
    return BigInt(value) <= 18_446_744_073_709_551_615n;
  } catch {
    return false;
  }
}

function compareProjectionRevisions(left: string, right: string): number | null {
  if (!isProjectionRevision(left) || !isProjectionRevision(right)) return null;
  const normalizedLeft = canonicalProjectionRevision(left);
  const normalizedRight = canonicalProjectionRevision(right);
  if (normalizedLeft.length !== normalizedRight.length) {
    return normalizedLeft.length > normalizedRight.length ? 1 : -1;
  }
  return normalizedLeft === normalizedRight ? 0 : normalizedLeft > normalizedRight ? 1 : -1;
}

function canonicalProjectionRevision(value: string): string {
  return BigInt(value).toString(10);
}

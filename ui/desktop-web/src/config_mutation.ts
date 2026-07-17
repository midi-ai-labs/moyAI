import type { ConfigMutationTarget } from "./types.ts";

export type { ConfigMutationTarget } from "./types.ts";

export interface ConfigValueInput {
  key: string;
  text: string;
}

export interface ConfigMutationOwner {
  configDirty: boolean;
  configDraftValues: Map<string, string>;
  configDraftBaselineValues: Map<string, string>;
  configDraftTarget: ConfigMutationTarget | null;
  configDraftRevision: bigint;
  nextConfigMutationGeneration: bigint;
  activeConfigMutationGeneration: bigint | null;
}

export interface ConfigMutationRequest {
  generation: bigint;
  draftRevision: bigint;
  target: ConfigMutationTarget;
}

export function updateConfigDraftValue(
  owner: ConfigMutationOwner,
  target: ConfigMutationTarget,
  baseValues: ConfigValueInput[],
  key: string,
  text: string,
): void {
  if (!sameConfigMutationTarget(target, owner.configDraftTarget)) {
    clearConfigDraft(owner);
    owner.configDraftTarget = { ...target };
  }
  for (const value of baseValues) {
    if (!owner.configDraftValues.has(value.key)) owner.configDraftValues.set(value.key, value.text);
    if (!owner.configDraftBaselineValues.has(value.key)) {
      owner.configDraftBaselineValues.set(value.key, value.text);
    }
  }
  owner.configDraftValues.set(key, text);
  owner.configDirty = Array.from(owner.configDraftValues).some(
    ([fieldKey, fieldValue]) => owner.configDraftBaselineValues.get(fieldKey) !== fieldValue,
  );
  owner.configDraftRevision += 1n;
  if (!owner.configDirty) resetConfigDraftStorage(owner);
}

export function configMutationValues(
  owner: ConfigMutationOwner,
  target: ConfigMutationTarget,
): ConfigValueInput[] | null {
  if (!configDraftAppliesTo(owner, target)) return null;
  return Array.from(owner.configDraftValues, ([key, text]) => ({ key, text }));
}

export function reconcileConfigDraftTarget(
  owner: ConfigMutationOwner,
  currentTarget: ConfigMutationTarget,
): boolean {
  if (!owner.configDirty) return true;
  if (sameConfigMutationTarget(currentTarget, owner.configDraftTarget)) return true;
  clearConfigDraft(owner);
  return false;
}

export function configDraftAppliesTo(
  owner: ConfigMutationOwner,
  currentTarget: ConfigMutationTarget,
): boolean {
  return owner.configDirty && sameConfigMutationTarget(currentTarget, owner.configDraftTarget);
}

export function beginConfigMutation(
  owner: ConfigMutationOwner,
  target: ConfigMutationTarget,
): ConfigMutationRequest {
  reconcileConfigDraftTarget(owner, target);
  const generation = owner.nextConfigMutationGeneration;
  owner.nextConfigMutationGeneration += 1n;
  owner.activeConfigMutationGeneration = generation;
  return { generation, draftRevision: owner.configDraftRevision, target: { ...target } };
}

export function finishConfigMutation(
  owner: ConfigMutationOwner,
  request: ConfigMutationRequest,
  succeeded: boolean,
  settlementTarget: ConfigMutationTarget,
  currentTarget: ConfigMutationTarget | null,
): boolean {
  if (owner.activeConfigMutationGeneration !== request.generation) return false;
  owner.activeConfigMutationGeneration = null;
  if (!sameConfigOwnerIdentity(request.target, settlementTarget)) return false;
  if (
    !sameConfigMutationTarget(request.target, currentTarget)
    && !sameConfigMutationTarget(settlementTarget, currentTarget)
  ) return false;
  if (
    succeeded
    && owner.configDraftRevision === request.draftRevision
    && configDraftAppliesTo(owner, request.target)
  ) {
    clearConfigDraft(owner);
  }
  return true;
}

export function sameConfigMutationTarget(
  expected: ConfigMutationTarget,
  actual: ConfigMutationTarget | null,
): boolean {
  return actual !== null
    && expected.workspacePath === actual.workspacePath
    && expected.sessionId === actual.sessionId
    && expected.configGeneration === actual.configGeneration;
}

function sameConfigOwnerIdentity(expected: ConfigMutationTarget, actual: ConfigMutationTarget): boolean {
  return expected.workspacePath === actual.workspacePath
    && expected.sessionId === actual.sessionId;
}

export function configMutationPending(owner: ConfigMutationOwner): boolean {
  return owner.activeConfigMutationGeneration !== null;
}

export function discardConfigDraft(owner: ConfigMutationOwner): void {
  clearConfigDraft(owner);
}

function clearConfigDraft(owner: ConfigMutationOwner): void {
  owner.configDirty = false;
  resetConfigDraftStorage(owner);
  owner.configDraftRevision += 1n;
}

function resetConfigDraftStorage(owner: ConfigMutationOwner): void {
  owner.configDraftValues.clear();
  owner.configDraftBaselineValues.clear();
  owner.configDraftTarget = null;
}

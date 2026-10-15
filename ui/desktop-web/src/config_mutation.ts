import type { ConfigMutationTarget } from "./types.ts";
import { validateProviderBaseUrl } from "./utils.ts";

export type { ConfigMutationTarget } from "./types.ts";

export interface ConfigValueInput {
  key: string;
  text: string;
}

export interface ConfigMutationOwner {
  configDirty: boolean;
  configDraftValues: Map<string, string>;
  configDraftBaselineValues: Map<string, string>;
  configDraftEditedKeys?: Set<string>;
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
  const role = key.split(".")[0];
  const connectionKey = key === `${role}.base_url` || key === `${role}.provider_profile`;
  const previous = owner.configDraftValues.get(key) ?? "";
  if (connectionKey && ["model", "side_chat", "approve"].includes(role)
    && connectionValueChanged(key, previous, text)) {
    const privateKeys = role === "model"
      ? ["model.api_key_env", "model.extra_headers_json", "model.extra_body_json"]
      : [`${role}.api_key_env`];
    for (const privateKey of privateKeys) {
      const baseline = owner.configDraftBaselineValues.get(privateKey);
      if (baseline !== undefined) owner.configDraftValues.set(privateKey, baseline);
      owner.configDraftEditedKeys?.delete(privateKey);
    }
  }
  owner.configDraftValues.set(key, text);
  (owner.configDraftEditedKeys ??= new Set()).add(key);
  owner.configDirty = Array.from(owner.configDraftValues).some(
    ([fieldKey, fieldValue]) => owner.configDraftBaselineValues.get(fieldKey) !== fieldValue,
  );
  owner.configDraftRevision += 1n;
  if (!owner.configDirty) resetConfigDraftStorage(owner);
}

function connectionValueChanged(key: string, previous: string, next: string): boolean {
  if (!key.endsWith(".base_url")) return previous.trim() !== next.trim();
  const before = validateProviderBaseUrl(previous);
  const after = validateProviderBaseUrl(next);
  if (!before.ok || !after.ok) return previous.trim() !== next.trim();
  return before.canonicalBaseUrl.replace(/\/v1$/, "")
    !== after.canonicalBaseUrl.replace(/\/v1$/, "");
}

/**
 * Atomically replaces the complete browser draft after a read-only validated import.
 * Missing, duplicated, or unknown fields are rejected without touching the current draft.
 */
export function replaceCompleteConfigDraft(
  owner: ConfigMutationOwner,
  target: ConfigMutationTarget,
  baselineValues: readonly ConfigValueInput[],
  importedValues: readonly ConfigValueInput[],
): boolean {
  const baseline = completeConfigValueMap(baselineValues);
  const imported = completeConfigValueMap(importedValues);
  if (
    baseline === null
    || imported === null
    || baseline.size !== imported.size
    || Array.from(baseline.keys()).some((key) => !imported.has(key))
  ) return false;

  owner.configDraftValues.clear();
  owner.configDraftBaselineValues.clear();
  owner.configDraftEditedKeys?.clear();
  for (const [key, baselineText] of baseline) {
    owner.configDraftBaselineValues.set(key, baselineText);
    owner.configDraftValues.set(key, imported.get(key)!);
    if (key === "model.api_key_env" || key === "side_chat.api_key_env"
      || key === "model.extra_headers_json" || key === "model.extra_body_json") {
      (owner.configDraftEditedKeys ??= new Set()).add(key);
    }
  }
  owner.configDraftTarget = { ...target };
  owner.configDirty = Array.from(owner.configDraftValues).some(
    ([key, text]) => baseline.get(key) !== text,
  );
  owner.configDraftRevision += 1n;
  if (!owner.configDirty) resetConfigDraftStorage(owner);
  return true;
}

export function configMutationValues(
  owner: ConfigMutationOwner,
  target: ConfigMutationTarget,
): ConfigValueInput[] | null {
  if (!configDraftAppliesTo(owner, target)) return null;
  return Array.from(owner.configDraftValues, ([key, text]) => ({ key, text }));
}

/** Only an edited credential grants its use to a changed provider connection. */
export function configCommandValues(
  owner: ConfigMutationOwner,
  target: ConfigMutationTarget,
  values: readonly ConfigValueInput[],
): ConfigValueInput[] {
  const draftApplies = configDraftAppliesTo(owner, target);
  const privateKeys = new Set(["model.api_key_env", "model.extra_headers_json", "model.extra_body_json",
    "side_chat.api_key_env", "approve.api_key_env"]);
  return values.filter(({ key, text }) => {
    if (!key.startsWith("approve.") && !privateKeys.has(key)) return true;
    if (!draftApplies) return false;
    if (owner.configDraftBaselineValues.get(key) !== text) return true;
    const role = key.split(".")[0];
    const connectionChanged = values.some(value =>
      (value.key === `${role}.base_url` || value.key === `${role}.provider_profile`)
      && owner.configDraftBaselineValues.get(value.key) !== value.text);
    return privateKeys.has(key) && connectionChanged && !!owner.configDraftEditedKeys?.has(key);
  });
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
  owner.configDraftEditedKeys?.clear();
  owner.configDraftTarget = null;
}

function completeConfigValueMap(
  values: readonly ConfigValueInput[],
): Map<string, string> | null {
  const result = new Map<string, string>();
  for (const value of values) {
    if (!value.key || result.has(value.key)) return null;
    result.set(value.key, value.text);
  }
  return result;
}

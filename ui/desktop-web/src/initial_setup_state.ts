import type {
  ConfigFieldProjection,
  ConfigMutationTarget,
  InitialSetupMutationTarget,
} from "./types.ts";
import { validateConfigInput, type ConfigFieldValue } from "./utils.ts";

export const INITIAL_SETUP_STEPS = [
  "start",
  "provider",
  "model",
  "permissions",
  "tools",
  "finish",
] as const;

export type InitialSetupStep = (typeof INITIAL_SETUP_STEPS)[number];

export interface InitialSetupFinishRequest {
  token: bigint;
  setupTarget: Readonly<InitialSetupMutationTarget>;
  configTarget: Readonly<ConfigMutationTarget>;
  draftRevision: bigint;
}

export interface InitialSetupState {
  owner: InitialSetupMutationTarget | null;
  step: InitialSetupStep;
  nextFinishToken: bigint;
  activeFinish: InitialSetupFinishRequest | null;
}

export interface InitialSetupStepValidation {
  ok: boolean;
  invalidKey: string | null;
  message: string;
}

export interface InitialSetupDiffEntry {
  key: string;
  before: string;
  after: string;
}

export interface InitialSetupFinishSettlement {
  succeeded: boolean;
  setupTarget: InitialSetupMutationTarget | null;
  configTarget: ConfigMutationTarget;
}

const PROVIDER_STEP_KEYS = new Set([
  "model.base_url",
  "model.provider_profile",
  "model.api_key_env",
  "model.context_window",
  "model.max_output_tokens",
]);

export function createInitialSetupState(): InitialSetupState {
  return {
    owner: null,
    step: "start",
    nextFinishToken: 1n,
    activeFinish: null,
  };
}

/**
 * Reconciles the wizard with the exact Rust setup-lifecycle owner.
 *
 * A setup generation change is a rebase even when the workspace/config-path
 * pair stays the same. Resetting navigation and the active finish lane
 * prevents an old draft from being presented as the new owner's review.
 */
export function reconcileInitialSetupOwner(
  state: InitialSetupState,
  target: InitialSetupMutationTarget,
): boolean {
  if (sameInitialSetupTarget(state.owner, target)) return true;
  state.owner = { ...target };
  state.step = "start";
  state.activeFinish = null;
  return false;
}

export function sameInitialSetupTarget(
  expected: InitialSetupMutationTarget | null,
  actual: InitialSetupMutationTarget | null,
): boolean {
  return expected !== null
    && actual !== null
    && expected.workspacePath === actual.workspacePath
    && expected.globalConfigPath === actual.globalConfigPath
    && expected.setupGeneration === actual.setupGeneration;
}

export function sameInitialSetupOwnerIdentity(
  expected: InitialSetupMutationTarget,
  actual: InitialSetupMutationTarget,
): boolean {
  return expected.workspacePath === actual.workspacePath
    && expected.globalConfigPath === actual.globalConfigPath;
}

export function initialSetupStepIndex(step: InitialSetupStep): number {
  return INITIAL_SETUP_STEPS.indexOf(step);
}

export function advanceInitialSetupStep(
  state: InitialSetupState,
  fields: readonly ConfigFieldProjection[],
  values: readonly ConfigFieldValue[],
): InitialSetupStepValidation {
  const validation = validateInitialSetupStep(state.step, fields, values);
  if (!validation.ok || state.activeFinish !== null) return validation;
  const index = initialSetupStepIndex(state.step);
  if (index < INITIAL_SETUP_STEPS.length - 1) state.step = INITIAL_SETUP_STEPS[index + 1];
  return validation;
}

export function retreatInitialSetupStep(state: InitialSetupState): boolean {
  if (state.activeFinish !== null) return false;
  const index = initialSetupStepIndex(state.step);
  if (index <= 0) return false;
  state.step = INITIAL_SETUP_STEPS[index - 1];
  return true;
}

/**
 * Validates only local typed config. Provider catalogs and readiness results
 * are deliberately absent from this boundary and therefore cannot gate Next.
 */
export function validateInitialSetupStep(
  step: InitialSetupStep,
  fields: readonly ConfigFieldProjection[],
  values: readonly ConfigFieldValue[],
): InitialSetupStepValidation {
  const valuesByKey = new Map(values.map((value) => [value.key, value.text]));
  const contextualValues = fields.map((field) => ({
    key: field.key,
    text: valuesByKey.get(field.key) ?? field.value,
  }));

  for (const field of fields) {
    if (!fieldBelongsToInitialSetupStep(field.key, step)) continue;
    const value = valuesByKey.get(field.key) ?? field.value;
    const validation = validateConfigInput(field, value, contextualValues);
    if (!validation.ok) {
      return {
        ok: false,
        invalidKey: field.key,
        message: validation.message,
      };
    }
  }
  return { ok: true, invalidKey: null, message: "入力形式は問題ありません。" };
}

export function initialSetupDiffSummary(
  fields: readonly ConfigFieldProjection[],
  baselineValues: readonly ConfigFieldValue[],
  draftValues: readonly ConfigFieldValue[],
): InitialSetupDiffEntry[] {
  const baseline = new Map(baselineValues.map((value) => [value.key, value.text]));
  const draft = new Map(draftValues.map((value) => [value.key, value.text]));
  const differences: InitialSetupDiffEntry[] = [];
  for (const field of fields) {
    const before = baseline.get(field.key) ?? field.value;
    const after = draft.get(field.key) ?? field.value;
    if (before !== after) differences.push({ key: field.key, before, after });
  }
  return differences;
}

export function beginInitialSetupFinish(
  state: InitialSetupState,
  setupTarget: InitialSetupMutationTarget,
  configTarget: ConfigMutationTarget,
  draftRevision: bigint,
  fields: readonly ConfigFieldProjection[],
  values: readonly ConfigFieldValue[],
): InitialSetupFinishRequest | null {
  if (state.step !== "finish" || state.activeFinish !== null) return null;
  if (!sameInitialSetupTarget(state.owner, setupTarget)) return null;
  if (!validateInitialSetupStep("finish", fields, values).ok) return null;

  const request: InitialSetupFinishRequest = {
    token: state.nextFinishToken,
    setupTarget: Object.freeze({ ...setupTarget }),
    configTarget: Object.freeze({ ...configTarget }),
    draftRevision,
  };
  state.nextFinishToken += 1n;
  state.activeFinish = request;
  return request;
}

/**
 * Accepts a finish result only for the active token, exact setup/config begin
 * targets, and the unchanged external config-draft revision. Success must
 * close the setup owner; its returned config generation may advance only
 * within the same workspace/session identity.
 */
export function finishInitialSetup(
  state: InitialSetupState,
  request: InitialSetupFinishRequest,
  currentConfigTarget: ConfigMutationTarget,
  currentDraftRevision: bigint,
  settlement: InitialSetupFinishSettlement,
): boolean {
  const active = state.activeFinish;
  if (active === null || active.token !== request.token) return false;
  state.activeFinish = null;
  if (!sameFinishRequest(active, request)) return false;
  if (!sameInitialSetupTarget(state.owner, request.setupTarget)) return false;
  if (!sameConfigTarget(request.configTarget, currentConfigTarget)) return false;
  if (currentDraftRevision !== request.draftRevision) return false;
  if (settlement.succeeded) {
    if (settlement.setupTarget !== null) return false;
    if (!sameConfigOwnerIdentity(request.configTarget, settlement.configTarget)) return false;
    state.owner = null;
    return true;
  }
  if (!sameInitialSetupTarget(request.setupTarget, settlement.setupTarget)) return false;
  return sameConfigTarget(request.configTarget, settlement.configTarget);
}

export function initialSetupFinishPending(state: InitialSetupState): boolean {
  return state.activeFinish !== null;
}

function fieldBelongsToInitialSetupStep(key: string, step: InitialSetupStep): boolean {
  switch (step) {
    case "start":
      return false;
    case "provider":
      return PROVIDER_STEP_KEYS.has(key);
    case "model":
      return key.startsWith("model.") && !PROVIDER_STEP_KEYS.has(key);
    case "permissions":
      return key === "permissions.access_mode";
    case "tools":
      return key.startsWith("docling.") || key.startsWith("mcp.");
    case "finish":
      return true;
  }
}

function sameFinishRequest(
  expected: InitialSetupFinishRequest,
  actual: InitialSetupFinishRequest,
): boolean {
  return expected.token === actual.token
    && expected.draftRevision === actual.draftRevision
    && sameInitialSetupTarget(expected.setupTarget, actual.setupTarget)
    && sameConfigTarget(expected.configTarget, actual.configTarget);
}

function sameConfigTarget(
  expected: ConfigMutationTarget,
  actual: ConfigMutationTarget,
): boolean {
  return expected.workspacePath === actual.workspacePath
    && expected.sessionId === actual.sessionId
    && expected.configGeneration === actual.configGeneration;
}

function sameConfigOwnerIdentity(
  expected: ConfigMutationTarget,
  actual: ConfigMutationTarget,
): boolean {
  return expected.workspacePath === actual.workspacePath
    && expected.sessionId === actual.sessionId;
}

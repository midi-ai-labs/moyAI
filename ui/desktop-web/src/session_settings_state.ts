import type {
  ConfigFieldProjection,
  ProviderProfile,
  SessionSettingsMutationTarget,
} from "./types.ts";
import { validateConfigInput } from "./utils.ts";

export type SessionAccessMode = "default" | "auto_review" | "full_access";

export type SessionSettingsTarget = SessionSettingsMutationTarget;

export interface SessionSettingsDraft {
  baseUrl: string;
  model: string;
  providerProfile: ProviderProfile;
  apiKeyEnv: string;
  contextWindow: string;
  maxOutputTokens: string;
  accessMode: SessionAccessMode;
}

export type SessionSettingsDraftField = keyof SessionSettingsDraft;

export interface SessionSettingsFieldValidation {
  ok: boolean;
  message: string;
}

export interface SessionSettingsValidation {
  ok: boolean;
  invalidField: SessionSettingsDraftField | null;
  message: string;
  fields: Readonly<Record<SessionSettingsDraftField, SessionSettingsFieldValidation>>;
}

export interface SessionSettingsMutationRequest {
  token: bigint;
  target: Readonly<SessionSettingsTarget>;
  draftRevision: bigint;
  draft: Readonly<SessionSettingsDraft>;
}

export interface SessionSettingsMutationSettlement {
  succeeded: boolean;
  target: SessionSettingsTarget;
  values: SessionSettingsDraft;
}

export interface SessionSettingsState {
  owner: SessionSettingsTarget | null;
  baseline: SessionSettingsDraft | null;
  draft: SessionSettingsDraft | null;
  dirty: boolean;
  validation: SessionSettingsValidation | null;
  draftRevision: bigint;
  nextMutationToken: bigint;
  activeMutation: SessionSettingsMutationRequest | null;
}

const U32_MAX = 4_294_967_295;

const SESSION_FIELD_DESCRIPTORS: Readonly<Record<SessionSettingsDraftField, ConfigFieldProjection>> = {
  baseUrl: configField("model.base_url", "string", true, null, null, []),
  model: configField("model.model", "string", true, null, null, []),
  providerProfile: configField(
    "model.provider_profile",
    "enum",
    true,
    null,
    null,
    ["lm_studio", "openai_compatible", "openai_responses", "lm_studio_chat_completions"],
  ),
  apiKeyEnv: configField("model.api_key_env", "string", false, null, null, []),
  contextWindow: configField("model.context_window", "integer", false, 1, U32_MAX, []),
  maxOutputTokens: configField("model.max_output_tokens", "integer", false, 0, U32_MAX, []),
  accessMode: configField(
    "permissions.access_mode",
    "enum",
    true,
    null,
    null,
    ["default", "auto_review", "full_access"],
  ),
};

export function createSessionSettingsState(): SessionSettingsState {
  return {
    owner: null,
    baseline: null,
    draft: null,
    dirty: false,
    validation: null,
    draftRevision: 0n,
    nextMutationToken: 1n,
    activeMutation: null,
  };
}

/**
 * Keeps a dirty browser draft attached to its root-session identity while exact mutation fences
 * advance. When the same root still projects the baseline values, only those fences are adopted.
 * A concurrent canonical value change leaves the old owner and connected draft visible as stale,
 * so Apply fails closed until the user discards or reopens. A different root always rebases.
 */
export function reconcileSessionSettings(
  state: SessionSettingsState,
  target: SessionSettingsTarget,
  values: SessionSettingsDraft,
): boolean {
  if (sameSessionSettingsTarget(state.owner, target)) {
    if (state.dirty) return true;
    if (state.draft !== null && sameSessionSettingsDraft(state.draft, values)) return true;
  }
  if (
    state.owner !== null
    && sameSessionSettingsRootOwner(state.owner, target)
    && state.dirty
    && state.baseline !== null
  ) {
    state.activeMutation = null;
    if (sameSessionSettingsDraft(state.baseline, values)) {
      // Runtime epochs and config generations are mutation fences, not draft owners. When the
      // canonical surfaced values did not change, advance the exact fence without replacing the
      // user's connected form or its dirty draft.
      state.owner = { ...target };
    }
    // If canonical values changed too, preserve the old draft/owner as a visible stale edit. The
    // action boundary rejects it until the user discards or reopens; polling must not erase input.
    return true;
  }
  replaceSessionSettingsOwner(state, target, values);
  return false;
}

export function sameSessionSettingsTarget(
  expected: SessionSettingsTarget | null,
  actual: SessionSettingsTarget | null,
): boolean {
  return expected !== null
    && actual !== null
    && expected.workspacePath === actual.workspacePath
    && expected.rootSessionId === actual.rootSessionId
    && expected.settingsRevision === actual.settingsRevision
    && expected.configGeneration === actual.configGeneration
    && expected.runtimeOwnerToken === actual.runtimeOwnerToken;
}

export function sameSessionSettingsRootOwner(
  expected: SessionSettingsTarget,
  actual: SessionSettingsTarget,
): boolean {
  return expected.workspacePath === actual.workspacePath
    && expected.rootSessionId === actual.rootSessionId;
}

export function updateSessionSettingsDraft<K extends SessionSettingsDraftField>(
  state: SessionSettingsState,
  target: SessionSettingsTarget,
  field: K,
  value: SessionSettingsDraft[K],
): boolean {
  if (!sameSessionSettingsTarget(state.owner, target) || state.draft === null) return false;
  if (state.draft[field] === value) return true;
  state.draft = { ...state.draft, [field]: value };
  state.draftRevision += 1n;
  refreshSessionSettingsDerivedState(state);
  return true;
}

export function discardSessionSettingsDraft(
  state: SessionSettingsState,
  target: SessionSettingsTarget,
): boolean {
  if (
    !sameSessionSettingsTarget(state.owner, target)
    || state.baseline === null
    || state.activeMutation !== null
  ) return false;
  state.draft = { ...state.baseline };
  state.draftRevision += 1n;
  refreshSessionSettingsDerivedState(state);
  return true;
}

export function clearSessionSettings(state: SessionSettingsState): void {
  state.owner = null;
  state.baseline = null;
  state.draft = null;
  state.dirty = false;
  state.validation = null;
  state.draftRevision += 1n;
  state.activeMutation = null;
}

export function validateSessionSettingsDraft(
  draft: SessionSettingsDraft,
): SessionSettingsValidation {
  const validations: Record<SessionSettingsDraftField, SessionSettingsFieldValidation> = {
    baseUrl: validateConfigInput(SESSION_FIELD_DESCRIPTORS.baseUrl, draft.baseUrl),
    model: validateConfigInput(SESSION_FIELD_DESCRIPTORS.model, draft.model),
    providerProfile: validateConfigInput(
      SESSION_FIELD_DESCRIPTORS.providerProfile,
      draft.providerProfile,
    ),
    apiKeyEnv: validateConfigInput(SESSION_FIELD_DESCRIPTORS.apiKeyEnv, draft.apiKeyEnv),
    contextWindow: validateConfigInput(
      SESSION_FIELD_DESCRIPTORS.contextWindow,
      draft.contextWindow,
    ),
    maxOutputTokens: validateConfigInput(
      SESSION_FIELD_DESCRIPTORS.maxOutputTokens,
      draft.maxOutputTokens,
    ),
    accessMode: validateConfigInput(SESSION_FIELD_DESCRIPTORS.accessMode, draft.accessMode),
  };
  const orderedFields: readonly SessionSettingsDraftField[] = [
    "baseUrl",
    "providerProfile",
    "apiKeyEnv",
    "model",
    "contextWindow",
    "maxOutputTokens",
    "accessMode",
  ];
  const invalidField = orderedFields.find((field) => !validations[field].ok) ?? null;
  return {
    ok: invalidField === null,
    invalidField,
    message: invalidField === null
      ? "入力形式は問題ありません。"
      : validations[invalidField].message,
    fields: validations,
  };
}

export function sessionSettingsApplyEnabled(state: SessionSettingsState): boolean {
  return state.owner !== null
    && state.draft !== null
    && state.dirty
    && state.validation?.ok === true
    && state.activeMutation === null;
}

export function beginSessionSettingsMutation(
  state: SessionSettingsState,
  target: SessionSettingsTarget,
): SessionSettingsMutationRequest | null {
  if (!sameSessionSettingsTarget(state.owner, target)) return null;
  if (!sessionSettingsApplyEnabled(state) || state.draft === null) return null;
  const request: SessionSettingsMutationRequest = {
    token: state.nextMutationToken,
    target: Object.freeze({ ...target }),
    draftRevision: state.draftRevision,
    draft: Object.freeze({ ...state.draft }),
  };
  state.nextMutationToken += 1n;
  state.activeMutation = request;
  return request;
}

/**
 * The active request must still own the exact pre-command target and draft
 * revision. Only then may a successful Rust projection advance the target and
 * replace the baseline with its canonical values.
 */
export function finishSessionSettingsMutation(
  state: SessionSettingsState,
  request: SessionSettingsMutationRequest,
  settlement: SessionSettingsMutationSettlement,
): boolean {
  const active = state.activeMutation;
  if (active === null || active.token !== request.token) return false;
  state.activeMutation = null;
  if (!sameSessionSettingsMutationRequest(active, request)) return false;
  if (!sameSessionSettingsTarget(state.owner, request.target)) return false;
  if (state.draftRevision !== request.draftRevision) return false;
  if (!sameSessionSettingsRootOwner(request.target, settlement.target)) return false;
  if (!settlement.succeeded) return true;

  state.owner = { ...settlement.target };
  state.baseline = { ...settlement.values };
  state.draft = { ...settlement.values };
  state.draftRevision += 1n;
  refreshSessionSettingsDerivedState(state);
  return true;
}

export function sessionSettingsMutationPending(state: SessionSettingsState): boolean {
  return state.activeMutation !== null;
}

function replaceSessionSettingsOwner(
  state: SessionSettingsState,
  target: SessionSettingsTarget,
  values: SessionSettingsDraft,
): void {
  state.owner = { ...target };
  state.baseline = { ...values };
  state.draft = { ...values };
  state.draftRevision += 1n;
  state.activeMutation = null;
  refreshSessionSettingsDerivedState(state);
}

function refreshSessionSettingsDerivedState(state: SessionSettingsState): void {
  state.dirty = state.baseline !== null
    && state.draft !== null
    && !sameSessionSettingsDraft(state.baseline, state.draft);
  state.validation = state.draft === null ? null : validateSessionSettingsDraft(state.draft);
}

function sameSessionSettingsDraft(
  expected: SessionSettingsDraft,
  actual: SessionSettingsDraft,
): boolean {
  return expected.baseUrl === actual.baseUrl
    && expected.model === actual.model
    && expected.providerProfile === actual.providerProfile
    && expected.apiKeyEnv === actual.apiKeyEnv
    && expected.contextWindow === actual.contextWindow
    && expected.maxOutputTokens === actual.maxOutputTokens
    && expected.accessMode === actual.accessMode;
}

function sameSessionSettingsMutationRequest(
  expected: SessionSettingsMutationRequest,
  actual: SessionSettingsMutationRequest,
): boolean {
  return expected.token === actual.token
    && expected.draftRevision === actual.draftRevision
    && sameSessionSettingsTarget(expected.target, actual.target)
    && sameSessionSettingsDraft(expected.draft, actual.draft);
}

function configField(
  key: string,
  valueType: ConfigFieldProjection["value_type"],
  required: boolean,
  minValue: number | null,
  maxValue: number | null,
  options: string[],
): ConfigFieldProjection {
  return {
    key,
    value: "",
    env_override: null,
    value_type: valueType,
    required,
    min_value: minValue,
    max_value: maxValue,
    options,
  };
}

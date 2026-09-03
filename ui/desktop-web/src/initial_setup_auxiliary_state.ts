import { sameConfigMutationTarget } from "./config_mutation.ts";
import {
  sameInitialSetupTarget,
} from "./initial_setup_state.ts";
import type {
  ConfigMutationTarget,
  InitialSetupMutationTarget,
} from "./types.ts";

export type InitialSetupAuxiliaryKind = "import" | "docling_readiness";

export interface InitialSetupAuxiliaryRequest {
  readonly token: bigint;
  readonly kind: InitialSetupAuxiliaryKind;
  readonly setupTarget: Readonly<InitialSetupMutationTarget>;
  readonly configTarget: Readonly<ConfigMutationTarget>;
  readonly draftRevision: bigint;
  readonly doclingReadinessEndpoint: string | null;
}

interface InitialSetupImportedSource {
  readonly sourcePath: string;
  readonly importGeneration: string;
  readonly configuredSensitiveKeys: readonly string[];
  readonly setupTarget: Readonly<InitialSetupMutationTarget>;
  readonly configTarget: Readonly<ConfigMutationTarget>;
}

interface InitialSetupDoclingReadinessOwner {
  readonly setupTarget: Readonly<InitialSetupMutationTarget>;
  readonly configTarget: Readonly<ConfigMutationTarget>;
  readonly draftRevision: bigint;
  readonly endpoint: string;
}

export interface InitialSetupAuxiliaryState {
  nextToken: bigint;
  active: InitialSetupAuxiliaryRequest | null;
  importedSource: InitialSetupImportedSource | null;
  doclingReadinessOwner: InitialSetupDoclingReadinessOwner | null;
}

export function createInitialSetupAuxiliaryState(): InitialSetupAuxiliaryState {
  return {
    nextToken: 1n,
    active: null,
    importedSource: null,
    doclingReadinessOwner: null,
  };
}

export function beginInitialSetupAuxiliaryRequest(
  state: InitialSetupAuxiliaryState,
  kind: InitialSetupAuxiliaryKind,
  setupTarget: InitialSetupMutationTarget,
  configTarget: ConfigMutationTarget,
  draftRevision: bigint,
  doclingReadinessEndpoint: string | null = null,
): InitialSetupAuxiliaryRequest | null {
  if (state.active !== null) return null;
  if (kind === "docling_readiness" && !doclingReadinessEndpoint) return null;
  if (kind === "import" && doclingReadinessEndpoint !== null) return null;
  const request: InitialSetupAuxiliaryRequest = Object.freeze({
    token: state.nextToken,
    kind,
    setupTarget: Object.freeze({ ...setupTarget }),
    configTarget: Object.freeze({ ...configTarget }),
    draftRevision,
    doclingReadinessEndpoint,
  });
  state.nextToken += 1n;
  state.active = request;
  if (kind === "docling_readiness") state.doclingReadinessOwner = null;
  return request;
}

/**
 * Releases the exact auxiliary request and accepts its result only while every
 * browser/Rust owner captured before the file picker or network dispatch is unchanged.
 */
export function finishInitialSetupAuxiliaryRequest(
  state: InitialSetupAuxiliaryState,
  request: InitialSetupAuxiliaryRequest,
  currentSetupTarget: InitialSetupMutationTarget | null,
  currentConfigTarget: ConfigMutationTarget,
  currentDraftRevision: bigint,
): boolean {
  const active = state.active;
  if (active === null || !sameInitialSetupAuxiliaryRequest(active, request)) return false;
  state.active = null;
  return sameInitialSetupTarget(request.setupTarget, currentSetupTarget)
    && sameConfigMutationTarget(request.configTarget, currentConfigTarget)
    && request.draftRevision === currentDraftRevision;
}

export function recordInitialSetupImportedSource(
  state: InitialSetupAuxiliaryState,
  setupTarget: InitialSetupMutationTarget,
  configTarget: ConfigMutationTarget,
  sourcePath: string,
  importGeneration: string,
  configuredSensitiveKeys: readonly string[],
): void {
  state.importedSource = {
    sourcePath,
    importGeneration,
    configuredSensitiveKeys: Object.freeze([...configuredSensitiveKeys]),
    setupTarget: Object.freeze({ ...setupTarget }),
    configTarget: Object.freeze({ ...configTarget }),
  };
}

export function initialSetupImportedConfigReference(
  state: InitialSetupAuxiliaryState,
  setupTarget: InitialSetupMutationTarget | null,
  configTarget: ConfigMutationTarget,
): { importGeneration: string; configuredSensitiveKeys: readonly string[] } | null {
  const owner = state.importedSource;
  return owner !== null && initialSetupAuxiliaryOwnerMatches(owner, setupTarget, configTarget)
    ? {
      importGeneration: owner.importGeneration,
      configuredSensitiveKeys: owner.configuredSensitiveKeys,
    }
    : null;
}

export function recordInitialSetupDoclingReadinessOwner(
  state: InitialSetupAuxiliaryState,
  request: InitialSetupAuxiliaryRequest,
): boolean {
  if (request.kind !== "docling_readiness" || request.doclingReadinessEndpoint === null) {
    return false;
  }
  state.doclingReadinessOwner = {
    setupTarget: request.setupTarget,
    configTarget: request.configTarget,
    draftRevision: request.draftRevision,
    endpoint: request.doclingReadinessEndpoint,
  };
  return true;
}

export function reconcileInitialSetupAuxiliaryState(
  state: InitialSetupAuxiliaryState,
  setupTarget: InitialSetupMutationTarget | null,
  configTarget: ConfigMutationTarget,
): void {
  if (
    state.active !== null
    && !initialSetupAuxiliaryOwnerMatches(state.active, setupTarget, configTarget)
  ) {
    state.active = null;
  }
  if (
    state.importedSource !== null
    && !initialSetupAuxiliaryOwnerMatches(state.importedSource, setupTarget, configTarget)
  ) {
    state.importedSource = null;
  }
  if (
    state.doclingReadinessOwner !== null
    && !initialSetupAuxiliaryOwnerMatches(
      state.doclingReadinessOwner,
      setupTarget,
      configTarget,
    )
  ) {
    state.doclingReadinessOwner = null;
  }
}

export function initialSetupAuxiliaryPendingKind(
  state: InitialSetupAuxiliaryState,
): InitialSetupAuxiliaryKind | null {
  return state.active?.kind ?? null;
}

export function initialSetupImportedSourcePath(
  state: InitialSetupAuxiliaryState,
  setupTarget: InitialSetupMutationTarget | null,
  configTarget: ConfigMutationTarget,
): string | null {
  return state.importedSource !== null
    && initialSetupAuxiliaryOwnerMatches(state.importedSource, setupTarget, configTarget)
      ? state.importedSource.sourcePath
      : null;
}

export function initialSetupDoclingReadinessVisible(
  state: InitialSetupAuxiliaryState,
  setupTarget: InitialSetupMutationTarget | null,
  configTarget: ConfigMutationTarget,
  currentDraftRevision: bigint,
  projectedEndpoint: string,
): boolean {
  const owner = state.doclingReadinessOwner;
  return owner !== null
    && initialSetupAuxiliaryOwnerMatches(owner, setupTarget, configTarget)
    && owner.draftRevision === currentDraftRevision
    && owner.endpoint === projectedEndpoint;
}

function initialSetupAuxiliaryOwnerMatches(
  owner: {
    setupTarget: Readonly<InitialSetupMutationTarget>;
    configTarget: Readonly<ConfigMutationTarget>;
  },
  setupTarget: InitialSetupMutationTarget | null,
  configTarget: ConfigMutationTarget,
): boolean {
  return sameInitialSetupTarget(owner.setupTarget, setupTarget)
    && sameConfigMutationTarget(owner.configTarget, configTarget);
}

function sameInitialSetupAuxiliaryRequest(
  expected: InitialSetupAuxiliaryRequest,
  actual: InitialSetupAuxiliaryRequest,
): boolean {
  return expected.token === actual.token
    && expected.kind === actual.kind
    && expected.draftRevision === actual.draftRevision
    && expected.doclingReadinessEndpoint === actual.doclingReadinessEndpoint
    && sameInitialSetupTarget(expected.setupTarget, actual.setupTarget)
    && sameConfigMutationTarget(expected.configTarget, actual.configTarget);
}

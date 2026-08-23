import type { StopMutationTarget } from "../src/types.ts";

export type RootStopMutationTarget = Extract<StopMutationTarget, { kind: "root" }>;
export type TurnStopMutationTarget = Extract<StopMutationTarget, { kind: "turn" }>;

export function rootStopTarget(
  overrides: Partial<Omit<RootStopMutationTarget, "kind">> = {},
): RootStopMutationTarget {
  return {
    kind: "root",
    workspacePath: "C:/workspace",
    sessionId: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
    rootGeneration: "9",
    latestTurnId: "01ARZ3NDEKTSV4RRFFQ69G5FAW",
    admissionRevision: "4",
    permissionConfirmationId: null,
    ...overrides,
  } satisfies RootStopMutationTarget;
}

export function turnStopTarget(
  overrides: Partial<Omit<TurnStopMutationTarget, "kind">> = {},
): TurnStopMutationTarget {
  return {
    kind: "turn",
    workspacePath: "C:/workspace",
    sessionId: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
    turnId: "01ARZ3NDEKTSV4RRFFQ69G5FAW",
    admissionRevision: "4",
    rootEpoch: "9",
    ...overrides,
  } satisfies TurnStopMutationTarget;
}

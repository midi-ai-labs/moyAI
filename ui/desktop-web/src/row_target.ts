import type { DesktopWebState, RowMutationTarget } from "./types.ts";

export function rowMutationTarget(state: DesktopWebState, rowId: string): RowMutationTarget {
  return {
    workspacePath: state.workspace_path,
    ownerProjectId: state.project_rows[state.selected_project_index]?.project_id ?? null,
    ownerSessionId: state.session_rows[state.selected_session_index]?.session_id ?? null,
    rowId,
  };
}

export function rowMutationArgs(
  state: DesktopWebState,
  index: number,
  rowId: string | null | undefined,
): { index: number; expectedTarget: RowMutationTarget } | null {
  if (!Number.isInteger(index) || index < 0 || !rowId) return null;
  return { index, expectedTarget: rowMutationTarget(state, rowId) };
}

export function sameRowMutationTarget(
  expected: RowMutationTarget,
  actual: RowMutationTarget | null,
): boolean {
  return actual !== null
    && expected.workspacePath === actual.workspacePath
    && expected.ownerProjectId === actual.ownerProjectId
    && expected.ownerSessionId === actual.ownerSessionId
    && expected.rowId === actual.rowId;
}

export function rowMutationTargetStillMatches(
  state: DesktopWebState,
  expected: RowMutationTarget,
  currentRowId: string | null | undefined,
): boolean {
  return Boolean(currentRowId) && sameRowMutationTarget(expected, rowMutationTarget(state, currentRowId ?? ""));
}

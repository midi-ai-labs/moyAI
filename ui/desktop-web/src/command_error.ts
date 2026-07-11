import type { DesktopWebState } from "./types";

export function commandConflictState(error: unknown): DesktopWebState | null {
  const payload = commandErrorPayload(error);
  if (!payload || payload.kind !== "conflict" || !isDesktopWebState(payload.state)) return null;
  return payload.state;
}

function commandErrorPayload(error: unknown): Record<string, unknown> | null {
  if (typeof error === "object" && error !== null) return error as Record<string, unknown>;
  if (typeof error !== "string" || !error.trim().startsWith("{")) return null;
  try {
    const parsed: unknown = JSON.parse(error);
    return typeof parsed === "object" && parsed !== null
      ? parsed as Record<string, unknown>
      : null;
  } catch {
    return null;
  }
}

function isDesktopWebState(value: unknown): value is DesktopWebState {
  if (typeof value !== "object" || value === null) return false;
  const state = value as Partial<DesktopWebState>;
  return isProjectionRevision(state.projection_revision)
    && typeof state.workspace_path === "string"
    && Array.isArray(state.project_rows)
    && Array.isArray(state.session_rows);
}

function isProjectionRevision(value: unknown): value is string {
  if (typeof value !== "string" || !/^(0|[1-9]\d*)$/.test(value)) return false;
  try {
    return BigInt(value) <= 18_446_744_073_709_551_615n;
  } catch {
    return false;
  }
}

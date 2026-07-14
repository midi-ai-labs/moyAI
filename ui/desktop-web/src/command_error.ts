import type { DesktopWebState } from "./types";

export type DesktopCommandErrorCategory =
  | "unknown"
  | "provider"
  | "model"
  | "image"
  | "permission"
  | "runtime"
  | "storage";

export type DesktopCommandErrorCode =
  | "unknown"
  | "provider_transport"
  | "model_unavailable"
  | "image_unsupported"
  | "permission_policy_denied"
  | "runtime_failure"
  | "storage_failure";

export interface DesktopCommandErrorInfo {
  kind: "conflict" | "internal" | "unknown";
  category: DesktopCommandErrorCategory;
  code: DesktopCommandErrorCode;
  message: string;
}

export function commandConflictState(error: unknown): DesktopWebState | null {
  const payload = commandErrorPayload(error);
  if (!payload || payload.kind !== "conflict" || !isDesktopWebState(payload.state)) return null;
  return payload.state;
}

export function commandInternalState(error: unknown): DesktopWebState | null {
  const payload = commandErrorPayload(error);
  if (!payload || payload.kind !== "internal" || !isDesktopWebState(payload.state)) return null;
  return payload.state;
}

export function commandErrorInfo(error: unknown): DesktopCommandErrorInfo {
  const payload = commandErrorPayload(error);
  if (!payload) {
    return {
      kind: "unknown",
      category: "unknown",
      code: "unknown",
      message: error instanceof Error ? error.message : String(error),
    };
  }
  return {
    kind: payload.kind === "conflict" || payload.kind === "internal" ? payload.kind : "unknown",
    category: commandErrorCategory(payload.category),
    code: commandErrorCode(payload.code),
    message: typeof payload.message === "string" ? payload.message : String(error),
  };
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

function commandErrorCategory(value: unknown): DesktopCommandErrorCategory {
  switch (value) {
    case "provider":
    case "model":
    case "image":
    case "permission":
    case "runtime":
    case "storage":
      return value;
    default:
      return "unknown";
  }
}

function commandErrorCode(value: unknown): DesktopCommandErrorCode {
  switch (value) {
    case "provider_transport":
    case "model_unavailable":
    case "image_unsupported":
    case "permission_policy_denied":
    case "runtime_failure":
    case "storage_failure":
      return value;
    default:
      return "unknown";
  }
}

import type { FocusTargetCandidate } from "./focus_arbiter.ts";
import type { DesktopViewState, DraftActionTarget, RowMutationTarget } from "./types.ts";

export type AttachmentMutationName =
  | "attach_image"
  | "browse_image"
  | "clear_images"
  | "remove_image";

export type AttachmentFocusTarget =
  | { kind: "attachment"; path: string }
  | { kind: "browse" }
  | { kind: "toggle" };

export interface AttachmentFocusContinuation {
  mutationName: AttachmentMutationName;
  owner: {
    workspacePath: string;
    projectId: string | null;
    sessionId: string | null;
    ownerGeneration: string;
  };
  pathsBefore: string[];
  removedIndex: number | null;
  removedPath: string | null;
}

export interface AttachmentFocusDecision {
  continuation: AttachmentFocusContinuation | null;
  focusTarget: AttachmentFocusTarget | null;
  trayOpen: boolean | null;
}

type AttachmentFocusState = Pick<
  DesktopViewState,
  | "attached_images"
  | "draft_target"
  | "project_rows"
  | "selected_project_index"
  | "session_rows"
  | "selected_session_index"
  | "workspace_path"
>;

export function beginAttachmentFocusContinuation(
  state: AttachmentFocusState,
  mutationName: string,
  args?: Record<string, unknown>,
): AttachmentFocusContinuation | null {
  if (!isAttachmentMutationName(mutationName)) return null;
  if (!sameDraftTarget(state.draft_target, args?.expectedTarget)) {
    if (mutationName !== "remove_image") return null;
  }

  const owner = attachmentOwner(state);
  const continuation: AttachmentFocusContinuation = {
    mutationName,
    owner,
    pathsBefore: [...state.attached_images],
    removedIndex: null,
    removedPath: null,
  };

  if (mutationName === "clear_images" && state.attached_images.length === 0) return null;
  if (mutationName !== "remove_image") return continuation;

  const index = args?.index;
  if (!Number.isInteger(index) || typeof index !== "number" || index < 0) return null;
  const path = state.attached_images[index];
  if (!path || !sameRemovalTarget(state, path, args?.expectedTarget)) return null;
  return { ...continuation, removedIndex: index, removedPath: path };
}

export function reconcileAttachmentFocusContinuation(
  current: AttachmentFocusContinuation | null,
  next: AttachmentFocusState,
  mutationName: string | null,
): AttachmentFocusDecision {
  if (!current) return noAttachmentFocusDecision();
  if (!sameAttachmentOwner(current, next)) return noAttachmentFocusDecision();
  if (mutationName !== current.mutationName) {
    return { continuation: current, focusTarget: null, trayOpen: null };
  }

  if (current.mutationName === "browse_image") {
    if (samePaths(current.pathsBefore, next.attached_images)) {
      return {
        continuation: null,
        focusTarget: { kind: "browse" },
        trayOpen: true,
      };
    }
    return attachmentAppendSucceeded(current.pathsBefore, next.attached_images)
      ? { continuation: null, focusTarget: { kind: "toggle" }, trayOpen: false }
      : noAttachmentFocusDecision();
  }

  if (current.mutationName === "attach_image") {
    return attachmentAppendSucceeded(current.pathsBefore, next.attached_images)
      ? { continuation: null, focusTarget: { kind: "toggle" }, trayOpen: false }
      : noAttachmentFocusDecision();
  }

  if (current.mutationName === "clear_images") {
    return next.attached_images.length === 0
      ? { continuation: null, focusTarget: { kind: "toggle" }, trayOpen: false }
      : noAttachmentFocusDecision();
  }

  const index = current.removedIndex;
  if (index === null || current.removedPath === null) return noAttachmentFocusDecision();
  const expected = current.pathsBefore.filter((_path, candidateIndex) => candidateIndex !== index);
  if (!samePaths(expected, next.attached_images)) return noAttachmentFocusDecision();
  if (next.attached_images.length === 0) {
    return { continuation: null, focusTarget: { kind: "toggle" }, trayOpen: false };
  }
  const adjacentPath = next.attached_images[Math.min(index, next.attached_images.length - 1)];
  return {
    continuation: null,
    focusTarget: { kind: "attachment", path: adjacentPath },
    trayOpen: null,
  };
}

/** Resolve the mutation-specific fallback chain against the committed DOM. */
export function attachmentFocusCandidates(
  documentTarget: Document,
  target: AttachmentFocusTarget,
): readonly FocusTargetCandidate[] {
  const prompt: FocusTargetCandidate = {
    resolve: () => documentTarget.querySelector<HTMLElement>("#prompt"),
  };
  const toggle: FocusTargetCandidate = {
    resolve: () => documentTarget.querySelector<HTMLElement>(
      "[data-action='toggle-attachment-tray']",
    ),
  };
  if (target.kind === "attachment") {
    return [
      {
        resolve: () => Array.from(
          documentTarget.querySelectorAll<HTMLElement>("[data-focus-key]"),
        ).find((candidate) => candidate.dataset.focusKey === `attachment:${target.path}`) ?? null,
      },
      toggle,
      prompt,
    ];
  }
  if (target.kind === "browse") {
    return [
      { resolve: () => documentTarget.querySelector<HTMLElement>("[data-action='browse-image']") },
      { resolve: () => documentTarget.querySelector<HTMLElement>("#image-input") },
      toggle,
      prompt,
    ];
  }
  return [toggle, prompt];
}

function attachmentOwner(state: AttachmentFocusState): AttachmentFocusContinuation["owner"] {
  return {
    workspacePath: state.draft_target.workspacePath,
    projectId: state.project_rows[state.selected_project_index]?.project_id ?? null,
    sessionId: state.draft_target.sessionId,
    ownerGeneration: state.draft_target.ownerGeneration,
  };
}

function sameAttachmentOwner(
  continuation: AttachmentFocusContinuation,
  state: AttachmentFocusState,
): boolean {
  const next = attachmentOwner(state);
  return continuation.owner.workspacePath === next.workspacePath
    && continuation.owner.projectId === next.projectId
    && continuation.owner.sessionId === next.sessionId
    && continuation.owner.ownerGeneration === next.ownerGeneration;
}

function sameDraftTarget(expected: DraftActionTarget, candidate: unknown): boolean {
  if (!candidate || typeof candidate !== "object") return false;
  const actual = candidate as Partial<DraftActionTarget>;
  return expected.workspacePath === actual.workspacePath
    && expected.sessionId === actual.sessionId
    && expected.ownerGeneration === actual.ownerGeneration;
}

function sameRemovalTarget(
  state: AttachmentFocusState,
  path: string,
  candidate: unknown,
): boolean {
  if (!candidate || typeof candidate !== "object") return false;
  const actual = candidate as Partial<RowMutationTarget>;
  return actual.workspacePath === state.workspace_path
    && actual.ownerProjectId === (state.project_rows[state.selected_project_index]?.project_id ?? null)
    && actual.ownerSessionId === (state.session_rows[state.selected_session_index]?.session_id ?? null)
    && actual.rowId === path;
}

function attachmentAppendSucceeded(before: readonly string[], after: readonly string[]): boolean {
  return after.length === before.length + 1
    && before.every((path, index) => after[index] === path);
}

function samePaths(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((path, index) => path === right[index]);
}

function isAttachmentMutationName(value: string): value is AttachmentMutationName {
  return value === "attach_image"
    || value === "browse_image"
    || value === "clear_images"
    || value === "remove_image";
}

function noAttachmentFocusDecision(): AttachmentFocusDecision {
  return { continuation: null, focusTarget: null, trayOpen: null };
}

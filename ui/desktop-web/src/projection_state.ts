import type { DesktopWebState } from "./types.ts";
import { composerOwner, type DraftMutationSnapshot } from "./view_state.ts";
import type { SettingsActionFocusContinuation } from "./settings_surface.ts";
import type { RefreshPromptFocusContinuation } from "./main_prompt_continuity.ts";

export interface StateUpdate {
  state: DesktopWebState;
  forceRender: boolean;
  mutationName: string | null;
  scheduleNavigation: boolean;
  sequence: number;
  draftSnapshot: DraftMutationSnapshot | null;
  refreshPromptFocusContinuation: RefreshPromptFocusContinuation | null;
  settingsAction: SettingsActionFocusContinuation | null;
  navigationRowId?: string;
}

export function retainDeferredCommand(preferred: StateUpdate, other: StateUpdate): StateUpdate {
  const hasCommand = (update: StateUpdate) => update.mutationName !== null || update.settingsAction !== null
    || update.draftSnapshot !== null || update.refreshPromptFocusContinuation !== null || update.scheduleNavigation;
  if (hasCommand(preferred) || !hasCommand(other)) return preferred;
  const state = preferred.state;
  const prior = other.state;
  if (state.overlay !== prior.overlay || state.hub_project_open !== prior.hub_project_open || state.confirmation_id !== prior.confirmation_id
    || state.composer_commit_generation !== prior.composer_commit_generation) return preferred;
  if (composerOwner(state) !== composerOwner(prior)) {
    // Selection first returns a loading projection owned by the old composer.
    // Only its explicit destination may receive the navigation completion.
    const target = other.navigationRowId;
    const selected = other.mutationName === "select_project" ? state.project_rows[state.selected_project_index]?.project_id
      : other.mutationName === "select_session" || other.mutationName === "select_chat_session" ? state.draft_target.sessionId : null;
    if (!target || target !== selected || !prior.navigation_loading || state.navigation_loading
      || BigInt(state.draft_target.ownerGeneration) !== BigInt(prior.draft_target.ownerGeneration) + 1n) return preferred;
    return { ...preferred, forceRender: true, mutationName: other.mutationName, navigationRowId: target, scheduleNavigation: other.scheduleNavigation };
  }
  return { ...other, state, sequence: preferred.sequence, forceRender: preferred.forceRender || other.forceRender };
}

export function projectionUpdateAccepted(
  lastAppliedRevision: string,
  candidateRevision: string,
  currentObject: boolean,
): boolean {
  const comparison = compareProjectionRevisions(candidateRevision, lastAppliedRevision);
  if (comparison === null) return false;
  return currentObject ? comparison >= 0 : comparison > 0;
}

export function appliedProjectionRevision(
  lastAppliedRevision: string,
  candidateRevision: string,
): string {
  const comparison = compareProjectionRevisions(candidateRevision, lastAppliedRevision);
  return comparison !== null && comparison > 0 ? canonicalProjectionRevision(candidateRevision) : lastAppliedRevision;
}

export function projectionRevisionAtLeast(
  candidateRevision: string,
  baselineRevision: string,
): boolean {
  const comparison = compareProjectionRevisions(candidateRevision, baselineRevision);
  return comparison !== null && comparison >= 0;
}

export function deferredProjectionCandidatePreferred(
  currentRevision: string,
  candidateRevision: string,
  currentSequence: number,
  candidateSequence: number,
): boolean {
  const comparison = compareProjectionRevisions(candidateRevision, currentRevision);
  if (comparison === null) return false;
  return comparison > 0 || (comparison === 0 && candidateSequence > currentSequence);
}

export function isProjectionRevision(value: unknown): value is string {
  if (typeof value !== "string" || !/^(0|[1-9]\d*)$/.test(value)) return false;
  try {
    return BigInt(value) <= 18_446_744_073_709_551_615n;
  } catch {
    return false;
  }
}

function compareProjectionRevisions(left: string, right: string): number | null {
  if (!isProjectionRevision(left) || !isProjectionRevision(right)) return null;
  const normalizedLeft = canonicalProjectionRevision(left);
  const normalizedRight = canonicalProjectionRevision(right);
  if (normalizedLeft.length !== normalizedRight.length) {
    return normalizedLeft.length > normalizedRight.length ? 1 : -1;
  }
  return normalizedLeft === normalizedRight ? 0 : normalizedLeft > normalizedRight ? 1 : -1;
}

function canonicalProjectionRevision(value: string): string {
  return BigInt(value).toString(10);
}

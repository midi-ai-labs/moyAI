import type { DesktopWebState } from "./types.ts";
import type { UiLocalState } from "./ui_state.ts";
import type { ActionContext } from "./actions.ts";
import { command } from "./api.ts";

/** Capture only the visible canonical user item; the Rust command decides editability. */
export function latestLocalRevisionSource(state: DesktopWebState): {
  sessionId: string;
  admissionRevision: string;
  historyItemId: string;
  prompt: string;
} | null {
  if (state.hub_project_open || state.selected_session_index < 0) return null;
  const session = state.session_rows[state.selected_session_index];
  if (!session) return null;
  const lastUser = [...state.transcript_rows].reverse().find(row => row.row_kind === "user" && row.stable_history_identity?.trim());
  if (!lastUser) return null;
  return { sessionId: session.session_id, admissionRevision: session.admission_revision,
    historyItemId: lastUser.stable_history_identity!.trim(), prompt: lastUser.body };
}

export function localRevisionActionEnabled(state: DesktopWebState, pending: boolean, historyItemId: string): boolean {
  const source = latestLocalRevisionSource(state);
  const selected = state.session_rows[state.selected_session_index];
  return Boolean(source && source.historyItemId === historyItemId && !pending
    && state.run_target.sessionId === source.sessionId
    && state.run_target.expectedState.kind === "idle"
    && state.run_target.expectedState.latestTurnId
    && state.run_target.expectedState.admissionRevision === source.admissionRevision
    && selected?.loaded_status !== "active" && selected?.status !== "running"
    && state.task_activity_state === "idle" && !state.busy && !state.navigation_loading
    && state.navigation_admission_open && state.pending_turn_inputs.length === 0);
}

function messageEditError(error: unknown): string {
  const detail = typeof error === "string" ? error : error && typeof error === "object" && "message" in error
    ? String(error.message) : "";
  return detail.includes("message with an image cannot be edited")
    ? "画像付きの依頼は編集できません。新しい依頼として送ってください。"
    : "依頼の編集を始められません。実行と起動中のアプリを停止し、最新の会話を確認してください。";
}

export async function prepareLocalMessageEdit(context: ActionContext, historyItemId: string): Promise<void> {
  const state = context.getProjection();
  const local = context.uiState.localMessageEdit;
  const source = state && latestLocalRevisionSource(state);
  if (!state || !source || !localRevisionActionEnabled(state, local.pending, historyItemId)) return;
  local.pending = true;
  local.error = "";
  context.rerender();
  try {
    const result = await command<{ state: DesktopWebState; forkedSessionId: string; editableText: string }>(
      "prepare_latest_message_edit", { expectedRunTarget: state.run_target, expectedHistoryItemId: source.historyItemId });
    local.handoff = { forkedSessionId: result.forkedSessionId, sourceSessionId: source.sessionId, workspacePath: state.run_target.workspacePath,
      editableText: result.editableText };
    context.acceptProjection(result.state, true);
  } catch (error) {
    context.recoverCommandConflict(error);
    local.error = messageEditError(error);
  } finally {
    local.pending = false;
    context.rerender();
  }
}

/** Move the returned text only when the newly created chat becomes the active owner. */
export function applyLocalMessageEditHandoff(uiState: UiLocalState, state: DesktopWebState): boolean {
  const handoff = uiState.localMessageEdit.handoff;
  if (!handoff) return false;
  if (!state.hub_project_open && !state.navigation_loading && state.selected_session_index >= 0
    && state.draft_target.sessionId === handoff.forkedSessionId
    && state.draft_target.workspacePath === handoff.workspacePath) {
    uiState.drafts.prompt = handoff.editableText;
    uiState.drafts.composerRevision += 1;
    uiState.mainComposerDrafts.set(uiState.drafts.composerSessionOwner, { prompt: handoff.editableText, imageInput: uiState.drafts.imageInput });
    uiState.localMessageEdit.handoff = null;
    uiState.localMessageEdit.error = "";
    uiState.focusPromptAfterRender = true;
    return true;
  }
  if (!state.navigation_loading && state.draft_target.sessionId !== handoff.sourceSessionId) {
    uiState.localMessageEdit.handoff = null;
  }
  return false;
}

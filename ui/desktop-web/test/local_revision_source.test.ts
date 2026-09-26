import assert from "node:assert/strict";
import test from "node:test";
import { applyLocalMessageEditHandoff, latestLocalRevisionSource, localRevisionActionEnabled, prepareLocalMessageEdit } from "../src/local_revision_source.ts";
import { createUiLocalState } from "../src/ui_state.ts";
import type { ActionContext } from "../src/actions.ts";
import type { DesktopWebState } from "../src/types.ts";

test("local edit source captures the latest canonical user item in the selected session", () => {
  const state = { hub_project_open: false, selected_session_index: 0,
    session_rows: [{ session_id: "session-a", admission_revision: "18446744073709551614" }],
    transcript_rows: [
      { row_kind: "user", stable_history_identity: "old-item", body: "古い依頼" },
      { row_kind: "assistant", stable_history_identity: "answer-item", body: "返答" },
      { row_kind: "user", stable_history_identity: "new-item", body: "修正前の依頼" },
      { row_kind: "work_summary_completed", stable_history_identity: "summary-item", body: "完了" },
    ] } as unknown as DesktopWebState;
  assert.deepEqual(latestLocalRevisionSource(state), { sessionId: "session-a", admissionRevision: "18446744073709551614",
    historyItemId: "new-item", prompt: "修正前の依頼" });
  assert.equal(latestLocalRevisionSource({ ...state, hub_project_open: true }), null);
  assert.equal(latestLocalRevisionSource({ ...state, selected_session_index: -1 }), null);
  assert.equal(latestLocalRevisionSource({ ...state, transcript_rows: [{ ...state.transcript_rows[2], stable_history_identity: null }] }), null);
});

function editableState(overrides: Record<string, unknown> = {}): DesktopWebState {
  return {
    hub_project_open: false, selected_session_index: 0,
    session_rows: [{ session_id: "session-a", admission_revision: "17", loaded_status: "idle", status: "completed" }],
    transcript_rows: [{ row_kind: "user", stable_history_identity: "item-1", body: "元の依頼" }],
    run_target: { workspacePath: "C:/workspace", sessionId: "session-a", runtimeOwnerToken: "owner",
      permissionConfirmationId: null, expectedState: { kind: "idle", latestTurnId: "turn-1", admissionRevision: "17" } },
    draft_target: { workspacePath: "C:/workspace", sessionId: "session-a" },
    task_activity_state: "idle", busy: false, navigation_loading: false,
    navigation_admission_open: true, pending_turn_inputs: [],
    ...overrides,
  } as unknown as DesktopWebState;
}

test("edit action requires the exact idle latest turn, selected message and revision", () => {
  const state = editableState();
  assert.equal(localRevisionActionEnabled(state, false, "item-1"), true);
  assert.equal(localRevisionActionEnabled(state, false, "old-item"), false);
  assert.equal(localRevisionActionEnabled(state, true, "item-1"), false);
  assert.equal(localRevisionActionEnabled(editableState({ task_activity_state: "running" }), false, "item-1"), false);
  assert.equal(localRevisionActionEnabled(editableState({ run_target: { ...state.run_target,
    expectedState: { kind: "idle", latestTurnId: "turn-1", admissionRevision: "18" } } }), false, "item-1"), false);
});

test("edit command carries one exact source and hands draft only to its new chat once", async () => {
  const source = editableState();
  const opening = editableState({ navigation_loading: true });
  const newChat = editableState({ selected_session_index: 0,
    session_rows: [{ session_id: "session-fork", admission_revision: "1", loaded_status: "idle", status: "idle" }],
    draft_target: { workspacePath: "C:/workspace", sessionId: "session-fork" } });
  const ui = createUiLocalState();
  ui.drafts.prompt = "元の画面に残す入力";
  const calls: Array<{name: string; args: Record<string, unknown>}> = [];
  const oldWindow = (globalThis as {window?: unknown}).window;
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (name: string, args: Record<string, unknown>) => {
    calls.push({name, args});
    return { state: opening, forkedSessionId: "session-fork", editableText: "元の依頼" };
  } } } });
  const accepted: DesktopWebState[] = [];
  const context = { uiState: ui, getProjection: () => source, acceptProjection: (state: DesktopWebState) => { accepted.push(state); },
    rerender: () => {}, recoverCommandConflict: () => false } as unknown as ActionContext;
  try {
    await prepareLocalMessageEdit(context, "item-1");
  } finally {
    Object.defineProperty(globalThis, "window", { configurable: true, value: oldWindow });
  }
  assert.deepEqual(calls, [{ name: "prepare_latest_message_edit",
    args: { expectedRunTarget: source.run_target, expectedHistoryItemId: "item-1" } }]);
  assert.deepEqual(accepted, [opening]);
  assert.equal(ui.drafts.prompt, "元の画面に残す入力");
  assert.equal(applyLocalMessageEditHandoff(ui, source), false, "stale source refresh must not drop the handoff");
  assert.equal(ui.localMessageEdit.handoff?.forkedSessionId, "session-fork");
  ui.drafts.composerSessionOwner = "C:/workspace\u0000project\u0000session-fork";
  assert.equal(applyLocalMessageEditHandoff(ui, newChat), true);
  assert.equal(ui.drafts.prompt, "元の依頼");
  assert.equal(ui.mainComposerDrafts.get(ui.drafts.composerSessionOwner)?.prompt, "元の依頼");
  assert.equal(ui.focusPromptAfterRender, true);
  assert.equal(applyLocalMessageEditHandoff(ui, newChat), false, "handoff may not replay over later typing");
  ui.drafts.prompt = "書き足した入力";
  assert.equal(ui.drafts.prompt, "書き足した入力");
});

test("an image-backed local message keeps the draft and shows the edit limitation", async () => {
  const source = editableState();
  const ui = createUiLocalState();
  ui.drafts.prompt = "送信前の別の入力";
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async () => {
    throw { kind: "internal", category: "storage", code: "storage_failure",
      message: "a message with an image cannot be edited in the text composer" };
  } } } });
  try {
    const context = { uiState: ui, getProjection: () => source, acceptProjection: () => { throw new Error("unexpected projection"); },
      rerender: () => {}, recoverCommandConflict: () => false } as unknown as ActionContext;
    await prepareLocalMessageEdit(context, "item-1");
    assert.equal(ui.localMessageEdit.error, "画像付きの依頼は編集できません。新しい依頼として送ってください。");
    assert.equal(ui.drafts.prompt, "送信前の別の入力");
    assert.equal(ui.localMessageEdit.handoff, null);
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});

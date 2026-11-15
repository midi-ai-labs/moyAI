import assert from "node:assert/strict";
import test from "node:test";
import {
  composerFallbackFocusWanted,
  conversationThreadRetainable,
  planThreadViewport,
  selectedNavigationRowYieldsFocus,
  threadContentAdvanced,
  threadScrollTopAfterLayout,
} from "../src/render_commit_plan.ts";

const conversation = { overlay: "none" as const, hub_project_open: false, confirmation_visible: false };
const quiet = { backgroundInert: false, sessionChanged: false, localModalWasOpen: false };

test("only an ordinary conversation-to-conversation update keeps the thread in place", () => {
  assert.equal(conversationThreadRetainable(conversation, conversation, quiet), true);
  assert.equal(conversationThreadRetainable(null, conversation, quiet), false);
  assert.equal(conversationThreadRetainable({ ...conversation, overlay: "config" }, conversation, quiet), false);
  assert.equal(conversationThreadRetainable(conversation, { ...conversation, overlay: "hub" }, quiet), false);
  assert.equal(conversationThreadRetainable({ ...conversation, hub_project_open: true }, conversation, quiet), false);
  assert.equal(conversationThreadRetainable(conversation, { ...conversation, hub_project_open: true }, quiet), false);
  assert.equal(conversationThreadRetainable({ ...conversation, confirmation_visible: true }, conversation, quiet), false);
  // A confirmation that opens in this update is rendered beside the retained thread.
  assert.equal(conversationThreadRetainable(conversation, { ...conversation, confirmation_visible: true }, quiet), true);
  for (const blocked of ["backgroundInert", "sessionChanged", "localModalWasOpen"] as const) {
    assert.equal(conversationThreadRetainable(conversation, conversation, { ...quiet, [blocked]: true }), false, blocked);
  }
});

test("thread content advances only when rows, queued inputs or file changes grow", () => {
  const rows = (transcript: number, pending: number, changes: number) => ({
    transcript_rows: Array(transcript).fill({}),
    pending_turn_inputs: Array(pending).fill({}),
    file_change_rows: Array(changes).fill({}),
  }) as unknown as Parameters<typeof threadContentAdvanced>[1];
  assert.equal(threadContentAdvanced(rows(2, 0, 0), rows(2, 0, 0)), false);
  assert.equal(threadContentAdvanced(rows(2, 0, 0), rows(3, 0, 0)), true);
  assert.equal(threadContentAdvanced(rows(2, 0, 0), rows(2, 1, 0)), true);
  assert.equal(threadContentAdvanced(rows(2, 0, 0), rows(2, 0, 1)), true);
  assert.equal(threadContentAdvanced(rows(3, 1, 1), rows(2, 0, 0)), false);
  assert.equal(threadContentAdvanced(null, rows(0, 0, 0)), false);
  assert.equal(threadContentAdvanced(null, rows(1, 0, 0)), true);
});

test("viewport restoration prefers a remembered session position, then the tail, then the old offset", () => {
  const plan = (overrides: Partial<Parameters<typeof planThreadViewport>[0]>) => planThreadViewport({
    threadPresent: true, rememberedSessionInteraction: false, revealEnd: false, previousThreadPresent: true, ...overrides,
  });
  assert.equal(plan({ threadPresent: false, rememberedSessionInteraction: true, revealEnd: true }), "none");
  assert.equal(plan({ rememberedSessionInteraction: true, revealEnd: true }), "session-interaction");
  assert.equal(plan({ revealEnd: true }), "reveal-end");
  assert.equal(plan({}), "keep-scroll-top");
  assert.equal(plan({ previousThreadPresent: false }), "none");
});

test("a thread near its end keeps the same distance from the end after layout", () => {
  const base = { previousScrollTop: 120, previousEndGap: 8, scrollHeight: 1_000, clientHeight: 400 };
  assert.equal(threadScrollTopAfterLayout({ ...base, previousWasNearEnd: true, sameSessionOwner: true }), 592);
  assert.equal(threadScrollTopAfterLayout({ ...base, previousWasNearEnd: false, sameSessionOwner: true }), 120);
  assert.equal(threadScrollTopAfterLayout({ ...base, previousWasNearEnd: true, sameSessionOwner: false }), 120);
  assert.equal(threadScrollTopAfterLayout({ ...base, previousEndGap: 900, previousWasNearEnd: true, sameSessionOwner: true }), 0);
});

test("only the focused row of the selection that just settled yields focus to the composer", () => {
  const settled = {
    focusPromptAfterRender: true, navigationLoading: false, activeAction: "session",
    activeFocusKey: "session:S1:select", selectedProjectId: "P1", selectedSessionId: "S1",
  };
  assert.equal(selectedNavigationRowYieldsFocus(settled), true);
  assert.equal(selectedNavigationRowYieldsFocus({ ...settled, activeAction: "chat-session", activeFocusKey: "chat-session:S1:select" }), true);
  assert.equal(selectedNavigationRowYieldsFocus({ ...settled, activeAction: "project", activeFocusKey: "project:P1:select" }), true);
  assert.equal(selectedNavigationRowYieldsFocus({ ...settled, focusPromptAfterRender: false }), false);
  assert.equal(selectedNavigationRowYieldsFocus({ ...settled, navigationLoading: true }), false);
  assert.equal(selectedNavigationRowYieldsFocus({ ...settled, activeFocusKey: "session:S2:select" }), false);
  assert.equal(selectedNavigationRowYieldsFocus({ ...settled, activeAction: "delete-session" }), false);
  assert.equal(selectedNavigationRowYieldsFocus({ ...settled, selectedSessionId: null, activeFocusKey: "session:null:select" }), false);
  assert.equal(selectedNavigationRowYieldsFocus({ ...settled, activeAction: undefined, activeFocusKey: undefined }), false);
});

test("the composer is the last focus fallback after a modal or titlebar menu closes", () => {
  const closing = {
    modalClosing: true, titlebarMenuFocusAction: null, titlebarMenuFocusReserved: false,
    initiatingTriggerYieldsPromptFocus: false, newSessionFocusRejected: false, focusSnapshotReserved: false,
    quickChatOwnsModalCloseFallback: false, confirmationVisible: false, localModalPending: false, overlay: "none" as const,
  };
  assert.equal(composerFallbackFocusWanted(closing), true);
  assert.equal(composerFallbackFocusWanted({ ...closing, modalClosing: false }), false);
  assert.equal(composerFallbackFocusWanted({ ...closing, modalClosing: false, titlebarMenuFocusAction: "open-settings" }), true);
  assert.equal(composerFallbackFocusWanted({ ...closing, modalClosing: false, titlebarMenuFocusAction: "open-settings", titlebarMenuFocusReserved: true }), false);
  for (const owner of ["initiatingTriggerYieldsPromptFocus", "newSessionFocusRejected", "focusSnapshotReserved",
    "quickChatOwnsModalCloseFallback", "confirmationVisible", "localModalPending"] as const) {
    assert.equal(composerFallbackFocusWanted({ ...closing, [owner]: true }), false, owner);
  }
  assert.equal(composerFallbackFocusWanted({ ...closing, overlay: "config" }), false);
});

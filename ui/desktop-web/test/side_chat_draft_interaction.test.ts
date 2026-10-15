import assert from "node:assert/strict";
import test from "node:test";
import { persistSideChatDraft, type ActionContext } from "../src/actions.ts";
import { InteractionLifecycle } from "../src/interaction_lifecycle.ts";
import { createUiLocalState, sideChatDraftForState, updateSideChatDraftFromManualEdit } from "../src/ui_state.ts";
import type { DesktopWebState } from "../src/types.ts";

const settleMicrotasks = () => new Promise<void>(resolve => setImmediate(resolve));

function fixture() {
  const ui = createUiLocalState();
  ui.drafts.prompt = "unchanged main draft";
  let current = { projection_revision: "1", draft_target: { sessionId: "session-a" },
    side_chat: { owner_session_id: "session-a", chat_id: "side-a", draft_text: "",
      draft_revision: "0", draft_quote: null, deleting: false } } as DesktopWebState;
  const lifecycle = new InteractionLifecycle<DesktopWebState>(() => true);
  let durableRevision = 0;
  let durableText = "";
  let beforeAccept: (() => void) | null = null;
  const calls: Array<{ name: string; expected: string; text: string }> = [];
  const errors: string[] = [];
  const accept = (projection: DesktopWebState) => {
    // Match main.applyStateUpdate: both its DOM and getProjection remain old while interacting.
    if (!lifecycle.defer(projection, false, true)) current = projection;
  };
  const context = { uiState: ui, getProjection: () => current,
    waitForInteractionIdle: () => lifecycle.whenIdle(),
    mutate: async (name: string, args: Record<string, unknown>) => {
      calls.push({ name, expected: String(args.expectedDraftRevision), text: String(args.text) });
      assert.equal(name, "save_side_chat_draft", "autosave must never submit either composer");
      if (args.expectedDraftRevision !== String(durableRevision)) errors.push("draft revision changed");
      else { durableText = String(args.text); durableRevision += 1; }
      beforeAccept?.();
      accept({ ...current, projection_revision: String(Number(current.projection_revision) + 1),
        side_chat: { ...current.side_chat, draft_text: durableText, draft_revision: String(durableRevision) } });
    },
  } as unknown as ActionContext;
  const draft = sideChatDraftForState(ui, current)!;
  return { ui, draft, context, lifecycle, calls, errors,
    get current() { return current; },
    get durableText() { return durableText; },
    setBeforeAccept(callback: (() => void) | null) { beforeAccept = callback; },
    endComposition() {
      const release = lifecycle.captureCompositionEnd()?.();
      if (release?.deferred) current = release.deferred;
    },
    switchOwner() { current = { ...current, draft_target: { ...current.draft_target, sessionId: "session-b" },
      side_chat: { ...current.side_chat, owner_session_id: "session-b", chat_id: "side-b" } }; },
  };
}

test("a paused Side IME saves only its final confirmed text with the initial CAS revision", async () => {
  const f = fixture();
  f.lifecycle.beginComposition();
  updateSideChatDraftFromManualEdit(f.draft, "にほん");
  const first = persistSideChatDraft(f.current, f.context);
  const pending = [first];
  try {
    await settleMicrotasks();
    updateSideChatDraftFromManualEdit(f.draft, "日本");
    pending.push(persistSideChatDraft(f.current, f.context));
    await settleMicrotasks();
    assert.deepEqual(f.calls, [], "450ms debounce during conversion cannot persist an unconfirmed candidate");
    assert.equal(f.draft.saveInFlight, true);
    f.endComposition();
    await Promise.all(pending);
    assert.deepEqual(f.calls, [{ name: "save_side_chat_draft", expected: "0", text: "日本" }]);
    assert.deepEqual(f.errors, []);
    assert.equal(f.durableText, "日本");
    assert.equal(f.draft.persistedRevision, "1");
    assert.equal(f.draft.saveInFlight, false);
    assert.equal(f.ui.drafts.prompt, "unchanged main draft");
  } finally { f.endComposition(); await Promise.allSettled(pending); }
});

test("Side autosave keeps its single-flight owner until a deferred receipt advances its revision", async () => {
  const f = fixture();
  f.setBeforeAccept(() => { f.setBeforeAccept(null); f.lifecycle.beginComposition(); });
  updateSideChatDraftFromManualEdit(f.draft, "にほん");
  const pending = [persistSideChatDraft(f.current, f.context)];
  try {
    await settleMicrotasks();
    assert.equal(f.current.side_chat.draft_revision, "0", "successful response is still held by composition");
    assert.equal(f.draft.saveInFlight, true, "receipt awaiting projection is still owned");
    updateSideChatDraftFromManualEdit(f.draft, "日本");
    pending.push(persistSideChatDraft(f.current, f.context));
    await settleMicrotasks();
    assert.equal(f.calls.length, 1, "a second debounce cannot reuse revision zero while the first receipt is held");
    f.endComposition();
    await Promise.all(pending);
    assert.deepEqual(f.calls, [
      { name: "save_side_chat_draft", expected: "0", text: "にほん" },
      { name: "save_side_chat_draft", expected: "1", text: "日本" },
    ]);
    assert.deepEqual(f.errors, []);
    assert.equal(f.draft.persistedRevision, "2");
    assert.equal(f.draft.text, "日本");
    assert.equal(f.ui.drafts.prompt, "unchanged main draft");
  } finally { f.endComposition(); await Promise.allSettled(pending); }
});

test("Side autosave revalidates its owner after composition rather than writing into a different chat", async () => {
  const f = fixture();
  f.lifecycle.beginComposition();
  updateSideChatDraftFromManualEdit(f.draft, "日本");
  const pending = persistSideChatDraft(f.current, f.context);
  try {
    await settleMicrotasks();
    f.switchOwner();
    f.endComposition();
    await pending;
    assert.deepEqual(f.calls, []);
    assert.equal(f.draft.text, "日本");
    assert.equal(f.draft.saveInFlight, false);
  } finally { f.endComposition(); await Promise.allSettled([pending]); }
});

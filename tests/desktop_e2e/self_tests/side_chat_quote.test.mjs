import assert from "node:assert/strict";
import test from "node:test";
import { sideChatStoppedSurfaceReady } from "../scenarios/side_chat_quote.mjs";

const TARGET = { ownerSessionId: "owner-a", chatId: "chat-a", expectedGeneration: "2" };
function stoppedSurface() {
  return {
    projection: { side_chat: {
      owner_session_id: TARGET.ownerSessionId, chat_id: TARGET.chatId, generation: TARGET.expectedGeneration,
      status: "cancelled", deleting: false, can_send: true, can_cancel: false,
      phase: "", last_error: "run stopped by user", draft_text: "", draft_quote: null,
    } },
    side: {
      pane_count: 1, pane_visible: true, setup_visible: false, owner_session_id: TARGET.ownerSessionId,
      status_count: 1, status_visible: true, status_text: "停止済み",
      notice_texts: ["サイドチャットの実行を停止しました。"],
      stop_count: 0, stop_visible: false, stop_enabled: false,
      prompt_visible: true, prompt_enabled: true, prompt_value: "",
      send_visible: true, send_enabled: false, pending_count: 0, error_count: 0,
    },
    visible_fatal_count: 0, visible_recoverable_error_count: 0,
  };
}

test("Side stop waits for the visible same-owner terminal UI after backend cancellation", () => {
  assert.equal(sideChatStoppedSurfaceReady(stoppedSurface(), TARGET), true);
  const early = stoppedSurface();
  Object.assign(early.side, {
    status_text: "実行中 · stop requested", stop_count: 1, stop_visible: true, stop_enabled: true,
    notice_texts: [],
  });
  assert.equal(sideChatStoppedSurfaceReady(early, TARGET), false, "the previously premature stopped screenshot must fail");
});

test("Side stop rejects wrong owners, leftover actions, unavailable input and non-terminal feedback", () => {
  for (const change of [
    value => { value.projection.side_chat.owner_session_id = "other"; },
    value => { value.projection.side_chat.chat_id = "other"; },
    value => { value.projection.side_chat.generation = "3"; },
    value => { value.projection.side_chat.status = "running"; },
    value => { value.projection.side_chat.deleting = true; },
    value => { value.projection.side_chat.can_cancel = true; },
    value => { value.projection.side_chat.phase = "stop requested"; },
    value => { value.projection.side_chat.last_error = "storage failure"; },
    value => { value.projection.side_chat.draft_text = "unexpected"; },
    value => { value.side.owner_session_id = "other"; },
    value => { value.side.pane_count = 2; },
    value => { value.side.pane_visible = false; },
    value => { value.side.status_count = 2; },
    value => { value.side.status_visible = false; },
    value => { value.side.status_text = "実行中"; },
    value => { value.side.notice_texts = []; },
    value => { value.side.stop_count = 1; },
    value => { value.side.stop_visible = true; },
    value => { value.side.prompt_enabled = false; },
    value => { value.side.prompt_value = "wrong draft"; },
    value => { value.side.send_visible = false; },
    value => { value.side.send_enabled = true; },
    value => { value.side.pending_count = 1; },
    value => { value.side.error_count = 1; },
    value => { value.visible_recoverable_error_count = 1; },
  ]) {
    const invalid = stoppedSurface(); change(invalid);
    assert.equal(sideChatStoppedSurfaceReady(invalid, TARGET), false);
  }
});

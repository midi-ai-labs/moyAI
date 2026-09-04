import assert from "node:assert/strict";
import test from "node:test";
import {
  SIDE_SESSION_MAIN_DRAFT,
  SIDE_SESSION_SYSTEM_PROMPT_MARKER,
  SIDE_SESSION_UNSENT_DRAFT,
  sideSessionBindingSnapshot,
  sideSessionDraftSaved,
  sideSessionFreshBinding,
  sideSessionLedgerMatches,
  sideSessionMainSnapshot,
  sideSessionNavigationSummary,
  sideSessionRestored,
  sideSessionTerminalNavigationMatches,
  waitForSideSessionSelection,
} from "../scenarios/side_chat_session.mjs";

const ALPHA = "00000000000000000000000001";
const BETA = "00000000000000000000000002";
const CHAT = "00000000000000000000000003";

test("session selection waits without input for exact rendered navigation readiness", async () => {
  const ready = {
    count: 1, connected: true, visible: true, enabled: true,
    identity: { tag: "BUTTON", action: "session", focusKey: `session:${ALPHA}:select` },
  };
  const observations = [{ count: 0 }, { ...ready, enabled: false }, ready];
  const result = await waitForSideSessionSelection({
    async observeExactTarget(locator) {
      assert.deepEqual(locator.identity, ready.identity);
      return { observation: observations.shift() };
    },
  }, ALPHA);
  assert.equal(result.value.classified.decision, "pass");
  assert.equal(observations.length, 0);
  for (const invalid of [{ ...ready, count: 2 }, { ...ready, identity: { ...ready.identity, focusKey: `session:${BETA}:select` } }]) {
    await assert.rejects(waitForSideSessionSelection({
      async observeExactTarget() { return { observation: invalid }; },
    }, ALPHA), { owner: "product", code: "side-chat-session-navigation-owner" });
  }
});

function surface(sessionId = ALPHA) {
  const title = sessionId === ALPHA ? "Session Alpha" : "Session Beta";
  const label = `${title} [完了] ${sessionId.slice(0, 8)}`;
  const messages = [
    { id: "side-user", role: "user", content: "このセッションの決定事項は？" },
    { id: "side-assistant", role: "assistant", content: "青、金曜日です。" },
  ];
  return {
    projection: {
      draft_target: { sessionId }, selected_project_index: 0, selected_session_index: 0,
      current_session_label: title, selected_session_title: label,
      project_rows: [{ project_id: "project" }],
      session_rows: [{
        session_id: sessionId, title, status: "completed", loaded_status: "idle",
        active_turn_id: null, active_turn_sequence_no: null, interrupt_target: null,
        pending_permission_requests: 0, pending_user_input_requests: 0, label,
      }],
      transcript_rows: [
        { stable_history_identity: "user", row_kind: "user", body: "青、金曜日" },
        { stable_history_identity: "assistant", row_kind: "assistant", body: "記録しました。" },
      ],
      run_status_key: "completed", task_activity_state: "idle", busy: false,
      navigation_loading: false, post_run_refresh_pending: false, can_cancel_run: false,
      side_chat: {
        owner_session_id: sessionId, chat_id: CHAT, generation: "1", provider_profile: "openai_responses",
        base_url: "http://127.0.0.1:1234", model: "test", context_as_of_append_position: "11",
        system_prompt: SIDE_SESSION_SYSTEM_PROMPT_MARKER,
        draft_revision: "3", draft_text: SIDE_SESSION_UNSENT_DRAFT, messages,
        configured: true, deleting: false, status: "completed", last_error: "", context_scope: "owner_session",
        context_truncated: false, can_send: true, can_cancel: false,
        draft_quote: null,
      },
    },
    main: {
      prompt_value: SIDE_SESSION_MAIN_DRAFT,
      primary_rows: [
        { id: "user", kind: "user", body: "青、金曜日" },
        { id: "assistant", kind: "assistant", body: "記録しました。" },
      ],
    },
    side: {
      pane_count: 1, pane_visible: true, setup_visible: false, owner_session_id: sessionId,
      prompt_value: SIDE_SESSION_UNSENT_DRAFT, messages: structuredClone(messages), pending_count: 0,
    },
    visible_fatal_count: 0, visible_recoverable_error_count: 0,
  };
}

test("session Side restoration requires the exact binding, main history, draft, and live DOM", async (t) => {
  const current = surface();
  const expected = {
    main: sideSessionMainSnapshot(current.projection),
    binding: sideSessionBindingSnapshot(current.projection.side_chat),
    mainDraft: SIDE_SESSION_MAIN_DRAFT,
  };
  assert.equal(sideSessionRestored(current, expected), true);
  const mutations = {
    "selected B": (value) => { value.projection.session_rows[0].session_id = BETA; },
    "stale navigation title": (value) => { value.projection.session_rows[0].title = "new chat"; },
    "stale navigation status": (value) => { value.projection.session_rows[0].status = "running"; },
    "stale loaded status": (value) => { value.projection.session_rows[0].loaded_status = "active"; },
    "stale active turn": (value) => { value.projection.session_rows[0].active_turn_id = CHAT; },
    "stale active turn sequence": (value) => { value.projection.session_rows[0].active_turn_sequence_no = 3; },
    "stale interrupt target": (value) => { value.projection.session_rows[0].interrupt_target = { kind: "root" }; },
    "stale pending permission": (value) => { value.projection.session_rows[0].pending_permission_requests = 1; },
    "stale pending user input": (value) => { value.projection.session_rows[0].pending_user_input_requests = 1; },
    "stale selected heading": (value) => { value.projection.selected_session_title = "new chat [実行中]"; },
    "draft owner B": (value) => { value.projection.draft_target.sessionId = BETA; },
    "side owner B": (value) => { value.projection.side_chat.owner_session_id = BETA; },
    "new hidden chat": (value) => { value.projection.side_chat.chat_id = BETA; },
    "system prompt changed": (value) => { value.projection.side_chat.system_prompt = "wrong prompt"; },
    "main history contaminated": (value) => { value.projection.transcript_rows.push({ row_kind: "assistant", body: "Side answer" }); },
    "main draft changed": (value) => { value.main.prompt_value = ""; },
    "stale rendered main history": (value) => { value.main.primary_rows[0].body = "赤、月曜日"; },
    "persisted side draft missing": (value) => { value.projection.side_chat.draft_text = ""; },
    "rendered draft missing": (value) => { value.side.prompt_value = ""; },
    "rendered side message missing": (value) => { value.side.messages.pop(); },
    "stale rendered owner": (value) => { value.side.owner_session_id = BETA; },
    "hidden pane": (value) => { value.side.pane_visible = false; },
    "provider error": (value) => { value.projection.side_chat.last_error = "failed"; },
    "main still running": (value) => { value.projection.run_status_key = "running"; },
    "unexpected quote": (value) => { value.side.pending_count = 1; },
  };
  for (const [name, mutate] of Object.entries(mutations)) {
    await t.test(name, () => {
      const invalid = structuredClone(current);
      mutate(invalid);
      assert.equal(sideSessionRestored(invalid, expected), false);
    });
  }
  const restarted = structuredClone(current);
  restarted.main.prompt_value = "";
  assert.equal(sideSessionRestored(restarted, { ...expected, mainDraft: "" }), true);
});

test("first open for B materializes global defaults without leaking A conversation or draft", () => {
  const beta = surface(BETA);
  const main = sideSessionMainSnapshot(beta.projection);
  beta.main.prompt_value = "";
  Object.assign(beta.projection.side_chat, {
    chat_id: BETA,
    messages: [],
    draft_text: "",
    status: "idle",
    can_send: true,
  });
  Object.assign(beta.side, { setup_visible: false, messages: [], prompt_value: "" });
  const expected = {
    previousChatId: CHAT,
    providerBaseUrl: "http://127.0.0.1:1234",
    model: "test",
    systemPrompt: SIDE_SESSION_SYSTEM_PROMPT_MARKER,
  };
  assert.equal(sideSessionFreshBinding(beta, main, expected), true);
  const leaked = structuredClone(beta);
  leaked.projection.side_chat.owner_session_id = ALPHA;
  assert.equal(sideSessionFreshBinding(leaked, main, expected), false);
  const reusedChat = structuredClone(beta);
  reusedChat.projection.side_chat.chat_id = CHAT;
  assert.equal(sideSessionFreshBinding(reusedChat, main, expected), false);
  const leakedHistory = structuredClone(beta);
  leakedHistory.projection.side_chat.messages = surface().projection.side_chat.messages;
  assert.equal(sideSessionFreshBinding(leakedHistory, main, expected), false);
  const hidden = structuredClone(beta);
  hidden.side.pane_visible = false;
  assert.equal(sideSessionFreshBinding(hidden, main, expected), false);
  const staleDom = structuredClone(beta);
  staleDom.side.messages = surface().side.messages;
  assert.equal(sideSessionFreshBinding(staleDom, main, expected), false);
  const staleGlobal = structuredClone(beta);
  staleGlobal.projection.side_chat.system_prompt = "old prompt";
  assert.equal(sideSessionFreshBinding(staleGlobal, main, expected), false);
  const storageError = structuredClone(beta);
  storageError.projection.side_chat.status = "failed";
  storageError.projection.side_chat.last_error = "storage read failed";
  assert.equal(sideSessionFreshBinding(storageError, main, expected), false);
});

test("terminal navigation summary rejects stale rows by stable session identity", async (t) => {
  const alpha = surface(ALPHA);
  const beta = surface(BETA);
  const sessions = [sideSessionMainSnapshot(alpha.projection), sideSessionMainSnapshot(beta.projection)];
  const projection = structuredClone(alpha.projection);
  projection.session_rows = [
    structuredClone(beta.projection.session_rows[0]),
    structuredClone(alpha.projection.session_rows[0]),
  ];

  assert.equal(sideSessionTerminalNavigationMatches(projection, sessions), true);
  assert.deepEqual(
    sideSessionNavigationSummary(projection, sessions).map((row) => row.session_id),
    [ALPHA, BETA],
    "the summary follows requested stable identities instead of row order",
  );

  const mutations = {
    "missing sibling": (value) => { value.session_rows.shift(); },
    "duplicate sibling": (value) => { value.session_rows.push(structuredClone(value.session_rows[0])); },
    "placeholder title": (value) => { value.session_rows[0].title = "new chat"; },
    "stale visible label": (value) => { value.session_rows[0].label = `new chat [実行中] ${BETA.slice(0, 8)}`; },
    "running status": (value) => { value.session_rows[0].status = "running"; },
    "active loaded status": (value) => { value.session_rows[0].loaded_status = "active"; },
    "active turn id": (value) => { value.session_rows[0].active_turn_id = CHAT; },
    "turn sequence": (value) => { value.session_rows[0].active_turn_sequence_no = 3; },
    "interrupt target": (value) => { value.session_rows[0].interrupt_target = { kind: "root" }; },
    "pending permission": (value) => { value.session_rows[0].pending_permission_requests = 1; },
    "pending user input": (value) => { value.session_rows[0].pending_user_input_requests = 1; },
  };
  for (const [name, mutate] of Object.entries(mutations)) {
    await t.test(name, () => {
      const invalid = structuredClone(projection);
      mutate(invalid);
      assert.equal(sideSessionTerminalNavigationMatches(invalid, sessions), false);
    });
  }

  const placeholderProjection = structuredClone(projection);
  const placeholderSessions = structuredClone(sessions);
  placeholderSessions[0].title = "新規チャット";
  const placeholderRow = placeholderProjection.session_rows.find((row) => row.session_id === ALPHA);
  placeholderRow.title = "新規チャット";
  placeholderRow.label = `新規チャット [完了] ${ALPHA.slice(0, 8)}`;
  assert.equal(
    sideSessionTerminalNavigationMatches(placeholderProjection, placeholderSessions),
    false,
    "a placeholder captured as both the canonical expectation and rendered row must still fail",
  );
});

test("Side autosave advances only the draft and cannot redefine the completed conversation", () => {
  const current = surface();
  const completed = sideSessionBindingSnapshot(current.projection.side_chat);
  completed.draft_text = "";
  completed.draft_revision = "2";
  assert.equal(sideSessionDraftSaved(current, completed, SIDE_SESSION_UNSENT_DRAFT), true);
  const changedChat = structuredClone(current);
  changedChat.projection.side_chat.chat_id = BETA;
  assert.equal(sideSessionDraftSaved(changedChat, completed, SIDE_SESSION_UNSENT_DRAFT), false);
  const changedHistory = structuredClone(current);
  changedHistory.projection.side_chat.messages.push({ id: "extra", role: "assistant", content: "unexpected" });
  assert.equal(sideSessionDraftSaved(changedHistory, completed, SIDE_SESSION_UNSENT_DRAFT), false);
  const noRevision = structuredClone(current);
  noRevision.projection.side_chat.draft_revision = "2";
  assert.equal(sideSessionDraftSaved(noRevision, completed, SIDE_SESSION_UNSENT_DRAFT), false);
});

test("session provider ledger admits only exact ordered successful requests", () => {
  const ledger = [
    "side_session_alpha",
    "side_session_beta",
    "side_session_main_after_config",
    "side_session_consult",
  ].map((role) => ({
    method: "POST",
    pathname: "/v1/responses",
    contract: {
      role,
      pass: true,
      consult_system_prompt_marker_present: role === "side_session_consult",
      consult_system_prompt_marker_exactly_once: role === "side_session_consult",
    },
    response_status: 200, response_phase: "completed",
  }));
  assert.equal(sideSessionLedgerMatches(ledger, 4), true);
  assert.equal(sideSessionLedgerMatches([...ledger, ledger[3]], 4), false);
  assert.equal(sideSessionLedgerMatches(ledger.slice(0, 3), 4), false);
  assert.equal(sideSessionLedgerMatches([ledger[1], ledger[0], ledger[2], ledger[3]], 4), false);
  const rejected = structuredClone(ledger);
  rejected[3].contract.pass = false;
  assert.equal(sideSessionLedgerMatches(rejected, 4), false);
  const missingMarker = structuredClone(ledger);
  missingMarker[3].contract.consult_system_prompt_marker_present = false;
  missingMarker[3].contract.consult_system_prompt_marker_exactly_once = false;
  assert.equal(sideSessionLedgerMatches(missingMarker, 4), false);
  const duplicateMarker = structuredClone(ledger);
  duplicateMarker[3].contract.consult_system_prompt_marker_exactly_once = false;
  assert.equal(sideSessionLedgerMatches(duplicateMarker, 4), false);
  const leakedToMain = structuredClone(ledger);
  leakedToMain[2].contract.consult_system_prompt_marker_present = true;
  leakedToMain[2].contract.consult_system_prompt_marker_exactly_once = true;
  assert.equal(sideSessionLedgerMatches(leakedToMain, 4), false);
});

import assert from "node:assert/strict";
import test from "node:test";

import {
  case52Stage5ActiveContextMatches,
  case52Stage5MainMatches,
  case52Stage5MainSnapshot,
  case52Stage5Ready,
  case52Stage5TerminalMatches,
  classifyCase52Stage5CommandError,
  executeCase52SideChatStage,
} from "../scenarios/case5_2_side_chat.mjs";

const SESSION = "01K47MTESTSESSION00000000000";
const CHAT = "01K47MTESTSIDECHAT000000000";
const PROFILE = "openai_responses";
const BASE_URL = "http://127.0.0.1:19431";
const MODEL = "stage5-model";
const QUESTION = "このセッションの設計判断と回帰結果を、根拠とともに要約してください。";
const QUESTION_INPUT = `${QUESTION}\n`;
const ANSWER = "設計判断と回帰結果を同じセッション履歴から確認しました。";

function surface({
  paneVisible = true,
  status = "idle",
  draft = "",
  generation = "0",
  draftRevision = draft === "" ? "0" : "1",
  messages = [],
  renderedMessages = null,
  owner = SESSION,
  chat = CHAT,
  selectedSession = SESSION,
  mainDraft = "未送信の Main メモ",
  primaryBody = "巨大セッションの確定済み応答",
  visiblePrimaryBody = primaryBody,
  lastError = "",
  contextTruncated = false,
  contextScope = "owner_session",
  appendPosition = "194",
  canonicalExtraRows = [],
} = {}) {
  const sideMessages = messages.map((message) => ({ ...message }));
  const domMessages = (renderedMessages ?? messages).map((message) => ({ ...message }));
  return {
    projection: {
      draft_target: { sessionId: SESSION },
      run_status_key: "completed",
      task_activity_state: "idle",
      busy: false,
      agent_tree_active: false,
      post_run_refresh_pending: false,
      navigation_loading: false,
      selected_project_index: 0,
      selected_session_index: 0,
      session_rows: [{
        session_id: selectedSession,
        active_turn_id: null,
        admission_revision: "7",
        latest_turn_id: "01K47MTESTTURN0000000000000",
      }],
      turn_page_total: 21,
      turn_page_limit: 4,
      transcript_rows: [{
        stable_history_identity: "01K47MTESTHISTORY000000000",
        row_kind: "assistant",
        body: primaryBody,
      }, ...canonicalExtraRows],
      side_chat: {
        configured: true,
        owner_session_id: owner,
        chat_id: chat,
        generation,
        draft_revision: draftRevision,
        context_as_of_append_position: appendPosition,
        provider_profile: PROFILE,
        base_url: BASE_URL,
        model: MODEL,
        status,
        last_error: lastError,
        can_send: ["idle", "completed"].includes(status),
        can_cancel: status === "running",
        draft_text: draft,
        draft_quote: null,
        messages: sideMessages,
        context_scope: contextScope,
        context_truncated: contextTruncated,
      },
    },
    main: {
      primary_rows: [{
        id: "01K47MTESTHISTORY000000000",
        kind: "assistant",
        body: visiblePrimaryBody,
      }],
      prompt_value: mainDraft,
    },
    side: {
      pane_count: 1,
      pane_visible: paneVisible,
      owner_session_id: owner,
      prompt_value: draft,
      prompt_visible: paneVisible,
      prompt_enabled: true,
      send_visible: paneVisible,
      send_enabled: paneVisible && ["idle", "completed"].includes(status) && draft !== "",
      stop_visible: paneVisible,
      stop_enabled: paneVisible && status === "running",
      metadata: ["参照: このタスクの履歴", "履歴位置: 194"],
      truncated_count: contextTruncated ? 1 : 0,
      pending_count: 0,
      messages: domMessages,
    },
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
  };
}

function readyExpected(main) {
  return {
    main,
    session_id: SESSION,
    provider_profile: PROFILE,
    provider_base_url: BASE_URL,
    model: MODEL,
  };
}

function completedTerminalSurface({ staleDom = false, answer = ANSWER } = {}) {
  const completed = surface({
    status: "completed",
    generation: "1",
    draftRevision: "1",
    messages: [
      { id: "side-user", role: "user", content: QUESTION },
      { id: "side-assistant", role: "assistant", content: answer },
    ],
    renderedMessages: staleDom ? [
      { id: "side-user", role: "user", content: QUESTION },
      { id: "side-assistant-stream", role: "assistant", content: "Side" },
    ] : null,
  });
  if (staleDom) completed.side.stop_enabled = true;
  return completed;
}

function terminalDomExecution(observeTerminal, { timeoutMs = 20_000 } = {}) {
  const state = {
    phase: "ready",
    now: 1_000,
    calls: [],
    sleeps: [],
    observations: 0,
    removed: false,
  };
  const run = () => executeCase52SideChatStage({
    cdp: {}, input: {}, sink: { async record() {}, async writeJson() { return {}; } },
    sessionId: SESSION, providerProfile: PROFILE, providerBaseUrl: BASE_URL, model: MODEL,
    promptInput: { text: QUESTION }, timeoutMs,
  }, {
    now: () => state.now,
    sleep: async (milliseconds) => {
      state.sleeps.push(milliseconds);
      state.now += milliseconds;
    },
    observe: async () => {
      if (state.phase === "ready") return surface();
      if (state.phase === "draft") return surface({ draft: QUESTION });
      const value = observeTerminal(state.observations, state);
      state.observations += 1;
      return value;
    },
    waitForStage: async ({ sample, accept }) => {
      const value = await sample();
      assert.equal(await accept(value), true);
      return { value };
    },
    click: async (_input, locator) => {
      if (locator.identity.action === "send-side-chat") {
        state.phase = "terminal";
        state.calls.push({
          sequence: 1,
          command: "submit_side_chat",
          args: {
            ownerSessionId: SESSION,
            chatId: CHAT,
            expectedGeneration: "0",
            expectedDraftRevision: "1",
            expectedOwnerAppendPosition: "194",
            quote: null,
            text: QUESTION,
          },
        });
      }
      return {};
    },
    insert: async () => { state.phase = "draft"; },
    createCommandProbe: () => ({
      async install() {},
      async snapshot(afterSequence = 0) {
        return {
          found: true,
          sequence: state.calls.length,
          dropped_through: 0,
          calls: state.calls.filter((call) => call.sequence > afterSequence),
        };
      },
      async remove() { state.removed = true; },
    }),
    screenshot: async ({ name }) => ({ relative_path: `${name}.png`, sha256: "9".repeat(64) }),
  });
  return { run, state };
}

test("Stage5 readiness fixes the exact selected Main owner and one empty configured Side conversation", () => {
  const current = surface();
  const main = case52Stage5MainSnapshot(current);
  assert.equal(case52Stage5MainMatches(current, main), true);
  assert.equal(case52Stage5Ready(current, readyExpected(main)), true);
});

test("Stage5 readiness rejects non-empty or cross-session Side state and any Main drift", async (t) => {
  const baseline = case52Stage5MainSnapshot(surface());
  const variants = [
    ["wrong owner", surface({ owner: "01K47MTESTFOREIGN000000000" })],
    ["wrong selected session", surface({ selectedSession: "01K47MTESTFOREIGN000000000" })],
    ["existing messages", surface({ messages: [{ id: "s1", role: "assistant", content: "old" }] })],
    ["existing draft", surface({ draft: "old" })],
    ["running", surface({ status: "running" })],
    ["hidden pane", surface({ paneVisible: false })],
    ["visible Main divergence", surface({ visiblePrimaryBody: "stale DOM" })],
    ["canonical Main divergence", surface({ primaryBody: "different canonical row" })],
    ["non-primary canonical Main divergence", surface({ canonicalExtraRows: [{
      stable_history_identity: "01K47MTESTTOOL0000000000000",
      row_kind: "tool_output",
      body: "changed tool row",
    }] })],
    ["missing append fence", surface({ appendPosition: null })],
    ["wrong context scope", surface({ contextScope: "workspace" })],
  ];
  for (const [name, candidate] of variants) {
    await t.test(name, () => {
      assert.equal(case52Stage5Ready(candidate, readyExpected(baseline)), false);
    });
  }
});

test("Stage5 terminal accepts a nonempty answer without prescribing answer keywords", () => {
  const initial = surface();
  const main = case52Stage5MainSnapshot(initial);
  const completed = surface({
    status: "completed",
    generation: "1",
    messages: [
      { id: "side-user", role: "user", content: QUESTION },
      { id: "side-assistant", role: "assistant", content: "- 任意の有効な応答" },
    ],
    renderedMessages: [
      { id: "side-user", role: "user", content: QUESTION },
      { id: "side-assistant", role: "assistant", content: "任意の有効な応答" },
    ],
  });
  assert.equal(case52Stage5TerminalMatches(completed, {
    ...readyExpected(main),
    chat_id: CHAT,
    question: QUESTION,
    generation: "0",
    context_as_of_append_position: "194",
  }), true);
});

test("Stage5 active context binds the exact owner fence and rendered truncation state", () => {
  const running = surface({ status: "running", contextTruncated: true });
  const expected = { session_id: SESSION, chat_id: CHAT, as_of_append_position: "194" };
  assert.equal(case52Stage5ActiveContextMatches(running, expected), true);
  const wrongFence = structuredClone(running);
  wrongFence.projection.side_chat.context_as_of_append_position = "195";
  assert.equal(case52Stage5ActiveContextMatches(wrongFence, expected), false);
  const staleDom = structuredClone(running);
  staleDom.side.truncated_count = 0;
  assert.equal(case52Stage5ActiveContextMatches(staleDom, expected), false);
  const wrongScope = structuredClone(running);
  wrongScope.projection.side_chat.context_scope = "workspace";
  assert.equal(case52Stage5ActiveContextMatches(wrongScope, expected), false);
});

test("Stage5 terminal rejects changed question, empty answer, control tokens, error, binding drift, and Main drift", async (t) => {
  const main = case52Stage5MainSnapshot(surface());
  const expected = {
    ...readyExpected(main),
    chat_id: CHAT,
    question: QUESTION,
    generation: "0",
    context_as_of_append_position: "194",
  };
  const messages = (answer = ANSWER, question = QUESTION) => [
    { id: "side-user", role: "user", content: question },
    { id: "side-assistant", role: "assistant", content: answer },
  ];
  const staleFinalControls = surface({ status: "completed", generation: "1", messages: messages() });
  staleFinalControls.side.stop_enabled = true;
  const variants = [
    ["unchanged generation", surface({ status: "completed", messages: messages() })],
    ["changed question", surface({ status: "completed", generation: "1", messages: messages(ANSWER, "other") })],
    ["empty answer", surface({ status: "completed", generation: "1", messages: messages("   ") })],
    ["control token", surface({ status: "completed", generation: "1", messages: messages("<|im_end|>") })],
    ["provider error", surface({ status: "completed", generation: "1", messages: messages(), lastError: "bad" })],
    ["changed chat", surface({ status: "completed", generation: "1", messages: messages(), chat: "01K47MTESTOTHERCHAT0000000" })],
    ["changed append fence", surface({ status: "completed", generation: "1", messages: messages(), appendPosition: "195" })],
    ["wrong context scope", surface({ status: "completed", generation: "1", messages: messages(), contextScope: "workspace" })],
    ["rendered message identity drift", surface({
      status: "completed",
      generation: "1",
      messages: messages(),
      renderedMessages: [
        { id: "other-user", role: "user", content: QUESTION },
        { id: "side-assistant", role: "assistant", content: ANSWER },
      ],
    })],
    ["stale final controls", staleFinalControls],
    ["changed Main", surface({ status: "completed", generation: "1", messages: messages(), mainDraft: "replaced" })],
  ];
  for (const [name, candidate] of variants) {
    await t.test(name, () => assert.equal(case52Stage5TerminalMatches(candidate, expected), false));
  }
});

test("executeCase52SideChatStage sends one exact Side command and records active context and bounded identities", async () => {
  let phase = "hidden";
  let runningObservations = 0;
  let now = 1_000;
  const records = [];
  const writes = [];
  const clicks = [];
  let removed = false;
  const sink = {
    async record(name, value) { records.push({ name, value }); },
    async writeJson(name, value) {
      writes.push({ name, value });
      return { relative_path: name, bytes: JSON.stringify(value).length, sha256: "f".repeat(64) };
    },
  };
  const observe = async () => {
    if (phase === "hidden") return surface({ paneVisible: false });
    if (phase === "ready") return surface();
    if (phase === "draft") return surface({ draft: QUESTION_INPUT });
    if (phase === "running") {
      runningObservations += 1;
      if (runningObservations === 1) return surface({
        status: "running",
        generation: "1",
        draft: "",
        draftRevision: "1",
        messages: [{ id: "side-user", role: "user", content: QUESTION }],
        contextTruncated: true,
      });
      phase = "completed";
    }
    return surface({
      status: "completed",
      generation: "1",
      draft: "",
      draftRevision: "1",
      messages: [
        { id: "side-user", role: "user", content: QUESTION },
        { id: "side-assistant", role: "assistant", content: ANSWER },
      ],
    });
  };
  const probe = {
    async install() { return { installed: true }; },
    async snapshot(afterSequence = 0) {
      const calls = ["running", "completed"].includes(phase) ? [{
        sequence: 1,
        command: "submit_side_chat",
        args: {
          ownerSessionId: SESSION,
          chatId: CHAT,
          expectedGeneration: "0",
          expectedDraftRevision: "1",
          expectedOwnerAppendPosition: "194",
          quote: null,
          text: QUESTION,
        },
      }] : [];
      return {
        found: true,
        sequence: calls.length,
        dropped_through: 0,
        calls: calls.filter((call) => call.sequence > afterSequence),
      };
    },
    async remove() { removed = true; return { removed: true }; },
  };
  const result = await executeCase52SideChatStage({
    cdp: {}, input: {}, sink, sessionId: SESSION,
    providerProfile: PROFILE, providerBaseUrl: BASE_URL, model: MODEL,
    promptInput: { text: QUESTION_INPUT }, timeoutMs: 10_000, evidenceName: "stage5-self-test",
  }, {
    now: () => now,
    sleep: async (milliseconds) => { now += milliseconds; },
    observe,
    waitForStage: async ({ sample, accept }) => {
      const value = await sample();
      assert.equal(await accept(value), true);
      return { value, elapsed_ms: 0 };
    },
    click: async (_input, locator) => {
      clicks.push(locator.identity.action);
      if (locator.identity.action === "show-side-chat-pane") phase = "ready";
      if (locator.identity.action === "send-side-chat") phase = "running";
      return { trusted: true, action: locator.identity.action };
    },
    insert: async (_input, _locator, text) => {
      assert.equal(text, QUESTION_INPUT);
      phase = "draft";
      return { trusted: true };
    },
    createCommandProbe: () => probe,
    screenshot: async ({ name }) => ({ relative_path: `${name}.png`, sha256: "e".repeat(64) }),
  });
  assert.equal(result.stage, "stage5");
  assert.equal(result.question, QUESTION);
  assert.equal(result.answer, ANSWER);
  assert.equal(result.binding.messages.length, 2);
  assert.equal(result.active_context_observation.truncated, true);
  assert.equal(result.first_progress_latency_ms, 500);
  assert.deepEqual(clicks, ["show-side-chat-pane", "send-side-chat"]);
  assert.equal(result.commandEvidence.calls.length, 1);
  assert.equal(removed, true);
  assert.equal(writes.length, 1);
  assert.equal(writes[0].value.main.primary_rows[0].body.bytes > 0, true);
  assert.equal(writes[0].value.answer.text, ANSWER);
  assert.equal(records.at(-1).name, "case5_2-stage5-side-chat-terminal");
});

test("executeCase52SideChatStage captures a running control-token leak and exact Side Cancel before product failure", async () => {
  let phase = "ready";
  let removed = false;
  const commandCalls = [];
  const clicks = [];
  const leakWrites = [];
  const initial = surface();
  const leaked = (status) => surface({
    status,
    generation: "1",
    draftRevision: "1",
    messages: [
      { id: "side-user", role: "user", content: QUESTION },
      { id: "side-assistant", role: "assistant", content: `bad <|im_start|> token` },
    ],
  });
  await assert.rejects(() => executeCase52SideChatStage({
    cdp: {}, input: {},
    sink: {
      async record() {},
      async writeJson(name, value) {
        leakWrites.push({ name, value });
        return { relative_path: name, sha256: "a".repeat(64) };
      },
    },
    sessionId: SESSION, providerProfile: PROFILE, providerBaseUrl: BASE_URL, model: MODEL,
    promptInput: { text: QUESTION }, timeoutMs: 10_000,
  }, {
    now: () => 1_000,
    sleep: async () => {},
    observe: async () => phase === "ready" ? initial
      : phase === "draft" ? surface({ draft: QUESTION })
        : leaked(phase === "cancelled" ? "cancelled" : "running"),
    waitForStage: async ({ sample, accept }) => {
      const value = await sample();
      assert.equal(await accept(value), true);
      return { value };
    },
    click: async (_input, locator) => {
      clicks.push(locator.identity.action);
      if (locator.identity.action === "send-side-chat") {
        phase = "running";
        commandCalls.push({
          sequence: 1,
          command: "submit_side_chat",
          args: {
            ownerSessionId: SESSION,
            chatId: CHAT,
            expectedGeneration: "0",
            expectedDraftRevision: "1",
            expectedOwnerAppendPosition: "194",
            quote: null,
            text: QUESTION,
          },
        });
      }
      if (locator.identity.action === "cancel-side-chat") {
        commandCalls.push({
          sequence: 2,
          command: "cancel_side_chat",
          args: { ownerSessionId: SESSION, chatId: CHAT, expectedGeneration: "1" },
        });
        phase = "cancelled";
      }
      return {};
    },
    insert: async () => { phase = "draft"; },
    createCommandProbe: () => ({
      async install() {},
      async snapshot(afterSequence = 0) {
        return {
          found: true,
          sequence: commandCalls.length,
          dropped_through: 0,
          calls: commandCalls.filter((call) => call.sequence > afterSequence),
        };
      },
      async remove() { removed = true; return { removed: true }; },
    }),
    screenshot: async ({ name }) => ({ relative_path: `${name}.png`, sha256: "b".repeat(64) }),
  }), (error) => {
    assert.equal(error?.owner, "product");
    assert.equal(error?.code, "case5_2-provider-control-token-leak");
    assert.equal(error?.evidence?.cancel_required, true);
    assert.equal(error?.evidence?.command_evidence?.calls?.length, 2);
    return true;
  });
  assert.deepEqual(clicks, ["send-side-chat", "cancel-side-chat"]);
  assert.equal(leakWrites.length, 1);
  assert.equal(Object.hasOwn(leakWrites[0].value.side, "provider_profile"), false);
  assert.equal(Object.hasOwn(leakWrites[0].value.side, "base_url"), false);
  assert.equal(Object.hasOwn(leakWrites[0].value.side, "model"), false);
  assert.equal(removed, true);
});

test("executeCase52SideChatStage records a completed control-token leak without claiming an unavailable Cancel", async () => {
  let phase = "ready";
  const calls = [];
  const clicks = [];
  await assert.rejects(() => executeCase52SideChatStage({
    cdp: {}, input: {},
    sink: {
      async record() {},
      async writeJson(name) { return { relative_path: name, sha256: "c".repeat(64) }; },
    },
    sessionId: SESSION, providerProfile: PROFILE, providerBaseUrl: BASE_URL, model: MODEL,
    promptInput: { text: QUESTION }, timeoutMs: 10_000,
  }, {
    now: () => 1_000,
    sleep: async () => {},
    observe: async () => phase === "ready" ? surface()
      : phase === "draft" ? surface({ draft: QUESTION })
        : surface({
          status: "completed",
          generation: "1",
          draftRevision: "1",
          messages: [
            { id: "side-user", role: "user", content: QUESTION },
            { id: "side-assistant", role: "assistant", content: "bad <|im_end|> token" },
          ],
        }),
    waitForStage: async ({ sample, accept }) => {
      const value = await sample();
      assert.equal(await accept(value), true);
      return { value };
    },
    click: async (_input, locator) => {
      clicks.push(locator.identity.action);
      if (locator.identity.action === "send-side-chat") {
        phase = "completed";
        calls.push({
          sequence: 1,
          command: "submit_side_chat",
          args: {
            ownerSessionId: SESSION,
            chatId: CHAT,
            expectedGeneration: "0",
            expectedDraftRevision: "1",
            expectedOwnerAppendPosition: "194",
            quote: null,
            text: QUESTION,
          },
        });
      }
      return {};
    },
    insert: async () => { phase = "draft"; },
    createCommandProbe: () => ({
      async install() {},
      async snapshot(afterSequence = 0) {
        return {
          found: true,
          sequence: calls.length,
          dropped_through: 0,
          calls: calls.filter((call) => call.sequence > afterSequence),
        };
      },
      async remove() {},
    }),
    screenshot: async ({ name }) => ({ relative_path: `${name}.png`, sha256: "d".repeat(64) }),
  }), (error) => {
    assert.equal(error?.owner, "product");
    assert.equal(error?.code, "case5_2-provider-control-token-leak");
    assert.equal(error?.evidence?.cancel_required, false);
    assert.equal(error?.evidence?.command_evidence?.calls?.length, 1);
    return true;
  });
  assert.deepEqual(clicks, ["send-side-chat"]);
});

test("executeCase52SideChatStage bounds a trusted Send that invokes no Side command", async () => {
  let phase = "ready";
  let admissionTimeout = null;
  let removed = false;
  await assert.rejects(() => executeCase52SideChatStage({
    cdp: {}, input: {}, sink: { async record() {}, async writeJson() { return {}; } },
    sessionId: SESSION, providerProfile: PROFILE, providerBaseUrl: BASE_URL, model: MODEL,
    promptInput: { text: QUESTION }, timeoutMs: 60_000,
  }, {
    now: () => 1_000,
    sleep: async () => {},
    observe: async () => phase === "draft" ? surface({ draft: QUESTION }) : surface(),
    waitForStage: async ({ label, timeoutMs, sample, accept, code }) => {
      const value = await sample();
      if (await accept(value)) return { value };
      admissionTimeout = { label, timeoutMs };
      throw Object.assign(new Error("bounded command admission timeout"), { code });
    },
    click: async (_input, locator) => {
      if (locator.identity.action === "send-side-chat") phase = "noop";
      return {};
    },
    insert: async () => { phase = "draft"; },
    createCommandProbe: () => ({
      async install() {},
      async snapshot() { return { found: true, sequence: 0, dropped_through: 0, calls: [] }; },
      async remove() { removed = true; },
    }),
    screenshot: async () => ({}),
  }), (error) => error?.code === "case5_2-stage5-submit-command");
  assert.deepEqual(admissionTimeout, {
    label: "case5_2 Stage5 exact Side submit command",
    timeoutMs: 10_000,
  });
  assert.equal(removed, true);
});

test("executeCase52SideChatStage classifies a wrong Side command as a product mismatch", async () => {
  let phase = "ready";
  const calls = [];
  let removed = false;
  await assert.rejects(() => executeCase52SideChatStage({
    cdp: {}, input: {}, sink: { async record() {}, async writeJson() { return {}; } },
    sessionId: SESSION, providerProfile: PROFILE, providerBaseUrl: BASE_URL, model: MODEL,
    promptInput: { text: QUESTION_INPUT }, timeoutMs: 60_000,
  }, {
    now: () => 1_000,
    sleep: async () => {},
    observe: async () => phase === "draft" ? surface({ draft: QUESTION_INPUT }) : surface(),
    waitForStage: async ({ sample, accept }) => {
      const value = await sample();
      assert.equal(await accept(value), true);
      return { value };
    },
    click: async (_input, locator) => {
      if (locator.identity.action === "send-side-chat") {
        phase = "wrong";
        calls.push({
          sequence: 1,
          command: "submit_side_chat",
          args: {
            ownerSessionId: SESSION,
            chatId: CHAT,
            expectedGeneration: "0",
            expectedDraftRevision: "1",
            expectedOwnerAppendPosition: "194",
            quote: null,
            text: QUESTION_INPUT,
          },
        });
      }
      return {};
    },
    insert: async () => { phase = "draft"; },
    createCommandProbe: () => ({
      async install() {},
      async snapshot(afterSequence = 0) {
        return {
          found: true,
          sequence: calls.length,
          dropped_through: 0,
          calls: calls.filter((call) => call.sequence > afterSequence),
        };
      },
      async remove() { removed = true; },
    }),
    screenshot: async () => ({}),
  }), (error) => error?.owner === "product" && error?.code === "case5_2-stage5-command-mismatch");
  assert.equal(removed, true);
});

test("Stage5 command classification preserves probe-integrity failures as harness-owned inputs", () => {
  for (const code of [
    "desktop-command-probe-snapshot-invalid",
    "desktop-command-probe-overflow",
    "desktop-command-probe-order",
  ]) {
    const integrityFailure = Object.assign(new Error("probe integrity failed"), { code });
    assert.equal(classifyCase52Stage5CommandError(integrityFailure, "terminal"), integrityFailure);
  }
  for (const code of ["desktop-command-probe-cardinality", "desktop-command-probe-call-mismatch"]) {
    const mismatch = Object.assign(new Error("product command mismatch"), { code });
    const classified = classifyCase52Stage5CommandError(mismatch, "terminal");
    assert.equal(classified.owner, "product");
    assert.equal(classified.code, "case5_2-stage5-command-mismatch");
  }
});

test("executeCase52SideChatStage waits for bounded Side DOM settlement after canonical completion", async () => {
  const { run, state } = terminalDomExecution((index) => completedTerminalSurface({ staleDom: index < 2 }));
  const result = await run();
  assert.equal(result.answer, ANSWER);
  assert.equal(result.terminal_dom_settle_ms, 1_000);
  assert.deepEqual(state.sleeps, [500, 500]);
  assert.equal(state.observations, 3);
  assert.equal(state.removed, true);
});

test("executeCase52SideChatStage bounds a persistent completed-canonical Side DOM mismatch", async () => {
  const { run, state } = terminalDomExecution(() => completedTerminalSurface({ staleDom: true }));
  await assert.rejects(run, (error) => {
    assert.equal(error?.owner, "product");
    assert.equal(error?.code, "case5_2-stage5-terminal-dom-settle-timeout");
    assert.equal(error?.evidence?.grace_ms, 5_000);
    assert.equal(error?.evidence?.elapsed_ms, 5_000);
    assert.equal(error?.evidence?.observed?.status, "completed");
    assert.equal(error?.evidence?.dom?.stop_enabled, true);
    assert.equal(error?.evidence?.dom?.messages?.[1]?.id, "side-assistant-stream");
    return true;
  });
  assert.equal(state.sleeps.length, 10);
  assert.equal(state.observations, 10);
  assert.equal(state.removed, true);
});

test("executeCase52SideChatStage fails fast on an immutable malformed completed terminal", async () => {
  let phase = "ready";
  const calls = [];
  let sleeps = 0;
  await assert.rejects(() => executeCase52SideChatStage({
    cdp: {}, input: {}, sink: { async record() {}, async writeJson() { return {}; } },
    sessionId: SESSION, providerProfile: PROFILE, providerBaseUrl: BASE_URL, model: MODEL,
    promptInput: { text: QUESTION }, timeoutMs: 60_000,
  }, {
    now: () => 1_000,
    sleep: async () => { sleeps += 1; },
    observe: async () => phase === "ready" ? surface()
      : phase === "draft" ? surface({ draft: QUESTION })
        : surface({
          status: "completed",
          generation: "1",
          draftRevision: "1",
          messages: [
            { id: "side-user", role: "user", content: QUESTION },
            { id: "side-assistant", role: "assistant", content: "   " },
          ],
        }),
    waitForStage: async ({ sample, accept }) => {
      const value = await sample();
      assert.equal(await accept(value), true);
      return { value };
    },
    click: async (_input, locator) => {
      if (locator.identity.action === "send-side-chat") {
        phase = "completed";
        calls.push({
          sequence: 1,
          command: "submit_side_chat",
          args: {
            ownerSessionId: SESSION,
            chatId: CHAT,
            expectedGeneration: "0",
            expectedDraftRevision: "1",
            expectedOwnerAppendPosition: "194",
            quote: null,
            text: QUESTION,
          },
        });
      }
      return {};
    },
    insert: async () => { phase = "draft"; },
    createCommandProbe: () => ({
      async install() {},
      async snapshot(afterSequence = 0) {
        return {
          found: true,
          sequence: calls.length,
          dropped_through: 0,
          calls: calls.filter((call) => call.sequence > afterSequence),
        };
      },
      async remove() {},
    }),
    screenshot: async () => ({}),
  }), (error) => error?.code === "case5_2-stage5-terminal-mismatch");
  assert.equal(sleeps, 0);
});

import assert from "node:assert/strict";
import test from "node:test";

import {
  beginSideChatFocusContinuation,
  invalidateSideChatFocusInteraction,
  pointerTargetsSideChatRunControl,
  reconcileSideChatFocusContinuation,
  sideChatFocusSurface,
  sideChatFocusTargetStillMatches,
  type SideChatFocusEnvironment,
  type SideChatFocusMutation,
} from "../src/side_chat_focus_continuation.ts";
import type { DesktopViewState, SideChatProjection } from "../src/types.ts";

function sideState(
  overrides: Partial<SideChatProjection> = {},
  ownerSessionId: string | null = "session-a",
): DesktopViewState {
  return {
    confirmation_visible: false,
    navigation_loading: false,
    overlay: "none",
    draft_target: {
      workspacePath: "C:/workspace",
      sessionId: ownerSessionId,
      ownerGeneration: "1",
    },
    side_chat: {
      configured: true,
      deleting: false,
      chat_id: "side-a",
      owner_session_id: ownerSessionId,
      model: "gemma-test",
      base_url: "http://127.0.0.1:1234/v1",
      status: "idle",
      phase: "idle",
      last_error: "",
      generation: "4",
      draft_text: "question",
      draft_revision: "2",
      messages: [],
      can_send: true,
      can_cancel: false,
      ...overrides,
    },
  } as DesktopViewState;
}

function mutation(
  kind: "send" | "cancel",
  generation = "4",
): SideChatFocusMutation {
  return { kind, chatId: "side-a", generation };
}

function environment(
  activeMutation: SideChatFocusMutation | null,
  overrides: Partial<SideChatFocusEnvironment> = {},
): SideChatFocusEnvironment {
  return {
    paneVisible: true,
    localModalOpen: false,
    mutation: activeMutation,
    ...overrides,
  };
}

class FakeElement {
  disabled = false;
  readonly dataset: Record<string, string> = {};
  private readonly owner: FakeDocument;
  readonly kind: "body" | "root" | "prompt" | "action" | "modal" | "other";

  constructor(
    owner: FakeDocument,
    kind: "body" | "root" | "prompt" | "action" | "modal" | "other",
    action = "",
  ) {
    this.owner = owner;
    this.kind = kind;
    if (action) this.dataset.action = action;
  }

  matches(selector: string): boolean {
    if (selector === "#side-chat-prompt") return this.kind === "prompt";
    if (selector === ":disabled") return this.disabled;
    return false;
  }

  closest(selector: string): FakeElement | null {
    if (this.kind === "modal" && selector.includes(".modal")) return this;
    if (selector === "[data-action]" && this.dataset.action) return this;
    return null;
  }

  focus(): void {
    this.owner.activeElement = this;
  }
}

class FakeDocument {
  readonly body = new FakeElement(this, "body");
  readonly documentElement = new FakeElement(this, "root");
  readonly prompt = new FakeElement(this, "prompt");
  readonly send = new FakeElement(this, "action", "send-side-chat");
  readonly stop = new FakeElement(this, "action", "cancel-side-chat");
  readonly mainSend = new FakeElement(this, "action", "send");
  readonly modal = new FakeElement(this, "modal");
  readonly other = new FakeElement(this, "other");
  activeElement: FakeElement | null = this.body;

  querySelector(selector: string): FakeElement | null {
    return selector === "#side-chat-prompt" ? this.prompt : null;
  }
}

function asDocument(value: FakeDocument): Document {
  return value as unknown as Document;
}

function asElement(value: FakeElement): Element {
  return value as unknown as Element;
}

test("pointer Side Send follows the real local-mutation and projection sequence before returning focus", () => {
  const dom = new FakeDocument();
  const idle = sideState();
  dom.activeElement = dom.prompt;
  const activation = beginSideChatFocusContinuation(idle, "send-side-chat");
  assert.ok(activation);

  // WebView pointer activation first focuses Send. The local mutation rerender
  // still projects generation N while the command is waiting for admission.
  dom.activeElement = dom.send;
  let decision = reconcileSideChatFocusContinuation(
    activation,
    idle,
    idle,
    sideChatFocusSurface(asDocument(dom)),
    environment(mutation("send")),
  );
  assert.equal(decision.continuation?.startingGeneration, "4");
  assert.equal(decision.continuation?.requestGeneration, null);
  assert.equal(decision.focusTarget, null);

  // The accepted Rust request owns exactly generation N+1. The matching local
  // mutation remains until the command response has been applied.
  dom.activeElement = dom.body;
  const running = sideState({
    generation: "5",
    status: "running",
    phase: "request_in_flight",
    can_send: false,
    can_cancel: true,
  });
  decision = reconcileSideChatFocusContinuation(
    decision.continuation,
    idle,
    running,
    sideChatFocusSurface(asDocument(dom)),
    environment(mutation("send")),
  );
  assert.equal(decision.continuation?.requestGeneration, "5");
  assert.equal(decision.focusTarget, null);

  decision = reconcileSideChatFocusContinuation(
    decision.continuation,
    running,
    running,
    sideChatFocusSurface(asDocument(dom)),
    environment(null),
  );
  assert.ok(decision.continuation);

  const completed = sideState({ generation: "5", status: "completed", phase: "", can_send: true });
  decision = reconcileSideChatFocusContinuation(
    decision.continuation,
    running,
    completed,
    sideChatFocusSurface(asDocument(dom)),
    environment(null),
  );
  assert.deepEqual(decision.focusTarget, {
    ownerSessionId: "session-a",
    chatId: "side-a",
    generation: "5",
  });
  assert.equal(sideChatFocusTargetStillMatches(decision.focusTarget!, completed), true);
});

test("a fast terminal projection waits for the matching Send mutation to clear", () => {
  const dom = new FakeDocument();
  const idle = sideState();
  const completed = sideState({ generation: "5", status: "completed", phase: "", can_send: true });
  dom.activeElement = dom.body;
  let decision = reconcileSideChatFocusContinuation(
    beginSideChatFocusContinuation(idle, "send-side-chat"),
    idle,
    completed,
    sideChatFocusSurface(asDocument(dom)),
    environment(mutation("send")),
  );
  assert.equal(decision.continuation?.requestGeneration, "5");
  assert.equal(decision.focusTarget, null);

  decision = reconcileSideChatFocusContinuation(
    decision.continuation,
    completed,
    completed,
    sideChatFocusSurface(asDocument(dom)),
    environment(null),
  );
  assert.equal(decision.continuation, null);
  assert.equal(decision.focusTarget?.generation, "5");
});

test("a non-admitted Side Send returns focus for retry only after its local mutation clears", () => {
  const dom = new FakeDocument();
  const idle = sideState();
  dom.activeElement = dom.body;
  let decision = reconcileSideChatFocusContinuation(
    beginSideChatFocusContinuation(idle, "send-side-chat"),
    idle,
    idle,
    sideChatFocusSurface(asDocument(dom)),
    environment(mutation("send")),
  );
  assert.ok(decision.continuation);
  assert.equal(decision.focusTarget, null);

  decision = reconcileSideChatFocusContinuation(
    decision.continuation,
    idle,
    idle,
    sideChatFocusSurface(asDocument(dom)),
    environment(null),
  );
  assert.equal(decision.continuation, null);
  assert.equal(decision.focusTarget?.generation, "4");
});

test("pointer Side Stop keeps generation N and returns focus at exact cancellation terminal", () => {
  const dom = new FakeDocument();
  const running = sideState({ status: "running", can_send: false, can_cancel: true });
  dom.activeElement = dom.stop;
  let decision = reconcileSideChatFocusContinuation(
    beginSideChatFocusContinuation(running, "cancel-side-chat"),
    running,
    running,
    sideChatFocusSurface(asDocument(dom)),
    environment(mutation("cancel")),
  );
  assert.equal(decision.continuation?.requestGeneration, "4");

  dom.activeElement = dom.body;
  decision = reconcileSideChatFocusContinuation(
    decision.continuation,
    running,
    running,
    sideChatFocusSurface(asDocument(dom)),
    environment(null),
  );
  assert.ok(decision.continuation);

  const cancelled = sideState({ status: "cancelled", phase: "", can_send: true, can_cancel: false });
  decision = reconcileSideChatFocusContinuation(
    decision.continuation,
    running,
    cancelled,
    sideChatFocusSurface(asDocument(dom)),
    environment(null),
  );
  assert.equal(decision.focusTarget?.generation, "4");
  assert.equal(sideChatFocusTargetStillMatches(decision.focusTarget!, cancelled), true);
});

test("Side continuation rejects owner, chat, generation, pane, modal, and mutation drift", () => {
  const dom = new FakeDocument();
  const idle = sideState();
  const activation = beginSideChatFocusContinuation(idle, "send-side-chat");
  assert.ok(activation);
  dom.activeElement = dom.body;

  const cases: Array<[DesktopViewState, SideChatFocusEnvironment]> = [
    [sideState({}, "session-b"), environment(mutation("send"))],
    [sideState({ chat_id: "side-b" }), environment(mutation("send"))],
    [sideState({ generation: "6", status: "running", can_send: false, can_cancel: true }), environment(mutation("send"))],
    [idle, environment(mutation("send"), { paneVisible: false })],
    [idle, environment(mutation("send"), { localModalOpen: true })],
    [idle, environment({ kind: "cancel", chatId: "side-a", generation: "4" })],
  ];
  for (const [next, nextEnvironment] of cases) {
    const decision = reconcileSideChatFocusContinuation(
      activation,
      idle,
      next,
      sideChatFocusSurface(asDocument(dom)),
      nextEnvironment,
    );
    assert.equal(decision.continuation, null);
    assert.equal(decision.focusTarget, null);
  }
});

test("new user focus and non-Side pointer ownership prevent focus stealing; Ctrl+Enter remains prompt-owned", () => {
  const dom = new FakeDocument();
  const idle = sideState();
  const running = sideState({ generation: "5", status: "running", can_send: false, can_cancel: true });
  const activation = beginSideChatFocusContinuation(idle, "send-side-chat");
  assert.ok(activation);

  dom.activeElement = dom.other;
  const moved = reconcileSideChatFocusContinuation(
    activation,
    idle,
    running,
    sideChatFocusSurface(asDocument(dom)),
    environment(null),
  );
  assert.equal(moved.continuation, null);
  assert.equal(dom.activeElement, dom.other);

  assert.equal(pointerTargetsSideChatRunControl(asElement(dom.send)), true);
  assert.equal(pointerTargetsSideChatRunControl(asElement(dom.stop)), true);
  assert.equal(pointerTargetsSideChatRunControl(asElement(dom.mainSend)), false);

  // Ctrl+Enter dispatches directly from the textarea instead of the delegated
  // click path, so the existing composer remains the focus owner.
  dom.activeElement = dom.prompt;
  const keyboard = reconcileSideChatFocusContinuation(
    null,
    idle,
    running,
    sideChatFocusSurface(asDocument(dom)),
    environment(null),
  );
  assert.equal(keyboard.continuation, null);
  assert.equal(keyboard.focusTarget, null);
  assert.equal(dom.activeElement, dom.prompt);
});

test("focused Side controls never arm a continuation without their delegated activation", () => {
  const dom = new FakeDocument();
  const idle = sideState();
  dom.activeElement = dom.send;
  const unrelatedIdleRender = reconcileSideChatFocusContinuation(
    null,
    idle,
    idle,
    sideChatFocusSurface(asDocument(dom)),
    environment(null),
  );
  assert.equal(unrelatedIdleRender.continuation, null);
  assert.equal(unrelatedIdleRender.focusTarget, null);

  const running = sideState({ status: "running", can_send: false, can_cancel: true });
  const completed = sideState({ status: "completed", phase: "", can_send: true, can_cancel: false });
  dom.activeElement = dom.stop;
  const naturalCompletion = reconcileSideChatFocusContinuation(
    null,
    running,
    completed,
    sideChatFocusSurface(asDocument(dom)),
    environment(null),
  );
  assert.equal(naturalCompletion.continuation, null);
  assert.equal(naturalCompletion.focusTarget, null);
  assert.equal(dom.activeElement, dom.stop);
});

test("a new interaction invalidates both the pending owner and a scheduled focus generation", () => {
  const idle = sideState();
  const interaction = {
    sideChatFocusContinuation: beginSideChatFocusContinuation(idle, "send-side-chat"),
    sideChatFocusInteractionGeneration: 9n,
  };
  assert.ok(interaction.sideChatFocusContinuation);

  invalidateSideChatFocusInteraction(interaction);

  assert.equal(interaction.sideChatFocusContinuation, null);
  assert.equal(interaction.sideChatFocusInteractionGeneration, 10n);
});

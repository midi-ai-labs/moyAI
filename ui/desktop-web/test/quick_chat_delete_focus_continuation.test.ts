import assert from "node:assert/strict";
import test from "node:test";

import {
  abandonQuickChatDeleteFocusContinuation,
  beginQuickChatDeleteFocusContinuation,
  quickChatDeleteFocusCandidates,
  reconcileQuickChatDeleteFocusContinuation,
} from "../src/quick_chat_delete_focus_continuation.ts";
import type { DesktopViewState, RowMutationTarget, SessionRow } from "../src/types.ts";

function row(sessionId: string): SessionRow {
  return {
    session_id: sessionId,
    label: sessionId,
    status: "completed",
    loaded_status: "not_loaded",
    archived: false,
    pending_permission_requests: 0,
    pending_user_input_requests: 0,
  } as SessionRow;
}

function state(
  ids: string[],
  selectedSessionId: string | null,
  overrides: Partial<DesktopViewState> = {},
): DesktopViewState {
  const rows = ids.map(row);
  return {
    workspace_path: "C:/workspace",
    project_rows: [],
    selected_project_index: -1,
    session_rows: rows,
    selected_session_index: selectedSessionId === null
      ? -1
      : rows.findIndex((candidate) => candidate.session_id === selectedSessionId),
    chat_session_rows: rows,
    confirmation_visible: false,
    navigation_loading: false,
    overlay: "none",
    ...overrides,
  } as DesktopViewState;
}

function target(rowId: string, ownerSessionId: string | null): RowMutationTarget {
  return {
    workspacePath: "C:/workspace",
    ownerProjectId: null,
    ownerSessionId,
    rowId,
  };
}

class FakeElement {
  readonly dataset: Record<string, string> = {};
  hidden = false;
  disabled = false;
  scrolled = false;
  private readonly owner: FakeDocument;
  private ariaDisabled = "false";
  inert = false;

  constructor(owner: FakeDocument, focusKey = "") {
    this.owner = owner;
    if (focusKey) this.dataset.focusKey = focusKey;
  }

  matches(selector: string): boolean {
    return selector === ":disabled" && this.disabled;
  }

  getAttribute(name: string): string | null {
    return name === "aria-disabled" ? this.ariaDisabled : null;
  }

  closest(selector: string): FakeElement | null {
    return selector.includes("[inert]") && this.inert ? this : null;
  }

  focus(): void {
    this.owner.activeElement = this;
  }

  scrollIntoView(): void {
    this.scrolled = true;
  }
}

class FakeDocument {
  readonly body = new FakeElement(this);
  readonly documentElement = new FakeElement(this);
  readonly prompt = new FakeElement(this);
  readonly other = new FakeElement(this);
  readonly rows = new Map<string, FakeElement>();
  activeElement: FakeElement | null = this.body;

  addRow(sessionId: string): FakeElement {
    const element = new FakeElement(this, `chat-session:${sessionId}:select`);
    this.rows.set(sessionId, element);
    return element;
  }

  querySelector(selector: string): FakeElement | null {
    return selector === "#prompt" ? this.prompt : null;
  }

  querySelectorAll(selector: string): FakeElement[] {
    return selector === "[data-focus-key]" ? [...this.rows.values()] : [];
  }
}

function asDocument(value: FakeDocument): Document {
  return value as unknown as Document;
}

test("selected quick-chat deletion settles on the stable next row after the exact target disappears", () => {
  const before = state(["chat-a", "chat-b", "chat-c"], "chat-b");
  const continuation = beginQuickChatDeleteFocusContinuation(
    before,
    1,
    target("chat-b", "chat-b"),
  );
  assert.ok(continuation);

  const pending = reconcileQuickChatDeleteFocusContinuation(continuation, before, true);
  assert.equal(pending.continuation, continuation);
  assert.equal(pending.focusTarget, null);

  const settled = state(["chat-a", "chat-c"], "chat-c");
  const decision = reconcileQuickChatDeleteFocusContinuation(continuation, settled, false);
  assert.deepEqual(decision.focusTarget, { kind: "chat-row", sessionId: "chat-c" });

});

test("last and final quick-chat deletion use previous-row then prompt fallbacks", () => {
  const last = state(["chat-a", "chat-b"], "chat-b");
  const lastContinuation = beginQuickChatDeleteFocusContinuation(
    last,
    1,
    target("chat-b", "chat-b"),
  );
  assert.ok(lastContinuation);
  assert.deepEqual(
    reconcileQuickChatDeleteFocusContinuation(
      lastContinuation,
      state(["chat-a"], "chat-a"),
      false,
    ).focusTarget,
    { kind: "chat-row", sessionId: "chat-a" },
  );

  const only = state(["chat-a"], "chat-a");
  const onlyContinuation = beginQuickChatDeleteFocusContinuation(
    only,
    0,
    target("chat-a", "chat-a"),
  );
  assert.ok(onlyContinuation);
  const finalDecision = reconcileQuickChatDeleteFocusContinuation(
    onlyContinuation,
    state([], null),
    false,
  );
  assert.deepEqual(finalDecision.focusTarget, { kind: "prompt" });
});

test("quick-chat candidate resolution and row scrolling are separate from focus", () => {
  const dom = new FakeDocument();
  const rowTarget = dom.addRow("chat-b");
  const rowCandidate = quickChatDeleteFocusCandidates(asDocument(dom), {
    kind: "chat-row",
    sessionId: "chat-b",
  })[0];
  assert.ok(rowCandidate);
  assert.equal(rowCandidate.resolve(), rowTarget);
  assert.equal(dom.activeElement, dom.body);
  assert.equal(rowTarget.scrolled, false);

  rowCandidate.settle?.(rowTarget as unknown as HTMLElement);
  assert.equal(rowTarget.scrolled, true);
  assert.equal(dom.activeElement, dom.body, "settlement never focuses");

  const promptCandidate = quickChatDeleteFocusCandidates(asDocument(dom), { kind: "prompt" })[0];
  assert.equal(promptCandidate?.resolve(), dom.prompt);
  assert.equal(promptCandidate?.settle, undefined);
});

test("settlement discards a wrong async owner and an abandoned continuation", () => {
  const before = state(["chat-a", "chat-b", "chat-c"], "chat-b");
  const continuation = beginQuickChatDeleteFocusContinuation(
    before,
    1,
    target("chat-b", "chat-b"),
  );
  assert.ok(continuation);

  const wrongOwner = reconcileQuickChatDeleteFocusContinuation(
    continuation,
    state(["chat-a", "chat-c"], "chat-a"),
    false,
  );
  assert.equal(wrongOwner.continuation, null);
  assert.equal(wrongOwner.focusTarget, null);
  assert.equal(wrongOwner.ownsModalCloseFallback, true);

  const settled = reconcileQuickChatDeleteFocusContinuation(
    continuation,
    state(["chat-a", "chat-c"], "chat-c"),
    false,
  );
  assert.ok(settled.focusTarget);

  const abandoned = abandonQuickChatDeleteFocusContinuation(continuation);
  const abandonedSettlement = reconcileQuickChatDeleteFocusContinuation(
    abandoned,
    state(["chat-a", "chat-c"], "chat-c"),
    false,
  );
  assert.equal(abandonedSettlement.continuation, null);
  assert.equal(abandonedSettlement.focusTarget, null);
  assert.equal(
    abandonedSettlement.ownsModalCloseFallback,
    true,
    "the quick-chat owner suppresses the generic prompt fallback after user interaction",
  );
});

test("target disappearance waits for navigation settlement and never crosses a modal or workspace", () => {
  const before = state(["chat-a", "chat-b"], "chat-a");
  const continuation = beginQuickChatDeleteFocusContinuation(
    before,
    1,
    target("chat-b", "chat-a"),
  );
  assert.ok(continuation);

  const loading = reconcileQuickChatDeleteFocusContinuation(
    continuation,
    state(["chat-a"], "chat-a", { navigation_loading: true }),
    false,
  );
  assert.equal(loading.continuation, continuation);
  assert.equal(loading.focusTarget, null);

  const modal = reconcileQuickChatDeleteFocusContinuation(
    continuation,
    state(["chat-a"], "chat-a", { overlay: "config" }),
    false,
  );
  assert.equal(modal.continuation, null);
  assert.equal(modal.focusTarget, null);

  const otherWorkspace = reconcileQuickChatDeleteFocusContinuation(
    continuation,
    state(["chat-a"], "chat-a", { workspace_path: "C:/other" }),
    false,
  );
  assert.equal(otherWorkspace.continuation, null);
  assert.equal(otherWorkspace.focusTarget, null);
});

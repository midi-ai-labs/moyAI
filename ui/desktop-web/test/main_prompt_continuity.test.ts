import assert from "node:assert/strict";
import test from "node:test";

import {
  invalidateRefreshPromptFocus,
  recordRefreshPointerInteraction,
  refreshPromptFocusContinuationAccepted,
  retainConnectedMainPrompt,
  takePendingRefreshPromptFocus,
  wireMainPromptInputOnce,
  type RefreshPromptFocusState,
} from "../src/main_prompt_continuity.ts";

class FakePrompt extends EventTarget {
  isConnected = true;
  id = "prompt";
  className = "";
  placeholder = "";
  disabled = false;
  readOnly = false;
  required = false;
  tabIndex = 0;
  defaultValue = "";
  value = "";
  replacement: unknown = null;
  private readonly attributes = new Map<string, string>();

  getAttribute(name: string): string | null {
    return this.attributes.get(name) ?? null;
  }

  setAttribute(name: string, value: string): void {
    this.attributes.set(name, value);
  }

  removeAttribute(name: string): void {
    this.attributes.delete(name);
  }

  replaceWith(node: unknown): void {
    this.replacement = node;
  }
}

function asPrompt(prompt: FakePrompt): HTMLTextAreaElement {
  return prompt as unknown as HTMLTextAreaElement;
}

function createFocusState(): RefreshPromptFocusState {
  return {
    refreshPromptFocusInteractionGeneration: 0n,
    pendingRefreshPromptFocus: null,
  };
}

test("Refresh pointer captures the pre-default Main prompt owner and exact interaction generation", () => {
  const state = createFocusState();
  const captured = recordRefreshPointerInteraction(state, {
    owner: "workspace\u00001\u0000session-a",
    targetsRefresh: true,
    promptFocused: true,
  });

  assert.deepEqual(captured, {
    owner: "workspace\u00001\u0000session-a",
    interactionGeneration: 1n,
  });
  assert.equal(takePendingRefreshPromptFocus(state, "select_session"), null);
  assert.equal(state.pendingRefreshPromptFocus, captured);

  const request = takePendingRefreshPromptFocus(state, "refresh_desktop");
  assert.equal(request, captured);
  assert.equal(state.pendingRefreshPromptFocus, null);
  assert.equal(refreshPromptFocusContinuationAccepted(
    request,
    state.refreshPromptFocusInteractionGeneration,
    "refresh_desktop",
    "workspace\u00001\u0000session-a",
    true,
  ), true);
});

test("Refresh focus continuation rejects a newer interaction, owner change, stale settlement, or claimed focus", () => {
  const state = createFocusState();
  const request = recordRefreshPointerInteraction(state, {
    owner: "workspace\u00001\u0000session-a",
    targetsRefresh: true,
    promptFocused: true,
  });
  assert.notEqual(request, null);

  assert.equal(refreshPromptFocusContinuationAccepted(
    request,
    state.refreshPromptFocusInteractionGeneration,
    "refresh_desktop",
    "workspace\u00002\u0000session-a",
    true,
  ), false, "a changed draft generation rejects the old request");
  assert.equal(refreshPromptFocusContinuationAccepted(
    request,
    state.refreshPromptFocusInteractionGeneration,
    "select_session",
    "workspace\u00001\u0000session-a",
    true,
  ), false, "only the exact Refresh settlement can return focus");
  assert.equal(refreshPromptFocusContinuationAccepted(
    request,
    state.refreshPromptFocusInteractionGeneration,
    "refresh_desktop",
    "workspace\u00001\u0000session-a",
    false,
  ), false, "newly claimed focus is not stolen");

  invalidateRefreshPromptFocus(state);
  assert.equal(refreshPromptFocusContinuationAccepted(
    request,
    state.refreshPromptFocusInteractionGeneration,
    "refresh_desktop",
    "workspace\u00001\u0000session-a",
    true,
  ), false, "a later pointer/key/IME/wheel interaction fences the old request");
});

test("same-owner render transplants the connected prompt node and adopts rendered capabilities", () => {
  const current = new FakePrompt();
  current.value = "local unsent draft";
  current.defaultValue = "older projection";
  current.placeholder = "old";
  current.setAttribute("aria-describedby", "old-help");
  current.setAttribute("aria-label", "runtime label");
  current.setAttribute("data-node-token", "stable-node");

  const next = new FakePrompt();
  next.value = "local unsent draft";
  next.defaultValue = "local unsent draft";
  next.placeholder = "moyAI に依頼する";
  next.disabled = true;
  next.setAttribute("aria-describedby", "goal-command-hint");

  assert.equal(retainConnectedMainPrompt(
    asPrompt(current),
    asPrompt(next),
    "workspace\u0000project\u0000session-a",
    "workspace\u0000project\u0000session-a",
  ), true);
  assert.equal(next.replacement, current, "the old connected editor is moved into the new frame");
  assert.equal(current.value, "local unsent draft");
  assert.equal(current.disabled, true);
  assert.equal(current.placeholder, "moyAI に依頼する");
  assert.equal(current.getAttribute("aria-describedby"), "goal-command-hint");
  assert.equal(current.getAttribute("aria-label"), null);
  assert.equal(current.getAttribute("data-node-token"), "stable-node", "runtime node identity metadata survives");

  const differentOwner = new FakePrompt();
  assert.equal(retainConnectedMainPrompt(
    asPrompt(current),
    asPrompt(differentOwner),
    "workspace\u0000project\u0000session-a",
    "workspace\u0000project\u0000session-b",
  ), false);
  assert.equal(differentOwner.replacement, null);
});

test("a transplanted Main prompt keeps exactly one direct input listener", () => {
  const prompt = new FakePrompt();
  let firstCalls = 0;
  let duplicateCalls = 0;

  assert.equal(wireMainPromptInputOnce(asPrompt(prompt), () => { firstCalls += 1; }), true);
  assert.equal(wireMainPromptInputOnce(asPrompt(prompt), () => { duplicateCalls += 1; }), false);
  prompt.dispatchEvent(new Event("input"));

  assert.equal(firstCalls, 1);
  assert.equal(duplicateCalls, 0);
});

import assert from "node:assert/strict";
import test from "node:test";

import {
  beginNewChatFocusContinuation,
  beginNewProjectSessionFocusContinuation,
  captureSessionInteractionSnapshot,
  focusThenRestoreSessionPromptInteraction,
  initiatingTriggerYieldsToComposerFocus,
  newSessionRetryFocusTarget,
  reconcileNewSessionFocusContinuation,
  restoreSessionPromptInteraction,
  restoreSessionThreadInteraction,
  sameNewSessionFocusRequest,
  settledNewSessionFocusContinuationIsCurrent,
  sessionPromptInteractionForRender,
  sessionSelectionRequestsComposerFocus,
} from "../src/session_interaction_state.ts";

class FakeScrollTarget {
  scrollLeft = 0;
  scrollTop = 0;
  style = { scrollBehavior: "smooth" };

  scrollTo(options: ScrollToOptions): void {
    this.scrollLeft = options.left ?? this.scrollLeft;
    this.scrollTop = options.top ?? this.scrollTop;
  }
}

class LayoutClampedScrollTarget extends FakeScrollTarget {
  maxScrollTop: number;

  constructor(maxScrollTop: number) {
    super();
    this.maxScrollTop = maxScrollTop;
  }

  override scrollTo(options: ScrollToOptions): void {
    this.scrollLeft = options.left ?? this.scrollLeft;
    this.scrollTop = Math.min(options.top ?? this.scrollTop, this.maxScrollTop);
  }
}

class FakePromptTarget extends FakeScrollTarget {
  selectionStart: number | null = 0;
  selectionEnd: number | null = 0;
  value: string;
  focusCalls = 0;

  constructor(value: string) {
    super();
    this.value = value;
  }

  setSelectionRange(start: number, end: number): void {
    this.selectionStart = start;
    this.selectionEnd = end;
  }

  focus(): void {
    this.focusCalls += 1;
  }
}

class FocusResettingPromptTarget extends FakePromptTarget {
  override focus(): void {
    super.focus();
    // WebView focus can reveal its default caret before an explicit range is restored.
    this.selectionStart = 0;
    this.selectionEnd = 0;
    this.scrollLeft = 0;
    this.scrollTop = 0;
  }
}

test("session interaction snapshot restores thread scroll and composer caret independently", () => {
  const originalThread = new FakeScrollTarget();
  originalThread.scrollLeft = 7;
  originalThread.scrollTop = 381;
  const originalPrompt = new FakePromptTarget("session A draft");
  originalPrompt.scrollLeft = 3;
  originalPrompt.scrollTop = 42;
  originalPrompt.selectionStart = 8;
  originalPrompt.selectionEnd = 12;

  const snapshot = captureSessionInteractionSnapshot(originalThread, originalPrompt);
  const restoredThread = new FakeScrollTarget();
  const restoredPrompt = new FakePromptTarget("session A draft");
  restoreSessionThreadInteraction(snapshot, restoredThread);
  restoreSessionPromptInteraction(snapshot, restoredPrompt);

  assert.deepEqual(
    { left: restoredThread.scrollLeft, top: restoredThread.scrollTop },
    { left: 7, top: 381 },
  );
  assert.deepEqual(
    {
      left: restoredPrompt.scrollLeft,
      top: restoredPrompt.scrollTop,
      start: restoredPrompt.selectionStart,
      end: restoredPrompt.selectionEnd,
    },
    { left: 3, top: 42, start: 8, end: 12 },
  );
  assert.equal(restoredThread.style.scrollBehavior, "smooth");
  assert.equal(restoredPrompt.style.scrollBehavior, "smooth");
});

test("session thread restoration uses the final composer layout scroll range", () => {
  const snapshot = {
    threadScrollLeft: 0,
    threadScrollTop: 67,
    promptScrollLeft: 0,
    promptScrollTop: 0,
    promptSelectionStart: 0,
    promptSelectionEnd: 0,
  };
  const beforeComposerLayout = new LayoutClampedScrollTarget(17);
  restoreSessionThreadInteraction(snapshot, beforeComposerLayout);
  assert.equal(
    beforeComposerLayout.scrollTop,
    17,
    "the default composer geometry would irreversibly clamp this Quick Chat viewport",
  );

  const afterComposerLayout = new LayoutClampedScrollTarget(117);
  restoreSessionThreadInteraction(snapshot, afterComposerLayout);
  assert.equal(
    afterComposerLayout.scrollTop,
    67,
    "the exact durable-session viewport is available after composer autosizing settles",
  );
});

test("composer selection restoration is clamped to the remembered session draft", () => {
  const prompt = new FakePromptTarget("short");
  restoreSessionPromptInteraction({
    threadScrollLeft: 0,
    threadScrollTop: 0,
    promptScrollLeft: 0,
    promptScrollTop: 0,
    promptSelectionStart: 50,
    promptSelectionEnd: 80,
  }, prompt);

  assert.equal(prompt.selectionStart, 5);
  assert.equal(prompt.selectionEnd, 5);
});

test("pending session focus carries prompt interaction through the navigation poll and restores after focus", () => {
  const sessionOwner = "workspace\u0000project\u0000session-a";
  const projectSelectionFocusPending = sessionSelectionRequestsComposerFocus("select_project");
  assert.equal(projectSelectionFocusPending, true);
  const rememberedPrompt = new FakePromptTarget("line one\nline two\nline three");
  rememberedPrompt.selectionStart = 14;
  rememberedPrompt.selectionEnd = 18;
  rememberedPrompt.scrollLeft = 2;
  rememberedPrompt.scrollTop = 37;
  const remembered = captureSessionInteractionSnapshot(new FakeScrollTarget(), rememberedPrompt);
  const snapshots = new Map([[sessionOwner, remembered]]);

  const loadingProjection = new FakePromptTarget(rememberedPrompt.value);
  const initialTransition = sessionPromptInteractionForRender(snapshots, {
    owner: sessionOwner,
    ownerChanged: true,
    durableSession: true,
    focusPending: projectSelectionFocusPending,
  });
  assert.equal(initialTransition, remembered);
  restoreSessionPromptInteraction(initialTransition!, loadingProjection);

  // The next projection represents navigation settlement for the same selected session.
  // The focus request is still pending because the loading projection could not be focused.
  snapshots.set(
    sessionOwner,
    captureSessionInteractionSnapshot(new FakeScrollTarget(), loadingProjection),
  );
  const settledProjection = new FocusResettingPromptTarget(rememberedPrompt.value);
  const carried = sessionPromptInteractionForRender(snapshots, {
    owner: sessionOwner,
    ownerChanged: false,
    durableSession: true,
    focusPending: projectSelectionFocusPending,
  });
  assert.notEqual(carried, null);
  restoreSessionPromptInteraction(carried!, settledProjection);
  focusThenRestoreSessionPromptInteraction(carried!, settledProjection);

  assert.deepEqual(
    {
      focusCalls: settledProjection.focusCalls,
      left: settledProjection.scrollLeft,
      top: settledProjection.scrollTop,
      start: settledProjection.selectionStart,
      end: settledProjection.selectionEnd,
    },
    { focusCalls: 1, left: 2, top: 37, start: 14, end: 18 },
  );
  const ordinarySameOwner = sessionPromptInteractionForRender(snapshots, {
    owner: sessionOwner,
    ownerChanged: false,
    durableSession: true,
    focusPending: false,
  });
  assert.deepEqual(
    ordinarySameOwner,
    remembered,
    "ordinary same-owner DOM replacement preserves the current durable composer interaction",
  );
  const polledPrompt = new FakePromptTarget(rememberedPrompt.value);
  restoreSessionPromptInteraction(ordinarySameOwner!, polledPrompt);
  assert.deepEqual(
    {
      left: polledPrompt.scrollLeft,
      top: polledPrompt.scrollTop,
      start: polledPrompt.selectionStart,
      end: polledPrompt.selectionEnd,
    },
    { left: 2, top: 37, start: 14, end: 18 },
  );
  assert.equal(sessionPromptInteractionForRender(snapshots, {
    owner: sessionOwner,
    ownerChanged: true,
    durableSession: false,
    focusPending: true,
  }), null, "an unowned new-session composer must not inherit a durable session snapshot");
});

test("completed owner-selection mutations request composer focus", () => {
  for (const mutationName of ["select_project", "select_session", "select_chat_session"]) {
    assert.equal(sessionSelectionRequestsComposerFocus(mutationName), true, mutationName);
  }
  for (const mutationName of ["desktop_state", "select_artifact", null]) {
    assert.equal(sessionSelectionRequestsComposerFocus(mutationName), false, String(mutationName));
  }
});

test("only exact new-session initiating controls yield their generic focus snapshot", () => {
  assert.equal(
    initiatingTriggerYieldsToComposerFocus({
      mutationName: "new_project_session",
      activeAction: "new-project-session",
      activeFocusKey: "project:project-b:new-session",
      selectedProjectId: "project-b",
    }),
    true,
  );
  for (const activeFocusKey of [
    "quick-chat:new-session",
    "titlebar-menu:file:new-chat",
    "palette-action:new-chat",
    "shortcut-action:new-chat",
  ]) {
    assert.equal(initiatingTriggerYieldsToComposerFocus({
      mutationName: "new_chat",
      activeAction: "new-chat",
      activeFocusKey,
      selectedProjectId: null,
    }), true, activeFocusKey);
  }

  const mismatches = [
    { mutationName: "desktop_state", activeAction: "new-project-session", activeFocusKey: "project:project-b:new-session", selectedProjectId: "project-b" },
    { mutationName: "new_project_session", activeAction: "refresh", activeFocusKey: "project:project-b:new-session", selectedProjectId: "project-b" },
    { mutationName: "new_project_session", activeAction: "new-project-session", activeFocusKey: "project:project-a:new-session", selectedProjectId: "project-b" },
    { mutationName: "new_project_session", activeAction: "new-project-session", activeFocusKey: "project:project-b:new-session", selectedProjectId: null },
    { mutationName: "new_chat", activeAction: "new-chat", activeFocusKey: null, selectedProjectId: null },
    { mutationName: "new_chat", activeAction: "new-chat", activeFocusKey: "unrelated:new-chat", selectedProjectId: null },
    { mutationName: "new_chat", activeAction: "new-chat", activeFocusKey: "quick-chat:new-session", selectedProjectId: "project-b" },
    { mutationName: "new_chat", activeAction: "show-file-menu", activeFocusKey: "quick-chat:new-session", selectedProjectId: null },
  ];
  for (const context of mismatches) {
    assert.equal(
      initiatingTriggerYieldsToComposerFocus(context),
      false,
      "unrelated meaningful focus must retain the generic focus snapshot",
    );
  }
});

test("new-chat focus continuation admits every exact visible route and the explicit shortcut", () => {
  const keys = [
    "quick-chat:new-session",
    "titlebar-menu:file:new-chat",
    "palette-action:new-chat",
    "shortcut-action:new-chat",
  ];
  for (const activeFocusKey of keys) {
    const element = { dataset: { action: "new-chat", focusKey: activeFocusKey } };
    const continuation = beginNewChatFocusContinuation(
      "element",
      element,
      7n,
      "new-chat",
      activeFocusKey,
      "C:/project\u00001\u0000session-a",
    );
    const decision = reconcileNewSessionFocusContinuation(continuation, {
      mutationName: "new_chat",
      currentActiveElement: element,
      currentFocusUnclaimed: false,
      currentInteractionGeneration: 7n,
      selectedProjectId: null,
      selectedSessionIndex: -1,
      currentOwner: "C:/quick-chat\u00002\u0000new",
      currentSessionId: null,
      navigationLoading: false,
    });
    assert.equal(decision.continuation, null, activeFocusKey);
    assert.deepEqual(decision.settled, {
      owner: "C:/quick-chat\u00002\u0000new",
      interactionGeneration: 7n,
      targetProjectId: null,
      requestToken: continuation!.requestToken,
    }, activeFocusKey);
    assert.equal(decision.yieldsInitiatingFocus, true, activeFocusKey);
    assert.equal(decision.rejected, false, activeFocusKey);
  }

  const shortcutOwner = { dataset: { action: "refresh", focusKey: "topbar:refresh" } };
  const shortcut = beginNewChatFocusContinuation(
    "shortcut",
    shortcutOwner,
    9n,
    "refresh",
    "topbar:refresh",
    "C:/project\u00002\u0000session-a",
  );
  assert.notEqual(shortcut, null);
  assert.notEqual(reconcileNewSessionFocusContinuation(shortcut, {
    mutationName: "new_chat",
    currentActiveElement: shortcutOwner,
    currentFocusUnclaimed: false,
    currentInteractionGeneration: 9n,
    selectedProjectId: null,
    selectedSessionIndex: -1,
    currentOwner: "C:/quick-chat\u00003\u0000new",
    currentSessionId: null,
    navigationLoading: false,
  }).settled, null, "Ctrl/Meta+N is its own typed route instead of an action-only match");
});

test("project to Quick Chat carries exact focus intent through loading until canonical owner settlement", () => {
  const element = {
    dataset: { action: "new-chat", focusKey: "titlebar-menu:file:new-chat" },
  };
  const request = beginNewChatFocusContinuation(
    "element",
    element,
    12n,
    "new-chat",
    "titlebar-menu:file:new-chat",
    "C:/project\u00004\u0000session-a",
  );
  const loading = reconcileNewSessionFocusContinuation(request, {
    mutationName: "new_chat",
    currentActiveElement: element,
    currentFocusUnclaimed: false,
    currentInteractionGeneration: 12n,
    selectedProjectId: "project-a",
    selectedSessionIndex: 0,
    currentOwner: "C:/project\u00004\u0000session-a",
    currentSessionId: "session-a",
    navigationLoading: true,
  });
  assert.equal(loading.continuation?.phase, "admitted");
  assert.equal(loading.settled, null);
  assert.equal(loading.yieldsInitiatingFocus, true);
  assert.equal(loading.rejected, false);

  const stillLoading = reconcileNewSessionFocusContinuation(loading.continuation, {
    mutationName: null,
    currentActiveElement: null,
    currentFocusUnclaimed: true,
    currentInteractionGeneration: 12n,
    selectedProjectId: "project-a",
    selectedSessionIndex: 0,
    currentOwner: "C:/project\u00004\u0000session-a",
    currentSessionId: "session-a",
    navigationLoading: true,
  });
  assert.equal(stillLoading.continuation?.phase, "admitted");
  assert.equal(stillLoading.yieldsInitiatingFocus, false);
  assert.equal(reconcileNewSessionFocusContinuation(stillLoading.continuation, {
    mutationName: null,
    currentActiveElement: null,
    currentFocusUnclaimed: true,
    currentInteractionGeneration: 13n,
    selectedProjectId: "project-a",
    selectedSessionIndex: 0,
    currentOwner: "C:/project\u00004\u0000session-a",
    currentSessionId: "session-a",
    navigationLoading: true,
  }).rejected, true, "a later interaction abandons the carried focus transfer");
  assert.equal(reconcileNewSessionFocusContinuation(stillLoading.continuation, {
    mutationName: null,
    currentActiveElement: null,
    currentFocusUnclaimed: true,
    currentInteractionGeneration: 12n,
    selectedProjectId: "project-a",
    selectedSessionIndex: 0,
    currentOwner: "C:/project\u00004\u0000session-a",
    currentSessionId: "session-a",
    navigationLoading: false,
  }).rejected, true, "idle non-canonical settlement is terminal");

  const settled = reconcileNewSessionFocusContinuation(stillLoading.continuation, {
    mutationName: null,
    currentActiveElement: null,
    currentFocusUnclaimed: true,
    currentInteractionGeneration: 12n,
    selectedProjectId: null,
    selectedSessionIndex: -1,
    currentOwner: "C:/quick-chat\u00005\u0000new",
    currentSessionId: null,
    navigationLoading: false,
  });
  assert.deepEqual(settled.settled, {
    owner: "C:/quick-chat\u00005\u0000new",
    interactionGeneration: 12n,
    targetProjectId: null,
    requestToken: request!.requestToken,
  });
  assert.equal(settled.yieldsInitiatingFocus, true);
});

test("new-chat focus intent survives an overtaking poll and an initiator detached by rerender", () => {
  const element = {
    dataset: { action: "new-chat", focusKey: "quick-chat:new-session" },
  };
  const request = beginNewChatFocusContinuation(
    "element",
    element,
    16n,
    "new-chat",
    "quick-chat:new-session",
    "C:/project\u00006\u0000session-a",
  );
  const oldOwnerPoll = reconcileNewSessionFocusContinuation(request, {
    mutationName: null,
    currentActiveElement: null,
    currentFocusUnclaimed: true,
    currentInteractionGeneration: 16n,
    selectedProjectId: "project-a",
    selectedSessionIndex: 0,
    currentOwner: "C:/project\u00006\u0000session-a",
    currentSessionId: "session-a",
    navigationLoading: false,
  });
  assert.equal(oldOwnerPoll.continuation?.phase, "initiating");
  assert.equal(oldOwnerPoll.rejected, false, "BODY focus after DOM replacement is unclaimed");

  const commandSettlement = reconcileNewSessionFocusContinuation(oldOwnerPoll.continuation, {
    mutationName: "new_chat",
    currentActiveElement: null,
    currentFocusUnclaimed: true,
    currentInteractionGeneration: 16n,
    selectedProjectId: null,
    selectedSessionIndex: -1,
    currentOwner: "C:/quick-chat\u00007\u0000new",
    currentSessionId: null,
    navigationLoading: false,
  });
  assert.equal(commandSettlement.settled?.owner, "C:/quick-chat\u00007\u0000new");

  const overtakenRequest = beginNewChatFocusContinuation(
    "element",
    element,
    17n,
    "new-chat",
    "quick-chat:new-session",
    "C:/project\u00006\u0000session-a",
  );
  const higherRevisionPoll = reconcileNewSessionFocusContinuation(overtakenRequest, {
    mutationName: null,
    currentActiveElement: null,
    currentFocusUnclaimed: true,
    currentInteractionGeneration: 17n,
    selectedProjectId: null,
    selectedSessionIndex: -1,
    currentOwner: "C:/quick-chat\u00008\u0000new",
    currentSessionId: null,
    navigationLoading: false,
  });
  assert.deepEqual(higherRevisionPoll.settled, {
    owner: "C:/quick-chat\u00008\u0000new",
    interactionGeneration: 17n,
    targetProjectId: null,
    requestToken: overtakenRequest!.requestToken,
  }, "a committed poll can settle typed intent before the older command response is accepted");
});

test("an admitted continuation keeps one request token so exact command error cancels later focus", () => {
  const element = {
    dataset: { action: "new-chat", focusKey: "palette-action:new-chat" },
  };
  const request = beginNewChatFocusContinuation(
    "element",
    element,
    18n,
    "new-chat",
    "palette-action:new-chat",
    "C:/project\u00009\u0000session-a",
  );
  const admitted = reconcileNewSessionFocusContinuation(request, {
    mutationName: "new_chat",
    currentActiveElement: null,
    currentFocusUnclaimed: true,
    currentInteractionGeneration: 18n,
    selectedProjectId: "project-a",
    selectedSessionIndex: 0,
    currentOwner: "C:/project\u00009\u0000session-a",
    currentSessionId: "session-a",
    navigationLoading: true,
  });
  assert.notEqual(admitted.continuation, request, "phase progression may replace the value object");
  assert.equal(sameNewSessionFocusRequest(admitted.continuation, request), true);

  request!.requestToken.rejected = true;
  let pending = admitted.continuation;
  if (sameNewSessionFocusRequest(pending, request)) pending = null;
  assert.equal(pending, null, "exact error cleanup follows the stable token across phase copies");
  assert.equal(reconcileNewSessionFocusContinuation(admitted.continuation, {
    mutationName: null,
    currentActiveElement: null,
    currentFocusUnclaimed: true,
    currentInteractionGeneration: 18n,
    selectedProjectId: null,
    selectedSessionIndex: -1,
    currentOwner: "C:/quick-chat\u000010\u0000new",
    currentSessionId: null,
    navigationLoading: false,
  }).settled, null, "a later canonical projection cannot revive a rejected request");
});

test("exact new-session errors return BODY focus to retry routes without stealing modal primary focus", () => {
  const railElement = {
    dataset: { action: "new-chat", focusKey: "quick-chat:new-session" },
  };
  const projectElement = {
    dataset: {
      action: "new-project-session",
      focusKey: "project:project-b:new-session",
    },
  };
  const railRequest = beginNewChatFocusContinuation(
    "element",
    railElement,
    24n,
    "new-chat",
    "quick-chat:new-session",
    "C:/workspace\u000014\u0000session-a",
  );
  const projectRequest = beginNewProjectSessionFocusContinuation(
    projectElement,
    24n,
    "new-project-session",
    "project:project-b:new-session",
    "C:/workspace\u000014\u0000session-a",
    "project-b",
  );
  for (const [label, request] of [["rail", railRequest], ["project", projectRequest]] as const) {
    request!.requestToken.rejected = true;
    const retryTarget = { label: `${label}-rerendered` };
    assert.equal(newSessionRetryFocusTarget(request, {
      currentOwner: "C:/workspace\u000014\u0000session-a",
      currentInteractionGeneration: 24n,
      focusUnclaimed: true,
      initiatingElementConnected: false,
      exactRouteTarget: retryTarget,
      promptTarget: null,
    }), retryTarget, `${label} restores its exact rerendered retry control from BODY`);
    assert.equal(newSessionRetryFocusTarget(request, {
      currentOwner: "C:/workspace\u000014\u0000session-a",
      currentInteractionGeneration: 24n,
      focusUnclaimed: false,
      initiatingElementConnected: false,
      exactRouteTarget: retryTarget,
      promptTarget: null,
    }), null, `${label} never replaces a meaningful focus owner`);
  }

  for (const focusKey of [
    "titlebar-menu:file:new-chat",
    "palette-action:new-chat",
    "shortcut-action:new-chat",
  ]) {
    const element = { dataset: { action: "new-chat", focusKey } };
    const request = beginNewChatFocusContinuation(
      "element",
      element,
      25n,
      "new-chat",
      focusKey,
      "C:/workspace\u000015\u0000session-a",
    );
    request!.requestToken.rejected = true;
    assert.equal(newSessionRetryFocusTarget(request, {
      currentOwner: "C:/workspace\u000015\u0000session-a",
      currentInteractionGeneration: 25n,
      focusUnclaimed: false,
      initiatingElementConnected: false,
      exactRouteTarget: { focusKey },
      promptTarget: null,
    }), null, `${focusKey} preserves the overlay's existing primary focus`);
  }

  const originalPrompt = { id: "prompt" };
  const shortcutRequest = beginNewChatFocusContinuation(
    "shortcut",
    originalPrompt,
    26n,
    null,
    null,
    "C:/workspace\u000016\u0000session-a",
  );
  shortcutRequest!.requestToken.rejected = true;
  const rerenderedPrompt = { id: "prompt" };
  assert.equal(newSessionRetryFocusTarget(shortcutRequest, {
    currentOwner: "C:/workspace\u000016\u0000session-a",
    currentInteractionGeneration: 26n,
    focusUnclaimed: true,
    initiatingElementConnected: false,
    exactRouteTarget: null,
    promptTarget: rerenderedPrompt,
  }), rerenderedPrompt, "Ctrl/Meta+N returns to the rerendered prompt it started from on error");
  assert.equal(newSessionRetryFocusTarget(shortcutRequest, {
    currentOwner: "C:/workspace\u000016\u0000other-session",
    currentInteractionGeneration: 26n,
    focusUnclaimed: true,
    initiatingElementConnected: false,
    exactRouteTarget: null,
    promptTarget: rerenderedPrompt,
  }), null, "owner drift rejects retry focus");
});

test("different-project new session carries its exact target through old-owner and loading projections", () => {
  const element = {
    dataset: {
      action: "new-project-session",
      focusKey: "project:project-b:new-session",
    },
  };
  const request = beginNewProjectSessionFocusContinuation(
    element,
    30n,
    "new-project-session",
    "project:project-b:new-session",
    "C:/workspace\u000011\u0000session-a",
    "project-b",
  );
  assert.notEqual(request, null);

  const oldOwnerPoll = reconcileNewSessionFocusContinuation(request, {
    mutationName: null,
    currentActiveElement: null,
    currentFocusUnclaimed: true,
    currentInteractionGeneration: 30n,
    selectedProjectId: "project-a",
    selectedSessionIndex: 0,
    currentOwner: "C:/workspace\u000011\u0000session-a",
    currentSessionId: "session-a",
    navigationLoading: false,
  });
  assert.equal(oldOwnerPoll.continuation?.phase, "initiating");

  const loading = reconcileNewSessionFocusContinuation(oldOwnerPoll.continuation, {
    mutationName: "new_project_session",
    currentActiveElement: null,
    currentFocusUnclaimed: true,
    currentInteractionGeneration: 30n,
    selectedProjectId: "project-a",
    selectedSessionIndex: 0,
    currentOwner: "C:/workspace\u000011\u0000session-a",
    currentSessionId: "session-a",
    navigationLoading: true,
  });
  assert.equal(loading.continuation?.phase, "admitted");
  assert.equal(loading.yieldsInitiatingFocus, true);

  const settled = reconcileNewSessionFocusContinuation(loading.continuation, {
    mutationName: null,
    currentActiveElement: null,
    currentFocusUnclaimed: true,
    currentInteractionGeneration: 30n,
    selectedProjectId: "project-b",
    selectedSessionIndex: -1,
    currentOwner: "C:/workspace\u000012\u0000new",
    currentSessionId: null,
    navigationLoading: false,
  });
  assert.deepEqual(settled.settled, {
    owner: "C:/workspace\u000012\u0000new",
    interactionGeneration: 30n,
    targetProjectId: "project-b",
    requestToken: request!.requestToken,
  });

  assert.equal(beginNewProjectSessionFocusContinuation(
    element,
    30n,
    "new-project-session",
    "project:project-a:new-session",
    "C:/workspace\u000011\u0000session-a",
    "project-b",
  ), null, "the expected row target and focus key must identify the same project");

  const unrelatedFocus = reconcileNewSessionFocusContinuation(request, {
    mutationName: null,
    currentActiveElement: { dataset: { action: "refresh", focusKey: "topbar:refresh" } },
    currentFocusUnclaimed: false,
    currentInteractionGeneration: 30n,
    selectedProjectId: "project-a",
    selectedSessionIndex: 0,
    currentOwner: "C:/workspace\u000011\u0000session-a",
    currentSessionId: "session-a",
    navigationLoading: false,
  });
  assert.equal(unrelatedFocus.rejected, true, "a different meaningful focus owner is never stolen");

  const higherRevisionPoll = reconcileNewSessionFocusContinuation(request, {
    mutationName: null,
    currentActiveElement: null,
    currentFocusUnclaimed: true,
    currentInteractionGeneration: 30n,
    selectedProjectId: "project-b",
    selectedSessionIndex: -1,
    currentOwner: "C:/workspace\u000013\u0000new",
    currentSessionId: null,
    navigationLoading: false,
  });
  assert.equal(
    higherRevisionPoll.settled?.targetProjectId,
    "project-b",
    "a canonical target-project poll can settle before an older command response",
  );
});

test("new-chat focus continuation rejects interaction, trigger, terminal owner, and rAF owner ABA drift", () => {
  const element = { dataset: { action: "new-chat", focusKey: "quick-chat:new-session" } };
  const request = beginNewChatFocusContinuation(
    "element",
    element,
    20n,
    "new-chat",
    "quick-chat:new-session",
    "C:/quick-chat\u00008\u0000new",
  );
  const base = {
    mutationName: "new_chat" as string | null,
    currentActiveElement: element as object | null,
    currentFocusUnclaimed: false,
    currentInteractionGeneration: 20n,
    selectedProjectId: null as string | null,
    selectedSessionIndex: -1,
    currentOwner: "C:/quick-chat\u00009\u0000new",
    currentSessionId: null as string | null,
    navigationLoading: false,
  };
  assert.equal(reconcileNewSessionFocusContinuation(request, {
    ...base,
    currentInteractionGeneration: 21n,
  }).rejected, true);
  assert.equal(reconcileNewSessionFocusContinuation(request, {
    ...base,
    currentActiveElement: { dataset: { action: "refresh", focusKey: "topbar:refresh" } },
  }).rejected, true);
  assert.equal(reconcileNewSessionFocusContinuation(request, {
    ...base,
    currentOwner: "C:/quick-chat\u00008\u0000new",
  }).rejected, true, "the canonical owner must advance");
  assert.equal(reconcileNewSessionFocusContinuation(request, {
    ...base,
    selectedSessionIndex: 0,
    currentSessionId: "session-b",
  }).rejected, true, "a durable session is not the canonical new-chat owner");

  const settled = {
    owner: base.currentOwner,
    interactionGeneration: 20n,
    targetProjectId: null,
    requestToken: { rejected: false },
  };
  assert.equal(settledNewSessionFocusContinuationIsCurrent(
    settled,
    20n,
    null,
    -1,
    base.currentOwner,
    null,
  ), true);
  assert.equal(settledNewSessionFocusContinuationIsCurrent(
    settled,
    20n,
    null,
    -1,
    "C:/quick-chat\u000010\u0000new",
    null,
  ), false, "ownerGeneration drift invalidates the queued animation-frame focus");
});

import assert from "node:assert/strict";
import test from "node:test";
import {
  acknowledgePendingHistoryPrepend,
  advancePendingHistoryPrepend,
  captureViewportAnchor,
  createPendingHistoryPrepend,
  focusHistoryPrependReturnIfUnowned,
  historyPrependFocusCandidates,
  historyPrependFocusContinuationIsCurrent,
  pinThreadToEnd,
  pinResolvedThreadToEnd,
  rejectPendingHistoryPrepend,
  restoreViewportAnchor,
  runCompletionEdge,
  shouldRevealThreadEnd,
  syncResolvedInactiveThreadViewport,
  ThreadTailFollowAffinity,
  transcriptAnchors,
  turnPageLoadPending,
  type HistoryPrependProjection,
} from "../src/history_navigation.ts";
import type { TranscriptRow } from "../src/types.ts";

test("an active run-tail owner reveals through a transient raw geometry gap", () => {
  assert.equal(shouldRevealThreadEnd({
    sessionChanged: false,
    runStartRequested: true,
    previouslyNearEnd: false,
    updateWantsEnd: false,
  }), true);
  assert.equal(shouldRevealThreadEnd({
    sessionChanged: false,
    runStartRequested: false,
    previouslyNearEnd: false,
    updateWantsEnd: true,
  }), false, "passive polling still preserves a deliberate older-history position");
  assert.equal(shouldRevealThreadEnd({
    sessionChanged: false,
    runStartRequested: false,
    previouslyNearEnd: true,
    updateWantsEnd: true,
  }), true, "AI output keeps following while the user remains near the tail");
  assert.equal(shouldRevealThreadEnd({
    sessionChanged: false,
    runStartRequested: false,
    previouslyNearEnd: false,
    updateWantsEnd: true,
  }), false, "AI output must not pull the user away from older history they are reading");

  const thread = { scrollTop: 0, scrollHeight: 2_400 };
  pinThreadToEnd(thread);
  assert.equal(thread.scrollTop, 2_400, "the run-start render pins synchronously before a poll can replace it");
});

test("run completion is an edge instead of a terminal status level", () => {
  const cases = [
    {
      name: "busy true -> false completes without a terminal-status change",
      previous: { busy: true, terminal: false },
      current: { busy: false, terminal: false },
      expected: true,
    },
    {
      name: "nonterminal -> terminal completes without a busy edge",
      previous: { busy: false, terminal: false },
      current: { busy: false, terminal: true },
      expected: true,
    },
    {
      name: "completed -> completed revision is not another completion",
      previous: { busy: false, terminal: true },
      current: { busy: false, terminal: true },
      expected: false,
    },
    {
      name: "completed -> failed terminal-kind change stays terminal -> terminal",
      previous: { busy: false, terminal: true },
      current: { busy: false, terminal: true },
      expected: false,
    },
    {
      name: "idle nonterminal revision has no completion edge",
      previous: { busy: false, terminal: false },
      current: { busy: false, terminal: false },
      expected: false,
    },
  ] as const;

  for (const scenario of cases) {
    assert.equal(
      runCompletionEdge(scenario.previous, scenario.current),
      scenario.expected,
      scenario.name,
    );
  }
});

test("a revision-only completed render preserves a near-tail viewport while completion edges still pin", () => {
  const revisionOnlyThread = clampedThread({
    scrollTop: 250,
    scrollHeight: 755,
    clientHeight: 423,
  });
  assert.equal(threadGap(revisionOnlyThread), 82, "the viewport is inside the passive 96px threshold");
  const revisionOnlyCompletion = runCompletionEdge(
    { busy: false, terminal: true },
    { busy: false, terminal: true },
  );
  assert.equal(revisionOnlyCompletion, false, "a completed projection level is not a new completion");
  if (shouldRevealThreadEnd({
    sessionChanged: false,
    runStartRequested: false,
    previouslyNearEnd: true,
    updateWantsEnd: revisionOnlyCompletion,
  })) {
    pinThreadToEnd(revisionOnlyThread);
  }
  assert.equal(revisionOnlyThread.scrollTop, 250, "revision-only Refresh keeps the exact thread position");

  const terminalThread = clampedThread({
    scrollTop: 250,
    scrollHeight: 755,
    clientHeight: 423,
  });
  const completion = runCompletionEdge(
    { busy: true, terminal: false },
    { busy: false, terminal: true },
  );
  assert.equal(completion, true);
  if (shouldRevealThreadEnd({
    sessionChanged: false,
    runStartRequested: false,
    previouslyNearEnd: true,
    updateWantsEnd: completion,
  })) {
    pinThreadToEnd(terminalThread);
  }
  assert.equal(threadGap(terminalThread), 0, "a real terminal edge still reveals the final output");
});

test("run-scoped tail affinity survives long composer layout, replacement, streaming, and terminal render", () => {
  const affinity = new ThreadTailFollowAffinity();
  const owner = tailOwner("idle:30");
  affinity.noteUserViewport(true);
  assert.equal(affinity.armRun(owner), true);

  let currentThread = clampedThread({ scrollTop: 46, scrollHeight: 869, clientHeight: 823 });
  let decision = affinity.reconcile(tailProjection("idle:30", false, true));
  assert.equal(decision.follow, true, "the pre-admission rerender follows without treating the prior terminal state as this run");
  assert.equal(decision.clearAfterPin, false);
  assert.equal(pinResolvedThreadToEnd(() => currentThread), true);

  currentThread.scrollHeight = 1_180;
  currentThread.clientHeight = 780;
  assert.ok(threadGap(currentThread) > 96, "long composer reserve can move raw geometry beyond the passive threshold");
  assert.equal(pinResolvedThreadToEnd(() => currentThread), true, "the post-layout callback resolves and pins the current thread");
  assert.equal(threadGap(currentThread), 0);
  affinity.completeRender(decision, true);

  currentThread = clampedThread({ scrollTop: currentThread.scrollTop, scrollHeight: 1_407, clientHeight: 780 });
  decision = affinity.reconcile(tailProjection("root:31", true, false));
  assert.equal(decision.follow, true, "the accepted long User row binds the root generation");
  pinResolvedThreadToEnd(() => currentThread);
  affinity.completeRender(decision, true);
  assert.equal(threadGap(currentThread), 0);

  for (const scrollHeight of [2_019, 2_183, 2_416, 2_724]) {
    currentThread = clampedThread({ scrollTop: currentThread.scrollTop, scrollHeight, clientHeight: 780 });
    decision = affinity.reconcile(tailProjection("root:31", true, false));
    assert.equal(decision.follow, true);
    pinResolvedThreadToEnd(() => currentThread);
    affinity.completeRender(decision, true);
    assert.equal(threadGap(currentThread), 0, `incoming output at height ${scrollHeight} remains at the tail`);
  }

  currentThread = clampedThread({ scrollTop: currentThread.scrollTop, scrollHeight: 2_120, clientHeight: 823 });
  decision = affinity.reconcile(tailProjection("idle:31", false, true));
  assert.deepEqual(decision, { follow: true, clearAfterPin: true });
  pinResolvedThreadToEnd(() => currentThread);
  affinity.completeRender(decision, true);
  assert.equal(threadGap(currentThread), 0, "the final assistant/summary render is pinned before affinity clears");
  assert.equal(affinity.followingRun, false);
});

test("explicit user scroll-away cancels run following while layout and DOM replacement do not", () => {
  const affinity = new ThreadTailFollowAffinity();
  affinity.noteUserViewport(true);
  assert.equal(affinity.armRun(tailOwner("idle:7")), true);
  assert.equal(affinity.reconcile(tailProjection("root:8", true, false)).follow, true);

  affinity.noteUserScrollAway();
  assert.equal(affinity.followingRun, false);
  assert.equal(affinity.reconcile(tailProjection("root:8", true, false)).follow, false);
  assert.equal(affinity.armRun(tailOwner("root:8")), false, "a viewport deliberately left behind cannot arm another follow");

  const prior = clampedThread({ scrollTop: 300, scrollHeight: 1_200, clientHeight: 600 });
  const replacement = clampedThread({ scrollTop: prior.scrollTop, scrollHeight: 1_800, clientHeight: 600 });
  assert.equal(threadGap(replacement), 900, "incoming growth preserves the explicit older-history position");

  assert.equal(affinity.syncInactiveViewport(true), true, "interaction completion re-observes a return to the tail");
  assert.equal(affinity.armRun(tailOwner("idle:8")), true, "returning to the tail permits a later run to follow");
});

test("inactive session geometry resets stale viewport affinity without clearing an active run on layout", () => {
  const affinity = new ThreadTailFollowAffinity();
  affinity.noteUserScrollAway();
  assert.equal(affinity.viewportIsNearEnd, false);
  assert.equal(affinity.syncInactiveViewport(true), true, "a newly selected session adopts its current tail geometry");
  assert.equal(affinity.viewportIsNearEnd, true);

  assert.equal(affinity.armRun(tailOwner("idle:12")), true);
  assert.equal(affinity.reconcile(tailProjection("root:13", true, false)).follow, true);
  const layoutShiftedThread = clampedThread({ scrollTop: 200, scrollHeight: 1_500, clientHeight: 600 });
  assert.equal(
    syncResolvedInactiveThreadViewport(
      affinity,
      () => layoutShiftedThread,
      (thread) => threadGap(thread) <= 96,
    ),
    false,
    "composer or transcript layout cannot overwrite semantic affinity while the run owner is active",
  );
  assert.equal(affinity.viewportIsNearEnd, true);
  assert.equal(affinity.followingRun, true);
});

test("rail smooth-scroll completion at the latest tail rearms the next run", () => {
  const affinity = new ThreadTailFollowAffinity();
  let currentThread = clampedThread({ scrollTop: 100, scrollHeight: 1_500, clientHeight: 600 });
  const resolveThread = () => currentThread;
  const isNearEnd = (thread: ClampedThread) => threadGap(thread) <= 96;

  affinity.noteUserScrollAway();
  assert.equal(affinity.armRun(tailOwner("idle:20")), false);
  assert.equal(syncResolvedInactiveThreadViewport(affinity, resolveThread, isNearEnd), true);
  assert.equal(affinity.viewportIsNearEnd, false, "an intermediate smooth-scroll frame remains away");

  currentThread = clampedThread({ scrollTop: 900, scrollHeight: 1_500, clientHeight: 600 });
  assert.equal(syncResolvedInactiveThreadViewport(affinity, resolveThread, isNearEnd), true);
  assert.equal(affinity.armRun(tailOwner("idle:20")), true, "the final native scroll frame restores tail affinity");
});

test("failed or fit-content history prepend resyncs current geometry for the next run", () => {
  for (const currentThread of [
    clampedThread({ scrollTop: 600, scrollHeight: 1_200, clientHeight: 600 }),
    clampedThread({ scrollTop: 0, scrollHeight: 480, clientHeight: 600 }),
  ]) {
    const affinity = new ThreadTailFollowAffinity();
    affinity.noteUserScrollAway();
    assert.equal(
      syncResolvedInactiveThreadViewport(affinity, () => currentThread, (thread) => threadGap(thread) <= 96),
      true,
    );
    assert.equal(affinity.armRun(tailOwner("idle:40")), true);
  }
});

test("resolved tail pin targets the current DOM replacement instead of a detached thread", () => {
  const detached = clampedThread({ scrollTop: 0, scrollHeight: 1_000, clientHeight: 500 });
  const replacement = clampedThread({ scrollTop: 120, scrollHeight: 1_600, clientHeight: 600 });
  let current: ClampedThread = detached;
  const resolve = () => current;
  current = replacement;

  assert.equal(pinResolvedThreadToEnd(resolve), true);
  assert.equal(detached.scrollTop, 0);
  assert.equal(threadGap(replacement), 0);
});

test("viewport anchor capture restores the first surviving visible candidate", () => {
  const before = [
    mockAnchor("above", 0, 40),
    mockAnchor("first", 70, 130),
    mockAnchor("second", 140, 200),
  ];
  const thread = mockThread(before, { scrollTop: 100, scrollHeight: 1_000 });
  const snapshot = captureViewportAnchor(thread.element);

  assert.deepEqual(snapshot?.candidates, [
    { id: "first", offsetTop: 20 },
    { id: "second", offsetTop: 90 },
  ]);
  assert.equal(snapshot?.scrollTop, 100);
  assert.equal(snapshot?.scrollHeight, 1_000);

  thread.nodes = [mockAnchor("replacement", 80, 130), mockAnchor("second", 260, 320)];
  assert.equal(restoreViewportAnchor(thread.element, snapshot!), true);
  assert.equal(thread.element.scrollTop, 220, "the changed first row falls through to the second anchor");
});

test("viewport anchor restore falls back to prepended scroll-height delta when every id changes", () => {
  const thread = mockThread(
    [mockAnchor("old-first", 70, 130), mockAnchor("old-second", 140, 200)],
    { scrollTop: 100, scrollHeight: 1_000 },
  );
  const snapshot = captureViewportAnchor(thread.element);
  assert.ok(snapshot);

  thread.nodes = [mockAnchor("reprojected", 70, 130)];
  thread.scrollHeight = 1_400;
  assert.equal(restoreViewportAnchor(thread.element, snapshot), true);
  assert.equal(thread.element.scrollTop, 500);
});

test("pending history prepend waits for its async owner projection and consumes once", () => {
  const start = historyState();
  const pending = createPendingHistoryPrepend(start, 7);
  assert.ok(pending);

  const beforeCommandResponse = advancePendingHistoryPrepend(pending, start);
  assert.equal(beforeCommandResponse.disposition, "wait");
  assert.strictEqual(beforeCommandResponse.pending, pending);

  const accepted = acknowledgePendingHistoryPrepend(pending);
  const immediate = advancePendingHistoryPrepend(accepted, historyState({
    pending_async_operations: ["turn_page_load"],
  }));
  assert.equal(immediate.disposition, "wait");

  const completed = advancePendingHistoryPrepend(immediate.pending, historyState({
    pending_async_operations: [],
    turn_page_offset: 0,
  }));
  assert.equal(completed.disposition, "consume");
  assert.equal(completed.pending, null);
  assert.equal(advancePendingHistoryPrepend(completed.pending, start).disposition, "none");
});

test("history prepend keeps focus inside the exact transaction while loading and returns to its trigger", () => {
  const dom = mockHistoryFocusDocument({ active: "trigger" });
  const start = historyState();
  const pending = createPendingHistoryPrepend(start, 7, dom.document);
  assert.ok(pending);
  assert.equal(pending.returnFocusRequested, true);
  const accepted = acknowledgePendingHistoryPrepend(pending);

  dom.setTriggerDisabled(true);
  const loadingState = historyState({ pending_async_operations: ["turn_page_load"] });
  const loading = advancePendingHistoryPrepend(accepted, loadingState);
  assert.equal(loading.focusPhase, "loading");
  assert.strictEqual(loading.focusContinuation, accepted);
  assert.equal(
    historyPrependFocusContinuationIsCurrent(
      loading.focusContinuation!,
      loadingState,
      loading.pending,
      7,
      "loading",
    ),
    true,
  );
  assert.equal(
    focusHistoryPrependReturnIfUnowned(dom.document, loading.focusContinuation!),
    "focused-thread",
    "a still-connected disabled trigger hands focus to its same-owner thread",
  );
  assert.equal(dom.activeKind(), "thread");

  dom.setTriggerDisabled(false);
  const settledState = historyState({ turn_page_offset: 40 });
  const settled = advancePendingHistoryPrepend(loading.pending, settledState);
  assert.equal(settled.focusPhase, "settled");
  assert.equal(
    historyPrependFocusContinuationIsCurrent(
      settled.focusContinuation!,
      settledState,
      settled.pending,
      7,
      "settled",
    ),
    true,
  );
  assert.equal(
    focusHistoryPrependReturnIfUnowned(dom.document, settled.focusContinuation!),
    "focused-trigger",
  );
  assert.equal(dom.activeKind(), "trigger");
});

test("history prepend keeps the anchor and returns off-viewport focus to its visible thread owner", () => {
  const dom = mockHistoryFocusDocument({
    active: "trigger",
    triggerBounds: mockFocusRect(20, -900, 220, -856),
    threadScrollTop: 764,
  });
  const pending = createPendingHistoryPrepend(historyState(), 9, dom.document);
  assert.ok(pending);
  const scrollTopBeforeFocus = dom.threadScrollTop();

  assert.equal(
    focusHistoryPrependReturnIfUnowned(dom.document, pending),
    "focused-thread",
  );
  assert.equal(dom.activeKind(), "thread");
  assert.deepEqual(dom.focusOptions("thread"), { preventScroll: true });
  assert.equal(dom.threadScrollTop(), scrollTopBeforeFocus, "focus fallback must not move the preserved anchor");
});

test("history prepend exports the visible trigger then thread fallback without focusing", () => {
  const dom = mockHistoryFocusDocument({ active: "trigger" });
  const pending = createPendingHistoryPrepend(historyState(), 10, dom.document);
  assert.ok(pending);
  dom.setActive("body");

  const candidates = historyPrependFocusCandidates(dom.document, pending);
  assert.equal(candidates.length, 2);
  assert.equal((candidates[0]?.resolve() as { kind?: string } | null)?.kind, "trigger");
  assert.equal((candidates[1]?.resolve() as { kind?: string } | null)?.kind, "thread");
  assert.equal(dom.activeKind(), "body", "resolving candidates has no focus side effect");

  const offscreen = mockHistoryFocusDocument({
    active: "trigger",
    triggerBounds: mockFocusRect(20, -900, 220, -856),
  });
  const offscreenPending = createPendingHistoryPrepend(historyState(), 11, offscreen.document);
  assert.ok(offscreenPending);
  offscreen.setActive("body");
  const offscreenCandidates = historyPrependFocusCandidates(offscreen.document, offscreenPending);
  assert.equal(offscreenCandidates[0]?.resolve(), null);
  assert.equal(
    (offscreenCandidates[1]?.resolve() as { kind?: string } | null)?.kind,
    "thread",
  );
});

test("history prepend uses the thread at offset zero and never steals meaningful focus", () => {
  const dom = mockHistoryFocusDocument({ active: "trigger" });
  const pending = createPendingHistoryPrepend(historyState(), 11, dom.document);
  assert.ok(pending);
  const accepted = acknowledgePendingHistoryPrepend(pending);
  const settledState = historyState({ turn_page_offset: 0 });
  const settled = advancePendingHistoryPrepend(accepted, settledState);
  assert.ok(settled.focusContinuation);

  dom.removeTrigger();
  dom.setActive("body");
  assert.equal(
    focusHistoryPrependReturnIfUnowned(dom.document, settled.focusContinuation),
    "focused-thread",
  );
  assert.equal(dom.activeKind(), "thread");

  const stillPaged = mockHistoryFocusDocument({ active: "other" });
  assert.equal(
    focusHistoryPrependReturnIfUnowned(stillPaged.document, settled.focusContinuation),
    "owned",
  );
  assert.equal(stillPaged.activeKind(), "other");
});

test("history prepend focus rejects owner, generation, error, and non-trigger activation drift", () => {
  const dom = mockHistoryFocusDocument({ active: "trigger" });
  const pending = createPendingHistoryPrepend(historyState(), 21, dom.document);
  assert.ok(pending);
  const accepted = acknowledgePendingHistoryPrepend(pending);
  const loadingState = historyState({ pending_async_operations: ["turn_page_load"] });

  assert.equal(
    historyPrependFocusContinuationIsCurrent(accepted, loadingState, accepted, 22, "loading"),
    false,
    "a newer request generation owns focus",
  );
  assert.equal(
    historyPrependFocusContinuationIsCurrent(
      accepted,
      historyState({ workspace_path: "C:/other", pending_async_operations: ["turn_page_load"] }),
      accepted,
      21,
      "loading",
    ),
    false,
    "a different workspace/session owner cannot receive focus",
  );
  const failed = advancePendingHistoryPrepend(accepted, historyState());
  assert.equal(failed.disposition, "discard");
  assert.equal(failed.focusContinuation, null, "a failed settlement cannot restore focus");
  assert.equal(rejectPendingHistoryPrepend(accepted, 21), null, "an explicit command error clears the owner");

  const otherActivation = mockHistoryFocusDocument({ active: "other" });
  const withoutFocus = createPendingHistoryPrepend(historyState(), 23, otherActivation.document);
  assert.ok(withoutFocus);
  assert.equal(withoutFocus.returnFocusRequested, false);
  assert.equal(focusHistoryPrependReturnIfUnowned(otherActivation.document, withoutFocus), "not-requested");
});

test("turn-page admission follows only the exact async operation owner", () => {
  assert.equal(turnPageLoadPending(historyState()), false);
  assert.equal(
    turnPageLoadPending(historyState({ pending_async_operations: ["snapshot_refresh"] })),
    false,
  );
  assert.equal(
    turnPageLoadPending(historyState({ pending_async_operations: ["turn_page_load"] })),
    true,
  );
});

test("pending history prepend discards owner changes, failures, and invalid offsets", () => {
  const pending = createPendingHistoryPrepend(historyState(), 11);
  assert.ok(pending);
  const accepted = acknowledgePendingHistoryPrepend(pending);

  assert.equal(
    advancePendingHistoryPrepend(accepted, historyState({ workspace_path: "C:/other" })).disposition,
    "discard",
  );
  assert.equal(
    advancePendingHistoryPrepend(accepted, historyState({ pending_async_operations: [] })).disposition,
    "discard",
    "an accepted command settling without a lower offset is a failed prepend",
  );
  assert.equal(
    advancePendingHistoryPrepend(accepted, historyState({ turn_page_offset: 160 })).disposition,
    "discard",
  );
  assert.equal(rejectPendingHistoryPrepend(pending, 11), null);
  assert.strictEqual(rejectPendingHistoryPrepend(pending, 12), pending, "a stale failure cannot cancel a newer transaction");
  assert.equal(createPendingHistoryPrepend(historyState({ turn_page_offset: 0 }), 12), null);
  assert.equal(createPendingHistoryPrepend(historyState({ selected_session_index: -1 }), 12), null);
});

test("work-summary disclosure identities survive only the phase-appropriate updates", () => {
  const runningBefore = transcriptRow("work_summary_running", "12s 作業中", "以前の進捗");
  const runningAfter = transcriptRow("work_summary_running", "14s 作業中", "新しい進捗");
  assert.equal(
    transcriptAnchors([runningBefore])[0]?.detailsId,
    transcriptAnchors([runningAfter])[0]?.detailsId,
  );
  assert.equal(
    transcriptAnchors([runningBefore])[0]?.id,
    transcriptAnchors([runningAfter])[0]?.id,
    "rail and keyboard focus identity survive live elapsed/body polling",
  );

  const completedA = transcriptRow("work_summary_completed", "Aを完了", "Aの結果");
  const completedB = transcriptRow("work_summary_completed", "Bを完了", "Bの結果");
  const aIdentity = transcriptAnchors([completedA])[0]?.detailsId;
  assert.equal(transcriptAnchors([completedA, completedB])[0]?.detailsId, aIdentity);
  assert.equal(transcriptAnchors([completedB, completedA])[1]?.detailsId, aIdentity);
  assert.notEqual(
    aIdentity,
    transcriptAnchors([transcriptRow("work_summary_completed", "Cを完了", "Cの結果")])[0]?.detailsId,
    "a different terminal disclosure cannot inherit the completed row's open state",
  );
  assert.notEqual(
    transcriptAnchors([runningAfter])[0]?.id,
    transcriptAnchors([completedA])[0]?.id,
    "the terminal row receives its own durable anchor identity",
  );
});

test("durable work-summary anchor survives phase changes while disclosure state is phase-scoped", () => {
  const stableIdentity = "turn:01STABLE:work-summary";
  const running = {
    ...transcriptRow("work_summary_running", "12s 作業中", "以前の進捗"),
    stable_history_identity: stableIdentity,
    file_changes: [{ label: "old", path: "old.txt", action: "更新", summary: "before" }],
  };
  const completed = {
    ...transcriptRow("work_summary_completed", "18s作業しました", "最終結果"),
    stable_history_identity: stableIdentity,
    file_changes: [{ label: "new", path: "new.txt", action: "追加", summary: "after" }],
  };
  const otherTurn = {
    ...completed,
    stable_history_identity: "turn:01OTHER:work-summary",
  };
  const completedUpdated = {
    ...completed,
    title: "19s作業しました",
    body: "更新後の最終結果",
    file_changes: [{ label: "latest", path: "latest.txt", action: "更新", summary: "latest" }],
  };

  const runningAnchor = transcriptAnchors([running])[0]!;
  const completedAnchor = transcriptAnchors([completed])[0]!;
  const completedUpdatedAnchor = transcriptAnchors([completedUpdated])[0]!;
  const otherTurnAnchor = transcriptAnchors([otherTurn])[0]!;

  assert.equal(completedAnchor.id, runningAnchor.id);
  assert.notEqual(
    completedAnchor.detailsId,
    runningAnchor.detailsId,
    "the automatically-open running disclosure must not keep the terminal card open",
  );
  assert.equal(
    completedUpdatedAnchor.detailsId,
    completedAnchor.detailsId,
    "an explicitly opened terminal disclosure survives mutable presentation refreshes",
  );
  assert.notEqual(otherTurnAnchor.id, completedAnchor.id);
  assert.notEqual(otherTurnAnchor.detailsId, completedAnchor.detailsId);
});

test("the latest streaming assistant keeps its rail identity while text grows", () => {
  const user = transcriptRow("user", "ユーザー依頼", "確認してください");
  const before = transcriptRow("assistant", "Assistant", "確認しています");
  const after = transcriptRow("assistant", "Assistant", "確認しています。完了しました");
  assert.equal(
    transcriptAnchors([user, before], { stableLatestAssistant: true })[1]?.id,
    transcriptAnchors([user, after], { stableLatestAssistant: true })[1]?.id,
  );
  assert.notEqual(
    transcriptAnchors([before])[0]?.id,
    transcriptAnchors([after])[0]?.id,
    "terminal/history assistants retain body-derived identities",
  );

  const nextUser = transcriptRow("user", "ユーザー依頼", "次も確認してください");
  assert.equal(
    transcriptAnchors([user, before, nextUser], { stableLatestAssistant: true })[1]?.id,
    transcriptAnchors([user, before])[1]?.id,
    "a prior response returns to its durable body identity once a newer turn starts",
  );
});

function historyState(
  overrides: Partial<HistoryPrependProjection> = {},
): HistoryPrependProjection {
  return {
    workspace_path: "C:/workspace",
    selected_session_index: 0,
    session_rows: [{ session_id: "root-session" }],
    turn_page_offset: 80,
    pending_async_operations: [],
    ...overrides,
  };
}

type HistoryFocusElementKind = "body" | "root" | "thread" | "trigger" | "other";

interface MockHistoryFocusDocument {
  document: Document;
  activeKind: () => HistoryFocusElementKind | null;
  focusOptions: (kind: HistoryFocusElementKind) => FocusOptions | null;
  threadScrollTop: () => number;
  setActive: (kind: HistoryFocusElementKind) => void;
  setTriggerDisabled: (disabled: boolean) => void;
  removeTrigger: () => void;
}

function mockHistoryFocusDocument(
  options: {
    active: HistoryFocusElementKind;
    triggerBounds?: DOMRect;
    threadScrollTop?: number;
  },
): MockHistoryFocusDocument {
  const owner: {
    active: MockHistoryFocusElement | null;
    trigger: MockHistoryFocusElement | null;
    focusOptions: Map<HistoryFocusElementKind, FocusOptions>;
    threadScrollTop: number;
  } = {
    active: null,
    trigger: null,
    focusOptions: new Map(),
    threadScrollTop: options.threadScrollTop ?? 0,
  };
  class MockHistoryFocusElement {
    hidden = false;
    disabled = false;
    readonly kind: HistoryFocusElementKind;

    constructor(kind: HistoryFocusElementKind) {
      this.kind = kind;
    }

    querySelector(selector: string): MockHistoryFocusElement | null {
      return this.kind === "thread" && selector === '[data-focus-key="load-previous-turn-page"]'
        ? owner.trigger
        : null;
    }

    matches(selector: string): boolean {
      return selector === ":disabled" && this.disabled;
    }

    getAttribute(name: string): string | null {
      return name === "aria-disabled" && this.disabled ? "true" : null;
    }

    closest(): MockHistoryFocusElement | null {
      return null;
    }

    getBoundingClientRect(): DOMRect {
      if (this.kind === "thread") return mockFocusRect(0, 100, 800, 500);
      if (this.kind === "trigger") return options.triggerBounds ?? mockFocusRect(20, 120, 220, 164);
      return mockFocusRect(0, 0, 1, 1);
    }

    focus(focusOptions: FocusOptions = {}): void {
      owner.focusOptions.set(this.kind, focusOptions);
      if (!focusOptions.preventScroll) owner.threadScrollTop = 0;
      owner.active = this;
    }
  }

  const elements = new Map<HistoryFocusElementKind, MockHistoryFocusElement>([
    ["body", new MockHistoryFocusElement("body")],
    ["root", new MockHistoryFocusElement("root")],
    ["thread", new MockHistoryFocusElement("thread")],
    ["trigger", new MockHistoryFocusElement("trigger")],
    ["other", new MockHistoryFocusElement("other")],
  ]);
  owner.trigger = elements.get("trigger")!;
  owner.active = elements.get(options.active)!;
  const documentTarget = {
    get activeElement() { return owner.active; },
    body: elements.get("body"),
    documentElement: elements.get("root"),
    querySelector: (selector: string) => selector === "#thread" ? elements.get("thread") : null,
  } as unknown as Document;
  return {
    document: documentTarget,
    activeKind: () => owner.active?.kind ?? null,
    focusOptions: (kind) => owner.focusOptions.get(kind) ?? null,
    threadScrollTop: () => owner.threadScrollTop,
    setActive: (kind) => { owner.active = elements.get(kind)!; },
    setTriggerDisabled: (disabled) => {
      if (owner.trigger) owner.trigger.disabled = disabled;
    },
    removeTrigger: () => { owner.trigger = null; },
  };
}

function mockFocusRect(left: number, top: number, right: number, bottom: number): DOMRect {
  return {
    x: left,
    y: top,
    left,
    top,
    right,
    bottom,
    width: right - left,
    height: bottom - top,
    toJSON: () => ({}),
  } as DOMRect;
}

function transcriptRow(
  rowKind: TranscriptRow["row_kind"],
  title: string,
  body: string,
): TranscriptRow {
  return { row_kind: rowKind, step: "1", title, body, file_changes: [] };
}

function tailOwner(runtimeOwnerToken: string) {
  return {
    workspacePath: "C:/workspace",
    sessionId: "root-session",
    runtimeOwnerToken,
  };
}

function tailProjection(runtimeOwnerToken: string, runActive: boolean, terminal: boolean) {
  return {
    ...tailOwner(runtimeOwnerToken),
    runActive,
    terminal,
  };
}

interface ClampedThread {
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
}

function clampedThread(initial: ClampedThread): ClampedThread {
  let scrollTop = initial.scrollTop;
  const thread = {
    scrollHeight: initial.scrollHeight,
    clientHeight: initial.clientHeight,
    get scrollTop() { return scrollTop; },
    set scrollTop(value: number) {
      scrollTop = Math.max(0, Math.min(value, thread.scrollHeight - thread.clientHeight));
    },
  };
  thread.scrollTop = initial.scrollTop;
  return thread;
}

function threadGap(thread: ClampedThread): number {
  return thread.scrollHeight - thread.scrollTop - thread.clientHeight;
}

interface MockAnchorGeometry {
  element: HTMLElement;
  top: number;
  bottom: number;
}

function mockAnchor(id: string, top: number, bottom: number): MockAnchorGeometry {
  const geometry: MockAnchorGeometry = {
    element: null as unknown as HTMLElement,
    top,
    bottom,
  };
  geometry.element = {
    dataset: { historyAnchor: id },
    getBoundingClientRect: () => ({ top: geometry.top, bottom: geometry.bottom }),
  } as unknown as HTMLElement;
  return geometry;
}

function mockThread(
  initialNodes: MockAnchorGeometry[],
  initial: { scrollTop: number; scrollHeight: number },
): { element: HTMLElement; nodes: MockAnchorGeometry[]; scrollHeight: number } {
  const owner = {
    nodes: initialNodes,
    scrollHeight: initial.scrollHeight,
    element: null as unknown as HTMLElement,
  };
  owner.element = {
    scrollTop: initial.scrollTop,
    get scrollHeight() { return owner.scrollHeight; },
    getBoundingClientRect: () => ({ top: 50, bottom: 450 }),
    querySelectorAll: () => owner.nodes.map((node) => node.element),
  } as unknown as HTMLElement;
  return owner;
}

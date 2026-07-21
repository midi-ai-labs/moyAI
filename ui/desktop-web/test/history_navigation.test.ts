import assert from "node:assert/strict";
import test from "node:test";
import {
  acknowledgePendingHistoryPrepend,
  advancePendingHistoryPrepend,
  captureViewportAnchor,
  createPendingHistoryPrepend,
  pinThreadToEnd,
  pinResolvedThreadToEnd,
  rejectPendingHistoryPrepend,
  restoreViewportAnchor,
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

test("durable work-summary identity survives mutable presentation and separates turns", () => {
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

  const runningAnchor = transcriptAnchors([running])[0]!;
  const completedAnchor = transcriptAnchors([completed])[0]!;
  const otherTurnAnchor = transcriptAnchors([otherTurn])[0]!;

  assert.equal(completedAnchor.id, runningAnchor.id);
  assert.equal(completedAnchor.detailsId, runningAnchor.detailsId);
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

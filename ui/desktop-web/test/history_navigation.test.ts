import assert from "node:assert/strict";
import test from "node:test";
import {
  acknowledgePendingHistoryPrepend,
  advancePendingHistoryPrepend,
  captureViewportAnchor,
  createPendingHistoryPrepend,
  pinThreadToEnd,
  rejectPendingHistoryPrepend,
  restoreViewportAnchor,
  shouldRevealThreadEnd,
  transcriptAnchors,
  turnPageLoadPending,
  type HistoryPrependProjection,
} from "../src/history_navigation.ts";
import type { TranscriptRow } from "../src/types.ts";

test("an explicit run start reveals the new turn even when history was scrolled away from the end", () => {
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

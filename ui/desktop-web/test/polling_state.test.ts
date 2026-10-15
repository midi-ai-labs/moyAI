import assert from "node:assert/strict";
import test from "node:test";
import { createSnapshotRefresh, installRuntimePolling, installSnapshotInvalidation } from "../src/polling_state.ts";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(accept => { resolve = accept; });
  return { promise, resolve };
}

// These generic lifecycle regressions previously lived with the retired manual
// publishing editor. Their normal Desktop snapshot-owner coverage remains active.
test("window restore refreshes an idle snapshot and disposed subscriptions stay closed", () => {
  for (const restoredEvent of ["focus", "visibilitychange"]) {
    const windowTarget = new EventTarget();
    const documentTarget = Object.assign(new EventTarget(), { hidden: true });
    let tick = () => {};
    let cleared = false;
    let refreshes = 0;
    const stop = installRuntimePolling(Object.assign(windowTarget, {
      setInterval(callback: TimerHandler) { tick = callback as () => void; return 1; },
      clearInterval(id: number) { assert.equal(id, 1); cleared = true; },
    }), documentTarget, () => false, afterInFlight => {
      assert.equal(afterInFlight, true);
      refreshes += 1;
    });
    tick();
    windowTarget.dispatchEvent(new Event("focus"));
    documentTarget.dispatchEvent(new Event("visibilitychange"));
    assert.equal(refreshes, 0);
    documentTarget.hidden = false;
    (restoredEvent === "focus" ? windowTarget : documentTarget).dispatchEvent(new Event(restoredEvent));
    assert.equal(refreshes, 1);
    stop();
    assert.equal(cleared, true);
    windowTarget.dispatchEvent(new Event("focus"));
    documentTarget.dispatchEvent(new Event("visibilitychange"));
    assert.equal(refreshes, 1);
  }
});

test("restore during an old snapshot coalesces one fresh read without overlapping requests", async () => {
  const delayed = deferred<number>();
  let calls = 0;
  let active = 0;
  let maxActive = 0;
  let observed = 0;
  const refresh = createSnapshotRefresh(async () => {
    active += 1;
    maxActive = Math.max(maxActive, active);
    observed = ++calls === 1 ? await delayed.promise : 8;
    active -= 1;
  });
  const windowTarget = Object.assign(new EventTarget(), {
    setInterval(_callback: TimerHandler) { return 1; }, clearInterval(_id: number) {},
  });
  const documentTarget = Object.assign(new EventTarget(), { hidden: true });
  const stop = installRuntimePolling(windowTarget, documentTarget, () => false, refresh);
  const oldRead = refresh();
  await Promise.resolve();
  const overlappingTick = refresh();
  documentTarget.hidden = false;
  windowTarget.dispatchEvent(new Event("focus"));
  documentTarget.dispatchEvent(new Event("visibilitychange"));
  assert.equal(calls, 1);
  delayed.resolve(7);
  await Promise.all([oldRead, overlappingTick]);
  stop();
  assert.equal(calls, 2);
  assert.equal(maxActive, 1);
  assert.equal(observed, 8);
});

test("ordinary overlapping polling does not queue an extra snapshot without a restore request", async () => {
  const delayed = deferred<void>();
  let calls = 0;
  const refresh = createSnapshotRefresh(async () => { calls += 1; await delayed.promise; });
  const first = refresh();
  await Promise.resolve();
  const second = refresh();
  delayed.resolve();
  await Promise.all([first, second]);
  assert.equal(calls, 1);
});

test("native state invalidation refreshes an idle shell without focus or periodic polling", async () => {
  const nativeEvents = new EventTarget();
  let state = "personal";
  let rendered = "personal";
  let reads = 0;
  const refresh = createSnapshotRefresh(async () => { reads += 1; rendered = state; });
  const unsubscribe = await installSnapshotInvalidation(async onChanged => {
    nativeEvents.addEventListener("changed", onChanged);
    return () => nativeEvents.removeEventListener("changed", onChanged);
  }, refresh);
  await refresh();
  let tick = () => {};
  const stopPolling = installRuntimePolling(Object.assign(new EventTarget(), {
    setInterval(callback: TimerHandler) { tick = callback as () => void; return 1; },
    clearInterval(_id: number) {},
  }), Object.assign(new EventTarget(), { hidden: false }), () => false, refresh);
  const before = reads;
  state = "team";
  tick();
  assert.equal(rendered, "personal", "idle periodic polling does not discover a native commit");
  nativeEvents.dispatchEvent(new Event("changed"));
  await refresh();
  assert.equal(rendered, "team");
  assert.equal(reads, before + 1);
  unsubscribe();
  nativeEvents.dispatchEvent(new Event("changed"));
  await Promise.resolve();
  assert.equal(reads, before + 1);
  stopPolling();
});

test("subscription settlement catches a native commit before listener registration", async () => {
  const subscribed = deferred<() => void>();
  let state = "personal";
  let rendered = state;
  const refresh = createSnapshotRefresh(async () => { rendered = state; });
  await refresh();
  const installing = installSnapshotInvalidation(() => subscribed.promise, refresh);
  state = "team";
  subscribed.resolve(() => {});
  await installing;
  await refresh();
  assert.equal(rendered, "team");
});

test("native invalidation during an old snapshot coalesces one fresh read", async () => {
  const nativeEvents = new EventTarget();
  const oldSnapshot = deferred<string>();
  let calls = 0;
  let active = 0;
  let maxActive = 0;
  let observed = "";
  const refresh = createSnapshotRefresh(async () => {
    active += 1;
    maxActive = Math.max(maxActive, active);
    observed = ++calls === 1 ? await oldSnapshot.promise : "team";
    active -= 1;
  });
  const unsubscribe = await installSnapshotInvalidation(async onChanged => {
    nativeEvents.addEventListener("changed", onChanged);
    return () => nativeEvents.removeEventListener("changed", onChanged);
  }, refresh);
  const pending = refresh();
  await Promise.resolve();
  nativeEvents.dispatchEvent(new Event("changed"));
  nativeEvents.dispatchEvent(new Event("changed"));
  oldSnapshot.resolve("personal");
  await pending;
  assert.equal(calls, 2);
  assert.equal(maxActive, 1);
  assert.equal(observed, "team");
  unsubscribe();
});

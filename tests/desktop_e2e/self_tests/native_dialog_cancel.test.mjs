import assert from "node:assert/strict";
import test from "node:test";

import { DesktopE2eError } from "../core/execution.mjs";
import {
  classifyPostNativeCancelProjectionFailure,
  classifyPostNativeCancelWindowFailure,
  settleNativeDialogBeforeExit,
} from "../scenarios/native_dialog_cancel.mjs";

const candidate = Object.freeze({
  hwnd: "0x200",
  root_hwnd: "0x200",
  owner_hwnd: null,
  process_id: 4100,
  thread_id: 812,
  class_name: "ObservedNativeDialogClass",
  title: "",
  visible: true,
  enabled: true,
  is_root: true,
});

function timeout(lastValue) {
  const error = new Error("observation timed out");
  error.code = "observation-timeout";
  error.evidence = { last_value: lastValue, attempts: 3 };
  return error;
}

function cleanupState(overrides = {}) {
  return {
    actionMayHaveDispatched: true,
    dialogSettled: false,
    candidate: { ...candidate },
    before: { windows: [] },
    nativeOwner: { expectedOwner: { process_id: 4100 } },
    sink: null,
    fallback: null,
    cleanupFailures: [],
    cleanupEvidenceFailures: [],
    ...overrides,
  };
}

test("verified native cancel converts reachable close and projection predicate failures to product failures", () => {
  const close = classifyPostNativeCancelWindowFailure(timeout({ live: true, window: { ...candidate } }), candidate);
  assert.ok(close instanceof DesktopE2eError);
  assert.equal(close.owner, "product");
  assert.equal(close.code, "native-dialog-did-not-close");

  const projection = classifyPostNativeCancelProjectionFailure(timeout({ status_message: "unexpected" }));
  assert.ok(projection instanceof DesktopE2eError);
  assert.equal(projection.owner, "product");
  assert.equal(projection.code, "native-dialog-cancel-projection-mismatch");
});

test("post-cancel transport failures remain harness acquisition failures", () => {
  const transport = new Error("snapshot transport unavailable");
  assert.equal(classifyPostNativeCancelWindowFailure(transport, candidate), transport);
  assert.equal(classifyPostNativeCancelProjectionFailure(transport), transport);
});

test("cleanup requires an exact destroyed probe for a retained HWND and evidence failure cannot skip shell exit", async () => {
  const calls = [];
  const state = cleanupState({
    sink: { record: async () => { throw new Error("evidence unavailable"); } },
  });
  const outcome = await settleNativeDialogBeforeExit({
    state,
    cdp: {},
    snapshotWindows: async () => { throw new Error("retained HWND must not use a visible-window snapshot"); },
    probeWindow: async ({ candidate: actual }) => {
      calls.push(`probe:${actual.hwnd}`);
      return {
        live: false,
        exact_identity: true,
        identity_state: "destroyed",
        expected_hwnd: actual.hwnd,
        window: null,
      };
    },
    closeWindow: async () => { calls.push("close"); },
    waitWindowGone: async () => { calls.push("wait"); },
    requestExit: async () => { calls.push("exit"); return { requested: true, reason: null }; },
  });
  assert.deepEqual(outcome, { requested: true, reason: null });
  assert.deepEqual(calls, ["probe:0x200", "exit"]);
  assert.equal(state.dialogSettled, true);
  assert.equal(state.fallback.kind, "already-destroyed");
  assert.equal(state.cleanupEvidenceFailures.length, 1);
});

test("cleanup closes a retained hidden exact HWND and verifies destruction before shell exit", async () => {
  const calls = [];
  const state = cleanupState();
  const outcome = await settleNativeDialogBeforeExit({
    state,
    cdp: {},
    snapshotWindows: async () => { throw new Error("retained HWND must not use a visible-window snapshot"); },
    probeWindow: async ({ candidate: actual }) => {
      calls.push(`probe:${actual.hwnd}`);
      return {
        live: true,
        exact_identity: true,
        identity_state: "live-exact-owner",
        expected_hwnd: actual.hwnd,
        window: { ...actual, visible: false, enabled: false },
      };
    },
    closeWindow: async ({ candidate: actual }) => {
      calls.push(`close:${actual.hwnd}`);
      assert.equal(actual.visible, false);
      return { requested: true, cleanup_only: true };
    },
    waitWindowGone: async (_owner, actual) => {
      calls.push(`wait:${actual.hwnd}`);
      return { attempts: 2, elapsed_ms: 25, value: { live: false, window: null } };
    },
    requestExit: async () => { calls.push("exit"); return { requested: true, reason: null }; },
  });
  assert.deepEqual(outcome, { requested: true, reason: null });
  assert.deepEqual(calls, ["probe:0x200", "close:0x200", "wait:0x200", "exit"]);
  assert.equal(state.dialogSettled, true);
  assert.equal(state.candidate, null);
  assert.equal(state.fallback.kind, "exact-hwnd-close");
});

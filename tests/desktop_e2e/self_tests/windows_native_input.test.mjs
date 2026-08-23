import assert from "node:assert/strict";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { mkdtemp, rm, writeFile } from "node:fs/promises";

import {
  NativeInputError,
  captureOwnedWindowPng,
  closeOwnedNativeDialog,
  closeOwnedWindowForCleanup,
  probeExactOwnedWindow,
  selectFreshOwnedRootWindow,
  selectFreshForegroundWindow,
  sendEscapeToOwnedForegroundWindow,
  snapshotOwnedTopLevelWindows,
} from "../drivers/windows_native_input.mjs";
import { invokeWindowsProcess } from "../drivers/windows_process.mjs";

const owner = Object.freeze({
  process_id: 4100,
  process_start_time_utc_ticks: "638914752000000000",
  executable_path: "C:\\moyai\\moyai-desktop.exe",
});

function windowRow(hwnd, overrides = {}) {
  return {
    hwnd,
    root_hwnd: hwnd,
    owner_hwnd: null,
    process_id: owner.process_id,
    thread_id: 812,
    class_name: "ObservedNativeDialogClass",
    title: "localized title is observation only",
    visible: true,
    enabled: true,
    is_root: true,
    rect: { left: 10, top: 20, right: 610, bottom: 420, width: 600, height: 400 },
    ...overrides,
  };
}

function snapshot(windows, foregroundRoot = null, overrides = {}) {
  return {
    owner: { ...owner },
    foreground_hwnd: foregroundRoot,
    foreground_root_hwnd: foregroundRoot,
    foreground_process_id: foregroundRoot === null ? null : owner.process_id,
    windows,
    ...overrides,
  };
}

test("fresh native dialog selection requires the exact-owner visible foreground root and tolerates auxiliary roots", () => {
  const main = windowRow("0x100", { class_name: "TauriMain", thread_id: 700 });
  const dialog = windowRow("0x200");
  const selected = selectFreshForegroundWindow(snapshot([main]), snapshot([main, dialog], dialog.hwnd), owner);
  assert.deepEqual(selected, dialog);

  assert.throws(
    () => selectFreshForegroundWindow(snapshot([main]), snapshot([main], main.hwnd), owner),
    (error) => error instanceof NativeInputError && error.code === "native-window-cardinality",
  );
  const tooltip = windowRow("0x300", { class_name: "tooltips_class32", owner_hwnd: dialog.hwnd });
  const shadow = windowRow("0x301", { class_name: "SysShadow" });
  assert.deepEqual(
    selectFreshForegroundWindow(snapshot([main]), snapshot([main, tooltip, shadow, dialog], dialog.hwnd), owner),
    dialog,
    "same-process auxiliary roots cannot make the exact foreground root ambiguous",
  );
  assert.throws(
    () => selectFreshForegroundWindow(snapshot([main]), snapshot([main, dialog, tooltip], "0x999"), owner),
    (error) => error instanceof NativeInputError
      && error.code === "native-window-cardinality"
      && error.evidence.fresh_windows.length === 2,
  );
  assert.throws(
    () => selectFreshForegroundWindow(snapshot([main]), snapshot([main, dialog], main.hwnd), owner),
    (error) => error instanceof NativeInputError && error.code === "native-window-not-foreground",
  );
  assert.throws(
    () => selectFreshForegroundWindow(snapshot([main]), snapshot([main, dialog], null), owner),
    (error) => error instanceof NativeInputError && error.code === "native-window-not-foreground",
  );
});

test("fresh owned-root selection isolates an exact native class before foreground activation", () => {
  const main = windowRow("0x100", { class_name: "TauriMain", thread_id: 700 });
  const dialog = windowRow("0x200", { class_name: "#32770" });
  const tooltip = windowRow("0x300", { class_name: "tooltips_class32", owner_hwnd: dialog.hwnd });
  const shadow = windowRow("0x301", { class_name: "SysShadow" });
  const before = snapshot([main], main.hwnd);
  const after = snapshot([main, tooltip, shadow, dialog], "0x999", {
    foreground_process_id: owner.process_id + 1,
  });

  assert.deepEqual(
    selectFreshOwnedRootWindow(before, after, owner, { expectedClassName: "#32770" }),
    dialog,
  );
  assert.throws(
    () => selectFreshOwnedRootWindow(before, after, owner, { expectedClassName: "MissingClass" }),
    (error) => error instanceof NativeInputError
      && error.code === "native-window-cardinality"
      && error.evidence.fresh_windows.length === 0
      && error.evidence.auxiliary_fresh_windows.length === 3,
  );
});

test("fresh selection rejects hidden, disabled, non-root, foreign, duplicate, and drifted process identities", () => {
  const main = windowRow("0x100", { class_name: "TauriMain" });
  const hidden = windowRow("0x200", { visible: false });
  assert.throws(
    () => selectFreshForegroundWindow(snapshot([main]), snapshot([main, hidden], hidden.hwnd), owner),
    (error) => error.code === "native-window-cardinality",
  );
  const disabled = windowRow("0x203", { enabled: false });
  assert.throws(
    () => selectFreshOwnedRootWindow(snapshot([main]), snapshot([main, disabled], disabled.hwnd), owner),
    (error) => error.code === "native-window-cardinality",
  );
  const nonRoot = windowRow("0x201", { root_hwnd: "0x100", is_root: false });
  assert.throws(
    () => selectFreshForegroundWindow(snapshot([main]), snapshot([main, nonRoot], nonRoot.hwnd), owner),
    (error) => error.code === "native-window-cardinality",
  );
  const foreign = windowRow("0x202", { process_id: owner.process_id + 1 });
  assert.throws(
    () => selectFreshForegroundWindow(snapshot([main]), snapshot([main, foreign], foreign.hwnd), owner),
    (error) => error.code === "native-window-owner-mismatch",
  );
  assert.throws(
    () => selectFreshForegroundWindow(snapshot([main]), snapshot([main, { ...main }], main.hwnd), owner),
    /duplicate HWND/,
  );
  const driftedOwner = { ...owner, process_start_time_utc_ticks: "638914752000000001" };
  assert.throws(
    () => selectFreshForegroundWindow(snapshot([main]), snapshot([main], null, { owner: driftedOwner }), owner),
    (error) => error.code === "native-owner-identity-drift",
  );
});

test("Escape, UIA dialog close, PNG, and exact-HWND cleanup wrappers preserve their delivery boundaries", async () => {
  const candidate = windowRow("0xA20");
  const calls = [];
  const invoke = async (action, parameters) => {
    calls.push({ action, parameters });
    if (action === "SendEscape") {
      return {
        foreground_activation_attempted: true,
        foreground_activation_verified: true,
        foreground_pre_input_verified: true,
        foreground_post_input_verified: true,
        foreground_verified: true,
        delivery_verified: true,
        input_count: 2,
        cleanup_only: false,
        representative_input: true,
        window: candidate,
      };
    }
    if (action === "CapturePng") {
      return { available: true, png_base64: "iVBORw0KGgo=", window: candidate };
    }
    if (action === "CloseDialog") {
      return {
        requested: true,
        request_count: 1,
        attempted: true,
        attempt_count: 1,
        may_have_dispatched: true,
        confirmed: true,
        call_returned: true,
        window_pattern_verified: true,
        foreground_required: false,
        cleanup_only: false,
        representative_input: true,
        ui_automation_window: { native_hwnd: candidate.hwnd, process_id: candidate.process_id },
        window: candidate,
      };
    }
    if (action === "CloseCleanup") {
      return { requested: true, cleanup_only: true, representative_input: false, window: candidate };
    }
    throw new Error(`unexpected action ${action}`);
  };

  const escape = await sendEscapeToOwnedForegroundWindow(
    { executionRoot: "C:\\execution", ownerPath: "C:\\execution\\owner.json", candidate },
    { invoke },
  );
  assert.equal(escape.input_count, 2);
  const dialogClose = await closeOwnedNativeDialog(
    { executionRoot: "C:\\execution", ownerPath: "C:\\execution\\owner.json", candidate },
    { invoke },
  );
  assert.equal(dialogClose.request_count, 1);
  const capture = await captureOwnedWindowPng(
    { executionRoot: "C:\\execution", ownerPath: "C:\\execution\\owner.json", candidate },
    { invoke },
  );
  assert.equal(capture.available, true);
  assert.deepEqual(capture.bytes.subarray(0, 8), Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]));
  const cleanup = await closeOwnedWindowForCleanup(
    { executionRoot: "C:\\execution", ownerPath: "C:\\execution\\owner.json", candidate },
    { invoke },
  );
  assert.equal(cleanup.cleanup_only, true);
  assert.deepEqual(calls.map((call) => call.action), ["SendEscape", "CloseDialog", "CapturePng", "CloseCleanup"]);
  for (const call of calls) {
    assert.equal(call.parameters.WindowHandle, candidate.hwnd);
    assert.equal(call.parameters.ExpectedThreadId, candidate.thread_id);
    assert.equal(call.parameters.ExpectedClassName, candidate.class_name);
  }

  await assert.rejects(
    () => sendEscapeToOwnedForegroundWindow(
      { executionRoot: "C:\\execution", ownerPath: "C:\\execution\\owner.json", candidate },
      { invoke: async () => ({
        foreground_activation_verified: false,
        foreground_pre_input_verified: false,
        foreground_post_input_verified: false,
        foreground_verified: false,
        delivery_verified: false,
        input_count: 0,
        cleanup_only: false,
        representative_input: false,
      }) },
    ),
    (error) => error.code === "native-escape-delivery-invalid" && error.evidence.input_count === 0,
  );
  await assert.rejects(
    () => sendEscapeToOwnedForegroundWindow(
      { executionRoot: "C:\\execution", ownerPath: "C:\\execution\\owner.json", candidate },
      { invoke: async () => ({
        foreground_activation_verified: true,
        foreground_pre_input_verified: true,
        foreground_post_input_verified: false,
        foreground_verified: false,
        delivery_verified: false,
        input_count: 2,
        cleanup_only: false,
        representative_input: true,
      }) },
    ),
    (error) => error.code === "native-escape-delivery-invalid" && error.evidence.input_count === 2,
  );
  await assert.rejects(
    () => closeOwnedNativeDialog(
      { executionRoot: "C:\\execution", ownerPath: "C:\\execution\\owner.json", candidate },
      { invoke: async () => ({
        requested: false,
        request_count: 1,
        attempted: true,
        attempt_count: 1,
        may_have_dispatched: true,
        confirmed: false,
        call_returned: false,
        window_pattern_verified: true,
        foreground_required: false,
        cleanup_only: false,
        representative_input: true,
        close_error: { type: "System.Runtime.InteropServices.COMException", hresult: -1, message: "ambiguous" },
        ui_automation_window: { native_hwnd: candidate.hwnd, process_id: candidate.process_id },
      }) },
    ),
    (error) => error.code === "native-dialog-close-invalid"
      && error.evidence.attempt_count === 1
      && error.evidence.may_have_dispatched === true
      && error.evidence.confirmed === false,
  );
  await assert.rejects(
    () => closeOwnedWindowForCleanup(
      { executionRoot: "C:\\execution", ownerPath: "C:\\execution\\owner.json", candidate },
      { invoke: async () => ({ requested: true, cleanup_only: false, representative_input: true }) },
    ),
    (error) => error.code === "native-cleanup-close-invalid",
  );
});

test("exact-HWND probe distinguishes hidden live windows from destroyed windows", async () => {
  const candidate = windowRow("0xA21");
  const hidden = { ...candidate, visible: false, enabled: false };
  const parameters = { executionRoot: "C:\\execution", ownerPath: "C:\\execution\\owner.json", candidate };

  const live = await probeExactOwnedWindow(parameters, {
    invoke: async (action) => {
      assert.equal(action, "ProbeWindow");
      return {
        live: true,
        exact_identity: true,
        identity_state: "live-exact-owner",
        expected_hwnd: candidate.hwnd,
        window: hidden,
        cleanup_only: false,
        representative_input: false,
      };
    },
  });
  assert.equal(live.live, true);
  assert.equal(live.window.visible, false);
  assert.equal(live.window.enabled, false);

  const destroyed = await probeExactOwnedWindow(parameters, {
    invoke: async () => ({
      live: false,
      exact_identity: true,
      identity_state: "destroyed",
      expected_hwnd: candidate.hwnd,
      window: null,
      cleanup_only: false,
      representative_input: false,
    }),
  });
  assert.equal(destroyed.live, false);

  await assert.rejects(
    () => probeExactOwnedWindow(parameters, {
      invoke: async () => ({
        live: true,
        exact_identity: true,
        identity_state: "live-exact-owner",
        expected_hwnd: candidate.hwnd,
        window: { ...hidden, thread_id: candidate.thread_id + 1 },
        cleanup_only: false,
        representative_input: false,
      }),
    }),
    (error) => error.code === "native-window-liveness-invalid",
  );
});

test("PowerShell snapshot revalidates the exact PID/start/executable owner without opening GUI", { skip: process.platform !== "win32" }, async (context) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "moyai-e2e-native-input-"));
  context.after(() => rm(root, { recursive: true, force: true }));
  const liveOwner = await invokeWindowsProcess("Capture", { ProcessId: process.pid });
  const ownerPath = path.join(root, "owner.json");
  await writeFile(ownerPath, JSON.stringify(liveOwner), { flag: "wx" });
  const observed = await snapshotOwnedTopLevelWindows({ executionRoot: root, ownerPath, expectedOwner: liveOwner });
  assert.equal(observed.owner.process_id, process.pid);
  assert.equal(observed.owner.process_start_time_utc_ticks, liveOwner.process_start_time_utc_ticks);
  assert.equal(Array.isArray(observed.windows), true);
  for (const row of observed.windows) {
    assert.match(row.hwnd, /^0x[0-9A-F]+$/);
    assert.equal(row.process_id, process.pid);
    assert.equal(row.visible, true);
  }

  const driftedPath = path.join(root, "owner-drifted.json");
  await writeFile(driftedPath, JSON.stringify({
    ...liveOwner,
    process_start_time_utc_ticks: String(BigInt(liveOwner.process_start_time_utc_ticks) + 1n),
  }), { flag: "wx" });
  await assert.rejects(
    () => snapshotOwnedTopLevelWindows({ executionRoot: root, ownerPath: driftedPath }),
    /Process start identity changed/,
  );
});

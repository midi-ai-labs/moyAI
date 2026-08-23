import { DesktopE2eError } from "../core/execution.mjs";
import { waitForObservation } from "../core/deadline.mjs";
import { WebviewInput, assertTrustedProbeSequence } from "../drivers/webview_input.mjs";
import {
  captureOwnedWindowPng,
  closeOwnedWindowForCleanup,
  closeOwnedNativeDialog,
  probeExactOwnedWindow,
  selectFreshOwnedRootWindow,
  snapshotOwnedTopLevelWindows,
} from "../drivers/windows_native_input.mjs";
import {
  acquireInteractiveShell,
  prepareShellBaseline,
  quiesceShellBaseline,
  requestGracefulExit as requestShellExit,
} from "./shell_baseline.mjs";
import {
  captureScenarioScreenshot,
  invokeDesktopCommand,
  selectedNavigationIdentity,
} from "./observations.mjs";

const OWNER = "scenario:native-dialog.cancel";
const NATIVE_FOLDER_DIALOG_CLASS = "#32770";
const PICKER = Object.freeze({
  selector: 'button[data-action="create-project-from-picker"][aria-label="プロジェクトを作成"]',
  identity: { tag: "BUTTON", action: "create-project-from-picker" },
});

function sameValue(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

function revisionAfter(value, baseline) {
  try { return BigInt(value ?? "-1") > BigInt(baseline ?? "-1"); }
  catch { return false; }
}

function nativeContext(context, runtime) {
  return {
    executionRoot: context.root,
    ownerPath: runtime.desktop_owner_path,
    expectedOwner: runtime.desktop_owner,
  };
}

async function waitForFreshDialog({ context, runtime, before, rememberCandidate }) {
  const owner = nativeContext(context, runtime);
  return waitForObservation({
    label: "fresh exact-PID native dialog foreground",
    timeoutMs: 30_000,
    pollMs: 100,
    retrySampleErrors: false,
    sample: async () => {
      const after = await snapshotOwnedTopLevelWindows(owner);
      try {
        const candidate = selectFreshOwnedRootWindow(before, after, runtime.desktop_owner, {
          expectedClassName: NATIVE_FOLDER_DIALOG_CLASS,
        });
        rememberCandidate(candidate);
        return { acquired: true, after, candidate, wait_reason: null };
      } catch (error) {
        if (error?.code === "native-window-cardinality" && error?.evidence?.fresh_windows?.length === 0) {
          return { acquired: false, after, candidate: null, wait_reason: error.code };
        }
        throw error;
      }
    },
    accept: (value) => value.acquired === true,
  });
}

async function waitForWindowDestroyed(
  owner,
  candidate,
  timeoutMs = 10_000,
  probeWindow = probeExactOwnedWindow,
) {
  return waitForObservation({
    label: `native window ${candidate.hwnd} destroyed`,
    timeoutMs,
    pollMs: 100,
    sample: () => probeWindow({ ...owner, candidate }),
    accept: (observation) => observation.live === false,
    retrySampleErrors: false,
  });
}

function productFailure(code, message, evidence) {
  return new DesktopE2eError("product", code, message, evidence);
}

function errorObservation(error) {
  return {
    name: error?.name ?? "Error",
    code: error?.code ?? null,
    message: error?.message ?? String(error),
    evidence: error?.evidence ?? null,
  };
}

export function classifyPostNativeCancelWindowFailure(error, candidate) {
  const last = error?.evidence?.last_value;
  if (
    error?.code === "observation-timeout"
    && last?.live === true
    && last?.window?.hwnd === candidate?.hwnd
  ) {
    return productFailure(
      "native-dialog-did-not-close",
      "the exact native dialog remained open after verified UI Automation WindowPattern.Close delivery",
      { candidate, observation: error.evidence },
    );
  }
  return error;
}

export function classifyPostNativeCancelProjectionFailure(error) {
  if (
    error?.code === "observation-timeout"
    && error?.evidence !== null
    && typeof error.evidence === "object"
    && Object.hasOwn(error.evidence, "last_value")
    && error.evidence.last_value !== null
  ) {
    return productFailure(
      "native-dialog-cancel-projection-mismatch",
      "the reachable Desktop projection did not settle to the native-dialog cancellation contract",
      { observation: error.evidence },
    );
  }
  return error;
}

export function selectCleanupDialogCandidate({ before, current, retained, expectedOwner }) {
  if (retained !== null) {
    return current.windows.some((window) => window.hwnd === retained.hwnd)
      ? structuredClone(retained)
      : null;
  }
  try {
    return selectFreshOwnedRootWindow(before, current, expectedOwner, {
      expectedClassName: NATIVE_FOLDER_DIALOG_CLASS,
    });
  } catch (error) {
    if (error?.code === "native-window-cardinality" && error?.evidence?.fresh_windows?.length === 0) {
      return null;
    }
    throw error;
  }
}

export async function settleNativeDialogBeforeExit({
  state,
  cdp,
  snapshotWindows = snapshotOwnedTopLevelWindows,
  probeWindow = probeExactOwnedWindow,
  closeWindow = closeOwnedWindowForCleanup,
  waitWindowGone = waitForWindowDestroyed,
  requestExit = requestShellExit,
}) {
  if (state.actionMayHaveDispatched && !state.dialogSettled) {
    if (state.nativeOwner === null || state.before === null) {
      return { requested: false, reason: "native-dialog-owner-unresolved" };
    }
    try {
      let candidate;
      let acquisition;
      if (state.candidate !== null) {
        const probe = await probeWindow({ ...state.nativeOwner, candidate: state.candidate });
        acquisition = { kind: "retained-exact-hwnd-probe", probe };
        candidate = probe.live ? probe.window : null;
      } else {
        const current = await snapshotWindows(state.nativeOwner);
        acquisition = { kind: "fresh-window-snapshot", snapshot: current };
        candidate = selectCleanupDialogCandidate({
          before: state.before,
          current,
          retained: null,
          expectedOwner: state.nativeOwner.expectedOwner,
        });
      }
      if (candidate === null) {
        state.dialogSettled = true;
        state.candidate = null;
        state.fallback = {
          kind: acquisition.kind === "retained-exact-hwnd-probe" ? "already-destroyed" : "already-absent",
          acquisition,
        };
      } else {
        state.candidate = candidate;
        const closed = await closeWindow({ ...state.nativeOwner, candidate });
        const gone = await waitWindowGone(state.nativeOwner, candidate);
        state.fallback = {
          kind: "exact-hwnd-close",
          acquisition,
          closed,
          attempts: gone.attempts,
          elapsed_ms: gone.elapsed_ms,
          final_probe: gone.value,
        };
        state.dialogSettled = true;
        state.candidate = null;
      }
      if (state.sink) {
        try {
          await state.sink.record("native-dialog-cleanup-fallback", state.fallback, { phase: "cleaning", owner: OWNER });
        } catch (error) {
          state.cleanupEvidenceFailures.push(errorObservation(error));
        }
      }
    } catch (error) {
      state.cleanupFailures.push(errorObservation(error));
      return { requested: false, reason: `native-dialog-cleanup-fallback-failed: ${error.message}` };
    }
  }
  return requestExit(cdp);
}

export function createNativeDialogCancelScenario() {
  const state = {
    actionMayHaveDispatched: false,
    closeAdapterInvoked: false,
    closeAttempted: false,
    closeConfirmed: false,
    closeAmbiguous: false,
    closeDispatchUnknown: false,
    closeFailure: null,
    dialogSettled: false,
    candidate: null,
    before: null,
    nativeOwner: null,
    sink: null,
    fallback: null,
    inputCleanupFailure: null,
    cleanupFailures: [],
    cleanupEvidenceFailures: [],
    executionEvidenceFailures: [],
  };
  return Object.freeze({
    id: "native-dialog.cancel",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    prepare: prepareShellBaseline,
    quiesce: quiesceShellBaseline,
    async execute({ context, runtime, driver: cdp, sink }) {
      state.sink = sink;
      state.nativeOwner = nativeContext(context, runtime);
      await acquireInteractiveShell({ context, driver: cdp, sink }, {
        evidenceOwner: OWNER,
        screenshotStem: "native-dialog-shell-ready",
      });
      const initial = await invokeDesktopCommand(cdp, "desktop_state");
      const initialIdentity = selectedNavigationIdentity(initial);
      const before = await snapshotOwnedTopLevelWindows(state.nativeOwner);
      state.before = before;
      await sink.record("native-window-baseline", { snapshot: before }, { phase: "executing", owner: OWNER });

      const input = new WebviewInput(cdp, { probeId: "native-dialog-pointer" });
      let primaryError = null;
      try {
        await input.installProbe();
        const pointerStart = (await input.snapshotProbe()).sequence;
        let target;
        try {
          target = await input.pointerDown(PICKER);
        } catch (error) {
          state.actionMayHaveDispatched = input.pointerPressed;
          throw error;
        }
        state.actionMayHaveDispatched = true;
        let pointerReleaseError = null;
        try {
          await input.pointerUp();
        } catch (error) {
          pointerReleaseError = error;
        }
        let acquired;
        try {
          acquired = await waitForFreshDialog({
            context,
            runtime,
            before,
            rememberCandidate: (candidate) => { state.candidate = candidate; },
          });
        } catch (error) {
          if (pointerReleaseError !== null) {
            throw new DesktopE2eError(
              "harness",
              "native-dialog-pointer-release-ambiguous",
              "native dialog acquisition failed after an ambiguous pointer release",
              { pointer_release: errorObservation(pointerReleaseError), dialog_acquisition: errorObservation(error) },
            );
          }
          throw error;
        }
        const candidate = acquired.value.candidate;
        state.candidate = candidate;
        const pointerProbe = await input.snapshotProbe(pointerStart);
        await sink.record("native-dialog-acquired", {
          input_kind: "browser_trusted_activation_then_windows_uia_window_close",
          pointer_target: target,
          pointer_probe_observation: pointerProbe,
          before,
          after: acquired.value.after,
          candidate,
        }, { phase: "executing", owner: OWNER });
        if (pointerReleaseError !== null) {
          throw new DesktopE2eError(
            "harness",
            "native-dialog-pointer-release-ambiguous",
            "the native dialog opened but browser pointer release delivery was ambiguous",
            { pointer_release: errorObservation(pointerReleaseError), candidate },
          );
        }
        const trustedPointer = assertTrustedProbeSequence(pointerProbe, {
          afterSequence: pointerStart,
          expected: [
            { type: "pointerdown", identity: PICKER.identity, button: 0, buttons: 1 },
            { type: "pointerup", identity: PICKER.identity, button: 0, buttons: 0 },
            { type: "click", identity: PICKER.identity, button: 0, buttons: 0 },
          ],
        });
        await sink.record("native-dialog-activation-validated", {
          pointer_probe: trustedPointer,
          candidate,
        }, { phase: "executing", owner: OWNER });
        const capture = await captureOwnedWindowPng({ ...state.nativeOwner, candidate });
        let dialogScreenshot = null;
        if (capture.available) {
          dialogScreenshot = await sink.writeBytes("screenshots/native-folder-dialog.png", capture.bytes);
        }
        await sink.record("native-dialog-capture", {
          candidate,
          screenshot: dialogScreenshot,
          screenshot_unavailable_reason: capture.available ? null : capture.reason,
        }, { phase: "executing", owner: OWNER });

        state.closeAdapterInvoked = true;
        let close;
        try {
          close = await closeOwnedNativeDialog({ ...state.nativeOwner, candidate });
          state.closeAttempted = close.attempted === true && close.attempt_count === 1;
          state.closeConfirmed = close.confirmed === true && close.call_returned === true;
        } catch (error) {
          const evidence = error?.evidence;
          state.closeAttempted = evidence?.attempted === true && evidence?.attempt_count === 1;
          state.closeAmbiguous = evidence?.may_have_dispatched === true;
          state.closeDispatchUnknown = evidence === null || evidence === undefined;
          state.closeFailure = errorObservation(error);
          throw error;
        }
        let closeEvidenceError = null;
        try {
          await sink.record("native-dialog-close-requested", { candidate, close }, { phase: "executing", owner: OWNER });
        } catch (error) {
          closeEvidenceError = error;
          state.executionEvidenceFailures.push(errorObservation(error));
        }
        let gone;
        try {
          gone = await waitForWindowDestroyed(state.nativeOwner, candidate);
        } catch (error) {
          throw classifyPostNativeCancelWindowFailure(error, candidate);
        }
        state.dialogSettled = true;
        state.candidate = null;
        let settled;
        try {
          settled = await waitForObservation({
            label: "folder picker cancellation projection",
            timeoutMs: 30_000,
            pollMs: 100,
            retrySampleErrors: false,
            sample: () => invokeDesktopCommand(cdp, "desktop_state"),
            accept: (projection) => projection?.status_message === "project creation cancelled"
              && projection?.overlay === "none"
              && projection?.navigation_loading === false
              && projection?.navigation_admission_open === true
              && revisionAfter(projection?.projection_revision, initial.projection_revision),
          });
        } catch (error) {
          throw classifyPostNativeCancelProjectionFailure(error);
        }
        const finalProjection = settled.value;
        const finalIdentity = selectedNavigationIdentity(finalProjection);
        if (!sameValue(finalIdentity, initialIdentity)) {
          throw productFailure("native-dialog-cancel-state-drift", "cancelling the project picker changed workspace/project/session identity", {
            initial_identity: initialIdentity,
            final_identity: finalIdentity,
          });
        }
        const afterScreenshot = await captureScenarioScreenshot({ cdp, sink, name: "native-dialog-cancelled", owner: OWNER });
        await sink.record("native-dialog-cancelled", {
          close_observation: { attempts: gone.attempts, elapsed_ms: gone.elapsed_ms, probe: gone.value },
          projection_revision: finalProjection.projection_revision,
          status_message: finalProjection.status_message,
          initial_identity: initialIdentity,
          final_identity: finalIdentity,
          identity_unchanged: true,
          screenshot: afterScreenshot,
        }, { phase: "executing", owner: OWNER });
        if (closeEvidenceError !== null) throw closeEvidenceError;
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        try {
          await input.cleanup();
        } catch (error) {
          state.inputCleanupFailure = errorObservation(error);
          if (primaryError === null) {
            throw new DesktopE2eError(
              "harness",
              "webview-input-cleanup-failed",
              "native dialog WebView input cleanup did not settle",
              state.inputCleanupFailure,
            );
          }
        }
      }
    },
    async requestGracefulExit(cdp) {
      return settleNativeDialogBeforeExit({ state, cdp });
    },
    async cleanup() {
      const resourceSettled = !state.actionMayHaveDispatched || state.dialogSettled;
      const pass = resourceSettled
        && state.inputCleanupFailure === null
        && state.cleanupFailures.length === 0
        && state.cleanupEvidenceFailures.length === 0
        && state.executionEvidenceFailures.length === 0;
      return {
        input: pass ? "pass" : "fail",
        resources: [{
          kind: "native-dialog",
          action_may_have_dispatched: state.actionMayHaveDispatched,
          close_adapter_invoked: state.closeAdapterInvoked,
          close_attempted: state.closeAttempted,
          close_confirmed: state.closeConfirmed,
          close_ambiguous: state.closeAmbiguous,
          close_dispatch_unknown: state.closeDispatchUnknown,
          close_failure: state.closeFailure,
          settled: resourceSettled,
          cleanup_fallback_used: state.fallback !== null,
          cleanup_fallback: state.fallback,
          input_cleanup_failure: state.inputCleanupFailure,
          cleanup_failures: state.cleanupFailures,
          cleanup_evidence_failures: state.cleanupEvidenceFailures,
          execution_evidence_failures: state.executionEvidenceFailures,
        }],
      };
    },
  });
}

import { DesktopE2eError } from "../core/execution.mjs";
import { TabFocusNavigator } from "../core/focus_navigation.mjs";
import { waitForObservation } from "../core/deadline.mjs";
import { WebviewInput, assertTrustedProbeSequence } from "../drivers/webview_input.mjs";
import {
  acquireInteractiveShell,
  cleanupShellBaseline,
  prepareShellBaseline,
  quiesceShellBaseline,
  requestGracefulExit,
} from "./shell_baseline.mjs";
import {
  captureScenarioScreenshot,
  invokeDesktopCommand,
  selectedNavigationIdentity,
  waitForDesktopProjection,
} from "./observations.mjs";

const OWNER = "scenario:input.pointer-keyboard";
const SHORTCUTS = Object.freeze({
  selector: 'button[data-action="show-shortcuts"]',
  identity: { tag: "BUTTON", action: "show-shortcuts" },
});
const CLOSE_SHORTCUTS_IDENTITY = Object.freeze({ tag: "BUTTON", action: "close-overlay" });
const REMEMBERED_DIALOG_KEY = "moyai.desktop_e2e.shortcuts-held-dialog.v1";

function sameValue(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

async function observeShortcutsDialog(cdp, { remember = false } = {}) {
  return cdp.evaluate(`(async () => {
    const identity = (element) => ({
      tag: element instanceof Element ? element.tagName.toUpperCase() : '',
      id: element instanceof Element && element.id ? element.id : null,
      action: element instanceof HTMLElement ? (element.dataset.action ?? null) : null,
      focusKey: element instanceof HTMLElement ? (element.dataset.focusKey ?? null) : null,
      configKey: element instanceof HTMLElement ? (element.dataset.configKey ?? null) : null,
      sideSetting: element instanceof HTMLElement ? (element.dataset.sideChatSetting ?? null) : null,
    });
    const visible = (element) => {
      if (!(element instanceof HTMLElement) || !element.isConnected) return false;
      const style = getComputedStyle(element);
      const rect = element.getBoundingClientRect();
      return style.display !== 'none' && style.visibility !== 'hidden' && Number(style.opacity) !== 0
        && rect.width > 0 && rect.height > 0;
    };
    const invoke = window.__TAURI_INTERNALS__?.invoke;
    const projection = typeof invoke === 'function' ? await invoke('desktop_state') : null;
    const dialogs = Array.from(document.querySelectorAll('[role="dialog"][aria-labelledby="shortcuts-dialog-title"]'));
    const dialog = dialogs.length === 1 ? dialogs[0] : null;
    const key = Symbol.for(${JSON.stringify(REMEMBERED_DIALOG_KEY)});
    if (${remember ? "true" : "false"} && dialog) globalThis[key] = dialog;
    return {
      projection_overlay: projection?.overlay ?? null,
      projection_revision: projection?.projection_revision ?? null,
      dialog_count: dialogs.length,
      dialog_connected: Boolean(dialog?.isConnected),
      dialog_visible: visible(dialog),
      same_dialog_node: dialog !== null && globalThis[key] === dialog,
      active: identity(document.activeElement),
      trigger_count: document.querySelectorAll('button[data-action="show-shortcuts"]').length,
      fatal_count: Array.from(document.querySelectorAll('.fatal')).filter(visible).length,
      recoverable_error_count: Array.from(document.querySelectorAll('.ui-error-notice')).filter(visible).length,
    };
  })()`);
}

function productFailure(code, message, evidence) {
  return new DesktopE2eError("product", code, message, evidence);
}

function errorDiagnostic(error) {
  return {
    owner: error instanceof DesktopE2eError ? error.owner : "harness",
    code: error?.code ?? "unclassified-error",
    message: error?.message ?? String(error),
    evidence: error?.evidence ?? null,
  };
}

export function shortcutsDialogReady(value) {
  return value?.projection_overlay === "shortcuts"
    && value?.dialog_count === 1
    && value?.dialog_connected === true
    && value?.dialog_visible === true
    && value?.active?.tag === "BUTTON"
    && value?.active?.action === "close-overlay"
    && value?.trigger_count === 1
    && value?.fatal_count === 0
    && value?.recoverable_error_count === 0;
}

async function waitForInteractiveShortcutsDialog(cdp) {
  try {
    return await waitForObservation({
      label: "shortcuts dialog DOM and focus readiness",
      timeoutMs: 10_000,
      pollMs: 50,
      sample: () => observeShortcutsDialog(cdp, { remember: true }),
      accept: shortcutsDialogReady,
      retrySampleErrors: false,
    });
  } catch (error) {
    if (error?.code !== "observation-timeout") throw error;
    if (error?.evidence?.last_error) {
      throw new DesktopE2eError(
        "harness",
        "shortcuts-dialog-observation-failed",
        "shortcuts dialog observation did not complete before its hard deadline",
        error.evidence,
      );
    }
    throw productFailure(
      "shortcuts-dialog-not-interactive",
      "trusted pointer opened an invalid shortcuts dialog",
      error.evidence,
    );
  }
}

export function assertTrustedTabNavigation(tabNavigation, tabProbe) {
  if (tabNavigation?.classification !== "acquired") {
    throw new DesktopE2eError(
      "harness",
      "trusted-tab-acquisition-failed",
      "trusted Tab event acquisition was invalid",
      { tab_navigation: tabNavigation, probe: tabProbe },
    );
  }
  if (tabNavigation.transition !== "in_document" || tabProbe?.active?.action !== "refresh") {
    throw productFailure(
      "trusted-tab-navigation-failed",
      "trusted Tab did not move focus from shortcuts to refresh",
      { tab_navigation: tabNavigation, probe: tabProbe },
    );
  }
  return tabNavigation;
}

async function clearRememberedShortcutsDialog(cdp) {
  const result = await cdp.evaluate(`(() => {
    const key = Symbol.for(${JSON.stringify(REMEMBERED_DIALOG_KEY)});
    const hadReference = Object.hasOwn(globalThis, key);
    const cleared = delete globalThis[key];
    return { had_reference: hadReference, cleared };
  })()`);
  if (result?.cleared !== true) {
    throw new DesktopE2eError(
      "harness",
      "remembered-dialog-cleanup-failed",
      "remembered shortcuts dialog reference could not be cleared",
      result,
    );
  }
  return result;
}

async function cleanupPointerKeyboardResources(input, cdp) {
  const resources = {
    owner: "webview-input",
    pass: false,
    input_cleanup: null,
    remembered_dialog: null,
    failures: [],
  };
  try { resources.input_cleanup = await input.cleanup(); }
  catch (error) { resources.failures.push(errorDiagnostic(error)); }
  try { resources.remembered_dialog = await clearRememberedShortcutsDialog(cdp); }
  catch (error) { resources.failures.push(errorDiagnostic(error)); }
  resources.pass = resources.failures.length === 0;
  if (!resources.pass) {
    throw new DesktopE2eError(
      "harness",
      "pointer-keyboard-resource-cleanup-failed",
      "pointer/keyboard scenario resources did not settle exactly",
      { resources },
    );
  }
  return resources;
}

export class PointerKeyboardCleanupOwner {
  #outcome = { input: "pass", resources: [] };
  #settled = false;

  get outcome() {
    return structuredClone(this.#outcome);
  }

  async settle(input, cdp, primaryError = null) {
    if (this.#settled) throw new TypeError("pointer/keyboard resources were already settled");
    this.#settled = true;
    try {
      const resources = await cleanupPointerKeyboardResources(input, cdp);
      this.#outcome = { input: "pass", resources: [resources] };
    } catch (error) {
      this.#outcome = {
        input: "fail",
        resources: [{
          owner: "webview-input",
          pass: false,
          failure: errorDiagnostic(error),
          primary_failure: primaryError === null ? null : errorDiagnostic(primaryError),
        }],
      };
      if (primaryError === null) throw error;
    }
    return this.outcome;
  }
}

export function createPointerKeyboardScenario() {
  const cleanupOwner = new PointerKeyboardCleanupOwner();
  return Object.freeze({
    id: "input.pointer-keyboard",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    prepare: prepareShellBaseline,
    requestGracefulExit,
    quiesce: quiesceShellBaseline,
    async cleanup() {
      const baseline = await cleanupShellBaseline();
      const input = cleanupOwner.outcome;
      return {
        input: baseline.input === "pass" && input.input === "pass" ? "pass" : "fail",
        resources: [...(baseline.resources ?? []), ...input.resources],
      };
    },
    async execute({ context, driver: cdp, sink }) {
      await acquireInteractiveShell({ context, driver: cdp, sink }, {
        evidenceOwner: OWNER,
        screenshotStem: "input-shell-ready",
      });
      const initial = await invokeDesktopCommand(cdp, "desktop_state");
      const initialIdentity = selectedNavigationIdentity(initial);
      const input = new WebviewInput(cdp, { probeId: "pointer-keyboard" });
      let primaryError = null;
      try {
        await input.installProbe();
        const pointerStart = (await input.snapshotProbe()).sequence;
        const pointerTarget = await input.click(SHORTCUTS);
        const pointerProbe = await input.snapshotProbe(pointerStart);
        const trustedPointer = assertTrustedProbeSequence(pointerProbe, {
          afterSequence: pointerStart,
          expected: [
            { type: "pointerdown", identity: SHORTCUTS.identity, button: 0, buttons: 1 },
            { type: "pointerup", identity: SHORTCUTS.identity, button: 0, buttons: 0 },
            { type: "click", identity: SHORTCUTS.identity, button: 0, buttons: 0 },
          ],
        });
        const opened = await waitForDesktopProjection({
          cdp,
          label: "shortcuts overlay opened from trusted pointer",
          accept: (projection) => projection?.overlay === "shortcuts",
        });
        const dialogReadiness = await waitForInteractiveShortcutsDialog(cdp);
        const dialog = dialogReadiness.value;
        await sink.record("trusted-pointer-acquired", {
          input_kind: "browser_trusted",
          target: pointerTarget,
          probe: trustedPointer,
          projection_revision: opened.value.projection_revision,
          dialog_readiness: { attempts: dialogReadiness.attempts, elapsed_ms: dialogReadiness.elapsed_ms },
          dialog,
        }, { phase: "executing", owner: OWNER });

        const escapeStart = pointerProbe.sequence;
        await input.keyDown("Escape");
        const held = await waitForDesktopProjection({
          cdp,
          label: "held Escape projection settlement",
          accept: (projection) => projection?.overlay === "none",
        });
        const heldDialog = await observeShortcutsDialog(cdp);
        const downProbe = await input.snapshotProbe(escapeStart);
        const trustedEscapeDown = assertTrustedProbeSequence(downProbe, {
          afterSequence: escapeStart,
          expected: [{ type: "keydown", key: "Escape", code: "Escape", identity: CLOSE_SHORTCUTS_IDENTITY }],
        });
        if (
          heldDialog.dialog_count !== 1
          || heldDialog.dialog_connected !== true
          || heldDialog.dialog_visible !== true
          || heldDialog.same_dialog_node !== true
          || heldDialog.active?.action !== "close-overlay"
        ) {
          throw productFailure("held-key-dom-replaced", "the active dialog changed before Escape keyup", { held: held.value, held_dialog: heldDialog });
        }
        const heldScreenshot = await captureScenarioScreenshot({ cdp, sink, name: "input-escape-held", owner: OWNER });
        await input.keyUp("Escape");
        const released = await waitForDesktopProjection({
          cdp,
          label: "Escape keyup overlay release",
          accept: (projection) => projection?.overlay === "none",
        });
        let releasedDialog = null;
        for (let attempt = 0; attempt < 50; attempt += 1) {
          releasedDialog = await observeShortcutsDialog(cdp);
          if (
            releasedDialog.dialog_count === 0
            && releasedDialog.active?.action === "show-shortcuts"
          ) break;
          await new Promise((resolve) => setTimeout(resolve, 50));
        }
        if (
          releasedDialog?.dialog_count !== 0
          || releasedDialog?.active?.action !== "show-shortcuts"
          || releasedDialog?.fatal_count !== 0
          || releasedDialog?.recoverable_error_count !== 0
        ) {
          throw productFailure("shortcuts-dialog-release-failed", "Escape keyup did not restore the shortcuts trigger", { released: released.value, dialog: releasedDialog });
        }
        const upProbe = await input.snapshotProbe(downProbe.sequence);
        const trustedEscapeUp = assertTrustedProbeSequence(upProbe, {
          afterSequence: downProbe.sequence,
          expected: [{ type: "keyup", key: "Escape", code: "Escape", identity: CLOSE_SHORTCUTS_IDENTITY }],
        });

        const tabStart = upProbe.sequence;
        const beforeTab = upProbe.active;
        await input.pressKey("Tab");
        const tabProbe = await input.snapshotProbe(tabStart);
        const tabNavigation = new TabFocusNavigator().observe({
          beforeSequence: tabStart,
          beforeActive: beforeTab,
          afterActive: tabProbe.active,
          events: tabProbe.events,
          dispatchError: null,
        });
        assertTrustedTabNavigation(tabNavigation, tabProbe);

        const finalProjection = await invokeDesktopCommand(cdp, "desktop_state");
        const finalIdentity = selectedNavigationIdentity(finalProjection);
        if (
          finalProjection.overlay !== "none"
          || finalProjection.draft_prompt !== initial.draft_prompt
          || !sameValue(finalIdentity, initialIdentity)
        ) {
          throw productFailure("input-roundtrip-state-drift", "pointer/keyboard roundtrip changed unrelated Desktop state", {
            initial_identity: initialIdentity,
            final_identity: finalIdentity,
            initial_draft: initial.draft_prompt,
            final_draft: finalProjection.draft_prompt,
            final_overlay: finalProjection.overlay,
          });
        }
        const finalScreenshot = await captureScenarioScreenshot({ cdp, sink, name: "input-roundtrip-complete", owner: OWNER });
        await sink.record("trusted-keyboard-acquired", {
          input_kind: "browser_trusted",
          escape_down: trustedEscapeDown,
          escape_up: trustedEscapeUp,
          held_projection_revision: held.value.projection_revision,
          held_dialog: heldDialog,
          tab_navigation: tabNavigation,
          held_screenshot: heldScreenshot,
          final_screenshot: finalScreenshot,
        }, { phase: "executing", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        await cleanupOwner.settle(input, cdp, primaryError);
      }
    },
  });
}

import { DesktopE2eError } from "../core/execution.mjs";
import { TabFocusNavigator } from "../core/focus_navigation.mjs";
import { waitForObservation } from "../core/deadline.mjs";
import {
  WebviewInput,
  assertTrustedProbeSequence,
  assertTrustedTextInsertion,
} from "../drivers/webview_input.mjs";
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
const COMPOSER_TEXT = "Unicode入力 😀\n複数行のactual GUI確認";
const COMPOSER = Object.freeze({
  selector: "section.composer textarea#prompt",
  identity: { tag: "TEXTAREA", id: "prompt" },
});
const SHORTCUTS = Object.freeze({
  selector: 'button[data-action="show-shortcuts"]',
  identity: { tag: "BUTTON", action: "show-shortcuts" },
});
const CLOSE_SHORTCUTS_IDENTITY = Object.freeze({ tag: "BUTTON", action: "close-overlay" });
const REMEMBERED_DIALOG_KEY = "moyai.desktop_e2e.shortcuts-held-dialog.v1";

function sameValue(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

async function observeComposer(cdp) {
  return cdp.evaluate(`(async () => {
    const invoke = window.__TAURI_INTERNALS__?.invoke;
    if (typeof invoke !== 'function') throw new Error('tauri-invoke-unavailable');
    const projection = await invoke('desktop_state');
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    const visible = (element) => {
      if (!(element instanceof HTMLElement) || !element.isConnected) return false;
      const style = getComputedStyle(element);
      const rect = element.getBoundingClientRect();
      return style.display !== 'none' && style.visibility !== 'hidden' && Number(style.opacity) !== 0
        && rect.width > 0 && rect.height > 0;
    };
    const nodes = Array.from(document.querySelectorAll('section.composer textarea#prompt'));
    const prompt = nodes.length === 1 ? nodes[0] : null;
    return {
      projection,
      prompt: {
        count: nodes.length,
        value: prompt instanceof HTMLTextAreaElement ? prompt.value : null,
        active: prompt !== null && document.activeElement === prompt,
        visible: visible(prompt),
        enabled: prompt instanceof HTMLTextAreaElement
          && !prompt.disabled
          && prompt.getAttribute('aria-disabled') !== 'true'
          && prompt.closest('[inert]') === null,
        selection_start: prompt instanceof HTMLTextAreaElement ? prompt.selectionStart : null,
        selection_end: prompt instanceof HTMLTextAreaElement ? prompt.selectionEnd : null,
      },
      fatal_count: Array.from(document.querySelectorAll('.fatal')).filter(visible).length,
      recoverable_error_count: Array.from(document.querySelectorAll('.ui-error-notice')).filter(visible).length,
    };
  })()`);
}

export function composerQualificationFailures(surface, expected) {
  const failures = [];
  if (surface?.prompt?.count !== 1) failures.push("composer-cardinality");
  if (surface?.prompt?.visible !== true) failures.push("composer-not-visible");
  if (surface?.prompt?.enabled !== true) failures.push("composer-not-enabled");
  if (surface?.prompt?.active !== expected.active) failures.push("composer-focus-drift");
  if (surface?.prompt?.value !== expected.domValue) failures.push("composer-dom-value-drift");
  if (surface?.prompt?.selection_start !== expected.selectionOffset
    || surface?.prompt?.selection_end !== expected.selectionOffset) {
    failures.push("composer-selection-drift");
  }
  if (surface?.projection?.overlay !== "none") failures.push("composer-overlay-drift");
  if (surface?.projection?.draft_prompt !== "") failures.push("composer-projected-draft-drift");
  if (!sameValue(surface?.projection?.draft_target, expected.draftTarget)) failures.push("composer-draft-target-drift");
  if (!sameValue(surface?.projection?.run_target, expected.runTarget)) failures.push("composer-run-target-drift");
  if (surface?.projection?.composer_commit_generation !== expected.composerCommitGeneration) {
    failures.push("composer-commit-generation-drift");
  }
  if (!sameValue(selectedNavigationIdentity(surface?.projection), expected.navigationIdentity)) {
    failures.push("composer-navigation-drift");
  }
  if (surface?.fatal_count !== 0) failures.push("composer-fatal-visible");
  if (surface?.recoverable_error_count !== 0) failures.push("composer-recoverable-error-visible");
  return failures;
}

async function waitForComposerQualification(cdp, { label, expected, code, message }) {
  try {
    return await waitForObservation({
      label,
      timeoutMs: 10_000,
      pollMs: 50,
      sample: () => observeComposer(cdp),
      accept: (surface) => composerQualificationFailures(surface, expected).length === 0,
      retrySampleErrors: false,
    });
  } catch (error) {
    if (error?.code !== "observation-timeout" || error?.evidence?.last_error) throw error;
    throw productFailure(code, message, {
      ...error.evidence,
      failures: composerQualificationFailures(error?.evidence?.last_value, expected),
      expected,
    });
  }
}

async function trustedClick(input, locator) {
  const start = (await input.snapshotProbe()).sequence;
  const target = await input.click(locator);
  const snapshot = await input.snapshotProbe(start);
  return {
    target,
    probe: assertTrustedProbeSequence(snapshot, {
      afterSequence: start,
      expected: [
        { type: "pointerdown", identity: locator.identity, button: 0, buttons: 1 },
        { type: "pointerup", identity: locator.identity, button: 0, buttons: 0 },
        { type: "click", identity: locator.identity, button: 0, buttons: 0 },
      ],
    }),
  };
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
      const initialSurface = await observeComposer(cdp);
      const initial = initialSurface.projection;
      const initialIdentity = selectedNavigationIdentity(initial);
      const composerOwner = {
        draftTarget: initial.draft_target,
        runTarget: initial.run_target,
        composerCommitGeneration: initial.composer_commit_generation,
        navigationIdentity: initialIdentity,
      };
      const freshComposerExpected = {
        ...composerOwner,
        active: initialSurface?.prompt?.active === true,
        domValue: "",
        selectionOffset: 0,
      };
      const freshComposerFailures = composerQualificationFailures(initialSurface, freshComposerExpected);
      if (freshComposerFailures.length > 0) {
        throw productFailure(
          "composer-not-fresh",
          "the common input qualification did not start from an empty, interactive composer",
          { failures: freshComposerFailures, surface: initialSurface },
        );
      }
      const input = new WebviewInput(cdp, { probeId: "pointer-keyboard" });
      let primaryError = null;
      try {
        await input.installProbe();

        const composerClick = await trustedClick(input, COMPOSER);
        const insertStart = (await input.snapshotProbe()).sequence;
        const insertion = await input.insertText(COMPOSER, COMPOSER_TEXT);
        const insertSnapshot = await input.snapshotProbe(insertStart);
        const trustedInsertion = assertTrustedTextInsertion(insertSnapshot, {
          afterSequence: insertStart,
          identity: COMPOSER.identity,
          text: COMPOSER_TEXT,
        });
        const insertedComposer = await waitForComposerQualification(cdp, {
          label: "Unicode multiline composer insertion",
          expected: {
            ...composerOwner,
            active: true,
            domValue: COMPOSER_TEXT,
            selectionOffset: COMPOSER_TEXT.length,
          },
          code: "composer-insertion-state-drift",
          message: "trusted Input.insertText did not produce the exact local draft while preserving the Rust owner projection",
        });
        await sink.record("trusted-composer-insert-text-acquired", {
          input_kind: "browser_trusted",
          composer_click: composerClick,
          insertion,
          probe: trustedInsertion,
          dom_value: insertedComposer.value.prompt.value,
          projected_draft: insertedComposer.value.projection.draft_prompt,
          draft_target: insertedComposer.value.projection.draft_target,
          run_target: insertedComposer.value.projection.run_target,
        }, { phase: "executing", owner: OWNER });

        const restoreStart = insertSnapshot.sequence;
        await input.keyDown("Control");
        try {
          await input.pressKey("a");
        } finally {
          await input.keyUp("Control");
        }
        await input.pressKey("Backspace");
        const restoreSnapshot = await input.snapshotProbe(restoreStart);
        const trustedRestore = assertTrustedProbeSequence(restoreSnapshot, {
          afterSequence: restoreStart,
          expected: [
            { type: "keydown", identity: COMPOSER.identity, key: "Control", code: "ControlLeft" },
            { type: "keydown", identity: COMPOSER.identity, key: "a", code: "KeyA" },
            { type: "keyup", identity: COMPOSER.identity, key: "a", code: "KeyA" },
            { type: "keyup", identity: COMPOSER.identity, key: "Control", code: "ControlLeft" },
            { type: "keydown", identity: COMPOSER.identity, key: "Backspace", code: "Backspace" },
            { type: "input", identity: COMPOSER.identity, inputType: "deleteContentBackward", data: null },
            { type: "keyup", identity: COMPOSER.identity, key: "Backspace", code: "Backspace" },
          ],
        });
        const restoredComposer = await waitForComposerQualification(cdp, {
          label: "trusted composer draft restoration",
          expected: {
            ...composerOwner,
            active: true,
            domValue: "",
            selectionOffset: 0,
          },
          code: "composer-restoration-state-drift",
          message: "trusted Ctrl+A and Backspace did not restore the exact fresh composer state",
        });
        await sink.record("trusted-composer-draft-restored", {
          input_kind: "browser_trusted",
          probe: trustedRestore,
          dom_value: restoredComposer.value.prompt.value,
          projected_draft: restoredComposer.value.projection.draft_prompt,
          draft_target: restoredComposer.value.projection.draft_target,
          run_target: restoredComposer.value.projection.run_target,
        }, { phase: "executing", owner: OWNER });

        const pointerStart = restoreSnapshot.sequence;
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

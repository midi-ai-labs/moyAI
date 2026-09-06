import { isDeepStrictEqual } from "node:util";
import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { WebviewInput, assertTrustedProbeSequence, assertTrustedTextInsertion } from "../drivers/webview_input.mjs";
import { startScriptedProvider } from "../drivers/scripted_provider.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { providerRestartFixtureConfig, quiesceProviderResource } from "./provider_restart.mjs";
import { exactHeldRunStopLedger, exactStoppedRunStopLedger, exactTurnStopTarget, RUN_STOP_PROMPT } from "./run_stop.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";

const OWNER = "scenario:shell.lynx";
const NODE_KEY = "moyai.desktop_e2e.lynx.connected-node.v1";
export const LYNX_LONG_PROMPT = Array.from({ length: 24 }, (_, index) => `${index + 1}. 日本語と English の長い依頼を編集します。入力と選択を保持してください。`).join("\n");
const locator = (selector, identity) => Object.freeze({ selector, identity });
const action = (scope, name) => locator(`${scope} button[data-action="${name}"]`, { tag: "BUTTON", action: name });
const PROMPT = locator("section.composer textarea#prompt", { tag: "TEXTAREA", id: "prompt" });
const REFRESH = action("aside.sidebar", "refresh");
const SETTINGS = action("aside.sidebar", "show-config");
const DIALOG = '[role="dialog"][aria-labelledby="config-dialog-title"]';
const MODEL_SETTINGS = locator(`${DIALOG} a[href="#settings-model"]`, { tag: "A", href: "#settings-model" });
const CONTEXT = locator(`${DIALOG} input[data-config-key="model.context_window"]`, { tag: "INPUT", configKey: "model.context_window" });
const DRAWER_CLOSE = action(".artifact-pane:not(.collapsed)", "toggle-artifact-pane");
const DRAWER_OPEN = action("header.topbar", "toggle-artifact-pane");

async function observeSurface(cdp) {
  return cdp.evaluate(`(async () => {
    const projection = await window.__TAURI_INTERNALS__.invoke('desktop_state');
    const one = (selector) => {
      const nodes = document.querySelectorAll(selector);
      return nodes.length === 1 ? nodes[0] : null;
    };
    const measure = (element) => {
      if (!(element instanceof HTMLElement)) return null;
      const rect = element.getBoundingClientRect();
      const style = getComputedStyle(element);
      const x = rect.left + rect.width / 2, y = rect.top + rect.height / 2;
      const hit = document.elementFromPoint(x, y);
      return {
        left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom,
        width: rect.width, height: rect.height,
        visible: element.isConnected && style.display !== 'none' && style.visibility !== 'hidden'
          && Number(style.opacity) !== 0 && rect.width > 0 && rect.height > 0,
        center_hit: hit !== null && (hit === element || element.contains(hit)),
        row: style.gridRowStart, padding_bottom: Number.parseFloat(style.paddingBottom),
      };
    };
    const prompt = one('#prompt'), thread = one('#thread'), composer = one('.composer');
    const send = one('.composer button[data-action="send"]');
    const runTargetText = composer?.getAttribute('data-run-target') ?? null;
    let renderedRunTarget = null, runTargetParseError = null;
    if (runTargetText !== null) {
      try { renderedRunTarget = JSON.parse(runTargetText); }
      catch (error) { runTargetParseError = String(error); }
    }
    const field = one('${CONTEXT.selector}');
    const remembered = globalThis[Symbol.for('${NODE_KEY}')];
    const dialog = one('${DIALOG}');
    return {
      projection,
      viewport: { width: innerWidth, height: innerHeight },
      active_element: { tag: document.activeElement?.tagName ?? null,
        id: document.activeElement?.id ?? null, action: document.activeElement?.getAttribute('data-action') ?? null },
      rendered_run_target: renderedRunTarget, run_target_parse_error: runTargetParseError,
      conversation: measure(one('.conversation')), topbar: measure(one('.topbar')),
      thread: measure(thread), composer: measure(composer), run_strip: measure(one('.run-strip')),
      hero: measure(one('.empty-thread h2')),
      prompt: { ...measure(prompt), value: prompt?.value ?? null, active: document.activeElement === prompt,
        disabled: prompt?.disabled ?? null,
        selection_start: prompt?.selectionStart ?? null, selection_end: prompt?.selectionEnd ?? null,
        same_node: remembered?.node === prompt, scroll_height: prompt?.scrollHeight ?? null,
        client_height: prompt?.clientHeight ?? null },
      send: { ...measure(send), text: send?.textContent.trim() ?? '', enabled: send !== null && !send.disabled },
      stop: measure(one('.run-strip button[data-action="cancel-run"]')),
      drawer: { ...measure(one('.artifact-pane')), collapsed: document.querySelector('.app-frame')?.classList.contains('artifact-collapsed') ?? null },
      settings: { ...measure(dialog), focused_inside: dialog !== null && dialog.contains(document.activeElement),
        field_value: field?.value ?? null, field_active: field !== null && document.activeElement === field,
        same_node: remembered?.node === field,
        dirty: one('.settings-modal .dirty-badge.visible') !== null,
        close_guard: one('[role="alertdialog"][aria-labelledby="settings-close-confirm-title"]') !== null },
      errors: Array.from(document.querySelectorAll('.fatal, .ui-error-notice')).filter((node) => measure(node)?.visible).length,
    };
  })()`);
}

function fits(rect, width, height) {
  return rect?.visible === true && rect.left >= -1 && rect.top >= -1
    && rect.right <= width + 1 && rect.bottom <= height + 1;
}

/** Geometry is relative to the live viewport and measured composer, not screenshot pixels. */
export function lynxLayoutFailures(surface, { running = false, empty = false } = {}) {
  const failures = [];
  const { viewport, conversation, topbar, thread, composer, run_strip: strip } = surface ?? {};
  if (!viewport || !conversation || !topbar || !thread || !composer) return ["missing-layout-owner"];
  if ([conversation, topbar, thread, composer].some((rect) =>
    [rect.left, rect.top, rect.right, rect.bottom, rect.width, rect.height].some((value) => !Number.isFinite(value)))) return ["invalid-layout-measurement"];
  if (thread.row !== "3") failures.push("thread-not-in-stretch-row");
  if (Math.abs(thread.bottom - conversation.bottom) > 2 || thread.height < conversation.height * 0.5) failures.push("thread-viewport-collapsed");
  const preceding = running ? strip : topbar;
  if (!preceding || Math.abs(thread.top - preceding.bottom) > 2) failures.push("thread-detached-from-header");
  if (running && (strip?.row !== "2" || surface.stop?.center_hit !== true)) failures.push("running-stop-occluded");
  if (!running && strip !== null) failures.push("unexpected-running-strip");
  if (!fits(composer, viewport.width, viewport.height)) failures.push("composer-outside-viewport");
  if (thread.padding_bottom < composer.height + 16) failures.push("composer-reserve-too-small");
  if (surface.prompt?.center_hit !== true || surface.send?.center_hit !== true) failures.push("composer-controls-occluded");
  if (!surface.send?.text?.includes("送信")) failures.push("send-has-no-visible-label");
  if (empty && (!surface.hero?.visible || surface.hero.bottom >= composer.top - 8
    || surface.hero.top < thread.top + 24)) failures.push("empty-state-placement");
  if (surface.errors !== 0) failures.push("visible-error");
  return failures;
}

export function lynxDraftFailures(surface, { value, active = true, sameNode = true, selection = value.length } = {}) {
  const failures = [];
  if (surface?.prompt?.value !== value) failures.push("prompt-value-changed");
  if (surface?.prompt?.active !== active) failures.push("prompt-focus-changed");
  if (sameNode && surface?.prompt?.same_node !== true) failures.push("prompt-node-replaced");
  if (surface?.prompt?.selection_start !== selection || surface?.prompt?.selection_end !== selection) failures.push("prompt-selection-changed");
  if (surface?.projection?.overlay !== "none" || surface?.errors !== 0) failures.push("unexpected-overlay-or-error");
  return failures;
}

export function lynxSettingsDraftReady(surface, value, { sameNode = true } = {}) {
  return surface?.projection?.overlay === "config" && surface.settings?.visible === true
    && surface.settings.field_value === value && surface.settings.field_active === true
    && (!sameNode || surface.settings.same_node === true)
    && surface.settings.dirty === true && surface.settings.close_guard === false && surface.errors === 0;
}

/** A fresh native Running state can precede the frontend's first-session admission settlement. */
export function lynxRunningComposerReady(surface) {
  const projection = surface?.projection;
  const target = projection?.run_target;
  return projection?.task_activity_state === "running" && projection.async_polling_required === true
    && projection.overlay === "none" && exactTurnStopTarget(projection.stop_target)
    && typeof target?.sessionId === "string" && target.sessionId.length > 0
    && projection.draft_target?.sessionId === target.sessionId
    && projection.draft_target?.workspacePath === target.workspacePath
    && target.expectedState?.kind === "turn"
    && target.expectedState.turnId === projection.stop_target.turnId
    && target.expectedState.admissionRevision === projection.stop_target.admissionRevision
    && surface.run_target_parse_error === null
    && isDeepStrictEqual(surface.rendered_run_target, target)
    && projection.draft_prompt === "" && surface.prompt?.value === ""
    && surface.prompt.disabled === false && surface.send?.enabled === false && surface.errors === 0;
}

function productFailure(label, evidence) {
  return new DesktopE2eError("product", "lynx-usability-mismatch", label, evidence);
}

async function waitForSurface(cdp, label, accept, timeoutMs = 10_000) {
  try {
    return (await waitForObservation({ label, timeoutMs, pollMs: 60,
      sample: () => observeSurface(cdp), accept, retrySampleErrors: false })).value;
  } catch (error) {
    if (error?.code === "observation-timeout" && !error.evidence?.last_error) throw productFailure(label, error.evidence);
    throw error;
  }
}

async function recordLayout(cdp, sink, label, options) {
  const surface = await waitForSurface(cdp, label, (value) => lynxLayoutFailures(value, options).length === 0);
  await sink.record(label, surface, { phase: "executing", owner: OWNER });
  await captureScenarioScreenshot({ cdp, sink, name: label, owner: OWNER });
  return surface;
}

async function click(input, target, sink) {
  const start = (await input.snapshotProbe()).sequence;
  const acquired = await input.click(target);
  const probe = assertTrustedProbeSequence(await input.snapshotProbe(start), {
    afterSequence: start,
    expected: [
      { type: "pointerdown", identity: target.identity, button: 0, buttons: 1 },
      { type: "pointerup", identity: target.identity, button: 0, buttons: 0 },
      { type: "click", identity: target.identity, button: 0, buttons: 0 },
    ],
  });
  await sink.record("lynx-trusted-click", { acquired, probe }, { phase: "executing", owner: OWNER });
}

export function lynxReplacementKeyEvents(identity) {
  return [
    { type: "keydown", identity, key: "Control", code: "ControlLeft" },
    { type: "keydown", identity, key: "a", code: "KeyA" },
    { type: "keyup", identity, key: "a", code: "KeyA" },
    { type: "keyup", identity, key: "Control", code: "ControlLeft" },
    { type: "keydown", identity, key: "Backspace", code: "Backspace" },
    { type: "keyup", identity, key: "Backspace", code: "Backspace" },
  ];
}

export function lynxEditorAcquisitionReady(surface, editor) {
  if (editor === "prompt") {
    return surface?.projection?.overlay === "none" && surface.prompt?.active === true
      && surface.prompt.same_node === true && surface.prompt.disabled === false
      && surface.run_target_parse_error === null && surface.rendered_run_target !== null
      && isDeepStrictEqual(surface.rendered_run_target, surface.projection.run_target) && surface.errors === 0;
  }
  return editor === "settings" && surface?.projection?.overlay === "config"
    && surface.settings?.field_active === true && surface.settings.same_node === true
    && surface.settings.close_guard === false && surface.errors === 0;
}

async function acquireFocusedEditor(cdp, target, sink) {
  const editor = target.identity.id === "prompt" ? "prompt" : "settings";
  await rememberNode(cdp, target.selector);
  let previousOwner = null;
  let consecutive = 0;
  const acquired = await waitForObservation({
    label: "LYNX focused editor after pointer settlement", timeoutMs: 1500, pollMs: 60, retrySampleErrors: false,
    sample: async () => {
      // Match the common Settings/run-next-turn observation boundary: let the pointer-release
      // render and its scheduled focus settlement finish before sending destructive keys.
      await cdp.evaluate("new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)))");
      return observeSurface(cdp);
    },
    accept: (surface) => {
      const owner = editor === "prompt" ? surface.projection?.run_target : surface.projection?.config_target;
      if (!owner || !lynxEditorAcquisitionReady(surface, editor)) {
        previousOwner = null;
        consecutive = 0;
        return false;
      }
      consecutive = isDeepStrictEqual(owner, previousOwner) ? consecutive + 1 : 1;
      previousOwner = structuredClone(owner);
      return consecutive >= 2;
    },
  });
  await sink.record("lynx-focused-editor-acquired", {
    target, owner: previousOwner, consecutive_samples: consecutive,
    active_element: acquired.value.active_element, elapsed_ms: acquired.elapsed_ms,
  }, { phase: "executing", owner: OWNER });
}

async function replaceText(cdp, input, target, value, sink) {
  await click(input, target, sink);
  await acquireFocusedEditor(cdp, target, sink);
  const start = (await input.snapshotProbe()).sequence;
  await input.keyDown("Control");
  try { await input.pressKey("a"); } finally { await input.keyUp("Control"); }
  await input.pressKey("Backspace");
  const selection = assertTrustedProbeSequence(await input.snapshotProbe(start), {
    afterSequence: start,
    expected: lynxReplacementKeyEvents(target.identity),
  });
  let insertion = null;
  if (value) {
    const insertStart = (await input.snapshotProbe()).sequence;
    await input.insertText(target, value);
    insertion = assertTrustedTextInsertion(await input.snapshotProbe(insertStart), {
      afterSequence: insertStart, identity: target.identity, text: value,
    });
  }
  await sink.record("lynx-trusted-edit", { target, selection, insertion }, { phase: "executing", owner: OWNER });
}

async function rememberNode(cdp, selector) {
  await cdp.evaluate(`(() => {
    const nodes = document.querySelectorAll(${JSON.stringify(selector)});
    if (nodes.length !== 1) throw new Error('lynx-remember-node-cardinality');
    globalThis[Symbol.for('${NODE_KEY}')] = { node: nodes[0] };
  })()`);
}

async function assertStable(cdp, sink, label, failures) {
  const started = Date.now();
  const observation = await waitForObservation({
    label, timeoutMs: 5000, pollMs: 120, retrySampleErrors: false,
    sample: () => observeSurface(cdp),
    accept: (surface) => {
      const mismatch = failures(surface);
      if (mismatch.length) throw productFailure(label, { failures: mismatch, surface });
      return Date.now() - started >= 1800;
    },
  });
  await sink.record(label, observation, { phase: "executing", owner: OWNER });
}

export function createShellLynxScenario() {
  const state = { provider: null, acceptedLedger: null, quiesce: null, cleanupFailures: [] };
  return Object.freeze({
    id: "shell.lynx", productOracle: "pass", manualGate: "not_required", databaseRequired: true,
    requestGracefulExit,
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({ expectedPrompt: RUN_STOP_PROMPT, responseBehavior: "hold_until_peer_close" });
      await prepareDesktopFixture({ context, sink, phase, owner: OWNER,
        configText: providerRestartFixtureConfig(state.provider.baseUrl),
        sentinelName: "E2E_SHELL_LYNX.txt", sentinelText: "LYNX visual usability fixture.\n" });
      await sink.record("lynx-provider-started", state.provider.resourceObservation(), { phase, owner: OWNER });
    },
    async execute({ context, driver: cdp, sink }) {
      await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: OWNER, screenshotStem: "lynx-shell-ready" });
      const input = new WebviewInput(cdp, { probeId: "shell-lynx" });
      let metricsOverridden = false;
      let primaryError = null;
      try {
        await input.installProbe();
        await recordLayout(cdp, sink, "lynx-idle-layout", { empty: true });
        await replaceText(cdp, input, PROMPT, LYNX_LONG_PROMPT, sink);
        await rememberNode(cdp, PROMPT.selector);
        await click(input, REFRESH, sink);
        await waitForSurface(cdp, "refresh returns focus to edited prompt", (value) => lynxDraftFailures(value, { value: LYNX_LONG_PROMPT }).length === 0);
        await assertStable(cdp, sink, "lynx-long-prompt-stable", (value) => lynxDraftFailures(value, { value: LYNX_LONG_PROMPT }));
        await recordLayout(cdp, sink, "lynx-long-prompt-layout", {});

        await click(input, SETTINGS, sink);
        await waitForSurface(cdp, "settings dialog opened", (value) => value.projection.overlay === "config" && value.settings.focused_inside);
        await click(input, MODEL_SETTINGS, sink);
        const settings = await waitForSurface(cdp, "model context focused", (value) => value.settings.field_active);
        const originalContext = settings.settings.field_value;
        const editedContext = String(Number(originalContext) + 1);
        await replaceText(cdp, input, CONTEXT, editedContext, sink);
        await rememberNode(cdp, CONTEXT.selector);
        // Exercise an ordinary projection refresh while the dialog's current input owns focus.
        await invokeDesktopCommand(cdp, "refresh_desktop");
        await assertStable(cdp, sink, "lynx-settings-draft-stable", (value) => lynxSettingsDraftReady(value, editedContext) ? [] : ["settings-draft-or-focus-changed"]);
        await captureScenarioScreenshot({ cdp, sink, name: "lynx-settings-draft", owner: OWNER });
        await click(input, action(DIALOG, "close-overlay"), sink);
        await waitForSurface(cdp, "dirty settings close is guarded", (value) => value.settings.close_guard);
        await click(input, action('[role="alertdialog"][aria-labelledby="settings-close-confirm-title"]', "cancel-local-confirm"), sink);
        await waitForSurface(cdp, "cancel close retains settings draft", (value) => value.projection.overlay === "config" && !value.settings.close_guard && value.settings.field_value === editedContext && value.settings.dirty);
        await click(input, action(DIALOG, "discard-config-draft"), sink);
        await waitForSurface(cdp, "explicit discard restores baseline", (value) => value.settings.field_value === originalContext && !value.settings.dirty);
        await click(input, action(DIALOG, "close-overlay"), sink);
        await waitForSurface(cdp, "settings close retains composer draft", (value) => value.projection.overlay === "none" && value.prompt.value === LYNX_LONG_PROMPT);

        await replaceText(cdp, input, PROMPT, "", sink);
        // Official CDP viewport emulation; this does not claim a native HWND resize.
        // https://chromedevtools.github.io/devtools-protocol/tot/Emulation/#method-setDeviceMetricsOverride
        metricsOverridden = true;
        try {
          await cdp.call("Emulation.setDeviceMetricsOverride", { width: 1100, height: 720, deviceScaleFactor: 1, mobile: false });
        } catch (error) {
          throw new DesktopE2eError("environment", "lynx-viewport-emulation-unavailable", "The attached WebView could not apply the requested CDP viewport", { message: error?.message ?? String(error) });
        }
        await waitForSurface(cdp, "1100 by 720 WebView viewport", (value) => value.viewport.width === 1100 && value.viewport.height === 720);
        await sink.record("lynx-viewport", { kind: "cdp_emulated_webview", width: 1100, height: 720, native_window_resize: "not_tested" }, { phase: "executing", owner: OWNER });
        let narrow = await observeSurface(cdp);
        if (narrow.drawer.collapsed) await click(input, DRAWER_OPEN, sink);
        await recordLayout(cdp, sink, "lynx-narrow-drawer-open", {});
        await click(input, DRAWER_CLOSE, sink);
        await waitForSurface(cdp, "output drawer closed", (value) => value.drawer.collapsed);
        await recordLayout(cdp, sink, "lynx-narrow-drawer-closed", { empty: true });
        await replaceText(cdp, input, PROMPT, LYNX_LONG_PROMPT, sink);
        await rememberNode(cdp, PROMPT.selector);
        await click(input, REFRESH, sink);
        await waitForSurface(cdp, "narrow refresh preserves draft and focus", (value) => lynxDraftFailures(value, { value: LYNX_LONG_PROMPT }).length === 0);
        await recordLayout(cdp, sink, "lynx-narrow-long-prompt", {});
        await click(input, DRAWER_OPEN, sink);
        await waitForSurface(cdp, "output drawer reopened", (value) => !value.drawer.collapsed);
        await recordLayout(cdp, sink, "lynx-narrow-long-prompt-drawer", {});

        if (state.provider.requestLedger.length !== 0) throw productFailure("editing contacted the provider", state.provider.requestLedger);
        await replaceText(cdp, input, PROMPT, RUN_STOP_PROMPT, sink);
        await click(input, action(".composer", "send"), sink);
        await waitForSurface(cdp, "one real held run with an exact Stop target", (value) => value.projection.task_activity_state === "running"
          && exactTurnStopTarget(value.projection.stop_target) && exactHeldRunStopLedger(state.provider.requestLedger), 30_000);
        let previousRenderedTarget = null;
        let settledSamples = 0;
        const runningComposer = await waitForSurface(cdp, "rendered running composer matches native admission owner", (value) => {
          if (!lynxRunningComposerReady(value) || !exactHeldRunStopLedger(state.provider.requestLedger)) {
            previousRenderedTarget = null;
            settledSamples = 0;
            return false;
          }
          settledSamples = isDeepStrictEqual(previousRenderedTarget, value.rendered_run_target) ? settledSamples + 1 : 1;
          previousRenderedTarget = structuredClone(value.rendered_run_target);
          return settledSamples >= 2;
        });
        await sink.record("lynx-running-composer-settled", runningComposer, { phase: "executing", owner: OWNER });
        await recordLayout(cdp, sink, "lynx-running-narrow-layout", { running: true });
        await replaceText(cdp, input, PROMPT, LYNX_LONG_PROMPT, sink);
        await rememberNode(cdp, PROMPT.selector);
        await assertStable(cdp, sink, "lynx-running-poll-retains-focused-draft", (value) => {
          const failures = lynxDraftFailures(value, { value: LYNX_LONG_PROMPT });
          if (value.projection.task_activity_state !== "running" || !value.projection.async_polling_required
            || !exactHeldRunStopLedger(state.provider.requestLedger)) failures.push("held-runtime-polling-not-active");
          if (!isDeepStrictEqual(value.rendered_run_target, runningComposer.rendered_run_target)
            || !isDeepStrictEqual(value.projection.run_target, runningComposer.projection.run_target)) failures.push("running-composer-owner-changed");
          return failures;
        });
        await click(input, action(".run-strip", "cancel-run"), sink);
        await waitForSurface(cdp, "trusted Stop settles to idle without replay", (value) => value.projection.task_activity_state === "idle"
          && value.projection.run_status_key === "cancelled" && value.projection.stop_target === null
          && exactStoppedRunStopLedger(state.provider.requestLedger), 30_000);
        state.acceptedLedger = structuredClone(state.provider.requestLedger);
        await recordLayout(cdp, sink, "lynx-stopped-narrow-layout", {});
        await cdp.call("Emulation.clearDeviceMetricsOverride");
        metricsOverridden = false;
        await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: OWNER, screenshotStem: "lynx-shell-restored" });
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) { primaryError = error; throw error; }
      finally {
        const cleanup = [() => input.cleanup(), () => cdp.evaluate(`delete globalThis[Symbol.for('${NODE_KEY}')]`)];
        if (metricsOverridden) cleanup.push(() => cdp.call("Emulation.clearDeviceMetricsOverride"));
        for (const release of cleanup) {
          try { await release(); } catch (error) { state.cleanupFailures.push(error?.message ?? String(error)); }
        }
        if (!primaryError && state.cleanupFailures.length) throw new DesktopE2eError("harness", "lynx-input-cleanup", "LYNX input cleanup failed", state.cleanupFailures);
      }
    },
    async quiesce({ inputs }) {
      state.quiesce ??= await quiesceProviderResource({ provider: state.provider, acceptedLedger: state.acceptedLedger, inputs });
      return structuredClone(state.quiesce);
    },
    async cleanup() {
      return { input: state.quiesce?.input === "pass" && state.cleanupFailures.length === 0 ? "pass" : "fail",
        resources: [{ kind: "lynx-input", failures: state.cleanupFailures }] };
    },
  });
}

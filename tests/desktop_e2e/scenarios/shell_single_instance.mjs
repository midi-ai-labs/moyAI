import path from "node:path";
import { isDeepStrictEqual } from "node:util";

import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { WebviewInput, assertTrustedProbeSequence, assertTrustedTextInsertion } from "../drivers/webview_input.mjs";
import { startScriptedProvider } from "../drivers/scripted_provider.mjs";
import {
  TAURI_MAIN_WINDOW_CLASS, probeExactOwnedWindow, selectSingleOwnedRootWindow,
  snapshotOwnedTopLevelWindows,
} from "../drivers/windows_native_input.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import { captureScenarioScreenshot, selectedNavigationIdentity } from "./observations.mjs";
import {
  PROVIDER_RESTART_PROMPT, providerRestartFixtureConfig, quiesceProviderResource,
  relevantProviderHistory, settledCompletedProviderTurn,
} from "./provider_restart.mjs";

const OWNER = "scenario:shell.single-instance";
const DRAFT = "Unsent draft survives restoring the existing Desktop. 日本語";
const PROMPT = { selector: "section.composer textarea#prompt", identity: { tag: "TEXTAREA", id: "prompt" } };
const SEND = { selector: 'button[data-action="send"]', identity: { tag: "BUTTON", action: "send" } };

export function duplicateLaunchAccepted(result, expectedOwner) {
  const desktops = result?.desktops;
  const owner = desktops?.[0];
  return result?.process?.outcome?.root_exit_code === 0
    && result.process.outcome.timed_out === false
    && result.process.job.descendant_zero === true
    && /moyAI Desktop is already running; showing the existing window\./.test(result.stdout)
    && desktops.length === 1
    && owner.process_id === expectedOwner.process_id
    && owner.process_start_time_utc_ticks === expectedOwner.process_start_time_utc_ticks
    && path.win32.normalize(owner.executable_path).toLowerCase() === path.win32.normalize(expectedOwner.executable_path).toLowerCase();
}

export function restoredSingleInstanceAccepted({ window, surface, before, candidate }) {
  return window?.live === true && window.exact_identity === true
    && window.window?.hwnd === candidate.hwnd
    && window.window?.visible === true && window.window?.minimized === false
    && surface?.notice_visible === true
    && /moyAI は既に起動しています。既存のウィンドウを表示しました。/.test(surface.notice)
    && surface.notice === surface.projection?.status_message
    && surface.prompt === before.prompt && surface.prompt.length > 0
    && isDeepStrictEqual(selectedNavigationIdentity(surface.projection), selectedNavigationIdentity(before.projection))
    && selectedNavigationIdentity(surface.projection).session_id !== null
    && isDeepStrictEqual(surface.projection.draft_target, before.projection.draft_target)
    && isDeepStrictEqual(relevantProviderHistory(surface.projection), relevantProviderHistory(before.projection))
    && surface.projection.busy === false && surface.errors.length === 0;
}

async function observe(cdp) {
  return cdp.evaluate(`(async () => {
    const projection = await window.__TAURI_INTERNALS__.invoke('desktop_state');
    const visible = el => {
      if (!(el instanceof HTMLElement) || !el.isConnected) return false;
      const r = el.getBoundingClientRect(), s = getComputedStyle(el);
      return r.width > 0 && r.height > 0 && s.display !== 'none' && s.visibility !== 'hidden' && Number(s.opacity) !== 0;
    };
    const notice = document.querySelector('header.topbar .status-line > span');
    const send = document.querySelector('section.composer button[data-action="send"]');
    return { projection, prompt: document.querySelector('section.composer #prompt')?.value ?? null,
      send_enabled: visible(send) && !send.disabled,
      notice: notice?.textContent?.trim() ?? '', notice_visible: visible(notice),
      errors: [...document.querySelectorAll('.fatal,.ui-error-notice')].filter(visible).map(el => el.textContent) };
  })()`);
}

async function wait(label, sample, accept, { product = true } = {}) {
  try {
    return (await waitForObservation({ label, timeoutMs: 15_000, pollMs: 100, retrySampleErrors: false, sample, accept })).value;
  } catch (error) {
    if (product && error?.code === "observation-timeout" && !error.evidence?.last_error) {
      throw new DesktopE2eError("product", "single-instance-restore-mismatch", label, error.evidence);
    }
    throw error;
  }
}

async function click(input, locator) {
  const start = (await input.snapshotProbe()).sequence;
  await input.click(locator);
  return assertTrustedProbeSequence(await input.snapshotProbe(start), { afterSequence: start, expected: [
    { type: "pointerdown", identity: locator.identity, button: 0, buttons: 1 },
    { type: "pointerup", identity: locator.identity, button: 0, buttons: 0 },
    { type: "click", identity: locator.identity, button: 0, buttons: 0 },
  ] });
}

async function type(input, text) {
  await click(input, PROMPT);
  const start = (await input.snapshotProbe()).sequence;
  await input.insertText(PROMPT, text);
  return assertTrustedTextInsertion(await input.snapshotProbe(start), { afterSequence: start, identity: PROMPT.identity, text });
}

export function createShellSingleInstanceScenario() {
  let provider = null, acceptedLedger = null;
  let inputCleanup = { input: "pass", resources: [] };
  return Object.freeze({
    id: "shell.single-instance", productOracle: "pass", manualGate: "not_required", databaseRequired: true,
    async prepare({ context, sink, phase }) {
      provider = await startScriptedProvider({ expectedPrompt: PROVIDER_RESTART_PROMPT });
      await prepareDesktopFixture({ context, sink, phase, owner: OWNER, configText: providerRestartFixtureConfig(provider.baseUrl) });
    },
    requestGracefulExit,
    async quiesce({ inputs }) { return quiesceProviderResource({ provider, acceptedLedger, inputs }); },
    async cleanup() { return inputCleanup; },
    async execute({ context, runtime, driver: cdp, host, sink }) {
      await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: OWNER, screenshotStem: "single-instance-shell" });
      const input = new WebviewInput(cdp, { probeId: "shell-single-instance" });
      let primary = null;
      try {
        await input.installProbe();
        await type(input, PROVIDER_RESTART_PROMPT);
        await wait("fixture prompt enables Send", () => observe(cdp), value => value.prompt === PROVIDER_RESTART_PROMPT && value.send_enabled);
        await click(input, SEND);
        await wait("one completed session exists before duplicate launch", () => observe(cdp), value => settledCompletedProviderTurn(value.projection));
        const typing = await type(input, DRAFT);
        const before = await wait("unsent draft belongs to the completed session", () => observe(cdp), value => value.prompt === DRAFT && value.send_enabled);
        const native = { executionRoot: context.root, ownerPath: runtime.desktop_owner_path, expectedOwner: runtime.desktop_owner };
        const candidate = selectSingleOwnedRootWindow(await snapshotOwnedTopLevelWindows(native), runtime.desktop_owner, { expectedClassName: TAURI_MAIN_WINDOW_CLASS });
        const target = { executionRoot: context.root, ownerPath: runtime.desktop_owner_path, candidate };
        await sink.record("single-instance-before", { surface: before, window: candidate, typing }, { phase: "executing", owner: OWNER });
        for (const state of ["hidden", "minimized"]) {
          const action = state === "hidden" ? "close-window" : "minimize-window";
          const setup = await click(input, { selector: `button[data-window-control][data-action="${action}"]`, identity: { tag: "BUTTON", action } });
          const prepared = await wait(`main HWND becomes ${state}`, () => probeExactOwnedWindow(target), value => value.live && (
            state === "hidden" ? value.window.visible === false : value.window.minimized === true
          ), { product: false });
          await sink.record("single-instance-window-prepared", { state, setup, observation: prepared }, { phase: "executing", owner: OWNER });
          const duplicate = await host.launchDuplicate({ context, sink });
          const nativeRestored = await wait(`duplicate restores the same ${state} HWND`, () => probeExactOwnedWindow(target),
            value => value.live && value.window.hwnd === candidate.hwnd && value.window.visible && value.window.minimized === false);
          const windows = await snapshotOwnedTopLevelWindows(native);
          await sink.record("single-instance-window-restored", {
            state, window: nativeRestored, foreground_root_hwnd: windows.foreground_root_hwnd,
            foreground_process_id: windows.foreground_process_id,
            main_is_foreground: windows.foreground_root_hwnd === candidate.hwnd,
          }, { phase: "executing", owner: OWNER });
          const surface = await wait(`duplicate launch restores the same ${state} HWND with an explicit visible notice`,
            () => observe(cdp), value => restoredSingleInstanceAccepted({ window: nativeRestored, surface: value, before, candidate }));
          const restored = { window: await probeExactOwnedWindow(target), surface };
          if (!restoredSingleInstanceAccepted({ ...restored, before, candidate })) {
            throw new DesktopE2eError("product", "single-instance-window-drift", "restored main window changed before the visible notice settled", restored);
          }
          if (!duplicateLaunchAccepted(duplicate, runtime.desktop_owner)) {
            throw new DesktopE2eError("product", "single-instance-duplicate-launch-mismatch", "duplicate must exit successfully with a clear message and leave exactly the original Desktop", duplicate);
          }
          await sink.record("single-instance-restored", { state, ...restored }, { phase: "executing", owner: OWNER });
          await captureScenarioScreenshot({ cdp, sink, name: `single-instance-${state}-restored`, owner: OWNER });
        }
        acceptedLedger = structuredClone(provider.requestLedger);
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primary = error;
        try { await captureScenarioScreenshot({ cdp, sink, name: "single-instance-failure", owner: OWNER }); } catch {}
        throw error;
      } finally {
        try { inputCleanup.resources.push({ kind: "webview-input", result: await input.cleanup() }); }
        catch (error) {
          inputCleanup = { input: "fail", resources: [{ kind: "webview-input", message: error.message }] };
          if (primary === null) throw error;
        }
      }
    },
  });
}

import path from "node:path";
import { readFile } from "node:fs/promises";
import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { normalizeHubBrowserOptions, startHubBrowserResource } from "../drivers/hub_browser_resource.mjs";
import { WebviewInput, assertTrustedProbeSequence } from "../drivers/webview_input.mjs";
import { snapshotOwnedTopLevelWindows, selectFreshOwnedRootWindow, openFilePathInOwnedNativeDialog,
  probeExactOwnedWindow, closeOwnedWindowForCleanup, captureOwnedWindowPng } from "../drivers/windows_native_input.mjs";
import { acquireInteractiveShell, prepareShellBaseline, requestGracefulExit } from "./shell_baseline.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";

const OWNER = "scenario:hub.browser-enrollment";
const DIALOG = '[role="dialog"][data-modal="hub"]';
const fail = (message, evidence) => new DesktopE2eError("product", "hub-browser-enrollment-mismatch", message, evidence);
export const byId = (id, tag = "BUTTON") => ({ selector: `[id=${JSON.stringify(id)}]`, identity: { tag, id } });
export const action = (name, scope = DIALOG) => ({ selector: `${scope} button[data-action="${name}"]`, identity: { tag: "BUTTON", action: name } });
export const hubSettingsCloseTarget = action("close-overlay", `${DIALOG} .hub-modal-footer`);

export function enrollmentAccepted(network, snapshot) {
  return network?.enrollment === "active" && typeof network.device_id === "string"
    && snapshot?.devices?.filter(device => device.device_id === network.device_id).length === 1;
}
export function independentSelectionsAccepted(hub, mainId, sideId) {
  return mainId !== sideId && hub?.status === "connected" && [
    ["main", mainId], ["side_chat", sideId],
  ].every(([context, model]) => hub[`${context}_confirmation`] === "confirmed"
    && hub[`${context}_mode`] === "hub"
    && hub[`${context}_review`]?.selection?.preferred_model_id === model
    && JSON.stringify(hub[`${context}_review`]?.selection?.allowed_model_ids) === JSON.stringify([model]));
}
export function connectedShellAccepted(value, mainId, sideId) {
  const state = value?.state;
  // An enrolled device keeps polling for Hub heartbeats even while its shell is idle.
  return independentSelectionsAccepted(state?.hub, mainId, sideId)
    && state?.device_network?.enrollment === "active" && state?.overlay === "none"
    && state?.run_status_key === "idle" && state?.busy === false && state?.provider_loading === false
    && state?.confirmation_visible === false && state?.background_mutation_pending === false
    && state?.pending_async_operations?.length === 0 && state?.navigation_loading === false
    && state?.navigation_admission_open === true && state?.can_submit === true
    && value?.prompt_enabled === true && value?.prompt_center_hit === true && value?.blocking_dialogs === 0;
}

export async function wait(label, sample, accept, timeoutMs = 30_000) {
  try { return (await waitForObservation({ label, sample, accept, timeoutMs, pollMs: 100, retrySampleErrors: false })).value; }
  catch (error) {
    if (error?.code === "observation-timeout" && !error.evidence?.last_error) throw fail(label, error.evidence);
    throw error;
  }
}
export async function trustedFocus(input, cdp, target) {
  await wait("Desktop control is available", () => cdp.evaluate(`(() => {
    const nodes = document.querySelectorAll(${JSON.stringify(target.selector)});
    return nodes.length === 1 && !nodes[0].disabled && nodes[0].closest('[hidden]') === null;
  })()`), value => value === true, 10_000);
  // Keyboard navigation lets the product bring off-screen settings into view.
  let focused = false, reentered = false;
  for (let count = 0; count < 100; ++count) {
    const observation = await cdp.evaluate(`(() => { const nodes = document.querySelectorAll(${JSON.stringify(target.selector)});
      return { count: nodes.length, focused: nodes.length === 1 && document.activeElement === nodes[0] }; })()`);
    if (observation.count !== 1) throw fail("Desktop control is not unique", { target, observation });
    if (observation.focused) {
      const { observation: geometry } = await input.observeExactTarget(target);
      if ((!geometry.center_in_viewport || !geometry.center_in_scroll_clip) && !reentered) {
        // Native Tab can reveal only a textarea's first caret line. Visit the
        // adjacent control beyond the clipped edge, then return once by keyboard.
        reentered = true;
        const below = geometry.center.y >= geometry.scroll_clip.bottom;
        if (below) await input.pressKey("Tab");
        await input.keyDown("Shift");
        try { await input.pressKey("Tab"); } finally { await input.keyUp("Shift"); }
        if (!below) await input.pressKey("Tab");
        continue;
      }
      focused = true; break;
    }
    await input.pressKey("Tab");
  }
  if (!focused) throw new DesktopE2eError("harness", "hub-browser-focus-unreachable", "Desktop control cannot be reached by keyboard", { target });
}
export async function trustedClick(input, cdp, target, sink) {
  await trustedFocus(input, cdp, target);
  const start = (await input.snapshotProbe()).sequence;
  await input.click(target, { stableHitSamples: 3 });
  const probe = assertTrustedProbeSequence(await input.snapshotProbe(start), { afterSequence: start, expected: [
    { type: "pointerdown", identity: target.identity, button: 0, buttons: 1 },
    { type: "pointerup", identity: target.identity, button: 0, buttons: 0 },
    { type: "click", identity: target.identity, button: 0, buttons: 0 },
  ] });
  await sink.record("hub-browser-desktop-trusted-click", { target, probe }, { phase: "executing", owner: OWNER });
}

export function createHubBrowserEnrollmentScenario(options = {}) {
  const settings = normalizeHubBrowserOptions(options);
  const state = { resource: null, input: null, nativeOwner: null, nativeCandidate: null, nativeBefore: null,
    importDispatched: false, inputFailures: [], close: null };
  return Object.freeze({
    id: "hub.browser-enrollment", productOracle: "pass", manualGate: "not_required", databaseRequired: true,
    async prepare(args) {
      await prepareShellBaseline(args);
      state.resource = await startHubBrowserResource({ ...args, options: settings });
    },
    async execute({ context, runtime, driver: cdp, sink }) {
      const resource = state.resource, { page, hub, provider } = resource;
      await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: OWNER, screenshotStem: "hub-browser-desktop-shell" });
      const input = state.input = new WebviewInput(cdp, { probeId: "hub-browser-enrollment" });
      try {
      await input.installProbe();
      await page.goto(hub.url);
      await page.locator("#management-status").filter({ hasText: "Hub本体に接続中" }).waitFor();
      await page.locator('nav a[href="#models"]').click();
      await page.locator("#endpoint").fill(provider.url);
      await page.locator("#profile").selectOption("openai_compatible_chat");
      await page.locator("#discover").click();
      await page.locator('#model option[value="fixture-alpha"]').waitFor({ state: "attached" });
      for (const [model, label] of [["fixture-alpha", "Enrollment Main"], ["fixture-beta", "Enrollment Side"]]) {
        await page.locator("#model").selectOption(model);
        await page.locator("#label").fill(label);
        await page.locator("#allow-tools").check();
        await page.locator("#register").click();
        await page.locator("#model-rows tr").filter({ hasText: label }).waitFor();
      }
      const registered = await hub.command("hub_snapshot");
      const models = registered.store.catalog.models;
      if (models.length !== 2 || new Set(models.map(model => model.label)).size !== 2) throw fail("Hub must contain the two browser-registered models", { models });
      await resource.screenshot("hub-browser-registered-models");
      await page.locator('nav a[href="#device-network"]').click();
      await page.locator("#network-ip").fill("127.0.0.1");
      await page.locator("#network-port").fill(String(hub.networkPort));
      await page.getByRole("button", { name: "ネットワークを開始", exact: true }).click();
      await page.getByRole("button", { name: "ネットワークを停止", exact: true }).waitFor();
      await hub.observeNetwork();
      await enrollDesktopFromHubBrowser({ resource, context, runtime, cdp, input, sink, nativeState: state });
      await trustedClick(input, cdp, byId("hub-tab-models"), sink);
      const connected = await wait("Enrollment automatically connects the model catalog", () => invokeDesktopCommand(cdp, "hub_projection"), value => value.status === "connected" && value.catalog?.models.length === 2);
      await wait("Desktop renders the managed model catalog", () => cdp.evaluate(`(() => {
        const rows = [...document.querySelectorAll('input[data-hub-field*="model:"]')];
        return rows.length === 4 && rows.every(row => !row.disabled);
      })()`), value => value === true);
      if (await cdp.evaluate(`document.querySelector('#hub-manual-connection')?.open`)) {
        throw fail("Managed model refresh must be reachable with manual connection closed", {});
      }
      await trustedClick(input, cdp, action("hub-refresh"), sink);
      await wait("Model refresh finishes without opening manual connection settings", () => cdp.evaluate(`(() => {
        const refresh = document.querySelector('button[data-action="hub-refresh"]');
        return Boolean(refresh && !refresh.disabled && !document.querySelector('#hub-manual-connection')?.open);
      })()`), value => value === true);
      await captureScenarioScreenshot({ cdp, sink, name: "hub-models-visible-refresh", owner: OWNER });
      const selected = Object.fromEntries([["main", "Enrollment Main"], ["side_chat", "Enrollment Side"]].map(([name, label]) => {
        const matches = connected.catalog.models.filter(model => model.label === label);
        if (matches.length !== 1) throw fail("Desktop catalog does not match browser registration", { label });
        return [name, matches[0].id];
      }));
      for (const name of ["main", "side_chat"]) {
        for (const model of connected.catalog.models) {
          const target = byId(`hub-${name}-model-${model.id}`, "INPUT");
          const checked = await cdp.evaluate(`document.querySelector(${JSON.stringify(target.selector)})?.checked`);
          if (checked !== (model.id === selected[name])) await trustedClick(input, cdp, target, sink);
        }
        const route = name === "main" ? "main" : "side";
        await trustedClick(input, cdp, action(`hub-save-${route}`), sink);
        await wait(`Desktop ${name} saved model selection`, () => invokeDesktopCommand(cdp, "hub_projection"), value => value[`${name}_confirmation`] === "confirmed");
        await trustedClick(input, cdp, action(`hub-${route}-hub`), sink);
      }
      const accepted = await wait("Desktop Main and Side independently use Hub selections", () => invokeDesktopCommand(cdp, "hub_projection"), value => independentSelectionsAccepted(value, selected.main, selected.side_chat));
      await captureScenarioScreenshot({ cdp, sink, name: "hub-browser-desktop-model-selection", owner: OWNER });
      await resource.screenshot("hub-browser-approved-device");
      await sink.record("hub-browser-independent-model-selection", { hub_id: accepted.hub_id, revision: accepted.catalog.revision,
        main: accepted.main_review, side_chat: accepted.side_chat_review }, { phase: "executing", owner: OWNER });
      if (resource.pageErrors().length) throw fail("Hub browser reported page errors", resource.pageErrors());
      await trustedClick(input, cdp, hubSettingsCloseTarget, sink);
      const shell = await wait("The enrolled Desktop returns to an interactive shell", () => cdp.evaluate(`(async () => {
        const state = await window.__TAURI_INTERNALS__.invoke('desktop_state');
        const prompts = document.querySelectorAll('textarea#prompt');
        const prompt = prompts.length === 1 ? prompts[0] : null;
        const rect = prompt?.getBoundingClientRect();
        return { state,
          prompt_enabled: prompt !== null && !prompt.disabled && !prompt.readOnly && prompt.closest('[hidden], [inert], [aria-hidden="true"]') === null,
          prompt_center_hit: Boolean(rect?.width && rect?.height && document.elementFromPoint(rect.left + rect.width / 2, rect.top + rect.height / 2) === prompt),
          blocking_dialogs: [...document.querySelectorAll('[data-modal], [role="dialog"], [role="alertdialog"]')].filter(node => {
            const rect = node.getBoundingClientRect(), style = getComputedStyle(node);
            return rect.width > 0 && rect.height > 0 && style.display !== 'none' && style.visibility !== 'hidden';
          }).length,
        };
      })()`), value => connectedShellAccepted(value, selected.main, selected.side_chat));
      await sink.record("hub-browser-enrolled-shell", { device_id: shell.state.device_network.device_id,
        async_polling_required: shell.state.async_polling_required, pending_async_operations: shell.state.pending_async_operations,
        prompt_enabled: shell.prompt_enabled, prompt_center_hit: shell.prompt_center_hit, blocking_dialogs: shell.blocking_dialogs,
      }, { phase: "executing", owner: OWNER });
      await captureScenarioScreenshot({ cdp, sink, name: "hub-browser-desktop-finished", owner: OWNER });
      await trustedClick(input, cdp, action("show-hub", "aside.sidebar"), sink);
      await trustedClick(input, cdp, byId("hub-tab-devices"), sink);
      const sharedShortcut = await wait("Connected device panel has rendered its shared-work shortcut", async () =>
        (await input.observeExactTarget(byId("device-network-open-shared"))).observation, value => value.visible);
      if (!sharedShortcut.center_in_viewport || !sharedShortcut.center_in_scroll_clip) {
        throw fail("Connected Desktop exposes the shared-work shortcut without scrolling", sharedShortcut);
      }
      await captureScenarioScreenshot({ cdp, sink, name: "hub-connection-next-steps", owner: OWNER });
      await trustedClick(input, cdp, byId("device-network-open-shared"), sink);
      const sharedEntry = await wait("Connection card opens shared work without reimport or automatic human login", () => cdp.evaluate(`(async () => {
        const state = await window.__TAURI_INTERNALS__.invoke('desktop_state');
        const shared = await window.__TAURI_INTERNALS__.invoke('shared_work_projection');
        const login = document.querySelector('#shared-username');
        return { overlay: state.overlay, hub_project_open: state.hub_project_open, connected: shared.connected, principal: shared.principal,
          login_visible: Boolean(login && login.getClientRects().length && !login.closest('[hidden], [inert]')) };
      })()`), value => value.hub_project_open === true && value.overlay === "none" && value.connected && value.principal === null && value.login_visible);
      await sink.record("hub-connection-shared-entry", sharedEntry, { phase: "executing", owner: OWNER });
      await captureScenarioScreenshot({ cdp, sink, name: "hub-connection-shared-login", owner: OWNER });
      return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } finally {
        try { await input.cleanup(); } catch (error) { state.inputFailures.push(error?.code ?? "input-cleanup-failed"); }
        state.input = null;
      }
    },
    requestGracefulExit: cdp => requestHubEnrollmentExit(cdp, state),
    async quiesce() {
      if (state.input) {
        try { await state.input.cleanup(); } catch (error) { state.inputFailures.push(error?.code ?? "input-cleanup-failed"); }
        state.input = null;
      }
      state.close ??= state.resource ? await state.resource.close() : { pass: true, started: false };
      return { input: state.close.pass && state.inputFailures.length === 0 ? "pass" : "fail",
        resources: [{ kind: "hub-browser", close: state.close, input_failures: state.inputFailures }] };
    },
    async cleanup() { return { input: state.close?.pass && state.inputFailures.length === 0 ? "pass" : "fail", resources: [] }; },
  });
}

export async function importDesktopHubParticipationFile({ context, runtime, cdp, input, sink, nativeState: state, importPath, entry = "hub" }) {
  if (!["hub", "initial-setup", "shared-work"].includes(entry)) throw new TypeError("Unknown Hub participation GUI entry");
  if (entry === "hub") {
    if ((await invokeDesktopCommand(cdp, "desktop_state")).overlay !== "hub") await trustedClick(input, cdp, action("show-hub", "aside.sidebar"), sink);
    await trustedClick(input, cdp, byId("hub-tab-devices"), sink);
  }
  await wait("Desktop is ready to import Hub config", () => invokeDesktopCommand(cdp, "device_network_projection"), value => value.enrollment === "unconfigured");
  state.nativeOwner = { executionRoot: context.root, ownerPath: runtime.desktop_owner_path, expectedOwner: runtime.desktop_owner };
  state.nativeBefore = await snapshotOwnedTopLevelWindows(state.nativeOwner);
  state.importDispatched = true;
  await trustedClick(input, cdp, entry === "shared-work" ? action("shared-import", ".shared-work") : byId(entry === "initial-setup" ? "initial-setup-hub" : "device-network-import"), sink);
  const native = await wait("Exact Desktop native config picker", async () => {
    const windows = await snapshotOwnedTopLevelWindows(state.nativeOwner);
    try { return selectFreshOwnedRootWindow(state.nativeBefore, windows, runtime.desktop_owner, { expectedClassName: "#32770" }); }
    catch (error) { if (error?.code === "native-window-cardinality" && error.evidence?.fresh_windows?.length === 0) return null; throw error; }
  }, Boolean);
  state.nativeCandidate = native;
  const nativeCapture = await captureOwnedWindowPng({ ...state.nativeOwner, candidate: native });
  if (nativeCapture.available) await sink.writeBytes("screenshots/hub-browser-native-config-picker.png", nativeCapture.bytes);
  const selection = await openFilePathInOwnedNativeDialog({ ...state.nativeOwner, candidate: native, selectedPath: importPath });
  await wait("Desktop native config picker closed", () => probeExactOwnedWindow({ ...state.nativeOwner, candidate: native }), value => !value.live);
  state.nativeCandidate = null; state.importDispatched = false;
  await sink.record("hub-browser-native-config-import", { selection, path: importPath }, { phase: "executing", owner: OWNER });
}

/** GUI enrollment shared by combined Desktop scenarios; nativeState owns interrupted picker cleanup. */
export async function enrollDesktopFromHubBrowser({ resource, context, runtime, cdp, input, sink, nativeState: state, entry = "hub", onEnrollmentError = null }) {
  const { page, hub } = resource;
  const downloading = page.waitForEvent("download");
  await page.locator("#network-save-config").click();
  const download = await downloading;
  if (download.suggestedFilename() !== "hub-config.moyai-join") throw fail("Unexpected Hub config download filename", {});
  const importPath = path.join(context.paths.workspace, "hub-config.moyai-join");
  await download.saveAs(importPath);
  const config = await readFile(importPath, "utf8");
  if (!config.includes("BEGIN CERTIFICATE") || config.includes("PRIVATE KEY") || !config.includes(`127.0.0.1:${hub.networkPort}`)) {
    throw fail("Downloaded participation config must contain this Hub URL and public CA only", {});
  }
  await importDesktopHubParticipationFile({ context, runtime, cdp, input, sink, nativeState: state, importPath, entry });
  const requested = await wait("Desktop finishes its enrollment request", () => invokeDesktopCommand(cdp, "device_network_projection"), value => value.enrollment === "error" || value.enrollment === "pending" && value.request_id);
  if (requested.enrollment === "error") {
    if (!onEnrollmentError) throw fail("Desktop enrollment was rejected", { error: requested.error });
    await onEnrollmentError(requested);
  }
  const pending = await wait("Desktop displays pending enrollment before approval", () => invokeDesktopCommand(cdp, "device_network_projection"), value => value.enrollment === "pending" && value.request_id);
  await wait("Desktop visible approval pending status", () => cdp.evaluate(`document.querySelector('[data-settings-passive="device-network-enrollment"]')?.textContent`), value => value?.includes("承認待ち"));
  await captureScenarioScreenshot({ cdp, sink, name: "hub-browser-desktop-pending", owner: OWNER });
  await page.locator('nav a[href="#clients"]').click();
  const row = page.locator(`[data-id="request:${pending.request_id}"]`);
  await row.waitFor();
  const request = (await hub.observeNetwork()).join_requests.find(value => value.request_id === pending.request_id);
  await resource.screenshot("hub-browser-pending-device");
  await row.locator("button[data-network-action]").click();
  await page.locator("#join-project-save").click();
  await page.locator("#join-project-dialog").waitFor({ state: "hidden" });
  const active = await wait("Approved Desktop enrolls without a code or restart", async () => ({
    network: await invokeDesktopCommand(cdp, "device_network_projection"), snapshot: await hub.observeNetwork(),
  }), value => enrollmentAccepted(value.network, value.snapshot), 45_000);
  await wait("Desktop visible enrolled status", () => cdp.evaluate(entry === "shared-work"
    ? `Boolean(document.querySelector('.shared-work #shared-username')?.getClientRects().length)`
    : `document.querySelector('[data-settings-passive="device-network-enrollment"]')?.textContent?.includes('Hubに接続済み')`), value => value === true);
  await sink.record("hub-browser-desktop-approved", { device_id: active.network.device_id,
    request_id: pending.request_id, hub_device_count: active.snapshot.devices.length }, { phase: "executing", owner: OWNER });
  const device = active.snapshot.devices.find(value => value.device_id === active.network.device_id);
  return { network: active.network, requestId: pending.request_id, keySha256: request?.key_sha256, certificateSha256: device?.certificate_sha256 };
}

export async function requestHubEnrollmentExit(cdp, state) {
  // An interrupted native picker must be settled before requesting the ordinary Desktop exit.
  if (state.importDispatched && state.nativeOwner) {
    try {
      const current = await snapshotOwnedTopLevelWindows(state.nativeOwner);
      let candidate = state.nativeCandidate;
      if (!candidate) {
        try { candidate = selectFreshOwnedRootWindow(state.nativeBefore, current, state.nativeOwner.expectedOwner, { expectedClassName: "#32770" }); }
        catch (error) { if (error?.code !== "native-window-cardinality" || error.evidence?.fresh_windows?.length !== 0) throw error; }
      }
      if (candidate && (await probeExactOwnedWindow({ ...state.nativeOwner, candidate })).live) await closeOwnedWindowForCleanup({ ...state.nativeOwner, candidate });
      state.nativeCandidate = null; state.importDispatched = false;
    } catch { return { requested: false, reason: "hub-browser-native-dialog-cleanup-failed" }; }
  }
  return requestGracefulExit(cdp);
}

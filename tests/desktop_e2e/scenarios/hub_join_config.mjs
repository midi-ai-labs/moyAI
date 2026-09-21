import path from "node:path";
import { X509Certificate, createHash, randomUUID } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { rootCertificates } from "node:tls";
import { pathToFileURL } from "node:url";
import { isDeepStrictEqual } from "node:util";
import { DesktopE2eError } from "../core/execution.mjs";
import { normalizeDesktopIsolation } from "../core/desktop_isolation.mjs";
import { normalizeHubBrowserOptions, startHubBrowserResource } from "../drivers/hub_browser_resource.mjs";
import { WebviewInput, assertTrustedTextInsertion } from "../drivers/webview_input.mjs";
import { DesktopCommandProbe } from "../drivers/desktop_command_probe.mjs";
import {
  TAURI_MAIN_WINDOW_CLASS, snapshotOwnedTopLevelWindows, selectSingleOwnedRootWindow,
  selectFreshOwnedRootWindow, probeExactOwnedWindow, invokeOwnedNativeDialogButton,
  captureOwnedWindowPng, closeOwnedWindowForCleanup,
} from "../drivers/windows_native_input.mjs";
import { prepareShellBaseline, acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";
import { duplicateLaunchAccepted } from "./shell_single_instance.mjs";
import { trustedClick, wait, enrollmentAccepted, byId } from "./hub_browser_enrollment.mjs";
import { sharedActionTarget, observeSharedWorkSurface } from "./shared_work_navigation.mjs";

const TITLE = "moyAI: チームの接続先を確認";
const DRAFT = "Keep this unsent draft when cancelling team participation. 日本語";
const PROMPT = { selector: "section.composer textarea#prompt", identity: { tag: "TEXTAREA", id: "prompt" } };
const fail = (message, evidence) => new DesktopE2eError("product", "join-config-activation-mismatch", message, evidence);

export function publicCertificatePem(config) {
  return config.replaceAll("\\r\\n", "\n").replaceAll("\\n", "\n")
    .match(/-----BEGIN CERTIFICATE-----[\s\S]+?-----END CERTIFICATE-----/)?.[0];
}

export function directRouteIdentity(p) {
  return { main: p?.hub?.main_mode, side: p?.hub?.side_chat_mode,
    base_url: p?.provider_effective_base_url, model: p?.provider_effective_model_id,
    profile: p?.provider_effective_profile };
}

export function cancelledActivationAccepted({ before, after, configBefore, configAfter, prefsBefore, prefsAfter }) {
  return configBefore === configAfter && prefsBefore === prefsAfter && after?.prompt === DRAFT
    && after?.network?.enrollment === "unconfigured" && after.network.device_id === null && after.network.request_id === null
    && isDeepStrictEqual(directRouteIdentity(before?.desktop), directRouteIdentity(after?.desktop))
    && isDeepStrictEqual(before?.desktop?.draft_target, after?.desktop?.draft_target);
}

export function joinedActivationAccepted(value, expectedDirect, url) {
  return value?.network?.enrollment === "active" && value.network.hub_url === url
    && typeof value.network.device_id === "string" && value.network.device_id.length > 0
    && value.shared?.connected === true && Boolean(value.shared.principal?.user_id) && value.shared.projects?.length === 0
    && value.loginVisible === false && value.desktop?.startup?.onboarding_intent === "team"
    && isDeepStrictEqual(directRouteIdentity(value.desktop), expectedDirect)
    && expectedDirect.main === "direct" && expectedDirect.side === "direct";
}

export function movedActivationAccepted(value, expected) {
  return value?.network?.enrollment === "active" && value.network.hub_url === expected.url
    && value.network.device_id === expected.device_id && value.shared?.connected === true
    && value.shared.principal?.user_id === expected.user_id && value.shared.principal.administrator === false
    && value.surface?.count === 1 && value.surface.login_visible === false
    && value.surface.account_text?.includes(expected.display_name)
    && isDeepStrictEqual(directRouteIdentity(value.desktop), expected.direct)
    && Array.isArray(value.calls) && !value.calls.some(call => ["login", "setup_password"].includes(call.args?.request?.kind));
}

async function deviceTrustIdentity(context) {
  const directory = path.join(path.dirname(context.paths.config_file), "device-network");
  const device = JSON.parse(await readFile(path.join(directory, "device.json"), "utf8"));
  return { hub_id: device.hub_id, device_id: device.device_id, certificate_sha256: device.certificate_sha256,
    key_file_sha256: createHash("sha256").update(await readFile(path.join(directory, "identity.json"))).digest("hex") };
}

async function observe(cdp) {
  return cdp.evaluate(`(async () => ({
    desktop: await window.__TAURI_INTERNALS__.invoke('desktop_state'),
    network: await window.__TAURI_INTERNALS__.invoke('device_network_projection'),
    shared: await window.__TAURI_INTERNALS__.invoke('shared_work_projection'),
    prompt: document.querySelector('section.composer #prompt')?.value ?? null,
    loginVisible: Boolean(document.querySelector('.shared-work #shared-username')?.getClientRects().length),
    sharedSurfaceCount: document.querySelectorAll('.shared-work').length,
    frameClass: document.querySelector('.app-frame')?.className ?? null,
  }))()`);
}

function createJoinConfigScenario(mode, options) {
  const settings = normalizeHubBrowserOptions(options), id = `hub.join-config-${mode}`, owner = `scenario:${id}`;
  const state = { resource: null, input: null, path: null, expectedText: null, url: null,
    nativeOwner: null, nativeCandidate: null, commands: null, close: null, inputFailures: [] };
  return Object.freeze({
    id, productOracle: "pass", manualGate: "not_required", databaseRequired: true,
    get joinConfigPath() { return mode === "cold" ? state.path : null; },
    async prepare(args) {
      if (normalizeDesktopIsolation(args.context.desktopIsolation) !== "fixture") {
        throw new DesktopE2eError("environment", "join-config-fixture-required", "Join activation qualification requires isolated Desktop fixture mode");
      }
      await prepareShellBaseline(args);
      state.resource = await startHubBrowserResource({ ...args, options: settings });
      const { page, hub } = state.resource;
      await page.locator('nav a[href="#device-network"]').click();
      await page.locator("#network-ip").fill("127.0.0.1");
      await page.locator("#network-port").fill(String(hub.networkPort));
      await page.locator("#network-start").click(); await page.locator("#network-stop").waitFor();
      await wait("Fixture Hub network is listening", () => hub.observeNetwork(), p => p.server.running === true);
      const downloading = page.waitForEvent("download");
      await page.locator("#network-save-config").click();
      const download = await downloading;
      if (download.suggestedFilename() !== "hub-config.moyai-join") throw fail("Unexpected public Hub config download", {});
      state.path = path.join(args.context.paths.workspace, "team config 日本語.moyai-join");
      await download.saveAs(state.path);
      const config = await readFile(state.path, "utf8");
      state.url = `https://127.0.0.1:${hub.networkPort}`;
      // TOML may serialize the PEM as a multiline string or an escaped basic string.
      const pem = publicCertificatePem(config);
      if (!config.includes(state.url) || !pem || config.includes("PRIVATE KEY")) throw fail("Downloaded Hub trust must be public and belong to this fixture", {});
      const fingerprint = new X509Certificate(pem).fingerprint256.replaceAll(":", "").toLowerCase();
      state.expectedText = [state.url, fingerprint];
      await args.sink.record("join-config-public-download", { mode, path: state.path, hub_url: state.url, ca_sha256: fingerprint }, { phase: args.phase, owner });
    },
    async execute({ context, runtime, driver: cdp, host, sink }) {
      state.nativeOwner = { executionRoot: context.root, ownerPath: runtime.desktop_owner_path, expectedOwner: runtime.desktop_owner };
      const native = state.nativeOwner;
      let mainWindow = null;
      async function dialog(before, buttonId, stage, review = { title: TITLE, expectedText: state.expectedText }) {
        state.nativeCandidate = await wait("Exact Desktop confirmation dialog is visible", async () => {
          const current = await snapshotOwnedTopLevelWindows(native);
          try { return before ? selectFreshOwnedRootWindow(before, current, runtime.desktop_owner, { expectedClassName: "#32770" })
            : selectSingleOwnedRootWindow(current, runtime.desktop_owner, { expectedClassName: "#32770" }); }
          catch (error) {
            if (error?.code === "native-window-cardinality" && (error.evidence?.fresh_windows ?? error.evidence?.candidate_windows)?.length === 0) return null;
            throw error;
          }
        }, Boolean);
        const target = { ...native, candidate: state.nativeCandidate };
        const screenshot = await captureOwnedWindowPng(target);
        if (screenshot.available) await sink.writeBytes(`screenshots/join-config-${stage}-native.png`, screenshot.bytes);
        const input = await invokeOwnedNativeDialogButton({ ...target, buttonId, expectedTitle: review.title, expectedText: review.expectedText });
        await wait("Reviewed native confirmation closed", () => probeExactOwnedWindow(target), p => !p.live);
        state.nativeCandidate = null;
        await sink.record("join-config-native-review", { mode, stage, input }, { phase: "executing", owner });
      }
      async function duplicate() {
        const before = await snapshotOwnedTopLevelWindows(native);
        const launched = await host.launchDuplicate({ context, sink, joinConfigPath: state.path });
        if (!duplicateLaunchAccepted(launched, runtime.desktop_owner)) throw fail("Activation must reuse the exact Desktop process", launched);
        return before;
      }
      try {
        if (mode === "warm") {
          await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: owner, screenshotStem: "join-config-warm-shell" });
          mainWindow = selectSingleOwnedRootWindow(await snapshotOwnedTopLevelWindows(native), runtime.desktop_owner, { expectedClassName: TAURI_MAIN_WINDOW_CLASS });
          state.input = new WebviewInput(cdp, { probeId: id }); await state.input.installProbe();
          await trustedClick(state.input, cdp, PROMPT, sink);
          const start = (await state.input.snapshotProbe()).sequence;
          await state.input.insertText(PROMPT, DRAFT);
          assertTrustedTextInsertion(await state.input.snapshotProbe(start), { afterSequence: start, identity: PROMPT.identity, text: DRAFT });
        }
        const before = await wait("Before consent the existing Direct setup remains unregistered", () => observe(cdp), p =>
          p.network.enrollment === "unconfigured" && p.network.device_id === null && p.network.request_id === null
          && p.shared.principal === null && Boolean(p.desktop.provider_effective_model_id));
        const expectedDirect = directRouteIdentity(before.desktop);
        if (expectedDirect.main !== "direct" || expectedDirect.side !== "direct" || !expectedDirect.model) throw fail("The fixture must start with an existing Direct model", expectedDirect);
        if (mode === "warm") {
          const configBefore = await readFile(context.paths.config_file, "utf8"), prefsBefore = await readFile(context.paths.prefs_file, "utf8");
          await dialog(await duplicate(), 2, "cancel");
          const after = await observe(cdp);
          const cancel = { before, after, configBefore, configAfter: await readFile(context.paths.config_file, "utf8"),
            prefsBefore, prefsAfter: await readFile(context.paths.prefs_file, "utf8") };
          if (!cancelledActivationAccepted(cancel) || (await state.resource.hub.observeNetwork()).join_requests.length !== 0) {
            throw fail("Cancel must preserve the draft, config and device identity without requesting admission", { before: before.network, after: after.network });
          }
          await captureScenarioScreenshot({ cdp, sink, name: "join-config-cancel-preserved", owner });
          await sink.record("join-config-cancel-preserved", { draft: after.prompt, direct: expectedDirect, config_unchanged: true, preferences_unchanged: true }, { phase: "executing", owner });
          await dialog(await duplicate(), 1, "warm-ok");
        } else {
          await dialog(null, 1, "cold-ok");
        }
        const pending = await wait("File activation requests admission at this Hub", () => invokeDesktopCommand(cdp, "device_network_projection"),
          p => p.hub_url === state.url && p.enrollment === "pending" && Boolean(p.request_id));
        const { page, hub } = state.resource;
        await page.locator('nav a[href="#clients"]').click();
        const row = page.locator(`[data-id="request:${pending.request_id}"]`); await row.waitFor();
        await row.locator("button[data-network-action]").click();
        await page.locator("#join-project-save").click(); await page.locator("#join-project-dialog").waitFor({ state: "hidden" });
        await wait("This approved Desktop device enrolls", async () => ({ network: await invokeDesktopCommand(cdp, "device_network_projection"), snapshot: await hub.observeNetwork() }),
          p => enrollmentAccepted(p.network, p.snapshot), 45_000);
        const joined = await wait("Approved device becomes usable without login with Direct settings retained", () => observe(cdp),
          p => joinedActivationAccepted(p, expectedDirect, state.url));
        await captureScenarioScreenshot({ cdp, sink, name: `join-config-${mode}-device-ready`, owner });
        await sink.record("join-config-completed", { mode, request_id: pending.request_id, device_id: joined.network.device_id,
          direct: directRouteIdentity(joined.desktop), principal: joined.shared.principal, login_visible: joined.loginVisible,
          scope: "Actual native review, public trust import, browser device approval and automatic device access; no project membership or model generation." }, { phase: "executing", owner });
        if (mainWindow) {
          const same = await probeExactOwnedWindow({ ...native, candidate: mainWindow });
          if (!same.live || !same.exact_identity || same.window.hwnd !== mainWindow.hwnd) throw fail("Warm activation replaced the main HWND", same);
        }
        if (mode === "warm") {
          state.commands = new DesktopCommandProbe(cdp, { probeId: id, commands: ["shared_work_command"] });
          await state.commands.install();
          const principal = (await wait("Approved device obtains its assigned actor without credentials", () => invokeDesktopCommand(cdp, "shared_work_projection"), p => p.principal && !p.principal.administrator)).principal;
          const userId = principal.user_id;
          const afterLogin = (await state.commands.snapshot()).sequence;
          const trustBefore = await deviceTrustIdentity(context);
          const configBeforeRejection = await readFile(context.paths.config_file, "utf8");
          const initialPath = state.path;
          // A different real public CA, supplied by Node's bundled trust roots, is rejected locally.
          const wrongCa = rootCertificates.find(pem => new X509Certificate(pem).ca
            && new X509Certificate(pem).fingerprint256.replaceAll(":", "").toLowerCase() !== state.expectedText[1]);
          if (!wrongCa) throw new DesktopE2eError("environment", "different-ca-fixture-missing", "No independent public CA is available in the local Node runtime");
          state.path = path.join(context.paths.workspace, "different-ca.moyai-join");
          await writeFile(state.path, `[device_network]\nhub_url = ${JSON.stringify(state.url)}\nca_certificate_pem = ${JSON.stringify(wrongCa)}\n`, { flag: "wx" });
          await dialog(await duplicate(), 2, "different-ca-rejected", { title: "moyAI: 接続ファイル", expectedText: ["同じHubであることを確認できません", "別Hubや公開CAの変更には対応していません"] });
          if (await readFile(context.paths.config_file, "utf8") !== configBeforeRejection
            || !isDeepStrictEqual(await deviceTrustIdentity(context), trustBefore)) throw fail("Rejected CA changed persisted configuration or device trust", {});
          state.path = initialPath;
          await sink.record("join-config-different-ca-rejected", { config_unchanged: true, device_trust_unchanged: true,
            native_error_acknowledged: true, foreign_ca_source: "Node bundled public root; no remote connection" }, { phase: "executing", owner });

          // Move the actual same Hub listener through its management UI. Its owner
          // observes both ports and proves they close during common resource cleanup.
          const { freePort } = await import(pathToFileURL(path.join(settings.hubRepository, "tests/browser/hub_server.mjs")));
          let newPort = await freePort();
          while (newPort === hub.networkPort) newPort = await freePort();
          const previousUrl = state.url;
          await page.locator('nav a[href="#device-network"]').click();
          await page.locator("#network-stop").click();
          await wait("Existing Hub network stopped before its address changes", () => hub.observeNetwork(), p => p.server.running === false);
          await page.locator("#network-port").fill(String(newPort));
          await page.locator("#network-start").click();
          await wait("Same Hub network listens at its new address", () => hub.observeNetwork(), p => p.server.running === true && p.server.bind.endsWith(`:${newPort}`));
          const downloading = page.waitForEvent("download"); await page.locator("#network-save-config").click();
          const download = await downloading;
          if (download.suggestedFilename() !== "hub-config.moyai-join") throw fail("Moved Hub exported an unexpected filename", {});
          state.path = path.join(context.paths.workspace, "moved-hub.moyai-join"); await download.saveAs(state.path);
          state.url = `https://127.0.0.1:${newPort}`;
          const movedConfig = await readFile(state.path, "utf8");
          const sameCa = new X509Certificate(publicCertificatePem(movedConfig)).fingerprint256.replaceAll(":", "").toLowerCase();
          if (!movedConfig.includes(state.url) || sameCa !== state.expectedText[1]) throw fail("The endpoint fixture changed trust instead of only the Hub address", {});
          state.expectedText = [state.url, previousUrl, sameCa];
          await dialog(await duplicate(), 1, "same-hub-new-address");
          const expected = { url: state.url, device_id: joined.network.device_id, user_id: userId, display_name: principal.display_name, direct: expectedDirect };
          const moved = await wait("Same Hub move restores the remembered person without another login", async () => ({
            ...await observe(cdp), surface: await observeSharedWorkSurface(cdp), calls: (await state.commands.snapshot(afterLogin)).calls,
          }), value => movedActivationAccepted(value, expected), 45_000);
          if (!isDeepStrictEqual(await deviceTrustIdentity(context), trustBefore)) throw fail("Endpoint move replaced the original PC identity or certificate", {});
          if (publicCertificatePem(await readFile(context.paths.config_file, "utf8")) !== publicCertificatePem(configBeforeRejection)) throw fail("Endpoint move rewrote the remembered CA representation", {});
          const sameWindow = await probeExactOwnedWindow({ ...native, candidate: mainWindow });
          if (!sameWindow.live || !sameWindow.exact_identity) throw fail("Endpoint move replaced the original Desktop window", sameWindow);
          await captureScenarioScreenshot({ cdp, sink, name: "join-config-same-person-new-address", owner });
          await sink.record("join-config-endpoint-moved", { old_url: previousUrl, new_url: state.url, user_id: userId,
            device_id: joined.network.device_id, trust_unchanged: true, direct_unchanged: true, login_commands: 0,
            surface: moved.surface, remembered_store_present: true,
            scope: "Controller PC; same authenticated Hub/device at a new actual listener, remembered ordinary person, native review and rejected different CA. Runner shutdown is a separate gate." }, { phase: "executing", owner });
        }
        if (state.resource.pageErrors().length || state.resource.provider.requests.some(r => r.method !== "GET")) throw fail("Activation raised a browser error or generated model content", {});
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        try { await captureScenarioScreenshot({ cdp, sink, name: `join-config-${mode}-failure`, owner }); } catch {}
        throw error;
      } finally {
        if (state.input) {
          try { await state.input.cleanup(); } catch (error) { state.inputFailures.push(error?.code ?? "input-cleanup"); }
          state.input = null;
        }
        if (state.commands) {
          try { await state.commands.remove(); } catch (error) { state.inputFailures.push(error?.code ?? "command-probe-cleanup"); }
          state.commands = null;
        }
      }
    },
    async requestGracefulExit(cdp) {
      if (state.nativeOwner) {
        try {
          const snapshot = await snapshotOwnedTopLevelWindows(state.nativeOwner);
          for (const candidate of snapshot.windows.filter(p => p.class_name === "#32770" && p.visible && p.is_root)) {
            await closeOwnedWindowForCleanup({ ...state.nativeOwner, candidate });
          }
        } catch { return { requested: false, reason: "join-config-native-dialog-cleanup-failed" }; }
      }
      return requestGracefulExit(cdp);
    },
    async quiesce() {
      state.close ??= state.resource ? await state.resource.close() : { pass: true, started: false };
      return { input: state.close.pass && state.inputFailures.length === 0 ? "pass" : "fail", resources: [{ kind: id, close: state.close, failures: state.inputFailures }] };
    },
    async cleanup() { return { input: state.close?.pass && state.inputFailures.length === 0 ? "pass" : "fail", resources: [] }; },
  });
}

export const createColdJoinConfigScenario = (options = {}) => createJoinConfigScenario("cold", options);
export const createWarmJoinConfigScenario = (options = {}) => createJoinConfigScenario("warm", options);

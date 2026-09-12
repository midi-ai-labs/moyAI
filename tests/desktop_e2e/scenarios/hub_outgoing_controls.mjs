import path from "node:path";
import crypto from "node:crypto";
import { pathToFileURL } from "node:url";
import { mkdir, readFile } from "node:fs/promises";
import { DesktopE2eError } from "../core/execution.mjs";
import { normalizeHubBrowserOptions, startHubBrowserResource } from "../drivers/hub_browser_resource.mjs";
import { WebviewInput } from "../drivers/webview_input.mjs";
import { startScriptedProvider, createChatToolContinuationProviderScript } from "../drivers/scripted_provider.mjs";
import { snapshotOwnedTopLevelWindows, selectFreshOwnedRootWindow, probeExactOwnedWindow,
  closeOwnedNativeDialog, captureOwnedWindowPng } from "../drivers/windows_native_input.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { acquireInteractiveShell } from "./shell_baseline.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";
import { providerChatToolContinuationFixtureConfig } from "./provider_chat_tool_continuation.mjs";
import { byId, action, wait, trustedClick, enrollDesktopFromHubBrowser, requestHubEnrollmentExit } from "./hub_browser_enrollment.mjs";
import { exerciseOutgoingPeerSearch } from "./outgoing_peer_search.mjs";

const OWNER = "scenario:hub.outgoing-controls", HUB = '[role="dialog"][data-modal="hub"]';
const PROMPT = "Ask the allowed GUI receiver fixture to make its one isolated receipt.";
const FINAL = "OUTGOING_GUI_REQUEST_SENT";
const JOB = "01M2AR00000000000000000001", PROFILE = "gui-receiver-project";
const hash = text => crypto.createHash("sha256").update(text).digest("hex");
const fail = (message, evidence = {}) => new DesktopE2eError("product", "outgoing-gui-control-mismatch", message, evidence);
export function outgoingFixtureArtifacts() {
  const text = "Isolated protocol receiver artifact.\n", sha256 = hash(text), name = "結果.txt";
  const files = [{ path: name, kind: "add", from_path: null, base_sha256: null, sha256, byte_length: Buffer.byteLength(text) }];
  return { manifest: { job_id: JOB, version: hash(JSON.stringify([JOB, files])), files },
    files: [{ path: name, sha256, text }] };
}
export function createOutgoingControlsScenario(options = {}) {
  const settings = normalizeHubBrowserOptions(options);
  const state = { resource: null, provider: null, receiver: null, participant: null, input: null, failures: [],
    close: null, jobState: "running", requestCount: 0, timer: null,
    nativeOwner: null, nativeBefore: null, nativeCandidate: null, importDispatched: false };
  return Object.freeze({ id: "hub.outgoing-controls", productOracle: "pass", manualGate: "pending", databaseRequired: true,
    async prepare({ context, sink, phase }) {
      state.resource = await startHubBrowserResource({ context, sink, phase, options: settings });
      const { page, hub } = state.resource;
      await page.goto(hub.url);
      await page.locator("#management-status").filter({ hasText: "Hub本体に接続中" }).waitFor();
      await page.locator('nav a[href="#device-network"]').click();
      await page.locator("#network-ip").fill("127.0.0.1");
      await page.locator("#network-port").fill(String(hub.networkPort));
      await page.locator("#network-start").click();
      await page.locator("#network-stop").waitFor();
      const network = await hub.observeNetwork();
      const { createDeviceParticipant } = await import(pathToFileURL(path.join(settings.hubRepository, "tests/browser/device-fixture.mjs")));
      state.participant = await createDeviceParticipant(network, "GUI receiver fixture");
      await page.locator('nav a[href="#clients"]').click();
      await page.locator("#network-clients-refresh").click();
      await page.locator(`[data-id="request:${state.participant.requestId}"]`).getByRole("button", { name: "GUI receiver fixtureの許可", exact: true }).click();
      const approved = await state.participant.collectApproval();
      if (approved.status !== "approved") throw fail("Protocol receiver was not approved");
      state.deviceId = approved.deviceId;
      const job = () => ({ job_id: JOB, profile_id: PROFILE, state: state.jobState, result: state.jobState === "interrupted" ? "GUI_STOP_RECEIPT" : null });
      const tools = ["delegate_task", "task_status", "cancel_task", "task_artifacts"].map(name => ({ name, description: `Isolated GUI ${name} fixture`, inputSchema: { type: "object", properties: {} } }));
      state.receiver = await state.participant.startMcpReceiver({ tools, call: async (name, args) => {
        if (name === "delegate_task") { if (++state.requestCount !== 1 || args.prompt !== "Create the isolated GUI receipt") throw new Error("Unexpected fixture delegation"); return job(); }
        if (args.job_id !== JOB) throw new Error("Fixture job identity mismatch");
        if (name === "cancel_task") { state.jobState = "interrupted"; return job(); }
        if (name === "task_status") return job();
        if (name === "task_artifacts") return outgoingFixtureArtifacts();
        throw new Error("Unexpected fixture tool");
      } });
      await state.participant.publish({ profile_id: PROFILE, name: "Isolated artifact project", endpoint: state.receiver.endpoint,
        mode: "agent", scope_id: PROFILE, enabled: true });
      await state.participant.presence();
      state.timer = setInterval(() => state.participant.presence().catch(() => state.failures.push("participant-presence")), 15000);
      const serverId = `hub-device-${hash(`${state.deviceId}\n${PROFILE}`)}`;
      state.provider = await startScriptedProvider({ responseBehavior: "hold_until_release",
        script: createChatToolContinuationProviderScript({ call: { prompt: PROMPT, name: "mcp_call",
          arguments: { server_id: serverId, tool_name: "delegate_task", arguments: { request_key: "gui-outgoing-once", prompt: "Create the isolated GUI receipt" } },
          outputMarker: JOB, responseText: FINAL } }) });
      await prepareDesktopFixture({ context, sink, phase, owner: OWNER,
        configText: providerChatToolContinuationFixtureConfig(state.provider.baseUrl).replace('[mcp]\nenabled = false', '[mcp]\nenabled = true').replace('access_mode = "default"', 'access_mode = "full_access"'),
        sentinelName: "E2E_OUTGOING.txt", sentinelText: "Keep the real GUI sender workspace unchanged.\n" });
      await mkdir(path.join(context.paths.workspace, ".git"));
    },
    async execute({ context, runtime, driver: cdp, sink }) {
      const { page, hub } = state.resource;
      const input = state.input = new WebviewInput(cdp, { probeId: "outgoing-controls" });
      await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: OWNER, screenshotStem: "outgoing-shell" });
      await input.installProbe();
      await page.locator('nav a[href="#device-network"]').click();
      const enrolled = await enrollDesktopFromHubBrowser({ resource: state.resource, context, runtime, cdp, input, sink, nativeState: state });
      await page.locator('nav a[href="#device-network"]').click();
      await page.locator("#network-rule-from").selectOption(`device:${enrolled.network.device_id}`);
      await page.locator("#network-rule-to").selectOption(`device:${state.deviceId}`);
      await page.locator("#network-rule-profile").selectOption(PROFILE);
      await page.getByRole("button", { name: "接続ルールを追加", exact: true }).click();
      await wait("The real Hub stores sender permission", () => hub.observeNetwork(), value => value.rules.some(rule => rule.from.id === enrolled.network.device_id && rule.to.id === state.deviceId && rule.profile_id === PROFILE));
      await trustedClick(input, cdp, byId("device-network-refresh"), sink);
      const peer = await wait("Actual Desktop lists the allowed receiver", () => invokeDesktopCommand(cdp, "device_network_projection"), value => value.peers?.some(row => row.device_id === state.deviceId && row.profile_id === PROFILE));
      await exerciseOutgoingPeerSearch({ cdp, input, sink, deviceId: state.deviceId, profileId: PROFILE });
      const key = JSON.stringify([state.deviceId, PROFILE]), useId = `device-network-use-${encodeURIComponent(key)}`;
      for (const selected of [true, false, true]) {
        await trustedClick(input, cdp, byId(useId), sink);
        await wait("Peer toggle persists the chosen state", () => invokeDesktopCommand(cdp, "device_network_projection"), value => value.peers?.find(row => row.device_id === state.deviceId && row.profile_id === PROFILE)?.selected === selected);
        await wait("Peer switch displays its actual saved state", () => cdp.evaluate(`document.getElementById(${JSON.stringify(useId)})?.getAttribute('aria-checked')`), value => value === String(selected));
      }
      await trustedClick(input, cdp, byId(`device-network-diagnose-${encodeURIComponent(key)}`), sink);
      const diagSelector = `[data-details-key=${JSON.stringify(`device-peer-diagnostic-${key}`)}]`;
      const diagnostic = await wait("Actual peer diagnostic completes every stage", () => cdp.evaluate(`(() => { const n=document.querySelector(${JSON.stringify(diagSelector)}); return [...n.querySelectorAll('[data-status]')].map(x=>({status:x.dataset.status,text:x.textContent})); })()`), value => value.length >= 3 && value.every(row => row.status === "pass"));
      await trustedClick(input, cdp, { selector: `${diagSelector} summary`, identity: { tag: "DETAILS", detailsKey: `device-peer-diagnostic-${key}` } }, sink);
      await captureScenarioScreenshot({ cdp, sink, name: "outgoing-peer-diagnostic", owner: OWNER });
      await trustedClick(input, cdp, action("close-overlay", `${HUB} .hub-modal-footer`), sink);
      await trustedClick(input, cdp, action("show-hub", "aside.sidebar"), sink);
      await trustedClick(input, cdp, byId("hub-tab-devices"), sink);
      if (await cdp.evaluate(`document.getElementById(${JSON.stringify(useId)})?.getAttribute('aria-checked')`) !== "true") throw fail("Selected peer did not survive close/reopen");
      await trustedClick(input, cdp, action("close-overlay", `${HUB} .hub-modal-footer`), sink);
      await trustedClick(input, cdp, byId("prompt", "TEXTAREA"), sink);
      await input.insertText(byId("prompt", "TEXTAREA"), PROMPT);
      await trustedClick(input, cdp, action("send", "section.composer"), sink);
      await wait("Real Desktop dispatches the single MCP call", () => state.provider.requestLedger,
        ledger => ledger.some(row => row.contract?.role === "chat_continuation" && row.contract.pass && row.response_phase === "held"));
      state.provider.releaseScriptRole("chat_continuation");
      await wait("Sender completes the canonical model turn", () => invokeDesktopCommand(cdp, "desktop_state"), value => value.run_status_key === "completed" && value.can_submit);
      await trustedClick(input, cdp, action("show-hub", "aside.sidebar"), sink);
      await trustedClick(input, cdp, byId("hub-tab-devices"), sink);
      const jobs = await wait("Actual outgoing reference is visible", () => invokeDesktopCommand(cdp, "device_network_jobs"), value => value.outgoing?.length === 1 && value.outgoing[0].job_id === JOB);
      const reference = jobs.outgoing[0].reference_id;
      await trustedClick(input, cdp, byId(`device-network-stop-${encodeURIComponent(`outgoing:${reference}`)}`), sink);
      const stopped = await wait("GUI stop observes the exact remote cancellation", () => invokeDesktopCommand(cdp, "device_network_jobs"), value => value.outgoing[0].state === "interrupted" && value.outgoing[0].stop_status === "confirmed");
      await captureScenarioScreenshot({ cdp, sink, name: "outgoing-stopped-before-artifacts", owner: OWNER });
      await sink.record("outgoing-stopped-dom", {stopped,dom:await cdp.evaluate(`document.querySelector('#device-network-jobs-list')?.outerHTML`)}, {phase:"executing",owner:OWNER});
      const details = `[data-details-key="device-artifacts-${reference}"]`;
      await trustedClick(input, cdp, { selector: `${details} summary`, identity: { tag: "DETAILS", detailsKey: `device-artifacts-${reference}` } }, sink);
      await trustedClick(input, cdp, byId(`device-network-artifacts-${reference}`), sink);
      await wait("Fetched immutable artifact is rendered", () => cdp.evaluate(`document.querySelector(${JSON.stringify(details)})?.textContent`), value => value?.includes("結果.txt") && value?.includes(outgoingFixtureArtifacts().manifest.version));
      await captureScenarioScreenshot({ cdp, sink, name: "outgoing-artifact-manifest", owner: OWNER });
      state.nativeOwner = { executionRoot: context.root, ownerPath: runtime.desktop_owner_path, expectedOwner: runtime.desktop_owner };
      state.nativeBefore = await snapshotOwnedTopLevelWindows(state.nativeOwner); state.importDispatched = true;
      await trustedClick(input, cdp, byId(`device-network-export-artifacts-${reference}`), sink);
      const picker = await wait("Actual artifact export opens a folder picker", async () => {
        const windows = await snapshotOwnedTopLevelWindows(state.nativeOwner);
        try { return selectFreshOwnedRootWindow(state.nativeBefore, windows, runtime.desktop_owner, { expectedClassName: "#32770" }); }
        catch (error) { if (error?.code === "native-window-cardinality" && error.evidence?.fresh_windows?.length === 0) return null; throw error; }
      }, Boolean);
      state.nativeCandidate = picker;
      const native = await captureOwnedWindowPng({ ...state.nativeOwner, candidate: picker });
      if (native.available) await sink.writeBytes("screenshots/outgoing-export-folder-picker.png", native.bytes);
      await closeOwnedNativeDialog({ ...state.nativeOwner, candidate: picker });
      await wait("Cancelled artifact folder picker closes", () => probeExactOwnedWindow({ ...state.nativeOwner, candidate: picker }), value => !value.live);
      state.nativeCandidate = null; state.importDispatched = false;
      await wait("Artifact cancel preserves the inspected manifest", () => cdp.evaluate(`document.querySelector(${JSON.stringify(details)})?.textContent`), value => value?.includes("書き出しをキャンセル") && value.includes(outgoingFixtureArtifacts().manifest.version));
      if (await readFile(path.join(context.paths.workspace, "E2E_OUTGOING.txt"), "utf8") !== "Keep the real GUI sender workspace unchanged.\n") throw fail("Sender sentinel changed");
      await sink.record("outgoing-controls-result", { sender: enrolled.network.device_id, receiver: state.deviceId, peer, diagnostic,
        job: stopped.outgoing[0], calls: state.receiver.calls(), provider: state.provider.requestLedger,
        scope: "Actual Desktop sender/Hub GUI and TLS; receiver is a bounded protocol fixture, not WinB. Export native picker cancelled; save confirmation remains separate." }, { phase: "executing", owner: OWNER });
      if (state.resource.pageErrors().length) throw fail("Hub browser page error", state.resource.pageErrors());
      await trustedClick(input, cdp, action("close-overlay", `${HUB} .hub-modal-footer`), sink);
      return { acquisition: "pass", oracle: "pass", manual: "pending" };
    },
    async requestGracefulExit(cdp) {
      if (state.input) { try { await state.input.cleanup(); } catch { state.failures.push("input-cleanup"); } state.input = null; }
      return requestHubEnrollmentExit(cdp, state);
    },
    async quiesce() {
      if (state.timer) { clearInterval(state.timer); state.timer = null; }
      if (state.input) { try { await state.input.cleanup(); } catch { state.failures.push("input-cleanup"); } state.input = null; }
      if (state.receiver) { try { await state.receiver.close(); } catch { state.failures.push("receiver-close"); } }
      const browser = state.resource ? await state.resource.close() : { pass: true };
      const provider = state.provider ? await state.provider.close() : { pass: true };
      state.close = { pass: browser.pass && provider.pass && state.failures.length === 0, browser, provider, failures: [...state.failures] };
      return { input: state.close.pass ? "pass" : "fail", resources: [{ kind: "outgoing-controls", ...state.close }] };
    },
    async cleanup() { return { input: state.close?.pass ? "pass" : "fail", resources: [] }; },
  });
}

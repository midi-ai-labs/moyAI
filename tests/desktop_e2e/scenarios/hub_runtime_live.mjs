import path from "node:path";
import { createServer } from "node:net";
import { randomBytes } from "node:crypto";
import { readFile } from "node:fs/promises";

import { HubTauriResource } from "../drivers/hub_tauri_resource.mjs";
import { WebviewInput, assertTrustedTextInsertion } from "../drivers/webview_input.mjs";
import { DesktopCommandProbe, assertExactDesktopCommandSequence } from "../drivers/desktop_command_probe.mjs";
import { scriptedProviderPortIsFetchSafe } from "../drivers/scripted_provider.mjs";
import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";
import { trustedClick, observeSideChatQuoteSurface } from "./side_chat_quote.mjs";
import { PROVIDER_OPENAI_COMPATIBLE_PROMPT, observeProviderConnectionLiveSurface,
  liveCurrentTimeTerminalDecision, currentTimeFromCompletedProjection,
  providerConnectionLiveSideChatQuestion, providerConnectionLiveSideChatAnswerAccepted } from "./provider_connection_live.mjs";
import { case52Stage5MainSnapshot, case52Stage5MainMatches } from "./case5_2_side_chat.mjs";

const OWNER = "scenario:manual.hub-runtime";
const DIRECT_ENDPOINT = "http://127.0.0.1:9/v1";
const DIRECT_MODEL = "direct-settings-preserved";
const byId = (id, tag = "INPUT") => ({ selector: `#${id}`, identity: { tag, id } });
const action = (name, scope = "") => ({ selector: `${scope} button[data-action="${name}"]`.trim(), identity: { tag: "BUTTON", action: name } });
const fail = (message, evidence) => new DesktopE2eError("product", "hub-runtime-mismatch", message, evidence);

export function normalizeHubRuntimeOptions(options) {
  if (!options || typeof options !== "object" || Array.isArray(options)
    || JSON.stringify(Object.keys(options).sort()) !== JSON.stringify(["hub_binary", "model", "provider_base_url"])) throw new TypeError("Hub runtime config requires hub_binary, model and provider_base_url");
  if (typeof options.hub_binary !== "string" || !path.isAbsolute(options.hub_binary)) throw new TypeError("hub_binary must be absolute");
  const endpoint = new URL(options.provider_base_url);
  if (!["http:", "https:"].includes(endpoint.protocol) || endpoint.username || endpoint.password || endpoint.search || endpoint.hash) throw new TypeError("provider_base_url must be a credential-free HTTP endpoint");
  if (typeof options.model !== "string" || !options.model.trim() || options.model.length > 256) throw new TypeError("model is invalid");
  return { hubBinary: options.hub_binary, providerBaseUrl: endpoint.href.replace(/\/$/, ""), model: options.model };
}

function fixtureConfig() {
  return `[model]
base_url = "${DIRECT_ENDPOINT}"
model = "${DIRECT_MODEL}"
provider_profile = "openai_compatible"
connect_timeout_ms = 10000
request_timeout_ms = 180000
max_retries = 0
context_window = 32768
supports_tools = true
supports_images = false
parallel_tool_calls = false

[side_chat]
base_url = "${DIRECT_ENDPOINT}"
model = "${DIRECT_MODEL}"
provider_profile = "openai_compatible"
connect_timeout_ms = 10000
request_timeout_ms = 180000
max_retries = 0
context_window = 32768

[permissions]
access_mode = "default"
[multi_agent]
enabled = false
mode = "explicit_request_only"
max_concurrent_agents = 2
max_concurrent_model_requests = 1
[docling]
enabled = false
[mcp]
enabled = false
`;
}

async function until(label, sample, accept, timeoutMs = 20_000) {
  try { return (await waitForObservation({ label, timeoutMs, pollMs: 150, sample, accept, retrySampleErrors: false })).value; }
  catch (error) {
    if (error?.code === "observation-timeout") throw fail(label, error.evidence);
    throw error;
  }
}

async function focusByTab(input, cdp, locator) {
  await until("Control enabled after state update", () => cdp.evaluate(`(() => {
    const nodes=document.querySelectorAll(${JSON.stringify(locator.selector)});
    if(nodes.length!==1) return false;
    const element=nodes[0];
    return !element.matches(':disabled') && element.getAttribute('aria-disabled')!=='true'
      && element.getClientRects().length>0;
  })()`), (enabled) => enabled === true);
  for (let index = 0; index < 180; index += 1) {
    if (await cdp.evaluate(`(() => {const nodes=document.querySelectorAll(${JSON.stringify(locator.selector)});return nodes.length===1&&document.activeElement===nodes[0]})()`)) return;
    await input.pressKey("Tab");
  }
  throw new DesktopE2eError("harness", "hub-runtime-control-unreachable", "Control was not reached by keyboard", locator);
}

async function click(input, cdp, locator, sink) {
  await focusByTab(input, cdp, locator);
  const evidence = await trustedClick(input, locator);
  await sink.record("hub-runtime-trusted-click", { locator, evidence }, { phase: "executing", owner: OWNER });
}

async function openManualConnection(input, cdp, detailsId, sink) {
  const opened = () => cdp.evaluate(`(() => {
    const nodes = document.querySelectorAll(${JSON.stringify(`#${detailsId}`)});
    return nodes.length === 1 && nodes[0].open === true;
  })()`);
  if (!await opened()) await click(input, cdp, {
    selector: `#${detailsId} > summary`, identity: { tag: "DETAILS", detailsKey: detailsId },
  }, sink);
  await until("Manual connection controls opened", opened, value => value === true);
}

async function edit(input, cdp, locator, text, sink, { secret = false } = {}) {
  await focusByTab(input, cdp, locator);
  await input.keyDown("Control");
  try { await input.pressKey("a"); } finally { await input.keyUp("Control"); }
  await input.pressKey("Backspace");
  const start = (await input.snapshotProbe()).sequence;
  try {
    await input.insertText(locator, text);
    const insertion = assertTrustedTextInsertion(await input.snapshotProbe(start), { afterSequence: start, identity: locator.identity, text });
    await sink.record("hub-runtime-trusted-edit", { locator, insertion: secret ? "[redacted; trusted]" : insertion }, { phase: "executing", owner: OWNER });
  } catch (error) {
    if (secret) throw new DesktopE2eError("harness", "hub-runtime-secret-input", "Token input could not be verified; evidence redacted");
    throw error;
  }
}

async function selectValue(input, cdp, id, value, sink) {
  const locator = byId(id, "SELECT");
  await focusByTab(input, cdp, locator);
  const values = await cdp.evaluate(`Array.from(document.getElementById(${JSON.stringify(id)}).options, option=>option.value)`);
  const index = values.indexOf(value);
  if (index < 0) throw fail("Expected select option missing", { id, value, values });
  await input.pressKey("Home");
  for (let step = 0; step < index; step += 1) await input.pressKey("ArrowDown");
  await input.pressKey("Tab");
  await until("Native select committed", () => cdp.evaluate(`document.getElementById(${JSON.stringify(id)}).value`), (selected) => selected === value);
  await sink.record("hub-runtime-native-select", { id, value }, { phase: "executing", owner: OWNER });
}

const hubSnapshot = (cdp) => cdp.evaluate("window.__TAURI_INTERNALS__.invoke('hub_snapshot')");
const desktopHub = (cdp) => cdp.evaluate("window.__TAURI_INTERNALS__.invoke('hub_projection')");

async function chooseHubPort() {
  for (let attempt = 0; attempt < 16; attempt += 1) {
    const listener = createServer();
    await new Promise((resolve, reject) => { listener.once("error", reject); listener.listen(0, "127.0.0.1", resolve); });
    const port = listener.address().port;
    await new Promise((resolve) => listener.close(resolve));
    if (scriptedProviderPortIsFetchSafe(port)) return port;
  }
  throw new Error("No Fetch-safe loopback port acquired");
}

export function createHubRuntimeLiveScenario(rawOptions = {}) {
  const options = normalizeHubRuntimeOptions(rawOptions);
  const state = { hub: null, hubs: [], inputs: [], probe: null, cleanup: null, primaryError: null, inputFailures: [] };
  return {
    id: "manual.hub-runtime", productOracle: "pass", manualGate: "not_required", databaseRequired: true,
    requestGracefulExit,
    async prepare({ context, sink, phase }) {
      await prepareDesktopFixture({ context, sink, phase, owner: OWNER, configText: fixtureConfig(), sentinelName: "HUB_RUNTIME_SENTINEL.txt" });
      const response = await fetch(`${options.providerBaseUrl}/models`, { signal: AbortSignal.timeout(15_000) });
      const listing = await response.json();
      if (!response.ok || !listing.data?.some((row) => row.id === options.model)) throw new DesktopE2eError("environment", "hub-runtime-model-unavailable", "The operator's hosted model is unavailable");
      await sink.record("hub-runtime-external-provider", { ...options, models: listing.data.map((row) => ({ id: row.id, owned_by: row.owned_by })), lifecycle: "external-unmanaged", model_mutations: 0 }, { phase, owner: OWNER });
    },
    async execute({ context, driver: cdp, sink }) {
      const token = randomBytes(32).toString("hex");
      const configBefore = await readFile(context.paths.config_file, "utf8");
      try {
        await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: OWNER, screenshotStem: "hub-runtime-desktop-ready" });
        state.hub = new HubTauriResource();
        state.hubs.push(state.hub);
        await state.hub.start({ context, sink, binary: options.hubBinary });
        const hubCdp = state.hub.driver;
        const hubInput = new WebviewInput(hubCdp, { probeId: "actual-hub-runtime" });
        const desktopInput = new WebviewInput(cdp, { probeId: "desktop-hub-runtime" });
        for (const input of [hubInput, desktopInput]) { state.inputs.push(input); await input.installProbe(); }
        await until("Hub management ready", () => hubSnapshot(hubCdp), (value) => value.store?.catalog?.models?.length === 0 && !value.server.running);
        await edit(hubInput, hubCdp, byId("endpoint"), options.providerBaseUrl, sink);
        await hubInput.pressKey("Tab");
        await until("oMLX model candidate discovered", () => hubCdp.evaluate("Array.from(document.querySelector('#model').options, x=>x.value)"), (values) => values.includes(options.model));
        await selectValue(hubInput, hubCdp, "model", options.model, sink);
        await edit(hubInput, hubCdp, byId("label"), "oMLX Qwen · Hub実行確認", sink);
        // Host metadata may omit tool support. This is an explicit administrator declaration.
        await click(hubInput, hubCdp, byId("allow-tools"), sink);
        await click(hubInput, hubCdp, byId("register", "BUTTON"), sink);
        const registered = await until("Hub model registered", () => hubSnapshot(hubCdp), (value) => value.store.catalog.models.length === 1);
        const model = registered.store.catalog.models[0];
        if (!model.capabilities.includes("tools")) throw fail("Hub model requires an explicit tools capability for the live tool round", model);
        const port = await chooseHubPort();
        const hubBase = `http://127.0.0.1:${port}`;
        await openManualConnection(hubInput, hubCdp, "manual-server-connection", sink);
        await edit(hubInput, hubCdp, byId("bind"), `127.0.0.1:${port}`, sink);
        await edit(hubInput, hubCdp, byId("token"), token, sink, { secret: true });
        await click(hubInput, hubCdp, byId("enable-gateway"), sink);
        await click(hubInput, hubCdp, byId("start-server", "BUTTON"), sink);
        const started = await until("Hub service and gateway started", () => hubSnapshot(hubCdp), (value) => value.server.running && value.gateway.enabled && value.gateway.ready);
        await sink.record("hub-runtime-service-started", started, { phase: "executing", owner: OWNER });
        await captureScenarioScreenshot({ cdp: hubCdp, sink, name: "hub-runtime-management", owner: OWNER });

        await click(desktopInput, cdp, action("show-hub", "aside.sidebar"), sink);
        await click(desktopInput, cdp, byId("hub-tab-models", "BUTTON"), sink);
        await until("Desktop Hub overlay ready", () => desktopHub(cdp), (value) => value.status === "disconnected");
        await openManualConnection(desktopInput, cdp, "hub-manual-connection", sink);
        await edit(desktopInput, cdp, byId("hub-endpoint"), hubBase, sink);
        await edit(desktopInput, cdp, byId("hub-label"), "LYNX local live", sink);
        await edit(desktopInput, cdp, byId("hub-token"), token, sink, { secret: true });
        await click(desktopInput, cdp, action("hub-connect"), sink);
        await until("Desktop connected to actual Hub", () => desktopHub(cdp), (value) => value.status === "connected" && value.catalog?.models.some((row) => row.id === model.id));
        for (const [contextName, actionName] of [["main", "main"], ["side_chat", "side"]]) {
          await click(desktopInput, cdp, byId(`hub-${contextName}-model-${model.id}`), sink);
          await click(desktopInput, cdp, action(`hub-save-${actionName}`), sink);
          await until(`${contextName} acknowledged`, () => desktopHub(cdp), (value) => value[`${contextName}_confirmation`] === "confirmed");
          await click(desktopInput, cdp, action(`hub-${actionName}-hub`), sink);
          await until(`${contextName} Hub route selected`, () => desktopHub(cdp), (value) => value[`${contextName}_mode`] === "hub");
        }
        await captureScenarioScreenshot({ cdp, sink, name: "hub-runtime-desktop-routes", owner: OWNER });
        await click(desktopInput, cdp, action("close-overlay", '[data-modal="hub"] .hub-modal-footer'), sink);
        // The initial common acquisition already qualified this shell. A connected Hub
        // keeps ordinary status polling active even while the composer is idle.
        await until("Hub panel closed and composer ready", () => observeSideChatQuoteSurface(cdp),
          (surface) => surface.projection.overlay === "none" && !surface.projection.busy
            && surface.main.prompt_visible && surface.main.prompt_enabled
            && surface.projection.hub?.main_mode === "hub");
        state.probe = new DesktopCommandProbe(cdp, { probeId: "hub-runtime-commands", commands: ["submit_prompt", "cancel_run", "submit_side_chat", "cancel_side_chat"] });
        await state.probe.install();
        await edit(desktopInput, cdp, byId("prompt", "TEXTAREA"), PROVIDER_OPENAI_COMPATIBLE_PROMPT, sink);
        const mainDraft = (await until("Main draft admitted", () => observeSideChatQuoteSurface(cdp),
          (surface) => surface.main.prompt_value === PROVIDER_OPENAI_COMPATIBLE_PROMPT && surface.main.send_enabled)).projection;
        const expectedMain = { command: "submit_prompt", args: { text: PROVIDER_OPENAI_COMPATIBLE_PROMPT,
          expectedTarget: structuredClone(mainDraft.draft_target), expectedRunTarget: structuredClone(mainDraft.run_target) } };
        await click(desktopInput, cdp, action("send"), sink);
        const activeHub = await until("Hub records actual Main execution", () => hubSnapshot(hubCdp), (value) => value.clients.some((client) => client.contexts.some((entry) => entry.context_id === "main" && entry.active_model_id === model.id)));
        await sink.record("hub-runtime-main-active", activeHub, { phase: "executing", owner: OWNER });
        await click(hubInput, hubCdp, { selector: 'a[href="#clients"]', identity: { tag: "A", href: "#clients" } }, sink);
        await until("Hub device grid shows the assigned context", () => hubCdp.evaluate("document.querySelector('#client-rows')?.innerText ?? ''"),
          (text) => text.includes("LYNX local live") && text.includes("割当中"));
        await captureScenarioScreenshot({ cdp: hubCdp, sink, name: "hub-runtime-active-device", owner: OWNER });
        const terminal = await until("Hub Main tool round completed", () => observeProviderConnectionLiveSurface(cdp), (surface) => {
          const decision = liveCurrentTimeTerminalDecision(surface, { allowIdleHubPolling: true });
          if (decision === "fail") throw fail("Hub Main tool round failed", surface);
          return decision === "pass";
        }, 420_000);
        const time = currentTimeFromCompletedProjection(terminal.projection);
        await sink.record("hub-runtime-main-completed", { time, surface: terminal }, { phase: "executing", owner: OWNER });
        await captureScenarioScreenshot({ cdp, sink, name: "hub-runtime-main-completed", owner: OWNER });
        const mainSettled = await until("Main canonical state settled", () => observeSideChatQuoteSurface(cdp),
          (surface) => case52Stage5MainMatches(surface, case52Stage5MainSnapshot(surface)));
        const baseline = case52Stage5MainSnapshot(mainSettled);
        await click(desktopInput, cdp, action("show-side-chat-pane"), sink);
        await until("Side Chat ready", () => observeSideChatQuoteSurface(cdp), (value) => value.side.pane_visible && value.side.prompt_enabled);
        const question = providerConnectionLiveSideChatQuestion(time);
        await edit(desktopInput, cdp, byId("side-chat-prompt", "TEXTAREA"), question, sink);
        const sideDraft = (await until("Side draft admitted", () => observeSideChatQuoteSurface(cdp),
          (surface) => case52Stage5MainMatches(surface, baseline) && surface.projection.side_chat?.draft_text === question
            && surface.projection.side_chat.draft_quote === null && surface.side.prompt_value === question && surface.side.send_enabled)).projection.side_chat;
        const expectedSide = { command: "submit_side_chat", args: { ownerSessionId: sideDraft.owner_session_id,
          chatId: sideDraft.chat_id, expectedGeneration: sideDraft.generation, expectedDraftRevision: sideDraft.draft_revision,
          expectedOwnerAppendPosition: sideDraft.context_as_of_append_position, quote: null, text: question.trim() } };
        await click(desktopInput, cdp, action("send-side-chat"), sink);
        const side = await until("Hub Side response completed", () => observeSideChatQuoteSurface(cdp), (surface) => {
          if (!case52Stage5MainMatches(surface, baseline)) throw fail("Side request changed the Main owner", surface);
          const messages = surface.projection.side_chat?.messages ?? [];
          if (messages.some((row) => row.role === "error")) throw fail("Hub Side response failed", surface);
          return messages.length === 2 && messages[1].role === "assistant"
            && providerConnectionLiveSideChatAnswerAccepted(messages[1].content, time)
            && surface.side.messages.length === 2 && surface.side.stop_enabled === false;
        }, 420_000);
        await sink.record("hub-runtime-side-completed", side, { phase: "executing", owner: OWNER });
        await captureScenarioScreenshot({ cdp, sink, name: "hub-runtime-side-completed", owner: OWNER });
        const finishedHub = await until("Hub execution released", () => hubSnapshot(hubCdp), (value) => value.clients.every((client) => client.contexts.every((entry) => entry.active_model_id === null))
          && value.gateway.running === 0 && value.gateway.reserved === 0 && !value.gateway.uncertain);
        const configAfter = await readFile(context.paths.config_file, "utf8");
        const settings = await readFile(path.join(context.paths.config, "hub-settings.json"), "utf8");
        if (configAfter !== configBefore || settings.includes(token) || settings.includes(options.providerBaseUrl) || settings.includes("/r/")) throw fail("Direct settings changed or Hub credentials/targets persisted", { direct_unchanged: configBefore === configAfter });
        const commands = assertExactDesktopCommandSequence(await state.probe.snapshot(), { expected: [expectedMain, expectedSide] });
        await sink.record("hub-runtime-final-contract", { finishedHub, commands, direct_config_unchanged: true, hub_preferences: JSON.parse(settings) }, { phase: "executing", owner: OWNER });

        await click(desktopInput, cdp, action("show-hub", "aside.sidebar"), sink);
        await click(desktopInput, cdp, byId("hub-tab-models", "BUTTON"), sink);
        await click(desktopInput, cdp, action("hub-disconnect"), sink);
        await until("Desktop explicitly disconnected", () => desktopHub(cdp), value => value.status === "disconnected");
        await hubInput.cleanup();
        state.inputs = state.inputs.filter(input => input !== hubInput);
        const firstClose = await state.hub.close();
        await sink.record("hub-runtime-first-window-close", firstClose, { phase: "executing", owner: OWNER });
        if (firstClose.input !== "pass") throw fail("Normal Hub close left an unclean execution marker or process", firstClose);

        // Reopen the same catalog with a fresh WebView and process Job. No inference is
        // repeated: this checks normal window close, retained identity and server restart.
        const restarted = new HubTauriResource();
        state.hub = restarted;
        state.hubs.push(restarted);
        await restarted.start({ context, sink, binary: options.hubBinary, generation: 2 });
        const restartCdp = restarted.driver;
        const restartInput = new WebviewInput(restartCdp, { probeId: "actual-hub-restart" });
        state.inputs.push(restartInput);
        await restartInput.installProbe();
        const restored = await until("Hub retained catalog and clean shutdown", () => hubSnapshot(restartCdp), value =>
          value.store.hub_id === finishedHub.store.hub_id && value.store.revision === finishedHub.store.revision
          && value.store.catalog.models.length === 1 && value.store.catalog.models[0].id === model.id
          && !value.server.running && !value.gateway.enabled && !value.gateway.uncertain && value.clients.length === 0);
        await openManualConnection(restartInput, restartCdp, "manual-server-connection", sink);
        await edit(restartInput, restartCdp, byId("bind"), `127.0.0.1:${port}`, sink);
        await edit(restartInput, restartCdp, byId("token"), randomBytes(32).toString("hex"), sink, { secret: true });
        await click(restartInput, restartCdp, byId("enable-gateway"), sink);
        await click(restartInput, restartCdp, byId("start-server", "BUTTON"), sink);
        const restartedServer = await until("Gateway starts after normal window close", () => hubSnapshot(restartCdp), value =>
          value.server.running && value.gateway.ready && !value.gateway.uncertain && value.gateway.running === 0);
        await sink.record("hub-runtime-restarted", { restored, restartedServer }, { phase: "executing", owner: OWNER });
        await captureScenarioScreenshot({ cdp: restartCdp, sink, name: "hub-runtime-restarted", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) { state.primaryError = error; throw error; }
      finally {
        for (const input of state.inputs) { try { await input.cleanup(); } catch (error) { state.inputFailures.push(error.message); } }
        try { await state.probe?.remove(); } catch (error) { state.inputFailures.push(error.message); }
        if (!state.primaryError && state.inputFailures.length) throw new DesktopE2eError("harness", "hub-runtime-input-cleanup", "Input cleanup failed", state.inputFailures);
      }
    },
    async quiesce({ sink }) {
      if (state.cleanup) return state.cleanup;
      const failures = state.inputFailures;
      const hubs = [];
      for (const hub of state.hubs) hubs.push(await hub.close());
      state.cleanup = { input: failures.length === 0 && hubs.every(hub => hub.input === "pass") ? "pass" : "fail", failures, hubs, external_provider_mutations: 0 };
      await sink.record("hub-runtime-resource-cleanup", state.cleanup, { phase: "cleaning", owner: OWNER });
      return state.cleanup;
    },
    async cleanup() { return { input: state.cleanup?.input ?? "fail" }; },
  };
}

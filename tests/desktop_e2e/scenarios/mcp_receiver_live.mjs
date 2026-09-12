import path from "node:path";
import { pathToFileURL } from "node:url";
import { DesktopE2eError } from "../core/execution.mjs";
import { normalizeHubBrowserOptions, startHubBrowserResource } from "../drivers/hub_browser_resource.mjs";
import { WebviewInput } from "../drivers/webview_input.mjs";
import { DesktopCommandProbe, assertExactDesktopCommandSequence } from "../drivers/desktop_command_probe.mjs";
import { startScriptedProvider, createChatToolContinuationProviderScript, SCRIPTED_PROVIDER_MODEL_ID,
  SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT, SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_RESPONSE } from "../drivers/scripted_provider.mjs";
import { snapshotOwnedTopLevelWindows, selectFreshOwnedRootWindow, probeExactOwnedWindow,
  closeOwnedNativeDialog, captureOwnedWindowPng } from "../drivers/windows_native_input.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { acquireInteractiveShell } from "./shell_baseline.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";
import { providerChatToolContinuationFixtureConfig } from "./provider_chat_tool_continuation.mjs";
import { byId, action, wait, trustedClick, enrollDesktopFromHubBrowser, requestHubEnrollmentExit } from "./hub_browser_enrollment.mjs";
import { receiverPermissionScenarioId, receiverPermissionPlan, prepareReceiverPermissionMain,
  executeReceiverPermissionDecision, observeReceiverPermissionSurface, receiverPermissionTerminalFailures } from "./mcp_receiver_permission.mjs";
import { readFile } from "node:fs/promises";
import { providerRestartFixtureConfig } from "./provider_restart.mjs";
import { mcpPaginationProviderOptions, exerciseMcpHistoryPagination } from "./mcp_history_pagination.mjs";
import { exerciseReceiverRecommendation, exerciseReceiverLocalDiagnostics } from "./mcp_receiver_connection_controls.mjs";

const OWNER = "scenario:mcp.receiver-live";
const HUB = '[role="dialog"][data-modal="hub"]';
const HISTORY = '[role="dialog"][data-modal="mcp_history"]';
const fail = (message, evidence = {}) => new DesktopE2eError("product", "mcp-receiver-live-mismatch", message, evidence);
export function receiverPublication(snapshot, deviceId, profileId) {
  const devices = snapshot?.devices?.filter(row => row.device_id === deviceId) ?? [];
  if (devices.length !== 1) return null;
  const device = devices[0];
  const matches = device?.publications?.filter(row => row.profile_id === profileId && row.enabled && row.mode === "agent") ?? [];
  return device?.online && matches.length === 1 ? matches[0] : null;
}
export function receiverActivityMatches(value, running) {
  if (value?.activity?.unavailable !== false) return false;
  return running ? value?.activity?.running === 1 && value?.stripCount === 1 && value?.runningBadge === 1 && value?.label?.includes("MCP実行中")
    : value?.activity?.running === 0 && value?.activity?.waiting === 0 && value?.activity?.awaiting_approval === 0
      && value?.activity?.cancelling === 0 && value?.stripCount === 0;
}
export function receiverCompletionOutcome(pageErrors) {
  if (pageErrors.length) throw fail("Hub browser reported page errors", { page_errors: [...pageErrors] });
  return { acquisition: "pass", oracle: "pass", manual: "pending" };
}
export function receiverHistoryReloadReady(value, jobId, terminalState = "completed") {
  return value?.dialogCount === 1 && value.page === "execution:0" && value.detailOwner === "execution:"
    && value.refresh?.count === 1 && value.refresh.disabled === false && value.refresh.ariaDisabled === "false"
    && value.selectedRows === 0 && value.listError === "" && value.rows?.length === 1
    && value.rows[0].id === jobId && value.rows[0].state === terminalState && value.rows[0].pressed === "false";
}
export function receiverStoppedWhileProviderHeld(job, ledger) {
  if (!Array.isArray(ledger)) return false;
  const held=ledger?.filter(row=>row.route==="chat_completions"&&row.contract?.role==="chat_continuation")??[];
  return job?.state==="interrupted"&&held.length===1&&held[0].contract.pass===true&&held[0].response_phase==="held"
    && !ledger.some(row=>row.contract?.role==="chat_continuation"&&row.response_phase==="completed");
}
async function activity(cdp) {
  const projection = await invokeDesktopCommand(cdp, "desktop_state");
  const dom = await cdp.evaluate(`(() => ({ stripCount: document.querySelectorAll('.mcp-run-strip').length,
    runningBadge: document.querySelectorAll('.mcp-run-strip [data-mcp-activity="running"]').length,
    label: document.querySelector('.mcp-run-strip')?.textContent ?? '',
    color: (() => { const node = document.querySelector('.mcp-activity-badge'); return node ? getComputedStyle(node).color : null; })() }))()`);
  return { activity: projection.mcp_activity, ...dom };
}
async function selectFirst(input, cdp, id, value, sink) {
  const target = byId(id, "SELECT");
  if (await cdp.evaluate(`document.getElementById(${JSON.stringify(id)})?.value`) === value) return;
  await trustedClick(input, cdp, target, sink);
  const index = await cdp.evaluate(`Array.from(document.getElementById(${JSON.stringify(id)}).options).findIndex(row => row.value === ${JSON.stringify(value)})`);
  if (index < 0) throw fail("Requested receiver option is missing", { id, value });
  await input.pressKey("Home");
  for (let count = 0; count < index; ++count) await input.pressKey("ArrowDown");
  await input.pressKey("Enter");
  await wait("Receiver selection reflects trusted keyboard input", () => cdp.evaluate(`document.getElementById(${JSON.stringify(id)})?.value`), current => current === value);
}

export function createMcpReceiverLiveScenario(options = {}) {
  return createMcpReceiverScenario(options,"live");
}
export function createMcpReceiverStopScenario(options = {}) {
  return createMcpReceiverScenario(options,"stop");
}
export function createMcpHistoryPaginationScenario(options = {}) {
  return createMcpReceiverScenario(options, "history-pagination");
}
export function createMcpReceiverPermissionScenario({ decision, ...options } = {}) {
  receiverPermissionScenarioId(decision);
  return createMcpReceiverScenario(options, decision);
}
function createMcpReceiverScenario(options, variant) {
  const stopFromHistory = variant === "stop";
  const historyPagination = variant === "history-pagination";
  const permissionDecision = ["approved", "denied", "abort"].includes(variant) ? variant : null;
  const id = historyPagination ? "mcp.history-pagination" : permissionDecision ? receiverPermissionScenarioId(permissionDecision) : `mcp.receiver-${variant}`;
  const OWNER=`scenario:${id}`;
  const settings = normalizeHubBrowserOptions(options);
  const state = { resource: null, provider: null, sender: null, peer: null, jobId: null, settled: false,
    released: false, input: null, failures: [], close: null, protocolCalls: [],
    nativeOwner: null, nativeBefore: null, nativeCandidate: null, importDispatched: false, commands:null,
    permissionPlan: null, permissionMain: null };
  return Object.freeze({
    id, productOracle: "pass", manualGate: "pending", databaseRequired: true,
    async prepare({ context, sink, phase }) {
      if (permissionDecision) state.permissionPlan = receiverPermissionPlan(context.root, permissionDecision);
      state.provider = await startScriptedProvider(historyPagination ? mcpPaginationProviderOptions() : { expectedPrompt: state.permissionPlan?.call.prompt ?? SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT,
        responseBehavior: "hold_until_release", script: createChatToolContinuationProviderScript(state.permissionPlan ? { call: state.permissionPlan.call } : {}) });
      await prepareDesktopFixture({ context, sink, phase, owner: OWNER,
        configText: historyPagination ? providerRestartFixtureConfig(state.provider.baseUrl) : providerChatToolContinuationFixtureConfig(state.provider.baseUrl),
        sentinelName: "E2E_MCP_RECEIVER.txt", sentinelText: "MCP receiver GUI fixture.\n" });
      state.resource = await startHubBrowserResource({ context, sink, phase, options: settings });
    },
    async execute({ context, runtime, driver: cdp, sink }) {
      const resource = state.resource, { page, hub } = resource;
      const input = state.input = new WebviewInput(cdp, { probeId: "mcp-receiver-live" });
      await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: OWNER, screenshotStem: "mcp-receiver-shell" });
      await input.installProbe();
      await page.goto(hub.url);
      await page.locator("#management-status").filter({ hasText: "Hub本体に接続中" }).waitFor();
      if (!historyPagination) {
      await page.locator("#endpoint").fill(state.provider.baseUrl);
      await page.locator("#profile").selectOption("openai_compatible_chat");
      await page.locator("#discover").click();
      await page.locator(`#model option[value="${SCRIPTED_PROVIDER_MODEL_ID}"]`).waitFor({ state: "attached" });
      await page.locator("#model").selectOption(SCRIPTED_PROVIDER_MODEL_ID);
      await page.locator("#label").fill("MCP receiver scripted model");
      await page.locator("#allow-tools").check();
      await page.locator("#register").click();
      await page.locator("#model-rows tr").filter({ hasText: "MCP receiver scripted model" }).waitFor();
      }
      await page.locator('nav a[href="#device-network"]').click();
      await page.locator("#network-ip").fill("127.0.0.1");
      await page.locator("#network-port").fill(String(hub.networkPort));
      await page.locator("#network-start").click();
      await page.locator("#network-stop").waitFor();
      const network = await hub.observeNetwork();
      const enrolled = await enrollDesktopFromHubBrowser({ resource, context, runtime, cdp, input, sink, nativeState: state });
      if (!historyPagination) {
      await trustedClick(input, cdp, byId("hub-tab-models"), sink);
      const catalog = await wait("Desktop sees the scripted Hub model", () => invokeDesktopCommand(cdp, "hub_projection"), value => value.status === "connected" && value.catalog?.models.length === 1);
      if (permissionDecision === "denied") await exerciseReceiverRecommendation({ cdp, input, sink, owner: OWNER });
      const model = catalog.catalog.models[0].id;
      const choice = byId(`hub-main-model-${model}`, "INPUT");
      if (!(await cdp.evaluate(`document.querySelector(${JSON.stringify(choice.selector)})?.checked`))) await trustedClick(input, cdp, choice, sink);
      await trustedClick(input, cdp, action("hub-save-main"), sink);
      await wait("Desktop confirms the receiver model", () => invokeDesktopCommand(cdp, "hub_projection"), value => value.main_confirmation === "confirmed");
      await trustedClick(input, cdp, action("hub-main-hub"), sink);
      }
      await trustedClick(input, cdp, byId("hub-tab-devices"), sink);
      await selectFirst(input, cdp, "device-network-target", "temp", sink);
      if (historyPagination && !await cdp.evaluate(`document.getElementById('device-network-model-details')?.open`)) {
        await trustedClick(input, cdp, { selector: "#device-network-model-details > summary", identity: { tag: "DETAILS", detailsKey: "device-network-model-details" } }, sink);
      }
      await selectFirst(input, cdp, "device-network-model", historyPagination ? "direct" : "hub", sink);
      if (permissionDecision) await selectFirst(input, cdp, "device-network-access", "default", sink);
      const confirmation = byId("device-network-receiver-confirmed", "INPUT");
      if (!(await cdp.evaluate(`document.querySelector(${JSON.stringify(confirmation.selector)})?.checked`))) await trustedClick(input, cdp, confirmation, sink);
      await trustedClick(input, cdp, byId("device-network-receiver-on"), sink);
      const receiving = await wait("Actual Desktop starts its mTLS receiver", () => invokeDesktopCommand(cdp, "device_network_projection"), value => value.receiver?.enabled && value.receiver.status === "receiving" && value.receiver.endpoint, 45_000);
      const publication = await wait("Actual Desktop announces the exact receiving profile", () => hub.observeNetwork(), snapshot => receiverPublication(snapshot, enrolled.network.device_id, receiving.receiver.profile_id));
      if (!receiverPublication(publication, enrolled.network.device_id, receiving.receiver.profile_id)) throw fail("Receiver publication missing");
      await captureScenarioScreenshot({ cdp, sink, name: "mcp-receiver-enabled", owner: OWNER });
      if (permissionDecision === "denied") await exerciseReceiverLocalDiagnostics({ state, cdp, input, sink, owner: OWNER });
      const { createDeviceParticipant } = await import(pathToFileURL(path.join(settings.hubRepository, "tests/browser/device-fixture.mjs")));
      const sender = state.sender = await createDeviceParticipant(network, "MCP GUI sender fixture");
      await page.locator('nav a[href="#clients"]').click();
      await page.locator("#network-clients-refresh").click();
      await page.locator(`[data-id="request:${sender.requestId}"]`).getByRole("button", { name: "MCP GUI sender fixtureの許可", exact: true }).click();
      const approval = await sender.collectApproval();
      if (approval.status !== "approved") throw fail("Sender participant not approved");
      await sender.presence();
      await page.locator("#network-refresh").click();
      await page.locator('nav a[href="#device-network"]').click();
      await page.locator("#network-rule-from").selectOption(`device:${approval.deviceId}`);
      await page.locator("#network-rule-to").selectOption(`device:${enrolled.network.device_id}`);
      await page.locator("#network-rule-profile").selectOption(receiving.receiver.profile_id);
      await page.getByRole("button", { name: "接続ルールを追加", exact: true }).click();
      await wait("Hub saves the exact direct allow rule", () => hub.observeNetwork(), snapshot => snapshot.rules.some(rule => rule.from.id === approval.deviceId && rule.to.id === enrolled.network.device_id && rule.profile_id === receiving.receiver.profile_id && rule.usage === "direct" && rule.effect === "allow"));
      await trustedClick(input, cdp, action("close-overlay", `${HUB} .hub-modal-footer`), sink);
      if (historyPagination) return exerciseMcpHistoryPagination({ state, resource, input, cdp, sink, owner: OWNER,
        deviceId: enrolled.network.device_id, profileId: receiving.receiver.profile_id });
      if (permissionDecision) state.permissionMain = await prepareReceiverPermissionMain({ cdp, input, sink, owner: OWNER });
      state.peer = await sender.connectMcpPeer({ audienceDeviceId: enrolled.network.device_id, profileId: receiving.receiver.profile_id,
        rootTaskId: "gui-receiver-root", requestKey: "gui-receiver-request" });
      const accepted = await state.peer.delegate(state.permissionPlan?.call.prompt ?? SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT);
      if (!/^[0-9A-Z]{26}$/.test(accepted.job_id ?? "")) throw fail("MCP delegate did not return a durable job identity");
      state.jobId = accepted.job_id;
      const historyRow = byId(`mcp-history-row-${state.jobId}`);
      let finished, running, permissionResult;
      const terminalState = stopFromHistory || permissionDecision === "abort" ? "interrupted" : "completed";
      if (permissionDecision) {
        permissionResult = await executeReceiverPermissionDecision({ state, cdp, input, sink, owner: OWNER,
          profileId: receiving.receiver.profile_id, requesterLabel: approval.deviceId });
        finished = permissionResult.finished;
        running = permissionResult.pending.surface.projection.mcp_activity;
        await trustedClick(input, cdp, action("show-mcp-history", "aside.sidebar"), sink);
        await trustedClick(input, cdp, byId("mcp-history-execution"), sink);
        // Match the existing live/stop reload precondition: a selected detail.
        // An initially unselected list otherwise looks identical to a finished
        // refresh before the queued Refresh action has reset the old rows.
        await trustedClick(input, cdp, historyRow, sink);
        await wait("Terminal permission job is selected before explicit history reload", () => cdp.evaluate(`(() => ({
          owner:document.querySelector('${HISTORY}')?.dataset.historyDetailOwner,
          selected:document.getElementById(${JSON.stringify(historyRow.identity.id)})?.getAttribute('aria-pressed'),
          text:document.querySelector('${HISTORY} [data-history-region="document"]')?.textContent
        }))()`), value => value.owner === `execution:${state.jobId}` && value.selected === "true" && value.text?.includes(state.jobId));
      } else {
      await wait("Actual receiver reaches held model continuation", () => state.provider.requestLedger,
        ledger => ledger.some(row => row.response_phase === "held" && row.contract?.pass && row.contract.role === "chat_continuation"));
      running = await wait("Receiver-wide red activity strip is visible", () => activity(cdp), value => receiverActivityMatches(value, true));
      await captureScenarioScreenshot({ cdp, sink, name: "mcp-receiver-running-strip", owner: OWNER });
      await trustedClick(input, cdp, action("show-mcp-execution-history", ".mcp-run-strip"), sink);
      await trustedClick(input, cdp, historyRow, sink);
      await wait("Actual execution history displays the running request", () => cdp.evaluate(`document.querySelector('${HISTORY} [data-history-region="document"]')?.textContent`), value => value?.includes(SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT));
      await captureScenarioScreenshot({ cdp, sink, name: "mcp-receiver-running-history", owner: OWNER });
      if(stopFromHistory){
        const commands=state.commands=new DesktopCommandProbe(cdp,{probeId:"mcp-receiver-history-stop",commands:["mcp_history_stop"]});
        await commands.install();
        await trustedClick(input,cdp,byId("mcp-history-stop"),sink);
        const stopped=await wait("History Stop interrupts the actual receiver while model response is held",async()=>({job:await state.peer.status(state.jobId),ledger:state.provider.requestLedger}),value=>receiverStoppedWhileProviderHeld(value.job,value.ledger),45_000);
        const proof=assertExactDesktopCommandSequence(await commands.snapshot(),{expected:[{command:"mcp_history_stop",args:{direction:"execution",id:state.jobId}}]});
        finished=stopped.job;state.settled=true;
        await sink.record("mcp-history-stop-before-provider-release",{job_id:state.jobId,job:finished,held_provider:stopped.ledger,command:proof},{phase:"executing",owner:OWNER});
        await commands.remove();state.commands=null;
        // The existing scripted continuation awaits explicit fixture release even if
        // its caller has disconnected. Release only after the real job is terminal;
        // this is fixture drainage, not simulated cancellation or model completion.
        state.provider.releaseScriptRole("chat_continuation");state.released=true;
        await wait("Stopped receiver ignores the subsequently drained fixture response",async()=>({job:await state.peer.status(state.jobId),provider:state.provider.resourceObservation()}),value=>value.job.state==="interrupted"&&value.provider.active_request_count===0);
      }else{
        state.provider.releaseScriptRole("chat_continuation"); state.released = true;
        finished = await wait("The actual remote task completes", () => state.peer.status(state.jobId), value => value.state === "completed", 45_000);
        state.settled = true;
        if (finished.result !== SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_RESPONSE) throw fail("Remote result differs from the controlled provider reply", { result: finished.result });
      }
      }
      await state.peer.close();
      state.protocolCalls = state.peer.calls(); state.peer = null;
      await trustedClick(input, cdp, byId("mcp-history-refresh"), sink);
      const reloaded = await wait("Execution history reload finishes before selecting the completed job", () => cdp.evaluate(`(() => {
        const dialogs = document.querySelectorAll('${HISTORY}');
        const dialog = dialogs.length === 1 ? dialogs[0] : null;
        const refresh = dialog?.querySelectorAll('#mcp-history-refresh') ?? [];
        const rows = Array.from(dialog?.querySelectorAll('[data-history-row]') ?? []);
        return { dialogCount: dialogs.length, page: dialog?.dataset.historyPage, detailOwner: dialog?.dataset.historyDetailOwner,
          refresh: { count: refresh.length, disabled: refresh[0]?.disabled, ariaDisabled: refresh[0]?.getAttribute('aria-disabled') },
          selectedRows: rows.filter(row => row.getAttribute('aria-pressed') === 'true').length,
          listError: dialog?.querySelector('[data-history-region="list-error"]')?.textContent?.trim(),
          rows: rows.filter(row => row.dataset.historyRow === ${JSON.stringify(state.jobId)}).map(row => ({ id: row.dataset.historyRow,
            state: row.querySelector('[data-state]')?.getAttribute('data-state'), pressed: row.getAttribute('aria-pressed') })) };
      })()`), value => receiverHistoryReloadReady(value, state.jobId,terminalState));
      await sink.record("mcp-receiver-history-reloaded", { job_id: state.jobId, surface: reloaded }, { phase: "executing", owner: OWNER });
      await trustedClick(input, cdp, historyRow, sink);
      const terminalText=terminalState === "interrupted" ? state.jobId : state.permissionPlan?.call.responseText ?? SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_RESPONSE;
      await wait("Actual execution history is terminal with the exact selected request", () => cdp.evaluate(`(() => ({ state: document.getElementById(${JSON.stringify(`mcp-history-row-${state.jobId}`)})?.querySelector('[data-state]')?.getAttribute('data-state'), text: document.querySelector('${HISTORY} [data-history-region="document"]')?.textContent,stopEnabled:document.querySelector('${HISTORY} #mcp-history-stop')?.disabled===false }))()`), value => value.state === terminalState && value.text?.includes(terminalText)&&value.stopEnabled===false);
      await captureScenarioScreenshot({ cdp, sink, name: `mcp-receiver-${terminalState}-history`, owner: OWNER });
      state.nativeBefore = await snapshotOwnedTopLevelWindows(state.nativeOwner);
      state.importDispatched = true;
      await trustedClick(input, cdp, byId("mcp-history-export"), sink);
      const picker = await wait("MCP Markdown export opens the exact native save dialog", async () => {
        const windows = await snapshotOwnedTopLevelWindows(state.nativeOwner);
        try { return selectFreshOwnedRootWindow(state.nativeBefore, windows, runtime.desktop_owner, { expectedClassName: "#32770" }); }
        catch (error) { if (error?.code === "native-window-cardinality" && error.evidence?.fresh_windows?.length === 0) return null; throw error; }
      }, Boolean);
      state.nativeCandidate = picker;
      const native = await captureOwnedWindowPng({ ...state.nativeOwner, candidate: picker });
      if (native.available) await sink.writeBytes("screenshots/mcp-receiver-native-export.png", native.bytes);
      await closeOwnedNativeDialog({ ...state.nativeOwner, candidate: picker });
      await wait("Cancelled export closes the exact native dialog", () => probeExactOwnedWindow({ ...state.nativeOwner, candidate: picker }), value => !value.live);
      state.nativeCandidate = null; state.importDispatched = false;
      await trustedClick(input, cdp, byId("mcp-history-close-top"), sink);
      await wait("Completed task removes the receiver activity strip", () => activity(cdp), value => receiverActivityMatches(value, false));
      if (permissionDecision) {
        await wait("Permission outcome remains intact after closing history and export", async () => {
          let receipt = null;
          try { receipt = await readFile(state.permissionPlan.receiptPath, "utf8"); }
          catch (error) { if (error?.code !== "ENOENT") throw error; }
          return { surface: await observeReceiverPermissionSurface(cdp), job: finished,
            ledger: state.provider.requestLedger, receipt };
        }, value => receiverPermissionTerminalFailures(value, permissionResult.expected).length === 0);
      }
      await page.locator('nav a[href="#mcp-history"]').click();
      await page.locator("#history-device").selectOption(enrolled.network.device_id);
      await page.locator("#history-direction").selectOption("execution");
      await page.locator("#history-request").click();
      const hubHistory = page.locator(`[data-history-id="${state.jobId}"]`);
      await hubHistory.waitFor({ timeout: 60000 });
      await hubHistory.getByRole("button", { name: /の詳細を見る/ }).click();
      await page.locator("#history-markdown").filter({ hasText: terminalText }).waitFor({ timeout: 60000 });
      await resource.screenshot("mcp-receiver-hub-history-match");
      await trustedClick(input, cdp, action("show-hub", "aside.sidebar"), sink);
      await trustedClick(input, cdp, byId("hub-tab-devices"), sink);
      await trustedClick(input, cdp, byId("device-network-receiver-off"), sink);
      await wait("Actual Desktop stops accepting new MCP requests", () => invokeDesktopCommand(cdp, "device_network_projection"), value => !value.receiver.enabled);
      await trustedClick(input, cdp, action("close-overlay", `${HUB} .hub-modal-footer`), sink);
      const outcome = receiverCompletionOutcome(resource.pageErrors());
      await sink.record("mcp-receiver-live-result", { job_id: state.jobId, receiver_device_id: enrolled.network.device_id,
        profile_id: receiving.receiver.profile_id, sender_device_id: approval.deviceId, running, result: finished.result,
        protocol_calls: state.protocolCalls, provider_ledger: state.provider.requestLedger,terminal_state:terminalState,
        cancellation:stopFromHistory?"Actual Desktop history Stop; job interrupted while provider held, before explicit fixture drainage":"not exercised",
        permission:permissionDecision ?? "not exercised",
        execution_history: "actual Desktop runtime, read back through Desktop and Hub GUI", instruction_history: "not covered: sender is a protocol fixture",
        export: "actual native save dialog opened and cancelled; save confirmation remains manual",
        visual_review_required: ["red/pink running indicator", "running and completed execution detail", "native export dialog", "Hub same job and result"] }, { phase: "executing", owner: OWNER });
      return outcome;
    },
    async requestGracefulExit(cdp) {
      if(state.commands){try{await state.commands.remove();}catch{state.failures.push("mcp-stop-command-probe");}state.commands=null;}
      if (state.jobId && !state.settled && state.peer) {
        try {
          await state.peer.cancel(state.jobId);
          await wait("Owned fixture remote job settles before exit", () => state.peer.status(state.jobId), value => ["completed", "failed", "interrupted"].includes(value.state), 15000);
          state.settled = true;
        } catch { state.failures.push("remote-job-not-settled"); }
      }
      if (state.peer) { try { await state.peer.close(); } catch { state.failures.push("mcp-session-close"); } state.peer = null; }
      if (state.input) { try { await state.input.cleanup(); } catch { state.failures.push("input-cleanup"); } state.input = null; }
      return requestHubEnrollmentExit(cdp, state);
    },
    async quiesce() {
      if(state.commands){try{await state.commands.remove();}catch{state.failures.push("mcp-stop-command-probe");}state.commands=null;}
      if (state.input) { try { await state.input.cleanup(); } catch { state.failures.push("input-cleanup"); } state.input = null; }
      if (state.peer) { try { await state.peer.close(); } catch { state.failures.push("mcp-session-close"); } state.peer = null; }
      const browser = state.resource ? await state.resource.close() : { pass: true };
      const provider = state.provider ? await state.provider.close() : { pass: true };
      state.close = { pass: browser.pass && provider.pass && !state.failures.length, browser, provider, failures: [...state.failures] };
      return { input: state.close.pass ? "pass" : "fail", resources: [{ kind: "mcp-receiver-live", ...state.close }] };
    },
    async cleanup() { return { input: state.close?.pass ? "pass" : "fail", resources: [] }; },
  });
}

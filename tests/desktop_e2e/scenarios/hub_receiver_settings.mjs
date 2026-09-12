import { isDeepStrictEqual } from "node:util";
import path from "node:path";
import { mkdir } from "node:fs/promises";
import { DesktopE2eError } from "../core/execution.mjs";
import { normalizeHubBrowserOptions, startHubBrowserResource } from "../drivers/hub_browser_resource.mjs";
import { WebviewInput, assertTrustedTextInsertion } from "../drivers/webview_input.mjs";
import { DesktopCommandProbe, assertExactDesktopCommandSequence } from "../drivers/desktop_command_probe.mjs";
import { acquireInteractiveShell, prepareShellBaseline } from "./shell_baseline.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";
import { byId, action, wait, trustedClick, enrollDesktopFromHubBrowser, requestHubEnrollmentExit } from "./hub_browser_enrollment.mjs";

const OWNER = "scenario:hub.receiver-settings-controls";
const HUB = '[role="dialog"][data-modal="hub"]';
const fail = (message, evidence = {}) => new DesktopE2eError("product", "hub-receiver-settings-mismatch", message, evidence);
const FIELDS = Object.freeze({ target: "device-network-target", access_mode: "device-network-access", model_mode: "device-network-model",
  start_on_launch: "device-network-start-on-launch", keep_when_hidden: "device-network-keep-hidden",
  bind_ip: "device-network-bind-ip", port: "device-network-port" });
const targetKey = target => target.kind === "temp" ? "temp" : `project:${target.project_id}`;

export function receiverSettingsMatch(projection, expected) {
  if (projection?.device_id !== expected.device_id || projection?.enrollment !== expected.enrollment) return false;
  const receiver = projection.receiver;
  return receiver?.profile_id === expected.profile_id && receiver?.enabled === expected.enabled && receiver.confirmed === true
    && (expected.enabled ? receiver.status === "receiving" && typeof receiver.endpoint === "string" && receiver.endpoint.length > 0
      : ["paused", "stopped"].includes(receiver.status))
    && Object.keys(FIELDS).every(key => isDeepStrictEqual(receiver[key], expected[key]));
}
export function receiverFormMatch(form, expected) {
  return Object.keys(FIELDS).every(key => {
    const field = form?.[key];
    if (field?.count !== 1) return false;
    return typeof expected[key] === "boolean" ? field.checked === expected[key]
      : field.value === (key === "target" ? targetKey(expected.target) : String(expected[key] ?? ""));
  });
}
export function invalidReceiverDraftMatch(observation) {
  return observation?.count === 1 && observation.save_disabled === true && observation.error.length > 0
    && observation.commands?.calls?.length === 0 && observation.commands.dropped_through === 0;
}
export function receiverSettingsOutcome(pageErrors) {
  if (pageErrors.length) throw fail("Hub browser reported page errors", { page_errors: pageErrors });
  return { acquisition: "pass", oracle: "pass", manual: "not_required" };
}

async function form(cdp) {
  return cdp.evaluate(`(() => Object.fromEntries(Object.entries(${JSON.stringify(FIELDS)}).map(([key,id]) => {
    const nodes=document.querySelectorAll('[id="'+id+'"]'), node=nodes[0];
    return [key,{count:nodes.length,value:node?.value,checked:node?.checked}];
  })))()`);
}
async function select(input, cdp, id, value, sink) {
  const target = byId(id, "SELECT");
  await trustedClick(input, cdp, target, sink);
  const index = await cdp.evaluate(`Array.from(document.getElementById(${JSON.stringify(id)}).options).findIndex(option=>option.value===${JSON.stringify(value)})`);
  if (index < 0) throw fail("Receiver option does not exist", { id, value });
  await input.pressKey("Home");
  for (let i = 0; i < index; ++i) await input.pressKey("ArrowDown");
  await input.pressKey("Enter");
  await wait("Receiver selection is visible", () => cdp.evaluate(`document.getElementById(${JSON.stringify(id)})?.value`), current => current === value);
}
async function checked(input, cdp, id, value, sink) {
  const target = byId(id, "INPUT");
  if (await cdp.evaluate(`document.querySelector(${JSON.stringify(target.selector)})?.checked`) !== value) await trustedClick(input, cdp, target, sink);
  await wait("Receiver checkbox reflects its selected value", () => cdp.evaluate(`document.querySelector(${JSON.stringify(target.selector)})?.checked`), current => current === value);
}
async function edit(input, cdp, id, value, sink) {
  const target = byId(id, "INPUT");
  await trustedClick(input, cdp, target, sink);
  await input.keyDown("Control");
  try { await input.pressKey("a"); } finally { await input.keyUp("Control"); }
  await input.pressKey("Backspace");
  if (value) {
    const start = (await input.snapshotProbe()).sequence;
    await input.insertText(target, value);
    const proof = assertTrustedTextInsertion(await input.snapshotProbe(start), { afterSequence: start, identity: target.identity, text: value });
    await sink.record("receiver-settings-trusted-edit", { target, proof }, { phase: "executing", owner: OWNER });
  }
  await input.pressKey("Tab");
  await wait("Receiver text edit is retained after blur", () => cdp.evaluate(`document.getElementById(${JSON.stringify(id)})?.value`), current => current === value);
}
async function details(input, cdp, id, open, sink) {
  const sample = () => cdp.evaluate(`document.getElementById(${JSON.stringify(id)})?.open`);
  if (await sample() !== open) await trustedClick(input, cdp, {
    selector: `#${id} > summary`, identity: { tag: "DETAILS", detailsKey: id },
  }, sink);
  await wait("Receiver disclosure changes its visible state", sample, value => value === open);
}

export function createHubReceiverSettingsScenario(options = {}) {
  const settings = normalizeHubBrowserOptions(options);
  const state = { resource: null, input: null, commands: null, failures: [], close: null,
    nativeOwner: null, nativeCandidate: null, nativeBefore: null, importDispatched: false };
  return Object.freeze({
    id: "hub.receiver-settings-controls", productOracle: "pass", manualGate: "not_required", databaseRequired: true,
    async prepare(args) {
      await prepareShellBaseline(args);
      // Give the fixture its own discovery boundary before the real Desktop opens it.
      await mkdir(path.join(args.context.paths.workspace, ".git"));
      state.resource = await startHubBrowserResource({ ...args, options: settings });
    },
    async execute({ context, runtime, driver: cdp, sink }) {
      const { resource } = state, { page, hub, provider } = resource;
      const input = state.input = new WebviewInput(cdp, { probeId: "hub-receiver-settings" });
      const projection = () => invokeDesktopCommand(cdp, "device_network_projection");
      await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: OWNER, screenshotStem: "receiver-settings-shell" });
      await input.installProbe();
      await page.goto(hub.url);
      await page.locator("#management-status").filter({ hasText: "Hub本体に接続中" }).waitFor();
      await page.locator('nav a[href="#models"]').click();
      await page.locator("#endpoint").fill(provider.url);
      await page.locator("#profile").selectOption("openai_compatible_chat");
      await page.locator("#discover").click();
      await page.locator('#model option[value="fixture-alpha"]').waitFor({ state: "attached" });
      await page.locator("#model").selectOption("fixture-alpha");
      await page.locator("#label").fill("Receiver settings model");
      await page.locator("#allow-tools").check();
      await page.locator("#register").click();
      await page.locator("#model-rows tr").filter({ hasText: "Receiver settings model" }).waitFor();
      await page.locator('nav a[href="#device-network"]').click();
      await page.locator("#network-ip").fill("127.0.0.1");
      await page.locator("#network-port").fill(String(hub.networkPort));
      await page.locator("#network-start").click();
      await page.locator("#network-stop").waitFor();
      const enrolled = await enrollDesktopFromHubBrowser({ resource, context, runtime, cdp, input, sink, nativeState: state });
      await trustedClick(input, cdp, byId("hub-tab-models"), sink);
      const catalog = await wait("The receiver can review the browser-registered model", () => invokeDesktopCommand(cdp, "hub_projection"), value => value.status === "connected" && value.catalog?.models.length === 1);
      await checked(input, cdp, `hub-main-model-${catalog.catalog.models[0].id}`, true, sink);
      await trustedClick(input, cdp, action("hub-save-main"), sink);
      await wait("Main Hub selection is confirmed", () => invokeDesktopCommand(cdp, "hub_projection"), value => value.main_confirmation === "confirmed");
      await trustedClick(input, cdp, action("hub-main-hub"), sink);
      await trustedClick(input, cdp, byId("hub-tab-devices"), sink);
      const initial = await projection();
      const projects = initial.targets.filter(row => row.target.kind === "project"
        && path.resolve(row.target.workspace_root) === path.resolve(context.paths.workspace));
      if (projects.length !== 1) throw fail("The isolated fixture project must be the unique selected publication target", { count: projects.length });
      const project = projects[0].target;
      for (const id of ["device-network-details", "device-network-bind-details", "device-network-background", "device-network-model-details", "device-network-leave-details"]) {
        await details(input, cdp, id, true, sink);
        await details(input, cdp, id, false, sink);
      }
      for (const id of ["device-network-bind-details", "device-network-background", "device-network-model-details"]) await details(input, cdp, id, true, sink);
      await trustedClick(input, cdp, action("hub-tab-models", "#device-network-model-details"), sink);
      await wait("The receiver model-review button opens model allocation", () => cdp.evaluate(`document.getElementById('hub-tab-models')?.getAttribute('aria-pressed')`), value => value === "true");
      await trustedClick(input, cdp, byId("hub-tab-devices"), sink);
      for (const id of ["device-network-bind-details", "device-network-background", "device-network-model-details"]) await details(input, cdp, id, true, sink);
      state.commands = new DesktopCommandProbe(cdp, { probeId: "receiver-settings-commands", commands: ["device_network_receiver", "device_network_leave"] });
      await state.commands.install();
      await select(input, cdp, FIELDS.target, "temp", sink);
      await select(input, cdp, FIELDS.access_mode, "default", sink);
      await select(input, cdp, FIELDS.model_mode, "hub", sink);
      await checked(input, cdp, "device-network-receiver-confirmed", true, sink);
      await checked(input, cdp, "device-network-receiver-confirmed", false, sink);
      const unchecked = await cdp.evaluate(`document.getElementById('device-network-receiver-on')?.disabled`);
      if (unchecked !== true) throw fail("An unconfirmed initial publication must not be saved");
      await checked(input, cdp, "device-network-receiver-confirmed", true, sink);
      for (const [id, value] of [[FIELDS.bind_ip, "0.0.0.0"], [FIELDS.bind_ip, "::1"], [FIELDS.port, "0"], [FIELDS.port, "65536"]]) {
        await edit(input, cdp, id, value, sink);
        const invalid = await wait("Invalid receiver address is visibly blocked without mutation", async () => ({
          ...await cdp.evaluate(`(() => ({count:document.querySelectorAll('#device-network-receiver-on').length,
            save_disabled:document.getElementById('device-network-receiver-on')?.disabled,
            error:document.getElementById('device-network-bind-error')?.textContent??''}))()`),
          commands: await state.commands.snapshot(),
        }), invalidReceiverDraftMatch);
        await sink.record("receiver-settings-invalid-draft", { id, value, invalid }, { phase: "executing", owner: OWNER });
        if (id === FIELDS.port && value === "65536") await captureScenarioScreenshot({ cdp, sink, name: "receiver-settings-invalid-port", owner: OWNER });
        await edit(input, cdp, id, "", sink);
      }
      const expected = { device_id: enrolled.network.device_id, profile_id: initial.receiver.profile_id, enrollment: "active", enabled: true, target: { kind: "temp" },
        access_mode: "default", model_mode: "hub", start_on_launch: false, keep_when_hidden: false, bind_ip: null, port: null };
      const save = async name => {
        await checked(input, cdp, "device-network-receiver-confirmed", true, sink);
        const start = (await state.commands.snapshot()).sequence;
        const current = await projection();
        await trustedClick(input, cdp, byId("device-network-receiver-on"), sink);
        const saved = await wait("The exact receiver settings are saved and receiving", projection, value => receiverSettingsMatch(value, expected), 45_000);
        const command = assertExactDesktopCommandSequence(await state.commands.snapshot(start), { afterSequence: start, expected: [{ command: "device_network_receiver", args: {
          enabled: true, target: expected.target, accessMode: expected.access_mode, modelMode: expected.model_mode, confirmed: true,
          startOnLaunch: expected.start_on_launch, keepWhenHidden: expected.keep_when_hidden, bindIp: expected.bind_ip, port: expected.port,
          expectedRevision: current.revision, expectedGeneration: current.generation,
        } }] });
        await trustedClick(input, cdp, action("close-overlay", `${HUB} .hub-modal-footer`), sink);
        await trustedClick(input, cdp, action("show-hub", "aside.sidebar"), sink);
        await trustedClick(input, cdp, byId("hub-tab-devices"), sink);
        await wait("Saved settings reappear unchanged after closing and reopening", () => form(cdp), value => receiverFormMatch(value, expected));
        for (const id of ["device-network-bind-details", "device-network-background", "device-network-model-details"]) await details(input, cdp, id, true, sink);
        await captureScenarioScreenshot({ cdp, sink, name, owner: OWNER });
        await sink.record("receiver-settings-saved", { name, expected: structuredClone(expected), receiver: saved.receiver, command }, { phase: "executing", owner: OWNER });
        return saved;
      };
      const first = await save("receiver-settings-default-auto");
      const fixedPort = Number(new URL(first.receiver.endpoint).port);
      if (!Number.isInteger(fixedPort) || fixedPort < 1) throw fail("The automatic receiver must report its owned listener port");
      const stop = async () => {
        await trustedClick(input, cdp, byId("device-network-receiver-off"), sink);
        await wait("Receiver OFF stops accepting requests", projection, value => receiverSettingsMatch(value, { ...expected, enabled: false }));
      };
      await stop();
      Object.assign(expected, { target: project, access_mode: "auto_review", model_mode: "direct", start_on_launch: true, keep_when_hidden: true, bind_ip: "127.0.0.1", port: fixedPort });
      await select(input, cdp, FIELDS.target, targetKey(project), sink);
      await select(input, cdp, FIELDS.access_mode, expected.access_mode, sink);
      await select(input, cdp, FIELDS.model_mode, expected.model_mode, sink);
      await checked(input, cdp, FIELDS.start_on_launch, true, sink);
      await checked(input, cdp, FIELDS.keep_when_hidden, true, sink);
      await edit(input, cdp, FIELDS.bind_ip, expected.bind_ip, sink);
      await edit(input, cdp, FIELDS.port, String(fixedPort), sink);
      const fixed = await save("receiver-settings-project-auto-review-fixed");
      if (new URL(fixed.receiver.endpoint).hostname !== "127.0.0.1" || Number(new URL(fixed.receiver.endpoint).port) !== fixedPort) throw fail("The configured receiver address must be the actual endpoint", { endpoint: fixed.receiver.endpoint });
      await stop();
      Object.assign(expected, { target: { kind: "temp" }, access_mode: "full_access", model_mode: "hub", start_on_launch: false, keep_when_hidden: false });
      await select(input, cdp, FIELDS.target, "temp", sink);
      await select(input, cdp, FIELDS.access_mode, expected.access_mode, sink);
      await select(input, cdp, FIELDS.model_mode, expected.model_mode, sink);
      await checked(input, cdp, FIELDS.start_on_launch, false, sink);
      await checked(input, cdp, FIELDS.keep_when_hidden, false, sink);
      await save("receiver-settings-temp-full-access");
      await stop();
      Object.assign(expected, { access_mode: "default", start_on_launch: true, keep_when_hidden: true, bind_ip: null, port: null });
      await select(input, cdp, FIELDS.access_mode, expected.access_mode, sink);
      await checked(input, cdp, FIELDS.start_on_launch, true, sink);
      await checked(input, cdp, FIELDS.keep_when_hidden, true, sink);
      await edit(input, cdp, FIELDS.bind_ip, "", sink);
      await edit(input, cdp, FIELDS.port, "", sink);
      await save("receiver-settings-restored-auto");
      await details(input, cdp, "device-network-leave-details", true, sink);
      const leaveStart = (await state.commands.snapshot()).sequence;
      await checked(input, cdp, "device-network-leave-confirmed", true, sink);
      await checked(input, cdp, "device-network-leave-confirmed", false, sink);
      if (!await cdp.evaluate(`document.querySelector('[data-action="device-network-leave"]')?.disabled`)) throw fail("Cancelling leave confirmation must disable disconnect");
      assertExactDesktopCommandSequence(await state.commands.snapshot(leaveStart), { afterSequence: leaveStart, expected: [] });
      await checked(input, cdp, "device-network-leave-confirmed", true, sink);
      await trustedClick(input, cdp, action("device-network-leave"), sink);
      const disconnected = await wait("Temporary disconnect preserves identity and stops the receiver", projection, value => receiverSettingsMatch(value, { ...expected, enrollment: "disconnected", enabled: false }));
      await wait("Reconnect action replaces refresh", () => cdp.evaluate(`document.getElementById('device-network-refresh')?.textContent`), value => value?.includes("再接続"));
      await captureScenarioScreenshot({ cdp, sink, name: "receiver-settings-disconnected", owner: OWNER });
      await trustedClick(input, cdp, byId("device-network-refresh"), sink);
      const reconnected = await wait("Explicit reconnect retains the same settings but leaves reception OFF", projection, value => receiverSettingsMatch(value, { ...expected, enabled: false }), 45_000);
      await wait("Reconnected form retains all saved settings", () => form(cdp), value => receiverFormMatch(value, expected));
      const snapshot = await hub.observeNetwork();
      if (snapshot.devices.filter(device => device.device_id === expected.device_id).length !== 1) throw fail("Reconnection must not register a second device");
      await captureScenarioScreenshot({ cdp, sink, name: "receiver-settings-reconnected-off", owner: OWNER });
      await sink.record("receiver-settings-disconnect-reconnect", { disconnected, reconnected, settings_only: ["start_on_launch", "keep_when_hidden"],
        not_exercised: ["application restart", "hidden window reception", "permission execution decisions", "direct-provider generation", "remote physical host"] }, { phase: "executing", owner: OWNER });
      await trustedClick(input, cdp, action("close-overlay", `${HUB} .hub-modal-footer`), sink);
      return receiverSettingsOutcome(resource.pageErrors());
    },
    async requestGracefulExit(cdp) {
      if (state.commands) { try { await state.commands.remove(); } catch { state.failures.push("command-probe-cleanup"); } state.commands = null; }
      if (state.input) { try { await state.input.cleanup(); } catch { state.failures.push("input-cleanup"); } state.input = null; }
      return requestHubEnrollmentExit(cdp, state);
    },
    async quiesce() {
      if (state.commands) { try { await state.commands.remove(); } catch { state.failures.push("command-probe-cleanup"); } state.commands = null; }
      if (state.input) { try { await state.input.cleanup(); } catch { state.failures.push("input-cleanup"); } state.input = null; }
      state.close ??= state.resource ? await state.resource.close() : { pass: true };
      return { input: state.close.pass && !state.failures.length ? "pass" : "fail", resources: [{ kind: "hub-receiver-settings", close: state.close, failures: state.failures }] };
    },
    async cleanup() { return { input: state.close?.pass && !state.failures.length ? "pass" : "fail", resources: [] }; },
  });
}

import { createHash } from "node:crypto";
import { mkdir, readFile, stat } from "node:fs/promises";
import path from "node:path";
import { prepareDesktopFixtureEnvironment } from "../core/desktop_isolation.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { normalizeHubBrowserOptions, startHubBrowserResource } from "../drivers/hub_browser_resource.mjs";
import { startSharedWorkflowProvider } from "../drivers/shared_work_runner_fixture.mjs";
import { createManagedExecutionRunner } from "../drivers/managed_execution_runner.mjs";
import { WebviewInput } from "../drivers/webview_input.mjs";
import { DesktopCommandProbe } from "../drivers/desktop_command_probe.mjs";
import { snapshotOwnedTopLevelWindows, selectFreshOwnedRootWindow, openFilePathInOwnedNativeDialog, probeExactOwnedWindow } from "../drivers/windows_native_input.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";
import { action, byId, trustedClick, wait, enrollDesktopFromHubBrowser, requestHubEnrollmentExit } from "./hub_browser_enrollment.mjs";
import { sharedActionTarget, setSharedLoginMode } from "./shared_work_navigation.mjs";

const ID = "settings.device-execution", OWNER = `scenario:${ID}`;
const fail = message => new DesktopE2eError("product", "device-execution-mismatch", message);
export function managedExecutionReady(projection, projectId) {
  return projection?.state === "ready" && projection.can_pause === true && projection.accepting === true
    && projection.projects.some(row => row.id === projectId && row.can_control && row.can_execute && row.preparation_state === "ready" && row.environment_id)
    && projection.access_mode === "default" && typeof projection.directory === "string" && projection.review === null;
}
export function oneTimeExecutionSetup(calls) {
  const commands = calls.filter(call => call.command === "device_execution_command").map(call => call.args?.request?.kind);
  return commands.length === 2 && commands[0] === "prepare" && commands[1] === "enable";
}
export async function observeConnectionDiagnosis(cdp, scope) {
  const key = `device-network-diagnostic-${JSON.stringify([scope, ""])}`;
  return cdp.evaluate(`([...document.querySelectorAll('.device-network-diagnostic[data-settings-passive]')]
    .find(element => element.getAttribute('data-settings-passive') === ${JSON.stringify(key)})?.textContent ?? '')`);
}

export async function quiesceDeviceExecutionResources({ runner, provider, resource }) {
  const result = { pass: true, failures: [] };
  // Preserve the Runner-before-store ordering, but always close the independent
  // provider and Hub even if the exact Runner owner cannot be settled.
  for (const [name, close] of [
    ["runner", () => runner ? runner.quiesce() : { pass: true, not_started: true }],
    ["provider", async () => { await provider?.close(); return { pass: true }; }],
    ["hub", () => resource ? resource.close() : { pass: true }],
  ]) {
    try {
      result[name] = await close();
      if (result[name]?.pass !== true) {
        result.pass = false;
        result.failures.push({ resource: name, code: "resource-not-settled" });
      }
    } catch (error) {
      const failure = { resource: name, code: error?.code ?? "resource-close-failed", message: error instanceof Error ? error.message : String(error) };
      result[name] = { pass: false, error: failure };
      result.failures.push(failure);
      result.pass = false;
    }
  }
  return result;
}
export function createDeviceExecutionScenario(options = {}) {
  const { runnerBinary, runnerTestBinary, ...hubOptions } = options;
  for (const value of [runnerBinary, runnerTestBinary]) if (value && !path.isAbsolute(value)) throw new TypeError("Runner fixture paths must be absolute");
  const settings = normalizeHubBrowserOptions(hubOptions);
  const state = { resource: null, provider: null, input: null, commands: null, context: null, environment: {}, runner: null,
    nativeOwner: null, nativeCandidate: null, nativeBefore: null, importDispatched: false, consentRequested: false, consentParentProcessId: null, close: null, priorCalls: [], inputFailures: [] };
  async function settleInput() {
    if (state.commands) {
      try { state.priorCalls.push(...(await state.commands.snapshot()).calls); }
      catch (error) { state.inputFailures.push({ resource: "command-snapshot", message: String(error) }); }
      try { await state.commands.remove(); }
      catch (error) { state.inputFailures.push({ resource: "command-probe", message: String(error) }); }
      state.commands = null;
    }
    if (state.input) {
      try { await state.input.cleanup(); }
      catch (error) { state.inputFailures.push({ resource: "input-probe", message: String(error) }); }
      state.input = null;
    }
  }
  return Object.freeze({ id: ID, productOracle: "pass", manualGate: "pending", databaseRequired: true,
    get environment() { return state.environment; },
    async prepare(args) {
      if (!runnerTestBinary || !(await stat(runnerTestBinary)).isFile()) throw new TypeError("This scenario requires current libtest runnerTestBinary and a Desktop built with desktop-e2e");
      state.context = args.context;
      const registry = args.context.desktopIsolation === "fixture"
        ? (await prepareDesktopFixtureEnvironment(args.context)).registry : path.join(args.context.root, "execution-machine-registry");
      if (args.context.desktopIsolation !== "fixture") await mkdir(registry);
      state.environment = { MOYAI_DESKTOP_E2E_RUNNER: runnerTestBinary, MOYAI_TEST_RESOURCE_REGISTRY: registry };
      state.runner = createManagedExecutionRunner({ context: args.context, sink: args.sink, runnerBinary, runnerTestBinary });
      state.provider = await startSharedWorkflowProvider();
      await prepareDesktopFixture({ ...args, owner: OWNER, configText: `[model]\nbase_url = ${JSON.stringify(state.provider.baseUrl)}\nmodel = "shared-workflow"\nprovider_profile = "openai_compatible"\nmax_retries = 0\n[multi_agent]\nenabled = false\n` });
      state.resource = await startHubBrowserResource({ ...args, options: settings });
      const bytes = await readFile(runnerTestBinary);
      await args.sink.record("managed-execution-test-boundary", { executable: runnerTestBinary, sha256: createHash("sha256").update(bytes).digest("hex"), size_bytes: bytes.length,
        registry, desktop_feature: "desktop-e2e", scope: "Desktop performs its normal automatic launch and IPC operations; only the dedicated E2E build substitutes the cfg(test) Runner process. Production builds do not have a machine-policy environment override." }, { phase: args.phase, owner: OWNER });
    },
    requestGracefulExit: cdp => requestHubEnrollmentExit(cdp, state),
    async execute({ context, runtime, driver, host, sink }) {
      let cdp = driver, currentRuntime = runtime;
      const { page, hub } = state.resource;
      const projection = () => invokeDesktopCommand(cdp, "device_execution_projection");
      async function attach() {
        await cdp.call("Runtime.enable"); await cdp.call("DOM.enable");
        state.input = new WebviewInput(cdp, { probeId: `${ID}-${currentRuntime.generation}` }); await state.input.installProbe();
        state.commands = new DesktopCommandProbe(cdp, { probeId: `${ID}-${currentRuntime.generation}`, commands: ["device_execution_command"] }); await state.commands.install();
      }
      async function openSetup() {
        const view = await invokeDesktopCommand(cdp, "desktop_state");
        if (view.overlay !== "hub") await trustedClick(state.input, cdp, action("show-hub", "aside.sidebar"), sink);
        await trustedClick(state.input, cdp, byId("hub-tab-devices"), sink);
      }
      try {
        await attach();
        if ((await projection()).isolated_test_host !== true) throw new DesktopE2eError("environment", "isolated-runner-build-required", "This scenario refuses to enable hosting unless Desktop was built with desktop-e2e and its dedicated isolated Runner settings");
        if ((await invokeDesktopCommand(cdp, "desktop_state")).overlay === "initial_setup") await trustedClick(state.input, cdp, byId("initial-setup-shared-work"), sink);
        const before = await state.runner.command(["identity"]).then(() => true, () => false);
        if (before) throw fail("The fresh fixture already has an execution host before consent");
        await page.locator('nav a[href="#device-network"]').click();
        await page.locator("#network-ip").fill("127.0.0.1"); await page.locator("#network-port").fill(String(hub.networkPort));
        await page.locator("#network-start").click(); await page.locator("#network-stop").waitFor();
        const enrolled = await enrollDesktopFromHubBrowser({ resource: state.resource, context, runtime, cdp, input: state.input, sink, nativeState: state });
        const connectionDetails = { selector: '#device-network-details > summary', identity: { tag: 'DETAILS', id: 'device-network-details' } };
        if (!await cdp.evaluate(`document.querySelector('#device-network-details')?.open`)) await trustedClick(state.input, cdp, connectionDetails, sink);
        await trustedClick(state.input, cdp, action('device-network-diagnose-hub'), sink);
        const hubDiagnosis = await wait('Controller connection diagnosis excludes optional AI gateway', () => observeConnectionDiagnosis(cdp, "hub"), text => text.includes('共有仕事の接続'));
        if (hubDiagnosis.includes('モデルGateway')) throw fail('Controller diagnosis incorrectly requires the AI gateway');
        await trustedClick(state.input, cdp, { selector: 'details[data-details-key="device-network-gateway"] > summary', identity: { tag: 'DETAILS', detailsKey: 'device-network-gateway' } }, sink);
        await trustedClick(state.input, cdp, action('device-network-diagnose-gateway'), sink);
        await wait('Explicit AI diagnosis uses the gateway scope', () => observeConnectionDiagnosis(cdp, "gateway"), text => text.includes('モデルGatewayへのTLS接続'));
        await captureScenarioScreenshot({ cdp, sink, name: 'onboarding-connection-scopes', owner: OWNER });
        await trustedClick(state.input, cdp, connectionDetails, sink);
        const approvedRoot = path.join(context.root, "approved-execution-root"); await mkdir(approvedRoot);
        const detail = { selector: '#device-execution details[data-details-key="device-execution-setup"] > summary', identity: { tag: "DETAILS", detailsKey: "device-execution-setup" } };
        if (!await cdp.evaluate(`document.querySelector('#device-execution details[data-details-key="device-execution-setup"]')?.open`)) await trustedClick(state.input, cdp, detail, sink);
        state.nativeOwner = { executionRoot: context.root, ownerPath: runtime.desktop_owner_path, expectedOwner: runtime.desktop_owner };
        state.nativeBefore = await snapshotOwnedTopLevelWindows(state.nativeOwner); state.importDispatched = true;
        await trustedClick(state.input, cdp, action("device-execution-prepare"), sink);
        state.nativeCandidate = await wait("Execution directory picker belongs to Desktop", async () => {
          try { return selectFreshOwnedRootWindow(state.nativeBefore, await snapshotOwnedTopLevelWindows(state.nativeOwner), runtime.desktop_owner, { expectedClassName: "#32770" }); }
          catch (error) { if (error?.code === "native-window-cardinality" && error.evidence?.fresh_windows?.length === 0) return null; throw error; }
        }, Boolean);
        const native = await openFilePathInOwnedNativeDialog({ ...state.nativeOwner, candidate: state.nativeCandidate, selectedPath: approvedRoot, intent: "directory" });
        await wait("Execution picker closes", () => probeExactOwnedWindow({ ...state.nativeOwner, candidate: state.nativeCandidate }), value => !value.live);
        state.nativeCandidate = null; state.importDispatched = false;
        const prepared = await wait("Native review identifies the requested scope", projection, p => p.review?.access_mode === "default" && path.resolve(p.review.directory) === approvedRoot);
        await captureScenarioScreenshot({ cdp, sink, name: "execution-one-time-consent", owner: OWNER });
        state.consentRequested = true;
        state.consentParentProcessId = runtime.desktop_process_id;
        await trustedClick(state.input, cdp, action("device-execution-enable"), sink);
        await wait("Desktop automatically starts and configures execution without a Runner control", projection, p => p.directory && p.review === null && p.can_pause, 45000);
        const started = await state.runner.capture(runtime.desktop_process_id);
        const initial = (await state.runner.command(["operations", "--runner", started.identity.runner_id])).projection;
        if (!initial.desktop_binding || initial.templates.filter(t => t.id === "desktop-default").length !== 1 || initial.mode !== "shared") throw fail("The automatic host did not persist the one-time standard template");
        await page.locator('nav a[href="#shared-administration"]').click();
        await page.locator('[data-sa-tab="projects"]').click();
        await page.locator('button[data-sa-operation="save_project"]').first().click();
        await page.locator("#shared-admin-label").fill("自動実行のプロジェクト");
        await page.getByRole("combobox", { name: "Desktop fixture administrator", exact: true }).selectOption("manager");
        await page.locator(`input[name="controller_device_ids"][value="${enrolled.network.device_id}"]`).check();
        await page.locator(`input[name="runner_device_ids"][value="${enrolled.network.device_id}"]`).check();
        const sent = [];
        const capture = request => { if (new URL(request.url()).pathname === "/admin/command" && request.method() === "POST") { const body = request.postDataJSON(); if (body.command === "hub_shared_command" && body.args?.request?.kind === "save_project") sent.push(body.args.request); } };
        page.on("request", capture);
        try { await page.locator("#shared-admin-save").click(); await page.locator("#shared-admin-form").waitFor({ state: "detached" }); } finally { page.off("request", capture); }
        if (sent.length !== 1) throw fail("Project setup must commit with one user save");
        const projectId = sent[0].id;
        const ready = await wait("Project selection creates its execution folder automatically", projection, p => managedExecutionReady(p, projectId), 60000);
        const environmentId = ready.projects.find(p => p.id === projectId).environment_id;
        const operations = (await state.runner.command(["operations", "--runner", started.identity.runner_id])).projection;
        const directory = operations.environments.find(e => e.environment_id === environmentId)?.directory;
        const relative = directory && path.relative(path.toNamespacedPath(approvedRoot), path.toNamespacedPath(directory));
        if (!relative || path.isAbsolute(relative) || relative.startsWith("..") || !(await stat(directory)).isDirectory()) throw fail("Automatic provisioning did not remain within the explicitly approved folder");
        await captureScenarioScreenshot({ cdp, sink, name: "execution-project-automatically-ready", owner: OWNER });
        await state.resource.screenshot("execution-hub-project-ready");
        // The same Windows user may explicitly use a human project membership;
        // Runner consent above deliberately did not require that login.
        await trustedClick(state.input, cdp, byId("device-network-open-shared"), sink);
        await setSharedLoginMode(state.input, cdp, sink, "password");
        for (const [id, value] of [["shared-username", state.resource.administrator.username], ["shared-password", state.resource.administrator.password]]) {
          const target = byId(id, "INPUT"); await trustedClick(state.input, cdp, target, sink); await state.input.insertText(target, value);
        }
        await trustedClick(state.input, cdp, sharedActionTarget("login"), sink);
        const sharedProjection = () => invokeDesktopCommand(cdp, "shared_work_projection");
        await wait("The explicit human member can submit in the prepared project", sharedProjection, p => p.selected_project_id === projectId && p.projects.some(row => row.id === projectId && row.can_submit));
        await trustedClick(state.input, cdp, sharedActionTarget("prepare-sample"), sink);
        const sample = await wait("Sample preparation attaches only public data without submitting", sharedProjection, p => p.inputs.some(a => a.name === "moyai-sample-numbers.csv"));
        if (sample.status.jobs.length) throw fail("Preparing the sample submitted a job without Send");
        await captureScenarioScreenshot({ cdp, sink, name: "onboarding-sample-before-send", owner: OWNER });
        await trustedClick(state.input, cdp, sharedActionTarget("submit"), sink);
        const reviewedApprovals = new Set();
        await wait("Ordinary sample work finishes with a shared output asset", async () => {
          const p = await sharedProjection();
          if (p.approval?.status === "pending" && p.approval.can_decide && !reviewedApprovals.has(p.approval.id)) {
            reviewedApprovals.add(p.approval.id);
            await trustedClick(state.input, cdp, sharedActionTarget("approve"), sink);
          }
          return p;
        }, p => p.detail?.state === "succeeded" && p.assets.some(a => a.name === "moyai-sample-result.md" && a.kind === "artifact"), 90000);
        const completed = await sharedProjection();
        if (state.provider.failures.length || !JSON.stringify(completed.detail.result).includes("60")) throw fail("The sample did not return the expected result through the normal job path");
        await captureScenarioScreenshot({ cdp, sink, name: "onboarding-first-result", owner: OWNER });
        await sink.record("onboarding-first-job", { job_id: completed.detail.id, state: completed.detail.state, asset: completed.assets.find(a => a.name === "moyai-sample-result.md"), provider_calls: state.provider.requests.length }, { phase: "executing", owner: OWNER });
        await settleInput();
        const restart = await host.restart({ context, scenario: this, sink, driver: cdp });
        cdp = restart.driver; currentRuntime = restart.runtime; await attach(); await openSetup();
        const restored = await wait("Desktop restart keeps the same execution consent automatically", projection, p => managedExecutionReady(p, projectId), 45000);
        const after = (await state.runner.command(["identity"])).identity;
        const calls = [...state.priorCalls, ...(await state.commands.snapshot()).calls];
        if (after.runner_id !== started.identity.runner_id || after.process_id !== started.identity.process_id || restored.directory !== ready.directory || !oneTimeExecutionSetup(calls)) throw fail("Desktop restart replaced its independent Runner or requested setup again");
        await captureScenarioScreenshot({ cdp, sink, name: "execution-after-desktop-restart", owner: OWNER });
        await sink.record("device-execution-managed-lifecycle", { pass: true, device_id: enrolled.network.device_id, project_id: projectId, environment_id: environmentId, directory,
          approved_root: approvedRoot, review_id: prepared.review.id, native, runner: started, runner_after_restart: after, restart: restart.restart,
          one_time_setup: true, same_independent_process: true, project_save_count: sent.length, setup_commands: calls }, { phase: "executing", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "pending" };
      } catch (error) {
        await sink.record("device-execution-failure", { desktop: await invokeDesktopCommand(cdp, "desktop_state"), execution: await projection() }, { phase: "executing", owner: OWNER }).catch(() => {});
        await captureScenarioScreenshot({ cdp, sink, name: "execution-failure", owner: OWNER }).catch(() => {});
        if (state.consentRequested && !state.runner.identity) await state.runner.capture(state.consentParentProcessId).catch(() => {});
        throw error;
      } finally { await settleInput(); }
    },
    async quiesce() {
      if (!state.close) {
        await settleInput();
        state.close = await quiesceDeviceExecutionResources(state);
        state.close.input_failures = [...state.inputFailures];
        state.close.pass &&= state.inputFailures.length === 0;
      }
      return { input: state.close.pass ? "pass" : "fail", resources: [{ kind: ID, ...state.close }] };
    },
    async cleanup() { return { input: state.close?.pass ? "pass" : "fail", resources: [] }; },
  });
}

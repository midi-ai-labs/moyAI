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
import { action, byId, hubSettingsCloseTarget, trustedClick, wait, enrollDesktopFromHubBrowser, requestHubEnrollmentExit } from "./hub_browser_enrollment.mjs";
import { sharedActionTarget, waitForSharedComposer } from "./shared_work_navigation.mjs";

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
export function gatewayConnectionDiagnosed(text) {
  // Stage keys are not rendered in the diagnostic DOM. Observe the current
  // public label without issuing another diagnosis or reading private UI state.
  return text.includes("AI中継サーバーへの暗号化接続");
}

export async function saveExecutionProject(page, sink, label) {
  const readDraft = () => page.locator("#shared-admin-form").evaluate(form => Array.from(form.querySelectorAll("input,select,textarea"), node => ({
    name: node.name, value: node.value, checked: node.type === "checkbox" ? node.checked : null,
  })));
  const draft = await readDraft();
  await page.locator("#shared-admin-save").click();
  const result = await wait("Hub saves the project or explains why the editor remains open", () => page.evaluate(() => ({
    closed: !document.querySelector("#shared-admin-form"),
    conflict: Boolean(document.querySelector("#shared-admin-review-conflict")?.getClientRects().length),
    error: document.querySelector("#shared-admin-form-error")?.textContent?.trim() || "",
  })), value => value.closed || value.conflict || Boolean(value.error));
  if (result.conflict) {
    await page.locator("#shared-admin-review-conflict").click();
    await page.locator("#shared-admin-accept-comparison").waitFor({ state: "visible" });
    const comparison = await page.locator("#shared-admin-comparison").innerText();
    if (!comparison.includes(label)) throw fail("Hub comparison lost the intended project");
    await page.locator("#shared-admin-accept-comparison").click();
    if (JSON.stringify(await readDraft()) !== JSON.stringify(draft)) throw fail("Hub comparison changed the project draft");
    await sink.record("device-execution-project-conflict-reviewed", { comparison, draft_preserved: true }, { phase: "executing", owner: OWNER });
    await page.locator("#shared-admin-save").click();
  } else if (result.error) throw fail(`Hub rejected the project form: ${result.error}`);
  await page.locator("#shared-admin-form").waitFor({ state: "detached" });
  return { saves: result.conflict ? 2 : 1, conflict_reviewed: result.conflict };
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
      async function chooseFolder(target, selectedPath) {
        state.nativeOwner = { executionRoot: context.root, ownerPath: currentRuntime.desktop_owner_path, expectedOwner: currentRuntime.desktop_owner };
        state.nativeBefore = await snapshotOwnedTopLevelWindows(state.nativeOwner); state.importDispatched = true;
        await trustedClick(state.input, cdp, target, sink);
        state.nativeCandidate = await wait("Execution directory picker belongs to Desktop", async () => {
          try { return selectFreshOwnedRootWindow(state.nativeBefore, await snapshotOwnedTopLevelWindows(state.nativeOwner), currentRuntime.desktop_owner, { expectedClassName: "#32770" }); }
          catch (error) { if (error?.code === "native-window-cardinality" && error.evidence?.fresh_windows?.length === 0) return null; throw error; }
        }, Boolean);
        const native = await openFilePathInOwnedNativeDialog({ ...state.nativeOwner, candidate: state.nativeCandidate, selectedPath, intent: "directory" });
        await wait("Execution picker closes", () => probeExactOwnedWindow({ ...state.nativeOwner, candidate: state.nativeCandidate }), value => !value.live);
        state.nativeCandidate = null; state.importDispatched = false;
        return native;
      }
      try {
        await attach();
        if ((await projection()).isolated_test_host !== true) throw new DesktopE2eError("environment", "isolated-runner-build-required", "This scenario refuses to enable hosting unless Desktop was built with desktop-e2e and its dedicated isolated Runner settings");
        if ((await invokeDesktopCommand(cdp, "desktop_state")).overlay === "initial_setup") await trustedClick(state.input, cdp, byId("initial-setup-shared-work"), sink);
        const before = await state.runner.command(["identity"]).then(() => true, () => false);
        if (before) throw fail("The fresh fixture already has an execution host before consent");
        await page.locator('nav a[href="#models"]').click();
        await page.locator("#endpoint").fill(state.provider.baseUrl);
        await page.locator("#profile").selectOption("openai_compatible_chat");
        await page.locator("#discover").click();
        await page.locator('#model option[value="shared-workflow"]').waitFor({ state: "attached" });
        await page.locator("#model").selectOption("shared-workflow");
        await page.locator("#label").fill("実行設定の試験用AI");
        await page.locator("#allow-tools").check();
        await page.locator("#register").click();
        await page.locator('nav a[href="#device-network"]').click();
        await page.locator("#network-ip").fill("127.0.0.1"); await page.locator("#network-port").fill(String(hub.networkPort));
        await page.locator("#network-start").click(); await page.locator("#network-stop").waitFor();
        const enrolled = await enrollDesktopFromHubBrowser({ resource: state.resource, context, runtime, cdp, input: state.input, sink, nativeState: state });
        const connectionDetails = { selector: '#device-network-details > summary', identity: { tag: 'DETAILS', id: 'device-network-details' } };
        if (!await cdp.evaluate(`document.querySelector('#device-network-details')?.open`)) await trustedClick(state.input, cdp, connectionDetails, sink);
        await trustedClick(state.input, cdp, action('device-network-diagnose-hub'), sink);
        const hubDiagnosis = await wait('Controller connection diagnosis excludes optional AI gateway', () => observeConnectionDiagnosis(cdp, "hub"), text => text.includes('共有仕事の接続'));
        if (hubDiagnosis.includes('AI中継サーバー')) throw fail('Controller diagnosis incorrectly requires the AI gateway');
        await trustedClick(state.input, cdp, { selector: 'details[data-details-key="device-network-gateway"] > summary', identity: { tag: 'DETAILS', detailsKey: 'device-network-gateway' } }, sink);
        await trustedClick(state.input, cdp, action('device-network-diagnose-gateway'), sink);
        await wait('Explicit AI diagnosis uses the gateway scope', () => observeConnectionDiagnosis(cdp, "gateway"), gatewayConnectionDiagnosed);
        await captureScenarioScreenshot({ cdp, sink, name: 'onboarding-connection-scopes', owner: OWNER });
        await trustedClick(state.input, cdp, connectionDetails, sink);
        const approvedRoot = path.join(context.root, "approved-execution-root"); await mkdir(approvedRoot);
        const detail = { selector: '#device-execution details[data-details-key="device-execution-setup"] > summary', identity: { tag: "DETAILS", detailsKey: "device-execution-setup" } };
        if (!await cdp.evaluate(`document.querySelector('#device-execution details[data-details-key="device-execution-setup"]')?.open`)) await trustedClick(state.input, cdp, detail, sink);
        const native = await chooseFolder(action("device-execution-prepare"), approvedRoot);
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
        await page.locator('[data-sa-operation="save_project"][data-sa-id=""]').click();
        const projectLabel = "自動実行のプロジェクト";
        await page.locator("#shared-admin-label").fill(projectLabel);
        await page.locator(`input[name="controller_device_ids"][value="${enrolled.network.device_id}"]`).check();
        await page.locator(`input[name="runner_device_ids"][value="${enrolled.network.device_id}"]`).check();
        const sent = [];
        const capture = request => { if (new URL(request.url()).pathname === "/admin/command" && request.method() === "POST") { const body = request.postDataJSON(); if (body.command === "hub_shared_command" && body.args?.request?.kind === "save_project") sent.push(body.args.request); } };
        page.on("request", capture);
        let projectSave;
        try { projectSave = await saveExecutionProject(page, sink, projectLabel); } finally { page.off("request", capture); }
        if (sent.length !== projectSave.saves || sent.some(row => JSON.stringify(row) !== JSON.stringify(sent[0]))) throw fail("Project setup changed or duplicated the intended save request");
        const projectId = sent[0].id;
        await wait("Hub assigns the project for explicit local-folder selection", projection,
          p => p.projects.some(row => row.id === projectId && row.can_control && row.can_execute && row.environment_id), 60000);
        const projectFolder = path.join(approvedRoot, "execution-project"); await mkdir(projectFolder);
        await chooseFolder(action("bind-project-folder"), projectFolder);
        const sameFolder = actual => typeof actual === "string" && path.toNamespacedPath(path.resolve(actual)).toLowerCase() === path.toNamespacedPath(projectFolder).toLowerCase();
        const ready = await wait("The selected project folder is ready for execution", projection,
          p => managedExecutionReady(p, projectId) && sameFolder(p.projects.find(row => row.id === projectId)?.directory), 60000);
        const environmentId = ready.projects.find(p => p.id === projectId).environment_id;
        const operations = (await state.runner.command(["operations", "--runner", started.identity.runner_id])).projection;
        const directory = operations.environments.find(e => e.environment_id === environmentId)?.directory;
        const relative = directory && path.relative(path.toNamespacedPath(approvedRoot), path.toNamespacedPath(directory));
        if (!relative || path.isAbsolute(relative) || relative.startsWith("..") || !sameFolder(directory) || !(await stat(directory)).isDirectory()) throw fail("Runner did not adopt the explicitly selected project folder");
        await captureScenarioScreenshot({ cdp, sink, name: "execution-project-folder-ready", owner: OWNER });
        await state.resource.screenshot("execution-hub-project-ready");
        // Use the current ordinary sidebar/chat, without the retired sample UI.
        await trustedClick(state.input, cdp, hubSettingsCloseTarget, sink);
        await wait("The ordinary shell is visible", () => invokeDesktopCommand(cdp, "desktop_state"), p => p.overlay === "none");
        const sharedProjection = () => invokeDesktopCommand(cdp, "shared_work_projection");
        await wait("The project appears in the ordinary sidebar", sharedProjection, p => p.projects.some(row => row.id === projectId && row.can_submit));
        await trustedClick(state.input, cdp, { selector: `.sidebar button[data-action="open-hub-project"][data-value=${JSON.stringify(projectId)}]`, identity: { tag: "BUTTON", action: "open-hub-project" } }, sink);
        await wait("The explicit human member can submit in the prepared project", sharedProjection, p => p.selected_project_id === projectId && p.projects.some(row => row.id === projectId && row.can_submit));
        const prompt = byId("shared-prompt", "TEXTAREA");
        await waitForSharedComposer(state.input, prompt);
        await state.input.click(prompt, { stableHitSamples: 3 });
        await state.input.insertText(prompt, "desktop-conversation-start: このプロジェクトで短く答えてください。");
        await captureScenarioScreenshot({ cdp, sink, name: "execution-chat-before-send", owner: OWNER });
        await trustedClick(state.input, cdp, sharedActionTarget("submit"), sink);
        const completed = await wait("Ordinary shared request completes before restarting Desktop", sharedProjection, p => p.detail?.state === "succeeded" || p.error, 60000);
        if (completed.error || !completed.detail?.conversation_id || state.provider.failures.length) throw fail("The ordinary request did not complete through the shared job path");
        await wait("The ordinary shared response is visible", () => cdp.evaluate(`document.querySelector('[data-shared-region="history-container"]')?.innerText`), text => text?.includes("最初の依頼をこのプロジェクトで実行しました。"));
        await captureScenarioScreenshot({ cdp, sink, name: "execution-chat-result", owner: OWNER });
        await sink.record("device-execution-first-job", { job_id: completed.detail.id, state: completed.detail.state, provider_calls: state.provider.requests.length }, { phase: "executing", owner: OWNER });
        await settleInput();
        let previousRunnerExited = null;
        const restart = await host.restart({ context, scenario: this, sink, driver: cdp, beforeRelaunch: async () => {
          previousRunnerExited = await state.runner.verifyExited();
          if (!previousRunnerExited.pass) throw fail("Desktop exited while its captured Runner was still running");
        } });
        cdp = restart.driver; currentRuntime = restart.runtime; await attach(); await openSetup();
        const restored = await wait("Desktop restart keeps the same execution consent automatically", projection, p => managedExecutionReady(p, projectId), 45000);
        const after = (await state.runner.capture(currentRuntime.desktop_process_id)).identity;
        state.consentParentProcessId = currentRuntime.desktop_process_id;
        const calls = [...state.priorCalls, ...(await state.commands.snapshot()).calls];
        if (after.runner_id === started.identity.runner_id || restored.directory !== ready.directory || !sameFolder(restored.projects.find(row => row.id === projectId)?.directory) || !oneTimeExecutionSetup(calls)) throw fail("Desktop Exit did not replace its Runner or lost the saved execution consent");
        await captureScenarioScreenshot({ cdp, sink, name: "execution-after-desktop-restart", owner: OWNER });
        await sink.record("device-execution-managed-lifecycle", { pass: true, device_id: enrolled.network.device_id, project_id: projectId, environment_id: environmentId, directory,
          approved_root: approvedRoot, review_id: prepared.review.id, native, runner: started, runner_after_restart: after, restart: restart.restart,
          one_time_setup: true, previous_runner_exited: previousRunnerExited, new_runner_incarnation: true, project_save_count: sent.length, project_conflict_reviewed: projectSave.conflict_reviewed, setup_commands: calls }, { phase: "executing", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "pending" };
      } catch (error) {
        await page.evaluate(() => ({
          form_open: Boolean(document.querySelector("#shared-admin-form")),
          error: document.querySelector("#shared-admin-form-error")?.textContent?.trim() || "",
          comparison: document.querySelector("#shared-admin-comparison")?.textContent?.trim() || "",
        })).then(value => sink.record("device-execution-hub-form-failure", value, { phase: "executing", owner: OWNER })).catch(() => {});
        await state.resource.screenshot("execution-hub-failure").catch(() => {});
        await sink.record("device-execution-failure", { desktop: await invokeDesktopCommand(cdp, "desktop_state"), execution: await projection() }, { phase: "executing", owner: OWNER }).catch(() => {});
        await captureScenarioScreenshot({ cdp, sink, name: "execution-failure", owner: OWNER }).catch(() => {});
        if (state.consentRequested && (!state.runner.identity || state.consentParentProcessId !== currentRuntime.desktop_process_id)) {
          await state.runner.capture(currentRuntime.desktop_process_id).then(() => { state.consentParentProcessId = currentRuntime.desktop_process_id; }).catch(() => {});
        }
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

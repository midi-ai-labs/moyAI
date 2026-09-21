import { createHash, randomUUID } from "node:crypto";
import { mkdir, readFile, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { createCompanionContext } from "../core/run_context.mjs";
import { prepareDesktopFixtureEnvironment } from "../core/desktop_isolation.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { normalizeHubBrowserOptions, startHubBrowserResource } from "../drivers/hub_browser_resource.mjs";
import { startSharedWorkflowProvider } from "../drivers/shared_work_runner_fixture.mjs";
import { createManagedExecutionRunner } from "../drivers/managed_execution_runner.mjs";
import { WebviewInput } from "../drivers/webview_input.mjs";
import { snapshotOwnedTopLevelWindows, selectFreshOwnedRootWindow, openFilePathInOwnedNativeDialog, probeExactOwnedWindow } from "../drivers/windows_native_input.mjs";
import { INPUT_NAME, SCRIPT_NAME, RESULT_NAME } from "../fixtures/onboarding_implementation.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";
import { action, byId, trustedClick, wait, enrollDesktopFromHubBrowser, requestHubEnrollmentExit } from "./hub_browser_enrollment.mjs";
import { openHubProjectSurface, openSharedDisclosure, sharedActionTarget } from "./shared_work_navigation.mjs";
import { isolatedDevicesAccepted } from "./shared_work_isolation.mjs";
import { quiesceDeviceExecutionResources } from "./device_execution.mjs";
import { normalizeProviderConnectionLiveOptions } from "./provider_connection_live.mjs";

const ID = "onboarding.win-a-to-win-b", OWNER = `scenario:${ID}`;
const sha = bytes => createHash("sha256").update(bytes).digest("hex");
const fail = (message, evidence = {}) => new DesktopE2eError("product", "onboarding-winab-mismatch", message, evidence);
export function latestArtifactVersion(assets, name) {
  return assets.filter(asset => asset.name === name && asset.kind === "artifact").sort((a, b) => b.version - a.version)[0];
}
export function implementedArtifactsAccepted(files) {
  return Array.isArray(files) && files.length === 2 && [SCRIPT_NAME, RESULT_NAME].every(name => files.some(file => file.name === name
    && /^[a-f0-9]{64}$/.test(file.hub_sha256) && file.saved_sha256 === file.hub_sha256 && file.execution_sha256 === file.hub_sha256))
    && files.find(file => file.name === RESULT_NAME)?.text.replaceAll("\r", "").trim() === "Count: 3\nSum: 60";
}
export function approvalVisibleBeforeScroll(observation) {
  return observation?.count === 1 && observation.visible === true && observation.enabled === true
    && observation.center_in_viewport === true && observation.center_in_scroll_clip === true && observation.center_hit === true;
}
export function createOnboardingWinAbScenario(options = {}) {
  const { runnerBinary, runnerTestBinary, liveProvider, expectProviderFailure = false, ...hubOptions } = options;
  const live = liveProvider === undefined ? null : normalizeProviderConnectionLiveOptions(liveProvider);
  if (typeof expectProviderFailure !== "boolean" || (expectProviderFailure && !live)) throw new TypeError("expectProviderFailure requires explicit live provider options");
  const settings = normalizeHubBrowserOptions(hubOptions);
  const a = { name: "a", input: null }, b = { name: "b", input: null };
  const state = { resource: null, provider: null, runner: null, close: null, failures: [], environment: {}, consent: false };
  const prepare = args => prepareDesktopFixture({ ...args, owner: OWNER, configMode: "absent", sentinelName: null, sentinelText: "" });
  async function settle(pc) { if (pc.input) { try { await pc.input.cleanup(); } catch (error) { state.failures.push(String(error)); } pc.input = null; } }
  const childScenario = {
    id: ID, databaseRequired: true,
    get environment() { return state.environment; },
    async prepare(args) {
      const fixture = await prepareDesktopFixtureEnvironment(args.context);
      state.environment = { MOYAI_DESKTOP_E2E_RUNNER: runnerTestBinary, MOYAI_TEST_RESOURCE_REGISTRY: fixture.registry };
      state.runner = createManagedExecutionRunner({ context: args.context, sink: args.sink, runnerBinary, runnerTestBinary });
      await prepare(args);
    },
    requestGracefulExit: cdp => requestHubEnrollmentExit(cdp, b),
    async quiesce() { await settle(b); return { input: state.failures.length ? "fail" : "pass", resources: [] }; },
    async cleanup() { return { input: state.failures.length ? "fail" : "pass", resources: [] }; },
  };
  return Object.freeze({ id: ID, productOracle: "pass", manualGate: "pending", databaseRequired: true,
    async prepare(args) {
      if (args.context.desktopIsolation !== "fixture" || !runnerTestBinary || !(await stat(runnerTestBinary)).isFile()) throw new TypeError("WinA/WinB requires fixture isolation and an immutable Runner libtest");
      await prepare(args); if (!live) state.provider = await startSharedWorkflowProvider();
      state.resource = await startHubBrowserResource({ ...args, options: settings });
      await args.sink.record("onboarding-boundary", { simulated_pcs: 2, actual_windows_hosts: 1, provider: live ? { kind: "live", ...live } : "scripted deterministic tool plan", test_runner: { path: runnerTestBinary, sha256: sha(await readFile(runnerTestBinary)) },
        preparation: "Common owners start an isolated Hub/browser/provider; Hub administrator setup uses browser controls. Both Desktop configurations start absent. No people, projects, enrollment or execution consent is created through a fixture API." }, { phase: args.phase, owner: OWNER });
    },
    requestGracefulExit: cdp => requestHubEnrollmentExit(cdp, a),
    async execute({ context, runtime, driver, host, sink }) {
      const { page, hub } = state.resource;
      const started = Date.now(), steps = []; let active = null, switches = 0, desktopActions = 0;
      const projection = (pc, command = "shared_work_projection") => invokeDesktopCommand(pc.driver, command);
      async function checkpoint(pc, name) {
        if (active && active !== pc.name) switches++; active = pc.name;
        const visible = await pc.driver.evaluate(`({ text: document.body.innerText, viewport: {width:innerWidth,height:innerHeight}, controls:[...document.querySelectorAll('button,input,select,textarea,summary')].filter(n=>n.getClientRects().length).map(n=>({tag:n.tagName,id:n.id,action:n.dataset.action??null,text:n.tagName==='INPUT'||n.tagName==='TEXTAREA'?'':n.textContent,disabled:Boolean(n.disabled),top:n.getBoundingClientRect().top})) })`);
        const row = { pc: pc.name, name, elapsed_ms: Date.now() - started, ...visible }; steps.push({ pc: pc.name, name, elapsed_ms: row.elapsed_ms });
        await pc.sink.record("onboarding-screen", row, { phase: "executing", owner: OWNER });
        await captureScenarioScreenshot({ cdp: pc.driver, sink: pc.sink, name: `journey-${pc.name}-${name}`, owner: OWNER });
      }
      async function hubCheckpoint(name) {
        if (active && active !== "hub") switches++; active = "hub"; steps.push({ pc: "hub", name, elapsed_ms: Date.now() - started });
        if (await page.locator("#shared-admin-setup-code").isVisible()) {
          await sink.record("onboarding-secret-screen-not-captured", { name, reason: "one-time person setup code is displayed" }, { phase: "executing", owner: OWNER });
          return;
        }
        await state.resource.screenshot(`journey-hub-${name}`);
      }
      async function nextStep(title, expected) {
        await wait("Hub identifies the next operation, responsible person and PC", async () => page.locator("#onboarding-next").innerText(), value => value.includes(title) && expected.every(text => value.includes(text)));
        await sink.record("onboarding-next-step", { title, text: await page.locator("#onboarding-next").innerText() }, { phase: "executing", owner: OWNER });
      }
      async function click(pc, target) { desktopActions++; await trustedClick(pc.input, pc.driver, target, pc.sink); }
      async function fill(pc, target, value) { await click(pc, target); await pc.input.keyDown("Control"); await pc.input.pressKey("a"); await pc.input.keyUp("Control"); await pc.input.insertText(target, value); }
      async function select(pc, target, value) {
        const values = await pc.driver.evaluate(`Array.from(document.querySelector(${JSON.stringify(target.selector)}).options,o=>o.value)`);
        const index = values.indexOf(value); if (index < 0) throw fail("Required choice is absent", { target, value, values });
        await click(pc, target); await pc.input.pressKey("Home"); for (let i = 0; i < index; i++) await pc.input.pressKey("ArrowDown"); await pc.input.pressKey("Enter");
        await wait("User selection is retained", () => pc.driver.evaluate(`document.querySelector(${JSON.stringify(target.selector)})?.value`), v => v === value);
      }
      async function attach(pc, connection) {
        Object.assign(pc, connection); await pc.driver.call("Runtime.enable"); await pc.driver.call("DOM.enable");
        pc.input = new WebviewInput(pc.driver, { probeId: `${ID}-${pc.name}`, maxProbeEvents: 16384 }); await pc.input.installProbe();
        await wait("Initial purpose choices are visible after the startup splash", () => pc.driver.evaluate(`Boolean(document.querySelector('[data-surface="initial-setup"] button[data-action="initial-setup-personal"]')?.getClientRects().length) && !document.querySelector('.splash-screen')?.getClientRects().length`), Boolean);
      }
      async function enroll(pc, entry = "shared-work") {
        if (entry === "shared-work") await openHubProjectSurface(pc.input, pc.driver, pc.sink);
        await checkpoint(pc, "team-before-connection");
        await page.locator('nav a[href="#device-network"]').click();
        const enrolled = await enrollDesktopFromHubBrowser({ resource: { ...state.resource, screenshot: name => state.resource.screenshot(`${pc.name}-${name}`) }, context: pc.context, runtime: pc.runtime, cdp: pc.driver, input: pc.input, sink: pc.sink, nativeState: pc, entry });
        pc.identity = { process_id: pc.runtime.desktop_process_id, network: enrolled.network, key_sha256: enrolled.keySha256, certificate_sha256: enrolled.certificateSha256 };
        await checkpoint(pc, entry === "hub" ? "registered-before-execution" : "registered-before-person");
      }
      async function nativeFile(pc, target, selectedPath, intent) {
        pc.nativeOwner = { executionRoot: pc.context.root, ownerPath: pc.runtime.desktop_owner_path, expectedOwner: pc.runtime.desktop_owner };
        pc.nativeBefore = await snapshotOwnedTopLevelWindows(pc.nativeOwner); pc.importDispatched = true; await click(pc, target);
        pc.nativeCandidate = await wait("Exact Desktop owns the native picker", async () => {
          try { return selectFreshOwnedRootWindow(pc.nativeBefore, await snapshotOwnedTopLevelWindows(pc.nativeOwner), pc.runtime.desktop_owner, { expectedClassName: "#32770" }); }
          catch (error) { if (error?.code === "native-window-cardinality" && error.evidence?.fresh_windows?.length === 0) return null; throw error; }
        }, Boolean);
        const result = await openFilePathInOwnedNativeDialog({ ...pc.nativeOwner, candidate: pc.nativeCandidate, selectedPath, intent });
        await wait("Native picker closes", () => probeExactOwnedWindow({ ...pc.nativeOwner, candidate: pc.nativeCandidate }), value => !value.live);
        pc.nativeCandidate = null; pc.importDispatched = false;
        await pc.sink.record("onboarding-native-selection", { intent, selectedPath, result }, { phase: "executing", owner: OWNER });
      }
      async function saveHubForm(expectedPerson = null) {
        await page.locator("#shared-admin-save").click();
        const result = await wait("Hub either saves the form or explains why it remains open", () => page.evaluate(() => ({
          open: Boolean(document.querySelector('#shared-admin-form')),
          error: document.querySelector('#shared-admin-form-error')?.textContent ?? '',
          notice: document.querySelector('#shared-admin-notice')?.textContent ?? '',
        })), value => !value.open || Boolean(value.error));
        if (result.open) {
          if (expectedPerson && result.error.includes("入力は残っています")) {
            await hubCheckpoint("visible-conflict-with-inputs");
            await page.locator("#shared-admin-review-conflict").click();
            await page.locator("#shared-admin-comparison").waitFor({ state: "visible" });
            const comparison = await page.locator("#shared-admin-comparison").innerText();
            if (!comparison.includes(expectedPerson.username) || !comparison.includes(expectedPerson.displayName) || !await page.locator("#shared-admin-save").isDisabled() || await page.locator("#shared-admin-setup-code").isVisible()) throw fail("Hub comparison must retain input without saving or issuing a code");
            await hubCheckpoint("compare-retained-person-inputs");
            await page.locator("#shared-admin-accept-comparison").click();
            if (await page.locator("#shared-admin-username").inputValue() !== expectedPerson.username || await page.locator("#shared-admin-display_name").inputValue() !== expectedPerson.displayName || await page.locator("#shared-admin-setup-code").isVisible()) throw fail("Accepting comparison changed the person draft or issued a code before Save");
            await sink.record("onboarding-explicit-conflict-recovery", { ...result, comparison, inputs_retained: true, code_issued_before_save: false, actions: ["compare current settings with retained input", "accept comparison", "save once"] }, { phase: "executing", owner: OWNER });
            return saveHubForm();
          }
          throw fail("Hub did not save the displayed form", result);
        }
      }
      try {
        await attach(a, { context, runtime, driver, sink }); await checkpoint(a, "first-launch");
        await page.locator('nav a[href="#team-onboarding"]').click(); await nextStep("Hubへの接続を開始してください", ["Hub管理者", "このHub管理画面"]); await hubCheckpoint("onboarding-initial");
        await page.locator('nav a[href="#device-network"]').click(); await page.locator("#network-ip").fill("127.0.0.1"); await page.locator("#network-port").fill(String(hub.networkPort));
        await hubCheckpoint("network-before-start"); await page.locator("#network-start").click(); await page.locator("#network-stop").waitFor();
        await enroll(a);
        // A/B share the actual Windows computer name: this rename is a simulation prerequisite.
        await page.locator("#network-clients-refresh").click(); const row = page.locator(`[data-id="device:${a.identity.network.device_id}"]`);
        await row.locator("details[data-device-identity] > summary").click(); await row.getByRole("button", { name: /を管理$/ }).click();
        await page.locator("#network-device-label").fill("WinA (操作PC)"); await page.locator("#network-device-save").click(); await page.locator("#network-device-dialog").waitFor({ state: "hidden" });
        const companion = await host.openCompanion({ context: await createCompanionContext(context, "desktop-b"), scenario: childScenario, sink });
        await attach(b, companion); await checkpoint(b, "first-launch");
        await click(b, action("initial-setup-execution", '[data-surface="initial-setup"]'));
        await wait("B selects execution setup while configuration is unfinished", () => projection(b, "desktop_state"), p => p.overlay === "initial_setup" && p.startup.onboarding_intent === "execution" && p.startup.initial_setup_required);
        const configTarget = (key, tag = "INPUT") => ({ selector: `[data-surface="initial-setup"] .settings-control[data-config-key=${JSON.stringify(key)}]`, identity: { tag, configKey: key } });
        await fill(b, configTarget("model.base_url"), live?.providerBaseUrl ?? state.provider.baseUrl);
        await select(b, configTarget("model.provider_profile", "SELECT"), "openai_compatible"); await checkpoint(b, "local-ai-provider");
        await click(b, action("initial-setup-next", '[data-surface="initial-setup"]'));
        await fill(b, byId("initial-setup-model-manual", "INPUT"), live?.model ?? "shared-workflow"); await checkpoint(b, "local-ai-model");
        await click(b, action("initial-setup-next", '[data-surface="initial-setup"]'));
        await wait("Execution AI review is rendered before its screenshot", () => b.driver.evaluate(`Boolean(document.querySelector('[data-surface="initial-setup"] [data-action="finish-initial-setup"]')?.getClientRects().length)`), Boolean);
        await checkpoint(b, "local-ai-before-save"); await click(b, action("finish-initial-setup", '[data-surface="initial-setup"]'));
        await wait("B's execution-purpose setup opens PC connection directly", () => projection(b, "desktop_state"), p => p.overlay === "hub" && p.startup.status === "ready" && p.startup.initial_setup_required === false);
        await wait("PC connection tab is visible after execution-purpose setup", () => b.driver.evaluate(`document.querySelector('#hub-tab-devices')?.getAttribute('aria-pressed') === 'true' && Boolean(document.querySelector('#device-network-import')?.getClientRects().length)`), Boolean);
        await enroll(b, "hub");
        if (!isolatedDevicesAccepted(a.identity, b.identity)) throw fail("A and B must have distinct live processes and credentials");
        await sink.record("onboarding-two-pcs", { a: a.identity, b: b.identity }, { phase: "executing", owner: OWNER });
        await click(b, byId("hub-tab-devices"));
        const execution = () => projection(b, "device_execution_projection");
        if ((await execution()).isolated_test_host !== true) throw fail("The execution host is not isolated");
        await checkpoint(b, "execution-before-setup");
        const approvedRoot = path.join(b.context.paths.workspace, "approved-execution"); await mkdir(approvedRoot);
        const detail = { selector: '#device-execution details[data-details-key="device-execution-setup"] > summary', identity: { tag: "DETAILS", detailsKey: "device-execution-setup" } };
        if (!await b.driver.evaluate(`document.querySelector('#device-execution details[data-details-key="device-execution-setup"]')?.open`)) await click(b, detail);
        await nativeFile(b, action("device-execution-prepare"), approvedRoot, "directory");
        await wait("B reviews the chosen execution directory", execution, p => p.review?.access_mode === "default" && path.resolve(p.review.directory) === approvedRoot);
        await checkpoint(b, "execution-consent"); state.consent = true; await click(b, action("device-execution-enable"));
        await wait("B automatically starts execution", execution, p => p.directory && p.can_pause && p.review === null, 45000);
        const runner = await state.runner.capture(b.runtime.desktop_process_id);
        await wait("B explains that saved execution settings still need the Hub administrator's assignment", () => b.driver.evaluate(`document.querySelector('.device-execution-handoff')?.textContent`), text => text?.includes("このPCの実行設定は保存済みです") && text.includes("Hub管理者"));
        await checkpoint(b, "enabled-before-project");
        await page.locator('nav a[href="#shared-administration"]').click(); await page.locator('[data-sa-tab="users"]').click();
        const person = { username: "win-a-user", displayName: "WinA 利用者" };
        await page.locator('[data-sa-operation="create_user_with_setup_code"]').click(); await page.locator("#shared-admin-username").fill(person.username); await page.locator("#shared-admin-display_name").fill(person.displayName);
        // A second administrator tab creates the actual job project while the person draft is open.
        // This guarantees a real revision conflict without fixture API mutation or lost user input.
        const otherAdmin = await page.context().newPage();
        try {
          await otherAdmin.goto(hub.url); await otherAdmin.locator('nav a[href="#shared-administration"]').click(); await otherAdmin.locator('[data-sa-tab="projects"]').click();
          await otherAdmin.locator('[data-sa-operation="save_project"][data-sa-id=""]').click(); await otherAdmin.locator("#shared-admin-label").fill("CSV集計の実装");
          await otherAdmin.locator("#shared-admin-save").click(); await otherAdmin.locator("#shared-admin-form").waitFor({ state: "detached" });
          await sink.record("onboarding-concurrent-admin-edit", { through: "second Hub browser tab", operation: "create this journey's real project while the person draft remains open", fixture_purpose: "deterministic comparison recovery" }, { phase: "executing", owner: OWNER });
        } finally { await otherAdmin.close(); }
        await saveHubForm(person); await page.locator("#shared-admin-setup-code").waitFor({ state: "visible" });
        const setupCode = await page.locator("#shared-admin-setup-code-value").inputValue();
        const userId = await page.locator(".shared-admin-row").filter({ has: page.getByRole("heading", { name: "WinA 利用者", exact: true }) }).locator('[data-sa-operation="update_user"]').getAttribute("data-sa-id");
        await page.locator("#shared-admin-setup-code-close").click(); await hubCheckpoint("person-created");
        await wait("Initial person setup fields are visible without opening a disclosure", () => a.driver.evaluate(`Boolean(document.querySelector('#shared-setup-code')?.getClientRects().length) && Boolean(document.querySelector('#shared-setup-confirm')?.getClientRects().length) && document.querySelector('[data-action="shared-auth-mode"][data-value="setup"]')?.getAttribute('aria-pressed') === 'true'`), Boolean);
        await checkpoint(a, "first-person-form"); const password = randomUUID();
        await fill(a, byId("shared-username", "INPUT"), "win-a-user"); await fill(a, byId("shared-password", "INPUT"), password);
        await fill(a, byId("shared-setup-code", "INPUT"), setupCode); await fill(a, byId("shared-setup-confirm", "INPUT"), password); await click(a, sharedActionTarget("setup-password"));
        await wait("A completes ordinary human setup", () => projection(a), p => p.principal?.user_id === userId && !p.principal.administrator); await checkpoint(a, "person-before-project");
        await page.locator('[data-sa-tab="projects"]').click();
        await page.locator(".shared-admin-row").filter({ has: page.getByRole("heading", { name: "CSV集計の実装", exact: true }) }).locator('[data-sa-operation="save_project"]').click();
        await page.getByRole("combobox", { name: "WinA 利用者", exact: true }).selectOption("contributor");
        await page.locator(`input[name="controller_device_ids"][value="${a.identity.network.device_id}"]`).check();
        await page.locator(`input[name="runner_device_ids"][value="${b.identity.network.device_id}"]`).check(); await hubCheckpoint("project-before-save"); await saveHubForm();
        const projectId = await page.locator(".shared-admin-row").filter({ has: page.getByRole("heading", { name: "CSV集計の実装", exact: true }) }).locator('[data-sa-operation="save_project"]').getAttribute("data-sa-id");
        const ready = await wait("Assigned B provisions its project execution folder", execution, p => p.projects.some(row => row.id === projectId && row.can_execute && row.preparation_state === "ready" && row.environment_id), 60000);
        const environmentId = ready.projects.find(row => row.id === projectId).environment_id;
        const operations = (await state.runner.command(["operations", "--runner", runner.identity.runner_id])).projection;
        const directory = operations.environments.find(row => row.environment_id === environmentId)?.directory;
        const relative = directory && path.relative(path.toNamespacedPath(approvedRoot), path.toNamespacedPath(directory));
        if (!relative || path.isAbsolute(relative) || relative.startsWith("..")) throw fail("B executes outside its consent directory");
        await checkpoint(b, "project-ready-without-human-login"); await hubCheckpoint("project-ready");
        await page.locator('nav a[href="#team-onboarding"]').click(); await nextStep("Desktopからサンプルを依頼してください", ["Desktop", "AIの接続・実行結果はこのサンプルで確認"]); await hubCheckpoint("onboarding-project-ready");
        await wait("A receives the assigned project", () => projection(a), p => p.projects.some(row => row.id === projectId && row.can_submit));
        await click(a, { selector: `.sidebar button[data-action="open-hub-project"][data-value="${projectId}"]`, identity: { tag: "BUTTON", action: "open-hub-project" } }); await checkpoint(a, "project-empty");
        const inputPath = path.join(a.context.paths.workspace, INPUT_NAME); await writeFile(inputPath, "value\n10\n20\n30\n", { flag: "wx" });
        await openSharedDisclosure(a.input, a.driver, a.sink, "hub-inputs"); await nativeFile(a, sharedActionTarget("upload-inputs"), inputPath, "open");
        await wait("CSV reaches the project input list", () => projection(a), p => p.inputs.some(asset => asset.name === INPUT_NAME));
        if (await a.driver.evaluate(`Boolean(document.getElementById('shared-environment'))`)) await select(a, byId("shared-environment", "SELECT"), environmentId);
        else if ((await projection(a)).status.environments.length !== 1 || (await projection(a)).status.environments[0].id !== environmentId) throw fail("The single displayed execution PC must be B");
        await openSharedDisclosure(a.input, a.driver, a.sink, "hub-new-chat-options");
        await fill(a, byId("shared-title", "INPUT"), "CSVを集計するスクリプトを作る");
        await fill(a, byId("shared-prompt", "TEXTAREA"), `${INPUT_NAME} の value 列を集計する ${SCRIPT_NAME} を作成し、PowerShellで実行してください。件数と合計を ${RESULT_NAME} に保存し、スクリプトと結果を返してください。${live ? '結果は「Count: 件数」「Sum: 合計」の2行にしてください。両方のファイルをこの会話の成果ファイルとして共有してください。' : ''}`);
        await checkpoint(a, "request-before-send"); await click(a, sharedActionTarget("submit"));
        const approvals = new Set(), approvalVisibility = [];
        const completed = await wait("B implements and runs the script; A receives both files", async () => {
          const p = await projection(a);
          if (p.approval?.status === "pending" && p.approval.can_decide && !approvals.has(p.approval.id)) {
            approvals.add(p.approval.id);
            const target = sharedActionTarget("approve", p.approval.id);
            const { observation } = await wait("Approval action is rendered before any focus or scroll", () => a.input.observeExactTarget(target), value => value.observation.count === 1 && value.observation.enabled);
            approvalVisibility.push({ id: p.approval.id, visible_before_scroll: approvalVisibleBeforeScroll(observation), observation });
            await sink.record("onboarding-approval-before-scroll", approvalVisibility.at(-1), { phase: "executing", owner: OWNER });
            await checkpoint(a, `approval-${approvals.size}`);
            if (p.approval.request.access === "shell") {
              const operation = await a.driver.evaluate(`document.querySelector('[data-shared-region="approval"]')?.innerText`);
              const commands = p.approval.request.details.filter(text => text.startsWith("Command: ")).map(text => text.slice("Command: ".length));
              if (!operation.includes("実行コマンド") || !operation.includes("保護を外して実行") || commands.some(command => !operation.includes(command))) throw fail("Approval must explain the effect in Japanese and preserve the full command", { operation, commands });
            }
            const detailKey = `shared-approval-${p.approval.id}`;
            const detailSelector = `[data-shared-region="approval"] details[data-details-key=${JSON.stringify(detailKey)}]`;
            const summaryTarget = { selector: `${detailSelector} > summary`, identity: { tag: "DETAILS", detailsKey: detailKey } };
            await openSharedDisclosure(a.input, a.driver, a.sink, detailKey);
            const observed = p.observed_at_ms, refreshes = new Set();
            await wait("Approval original data stays open across two refreshes", async () => {
              const view = await projection(a); if (view.observed_at_ms > observed) refreshes.add(view.observed_at_ms);
              return { count: refreshes.size, open: await a.driver.evaluate(`document.querySelector(${JSON.stringify(detailSelector)})?.open`) };
            }, value => value.count >= 2 && value.open);
            await checkpoint(a, `approval-${approvals.size}-original-data`);
            await click(a, summaryTarget);
            if (live) {
              const review = { approval: p.approval, execution_directory: directory };
              const requestSha256 = sha(Buffer.from(JSON.stringify(review)));
              const requestPath = path.join(context.root, `live-approval-${approvals.size}.json`);
              const decisionPath = path.join(context.root, `live-approval-${approvals.size}-decision.json`);
              await writeFile(requestPath, JSON.stringify({ ...review, request_sha256: requestSha256 }, null, 2), { flag: "wx" });
              const decision = await wait("Live tool request is reviewed before a GUI approval", async () => {
                try { return JSON.parse(await readFile(decisionPath, "utf8")); }
                catch (error) { if (error.code === "ENOENT") return null; throw error; }
              }, Boolean, 300000);
              if (decision.approval_id !== p.approval.id || decision.request_sha256 !== requestSha256 || decision.decision !== "approve") throw fail("Live approval was not authorized for this exact request");
              await sink.record("onboarding-live-approval-reviewed", { requestPath, decisionPath, request_sha256: requestSha256 }, { phase: "executing", owner: OWNER });
            }
            await click(a, target);
          }
          if (p.detail?.state === "failed" && !expectProviderFailure) throw fail("B's implementation failed", { detail: p.detail, provider_failures: state.provider?.failures });
          return p;
        }, p => expectProviderFailure ? p.detail?.state === "failed" : p.detail?.state === "succeeded" && [SCRIPT_NAME, RESULT_NAME].every(name => p.assets.some(asset => asset.name === name && asset.kind === "artifact")), live ? 900000 : 120000);
        if (expectProviderFailure) {
          const reason = completed.detail.result?.summary?.terminal?.outcome?.error;
          if (!reason || !completed.detail.can_continue) throw fail("Failed work must retain its error and continuation capability");
          await wait("The actual provider failure is readable without opening diagnostic details", () => a.driver.evaluate(`(() => { const region = document.querySelector('[data-shared-region="detail"]'); return { visible: region?.innerText, open: [...region.querySelectorAll('details')].some(n => n.open), followup: Boolean(document.querySelector('#shared-followup')) }; })()`), value => value.visible?.includes(reason) && value.visible.includes("仕事を完了できませんでした") && value.visible.includes("URL") && !value.visible.includes("回答文はありません") && !value.open && value.followup);
          await checkpoint(a, "provider-failure-readable");
          await click(a, { selector: '[data-shared-region="detail"] a[href="#shared-followup"]', identity: { tag: "A", href: "#shared-followup" } });
          await wait("Failure recovery link reaches the existing continuation input without submitting", () => a.driver.evaluate(`(() => { const field = document.querySelector('#shared-followup'); const rect = field?.getBoundingClientRect(); return { focused: document.activeElement === field, visible: rect && rect.top >= 0 && rect.top < innerHeight, value: field?.value, disabled: field?.disabled }; })()`), value => value.focused && value.visible && value.value === "" && !value.disabled);
          await checkpoint(a, "provider-failure-continuation-input");
          await sink.record("onboarding-provider-failure-readable", { reason, can_continue: completed.detail.can_continue, job_id: completed.detail.id }, { phase: "executing", owner: OWNER });
          return { acquisition: "pass", oracle: "pass", manual: "pending" };
        }
        await wait("Completed approval and artifact checksums stay folded", () => a.driver.evaluate(`(() => { const assets = document.querySelector('[data-shared-region="assets"]'); const approval = document.querySelector('[data-shared-region="approval"]'); return { visible: assets?.innerText, raw: assets?.textContent, open: [...(approval?.querySelectorAll('details') ?? [])].some(d => d.open) }; })()`), value => value.raw?.includes("SHA-256") && !value.visible?.includes("SHA-256") && !value.open);
        const scriptVersions = completed.assets.filter(asset => asset.name === SCRIPT_NAME && asset.kind === "artifact");
        if (!live && scriptVersions.length < 2) throw fail("The scripted journey must exercise corrected and older published versions");
        if (scriptVersions.length > 1) {
          const history = await a.driver.evaluate(`(() => { const region = document.querySelector('[data-shared-region="assets"]'); const detail = region?.querySelector('details[data-details-key^="shared-asset-versions:"]'); return { text: region?.innerText, key: detail?.dataset.detailsKey, open: detail?.open }; })()`);
          if (!history.key || history.open || !history.text.includes("最新版")) throw fail("Latest artifact must be clear before opening older versions", history);
          const selector = `[data-shared-region="assets"] details[data-details-key=${JSON.stringify(history.key)}]`;
          const target = { selector: `${selector} > summary`, identity: { tag: "DETAILS", detailsKey: history.key } };
          await click(a, target);
          const observed = (await projection(a)).observed_at_ms, refreshes = new Set();
          await wait("Older versions remain open over two refreshes", async () => {
            const view = await projection(a); if (view.observed_at_ms > observed) refreshes.add(view.observed_at_ms);
            return { count: refreshes.size, open: await a.driver.evaluate(`document.querySelector(${JSON.stringify(selector)})?.open`) };
          }, value => value.count >= 2 && value.open);
          await checkpoint(a, "previous-artifact-versions"); await click(a, target);
        }
        await checkpoint(a, "implementation-result"); await checkpoint(b, "after-implementation");
        const files = [];
        for (const name of [SCRIPT_NAME, RESULT_NAME]) {
          const asset = latestArtifactVersion(completed.assets, name); const saved = path.join(a.context.paths.workspace, name);
          await nativeFile(a, sharedActionTarget("save-asset", asset.id), saved, "save_new");
          const savedBytes = await readFile(saved), executionBytes = await readFile(path.join(directory, name));
          files.push({ name, version: asset.version, saved, execution_path: path.join(directory, name), hub_sha256: asset.sha256, saved_sha256: sha(savedBytes), execution_sha256: sha(executionBytes), text: savedBytes.toString("utf8").replace(/^\uFEFF/, "") });
        }
        if (!implementedArtifactsAccepted(files) || state.provider?.failures.length) throw fail("A's saved output differs from B's actual files", { files, provider_failures: state.provider?.failures });
        await checkpoint(a, "saved-results");
        await hubCheckpoint("onboarding-after-result");
        await sink.record("onboarding-journey-complete", { elapsed_ms: Date.now() - started, checkpoint_role_switches: switches, explicit_desktop_actions: desktopActions, steps,
          files, project_id: projectId, environment_id: environmentId, approvals: approvals.size, approval_visibility: approvalVisibility, provider_calls: state.provider?.requests.length ?? null,
          actual_tools: state.provider?.requests.flatMap(r => r.messages.filter(m => m.role === "assistant").flatMap(m => m.tool_calls ?? []).map(t => t.function.name)) ?? null,
          caveats: ["One Windows host and account, two isolated actual Tauri processes; no VM/network installation proof.", live ? "Live provider; canonical tool counts must be audited from persisted runtime evidence, not mock request history." : "Scripted plan proves real tool/file/approval transport and execution, not LLM quality.", "Hub/A/B UI mutations use controls; fixture prepares processes and source CSV. A rename compensates for the simulated same Windows machine name. A second Hub tab creates the real project to exercise concurrent-edit comparison deliberately.", "Checkpoint switches and explicit actions are lower bounds; helper keyboard navigation, native controls and Hub browser actions are recorded separately."] }, { phase: "executing", owner: OWNER });
        if (!approvalVisibility.length || approvalVisibility.some(row => !row.visible_before_scroll)) throw fail("Approval needed scrolling before its action could be found", { approval_visibility: approvalVisibility, artifacts_verified: true });
        return { acquisition: "pass", oracle: "pass", manual: "pending" };
      } catch (error) {
        await hubCheckpoint("failure").catch(() => {});
        await sink.record("onboarding-hub-failure-text", { text: await page.locator("body").innerText() }, { phase: "executing", owner: OWNER }).catch(() => {});
        for (const pc of [a, b]) if (pc.driver) await checkpoint(pc, "failure").catch(() => {});
        if (state.consent && !state.runner?.identity) await state.runner.capture(b.runtime.desktop_process_id).catch(() => {});
        throw error;
      } finally { await settle(a); await settle(b); }
    },
    async quiesce() { await settle(a); await settle(b); state.close ??= await quiesceDeviceExecutionResources(state); return { input: state.close.pass && !state.failures.length ? "pass" : "fail", resources: [{ kind: ID, close: state.close, failures: state.failures }] }; },
    async cleanup() { return { input: state.close?.pass && !state.failures.length ? "pass" : "fail", resources: [] }; },
  });
}

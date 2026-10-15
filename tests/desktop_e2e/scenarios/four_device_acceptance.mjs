import { createHash } from "node:crypto";
import { mkdir, readFile, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { createCompanionContext } from "../core/run_context.mjs";
import { prepareDesktopFixtureEnvironment } from "../core/desktop_isolation.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { normalizeHubBrowserOptions, startHubBrowserResource } from "../drivers/hub_browser_resource.mjs";
import { startSharedWorkflowProvider } from "../drivers/shared_work_runner_fixture.mjs";
import { startContextComparisonProxy } from "../drivers/context_comparison_proxy.mjs";
import { createManagedExecutionRunner } from "../drivers/managed_execution_runner.mjs";
import { WebviewInput } from "../drivers/webview_input.mjs";
import { snapshotOwnedTopLevelWindows, selectFreshOwnedRootWindow, openFilePathInOwnedNativeDialog, probeExactOwnedWindow } from "../drivers/windows_native_input.mjs";
import { APPLICATION_PROMPT, MODEL_PROMPT, INDEPENDENT_PROMPT } from "../fixtures/three_node_acceptance.mjs";
import { CONTROL_PROMPT, KEEPALIVE_COMMAND, INDEPENDENT_COMMAND, controlReply } from "../fixtures/four_device_control.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";
import { action, byId, hubSettingsCloseTarget, trustedClick, trustedFocus, wait, enrollDesktopFromHubBrowser, requestHubEnrollmentExit } from "./hub_browser_enrollment.mjs";
import { openHubProjectSurface, openSharedJob, sharedActionTarget } from "./shared_work_navigation.mjs";
import { saveExecutionProject, quiesceDeviceExecutionResources } from "./device_execution.mjs";
import { normalizeProviderConnectionLiveOptions } from "./provider_connection_live.mjs";
import { applicationProcessOwners, componentExited, verifyThreeNodeApplication } from "./three_node_app_oracle.mjs";
import { reviewGeneratedSources } from "./three_node_source_review.mjs";
import { verifyHubModelPromptSettings } from "./hub_model_prompt_settings.mjs";

const ID = "project.four-device-acceptance", OWNER = `scenario:${ID}`;
const sha = bytes => createHash("sha256").update(bytes).digest("hex");
const fail = (message, evidence = {}) => new DesktopE2eError("product", "four-device-acceptance", message, evidence);
export const sameWindowsPath = (a, b) => typeof a === "string" && typeof b === "string" &&
  path.win32.toNamespacedPath(path.win32.resolve(a)).toLowerCase() === path.win32.toNamespacedPath(path.win32.resolve(b)).toLowerCase();

export const approvalReviewRoot = (approval, selectedJobId) => approval.context?.root_id ?? selectedJobId;
export const foreignInspectionName = (originName, jobId, approval = null) => `foreign-${originName}-${(approval?.id ?? jobId).toLowerCase()}-read-only`;
export function foreignRootArrived(view, jobId) {
  const job = view.status?.jobs.find(row => row.id === jobId);
  return Boolean(job && !job.parent_id && view.conversations?.some(row => row.id === (job.conversation_id ?? job.root_id)));
}
export function foreignOriginViewAccepted(view, jobId, controls, expectedApproval = null) {
  if (view.detail?.id !== jobId || controls.handover || controls.normal.length) return false;
  const job = view.status?.jobs.find(row => row.id === jobId);
  if (!job || job.can_cancel || view.detail.can_continue || view.detail.can_revise || view.approval?.can_decide || view.approval?.can_reconfirm) return false;
  return !expectedApproval || (view.approval?.id === expectedApproval.id && view.approval.can_decide === false &&
    view.approval.context?.job_id === expectedApproval.context?.job_id && view.approval.context?.root_id === jobId);
}

export function normalizeFourDeviceOptions(options = {}) {
  const { runnerBinary, runnerTestBinary, liveProvider, upstreamCredentialFile, contextComparison = null,
    roleMapping = DEFAULT_ROLE_MAPPING, generationObservationMs = 3600000, ...hubOptions } = options;
  for (const value of [runnerBinary, runnerTestBinary, upstreamCredentialFile]) if (value !== undefined && (typeof value !== "string" || !path.isAbsolute(value))) throw new TypeError("Four-device binary and credential paths must be absolute");
  const live = liveProvider === undefined ? null : normalizeProviderConnectionLiveOptions(liveProvider, { extendedConnection: false });
  if (upstreamCredentialFile && !live) throw new TypeError("An upstream credential requires an explicit live provider");
  if (contextComparison !== null && (!["remove", "preserve"].includes(contextComparison) || !live || !upstreamCredentialFile))
    throw new TypeError("Context comparison requires remove/preserve, a live model and its exact credential file");
  if (!Number.isSafeInteger(generationObservationMs) || generationObservationMs <= 0 || generationObservationMs > 2147483647)
    throw new TypeError("Generation observation budget must be positive integer milliseconds within the timer range");
  const mapping = normalizeRoleMapping(roleMapping);
  return { runnerBinary, runnerTestBinary, live, upstreamCredentialFile, contextComparison, roleMapping: mapping,
    generationObservationMs, hub: normalizeHubBrowserOptions(hubOptions) };
}

const DEFAULT_ROLE_MAPPING = Object.freeze({ api: "a", worker: "b", database: "c" });
function normalizeRoleMapping(mapping) {
  if (!mapping || Object.keys(mapping).sort().join(",") !== "api,database,worker" || mapping.api !== "a"
    || !["b", "c"].includes(mapping.worker) || !["b", "c"].includes(mapping.database) || mapping.worker === mapping.database)
    throw new TypeError("The role mapping keeps API on A and assigns Worker/DB once each to B/C");
  return { ...mapping };
}
export function fourDeviceLayout(mapping = DEFAULT_ROLE_MAPPING) {
  const roles = normalizeRoleMapping(mapping);
  return [
    { name: "a", label: "WinA", folder: "api", control: true },
    { name: "b", label: "WinB", folder: "worker", control: false },
    { name: "c", label: "WinC", folder: "database", control: false },
    { name: "d", label: "WinD", folder: null, control: true },
  ].map(pc => ({ ...pc, role: Object.keys(roles).find(role => roles[role] === pc.name) ?? null }));
}

export function projectOverview(environments, mapping = DEFAULT_ROLE_MAPPING) {
  const pcs = Object.fromEntries(fourDeviceLayout(mapping).filter(pc => pc.role).map(pc => [pc.role, pc]));
  return `目的: 非同期テキスト解析Webアプリ。UI/APIがjobを受け付け、WorkerがDBの待機jobを処理し、DBが状態と結果を永続化する。\n` +
    `UI/API環境: ${environments.api.environment_id} (${pcs.api.label})。利用者向け画面と受付APIを置く。\n` +
    `Worker環境: ${environments.worker.environment_id} (${pcs.worker.label})。解析処理を置く。\n` +
    `DB環境: ${environments.database.environment_id} (${pcs.database.label})。job、状態、結果、エラーの保存と通信窓口を置く。\n` +
    `接続: UI/API→DB、Worker→DB。各componentは担当環境の作業フォルダーに置き、他環境のフォルダーへ直接アクセスしない。\n` +
    `試験基盤では別々の端末profileを同じWindows上で隔離しているため、通信は空きloopback TCP portを使える。アプリを1環境へ集約しない。WinDは操作のみ。`;
}

export function createFourDeviceAcceptanceScenario(options = {}) {
  const settings = normalizeFourDeviceOptions(options), { runnerBinary, runnerTestBinary, live } = settings;
  const pcs = fourDeviceLayout(settings.roleMapping).map(pc => ({ ...pc, input: null, runner: null, environment: {} }));
  const rolePcs = Object.fromEntries(pcs.filter(pc => pc.role).map(pc => [pc.role, pc]));
  const [a, b, c, d] = pcs, environments = {};
  const state = { resource: null, provider: null, close: null, failures: [], approvals: new Set(), approvalNumber: 0 };
  async function preparePc(pc, args) {
    pc.context = args.context;
    if (pc.role) {
      const fixture = await prepareDesktopFixtureEnvironment(args.context);
      pc.captureDirectory = path.join(args.context.paths.data, "http-request-capture");
      await mkdir(pc.captureDirectory, { recursive: true });
      pc.environment = { MOYAI_DESKTOP_E2E_RUNNER: runnerTestBinary, MOYAI_TEST_RESOURCE_REGISTRY: fixture.registry, MOYAI_HTTP_REQUEST_CAPTURE_DIR: pc.captureDirectory };
      pc.runner = createManagedExecutionRunner({ context: args.context, sink: args.sink, runnerBinary, runnerTestBinary });
    }
    await prepareDesktopFixture({ ...args, owner: OWNER, configMode: "absent", sentinelName: null, sentinelText: "" });
  }
  async function settle(pc) {
    if (pc.input) { try { await pc.input.cleanup(); } catch (error) { state.failures.push(String(error)); } pc.input = null; }
  }
  const childScenario = pc => ({ id: ID, databaseRequired: true,
    get environment() { return pc.environment; }, prepare: args => preparePc(pc, args),
    requestGracefulExit: cdp => requestHubEnrollmentExit(cdp, pc),
    async quiesce() { await settle(pc); return { input: state.failures.length ? "fail" : "pass", resources: [] }; },
    async cleanup() { return { input: state.failures.length ? "fail" : "pass", resources: [] }; },
  });
  return Object.freeze({ id: ID, productOracle: "pass", manualGate: "pending", databaseRequired: true,
    get environment() { return a.environment; },
    async prepare(args) {
      if (args.context.desktopIsolation !== "fixture" || !runnerTestBinary || !(await stat(runnerTestBinary)).isFile()) throw new TypeError("Four-device acceptance requires fixture isolation and a current Runner libtest");
      await preparePc(a, args);
      if (!live) state.provider = await startSharedWorkflowProvider({ reply: request => controlReply(request, environments) });
      else if (settings.contextComparison) state.provider = await startContextComparisonProxy({ endpoint: live.providerBaseUrl,
        model: live.model, credentialFile: settings.upstreamCredentialFile, mode: settings.contextComparison,
        captureDirectory: path.join(args.context.paths.data, "provider-context-comparison") });
      state.resource = await startHubBrowserResource({ ...args, options: settings.hub });
      await args.sink.record("four-device-boundary", { simulated_pcs: 4, actual_windows_hosts: 1, execution_pcs: 3,
        provider_kind: live ? "live" : "scripted-control-only", live_provider: live, application_acceptance: Boolean(live),
        context_comparison: settings.contextComparison, role_mapping: settings.roleMapping,
        generation_observation_ms: live ? settings.generationObservationMs : 240000,
        comparison_capture_directory: settings.contextComparison ? state.provider.captureDirectory : null,
        runner_test_binary: { path: runnerTestBinary, sha256: sha(await readFile(runnerTestBinary)) },
        preparation: "Separate actual Tauri profiles, credentials, Runner owners and project folders. GUI enrollment, hosting consent, model and project definition. The harness does not generate application source." }, { phase: args.phase, owner: OWNER });
    },
    requestGracefulExit: cdp => requestHubEnrollmentExit(cdp, a),
    async execute({ context, runtime, driver, host, sink }) {
      const { page, hub } = state.resource;
      const projection = (pc, command = "shared_work_projection") => invokeDesktopCommand(pc.driver, command);
      const click = (pc, target) => trustedClick(pc.input, pc.driver, target, pc.sink);
      const record = (name, value) => sink.record(name, value, { phase: "executing", owner: OWNER });
      async function checkpoint(pc, name) {
        await pc.sink.record("four-device-screen", { pc: pc.label, name, projection: await projection(pc), text: await pc.driver.evaluate("document.body.innerText") }, { phase: "executing", owner: OWNER });
        await captureScenarioScreenshot({ cdp: pc.driver, sink: pc.sink, name: `${pc.name}-${name}`, owner: OWNER });
      }
      async function attach(pc, connection) {
        Object.assign(pc, connection); await pc.driver.call("Runtime.enable"); await pc.driver.call("DOM.enable");
        pc.input = new WebviewInput(pc.driver, { probeId: `${ID}-${pc.name}`, maxProbeEvents: 32768 }); await pc.input.installProbe();
        await wait(`${pc.label} starts at purpose selection`, () => pc.driver.evaluate(`Boolean(document.querySelector('[data-surface="initial-setup"] button[data-action="initial-setup-personal"]')?.getClientRects().length) && !document.querySelector('.splash-screen')?.getClientRects().length`), Boolean);
      }
      async function fill(pc, target, value) {
        await click(pc, target);
        for (let attempt = 0; attempt < 3; attempt++) {
          await trustedFocus(pc.input, pc.driver, target);
          await pc.input.keyDown("Control"); await pc.input.pressKey("a"); await pc.input.keyUp("Control");
          await trustedFocus(pc.input, pc.driver, target);
          try { await pc.input.insertText(target, value); return; }
          catch (error) { if (error?.code !== "text-insert-focus-owner" || attempt === 2) throw error; }
        }
      }
      async function nativeFolder(pc, target, directory) {
        pc.nativeOwner = { executionRoot: context.root, ownerPath: pc.runtime.desktop_owner_path, expectedOwner: pc.runtime.desktop_owner };
        pc.nativeBefore = await snapshotOwnedTopLevelWindows(pc.nativeOwner); pc.importDispatched = true; await click(pc, target);
        pc.nativeCandidate = await wait(`${pc.label} owns the folder picker`, async () => {
          try { return selectFreshOwnedRootWindow(pc.nativeBefore, await snapshotOwnedTopLevelWindows(pc.nativeOwner), pc.runtime.desktop_owner, { expectedClassName: "#32770" }); }
          catch (error) { if (error?.code === "native-window-cardinality" && error.evidence?.fresh_windows?.length === 0) return null; throw error; }
        }, Boolean);
        await openFilePathInOwnedNativeDialog({ ...pc.nativeOwner, candidate: pc.nativeCandidate, selectedPath: directory, intent: "directory" });
        await wait("Folder picker closes", () => probeExactOwnedWindow({ ...pc.nativeOwner, candidate: pc.nativeCandidate }), value => !value.live);
        pc.nativeCandidate = null; pc.importDispatched = false;
      }
      async function enroll(pc) {
        if (pc.control) await openHubProjectSurface(pc.input, pc.driver, pc.sink);
        else {
          await click(pc, action("initial-setup-execution", '[data-surface="initial-setup"]'));
          await wait("Execution-purpose connection opens", () => projection(pc, "desktop_state"), p => p.overlay === "hub" && p.startup.onboarding_intent === "execution");
        }
        await page.locator('nav a[href="#device-network"]').click();
        const enrolled = await enrollDesktopFromHubBrowser({ resource: { ...state.resource, screenshot: name => state.resource.screenshot(`${pc.name}-${name}`) }, context: pc.context, runtime: pc.runtime, cdp: pc.driver, input: pc.input, sink: pc.sink, nativeState: pc, entry: pc.control ? "shared-work" : "hub" });
        pc.identity = { process_id: pc.runtime.desktop_process_id, network: enrolled.network, key_sha256: enrolled.keySha256, certificate_sha256: enrolled.certificateSha256 };
        await page.locator("#network-clients-refresh").click();
        const row = page.locator(`[data-id="device:${pc.identity.network.device_id}"]`);
        await row.locator("details[data-device-identity] > summary").click(); await row.getByRole("button", { name: /を管理$/ }).click();
        await page.locator("#network-device-label").fill(pc.label); await page.locator("#network-device-save").click(); await page.locator("#network-device-dialog").waitFor({ state: "hidden" });
        pc.identity.network = await wait("Renamed device keeps its identity", () => projection(pc, "device_network_projection"), p => p.device_id === pc.identity.network.device_id && p.display_name === pc.label);
        if (!pc.role) return;
        if ((await projection(pc, "desktop_state")).overlay !== "hub") await click(pc, action("show-hub", "aside.sidebar"));
        await click(pc, byId("hub-tab-devices"));
        const execution = () => projection(pc, "device_execution_projection");
        if ((await execution()).isolated_test_host !== true) throw fail("Execution machine is not isolated");
        pc.approvedRoot = path.join(pc.context.paths.workspace, "approved-execution"); await mkdir(pc.approvedRoot);
        if (!await pc.driver.evaluate(`document.querySelector('#device-execution details[data-details-key="device-execution-setup"]')?.open`)) await click(pc, { selector: '#device-execution details[data-details-key="device-execution-setup"] > summary', identity: { tag: "DETAILS", detailsKey: "device-execution-setup" } });
        await nativeFolder(pc, action("device-execution-prepare"), pc.approvedRoot);
        await wait("Review exact hosting directory with ordinary permissions", execution, p => p.review?.access_mode === "default" && sameWindowsPath(p.review.directory, pc.approvedRoot));
        pc.consentRequested = true; await click(pc, action("device-execution-enable"));
        await wait("Desktop starts its Runner after explicit consent", execution, p => p.can_pause && p.directory && p.review === null, 45000);
        pc.runnerOwner = await pc.runner.capture(pc.runtime.desktop_process_id);
      }
      async function openProject(pc, projectId) {
        if ((await projection(pc, "desktop_state")).overlay === "hub") await click(pc, hubSettingsCloseTarget);
        await wait("Assigned project arrives", () => projection(pc), p => p.projects.some(row => row.id === projectId));
        await click(pc, { selector: `.sidebar button[data-action="open-hub-project"][data-value="${projectId}"]`, identity: { tag: "BUTTON", action: "open-hub-project" } });
        await wait("Selected project settles", () => projection(pc), p => p.selected_project_id === projectId);
      }
      async function inspectForeign(origin, foreign, jobId, expectedApproval = null) {
        await wait("Origin's root and conversation arrive on the other operator", () => projection(foreign), p => foreignRootArrived(p, jobId));
        await openSharedJob(foreign.input, foreign.driver, foreign.sink, jobId);
        const view = await wait("Foreign operator sees the same work and approval", () => projection(foreign), p =>
          p.detail?.id === jobId && (!expectedApproval || p.approval?.id === expectedApproval.id));
        const controls = await foreign.driver.evaluate(`({ handover:!!document.querySelector('[data-shared-region="handover"],[data-action="shared-handover"]'), normal:[...document.querySelectorAll('[data-action="shared-approve"],[data-action="shared-deny"],[data-action="shared-stop"],[data-action="shared-cancel"],[data-action="shared-stop-conversation"],[data-action="shared-start-revise"],[data-action="shared-reconfirm-approval"]')].filter(n=>n.getClientRects().length&&!n.disabled).map(n=>n.dataset.action) })`);
        if (!foreignOriginViewAccepted(view, jobId, controls, expectedApproval)) throw fail("Foreign operator does not show the exact origin-owned work read-only", { origin: origin.label, foreign: foreign.label, controls, view, expectedApproval });
        await checkpoint(foreign, foreignInspectionName(origin.name, jobId, expectedApproval));
      }
      async function handleApproval(pc) {
        let p = await projection(pc);
        const pending = p.inbox?.items.find(item => item.kind === "approval" && item.can_act && item.approval_id !== p.approval?.id);
        if (pending) { await click(pc, sharedActionTarget("inbox-open", pending.id)); p = await projection(pc); }
        const approval = p.approval;
        if (!approval || !approval.can_decide || approval.status !== "pending" || state.approvals.has(approval.id)) return;
        const other = pc === a ? d : a;
        // Child approvals are presented on the originating conversation's root.
        // The normal chat history has no separate navigation button for a child.
        await inspectForeign(pc, other, approvalReviewRoot(approval, p.detail.id), approval);
        await checkpoint(pc, `approval-${++state.approvalNumber}`);
        if (live) {
          const review = { approval, environments, requesting_pc: pc.label }, hash = sha(Buffer.from(JSON.stringify(review)));
          const requestPath = path.join(context.root, `live-approval-${state.approvalNumber}.json`), decisionPath = path.join(context.root, `live-approval-${state.approvalNumber}-decision.json`);
          await writeFile(requestPath, JSON.stringify({ ...review, request_sha256: hash }, null, 2), { flag: "wx" });
          const decision = await wait("Agent reviews this exact live operation before GUI approval", async () => {
            try { return JSON.parse(await readFile(decisionPath, "utf8")); } catch (error) { if (error.code === "ENOENT") return null; throw error; }
          }, Boolean, 900000);
          if (decision.approval_id !== approval.id || decision.request_sha256 !== hash || decision.decision !== "approve") throw fail("Live operation has no exact approval decision");
        } else {
          const commands = approval.request.details.filter(value => value.startsWith("Command: ")).map(value => value.slice(9));
          const expectedCommand = pc === a ? KEEPALIVE_COMMAND : INDEPENDENT_COMMAND;
          if (approval.request.access !== "shell" || commands.length !== 1 || commands[0] !== expectedCommand) throw fail("Scripted approval exceeded its exact controlled command", { approval });
        }
        state.approvals.add(approval.id); await click(pc, sharedActionTarget("approve", approval.id));
      }
      try {
        await attach(a, { context, runtime, driver, sink });
        await page.locator('nav a[href="#models"]').click();
        await page.locator("#endpoint").fill(state.provider?.baseUrl ?? live.providerBaseUrl);
        await page.locator("#profile").selectOption("openai_compatible_chat");
        if (settings.upstreamCredentialFile && !settings.contextComparison) {
          const credential = JSON.parse(await readFile(settings.upstreamCredentialFile, "utf8"));
          const canonical = value => new URL(value).href.replace(/\/v1\/?$/, "").replace(/\/$/, "");
          if (canonical(credential.endpoint) !== canonical(live.providerBaseUrl) || credential.provider_profile !== "openai_compatible_chat" || typeof credential.key !== "string" || !credential.key) throw new Error("Credential does not match the selected live endpoint and profile");
          await page.locator("#api-key").fill(credential.key);
        }
        await page.locator("#discover").click();
        const model = live?.model ?? "shared-workflow";
        await page.locator(`#model option[value=${JSON.stringify(model)}]`).waitFor({ state: "attached" }); await page.locator("#model").selectOption(model);
        await page.locator("#label").fill("プロジェクトの標準AI"); await page.locator("#allow-tools").check(); await page.locator("#model-system-prompt").fill(MODEL_PROMPT);
        await page.locator("#register").click(); await page.locator("#model-rows tr").filter({ hasText: "プロジェクトの標準AI" }).waitFor();
        await page.locator('nav a[href="#device-network"]').click(); await page.locator("#network-ip").fill("127.0.0.1"); await page.locator("#network-port").fill(String(hub.networkPort));
        await page.locator("#network-start").click(); await page.locator("#network-stop").waitFor();
        await enroll(a);
        for (const pc of [b, c, d]) {
          await attach(pc, await host.openCompanion({ context: await createCompanionContext(context, `desktop-${pc.name}`), scenario: childScenario(pc), sink }));
          await enroll(pc);
        }
        for (const field of ["process_id", "key_sha256", "certificate_sha256"]) if (new Set(pcs.map(pc => pc.identity[field])).size !== 4) throw fail(`Four profiles share ${field}`);
        if (new Set(pcs.map(pc => pc.identity.network.device_id)).size !== 4) throw fail("Four profiles share a device ID");
        const label = "3環境の非同期テキスト解析";
        await page.locator('nav a[href="#shared-administration"]').click(); await page.locator('[data-sa-tab="projects"]').click();
        await page.locator('[data-sa-operation="save_project"][data-sa-id=""]').click(); await page.locator("#shared-admin-label").fill(label);
        await page.locator("#shared-admin-overview").fill("非同期テキスト解析Webアプリ。環境の準備後に役割と接続関係を定義します。");
        for (const pc of pcs) {
          if (pc.control) await page.locator(`input[name="controller_device_ids"][value="${pc.identity.network.device_id}"]`).check();
          if (pc.role) await page.locator(`input[name="runner_device_ids"][value="${pc.identity.network.device_id}"]`).check();
        }
        await saveExecutionProject(page, sink, label);
        const projectId = await page.locator(".shared-admin-row").filter({ has: page.getByRole("heading", { name: label, exact: true }) }).locator('[data-sa-operation="save_project"]').getAttribute("data-sa-id");
        for (const pc of [a, b, c]) {
          const execution = () => projection(pc, "device_execution_projection");
          const assigned = await wait("Assigned executor receives its stable environment", execution, p => p.projects.some(row => row.id === projectId && row.can_execute && row.environment_id), 60000);
          const environmentId = assigned.projects.find(row => row.id === projectId).environment_id;
          const directory = path.join(pc.approvedRoot, pc.folder); await mkdir(directory);
          await nativeFolder(pc, action("bind-project-folder"), directory);
          await wait("Project directory is ready", execution, p => p.projects.some(row => row.id === projectId && row.preparation_state === "ready" && sameWindowsPath(row.directory, directory)), 60000);
          environments[pc.role] = { environment_id: environmentId, directory };
          const operations = (await pc.runner.command(["operations", "--runner", pc.runner.identity.runner_id])).projection;
          if (!sameWindowsPath(operations.environments.find(row => row.environment_id === environmentId)?.directory, directory)) throw fail("Runner did not adopt its GUI-bound folder");
        }
        await page.locator(`[data-sa-operation="save_project"][data-sa-id="${projectId}"]`).click();
        const overview = projectOverview(environments, settings.roleMapping); await page.locator("#shared-admin-overview").fill(overview); await saveExecutionProject(page, sink, label);
        await state.resource.screenshot("four-device-project-definition");
        for (const pc of [a, d]) await openProject(pc, projectId);
        await verifyHubModelPromptSettings({ pc: a, expectedPrompt: MODEL_PROMPT, owner: OWNER });
        const dExecution = await projection(d, "device_execution_projection");
        if (dExecution.can_pause || dExecution.projects.some(row => row.can_execute)) throw fail("Control-only WinD became an executor");
        await record("four-device-ready", { project_id: projectId, overview, environments, devices: pcs.map(pc => ({ name: pc.label, identity: pc.identity, runner: pc.runnerOwner ?? null, request_capture_directory: pc.captureDirectory ?? null })) });
        if (await a.driver.evaluate(`Boolean(document.getElementById('shared-environment') || document.getElementById('shared-title') || document.querySelector('[data-action="shared-handover"]'))`)) throw fail("Regular prompt requires explicit target controls or exposes handover");
        await fill(a, byId("shared-prompt", "TEXTAREA"), live ? APPLICATION_PROMPT : CONTROL_PROMPT); await checkpoint(a, "ordinary-goal-before-send"); await click(a, sharedActionTarget("submit"));
        const admitted = await wait("Goal becomes one origin-A root", () => projection(a), p => Boolean(p.detail?.id || p.error));
        if (!admitted.detail?.id || admitted.detail.parent_id) throw fail("Goal was not admitted", { admitted });
        const rootId = admitted.detail.id, conversationId = admitted.detail.conversation_id ?? rootId;
        await record("four-device-goal-admitted", { root_id: rootId, conversation_id: conversationId, environment_id: admitted.detail.environment_id });
        await wait("Origin A reaches a completed result while approving only its own operations", async () => {
          await handleApproval(a);
          const p = await projection(a), root = p.status?.jobs.find(job => job.id === rootId);
          if (root?.state === "failed" || root?.state === "cancelled") throw fail("Generated work did not complete", { root, detail: p.detail });
          return p;
        }, p => p.status?.jobs.some(job => job.id === rootId && job.state === "succeeded"), live ? settings.generationObservationMs : 240000);
        await openSharedJob(a.input, a.driver, a.sink, rootId);
        const completed = await projection(a);
        const retained = await wait("All three environments retain their actual service capacity", () => projection(a), p => ["api", "worker", "database"].every(role => p.status?.environments.some(env => env.id === environments[role].environment_id && env.occupied >= 1)), 30000);
        await inspectForeign(a, d, rootId); await checkpoint(a, "completed-services-retained");
        let application = null;
        if (live) {
          const manifestAsset = completed.assets.filter(row => row.kind === "artifact" && row.name === "acceptance.json").sort((left, right) => right.version - left.version)[0];
          if (!manifestAsset) throw fail("Live app did not publish its acceptance manifest");
          await reviewGeneratedSources({ context, environments, sink });
          try {
            application = await verifyThreeNodeApplication({ context, environments, pcs: rolePcs, sink, browser: page.context() });
          } catch (error) {
            throw fail("Generated application did not satisfy the acceptance oracle", { cause: String(error), details: error?.evidence ?? null });
          }
          if (manifestAsset.sha256 !== application.manifestSha256) throw fail("Published manifest differs from the actual UI/API environment file");
        }
        await click(d, sharedActionTarget("new-conversation"));
        await wait("D starts an independent empty conversation", () => projection(d), p => !p.selected_job_id);
        await fill(d, byId("shared-prompt", "TEXTAREA"), INDEPENDENT_PROMPT); await click(d, sharedActionTarget("submit"));
        const queued = await wait("D's independent request waits for retained capacity", () => projection(d), p => p.detail?.id && p.detail.id !== rootId && p.detail.state === "queued" && p.detail.wait_reason, 30000);
        const dRootId = queued.detail.id;
        if (queued.detail.parent_id || queued.detail.root_id === rootId || queued.detail.conversation_id === conversationId) throw fail("D inherited A's work instead of owning a separate root");
        await inspectForeign(d, a, dRootId); await checkpoint(d, "independent-request-waits");
        await openSharedJob(a.input, a.driver, a.sink, rootId);
        await click(a, sharedActionTarget("stop-conversation", conversationId));
        if (application) {
          const owners = applicationProcessOwners(application.owners);
          const stopped = await wait("Every application and supervisor exits before D executes", async () => {
            // Read D first. If it has started and an exact owner is still live
            // in the subsequent OS observation, capacity was released early.
            const view = await projection(d), independent = view.status?.jobs.find(job => job.id === dRootId);
            const live = (await Promise.all(owners.map(async owner => ({ owner, exited: await componentExited(owner, context) })))).filter(row => !row.exited);
            if (independent && independent.state !== "queued" && live.length) throw fail("D started before A's actual process tree drained", { independent, live: live.map(row => ({ role: row.owner.role, pid: row.owner.pid })) });
            return { independent_state: independent?.state ?? null, live: live.map(row => row.owner.pid), owners: owners.map(owner => ({ role: owner.role, pid: owner.pid, ownerPath: owner.ownerPath })) };
          }, observation => observation.live.length === 0, 60000);
          await record("four-device-whole-stop-process-drain", stopped);
        }
        await wait("D starts only after actual capacity release and completes", async () => {
          await openSharedJob(d.input, d.driver, d.sink, dRootId); await handleApproval(d);
          const p = await projection(d);
          if (["failed", "cancelled"].includes(p.detail?.state)) throw fail("D's independent request failed", { detail: p.detail });
          return p;
        }, p => p.detail?.id === dRootId && p.detail.state === "succeeded", live ? 900000 : 120000);
        const released = await wait("All retained capacity is released", () => projection(d), p => p.status?.environments.every(env => env.occupied === 0), 60000);
        await checkpoint(d, "independent-result-after-release");
        await openSharedJob(a.input, a.driver, a.sink, rootId);
        await wait("Origin A shows its original completed work", () => projection(a), p => p.detail?.id === rootId);
        await wait("Origin A's execution view observes the completed release", () => projection(a, "receiver_activity_projection"),
          p => !p.unavailable && p.attempts.length === 0 && p.retained_services.length === 0);
        await checkpoint(a, "origin-a-stopped");
        await record("four-device-acceptance-complete", { mode: live ? "live-application" : "scripted-control-only", project_id: projectId, root_id: rootId, independent_root_id: dRootId,
          application: application ? { manifest: application.manifest, checks: application.checkpoints, process_owners: application.owners } : "NOT_RUN: scripted control qualification does not prove application development",
          held_environments: retained.status.environments, released_environments: released.status.environments, approvals: state.approvals.size,
          limits: "Four isolated actual Tauri profiles on one Windows host; not evidence of four physical PCs or a LAN deployment. Live output and request captures require separate language/system-prompt audit." });
        return { acquisition: "pass", oracle: "pass", manual: "pending" };
      } catch (error) {
        for (const pc of pcs) if (pc.driver) await checkpoint(pc, "failure").catch(() => {});
        for (const pc of pcs) if (pc.consentRequested && !pc.runner?.identity) await pc.runner.capture(pc.runtime.desktop_process_id).catch(() => {});
        await state.resource.screenshot("four-device-failure").catch(() => {});
        throw error;
      } finally { for (const pc of pcs) await settle(pc); }
    },
    async quiesce() {
      for (const pc of pcs) await settle(pc);
      if (!state.close) {
        const runners = [];
        for (const pc of pcs.filter(pc => pc.runner)) {
          try { runners.push({ pc: pc.label, ...await pc.runner.quiesce() }); }
          catch (error) { runners.push({ pc: pc.label, pass: false, error: String(error) }); }
        }
        const resources = await quiesceDeviceExecutionResources({ runner: null, provider: state.provider, resource: state.resource });
        state.close = { pass: runners.every(row => row.pass) && resources.pass, runners, resources };
      }
      return { input: state.close.pass && !state.failures.length ? "pass" : "fail", resources: [{ kind: ID, close: state.close, failures: state.failures }] };
    },
    async cleanup() { return { input: state.close?.pass && !state.failures.length ? "pass" : "fail", resources: [] }; },
  });
}

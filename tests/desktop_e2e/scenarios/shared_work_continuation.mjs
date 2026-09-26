import { createHash } from "node:crypto";
import { mkdir, readFile, stat } from "node:fs/promises";
import path from "node:path";
import { prepareDesktopFixtureEnvironment } from "../core/desktop_isolation.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { normalizeHubBrowserOptions, startHubBrowserResource } from "../drivers/hub_browser_resource.mjs";
import { startSharedWorkflowProvider } from "../drivers/shared_work_runner_fixture.mjs";
import { createManagedExecutionRunner } from "../drivers/managed_execution_runner.mjs";
import { WebviewInput } from "../drivers/webview_input.mjs";
import { snapshotOwnedTopLevelWindows, selectFreshOwnedRootWindow, openFilePathInOwnedNativeDialog, probeExactOwnedWindow } from "../drivers/windows_native_input.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";
import { action, byId, hubSettingsCloseTarget, trustedClick, wait, enrollDesktopFromHubBrowser, requestHubEnrollmentExit } from "./hub_browser_enrollment.mjs";
import { openHubProjectSurface, sharedActionTarget } from "./shared_work_navigation.mjs";
import { quiesceDeviceExecutionResources } from "./device_execution.mjs";

const ID = "settings.shared-work-continuation", OWNER = `scenario:${ID}`;
const fail = (message, evidence = {}) => new DesktopE2eError("product", "shared-continuation-mismatch", message, evidence);
export const sameFixtureFolder = (actual, expected) => typeof actual === "string"
  && path.toNamespacedPath(path.resolve(actual)).toLowerCase() === path.toNamespacedPath(path.resolve(expected)).toLowerCase();

/** One actual Desktop and its independent Runner exercise the normal Hub chat twice. */
export function createSharedWorkContinuationScenario(options = {}) {
  const { runnerBinary, runnerTestBinary, ...hubOptions } = options;
  const settings = normalizeHubBrowserOptions(hubOptions);
  const state = { resource: null, provider: null, runner: null, input: null, close: null, environment: {},
    consent: false, nativeOwner: null, nativeCandidate: null, nativeBefore: null, importDispatched: false };
  async function settleInput() { if (state.input) { await state.input.cleanup(); state.input = null; } }
  return Object.freeze({ id: ID, productOracle: "pass", manualGate: "pending", databaseRequired: true,
    get environment() { return state.environment; },
    async prepare(args) {
      if (args.context.desktopIsolation !== "fixture" || !runnerTestBinary || !(await stat(runnerTestBinary)).isFile())
        throw new TypeError("Shared conversation GUI verification needs an isolated desktop-e2e Runner libtest");
      const registry = (await prepareDesktopFixtureEnvironment(args.context)).registry;
      state.environment = { MOYAI_DESKTOP_E2E_RUNNER: runnerTestBinary, MOYAI_TEST_RESOURCE_REGISTRY: registry };
      state.runner = createManagedExecutionRunner({ context: args.context, sink: args.sink, runnerBinary, runnerTestBinary });
      state.provider = await startSharedWorkflowProvider();
      await prepareDesktopFixture({ ...args, owner: OWNER, configText: `[model]\nbase_url = ${JSON.stringify(state.provider.baseUrl)}\nmodel = "shared-workflow"\nprovider_profile = "openai_compatible"\nmax_retries = 0\n[multi_agent]\nenabled = false\n` });
      state.resource = await startHubBrowserResource({ ...args, options: settings });
      await args.sink.record("shared-conversation-test-boundary", { runner: runnerTestBinary,
        sha256: createHash("sha256").update(await readFile(runnerTestBinary)).digest("hex"),
        scope: "One actual Tauri Desktop and its independent isolated Runner on one Windows PC. Physical WinB is not exercised." },
        { phase: args.phase, owner: OWNER });
    },
    requestGracefulExit: cdp => requestHubEnrollmentExit(cdp, state),
    async execute({ context, runtime, driver: cdp, sink }) {
      const { page, hub } = state.resource;
      const shared = () => invokeDesktopCommand(cdp, "shared_work_projection");
      const execution = () => invokeDesktopCommand(cdp, "device_execution_projection");
      async function click(target) { await trustedClick(state.input, cdp, target, sink); }
      async function fill(target, value) {
        // The conversation textarea is reached by a visible pointer click;
        // settings-dialog tab traversal does not model the chat composer.
        await wait("The shared chat input is ready", () => cdp.evaluate(`(() => {
          const nodes = document.querySelectorAll(${JSON.stringify(target.selector)});
          return nodes.length === 1 && !nodes[0].disabled && !nodes[0].closest('[hidden]');
        })()`), ready => ready === true, 15000);
        for (let attempt = 0; attempt < 3; attempt++) {
          await state.input.click(target, { stableHitSamples: 3 });
          await state.input.keyDown("Control"); await state.input.pressKey("a"); await state.input.keyUp("Control");
          try { await state.input.insertText(target, value); return; }
          catch (error) { if (error?.code !== "text-insert-focus-owner" || attempt === 2) throw error; }
        }
      }
      async function chooseFolder(target, selectedPath) {
        state.nativeOwner = { executionRoot: context.root, ownerPath: runtime.desktop_owner_path, expectedOwner: runtime.desktop_owner };
        state.nativeBefore = await snapshotOwnedTopLevelWindows(state.nativeOwner); state.importDispatched = true;
        await click(target);
        state.nativeCandidate = await wait("Project folder picker belongs to this Desktop", async () => {
          try { return selectFreshOwnedRootWindow(state.nativeBefore, await snapshotOwnedTopLevelWindows(state.nativeOwner), runtime.desktop_owner, { expectedClassName: "#32770" }); }
          catch (error) { if (error?.code === "native-window-cardinality" && error.evidence?.fresh_windows?.length === 0) return null; throw error; }
        }, Boolean);
        const result = await openFilePathInOwnedNativeDialog({ ...state.nativeOwner, candidate: state.nativeCandidate, selectedPath, intent: "directory" });
        await wait("Project folder picker closes", () => probeExactOwnedWindow({ ...state.nativeOwner, candidate: state.nativeCandidate }), value => !value.live);
        state.nativeCandidate = null; state.importDispatched = false;
        await sink.record("shared-conversation-folder-selection", { selectedPath, result }, { phase: "executing", owner: OWNER });
      }
      try {
        await cdp.call("Runtime.enable"); await cdp.call("DOM.enable");
        state.input = new WebviewInput(cdp, { probeId: ID }); await state.input.installProbe();
        if ((await execution()).isolated_test_host !== true) throw fail("The Runner is not isolated from this PC's real settings");
        await openHubProjectSurface(state.input, cdp, sink);
        await page.locator('nav a[href="#models"]').click();
        await page.locator("#endpoint").fill(state.provider.baseUrl);
        await page.locator("#profile").selectOption("openai_compatible_chat");
        await page.locator("#discover").click();
        await page.locator('#model option[value="shared-workflow"]').waitFor({ state: "attached" });
        await page.locator("#model").selectOption("shared-workflow");
        await page.locator("#label").fill("会話の試験用AI");
        await page.locator("#allow-tools").check();
        await page.locator("#register").click();
        await page.locator('nav a[href="#device-network"]').click();
        await page.locator("#network-ip").fill("127.0.0.1"); await page.locator("#network-port").fill(String(hub.networkPort));
        await page.locator("#network-start").click();
        try { await page.locator("#network-stop").waitFor(); }
        catch (error) {
          const observed = await page.evaluate(() => ({
            status: document.querySelector("#network-server-status")?.textContent?.trim() || "",
            notice: document.querySelector("#network-notice")?.textContent?.trim() || "",
            hostingError: document.querySelector("#network-hosting-error")?.textContent?.trim() || "",
            bind: document.querySelector("#network-identity")?.textContent?.trim() || "",
            configuredPort: document.querySelector("#network-port")?.value || "",
          })).catch(() => null);
          await sink.record("shared-conversation-hub-start-failure", observed, { phase: "executing", owner: OWNER });
          if (observed?.notice || observed?.hostingError) throw fail("Hub did not start its connection listener", observed);
          throw error;
        }
        const enrolled = await enrollDesktopFromHubBrowser({ resource: state.resource, context, runtime, cdp, input: state.input, sink, nativeState: state, entry: "shared-work" });
        await click(action("show-hub", "aside.sidebar"));
        await click(byId("hub-tab-devices"));
        const approvedRoot = path.join(context.root, "approved-execution"); await mkdir(approvedRoot);
        const setup = { selector: '#device-execution details[data-details-key="device-execution-setup"] > summary', identity: { tag: "DETAILS", detailsKey: "device-execution-setup" } };
        if (!await cdp.evaluate(`document.querySelector('#device-execution details[data-details-key="device-execution-setup"]')?.open`)) await click(setup);
        await chooseFolder(action("device-execution-prepare"), approvedRoot);
        await wait("One-time execution consent shows the chosen parent folder", execution,
          p => p.review?.access_mode === "default" && sameFixtureFolder(p.review.directory, approvedRoot));
        state.consent = true; await click(action("device-execution-enable"));
        await wait("This PC's independent Runner is ready", execution, p => p.can_pause && p.review === null, 45000);
        const runner = await state.runner.capture(runtime.desktop_process_id);
        await page.locator('nav a[href="#shared-administration"]').click();
        await page.locator('[data-sa-tab="projects"]').click();
        await page.locator('[data-sa-operation="save_project"][data-sa-id=""]').click();
        await page.locator("#shared-admin-label").fill("通常の共有チャット");
        await page.locator(`input[name="controller_device_ids"][value="${enrolled.network.device_id}"]`).check();
        await page.locator(`input[name="runner_device_ids"][value="${enrolled.network.device_id}"]`).check();
        const readDraft = () => page.locator("#shared-admin-form").evaluate(form => Array.from(form.querySelectorAll("input,select,textarea"), node => ({
          name: node.name, value: node.value, checked: node.type === "checkbox" ? node.checked : null,
        })));
        const projectDraft = await readDraft();
        await page.locator("#shared-admin-save").click();
        const saveResult = await wait("Hub saves the project or requests a current-state comparison", () => page.evaluate(() => ({
          closed: !document.querySelector("#shared-admin-form"),
          conflict: Boolean(document.querySelector("#shared-admin-review-conflict")?.getClientRects().length),
          error: document.querySelector("#shared-admin-form-error")?.textContent?.trim() || "",
        })), value => value.closed || value.conflict || Boolean(value.error));
        if (saveResult.conflict) {
          await page.locator("#shared-admin-review-conflict").click();
          await page.locator("#shared-admin-accept-comparison").waitFor({ state: "visible" });
          const comparison = await page.locator("#shared-admin-comparison").innerText();
          if (!comparison.includes("通常の共有チャット")) throw fail("Hub comparison lost the intended project", { comparison });
          await page.locator("#shared-admin-accept-comparison").click();
          if (JSON.stringify(await readDraft()) !== JSON.stringify(projectDraft)) throw fail("Hub comparison changed the project draft");
          await sink.record("shared-conversation-project-conflict-reviewed", { comparison, draft_preserved: true }, { phase: "executing", owner: OWNER });
          await page.locator("#shared-admin-save").click();
        } else if (saveResult.error) throw fail("Hub rejected the project form", saveResult);
        await page.locator("#shared-admin-form").waitFor({ state: "detached" });
        const projectId = await page.locator(".shared-admin-row").filter({ has: page.getByRole("heading", { name: "通常の共有チャット", exact: true }) })
          .locator('[data-sa-operation="save_project"]').getAttribute("data-sa-id");
        if (!projectId) throw fail("Hub did not return the created project identity");
        const assigned = await wait("Hub assigns this PC's exact project environment", execution,
          p => p.projects.some(row => row.id === projectId && row.can_control && row.can_execute && row.environment_id), 60000);
        const environmentId = assigned.projects.find(row => row.id === projectId).environment_id;
        const projectFolder = path.join(context.paths.workspace, "shared-conversation"); await mkdir(projectFolder);
        await chooseFolder(action("bind-project-folder"), projectFolder);
        await wait("The chosen existing folder is bound to this project", execution,
          p => p.projects.some(row => row.id === projectId && row.environment_id === environmentId
            && row.preparation_state === "ready" && sameFixtureFolder(row.directory, projectFolder)), 60000);
        const operations = (await state.runner.command(["operations", "--runner", runner.identity.runner_id])).projection;
        if (!sameFixtureFolder(operations.environments.find(row => row.environment_id === environmentId)?.directory, projectFolder))
          throw fail("The independent Runner did not adopt the chosen project folder");
        await click(hubSettingsCloseTarget);
        await wait("The ordinary shell is visible", () => invokeDesktopCommand(cdp, "desktop_state"), p => p.overlay === "none");
        await wait("The assigned Hub project appears in the ordinary sidebar", shared, p => p.projects.some(row => row.id === projectId && row.can_submit));
        await click({ selector: `.sidebar button[data-action="open-hub-project"][data-value=${JSON.stringify(projectId)}]`, identity: { tag: "BUTTON", action: "open-hub-project" } });
        if (await cdp.evaluate(`Boolean(document.querySelector('#shared-environment, #shared-title, #shared-job-kind'))`))
          throw fail("The shared chat still asks the person to choose a PC, title or job type");
        await fill(byId("shared-prompt", "TEXTAREA"), "desktop-conversation-start: このプロジェクトで短く答えてください。");
        await captureScenarioScreenshot({ cdp, sink, name: "shared-conversation-before-send", owner: OWNER });
        await click(sharedActionTarget("submit"));
        const first = await wait("Ordinary Send creates one Hub job and completes it", shared,
          p => p.detail?.state === "succeeded" || p.error, 60000);
        if (first.error || !first.detail?.id || !first.detail.conversation_id) throw fail("Common Send did not create a Hub job", { error: first.error, detail: first.detail });
        const firstJobId = first.detail.id, conversationId = first.detail.conversation_id;
        await wait("First request and result appear in the same ordinary chat", () => cdp.evaluate(`document.querySelector('[data-shared-region="history-container"]')?.innerText`),
          text => text?.includes("desktop-conversation-start") && text.includes("最初の依頼をこのプロジェクトで実行しました。"));
        await fill(byId("shared-followup", "TEXTAREA"), "desktop-conversation-followup: 同じ会話で続けてください。");
        await click(sharedActionTarget("continue"));
        const second = await wait("Additional Send creates a later turn in the same Hub conversation", shared,
          p => p.detail?.id !== firstJobId && (p.detail?.state === "succeeded" || p.error), 60000);
        if (second.error || second.detail?.conversation_id !== conversationId) throw fail("Follow-up left the original conversation", { error: second.error, detail: second.detail, conversationId });
        await wait("Both user turns and answers remain readable", () => cdp.evaluate(`document.querySelector('[data-shared-region="history-container"]')?.innerText`),
          text => text?.includes("desktop-conversation-start") && text.includes("desktop-conversation-followup")
            && text.includes("最初の依頼をこのプロジェクトで実行しました。") && text.includes("追加の依頼にも、同じ共有チャットで回答しました。"));
        await fill(byId("shared-followup", "TEXTAREA"), "desktop-conversation-stop: この依頼を実行中に停止してください。");
        await click(sharedActionTarget("continue"));
        const stopping = await wait("The latest job exposes Stop while the provider is still working", shared,
          p => p.detail?.id !== second.detail.id && p.status?.jobs?.some(job => job.id === p.detail?.id && job.can_cancel)
            && state.provider.requests.some(request =>
            JSON.stringify(request.messages?.filter(message => message.role === "user")).includes("desktop-conversation-stop")), 60000);
        const stoppedJobId = stopping.detail.id;
        await wait("This PC shows the received work while it is running", () => cdp.evaluate(`Boolean(document.querySelector('section.receiver-activity'))`),
          visible => visible === true, 15000);
        await captureScenarioScreenshot({ cdp, sink, name: "shared-conversation-running-stop", owner: OWNER });
        await click(sharedActionTarget("cancel", stoppedJobId));
        const stopped = await wait("Stop has settled and the latest message can be edited", shared,
          p => p.detail?.id === stoppedJobId && p.detail.state === "cancelled" && p.detail.can_revise, 60000);
        if (stopped.detail.conversation_id !== conversationId) throw fail("Stop left the shared conversation", { stoppedJobId, conversationId });
        await click(sharedActionTarget("start-revise", stoppedJobId));
        await wait("The latest stopped message opens in the ordinary editor", () => cdp.evaluate(`({
          input: Boolean(document.querySelector('#shared-revise-prompt')),
          warning: document.querySelector('[data-shared-region="revision-editor"]')?.innerText || ''
        })`), value => value.input && value.warning.includes("作成済みのファイルや起動中のアプリは元に戻りません"));
        await fill(byId("shared-revise-prompt", "TEXTAREA"), "desktop-conversation-revised: 停止した最新の依頼を修正して回答してください。");
        await click(sharedActionTarget("save-revise"));
        const revised = await wait("Resend creates a new job in the same conversation", shared,
          p => p.detail?.id !== stoppedJobId && (p.detail?.state === "succeeded" || p.error), 60000);
        if (revised.error || revised.detail?.conversation_id !== conversationId || revised.detail?.revises_job_id !== stoppedJobId)
          throw fail("The edited latest message did not create the expected conversation revision", { error: revised.error,
            detail: revised.detail, conversationId, stoppedJobId });
        await wait("The revised answer and prior turns stay readable", () => cdp.evaluate(`document.querySelector('[data-shared-region="history-container"]')?.innerText`),
          text => text?.includes("desktop-conversation-start") && text.includes("desktop-conversation-followup")
            && text.includes("編集後の依頼をこのプロジェクトで実行しました。") && text.includes("編集前の依頼"));
        await wait("The Runner releases this PC after the revised work finishes", () => invokeDesktopCommand(cdp, "receiver_activity_projection"),
          p => !p.unavailable && p.attempts.length === 0 && p.retained_services.length === 0, 30000);
        await wait("The actual chat clears its receiver-use banner", () => cdp.evaluate(`Boolean(document.querySelector('section.receiver-activity'))`),
          visible => visible === false, 15000);
        if (state.provider.failures.length) throw fail("The provider fixture rejected the current agent route", { failures: state.provider.failures });
        await captureScenarioScreenshot({ cdp, sink, name: "shared-conversation-stop-edit-result", owner: OWNER });
        await sink.record("shared-conversation-continuation-complete", { project_id: projectId, environment_id: environmentId,
          folder: projectFolder, first_job_id: firstJobId, second_job_id: second.detail.id, conversation_id: conversationId,
          stopped_job_id: stoppedJobId, revised_job_id: revised.detail.id, provider_calls: state.provider.requests.length,
          scope: "One Windows PC with actual Tauri Desktop, independent Runner, Hub and GUI picker. Physical WinB is not exercised." },
          { phase: "executing", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "pending" };
      } catch (error) {
        await sink.record("shared-continuation-failure-state", { desktop: await invokeDesktopCommand(cdp, "desktop_state"), shared: await shared(),
          execution: await execution(), receiver: await invokeDesktopCommand(cdp, "receiver_activity_projection") },
          { phase: "executing", owner: OWNER }).catch(() => {});
        await captureScenarioScreenshot({ cdp, sink, name: "shared-continuation-failure", owner: OWNER }).catch(() => {});
        if (state.consent && !state.runner?.identity) await state.runner.capture(runtime.desktop_process_id).catch(() => {});
        throw error;
      } finally { await settleInput(); }
    },
    async quiesce() { await settleInput(); state.close ??= await quiesceDeviceExecutionResources(state);
      return { input: state.close.pass ? "pass" : "fail", resources: [{ kind: ID, ...state.close }] }; },
    async cleanup() { return { input: state.close?.pass ? "pass" : "fail", resources: [] }; },
  });
}

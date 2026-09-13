import { createHash, randomUUID } from "node:crypto";
import { mkdir, readFile, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { DesktopE2eError } from "../core/execution.mjs";
import { normalizeHubBrowserOptions, startHubBrowserResource } from "../drivers/hub_browser_resource.mjs";
import { startSharedWorkflowProvider, startSharedWorkflowRunner } from "../drivers/shared_work_runner_fixture.mjs";
import { WebviewInput, assertTrustedProbeSequence } from "../drivers/webview_input.mjs";
import { snapshotOwnedTopLevelWindows, selectFreshOwnedRootWindow, openFilePathInOwnedNativeDialog, probeExactOwnedWindow } from "../drivers/windows_native_input.mjs";
import { auditClosedSqlite } from "../drivers/sqlite_cleanup.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";
import { action, byId, trustedClick, trustedFocus, wait, enrollDesktopFromHubBrowser, requestHubEnrollmentExit } from "./hub_browser_enrollment.mjs";

const ID = "settings.shared-work-continuation", OWNER = `scenario:${ID}`;
const fail = message => new DesktopE2eError("product", "shared-continuation-mismatch", message);
const scopedSink = (sink, prefix) => ({ record: sink.record.bind(sink), writeBytes: (name, bytes) => sink.writeBytes(`${prefix}/${name}`, bytes) });

export function createSharedWorkContinuationScenario(options = {}) {
  const { runnerBinary, runnerTestBinary, ...hubOptions } = options;
  const settings = normalizeHubBrowserOptions(hubOptions);
  const state = { resource: null, provider: null, runner: null, input: null, close: null, primaryContext: null,
    nativeOwner: null, nativeCandidate: null, nativeBefore: null, importDispatched: false };
  async function settleInput() { if (state.input) { await state.input.cleanup(); state.input = null; } }
  return Object.freeze({ id: ID, productOracle: "pass", manualGate: "pending", databaseRequired: true,
    async prepare(args) {
      state.primaryContext = args.context;
      state.provider = await startSharedWorkflowProvider();
      await prepareDesktopFixture({ ...args, owner: OWNER, configText: `[model]\nbase_url = ${JSON.stringify(state.provider.baseUrl)}\nmodel = "shared-workflow"\nprovider_profile = "openai_compatible"\nmax_retries = 0\n[multi_agent]\nenabled = false\n` });
      state.resource = await startHubBrowserResource({ ...args, options: settings });
    },
    requestGracefulExit: cdp => requestHubEnrollmentExit(cdp, state),
    async execute({ context, runtime, driver, host, sink }) {
      let cdp = driver, currentContext = context, currentRuntime = runtime;
      const { page, hub } = state.resource;
      const provider = state.provider;
      async function attach() {
        await cdp.call("Runtime.enable"); await cdp.call("DOM.enable");
        state.input = new WebviewInput(cdp, { probeId: `${ID}-${currentRuntime.generation}` }); await state.input.installProbe();
        const view = await invokeDesktopCommand(cdp, "desktop_state");
        if (view.overlay === "initial_setup") await trustedClick(state.input, cdp, byId("initial-setup-shared-work"), sink);
        else await trustedClick(state.input, cdp, action("show-shared-work", "aside.sidebar"), sink);
        await wait("Shared entry opens", () => invokeDesktopCommand(cdp, "desktop_state"), p => p.overlay === "shared_work");
      }
      async function click(kind, value = "") {
        const target = value ? { selector: `.shared-work button[data-action="shared-${kind}"][data-value=${JSON.stringify(value)}]`, identity: { tag: "BUTTON", action: `shared-${kind}` } } : action(`shared-${kind}`, ".shared-work");
        await trustedClick(state.input, cdp, target, sink);
      }
      async function fill(id, value, tag = "INPUT") {
        const target = byId(id, tag); await trustedClick(state.input, cdp, target, sink);
        await state.input.keyDown("Control"); await state.input.pressKey("a"); await state.input.keyUp("Control"); await state.input.insertText(target, value);
        if (["shared-title", "shared-prompt"].includes(id)) await sink.record("shared-draft-selection-observed", {
          after_field: id, environment: await cdp.evaluate(`document.getElementById('shared-environment')?.value`),
        }, { phase: "executing", owner: OWNER });
      }
      async function select(id, index, value) {
        const target = byId(id, "SELECT");
        const afterSequence = (await state.input.snapshotProbe()).sequence;
        const observations = [];
        async function observe(step) { observations.push({ step, ...await cdp.evaluate(`(() => { const input = document.getElementById(${JSON.stringify(id)}); return { value: input?.value, focused: document.activeElement === input, options: Array.from(input?.options ?? [], option => option.value) }; })()`) }); }
        await trustedClick(state.input, cdp, target, sink); await observe("click");
        await state.input.pressKey("Home"); await observe("home");
        for (let n = 0; n < index; n++) { await state.input.pressKey("ArrowDown"); await observe(`down-${n + 1}`); }
        await state.input.pressKey("Enter"); await observe("enter");
        const probe = await state.input.snapshotProbe(afterSequence);
        const events = probe.events.filter(event => event.id === id && ["keydown", "keyup", "input", "change"].includes(event.type));
        await sink.record("shared-select-observed", { id, index, observations, events }, { phase: "executing", owner: OWNER });
        assertTrustedProbeSequence(probe, { afterSequence, expected: [
          { type: "input", identity: target.identity }, { type: "change", identity: target.identity },
        ] });
        await wait("Shared selection committed its requested value", () => cdp.evaluate(`document.getElementById(${JSON.stringify(id)})?.value`), current => current === value);
      }
      async function selectValue(id, value) {
        const observed = await cdp.evaluate(`Array.from(document.getElementById(${JSON.stringify(id)}).options, option => option.value)`);
        const index = observed.indexOf(value);
        if (index < 0) throw fail(`Option ${value} is absent from ${id}`);
        await select(id, index, value);
      }
      const projection = () => invokeDesktopCommand(cdp, "shared_work_projection");
      async function focusDefaultDeadline(id) {
        await trustedClick(state.input, cdp, byId(id, "INPUT"), sink);
        const value = await cdp.evaluate(`(() => { const input = document.getElementById(${JSON.stringify(id)}); return { focused: document.activeElement === input, type: input?.type, value: input?.value }; })()`);
        if (!value.focused || value.type !== "datetime-local" || value.value !== "") throw fail("The default start deadline is not an interactive empty date field");
      }
      async function checkReadableTranscript() {
        await wait("Canonical request and answer are shown as readable text", () => cdp.evaluate(`Array.from(document.querySelectorAll('[data-shared-region="transcript"] .markdown-body'), node => node.textContent).join('\\n')`), text => text.includes("desktop-transfer-parent") && text.includes("親は子の解析結果"));
        await wait("Job result shows the answer before internal metadata", () => cdp.evaluate(`document.querySelector('[data-shared-region="detail"]')?.innerText`), text => text.includes("親は子の解析結果") && !text.includes("admission_revision"));
        for (const [region, image] of [["detail", "shared-b-readable-result"], ["transcript", "shared-b-readable-conversation"]]) {
        const selector = `[data-shared-region="${region}"] details[data-details-key]`;
        const first = await cdp.evaluate(`(() => { const items = [...document.querySelectorAll(${JSON.stringify(selector)})]; return { key: items[0]?.dataset.detailsKey, open: items.some(item => item.open) }; })()`);
        if (!first.key || first.open) throw fail("Transcript technical details must begin collapsed for a fresh person/job");
        const exact = `${selector}[data-details-key=${JSON.stringify(first.key)}]`;
        const summary = { selector: `${exact} > summary`, identity: { tag: "DETAILS", detailsKey: first.key } };
        for (const open of [true, false]) {
          const observed = (await projection()).observed_at_ms;
          const refreshes = new Set();
          await trustedClick(state.input, cdp, summary, sink);
          await wait("Two subsequent shared refreshes retain the disclosure state", async () => {
            const view = await projection(); if (view.observed_at_ms > observed) refreshes.add(view.observed_at_ms);
            return { count: refreshes.size, open: await cdp.evaluate(`document.querySelector(${JSON.stringify(exact)})?.open`) };
          }, value => value.count >= 2 && value.open === open);
          await captureScenarioScreenshot({ cdp, sink, name: open ? `${image}-details-open` : image, owner: OWNER });
        }
        }
      }
      async function nativeFile(kind, selectedPath, value = "") {
        state.nativeOwner = { executionRoot: currentContext.root, ownerPath: currentRuntime.desktop_owner_path, expectedOwner: currentRuntime.desktop_owner };
        state.nativeBefore = await snapshotOwnedTopLevelWindows(state.nativeOwner); state.importDispatched = true;
        await click(kind, value);
        const native = await wait("Shared file picker belongs to exact Desktop", async () => {
          try { return selectFreshOwnedRootWindow(state.nativeBefore, await snapshotOwnedTopLevelWindows(state.nativeOwner), currentRuntime.desktop_owner, { expectedClassName: "#32770" }); }
          catch (error) { if (error?.code === "native-window-cardinality" && error.evidence?.fresh_windows?.length === 0) return null; throw error; }
        }, Boolean);
        state.nativeCandidate = native;
        const nativeResult = await openFilePathInOwnedNativeDialog({ ...state.nativeOwner, candidate: native, selectedPath, intent: kind === "save-asset" ? "save_new" : kind === "provider-prepare" ? "directory" : "open" });
        await sink.record("shared-native-path-selection", { action: kind, result: nativeResult }, { phase: "executing", owner: OWNER });
        await wait("Shared native picker closes", () => probeExactOwnedWindow({ ...state.nativeOwner, candidate: native }), value => !value.live);
        state.nativeCandidate = null; state.importDispatched = false;
      }
      try {
        await attach();
        await page.locator('nav a[href="#device-network"]').click();
        await page.locator("#network-ip").fill("127.0.0.1"); await page.locator("#network-port").fill(String(hub.networkPort));
        await page.locator("#network-start").click(); await page.locator("#network-stop").waitFor();
        const enrolled = await enrollDesktopFromHubBrowser({ resource: { ...state.resource, screenshot: name => state.resource.screenshot(`a-${name}`) }, context, runtime, cdp, input: state.input, sink: scopedSink(sink, "desktop-a"), nativeState: state, entry: "shared-work" });
        const network = await hub.observeNetwork();
        const { createDeviceParticipant } = await import(pathToFileURL(path.join(settings.hubRepository, "tests/browser/device-fixture.mjs")));
        const actor = await createDeviceParticipant(network, "Shared workflow setup");
        await page.locator('nav a[href="#clients"]').click(); await page.locator("#network-clients-refresh").click();
        await page.locator(`[data-id="request:${actor.requestId}"] button[data-network-action]`).click(); await actor.collectApproval(); await actor.presence();
        await actor.sharedLogin(state.resource.administrator.username, state.resource.administrator.password);
        const password = randomUUID();
        const alice = await actor.sharedCall("createUser", { username: "workflow-alice", display_name: "担当 Alice", password, administrator: false });
        const bob = await actor.sharedCall("createUser", { username: "workflow-bob", display_name: "担当 Bob", password, administrator: false });
        await actor.sharedCall("createProject", { id: "workflow", label: "端末を移る共有業務" });
        for (const user of [alice, bob]) await actor.sharedCall("membership", { project_id: "workflow", user_id: user.user_id, role: "contributor" });
        for (const [id, label] of [["analysis", "親の解析"], ["solver", "子の解析"]]) await actor.sharedCall("environment", { id, label, resource_id: "workflow-device", runner_id: enrolled.network.device_id, capacity: 1, project_ids: ["workflow"] });
        state.runner = await startSharedWorkflowRunner({ context, deviceId: enrolled.network.device_id, hubId: network.hub_id, runnerBinary: runnerBinary ?? path.join(path.dirname(context.binary), "moyai-runner.exe"), runnerTestBinary, sink });
        await click("provider-status"); await wait("A observes its actual local Runner", projection, p => p.provider?.runner_id === state.runner.incarnation);
        await click("provider-pause"); await wait("OS-local providing can pause before human login", projection, p => p.principal === null && p.provider?.accepting === false);
        await click("provider-resume");
        await wait("The actual Runner applies its resumed mode", () => state.runner.command(["operations", "--runner", state.runner.incarnation]), value => value.projection?.accepting === true);
        await click("provider-status"); await wait("The same actual Runner resumes accepting", projection, p => p.provider?.accepting === true);
        await captureScenarioScreenshot({ cdp, sink, name: "shared-a-provider-resumed", owner: OWNER });
        const provisionRoot = path.join(state.runner.root, "operator-approved-root"); await mkdir(provisionRoot);
        await fill("shared-templateLabel", "共有作業の領域");
        await nativeFile("provider-prepare", provisionRoot);
        const prepared = await wait("A name and native folder selection prepare the preset with a stable generated ID", projection, p => p.provider_draft?.label === "共有作業の領域" && p.provider_draft.access_mode === "default" && path.resolve(p.provider_draft.base_root) === provisionRoot);
        const templateId = prepared.provider_draft.id;
        if (!/^folder-[0-9a-f-]{36}$/.test(templateId)) throw fail("A new preset must receive its generated stable draft ID");
        const optionsKey = await cdp.evaluate(`document.querySelector('[data-shared-region="provider"] details[data-details-key^="shared-provider-options:"]')?.dataset.detailsKey`);
        const optionsSummary = { selector: '[data-shared-region="provider"] details[data-details-key^="shared-provider-options:"] > summary', identity: { tag: "DETAILS", detailsKey: optionsKey } };
        await trustedClick(state.input, cdp, optionsSummary, sink);
        const presetDefaults = await cdp.evaluate(`({ id: document.getElementById('shared-templateId').value, access: document.getElementById('shared-accessMode').value, scope: document.getElementById('shared-resourceScope').value })`);
        if (presetDefaults.id !== templateId || presetDefaults.access !== "default" || presetDefaults.scope !== "device") throw fail("The preset review must show the generated ID and existing safe defaults");
        await trustedClick(state.input, cdp, optionsSummary, sink);
        await captureScenarioScreenshot({ cdp, sink, name: "shared-provider-preset-review", owner: OWNER });
        await click("provider-install"); await wait("The real Runner publishes the prepared template", projection, p => p.provider?.templates.some(t => t.id === templateId));
        await selectValue("shared-editTemplateId", templateId);
        const copied = await cdp.evaluate(`({ label: document.getElementById('shared-templateLabel').value, id: document.getElementById('shared-templateId').value })`);
        if (copied.id !== templateId || copied.label !== "共有作業の領域") throw fail("Existing preset selection must reuse its current fields");
        await fill("shared-templateLabel", "共有作業の領域（確認済み）");
        await wait("Edited fields invalidate the earlier folder preview", () => cdp.evaluate(`document.querySelector('[data-action="shared-provider-install"]')?.disabled`), disabled => disabled === true);
        if ((await projection()).provider_draft.label !== "共有作業の領域") throw fail("Editing the frontend draft must not mutate the native-folder preview");
        await nativeFile("provider-prepare", provisionRoot);
        await wait("Native folder confirmation adopts the edited preset", projection, p => p.provider_draft?.id === templateId && p.provider_draft.label === "共有作業の領域（確認済み）");
        await click("provider-install"); await wait("The same preset is updated explicitly", projection, p => p.provider?.templates.some(t => t.id === templateId && t.label === "共有作業の領域（確認済み）"));
        await sink.record("shared-provider-simple-setup", { template_id: templateId, generated_id: true, default_access: presetDefaults.access, default_scope: presetDefaults.scope, existing_fields_reused: true, stale_preview_blocked: true }, { phase: "executing", owner: OWNER });
        await page.locator('nav a[href="#shared-administration"]').click();
        await page.locator('[data-sa-tab="environments"]').click();
        await page.locator("#shared-admin-content .shared-admin-row").filter({ hasText: "共有作業の領域（確認済み）" })
          .locator(`button[data-sa-template="${templateId}"][data-sa-runner="${enrolled.network.device_id}"]`).click();
        const adminSave = async () => { await page.locator("#shared-admin-save").click(); await page.locator("#shared-admin-form").waitFor({ state: "detached" }); };
        const environmentPreset = await page.locator('#shared-admin-form').evaluate(form => ({ id: form.querySelector('#shared-admin-id').value, label: form.querySelector('#shared-admin-label').value, runner: form.querySelector('#shared-admin-runner_id').value, template: form.querySelector('#shared-admin-template_id').value, resource: form.querySelector('#shared-admin-resource_id').value, capacity: form.querySelector('#shared-admin-capacity').value }));
        if (environmentPreset.runner !== enrolled.network.device_id || environmentPreset.template !== templateId || environmentPreset.resource !== "workflow-device" || environmentPreset.capacity !== "1") throw fail("The Hub preset must retain its exact Runner, template and current shared resource");
        if (!environmentPreset.id.startsWith("environment-") || environmentPreset.label !== "共有作業の領域（確認済み）") throw fail("The Hub preset must generate the environment ID and reuse the display name");
        const createdEnvironmentId = environmentPreset.id;
        await page.locator('input[name="project_ids"][value="workflow"]').check(); await adminSave();
        await page.locator("#shared-admin-content .shared-admin-row").filter({ hasText: environmentPreset.label }).filter({ hasText: "フォルダの準備完了" }).waitFor({ timeout: 30000 });
        await state.resource.screenshot("shared-hub-provisioned-environment");
        await click("provider-status");
        const provisioned = await wait("Desktop sees the Runner-created environment", projection, p => p.provider?.environments.some(e => e.environment_id === createdEnvironmentId));
        const createdDirectory = path.resolve(provisioned.provider.environments.find(e => e.environment_id === createdEnvironmentId).directory);
        const relativeCreated = path.relative(path.toNamespacedPath(provisionRoot), path.toNamespacedPath(createdDirectory));
        if (path.isAbsolute(relativeCreated) || relativeCreated.startsWith("..") || !relativeCreated || !(await stat(createdDirectory)).isDirectory()) throw fail("Provisioning escaped the operator-approved root or did not create a directory");
        await captureScenarioScreenshot({ cdp, sink, name: "shared-a-provisioned-local-environment", owner: OWNER });
        await click("provider-remove-template", templateId);
        await wait("Stopping template publication retains its created environment", projection, p => !p.provider?.templates.some(t => t.id === templateId) && p.provider?.environments.some(e => e.environment_id === createdEnvironmentId));
        await sink.record("shared-provisioning-actual-consumer", { environment_id: createdEnvironmentId, created_directory: createdDirectory, approved_root: provisionRoot, template_unpublished_mapping_retained: true }, { phase: "executing", owner: OWNER });
        await fill("shared-username", "workflow-alice"); await fill("shared-password", password); await click("login");
        await wait("Alice can submit to the real Runner", projection, p => p.principal?.user_id === alice.user_id && ["analysis", "solver", createdEnvironmentId].every(id => p.status?.environments.some(e => e.id === id)));
        const inputPath = path.join(context.paths.workspace, "input.txt"); await writeFile(inputPath, "input snapshot 日本語\n", { flag: "wx" });
        await nativeFile("upload-inputs", inputPath);
        const firstInput = await wait("Input upload is visible", projection, p => p.inputs.length === 1);
        await nativeFile("upload-inputs", inputPath);
        await wait("Selecting the same file replaces the draft attachment with a fresh upload intent", projection, p => p.inputs.length === 1 && p.inputs[0].id !== firstInput.inputs[0].id && p.feedback?.includes("置き換えました"));
        await selectValue("shared-environment", "analysis"); await fill("shared-title", "端末 A から依頼した親の仕事"); await fill("shared-prompt", "desktop-transfer-parent: 子の解析を待って結果をまとめてください。", "TEXTAREA");
        await focusDefaultDeadline("shared-startBefore"); const submittedAt = Date.now(); await click("submit");
        const parentView = await wait("Parent releases its slot while the actual child is running", projection, p => p.detail?.state === "waiting_child" && provider.requests.some(r => JSON.stringify(r.messages.filter(m => m.role === "user")).includes("desktop-transfer-child")), 60000);
        const parentId = parentView.detail.id, childId = parentView.detail.awaiting_child_id;
        if (!Number.isFinite(parentView.detail.start_before_ms) || parentView.detail.start_before_ms < submittedAt + 86400000 || parentView.detail.start_before_ms > Date.now() + 86400000) throw fail("Submitted root deadline was not fixed to 24 hours");
        await selectValue("shared-assigneeId", bob.user_id); await click("handover");
        await wait("Handover is pending at the running child", projection, p => p.handover?.pending?.new_assignee_id === bob.user_id);
        await captureScenarioScreenshot({ cdp, sink, name: "shared-a-waiting-child-handover", owner: OWNER });
        await settleInput();
        const secondRoot = path.join(context.root, "desktop-b"); await mkdir(secondRoot);
        const directories = Object.fromEntries(["workspace", "config", "data", "prefs", "webview"].map(name => [name, path.join(secondRoot, name)]));
        for (const directory of Object.values(directories)) await mkdir(directory);
        const nextContext = { ...context, paths: { ...context.paths, ...directories, config_file: path.join(directories.config, "config.toml"), prefs_file: path.join(directories.prefs, "desktop.toml"), database: path.join(directories.data, "moyai.sqlite3") } };
        await prepareDesktopFixture({ context: nextContext, sink, phase: "executing", owner: OWNER, configMode: "absent", sentinelName: null, sentinelText: "" });
        const restarted = await host.restart({ context, nextContext, scenario: this, sink, driver: cdp });
        cdp = restarted.driver; currentRuntime = restarted.runtime; currentContext = nextContext;
        await attach();
        const fresh = await invokeDesktopCommand(cdp, "desktop_state"); if (!fresh.startup.initial_setup_required) throw fail("Desktop B unexpectedly has local model setup");
        await page.locator('nav a[href="#device-network"]').click();
        const second = await enrollDesktopFromHubBrowser({ resource: { ...state.resource, screenshot: name => state.resource.screenshot(`b-${name}`) }, context: currentContext, runtime: currentRuntime, cdp, input: state.input, sink: scopedSink(sink, "desktop-b"), nativeState: state, entry: "shared-work", onEnrollmentError: async rejected => {
          if (rejected.error !== "device_name_conflict") throw fail("Unexpected B enrollment rejection");
          await wait("Shared entry explains B's duplicate registered name", () => cdp.evaluate(`document.querySelector('[data-shared-region="message"]')?.textContent`), value => value?.includes("同じ端末名が既に登録されています"));
          await captureScenarioScreenshot({ cdp, sink, name: "shared-b-enrollment-error", owner: OWNER });
          // A and B use distinct identities on one host. Resolve the real name
          // conflict through Hub management, then retry B's existing request UI.
          await page.locator('nav a[href="#clients"]').click();
          await page.locator("#network-clients-refresh").click();
          const firstDevice = page.locator(`[data-id="device:${enrolled.network.device_id}"]`);
          await firstDevice.locator("summary").click();
          await firstDevice.getByRole("button", { name: /を管理$/ }).click();
          await page.locator("#network-device-label").fill("共有 Runner A");
          await page.locator("#network-device-save").click();
          await page.locator("#network-device-dialog").waitFor({ state: "hidden" });
          await wait("A's registered name is distinct for this same-host B fixture", () => hub.observeNetwork(), p => p.devices.some(d => d.device_id === enrolled.network.device_id && d.label === "共有 Runner A"));
          await click("reconnect");
        } });
        if (second.network.device_id === enrolled.network.device_id) throw fail("B reused the same device identity");
        await fill("shared-username", "workflow-alice"); await fill("shared-password", password); await click("login");
        await wait("Fresh B sees the same parent job", projection, p => p.status?.jobs.some(j => j.id === parentId));
        provider.releaseChild();
        await click("detail", childId);
        const approval = await wait("B receives the actual child's write approval", projection, p => p.approval?.can_decide && p.approval.status === "pending", 60000);
        const marker = path.join(state.runner.child, "approved-marker.txt");
        if (await stat(marker).then(() => true, () => false)) throw fail("Controlled shell effect occurred before B's approval");
        if (!approval.approval.request.details.some(text => text.includes("approved-marker.txt"))) throw fail("Approval does not identify the controlled shell effect");
        await trustedFocus(state.input, cdp, action("shared-approve", ".shared-work"));
        await captureScenarioScreenshot({ cdp, sink, name: "shared-b-real-child-approval", owner: OWNER });
        await click("approve", approval.approval.id);
        await wait("Only B approval allows the controlled shell effect", () => readFile(marker, "utf8").catch(() => null), text => text === "approved");
        await wait("The parent completes after actual child output and safe handover", projection, p => p.status?.jobs.some(j => j.id === parentId && j.state === "succeeded" && j.assignee.user_id === bob.user_id), 60000);
        await wait("Child result file is stored on Hub", projection, p => p.assets.some(a => a.name === "result.txt" && a.kind === "artifact"));
        const asset = (await projection()).assets.find(a => a.name === "result.txt" && a.kind === "artifact");
        const saved = path.join(currentContext.paths.workspace, "downloaded-result.txt"); await nativeFile("save-asset", saved, asset.id);
        await wait("B saves verified artifact bytes", () => readFile(saved).catch(() => null), bytes => bytes !== null && createHash("sha256").update(bytes).digest("hex") === asset.sha256 && bytes.toString("utf8").replaceAll("\r\n", "\n") === "shared solver result 日本語\n");
        await click("logout"); await fill("shared-username", "workflow-bob"); await fill("shared-password", password); await click("login");
        const notified = await wait("New assignee receives a handover notification", projection, p => p.principal?.user_id === bob.user_id && p.inbox?.items.some(i => i.kind === "handover" && i.job_id === parentId));
        const notification = notified.inbox.items.find(i => i.kind === "handover" && i.job_id === parentId);
        await click("inbox-open", notification.id);
        await wait("Opening the notification marks it read and restores the Hub conversation", projection, p => p.detail?.id === parentId && p.detail.can_continue && p.transcript?.items.length && p.inbox?.items.some(i => i.id === notification.id && i.read_at_ms !== null));
        await checkReadableTranscript();
        await fill("shared-followup", "desktop-followup: 元の解析結果を参照して追加の説明をしてください。", "TEXTAREA");
        await focusDefaultDeadline("shared-followupStartBefore"); const continuedAt = Date.now(); await click("continue");
        const continued = await wait("Continuation finishes from Hub canonical history", projection, p => p.detail?.id !== parentId && p.detail?.state === "succeeded", 60000);
        if (!Number.isFinite(continued.detail.start_before_ms) || continued.detail.start_before_ms < continuedAt + 86400000 || continued.detail.start_before_ms > Date.now() + 86400000) throw fail("Continued job deadline was not fixed to 24 hours");
        await sink.record("shared-accepted-start-deadlines", { parent: { job_id: parentId, requested_after_ms: submittedAt, start_before_ms: parentView.detail.start_before_ms }, continuation: { job_id: continued.detail.id, requested_after_ms: continuedAt, start_before_ms: continued.detail.start_before_ms }, default_hours: 24 }, { phase: "executing", owner: OWNER });
        await wait("The new job has its own collapsed technical details", () => cdp.evaluate(`(() => { const region = document.querySelector('[data-shared-region="transcript"]'); return { owner: region?.dataset.sharedRecordOwner, open: [...document.querySelectorAll('[data-shared-region="transcript"] details, [data-shared-region="detail"] details')].some(d => d.open) }; })()`), value => decodeURIComponent(value.owner ?? "").includes(continued.detail.id) && !value.open);
        const resultKey = await cdp.evaluate(`document.querySelector('[data-shared-region="detail"] details')?.dataset.detailsKey`);
        await trustedFocus(state.input, cdp, { selector: `[data-shared-region="detail"] details > summary`, identity: { tag: "DETAILS", detailsKey: resultKey } });
        if (provider.failures.length) throw fail(provider.failures.join("; "));
        await captureScenarioScreenshot({ cdp, sink, name: "shared-b-canonical-continuation-result", owner: OWNER });
        await sink.record("shared-a-to-b-actual-runner-complete", { parent_id: parentId, child_id: childId, continued_id: continued.detail.id, device_a: enrolled.network.device_id, device_b: second.network.device_id, no_local_setup_on_b: true, runner_incarnation: state.runner.incarnation, artifact: { id: asset.id, sha256: asset.sha256, saved }, provider_calls: provider.requests.length, scope: "Two isolated Desktop device identities on one Windows account; actual Runner, Hub, native input/save, human approval and canonical continuation. Physical different PCs and external solver are not exercised." }, { phase: "executing", owner: OWNER });
        await sink.writeJson("shared-workflow-provider-requests.json", provider.requests);
        return { acquisition: "pass", oracle: "pass", manual: "pending" };
      } catch (error) {
        await sink.record("shared-continuation-failure-state", { desktop: await invokeDesktopCommand(cdp, "desktop_state"), shared: await projection() }, { phase: "executing", owner: OWNER }).catch(() => {});
        await captureScenarioScreenshot({ cdp, sink, name: "shared-continuation-failure", owner: OWNER }).catch(() => {});
        await state.resource.screenshot("shared-hub-continuation-failure").catch(() => {}); throw error;
      }
      finally { await settleInput(); }
    },
    async quiesce() {
      await settleInput();
      if (!state.close) {
        const runner = state.runner ? await state.runner.close() : { pass: true };
        await state.provider?.close();
        const sqlite = state.primaryContext ? await auditClosedSqlite({ executionRoot: state.primaryContext.root, database: state.primaryContext.paths.database, required: true }) : { pass: true };
        const hub = state.resource ? await state.resource.close() : { pass: true };
        state.close = { pass: runner.pass && sqlite.pass && hub.pass, runner, sqlite, hub };
      }
      return { input: state.close.pass ? "pass" : "fail", resources: [{ kind: ID, ...state.close }] };
    },
    async cleanup() { return { input: state.close?.pass ? "pass" : "fail", resources: [] }; },
  });
}

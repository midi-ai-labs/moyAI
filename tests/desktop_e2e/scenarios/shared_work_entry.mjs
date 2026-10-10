import { randomUUID } from "node:crypto";
import { pathToFileURL } from "node:url";
import path from "node:path";
import { readFile } from "node:fs/promises";
import { DesktopE2eError } from "../core/execution.mjs";
import { normalizeHubBrowserOptions, startHubBrowserResource } from "../drivers/hub_browser_resource.mjs";
import { WebviewInput } from "../drivers/webview_input.mjs";
import { DesktopCommandProbe, assertExactDesktopCommandSequence } from "../drivers/desktop_command_probe.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";
import { action, byId, hubSettingsCloseTarget, trustedClick, trustedFocus, wait, enrollDesktopFromHubBrowser, requestHubEnrollmentExit } from "./hub_browser_enrollment.mjs";
import { hubProjectReady, observeSharedWorkSurface, sharedWorkSurfaceMatches, openHubProjectSurface, openSharedDisclosure, rememberedRestartAccepted, sharedActionTarget, openSharedJob, bindHubDevice, waitForSharedComposer } from "./shared_work_navigation.mjs";

const ID = "settings.shared-work", OWNER = `scenario:${ID}`;
const failure = (message, evidence) => new DesktopE2eError("product", "shared-work-mismatch", message, evidence);
export function sharedEntryReady(value) {
  return hubProjectReady(value) && value.startup?.initial_setup_required === true;
}
export function sharedSettingsClosed(value, before) {
  return hubProjectReady(value) && typeof before?.startup?.initial_setup_required === "boolean"
    && value.startup?.initial_setup_required === before.startup.initial_setup_required
    && value.startup?.status === before.startup.status;
}
export function sharedApprovalParked(value, expected) {
  const approval = value?.approval;
  return value?.detail?.id === expected.jobId && value.detail.state === "running"
    && approval?.id === expected.approvalId && approval.attempt_id === expected.attemptId
    && approval.context?.job_id === expected.jobId && approval.status === "expired"
    && approval.decision === null && approval.can_decide === false && approval.can_reconfirm === true
    && Number.isFinite(approval.expires_at_ms) && value.observed_at_ms >= approval.expires_at_ms
    && value.status?.environments?.some(row => row.id === expected.environmentId && row.occupied === 1);
}
export function sharedRenameEditorRetained(value, expected) {
  return value?.connected === true && value.same_node === true && value.focused === true
    && value.value === expected.value && value.selection_start === expected.selection_start
    && value.selection_end === expected.selection_end && value.selection_direction === expected.selection_direction;
}
export function createSharedWorkEntryScenario(options = {}) {
  const settings = normalizeHubBrowserOptions(options);
  const state = { resource: null, input: null, commands: null, nativeOwner: null, nativeCandidate: null, nativeBefore: null, importDispatched: false, failures: [], close: null, notificationLog: null };
  async function settle() {
    if (state.input) { try { await state.input.cleanup(); } catch { state.failures.push("input-cleanup"); } state.input = null; }
    if (state.commands) { try { await state.commands.remove(); } catch { state.failures.push("command-cleanup"); } state.commands = null; }
  }
  return Object.freeze({ id: ID, productOracle: "pass", manualGate: "pending", databaseRequired: true,
    get environment() { return state.notificationLog ? { MOYAI_NOTIFICATION_DEBUG_LOG: state.notificationLog } : {}; },
    async prepare(args) {
      state.notificationLog = path.join(args.context.root, "native-notifications.log");
      await prepareDesktopFixture({ ...args, owner: OWNER, configMode: "absent", sentinelName: null, sentinelText: "" });
      state.resource = await startHubBrowserResource({ ...args, options: settings });
    },
    requestGracefulExit: cdp => requestHubEnrollmentExit(cdp, state),
    async execute({ context, runtime, driver, host, sink }) {
      let cdp = driver, input;
      const resource = state.resource, { page, hub } = resource;
      async function attach() {
        await cdp.call("Runtime.enable"); await cdp.call("DOM.enable");
        input = state.input = new WebviewInput(cdp, { probeId: ID }); await input.installProbe();
        state.commands = new DesktopCommandProbe(cdp, { probeId: ID, commands: ["shared_work_command"] }); await state.commands.install();
      }
      async function restart() {
        await settle();
        const result = await host.restart({ context, scenario: thisScenario, sink, driver: cdp });
        cdp = result.driver; await attach();
        await wait("Restart keeps setup complete without local model setup", () => invokeDesktopCommand(cdp, "desktop_state"),
          p => p.overlay === "none" && !p.busy && !p.startup.initial_setup_required);
        await openHubProjectSurface(input, cdp, sink);
        return result.restart;
      }
      const thisScenario = this;
      await attach();
      try {
        await wait("Fresh Desktop initial setup", () => invokeDesktopCommand(cdp, "desktop_state"), p => p.overlay === "initial_setup" && p.startup.initial_setup_required);
        await trustedClick(input, cdp, byId("initial-setup-shared-work"), sink);
        const entry = await wait("Projectless shared entry keeps local setup unfinished", () => invokeDesktopCommand(cdp, "desktop_state"), sharedEntryReady);
        await captureScenarioScreenshot({ cdp, sink, name: "shared-projectless-entry", owner: OWNER });
        await page.locator('nav a[href="#device-network"]').click();
        await page.locator("#network-ip").fill("127.0.0.1"); await page.locator("#network-port").fill(String(hub.networkPort));
        await page.locator("#network-start").click(); await page.locator("#network-stop").waitFor();
        const network = await hub.observeNetwork();
        // Fixture-only real mTLS actor provisions users/projects and occupies one shared resource.
        const { createDeviceParticipant } = await import(pathToFileURL(path.join(settings.hubRepository, "tests/browser/device-fixture.mjs")));
        const participant = await createDeviceParticipant(network, "Shared fixture runner");
        await page.locator('nav a[href="#clients"]').click(); await page.locator("#network-clients-refresh").click();
        await page.locator(`[data-id="request:${participant.requestId}"] button[data-network-action]`).click();
        await page.locator("#join-project-save").click();
        await page.locator("#join-project-dialog").waitFor({ state: "hidden" });
        const approved = await participant.collectApproval(); await participant.presence();
        const bob = (await participant.sharedDeviceSession()).principal;
        await bindHubDevice(resource, approved.deviceId, resource.administrator.user_id);
        await participant.sharedDeviceSession();
        // A PC label is deliberately public to its authorized project. Give
        // the private actor a distinct name so the privacy oracle can tell
        // the actor from that legitimately displayed execution PC.
        bob.display_name = "非公開の利用者 Bob";
        const actorSetup = await participant.sharedCall("snapshot");
        await participant.sharedCall("command", { request_id: randomUUID(), expected_revision: actorSetup.revision,
          request: { kind: "update_user", user_id: bob.user_id, display_name: bob.display_name, administrator: false, disabled: false } });
        await page.locator('nav a[href="#device-network"]').click();
        const enrolled = await enrollDesktopFromHubBrowser({ resource, context, runtime, cdp, input, sink, nativeState: state, entry: "shared-work" });
        const alice = (await wait("New Desktop uses its dedicated device actor", () => invokeDesktopCommand(cdp, "shared_work_projection"), p => p.principal && !p.principal.administrator)).principal;
        for (const [id, label, user] of [["project-a", "解析プロジェクト A", alice], ["project-b", "解析プロジェクト B", bob]]) {
          await participant.sharedCall("createProject", { id, label });
          await participant.sharedCall("membership", { project_id: id, user_id: user.user_id, role: "contributor" });
          await participant.sharedCall("environment", { id: `env-${id}`, label: `解析環境 ${id.slice(-1).toUpperCase()}`, resource_id: "shared-solver", runner_id: approved.deviceId, capacity: 1, project_ids: [id] });
        }
        await bindHubDevice(resource, approved.deviceId, bob.user_id);
        await participant.sharedDeviceSession();
        const occupied = await participant.sharedCall("submit", { request_id: randomUUID(), project_id: "project-b", environment_id: "env-project-b", title: "Bobだけが見える仕事", input: { version: 1, prompt: "Fixture retained shared work" }, descendant_budget: 0 });
        const assignment = await participant.sharedCall("claim", { environment_ids: ["env-project-b"] });
        async function fill(id, value, tag = "INPUT") {
          // The composer is already visible. Use the shared pointer driver;
          // keyboard traversal after replacing a new-chat form is a separate gate.
          const target = byId(id, tag);
          await waitForSharedComposer(input, target);
          await input.click(target, { stableHitSamples: 3 });
          await input.insertText(target, value);
          await wait("Composer retains the exact entered draft", () => cdp.evaluate(`document.getElementById(${JSON.stringify(id)})?.value`), actual => actual === value);
        }
        async function click(name, value) {
          const target = sharedActionTarget(name, value);
          await trustedFocus(input, cdp, target);
          await waitForSharedComposer(input, target);
          await trustedClick(input, cdp, target, sink);
        }
        const aliceView = await wait("Alice sees only her project and redacted occupied resource", () => invokeDesktopCommand(cdp, "shared_work_projection"), p => p.principal?.user_id === alice.user_id && p.projects.length === 1 && p.status?.environments[0]?.other_occupants === 1);
        const credentialCalls = (await state.commands.snapshot()).calls.filter(call => ["login", "setup_password"].includes(call.args?.request?.kind));
        if (credentialCalls.length) throw failure("Device approval still required an application login", {});
        await sink.record("shared-device-ready", { user_id: alice.user_id, password_commands: 0 }, { phase: "executing", owner: OWNER });
        const aliceText = await cdp.evaluate("document.querySelector('.shared-work').textContent");
        if (aliceText.includes("Bobだけが見える仕事") || aliceText.includes(bob.display_name)) throw failure("Unauthorized occupancy details leaked", {});
        await captureScenarioScreenshot({ cdp, sink, name: "shared-alice-occupancy", owner: OWNER });
        const projectButton = { selector: '.sidebar button[data-action="open-hub-project"][data-value="project-a"]', identity: { tag: "BUTTON", action: "open-hub-project" } };
        await trustedClick(input, cdp, projectButton, sink);
        await click("new-conversation");
        if (await cdp.evaluate("document.querySelector('#shared-environment') !== null")) throw failure("A single execution PC must not require a selector", {});
        await fill("shared-prompt", "作り直す前の下書き", "TEXTAREA");
        await click("new-conversation");
        await wait("New chat clears an unsent draft without submitting it", () => cdp.evaluate("document.querySelector('#shared-prompt')?.value"), value => value === "");
        const prompt = "共有環境で解析してください。";
        await fill("shared-prompt", prompt, "TEXTAREA");
        const beforePoll = await cdp.evaluate("document.querySelector('#shared-prompt').value");
        const revision = (await invokeDesktopCommand(cdp, "shared_work_projection")).revision;
        await wait("Polling keeps the focused draft connected", async () => ({ p: await invokeDesktopCommand(cdp, "shared_work_projection"), value: await cdp.evaluate("document.querySelector('#shared-prompt').value") }), value => BigInt(value.p.revision) > BigInt(revision) && value.value === beforePoll);
        await input.keyDown("Control"); try { await input.pressKey("Enter"); } finally { await input.keyUp("Control"); }
        const submitted = await wait("The normal composer generates a chat name and queues at the selected PC", () => invokeDesktopCommand(cdp, "shared_work_projection"), p => p.status?.jobs.some(j => j.title === prompt && j.state === "queued"));
        const job = submitted.status.jobs.find(j => j.title === prompt);
        await openSharedJob(input, cdp, sink, job.id);
        await wait("Job detail displays the submitted prompt", () => invokeDesktopCommand(cdp, "shared_work_projection"), p => p.detail?.id === job.id && p.detail.input?.prompt === "共有環境で解析してください。");
        const conversationId = submitted.selected_conversation_id ?? job.conversation_id;
        if (!conversationId) throw failure("Submitted work has no shared conversation identity", { job });
        await trustedClick(input, cdp, { selector: `.sidebar button[data-action="shared-start-rename-conversation"][data-value=${JSON.stringify(conversationId)}]`,
          identity: { tag: "BUTTON", action: "shared-start-rename-conversation" } }, sink);
        await wait("Rename starts with keyboard focus on its editor", () => cdp.evaluate("document.activeElement?.id"), value => value === "shared-rename-title");
        const editorReference = await cdp.call("Runtime.evaluate", { expression: "document.getElementById('shared-rename-title')", returnByValue: false });
        const editorObjectId = editorReference.result?.objectId;
        if (!editorObjectId) throw failure("Rename editor is unavailable after the public start action", editorReference);
        const renamedTitle = "名前変更の入力保持を確認";
        try {
          const editor = async () => {
            const result = await cdp.call("Runtime.callFunctionOn", { objectId: editorObjectId, returnByValue: true,
              functionDeclaration: "function () { return { connected: this.isConnected, same_node: document.getElementById('shared-rename-title') === this, focused: document.activeElement === this, value: this.value, selection_start: this.selectionStart, selection_end: this.selectionEnd, selection_direction: this.selectionDirection }; }" });
            return result.result?.value;
          };
          async function retainThroughPolling(label, expected) {
            const afterSequence = (await state.commands.snapshot()).sequence;
            // Refresh is serial: seeing the third call establishes that two
            // preceding refreshes have returned and rendered the actual UI.
            return wait(label, async () => {
              const commands = await state.commands.snapshot(afterSequence), observed = await editor();
              if (commands.dropped_through > afterSequence || !sharedRenameEditorRetained(observed, expected))
                throw failure("Shared polling changed the active rename editor or its selection", { expected, observed, commands });
              return { editor: observed, refresh_calls: commands.calls.filter(call => call.args?.request?.kind === "refresh") };
            }, value => value.refresh_calls.length >= 3, 30_000);
          }
          await input.keyDown("Control"); try { await input.pressKey("a"); } finally { await input.keyUp("Control"); }
          await input.insertText(byId("shared-rename-title", "INPUT"), renamedTitle);
          await input.keyDown("Control"); try { await input.pressKey("a"); } finally { await input.keyUp("Control"); }
          const keyboardSelection = await editor();
          if (!sharedRenameEditorRetained(keyboardSelection, { ...keyboardSelection, value: renamedTitle, selection_start: 0, selection_end: renamedTitle.length }))
            throw failure("Keyboard editing did not retain the selected rename draft", keyboardSelection);
          const keyboardProof = await retainThroughPolling("Shared polling keeps the selected rename draft focused", keyboardSelection);
          await input.click(byId("shared-rename-title", "INPUT"), { stableHitSamples: 3 });
          const pointerSelection = await editor();
          if (!(pointerSelection?.connected && pointerSelection.same_node && pointerSelection.focused && pointerSelection.value === renamedTitle
            && Number.isInteger(pointerSelection.selection_start) && pointerSelection.selection_start >= 0 && pointerSelection.selection_end <= renamedTitle.length))
            throw failure("Pointer editing lost the rename draft", pointerSelection);
          if (pointerSelection.selection_start !== pointerSelection.selection_end)
            throw failure("Clicking the rename editor did not place the text cursor", pointerSelection);
          const pointerProof = await retainThroughPolling("Shared polling keeps the clicked text cursor focused", pointerSelection);
          await sink.record("shared-rename-editor-retention", { conversation_id: conversationId, keyboard: keyboardProof, pointer: pointerProof }, { phase: "executing", owner: OWNER });
          await captureScenarioScreenshot({ cdp, sink, name: "shared-rename-input-retained", owner: OWNER });
          const beforeRename = (await state.commands.snapshot()).sequence;
          const renameGeneration = (await invokeDesktopCommand(cdp, "shared_work_projection")).generation;
          await click("save-rename-conversation");
          await wait("Saving the shared name updates the conversation and visible heading", async () => ({ projection: await invokeDesktopCommand(cdp, "shared_work_projection"),
            heading: await cdp.evaluate("document.getElementById('shared-heading')?.textContent"), editor: await cdp.evaluate("document.getElementById('shared-rename-title') !== null") }),
          value => value.projection.conversations?.some(row => row.id === conversationId && row.title === renamedTitle) && value.heading === renamedTitle && !value.editor);
          const renameCommands = await state.commands.snapshot(beforeRename);
          const renameProof = assertExactDesktopCommandSequence({ ...renameCommands, calls: renameCommands.calls.filter(call => call.args?.request?.kind !== "refresh") }, {
            afterSequence: beforeRename, expected: [{ command: "shared_work_command", args: { expectedGeneration: renameGeneration,
              request: { kind: "rename_conversation", project_id: "project-a", conversation_id: conversationId, title: renamedTitle } } }],
          });
          await sink.record("shared-rename-saved", { conversation_id: conversationId, title: renamedTitle, command: renameProof }, { phase: "executing", owner: OWNER });
        } finally { await cdp.call("Runtime.releaseObject", { objectId: editorObjectId }); }
        await captureScenarioScreenshot({ cdp, sink, name: "shared-submitted-job", owner: OWNER });
        const beforeRestart = { user_id: alice.user_id, project_id: "project-a", job_id: job.id };
        const restartProof = await restart();
        const restored = await wait("Same-profile restart restores Alice and her project without a login command", async () => ({
          desktop: await invokeDesktopCommand(cdp, "desktop_state"), shared: await invokeDesktopCommand(cdp, "shared_work_projection"),
          surface: await observeSharedWorkSurface(cdp), calls: (await state.commands.snapshot()).calls,
        }), value => rememberedRestartAccepted(value, beforeRestart));
        await sink.record("shared-remembered-person-restart", { ...beforeRestart, restart: restartProof, connected: restored.shared.connected, surface: restored.surface, login_commands: 0 }, { phase: "executing", owner: OWNER });
        await captureScenarioScreenshot({ cdp, sink, name: "shared-person-restored-after-restart", owner: OWNER });
        await openSharedJob(input, cdp, sink, job.id);
        await click("cancel", job.id);
        await wait("Cancel changes the shared job", () => invokeDesktopCommand(cdp, "shared_work_projection"), p => p.status?.jobs.some(j => j.id === job.id && j.state === "cancelled"));
        await bindHubDevice(resource, enrolled.network.device_id, bob.user_id);
        await wait("Administrator association removes Alice's protected view", () => invokeDesktopCommand(cdp, "shared_work_projection"), p => p.principal?.user_id === bob.user_id && !p.projects.some(project => project.id === "project-a"));
        await restart();
        const bobView = await wait("Another human sees their own shared job", () => invokeDesktopCommand(cdp, "shared_work_projection"), p => p.principal?.user_id === bob.user_id && p.projects.length === 1 && p.status?.jobs.some(j => j.id === occupied.id));
        if ((await cdp.evaluate("document.querySelector('.shared-work').textContent")).includes(prompt)) throw failure("Changed device association retained the former actor's work", {});
        await captureScenarioScreenshot({ cdp, sink, name: "shared-bob-reconnected", owner: OWNER });
        const approvalId = randomUUID();
        await participant.sharedCall("report", { event_id: randomUUID(), attempt_id: assignment.attempt_id, generation: assignment.generation, outcome: { kind: "started" } });
        await openSharedJob(input, cdp, sink, occupied.id);
        const contacts = async () => {
          const p = await invokeDesktopCommand(cdp, "shared_work_projection");
          const job = p.status?.jobs.find(row => row.id === occupied.id);
          const environment = p.status?.environments.find(row => row.id === "env-project-b");
          return { job, environment, detail: p.detail, observed_at_ms: p.observed_at_ms,
            // The current conversation shows PC contact in its right pane;
            // the former separate job-detail region no longer exists.
            text: { environments: await cdp.evaluate("document.querySelector('[data-shared-region=\"environments\"]')?.textContent ?? ''") } };
        };
        const contactState = (value, state) => value.job?.state === "running" && value.detail?.id === occupied.id
          && value.detail.state === "running" && value.environment?.occupied === 1
          && [value.job, value.environment, value.detail].every(row => row.runner_contact?.state === state)
          && Object.values(value.text).every(text => text.includes(state === "recent" ? "PCからの応答あり" : "PCの応答なし（状態不明）"));
        async function confirmRunnerContact() {
          const requestedAt = Date.now();
          await participant.sharedAssignments(["env-project-b"]);
          return wait("This Runner control response reaches environment list and detail", contacts, value => contactState(value, "recent")
            && [value.job, value.environment, value.detail].every(row => row.runner_contact.last_contact_ms >= requestedAt));
        }
        const recent = await confirmRunnerContact();
        const lastContact = recent.detail.runner_contact.last_contact_ms;
        if (!Number.isFinite(lastContact)) throw failure("Runner contact has no Hub observation time", {});
        const stale = await wait("Fifteen seconds without Runner control contact preserves the occupied running work", contacts,
          value => contactState(value, "stale") && [value.job, value.environment, value.detail].every(row => row.runner_contact.last_contact_ms === lastContact), 30_000);
        if (stale.observed_at_ms <= recent.observed_at_ms || Object.values(stale.text).some(text => text.includes("空きがあります"))) throw failure("Desktop reads must not refresh Runner contact or declare the occupied environment free", {});
        await openSharedJob(input, cdp, sink, occupied.id);
        await captureScenarioScreenshot({ cdp, sink, name: "shared-runner-contact-stale", owner: OWNER });
        const recovered = await confirmRunnerContact();
        if ([recovered.job, recovered.environment, recovered.detail].some(row => row.runner_contact.last_contact_ms <= lastContact)) throw failure("The new Runner control response did not advance the contact time", {});
        await captureScenarioScreenshot({ cdp, sink, name: "shared-runner-contact-recovered", owner: OWNER });
        await sink.record("shared-runner-contact-recovery", { job_id: occupied.id, attempt_id: assignment.attempt_id, generation: assignment.generation,
          last_contact_ms: lastContact, stale_contact_ms: stale.detail.runner_contact.last_contact_ms, recovered_contact_ms: recovered.detail.runner_contact.last_contact_ms,
          desktop_observation_advanced: stale.observed_at_ms > recent.observed_at_ms, occupancy_before: recent.environment.occupied, occupancy_stale: stale.environment.occupied, occupancy_after: recovered.environment.occupied,
          scope: "Actual Desktop and real mTLS Hub assignments API; fixture controls its own Runner-role contact, with the default 15-second Hub threshold and no clock override. Process liveness and progress are not inferred." }, { phase: "executing", owner: OWNER });
        // Same person on a different PC does not inherit approval authority.
        // Finish the fixture's own job and submit the reviewed work from this
        // actual Desktop, so the GUI is the captured originating controller.
        await participant.sharedCall("report", { event_id: randomUUID(), attempt_id: assignment.attempt_id, generation: assignment.generation,
          outcome: { kind: "finished", success: true, result: { text: "Contact fixture complete" }, resources_released: true } });
        await click("new-conversation");
        await fill("shared-prompt", "このPCから依頼する承認試験", "TEXTAREA");
        await input.keyDown("Control"); try { await input.pressKey("Enter"); } finally { await input.keyUp("Control"); }
        const reviewJob = await wait("Desktop-origin approval work is queued", () => invokeDesktopCommand(cdp, "shared_work_projection"),
          p => p.detail?.title === "このPCから依頼する承認試験" && p.detail.state === "queued");
        const reviewAssignment = await participant.sharedCall("claim", { environment_ids: ["env-project-b"] });
        if (reviewAssignment?.job?.id !== reviewJob.detail.id) throw failure("Runner claimed a different approval job", {});
        await participant.sharedCall("report", { event_id: randomUUID(), attempt_id: reviewAssignment.attempt_id, generation: reviewAssignment.generation, outcome: { kind: "started" } });
        // This fixture uses the existing Hub permission lifetime contract; no product clock or timeout override.
        const approvalExpiresAt = Date.now() + 60_000;
        const approvalRequest = { access: "shell", summary: "解析環境で入力ファイルの確認を実行", details: ["Command: Get-ChildItem -LiteralPath .", `Workdir: ${context.paths.workspace}`], targets: [context.paths.workspace], outside_workspace: false, risks: ["unclassified_shell"] };
        await participant.sharedCall("report", { event_id: randomUUID(), attempt_id: reviewAssignment.attempt_id, generation: reviewAssignment.generation,
          outcome: { kind: "approval_requested", approval_id: approvalId, expires_at_ms: approvalExpiresAt, request: approvalRequest } });
        await openSharedJob(input, cdp, sink, reviewJob.detail.id);
        await wait("Shared detail receives the pending approval", () => invokeDesktopCommand(cdp, "shared_work_projection"), p => p.approval?.id === approvalId && p.approval.status === "pending" && p.approval.can_decide);
        if (!(await cdp.evaluate("document.querySelector('[data-shared-region=approval]').textContent")).includes("Get-ChildItem -LiteralPath .")) throw failure("Approval omitted concrete operation details", {});
        const pendingReview = await invokeDesktopCommand(cdp, "shared_work_projection");
        if (pendingReview.approval.context.controller_device_id !== enrolled.network.device_id
          || pendingReview.approval.context.execution_device_id !== approved.deviceId) throw failure("Approval confused origin and execution PCs", {});
        await captureScenarioScreenshot({ cdp, sink, name: "shared-approval-pending-target", owner: OWNER });
        await wait("Origin Desktop delivers a Windows approval balloon", async () => {
          try { return await readFile(state.notificationLog, "utf8"); } catch (error) { if (error.code === "ENOENT") return ""; throw error; }
        }, text => text.split(/\r?\n/).some(line => line.includes("このPCから依頼する承認試験") && line.includes("操作の承認を待っています") && line.includes("native balloon result=true")), 15000);
        const parkedTarget = { jobId: reviewJob.detail.id, approvalId, attemptId: reviewAssignment.attempt_id, environmentId: "env-project-b" };
        const expired = await wait("The real Hub clock expires only the permission while the same work stays occupied", () => invokeDesktopCommand(cdp, "shared_work_projection"),
          p => sharedApprovalParked(p, parkedTarget), 90_000);
        const consume = id => participant.sharedConsumeApproval(id, reviewAssignment.attempt_id, reviewAssignment.generation);
        for (let poll = 0; poll < 2; poll++) {
          if (await consume(approvalId) !== null) throw failure("Expired approval delivered a decision instead of parking", {});
        }
        await captureScenarioScreenshot({ cdp, sink, name: "shared-approval-expired-parked", owner: OWNER });
        const beforeReconfirm = (await state.commands.snapshot()).sequence;
        await click("reconfirm-approval", approvalId);
        const reconfirmation = await wait("The exact expired request requires a fresh review only after the origin click", () => consume(approvalId),
          value => value?.approval_id === approvalId && value.reconfirmation_required === true && value.decision === undefined);
        const reconfirmCommand = assertExactDesktopCommandSequence(await state.commands.snapshot(beforeReconfirm), {
          afterSequence: beforeReconfirm, expected: [{ command: "shared_work_command", args: { expectedGeneration: expired.generation,
            request: { kind: "reconfirm_approval", project_id: "project-b", job_id: reviewJob.detail.id, approval_id: approvalId } } }],
        });
        const renewedApprovalId = randomUUID();
        await participant.sharedCall("report", { event_id: randomUUID(), attempt_id: reviewAssignment.attempt_id, generation: reviewAssignment.generation,
          outcome: { kind: "approval_requested", approval_id: renewedApprovalId, expires_at_ms: Date.now() + 300_000, request: approvalRequest } });
        const renewed = await wait("The same paused operation is shown with a new approval identity", () => invokeDesktopCommand(cdp, "shared_work_projection"),
          p => p.detail?.id === reviewJob.detail.id && p.detail.state === "running" && p.approval?.id === renewedApprovalId
            && p.approval.attempt_id === reviewAssignment.attempt_id && p.approval.status === "pending" && p.approval.can_decide);
        if (JSON.stringify(renewed.approval.request) !== JSON.stringify(pendingReview.approval.request)
          || renewed.approval.context.controller_device_id !== enrolled.network.device_id
          || renewed.approval.context.execution_device_id !== approved.deviceId) throw failure("Reconfirmation changed the operation or its origin/execution device", {});
        if ((await consume(approvalId))?.reconfirmation_required !== true) throw failure("An expired approval became usable after reissue", {});
        await captureScenarioScreenshot({ cdp, sink, name: "shared-approval-reconfirmed-target", owner: OWNER });
        await click("approve", renewedApprovalId);
        await wait("Explicit GUI approval is recorded for the replacement request", () => invokeDesktopCommand(cdp, "shared_work_projection"),
          p => p.approval?.id === renewedApprovalId && p.approval.status === "decided" && p.approval.decision === "approve" && !p.approval.can_decide);
        const answer = await consume(renewedApprovalId);
        if (answer?.approval_id !== renewedApprovalId || answer.decision !== "approve" || answer.reconfirmation_required !== undefined) throw failure("The Runner did not receive the exact replacement approval", {});
        await captureScenarioScreenshot({ cdp, sink, name: "shared-approval-decided", owner: OWNER });
        await sink.record("shared-approval-expiry-reconfirmation", { job_id: reviewJob.detail.id, attempt_id: reviewAssignment.attempt_id, generation: reviewAssignment.generation,
          original_approval_id: approvalId, replacement_approval_id: renewedApprovalId, expires_at_ms: approvalExpiresAt, expired_observed_at_ms: expired.observed_at_ms,
          occupancy_while_expired: 1, expired_consume_results: [null, null], reconfirmation, reconfirm_command: reconfirmCommand, replacement_answer: answer,
          scope: "Actual Tauri input and real Hub clock/API with a 60-second fixture-reported permission. The fixture emulates the Runner's consume/reissue exchange; no real Runner 15-minute soak or approved shell effect." }, { phase: "executing", owner: OWNER });
        await participant.sharedCall("report", { event_id: randomUUID(), attempt_id: reviewAssignment.attempt_id, generation: reviewAssignment.generation,
          outcome: { kind: "finished", success: true, result: { text: "Approval protocol fixture complete; no shell effect executed" }, resources_released: true } });
        await wait("The fixture's completed work releases its capacity", () => invokeDesktopCommand(cdp, "shared_work_projection"),
          p => p.detail?.id === reviewJob.detail.id && p.detail.state === "succeeded" && p.status?.environments.some(row => row.id === "env-project-b" && row.occupied === 0));
        const beforeSettings = await invokeDesktopCommand(cdp, "desktop_state");
        await trustedClick(input, cdp, action("show-hub", "aside.sidebar"), sink);
        await trustedClick(input, cdp, hubSettingsCloseTarget, sink);
        await wait("Closing connection settings keeps the project surface and current setup readiness", () => invokeDesktopCommand(cdp, "desktop_state"), value => sharedSettingsClosed(value, beforeSettings));
        if (resource.provider.requests.some(r => r.method !== "GET")) throw failure("Projectless tracking attempted model generation", {});
        await sink.record("shared-work-complete", { initial_entry_setup_required: entry.startup.initial_setup_required, setup_status_before_settings: beforeSettings.startup.status, setup_required_before_settings: beforeSettings.startup.initial_setup_required, device_id: enrolled.network.device_id, alice_project_count: aliceView.projects.length, bob_project_count: bobView.projects.length, submitted_job: job.id, cancelled: true, previous_actor_cleared: true, expired_approval_id: approvalId, approval_id: renewedApprovalId, approval_decision: "approve", credentials_in_webview_projection: false, scope: "Real Tauri input, native public trust import, Hub browser device approval, automatic device session, project membership, occupancy redaction, submit/detail/cancel, and explicit administrator reassociation. Real-clock expiry parks the fixture-reported permission; the originating Desktop explicitly requests a fresh review and approves its replacement identity. No solver/model execution, real Runner 15-minute soak, or approved shell effect." }, { phase: "executing", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "pending" };
      } catch (error) {
        await resource.screenshot("shared-hub-failure");
        await sink.record("shared-hub-failure-observation", {
          notice: await page.locator("#shared-admin-notice").textContent(),
          form_error: await page.locator("#shared-admin-form-error").count() ? await page.locator("#shared-admin-form-error").textContent() : null,
        }, { phase: "executing", owner: OWNER });
        await sink.record("shared-failure-observation", { view: await invokeDesktopCommand(cdp, "shared_work_projection"), fields: await cdp.evaluate("({credential_fields:document.querySelectorAll('#shared-username, #shared-password').length,active:document.activeElement?.id})"), commands: await state.commands.snapshot() }, { phase: "executing", owner: OWNER });
        await captureScenarioScreenshot({ cdp, sink, name: "shared-failure", owner: OWNER });
        throw error;
      } finally { await settle(); }
    },
    async quiesce() { await settle(); state.close ??= state.resource ? await state.resource.close() : { pass: true }; return { input: state.close.pass && !state.failures.length ? "pass" : "fail", resources: [{ kind: ID, close: state.close, failures: state.failures }] }; },
    async cleanup() { return { input: state.close?.pass && !state.failures.length ? "pass" : "fail", resources: [] }; },
  });
}

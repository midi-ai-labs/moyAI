import { randomUUID } from "node:crypto";
import { pathToFileURL } from "node:url";
import path from "node:path";
import { DesktopE2eError } from "../core/execution.mjs";
import { normalizeHubBrowserOptions, startHubBrowserResource } from "../drivers/hub_browser_resource.mjs";
import { WebviewInput } from "../drivers/webview_input.mjs";
import { DesktopCommandProbe } from "../drivers/desktop_command_probe.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";
import { action, byId, hubSettingsCloseTarget, trustedClick, trustedFocus, wait, enrollDesktopFromHubBrowser, requestHubEnrollmentExit } from "./hub_browser_enrollment.mjs";
import { hubProjectReady, openHubProjectSurface, openSharedDisclosure, rememberedRestartAccepted, sharedActionTarget } from "./shared_work_navigation.mjs";

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
export function createSharedWorkEntryScenario(options = {}) {
  const settings = normalizeHubBrowserOptions(options);
  const state = { resource: null, input: null, commands: null, nativeOwner: null, nativeCandidate: null, nativeBefore: null, importDispatched: false, failures: [], close: null };
  async function settle() {
    if (state.input) { try { await state.input.cleanup(); } catch { state.failures.push("input-cleanup"); } state.input = null; }
    if (state.commands) { try { await state.commands.remove(); } catch { state.failures.push("command-cleanup"); } state.commands = null; }
  }
  return Object.freeze({ id: ID, productOracle: "pass", manualGate: "pending", databaseRequired: true,
    async prepare(args) {
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
        cdp = result.driver; await attach(); await openHubProjectSurface(input, cdp, sink);
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
        const approved = await participant.collectApproval(); await participant.presence();
        await participant.sharedLogin(resource.administrator.username, resource.administrator.password);
        const password = randomUUID();
        const alice = await participant.sharedCall("createUser", { username: "shared-alice", display_name: "利用者 Alice", password, administrator: false });
        const bob = await participant.sharedCall("createUser", { username: "shared-bob", display_name: "利用者 Bob", password, administrator: false });
        for (const [id, label, user] of [["project-a", "解析プロジェクト A", alice], ["project-b", "解析プロジェクト B", bob]]) {
          await participant.sharedCall("createProject", { id, label });
          await participant.sharedCall("membership", { project_id: id, user_id: user.user_id, role: "contributor" });
          await participant.sharedCall("environment", { id: `env-${id}`, label: `解析環境 ${id.slice(-1).toUpperCase()}`, resource_id: "shared-solver", runner_id: approved.deviceId, capacity: 1, project_ids: [id] });
        }
        await participant.sharedLogin("shared-bob", password);
        const occupied = await participant.sharedCall("submit", { request_id: randomUUID(), project_id: "project-b", environment_id: "env-project-b", title: "Bobだけが見える仕事", input: { version: 1, prompt: "Fixture retained shared work" }, descendant_budget: 0 });
        const assignment = await participant.sharedCall("claim", { environment_ids: ["env-project-b"] });
        await page.locator('nav a[href="#device-network"]').click();
        const enrolled = await enrollDesktopFromHubBrowser({ resource, context, runtime, cdp, input, sink, nativeState: state, entry: "shared-work" });
        async function fill(id, value, tag = "INPUT") {
          const target = byId(id, tag); await trustedClick(input, cdp, target, sink); await input.insertText(target, value);
        }
        async function click(name, value) {
          if (name === "logout") {
            await openSharedDisclosure(input, cdp, sink, "hub-project-account");
            const afterSequence = (await state.commands.snapshot()).sequence;
            // Refreshes are serialized: the second dispatch proves the preceding poll rendered.
            const polls = await wait("Account remains open across a completed automatic poll", () => state.commands.snapshot(afterSequence),
              snapshot => snapshot.calls.filter(call => call.args?.request?.kind === "refresh").length >= 2);
            const account = await cdp.evaluate(`(() => {
              const detail = document.querySelector('.shared-work details[data-details-key="hub-project-account"]');
              const logout = detail?.querySelector('[data-action="shared-logout"]');
              return { open: detail?.open === true, logout_visible: Boolean(logout?.getClientRects().length), logout_enabled: logout?.disabled === false };
            })()`);
            if (!account.open || !account.logout_visible || !account.logout_enabled) throw failure("Polling closed the account or removed its Logout action", account);
            await sink.record("shared-account-preserved-after-poll", { ...account, observed_refreshes: polls.calls.filter(call => call.args?.request?.kind === "refresh").length }, { phase: "executing", owner: OWNER });
          }
          await trustedClick(input, cdp, sharedActionTarget(name, value), sink);
        }
        await fill("shared-username", "shared-alice"); await fill("shared-password", password);
        await sink.record("shared-login-before", await cdp.evaluate("({ username:document.querySelector('#shared-username')?.value, password_length:document.querySelector('#shared-password')?.value.length, disabled:document.querySelector('[data-action=shared-login]')?.disabled, active:document.activeElement?.id })"), { phase: "executing", owner: OWNER });
        await click("login");
        const aliceView = await wait("Alice sees only her project and redacted occupied resource", () => invokeDesktopCommand(cdp, "shared_work_projection"), p => p.principal?.user_id === alice.user_id && p.projects.length === 1 && p.status?.environments[0]?.other_occupants === 1);
        const aliceText = await cdp.evaluate("document.querySelector('.shared-work').textContent");
        if (aliceText.includes("Bobだけが見える仕事") || aliceText.includes("利用者 Bob")) throw failure("Unauthorized occupancy details leaked", {});
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
        await click("detail", job.id);
        await wait("Job detail displays the submitted prompt", () => invokeDesktopCommand(cdp, "shared_work_projection"), p => p.detail?.id === job.id && p.detail.input?.prompt === "共有環境で解析してください。");
        await captureScenarioScreenshot({ cdp, sink, name: "shared-submitted-job", owner: OWNER });
        const beforeRestart = { user_id: alice.user_id, project_id: "project-a", job_id: job.id };
        const restartProof = await restart();
        const restored = await wait("Same-profile restart restores Alice and her project without a login command", async () => ({
          desktop: await invokeDesktopCommand(cdp, "desktop_state"), shared: await invokeDesktopCommand(cdp, "shared_work_projection"),
          login_visible: await cdp.evaluate("Boolean(document.querySelector('#shared-username')?.getClientRects().length)"), calls: (await state.commands.snapshot()).calls,
        }), value => rememberedRestartAccepted(value, beforeRestart));
        await sink.record("shared-remembered-person-restart", { ...beforeRestart, restart: restartProof, connected: restored.shared.connected, login_visible: restored.login_visible, login_commands: 0 }, { phase: "executing", owner: OWNER });
        await captureScenarioScreenshot({ cdp, sink, name: "shared-person-restored-after-restart", owner: OWNER });
        await click("detail", job.id);
        await click("cancel", job.id);
        await wait("Cancel changes the shared job", () => invokeDesktopCommand(cdp, "shared_work_projection"), p => p.status?.jobs.some(j => j.id === job.id && j.state === "cancelled"));
        await click("logout");
        await wait("Logout clears every protected projection", () => invokeDesktopCommand(cdp, "shared_work_projection"), p => p.principal === null && p.projects.length === 0 && p.status === null && p.detail === null);
        const loggedOutText = await cdp.evaluate("document.querySelector('.shared-work').textContent");
        if (loggedOutText.includes(prompt) || loggedOutText.includes("解析プロジェクト A")) throw failure("Logout retained old user's display", {});
        const logoutRestart = await restart();
        await wait("Explicit logout survives restart", () => invokeDesktopCommand(cdp, "shared_work_projection"), p => p.connected && p.principal === null && p.projects.length === 0 && p.status === null && p.detail === null);
        await sink.record("shared-logout-survives-restart", { restart: logoutRestart, principal: null, protected_projects: 0 }, { phase: "executing", owner: OWNER });
        const username = byId("shared-username", "INPUT"); await trustedClick(input, cdp, username, sink); await input.keyDown("Control"); await input.pressKey("a"); await input.keyUp("Control"); await input.insertText(username, "shared-bob");
        await fill("shared-password", password); await click("login");
        const bobView = await wait("Another human sees their own shared job", () => invokeDesktopCommand(cdp, "shared_work_projection"), p => p.principal?.user_id === bob.user_id && p.projects.length === 1 && p.status?.jobs.some(j => j.id === occupied.id));
        if ((await cdp.evaluate("document.querySelector('.shared-work').textContent")).includes(prompt)) throw failure("Second login retained first user's work", {});
        await captureScenarioScreenshot({ cdp, sink, name: "shared-bob-reconnected", owner: OWNER });
        const approvalId = randomUUID();
        await participant.sharedCall("report", { event_id: randomUUID(), attempt_id: assignment.attempt_id, generation: assignment.generation, outcome: { kind: "started" } });
        await click("detail", occupied.id);
        const contacts = async () => {
          const p = await invokeDesktopCommand(cdp, "shared_work_projection");
          const job = p.status?.jobs.find(row => row.id === occupied.id);
          const environment = p.status?.environments.find(row => row.id === "env-project-b");
          return { job, environment, detail: p.detail, observed_at_ms: p.observed_at_ms,
            text: await cdp.evaluate(`Object.fromEntries(["environments", "detail"].map(name => [name, document.querySelector('[data-shared-region="' + name + '"]')?.textContent ?? ""]))`) };
        };
        const contactState = (value, state) => value.job?.state === "running" && value.detail?.id === occupied.id
          && value.detail.state === "running" && value.environment?.occupied === 1
          && [value.job, value.environment, value.detail].every(row => row.runner_contact?.state === state)
          && Object.values(value.text).every(text => text.includes(state === "recent" ? "最近の応答あり" : "PCの応答なし（状態不明）"));
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
        const selectedJob = sharedActionTarget("detail", occupied.id);
        await trustedFocus(input, cdp, selectedJob);
        await captureScenarioScreenshot({ cdp, sink, name: "shared-runner-contact-stale", owner: OWNER });
        const recovered = await confirmRunnerContact();
        if ([recovered.job, recovered.environment, recovered.detail].some(row => row.runner_contact.last_contact_ms <= lastContact)) throw failure("The new Runner control response did not advance the contact time", {});
        await captureScenarioScreenshot({ cdp, sink, name: "shared-runner-contact-recovered", owner: OWNER });
        await sink.record("shared-runner-contact-recovery", { job_id: occupied.id, attempt_id: assignment.attempt_id, generation: assignment.generation,
          last_contact_ms: lastContact, stale_contact_ms: stale.detail.runner_contact.last_contact_ms, recovered_contact_ms: recovered.detail.runner_contact.last_contact_ms,
          desktop_observation_advanced: stale.observed_at_ms > recent.observed_at_ms, occupancy_before: recent.environment.occupied, occupancy_stale: stale.environment.occupied, occupancy_after: recovered.environment.occupied,
          scope: "Actual Desktop and real mTLS Hub assignments API; fixture controls its own Runner-role contact, with the default 15-second Hub threshold and no clock override. Process liveness and progress are not inferred." }, { phase: "executing", owner: OWNER });
        await participant.sharedCall("report", { event_id: randomUUID(), attempt_id: assignment.attempt_id, generation: assignment.generation, outcome: { kind: "approval_requested", approval_id: approvalId, expires_at_ms: Date.now() + 300000, request: { access: "shell", summary: "解析環境で入力ファイルの確認を実行", details: ["実行予定: Get-ChildItem -LiteralPath ."], targets: [context.paths.workspace], outside_workspace: false, risks: ["unclassified_shell"] } } });
        await click("detail", occupied.id);
        await wait("Shared detail receives the pending approval", () => invokeDesktopCommand(cdp, "shared_work_projection"), p => p.approval?.id === approvalId && p.approval.status === "pending" && p.approval.can_decide);
        if (!(await cdp.evaluate("document.querySelector('[data-shared-region=approval]').textContent")).includes("Get-ChildItem -LiteralPath .")) throw failure("Approval omitted concrete operation details", {});
        await click("approve", approvalId);
        await wait("Explicit GUI approval is recorded for this exact request", () => invokeDesktopCommand(cdp, "shared_work_projection"), p => p.approval?.id === approvalId && p.approval.status === "decided" && p.approval.decision === "approve" && !p.approval.can_decide);
        await captureScenarioScreenshot({ cdp, sink, name: "shared-approval-decided", owner: OWNER });
        await click("logout");
        const beforeSettings = await invokeDesktopCommand(cdp, "desktop_state");
        await trustedClick(input, cdp, action("show-hub", "aside.sidebar"), sink);
        await trustedClick(input, cdp, hubSettingsCloseTarget, sink);
        await wait("Closing connection settings keeps the project surface and current setup readiness", () => invokeDesktopCommand(cdp, "desktop_state"), value => sharedSettingsClosed(value, beforeSettings));
        if (resource.provider.requests.some(r => r.method !== "GET")) throw failure("Projectless tracking attempted model generation", {});
        await sink.record("shared-work-complete", { initial_entry_setup_required: entry.startup.initial_setup_required, setup_status_before_settings: beforeSettings.startup.status, setup_required_before_settings: beforeSettings.startup.initial_setup_required, device_id: enrolled.network.device_id, alice_project_count: aliceView.projects.length, bob_project_count: bobView.projects.length, submitted_job: job.id, cancelled: true, logout_cleared: true, approval_id: approvalId, approval_decision: "approve", credentials_in_webview_projection: false, scope: "Real Tauri input, native public trust import, Hub browser device approval, human login, project membership, occupancy redaction, submit/detail/cancel, logout and another user login. Explicit human approval decision for a fixture-reported request; no solver/model execution or approved shell effect." }, { phase: "executing", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "pending" };
      } catch (error) {
        await sink.record("shared-failure-observation", { view: await invokeDesktopCommand(cdp, "shared_work_projection"), fields: await cdp.evaluate("({username:document.querySelector('#shared-username')?.value,password_length:document.querySelector('#shared-password')?.value.length,disabled:document.querySelector('[data-action=shared-login]')?.disabled,active:document.activeElement?.id})"), commands: await state.commands.snapshot() }, { phase: "executing", owner: OWNER });
        await captureScenarioScreenshot({ cdp, sink, name: "shared-failure", owner: OWNER });
        throw error;
      } finally { await settle(); }
    },
    async quiesce() { await settle(); state.close ??= state.resource ? await state.resource.close() : { pass: true }; return { input: state.close.pass && !state.failures.length ? "pass" : "fail", resources: [{ kind: ID, close: state.close, failures: state.failures }] }; },
    async cleanup() { return { input: state.close?.pass && !state.failures.length ? "pass" : "fail", resources: [] }; },
  });
}

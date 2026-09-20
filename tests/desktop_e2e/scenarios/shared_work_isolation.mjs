import { randomUUID } from "node:crypto";
import { DesktopE2eError } from "../core/execution.mjs";
import { createCompanionContext } from "../core/run_context.mjs";
import { normalizeHubBrowserOptions, startHubBrowserResource } from "../drivers/hub_browser_resource.mjs";
import { WebviewInput } from "../drivers/webview_input.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";
import { byId, trustedClick, wait, enrollDesktopFromHubBrowser, requestHubEnrollmentExit } from "./hub_browser_enrollment.mjs";
import { hubProjectReady, openHubProjectSurface, openSharedDisclosure, sharedActionTarget, setSharedLoginMode } from "./shared_work_navigation.mjs";

const ID = "settings.shared-work-isolation", OWNER = `scenario:${ID}`;
const failure = (message, evidence = {}) => new DesktopE2eError("product", "shared-isolation-mismatch", message, evidence);
const digest = value => typeof value === "string" && /^[a-f0-9]{64}$/i.test(value);

export function sharedMainFillsShell({ shell, sidebar, main } = {}) {
  return [shell, sidebar, main].every(rect => Number.isFinite(rect?.left) && Number.isFinite(rect?.right)
    && rect.right > rect.left && Number.isFinite(rect.height) && rect.height > 0)
    && Math.abs(sidebar.left - shell.left) <= 1
    && Math.abs(main.left - sidebar.right) <= 1
    && Math.abs(main.right - shell.right) <= 1;
}

export function isolatedDevicesAccepted(a, b) {
  return [a, b].every(value => Number.isSafeInteger(value?.process_id) && value.process_id > 0
    && value.network?.enrollment === "active" && typeof value.network.device_id === "string" && value.network.device_id.length > 0
    && digest(value.key_sha256) && digest(value.certificate_sha256))
    && a.process_id !== b.process_id && a.network.device_id !== b.network.device_id
    && a.key_sha256.toLowerCase() !== b.key_sha256.toLowerCase()
    && a.certificate_sha256.toLowerCase() !== b.certificate_sha256.toLowerCase();
}

export function personalProjectAccepted(value, expected) {
  return hubProjectReady(value?.desktop) && value.shared?.connected === true
    && value.shared.principal?.user_id === expected.user_id && value.shared.principal.administrator === false
    && value.shared.projects?.length === 1 && value.shared.projects[0].id === expected.project_id
    && value.shared.selected_project_id === expected.project_id && Array.isArray(value.shared.status?.jobs)
    && value.visible_project_ids?.length === 1 && value.visible_project_ids[0] === expected.project_id
    && value.person_text?.includes(expected.display_name) === true && value.project_heading === expected.project_label
    && value.login_visible === false;
}

export function isolatedLogoutAccepted(value, expectedB, observedBeforeLogout) {
  const a = value?.a;
  return hubProjectReady(a?.desktop) && a.shared?.connected === true && a.shared.principal === null
    && a.shared.projects?.length === 0 && a.shared.status === null && a.shared.detail === null
    && a.visible_project_ids?.length === 0 && a.login_visible === true
    && personalProjectAccepted(value.b, expectedB)
    && Number.isFinite(value.b.shared.observed_at_ms) && value.b.shared.observed_at_ms > observedBeforeLogout;
}

export function createSharedWorkIsolationScenario(options = {}) {
  const settings = normalizeHubBrowserOptions(options);
  const a = { input: null }, b = { input: null };
  const state = { resource: null, close: null, failures: [] };
  async function settle(pc) {
    if (pc.input) {
      try { await pc.input.cleanup(); } catch { state.failures.push("input-cleanup"); }
      pc.input = null;
    }
  }
  const prepare = args => prepareDesktopFixture({ ...args, owner: OWNER, configMode: "absent", sentinelName: null, sentinelText: "" });
  const childScenario = {
    id: ID, databaseRequired: true, prepare,
    requestGracefulExit: cdp => requestHubEnrollmentExit(cdp, b),
    async quiesce() { await settle(b); return { input: state.failures.length ? "fail" : "pass", resources: [] }; },
    async cleanup() { return { input: state.failures.length ? "fail" : "pass", resources: [] }; },
  };
  return Object.freeze({ id: ID, productOracle: "pass", manualGate: "pending", databaseRequired: true,
    async prepare(args) {
      if (args.context.desktopIsolation !== "fixture") throw new DesktopE2eError("harness", "fixture-isolation-required", "Two simultaneous Desktops require --desktop-isolation fixture");
      await prepare(args);
      state.resource = await startHubBrowserResource({ ...args, options: settings });
    },
    requestGracefulExit: cdp => requestHubEnrollmentExit(cdp, a),
    async execute({ context, runtime, driver, host, sink }) {
      const resource = state.resource, { page, hub } = resource;
      async function attach(pc, connection, name) {
        Object.assign(pc, connection, { name });
        await pc.driver.call("Runtime.enable"); await pc.driver.call("DOM.enable");
        pc.input = new WebviewInput(pc.driver, { probeId: `${ID}-${name}` }); await pc.input.installProbe();
        await openHubProjectSurface(pc.input, pc.driver, pc.sink);
      }
      async function enroll(pc) {
        await page.locator('nav a[href="#device-network"]').click();
        const enrolled = await enrollDesktopFromHubBrowser({
          resource: { ...resource, screenshot: name => resource.screenshot(`${pc.name}-${name}`) },
          context: pc.context, runtime: pc.runtime, cdp: pc.driver, input: pc.input, sink: pc.sink, nativeState: pc, entry: "shared-work",
        });
        pc.identity = { process_id: pc.runtime.desktop_process_id, network: enrolled.network,
          key_sha256: enrolled.keySha256, certificate_sha256: enrolled.certificateSha256 };
        const shared = await invokeDesktopCommand(pc.driver, "shared_work_projection");
        if (!shared.connected || shared.principal !== null || shared.projects.length !== 0) throw failure("PC enrollment must not acquire a human session", { desktop: pc.name });
      }
      async function observe(pc) {
        const desktop = await invokeDesktopCommand(pc.driver, "desktop_state");
        const shared = await invokeDesktopCommand(pc.driver, "shared_work_projection");
        const visible = await pc.driver.evaluate(`({
          visible_project_ids: [...document.querySelectorAll('.sidebar button[data-action="open-hub-project"]')].filter(node => node.getClientRects().length).map(node => node.dataset.value),
          person_text: document.querySelector('.shared-header > div > p')?.textContent ?? '',
          project_heading: document.querySelector('#shared-heading')?.textContent ?? '',
          login_visible: Boolean(document.querySelector('.shared-work #shared-username')?.getClientRects().length)
        })`);
        return { desktop, shared, ...visible };
      }
      async function layout(pc) {
        return pc.driver.evaluate(`(() => {
          const shell = document.querySelector('.shell.hub-project-shell');
          const bounds = node => { if (!node) return null; const r = node.getBoundingClientRect(); return { left:r.left, right:r.right, height:r.height }; };
          return { shell:bounds(shell), sidebar:bounds(shell?.querySelector(':scope > .sidebar')), main:bounds(shell?.querySelector(':scope > .shared-work')) };
        })()`);
      }
      async function save() {
        await page.locator("#shared-admin-save").click();
        await page.locator("#shared-admin-form").waitFor({ state: "detached" });
      }
      async function createPerson(username, displayName) {
        const password = randomUUID();
        await page.locator('[data-sa-tab="users"]').click();
        await page.locator('[data-sa-operation="create_user"]').click();
        await page.locator("#shared-admin-username").fill(username);
        await page.locator("#shared-admin-display_name").fill(displayName);
        await page.locator("#shared-admin-password").fill(password); await save();
        const card = page.locator(".shared-admin-row").filter({ has: page.getByRole("heading", { name: displayName, exact: true }) });
        const userId = await card.locator('[data-sa-operation="update_user"]').getAttribute("data-sa-id");
        if (!userId) throw failure("Hub user creation did not expose the saved user identity");
        return { username, displayName, password, user_id: userId };
      }
      async function createProject(person, label) {
        await page.locator('[data-sa-tab="projects"]').click();
        await page.locator('[data-sa-operation="save_project"][data-sa-id=""]').click();
        await page.locator("#shared-admin-label").fill(label);
        await page.getByRole("combobox", { name: person.displayName, exact: true }).selectOption("contributor");
        // Both PCs can control both projects. Human membership is the only difference.
        for (const pc of [a, b]) await page.locator(`input[name="controller_device_ids"][value=${JSON.stringify(pc.identity.network.device_id)}]`).check();
        await save();
        const card = page.locator(".shared-admin-row").filter({ has: page.getByRole("heading", { name: label, exact: true }) });
        const projectId = await card.locator('[data-sa-operation="save_project"]').getAttribute("data-sa-id");
        if (!projectId) throw failure("Hub project creation did not expose the saved project identity");
        return { user_id: person.user_id, display_name: person.displayName, project_id: projectId, project_label: label };
      }
      async function login(pc, person, expected) {
        await setSharedLoginMode(pc.input, pc.driver, pc.sink, "password");
        for (const [id, value] of [["shared-username", person.username], ["shared-password", person.password]]) {
          const target = byId(id, "INPUT"); await trustedClick(pc.input, pc.driver, target, pc.sink); await pc.input.insertText(target, value);
        }
        await trustedClick(pc.input, pc.driver, sharedActionTarget("login"), pc.sink);
        await wait(`${pc.name} receives only its person's project`, () => invokeDesktopCommand(pc.driver, "shared_work_projection"), value =>
          value.principal?.user_id === expected.user_id && value.projects.length === 1 && value.projects[0].id === expected.project_id);
        await trustedClick(pc.input, pc.driver, { selector: `.sidebar button[data-action="open-hub-project"][data-value=${JSON.stringify(expected.project_id)}]`, identity: { tag: "BUTTON", action: "open-hub-project" } }, pc.sink);
        return wait(`${pc.name} renders the authorized project in the main conversation`, () => observe(pc), value => personalProjectAccepted(value, expected));
      }
      await attach(a, { context, runtime, driver, sink }, "desktop-a");
      try {
        await page.locator('nav a[href="#device-network"]').click();
        await page.locator("#network-ip").fill("127.0.0.1"); await page.locator("#network-port").fill(String(hub.networkPort));
        await page.locator("#network-start").click(); await page.locator("#network-stop").waitFor();
        await enroll(a);
        // The physical Windows name is initially identical. Rename the approved
        // PC through the normal Hub control before the second independent join.
        await page.locator("#network-clients-refresh").click();
        const row = page.locator(`[data-id="device:${a.identity.network.device_id}"]`);
        await row.locator("details[data-device-identity] > summary").click();
        await row.getByRole("button", { name: /を管理$/ }).click();
        await page.locator("#network-device-label").fill("隔離Desktop A"); await page.locator("#network-device-save").click();
        await page.locator("#network-device-dialog").waitFor({ state: "hidden" });
        await wait("Hub keeps the explicitly renamed PC", () => hub.observeNetwork(), value => value.devices.some(device => device.device_id === a.identity.network.device_id && device.label === "隔離Desktop A"));
        const companion = await host.openCompanion({ context: await createCompanionContext(context, "desktop-b"), scenario: childScenario, sink });
        await attach(b, companion, "desktop-b"); await enroll(b);
        // Read both still-live processes after B joins; no close/restart replaces A.
        a.identity.network = await invokeDesktopCommand(a.driver, "device_network_projection");
        b.identity.network = await invokeDesktopCommand(b.driver, "device_network_projection");
        if (!isolatedDevicesAccepted(a.identity, b.identity)) throw failure("The simultaneous Desktops reused a process, device identity or key", { a: a.identity, b: b.identity });
        await sink.record("shared-isolation-devices", { a: a.identity, b: b.identity }, { phase: "executing", owner: OWNER });
        await page.locator('nav a[href="#shared-administration"]').click();
        const alice = await createPerson("isolation-alice", "隔離試験 Alice"), bob = await createPerson("isolation-bob", "隔離試験 Bob");
        const expectedA = await createProject(alice, "Alice のプロジェクト"), expectedB = await createProject(bob, "Bob のプロジェクト");
        await resource.screenshot("shared-isolation-project-people-and-pcs");
        await login(a, alice, expectedA); await login(b, bob, expectedB);
        await wait("Both live Desktops retain independent people and project lists", async () => ({ a: await observe(a), b: await observe(b) }),
          value => personalProjectAccepted(value.a, expectedA) && personalProjectAccepted(value.b, expectedB));
        await captureScenarioScreenshot({ cdp: a.driver, sink: a.sink, name: "shared-isolation-alice", owner: OWNER });
        await captureScenarioScreenshot({ cdp: b.driver, sink: b.sink, name: "shared-isolation-bob", owner: OWNER });
        const geometry = { a: await layout(a), b: await layout(b) };
        await sink.record("shared-isolation-main-layout", geometry, { phase: "executing", owner: OWNER });
        if (!sharedMainFillsShell(geometry.a) || !sharedMainFillsShell(geometry.b)) throw failure("The Hub project leaves an unused column beside the main conversation", geometry);
        await openSharedDisclosure(a.input, a.driver, a.sink, "hub-project-account");
        await trustedClick(a.input, a.driver, sharedActionTarget("logout"), a.sink);
        const cleared = await wait("A completes logout while B retains its person", async () => ({ a: await observe(a), b: await observe(b) }),
          value => isolatedLogoutAccepted(value, expectedB, -1));
        const logout = await wait("Logging out A leaves B authenticated after a fresh automatic Hub observation", async () => ({ a: await observe(a), b: await observe(b) }),
          value => isolatedLogoutAccepted(value, expectedB, cleared.b.shared.observed_at_ms));
        await captureScenarioScreenshot({ cdp: a.driver, sink: a.sink, name: "shared-isolation-a-logged-out", owner: OWNER });
        await captureScenarioScreenshot({ cdp: b.driver, sink: b.sink, name: "shared-isolation-b-still-logged-in", owner: OWNER });
        if (resource.provider.requests.some(request => request.method !== "GET")) throw failure("Identity isolation requested an unrelated model generation");
        await sink.record("shared-isolation-complete", { a: expectedA, b: expectedB,
          desktop_process_ids: [a.identity.process_id, b.identity.process_id], a_logged_out: true, b_still_authenticated: true,
          b_observed_at_logout_ms: cleared.b.shared.observed_at_ms, b_observed_after_logout_ms: logout.b.shared.observed_at_ms,
          scope: "Two simultaneous isolated Tauri Desktops; native public configuration import, independent device keys and approvals, Hub GUI people/projects, trusted Desktop logins and logout. Both PCs can control both projects; person membership limits visibility. No solver work or private credential contents are inspected." }, { phase: "executing", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "pending" };
      } finally { await settle(a); await settle(b); }
    },
    async quiesce() {
      await settle(a); await settle(b);
      state.close ??= state.resource ? await state.resource.close() : { pass: true };
      return { input: state.close.pass && !state.failures.length ? "pass" : "fail", resources: [{ kind: ID, close: state.close, failures: state.failures }] };
    },
    async cleanup() { return { input: state.close?.pass && !state.failures.length ? "pass" : "fail", resources: [] }; },
  });
}

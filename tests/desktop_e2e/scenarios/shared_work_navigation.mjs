import { invokeDesktopCommand } from "./observations.mjs";
import { action, byId, trustedClick, wait } from "./hub_browser_enrollment.mjs";

export const hubProjectReady = value => value?.hub_project_open === true && value.overlay === "none" && !value.busy;

export function sharedActionTarget(kind, value = "") {
  const scope = kind === "detail" ? ".sidebar" : ".shared-work";
  return value ? { selector: `${scope} button[data-action="shared-${kind}"][data-value=${JSON.stringify(value)}]`, identity: { tag: "BUTTON", action: `shared-${kind}` } }
    : action(`shared-${kind}`, scope);
}

export async function openHubProjectSurface(input, cdp, sink) {
  const view = await invokeDesktopCommand(cdp, "desktop_state");
  if (hubProjectReady(view)) return view;
  if (view.overlay === "initial_setup") {
    await trustedClick(input, cdp, byId("initial-setup-shared-work"), sink);
  } else {
    // This is the ordinary Hub connection entry, also available before login.
    if (view.overlay !== "hub") await trustedClick(input, cdp, action("show-hub", "aside.sidebar"), sink);
    await trustedClick(input, cdp, byId("hub-tab-devices"), sink);
    await trustedClick(input, cdp, byId("device-network-open-shared"), sink);
  }
  return wait("Hub project is the main work surface", () => invokeDesktopCommand(cdp, "desktop_state"), hubProjectReady);
}

export async function openSharedDisclosure(input, cdp, sink, key) {
  const selector = `.shared-work details[data-details-key=${JSON.stringify(key)}]`;
  const isOpen = await cdp.evaluate(`document.querySelector(${JSON.stringify(selector)})?.open`);
  if (isOpen !== true) await trustedClick(input, cdp, { selector: `${selector} > summary`, identity: { tag: "DETAILS", detailsKey: key } }, sink);
}

/** Explicit administrator association through the real Hub browser form. */
export async function bindHubDevice(resource, deviceId, userId) {
  const page = resource.page;
  await page.locator('nav a[href="#shared-administration"]').click();
  const users = page.locator('[data-sa-tab="users"]');
  if (!await users.isVisible()) await page.getByText("既存の利用者・権限（詳細）", { exact: true }).click();
  await users.click();
  await page.locator(`[data-sa-operation="bind_device_principal"][data-sa-id=${JSON.stringify(deviceId)}]`).click();
  await page.locator("#shared-admin-user_id").selectOption(userId ?? "");
  await page.locator("#shared-admin-save").click();
  await page.locator("#shared-admin-form").waitFor({ state: "detached" });
}

export async function deviceActor(resource, deviceId) {
  const page = resource.page;
  await page.locator('nav a[href="#shared-administration"]').click();
  const users = page.locator('[data-sa-tab="users"]');
  if (!await users.isVisible()) await page.getByText("既存の利用者・権限（詳細）", { exact: true }).click();
  await users.click();
  await page.locator(`[data-sa-operation="bind_device_principal"][data-sa-id=${JSON.stringify(deviceId)}]`).click();
  const user_id = await page.locator("#shared-admin-user_id").inputValue();
  const display_name = await page.locator("#shared-admin-user_id option:checked").innerText();
  await page.locator("#shared-admin-close").click();
  if (!user_id) throw new Error("Approved device has no assigned actor");
  return { user_id, display_name };
}

export async function observeSharedWorkSurface(cdp) {
  return cdp.evaluate(`(() => {
    const visible = element => Boolean(element?.isConnected && element.getClientRects().length
      && getComputedStyle(element).display !== 'none' && getComputedStyle(element).visibility !== 'hidden'
      && !element.closest('[hidden],[inert],[aria-hidden="true"]'));
    const roots = [...document.querySelectorAll('main.shared-work')].filter(visible), root = roots[0];
    const login = root?.querySelector('#shared-username');
    return { count:roots.length, splash_visible:[...document.querySelectorAll('.splash-screen')].some(visible),
      heading:root?.querySelector('#shared-heading')?.textContent?.trim() ?? '',
      account_text:root?.querySelector('[data-shared-region="account"]')?.innerText ?? '',
      connection_visible:visible(root?.querySelector('[data-shared-region="connection"]')), login_visible:visible(login), login_enabled:Boolean(login && !login.disabled) };
  })()`);
}

export function sharedWorkSurfaceMatches(surface, shared) {
  if (surface?.count !== 1 || surface.splash_visible !== false) return false;
  if (!shared?.principal) return surface.connection_visible === true && surface.login_visible === false;
  const project = shared.projects?.find(row => row.id === shared.selected_project_id);
  return surface.login_visible === false && Boolean(shared.principal.display_name)
    && surface.account_text.includes(shared.principal.display_name)
    && Boolean(project) && surface.heading === project.label;
}

export function rememberedRestartAccepted({ desktop, shared, surface, calls }, expected) {
  return hubProjectReady(desktop) && shared?.connected === true
    && shared.principal?.user_id === expected.user_id
    && shared.selected_project_id === expected.project_id
    && shared.status?.jobs.some(job => job.id === expected.job_id)
    && sharedWorkSurfaceMatches(surface, shared) && Array.isArray(calls)
    && !calls.some(call => call.command === "shared_work_command" && call.args?.request?.kind === "login");
}

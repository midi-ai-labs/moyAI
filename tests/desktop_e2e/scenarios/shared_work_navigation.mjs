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

export function rememberedRestartAccepted({ desktop, shared, login_visible, calls }, expected) {
  return hubProjectReady(desktop) && shared?.connected === true
    && shared.principal?.user_id === expected.user_id
    && shared.selected_project_id === expected.project_id
    && shared.status?.jobs.some(job => job.id === expected.job_id)
    && login_visible === false && Array.isArray(calls)
    && !calls.some(call => call.command === "shared_work_command" && call.args?.request?.kind === "login");
}

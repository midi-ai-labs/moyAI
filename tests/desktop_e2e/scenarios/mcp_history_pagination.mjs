import { DesktopE2eError } from "../core/execution.mjs";
import { byId, action, wait, trustedClick } from "./hub_browser_enrollment.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";

const HISTORY = '[role="dialog"][data-modal="mcp_history"]';
const HUB = '[role="dialog"][data-modal="hub"]';
const fail = (message, evidence = {}) => new DesktopE2eError("product", "mcp-history-pagination-mismatch", message, evidence);
export function mcpPaginationPlan() {
  return Array.from({ length: 21 }, (_, index) => {
    const suffix = String(index + 1).padStart(2, "0");
    return { rootTaskId: `gui-pagination-root-${suffix}`, requestKey: `gui-pagination-request-${suffix}`,
      prompt: `MCP history pagination request ${suffix}`, responseText: `PAGINATION_UNUSED_${suffix}` };
  });
}
export function mcpPaginationProviderOptions() {
  return { responseBehavior: "hold_until_release", turns: mcpPaginationPlan().map(({ prompt, responseText }) => ({ prompt, responseText })) };
}
export function mcpHistoryPageMatches(value, ids, page) {
  const first = page === 1;
  return value?.dialogCount === 1 && value.page === `execution:${first ? 0 : 20}` && value.label === `${page}ページ`
    && value.detailOwner === "execution:" && value.error === "" && value.selected === 0
    && value.previous?.count === 1 && value.previous.disabled === first && value.previous.ariaDisabled === String(first)
    && value.next?.count === 1 && value.next.disabled === !first && value.next.ariaDisabled === String(!first)
    && value.export?.count === 1 && value.export.disabled === true && value.export.ariaDisabled === "true"
    && value.rows?.length === ids.length && new Set(value.rows.map(row => row.id)).size === ids.length
    && value.rows.every((row, index) => row.id === ids[index] && row.state === "interrupted" && row.pressed === "false");
}
export function mcpPaginationJobSetMatches(jobs) {
  const plan = mcpPaginationPlan();
  return jobs?.length === plan.length && new Set(jobs.map(job => job.job_id)).size === plan.length
    && jobs.every((job, index) => /^[0-9A-Z]{26}$/.test(job.job_id) && job.state === "interrupted"
      && job.request_key === plan[index].requestKey && job.root_task_id === plan[index].rootTaskId);
}
export function mcpPaginationOutcome(pageErrors) {
  if (pageErrors.length) throw fail("Hub browser reported page errors", { page_errors: pageErrors });
  return { acquisition: "pass", oracle: "pass", manual: "pending" };
}
async function observeHistory(cdp) {
  return cdp.evaluate(`(() => {
    const dialogs=document.querySelectorAll('${HISTORY}'), dialog=dialogs[0];
    const control=id=>{const nodes=dialog?.querySelectorAll('#'+id)??[], node=nodes[0];return {count:nodes.length,disabled:node?.disabled,ariaDisabled:node?.getAttribute('aria-disabled')};};
    return {dialogCount:dialogs.length,page:dialog?.dataset.historyPage,detailOwner:dialog?.dataset.historyDetailOwner,
      label:dialog?.querySelector('[data-history-region="page-label"]')?.textContent?.trim(),
      error:dialog?.querySelector('[data-history-region="list-error"]')?.textContent??'',
      document:dialog?.querySelector('[data-history-region="document"]')?.textContent??'',
      selected:dialog?.querySelectorAll('[data-history-row][aria-pressed="true"]').length,
      previous:control('mcp-history-previous'),next:control('mcp-history-next'),export:control('mcp-history-export'),
      rows:[...dialog?.querySelectorAll('[data-history-row]')??[]].map(row=>({id:row.dataset.historyRow,pressed:row.getAttribute('aria-pressed'),state:row.querySelector('[data-history-cell="state"]')?.dataset.state}))};
  })()`);
}

/** Receives the existing actual receiver's resource and cleanup-owned peer/job slots. */
export async function exerciseMcpHistoryPagination({ state, resource, input, cdp, sink, owner, deviceId, profileId }) {
  const jobs = [], plan = mcpPaginationPlan();
  for (const step of plan) {
    state.peer = await state.sender.connectMcpPeer({ audienceDeviceId: deviceId, profileId, rootTaskId: step.rootTaskId, requestKey: step.requestKey });
    state.settled = false; state.jobId = null;
    const accepted = await state.peer.delegate(step.prompt);
    if (!/^[0-9A-Z]{26}$/.test(accepted.job_id ?? "")) throw fail("Pagination request did not return a durable job identity");
    state.jobId = accepted.job_id;
    // Each separate ordinary request has its own fixture hold; no script-role replay.
    await wait("The next pagination request reaches its exact held provider turn", () => state.provider.requestLedger, ledger => {
      const responses = ledger.filter(row => row.route === "responses");
      return responses.length === jobs.length + 1 && responses.every(row => row.contract?.pass === true && row.response_phase === "held");
    });
    await state.peer.cancel(state.jobId);
    const terminal = await wait("Cancelled pagination job releases its worker before the next request", () => state.peer.status(state.jobId), job => job.state === "interrupted", 30_000);
    state.settled = true;
    jobs.push({ job_id: state.jobId, state: terminal.state, request_key: step.requestKey, root_task_id: step.rootTaskId });
    state.protocolCalls.push(...state.peer.calls());
    await state.peer.close(); state.peer = null;
    await sink.record("mcp-pagination-job-settled", jobs.at(-1), { phase: "executing", owner });
  }
  if (!mcpPaginationJobSetMatches(jobs)) throw fail("Pagination requires 21 distinct settled jobs with exact request identities", { jobs });
  await wait("No pagination job remains active", async () => (await invokeDesktopCommand(cdp, "desktop_state")).mcp_activity,
    value => value?.unavailable === false && ["running", "waiting", "awaiting_approval", "cancelling"].every(key => value[key] === 0));
  await trustedClick(input, cdp, action("show-mcp-history", "aside.sidebar"), sink);
  await trustedClick(input, cdp, byId("mcp-history-execution"), sink);
  const newestFirst = jobs.map(job => job.job_id).reverse(), firstIds = newestFirst.slice(0, 20), lastIds = newestFirst.slice(20);
  const first = await wait("Execution page one contains the latest 20 exact jobs", () => observeHistory(cdp), value => mcpHistoryPageMatches(value, firstIds, 1));
  await captureScenarioScreenshot({ cdp, sink, name: "mcp-pagination-first-page", owner });
  const selectRow = async jobId => {
    await trustedClick(input, cdp, { selector: `${HISTORY} button[data-history-row="${jobId}"]`, identity: { tag: "BUTTON", action: "mcp-history-select" } }, sink);
    await wait("Selected job owns the visible detail", () => observeHistory(cdp), value => value.detailOwner === `execution:${jobId}`
      && value.selected === 1 && value.rows.find(row => row.id === jobId)?.pressed === "true"
      && value.document.includes(plan[jobs.findIndex(job => job.job_id === jobId)].prompt)
      && value.export.disabled === false && value.export.ariaDisabled === "false");
  };
  await selectRow(firstIds[0]);
  await trustedClick(input, cdp, byId("mcp-history-next"), sink);
  const last = await wait("Next page shows only job 21 and clears the previous detail", () => observeHistory(cdp), value => mcpHistoryPageMatches(value, lastIds, 2));
  await captureScenarioScreenshot({ cdp, sink, name: "mcp-pagination-last-page", owner });
  await selectRow(lastIds[0]);
  await captureScenarioScreenshot({ cdp, sink, name: "mcp-pagination-last-selected", owner });
  await trustedClick(input, cdp, byId("mcp-history-previous"), sink);
  const returned = await wait("Previous restores the same first-page order and clears selection", () => observeHistory(cdp), value => mcpHistoryPageMatches(value, firstIds, 1));
  await captureScenarioScreenshot({ cdp, sink, name: "mcp-pagination-returned-first", owner });
  await sink.record("mcp-pagination-pages", { jobs, first, last, returned,
    request_setup: "21 actual mTLS delegate_task calls; each held provider turn cancelled and terminal before the next",
    not_proven: ["successful model generation", "instruction-side GUI/history", "physical WinB", "concurrent task throughput"] }, { phase: "executing", owner });
  await trustedClick(input, cdp, byId("mcp-history-close-top"), sink);
  await trustedClick(input, cdp, action("show-hub", "aside.sidebar"), sink);
  await trustedClick(input, cdp, byId("hub-tab-devices"), sink);
  await trustedClick(input, cdp, byId("device-network-receiver-off"), sink);
  await wait("Pagination receiver stops accepting tasks", () => invokeDesktopCommand(cdp, "device_network_projection"), value => value.device_id === deviceId && !value.receiver.enabled);
  await trustedClick(input, cdp, action("close-overlay", `${HUB} .hub-modal-footer`), sink);
  return mcpPaginationOutcome(resource.pageErrors());
}

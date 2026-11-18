import assert from "node:assert/strict";
import { invokeDesktopCommand, captureScenarioScreenshot } from "./observations.mjs";
import { wait } from "./hub_browser_enrollment.mjs";
import { sharedActionTarget } from "./shared_work_navigation.mjs";

const owner = "scenario:settings.shared-work-continuation";

export async function exerciseRunnerRecovery({ mode, state, page, cdp, sink, shared, click, stoppedJobId,
  conversationId, firstJobId, secondJobId, deviceId }) {
  const record = (name, value) => sink.record(name, value, { phase: "executing", owner });
  const latestStopRequests = () => state.provider.requests.filter(request =>
    JSON.stringify(request.messages.filter(message => message.role === "user").at(-1)).includes("desktop-conversation-stop")).length;
  assert.equal(latestStopRequests(), 1);
  assert.equal(new Set([firstJobId, secondJobId, stoppedJobId]).size, 3);
  if (mode === "crash") {
    const pid = await state.runner.interruptCapturedProcess();
    await click(sharedActionTarget("cancel", stoppedJobId));
    const pending = await wait("Cancellation awaits confirmation for the crashed Runner", shared,
      p => p.detail?.id === stoppedJobId && p.detail.state === "cancelling");
    await captureScenarioScreenshot({ cdp, sink, name: "crashed-runner-awaiting-stop", owner });
    await page.locator('nav a[href="#shared-administration"]').click();
    await page.locator('[data-sa-tab="maintenance"]').click();
    await page.locator("#shared-admin-refresh").click();
    const row = page.locator(".shared-admin-row").filter({ hasText: stoppedJobId });
    const settle = row.locator('button[data-sa-operation="settle_attempt"]');
    const attemptId = await settle.getAttribute("data-sa-id");
    await settle.click();
    await page.locator("#shared-admin-reason").fill("隔離RunnerのPIDと終了を照合。固定応答を待つ仕事で外部プロセスの起動はありません。");
    await page.locator("#shared-admin-save").click();
    assert.equal(await page.locator("#shared-admin-form").isVisible(), true);
    assert.equal((await shared()).detail.state, "cancelling");
    assert.equal((await state.runner.verifyExited()).pass, true);
    await state.resource.screenshot("hub-confirm-exact-stopped-attempt");
    await page.locator('input[name="acknowledge_stopped"]').check();
    await page.locator("#shared-admin-save").click();
    await page.locator("#shared-admin-form").waitFor({ state: "hidden" });
    await settle.waitFor({ state: "detached" });
    const done = await wait("Only the confirmed turn settles in its original conversation", shared,
      p => p.detail?.id === stoppedJobId && p.selected_conversation_id === conversationId && p.detail.state === "cancelled");
    await captureScenarioScreenshot({ cdp, sink, name: "crashed-runner-settled", owner });
    await record("recovery-crash-settled", { pid, conversationId, firstJobId, secondJobId, stoppedJobId,
      attemptId, pending: pending.detail.state, terminal: done.detail.state, providerCallsForStoppedJob: latestStopRequests() });
  } else {
    await page.locator('nav a[href="#clients"]').click();
    const row = page.locator(`[data-id="device:${deviceId}"]`);
    await row.getByRole("button", { name: /を管理$/ }).click();
    await page.locator("#network-device-revoke-details > summary").click();
    await page.locator("#network-device-revoked").check();
    await page.locator("#network-device-save").click();
    await row.getByText("恒久失効", { exact: true }).waitFor();
    const denied = await wait("Revoked Desktop loses its shared principal", shared,
      p => !p.principal && !p.projects.length && !p.detail, 60_000);
    const network = await wait("The next device heartbeat reports revocation for this PC",
      () => invokeDesktopCommand(cdp, "device_network_projection"), p => p.enrollment === "revoked", 45_000);
    await wait("The visible chat removes previous messages and stops offering actions on the revoked job", () => cdp.evaluate(
      `!document.querySelector('.shared-work button[data-action="send"]:enabled')
      && !(document.querySelector('[data-shared-region="history-container"]')?.innerText ?? '').includes("desktop-conversation-stop")
      && !document.querySelector('button[data-action="shared-cancel"][data-value="' + ${JSON.stringify(stoppedJobId)} + '"]')`), Boolean, 30_000);
    await wait("The connection panel explains the revoked credential", () => cdp.evaluate(
      `document.querySelector('[data-shared-region="connection"]')?.innerText ?? ''`), text => text.includes("失効"), 18_000);
    await captureScenarioScreenshot({ cdp, sink, name: "revoked-desktop", owner });
    await state.resource.screenshot("hub-revoked-active-runner");
    await record("recovery-revoked", { stoppedJobId, deviceId, conversationId, principal: denied.principal,
      projects: denied.projects.length, providerCallsForStoppedJob: latestStopRequests(),
      enrollment: network.enrollment });
  }
  assert.equal(latestStopRequests(), 1);
  return { acquisition: "pass", oracle: "pass", manual: "pending" };
}

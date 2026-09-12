import assert from "node:assert/strict";
import test from "node:test";
import { renderConfirmation } from "../src/render_overlays.ts";
import type { DesktopWebState } from "../src/types.ts";
import { renderDeviceNetworkJobs } from "../src/device_network_jobs.ts";
import { deviceUiFixture } from "./device_network_fixture.ts";
import { turnStopTarget } from "./stop_target_fixture.ts";

test("receiver permission presents exact work and separate allow, deny and task stop without a local Main stop", () => {
  const state = {
    confirmation_visible: true, confirmation_id: "remote-permission-a", confirmation_text: "",
    can_cancel_run: true, stop_target: turnStopTarget(),
    confirmation: { summary: "CPU使用率を調べます", details: ["Get-Counter Processor"], targets: ["Temp"],
      outside_workspace: false, risks: [], remote: { job_id: "job-b", profile_id: "receiver-b", session_id: "session-b",
        requester_label: "WinA <controller>", target_label: "Temp（一時作業用）" } },
  } as unknown as DesktopWebState;
  const html = renderConfirmation(state);
  assert.match(html, /WinA &lt;controller&gt;/);
  assert.match(html, /Temp（一時作業用）/);
  assert.match(html, /job-b/);
  assert.match(html, /data-action="deny-permission"[^>]*autofocus>許可しない/);
  assert.match(html, /data-action="approve-permission"[^>]*>この操作を許可/);
  assert.match(html, /data-action="abort-permission"[^>]*>タスクを停止/);
  assert.doesNotMatch(html, /data-action="cancel-run"/);
  assert.doesNotMatch(html, /指示を変更する/);
  const pending = renderConfirmation(state, { phase: "submitting", requestId: "remote-permission-a", submissionId: 1, decision: "denied" });
  assert.match(pending, /拒否を反映しています/);
  for (const button of pending.matchAll(/<button[^>]*data-permission-action[^>]*>/g)) assert.match(button[0], /disabled/);
  assert.doesNotMatch(pending, /autofocus/);
});

test("sender and receiver show canonical approval waiting while preserving task stop availability", () => {
  const local = deviceUiFixture();
  local.jobs.outgoing[0].state = "awaiting_approval";
  local.jobs.incoming[0].state = "awaiting_approval";
  const html = renderDeviceNetworkJobs(local);
  assert.equal([...html.matchAll(/受入端末で承認待ち/g)].length, 2);
  assert.match(html, /Win00 → Win19 → Win20/);
  const stops = [...html.matchAll(/<button[^>]*data-action="device-network-stop-job"[^>]*>/g)];
  assert.equal(stops.length, 2);
  for (const button of stops) assert.doesNotMatch(button[0], /disabled/);
});

test("long permission details have a keyboard-readable region separate from identity and decisions", () => {
  const requester = "WinA <controller> " + "long device name ".repeat(12);
  const target = "C:\\projects\\" + "long receiving folder\\".repeat(18);
  const command = "Write-Output <reviewed>\n".repeat(100);
  const summary = "Review this exact operation. ".repeat(80);
  const state = {
    confirmation_visible: true, confirmation_id: "permission-long", confirmation_text: "",
    confirmation: { summary, details: [command], targets: [target], outside_workspace: true,
      risks: ["outside target"], remote: { job_id: "job-long", profile_id: "receiver-long",
        session_id: "session-long", requester_label: requester, target_label: target } },
  } as unknown as DesktopWebState;
  const html = renderConfirmation(state);
  const header = html.match(/<header\b[^>]*>([\s\S]*?)<\/header>/)?.[1];
  const review = html.match(/<section\b[^>]*role="region"[^>]*>([\s\S]*?)<\/section>/)?.[1];
  const footer = html.match(/<footer\b[^>]*>([\s\S]*?)<\/footer>/)?.[1];
  assert.ok(header, "permission identity must be outside the scrollable operation details");
  assert.ok(review, "complete details must remain accessible in their own named region");
  assert.ok(footer, "decision actions and status must remain outside the details region");
  assert.match(header, /id="permission-title">受入タスクの操作を確認/);
  assert.match(header, /依頼元/);
  assert.match(header, /受入場所/);
  assert.match(header, /WinA &lt;controller&gt;/);
  assert.ok(header.includes(target));
  assert.match(html, /<section[^>]*role="region"[^>]*aria-label="操作の詳細"[^>]*tabindex="0"/);
  assert.match(review, /id="permission-summary"/);
  assert.ok(review.includes(summary), "long operation summary is never truncated from the review");
  assert.ok(review.includes(command.replaceAll("<", "&lt;").replaceAll(">", "&gt;")));
  assert.ok(review.includes(target), "complete receiving path remains available when its header preview is narrow");
  assert.match(review, /job-long/);
  assert.match(review, /outside target/);
  assert.doesNotMatch(review, /data-permission-action/);
  assert.doesNotMatch(header, /Write-Output|data-permission-action/);
  assert.match(footer, /role="status"[^>]*aria-live="polite"/);
  assert.deepEqual([...footer.matchAll(/data-action="([^"]+)"/g)].map(match => match[1]),
    ["deny-permission", "abort-permission", "approve-permission"]);
  assert.match(footer, /data-action="deny-permission"[^>]*autofocus/);
  assert.doesNotMatch(footer, /Write-Output/);
});

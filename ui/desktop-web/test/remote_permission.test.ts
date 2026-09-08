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

import assert from "node:assert/strict";
import test from "node:test";
import { acceptDeviceNetworkProjection, createDeviceNetworkUiState, deviceCanJoin, deviceCanReceive, deviceCanSelect,
  deviceCanStopJob, deviceDraftCurrent, deviceNetworkPresentation, devicePeerAvailability, devicePeerKey,
  editDeviceNetworkField, visibleDevicePeers } from "../src/device_network_state.ts";
import { renderDeviceNetwork } from "../src/device_network_render.ts";
import { devicePathLabel, renderDeviceNetworkJobs } from "../src/device_network_jobs.ts";
import { renderHubOverlay } from "../src/hub_render.ts";
import { createHubUiState } from "../src/hub_state.ts";
import { synchronizeRetainedSettingsSurface } from "../src/settings_surface.ts";
import { deviceProjection, deviceUiFixture } from "./device_network_fixture.ts";

test("join consent is separate from the explicit receiver grant and never selects peers automatically", () => {
  const local = createDeviceNetworkUiState();
  acceptDeviceNetworkProjection(local, deviceProjection({ device_id: null, enrollment: "not_enrolled", can_join: true }));
  assert.equal(deviceCanJoin(local), false);
  editDeviceNetworkField(local, "join_confirmed", "", true);
  assert.equal(deviceCanJoin(local), true);
  assert.equal(deviceCanReceive(local, true), false);
  acceptDeviceNetworkProjection(local, deviceProjection());
  assert.deepEqual(local.target, { kind: "temp" });
  assert.equal(local.startOnLaunch, false);
  assert.equal(local.keepWhenHidden, false);
  assert.equal(local.projection!.peers.some(peer => peer.selected), false);
  assert.equal(deviceCanReceive(local, true), false);
  editDeviceNetworkField(local, "receiver_confirmed", "", true);
  assert.equal(deviceCanReceive(local, true), true);
  editDeviceNetworkField(local, "access", "full_access", false);
  assert.equal(local.receiverConfirmed, false);
  assert.equal(deviceCanReceive(local, true), false);
});

test("poll preserves receiver draft and its reviewed owner; explicit refresh acquires the new owner without consent", () => {
  const local = deviceUiFixture();
  editDeviceNetworkField(local, "target", "project:project-a", false);
  editDeviceNetworkField(local, "keep_when_hidden", "", true);
  editDeviceNetworkField(local, "receiver_confirmed", "", true);
  const target = structuredClone(local.target);
  const newer = deviceProjection({ revision: "4", generation: "8" });
  acceptDeviceNetworkProjection(local, newer);
  assert.deepEqual(local.target, target);
  assert.equal(local.keepWhenHidden, true);
  assert.equal(deviceDraftCurrent(local), false);
  assert.equal(deviceCanReceive(local, true), false);
  assert.equal(acceptDeviceNetworkProjection(local, deviceProjection()), false);
  acceptDeviceNetworkProjection(local, newer, { reviewLatest: true });
  assert.deepEqual(local.target, target);
  assert.equal(local.keepWhenHidden, true);
  assert.equal(deviceDraftCurrent(local), true);
  assert.equal(local.receiverConfirmed, false);
});

test("the current exact project path is required and reception OFF does not need a new grant", () => {
  const local = deviceUiFixture();
  editDeviceNetworkField(local, "target", "project:project-a", false);
  editDeviceNetworkField(local, "receiver_confirmed", "", true);
  local.projection!.targets[1].target = { kind: "project", project_id: "project-a", workspace_root: "C:/moved" };
  assert.equal(deviceCanReceive(local, true), false);
  local.projection!.receiver.enabled = true;
  assert.equal(deviceCanReceive(local, false), true);
  local.projection!.receiver.can_change = false;
  assert.equal(deviceCanReceive(local, false), false);
});

test("saved receiver consent permits same-authority restart and never grants changed target, access or model", () => {
  const local = deviceUiFixture();
  local.projection!.receiver.confirmed = true;
  assert.equal(deviceCanReceive(local, true), true);
  editDeviceNetworkField(local, "keep_when_hidden", "", true);
  assert.equal(deviceCanReceive(local, true), true);
  editDeviceNetworkField(local, "access", "full_access", false);
  assert.equal(deviceCanReceive(local, true), false);
  editDeviceNetworkField(local, "access", "default", false);
  assert.equal(deviceCanReceive(local, true), true);
  editDeviceNetworkField(local, "model", "direct", false);
  assert.equal(deviceCanReceive(local, true), false);
  editDeviceNetworkField(local, "model", "hub", false);
  editDeviceNetworkField(local, "target", "project:project-a", false);
  assert.equal(deviceCanReceive(local, true), false);
});

test("device and publication identity gate selection; a disconnected selected peer can still be disabled", () => {
  const local = deviceUiFixture();
  const peer = local.projection!.peers[0];
  assert.equal(deviceCanSelect(local, devicePeerKey(peer)), true);
  assert.equal(deviceCanSelect(local, JSON.stringify([peer.device_id, "another-profile"])), false);
  peer.can_use = false; peer.reason = "policy_denied";
  assert.equal(deviceCanSelect(local, devicePeerKey(peer)), false);
  peer.selected = true; local.projection!.enrollment = "disconnected";
  assert.equal(deviceCanSelect(local, devicePeerKey(peer)), true);
  local.pending = "select";
  assert.equal(deviceCanSelect(local, devicePeerKey(peer)), false);
});

test("directory announcements do not infer connection history and mixed absence stays explicit", () => {
  const local = deviceUiFixture();
  const peer = local.projection!.peers[0];
  peer.selected = true;
  assert.equal(devicePeerAvailability(peer).label, "受付申告あり");
  assert.doesNotMatch(devicePeerAvailability(peer).label, /利用可能|接続済み|接続確認前/);
  assert.match(renderDeviceNetwork(local), /接続は実際の依頼時に確認します/);
  peer.online = false; peer.can_use = false; peer.reason = "not_available_or_not_allowed";
  assert.match(devicePeerAvailability(peer).label, /状態・許可/);
  const html = renderDeviceNetwork(local);
  assert.doesNotMatch(html, /not_available_or_not_allowed/);
  assert.match(html, /受付・接続、またはHubの許可/);
  editDeviceNetworkField(local, "search", "開発", false);
  assert.deepEqual(visibleDevicePeers(local).map(peer => peer.device_id), ["device-20"]);
});

test("receiver status follows the actual Rust receiving and paused projection without claiming an unknown state is stopped", () => {
  const local = deviceUiFixture();
  for (const [status, enabled, label] of [["receiving", true, "受付 ON · 受付中"], ["paused", false, "受付 OFF · 停止中"],
    ["starting", true, "受付 ON · 受付を準備中"], ["stopping", false, "受付 OFF · 停止を確認中"],
    ["error", true, "受付 ON · 受付できません"], ["future-state", true, "受付 ON · 状態を確認しています"]] as const) {
    const projection = deviceProjection();
    projection.receiver = { ...projection.receiver, status, enabled, confirmed: true };
    acceptDeviceNetworkProjection(local, projection);
    const markup = renderDeviceNetwork(local);
    const shown = markup.match(/data-settings-passive="device-network-receiver-status">([^<]*)</)![1];
    assert.equal(shown, label);
  }
});

test("successful participation feedback is a notice while local and projected failures retain their error tone", () => {
  const local = deviceUiFixture();
  const feedback = () => renderDeviceNetwork(local).match(/<div id="device-network-feedback"([^>]*)>([^<]*)<\/div>/)!;
  local.notice = "Hubに参加しました。公開対象と権限を確認してください。";
  assert.match(feedback()[1], /data-error="false"/);
  assert.equal(feedback()[2], local.notice);
  local.error = "参加コードを確認してください。";
  assert.match(feedback()[1], /data-error="true"/);
  assert.equal(feedback()[2], local.error);
  local.error = "";
  local.projection!.error = "policy_denied";
  assert.match(feedback()[1], /data-error="true"/);
  assert.notEqual(feedback()[2], local.notice);
  local.projection!.error = null;
  assert.match(feedback()[1], /data-error="false"/);
  assert.equal(feedback()[2], local.notice);
  local.notice = "";
  assert.match(feedback()[1], /\bhidden\b/);
});

test("temporary disconnect preserves registration and selected targets with an explicit reconnect action", () => {
  const local = deviceUiFixture();
  local.projection!.peers[0].selected = true;
  local.projection!.enrollment = "disconnected";
  const html = renderDeviceNetwork(local);
  assert.match(html, /id="device-network-refresh"[^>]*>再接続</);
  assert.match(html, /登録ID・公開対象・実行権限・利用先の選択は保持/);
  assert.match(html, /受付は手動でON/);
  assert.match(html, /data-action="device-network-leave"[^>]*>接続を一時解除</);
  assert.doesNotMatch(html, /参加を解除/);
  assert.match(html, /data-network-visible="join" hidden/);
  assert.equal(local.projection!.device_id, "device-00");
  assert.equal(local.projection!.peers[0].selected, true);
});

test("route display keeps delegated path, exact stop identity, and unconfirmed cancellation distinct", () => {
  const local = deviceUiFixture();
  assert.equal(devicePathLabel(local, local.jobs.outgoing[0].device_path), "Win00 → Win19 → Win20");
  assert.equal(deviceCanStopJob(local, "outgoing:reference-a"), true);
  assert.equal(deviceCanStopJob(local, "outgoing:task-00"), false);
  assert.equal(deviceCanStopJob(local, "incoming:job-00"), true);
  local.jobs.incoming[0].profile_id = "other-profile";
  assert.equal(deviceCanStopJob(local, "incoming:job-00"), false);
  local.jobs.outgoing[0].stop_status = "unconfirmed";
  local.jobs.outgoing[0].state = "unknown";
  const html = renderDeviceNetworkJobs(local);
  assert.match(html, /停止完了は未確認/);
  assert.doesNotMatch(html, /停止確認済み/);
  assert.match(html, /Win00 → Win19 → Win20/);
  assert.doesNotMatch(html, /このPCの作業を確認/);
});

test("joined devices use the model owner with manual connection folded; leaving stays discoverable while revoked", () => {
  const local = deviceUiFixture();
  const hub = createHubUiState();
  const html = renderHubOverlay(hub, deviceNetworkPresentation(local));
  assert.match(html, /端末連携で参加したHubからモデルを取得/);
  assert.match(html, /<details id="hub-manual-connection"[^>]*>/);
  assert.doesNotMatch(html.match(/<details id="hub-manual-connection"[^>]*>/)![0], /\bopen\b/);
  assert.doesNotMatch(html, /class="modal-backdrop"[^>]*data-action/);
  local.projection!.enrollment = "revoked";
  const revoked = renderDeviceNetwork(local);
  assert.doesNotMatch(revoked.match(/<details class="device-network-leave"[^>]*>/)![0], /\bhidden\b/);
});

test("a newly inserted job cannot apply its Stop availability to the retained previous job button", () => {
  function controls(html: string) {
    return [...html.matchAll(/<button\b([^>]*)>([\s\S]*?)<\/button>/g)].map(match => {
      const attrs = new Map([...match[1].matchAll(/([\w-]+)="([^"]*)"/g)].map(item => [item[1], item[2]]));
      return { tagName: "BUTTON", innerHTML: match[2], disabled: /\bdisabled\b/.test(match[1]), hidden: false,
        getAttribute: (name: string) => attrs.get(name) ?? null, hasAttribute: (name: string) => attrs.has(name),
        setAttribute: (name: string, value: string) => attrs.set(name, value), removeAttribute: (name: string) => attrs.delete(name) };
    });
  }
  function modal(buttons: ReturnType<typeof controls>) {
    return { setAttribute() {}, querySelector: () => null,
      querySelectorAll: (selector: string) => selector === "button, input, select, textarea" ? buttons : [] } as unknown as HTMLElement;
  }
  const local = deviceUiFixture();
  const current = controls(renderDeviceNetworkJobs(local));
  const original = current.find(button => button.getAttribute("data-value") === "outgoing:reference-a")!;
  local.jobs.outgoing.unshift({ ...local.jobs.outgoing[0], reference_id: "new-job", can_stop: false, state: "completed" });
  const next = controls(renderDeviceNetworkJobs(local));
  synchronizeRetainedSettingsSurface(modal(current), modal(next), false);
  assert.equal(original.disabled, false, "the existing running job retains its own enabled Stop control");
  local.jobs.outgoing[1].can_stop = false;
  synchronizeRetainedSettingsSurface(modal(current), modal(controls(renderDeviceNetworkJobs(local))), false);
  assert.equal(original.disabled, true, "the same exact job's later completion closes its Stop control");
});

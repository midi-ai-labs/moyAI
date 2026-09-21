import assert from "node:assert/strict";
import test from "node:test";
import { acceptDeviceNetworkProjection, createDeviceNetworkUiState, deviceCanJoin, deviceCanReceive, deviceCanSelect,
  deviceCanStopJob, deviceDraftCurrent, deviceNetworkError, deviceNetworkPresentation, devicePeerAvailability, devicePeerKey,
  editDeviceNetworkField, visibleDevicePeers } from "../src/device_network_state.ts";
import { renderDeviceConnectionReset, renderDeviceNetwork } from "../src/device_network_render.ts";
import { devicePathLabel, renderDeviceNetworkJobs } from "../src/device_network_jobs.ts";
import { renderHubOverlay, renderManagedAiConnection } from "../src/hub_render.ts";
import { createHubUiState } from "../src/hub_state.ts";
import { synchronizeRetainedSettingsSurface } from "../src/settings_surface.ts";
import { deviceProjection, deviceUiFixture } from "./device_network_fixture.ts";

test("endpoint change failures distinguish retained trust, busy execution and an unconfirmed shutdown", () => {
  assert.match(deviceNetworkError("different_hub"), /別Hubや公開CAの変更には対応していません/);
  assert.match(deviceNetworkError("endpoint_change_busy"), /受付を一時停止/);
  assert.match(deviceNetworkError("endpoint_change_runner_unconfirmed"), /安全な終了を確認できません/);
  assert.match(deviceNetworkError("endpoint_change_not_saved"), /接続先は変更していません/);
  assert.match(deviceNetworkError("endpoint_change_not_saved"), /自動再開しません/);
});

test("approval enrollment never grants reception or selects peers automatically", () => {
  const local = createDeviceNetworkUiState();
  acceptDeviceNetworkProjection(local, deviceProjection({ device_id: null, enrollment: "not_enrolled", can_join: true }));
  assert.equal(deviceCanJoin(local), true);
  acceptDeviceNetworkProjection(local, deviceProjection({ device_id: null, enrollment: "pending", can_join: false, request_id: "request-a" }));
  assert.equal(deviceCanJoin(local), false);
  const pendingHtml = renderDeviceNetwork(local);
  assert.match(pendingHtml, /管理者の参加承認待ち/);
  assert.doesNotMatch(pendingHtml, /id="device-network-code"|id="device-network-join-confirmed"/);
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

test("receiver address drafts survive polling and invalid fixed settings block ON without blocking OFF", () => {
  const local = deviceUiFixture();
  local.projection!.receiver.confirmed = true;
  local.projection!.receiver.enabled = true;
  editDeviceNetworkField(local, "bind_ip", "192.168.10.22", false);
  editDeviceNetworkField(local, "port", "7443", false);
  assert.equal(deviceCanReceive(local, true), true);
  const newer = deviceProjection({ revision: "4", generation: "8" });
  newer.receiver = { ...newer.receiver, confirmed: true, enabled: true, bind_ip: "10.0.0.8", port: 7332 };
  acceptDeviceNetworkProjection(local, newer);
  assert.equal(local.bindIp, "192.168.10.22");
  assert.equal(local.port, "7443");
  assert.equal(deviceCanReceive(local, true), false);
  acceptDeviceNetworkProjection(local, newer, { reviewLatest: true });
  assert.equal(deviceCanReceive(local, true), true);
  for (const ip of ["0.0.0.0", "224.0.0.1", "255.255.255.255", "192.168.1.999", "192.168.1", "localhost"]) {
    editDeviceNetworkField(local, "bind_ip", ip, false);
    assert.equal(deviceCanReceive(local, true), false, ip);
    assert.equal(deviceCanReceive(local, false), true);
  }
  editDeviceNetworkField(local, "bind_ip", "", false);
  for (const port of ["0", "65536", "1.5", "abc"]) {
    editDeviceNetworkField(local, "port", port, false);
    assert.equal(deviceCanReceive(local, true), false, port);
  }
  editDeviceNetworkField(local, "port", "", false);
  assert.equal(deviceCanReceive(local, true), true);
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
  assert.doesNotMatch(renderDeviceNetwork(local), /data-action="device-network-select"/);
  peer.online = false; peer.can_use = false; peer.reason = "not_available_or_not_allowed";
  assert.match(devicePeerAvailability(peer).label, /状態・許可/);
  const html = renderDeviceNetwork(local);
  assert.doesNotMatch(html, /not_available_or_not_allowed/);
  assert.match(html, /操作・実行に使うPCはHub管理者が設定/);
  editDeviceNetworkField(local, "search", "開発", false);
  assert.deepEqual(visibleDevicePeers(local).map(peer => peer.device_id), ["device-20"]);
});

test("retired receiver and individual peer controls stay absent even with old saved settings", () => {
  const local = deviceUiFixture();
  local.projection.receiver.enabled = true;
  local.projection.peers[0].selected = true;
  const html = renderDeviceNetwork(local);
  assert.doesNotMatch(html, /data-action="device-network-(?:receiver-on|receiver-off|select)"/);
  assert.match(html, /id="device-execution"/);
});

test("successful participation feedback is a notice while local and projected failures retain their error tone", () => {
  const local = deviceUiFixture();
  const feedback = () => renderDeviceNetwork(local).match(/<div id="device-network-feedback"([^>]*)>([^<]*)<\/div>/)!;
  local.notice = "Hubに参加しました。公開対象と権限を確認してください。";
  assert.match(feedback()[1], /data-error="false"/);
  assert.equal(feedback()[2], local.notice);
  local.error = "Hubの参加申請の状態を確認してください。";
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
  assert.match(html, /id="device-network-refresh"[^>]*>再接続・最新情報を取得</);
  assert.match(html, /PCの登録を保持して接続を解除/);
  assert.match(html, /同じ設定で再接続/);
  assert.match(html, /data-action="device-network-leave"[^>]*>接続を一時解除</);
  assert.doesNotMatch(html, /参加を解除/);
  assert.match(html, /id="device-network-join"[^>]*disabled/);
  assert.equal(local.projection!.device_id, "device-00");
  assert.equal(local.projection!.peers[0].selected, true);
});

test("reset reports its result next to the action in either settings surface", () => {
  const local = deviceUiFixture();
  const feedback = () => renderDeviceConnectionReset(local).match(/<div id="device-network-reset-feedback"([^>]*)>([^<]*)<\/div>/)!;
  assert.match(feedback()[1], /\bhidden\b/);
  local.notice = "接続設定をリセットしました。履歴・成果物・未確認記録は保持しています。";
  assert.equal(feedback()[2], local.notice);
  assert.doesNotMatch(feedback()[1], /\bhidden\b/);
  assert.match(feedback()[1], /data-settings-passive="device-network-reset-feedback"/);
  local.projection.error = "reset_autostart_unconfirmed";
  assert.match(feedback()[1], /data-error="true"/);
  assert.equal(feedback()[2], deviceNetworkError("reset_autostart_unconfirmed"));
  local.error = "保存先を確認してください。";
  assert.equal(feedback()[2], local.error);
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

test("joined devices link to the single AI settings form; leaving stays discoverable while revoked", () => {
  const local = deviceUiFixture();
  const hub = createHubUiState();
  const html = renderHubOverlay(hub, deviceNetworkPresentation(local));
  assert.match(html, /data-action="show-config">AIの接続/);
  assert.doesNotMatch(html, /hub-manual-connection|hub-token/);
  assert.doesNotMatch(html, /class="modal-backdrop"[^>]*data-action/);
  local.projection!.enrollment = "revoked";
  const revoked = renderDeviceNetwork(local);
  assert.doesNotMatch(revoked.match(/<details class="device-network-leave"[^>]*>/)![0], /\bhidden\b/);
});

test("pending approval updates the connected model help through retained settings without replacing the receiver draft", () => {
  function retainedHelp(html: string) {
    const match = html.match(/<p[^>]*class="hub-help"([^>]*data-settings-passive="ai-main-status"[^>]*)>([^<]*)<\/p>/)!;
    const attributes = new Map([...match[1].matchAll(/([\w-]+)="([^"]*)"/g)].map(item => [item[1], item[2]]));
    const region = {
      textContent: match[2], dataset: { settingsPassive: attributes.get("data-settings-passive") },
      hasAttribute: (name: string) => attributes.has(name),
      querySelectorAll: () => [],
      isEqualNode: (next: { textContent: string }) => region.textContent === next.textContent,
      replaceWith(next: { textContent: string }) { region.textContent = next.textContent; },
    };
    return { region, setAttribute() {}, querySelector: () => null,
      contains: (node: unknown) => node === region,
      querySelectorAll: (selector: string) => selector === "[data-settings-passive]" && region.dataset.settingsPassive ? [region] : [] };
  }
  const local = deviceUiFixture();
  acceptDeviceNetworkProjection(local, deviceProjection({ enrollment: "pending", device_id: null }));
  editDeviceNetworkField(local, "target", "project:project-a", false);
  const draft = structuredClone(local.target);
  const hub = createHubUiState();
  const current = retainedHelp(renderManagedAiConnection(hub, "main", deviceNetworkPresentation(local)));
  assert.match(current.region.textContent, /参加承認を待っています/);
  acceptDeviceNetworkProjection(local, deviceProjection({ generation: "8" }));
  const next = retainedHelp(renderManagedAiConnection(hub, "main", deviceNetworkPresentation(local)));
  synchronizeRetainedSettingsSurface(current as unknown as HTMLElement, next as unknown as HTMLElement, false);
  assert.match(current.region.textContent, /再接続を待っています/);
  assert.doesNotMatch(current.region.textContent, /参加承認を待っています/);
  assert.deepEqual(local.target, draft);
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

test("administrator stop and credential revocation stay distinct while pending approval preserves receiver drafts", () => {
  const local = deviceUiFixture();
  editDeviceNetworkField(local,"target","project:project-a",false);
  const target = structuredClone(local.target);
  acceptDeviceNetworkProjection(local,deviceProjection({enrollment:"stopped",error:"device_stopped",generation:"8"}));
  assert.match(renderDeviceNetwork(local),/管理者が利用を停止中/);
  assert.equal(deviceCanReceive(local,true),false);
  acceptDeviceNetworkProjection(local,deviceProjection({enrollment:"revoked",error:"device_revoked",generation:"9"}));
  assert.match(renderDeviceNetwork(local),/このPCの認証が失効しています/);
  assert.deepEqual(local.target,target);
  acceptDeviceNetworkProjection(local,deviceProjection({enrollment:"active",generation:"10"}));
  assert.match(local.notice,/承認され、接続しました/);
  assert.deepEqual(local.target,target);
  assert.equal(local.receiverConfirmed,false);
});

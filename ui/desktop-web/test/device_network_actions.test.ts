import assert from "node:assert/strict";
import test from "node:test";
import type { ActionContext } from "../src/actions.ts";
import { importDeviceNetwork, joinDeviceNetwork, loadDeviceNetwork, refreshDeviceNetworkJobs, selectDevicePeer, setDeviceReceiver, stopDeviceNetworkJob } from "../src/device_network_actions.ts";
import { acceptDeviceNetworkProjection, devicePeerKey, editDeviceNetworkField, type DeviceNetworkProjection } from "../src/device_network_state.ts";
import { deviceProjection, deviceUiFixture } from "./device_network_fixture.ts";
import { renderDeviceNetwork } from "../src/device_network_render.ts";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function fixture() {
  const local = deviceUiFixture();
  const code = { value: "one-use-private-code" };
  const view = { overlay: "hub" };
  const context = { uiState: { deviceNetwork: local, hub: { tab: "devices" } }, getViewState: () => view, rerender() {} } as unknown as ActionContext;
  return { context, local, code, view };
}
async function withContext(invoke: (name: string, args: Record<string, unknown>) => Promise<unknown>, run: (value: ReturnType<typeof fixture>) => Promise<void>) {
  const value = fixture();
  const globals = ["window", "document"] as const;
  const originals = globals.map(key => Object.getOwnPropertyDescriptor(globalThis, key));
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke } } });
  Object.defineProperty(globalThis, "document", { configurable: true, value: { querySelector: (key: string) => key === "#device-network-code" ? value.code : null } });
  try { await run(value); }
  finally { globals.forEach((key, index) => { if (originals[index]) Object.defineProperty(globalThis, key, originals[index]!); else delete (globalThis as Record<string, unknown>)[key]; }); }
}

test("public configuration import starts approval enrollment without caller name, code or receiver authority", async () => {
  const calls: { name: string; args: Record<string, unknown> }[] = [];
  await withContext(async (name, args) => {
    calls.push({ name, args });
    return deviceProjection({ device_id: null, enrollment: "pending", can_join: false, request_id: "request-a", local_ipv4: "192.168.2.10", generation: "8" });
  }, async ({ context, local }) => {
    acceptDeviceNetworkProjection(local, deviceProjection({ device_id: null, enrollment: "unconfigured", can_join: false }));
    await importDeviceNetwork(context);
    assert.deepEqual(calls, [{name:"device_network_import", args:{expectedRevision:"3",expectedGeneration:"7"}}]);
    assert.equal(local.projection?.enrollment, "pending");
    assert.match(local.notice, /承認を待っています/);
    assert.equal(local.receiverConfirmed, false);
    assert.equal(local.projection?.receiver.enabled, false);
    assert.equal(local.projection?.peers.some(peer => peer.selected), false);
  });
});

test("failed enrollment retry preserves the command lane and retries only the latest owner without a code", async () => {
  const recovery = deferred<DeviceNetworkProjection>();
  const calls: {name:string;args:Record<string,unknown>}[] = [];
  await withContext(async (name, args) => {
    if (name === "device_network_projection") return recovery.promise;
    calls.push({name,args});
    if (calls.length === 1) throw "enrollment_denied";
    return deviceProjection({ revision: "4", generation: "9", device_id:null, enrollment:"pending", can_join:false });
  }, async ({ context, local }) => {
    acceptDeviceNetworkProjection(local, deviceProjection({ device_id: null, enrollment: "not_enrolled", can_join: true }));
    const first = joinDeviceNetwork(context);
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(local.pending, "join");
    await joinDeviceNetwork(context);
    assert.equal(calls.length, 1);
    recovery.resolve(deviceProjection({ device_id: null, enrollment: "error", can_join: true, generation: "8" }));
    await first;
    assert.equal(local.pending, null);
    assert.match(local.error, /参加申請/);
    await joinDeviceNetwork(context);
    assert.deepEqual(calls[1], {name:"device_network_request_join",args:{expectedRevision:"3",expectedGeneration:"8"}});
    assert.equal(local.projection?.enrollment, "pending");
  });
});

test("stale load completion cannot replace a newer owner or its receiver draft", async () => {
  const old = deferred<DeviceNetworkProjection>();
  await withContext(async name => name === "device_network_projection" ? old.promise : { incoming: [], outgoing: [] }, async ({ context, local, view }) => {
    const load = loadDeviceNetwork(context);
    ++local.requestSerial; local.pending = null;
    acceptDeviceNetworkProjection(local, deviceProjection({ revision: "9", generation: "11" }));
    editDeviceNetworkField(local, "access", "full_access", false);
    editDeviceNetworkField(local, "bind_ip", "192.168.2.22", false);
    editDeviceNetworkField(local, "port", "invalid draft", false);
    editDeviceNetworkField(local, "search", "Win20", false);
    view.overlay = "none";
    old.resolve(deviceProjection());
    await load;
    assert.equal(local.projection!.generation, "11");
    assert.equal(local.accessMode, "full_access");
    assert.equal(local.search, "Win20");
    assert.equal(local.dirty, true);
  });
});

test("receiver OFF sends the persisted grant and retains edited authority rather than publishing it", async () => {
  const calls: { name: string; args: Record<string, unknown> }[] = [];
  await withContext(async (name, args) => { calls.push({ name, args }); return deviceProjection({ revision: "4", generation: "8" }); }, async ({ context, local }) => {
    local.projection!.receiver.enabled = true;
    editDeviceNetworkField(local, "target", "project:project-a", false);
    editDeviceNetworkField(local, "access", "full_access", false);
    editDeviceNetworkField(local, "bind_ip", "192.168.2.22", false);
    editDeviceNetworkField(local, "port", "invalid draft", false);
    const draft = structuredClone(local.target);
    await setDeviceReceiver(context, false);
    assert.deepEqual(calls, [{ name: "device_network_receiver", args: { enabled: false, target: { kind: "temp" }, accessMode: "default",
      modelMode: "hub", confirmed: false, startOnLaunch: false, keepWhenHidden: false, bindIp: null, port: null, expectedRevision: "3", expectedGeneration: "7" } }]);
    assert.deepEqual(local.target, draft);
    assert.equal(local.accessMode, "full_access");
    assert.equal(local.dirty, true);
    assert.equal(local.bindIp, "192.168.2.22");
    assert.equal(local.port, "invalid draft");
  });
});

test("receiver ON delivers fixed address and port with the reviewed owner; automatic settings remain null", async () => {
  const calls: Record<string, unknown>[] = [];
  await withContext(async (_name, args) => {
    calls.push(args);
    const projection = deviceProjection({ revision: String(3 + calls.length), generation: String(7 + calls.length) });
    projection.receiver = { ...projection.receiver, confirmed: true, enabled: true,
      bind_ip: args.bindIp as string | null, port: args.port as number | null };
    return projection;
  }, async ({ context, local }) => {
    local.projection!.receiver.confirmed = true;
    editDeviceNetworkField(local, "bind_ip", " 192.168.2.22 ", false);
    editDeviceNetworkField(local, "port", "7443", false);
    await setDeviceReceiver(context, true);
    assert.equal(calls[0].bindIp, "192.168.2.22");
    assert.equal(calls[0].port, 7443);
    assert.equal(calls[0].expectedRevision, "3");
    assert.equal(calls[0].expectedGeneration, "7");
    assert.equal(local.bindIp, "192.168.2.22");
    assert.equal(local.port, "7443");
    editDeviceNetworkField(local, "bind_ip", "", false);
    editDeviceNetworkField(local, "port", "", false);
    await setDeviceReceiver(context, true);
    assert.equal(calls[1].bindIp, null);
    assert.equal(calls[1].port, null);
    assert.equal(local.port, "");
  });
});

test("confirmed receiver restart captures the exact saved authority and explicitly chosen background setting", async () => {
  const calls: Record<string, unknown>[] = [];
  await withContext(async (_name, args) => { calls.push(args); return deviceProjection({ revision: "4", generation: "8" }); }, async ({ context, local }) => {
    local.projection!.receiver.confirmed = true;
    editDeviceNetworkField(local, "keep_when_hidden", "", true);
    await setDeviceReceiver(context, true);
    assert.equal(calls.length, 1);
    assert.deepEqual(calls[0].target, { kind: "temp" });
    assert.equal(calls[0].confirmed, true);
    assert.equal(calls[0].keepWhenHidden, true);
    assert.equal(calls[0].startOnLaunch, false);
  });
});

test("selection uses both device and profile identity and refuses unauthorized automatic additions", async () => {
  const calls: Record<string, unknown>[] = [];
  await withContext(async (_name, args) => { calls.push(args); return deviceProjection(); }, async ({ context, local }) => {
    const peer = local.projection!.peers[0];
    peer.can_use = false;
    await selectDevicePeer(context, devicePeerKey(peer));
    assert.equal(calls.length, 0);
    peer.selected = true; local.projection!.enrollment = "disconnected";
    await selectDevicePeer(context, devicePeerKey(peer));
    assert.deepEqual(calls[0], { deviceId: "device-19", profileId: "receiver-19", enabled: false, expectedRevision: "3", expectedGeneration: "7" });
  });
});

test("peer switch keeps its saved checked state while saving and blocks repeated activation", async () => {
  const saved = deferred<DeviceNetworkProjection>();
  let calls = 0;
  await withContext(async () => { calls++; return saved.promise; }, async ({ context, local }) => {
    const key = devicePeerKey(local.projection!.peers[0]);
    const selection = selectDevicePeer(context, key);
    assert.equal(local.pending, "select");
    assert.equal(local.selectionKey, key);
    assert.equal(local.projection!.peers[0].selected, false);
    assert.match(renderDeviceNetwork(local), /role="switch"[^>]*aria-checked="false"[^>]*aria-busy="true"[^>]*disabled/);
    await selectDevicePeer(context, key);
    assert.equal(calls, 1);
    const next = deviceProjection({ revision: "4", generation: "8" }); next.peers[0].selected = true;
    saved.resolve(next); await selection;
    assert.equal(local.pending, null);
    assert.match(renderDeviceNetwork(local), /role="switch"[^>]*aria-checked="true"[^>]*aria-busy="false"/);
    assert.match(local.notice, /利用をONで保存/);
  });
});

test("failed peer save recovers the actual selection, preserves receiver drafts, and allows retry", async () => {
  await withContext(async name => {
    if (name === "device_network_select") throw "storage_error";
    return deviceProjection({ generation: "8" });
  }, async ({ context, local }) => {
    editDeviceNetworkField(local, "bind_ip", "192.168.2.22", false);
    const key = devicePeerKey(local.projection!.peers[0]);
    await selectDevicePeer(context, key);
    const html = renderDeviceNetwork(local);
    assert.equal(local.pending, null);
    assert.equal(local.projection!.peers[0].selected, false);
    assert.equal(local.bindIp, "192.168.2.22");
    const button = html.match(/<button[^>]*role="switch"[^>]*>/)![0];
    assert.match(button, /aria-checked="false"/);
    assert.doesNotMatch(button, /disabled/);
    assert.match(html, /利用先に選択していません。端末設定を保存できませんでした/);
  });
});

test("late job polling cannot overwrite exact cancellation and missing acknowledgement remains unconfirmed", async () => {
  const oldJobs = deferred<ReturnType<typeof deviceUiFixture>["jobs"]>();
  const calls: { name: string; args: Record<string, unknown> }[] = [];
  await withContext(async (name, args) => {
    calls.push({ name, args });
    if (name === "device_network_jobs") return oldJobs.promise;
    return { ...deviceUiFixture().jobs.outgoing[0], state: "unknown", stop_status: "unconfirmed", can_stop: true };
  }, async ({ context, local }) => {
    const poll = refreshDeviceNetworkJobs(context);
    await stopDeviceNetworkJob(context, "outgoing:reference-a");
    assert.equal(local.jobs.outgoing[0].stop_status, "unconfirmed");
    oldJobs.resolve(deviceUiFixture().jobs);
    await poll;
    assert.equal(local.jobs.outgoing[0].stop_status, "unconfirmed");
    assert.deepEqual(calls[1], { name: "device_network_cancel", args: { referenceId: "reference-a" } });
  });
});

test("incoming stop stays scoped to the managed receiver and retains route metadata", async () => {
  const calls: Record<string, unknown>[] = [];
  await withContext(async (_name, args) => {
    calls.push(args);
    const row = deviceUiFixture().jobs.incoming[0];
    return { ...row, state: "cancelling", network: null };
  }, async ({ context, local }) => {
    await stopDeviceNetworkJob(context, "incoming:job-00");
    assert.deepEqual(calls, [{ profileId: "receiver-00", jobId: "job-00" }]);
    assert.deepEqual(local.jobs.incoming[0].network?.device_path, ["device-19", "device-00"]);
    local.jobs.incoming[0].profile_id = "manual-profile";
    await stopDeviceNetworkJob(context, "incoming:job-00");
    assert.equal(calls.length, 1);
  });
});

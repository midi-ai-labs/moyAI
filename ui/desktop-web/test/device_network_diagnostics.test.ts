import assert from "node:assert/strict";
import test from "node:test";
import type { ActionContext } from "../src/actions.ts";
import { acceptDeviceNetworkProjection, devicePeerKey, editDeviceNetworkField } from "../src/device_network_state.ts";
import { deviceCanDiagnose, deviceDiagnosticKey, deviceLocalIpv4Choices, diagnoseDeviceNetwork, renderDeviceDiagnostic,
  type DeviceDiagnosticResult } from "../src/device_network_diagnostics.ts";
import { deviceProjection, deviceUiFixture } from "./device_network_fixture.ts";

function result(overrides: Partial<DeviceDiagnosticResult> = {}): DeviceDiagnosticResult {
  return { scope: "hub", device_id: null, profile_id: null, revision: "3", generation: "7", checked_at: "1788742800000",
    stages: [{ key: "tls", label: "TLS", status: "pass", detail: "本人確認済み", hint: null },
      { key: "firewall", label: "別端末からの到達", status: "skipped", detail: "この端末内だけでは未確認", hint: "Windowsのファイアウォールを確認してください。" }],
    local_ipv4: ["192.168.1.22"], ...overrides };
}
async function withContext(invoke: (name: string, args: Record<string, unknown>) => Promise<unknown>,
  run: (value: { context: ActionContext; local: ReturnType<typeof deviceUiFixture>; view: { overlay: string } }) => Promise<void>) {
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  const local = deviceUiFixture();
  const view = { overlay: "hub" };
  const context = { uiState: { deviceNetwork: local }, getViewState: () => view, rerender() {} } as unknown as ActionContext;
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke } } });
  try { await run({ context, local, view }); }
  finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
}

test("a peer diagnosis sends the exact selected target once, leaves selection unchanged and preserves skipped stages", async () => {
  const calls: { name: string; args: Record<string, unknown> }[] = [];
  await withContext(async (name, args) => { calls.push({ name, args }); return result({ scope: "peer", device_id: "device-19", profile_id: "receiver-19" }); },
    async ({ context, local }) => {
      const peer = local.projection!.peers[0];
      const key = devicePeerKey(peer);
      await diagnoseDeviceNetwork(context, "peer", key);
      assert.deepEqual(calls, [{ name: "device_network_diagnose", args: { scope: "peer", deviceId: "device-19", profileId: "receiver-19", expectedRevision: "3", expectedGeneration: "7" } }]);
      assert.equal(peer.selected, false);
      const html = renderDeviceDiagnostic(local, "peer", key);
      assert.match(html, /確認済み/);
      assert.match(html, /未実施/);
      assert.match(html, /ファイアウォール/);
      assert.doesNotMatch(html, /Invalid Date|すべて.*成功/);
      assert.deepEqual(deviceLocalIpv4Choices(local), ["192.168.1.22"]);
    });
});

test("an in-flight old diagnosis cannot survive a newer owner and never replaces receiver input", async () => {
  let release!: (value: DeviceDiagnosticResult) => void;
  const deferred = new Promise<DeviceDiagnosticResult>(resolve => { release = resolve; });
  let calls = 0;
  await withContext(async () => { ++calls; return deferred; }, async ({ context, local }) => {
    editDeviceNetworkField(local, "bind_ip", "10.1.1.22", false);
    const first = diagnoseDeviceNetwork(context, "hub");
    assert.equal(deviceCanDiagnose(local, "hub"), false);
    await diagnoseDeviceNetwork(context, "hub");
    assert.equal(calls, 1);
    acceptDeviceNetworkProjection(local, deviceProjection({ generation: "8" }));
    release(result());
    await first;
    assert.deepEqual(local.diagnostics, {});
    assert.equal(local.bindIp, "10.1.1.22");
    assert.equal(local.dirty, true);
    assert.equal(local.diagnosticPending, null);
  });
});

test("diagnosis rejects mismatched identity and invalidates previous facts when settings change", async () => {
  await withContext(async () => result({ device_id: "unexpected-device" }), async ({ context, local }) => {
    await diagnoseDeviceNetwork(context, "hub");
    assert.deepEqual(local.diagnostics, {});
    assert.match(local.diagnosticErrors[deviceDiagnosticKey("hub")], /診断対象/);
    local.diagnostics[deviceDiagnosticKey("receiver")] = result({ scope: "receiver" });
    acceptDeviceNetworkProjection(local, deviceProjection({ revision: "4", generation: "8" }));
    assert.deepEqual(local.diagnostics, {});
    assert.deepEqual(local.diagnosticErrors, {});
    assert.deepEqual(deviceLocalIpv4Choices(local), []);
  });
});

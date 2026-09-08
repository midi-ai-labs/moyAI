import { command } from "./api.ts";
import type { ActionContext } from "./actions.ts";
import { deviceNetworkError, deviceNetworkTarget, devicePeerKey, type DeviceNetworkPresentation } from "./device_network_state.ts";
import { escapeHtml } from "./utils.ts";

export type DeviceDiagnosticScope = "hub" | "receiver" | "peer";
export interface DeviceDiagnosticResult {
  scope: DeviceDiagnosticScope;
  device_id: string | null;
  profile_id: string | null;
  revision: string;
  generation: string;
  checked_at: string;
  stages: { key: string; label: string; status: "pass" | "fail" | "skipped"; detail: string; hint: string | null }[];
  local_ipv4: string[];
}
export function deviceDiagnosticKey(scope: DeviceDiagnosticScope, peerKey = ""): string {
  return JSON.stringify([scope, scope === "peer" ? peerKey : ""]);
}
export function deviceCanDiagnose(local: DeviceNetworkPresentation, scope: DeviceDiagnosticScope, peerKey = ""): boolean {
  if (local.pending || local.diagnosticPending || !local.projection) return false;
  if (scope === "hub") return Boolean(local.projection.hub_url);
  if (scope === "receiver") return Boolean(local.projection.device_id);
  return local.projection.peers.some(peer => devicePeerKey(peer) === peerKey);
}
export async function diagnoseDeviceNetwork(context: ActionContext, scope: DeviceDiagnosticScope, peerKey = ""): Promise<void> {
  const local = context.uiState.deviceNetwork;
  if (context.getViewState()?.overlay !== "hub" || !deviceCanDiagnose(local, scope, peerKey)) return;
  const projection = local.projection!;
  const peer = scope === "peer" ? projection.peers.find(candidate => devicePeerKey(candidate) === peerKey)! : null;
  const target = deviceNetworkTarget(projection);
  const key = deviceDiagnosticKey(scope, peerKey);
  const serial = ++local.diagnosticSerial;
  local.diagnosticPending = key;
  delete local.diagnosticErrors[key];
  context.rerender();
  const current = () => serial === local.diagnosticSerial && context.getViewState()?.overlay === "hub"
    && local.projection?.revision === target.expectedRevision && local.projection.generation === target.expectedGeneration
    && (!peer || local.projection.peers.some(candidate => devicePeerKey(candidate) === peerKey));
  try {
    const result = await command<DeviceDiagnosticResult>("device_network_diagnose", { scope,
      ...(peer ? { deviceId: peer.device_id, profileId: peer.profile_id } : {}), ...target });
    if (!current()) return;
    if (result.scope !== scope || result.revision !== target.expectedRevision || result.generation !== target.expectedGeneration
      || (peer ? result.device_id !== peer.device_id || result.profile_id !== peer.profile_id
        : result.device_id !== null || result.profile_id !== null)) {
      local.diagnosticErrors[key] = "診断対象の状態が変わりました。最新情報を取得して、もう一度診断してください。";
      return;
    }
    local.diagnostics[key] = result;
  } catch (error) { if (current()) local.diagnosticErrors[key] = deviceNetworkError(error); }
  finally {
    if (serial === local.diagnosticSerial) { local.diagnosticPending = null; context.rerender(); }
  }
}

export function renderDeviceDiagnostic(local: DeviceNetworkPresentation, scope: DeviceDiagnosticScope, peerKey = ""): string {
  const key = deviceDiagnosticKey(scope, peerKey);
  const result = local.diagnostics[key];
  const pending = local.diagnosticPending === key;
  const error = local.diagnosticErrors[key];
  const labels = { pass: "確認済み", fail: "確認できません", skipped: "未実施" };
  return `<div class="device-network-diagnostic" data-settings-passive="device-network-diagnostic-${escapeHtml(key)}" role="status" aria-live="polite">
    ${pending ? '<p class="hub-help">接続を診断しています…</p>' : ""}${error ? `<p class="hub-help warning">${escapeHtml(error)}</p>` : ""}
    ${result ? `<p class="hub-help">診断日時: ${escapeHtml(new Date(Number(result.checked_at)).toLocaleString("ja-JP"))} · この時点の確認結果です。</p><ol class="device-network-diagnostic-stages">${result.stages.map(stage => `<li data-status="${stage.status}"><strong>${escapeHtml(stage.label)} <span>${labels[stage.status]}</span></strong><p>${escapeHtml(stage.detail)}</p>${stage.hint ? `<p class="device-network-diagnostic-hint">${escapeHtml(stage.hint)}</p>` : ""}</li>`).join("")}</ol>` : !pending && !error ? '<p class="hub-help">まだ診断していません。診断は設定や利用先の選択を変更しません。</p>' : ""}
  </div>`;
}

export function deviceLocalIpv4Choices(local: DeviceNetworkPresentation): string[] {
  return [...new Set(Object.values(local.diagnostics).flatMap(result => result.local_ipv4))];
}

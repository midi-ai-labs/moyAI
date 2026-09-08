import { command } from "./api.ts";
import type { ActionContext } from "./actions.ts";
import { deviceNetworkError, type DeviceNetworkPresentation, type DeviceOutgoingJob } from "./device_network_state.ts";
import { escapeHtml } from "./utils.ts";

export interface RemoteArtifactManifest {
  job_id: string;
  version: string;
  files: { path: string; kind: "add" | "update" | "delete" | "move"; from_path: string | null;
    base_sha256: string | null; sha256: string | null; byte_length: number }[];
}
export interface DeviceArtifactReceipt { reference_id: string; job_id: string; version: string; directory: string }
export interface DeviceArtifactView {
  deviceId: string;
  manifest: RemoteArtifactManifest;
  receipt: DeviceArtifactReceipt | null;
}

function job(local: DeviceNetworkPresentation, referenceId: string): DeviceOutgoingJob | undefined {
  return local.jobs.outgoing.find(row => row.reference_id === referenceId);
}
export function deviceCanInspectArtifacts(local: DeviceNetworkPresentation, referenceId: string): boolean {
  const target = job(local, referenceId);
  return !local.pending && !local.artifactPending && Boolean(local.projection?.device_id && target?.job_id
    && ["completed", "failed", "interrupted"].includes(target.state));
}
function currentArtifact(local: DeviceNetworkPresentation, referenceId: string): DeviceArtifactView | null {
  const entry = local.artifacts[referenceId];
  return entry && entry.deviceId === local.projection?.device_id && entry.manifest.job_id === job(local, referenceId)?.job_id ? entry : null;
}
export function deviceCanExportArtifacts(local: DeviceNetworkPresentation, referenceId: string): boolean {
  return deviceCanInspectArtifacts(local, referenceId) && Boolean(currentArtifact(local, referenceId)?.manifest.files.length);
}
export async function inspectDeviceArtifacts(context: ActionContext, referenceId: string): Promise<void> {
  await operateArtifact(context, referenceId, false);
}
export async function exportDeviceArtifacts(context: ActionContext, referenceId: string): Promise<void> {
  await operateArtifact(context, referenceId, true);
}
async function operateArtifact(context: ActionContext, referenceId: string, exporting: boolean): Promise<void> {
  const local = context.uiState.deviceNetwork;
  if (context.getViewState()?.overlay !== "hub" || !(exporting ? deviceCanExportArtifacts(local, referenceId) : deviceCanInspectArtifacts(local, referenceId))) return;
  const target = { ...job(local, referenceId)! };
  const deviceId = local.projection!.device_id!;
  const version = exporting ? currentArtifact(local, referenceId)!.manifest.version : null;
  const serial = ++local.artifactSerial;
  local.artifactPending = { referenceId, operation: exporting ? "export" : "inspect" };
  delete local.artifactErrors[referenceId];
  delete local.artifactNotices[referenceId];
  context.rerender();
  const current = () => serial === local.artifactSerial && context.getViewState()?.overlay === "hub"
    && local.projection?.device_id === deviceId && job(local, referenceId)?.job_id === target.job_id
    && job(local, referenceId)?.device_id === target.device_id && job(local, referenceId)?.profile_id === target.profile_id;
  try {
    if (exporting) {
      const receipt = await command<DeviceArtifactReceipt | null>("device_network_export_artifacts", { referenceId, version });
      if (!current()) return;
      if (!receipt) { local.artifactNotices[referenceId] = "書き出しをキャンセルしました。成果物の確認内容は保持しています。"; return; }
      if (receipt.reference_id !== referenceId || receipt.job_id !== target.job_id || receipt.version !== version) throw new Error("artifact_identity_mismatch");
      currentArtifact(local, referenceId)!.receipt = receipt;
      local.artifactNotices[referenceId] = "選択した場所に新しいフォルダを作成して書き出しました。元のプロジェクトには適用していません。";
    } else {
      const response = await command<{ reference_id: string; manifest: RemoteArtifactManifest }>("device_network_artifacts", { referenceId });
      if (!current()) return;
      if (response.reference_id !== referenceId || response.manifest.job_id !== target.job_id) throw new Error("artifact_identity_mismatch");
      local.artifacts[referenceId] = { deviceId, manifest: response.manifest, receipt: null };
    }
  } catch (error) { if (current()) local.artifactErrors[referenceId] = typeof error === "string" && ["authority_retired", "recovery_required", "artifacts_unavailable", "policy_denied", "device_revoked"].includes(error)
    ? deviceNetworkError(error) : exporting
    ? "書き出せませんでした。成果物の版・接続状態と保存先を確認してください。既存の同名フォルダへは上書きしません。"
    : "成果物を取得できませんでした。相手の受付状態と、このタスクの接続許可を確認してください。"; }
  finally { if (serial === local.artifactSerial) { local.artifactPending = null; context.rerender(); } }
}

export function renderDeviceArtifacts(local: DeviceNetworkPresentation, referenceId: string): string {
  const target = job(local, referenceId);
  if (!target?.job_id || !["completed", "failed", "interrupted"].includes(target.state)) return "";
  const entry = currentArtifact(local, referenceId);
  const pending = local.artifactPending?.referenceId === referenceId ? local.artifactPending.operation : null;
  const labels = { add: "追加", update: "更新", delete: "削除記録", move: "移動" };
  return `<details class="device-network-artifacts" data-details-key="device-artifacts-${escapeHtml(referenceId)}"><summary>成果物の確認と書き出し</summary>
    <p class="hub-help">この依頼で記録されたファイル変更だけを、版を指定して取得します。シェルで作成した任意のファイルやプロジェクト全体を自動同期しません。</p>
    <button id="device-network-artifacts-${encodeURIComponent(referenceId)}" data-action="device-network-artifacts" data-value="${escapeHtml(referenceId)}" ${deviceCanInspectArtifacts(local, referenceId) ? "" : "disabled"}>成果物を確認</button>
    <div data-settings-passive="device-artifact-result-${escapeHtml(referenceId)}" role="status" aria-live="polite">
      ${pending ? `<p class="hub-help">${pending === "inspect" ? "成果物を確認しています…" : "選択した版を書き出しています…"}</p>` : ""}
      ${local.artifactErrors[referenceId] ? `<p class="hub-help warning">${escapeHtml(local.artifactErrors[referenceId])}</p>` : ""}
      ${local.artifactNotices[referenceId] ? `<p class="hub-help">${escapeHtml(local.artifactNotices[referenceId])}</p>` : ""}
      ${entry ? `<p class="hub-help">確認した版: <code>${escapeHtml(entry.manifest.version)}</code></p>${entry.manifest.files.length
        ? `<ul class="device-network-artifact-files">${entry.manifest.files.map(file => `<li><strong>${labels[file.kind]} · ${escapeHtml(file.path)}</strong>${file.from_path ? `<span>移動元: ${escapeHtml(file.from_path)}</span>` : ""}<span>${file.byte_length.toLocaleString("ja-JP")} bytes</span></li>`).join("")}</ul>`
        : '<p class="hub-help">この版に書き出せるファイル変更はありません。</p>'}
      ${entry.receipt ? `<p class="hub-help">保存先: <code>${escapeHtml(entry.receipt.directory)}</code></p>` : ""}` : ""}
    </div>
    <p class="hub-help">選択した場所に新しいフォルダを作成します。既存のプロジェクトへの自動適用や削除は行いません。</p>
    <button id="device-network-export-artifacts-${encodeURIComponent(referenceId)}" data-action="device-network-export-artifacts" data-value="${escapeHtml(referenceId)}" ${deviceCanExportArtifacts(local, referenceId) ? "" : "disabled"}>保存先を選んで書き出す</button>
  </details>`;
}

import { escapeHtml } from "./utils.ts";
import { renderDeviceArtifacts } from "./device_network_artifacts.ts";
import { deviceCanStopJob, type DeviceNetworkPresentation } from "./device_network_state.ts";

const stateLabels: Record<string, string> = { preparing: "受付を確認中", accepted: "受付済み", running: "実行中", awaiting_approval: "受入端末で承認待ち", cancelling: "停止処理中", completed: "完了", failed: "失敗", interrupted: "中断", unknown: "状態未確認" };
const stopLabels = { none: "", requested: "停止要求済み", unconfirmed: "停止完了は未確認", confirmed: "停止確認済み" };
export function devicePathLabel(local: DeviceNetworkPresentation, path: string[]): string {
  return path.map(id => id === local.projection?.device_id ? local.projection.display_name || id
    : local.projection?.peers.find(peer => peer.device_id === id)?.display_name || id).join(" → ");
}
export function renderDeviceNetworkJobs(local: DeviceNetworkPresentation): string {
  const outgoing = local.jobs.outgoing.map(job => ({ key: `outgoing:${job.reference_id}`, title: "この端末からの依頼",
    path: job.device_path, state: job.state, stop: stopLabels[job.stop_status], result: job.result, root: job.root_task_id, referenceId: job.reference_id }));
  const incoming = local.jobs.incoming.filter(job => job.profile_id === local.projection?.receiver.profile_id).map(job => ({ key: `incoming:${job.job_id}`,
    title: job.prompt_preview || "この端末で受け付けた依頼", path: job.network?.device_path ?? [], state: job.state,
    stop: job.state === "cancelling" ? "停止処理中" : job.state === "interrupted" ? "中断を確認" : "", result: job.result,
    root: job.network?.root_task_id ?? job.parent.task_id, referenceId: null }));
  return `<section class="device-network-card" aria-labelledby="device-network-jobs-title"><h3 id="device-network-jobs-title">委任経路と停止状況</h3>
    <p class="hub-help">矢印は実際に委任された経路です。停止要求と停止確認を分けて表示します。相手が到達不能な場合、停止完了を確認できるまで未確認として扱います。</p>
    <p class="hub-help" role="status" data-settings-passive="device-network-jobs-error">${escapeHtml(local.jobsError)}</p>
    <div id="device-network-jobs-list">${[...outgoing, ...incoming].map(job => `<article class="device-network-job" data-network-job-id="${escapeHtml(job.key)}">
      <div data-settings-passive="device-job-status-${escapeHtml(job.key)}"><strong>${escapeHtml(job.title)}</strong><p class="device-network-path">${escapeHtml(devicePathLabel(local, job.path) || "経路情報なし")}</p><p><span class="device-network-status">${escapeHtml(stateLabels[job.state] ?? "状態未確認")}</span>${job.stop ? ` · <span class="device-network-status ${job.stop.includes("未確認") ? "warning" : ""}">${escapeHtml(job.stop)}</span>` : ""}</p></div>
      <details data-details-key="device-job-${escapeHtml(job.key)}"><summary>結果と識別情報</summary><div data-settings-passive="device-job-result-${escapeHtml(job.key)}"><p class="hub-help">${escapeHtml(job.result ?? "結果はまだありません。")}</p><code>依頼元タスク: ${escapeHtml(job.root)}</code></div></details>
      ${job.referenceId ? renderDeviceArtifacts(local, job.referenceId) : ""}
      <button id="device-network-stop-${encodeURIComponent(job.key)}" data-action="device-network-stop-job" data-value="${escapeHtml(job.key)}" ${deviceCanStopJob(local, job.key) ? "" : "disabled"}>このタスクを停止</button>
    </article>`).join("")}</div><p id="device-network-jobs-empty" class="hub-help" data-settings-passive="device-network-jobs-empty" ${outgoing.length + incoming.length ? "hidden" : ""}>委任されたタスクはまだありません。</p>
  </section>`;
}

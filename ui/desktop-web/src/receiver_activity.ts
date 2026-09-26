import { command } from "./api.ts";
import type { ActionContext } from "./actions.ts";
import type { DeviceNetworkPresentation } from "./device_network_state.ts";
import type { SharedWorkPresentation } from "./shared_work_state.ts";
import { escapeHtml } from "./utils.ts";

export interface ReceiverAttempt {
  attempt_id: string;
  generation: string;
  job_id: string;
  project_id: string;
  environment_id: string;
  run_id: string;
  state: "preparing" | "executing" | "report_pending" | "unknown";
  local_state: "starting" | "running" | "waiting_approval" | "stopping" | "draining" | "processes_running" | "settled" | "unknown" | null;
}
export interface ReceiverService {
  service_id: string;
  attempt_id: string;
  generation: string;
  project_id: string;
  conversation_id: string;
  environment_id: string;
  expires_at_ms: number;
  local_state: "running" | "stopping" | "stopped" | "unknown";
  uncertain: boolean;
}
export interface ReceiverActivityProjection {
  runner_id: string | null;
  attempts: ReceiverAttempt[];
  retained_services: ReceiverService[];
  observed_at_ms: number | null;
  unavailable: boolean;
}

export function receiverAttemptKey(attempt: ReceiverAttempt): string {
  return `${attempt.attempt_id}/${attempt.generation}/${attempt.run_id}`;
}
export function receiverServiceKey(service: ReceiverService): string {
  return `${service.service_id}/${service.attempt_id}/${service.generation}`;
}

/** The Runner keeps this PC's shared execution slot until the work or app drains. */
export function receiverBlocksLocalSend(local: DeviceNetworkPresentation): boolean {
  const activity = local.receiverActivity;
  return Boolean(activity && (activity.attempts.length > 0
    || activity.retained_services.some(service => service.local_state !== "stopped")
    || (activity.unavailable && Boolean(local.execution?.directory))));
}

export function receiverStopEnabled(local: DeviceNetworkPresentation, key: string): boolean {
  const activity = local.receiverActivity;
  return Boolean(!local.receiverPending && !activity?.unavailable && activity?.runner_id
    && activity.attempts.some(attempt => receiverAttemptKey(attempt) === key && attempt.state === "executing"
      && attempt.local_state !== "stopping" && attempt.local_state !== "draining" && attempt.local_state !== "settled"));
}
export function receiverServiceStopEnabled(local: DeviceNetworkPresentation, key: string): boolean {
  const activity = local.receiverActivity;
  return Boolean(!local.receiverPending && !activity?.unavailable && activity?.runner_id
    && activity.retained_services.some(service => receiverServiceKey(service) === key && service.local_state === "running" && !service.uncertain));
}

export async function refreshReceiverActivity(context: ActionContext): Promise<void> {
  const local = context.uiState.deviceNetwork;
  if (local.receiverPending) return;
  const serial = ++local.receiverSerial;
  try {
    const projection = await command<ReceiverActivityProjection>("receiver_activity_projection");
    if (serial !== local.receiverSerial) return;
    local.receiverActivity = projection;
    local.receiverError = "";
    context.rerender();
  } catch {
    if (serial === local.receiverSerial) {
      if (local.receiverActivity) local.receiverActivity = { ...local.receiverActivity, unavailable: true };
      local.receiverError = "このPCの実行状況を確認できません。";
      context.rerender();
    }
  }
}

export async function stopReceiverService(context: ActionContext, key: string): Promise<void> {
  const local = context.uiState.deviceNetwork;
  if (!receiverServiceStopEnabled(local, key)) return;
  const activity = local.receiverActivity!;
  const service = activity.retained_services.find(row => receiverServiceKey(row) === key)!;
  const serial = ++local.receiverSerial;
  local.receiverPending = key;
  local.receiverError = "";
  context.rerender();
  try {
    const projection = await command<ReceiverActivityProjection>("receiver_service_stop", {
      target: { runner_id: activity.runner_id, service_id: service.service_id,
        attempt_id: service.attempt_id, generation: service.generation },
    });
    if (serial === local.receiverSerial) local.receiverActivity = projection;
  } catch {
    if (serial === local.receiverSerial) local.receiverError = "停止対象のアプリを確認できませんでした。現在の状態を確認してください。";
  } finally {
    if (serial === local.receiverSerial) { local.receiverPending = null; context.rerender(); }
  }
}

export async function stopReceiverAttempt(context: ActionContext, key: string): Promise<void> {
  const local = context.uiState.deviceNetwork;
  if (!receiverStopEnabled(local, key)) return;
  const activity = local.receiverActivity!;
  const attempt = activity.attempts.find(row => receiverAttemptKey(row) === key)!;
  const serial = ++local.receiverSerial;
  local.receiverPending = key;
  local.receiverError = "";
  context.rerender();
  try {
    const projection = await command<ReceiverActivityProjection>("receiver_activity_stop", {
      target: { runner_id: activity.runner_id, attempt_id: attempt.attempt_id,
        generation: attempt.generation, run_id: attempt.run_id },
    });
    if (serial === local.receiverSerial) local.receiverActivity = projection;
  } catch {
    if (serial === local.receiverSerial) local.receiverError = "停止対象を確認できませんでした。現在の状態を確認してください。";
  } finally {
    if (serial === local.receiverSerial) { local.receiverPending = null; context.rerender(); }
  }
}

function attemptLabel(attempt: ReceiverAttempt): string {
  if (attempt.state === "unknown") return "停止を確認できない仕事があります";
  if (attempt.state === "report_pending") return "実行結果をHubへ報告中です";
  if (attempt.local_state === "waiting_approval") return "このPCの仕事は承認待ちです";
  if (attempt.local_state === "stopping" || attempt.local_state === "draining") return "このPCの仕事を停止中です";
  if (attempt.state === "preparing") return "このPCで仕事を準備しています";
  return "このPCで仕事を実行中です";
}

/** Runner observations are device-wide; project details appear only if Hub granted this viewer access. */
export function renderReceiverActivity(local: DeviceNetworkPresentation, shared?: SharedWorkPresentation): string {
  const activity = local.receiverActivity;
  if (!activity || (!activity.attempts.length && !activity.retained_services.length
    && !(activity.unavailable && local.execution?.directory))) return "";
  const esc = escapeHtml;
  const jobs = shared?.conceal ? [] : shared?.projection?.status?.jobs ?? [];
  const stoppedOnly = !activity.attempts.length && activity.retained_services.every(service => service.local_state === "stopped" && !service.uncertain);
  return `<section class="run-strip receiver-activity" role="status" aria-label="このPCの使用状況">
    <strong>${activity.unavailable ? "このPCの実行状態を確認できません" : stoppedOnly ? "このPCの停止結果を報告中" : "このPCは使用中"}</strong>
    <div class="receiver-activity-items">${activity.attempts.map(attempt => {
      const visible = jobs.find(job => job.id === attempt.job_id && shared?.projection?.status?.project_id === attempt.project_id);
      const detail = visible ? `${visible.requestor.display_name}からの依頼 · ${visible.title}` : "詳細を表示できない仕事";
      const stop = receiverStopEnabled(local, receiverAttemptKey(attempt));
      return `<div class="receiver-activity-item"><span>${esc(attemptLabel(attempt))} · ${esc(detail)}</span>${stop ? `<button class="run-stop-button danger" data-action="receiver-stop" data-value="${esc(receiverAttemptKey(attempt))}" aria-label="このPCの受信作業を停止">停止</button>` : ""}</div>`;
    }).join("")}${activity.retained_services.map(service => {
      const visible = shared?.conceal ? null : shared?.projection?.detail;
      const detail = visible?.project_id === service.project_id && visible.conversation_id === service.conversation_id
        ? visible.title : "詳細を表示できないアプリ";
      const state = service.uncertain || service.local_state === "unknown" ? "稼働状態を確認できません"
        : service.local_state === "stopping" ? "停止を確認中"
        : service.local_state === "stopped" ? "停止済み・Hubへの報告待ち" : "このPCでアプリを起動中";
      const stop = receiverServiceStopEnabled(local, receiverServiceKey(service));
      return `<div class="receiver-activity-item"><span>${esc(state)} · ${esc(detail)} · 保持期限 ${esc(new Date(service.expires_at_ms).toLocaleString("ja-JP"))}</span>${stop ? `<button class="run-stop-button danger" data-action="receiver-service-stop" data-value="${esc(receiverServiceKey(service))}" aria-label="このPCのアプリを停止">停止</button>` : ""}</div>`;
    }).join("")}</div>
    <small>同じ実行枠を使う新しい仕事は待機または拒否されます。入力や閲覧は続けられます。</small>
    ${activity.unavailable ? `<small>最終確認: ${activity.observed_at_ms === null ? "未確認" : esc(new Date(activity.observed_at_ms).toLocaleString("ja-JP"))}</small>` : ""}
    ${local.receiverError ? `<small class="shared-error">${esc(local.receiverError)}</small>` : ""}
  </section>`;
}

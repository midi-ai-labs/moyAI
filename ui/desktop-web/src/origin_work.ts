import { command } from "./api.ts";
import type { ActionContext } from "./actions.ts";
import type { DeviceNetworkPresentation, DeviceNetworkUiState } from "./device_network_state.ts";
import type { DraftActionTarget } from "./types.ts";
import { escapeHtml } from "./utils.ts";

interface OriginJob {
  project_id: string;
  job: { id: string; conversation_id?: string | null; title: string; state: string;
    environment_label: string; device_label?: string | null; updated_at_ms: number };
  artifacts?: Array<{ id: string; name: string; byte_length: number }>;
  more_artifacts?: boolean;
}
interface OriginService {
  project_id: string;
  service: { service_id: string; conversation_id: string; environment_id: string;
    expires_at_ms: number; stop_requested: boolean; uncertain: boolean; can_stop: boolean };
}
export interface OriginWorkProjection {
  origin_session_ref: string;
  jobs: OriginJob[];
  retained_services: OriginService[];
  hidden_active_work?: boolean;
  observed_at_ms: number;
  admission_revision: string;
  stop_pending?: boolean;
  stop_error?: string | null;
}

function ownerKey(target: DraftActionTarget | null, local: DeviceNetworkUiState | DeviceNetworkPresentation): string | null {
  if (!target?.sessionId || !local.projection?.hub_url || !local.projection.device_id) return null;
  return JSON.stringify([target.workspacePath, target.sessionId, target.ownerGeneration,
    local.projection.hub_url, local.projection.device_id]);
}

function currentOwner(context: ActionContext): string | null {
  const state = context.getProjection();
  if (!state || state.hub_project_open) return null;
  return ownerKey(state.draft_target, context.uiState.deviceNetwork);
}

export function originAppsStopEnabled(local: DeviceNetworkPresentation): boolean {
  const rows = local.originWork?.retained_services ?? [];
  return Boolean(local.originOwner && !local.originPending && !local.originStopPending && !local.originError
    && local.projection?.enrollment === "active" && rows.some(row => !row.service.stop_requested)
    && rows.every(row => row.service.can_stop && !row.service.uncertain));
}

export function originAllStopEnabled(local: DeviceNetworkPresentation): boolean {
  const work = local.originWork;
  const active = work?.jobs.some(row => ["queued", "offered", "running", "waiting_child", "awaiting_approval", "cancelling", "unknown"].includes(row.job.state))
    || work?.retained_services.some(row => !row.service.stop_requested);
  return Boolean(local.originOwner && work && !local.originPending && !local.originStopPending
    && !work.stop_pending && /^[1-9][0-9]*$/.test(work.admission_revision) && active);
}

export async function refreshOriginWork(context: ActionContext): Promise<void> {
  const local = context.uiState.deviceNetwork;
  const owner = currentOwner(context);
  if (owner !== local.originOwner) {
    ++local.originSerial;
    local.originOwner = owner;
    local.originWork = null;
    local.originPending = false;
    local.originStopPending = false;
    local.originError = "";
    local.originLastFetchMs = 0;
  }
  if (!owner || local.originPending || local.originStopPending) return;
  if (local.projection?.enrollment !== "active") {
    if (local.originWork) {
      local.originError = "Hubに接続できないため、この会話の別PCの状態を確認できません。";
      context.rerender();
    }
    return;
  }
  if (Date.now() - local.originLastFetchMs < 5000) return;
  const target = context.getProjection()?.draft_target;
  if (!target?.sessionId) return;
  const serial = ++local.originSerial;
  local.originPending = true;
  local.originLastFetchMs = Date.now();
  try {
    const projection = await command<OriginWorkProjection>("origin_work_projection", { expectedTarget: target });
    if (serial !== local.originSerial || currentOwner(context) !== owner) return;
    if (projection.origin_session_ref !== target.sessionId) throw new Error("chat origin changed");
    local.originWork = projection;
    local.originError = "";
  } catch {
    if (serial === local.originSerial && currentOwner(context) === owner) {
      local.originError = "別PCの仕事とアプリの状態を確認できません。Hubへの接続を確認してください。";
    }
  } finally {
    if (serial === local.originSerial) { local.originPending = false; context.rerender(); }
  }
}

export async function stopOriginApps(context: ActionContext): Promise<void> {
  const local = context.uiState.deviceNetwork;
  if (!originAppsStopEnabled(local)) return;
  const owner = local.originOwner;
  const target = context.getProjection()?.draft_target;
  if (!owner || !target?.sessionId || currentOwner(context) !== owner) return;
  const expectedServiceIds = local.originWork!.retained_services.map(row => row.service.service_id);
  const serial = ++local.originSerial;
  local.originStopPending = true;
  local.originError = "";
  context.rerender();
  try {
    const projection = await command<OriginWorkProjection>("origin_work_stop_apps", {
      expectedTarget: target, expectedServiceIds,
    });
    if (serial !== local.originSerial || currentOwner(context) !== owner) return;
    local.originWork = projection;
    local.originLastFetchMs = Date.now();
  } catch {
    if (serial === local.originSerial && currentOwner(context) === owner) {
      local.originError = "アプリ停止の結果を確認できません。Hubの状態を更新してから、再度操作してください。";
      local.originLastFetchMs = 0;
    }
  } finally {
    if (serial === local.originSerial) { local.originStopPending = false; context.rerender(); }
  }
}

export async function stopOriginAll(context: ActionContext): Promise<void> {
  const local = context.uiState.deviceNetwork;
  const state = context.getProjection();
  if (!state || !originAllStopEnabled(local)) return;
  const owner = local.originOwner;
  const target = state.draft_target;
  const revision = local.originWork?.admission_revision;
  if (!owner || !target?.sessionId || !revision || currentOwner(context) !== owner) return;
  const serial = ++local.originSerial;
  local.originStopPending = true;
  local.originError = "";
  context.rerender();
  try {
    const projection = await command<OriginWorkProjection>("origin_work_stop_all", {
      expectedTarget: target,
      expectedAdmissionRevision: revision,
      expectedStopTarget: state.can_cancel_run ? state.stop_target : null,
    });
    if (serial !== local.originSerial || currentOwner(context) !== owner) return;
    local.originWork = projection;
    local.originLastFetchMs = Date.now();
  } catch {
    if (serial === local.originSerial && currentOwner(context) === owner) {
      local.originError = "停止受付の結果を確認できません。状態を更新して確認してください。";
      local.originLastFetchMs = 0;
    }
  } finally {
    if (serial === local.originSerial) { local.originStopPending = false; context.rerender(); }
  }
}

const jobState = (state: string): string => ({ queued: "待機中", offered: "引渡し中", running: "実行中",
  awaiting_approval: "承認待ち", cancelling: "停止確認中", succeeded: "完了", failed: "失敗", cancelled: "取消済み", unknown: "状態不明" }[state] ?? "状態不明");

export function renderOriginWork(local: DeviceNetworkPresentation, sessionId: string | null, currentTurnCanStop = false): string {
  const work = local.originWork;
  if (!sessionId || !work || work.origin_session_ref !== sessionId || !local.originOwner
    || (!work.jobs.length && !work.retained_services.length && !work.hidden_active_work && !work.stop_pending && !work.stop_error)) return "";
  const esc = escapeHtml;
  const latestJobs = [...work.jobs].sort((a, b) => b.job.updated_at_ms - a.job.updated_at_ms).slice(0, 4);
  const canStop = originAppsStopEnabled(local);
  const canStopAll = originAllStopEnabled(local);
  return `<section class="run-strip origin-work" role="status" aria-label="この会話で別PCに依頼した仕事">
    <strong>この会話で使った別のPC</strong>
    <div class="origin-work-rows">${latestJobs.map(row => `<div class="origin-work-row"><span>${esc(row.job.device_label ?? row.job.environment_label)} · ${esc(row.job.title)} · ${esc(jobState(row.job.state))}</span>${(row.artifacts ?? []).map(asset => `<small>成果: ${esc(asset.name)} · ID ${esc(asset.id)} · ${esc(String(asset.byte_length))} bytes</small>`).join("")}${row.more_artifacts ? '<small>ほかの成果はHubのプロジェクトで確認できます</small>' : ""}</div>`).join("")}
    ${work.jobs.length > latestJobs.length ? `<small>ほか ${work.jobs.length - latestJobs.length} 件の仕事</small>` : ""}
    ${work.retained_services.map(row => `<div class="origin-work-row"><span>起動中のアプリ · ${esc(row.service.environment_id)} · ${row.service.uncertain ? "状態不明" : row.service.stop_requested ? "停止確認中" : `保持期限 ${esc(new Date(row.service.expires_at_ms).toLocaleString("ja-JP"))}`}</span></div>`).join("")}
    ${work.hidden_active_work ? '<small>現在の権限では詳細を表示できない別PCの仕事があります。Hub管理者に利用権限を確認してください。</small>' : ""}
    ${work.stop_pending ? '<small>この会話から依頼した実行の停止受付をHubで確認中です。</small>' : ""}
    ${work.stop_error ? '<small class="shared-error">停止受付をHubで確認できません。接続が戻ると再確認します。</small>' : ""}
    ${local.originError ? `<small class="shared-error">${esc(local.originError)}（最終確認: ${esc(new Date(work.observed_at_ms).toLocaleString("ja-JP"))}）</small>` : ""}</div>
    ${canStopAll ? `<button class="run-stop-button danger" data-action="origin-stop-all" aria-label="この会話の実行をすべて停止">この会話の実行をすべて停止</button><small>${currentTurnCanStop ? "現在の回答と" : ""}別PCの仕事、起動中のアプリが対象です。Hub受付後も停止を確認してください。</small>` : ""}
    ${canStop ? '<button class="run-stop-button danger" data-action="origin-stop-apps" aria-label="この会話の起動中のアプリを停止">起動中のアプリを停止</button>' : ""}
    ${latestJobs.some(row => row.artifacts?.length) ? '<small>成果ファイルの保存はこの会話で依頼できます。保存時にHubが権限を再確認します。</small>' : ""}
    ${work.retained_services.length ? '<small>この操作は起動中のアプリを停止します。現在の回答や実行中の仕事は別に停止してください。</small>' : ""}
  </section>`;
}

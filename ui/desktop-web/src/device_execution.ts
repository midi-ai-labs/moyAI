import { command } from "./api.ts";
import type { ActionContext } from "./actions.ts";
import { executionRecoveryKey, resetExecutionRecovery, type DeviceAccessMode, type DeviceNetworkPresentation } from "./device_network_state.ts";
import { escapeHtml } from "./utils.ts";

export interface DeviceExecutionProjection {
  revision: string;
  state: "unconnected" | "not_selected" | "needs_setup" | "starting" | "ready" | "paused" | "unavailable";
  projects: { id: string; label: string; can_control: boolean; can_execute: boolean; environment_id: string | null; preparation_state: "not_selected" | "waiting_setup" | "pending" | "ready" | "failed"; error: string | null }[];
  review: { id: string; directory: string; access_mode: DeviceAccessMode } | null;
  directory: string | null; access_mode: DeviceAccessMode | null; accepting: boolean; can_pause: boolean; can_resume: boolean; error: string | null;
  unknown_attempts: { attempt_id: string; generation: number; job_id: string; environment_id: string; run_id: string; state: string }[];
}
function acceptExecution(local: DeviceNetworkPresentation, projection: DeviceExecutionProjection): void {
  if (local.execution && BigInt(projection.revision) < BigInt(local.execution.revision)) return;
  if (local.execution?.review?.id !== projection.review?.id
    || !projection.unknown_attempts.some(row => executionRecoveryKey(row) === local.executionRecoveryTarget)) resetExecutionRecovery(local);
  local.execution = projection; local.executionError = "";
}
export async function refreshDeviceExecution(context: ActionContext): Promise<void> {
  const local = context.uiState.deviceNetwork;
  if (context.getViewState()?.overlay !== "hub" || local.executionPending) return;
  const serial = ++local.executionSerial;
  try {
    const projection = await command<DeviceExecutionProjection>("device_execution_projection");
    if (serial !== local.executionSerial || context.getViewState()?.overlay !== "hub") return;
    acceptExecution(local, projection);
  } catch { if (serial === local.executionSerial) local.executionError = "このPCの実行設定を確認できません。"; }
}
export async function deviceExecutionAction(context: ActionContext, kind: string, value = ""): Promise<void> {
  const local = context.uiState.deviceNetwork, p = local.execution;
  if (context.getViewState()?.overlay !== "hub" || !deviceExecutionActionEnabled(local, kind, value) || !p) return;
  const serial = ++local.executionSerial;
  local.executionPending = kind; local.executionError = "";
  const request: Record<string, unknown> = { kind };
  if (kind === "prepare") request.access_mode = local.executionAccess;
  if (kind === "enable") request.review_id = p.review?.id;
  if (kind === "reconcile") Object.assign(request, { attempt_id: value, generation: p.unknown_attempts.find(row => row.attempt_id === value)?.generation, reason: local.executionRecoveryReason,
    evidence: { kind: "operator_confirmed_stopped", effects_reviewed: local.executionEffectsReviewed, processes_stopped: local.executionProcessesStopped } });
  context.rerender();
  try {
    const projection = await command<DeviceExecutionProjection>("device_execution_command", { expectedRevision: p.revision, request });
    if (serial === local.executionSerial) acceptExecution(local, projection);
  } catch { if (serial === local.executionSerial) local.executionError = "設定を完了できません。接続とこのPCの状態を確認してください。"; }
  finally { if (serial === local.executionSerial) { local.executionPending = null; context.rerender(); } }
}
export function deviceExecutionActionEnabled(local: DeviceNetworkPresentation, kind: string, value = ""): boolean {
  const p = local.execution;
  if (!p || local.executionPending || p.state === "unconnected") return false;
  if (kind === "prepare") return p.state !== "starting";
  if (kind === "enable") return Boolean(p.review && p.review.access_mode === local.executionAccess && p.state !== "starting");
  if (kind === "pause") return p.can_pause;
  if (kind === "resume") return p.can_resume;
  if (kind === "reconcile") return Boolean(local.executionRecoveryReason.trim() && local.executionEffectsReviewed && local.executionProcessesStopped
    && p.unknown_attempts.some(row => row.attempt_id === value && executionRecoveryKey(row) === local.executionRecoveryTarget));
  return false;
}
export function renderDeviceExecution(local: DeviceNetworkPresentation): string {
  const p = local.execution, busy = Boolean(local.executionPending), esc = escapeHtml;
  const labels = { unconnected: "Hubへの接続待ち", not_selected: "実行するPCとしての割り当てはありません", needs_setup: "初回の実行設定が必要です", starting: "実行の準備をしています", ready: "実行機能が動作中", paused: "新規の実行を一時停止中", unavailable: "実行の状態を確認してください" };
  const accessLabels = { default: "承認を求める", auto_review: "代理で承認", full_access: "フルアクセス" };
  return `<section class="device-network-card" id="device-execution"><h3>このPCで仕事を実行</h3><div data-settings-passive="device-execution-status"><p role="status">${p ? labels[p.state] : "状態を確認しています…"}</p>${p?.projects.filter(row => row.can_execute).map(row => `<p>${esc(row.label)} · ${{not_selected:"未割り当て",waiting_setup:"このPCの設定待ち",pending:"作業フォルダーの準備中",ready:"準備完了",failed:"準備を確認してください"}[row.preparation_state]}${row.error ? ` · ${esc(row.error)}` : ""}</p>`).join("") ?? ""}${local.executionError || p?.error ? `<p class="hub-feedback" data-error="true">${esc(local.executionError || p?.error || "")}</p>` : ""}</div>
    <p class="hub-help">仕事を依頼するだけなら設定は不要です。このPCを実行に使う場合だけ、保存先と権限を一度設定します。プロジェクトの追加と作業フォルダーの作成はHubが行います。</p>
    <div data-settings-passive="device-execution-directory">${p?.directory ? `<p>保存先: ${esc(p.directory)}<br>実行権限: ${esc(accessLabels[p.access_mode ?? "default"])}</p>` : ""}</div>
    <details data-details-key="device-execution-setup" ${p?.state === "needs_setup" ? "open" : ""}><summary>${p?.directory ? "今後追加されるプロジェクトの保存先・権限" : "初回の実行設定"}</summary>${p?.directory ? `<p class="hub-help">変更は今後追加されるプロジェクトに適用します。作成済みの作業フォルダーと実行権限は変更しません。</p>` : ""}<label class="hub-field">実行する操作の確認<select id="device-execution-access" data-network-field="execution_access" class="settings-control" ${busy ? "disabled" : ""}>${Object.entries(accessLabels).map(([value,label]) => `<option value="${value}" ${local.executionAccess === value ? "selected" : ""}>${label}</option>`).join("")}</select></label><p class="hub-help">「承認を求める」では、確認が必要な操作を依頼の担当者が判断します。実行にはこのPCのAI設定を使います。</p><button data-action="device-execution-prepare">保存先フォルダーを選ぶ</button><div data-settings-passive="device-execution-review">${p?.review ? `<p>保存先: ${esc(p.review.directory)}<br>実行権限: ${esc(accessLabels[p.review.access_mode])}</p><p>この範囲で、Hubが許可したプロジェクトの仕事を実行します。同じプロジェクトで許可されたPCへの依頼も含みます。</p><button data-action="device-execution-enable">この設定で実行を許可</button>` : ""}</div></details>
    <div class="device-network-actions" data-settings-passive="device-execution-actions">${p?.directory && (p.can_pause || p.can_resume) ? `<button data-action="device-execution-${p.can_pause ? "pause" : "resume"}">${p.can_pause ? "新しい仕事の受付を一時停止" : "受付を再開"}</button>` : ""}</div>
    ${renderExecutionRecovery(local)}
    <p class="hub-help">設定後は自動で起動・再接続します。Desktopを閉じても実行中の仕事は続きます。</p></section>`;
}
function renderExecutionRecovery(local: DeviceNetworkPresentation): string {
  const attempts = local.execution?.unknown_attempts ?? [], esc = escapeHtml;
  const selected = attempts.find(row => executionRecoveryKey(row) === local.executionRecoveryTarget);
  const disabled = local.executionPending || !selected ? "disabled" : "";
  return `<div data-settings-passive="device-execution-recovery" data-settings-preserve-focused-region>${attempts.length ? `<details data-details-key="device-execution-recovery"><summary>停止を確認できない処理（${attempts.length}件）</summary><p>対象の処理と外部システムへの影響を現地で確認してから、停止済みとして記録してください。</p>
    <div data-settings-passive="device-execution-recovery-target" data-settings-preserve-focused-region><label class="hub-field">確認する処理<select id="device-execution-recovery-target" data-network-field="execution_recovery_target" ${local.executionPending ? "disabled" : ""}><option value="">処理を選択してください</option>${attempts.map(row => `<option value="${esc(executionRecoveryKey(row))}" ${row === selected ? "selected" : ""}>仕事 ${esc(row.job_id)} · ${esc(row.attempt_id)} / ${row.generation}</option>`).join("")}</select></label></div>
    <div data-settings-passive="device-execution-recovery-evidence" data-settings-preserve-focused-region><label class="hub-field">確認内容<input id="device-execution-recovery-reason" data-network-field="execution_recovery_reason" value="${esc(local.executionRecoveryReason)}" ${disabled}></label><label><input id="device-execution-recovery-effects" type="checkbox" data-network-field="execution_recovery_effects" ${local.executionEffectsReviewed ? "checked" : ""} ${disabled}>外部への影響を確認した</label><label><input id="device-execution-recovery-stopped" type="checkbox" data-network-field="execution_recovery_stopped" ${local.executionProcessesStopped ? "checked" : ""} ${disabled}>関連するプロセスが停止したことを確認した</label></div>
    <div data-settings-passive="device-execution-recovery-submit">${selected ? `<button data-action="device-execution-reconcile" data-value="${esc(selected.attempt_id)}" ${deviceExecutionActionEnabled(local, "reconcile", selected.attempt_id) ? "" : "disabled"}>この処理の停止確認を記録</button>` : ""}</div></details>` : ""}</div>`;
}

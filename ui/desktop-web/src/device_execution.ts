import { command } from "./api.ts";
import type { ActionContext } from "./actions.ts";
import { executionRecoveryKey, resetExecutionRecovery, type DeviceAccessMode, type DeviceNetworkPresentation } from "./device_network_state.ts";
import { acceptSharedWork, type SharedWorkPresentation, type SharedWorkProjection } from "./shared_work_state.ts";
import { escapeHtml } from "./utils.ts";

export interface DeviceExecutionProjection {
  revision: string;
  autostart?: boolean;
  reset_review_required?: boolean;
  state: "unconnected" | "not_selected" | "needs_setup" | "starting" | "ready" | "paused" | "unavailable";
  projects: { id: string; label: string; can_control: boolean; can_execute: boolean; environment_id: string | null; preparation_state: "not_selected" | "waiting_setup" | "pending" | "ready" | "failed"; error: string | null; directory?: string | null; access_mode?: DeviceAccessMode | null; participation_generation?: number }[];
  review: { id: string; directory: string; access_mode: DeviceAccessMode } | null;
  directory: string | null; access_mode: DeviceAccessMode | null; accepting: boolean; can_pause: boolean; can_resume: boolean; error: string | null;
  unknown_attempts: { attempt_id: string; generation: number; job_id: string; environment_id: string; run_id: string; state: string }[];
}
function acceptExecution(local: DeviceNetworkPresentation, projection: DeviceExecutionProjection): void {
  if (local.execution && BigInt(projection.revision) < BigInt(local.execution.revision)) return;
  if (local.execution?.review?.id !== projection.review?.id) local.executionResetConfirmed = false;
  if (local.execution?.review?.id !== projection.review?.id
    || !projection.unknown_attempts.some(row => executionRecoveryKey(row) === local.executionRecoveryTarget)) resetExecutionRecovery(local);
  local.execution = projection; local.executionError = "";
  const confirmation = local.executionLeaveConfirmation;
  if (confirmation && !projection.projects.some(row => row.id === confirmation.projectId
    && (confirmation.participationGeneration === null || row.participation_generation === confirmation.participationGeneration))) {
    local.executionLeaveConfirmation = null;
  }
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
  if (kind === "install-autostart" || kind === "remove-autostart") request.kind = kind.replaceAll("-", "_");
  if (kind === "prepare") request.access_mode = local.executionAccess;
  if (kind === "enable") request.review_id = p.review?.id;
  if (kind === "enable" && p.reset_review_required) request.previous_execution_confirmed = local.executionResetConfirmed;
  if (kind === "reconcile") Object.assign(request, { attempt_id: value, generation: p.unknown_attempts.find(row => row.attempt_id === value)?.generation, reason: local.executionRecoveryReason,
    evidence: { kind: "operator_confirmed_stopped", effects_reviewed: local.executionEffectsReviewed, processes_stopped: local.executionProcessesStopped } });
  context.rerender();
  try {
    const projection = await command<DeviceExecutionProjection>("device_execution_command", { expectedRevision: p.revision, request });
    if (serial === local.executionSerial) acceptExecution(local, projection);
  } catch { if (serial === local.executionSerial) local.executionError = "設定を完了できません。接続とこのPCの状態を確認してください。"; }
  finally { if (serial === local.executionSerial) { local.executionPending = null; context.rerender(); } }
}

export async function bindProjectFolder(context: ActionContext, projectId: string): Promise<void> {
  const local = context.uiState.deviceNetwork;
  const before = local.execution;
  const project = before?.projects.find(row => row.id === projectId && row.can_execute && row.environment_id);
  if (context.getViewState()?.overlay !== "hub" || !before || !project || !projectFolderBindingEnabled(local, projectId)) return;
  const environmentId = project.environment_id!;
  const oldDirectory = project.directory ?? null;
  const serial = ++local.executionSerial;
  local.executionPending = "bind-project-folder";
  local.executionError = "";
  context.rerender();
  try {
    const directory = await command<string | null>("browse_shared_project_folder");
    if (!directory || serial !== local.executionSerial || context.getViewState()?.overlay !== "hub") return;
    const current = await command<DeviceExecutionProjection>("device_execution_projection");
    const target = current.projects.find(row => row.id === projectId && row.can_execute && row.environment_id === environmentId);
    if (!target || (target.directory ?? null) !== oldDirectory) {
      acceptExecution(local, current);
      local.executionError = "プロジェクトの設定が変わりました。最新の状態を確認してから選び直してください。";
      return;
    }
    const shared = await command<import("./shared_work_state.ts").SharedWorkProjection>("shared_work_projection");
    const binding = await command<import("./shared_work_state.ts").SharedWorkProjection>("shared_work_command", { expectedGeneration: shared.generation, request: {
      kind: "bind_project_folder", project_id: projectId, environment_id: environmentId,
      directory, access_mode: target.access_mode ?? current.access_mode ?? local.executionAccess,
      expected_directory: oldDirectory,
    } });
    if (binding.error) { local.executionError = binding.error; return; }
    const refreshed = await command<DeviceExecutionProjection>("device_execution_projection");
    acceptExecution(local, refreshed);
  } catch {
    if (serial === local.executionSerial) local.executionError = "作業フォルダーを登録できません。現在の利用状況と接続を確認してください。";
  } finally {
    if (serial === local.executionSerial) { local.executionPending = null; context.rerender(); }
  }
}

export function projectFolderBindingEnabled(local: DeviceNetworkPresentation, projectId: string): boolean {
  const p = local.execution;
  return Boolean(p?.directory && p.state !== "unconnected" && p.state !== "starting" && !local.executionPending
    && p.projects.some(row => row.id === projectId && row.can_execute && row.environment_id));
}

export function executionProjectLeaveEnabled(local: DeviceNetworkPresentation, projectId: string, pendingProjectId?: string | null): boolean {
  return Boolean(!local.executionPending && pendingProjectId !== projectId
    && local.execution?.projects.some(row => row.id === projectId && row.can_execute && !row.can_control
      && Number.isSafeInteger(row.participation_generation) && (row.participation_generation ?? 0) > 0));
}
export function requestExecutionProjectLeave(context: ActionContext, projectId: string): void {
  const local = context.uiState.deviceNetwork;
  if (context.getViewState()?.overlay !== "hub" || !executionProjectLeaveEnabled(local, projectId,
    context.uiState.sharedWork.projection?.leave_pending_project_id)) return;
  const project = local.execution!.projects.find(row => row.id === projectId)!;
  local.executionLeaveConfirmation = { projectId, participationGeneration: project.participation_generation ?? null };
  local.executionError = "";
  context.rerender();
}
export function cancelExecutionProjectLeave(context: ActionContext): void {
  context.uiState.deviceNetwork.executionLeaveConfirmation = null;
  context.rerender();
}
export async function confirmExecutionProjectLeave(context: ActionContext): Promise<void> {
  const local = context.uiState.deviceNetwork;
  const confirmation = local.executionLeaveConfirmation;
  if (context.getViewState()?.overlay !== "hub" || !confirmation || !executionProjectLeaveEnabled(local, confirmation.projectId,
    context.uiState.sharedWork.projection?.leave_pending_project_id)) return;
  const serial = ++local.executionSerial;
  local.executionPending = "leave-project";
  local.executionError = "";
  context.rerender();
  try {
    const current = await command<DeviceExecutionProjection>("device_execution_projection");
    if (serial !== local.executionSerial) return;
    const project = current.projects.find(row => row.id === confirmation.projectId && row.can_execute && !row.can_control);
    if (!project || (confirmation.participationGeneration !== null
      && project.participation_generation !== confirmation.participationGeneration)) {
      acceptExecution(local, current);
      local.executionError = "プロジェクトの参加状態が変わりました。最新の状態を確認してください。";
      return;
    }
    const shared = await command<SharedWorkProjection>("shared_work_projection");
    if (serial !== local.executionSerial) return;
    const result = await command<SharedWorkProjection>("shared_work_command", { expectedGeneration: shared.generation,
      request: { kind: "leave_project", project_id: confirmation.projectId } });
    if (serial !== local.executionSerial) return;
    acceptSharedWork(context.uiState.sharedWork, result);
    if (result.error) {
      local.executionError = result.error;
      if (result.leave_pending_project_id !== confirmation.projectId) return;
    }
    local.executionLeaveConfirmation = null;
    const refreshed = await command<DeviceExecutionProjection>("device_execution_projection");
    if (serial === local.executionSerial) acceptExecution(local, refreshed);
  } catch {
    if (serial === local.executionSerial) local.executionError = "離脱を確認できません。接続と最新の参加状態を確認してください。";
  } finally {
    if (serial === local.executionSerial) { local.executionPending = null; context.rerender(); }
  }
}
export function deviceExecutionActionEnabled(local: DeviceNetworkPresentation, kind: string, value = ""): boolean {
  const p = local.execution;
  if (!p || local.executionPending || p.state === "unconnected") return false;
  if (kind === "prepare") return p.state !== "starting";
  if (kind === "enable") return Boolean(p.review && p.review.access_mode === local.executionAccess && p.state !== "starting"
    && (!p.reset_review_required || local.executionResetConfirmed));
  if (kind === "pause") return p.can_pause;
  if (kind === "resume") return p.can_resume;
  if (kind === "install-autostart") return Boolean(p.directory && !p.autostart);
  if (kind === "remove-autostart") return Boolean(p.directory && p.autostart);
  if (kind === "reconcile") return Boolean(local.executionRecoveryReason.trim() && local.executionEffectsReviewed && local.executionProcessesStopped
    && p.unknown_attempts.some(row => row.attempt_id === value && executionRecoveryKey(row) === local.executionRecoveryTarget));
  return false;
}
export function renderDeviceExecution(local: DeviceNetworkPresentation, shared?: SharedWorkPresentation): string {
  const p = local.execution, busy = Boolean(local.executionPending), esc = escapeHtml;
  const labels = { unconnected: "先に上の「Hubへの接続」で、このPCの参加を完了してください。", not_selected: "このPCで実行する場合は、実行許可を設定します。プロジェクトへの割り当てはHub管理者が行います。", needs_setup: "まず「1. このPCの実行許可」を設定してください。", starting: "実行機能を起動しています。起動後にプロジェクトの作業フォルダーを選べます。", ready: "このPCの実行機能は動作中です。プロジェクトごとの作業フォルダーを下で確認してください。", paused: "新規の実行を一時停止中", unavailable: "実行の状態を確認してください" };
  const accessLabels = { default: "承認を求める", auto_review: "代理で承認", full_access: "フルアクセス" };
  const awaitingAssignment = p?.state === "not_selected" && Boolean(p.directory);
  const pcName = local.projection?.display_name || local.projection?.local_hostname || "このPC";
  const pendingLeaveId = shared?.projection?.leave_pending_project_id;
  const leaveRows = p?.projects.filter(row => row.can_execute && !row.can_control).map(row => {
    const pending = pendingLeaveId === row.id;
    const confirming = local.executionLeaveConfirmation?.projectId === row.id;
    const supported = Number.isSafeInteger(row.participation_generation) && (row.participation_generation ?? 0) > 0;
    return `<div class="device-project-leave"><strong>${esc(row.label)}</strong>${pending
      ? '<p role="status">このPCの離脱処理待ちです。停止と離脱の確定を確認しています。</p>'
      : supported ? `<button data-action="request-execution-project-leave" data-value="${esc(row.id)}" ${busy ? "disabled" : ""}>このPCをプロジェクトから離脱</button>`
        : '<p>このPCから離脱するにはHubを更新し、接続を確認してください。</p>'}
      ${confirming && !pending ? `<div role="group" aria-label="実行PCのプロジェクト離脱"><p>このPCが「${esc(row.label)}」から離脱します。他のPC、共有チャット、各PCのファイルは残ります。実行中の仕事は停止確認に進みます。</p><button data-action="confirm-execution-project-leave" ${busy ? "disabled" : ""}>離脱する</button><button data-action="cancel-execution-project-leave" ${busy ? "disabled" : ""}>戻る</button></div>` : ""}</div>`;
  }).join("") ?? "";
  return `<section class="device-network-card" id="device-execution"><h3>このPCで仕事を実行</h3><p class="hub-help">仕事を依頼・閲覧するだけのPCでは、ここでの設定は不要です。実行するPCでは、参加承認の後に次の順で設定します。</p><div data-settings-passive="device-execution-status">${awaitingAssignment ? `<div class="device-execution-handoff" role="status"><strong>このPCの実行設定は保存済みです</strong><p>次はHub管理者の操作です。「${esc(pcName)}」をプロジェクトの実行PCに割り当てるよう依頼してください。</p></div>` : `<p role="status">${p ? labels[p.state] : "状態を確認しています…"}</p>`}${local.executionError || p?.error ? `<p class="hub-feedback" data-error="true">${esc(local.executionError || p?.error || "")}</p>` : ""}</div>
    <h4>1. このPCの実行許可</h4><div data-settings-passive="device-execution-directory">${p?.directory ? `<p>設定済み · フォルダーの作成先: ${esc(p.directory)}<br>操作の確認: ${esc(accessLabels[p.access_mode ?? "default"])}</p>` : ""}</div>
    <details data-details-key="device-execution-setup" ${p && !p.directory && p.state !== "unconnected" ? "open" : ""}><summary><span data-settings-passive="device-execution-setup-label">${p?.directory ? "このPCの実行設定を変更" : "実行許可を設定"}</span></summary><p class="hub-help">実行用フォルダーを新しく作るときの保存先と、操作の承認方法を設定します。アプリのファイルを置く作業フォルダーは、この後プロジェクトごとに選びます。</p><label class="hub-field">実行する操作の確認<select id="device-execution-access" data-network-field="execution_access" class="settings-control" ${deviceExecutionActionEnabled(local, "prepare") ? "" : "disabled"}>${Object.entries(accessLabels).map(([value,label]) => `<option value="${value}" ${local.executionAccess === value ? "selected" : ""}>${label}</option>`).join("")}</select></label><p class="hub-help">「承認を求める」では、確認が必要な操作を依頼の担当者が判断します。AIはHubで登録された接続先を使います。</p><button data-action="device-execution-prepare" ${deviceExecutionActionEnabled(local, "prepare") ? "" : "disabled"}>フォルダーの作成先を選ぶ</button><div data-settings-passive="device-execution-review" data-settings-preserve-focused-region>${p?.review ? `<p>フォルダーの作成先: ${esc(p.review.directory)}<br>操作の確認: ${esc(accessLabels[p.review.access_mode])}</p><p>この設定で、Hubが許可したプロジェクトの仕事を実行します。同じプロジェクトで許可されたPCへの依頼も含みます。</p>${p.reset_review_required ? `<p class="hub-help">以前のHubで実行した仕事の記録は保持されています。新しい実行を始める前に、このPCで以前の処理と関連プロセスが停止したこと、ファイルと外部システムへの影響を確認してください。旧仕事を完了扱いにはしません。</p><label><input id="device-execution-reset-confirmed" type="checkbox" data-network-field="execution_reset_confirmed" ${local.executionResetConfirmed ? "checked" : ""}>以前の処理の停止と影響を確認した</label>` : ""}<button data-action="device-execution-enable" ${deviceExecutionActionEnabled(local, "enable") ? "" : "disabled"}>この設定で実行を許可</button>` : ""}</div></details>
    <div data-settings-passive="device-execution-project-folders" data-settings-preserve-focused-region><h4>2. プロジェクトの作業フォルダー</h4><p class="hub-help">AIが読み書きするフォルダーを、このPCのプロジェクトごとに選びます。作成途中のアプリが入った既存フォルダーも利用できます。</p>${p?.projects.filter(row => row.can_execute).map(row => `<div class="device-project-folder"><strong>${esc(row.label)}</strong><p>${row.directory ? `作業フォルダー: ${esc(row.directory)}` : !p.directory ? "上の「1. このPCの実行許可」を保存すると選べます。" : !row.environment_id ? "Hubでこのプロジェクトの実行場所を準備しています。" : row.preparation_state === "ready" ? "作業フォルダーを再選択してください。選ぶまでこのPCでは新しい仕事を実行できません。" : "このプロジェクトの作業フォルダーを選んでください。"}${p.directory && row.error ? ` · ${esc(row.error)}` : ""}</p>${row.environment_id ? `<button data-action="bind-project-folder" data-value="${esc(row.id)}" ${projectFolderBindingEnabled(local, row.id) ? "" : "disabled"}>${row.directory ? "作業フォルダーを変更" : "作業フォルダーを選ぶ"}</button>` : ""}</div>`).join("") || '<p class="hub-help">Hub管理者がこのPCをプロジェクトの実行PCに指定すると、ここにプロジェクトが表示されます。</p>'}</div>
    ${leaveRows ? `<div class="device-project-leave-list" data-settings-passive="execution-project-leave">${leaveRows}</div>` : ""}
    <div class="device-network-actions" data-settings-passive="device-execution-actions">${p?.directory && (p.can_pause || p.can_resume) ? `<button data-action="device-execution-${p.can_pause ? "pause" : "resume"}">${p.can_pause ? "新しい仕事の受付を一時停止" : "受付を再開"}</button>` : ""}</div>
    ${renderExecutionRecovery(local)}
    <div data-settings-passive="device-execution-autostart">${p?.directory ? `<h4>このPCで仕事を受け付ける時間</h4><p>${p.autostart ? "Windowsへのサインイン時に実行機能を起動します。" : "moyAIを開くと実行機能を起動します。"} moyAIの画面を閉じても実行中の仕事は続きます。Windowsからサインアウト中・PCの電源が切れている間は実行できません。</p><button data-action="device-execution-${p.autostart ? "remove" : "install"}-autostart" ${busy ? "disabled" : ""}>${p.autostart ? "サインイン時の自動起動を解除" : "サインイン時の自動起動を有効にする"}</button>` : ""}</div>
    <p class="hub-help">設定が済んだら、操作PCの左側にあるプロジェクトを開き、いつものチャットと同じように依頼できます。</p></section>`;
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

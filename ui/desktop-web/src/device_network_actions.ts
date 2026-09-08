import { command } from "./api.ts";
import type { ActionContext } from "./actions.ts";
import { acceptDeviceNetworkProjection, deviceCanJoin, deviceCanReceive, deviceCanSelect, deviceNetworkError,
  deviceNetworkTarget, devicePeerKey, deviceCanStopJob, deviceReceiverConfirmed, type DeviceIncomingJob, type DeviceNetworkJobs,
  type DeviceNetworkProjection, type DeviceNetworkUiState, type DeviceOutgoingJob } from "./device_network_state.ts";

async function request(context: ActionContext, pending: NonNullable<DeviceNetworkUiState["pending"]>, name: string, args: Record<string, unknown> = {}): Promise<void> {
  const local = context.uiState.deviceNetwork;
  if (local.pending || context.getViewState()?.overlay !== "hub") return;
  const serial = ++local.requestSerial;
  local.pending = pending; local.error = ""; local.notice = "";
  context.rerender();
  const current = () => serial === local.requestSerial && context.getViewState()?.overlay === "hub";
  try {
    const projection = await command<DeviceNetworkProjection>(name, args);
    if (!current()) return;
    const accepted = acceptDeviceNetworkProjection(local, projection, { savedReceiver: pending === "receiver" && args.enabled === true, reviewLatest: pending === "refresh" });
    if (!accepted) return;
    if ((pending === "import" || pending === "join") && projection.enrollment === "pending") {
      local.notice = "参加申請を送信しました。Hub管理者の承認を待っています。承認後は自動で接続します。";
    } else if ((pending === "import" || pending === "join") && projection.enrollment === "active") {
      local.notice = "Hubに接続しました。「モデル割当」を確認してください。この端末の受付は、公開対象と権限を確認してから開始します。";
    } else if (pending === "receiver") local.notice = projection.receiver.enabled ? "受付設定を保存しました。稼働状態を確認してください。" : "受付をOFFにしました。実行中タスクの停止完了は経路の状態を確認してください。";
    else if (pending === "select") local.notice = "利用先の選択を保存しました。次の依頼から適用されます。";
    else if (pending === "leave") {
      local.leaveConfirmed = false;
      local.notice = "接続を一時解除しました。登録IDと設定は保持されています。「再接続」で接続を戻せます。";
    }
  } catch (error) {
    if (!current()) return;
    const original = deviceNetworkError(error);
    if (name !== "device_network_projection") {
      try { const projection = await command<DeviceNetworkProjection>("device_network_projection"); if (current()) acceptDeviceNetworkProjection(local, projection); }
      catch { /* Keep the actionable original failure and the draft's reviewed target. */ }
    }
    if (current()) local.error = original;
  } finally { if (serial === local.requestSerial) { local.pending = null; context.rerender(); } }
}
export async function loadDeviceNetwork(context: ActionContext): Promise<void> {
  await request(context, "load", "device_network_projection");
  await refreshDeviceNetworkJobs(context);
}
export async function refreshDeviceNetwork(context: ActionContext): Promise<void> {
  await request(context, "refresh", "device_network_refresh");
  await refreshDeviceNetworkJobs(context);
}
export async function importDeviceNetwork(context: ActionContext): Promise<void> {
  const projection = context.uiState.deviceNetwork.projection;
  if (projection) await request(context, "import", "device_network_import", { ...deviceNetworkTarget(projection) });
}
export async function joinDeviceNetwork(context: ActionContext): Promise<void> {
  const local = context.uiState.deviceNetwork;
  if (!deviceCanJoin(local) || !local.projection) return;
  await request(context, "join", "device_network_request_join", { ...deviceNetworkTarget(local.projection) });
}
export async function setDeviceReceiver(context: ActionContext, enabled: boolean): Promise<void> {
  const local = context.uiState.deviceNetwork;
  if (!deviceCanReceive(local, enabled) || !local.projection) return;
  const receiver = local.projection.receiver;
  await request(context, "receiver", "device_network_receiver", { enabled,
    target: enabled ? local.target : receiver.target, accessMode: enabled ? local.accessMode : receiver.access_mode,
    modelMode: enabled ? local.modelMode : receiver.model_mode, confirmed: enabled && deviceReceiverConfirmed(local),
    startOnLaunch: enabled ? local.startOnLaunch : receiver.start_on_launch,
    keepWhenHidden: enabled ? local.keepWhenHidden : receiver.keep_when_hidden,
    bindIp: enabled ? local.bindIp.trim() || null : receiver.bind_ip,
    port: enabled ? local.port.trim() ? Number(local.port.trim()) : null : receiver.port,
    ...(enabled ? local.draftTarget! : deviceNetworkTarget(local.projection)),
  });
}
export async function selectDevicePeer(context: ActionContext, key: string): Promise<void> {
  const local = context.uiState.deviceNetwork;
  if (!deviceCanSelect(local, key) || !local.projection) return;
  const peer = local.projection.peers.find(row => devicePeerKey(row) === key)!;
  await request(context, "select", "device_network_select", { deviceId: peer.device_id, profileId: peer.profile_id,
    enabled: !peer.selected, ...deviceNetworkTarget(local.projection) });
}
export async function leaveDeviceNetwork(context: ActionContext): Promise<void> {
  const local = context.uiState.deviceNetwork;
  if (local.projection?.can_leave && local.leaveConfirmed) await request(context, "leave", "device_network_leave", { ...deviceNetworkTarget(local.projection) });
}
/** Uses the application's existing snapshot cadence; no independent network timer. */
export async function refreshDeviceNetworkJobs(context: ActionContext): Promise<void> {
  const local = context.uiState.deviceNetwork;
  if (context.getViewState()?.overlay !== "hub" || context.uiState.hub.tab !== "devices" || local.pending) return;
  const serial = ++local.jobsSerial;
  try {
    const jobs = await command<DeviceNetworkJobs>("device_network_jobs");
    if (serial !== local.jobsSerial || context.getViewState()?.overlay !== "hub") return;
    local.jobs = jobs; local.jobsError = "";
    const references = new Set(jobs.outgoing.map(row => row.reference_id));
    for (const entries of [local.artifacts, local.artifactErrors, local.artifactNotices]) {
      for (const reference of Object.keys(entries)) if (!references.has(reference)) delete entries[reference];
    }
  } catch {
    if (serial !== local.jobsSerial || context.getViewState()?.overlay !== "hub") return;
    local.jobsError = "経路と停止状況を取得できません。表示は最後に確認した状態です。";
  }
  context.rerender();
}
export async function stopDeviceNetworkJob(context: ActionContext, key: string): Promise<void> {
  const local = context.uiState.deviceNetwork;
  if (context.getViewState()?.overlay !== "hub" || !deviceCanStopJob(local, key)) return;
  const serial = ++local.requestSerial;
  ++local.jobsSerial;
  local.pending = "cancel_job"; local.error = ""; context.rerender();
  const current = () => serial === local.requestSerial && context.getViewState()?.overlay === "hub";
  try {
    if (key.startsWith("outgoing:")) {
      const referenceId = key.slice(9);
      const row = await command<DeviceOutgoingJob>("device_network_cancel", { referenceId });
      if (current() && row.reference_id === referenceId) local.jobs.outgoing = local.jobs.outgoing.map(job => job.reference_id === referenceId ? row : job);
    } else {
      const job = local.jobs.incoming.find(row => row.job_id === key.slice(9) && row.profile_id === local.projection?.receiver.profile_id)!;
      const row = await command<DeviceIncomingJob>("mcp_publish_cancel_job", { profileId: job.profile_id, jobId: job.job_id });
      if (current() && row.job_id === job.job_id && row.profile_id === job.profile_id) local.jobs.incoming = local.jobs.incoming.map(candidate => candidate.job_id === row.job_id ? { ...row, network: row.network ?? candidate.network } : candidate);
    }
    if (current()) local.notice = "停止を要求しました。停止確認済みになるまで状態を確認してください。";
  } catch (error) { if (current()) local.error = deviceNetworkError(error); }
  finally { if (serial === local.requestSerial) { local.pending = null; context.rerender(); } }
}

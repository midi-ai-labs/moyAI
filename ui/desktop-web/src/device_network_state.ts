import type { PublishJob, PublishTarget } from "./mcp_publish_state.ts";

export type DeviceTarget = Exclude<PublishTarget, { kind: "legacy_session" }>;
export type DeviceAccessMode = "default" | "auto_review" | "full_access";
export interface DeviceNetworkPeer {
  device_id: string;
  profile_id: string;
  display_name: string;
  name?: string;
  selected: boolean;
  online: boolean;
  receiving: boolean;
  can_use: boolean;
  reason: string | null;
}
export interface DeviceNetworkProjection {
  generation: string;
  revision: string;
  hub_url: string;
  device_id: string | null;
  display_name: string;
  local_hostname: string;
  enrollment: "unconfigured" | "not_enrolled" | "pending" | "active" | "disconnected" | "revoked" | "error";
  receiver: {
    profile_id: string;
    enabled: boolean;
    status: string;
    target: DeviceTarget;
    access_mode: DeviceAccessMode;
    model_mode: "hub" | "direct";
    start_on_launch: boolean;
    keep_when_hidden: boolean;
    endpoint: string | null;
    can_change: boolean;
    confirmed: boolean;
    reason: string | null;
  };
  peers: DeviceNetworkPeer[];
  targets: { target: DeviceTarget; label: string }[];
  can_join: boolean;
  can_leave: boolean;
  error: string | null;
}
export interface DeviceNetworkTarget { expectedRevision: string; expectedGeneration: string }
export interface DeviceIncomingJob extends PublishJob {
  network?: { origin_device_id: string; actor_device_id: string; root_task_id: string; device_path: string[] } | null;
}
export interface DeviceOutgoingJob {
  reference_id: string; root_task_id: string; device_id: string; profile_id: string; device_path: string[];
  job_id: string | null; state: "preparing" | "accepted" | "running" | "cancelling" | "completed" | "failed" | "interrupted" | "unknown";
  stop_status: "none" | "requested" | "unconfirmed" | "confirmed"; can_stop: boolean; result: string | null;
}
export interface DeviceNetworkJobs { incoming: DeviceIncomingJob[]; outgoing: DeviceOutgoingJob[] }
export interface DeviceNetworkUiState {
  projection: DeviceNetworkProjection | null;
  pending: "load" | "import" | "join" | "receiver" | "select" | "refresh" | "leave" | "cancel_job" | null;
  requestSerial: number;
  search: string;
  joinConfirmed: boolean;
  receiverConfirmed: boolean;
  target: DeviceTarget;
  accessMode: DeviceAccessMode;
  modelMode: "hub" | "direct";
  startOnLaunch: boolean;
  keepWhenHidden: boolean;
  dirty: boolean;
  draftTarget: DeviceNetworkTarget | null;
  leaveConfirmed: boolean;
  error: string;
  notice: string;
  jobs: DeviceNetworkJobs;
  jobsSerial: number;
  jobsError: string;
}
export type DeviceNetworkPresentation = Omit<DeviceNetworkUiState, "requestSerial" | "jobsSerial">;
export function createDeviceNetworkUiState(): DeviceNetworkUiState {
  return { projection: null, pending: null, requestSerial: 0, search: "", joinConfirmed: false,
    receiverConfirmed: false, target: { kind: "temp" }, accessMode: "default", modelMode: "hub",
    dirty: false, draftTarget: null, leaveConfirmed: false, error: "", notice: "", startOnLaunch: false, keepWhenHidden: false,
    jobs: { incoming: [], outgoing: [] }, jobsSerial: 0, jobsError: "" };
}
export function deviceNetworkPresentation(state: DeviceNetworkUiState): DeviceNetworkPresentation {
  const { requestSerial: _serial, jobsSerial: _jobsSerial, ...presentation } = state;
  return presentation;
}
export function deviceNetworkTarget(projection: DeviceNetworkProjection): DeviceNetworkTarget {
  return { expectedRevision: projection.revision, expectedGeneration: projection.generation };
}
export function acceptDeviceNetworkProjection(
  state: DeviceNetworkUiState, projection: DeviceNetworkProjection,
  options: { savedReceiver?: boolean; reviewLatest?: boolean } = {},
): boolean {
  const previous = state.projection;
  if (previous && (BigInt(projection.generation) < BigInt(previous.generation)
    || BigInt(projection.revision) < BigInt(previous.revision))) return false;
  state.projection = projection;
  if (!state.dirty || options.savedReceiver) {
    state.target = structuredClone(projection.receiver.target);
    state.accessMode = projection.receiver.access_mode;
    state.modelMode = projection.receiver.model_mode;
    state.startOnLaunch = projection.receiver.start_on_launch;
    state.keepWhenHidden = projection.receiver.keep_when_hidden;
    state.dirty = false;
    state.draftTarget = deviceNetworkTarget(projection);
    if (options.savedReceiver) state.receiverConfirmed = false;
  } else if (options.reviewLatest) {
    state.draftTarget = deviceNetworkTarget(projection);
    state.receiverConfirmed = false;
  }
  return true;
}
export function deviceTargetKey(target: DeviceTarget): string {
  return target.kind === "temp" ? "temp" : `project:${target.project_id}`;
}
export function editDeviceNetworkField(state: DeviceNetworkUiState, field: string, value: string, checked: boolean): void {
  if (state.pending) return;
  if (field === "search") { state.search = value; return; }
  if (field === "code") { state.error = ""; return; } // The one-use code stays in its connected password input.
  if (field === "join_confirmed") { state.joinConfirmed = checked; return; }
  if (field === "receiver_confirmed") { state.receiverConfirmed = checked; return; }
  if (field === "leave_confirmed") { state.leaveConfirmed = checked; return; }
  if (field === "target") {
    const target = state.projection?.targets.find(row => deviceTargetKey(row.target) === value)?.target;
    if (!target) return;
    state.target = structuredClone(target);
  } else if (field === "access" && ["default", "auto_review", "full_access"].includes(value)) state.accessMode = value as DeviceAccessMode;
  else if (field === "model" && ["hub", "direct"].includes(value)) state.modelMode = value as "hub" | "direct";
  else if (field === "start_on_launch") state.startOnLaunch = checked;
  else if (field === "keep_when_hidden") state.keepWhenHidden = checked;
  else return;
  state.dirty = true;
  state.receiverConfirmed = false;
  state.error = "";
  state.notice = "";
}
export function deviceDraftCurrent(state: DeviceNetworkPresentation): boolean {
  return Boolean(state.projection && state.draftTarget
    && state.draftTarget.expectedRevision === state.projection.revision
    && state.draftTarget.expectedGeneration === state.projection.generation);
}
export function deviceCanJoin(state: DeviceNetworkPresentation): boolean {
  return !state.pending && Boolean(state.projection?.can_join) && state.joinConfirmed;
}
export function deviceCanReceive(state: DeviceNetworkPresentation, enabled: boolean): boolean {
  if (state.pending || !state.projection?.receiver.can_change) return false;
  if (!enabled) return state.projection.receiver.enabled;
  return state.projection.enrollment === "active" && deviceReceiverConfirmed(state) && deviceDraftCurrent(state)
    && state.projection.targets.some(row => JSON.stringify(row.target) === JSON.stringify(state.target));
}
export function deviceReceiverConfirmed(state: DeviceNetworkPresentation): boolean {
  const receiver = state.projection?.receiver;
  return state.receiverConfirmed || Boolean(receiver?.confirmed
    && JSON.stringify(receiver.target) === JSON.stringify(state.target)
    && receiver.access_mode === state.accessMode && receiver.model_mode === state.modelMode);
}
export function devicePeerKey(peer: Pick<DeviceNetworkPeer, "device_id" | "profile_id">): string {
  return JSON.stringify([peer.device_id, peer.profile_id]);
}
export function deviceCanStopJob(state: DeviceNetworkPresentation, key: string): boolean {
  if (state.pending) return false;
  if (key.startsWith("outgoing:")) return state.jobs.outgoing.some(job => job.reference_id === key.slice(9) && job.can_stop);
  return key.startsWith("incoming:") && state.jobs.incoming.some(job => job.job_id === key.slice(9)
    && job.profile_id === state.projection?.receiver.profile_id && job.can_stop);
}
export function deviceCanSelect(state: DeviceNetworkPresentation, key: string): boolean {
  const peer = state.projection?.peers.find(row => devicePeerKey(row) === key);
  return !state.pending && Boolean(state.projection?.device_id && peer
    && (peer.selected || (state.projection?.enrollment === "active" && peer.can_use)));
}
export function visibleDevicePeers(state: DeviceNetworkPresentation): DeviceNetworkPeer[] {
  const term = state.search.trim().toLocaleLowerCase();
  return state.projection?.peers.filter(peer => !term || [peer.display_name, peer.name ?? "", peer.device_id]
    .some(value => value.toLocaleLowerCase().includes(term))) ?? [];
}
export function devicePeerAvailability(peer: DeviceNetworkPeer): { label: string; tone: string } {
  if (peer.reason === "policy_denied" || peer.reason === "device_revoked") return { label: "利用権限なし", tone: "warning" };
  if (peer.reason === "not_available_or_not_allowed") return { label: "利用不可（状態・許可を確認）", tone: "warning" };
  if (peer.reason === "read_tools_only") return { label: "読み取り専用の公開対象", tone: "muted" };
  if (!peer.online) return { label: "到達・接続を確認できません", tone: "muted" };
  if (!peer.receiving) return { label: "受付停止中", tone: "muted" };
  if (!peer.can_use) return { label: "利用できません", tone: "warning" };
  return peer.selected ? { label: "受付申告あり", tone: "ready" } : { label: "受付申告あり・未選択", tone: "ready" };
}
export function deviceNetworkError(error: unknown): string {
  const messages: Record<string, string> = {
    unconfigured: "Hubの共通設定ファイルを読み込んでください。",
    invalid_config: "共通設定を読み込めませんでした。Hubが出力した設定ファイルを確認してください。",
    enrollment_denied: "参加コードを確認してください。期限切れ、使用済み、または未承認のコードです。",
    device_revoked: "この端末の認証はHubで失効しています。管理者へ確認してください。",
    policy_denied: "この端末への接続は許可されていません。Hub管理者へ確認してください。",
    stale_revision: "設定または接続状態が変わりました。最新情報を取得し、入力内容を確認してから保存してください。",
    network_unavailable: "Hubへ接続できません。ネットワークとHubの稼働状況を確認してください。",
    invalid_configuration: "Hubが出力した共通設定ファイルを確認してください。",
    invalid_identity: "端末の認証情報を利用できません。Hub管理者へ確認してください。",
    settings_corrupt: "保存された端末設定を読み込めません。設定を上書きせず、管理者へ確認してください。",
    settings_changed: "保存設定が変わりました。最新情報を取得し、入力内容を確認してから保存してください。",
    connection_changed: "接続状態が変わりました。最新情報を取得し、入力内容を確認してから保存してください。",
    store_busy: "保存データを別の処理が使用しています。処理が終わってから再操作してください。",
    storage_error: "端末設定を保存できませんでした。保存先の状態を確認してください。",
    unavailable: "接続を確認できません。Hubと相手端末の稼働状況を確認してください。",
    grant_denied: "今回の委任は許可されませんでした。接続ルールと停止状態を確認してください。",
    invalid_response: "接続先の応答を確認できませんでした。Hubのバージョンと稼働状態を確認してください。",
    receiver_busy: "受付の実行・停止処理中です。完了してから設定を変更してください。",
    confirmation_required: "公開対象と実行権限を確認して、確認欄を選択してください。",
    connection_required: "先にHubへの参加・接続を完了してください。",
    read_tools_only: "読み取りtoolの公開対象です。エージェントへのタスク委任には使えません。",
    not_available_or_not_allowed: "相手の受付・接続、またはHubの許可を確認できません。最新情報を取得してください。",
  };
  return typeof error === "string" && messages[error] ? messages[error]
    : "操作を完了できませんでした。最新情報とHubへの接続状態を確認してください。";
}

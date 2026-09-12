import type { PublishJob, PublishTarget } from "./mcp_publish_state.ts";
import type { DeviceDiagnosticResult } from "./device_network_diagnostics.ts";
import type { DeviceArtifactView } from "./device_network_artifacts.ts";

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
  request_id: string | null;
  local_ipv4: string | null;
  enrollment: "unconfigured" | "not_enrolled" | "pending" | "active" | "stopped" | "expired" | "disconnected" | "revoked" | "error";
  receiver: {
    profile_id: string;
    enabled: boolean;
    status: string;
    target: DeviceTarget;
    access_mode: DeviceAccessMode;
    model_mode: "hub" | "direct";
    start_on_launch: boolean;
    keep_when_hidden: boolean;
    bind_ip: string | null;
    port: number | null;
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
  job_id: string | null; state: "preparing" | "accepted" | "running" | "awaiting_approval" | "cancelling" | "completed" | "failed" | "interrupted" | "unknown";
  stop_status: "none" | "requested" | "unconfirmed" | "confirmed"; can_stop: boolean; result: string | null;
}
export interface DeviceNetworkJobs { incoming: DeviceIncomingJob[]; outgoing: DeviceOutgoingJob[] }
export interface DeviceNetworkUiState {
  projection: DeviceNetworkProjection | null;
  pending: "load" | "import" | "join" | "receiver" | "select" | "refresh" | "leave" | "cancel_job" | null;
  selectionKey: string | null;
  requestSerial: number;
  search: string;
  receiverConfirmed: boolean;
  target: DeviceTarget;
  accessMode: DeviceAccessMode;
  modelMode: "hub" | "direct";
  startOnLaunch: boolean;
  keepWhenHidden: boolean;
  bindIp: string;
  port: string;
  dirty: boolean;
  draftTarget: DeviceNetworkTarget | null;
  leaveConfirmed: boolean;
  error: string;
  notice: string;
  jobs: DeviceNetworkJobs;
  jobsSerial: number;
  jobsError: string;
  diagnostics: Record<string, DeviceDiagnosticResult>;
  diagnosticErrors: Record<string, string>;
  diagnosticPending: string | null;
  diagnosticSerial: number;
  artifacts: Record<string, DeviceArtifactView>;
  artifactErrors: Record<string, string>;
  artifactNotices: Record<string, string>;
  artifactPending: { referenceId: string; operation: "inspect" | "export" } | null;
  artifactSerial: number;
}
export type DeviceNetworkPresentation = Omit<DeviceNetworkUiState, "requestSerial" | "jobsSerial" | "diagnosticSerial" | "artifactSerial">;
export function createDeviceNetworkUiState(): DeviceNetworkUiState {
  return { projection: null, pending: null, selectionKey: null, requestSerial: 0, search: "",
    receiverConfirmed: false, target: { kind: "temp" }, accessMode: "default", modelMode: "hub",
    dirty: false, draftTarget: null, leaveConfirmed: false, error: "", notice: "", startOnLaunch: false, keepWhenHidden: false,
    bindIp: "", port: "",
    jobs: { incoming: [], outgoing: [] }, jobsSerial: 0, jobsError: "",
    diagnostics: {}, diagnosticErrors: {}, diagnosticPending: null, diagnosticSerial: 0,
    artifacts: {}, artifactErrors: {}, artifactNotices: {}, artifactPending: null, artifactSerial: 0 };
}
export function deviceNetworkPresentation(state: DeviceNetworkUiState): DeviceNetworkPresentation {
  const { requestSerial: _serial, jobsSerial: _jobsSerial, diagnosticSerial: _diagnosticSerial, artifactSerial: _artifactSerial, ...presentation } = state;
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
  if (previous && previous.enrollment !== "active" && projection.enrollment === "active") {
    state.notice = "Hubへの参加が承認され、接続しました。現在のモデルと送信先は「モデル割当」で確認できます。";
  }
  for (const [key, result] of Object.entries(state.diagnostics)) {
    if (result.revision !== projection.revision || result.generation !== projection.generation
      || (result.scope === "peer" && !projection.peers.some(peer => peer.device_id === result.device_id && peer.profile_id === result.profile_id))) delete state.diagnostics[key];
  }
  if (previous && (previous.revision !== projection.revision || previous.generation !== projection.generation)) state.diagnosticErrors = {};
  if (!state.dirty || options.savedReceiver) {
    state.target = structuredClone(projection.receiver.target);
    state.accessMode = projection.receiver.access_mode;
    state.modelMode = projection.receiver.model_mode;
    state.startOnLaunch = projection.receiver.start_on_launch;
    state.keepWhenHidden = projection.receiver.keep_when_hidden;
    state.bindIp = projection.receiver.bind_ip ?? "";
    state.port = projection.receiver.port === null ? "" : String(projection.receiver.port);
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
  else if (field === "bind_ip") state.bindIp = value;
  else if (field === "port") state.port = value;
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
  return !state.pending && Boolean(state.projection?.can_join);
}
export function deviceCanReceive(state: DeviceNetworkPresentation, enabled: boolean): boolean {
  if (state.pending || !state.projection?.receiver.can_change) return false;
  if (!enabled) return state.projection.receiver.enabled;
  return state.projection.enrollment === "active" && deviceReceiverConfirmed(state) && deviceDraftCurrent(state)
    && deviceReceiverBindError(state) === ""
    && state.projection.targets.some(row => JSON.stringify(row.target) === JSON.stringify(state.target));
}
export function deviceReceiverBindError(state: Pick<DeviceNetworkPresentation, "bindIp" | "port">): string {
  const ip = state.bindIp.trim();
  if (ip) {
    const parts = ip.split(".");
    if (parts.length !== 4 || parts.some(part => !/^(0|[1-9]\d{0,2})$/.test(part) || Number(part) > 255)
      || parts.every(part => part === "0") || parts.every(part => part === "255")
      || (Number(parts[0]) >= 224 && Number(parts[0]) <= 239)) {
      return "このPCで待ち受ける具体的なIPv4を入力してください。空欄にすると自動設定します。";
    }
  }
  const port = state.port.trim();
  if (port && (!/^\d+$/.test(port) || Number(port) < 1 || Number(port) > 65535)) {
    return "受付ポートは1〜65535で指定してください。空欄にすると自動設定します。";
  }
  return "";
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
    enrollment_denied: "参加申請を送信できません。Hub管理者に申請状況と接続を確認してください。",
    device_name_conflict: "同じ端末名が既に登録されています。Hub管理者に既存登録の名前変更または不要登録の削除を依頼し、参加申請を再試行してください。",
    device_stopped: "Hub管理者がこの端末の利用を停止しています。再許可されると自動で接続状態を更新します。",
    join_superseded: "この参加申請の証明書は更新済みです。既存の端末設定を確認してください。別の端末として自動登録はしません。",
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
    receiver_port_in_use: "指定した受付ポートは使用中です。ほかのアプリの待受を停止するか、受付ポートを変更してください。",
    receiver_address_unavailable: "指定した受付IPで待ち受けられません。このPCのIPv4とネットワーク接続を確認してください。",
    authority_retired: "この仕事を遠隔操作できる保持期間が終了しました。保存済みの結果は引き続き参照できます。",
    recovery_required: "以前の依頼の受付を確認できません。自動で再実行せず、実行端末で状態を確認してから新しい依頼を送ってください。",
    artifacts_unavailable: "この版の成果物を取得できません。成果物の記録と相手の受付状態を確認してください。",
    confirmation_required: "公開対象と実行権限を確認して、確認欄を選択してください。",
    connection_required: "先にHubへの参加・接続を完了してください。",
    read_tools_only: "読み取りtoolの公開対象です。エージェントへのタスク委任には使えません。",
    not_available_or_not_allowed: "相手の受付・接続、またはHubの許可を確認できません。最新情報を取得してください。",
  };
  return typeof error === "string" && messages[error] ? messages[error]
    : "操作を完了できませんでした。最新情報とHubへの接続状態を確認してください。";
}

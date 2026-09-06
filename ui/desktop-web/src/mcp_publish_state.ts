export type PublishTool = "list" | "glob" | "grep" | "read" | "inspect_directory" | "current_time";
export type PublishBackground = "stop_when_window_closes" | "keep_while_application_running";
export type PublishMode = { kind: "read_tools" }
  | { kind: "agent"; access_mode: "default" | "auto_review" | "full_access" };
export interface PublishTls { certificate_path: string; private_key_path: string }
export type PublishTarget = { kind: "project"; project_id: string; workspace_root: string }
  | { kind: "temp" }
  | { kind: "legacy_session"; project_id: string; root_session_id: string; workspace_root: string };
export interface PublishProfileDraft {
  label: string;
  bind: string;
  mode: PublishMode;
  tls: PublishTls | null;
  target: PublishTarget | null;
  tools: PublishTool[];
  max_concurrent_calls: number;
  background: PublishBackground;
}
export interface PublishProfile extends Omit<PublishProfileDraft, "target"> {
  target: PublishTarget;
  id: string;
  enabled: boolean;
  transport: "streamable_http";
  authentication: { kind: "unpaired" } | { kind: "local_credential"; credential_id: string };
}
export interface PublishProfileRow {
  profile: PublishProfile;
  status: "stopped" | "starting" | "running" | "stopping" | "error";
  status_message: string | null;
  endpoint: string | null;
  active_calls: number;
  connected_sessions: number;
  recent_calls: { id: string; tool: string; status: string }[];
  credential_configured: boolean;
  can_edit: boolean;
  can_delete: boolean;
  can_start: boolean;
  can_stop: boolean;
  can_issue_token: boolean;
  can_revoke_token: boolean;
}
export interface PublishProjection {
  revision: string;
  generation: string;
  profiles: PublishProfileRow[];
  targets: { target: PublishTarget; label: string }[];
  error: string | null;
}
export interface PublishJob {
  job_id: string; profile_id: string;
  parent: { peer_id: string; task_id: string; turn_id: string };
  prompt_preview: string; session_id: string;
  state: "accepted" | "running" | "cancelling" | "completed" | "failed" | "interrupted";
  model: string; result: string | null; result_truncated: boolean; can_stop: boolean;
}
export interface PublishEditor {
  value: PublishProfileDraft;
  port: string;
  host: string;
  concurrency: string;
  baseline: PublishProfileDraft | null;
  revision: string;
}
export type PublishOperation = "load" | "save" | "delete" | "start" | "stop" | "issue_token" | "revoke_token"
  | "create_certificate" | "certificate" | "cancel_job";
export type PublishProfileOperation = "delete" | "start" | "stop" | "issue_token" | "revoke_token";
export interface PublishUiState {
  projection: PublishProjection | null;
  selectedId: string | null;
  drafts: Record<string, PublishEditor>;
  pending: PublishOperation | null;
  requestSerial: number;
  error: string;
  notice: string;
  deleteConfirmation: string | null;
  jobs: PublishJob[];
  jobsSerial: number;
  jobsError: string;
}
export type PublishPresentation = Omit<PublishUiState, "requestSerial" | "jobsSerial">;
export const NEW_PUBLISH_PROFILE = "new";
export const PUBLISH_TOOLS: readonly { name: PublishTool; label: string; description: string }[] = [
  { name: "list", label: "一覧を見る", description: "フォルダー内のファイル一覧" },
  { name: "glob", label: "名前で探す", description: "パターンに一致するパスの検索" },
  { name: "grep", label: "内容を検索", description: "ファイル内の文字列検索" },
  { name: "read", label: "ファイルを読む", description: "指定したファイルの読み取り" },
  { name: "inspect_directory", label: "構成を調べる", description: "フォルダー構成の確認" },
  { name: "current_time", label: "現在時刻", description: "端末の日時とタイムゾーン" },
];

export function createPublishUiState(): PublishUiState {
  return { projection: null, selectedId: null, drafts: {}, pending: null, requestSerial: 0,
    error: "", notice: "", deleteConfirmation: null, jobs: [], jobsSerial: 0, jobsError: "" };
}
export function publishPresentation(state: PublishUiState): PublishPresentation {
  const { requestSerial: _serial, jobsSerial: _jobsSerial, ...presentation } = state;
  return presentation;
}
function profileDraft(profile: PublishProfile): PublishProfileDraft {
  return structuredClone({ label: profile.label, bind: profile.bind, target: profile.target,
    mode: profile.mode, tls: profile.tls,
    tools: profile.tools, max_concurrent_calls: profile.max_concurrent_calls, background: profile.background });
}
function editorFor(value: PublishProfileDraft, revision: string, existing: boolean): PublishEditor {
  const ipv6 = value.bind.startsWith("[");
  const separator = value.bind.lastIndexOf(":");
  return { value: structuredClone(value), baseline: existing ? structuredClone(value) : null, revision,
    host: ipv6 ? value.bind.slice(1, value.bind.indexOf("]")) : value.bind.slice(0, separator),
    port: value.bind.slice(separator + 1), concurrency: String(value.max_concurrent_calls) };
}
export function publishEditor(state: PublishPresentation): PublishEditor | null {
  return state.selectedId ? state.drafts[state.selectedId] ?? null : null;
}
export function publishRow(state: PublishPresentation): PublishProfileRow | null {
  return state.projection?.profiles.find((row) => row.profile.id === state.selectedId) ?? null;
}
export function publishDraftValue(editor: PublishEditor): PublishProfileDraft {
  return { ...structuredClone(editor.value),
    bind: `${editor.host.includes(":") ? `[${editor.host}]` : editor.host}:${editor.port}`,
    max_concurrent_calls: Number(editor.concurrency) };
}
export function publishDirty(editor: PublishEditor): boolean {
  return !editor.baseline || JSON.stringify(publishDraftValue(editor)) !== JSON.stringify(editor.baseline);
}

export function acceptPublishProjection(state: PublishUiState, projection: PublishProjection,
  savedId?: string): boolean {
  const previous = state.projection;
  if (previous && (BigInt(projection.revision) < BigInt(previous.revision)
    || BigInt(projection.generation) < BigInt(previous.generation))) return false;
  state.projection = projection;
  for (const row of projection.profiles) {
    const id = row.profile.id;
    const canonical = profileDraft(row.profile);
    const editor = state.drafts[id];
    if (!editor || !publishDirty(editor) || id === savedId) {
      state.drafts[id] = editorFor(canonical, projection.revision, true);
    } else if (JSON.stringify(canonical) === JSON.stringify(editor.baseline)) {
      // Another profile or its runtime changed. This profile's saved edit baseline is identical.
      editor.revision = projection.revision;
    }
  }
  for (const id of Object.keys(state.drafts)) {
    if (id !== NEW_PUBLISH_PROFILE && !projection.profiles.some((row) => row.profile.id === id)
      && !publishDirty(state.drafts[id])) delete state.drafts[id];
  }
  if (savedId && state.selectedId === NEW_PUBLISH_PROFILE) {
    delete state.drafts[NEW_PUBLISH_PROFILE];
    state.selectedId = savedId;
  }
  if (state.selectedId === null || !state.drafts[state.selectedId]) {
    state.selectedId = projection.profiles[0]?.profile.id ?? null;
  }
  return true;
}
export function newPublishProfile(state: PublishUiState): boolean {
  if (state.pending || !state.projection || state.projection.profiles.length >= 32) return false;
  state.drafts[NEW_PUBLISH_PROFILE] ??= editorFor({ label: "", bind: "127.0.0.1:7332",
    mode: { kind: "read_tools" }, tls: null,
    target: null, tools: [], max_concurrent_calls: 1, background: "stop_when_window_closes" }, state.projection.revision, false);
  state.selectedId = NEW_PUBLISH_PROFILE;
  state.error = "";
  state.notice = "";
  state.deleteConfirmation = null;
  return true;
}
export function selectPublishProfile(state: PublishUiState, id: string): void {
  if (state.pending || !state.drafts[id]) return;
  state.selectedId = id;
  state.error = "";
  state.notice = "";
  state.deleteConfirmation = null;
}
export function discardPublishDraft(state: PublishUiState): void {
  if (state.pending || !state.projection) return;
  const row = publishRow(state);
  if (row) state.drafts[row.profile.id] = editorFor(profileDraft(row.profile), state.projection.revision, true);
  else if (state.selectedId) {
    delete state.drafts[state.selectedId];
    state.selectedId = state.projection.profiles[0]?.profile.id ?? null;
  }
  state.error = "";
  state.notice = "";
}
export function editPublishField(state: PublishUiState, field: string, value: string, checked = false): void {
  const editor = publishEditor(state);
  if (!editor || state.pending || (state.selectedId !== NEW_PUBLISH_PROFILE && !publishRow(state)?.can_edit)) return;
  if (field === "label") editor.value.label = value;
  else if (field === "mode" && (value === "read_tools" || value === "agent") && value !== editor.value.mode.kind) {
    editor.value.mode = value === "agent" ? { kind: "agent", access_mode: "default" } : { kind: "read_tools" };
    editor.value.tools = [];
  } else if (field === "access_mode" && editor.value.mode.kind === "agent"
    && ["default", "auto_review", "full_access"].includes(value)) {
    editor.value.mode.access_mode = value as "default" | "auto_review" | "full_access";
  } else if (field === "tls") {
    if (value === "enabled") editor.value.tls ??= { certificate_path: "", private_key_path: "" };
    else if (value === "disabled") editor.value.tls = null;
  } else if ((field === "certificate_path" || field === "private_key_path") && editor.value.tls) {
    editor.value.tls[field] = value;
  }
  else if (field === "host") editor.host = value;
  else if (field === "port") editor.port = value;
  else if (field === "concurrency") editor.concurrency = value;
  else if (field === "background" && ["stop_when_window_closes", "keep_while_application_running"].includes(value)) {
    editor.value.background = value as PublishBackground;
  } else if (field === "target") {
    const target = publishTargetChoices(state).find((choice) => publishTargetKey(choice.target) === value)?.target;
    if (target) {
      editor.value.target = structuredClone(target);
      editor.value.tools = editor.value.tools.filter((tool) => publishToolSupported(target, tool));
    }
  } else if (field.startsWith("tool:")) {
    const tool = PUBLISH_TOOLS.find((candidate) => candidate.name === field.slice(5))?.name;
    if (editor.value.mode.kind === "read_tools" && tool && publishToolSupported(editor.value.target, tool)) editor.value.tools = PUBLISH_TOOLS.map((candidate) => candidate.name)
      .filter((name) => name === tool ? checked : editor.value.tools.includes(name));
  }
  state.error = "";
  state.notice = "";
  state.deleteConfirmation = null;
}
export function publishTargetKey(target: PublishTarget | null | undefined): string {
  if (!target) return "";
  return encodeURIComponent(JSON.stringify(target.kind === "temp" ? [target.kind]
    : target.kind === "project" ? [target.kind, target.project_id, target.workspace_root]
      : [target.kind, target.project_id, target.root_session_id, target.workspace_root]));
}
export function samePublishTarget(left: PublishTarget | null | undefined, right: PublishTarget | null | undefined): boolean {
  return Boolean(left && right && publishTargetKey(left) === publishTargetKey(right));
}
export function publishTargetChoices(state: PublishPresentation): PublishProjection["targets"] {
  const savedTarget = publishRow(state)?.profile.target;
  return (state.projection?.targets ?? []).filter((choice) => choice.target.kind !== "legacy_session"
    || (publishEditor(state)?.value.mode.kind !== "agent" && samePublishTarget(choice.target, savedTarget)));
}
export function publishExplicitIp(host: string): boolean {
  if (/^(?:\d{1,3}\.){3}\d{1,3}$/.test(host)) {
    const parts = host.split(".");
    return parts.every((part) => String(Number(part)) === part && Number(part) <= 255)
      && Number(parts[0]) > 0 && Number(parts[0]) < 224;
  }
  if (!/^[0-9a-f:]+$/i.test(host) || !host.includes(":")) return false;
  try {
    const canonical = new URL(`http://[${host}]/`).hostname;
    return canonical !== "[::]" && !canonical.startsWith("[ff");
  } catch { return false; }
}
export function publishToolSupported(target: PublishTarget | null | undefined, tool: PublishTool): boolean {
  return target?.kind !== "temp" || tool === "current_time";
}
export function publishValidation(state: PublishPresentation): string | null {
  const editor = publishEditor(state);
  if (!editor) return "配信プロファイルを選択してください。";
  if (!editor.value.label.trim() || [...editor.value.label].length > 80 || /[\u0000-\u001f\u007f]/.test(editor.value.label)) {
    return "表示名を1〜80文字で入力してください。";
  }
  if (!publishExplicitIp(editor.host)) return "待受アドレスに、この端末の具体的なIPアドレスを入力してください。";
  if (!editor.value.tls && !(editor.host.startsWith("127.") || editor.host === "::1")) return "別の端末へ配信するにはTLS証明書を設定してください。";
  if (editor.value.tls && (!editor.value.tls.certificate_path.trim() || !editor.value.tls.private_key_path.trim())) return "TLS証明書を作成するか、証明書と秘密鍵のファイルを指定してください。";
  if (!/^[1-9]\d{0,4}$/.test(editor.port) || Number(editor.port) > 65535) return "ポートを1〜65535の整数で入力してください。";
  if (!/^(?:[1-9]|1[0-6])$/.test(editor.concurrency)) return "同時実行上限は1〜16です。";
  if (!publishTargetChoices(state).some((choice) => samePublishTarget(choice.target, editor.value.target))) {
    return "公開するプロジェクトを選択してください。";
  }
  if (editor.value.mode.kind === "agent" && editor.value.tools.length) return "エージェント受付では、読み取りツール個別の公開を解除してください。";
  if (editor.value.mode.kind === "read_tools" && editor.value.tools.some((tool) => !publishToolSupported(editor.value.target, tool))) return "tempでは現在時刻だけを公開できます。ファイル操作を外してください。";
  if (state.selectedId !== NEW_PUBLISH_PROFILE && !publishRow(state)) return "このプロファイルは削除されました。変更を破棄して選び直してください。";
  if (state.selectedId !== NEW_PUBLISH_PROFILE && editor.revision !== state.projection?.revision) return "保存済み設定が変更されています。入力を確認し、変更を破棄して読み直してください。";
  return null;
}
export function publishCanSave(state: PublishPresentation): boolean {
  const editor = publishEditor(state);
  return !state.pending && Boolean(editor && publishDirty(editor)) && publishValidation(state) === null
    && (state.selectedId === NEW_PUBLISH_PROFILE || publishRow(state)?.can_edit === true);
}
export function publishCanCreateCertificate(state: PublishPresentation): boolean {
  const editor = publishEditor(state);
  return !state.pending && Boolean(publishRow(state)?.can_edit && editor?.value.tls && publishExplicitIp(editor.host));
}
export function publishCanStopJob(state: PublishPresentation, jobId: string): boolean {
  return !state.pending && state.jobs.some((job) => job.job_id === jobId && job.profile_id === state.selectedId && job.can_stop);
}
export function publishCanOperate(state: PublishPresentation, operation: PublishProfileOperation): boolean {
  if (state.pending) return false;
  const row = publishRow(state);
  if (!row) return false;
  if (operation === "stop") return row.can_stop;
  if (operation === "revoke_token") return row.can_revoke_token;
  if (publishEditor(state) && publishDirty(publishEditor(state)!)) return false;
  return operation === "start" ? row.can_start : operation === "delete" ? row.can_delete : row.can_issue_token;
}
export function publishErrorText(error: unknown): string {
  const key = typeof error === "string" ? error : "";
  const known: Record<string, string> = {
    stale_revision: "設定が更新されました。最新情報を取得して内容を確認してください。",
    stale_generation: "配信状態が変わりました。最新情報を取得して再操作してください。",
    target_mismatch: "公開対象が変更されたか、利用できません。対象を選び直してください。",
    unknown_profile: "配信プロファイルが見つかりません。",
    unpaired: "接続用トークンを発行してから開始してください。",
    bind_failed: "待受を開始できません。ポートがほかのアプリで使用されていないか確認してください。",
    profile_busy: "配信を停止してから設定を変更してください。",
    tool_unavailable: "公開するツールを確認してください。",
    store_busy: "設定の保存中です。少し待って再操作してください。",
    invalid_configuration: "入力した配信設定を確認してください。",
  };
  return known[key] ?? (/[\u3040-\u30ff\u3400-\u9fff]/.test(key) ? key
    : "MCP配信の操作を完了できませんでした。最新情報を取得して状態を確認してください。");
}

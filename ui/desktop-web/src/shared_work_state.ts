export interface WorkPerson { user_id: string; display_name: string }
export interface WorkAsset { id: string; project_id: string; job_id: string | null; kind: string; name: string; sha256: string; byte_length: number; created_at_ms: number; version: number; base_sha256: string | null; purged_at_ms: number | null }
export interface ProviderTemplate { id: string; label: string; base_root: string; access_mode: string; allowed_child_environments: string[] }
export interface ProviderProjection {
  runner_id: string; mode: string; state: string; accepting: boolean; maintenance_until_ms: number | null; autostart: boolean;
  templates: ProviderTemplate[]; environments: { environment_id: string; directory: string; access_mode: string; allowed_child_environments: string[] }[];
  active_attempts: ProviderAttempt[]; unknown_attempts: ProviderAttempt[]; error: string | null;
}
export interface ProviderAttempt { attempt_id: string; generation: number; job_id: string; environment_id: string; run_id: string; state: string }
export interface WorkProject { id: string; label: string; can_submit: boolean }
export interface WorkRunnerContact { last_contact_ms: number | null; state: "unconfirmed" | "recent" | "stale" }
export interface WorkSummary {
  id: string; root_id: string; parent_id: string | null; title: string; state: string;
  environment_id: string; environment_label: string; requestor: WorkPerson; assignee: WorkPerson;
  wait_reason: string | null; uncertainty_reason: string | null; can_cancel: boolean; can_continue?: boolean; can_handover?: boolean;
  revision: number; created_at_ms: number; updated_at_ms: number;
  start_before_ms?: number | null;
  runner_contact?: WorkRunnerContact | null;
}
export interface WorkEnvironment {
  id: string; label: string; resource_id: string; runner_id: string; enabled: boolean;
  capacity: number; occupied: number;
  occupants: { job_id: string; title: string; project_id: string; project_label: string; user: WorkPerson; state: string }[];
  additional_visible_occupants: number; other_occupants: number;
  runner_contact?: WorkRunnerContact | null;
}
export interface SharedWorkProjection {
  revision: string; generation: string; connected: boolean; hub_url: string; enrollment: string; enrollment_error: string | null;
  principal: (WorkPerson & { administrator: boolean }) | null; expires_at_ms: number | null;
  projects: WorkProject[]; selected_project_id: string | null; selected_job_id: string | null;
  status: { project_id: string; jobs: WorkSummary[]; environments: WorkEnvironment[]; next_before: string | null; next_environment_before: string | null } | null;
  detail: { id: string; project_id: string; root_id: string; parent_id: string | null; environment_id: string; title: string; input: unknown; result: unknown; state: string; awaiting_child_id: string | null; revision: number; created_at_ms: number; updated_at_ms: number; start_before_ms?: number | null; continued_from_id?: string | null; can_continue?: boolean; can_handover?: boolean; wait_reason?: string | null; uncertainty_reason?: string | null; runner_contact?: WorkRunnerContact | null } | null;
  approval: { id: string; attempt_id: string; request: { access: string; summary: string; details: string[]; targets: string[]; outside_workspace: boolean; risks: string[]; agent_path?: string; agent_task_name?: string }; status: string; decision: string | null; expires_at_ms: number; can_decide: boolean } | null;
  observed_at_ms: number | null; error: string | null; submission_uncertain: boolean; submission_storage_error: string | null;
  feedback: string | null; inputs: WorkAsset[]; assets: WorkAsset[];
  transcript: { items: { position: number; kind: string; payload: unknown }[]; next_after: number | null } | null;
  handover: { candidates: WorkPerson[]; pending: { new_assignee_id: string; requested_by: string; requested_at_ms: number } | null; can_handover: boolean } | null;
  inbox: { items: { id: string; job_id: string; project_id: string; kind: string; title: string; created_at_ms: number; read_at_ms: number | null; can_act: boolean; approval_id: string | null }[]; next_before: string | null; unread_count: number } | null;
  provider: ProviderProjection | null; provider_draft: ProviderTemplate | null; provider_error: string | null;
  provider_scope: { kind: "device" } | { kind: "workspace_isolation"; confirmed: boolean };
}
export interface SharedWorkUiState {
  projection: SharedWorkProjection | null;
  username: string; password: string; title: string; prompt: string; environmentId: string;
  pending: string | null; serial: number; polling: boolean; error: string; conceal: boolean;
  draft: Record<string, string>;
}
export type SharedWorkPresentation = Omit<SharedWorkUiState, "serial" | "polling">;
export function createSharedWorkUiState(): SharedWorkUiState {
  return { projection: null, username: "", password: "", title: "", prompt: "", environmentId: "", pending: null, serial: 0, polling: false, error: "", conceal: false, draft: {} };
}
export function sharedWorkPresentation(local: SharedWorkUiState): SharedWorkPresentation {
  return { projection: local.projection, username: local.username, password: local.password, title: local.title,
    prompt: local.prompt, environmentId: local.environmentId, pending: local.pending, error: local.error, conceal: local.conceal, draft: local.draft };
}
export function acceptSharedWork(local: SharedWorkUiState, projection: SharedWorkProjection): boolean {
  const previous = local.projection;
  if (previous && BigInt(projection.revision) < BigInt(previous.revision)) return false;
  if (previous?.generation !== projection.generation) {
    local.password = ""; local.title = ""; local.prompt = ""; local.environmentId = "";
    local.draft = {};
  } else if (previous?.selected_project_id !== projection.selected_project_id) {
    local.title = ""; local.prompt = ""; local.environmentId = "";
    local.draft.followup = ""; local.draft.assigneeId = ""; local.draft.startBefore = ""; local.draft.followupStartBefore = "";
  }
  if (previous?.selected_job_id !== projection.selected_job_id) { local.draft.followup = ""; local.draft.assigneeId = ""; local.draft.followupStartBefore = ""; }
  local.projection = projection;
  return true;
}
export function editSharedWork(local: SharedWorkUiState, field: string, value: string): void {
  if (local.pending) return;
  if (field === "draft:editTemplateId") {
    const template = local.projection?.provider?.templates.find(row => row.id === value);
    if (value && !template) return;
    Object.assign(local.draft, { editTemplateId: value, templateId: template?.id ?? "", templateLabel: template?.label ?? "",
      accessMode: template?.access_mode ?? "default", children: template?.allowed_child_environments.join(" ") ?? "" });
    return;
  }
  if (field === "username" || field === "password" || field === "title" || field === "prompt" || field === "environmentId") local[field] = value;
  else if (field.startsWith("draft:")) local.draft[field.slice(6)] = value;
}
export function providerDraftValues(local: SharedWorkPresentation) {
  return { id: local.draft.templateId?.trim() ?? "", label: local.draft.templateLabel?.trim() ?? "",
    access_mode: local.draft.accessMode || "default", allowed_child_environments: (local.draft.children || "").split(/[\s,]+/).filter(Boolean),
    resource_scope: local.draft.resourceScope === "isolated" ? { kind: "workspace_isolation" as const, confirmed: true } : { kind: "device" as const } };
}
/** The native-folder preview remains Rust-owned; changed input must be reviewed again. */
export function providerDraftMatches(local: SharedWorkPresentation): boolean {
  const prepared = local.projection?.provider_draft;
  if (!prepared) return false;
  const draft = providerDraftValues(local);
  return prepared.id === draft.id && prepared.label === draft.label && prepared.access_mode === draft.access_mode
    && JSON.stringify(prepared.allowed_child_environments) === JSON.stringify(draft.allowed_child_environments)
    && (local.projection?.provider?.mode === "shared" || (local.projection?.provider_scope.kind === draft.resource_scope.kind
      && (local.projection.provider_scope.kind === "device" || local.projection.provider_scope.confirmed === true)));
}
export function workStateLabel(state: string): string {
  return ({ queued: "順番待ち", assigned: "実行準備中", running: "実行中", waiting_child: "子の結果待ち", succeeded: "完了", failed: "失敗", cancelling: "取消処理中", cancelled: "取消済み" } as Record<string, string>)[state] ?? state;
}
export function sharedWorkActionEnabled(local: SharedWorkPresentation, kind: string, value: string): boolean {
  if (local.pending || !local.projection) return false;
  const p = local.projection;
  if (local.conceal) return kind === "logout";
  if (kind.startsWith("provider-")) {
    if (["provider-status", "provider-start"].includes(kind)) return true;
    if (kind === "provider-prepare") return p.connected && Boolean(providerDraftValues(local).label);
    if (!p.provider) return false;
    if (kind === "provider-install") return p.connected && providerDraftMatches(local);
    if (kind === "provider-remove-template") return p.provider.templates.some(template => template.id === value);
    if (kind === "provider-reconcile") return Boolean(local.draft.reason?.trim() && local.draft.effects === "yes" && local.draft.stopped === "yes" && p.provider.unknown_attempts.some(a => a.attempt_id === value));
    return true;
  }
  if (kind === "logout") return p.principal !== null;
  if (kind === "login") return p.connected && !p.principal && Boolean(local.username.trim() && local.password);
  if (kind === "reconnect" || kind === "import") return !p.principal;
  if (!p.principal) return false;
  if (kind === "continue") return Boolean(p.detail?.can_continue && local.draft.followup?.trim() && !p.submission_uncertain && !p.submission_storage_error);
  if (kind === "handover") return Boolean(p.handover?.can_handover && p.handover.candidates.some(u => u.user_id === local.draft.assigneeId));
  if (kind === "upload-inputs") return Boolean(p.projects.some(row => row.id === p.selected_project_id && row.can_submit) && !p.submission_uncertain);
  if (kind === "remove-input") return p.inputs.some(a => a.id === value) && !p.submission_uncertain;
  if (kind === "save-asset" || kind === "import-asset") return p.assets.some(a => a.id === value && a.purged_at_ms === null);
  if (kind === "transcript-next") return p.transcript?.next_after !== null && Boolean(p.transcript);
  if (kind === "inbox-next") return Boolean(p.inbox?.next_before);
  if (kind === "inbox-open") return p.inbox?.items.some(item => item.id === value) ?? false;
  if (kind === "project") return p.projects.some(row => row.id === value);
  if (kind === "retry-submission") return p.submission_uncertain && !p.submission_storage_error;
  if (kind === "submit") return Boolean(!p.submission_uncertain && !p.submission_storage_error && p.projects.some(row => row.id === p.selected_project_id && row.can_submit)
    && p.status?.environments.some(row => row.id === local.environmentId && row.enabled)
    && local.title.trim() && local.prompt.trim());
  if (kind === "cancel") return p.status?.jobs.some(row => row.id === value && row.can_cancel) ?? false;
  if (kind === "detail") return p.status?.jobs.some(row => row.id === value) ?? false;
  if (["approve", "deny", "stop"].includes(kind)) return Boolean(p.approval?.can_decide && p.approval.status === "pending" && p.approval.id === value);
  if (kind === "next-jobs") return Boolean(p.status?.next_before);
  if (kind === "next-environments") return Boolean(p.status?.next_environment_before);
  return true;
}

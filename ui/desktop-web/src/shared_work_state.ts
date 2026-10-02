export interface WorkPerson { user_id: string; display_name: string }
export interface WorkApprovalContext {
  job_id: string; project_id: string; root_id: string; conversation_id: string;
  job_title: string; controller_device_id: string | null; controller_device_label: string | null;
  execution_device_id: string; execution_device_label: string | null;
}
export interface WorkAsset { id: string; project_id: string; job_id: string | null; kind: string; name: string; sha256: string; byte_length: number; created_at_ms: number; version: number; base_sha256: string | null; purged_at_ms: number | null }
export interface WorkProject { id: string; label: string; can_submit: boolean; can_execute?: boolean; participation_generation?: number }
export interface WorkConversation { id: string; title: string; latest_job_id: string | null; updated_at_ms: number; revision?: number; delete_pending?: boolean; can_rename?: boolean; can_delete?: boolean }
export interface WorkRunnerContact { last_contact_ms: number | null; state: "unconfirmed" | "recent" | "stale" }
export interface WorkRetainedService { service_id: string; environment_id: string; expires_at_ms: number | null; stop_requested: boolean; uncertain: boolean; can_stop?: boolean }
export interface WorkTranscriptItem { position: number; kind: string; payload: unknown }
export interface WorkActivity {
  job_id: string; attempt_id: string; generation: number; environment_id: string; runner_id: string;
  revision: number; observed_at_ms: number; items: WorkTranscriptItem[]; truncated: boolean;
}
export interface WorkConversationHistory {
  project_id: string;
  conversation_id: string;
  snapshot: number;
  conversation_epoch?: number;
  jobs: { job: WorkSummary; input: unknown; result: unknown; artifacts: WorkAsset[]; more_artifacts: boolean }[];
  next_before: number | null;
}
export interface WorkSummary {
  id: string; root_id: string; parent_id: string | null; conversation_id?: string; origin_session_ref?: string | null; title: string; state: string;
  revises_job_id?: string | null; can_revise?: boolean;
  environment_id: string; environment_label: string; requestor: WorkPerson; assignee: WorkPerson;
  device_label?: string | null;
  wait_reason: string | null; uncertainty_reason: string | null; can_cancel: boolean; can_continue?: boolean;
  conversation_epoch?: number;
  revision: number; created_at_ms: number; updated_at_ms: number;
  start_before_ms?: number | null;
  runner_contact?: WorkRunnerContact | null;
}
export interface WorkEnvironment {
  id: string; label: string; resource_id: string; runner_id: string; enabled: boolean;
  device_label?: string | null;
  capacity: number; occupied: number;
  occupants: { job_id: string; title: string; project_id: string; project_label: string; user: WorkPerson; state: string }[];
  additional_visible_occupants: number; other_occupants: number;
  runner_contact?: WorkRunnerContact | null;
}
export interface WorkExecutionDevice {
  device_id: string; device_label: string | null; environment_id: string | null;
  preparation_state: string; error: string | null;
}
export interface SharedWorkProjection {
  revision: string; generation: string; connected: boolean; hub_url: string; enrollment: string; enrollment_error: string | null;
  projects_stale?: boolean;
  principal: (WorkPerson & { administrator: boolean }) | null; expires_at_ms: number | null;
  projects: WorkProject[]; selected_project_id: string | null; selected_job_id: string | null;
  selected_conversation_id: string | null; conversations: WorkConversation[]; conversation_revision: string;
  leave_pending_project_id?: string | null;
  project_access?: "no_membership" | "device_not_allowed" | "ready" | null;
  status: { project_id: string; jobs: WorkSummary[]; environments: WorkEnvironment[]; execution_devices?: WorkExecutionDevice[]; next_before: string | null; next_environment_before: string | null } | null;
  detail: { id: string; project_id: string; root_id: string; parent_id: string | null; conversation_id?: string; origin_session_ref?: string | null; environment_id: string; title: string; input: unknown; result: unknown; state: string; awaiting_child_id: string | null; revision: number; conversation_epoch?: number; created_at_ms: number; updated_at_ms: number; start_before_ms?: number | null; continued_from_id?: string | null; revises_job_id?: string | null; can_revise?: boolean; can_continue?: boolean; wait_reason?: string | null; uncertainty_reason?: string | null; runner_contact?: WorkRunnerContact | null; retained_services?: WorkRetainedService[] } | null;
  approval: { id: string; attempt_id: string; context?: WorkApprovalContext | null; request: { access: string; summary: string; details: string[]; targets: string[]; outside_workspace: boolean; risks: string[]; agent_path?: string; agent_task_name?: string }; status: string; decision: string | null; expires_at_ms: number; can_decide: boolean; can_reconfirm?: boolean } | null;
  observed_at_ms: number | null; error: string | null; submission_uncertain: boolean; submission_storage_error: string | null;
  feedback: string | null; inputs: WorkAsset[]; assets: WorkAsset[];
  transcript: { items: WorkTranscriptItem[]; next_after: number | null; activity?: WorkActivity | null } | null;
  conversation_history: WorkConversationHistory | null;
  inbox: { items: { id: string; job_id: string; project_id: string; kind: string; title: string; created_at_ms: number; read_at_ms: number | null; can_act: boolean; approval_id: string | null; approval_status?: string | null; approval_decision?: string | null }[]; next_before: string | null; unread_count: number } | null;
}
export interface SharedWorkUiState {
  projection: SharedWorkProjection | null;
  title: string; prompt: string;
  pending: string | null; serial: number; polling: boolean; error: string; conceal: boolean;
  confirmation: { kind: "leave_project" | "delete_conversation"; projectId: string; participationGeneration: number; conversationId: string | null; generation: string; title: string } | null;
  editingConversationId: string | null; renameDraft: string;
  editingJobId: string | null; editingJobRevision: number | null; revisionDraft: string;
  draft: Record<string, string>;
  conversationDrafts: Record<string, { title: string; prompt: string; followup: string; startBefore: string; followupStartBefore: string }>;
}
export type SharedWorkPresentation = Omit<SharedWorkUiState, "serial" | "polling" | "conversationDrafts">;
export interface SharedConversationRow { conversationId: string; title: string; jobId: string | null; selected: boolean; deletePending?: boolean; canRename?: boolean; canDelete?: boolean; }

/** Only the Hub's persisted identity can merge jobs into one visible conversation. */
export function sharedConversationRows(conversations: WorkConversation[], selectedConversationId: string | null, selectedJobId: string | null): SharedConversationRow[] {
  return conversations.map(row => ({
    conversationId: row.id,
    title: row.title,
    jobId: row.latest_job_id,
    selected: row.id === selectedConversationId || Boolean(row.latest_job_id && row.latest_job_id === selectedJobId),
    deletePending: row.delete_pending,
    canRename: row.can_rename,
    canDelete: row.can_delete,
  }));
}
export function createSharedWorkUiState(): SharedWorkUiState {
  return { projection: null, title: "", prompt: "", pending: null, serial: 0, polling: false, error: "", conceal: false, confirmation: null,
    editingConversationId: null, renameDraft: "", editingJobId: null, editingJobRevision: null, revisionDraft: "", draft: {}, conversationDrafts: {} };
}
export function sharedWorkPresentation(local: SharedWorkUiState): SharedWorkPresentation {
  return { projection: local.projection, title: local.title,
    prompt: local.prompt, pending: local.pending, error: local.error, conceal: local.conceal,
    confirmation: local.confirmation, editingConversationId: local.editingConversationId, renameDraft: local.renameDraft,
    editingJobId: local.editingJobId, editingJobRevision: local.editingJobRevision, revisionDraft: local.revisionDraft, draft: local.draft };
}
export function acceptSharedWork(local: SharedWorkUiState, projection: SharedWorkProjection): boolean {
  const previous = local.projection;
  if (previous && BigInt(projection.revision) < BigInt(previous.revision)) return false;
  const previousKey = previous ? sharedDraftKey(previous) : null;
  const nextKey = sharedDraftKey(projection);
  const retiredProjects = new Set((previous?.projects ?? []).filter(project => !projection.projects_stale
    && sharedProjectParticipation(previous!, project.id) !== sharedProjectParticipation(projection, project.id)).map(project => project.id));
  for (const key of Object.keys(local.conversationDrafts)) {
    if (retiredProjects.has(JSON.parse(key)[2])) delete local.conversationDrafts[key];
  }
  if (previous?.generation !== projection.generation) {
    local.title = ""; local.prompt = "";
    local.draft = {};
    local.conversationDrafts = {};
    local.confirmation = null;
    local.editingConversationId = null;
    local.renameDraft = "";
    local.editingJobId = null;
    local.editingJobRevision = null;
    local.revisionDraft = "";
  } else if (previousKey !== nextKey) {
    local.editingConversationId = null;
    local.renameDraft = "";
    local.editingJobId = null;
    local.editingJobRevision = null;
    local.revisionDraft = "";
    if (previousKey && !retiredProjects.has(previous!.selected_project_id ?? "")) local.conversationDrafts[previousKey] = {
      title: local.title, prompt: local.prompt,
      followup: local.draft.followup ?? "", startBefore: local.draft.startBefore ?? "",
      followupStartBefore: local.draft.followupStartBefore ?? "",
    };
    const saved = local.conversationDrafts[nextKey];
    local.title = saved?.title ?? ""; local.prompt = saved?.prompt ?? "";
    local.draft.followup = saved?.followup ?? "";
    local.draft.startBefore = saved?.startBefore ?? "";
    local.draft.followupStartBefore = saved?.followupStartBefore ?? "";
  }
  local.projection = projection;
  if (local.confirmation && (sharedProjectParticipation(projection, local.confirmation.projectId) !== local.confirmation.participationGeneration
    || local.confirmation.generation !== projection.generation)) local.confirmation = null;
  if (local.editingConversationId && !projection.conversations?.some(row => row.id === local.editingConversationId)) {
    local.editingConversationId = null;
    local.renameDraft = "";
  }
  if (local.editingJobId && projection.detail?.id !== local.editingJobId) {
    local.editingJobId = null;
    local.editingJobRevision = null;
    local.revisionDraft = "";
  }
  return true;
}

export function sharedProjectParticipation(projection: SharedWorkProjection, projectId: string | null): number | null {
  const generation = projection.projects.find(project => project.id === projectId)?.participation_generation;
  return Number.isSafeInteger(generation) && (generation ?? 0) > 0 ? generation! : null;
}

function sharedDraftKey(projection: SharedWorkProjection): string {
  const job = projection.status?.jobs.find(row => row.id === projection.selected_job_id);
  const conversationId = projection.detail?.conversation_id
    ?? projection.conversation_history?.conversation_id
    ?? job?.conversation_id
    ?? projection.selected_job_id
    ?? "new";
  return JSON.stringify([projection.generation, projection.principal?.user_id, projection.selected_project_id,
    sharedProjectParticipation(projection, projection.selected_project_id), conversationId]);
}
export function clearSharedCurrentDraft(local: SharedWorkUiState): void {
  if (local.projection) delete local.conversationDrafts[sharedDraftKey(local.projection)];
  local.title = ""; local.prompt = "";
  local.draft.followup = ""; local.draft.startBefore = ""; local.draft.followupStartBefore = "";
}
export function editSharedWork(local: SharedWorkUiState, field: string, value: string): void {
  if (local.pending) return;
  if (field === "title" || field === "prompt") local[field] = value;
  else if (field === "renameTitle") local.renameDraft = value;
  else if (field === "revisionPrompt") local.revisionDraft = value;
  else if (field.startsWith("draft:")) local.draft[field.slice(6)] = value;
}
export function workStateLabel(state: string): string {
  return ({ queued: "順番待ち", assigned: "実行準備中", running: "実行中", waiting_child: "サブエージェントの結果待ち", succeeded: "完了", failed: "失敗", cancelling: "取消処理中", cancelled: "取消済み" } as Record<string, string>)[state] ?? state;
}
export function selectedSharedConversationDeletePending(projection: SharedWorkProjection): boolean {
  const conversationId = projection.selected_conversation_id ?? projection.detail?.conversation_id ?? projection.conversation_history?.conversation_id;
  return projection.conversations?.some(row => row.id === conversationId && row.delete_pending === true) ?? false;
}
export function selectedSharedRequestHasAttachments(projection: SharedWorkProjection): boolean {
  const input = projection.detail?.input;
  if (!input || typeof input !== "object" || Array.isArray(input)) return false;
  return Array.isArray((input as { input_refs?: unknown }).input_refs)
    && (input as { input_refs: unknown[] }).input_refs.length > 0;
}
export function selectedSharedJobIsLatest(projection: SharedWorkProjection): boolean {
  const detail = projection.detail;
  if (!detail) return false;
  const conversationId = projection.selected_conversation_id ?? detail.conversation_id;
  return projection.conversations?.some(row => row.id === conversationId && row.latest_job_id === detail.id) ?? false;
}
export function sharedWorkActionEnabled(local: SharedWorkPresentation, kind: string, value: string): boolean {
  if (local.pending || !local.projection) return false;
  const p = local.projection;
  if (local.conceal) return false;
  if (kind === "reconnect" || kind === "import") return !p.principal;
  if (kind === "handover") return false;
  if (kind === "refresh") return p.connected;
  if (p.projects_stale) return false;
  if (!p.principal) return false;
  const deletionPending = selectedSharedConversationDeletePending(p);
  if (kind === "open-management") return p.principal.administrator;
  if (kind === "prepare-sample") return Boolean(p.projects.some(row => row.id === p.selected_project_id && row.can_submit)
    && !p.selected_job_id && !p.submission_uncertain && !p.submission_storage_error
    && !p.inputs.length && !local.prompt.trim() && !local.title.trim());
  if (kind === "continue") return Boolean(!deletionPending && selectedSharedJobIsLatest(p)
    && !local.editingJobId && p.leave_pending_project_id !== p.selected_project_id
    && p.detail?.can_continue && local.draft.followup?.trim() && !p.submission_uncertain && !p.submission_storage_error);
  if (kind === "upload-inputs") return Boolean(p.projects.some(row => row.id === p.selected_project_id && row.can_submit) && !p.submission_uncertain);
  if (kind === "remove-input") return p.inputs.some(a => a.id === value) && !p.submission_uncertain;
  if (kind === "save-asset" || kind === "import-asset") return p.assets.some(a => a.id === value && a.purged_at_ms === null);
  if (kind === "transcript-next") return p.transcript?.next_after !== null && Boolean(p.transcript);
  if (kind === "history-next") return Boolean(p.conversation_history?.next_before);
  if (kind === "stop-conversation") {
    const detail = p.detail;
    const conversationId = detail?.conversation_id ?? detail?.root_id;
    return Boolean(conversationId === value && p.projects.some(project => project.id === p.selected_project_id && project.can_submit)
      && !p.submission_uncertain && !p.submission_storage_error
      && (p.status?.jobs.some(job => job.conversation_id === conversationId && job.can_cancel)
        || p.conversation_history?.jobs.some(entry => entry.job.can_cancel)
        || detail?.retained_services?.some(service => service.can_stop && !service.stop_requested)));
  }
  if (kind === "inbox-next") return Boolean(p.inbox?.next_before);
  if (kind === "inbox-open") return p.inbox?.items.some(item => item.id === value) ?? false;
  if (kind === "project") return p.projects.some(row => row.id === value);
  if (kind === "leave-project") return sharedProjectParticipation(p, value) !== null
    && p.leave_pending_project_id !== value;
  if (kind === "select-conversation") return p.conversations?.some(row => row.id === value && !row.delete_pending) ?? false;
  if (kind === "request-delete-conversation") return p.conversations?.some(row => row.id === value && row.can_delete !== false && !row.delete_pending) ?? false;
  if (kind === "rename-conversation") return p.conversations?.some(row => row.id === value && row.can_rename !== false && !row.delete_pending) ?? false;
  if (kind === "start-revise") return Boolean(!deletionPending
    && !local.editingJobId && p.detail?.id === value && p.detail.can_revise
    && p.leave_pending_project_id !== p.selected_project_id && p.conversation_history?.jobs[0]?.job.id === value);
  if (kind === "save-revise") return Boolean(!deletionPending
    && local.editingJobId && p.detail?.id === local.editingJobId && p.detail.can_revise
    && p.detail.revision === local.editingJobRevision && local.revisionDraft.trim()
    && p.leave_pending_project_id !== p.selected_project_id && !p.submission_uncertain && !p.submission_storage_error);
  if (kind === "save-rename-conversation") return Boolean(local.editingConversationId
    && p.conversations?.some(row => row.id === local.editingConversationId)
    && local.renameDraft.trim() && local.renameDraft.trim().length <= 256);
  if (kind === "new-conversation") return Boolean(p.selected_project_id && !p.submission_uncertain);
  if (kind === "retry-submission") return p.submission_uncertain && !p.submission_storage_error;
  if (kind === "submit") return Boolean(!deletionPending && !local.editingJobId && p.leave_pending_project_id !== p.selected_project_id
    && !p.submission_uncertain && !p.submission_storage_error && p.projects.some(row => row.id === p.selected_project_id && row.can_submit)
    && (p.status?.environments.some(row => row.enabled) || p.status?.next_environment_before)
    && local.prompt.trim());
  if (kind === "cancel") return Boolean(p.status?.jobs.some(row => row.id === value && row.can_cancel)
    || p.conversation_history?.jobs.some(row => row.job.id === value && row.job.can_cancel));
  if (kind === "stop-service") return p.detail?.retained_services?.some(row => row.service_id === value && row.can_stop && !row.stop_requested) ?? false;
  if (kind === "detail") return p.status?.jobs.some(row => row.id === value) ?? false;
  if (kind === "reconfirm-approval") return Boolean(p.approval?.can_reconfirm && p.approval.status === "expired" && p.approval.id === value);
  if (["approve", "deny", "stop"].includes(kind)) return Boolean(p.approval?.can_decide && p.approval.status === "pending" && p.approval.id === value);
  if (kind === "next-environments") return Boolean(p.status?.next_environment_before);
  return true;
}

import { command } from "./api.ts";
import type { ActionContext } from "./actions.ts";
import type { DeviceNetworkProjection } from "./device_network_state.ts";
import { deviceNetworkError } from "./device_network_state.ts";
import { acceptSharedWork, clearSharedCurrentDraft, selectedSharedConversationDeletePending, selectedSharedJobIsLatest, selectedSharedRequestHasAttachments, sharedWorkActionEnabled, type SharedWorkProjection } from "./shared_work_state.ts";
import { SAMPLE_PROMPT, SAMPLE_TITLE } from "./shared_work_onboarding.ts";

export async function openSharedWork(context: ActionContext): Promise<void> {
  await context.mutate("show_shared_work");
  await refreshSharedWork(context);
}
export async function openHubProject(context: ActionContext, projectId: string): Promise<void> {
  const local = context.uiState.sharedWork;
  if (local.pending || local.conceal || !local.projection?.projects.some(row => row.id === projectId)) return;
  await context.mutate("show_shared_work");
  if (context.getViewState()?.hub_project_open === true) await sharedWorkAction(context, "project", projectId);
}
export async function refreshSharedWork(context: ActionContext): Promise<void> {
  const local = context.uiState.sharedWork;
  if (local.pending || local.polling || local.conceal) return;
  local.polling = true;
  const serial = local.serial;
  try {
    let projection = await command<SharedWorkProjection>("shared_work_projection");
    if (serial !== local.serial) return;
    acceptSharedWork(local, projection);
    if (projection.connected) {
      projection = await command<SharedWorkProjection>("shared_work_command", { expectedGeneration: projection.generation, request: { kind: "refresh" } });
      if (serial !== local.serial) return;
      acceptSharedWork(local, projection);
    }
    local.error = "";
  } catch { if (serial === local.serial) local.error = "Hubのプロジェクトを確認できません。接続の復帰後に自動で更新します。"; }
  finally { local.polling = false; context.rerender(); }
}
export async function sharedWorkAction(context: ActionContext, kind: string, value = ""): Promise<void> {
  const local = context.uiState.sharedWork;
  if ((kind !== "leave_project" && context.getViewState()?.hub_project_open !== true)
    || context.getViewState()?.overlay !== "none" || local.pending) return;
  const projection = local.projection;
  if (!projection) return;
  if (["submit", "continue", "revise"].includes(kind) && selectedSharedConversationDeletePending(projection)) return;
  if (kind === "continue" && !selectedSharedJobIsLatest(projection)) return;
  if (kind === "revise" && selectedSharedRequestHasAttachments(projection)) return;
  if (kind === "leave_project" && !projection.projects.some(row => row.id === value)) return;
  if (["select_conversation", "rename_conversation", "delete_conversation"].includes(kind)
    && !projection.conversations?.some(row => row.id === value)) return;
  if (kind === "prepare_sample" && (local.prompt.trim() || local.title.trim() || projection.inputs.length || projection.selected_job_id)) return;
  let startBeforeMs: number | null = null;
  if (kind === "submit" || kind === "continue") {
    const deadline = local.draft[kind === "submit" ? "startBefore" : "followupStartBefore"];
    if (deadline) {
      startBeforeMs = new Date(deadline).getTime();
      if (!Number.isSafeInteger(startBeforeMs) || startBeforeMs <= Date.now()) {
        local.error = "開始期限には未来の日時を指定してください。";
        context.rerender();
        return;
      }
    }
  }
  const serial = ++local.serial;
  local.pending = kind; local.error = "";
  const request: Record<string, unknown> = { kind };
  if (kind === "project") request.project_id = value;
  if (kind === "leave_project") request.project_id = value;
  if (["new_conversation", "detail", "cancel", "stop_service", "stop_conversation", "next_jobs", "next_environments", "latest", "submit", "continue", "revise", "prepare_sample", "upload_inputs", "remove_input", "save_asset", "import_asset", "transcript_next", "history_next", "handover", "select_conversation", "rename_conversation", "delete_conversation"].includes(kind)) request.project_id = projection.selected_project_id;
  if (["select_conversation", "rename_conversation", "delete_conversation"].includes(kind)) request.conversation_id = value;
  if (kind === "rename_conversation") request.title = local.renameDraft.trim();
  if (kind === "detail" || kind === "cancel") request.job_id = value;
  if (kind === "stop_service") request.service_id = value;
  if (kind === "stop_conversation") request.conversation_id = value;
  if (kind === "history_next") request.conversation_id = projection.conversation_history?.conversation_id;
  if (kind === "submit") {
    Object.assign(request, { title: "", prompt: local.prompt, start_before_ms: startBeforeMs });
  }
  if (["continue", "handover", "save_asset", "import_asset", "transcript_next"].includes(kind)) request.job_id = projection.selected_job_id;
  if (kind === "continue") Object.assign(request, { expected_revision: projection.detail?.revision, prompt: local.draft.followup, start_before_ms: startBeforeMs });
  if (kind === "revise") Object.assign(request, { conversation_id: projection.detail?.conversation_id ?? projection.detail?.root_id,
    job_id: local.editingJobId, expected_revision: local.editingJobRevision, prompt: local.revisionDraft });
  if (kind === "handover") Object.assign(request, { expected_revision: projection.detail?.revision, new_assignee_id: local.draft.assigneeId });
  if (kind === "remove_input") request.asset_id = value;
  if (kind === "save_asset" || kind === "import_asset") Object.assign(request, { kind: "save_asset", asset_id: value, import: kind === "import_asset" });
  if (kind === "inbox_open") request.notification_id = value;
  if (["approve", "deny", "stop"].includes(kind)) Object.assign(request, { kind: "decide", decision: kind, project_id: projection.selected_project_id, job_id: projection.selected_job_id, approval_id: value });
  context.rerender();
  try {
    let result: SharedWorkProjection;
    if (kind === "reconnect" || kind === "import") {
      if (kind === "import") {
        const network = await command<DeviceNetworkProjection>("device_network_projection");
        await command("device_network_import", { expectedRevision: network.revision, expectedGeneration: network.generation });
      } else await command("device_network_refresh");
      result = await command<SharedWorkProjection>("shared_work_projection");
    } else result = await command<SharedWorkProjection>("shared_work_command", { expectedGeneration: projection.generation, request });
    if (serial !== local.serial) return;
    if (kind === "submit" && !result.submission_uncertain && !result.error) {
      // Clear the old new-chat draft before the accepted job changes its owner.
      local.title = ""; local.prompt = ""; local.draft.startBefore = "";
    }
    if (kind === "continue" && !result.submission_uncertain && !result.error) {
      local.draft.followup = ""; local.draft.followupStartBefore = "";
    }
    if (kind === "revise" && !result.submission_uncertain && !result.error) {
      local.editingJobId = null; local.editingJobRevision = null; local.revisionDraft = "";
    }
    acceptSharedWork(local, result);
    if (kind === "prepare_sample" && !result.error && result.generation === projection.generation
      && result.selected_project_id === projection.selected_project_id && !result.selected_job_id
      && result.inputs.some(asset => asset.name === "moyai-sample-numbers.csv")) {
      local.title = SAMPLE_TITLE; local.prompt = SAMPLE_PROMPT;
    }
    if (kind === "new_conversation" && !result.error && result.selected_job_id === null) clearSharedCurrentDraft(local);
    if ((kind === "leave_project" && (result.leave_pending_project_id === value || !result.error))
      || (kind === "delete_conversation" && !result.error)) local.confirmation = null;
    if (kind === "rename_conversation" && !result.error) {
      local.editingConversationId = null;
      local.renameDraft = "";
    }
  } catch (error) {
    if (serial === local.serial) local.error = kind === "import" ? deviceNetworkError(error)
      : "操作を確認できません。接続と最新の状態を確認してください。";
  } finally {
    if (serial === local.serial) { local.pending = null; context.rerender(); }
  }
  if (kind === "reconnect" || kind === "import") await refreshSharedWork(context);
}

export function requestSharedConfirmation(context: ActionContext, kind: "leave_project" | "delete_conversation", value: string): void {
  const local = context.uiState.sharedWork;
  const projection = local.projection;
  if (!projection || local.conceal || local.pending) return;
  const projectId = kind === "leave_project" ? value : projection.selected_project_id;
  const project = projection.projects.find(row => row.id === projectId);
  if (!project || !projectId) return;
  const conversation = kind === "delete_conversation" ? projection.conversations?.find(row => row.id === value) : null;
  if (kind === "delete_conversation" && !conversation) return;
  local.confirmation = { kind, projectId, conversationId: conversation?.id ?? null,
    generation: projection.generation, title: conversation?.title ?? project.label };
  context.rerender();
}

export function cancelSharedConfirmation(context: ActionContext): void {
  context.uiState.sharedWork.confirmation = null;
  context.rerender();
}

export async function confirmSharedConfirmation(context: ActionContext): Promise<void> {
  const local = context.uiState.sharedWork;
  const confirmation = local.confirmation;
  const projection = local.projection;
  if (!confirmation || !projection || confirmation.generation !== projection.generation || local.pending) return;
  if (confirmation.kind === "leave_project") {
    if (!projection.projects.some(row => row.id === confirmation.projectId)) return;
    await sharedWorkAction(context, "leave_project", confirmation.projectId);
  } else if (confirmation.conversationId && projection.selected_project_id === confirmation.projectId
    && projection.conversations?.some(row => row.id === confirmation.conversationId)) {
    await sharedWorkAction(context, "delete_conversation", confirmation.conversationId);
  }
}

export function startSharedRename(context: ActionContext, conversationId: string): void {
  const local = context.uiState.sharedWork;
  const row = local.projection?.conversations?.find(item => item.id === conversationId);
  if (!row || local.pending || context.getViewState()?.hub_project_open !== true) return;
  local.editingConversationId = conversationId;
  local.renameDraft = row.title;
  context.rerender();
  document.querySelector<HTMLInputElement>("#shared-rename-title")?.focus();
}

export function cancelSharedRename(context: ActionContext): void {
  const local = context.uiState.sharedWork;
  local.editingConversationId = null;
  local.renameDraft = "";
  context.rerender();
}

export async function saveSharedRename(context: ActionContext): Promise<void> {
  const local = context.uiState.sharedWork;
  if (!local.editingConversationId || !local.renameDraft.trim()) return;
  await sharedWorkAction(context, "rename_conversation", local.editingConversationId);
}

export function startSharedRevision(context: ActionContext, jobId: string): void {
  const local = context.uiState.sharedWork;
  const p = local.projection;
  if (!p || context.getViewState()?.hub_project_open !== true || !sharedWorkActionEnabled(local, "start-revise", jobId)) return;
  const prompt = p.conversation_history?.jobs.find(row => row.job.id === jobId)?.input;
  if (!prompt || typeof prompt !== "object" || !("prompt" in prompt) || typeof prompt.prompt !== "string") return;
  local.editingJobId = jobId;
  local.editingJobRevision = p.detail!.revision;
  local.revisionDraft = prompt.prompt;
  context.rerender();
  document.querySelector<HTMLTextAreaElement>("#shared-revise-prompt")?.focus();
}

export function cancelSharedRevision(context: ActionContext): void {
  const local = context.uiState.sharedWork;
  local.editingJobId = null; local.editingJobRevision = null; local.revisionDraft = "";
  context.rerender();
}

export async function saveSharedRevision(context: ActionContext): Promise<void> {
  const local = context.uiState.sharedWork;
  if (!local.editingJobId || !sharedWorkActionEnabled(local, "save-revise", "")) return;
  await sharedWorkAction(context, "revise", local.editingJobId);
}

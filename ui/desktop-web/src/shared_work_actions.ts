import { command } from "./api.ts";
import type { ActionContext } from "./actions.ts";
import type { DeviceNetworkProjection } from "./device_network_state.ts";
import { deviceNetworkError } from "./device_network_state.ts";
import { acceptSharedWork, type SharedWorkProjection } from "./shared_work_state.ts";
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
  if (context.getViewState()?.hub_project_open !== true || context.getViewState()?.overlay !== "none" || local.pending) return;
  const projection = local.projection;
  if (!projection) return;
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
  if (kind === "logout") { local.conceal = true; local.password = ""; local.setupCode = ""; local.setupPasswordConfirm = ""; local.prompt = ""; local.title = ""; }
  const request: Record<string, unknown> = { kind };
  if (kind === "login" || kind === "setup_password") {
    if (kind === "setup_password") request.code = local.setupCode.trim();
    request.username = local.username; request.password = local.password; local.password = "";
    local.setupCode = ""; local.setupPasswordConfirm = "";
    for (const id of ["shared-password", "shared-setup-code", "shared-setup-confirm"]) {
      const input = document.querySelector<HTMLInputElement>(`#${id}`);
      if (input) input.value = "";
    }
  }
  if (kind === "project") request.project_id = value;
  if (["new_conversation", "detail", "cancel", "next_jobs", "next_environments", "latest", "submit", "continue", "prepare_sample", "upload_inputs", "remove_input", "save_asset", "import_asset", "transcript_next", "handover"].includes(kind)) request.project_id = projection.selected_project_id;
  if (kind === "detail" || kind === "cancel") request.job_id = value;
  if (kind === "submit") Object.assign(request, { title: local.title, prompt: local.prompt, environment_id: local.environmentId, start_before_ms: startBeforeMs });
  if (["continue", "handover", "save_asset", "import_asset", "transcript_next"].includes(kind)) request.job_id = projection.selected_job_id;
  if (kind === "continue") Object.assign(request, { expected_revision: projection.detail?.revision, prompt: local.draft.followup, start_before_ms: startBeforeMs });
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
    acceptSharedWork(local, result);
    if (kind === "setup_password" && !result.principal && result.error) {
      local.error = `${result.error} 応答を受け取れなかった場合は、設定したパスワードで通常ログインを確認してください。コードの再発行は管理者へ依頼できます。`;
    }
    if (kind === "prepare_sample" && !result.error && result.generation === projection.generation
      && result.selected_project_id === projection.selected_project_id && !result.selected_job_id
      && result.inputs.some(asset => asset.name === "moyai-sample-numbers.csv")) {
      local.title = SAMPLE_TITLE; local.prompt = SAMPLE_PROMPT;
    }
    if (kind === "new_conversation" && !result.error && result.selected_job_id === null) {
      local.title = ""; local.prompt = ""; local.draft.startBefore = "";
      local.draft.followup = ""; local.draft.followupStartBefore = "";
    }
    if (kind === "submit" && !result.submission_uncertain && !result.error) { local.title = ""; local.prompt = ""; local.draft.startBefore = ""; }
    if (kind === "continue" && !result.submission_uncertain && !result.error) { local.draft.followup = ""; local.draft.followupStartBefore = ""; }
    if (kind === "logout") local.conceal = false;
  } catch (error) {
    if (serial === local.serial) local.error = kind === "logout" ? "ログアウトを確認できません。再度ログアウトしてください。"
      : kind === "setup_password" ? "本人設定の結果を確認できません。設定したパスワードで通常ログインを確認してください。ログインできない場合は管理者にコードの再発行を依頼してください。"
      : kind === "import" ? deviceNetworkError(error)
      : "操作を確認できません。接続と最新の状態を確認してください。";
  } finally {
    if (serial === local.serial) { local.pending = null; context.rerender(); }
  }
  if (kind === "login" || (kind === "setup_password" && local.projection?.principal) || kind === "reconnect" || kind === "import") await refreshSharedWork(context);
}

import { command } from "./api.ts";
import type { ActionContext } from "./actions.ts";
import type { DeviceNetworkProjection } from "./device_network_state.ts";
import { acceptSharedWork, providerDraftMatches, providerDraftValues, type SharedWorkProjection } from "./shared_work_state.ts";

export async function openSharedWork(context: ActionContext): Promise<void> {
  await context.mutate("show_shared_work");
  await refreshSharedWork(context);
}
export async function refreshSharedWork(context: ActionContext): Promise<void> {
  const local = context.uiState.sharedWork;
  if (context.getViewState()?.overlay !== "shared_work" || local.pending || local.polling || local.conceal) return;
  local.polling = true;
  const serial = local.serial;
  try {
    let projection = await command<SharedWorkProjection>("shared_work_projection");
    if (serial !== local.serial || context.getViewState()?.overlay !== "shared_work") return;
    acceptSharedWork(local, projection);
    if (projection.principal) {
      projection = await command<SharedWorkProjection>("shared_work_command", { expectedGeneration: projection.generation, request: { kind: "refresh" } });
      if (serial !== local.serial || context.getViewState()?.overlay !== "shared_work") return;
      acceptSharedWork(local, projection);
    }
  } catch { if (serial === local.serial) local.error = "共有仕事の状態を確認できません。更新してください。"; }
  finally { local.polling = false; context.rerender(); }
}
export async function sharedWorkAction(context: ActionContext, kind: string, value = ""): Promise<void> {
  const local = context.uiState.sharedWork;
  if (context.getViewState()?.overlay !== "shared_work" || local.pending) return;
  const projection = local.projection;
  if (!projection) return;
  if (kind === "provider_install" && !providerDraftMatches(local)) {
    local.error = "設定が変わっています。保存先フォルダを選び直し、内容を確認してから公開してください。";
    context.rerender(); return;
  }
  if (kind === "provider_prepare" && !local.draft.templateId?.trim()) {
    // The identifier is a stable editable draft, never a new permission or a published setting.
    local.draft.templateId = `folder-${crypto.randomUUID()}`;
  }
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
  if (kind === "logout") { local.conceal = true; local.password = ""; local.prompt = ""; local.title = ""; }
  const request: Record<string, unknown> = { kind };
  if (kind === "login") {
    request.username = local.username; request.password = local.password; local.password = "";
    const password = document.querySelector<HTMLInputElement>("#shared-password");
    if (password) password.value = "";
  }
  if (kind === "project") request.project_id = value;
  if (["detail", "cancel", "next_jobs", "next_environments", "latest", "submit", "continue", "upload_inputs", "remove_input", "save_asset", "import_asset", "transcript_next", "handover"].includes(kind)) request.project_id = projection.selected_project_id;
  if (kind === "detail" || kind === "cancel") request.job_id = value;
  if (kind === "submit") Object.assign(request, { title: local.title, prompt: local.prompt, environment_id: local.environmentId, start_before_ms: startBeforeMs });
  if (["continue", "handover", "save_asset", "import_asset", "transcript_next"].includes(kind)) request.job_id = projection.selected_job_id;
  if (kind === "continue") Object.assign(request, { expected_revision: projection.detail?.revision, prompt: local.draft.followup, start_before_ms: startBeforeMs });
  if (kind === "handover") Object.assign(request, { expected_revision: projection.detail?.revision, new_assignee_id: local.draft.assigneeId });
  if (kind === "remove_input") request.asset_id = value;
  if (kind === "save_asset" || kind === "import_asset") Object.assign(request, { kind: "save_asset", asset_id: value, import: kind === "import_asset" });
  if (kind === "inbox_open") request.notification_id = value;
  if (kind === "provider_prepare") Object.assign(request, providerDraftValues(local));
  if (kind === "provider_install") request.runner_id = projection.provider?.runner_id;
  if (kind === "provider_remove_template") Object.assign(request, { runner_id: projection.provider?.runner_id, template_id: value });
  if (kind.startsWith("provider_") && !["provider_status", "provider_start", "provider_prepare", "provider_install", "provider_remove_template"].includes(kind)) {
    const operation: Record<string, unknown> = { operation: kind.slice("provider_".length) };
    if (kind === "provider_autostart") operation.operation = "install_autostart";
    if (kind === "provider_no_autostart") operation.operation = "remove_autostart";
    if (kind === "provider_maintenance") operation.until_ms = local.draft.maintenanceUntil ? new Date(local.draft.maintenanceUntil).getTime() : null;
    if (kind === "provider_provision") Object.assign(operation, { template_id: local.draft.provisionTemplate, environment_id: local.draft.provisionEnvironment });
    if (kind === "provider_reconcile") Object.assign(operation, { operation: "reconcile_unknown", attempt_id: value,
      generation: projection.provider?.unknown_attempts.find(a => a.attempt_id === value)?.generation, reason: local.draft.reason,
      evidence: { kind: "operator_confirmed_stopped", effects_reviewed: local.draft.effects === "yes", processes_stopped: local.draft.stopped === "yes" } });
    Object.assign(request, { kind: "provider_operation", runner_id: projection.provider?.runner_id, operation });
  }
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
    if (kind === "submit" && !result.submission_uncertain && !result.error) { local.title = ""; local.prompt = ""; local.draft.startBefore = ""; }
    if (kind === "continue" && !result.submission_uncertain && !result.error) { local.draft.followup = ""; local.draft.followupStartBefore = ""; }
    if (kind === "logout") local.conceal = false;
  } catch {
    if (serial === local.serial) local.error = kind === "logout" ? "ログアウトを確認できません。再度ログアウトしてください。" : "操作を確認できません。接続と最新の状態を確認してください。";
  } finally {
    if (serial === local.serial) { local.pending = null; context.rerender(); }
  }
  if (kind === "login" || kind === "reconnect") await refreshSharedWork(context);
}

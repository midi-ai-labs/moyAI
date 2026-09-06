import { command } from "./api.ts";
import type { ActionContext } from "./actions.ts";
import { clearPublishSecret, revealPublishEditorStart } from "./mcp_publish_dom.ts";
import {
  acceptPublishProjection, discardPublishDraft, newPublishProfile, publishCanOperate,
  publishCanSave, publishDraftValue, publishEditor, publishErrorText, publishRow,
  publishCanCreateCertificate, publishCanStopJob,
  selectPublishProfile, NEW_PUBLISH_PROFILE,
  type PublishOperation, type PublishProfileOperation, type PublishProjection, type PublishJob, type PublishTls,
} from "./mcp_publish_state.ts";

interface PublishTokenResult { projection: PublishProjection; profile_id: string; token: string }

async function publishRequest(context: ActionContext, operation: PublishOperation,
  args: Record<string, unknown> = {}): Promise<void> {
  const local = context.uiState.mcpPublish;
  if (local.pending || context.getViewState()?.overlay !== "mcp_publish") return;
  const serial = ++local.requestSerial;
  const selectedId = local.selectedId;
  const beforeIds = new Set(local.projection?.profiles.map((row) => row.profile.id) ?? []);
  local.pending = operation;
  local.error = "";
  local.notice = "";
  context.rerender();
  let issued: PublishTokenResult | null = null;
  try {
    const result = await command<PublishProjection | PublishTokenResult>(
      operation === "load" ? "mcp_publish_projection" : `mcp_publish_${operation}`, args);
    if (serial !== local.requestSerial || context.getViewState()?.overlay !== "mcp_publish") return;
    const projection = "projection" in result ? result.projection : result;
    const savedId = operation === "save"
      ? selectedId === NEW_PUBLISH_PROFILE
        ? projection.profiles.find((row) => !beforeIds.has(row.profile.id))?.profile.id
        : selectedId ?? undefined
      : undefined;
    if (acceptPublishProjection(local, projection, savedId)) {
      if (operation === "delete" && selectedId) {
        delete local.drafts[selectedId];
        local.selectedId = projection.profiles[0]?.profile.id ?? null;
      }
      local.deleteConfirmation = null;
      local.notice = operation === "save" ? "設定を保存しました。配信は開始していません。"
        : operation === "delete" ? "配信プロファイルを削除しました。"
          : operation === "issue_token" ? "トークンを発行しました。この画面でコピーしてください。"
            : operation === "revoke_token" ? "トークンを失効しました。以前のトークンでは接続できません。" : "";
      if ("token" in result && operation === "issue_token") issued = result;
    }
    if (operation === "delete" || operation === "revoke_token") clearPublishSecret();
  } catch (error) {
    if (serial !== local.requestSerial || context.getViewState()?.overlay !== "mcp_publish") return;
    if (operation !== "load") {
      try {
        const projection = await command<PublishProjection>("mcp_publish_projection");
        if (serial === local.requestSerial && context.getViewState()?.overlay === "mcp_publish") {
          acceptPublishProjection(local, projection);
        }
      } catch { /* Keep the original operation failure and its editable draft. */ }
    }
    if (serial === local.requestSerial && context.getViewState()?.overlay === "mcp_publish") local.error = publishErrorText(error);
  } finally {
    if (serial === local.requestSerial) {
      local.pending = null;
      context.rerender();
    }
  }
  if (issued && serial === local.requestSerial && local.selectedId === issued.profile_id
    && context.getViewState()?.overlay === "mcp_publish") {
    const input = document.querySelector<HTMLInputElement>("#mcp-publish-token");
    if (input) input.value = issued.token;
  }
}

export async function openPublish(context: ActionContext): Promise<void> {
  await context.mutate("show_mcp_publish_editor");
  await publishRequest(context, "load");
  await refreshPublishJobs(context);
}
export async function refreshPublish(context: ActionContext): Promise<void> {
  await publishRequest(context, "load");
  await refreshPublishJobs(context);
}
/** The ordinary Desktop polling owner calls this read while the publish dialog is visible. */
export async function refreshPublishJobs(context: ActionContext): Promise<void> {
  const local = context.uiState.mcpPublish;
  if (context.getViewState()?.overlay !== "mcp_publish" || local.pending === "cancel_job") return;
  const serial = ++local.jobsSerial;
  try {
    const jobs = await command<PublishJob[]>("mcp_publish_jobs");
    if (serial !== local.jobsSerial || context.getViewState()?.overlay !== "mcp_publish") return;
    local.jobs = jobs;
    local.jobsError = "";
  } catch {
    if (serial !== local.jobsSerial || context.getViewState()?.overlay !== "mcp_publish") return;
    local.jobsError = "受け付けたタスクの状態を取得できません。表示は最後に確認した状態です。";
  }
  context.rerender();
}
export async function stopPublishJob(context: ActionContext, jobId: string): Promise<void> {
  const local = context.uiState.mcpPublish;
  if (context.getViewState()?.overlay !== "mcp_publish" || !publishCanStopJob(local, jobId)) return;
  const job = local.jobs.find((candidate) => candidate.job_id === jobId && candidate.profile_id === local.selectedId)!;
  const serial = ++local.requestSerial;
  ++local.jobsSerial;
  local.pending = "cancel_job";
  local.error = "";
  context.rerender();
  try {
    const updated = await command<PublishJob>("mcp_publish_cancel_job", { profileId: job.profile_id, jobId: job.job_id });
    if (serial !== local.requestSerial || context.getViewState()?.overlay !== "mcp_publish") return;
    if (updated.job_id === job.job_id && updated.profile_id === job.profile_id) {
      local.jobs = local.jobs.map((candidate) => candidate.job_id === job.job_id ? updated : candidate);
    }
    local.notice = "停止を要求しました。実行が終了するまで状態を確認してください。";
  } catch (error) {
    if (serial === local.requestSerial) local.error = publishErrorText(error);
  } finally {
    if (serial === local.requestSerial) { local.pending = null; context.rerender(); }
  }
}

interface PublishCertificateReceipt { tls: PublishTls; certificate_pem: string; sha256: string }
export async function publishCertificate(context: ActionContext, create: boolean): Promise<void> {
  const local = context.uiState.mcpPublish;
  const row = publishRow(local);
  const editor = publishEditor(local);
  if (context.getViewState()?.overlay !== "mcp_publish" || !row || !editor || local.pending
    || (create ? !publishCanCreateCertificate(local) : !row.profile.tls)) return;
  const draftAtStart = JSON.stringify(publishDraftValue(editor));
  const profileAtStart = JSON.stringify(row.profile);
  const serial = ++local.requestSerial;
  local.pending = create ? "create_certificate" : "certificate";
  local.error = "";
  local.notice = "";
  context.rerender();
  try {
    const receipt = await command<PublishCertificateReceipt>(create ? "mcp_publish_create_certificate" : "mcp_publish_certificate",
      create ? { id: row.profile.id, bindIp: editor.host, revision: local.projection!.revision, generation: local.projection!.generation }
        : { id: row.profile.id, revision: local.projection!.revision, generation: local.projection!.generation });
    if (serial !== local.requestSerial || local.selectedId !== row.profile.id || context.getViewState()?.overlay !== "mcp_publish") return;
    const currentEditor = publishEditor(local);
    if (!currentEditor || JSON.stringify(publishRow(local)?.profile) !== profileAtStart
      || JSON.stringify(publishDraftValue(currentEditor)) !== draftAtStart) {
      local.error = "証明書の作成中に設定が変わりました。現在の設定を確認して再操作してください。";
      return;
    }
    if (create) {
      currentEditor.value.tls = receipt.tls;
      local.notice = "証明書を作成しました。「設定を保存」で反映してから、公開証明書を接続先へ渡してください。";
    } else {
      await navigator.clipboard.writeText(receipt.certificate_pem);
      if (serial === local.requestSerial && context.getViewState()?.overlay === "mcp_publish") {
        local.notice = `公開証明書をコピーしました。SHA-256: ${receipt.sha256}`;
      }
    }
  } catch (error) {
    if (serial === local.requestSerial && context.getViewState()?.overlay === "mcp_publish") local.error = publishErrorText(error);
  } finally {
    if (serial === local.requestSerial) { local.pending = null; context.rerender(); }
  }
}
export function addPublish(context: ActionContext): void {
  if (!newPublishProfile(context.uiState.mcpPublish)) return;
  clearPublishSecret();
  revealPublishEditorStart();
  context.rerender();
}
export function choosePublish(context: ActionContext, id: string): void {
  clearPublishSecret();
  selectPublishProfile(context.uiState.mcpPublish, id);
  context.rerender();
}
export function discardPublish(context: ActionContext): void {
  discardPublishDraft(context.uiState.mcpPublish);
  context.rerender();
}
export async function savePublish(context: ActionContext): Promise<void> {
  const state = context.uiState.mcpPublish;
  if (!publishCanSave(state) || !state.projection) return;
  const editor = publishEditor(state)!;
  await publishRequest(context, "save", {
    profileId: state.selectedId === NEW_PUBLISH_PROFILE ? null : state.selectedId,
    draft: publishDraftValue(editor),
    expectedRevision: state.selectedId === NEW_PUBLISH_PROFILE ? state.projection.revision : editor.revision,
    expectedGeneration: state.projection.generation,
  });
}
export async function operatePublish(context: ActionContext,
  operation: PublishProfileOperation): Promise<void> {
  const state = context.uiState.mcpPublish;
  if (!publishCanOperate(state, operation) || !state.projection) return;
  if (operation === "delete" && state.deleteConfirmation !== state.selectedId) {
    state.deleteConfirmation = state.selectedId;
    context.rerender();
    return;
  }
  await publishRequest(context, operation, { profileId: state.selectedId,
    expectedRevision: state.projection.revision, expectedGeneration: state.projection.generation });
}
export function cancelPublishDelete(context: ActionContext): void {
  context.uiState.mcpPublish.deleteConfirmation = null;
  context.rerender();
}
export async function copyPublish(context: ActionContext, configuration: boolean): Promise<void> {
  const state = context.uiState.mcpPublish;
  const row = publishRow(state);
  const serial = state.requestSerial;
  const token = document.querySelector<HTMLInputElement>("#mcp-publish-token")?.value ?? "";
  if (!row || state.pending) return;
  if (!token) {
    state.error = "トークンは再表示できません。再発行してからコピーしてください。";
    context.rerender();
    return;
  }
  if (configuration && !row.endpoint) {
    state.error = "配信を開始して接続先URLを確認してください。";
    context.rerender();
    return;
  }
  const value = configuration ? JSON.stringify({ mcpServers: { [row.profile.label]: {
    type: "http", url: row.endpoint, headers: { Authorization: `Bearer ${token}` },
  } } }, null, 2) : token;
  try {
    await navigator.clipboard.writeText(value);
    if (serial === state.requestSerial && state.selectedId === row.profile.id && context.getViewState()?.overlay === "mcp_publish") {
      state.notice = configuration ? "接続側の設定例をコピーしました。貼り付け先の形式に合わせてください。" : "トークンをコピーしました。";
      state.error = "";
    }
  } catch {
    if (serial === state.requestSerial && state.selectedId === row.profile.id && context.getViewState()?.overlay === "mcp_publish") {
      state.error = "コピーできませんでした。トークン欄を選択して手動でコピーしてください。";
    }
  }
  context.rerender();
}

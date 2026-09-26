import { escapeHtml } from "./utils.ts";
import { renderMarkdown } from "./markdown.ts";
import { renderConversationInput, renderConversationMain } from "./conversation_surface.ts";
import { renderTranscriptRows } from "./render_transcript.ts";
import { icon } from "./icons.ts";
import { renderSharedWorkOnboarding } from "./shared_work_onboarding.ts";
import { deviceNetworkError } from "./device_network_state.ts";
import { selectedSharedConversationDeletePending, selectedSharedJobIsLatest, selectedSharedRequestHasAttachments, sharedWorkActionEnabled, workStateLabel, type SharedWorkPresentation, type WorkExecutionDevice, type WorkRunnerContact } from "./shared_work_state.ts";
import { renderWorkDetails, renderWorkInbox, renderWorkInputs, renderWorkResult, workRecordOwner } from "./shared_work_details.ts";
import type { TranscriptRow } from "./types.ts";

const esc = (value: unknown) => escapeHtml(String(value ?? ""));
const button = (action: string, label: string, value = "", disabled = false) => `<button data-action="shared-${action}" data-value="${esc(value)}" ${disabled ? "disabled" : ""}>${esc(label)}</button>`;
function contact(value?: WorkRunnerContact | null): string {
  const label = value?.state === "recent" ? "PCからの応答あり" : "PCの応答なし（状態不明）";
  return `<span class="shared-secondary" data-runner-contact="${esc(value?.state ?? "unconfirmed")}">${label} · 最終応答: ${value?.last_contact_ms != null ? esc(new Date(value.last_contact_ms).toLocaleString("ja-JP")) : "未確認"}</span>`;
}
function preparation(device: WorkExecutionDevice): string {
  const label = ({ waiting_setup: "このPCの初回設定待ち", pending: "作業フォルダーの登録待ち", failed: "作業フォルダーを登録できませんでした", ready: "作業フォルダー登録済み（AIの動作は未確認）" } as Record<string, string>)[device.preparation_state] ?? "実行設定を読み込み中";
  return `<p>${label}</p>${device.preparation_state === "waiting_setup" ? `<p class="shared-secondary">「${esc(device.device_label ?? device.device_id)}」でmoyAI Hubの設定を開き、仕事の実行を許可してください。</p>` : ""}${device.error ? `<p class="shared-error">${esc(device.error)}</p>` : ""}`;
}
function executionStatus(local: SharedWorkPresentation): string {
  const status = local.projection?.status;
  if (!status) return "<p>PCの利用状況を確認しています。</p>";
  const devices = status.execution_devices ?? [];
  const environments = status.environments.map(env => {
    const device = devices.find(row => row.environment_id === env.id);
    return `<article class="shared-environment"><h3>${esc(env.device_label ?? env.label)}</h3>${device && device.preparation_state !== "ready" ? preparation(device) : ""}<p>${env.occupied} / ${env.capacity} 枠を使用中${env.enabled ? "" : " · 準備中または受付停止"}</p>${contact(env.runner_contact)}${env.occupants.map(o => `<p>${esc(o.user.display_name)} · ${esc(o.project_label)} · ${esc(o.title)}（${esc(workStateLabel(o.state))}）</p>`).join("")}${env.other_occupants ? `<p>ほかに ${env.other_occupants} 枠使用中（起動中のアプリを含む）</p>` : ""}${env.additional_visible_occupants ? `<p>ほかに ${env.additional_visible_occupants} 件の実行があります。</p>` : ""}</article>`;
  }).join("");
  const otherDevices = devices.filter(device => !status.environments.some(env => env.id === device.environment_id)).map(device => `<article class="shared-environment"><h3>${esc(device.device_label ?? device.device_id)}</h3>${preparation(device)}${device.preparation_state === "ready" ? "<p class=\"shared-secondary\">利用状況は別のページに表示されています。</p>" : ""}</article>`).join("");
  return environments + otherDevices || "<p>管理者が実行するPCを割り当てると、ここに表示されます。</p>";
}

function object(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}
function messageRow(kind: "user" | "assistant" | "error", body: string, identity: string): TranscriptRow {
  return { row_kind: kind, stable_history_identity: identity, step: "", title: kind === "error" ? "仕事を完了できませんでした" : "", body, file_changes: [] };
}
function historyAnswer(result: unknown): { kind: "assistant" | "error"; text: string } | null {
  if (result == null) return null;
  if (typeof result === "string") return { kind: "assistant", text: result };
  const value = object(result);
  const outcome = object(object(object(value.summary).terminal).outcome);
  const failure = typeof value.error === "string" ? value.error : outcome.kind === "failed" && typeof outcome.error === "string" ? outcome.error : "";
  const answer = typeof value.text === "string" ? value.text : typeof value.message === "string" ? value.message : "";
  if (failure) return { kind: "error", text: `${failure}${typeof value.text === "string" && value.text !== failure ? `\n\n中断までの回答: ${value.text}` : ""}` };
  return answer ? { kind: "assistant", text: answer } : null;
}

/** Hub and local chats share the visible conversation shell and message renderer. */
export function renderHubConversation(local: SharedWorkPresentation, approvalMarkup: string, receiverActivity = ""): string {
  const p = local.projection, person = local.conceal ? null : p?.principal;
  const project = person ? p?.projects.find(row => row.id === p.selected_project_id) : null;
  const detail = person ? p?.detail : null, busy = Boolean(local.pending);
  const status = person ? p?.status : null;
  const owner = `${p?.generation ?? "loading"}:${person?.user_id ?? "anonymous"}:${project?.id ?? ""}:${detail?.conversation_id ?? detail?.id ?? ""}:${local.conceal}:${p?.connected}:${project?.can_submit}`;
  const error = local.error || (!local.conceal ? p?.error || p?.submission_storage_error || (p?.enrollment_error ? deviceNetworkError(p.enrollment_error) : null) : null);
  const recordOwner = p ? workRecordOwner(local) : "";
  const environments = status?.environments.filter(row => row.enabled) ?? [];
  const selectedJob = status?.jobs.find(row => row.id === detail?.id)
    ?? p?.conversation_history?.jobs.find(row => row.job.id === detail?.id)?.job;
  const projectAccessHint = "管理者に、このPCをプロジェクトの操作PCへ追加するよう依頼してください。";
  const connection = `<section data-shared-region="connection" class="shared-card"><h2>${p?.connected ? "このPCの利用登録を確認" : "このPCをHubに接続"}</h2>${!p?.connected ? `<p>管理者から受け取った接続ファイル（.moyai-join または .toml）を読み込み、このPCの参加を申請してください。</p>${button("import", "Hubの設定ファイルを読み込む", "", busy)}${button("reconnect", "再接続", "", busy)}` : `<p>Hubに接続しています。管理者がこのPCを承認し、プロジェクトの操作PCに指定すると、そのまま利用できます。</p>${button("refresh", "利用できるプロジェクトを確認", "", busy)}`}</section>`;
  const fallbackRecord = detail ? `<section data-shared-region="detail" data-shared-record-owner="${esc(recordOwner)}" class="shared-card shared-conversation-record"><p>Hubの会話履歴を読み込めません。選択した仕事の記録を表示しています。</p><h2>${esc(detail.title)}</h2><p>${esc(workStateLabel(detail.state))}${selectedJob ? ` · ${esc(selectedJob.requestor.display_name)} → ${esc(selectedJob.device_label ?? selectedJob.environment_label)}` : ""}</p><div data-shared-runner-observation>${contact(detail.runner_contact)}</div>${detail.wait_reason ? `<p>${esc(detail.wait_reason)}</p>` : ""}${detail.uncertainty_reason ? `<p class="shared-error">${esc(detail.uncertainty_reason)}</p>` : ""}<div class="markdown-body shared-user-message">${renderMarkdown(typeof object(detail.input).prompt === "string" ? String(object(detail.input).prompt) : "")}</div>${renderWorkResult(detail.result, recordOwner, Boolean(detail.can_continue && p && selectedSharedJobIsLatest(p)))}${detail.start_before_ms != null ? `<details data-details-key="hub-job-deadline"><summary>実行の設定</summary><p>開始期限: ${esc(new Date(detail.start_before_ms).toLocaleString("ja-JP"))}</p></details>` : ""}${selectedJob?.can_cancel ? button("cancel", "実行を停止", detail.id, busy) : ""}</section>` : "";
  const history = `<div data-shared-region="history-container" data-shared-record-owner="${esc(owner)}">${p?.conversation_history ? renderConversationHistory(local, detail?.id ?? null) : fallbackRecord || '<div class="empty-thread"><h2>新しいチャット</h2><p>このプロジェクトで行いたいことを入力してください。</p></div>'}</div>`;
  const composing = Boolean(project?.can_submit);
  const followup = Boolean(detail);
  const field = followup ? "draft:followup" : "prompt";
  const prompt = followup ? local.draft.followup ?? "" : local.prompt;
  const leavePending = Boolean(project?.id && p?.leave_pending_project_id === project.id);
  const deletionPending = p ? selectedSharedConversationDeletePending(p) : false;
  const canSend = !busy && !leavePending && !deletionPending && !p?.projects_stale && !p?.submission_uncertain && !p?.submission_storage_error
    && Boolean(prompt.trim()) && (followup ? Boolean(detail?.can_continue && p && selectedSharedJobIsLatest(p)) : environments.length > 0 || Boolean(status?.next_environment_before));
  const followupHint = detail && p && !selectedSharedJobIsLatest(p) ? "最新の仕事を開くと追加の依頼を送れます"
    : detail && !detail.can_continue ? "前の仕事が終わると追加の依頼を送れます" : "画面を閉じても実行は続きます";
  const composer = composing && !local.editingJobId ? `<section class="composer shared-composer" data-shared-region="${followup ? "followup" : "draft"}">
    ${renderWorkInputs(local)}
    ${renderConversationInput({ id: followup ? "shared-followup" : "shared-prompt", value: prompt, label: "moyAIへの依頼", placeholder: "moyAI に依頼する", attributes: `data-shared-field="${field}" ${busy ? "disabled" : ""}` })}
    <div class="composer-actions"><button class="add-button icon-only" data-action="shared-upload-inputs" title="ファイルを添付" aria-label="ファイルを添付" ${busy || p?.submission_uncertain ? "disabled" : ""}>${icon("plus")}</button><button class="send composer-text-action" data-action="send" title="送信" aria-label="送信" ${canSend ? "" : "disabled"}><span>送信</span>${icon("send")}</button></div>
    <div class="composer-meta"><span>プロジェクトで共有</span><span>${esc(followupHint)}</span></div>
  </section>` : "";
  const editing = p?.conversations?.find(row => row.id === local.editingConversationId);
  const rename = editing ? `<div class="shared-conversation-rename"><label for="shared-rename-title">チャット名</label><input id="shared-rename-title" data-shared-field="renameTitle" maxlength="256" value="${esc(local.renameDraft)}"><button data-action="shared-save-rename-conversation">保存</button><button data-action="shared-cancel-rename-conversation">戻る</button></div>` : "";
  const topbar = `<header data-shared-region="account" class="topbar shared-header"><div class="title-copy"><h1 id="shared-heading">${esc(p?.selected_conversation_id ? p.conversations?.find(row => row.id === p.selected_conversation_id)?.title ?? detail?.title : detail?.title ?? project?.label ?? "Hubのプロジェクト")}</h1><p>${person ? `${esc(project?.label ?? person.display_name)}${project ? " · プロジェクトで共有" : " · プロジェクトの割り当てを待っています"}` : "このPCの参加と用途を確認してください"}</p>${rename}</div><div class="shared-actions">${project ? button("new-conversation", "新しいチャット", "", busy) : ""}<details data-details-key="hub-project-account"><summary>接続</summary><p>${esc(p?.hub_url)}</p>${person?.administrator ? button("open-management", "Hubの管理画面を開く", "", busy) : ""}<button data-action="show-hub">このPCの設定</button></details></div></header>`;
  const actionableApprovals = person && project ? (p?.inbox?.items ?? []).filter(item =>
    item.kind === "approval" && item.can_act && item.project_id === project.id && item.approval_id !== p?.approval?.id) : [];
  const activity = `<div data-shared-region="receiver-activity">${receiverActivity}</div><div data-shared-region="message" class="shared-work-message ${person && (p?.approval?.status === "pending" || actionableApprovals.length) ? "has-approval" : ""}" role="status">${person && p?.approval?.status === "pending" ? `<p class="shared-approval-notice">${p.approval.can_decide ? "あなたの承認を待っています。" : "担当者の承認を待っています。"} <a href="#shared-approval-title">承認内容と操作ボタンへ</a></p>` : ""}${actionableApprovals.map(item => `<p class="shared-approval-notice">承認が必要な仕事があります。${button("inbox-open", `承認内容を開く: ${item.title}`, item.id, busy)}</p>`).join("")}${!p?.connected ? `<p data-settings-passive="device-network-enrollment">${p?.enrollment === "pending" ? "このPCの参加申請は承認待ちです。管理者が承認すると自動で接続します。" : "Hubに接続していません。"}</p>` : ""}${leavePending ? "<p>このPCのプロジェクト離脱を確認中です。確定するまで新しい依頼は送れません。</p>" : ""}${error ? `<p class="shared-error">${esc(error)}</p>` : ""}${busy ? "<p>処理中…</p>" : ""}${person && p?.submission_uncertain ? "<p>依頼の受付結果を確認しています。新たに送信せず、下の確認ボタンをご利用ください。</p>" : ""}</div>`;
  const conversationId = detail?.conversation_id ?? detail?.root_id ?? "";
  const attachmentEditUnavailable = Boolean(detail && ["succeeded", "failed", "cancelled"].includes(detail.state)
    && p?.conversation_history?.jobs[0]?.job.id === detail.id && selectedSharedRequestHasAttachments(p));
  const activityWithStop = `${activity}${p?.projects_stale ? '<p class="shared-work-message" role="status">Hubのプロジェクト一覧は未更新です。接続を確認するまで依頼は送れません。</p>' : ""}${deletionPending ? '<p class="shared-work-message" role="status">このチャットは削除処理中です。実行停止が確定するまで追加の依頼や編集・再送はできません。</p>' : ""}${attachmentEditUnavailable ? '<p class="shared-work-message" role="status">添付付きの依頼は編集できません。新しい依頼として送ってください。</p>' : ""}<div data-shared-region="stop-all" class="shared-stop-all">${conversationId && sharedWorkActionEnabled(local, "stop-conversation", conversationId) ? `<span>この会話で動いている仕事とアプリを停止します。受付後も、停止が画面に反映されるまで確認してください。</span>${button("stop-conversation", "この会話の実行をすべて停止", conversationId, busy)}` : ""}</div>`;
  const confirmDelete = local.confirmation?.kind === "delete_conversation" && local.confirmation.projectId === project?.id
    ? `<section data-shared-region="confirmation" class="shared-card" role="group" aria-label="共有チャットの削除"><p>「${esc(local.confirmation.title)}」を削除すると、すべての操作PCから見えなくなります。実行中の仕事は停止に進み、各PCのファイルは残ります。</p><button data-action="shared-confirm-confirmation">共有チャットを削除</button><button data-action="shared-cancel-confirmation">戻る</button></section>`
    : "";
  const revise = local.editingJobId && detail?.id === local.editingJobId
    ? `<section data-shared-region="revision-editor" class="shared-card shared-revision-editor"><h2>依頼を編集して再送</h2><p>前の仕事を停止した後の新しい依頼です。作成済みのファイルや起動中のアプリは元に戻りません。</p>${renderConversationInput({ id: "shared-revise-prompt", value: local.revisionDraft, label: "再送する依頼", placeholder: "修正する内容", attributes: `data-shared-field="revisionPrompt" ${busy ? "disabled" : ""}` })}<div class="shared-actions">${button("save-revise", "編集して再送", "", !sharedWorkActionEnabled(local, "save-revise", ""))}${button("cancel-revise", "戻る", "", busy)}</div>${detail.revision !== local.editingJobRevision ? "<p class=\"shared-error\">会話の状態が変わりました。編集を取り消してから最新の依頼を確認してください。</p>" : ""}</section>`
    : "";
  const thread = `${!person ? connection + renderSharedWorkOnboarding(local) : project ? `${approvalMarkup}${confirmDelete}${history}${revise}${detail ? renderWorkDetails(local, "record") : ""}` : `<section data-shared-region="projects" class="shared-card"><h2>利用できるプロジェクトを確認</h2><p>${projectAccessHint}</p><p>設定後は自動で表示します。</p></section>`}<div data-shared-region="receipt">${person && p?.submission_uncertain ? button("retry-submission", "前回の依頼の受付を確認", "", busy || Boolean(p.submission_storage_error)) : ""}</div><p data-shared-region="feedback" role="status">${!local.conceal ? esc(p?.feedback) : ""}</p>`;
  return `<div class="shared-work ${project ? "" : "without-aside"}" data-shared-owner="${esc(owner)}">${renderConversationMain({ className: "shared-conversation", attributes: 'tabindex="-1" aria-labelledby="shared-heading"', topbar, activity: activityWithStop, thread, threadClassName: "shared-thread", composer })}${project ? `<aside class="artifact-pane shared-project-status" aria-label="プロジェクトの状況"><div class="pane-title"><h2>出力とPCの状況</h2></div><div class="output-scroll"><section data-shared-region="environments" class="shared-card"><h2>PCの利用状況</h2>${executionStatus(local)}${status?.next_environment_before ? button("next-environments", "ほかのPC", "", busy) : ""}</section>${detail ? renderWorkDetails(local, "support") : ""}${renderWorkInbox(local)}</div></aside>` : ""}</div>`;
}
function renderConversationHistory(local: SharedWorkPresentation, selectedJobId: string | null): string {
  const history = local.projection?.conversation_history;
  if (!history) return "";
  const owner = esc(JSON.stringify([local.projection?.generation, local.projection?.principal?.user_id, history.project_id, history.conversation_id]));
  const oldestFirst = [...history.jobs].reverse();
  const revisedJobs = new Set(history.jobs.map(row => row.job.revises_job_id).filter((id): id is string => Boolean(id)));
  if (local.projection?.detail?.revises_job_id) revisedJobs.add(local.projection.detail.revises_job_id);
  return `<section class="shared-history" data-shared-conversation="${owner}" aria-label="この会話の履歴">
    ${history.next_before !== null ? `<div class="history-load-earlier">${button("history-next", "以前の会話を表示", "", Boolean(local.pending))}</div>` : ""}
    ${oldestFirst.map(({ job, input, result, artifacts, more_artifacts }) => {
      const prompt = object(input).prompt;
      const answer = historyAnswer(result);
      const selected = job.id === selectedJobId;
      const transcript = [
        ...(typeof prompt === "string" ? [messageRow("user", prompt, `${job.id}:request`)] : []),
        ...(answer ? [messageRow(answer.kind, answer.text, `${job.id}:result`)] : []),
      ];
      const status = `<div class="shared-history-status"><span>${esc(workStateLabel(job.state))} · ${esc(job.device_label ?? job.environment_label)}</span>${selected ? "" : button("detail", "この仕事を開く", job.id, Boolean(local.pending))}${job.can_cancel ? button("cancel", "実行を停止", job.id, Boolean(local.pending)) : ""}${selected && sharedWorkActionEnabled(local, "start-revise", job.id) ? button("start-revise", "依頼を編集して再送", job.id, Boolean(local.pending)) : ""}</div>`;
      const files = artifacts.length || more_artifacts ? `<p class="shared-history-assets">成果ファイル: ${artifacts.map(asset => esc(asset.name)).join("、")}${more_artifacts ? " ほかにもあります。" : ""}${selected ? " 右側から保存できます。" : " この仕事を開くと保存できます。"}</p>` : "";
      const recovery = selected && answer?.kind === "error" && job.can_continue && local.projection && selectedSharedJobIsLatest(local.projection)
        ? '<p>原因を解消してから、この会話で続きの依頼を送れます。 <a href="#shared-followup">続きの依頼へ</a></p>' : "";
      const unknown = result != null && !answer ? `<details data-details-key="hub-result-${esc(job.id)}"><summary>結果の詳細</summary><pre>${esc(JSON.stringify(result, null, 2))}</pre></details>` : "";
      const content = `${job.revises_job_id ? '<p class="shared-revision-label">編集して再送した依頼</p>' : ""}${renderTranscriptRows(transcript, { anchorPrefix: `hub-${job.id}` })}${status}${files}${recovery}${unknown}`;
      return revisedJobs.has(job.id)
        ? `<details class="shared-history-turn shared-revised-turn ${selected ? "selected" : ""}" data-details-key="shared-revised:${esc(job.id)}" data-shared-job-id="${esc(job.id)}" ${selected ? "open" : ""}><summary>編集前の依頼 · ${esc(new Date(job.created_at_ms).toLocaleString("ja-JP"))}</summary>${content}</details>`
        : `<div class="shared-history-turn ${selected ? "selected" : ""}" data-shared-job-id="${esc(job.id)}">${content}</div>`;
    }).join("") || "<p>この会話の履歴はまだありません。</p>"}
  </section>`;
}

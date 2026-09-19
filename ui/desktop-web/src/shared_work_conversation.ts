import { escapeHtml } from "./utils.ts";
import { renderMarkdown } from "./markdown.ts";
import { deviceNetworkError } from "./device_network_state.ts";
import { workStateLabel, type SharedWorkPresentation, type WorkExecutionDevice, type WorkRunnerContact } from "./shared_work_state.ts";
import { renderWorkDetails, renderWorkInbox, renderWorkInputs, renderStartDeadline, renderWorkResult, workRecordOwner } from "./shared_work_details.ts";

const esc = (value: unknown) => escapeHtml(String(value ?? ""));
const button = (action: string, label: string, value = "", disabled = false) => `<button data-action="shared-${action}" data-value="${esc(value)}" ${disabled ? "disabled" : ""}>${esc(label)}</button>`;
function contact(value?: WorkRunnerContact | null): string {
  const label = value?.state === "recent" ? "最近の応答あり" : "PCの応答なし（状態不明）";
  return `<span class="shared-secondary" data-runner-contact="${esc(value?.state ?? "unconfirmed")}">${label} · 最終応答: ${value?.last_contact_ms != null ? esc(new Date(value.last_contact_ms).toLocaleString("ja-JP")) : "未確認"}</span>`;
}
function preparation(device: WorkExecutionDevice): string {
  const label = ({ waiting_setup: "このPCの初回設定待ち", pending: "実行場所を準備中", failed: "実行場所の準備に失敗しました", ready: "準備完了" } as Record<string, string>)[device.preparation_state] ?? "実行設定を確認中";
  return `<p>${label}</p>${device.preparation_state === "waiting_setup" ? `<p class="shared-secondary">「${esc(device.device_label ?? device.device_id)}」でmoyAI Hubの設定を開き、仕事の実行を許可してください。</p>` : ""}${device.error ? `<p class="shared-error">${esc(device.error)}</p>` : ""}`;
}
function executionStatus(local: SharedWorkPresentation): string {
  const status = local.projection?.status;
  if (!status) return "<p>PCの利用状況を確認しています。</p>";
  const devices = status.execution_devices ?? [];
  const environments = status.environments.map(env => {
    const device = devices.find(row => row.environment_id === env.id);
    return `<article class="shared-environment"><h3>${esc(env.device_label ?? env.label)}</h3>${device && device.preparation_state !== "ready" ? preparation(device) : ""}<p>${env.occupied} / ${env.capacity} 枠を使用中${env.enabled ? "" : " · 準備中または受付停止"}</p>${contact(env.runner_contact)}${env.occupants.map(o => `<p>${esc(o.user.display_name)} · ${esc(o.project_label)} · ${esc(o.title)}（${esc(workStateLabel(o.state))}）</p>`).join("")}${env.other_occupants ? `<p>他のプロジェクトで ${env.other_occupants} 件使用中</p>` : ""}${env.additional_visible_occupants ? `<p>ほかに ${env.additional_visible_occupants} 件の実行があります。</p>` : ""}</article>`;
  }).join("");
  const otherDevices = devices.filter(device => !status.environments.some(env => env.id === device.environment_id)).map(device => `<article class="shared-environment"><h3>${esc(device.device_label ?? device.device_id)}</h3>${preparation(device)}${device.preparation_state === "ready" ? "<p class=\"shared-secondary\">利用状況は別のページに表示されています。</p>" : ""}</article>`).join("");
  return environments + otherDevices || "<p>管理者が実行するPCを割り当てると、ここに表示されます。</p>";
}

/** A Hub project is a normal work surface; account and advanced options stay secondary. */
export function renderHubConversation(local: SharedWorkPresentation, approvalMarkup: string): string {
  const p = local.projection, person = local.conceal ? null : p?.principal;
  const project = person ? p?.projects.find(row => row.id === p.selected_project_id) : null;
  const detail = person ? p?.detail : null, busy = Boolean(local.pending);
  const status = person ? p?.status : null;
  const owner = `${p?.generation ?? "loading"}:${person?.user_id ?? "anonymous"}:${project?.id ?? ""}:${detail?.id ?? ""}:${local.conceal}:${p?.connected}:${project?.can_submit}`;
  const error = local.error || (!local.conceal ? p?.error || p?.submission_storage_error || (p?.enrollment_error ? deviceNetworkError(p.enrollment_error) : null) : null);
  const recordOwner = p ? workRecordOwner(local) : "";
  const environments = status?.environments.filter(row => row.enabled) ?? [];
  const selectedJob = status?.jobs.find(row => row.id === detail?.id);
  return `<main class="shared-work" tabindex="-1" data-shared-owner="${esc(owner)}" aria-labelledby="shared-heading">
    <header data-shared-region="account" class="shared-header"><div><h1 id="shared-heading">${esc(project?.label ?? "Hubのプロジェクト")}</h1><p>${person ? `${esc(person.display_name)}${project ? " · プロジェクトの参加者で会話と成果を共有します" : " · 管理者が割り当てると、左のプロジェクト一覧に表示されます"}` : "初回だけHubの利用者としてログインします。"}</p></div><div class="shared-actions">${project ? button("new-conversation", "新しいチャット", "", busy) : ""}<details data-details-key="hub-project-account"><summary>アカウント</summary><p>${esc(p?.hub_url)}</p>${person || local.conceal ? button("logout", "ログアウト", "", busy) : ""}<button data-action="show-hub">接続・このPCの設定</button></details></div></header>
    <div data-shared-region="message" role="status">${!p?.connected ? `<p data-settings-passive="device-network-enrollment">${p?.enrollment === "pending" ? "PCの参加承認待ちです。管理者の登録後に自動で接続します。" : "Hubに接続していません。"}</p>` : ""}${error ? `<p class="shared-error">${esc(error)}</p>` : ""}${busy ? "<p>処理中…</p>" : ""}${person && p?.submission_uncertain ? "<p>依頼の受付結果を確認しています。新たに送信せず、下の確認ボタンをご利用ください。</p>" : ""}</div>
    ${!person ? `<section data-shared-region="login" class="shared-card shared-login"><h2>初回ログイン</h2>${!p?.connected ? `<p>管理者から受け取ったhub-config.tomlを読み込むと、このPCの参加を申請します。</p>${button("import", "Hubの設定ファイルを読み込む", "", busy)}${button("reconnect", "再接続", "", busy)}` : `<p>管理者が登録した利用者名とパスワードを入力してください。次回からこのWindowsユーザーで自動ログインします。プロジェクトごとの登録は不要です。</p><label>利用者名<input id="shared-username" data-shared-field="username" autocomplete="username" value="${esc(local.username)}" ${busy ? "disabled" : ""}></label><label>パスワード<input id="shared-password" type="password" data-shared-field="password" autocomplete="current-password" value="${esc(local.password)}" ${busy ? "disabled" : ""}></label>${button("login", "ログインして続ける", "", busy || !local.username || !local.password)}`}</section>` : project ? `<div class="shared-columns"><div class="shared-conversation-column">
      <section data-shared-region="detail" data-shared-record-owner="${esc(recordOwner)}" class="shared-card shared-conversation-record">${detail ? `<h2>${esc(detail.title)}</h2><p>${esc(workStateLabel(detail.state))}${selectedJob ? ` · ${esc(selectedJob.requestor.display_name)} → ${esc(selectedJob.device_label ?? selectedJob.environment_label)}` : ""}</p><div data-shared-runner-observation>${contact(detail.runner_contact)}</div>${detail.wait_reason ? `<p>${esc(detail.wait_reason)}</p>` : ""}${detail.uncertainty_reason ? `<p class="shared-error">${esc(detail.uncertainty_reason)}</p>` : ""}<div class="markdown-body shared-user-message">${renderMarkdown(typeof detail.input === "object" && detail.input !== null && "prompt" in detail.input ? String(detail.input.prompt) : "")}</div>${renderWorkResult(detail.result, recordOwner)}${detail.start_before_ms != null ? `<details data-details-key="hub-job-deadline"><summary>実行の設定</summary><p>開始期限: ${esc(new Date(detail.start_before_ms).toLocaleString("ja-JP"))}</p></details>` : ""}${selectedJob?.can_cancel ? button("cancel", "実行を停止", detail.id, busy) : ""}` : `<h2>新しいチャット</h2><p>このプロジェクトで行いたいことを入力してください。</p>`}</section>
      ${detail ? renderWorkDetails(local, "conversation") : ""}
      ${approvalMarkup}
      ${project.can_submit ? `${renderWorkInputs(local)}${!detail ? `<section data-shared-region="draft" class="shared-card shared-composer">${environments.length === 1 && local.environmentId === environments[0].id ? `<p>実行するPC: <strong>${esc(environments[0].device_label ?? environments[0].label)}</strong></p>` : `<label>実行するPC<select id="shared-environment" data-shared-field="environmentId" ${busy ? "disabled" : ""}><option value="">PCを選択してください</option>${environments.map(env => `<option value="${esc(env.id)}" ${local.environmentId === env.id ? "selected" : ""}>${esc(env.device_label ?? env.label)}</option>`).join("")}</select></label>`}<label class="shared-prompt-label">メッセージ<textarea id="shared-prompt" data-shared-field="prompt" rows="5" placeholder="このプロジェクトで行いたいこと" ${busy ? "disabled" : ""}>${esc(local.prompt)}</textarea></label><details data-details-key="hub-new-chat-options"><summary>追加設定</summary><label>チャット名（空欄で自動作成）<input id="shared-title" data-shared-field="title" value="${esc(local.title)}" ${busy ? "disabled" : ""}></label>${renderStartDeadline(local, "startBefore")}</details>${button("submit", "送信", "", busy || p!.submission_uncertain || !local.prompt.trim() || !local.environmentId)}<p class="shared-secondary">画面を閉じても実行は続きます。</p></section>` : ""}` : ""}
    </div><aside class="shared-project-status" aria-label="プロジェクトの状況">
      <section data-shared-region="environments" class="shared-card"><h2>PCの利用状況</h2>${executionStatus(local)}${status?.next_environment_before ? button("next-environments", "ほかのPC", "", busy) : ""}</section>
      ${detail ? renderWorkDetails(local, "support") : ""}${renderWorkInbox(local)}
    </aside></div>` : `<section data-shared-region="projects" class="shared-card"><h2>プロジェクトの割り当て待ち</h2><p>管理者に、参加するプロジェクトとこのPCの登録を依頼してください。完了後は自動で表示します。</p></section>`}
    <div data-shared-region="receipt">${person && p?.submission_uncertain ? `${button("retry-submission", "前回の依頼の受付を確認", "", busy || Boolean(p.submission_storage_error))}` : ""}</div>
    <p data-shared-region="feedback" role="status">${!local.conceal ? esc(p?.feedback) : ""}</p>
  </main>`;
}

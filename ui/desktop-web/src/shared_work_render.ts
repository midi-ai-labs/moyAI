import { escapeHtml } from "./utils.ts";
import { deviceNetworkError } from "./device_network_state.ts";
import { mcpHistoryRegionHasSelection } from "./mcp_history_dom.ts";
import { workStateLabel, type SharedWorkPresentation, type WorkRunnerContact } from "./shared_work_state.ts";
import { renderWorkDetails, renderWorkInbox, renderWorkInputs, renderWorkProvider, retainWorkRecord, renderStartDeadline, renderWorkResult, workRecordOwner } from "./shared_work_details.ts";
const esc = (value: unknown) => escapeHtml(String(value ?? ""));
function button(action: string, label: string, value = "", disabled = false): string {
  return `<button data-action="shared-${action}" data-value="${esc(value)}" ${disabled ? "disabled" : ""}>${esc(label)}</button>`;
}
function renderRunnerContact(contact: WorkRunnerContact | null | undefined): string {
  if (!contact) return "";
  const label = contact.state === "recent" ? "最近の応答あり" : contact.state === "stale" ? "状態不明（最近の応答を確認できません）" : "状態不明（応答を確認していません）";
  return `<p class="${contact.state === "recent" ? "shared-secondary" : "shared-error"}" data-runner-contact="${esc(contact.state)}">Runner通信: ${label}</p><p class="shared-secondary">Runnerの最終応答: ${contact.last_contact_ms == null ? "未確認" : esc(new Date(contact.last_contact_ms).toLocaleString("ja-JP"))}</p>`;
}
export function renderSharedWork(local: SharedWorkPresentation): string {
  const p = local.projection;
  const person = local.conceal ? null : p?.principal;
  const project = p?.projects.find(row => row.id === p.selected_project_id);
  const status = person ? p?.status : null;
  const busy = local.pending !== null;
  const owner = `${p?.generation ?? "loading"}:${person?.user_id ?? "anonymous"}:${p?.selected_project_id ?? ""}:${local.conceal}:${p?.connected}:${project?.can_submit}`;
  const error = local.error || (!local.conceal ? p?.error || p?.submission_storage_error || (p?.enrollment_error ? deviceNetworkError(p.enrollment_error) : null) : null);
  const detail = person ? p?.detail : null;
  const recordOwner = p ? workRecordOwner(local) : "";
  return `<main class="shared-work" role="dialog" aria-modal="true" tabindex="-1" data-shared-owner="${esc(owner)}" aria-labelledby="shared-heading">
    <header data-shared-region="account" class="shared-header"><div><h1 id="shared-heading">共有仕事</h1><p>${person ? `${esc(person.display_name)} · ${esc(p?.hub_url)}` : "Hubの実行環境へ仕事を依頼し、別の端末からも状況を確認できます。"}</p></div><div class="shared-actions">${person || local.conceal ? button("logout", "ログアウト", "", busy) : ""}<button data-action="close-overlay">ローカル画面へ戻る</button></div></header>
    <div data-shared-region="message" role="status"><p data-settings-passive="device-network-enrollment">${p?.connected ? "参加済み" : p?.enrollment === "pending" ? "承認待ち — Hub管理者がこの端末の参加を確認しています。" : "端末のHub接続を確認してください。"}</p>${error ? `<p class="shared-error">${esc(error)}</p>` : ""}${local.pending ? `<p>処理中…</p>` : ""}${p?.observed_at_ms && person ? `<p class="shared-secondary">Hubの情報取得: ${esc(new Date(p.observed_at_ms).toLocaleString("ja-JP"))}</p>` : ""}${p?.submission_uncertain && person ? "<p>受付結果を確認できていません。同じ内容で再試行すると、同じ依頼の受付を確認します。</p>" : ""}</div>
    ${!person ? `<section data-shared-region="login" class="shared-card shared-login"><h2>Hub利用者としてログイン</h2>${!p?.connected ? `<p>管理者から受け取った共通設定で端末を登録します。参加承認後にログインできます。</p><div class="shared-actions">${button("import", "Hubの共通設定を読み込む", "", busy)}${button("reconnect", "再接続・承認状況を確認", "", busy)}</div>` : `<p>この端末はHubに接続しています。所属プロジェクトの利用者アカウントを入力してください。</p><label>利用者名<input id="shared-username" data-shared-field="username" autocomplete="username" value="${esc(local.username)}" ${busy ? "disabled" : ""}></label><label>パスワード<input id="shared-password" type="password" data-shared-field="password" autocomplete="current-password" value="${esc(local.password)}" ${busy ? "disabled" : ""}></label>${button("login", "ログイン", "", busy || !local.username || !local.password)}`}</section>` : `
    <section data-shared-region="projects" class="shared-projects"><h2>所属プロジェクト</h2><div class="shared-actions">${p!.projects.map(row => `<button data-action="shared-project" data-value="${esc(row.id)}" aria-pressed="${row.id === p!.selected_project_id}" ${busy ? "disabled" : ""}>${esc(row.label)}</button>`).join("") || "<p>所属プロジェクトがありません。管理者に参加を依頼してください。</p>"}${button("refresh", "更新", "", busy)}</div></section>
    ${project ? `<div class="shared-columns"><div>
      <section data-shared-region="environments" class="shared-card"><h2>実行環境の利用状況</h2>${status?.environments.map(env => `<article class="shared-environment"><h3>${esc(env.label)} <small>${env.occupied} / ${env.capacity} 枠使用中（Hub記録）${env.enabled ? "" : " · 受付停止"}</small></h3><p class="shared-secondary">共通枠: ${esc(env.resource_id)}</p>${renderRunnerContact(env.runner_contact ?? { state: "unconfirmed", last_contact_ms: null })}${env.occupants.map(o => `<p>${esc(o.user.display_name)} · ${esc(o.project_label)} · ${esc(o.title)}（${esc(workStateLabel(o.state))}）</p>`).join("")}${env.other_occupants ? `<p>他のプロジェクトで ${env.other_occupants} 件使用中</p>` : ""}${env.additional_visible_occupants ? `<p>このほか閲覧可能な仕事が ${env.additional_visible_occupants} 件実行中です。</p>` : ""}</article>`).join("") || "<p>利用できる実行環境がありません。</p>"}${status?.next_environment_before ? button("next-environments", "次の実行環境", "", busy) : ""}</section>
      ${project.can_submit ? `${renderWorkInputs(local)}<section data-shared-region="draft" class="shared-card"><h2>仕事を依頼する</h2><label>実行環境<select id="shared-environment" data-shared-field="environmentId" ${busy ? "disabled" : ""}><option value="">選択してください</option>${status?.environments.filter(e => e.enabled).map(e => `<option value="${esc(e.id)}" ${local.environmentId === e.id ? "selected" : ""}>${esc(e.label)}</option>`).join("") ?? ""}</select></label><label>件名<input id="shared-title" data-shared-field="title" value="${esc(local.title)}" ${busy ? "disabled" : ""}></label><label>依頼内容<textarea id="shared-prompt" data-shared-field="prompt" rows="6" ${busy ? "disabled" : ""}>${esc(local.prompt)}</textarea></label>${renderStartDeadline(local, "startBefore")}${button("submit", "仕事を投入", "", busy || p!.submission_uncertain || !local.title.trim() || !local.prompt.trim() || !local.environmentId)}<p class="shared-secondary">画面を閉じても仕事は続きます。結果はこのプロジェクトから確認できます。</p></section>` : ""}
    </div><div>
      <section data-shared-region="jobs" class="shared-card"><h2>仕事一覧</h2>${status?.jobs.map(job => `<article class="shared-job ${p!.selected_job_id === job.id ? "selected" : ""}"><div class="shared-actions">${button("detail", job.title, job.id, busy)}<span>${esc(workStateLabel(job.state))}</span></div><p>${esc(job.requestor.display_name)} → ${esc(job.environment_label)}${job.assignee.user_id !== job.requestor.user_id ? ` · 担当 ${esc(job.assignee.display_name)}` : ""}</p>${renderRunnerContact(job.runner_contact)}${job.wait_reason ? `<p>${esc(job.wait_reason)}</p>` : ""}${job.uncertainty_reason ? `<p class="shared-error">${esc(job.uncertainty_reason)}</p>` : ""}${job.can_cancel ? button("cancel", "この仕事と子の仕事を取消", job.id, busy) : ""}</article>`).join("") || "<p>仕事はまだありません。</p>"}<div class="shared-actions">${status?.next_before ? button("next-jobs", "以前の仕事", "", busy) : ""}${button("latest", "最新の仕事・環境へ", "", busy)}</div></section>
      <section data-shared-region="detail" data-shared-record-owner="${esc(recordOwner)}" class="shared-card"><h2>仕事の詳細</h2>${detail ? `<h3>${esc(detail.title)}</h3><p>${esc(workStateLabel(detail.state))} · ${esc(new Date(detail.updated_at_ms).toLocaleString("ja-JP"))}</p><div data-shared-runner-observation>${renderRunnerContact(detail.runner_contact)}</div>${detail.wait_reason ? `<p>${esc(detail.wait_reason)}</p>` : ""}${detail.uncertainty_reason ? `<p class="shared-error">${esc(detail.uncertainty_reason)}</p>` : ""}${detail.start_before_ms != null ? `<p>開始期限: ${esc(new Date(detail.start_before_ms).toLocaleString("ja-JP"))}</p>` : ""}${detail.parent_id ? `<p>親の仕事: ${esc(detail.parent_id)}</p>` : ""}<h3>依頼内容</h3><pre>${esc(typeof detail.input === "object" && detail.input !== null && "prompt" in detail.input ? detail.input.prompt : JSON.stringify(detail.input, null, 2))}</pre>${renderWorkResult(detail.result, recordOwner)}` : "<p>一覧から仕事を選択してください。</p>"}</section>
      ${renderWorkDetails(local)}
    </div></div>` : ""}${renderWorkInbox(local)}`}
    ${person ? `<div data-shared-region="receipt">${p?.submission_uncertain ? `<p>前回の依頼の受付結果が未確認です。保存済みの依頼内容と受付IDを使って確認します。</p>${button("retry-submission", "前回の依頼の受付を確認", "", busy || Boolean(p.submission_storage_error))}` : ""}</div>${renderSharedApproval(local)}` : ""}
    ${p?.feedback && !local.conceal ? `<p data-shared-region="feedback" role="status">${esc(p.feedback)}</p>` : `<p data-shared-region="feedback" role="status"></p>`}
    ${!local.conceal ? renderWorkProvider(local) : ""}
  </main>`;
}

function renderSharedApproval(local: SharedWorkPresentation): string {
  const approval = local.projection?.approval;
  return `<section data-shared-region="approval" class="shared-card" aria-labelledby="shared-approval-title">${approval ? `<h2 id="shared-approval-title">実行の承認</h2><p>${esc(approval.request.summary)}</p><p>操作種別: ${esc(approval.request.access)}${approval.request.agent_task_name ? ` · ${esc(approval.request.agent_task_name)}` : ""}</p>${approval.request.details.map(detail => `<pre>${esc(detail)}</pre>`).join("")}<p>対象: ${approval.request.targets.map(esc).join("、") || "なし"}</p>${approval.request.outside_workspace ? "<p>実行環境の作業フォルダ外への操作を含みます。</p>" : ""}${approval.request.risks.length ? `<p>確認事項: ${approval.request.risks.map(esc).join("、")}</p>` : ""}${approval.status === "pending" ? `<p>有効期限: ${esc(new Date(approval.expires_at_ms).toLocaleString("ja-JP"))}</p>${approval.can_decide ? `<div class="shared-actions">${button("approve", "この操作の実行を許可", approval.id, Boolean(local.pending))}${button("deny", "この操作を拒否", approval.id, Boolean(local.pending))}${button("stop", "仕事を停止", approval.id, Boolean(local.pending))}</div>` : "<p>担当者またはプロジェクト管理者の判断を待っています。</p>"}` : `<p>承認状態: ${esc(approval.status)}${approval.decision ? ` · ${esc(approval.decision)}` : ""}</p>`}` : "<p>選択中の仕事に、現在の承認依頼はありません。</p>"}</section>`;
}

/** Keep the active form connected during polling, including IME composition. */
export function retainSharedWorkSurface(current: HTMLElement, next: HTMLElement): boolean {
  if (current.dataset.sharedOwner !== next.dataset.sharedOwner) return false;
  for (const nextRegion of next.querySelectorAll<HTMLElement>("[data-shared-region]")) {
    const region = current.querySelector<HTMLElement>(`[data-shared-region="${nextRegion.dataset.sharedRegion}"]`);
    if (!region) return false;
    if (nextRegion.dataset.sharedRegion === "provider") {
      const opened = new Map(Array.from(region.querySelectorAll<HTMLDetailsElement>("details[data-details-key]"), detail => [detail.dataset.detailsKey, detail.open]));
      for (const detail of nextRegion.querySelectorAll<HTMLDetailsElement>("details[data-details-key]")) {
        detail.open = opened.get(detail.dataset.detailsKey) ?? detail.open;
      }
      // Adopt the other fields without detaching the native SELECT between input and change.
      if (region.dataset.sharedFormOwner !== nextRegion.dataset.sharedFormOwner) {
        const active = document.activeElement;
        if (region.contains(active) && active?.matches('select[data-shared-field="draft:editTemplateId"]')) {
          for (const field of region.querySelectorAll<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>("[data-shared-field]")) {
            const replacement = nextRegion.querySelector<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>(`[data-shared-field="${field.dataset.sharedField}"]`);
            if (field !== active && replacement) field.value = replacement.value;
          }
          region.dataset.sharedFormOwner = nextRegion.dataset.sharedFormOwner;
        } else { region.replaceWith(nextRegion); continue; }
      }
    }
    if (nextRegion.dataset.sharedRegion === "detail" && region.dataset.sharedRecordOwner
      && region.dataset.sharedRecordOwner === nextRegion.dataset.sharedRecordOwner) {
      // Communication is passive Hub state, independent of a focused or selected raw record.
      const observation = region.querySelector<HTMLElement>("[data-shared-runner-observation]");
      const nextObservation = nextRegion.querySelector<HTMLElement>("[data-shared-runner-observation]");
      if (observation && nextObservation && !mcpHistoryRegionHasSelection(observation)
        && !observation.isEqualNode(nextObservation)) observation.replaceWith(nextObservation.cloneNode(true));
    }
    const active = region.contains(document.activeElement);
    if (active && document.activeElement?.matches("input,textarea,select") && ["login", "draft", "followup", "handover", "provider"].includes(nextRegion.dataset.sharedRegion ?? "")) {
      for (const field of region.querySelectorAll<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>("[data-shared-field]")) {
        const replacement = nextRegion.querySelector<HTMLInputElement>(`[data-shared-field="${field.dataset.sharedField}"]`);
        if (replacement) field.disabled = replacement.disabled;
      }
      for (const button of region.querySelectorAll<HTMLButtonElement>("button[data-action]")) {
        const replacement = nextRegion.querySelector<HTMLButtonElement>(`button[data-action="${button.dataset.action}"]`);
        if (replacement) { button.disabled = replacement.disabled; button.setAttribute("aria-disabled", String(replacement.disabled)); button.textContent = replacement.textContent; }
      }
    } else if (!["transcript", "detail"].includes(nextRegion.dataset.sharedRegion ?? "") || !retainWorkRecord(region, nextRegion)) region.replaceWith(nextRegion);
  }
  return true;
}

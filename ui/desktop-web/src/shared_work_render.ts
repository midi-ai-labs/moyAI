import { escapeHtml } from "./utils.ts";
import { mcpHistoryRegionHasSelection } from "./mcp_history_dom.ts";
import type { SharedWorkPresentation, SharedWorkProjection } from "./shared_work_state.ts";
import { renderHubConversation } from "./shared_work_conversation.ts";
import { retainWorkRecord } from "./shared_work_details.ts";
import { guardianReasonPrefix, permissionReviewReason, permissionRiskLabel } from "./permission_copy.ts";
const esc = (value: unknown) => escapeHtml(String(value ?? ""));
function button(action: string, label: string, value = "", disabled = false): string {
  return `<button data-action="shared-${action}" data-value="${esc(value)}" ${disabled ? "disabled" : ""}>${esc(label)}</button>`;
}
export function renderSharedWork(local: SharedWorkPresentation, receiverActivity = ""): string {
  return renderHubConversation(local, local.projection?.principal && !local.conceal ? renderSharedApproval(local) : "", receiverActivity);
}

type ApprovalRequest = NonNullable<SharedWorkProjection["approval"]>["request"];
const approvalAccessLabels: Record<string, string> = { list: "フォルダー内の一覧の取得", search: "ファイルの検索", read: "ファイルの読取り", edit: "ファイルの変更", shell: "コマンドの実行" };

function approvalDetail(request: ApprovalRequest, prefix: string): string | undefined {
  return request.details.find(detail => detail.startsWith(prefix) && detail.slice(prefix.length).trim())?.slice(prefix.length);
}

function renderApprovalOperation(request: ApprovalRequest): string {
  const shell = request.access === "shell";
  const operation = approvalDetail(request, "操作: ");
  const target = request.targets.length ? undefined : approvalDetail(request, "対象: ");
  const reviewReason = permissionReviewReason(request.details);
  // These are explanatory strings owned by shell.rs/context.rs, not a second
  // permission classifier. Unknown details stay visible; the complete request
  // is always retained below, including all original strings and identifiers.
  const details = request.details.flatMap(detail => {
    if (detail.startsWith(guardianReasonPrefix)) return [];
    if ((operation && detail === `操作: ${operation}`) || (target && detail === `対象: ${target}`)) return [];
    if (shell) {
      for (const [prefix, label] of [["Command: ", "実行コマンド"], ["Workdir: ", "実行するフォルダー"], ["Requested sandbox elevation: ", "保護を外す理由（AIの説明）"]]) {
        if (detail.startsWith(prefix)) return [`<p><strong>${label}</strong></p><pre>${esc(detail.slice(prefix.length))}</pre>`];
      }
      if (detail.startsWith("Canonical executable candidate (identity pinned): ")
        || detail === "execution boundary: approval grants this process effect elevation outside the workspace-write OS sandbox"
        || detail === "Workspace modes run this process in the native workspace-write OS sandbox; an approved elevation or Full Access runs it unrestricted under the current user account. The unelevated Windows backend uses advisory network controls rather than firewall enforcement.") return [];
    }
    return [`<pre>${esc(detail)}</pre>`];
  });
  const risks = request.risks.map(permissionRiskLabel);
  const permission = shell ? '<p class="shared-error"><strong>許可すると、このコマンドを作業フォルダー内に限定する保護を外して実行します。</strong>実行PCのユーザー権限で、表示された対象以外のファイル操作や外部通信も可能になります。</p>' : "";
  const summary = shell ? "次のコマンドを実行しようとしています。" : request.summary;
  const outside = request.outside_workspace ? `<p class="shared-error">${shell ? "作業フォルダー外への操作、または保護を外した実行の要求を含みます。" : "実行環境の作業フォルダー外への操作を含みます。"}</p>` : "";
  const targets = request.targets.map(esc).join("、") || esc(target ?? "指定なし");
  return `${reviewReason ? `<p><strong>確認が必要な理由</strong><br>${esc(reviewReason)}</p>` : ""}<p>${esc(summary)}</p><p><strong>対象:</strong> ${targets}</p>${outside}${permission}<div class="shared-approval-operation" aria-label="実行する操作">${details.join("") || (shell ? "<p>具体的な操作内容が記載されていません。元の承認データを確認してください。</p>" : "")}</div>${risks.length ? `<p>操作前の確認事項: ${risks.map(esc).join("、")}</p>${shell ? '<p class="shared-secondary">コマンドに含まれる文字列からの判定です。実際に行う操作は、上のコマンドを確認してください。</p>' : ""}` : ""}`;
}

function renderSharedApproval(local: SharedWorkPresentation): string {
  const approval = local.projection?.approval;
  if (!approval) return '<section data-shared-region="approval" class="shared-card" hidden></section>';
  const operation = renderApprovalOperation(approval.request);
  const metadata = `<p>操作種別: ${esc(approvalDetail(approval.request, "操作: ") ?? approvalAccessLabels[approval.request.access] ?? approval.request.access)}${approval.request.agent_task_name ? ` · ${esc(approval.request.agent_task_name)}` : ""}</p><p>有効期限: ${esc(new Date(approval.expires_at_ms).toLocaleString("ja-JP"))}</p><h3>元の承認データ</h3><pre>${esc(JSON.stringify(approval.request, null, 2))}</pre>`;
  if (approval.status !== "pending") {
    // A different details owner closes the record when an expanded pending
    // explanation settles; subsequent reading still retains its open state.
    return `<section data-shared-region="approval" class="shared-card shared-approval-record" aria-labelledby="shared-approval-title"><details data-details-key="shared-approval-record-${esc(approval.id)}"><summary id="shared-approval-title">${approvalResultTitle(approval.status, approval.decision)} <span class="shared-secondary">（詳細）</span></summary>${operation}${metadata}</details></section>`;
  }
  const review = `<div class="shared-approval-review" role="region" aria-label="承認する操作の詳細" tabindex="0">${operation}<details data-details-key="shared-approval-${esc(approval.id)}"><summary>承認の詳細・元のデータ</summary>${metadata}</details></div>`;
  const decisions = `<div class="shared-approval-decisions">${approval.can_decide ? `<p>許可は、この承認依頼に記載された操作にだけ適用されます。</p><div class="shared-actions">${button("approve", "この操作の実行を許可", approval.id, Boolean(local.pending))}${button("deny", "この操作を拒否", approval.id, Boolean(local.pending))}${button("stop", "仕事を停止", approval.id, Boolean(local.pending))}</div>` : "<p>担当者またはプロジェクト管理者が、この画面で判断できます。</p>"}</div>`;
  return `<section data-shared-region="approval" class="shared-card shared-approval-pending" aria-labelledby="shared-approval-title"><h2 id="shared-approval-title">${approval.can_decide ? "あなたの承認を待っています" : "担当者の承認待ち"}</h2>${review}${decisions}</section>`;
}

function approvalResultTitle(status: string, decision: string | null): string {
  // Hub ApprovalView.status is a string; only these current states establish
  // that a decision was recorded. They never establish execution success.
  if (status === "cancelled") return "この承認依頼は取り消されました";
  if (status === "expired") return "承認の有効期限が切れました";
  if (status === "decided" || status === "consumed") {
    if (decision === "approve") return "この操作を許可しました";
    if (decision === "deny") return "この操作を拒否しました";
    if (decision === "stop") return "仕事の停止を指示しました";
  }
  return "承認の記録";
}

/** Keep the active form connected during polling, including IME composition. */
export function retainSharedWorkSurface(current: HTMLElement, next: HTMLElement): boolean {
  if (current.dataset.sharedOwner !== next.dataset.sharedOwner) return false;
  for (const nextRegion of next.querySelectorAll<HTMLElement>("[data-shared-region]")) {
    const region = current.querySelector<HTMLElement>(`[data-shared-region="${nextRegion.dataset.sharedRegion}"]`);
    if (!region) return false;
    const opened = new Map(Array.from(region.querySelectorAll<HTMLDetailsElement>("details[data-details-key]"), detail => [detail.dataset.detailsKey, detail.open]));
    for (const detail of nextRegion.querySelectorAll<HTMLDetailsElement>("details[data-details-key]")) {
      const open = opened.get(detail.dataset.detailsKey);
      if (open !== undefined) detail.open = open;
    }
    if (nextRegion.dataset.sharedRegion === "detail" && region.dataset.sharedRecordOwner
      && region.dataset.sharedRecordOwner === nextRegion.dataset.sharedRecordOwner) {
      // Communication is passive Hub state, independent of a focused or selected raw record.
      const observation = region.querySelector<HTMLElement>("[data-shared-runner-observation]");
      const nextObservation = nextRegion.querySelector<HTMLElement>("[data-shared-runner-observation]");
      if (observation && nextObservation && !mcpHistoryRegionHasSelection(observation)
        && !observation.isEqualNode(nextObservation)) observation.replaceWith(nextObservation.cloneNode(true));
    }
    if (region.isEqualNode(nextRegion)) continue;
    const active = region.contains(document.activeElement);
    if (active && document.activeElement?.matches("input,textarea,select") && ["account", "draft", "followup", "handover", "revision-editor"].includes(nextRegion.dataset.sharedRegion ?? "")) {
      for (const field of region.querySelectorAll<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>("[data-shared-field]")) {
        const replacement = nextRegion.querySelector<HTMLInputElement>(`[data-shared-field="${field.dataset.sharedField}"]`);
        if (replacement) field.disabled = replacement.disabled;
      }
      for (const button of region.querySelectorAll<HTMLButtonElement>("button[data-action]")) {
        const replacement = Array.from(nextRegion.querySelectorAll<HTMLButtonElement>("button[data-action]")).find(candidate => candidate.dataset.action === button.dataset.action && candidate.dataset.value === button.dataset.value);
        if (replacement) { button.disabled = replacement.disabled; button.setAttribute("aria-disabled", String(replacement.disabled)); button.textContent = replacement.textContent; }
      }
    } else if (!["transcript", "detail", "history-container"].includes(nextRegion.dataset.sharedRegion ?? "") || !retainWorkRecord(region, nextRegion)) region.replaceWith(nextRegion);
  }
  return true;
}

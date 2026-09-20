import { escapeHtml } from "./utils.ts";
import { mcpHistoryRegionHasSelection } from "./mcp_history_dom.ts";
import type { SharedWorkPresentation } from "./shared_work_state.ts";
import { renderHubConversation } from "./shared_work_conversation.ts";
import { retainWorkRecord } from "./shared_work_details.ts";
const esc = (value: unknown) => escapeHtml(String(value ?? ""));
function button(action: string, label: string, value = "", disabled = false): string {
  return `<button data-action="shared-${action}" data-value="${esc(value)}" ${disabled ? "disabled" : ""}>${esc(label)}</button>`;
}
export function renderSharedWork(local: SharedWorkPresentation): string {
  return renderHubConversation(local, local.projection?.principal && !local.conceal ? renderSharedApproval(local) : "");
}

function renderSharedApproval(local: SharedWorkPresentation): string {
  const approval = local.projection?.approval;
  if (!approval) return '<section data-shared-region="approval" class="shared-card" hidden></section>';
  const risks = approval.request.risks.map(risk => ({ destructive_delete: "ファイルやデータの削除", move_or_rename: "移動・名前の変更", network: "ネットワーク通信", external_connection: "外部への接続・接続設定", configured_local_service: "設定済みローカルサービスの利用", protected_workspace_authority: "保護された作業設定への操作", external_mutation: "外部システムの変更", external_destructive_operation: "外部システムの削除などの操作", unclassified_shell: "実行内容を事前に分類できないコマンド" } as Record<string, string>)[risk] ?? risk);
  const operation = `<p>${esc(approval.request.summary)}</p><p><strong>対象:</strong> ${approval.request.targets.map(esc).join("、") || "指定なし"}</p>${approval.request.outside_workspace ? '<p class="shared-error">実行環境の作業フォルダー外への操作を含みます。</p>' : ""}<div class="shared-approval-operation" aria-label="実行する操作">${approval.request.details.map(detail => `<pre>${esc(detail)}</pre>`).join("")}</div>${risks.length ? `<p>確認事項: ${risks.map(esc).join("、")}</p>` : ""}`;
  const metadata = `<p>操作種別: ${esc(approval.request.access)}${approval.request.agent_task_name ? ` · ${esc(approval.request.agent_task_name)}` : ""}</p><p>有効期限: ${esc(new Date(approval.expires_at_ms).toLocaleString("ja-JP"))}</p>`;
  if (approval.status !== "pending") {
    // A different details owner closes the record when an expanded pending
    // explanation settles; subsequent reading still retains its open state.
    return `<section data-shared-region="approval" class="shared-card shared-approval-record" aria-labelledby="shared-approval-title"><details data-details-key="shared-approval-record-${esc(approval.id)}"><summary id="shared-approval-title">${approvalResultTitle(approval.status, approval.decision)} <span class="shared-secondary">（詳細）</span></summary>${operation}${metadata}</details></section>`;
  }
  return `<section data-shared-region="approval" class="shared-card shared-approval-pending" aria-labelledby="shared-approval-title"><h2 id="shared-approval-title">${approval.can_decide ? "あなたの承認を待っています" : "担当者の承認待ち"}</h2>${operation}${approval.can_decide ? `<p>許可は、この承認依頼に記載された操作にだけ適用されます。</p><div class="shared-actions">${button("approve", "この操作の実行を許可", approval.id, Boolean(local.pending))}${button("deny", "この操作を拒否", approval.id, Boolean(local.pending))}${button("stop", "仕事を停止", approval.id, Boolean(local.pending))}</div>` : "<p>担当者またはプロジェクト管理者が、この画面で判断できます。</p>"}<details data-details-key="shared-approval-${esc(approval.id)}"><summary>承認の詳細・確認事項</summary>${metadata}</details></section>`;
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
    if (active && document.activeElement?.matches("input,textarea,select") && ["login", "draft", "followup", "handover"].includes(nextRegion.dataset.sharedRegion ?? "")) {
      for (const field of region.querySelectorAll<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>("[data-shared-field]")) {
        const replacement = nextRegion.querySelector<HTMLInputElement>(`[data-shared-field="${field.dataset.sharedField}"]`);
        if (replacement) field.disabled = replacement.disabled;
      }
      for (const button of region.querySelectorAll<HTMLButtonElement>("button[data-action]")) {
        const replacement = Array.from(nextRegion.querySelectorAll<HTMLButtonElement>("button[data-action]")).find(candidate => candidate.dataset.action === button.dataset.action && candidate.dataset.value === button.dataset.value);
        if (replacement) { button.disabled = replacement.disabled; button.setAttribute("aria-disabled", String(replacement.disabled)); button.textContent = replacement.textContent; }
      }
    } else if (!["transcript", "detail"].includes(nextRegion.dataset.sharedRegion ?? "") || !retainWorkRecord(region, nextRegion)) region.replaceWith(nextRegion);
  }
  return true;
}

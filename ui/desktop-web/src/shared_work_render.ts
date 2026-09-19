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
  return `<section data-shared-region="approval" class="shared-card" aria-labelledby="shared-approval-title">${approval ? `<h2 id="shared-approval-title">実行の承認</h2><p>${esc(approval.request.summary)}</p><p>操作種別: ${esc(approval.request.access)}${approval.request.agent_task_name ? ` · ${esc(approval.request.agent_task_name)}` : ""}</p>${approval.request.details.map(detail => `<pre>${esc(detail)}</pre>`).join("")}<p>対象: ${approval.request.targets.map(esc).join("、") || "なし"}</p>${approval.request.outside_workspace ? "<p>実行環境の作業フォルダ外への操作を含みます。</p>" : ""}${approval.request.risks.length ? `<p>確認事項: ${approval.request.risks.map(esc).join("、")}</p>` : ""}${approval.status === "pending" ? `<p>有効期限: ${esc(new Date(approval.expires_at_ms).toLocaleString("ja-JP"))}</p>${approval.can_decide ? `<div class="shared-actions">${button("approve", "この操作の実行を許可", approval.id, Boolean(local.pending))}${button("deny", "この操作を拒否", approval.id, Boolean(local.pending))}${button("stop", "仕事を停止", approval.id, Boolean(local.pending))}</div>` : "<p>担当者またはプロジェクト管理者の判断を待っています。</p>"}` : `<p>承認状態: ${esc(approval.status)}${approval.decision ? ` · ${esc(approval.decision)}` : ""}</p>`}` : "<p>選択中の仕事に、現在の承認依頼はありません。</p>"}</section>`;
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
        const replacement = nextRegion.querySelector<HTMLButtonElement>(`button[data-action="${button.dataset.action}"]`);
        if (replacement) { button.disabled = replacement.disabled; button.setAttribute("aria-disabled", String(replacement.disabled)); button.textContent = replacement.textContent; }
      }
    } else if (!["transcript", "detail"].includes(nextRegion.dataset.sharedRegion ?? "") || !retainWorkRecord(region, nextRegion)) region.replaceWith(nextRegion);
  }
  return true;
}

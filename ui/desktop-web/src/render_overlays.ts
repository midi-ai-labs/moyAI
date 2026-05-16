import type { DesktopWebState } from "./types";
import { escapeHtml } from "./utils";

export type LocalConfirmation = {
  kind: "project" | "session" | "chat_session";
  index: number;
  title: string;
  detail: string;
};

export function renderConfirmation(state: DesktopWebState): string {
  const confirmation = state.confirmation ?? {
    summary: state.confirmation_text || "権限確認が必要です",
    targets: [],
    outside_workspace: false,
    risks: [],
  };
  const targets = confirmation.targets.length > 0 ? confirmation.targets.join(", ") : "(なし)";
  const risks = confirmation.risks.length > 0 ? confirmation.risks.join(", ") : "なし";
  return `
    <div class="modal-backdrop">
      <section class="modal confirmation" role="alertdialog" aria-modal="true">
        <h2>確認が必要です</h2>
        <div class="confirm-summary">${escapeHtml(confirmation.summary)}</div>
        <dl class="confirm-details">
          <dt>対象</dt><dd>${escapeHtml(targets)}</dd>
          <dt>ワークスペース外</dt><dd>${escapeHtml(confirmation.outside_workspace ? "はい" : "いいえ")}</dd>
          <dt>リスク</dt><dd>${escapeHtml(risks)}</dd>
        </dl>
        <div class="modal-actions">
          <button data-action="deny" autofocus>拒否</button>
          <button class="send wide-send" data-action="allow">許可</button>
        </div>
      </section>
    </div>
  `;
}

export function renderLocalConfirmation(confirm: LocalConfirmation): string {
  const target = confirm.kind === "project" ? "プロジェクト" : "チャット";
  const consequence =
    confirm.kind === "project"
      ? "履歴とセッション情報を削除します。ワークスペース内の実ファイルは削除しません。"
      : "このチャット履歴を削除します。ワークスペース内の実ファイルは削除しません。";
  return `
    <div class="modal-backdrop">
      <section class="modal confirmation" role="alertdialog" aria-modal="true">
        <h2>${target}を削除しますか？</h2>
        <div class="confirm-summary">${escapeHtml(confirm.title)}</div>
        <dl class="confirm-details">
          <dt>対象</dt><dd>${escapeHtml(confirm.detail)}</dd>
          <dt>影響</dt><dd>${escapeHtml(consequence)}</dd>
        </dl>
        <div class="modal-actions">
          <button data-action="cancel-local-confirm" autofocus>キャンセル</button>
          <button class="danger-button" data-action="confirm-local-delete">削除</button>
        </div>
      </section>
    </div>
  `;
}

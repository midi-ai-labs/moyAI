import type { DesktopWebState } from "./types";
import { escapeHtml } from "./utils";

export type LocalConfirmation = {
  kind: "project" | "session" | "chat_session" | "archive_session" | "unarchive_session" | "rollback_session";
  index: number;
  title: string;
  detail: string;
};

export function renderConfirmation(state: DesktopWebState): string {
  const confirmation = state.confirmation ?? {
    summary: state.confirmation_text || "権限確認が必要です",
    details: [],
    targets: [],
    outside_workspace: false,
    risks: [],
  };
  const targets = confirmation.targets.length > 0 ? confirmation.targets.join(", ") : "(なし)";
  const risks = confirmation.risks.length > 0 ? confirmation.risks.join(", ") : "なし";
  const details = confirmation.details.length > 0 ? confirmation.details.join("\n") : "なし";
  return `
    <div class="modal-backdrop">
      <section class="modal confirmation" role="alertdialog" aria-modal="true">
        <h2>確認が必要です</h2>
        <div class="confirm-summary">${escapeHtml(confirmation.summary)}</div>
        <div class="confirm-command" aria-label="実行内容">${escapeHtml(details)}</div>
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
  const archive = confirm.kind === "archive_session";
  const unarchive = confirm.kind === "unarchive_session";
  const rollback = confirm.kind === "rollback_session";
  const target = confirm.kind === "project" ? "プロジェクト" : "チャット";
  const verb = archive ? "アーカイブ" : unarchive ? "復元" : rollback ? "ロールバック" : "削除";
  const consequence = archive
    ? "このチャットを通常の一覧から隠します。履歴、実行証跡、ワークスペース内の実ファイルは削除しません。"
    : unarchive
      ? "このチャットを通常の一覧に戻します。履歴、実行証跡、ワークスペース内の実ファイルは変更しません。"
    : rollback
      ? "canonical history の最新 turn を削除し、session state / todo を残った履歴へ戻します。ワークスペース内の実ファイルは変更しません。"
    : confirm.kind === "project"
      ? "履歴とセッション情報を削除します。ワークスペース内の実ファイルは削除しません。"
      : "このチャット履歴を削除します。ワークスペース内の実ファイルは削除しません。";
  const archiveStateChange = archive || unarchive;
  const historyMutation = rollback;
  return `
    <div class="modal-backdrop">
      <section class="modal confirmation" role="alertdialog" aria-modal="true">
        <h2>${target}を${verb}しますか？</h2>
        <div class="confirm-summary">${escapeHtml(confirm.title)}</div>
        <dl class="confirm-details">
          <dt>対象</dt><dd>${escapeHtml(confirm.detail)}</dd>
          <dt>影響</dt><dd>${escapeHtml(consequence)}</dd>
        </dl>
        <div class="modal-actions">
          <button data-action="cancel-local-confirm" autofocus>キャンセル</button>
          <button class="${archiveStateChange ? "wide-send" : "danger-button"}" data-action="${archiveStateChange ? "confirm-local-archive-state" : historyMutation ? "confirm-local-rollback" : "confirm-local-delete"}">${verb}</button>
        </div>
      </section>
    </div>
  `;
}

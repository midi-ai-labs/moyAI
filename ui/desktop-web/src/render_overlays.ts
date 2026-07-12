import type { DesktopWebState, RowMutationTarget } from "./types.ts";
import { renderPermissionAgentIdentity } from "./render_agent_activity.ts";
import { escapeHtml } from "./utils.ts";

export type LocalConfirmation = {
  kind: "project" | "session" | "chat_session" | "archive_session" | "unarchive_session" | "rollback_session";
  index: number;
  title: string;
  detail: string;
  expectedTarget: RowMutationTarget;
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
  const agentPath = state.confirmation?.agent_path?.trim() ?? "";
  const agentTaskName = state.confirmation?.agent_task_name?.trim() ?? "";
  return `
    <div class="modal-backdrop">
      <section class="modal confirmation" role="alertdialog" aria-modal="true" aria-labelledby="permission-title" aria-describedby="permission-summary" tabindex="-1">
        <h2 id="permission-title">確認が必要です</h2>
        <div class="confirm-summary" id="permission-summary">${escapeHtml(confirmation.summary)}</div>
        <div class="confirm-command" aria-label="実行内容">${escapeHtml(details)}</div>
        <dl class="confirm-details">
          ${agentPath ? `<dt>要求元</dt><dd>${renderPermissionAgentIdentity(agentPath, agentTaskName)}</dd>` : ""}
          <dt>対象</dt><dd>${escapeHtml(targets)}</dd>
          <dt>ワークスペース外</dt><dd>${escapeHtml(confirmation.outside_workspace ? "はい" : "いいえ")}</dd>
          <dt>リスク</dt><dd>${escapeHtml(risks)}</dd>
        </dl>
        <div class="permission-decision-status" role="status" aria-live="polite" tabindex="-1"></div>
        <div class="modal-actions">
          <button data-action="deny" data-permission-action autofocus>拒否</button>
          <button class="send wide-send" data-action="allow" data-permission-action>許可</button>
        </div>
      </section>
    </div>
  `;
}

export function renderLocalConfirmation(confirm: LocalConfirmation, pending = false, error = ""): string {
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
      <section class="modal confirmation" role="alertdialog" aria-modal="true" aria-labelledby="local-confirm-title" aria-describedby="local-confirm-summary" tabindex="-1" ${pending ? 'aria-busy="true"' : ""}>
        <h2 id="local-confirm-title">${target}を${verb}しますか？</h2>
        <div class="confirm-summary" id="local-confirm-summary">${escapeHtml(confirm.title)}</div>
        <dl class="confirm-details">
          <dt>対象</dt><dd>${escapeHtml(confirm.detail)}</dd>
          <dt>影響</dt><dd>${escapeHtml(consequence)}</dd>
        </dl>
        <div class="permission-decision-status" role="status" aria-live="polite" tabindex="-1">${pending ? `${verb}を反映しています。` : escapeHtml(error)}</div>
        <div class="modal-actions">
          <button data-action="cancel-local-confirm" ${pending ? "disabled" : "autofocus"}>キャンセル</button>
          <button class="${archiveStateChange ? "wide-send" : "danger-button"}" data-action="${archiveStateChange ? "confirm-local-archive-state" : historyMutation ? "confirm-local-rollback" : "confirm-local-delete"}" ${pending ? "disabled" : ""}>${pending ? `${verb}しています…` : verb}</button>
        </div>
      </section>
    </div>
  `;
}

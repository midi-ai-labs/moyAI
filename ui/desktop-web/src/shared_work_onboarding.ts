import type { SharedWorkPresentation } from "./shared_work_state.ts";
import { escapeHtml } from "./utils.ts";

export const SAMPLE_TITLE = "最初の仕事：件数と合計の確認";
export const SAMPLE_PROMPT = "添付した moyai-sample-numbers.csv の value 列について、データの件数と合計を計算してください。結果を日本語で回答し、件数と合計を記載した moyai-sample-result.md を作業フォルダーに作成してください。ほかのファイルの変更や外部への接続は不要です。";

/** Presentation only: every step is derived from the current authenticated snapshot. */
export function renderSharedWorkOnboarding(local: SharedWorkPresentation): string {
  const p = local.projection;
  if (!p || local.conceal) return "";
  const project = p.projects.find(row => row.id === p.selected_project_id);
  const projectHint = "管理者に、このPCをプロジェクトの操作PCへ追加するよう依頼してください。";
  if (p.detail) return "";
  const steps = [
    [p.connected ? "接続済み" : p.enrollment === "pending" ? "管理者の承認待ち" : "未接続", "このPCの参加", p.connected ? "Hubに接続しています。" : "管理者から接続ファイルを受け取り、このPCの申請を承認してもらいます。"],
    [project ? "参加済み" : "未完了", "プロジェクト", project ? `${project.label} · ${project.can_submit ? "依頼できます" : "閲覧できます"}` : projectHint],
  ];
  const available = p.status?.environments.filter(row => row.enabled) ?? [];
  const executionHint = available.length ? "実行するPCを選んで、サンプルの仕事を試せます。"
    : p.status?.execution_devices?.some(row => row.preparation_state === "ready") || p.status?.environments.length
      ? "実行用に登録されたPCはありますが、この一覧には受付中のPCがありません。「PCの利用状況」で受付状態を確認してください。"
      : "管理者に実行PCの割り当てを依頼してください。PC担当者がAI・保存先・実行許可を設定すると送信できます。";
  const sampleEnabled = project?.can_submit && !local.pending && !p.submission_uncertain
    && !p.submission_storage_error && !local.prompt.trim() && !local.title.trim() && !p.inputs.length;
  const expanded = Boolean(p.principal) && !p.status?.jobs.length;
  return `<section data-shared-region="onboarding" class="shared-card shared-onboarding" aria-label="接続とプロジェクトの状況"><details data-details-key="shared-onboarding" ${expanded ? "open" : ""}><summary>接続状況とサンプル</summary><ol>${steps.map(([state, label, detail]) => `<li><strong>${escapeHtml(label)} · ${escapeHtml(state)}</strong><p>${escapeHtml(detail)}</p></li>`).join("")}</ol>
    ${project?.can_submit ? `<h3>最初の仕事を試す</h3><p>${executionHint}</p><p>数値3件（10・20・30）のファイルを添付し、依頼文を入力します。内容を確認して「送信」した後、回答が件数3・合計60になっているか、成果ファイルがあるかを確認してください。</p><button data-action="shared-prepare-sample" ${sampleEnabled ? "" : "disabled"}>サンプルの依頼を入力</button>${!sampleEnabled && (local.prompt.trim() || local.title.trim() || p.inputs.length) ? "<p>入力中の内容を保持しています。サンプルを使う場合は新しいチャットを開いてください。</p>" : ""}<p class="shared-secondary">操作の承認を求められた場合は、担当者かプロジェクト管理者がこの会話で判断します。</p>` : project ? "<p>このプロジェクトの会話と結果を閲覧できます。新しい依頼は、依頼する権限のある参加者が送信します。</p>" : "<p class=\"shared-secondary\">依頼・閲覧だけのPCでは、このPCにAIや実行用フォルダーを設定する必要はありません。</p>"}
  </details></section>`;
}

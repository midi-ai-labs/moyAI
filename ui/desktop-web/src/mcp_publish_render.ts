import { icon } from "./icons.ts";
import { escapeHtml } from "./utils.ts";
import {
  createPublishUiState, publishCanOperate, publishCanSave, publishDirty, publishEditor,
  publishErrorText, publishRow, publishValidation, publishTargetChoices, publishTargetKey,
  publishToolSupported, publishCanCreateCertificate, samePublishTarget, NEW_PUBLISH_PROFILE, PUBLISH_TOOLS,
  type PublishPresentation, type PublishProfileRow, type PublishJob,
} from "./mcp_publish_state.ts";

const statusLabel: Record<PublishProfileRow["status"], string> = {
  stopped: "停止中", starting: "開始しています", running: "配信中", stopping: "停止しています", error: "開始できません",
};
const disabled = (available: boolean) => available ? "" : "disabled";
const selected = (actual: string | undefined, expected: string) => actual === expected ? "selected" : "";
function status(row: PublishProfileRow | null): string {
  return `<span class="mcp-publish-status" data-status="${row?.status ?? "stopped"}">${row ? statusLabel[row.status] : "未保存"}</span>`;
}
function callStatus(value: string): string {
  return ({ running: "実行中", completed: "完了", failed: "失敗", cancelled: "停止" } as Record<string, string>)[value] ?? "状態不明";
}
const jobStatus: Record<PublishJob["state"], string> = {
  accepted: "受付済み", running: "実行中", awaiting_approval: "この端末で承認待ち", cancelling: "停止を要求中", completed: "完了", failed: "失敗", interrupted: "停止済み",
};
function noJobResult(state: PublishJob["state"]): string {
  if (state === "interrupted") return "タスクは停止しました。返却された結果はありません。";
  if (state === "failed") return "タスクは失敗しました。返却された結果はありません。";
  if (state === "completed") return "タスクは完了しました。返却された結果はありません。";
  return "結果はまだ届いていません。";
}
function renderJobs(local: PublishPresentation): string {
  const jobs = local.jobs.filter((job) => job.profile_id === local.selectedId);
  return `${local.jobsError ? `<p class="mcp-publish-help" role="status">${escapeHtml(local.jobsError)}</p>` : ""}
    ${jobs.length ? [...jobs].reverse().map((job) => `<article class="mcp-publish-job" aria-labelledby="mcp-publish-job-${escapeHtml(job.job_id)}">
      <div class="mcp-publish-section-heading"><h4 id="mcp-publish-job-${escapeHtml(job.job_id)}">${escapeHtml(job.prompt_preview || "受け付けたタスク")}</h4><span class="mcp-publish-status" data-status="${job.state}">${jobStatus[job.state]}</span></div>
      <p class="mcp-publish-help">委任元: ${escapeHtml(job.parent.peer_id)}（接続元の申告） · モデル: ${escapeHtml(job.model)}</p>
      <p class="mcp-publish-help">親タスク: ${escapeHtml(job.parent.task_id)} · ジョブ: ${escapeHtml(job.job_id)}</p>
      ${job.result !== null ? `<details id="mcp-publish-result-${escapeHtml(job.job_id)}" data-details-key="mcp-publish-result-${escapeHtml(job.job_id)}"><summary>結果を表示${job.result_truncated ? "（一部省略）" : ""}</summary><pre class="mcp-publish-job-result">${escapeHtml(job.result)}</pre></details>` : `<p class="mcp-publish-help">${noJobResult(job.state)}</p>`}
      ${job.can_stop ? `<button id="mcp-publish-stop-job-${escapeHtml(job.job_id)}" class="mcp-publish-danger" data-action="mcp-publish-stop-job" data-value="${escapeHtml(job.job_id)}" ${disabled(!local.pending)}>このタスクを停止</button>` : ""}
    </article>`).join("") : '<p class="mcp-publish-help">このプロファイルで受け付けたタスクはありません。</p>'}`;
}

export function renderPublishOverlay(input?: PublishPresentation): string {
  const local = input ?? createPublishUiState();
  const projection = local.projection;
  const editor = publishEditor(local);
  const row = publishRow(local);
  const credentialId = row?.credential_configured && row.profile.authentication.kind === "local_credential"
    ? row.profile.authentication.credential_id : "";
  const isNew = local.selectedId === NEW_PUBLISH_PROFILE;
  const editable = Boolean(editor && !local.pending && (isNew || row?.can_edit));
  const dirty = Boolean(editor && publishDirty(editor));
  const tools = editor?.value.tools ?? [];
  const agentMode = editor?.value.mode.kind === "agent";
  const accessMode = editor?.value.mode.kind === "agent" ? editor.value.mode.access_mode : "default";
  const target = editor?.value.target;
  const targetChoices = publishTargetChoices(local);
  const targetFound = targetChoices.some((choice) => samePublishTarget(choice.target, target));
  const targetInfo = target?.kind === "temp"
    ? agentMode ? "tempではプロジェクトを選ばずにタスクを受け付けます。実行範囲と権限は、この端末の受付設定に従います。"
      : "tempはプロジェクトを使わず、現在時刻だけを公開できます。ファイル操作の選択は解除されます。公開するツールは下で選んでください。"
    : target ? `${escapeHtml(target.workspace_root)}${target.kind === "legacy_session"
      ? "<code>旧設定のチャット・フォルダ範囲を維持しています。プロジェクトを選び直すと公開範囲が切り替わります。</code>"
      : "<code>このプロジェクトのフォルダーを公開します。チャットの作成や送信は不要です。</code>"}`
      : "公開するプロジェクトを選択してください。フォルダーを公開しない場合はtempを選べます。";
  const issue = local.error || (projection?.error ? publishErrorText(projection.error) : "");
  const validation = editor ? publishValidation(local) : null;
  const deleteVisible = local.deleteConfirmation !== null && local.deleteConfirmation === local.selectedId;
  const statusDetail = row?.status_message ?? (row?.status === "running"
    ? `処理中の要求 ${row.active_calls} 件。公開する対象とツールは保存した設定に固定されています。`
    : row && row.profile.mode.kind === "read_tools" && !row.profile.tools.length ? "公開する読み取りツールを1つ以上選び、設定を保存してください。"
      : row && !targetFound ? "保存した公開対象を確認できません。対象を確認して最新情報を取得してください。"
        : row?.credential_configured ? "設定を保存しても配信は始まりません。「配信を開始」で公開します。"
      : "設定を保存し、接続用トークンを発行してから配信を開始します。");
  return `<div class="modal-backdrop">
    <section class="modal settings-modal mcp-publish-modal" data-modal="mcp_publish" data-surface="mcp_publish" data-profile-id="${escapeHtml(local.selectedId ?? "")}" data-credential-id="${escapeHtml(credentialId)}" role="dialog" aria-modal="true" aria-labelledby="mcp-publish-title" aria-describedby="mcp-publish-scope" tabindex="-1">
      <header class="mcp-publish-header"><div><span class="mcp-publish-eyebrow">LYNX · WORKSPACE SHARING</span>
        <h2 id="mcp-publish-title">旧配信設定の管理</h2><p id="mcp-publish-scope">保存済みの手動配信設定を管理します。Hub経由の受付と証明書の自動設定は「moyAI Hub」の端末連携から操作してください。</p></div>
        <button class="icon-button" data-action="close-overlay" aria-label="閉じる" title="閉じる">${icon("x")}</button></header>
      <div class="mcp-publish-body">
        <aside class="mcp-publish-sidebar" aria-label="配信プロファイル">
          <button id="mcp-publish-add" data-action="mcp-publish-add" ${disabled(Boolean(projection && !local.pending && projection.profiles.length < 32))}>＋ プロファイルを追加</button>
          <nav class="mcp-publish-profile-list" data-settings-passive="mcp-publish-profiles" data-settings-preserve-focused-region aria-label="保存した配信設定">
            ${(projection?.profiles ?? []).map((candidate) => `<button class="mcp-publish-profile" id="mcp-publish-profile-${escapeHtml(candidate.profile.id)}" data-action="mcp-publish-select" data-value="${escapeHtml(candidate.profile.id)}" aria-pressed="${local.selectedId === candidate.profile.id}" ${disabled(!local.pending)}>
              <strong>${escapeHtml(candidate.profile.label)}</strong>${status(candidate)}<small>${local.drafts[candidate.profile.id] && publishDirty(local.drafts[candidate.profile.id]) ? "未保存の変更あり" : candidate.profile.mode.kind === "agent" ? "エージェント受付" : `${candidate.profile.tools.length} 種のツール`}</small></button>`).join("")}
            ${local.drafts[NEW_PUBLISH_PROFILE] ? `<button class="mcp-publish-profile" id="mcp-publish-profile-new" data-action="mcp-publish-select" data-value="new" aria-pressed="${isNew}" ${disabled(!local.pending)}><strong>新しい配信設定</strong><small>未保存</small></button>` : ""}
          </nav>
        </aside>
        <div class="mcp-publish-content settings-content">
          <div data-mcp-section="empty" class="mcp-publish-empty" ${editor ? "hidden" : ""}><h3>公開する範囲を選びましょう</h3><p class="mcp-publish-help">配信プロファイルに名前を付け、対象と読み取りツールを選びます。<br>保存と開始は別の操作です。</p></div>
          <div data-mcp-section="editor" ${editor ? "" : "hidden"}>
            <section class="mcp-publish-section" aria-labelledby="mcp-publish-settings-title">
              <div class="mcp-publish-section-heading"><h3 id="mcp-publish-settings-title">配信する内容</h3><span class="mcp-publish-help" data-settings-passive="mcp-publish-dirty">${dirty ? "未保存の変更があります" : "保存済み"}</span></div>
              <div class="mcp-publish-fields">
                <label class="mcp-publish-field wide">表示名<input id="mcp-publish-label" class="settings-control" data-mcp-publish-field="label" value="${escapeHtml(editor?.value.label ?? "")}" placeholder="例: プロジェクトの調査" maxlength="80" autocomplete="off" ${disabled(editable)} /></label>
                <label class="mcp-publish-field wide">公開モード<select id="mcp-publish-mode" class="settings-control" data-mcp-publish-field="mode" ${disabled(editable)}>
                  <option value="read_tools" ${selected(editor?.value.mode.kind ?? "read_tools", "read_tools")}>読み取りツールを公開</option>
                  <option value="agent" ${selected(editor?.value.mode.kind, "agent")}>エージェントとしてタスクを受付</option></select></label>
                <div class="wide" data-mcp-section="agent-permission" ${agentMode ? "" : "hidden"}>
                  <label class="mcp-publish-field">実行権限<select id="mcp-publish-access-mode" class="settings-control" data-mcp-publish-field="access_mode" ${disabled(editable && agentMode)}>
                    <option value="default" ${selected(accessMode, "default")}>既定 — 承認が必要な操作を拒否</option>
                    <option value="auto_review" ${selected(accessMode, "auto_review")}>自動レビュー — 操作をレビューして判断</option>
                    <option value="full_access" ${selected(accessMode, "full_access")}>フルアクセス — 承認を省略して実行</option></select></label>
                  <p class="mcp-publish-help">委任元の権限は引き継ぎません。この中間版は遠隔タスクの対話承認に未対応です。フルアクセスではファイル変更やコマンドも承認なしで実行できます。</p>
                  <p class="mcp-publish-help">タスクはこの端末で実行します。モデルは配信開始時のグローバルMain Direct設定を使い、モデルの接続先と実行端末は別です。</p>
                </div>
                <p class="mcp-publish-help wide">モードや実行権限を変更して保存すると、以前のトークンは失効します。再発行して接続先へ渡してください。</p>
                <label class="mcp-publish-field wide">公開するプロジェクト
                  <select id="mcp-publish-target" class="settings-control" data-mcp-publish-field="target" ${disabled(editable)}>
                    ${!targetFound ? `<option value="" selected disabled>${target ? "保存した対象は現在利用できません" : "公開するプロジェクトを選択してください"}</option>` : ""}
                    ${targetChoices.map((choice) => `<option value="${escapeHtml(publishTargetKey(choice.target))}" ${samePublishTarget(target, choice.target) ? "selected" : ""}>${escapeHtml(choice.label)}</option>`).join("")}
                  </select></label>
                <div class="mcp-publish-target-info wide" data-settings-passive="mcp-publish-target-info">${targetInfo}</div>
                <label class="mcp-publish-field">待受アドレス<input id="mcp-publish-host" class="settings-control" data-mcp-publish-field="host" value="${escapeHtml(editor?.host ?? "127.0.0.1")}" autocomplete="off" spellcheck="false" ${disabled(editable)} /></label>
                <label class="mcp-publish-field">ポート<input id="mcp-publish-port" class="settings-control" data-mcp-publish-field="port" value="${escapeHtml(editor?.port ?? "7332")}" inputmode="numeric" autocomplete="off" ${disabled(editable)} /></label>
              </div>
              <p class="mcp-publish-help">別端末へ配信する場合は、この端末のIPアドレスとTLS証明書を設定します。配信中の設定変更は、先に停止してください。</p>
              <label class="mcp-publish-field">接続の暗号化<select id="mcp-publish-tls" class="settings-control" data-mcp-publish-field="tls" ${disabled(editable)}>
                <option value="disabled" ${editor?.value.tls ? "" : "selected"}>同じPCのみ — TLSなし</option>
                <option value="enabled" ${editor?.value.tls ? "selected" : ""}>TLSを使用 — 別端末からも接続可</option></select></label>
              <div data-mcp-section="tls" ${editor?.value.tls ? "" : "hidden"}>
                <p class="mcp-publish-help">まずプロファイルを保存してください。その後、待受IPを入力して証明書を作成できます。作成後は設定を保存し、公開証明書を接続側へ渡してください。</p>
                <div class="mcp-publish-actions"><button id="mcp-publish-create-certificate" data-action="mcp-publish-create-certificate" ${disabled(publishCanCreateCertificate(local))}>入力中のIPで証明書を作成</button>
                  <button data-action="mcp-publish-copy-certificate" ${disabled(Boolean(row?.profile.tls && !local.pending))}>保存済みの公開証明書をコピー</button></div>
                <div class="mcp-publish-fields">
                  <label class="mcp-publish-field wide">証明書ファイル<input id="mcp-publish-certificate-path" class="settings-control" data-mcp-publish-field="certificate_path" value="${escapeHtml(editor?.value.tls?.certificate_path ?? "")}" autocomplete="off" spellcheck="false" ${disabled(editable)} /></label>
                  <label class="mcp-publish-field wide">秘密鍵ファイル<input id="mcp-publish-private-key-path" class="settings-control" data-mcp-publish-field="private_key_path" value="${escapeHtml(editor?.value.tls?.private_key_path ?? "")}" autocomplete="off" spellcheck="false" ${disabled(editable)} /></label>
                </div>
                <p class="mcp-publish-help">秘密鍵はこの端末だけで保持します。接続先に渡すのは公開証明書と接続用トークンです。</p>
              </div>
              <div data-mcp-section="read-tools" ${agentMode ? "hidden" : ""}>
              <h3 id="mcp-publish-tools-title">公開する読み取りツール</h3>
              <div class="mcp-publish-tools" role="group" aria-labelledby="mcp-publish-tools-title">${PUBLISH_TOOLS.map((tool) => `<label class="mcp-publish-tool">
                <input type="checkbox" id="mcp-publish-tool-${tool.name}" class="settings-control" data-mcp-publish-field="tool:${tool.name}" ${tools.includes(tool.name) ? "checked" : ""} ${disabled(editable && !agentMode && publishToolSupported(target, tool.name))} />
                <span><strong>${tool.label}</strong><small>${tool.description}</small></span></label>`).join("")}</div></div>
              <div class="mcp-publish-fields">
                <label class="mcp-publish-field">同時実行する要求の上限<input id="mcp-publish-concurrency" class="settings-control" data-mcp-publish-field="concurrency" value="${escapeHtml(editor?.concurrency ?? "1")}" inputmode="numeric" ${disabled(editable)} /></label>
                <label class="mcp-publish-field">Desktopのウィンドウを閉じたとき<select id="mcp-publish-background" class="settings-control" data-mcp-publish-field="background" ${disabled(editable)}>
                  <option value="stop_when_window_closes" ${selected(editor?.value.background, "stop_when_window_closes")}>配信を停止する</option>
                  <option value="keep_while_application_running" ${selected(editor?.value.background, "keep_while_application_running")}>トレイに格納して配信を続ける</option></select></label>
              </div>
              <p class="mcp-publish-help">アプリを完全終了すると、すべての配信を停止します。次回起動時に自動では開始しません。</p>
            </section>
            <section class="mcp-publish-section" aria-labelledby="mcp-publish-runtime-title">
              <div class="mcp-publish-section-heading"><h3 id="mcp-publish-runtime-title">配信の状態</h3><span data-settings-passive="mcp-publish-status">${status(row)}</span></div>
              <p class="mcp-publish-help" data-settings-passive="mcp-publish-runtime-detail" role="status">${escapeHtml(statusDetail)}</p>
              <p class="mcp-publish-help" data-settings-passive="mcp-publish-clients">接続中のMCPセッション ${row?.connected_sessions ?? 0} 件 · 処理中 ${row?.active_calls ?? 0} 件</p>
              <details id="mcp-publish-recent" data-details-key="mcp-publish-recent"><summary>直近のツール呼び出し</summary><div class="mcp-publish-call-list" data-settings-passive="mcp-publish-calls">${row?.recent_calls.length
                ? [...row.recent_calls].reverse().map((call) => `<p><strong>${escapeHtml(PUBLISH_TOOLS.find((tool) => tool.name === call.tool)?.label ?? call.tool)}</strong><span>${callStatus(call.status)}</span><small>${escapeHtml(call.id)}</small></p>`).join("")
                : '<p class="mcp-publish-help">呼び出しはありません。引数や読み取った本文はここに表示しません。</p>'}</div></details>
              <div class="mcp-publish-actions"><button class="mcp-publish-primary" id="mcp-publish-start" data-action="mcp-publish-start" ${disabled(publishCanOperate(local, "start"))}>配信を開始</button>
                <button class="mcp-publish-danger" id="mcp-publish-stop" data-action="mcp-publish-stop" ${disabled(publishCanOperate(local, "stop"))}>配信を停止</button></div>
            </section>
            <section class="mcp-publish-section" aria-labelledby="mcp-publish-jobs-title">
              <h3 id="mcp-publish-jobs-title">受け付けたタスク</h3>
              <p class="mcp-publish-help">この端末で実行するタスクの状態と最終結果です。配信停止後も、実行が終わるまで確認してください。</p>
              <div data-settings-passive="mcp-publish-jobs" data-settings-preserve-focused-region>${renderJobs(local)}</div>
            </section>
            <section class="mcp-publish-section" aria-labelledby="mcp-publish-connection-title">
              <div class="mcp-publish-section-heading"><h3 id="mcp-publish-connection-title">接続側アプリへ渡す情報</h3><span class="mcp-publish-help" data-settings-passive="mcp-publish-credential">${row?.credential_configured ? "トークン発行済み" : "トークン未発行"}</span></div>
              <div class="mcp-publish-target-info" data-settings-passive="mcp-publish-endpoint">Streamable HTTP<code>${escapeHtml(row?.endpoint ?? "配信を開始すると接続先URLを表示します。")}</code></div>
              <label class="mcp-publish-field">今回発行した接続用トークン<input id="mcp-publish-token" class="settings-control mcp-publish-secret" data-mcp-publish-field="token" type="password" readonly autocomplete="off" placeholder="発行直後だけ表示します" aria-describedby="mcp-publish-token-help" /></label>
              <p id="mcp-publish-token-help" class="mcp-publish-help">発行・再発行すると以前のトークンは使えなくなります。画面を閉じたり別のプロファイルへ移動すると再表示できません。必要な接続先だけに渡してください。</p>
              <div class="mcp-publish-actions"><button id="mcp-publish-issue-token" data-action="mcp-publish-issue-token" ${disabled(publishCanOperate(local, "issue_token"))}>トークンを発行・再発行</button>
                <button data-action="mcp-publish-copy-token" ${disabled(Boolean(row?.credential_configured && !local.pending))}>トークンをコピー</button>
                <button data-action="mcp-publish-revoke-token" ${disabled(publishCanOperate(local, "revoke_token"))}>トークンを失効</button></div>
              <button id="mcp-publish-copy-config" data-action="mcp-publish-copy-config" ${disabled(Boolean(row?.endpoint && row.credential_configured && !local.pending))}>接続側のMCP設定例をコピー</button>
              <p class="mcp-publish-help">URLとBearerトークンを含むJSON例です。貼り付け先アプリのStreamable HTTP設定形式に合わせてください。このDesktopの「接続先MCP」設定へ追加する操作ではありません。</p>
            </section>
            <section class="mcp-publish-section" aria-label="プロファイルの削除"><button class="mcp-publish-danger" data-action="mcp-publish-delete" ${disabled(publishCanOperate(local, "delete"))}>このプロファイルを削除</button>
              <div class="mcp-publish-feedback" data-settings-passive="mcp-publish-delete-confirm" ${deleteVisible ? "" : "hidden"}>「${escapeHtml(row?.profile.label ?? "") }」を削除します。接続用トークンも使えなくなります。
                <div class="mcp-publish-actions"><button class="mcp-publish-danger" data-action="mcp-publish-delete" ${disabled(publishCanOperate(local, "delete"))}>削除する</button><button data-action="mcp-publish-cancel-delete">キャンセル</button></div></div>
            </section>
          </div>
        </div>
      </div>
      <footer class="mcp-publish-footer"><div id="mcp-publish-feedback" class="mcp-publish-feedback" data-settings-passive="mcp-publish-feedback" data-error="${Boolean(issue)}" role="status" aria-live="polite">${escapeHtml(issue || local.notice || validation || "この設定画面を閉じても、配信状態は変わりません。")}</div>
        <div class="mcp-publish-actions"><button class="mcp-publish-primary" id="mcp-publish-save" data-action="mcp-publish-save" aria-describedby="mcp-publish-feedback" ${disabled(publishCanSave(local))}>${local.pending === "save" ? "保存しています…" : "設定を保存"}</button>
          <button data-action="mcp-publish-discard" ${disabled(Boolean(dirty && !local.pending))}>変更を破棄</button></div>
        <div class="mcp-publish-actions"><button data-action="mcp-publish-refresh" ${disabled(!local.pending)}>最新情報を取得</button><button data-action="close-overlay">閉じる</button></div></footer>
    </section></div>`;
}

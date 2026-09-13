import { escapeHtml } from "./utils.ts";
import { renderMarkdown } from "./markdown.ts";
import { mcpHistoryRegionHasSelection } from "./mcp_history_dom.ts";
import { providerDraftMatches, type SharedWorkPresentation } from "./shared_work_state.ts";
const esc = (value: unknown) => escapeHtml(String(value ?? ""));
const button = (action: string, label: string, value = "", disabled = false) => `<button data-action="shared-${action}" data-value="${esc(value)}" ${disabled ? "disabled" : ""}>${esc(label)}</button>`;
const pretty = (value: unknown): string => typeof value === "string" ? value : JSON.stringify(value, null, 2);
const record = (value: unknown): Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : {};
const text = (value: unknown): string => typeof value === "string" ? value : "";
function renderTranscriptItem(item: { position: number; kind: string; payload: unknown }, owner: string): string {
  const payload = record(item.payload);
  let label = ({ user_turn: "依頼", steer_turn: "追加の指示", assistant_message: "応答", tool_call: "操作", tool_output: "操作結果", error: "エラー", file_change: "ファイルの変更", compaction: "会話の要約", user: "依頼", assistant: "応答", tool: "操作結果", event: "実行記録" } as Record<string, string>)[item.kind] ?? `実行記録（${item.kind}）`;
  let body = "";
  if (["user_turn", "steer_turn", "assistant_message", "user", "assistant"].includes(item.kind)) {
    const parts = Array.isArray(payload.content) ? payload.content : [];
    body = parts.map(part => {
      const content = record(part);
      if (content.kind === "text" && typeof content.text === "string") return renderMarkdown(content.text);
      return `<p class="shared-secondary">${content.kind === "image" ? "画像を含みます。" : "本文以外の内容を含みます。"}元データは詳細で確認できます。</p>`;
    }).join("");
    if (!parts.length) body = renderMarkdown(text(item.payload) || text(payload.content));
    body = `<div class="markdown-body">${body || "<p>本文はありません。元データは詳細で確認できます。</p>"}</div>`;
  } else if (item.kind === "tool_call") {
    label = `操作: ${text(payload.tool_name) || "ツール"}`;
  } else if (item.kind === "tool_output" || item.kind === "tool") {
    if (text(payload.title)) label = `操作結果: ${text(payload.title)}`;
    body = `<pre>${esc(text(payload.output_text) || text(item.payload))}</pre>`;
  } else if (["error", "file_change", "compaction"].includes(item.kind)) {
    body = `<div class="markdown-body">${renderMarkdown(text(payload.message) || text(payload.summary))}</div>`;
  } else {
    body = "<p class=\"shared-secondary\">この記録の内容は詳細で確認できます。</p>";
  }
  return `<article class="shared-job"><h3>${esc(label)} · ${item.position}</h3>${body}<details data-details-key="shared-transcript:${esc(owner)}:${item.position}"><summary>記録の詳細</summary><pre>${esc(pretty(item.payload))}</pre></details></article>`;
}

/** Disclosure, selection and focused summaries remain browser-owned within one person/job/page. */
export function retainWorkRecord(current: HTMLElement, next: HTMLElement): boolean {
  if (!current.dataset.sharedRecordOwner || current.dataset.sharedRecordOwner !== next.dataset.sharedRecordOwner) return false;
  const open = new Map(Array.from(current.querySelectorAll<HTMLDetailsElement>("details[data-details-key]"), detail => [detail.dataset.detailsKey, detail.open]));
  for (const detail of next.querySelectorAll<HTMLDetailsElement>("details[data-details-key]")) {
    const value = open.get(detail.dataset.detailsKey);
    if (value !== undefined) detail.open = value;
  }
  return current.isEqualNode(next) || mcpHistoryRegionHasSelection(current)
    || Boolean(current.contains(current.ownerDocument.activeElement) && current.ownerDocument.activeElement?.matches("summary"));
}
export function workRecordOwner(local: SharedWorkPresentation): string {
  const p = local.projection!;
  return encodeURIComponent(JSON.stringify([p.generation, p.principal?.user_id, p.selected_project_id, p.selected_job_id]));
}
export function renderWorkResult(result: unknown, owner: string): string {
  if (result === null) return "<p>結果はまだありません。</p>";
  const value = record(result);
  const answer = text(result) || text(value.text) || text(value.message) || text(value.error);
  return `<h3>結果</h3><div class="markdown-body">${answer ? renderMarkdown(answer) : "<p>この結果は詳細で確認できます。</p>"}</div><details data-details-key="shared-result:${esc(owner)}"><summary>結果の詳細</summary><pre>${esc(pretty(result))}</pre></details>`;
}
function textField(local: SharedWorkPresentation, name: string, label: string, type = "text"): string {
  return `<label>${esc(label)}<input id="shared-${name}" data-shared-field="draft:${name}" type="${type}" value="${esc(local.draft[name])}" ${local.pending ? "disabled" : ""}></label>`;
}
export function renderStartDeadline(local: SharedWorkPresentation, name: "startBefore" | "followupStartBefore"): string {
  return `${textField(local, name, "開始期限（空欄は投入から24時間）", "datetime-local")}<p class="shared-secondary">期限を過ぎた仕事は新たに開始・再開しません。開始済みの処理を打ち切る期限ではありません。</p>`;
}
function select(local: SharedWorkPresentation, name: string, label: string, choices: [string, string][], fallback = "", placeholder = "選択してください"): string {
  const selected = local.draft[name] || fallback;
  return `<label>${esc(label)}<select id="shared-${name}" data-shared-field="draft:${name}" ${local.pending ? "disabled" : ""}><option value="">${esc(placeholder)}</option>${choices.map(([value, text]) => `<option value="${esc(value)}" ${selected === value ? "selected" : ""}>${esc(text)}</option>`).join("")}</select></label>`;
}
export function renderWorkInputs(local: SharedWorkPresentation): string {
  const p = local.projection!;
  return `<section data-shared-region="inputs" class="shared-card"><h2>添付する入力ファイル</h2><p>1件8 MiB、32件まで。選択時点の内容をHubへ保存し、次の仕事に渡します。</p>${p.inputs.map(a => `<p>${esc(a.name)} · ${a.byte_length.toLocaleString()} bytes ${button("remove-input", "添付から外す", a.id, Boolean(local.pending || p.submission_uncertain))}</p>`).join("")}${button("upload-inputs", "ファイルを選択して添付", "", Boolean(local.pending || p.submission_uncertain))}</section>`;
}
export function renderWorkInbox(local: SharedWorkPresentation): string {
  const inbox = local.projection?.inbox;
  return `<section data-shared-region="inbox" class="shared-card"><h2>お知らせ・要対応 ${inbox?.unread_count ? `<span aria-label="未読">${inbox.unread_count}</span>` : ""}</h2>${inbox?.items.map(item => `<article class="shared-job"><p>${item.read_at_ms === null ? "未読 · " : ""}${esc(({ approval: "実行の承認", finished: "仕事の終了", handover: "担当の引継ぎ" } as Record<string, string>)[item.kind] ?? item.kind)} · ${esc(new Date(item.created_at_ms).toLocaleString("ja-JP"))}</p>${button("inbox-open", item.title, item.id, Boolean(local.pending))}${item.can_act ? "<p>この仕事で対応できる操作があります。</p>" : ""}</article>`).join("") || "<p>現在のお知らせはありません。</p>"}<div class="shared-actions">${inbox?.next_before ? button("inbox-next", "以前のお知らせ", "", Boolean(local.pending)) : ""}${button("inbox-latest", "最新のお知らせ", "", Boolean(local.pending))}</div></section>`;
}
export function renderWorkDetails(local: SharedWorkPresentation): string {
  const p = local.projection!, detail = p.detail, busy = Boolean(local.pending);
  const transcriptOwner = encodeURIComponent(JSON.stringify([p.generation, p.principal?.user_id, p.selected_project_id, p.selected_job_id, p.transcript?.items[0]?.position ?? null]));
  return `<section data-shared-region="assets" class="shared-card"><h2>入力と成果ファイル</h2>${p.assets.map(a => `<article class="shared-job"><h3>${esc(a.name)}</h3><p>${a.kind === "input" ? "入力" : "成果"} · ${a.byte_length.toLocaleString()} bytes · 版 ${a.version}</p><p class="shared-secondary">SHA-256: ${esc(a.sha256)}</p><div class="shared-actions">${a.purged_at_ms ? "<p>保持期限により内容は削除済みです。</p>" : ""}${button("save-asset", "名前を付けて保存", a.id, busy || a.purged_at_ms !== null)}${a.kind !== "input" ? button("import-asset", "元の版と照合して取り込む", a.id, busy || a.purged_at_ms !== null) : ""}</div></article>`).join("") || "<p>選択した仕事のファイルはまだありません。</p>"}</section>
  <section data-shared-region="transcript" data-shared-record-owner="${esc(transcriptOwner)}" class="shared-card"><h2>共有された会話</h2>${p.transcript?.items.map(item => renderTranscriptItem(item, transcriptOwner)).join("") || "<p>共有済みの会話はまだありません。</p>"}${p.transcript?.next_after !== null && p.transcript ? button("transcript-next", "続きの会話を表示", "", busy) : ""}</section>
  <section data-shared-region="followup" class="shared-card"><h2>追加の依頼</h2>${detail?.can_continue ? `<p>この仕事の会話を引き継ぐ新しい仕事を作成します。上で添付した入力ファイルも渡します。</p><label>追加の依頼内容<textarea id="shared-followup" data-shared-field="draft:followup" rows="4" ${busy ? "disabled" : ""}>${esc(local.draft.followup)}</textarea></label>${renderStartDeadline(local, "followupStartBefore")}${button("continue", "会話を引き継いで依頼", "", busy || !local.draft.followup?.trim() || p.submission_uncertain)}` : "<p>終了した仕事に、会話の保存と継続する権限がある場合に依頼できます。</p>"}</section>
  <section data-shared-region="handover" class="shared-card"><h2>担当の引継ぎ</h2>${p.handover?.pending ? `<p>担当変更を受け付けました。実行が安全に区切れるまで待っています。</p>` : ""}${p.handover?.can_handover ? select(local, "assigneeId", "次の担当者", p.handover.candidates.map(u => [u.user_id, u.display_name])) + button("handover", "この利用者へ引き継ぐ", "", busy || !local.draft.assigneeId) : "<p>現在の担当者またはプロジェクト管理者が引き継ぎます。</p>"}</section>`;
}
export function renderWorkProvider(local: SharedWorkPresentation): string {
  const p = local.projection, provider = p?.provider, busy = Boolean(local.pending);
  const key = encodeURIComponent(JSON.stringify([p?.generation, provider?.runner_id]));
  const accessLabel = (value: string) => ({ default: "必要な操作を確認（標準）", auto_review: "自動レビューに確認を任せる", full_access: "操作の確認を省略" } as Record<string, string>)[value] ?? value;
  const stateLabel = provider && (({ available: "現在は受付できません", paused: "受付を一時停止中", draining: "実行完了を待って停止", maintenance: "保守中", stopping: "停止処理中" } as Record<string, string>)[provider.state] ?? provider.state);
  const prepared = p?.provider_draft, matches = providerDraftMatches(local);
  const environmentLabel = (id: string) => p?.status?.environments.find(env => env.id === id)?.label ?? id;
  return `<section data-shared-region="provider" data-shared-form-owner="${esc(local.draft.editTemplateId)}" class="shared-card shared-provider"><h2>このPCで仕事を受け付ける</h2>
    <p>ほかの端末から依頼された仕事を、このPCで実行するための設定です。仕事を依頼・確認するだけなら、設定は不要です。</p>
    ${p?.provider_error ? `<p class="shared-error">${esc(p.provider_error)}</p>` : ""}
    ${!p?.connected ? "<p>まず上の共通設定の読み込みと端末の参加承認を完了してください。</p>" : ""}
    <div class="shared-actions">${button("provider-status", provider ? "受付状況を更新" : "このPCの状態を確認", "", busy)}${!provider || p?.provider_error ? button("provider-start", "実行用アプリ（Runner）を起動", "", busy) : ""}</div>
    ${provider ? `<p class="shared-provider-status">${provider.mode === "shared" ? `共有仕事: ${provider.accepting ? "受付中" : esc(stateLabel)}` : "Runnerを確認しました（ひな形は未公開）"}</p>${provider.error ? `<p class="shared-error">${esc(provider.error)}</p>` : ""}
    ${provider.mode === "shared" ? `<div class="shared-actions">${button(provider.accepting ? "provider-pause" : "provider-resume", provider.accepting ? "新しい仕事の受付を止める" : "仕事の受付を再開", "", busy)}</div>` : ""}` : `<p class="shared-secondary">初めて使う場合はRunnerを起動し、状態を確認してください。このPCには実行に使うAIの設定も必要です。</p>`}
    <div class="shared-provider-setup"><h3>作業フォルダのひな形</h3><p>保存先と実行の権限をまとめて公開します。Hub管理者はこのひな形から、プロジェクト用のフォルダを作成できます。</p>
    ${provider?.templates.length ? select(local, "editTemplateId", "作成・変更するひな形", provider.templates.map(t => [t.id, t.label]), "", "新しいひな形を作る") : ""}
    ${textField(local, "templateLabel", "ひな形の名前（例: 解析チームの作業フォルダ）")}
    ${select(local, "accessMode", "実行の権限", [["default", "必要な操作を確認（標準）"], ["auto_review", "自動レビューに確認を任せる"], ["full_access", "操作の確認を省略"]], "default")}
    <p class="shared-secondary">標準では、追加の許可が必要な操作を担当者に確認します。保存先の中に、仕事に使うフォルダを作成します。</p>
    <details class="shared-options" data-details-key="shared-provider-options:${key}"><summary>追加設定（識別名・子の仕事・資源の分離）</summary>
    ${textField(local, "templateId", "ひな形の識別名（空欄なら自動作成）")}
    ${textField(local, "children", "子の仕事を依頼できる環境の識別名（任意・空白区切り）")}<p class="shared-secondary">他の実行環境に子の仕事を任せる場合だけ、Hub管理者に確認した識別名を指定します。</p>
    ${select(local, "resourceScope", "実行枠のまとめ方（初回の公開時のみ）", [["device", "このPCのmoyAI実行を一つの共有枠にする（標準）"], ["isolated", "ソフト・領域・外部効果が独立していることを確認した"]], "device")}
    <p class="shared-secondary">標準では、このPCのmoyAI実行を一つの共有枠にまとめます。公開済みの端末の共有範囲はここでは変更しません。</p></details>
    ${button("provider-prepare", "保存先フォルダを選んで確認", "", busy || !p?.connected || !local.draft.templateLabel?.trim())}
    ${prepared ? `<div class="shared-provider-review"><h3>公開前の確認</h3><p><strong>${esc(prepared.label)}</strong><br>保存先: ${esc(prepared.base_root)}<br>実行の権限: ${esc(accessLabel(prepared.access_mode))}<br>子の仕事: ${prepared.allowed_child_environments.length ? prepared.allowed_child_environments.map(id => esc(environmentLabel(id))).join("、") : "依頼先を指定しない"}${provider?.mode !== "shared" ? `<br>実行枠: ${p!.provider_scope.kind === "device" ? "このPCの共通枠" : "独立資源を確認した領域"}` : ""}</p>
    ${!matches ? `<p>入力を変更しました。保存先を選び直して、変更後の内容を確認してください。</p>` : ""}
    ${!provider ? `<p>Runnerを起動し、このPCの状態を確認してから公開してください。</p>` : ""}
    ${button("provider-install", "このひな形をHubに公開", "", busy || !p?.connected || !provider || !matches)}<p class="shared-secondary">公開後、Hub管理者が「利用者と仕事の設定」で実行環境を追加します。ここで公開しただけでは、仕事は実行されません。</p></div>` : ""}</div>
    ${provider?.templates.length ? `<h3>公開中のひな形</h3>${provider.templates.map(t => `<article class="shared-environment"><h3>${esc(t.label)}</h3><p>${esc(t.base_root)} · ${esc(accessLabel(t.access_mode))}</p>${button("provider-remove-template", "公開をやめる", t.id, busy)}</article>`).join("")}<p class="shared-secondary">公開をやめても、作成済みのフォルダは残ります。</p>` : ""}
    ${provider ? `<details class="shared-options" data-details-key="shared-provider-operations:${key}"><summary>受付・自動起動・作成済みフォルダの管理</summary>
    ${button("provider-drain", "新規受付を止め、実行中の仕事が終わるのを待つ", "", busy)}
    ${textField(local, "maintenanceUntil", "保守の終了予定（空欄は未定）", "datetime-local")}${button("provider-maintenance", "保守状態にする", "", busy)}
    <p>Windowsログイン時の自動起動: ${provider.autostart ? "有効" : "無効"}</p>${button(provider.autostart ? "provider-no-autostart" : "provider-autostart", provider.autostart ? "自動起動を解除" : "自動起動を有効にする", "", busy)}
    <h3>作成済みの作業フォルダ</h3>${provider.environments.map(env => `<p>${esc(environmentLabel(env.environment_id))} · ${esc(env.directory)} · ${esc(accessLabel(env.access_mode))}</p>`).join("") || "<p>まだ作成されていません。ひな形の公開後、Hub管理者に実行環境の追加を依頼してください。</p>"}
    <h3>登録済みの実行環境へ手動で作成</h3><p class="shared-secondary">通常はHub管理画面から作成を依頼できます。手動で作成する場合だけ入力してください。</p>${select(local, "provisionTemplate", "使うひな形", provider.templates.map(t => [t.id, t.label]))}${textField(local, "provisionEnvironment", "Hubに登録した環境の識別名")}${button("provider-provision", "作業フォルダを作成", "", busy || !local.draft.provisionTemplate || !local.draft.provisionEnvironment)}</details>
    ${provider.unknown_attempts.length ? `<div class="shared-provider-review"><h3>処理の停止を確認してください</h3><p>外部システムへの効果を確認し、関連プロセスを止めてから記録してください。接続切れだけでは停止したと判断できません。</p>${textField(local, "reason", "確認内容と理由")}${select(local, "effects", "外部への効果を現地で確認しました", [["yes", "確認しました"]])}${select(local, "stopped", "関連するプロセスを停止しました", [["yes", "停止しました"]])}${provider.unknown_attempts.map(a => `<p>${esc(a.job_id)} · ${esc(environmentLabel(a.environment_id))} · 試行 ${esc(a.attempt_id)} / ${a.generation}</p>${button("provider-reconcile", "この試行の停止確認を記録", a.attempt_id, busy || local.draft.effects !== "yes" || local.draft.stopped !== "yes" || !local.draft.reason?.trim())}`).join("")}</div>` : ""}` : ""}
    <p class="shared-secondary">このWindows利用者のRunnerを操作します。Hub利用者のログアウトでは、仕事の受付や実行は停止しません。</p>
  </section>`;
}

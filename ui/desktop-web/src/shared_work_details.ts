import { escapeHtml } from "./utils.ts";
import { renderMarkdown } from "./markdown.ts";
import { mcpHistoryRegionHasSelection } from "./mcp_history_dom.ts";
import type { SharedWorkPresentation, WorkAsset } from "./shared_work_state.ts";
const esc = (value: unknown) => escapeHtml(String(value ?? ""));
const button = (action: string, label: string, value = "", disabled = false) => `<button data-action="shared-${action}" data-value="${esc(value)}" ${disabled ? "disabled" : ""}>${esc(label)}</button>`;
const pretty = (value: unknown): string => typeof value === "string" ? value : JSON.stringify(value, null, 2);
const record = (value: unknown): Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : {};
const text = (value: unknown): string => typeof value === "string" ? value : "";
function renderTranscriptItem(item: { position: number; kind: string; payload: unknown }, owner: string): string {
  const payload = record(item.payload);
  let label = ({ user_turn: "依頼", steer_turn: "追加の指示", assistant_message: "応答", tool_call: "操作", tool_output: "操作結果", error: "エラー", file_change: "ファイルの変更", compaction: "会話の要約", user: "依頼", assistant: "応答", tool: "操作結果", event: "実行記録", world_state: "実行環境の確認" } as Record<string, string>)[item.kind] ?? "実行記録";
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
    label = ({ read: "ファイルの読取り", write: "ファイルの保存", apply_patch: "ファイルの編集", shell: "コマンドの実行", shell_start: "継続するコマンドの起動", shell_status: "コマンドの状態確認", shell_stop: "コマンドの停止", shared_publish_artifact: "成果ファイルの共有", shared_delegate: "別の実行先への依頼", inspect_directory: "フォルダーの確認", list: "フォルダーの確認", glob: "ファイルの検索", grep: "内容の検索", current_time: "現在時刻の確認", update_plan: "作業手順の更新" } as Record<string, string>)[text(payload.tool_name)] ?? "操作";
  } else if (item.kind === "tool_output" || item.kind === "tool") {
    if (text(payload.title)) label = `操作結果: ${text(payload.title)}`;
    body = `<pre>${esc(text(payload.output_text) || text(item.payload))}</pre>`;
  } else if (["error", "file_change", "compaction"].includes(item.kind)) {
    body = `<div class="markdown-body">${renderMarkdown(text(payload.message) || text(payload.summary))}</div>`;
  } else {
    return `<article class="shared-job"><details data-details-key="shared-transcript:${esc(owner)}:${item.position}"><summary>${esc(label)} · ${item.position}</summary><pre>${esc(pretty({ kind: item.kind, payload: item.payload }))}</pre></details></article>`;
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
export function renderWorkResult(result: unknown, owner: string, canContinue = false): string {
  if (result === null) return "<p>結果はまだありません。</p>";
  const value = record(result);
  const outcome = record(record(record(value.summary).terminal).outcome);
  const failure = text(value.error) || (outcome.kind === "failed" ? text(outcome.error) : "");
  if (failure) {
    return `<h3>仕事を完了できませんでした</h3><pre>${esc(text(value.message) || failure)}</pre><p>設定を変更できない場合は、実行するPCの担当者にこのエラーを伝えてください。</p>${canContinue ? '<p>原因を解消してから、この会話で続きの依頼を送れます。 <a href="#shared-followup">続きの依頼へ</a></p>' : ""}${text(value.text) ? `<h4>中断までの回答</h4><div class="markdown-body">${renderMarkdown(text(value.text))}</div>` : ""}<details data-details-key="shared-result:${esc(owner)}"><summary>結果の詳細</summary><pre>${esc(pretty(result))}</pre></details>`;
  }
  const answer = text(result) || text(value.text) || text(value.message) || text(value.error);
  return `<h3>結果</h3><div class="markdown-body">${answer ? renderMarkdown(answer) : "<p>回答文はありません。「結果の詳細」を開くと保存された結果を確認できます。</p>"}</div><details data-details-key="shared-result:${esc(owner)}"><summary>結果の詳細</summary><pre>${esc(pretty(result))}</pre></details>`;
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
  return `<section data-shared-region="inputs" class="shared-card"><details data-details-key="hub-inputs"><summary>添付ファイル${p.inputs.length ? `（${p.inputs.length}件）` : ""}</summary><p class="shared-secondary">下の「ファイルを添付」から選べます。1件8 MiB、32件まで。選んだ内容をプロジェクトの参加者と共有します。</p>${p.inputs.map(a => `<p>${esc(a.name)} · ${a.byte_length.toLocaleString()} bytes ${button("remove-input", "添付から外す", a.id, Boolean(local.pending || p.submission_uncertain))}</p>`).join("")}</details></section>`;
}
export function renderWorkInbox(local: SharedWorkPresentation): string {
  const inbox = local.projection?.inbox;
  const items = inbox?.items.map(item => {
    let label = ({ approval: "実行の承認", finished: "仕事の終了", handover: "担当の引継ぎ" } as Record<string, string>)[item.kind] ?? item.kind;
    if (item.kind === "approval") {
      if (item.approval_status === "cancelled") label = "承認依頼の取消済み";
      else if (item.approval_status === "expired") label = "承認期限切れ";
      else if (["decided", "consumed"].includes(item.approval_status ?? "")) {
        label = ({ approve: "許可済み", deny: "拒否済み", stop: "停止を指示済み" } as Record<string, string>)[item.approval_decision ?? ""] ?? "承認の回答済み";
      } else if (item.approval_status === "pending") label = item.can_act ? "あなたの承認待ち" : "担当者の承認待ち";
      else if (item.can_act) label = "あなたの承認待ち";
    }
    return `<article class="shared-job${item.can_act ? " shared-notice-actionable" : ""}"><p><strong>${esc(label)}</strong> · ${esc(new Date(item.created_at_ms).toLocaleString("ja-JP"))}${item.read_at_ms === null ? ' · <span class="shared-secondary">未読のお知らせ</span>' : ""}</p>${button("inbox-open", item.title, item.id, Boolean(local.pending))}</article>`;
  }).join("");
  return `<section data-shared-region="inbox" class="shared-card"><h2>お知らせ ${inbox?.unread_count ? `<span class="shared-secondary" aria-label="未読のお知らせ">（未読 ${inbox.unread_count} 件）</span>` : ""}</h2>${items || "<p>現在のお知らせはありません。</p>"}<div class="shared-actions">${inbox?.next_before ? button("inbox-next", "以前のお知らせ", "", Boolean(local.pending)) : ""}${button("inbox-latest", "最新のお知らせ", "", Boolean(local.pending))}</div></section>`;
}

function workRecordWaitingText(local: SharedWorkPresentation): string {
  const detail = local.projection?.detail;
  if (!detail) return "実行記録はまだありません。";
  if (detail.uncertainty_reason) return "実行状況を確認できません。PCの利用状況を確認してください。";
  if (local.projection?.approval?.status === "pending") return "承認待ちです。承認内容を確認してください。";
  if (detail.state === "running") return "実行中です。応答や操作結果は、Hubに届き次第ここに表示します。";
  if (detail.state === "waiting_child") return "依頼先の仕事の結果を待っています。応答や操作結果は、Hubに届き次第ここに表示します。";
  if (detail.state === "cancelling") return "停止を確認しています。停止が完了するまでお待ちください。";
  if (detail.state === "queued") return detail.wait_reason || "実行の開始を待っています。";
  if (detail.state === "assigned") return "実行するPCで準備しています。応答や操作結果は、Hubに届き次第ここに表示します。";
  return ["succeeded", "failed", "cancelled"].includes(detail.state)
    ? "この仕事の実行記録はありません。結果は会話内で確認できます。" : "実行記録はまだ届いていません。";
}
function renderWorkAsset(a: WorkAsset, owner: string, busy: boolean, latest = false): string {
  return `<article class="shared-job"><h3>${esc(a.name)}${latest ? " · 最新版" : ""}</h3><p>${a.kind === "input" ? "入力" : "成果"} · ${a.byte_length.toLocaleString()} bytes · 版 ${a.version}</p><details data-details-key="shared-asset:${esc(owner)}:${esc(a.id)}"><summary>ファイルの詳細</summary><p class="shared-secondary">SHA-256: ${esc(a.sha256)}</p></details><div class="shared-actions">${a.purged_at_ms ? "<p>保持期限により内容は削除済みです。</p>" : ""}${button("save-asset", "名前を付けて保存", a.id, busy || a.purged_at_ms !== null)}${a.kind !== "input" ? button("import-asset", "元の版と照合して取り込む", a.id, busy || a.purged_at_ms !== null) : ""}</div></article>`;
}
function renderWorkAssets(local: SharedWorkPresentation): string {
  const groups = new Map<string, WorkAsset[]>();
  for (const asset of local.projection!.assets) {
    const key = JSON.stringify([asset.kind, asset.name]);
    const group = groups.get(key) ?? [];
    group.push(asset); groups.set(key, group);
  }
  const owner = workRecordOwner(local), busy = Boolean(local.pending);
  const files = Array.from(groups, ([key, versions]) => {
    versions.sort((a, b) => b.version - a.version || b.created_at_ms - a.created_at_ms);
    const [latest, ...previous] = versions;
    const history = previous.length ? `<details data-details-key="shared-asset-versions:${esc(owner)}:${esc(encodeURIComponent(key))}"><summary>以前の版（${previous.length}件）</summary>${previous.map(asset => renderWorkAsset(asset, owner, busy)).join("")}</details>` : "";
    return renderWorkAsset(latest, owner, busy, latest.kind === "artifact" && previous.length > 0) + history;
  }).join("");
  return `<section data-shared-region="assets" class="shared-card"><h2>入力と成果ファイル</h2>${files || "<p>選択した仕事のファイルはまだありません。</p>"}</section>`;
}
function renderRetainedServices(local: SharedWorkPresentation): string {
  const services = local.projection?.detail?.retained_services ?? [];
  if (!services.length) return '<section data-shared-region="services" hidden></section>';
  return `<section data-shared-region="services" class="shared-card"><h2>この会話で起動中のアプリ</h2>${services.map(service => {
    const environment = local.projection?.status?.environments.find(row => row.id === service.environment_id);
    const state = service.uncertain ? "稼働状態を確認できません" : service.stop_requested ? "停止を確認中" : "起動中";
    const stop = service.can_stop && !service.stop_requested;
    return `<article class="shared-job"><h3>${esc(environment?.device_label ?? environment?.label ?? "実行PC")}</h3><p>${state} · 保持期限 ${esc(new Date(service.expires_at_ms).toLocaleString("ja-JP"))}</p>${stop ? button("stop-service", "このアプリを停止", service.service_id, Boolean(local.pending)) : ""}</article>`;
  }).join("")}</section>`;
}
export function renderWorkDetails(local: SharedWorkPresentation, mode: "conversation" | "support" | "record" | "composer" = "conversation"): string {
  const p = local.projection!, detail = p.detail, busy = Boolean(local.pending);
  const transcriptOwner = encodeURIComponent(JSON.stringify([p.generation, p.principal?.user_id, p.selected_project_id, p.selected_job_id, p.transcript?.items[0]?.position ?? null]));
  const assets = renderWorkAssets(local);
  const emptyRecord = !p.transcript?.items.length;
  const waitingText = workRecordWaitingText(local);
  const transcriptBody = `${p.transcript?.items.map(item => renderTranscriptItem(item, transcriptOwner)).join("") || `<p>${esc(waitingText)}</p>`}${p.transcript?.next_after !== null && p.transcript ? button("transcript-next", "続きの会話を表示", "", busy) : ""}`;
  const transcript = `<section data-shared-region="transcript" data-shared-record-owner="${esc(transcriptOwner)}" class="shared-card"><h2>会話と実行の記録</h2>${transcriptBody}</section>`;
  const composer = `<section data-shared-region="followup" class="shared-card shared-composer-content"><h2>メッセージ</h2>${detail?.can_continue ? `<label>追加の依頼内容<textarea id="shared-followup" data-shared-field="draft:followup" rows="4" ${busy ? "disabled" : ""}>${esc(local.draft.followup)}</textarea></label><details data-details-key="hub-followup-options"><summary>追加設定</summary>${renderStartDeadline(local, "followupStartBefore")}</details>${button("continue", "送信", "", busy || !local.draft.followup?.trim() || p.submission_uncertain)}` : "<p>追加の依頼には、前の仕事の終了、保存された会話、継続する権限が必要です。</p>"}</section>`;
  const handover = `<section data-shared-region="handover" class="shared-card"><h2>担当の引継ぎ</h2>${p.handover?.pending ? `<p>担当変更を受け付けました。実行が安全に区切れるまで待っています。</p>` : ""}${p.handover?.can_handover ? select(local, "assigneeId", "次の担当者", p.handover.candidates.map(u => [u.user_id, u.display_name])) + button("handover", "この利用者へ引き継ぐ", "", busy || !local.draft.assigneeId) : "<p>現在の担当者またはプロジェクト管理者が引き継ぎます。</p>"}</section>`;
  if (mode === "support") return renderRetainedServices(local) + assets + handover;
  if (mode === "record") return `<section data-shared-region="transcript" data-shared-record-owner="${esc(transcriptOwner)}" class="shared-card">${emptyRecord && detail && ["running", "waiting_child", "queued", "assigned", "cancelling"].includes(detail.state) ? `<p class="shared-record-waiting" role="status">${esc(waitingText)}</p>` : ""}<details data-details-key="hub-selected-job-transcript"><summary>選択した仕事の実行記録</summary>${transcriptBody}</details></section>`;
  if (mode === "composer") return composer;
  return transcript + composer;
}

import { icon } from "./icons.ts";
import { renderMarkdown } from "./markdown.ts";
import { escapeHtml } from "./utils.ts";
import { createMcpHistoryUiState, mcpHistoryPresentation, mcpHistoryDate, mcpHistoryStateLabel,
  mcpHistoryStopLabel, selectedMcpHistoryRow, type McpHistoryPresentation, type McpHistoryRow } from "./mcp_history_state.ts";

const disabled = (value: boolean) => value ? "disabled" : "";
function renderRow(row: McpHistoryRow, selected: string | null): string {
  return `<button id="mcp-history-row-${escapeHtml(row.id)}" class="mcp-history-row" data-action="mcp-history-select" data-value="${escapeHtml(row.id)}" data-history-row="${escapeHtml(row.id)}" aria-pressed="${selected === row.id}">
    <strong data-history-cell="title">${escapeHtml(row.title || "無題のタスク")}</strong>
    <span data-history-cell="peer">${row.direction === "instruction" ? "実行先" : "依頼元"}: ${escapeHtml(row.peer_label)}</span>
    <span data-history-cell="target">${escapeHtml(row.target_label)}</span>
    <span class="mcp-history-row-bottom"><time data-history-cell="time">${escapeHtml(mcpHistoryDate(row.created_at_ms))}</time><span class="mcp-history-state" data-history-cell="state" data-state="${escapeHtml(row.state)}">${mcpHistoryStateLabel(row.state)}</span></span>
  </button>`;
}
function renderSummary(row: McpHistoryRow | null): string {
  if (!row) return "";
  const stop = mcpHistoryStopLabel(row.stop_status);
  return `<dl class="mcp-history-facts">
    <div><dt>${row.direction === "instruction" ? "実行先" : "依頼元"}</dt><dd>${escapeHtml(row.peer_label)}</dd></div>
    <div><dt>対象</dt><dd>${escapeHtml(row.target_label)}</dd></div>
    <div><dt>${row.direction === "instruction" ? "指示日時" : "受付日時"}</dt><dd>${escapeHtml(mcpHistoryDate(row.created_at_ms))}</dd></div>
    <div><dt>最終記録日時</dt><dd>${escapeHtml(mcpHistoryDate(row.updated_at_ms))}</dd></div>
    <div><dt>${row.state_source === "last_observed" ? "最終確認状態" : "実行状態"}</dt><dd><span class="mcp-history-state" data-state="${escapeHtml(row.state)}">${mcpHistoryStateLabel(row.state)}</span>${stop ? ` · ${stop}` : ""}</dd></div>
    <div><dt>${row.direction === "instruction" ? "結果の受信" : "指示側の結果受信"}</dt><dd>${row.direction === "execution" ? "この端末では未確認" : row.result_received ? "受信済み" : "未記録"}</dd></div>
  </dl>`;
}
export function renderMcpHistoryOverlay(input?: McpHistoryPresentation, legacyProfiles = false): string {
  const local = input ?? mcpHistoryPresentation(createMcpHistoryUiState());
  const row = selectedMcpHistoryRow(local);
  const detail = local.detail?.row.id === local.selectedId && local.detail.row.direction === local.direction ? local.detail : null;
  const direction = local.direction;
  return `<div class="modal-backdrop">
    <section class="modal settings-modal mcp-history-modal" data-modal="mcp_history" data-surface="mcp_history" data-history-page="${direction}:${local.offset}" data-history-detail-owner="${direction}:${escapeHtml(local.selectedId ?? "")}" role="dialog" aria-modal="true" aria-labelledby="mcp-history-title" tabindex="-1">
      <header class="mcp-history-header"><div><span class="hub-eyebrow">LYNX · MCP HISTORY</span><h2 id="mcp-history-title">MCP履歴</h2><p>他端末への指示と、この端末で受け付けた実行を確認できます。</p></div><button id="mcp-history-close-top" class="icon-button" data-action="close-overlay" aria-label="閉じる" title="閉じる">${icon("x")}</button></header>
      <div class="mcp-history-toolbar"><nav aria-label="履歴の種類">
        <button id="mcp-history-instruction" data-action="mcp-history-direction" data-value="instruction" aria-pressed="${direction === "instruction"}">${icon("send")}MCP指示</button>
        <button id="mcp-history-execution" data-action="mcp-history-direction" data-value="execution" aria-pressed="${direction === "execution"}">${icon("download")}MCP実行</button></nav>
        <button id="mcp-history-refresh" data-action="mcp-history-refresh" title="最新の履歴を取得し、1ページ目に戻る" ${disabled(local.listPending || local.detailPending || local.operation === "stop")}>${icon("refresh")}更新</button></div>
      <p class="mcp-history-scope" data-history-region="scope">${direction === "instruction"
        ? "Hub経由の委任について、この端末が保存した最終確認状態を表示します。実行先へ状態を問い合わせる操作ではありません。新しい履歴は「更新」で取得できます。"
        : "この端末に保存された受付・実行の記録を表示します。指示側の結果受信状況とは異なります。"}</p>
      <div class="mcp-history-feedback" data-history-region="list-error" role="status">${escapeHtml(local.error)}</div>
      <div class="mcp-history-body">
        <aside class="mcp-history-sidebar" aria-label="MCP履歴の一覧"><div class="mcp-history-list" data-history-list tabindex="0" aria-label="履歴一覧">
          ${local.rows.length ? local.rows.map(candidate => renderRow(candidate, local.selectedId)).join("")
            : `<p class="mcp-history-empty">${!local.loaded && local.listPending ? "履歴を読み込んでいます…" : local.error ? "履歴を表示できません。" : direction === "instruction" ? "この端末から指示した履歴はありません。" : "この端末で受け付けた実行履歴はありません。"}</p>`}
        </div><div class="mcp-history-pager"><button id="mcp-history-previous" data-action="mcp-history-previous" ${disabled(local.listPending || !local.previousOffsets.length)} aria-label="前のページ">${icon("chevron-left")}前へ</button><span data-history-region="page-label">${local.previousOffsets.length + 1}ページ</span><button id="mcp-history-next" data-action="mcp-history-next" ${disabled(local.listPending || local.nextOffset === null)} aria-label="次のページ">次へ${icon("chevron-right")}</button></div></aside>
        <section class="mcp-history-detail" aria-label="選択した履歴の詳細">
          <header class="mcp-history-detail-header"><h3 data-history-region="detail-title">${escapeHtml(row?.title || (local.selectedId ? "履歴の詳細" : "履歴を選択してください"))}</h3><div class="mcp-history-detail-actions">
            <button id="mcp-history-export" data-action="mcp-history-export" ${disabled(!row || local.operation !== null)}>${icon("download")}Markdownで保存</button>
            <button id="mcp-history-stop" class="mcp-history-danger" data-action="mcp-history-stop" ${disabled(!row?.can_stop || local.operation !== null)}>停止を要求</button>
          </div></header>
          <div class="mcp-history-detail-scroll" data-history-scroll tabindex="0" aria-label="履歴本文">
            <div data-history-region="detail-summary">${renderSummary(row)}</div>
            <p class="mcp-history-feedback" data-history-region="detail-feedback" role="status">${escapeHtml(local.detailError || local.notice || (local.operation === "export" ? "保存先を選択してください。" : local.operation === "stop" ? "停止を要求しています…" : ""))}</p>
            <p class="mcp-history-truncated" data-history-region="truncated">${detail?.truncated ? "長い履歴は一部を省略しています。省略範囲は本文内にも記載されます。" : ""}</p>
            <article class="mcp-history-document" data-history-region="document">${detail ? renderMarkdown(detail.markdown)
              : `<p class="mcp-history-empty">${local.detailPending ? "詳細を読み込んでいます…" : local.selectedId ? "詳細はまだ取得できていません。" : "左の一覧から指示または実行を選ぶと、記録された指示・処理・結果を確認できます。"}</p>`}</article>
          </div>
        </section>
      </div>
      <p class="mcp-history-feedback" data-history-region="retirement" role="status">${legacyProfiles ? "旧手動配信は廃止されました。保存済みの設定・証明書・履歴は保持され、旧配信は再開しません。今後の受付は「受付・接続設定」で対象と権限を確認して設定してください。" : ""}</p>
      <footer class="mcp-history-footer"><div><button id="mcp-history-settings" data-action="show-hub">受付・接続設定</button></div><button id="mcp-history-close" data-action="close-overlay">閉じる</button></footer>
    </section>
  </div>`;
}

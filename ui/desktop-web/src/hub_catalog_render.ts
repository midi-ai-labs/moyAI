import type { HubCatalogComparison, HubContext, HubModel } from "./hub_state.ts";
import { escapeHtml } from "./utils.ts";

function field(label: string, before: string | null, after: string | null): string {
  return `<tr><th scope="row">${escapeHtml(label)}</th><td>${escapeHtml(before ?? "—")}</td><td>${escapeHtml(after ?? "—")}</td></tr>`;
}
function table(rows: string): string {
  return `<table><thead><tr><th scope="col">項目</th><th scope="col">確認時</th><th scope="col">現在</th></tr></thead><tbody>${rows}</tbody></table>`;
}
function capabilities(model: HubModel | null): string | null {
  return model ? model.capabilities.join(", ") || "機能情報なし" : null;
}

export function renderHubCatalogComparison(comparison: HubCatalogComparison | undefined, context: HubContext): string {
  if (!comparison) return '<p class="hub-help">カタログの比較情報を取得していません。</p>';
  switch (comparison.status) {
    case "first_review": return '<p class="hub-help">初回の確認です。現在のモデル一覧を確認して保存すると、次回から変更点を比較できます。</p>';
    case "baseline_unavailable": return `<p class="hub-help">確認した更新番号 ${escapeHtml(comparison.reviewed_revision ?? "—")} の比較元は保存されていません。現在の内容を確認して保存すると、次回から変更点を比較できます。</p>`;
    case "current_unavailable": return '<p class="hub-help">接続して現在のカタログを取得すると、確認時からの変更点を表示します。</p>';
    case "invalid": return '<p class="hub-feedback" role="alert">比較元と現在のカタログの識別情報・更新番号・内容に不整合があり、差分を表示できません。接続状態と設定を確認してください。</p>';
  }
  const softwareChanged = comparison.software_before !== comparison.software_after;
  const added = comparison.models.filter((row) => !row.before).length;
  const removed = comparison.models.filter((row) => !row.after).length;
  const changed = comparison.models.length - added - removed;
  const heading = `確認時の更新 ${comparison.reviewed_revision} → 現在の更新 ${comparison.current_revision}`;
  if (!comparison.models.length && !softwareChanged) {
    return `<p class="hub-help">${escapeHtml(heading)}<br>モデル・表示名・機能・Hubバージョンの差分はありません。${comparison.reviewed_revision !== comparison.current_revision ? "割当方針などの変更は下の変更履歴も確認してください。" : ""}</p>`;
  }
  return `<p class="hub-help">${escapeHtml(heading)}<br>モデル: 追加 ${added} / 削除 ${removed} / 変更 ${changed}${softwareChanged ? " · Hubバージョン変更あり" : ""}</p>
    <details class="hub-catalog-diff" data-details-key="hub-${context}-catalog-diff" id="hub-${context}-catalog-diff" open>
      <summary>確認時からの変更内容</summary><div class="hub-catalog-diff-list" tabindex="0" role="region" aria-label="${context === "main" ? "Main" : "Side"}のカタログ変更内容">
      ${softwareChanged ? `<article><h4>Hubバージョン</h4>${table(field("バージョン", comparison.software_before, comparison.software_after))}</article>` : ""}
      ${comparison.models.map((row) => `<article><h4>${!row.before ? "追加" : !row.after ? "削除" : "変更"} · <code>${escapeHtml(row.id)}</code></h4>${table(field("表示名", row.before?.label ?? null, row.after?.label ?? null) + field("機能", capabilities(row.before), capabilities(row.after)))}</article>`).join("")}
      </div></details>`;
}

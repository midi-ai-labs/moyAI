import assert from "node:assert/strict";
import test from "node:test";
import { renderHubCatalogComparison } from "../src/hub_catalog_render.ts";
import { renderHubOverlay } from "../src/hub_render.ts";
import { createHubUiState, type HubCatalogComparison, type HubProjection } from "../src/hub_state.ts";

function comparison(overrides: Partial<HubCatalogComparison> = {}): HubCatalogComparison {
  return {
    status: "compared", reviewed_revision: "3", current_revision: "8",
    software_before: "0.1.0", software_after: "0.2.0",
    models: [
      { id: "new", before: null, after: { id: "new", label: "New model", capabilities: ["chat"] } },
      { id: "removed", before: { id: "removed", label: "Removed model", capabilities: ["tools"] }, after: null },
      { id: "renamed", before: { id: "renamed", label: "Former name", capabilities: ["chat"] }, after: { id: "renamed", label: "New <name>", capabilities: ["chat", "vision"] } },
    ],
    ...overrides,
  };
}

test("catalog comparison displays additions, removals and label/capability/software before and after safely", () => {
  const html = renderHubCatalogComparison(comparison(), "main");
  assert.match(html, /確認時の更新 3 → 現在の更新 8/);
  assert.match(html, /追加 1 \/ 削除 1 \/ 変更 1/);
  assert.match(html, /Hubバージョン変更あり/);
  assert.match(html, /<td>0\.1\.0<\/td><td>0\.2\.0<\/td>/);
  assert.match(html, /<td>—<\/td><td>New model<\/td>/);
  assert.match(html, /<td>Removed model<\/td><td>—<\/td>/);
  assert.match(html, /<td>Former name<\/td><td>New &lt;name&gt;<\/td>/);
  assert.match(html, /<td>chat<\/td><td>chat, vision<\/td>/);
  assert.doesNotMatch(html, /<name>/);
  assert.match(html, /data-details-key="hub-main-catalog-diff"/);
  assert.match(html, /tabindex="0" role="region" aria-label="Mainのカタログ変更内容"/);
});

test("missing legacy, disconnected and invalid baselines never claim unchanged", () => {
  for (const status of ["first_review", "baseline_unavailable", "current_unavailable", "invalid"] as const) {
    const html = renderHubCatalogComparison(comparison({ status, models: [] }), "side_chat");
    assert.doesNotMatch(html, /差分はありません/);
    if (status === "baseline_unavailable") assert.match(html, /更新番号 3 の比較元は保存されていません/);
    if (status === "invalid") assert.match(html, /不整合があり、差分を表示できません/);
  }
  const policyOnly = renderHubCatalogComparison(comparison({ models: [], software_after: "0.1.0" }), "main");
  assert.match(policyOnly, /モデル・表示名・機能・Hubバージョンの差分はありません/);
  assert.match(policyOnly, /割当方針などの変更は下の変更履歴も確認/);
});

test("Main and Side show their own reviewed revisions inside independent retained settings regions", () => {
  const state = createHubUiState();
  state.projection = {
    settings_revision: "4", connection_generation: "1", status: "connected", endpoint: "http://127.0.0.1:9470/",
    label: "Desktop", hub_id: "hub-a", catalog: null, main_review: null, side_chat_review: null,
    main_confirmation: "review_required", side_chat_confirmation: "confirmed", error: null,
    main_mode: "direct", side_chat_mode: "direct", active_main: null, active_side_chat: null,
    can_enable_main_hub: false, can_enable_side_chat_hub: false, can_change_main_mode: true, can_change_side_chat_mode: true,
    main_catalog_comparison: comparison(),
    side_chat_catalog_comparison: comparison({ reviewed_revision: "8", models: [], software_before: "0.2.0" }),
  } satisfies HubProjection;
  const html = renderHubOverlay(state);
  const main = html.slice(html.indexOf('aria-labelledby="hub-main-title"'), html.indexOf('aria-labelledby="hub-side_chat-title"'));
  const side = html.slice(html.indexOf('aria-labelledby="hub-side_chat-title"'));
  assert.match(main, /確認時の更新 3 → 現在の更新 8/);
  assert.match(main, /data-settings-passive="hub-main-catalog-comparison"/);
  assert.match(side, /確認時の更新 8 → 現在の更新 8/);
  assert.match(side, /data-settings-passive="hub-side_chat-catalog-comparison"/);
  assert.doesNotMatch(side, /Former name|削除 1/);
});

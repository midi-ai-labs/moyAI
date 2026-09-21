import { icon } from "./icons.ts";
import { escapeHtml } from "./utils.ts";
import { renderDeviceNetwork } from "./device_network_render.ts";
import { renderHubCatalogComparison } from "./hub_catalog_render.ts";
import type { DeviceNetworkPresentation } from "./device_network_state.ts";
import {
  createHubUiState, hubActiveRoute, hubCanSave, hubErrorText, hubModelChoice,
  hubSaveFeedback, type HubContext, type HubPresentation,
} from "./hub_state.ts";

export function aiConnectionManaged(hub?: HubPresentation, network?: DeviceNetworkPresentation): boolean {
  return Boolean(network?.projection?.hub_url || (hub?.projection?.endpoint
    && (hub.projection.main_mode === "hub" || hub.projection.side_chat_mode === "hub")));
}

/** One AI connection form: only the public Hub endpoint and catalog reach this surface. */
export function renderManagedAiConnection(local: HubPresentation | undefined, context: HubContext, network?: DeviceNetworkPresentation): string {
  const state = local ?? createHubUiState();
  const projection = state.projection;
  const endpoint = network?.projection?.hub_url || projection?.endpoint || "";
  const catalog = projection?.catalog;
  const draft = state.drafts[context];
  const choice = hubModelChoice(state, context);
  const route = hubActiveRoute(state, context);
  const enabled = projection?.status === "connected" && !state.pending && !route;
  const enrollment = network?.projection?.enrollment;
  const status = enrollment === "pending" ? "PCの参加承認を待っています。"
    : projection?.status !== "connected" ? "Hubへの再接続を待っています。保存済みのモデル選択は保持しています。"
      : !catalog?.models.length ? "Hubにモデルが登録されていません。管理画面でモデルを追加してください。"
        : route ? "実行中の仕事は現在のモデルで続けます。終了後に選択を変更できます。"
          : "Hubが提供するモデルを選びます。接続先と接続方式はHubで管理します。";
  const feedback = state.error && (!state.errorContext || state.errorContext === context || state.errorContext === "connection")
    ? state.error : hubErrorText(projection?.error) || hubSaveFeedback(state, context);
  const savedLabel = draft.selection.allowed_model_ids.map((id) => catalog?.models.find((model) => model.id === id)?.label ?? id).join("、");
  return `<div class="ai-connection" data-ai-connection="${context}">
    <div class="settings-grid-two">
      <label class="hub-field" for="ai-${context}-endpoint">接続先URL<input id="ai-${context}-endpoint" class="settings-control" value="${escapeHtml(endpoint)}" readonly aria-readonly="true" aria-describedby="ai-${context}-status" /></label>
      <label class="hub-field" for="ai-${context}-mode">接続方式<input id="ai-${context}-mode" class="settings-control" value="Hubから取得" readonly aria-readonly="true" aria-describedby="ai-${context}-status" /></label>
      <label class="hub-field" for="ai-${context}-model">モデル<div data-settings-passive="ai-${context}-models" data-settings-preserve-focused-region>
        <select id="ai-${context}-model" class="settings-control" data-hub-field="${context}:choice" aria-describedby="ai-${context}-status ai-${context}-feedback" ${enabled ? "" : "disabled"}>
          <option value="" ${choice === "" ? "selected" : ""} disabled>モデルを選択してください</option>
          ${projection?.recommended_main_selection || choice === ":hub-default" ? `<option value=":hub-default" ${choice === ":hub-default" ? "selected" : ""}>Hubの標準モデル</option>` : ""}
          ${choice === ":saved" ? `<option value=":saved" selected>保存済みの選択: ${escapeHtml(savedLabel)}</option>` : ""}
          ${(catalog?.models ?? []).map((model) => `<option value="${escapeHtml(model.id)}" ${choice === model.id ? "selected" : ""}>${escapeHtml(model.label)}</option>`).join("")}
        </select></div></label>
    </div>
    <p id="ai-${context}-status" class="hub-help" data-settings-passive="ai-${context}-status" role="status">${escapeHtml(status)}</p>
    <div data-settings-passive="ai-${context}-comparison">${renderHubCatalogComparison(projection?.[`${context}_catalog_comparison`], context)}</div>
    <p id="ai-${context}-feedback" class="hub-help" data-settings-passive="ai-${context}-feedback" role="status">${escapeHtml(feedback || hubErrorText(projection?.error))}</p>
    <div class="hub-connection-actions"><button data-action="hub-save-${context === "main" ? "main" : "side"}" ${hubCanSave(state, context) ? "" : "disabled"}>${state.pending === context ? "保存しています…" : "モデル選択を保存"}</button><button data-action="hub-refresh" ${state.pending ? "disabled" : ""}>最新情報を取得</button></div>
    <p class="hub-help">メインとサイドは別々に保存します。共有仕事には、実行するPCのメインのモデル選択を使います。</p>
  </div>`;
}

export function renderHubOverlay(input?: HubPresentation, network?: DeviceNetworkPresentation): string {
  const local = input ?? createHubUiState();
  return `<div class="modal-backdrop"><section class="modal settings-modal hub-modal" data-modal="hub" data-surface="hub" role="dialog" aria-modal="true" aria-labelledby="hub-dialog-title" tabindex="-1">
    <header class="hub-modal-header"><div><h2 id="hub-dialog-title">moyAI Hub</h2><p>このPCをHubにつなぎ、チームの仕事に参加します。</p></div><button class="icon-button" data-action="close-overlay" aria-label="閉じる">${icon("x")}</button></header>
    <nav class="hub-tabs" aria-label="Hub設定の分類"><button id="hub-tab-devices" data-action="hub-tab-devices" aria-controls="hub-panel-devices" aria-pressed="${local.tab === "devices"}">PCの接続</button><button data-action="show-config">AIの接続</button></nav>
    <div class="hub-modal-body settings-content"><div id="hub-panel-devices" data-hub-panel="devices">${renderDeviceNetwork(network)}</div></div>
    <footer class="hub-modal-footer"><span>AIの接続先とモデルは「設定」の同じ欄で確認できます。</span><button data-action="close-overlay">閉じる</button></footer>
  </section></div>`;
}

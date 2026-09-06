import { icon } from "./icons.ts";
import { escapeHtml } from "./utils.ts";
import { renderDeviceNetwork } from "./device_network_render.ts";
import type { DeviceNetworkPresentation } from "./device_network_state.ts";
import {
  createHubUiState, hubActiveRoute, hubCanSave, hubCanSetRouteMode, hubDraftHasChanges,
  hubErrorText, hubRouteMode, hubRouteModeBlocker, hubSaveFeedback,
  type HubContext, type HubPresentation,
} from "./hub_state.ts";

const statusText = {
  disconnected: "未接続", connecting: "接続しています…", connected: "接続中", stale: "接続を確認してください", error: "接続できません",
};
function channelMarkup(local: HubPresentation, context: HubContext): string {
  const projection = local.projection;
  const catalog = projection?.catalog ?? null;
  const draft = local.drafts[context];
  const review = context === "main" ? projection?.main_review : projection?.side_chat_review;
  const confirmation = context === "main" ? projection?.main_confirmation : projection?.side_chat_confirmation;
  const models = catalog?.models ?? [];
  const unresolved = draft.selection.allowed_model_ids.filter((id) => !models.some((model) => model.id === id));
  const missing = catalog ? unresolved : [];
  const candidates = [...models, ...unresolved.map((id) => ({ id, label: catalog ? "削除されたモデル" : "保存済みのモデル", capabilities: [] }))];
  const enabled = projection?.status === "connected" && !local.pending;
  const changed = hubDraftHasChanges(local, context);
  const saved = !changed && confirmation === "confirmed";
  const feedback = local.error && local.errorContext === context ? local.error : hubSaveFeedback(local, context);
  const confirmationText = changed ? "未保存の変更" : confirmation === "confirmed" ? "保存済み・Hubで確認済み"
    : confirmation === "review_required" ? "再確認が必要" : review ? "保存済み・Hubで未確認" : "未設定";
  const allowed = candidates.filter((model) => draft.selection.allowed_model_ids.includes(model.id));
  const mode = hubRouteMode(local, context);
  const route = hubActiveRoute(local, context);
  const routeBlocker = hubRouteModeBlocker(local, context, "hub");
  const routeContext = context === "main" ? "main" : "side";
  const routeStatus = route
    ? `${route.phase === "waiting" ? "Hubの実行枠を待っています" : "Hubのモデルで実行中"}${route.logical_model_id ? ` · ${route.logical_model_id}` : ""}`
    : mode === "direct" ? "現在は直接接続を使用します。"
      : mode === "hub" ? "現在はHubの割当を使用します。" : "送信先を読み込んでいます。";
  const capabilityText = (values: string[]) => values.map((value) => value === "tools" ? "ツール" : value === "vision" ? "画像" : value).join(" · ");
  return `<section class="hub-channel" aria-labelledby="hub-${context}-title">
    <header><div><span class="hub-eyebrow">${context === "main" ? "MAIN CHAT" : "SIDE CHAT"}</span><h3 id="hub-${context}-title">${context === "main" ? "メインチャット" : "サイドチャット"}</h3></div>
      <span class="hub-review-status" data-settings-passive="hub-${context}-confirmation">${confirmationText}${review ? `<small>確認した更新番号 ${escapeHtml(review.reviewed_revision)}</small>` : ""}</span></header>
    <div class="hub-route-selector" role="group" aria-labelledby="hub-${context}-route-label" aria-describedby="hub-${context}-route-status">
      <span id="hub-${context}-route-label">送信先</span>
      <button data-action="hub-${routeContext}-direct" aria-pressed="${mode === "direct"}" ${hubCanSetRouteMode(local, context, "direct") ? "" : "disabled"}>直接接続 <small>Direct</small></button>
      <button data-action="hub-${routeContext}-hub" aria-pressed="${mode === "hub"}" ${hubCanSetRouteMode(local, context, "hub") ? "" : "disabled"}>Hubを利用</button>
    </div>
    <div id="hub-${context}-route-status" class="hub-route-status" data-settings-passive="hub-${context}-route-status" role="status">
      <strong>${escapeHtml(routeStatus)}</strong><span>${escapeHtml(routeBlocker ?? "切り替えは次の依頼から適用します。Hub利用時に直接接続へ自動では切り替えません。")}</span>
    </div>
    <p class="hub-help">利用候補に含めるモデルを選びます。${context === "main" ? "Main" : "Side"}の選択は独立して保存されます。</p>
    <div class="hub-model-list" data-settings-passive="hub-${context}-models" data-settings-preserve-focused-region>
      ${candidates.length ? candidates.map((model) => `<label class="hub-model-row ${missing.includes(model.id) ? "is-missing" : ""}">
        <input type="checkbox" class="settings-control" data-hub-field="${context}:model:${escapeHtml(model.id)}" id="hub-${context}-model-${escapeHtml(model.id)}" ${draft.selection.allowed_model_ids.includes(model.id) ? "checked" : ""} ${enabled ? "" : "disabled"} />
        <span><strong>${escapeHtml(model.label)}</strong><small>${escapeHtml(model.id)}</small></span><span class="hub-capability">${escapeHtml(capabilityText(model.capabilities) || "機能情報なし")}</span>
      </label>`).join("") : `<p class="hub-empty">${catalog ? "登録モデルがありません。Hubの管理画面でモデルを登録してください。" : "接続すると、Hubの登録モデルを選択できます。"}</p>`}
    </div>
    <label class="hub-field">優先モデル<div data-settings-passive="hub-${context}-preferred" data-settings-preserve-focused-region>
      <select id="hub-${context}-preferred" class="settings-control" data-hub-field="${context}:preferred" ${enabled && allowed.length ? "" : "disabled"}>
        ${allowed.length ? allowed.map((model) => `<option value="${escapeHtml(model.id)}" ${model.id === draft.selection.preferred_model_id ? "selected" : ""}>${escapeHtml(model.label)}</option>`).join("") : '<option value="">利用候補から選択してください</option>'}
      </select></div></label>
    <label class="hub-field">優先モデルが空いていない場合<select id="hub-${context}-wait" class="settings-control" data-hub-field="${context}:wait" ${enabled ? "" : "disabled"}>
      <option value="wait_for_preferred" ${draft.selection.wait_policy === "wait_for_preferred" ? "selected" : ""}>優先モデルが空くまで待つ</option>
      <option value="allow_selected_fallback" ${draft.selection.wait_policy === "allow_selected_fallback" ? "selected" : ""}>選択した別のモデルを許可する</option></select></label>
    <details class="hub-advanced" id="hub-${context}-advanced" data-details-key="hub-${context}-advanced"><summary>機能条件と継続ターン数</summary>
      <label class="hub-field">必要な機能<input id="hub-${context}-capabilities" class="settings-control" data-hub-field="${context}:capabilities" value="${escapeHtml(draft.capabilitiesText)}" placeholder="例: tools, vision" ${enabled ? "" : "disabled"} /></label>
      <label class="hub-field">同じモデルを継続するターン数<input id="hub-${context}-affinity" class="settings-control" data-hub-field="${context}:affinity" inputmode="numeric" value="${escapeHtml(draft.affinityText)}" ${enabled ? "" : "disabled"} /></label>
      <p class="hub-help">1〜100。ユーザーが送信した依頼を1ターンとして数えます。</p></details>
    <div id="hub-${context}-feedback" class="hub-channel-feedback" data-settings-passive="hub-${context}-feedback" role="status">${escapeHtml(feedback)}</div>
    <button class="hub-primary" data-action="hub-save-${context === "main" ? "main" : "side"}" aria-describedby="hub-${context}-feedback" ${hubCanSave(local, context) ? "" : "disabled"}>${local.pending === context ? "確認・保存しています…" : saved ? "保存済み" : "この選択を確認して保存"}</button>
  </section>`;
}
export function renderHubOverlay(input?: HubPresentation, network?: DeviceNetworkPresentation): string {
  const local = input ?? createHubUiState();
  const projection = local.projection;
  const status = projection?.status ?? "disconnected";
  const connected = status === "connected" || status === "stale";
  const error = local.error && local.errorContext && local.errorContext !== "connection" ? "" : local.error || hubErrorText(projection?.error);
  const locked = Boolean(local.pending) || connected || status === "connecting";
  const managed = network?.projection?.enrollment === "active";
  return `<div class="modal-backdrop"><section class="modal settings-modal hub-modal" data-modal="hub" data-surface="hub" role="dialog" aria-modal="true" aria-labelledby="hub-dialog-title" aria-describedby="hub-dialog-help" tabindex="-1">
    <header class="hub-modal-header"><div><span class="hub-eyebrow">LYNX · CONNECTIONS</span><h2 id="hub-dialog-title">moyAI Hub</h2><p id="hub-dialog-help">端末の参加とタスク委任、Main・Sideのモデル割当を管理します。</p></div><button class="icon-button" data-action="close-overlay" aria-label="閉じる" title="閉じる">${icon("x")}</button></header>
    <nav class="hub-tabs" aria-label="Hub設定の分類"><button id="hub-tab-devices" data-action="hub-tab-devices" aria-controls="hub-panel-devices" aria-pressed="${local.tab === "devices"}">端末連携</button><button id="hub-tab-models" data-action="hub-tab-models" aria-controls="hub-panel-models" aria-pressed="${local.tab === "models"}">モデル割当</button></nav>
    <div class="hub-modal-body settings-content">
      <div id="hub-panel-devices" data-hub-panel="devices" ${local.tab === "devices" ? "" : "hidden"}>${renderDeviceNetwork(network)}</div>
      <div id="hub-panel-models" data-hub-panel="models" ${local.tab === "models" ? "" : "hidden"}>
      <p id="hub-scope-help" class="hub-scope-note">${managed ? "端末連携で参加したHubからモデルを取得します。" : "Hubを使う場合は、接続してモデルを取得します。手動接続は同じPCのHub向けです。"} 利用モデルを確認・保存し、Main・Sideそれぞれの送信先を切り替えます。Chat Completions対応モデルを利用します。Hub利用中の依頼の整形・Mainの並列サブエージェント実行は未対応です。</p>
      <div class="hub-identity" data-settings-passive="hub-model-connection" role="status">モデル接続: ${statusText[status]}${projection?.catalog ? ` · 登録 ${projection.catalog.models.length} モデル · 更新 ${escapeHtml(projection.catalog.revision)}` : ""}${error ? `<p>${escapeHtml(error)}</p>` : ""}</div>
      <details id="hub-manual-connection" data-details-key="hub-manual-connection"><summary>手動のHub接続（既存構成との互換用）</summary>
      <section class="hub-connection" aria-labelledby="hub-connection-title"><div class="hub-section-heading"><h3 id="hub-connection-title">Hubに接続</h3><span class="hub-connection-status" data-settings-passive="hub-connection-status" data-status="${status}">${statusText[status]}</span></div>
        <div class="hub-connection-fields"><label class="hub-field">接続先<input id="hub-endpoint" class="settings-control" data-hub-field="endpoint" value="${escapeHtml(local.endpoint)}" placeholder="127.0.0.1:9470" autocomplete="off" spellcheck="false" ${locked ? "disabled" : ""} /></label>
        <label class="hub-field">端末の表示名<input id="hub-label" class="settings-control" data-hub-field="label" value="${escapeHtml(local.label)}" autocomplete="off" ${locked ? "disabled" : ""} /></label>
        <label class="hub-field">接続用トークン<input id="hub-token" class="settings-control" data-hub-field="token" type="password" autocomplete="off" aria-describedby="hub-connection-feedback" placeholder="Hubの管理画面で設定したトークン" ${locked ? "disabled" : ""} /></label></div>
        <div class="hub-connection-actions"><button class="hub-primary" data-action="hub-connect" aria-describedby="hub-connection-feedback" ${projection && !locked ? "" : "disabled"}>${local.pending === "connect" ? "接続しています…" : "Hubに接続"}</button>
        <button data-action="hub-refresh" ${local.pending ? "disabled" : ""}>${local.pending === "refresh" ? "取得しています…" : "最新情報を取得"}</button><button data-action="hub-disconnect" ${projection && status !== "disconnected" && !local.pending ? "" : "disabled"}>接続を解除</button></div>
        <div id="hub-connection-feedback" class="hub-feedback" data-settings-passive="hub-error" role="status" aria-live="polite" ${error ? "" : "hidden"}>${escapeHtml(error)}</div>
        <p class="hub-help">この開発版は同じPCのHubに接続します。トークンは保存しません。Desktopのウィンドウを閉じると接続を解除し、次回はトークンを入力して再接続します。</p>
        <div class="hub-identity" data-settings-passive="hub-identity">${projection?.hub_id ? `<span>Hub <code>${escapeHtml(projection.hub_id)}</code></span><span>バージョン ${escapeHtml(projection.catalog?.software_version ?? "—")} · 更新番号 ${escapeHtml(projection.catalog?.revision ?? "—")}</span>` : "接続するとHubの識別情報を表示します。"}</div>
      </section>
      </details>
      <div class="hub-channels">${channelMarkup(local, "main")}${channelMarkup(local, "side_chat")}</div>
      <details class="hub-history" id="hub-change-history" data-details-key="hub-change-history"><summary>カタログの変更履歴</summary><div data-settings-passive="hub-history">${projection?.catalog?.changes.length ? [...projection.catalog.changes].reverse().map((change) => `<p><strong>更新 ${escapeHtml(change.revision)}</strong><span>${escapeHtml(change.summary)}</span></p>`).join("") : "変更履歴はありません。"}</div></details>
      </div>
    </div>
    <footer class="hub-modal-footer"><span>操作は各カードで確定します。この設定画面を閉じても接続は続きます。</span><button data-action="close-overlay">閉じる</button></footer>
  </section></div>`;
}

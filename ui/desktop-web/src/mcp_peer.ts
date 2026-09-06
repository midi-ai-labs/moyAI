import { command } from "./api.ts";
import type { ActionContext } from "./actions.ts";
import { beginConfigMutation, configMutationPending, finishConfigMutation, sameConfigMutationTarget } from "./config_mutation.ts";
import type { DesktopWebState } from "./types.ts";
import { escapeHtml } from "./utils.ts";

export interface McpPeerRow {
  id: string; base_url: string; enabled: boolean; credential_configured: boolean; certificate_sha256: string | null;
}
export interface McpPeerState {
  rows: McpPeerRow[]; id: string; baseUrl: string; certificate: string;
  pending: "load" | "add" | "remove" | "check" | null;
  serial: number; error: string; notice: string; checks: Record<string, string>;
}
export type McpPeerPresentation = Omit<McpPeerState, "serial">;
export function createMcpPeerState(): McpPeerState {
  return { rows: [], id: "", baseUrl: "", certificate: "", pending: null, serial: 0, error: "", notice: "", checks: {} };
}
export function mcpPeerPresentation(state: McpPeerState): McpPeerPresentation {
  const { serial: _serial, ...presentation } = state;
  return presentation;
}
export function clearMcpPeerToken(): void {
  const token = document.querySelector<HTMLInputElement>("#mcp-peer-token");
  if (token) token.value = "";
}
export function editMcpPeerField(state: McpPeerState, field: string, value: string): void {
  if (state.pending) return;
  if (field === "id") state.id = value;
  else if (field === "base_url") state.baseUrl = value;
  else if (field === "certificate") state.certificate = value;
  // Bearer tokens belong only to the connected password input.
  state.error = "";
  state.notice = "";
}
export function mcpPeerDraftValid(state: McpPeerPresentation): boolean {
  if (!state.id.trim() || state.rows.some((row) => row.id === state.id.trim())) return false;
  try {
    const url = new URL(state.baseUrl.trim());
    return ["http:", "https:"].includes(url.protocol) && !url.username && !url.password && !url.hash && !url.search;
  } catch { return false; }
}
export function renderMcpPeers(local: McpPeerPresentation, configBusy: boolean, configDirty: boolean): string {
  const busy = Boolean(local.pending || configBusy);
  return `<div class="mcp-peer-panel"><h4>moyAI端末へタスクを委任</h4>
    <p id="mcp-peer-help" class="mcp-publish-help">接続先のDesktopでエージェント受付を開始し、URL・トークン・公開証明書を受け取ってください。接続先のプロジェクトや実行権限は、接続先が決めます。</p>
    <div class="mcp-publish-fields">
      <label for="mcp-peer-id" class="mcp-publish-field">端末名<input id="mcp-peer-id" class="settings-control" data-mcp-peer-field="id" value="${escapeHtml(local.id)}" aria-describedby="mcp-peer-help mcp-peer-feedback" placeholder="例: WinB" autocomplete="off" ${busy ? "disabled" : ""} /></label>
      <label for="mcp-peer-url" class="mcp-publish-field">接続先URL<input id="mcp-peer-url" class="settings-control" data-mcp-peer-field="base_url" value="${escapeHtml(local.baseUrl)}" aria-describedby="mcp-peer-help mcp-peer-feedback" placeholder="https://192.168.10.22:7332/mcp" spellcheck="false" autocomplete="off" ${busy ? "disabled" : ""} /></label>
      <label for="mcp-peer-token" class="mcp-publish-field wide">接続用トークン<input id="mcp-peer-token" class="settings-control" data-mcp-peer-field="token" data-settings-dom-value type="password" autocomplete="off" value="" aria-describedby="mcp-peer-credential-help mcp-peer-feedback" ${busy ? "disabled" : ""} /></label>
      <label for="mcp-peer-certificate" class="mcp-publish-field wide">接続先の公開証明書（PEM）<textarea id="mcp-peer-certificate" class="settings-control" data-mcp-peer-field="certificate" rows="3" spellcheck="false" aria-describedby="mcp-peer-credential-help mcp-peer-feedback" placeholder="接続先の「公開証明書をコピー」から貼り付け" ${busy ? "disabled" : ""}>${escapeHtml(local.certificate)}</textarea></label>
    </div>
    <p id="mcp-peer-credential-help" class="mcp-publish-help">秘密鍵は入力しません。同じPCのHTTP接続では公開証明書は不要です。接続用トークンは保存後に再表示しません。</p>
    <div class="mcp-publish-actions"><button data-action="mcp-peer-add" ${busy || configDirty || !mcpPeerDraftValid(local) ? "disabled" : ""}>端末の接続を保存</button><button data-action="mcp-peer-refresh" ${busy ? "disabled" : ""}>登録済み端末を更新</button></div>
    <p id="mcp-peer-feedback" class="mcp-publish-help" role="status" data-settings-passive="mcp-peer-feedback">${escapeHtml(local.error || local.notice || (configDirty ? "設定に未保存の変更があります。先に設定を保存するか、変更を破棄してください。" : "追加した接続は次のタスクから使用できます。"))}</p>
    <div data-settings-passive="mcp-peer-rows" data-settings-preserve-focused-region>${local.rows.length ? local.rows.map((row) => `<article class="mcp-publish-job">
      <strong>${escapeHtml(row.id)}</strong><p class="mcp-publish-help">${escapeHtml(row.base_url)} · ${row.enabled ? "有効" : "無効"} · ${row.credential_configured ? "トークン保存済み" : "トークンなし"}</p>
      ${row.certificate_sha256 ? `<p class="mcp-publish-help">証明書 SHA-256: ${escapeHtml(row.certificate_sha256)}</p>` : ""}
      <p class="mcp-publish-help">${escapeHtml(local.checks[row.id] ?? "接続は未確認です。")}</p>
      <div class="mcp-publish-actions"><button data-action="mcp-peer-check" data-value="${escapeHtml(row.id)}" ${busy ? "disabled" : ""}>接続を確認</button>
      <button data-action="mcp-peer-remove" data-value="${escapeHtml(row.id)}" ${busy || configDirty ? "disabled" : ""}>接続を削除</button></div></article>`).join("") : '<p class="mcp-publish-help">登録したmoyAI端末はありません。</p>'}</div></div>`;
}
export async function refreshMcpPeers(context: ActionContext): Promise<void> {
  const local = context.uiState.mcpPeers;
  if (local.pending || context.getViewState()?.overlay !== "config") return;
  const serial = ++local.serial;
  local.pending = "load";
  context.rerender();
  try {
    const result = await command<{ rows: McpPeerRow[] }>("mcp_peer_projection");
    if (serial !== local.serial || context.getViewState()?.overlay !== "config") return;
    local.rows = result.rows;
    local.error = "";
  } catch { if (serial === local.serial) local.error = "登録済み端末を取得できませんでした。"; }
  finally { if (serial === local.serial) { local.pending = null; context.rerender(); } }
}
export async function mutateMcpPeer(context: ActionContext, removeId?: string): Promise<void> {
  const local = context.uiState.mcpPeers;
  const view = context.getViewState();
  if (!view || view.overlay !== "config" || local.pending || configMutationPending(context.uiState) || context.uiState.configDirty) return;
  if (removeId ? !local.rows.some((row) => row.id === removeId) : !mcpPeerDraftValid(local)) return;
  const token = document.querySelector<HTMLInputElement>("#mcp-peer-token")?.value ?? "";
  if (!removeId && !token.trim()) { local.error = "接続用トークンを入力してください。"; context.rerender(); return; }
  const request = beginConfigMutation(context.uiState, view.config_target);
  const content = document.querySelector<HTMLElement>(".settings-modal .settings-content");
  const viewport = content ? { sourceTarget: { ...request.target }, scrollLeft: content.scrollLeft, scrollTop: content.scrollTop } : undefined;
  const serial = ++local.serial;
  local.pending = removeId ? "remove" : "add";
  local.error = "";
  context.rerender();
  try {
    const [state, succeeded] = await command<[DesktopWebState, boolean]>(removeId ? "mcp_peer_remove" : "mcp_peer_add", removeId
      ? { id: removeId, expectedTarget: request.target }
      : { peer: { id: local.id.trim(), base_url: local.baseUrl.trim(), token, trusted_certificate_pem: local.certificate.trim() || null, remote_agent: true }, expectedTarget: request.target });
    if (!finishConfigMutation(context.uiState, request, succeeded, state.config_target, context.getViewState()?.config_target ?? null)) return;
    if (serial !== local.serial || context.getViewState()?.overlay !== "config") return;
    if (succeeded) {
      clearMcpPeerToken();
      if (!removeId) { local.id = ""; local.baseUrl = ""; local.certificate = ""; }
      local.notice = removeId ? "接続を削除しました。" : "端末の接続を保存しました。接続を確認してください。";
    } else local.error = "接続設定を保存できませんでした。設定の案内を確認してください。";
    context.acceptProjection(state, true, succeeded ? {
      target: { ...state.config_target }, primaryAction: "mcp-peer-refresh", fallbackAction: "close-overlay", viewport,
    } : undefined);
  } catch (error) {
    finishConfigMutation(context.uiState, request, false, request.target, context.getViewState()?.config_target ?? null);
    if (serial === local.serial && !context.recoverCommandConflict(error)) local.error = "端末の接続設定を更新できませんでした。名前の重複、URLと証明書を確認してください。";
  } finally {
    if (serial === local.serial) { local.pending = null; context.rerender(); }
  }
  await refreshMcpPeers(context);
}
export async function checkMcpPeer(context: ActionContext, id: string): Promise<void> {
  const local = context.uiState.mcpPeers;
  const view = context.getViewState();
  if (!view || view.overlay !== "config" || local.pending || !local.rows.some((row) => row.id === id)) return;
  const target = { ...view.config_target };
  const serial = ++local.serial;
  local.pending = "check";
  context.rerender();
  try {
    const result = await command<{ id: string; tools: { name: string; description?: string }[] }>("mcp_peer_check", { id });
    if (serial !== local.serial || context.getViewState()?.overlay !== "config" || !sameConfigMutationTarget(target, context.getViewState()!.config_target)) return;
    local.checks[id] = result.id === id && result.tools.some((tool) => tool.name === "delegate_task")
      ? "接続できました。エージェント受付を確認しました。" : "接続できました。エージェント受付ツールは確認できません。";
  } catch {
    if (serial === local.serial) local.checks[id] = "接続できません。配信状態・URL・トークン・証明書を確認してください。";
  } finally { if (serial === local.serial) { local.pending = null; context.rerender(); } }
}

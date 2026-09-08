import { command } from "./api.ts";
import type { ActionContext } from "./actions.ts";
import { asyncTransactionIsCurrent, beginAsyncTransaction, clearAsyncTransaction } from "./async_transaction.ts";
import { invalidateMcpHistory, selectedMcpHistoryRow, type McpHistoryDetail, type McpHistoryDirection,
  type McpHistoryPage, type McpHistoryRequest, type McpHistoryUiState } from "./mcp_history_state.ts";

function target(local: McpHistoryUiState, id: string | null = null) {
  return { direction: local.direction, offset: local.offset, anchor: local.anchor, id };
}
function stillVisible(context: ActionContext, request: McpHistoryRequest): boolean {
  const local = context.uiState.mcpHistory;
  return context.getViewState()?.overlay === "mcp_history" && local.direction === request.direction
    && local.offset === request.offset && (request.id === null || local.selectedId === request.id);
}
export async function openMcpHistory(context: ActionContext): Promise<void> {
  const local = context.uiState.mcpHistory;
  resetHistoryPage(local, local.direction, 0, [], null);
  await context.mutate("show_mcp_history");
  await refreshMcpHistory(context, true);
}

async function loadList(context: ActionContext): Promise<void> {
  const local = context.uiState.mcpHistory;
  const request = beginAsyncTransaction(local.listRequest, target(local), "single-flight", (token, value) => ({ token, ...value }));
  if (!request) return;
  if (!local.loaded) context.rerender();
  try {
    const page = await command<McpHistoryPage>("mcp_history_list", { direction: request.direction, offset: request.offset, anchor: request.anchor });
    if (!asyncTransactionIsCurrent(local.listRequest, request) || !stillVisible(context, request)) return;
    local.rows = page.rows;
    local.nextOffset = page.next_offset;
    local.anchor = page.anchor;
    local.loaded = true;
    local.error = "";
  } catch {
    if (asyncTransactionIsCurrent(local.listRequest, request) && stillVisible(context, request)) {
      local.error = "履歴を取得できませんでした。表示中の内容は前回取得した記録です。「更新」で再試行できます。";
    }
  } finally {
    if (clearAsyncTransaction(local.listRequest, request) && stillVisible(context, request)) context.rerender();
  }
}
async function loadDetail(context: ActionContext): Promise<void> {
  const local = context.uiState.mcpHistory;
  if (!local.selectedId) return;
  const request = beginAsyncTransaction(local.detailRequest, target(local, local.selectedId), "single-flight", (token, value) => ({ token, ...value }));
  if (!request) return;
  if (!local.detail) context.rerender();
  try {
    const detail = await command<McpHistoryDetail>("mcp_history_detail", { direction: request.direction, id: request.id });
    if (!asyncTransactionIsCurrent(local.detailRequest, request) || !stillVisible(context, request)) return;
    if (detail.row.id !== request.id || detail.row.direction !== request.direction) throw new Error("history target changed");
    local.detail = detail;
    local.detailError = "";
  } catch {
    if (asyncTransactionIsCurrent(local.detailRequest, request) && stillVisible(context, request)) {
      local.detailError = "詳細を取得できませんでした。対象の記録を確認して「更新」で再試行してください。";
    }
  } finally {
    if (clearAsyncTransaction(local.detailRequest, request) && stillVisible(context, request)) context.rerender();
  }
}

/** Reads only this machine's durable records; it never queries a remote worker implicitly. */
export async function refreshMcpHistory(context: ActionContext, force = false): Promise<void> {
  const local = context.uiState.mcpHistory;
  if (context.getViewState()?.overlay !== "mcp_history" || local.operationRequest.active?.kind === "stop") return;
  const now = Date.now();
  if (!force && now - local.lastPollAt < 3000) return;
  local.lastPollAt = now;
  await Promise.all([loadList(context), loadDetail(context)]);
}
export async function selectMcpHistoryDirection(context: ActionContext, direction: string): Promise<void> {
  const local = context.uiState.mcpHistory;
  if (context.getViewState()?.overlay !== "mcp_history" || (direction !== "instruction" && direction !== "execution") || local.direction === direction) return;
  resetHistoryPage(local, direction, 0, [], null);
  context.rerender();
  await refreshMcpHistory(context, true);
}
function resetHistoryPage(local: McpHistoryUiState, direction: McpHistoryDirection, offset: number, previousOffsets: number[], anchor: string | null): void {
  invalidateMcpHistory(local);
  Object.assign(local, { direction, offset, anchor, previousOffsets, rows: [], nextOffset: null,
    loaded: false, selectedId: null, detail: null, error: "", detailError: "", notice: "" });
}
export async function reloadMcpHistory(context: ActionContext): Promise<void> {
  const local = context.uiState.mcpHistory;
  if (context.getViewState()?.overlay !== "mcp_history") return;
  resetHistoryPage(local, local.direction, 0, [], null);
  context.rerender();
  await refreshMcpHistory(context, true);
}
export async function pageMcpHistory(context: ActionContext, forward: boolean): Promise<void> {
  const local = context.uiState.mcpHistory;
  if (context.getViewState()?.overlay !== "mcp_history" || local.listRequest.active) return;
  if (forward) {
    if (local.nextOffset === null) return;
    resetHistoryPage(local, local.direction, local.nextOffset, [...local.previousOffsets, local.offset], local.anchor);
  } else {
    if (!local.previousOffsets.length) return;
    resetHistoryPage(local, local.direction, local.previousOffsets.at(-1)!, local.previousOffsets.slice(0, -1), local.anchor);
  }
  context.rerender();
  await refreshMcpHistory(context, true);
}
export async function selectMcpHistory(context: ActionContext, id: string): Promise<void> {
  const local = context.uiState.mcpHistory;
  if (context.getViewState()?.overlay !== "mcp_history" || !local.rows.some(row => row.id === id && row.direction === local.direction)) return;
  if (local.selectedId === id) return;
  local.detailRequest.active = null;
  local.selectedId = id;
  local.detail = null;
  local.detailError = "";
  local.notice = "";
  context.rerender();
  await loadDetail(context);
}
export async function operateMcpHistory(context: ActionContext, kind: "export" | "stop"): Promise<void> {
  const local = context.uiState.mcpHistory;
  const row = selectedMcpHistoryRow(local);
  if (context.getViewState()?.overlay !== "mcp_history" || !row || (kind === "stop" && !row.can_stop)) return;
  const request = beginAsyncTransaction(local.operationRequest, { ...target(local, row.id), kind }, "single-flight", (token, value) => ({ token, ...value }));
  if (!request) return;
  if (kind === "stop") { local.listRequest.active = null; local.detailRequest.active = null; }
  local.notice = "";
  local.detailError = "";
  context.rerender();
  try {
    const result = await command<McpHistoryDetail | { path: string | null }>(`mcp_history_${kind}`, { direction: request.direction, id: request.id });
    if (!asyncTransactionIsCurrent(local.operationRequest, request) || !stillVisible(context, request)) return;
    if ("path" in result) {
      local.notice = result.path === null ? "保存をキャンセルしました。" : `Markdownを保存しました: ${result.path}`;
    } else if (result.row.id === request.id && result.row.direction === request.direction) {
      local.detail = result;
      local.rows = local.rows.map(candidate => candidate.id === row.id ? result.row : candidate);
      local.notice = "停止を要求しました。停止確認の状態を確認してください。";
    }
  } catch {
    if (asyncTransactionIsCurrent(local.operationRequest, request) && stillVisible(context, request)) {
      local.detailError = kind === "export" ? "Markdownを保存できませんでした。保存先を確認して再試行してください。"
        : "停止を要求できませんでした。通信状態と実行端末の履歴を確認してください。";
    }
  } finally {
    if (clearAsyncTransaction(local.operationRequest, request) && context.getViewState()?.overlay === "mcp_history") context.rerender();
  }
}

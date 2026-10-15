import { createAsyncTransactionSlot, type AsyncTransactionSlot } from "./async_transaction.ts";

export type McpHistoryDirection = "instruction" | "execution";
export interface McpHistoryRow {
  id: string;
  direction: McpHistoryDirection;
  created_at_ms: number;
  updated_at_ms: number | null;
  title: string;
  peer_label: string;
  target_label: string;
  state: string;
  stop_status: string;
  session_id: string;
  job_id: string | null;
  profile_id: string;
  root_task_id: string;
  device_path: string[];
  can_stop: boolean;
  state_source: "local_runtime" | "last_observed";
  result_received: boolean;
}
export interface McpHistoryPage { rows: McpHistoryRow[]; next_offset: number | null; anchor: string | null }
export interface McpHistoryDetail { row: McpHistoryRow; markdown: string; truncated: boolean }
export interface McpHistoryRequest {
  readonly token: number;
  readonly direction: McpHistoryDirection;
  readonly offset: number;
  readonly anchor: string | null;
  readonly id: string | null;
}
export interface McpHistoryUiState {
  direction: McpHistoryDirection;
  offset: number;
  anchor: string | null;
  previousOffsets: number[];
  rows: McpHistoryRow[];
  nextOffset: number | null;
  loaded: boolean;
  selectedId: string | null;
  detail: McpHistoryDetail | null;
  error: string;
  detailError: string;
  notice: string;
  listRequest: AsyncTransactionSlot<McpHistoryRequest>;
  detailRequest: AsyncTransactionSlot<McpHistoryRequest>;
  operationRequest: AsyncTransactionSlot<McpHistoryRequest & { kind: "export" | "stop" }>;
  lastPollAt: number;
}
export type McpHistoryPresentation = Omit<McpHistoryUiState, "listRequest" | "detailRequest" | "operationRequest" | "lastPollAt">
  & { listPending: boolean; detailPending: boolean; operation: "export" | "stop" | null };

export function createMcpHistoryUiState(): McpHistoryUiState {
  return { direction: "instruction", offset: 0, anchor: null, previousOffsets: [], rows: [], nextOffset: null,
    loaded: false, selectedId: null, detail: null, error: "", detailError: "", notice: "",
    listRequest: createAsyncTransactionSlot(), detailRequest: createAsyncTransactionSlot(),
    operationRequest: createAsyncTransactionSlot(), lastPollAt: 0 };
}
export function mcpHistoryPresentation(state: McpHistoryUiState): McpHistoryPresentation {
  const { listRequest, detailRequest, operationRequest, lastPollAt: _poll, ...value } = state;
  return { ...value, listPending: listRequest.active !== null, detailPending: detailRequest.active !== null,
    operation: operationRequest.active?.kind ?? null };
}
export function invalidateMcpHistory(state: McpHistoryUiState): void {
  state.listRequest.active = null;
  state.detailRequest.active = null;
  state.operationRequest.active = null;
  state.lastPollAt = 0;
}
export function selectedMcpHistoryRow(state: Pick<McpHistoryPresentation, "selectedId" | "direction" | "detail" | "rows">): McpHistoryRow | null {
  if (state.detail?.row.id === state.selectedId && state.detail.row.direction === state.direction) return state.detail.row;
  return state.rows.find(row => row.id === state.selectedId && row.direction === state.direction) ?? null;
}
export function mcpHistoryStateLabel(value: string): string {
  return ({ preparing: "準備中", accepted: "受付済み", running: "実行中", awaiting_approval: "実行端末で承認待ち",
    cancelling: "停止要求中", completed: "完了", failed: "失敗", interrupted: "中断", unknown: "未確認" } as Record<string, string>)[value] ?? "未確認";
}
export function mcpHistoryStopLabel(value: string): string {
  return ({ requested: "停止を要求済み", unconfirmed: "停止を確認できていません", confirmed: "停止確認済み" } as Record<string, string>)[value] ?? "";
}
export function mcpHistoryDate(value: number | null): string {
  if (value === null || !Number.isFinite(value)) return "記録なし";
  return new Date(value).toLocaleString("ja-JP", { hour12: false });
}

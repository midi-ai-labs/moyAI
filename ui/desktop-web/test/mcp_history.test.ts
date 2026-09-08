import assert from "node:assert/strict";
import test from "node:test";
import type { ActionContext } from "../src/actions.ts";
import { ACTIONS, actionById } from "../src/actions.ts";
import { invalidateMcpHistory, createMcpHistoryUiState, mcpHistoryPresentation, type McpHistoryRow, type McpHistoryDetail, type McpHistoryPage } from "../src/mcp_history_state.ts";
import { openMcpHistory, refreshMcpHistory, reloadMcpHistory, selectMcpHistoryDirection, selectMcpHistory, pageMcpHistory, operateMcpHistory } from "../src/mcp_history_actions.ts";
import { renderMcpHistoryOverlay } from "../src/mcp_history_render.ts";
import { createDesktopRenderModel, DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION } from "../src/render_projection.ts";
import type { DesktopViewState } from "../src/types.ts";
import { settingsSurfaceIdentity } from "../src/settings_surface.ts";

function row(overrides: Partial<McpHistoryRow> = {}): McpHistoryRow {
  return { id: "ref-a", direction: "instruction", created_at_ms: 1_700_000_000_000, updated_at_ms: null,
    title: "WinBのCPU使用率", peer_label: "WinB", target_label: "temp", state: "running", stop_status: "none",
    session_id: "chat-a", job_id: "job-b", profile_id: "profile-b", root_task_id: "chat-a", device_path: ["WinA", "WinB"],
    can_stop: true, state_source: "last_observed", result_received: false, ...overrides };
}
const detail = (value = row()): McpHistoryDetail => ({ row: value, markdown: `# ${value.title}\n\n処理の記録`, truncated: false });
const page = (rows = [row()], next_offset: number | null = null, anchor: string | null = "anchor-a"): McpHistoryPage => ({ rows, next_offset, anchor });
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (value: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function fixture() {
  const local = createMcpHistoryUiState();
  const view = { overlay: "mcp_history" };
  const calls: { name: string; args: Record<string, unknown> }[] = [];
  const context = { uiState: { mcpHistory: local }, getViewState: () => view, rerender() {},
    async mutate(name: string) { calls.push({ name, args: {} }); view.overlay = "mcp_history"; } } as unknown as ActionContext;
  return { local, view, context, calls };
}
async function withContext(invoke: (name: string, args: Record<string, unknown>) => Promise<unknown>, run: (fixture: ReturnType<typeof fixture>) => Promise<void>) {
  const value = fixture();
  const previous = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (name: string, args: Record<string, unknown>) => { value.calls.push({ name, args }); return invoke(name, args); } } } });
  try { await run(value); }
  finally { if (previous) Object.defineProperty(globalThis, "window", previous); else delete (globalThis as Record<string, unknown>).window; }
}

test("history opens locally, separates instructions and executions, and never queries a peer implicitly", async () => {
  await withContext(async (name, args) => name === "mcp_history_list"
    ? page([row({ direction: args.direction as McpHistoryRow["direction"] })])
    : detail(row({ direction: args.direction as McpHistoryRow["direction"] })), async ({ context, local, calls }) => {
    await openMcpHistory(context);
    assert.equal(local.selectedId, null);
    assert.equal(local.rows[0].direction, "instruction");
    await selectMcpHistory(context, "ref-a");
    assert.equal(local.detail?.row.direction, "instruction");
    await selectMcpHistoryDirection(context, "execution");
    assert.equal(local.selectedId, null);
    assert.equal(local.detail, null);
    await selectMcpHistory(context, "ref-a");
    assert.equal(local.detail?.row.direction, "execution");
    assert.deepEqual(calls.map(call => call.name), ["show_mcp_history", "mcp_history_list", "mcp_history_detail", "mcp_history_list", "mcp_history_detail"]);
    assert.deepEqual(calls[3].args, { direction: "execution", offset: 0, anchor: null });
  });
});

test("paging retains the returned anchor and explicit refresh returns to the newest first page", async () => {
  await withContext(async (_name, args) => page([row({ id: `ref-${args.offset}` })], args.offset === 0 ? 50 : args.offset === 50 ? 100 : null, "snapshot-first"), async ({ context, local, calls }) => {
    await refreshMcpHistory(context, true);
    await pageMcpHistory(context, true);
    await pageMcpHistory(context, true);
    assert.deepEqual(local.previousOffsets, [0, 50]);
    assert.equal(local.offset, 100);
    await pageMcpHistory(context, true);
    assert.equal(calls.length, 3);
    await pageMcpHistory(context, false);
    assert.equal(local.offset, 50);
    await reloadMcpHistory(context);
    assert.equal(local.offset, 0);
    assert.deepEqual(local.previousOffsets, []);
    assert.deepEqual(calls.map(call => call.args), [
      { direction: "instruction", offset: 0, anchor: null },
      { direction: "instruction", offset: 50, anchor: "snapshot-first" },
      { direction: "instruction", offset: 100, anchor: "snapshot-first" },
      { direction: "instruction", offset: 50, anchor: "snapshot-first" },
      { direction: "instruction", offset: 0, anchor: null },
    ]);
  });
});

test("slow old-direction list success cannot replace the selected execution page", async () => {
  const old = deferred<McpHistoryPage>();
  await withContext(async (_name, args) => args.direction === "instruction" ? old.promise : page([row({ direction: "execution", id: "job-b" })]), async ({ context, local }) => {
    const pending = refreshMcpHistory(context, true);
    await selectMcpHistoryDirection(context, "execution");
    old.resolve(page());
    await pending;
    assert.equal(local.direction, "execution");
    assert.equal(local.rows[0].id, "job-b");
    assert.equal(local.error, "");
  });
});

test("old detail success and failure cannot repaint another selected task or a reopened overlay", async () => {
  const old = deferred<McpHistoryDetail>();
  await withContext(async (_name, args) => args.id === "ref-a" ? old.promise : detail(row({ id: "ref-b" })), async ({ context, local, view }) => {
    local.rows = [row(), row({ id: "ref-b" })];
    const pending = selectMcpHistory(context, "ref-a");
    await selectMcpHistory(context, "ref-b");
    old.resolve(detail());
    await pending;
    assert.equal(local.detail?.row.id, "ref-b");
  });
  const closed = deferred<McpHistoryDetail>();
  await withContext(async () => closed.promise, async ({ context, local, view }) => {
    local.rows = [row()];
    const pending = selectMcpHistory(context, "ref-a");
    view.overlay = "none";
    invalidateMcpHistory(local);
    view.overlay = "mcp_history";
    closed.resolve(detail());
    await pending;
    assert.equal(local.detail, null);
    assert.equal(local.detailError, "");
    assert.equal(local.detailRequest.active, null);
  });
  const failure = deferred<McpHistoryDetail>();
  await withContext(async (_name, args) => args.id === "ref-a" ? failure.promise : detail(row({ id: "ref-b" })), async ({ context, local }) => {
    local.rows = [row(), row({ id: "ref-b" })];
    const pending = selectMcpHistory(context, "ref-a");
    await selectMcpHistory(context, "ref-b");
    failure.reject(new Error("old failure"));
    await pending;
    assert.equal(local.detailError, "");
    assert.equal(local.detail?.row.id, "ref-b");
  });
});

test("export captures direction and task, ignores stale settlement, and distinguishes save cancellation", async () => {
  const old = deferred<{ path: string | null }>();
  let exports = 0;
  await withContext(async (name, args) => name === "mcp_history_export" ? ++exports === 1 ? old.promise : { path: null }
    : detail(row({ id: args.id as string })), async ({ context, local, calls }) => {
    local.rows = [row(), row({ id: "ref-b" })]; local.selectedId = "ref-a";
    const pending = operateMcpHistory(context, "export");
    await operateMcpHistory(context, "export");
    assert.equal(exports, 1);
    await selectMcpHistory(context, "ref-b");
    old.resolve({ path: "C:/exports/old.md" });
    await pending;
    assert.equal(local.notice, "");
    assert.deepEqual(calls[0], { name: "mcp_history_export", args: { direction: "instruction", id: "ref-a" } });
    await operateMcpHistory(context, "export");
    assert.match(local.notice, /キャンセル/);
    assert.equal(local.detailError, "");
  });
});

test("export failure remains inside the selected history and releases its action for retry", async () => {
  await withContext(async () => { throw new Error("write denied"); }, async ({ context, local }) => {
    local.rows = [row()]; local.selectedId = "ref-a";
    await operateMcpHistory(context, "export");
    assert.match(local.detailError, /保存できません/);
    assert.equal(local.operationRequest.active, null);
    assert.equal(local.notice, "");
    assert.equal(local.selectedId, "ref-a");
  });
});

test("stop uses the selected server-owned capability and refreshes the exact task receipt", async () => {
  await withContext(async () => detail(row({ state: "interrupted", stop_status: "confirmed", can_stop: false })), async ({ context, local, calls }) => {
    local.rows = [row({ can_stop: false })]; local.selectedId = "ref-a";
    await operateMcpHistory(context, "stop");
    assert.equal(calls.length, 0);
    local.rows[0].can_stop = true;
    await operateMcpHistory(context, "stop");
    assert.deepEqual(calls, [{ name: "mcp_history_stop", args: { direction: "instruction", id: "ref-a" } }]);
    assert.equal(local.detail?.row.stop_status, "confirmed");
    assert.equal(local.rows[0].can_stop, false);
  });
});

test("polling is single-flight, throttled and closed-overlay reads are absent", async () => {
  const delayed = deferred<McpHistoryPage>();
  await withContext(async () => delayed.promise, async ({ context, local, calls, view }) => {
    const pending = refreshMcpHistory(context, true);
    await refreshMcpHistory(context, true);
    assert.equal(calls.length, 1);
    delayed.resolve(page());
    await pending;
    await refreshMcpHistory(context);
    assert.equal(calls.length, 1);
    view.overlay = "none";
    invalidateMcpHistory(local);
    await refreshMcpHistory(context, true);
    assert.equal(calls.length, 1);
  });
});

test("list failures retain the latest visible records and detail failures cannot enable a wrong task", async () => {
  await withContext(async () => { throw new Error("storage unavailable"); }, async ({ context, local }) => {
    local.rows = [row()]; local.loaded = true;
    await refreshMcpHistory(context, true);
    assert.equal(local.rows.length, 1);
    assert.match(local.error, /前回取得した記録/);
    await selectMcpHistory(context, "missing");
    assert.equal(local.selectedId, null);
    await selectMcpHistory(context, "ref-a");
    assert.equal(local.detail, null);
    assert.match(local.detailError, /詳細を取得できません/);
  });
});

test("rendered history separates state observation from result receipt and treats remote markdown as untrusted", () => {
  const local = createMcpHistoryUiState(); local.rows = [row()]; local.selectedId = "ref-a";
  local.detail = { row: row(), truncated: true, markdown: '# 実行記録\n\n<script>evil()</script>\n\n[危険](javascript:evil)\n\n```html\n<img src=x onerror=evil()>\n```' };
  const html = renderMcpHistoryOverlay(mcpHistoryPresentation(local));
  assert.match(html, /最終確認状態/);
  assert.match(html, /最終記録日時/);
  assert.match(html, /記録なし/);
  assert.match(html, /結果の受信/);
  assert.match(html, /一部を省略/);
  assert.match(html, /&lt;script&gt;/);
  assert.doesNotMatch(html, /<script|<img|href="javascript:/);
  assert.doesNotMatch(html, /class="modal-backdrop" data-action/);
  assert.match(html, /id="mcp-history-legacy"[^>]* hidden/);
  assert.doesNotMatch(renderMcpHistoryOverlay(mcpHistoryPresentation(local), true), /id="mcp-history-legacy"[^>]* hidden/);
  local.direction = "execution"; local.rows = [row({ direction: "execution", state_source: "local_runtime" })]; local.detail = null;
  const incoming = renderMcpHistoryOverlay(mcpHistoryPresentation(local));
  assert.match(incoming, /指示側の結果受信/);
  assert.match(incoming, /この端末では未確認/);
});

test("history empty states, primary action registry and retained surface identity agree", () => {
  const local = createMcpHistoryUiState(); local.loaded = true;
  assert.match(renderMcpHistoryOverlay(mcpHistoryPresentation(local)), /指示した履歴はありません/);
  local.direction = "execution";
  assert.match(renderMcpHistoryOverlay(mcpHistoryPresentation(local)), /受け付けた実行履歴はありません/);
  const view = { overlay: "mcp_history", confirmation_visible: false } as DesktopViewState;
  assert.equal(settingsSurfaceIdentity(view), "mcp-history:application");
  assert.equal(actionById("show-mcp-history")?.label, "MCP履歴");
  assert.equal(ACTIONS.find(action => action.id === "show-mcp-history")?.menu, "view");
  assert.equal(ACTIONS.find(action => action.id === "show-mcp-publish")?.menu, undefined);
  const model = createDesktopRenderModel(view, DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION);
  assert.equal(actionById("mcp-history-export")?.enabled(model, { value: "", index: -1 }), false);
  assert.equal(actionById("mcp-history-stop")?.enabled(model, { value: "", index: -1 }), false);
});

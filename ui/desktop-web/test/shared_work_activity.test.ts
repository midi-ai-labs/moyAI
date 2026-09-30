import assert from "node:assert/strict";
import test from "node:test";
import { renderWorkDetails } from "../src/shared_work_details.ts";
import { retainSharedWorkSurface } from "../src/shared_work_render.ts";
import type { WorkActivity } from "../src/shared_work_state.ts";
import { sharedUiFixture } from "./shared_work_fixture.ts";

function fixture() {
  const local = sharedUiFixture();
  local.projection!.selected_job_id = "job-a";
  local.projection!.detail = { id: "job-a", project_id: "project-a", root_id: "job-a", parent_id: null,
    environment_id: "env-a", title: "Build", input: {}, result: null, state: "waiting_child", awaiting_child_id: "child",
    revision: 1, created_at_ms: 1, updated_at_ms: 1 };
  local.projection!.status!.environments.push({ ...local.projection!.status!.environments[0], id: "env-b", runner_id: "runner-b", device_label: "WinB" });
  const activity: WorkActivity = { job_id: "child", attempt_id: "attempt-b", generation: 1, environment_id: "env-b", runner_id: "runner-b",
    revision: 4, observed_at_ms: 1_800_000_000_000, truncated: false, items: [
      { position: 2, kind: "assistant_message", payload: { content: [{ kind: "text", text: "**Flask**を起動します。" }] } },
      { position: 3, kind: "tool_call", payload: { call_id: "call-b", tool_name: "shell", arguments_json: '{"command":"uv run python app.py"}' } },
    ] };
  local.projection!.transcript = Object.assign({ items: [], next_after: null }, { activity });
  return { local, activity };
}
function visible(html: string): string {
  let depth = 0;
  return html.split(/(<\/?details\b[^>]*>)/).filter(part => {
    if (part.startsWith("<details")) { depth++; return false; }
    if (part.startsWith("</details")) { depth--; return false; }
    return depth === 0;
  }).join("");
}

test("a child public response and current operation are visible without opening the archived record", () => {
  const { local } = fixture();
  const html = visible(renderWorkDetails(local, "record"));
  assert.match(html, /WinB/);
  assert.match(html, /<strong>Flask<\/strong>を起動します/);
  assert.match(html, /コマンドの実行/);
  assert.match(html, /uv run python app\.py/);
  assert.match(html, /結果待ち/);
  assert.match(html, /更新/);
  assert.doesNotMatch(html.replace(/<[^>]*>/g, ""), /attempt-b|runner-b|env-b|call-b|Hubに届き次第/);
});

test("activity public output never exposes private parts, metadata, unknown events or executable markup", () => {
  const { local, activity } = fixture();
  activity.items.push(
    { position: 4, kind: "assistant_message", payload: { content: [{ kind: "text", text: "公開回答" }, { kind: "thinking", text: "private-thought" }], metadata: "private-metadata" } },
    { position: 5, kind: "request_diagnostics", payload: { message: "private-diagnostics" } },
    { position: 6, kind: "future_kind", payload: { message: "private-unknown" } },
    { position: 7, kind: "tool_output", payload: { call_id: "call-b", status: "completed", success: true, title: "起動確認", output_text: "<script>alert(1)</script>", metadata: "private-output-metadata" } },
  );
  const html = visible(renderWorkDetails(local, "record"));
  assert.match(html, /公開回答/);
  assert.match(html, /&lt;script&gt;/);
  assert.doesNotMatch(html, /private-|<script>|結果待ち/);
});

test("a completed transport with unsuccessful tool output is displayed as failure", () => {
  const { local, activity } = fixture();
  activity.items.push({ position: 4, kind: "tool_output", payload: { call_id: "call-b", status: "completed", success: false, title: "shell", output_text: "exit code 2" } });
  const html = visible(renderWorkDetails(local, "record"));
  assert.match(html, /失敗/);
  assert.match(html, /exit code 2/);
  assert.doesNotMatch(html, /結果待ち|操作が成功/);
});

test("human denial and cancellation stay distinct from execution failure", () => {
  for (const [status, label] of [["declined", "拒否"], ["cancelled", "停止"]]) {
    const { local, activity } = fixture();
    activity.items.push({ position: 4, kind: "tool_output", payload: { call_id: "call-b", status, success: false, title: "shell", output_text: "未実行" } });
    const html = visible(renderWorkDetails(local, "record"));
    assert.ok(html.includes(`操作結果（${label}）`));
    assert.doesNotMatch(html, /失敗|結果待ち|操作結果（完了）/);
  }
});

test("terminal state retires a stale activity snapshot and shows the canonical answer once", () => {
  const { local } = fixture();
  local.projection!.transcript!.items.push({ position: 1, kind: "assistant_message", payload: "確定回答" });
  for (const state of ["succeeded", "failed", "cancelled"]) {
    local.projection!.detail!.state = state;
    const html = renderWorkDetails(local, "record");
    assert.doesNotMatch(visible(html), /Flask|結果待ち/);
    assert.equal(html.split("確定回答").length - 1, 2, "one rendered answer plus its closed source record");
  }
});

test("older Hub without activity keeps the existing state and transcript behavior", () => {
  const { local } = fixture();
  local.projection!.transcript = { items: [], next_after: null };
  assert.match(visible(renderWorkDetails(local, "record")), /依頼先の仕事の結果を待っています/);
});

test("only the current attempt owns activity selection and a bounded tail is explained", () => {
  const { local, activity } = fixture();
  activity.truncated = true;
  const initial = renderWorkDetails(local, "record");
  assert.match(visible(initial), /直近/);
  const owner = initial.match(/data-shared-region="activity" data-shared-record-owner="([^"]+)"/)?.[1];
  assert.ok(owner);
  activity.attempt_id = "attempt-new";
  assert.notEqual(renderWorkDetails(local, "record").match(/data-shared-region="activity" data-shared-record-owner="([^"]+)"/)?.[1], owner);
});

test("pending activity follows the exact child approval and does not call permission pending executed", () => {
  const { local, activity } = fixture();
  local.projection!.approval = { id: "approval-b", attempt_id: "attempt-b", status: "pending", decision: null,
    expires_at_ms: 1_800_000_100_000, can_decide: true, request: { access: "shell", summary: "Start server", details: [], targets: [], outside_workspace: false, risks: [] },
    context: { job_id: "child", project_id: "project-a", root_id: "job-a", conversation_id: "conversation-a", job_title: "Host", controller_device_id: "runner-a", controller_device_label: "WinA", execution_device_id: "runner-b", execution_device_label: "WinB" } };
  activity.items.push({ position: 4, kind: "tool_call", payload: { call_id: "other-call", tool_name: "read", arguments_json: '{"path":"README.md"}' } });
  const pending = visible(renderWorkDetails(local, "record"));
  assert.match(pending, /この仕事は承認待ちです/);
  assert.equal(pending.split("結果待ち").length - 1, 2, "job-level approval does not identify which of the pending tools it authorizes");
  assert.doesNotMatch(pending, /コマンドの実行 · 承認待ち|ファイルの読取り · 承認待ち/);
  local.projection!.approval.attempt_id = "old-attempt";
  assert.doesNotMatch(visible(renderWorkDetails(local, "record")), /承認待ち/);
  local.projection!.approval.attempt_id = "attempt-b";
  local.projection!.approval.context!.job_id = "other-child";
  assert.doesNotMatch(visible(renderWorkDetails(local, "record")), /承認待ち/);
});

test("reading the archived record does not freeze activity, while selecting activity retains only its exact attempt", () => {
  const original = Object.getOwnPropertyDescriptor(globalThis, "document");
  let selected: unknown = null;
  const doc = { activeElement: { matches: () => true }, getSelection: () => ({ isCollapsed: selected === null, rangeCount: 1, containsNode: (node: unknown) => node === selected }) };
  Object.defineProperty(globalThis, "document", { configurable: true, value: doc });
  const make = (name: string, owner: string) => {
    const value = { dataset: { sharedRegion: name, sharedRecordOwner: owner }, ownerDocument: doc, replacement: null as unknown,
      querySelectorAll: () => [], contains: () => name === "transcript", isEqualNode: () => false,
      replaceWith(next: unknown) { value.replacement = next; } };
    return value;
  };
  const archive = make("transcript", "same-archive");
  const activity = make("activity", "attempt-b");
  const nextArchive = make("transcript", "same-archive");
  const nextActivity = make("activity", "attempt-b");
  const current = { dataset: { sharedOwner: "same-conversation" }, querySelector: (selector: string) => selector.includes('"activity"') ? activity : archive };
  const next = { dataset: { sharedOwner: "same-conversation" }, querySelectorAll: () => [nextArchive, nextActivity] };
  try {
    assert.equal(retainSharedWorkSurface(current as unknown as HTMLElement, next as unknown as HTMLElement), true);
    assert.equal(archive.replacement, null, "focused archive is kept connected");
    assert.equal(activity.replacement, nextActivity, "the active child still updates");
    activity.replacement = null;
    selected = activity;
    retainSharedWorkSurface(current as unknown as HTMLElement, next as unknown as HTMLElement);
    assert.equal(activity.replacement, null, "reading an exact activity snapshot is not interrupted");
    nextActivity.dataset.sharedRecordOwner = "attempt-new";
    retainSharedWorkSurface(current as unknown as HTMLElement, next as unknown as HTMLElement);
    assert.equal(activity.replacement, nextActivity, "a different attempt cannot retain old activity");
  } finally { if (original) Object.defineProperty(globalThis, "document", original); else delete (globalThis as Record<string, unknown>).document; }
});

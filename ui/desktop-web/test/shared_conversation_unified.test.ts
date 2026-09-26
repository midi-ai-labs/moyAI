import assert from "node:assert/strict";
import test from "node:test";
import { acceptSharedWork, sharedWorkActionEnabled, sharedWorkPresentation } from "../src/shared_work_state.ts";
import { renderSharedWork } from "../src/shared_work_render.ts";
import { sharedWorkAction } from "../src/shared_work_actions.ts";
import type { ActionContext } from "../src/actions.ts";
import { sharedProjection, sharedUiFixture } from "./shared_work_fixture.ts";

test("Hub conversation uses the normal message and send renderer for every continuation", () => {
  const local = sharedUiFixture();
  const first = { ...local.projection!.status!.jobs[0], id: "job-first", conversation_id: "conversation-a", title: "最初", state: "succeeded", can_cancel: false };
  const second = { ...first, id: "job-second", title: "続き", created_at_ms: 2 };
  local.projection!.selected_job_id = second.id;
  local.projection!.selected_conversation_id = "conversation-a";
  local.projection!.conversations = [{ id: "conversation-a", title: "最初", latest_job_id: second.id, updated_at_ms: 3 }];
  local.projection!.detail = { id: second.id, project_id: "project-a", conversation_id: "conversation-a", root_id: first.id,
    parent_id: null, environment_id: "env-a", title: second.title, input: { prompt: "次を調べて" }, result: { text: "結果二" },
    state: "succeeded", awaiting_child_id: null, revision: 2, created_at_ms: 2, updated_at_ms: 3, can_continue: true };
  local.projection!.conversation_history = { project_id: "project-a", conversation_id: "conversation-a", snapshot: 2,
    jobs: [
      { job: second, input: { prompt: "次を調べて" }, result: { text: "結果二" }, artifacts: [{ id: "asset-b", project_id: "project-a", job_id: second.id, kind: "artifact", name: "result.md", sha256: "a".repeat(64), byte_length: 9, created_at_ms: 2, version: 1, base_sha256: null, purged_at_ms: null }], more_artifacts: false },
      { job: first, input: { prompt: "最初の依頼" }, result: { text: "結果一" }, artifacts: [], more_artifacts: false },
    ], next_before: 1 };
  const html = renderSharedWork(sharedWorkPresentation(local));
  assert.ok(html.indexOf("最初の依頼") < html.indexOf("結果一"));
  assert.ok(html.indexOf("結果一") < html.indexOf("次を調べて"));
  assert.ok(html.indexOf("次を調べて") < html.indexOf("結果二"));
  assert.equal((html.match(/class="message user"/g) ?? []).length, 2);
  assert.equal((html.match(/class="message assistant"/g) ?? []).length, 2);
  assert.match(html, /data-action="shared-history-next"/);
  assert.match(html, /result\.md/);
  assert.match(html, /<textarea id="shared-followup"/);
  assert.match(html, /data-action="send" title="送信"/);
  assert.doesNotMatch(html, /data-action="shared-continue"|data-action="shared-submit"/);
});

test("Hub conversation drafts follow project and conversation identity, while a new generation discards them", () => {
  const local = sharedUiFixture();
  const a = sharedProjection({ revision: "2", selected_job_id: "job-a", detail: { id: "job-a", project_id: "project-a", conversation_id: "conversation-a", root_id: "job-a", parent_id: null,
    environment_id: "env-a", title: "A", input: {}, result: null, state: "succeeded", awaiting_child_id: null, revision: 1, created_at_ms: 1, updated_at_ms: 1 } });
  const b = sharedProjection({ ...a, revision: "3", selected_job_id: "job-b", detail: { ...a.detail!, id: "job-b", conversation_id: "conversation-b" } });
  acceptSharedWork(local, a); local.draft.followup = "Aの書きかけ";
  acceptSharedWork(local, b); assert.equal(local.draft.followup, "");
  local.draft.followup = "Bの書きかけ";
  acceptSharedWork(local, { ...a, revision: "4" }); assert.equal(local.draft.followup, "Aの書きかけ");
  acceptSharedWork(local, { ...b, revision: "5" }); assert.equal(local.draft.followup, "Bの書きかけ");
  acceptSharedWork(local, { ...b, revision: "6", generation: "2" }); assert.equal(local.draft.followup ?? "", "");
});

test("failed Hub work explains recovery in the common conversation without opening raw details", () => {
  const local = sharedUiFixture();
  const job = { ...local.projection!.status!.jobs[0], id: "failed-job", conversation_id: "conversation-a", state: "failed", can_continue: true };
  local.projection!.selected_job_id = job.id;
  local.projection!.selected_conversation_id = "conversation-a";
  local.projection!.conversations = [{ id: "conversation-a", title: "失敗した仕事", latest_job_id: job.id, updated_at_ms: 2 }];
  local.projection!.detail = { id: job.id, project_id: "project-a", conversation_id: "conversation-a", root_id: job.id,
    parent_id: null, environment_id: "env-a", title: "失敗した仕事", input: { prompt: "接続を確認" }, result: null,
    state: "failed", awaiting_child_id: null, revision: 2, created_at_ms: 1, updated_at_ms: 2, can_continue: true };
  local.projection!.conversation_history = { project_id: "project-a", conversation_id: "conversation-a", snapshot: 1,
    jobs: [{ job, input: { prompt: "接続を確認" }, result: { summary: { terminal: { outcome: { kind: "failed", error: "URLへ到達できません" } } } },
      artifacts: [], more_artifacts: false }], next_before: null };
  const html = renderSharedWork(sharedWorkPresentation(local));
  assert.match(html, /仕事を完了できませんでした/);
  assert.match(html, /URLへ到達できません/);
  assert.match(html, /href="#shared-followup"/);
  assert.doesNotMatch(html, /回答文はありません/);
});

test("Hub full stop targets the selected conversation and stays distinct from one app stop", async () => {
  const local = sharedUiFixture();
  local.projection!.selected_job_id = "job-a";
  local.projection!.detail = { id: "job-a", project_id: "project-a", conversation_id: "conversation-a", root_id: "job-a", parent_id: null,
    environment_id: "env-a", title: "進行中", input: { prompt: "作成" }, result: null, state: "running", awaiting_child_id: null,
    revision: 3, created_at_ms: 1, updated_at_ms: 2 };
  local.projection!.status!.jobs[0].conversation_id = "conversation-a";
  assert.equal(sharedWorkActionEnabled(sharedWorkPresentation(local), "stop-conversation", "conversation-a"), true);
  assert.equal(sharedWorkActionEnabled(sharedWorkPresentation(local), "stop-conversation", "conversation-other"), false);
  assert.match(renderSharedWork(sharedWorkPresentation(local)), /data-action="shared-stop-conversation" data-value="conversation-a"/);
  let request: unknown;
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (_name: string, args: unknown) => {
    request = args; return { ...local.projection, revision: "2" };
  } } } });
  try {
    const context = { uiState: { sharedWork: local }, getViewState: () => ({ overlay: "none", hub_project_open: true }), rerender() {} } as unknown as ActionContext;
    await sharedWorkAction(context, "stop_conversation", "conversation-a");
    assert.deepEqual(request, { expectedGeneration: "1", request: { kind: "stop_conversation", project_id: "project-a", conversation_id: "conversation-a" } });
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});

test("retained app occupancy does not imply that a different project is using the PC", () => {
  const local = sharedUiFixture();
  local.projection!.detail = { id: "job-a", project_id: "project-a", conversation_id: "conversation-a", root_id: "job-a", parent_id: null,
    environment_id: "env-a", title: "TODOアプリ", input: {}, result: null, state: "succeeded", awaiting_child_id: null,
    revision: 3, created_at_ms: 1, updated_at_ms: 2,
    retained_services: [{ service_id: "app-a", environment_id: "env-a", expires_at_ms: 9_999_999_999_999, stop_requested: false, uncertain: false, can_stop: true }] };
  const html = renderSharedWork(sharedWorkPresentation(local));
  assert.match(html, /1 \/ 2 枠を使用中/);
  assert.match(html, /ほかに 1 枠使用中（起動中のアプリを含む）/);
  assert.match(html, /この会話で起動中のアプリ/);
  assert.match(html, /data-action="shared-stop-service" data-value="app-a"/);
  assert.doesNotMatch(html, /他のプロジェクトで/);
});

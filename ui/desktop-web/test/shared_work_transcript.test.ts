import assert from "node:assert/strict";
import test from "node:test";
import { renderWorkDetails, renderWorkResult, retainWorkRecord } from "../src/shared_work_details.ts";
import { sharedWorkPresentation, type WorkAsset } from "../src/shared_work_state.ts";
import { renderSharedWork } from "../src/shared_work_render.ts";
import { sharedUiFixture } from "./shared_work_fixture.ts";

test("a failed Runner outcome explains the failure before collapsed diagnostics even if it has partial answer text", () => {
  const error = "Could not connect to the model provider. <script>not markup</script>";
  for (const result of [
    { version: 1, text: null, summary: { terminal: { outcome: { kind: "failed", error } } } },
    { version: 1, text: "Partial answer", summary: { terminal: { outcome: { kind: "failed", error } } } },
    { version: 1, error },
  ]) {
    const html = renderWorkResult(result, "job");
    const visible = html.replace(/<details\b[\s\S]*?<\/details>/g, "");
    assert.match(visible, /仕事を完了できませんでした/);
    assert.match(visible, /Could not connect to the model provider/);
    assert.match(visible, /実行するPCの担当者/);
    assert.doesNotMatch(visible, /回答文はありません|<script>/);
    if (result.text) { assert.match(visible, /中断までの回答/); assert.match(visible, /Partial answer/); }
    assert.match(html, /&lt;script&gt;/);
  }
  assert.match(renderWorkResult({ text: "完成", summary: { terminal: { outcome: { kind: "completed" } } } }, "job"), /完成/);
});

test("a failed conversation links directly to its continuation input only when the Hub allows continuation", () => {
  const local = sharedUiFixture();
  local.projection!.selected_job_id = "job-a";
  local.projection!.detail = {
    id: "job-a", project_id: "project-a", root_id: "job-a", parent_id: null, environment_id: "env-a",
    title: "集計", input: { prompt: "CSVを集計" }, result: { error: "接続できません", text: "途中までの回答" },
    state: "failed", awaiting_child_id: null, revision: 1, created_at_ms: 1, updated_at_ms: 2,
  };
  for (const canContinue of [undefined, false, true]) {
    local.projection!.detail.can_continue = canContinue;
    const html = renderSharedWork(sharedWorkPresentation(local));
    const visible = html.replace(/<details\b[\s\S]*?<\/details>/g, "");
    assert.match(visible, /実行するPCの担当者/);
    assert.match(visible, /途中までの回答/);
    if (canContinue) {
      assert.match(visible, /<a href="#shared-followup">続きの依頼へ<\/a>/);
      assert.match(visible, /<textarea id="shared-followup"/);
    } else {
      assert.doesNotMatch(visible, /href="#shared-followup"|<textarea id="shared-followup"/);
    }
  }
  assert.doesNotMatch(renderWorkResult({ text: "完成" }, "job", true), /href="#shared-followup"/);
});

test("canonical conversation shows readable text while keeping identifiers and payload in closed details", () => {
  const local = sharedUiFixture();
  local.projection!.transcript = { next_after: null, items: [
    { position: 1, kind: "user_turn", payload: { kind: "user_turn", content: [{ kind: "text", text: "**集計**してください" }] } },
    { position: 2, kind: "steer_turn", payload: { kind: "steer_turn", expected_turn_id: "private-turn-id", content: [{ kind: "text", text: "九月に絞ってください" }] } },
    { position: 3, kind: "assistant_message", payload: { kind: "assistant_message", response_id: "private-response-id", content: [{ kind: "text", text: "## 結果\n集計できました" }] } },
  ] };
  const html = renderWorkDetails(sharedWorkPresentation(local));
  const visible = html.replace(/<details\b[\s\S]*?<\/details>/g, "");
  assert.match(visible, /<strong>集計<\/strong>してください/);
  assert.match(visible, /追加の指示/);
  assert.match(visible, /九月に絞ってください/);
  assert.match(visible, /<h[1-6]>結果<\/h[1-6]>/);
  assert.doesNotMatch(visible, /private-turn-id|private-response-id|response_id|expected_turn_id/);
  assert.match(html, /private-response-id/);
  assert.doesNotMatch(html, /<details[^>]*\sopen(?:\s|>)/);
});

test("tool output is readable and commands, unknown records and nontext content remain available without executing markup", () => {
  const local = sharedUiFixture();
  local.projection!.transcript = { next_after: null, items: [
    { position: 1, kind: "tool_call", payload: { kind: "tool_call", tool_name: "shell", arguments_json: '{"command":"private-command"}', call_id: "private-call-id" } },
    { position: 2, kind: "tool_output", payload: { kind: "tool_output", title: "集計を保存", output_text: "完了\n<script>alert(1)</script>", metadata: { secret: "technical-metadata" } } },
    { position: 3, kind: "future_kind", payload: { future_record: "preserved-value" } },
    { position: 4, kind: "user_turn", payload: { kind: "user_turn", content: [{ kind: "image", image: { path: "retained-image" } }, { kind: "future_part", value: "retained-part" }] } },
  ] };
  const html = renderWorkDetails(sharedWorkPresentation(local));
  const visible = html.replace(/<details\b[\s\S]*?<\/details>/g, "");
  assert.match(visible, /コマンドの実行/);
  assert.match(visible, /操作結果: 集計を保存/);
  assert.match(visible, /完了\n&lt;script&gt;/);
  assert.match(visible, /画像を含みます/);
  assert.doesNotMatch(visible, /future_kind/);
  assert.doesNotMatch(visible, /private-command|private-call-id|technical-metadata|preserved-value/);
  for (const value of ["private-command", "private-call-id", "technical-metadata", "future_kind", "preserved-value", "retained-image", "retained-part"]) assert.ok(html.includes(value));
  assert.doesNotMatch(html, /<script>/);
});

test("disclosure keys belong to the authenticated person, project, job and page", () => {
  const local = sharedUiFixture();
  local.projection!.selected_job_id = "job-a";
  local.projection!.transcript = { next_after: null, items: [{ position: 1, kind: "error", payload: { message: "error" } }] };
  const key = () => renderWorkDetails(sharedWorkPresentation(local)).match(/data-details-key="([^"]+)"/)![1];
  const initial = key();
  assert.equal(key(), initial);
  local.projection!.selected_job_id = "job-b"; assert.notEqual(key(), initial);
  local.projection!.selected_job_id = "job-a";
  local.projection!.selected_project_id = "project-b"; assert.notEqual(key(), initial);
  local.projection!.selected_project_id = "project-a";
  local.projection!.generation = "2"; assert.notEqual(key(), initial);
  local.projection!.generation = "1";
  local.projection!.transcript.items[0].position = 41; assert.notEqual(key(), initial);
  local.projection!.transcript.items[0].position = 1;
  local.projection!.principal = null; assert.notEqual(key(), initial);
});

test("routine environment records and artifact checksums are readable on demand without exposing internal names in the main view", () => {
  const local = sharedUiFixture();
  local.projection!.transcript = { next_after: null, items: [{ position: 2, kind: "world_state", payload: { kind: "world_state", summary: "world state updated: 3 sections" } }] };
  const transcript = renderWorkDetails(sharedWorkPresentation(local));
  assert.match(transcript, /<summary>実行環境の確認 · 2<\/summary>/);
  assert.match(transcript, /world state updated/);
  assert.doesNotMatch(transcript.replace(/<details\b[\s\S]*?<\/details>/g, ""), /world_state|world state/);
  local.projection!.assets = [{ id: "result", project_id: "project-a", job_id: "job-a", kind: "artifact", name: "result.md", sha256: "a".repeat(64), byte_length: 20, created_at_ms: 1, version: 1, base_sha256: null, purged_at_ms: null }];
  const html = renderWorkDetails(sharedWorkPresentation(local), "support");
  const visible = html.replace(/<details\b[\s\S]*?<\/details>/g, "");
  assert.match(visible, /result.md/); assert.match(visible, /名前を付けて保存/);
  assert.doesNotMatch(visible, /SHA-256|aaaaaaaa/);
  assert.match(html, /SHA-256: a{64}/);
});

function withoutClosedDetails(html: string): string {
  let depth = 0;
  return html.split(/(<\/?details\b[^>]*>)/).filter(part => {
    if (part.startsWith("<details")) { depth++; return false; }
    if (part.startsWith("</details")) { depth--; return false; }
    return depth === 0;
  }).join("");
}
function workAsset(id: string, version: number, changes: Partial<WorkAsset> = {}): WorkAsset {
  return { id, project_id: "project-a", job_id: "job-a", kind: "artifact", name: "script.ps1", sha256: `${version}`.repeat(64), byte_length: version * 100, created_at_ms: version, version, base_sha256: null, purged_at_ms: null, ...changes };
}

test("the newest artifact is prominent while every older version remains available in closed history", () => {
  const local = sharedUiFixture();
  local.projection!.assets = [
    workAsset("old", 1, { created_at_ms: 999 }), workAsset("readme", 1, { name: "README.md" }),
    workAsset("input-same-name", 99, { kind: "input" }), workAsset("latest", 3), workAsset("middle", 2),
  ];
  const originalIds = local.projection!.assets.map(asset => asset.id);
  const html = renderWorkDetails(sharedWorkPresentation(local), "support"), visible = withoutClosedDetails(html);
  assert.match(visible, /script\.ps1 · 最新版/);
  assert.match(visible, /300 bytes · 版 3/);
  assert.match(visible, /data-action="shared-save-asset" data-value="latest"/);
  assert.match(visible, /data-action="shared-import-asset" data-value="latest"/);
  assert.match(visible, /README\.md/);
  assert.match(visible, /data-action="shared-save-asset" data-value="readme"/);
  assert.match(visible, /入力 · 9,900 bytes · 版 99/);
  assert.match(visible, /data-action="shared-save-asset" data-value="input-same-name"/);
  assert.doesNotMatch(visible, /data-value="(?:old|middle)"/);
  assert.doesNotMatch(html, /data-action="shared-import-asset" data-value="input-same-name"/);
  assert.match(html, /<details data-details-key="[^"]+"><summary>以前の版（2件）<\/summary>/);
  assert.doesNotMatch(html, /<details[^>]*\sopen(?:\s|>)/);
  for (const id of ["latest", "middle", "old"]) {
    for (const action of ["save-asset", "import-asset"]) assert.equal(html.split(`data-action="shared-${action}" data-value="${id}"`).length - 1, 1);
  }
  for (const version of [1, 2, 3]) assert.ok(html.includes(`SHA-256: ${`${version}`.repeat(64)}`));
  assert.ok(html.indexOf('data-value="latest"') < html.indexOf("以前の版（2件）"));
  assert.ok(html.indexOf("以前の版（2件）") < html.indexOf('data-value="middle"'));
  assert.ok(html.indexOf('data-value="middle"') < html.indexOf('data-value="old"'));
  assert.deepEqual(local.projection!.assets.map(asset => asset.id), originalIds, "rendering does not reorder the Hub projection");
});

test("a purged latest artifact stays identified and disabled instead of presenting an older copy as latest", () => {
  const local = sharedUiFixture();
  local.projection!.assets = [workAsset("available-old", 1), workAsset("purged-latest", 2, { purged_at_ms: 100 })];
  let html = renderWorkDetails(sharedWorkPresentation(local), "support"), visible = withoutClosedDetails(html);
  assert.match(visible, /最新版/);
  assert.match(visible, /保持期限により内容は削除済みです/);
  for (const action of ["save-asset", "import-asset"]) {
    assert.match(visible, new RegExp(`data-action="shared-${action}" data-value="purged-latest" disabled`));
    assert.match(html, new RegExp(`data-action="shared-${action}" data-value="available-old" >`));
  }
  local.pending = "save-asset";
  html = renderWorkDetails(sharedWorkPresentation(local), "support");
  assert.match(html, /data-action="shared-save-asset" data-value="available-old" disabled/);
});

test("version history disclosure identity survives a newer version but is isolated by person, project and conversation", () => {
  const local = sharedUiFixture();
  local.projection!.selected_job_id = "job-a";
  local.projection!.assets = [workAsset("old", 1), workAsset("latest", 2)];
  const key = () => renderWorkDetails(sharedWorkPresentation(local), "support").match(/<details data-details-key="([^"]+)"><summary>以前の版/)![1];
  const initial = key();
  local.projection!.assets.push(workAsset("new-latest", 3)); assert.equal(key(), initial);
  local.projection!.selected_job_id = "job-b"; assert.notEqual(key(), initial);
  local.projection!.selected_job_id = "job-a";
  local.projection!.selected_project_id = "project-b"; assert.notEqual(key(), initial);
  local.projection!.selected_project_id = "project-a";
  local.projection!.principal!.user_id = "other-person"; assert.notEqual(key(), initial);
});

test("same-owner polls retain native disclosure and selection while changed owners start closed", () => {
  const region = (owner: string, key: string, isOpen: boolean, selected = false) => {
    const detail = { dataset: { detailsKey: key }, open: isOpen };
    const element = { dataset: { sharedRecordOwner: owner }, querySelectorAll: () => [detail],
      ownerDocument: { activeElement: null, getSelection: () => selected ? { isCollapsed: false, rangeCount: 1, containsNode: () => true } : null },
      contains: () => false, isEqualNode: (other: typeof element) => detail.open === other.querySelectorAll()[0].open };
    return { detail, element: element as unknown as HTMLElement };
  };
  const current = region("person/project/job/page", "item-a", true);
  const next = region("person/project/job/page", "item-a", false);
  assert.equal(retainWorkRecord(current.element, next.element), true);
  assert.equal(next.detail.open, true);
  const other = region("other-person/project/job/page", "item-a", false);
  assert.equal(retainWorkRecord(current.element, other.element), false);
  assert.equal(other.detail.open, false);
  const selected = region("same", "selected", true, true);
  const changed = region("same", "new-item", false);
  assert.equal(retainWorkRecord(selected.element, changed.element), true);
});

test("submission and continuation expose separate deadline drafts and show the accepted deadline", () => {
  const local = sharedUiFixture();
  const deadline = Date.parse("2030-09-14T10:00:00+09:00");
  local.draft.startBefore = "2030-09-14T10:00";
  local.draft.followupStartBefore = "2030-09-15T11:00";
  local.projection!.selected_job_id = "finished";
  local.projection!.detail = { id: "finished", project_id: "project-a", root_id: "finished", parent_id: null,
    environment_id: "env-a", title: "Finished", input: {}, result: "done", state: "succeeded", awaiting_child_id: null,
    revision: 1, created_at_ms: 1, updated_at_ms: 2, can_continue: true, start_before_ms: deadline };
  let html = renderSharedWork(sharedWorkPresentation(local));
  assert.doesNotMatch(html, /data-shared-field="draft:startBefore"/);
  assert.match(html, /data-shared-field="draft:followupStartBefore" type="datetime-local" value="2030-09-15T11:00"/);
  assert.match(html, /開始期限（空欄は投入から24時間）/);
  assert.match(html, /開始済みの処理を打ち切る期限ではありません/);
  assert.ok(html.includes(`開始期限: ${new Date(deadline).toLocaleString("ja-JP")}`));
  local.pending = "submit";
  html = renderSharedWork(sharedWorkPresentation(local));
  assert.doesNotMatch(html, /data-shared-field="draft:startBefore"/);
  assert.match(html, /data-shared-field="draft:followupStartBefore"[^>]* disabled/);
  local.projection!.projects[0].can_submit = false;
  local.projection!.detail.can_continue = false;
  html = renderSharedWork(sharedWorkPresentation(local));
  assert.doesNotMatch(html, /data-shared-field="draft:(?:startBefore|followupStartBefore)"/);
});

test("a job result shows its answer before metadata and preserves unknown data in closed details", () => {
  const local = sharedUiFixture();
  local.projection!.selected_job_id = "finished";
  local.projection!.detail = { id: "finished", project_id: "project-a", root_id: "finished", parent_id: null,
    environment_id: "env-a", title: "Finished", input: {}, result: { version: 1, text: "**解析**できました。", summary: { call_id: "technical-call" } }, state: "succeeded", awaiting_child_id: null,
    revision: 1, created_at_ms: 1, updated_at_ms: 2 };
  const html = renderSharedWork(sharedWorkPresentation(local));
  const visible = html.replace(/<details\b[\s\S]*?<\/details>/g, "");
  assert.match(visible, /<strong>解析<\/strong>できました/);
  assert.doesNotMatch(visible, /technical-call|call_id/);
  assert.match(html, /technical-call/);
  assert.doesNotMatch(html, /<details[^>]*\sopen(?:\s|>)/);
  const firstKey = html.match(/data-details-key="shared-result:([^"]+)"/)![1];
  local.projection!.detail.result = { error: "start_deadline_exceeded", message: "新しい実行の開始期限を過ぎました。" };
  const expired = renderSharedWork(sharedWorkPresentation(local)).replace(/<details\b[\s\S]*?<\/details>/g, "");
  assert.match(expired, /新しい実行の開始期限を過ぎました/); assert.doesNotMatch(expired, /start_deadline_exceeded/);
  local.projection!.detail.result = { unknown_schema: "future-data" };
  const unknown = renderSharedWork(sharedWorkPresentation(local));
  assert.match(unknown, /future-data/);
  assert.doesNotMatch(unknown.replace(/<details\b[\s\S]*?<\/details>/g, ""), /future-data/);
  local.projection!.selected_job_id = "next-job";
  assert.notEqual(renderSharedWork(sharedWorkPresentation(local)).match(/data-details-key="shared-result:([^"]+)"/)![1], firstKey);
});

import assert from "node:assert/strict";
import test from "node:test";
import type { ActionContext } from "../src/actions.ts";
import { refreshSharedWork, sharedWorkAction } from "../src/shared_work_actions.ts";
import { acceptSharedWork, sharedWorkActionEnabled, sharedWorkPresentation } from "../src/shared_work_state.ts";
import { renderSharedWork, retainSharedWorkSurface } from "../src/shared_work_render.ts";
import { sharedProjection, sharedUiFixture } from "./shared_work_fixture.ts";

test("shared entry explains rejected device enrollment using the existing network reason", () => {
  const local = sharedUiFixture();
  local.projection = sharedProjection({ connected: false, principal: null, enrollment: "error", enrollment_error: "device_name_conflict" });
  assert.match(renderSharedWork(sharedWorkPresentation(local)), /同じ端末名が既に登録されています/);
  local.projection = sharedProjection({ revision: "2", principal: null, enrollment: "active", enrollment_error: null });
  assert.doesNotMatch(renderSharedWork(sharedWorkPresentation(local)), /同じ端末名が既に登録されています/);
});

test("a different human generation erases drafts and older snapshots cannot restore them", () => {
  const local = sharedUiFixture(); local.title = "private"; local.prompt = "private prompt"; local.password = "private password";
  acceptSharedWork(local, sharedProjection({ generation: "2", revision: "3", principal: null, projects: [], status: null, selected_project_id: null }));
  assert.equal(local.title + local.prompt + local.password, "");
  assert.equal(acceptSharedWork(local, sharedProjection({ revision: "2" })), false);
  assert.equal(local.projection?.principal, null);
});
test("project role and selected enabled environment govern input while readers retain observation", () => {
  const local = sharedUiFixture(); local.title = "run"; local.prompt = "solve"; local.environmentId = "env-a";
  assert.equal(sharedWorkActionEnabled(sharedWorkPresentation(local), "submit", ""), true);
  local.projection!.projects[0].can_submit = false;
  assert.equal(sharedWorkActionEnabled(sharedWorkPresentation(local), "submit", ""), false);
  assert.equal(sharedWorkActionEnabled(sharedWorkPresentation(local), "detail", "job-a"), true);
  assert.doesNotMatch(renderSharedWork(sharedWorkPresentation(local)), /id="shared-prompt"/);
});
test("continuation requires the Hub capability and sends the selected job revision with its draft", async () => {
  const local = sharedUiFixture();
  local.projection!.selected_job_id = "finished-a";
  local.projection!.detail = { id: "finished-a", project_id: "project-a", root_id: "finished-a", parent_id: null, environment_id: "env-a", title: "Original", input: {}, result: "done", state: "succeeded", awaiting_child_id: null, revision: 7, created_at_ms: 1, updated_at_ms: 2, can_continue: false };
  local.draft.followup = "追加の分析";
  assert.equal(sharedWorkActionEnabled(sharedWorkPresentation(local), "continue", ""), false);
  local.projection!.detail.can_continue = true;
  assert.equal(sharedWorkActionEnabled(sharedWorkPresentation(local), "continue", ""), true);
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  let sent: unknown;
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (_name: string, args: unknown) => { sent = args; return { ...local.projection, revision: "2" }; } } } });
  try {
    const context = { uiState: { sharedWork: local }, getViewState: () => ({ overlay: "shared_work" }), rerender: () => {} } as unknown as ActionContext;
    await sharedWorkAction(context, "continue");
    assert.deepEqual(sent, { expectedGeneration: "1", request: { kind: "continue", project_id: "project-a", job_id: "finished-a", expected_revision: 7, prompt: "追加の分析", start_before_ms: null } });
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});
test("a chosen start deadline crosses the command boundary unchanged and invalid dates do not submit", async () => {
  const local = sharedUiFixture();
  local.title = "Timed work"; local.prompt = "Run when available"; local.environmentId = "env-a";
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  const calls: Record<string, unknown>[] = [];
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (_name: string, args: Record<string, unknown>) => {
    calls.push(args); return { ...local.projection, revision: "2", submission_uncertain: true };
  } } } });
  try {
    const context = { uiState: { sharedWork: local }, getViewState: () => ({ overlay: "shared_work" }), rerender: () => {} } as unknown as ActionContext;
    for (const deadline of ["invalid-date", "2000-01-01T00:00"]) {
      local.draft.startBefore = deadline;
      await sharedWorkAction(context, "submit");
      assert.equal(calls.length, 0);
      assert.equal(local.pending, null);
      assert.match(local.error, /未来の日時/);
    }
    const future = new Date(Date.now() + 86_400_000).toISOString();
    local.draft.startBefore = future;
    await sharedWorkAction(context, "submit");
    assert.equal((calls[0].request as Record<string, unknown>).start_before_ms, new Date(future).getTime());
    assert.equal(local.draft.startBefore, future);
    await sharedWorkAction(context, "retry_submission");
    assert.deepEqual(calls[1].request, { kind: "retry_submission" });
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});
test("new person and new detail clear continuation and handover drafts", () => {
  const local = sharedUiFixture(); local.draft.followup = "private continuation"; local.draft.assigneeId = "private-user";
  acceptSharedWork(local, sharedProjection({ revision: "2", selected_job_id: "other-job" }));
  assert.equal(local.draft.followup, ""); assert.equal(local.draft.assigneeId, "");
  local.draft.followup = "private again";
  acceptSharedWork(local, sharedProjection({ revision: "3", generation: "2", principal: null }));
  assert.equal(Object.values(local.draft).join(""), "");
});
test("file controls accept only projected assets and continued receipts block another submission", () => {
  const local = sharedUiFixture();
  assert.equal(sharedWorkActionEnabled(sharedWorkPresentation(local), "save-asset", "not-owned"), false);
  local.projection!.submission_uncertain = true;
  assert.equal(sharedWorkActionEnabled(sharedWorkPresentation(local), "upload-inputs", ""), false);
  assert.equal(sharedWorkActionEnabled(sharedWorkPresentation(local), "retry-submission", ""), true);
});
test("retained metadata shows deleted content and cannot save or import it", () => {
  const local = sharedUiFixture();
  local.projection!.assets = [{ id: "expired-artifact", project_id: "project-a", job_id: "job-a", kind: "artifact", name: "result.txt", sha256: "a".repeat(64), byte_length: 42, created_at_ms: 1, version: 1, base_sha256: null, purged_at_ms: 2 }];
  const view = sharedWorkPresentation(local);
  assert.equal(sharedWorkActionEnabled(view, "save-asset", "expired-artifact"), false);
  assert.equal(sharedWorkActionEnabled(view, "import-asset", "expired-artifact"), false);
  const html = renderSharedWork(view);
  assert.match(html, /保持期限により内容は削除済みです/);
  assert.match(html, /data-action="shared-save-asset"[^>]*disabled/);
  assert.match(html, /data-action="shared-import-asset"[^>]*disabled/);
});
test("shared surface shows restricted occupancy only as a count and escapes returned work text", () => {
  const local = sharedUiFixture(); local.projection!.status!.jobs[0].title = "<script>alert(1)</script>";
  const html = renderSharedWork(sharedWorkPresentation(local));
  assert.match(html, /他のプロジェクトで 1 件使用中/);
  assert.match(html, /&lt;script&gt;/); assert.doesNotMatch(html, /<script>/);
});
test("uncertain submission has an independent retry control and blocks a new draft submission", () => {
  const local = sharedUiFixture(); local.projection!.submission_uncertain = true;
  local.title = "changed"; local.prompt = "changed"; local.environmentId = "env-a";
  assert.equal(sharedWorkActionEnabled(sharedWorkPresentation(local), "submit", ""), false);
  assert.equal(sharedWorkActionEnabled(sharedWorkPresentation(local), "retry-submission", ""), true);
  assert.match(renderSharedWork(sharedWorkPresentation(local)), /data-action="shared-retry-submission"/);
  local.projection!.submission_storage_error = "保存できません";
  assert.equal(sharedWorkActionEnabled(sharedWorkPresentation(local), "retry-submission", ""), false);
});
test("retaining an active login form synchronizes native and accessible button availability together", () => {
  const attributes = new Map([["aria-disabled", "true"]]);
  const button = { dataset: { action: "shared-login" }, disabled: true, textContent: "ログイン", setAttribute: (name: string, value: string) => attributes.set(name, value) };
  const replacement = { disabled: false, textContent: "ログイン", getAttribute: () => "false" };
  const region = { contains: () => true, querySelectorAll: (selector: string) => selector.startsWith("button") ? [button] : [] };
  const nextRegion = { dataset: { sharedRegion: "login" }, querySelector: () => replacement };
  const current = { dataset: { sharedOwner: "same-person" }, querySelector: () => region };
  const next = { dataset: { sharedOwner: "same-person" }, querySelectorAll: () => [nextRegion] };
  const original = Object.getOwnPropertyDescriptor(globalThis, "document");
  Object.defineProperty(globalThis, "document", { configurable: true, value: { activeElement: { matches: () => true } } });
  try {
    assert.equal(retainSharedWorkSurface(current as unknown as HTMLElement, next as unknown as HTMLElement), true);
    assert.equal(button.disabled, false);
    assert.equal(attributes.get("aria-disabled"), "false");
  } finally { if (original) Object.defineProperty(globalThis, "document", original); else delete (globalThis as Record<string, unknown>).document; }
});
test("logout conceals prior data immediately and discards an already running poll", async () => {
  const local = sharedUiFixture(); let finishPoll!: (value: unknown) => void;
  const waiting = new Promise(resolve => { finishPoll = resolve; });
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (name: string, args: { request?: { kind: string } }) => {
    if (name === "shared_work_projection") return waiting;
    assert.equal(args.request?.kind, "logout");
    assert.equal(local.conceal, true);
    assert.doesNotMatch(renderSharedWork(sharedWorkPresentation(local)), /試験の仕事|利用者 A/);
    return sharedProjection({ generation: "2", revision: "3", principal: null, projects: [], status: null, selected_project_id: null });
  } } } });
  const context = { uiState: { sharedWork: local }, getViewState: () => ({ overlay: "shared_work" }), rerender() {} } as unknown as ActionContext;
  try {
    const poll = refreshSharedWork(context);
    await sharedWorkAction(context, "logout");
    finishPoll(sharedProjection({ revision: "2" })); await poll;
    assert.equal(local.projection?.principal, null);
    assert.equal(local.conceal, false);
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});

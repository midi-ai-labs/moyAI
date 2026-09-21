import assert from "node:assert/strict";
import test from "node:test";
import { renderSharedWorkOnboarding, SAMPLE_PROMPT } from "../src/shared_work_onboarding.ts";
import { sharedWorkActionEnabled } from "../src/shared_work_state.ts";
import { sharedWorkAction } from "../src/shared_work_actions.ts";
import { sharedUiFixture, sharedProjection } from "./shared_work_fixture.ts";
import type { ActionContext } from "../src/actions.ts";

test("controller onboarding needs device approval and project assignment without another login", () => {
  const local = sharedUiFixture();
  local.projection = sharedProjection({ connected: false, enrollment: "pending", principal: null, projects: [], selected_project_id: null, status: null });
  const html = renderSharedWorkOnboarding(local);
  assert.match(html, /管理者の承認待ち/);
  assert.doesNotMatch(html, /本人ログイン|パスワード/);
  assert.match(html, /このPCにAIや実行用フォルダーを設定する必要はありません/);
  assert.doesNotMatch(html, /data-action="shared-prepare-sample"/);
});

test("reader is ready to observe without submitting a test job; sample never overwrites existing work", () => {
  const local = sharedUiFixture();
  assert.equal(sharedWorkActionEnabled(local, "prepare-sample", ""), true);
  local.prompt = "既存の依頼";
  assert.equal(sharedWorkActionEnabled(local, "prepare-sample", ""), false);
  local.prompt = "";
  local.projection!.projects[0].can_submit = false;
  assert.equal(sharedWorkActionEnabled(local, "prepare-sample", ""), false);
  assert.match(renderSharedWorkOnboarding(local), /このプロジェクトの会話と結果を閲覧できます/);
  assert.doesNotMatch(renderSharedWorkOnboarding(local), /data-action="shared-prepare-sample"/);
});

test("sample preparation uploads through the selected project but never submits automatically", async () => {
  const local = sharedUiFixture(), calls: unknown[] = [];
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (_name: string, args: unknown) => {
    calls.push(args);
    return sharedProjection({ revision: "2", inputs: [{ id: "sample", project_id: "project-a", job_id: null, kind: "input", name: "moyai-sample-numbers.csv", sha256: "a".repeat(64), byte_length: 15, created_at_ms: 1, version: 1, base_sha256: null, purged_at_ms: null }] });
  } } } });
  try {
    const context = { uiState: { sharedWork: local }, getViewState: () => ({ overlay: "none", hub_project_open: true }), rerender() {} } as unknown as ActionContext;
    await sharedWorkAction(context, "prepare_sample");
    assert.deepEqual(calls, [{ expectedGeneration: "1", request: { kind: "prepare_sample", project_id: "project-a" } }]);
    assert.equal(local.prompt, SAMPLE_PROMPT);
    await sharedWorkAction(context, "prepare_sample");
    assert.equal(calls.length, 1);
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});

import assert from "node:assert/strict";
import test from "node:test";
import { refreshSharedWork } from "../src/shared_work_actions.ts";
import { sharedWorkActionEnabled } from "../src/shared_work_state.ts";
import { renderHubConversation } from "../src/shared_work_conversation.ts";
import { sharedProjection, sharedUiFixture } from "./shared_work_fixture.ts";
import type { ActionContext } from "../src/actions.ts";

test("an approved device refreshes projects automatically without collecting credentials", async () => {
  const local = sharedUiFixture(), sent: unknown[] = [];
  local.projection = sharedProjection({ principal: null, projects: [], selected_project_id: null, status: null });
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (name: string, args: unknown) => {
    if (name === "shared_work_projection") return local.projection;
    sent.push(args); return sharedProjection({ revision: "2" });
  } } } });
  try {
    const html = renderHubConversation(local, "");
    assert.match(html, /このPCの利用登録を確認/);
    assert.doesNotMatch(html, /shared-(?:username|password|setup-code|login|logout)|type="password"/);
    const context = { uiState: { sharedWork: local }, rerender() {} } as unknown as ActionContext;
    await refreshSharedWork(context);
    assert.deepEqual(sent, [{ expectedGeneration: "1", request: { kind: "refresh" } }]);
    assert.equal(local.projection?.projects[0].id, "project-a");
  } finally {
    if (original) Object.defineProperty(globalThis, "window", original); else Reflect.deleteProperty(globalThis, "window");
  }
});

test("legacy and current empty-project snapshots both explain PC assignment", () => {
  const local = sharedUiFixture();
  for (const access of ["no_membership", "device_not_allowed", undefined] as const) {
    local.projection = sharedProjection({ projects: [], selected_project_id: null, status: null, project_access: access });
    const html = renderHubConversation(local, "");
    assert.match(html, /このPCをプロジェクトの操作PCへ追加/);
    assert.doesNotMatch(html, /ログイン|パスワード|本人設定コード/);
  }
});

test("pending enrollment and rejected or old Hub responses retain the next action", () => {
  const local = sharedUiFixture();
  local.projection = sharedProjection({ connected: false, enrollment: "pending", principal: null, projects: [], selected_project_id: null, status: null });
  assert.match(renderHubConversation(local, ""), /管理者が承認すると自動で接続/);
  assert.equal(sharedWorkActionEnabled(local, "submit", ""), false);
  local.projection = sharedProjection({ principal: null, projects: [], selected_project_id: null, status: null, error: "Hubを更新してください。" });
  assert.match(renderHubConversation(local, ""), /Hubを更新してください/);
  assert.equal(sharedWorkActionEnabled(local, "refresh", ""), true);
});

test("management opening is available only for a projected administrator without exposing a ticket", () => {
  const local = sharedUiFixture();
  assert.equal(sharedWorkActionEnabled(local, "open-management", ""), false);
  assert.doesNotMatch(renderHubConversation(local, ""), /data-action="shared-open-management"/);
  local.projection!.principal!.administrator = true;
  assert.equal(sharedWorkActionEnabled(local, "open-management", ""), true);
  assert.match(renderHubConversation(local, ""), /data-action="shared-open-management"/);
  assert.doesNotMatch(renderHubConversation(local, ""), /#access=/);
});

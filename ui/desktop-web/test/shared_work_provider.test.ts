import assert from "node:assert/strict";
import test from "node:test";
import { renderWorkProvider } from "../src/shared_work_details.ts";
import { acceptSharedWork, editSharedWork, providerDraftMatches, sharedWorkActionEnabled } from "../src/shared_work_state.ts";
import { sharedWorkAction } from "../src/shared_work_actions.ts";
import type { ActionContext } from "../src/actions.ts";
import { sharedUiFixture } from "./shared_work_fixture.ts";

test("a new work-folder preset needs a name without an operator inventing an internal ID", () => {
  const local = sharedUiFixture();
  local.draft.templateLabel = "解析チームの作業フォルダ";
  const html = renderWorkProvider(local);
  const prepare = html.match(/<button[^>]*data-action="shared-provider-prepare"[^>]*>/)?.[0] ?? "";
  assert.ok(prepare);
  assert.doesNotMatch(prepare, /disabled/);
  assert.match(html, /<option value="default" selected>/);
  assert.match(html, /<option value="device" selected>/);
});

function preparedProvider() {
  const local = sharedUiFixture();
  const template = { id: "team-analysis", label: "解析チーム", base_root: "C:/Work", access_mode: "default", allowed_child_environments: ["solver"] };
  local.projection!.provider = { runner_id: "runner-1", mode: "shared", state: "available", accepting: true, maintenance_until_ms: null,
    autostart: false, templates: [template], environments: [], active_attempts: [], unknown_attempts: [], error: null };
  local.projection!.provider_draft = template;
  editSharedWork(local, "draft:editTemplateId", template.id);
  return local;
}

test("changed preset fields cannot publish an earlier native-folder preview", () => {
  const local = preparedProvider();
  assert.equal(sharedWorkActionEnabled(local, "provider-install", ""), true);
  local.draft.templateLabel = "変更後の名前";
  assert.equal(sharedWorkActionEnabled(local, "provider-install", ""), false);
  assert.match(renderWorkProvider(local), /保存先を選び直して/);
});

test("editing a projected preset reuses its values and choosing new does not overwrite it", () => {
  const local = preparedProvider();
  assert.equal(local.draft.templateId, "team-analysis");
  assert.equal(local.draft.templateLabel, "解析チーム");
  assert.equal(local.draft.children, "solver");
  assert.equal(providerDraftMatches(local), true);
  const before = { ...local.draft };
  editSharedWork(local, "draft:editTemplateId", "not-projected");
  assert.deepEqual(local.draft, before);
  editSharedWork(local, "draft:editTemplateId", "");
  assert.equal(local.draft.templateId, "");
  assert.equal(local.draft.children, "");
  assert.equal(providerDraftMatches(local), false);
  assert.equal(local.projection!.provider!.templates[0].id, "team-analysis");
});

test("generated preset IDs stay with the same draft and safe visible defaults cross the boundary", async () => {
  const local = sharedUiFixture(); local.draft.templateLabel = "共有フォルダ";
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  const requests: Record<string, unknown>[] = [];
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (_name: string, args: { request: Record<string, unknown> }) => {
    requests.push(args.request); return { ...local.projection, revision: String(requests.length + 1) };
  } } } });
  try {
    const context = { uiState: { sharedWork: local }, getViewState: () => ({ overlay: "shared_work" }), rerender() {} } as unknown as ActionContext;
    await sharedWorkAction(context, "provider_prepare");
    await sharedWorkAction(context, "provider_prepare");
    assert.match(String(requests[0].id), /^folder-[0-9a-f-]{36}$/);
    assert.equal(requests[0].id, requests[1].id);
    assert.deepEqual(requests[0], { kind: "provider_prepare", id: local.draft.templateId, label: "共有フォルダ", access_mode: "default", allowed_child_environments: [], resource_scope: { kind: "device" } });
    assert.equal(local.projection!.provider_draft, null);
    acceptSharedWork(local, { ...local.projection!, generation: "2", revision: "4", principal: null });
    assert.equal(local.draft.templateId, undefined);
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});

test("preset disclosure defaults leave maintenance and delegation available without changing grants", () => {
  const local = preparedProvider();
  const html = renderWorkProvider(local);
  assert.match(html, /data-details-key="shared-provider-options:[^"]+"><summary>/);
  assert.match(html, /data-details-key="shared-provider-operations:[^"]+"><summary>/);
  assert.match(html, /data-action="shared-provider-no-autostart"|data-action="shared-provider-autostart"/);
  assert.match(html, /data-shared-field="draft:children"/);
  assert.match(html, /Hub管理者が「利用者と仕事の設定」で実行環境を追加/);
});

test("a failed Runner status read keeps an explicit start route alongside the old projection", () => {
  const local = preparedProvider();
  local.projection!.provider_error = "Runnerに接続できません";
  const html = renderWorkProvider(local);
  assert.match(html, /data-action="shared-provider-start"/);
  assert.match(html, /Runnerに接続できません/);
});

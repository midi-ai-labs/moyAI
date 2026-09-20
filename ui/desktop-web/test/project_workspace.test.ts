import assert from "node:assert/strict";
import test from "node:test";
import { acceptSharedWork, sharedWorkActionEnabled, sharedWorkPresentation } from "../src/shared_work_state.ts";
import { renderSharedWork } from "../src/shared_work_render.ts";
import { renderSidebar } from "../src/render.ts";
import { modalIsOpen } from "../src/modal_state.ts";
import { openHubProject, refreshSharedWork, sharedWorkAction } from "../src/shared_work_actions.ts";
import type { ActionContext } from "../src/actions.ts";
import type { DesktopWebState } from "../src/types.ts";
import { sharedProjection, sharedUiFixture } from "./shared_work_fixture.ts";

test("a single execution PC is selected automatically and a message needs no separately entered title", () => {
  const local = sharedUiFixture();
  acceptSharedWork(local, sharedProjection({ revision: "2" }));
  local.prompt = "解析してください";
  assert.equal(local.environmentId, "env-a");
  assert.equal(local.title, "");
  assert.equal(sharedWorkActionEnabled(local, "submit", ""), true);
  const html = renderSharedWork(local);
  assert.doesNotMatch(html, /id="shared-environment"|role="dialog"|aria-modal|shared-provider-/);
  assert.match(html, /実行するPC: <strong>解析用 PC/);
  assert.equal(modalIsOpen({ overlay: "none", confirmation_visible: false }, false), false);
});
test("an assigned PC awaiting first setup is visible without becoming a submit target", () => {
  const local = sharedUiFixture(), p = sharedProjection();
  p.status!.environments = [];
  p.status!.execution_devices = [{ device_id: "pc-b", device_label: "手動PC B", environment_id: null, preparation_state: "waiting_setup", error: null }];
  acceptSharedWork(local, p); local.prompt = "解析してください";
  const html = renderSharedWork(local);
  assert.match(html, /手動PC B/);
  assert.match(html, /このPCの初回設定待ち/);
  assert.doesNotMatch(html, /管理者が実行するPCを割り当てると/);
  assert.equal(local.environmentId, "");
  assert.equal(sharedWorkActionEnabled(local, "submit", ""), false);
});
test("execution PC labels come from the device while environment and project labels stay distinct", () => {
  const local = sharedUiFixture(), p = sharedProjection();
  p.status!.environments[0].label = "解析 A";
  p.status!.environments[0].device_label = "手動PC B";
  p.status!.execution_devices = [{ device_id: "runner-a", device_label: "手動PC B", environment_id: "env-a", preparation_state: "ready", error: null }];
  acceptSharedWork(local, p);
  const html = renderSharedWork(local);
  assert.match(html, /実行するPC: <strong>手動PC B<\/strong>/);
  assert.match(html, /<h3>手動PC B<\/h3>/);
  assert.doesNotMatch(html, /<h3>解析 A<\/h3>/);
});
test("preparation state is distinct from assignment and ready capacity, including errors on another page", () => {
  const local = sharedUiFixture(), p = sharedProjection();
  const environment = p.status!.environments[0];
  environment.enabled = false;
  environment.device_label = "PC B";
  p.status!.execution_devices = [
    { device_id: "runner-a", device_label: "PC B", environment_id: "env-a", preparation_state: "pending", error: null },
    { device_id: "runner-c", device_label: "<PC C>", environment_id: "other-page", preparation_state: "failed", error: "<保存先の確認が必要>" },
  ];
  acceptSharedWork(local, p); local.prompt = "解析";
  const html = renderSharedWork(local);
  assert.match(html, /作業フォルダーを作成中/);
  assert.match(html, /作業フォルダーを作成できませんでした/);
  assert.match(html, /&lt;PC C&gt;/);
  assert.match(html, /&lt;保存先の確認が必要&gt;/);
  assert.equal((html.match(/<h3>PC B<\/h3>/g) ?? []).length, 1);
  assert.equal(sharedWorkActionEnabled(local, "submit", ""), false);
});
test("an unassigned project keeps assignment guidance and PC details disappear on logout", () => {
  const local = sharedUiFixture(), p = sharedProjection();
  p.status!.environments = []; p.status!.execution_devices = [];
  acceptSharedWork(local, p);
  assert.match(renderSharedWork(local), /管理者が実行するPCを割り当てると/);
  p.status!.execution_devices.push({ device_id: "pc-secret", device_label: "Secret PC", environment_id: null, preparation_state: "waiting_setup", error: null });
  local.conceal = true;
  assert.doesNotMatch(renderSharedWork(local), /Secret PC|pc-secret/);
});
test("an empty environment page still identifies assigned ready PCs without inventing capacity", () => {
  const local = sharedUiFixture(), p = sharedProjection();
  p.status!.environments = [];
  p.status!.next_environment_before = null;
  p.status!.execution_devices = [{ device_id: "runner-b", device_label: "PC B", environment_id: "previous-page", preparation_state: "ready", error: null }];
  acceptSharedWork(local, p);
  const html = renderSharedWork(local);
  assert.match(html, /<h3>PC B<\/h3>/);
  assert.match(html, /作業フォルダー作成済み（AIの動作は未確認）/);
  assert.match(html, /利用状況は別のページに表示されています。/);
  assert.doesNotMatch(html, /管理者が実行するPCを割り当てると|枠を使用中/);
  assert.equal(local.environmentId, "");
  assert.equal(sharedWorkActionEnabled(local, "submit", ""), false);
});
test("multiple PCs require a choice and a revoked choice never silently reroutes work", () => {
  const local = sharedUiFixture(), p = sharedProjection();
  p.status!.environments.push({ ...p.status!.environments[0], id: "env-b", label: "PC B" });
  acceptSharedWork(local, p); local.prompt = "solve";
  assert.equal(local.environmentId, "");
  assert.equal(sharedWorkActionEnabled(local, "submit", ""), false);
  local.environmentId = "env-b";
  acceptSharedWork(local, { ...p, revision: "2" });
  assert.equal(local.environmentId, "env-b");
  p.status!.environments[1].enabled = false;
  acceptSharedWork(local, { ...p, revision: "3" });
  assert.equal(local.environmentId, "env-b");
  assert.equal(sharedWorkActionEnabled(local, "submit", ""), false);
  assert.match(renderSharedWork(local), /id="shared-environment"/);
});
test("Hub projects and chats occupy the normal sidebar and logout removes them", () => {
  const local = sharedUiFixture();
  local.projection!.status!.jobs[0].title = "<private-job>";
  const state = { overlay: "none", hub_project_open: true, navigation_admission_open: true, project_rows: [], chat_session_rows: [] } as unknown as DesktopWebState;
  const html = renderSidebar(state, local);
  assert.match(html, /data-action="open-hub-project" data-value="project-a"/);
  assert.match(html, /data-action="shared-detail" data-value="job-a"/);
  assert.match(html, /&lt;private-job&gt;/);
  assert.doesNotMatch(html, /<private-job>|<span>共有仕事<\/span>|<span>過去の連携履歴<\/span>/);
  local.conceal = true;
  assert.doesNotMatch(renderSidebar(state, local), /project-a|job-a|private-job/);
});
test("background refresh populates projects without opening a separate mode", async () => {
  const local = sharedUiFixture(), calls: string[] = [];
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (name: string) => { calls.push(name); return sharedProjection({ revision: "2" }); } } } });
  try {
    const context = { uiState: { sharedWork: local }, getViewState: () => ({ overlay: "none", hub_project_open: false }), rerender() {} } as unknown as ActionContext;
    await refreshSharedWork(context);
    assert.deepEqual(calls, ["shared_work_projection", "shared_work_command"]);
    assert.equal(local.projection?.projects[0].id, "project-a");
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});
test("a sidebar project selection uses the projected project ID and new chat clears the exact shared selection", async () => {
  const local = sharedUiFixture(), calls: unknown[] = [];
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (_name: string, args: unknown) => { calls.push(args); return sharedProjection({ revision: String(calls.length + 1) }); } } } });
  try {
    const context = { uiState: { sharedWork: local }, getViewState: () => ({ overlay: "none", hub_project_open: true }), mutate: async () => {}, rerender() {} } as unknown as ActionContext;
    await openHubProject(context, "not-a-project"); assert.equal(calls.length, 0);
    await openHubProject(context, "project-a");
    await sharedWorkAction(context, "new_conversation");
    assert.deepEqual(calls, [
      { expectedGeneration: "1", request: { kind: "project", project_id: "project-a" } },
      { expectedGeneration: "1", request: { kind: "new_conversation", project_id: "project-a" } },
    ]);
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});

test("new chat clears an unsent draft even when no previous job was selected", async () => {
  const local = sharedUiFixture();
  local.projection!.selected_job_id = null; local.projection!.detail = null;
  local.prompt = "以前の下書き"; local.title = "以前の名前"; local.draft.startBefore = "2030-01-01T12:00";
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async () => ({ ...local.projection, revision: "2" }) } } });
  try {
    const context = { uiState: { sharedWork: local }, getViewState: () => ({ overlay: "none", hub_project_open: true }), rerender() {} } as unknown as ActionContext;
    await sharedWorkAction(context, "new_conversation");
    assert.equal(local.prompt, ""); assert.equal(local.title, ""); assert.equal(local.draft.startBefore, "");
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});

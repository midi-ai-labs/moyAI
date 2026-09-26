import assert from "node:assert/strict";
import test from "node:test";
import { acceptSharedWork, sharedConversationRows, sharedWorkActionEnabled, sharedWorkPresentation } from "../src/shared_work_state.ts";
import { renderSharedWork } from "../src/shared_work_render.ts";
import { renderSidebar } from "../src/render.ts";
import { modalIsOpen } from "../src/modal_state.ts";
import { openHubProject, refreshSharedWork, sharedWorkAction } from "../src/shared_work_actions.ts";
import type { ActionContext } from "../src/actions.ts";
import type { DesktopWebState } from "../src/types.ts";
import { sharedProjection, sharedUiFixture } from "./shared_work_fixture.ts";

test("a Hub project starts from the ordinary message field and leaves initial PC placement to Hub", () => {
  const local = sharedUiFixture();
  acceptSharedWork(local, sharedProjection({ revision: "2" }));
  local.prompt = "解析してください";
  assert.equal(local.title, "");
  assert.equal(sharedWorkActionEnabled(local, "submit", ""), true);
  const html = renderSharedWork(local);
  assert.match(html, /class="conversation shared-conversation"/);
  assert.match(html, /class="thread shared-thread"/);
  assert.match(html, /class="composer shared-composer"/);
  assert.doesNotMatch(html, /id="shared-environment"|最初に使うPC|shared-title|hub-new-chat-options/);
  assert.doesNotMatch(html, /role="dialog"|aria-modal|shared-provider-/);
  assert.equal(modalIsOpen({ overlay: "none", confirmation_visible: false }, false), false);
});
test("an assigned PC awaiting first setup is visible without becoming a submit target", () => {
  const local = sharedUiFixture(), p = sharedProjection();
  p.status!.environments = [];
  p.status!.next_environment_before = null;
  p.status!.execution_devices = [{ device_id: "pc-b", device_label: "手動PC B", environment_id: null, preparation_state: "waiting_setup", error: null }];
  acceptSharedWork(local, p); local.prompt = "解析してください";
  const html = renderSharedWork(local);
  assert.match(html, /手動PC B/);
  assert.match(html, /このPCの初回設定待ち/);
  assert.doesNotMatch(html, /管理者が実行するPCを割り当てると/);
  assert.equal(sharedWorkActionEnabled(local, "submit", ""), false);
});
test("execution PC labels come from the device while environment and project labels stay distinct", () => {
  const local = sharedUiFixture(), p = sharedProjection();
  p.status!.environments[0].label = "解析 A";
  p.status!.environments[0].device_label = "手動PC B";
  p.status!.execution_devices = [{ device_id: "runner-a", device_label: "手動PC B", environment_id: "env-a", preparation_state: "ready", error: null }];
  acceptSharedWork(local, p);
  const html = renderSharedWork(local);
  assert.doesNotMatch(html, /id="shared-environment"|最初に使うPC/);
  assert.match(html, /<h3>手動PC B<\/h3>/);
  assert.doesNotMatch(html, /<h3>解析 A<\/h3>/);
});
test("preparation state is distinct from assignment and ready capacity, including errors on another page", () => {
  const local = sharedUiFixture(), p = sharedProjection();
  const environment = p.status!.environments[0];
  environment.enabled = false;
  p.status!.next_environment_before = null;
  environment.device_label = "PC B";
  p.status!.execution_devices = [
    { device_id: "runner-a", device_label: "PC B", environment_id: "env-a", preparation_state: "pending", error: null },
    { device_id: "runner-c", device_label: "<PC C>", environment_id: "other-page", preparation_state: "failed", error: "<保存先の確認が必要>" },
  ];
  acceptSharedWork(local, p); local.prompt = "解析";
  const html = renderSharedWork(local);
  assert.match(html, /作業フォルダーの登録待ち/);
  assert.match(html, /作業フォルダーを登録できませんでした/);
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
  assert.match(html, /作業フォルダー登録済み（AIの動作は未確認）/);
  assert.match(html, /利用状況は別のページに表示されています。/);
  assert.doesNotMatch(html, /管理者が実行するPCを割り当てると|枠を使用中/);
  assert.equal(sharedWorkActionEnabled(local, "submit", ""), false);
});
test("multiple authorized PCs are eligible without a person choosing one in the composer", () => {
  const local = sharedUiFixture(), p = sharedProjection();
  p.status!.environments.push({ ...p.status!.environments[0], id: "env-b", label: "PC B" });
  acceptSharedWork(local, p); local.prompt = "solve";
  assert.equal(sharedWorkActionEnabled(local, "submit", ""), true);
  p.status!.environments[1].enabled = false;
  acceptSharedWork(local, { ...p, revision: "2" });
  assert.equal(sharedWorkActionEnabled(local, "submit", ""), true);
  p.status!.environments[0].enabled = false;
  p.status!.next_environment_before = null;
  acceptSharedWork(local, { ...p, revision: "3" });
  assert.equal(sharedWorkActionEnabled(local, "submit", ""), false);
  assert.doesNotMatch(renderSharedWork(local), /id="shared-environment"/);
});
test("automatic initial placement sends no stale PC choice", async () => {
  const local = sharedUiFixture();
  local.prompt = "WinB で試験してください";
  let sent: unknown;
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (name: string, args: unknown) => {
    assert.equal(name, "shared_work_command"); sent = args;
    return { ...local.projection, revision: "2" };
  } } } });
  try {
    const context = { uiState: { sharedWork: local }, getViewState: () => ({ overlay: "none", hub_project_open: true }), rerender() {} } as unknown as ActionContext;
    await sharedWorkAction(context, "submit");
    assert.equal((sent as {request: Record<string, unknown>}).request.environment_id, undefined);
    assert.equal((sent as {request: Record<string, unknown>}).request.prompt, "WinB で試験してください");
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});

test("a retained app stays visible in the same conversation and its stop targets the exact handle", async () => {
  const local = sharedUiFixture();
  local.projection!.selected_job_id = "job-a";
  local.projection!.detail = { id: "job-a", project_id: "project-a", root_id: "job-a", parent_id: null, environment_id: "env-a", title: "試験の仕事", input: { prompt: "アプリを起動" }, result: null, state: "succeeded", awaiting_child_id: null, revision: 1, created_at_ms: 1, updated_at_ms: 2,
    retained_services: [{ service_id: "service-a", environment_id: "env-a", expires_at_ms: Date.now() + 60_000, stop_requested: false, uncertain: false, can_stop: true }] };
  const html = renderSharedWork(local);
  assert.match(html, /この会話で起動中のアプリ/);
  assert.match(html, /data-action="shared-stop-service" data-value="service-a"/);
  assert.equal(sharedWorkActionEnabled(local, "stop-service", "service-a"), true);
  let sent: unknown;
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (name: string, args: unknown) => {
    assert.equal(name, "shared_work_command"); sent = args;
    return { ...local.projection, revision: "2" };
  } } } });
  try {
    const context = { uiState: { sharedWork: local }, getViewState: () => ({ overlay: "none", hub_project_open: true }), rerender() {} } as unknown as ActionContext;
    await sharedWorkAction(context, "stop_service", "service-a");
    assert.deepEqual(sent, { expectedGeneration: "1", request: { kind: "stop_service", project_id: "project-a", service_id: "service-a" } });
    local.projection!.detail!.retained_services![0].can_stop = false;
    assert.doesNotMatch(renderSharedWork(local), /data-action="shared-stop-service"/);
    assert.equal(sharedWorkActionEnabled(local, "stop-service", "service-a"), false);
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});
test("Hub projects and chats occupy the normal sidebar and logout removes them", () => {
  const local = sharedUiFixture();
  local.projection!.status!.jobs[0].title = "<private-job>";
  const state = { overlay: "none", hub_project_open: true, navigation_admission_open: true, project_rows: [], chat_session_rows: [] } as unknown as DesktopWebState;
  const html = renderSidebar(state, local);
  assert.match(html, /data-action="open-hub-project" data-value="project-a"/);
  assert.match(html, /data-action="shared-detail" data-value="job-a"/);
  assert.match(html, /class="project-source-badge">MCP<\/small>/);
  assert.match(html, /&lt;private-job&gt;/);
  assert.doesNotMatch(html, /<private-job>|<span>共有仕事<\/span>|<span>過去の連携履歴<\/span>/);
  local.conceal = true;
  assert.doesNotMatch(renderSidebar(state, local), /project-a|job-a|private-job/);
});
test("children and continuations keep one Hub conversation while a private-origin job stays in its local chat", () => {
  const first = sharedProjection().status!.jobs[0];
  const jobs = [
    { ...first, id: "root", conversation_id: "root", parent_id: null, root_id: "root", title: "TODO アプリ", created_at_ms: 1 },
    { ...first, id: "child", conversation_id: "root", parent_id: "root", root_id: "root", title: "WinB の作業", created_at_ms: 2 },
    { ...first, id: "continuation", conversation_id: "root", parent_id: null, root_id: "root", title: "変更を確認", created_at_ms: 3 },
    { ...first, id: "private-job", conversation_id: "private-job", origin_session_ref: "local-session", parent_id: null, root_id: "private-job", created_at_ms: 4 },
  ];
  assert.deepEqual(sharedConversationRows(jobs, "continuation"), [{ conversationId: "root", title: "TODO アプリ", jobId: "continuation", selected: true }]);
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

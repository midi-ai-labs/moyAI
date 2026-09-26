import assert from "node:assert/strict";
import test from "node:test";
import type { ActionContext } from "../src/actions.ts";
import { bindProjectFolder, confirmExecutionProjectLeave, renderDeviceExecution, requestExecutionProjectLeave, type DeviceExecutionProjection } from "../src/device_execution.ts";
import { createDeviceNetworkUiState } from "../src/device_network_state.ts";
import { renderSidebar } from "../src/render.ts";
import { renderSharedWork } from "../src/shared_work_render.ts";
import { confirmSharedConfirmation, requestSharedConfirmation, sharedWorkAction } from "../src/shared_work_actions.ts";
import { sharedWorkActionEnabled, sharedWorkPresentation } from "../src/shared_work_state.ts";
import type { DesktopWebState } from "../src/types.ts";
import { sharedProjection, sharedUiFixture } from "./shared_work_fixture.ts";

const conversation = { id: "conversation-a", title: "TODO アプリ", latest_job_id: "job-a", updated_at_ms: 4,
  revision: 2, delete_pending: false, can_rename: true, can_delete: true };

test("one project list shows Hub projects by ID with an MCP badge and authoritative shared chats", () => {
  const local = sharedUiFixture();
  local.projection = sharedProjection({ conversations: [conversation], selected_conversation_id: conversation.id });
  const state = { overlay: "none", hub_project_open: true, navigation_admission_open: true,
    project_rows: [{ project_id: "local-clock", label: "デスクトップ時計アプリの作成", path: "C:\\clock" }],
    chat_session_rows: [] } as unknown as DesktopWebState;
  const html = renderSidebar(state, sharedWorkPresentation(local));
  assert.match(html, /data-action="open-hub-project" data-value="project-a"/);
  assert.match(html, /class="project-source-badge">MCP<\/small>/);
  assert.match(html, /デスクトップ時計アプリの作成/);
  assert.match(html, /data-action="shared-select-conversation" data-value="conversation-a"/);
  assert.match(html, /data-action="shared-start-rename-conversation" data-value="conversation-a"/);
  assert.match(html, /data-action="shared-request-delete-conversation" data-value="conversation-a"/);
});

test("a temporarily unavailable Hub retains its last confirmed projects as read-only, unupdated rows", () => {
  const local = sharedUiFixture();
  local.projection = sharedProjection({ principal: null, projects_stale: true, conversations: [conversation] });
  const state = { overlay: "none", hub_project_open: false, navigation_admission_open: true,
    project_rows: [], chat_session_rows: [] } as unknown as DesktopWebState;
  const html = renderSidebar(state, sharedWorkPresentation(local));
  assert.match(html, /project-source-badge">MCP · 未更新/);
  assert.match(html, /解析 A/);
  assert.doesNotMatch(html, /data-action="open-hub-project"/);
  assert.doesNotMatch(html, /data-action="shared-request-leave-project"/);
  assert.equal(sharedWorkActionEnabled(sharedWorkPresentation(local), "submit", ""), false);
});

test("shared chat rename, delete and local departure send exact Hub identities", async () => {
  const local = sharedUiFixture();
  local.projection = sharedProjection({ conversations: [conversation], selected_conversation_id: conversation.id });
  const sent: Array<Record<string, unknown>> = [];
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (_name: string, args: {request: Record<string, unknown>}) => {
    sent.push(args.request);
    return { ...local.projection, revision: String(sent.length + 1) };
  } } } });
  const context = { uiState: { sharedWork: local }, getViewState: () => ({ overlay: "none", hub_project_open: true }), rerender() {} } as unknown as ActionContext;
  try {
    await sharedWorkAction(context, "select_conversation", conversation.id);
    local.editingConversationId = conversation.id; local.renameDraft = "TODOアプリ改訂";
    await sharedWorkAction(context, "rename_conversation", conversation.id);
    requestSharedConfirmation(context, "delete_conversation", conversation.id);
    assert.equal(local.confirmation?.title, "TODO アプリ");
    await confirmSharedConfirmation(context);
    requestSharedConfirmation(context, "leave_project", "project-a");
    assert.equal(local.confirmation?.kind, "leave_project");
    await confirmSharedConfirmation(context);
    assert.deepEqual(sent, [
      { kind: "select_conversation", project_id: "project-a", conversation_id: "conversation-a" },
      { kind: "rename_conversation", project_id: "project-a", conversation_id: "conversation-a", title: "TODOアプリ改訂" },
      { kind: "delete_conversation", project_id: "project-a", conversation_id: "conversation-a" },
      { kind: "leave_project", project_id: "project-a" },
    ]);
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});

test("an execution device chooses its existing project folder once and carries the previous path for a safe change", async () => {
  const device = createDeviceNetworkUiState();
  const execution: DeviceExecutionProjection = { revision: "3", state: "ready", projects: [{ id: "project-a", label: "TODO アプリ", can_control: true,
    can_execute: true, environment_id: "environment-a", preparation_state: "ready", error: null, directory: "C:\\old", access_mode: "default" }],
    review: null, directory: "C:\\base", access_mode: "default", accepting: true, can_pause: true, can_resume: false, error: null, unknown_attempts: [] };
  device.execution = execution;
  assert.match(renderDeviceExecution(device), /data-action="bind-project-folder" data-value="project-a"/);
  const calls: Array<{name: string; args: unknown}> = [];
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (name: string, args: unknown) => {
    calls.push({ name, args });
    if (name === "browse_shared_project_folder") return "C:\\new";
    if (name === "device_execution_projection") return execution;
    if (name === "shared_work_projection") return sharedProjection();
    return sharedProjection({ revision: "4" });
  } } } });
  try {
    const context = { uiState: { deviceNetwork: device }, getViewState: () => ({ overlay: "hub" }), rerender() {} } as unknown as ActionContext;
    await bindProjectFolder(context, "project-a");
    assert.deepEqual(calls.find(call => call.name === "shared_work_command")?.args, { expectedGeneration: "1", request: {
      kind: "bind_project_folder", project_id: "project-a", environment_id: "environment-a", directory: "C:\\new",
      access_mode: "default", expected_directory: "C:\\old",
    } });
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});

test("an execution-only device can bind its folder without a controller project or login", async () => {
  const device = createDeviceNetworkUiState();
  const project = { id: "project-a", label: "TODO アプリ", can_control: false, can_execute: true,
    environment_id: "environment-b", preparation_state: "pending" as const, error: null,
    directory: null, access_mode: "default" as const };
  const before: DeviceExecutionProjection = { revision: "3", state: "ready", projects: [project],
    review: null, directory: "C:\\base", access_mode: "default", accepting: true, can_pause: true,
    can_resume: false, error: null, unknown_attempts: [] };
  device.execution = before;
  assert.match(renderDeviceExecution(device), /data-action="bind-project-folder" data-value="project-a"/);
  const calls: Array<{name: string; args: unknown}> = [];
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  let projectionFetches = 0;
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (name: string, args: unknown) => {
    calls.push({ name, args });
    if (name === "browse_shared_project_folder") return "C:\\existing";
    if (name === "device_execution_projection") return ++projectionFetches === 1 ? before : { ...before, revision: "4",
      projects: [{ ...project, directory: "\\\\?\\C:\\existing", preparation_state: "ready" }] };
    if (name === "shared_work_projection") return sharedProjection({ principal: null, projects: [], selected_project_id: null });
    if (name === "shared_work_command") return sharedProjection({ principal: null, projects: [], selected_project_id: null });
    throw new Error(`unexpected command ${name}`);
  } } } });
  try {
    const context = { uiState: { deviceNetwork: device }, getViewState: () => ({ overlay: "hub" }), rerender() {} } as unknown as ActionContext;
    await bindProjectFolder(context, "project-a");
    assert.deepEqual(calls.find(call => call.name === "shared_work_command")?.args, { expectedGeneration: "1", request: {
      kind: "bind_project_folder", project_id: "project-a", environment_id: "environment-b", directory: "C:\\existing",
      access_mode: "default", expected_directory: null,
    } });
    assert.equal(device.executionError, "");
    assert.equal(device.execution?.projects[0].directory, "\\\\?\\C:\\existing");
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});

test("folder binding explains a returned command failure", async () => {
  const device = createDeviceNetworkUiState();
  const execution: DeviceExecutionProjection = { revision: "3", state: "ready", projects: [{ id: "project-a", label: "TODO アプリ",
    can_control: false, can_execute: true, environment_id: "environment-b", preparation_state: "pending", error: null,
    directory: null, access_mode: "default" }], review: null, directory: "C:\\base", access_mode: "default",
    accepting: true, can_pause: true, can_resume: false, error: null, unknown_attempts: [] };
  device.execution = execution;
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (name: string) => {
    if (name === "browse_shared_project_folder") return "C:\\existing";
    if (name === "device_execution_projection") return execution;
    if (name === "shared_work_projection") return sharedProjection({ principal: null, projects: [] });
    if (name === "shared_work_command") return sharedProjection({ error: "このPCの割り当てが変わりました。" });
    throw new Error(`unexpected command ${name}`);
  } } } });
  try {
    const context = { uiState: { deviceNetwork: device }, getViewState: () => ({ overlay: "hub" }), rerender() {} } as unknown as ActionContext;
    await bindProjectFolder(context, "project-a");
    assert.equal(device.executionError, "このPCの割り当てが変わりました。");
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});

test("an execution-only PC can confirm leaving from Settings and retains the durable pending state", async () => {
  const device = createDeviceNetworkUiState();
  const shared = sharedUiFixture();
  const execution: DeviceExecutionProjection = { revision: "3", state: "ready", projects: [{ id: "project-a", label: "TODO アプリ",
    can_control: false, can_execute: true, environment_id: "environment-b", preparation_state: "ready", error: null,
    directory: "C:\\existing", access_mode: "default", participation_generation: 7 }],
    review: null, directory: "C:\\base", access_mode: "default", accepting: true, can_pause: true,
    can_resume: false, error: null, unknown_attempts: [] };
  device.execution = execution;
  const context = { uiState: { deviceNetwork: device, sharedWork: shared }, getViewState: () => ({ overlay: "hub" }), rerender() {} } as unknown as ActionContext;
  assert.match(renderDeviceExecution(device, sharedWorkPresentation(shared)), /data-action="request-execution-project-leave" data-value="project-a"/);
  requestExecutionProjectLeave(context, "project-a");
  assert.deepEqual(device.executionLeaveConfirmation, { projectId: "project-a", participationGeneration: 7 });
  assert.match(renderDeviceExecution(device, sharedWorkPresentation(shared)), /このPCが「TODO アプリ」から離脱します/);
  const calls: Array<{name: string; args: unknown}> = [];
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (name: string, args: unknown) => {
    calls.push({ name, args });
    if (name === "device_execution_projection") return execution;
    if (name === "shared_work_projection") return sharedProjection({ principal: null, projects: [], selected_project_id: null });
    if (name === "shared_work_command") return sharedProjection({ revision: "2", principal: null, projects: [], selected_project_id: null,
      leave_pending_project_id: "project-a" });
    throw new Error(`unexpected command ${name}`);
  } } } });
  try {
    await confirmExecutionProjectLeave(context);
    assert.deepEqual(calls.find(call => call.name === "shared_work_command")?.args,
      { expectedGeneration: "1", request: { kind: "leave_project", project_id: "project-a" } });
    assert.equal(device.executionLeaveConfirmation, null);
    assert.equal(shared.projection?.leave_pending_project_id, "project-a");
    assert.match(renderDeviceExecution(device, sharedWorkPresentation(shared)), /このPCの離脱処理待ちです/);
    shared.projection!.leave_pending_project_id = null;
    device.execution!.projects[0].participation_generation = 0;
    const oldHub = renderDeviceExecution(device, sharedWorkPresentation(shared));
    assert.doesNotMatch(oldHub, /data-action="request-execution-project-leave"/);
    assert.match(oldHub, /Hubを更新し、接続を確認してください/);
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});

test("a folder choice made against a changed mapping does not submit and explains the conflict", async () => {
  const device = createDeviceNetworkUiState();
  const before: DeviceExecutionProjection = { revision: "3", state: "ready", projects: [{ id: "project-a", label: "TODO アプリ", can_control: true,
    can_execute: true, environment_id: "environment-a", preparation_state: "ready", error: null, directory: "C:\\old", access_mode: "default" }],
    review: null, directory: "C:\\base", access_mode: "default", accepting: true, can_pause: true, can_resume: false, error: null, unknown_attempts: [] };
  device.execution = before;
  const calls: string[] = [];
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (name: string) => {
    calls.push(name);
    if (name === "browse_shared_project_folder") return "C:\\new";
    if (name === "device_execution_projection") return { ...before, revision: "4", projects: [{ ...before.projects[0], directory: "C:\\other" }] };
    throw new Error(`unexpected command ${name}`);
  } } } });
  try {
    const context = { uiState: { deviceNetwork: device }, getViewState: () => ({ overlay: "hub" }), rerender() {} } as unknown as ActionContext;
    await bindProjectFolder(context, "project-a");
    assert.deepEqual(calls, ["browse_shared_project_folder", "device_execution_projection"]);
    assert.match(device.executionError, /設定が変わりました/);
    assert.equal(device.execution?.projects[0].directory, "C:\\other");
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
});

test("a stopped shared turn can be revised once with its exact job revision; an active or changed turn cannot", async () => {
  const local = sharedUiFixture();
  const old = { ...sharedProjection().status!.jobs[0], id: "job-a", state: "cancelled", can_cancel: false };
  local.projection = sharedProjection({ selected_job_id: old.id, selected_conversation_id: "conversation-a",
    conversations: [conversation], detail: { id: old.id, project_id: "project-a", root_id: old.id, parent_id: null,
      conversation_id: "conversation-a", environment_id: "env-a", title: "旧依頼", input: { prompt: "旧依頼" }, result: null,
      state: "cancelled", awaiting_child_id: null, revision: 7, created_at_ms: 1, updated_at_ms: 2, can_revise: true },
    conversation_history: { project_id: "project-a", conversation_id: "conversation-a", snapshot: 1, next_before: null,
      jobs: [{ job: old, input: { prompt: "旧依頼" }, result: null, artifacts: [], more_artifacts: false }] } });
  assert.equal(sharedWorkActionEnabled(local, "start-revise", old.id), true);
  local.editingJobId = old.id; local.editingJobRevision = 7; local.revisionDraft = "修正した依頼";
  assert.equal(sharedWorkActionEnabled(local, "save-revise", ""), true);
  const sent: Record<string, unknown>[] = [];
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (_name: string, args: {request: Record<string, unknown>}) => {
    sent.push(args.request);
    return { ...local.projection, revision: "2" };
  } } } });
  const context = { uiState: { sharedWork: local }, getViewState: () => ({ overlay: "none", hub_project_open: true }), rerender() {} } as unknown as ActionContext;
  try {
    await sharedWorkAction(context, "revise", old.id);
    assert.deepEqual(sent, [{ kind: "revise", project_id: "project-a", conversation_id: "conversation-a",
      job_id: old.id, expected_revision: 7, prompt: "修正した依頼" }]);
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
  local.editingJobId = old.id; local.editingJobRevision = 7; local.revisionDraft = "もう一度";
  local.projection.detail!.revision = 8;
  assert.equal(sharedWorkActionEnabled(local, "save-revise", ""), false);
  local.projection.detail!.revision = 7;
  local.projection.detail!.can_revise = false;
  assert.equal(sharedWorkActionEnabled(local, "save-revise", ""), false);
  local.projection.detail!.can_revise = true;
  local.projection.leave_pending_project_id = "project-a";
  assert.equal(sharedWorkActionEnabled(local, "save-revise", ""), false);
});

test("a revised Hub request keeps the old turn under an explicit history disclosure", () => {
  const local = sharedUiFixture();
  const base = sharedProjection().status!.jobs[0];
  const old = { ...base, id: "old-job", title: "旧依頼", state: "cancelled", can_cancel: false };
  const revised = { ...base, id: "new-job", title: "修正依頼", state: "succeeded", can_cancel: false, revises_job_id: old.id };
  local.projection = sharedProjection({ selected_job_id: revised.id,
    detail: { id: revised.id, project_id: "project-a", root_id: revised.id, parent_id: null,
      conversation_id: "conversation-a", environment_id: "env-a", title: "修正依頼", input: { prompt: "修正依頼" },
      result: null, state: "succeeded", awaiting_child_id: null, revision: 1, created_at_ms: 4, updated_at_ms: 5,
      revises_job_id: old.id },
    conversation_history: { project_id: "project-a", conversation_id: "conversation-a", snapshot: 1, next_before: null,
      jobs: [{ job: revised, input: { prompt: "修正依頼" }, result: null, artifacts: [], more_artifacts: false },
        { job: old, input: { prompt: "旧依頼" }, result: null, artifacts: [], more_artifacts: false }] } });
  const html = renderSharedWork(sharedWorkPresentation(local));
  assert.match(html, /<details class="shared-history-turn shared-revised-turn[^>]*data-shared-job-id="old-job"/);
  assert.match(html, /<summary>編集前の依頼/);
  assert.match(html, /編集して再送した依頼/);
});

test("a conversation awaiting deletion cannot receive follow-ups or revisions", async () => {
  const local = sharedUiFixture();
  const job = { ...sharedProjection().status!.jobs[0], id: "job-a", state: "succeeded", can_continue: true, can_revise: true };
  local.projection = sharedProjection({ selected_job_id: job.id, selected_conversation_id: conversation.id,
    conversations: [{ ...conversation, delete_pending: true }],
    detail: { id: job.id, project_id: "project-a", root_id: job.id, parent_id: null,
      conversation_id: conversation.id, environment_id: "env-a", title: "旧依頼", input: { prompt: "旧依頼" }, result: null,
      state: "succeeded", awaiting_child_id: null, revision: 7, created_at_ms: 1, updated_at_ms: 2,
      can_continue: true, can_revise: true },
    conversation_history: { project_id: "project-a", conversation_id: conversation.id, snapshot: 1, next_before: null,
      jobs: [{ job, input: { prompt: "旧依頼" }, result: null, artifacts: [], more_artifacts: false }] } });
  local.draft.followup = "続けて";
  assert.equal(sharedWorkActionEnabled(local, "continue", ""), false);
  assert.equal(sharedWorkActionEnabled(local, "start-revise", job.id), false);
  local.editingJobId = job.id; local.editingJobRevision = 7; local.revisionDraft = "編集案";
  assert.equal(sharedWorkActionEnabled(local, "save-revise", ""), false);
  local.editingJobId = null;
  const html = renderSharedWork(sharedWorkPresentation(local));
  assert.match(html, /このチャットは削除処理中です/);
  assert.match(html, /data-action="send"[^>]*disabled/);
  const calls: string[] = [];
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: async (name: string) => { calls.push(name); } } } });
  try {
    const context = { uiState: { sharedWork: local }, getViewState: () => ({ overlay: "none", hub_project_open: true }), rerender() {} } as unknown as ActionContext;
    await sharedWorkAction(context, "continue");
    await sharedWorkAction(context, "revise", job.id);
    assert.deepEqual(calls, []);
  } finally { if (original) Object.defineProperty(globalThis, "window", original); else delete (globalThis as Record<string, unknown>).window; }
  local.projection.detail = null;
  local.projection.selected_job_id = null;
  local.prompt = "新しい依頼";
  assert.equal(sharedWorkActionEnabled(local, "submit", ""), false);
});

test("the latest shared request with attachments explains why editing is unavailable", () => {
  const local = sharedUiFixture();
  const job = { ...sharedProjection().status!.jobs[0], state: "succeeded", can_revise: true };
  local.projection = sharedProjection({ selected_job_id: job.id, selected_conversation_id: conversation.id,
    conversations: [conversation], detail: { id: job.id, project_id: "project-a", root_id: job.id, parent_id: null,
      conversation_id: conversation.id, environment_id: "env-a", title: "添付付き", input: { prompt: "解析", input_refs: ["asset-a"] },
      result: null, state: "succeeded", awaiting_child_id: null, revision: 7, created_at_ms: 1, updated_at_ms: 2,
      can_revise: true }, conversation_history: { project_id: "project-a", conversation_id: conversation.id,
      snapshot: 1, next_before: null, jobs: [{ job, input: { prompt: "解析", input_refs: ["asset-a"] }, result: null,
        artifacts: [], more_artifacts: false }] } });
  assert.equal(sharedWorkActionEnabled(local, "start-revise", job.id), false);
  assert.match(renderSharedWork(sharedWorkPresentation(local)), /添付付きの依頼は編集できません。新しい依頼として送ってください。/);
});

test("an unupdated Hub project shows why sending is unavailable", () => {
  const local = sharedUiFixture();
  local.projection = sharedProjection({ projects_stale: true });
  local.prompt = "TODO アプリを作って";
  const html = renderSharedWork(sharedWorkPresentation(local));
  assert.match(html, /Hubのプロジェクト一覧は未更新です/);
  assert.match(html, /data-action="send"[^>]*disabled/);
});

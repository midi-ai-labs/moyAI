import assert from "node:assert/strict";
import test from "node:test";
import type { ActionContext } from "../src/actions.ts";
import {
  acceptPublishProjection, createPublishUiState, discardPublishDraft, editPublishField,
  newPublishProfile, publishCanOperate, publishCanSave, publishDirty, publishEditor,
  publishPresentation, publishValidation, selectPublishProfile, publishTargetKey, publishTargetChoices,
  type PublishProfileRow, type PublishProjection, type PublishTarget, type PublishJob,
} from "../src/mcp_publish_state.ts";
import { addPublish, choosePublish, copyPublish, operatePublish, savePublish, refreshPublishJobs, stopPublishJob, publishCertificate } from "../src/mcp_publish_actions.ts";
import { renderPublishOverlay } from "../src/mcp_publish_render.ts";
import { clearPublishSecret, synchronizePublishControlValues } from "../src/mcp_publish_dom.ts";
import { createSnapshotRefresh, installRuntimePolling, runtimePollingRequired } from "../src/polling_state.ts";

const target = { kind: "project", project_id: "project-a", workspace_root: "C:/workspace" } as const;
const temp = { kind: "temp" } as const;
function row(id = "profile-a"): PublishProfileRow {
  return { profile: { id, label: id, bind: "127.0.0.1:7332", target, tools: ["read"], max_concurrent_calls: 1,
    mode: { kind: "read_tools" }, tls: null,
    background: "stop_when_window_closes", enabled: false, transport: "streamable_http", authentication: { kind: "unpaired" } },
  status: "stopped", status_message: null, endpoint: null, active_calls: 0, connected_sessions: 0, recent_calls: [], credential_configured: false,
  can_edit: true, can_delete: true, can_start: false, can_stop: false, can_issue_token: true, can_revoke_token: false };
}
function projection(overrides: Partial<PublishProjection> = {}): PublishProjection {
  return { revision: "1", generation: "1", profiles: [row()], targets: [{ target, label: "Project" }, { target: temp, label: "temp" }], error: null, ...overrides };
}
function state() {
  const value = createPublishUiState();
  acceptPublishProjection(value, projection());
  return value;
}
function job(overrides: Partial<PublishJob> = {}): PublishJob {
  return { job_id: "job-a", profile_id: "profile-a", parent: { peer_id: "WinA", task_id: "task-a", turn_id: "turn-a" },
    prompt_preview: "調査の依頼", session_id: "session-a", state: "running", model: "model-a", result: null,
    result_truncated: false, can_stop: true, ...overrides };
}

test("inbound jobs retain provenance and escape final results with immutable Stop identity", () => {
  const local = state();
  local.jobs = [job(), job({ job_id: "job-b", state: "completed", result: "<script>bad()</script>", result_truncated: true, can_stop: false }),
    job({ profile_id: "profile-b", prompt_preview: "other profile task" })];
  const markup = renderPublishOverlay(publishPresentation(local));
  assert.match(markup, /WinA（接続元の申告）/);
  assert.match(markup, /data-action="mcp-publish-stop-job" data-value="job-a"/);
  assert.doesNotMatch(markup, /data-action="mcp-publish-stop-job" data-value="job-b"/);
  assert.match(markup, /結果を表示（一部省略）/);
  assert.match(markup, /&lt;script&gt;bad\(\)&lt;\/script&gt;/);
  assert.doesNotMatch(markup, /other profile task/);
  for (const terminal of ["interrupted", "failed", "completed"] as const) {
    local.jobs = [job({ state: terminal, result: null, can_stop: false })];
    const terminalMarkup = renderPublishOverlay(publishPresentation(local));
    assert.match(terminalMarkup, /返却された結果はありません/);
    assert.match(terminalMarkup, new RegExp(terminal === "interrupted" ? "停止しました" : terminal === "failed" ? "失敗しました" : "完了しました"));
    assert.doesNotMatch(terminalMarkup, /結果はまだ届いていません/);
  }
  for (const active of ["accepted", "running", "cancelling"] as const) {
    local.jobs = [job({ state: active })];
    assert.match(renderPublishOverlay(publishPresentation(local)), /結果はまだ届いていません/);
  }
});

test("new MCP publication is a local stopped draft without enabled intent, credentials or automatic start", () => {
  const local = createPublishUiState();
  acceptPublishProjection(local, projection({ profiles: [] }));
  newPublishProfile(local);
  const draft = publishEditor(local)!;
  assert.equal(draft.value.bind, "127.0.0.1:7332");
  assert.equal(draft.value.background, "stop_when_window_closes");
  assert.deepEqual(draft.value.tools, []);
  assert.equal(draft.value.target, null, "a new profile must not silently publish the first available project");
  assert.equal("enabled" in draft.value, false);
  assert.equal("authentication" in draft.value, false);
  assert.equal(publishCanSave(local), false);
  editPublishField(local, "label", "調査用");
  assert.equal(publishValidation(local), "公開するプロジェクトを選択してください。");
  assert.equal(publishCanSave(local), false);
  editPublishField(local, "target", publishTargetKey(target));
  assert.equal(publishCanSave(local), true, "saving a disabled profile need not start a listener");
  assert.equal(publishCanOperate(local, "start"), false);
});

test("agent publication is an explicit permission choice and never inherits read-tool grants", () => {
  const local = state();
  editPublishField(local, "mode", "agent");
  assert.deepEqual(publishEditor(local)!.value.mode, { kind: "agent", access_mode: "default" });
  assert.deepEqual(publishEditor(local)!.value.tools, []);
  editPublishField(local, "tool:read", "", true);
  assert.deepEqual(publishEditor(local)!.value.tools, []);
  editPublishField(local, "access_mode", "full_access");
  assert.deepEqual(publishEditor(local)!.value.mode, { kind: "agent", access_mode: "full_access" });
  editPublishField(local, "target", publishTargetKey(temp));
  assert.equal(publishCanSave(local), true, "agent temp is a supported explicit target");
  const markup = renderPublishOverlay(publishPresentation(local));
  assert.match(markup, /data-mcp-section="read-tools"[^>]*hidden/);
  assert.match(markup, /実行権限/);
  assert.match(markup, /トークン.*再発行/);
  editPublishField(local, "mode", "read_tools");
  assert.deepEqual(publishEditor(local)!.value.mode, { kind: "read_tools" });
  assert.deepEqual(publishEditor(local)!.value.tools, []);
  editPublishField(local, "access_mode", "full_access");
  assert.deepEqual(publishEditor(local)!.value.mode, { kind: "read_tools" });
});

test("agent publication cannot reuse legacy authority and TLS requires explicit host and certificate paths", () => {
  const local = state();
  const legacy = { kind: "legacy_session", project_id: "project-a", root_session_id: "root-a", workspace_root: "C:/workspace" } as const;
  const saved = row();
  saved.profile.target = legacy;
  acceptPublishProjection(local, projection({ profiles: [saved], targets: [{ target: legacy, label: "Old chat" }, { target, label: "Project" }, { target: temp, label: "temp" }] }));
  editPublishField(local, "mode", "agent");
  assert.deepEqual(publishEditor(local)!.value.target, legacy, "mode changes must not silently replace the target");
  assert.equal(publishCanSave(local), false);
  assert.ok(publishTargetChoices(local).every((choice) => choice.target.kind !== "legacy_session"));
  editPublishField(local, "target", publishTargetKey(target));
  editPublishField(local, "host", "192.168.10.22");
  assert.equal(publishCanSave(local), false);
  editPublishField(local, "tls", "enabled");
  assert.equal(publishCanSave(local), false);
  editPublishField(local, "certificate_path", "C:/certs/server.pem");
  editPublishField(local, "private_key_path", "C:/certs/server-key.pem");
  assert.equal(publishCanSave(local), true);
  editPublishField(local, "host", "0.0.0.0");
  assert.equal(publishCanSave(local), false);
  editPublishField(local, "host", "server.example");
  assert.equal(publishCanSave(local), false);
  editPublishField(local, "host", "192.168.10.22");
  editPublishField(local, "tls", "disabled");
  assert.equal(publishCanSave(local), false, "removing TLS never silently falls back to plaintext LAN");
});

test("publication settings require explicit close and keep Save with its explanation in the footer", () => {
  const local = state();
  editPublishField(local, "label", "");
  const markup = renderPublishOverlay(publishPresentation(local));
  const backdrop = markup.match(/^<div\b([^>]*)>/)?.[1];
  assert.ok(backdrop);
  assert.doesNotMatch(backdrop, /data-action=/, "a background click must not dismiss the editor");
  const footer = markup.match(/<footer\b[^>]*>([\s\S]*?)<\/footer>/)?.[1];
  assert.ok(footer);
  assert.match(footer, /id="mcp-publish-save"[^>]*disabled/);
  assert.match(footer, /表示名を1〜80文字で入力してください/);
  assert.match(footer, /data-action="mcp-publish-discard"/);
  assert.match(footer, /data-action="close-overlay"/);
  assert.equal([...markup.matchAll(/id="mcp-publish-save"/g)].length, 1);
});

test("profile switching and runtime polling preserve each profile's unsaved editor", () => {
  const local = state();
  acceptPublishProjection(local, projection({ profiles: [row(), row("profile-b")] }));
  editPublishField(local, "label", "日本語の編集中");
  editPublishField(local, "port", "7444");
  selectPublishProfile(local, "profile-b");
  editPublishField(local, "concurrency", "3");
  const refreshed = projection({ profiles: [row(), { ...row("profile-b"), active_calls: 2 }] });
  acceptPublishProjection(local, refreshed);
  selectPublishProfile(local, "profile-a");
  assert.equal(publishEditor(local)!.value.label, "日本語の編集中");
  assert.equal(publishEditor(local)!.port, "7444");
  assert.equal(local.drafts["profile-b"].concurrency, "3");
});

test("another profile's save may advance unchanged canonical baseline while same-profile external edits stay stale", () => {
  const local = state();
  editPublishField(local, "label", "local edit");
  acceptPublishProjection(local, projection({ revision: "2", generation: "2", profiles: [row(), row("profile-b")] }));
  assert.equal(publishEditor(local)!.revision, "2");
  assert.equal(publishCanSave(local), true);
  const external = row();
  external.profile.label = "external edit";
  acceptPublishProjection(local, projection({ revision: "3", generation: "3", profiles: [external] }));
  assert.equal(publishEditor(local)!.value.label, "local edit");
  assert.equal(publishEditor(local)!.revision, "2");
  assert.equal(publishCanSave(local), false);
  discardPublishDraft(local);
  assert.equal(publishEditor(local)!.value.label, "external edit");
  assert.equal(publishDirty(publishEditor(local)!), false);
});

test("projection rejects old decimal revisions and generations without rounding u64 identifiers", () => {
  const local = state();
  acceptPublishProjection(local, projection({ revision: "9007199254740993", generation: "9007199254740993" }));
  editPublishField(local, "label", "held draft");
  assert.equal(acceptPublishProjection(local, projection({ revision: "9007199254740992", generation: "9007199254740994" })), false);
  assert.equal(acceptPublishProjection(local, projection({ revision: "9007199254740994", generation: "9007199254740992" })), false);
  assert.equal(publishEditor(local)!.value.label, "held draft");
});

test("removed profiles preserve dirty text but cannot save or start under another owner", () => {
  const local = state();
  editPublishField(local, "label", "unsaved text");
  acceptPublishProjection(local, projection({ revision: "2", generation: "2", profiles: [] }));
  assert.equal(publishEditor(local)!.value.label, "unsaved text");
  assert.equal(publishCanSave(local), false);
  assert.equal(publishCanOperate(local, "start"), false);
  discardPublishDraft(local);
  assert.equal(local.selectedId, null);
});

test("local publication validation keeps malformed numeric input and requires an available exact target", () => {
  for (const [field, text] of [["host", "0.0.0.0"], ["host", "127.999.1.2"], ["port", "65536"],
    ["port", "12.5"], ["concurrency", "0"], ["concurrency", "17"]]) {
    const local = state();
    editPublishField(local, field, text);
    assert.ok(publishValidation(local), `${field}: ${text}`);
    assert.equal(publishCanSave(local), false);
  }
  const local = state();
  editPublishField(local, "host", "::1");
  assert.equal(publishValidation(local), null);
  acceptPublishProjection(local, projection({ targets: [] }));
  assert.equal(publishValidation(local), "公開するプロジェクトを選択してください。");
  assert.equal(publishCanSave(local), false);
});

test("changed MCP project root requires explicit target reselection even with the same project ID", () => {
  const local = state();
  editPublishField(local, "label", "保持する編集中の名前");
  const moved = { ...target, workspace_root: "C:/workspace/nested" };
  acceptPublishProjection(local, projection({ targets: [{ target: moved, label: "Moved project" }] }));
  assert.deepEqual(publishEditor(local)!.value.target, target, "polling must not change the draft authority");
  assert.equal(publishCanSave(local), false);

  const options = () => {
    const markup = renderPublishOverlay(publishPresentation(local));
    const select = markup.match(/<select\b[^>]*id="mcp-publish-target"[^>]*>([\s\S]*?)<\/select>/)?.[1];
    assert.ok(select, "the target selector is rendered");
    return [...select.matchAll(/<option\b([^>]*)>([\s\S]*?)<\/option>/g)].map((match) => ({
      value: match[1].match(/\bvalue="([^"]*)"/)?.[1],
      selected: /\bselected\b/.test(match[1]),
      label: match[2],
    }));
  };
  const before = options();
  const placeholder = before.find((option) => option.selected);
  assert.ok(placeholder);
  assert.match(placeholder.label, /保存した対象は現在利用できません/);
  const available = before.find((option) => option.value === publishTargetKey(moved));
  assert.ok(available);
  assert.equal(available.selected, false, "the sole current candidate must not look preselected");
  assert.notEqual(placeholder.value, available.value, "selecting the current candidate must produce a native change");
  editPublishField(local, "target", placeholder.value!);
  assert.deepEqual(publishEditor(local)!.value.target, target);
  assert.equal(publishCanSave(local), false);

  // events.ts forwards the explicitly selected option value to this draft owner.
  editPublishField(local, "target", available.value!);
  assert.deepEqual(publishEditor(local)!.value.target, moved);
  assert.equal(publishEditor(local)!.value.label, "保持する編集中の名前");
  assert.deepEqual(local.projection!.profiles[0].profile.target, target, "selection has not saved the profile");
  assert.equal(publishCanSave(local), true);
  assert.deepEqual(options().map((option) => [option.value, option.selected]), [[publishTargetKey(moved), true]]);
});

test("Temp selection removes file grants without granting current_time and cannot accept file-tool edits", () => {
  const local = state();
  editPublishField(local, "tool:grep", "", true);
  editPublishField(local, "target", publishTargetKey(temp));
  assert.deepEqual(publishEditor(local)!.value.target, temp);
  assert.deepEqual(publishEditor(local)!.value.tools, [], "changing scope cannot grant a new tool");
  assert.equal(publishCanSave(local), true, "an empty tool list may be saved without starting");
  const markup = renderPublishOverlay(publishPresentation(local));
  assert.match(markup, /tempはプロジェクトを使わず、現在時刻だけを公開できます/);
  for (const name of ["list", "glob", "grep", "read", "inspect_directory"]) {
    assert.match(markup, new RegExp(`id="mcp-publish-tool-${name}"[^>]*disabled`));
    editPublishField(local, `tool:${name}`, "", true);
  }
  assert.deepEqual(publishEditor(local)!.value.tools, []);
  assert.doesNotMatch(markup.match(/<input\b[^>]*id="mcp-publish-tool-current_time"[^>]*>/)![0], /\bdisabled\b|\bchecked\b/);
  editPublishField(local, "tool:current_time", "", true);
  assert.deepEqual(publishEditor(local)!.value.tools, ["current_time"]);
  editPublishField(local, "target", publishTargetKey(target));
  assert.deepEqual(publishEditor(local)!.value.tools, ["current_time"], "returning to a project does not restore removed file grants");
  editPublishField(local, "tool:read", "", true);
  editPublishField(local, "target", publishTargetKey(temp));
  assert.deepEqual(publishEditor(local)!.value.tools, ["current_time"], "already selected current_time remains allowed");
});

test("Temp remains explicitly selectable without a project or chat and polling never changes scope", () => {
  const local = createPublishUiState();
  acceptPublishProjection(local, projection({ profiles: [], targets: [{ target: temp, label: "temp" }] }));
  newPublishProfile(local);
  editPublishField(local, "label", "時刻だけ");
  assert.equal(publishEditor(local)!.value.target, null);
  assert.equal(publishCanSave(local), false);
  editPublishField(local, "target", publishTargetKey(temp));
  editPublishField(local, "tool:current_time", "", true);
  assert.equal(publishCanSave(local), true);
  acceptPublishProjection(local, projection({ profiles: [] }));
  assert.deepEqual(publishEditor(local)!.value.target, temp);
  assert.deepEqual(publishEditor(local)!.value.tools, ["current_time"]);
  assert.equal(publishCanSave(local), true);
});

test("legacy chat scope is offered only to its saved profile and changes require explicit project selection", () => {
  const legacy = { kind: "legacy_session", project_id: target.project_id, root_session_id: "root-a",
    workspace_root: `${target.workspace_root}/nested` } as const;
  const saved = row(); saved.profile.target = legacy;
  const local = state();
  const candidates = [{ target, label: "Project" }, { target: temp, label: "temp" },
    { target: legacy, label: "Existing chat (compatibility)" }];
  acceptPublishProjection(local, projection({ profiles: [saved, row("profile-b")], targets: candidates }));
  editPublishField(local, "label", "旧範囲を保持");
  assert.equal(publishCanSave(local), true);
  assert.match(renderPublishOverlay(publishPresentation(local)), /旧設定のチャット・フォルダ範囲を維持/);
  editPublishField(local, "target", publishTargetKey(target));
  assert.deepEqual(publishEditor(local)!.value.target, target);
  editPublishField(local, "target", publishTargetKey(legacy));
  assert.deepEqual(publishEditor(local)!.value.target, legacy, "the saved scope can be restored before saving");
  selectPublishProfile(local, "profile-b");
  assert.equal(publishTargetChoices(local).some((choice) => choice.target.kind === "legacy_session"), false);
  editPublishField(local, "target", publishTargetKey(legacy));
  assert.deepEqual(publishEditor(local)!.value.target, target);
  newPublishProfile(local);
  editPublishField(local, "target", publishTargetKey(legacy));
  assert.equal(publishEditor(local)!.value.target, null);
  selectPublishProfile(local, "profile-a");
  const changedLegacy = { ...legacy, workspace_root: `${target.workspace_root}/changed` };
  acceptPublishProjection(local, projection({ profiles: [saved], targets: [...candidates.slice(0, 2),
    { target: changedLegacy, label: "Changed old chat" }] }));
  assert.deepEqual(publishEditor(local)!.value.target, legacy, "old chat scope cannot follow changed cwd");
  assert.equal(publishCanSave(local), false);
  editPublishField(local, "target", publishTargetKey(changedLegacy));
  assert.deepEqual(publishEditor(local)!.value.target, legacy);
  editPublishField(local, "target", publishTargetKey(target));
  assert.equal(publishCanSave(local), true);
});

test("publish target identity separates kinds, projects, legacy sessions and exact paths", () => {
  const targets: PublishTarget[] = [temp, target, { ...target, workspace_root: `${target.workspace_root}/nested` },
    { ...target, project_id: "project-b" },
    { ...target, kind: "legacy_session", root_session_id: "root-a" },
    { ...target, kind: "legacy_session", root_session_id: "root-b" }];
  assert.equal(new Set(targets.map(publishTargetKey)).size, targets.length);
  assert.ok(targets.every((candidate) => publishTargetKey(candidate) !== publishTargetKey(null)));
});

test("runtime capabilities and dirty state gate start while Stop and revoke remain explicit recovery operations", () => {
  const local = state();
  const ready = { ...row(), credential_configured: true, can_start: true, can_revoke_token: true };
  acceptPublishProjection(local, projection({ profiles: [ready] }));
  assert.equal(publishCanOperate(local, "start"), true);
  editPublishField(local, "label", "dirty");
  assert.equal(publishCanOperate(local, "start"), false);
  acceptPublishProjection(local, projection({ generation: "2", profiles: [{ ...ready, status: "running", can_edit: false,
    can_delete: false, can_start: false, can_stop: true, can_issue_token: false }] }));
  assert.equal(publishCanOperate(local, "stop"), true);
  assert.equal(publishCanOperate(local, "revoke_token"), true);
  const before = publishEditor(local)!.value.label;
  editPublishField(local, "label", "wrongly changed");
  assert.equal(publishEditor(local)!.value.label, before);
});

test("publication rendering exposes independent save/start, typed status and bounded read tools without secret values", () => {
  const local = state();
  const markup = renderPublishOverlay(publishPresentation(local));
  assert.match(markup, /旧配信設定の管理/);
  assert.match(markup, /証明書の自動設定は「moyAI Hub」の端末連携/);
  assert.match(markup, />設定を保存<\/button>/);
  assert.match(markup, />配信を開始<\/button>/);
  assert.match(markup, /停止中/);
  for (const label of ["一覧を見る", "名前で探す", "内容を検索", "ファイルを読む", "構成を調べる", "現在時刻"]) assert.ok(markup.includes(label));
  assert.match(markup, /type="password" readonly/);
  assert.equal("token" in publishPresentation(local), false);
  assert.match(markup, /次回起動時に自動では開始しません/);
});

test("publication start receipt keeps existing Desktop polling alive before the ordinary snapshot arrives", () => {
  assert.equal(runtimePollingRequired(false, false, null, projection()), false);
  for (const status of ["starting", "running", "stopping"] as const) {
    assert.equal(runtimePollingRequired(false, false, null, projection({ profiles: [{ ...row(), status }] })), true);
  }
});

test("window restore refreshes a stopped publication owner and retains its dirty settings after idle polling stops", () => {
  for (const restoredEvent of ["focus", "visibilitychange"]) {
    const local = state();
    const hidden = { ...row(), credential_configured: true, can_start: false };
    let native = projection({ generation: "7", profiles: [hidden] });
    acceptPublishProjection(local, native);
    editPublishField(local, "background", "keep_while_application_running");
    assert.equal(runtimePollingRequired(false, false, null, local.projection), false);

    const windowTarget = new EventTarget();
    const documentTarget = Object.assign(new EventTarget(), { hidden: true });
    let tick: () => void = () => undefined;
    let cleared = false;
    let refreshes = 0;
    const stop = installRuntimePolling(Object.assign(windowTarget, {
      setInterval(callback: TimerHandler) { tick = callback as () => void; return 1; },
      clearInterval(id: number) { assert.equal(id, 1); cleared = true; },
    }), documentTarget, () => runtimePollingRequired(false, false, null, local.projection), () => {
      refreshes += 1;
      acceptPublishProjection(local, native);
    });
    tick();
    windowTarget.dispatchEvent(new Event("focus"));
    documentTarget.dispatchEvent(new Event("visibilitychange"));
    assert.equal(refreshes, 0, "a hidden stopped window needs no continuous polling");

    native = projection({ generation: "8", profiles: [{ ...hidden, can_start: true }] });
    documentTarget.hidden = false;
    (restoredEvent === "focus" ? windowTarget : documentTarget).dispatchEvent(new Event(restoredEvent));
    assert.equal(local.projection!.generation, "8", restoredEvent);
    assert.equal(publishEditor(local)!.value.background, "keep_while_application_running");
    assert.equal(publishCanSave(local), true, "restore preserves the same canonical edit baseline");
    assert.equal(publishCanOperate(local, "start"), false, "an unsaved edit still requires explicit save");
    discardPublishDraft(local);
    assert.equal(publishCanOperate(local, "start"), true, "fresh visible native capability re-enables Start");
    stop();
    assert.equal(cleared, true);
    windowTarget.dispatchEvent(new Event("focus"));
    documentTarget.dispatchEvent(new Event("visibilitychange"));
    assert.equal(refreshes, 1, "disposed window subscriptions do not refresh");
  }
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((accept, fail) => { resolve = accept; reject = fail; });
  return { promise, resolve, reject };
}

test("restore during an old hidden snapshot coalesces one fresh read and retains the publication draft", async () => {
  const local = state();
  const hidden = { ...row(), credential_configured: true, can_start: false };
  const oldSnapshot = projection({ generation: "7", profiles: [hidden] });
  const shownSnapshot = projection({ generation: "8", profiles: [{ ...hidden, can_start: true }] });
  acceptPublishProjection(local, oldSnapshot);
  editPublishField(local, "background", "keep_while_application_running");
  const delayed = deferred<PublishProjection>();
  let calls = 0;
  let active = 0;
  let maxActive = 0;
  const refresh = createSnapshotRefresh(async () => {
    active += 1;
    maxActive = Math.max(maxActive, active);
    const snapshot = ++calls === 1 ? await delayed.promise : shownSnapshot;
    acceptPublishProjection(local, snapshot);
    active -= 1;
  });
  const windowTarget = Object.assign(new EventTarget(), {
    setInterval(_callback: TimerHandler) { return 1; },
    clearInterval(_id: number) {},
  });
  const documentTarget = Object.assign(new EventTarget(), { hidden: true });
  const stop = installRuntimePolling(windowTarget, documentTarget,
    () => runtimePollingRequired(false, false, null, local.projection), refresh);
  const oldRead = refresh();
  await Promise.resolve();
  assert.equal(calls, 1);
  const overlappingTick = refresh();
  documentTarget.hidden = false;
  windowTarget.dispatchEvent(new Event("focus"));
  documentTarget.dispatchEvent(new Event("visibilitychange"));
  assert.equal(calls, 1, "restore events do not open concurrent snapshot requests");
  delayed.resolve(oldSnapshot);
  await Promise.all([oldRead, overlappingTick]);
  stop();
  assert.equal(calls, 2, "both restore events request one fresh read after the old response");
  assert.equal(maxActive, 1);
  assert.equal(local.projection!.generation, "8");
  assert.equal(publishEditor(local)!.value.background, "keep_while_application_running");
  assert.equal(publishCanSave(local), true);
  discardPublishDraft(local);
  assert.equal(publishCanOperate(local, "start"), true);
  assert.equal(runtimePollingRequired(false, false, null, local.projection), false);
});

test("ordinary overlapping polling does not queue an extra snapshot without a restore request", async () => {
  const delayed = deferred<void>();
  let calls = 0;
  const refresh = createSnapshotRefresh(async () => { calls += 1; await delayed.promise; });
  const first = refresh();
  await Promise.resolve();
  const second = refresh();
  delayed.resolve();
  await Promise.all([first, second]);
  assert.equal(calls, 1);
  await refresh();
  assert.equal(calls, 2, "the settled owner admits the next ordinary read");
});
async function withContext(invoke: (name: string, args: Record<string, unknown>) => Promise<unknown>,
  run: (fixture: { context: ActionContext; local: ReturnType<typeof state>; token: { value: string };
    content: { scrollTop: number }; view: { overlay: string }; copied: string[] }) => Promise<void>): Promise<void> {
  const local = state();
  const token = { value: "" };
  const content = { scrollTop: 0 };
  const view = { overlay: "mcp_publish" };
  const copied: string[] = [];
  const context = { uiState: { mcpPublish: local }, getViewState: () => view, rerender: () => undefined } as unknown as ActionContext;
  const saved = new Map(["window", "document", "navigator"].map((name) => [name, Object.getOwnPropertyDescriptor(globalThis, name)]));
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke } } });
  Object.defineProperty(globalThis, "document", { configurable: true, value: { querySelector: (selector: string) =>
    selector === "#mcp-publish-token" ? token : selector === '[data-modal="mcp_publish"] .mcp-publish-content' ? content : null } });
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: { clipboard: { writeText: async (text: string) => { copied.push(text); } } } });
  try { await run({ context, local, token, content, view, copied }); }
  finally {
    for (const [name, descriptor] of saved) {
      if (descriptor) Object.defineProperty(globalThis, name, descriptor);
      else delete (globalThis as Record<string, unknown>)[name];
    }
  }
}

test("jobs read discards an old response after a newer read and never rolls a Stop receipt back", async () => {
  const old = deferred<PublishJob[]>();
  let reads = 0;
  const calls: { name: string; args: Record<string, unknown> }[] = [];
  await withContext(async (name, args) => {
    calls.push({ name, args });
    if (name === "mcp_publish_cancel_job") return job({ state: "cancelling", can_stop: false });
    return ++reads === 1 ? old.promise : [job()];
  }, async ({ context, local }) => {
    const first = refreshPublishJobs(context);
    await refreshPublishJobs(context);
    assert.equal(local.jobs[0].state, "running");
    await stopPublishJob(context, "job-a");
    old.resolve([job({ state: "accepted" })]);
    await first;
    assert.equal(local.jobs[0].state, "cancelling");
    assert.deepEqual(calls.find((call) => call.name === "mcp_publish_cancel_job")?.args,
      { profileId: "profile-a", jobId: "job-a" });
    const count = calls.length;
    await stopPublishJob(context, "job-a");
    assert.equal(calls.length, count, "a cancelling immutable job cannot be stopped again via stale UI");
  });
});

test("TLS certificate generation changes only the selected draft and copying uses saved profile CAS", async () => {
  const calls: { name: string; args: Record<string, unknown> }[] = [];
  const tls = { certificate_path: "C:/certs/server.pem", private_key_path: "C:/certs/server-key.pem" };
  await withContext(async (name, args) => { calls.push({ name, args }); return { tls, certificate_pem: "PUBLIC CERTIFICATE", sha256: "sha256-public" }; },
    async ({ context, local, copied }) => {
      editPublishField(local, "tls", "enabled");
      editPublishField(local, "host", "192.168.10.22");
      await publishCertificate(context, true);
      assert.deepEqual(publishEditor(local)!.value.tls, tls);
      assert.equal(local.projection!.profiles[0].profile.tls, null);
      assert.equal(publishDirty(publishEditor(local)!), true);
      assert.deepEqual(calls[0].args, { id: "profile-a", bindIp: "192.168.10.22", revision: "1", generation: "1" });
      assert.deepEqual(copied, [], "generating a certificate is not a clipboard operation");
      const saved = row(); saved.profile.tls = tls;
      acceptPublishProjection(local, projection({ revision: "2", generation: "2", profiles: [saved] }), "profile-a");
      await publishCertificate(context, false);
      assert.deepEqual(calls[1].args, { id: "profile-a", revision: "2", generation: "2" });
      assert.deepEqual(copied, ["PUBLIC CERTIFICATE"]);
      assert.doesNotMatch(copied.join(""), /server-key/);
    });
});

test("certificate completion targets the current retained editor after a clean polling replacement", async () => {
  const response = deferred<{ tls: { certificate_path: string; private_key_path: string }; certificate_pem: string; sha256: string }>();
  await withContext(async () => response.promise, async ({ context, local }) => {
    const saved = row();
    saved.profile.tls = { certificate_path: "old.pem", private_key_path: "old-key.pem" };
    acceptPublishProjection(local, projection({ profiles: [saved] }));
    const operation = publishCertificate(context, true);
    acceptPublishProjection(local, projection({ profiles: [saved] }));
    response.resolve({ tls: { certificate_path: "new.pem", private_key_path: "new-key.pem" }, certificate_pem: "PUBLIC", sha256: "HASH" });
    await operation;
    assert.equal(publishEditor(local)!.value.tls?.certificate_path, "new.pem");
    assert.equal(local.projection!.profiles[0].profile.tls?.certificate_path, "old.pem");
  });
});

test("explicit Add reveals the publication editor before render while polling and normal edits retain scroll", async () => {
  const calls: string[] = [];
  await withContext(async (name) => { calls.push(name); return projection(); }, async ({ context, local, content }) => {
    editPublishField(local, "label", "保存前の既存設定");
    content.scrollTop = 780;
    const scrollAtRender: number[] = [];
    context.rerender = () => { scrollAtRender.push(content.scrollTop); };
    addPublish(context);
    assert.equal(local.selectedId, "new");
    assert.equal(content.scrollTop, 0, "Add must reveal the name and target instead of retaining the token area's position");
    assert.deepEqual(scrollAtRender, [0], "the existing render snapshot must capture the new start position");
    assert.equal(local.drafts["profile-a"].value.label, "保存前の既存設定");

    content.scrollTop = 460;
    editPublishField(local, "label", "新規設定の入力中");
    acceptPublishProjection(local, projection({ generation: "2" }));
    assert.equal(content.scrollTop, 460, "editing and polling must not become scroll-to-top triggers");
    choosePublish(context, "profile-a");
    choosePublish(context, "new");
    assert.equal(content.scrollTop, 460, "ordinary draft selection preserves the current viewport");
    assert.equal(publishEditor(local)!.value.label, "新規設定の入力中");

    local.pending = "save";
    addPublish(context);
    assert.equal(content.scrollTop, 460, "a rejected Add must not change the viewport");
    local.pending = null;
    addPublish(context);
    assert.equal(content.scrollTop, 0, "explicit Add also reveals the existing unsaved draft");
    assert.equal(publishEditor(local)!.value.label, "新規設定の入力中");
    assert.deepEqual(calls, []);
  });
});

test("saving sends only the captured profile draft and CAS owner, never starts or accepts optimistic changes", async () => {
  const response = deferred<PublishProjection>();
  const calls: { name: string; args: Record<string, unknown> }[] = [];
  await withContext(async (name, args) => { calls.push({ name, args }); return response.promise; }, async ({ context, local }) => {
    editPublishField(local, "label", "Saved label");
    const operation = savePublish(context);
    assert.equal(local.pending, "save");
    assert.equal(local.projection!.profiles[0].profile.label, "profile-a");
    assert.equal(calls[0].name, "mcp_publish_save");
    assert.equal(calls[0].args.expectedRevision, "1");
    assert.equal(calls[0].args.expectedGeneration, "1");
    assert.equal("enabled" in (calls[0].args.draft as object), false);
    const updated = row(); updated.profile.label = "Saved label";
    response.resolve(projection({ revision: "2", generation: "2", profiles: [updated] }));
    await operation;
    assert.equal(publishDirty(publishEditor(local)!), false);
    assert.equal(local.projection!.profiles[0].status, "stopped");
    assert.equal(calls.length, 1);
  });
});

test("failed save keeps input and the lock until fresh native owner is reacquired", async () => {
  const reacquired = deferred<PublishProjection>();
  const calls: string[] = [];
  await withContext(async (name) => { calls.push(name); if (name === "mcp_publish_save") throw "stale_generation"; return reacquired.promise; }, async ({ context, local }) => {
    editPublishField(local, "label", "retain me");
    const operation = savePublish(context);
    await Promise.resolve(); await Promise.resolve();
    assert.equal(local.pending, "save");
    reacquired.resolve(projection({ generation: "2" }));
    await operation;
    assert.equal(local.pending, null);
    assert.equal(publishEditor(local)!.value.label, "retain me");
    assert.equal(local.projection!.generation, "2");
    assert.match(local.error, /配信状態が変わりました/);
    assert.deepEqual(calls, ["mcp_publish_save", "mcp_publish_projection"]);
  });
});

test("new project and Temp saves send only the explicitly selected target and grants", async () => {
  for (const selectedTarget of [target, temp]) {
    const calls: { name: string; args: Record<string, unknown> }[] = [];
    const saved = row("profile-new");
    saved.profile.target = selectedTarget;
    saved.profile.tools = ["current_time"];
    await withContext(async (name, args) => {
      calls.push({ name, args });
      return projection({ revision: "2", generation: "2", profiles: [row(), saved] });
    }, async ({ context, local }) => {
      addPublish(context);
      editPublishField(local, "label", "明示した公開範囲");
      await savePublish(context);
      assert.deepEqual(calls, [], "the unselected target cannot be submitted");
      editPublishField(local, "target", publishTargetKey(selectedTarget));
      editPublishField(local, "tool:current_time", "", true);
      await savePublish(context);
      assert.equal(calls.length, 1);
      assert.equal(calls[0].name, "mcp_publish_save");
      assert.equal(calls[0].args.profileId, null);
      const draft = calls[0].args.draft as { target: PublishTarget; tools: string[] };
      assert.deepEqual(draft.target, selectedTarget);
      assert.deepEqual(draft.tools, ["current_time"]);
      assert.equal("root_session_id" in draft.target, false);
      assert.equal(local.selectedId, saved.profile.id);
      assert.equal(publishDirty(publishEditor(local)!), false);
    });
  }
});

test("late successful command cannot roll back a newer profile projection or clear its draft", async () => {
  const receipt = deferred<PublishProjection>();
  await withContext(async () => receipt.promise, async ({ context, local }) => {
    editPublishField(local, "label", "held edit");
    const operation = savePublish(context);
    const newer = row(); newer.profile.label = "external winner";
    acceptPublishProjection(local, projection({ revision: "3", generation: "3", profiles: [newer] }));
    receipt.resolve(projection({ revision: "2", generation: "2" }));
    await operation;
    assert.equal(local.projection!.profiles[0].profile.label, "external winner");
    assert.equal(publishEditor(local)!.value.label, "held edit");
  });
});

test("token is delivered only to current DOM, copied with the observed endpoint, and cleared on profile change", async () => {
  const issuedRow = { ...row(), credential_configured: true, can_start: true, can_revoke_token: true,
    endpoint: "http://127.0.0.1:7332/mcp" };
  await withContext(async () => ({ profile_id: "profile-a", token: "secret-one-shot", projection: projection({ generation: "2", profiles: [issuedRow, row("profile-b")] }) }),
    async ({ context, local, token, copied }) => {
      await operatePublish(context, "issue_token");
      assert.equal(token.value, "secret-one-shot");
      assert.equal(JSON.stringify(local).includes("secret-one-shot"), false);
      assert.equal(renderPublishOverlay(publishPresentation(local)).includes("secret-one-shot"), false);
      await copyPublish(context, true);
      const configuration = JSON.parse(copied[0]);
      assert.equal(configuration.mcpServers["profile-a"].url, issuedRow.endpoint);
      assert.equal(configuration.mcpServers["profile-a"].headers.Authorization, "Bearer secret-one-shot");
      choosePublish(context, "profile-b");
      assert.equal(token.value, "");
      token.value = "another-secret";
      clearPublishSecret();
      assert.equal(token.value, "");
    });
});

test("closing the overlay before a token receipt arrives never reveals its secret", async () => {
  const response = deferred<unknown>();
  await withContext(async () => response.promise, async ({ context, local, view, token }) => {
    const operation = operatePublish(context, "issue_token");
    view.overlay = "none";
    response.resolve({ profile_id: "profile-a", token: "old-secret", projection: projection({ generation: "2" }) });
    await operation;
    assert.equal(token.value, "");
    assert.equal(local.pending, null);
    assert.equal(JSON.stringify(local).includes("old-secret"), false);
  });
});

test("credential rotation clears the retained secret while unrelated polling keeps it", () => {
  const token = { value: "retained-secret" };
  const current = { dataset: { profileId: "profile-a", credentialId: "credential-a" },
    querySelector: () => token } as unknown as HTMLElement;
  const next = { dataset: { profileId: "profile-a", credentialId: "credential-a" },
    querySelectorAll: () => [] } as unknown as HTMLElement;
  synchronizePublishControlValues(current, next);
  assert.equal(token.value, "retained-secret");
  next.dataset.credentialId = "credential-b";
  synchronizePublishControlValues(current, next);
  assert.equal(token.value, "");
});

test("late clipboard failure cannot annotate a different profile or closed settings surface", async () => {
  await withContext(async () => projection(), async ({ context, local, token, view }) => {
    const clipboard = deferred<void>();
    navigator.clipboard.writeText = async () => clipboard.promise;
    token.value = "temporary-token";
    const operation = copyPublish(context, false);
    view.overlay = "none";
    local.notice = "new owner notice";
    clipboard.reject(new Error("clipboard unavailable"));
    await operation;
    assert.equal(local.error, "");
    assert.equal(local.notice, "new owner notice");
  });
});

test("delete requires explicit inline confirmation and new-profile creation never sends a command", async () => {
  const calls: string[] = [];
  await withContext(async (name) => { calls.push(name); return projection({ revision: "2", generation: "2", profiles: [] }); }, async ({ context, local }) => {
    addPublish(context);
    assert.deepEqual(calls, []);
    choosePublish(context, "profile-a");
    await operatePublish(context, "delete");
    assert.equal(local.deleteConfirmation, "profile-a");
    assert.deepEqual(calls, []);
    await operatePublish(context, "delete");
    assert.deepEqual(calls, ["mcp_publish_delete"]);
    assert.equal(local.selectedId, null);
  });
});

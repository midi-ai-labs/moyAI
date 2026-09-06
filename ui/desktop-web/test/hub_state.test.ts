import assert from "node:assert/strict";
import test from "node:test";

import { renderHubOverlay } from "../src/hub_render.ts";
import {
  acceptHubProjection, createHubUiState, editHubField, hubCanSave,
  hubCanSetRouteMode, hubDraftHasChanges, hubDraftTargetIsCurrent, hubExecutionRoute, hubRouteModeBlocker, hubSaveFeedback, hubSelectionError, hubSelectionFromDraft,
  type HubProjection,
} from "../src/hub_state.ts";

function projection(overrides: Partial<HubProjection> = {}): HubProjection {
  return {
    settings_revision: "1", connection_generation: "1", status: "connected",
    endpoint: "http://127.0.0.1:9470", label: "Desktop", hub_id: "hub-a",
    catalog: {
      hub_id: "hub-a", software_version: "0.1.0", revision: "7", changes: [],
      models: [
        { id: "model-a", label: "Model A", capabilities: ["tools"] },
        { id: "model-b", label: "Model B", capabilities: ["vision"] },
      ],
    },
    main_review: null, side_chat_review: null,
    main_confirmation: "unconfirmed", side_chat_confirmation: "unconfirmed", error: null,
    main_mode: "direct", side_chat_mode: "direct", active_main: null, active_side_chat: null,
    can_enable_main_hub: true, can_enable_side_chat_hub: true, can_change_main_mode: true, can_change_side_chat_mode: true,
    ...overrides,
  };
}

test("Hub route controls require independent confirmation and retain explicit Direct recovery", () => {
  const state = createHubUiState();
  acceptHubProjection(state, projection());
  assert.equal(hubCanSetRouteMode(state, "main", "hub"), false);
  assert.match(hubRouteModeBlocker(state, "main", "hub")!, /確認・保存/);
  acceptHubProjection(state, projection({ main_confirmation: "confirmed" }));
  assert.equal(hubCanSetRouteMode(state, "main", "hub"), true);
  assert.equal(hubCanSetRouteMode(state, "side_chat", "hub"), false);
  editHubField(state, "main:affinity", "3", false);
  assert.equal(hubCanSetRouteMode(state, "main", "hub"), false, "unsaved review cannot silently become the active route");
  acceptHubProjection(state, projection({ main_mode: "hub", status: "disconnected", main_confirmation: "unconfirmed" }));
  assert.equal(hubCanSetRouteMode(state, "main", "direct"), true);
  assert.equal(state.projection!.main_mode, "hub", "disconnect does not select Direct");
  assert.equal(state.drafts.main.affinityText, "3");
});

test("backend route eligibility blocks unsupported or active contexts without locking the other channel", () => {
  const state = createHubUiState();
  acceptHubProjection(state, projection({
    main_confirmation: "confirmed", side_chat_confirmation: "confirmed",
    can_enable_main_hub: false,
  }));
  assert.equal(hubCanSetRouteMode(state, "main", "hub"), false);
  assert.equal(hubCanSetRouteMode(state, "side_chat", "hub"), true);
  acceptHubProjection(state, projection({
    main_mode: "hub", main_confirmation: "confirmed", side_chat_confirmation: "confirmed",
    can_change_main_mode: false, active_main: { turn_id: "turn-a", phase: "waiting", logical_model_id: null },
  }));
  assert.equal(hubCanSetRouteMode(state, "main", "direct"), false);
  assert.match(hubRouteModeBlocker(state, "main", "direct")!, /待機・実行/);
  assert.equal(hubCanSetRouteMode(state, "side_chat", "hub"), true);
});

test("runtime labels distinguish preferred versus allocated model and never present Direct as Hub fallback", () => {
  const state = projection({ main_mode: "hub", main_confirmation: "confirmed",
    main_review: { hub_id: "hub-a", reviewed_revision: "7", selection: {
      allowed_model_ids: ["model-a", "model-b"], preferred_model_id: "model-a",
      required_capabilities: [], wait_policy: "allow_selected_fallback", affinity_turns: 1,
    } },
  });
  assert.equal(hubExecutionRoute(state, "side_chat"), null);
  assert.equal(hubExecutionRoute(state, "main")!.modelLabel, "優先モデル: Model A");
  state.active_main = { turn_id: "turn-a", phase: "waiting", logical_model_id: null };
  assert.equal(hubExecutionRoute(state, "main")!.phaseLabel, "Hubの実行枠を待っています");
  state.active_main = { turn_id: "turn-a", phase: "running", logical_model_id: "model-b" };
  assert.equal(hubExecutionRoute(state, "main")!.modelLabel, "使用中: Model B");
  assert.equal(hubExecutionRoute(state, "main")!.phaseLabel, null, "existing run phase keeps detailed tool/provider progress");
  state.status = "disconnected";
  assert.equal(hubExecutionRoute(state, "main")!.blockedReason, null, "an admitted active turn keeps its captured route");
  state.active_main = null;
  assert.match(hubExecutionRoute(state, "main")!.blockedReason!, /Hubのまま/);
});

test("Hub polling preserves an edited selection and its original revision until explicit refresh", () => {
  const state = createHubUiState();
  const first = projection();
  acceptHubProjection(state, first);
  editHubField(state, "main:model:model-a", "", true);
  editHubField(state, "main:affinity", "12", false);
  const target = structuredClone(state.drafts.main.target);
  assert.equal(hubCanSave(state, "main"), true);
  const newer = projection({ catalog: { ...first.catalog!, revision: "8" } });
  acceptHubProjection(state, newer);
  assert.deepEqual(state.drafts.main.target, target);
  assert.equal(state.drafts.main.affinityText, "12");
  assert.equal(hubCanSave(state, "main"), false);
  assert.match(renderHubOverlay(state), /モデル情報が更新されました/);
  acceptHubProjection(state, newer, { refreshTargets: true });
  assert.equal(hubCanSave(state, "main"), true);
  assert.equal(state.drafts.main.target!.expectedCatalogRevision, "8");
  assert.equal(state.drafts.main.affinityText, "12");
});

test("an acknowledged local save advances only the untouched draft settings baseline", () => {
  const state = createHubUiState();
  const before = projection();
  acceptHubProjection(state, before);
  editHubField(state, "main:model:model-a", "", true);
  editHubField(state, "side_chat:model:model-b", "", true);
  const side = structuredClone(state.drafts.side_chat);
  acceptHubProjection(state, projection({
    settings_revision: "2", main_confirmation: "confirmed",
    main_review: { hub_id: "hub-a", reviewed_revision: "7", selection: structuredClone(state.drafts.main.selection) },
  }), { savedContext: "main", localSave: { before, context: "main", kind: "review" } });
  assert.equal(state.drafts.main.dirty, false);
  assert.deepEqual(state.drafts.side_chat, { ...side, target: { ...side.target, expectedSettingsRevision: "2" } });
  assert.equal(hubCanSave(state, "side_chat"), true);
  assert.equal(hubCanSave(state, "main"), false);
  assert.deepEqual(state.drafts.side_chat.selection.allowed_model_ids, ["model-b"]);
  assert.equal(state.projection!.side_chat_confirmation, "unconfirmed");
});

test("local save receipt survives a passive poll arriving before the command result", () => {
  const state = createHubUiState();
  const before = projection();
  acceptHubProjection(state, before);
  editHubField(state, "main:model:model-a", "", true);
  editHubField(state, "side_chat:model:model-b", "", true);
  const after = projection({ settings_revision: "2", side_chat_confirmation: "confirmed",
    side_chat_review: { hub_id: "hub-a", reviewed_revision: "7", selection: structuredClone(state.drafts.side_chat.selection) } });
  acceptHubProjection(state, after);
  assert.equal(hubCanSave(state, "main"), false, "passive projection alone never rebases");
  acceptHubProjection(state, after, { savedContext: "side_chat", localSave: { before, context: "side_chat", kind: "review" } });
  assert.equal(hubCanSave(state, "main"), true);
  assert.equal(state.drafts.main.dirty, true);
  assert.deepEqual(state.drafts.main.selection.allowed_model_ids, ["model-a"]);
});

test("a newer catalog poll cannot be rolled back by a delayed local save receipt", () => {
  const state = createHubUiState();
  const before = projection({ connection_generation: "2" });
  acceptHubProjection(state, before);
  editHubField(state, "main:model:model-a", "", true);
  editHubField(state, "side_chat:model:model-b", "", true);
  editHubField(state, "side_chat:affinity", "9", false);
  const drafts = structuredClone(state.drafts);
  const receipt = { ...before, settings_revision: "2", main_confirmation: "confirmed" as const,
    main_review: { hub_id: "hub-a", reviewed_revision: "7", selection: structuredClone(state.drafts.main.selection) } };
  const newer = { ...receipt, main_confirmation: "review_required" as const,
    catalog: { ...before.catalog!, revision: "8" }, can_enable_main_hub: false };
  acceptHubProjection(state, newer);
  assert.equal(acceptHubProjection(state, receipt, {
    savedContext: "main", localSave: { before, context: "main", kind: "review" },
  }), false);
  assert.equal(state.projection, newer);
  assert.deepEqual(state.drafts, drafts);
  assert.equal(state.projection!.main_confirmation, "review_required");
  assert.equal(hubCanSave(state, "main"), false);
  assert.equal(hubCanSave(state, "side_chat"), false);
  assert.match(hubSaveFeedback(state, "side_chat"), /モデル情報が更新されました/);
  acceptHubProjection(state, newer, { refreshTargets: true });
  assert.equal(hubCanSave(state, "side_chat"), true);
  assert.equal(state.drafts.side_chat.affinityText, "9");
});

test("a delayed local save receipt cannot replace newer same-revision lifecycle or runtime state", () => {
  for (const update of ["stale", "unauthorized", "main_running", "side_running", "other_confirmation"] as const) {
    const state = createHubUiState();
    const before = projection({ connection_generation: "2" });
    acceptHubProjection(state, before);
    editHubField(state, "main:model:model-a", "", true);
    editHubField(state, "side_chat:model:model-b", "", true);
    const drafts = structuredClone(state.drafts);
    const receipt = { ...before, settings_revision: "2", main_confirmation: "confirmed" as const,
      main_review: { hub_id: "hub-a", reviewed_revision: "7", selection: structuredClone(state.drafts.main.selection) } };
    const newer = structuredClone(receipt);
    if (update === "stale") { newer.status = "stale"; newer.error = "unavailable"; }
    if (update === "unauthorized") { newer.status = "error"; newer.error = "unauthorized"; }
    if (update === "main_running") newer.active_main = { turn_id: "main-turn", phase: "running", logical_model_id: "model-a" };
    if (update === "side_running") newer.active_side_chat = { turn_id: "side-turn", phase: "running", logical_model_id: "model-b" };
    if (update === "other_confirmation") newer.side_chat_confirmation = "confirmed";
    acceptHubProjection(state, newer);
    assert.equal(acceptHubProjection(state, receipt, {
      savedContext: "main", localSave: { before, context: "main", kind: "review" },
    }), false, update);
    assert.equal(state.projection, newer, update);
    assert.deepEqual(state.drafts, drafts, update);
    assert.equal(hubCanSave(state, "side_chat"), false, update);
  }
});

test("a local review acknowledgement may confirm only its own pre-acknowledgement poll", () => {
  const state = createHubUiState();
  const before = projection({ connection_generation: "2", can_enable_main_hub: false });
  acceptHubProjection(state, before);
  editHubField(state, "main:model:model-a", "", true);
  editHubField(state, "side_chat:model:model-b", "", true);
  const receipt = { ...before, settings_revision: "2", main_confirmation: "confirmed" as const, can_enable_main_hub: true,
    main_review: { hub_id: "hub-a", reviewed_revision: "7", selection: structuredClone(state.drafts.main.selection) } };
  acceptHubProjection(state, { ...receipt, main_confirmation: "unconfirmed", can_enable_main_hub: false });
  assert.equal(hubCanSave(state, "side_chat"), false);
  assert.equal(acceptHubProjection(state, receipt, {
    savedContext: "main", localSave: { before, context: "main", kind: "review" },
  }), true);
  assert.equal(state.projection!.main_confirmation, "confirmed");
  assert.equal(state.drafts.main.dirty, false);
  assert.equal(state.drafts.side_chat.dirty, true);
  assert.equal(hubCanSave(state, "side_chat"), true);
});

test("local save receipt does not rebase external settings, catalog changes, connection ABA, or an already-stale draft", () => {
  for (const conflict of ["external_revision", "catalog", "catalog_content", "connection_aba", "other_review", "mode", "stale_draft"] as const) {
    const state = createHubUiState();
    const before = projection();
    acceptHubProjection(state, before);
    editHubField(state, "main:model:model-a", "", true);
    editHubField(state, "side_chat:model:model-b", "", true);
    if (conflict === "stale_draft") state.drafts.side_chat.target!.expectedSettingsRevision = "0";
    const target = structuredClone(state.drafts.side_chat.target);
    const after = projection({ settings_revision: "2", main_confirmation: "confirmed",
      main_review: { hub_id: "hub-a", reviewed_revision: "7", selection: structuredClone(state.drafts.main.selection) } });
    if (conflict === "external_revision") after.settings_revision = "3";
    if (conflict === "catalog") after.catalog!.revision = "8";
    if (conflict === "catalog_content") after.catalog!.models[0].label = "更新された表示名";
    if (conflict === "connection_aba") after.connection_generation = "3";
    if (conflict === "mode") after.side_chat_mode = "hub";
    if (conflict === "other_review") after.side_chat_review = { hub_id: "hub-a", reviewed_revision: "7", selection: structuredClone(state.drafts.main.selection) };
    acceptHubProjection(state, after, { savedContext: "main", localSave: { before, context: "main", kind: "review" } });
    assert.deepEqual(state.drafts.side_chat.target, target, conflict);
    assert.equal(hubCanSave(state, "side_chat"), false, conflict);
  }
});

test("unchanged confirmed selection is saved, edits reverted to the saved value do not enable save, reconnect still requires review", () => {
  const state = createHubUiState();
  const first = projection();
  acceptHubProjection(state, first);
  editHubField(state, "main:model:model-a", "", true);
  const saved = projection({ settings_revision: "2", main_confirmation: "confirmed",
    main_review: { hub_id: "hub-a", reviewed_revision: "7", selection: structuredClone(state.drafts.main.selection) } });
  acceptHubProjection(state, saved, { savedContext: "main" });
  assert.equal(hubCanSave(state, "main"), false);
  assert.match(hubSaveFeedback(state, "main"), /保存済み/);
  assert.match(renderHubOverlay(state), /data-action="hub-save-main"[^>]*disabled>保存済み/);
  editHubField(state, "main:affinity", "4", false);
  assert.equal(hubCanSave(state, "main"), true);
  editHubField(state, "main:affinity", "1", false);
  assert.equal(hubDraftHasChanges(state, "main"), false);
  assert.equal(hubCanSave(state, "main"), false);
  acceptHubProjection(state, { ...saved, main_confirmation: "unconfirmed" });
  assert.equal(hubCanSave(state, "main"), true);
  assert.match(hubSaveFeedback(state, "main"), /現在のHubでは未確認/);
});

test("stale draft guidance distinguishes settings changes from catalog and connection changes", () => {
  const state = createHubUiState();
  acceptHubProjection(state, projection());
  editHubField(state, "main:model:model-a", "", true);
  acceptHubProjection(state, projection({ settings_revision: "2" }));
  assert.match(hubSaveFeedback(state, "main"), /別の設定変更/);
  assert.doesNotMatch(hubSaveFeedback(state, "main"), /モデル情報が更新/);
  acceptHubProjection(state, projection({ settings_revision: "2", connection_generation: "2" }));
  assert.match(hubSaveFeedback(state, "main"), /接続状態が変わりました/);
});

test("connection authentication feedback is adjacent to the connection action and describes the token field", () => {
  const state = createHubUiState();
  acceptHubProjection(state, projection({ status: "error", error: "unauthorized" }));
  const html = renderHubOverlay(state);
  const connection = html.slice(html.indexOf('<section class="hub-connection"'), html.indexOf('<div class="hub-channels"'));
  assert.match(connection, /id="hub-connection-feedback"[^>]*>Hubの認証に失敗/);
  assert.match(connection, /id="hub-token"[^>]*aria-describedby="hub-connection-feedback"/);
  assert.match(connection, /data-action="hub-connect"[^>]*aria-describedby="hub-connection-feedback"/);
  assert.ok(connection.indexOf('id="hub-connection-feedback"') < connection.indexOf('<p class="hub-help">'));
});

test("late old generations and old settings cannot overwrite the current connection or edited draft", () => {
  const state = createHubUiState();
  const current = projection({ connection_generation: "9007199254740993", settings_revision: "8" });
  acceptHubProjection(state, current);
  editHubField(state, "main:model:model-a", "", true);
  assert.equal(acceptHubProjection(state, projection({ connection_generation: "9007199254740992", settings_revision: "9" })), false);
  assert.equal(acceptHubProjection(state, projection({ connection_generation: "9007199254740994", settings_revision: "7" })), false);
  assert.equal(state.projection, current);
  assert.deepEqual(state.drafts.main.selection.allowed_model_ids, ["model-a"]);
  acceptHubProjection(state, { ...current, connection_generation: "9007199254740994" });
  assert.equal(hubDraftTargetIsCurrent(state, "main"), false);
});

test("catalog revisions stay monotonic within one connection without blocking null catalogs or reconnects", () => {
  const state = createHubUiState();
  const first = projection();
  const current = { ...first, catalog: { ...first.catalog!, revision: "9007199254740993" } };
  acceptHubProjection(state, current);
  assert.equal(acceptHubProjection(state, { ...current,
    catalog: { ...current.catalog, revision: "9007199254740992" } }), false);
  assert.equal(state.projection, current);
  const disconnected = { ...current, connection_generation: "2", status: "disconnected" as const, catalog: null };
  assert.equal(acceptHubProjection(state, disconnected), true);
  assert.equal(state.projection!.catalog, null);
  assert.equal(acceptHubProjection(state, current), false, "a late catalog cannot restore a revoked connection");
  const connecting = { ...disconnected, connection_generation: "3", status: "connecting" as const };
  assert.equal(acceptHubProjection(state, connecting), true);
  const reconnected = { ...first, connection_generation: "3" };
  assert.equal(acceptHubProjection(state, reconnected), true);
  assert.equal(state.projection!.catalog!.revision, "7");
  assert.equal(acceptHubProjection(state, { ...reconnected, status: "error", catalog: null }), true);
  assert.equal(state.projection!.catalog, null, "a catalog-less lifecycle result is not an older catalog");
});

test("an explicit different-Hub connection clears both old Hub drafts", () => {
  const state = createHubUiState();
  acceptHubProjection(state, projection());
  editHubField(state, "main:model:model-a", "", true);
  editHubField(state, "side_chat:model:model-b", "", true);
  const next = projection();
  acceptHubProjection(state, { ...next, hub_id: "hub-b", catalog: { ...next.catalog!, hub_id: "hub-b" } }, { connected: true });
  assert.deepEqual(state.drafts.main.selection.allowed_model_ids, []);
  assert.deepEqual(state.drafts.side_chat.selection.allowed_model_ids, []);
  assert.equal(state.drafts.main.dirty, false);
});

test("capability intersection, removed models and preferred-wait restrictions block invalid review", () => {
  const state = createHubUiState();
  acceptHubProjection(state, projection());
  editHubField(state, "main:model:model-a", "", true);
  editHubField(state, "main:model:model-b", "", true);
  editHubField(state, "main:capabilities", "tools, vision", false);
  assert.match(hubSelectionError(state.drafts.main, state.projection!.catalog)!, /満たすものがありません/);
  editHubField(state, "main:capabilities", "vision", false);
  assert.match(hubSelectionError(state.drafts.main, state.projection!.catalog)!, /優先モデル/);
  editHubField(state, "main:wait", "allow_selected_fallback", false);
  assert.equal(hubCanSave(state, "main"), true);
  editHubField(state, "main:model:model-removed", "", true);
  assert.match(hubSelectionError(state.drafts.main, state.projection!.catalog)!, /削除/);
  editHubField(state, "main:affinity", "101", false);
  assert.equal(hubSelectionFromDraft(state.drafts.main), null);
});

test("connection drafts survive polling and tokens never enter UI state or rendered HTML", () => {
  const state = createHubUiState();
  editHubField(state, "endpoint", "127.0.0.1:5000", false);
  editHubField(state, "label", "編集している端末", false);
  editHubField(state, "token", "secret-never-persist", false);
  acceptHubProjection(state, projection({ status: "disconnected" }));
  assert.equal(state.endpoint, "127.0.0.1:5000");
  assert.equal(state.label, "編集している端末");
  assert.doesNotMatch(JSON.stringify(state), /secret-never-persist/);
  const html = renderHubOverlay(state);
  assert.doesNotMatch(html, /secret-never-persist/);
  assert.match(html, /id="hub-token"[^>]*type="password"/);
  assert.doesNotMatch(html.match(/<input id="hub-token"[^>]*>/)![0], /value=/);
});

test("Hub catalog labels and IDs are escaped in markup, and disconnected reviews cannot be saved", () => {
  const state = createHubUiState();
  const current = projection();
  current.catalog!.models[0] = { id: 'model-"<script>', label: '<img onerror="bad">', capabilities: ["tools"] };
  acceptHubProjection(state, current);
  editHubField(state, 'main:model:model-"<script>', "", true);
  const html = renderHubOverlay(state);
  assert.doesNotMatch(html, /<script>|<img onerror/);
  assert.match(html, /&lt;img onerror=/);
  acceptHubProjection(state, { ...current, status: "disconnected" });
  assert.equal(hubCanSave(state, "main"), false);
});

test("saved models remain unverified before catalog retrieval and become missing only after a successful catalog", () => {
  const state = createHubUiState();
  acceptHubProjection(state, projection({
    status: "disconnected", catalog: null,
    main_review: { hub_id: "hub-a", reviewed_revision: "7", selection: {
      allowed_model_ids: ["saved-model"], preferred_model_id: "saved-model", required_capabilities: [],
      wait_policy: "wait_for_preferred", affinity_turns: 4,
    } },
  }));
  const disconnected = renderHubOverlay(state);
  assert.match(disconnected, /保存済みのモデル/);
  assert.doesNotMatch(disconnected, /削除されたモデル|hub-model-row is-missing/);
  assert.equal(hubCanSave(state, "main"), false);
  const current = projection();
  acceptHubProjection(state, { ...state.projection!, status: "connected", catalog: current.catalog });
  assert.match(renderHubOverlay(state), /削除されたモデル/);
  assert.match(renderHubOverlay(state), /hub-model-row is-missing/);
  assert.equal(hubCanSave(state, "main"), false);
});

import assert from "node:assert/strict";
import test from "node:test";

import {
  HUB_FIXTURE_ID, HubSettingsFixture, createHubConnectionSettingsScenario,
  hubAdvancedControlsReady, hubConnectedLayoutFailures, hubContextConfirmed, hubDraftRetained, hubPersistenceFailures, hubRestartPreferencesReady,
} from "../scenarios/hub_connection_settings.mjs";

function selection(model = "e2e-main", affinity = 9) {
  return { allowed_model_ids: [model], preferred_model_id: model, required_capabilities: [],
    wait_policy: "wait_for_preferred", affinity_turns: affinity };
}
function review(model = "e2e-main", revision = "8", affinity = 9) {
  return { hub_id: HUB_FIXTURE_ID, reviewed_revision: revision, selection: selection(model, affinity) };
}
function durable() {
  return { schema_version: 2, revision: "4", endpoint: "http://127.0.0.1:43210/", label: "Desktop E2E device", hub_id: HUB_FIXTURE_ID,
    main_mode: "direct", side_chat_mode: "direct",
    main_review: review(), side_chat_review: review("e2e-side", "7", 2) };
}
function confirmedSurface() {
  return { hub: { status: "connected", main_confirmation: "confirmed", side_chat_confirmation: "review_required",
    main_review: review(), side_chat_review: review("e2e-side", "7", 2) },
  main: { confirmation: "Hubで確認済み 更新番号 8" }, fatal_count: 0 };
}
async function request(fixture, token, pathname, body = null) {
  const response = await fetch(`${fixture.baseUrl}${pathname}`, {
    method: body === null ? "GET" : "POST",
    headers: { Authorization: `Bearer ${token}`, ...(body === null ? {} : { "Content-Type": "application/json" }) },
    ...(body === null ? {} : { body: JSON.stringify(body) }),
  });
  return { status: response.status, body: await response.json() };
}

test("Hub scenario uses the shared execution lifecycle with a bounded public name", () => {
  const scenario = createHubConnectionSettingsScenario();
  assert.equal(scenario.id, "hub.connection-settings");
  assert.equal(scenario.productOracle, "pass");
  assert.equal(scenario.manualGate, "not_required");
  for (const operation of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) assert.equal(typeof scenario[operation], "function");
  assert.equal("launch" in scenario, false);
});

test("Connected layout rejects clipped footer, oversized checkbox, missing label and unreachable close", () => {
  const rectangle = { left: 100, top: 750, right: 900, bottom: 800, width: 800, height: 50, visible: true, center_hit: true };
  const row = { id: "logical-model", model_id: "logical-model", model_label: "Readable model", type: "checkbox", enabled: true,
    label_present: true, checkbox: { ...rectangle, width: 18, height: 18 } };
  const ready = { viewport: { width: 1280, height: 900 }, footer: rectangle, footer_close: { ...rectangle, left: 800, width: 100 },
    hub: { catalog: { models: [{ id: "logical-model", label: "Readable model" }] } },
    main: { models: [row] }, side_chat: { models: [structuredClone(row)] } };
  assert.deepEqual(hubConnectedLayoutFailures(ready), []);
  const cases = [
    [(value) => { value.footer.bottom = 1100; }, "footer-outside-viewport"],
    [(value) => { value.footer_close.center_hit = false; }, "footer-close-unreachable"],
    [(value) => { value.main.models[0].checkbox.width = 414; }, "main-logical-model-checkbox-size-or-state"],
    [(value) => { value.main.models[0].checkbox.height = 40; }, "main-logical-model-checkbox-size-or-state"],
    [(value) => { value.main.models[0].checkbox.width = Number.NaN; }, "main-logical-model-checkbox-size-or-state"],
    [(value) => { value.side_chat.models[0].model_label = ""; }, "side_chat-logical-model-model-label-missing"],
    [(value) => { value.side_chat.models[0].label_present = false; }, "side_chat-logical-model-model-label-missing"],
    [(value) => { value.side_chat.models[0].id = "other-model"; }, "side_chat-logical-model-model-row-identity"],
    [(value) => { value.main.models = []; }, "main-model-row-count"],
  ];
  for (const [change, expectedFailure] of cases) {
    const invalid = structuredClone(ready);
    change(invalid);
    assert.ok(hubConnectedLayoutFailures(invalid).includes(expectedFailure));
  }
});

test("Hub context confirmation requires the exact independent logical selection and visible confirmation", () => {
  const ready = confirmedSurface();
  assert.equal(hubContextConfirmed(ready, "main", "8", "e2e-main", 9), true);
  assert.equal(hubContextConfirmed(ready, "side_chat", "7", "e2e-side", 2), false);
  for (const change of [
    (value) => { value.hub.main_confirmation = "unconfirmed"; },
    (value) => { value.hub.main_review.reviewed_revision = "7"; },
    (value) => { value.hub.main_review.selection.allowed_model_ids = ["e2e-side"]; },
    (value) => { value.hub.main_review.selection.preferred_model_id = "e2e-side"; },
    (value) => { value.main.confirmation = "保存済み・Hubで未確認"; },
    (value) => { value.fatal_count = 1; },
  ]) {
    const invalid = structuredClone(ready);
    change(invalid);
    assert.equal(hubContextConfirmed(invalid, "main", "8", "e2e-main", 9), false);
  }
});

test("Advanced controls require an expanded unique details owner and a visible enabled editor", () => {
  const ready = { details_count: 1, summary_count: 1, open: true, affinity_count: 1, affinity_visible: true, affinity_enabled: true };
  assert.equal(hubAdvancedControlsReady(ready), true);
  for (const change of [
    { details_count: 2 }, { summary_count: 0 }, { open: false },
    { affinity_count: 0 }, { affinity_visible: false }, { affinity_enabled: false },
  ]) assert.equal(hubAdvancedControlsReady({ ...ready, ...change }), false);
});

test("Catalog drift oracle rejects a lost draft, replaced DOM owner, focus loss or prematurely enabled save", () => {
  const ready = { hub: { catalog: { revision: "8" }, main_confirmation: "review_required", side_chat_confirmation: "review_required" },
    main: { affinity: "9", save_enabled: false, feedback: "最新情報を取得してください" }, draft_active: true, draft_same_node: true, fatal_count: 0 };
  assert.equal(hubDraftRetained(ready, "8", "9"), true);
  for (const change of [
    (value) => { value.main.affinity = "4"; },
    (value) => { value.draft_same_node = false; },
    (value) => { value.draft_active = false; },
    (value) => { value.main.save_enabled = true; },
    (value) => { value.hub.side_chat_confirmation = "confirmed"; },
    (value) => { value.main.feedback = ""; },
  ]) {
    const invalid = structuredClone(ready);
    change(invalid);
    assert.equal(hubDraftRetained(invalid, "8", "9"), false);
  }
});

test("Durable preferences oracle rejects credential fields and independent selection loss", () => {
  const expected = { endpoint: "http://127.0.0.1:43210" };
  assert.deepEqual(hubPersistenceFailures(durable(), expected), []);
  for (const change of [
    (value) => { value.client_token = "secret"; },
    (value) => { value.main_review.selection.endpoint = "http://provider"; },
    (value) => { value.side_chat_review = value.main_review; },
    (value) => { value.main_review.selection.affinity_turns = 4; },
    (value) => { value.hub_id = "different-hub"; },
    (value) => { value.main_mode = "hub"; },
  ]) {
    const invalid = durable();
    change(invalid);
    assert.notEqual(hubPersistenceFailures(invalid, expected).length, 0);
  }
});

test("Restart requires exact saved choices, empty credentials and unconfirmed disconnected UI", () => {
  const saved = durable();
  const surface = { overlay: "hub", dialog_count: 1, focused_inside: true, fatal_count: 0,
    hub: { status: "disconnected", catalog: null, error: null, settings_revision: saved.revision,
      endpoint: saved.endpoint, label: saved.label, hub_id: saved.hub_id,
      main_mode: saved.main_mode, side_chat_mode: saved.side_chat_mode,
      main_review: saved.main_review, side_chat_review: saved.side_chat_review, main_confirmation: "unconfirmed", side_chat_confirmation: "unconfirmed" },
    endpoint_value: saved.endpoint, label_value: saved.label, token_empty: true, token_password: true, token_value_attribute: null,
    main: { selected: ["e2e-main"], affinity: "9", save_enabled: false, confirmation: "保存済み・Hubで未確認", models: [{ model_label: "保存済みのモデル", marked_missing: false }] },
    side_chat: { selected: ["e2e-side"], affinity: "2", save_enabled: false, confirmation: "保存済み・Hubで未確認", models: [{ model_label: "保存済みのモデル", marked_missing: false }] } };
  assert.equal(hubRestartPreferencesReady(surface, saved), true);
  for (const change of [
    (value) => { value.hub.status = "connected"; },
    (value) => { value.hub.catalog = { revision: "8" }; },
    (value) => { value.hub.main_confirmation = "confirmed"; },
    (value) => { value.hub.main_mode = "hub"; },
    (value) => { value.hub.side_chat_review.reviewed_revision = "8"; },
    (value) => { value.main.selected = []; },
    (value) => { value.side_chat.affinity = "9"; },
    (value) => { value.token_empty = false; },
    (value) => { value.token_value_attribute = "restored-secret"; },
    (value) => { value.endpoint_value = "http://wrong-host/"; },
    (value) => { value.hub.settings_revision = "999"; },
    (value) => { value.main.save_enabled = true; },
    (value) => { value.main.models[0].model_label = "削除されたモデル"; },
    (value) => { value.side_chat.models[0].marked_missing = true; },
  ]) {
    const invalid = structuredClone(surface);
    change(invalid);
    assert.equal(hubRestartPreferencesReady(invalid, saved), false);
  }
});

test("Hub wire fixture enforces client ownership, independent review, revision gate and credential-free ledger", async (t) => {
  const fixture = await new HubSettingsFixture().start();
  t.after(async () => { const closed = await fixture.close(); assert.equal(closed.pass, true); assert.equal(closed.forced_connection_count, 0); });
  const registration = await request(fixture, fixture.bootstrapToken, "/v1/clients/register", { label: "E2E Main + Side" });
  assert.equal(registration.status, 200);
  const { id, client_token: token } = registration.body;
  assert.equal(registration.body.hub_id, HUB_FIXTURE_ID);
  assert.equal(registration.body.revision, "7");
  assert.equal(registration.body.identity_scope, "server_session");
  const catalog = await request(fixture, token, "/v1/catalog");
  assert.equal(catalog.status, 200);
  assert.equal(catalog.body.models.length, 2);
  const body = { id, context: "main", expected_hub_id: HUB_FIXTURE_ID, reviewed_revision: "7", selection: selection() };
  assert.equal((await request(fixture, fixture.bootstrapToken, "/v1/clients/review", body)).status, 401);
  assert.equal((await request(fixture, token, "/v1/clients/review", { ...body, id: "other-device" })).status, 401);
  assert.equal((await request(fixture, token, "/v1/clients/review", body)).status, 200);
  assert.equal(fixture.reviewFor("side_chat"), null);
  assert.equal((await request(fixture, token, "/v1/clients/review", { ...body, context: "side_chat", selection: selection("e2e-side", 2) })).status, 200);
  fixture.advanceRevision();
  const stale = await request(fixture, token, "/v1/clients/review", body);
  assert.deepEqual(stale, { status: 409, body: { error: "review_required", current_revision: "8" } });
  assert.equal(fixture.reviewFor("main").reviewed_revision, "7");
  assert.equal(fixture.reviewFor("side_chat").selection.preferred_model_id, "e2e-side");
  assert.equal((await request(fixture, token, "/v1/clients/review", { ...body, reviewed_revision: "8" })).status, 200);
  assert.equal(fixture.reviewFor("side_chat").reviewed_revision, "7");
  assert.equal((await request(fixture, token, "/v1/clients/disconnect", { id })).status, 200);
  assert.equal(fixture.clientCount, 0);
  assert.equal((await request(fixture, token, "/v1/catalog")).status, 401);
  assert.equal(fixture.containsCredential(JSON.stringify(fixture.requestLedger)), false);
  const closed = await fixture.close();
  assert.equal(closed.pass, true);
  assert.deepEqual(await fixture.close(), closed);
});

test("Hub wire fixture rejects wrong identity, malformed review DTO, missing model and incompatible capability", async (t) => {
  const fixture = await new HubSettingsFixture().start();
  t.after(() => fixture.close());
  const { body: registered } = await request(fixture, fixture.bootstrapToken, "/v1/clients/register", { label: "negative matrix" });
  const token = registered.client_token;
  const body = { id: registered.id, context: "main", expected_hub_id: HUB_FIXTURE_ID, reviewed_revision: "7", selection: selection() };
  for (const [patch, error, status = 400] of [
    [{ expected_hub_id: "other-hub" }, "different_hub", 409],
    [{ reviewed_revision: "07" }, "invalid_request"],
    [{ reviewed_revision: 7 }, "invalid_request"],
    [{ context: "unbounded-surface" }, "invalid_request"],
    [{ extra: true }, "invalid_request"],
    [{ selection: { ...selection(), allowed_model_ids: [] } }, "invalid_selection"],
    [{ selection: selection("removed-model") }, "model_removed"],
    [{ selection: { ...selection(), required_capabilities: ["vision"] } }, "capability_mismatch"],
  ]) {
    const actual = await request(fixture, token, "/v1/clients/review", { ...body, ...patch });
    assert.equal(actual.status, status);
    assert.equal(actual.body.error, error);
    assert.equal(fixture.reviewFor("main"), null);
  }
});

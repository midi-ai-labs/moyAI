import assert from "node:assert/strict";
import test from "node:test";

import {
  DOCLING_READINESS_CONTEXT_WINDOW,
  DOCLING_READINESS_HTTP_STATUS,
  checkingDoclingReadinessReady,
  createSettingsDoclingReadinessScenario,
  doclingReadinessFixtureConfig,
  exactCompletedDoclingReadinessLedger,
  exactHeldDoclingReadinessLedger,
  expectedDoclingReadinessCommand,
  idleDoclingReadinessReady,
  terminalDoclingReadinessReady,
} from "../scenarios/settings_docling_readiness.mjs";

const target = Object.freeze({
  workspacePath: "C:\\workspace",
  sessionId: null,
  configGeneration: "17",
});
const endpoint = "http://127.0.0.1:43111/ready";

function requestRow(overrides = {}) {
  return {
    sequence: 1,
    method: "GET",
    pathname: "/ready",
    query_present: false,
    route: "docling_readiness",
    contract: { pass: true, expected_method: "GET", expected_pathname: "/ready" },
    response_phase: "held",
    response_status: null,
    ...overrides,
  };
}

function surface(status = "idle", httpStatus = null, overrides = {}) {
  const terminal = status === "ready" || status === "unavailable";
  return {
    projection: {
      overlay: "config",
      config_target: { ...target },
      config_fields: [
        { key: "model.context_window", value: DOCLING_READINESS_CONTEXT_WINDOW },
        { key: "docling.enabled", value: "true" },
        { key: "docling.base_url", value: "http://127.0.0.1:43111" },
      ],
      docling_readiness: {
        status,
        endpoint: status === "idle" ? "" : endpoint,
        httpStatus,
        message: terminal ? `Docling /ready returned HTTP ${httpStatus}.` : "Checking Docling /ready...",
      },
      pending_async_operations: status === "checking" ? ["docling_readiness_check"] : [],
    },
    settings: {
      dialog_count: 1,
      dialog_visible: true,
      docling: { count: 1, checked: true },
      docling_label: { count: 1, visible: true, text: "Docling を有効化" },
      dirty_badge_visible: false,
      save: { count: 1, enabled: false },
      discard: { count: 0 },
      close: { count: 1 },
      docling_readiness: {
        button: { count: 1, visible: true, enabled: status !== "checking" },
        status_count: 1,
        status_visible: true,
        status,
        aria_busy: String(status === "checking"),
        text: terminal ? `Docling /ready returned HTTP ${httpStatus}.` : "Docling readiness status",
      },
    },
    close_confirmation: { count: 0 },
    visible_dialog_count: 1,
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
    visible_validation_error_count: 0,
    ...overrides,
  };
}

test("Docling readiness fixture uses one enabled loopback target without implicit startup traffic", () => {
  const config = doclingReadinessFixtureConfig("http://127.0.0.1:43111");
  assert.equal((config.match(/base_url = "http:\/\/127\.0\.0\.1:43111"/g) ?? []).length, 2);
  assert.match(config, /\[docling\]\nenabled = true/);
  assert.match(config, /context_window = 65536/);
  assert.match(config, /timeout_ms = 5000/);
  assert.doesNotMatch(config, /max_(?:output_)?tokens|reasoning_(?:effort|summary)|supports_reasoning|temperature|top_p|top_k|presence_penalty|frequency_penalty|seed\s*=|stop(?:_sequences)?\s*=|\[model\.extra_body_json\]/);
});

test("Docling readiness ledger accepts only one exact held then completed GET", () => {
  const held = [requestRow()];
  const completed = [requestRow({ response_phase: "completed", response_status: DOCLING_READINESS_HTTP_STATUS })];
  assert.equal(exactHeldDoclingReadinessLedger(held), true);
  assert.equal(exactCompletedDoclingReadinessLedger(completed), true);
  assert.equal(exactHeldDoclingReadinessLedger([]), false);
  assert.equal(exactHeldDoclingReadinessLedger([...held, requestRow({ sequence: 2 })]), false);
  assert.equal(exactCompletedDoclingReadinessLedger([requestRow({
    response_phase: "completed",
    response_status: 204,
    pathname: "/v1/models",
  })]), false);
});

test("Docling readiness predicates bind exact target, typed checking, and ready or unavailable terminal state", () => {
  const idle = surface();
  assert.equal(idleDoclingReadinessReady(idle, [], target), true);
  assert.equal(idleDoclingReadinessReady(idle, [requestRow()], target), false);
  assert.equal(idleDoclingReadinessReady(idle, [], { ...target, configGeneration: "18" }), false);

  const held = [requestRow()];
  const checking = surface("checking");
  assert.equal(checkingDoclingReadinessReady(checking, held, target, endpoint), true);
  assert.equal(checkingDoclingReadinessReady({
    ...checking,
    settings: {
      ...checking.settings,
      docling_readiness: {
        ...checking.settings.docling_readiness,
        button: { count: 1, visible: true, enabled: true },
      },
    },
  }, held, target, endpoint), false);

  const completed = [requestRow({ response_phase: "completed", response_status: 204 })];
  const ready = surface("ready", 204);
  assert.equal(terminalDoclingReadinessReady(ready, completed, target, endpoint), true);
  assert.equal(terminalDoclingReadinessReady({ ...ready, visible_recoverable_error_count: 1 }, completed, target, endpoint), false);

  const unavailableLedger = [requestRow({ response_phase: "completed", response_status: 503 })];
  const unavailable = surface("unavailable", 503);
  assert.equal(terminalDoclingReadinessReady(unavailable, unavailableLedger, target, endpoint, 503), true);
});

test("Docling readiness command captures the exact current config target", () => {
  assert.deepEqual(expectedDoclingReadinessCommand(surface()), {
    command: "check_docling_readiness",
    args: { expectedTarget: target },
  });
  assert.throws(() => expectedDoclingReadinessCommand({ projection: { config_target: null } }), /requires a config target/);
});

test("Docling readiness scenario factory returns fresh common-runner contracts", () => {
  const first = createSettingsDoclingReadinessScenario();
  const second = createSettingsDoclingReadinessScenario();
  assert.notEqual(first, second);
  assert.equal(first.id, "settings.docling-readiness");
  assert.equal(first.productOracle, "pass");
  assert.equal(first.manualGate, "not_required");
  for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) {
    assert.equal(typeof first[method], "function", method);
  }
});

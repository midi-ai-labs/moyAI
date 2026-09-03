import assert from "node:assert/strict";
import crypto from "node:crypto";
import { createReadStream } from "node:fs";
import { createServer } from "node:http";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { mkdtemp, open, rename, rm, writeFile } from "node:fs/promises";

import {
  case52EvaluatorAccepted,
  case52EvaluatorFailures,
  case52NormalTerminalFailures,
  case52RestartPreviousPageTransitionFailures,
  case52RestartContinuityAccepted,
  case52RestartContinuityFailures,
  case52Stage1ManifestFailures,
  case52Stage2ManifestFailures,
  classifyCase52RestartHistoryTarget,
  classifyCase52NonConvergence,
  classifyCase52NormalTerminal,
  classifyCase52RestartContinuity,
  classifyCase52RestartTurnPage,
} from "../case5_2_predicates.mjs";
import {
  assertCase52PhysicalFileIdentity,
  case52EvaluatorWorkspaceDiff,
  case52EvidenceOptions,
  case52ExpectedMainGlobalSave,
  case52ExternalLmStudioObservation,
  case52ExtraBodyEvidence,
  case52ForbiddenWorkspacePaths,
  case52FixtureConfig,
  case52MainProviderSelectionKeys,
  case52NewRequestComposerSurfaceReady,
  case52NewTurnAcquisitionAccepted,
  case52PhysicalFileIdentity,
  case52ProviderHostFingerprint,
  case52LmStudioLoadedContext,
  case52ProviderControlTokenLeaks,
  case52ProviderControlTokenLeakEvidence,
  case52ProviderModelState,
  case52ProviderCleanupPlan,
  case52ProviderSummaryEvidence,
  case52SideProviderSummary,
  case52LegacySideSummaryV1,
  case52SideScreenshotSurfaceReady,
  case52ProviderControlTokenLeakFailure,
  classifyCase52MainSaveCommandError,
  classifyCase52MainPreferencesObservationError,
  classifyCase52SideScreenshotObservationError,
  normalizeCase52PromptText,
  normalizeCase52Options,
  readCase52ExternalOutput,
  loadMainProvider,
  providerMustKeepSideUnloaded,
  settleCase52MainCommandProbe,
  settleCase52WorkspaceEvaluator,
  unloadMainProvider,
  waitForCase52RestartHistoryTarget,
  createCase52Scenario,
} from "../scenarios/case5_2.mjs";

const SESSION_ID = "01K3CASE52SESSION0000000000";
const TURN_ID = "01K3CASE52TURN000000000000";

test("manual.case5_2 waits for the visible GUI composer to leave steer mode before Send", () => {
  const prompt = "implement stage 3";
  const runTarget = {
    workspacePath: "C:/workspace",
    sessionId: SESSION_ID,
    runtimeOwnerToken: "idle:2",
    permissionConfirmationId: null,
    expectedState: { kind: "idle", latestTurnId: TURN_ID, admissionRevision: "2" },
  };
  const ready = {
    composer_count: 1,
    prompt_count: 1,
    prompt_value: prompt,
    prompt_disabled: false,
    send_count: 1,
    send_disabled: false,
    send_title: "送信",
    send_aria_label: "送信",
    run_strip_count: 0,
    visible_stop_count: 0,
    rendered_run_target: runTarget,
    run_target_parse_error: null,
  };
  assert.equal(case52NewRequestComposerSurfaceReady(ready, prompt, runTarget), true);
  for (const drift of [
    { composer_count: 0 },
    { prompt_value: "stale draft" },
    { send_disabled: true },
    { send_title: "実行中のタスクへ追加指示を送信" },
    { send_aria_label: "実行中のタスクへ追加指示を送信" },
    { run_strip_count: 1 },
    { visible_stop_count: 1 },
    { rendered_run_target: { ...runTarget, runtimeOwnerToken: "idle:1" } },
    { rendered_run_target: { ...runTarget, expectedState: { ...runTarget.expectedState, admissionRevision: "1" } } },
    { rendered_run_target: null },
    { run_target_parse_error: "SyntaxError: malformed JSON" },
  ]) {
    assert.equal(case52NewRequestComposerSurfaceReady({ ...ready, ...drift }, prompt, runTarget), false);
  }
  assert.throws(() => case52NewRequestComposerSurfaceReady(ready, null, runTarget), /expected composer prompt/);
  assert.throws(() => case52NewRequestComposerSurfaceReady(ready, prompt, null), /expected run target/);
});

test("manual.case5_2 accepts only a newly admitted Turn after an Idle owner", () => {
  const previousExpectedState = {
    kind: "idle",
    latestTurnId: TURN_ID,
    admissionRevision: "7",
  };
  const nextTurnId = "01K3CASE52TURNNEXT000000000";
  const running = {
    selected_project_index: 0,
    selected_session_index: 0,
    session_rows: [{
      session_id: SESSION_ID,
      status: "running",
      loaded_status: "active",
      active_turn_id: nextTurnId,
      admission_revision: "8",
    }],
    run_target: {
      expectedState: { kind: "turn", turnId: nextTurnId, admissionRevision: "8" },
    },
  };
  const expected = { expectedSessionId: SESSION_ID, previousExpectedState };
  assert.equal(case52NewTurnAcquisitionAccepted(running, expected), true);
  assert.equal(case52NewTurnAcquisitionAccepted({
    ...running,
    session_rows: [{ ...running.session_rows[0], active_turn_id: TURN_ID }],
    run_target: { expectedState: { kind: "turn", turnId: TURN_ID, admissionRevision: "8" } },
  }, expected), false);
  assert.equal(case52NewTurnAcquisitionAccepted({
    ...running,
    session_rows: [{ ...running.session_rows[0], admission_revision: "7" }],
    run_target: { expectedState: { ...running.run_target.expectedState, admissionRevision: "7" } },
  }, expected), false);
  assert.equal(case52NewTurnAcquisitionAccepted({
    ...running,
    run_target: { expectedState: { ...running.run_target.expectedState, admissionRevision: "9" } },
  }, expected), false);
});

test("manual.case5_2 options and Quality config are explicit, portable, and reject legacy timeout drift", () => {
  const normalized = normalizeCase52Options({
    fixture_source: "C:\\fixture",
    provider_base_url: "http://192.0.2.1:1234/",
    main_model: "qwen/qwen3.6-27b",
    side_model: "google/gemma-4-12b-qat",
    expected_main_variant: "qwen/qwen3.6-27b@6bit",
    expected_side_variant: "google/gemma-4-12b-qat@q4_0",
  });
  assert.equal(normalized.fixtureSource, "C:\\fixture");
  assert.equal(normalized.providerBaseUrl, "http://192.0.2.1:1234");
  assert.equal(normalized.providerProfile, "lm_studio");
  assert.equal(normalized.scenarioConfigProfile, "legacy-lm-studio-six-field");
  assert.equal(normalized.configureMainViaGui, false);
  const config = case52FixtureConfig(normalized);
  assert.match(config, /model = "qwen\/qwen3\.6-27b"/);
  assert.match(config, /request_timeout_ms = 3600000/);
  assert.match(config, /context_window = 131072/);
  assert.doesNotMatch(config, /max_(?:output_)?tokens|reasoning_(?:effort|summary)|supports_reasoning|temperature|top_p|top_k|presence_penalty|frequency_penalty|seed\s*=|stop(?:_sequences)?\s*=|\[model\.extra_body_json\]|num_ctx/);
  assert.match(config, /access_mode = "auto_review"/);
  assert.match(config, /\[multi_agent\]\nenabled = false/);
  assert.match(config, /\[docling\]\nenabled = false/);
  assert.match(config, /\[mcp\]\nenabled = false/);
  assert.doesNotMatch(config, /stream_idle_timeout_ms/);
  assert.throws(() => normalizeCase52Options({}), /fixture_source/);
  assert.throws(() => normalizeCase52Options({ ...case52OptionsForFailure(), run_number: 95 }), /unknown/);
});

test("manual.case5_2 preserves legacy execution-owned LM Studio and accepts an explicit external-unmanaged lifecycle", () => {
  const legacy = normalizeCase52Options(case52OptionsForFailure());
  assert.equal(legacy.providerLifecycle, "execution-owned");
  assert.equal(legacy.scenarioConfigProfile, "legacy-lm-studio-six-field");

  const external = normalizeCase52Options({
    ...case52OptionsForFailure(),
    provider_profile: "lm_studio",
    provider_lifecycle: "external-unmanaged",
    configure_main_via_gui: true,
  });
  assert.equal(external.providerLifecycle, "external-unmanaged");
  assert.equal(external.scenarioConfigProfile, "lm-studio-native-external-unmanaged");
  assert.equal(external.configureMainViaGui, true);
  assert.match(case52FixtureConfig(external), /base_url = "http:\/\/127\.0\.0\.1:9"/);
  assert.throws(
    () => normalizeCase52Options({ ...case52OptionsForFailure(), provider_lifecycle: "host-owned" }),
    /provider_lifecycle/,
  );
});

test("manual.case5_2 accepts an external-unmanaged OpenAI-compatible /v1 provider without LM Studio fields", () => {
  const normalized = normalizeCase52Options({
    fixture_source: "C:\\fixture",
    provider_profile: "openai_compatible",
    provider_base_url: "http://192.0.2.10:8119/v1/",
    main_model: "Qwen3.8-27B-4bit",
  });
  assert.deepEqual(normalized, {
    fixtureSource: "C:\\fixture",
    providerBaseUrl: "http://192.0.2.10:8119/v1",
    providerProfile: "openai_compatible",
    mainModel: "Qwen3.8-27B-4bit",
    sideModel: "Qwen3.8-27B-4bit",
    expectedMainVariant: null,
    expectedSideVariant: null,
    providerLifecycle: "external-unmanaged",
    scenarioConfigProfile: "openai-compatible-v1",
    configureMainViaGui: false,
  });
  const config = case52FixtureConfig(normalized);
  assert.match(config, /provider_profile = "openai_compatible"/);
  assert.doesNotMatch(config, /provider_metadata_mode|provider_api_mode|num_ctx/);
  assert.match(config, /context_window = 131072/);
  assert.throws(
    () => normalizeCase52Options({
      ...case52OptionsForFailure(),
      provider_profile: "openai_compatible",
      provider_base_url: "http://192.0.2.10:8119/v1",
    }),
    /does not accept LM Studio fields/,
  );
  assert.throws(
    () => normalizeCase52Options({
      fixture_source: "C:\\fixture",
      provider_profile: "openai_compatible",
      provider_base_url: "http://192.0.2.10:8119",
      main_model: "Qwen3.8-27B-4bit",
    }),
    /\/v1 base URL/,
  );
  assert.throws(
    () => normalizeCase52Options({
      fixture_source: "C:\\fixture",
      provider_profile: "openai_compatible",
      configure_main_via_gui: true,
      provider_base_url: "http://192.0.2.10:8119/v1",
      main_model: "Qwen3.8-27B-4bit",
    }),
    /configure_main_via_gui is supported only by lm_studio/,
  );
  assert.throws(
    () => normalizeCase52Options({
      fixture_source: "C:\\fixture",
      provider_profile: "openai_compatible",
      configure_main_via_gui: false,
      provider_base_url: "http://192.0.2.10:8119/v1",
      main_model: "Qwen3.8-27B-4bit",
    }),
    /configure_main_via_gui is supported only by lm_studio/,
  );
  assert.throws(
    () => normalizeCase52Options({
      fixture_source: "C:\\fixture",
      provider_profile: "openai_compatible",
      provider_lifecycle: "execution-owned",
      provider_base_url: "http://192.0.2.10:8119/v1",
      main_model: "Qwen3.8-27B-4bit",
    }),
    /supports only provider_lifecycle external-unmanaged/,
  );
});

test("manual.case5_2 can seed a neutral connection for same-execution trusted Main Preferences input", () => {
  const normalized = normalizeCase52Options({
    ...case52OptionsForFailure(),
    provider_profile: "lm_studio",
    configure_main_via_gui: true,
  });
  assert.equal(normalized.configureMainViaGui, true);
  const config = case52FixtureConfig(normalized);
  assert.match(config, /base_url = "http:\/\/127\.0\.0\.1:9"/);
  assert.match(config, /model = "moyai-case5-2-before-gui-save"/);
  assert.doesNotMatch(config, /base_url = "http:\/\/192\.0\.2\.1:1234"/);
  assert.doesNotMatch(config, /model = "main"/);
  assert.match(config, /context_window = 131072/);
  assert.doesNotMatch(config, /max_(?:output_)?tokens|reasoning_(?:effort|summary)|supports_reasoning|temperature|top_p|top_k|presence_penalty|frequency_penalty|seed\s*=|stop(?:_sequences)?\s*=|\[model\.extra_body_json\]|num_ctx/);
  const explicitFalse = normalizeCase52Options({
    ...case52OptionsForFailure(),
    provider_profile: "lm_studio",
    configure_main_via_gui: false,
  });
  assert.equal(explicitFalse.configureMainViaGui, false);
  assert.match(case52FixtureConfig(explicitFalse), /base_url = "http:\/\/192\.0\.2\.1:1234"/);
  assert.throws(
    () => normalizeCase52Options({ ...case52OptionsForFailure(), configure_main_via_gui: "true" }),
    /must be boolean/,
  );
  assert.deepEqual(case52MainProviderSelectionKeys("lm_studio"), ["Home"]);
  assert.deepEqual(case52MainProviderSelectionKeys("openai_compatible"), ["Home", "ArrowDown"]);
  assert.throws(() => case52MainProviderSelectionKeys("lm_studio_chat_completions"), /unsupported/);

  const surface = {
    projection: {
      config_target: { workspacePath: "C:\\workspace", sessionId: null, configGeneration: "7" },
      config_fields: [
        { key: "model.base_url", value: "http://127.0.0.1:9" },
        { key: "model.model", value: "moyai-case5-2-before-gui-save" },
        { key: "model.provider_profile", value: "lm_studio" },
        { key: "model.api_key_env", value: "" },
        { key: "model.context_window", value: "131072" },
      ],
    },
  };
  assert.deepEqual(case52ExpectedMainGlobalSave(surface, normalized), {
    command: "save_global_config",
    args: {
      values: [
        { key: "model.base_url", text: "http://192.0.2.1:1234" },
        { key: "model.model", text: "main" },
        { key: "model.provider_profile", text: "lm_studio" },
        { key: "model.api_key_env", text: "" },
        { key: "model.context_window", text: "131072" },
      ],
      expectedTarget: { workspacePath: "C:\\workspace", sessionId: null, configGeneration: "7" },
    },
  });
});

test("manual.case5_2 classifies only Main Preferences observation timeouts as product failures", () => {
  const timeout = Object.assign(new Error("timed out"), {
    code: "observation-timeout",
    evidence: {
      label: "Main draft",
      attempts: 4,
      elapsed_ms: 10_000,
      last_value: { dirty: false, base: { value: "http://127.0.0.1:9" } },
      last_error: null,
    },
  });
  const classified = classifyCase52MainPreferencesObservationError(timeout, "editing the Main connection draft");
  assert.equal(classified.owner, "product");
  assert.equal(classified.code, "case5_2-main-preferences-observation-timeout");
  assert.equal(classified.evidence.action, "editing the Main connection draft");
  assert.deepEqual(classified.evidence.last_value, timeout.evidence.last_value);

  const transport = Object.assign(new Error("CDP transport closed"), { code: "cdp-transport-closed" });
  assert.equal(
    classifyCase52MainPreferencesObservationError(transport, "saving the Main connection"),
    transport,
  );
  const sample = new Error("desktop_state sample failed");
  assert.equal(
    classifyCase52MainPreferencesObservationError(sample, "opening Main Preferences"),
    sample,
  );
  const retriedSampleFailure = Object.assign(new Error("sample never recovered"), {
    code: "observation-timeout",
    evidence: {
      last_value: null,
      last_error: "desktop_state sample failed",
    },
  });
  assert.equal(
    classifyCase52MainPreferencesObservationError(retriedSampleFailure, "opening Main Preferences"),
    retriedSampleFailure,
  );
  const missingObservation = Object.assign(new Error("no observation"), {
    code: "observation-timeout",
    evidence: { last_error: null },
  });
  assert.equal(
    classifyCase52MainPreferencesObservationError(missingObservation, "opening Main Preferences"),
    missingObservation,
  );
});

test("manual.case5_2 product-owns only cardinality and call mismatch after trusted Main Save", () => {
  for (const code of ["desktop-command-probe-cardinality", "desktop-command-probe-call-mismatch"]) {
    const mismatch = Object.assign(new Error("unexpected Save command"), {
      code,
      evidence: { expected: "save_global_config", actual: "other" },
    });
    const classified = classifyCase52MainSaveCommandError(mismatch);
    assert.equal(classified.owner, "product");
    assert.equal(classified.code, "case5_2-main-provider-save-command");
    assert.equal(classified.evidence.code, code);
  }
  for (const code of [
    "desktop-command-probe-snapshot-invalid",
    "desktop-command-probe-overflow",
    "desktop-command-probe-order",
  ]) {
    const harnessError = Object.assign(new Error("probe integrity failed"), { code });
    assert.equal(classifyCase52MainSaveCommandError(harnessError), harnessError);
  }
});

test("manual.case5_2 settles a successful Main Preferences command probe removal", async () => {
  const cleanupFailures = [];
  const recorded = [];
  const result = await settleCase52MainCommandProbe({
    commandProbe: { probeId: "main", remove: async () => ({ removed: true, probe_id: "main", sequence: 1 }) },
    sink: { record: async (...args) => { recorded.push(args); } },
    cleanupFailures,
  });
  assert.equal(result.removal.removed, true);
  assert.equal(result.cleanup_failure, null);
  assert.deepEqual(cleanupFailures, []);
  assert.equal(recorded.length, 1);
  assert.equal(recorded[0][0], "case5_2-main-provider-command-probe-settled");
});

test("manual.case5_2 records command probe removal failure while preserving a primary GUI error", async () => {
  const cleanupFailures = [];
  const primary = new Error("Main Preferences did not settle");
  const removal = Object.assign(new Error("probe owner drifted"), {
    code: "desktop-command-probe-remove",
    evidence: { reason: "owner-drift" },
  });
  const result = await settleCase52MainCommandProbe({
    commandProbe: { probeId: "main", remove: async () => { throw removal; } },
    sink: { record: async () => { throw new Error("failure must not be recorded as success"); } },
    cleanupFailures,
    primaryError: primary,
  });
  assert.equal(result.primary_error.message, primary.message);
  assert.equal(result.cleanup_failure.code, removal.code);
  assert.equal(cleanupFailures.length, 1);
  assert.equal(cleanupFailures[0].message, removal.message);
});

test("manual.case5_2 records and surfaces command probe removal failure without a primary GUI error", async () => {
  const cleanupFailures = [];
  await assert.rejects(
    () => settleCase52MainCommandProbe({
      commandProbe: { probeId: "main", remove: async () => { throw new Error("probe removal failed"); } },
      sink: { record: async () => { throw new Error("failure must not be recorded as success"); } },
      cleanupFailures,
    }),
    (error) => error.owner === "harness"
      && error.code === "case5_2-main-provider-command-probe-cleanup",
  );
  assert.equal(cleanupFailures.length, 1);
  assert.equal(cleanupFailures[0].message, "probe removal failed");
});

test("manual.case5_2 records settlement evidence failure while preserving a primary GUI error", async () => {
  const cleanupFailures = [];
  const primary = new Error("Main Preferences did not settle");
  const result = await settleCase52MainCommandProbe({
    commandProbe: { probeId: "main", remove: async () => ({ removed: true, probe_id: "main", sequence: 2 }) },
    sink: { record: async () => { throw new Error("evidence write failed"); } },
    cleanupFailures,
    primaryError: primary,
  });
  assert.equal(result.removal.removed, true);
  assert.equal(result.primary_error.message, primary.message);
  assert.equal(result.cleanup_failure.owner, "evidence-sink");
  assert.equal(cleanupFailures.length, 1);
  assert.equal(cleanupFailures[0].message, "evidence write failed");
});

test("manual.case5_2 records and surfaces settlement evidence failure without a primary GUI error", async () => {
  const cleanupFailures = [];
  await assert.rejects(
    () => settleCase52MainCommandProbe({
      commandProbe: { probeId: "main", remove: async () => ({ removed: true, probe_id: "main", sequence: 2 }) },
      sink: { record: async () => { throw new Error("evidence write failed"); } },
      cleanupFailures,
    }),
    (error) => error.owner === "harness"
      && error.code === "case5_2-main-provider-command-probe-cleanup",
  );
  assert.equal(cleanupFailures.length, 1);
  assert.equal(cleanupFailures[0].owner, "evidence-sink");
});

test("manual.case5_2 rejects unresolved or mismatched Main Preferences command probe removal", async () => {
  for (const removal of [
    { removed: false, probe_id: "main", sequence: null },
    { removed: true, probe_id: "other", sequence: 1 },
    { removed: true, probe_id: "main", sequence: null },
  ]) {
    const cleanupFailures = [];
    const primary = new Error("probe installation or GUI acquisition was ambiguous");
    const result = await settleCase52MainCommandProbe({
      commandProbe: { probeId: "main", remove: async () => removal },
      sink: { record: async () => { throw new Error("invalid removal must not be recorded as settled"); } },
      cleanupFailures,
      primaryError: primary,
    });
    assert.equal(result.removal, null);
    assert.equal(result.primary_error.message, primary.message);
    assert.equal(result.cleanup_failure.code, "desktop-command-probe-remove");
    assert.deepEqual(result.cleanup_failure.evidence, removal);
    assert.equal(cleanupFailures.length, 1);
  }
});

test("manual.case5_2 Side screenshot readiness requires every configured value in the viewport", () => {
  const options = {
    providerBaseUrl: "http://192.0.2.10:1234",
    providerProfile: "lm_studio",
    sideModel: "google/gemma-4-12b-qat",
  };
  const visible = { visible: true, viewport_visible: true };
  const surface = {
    projection: {
      side_chat: {
        configured: true,
        deleting: false,
        owner_session_id: SESSION_ID,
        chat_id: "01K3CASE52SIDECHAT000000000",
        base_url: options.providerBaseUrl,
        model: options.sideModel,
        provider_profile: options.providerProfile,
        status: "idle",
        phase: "",
        last_error: "",
        draft_text: "",
        messages: [],
        can_send: true,
        can_cancel: false,
      },
    },
    settings: { ...visible },
    section: { ...visible, owner: SESSION_ID },
    details: { ...visible, open: true },
    profile: { ...visible, value: options.providerProfile },
    base: { ...visible, value: options.providerBaseUrl },
    manual: { ...visible, value: options.sideModel },
  };
  assert.equal(case52SideScreenshotSurfaceReady(surface, options, SESSION_ID), true);
  for (const changed of [
    { settings: { ...surface.settings, viewport_visible: false } },
    { section: { ...surface.section, viewport_visible: false } },
    { details: { ...surface.details, open: false } },
    { profile: { ...surface.profile, value: "openai_compatible" } },
    { base: { ...surface.base, viewport_visible: false } },
    { manual: { ...surface.manual, viewport_visible: false } },
  ]) {
    assert.equal(case52SideScreenshotSurfaceReady({ ...surface, ...changed }, options, SESSION_ID), false);
  }
});

test("manual.case5_2 product-owns only reachable Side screenshot observation mismatches", () => {
  const timeout = Object.assign(new Error("Side controls stayed outside the viewport"), {
    code: "observation-timeout",
    evidence: {
      label: "visible configured Side Chat Settings section",
      attempts: 10,
      elapsed_ms: 10_000,
      last_value: { section: { visible: true }, manual: { viewport_visible: false } },
      last_error: null,
    },
  });
  const classified = classifyCase52SideScreenshotObservationError(timeout, "showing the configured Side Chat model");
  assert.equal(classified.owner, "product");
  assert.equal(classified.code, "case5_2-side-screenshot-observation-timeout");
  assert.deepEqual(classified.evidence.last_value, timeout.evidence.last_value);
  const restored = classifyCase52SideScreenshotObservationError(timeout, "showing the restored Side Chat model");
  assert.equal(restored.owner, "product");
  assert.equal(restored.evidence.action, "showing the restored Side Chat model");

  const sampleFailure = Object.assign(new Error("desktop_state sample failed"), {
    code: "observation-timeout",
    evidence: { last_value: null, last_error: "cdp disconnected" },
  });
  assert.equal(
    classifyCase52SideScreenshotObservationError(sampleFailure, "showing the configured Side Chat model"),
    sampleFailure,
  );
  const screenshotFailure = Object.assign(new Error("Page.captureScreenshot failed"), {
    code: "cdp-screenshot-failed",
  });
  assert.equal(
    classifyCase52SideScreenshotObservationError(screenshotFailure, "showing the configured Side Chat model"),
    screenshotFailure,
  );
});

test("manual.case5_2 leaves generation settings to the host and rejects client overrides", () => {
  const raw = {
    fixture_source: "C:\\fixture",
    provider_profile: "openai_compatible",
    provider_base_url: "http://192.0.2.10:8119/v1",
    main_model: "Qwen3.8-27B-4bit",
  };
  const normalized = normalizeCase52Options(raw);
  assert.deepEqual(createCase52Scenario(raw).environment, {});
  const config = case52FixtureConfig(normalized);
  assert.doesNotMatch(config, /max_(?:output_)?tokens|reasoning_(?:effort|summary)|supports_reasoning|temperature|top_p|top_k|presence_penalty|frequency_penalty|seed\s*=|stop(?:_sequences)?\s*=|\[model\.extra_body_json\]|num_ctx/);
  const metadata = case52ExtraBodyEvidence();
  assert.deepEqual(metadata, {
    configured: false,
    environment_key: null,
    compact_json_sha256: null,
    compact_json_size_bytes: 0,
    generation_fields: [],
    allowlist_profile: null,
  });
  const evidenceOptions = case52EvidenceOptions(normalized);
  assert.deepEqual(evidenceOptions.extra_body_json, metadata);
  assert.throws(
    () => normalizeCase52Options({
      ...raw,
      extra_body_json: { chat_template_kwargs: { enable_thinking: false } },
    }),
    /unknown manual\.case5_2 option: extra_body_json/,
  );
  assert.throws(
    () => normalizeCase52Options({ ...case52OptionsForFailure(), extra_body_json: {} }),
    /unknown manual\.case5_2 option: extra_body_json/,
  );
});

test("manual.case5_2 detects only exact chat-template control tokens in assistant bodies", () => {
  assert.deepEqual(case52ProviderControlTokenLeaks({
    transcript_rows: [
      { row_kind: "assistant", stable_history_identity: "assistant-safe", body: "一般的な <|token|> は対象外です。" },
      { row_kind: "tool", stable_history_identity: "tool-leak", body: "<|im_start|>tool" },
    ],
  }), []);
  const leaks = case52ProviderControlTokenLeaks({
    transcript_rows: [{
      row_kind: "assistant",
      stable_history_identity: "assistant-leak",
      body: "prefix <|im_start|>assistant payload<|im_end|> suffix",
    }],
  });
  assert.equal(leaks.length, 1);
  assert.deepEqual(leaks[0].markers, ["<|im_start|>", "<|im_end|>"]);
  assert.equal(leaks[0].stable_history_identity, "assistant-leak");
  assert.match(leaks[0].body_sha256, /^[a-f0-9]{64}$/);
  assert.match(leaks[0].bounded_excerpt, /<\|im_start\|>/);

  const minimized = case52ProviderControlTokenLeakEvidence({
    run_status_key: "running",
    run_phase: "streaming",
    task_activity_state: "running",
    busy: true,
    agent_tree_active: true,
    selected_project_index: -1,
    selected_session_index: -1,
    chat_session_rows: [],
    config_fields: [{ key: "model.extra_body_json", value: '{"api_key":"must-not-leak"}' }],
    transcript_rows: [{ row_kind: "assistant", body: "<|im_start|>" }],
  }, leaks);
  const minimizedJson = JSON.stringify(minimized);
  assert.doesNotMatch(minimizedJson, /config_fields|extra_body_json|api_key|must-not-leak/);
  assert.match(minimizedJson, /assistant-leak/);
});

test("manual.case5_2 keeps a detected control-token leak while Stop acquisition remains harness-owned", () => {
  const settled = {
    stage: "stage1",
    leaks: [{ stable_history_identity: "assistant-leak", markers: ["<|im_start|>"] }],
    projection: { path: "stage1-provider-control-token-leak.json" },
    projection_error: null,
    screenshot: { path: "stage1-provider-control-token-leak.png" },
    screenshot_error: null,
    visible_stop_count: 1,
    stop: { action: "stage1-provider-control-token-leak-visible-stop" },
    stop_error: null,
    terminal: { run_status_key: "cancelled", task_activity_state: "idle" },
    record_error: null,
  };
  const product = case52ProviderControlTokenLeakFailure("stage1", settled);
  assert.equal(product.owner, "product");
  assert.equal(product.code, "case5_2-provider-control-token-leak");

  const unsettled = {
    ...settled,
    visible_stop_count: 0,
    stop: null,
    stop_error: { code: "cdp-transport", message: "connection closed" },
    terminal: null,
  };
  const harness = case52ProviderControlTokenLeakFailure("stage1", unsettled);
  assert.equal(harness.owner, "harness");
  assert.equal(harness.code, "case5_2-provider-control-token-leak-stop");
  assert.equal(harness.evidence.observed_product_failure.owner, "product");
  assert.equal(harness.evidence.observed_product_failure.code, "case5_2-provider-control-token-leak");
  assert.deepEqual(harness.evidence.observed_product_failure.evidence, unsettled);

  for (const field of ["projection_error", "screenshot_error", "record_error"]) {
    const failedEvidence = {
      ...settled,
      [field]: { code: `injected-${field}`, message: `${field} failed` },
    };
    const failure = case52ProviderControlTokenLeakFailure("stage1", failedEvidence);
    assert.equal(failure.owner, "harness");
    assert.equal(failure.evidence.observed_product_failure.owner, "product");
    assert.deepEqual(failure.evidence.observed_product_failure.evidence, failedEvidence);
  }
});

test("manual.case5_2 summary v1 preserves unloaded samples and adds provider samples", () => {
  const samples = [{ name: "desktop-ready", provider_load_state: "not-observable-external-unmanaged" }];
  assert.deepEqual(case52SideProviderSummary({ providerProfile: "openai_compatible" }, samples), {
    selected_model_unloaded_samples: [],
    selected_model_provider_samples: samples,
  });
  assert.deepEqual(case52SideProviderSummary({ providerProfile: "lm_studio" }, samples), {
    selected_model_unloaded_samples: samples,
    selected_model_provider_samples: samples,
  });
  const stage4SideChat = { configured: true, status: "idle", messages: [] };
  const legacy = case52LegacySideSummaryV1({
    stage4SideChat,
    restoredSideChat: { messages: [] },
    providerSummary: case52SideProviderSummary({ providerProfile: "openai_compatible" }, samples),
  });
  assert.deepEqual(legacy, {
    side_chat: stage4SideChat,
    side_chat_request_observation: {
      trusted_side_send_action_count: "not-derived-from-event-ledger",
      persisted_message_count_at_restart_restore: 0,
      persisted_message_count_at_stage4_terminal: 0,
      selected_model_unloaded_samples: [],
      selected_model_provider_samples: samples,
      provider_generation_request_zero: "unverified-no-traffic-ledger",
    },
  });
});

test("manual.case5_2 reads exact OpenAI-compatible model identity and optional context capacity", () => {
  const options = {
    providerProfile: "openai_compatible",
    mainModel: "Qwen3.8-27B-4bit",
  };
  assert.deepEqual(case52ProviderModelState({
    models: {
      value: {
        data: [{ id: "Qwen3.8-27B-4bit", owned_by: "omlx", max_model_len: 131_072 }],
      },
    },
  }, options), {
    main: { id: "Qwen3.8-27B-4bit", owned_by: "omlx", max_model_len: 131_072 },
    side: { id: "Qwen3.8-27B-4bit", owned_by: "omlx", max_model_len: 131_072 },
    main_match_count: 1,
    context_capacity: {
      reported: true,
      candidates: [{ field: "max_model_len", value: 131_072 }],
      effective: 131_072,
      conflict: false,
    },
  });
});

test("manual.case5_2 normalizes checkout line endings before exact trusted GUI insertion", () => {
  assert.equal(normalizeCase52PromptText("one\r\ntwo\rthree\n"), "one\ntwo\nthree\n");
  assert.throws(() => normalizeCase52PromptText(null), /prompt text/);
});

test("manual.case5_2 rejects common in-workspace dependency installation roots", () => {
  assert.deepEqual(case52ForbiddenWorkspacePaths([
    "backend/src/cancel.py",
    "backend/.venv/Lib/site-packages/pkg/__init__.py",
    "backend/vendor/pkg-1.0.dist-info/METADATA",
    "backend/vendor/pkg.egg-info/PKG-INFO",
    "frontend/node_modules/pkg/index.js",
    "backend/.eggs/pkg/__init__.py",
    "backend/pip-wheel-metadata/pkg.json",
    "backend/__pypackages__/3.13/lib/pkg.py",
    "backend/.tox/py313/Lib/site-packages/pkg.py",
    "backend/.nox/tests/Lib/site-packages/pkg.py",
  ]), [
    "backend/.eggs/pkg/__init__.py",
    "backend/.nox/tests/Lib/site-packages/pkg.py",
    "backend/.tox/py313/Lib/site-packages/pkg.py",
    "backend/.venv/Lib/site-packages/pkg/__init__.py",
    "backend/__pypackages__/3.13/lib/pkg.py",
    "backend/pip-wheel-metadata/pkg.json",
    "backend/vendor/pkg-1.0.dist-info/METADATA",
    "backend/vendor/pkg.egg-info/PKG-INFO",
    "frontend/node_modules/pkg/index.js",
  ]);
});

test("manual.case5_2 evaluator workspace diff exposes every source mutation", () => {
  const before = {
    files: [
      { path: "README.md", sha256: "a", bytes: 1 },
      { path: "backend/app.py", sha256: "b", bytes: 2 },
    ],
  };
  assert.deepEqual(case52EvaluatorWorkspaceDiff(before, { files: structuredClone(before.files) }), {
    modified: [],
    deleted: [],
    added: [],
  });
  assert.deepEqual(case52EvaluatorWorkspaceDiff(before, {
    files: [
      { path: "README.md", sha256: "changed", bytes: 7 },
      { path: "backend/generated.py", sha256: "c", bytes: 3 },
    ],
  }), {
    modified: ["README.md"],
    deleted: ["backend/app.py"],
    added: ["backend/generated.py"],
  });
});

test("manual.case5_2 evaluator settlement preserves the process-owner error after post-manifest recovery", async () => {
  const primary = new Error("process owner failed");
  const secondary = new Error("workspace changed");
  let recovery = null;
  await assert.rejects(
    settleCase52WorkspaceEvaluator({
      label: "public-suite",
      evaluate: async () => { throw primary; },
      captureAfter: async () => ({ stage: "post", files: [] }),
      assertStable: async () => { throw secondary; },
      recordRecovery: async (evidence) => { recovery = evidence; },
    }),
    (error) => error === primary,
  );
  assert.equal(recovery.primary_error.message, primary.message);
  assert.equal(recovery.integrity_errors[0].message, secondary.message);
  assert.equal(recovery.after_manifest.stage, "post");
});

test("manual.case5_2 evaluator settlement returns result, post-manifest, and exact diff together", async () => {
  const result = { exit_code: 0 };
  const manifest = { stage: "post", files: [] };
  const diff = { modified: [], deleted: [], added: [] };
  const settled = await settleCase52WorkspaceEvaluator({
    label: "public-suite",
    evaluate: async () => result,
    captureAfter: async () => manifest,
    assertStable: async () => diff,
    recordRecovery: async () => { throw new Error("recovery must not run on success"); },
  });
  assert.deepEqual(settled, { result, afterManifest: manifest, workspaceDiff: diff });
});

test("manual.case5_2 cleanup owns actual Main and unexpected Side instance IDs", () => {
  assert.deepEqual(case52ProviderCleanupPlan({
    main: { loaded_instances: [{ id: "main-instance" }] },
    side: { loaded_instances: [{ id: "side-instance" }] },
  }), {
    instances: [
      { instance_id: "main-instance", roles: ["main"] },
      { instance_id: "side-instance", roles: ["side"] },
    ],
    unexpected_side_loaded: true,
    failures: [],
  });
  assert.deepEqual(case52ProviderCleanupPlan({
    main: { loaded_instances: [{ id: "shared" }] },
    side: { loaded_instances: [{ id: "shared" }, {}] },
  }), {
    instances: [{ instance_id: "shared", roles: ["main", "side"] }],
    unexpected_side_loaded: true,
    failures: ["side-instance-id-invalid"],
  });
});

function providerModels(mainIds = [], sideIds = []) {
  return {
    main: {
      selected_variant: "main@q6",
      loaded_instances: mainIds.map((id) => ({ id })),
    },
    side: {
      selected_variant: "side@q4",
      loaded_instances: sideIds.map((id) => ({ id })),
    },
    main_v0: { state: mainIds.length === 0 ? "not-loaded" : "loaded" },
    side_v0: { state: sideIds.length === 0 ? "not-loaded" : "loaded" },
  };
}

function externalLmStudioOptions(overrides = {}) {
  return {
    providerProfile: "lm_studio",
    providerLifecycle: "external-unmanaged",
    providerBaseUrl: "http://192.0.2.1:1234",
    mainModel: "main",
    sideModel: "side",
    expectedMainVariant: "main@q6",
    expectedSideVariant: "side@q4",
    ...overrides,
  };
}

function externalLmStudioSnapshot({
  capturedAt = "2026-08-27T00:00:00.000Z",
  elapsedMs = 1,
  context = 169_728,
  mainVariant = "main@q6",
  sideVariant = "side@q4",
  mainLoaded = true,
  sideLoaded = false,
  volatileTimestamp = "2026-08-27T00:00:00.000Z",
} = {}) {
  const mainInstances = mainLoaded ? [{
    id: "main",
    config: {
      context_length: context,
      parallel: 4,
      loaded_at: volatileTimestamp,
    },
    elapsed_ms: elapsedMs,
  }] : [];
  const sideInstances = sideLoaded ? [{ id: "side", config: { context_length: context } }] : [];
  return {
    captured_at: capturedAt,
    v1: {
      endpoint: "http://192.0.2.1:1234/api/v1/models",
      status: 200,
      elapsed_ms: elapsedMs,
      value: {
        models: [
          {
            key: "main",
            type: "llm",
            selected_variant: mainVariant,
            loaded_instances: mainInstances,
            updated_at: volatileTimestamp,
          },
          {
            key: "side",
            type: "llm",
            selected_variant: sideVariant,
            loaded_instances: sideInstances,
          },
        ],
      },
    },
    v0: {
      endpoint: "http://192.0.2.1:1234/api/v0/models",
      status: 200,
      elapsed_ms: elapsedMs,
      value: {
        data: [
          {
            id: "main",
            state: mainLoaded ? "loaded" : "not-loaded",
            ...(mainLoaded ? { loaded_context_length: context } : {}),
            loaded_at: volatileTimestamp,
          },
          { id: "side", state: sideLoaded ? "loaded" : "not-loaded" },
        ],
      },
    },
  };
}

test("manual.case5_2 external LM Studio fingerprint excludes observation timing but detects host drift", () => {
  const options = externalLmStudioOptions();
  const firstSnapshot = externalLmStudioSnapshot();
  const secondSnapshot = externalLmStudioSnapshot({
    capturedAt: "2026-08-27T01:00:00.000Z",
    elapsedMs: 999,
    volatileTimestamp: "2026-08-27T01:00:00.000Z",
  });
  const first = case52ProviderHostFingerprint(firstSnapshot, options);
  const second = case52ProviderHostFingerprint(secondSnapshot, options);
  assert.equal(first.sha256, second.sha256);
  assert.equal(first.excluded_fields.some((field) => field.endsWith("loaded_at")), true);
  assert.equal(first.excluded_fields.some((field) => field.endsWith("elapsed_ms")), true);

  const drifted = case52ProviderHostFingerprint(externalLmStudioSnapshot({ context: 262_144 }), options);
  assert.notEqual(first.sha256, drifted.sha256);
});

test("manual.case5_2 external LM Studio observation requires exact load ownership, variants, and sufficient reported context", () => {
  const options = externalLmStudioOptions();
  const snapshot = externalLmStudioSnapshot();
  const fingerprint = case52ProviderHostFingerprint(snapshot, options);
  const observation = case52ExternalLmStudioObservation(snapshot, options, fingerprint);
  assert.deepEqual(observation.failures, []);
  assert.equal(observation.host_context.requested, null);
  assert.equal(observation.host_context.applied, null);
  assert.equal(observation.host_context.reported_loaded, 169_728);
  assert.deepEqual(observation.lifecycle_actions, {
    load_attempted: false,
    unload_attempted: false,
    unload_authorized: false,
  });
  assert.equal(observation.host_fingerprint_stable, true);
  assert.deepEqual(case52LmStudioLoadedContext(observation.models), {
    reported: true,
    candidates: [
      { field: "loaded_instances[0].config.context_length", value: 169_728 },
      { field: "main_v0.loaded_context_length", value: 169_728 },
    ],
    effective: 169_728,
    conflict: false,
  });

  assert.match(
    case52ExternalLmStudioObservation(externalLmStudioSnapshot({ context: 65_536 }), options).failures.join(","),
    /main-loaded-context-below-local-budget/,
  );
  assert.match(
    case52ExternalLmStudioObservation(externalLmStudioSnapshot({ mainLoaded: false }), options).failures.join(","),
    /main-load-state-mismatch/,
  );
  assert.match(
    case52ExternalLmStudioObservation(externalLmStudioSnapshot({ sideLoaded: true }), options).failures.join(","),
    /side-model-loaded/,
  );
  assert.match(
    case52ExternalLmStudioObservation(externalLmStudioSnapshot({ mainVariant: "main@q4" }), options).failures.join(","),
    /main-variant-mismatch/,
  );
  const conflictingContext = externalLmStudioSnapshot();
  conflictingContext.v0.value.data[0].loaded_context_length = 262_144;
  assert.match(
    case52ExternalLmStudioObservation(conflictingContext, options).failures.join(","),
    /main-loaded-context-conflict/,
  );
});

test("manual.case5_2 external LM Studio preflight is GET-observation-only and seals two stable samples", async () => {
  const options = externalLmStudioOptions();
  const snapshots = [
    externalLmStudioSnapshot(),
    externalLmStudioSnapshot({
      capturedAt: "2026-08-27T00:00:01.000Z",
      elapsedMs: 12,
      volatileTimestamp: "2026-08-27T00:00:01.000Z",
    }),
  ];
  let loadCalls = 0;
  const writes = [];
  const state = {
    providerLoadAttempted: false,
    providerHostFingerprintSamples: [],
  };
  await loadMainProvider({
    options,
    sink: {
      writeJson: async (...args) => { writes.push(["write", ...args]); },
      record: async (...args) => { writes.push(["record", ...args]); },
    },
    state,
    phase: "preparing",
    providerIo: {
      capture: async () => ({ snapshot: snapshots.shift() }),
      load: async () => { loadCalls += 1; throw new Error("external provider load must not run"); },
    },
  });
  assert.equal(loadCalls, 0);
  assert.equal(snapshots.length, 0);
  assert.equal(state.providerLoadAttempted, false);
  assert.equal(state.providerExternalPreflightObserved, true);
  assert.equal(state.providerEffectiveContext, 169_728);
  assert.equal(state.providerHostFingerprintSamples.length, 2);
  assert.equal(writes.length, 2);
  assert.deepEqual(case52ProviderSummaryEvidence(options, state), {
    provider_requested_context: null,
    provider_applied_context: null,
    provider_reported_context_capacity: null,
    provider_reported_loaded_context: 169_728,
    provider_lifecycle_actions: {
      load_attempted: false,
      unload_attempted: false,
      unload_authorized: false,
    },
    provider_host_fingerprint: state.providerHostFingerprint,
    provider_host_fingerprint_samples: state.providerHostFingerprintSamples,
  });
});

test("manual.case5_2 external LM Studio preflight rejects fingerprint drift without issuing a load", async () => {
  const options = externalLmStudioOptions();
  const snapshots = [
    externalLmStudioSnapshot(),
    externalLmStudioSnapshot({ context: 262_144 }),
  ];
  let loadCalls = 0;
  await assert.rejects(
    () => loadMainProvider({
      options,
      sink: { writeJson: async () => {}, record: async () => {} },
      state: { providerLoadAttempted: false, providerHostFingerprintSamples: [] },
      phase: "preparing",
      providerIo: {
        capture: async () => ({ snapshot: snapshots.shift() }),
        load: async () => { loadCalls += 1; },
      },
    }),
    (error) => error?.owner === "environment"
      && error?.code === "case5_2-provider-preflight"
      && error?.evidence?.failures?.includes("confirmation:provider-host-fingerprint-drift"),
  );
  assert.equal(loadCalls, 0);
});

test("manual.case5_2 external LM Studio checkpoints reject drift without repair", async () => {
  const options = externalLmStudioOptions();
  const baselineSnapshot = externalLmStudioSnapshot();
  const baseline = case52ProviderHostFingerprint(baselineSnapshot, options);
  const state = {
    providerHostFingerprint: baseline,
    providerHostFingerprintSamples: [],
    sideProviderSamples: [],
  };
  const sink = { writeJson: async () => ({ sha256: "evidence" }) };
  const stable = await providerMustKeepSideUnloaded({
    options,
    sink,
    state,
    name: "desktop-ready",
    providerIo: { capture: async () => ({ snapshot: externalLmStudioSnapshot({ elapsedMs: 9 }) }) },
  });
  assert.deepEqual(stable.failures, []);
  assert.equal(state.sideProviderSamples[0].host_fingerprint_stable, true);

  await assert.rejects(
    () => providerMustKeepSideUnloaded({
      options,
      sink,
      state,
      name: "final-terminal",
      providerIo: { capture: async () => ({ snapshot: externalLmStudioSnapshot({ context: 262_144 }) }) },
    }),
    (error) => error?.owner === "product"
      && error?.code === "case5_2-provider-runtime-drift"
      && error?.evidence?.failures?.includes("provider-host-fingerprint-drift"),
  );
});

test("manual.case5_2 provider cleanup replans after snapshot failure and late Main/Side load", async () => {
  const options = {
    providerBaseUrl: "http://192.0.2.1:1234",
    mainModel: "main",
    sideModel: "side",
    expectedMainVariant: "main@q6",
    expectedSideVariant: "side@q4",
  };
  const sequence = [
    new Error("transient catalog failure"),
    providerModels(),
    providerModels(["late-main"], ["unexpected-side"]),
    providerModels(),
    providerModels(),
  ];
  const unloaded = [];
  let clock = 0;
  const result = await unloadMainProvider({
    options,
    state: {
      providerLoadAttempted: true,
      providerLoadResponseObserved: true,
      mainProviderInstanceId: "accepted-main",
      providerOwned: true,
    },
    providerIo: {
      capture: async () => {
        const value = sequence.shift();
        if (value instanceof Error) throw value;
        return { snapshot: { sequence: 5 - sequence.length }, models: value };
      },
      unload: async (instanceId) => {
        unloaded.push(instanceId);
        return { status: "unloaded", instance_id: instanceId };
      },
      now: () => clock,
      delay: async (milliseconds) => { clock += milliseconds; },
      timeoutMs: 100,
      pollMs: 1,
      stableSamples: 2,
    },
  });
  assert.equal(result.input, "pass");
  assert.equal(result.resources[0].stable_zero, true);
  assert.equal(result.resources[0].observations.length, 5);
  assert.deepEqual(unloaded, ["accepted-main", "late-main", "unexpected-side"]);
  assert.equal(result.productFailure?.code, "case5_2-side-model-loaded");
});

test("manual.case5_2 provider cleanup cannot accept stable snapshots without a load response", async () => {
  const options = {
    providerBaseUrl: "http://192.0.2.1:1234",
    mainModel: "main",
    sideModel: "side",
    expectedMainVariant: "main@q6",
    expectedSideVariant: "side@q4",
  };
  let clock = 0;
  const result = await unloadMainProvider({
    options,
    state: {
      providerLoadAttempted: true,
      providerLoadResponseObserved: false,
      mainProviderInstanceId: null,
      providerOwned: false,
    },
    providerIo: {
      capture: async () => ({ snapshot: { clock }, models: providerModels() }),
      unload: async () => { throw new Error("unexpected unload"); },
      now: () => clock,
      delay: async (milliseconds) => { clock += milliseconds; },
      timeoutMs: 3,
      pollMs: 1,
      stableSamples: 2,
    },
  });
  assert.equal(result.input, "fail");
  assert.equal(result.resources[0].stable_zero, false);
  assert.match(result.resources[0].failures.join(","), /provider-load-response-unobserved/);
});

test("manual.case5_2 provider cleanup preserves Side activity seen only by the v0 catalog", async () => {
  const options = {
    providerBaseUrl: "http://192.0.2.1:1234",
    mainModel: "main",
    sideModel: "side",
    expectedMainVariant: "main@q6",
    expectedSideVariant: "side@q4",
  };
  const v0OnlySideActivity = providerModels();
  v0OnlySideActivity.side_v0.state = "loaded";
  const sequence = [v0OnlySideActivity, providerModels(), providerModels()];
  let clock = 0;
  const result = await unloadMainProvider({
    options,
    state: {
      providerLoadAttempted: true,
      providerLoadResponseObserved: true,
      mainProviderInstanceId: null,
      providerOwned: true,
    },
    providerIo: {
      capture: async () => ({ snapshot: { clock }, models: sequence.shift() }),
      unload: async () => { throw new Error("unexpected unload without an instance id"); },
      now: () => clock,
      delay: async (milliseconds) => { clock += milliseconds; },
      timeoutMs: 100,
      pollMs: 1,
      stableSamples: 2,
    },
  });
  assert.equal(result.input, "pass");
  assert.equal(result.resources[0].stable_zero, true);
  assert.equal(result.productFailure?.code, "case5_2-side-model-loaded");
  assert.match(result.productFailure.evidence.observations[0].catalog_failures.join(","), /side-v0-state-mismatch/);
});

test("manual.case5_2 external provider cleanup verifies availability without load or unload", async () => {
  let unloadCalls = 0;
  const result = await unloadMainProvider({
    options: {
      providerProfile: "openai_compatible",
      providerBaseUrl: "http://192.0.2.10:8119/v1",
      mainModel: "Qwen3.8-27B-4bit",
      sideModel: "Qwen3.8-27B-4bit",
    },
    state: {
      providerExternalPreflightObserved: true,
      providerComparabilityDeviations: ["provider-lifecycle-external-unmanaged"],
    },
    providerIo: {
      capture: async () => ({
        snapshot: { captured_at: "2026-08-24T00:00:00.000Z" },
        models: {
          main: { id: "Qwen3.8-27B-4bit" },
          side: { id: "Qwen3.8-27B-4bit" },
          main_match_count: 1,
          context_capacity: {
            reported: true,
            candidates: [{ field: "max_model_len", value: 131_072 }],
            effective: 131_072,
            conflict: false,
          },
        },
      }),
      unload: async () => { unloadCalls += 1; },
    },
  });
  assert.equal(result.input, "pass");
  assert.equal(unloadCalls, 0);
  assert.equal(result.resources[0].lifecycle, "external-unmanaged");
  assert.equal(result.resources[0].load_attempted, false);
  assert.equal(result.resources[0].unload_attempted, false);
});

test("manual.case5_2 external LM Studio quiesce observes the stable host without unload or repair", async () => {
  const options = externalLmStudioOptions();
  const snapshot = externalLmStudioSnapshot();
  const fingerprint = case52ProviderHostFingerprint(snapshot, options);
  let unloadCalls = 0;
  const result = await unloadMainProvider({
    options,
    state: {
      providerExternalPreflightObserved: true,
      providerHostFingerprint: fingerprint,
      providerComparabilityDeviations: ["provider-lifecycle-external-unmanaged"],
    },
    providerIo: {
      capture: async () => ({ snapshot: externalLmStudioSnapshot({ elapsedMs: 42 }) }),
      unload: async () => { unloadCalls += 1; },
    },
  });
  assert.equal(result.input, "pass");
  assert.equal(unloadCalls, 0);
  assert.equal(result.resources[0].kind, "lm-studio-external-model");
  assert.equal(result.resources[0].host_context.requested, null);
  assert.equal(result.resources[0].host_context.applied, null);
  assert.equal(result.resources[0].host_context.reported_loaded, 169_728);
  assert.equal(result.resources[0].host_fingerprint_stable, true);
  assert.equal(result.resources[0].load_attempted, false);
  assert.equal(result.resources[0].unload_attempted, false);
  assert.equal(result.resources[0].unload_authorized, false);

  const drift = await unloadMainProvider({
    options,
    state: {
      providerExternalPreflightObserved: true,
      providerHostFingerprint: fingerprint,
      providerComparabilityDeviations: ["provider-lifecycle-external-unmanaged"],
    },
    providerIo: {
      capture: async () => ({ snapshot: externalLmStudioSnapshot({ context: 262_144 }) }),
      unload: async () => { unloadCalls += 1; },
    },
  });
  assert.equal(drift.input, "fail");
  assert.match(drift.resources[0].failures.join(","), /provider-host-fingerprint-drift/);
  assert.equal(unloadCalls, 0);
});

test("manual.case5_2 external LM Studio provider lifecycle emits catalog GETs only", async (context) => {
  const source = externalLmStudioSnapshot();
  const requests = [];
  const server = createServer((request, response) => {
    requests.push({ method: request.method, url: request.url });
    const value = request.url === "/api/v1/models"
      ? source.v1.value
      : request.url === "/api/v0/models"
        ? source.v0.value
        : null;
    const status = value === null ? 404 : 200;
    const body = JSON.stringify(value ?? { error: "unexpected route" });
    response.writeHead(status, {
      "content-type": "application/json",
      "content-length": Buffer.byteLength(body),
      connection: "close",
    });
    response.end(body);
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  context.after(() => new Promise((resolve, reject) => {
    server.close((error) => error === undefined ? resolve() : reject(error));
  }));
  const address = server.address();
  assert.notEqual(address, null);
  const options = externalLmStudioOptions({ providerBaseUrl: `http://127.0.0.1:${address.port}` });
  const state = {
    providerLoadAttempted: false,
    providerHostFingerprintSamples: [],
    sideProviderSamples: [],
  };
  const sink = { writeJson: async () => ({ sha256: "evidence" }), record: async () => {} };

  await loadMainProvider({ options, sink, state, phase: "preparing" });
  await providerMustKeepSideUnloaded({ options, sink, state, name: "desktop-ready" });
  const quiesce = await unloadMainProvider({ options, state });

  assert.equal(quiesce.input, "pass");
  assert.equal(requests.length, 8);
  assert.equal(requests.every((request) => request.method === "GET"), true);
  assert.deepEqual(
    [...new Set(requests.map((request) => request.url))].sort(),
    ["/api/v0/models", "/api/v1/models"],
  );
  assert.equal(requests.some((request) => /(?:load|unload|config)/i.test(request.url)), false);
});

async function streamSha256(candidate) {
  const digest = crypto.createHash("sha256");
  for await (const chunk of createReadStream(candidate)) digest.update(chunk);
  return digest.digest("hex");
}

test("manual.case5_2 reads oversized external output as a bounded head-tail sample", async (context) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "moyai-case5-2-output-"));
  context.after(() => rm(root, { recursive: true, force: true }));
  const candidate = path.join(root, "oversized.log");
  const size = 16 * 1024 * 1024 + 1;
  const handle = await open(candidate, "wx");
  try { await handle.truncate(size); }
  finally { await handle.close(); }
  await assert.rejects(() => readCase52ExternalOutput(candidate, {
    path: candidate,
    sha256: "0".repeat(64),
    size_bytes: size,
  }), /SHA-256 changed/);
  const actualSha256 = await streamSha256(candidate);
  const capture = await readCase52ExternalOutput(candidate, {
    path: candidate,
    sha256: actualSha256,
    size_bytes: size,
  });
  assert.equal(capture.identity.sample_kind, "head-tail");
  assert.equal(capture.identity.raw.size_bytes, size);
  assert.ok(capture.bytes.byteLength < 129 * 1024);
  assert.match(capture.bytes.toString("utf8"), /bounded sample; raw bytes=16777217/);
});

test("manual.case5_2 sealed oracle rejects a same-byte physical file replacement", async (context) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "moyai-case5-2-oracle-"));
  context.after(() => rm(root, { recursive: true, force: true }));
  const candidate = path.join(root, "oracle.py");
  const retained = path.join(root, "oracle-retained.py");
  await writeFile(candidate, "def test_oracle():\n    assert True\n", { flag: "wx" });
  const identity = await case52PhysicalFileIdentity(candidate);
  assert.deepEqual(await assertCase52PhysicalFileIdentity(identity), identity);
  await rename(candidate, retained);
  await writeFile(candidate, "def test_oracle():\n    assert True\n", { flag: "wx" });
  await assert.rejects(
    () => assertCase52PhysicalFileIdentity(identity),
    /physical identity changed/,
  );
});

function case52OptionsForFailure() {
  return {
    fixture_source: "C:\\fixture",
    provider_base_url: "http://192.0.2.1:1234",
    main_model: "main",
    side_model: "side",
    expected_main_variant: "main@q6",
    expected_side_variant: "side@q4",
  };
}

function transcript() {
  return [
    { row_kind: "user", stable_history_identity: "history-user-1", body: "stage prompt" },
    { row_kind: "tool", stable_history_identity: "history-tool-1", body: "write" },
    { row_kind: "work_summary_completed", stable_history_identity: `turn:${TURN_ID}:work-summary`, body: "" },
    { row_kind: "assistant", stable_history_identity: null, body: "done" },
  ];
}

function terminalProjection(overrides = {}) {
  return {
    run_status_key: "completed",
    task_activity_state: "idle",
    busy: false,
    agent_tree_active: false,
    post_run_refresh_pending: false,
    background_mutation_pending: false,
    async_polling_required: false,
    pending_async_operations: [],
    navigation_loading: false,
    navigation_admission_open: true,
    turn_page_admission_open: true,
    provider_loading: false,
    overlay: "none",
    confirmation_visible: false,
    confirmation_id: null,
    confirmation: null,
    draft_prompt: "",
    composer_submit_mode: "new_request",
    can_submit: true,
    selected_project_index: 0,
    selected_session_index: 0,
    session_rows: [{
      session_id: SESSION_ID,
      status: "completed",
      loaded_status: "idle",
      active_turn_id: null,
      interrupt_target: null,
      pending_permission_requests: 0,
      pending_user_input_requests: 0,
      admission_revision: "7",
    }],
    chat_session_rows: [],
    run_target: {
      expectedState: { kind: "idle", latestTurnId: TURN_ID, admissionRevision: "7" },
    },
    transcript_rows: transcript(),
    turn_page_offset: 0,
    turn_page_limit: 80,
    turn_page_total: 4,
    turn_page_has_more: false,
    ...overrides,
  };
}

function file(path, sha256 = `sha-${path}`, bytes = 10) {
  return { path, sha256, bytes };
}

function documents(cancelExists) {
  return [
    { name: "README.md", exists: true, bytes: 100 },
    { name: "basic_design.md", exists: true, bytes: 100 },
    { name: "detail_design.md", exists: true, bytes: 100 },
    { name: "evidence_matrix.md", exists: true, bytes: 100 },
    { name: "cancel_contract.md", exists: cancelExists, bytes: cancelExists ? 100 : 0 },
  ];
}

function stage1Manifest(overrides = {}) {
  const added = ["README.md", "basic_design.md", "detail_design.md", "evidence_matrix.md"];
  return {
    baseline_aggregate_sha256: "seed-sha",
    files: [file("backend/app.py"), file("task.md"), ...added.map((name) => file(name))],
    diff: { modified: [], deleted: [], added },
    documents: documents(false),
    evidence_matrix_rows: 25,
    ...overrides,
  };
}

function stage2Manifest(overrides = {}) {
  const stage1 = stage1Manifest();
  const added = [...stage1.diff.added, "cancel_contract.md"];
  return {
    ...stage1,
    files: [...stage1.files, file("cancel_contract.md")],
    diff: { modified: [], deleted: [], added },
    documents: documents(true),
    ...overrides,
  };
}

function evaluationReport(overrides = {}) {
  return {
    public_suite: { exit_code: 0 },
    hidden_oracle: { exit_code: 0 },
    public_suite_pass: true,
    hidden_oracle_pass: true,
    all_required_documents: true,
    documents: documents(true),
    ...overrides,
  };
}

test("case5_2 normal terminal requires one settled completed session owner", () => {
  const projection = terminalProjection();
  const options = {
    expectedSessionId: SESSION_ID,
    expectedTurnId: TURN_ID,
    expectedPrompt: "stage prompt",
  };
  assert.deepEqual(case52NormalTerminalFailures(projection, options), []);
  assert.deepEqual(classifyCase52NormalTerminal(projection, options), { decision: "pass", failures: [] });

  assert.match(
    case52NormalTerminalFailures(terminalProjection({ busy: true }), options).join(","),
    /projection-not-settled/,
  );
  assert.match(
    case52NormalTerminalFailures(terminalProjection({ overlay: "permission" }), options).join(","),
    /blocking-interaction-visible/,
  );
  assert.match(
    case52NormalTerminalFailures(terminalProjection({ draft_prompt: "stale" }), options).join(","),
    /composer-not-rearmed/,
  );
  const wrongRevision = terminalProjection();
  wrongRevision.session_rows[0].admission_revision = "8";
  assert.match(case52NormalTerminalFailures(wrongRevision, options).join(","), /terminal-admission-revision-mismatch/);
});

test("case5_2 terminal permits only an explicitly owned restart command palette", () => {
  const palette = terminalProjection({ overlay: "command_palette" });
  const options = { allowedOverlay: "command_palette" };
  assert.equal(classifyCase52NormalTerminal(palette).decision, "fail");
  assert.deepEqual(classifyCase52NormalTerminal(palette, options), { decision: "pass", failures: [] });
  assert.equal(classifyCase52NormalTerminal({ ...palette, overlay: "none" }, options).decision, "fail");
  assert.equal(classifyCase52NormalTerminal({ ...palette, overlay: "config" }, options).decision, "fail");
  assert.equal(classifyCase52NormalTerminal({
    ...palette,
    navigation_loading: true,
    turn_page_admission_open: false,
    pending_async_operations: ["turn_page_load"],
  }, options).decision, "pending");
  for (const drift of [
    { confirmation_visible: true },
    { confirmation_id: "restart-confirmation" },
    { confirmation: { id: "restart-confirmation" } },
  ]) {
    assert.equal(classifyCase52NormalTerminal({ ...palette, ...drift }, options).decision, "fail");
  }
  assert.equal(classifyCase52NormalTerminal({ ...palette, run_status_key: "failed" }, options).decision, "fail");
  assert.throws(
    () => classifyCase52NormalTerminal(palette, { allowedOverlay: "any" }),
    /allowedOverlay/,
  );
});

test("case5_2 terminal classifier fail-stops durable failure and immutable session drift", () => {
  assert.equal(classifyCase52NormalTerminal(terminalProjection({ run_status_key: "running" })).decision, "pending");
  assert.equal(classifyCase52NormalTerminal(terminalProjection({ run_status_key: "failed" })).decision, "fail");
  assert.equal(classifyCase52NormalTerminal(terminalProjection({ run_status_key: "cancelled" })).decision, "fail");
  const other = terminalProjection();
  other.session_rows[0].session_id = "01K3OTHERSESSION00000000000";
  assert.equal(classifyCase52NormalTerminal(other, { expectedSessionId: SESSION_ID }).decision, "fail");
});

test("case5_2 terminal classifier waits only for explicit active work", () => {
  for (const projection of [
    terminalProjection({ run_status_key: "running" }),
    terminalProjection({ task_activity_state: "finalizing" }),
    terminalProjection({ post_run_refresh_pending: true }),
    terminalProjection({ background_mutation_pending: true }),
    terminalProjection({ async_polling_required: true }),
    terminalProjection({ pending_async_operations: ["post-run-refresh"] }),
    terminalProjection({ navigation_loading: true }),
    terminalProjection({ provider_loading: true }),
  ]) {
    assert.equal(classifyCase52NormalTerminal(projection).decision, "pending");
  }

  assert.equal(classifyCase52NormalTerminal(null).decision, "fail");
  assert.equal(classifyCase52NormalTerminal(terminalProjection({ run_status_key: "idle" })).decision, "fail");
  assert.equal(classifyCase52NormalTerminal(terminalProjection({ navigation_admission_open: false })).decision, "fail");
  assert.equal(classifyCase52NormalTerminal(terminalProjection({ pending_async_operations: null })).decision, "fail");
});

test("case5_2 settled acquired terminal mismatches are product failures", () => {
  const expected = {
    expectedSessionId: SESSION_ID,
    expectedTurnId: TURN_ID,
    expectedPrompt: "stage prompt",
  };
  const cases = [
    ["wrong-session", (() => {
      const projection = terminalProjection();
      projection.session_rows[0].session_id = "01K3OTHERSESSION00000000000";
      return projection;
    })(), "selected-session-id-mismatch"],
    ["wrong-turn", terminalProjection({
      run_target: { expectedState: { kind: "idle", latestTurnId: "01K3OTHERTURN0000000000000", admissionRevision: "7" } },
    }), "terminal-turn-id-mismatch"],
    ["wrong-prompt", terminalProjection({
      transcript_rows: transcript().map((row) => row.row_kind === "user" ? { ...row, body: "different" } : row),
    }), "terminal-user-prompt-mismatch"],
    ["missing-summary", terminalProjection({
      transcript_rows: transcript().filter((row) => row.row_kind !== "work_summary_completed"),
    }), "completed-summary-missing"],
    ["wrong-admission", (() => {
      const projection = terminalProjection();
      projection.session_rows[0].admission_revision = "8";
      return projection;
    })(), "terminal-admission-revision-mismatch"],
    ["stale-composer", terminalProjection({ draft_prompt: "stale" }), "composer-not-rearmed"],
    ["blocking-overlay", terminalProjection({ overlay: "permission" }), "blocking-interaction-visible"],
  ];
  for (const [label, projection, expectedFailure] of cases) {
    const decision = classifyCase52NormalTerminal(projection, expected);
    assert.equal(decision.decision, "fail", label);
    assert.equal(decision.failures.includes(expectedFailure), true, label);
  }

  const durableInterrupted = terminalProjection({ run_status_key: "running", busy: true });
  durableInterrupted.session_rows[0].status = "cancelled";
  assert.equal(
    classifyCase52NormalTerminal(durableInterrupted, expected).decision,
    "fail",
    "a durable selected-session interruption outranks transient active flags",
  );
});

test("case5_2 Stage 2-4 terminals reject summaries that belong only to past turns", () => {
  const turnIds = [
    TURN_ID,
    "01K3CASE52TURN2000000000000",
    "01K3CASE52TURN3000000000000",
    "01K3CASE52TURN4000000000000",
  ];
  for (let stageIndex = 1; stageIndex < turnIds.length; stageIndex += 1) {
    const expectedTurnId = turnIds[stageIndex];
    const expectedPrompt = `stage ${stageIndex + 1} prompt`;
    const pastSummaries = turnIds.slice(0, stageIndex).map((turnId) => ({
      row_kind: "work_summary_completed",
      stable_history_identity: `turn:${turnId}:work-summary`,
      body: "",
    }));
    const projection = terminalProjection({
      run_target: {
        expectedState: { kind: "idle", latestTurnId: expectedTurnId, admissionRevision: "7" },
      },
      transcript_rows: [
        ...pastSummaries,
        {
          row_kind: "user",
          stable_history_identity: `history-user-stage-${stageIndex + 1}`,
          body: expectedPrompt,
        },
      ],
    });
    const classified = classifyCase52NormalTerminal(projection, {
      expectedSessionId: SESSION_ID,
      expectedTurnId,
      expectedPrompt,
      minimumCompletedSummaryCount: 1,
    });
    assert.equal(classified.decision, "fail", `stage ${stageIndex + 1}`);
    assert.equal(
      classified.failures.includes("completed-summary-missing"),
      true,
      `stage ${stageIndex + 1}`,
    );
  }
});

test("restart continuity compares durable conversation turns and ignores runtime-only rows", () => {
  const before = transcript();
  const value = {
    beforeSessionId: SESSION_ID,
    afterSessionId: SESSION_ID,
    beforeHistory: before,
    afterHistory: [...before, { row_kind: "user", stable_history_identity: "history-user-2", body: "stage 4" }],
  };
  assert.equal(case52RestartContinuityAccepted(value), true);
  assert.deepEqual(case52RestartContinuityFailures(value), []);
  assert.match(
    case52RestartContinuityFailures({ ...value, afterSessionId: "other" }).join(","),
    /restart-session-id-mismatch/,
  );
  assert.match(
    case52RestartContinuityFailures({ ...value, afterHistory: [] }).join(","),
    /restart-history-truncated/,
  );
  const rewritten = structuredClone(before);
  rewritten[0].body = "rewritten";
  assert.match(
    case52RestartContinuityFailures({ ...value, afterHistory: rewritten }).join(","),
    /restart-history-prefix-mismatch/,
  );

  const live = [
    { row_kind: "user", stable_history_identity: "user-1", body: "stage 1" },
    { row_kind: "assistant", body: "first complete" },
    { row_kind: "user", stable_history_identity: "user-2", body: "stage 2" },
    { row_kind: "error", stable_history_identity: "error-1", body: "first durable tool failure" },
    { row_kind: "error", stable_history_identity: "error-2", body: "second durable tool failure" },
    { row_kind: "assistant", body: "second complete" },
    { row_kind: "user", stable_history_identity: "user-3", body: "stage 3" },
    { row_kind: "system", body: "display-only runtime notice" },
    { row_kind: "work_summary_completed", body: "live summary" },
    { row_kind: "assistant", body: "canonical final pre" },
  ];
  const reopened = [
    { row_kind: "user", stable_history_identity: "user-1", body: "stage 1" },
    { row_kind: "assistant", body: "first complete" },
    { row_kind: "user", stable_history_identity: "user-2", body: "stage 2" },
    { row_kind: "error", stable_history_identity: "error-1", body: "first durable tool failure" },
    { row_kind: "error", stable_history_identity: "error-2", body: "second durable tool failure" },
    { row_kind: "assistant", body: "second complete" },
    { row_kind: "user", stable_history_identity: "user-3", body: "stage 3" },
    { row_kind: "work_summary_completed", body: "canonical summary" },
    { row_kind: "assistant", body: "canonical final prefix completed" },
    { row_kind: "file_changes", body: "two files" },
  ];
  assert.deepEqual(case52RestartContinuityFailures({
    beforeSessionId: SESSION_ID,
    afterSessionId: SESSION_ID,
    beforeHistory: live,
    afterHistory: reopened,
  }), []);
  const olderAssistantRewrite = structuredClone(reopened);
  olderAssistantRewrite[1].body = "first rewritten";
  assert.match(case52RestartContinuityFailures({
    beforeSessionId: SESSION_ID,
    afterSessionId: SESSION_ID,
    beforeHistory: live,
    afterHistory: olderAssistantRewrite,
  }).join(","), /restart-history-prefix-mismatch/);
  const missingError = reopened.filter((row) => row.stable_history_identity !== "error-1");
  assert.match(case52RestartContinuityFailures({
    beforeSessionId: SESSION_ID,
    afterSessionId: SESSION_ID,
    beforeHistory: live,
    afterHistory: missingError,
  }).join(","), /restart-history-prefix-mismatch/);
  const rewrittenError = structuredClone(reopened);
  rewrittenError.find((row) => row.stable_history_identity === "error-1").body = "rewritten";
  assert.match(case52RestartContinuityFailures({
    beforeSessionId: SESSION_ID,
    afterSessionId: SESSION_ID,
    beforeHistory: live,
    afterHistory: rewrittenError,
  }).join(","), /restart-history-prefix-mismatch/);
  const reorderedErrors = structuredClone(reopened);
  [reorderedErrors[3], reorderedErrors[4]] = [reorderedErrors[4], reorderedErrors[3]];
  assert.match(case52RestartContinuityFailures({
    beforeSessionId: SESSION_ID,
    afterSessionId: SESSION_ID,
    beforeHistory: live,
    afterHistory: reorderedErrors,
  }).join(","), /restart-history-prefix-mismatch/);
  const changedUserIdentity = structuredClone(reopened);
  changedUserIdentity[0].stable_history_identity = "different-user";
  assert.match(case52RestartContinuityFailures({
    beforeSessionId: SESSION_ID,
    afterSessionId: SESSION_ID,
    beforeHistory: live,
    afterHistory: changedUserIdentity,
  }).join(","), /restart-history-prefix-mismatch/);
});

test("restart bounded page classifier accepts the exact latest suffix and every previous transition", () => {
  const expected = {
    expectedSessionId: SESSION_ID,
    expectedTurnId: TURN_ID,
    expectedAdmissionRevision: "7",
    expectedTotal: 529,
    expectedLimit: 80,
  };
  const latest = terminalProjection({
    turn_page_offset: 449,
    turn_page_limit: 80,
    turn_page_total: 529,
    turn_page_has_more: false,
  });
  assert.deepEqual(classifyCase52RestartTurnPage(latest, {
    ...expected,
    requireLatestSuffix: true,
  }), {
    decision: "page_needed",
    failures: [],
    metadata: { offset: 449, limit: 80, total: 529, has_more: false },
  });

  const offsets = [449, 369, 289, 209, 129, 49, 0];
  for (let index = 0; index < offsets.length - 1; index += 1) {
    assert.deepEqual(case52RestartPreviousPageTransitionFailures({
      before: { offset: offsets[index], limit: 80, total: 529, has_more: false },
      after: { offset: offsets[index + 1], limit: 80, total: 529, has_more: false },
    }), []);
  }
  assert.equal(classifyCase52RestartTurnPage(terminalProjection({
    turn_page_offset: 0,
    turn_page_limit: 80,
    turn_page_total: 529,
    turn_page_has_more: false,
  }), expected).decision, "ready");
  assert.match(case52RestartPreviousPageTransitionFailures({
    before: { offset: 449, limit: 80, total: 529, has_more: false },
    after: { offset: 449, limit: 80, total: 529, has_more: false },
  }).join(","), /restart-turn-page-offset-drift/);
  assert.equal(classifyCase52RestartTurnPage({ ...latest, turn_page_total: 530 }, {
    ...expected,
    requireLatestSuffix: true,
  }).decision, "fail");
  assert.equal(classifyCase52RestartTurnPage(terminalProjection({
    turn_page_offset: 0,
    turn_page_limit: 529,
    turn_page_total: 529,
    turn_page_has_more: false,
  }), {
    ...expected,
    requireLatestSuffix: true,
  }).decision, "fail");
});

test("restart history target classifier waits for WebView rerender after every page settlement", () => {
  assert.deepEqual(classifyCase52RestartHistoryTarget({ observation: { count: 0 } }), {
    decision: "pending",
    failures: [],
  });
  assert.deepEqual(classifyCase52RestartHistoryTarget({
    observation: {
      count: 1,
      connected: true,
      visible: true,
      enabled: false,
      identity: { tag: "BUTTON", action: "load-previous-turn-page" },
    },
  }), {
    decision: "pending",
    failures: [],
  });
  assert.deepEqual(classifyCase52RestartHistoryTarget({
    observation: {
      count: 1,
      connected: true,
      visible: true,
      enabled: true,
      identity: { tag: "BUTTON", action: "load-previous-turn-page" },
    },
  }), {
    decision: "pass",
    failures: [],
  });
  assert.match(classifyCase52RestartHistoryTarget({ observation: { count: 2 } }).failures.join(","), /cardinality/);
});

test("restart history target settlement delays the next trusted action until the exact row rerenders", async () => {
  const observations = [
    { count: 0 },
    {
      count: 1,
      connected: true,
      visible: true,
      enabled: false,
      identity: { tag: "BUTTON", action: "load-previous-turn-page" },
    },
    {
      count: 1,
      connected: true,
      visible: true,
      enabled: true,
      identity: { tag: "BUTTON", action: "load-previous-turn-page" },
    },
  ];
  let calls = 0;
  const settled = await waitForCase52RestartHistoryTarget({
    input: {
      async observeExactTarget() {
        const observation = observations[Math.min(calls, observations.length - 1)];
        calls += 1;
        return { locator: { selector: "previous" }, observation };
      },
    },
  });
  assert.equal(calls, 3);
  assert.equal(settled.value.classified.decision, "pass");

  await assert.rejects(
    () => waitForCase52RestartHistoryTarget({
      input: {
        async observeExactTarget() {
          return { locator: { selector: "previous" }, observation: { count: 2 } };
        },
      },
    }),
    (error) => error?.owner === "product" && error?.code === "case5_2-restart-history-target",
  );
});

test("restart decision exposes complete terminal and continuity reasons", () => {
  const options = {
    beforeSessionId: SESSION_ID,
    beforeHistory: transcript(),
    expectedTurnId: TURN_ID,
    expectedPrompt: "stage prompt",
  };
  assert.deepEqual(classifyCase52RestartContinuity(terminalProjection(), options), {
    decision: "pass",
    failures: [],
    terminal_failures: [],
    continuity_failures: [],
  });

  const loading = terminalProjection({
    post_run_refresh_pending: true,
    transcript_rows: transcript().slice(1),
  });
  const pending = classifyCase52RestartContinuity(loading, options);
  assert.equal(pending.decision, "pending");
  assert.equal(pending.terminal_failures.includes("projection-not-settled"), true);
  assert.equal(pending.continuity_failures.includes("restart-history-invalid"), true);
  assert.equal(pending.failures.includes("restart-history-invalid"), true);

  const rewritten = terminalProjection();
  rewritten.transcript_rows[0] = { ...rewritten.transcript_rows[0], body: "rewritten" };
  const persistent = classifyCase52RestartContinuity(rewritten, options);
  assert.equal(persistent.decision, "fail");
  assert.deepEqual(persistent.terminal_failures, ["terminal-user-prompt-mismatch"]);
  assert.deepEqual(persistent.continuity_failures, ["restart-history-prefix-mismatch"]);
  assert.deepEqual(persistent.failures, [
    "terminal-user-prompt-mismatch",
    "restart-history-prefix-mismatch",
  ]);

  const failed = classifyCase52RestartContinuity(
    terminalProjection({ run_status_key: "failed", busy: true }),
    options,
  );
  assert.equal(failed.decision, "fail");
  assert.equal(failed.terminal_failures.includes("run-not-completed"), true);
});

test("Stage 1 permits exactly four root documents and at least 25 evidence rows", () => {
  assert.deepEqual(case52Stage1ManifestFailures(stage1Manifest()), []);
  assert.match(
    case52Stage1ManifestFailures(stage1Manifest({ evidence_matrix_rows: 24 })).join(","),
    /stage1-evidence-matrix-too-small/,
  );
  assert.match(
    case52Stage1ManifestFailures(stage1Manifest({ diff: { modified: ["backend/app.py"], deleted: [], added: [] } })).join(","),
    /stage-baseline-file-modified/,
  );
  assert.match(
    case52Stage1ManifestFailures(stage1Manifest({ documents: documents(true) })).join(","),
    /stage1-cancel-contract-created-early/,
  );
});

test("Stage 2 preserves Stage 1 bytes and adds only cancel_contract.md", () => {
  const first = stage1Manifest();
  assert.deepEqual(case52Stage2ManifestFailures(first, stage2Manifest()), []);

  const rewritten = stage2Manifest();
  rewritten.files = rewritten.files.map((row) => row.path === "README.md" ? file("README.md", "rewritten") : row);
  assert.match(
    case52Stage2ManifestFailures(first, rewritten).join(","),
    /stage2-only-cancel-contract-not-preserved/,
  );
  assert.match(
    case52Stage2ManifestFailures(first, stage2Manifest({ baseline_aggregate_sha256: "different" })).join(","),
    /stage-baseline-identity-mismatch/,
  );
  const extra = stage2Manifest();
  extra.files.push(file("unexpected.md"));
  extra.diff.added.push("unexpected.md");
  assert.match(case52Stage2ManifestFailures(first, extra).join(","), /stage-added-paths-mismatch/);
});

test("evaluator acceptance requires both exact exit codes and every non-empty document", () => {
  assert.equal(case52EvaluatorAccepted(evaluationReport()), true);
  assert.deepEqual(case52EvaluatorFailures(evaluationReport()), []);
  assert.match(
    case52EvaluatorFailures(evaluationReport({ public_suite: { exit_code: 1 }, public_suite_pass: false })).join(","),
    /public-suite-failed/,
  );
  assert.match(
    case52EvaluatorFailures(evaluationReport({ hidden_oracle: { exit_code: 1 }, hidden_oracle_pass: false })).join(","),
    /hidden-oracle-failed/,
  );
  const emptyDocument = documents(true);
  emptyDocument[4].bytes = 0;
  assert.match(
    case52EvaluatorFailures(evaluationReport({ documents: emptyDocument })).join(","),
    /evaluation-document-missing:cancel_contract\.md/,
  );
});

test("non-convergence cutoff is exclusive at ten minutes and requires zero artifacts plus three repeats", () => {
  const base = {
    elapsedMs: 600_001,
    requiredArtifactCount: 0,
    repeatedNextActionCount: 3,
    repeatedSourceReadCount: 0,
  };
  assert.equal(classifyCase52NonConvergence(base).decision, "stop");
  assert.equal(classifyCase52NonConvergence({ ...base, elapsedMs: 600_000 }).decision, "continue");
  assert.equal(classifyCase52NonConvergence({ ...base, requiredArtifactCount: 1 }).decision, "continue");
  assert.equal(classifyCase52NonConvergence({ ...base, repeatedNextActionCount: 2 }).decision, "continue");
  assert.equal(classifyCase52NonConvergence({ ...base, repeatedNextActionCount: 0, repeatedSourceReadCount: 3 }).decision, "stop");
  assert.throws(
    () => classifyCase52NonConvergence({ ...base, repeatedSourceReadCount: -1 }),
    /non-negative integer/,
  );
});

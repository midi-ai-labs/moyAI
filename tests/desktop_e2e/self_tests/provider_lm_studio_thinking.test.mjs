import assert from "node:assert/strict";
import crypto from "node:crypto";
import os from "node:os";
import path from "node:path";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import test from "node:test";

import {
  LM_STUDIO_THINKING_ARTIFACT_CONTENT,
  LM_STUDIO_THINKING_ARTIFACT_NAME,
  LM_STUDIO_THINKING_FINAL,
  LM_STUDIO_THINKING_FORBIDDEN_WIRE_KEYS,
  LM_STUDIO_THINKING_HOST_OWNED_CONFIG_KEYS,
  LM_STUDIO_THINKING_PLAN_STEPS,
  LM_STUDIO_THINKING_PROMPT,
  createLmStudioThinkingScenario,
  expectedLmStudioThinkingGlobalSave,
  inspectLmStudioThinkingRequestCaptures,
  lmStudioThinkingActivePlanAccepted,
  lmStudioThinkingControlTokenLeaks,
  lmStudioThinkingFixtureConfig,
  lmStudioThinkingRequestCaptureFailures,
  lmStudioThinkingTerminalDecision,
  lmStudioThinkingTerminalFailures,
  normalizeLmStudioThinkingOptions,
  restoredLmStudioThinkingConnectionReady,
} from "../scenarios/provider_lm_studio_thinking.mjs";

const RAW_OPTIONS = Object.freeze({
  provider_base_url: "http://127.0.0.1:1234/",
  model: " qwen/qwen3.8-27b ",
});
const OPTIONS = Object.freeze({
  providerBaseUrl: "http://127.0.0.1:1234",
  model: "qwen/qwen3.8-27b",
});
const CONFIG_TARGET = Object.freeze({
  workspacePath: "C:\\workspace",
  sessionId: null,
  configGeneration: "42",
});
const CONFIG_FIELDS = Object.freeze([
  { key: "model.base_url", value: OPTIONS.providerBaseUrl },
  { key: "model.model", value: OPTIONS.model },
  { key: "model.provider_profile", value: "lm_studio" },
  { key: "model.api_key_env", value: "" },
  { key: "model.supports_tools", value: "true" },
  { key: "permissions.access_mode", value: "default" },
]);

function settingsSurface({ projection = {}, settings = {}, surface = {} } = {}) {
  return {
    projection: {
      overlay: "config",
      config_target: structuredClone(CONFIG_TARGET),
      config_fields: structuredClone(CONFIG_FIELDS),
      provider_effective_base_url: OPTIONS.providerBaseUrl,
      provider_effective_model_id: OPTIONS.model,
      provider_effective_profile: "lm_studio",
      provider_effective_api_key_env: "",
      ...projection,
    },
    settings: {
      dialog: { count: 1, visible: true },
      host_owned_config_key_counts: Object.fromEntries(
        LM_STUDIO_THINKING_HOST_OWNED_CONFIG_KEYS.map((key) => [key, 0]),
      ),
      base_url: { count: 1, visible: true, enabled: true, value: OPTIONS.providerBaseUrl },
      profile: {
        count: 1,
        visible: true,
        enabled: true,
        value: "lm_studio",
        options: ["lm_studio", "openai_compatible"],
      },
      model_details: { count: 1, visible: true, open: true },
      model: { count: 1, visible: true, enabled: true, value: OPTIONS.model },
      dirty: false,
      save: { count: 1, visible: true, enabled: false },
      close: { count: 1, visible: true, enabled: true },
      ...settings,
    },
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
    visible_validation_error_count: 0,
    visible_transcript_error_count: 0,
    ...surface,
  };
}

function preparedResponsesBody(overrides = {}) {
  return {
    model: OPTIONS.model,
    instructions: "bounded system instructions",
    input: [{ type: "message", role: "user", content: [{ type: "input_text", text: "task" }] }],
    tools: [{ type: "function", name: "update_plan", description: "plan", parameters: { type: "object" } }],
    tool_choice: "auto",
    parallel_tool_calls: false,
    store: false,
    stream: true,
    ...overrides,
  };
}

function captureRecord({ body = preparedResponsesBody(), metadata = {}, index = 0 } = {}) {
  const requestBytes = Buffer.from(JSON.stringify(body), "utf8");
  const stem = `00000000000000000001-0000000001-${String(index).padStart(10, "0")}-responses`;
  const requestBodyFile = `${stem}.request.json`;
  return {
    metadata_file: `${stem}.metadata.json`,
    request_body_file: requestBodyFile,
    request_body_bytes: requestBytes.byteLength,
    request_body_sha256: crypto.createHash("sha256").update(requestBytes).digest("hex"),
    body,
    metadata: {
      schema_version: 2,
      transport: "http",
      capture_stage: "prepared",
      request_id: `request-${index}`,
      captured_at_unix_ms: 1,
      process_id: 1,
      sequence: index,
      api_mode: "responses",
      endpoint_path: "v1/responses",
      request_body_file: requestBodyFile,
      request_body_bytes: requestBytes.byteLength,
      ...metadata,
    },
  };
}

async function writeCapturePair(directory, capture) {
  const bodyBytes = Buffer.from(JSON.stringify(capture.body), "utf8");
  const metadata = {
    ...capture.metadata,
    request_body_bytes: bodyBytes.byteLength,
  };
  await writeFile(path.join(directory, capture.request_body_file), bodyBytes, { flag: "wx" });
  await writeFile(
    path.join(directory, capture.metadata_file),
    Buffer.from(JSON.stringify(metadata), "utf8"),
    { flag: "wx" },
  );
}

function planDom(statuses) {
  const statusText = { in_progress: "進行中", pending: "未着手", completed: "完了" };
  return {
    count: 1,
    visible: true,
    heading: { count: 1, visible: true, text: "計画" },
    count_text: { count: 1, visible: true, text: "2件" },
    items: LM_STUDIO_THINKING_PLAN_STEPS.map((step, index) => ({
      status: statuses[index],
      step,
      status_text: statusText[statuses[index]],
      visible: true,
    })),
  };
}

function activeSurface() {
  return {
    projection: {
      run_status_key: "running",
      plan: {
        steps: LM_STUDIO_THINKING_PLAN_STEPS.map((step, index) => ({
          step,
          status: index === 0 ? "in_progress" : "pending",
        })),
      },
      transcript_rows: [{ row_kind: "user", body: LM_STUDIO_THINKING_PROMPT }],
    },
    plan_dom: planDom(["in_progress", "pending"]),
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
    visible_validation_error_count: 0,
    visible_transcript_error_count: 0,
  };
}

function artifactIdentity() {
  const bytes = Buffer.from(LM_STUDIO_THINKING_ARTIFACT_CONTENT, "utf8");
  return {
    name: LM_STUDIO_THINKING_ARTIFACT_NAME,
    content: LM_STUDIO_THINKING_ARTIFACT_CONTENT,
    size_bytes: bytes.byteLength,
    sha256: crypto.createHash("sha256").update(bytes).digest("hex"),
    workspace_entries: ["E2E_LM_STUDIO_THINKING.txt", LM_STUDIO_THINKING_ARTIFACT_NAME],
  };
}

function terminalSurface(overrides = {}) {
  const projection = {
    run_status_key: "completed",
    task_activity_state: "idle",
    busy: false,
    agent_tree_active: false,
    post_run_refresh_pending: false,
    background_mutation_pending: false,
    async_polling_required: false,
    pending_async_operations: [],
    navigation_loading: false,
    provider_loading: false,
    overlay: "none",
    confirmation_visible: false,
    confirmation_id: null,
    confirmation: null,
    draft_prompt: "",
    composer_submit_mode: "new_request",
    can_submit: true,
    plan: {
      steps: LM_STUDIO_THINKING_PLAN_STEPS.map((step) => ({ step, status: "completed" })),
    },
    tool_status_text: "ツール:\n- Plan updated [completed] Plan updated\n- Applied 1 change(s) [completed] Added THINKING_SMOKE.md\n- Plan updated [completed] Plan updated",
    progress_text: "Completed\nツール: 3件開始 / 3件完了 / 0件拒否 / 0件キャンセル / 0件失敗",
    transcript_rows: [
      { row_kind: "user", body: LM_STUDIO_THINKING_PROMPT },
      { row_kind: "work_summary_completed", body: "3件のツールを完了しました" },
      { row_kind: "assistant", body: LM_STUDIO_THINKING_FINAL },
    ],
    artifact_rows: [{
      label: LM_STUDIO_THINKING_ARTIFACT_NAME,
      path: LM_STUDIO_THINKING_ARTIFACT_NAME,
      action: "追加",
    }],
    file_change_rows: [{
      label: LM_STUDIO_THINKING_ARTIFACT_NAME,
      path: LM_STUDIO_THINKING_ARTIFACT_NAME,
      action: "追加",
    }],
    ...overrides.projection,
  };
  return {
    projection,
    assistants: [{ text: LM_STUDIO_THINKING_FINAL, visible: true }],
    reasoning_summaries: [],
    plan_dom: planDom(["completed", "completed"]),
    artifact_dom: {
      heading: { count: 1, visible: true, text: "ファイル" },
      rows: [{ label: LM_STUDIO_THINKING_ARTIFACT_NAME, path: LM_STUDIO_THINKING_ARTIFACT_NAME, visible: true }],
    },
    terminal_dom: {
      visible_run_strip_count: 0,
      visible_task_activity_indicator_count: 0,
      visible_selected_activity_row_count: 0,
    },
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
    visible_validation_error_count: 0,
    visible_transcript_error_count: 0,
    ...overrides.surface,
  };
}

test("LM Studio thinking options accept only one credential-free endpoint and model", () => {
  assert.deepEqual(normalizeLmStudioThinkingOptions(RAW_OPTIONS), OPTIONS);
  for (const invalid of [
    null,
    [],
    {},
    { provider_base_url: "ftp://127.0.0.1:1234", model: "qwen" },
    { provider_base_url: "http://user@127.0.0.1:1234", model: "qwen" },
    { provider_base_url: "http://127.0.0.1:1234?token=x", model: "qwen" },
    { provider_base_url: "http://127.0.0.1:1234", model: "" },
    { provider_base_url: "http://127.0.0.1:1234", model: "qwen\ninvalid" },
    { provider_base_url: "http://127.0.0.1:1234", model: "qwen", unload: true },
  ]) {
    assert.throws(() => normalizeLmStudioThinkingOptions(invalid), TypeError);
  }
});

test("fixture leaves LM Studio sampling and thinking at host defaults", () => {
  const config = lmStudioThinkingFixtureConfig();
  assert.match(config, /provider_profile = "lm_studio"/);
  assert.match(config, /provider_metadata_mode = "lm_studio_native_required"/);
  assert.match(config, /provider_api_mode = "responses"/);
  assert.doesNotMatch(config, /^api_key_env\s*=/m);
  assert.doesNotMatch(config, /^max_output_tokens\s*=/m);
  assert.doesNotMatch(config, /reasoning_(?:effort|summary)|supports_reasoning|temperature|top_p|top_k|presence_penalty|frequency_penalty|seed\s*=|stop(?:_sequences)?\s*=|\[model\.extra_body_json\]/);
  assert.match(config, /supports_tools = true/);
  assert.match(config, /access_mode = "default"/);
  assert.match(config, /\[format\]\ndefault_newline = "lf"\nensure_trailing_newline = true/);
  assert.match(config, /max_retries = 0/);
  assert.doesNotMatch(config, /qwen\/qwen3\.8-27b|127\.0\.0\.1:1234/);
});

test("global Save preserves every non-generation fixture field and exact target", () => {
  const surface = settingsSurface({
    projection: {
      config_fields: CONFIG_FIELDS.map((field) => {
        if (field.key === "model.base_url") return { ...field, value: "http://127.0.0.1:9" };
        if (field.key === "model.model") return { ...field, value: "before" };
        return field;
      }),
    },
  });
  assert.deepEqual(expectedLmStudioThinkingGlobalSave(surface, OPTIONS), {
    command: "save_global_config",
    args: {
      values: CONFIG_FIELDS.map((field) => ({ key: field.key, text: field.value })),
      expectedTarget: CONFIG_TARGET,
    },
  });
});

test("typed Rust projection rejects reintroduced host-owned generation fields before global Save", () => {
  for (const key of LM_STUDIO_THINKING_HOST_OWNED_CONFIG_KEYS) {
    const surface = settingsSurface({
      projection: {
        config_fields: [...CONFIG_FIELDS, { key, value: "legacy-value" }],
      },
    });
    assert.equal(
      restoredLmStudioThinkingConnectionReady(surface, OPTIONS),
      false,
      `${key} must be absent from the typed Rust projection`,
    );
    assert.throws(
      () => expectedLmStudioThinkingGlobalSave(surface, OPTIONS),
      /save projection contains host-owned generation fields/,
      `${key} must fail closed before save command construction`,
    );
  }
});

test("restart persistence requires the exact LM Studio connection without client thinking control", () => {
  assert.equal(restoredLmStudioThinkingConnectionReady(settingsSurface(), OPTIONS), true);
  for (const key of LM_STUDIO_THINKING_HOST_OWNED_CONFIG_KEYS) {
    assert.equal(restoredLmStudioThinkingConnectionReady(settingsSurface({
      settings: {
        host_owned_config_key_counts: {
          ...settingsSurface().settings.host_owned_config_key_counts,
          [key]: 1,
        },
      },
    }), OPTIONS), false, `${key} must be absent from the actual Preferences dialog`);
  }
  assert.equal(restoredLmStudioThinkingConnectionReady(settingsSurface({
    projection: { provider_effective_profile: "openai_compatible" },
  }), OPTIONS), false);
});

test("prepared Responses capture contract rejects every client generation override and target drift", () => {
  assert.deepEqual(lmStudioThinkingRequestCaptureFailures([captureRecord()], OPTIONS), []);
  for (const key of LM_STUDIO_THINKING_FORBIDDEN_WIRE_KEYS) {
    const failures = lmStudioThinkingRequestCaptureFailures([
      captureRecord({ body: preparedResponsesBody({ [key]: key === "stop" ? ["STOP"] : true }) }),
    ], OPTIONS);
    assert.ok(failures.some((failure) => failure.code === "request-capture-generation-override-present"
      && failure.forbidden_keys.includes(key)), `${key} must fail closed`);
  }
  const targetFailures = lmStudioThinkingRequestCaptureFailures([
    captureRecord({ metadata: { api_mode: "chat_completions", endpoint_path: "v1/chat/completions" } }),
  ], OPTIONS);
  assert.ok(targetFailures.some((failure) => failure.code === "request-capture-target-mismatch"));
  assert.ok(targetFailures.some((failure) => failure.code === "request-capture-filename-owner-mismatch"));
  const structuralFailures = lmStudioThinkingRequestCaptureFailures([
    captureRecord({ body: preparedResponsesBody({ model: "wrong-model", input: [] }) }),
  ], OPTIONS);
  assert.ok(structuralFailures.some((failure) => failure.code === "request-capture-structural-contract-mismatch"));
  assert.deepEqual(
    lmStudioThinkingRequestCaptureFailures([], OPTIONS).map((failure) => failure.code),
    ["request-capture-empty"],
  );
});

test("prepared request capture inspection seals exact metadata/body pairs without retaining prompt text", async (context) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "moyai-lm-studio-thinking-capture-"));
  context.after(() => rm(root, { recursive: true, force: true }));
  const capture = captureRecord();
  await writeCapturePair(root, capture);

  const evidence = await inspectLmStudioThinkingRequestCaptures(root, OPTIONS);

  assert.equal(evidence.capture_count, 1);
  assert.deepEqual(evidence.failures, []);
  assert.equal(evidence.evidence_kind, "exact-prepared-outbound-body");
  assert.equal(evidence.network_attempt_proven, false);
  assert.equal(evidence.provider_receipt_proven, false);
  assert.deepEqual(evidence.captures[0].top_level_keys, Object.keys(capture.body).sort());
  assert.equal(evidence.captures[0].model_matches, true);
  assert.match(evidence.captures[0].instructions_sha256, /^[a-f0-9]{64}$/u);
  assert.equal(JSON.stringify(evidence).includes("bounded system instructions"), false);
  assert.equal(JSON.stringify(evidence).includes('"text":"task"'), false);
});

test("prepared request capture inspection classifies missing, malformed, unpaired, and forbidden evidence as product failures", async (context) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "moyai-lm-studio-thinking-capture-negative-"));
  context.after(() => rm(root, { recursive: true, force: true }));
  const missing = path.join(root, "missing");
  await assert.rejects(
    () => inspectLmStudioThinkingRequestCaptures(missing, OPTIONS),
    (error) => error?.owner === "product" && error?.code === "lm-studio-thinking-request-capture-missing",
  );

  const empty = path.join(root, "empty");
  await mkdir(empty);
  await assert.rejects(
    () => inspectLmStudioThinkingRequestCaptures(empty, OPTIONS),
    (error) => error?.owner === "product" && error?.code === "lm-studio-thinking-request-capture-empty",
  );

  const malformed = path.join(root, "malformed");
  await mkdir(malformed);
  const malformedStem = "00000000000000000001-0000000001-0000000000-responses";
  await writeFile(path.join(malformed, `${malformedStem}.request.json`), "{}", { flag: "wx" });
  await writeFile(path.join(malformed, `${malformedStem}.metadata.json`), "{", { flag: "wx" });
  await assert.rejects(
    () => inspectLmStudioThinkingRequestCaptures(malformed, OPTIONS),
    (error) => error?.owner === "product" && error?.code === "lm-studio-thinking-request-capture-invalid-json",
  );

  const orphan = path.join(root, "orphan");
  await mkdir(orphan);
  await writeFile(path.join(orphan, `${malformedStem}.request.json`), "{}", { flag: "wx" });
  await assert.rejects(
    () => inspectLmStudioThinkingRequestCaptures(orphan, OPTIONS),
    (error) => error?.owner === "product" && error?.code === "lm-studio-thinking-request-capture-orphan",
  );

  const dangling = path.join(root, "dangling");
  await mkdir(dangling);
  const danglingCapture = captureRecord();
  await writeFile(
    path.join(dangling, danglingCapture.metadata_file),
    JSON.stringify(danglingCapture.metadata),
    { flag: "wx" },
  );
  await assert.rejects(
    () => inspectLmStudioThinkingRequestCaptures(dangling, OPTIONS),
    (error) => error?.owner === "product" && error?.code === "lm-studio-thinking-request-capture-pair-mismatch",
  );

  const forbidden = path.join(root, "forbidden");
  await mkdir(forbidden);
  await writeCapturePair(forbidden, captureRecord({ body: preparedResponsesBody({ reasoning: { effort: "low" } }) }));
  await assert.rejects(
    () => inspectLmStudioThinkingRequestCaptures(forbidden, OPTIONS),
    (error) => error?.owner === "product"
      && error?.code === "lm-studio-thinking-request-capture-contract-mismatch"
      && error?.evidence?.failures?.some((failure) => failure.code === "request-capture-generation-override-present"),
  );
});

test("running plan requires exact typed steps and visible Japanese DOM statuses", () => {
  assert.equal(lmStudioThinkingActivePlanAccepted(activeSurface()), true);
  const drift = activeSurface();
  drift.plan_dom.items[1].status_text = "進行中";
  assert.equal(lmStudioThinkingActivePlanAccepted(drift), false);
});

test("terminal accepts host-provided reasoning summary but still requires exact plan, patch, and artifact", () => {
  const surface = terminalSurface();
  assert.deepEqual(lmStudioThinkingTerminalFailures(surface, artifactIdentity()), []);
  assert.equal(lmStudioThinkingTerminalDecision(surface, artifactIdentity()), "pass");

  const toolDrift = terminalSurface({
    projection: {
      tool_status_text: "ツール:\n- Plan updated [completed]\n- Applied 1 change(s) [completed]",
    },
  });
  assert.ok(lmStudioThinkingTerminalFailures(toolDrift, artifactIdentity()).includes("terminal-tool-projection-mismatch"));

  const reasoning = terminalSurface({
    projection: {
      transcript_rows: [
        { row_kind: "user", body: LM_STUDIO_THINKING_PROMPT },
        { row_kind: "reasoning_summary", body: "host-provided bounded summary" },
        { row_kind: "assistant", body: LM_STUDIO_THINKING_FINAL },
      ],
    },
    surface: { reasoning_summaries: [{ text: "host-provided bounded summary", visible: true }] },
  });
  assert.deepEqual(lmStudioThinkingTerminalFailures(reasoning, artifactIdentity()), []);

  const wrongBytes = { ...artifactIdentity(), content: `${LM_STUDIO_THINKING_ARTIFACT_CONTENT}extra` };
  assert.ok(lmStudioThinkingTerminalFailures(surface, wrongBytes).includes("terminal-artifact-bytes-mismatch"));

  const canonicalPath = LM_STUDIO_THINKING_ARTIFACT_NAME.toLowerCase();
  const canonicalizedSurface = terminalSurface({
    projection: {
      artifact_rows: [{ label: canonicalPath, path: canonicalPath, action: "追加" }],
      file_change_rows: [{ label: canonicalPath, path: canonicalPath, action: "追加" }],
    },
    surface: {
      artifact_dom: {
        heading: { count: 1, visible: true, text: "ファイル" },
        rows: [{ label: canonicalPath, path: canonicalPath, visible: true }],
      },
    },
  });
  assert.equal(
    lmStudioThinkingTerminalFailures(canonicalizedSurface, artifactIdentity()).includes("terminal-artifact-projection-mismatch"),
    true,
  );

  const physicalCaseDrift = {
    ...artifactIdentity(),
    name: canonicalPath,
    workspace_entries: ["E2E_LM_STUDIO_THINKING.txt", canonicalPath],
  };
  assert.ok(lmStudioThinkingTerminalFailures(surface, physicalCaseDrift).includes("terminal-artifact-bytes-mismatch"));
});

test("terminal waits for the DOM to catch up to an exact canonical final", () => {
  const streamingDom = terminalSurface({
    surface: {
      assistants: [{
        text: "THINKING_SMOKE.md を作成し、計画を完了",
        visible: true,
      }],
    },
  });
  assert.ok(
    lmStudioThinkingTerminalFailures(streamingDom, artifactIdentity()).includes("terminal-assistant-dom-not-exact"),
  );
  assert.equal(lmStudioThinkingTerminalDecision(streamingDom, artifactIdentity()), "pending");

  const canonicalDrift = terminalSurface({
    projection: {
      transcript_rows: [
        { row_kind: "user", body: LM_STUDIO_THINKING_PROMPT },
        { row_kind: "assistant", body: "canonical response drift" },
      ],
    },
  });
  assert.equal(lmStudioThinkingTerminalDecision(canonicalDrift, artifactIdentity()), "fail");
});

test("terminal waits for post-run refresh before judging canonical history and tool projection", () => {
  const preRefresh = terminalSurface({
    projection: {
      post_run_refresh_pending: true,
      async_polling_required: true,
      pending_async_operations: ["terminal_run_refresh", "snapshot_refresh"],
      composer_submit_mode: "blocked",
      can_submit: false,
      transcript_rows: [],
      tool_status_text: "ツール: 実行履歴はまだありません。",
      progress_text: "Completed\nツール: 3件開始 / 0件完了 / 0件拒否 / 0件キャンセル / 0件失敗",
    },
    surface: {
      terminal_dom: {
        visible_run_strip_count: 1,
        visible_task_activity_indicator_count: 0,
        visible_selected_activity_row_count: 1,
      },
    },
  });
  assert.equal(lmStudioThinkingTerminalDecision(preRefresh, artifactIdentity()), "pending");
});

test("visible model-output leak guard covers assistant and reasoning-summary rows", () => {
  for (const marker of ["<|im_start|>", "<|im_end|>", "<think>", "</think>"]) {
    const projection = {
      transcript_rows: [
        { row_kind: "tool", body: marker },
        { row_kind: "assistant", body: `visible ${marker} leak` },
      ],
    };
    assert.deepEqual(lmStudioThinkingControlTokenLeaks(projection), [{ row_index: 1, markers: [marker] }]);
    assert.deepEqual(lmStudioThinkingControlTokenLeaks({
      transcript_rows: [{ row_kind: "reasoning_summary", body: `visible ${marker} leak` }],
    }), [{ row_index: 0, markers: [marker] }]);
    const leaked = terminalSurface({
      projection: {
        transcript_rows: [
          { row_kind: "user", body: LM_STUDIO_THINKING_PROMPT },
          { row_kind: "assistant", body: `visible ${marker} leak` },
        ],
      },
    });
    assert.equal(lmStudioThinkingTerminalDecision(leaked, artifactIdentity()), "fail");
  }
  assert.deepEqual(lmStudioThinkingControlTokenLeaks({
    transcript_rows: [{ row_kind: "assistant", body: "<|tool_call|> is not ChatML framing" }],
  }), []);
});

test("scenario is reusable, database-backed, and reports an external unmanaged provider", async () => {
  const first = createLmStudioThinkingScenario(RAW_OPTIONS);
  const second = createLmStudioThinkingScenario(RAW_OPTIONS);
  assert.notEqual(first, second);
  assert.equal(first.id, "manual.provider-lm-studio-thinking");
  assert.equal(first.databaseRequired, true);
  assert.equal(first.manualGate, "not_required");
  const quiesced = await first.quiesce();
  assert.equal(quiesced.input, "pass");
  assert.deepEqual(quiesced.resources[0], {
    kind: "external-lm-studio-provider",
    provider_base_url: OPTIONS.providerBaseUrl,
    model: OPTIONS.model,
    owned_by_scenario: false,
    lifecycle: "already-loaded-unmanaged",
    cleanup_action: "none",
    generation_resources: [],
  });
});

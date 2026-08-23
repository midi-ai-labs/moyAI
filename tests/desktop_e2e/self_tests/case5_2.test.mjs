import assert from "node:assert/strict";
import crypto from "node:crypto";
import { createReadStream } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { mkdtemp, open, rename, rm, writeFile } from "node:fs/promises";

import {
  case52EvaluatorAccepted,
  case52EvaluatorFailures,
  case52NormalTerminalFailures,
  case52RestartContinuityAccepted,
  case52RestartContinuityFailures,
  case52Stage1ManifestFailures,
  case52Stage2ManifestFailures,
  classifyCase52NonConvergence,
  classifyCase52NormalTerminal,
  classifyCase52RestartContinuity,
} from "../case5_2_predicates.mjs";
import {
  assertCase52PhysicalFileIdentity,
  case52EvaluatorWorkspaceDiff,
  case52ForbiddenWorkspacePaths,
  case52FixtureConfig,
  case52PhysicalFileIdentity,
  case52ProviderCleanupPlan,
  normalizeCase52PromptText,
  normalizeCase52Options,
  readCase52ExternalOutput,
  settleCase52WorkspaceEvaluator,
  unloadMainProvider,
} from "../scenarios/case5_2.mjs";

const SESSION_ID = "01K3CASE52SESSION0000000000";
const TURN_ID = "01K3CASE52TURN000000000000";

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
  const config = case52FixtureConfig(normalized);
  assert.match(config, /model = "qwen\/qwen3\.6-27b"/);
  assert.match(config, /request_timeout_ms = 3600000/);
  assert.match(config, /context_window = 131072/);
  assert.match(config, /max_output_tokens = 32768/);
  assert.match(config, /num_ctx = 131072/);
  assert.match(config, /access_mode = "auto_review"/);
  assert.match(config, /\[multi_agent\]\nenabled = false/);
  assert.match(config, /\[docling\]\nenabled = false/);
  assert.match(config, /\[mcp\]\nenabled = false/);
  assert.doesNotMatch(config, /stream_idle_timeout_ms/);
  assert.throws(() => normalizeCase52Options({}), /fixture_source/);
  assert.throws(() => normalizeCase52Options({ ...case52OptionsForFailure(), run_number: 95 }), /unknown/);
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

test("restart continuity preserves the exact session and allows only a history suffix", () => {
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
    case52RestartContinuityFailures({ ...value, afterHistory: before.slice(1) }).join(","),
    /restart-history-truncated/,
  );
  const rewritten = structuredClone(before);
  rewritten[0].body = "rewritten";
  assert.match(
    case52RestartContinuityFailures({ ...value, afterHistory: rewritten }).join(","),
    /restart-history-prefix-mismatch/,
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
  assert.equal(pending.continuity_failures.includes("restart-history-truncated"), true);
  assert.equal(pending.failures.includes("restart-history-truncated"), true);

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

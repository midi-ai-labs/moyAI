import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { EventEmitter } from "node:events";
import { mkdtemp, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { PassThrough } from "node:stream";
import test from "node:test";
import { promisify } from "node:util";

import { DesktopE2eError } from "../core/execution.mjs";
import {
  PERMISSION_TEMP_ESCALATION_COMMAND,
  PERMISSION_TEMP_ESCALATION_GUARDIAN_DELAY_MS,
  PERMISSION_TEMP_ESCALATION_JUSTIFICATION,
  PERMISSION_TEMP_ESCALATION_LIVE_PROMPT,
  PERMISSION_TEMP_ESCALATION_LIVE_RESPONSE,
  PERMISSION_TEMP_ESCALATION_PROMPT,
  PERMISSION_TEMP_ESCALATION_REQUEST_TIMEOUT_MS,
  PERMISSION_TEMP_ESCALATION_RESPONSE,
  PERMISSION_TEMP_ESCALATION_TERMINAL_CONFIRMATION_MS,
  PERMISSION_TEMP_ESCALATION_TERMINAL_TIMEOUT_MS,
  createPermissionTempEscalationLiveTerminalDecision,
  createPermissionTempEscalationTerminalDecision,
  exactPermissionTempEscalationLedger,
  normalizePermissionTempEscalationLmStudioOptions,
  parsePermissionTempEscalationCaptureJson,
  permissionTempEscalationFixtureConfig,
  permissionTempEscalationHeldDecision,
  permissionTempEscalationHeldFailures,
  permissionTempEscalationCaptureFailureObservation,
  permissionTempEscalationLiveCleanupFailures,
  permissionTempEscalationLiveCaptureContract,
  permissionTempEscalationLiveFinalFailures,
  permissionTempEscalationLiveTerminalFailureEvidence,
  permissionTempEscalationLiveTerminalFailures,
  permissionTempEscalationLmStudioFixtureConfig,
  permissionTempEscalationPersistenceFailures,
  readPermissionTempEscalationPersistence,
  runReadOnlySqlite,
  permissionTempEscalationTerminalFailures,
  permissionTempEscalationTerminalFailureEvidence,
  permissionTempEscalationWorkspaceFailures,
} from "../scenarios/permission_temp_escalation.mjs";

const execFileAsync = promisify(execFile);

const SESSION_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const TURN_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const ROLES = Object.freeze([
  "temp_initial",
  "temp_escalation",
  "temp_guardian",
  "temp_continuation",
]);

function restrictedEvidence(overrides = {}) {
  return {
    pass: true,
    command_matches: true,
    host_non_success: true,
    shell_tool: true,
    completed_lifecycle: true,
    hint_kind: true,
    automatic_retry_false: true,
    exit_code_one: true,
    guidance_present: true,
    exact_escalation_hint: true,
    no_project_workaround_hint: true,
    single_note: true,
    same_line_windows_signature: true,
    output_size_bytes: 512,
    output_sha256: "a".repeat(64),
    ...overrides,
  };
}

function elevatedEvidence(overrides = {}) {
  return {
    pass: true,
    command_matches: true,
    exit_code_zero: true,
    pytest_passed: true,
    effect_temp_absent: true,
    sandbox_note_absent: true,
    output_size_bytes: 192,
    output_sha256: "b".repeat(64),
    ...overrides,
  };
}

function responseRow(role, overrides = {}) {
  const roleEvidence = role === "temp_escalation"
    ? { restricted_output: restrictedEvidence() }
    : role === "temp_continuation"
      ? { elevated_output: elevatedEvidence(), restricted_output: restrictedEvidence() }
      : role === "temp_guardian"
        ? { payload: { authority_count: 1, authority_matches: true } }
        : {};
  return {
    route: "responses",
    method: "POST",
    pathname: "/v1/responses",
    query_present: false,
    contract: { pass: true, role, role_evidence: roleEvidence },
    response_phase: "completed",
    response_status: 200,
    response_delay: role === "temp_guardian" ? {
      schema_version: "desktop-e2e.scripted-provider-guardian-delay.v1",
      configured_delay_ms: PERMISSION_TEMP_ESCALATION_GUARDIAN_DELAY_MS,
      delay_elapsed_ms: PERMISSION_TEMP_ESCALATION_GUARDIAN_DELAY_MS,
      delay_completed: true,
      peer_close_observed: false,
      headers_sent_elapsed_ms: PERMISSION_TEMP_ESCALATION_GUARDIAN_DELAY_MS,
    } : null,
    ...overrides,
  };
}

function ledger() {
  return [
    {
      route: "lm_studio_models",
      method: "GET",
      response_phase: "completed",
      response_status: 200,
    },
    ...ROLES.map((role) => responseRow(role)),
  ];
}

function baseProjection(overrides = {}) {
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
    provider_loading: false,
    overlay: "none",
    confirmation_visible: false,
    confirmation_id: null,
    draft_prompt: "",
    composer_submit_mode: "new_request",
    can_submit: true,
    progress_text: "Completed\nツール: 2件開始 / 2件完了 / 0件拒否 / 0件キャンセル / 0件失敗",
    tool_status_text: [
      "ツール:",
      "- first [completed] output",
      "- second [completed] output",
    ].join("\n"),
    draft_target: { sessionId: SESSION_ID },
    run_target: {
      sessionId: SESSION_ID,
      expectedState: {
        kind: "idle",
        latestTurnId: TURN_ID,
        admissionRevision: "2",
      },
    },
    transcript_rows: [
      { row_kind: "user", body: PERMISSION_TEMP_ESCALATION_PROMPT },
      {
        row_kind: "work_summary_completed",
        body: [
          "### 作業サマリ",
          "- 結果: セッションは完了しました。",
          "- コマンド/ツール: 4件",
          "",
          "### 作業履歴",
          "- [待機] shell",
          "- [完了] shell",
          "- [待機] shell",
          "- [完了] shell",
        ].join("\n"),
      },
      { row_kind: "assistant", body: PERMISSION_TEMP_ESCALATION_RESPONSE },
    ],
    ...overrides,
  };
}

function surface(projection = baseProjection(), overrides = {}) {
  return {
    projection,
    users: [{ visible: true, text: PERMISSION_TEMP_ESCALATION_PROMPT }],
    assistants: [{ visible: true, text: PERMISSION_TEMP_ESCALATION_RESPONSE }],
    completed_summaries: [{ visible: true, text: "completed" }],
    prompt: { count: 1, value: "", visible: true, enabled: true },
    send: { count: 1, visible: true, enabled: false },
    visible_fatal_count: 0,
    visible_recoverable_error_count: 0,
    visible_dialog_count: 0,
    visible_modal_backdrop_count: 0,
    ...overrides,
  };
}

function heldSample() {
  const projection = baseProjection({
    run_status_key: "running",
    task_activity_state: "running",
    busy: true,
    run_target: {
      sessionId: SESSION_ID,
      expectedState: {
        kind: "turn",
        turnId: TURN_ID,
        admissionRevision: "2",
      },
    },
    transcript_rows: [
      { row_kind: "user", body: PERMISSION_TEMP_ESCALATION_PROMPT },
      {
        row_kind: "work_summary_running",
        body: [
          "作業中",
          "実行中",
          "フェーズ",
          "Provider要求処理中",
          "手順",
          "Provider request",
          "request_in_flight",
          "モデル要求",
          "0",
        ].join("\n"),
      },
    ],
  });
  const heldLedger = ledger().slice(0, 3);
  heldLedger[2] = responseRow("temp_escalation", {
    response_phase: "held",
    response_status: null,
  });
  return {
    surface: surface(projection, {
      assistants: [],
      completed_summaries: [],
      prompt: { count: 1, value: "", visible: true, enabled: false },
      send: { count: 1, visible: true, enabled: true },
    }),
    ledger: heldLedger,
  };
}

function liveUser(text) {
  return {
    type: "message",
    role: "user",
    content: [{ type: "input_text", text }],
  };
}

function liveTaskBody(model, input) {
  return {
    model,
    instructions: "bounded live permission regression",
    input,
    tools: [{ type: "function", name: "shell", description: "shell", parameters: {} }],
    tool_choice: "auto",
    parallel_tool_calls: false,
    store: false,
    stream: true,
  };
}

function liveCaptureBodies(model = "qwen/example") {
  const restrictedCall = {
    type: "function_call",
    call_id: "call-live-restricted",
    name: "shell",
    arguments: JSON.stringify({
      command: PERMISSION_TEMP_ESCALATION_COMMAND,
      sandbox_permissions: "use_default",
    }),
  };
  const elevatedCall = {
    type: "function_call",
    call_id: "call-live-elevated",
    name: "shell",
    arguments: JSON.stringify({
      command: PERMISSION_TEMP_ESCALATION_COMMAND,
      sandbox_permissions: "require_escalated",
      justification: PERMISSION_TEMP_ESCALATION_JUSTIFICATION,
    }),
  };
  const restrictedOutput = [
    "Tool outcome (host projection): non-success",
    'tool: "shell"',
    "lifecycle_status: completed",
    "kind: workspace_write_effect_temp_access_denied",
    "automatic_retry: false",
    "exit_code: 1",
    "guidance: Sandbox note: exact command with sandbox_permissions=require_escalated; do not change project files solely to bypass this sandbox restriction",
    `Command: ${PERMISSION_TEMP_ESCALATION_COMMAND}`,
    "E PermissionError: [WinError 5] denied C:\\Temp\\moyai-sandbox-effect-ABC",
  ].join("\n");
  const elevatedOutput = [
    `Command: ${PERMISSION_TEMP_ESCALATION_COMMAND}`,
    "Exit code: 0",
    "1 passed in 0.10s",
  ].join("\n");
  const guardianPayload = {
    trusted_world_state: { schema_version: "fixture.v1" },
    task_context: JSON.stringify({
      authority_session_id: SESSION_ID,
      canonical_user_authority: [{
        kind: "user_turn",
        history_item_id: TURN_ID,
        text: PERMISSION_TEMP_ESCALATION_LIVE_PROMPT,
      }],
    }),
    recent_committed_response: {
      response_id: "response-live-elevated",
      assistant_text: "",
      tool_request: {
        call_id: elevatedCall.call_id,
        tool_name: elevatedCall.name,
        arguments_json: elevatedCall.arguments,
      },
      prior_committed_tool_results: [],
    },
    permission_request: {
      access: "shell",
      summary: "exact elevated retry",
      details: [`Requested sandbox elevation: ${PERMISSION_TEMP_ESCALATION_JUSTIFICATION}`],
      targets: ["C:/fixture/workspace"],
      outside_workspace: true,
      risks: [],
    },
    action_evidence: { kind: "permission_request" },
  };
  return [
    { body: liveTaskBody(model, [liveUser(PERMISSION_TEMP_ESCALATION_LIVE_PROMPT)]) },
    { body: liveTaskBody(model, [
      liveUser(PERMISSION_TEMP_ESCALATION_LIVE_PROMPT),
      restrictedCall,
      { type: "function_call_output", call_id: restrictedCall.call_id, output: restrictedOutput },
    ]) },
    { body: {
      model,
      instructions: "You are moyAI's independent permission guardian.",
      input: [liveUser(JSON.stringify(guardianPayload))],
      store: false,
      stream: true,
    } },
    { body: liveTaskBody(model, [
      liveUser(PERMISSION_TEMP_ESCALATION_LIVE_PROMPT),
      restrictedCall,
      { type: "function_call_output", call_id: restrictedCall.call_id, output: restrictedOutput },
      elevatedCall,
      { type: "function_call_output", call_id: elevatedCall.call_id, output: elevatedOutput },
    ]) },
  ];
}

test("permission TEMP escalation fixture is a provider-neutral AutoReview tool profile", () => {
  assert.ok(PERMISSION_TEMP_ESCALATION_GUARDIAN_DELAY_MS > 90_000);
  assert.ok(
    PERMISSION_TEMP_ESCALATION_GUARDIAN_DELAY_MS
      < PERMISSION_TEMP_ESCALATION_REQUEST_TIMEOUT_MS,
  );
  assert.ok(
    PERMISSION_TEMP_ESCALATION_REQUEST_TIMEOUT_MS
      < PERMISSION_TEMP_ESCALATION_TERMINAL_TIMEOUT_MS,
  );
  assert.ok(
    PERMISSION_TEMP_ESCALATION_TERMINAL_CONFIRMATION_MS
      < PERMISSION_TEMP_ESCALATION_TERMINAL_TIMEOUT_MS,
  );
  const config = permissionTempEscalationFixtureConfig("http://127.0.0.1:43123");
  assert.match(config, /base_url = "http:\/\/127\.0\.0\.1:43123"/u);
  assert.match(config, /provider_profile = "lm_studio"/u);
  assert.match(config, /supports_tools = true/u);
  assert.match(config, /\[permissions\][\s\S]*access_mode = "auto_review"/u);
  assert.match(config, /max_retries = 0/u);
  assert.match(
    config,
    new RegExp(`request_timeout_ms = ${PERMISSION_TEMP_ESCALATION_REQUEST_TIMEOUT_MS}`, "u"),
  );
  assert.doesNotMatch(
    config,
    /max_(?:output_)?tokens|reasoning_(?:effort|summary)|supports_reasoning|temperature|top_p|top_k|presence_penalty|frequency_penalty|seed\s*=|stop(?:_sequences)?\s*=|\[model\.extra_body_json\]/u,
  );
});

test("live LM Studio permission TEMP profile leaves generation and thinking settings host-owned", () => {
  const options = normalizePermissionTempEscalationLmStudioOptions({
    provider_base_url: "http://127.0.0.1:1234/",
    model: " qwen/example ",
  });
  assert.deepEqual(options, {
    providerBaseUrl: "http://127.0.0.1:1234",
    model: "qwen/example",
  });
  const config = permissionTempEscalationLmStudioFixtureConfig(options);
  assert.match(config, /provider_profile = "lm_studio"/u);
  assert.match(config, /provider_api_mode = "responses"/u);
  assert.match(config, /access_mode = "auto_review"/u);
  assert.match(config, /request_timeout_ms = 180000/u);
  assert.doesNotMatch(
    config,
    /max_(?:output_)?tokens|reasoning_(?:effort|summary)|supports_reasoning|temperature|top_p|top_k|presence_penalty|frequency_penalty|seed\s*=|stop(?:_sequences)?\s*=|\[model\.extra_body_json\]/u,
  );
  assert.throws(
    () => normalizePermissionTempEscalationLmStudioOptions({ model: "qwen/example" }),
    /provider_base_url must be a non-empty string/u,
  );
  assert.throws(
    () => normalizePermissionTempEscalationLmStudioOptions({
      provider_base_url: "http://127.0.0.1:1234",
      model: "qwen/example",
      extra: true,
    }),
    /unknown manual\.permission-temp-escalation-lm-studio option/u,
  );
});

test("live LM Studio capture contract requires exact restricted, Guardian, and elevated bodies", () => {
  const exact = liveCaptureBodies();
  assert.deepEqual(permissionTempEscalationLiveCaptureContract(exact, {
    model: "qwen/example",
  }).failures, []);

  const withDescriptions = (restrictedDescription, elevatedDescription) => {
    const describedCapture = structuredClone(exact);
    const restrictedArguments = {
      ...JSON.parse(describedCapture[1].body.input[1].arguments),
      description: restrictedDescription,
    };
    const restrictedArgumentsJson = JSON.stringify(restrictedArguments);
    describedCapture[1].body.input[1].arguments = restrictedArgumentsJson;
    describedCapture[3].body.input[1].arguments = restrictedArgumentsJson;
    const elevatedArguments = {
      ...JSON.parse(describedCapture[3].body.input[3].arguments),
      description: elevatedDescription,
    };
    const elevatedArgumentsJson = JSON.stringify(elevatedArguments);
    describedCapture[3].body.input[3].arguments = elevatedArgumentsJson;
    const describedGuardian = JSON.parse(describedCapture[2].body.input[0].content[0].text);
    describedGuardian.recent_committed_response.tool_request.arguments_json = elevatedArgumentsJson;
    describedCapture[2].body.input[0].content[0].text = JSON.stringify(describedGuardian);
    return describedCapture;
  };
  const descriptionPairs = [
    [
      "Run bounded permission regression test in sandbox",
      "Retry trusted pytest command outside workspace-write sandbox",
    ],
    [
      "Run the bounded permission regression test with default workspace-write sandbox.",
      "Retry the exact trusted pytest command outside the workspace-write sandbox as instructed by the host denial.",
    ],
  ];
  for (const descriptions of descriptionPairs) {
    assert.deepEqual(permissionTempEscalationLiveCaptureContract(
      withDescriptions(...descriptions),
      { model: "qwen/example" },
    ).failures, []);
  }
  const described = withDescriptions(...descriptionPairs[1]);

  const emptyDescription = structuredClone(described);
  const emptyArguments = JSON.parse(emptyDescription[1].body.input[1].arguments);
  emptyArguments.description = "";
  emptyDescription[1].body.input[1].arguments = JSON.stringify(emptyArguments);
  assert.deepEqual(permissionTempEscalationLiveCaptureContract(emptyDescription, {
    model: "qwen/example",
  }).failures, []);

  const nonStringDescription = structuredClone(described);
  const nonStringArguments = JSON.parse(nonStringDescription[1].body.input[1].arguments);
  nonStringArguments.description = null;
  nonStringDescription[1].body.input[1].arguments = JSON.stringify(nonStringArguments);
  assert.ok(permissionTempEscalationLiveCaptureContract(nonStringDescription, {
    model: "qwen/example",
  }).failures.includes("live-restricted-projection-mismatch"));

  const replayDescriptionDrift = structuredClone(described);
  const replayedRestrictedCall = structuredClone(replayDescriptionDrift[3].body.input[1]);
  const replayArguments = JSON.parse(replayedRestrictedCall.arguments);
  replayArguments.description = "A different replay-only display description.";
  replayedRestrictedCall.arguments = JSON.stringify(replayArguments);
  replayDescriptionDrift[3].body.input[1] = replayedRestrictedCall;
  assert.ok(permissionTempEscalationLiveCaptureContract(replayDescriptionDrift, {
    model: "qwen/example",
  }).failures.includes("live-elevated-continuation-mismatch"));

  const replayOutputDrift = structuredClone(described);
  const replayedRestrictedOutput = structuredClone(replayOutputDrift[3].body.input[2]);
  replayedRestrictedOutput.output = replayedRestrictedOutput.output.replace(
    "moyai-sandbox-effect-ABC",
    "moyai-sandbox-effect-XYZ",
  );
  replayOutputDrift[3].body.input[2] = replayedRestrictedOutput;
  assert.ok(permissionTempEscalationLiveCaptureContract(replayOutputDrift, {
    model: "qwen/example",
  }).failures.includes("live-elevated-continuation-mismatch"));

  const reusedCallId = structuredClone(described);
  const restrictedCallId = reusedCallId[3].body.input[1].call_id;
  reusedCallId[3].body.input[3].call_id = restrictedCallId;
  reusedCallId[3].body.input[4].call_id = restrictedCallId;
  const reusedCallIdGuardian = JSON.parse(reusedCallId[2].body.input[0].content[0].text);
  reusedCallIdGuardian.recent_committed_response.tool_request.call_id = restrictedCallId;
  reusedCallId[2].body.input[0].content[0].text = JSON.stringify(reusedCallIdGuardian);
  assert.ok(permissionTempEscalationLiveCaptureContract(reusedCallId, {
    model: "qwen/example",
  }).failures.includes("live-elevated-continuation-mismatch"));

  for (const forbiddenKey of ["timeout_ms", "workdir"]) {
    const extraArgument = structuredClone(described);
    const firstArguments = JSON.parse(extraArgument[1].body.input[1].arguments);
    firstArguments[forbiddenKey] = forbiddenKey === "timeout_ms" ? 30_000 : "C:/elsewhere";
    extraArgument[1].body.input[1].arguments = JSON.stringify(firstArguments);
    assert.ok(permissionTempEscalationLiveCaptureContract(extraArgument, {
      model: "qwen/example",
    }).failures.includes("live-restricted-projection-mismatch"));
  }

  const clientThinking = structuredClone(exact);
  clientThinking[0].body.reasoning = { effort: "low" };
  assert.ok(permissionTempEscalationLiveCaptureContract(clientThinking, {
    model: "qwen/example",
  }).failures.includes("live-request-common-contract-mismatch"));

  const workaround = structuredClone(exact);
  const firstCall = workaround[1].body.input[1];
  firstCall.arguments = JSON.stringify({
    command: `${PERMISSION_TEMP_ESCALATION_COMMAND} --basetemp .pytest_tmp`,
    sandbox_permissions: "use_default",
  });
  assert.ok(permissionTempEscalationLiveCaptureContract(workaround, {
    model: "qwen/example",
  }).failures.includes("live-restricted-projection-mismatch"));

  const guardianDrift = structuredClone(exact);
  const payload = JSON.parse(guardianDrift[2].body.input[0].content[0].text);
  const context = JSON.parse(payload.task_context);
  context.canonical_user_authority[0].text = "different authority";
  payload.task_context = JSON.stringify(context);
  guardianDrift[2].body.input[0].content[0].text = JSON.stringify(payload);
  assert.ok(permissionTempEscalationLiveCaptureContract(guardianDrift, {
    model: "qwen/example",
  }).failures.includes("live-guardian-request-mismatch"));
});

test("live LM Studio terminal uses the same settled two-shell workspace owner", () => {
  const liveProjection = baseProjection({
    transcript_rows: [
      { row_kind: "user", body: PERMISSION_TEMP_ESCALATION_LIVE_PROMPT },
      {
        row_kind: "work_summary_completed",
        body: [
          "### 作業サマリ",
          "- コマンド/ツール: 4件",
          "### 作業履歴",
          "- [待機] shell",
          "- [完了] shell",
          "- [待機] shell",
          "- [完了] shell",
        ].join("\n"),
      },
      { row_kind: "assistant", body: PERMISSION_TEMP_ESCALATION_LIVE_RESPONSE },
    ],
  });
  const renderedPrompt = PERMISSION_TEMP_ESCALATION_LIVE_PROMPT.replace(/\n/gu, " ");
  const exact = surface(liveProjection, {
    users: [{ visible: true, text: renderedPrompt }],
    assistants: [{ visible: true, text: PERMISSION_TEMP_ESCALATION_LIVE_RESPONSE }],
  });
  assert.deepEqual(permissionTempEscalationLiveTerminalFailures(exact), []);

  const domContentDrift = structuredClone(exact);
  domContentDrift.users[0].text = renderedPrompt.replace("shell only.", "tools only.");
  assert.ok(permissionTempEscalationLiveTerminalFailures(domContentDrift).includes(
    "live-terminal-dom-mismatch",
  ));

  const rawAuthorityWhitespaceDrift = structuredClone(exact);
  rawAuthorityWhitespaceDrift.projection.transcript_rows[0].body = renderedPrompt;
  assert.ok(permissionTempEscalationLiveTerminalFailures(rawAuthorityWhitespaceDrift).includes(
    "live-terminal-user-authority-mismatch",
  ));

  const domLag = structuredClone(exact);
  domLag.assistants = [];
  domLag.completed_summaries = [];
  const decision = createPermissionTempEscalationLiveTerminalDecision();
  assert.equal(decision(domLag), "pending");
  assert.equal(decision(exact), "pass");

  const extraTool = structuredClone(exact);
  extraTool.projection.transcript_rows[1].body += "\n- [完了] read";
  assert.ok(permissionTempEscalationLiveTerminalFailures(extraTool).includes(
    "live-terminal-tool-history-mismatch",
  ));
  assert.ok(permissionTempEscalationLiveTerminalFailureEvidence(extraTool)
    .terminal_oracle_failures.includes("live-terminal-tool-history-mismatch"));
});

test("permission TEMP escalation ledger and held oracle require the exact projected failure", () => {
  const held = heldSample();
  assert.equal(exactPermissionTempEscalationLedger(
    held.ledger,
    ["temp_initial", "temp_escalation"],
    { heldRole: "temp_escalation" },
  ), true);
  assert.deepEqual(permissionTempEscalationHeldFailures(held), []);
  assert.equal(permissionTempEscalationHeldDecision(held), "pass");
  for (let elapsedMs = 50; elapsedMs <= 30_000; elapsedMs += 50) {
    assert.equal(
      permissionTempEscalationHeldDecision(structuredClone(held)),
      "pass",
      `unchanged Provider-in-flight GUI summary must not delay release at ${elapsedMs}ms`,
    );
  }

  const projectionLag = structuredClone(held);
  projectionLag.surface.projection.transcript_rows = [{
    row_kind: "user",
    body: PERMISSION_TEMP_ESCALATION_PROMPT,
  }];
  assert.ok(permissionTempEscalationHeldFailures(projectionLag).includes(
    "held-work-summary-not-exact",
  ));
  assert.equal(permissionTempEscalationHeldDecision(projectionLag), "pending");

  const beforeProvider = structuredClone(projectionLag);
  beforeProvider.ledger = beforeProvider.ledger.slice(0, 1);
  assert.equal(permissionTempEscalationHeldDecision(beforeProvider), "pending");

  const rejected = structuredClone(held);
  rejected.ledger[2].contract.pass = false;
  rejected.ledger[2].response_phase = "rejected";
  rejected.ledger[2].response_status = 422;
  assert.equal(permissionTempEscalationHeldDecision(rejected), "fail");

  const missingHint = structuredClone(held);
  missingHint.ledger[2].contract.role_evidence.restricted_output.hint_kind = false;
  assert.ok(permissionTempEscalationHeldFailures(missingHint).includes("restricted-host-hint-not-exact"));

  const prematureAnswer = structuredClone(held);
  prematureAnswer.surface.projection.transcript_rows.push({ row_kind: "assistant", body: "early" });
  prematureAnswer.surface.assistants.push({ visible: true, text: "early" });
  assert.ok(permissionTempEscalationHeldFailures(prematureAnswer).includes(
    "held-primary-error-or-answer-present",
  ));

  const reordered = structuredClone(held);
  [reordered.ledger[1], reordered.ledger[2]] = [reordered.ledger[2], reordered.ledger[1]];
  assert.equal(exactPermissionTempEscalationLedger(
    reordered.ledger,
    ["temp_initial", "temp_escalation"],
    { heldRole: "temp_escalation" },
  ), false);
});

test("permission TEMP SQLite oracle requires nested failure and elevated success projections", () => {
  const owner = { sessionId: SESSION_ID, turnId: TURN_ID };
  const exact = {
    schema_version: "desktop-e2e.permission-temp-persistence.v1",
    read_only: true,
    rows: [
      {
        history_item_id: "01ARZ3NDEKTSV4RRFFQ69G5FAX",
        session_id: SESSION_ID,
        turn_id: TURN_ID,
        sequence_no: 5,
        kind: "tool_output",
        call_id: "01ARZ3NDEKTSV4RRFFQ69G5FAY",
        status: "completed",
        success: 0,
        metadata_success: 0,
        tool_metadata_success: 0,
        sandbox_failure_hint: "workspace_write_effect_temp_access_denied",
      },
      {
        history_item_id: "01ARZ3NDEKTSV4RRFFQ69G5FAZ",
        session_id: SESSION_ID,
        turn_id: TURN_ID,
        sequence_no: 8,
        kind: "tool_output",
        call_id: "01ARZ3NDEKTSV4RRFFQ69G5FB0",
        status: "completed",
        success: 1,
        metadata_success: 1,
        tool_metadata_success: 1,
        sandbox_failure_hint: null,
      },
    ],
  };
  assert.deepEqual(permissionTempEscalationPersistenceFailures(exact, owner), []);

  const legacyOuterSuccess = structuredClone(exact);
  legacyOuterSuccess.rows[0].success = 1;
  assert.ok(permissionTempEscalationPersistenceFailures(legacyOuterSuccess, owner).includes(
    "persistence-restricted-success-projection-mismatch",
  ));

  const nestedFailureOnSuccess = structuredClone(exact);
  nestedFailureOnSuccess.rows[1].tool_metadata_success = 0;
  assert.ok(permissionTempEscalationPersistenceFailures(nestedFailureOnSuccess, owner).includes(
    "persistence-elevated-success-projection-mismatch",
  ));

  const incomplete = structuredClone(exact);
  incomplete.rows[0].status = "running";
  assert.ok(permissionTempEscalationPersistenceFailures(incomplete, owner).includes(
    "persistence-tool-output-0-identity-mismatch",
  ));

  const missingElevated = structuredClone(exact);
  missingElevated.rows.pop();
  assert.deepEqual(permissionTempEscalationPersistenceFailures(missingElevated, owner), [
    "persistence-tool-output-cardinality-mismatch",
  ]);
});

test("permission TEMP capture JSON treats invalid UTF-8 or JSON as product evidence", () => {
  assert.deepEqual(parsePermissionTempEscalationCaptureJson(
    Buffer.from('{"schema_version":2}', "utf8"),
    { file: "capture.metadata.json", kind: "metadata" },
  ), { schema_version: 2 });

  for (const [bytes, file, kind] of [
    [Buffer.from("{", "utf8"), "capture.metadata.json", "metadata"],
    [Buffer.from([0xff]), "capture.request.json", "request"],
  ]) {
    assert.throws(
      () => parsePermissionTempEscalationCaptureJson(bytes, { file, kind }),
      (error) => error?.owner === "product"
        && error?.code === "permission-temp-live-request-capture-json"
        && error?.evidence?.file === file
        && error?.evidence?.kind === kind,
    );
  }
});

test("permission TEMP final capture observation preserves product and harness owners", () => {
  let productError;
  try {
    parsePermissionTempEscalationCaptureJson(Buffer.from("{"), {
      file: "capture.metadata.json",
      kind: "metadata",
    });
  } catch (error) {
    productError = error;
  }
  assert.deepEqual(permissionTempEscalationCaptureFailureObservation(productError), {
    name: "DesktopE2eError",
    code: "permission-temp-live-request-capture-json",
    message: "one prepared request capture metadata file is not valid UTF-8 JSON",
    evidence: productError.evidence,
  });

  const harnessError = new DesktopE2eError(
    "harness",
    "permission-temp-live-request-capture-read",
    "capture bytes could not be read",
  );
  assert.throws(
    () => permissionTempEscalationCaptureFailureObservation(harnessError),
    (error) => error === harnessError,
  );
  const unexpected = new Error("unexpected parser failure");
  assert.throws(
    () => permissionTempEscalationCaptureFailureObservation(unexpected),
    (error) => error === unexpected,
  );
});

test("permission TEMP SQLite timeout waits for close and has a bounded termination deadline", async () => {
  const fakeChild = (kill) => {
    const child = new EventEmitter();
    child.stdout = new PassThrough();
    child.stderr = new PassThrough();
    child.kill = kill;
    return child;
  };

  let observeKill;
  const killed = new Promise((resolve) => { observeKill = resolve; });
  let closingChild;
  const closingQuery = runReadOnlySqlite("fixture.sqlite3", "SELECT 1", 1, {
    spawnProcess: () => {
      closingChild = fakeChild(() => {
        observeKill();
        return true;
      });
      return closingChild;
    },
    terminationTimeoutMs: 1_000,
  });
  await killed;
  let settledBeforeClose = false;
  closingQuery.then(
    () => { settledBeforeClose = true; },
    () => { settledBeforeClose = true; },
  );
  await Promise.resolve();
  assert.equal(settledBeforeClose, false);
  closingChild.emit("close", null);
  await assert.rejects(closingQuery, /SQLite query timed out/u);

  const neverClosingQuery = runReadOnlySqlite("fixture.sqlite3", "SELECT 1", 1, {
    spawnProcess: () => fakeChild(() => true),
    terminationTimeoutMs: 20,
  });
  await assert.rejects(neverClosingQuery, /did not close after termination/u);
});

test("permission TEMP persistence reader queries the exact turn through sqlite3 read-only mode", async (context) => {
  const temporaryRoot = await mkdtemp(path.join(
    os.tmpdir(),
    "moyai-e2e-permission-temp-persistence-",
  ));
  context.after(async () => rm(temporaryRoot, { recursive: true, force: true }));
  const database = path.join(temporaryRoot, "moyai.sqlite3");
  const sqlLiteral = (value) => `'${value.replaceAll("'", "''")}'`;
  const restrictedPayload = JSON.stringify({
    kind: "tool_output",
    call_id: "01ARZ3NDEKTSV4RRFFQ69G5FAY",
    status: "completed",
    metadata: {
      success: false,
      tool_metadata: {
        success: false,
        sandbox_failure_hint: "workspace_write_effect_temp_access_denied",
      },
    },
    success: false,
  });
  const elevatedPayload = JSON.stringify({
    kind: "tool_output",
    call_id: "01ARZ3NDEKTSV4RRFFQ69G5FB0",
    status: "completed",
    metadata: { success: true, tool_metadata: { success: true } },
    success: true,
  });
  const setupSql = [
    "CREATE TABLE protocol_history_items (id TEXT, session_id TEXT, turn_id TEXT, sequence_no INTEGER, payload_json TEXT);",
    `INSERT INTO protocol_history_items VALUES (${sqlLiteral("01ARZ3NDEKTSV4RRFFQ69G5FAX")}, ${sqlLiteral(SESSION_ID)}, ${sqlLiteral(TURN_ID)}, 5, ${sqlLiteral(restrictedPayload)});`,
    `INSERT INTO protocol_history_items VALUES (${sqlLiteral("01ARZ3NDEKTSV4RRFFQ69G5FAZ")}, ${sqlLiteral(SESSION_ID)}, ${sqlLiteral(TURN_ID)}, 8, ${sqlLiteral(elevatedPayload)});`,
  ].join("\n");
  await execFileAsync("sqlite3.exe", ["-batch", "-bail", database, setupSql], {
    windowsHide: true,
  });

  const owner = { sessionId: SESSION_ID, turnId: TURN_ID };
  const evidence = await readPermissionTempEscalationPersistence({ database, owner });
  assert.equal(evidence.read_only, true);
  assert.deepEqual(permissionTempEscalationPersistenceFailures(evidence, owner), []);
  assert.deepEqual(evidence.rows.map((row) => [
    row.status,
    row.success,
    row.metadata_success,
    row.tool_metadata_success,
  ]), [
    ["completed", 0, 0, 0],
    ["completed", 1, 1, 1],
  ]);
});

test("permission TEMP escalation terminal requires exact retry and two completed shell lifecycles", () => {
  const exact = { surface: surface(), ledger: ledger() };
  assert.deepEqual(permissionTempEscalationTerminalFailures(exact), []);

  const shortGuardianDeadline = structuredClone(exact);
  shortGuardianDeadline.ledger[3].response_delay.delay_elapsed_ms = 90_000;
  shortGuardianDeadline.ledger[3].response_delay.headers_sent_elapsed_ms = 90_000;
  assert.ok(permissionTempEscalationTerminalFailures(shortGuardianDeadline).includes(
    "provider-role-ledger-mismatch",
  ));

  const successDrift = structuredClone(exact);
  successDrift.ledger.at(-1).contract.role_evidence.elevated_output.pytest_passed = false;
  assert.ok(permissionTempEscalationTerminalFailures(successDrift).includes(
    "elevated-pytest-success-not-exact",
  ));

  const oneTool = structuredClone(exact);
  oneTool.surface.projection.transcript_rows[1].body = oneTool.surface.projection.transcript_rows[1].body
    .replace("2件", "1件")
    .replace("\n- [完了] shell", "");
  assert.ok(permissionTempEscalationTerminalFailures(oneTool).includes("terminal-tool-history-mismatch"));

  const visibleError = structuredClone(exact);
  visibleError.surface.visible_recoverable_error_count = 1;
  assert.ok(permissionTempEscalationTerminalFailures(visibleError).includes(
    "terminal-surface-not-settled",
  ));
});

test("permission TEMP terminal waits for DOM convergence and confirms canonical mismatch", () => {
  const exact = { surface: surface(), ledger: ledger() };
  const domLag = structuredClone(exact);
  domLag.surface.assistants = [];
  domLag.surface.completed_summaries = [];
  assert.deepEqual(permissionTempEscalationTerminalFailures(domLag), [
    "terminal-dom-conversation-mismatch",
  ]);
  let observedAt = 1_000;
  const domDecision = createPermissionTempEscalationTerminalDecision({
    now: () => observedAt,
  });
  assert.equal(domDecision(domLag), "pending");
  observedAt += PERMISSION_TEMP_ESCALATION_TERMINAL_CONFIRMATION_MS * 2;
  assert.equal(domDecision(domLag), "pending");
  assert.equal(domDecision(exact), "pass");

  const canonicalMismatch = structuredClone(exact);
  canonicalMismatch.surface.projection.transcript_rows[1].body = canonicalMismatch
    .surface.projection.transcript_rows[1].body.replace(
      "- コマンド/ツール: 4件",
      "- コマンド/ツール: 3件",
    );
  assert.ok(permissionTempEscalationTerminalFailureEvidence(canonicalMismatch)
    .terminal_oracle_failures.includes("terminal-tool-history-mismatch"));
  observedAt = 2_000;
  const canonicalDecision = createPermissionTempEscalationTerminalDecision({
    now: () => observedAt,
  });
  assert.equal(canonicalDecision(canonicalMismatch), "pending");
  observedAt += PERMISSION_TEMP_ESCALATION_TERMINAL_CONFIRMATION_MS - 1;
  assert.equal(canonicalDecision(canonicalMismatch), "pending");
  observedAt += 1;
  assert.equal(canonicalDecision(canonicalMismatch), "fail");
});

test("permission TEMP escalation workspace oracle rejects every fixture or top-level drift", () => {
  const baseline = {
    schema_version: "desktop-e2e.workspace-snapshot.v1",
    entries: [
      {
        name: "E2E_PERMISSION_TEMP_ESCALATION.txt",
        kind: "file",
        size_bytes: 8,
        sha256: "a".repeat(64),
      },
      {
        name: "test_e2e_owner_only_temp.py",
        kind: "file",
        size_bytes: 72,
        sha256: "b".repeat(64),
      },
    ],
  };
  assert.deepEqual(permissionTempEscalationWorkspaceFailures(baseline, baseline), []);
  assert.deepEqual(permissionTempEscalationWorkspaceFailures({
    ...baseline,
    entries: [...baseline.entries, {
      name: ".pytest_tmp",
      kind: "directory",
      size_bytes: null,
      sha256: null,
    }],
  }, baseline), [
    "workspace-top-level-drift",
    "workspace-temp-workaround-present",
    "workspace-fixture-mutated",
  ]);
  const modified = structuredClone(baseline);
  modified.entries[1].sha256 = "c".repeat(64);
  assert.deepEqual(permissionTempEscalationWorkspaceFailures(modified, baseline), [
    "workspace-fixture-mutated",
  ]);
});

test("permission TEMP live final and cleanup gates reject late workspace or request drift", () => {
  const capture = {
    schema_version: "desktop-e2e.permission-temp-live-captures.v1",
    capture_count: 4,
    request_body_hashes: ["a", "b", "c", "d"],
  };
  const exact = {
    workspace: { entries: [] },
    workspace_failures: [],
    terminal_workspace_matches: true,
    prepared_request_capture: capture,
    prepared_request_capture_failure: null,
    prepared_request_capture_matches: true,
  };
  assert.deepEqual(permissionTempEscalationLiveFinalFailures(exact), []);
  assert.deepEqual(permissionTempEscalationLiveCleanupFailures({
    quiesceInput: "pass",
    quiesceFinalObservation: { ...exact, failures: [] },
    cleanupFinalObservation: { ...exact, failures: [] },
  }), []);

  assert.deepEqual(permissionTempEscalationLiveFinalFailures({
    ...exact,
    workspace_failures: ["workspace-fixture-mutated"],
    terminal_workspace_matches: false,
    prepared_request_capture_failure: { code: "invalid-json" },
    prepared_request_capture_matches: false,
  }), [
    "live-final-workspace-drift",
    "live-final-terminal-workspace-mismatch",
    "live-final-request-capture-read-failed",
    "live-final-request-capture-drift",
  ]);

  const lateCapture = structuredClone({ ...exact, failures: [] });
  lateCapture.prepared_request_capture.capture_count = 5;
  assert.deepEqual(permissionTempEscalationLiveCleanupFailures({
    quiesceInput: "pass",
    quiesceFinalObservation: { ...exact, failures: [] },
    cleanupFinalObservation: lateCapture,
  }), ["live-cleanup-final-observation-drift"]);
  assert.deepEqual(permissionTempEscalationLiveCleanupFailures({
    quiesceInput: "fail",
    quiesceFinalObservation: null,
    cleanupFinalObservation: null,
  }), [
    "live-cleanup-quiesce-failed",
    "live-cleanup-final-observation-drift",
  ]);
});

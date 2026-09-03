import assert from "node:assert/strict";
import test from "node:test";

import {
  PERMISSION_TEMP_ESCALATION_COMMAND,
  PERMISSION_GUARDIAN_SECRET_CANARY,
  PERMISSION_TEMP_ESCALATION_JUSTIFICATION,
  PERMISSION_TEMP_ESCALATION_LIVE_PROMPT,
  createPermissionGuardianOpenAiCompatibleScenario,
  normalizePermissionGuardianOpenAiCompatibleOptions,
  permissionGuardianOpenAiCompatibleCaptureMetadataFailures,
  permissionGuardianOpenAiCompatibleFixtureConfig,
  permissionGuardianOpenAiCompatibleLiveCaptureContract,
  permissionGuardianOpenAiCompatibleLiveTerminalFailures,
} from "../scenarios/permission_temp_escalation.mjs";

const MODEL = "example/oMLX-guardian";
const OPTIONS = Object.freeze({
  providerBaseUrl: "http://127.0.0.1:8119/v1",
  model: MODEL,
});
const SESSION_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const TURN_ID = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

function taskBody(messages) {
  return {
    model: MODEL,
    messages,
    tools: [{
      type: "function",
      function: {
        name: "shell",
        description: "Run one bounded shell command",
        parameters: { type: "object" },
      },
    }],
    parallel_tool_calls: false,
    n: 1,
    stream: true,
    stream_options: { include_usage: true },
  };
}

function textMessage(role, content) {
  return { role, content };
}

function toolCall(id, args) {
  return {
    role: "assistant",
    tool_calls: [{
      id,
      type: "function",
      function: { name: "shell", arguments: JSON.stringify(args) },
    }],
  };
}

function toolOutput(id, content) {
  return { role: "tool", tool_call_id: id, content };
}

function liveChatCaptures() {
  const system = textMessage("system", "bounded live permission regression");
  const user = textMessage("user", PERMISSION_TEMP_ESCALATION_LIVE_PROMPT);
  const restrictedId = "call-live-chat-restricted";
  const elevatedId = "call-live-chat-elevated";
  const restricted = toolCall(restrictedId, {
    command: PERMISSION_TEMP_ESCALATION_COMMAND,
    sandbox_permissions: "use_default",
  });
  const elevated = toolCall(elevatedId, {
    command: PERMISSION_TEMP_ESCALATION_COMMAND,
    sandbox_permissions: "require_escalated",
    justification: PERMISSION_TEMP_ESCALATION_JUSTIFICATION,
  });
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
      response_id: "response-live-chat-elevated",
      assistant_text: "",
      tool_request: {
        call_id: elevatedId,
        tool_name: "shell",
        arguments_json: elevated.tool_calls[0].function.arguments,
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
    { body: taskBody([system, user]) },
    { body: taskBody([
      system,
      user,
      restricted,
      toolOutput(restrictedId, restrictedOutput),
    ]) },
    { body: {
      model: MODEL,
      messages: [
        textMessage("system", "You are moyAI's independent permission guardian."),
        textMessage("user", JSON.stringify(guardianPayload)),
      ],
      n: 1,
      stream: true,
      stream_options: { include_usage: true },
    } },
    { body: taskBody([
      system,
      user,
      restricted,
      toolOutput(restrictedId, restrictedOutput),
      elevated,
      toolOutput(elevatedId, elevatedOutput),
    ]) },
  ];
}

test("live OpenAI-compatible Guardian fixture is credential-free Chat AutoReview", () => {
  const options = normalizePermissionGuardianOpenAiCompatibleOptions({
    provider_base_url: "http://127.0.0.1:8119/v1/",
    model: ` ${MODEL} `,
  });
  assert.deepEqual(options, OPTIONS);
  const config = permissionGuardianOpenAiCompatibleFixtureConfig(options);
  assert.match(config, /provider_profile = "openai_compatible"/u);
  assert.doesNotMatch(config, /provider_(?:api|metadata)_mode/u);
  assert.doesNotMatch(config, /api_key_env/u);
  assert.match(config, /access_mode = "auto_review"/u);
  assert.match(config, /max_retries = 0/u);
  assert.doesNotMatch(
    config,
    /max_(?:output_)?tokens|reasoning_(?:effort|summary)|supports_reasoning|temperature|top_p|top_k|presence_penalty|frequency_penalty|seed\s*=|stop(?:_sequences)?\s*=|\[model\.extra_body_json\]/u,
  );
  const scenario = createPermissionGuardianOpenAiCompatibleScenario({
    provider_base_url: OPTIONS.providerBaseUrl,
    model: MODEL,
  });
  assert.equal(scenario.id, "manual.permission-guardian-openai-compatible");
  assert.deepEqual(Object.keys(scenario.environment), [
    "MOYAI_E2E_PERMISSION_GUARDIAN_SECRET",
  ]);
  assert.deepEqual(Object.values(scenario.environment), [PERMISSION_GUARDIAN_SECRET_CANARY]);
  assert.throws(
    () => normalizePermissionGuardianOpenAiCompatibleOptions({
      provider_base_url: "http://user:secret@127.0.0.1:8119/v1",
      model: MODEL,
    }),
    /credential-free HTTP\(S\) endpoint/u,
  );
  assert.throws(
    () => normalizePermissionGuardianOpenAiCompatibleOptions({
      provider_base_url: OPTIONS.providerBaseUrl,
      model: MODEL,
      api_key: "secret",
    }),
    /unknown manual\.permission-guardian-openai-compatible option/u,
  );
});

test("live OpenAI-compatible capture proves one minimal tool-less Guardian and continuation", () => {
  const exact = liveChatCaptures();
  const accepted = permissionGuardianOpenAiCompatibleLiveCaptureContract(exact, OPTIONS);
  assert.deepEqual(accepted.failures, []);
  assert.deepEqual(accepted.roles, [
    "live_chat_initial",
    "live_chat_escalation",
    "live_chat_guardian",
    "live_chat_continuation",
  ]);
  assert.equal(accepted.guardian_request_count, 1);
  assert.equal(accepted.guardian_tool_surface_absent, true);
  assert.equal(accepted.secret_body_fields_absent, true);
  assert.equal(accepted.secret_canary_absent, true);

  for (const key of ["tools", "tool_choice", "parallel_tool_calls", "temperature", "reasoning"]) {
    const leaked = structuredClone(exact);
    leaked[2].body[key] = key === "tools" ? []
      : key === "parallel_tool_calls" ? false
        : key === "temperature" ? 0
          : key === "reasoning" ? { effort: "low" }
            : "none";
    assert.ok(permissionGuardianOpenAiCompatibleLiveCaptureContract(
      leaked,
      OPTIONS,
    ).failures.includes("live-chat-guardian-request-mismatch"));
  }

  const secret = structuredClone(exact);
  secret[2].body.authorization = "Bearer must-not-be-captured";
  const secretResult = permissionGuardianOpenAiCompatibleLiveCaptureContract(secret, OPTIONS);
  assert.ok(secretResult.failures.includes("live-chat-request-common-contract-mismatch"));
  assert.equal(secretResult.secret_body_fields_absent, false);

  const canary = structuredClone(exact);
  canary[2].body.messages[1].content += PERMISSION_GUARDIAN_SECRET_CANARY;
  const canaryResult = permissionGuardianOpenAiCompatibleLiveCaptureContract(canary, OPTIONS);
  assert.ok(canaryResult.failures.includes("live-chat-request-common-contract-mismatch"));
  assert.equal(canaryResult.secret_canary_absent, false);

  const responseFallback = structuredClone(exact);
  responseFallback[2].body = {
    model: MODEL,
    instructions: "You are moyAI's independent permission guardian.",
    input: [],
    store: false,
    stream: true,
  };
  assert.ok(permissionGuardianOpenAiCompatibleLiveCaptureContract(
    responseFallback,
    OPTIONS,
  ).failures.includes("live-chat-guardian-request-mismatch"));

  const replayDrift = structuredClone(exact);
  replayDrift[3].body.messages[2].tool_calls[0].id = "different-restricted-call";
  assert.ok(permissionGuardianOpenAiCompatibleLiveCaptureContract(
    replayDrift,
    OPTIONS,
  ).failures.includes("live-chat-elevated-continuation-mismatch"));
});

test("live OpenAI-compatible capture metadata rejects Responses and route aliases", () => {
  const exact = {
    schema_version: 2,
    transport: "http",
    capture_stage: "prepared",
    api_mode: "chat_completions",
    endpoint_path: "v1/chat/completions",
    captured_at_unix_ms: 1_750_000_000_000,
    process_id: 1234,
    sequence: 7,
    request_id: "request-live-chat-7",
    request_body_bytes: 1024,
    request_body_file: "0007.request.json",
  };
  assert.deepEqual(permissionGuardianOpenAiCompatibleCaptureMetadataFailures(exact), []);

  for (const [key, value] of [
    ["api_mode", "responses"],
    ["endpoint_path", "v1/responses"],
    ["capture_stage", "received"],
  ]) {
    assert.ok(permissionGuardianOpenAiCompatibleCaptureMetadataFailures({
      ...exact,
      [key]: value,
    }).includes("live-request-metadata-route-mismatch"));
  }
  assert.ok(permissionGuardianOpenAiCompatibleCaptureMetadataFailures({
    ...exact,
    authorization: "Bearer secret",
  }).includes("live-request-metadata-shape-mismatch"));
});

test("live OpenAI-compatible terminal binds the effective credential-free provider tuple", () => {
  const surface = {
    projection: {
      provider_effective_profile: "openai_compatible",
      provider_effective_base_url: OPTIONS.providerBaseUrl,
      provider_effective_model_id: MODEL,
      provider_effective_api_key_env: "",
    },
  };
  assert.equal(permissionGuardianOpenAiCompatibleLiveTerminalFailures(
    surface,
    OPTIONS,
  ).includes("live-chat-provider-target-mismatch"), false);
  assert.equal(permissionGuardianOpenAiCompatibleLiveTerminalFailures(
    surface,
    OPTIONS,
  ).includes("live-chat-secret-boundary-mismatch"), false);
  assert.equal(permissionGuardianOpenAiCompatibleLiveTerminalFailures(
    surface,
    OPTIONS,
  ).includes("live-chat-secret-canary-exposed"), false);

  const secretName = structuredClone(surface);
  secretName.projection.provider_effective_api_key_env = "OPENAI_API_KEY";
  assert.ok(permissionGuardianOpenAiCompatibleLiveTerminalFailures(
    secretName,
    OPTIONS,
  ).includes("live-chat-secret-boundary-mismatch"));

  const exposed = structuredClone(surface);
  exposed.assistants = [{ text: PERMISSION_GUARDIAN_SECRET_CANARY }];
  assert.ok(permissionGuardianOpenAiCompatibleLiveTerminalFailures(
    exposed,
    OPTIONS,
  ).includes("live-chat-secret-canary-exposed"));
});

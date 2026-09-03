import assert from "node:assert/strict";
import crypto from "node:crypto";
import test from "node:test";

import {
  SCRIPTED_PROVIDER_MODEL_ID,
  SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_MAX_RESPONSES,
  createPermissionRestartGuardianProviderScript,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";

const SEED_PROMPT = "establish one durable Chat restart authority turn";
const SEED_RESPONSE = "CHAT_RESTART_AUTHORITY_SEEDED";
const TASK_PROMPT = "run the bounded Chat elevated no-op and report completion";
const COMMAND = "Write-Output E2E_GUARDIAN_CHAT_OK";
const JUSTIFICATION = "exercise the exact Chat automatic permission review path";
const FINAL_RESPONSE = "RESTART_GUARDIAN_CHAT_COMPLETE";
const INSTRUCTIONS = "Deterministic Chat permission restart fixture instructions.";
const GUARDIAN_INSTRUCTIONS = "You are moyAI's independent permission guardian.";
const SHELL_OUTPUT = [
  `Command: ${COMMAND}`,
  "",
  "Exit code: 0",
  "",
  "Stdout:",
  "E2E_GUARDIAN_CHAT_OK",
  "",
  "Stderr:",
  "(empty)",
].join("\n");

function sha256(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

function textMessage(role, content) {
  return { role, content };
}

function shellTools() {
  return [{
    type: "function",
    function: {
      name: "shell",
      description: "Run a PowerShell command.",
      parameters: {
        type: "object",
        required: ["command"],
        properties: {
          command: { type: "string" },
          workdir: { type: "string" },
          timeout_ms: { type: "integer" },
          description: { type: "string" },
          sandbox_permissions: {
            type: "string",
            enum: ["use_default", "require_escalated"],
            description: "Select the reviewed process admission.",
          },
          justification: { type: "string" },
        },
      },
    },
  }];
}

function taskRequest(messages) {
  return {
    model: SCRIPTED_PROVIDER_MODEL_ID,
    stream: true,
    stream_options: { include_usage: true },
    n: 1,
    messages,
    tools: shellTools(),
    parallel_tool_calls: false,
  };
}

function guardianPayload(call) {
  return {
    trusted_world_state: { schema_version: "fixture-world-state.v1" },
    task_context: JSON.stringify({
      authority_session_id: "01M00000000000000000000000",
      canonical_user_authority: [
        {
          kind: "user_turn",
          history_item_id: "01M00000000000000000000001",
          text: SEED_PROMPT,
        },
        {
          kind: "user_turn",
          history_item_id: "01M00000000000000000000002",
          text: TASK_PROMPT,
        },
      ],
    }),
    recent_committed_response: {
      response_id: "01M00000000000000000000003",
      assistant_text: "",
      tool_request: {
        call_id: call.id,
        tool_name: call.function.name,
        arguments_json: call.function.arguments,
      },
      prior_committed_tool_results: [],
    },
    permission_request: {
      access: "shell",
      summary: "Run the bounded fixture command",
      details: [`Requested sandbox elevation: ${JUSTIFICATION}`],
      targets: ["C:/fixture/workspace"],
      outside_workspace: true,
      risks: [],
    },
    action_evidence: { kind: "permission_request" },
  };
}

function guardianRequest(call, overrides = {}) {
  return {
    model: SCRIPTED_PROVIDER_MODEL_ID,
    stream: true,
    stream_options: { include_usage: true },
    n: 1,
    messages: [
      textMessage("system", GUARDIAN_INSTRUCTIONS),
      textMessage("user", JSON.stringify(guardianPayload(call))),
    ],
    ...overrides,
  };
}

function script() {
  return createPermissionRestartGuardianProviderScript({
    apiMode: "chat_completions",
    seedPrompt: SEED_PROMPT,
    seedResponseText: SEED_RESPONSE,
    taskPrompt: TASK_PROMPT,
    command: COMMAND,
    justification: JUSTIFICATION,
    responseText: FINAL_RESPONSE,
  });
}

function parseSse(text) {
  return text
    .split("\n\n")
    .filter(Boolean)
    .map((block) => {
      assert.match(block, /^data: /);
      return JSON.parse(block.slice("data: ".length));
    });
}

async function post(provider, body, path = "/v1/chat/completions") {
  return fetch(`${provider.baseUrl}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

async function waitFor(predicate, timeoutMs = 2_000) {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() >= deadline) throw new Error("timed out waiting for scripted provider state");
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

async function startChatGuardianProvider() {
  return startScriptedProvider({
    responseBehavior: "hold_until_release",
    script: script(),
  });
}

async function seed(provider) {
  const response = await post(provider, taskRequest([
    textMessage("system", INSTRUCTIONS),
    textMessage("user", SEED_PROMPT),
  ]));
  assert.equal(response.status, 200);
  const events = parseSse(await response.text());
  assert.equal(events[0].choices[0].delta.content, SEED_RESPONSE);
  assert.equal(events[0].choices[0].finish_reason, "stop");
  assert.deepEqual(events[1].choices, []);
}

async function heldShellCall(provider) {
  const responsePromise = post(provider, taskRequest([
    textMessage("system", INSTRUCTIONS),
    textMessage("user", SEED_PROMPT),
    textMessage("assistant", SEED_RESPONSE),
    textMessage("user", TASK_PROMPT),
  ]));
  await waitFor(() => provider.requestLedger.some((row) => (
    row.contract?.role === "guardian_tool_initial" && row.response_phase === "held"
  )));
  const release = provider.releaseScriptRole("guardian_tool_initial");
  assert.equal(release.request.route, "chat_completions");
  const response = await responsePromise;
  assert.equal(response.status, 200);
  const events = parseSse(await response.text());
  const streamed = events[0].choices[0].delta.tool_calls[0];
  assert.equal(events[0].choices[0].finish_reason, "tool_calls");
  return {
    id: streamed.id,
    type: streamed.type,
    function: streamed.function,
  };
}

function continuationRequest(call, { assistantContent = undefined } = {}) {
  const assistant = { role: "assistant", tool_calls: [call] };
  if (assistantContent !== undefined) assistant.content = assistantContent;
  return taskRequest([
    textMessage("system", INSTRUCTIONS),
    textMessage("user", SEED_PROMPT),
    textMessage("assistant", SEED_RESPONSE),
    textMessage("user", TASK_PROMPT),
    assistant,
    { role: "tool", tool_call_id: call.id, content: SHELL_OUTPUT },
  ]);
}

test("OpenAI-compatible Chat Guardian script completes the exact four-role tool-less review", async (context) => {
  const provider = await startChatGuardianProvider();
  context.after(() => provider.close());

  await seed(provider);
  const call = await heldShellCall(provider);
  assert.equal(call.id, "call_permission_restart_guardian_shell");
  assert.equal(call.type, "function");
  assert.equal(call.function.name, "shell");
  assert.deepEqual(JSON.parse(call.function.arguments), {
    command: COMMAND,
    sandbox_permissions: "require_escalated",
    justification: JUSTIFICATION,
  });

  const guardian = await post(provider, guardianRequest(call));
  assert.equal(guardian.status, 200);
  const guardianEvents = parseSse(await guardian.text());
  assert.equal(guardianEvents[0].choices[0].delta.content, JSON.stringify({
    decision: "allow",
    rationale: "bounded deterministic fixture command",
  }));
  assert.equal(guardianEvents[0].choices[0].finish_reason, "stop");

  const continuation = await post(provider, continuationRequest(call));
  assert.equal(continuation.status, 200);
  const continuationEvents = parseSse(await continuation.text());
  assert.equal(continuationEvents[0].choices[0].delta.content, FINAL_RESPONSE);
  assert.equal(continuationEvents[0].choices[0].finish_reason, "stop");

  const rows = provider.requestLedger.filter((row) => row.route === "chat_completions");
  assert.equal(rows.length, SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_MAX_RESPONSES);
  assert.deepEqual(rows.map((row) => [
    row.contract.role,
    row.contract.pass,
    row.response_phase,
    row.response_status,
  ]), [
    ["guardian_seed", true, "completed", 200],
    ["guardian_tool_initial", true, "completed", 200],
    ["guardian_review", true, "completed", 200],
    ["guardian_continuation", true, "completed", 200],
  ]);
  assert.equal(rows.every((row) => row.pathname === "/v1/chat/completions"), true);
  assert.equal(rows.every((row) => row.contract.client_generation_fields_absent), true);
  const guardianContract = rows[2].contract;
  assert.deepEqual(guardianContract.top_level_keys, [
    "messages",
    "model",
    "n",
    "stream",
    "stream_options",
  ]);
  assert.deepEqual(guardianContract.role_evidence.message_roles, ["system", "user"]);
  assert.equal(guardianContract.role_evidence.guardian_instructions_match, true);
  assert.equal(guardianContract.role_evidence.payload.pass, true);
  assert.deepEqual(guardianContract.sampling_fields_present, []);
  assert.equal(guardianContract.sampling_absent, true);
  assert.deepEqual(guardianContract.reasoning_fields_present, []);
  assert.equal(guardianContract.reasoning_absent, true);
  assert.equal(guardianContract.tools_absent, true);
  assert.equal(rows[3].contract.role_evidence.assistant_content_absent, true);
  assert.equal(rows[3].contract.role_evidence.shell_call_matches, true);
  assert.equal(rows[3].contract.role_evidence.tool_output_matches, true);
  assert.equal(rows[3].contract.role_evidence.tool_output_sha256, sha256(SHELL_OUTPUT));
  assert.deepEqual(provider.resourceObservation().scripted_response_roles, [
    "guardian_seed",
    "guardian_tool_initial",
    "guardian_review",
    "guardian_continuation",
  ]);
});

test("OpenAI-compatible Chat Guardian script rejects sampling, reasoning, tools, and wrong route", async (context) => {
  const mutations = [
    { temperature: 0 },
    { reasoning: { effort: "none" } },
    { tools: [] },
    { parallel_tool_calls: false },
  ];
  const providers = await Promise.all(mutations.map(() => startChatGuardianProvider()));
  const wrongRoute = await startChatGuardianProvider();
  context.after(async () => Promise.all([
    ...providers.map((provider) => provider.close()),
    wrongRoute.close(),
  ]));

  for (let index = 0; index < providers.length; index += 1) {
    await seed(providers[index]);
    const call = await heldShellCall(providers[index]);
    const response = await post(providers[index], guardianRequest(call, mutations[index]));
    assert.equal(response.status, 422);
    assert.deepEqual(await response.json(), { error: "request_contract_mismatch" });
    const contract = providers[index].requestLedger.at(-1).contract;
    assert.equal(contract.role, "guardian_review");
    assert.equal(contract.pass, false);
  }
  assert.equal(providers[0].requestLedger.at(-1).contract.sampling_absent, false);
  assert.equal(providers[1].requestLedger.at(-1).contract.reasoning_absent, false);
  assert.equal(providers[2].requestLedger.at(-1).contract.tools_absent, false);
  assert.equal(providers[3].requestLedger.at(-1).contract.tools_absent, false);

  const response = await post(wrongRoute, taskRequest([
    textMessage("system", INSTRUCTIONS),
    textMessage("user", SEED_PROMPT),
  ]), "/v1/responses");
  assert.equal(response.status, 404);
  assert.deepEqual(await response.json(), { error: "not_found" });
  assert.deepEqual(wrongRoute.resourceObservation().scripted_response_roles, []);
});

test("OpenAI-compatible Chat Guardian continuation rejects assistant content drift", async (context) => {
  const provider = await startChatGuardianProvider();
  context.after(() => provider.close());

  await seed(provider);
  const call = await heldShellCall(provider);
  assert.equal((await post(provider, guardianRequest(call))).status, 200);
  const continuation = await post(provider, continuationRequest(call, {
    assistantContent: "unexpected assistant text",
  }));
  assert.equal(continuation.status, 422);
  assert.deepEqual(await continuation.json(), { error: "request_contract_mismatch" });
  const contract = provider.requestLedger.at(-1).contract;
  assert.equal(contract.role, null);
  assert.equal(contract.role_evidence.assistant_content_absent, false);
  assert.equal(contract.pass, false);
});

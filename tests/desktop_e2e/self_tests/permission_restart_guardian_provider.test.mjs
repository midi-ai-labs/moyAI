import assert from "node:assert/strict";
import crypto from "node:crypto";
import test from "node:test";

import {
  SCRIPTED_PROVIDER_MAX_OUTPUT_TOKENS,
  SCRIPTED_PROVIDER_MODEL_ID,
  SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_KIND,
  SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_MAX_RESPONSES,
  createPermissionRestartGuardianProviderScript,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";

const SEED_PROMPT = "establish one durable restart authority turn";
const SEED_RESPONSE = "RESTART_AUTHORITY_SEEDED";
const TASK_PROMPT = "run the bounded elevated no-op and report completion";
const COMMAND = "Write-Output E2E_GUARDIAN_OK";
const JUSTIFICATION = "exercise the exact automatic permission review path";
const FINAL_RESPONSE = "RESTART_GUARDIAN_COMPLETE";
const INSTRUCTIONS = "Deterministic permission restart fixture instructions.";
const GUARDIAN_INSTRUCTIONS = "You are moyAI's independent permission guardian.";
const SHELL_OUTPUT = [
  `Command: ${COMMAND}`,
  "",
  "Exit code: 0",
  "",
  "Stdout:",
  "E2E_GUARDIAN_OK",
  "",
  "Stderr:",
  "(empty)",
].join("\n");

function sha256(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

function userMessage(text) {
  return {
    type: "message",
    role: "user",
    content: [{ type: "input_text", text }],
  };
}

function assistantMessage(text) {
  return {
    type: "message",
    role: "assistant",
    content: [{ type: "output_text", text }],
  };
}

function shellTools() {
  return [{
    type: "function",
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
  }];
}

function taskRequest(input) {
  return {
    model: SCRIPTED_PROVIDER_MODEL_ID,
    instructions: INSTRUCTIONS,
    input,
    tools: shellTools(),
    tool_choice: "auto",
    parallel_tool_calls: false,
    max_output_tokens: SCRIPTED_PROVIDER_MAX_OUTPUT_TOKENS,
    store: false,
    stream: true,
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
        call_id: call.call_id,
        tool_name: call.name,
        arguments_json: call.arguments,
      },
      prior_committed_tool_results: [],
    },
    permission_request: {
      access: "shell",
      summary: "Run the bounded fixture command",
      details: [
        `Requested sandbox elevation: ${JUSTIFICATION}`,
      ],
      targets: ["C:/fixture/workspace"],
      outside_workspace: true,
      risks: [],
    },
    action_evidence: { kind: "permission_request" },
  };
}

function guardianRequest(call, transform = (value) => value) {
  const payload = transform(guardianPayload(call));
  return {
    model: SCRIPTED_PROVIDER_MODEL_ID,
    instructions: GUARDIAN_INSTRUCTIONS,
    input: [userMessage(JSON.stringify(payload))],
    max_output_tokens: 512,
    store: false,
    stream: true,
    reasoning: { effort: "none" },
  };
}

function script() {
  return createPermissionRestartGuardianProviderScript({
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

async function post(provider, body) {
  return fetch(`${provider.baseUrl}/v1/responses`, {
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

async function seed(provider) {
  const response = await post(provider, taskRequest([userMessage(SEED_PROMPT)]));
  assert.equal(response.status, 200);
  const events = parseSse(await response.text());
  assert.equal(events[0].delta, SEED_RESPONSE);
}

async function heldShellCall(provider) {
  const responsePromise = post(provider, taskRequest([
    userMessage(SEED_PROMPT),
    assistantMessage(SEED_RESPONSE),
    userMessage(TASK_PROMPT),
  ]));
  await waitFor(() => provider.requestLedger.at(-1)?.response_phase === "held");
  const resource = provider.resourceObservation();
  assert.deepEqual(resource.script_role_release, {
    role: "guardian_tool_initial",
    released: false,
    released_by_cleanup: false,
  });
  const release = provider.releaseScriptRole("guardian_tool_initial");
  assert.equal(release.released, true);
  assert.equal(release.role, "guardian_tool_initial");
  const response = await responsePromise;
  assert.equal(response.status, 200);
  return parseSse(await response.text())[0].item;
}

test("permission restart Guardian script serves native metadata and exact four-role recovery", async (context) => {
  const provider = await startScriptedProvider({
    responseBehavior: "hold_until_release",
    script: script(),
  });
  context.after(() => provider.close());

  const models = await fetch(`${provider.baseUrl}/api/v1/models`);
  assert.equal(models.status, 200);
  const catalog = await models.json();
  assert.equal(catalog.models[0].key, SCRIPTED_PROVIDER_MODEL_ID);
  assert.equal(catalog.models[0].type, "llm");
  assert.equal(catalog.models[0].loaded_instances.length, 1);
  assert.equal(catalog.models[0].capabilities.trained_for_tool_use, true);

  await seed(provider);
  const call = await heldShellCall(provider);
  assert.equal(call.type, "function_call");
  assert.equal(call.name, "shell");
  assert.deepEqual(JSON.parse(call.arguments), {
    command: COMMAND,
    sandbox_permissions: "require_escalated",
    justification: JUSTIFICATION,
  });

  const guardian = await post(provider, guardianRequest(call));
  assert.equal(guardian.status, 200);
  const guardianEvents = parseSse(await guardian.text());
  assert.equal(guardianEvents[0].delta, JSON.stringify({
    decision: "allow",
    rationale: "bounded deterministic fixture command",
  }));
  assert.equal(guardianEvents.at(-1).response.usage.output_tokens_details.reasoning_tokens, 0);

  const continuation = await post(provider, taskRequest([
    userMessage(SEED_PROMPT),
    assistantMessage(SEED_RESPONSE),
    userMessage(TASK_PROMPT),
    {
      type: "function_call",
      call_id: call.call_id,
      name: call.name,
      arguments: call.arguments,
    },
    {
      type: "function_call_output",
      call_id: call.call_id,
      output: SHELL_OUTPUT,
    },
  ]));
  assert.equal(continuation.status, 200);
  const continuationEvents = parseSse(await continuation.text());
  assert.equal(continuationEvents[0].delta, FINAL_RESPONSE);
  assert.equal(continuationEvents[1].item.content[0].text, FINAL_RESPONSE);

  const responseRows = provider.requestLedger.filter((row) => row.route === "responses");
  assert.deepEqual(responseRows.map((row) => [
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
  assert.equal(responseRows[2].contract.role_evidence.payload.authority_matches, true);
  assert.equal(responseRows[2].contract.reasoning_none, true);
  assert.equal(responseRows[3].contract.role_evidence.tool_output_sha256, sha256(SHELL_OUTPUT));

  const resource = provider.resourceObservation();
  assert.equal(resource.script_kind, SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_KIND);
  assert.equal(
    resource.scripted_responses_maximum,
    SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_MAX_RESPONSES,
  );
  assert.equal(resource.scripted_responses_request_count, 4);
  assert.equal(resource.accepted_response_count, 4);
  assert.equal(resource.successful_response_count, 4);
  assert.deepEqual(resource.scripted_response_roles, [
    "guardian_seed",
    "guardian_tool_initial",
    "guardian_review",
    "guardian_continuation",
  ]);
  assert.deepEqual(resource.script_role_release, {
    role: "guardian_tool_initial",
    released: true,
    released_by_cleanup: false,
  });

  const serializedLedger = JSON.stringify(provider.requestLedger);
  for (const secret of [SEED_PROMPT, SEED_RESPONSE, TASK_PROMPT, COMMAND, JUSTIFICATION, SHELL_OUTPUT]) {
    assert.equal(serializedLedger.includes(secret), false, `ledger leaked ${secret}`);
  }
});

test("permission restart Guardian script fails closed on order, replay, and evidence drift", async (context) => {
  const premature = await startScriptedProvider({ script: script() });
  const replay = await startScriptedProvider({ script: script() });
  const drift = await startScriptedProvider({ script: script() });
  context.after(async () => Promise.all([premature.close(), replay.close(), drift.close()]));
  const call = {
    type: "function_call",
    call_id: "call_permission_restart_guardian_shell",
    name: "shell",
    arguments: JSON.stringify({
      command: COMMAND,
      sandbox_permissions: "require_escalated",
      justification: JUSTIFICATION,
    }),
  };

  const prematureReview = await post(premature, guardianRequest(call));
  assert.equal(prematureReview.status, 409);
  assert.deepEqual(await prematureReview.json(), { error: "script_role_prerequisite_missing" });
  assert.deepEqual(premature.resourceObservation().scripted_response_roles, []);

  await seed(replay);
  const duplicateSeed = await post(replay, taskRequest([userMessage(SEED_PROMPT)]));
  assert.equal(duplicateSeed.status, 409);
  assert.deepEqual(await duplicateSeed.json(), { error: "script_role_already_consumed" });
  assert.deepEqual(replay.resourceObservation().scripted_response_roles, ["guardian_seed"]);

  await seed(drift);
  const initial = await post(drift, taskRequest([
    userMessage(SEED_PROMPT),
    assistantMessage(SEED_RESPONSE),
    userMessage(TASK_PROMPT),
  ]));
  assert.equal(initial.status, 200);
  const exactCall = parseSse(await initial.text())[0].item;
  const drifted = await post(drift, guardianRequest(exactCall, (payload) => ({
    ...payload,
    task_context: JSON.stringify({
      ...JSON.parse(payload.task_context),
      canonical_user_authority: JSON.parse(payload.task_context).canonical_user_authority.slice(1),
    }),
  })));
  assert.equal(drifted.status, 422);
  assert.deepEqual(await drifted.json(), { error: "request_contract_mismatch" });
  assert.equal(drift.requestLedger.at(-1).contract.role, null);
  assert.equal(drift.requestLedger.at(-1).contract.role_evidence.payload.authority_matches, false);
  assert.deepEqual(drift.resourceObservation().scripted_response_roles, [
    "guardian_seed",
    "guardian_tool_initial",
  ]);
});

test("permission restart Guardian script validates its exact builder and release lifecycle", async () => {
  assert.throws(
    () => createPermissionRestartGuardianProviderScript({
      seedPrompt: "",
      seedResponseText: SEED_RESPONSE,
      taskPrompt: TASK_PROMPT,
      command: COMMAND,
      justification: JUSTIFICATION,
      responseText: FINAL_RESPONSE,
    }),
    /seedPrompt must be a non-empty string/,
  );
  await assert.rejects(
    startScriptedProvider({ script: { ...script(), extra: true } }),
    /exact schema/,
  );
  await assert.rejects(
    startScriptedProvider({
      script: script(),
      responseBehavior: "hold_until_peer_close",
    }),
    /owns its response lifecycle/,
  );
  const provider = await startScriptedProvider({ script: script() });
  assert.throws(
    () => provider.releaseScriptRole("guardian_tool_initial"),
    /not configured/,
  );
  assert.equal((await provider.close()).pass, true);
});

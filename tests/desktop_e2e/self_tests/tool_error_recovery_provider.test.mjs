import assert from "node:assert/strict";
import crypto from "node:crypto";
import test from "node:test";

import {
  SCRIPTED_PROVIDER_MODEL_ID,
  SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_KIND,
  SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_MAX_RESPONSES,
  createToolErrorRecoveryProviderScript,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";

const PROMPT = "recover after one exact missing read";
const MISSING_PATH = "__moyai_e2e_missing__/tool-error-recovery.txt";
const RESPONSE_TEXT = "RECOVERY_COMPLETED";
const STREAMED_PREFIX = "RECOVERY_";
const TOOL_OUTPUT = "read path not found: fixture sentinel";
const INSTRUCTIONS = "Deterministic tool error recovery fixture instructions.";

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

function tools() {
  return [
    {
      type: "function",
      name: "read",
      description: "Read one bounded text file.",
      parameters: {
        type: "object",
        required: ["path"],
        properties: {
          path: { type: "string" },
          offset: { type: "integer" },
          limit: { type: "integer" },
        },
      },
    },
    {
      type: "function",
      name: "apply_patch",
      description: "Apply one patch.",
      parameters: { type: "object", properties: {} },
    },
  ];
}

function request(input) {
  return {
    model: SCRIPTED_PROVIDER_MODEL_ID,
    instructions: INSTRUCTIONS,
    input,
    tools: tools(),
    tool_choice: "auto",
    parallel_tool_calls: false,
    store: false,
    stream: true,
  };
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

function script() {
  return createToolErrorRecoveryProviderScript({
    missingPath: MISSING_PATH,
    responseText: RESPONSE_TEXT,
    streamedPrefix: STREAMED_PREFIX,
  });
}

test("tool error recovery script emits one failing read then a prefix/full final Assistant", async (context) => {
  const provider = await startScriptedProvider({ expectedPrompt: PROMPT, script: script() });
  context.after(() => provider.close());

  const models = await fetch(`${provider.baseUrl}/v1/models`);
  assert.equal(models.status, 200);
  assert.equal((await models.json()).data[0].capabilities.tools, true);

  const initial = await post(provider, request([userMessage(PROMPT)]));
  assert.equal(initial.status, 200);
  const initialEvents = parseSse(await initial.text());
  assert.deepEqual(initialEvents.map((event) => event.type), [
    "response.output_item.done",
    "response.completed",
  ]);
  const call = initialEvents[0].item;
  assert.equal(call.type, "function_call");
  assert.equal(call.name, "read");
  assert.deepEqual(JSON.parse(call.arguments), { path: MISSING_PATH });
  assert.deepEqual(initialEvents[1].response.output, [call]);

  const continuation = await post(provider, request([
    userMessage(PROMPT),
    {
      type: "function_call",
      call_id: call.call_id,
      name: call.name,
      arguments: call.arguments,
    },
    {
      type: "function_call_output",
      call_id: call.call_id,
      output: TOOL_OUTPUT,
    },
  ]));
  assert.equal(continuation.status, 200);
  const finalEvents = parseSse(await continuation.text());
  assert.deepEqual(finalEvents.map((event) => event.type), [
    "response.output_text.delta",
    "response.output_item.done",
    "response.completed",
  ]);
  assert.equal(finalEvents[0].delta, STREAMED_PREFIX);
  assert.equal(RESPONSE_TEXT.startsWith(finalEvents[0].delta), true);
  assert.notEqual(finalEvents[0].delta, RESPONSE_TEXT);
  assert.equal(finalEvents[1].item.content[0].text, RESPONSE_TEXT);
  assert.equal(finalEvents[2].response.output[0].content[0].text, RESPONSE_TEXT);

  const resource = provider.resourceObservation();
  assert.equal(resource.script_kind, SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_KIND);
  assert.equal(resource.scripted_responses_maximum, SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_MAX_RESPONSES);
  assert.equal(resource.scripted_responses_request_count, 2);
  assert.equal(resource.accepted_response_count, 2);
  assert.equal(resource.successful_response_count, 2);
  assert.deepEqual(resource.scripted_response_roles, [
    "tool_error_initial",
    "tool_error_continuation",
  ]);

  const responses = provider.requestLedger.filter((row) => row.route === "responses");
  assert.deepEqual(responses.map((row) => [
    row.contract.role,
    row.contract.pass,
    row.response_phase,
    row.response_status,
  ]), [
    ["tool_error_initial", true, "completed", 200],
    ["tool_error_continuation", true, "completed", 200],
  ]);
  assert.equal(responses[1].contract.role_evidence.tool_output_non_empty, true);
  assert.equal(responses[1].contract.role_evidence.tool_output_sha256, sha256(TOOL_OUTPUT));
  const serializedLedger = JSON.stringify(provider.requestLedger);
  assert.equal(serializedLedger.includes(PROMPT), false);
  assert.equal(serializedLedger.includes(MISSING_PATH), false);
  assert.equal(serializedLedger.includes(TOOL_OUTPUT), false);

  const exhausted = await post(provider, request([userMessage(PROMPT)]));
  assert.equal(exhausted.status, 409);
  assert.deepEqual(await exhausted.json(), { error: "scripted_response_request_limit_exceeded" });
  assert.deepEqual(provider.resourceObservation().scripted_response_roles, [
    "tool_error_initial",
    "tool_error_continuation",
  ]);
});

test("tool error recovery script can release-hold only its initial tool-call response", async (context) => {
  const provider = await startScriptedProvider({
    expectedPrompt: PROMPT,
    responseBehavior: "hold_until_release",
    script: script(),
  });
  context.after(() => provider.close());

  const initialRequest = post(provider, request([userMessage(PROMPT)]));
  await waitFor(() => provider.requestLedger[0]?.response_phase === "held");
  assert.equal(provider.resourceObservation().successful_response_count, 0);
  assert.equal(provider.resourceObservation().response_release_controlled, true);
  assert.deepEqual(provider.releaseResponse(0), {
    released: true,
    turn_index: 0,
    request: provider.requestLedger[0],
  });

  const initial = await initialRequest;
  assert.equal(initial.status, 200);
  const call = parseSse(await initial.text())[0].item;
  const continuation = await post(provider, request([
    userMessage(PROMPT),
    {
      type: "function_call",
      call_id: call.call_id,
      name: call.name,
      arguments: call.arguments,
    },
    { type: "function_call_output", call_id: call.call_id, output: TOOL_OUTPUT },
  ]));
  assert.equal(continuation.status, 200);
  assert.equal(provider.resourceObservation().response_release_count, 1);
  assert.equal(provider.resourceObservation().successful_response_count, 2);
  assert.deepEqual(provider.requestLedger.map((row) => row.response_phase), ["completed", "completed"]);
});

test("tool error recovery script fails closed on order, replay, and continuation schema", async (context) => {
  const prematureProvider = await startScriptedProvider({ expectedPrompt: PROMPT, script: script() });
  const replayProvider = await startScriptedProvider({ expectedPrompt: PROMPT, script: script() });
  const schemaProvider = await startScriptedProvider({ expectedPrompt: PROMPT, script: script() });
  context.after(async () => {
    await Promise.all([prematureProvider.close(), replayProvider.close(), schemaProvider.close()]);
  });

  const exactCall = {
    type: "function_call",
    call_id: "call_tool_error_recovery_read",
    name: "read",
    arguments: JSON.stringify({ path: MISSING_PATH }),
  };
  const premature = await post(prematureProvider, request([
    userMessage(PROMPT),
    exactCall,
    { type: "function_call_output", call_id: exactCall.call_id, output: TOOL_OUTPUT },
  ]));
  assert.equal(premature.status, 409);
  assert.deepEqual(await premature.json(), { error: "script_role_prerequisite_missing" });
  assert.deepEqual(prematureProvider.resourceObservation().scripted_response_roles, []);

  const initialBody = request([userMessage(PROMPT)]);
  assert.equal((await post(replayProvider, initialBody)).status, 200);
  const replay = await post(replayProvider, initialBody);
  assert.equal(replay.status, 409);
  assert.deepEqual(await replay.json(), { error: "script_role_already_consumed" });
  assert.deepEqual(replayProvider.resourceObservation().scripted_response_roles, ["tool_error_initial"]);

  assert.equal((await post(schemaProvider, initialBody)).status, 200);
  const emptyOutput = await post(schemaProvider, request([
    userMessage(PROMPT),
    exactCall,
    { type: "function_call_output", call_id: exactCall.call_id, output: "   " },
  ]));
  assert.equal(emptyOutput.status, 422);
  assert.deepEqual(await emptyOutput.json(), { error: "request_contract_mismatch" });
  assert.equal(schemaProvider.requestLedger.at(-1).contract.role, null);
  assert.equal(schemaProvider.requestLedger.at(-1).contract.role_evidence.tool_output_non_empty, false);
  assert.deepEqual(schemaProvider.resourceObservation().scripted_response_roles, ["tool_error_initial"]);
});

test("tool error recovery script rejects unsafe paths, non-prefix streams, excess keys, and lifecycle conflicts", async () => {
  assert.throws(
    () => createToolErrorRecoveryProviderScript({
      missingPath: "../outside.txt",
      responseText: RESPONSE_TEXT,
      streamedPrefix: STREAMED_PREFIX,
    }),
    /normalized relative path without traversal/,
  );
  assert.throws(
    () => createToolErrorRecoveryProviderScript({
      missingPath: MISSING_PATH,
      responseText: RESPONSE_TEXT,
      streamedPrefix: RESPONSE_TEXT,
    }),
    /strict prefix/,
  );
  await assert.rejects(
    startScriptedProvider({ script: { ...script(), excess: true } }),
    /exact schema/,
  );
  await assert.rejects(
    startScriptedProvider({
      script: script(),
      responseBehavior: "hold_until_peer_close",
    }),
    /owns its response lifecycle/,
  );
  await assert.rejects(
    startScriptedProvider({
      script: createToolErrorRecoveryProviderScript({
        missingPath: MISSING_PATH,
        responseText: RESPONSE_TEXT,
        streamedPrefix: STREAMED_PREFIX,
      }),
      turns: [{ prompt: PROMPT, responseText: RESPONSE_TEXT }],
      responseBehavior: "hold_until_release",
    }),
    /cannot use ordinary turns/,
  );
});

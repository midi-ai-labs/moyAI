import assert from "node:assert/strict";
import test from "node:test";

import {
  SCRIPTED_PROVIDER_AGENT_INTERRUPT_KIND,
  SCRIPTED_PROVIDER_AGENT_INTERRUPT_MAX_RESPONSES,
  SCRIPTED_PROVIDER_AGENT_INTERRUPT_MESSAGE,
  SCRIPTED_PROVIDER_AGENT_INTERRUPT_ROOT_RESPONSE,
  SCRIPTED_PROVIDER_AGENT_INTERRUPT_TASK_NAME,
  SCRIPTED_PROVIDER_MODEL_ID,
  createAgentInterruptProviderScript,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";

const PROMPT = "delegate exact child interrupt";
const INSTRUCTIONS = "Deterministic multi-agent fixture instructions.";

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
      description: "Read one file.",
      parameters: { type: "object", properties: {} },
    },
    {
      type: "function",
      name: "spawn_agent",
      description: "Spawn one bounded child task.",
      parameters: {
        type: "object",
        required: ["task_name", "message"],
        additionalProperties: false,
        properties: {
          task_name: { type: "string", description: "Task name." },
          message: { type: "string", description: "Task message." },
          fork_turns: { type: "string", description: "Fork selection." },
        },
      },
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

async function post(provider, body, signal = undefined) {
  return fetch(`${provider.baseUrl}/v1/responses`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
    signal,
  });
}

async function waitFor(predicate, timeoutMs = 2_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  assert.fail(`condition did not settle within ${timeoutMs}ms`);
}

function childEnvelope() {
  return `Message Type: NEW_TASK\nTask name: /root/${SCRIPTED_PROVIDER_AGENT_INTERRUPT_TASK_NAME}\nSender: /root\nPayload:\n${SCRIPTED_PROVIDER_AGENT_INTERRUPT_MESSAGE}`;
}

test("agent interrupt script accepts exact root/tool/child roles independent of continuation race", async (context) => {
  const provider = await startScriptedProvider({
    expectedPrompt: PROMPT,
    script: createAgentInterruptProviderScript(),
  });
  context.after(() => provider.close());

  const models = await fetch(`${provider.baseUrl}/v1/models`);
  assert.equal(models.status, 200);
  assert.equal((await models.json()).data[0].capabilities.tools, true);

  const initial = await post(provider, request([userMessage(PROMPT)]));
  assert.equal(initial.status, 200);
  const spawnEvents = parseSse(await initial.text());
  assert.deepEqual(spawnEvents.map((event) => event.type), [
    "response.output_item.done",
    "response.completed",
  ]);
  const spawn = spawnEvents[0].item;
  assert.equal(spawn.type, "function_call");
  assert.equal(spawn.name, "spawn_agent");
  assert.deepEqual(JSON.parse(spawn.arguments), {
    task_name: SCRIPTED_PROVIDER_AGENT_INTERRUPT_TASK_NAME,
    message: SCRIPTED_PROVIDER_AGENT_INTERRUPT_MESSAGE,
    fork_turns: "none",
  });

  const childAbort = new AbortController();
  const heldChild = post(provider, request([userMessage(childEnvelope())]), childAbort.signal);
  await waitFor(() => provider.requestLedger.some((row) => (
    row.contract?.role === "child_held" && row.response_phase === "held"
  )));

  const continuationInput = [
    userMessage(PROMPT),
    {
      type: "function_call",
      call_id: spawn.call_id,
      name: spawn.name,
      arguments: spawn.arguments,
    },
    {
      type: "function_call_output",
      call_id: spawn.call_id,
      output: JSON.stringify({ task_name: `/root/${SCRIPTED_PROVIDER_AGENT_INTERRUPT_TASK_NAME}` }),
    },
  ];
  const continuation = await post(provider, request(continuationInput));
  assert.equal(continuation.status, 200);
  const finalEvents = parseSse(await continuation.text());
  assert.deepEqual(finalEvents.map((event) => event.type), [
    "response.output_text.delta",
    "response.output_item.done",
    "response.completed",
  ]);
  assert.equal(finalEvents[0].delta, SCRIPTED_PROVIDER_AGENT_INTERRUPT_ROOT_RESPONSE);

  const inFlight = provider.resourceObservation();
  assert.equal(inFlight.script_kind, SCRIPTED_PROVIDER_AGENT_INTERRUPT_KIND);
  assert.equal(inFlight.scripted_responses_maximum, SCRIPTED_PROVIDER_AGENT_INTERRUPT_MAX_RESPONSES);
  assert.equal(inFlight.scripted_responses_request_count, 3);
  assert.equal(inFlight.accepted_response_count, 3);
  assert.equal(inFlight.successful_response_count, 2);
  assert.equal(inFlight.active_request_count, 1);

  childAbort.abort();
  await assert.rejects(heldChild, (error) => error?.name === "AbortError");
  await waitFor(() => provider.requestLedger.some((row) => (
    row.contract?.role === "child_held" && row.response_phase === "peer_closed"
  )));

  const responses = provider.requestLedger.filter((row) => row.route === "responses");
  assert.equal(responses.length, 3);
  assert.deepEqual(new Set(responses.map((row) => row.contract.role)), new Set([
    "root_initial",
    "root_continuation",
    "child_held",
  ]));
  assert.equal(responses.every((row) => row.contract.pass), true);
  assert.equal(responses.find((row) => row.contract.role === "child_held").response_phase, "peer_closed");
  assert.equal(responses.filter((row) => row.response_status === 200).length, 2);
  assert.equal(responses.every((row) => row.contract.tools.spawn_agent_schema_matches), true);
  const serialized = JSON.stringify(provider.requestLedger);
  assert.equal(serialized.includes(PROMPT), false);
  assert.equal(serialized.includes(SCRIPTED_PROVIDER_AGENT_INTERRUPT_MESSAGE), false);

  const closed = await provider.close();
  assert.equal(closed.pass, true);
  assert.equal(closed.forced_connection_count, 0);
  assert.equal(closed.after.active_request_count, 0);
  assert.equal(closed.after.accepted_response_count, 3);
  assert.equal(closed.after.successful_response_count, 2);
});

test("agent interrupt script fails closed on role order, input drift, and the fourth Responses request", async (context) => {
  const provider = await startScriptedProvider({
    expectedPrompt: PROMPT,
    script: createAgentInterruptProviderScript(),
  });
  context.after(() => provider.close());

  const prematureChild = await post(provider, request([userMessage(childEnvelope())]));
  assert.equal(prematureChild.status, 409);
  assert.deepEqual(await prematureChild.json(), { error: "script_role_prerequisite_missing" });

  const drifted = await post(provider, request([userMessage(`${PROMPT} drift`)]));
  assert.equal(drifted.status, 422);
  assert.deepEqual(await drifted.json(), { error: "request_contract_mismatch" });

  const initial = await post(provider, request([userMessage(PROMPT)]));
  assert.equal(initial.status, 200);
  const spawn = parseSse(await initial.text())[0].item;
  const fourth = await post(provider, request([
    userMessage(PROMPT),
    { type: "function_call", call_id: spawn.call_id, name: spawn.name, arguments: spawn.arguments },
    {
      type: "function_call_output",
      call_id: spawn.call_id,
      output: JSON.stringify({ task_name: `/root/${SCRIPTED_PROVIDER_AGENT_INTERRUPT_TASK_NAME}` }),
    },
  ]));
  assert.equal(fourth.status, 409);
  assert.deepEqual(await fourth.json(), { error: "scripted_response_request_limit_exceeded" });

  const ledger = provider.requestLedger;
  assert.equal(ledger.length, 4);
  assert.deepEqual(ledger.map((row) => row.contract?.role), [
    "child_held",
    null,
    "root_initial",
    "root_continuation",
  ]);
  assert.deepEqual(ledger.map((row) => row.response_status), [409, 422, 200, 409]);
  assert.equal(provider.resourceObservation().accepted_response_count, 1);
  assert.equal(provider.resourceObservation().scripted_responses_request_count, 4);
});

test("agent interrupt script configuration rejects excess keys and conflicting lifecycle ownership", async () => {
  assert.throws(
    () => createAgentInterruptProviderScript({ childTaskName: "Bad-Task" }),
    /lowercase letters, digits, and underscores/,
  );
  await assert.rejects(
    startScriptedProvider({
      script: {
        ...createAgentInterruptProviderScript(),
        excess: true,
      },
    }),
    /exact schema/,
  );
  await assert.rejects(
    startScriptedProvider({
      script: createAgentInterruptProviderScript(),
      responseBehavior: "hold_until_peer_close",
    }),
    /owns its response lifecycle/,
  );
});

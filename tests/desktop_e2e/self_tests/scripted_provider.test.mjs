import assert from "node:assert/strict";
import crypto from "node:crypto";
import test from "node:test";

import {
  SCRIPTED_PROVIDER_MODEL_ID,
  SCRIPTED_PROVIDER_MAX_TURNS,
  SCRIPTED_PROVIDER_CLIENT_GENERATION_KEYS,
  SCRIPTED_PROVIDER_PROMPT,
  SCRIPTED_PROVIDER_RESPONSE,
  scriptedProviderPortIsFetchSafe,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";

test("scripted provider keeps long deterministic history fixtures explicitly bounded", async () => {
  const turn = (index) => ({ prompt: `prompt-${index}`, responseText: `response-${index}` });
  const maximum = Array.from({ length: SCRIPTED_PROVIDER_MAX_TURNS }, (_, index) => turn(index + 1));
  const provider = await startScriptedProvider({ turns: maximum });
  assert.equal(provider.resourceObservation().scripted_responses_maximum, SCRIPTED_PROVIDER_MAX_TURNS);
  assert.equal((await provider.close()).pass, true);
  await assert.rejects(
    startScriptedProvider({ turns: [...maximum, turn(SCRIPTED_PROVIDER_MAX_TURNS + 1)] }),
    new RegExp(`1 through ${SCRIPTED_PROVIDER_MAX_TURNS}`),
  );
});

test("scripted provider validates one exact ordered same-session conversation", async (context) => {
  const turns = [
    { prompt: "first prompt", responseText: "FIRST_RESPONSE" },
    { prompt: "second prompt", responseText: "SECOND_RESPONSE" },
  ];
  const provider = await startScriptedProvider({ turns, orderedConversation: true });
  context.after(() => provider.close());

  const first = await fetch(`${provider.baseUrl}/v1/responses`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(responsesRequest(turns[0].prompt)),
  });
  assert.equal(first.status, 200);
  await first.text();

  const secondBody = responsesRequest(turns[1].prompt);
  secondBody.input = [
    { type: "message", role: "user", content: [{ type: "input_text", text: turns[0].prompt }] },
    { type: "message", role: "assistant", content: [{ type: "output_text", text: turns[0].responseText }] },
    { type: "message", role: "user", content: [{ type: "input_text", text: turns[1].prompt }] },
  ];
  const second = await fetch(`${provider.baseUrl}/v1/responses`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(secondBody),
  });
  assert.equal(second.status, 200);
  const events = parseSse(await second.text());
  assert.equal(events[0].delta, turns[1].responseText);
  assert.deepEqual(provider.requestLedger.map((row) => ({
    status: row.response_status,
    pass: row.contract?.pass,
    conversation: row.contract?.ordered_conversation,
  })), [
    {
      status: 200,
      pass: true,
      conversation: {
        input_count: 1,
        expected_input_count: 1,
        roles: ["user"],
        text_sha256: [sha256(turns[0].prompt)],
        matches: true,
      },
    },
    {
      status: 200,
      pass: true,
      conversation: {
        input_count: 3,
        expected_input_count: 3,
        roles: ["user", "assistant", "user"],
        text_sha256: [
          sha256(turns[0].prompt),
          sha256(turns[0].responseText),
          sha256(turns[1].prompt),
        ],
        matches: true,
      },
    },
  ]);

  const exhausted = await fetch(`${provider.baseUrl}/v1/responses`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(secondBody),
  });
  assert.equal(exhausted.status, 409);
  assert.deepEqual(await exhausted.json(), { error: "successful_response_already_consumed" });
  assert.equal(provider.requestLedger.at(-1)?.response_phase, "rejected");
  assert.equal(provider.requestLedger.at(-1)?.response_status, 409);
  assert.equal(provider.requestLedger.at(-1)?.contract, null);
});

function sha256(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

function responsesRequest(prompt = SCRIPTED_PROVIDER_PROMPT) {
  return {
    model: SCRIPTED_PROVIDER_MODEL_ID,
    instructions: "Deterministic fixture instructions.",
    input: [{
      type: "message",
      role: "user",
      content: [{ type: "input_text", text: prompt }],
    }],
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

async function waitFor(predicate, timeoutMs = 2_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  assert.fail(`condition did not settle within ${timeoutMs}ms`);
}

test("scripted provider never publishes a Fetch-forbidden loopback port", async (context) => {
  for (const port of [1, 21, 2_049, 5_060, 6_000, 6_665, 6_669, 10_080]) {
    assert.equal(scriptedProviderPortIsFetchSafe(port), false, `port ${port}`);
  }
  for (const port of [80, 1_024, 10_081, 49_152, 65_535]) {
    assert.equal(scriptedProviderPortIsFetchSafe(port), true, `port ${port}`);
  }

  const provider = await startScriptedProvider();
  context.after(() => provider.close());
  assert.equal(scriptedProviderPortIsFetchSafe(provider.resourceObservation().address.port), true);
  const response = await fetch(`${provider.baseUrl}/v1/models`);
  assert.equal(response.status, 200);
});

test("scripted provider holds one exact Docling readiness GET until explicit release", async (context) => {
  const provider = await startScriptedProvider({ doclingReadinessStatus: 204 });
  context.after(() => provider.close());
  assert.deepEqual(provider.requestLedger, []);

  const responsePromise = fetch(`${provider.baseUrl}/ready`);
  await waitFor(() => {
    const [row] = provider.requestLedger;
    const resource = provider.resourceObservation();
    return row?.method === "GET"
      && row.pathname === "/ready"
      && row.route === "docling_readiness"
      && row.contract?.pass === true
      && row.response_phase === "held"
      && row.response_status === null
      && resource.active_request_count === 1
      && resource.docling_readiness_request_count === 1
      && resource.docling_readiness_released === false;
  });

  const release = provider.releaseDoclingReadiness();
  assert.equal(release.released, true);
  assert.equal(release.response_status, 204);
  const response = await responsePromise;
  assert.equal(response.status, 204);
  await waitFor(() => {
    const [row] = provider.requestLedger;
    return row?.response_phase === "completed"
      && row.response_status === 204
      && provider.resourceObservation().active_request_count === 0;
  });
  assert.deepEqual(provider.requestLedger.map((row) => [
    row.method,
    row.pathname,
    row.query_present,
    row.response_phase,
    row.response_status,
  ]), [["GET", "/ready", false, "completed", 204]]);
  assert.throws(() => provider.releaseDoclingReadiness(), /already released/);

  const close = await provider.close();
  assert.equal(close.pass, true);
  assert.equal(close.after.docling_readiness_released, true);
  assert.equal(close.after.docling_readiness_released_by_cleanup, false);
});

test("scripted provider serves one exact Responses turn without retaining request secrets", async (context) => {
  const provider = await startScriptedProvider();
  context.after(() => provider.close());
  const started = provider.resourceObservation();
  assert.equal(started.address.host, "127.0.0.1");
  assert.equal(started.address.family, "IPv4");
  assert.ok(Number.isInteger(started.address.port) && started.address.port > 0);
  assert.equal(started.listening, true);

  const models = await fetch(`${provider.baseUrl}/v1/models`);
  assert.equal(models.status, 200);
  assert.match(models.headers.get("content-type"), /^application\/json/);
  assert.deepEqual(await models.json(), {
    object: "list",
    data: [{
      id: SCRIPTED_PROVIDER_MODEL_ID,
      object: "model",
      owned_by: "moyai-desktop-e2e",
      context_window: 65_536,
      max_output_tokens: 1_024,
      max_parallel_predictions: 1,
      capabilities: { tools: false, reasoning: false, vision: false },
    }],
  });

  const wire = JSON.stringify(responsesRequest());
  const response = await fetch(`${provider.baseUrl}/v1/responses`, {
    method: "POST",
    headers: {
      authorization: "Bearer must-never-enter-the-ledger",
      "content-type": "application/json",
    },
    body: wire,
  });
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-type"), "text/event-stream");
  const events = parseSse(await response.text());
  assert.deepEqual(events.map((event) => event.type), [
    "response.output_text.delta",
    "response.output_item.done",
    "response.completed",
  ]);
  assert.equal(events[0].delta, SCRIPTED_PROVIDER_RESPONSE);
  assert.equal(events[1].item.content[0].text, SCRIPTED_PROVIDER_RESPONSE);
  assert.equal(events[2].response.output[0].content[0].text, SCRIPTED_PROVIDER_RESPONSE);
  assert.deepEqual(events[2].response.usage, {
    input_tokens: 4,
    output_tokens: 2,
    total_tokens: 6,
    output_tokens_details: { reasoning_tokens: 0 },
  });

  const duplicate = await fetch(`${provider.baseUrl}/v1/responses`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: wire,
  });
  assert.equal(duplicate.status, 409);
  assert.deepEqual(await duplicate.json(), { error: "successful_response_already_consumed" });

  const ledger = provider.requestLedger;
  assert.equal(ledger.length, 3);
  assert.deepEqual(ledger.map((entry) => [entry.method, entry.pathname, entry.response_status]), [
    ["GET", "/v1/models", 200],
    ["POST", "/v1/responses", 200],
    ["POST", "/v1/responses", 409],
  ]);
  assert.equal(ledger[1].body.size_bytes, Buffer.byteLength(wire));
  assert.equal(ledger[1].body.sha256, sha256(Buffer.from(wire)));
  assert.equal(ledger[1].body.limit_exceeded, false);
  assert.equal(ledger[1].request_headers.authorization, "<redacted>");
  assert.equal(ledger[1].contract.pass, true);
  assert.equal(ledger[1].contract.model_sha256, sha256(Buffer.from(SCRIPTED_PROVIDER_MODEL_ID)));
  assert.equal(ledger[1].contract.input_text_sha256, sha256(Buffer.from(SCRIPTED_PROVIDER_PROMPT)));
  assert.equal(ledger[1].contract.instructions_sha256, sha256(Buffer.from("Deterministic fixture instructions.")));
  assert.deepEqual(ledger[1].contract.top_level_keys, ["input", "instructions", "model", "store", "stream"]);
  assert.deepEqual(ledger[1].contract.forbidden_fields_present, []);
  const serializedLedger = JSON.stringify(ledger);
  assert.doesNotMatch(serializedLedger, /must-never-enter-the-ledger/);
  assert.equal(serializedLedger.includes(SCRIPTED_PROVIDER_PROMPT), false);
  assert.equal(serializedLedger.includes(wire), false);

  const close = await provider.close();
  assert.equal(close.pass, true);
  assert.equal(close.before.listening, true);
  assert.equal(close.after.listening, false);
  assert.equal(close.after.closed, true);
  assert.equal(close.after.active_request_count, 0);
  assert.equal(close.after.open_connection_count, 0);
  assert.equal(close.after.successful_response_count, 1);
  assert.deepEqual(await provider.close(), close, "close is an idempotent observation of the same resource settlement");
});

test("scripted provider serves a bounded ordered multi-root turn script", async (context) => {
  const turns = [
    { prompt: "create root alpha", responseText: "ALPHA_OK" },
    { prompt: "create root beta", responseText: "BETA_OK" },
  ];
  const provider = await startScriptedProvider({ turns });
  context.after(() => provider.close());
  assert.equal(provider.resourceObservation().script_kind, "ordered_turns");
  assert.equal(provider.resourceObservation().scripted_responses_maximum, 2);

  for (const turn of turns) {
    const response = await fetch(`${provider.baseUrl}/v1/responses`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(responsesRequest(turn.prompt)),
    });
    assert.equal(response.status, 200);
    const events = parseSse(await response.text());
    assert.equal(events[0].delta, turn.responseText);
  }
  assert.deepEqual(provider.requestLedger.map((row) => [
    row.sequence,
    row.contract?.pass,
    row.response_phase,
    row.response_status,
  ]), [
    [1, true, "completed", 200],
    [2, true, "completed", 200],
  ]);
  assert.equal(provider.resourceObservation().successful_response_count, 2);
});

test("scripted provider rejects omitted, drifted, and forbidden fixed-config request fields", async (context) => {
  const provider = await startScriptedProvider();
  context.after(() => provider.close());
  const generationBodies = SCRIPTED_PROVIDER_CLIENT_GENERATION_KEYS.map((key) => ({
    ...responsesRequest(),
    [key]: key === "reasoning"
      ? { effort: "low" }
      : key === "stop" || key === "stop_sequences"
        ? ["STOP"]
        : key === "chat_template_kwargs" || key === "extra_body" || key === "extra_body_json"
          ? { enable_thinking: false }
          : true,
  }));
  const invalidBodies = [
    (() => { const body = responsesRequest(); delete body.instructions; return body; })(),
    ...generationBodies,
    { ...responsesRequest(), tools: [] },
    { ...responsesRequest(), previous_response_id: "must-not-be-sent" },
  ];
  for (const body of invalidBodies) {
    const response = await fetch(`${provider.baseUrl}/v1/responses`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    assert.equal(response.status, 422);
    assert.deepEqual(await response.json(), { error: "request_contract_mismatch" });
  }
  const ledger = provider.requestLedger;
  assert.equal(ledger.length, invalidBodies.length);
  assert.equal(ledger.every((row) => row.contract?.pass === false && row.response_status === 422), true);
  for (let index = 0; index < SCRIPTED_PROVIDER_CLIENT_GENERATION_KEYS.length; index += 1) {
    const key = SCRIPTED_PROVIDER_CLIENT_GENERATION_KEYS[index];
    const contract = ledger[index + 1].contract;
    assert.deepEqual(contract.client_generation_fields_present, [key], key);
    assert.equal(contract.client_generation_fields_absent, false, key);
    assert.equal(contract.forbidden_fields_present.includes(key), true, key);
  }
  const structuralOffset = 1 + SCRIPTED_PROVIDER_CLIENT_GENERATION_KEYS.length;
  assert.deepEqual(ledger[structuralOffset].contract.forbidden_fields_present, ["tools"]);
  assert.deepEqual(ledger[structuralOffset + 1].contract.forbidden_fields_present, ["previous_response_id"]);
});

test("scripted provider rejects non-exact routes, methods, and oversized bodies", async (context) => {
  const provider = await startScriptedProvider({ maxBodyBytes: 256 });
  context.after(() => provider.close());

  const wrongModelsMethod = await fetch(`${provider.baseUrl}/v1/models`, { method: "POST" });
  assert.equal(wrongModelsMethod.status, 405);
  assert.equal(wrongModelsMethod.headers.get("allow"), "GET");

  const wrongResponsesMethod = await fetch(`${provider.baseUrl}/v1/responses`);
  assert.equal(wrongResponsesMethod.status, 405);
  assert.equal(wrongResponsesMethod.headers.get("allow"), "POST");

  const queryTarget = await fetch(`${provider.baseUrl}/v1/models?probe=redacted-query-value`);
  assert.equal(queryTarget.status, 404);

  const oversizedWire = JSON.stringify({ padding: "x".repeat(300) });
  assert.ok(Buffer.byteLength(oversizedWire) > 256);
  const oversized = await fetch(`${provider.baseUrl}/v1/responses`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: oversizedWire,
  });
  assert.equal(oversized.status, 413);
  assert.deepEqual(await oversized.json(), { error: "request_body_too_large" });

  const ledger = provider.requestLedger;
  assert.deepEqual(ledger.map((entry) => [entry.method, entry.pathname, entry.query_present, entry.response_status]), [
    ["POST", "/v1/models", false, 405],
    ["GET", "/v1/responses", false, 405],
    ["GET", "/v1/models", true, 404],
    ["POST", "/v1/responses", false, 413],
  ]);
  assert.equal(ledger[3].body.limit_exceeded, true);
  assert.equal(ledger[3].body.size_bytes, Buffer.byteLength(oversizedWire));
  assert.equal(ledger[3].body.sha256, sha256(Buffer.from(oversizedWire)));
  assert.equal(JSON.stringify(ledger).includes("redacted-query-value"), false);
  assert.equal(JSON.stringify(ledger).includes(oversizedWire), false);

  const close = await provider.close();
  assert.equal(close.pass, true);
  assert.equal(close.after.successful_response_count, 0);
  assert.equal(close.after.request_count, 4);
});

test("scripted provider can hold one valid Responses request in flight until the exact peer closes", async (context) => {
  const provider = await startScriptedProvider({ responseBehavior: "hold_until_peer_close" });
  context.after(() => provider.close());
  const controller = new AbortController();
  const request = fetch(`${provider.baseUrl}/v1/responses`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(responsesRequest()),
    signal: controller.signal,
  });

  await waitFor(() => {
    const [row] = provider.requestLedger;
    const resource = provider.resourceObservation();
    return row?.contract?.pass === true
      && row.response_phase === "held"
      && row.response_status === null
      && resource.active_request_count === 1
      && resource.accepted_response_count === 1
      && resource.successful_response_count === 0;
  });
  assert.deepEqual(provider.requestLedger.map((row) => [
    row.method,
    row.pathname,
    row.response_phase,
    row.response_status,
  ]), [["POST", "/v1/responses", "held", null]]);

  controller.abort();
  await assert.rejects(request, (error) => error?.name === "AbortError");
  await waitFor(() => {
    const [row] = provider.requestLedger;
    return row?.response_phase === "peer_closed"
      && provider.resourceObservation().active_request_count === 0;
  });
  assert.equal(provider.requestLedger.length, 1);
  assert.equal(provider.requestLedger[0].response_status, null);

  const close = await provider.close();
  assert.equal(close.pass, true);
  assert.equal(close.forced_connection_count, 0);
  assert.equal(close.after.accepted_response_count, 1);
  assert.equal(close.after.successful_response_count, 0);
});

test("scripted provider releases ordered Responses turns only through their exact index", async (context) => {
  const turns = [
    { prompt: "first prompt", responseText: "FIRST_RESPONSE" },
    { prompt: "second prompt", responseText: "SECOND_RESPONSE" },
  ];
  const provider = await startScriptedProvider({
    turns,
    orderedConversation: true,
    responseBehavior: "hold_until_release",
  });
  context.after(() => provider.close());

  const firstRequest = fetch(`${provider.baseUrl}/v1/responses`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(responsesRequest(turns[0].prompt)),
  });
  await waitFor(() => provider.requestLedger[0]?.response_phase === "held");
  assert.equal(provider.resourceObservation().successful_response_count, 0);
  assert.deepEqual(provider.releaseResponse(0), {
    released: true,
    turn_index: 0,
    request: provider.requestLedger[0],
  });
  assert.equal((await firstRequest).status, 200);
  await waitFor(() => provider.requestLedger[0]?.response_phase === "completed");
  assert.throws(() => provider.releaseResponse(0), /already released/);

  const secondBody = responsesRequest(turns[1].prompt);
  secondBody.input = [
    { type: "message", role: "user", content: [{ type: "input_text", text: turns[0].prompt }] },
    { type: "message", role: "assistant", content: [{ type: "output_text", text: turns[0].responseText }] },
    { type: "message", role: "user", content: [{ type: "input_text", text: turns[1].prompt }] },
  ];
  const secondRequest = fetch(`${provider.baseUrl}/v1/responses`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(secondBody),
  });
  await waitFor(() => provider.requestLedger[1]?.response_phase === "held");
  assert.equal(provider.resourceObservation().response_release_count, 1);
  provider.releaseResponse(1);
  assert.equal((await secondRequest).status, 200);
  await waitFor(() => provider.requestLedger[1]?.response_phase === "completed");
  assert.deepEqual(provider.requestLedger.map((row) => [row.response_phase, row.response_status]), [
    ["completed", 200],
    ["completed", 200],
  ]);
  assert.equal(provider.resourceObservation().response_release_count, 2);
  assert.equal(provider.resourceObservation().successful_response_count, 2);
});

test("scripted provider rejects unknown response behavior before binding a listener", async () => {
  await assert.rejects(
    startScriptedProvider({ responseBehavior: "run-number-95" }),
    /unknown scripted provider response behavior/,
  );
  await assert.rejects(
    startScriptedProvider({ doclingReadinessStatus: 199 }),
    /HTTP status from 200 through 599/,
  );
  await assert.rejects(
    startScriptedProvider({ turns: [] }),
    new RegExp(`must contain 1 through ${SCRIPTED_PROVIDER_MAX_TURNS} entries`),
  );
  await assert.rejects(
    startScriptedProvider({ turns: [{ prompt: "one", responseText: "ONE", extra: true }] }),
    /must use its exact schema/,
  );
  await assert.rejects(
    startScriptedProvider({ orderedConversation: true }),
    /requires at least two explicit turns/,
  );
  await assert.rejects(
    startScriptedProvider({
      turns: [{ prompt: "one", responseText: "ONE" }, { prompt: "two", responseText: "TWO" }],
      orderedConversation: "yes",
    }),
    /must be boolean/,
  );
});

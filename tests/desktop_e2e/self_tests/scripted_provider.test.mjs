import assert from "node:assert/strict";
import crypto from "node:crypto";
import test from "node:test";

import {
  SCRIPTED_PROVIDER_MODEL_ID,
  SCRIPTED_PROVIDER_MAX_OUTPUT_TOKENS,
  SCRIPTED_PROVIDER_PROMPT,
  SCRIPTED_PROVIDER_RESPONSE,
  scriptedProviderPortIsFetchSafe,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";

function sha256(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

function responsesRequest() {
  return {
    model: SCRIPTED_PROVIDER_MODEL_ID,
    instructions: "Deterministic fixture instructions.",
    input: [{
      type: "message",
      role: "user",
      content: [{ type: "input_text", text: SCRIPTED_PROVIDER_PROMPT }],
    }],
    max_output_tokens: SCRIPTED_PROVIDER_MAX_OUTPUT_TOKENS,
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
  assert.deepEqual(ledger[1].contract.top_level_keys, ["input", "instructions", "max_output_tokens", "model", "store", "stream"]);
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

test("scripted provider rejects omitted, drifted, and forbidden fixed-config request fields", async (context) => {
  const provider = await startScriptedProvider();
  context.after(() => provider.close());
  const invalidBodies = [
    (() => { const body = responsesRequest(); delete body.instructions; return body; })(),
    { ...responsesRequest(), max_output_tokens: SCRIPTED_PROVIDER_MAX_OUTPUT_TOKENS + 1 },
    { ...responsesRequest(), tools: [] },
    { ...responsesRequest(), previous_response_id: "must-not-be-sent" },
    { ...responsesRequest(), temperature: 0 },
    { ...responsesRequest(), seed: 7 },
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
  assert.deepEqual(ledger[2].contract.forbidden_fields_present, ["tools"]);
  assert.deepEqual(ledger[3].contract.forbidden_fields_present, ["previous_response_id"]);
  assert.deepEqual(ledger[4].contract.forbidden_fields_present, ["temperature"]);
  assert.deepEqual(ledger[5].contract.forbidden_fields_present, ["seed"]);
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

test("scripted provider rejects unknown response behavior before binding a listener", async () => {
  await assert.rejects(
    startScriptedProvider({ responseBehavior: "run-number-95" }),
    /unknown scripted provider response behavior/,
  );
});

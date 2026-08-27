import assert from "node:assert/strict";
import test from "node:test";

import {
  SCRIPTED_PROVIDER_MODEL_ID,
  SCRIPTED_PROVIDER_RESPONSE_PACING_LIMITS,
  createAgentInterruptProviderScript,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";

const PROMPT = "return only PACED_STREAM_OK";
const RESPONSE_TEXT = "PACED_STREAM_OK";
const CADENCE_MS = 250;
const DELTA_COUNT = 5;
const TOTAL_DURATION_MS = CADENCE_MS * (DELTA_COUNT + 1);
const EXPECTED_EVENT_COUNT = DELTA_COUNT + 2;

function responsesRequest() {
  return {
    model: SCRIPTED_PROVIDER_MODEL_ID,
    instructions: "Deterministic paced Responses fixture.",
    input: [{
      type: "message",
      role: "user",
      content: [{ type: "input_text", text: PROMPT }],
    }],
    store: false,
    stream: true,
  };
}

async function request(provider, signal = undefined) {
  return fetch(`${provider.baseUrl}/v1/responses`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(responsesRequest()),
    signal,
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

async function waitFor(predicate, timeoutMs = 3_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  assert.fail(`condition did not settle within ${timeoutMs}ms`);
}

function pacedProviderOptions(overrides = {}) {
  return {
    expectedPrompt: PROMPT,
    responseText: RESPONSE_TEXT,
    responsePacing: { cadenceMs: CADENCE_MS, deltaCount: DELTA_COUNT },
    ...overrides,
  };
}

test("paced Responses sends headers immediately and records valid progress beyond 900ms", async (context) => {
  const provider = await startScriptedProvider(pacedProviderOptions());
  context.after(() => provider.close());

  const response = await request(provider);
  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-type"), "text/event-stream");
  assert.equal(response.headers.get("content-length"), null);

  const [atHeaders] = provider.requestLedger;
  assert.equal(atHeaders.response_phase, "streaming");
  assert.equal(atHeaders.response_status, 200);
  assert.equal(atHeaders.response_stream.headers_sent_elapsed_ms >= 0, true);
  assert.equal(atHeaders.response_stream.terminal_sent, false);
  assert.equal(atHeaders.response_stream.response_finished, false);

  const bodyPromise = response.text();
  await waitFor(() => provider.requestLedger[0]?.response_stream?.events.length >= DELTA_COUNT);
  const [pastTimeout] = provider.requestLedger;
  assert.equal(pastTimeout.response_phase, "streaming");
  assert.equal(pastTimeout.response_stream.events[DELTA_COUNT - 1].event_type, "response.output_text.delta");
  assert.equal(pastTimeout.response_stream.events[DELTA_COUNT - 1].elapsed_ms >= 900, true);
  assert.equal(pastTimeout.response_stream.terminal_sent, false);

  const events = parseSse(await bodyPromise);
  assert.deepEqual(events.map((event) => event.type), [
    ...Array.from({ length: DELTA_COUNT }, () => "response.output_text.delta"),
    "response.output_item.done",
    "response.completed",
  ]);
  assert.equal(
    events.filter((event) => event.type === "response.output_text.delta")
      .map((event) => event.delta)
      .join(""),
    RESPONSE_TEXT,
  );
  assert.equal(events.at(-2).item.content[0].text, RESPONSE_TEXT);
  assert.equal(events.at(-1).response.output[0].content[0].text, RESPONSE_TEXT);

  await waitFor(() => provider.resourceObservation().active_request_count === 0);
  const [completed] = provider.requestLedger;
  const stream = completed.response_stream;
  assert.equal(completed.response_phase, "completed");
  assert.equal(stream.schema_version, "desktop-e2e.scripted-provider-response-stream.v1");
  assert.equal(stream.cadence_ms, CADENCE_MS);
  assert.equal(stream.delta_count, DELTA_COUNT);
  assert.equal(stream.configured_total_duration_ms, TOTAL_DURATION_MS);
  assert.equal(stream.expected_event_count, EXPECTED_EVENT_COUNT);
  assert.equal(stream.headers_sent_elapsed_ms < CADENCE_MS, true);
  assert.equal(stream.events.length, EXPECTED_EVENT_COUNT);
  assert.deepEqual(stream.events.map((event) => event.sequence), [1, 2, 3, 4, 5, 6, 7]);
  assert.equal(stream.events.every((event) => event.size_bytes > 0), true);
  assert.equal(stream.events[0].elapsed_ms < CADENCE_MS, true);
  assert.equal(stream.events.every((event, index) => (
    index === 0 || event.elapsed_ms > stream.events[index - 1].elapsed_ms
  )), true);
  assert.equal(stream.events.every((event, index) => (
    index === 0
      || (event.elapsed_ms - stream.events[index - 1].elapsed_ms >= CADENCE_MS - 25
        && event.elapsed_ms - stream.events[index - 1].elapsed_ms < 900)
  )), true);
  assert.equal(stream.terminal_sent, true);
  assert.equal(stream.terminal_elapsed_ms, stream.events.at(-1).elapsed_ms);
  assert.equal(stream.terminal_elapsed_ms >= 900, true);
  assert.equal(stream.response_finished, true);
  assert.equal(stream.response_finished_elapsed_ms >= stream.terminal_elapsed_ms, true);
  assert.equal(stream.peer_close_observed, false);
  assert.equal(stream.peer_close_elapsed_ms, null);
  assert.equal(stream.peer_closed_before_terminal, false);

  const resource = provider.resourceObservation();
  assert.equal(resource.accepted_response_count, 1);
  assert.equal(resource.successful_response_count, 1);
  assert.equal(resource.paced_response_configured, true);
  assert.deepEqual(resource.paced_response_pacing, {
    cadence_ms: CADENCE_MS,
    delta_count: DELTA_COUNT,
    configured_total_duration_ms: TOTAL_DURATION_MS,
  });
  assert.deepEqual(resource.paced_response_streams, [{
    request_sequence: completed.sequence,
    response_phase: completed.response_phase,
    response_status: completed.response_status,
    ...stream,
  }]);

  const close = await provider.close();
  assert.equal(close.pass, true);
  assert.equal(close.forced_connection_count, 0);
});

test("paced Responses records a peer close before its terminal event", async (context) => {
  const provider = await startScriptedProvider(pacedProviderOptions());
  context.after(() => provider.close());
  const controller = new AbortController();

  const response = await request(provider, controller.signal);
  assert.equal(response.status, 200);
  const bodyPromise = response.text();
  controller.abort();
  await assert.rejects(bodyPromise, (error) => error?.name === "AbortError");

  await waitFor(() => {
    const [row] = provider.requestLedger;
    return row?.response_phase === "peer_closed"
      && row.response_stream?.peer_close_observed === true
      && provider.resourceObservation().active_request_count === 0;
  });
  const [closed] = provider.requestLedger;
  assert.equal(closed.response_status, 200);
  assert.equal(closed.response_stream.terminal_sent, false);
  assert.equal(closed.response_stream.response_finished, false);
  assert.equal(closed.response_stream.peer_closed_before_terminal, true);
  assert.equal(closed.response_stream.peer_close_elapsed_ms >= 0, true);
  assert.equal(closed.response_stream.events.length < EXPECTED_EVENT_COUNT, true);

  const resource = provider.resourceObservation();
  assert.equal(resource.accepted_response_count, 1);
  assert.equal(resource.successful_response_count, 0);
  assert.deepEqual(resource.paced_response_streams, [{
    request_sequence: closed.sequence,
    response_phase: closed.response_phase,
    response_status: closed.response_status,
    ...closed.response_stream,
  }]);

  const close = await provider.close();
  assert.equal(close.pass, true);
  assert.equal(close.forced_connection_count, 0);
});

test("paced Responses configuration is exact and bounded without changing the default mode", async () => {
  assert.deepEqual(SCRIPTED_PROVIDER_RESPONSE_PACING_LIMITS, {
    maximum_cadence_ms: 5_000,
    maximum_delta_count: 32,
    maximum_total_duration_ms: 30_000,
  });
  await assert.rejects(
    startScriptedProvider(pacedProviderOptions({
      responsePacing: { cadenceMs: CADENCE_MS, deltaCount: DELTA_COUNT, extra: true },
    })),
    /responsePacing must use its exact schema/,
  );
  await assert.rejects(
    startScriptedProvider(pacedProviderOptions({ responsePacing: { cadenceMs: 0, deltaCount: 5 } })),
    /cadenceMs must be a positive safe integer/,
  );
  await assert.rejects(
    startScriptedProvider(pacedProviderOptions({ responsePacing: { cadenceMs: 5_001, deltaCount: 5 } })),
    /cadenceMs must not exceed 5000/,
  );
  await assert.rejects(
    startScriptedProvider(pacedProviderOptions({ responsePacing: { cadenceMs: 250, deltaCount: 1 } })),
    /deltaCount must be between 2 and 32/,
  );
  await assert.rejects(
    startScriptedProvider(pacedProviderOptions({ responsePacing: { cadenceMs: 5_000, deltaCount: 6 } })),
    /total duration must not exceed 30000ms/,
  );
  await assert.rejects(
    startScriptedProvider(pacedProviderOptions({ responseBehavior: "hold_until_peer_close" })),
    /paced Responses require complete response behavior/,
  );
  await assert.rejects(
    startScriptedProvider(pacedProviderOptions({ script: createAgentInterruptProviderScript() })),
    /paced Responses cannot use a scripted provider mode/,
  );
  await assert.rejects(
    startScriptedProvider(pacedProviderOptions({ responseText: "tiny" })),
    /paced response text must contain at least 5 Unicode characters/,
  );

  const provider = await startScriptedProvider();
  const resource = provider.resourceObservation();
  assert.equal(resource.paced_response_configured, false);
  assert.equal(resource.paced_response_pacing, null);
  assert.deepEqual(resource.paced_response_streams, []);
  assert.equal((await provider.close()).pass, true);
});

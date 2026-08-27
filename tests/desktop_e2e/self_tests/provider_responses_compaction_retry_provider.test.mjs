import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  SCRIPTED_PROVIDER_CLIENT_GENERATION_KEYS,
  SCRIPTED_PROVIDER_COMPACTION_HEADINGS,
  SCRIPTED_PROVIDER_COMPACTION_SUMMARY_PREFIX,
  SCRIPTED_PROVIDER_MODEL_ID,
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_CHECKPOINT,
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_KIND,
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES,
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_PROMPT,
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RAW_REASONING_SENTINEL,
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RESPONSE,
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_SENTINEL,
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT,
  createResponsesCompactionProviderScript,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";

const SYSTEM = "Deterministic Responses compaction retry fixture instructions.";
const TOOL_OUTPUT = "bounded-read-evidence:".padEnd(12_000, "x");
const COMPACTION_PROMPT_URL = new URL("../../../assets/prompts/compaction.md", import.meta.url);

function inputText(text) {
  return { type: "message", role: "user", content: [{ type: "input_text", text }] };
}

function readTool() {
  return {
    type: "function",
    name: "read",
    description: "Read a bounded text file page.",
    parameters: {
      type: "object",
      properties: {
        path: { type: "string" },
        offset: { type: "integer" },
        limit: { type: "integer" },
      },
      required: ["path"],
    },
  };
}

function readCall(callIndex) {
  const callId = `call_responses_compaction_read_${callIndex}`;
  return [{
    type: "function_call",
    call_id: callId,
    name: "read",
    arguments: JSON.stringify({
      path: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_SENTINEL,
      offset: callIndex,
      limit: 1,
    }),
  }, {
    type: "function_call_output",
    call_id: callId,
    output: `${callIndex}:${TOOL_OUTPUT}`,
  }];
}

function readPrefix(readCount) {
  return [
    inputText(SCRIPTED_PROVIDER_RESPONSES_COMPACTION_PROMPT),
    ...Array.from({ length: readCount }, (_, index) => readCall(index + 1)).flat(),
  ];
}

function toolBody(input, overrides = {}) {
  return {
    model: SCRIPTED_PROVIDER_MODEL_ID,
    instructions: SYSTEM,
    input,
    tools: [readTool()],
    tool_choice: "auto",
    parallel_tool_calls: false,
    store: false,
    stream: true,
    ...overrides,
  };
}

function compactBody(source, compactionPrompt, overrides = {}) {
  return {
    model: SCRIPTED_PROVIDER_MODEL_ID,
    instructions: SYSTEM,
    input: [...source, inputText(compactionPrompt)],
    store: false,
    stream: true,
    ...overrides,
  };
}

function serializedInputBytes(body) {
  return Buffer.byteLength(JSON.stringify(body.input), "utf8");
}

function parseSse(text) {
  return text
    .split("\n\n")
    .filter(Boolean)
    .map((block) => {
      assert.match(block, /^data: /u);
      return JSON.parse(block.slice("data: ".length));
    });
}

async function post(provider, body) {
  return fetch(`${provider.baseUrl}/v1/responses`, {
    method: "POST",
    headers: { "content-type": "application/json; charset=utf-8" },
    body: JSON.stringify(body),
  });
}

async function waitForResponsePhase(provider, role, expected) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const row = provider.requestLedger.find((candidate) => candidate.contract?.role === role);
    if (row?.response_phase === expected) return row;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  const row = provider.requestLedger.find((candidate) => candidate.contract?.role === role);
  assert.equal(row?.response_phase, expected);
  return row;
}

async function fixture() {
  const compactionPrompt = (await readFile(COMPACTION_PROMPT_URL, "utf8")).trim();
  const headings = Array.from(compactionPrompt.matchAll(/^## .+$/gmu), (match) => match[0]);
  assert.deepEqual(headings, SCRIPTED_PROVIDER_COMPACTION_HEADINGS);
  const oversized = compactBody(readPrefix(3), compactionPrompt);
  const retry = compactBody(readPrefix(1), compactionPrompt);
  const oversizedBytes = serializedInputBytes(oversized);
  const retryBytes = serializedInputBytes(retry);
  assert.ok(oversizedBytes > retryBytes);
  const threshold = Math.floor((oversizedBytes + retryBytes) / 2);
  return { compactionPrompt, oversized, retry, oversizedBytes, retryBytes, threshold };
}

async function startCompactionProvider(inputByteThreshold, options = {}) {
  return startScriptedProvider({
    expectedPrompt: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_PROMPT,
    script: createResponsesCompactionProviderScript({ inputByteThreshold }),
    ...options,
  });
}

async function consumeReads(provider) {
  for (let readCount = 0; readCount < SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT; readCount += 1) {
    const response = await post(provider, toolBody(readPrefix(readCount)));
    assert.equal(response.status, 200);
    const events = parseSse(await response.text());
    assert.deepEqual(events.map((event) => event.type), [
      "response.output_item.done",
      "response.completed",
    ]);
    assert.equal(events[0].item.type, "function_call");
    assert.equal(events[0].item.name, "read");
  }
}

test("Responses compaction script returns one reasoning-only saturation, one aligned checkpoint, then final text", async (context) => {
  const prepared = await fixture();
  const provider = await startCompactionProvider(prepared.threshold);
  context.after(() => provider.close());

  await consumeReads(provider);
  const empty = await post(provider, prepared.oversized);
  assert.equal(empty.status, 200);
  const emptyEvents = parseSse(await empty.text());
  assert.deepEqual(emptyEvents.map((event) => event.type), [
    "response.output_item.added",
    "response.reasoning_text.delta",
    "response.reasoning_text.done",
    "response.output_item.done",
    "response.completed",
  ]);
  assert.equal(emptyEvents.some((event) => event.type.includes("output_text")), false);
  assert.equal(emptyEvents[1].delta, SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RAW_REASONING_SENTINEL);
  assert.equal(emptyEvents[2].text, SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RAW_REASONING_SENTINEL);
  assert.deepEqual(emptyEvents.at(-1).response.output.map((item) => item.type), ["reasoning"]);
  assert.deepEqual(emptyEvents.at(-1).response.usage.output_tokens_details, {
    reasoning_tokens: emptyEvents.at(-1).response.usage.output_tokens,
  });

  const checkpoint = await post(provider, prepared.retry);
  assert.equal(checkpoint.status, 200);
  const checkpointEvents = parseSse(await checkpoint.text());
  assert.equal(checkpointEvents[0].delta, SCRIPTED_PROVIDER_RESPONSES_COMPACTION_CHECKPOINT);

  const resumedInput = [
    inputText(SCRIPTED_PROVIDER_RESPONSES_COMPACTION_PROMPT),
    inputText(`${SCRIPTED_PROVIDER_COMPACTION_SUMMARY_PREFIX}\n${SCRIPTED_PROVIDER_RESPONSES_COMPACTION_CHECKPOINT}`),
    ...Array.from(
      { length: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT - 1 },
      (_, index) => readCall(index + 2),
    ).flat(),
  ];
  const final = await post(provider, toolBody(resumedInput));
  assert.equal(final.status, 200);
  const finalEvents = parseSse(await final.text());
  assert.equal(finalEvents[0].delta, SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RESPONSE);

  const rows = provider.requestLedger.filter((row) => row.route === "responses");
  assert.equal(rows.length, SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES);
  assert.deepEqual(rows.map((row) => row.contract.role), [
    ...Array.from(
      { length: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT },
      (_, index) => `read_${index + 1}`,
    ),
    "compaction_empty",
    "compaction_valid",
    "resumed_final",
  ]);
  assert.equal(rows.every((row) => row.contract.pass), true);
  assert.equal(rows.every((row) => row.contract.client_generation_fields_absent), true);
  assert.equal(rows.every((row) => /^[a-f0-9]{64}$/u.test(row.contract.instructions_sha256)), true);
  assert.equal(new Set(rows.map((row) => row.contract.instructions_sha256)).size, 1);
  assert.equal(rows.every((row) => row.response_phase === "completed" && row.response_status === 200), true);
  assert.equal(rows[8].contract.compaction_tools_absent, true);
  assert.equal(rows[9].contract.compaction_tools_absent, true);
  assert.equal(rows[8].contract.role_evidence.input_exceeds_threshold, true);
  assert.equal(rows[9].contract.role_evidence.input_exceeds_threshold, false);
  assert.equal(rows[9].contract.retry_alignment.pass, true);
  assert.equal(rows[10].contract.compaction_replay.pass, true);
  assert.equal(rows[8].contract.role_evidence.input_size_bytes, prepared.oversizedBytes);
  assert.equal(rows[9].contract.role_evidence.input_size_bytes, prepared.retryBytes);

  const resource = provider.resourceObservation();
  assert.equal(resource.script_kind, SCRIPTED_PROVIDER_RESPONSES_COMPACTION_KIND);
  assert.equal(resource.scripted_responses_request_count, SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES);
  assert.equal(resource.accepted_response_count, SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES);
  assert.equal(resource.successful_response_count, SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES);
});

test("Responses compaction script holds after the raw reasoning delta until its exact role release", async (context) => {
  const prepared = await fixture();
  const provider = await startCompactionProvider(prepared.threshold, {
    responseBehavior: "hold_until_release",
  });
  context.after(() => provider.close());

  await consumeReads(provider);
  const empty = await post(provider, prepared.oversized);
  assert.equal(empty.status, 200);
  const heldRows = provider.requestLedger.filter((row) => row.route === "responses");
  assert.equal(heldRows.length, SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT + 1);
  const held = heldRows.at(-1);
  assert.equal(held.contract.role, "compaction_empty");
  assert.equal(held.response_phase, "held");
  assert.equal(held.response_status, 200);
  assert.deepEqual(held.response_stream.events.map((event) => event.event_type), [
    "response.output_item.added",
    "response.reasoning_text.delta",
  ]);
  assert.equal(held.response_stream.hold_after_event_type, "response.reasoning_text.delta");
  assert.equal(held.response_stream.release_observed, false);
  assert.equal(held.response_stream.terminal_sent, false);
  assert.equal(held.response_stream.response_finished, false);
  assert.deepEqual(provider.resourceObservation().script_role_release, {
    role: "compaction_empty",
    released: false,
    released_by_cleanup: false,
  });

  const release = provider.releaseScriptRole("compaction_empty");
  assert.equal(release.released, true);
  assert.equal(release.role, "compaction_empty");
  assert.equal(release.request.response_phase, "held");
  assert.throws(
    () => provider.releaseScriptRole("compaction_empty"),
    /already released/u,
  );
  const events = parseSse(await empty.text());
  assert.deepEqual(events.map((event) => event.type), [
    "response.output_item.added",
    "response.reasoning_text.delta",
    "response.reasoning_text.done",
    "response.output_item.done",
    "response.completed",
  ]);
  assert.equal(events[1].delta, SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RAW_REASONING_SENTINEL);
  const completed = await waitForResponsePhase(provider, "compaction_empty", "completed");
  assert.equal(completed.response_stream.release_observed, true);
  assert.equal(completed.response_stream.terminal_sent, true);
  assert.equal(completed.response_stream.response_finished, true);
  assert.equal(completed.response_stream.peer_close_observed, false);
  assert.deepEqual(provider.resourceObservation().script_role_release, {
    role: "compaction_empty",
    released: true,
    released_by_cleanup: false,
  });
});

test("Responses compaction script rejects host-policy overrides and a non-tool-less compaction", async (context) => {
  const prepared = await fixture();
  const generationProviders = await Promise.all(
    SCRIPTED_PROVIDER_CLIENT_GENERATION_KEYS.map(() => startCompactionProvider(prepared.threshold)),
  );
  const toolProvider = await startCompactionProvider(prepared.threshold);
  context.after(async () => Promise.all([
    ...generationProviders.map((provider) => provider.close()),
    toolProvider.close(),
  ]));

  for (let index = 0; index < generationProviders.length; index += 1) {
    const key = SCRIPTED_PROVIDER_CLIENT_GENERATION_KEYS[index];
    const response = await post(generationProviders[index], toolBody(readPrefix(0), {
      [key]: key === "reasoning" ? { effort: "low" } : true,
    }));
    assert.equal(response.status, 422);
    assert.deepEqual(generationProviders[index].requestLedger[0].contract.client_generation_fields_present, [key]);
  }

  const withTools = await post(toolProvider, toolBody(prepared.oversized.input));
  assert.equal(withTools.status, 422);
  assert.equal(toolProvider.requestLedger[0].contract.pass, false);
});

test("Responses compaction script rejects a smaller request that is not an oldest semantic-unit prefix", async (context) => {
  const prepared = await fixture();
  const provider = await startCompactionProvider(prepared.threshold);
  context.after(() => provider.close());

  await consumeReads(provider);
  assert.equal((await post(provider, prepared.oversized)).status, 200);
  const misalignedSource = [
    inputText(SCRIPTED_PROVIDER_RESPONSES_COMPACTION_PROMPT),
    ...readCall(2),
  ];
  const misaligned = compactBody(misalignedSource, prepared.compactionPrompt);
  assert.ok(serializedInputBytes(misaligned) <= prepared.threshold);
  const response = await post(provider, misaligned);
  assert.equal(response.status, 422);
  assert.deepEqual(await response.json(), { error: "request_contract_mismatch" });
  assert.equal(provider.resourceObservation().accepted_response_count, 9);
});

test("Responses compaction script constructor keeps its exact bounded schema", async () => {
  assert.throws(
    () => createResponsesCompactionProviderScript(),
    /inputByteThreshold must be a positive safe integer/u,
  );
  await assert.rejects(
    startScriptedProvider({
      script: {
        ...createResponsesCompactionProviderScript({ inputByteThreshold: 32_000 }),
        extra: true,
      },
    }),
    /exact schema/u,
  );
});

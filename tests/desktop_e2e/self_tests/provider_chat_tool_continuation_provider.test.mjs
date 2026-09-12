import assert from "node:assert/strict";
import crypto from "node:crypto";
import test from "node:test";

import {
  SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_CALL_ID,
  SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_KIND,
  SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_MAX_RESPONSES,
  SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT,
  SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_RESPONSE,
  SCRIPTED_PROVIDER_CLIENT_GENERATION_KEYS,
  SCRIPTED_PROVIDER_MODEL_ID,
  createChatToolContinuationProviderScript,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";

const SYSTEM = "Deterministic Chat tool continuation fixture instructions.";
const AUTHORIZATION_SECRET = "Bearer must-not-enter-ledger";
const TOOL_OUTPUT = [
  "local: 2026-08-26T12:34:56+09:00",
  "utc: 2026-08-26T03:34:56Z",
  "timezone: +09:00",
  "unix_ms: 1787715296000",
].join("\n");

function sha256(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

function currentTimeTool() {
  return {
    type: "function",
    function: {
      name: "current_time",
      description: "Return the current local and UTC time for date-sensitive work.",
      parameters: { type: "object", properties: {} },
    },
  };
}

function readTool() {
  return {
    type: "function",
    function: {
      name: "read",
      description: "Read one bounded text file.",
      parameters: {
        type: "object",
        properties: { path: { type: "string" } },
        required: ["path"],
      },
    },
  };
}

function initialMessages() {
  return [
    { role: "system", content: SYSTEM },
    { role: "user", content: SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT },
  ];
}

function continuationMessages({ assistantContent = undefined, toolOutput = TOOL_OUTPUT } = {}) {
  const assistant = {
    role: "assistant",
    tool_calls: [{
      id: SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_CALL_ID,
      type: "function",
      function: { name: "current_time", arguments: "{}" },
    }],
  };
  if (assistantContent !== undefined) assistant.content = assistantContent;
  return [
    ...initialMessages(),
    assistant,
    {
      role: "tool",
      content: toolOutput,
      tool_call_id: SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_CALL_ID,
    },
  ];
}

function chatRequest(messages, overrides = {}) {
  return {
    model: SCRIPTED_PROVIDER_MODEL_ID,
    stream: true,
    stream_options: { include_usage: true },
    n: 1,
    messages,
    tools: [currentTimeTool(), readTool()],
    parallel_tool_calls: false,
    ...overrides,
  };
}

function parseSse(text) {
  const blocks = text.split("\n\n").filter(Boolean);
  assert.equal(blocks.pop(), "data: [DONE]", "each successful Chat response must terminate explicitly before EOF");
  return blocks
    .map((block) => {
      assert.match(block, /^data: /);
      return JSON.parse(block.slice("data: ".length));
    });
}

async function post(provider, body, path = "/v1/chat/completions") {
  return fetch(`${provider.baseUrl}${path}`, {
    method: "POST",
    headers: {
      authorization: AUTHORIZATION_SECRET,
      "content-type": "application/json; charset=utf-8",
    },
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

async function startChatProvider() {
  return startScriptedProvider({
    responseBehavior: "hold_until_release",
    script: createChatToolContinuationProviderScript(),
  });
}

test("a selected GUI fixture tool keeps exact arguments and requires its actual output marker", async (context) => {
  const call = { prompt: "Run the isolated read fixture", name: "read", arguments: { path: "receipt.txt" },
    outputMarker: "RECEIPT_FROM_TOOL", responseText: "GUI_FIXTURE_DONE" };
  const provider = await startScriptedProvider({ responseBehavior: "hold_until_release",
    script: createChatToolContinuationProviderScript({ call }) });
  context.after(() => provider.close());
  const initial = [{ role: "system", content: SYSTEM }, { role: "user", content: call.prompt }];
  const absentProvider = await startScriptedProvider({ responseBehavior: "hold_until_release",
    script: createChatToolContinuationProviderScript({ call }) });
  context.after(() => absentProvider.close());
  const absentTool = await post(absentProvider, chatRequest(initial, { tools: [currentTimeTool()] }));
  assert.equal(absentTool.status, 422);
  const response = await post(provider, chatRequest(initial));
  assert.equal(response.status, 200);
  const emitted = parseSse(await response.text())[1].choices[0].delta.tool_calls[0];
  assert.deepEqual(emitted.function, { name: call.name, arguments: JSON.stringify(call.arguments) });
  const continuation = [ ...initial, { role: "assistant", tool_calls: [{
    id: emitted.id, type: emitted.type, function: emitted.function,
  }] }, { role: "tool", tool_call_id: emitted.id, content: call.outputMarker } ];
  for (const mutate of [
    rows => { rows[2].tool_calls[0].function.arguments = '{"path":"wrong.txt"}'; },
    rows => { rows[2].tool_calls[0].function.name = "current_time"; },
    rows => { rows[3].content = "NO_RECEIPT"; },
  ]) {
    const invalidProvider = await startScriptedProvider({ responseBehavior: "hold_until_release",
      script: createChatToolContinuationProviderScript({ call }) });
    context.after(() => invalidProvider.close());
    const invalid = structuredClone(continuation); mutate(invalid);
    assert.equal((await post(invalidProvider, chatRequest(invalid))).status, 422);
  }
  const held = post(provider, chatRequest(continuation));
  await waitFor(() => provider.requestLedger.some(row => row.response_phase === "held"));
  provider.releaseScriptRole("chat_continuation");
  const final = await held;
  assert.equal(final.status, 200);
  assert.equal(parseSse(await final.text())[0].choices[0].delta.content, call.responseText);
  assert.equal(provider.resourceObservation().successful_response_count, 2);
});

test("selected GUI tool fixture rejects malformed or unbounded calls", () => {
  const call = { prompt: "Fixture", name: "read", arguments: { path: "receipt.txt" },
    outputMarker: "RECEIPT", responseText: "DONE" };
  for (const changed of [ { arguments: null }, { arguments: [] }, { name: "Invalid-tool" },
    { arguments: { text: "x".repeat(32769) } }, { excess: true }, { outputMarker: "" } ]) {
    assert.throws(() => createChatToolContinuationProviderScript({ call: { ...call, ...changed } }));
  }
});

test("Chat continuation HTTP responses terminate with DONE after the split tool call and released final", async (context) => {
  const provider = await startChatProvider();
  context.after(() => provider.close());

  const models = await fetch(`${provider.baseUrl}/v1/models`);
  assert.equal(models.status, 200);
  assert.equal((await models.json()).data[0].capabilities.tools, true);

  const initial = await post(provider, chatRequest(initialMessages()));
  assert.equal(initial.status, 200);
  const initialChunks = parseSse(await initial.text());
  assert.equal(initialChunks.length, 3);
  assert.equal(initialChunks[0].choices[0].delta.content, "\n\n<|im_");
  assert.equal(initialChunks[0].choices[0].finish_reason, null);
  assert.equal(initialChunks[1].choices[0].delta.content, "start|>");
  assert.equal(initialChunks[1].choices[0].finish_reason, "tool_calls");
  assert.deepEqual(initialChunks[1].choices[0].delta.tool_calls, [{
    index: 0,
    id: SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_CALL_ID,
    type: "function",
    function: { name: "current_time", arguments: "{}" },
  }]);
  assert.deepEqual(initialChunks[2].choices, []);
  assert.equal(initialChunks[2].usage.total_tokens, 14);

  const continuationRequest = post(provider, chatRequest(continuationMessages()));
  await waitFor(() => provider.requestLedger.some((row) => row.contract?.role === "chat_continuation"
    && row.response_phase === "held"));
  const held = provider.requestLedger.find((row) => row.contract?.role === "chat_continuation");
  assert.equal(held.route, "chat_completions");
  assert.equal(held.contract.role, "chat_continuation");
  assert.equal(held.contract.pass, true);
  assert.equal(held.response_status, null);
  assert.deepEqual(held.contract.role_evidence.message_roles, [
    "system",
    "user",
    "assistant",
    "tool",
  ]);
  assert.equal(held.contract.role_evidence.assistant_content_absent, true);
  assert.equal(held.contract.role_evidence.current_time_call_matches, true);
  assert.equal(held.contract.role_evidence.tool_output_shape_matches, true);
  assert.equal(held.contract.role_evidence.tool_output_sha256, sha256(TOOL_OUTPUT));
  assert.equal(provider.resourceObservation().successful_response_count, 1);
  assert.deepEqual(provider.releaseScriptRole("chat_continuation"), {
    released: true,
    role: "chat_continuation",
    request: held,
  });

  const continuation = await continuationRequest;
  assert.equal(continuation.status, 200);
  const finalChunks = parseSse(await continuation.text());
  assert.equal(finalChunks.length, 2);
  assert.equal(finalChunks[0].choices[0].delta.content, SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_RESPONSE);
  assert.equal(finalChunks[0].choices[0].finish_reason, "stop");
  assert.deepEqual(finalChunks[1].choices, []);
  assert.equal(finalChunks[1].usage.total_tokens, 20);

  const resource = provider.resourceObservation();
  assert.equal(resource.script_kind, SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_KIND);
  assert.equal(resource.scripted_responses_maximum, SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_MAX_RESPONSES);
  assert.equal(resource.scripted_responses_request_count, 2);
  assert.equal(resource.accepted_response_count, 2);
  assert.equal(resource.successful_response_count, 2);
  assert.deepEqual(resource.scripted_response_roles, ["chat_tool_initial", "chat_continuation"]);
  assert.deepEqual(resource.script_role_release, {
    role: "chat_continuation",
    released: true,
    released_by_cleanup: false,
  });

  const rows = provider.requestLedger.filter((row) => row.route === "chat_completions");
  assert.deepEqual(rows.map((row) => [
    row.contract.role,
    row.contract.pass,
    row.response_phase,
    row.response_status,
  ]), [
    ["chat_tool_initial", true, "completed", 200],
    ["chat_continuation", true, "completed", 200],
  ]);
  assert.equal(rows.every((row) => row.contract.model_matches), true);
  assert.equal(rows.every((row) => row.contract.stream_true), true);
  assert.equal(rows.every((row) => row.contract.include_usage_true), true);
  assert.equal(rows.every((row) => row.contract.n_one), true);
  assert.equal(rows.every((row) => row.contract.client_generation_fields_absent), true);
  assert.deepEqual(rows.map((row) => row.contract.client_generation_fields_present), [[], []]);
  assert.equal(rows.every((row) => row.contract.max_tokens_absent), true);
  assert.equal(rows.every((row) => row.contract.parallel_tool_calls_false), true);
  assert.equal(rows.every((row) => row.contract.tools.current_time_schema_matches), true);
  assert.equal(rows.every((row) => row.request_headers.authorization === "<redacted>"), true);
  const serializedLedger = JSON.stringify(provider.requestLedger);
  for (const secret of [SYSTEM, SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT, TOOL_OUTPUT, AUTHORIZATION_SECRET]) {
    assert.equal(serializedLedger.includes(secret), false);
  }
});

test("Chat continuation script rejects malformed DTOs without accepting a role", async (context) => {
  const generationMutations = SCRIPTED_PROVIDER_CLIENT_GENERATION_KEYS.map((key) => (
    (body) => ({
      ...body,
      [key]: key === "reasoning"
        ? { effort: "low" }
        : key === "stop" || key === "stop_sequences"
          ? ["STOP"]
          : key === "chat_template_kwargs" || key === "extra_body" || key === "extra_body_json"
            ? { enable_thinking: false }
            : true,
    })
  ));
  const mutations = [
    (body) => ({ ...body, stream_options: { include_usage: false } }),
    (body) => ({ ...body, n: 2 }),
    ...generationMutations,
    (body) => ({ ...body, parallel_tool_calls: true }),
    (body) => ({ ...body, unexpected: true }),
    (body) => ({
      ...body,
      tools: [{
        ...currentTimeTool(),
        function: {
          ...currentTimeTool().function,
          parameters: { type: "object", properties: {}, additionalProperties: false },
        },
      }],
    }),
  ];
  const providers = await Promise.all(mutations.map(() => startChatProvider()));
  context.after(async () => Promise.all(providers.map((provider) => provider.close())));

  for (let index = 0; index < mutations.length; index += 1) {
    const response = await post(providers[index], mutations[index](chatRequest(initialMessages())));
    assert.equal(response.status, 422);
    assert.deepEqual(await response.json(), { error: "request_contract_mismatch" });
    assert.deepEqual(providers[index].resourceObservation().scripted_response_roles, []);
    assert.equal(providers[index].requestLedger[0].contract.pass, false);
  }
  for (let index = 0; index < SCRIPTED_PROVIDER_CLIENT_GENERATION_KEYS.length; index += 1) {
    const contract = providers[index + 2].requestLedger[0].contract;
    assert.deepEqual(contract.client_generation_fields_present, [SCRIPTED_PROVIDER_CLIENT_GENERATION_KEYS[index]]);
    assert.equal(contract.client_generation_fields_absent, false);
  }
});

test("Chat continuation script enforces continuation shape, order, route, and lifecycle", async (context) => {
  const prematureProvider = await startChatProvider();
  const contentProvider = await startChatProvider();
  const routeProvider = await startChatProvider();
  const ordinaryProvider = await startScriptedProvider();
  context.after(async () => Promise.all([
    prematureProvider.close(),
    contentProvider.close(),
    routeProvider.close(),
    ordinaryProvider.close(),
  ]));

  const premature = await post(prematureProvider, chatRequest(continuationMessages()));
  assert.equal(premature.status, 409);
  assert.deepEqual(await premature.json(), { error: "script_role_prerequisite_missing" });
  assert.deepEqual(prematureProvider.resourceObservation().scripted_response_roles, []);

  assert.equal((await post(contentProvider, chatRequest(initialMessages()))).status, 200);
  const contentPresent = await post(contentProvider, chatRequest(continuationMessages({
    assistantContent: "\n\n<|im_start|>",
  })));
  assert.equal(contentPresent.status, 422);
  assert.deepEqual(await contentPresent.json(), { error: "request_contract_mismatch" });
  assert.equal(contentProvider.requestLedger[1].contract.role, null);
  assert.equal(contentProvider.requestLedger[1].contract.role_evidence.assistant_content_absent, false);

  const wrongScriptRoute = await post(
    routeProvider,
    chatRequest(initialMessages()),
    "/v1/responses",
  );
  assert.equal(wrongScriptRoute.status, 404);
  assert.deepEqual(await wrongScriptRoute.json(), { error: "not_found" });
  const ordinaryChatRoute = await post(ordinaryProvider, chatRequest(initialMessages()));
  assert.equal(ordinaryChatRoute.status, 404);
  assert.deepEqual(await ordinaryChatRoute.json(), { error: "not_found" });

  await assert.rejects(
    startScriptedProvider({ script: createChatToolContinuationProviderScript() }),
    /requires hold_until_release behavior/,
  );
  await assert.rejects(
    startScriptedProvider({
      responseBehavior: "hold_until_release",
      script: { ...createChatToolContinuationProviderScript(), excess: true },
    }),
    /exact schema/,
  );
});

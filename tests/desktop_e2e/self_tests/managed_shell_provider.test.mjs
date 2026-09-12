import assert from "node:assert/strict";
import test from "node:test";
import {
  SCRIPTED_PROVIDER_MANAGED_SHELL_CALL_ID,
  SCRIPTED_PROVIDER_MANAGED_SHELL_KIND,
  SCRIPTED_PROVIDER_MANAGED_SHELL_MAX_OUTPUT_BYTES,
  SCRIPTED_PROVIDER_MODEL_ID,
  createManagedShellProviderScript,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";

const options = {
  taskPrompt: "Start this bounded fixture command, then acknowledge its observed start.",
  command: Array.from({ length: 24 }, (_, index) => `# Fixture line ${index}: bounded server source; not executed by provider tests.`).join("\n"),
  workdir: "C:/fixture/workspace",
  responseText: "MANAGED_START_OBSERVED",
};
const processId = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

function initialRequest() {
  return {
    model: SCRIPTED_PROVIDER_MODEL_ID, stream: true, stream_options: { include_usage: true },
    n: 1, parallel_tool_calls: false,
    messages: [
      { role: "system", content: "Managed shell provider fixture instructions." },
      { role: "user", content: options.taskPrompt },
    ],
    tools: [{ type: "function", function: {
      name: "shell_start", description: "Start an owned bounded command.",
      parameters: { type: "object", required: ["command"], properties: {
        command: { type: "string" }, workdir: { type: "string" },
        timeout_ms: { type: "integer" },
        sandbox_permissions: { type: "string", enum: ["use_default", "require_escalated"] },
      } },
    } }],
  };
}

function continuationRequest(state = "running") {
  const body = initialRequest();
  body.messages.push({ role: "assistant", tool_calls: [{
    id: SCRIPTED_PROVIDER_MANAGED_SHELL_CALL_ID, type: "function",
    function: { name: "shell_start", arguments: JSON.stringify({
      command: options.command, sandbox_permissions: "use_default", timeout_ms: 60_000,
      workdir: options.workdir,
    }) },
  }] }, { role: "tool", tool_call_id: SCRIPTED_PROVIDER_MANAGED_SHELL_CALL_ID,
    content: JSON.stringify({ process_id: processId, state, command: options.command,
      workdir: options.workdir, timeout_ms: 60_000, sandbox: "unrestricted",
      pid: state === "starting" ? null : 4321,
      started_at: state === "starting" ? null : "2026-09-11T03:04:05.123Z",
      finished_at: null, elapsed_ms: null, readiness: "not_checked", success: null,
      result: null, error: null, output_available: false }, null, 2),
  });
  return body;
}

function post(provider, body, path = "/v1/chat/completions") {
  return fetch(`${provider.baseUrl}${path}`, {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body),
  });
}

function parseSse(text) {
  return text.trim().split("\n\n").map((block) => JSON.parse(block.slice("data: ".length)));
}

function startProvider(script = createManagedShellProviderScript(options)) {
  return startScriptedProvider({ script });
}

test("managed shell script emits an exact default-permission call and accepts observed start only", async (context) => {
  for (const state of ["starting", "running"]) {
    await context.test(state, async () => {
      const provider = await startProvider();
      try {
        const catalog = await fetch(`${provider.baseUrl}/v1/models`);
        assert.equal((await catalog.json()).data[0].capabilities.tools, true);
        const initial = await post(provider, initialRequest());
        assert.equal(initial.status, 200);
        const chunks = parseSse(await initial.text());
        assert.equal(chunks[0].model, SCRIPTED_PROVIDER_MODEL_ID);
        assert.equal(chunks[0].choices[0].finish_reason, "tool_calls");
        const call = chunks[0].choices[0].delta.tool_calls[0];
        assert.equal(call.id, SCRIPTED_PROVIDER_MANAGED_SHELL_CALL_ID);
        assert.equal(call.function.name, "shell_start");
        assert.deepEqual(JSON.parse(call.function.arguments), {
          command: options.command, workdir: options.workdir, timeout_ms: 60_000,
          sandbox_permissions: "use_default",
        });
        const request = continuationRequest(state);
        assert.ok(Buffer.byteLength(request.messages[3].content, "utf8") > 512);
        const continuation = await post(provider, request);
        assert.equal(continuation.status, 200);
        const final = parseSse(await continuation.text())[0].choices[0];
        assert.equal(final.delta.content, options.responseText);
        assert.equal(final.finish_reason, "stop");
        const resource = provider.resourceObservation();
        assert.equal(resource.script_kind, SCRIPTED_PROVIDER_MANAGED_SHELL_KIND);
        assert.equal(resource.scripted_responses_maximum, 2);
        assert.equal(resource.accepted_response_count, 2);
        assert.equal(resource.successful_response_count, 2);
        assert.deepEqual(resource.scripted_response_roles, [
          "managed_shell_initial", "managed_shell_continuation",
        ]);
        const evidence = provider.requestLedger.at(-1).contract.role_evidence;
        assert.equal(evidence.process_id, processId);
        assert.equal(evidence.state, state);
        assert.equal(evidence.observed_start_matches, true);
        assert.equal(evidence.shell_start_call_matches, true);
        assert.match(evidence.tool_output_sha256, /^[0-9a-f]{64}$/);
        const ledger = JSON.stringify(provider.requestLedger);
        for (const value of Object.values(options)) assert.equal(ledger.includes(value), false);
        const extra = await post(provider, continuationRequest());
        assert.equal(extra.status, 409);
        assert.deepEqual(await extra.json(), { error: "scripted_response_request_limit_exceeded" });
      } finally { await provider.close(); }
    });
  }
});

test("managed shell script rejects incorrect initial prompt, model, stream, or tool advertisement", async () => {
  const mutations = [
    (body) => { body.messages[1].content = "untrusted alternate task"; },
    (body) => { body.model = "wrong/model"; },
    (body) => { body.stream = false; },
    (body) => { body.tools[0].function.name = "shell"; },
    (body) => { body.tools[0].function.parameters.properties.timeout_ms.type = "string"; },
    (body) => { body.tools[0].function.parameters.required = {}; },
    (body) => { body.tools[0].function.parameters.properties.sandbox_permissions.enum = {}; },
    (body) => { body.tools.push(structuredClone(body.tools[0])); },
  ];
  for (const mutate of mutations) {
    const provider = await startProvider();
    try {
      const request = initialRequest();
      mutate(request);
      const response = await post(provider, request);
      assert.equal(response.status, 422);
      assert.equal(provider.resourceObservation().accepted_response_count, 0);
    } finally { await provider.close(); }
  }
});

test("managed shell script rejects failed, terminal, inferred, mismatched, and extra tool results", async () => {
  const observations = [
    { process_id: "1234" }, { state: "completed" }, { state: "failed" },
    { timeout_ms: 1 }, { readiness: "ready" }, { success: true }, { result: {} },
    { output_available: true }, { error: "start failed" },
  ];
  const mutations = observations.map((patch) => (body) => {
    body.messages[3].content = JSON.stringify({ ...JSON.parse(body.messages[3].content), ...patch });
  });
  mutations.push(
    (body) => { body.messages[3].content = "Tool outcome (host projection): non-success"; },
    (body) => { body.messages[2].tool_calls[0].function.name = "shell"; },
    (body) => { body.messages[2].tool_calls[0].function.arguments = "{}"; },
    (body) => { body.messages[2].tool_calls = { 0: body.messages[2].tool_calls[0], length: 1 }; },
    (body) => { body.messages[3].tool_call_id = "other_call"; },
    (body) => { body.messages.push(structuredClone(body.messages[3])); },
    (body) => {
      body.messages[3].content = JSON.stringify({
        ...JSON.parse(body.messages[3].content),
        command: "x".repeat(SCRIPTED_PROVIDER_MANAGED_SHELL_MAX_OUTPUT_BYTES),
      });
    },
  );
  for (const mutate of mutations) {
    const provider = await startProvider();
    try {
      const initial = await post(provider, initialRequest());
      await initial.text();
      const request = continuationRequest();
      mutate(request);
      const response = await post(provider, request);
      assert.equal(response.status, 422);
      assert.equal(provider.resourceObservation().accepted_response_count, 1);
    } finally { await provider.close(); }
  }
});

test("managed shell script uses the shared role order, route, schema, and lifecycle boundaries", async () => {
  for (const [first, second, expected] of [
    [continuationRequest(), null, "script_role_prerequisite_missing"],
    [initialRequest(), initialRequest(), "script_role_already_consumed"],
  ]) {
    const provider = await startProvider();
    try {
      let response = await post(provider, first);
      if (second) { await response.text(); response = await post(provider, second); }
      assert.equal(response.status, 409);
      assert.deepEqual(await response.json(), { error: expected });
      const wrongRoute = await post(provider, initialRequest(), "/v1/responses");
      assert.equal(wrongRoute.status, 404);
    } finally { await provider.close(); }
  }
  assert.throws(() => createManagedShellProviderScript({ ...options, timeoutMs: 0 }), /positive/);
  assert.throws(() => createManagedShellProviderScript({ ...options, workdir: "" }), /non-empty/);
  await assert.rejects(startProvider({ ...createManagedShellProviderScript(options), extra: true }), /exact schema/);
  await assert.rejects(startScriptedProvider({
    script: createManagedShellProviderScript(options), responseBehavior: "hold_until_release",
  }), /owns its response lifecycle/);
});

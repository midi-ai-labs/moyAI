import assert from "node:assert/strict";
import test from "node:test";
import {
  PERMISSION_TEMP_WRITE_COMMAND,
  PERMISSION_TEMP_WRITE_LIVE_PROMPT,
  PERMISSION_TEMP_WRITE_SECRET_CANARY,
  createPermissionTempWriteOpenAiCompatibleScenario,
  normalizePermissionTempWriteOpenAiCompatibleOptions,
  permissionTempWriteOpenAiCompatibleCaptureMetadataFailures,
  permissionTempWriteOpenAiCompatibleFixtureConfig,
  permissionTempWriteOpenAiCompatibleLiveCaptureContract,
  permissionTempWriteOpenAiCompatibleLiveTerminalFailures,
} from "../scenarios/permission_temp_write.mjs";

const options = { providerBaseUrl: "http://localhost:8119/v1", model: "qwen/example" };
function bodies() {
  const user = { role: "user", content: PERMISSION_TEMP_WRITE_LIVE_PROMPT };
  const system = { role: "system", content: "bounded TEMP write task" };
  const call = { role: "assistant", tool_calls: [{ type: "function", id: "call-temp",
    function: { name: "shell", arguments: JSON.stringify({ command: PERMISSION_TEMP_WRITE_COMMAND,
      sandbox_permissions: "use_default" }) } }] };
  const tool = { role: "tool", tool_call_id: "call-temp",
    content: `Command: ${PERMISSION_TEMP_WRITE_COMMAND}\nExit code: 0\n1 passed in 0.1s` };
  const body = messages => ({ model: options.model, messages, n: 1, stream: true,
    stream_options: { include_usage: true }, parallel_tool_calls: false,
    tools: [{ type: "function", function: { name: "shell", description: "shell", parameters: {} } }] });
  return structuredClone([{ body: body([system, user]) }]).concat(
    structuredClone([{ body: body([system, user, call, tool]) }]));
}

test("OpenAI-compatible TEMP write route is credential-free, fixed-profile and separate from Guardian", () => {
  const normalized = normalizePermissionTempWriteOpenAiCompatibleOptions({ provider_base_url: options.providerBaseUrl,
    model: options.model });
  assert.deepEqual(normalized, options);
  const config = permissionTempWriteOpenAiCompatibleFixtureConfig(normalized);
  assert.match(config, /provider_profile = "openai_compatible"/u);
  assert.doesNotMatch(config, /api_key_env|temperature|reasoning|\[approve\]/u);
  const scenario = createPermissionTempWriteOpenAiCompatibleScenario({ provider_base_url: options.providerBaseUrl,
    model: options.model });
  assert.equal(scenario.id, "manual.permission-temp-write-openai-compatible");
  assert.equal(scenario.environment.MOYAI_E2E_PERMISSION_TEMP_WRITE_SECRET, PERMISSION_TEMP_WRITE_SECRET_CANARY);
  for (const value of [{ provider_base_url: "https://user:secret@host/v1", model: "a" },
    { provider_base_url: options.providerBaseUrl, model: "a", api_key_env: "KEY" }]) {
    assert.throws(() => normalizePermissionTempWriteOpenAiCompatibleOptions(value), TypeError);
  }
});

test("Chat TEMP write accepts two Main requests with one ordinary successful shell pair", () => {
  const result = permissionTempWriteOpenAiCompatibleLiveCaptureContract(bodies(), options);
  assert.deepEqual(result.failures, []);
  assert.deepEqual(result.roles, ["live_initial", "live_continuation"]);
  assert.equal(result.secret_body_fields_absent, true);
  assert.equal(result.secret_canary_absent, true);
  assert.match(result.restricted_output_sha256, /^[a-f0-9]{64}$/u);
});

test("Chat TEMP write rejects elevation, Guardian, changed identity and unsuccessful output", () => {
  for (const mutate of [
    value => value.push(value[1]),
    value => { delete value[1].body.tools; delete value[1].body.parallel_tool_calls; },
    value => { value[1].body.messages[2].tool_calls[0].function.arguments = JSON.stringify({
      command: PERMISSION_TEMP_WRITE_COMMAND, sandbox_permissions: "require_escalated" }); },
    value => { value[1].body.messages[3].tool_call_id = "different"; },
    value => { value[1].body.messages[3].content = "Exit code: 1\n1 error"; },
    value => { value[1].body.messages[0].content = "changed system"; },
    value => { value[1].body.messages[1].content = "changed prompt"; },
    value => { value[1].body.messages[3].content += "\nSandbox note: retry"; },
  ]) {
    const value = bodies(); mutate(value);
    assert.notDeepEqual(permissionTempWriteOpenAiCompatibleLiveCaptureContract(value, options).failures, []);
  }
});

test("Chat TEMP write rejects generation overrides and credential leakage on either Main request", () => {
  for (const key of ["temperature", "max_tokens", "reasoning", "api_key_env", "authorization", "headers"]) {
    for (const index of [0, 1]) {
      const value = bodies(); value[index].body[key] = "unexpected";
      assert.notDeepEqual(permissionTempWriteOpenAiCompatibleLiveCaptureContract(value, options).failures, []);
    }
  }
  const value = bodies(); value[1].body.messages[3].content += PERMISSION_TEMP_WRITE_SECRET_CANARY;
  assert.ok(permissionTempWriteOpenAiCompatibleLiveCaptureContract(value, options).failures.includes("live-chat-secret-boundary-mismatch"));
});

test("Chat prepared capture metadata must bind the exact current route and stage", () => {
  const exact = { api_mode: "chat_completions", capture_stage: "prepared", captured_at_unix_ms: 1,
    endpoint_path: "v1/chat/completions", process_id: 1, request_body_bytes: 12,
    request_body_file: "capture.request.json", request_id: "request-1", schema_version: 2,
    sequence: 0, transport: "http" };
  assert.deepEqual(permissionTempWriteOpenAiCompatibleCaptureMetadataFailures(exact), []);
  for (const change of [{ api_mode: "responses" }, { endpoint_path: "v1/responses" },
    { capture_stage: "network_attempt" }, { authorization: "unexpected" }, { request_id: "" }]) {
    assert.notDeepEqual(permissionTempWriteOpenAiCompatibleCaptureMetadataFailures({ ...exact, ...change }), []);
  }
});

test("Chat TEMP write terminal checks provider identity and canary independently of ordinary completion", () => {
  const projection = { provider_effective_profile: "openai_compatible",
    provider_effective_base_url: options.providerBaseUrl, provider_effective_model_id: options.model,
    provider_effective_api_key_env: "" };
  assert.ok(!permissionTempWriteOpenAiCompatibleLiveTerminalFailures({ projection }, options).includes("live-chat-provider-target-mismatch"));
  assert.ok(permissionTempWriteOpenAiCompatibleLiveTerminalFailures({ projection: { ...projection,
    provider_effective_model_id: "other" } }, options).includes("live-chat-provider-target-mismatch"));
  assert.ok(permissionTempWriteOpenAiCompatibleLiveTerminalFailures({ projection: { ...projection,
    provider_effective_api_key_env: "KEY" } }, options).includes("live-chat-secret-boundary-mismatch"));
  assert.ok(permissionTempWriteOpenAiCompatibleLiveTerminalFailures({ projection,
    text: PERMISSION_TEMP_WRITE_SECRET_CANARY }, options).includes("live-chat-secret-canary-exposed"));
});

import assert from "node:assert/strict";
import crypto from "node:crypto";
import test from "node:test";

import {
  SCRIPTED_PROVIDER_MODEL_ID,
  SCRIPTED_PROVIDER_PERMISSION_TEMP_ESCALATION_KIND,
  SCRIPTED_PROVIDER_PERMISSION_TEMP_ESCALATION_MAX_RESPONSES,
  createPermissionTempEscalationProviderScript,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";

const TASK_PROMPT = "run the exact bounded owner-only TEMP regression";
const COMMAND = "python -B -X utf8 -m pytest -p no:cacheprovider -q --tb=short test_e2e_owner_only_temp.py";
const JUSTIFICATION = "retry the exact trusted pytest command outside the workspace-write sandbox";
const FINAL_RESPONSE = "PERMISSION_TEMP_ESCALATION_OK";
const INSTRUCTIONS = "Deterministic permission TEMP escalation fixture instructions.";
const GUARDIAN_INSTRUCTIONS = "You are moyAI's independent permission guardian.";
const NOTE = "Sandbox note: this workspace-write run matched the known protected effect-TEMP access-denied signature. No retry occurred. If this exact command is required and the workspace is trusted, issue a new shell call with sandbox_permissions=require_escalated and concise justification; do not change project files solely to bypass this sandbox restriction.";
const RESTRICTED_OUTPUT = [
  "Tool outcome (host projection): non-success",
  'tool: "shell"',
  "lifecycle_status: completed",
  "kind: workspace_write_effect_temp_access_denied",
  "automatic_retry: false",
  "exit_code: 1",
  `guidance: ${NOTE}`,
  "",
  "Tool evidence (bounded; canonical result unchanged):",
  `Command: ${COMMAND}`,
  "",
  "Exit code: 1",
  "",
  "Stderr:",
  "E PermissionError: [WinError 5] Access is denied: 'C:\\Temp\\moyai-sandbox-effect-ABC\\pytest-of-user'",
].join("\n");
const ELEVATED_OUTPUT = [
  `Command: ${COMMAND}`,
  "",
  "Exit code: 0",
  "",
  "Stdout:",
  ".                                                                        [100%]",
  "1 passed in 0.12s",
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
    store: false,
    stream: true,
  };
}

function guardianPayload(call) {
  return {
    trusted_world_state: { schema_version: "fixture-world-state.v1" },
    task_context: JSON.stringify({
      authority_session_id: "01M00000000000000000000000",
      canonical_user_authority: [{
        kind: "user_turn",
        history_item_id: "01M00000000000000000000001",
        text: TASK_PROMPT,
      }],
    }),
    recent_committed_response: {
      response_id: "01M00000000000000000000002",
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
      summary: "Retry the exact trusted pytest command",
      details: [`Requested sandbox elevation: ${JUSTIFICATION}`],
      targets: ["C:/fixture/workspace"],
      outside_workspace: true,
      risks: [],
    },
    action_evidence: { kind: "permission_request" },
  };
}

function guardianRequest(call, transform = (value) => value) {
  return {
    model: SCRIPTED_PROVIDER_MODEL_ID,
    instructions: GUARDIAN_INSTRUCTIONS,
    input: [userMessage(JSON.stringify(transform(guardianPayload(call))))],
    store: false,
    stream: true,
  };
}

function script(guardianDelayMs = 0) {
  return createPermissionTempEscalationProviderScript({
    taskPrompt: TASK_PROMPT,
    command: COMMAND,
    justification: JUSTIFICATION,
    responseText: FINAL_RESPONSE,
    guardianDelayMs,
  });
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

async function restrictedCall(provider) {
  const response = await post(provider, taskRequest([userMessage(TASK_PROMPT)]));
  assert.equal(response.status, 200);
  return parseSse(await response.text())[0].item;
}

async function heldElevatedCall(provider, firstCall) {
  const responsePromise = post(provider, taskRequest([
    userMessage(TASK_PROMPT),
    {
      type: "function_call",
      call_id: firstCall.call_id,
      name: firstCall.name,
      arguments: firstCall.arguments,
    },
    {
      type: "function_call_output",
      call_id: firstCall.call_id,
      output: RESTRICTED_OUTPUT,
    },
  ]));
  await waitFor(() => provider.requestLedger.at(-1)?.response_phase === "held");
  const held = provider.requestLedger.at(-1);
  assert.equal(held.contract.role, "temp_escalation");
  assert.equal(held.contract.role_evidence.restricted_output.pass, true);
  assert.equal(held.contract.role_evidence.restricted_output.single_note, true);
  assert.equal(held.contract.role_evidence.restricted_output.same_line_windows_signature, true);
  assert.deepEqual(provider.resourceObservation().script_role_release, {
    role: "temp_escalation",
    released: false,
    released_by_cleanup: false,
  });
  assert.deepEqual(provider.releaseScriptRole("temp_escalation"), {
    released: true,
    role: "temp_escalation",
    request: held,
  });
  const response = await responsePromise;
  assert.equal(response.status, 200);
  return parseSse(await response.text())[0].item;
}

test("permission TEMP escalation script enforces restricted failure, exact elevation, Guardian, and success", async (context) => {
  const guardianDelayMs = 25;
  const provider = await startScriptedProvider({
    responseBehavior: "hold_until_release",
    script: script(guardianDelayMs),
  });
  context.after(() => provider.close());

  const models = await fetch(`${provider.baseUrl}/api/v1/models`);
  assert.equal(models.status, 200);
  const catalog = await models.json();
  assert.equal(catalog.models[0].key, SCRIPTED_PROVIDER_MODEL_ID);
  assert.equal(catalog.models[0].capabilities.trained_for_tool_use, true);

  const firstCall = await restrictedCall(provider);
  assert.deepEqual(JSON.parse(firstCall.arguments), {
    command: COMMAND,
    sandbox_permissions: "use_default",
  });
  const secondCall = await heldElevatedCall(provider, firstCall);
  assert.deepEqual(JSON.parse(secondCall.arguments), {
    command: COMMAND,
    sandbox_permissions: "require_escalated",
    justification: JUSTIFICATION,
  });

  const guardian = await post(provider, guardianRequest(secondCall));
  assert.equal(guardian.status, 200);
  assert.equal(parseSse(await guardian.text())[0].delta, JSON.stringify({
    decision: "allow",
    rationale: "bounded deterministic fixture command",
  }));

  const continuation = await post(provider, taskRequest([
    userMessage(TASK_PROMPT),
    {
      type: "function_call",
      call_id: firstCall.call_id,
      name: firstCall.name,
      arguments: firstCall.arguments,
    },
    {
      type: "function_call_output",
      call_id: firstCall.call_id,
      output: RESTRICTED_OUTPUT,
    },
    {
      type: "function_call",
      call_id: secondCall.call_id,
      name: secondCall.name,
      arguments: secondCall.arguments,
    },
    {
      type: "function_call_output",
      call_id: secondCall.call_id,
      output: ELEVATED_OUTPUT,
    },
  ]));
  assert.equal(continuation.status, 200);
  assert.equal(parseSse(await continuation.text())[0].delta, FINAL_RESPONSE);

  const rows = provider.requestLedger.filter((row) => row.route === "responses");
  assert.deepEqual(rows.map((row) => [
    row.contract.role,
    row.contract.pass,
    row.response_phase,
    row.response_status,
  ]), [
    ["temp_initial", true, "completed", 200],
    ["temp_escalation", true, "completed", 200],
    ["temp_guardian", true, "completed", 200],
    ["temp_continuation", true, "completed", 200],
  ]);
  assert.equal(rows[1].contract.role_evidence.restricted_output.output_sha256, sha256(RESTRICTED_OUTPUT));
  assert.equal(rows[2].contract.role_evidence.payload.authority_count, 1);
  assert.equal(rows[2].contract.role_evidence.payload.authority_matches, true);
  assert.equal(rows[2].contract.reasoning_absent, true);
  assert.deepEqual(rows[2].response_delay, {
    schema_version: "desktop-e2e.scripted-provider-guardian-delay.v1",
    configured_delay_ms: guardianDelayMs,
    delay_elapsed_ms: rows[2].response_delay.delay_elapsed_ms,
    delay_completed: true,
    peer_close_observed: false,
    headers_sent_elapsed_ms: rows[2].response_delay.headers_sent_elapsed_ms,
  });
  assert.ok(rows[2].response_delay.delay_elapsed_ms >= guardianDelayMs);
  assert.ok(
    rows[2].response_delay.headers_sent_elapsed_ms >= rows[2].response_delay.delay_elapsed_ms,
  );
  assert.equal(rows[3].contract.role_evidence.elevated_output.output_sha256, sha256(ELEVATED_OUTPUT));

  const resource = provider.resourceObservation();
  assert.equal(resource.script_kind, SCRIPTED_PROVIDER_PERMISSION_TEMP_ESCALATION_KIND);
  assert.equal(resource.scripted_responses_maximum, SCRIPTED_PROVIDER_PERMISSION_TEMP_ESCALATION_MAX_RESPONSES);
  assert.equal(resource.scripted_responses_request_count, 4);
  assert.equal(resource.accepted_response_count, 4);
  assert.equal(resource.successful_response_count, 4);
  assert.deepEqual(resource.scripted_response_roles, [
    "temp_initial",
    "temp_escalation",
    "temp_guardian",
    "temp_continuation",
  ]);

  const serializedLedger = JSON.stringify(provider.requestLedger);
  for (const secret of [TASK_PROMPT, COMMAND, JUSTIFICATION, RESTRICTED_OUTPUT, ELEVATED_OUTPUT]) {
    assert.equal(serializedLedger.includes(secret), false, `ledger leaked ${secret}`);
  }
});

test("permission TEMP escalation script fails closed when the exact host hint or command drifts", async (context) => {
  const splitSignature = await startScriptedProvider({ script: script() });
  const commandDrift = await startScriptedProvider({ script: script() });
  context.after(async () => Promise.all([splitSignature.close(), commandDrift.close()]));

  const splitCall = await restrictedCall(splitSignature);
  const split = await post(splitSignature, taskRequest([
    userMessage(TASK_PROMPT),
    {
      type: "function_call",
      call_id: splitCall.call_id,
      name: splitCall.name,
      arguments: splitCall.arguments,
    },
    {
      type: "function_call_output",
      call_id: splitCall.call_id,
      output: RESTRICTED_OUTPUT.replace(
        "PermissionError: [WinError 5] Access is denied: 'C:\\Temp\\moyai-sandbox-effect-",
        "PermissionError: [WinError 5] Access is denied\nC:\\Temp\\moyai-sandbox-effect-",
      ),
    },
  ]));
  assert.equal(split.status, 422);
  assert.deepEqual(await split.json(), { error: "request_contract_mismatch" });
  assert.equal(splitSignature.requestLedger.at(-1).contract.role, null);
  assert.equal(
    splitSignature.requestLedger.at(-1).contract.role_evidence.restricted_output.same_line_windows_signature,
    false,
  );

  const driftCall = await restrictedCall(commandDrift);
  const drift = await post(commandDrift, taskRequest([
    userMessage(TASK_PROMPT),
    {
      type: "function_call",
      call_id: driftCall.call_id,
      name: driftCall.name,
      arguments: driftCall.arguments,
    },
    {
      type: "function_call_output",
      call_id: driftCall.call_id,
      output: RESTRICTED_OUTPUT.replace(`Command: ${COMMAND}`, `Command: ${COMMAND} --basetemp .pytest_tmp`),
    },
  ]));
  assert.equal(drift.status, 422);
  assert.deepEqual(await drift.json(), { error: "request_contract_mismatch" });
  assert.equal(commandDrift.requestLedger.at(-1).contract.role, null);
  assert.equal(commandDrift.requestLedger.at(-1).contract.role_evidence.restricted_output.command_matches, false);
  assert.equal(commandDrift.requestLedger.at(-1).contract.role_evidence.restricted_output.pass, false);
});

test("permission TEMP escalation script validates its exact builder and release lifecycle", async () => {
  assert.throws(
    () => createPermissionTempEscalationProviderScript({
      taskPrompt: TASK_PROMPT,
      command: "",
      justification: JUSTIFICATION,
      responseText: FINAL_RESPONSE,
    }),
    /command must be a non-empty string/u,
  );
  assert.throws(
    () => createPermissionTempEscalationProviderScript({
      taskPrompt: TASK_PROMPT,
      command: COMMAND,
      justification: JUSTIFICATION,
      responseText: FINAL_RESPONSE,
      guardianDelayMs: -1,
    }),
    /guardianDelayMs must be a non-negative safe integer/u,
  );
  await assert.rejects(
    startScriptedProvider({ script: { ...script(), extra: true } }),
    /exact schema/u,
  );
  await assert.rejects(
    startScriptedProvider({
      script: script(),
      responseBehavior: "hold_until_peer_close",
    }),
    /owns its response lifecycle/u,
  );
  const provider = await startScriptedProvider({ script: script() });
  assert.throws(() => provider.releaseScriptRole("temp_escalation"), /not configured/u);
  assert.equal((await provider.close()).pass, true);
});

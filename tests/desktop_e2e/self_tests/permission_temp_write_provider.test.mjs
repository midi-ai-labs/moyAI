import assert from "node:assert/strict";
import crypto from "node:crypto";
import test from "node:test";

import {
  SCRIPTED_PROVIDER_MODEL_ID,
  SCRIPTED_PROVIDER_PERMISSION_TEMP_WRITE_KIND,
  SCRIPTED_PROVIDER_PERMISSION_TEMP_WRITE_MAX_RESPONSES,
  createPermissionTempWriteProviderScript,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";

const TASK_PROMPT = "run the exact bounded owner-only TEMP regression";
const COMMAND = "python -B -X utf8 -m pytest -p no:cacheprovider -q --tb=short test_e2e_owner_only_temp.py";

const FINAL_RESPONSE="PERMISSION_TEMP_WRITE_OK";
const INSTRUCTIONS="Deterministic permission TEMP write fixture instructions.";
const output='Command: '+COMMAND+'\nExit code: 0\n1 passed in 0.12s';
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

function script(){return createPermissionTempWriteProviderScript({taskPrompt:TASK_PROMPT,command:COMMAND,responseText:FINAL_RESPONSE});}
function continuation(call,result=output){return taskRequest([userMessage(TASK_PROMPT),
  {type:"function_call",call_id:call.call_id,name:call.name,arguments:call.arguments},
  {type:"function_call_output",call_id:call.call_id,output:result}]);}
test("TEMP write provider returns one use_default call then holds the successful continuation",async context=>{
  const provider=await startScriptedProvider({script:script(),responseBehavior:"hold_until_release"});context.after(()=>provider.close());
  const call=await restrictedCall(provider);assert.deepEqual(JSON.parse(call.arguments),{command:COMMAND,sandbox_permissions:"use_default"});
  const pending=post(provider,continuation(call));await waitFor(()=>provider.requestLedger.at(-1)?.response_phase==="held");
  assert.equal(provider.requestLedger.at(-1).contract.role,"temp_continuation");
  assert.equal(provider.requestLedger.at(-1).contract.role_evidence.restricted_output.pass,true);
  assert.equal(provider.releaseScriptRole("temp_continuation").released,true);
  const response=await pending;assert.equal(response.status,200);assert.ok((await response.text()).includes(FINAL_RESPONSE));
  assert.deepEqual(provider.requestLedger.map(row=>row.contract.role),["temp_initial","temp_continuation"]);
  assert.equal(provider.resourceObservation().accepted_response_count,2);
  assert.equal(SCRIPTED_PROVIDER_PERMISSION_TEMP_WRITE_MAX_RESPONSES,2);
});
test("TEMP write provider rejects failed output, command drift and elevation",async context=>{
  for(const mutation of [body=>body.input[2].output=output.replace('Exit code: 0','Exit code: 1'),
    body=>body.input[2].output+='\nSandbox note: retry',body=>body.input[2].call_id="wrong",
    body=>body.input[1].arguments=JSON.stringify({command:COMMAND,sandbox_permissions:"require_escalated"}),
    body=>body.input[1].arguments=JSON.stringify({command:COMMAND+' --basetemp=.',sandbox_permissions:"use_default"})]){
    const provider=await startScriptedProvider({script:script()});context.after(()=>provider.close());
    const body=continuation(await restrictedCall(provider));mutation(body);
    const response=await post(provider,body);assert.equal(response.status,422);await response.text();
  }
});
test("TEMP write provider rejects tool-less Guardian-shaped requests",async context=>{
  const provider=await startScriptedProvider({script:script()});context.after(()=>provider.close());
  await restrictedCall(provider);
  const response=await post(provider,{model:SCRIPTED_PROVIDER_MODEL_ID,instructions:"independent permission guardian",
    input:[userMessage(TASK_PROMPT)],store:false,stream:true});assert.equal(response.status,422);await response.text();
});
test("TEMP write provider rejects out-of-order continuation and repeated initial requests", async context => {
  const first = await startScriptedProvider({ script: script() });
  const second = await startScriptedProvider({ script: script() });
  context.after(() => Promise.all([first.close(), second.close()]));
  const call = await restrictedCall(first);
  const outOfOrder = await post(second, continuation(call));
  assert.equal(outOfOrder.status, 409);
  assert.deepEqual(await outOfOrder.json(), { error: "script_role_prerequisite_missing" });
  const repeated = await post(first, taskRequest([userMessage(TASK_PROMPT)]));
  assert.equal(repeated.status, 409);
  assert.deepEqual(await repeated.json(), { error: "script_role_already_consumed" });
  const overLimit = await post(first, continuation(call));
  assert.equal(overLimit.status, 409);
  assert.deepEqual(await overLimit.json(), { error: "scripted_response_request_limit_exceeded" });
  assert.equal(first.resourceObservation().accepted_response_count, 1);
});
test("TEMP write provider owns the two-role schema and closes a held response on cleanup",async()=>{
  assert.equal(script().kind,SCRIPTED_PROVIDER_PERMISSION_TEMP_WRITE_KIND);
  assert.throws(() => createPermissionTempWriteProviderScript({ taskPrompt: TASK_PROMPT,
    command: "", responseText: FINAL_RESPONSE }), /command must be a non-empty string/u);
  await assert.rejects(()=>startScriptedProvider({script:{...script(),guardianDelayMs:1}}),TypeError);
  const provider=await startScriptedProvider({script:script(),responseBehavior:"hold_until_release"});
  const pending=post(provider,continuation(await restrictedCall(provider)));
  await waitFor(()=>provider.requestLedger.at(-1)?.response_phase==="held");
  await provider.close();await pending.then(response=>response.text()).catch(error=>assert.match(error.message,/fetch failed|terminated/u));
  assert.equal(provider.resourceObservation().script_role_release.released_by_cleanup,true);
});

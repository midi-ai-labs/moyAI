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


function withClock(input) {
  const body=request(input);
  body.tools.push({type:'function',name:'current_time',description:'Read the current local clock.',parameters:{type:'object',properties:{},additionalProperties:false}});
  return body;
}
const CLOCK='local: 2026-09-12T12:00:00+09:00\nutc: 2026-09-12T03:00:00Z\ntimezone: +09:00\nunix_ms: 1789182000000';
test('bounded child history executes 42 exact clock continuations then remains interruptible',async context=>{
  const provider=await startScriptedProvider({expectedPrompt:PROMPT,script:createAgentInterruptProviderScript({childToolCalls:42})});
  context.after(()=>provider.close());
  const spawnResponse=await post(provider,withClock([userMessage(PROMPT)]));
  assert.equal(spawnResponse.status,200);
  const spawn=parseSse(await spawnResponse.text())[0].item;
  const rootInput=[userMessage(PROMPT),{type:'function_call',call_id:spawn.call_id,name:spawn.name,arguments:spawn.arguments},
    {type:'function_call_output',call_id:spawn.call_id,output:JSON.stringify({task_name:`/root/${SCRIPTED_PROVIDER_AGENT_INTERRUPT_TASK_NAME}`})}];
  const root=await post(provider,withClock(rootInput));assert.equal(root.status,200);await root.text();
  const input=[userMessage(childEnvelope())];
  for(let index=0;index<42;index++){
    const response=await post(provider,withClock(input));assert.equal(response.status,200);
    const events=parseSse(await response.text()),call=events[0].item;
    assert.deepEqual({name:call.name,args:call.arguments,id:call.call_id},{name:'current_time',args:'{}',id:`call_agent_history_${index}`});
    input.push({type:'function_call',call_id:call.call_id,name:call.name,arguments:call.arguments},
      {type:'function_call_output',call_id:call.call_id,output:CLOCK});
  }
  const abort=new AbortController(),held=post(provider,withClock(input),abort.signal);
  await waitFor(()=>provider.requestLedger.some(row=>row.contract?.role==='child_held'&&row.response_phase==='held'));
  assert.equal(provider.resourceObservation().scripted_responses_maximum,45);
  assert.equal(provider.requestLedger.length,45);
  assert.equal(provider.requestLedger.every(row=>row.contract.pass),true);
  abort.abort();await assert.rejects(held,error=>error.name==='AbortError');
  await waitFor(()=>provider.requestLedger.at(-1).response_phase==='peer_closed');
});
test('child clock continuation rejects altered call arguments and output rather than accepting arbitrary context',async context=>{
  const provider=await startScriptedProvider({expectedPrompt:PROMPT,script:createAgentInterruptProviderScript({childToolCalls:2})});
  context.after(()=>provider.close());
  await (await post(provider,withClock([userMessage(PROMPT)]))).text();
  const first=await post(provider,withClock([userMessage(childEnvelope())]));const call=parseSse(await first.text())[0].item;
  const bad=[userMessage(childEnvelope()),{type:'function_call',call_id:call.call_id,name:call.name,arguments:'{"bad":true}'},
    {type:'function_call_output',call_id:call.call_id,output:CLOCK}];
  const rejected=await post(provider,withClock(bad));assert.equal(rejected.status,422);await rejected.text();
  bad[1].arguments='{}';bad[2].output='arbitrary';
  const outputRejected=await post(provider,withClock(bad));assert.equal(outputRejected.status,422);await outputRejected.text();
  assert.equal(provider.requestLedger.filter(row=>row.response_phase==='rejected').length,2);
});
test('child history limit is optional, integral and bounded without changing default shape',()=>{
  assert.deepEqual(createAgentInterruptProviderScript({childToolCalls:0}),createAgentInterruptProviderScript());
  for(const childToolCalls of [-1,65,1.1,'42',null])assert.throws(()=>createAgentInterruptProviderScript({childToolCalls}));
});

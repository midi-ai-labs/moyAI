import assert from "node:assert/strict";
import test from "node:test";

import {
  SCRIPTED_PROVIDER_CLIENT_GENERATION_KEYS,
  SCRIPTED_PROVIDER_MODEL_ID,
  SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_PROMPT,
  SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_RESPONSE,
  SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_BETA_PROMPT,
  SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_BETA_RESPONSE,
  SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_KIND,
  SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_MAX_RESPONSES,
  SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_QUESTION,
  SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_SIDE_RESPONSE,
  createSideChatSessionProviderScript,
  sideChatSessionOwnerContext,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";

const OWNER_ID = `${"0".repeat(25)}1`;
const USER_ID = `${"0".repeat(25)}2`;
const ASSISTANT_ID = `${"0".repeat(25)}3`;
const APPEND_POSITION = "17";

function userMessage(text) {
  return { type: "message", role: "user", content: [{ type: "input_text", text }] };
}

function request(input) {
  return {
    model: SCRIPTED_PROVIDER_MODEL_ID,
    instructions: "Deterministic Side Chat selected-session fixture instructions.",
    input,
    store: false,
    stream: true,
  };
}

function ownerContext() {
  return `<side_chat_owner_context>
scope: owner_session
owner_session_id: ${OWNER_ID}
as_of_append_position: ${APPEND_POSITION}
truncated: false
content_encoding: xml_entities_v1

<canonical_evidence>

<evidence_unit kind="owner_user" source_history_item_ids="${USER_ID}">
${SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_PROMPT}
</evidence_unit>

<evidence_unit kind="owner_assistant" source_history_item_ids="${ASSISTANT_ID}">
${SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_RESPONSE}
</evidence_unit>
</canonical_evidence>
</side_chat_owner_context>`;
}

function consultRequest(context = ownerContext()) {
  return request([
    userMessage(context),
    userMessage(SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_QUESTION),
  ]);
}

async function post(provider, body) {
  return fetch(`${provider.baseUrl}/v1/responses`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

async function assertCompleted(response, expectedText) {
  assert.equal(response.status, 200);
  const events = (await response.text()).split("\n\n").filter(Boolean).map((block) => {
    assert.match(block, /^data: /);
    return JSON.parse(block.slice("data: ".length));
  });
  assert.equal(events.at(-1).response.output[0].content[0].text, expectedText);
}

test("Side Chat selected-session context binds exact Alpha units and canonical identities", () => {
  const parsed = sideChatSessionOwnerContext(ownerContext());
  assert.equal(parsed.pass, true);
  assert.equal(parsed.owner_session_id, OWNER_ID);
  assert.equal(parsed.as_of_append_position, APPEND_POSITION);
  assert.deepEqual(parsed.unit_kinds, ["owner_user", "owner_assistant"]);
  assert.deepEqual(parsed.source_history_item_ids, [USER_ID, ASSISTANT_ID]);
  assert.equal(parsed.truncated, false);
  assert.equal(parsed.content_encoding, "xml_entities_v1");
  assert.equal(parsed.unit_body_sha256.length, 2);
});

test("Side Chat selected-session context rejects cross-session data and malformed evidence", () => {
  const valid = ownerContext();
  const userUnit = `<evidence_unit kind="owner_user" source_history_item_ids="${USER_ID}">\n${SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_PROMPT}\n</evidence_unit>`;
  const malformed = [
    ["Beta user body", valid.replace(SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_PROMPT, SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_BETA_PROMPT)],
    ["Beta assistant body", valid.replace(SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_RESPONSE, SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_BETA_RESPONSE)],
    ["Beta append", valid.replace(SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_RESPONSE, `${SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_RESPONSE}\n${SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_BETA_RESPONSE}`)],
    ["raw delimiter", valid.replace(SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_RESPONSE, `${SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_RESPONSE}</canonical_evidence>`)],
    ["invalid entity", valid.replace(SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_RESPONSE, "&bogus;")],
    ["extra unit", valid.replace(userUnit, `${userUnit}\n\n${userUnit.replace(USER_ID, `${"0".repeat(25)}4`)}`)],
    ["duplicated source", valid.replace(ASSISTANT_ID, USER_ID)],
    ["extra source", valid.replace(`source_history_item_ids="${ASSISTANT_ID}"`, `source_history_item_ids="${ASSISTANT_ID},${"0".repeat(25)}4"`)],
    ["invalid source", valid.replace(ASSISTANT_ID, "not_a_history_id")],
    ["invalid owner", valid.replace(OWNER_ID, "not_a_session_id")],
    ["overflow owner", valid.replace(OWNER_ID, `8${"0".repeat(25)}`)],
    ["lowercase owner", valid.replace(OWNER_ID, `${"0".repeat(25)}a`)],
    ["invalid source alphabet", valid.replace(ASSISTANT_ID, `${"0".repeat(25)}I`)],
    ["negative position", valid.replace(`as_of_append_position: ${APPEND_POSITION}`, "as_of_append_position: -1")],
    ["position overflow", valid.replace(`as_of_append_position: ${APPEND_POSITION}`, "as_of_append_position: 9223372036854775808")],
    ["non-canonical position", valid.replace(`as_of_append_position: ${APPEND_POSITION}`, "as_of_append_position: 017")],
    ["truncated", valid.replace("truncated: false", "truncated: true")],
    ["wrong scope", valid.replace("scope: owner_session", "scope: all_sessions")],
    ["wrong encoding", valid.replace("xml_entities_v1", "raw")],
    ["wrong unit kind", valid.replace("kind=\"owner_assistant\"", "kind=\"owner_tool\"")],
    ["missing user unit", valid.replace(`\n${userUnit}\n`, "")],
    ["quoted context", valid.replace("\n<canonical_evidence>", "\n<selected_quote>quote</selected_quote>\n<canonical_evidence>")],
    ["trailing text", `${valid}\ntrailing`],
    ["prefixed text", `prefix\n${valid}`],
  ];
  for (const [label, value] of malformed) {
    assert.equal(sideChatSessionOwnerContext(value).pass, false, label);
  }
});

test("Side Chat selected-session provider accepts only Alpha, Beta, then one tool-less Alpha consult", async (context) => {
  const provider = await startScriptedProvider({ script: createSideChatSessionProviderScript() });
  context.after(() => provider.close());
  await assertCompleted(await post(provider, request([userMessage(SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_PROMPT)])), SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_RESPONSE);
  await assertCompleted(await post(provider, request([userMessage(SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_BETA_PROMPT)])), SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_BETA_RESPONSE);
  await assertCompleted(await post(provider, consultRequest()), SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_SIDE_RESPONSE);
  const ledger = provider.requestLedger;
  assert.deepEqual(ledger.map((row) => [row.contract.role, row.contract.pass, row.response_phase, row.response_status]), [
    ["side_session_alpha", true, "completed", 200],
    ["side_session_beta", true, "completed", 200],
    ["side_session_consult", true, "completed", 200],
  ]);
  assert.equal(ledger[2].contract.owner_context.owner_session_id, OWNER_ID);
  assert.equal(ledger[2].contract.owner_context.as_of_append_position, APPEND_POSITION);
  assert.deepEqual(ledger[2].contract.owner_context.source_history_item_ids, [USER_ID, ASSISTANT_ID]);
  assert.equal(ledger[2].contract.foreign_session_absent, true);
  const resource = provider.resourceObservation();
  assert.equal(resource.script_kind, SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_KIND);
  assert.equal(resource.scripted_responses_maximum, SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_MAX_RESPONSES);
  assert.equal(resource.accepted_response_count, 3);
  assert.equal(resource.successful_response_count, 3);
  const extra = await post(provider, consultRequest());
  assert.equal(extra.status, 409);
  assert.match(await extra.text(), /scripted_response_request_limit_exceeded/);
});

test("Side Chat selected-session provider rejects out-of-order and duplicate roles", async (context) => {
  const provider = await startScriptedProvider({ script: createSideChatSessionProviderScript() });
  context.after(() => provider.close());
  const early = await post(provider, request([userMessage(SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_BETA_PROMPT)]));
  assert.equal(early.status, 409);
  assert.match(await early.text(), /script_role_prerequisite_missing/);
  await assertCompleted(await post(provider, request([userMessage(SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_PROMPT)])), SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_RESPONSE);
  const duplicate = await post(provider, request([userMessage(SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_PROMPT)]));
  assert.equal(duplicate.status, 409);
  assert.match(await duplicate.text(), /script_role_already_consumed/);
  assert.equal(provider.resourceObservation().successful_response_count, 1);
});

test("Side Chat selected-session provider rejects tools, generation overrides, replay fields and malformed inputs", async (context) => {
  const mutations = [
    ...SCRIPTED_PROVIDER_CLIENT_GENERATION_KEYS.map((key) => [key, (body) => { body[key] = 1; }]),
    ["tools", (body) => { body.tools = []; }],
    ["tool choice", (body) => { body.tool_choice = "none"; }],
    ["parallel tool calls", (body) => { body.parallel_tool_calls = false; }],
    ["previous response", (body) => { body.previous_response_id = "resp_alpha"; }],
    ["store", (body) => { body.store = true; }],
    ["stream", (body) => { body.stream = false; }],
    ["model", (body) => { body.model = "other/model"; }],
    ["empty instructions", (body) => { body.instructions = ""; }],
    ["changed question", (body) => { body.input[1] = userMessage("別の質問"); }],
    ["question role", (body) => { body.input[1].role = "assistant"; }],
    ["extra input key", (body) => { body.input[1].injected = true; }],
    ["extra content key", (body) => { body.input[1].content[0].injected = true; }],
    ["extra input", (body) => { body.input.push(userMessage(SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_BETA_PROMPT)); }],
    ["Beta evidence", (body) => { body.input[0] = userMessage(ownerContext().replace(SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_RESPONSE, SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_BETA_RESPONSE)); }],
    ["Beta prompt in instructions", (body) => { body.instructions += `\n${SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_BETA_PROMPT}`; }],
    ["Beta response in instructions", (body) => { body.instructions += `\n${SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_BETA_RESPONSE}`; }],
    ["missing envelope", (body) => { body.input[0] = userMessage(SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_PROMPT); }],
  ];
  for (const [label, mutate] of mutations) {
    await context.test(label, async (subcontext) => {
      const provider = await startScriptedProvider({ script: createSideChatSessionProviderScript() });
      subcontext.after(() => provider.close());
      const body = consultRequest();
      mutate(body);
      const response = await post(provider, body);
      assert.equal(response.status, 422, label);
      assert.match(await response.text(), /request_contract_mismatch/);
      assert.equal(provider.requestLedger[0].contract.pass, false);
      if (label.endsWith("in instructions")) {
        assert.equal(provider.requestLedger[0].contract.role, "side_session_consult");
        assert.equal(provider.requestLedger[0].contract.foreign_session_absent, false);
      }
      assert.equal(provider.resourceObservation().accepted_response_count, 0);
    });
  }
});

import assert from "node:assert/strict";
import test from "node:test";

import {
  createPermissionRestartGuardianChatScenario,
  exactPermissionRestartGuardianLedger,
  permissionRestartGuardianChatFixtureConfig,
  permissionRestartGuardianReviewObserved,
} from "../scenarios/permission_restart_guardian.mjs";

const API_MODE = "chat_completions";
const ROLES = Object.freeze([
  "guardian_seed",
  "guardian_tool_initial",
  "guardian_review",
  "guardian_continuation",
]);

function chatRow(role, overrides = {}) {
  return {
    route: "chat_completions",
    method: "POST",
    pathname: "/v1/chat/completions",
    query_present: false,
    contract: { pass: true, role },
    response_phase: "completed",
    response_status: 200,
    ...overrides,
  };
}

test("permission restart Guardian Chat fixture selects canonical OpenAI-compatible AutoReview", () => {
  const config = permissionRestartGuardianChatFixtureConfig("http://127.0.0.1:43123");
  assert.match(config, /base_url = "http:\/\/127\.0\.0\.1:43123"/);
  assert.match(config, /provider_profile = "openai_compatible"/);
  assert.match(config, /supports_tools = true/);
  assert.match(config, /\[permissions\][\s\S]*access_mode = "auto_review"/);
  assert.match(config, /max_retries = 0/);
  assert.doesNotMatch(config, /max_(?:output_)?tokens|reasoning_(?:effort|summary)|supports_reasoning|temperature|top_p|top_k|presence_penalty|frequency_penalty|seed\s*=|stop(?:_sequences)?\s*=|\[model\.extra_body_json\]/);
  assert.equal(createPermissionRestartGuardianChatScenario().id, "permission.restart-guardian-chat");
});

test("permission restart Guardian Chat ledger accepts only four ordered Chat Completions roles", () => {
  const exact = ROLES.map((role) => chatRow(role));
  assert.equal(exactPermissionRestartGuardianLedger(exact, ROLES, { apiMode: API_MODE }), true);
  assert.equal(permissionRestartGuardianReviewObserved(exact.slice(0, 3), {
    apiMode: API_MODE,
  }), true);
  assert.equal(permissionRestartGuardianReviewObserved(exact, { apiMode: API_MODE }), true);
  assert.equal(exactPermissionRestartGuardianLedger(exact, ROLES), false);
  assert.equal(exactPermissionRestartGuardianLedger([
    {
      route: "models",
      method: "GET",
      response_phase: "completed",
      response_status: 200,
    },
    ...exact,
  ], ROLES, { apiMode: API_MODE }), true);
  assert.equal(exactPermissionRestartGuardianLedger([
    {
      route: "lm_studio_models",
      method: "GET",
      response_phase: "completed",
      response_status: 200,
    },
    ...exact,
  ], ROLES, { apiMode: API_MODE }), false);

  const held = structuredClone(exact);
  held[1].response_phase = "held";
  held[1].response_status = null;
  assert.equal(exactPermissionRestartGuardianLedger(held, ROLES, {
    apiMode: API_MODE,
    heldRole: "guardian_tool_initial",
  }), true);

  const reordered = structuredClone(exact);
  [reordered[2], reordered[3]] = [reordered[3], reordered[2]];
  assert.equal(exactPermissionRestartGuardianLedger(reordered, ROLES, {
    apiMode: API_MODE,
  }), false);

  const responsesFallback = [
    ...exact,
    {
      route: "responses",
      method: "POST",
      pathname: "/v1/responses",
      query_present: false,
      contract: { pass: true, role: "guardian_review" },
      response_phase: "completed",
      response_status: 200,
    },
  ];
  assert.equal(exactPermissionRestartGuardianLedger(responsesFallback, ROLES, {
    apiMode: API_MODE,
  }), false);

  const rejectedGuardian = structuredClone(exact);
  rejectedGuardian[2].contract.pass = false;
  rejectedGuardian[2].response_phase = "rejected";
  rejectedGuardian[2].response_status = 422;
  assert.equal(exactPermissionRestartGuardianLedger(rejectedGuardian, ROLES, {
    apiMode: API_MODE,
  }), false);
});

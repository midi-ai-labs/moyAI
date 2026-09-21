import assert from "node:assert/strict";
import test from "node:test";
import { onboardingImplementationReply, INPUT_NAME, SCRIPT_NAME, RESULT_NAME } from "../fixtures/onboarding_implementation.mjs";
import { createOnboardingWinAbScenario, implementedArtifactsAccepted, approvalVisibleBeforeScroll, latestArtifactVersion, completedRequestorIdentityAccepted } from "../scenarios/onboarding_winab.mjs";

test("completed requestor identity comes from the matching status summary, not the detail DTO", () => {
  const expected = { job_id: "job-a", user_id: "actor-a", display_name: "WinA (操作PC)" };
  const requestor = { user_id: expected.user_id, display_name: expected.display_name };
  const detail = { id: expected.job_id, state: "succeeded" };
  const job = { ...detail, requestor };
  const shared = { detail, status: { jobs: [job] } };
  assert.equal(completedRequestorIdentityAccepted(shared, expected), true);
  for (const changed of [
    { ...job, id: "another-job" },
    { ...job, state: "running" },
    { ...job, requestor: { ...requestor, user_id: "another-actor" } },
    { ...job, requestor: { ...requestor, display_name: "Previous PC name" } },
  ]) assert.equal(completedRequestorIdentityAccepted({ ...shared, status: { jobs: [changed] } }, expected), false);
  assert.equal(completedRequestorIdentityAccepted({ detail, status: null }, expected), false);
  assert.equal(completedRequestorIdentityAccepted({ ...shared, detail: { ...detail, id: "another-job" } }, expected), false);
  assert.equal(completedRequestorIdentityAccepted({ detail: { ...detail, requestor }, status: { jobs: [] } }, expected), false);
});
test("live onboarding requires an explicit credential-free endpoint and model before starting any resource", () => {
  const liveProvider = { provider_base_url: "http://localhost:8119/v1", model: "test-model" };
  assert.equal(createOnboardingWinAbScenario({ liveProvider }).id, "onboarding.win-a-to-win-b");
  assert.equal(createOnboardingWinAbScenario({ liveProvider, expectProviderFailure: true }).id, "onboarding.win-a-to-win-b");
  assert.throws(() => createOnboardingWinAbScenario({ expectProviderFailure: true }), TypeError);
  assert.throws(() => createOnboardingWinAbScenario({ liveProvider, expectProviderFailure: "true" }), TypeError);
  for (const patch of [{ provider_base_url: "http://secret:password@localhost:8119" }, { model: "" }, { model: "bad\nmodel" }, { unexpected: true }]) {
    assert.throws(() => createOnboardingWinAbScenario({ liveProvider: { ...liveProvider, ...patch } }), TypeError);
  }
});
test("implementation plan executes its created script and reads actual output before answering", () => {
  const messages = [{ role: "user", content: `.moyai-shared-inputs-example/${INPUT_NAME}` }];
  const next = (id, content) => { if (id) messages.push({ role: "tool", tool_call_id: id, content }); return onboardingImplementationReply(messages); };
  assert.equal(next().delta.tool_calls[0].function.name, "read");
  const patch = next("implementation-read-input", "value\n10\n20\n30").delta.tool_calls[0];
  assert.match(JSON.parse(patch.function.arguments).patch_text, /Import-Csv/);
  assert.equal(next("implementation-script", "success").delta.tool_calls[0].function.name, "shared_publish_artifact");
  assert.equal(next("implementation-publish-script-first", "Saved shared artifact").delta.tool_calls[0].function.name, "apply_patch");
  assert.equal(next("implementation-script-final", "success").delta.tool_calls[0].function.name, "shared_publish_artifact");
  const shell = next("implementation-publish-script-final", "Saved shared artifact").delta.tool_calls[0];
  assert.equal(shell.function.name, "shell"); assert.match(JSON.parse(shell.function.arguments).command, /-File .\/Summarize-Numbers.ps1/);
  assert.equal(next("implementation-execute", "success").delta.tool_calls[0].function.name, "read");
  assert.equal(next("implementation-read-result", "Count: 3\nSum: 60").delta.tool_calls[0].function.name, "shared_publish_artifact");
  assert.equal(next("implementation-publish-result", "Saved shared artifact onboarding-result.md").finish, "stop");
  assert.throws(() => onboardingImplementationReply([...messages.slice(0, -2), { role: "tool", tool_call_id: "implementation-read-result", content: "error" }]), /real script/);
  assert.throws(() => onboardingImplementationReply([...messages.slice(0, -1), { role: "tool", tool_call_id: "implementation-publish-result", content: "error" }]), /not published/);
});
test("the GUI saves the newest published artifact rather than the first matching name", () => {
  const first = { id: "old", kind: "artifact", name: SCRIPT_NAME, version: 1 };
  const latest = { id: "new", kind: "artifact", name: SCRIPT_NAME, version: 2 };
  const input = { id: "input", kind: "input", name: SCRIPT_NAME, version: 9 };
  const unrelated = { id: "other", kind: "artifact", name: RESULT_NAME, version: 8 };
  const assets = [first, input, latest, unrelated];
  assert.equal(latestArtifactVersion(assets, SCRIPT_NAME), latest);
  assert.equal(latestArtifactVersion([...assets].reverse(), SCRIPT_NAME), latest);
  assert.deepEqual(assets, [first, input, latest, unrelated]);
});
test("artifact oracle requires both Hub and B execution hashes to match A's saved files", () => {
  const files = [SCRIPT_NAME, RESULT_NAME].map(name => ({ name, hub_sha256: "a".repeat(64), saved_sha256: "a".repeat(64), execution_sha256: "a".repeat(64), text: "Count: 3\r\nSum: 60\r\n" }));
  assert.equal(implementedArtifactsAccepted(files), true);
  assert.equal(implementedArtifactsAccepted(files.slice(1)), false);
  assert.equal(implementedArtifactsAccepted(files.map((file, index) => index ? file : { ...file, execution_sha256: "b".repeat(64) })), false);
});
test("approval discovery requires the enabled action to be reachable before scrolling", () => {
  const visible = { count: 1, visible: true, enabled: true, center_in_viewport: true, center_in_scroll_clip: true, center_hit: true };
  assert.equal(approvalVisibleBeforeScroll(visible), true);
  for (const patch of [{ count: 0 }, { count: 2 }, { visible: false }, { enabled: false }, { center_in_viewport: false }, { center_in_scroll_clip: false }, { center_hit: false }]) assert.equal(approvalVisibleBeforeScroll({ ...visible, ...patch }), false);
});

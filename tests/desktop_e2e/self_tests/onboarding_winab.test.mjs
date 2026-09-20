import assert from "node:assert/strict";
import test from "node:test";
import { onboardingImplementationReply, INPUT_NAME, SCRIPT_NAME, RESULT_NAME } from "../fixtures/onboarding_implementation.mjs";
import { implementedArtifactsAccepted, approvalVisibleBeforeScroll } from "../scenarios/onboarding_winab.mjs";
test("implementation plan executes its created script and reads actual output before answering", () => {
  const messages = [{ role: "user", content: `.moyai-shared-inputs-example/${INPUT_NAME}` }];
  const next = (id, content) => { if (id) messages.push({ role: "tool", tool_call_id: id, content }); return onboardingImplementationReply(messages); };
  assert.equal(next().delta.tool_calls[0].function.name, "read");
  const patch = next("implementation-read-input", "value\n10\n20\n30").delta.tool_calls[0];
  assert.match(JSON.parse(patch.function.arguments).patch_text, /Import-Csv/);
  const shell = next("implementation-script", "success").delta.tool_calls[0];
  assert.equal(shell.function.name, "shell"); assert.match(JSON.parse(shell.function.arguments).command, /-File .\/Summarize-Numbers.ps1/);
  assert.equal(next("implementation-execute", "success").delta.tool_calls[0].function.name, "read");
  assert.equal(next("implementation-read-result", "Count: 3\nSum: 60").delta.tool_calls[0].function.name, "shared_publish_artifact");
  assert.equal(next("implementation-publish-result", "Saved shared artifact onboarding-result.md").finish, "stop");
  assert.throws(() => onboardingImplementationReply([...messages.slice(0, -2), { role: "tool", tool_call_id: "implementation-read-result", content: "error" }]), /real script/);
  assert.throws(() => onboardingImplementationReply([...messages.slice(0, -1), { role: "tool", tool_call_id: "implementation-publish-result", content: "error" }]), /not published/);
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

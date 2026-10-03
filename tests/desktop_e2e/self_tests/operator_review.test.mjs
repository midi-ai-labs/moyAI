import assert from "node:assert/strict";
import test from "node:test";
import { PassThrough } from "node:stream";
import { operatorRequestFingerprint, validateOperatorDecision, waitForOperatorReview } from "../drivers/operator_review.mjs";

const REQUEST = { confirmation_id: "42", request: { summary: "Run workspace tests", details: ["python -m unittest"] }, run_target: { sessionId: "session-a" } };
const decision = (value = "approve", request = REQUEST) => ({ confirmation_id: request.confirmation_id,
  request_sha256: operatorRequestFingerprint(request), decision: value });

test("operator decisions bind one request identity and its full fingerprint without permission rules", () => {
  for (const value of ["approve", "stop", "deny"]) assert.deepEqual(validateOperatorDecision(decision(value), REQUEST), decision(value));
  for (const value of [null, { ...decision(), confirmation_id: "43" }, { ...decision(), request_sha256: "0".repeat(64) },
    { ...decision(), decision: "always-approve" }, { ...decision(), rule: "allow" }]) assert.throws(() => validateOperatorDecision(value, REQUEST));
  assert.throws(() => validateOperatorDecision(decision(), { ...REQUEST, request: { summary: "Different command" } }));
});

function channels() {
  const input = new PassThrough();
  const output = new PassThrough();
  let text = "";
  output.on("data", bytes => { text += bytes.toString("utf8"); });
  const listeners = Object.fromEntries(["data", "end", "error", "close"].map(name => [name, input.listenerCount(name)]));
  return { input, output, text: () => text, listeners };
}

function released(channel) {
  for (const [name, count] of Object.entries(channel.listeners)) assert.equal(channel.input.listenerCount(name), count, name);
  assert.equal(channel.input.isPaused(), true);
}

test("operator stdin rejects stale or invalid lines and accepts exactly the reviewed request", async () => {
  const channel = channels();
  channel.input.pause();
  const pending = waitForOperatorReview(REQUEST, { ...channel, timeoutMs: 1000 });
  const notice = JSON.parse(channel.text().trim());
  assert.equal(notice.type, "operator-review-request");
  assert.equal(notice.request_sha256, operatorRequestFingerprint(REQUEST));
  channel.input.write("not JSON\n");
  channel.input.write(`${JSON.stringify({ ...decision(), confirmation_id: "43" })}\n`);
  channel.input.write(`${JSON.stringify(decision())}\n${JSON.stringify(decision("deny"))}\n`);
  assert.deepEqual(await pending, { status: "decided", ...decision() });
  assert.equal(channel.text().split("\n").filter(row => row.includes("operator-review-rejected")).length, 2);
  released(channel);
  channel.input.destroy(); channel.output.destroy();
});

test("operator EOF and timeout return incomplete and release every stdin listener", async () => {
  for (const reason of ["stdin-closed", "operator-timeout"]) {
    const channel = channels();
    channel.input.pause();
    const pending = waitForOperatorReview(REQUEST, { ...channel, timeoutMs: 15 });
    if (reason === "stdin-closed") channel.input.end();
    assert.deepEqual(await pending, { status: "not_decided", reason });
    released(channel);
    channel.input.destroy(); channel.output.destroy();
  }
  await assert.rejects(waitForOperatorReview(REQUEST, { timeoutMs: 300001 }), TypeError);
});

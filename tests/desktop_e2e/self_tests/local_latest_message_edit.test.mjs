import test from "node:test";
import assert from "node:assert/strict";
import vm from "node:vm";
import { latestEditableLocalMessage, waitForImageAttachmentControls } from "../scenarios/local_latest_message_edit.mjs";

test("local edit identifies the latest canonical user message including its image display metadata", () => {
  const prompt = "edit this request";
  const original = { row_kind: "user", body: `${prompt}\nC:/workspace/original.png (68 bytes)`, stable_history_identity: "original-item" };
  const projection = { run_target: { sessionId: "source-session" }, transcript_rows: [original,
    { row_kind: "assistant", body: "finished" }] };
  assert.equal(latestEditableLocalMessage(projection, prompt), original);
  assert.equal(latestEditableLocalMessage({ ...projection, transcript_rows: [{ ...original, body: prompt }] }, prompt)?.stable_history_identity, "original-item");
  for (const changed of [
    { ...projection, run_target: {} },
    { ...projection, transcript_rows: [{ ...original, stable_history_identity: "" }] },
    { ...projection, transcript_rows: [{ ...original, body: "a different request" }] },
    { ...projection, transcript_rows: [{ ...original, body: `${prompt} with different text` }] },
    { ...projection, transcript_rows: [...projection.transcript_rows, { ...original, body: "newer user message" }] },
  ]) assert.equal(latestEditableLocalMessage(changed, prompt), null);
});

test("local image edit waits for the inserted attachment field without changing keyboard focus", async () => {
  let samples = 0;
  const unavailable = { disabled: true, closest: () => null };
  const ready = { disabled: false, closest: () => null };
  const observations = [[], [unavailable], [ready]];
  const document = {
    querySelectorAll: () => observations[Math.min(samples++, observations.length - 1)],
    get activeElement() { throw new Error("the render wait must not change or traverse focus"); },
  };
  const cdp = { evaluate: async source => vm.runInNewContext(source, { document }) };
  await waitForImageAttachmentControls(cdp);
  assert.equal(samples, 3);
});

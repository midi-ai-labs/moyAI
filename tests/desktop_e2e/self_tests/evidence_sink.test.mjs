import assert from "node:assert/strict";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { mkdtemp, readFile, rm } from "node:fs/promises";

import { EvidenceSink } from "../core/evidence_sink.mjs";

test("evidence sink writes create-new events and one deterministic seal", async (context) => {
  const parent = await mkdtemp(path.join(os.tmpdir(), "moyai-e2e-evidence-"));
  context.after(() => rm(parent, { recursive: true, force: true }));
  const sink = await EvidenceSink.create(path.join(parent, "evidence"));
  await sink.record("preflight-pass", { value: 1 }, { phase: "preflight", at: "2026-08-22T00:00:00.000Z" });
  await sink.record("attached", { target: "main" }, { phase: "attached", at: "2026-08-22T00:00:01.000Z" });
  const seal = await sink.seal({ classification: "pass" });
  assert.equal(seal.event_count, 2);
  assert.match(seal.result_sha256, /^[a-f0-9]{64}$/);
  assert.match(seal.evidence_tree_sha256, /^[a-f0-9]{64}$/);
  assert.equal(sink.sealed, true);
  assert.equal(JSON.parse(await readFile(path.join(parent, "evidence", "result.json"), "utf8")).classification, "pass");
  await assert.rejects(() => sink.record("late-event", {}), /already sealed/);
  await assert.rejects(() => sink.seal({ classification: "pass" }), /already sealed/);
});

test("evidence sink rejects overwrite and path escape", async (context) => {
  const parent = await mkdtemp(path.join(os.tmpdir(), "moyai-e2e-evidence-"));
  context.after(() => rm(parent, { recursive: true, force: true }));
  const sink = await EvidenceSink.create(path.join(parent, "evidence"));
  await sink.writeJson("custom/value.json", { first: true });
  await assert.rejects(() => sink.writeJson("custom/value.json", { second: true }), /EEXIST/);
  await assert.rejects(() => sink.writeJson("../escape.json", {}), /unsafe evidence path/);
  await assert.rejects(() => sink.writeJson(path.resolve(parent, "escape.json"), {}), /absolute evidence path/);
  await assert.rejects(() => sink.writeJson("custom/value.json:stream", {}), /unsafe Windows evidence path/);
  await assert.rejects(() => sink.writeJson("custom/CON.json", {}), /reserved Windows evidence path/);
  await assert.rejects(() => sink.writeJson("custom/trailing. ", {}), /unsafe Windows evidence path/);
});

test("failed event writes do not advance the event sequence", async (context) => {
  const parent = await mkdtemp(path.join(os.tmpdir(), "moyai-e2e-evidence-"));
  context.after(() => rm(parent, { recursive: true, force: true }));
  const sink = await EvidenceSink.create(path.join(parent, "evidence"));
  await sink.writeJson("events/000001-collision.json", { reserved: true });
  await assert.rejects(() => sink.record("collision", {}), /EEXIST/);
  assert.equal(sink.eventCount, 0);
  const recorded = await sink.record("next", {});
  assert.equal(recorded.event.sequence, 1);
  assert.equal(recorded.identity.relative_path, "events/000001-next.json");
});

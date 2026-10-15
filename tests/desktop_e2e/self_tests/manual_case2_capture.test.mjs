import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { mkdir, mkdtemp, rm, unlink, utimes, writeFile } from "node:fs/promises";
import { manualCase2CaptureSnapshot, manualCase2PreparedRequests } from "../scenarios/manual_case2.mjs";

const parent = fileURLToPath(new URL("../../../../project_sandbox/manual-case2-capture-self-tests/", import.meta.url));
const sink = { writeJson: async () => {} };

async function fixture(run) {
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(path.join(parent, "captures-"));
  try { await run(root); }
  finally {
    assert.equal(path.dirname(path.resolve(root)).toLowerCase(), path.resolve(parent).toLowerCase());
    await rm(root, { recursive: true });
  }
}

async function pair(root, id, body) {
  const bytes = Buffer.from(JSON.stringify(body));
  const bodyName = `${id}.request.json`;
  await writeFile(path.join(root, bodyName), bytes, { flag: "wx" });
  await writeFile(path.join(root, `${id}.metadata.json`), JSON.stringify({ schema_version: 2, capture_stage: "prepared",
    transport: "http", request_id: id, request_body_file: bodyName, request_body_bytes: bytes.length,
    api_mode: "responses", endpoint_path: "/responses" }), { flag: "wx" });
}

test("Case2 ignores earlier captures and attributes only new pairs without timestamp ownership", async () => {
  await fixture(async root => {
    // Earlier traffic need not be a Case2-shaped body; it is hashed, not reinterpreted.
    await writeFile(path.join(root, "earlier.metadata.json"), "not Case2 metadata");
    await writeFile(path.join(root, "earlier.request.json"), "not JSON");
    const baseline = await manualCase2CaptureSnapshot(root);
    const image = Buffer.from("new image");
    await pair(root, "case2", { model: "new-model", input: [{ role: "user", content: [
      { type: "input_image", image_url: `data:image/png;base64,${image.toString("base64")}` },
    ] }] });
    await utimes(path.join(root, "case2.metadata.json"), new Date(0), new Date(0));
    const result = await manualCase2PreparedRequests(root, sink, baseline);
    assert.deepEqual(result.requests.map(row => row.request_id), ["case2"]);
    assert.equal(result.requests[0].image_count, 1);
    assert.deepEqual(result.capture_scope.baseline, baseline);
    assert.equal(result.capture_scope.baseline_unchanged, true);
    assert.deepEqual(result.capture_scope.new_files.map(row => row.filename), ["case2.metadata.json", "case2.request.json"]);
  });
});

test("Case2 rejects an earlier capture digest mutation even when its size and timestamp are unchanged", async () => {
  await fixture(async root => {
    const file = path.join(root, "earlier.request.json");
    const time = new Date(0);
    await writeFile(file, "aaaa"); await utimes(file, time, time);
    const baseline = await manualCase2CaptureSnapshot(root);
    await writeFile(file, "bbbb"); await utimes(file, time, time);
    await assert.rejects(manualCase2PreparedRequests(root, sink, baseline), error => error.owner === "harness" && error.code === "case2-capture-baseline-changed");
  });
});

test("Case2 rejects deletion of an earlier capture", async () => {
  await fixture(async root => {
    await pair(root, "earlier", { model: "old-model", input: [] });
    const baseline = await manualCase2CaptureSnapshot(root);
    await unlink(path.join(root, "earlier.request.json"));
    await assert.rejects(manualCase2PreparedRequests(root, sink, baseline), error => error.code === "case2-capture-baseline-changed");
  });
});

test("Case2 rejects new metadata claiming a body that existed before case entry", async () => {
  await fixture(async root => {
    const body = Buffer.from(JSON.stringify({ model: "old-model", input: [] }));
    await writeFile(path.join(root, "earlier.request.json"), body);
    const baseline = await manualCase2CaptureSnapshot(root);
    await writeFile(path.join(root, "earlier.metadata.json"), JSON.stringify({ schema_version: 2,
      capture_stage: "prepared", transport: "http", request_id: "earlier", request_body_file: "earlier.request.json",
      request_body_bytes: body.length }));
    await assert.rejects(manualCase2PreparedRequests(root, sink, baseline), error => error.code === "case2-capture-owner");
  });
});

test("standalone Case2 still starts with an empty capture baseline and reads its current request", async () => {
  await fixture(async root => {
    const baseline = await manualCase2CaptureSnapshot(root, { requireEmpty: true });
    assert.deepEqual(baseline.files, []);
    await pair(root, "standalone", { model: "single", input: [] });
    assert.deepEqual((await manualCase2PreparedRequests(root, sink, baseline)).requests.map(row => row.request_id), ["standalone"]);
    await assert.rejects(manualCase2CaptureSnapshot(root, { requireEmpty: true }), error => error.code === "case2-capture-not-fresh");
  });
});

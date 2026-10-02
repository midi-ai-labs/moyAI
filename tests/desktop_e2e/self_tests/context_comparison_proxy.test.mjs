import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { gzipSync } from "node:zlib";
import test from "node:test";
import { comparisonUpstream, startContextComparisonProxy, transformComparisonRequest } from "../drivers/context_comparison_proxy.mjs";

const context = '<shared_project_context source="hub" kind="descriptive">project and roles</shared_project_context>';
const request = () => ({ model: "test-model", stream: true, temperature: 0.3,
  messages: [{ role: "system", content: `M1 instruction\n${context}\npermission boundary` },
    { role: "user", content: `Goal; literal user text ${context}` }, { role: "assistant", content: "prior answer" },
    { role: "tool", tool_call_id: "prior", content: "actual observation" }], tools: [{ type: "function", function: { name: "read", parameters: {} } }] });
const hash = bytes => createHash("sha256").update(bytes).digest("hex");
const scratchParent = fileURLToPath(new URL("../../../../project_sandbox/desktop-e2e-context-comparison/", import.meta.url));

async function fixture(t, mode, respond) {
  await mkdir(scratchParent, { recursive: true });
  const root = await mkdtemp(path.join(scratchParent, "case-")), sockets = new Set(), received = [];
  const state = { proxy: null, expectedProxyFailure: false };
  const server = createServer(async (req, res) => {
    const chunks = []; for await (const chunk of req) chunks.push(chunk);
    const row = { method: req.method, url: req.url, authorization: req.headers.authorization, bytes: Buffer.concat(chunks) };
    received.push(row); await respond(row, res);
  });
  server.on("connection", socket => { sockets.add(socket); socket.once("close", () => sockets.delete(socket)); });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  const endpoint = `http://127.0.0.1:${server.address().port}`, secret = "fixture-only-upstream-secret";
  const credentialFile = path.join(root, "credential.json"), captureDirectory = path.join(root, "private-captures");
  await writeFile(credentialFile, JSON.stringify({ endpoint, provider_profile: "openai_compatible_chat", key: secret }));
  t.after(async () => {
    try {
      if (state.expectedProxyFailure) await state.proxy?.close().catch(() => {});
      else await state.proxy?.close();
    } finally {
      const stopped = new Promise(resolve => server.close(resolve));
      for (const socket of sockets) socket.destroy(); await stopped;
      const resolved = path.resolve(root), boundary = `${path.resolve(scratchParent)}${path.sep}`;
      assert.ok(resolved.startsWith(boundary));
      await rm(resolved, { recursive: true, force: true });
    }
  });
  state.proxy = await startContextComparisonProxy({ endpoint, model: "test-model", credentialFile, mode, captureDirectory });
  return { ...state, state, proxy: state.proxy, received, endpoint, secret, credentialFile, captureDirectory };
}
const post = (f, body = request(), signal) => fetch(`${f.proxy.baseUrl}/chat/completions`, {
  method: "POST", headers: { "content-type": "application/json", authorization: "Bearer local-header-must-not-forward" },
  body: typeof body === "string" ? body : JSON.stringify(body), signal,
});

test("A removes only one descriptive system/developer section and B preserves every field", () => {
  for (const role of ["system", "developer"]) for (const parts of [false, true]) {
    const input = request(); input.messages[0].role = role;
    if (parts) input.messages[0].content = [{ type: "text", text: input.messages[0].content }, { type: "text", text: "other instructions" }];
    const original = structuredClone(input), expected = structuredClone(input);
    if (parts) expected.messages[0].content[0].text = expected.messages[0].content[0].text.replace(context, "");
    else expected.messages[0].content = expected.messages[0].content.replace(context, "");
    assert.deepEqual(transformComparisonRequest(input, "remove").output, expected);
    assert.deepEqual(transformComparisonRequest(input, "preserve").output, original);
    assert.deepEqual(input, original);
  }
  const guardian = { model: "test-model", messages: [{ role: "system", content: "Judge one operation" }] };
  assert.deepEqual(transformComparisonRequest(guardian, "remove"), { output: guardian, sections: 0, changedPaths: [] });
  for (const text of [context + context, context.replace('kind="descriptive"', 'kind="unknown"'), context.replace("</shared_project_context>", "")]) {
    const input = request(); input.messages[0].content = text;
    assert.throws(() => transformComparisonRequest(input, "remove"), /boundaries/);
  }
});

test("credentials bind the exact endpoint and Chat Completions profile", () => {
  const saved = { endpoint: "http://127.0.0.1:8119/v1", provider_profile: "openai_compatible_chat", key: "fixture-secret" };
  assert.equal(comparisonUpstream(saved, "http://127.0.0.1:8119").baseUrl, "http://127.0.0.1:8119/v1");
  for (const endpoint of ["http://127.0.0.1:8120", "http://127.0.0.1:8119/other", "http://user:secret@127.0.0.1:8119", "http://127.0.0.1:8119?secret=x"])
    assert.throws(() => comparisonUpstream(saved, endpoint));
  for (const patch of [{ provider_profile: "openai_compatible_responses" }, { key: "" }, { key: "bad\r\nheader" }])
    assert.throws(() => comparisonUpstream({ ...saved, ...patch }, saved.endpoint));
});

for (const mode of ["remove", "preserve"]) test(`${mode} proxy captures private pre/post bodies and keeps discovery/model/auth routing exact`, async t => {
  const f = await fixture(t, mode, (row, res) => {
    res.setHeader("content-type", "application/json");
    res.end(row.url === "/v1/models" ? '{"data":[{"id":"test-model"}]}' : '{"choices":[{"message":{"content":"ok"}}]}');
  });
  assert.equal((await (await fetch(`${f.proxy.baseUrl}/models`)).json()).data[0].id, "test-model");
  const raw = JSON.stringify(request(), null, 2);
  const response = await post(f, raw); assert.equal(response.status, 200); await response.text();
  await f.proxy.close();
  assert.deepEqual(f.received.map(r => [r.method, r.url]), [["GET", "/v1/models"], ["POST", "/v1/chat/completions"]]);
  assert.ok(f.received.every(r => r.authorization === `Bearer ${f.secret}`));
  const expected = transformComparisonRequest(JSON.parse(raw), mode);
  assert.deepEqual(JSON.parse(f.received[1].bytes), expected.output);
  const pre = await readFile(path.join(f.captureDirectory, "000002.request.pre.json"));
  const after = await readFile(path.join(f.captureDirectory, "000002.request.post.json"));
  assert.equal(pre.toString(), raw); assert.deepEqual(after, f.received[1].bytes);
  if (mode === "preserve") assert.deepEqual(pre, after);
  const metadata = JSON.parse(await readFile(path.join(f.captureDirectory, "000002.metadata.json")));
  assert.equal(metadata.before_sha256, hash(pre)); assert.equal(metadata.after_sha256, hash(after));
  assert.deepEqual(metadata.changed_paths, expected.changedPaths);
  for (const name of await readdir(f.captureDirectory)) {
    const text = (await readFile(path.join(f.captureDirectory, name))).toString();
    assert.equal(text.includes(f.secret), false); assert.equal(text.includes("local-header-must-not-forward"), false);
  }
});

test("SSE chunks stream immediately and preserve split UTF-8, finish and DONE bytes", { timeout: 10000 }, async t => {
  let release; const later = new Promise(resolve => { release = resolve; });
  const first = Buffer.from('data: {"choices":[{"delta":{"content":"');
  const tail = Buffer.from('日本語"},"finish_reason":null}]}\n\ndata: {"choices":[{"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n');
  const f = await fixture(t, "remove", async (_row, res) => {
    res.writeHead(200, { "content-type": "text/event-stream" }); res.write(first);
    await later; res.write(tail.subarray(0, 1)); res.end(tail.subarray(1));
  });
  t.after(() => release());
  const response = await post(f), reader = response.body.getReader();
  const early = await reader.read(); assert.deepEqual(Buffer.from(early.value), first);
  release(); const chunks = [early.value];
  for (;;) { const item = await reader.read(); if (item.done) break; chunks.push(item.value); }
  assert.deepEqual(Buffer.concat(chunks), Buffer.concat([first, tail]));
  await f.proxy.close();
  const saved = await readFile(path.join(f.captureDirectory, "000001.response.bin"));
  assert.deepEqual(saved, Buffer.concat([first, tail]));
  const metadata = JSON.parse(await readFile(path.join(f.captureDirectory, "000001.metadata.json")));
  assert.equal(metadata.response_sha256, hash(saved)); assert.equal(metadata.response_bytes, saved.length);
});

test("large response captures match transferred bytes and compressed responses keep their encoding", async t => {
  const decoded = Buffer.from("data: multilingual 日本語 stream payload\n\n".repeat(16384)), encoded = gzipSync(decoded);
  const f = await fixture(t, "preserve", (row, res) => {
    const compressed = row.url === "/v1/chat/completions";
    res.writeHead(200, { "content-type": "text/event-stream", ...(compressed ? { "content-encoding": "gzip" } : {}) });
    res.end(compressed ? encoded : decoded);
  });
  const plain = await fetch(`${f.proxy.baseUrl}/models`);
  assert.deepEqual(Buffer.from(await plain.arrayBuffer()), decoded);
  const response = await post(f); assert.equal(response.headers.get("content-encoding"), "gzip");
  assert.deepEqual(Buffer.from(await response.arrayBuffer()), decoded);
  await f.proxy.close();
  for (const [index, expected] of [["000001", decoded], ["000002", encoded]]) {
    const saved = await readFile(path.join(f.captureDirectory, `${index}.response.bin`));
    const metadata = JSON.parse(await readFile(path.join(f.captureDirectory, `${index}.metadata.json`)));
    assert.deepEqual(saved, expected);
    assert.equal(metadata.response_sha256, hash(saved)); assert.equal(metadata.response_bytes, saved.length);
    assert.equal(metadata.content_type, "text/event-stream");
    assert.equal(metadata.content_encoding, index === "000002" ? "gzip" : null);
  }
});

test("non-success responses are forwarded once and redirects never forward credentials elsewhere", async t => {
  const f = await fixture(t, "preserve", (_row, res) => { res.writeHead(503, { "content-type": "application/json" }); res.end('{"error":"busy"}'); });
  const response = await post(f); assert.equal(response.status, 503); assert.equal(await response.text(), '{"error":"busy"}');
  assert.equal((await fetch(`${f.proxy.baseUrl}/other`)).status, 404);
  await f.proxy.close(); assert.equal(f.received.length, 1);
});

test("redirect status does not cause either proxy or client to follow Location", async t => {
  const f = await fixture(t, "preserve", (_row, res) => { res.writeHead(302, { location: "http://127.0.0.1:1/private" }); res.end("redirect body"); });
  const response = await post(f); assert.equal(response.status, 302); assert.equal(response.headers.get("location"), null);
  assert.equal(await response.text(), "redirect body"); await f.proxy.close(); assert.equal(f.received.length, 1);
});

for (const action of ["abort", "close"]) test(`${action} releases the pending upstream stream without retry`, { timeout: 10000 }, async t => {
  let upstreamClosed; const ended = new Promise(resolve => { upstreamClosed = resolve; });
  const f = await fixture(t, "preserve", (_row, res) => {
    res.once("close", upstreamClosed); res.writeHead(200, { "content-type": "text/event-stream" }); res.write("data: pending\n\n");
  });
  const controller = new AbortController(), response = await post(f, request(), controller.signal);
  const reader = response.body.getReader(); await reader.read();
  if (action === "abort") controller.abort(); else await f.proxy.close();
  await ended; await f.proxy.close(); await reader.cancel().catch(() => {});
  assert.equal(f.received.length, 1);
  const metadata = JSON.parse(await readFile(path.join(f.captureDirectory, "000001.metadata.json")));
  assert.equal(metadata.aborted, true); assert.equal(metadata.completed, false);
});

test("a mismatched model is rejected before upstream or private body capture", async t => {
  const f = await fixture(t, "remove", (_row, res) => res.end("unexpected"));
  f.state.expectedProxyFailure = true;
  assert.equal((await post(f, { ...request(), model: "wrong-model" })).status, 502);
  await assert.rejects(f.proxy.close(), /Comparison proxy failed/);
  assert.equal(f.received.length, 0);
  assert.equal((await readdir(f.captureDirectory)).some(name => name.endsWith("request.pre.json")), false);
});

test("an upstream stream failure remains a failed stream without fabricated completion or retry", { timeout: 10000 }, async t => {
  let interrupt; const interrupted = new Promise(resolve => { interrupt = resolve; });
  const f = await fixture(t, "preserve", async (_row, res) => {
    res.writeHead(200, { "content-type": "text/event-stream" }); res.write("data: first\n\n");
    await interrupted; res.destroy();
  });
  f.state.expectedProxyFailure = true; t.after(() => interrupt());
  const response = await post(f), reader = response.body.getReader();
  assert.equal(Buffer.from((await reader.read()).value).toString(), "data: first\n\n");
  interrupt(); await assert.rejects(reader.read());
  await assert.rejects(f.proxy.close(), /Comparison proxy failed/);
  assert.equal(f.received.length, 1);
  const metadata = JSON.parse(await readFile(path.join(f.captureDirectory, "000001.metadata.json")));
  assert.equal(metadata.completed, false); assert.equal(metadata.failure, "comparison-request-failed");
  assert.equal((await readFile(path.join(f.captureDirectory, "000001.response.bin"))).toString(), "data: first\n\n");
});

test("a discovery connection reset retains its transport code without retry or error text", { timeout: 10000 }, async t => {
  const f = await fixture(t, "preserve", (_row, res) => res.destroy());
  f.state.expectedProxyFailure = true;
  const response = await fetch(`${f.proxy.baseUrl}/models`);
  assert.equal(response.status, 502); await response.text();
  await assert.rejects(f.proxy.close(), /Comparison proxy failed/);
  assert.equal(f.received.length, 1);
  const metadata = JSON.parse(await readFile(path.join(f.captureDirectory, "000001.metadata.json")));
  assert.equal(metadata.status, null); assert.equal(metadata.completed, false);
  assert.equal(metadata.failure, "comparison-request-failed");
  assert.equal(metadata.error_code, "ECONNRESET");
  assert.equal("error_message" in metadata, false);
  assert.equal("headers" in metadata, false);
});

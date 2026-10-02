// Test-only upstream observation/ablation. No model-selected effect is simulated.
import { createHash } from "node:crypto";
import { once } from "node:events";
import { mkdir, open, readFile, writeFile } from "node:fs/promises";
import { Agent as HttpAgent, createServer, request as httpRequest } from "node:http";
import { Agent as HttpsAgent, request as httpsRequest } from "node:https";
import path from "node:path";

const digest = value => createHash("sha256").update(value).digest("hex");
const section = /<shared_project_context source="hub" kind="descriptive">[\s\S]*?<\/shared_project_context>/g;
const markers = /<\/?shared_project_context(?:\s|>)/g;

export function transformComparisonRequest(input, mode) {
  if (!["remove", "preserve"].includes(mode)) throw new TypeError("Unknown context comparison mode");
  if (!input || typeof input !== "object" || !Array.isArray(input.messages)) throw new TypeError("Expected a Chat Completions request");
  const output = structuredClone(input), changedPaths = [];
  let sections = 0, tags = 0;
  const inspect = (value, location) => {
    tags += [...value.matchAll(markers)].length;
    return value.replace(section, text => {
      sections++;
      if (mode === "remove") { changedPaths.push(location); return ""; }
      return text;
    });
  };
  output.messages.forEach((message, index) => {
    if (!["system", "developer"].includes(message.role)) return;
    if (typeof message.content === "string") message.content = inspect(message.content, `/messages/${index}/content`);
    else if (Array.isArray(message.content)) message.content.forEach((part, partIndex) => {
      if (typeof part.text === "string") part.text = inspect(part.text, `/messages/${index}/content/${partIndex}/text`);
    });
  });
  if (sections > 1 || tags !== sections * 2) throw new TypeError("Shared context has duplicate or malformed boundaries");
  return { output, sections, changedPaths };
}

function canonicalEndpoint(value) {
  const url = new URL(value);
  if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash)
    throw new TypeError("Upstream endpoint must be an explicit credential-free HTTP URL");
  return url.href.replace(/\/v1\/?$/, "").replace(/\/$/, "");
}

export function comparisonUpstream(credential, endpoint) {
  const base = canonicalEndpoint(endpoint);
  if (canonicalEndpoint(credential.endpoint) !== base || credential.provider_profile !== "openai_compatible_chat"
    || typeof credential.key !== "string" || !credential.key.trim() || /[\r\n]/.test(credential.key))
    throw new TypeError("Credential does not match the exact comparison endpoint and profile");
  return { baseUrl: `${base}/v1`, authorization: `Bearer ${credential.key}` };
}

export async function startContextComparisonProxy({ endpoint, model, credentialFile, mode, captureDirectory }) {
  if (!["remove", "preserve"].includes(mode) || typeof model !== "string" || !model)
    throw new TypeError("Comparison requires a mode and exact model");
  if (!path.isAbsolute(credentialFile ?? "") || !path.isAbsolute(captureDirectory ?? ""))
    throw new TypeError("Comparison credential and private capture paths must be absolute");
  // Read once, retain only in memory, and never put request headers in evidence.
  const upstream = comparisonUpstream(JSON.parse(await readFile(credentialFile, "utf8")), endpoint);
  const secure = new URL(upstream.baseUrl).protocol === "https:";
  // The product owns request/idle deadlines. Built-in fetch adds Undici's
  // independent five-minute header/body timeout, so use a deadline-free agent.
  const upstreamAgent = secure ? new HttpsAgent({ keepAlive: true, timeout: 0 }) : new HttpAgent({ keepAlive: true, timeout: 0 });
  const send = (route, method, body, signal) => new Promise((resolve, reject) => {
    const outgoing = (secure ? httpsRequest : httpRequest)(`${upstream.baseUrl}${route}`, { method, agent: upstreamAgent, signal,
      headers: { authorization: upstream.authorization, ...(body ? { "content-type": "application/json" } : {}) } }, resolve);
    outgoing.once("error", reject); outgoing.end(body);
  });
  await mkdir(captureDirectory);
  await writeFile(path.join(captureDirectory, "comparison.json"), JSON.stringify({ mode, model, endpoint: upstream.baseUrl,
    scope: "Only high-priority shared_project_context is removed in A; B is byte-identical. No retries or response rewriting.",
    private_bodies: true }, null, 2), { flag: "wx" });
  const sockets = new Set(), active = new Set(), pending = new Set(), failures = [];
  let sequence = 0, closing = false;
  async function handle(req, res) {
    const route = req.method === "GET" && req.url === "/v1/models" ? "/models"
      : req.method === "POST" && req.url === "/v1/chat/completions" ? "/chat/completions" : null;
    if (!route) { res.writeHead(404); res.end(); return; }
    const index = String(++sequence).padStart(6, "0"), prefix = path.join(captureDirectory, index);
    const controller = new AbortController(); active.add(controller);
    const abort = () => controller.abort();
    req.once("aborted", abort);
    res.once("close", abort);
    const metadata = { sequence, mode, method: req.method, route, model: route === "/chat/completions" ? model : null,
      started: new Date().toISOString(), sections: 0, changed_paths: [], status: null, completed: false, aborted: false };
    const responseHash = createHash("sha256");
    let responseFile, responseBytes = 0;
    try {
      let body;
      if (route === "/chat/completions") {
        const chunks = []; let bytes = 0;
        for await (const chunk of req) {
          bytes += chunk.length;
          if (bytes > 16 * 1024 * 1024) throw new Error("request-bound");
          chunks.push(chunk);
        }
        const before = Buffer.concat(chunks), parsed = JSON.parse(before);
        if (parsed.model !== model) throw new Error("model-mismatch");
        const transformed = transformComparisonRequest(parsed, mode);
        body = mode === "preserve" ? before : Buffer.from(JSON.stringify(transformed.output));
        metadata.sections = transformed.sections;
        metadata.changed_paths = transformed.changedPaths;
        metadata.before_sha256 = digest(before); metadata.after_sha256 = digest(body);
        await writeFile(`${prefix}.request.pre.json`, before, { flag: "wx" });
        await writeFile(`${prefix}.request.post.json`, body, { flag: "wx" });
      }
      const response = await send(route, req.method, body, controller.signal);
      metadata.status = response.statusCode;
      metadata.content_type = response.headers["content-type"] ?? null;
      metadata.content_encoding = response.headers["content-encoding"] ?? null;
      // A redirect must never move the credential or make the client discover a
      // different endpoint. Status/body are retained without forwarding Location.
      responseFile = await open(`${prefix}.response.bin`, "wx");
      res.writeHead(response.statusCode, { "content-type": metadata.content_type ?? "application/octet-stream",
        ...(metadata.content_encoding ? { "content-encoding": metadata.content_encoding } : {}) });
      res.flushHeaders();
      for await (const chunk of response) {
        responseHash.update(chunk); responseBytes += chunk.length;
        await responseFile.writeFile(chunk);
        if (!res.write(chunk)) await once(res, "drain", { signal: controller.signal });
      }
      metadata.completed = true;
      res.end();
    } catch (error) {
      metadata.aborted = controller.signal.aborted;
      metadata.failure = metadata.aborted ? "request-aborted" : "comparison-request-failed";
      metadata.error_code = typeof error?.code === "string" && /^[A-Z][A-Z0-9_]{0,63}$/.test(error.code) ? error.code : null;
      if (!metadata.aborted && !closing) failures.push({ sequence: metadata.sequence, code: metadata.failure });
      if (!res.headersSent && !res.destroyed) { res.writeHead(502, { "content-type": "application/json" }); res.end('{"error":"comparison proxy request failed"}'); }
      else if (!res.destroyed) res.destroy();
    } finally {
      await responseFile?.close();
      metadata.finished = new Date().toISOString();
      metadata.response_bytes = responseBytes; metadata.response_sha256 = responseHash.digest("hex");
      await writeFile(`${prefix}.metadata.json`, JSON.stringify(metadata, null, 2), { flag: "wx" });
      req.off("aborted", abort); res.off("close", abort); active.delete(controller);
    }
  }
  const server = createServer((req, res) => {
    const task = handle(req, res).catch(() => {
      failures.push({ code: "comparison-evidence-write-failed" }); res.destroy();
    }).finally(() => pending.delete(task));
    pending.add(task);
  });
  server.on("connection", socket => { sockets.add(socket); socket.once("close", () => sockets.delete(socket)); });
  await new Promise((resolve, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", resolve); });
  return { baseUrl: `http://127.0.0.1:${server.address().port}/v1`, failures, captureDirectory,
    async close() {
      if (closing) return;
      closing = true;
      const stopped = new Promise(resolve => server.close(resolve));
      for (const controller of active) controller.abort();
      upstreamAgent.destroy();
      for (const socket of sockets) socket.destroy();
      await Promise.allSettled([...pending]); await stopped;
      if (failures.length) throw new Error("Comparison proxy failed; inspect sanitized metadata in the private capture directory");
    } };
}

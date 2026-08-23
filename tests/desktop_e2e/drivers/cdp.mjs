import path from "node:path";
import { readdir, readFile } from "node:fs/promises";

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

export function parseDevToolsActivePort(text) {
  const lines = String(text).split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
  const port = Number(lines[0]);
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new TypeError("DevToolsActivePort has an invalid port");
  if (lines.length < 2 || !lines[1].startsWith("/devtools/browser/")) {
    throw new TypeError("DevToolsActivePort has an invalid browser endpoint");
  }
  return {
    port,
    browser_path: lines[1],
    browser_websocket_url: `ws://127.0.0.1:${port}${lines[1]}`,
  };
}

export function sameDevToolsGeneration(left, right) {
  return left !== null
    && right !== null
    && left?.port === right?.port
    && left?.browser_path === right?.browser_path;
}

async function findNamedFiles(root, name, maxDepth = 5) {
  const found = [];
  async function visit(directory, depth) {
    if (depth > maxDepth) return;
    let entries;
    try {
      entries = await readdir(directory, { withFileTypes: true });
    } catch (error) {
      if (error?.code === "ENOENT") return;
      throw error;
    }
    for (const entry of entries) {
      const candidate = path.join(directory, entry.name);
      if (entry.isSymbolicLink()) continue;
      if (entry.isDirectory()) await visit(candidate, depth + 1);
      else if (entry.isFile() && entry.name === name) found.push(candidate);
    }
  }
  await visit(path.resolve(root), 0);
  return found.sort((left, right) => left.localeCompare(right));
}

export async function discoverDevToolsEndpoint(
  userDataRoot,
  { timeoutMs = 60_000, pollMs = 100, afterEndpoint = null } = {},
) {
  const deadline = Date.now() + timeoutMs;
  let last = null;
  while (Date.now() < deadline) {
    const candidates = await findNamedFiles(userDataRoot, "DevToolsActivePort");
    if (candidates.length > 1) throw new Error(`multiple DevToolsActivePort owners: ${candidates.join(", ")}`);
    if (candidates.length === 1) {
      try {
        const endpoint = parseDevToolsActivePort(await readFile(candidates[0], "utf8"));
        const stale = sameDevToolsGeneration(endpoint, afterEndpoint);
        if (!stale) return { ...endpoint, active_port_file: candidates[0] };
        last = new Error("DevToolsActivePort still belongs to the previous Desktop generation");
      } catch (error) {
        last = error;
      }
    }
    await delay(pollMs);
  }
  throw new Error(`DevToolsActivePort discovery timed out${last ? `: ${last.message}` : ""}`);
}

export function selectExactTarget(targets, predicate) {
  if (!Array.isArray(targets)) throw new TypeError("CDP target list must be an array");
  if (typeof predicate !== "function") throw new TypeError("CDP target predicate must be a function");
  const matches = targets.filter((target) => target?.webSocketDebuggerUrl && predicate(target));
  if (matches.length !== 1) throw new Error(`expected exactly one CDP target, found ${matches.length}`);
  return structuredClone(matches[0]);
}

export function assertLocalTargetEndpoint(target, port) {
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new TypeError("CDP target port is invalid");
  let endpoint;
  try { endpoint = new URL(target?.webSocketDebuggerUrl); }
  catch { throw new TypeError("CDP target WebSocket endpoint is invalid"); }
  if (endpoint.protocol !== "ws:" || endpoint.hostname !== "127.0.0.1" || Number(endpoint.port) !== port) {
    throw new Error("CDP target endpoint escaped the execution-owned loopback port");
  }
  if (endpoint.username !== "" || endpoint.password !== "" || endpoint.search !== "" || endpoint.hash !== "") {
    throw new Error("CDP target endpoint contains unexpected authority or suffix data");
  }
  if (typeof target?.id !== "string" || endpoint.pathname !== `/devtools/page/${target.id}`) {
    throw new Error("CDP target endpoint does not match its page identity");
  }
  return endpoint.href;
}

export async function listCdpTargets(port, { timeoutMs = 5_000 } = {}) {
  const response = await fetch(`http://127.0.0.1:${port}/json/list`, { signal: AbortSignal.timeout(timeoutMs) });
  if (!response.ok) throw new Error(`CDP target discovery returned HTTP ${response.status}`);
  const targets = await response.json();
  if (!Array.isArray(targets)) throw new Error("CDP target discovery did not return an array");
  return targets;
}

export async function waitForExactCdpTarget(port, predicate, { timeoutMs = 60_000, pollMs = 100 } = {}) {
  const deadline = Date.now() + timeoutMs;
  let last = null;
  while (Date.now() < deadline) {
    try {
      const targets = await listCdpTargets(port, { timeoutMs: Math.min(2_000, timeoutMs) });
      const matches = targets.filter((target) => target?.webSocketDebuggerUrl && predicate(target));
      if (matches.length === 1) return structuredClone(matches[0]);
      if (matches.length > 1) throw new Error(`multiple exact CDP targets were exposed: ${matches.length}`);
      last = new Error("exact CDP target is not ready");
    } catch (error) {
      if (/multiple exact CDP targets/.test(error?.message ?? "")) throw error;
      last = error;
    }
    await delay(pollMs);
  }
  throw new Error(`exact CDP target discovery timed out${last ? `: ${last.message}` : ""}`);
}

export function waitForWebSocketOpen(
  socket,
  timeoutMs,
  { setTimer = setTimeout, clearTimer = clearTimeout } = {},
) {
  if (socket === null || typeof socket?.addEventListener !== "function" || typeof socket?.removeEventListener !== "function") {
    throw new TypeError("CDP socket must support DOM event listeners");
  }
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) throw new TypeError("CDP connect timeout must be positive");
  return new Promise((resolve, reject) => {
    let settled = false;
    let timer;
    const finish = (callback, value) => {
      if (settled) return;
      settled = true;
      clearTimer(timer);
      socket.removeEventListener("open", onOpen);
      socket.removeEventListener("error", onError);
      callback(value);
    };
    const onOpen = () => finish(resolve);
    const onError = () => finish(reject, new Error("CDP WebSocket connection failed"));
    timer = setTimer(() => finish(reject, new Error("CDP WebSocket connection timed out")), timeoutMs);
    socket.addEventListener("open", onOpen, { once: true });
    socket.addEventListener("error", onError, { once: true });
  });
}

export class CdpClient {
  #socket;
  #nextId = 0;
  #pending = new Map();
  #closed = false;

  static async connect(webSocketUrl, { connectTimeoutMs = 20_000, commandTimeoutMs = 45_000 } = {}) {
    const socket = new WebSocket(webSocketUrl);
    try { await waitForWebSocketOpen(socket, connectTimeoutMs); }
    catch (error) {
      socket.close();
      throw error;
    }
    return new CdpClient(socket, commandTimeoutMs);
  }

  constructor(socket, commandTimeoutMs) {
    this.#socket = socket;
    this.commandTimeoutMs = commandTimeoutMs;
    socket.addEventListener("message", (event) => {
      const payload = JSON.parse(event.data);
      if (!Object.hasOwn(payload, "id")) return;
      const waiter = this.#pending.get(payload.id);
      if (!waiter) return;
      clearTimeout(waiter.timer);
      this.#pending.delete(payload.id);
      if (payload.error) waiter.reject(new Error(JSON.stringify(payload.error)));
      else waiter.resolve(payload.result);
    });
    const rejectAll = () => {
      this.#closed = true;
      for (const waiter of this.#pending.values()) {
        clearTimeout(waiter.timer);
        waiter.reject(new Error("CDP WebSocket closed with a pending command"));
      }
      this.#pending.clear();
    };
    socket.addEventListener("close", rejectAll, { once: true });
    socket.addEventListener("error", rejectAll, { once: true });
  }

  call(method, params = {}) {
    if (this.#closed) return Promise.reject(new Error("CDP client is closed"));
    const id = ++this.#nextId;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#pending.delete(id);
        reject(new Error(`CDP ${method} timed out`));
      }, this.commandTimeoutMs);
      this.#pending.set(id, { resolve, reject, timer });
      this.#socket.send(JSON.stringify({ id, method, params }));
    });
  }

  async evaluate(expression) {
    const result = await this.call("Runtime.evaluate", {
      expression,
      returnByValue: true,
      awaitPromise: true,
      userGesture: true,
    });
    if (result?.exceptionDetails) throw new Error(result.exceptionDetails.text ?? "Runtime.evaluate failed");
    return result?.result?.value;
  }

  async screenshot() {
    await this.call("Page.enable");
    const result = await this.call("Page.captureScreenshot", { format: "png", fromSurface: true });
    if (typeof result?.data !== "string" || result.data.length === 0) throw new Error("CDP screenshot was empty");
    return Buffer.from(result.data, "base64");
  }

  close() {
    if (this.#closed) return;
    this.#closed = true;
    this.#socket.close();
  }
}

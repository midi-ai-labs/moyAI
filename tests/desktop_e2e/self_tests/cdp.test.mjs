import assert from "node:assert/strict";
import test from "node:test";

import {
  assertLocalTargetEndpoint,
  parseDevToolsActivePort,
  sameDevToolsGeneration,
  selectExactTarget,
  waitForWebSocketOpen,
} from "../drivers/cdp.mjs";

test("dynamic WebView2 DevTools endpoint is parsed without a fixed port", () => {
  assert.deepEqual(parseDevToolsActivePort("53142\n/devtools/browser/abc-123\n"), {
    port: 53142,
    browser_path: "/devtools/browser/abc-123",
    browser_websocket_url: "ws://127.0.0.1:53142/devtools/browser/abc-123",
  });
  assert.throws(() => parseDevToolsActivePort("9946\ninvalid\n"), /invalid browser endpoint/);
  assert.throws(() => parseDevToolsActivePort("0\n/devtools/browser/x\n"), /invalid port/);
});

test("restart discovery rejects the previous browser generation", () => {
  const previous = { port: 53142, browser_path: "/devtools/browser/abc-123" };
  assert.equal(sameDevToolsGeneration({ ...previous }, previous), true);
  assert.equal(sameDevToolsGeneration({ ...previous, port: 53143 }, previous), false);
  assert.equal(sameDevToolsGeneration({ ...previous, browser_path: "/devtools/browser/next" }, previous), false);
  assert.equal(sameDevToolsGeneration(previous, null), false);
});

test("target selection is exact and rejects ambiguity", () => {
  const targets = [
    { id: "worker", type: "worker", title: "moyAI", webSocketDebuggerUrl: "ws://worker" },
    { id: "main", type: "page", title: "moyAI", webSocketDebuggerUrl: "ws://main" },
  ];
  assert.equal(selectExactTarget(targets, (target) => target.type === "page" && target.title === "moyAI").id, "main");
  assert.throws(() => selectExactTarget(targets, (target) => target.title === "moyAI"), /found 2/);
  assert.throws(() => selectExactTarget(targets, (target) => target.title === "missing"), /found 0/);
});

test("WebSocket open wait cancels its deadline as soon as the socket opens", async () => {
  const socket = new EventTarget();
  const marker = {};
  let cleared = null;
  const waiting = waitForWebSocketOpen(socket, 20_000, {
    setTimer: () => marker,
    clearTimer: (value) => { cleared = value; },
  });
  socket.dispatchEvent(new Event("open"));
  await waiting;
  assert.equal(cleared, marker);
});

test("CDP page endpoint is bound to the discovered loopback port and target id", () => {
  const target = { id: "page-1", webSocketDebuggerUrl: "ws://127.0.0.1:53142/devtools/page/page-1" };
  assert.equal(assertLocalTargetEndpoint(target, 53142), target.webSocketDebuggerUrl);
  assert.throws(() => assertLocalTargetEndpoint({ ...target, webSocketDebuggerUrl: "ws://localhost:53142/devtools/page/page-1" }, 53142), /escaped/);
  assert.throws(() => assertLocalTargetEndpoint({ ...target, webSocketDebuggerUrl: "ws://127.0.0.1:53143/devtools/page/page-1" }, 53142), /escaped/);
  assert.throws(() => assertLocalTargetEndpoint({ ...target, webSocketDebuggerUrl: "ws://127.0.0.1:53142/devtools/page/other" }, 53142), /page identity/);
});

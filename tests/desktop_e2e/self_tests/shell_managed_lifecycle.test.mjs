import assert from "node:assert/strict";
import http from "node:http";
import test from "node:test";

import { createShellManagedLifecycleScenario, managedShellExitMenuReady, probeManagedShell, shutdownBeforeNaturalExpiry } from "../scenarios/shell_managed_lifecycle.mjs";

test("managed lifecycle scenario uses the common host contract and isolated state", () => {
  const scenario = createShellManagedLifecycleScenario();
  assert.equal(scenario.id, "shell.managed-lifecycle");
  assert.notEqual(scenario, createShellManagedLifecycleScenario());
  for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) assert.equal(typeof scenario[method], "function");
});

test("HTTP oracle requires exact identity and observes refusal after owned listener closes", async () => {
  const server = http.createServer((_request, response) => response.end("fixture-identity"));
  await new Promise((resolve, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", resolve); });
  const port = server.address().port;
  try {
    const observed = await probeManagedShell(port, "fixture-identity");
    assert.equal(observed.ready, true);
    assert.equal(observed.refused, false);
    assert.equal(observed.status, 200);
    assert.equal((await probeManagedShell(port, "another-fixture")).ready, false);
  } finally { await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve())); }
  assert.equal((await probeManagedShell(port, "fixture-identity")).refused, true);
});

test("HTTP timeout is not accepted as closed listener evidence", async () => {
  const sockets = new Set();
  const server = http.createServer(() => {});
  server.on("connection", (socket) => { sockets.add(socket); socket.on("close", () => sockets.delete(socket)); });
  await new Promise((resolve, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", resolve); });
  try {
    const observed = await probeManagedShell(server.address().port, "fixture-identity");
    assert.equal(observed.ready, false);
    assert.equal(observed.refused, false);
    assert.equal(observed.error, "ETIMEDOUT");
  } finally {
    for (const socket of sockets) socket.destroy();
    await new Promise((resolve) => server.close(resolve));
  }
});

test("shutdown oracle cannot confuse finite timeout with application cleanup", () => {
  const submitted = Date.parse("2026-01-01T00:00:00Z");
  const ready = { started_at: "2026-01-01T00:00:02Z", lifetime_ms: 60000 };
  const closed = { refused: true, finished_at_ms: submitted + 10000 };
  assert.equal(shutdownBeforeNaturalExpiry(ready, closed, submitted), true);
  assert.equal(shutdownBeforeNaturalExpiry(ready, { ...closed, refused: false }, submitted), false);
  assert.equal(shutdownBeforeNaturalExpiry(ready, { ...closed, finished_at_ms: submitted + 55000 }, submitted), false);
  assert.equal(shutdownBeforeNaturalExpiry(ready, { ...closed, finished_at_ms: submitted + 61000 }, submitted), false);
  assert.equal(shutdownBeforeNaturalExpiry({ ...ready, started_at: "unknown" }, closed, submitted), false);
  assert.equal(shutdownBeforeNaturalExpiry(ready, closed, null), false);
});

test("File click receipt is insufficient until the Exit action is rendered and usable", () => {
  const ready = { overlay: "file_menu", expanded: "true", exit_count: 1, exit_visible: true, exit_enabled: true };
  assert.equal(managedShellExitMenuReady(ready), true);
  for (const missing of [{ overlay: "none" }, { expanded: "false" }, { exit_count: 0 }, { exit_count: 2 }, { exit_visible: false }, { exit_enabled: false }]) {
    assert.equal(managedShellExitMenuReady({ ...ready, ...missing }), false);
  }
});

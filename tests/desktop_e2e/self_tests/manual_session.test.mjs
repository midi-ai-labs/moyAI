import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { parseManualArguments, parseManualCommand, processManualCommands, restartManualPC, quiesceManualPC } from "../manual_session.mjs";

const keys = ["binary", "hub-binary", "runner-binary", "runner-test-binary", "artifact-parent"];
const args = keys.flatMap(key => [`--${key}`, path.resolve("manual-fixture", key)]);

test("manual entry requires every explicit absolute artifact path without duplicate options", () => {
  assert.equal(Object.keys(parseManualArguments(args)).length, 5);
  for (const invalid of [args.slice(0, -2), [...args, "--binary", path.resolve("other")], [...args, "--unknown", path.resolve("other")], ["--binary", "relative", ...args.slice(2)]]) {
    assert.throws(() => parseManualArguments(invalid), TypeError);
  }
});

test("manual live entry accepts only an explicit absolute config path", () => {
  const config = path.resolve("private-fixture", "config.toml");
  assert.equal(parseManualArguments([...args, "--config-file", config])["config-file"], config);
  assert.throws(() => parseManualArguments([...args, "--config-file", "relative.toml"]), TypeError);
  assert.throws(() => parseManualArguments([...args, "--config-file", config, "--config-file", config]), TypeError);
});

test("manual command protocol permits only bounded lifecycle requests and an explicit verdict", () => {
  for (const command of [{ command: "start-b" }, { command: "capture-runner", pc: "b" }, { command: "finish", verdict: "pass", observations: ["Both windows remained independent."] }, { command: "finish", verdict: "pending", observations: [] }]) {
    assert.deepEqual(parseManualCommand(JSON.stringify(command)), command);
  }
  for (const value of [null, [], { command: "start-b", path: "other" }, { command: "capture-runner", pc: "c" }, { command: "finish", verdict: "pass", observations: [] }, { command: "finish", verdict: "success", observations: ["x"] }, { command: "finish", verdict: "fail", observations: ["x".repeat(4097)] }]) {
    assert.throws(() => parseManualCommand(JSON.stringify(value)), TypeError);
  }
  assert.throws(() => parseManualCommand(" ".repeat(65_537)), TypeError);
});

async function* lines(values) { for (const value of values) yield JSON.stringify(value); }

test("manual commands run in order and finish stops further mutations", async () => {
  const calls = [];
  const result = await processManualCommands(lines([{ command: "start-b" }, { command: "capture-runner", pc: "b" }, { command: "finish", verdict: "pending", observations: [] }, { command: "start-b" }]), {
    startB: async () => calls.push("b"), captureRunner: async pc => calls.push(pc + "-runner"), recordFinish: async value => calls.push(value.verdict),
  });
  assert.deepEqual(calls, ["b", "b-runner", "pending"]);
  assert.deepEqual(result, { acquisition: "pass", oracle: "not_required", manual: "pending" });
});

test("manual restart is bounded to an existing PC and an explicit startup target", async () => {
  for (const pc of ["a", "b"]) for (const startupTarget of ["workspace", "preferences"]) {
    const command = { command: "restart", pc, startupTarget };
    assert.deepEqual(parseManualCommand(JSON.stringify(command)), command);
  }
  for (const command of [{ command: "restart", pc: "c", startupTarget: "workspace" },
    { command: "restart", pc: "a" }, { command: "restart", pc: "a", startupTarget: "any" },
    { command: "restart", pc: "a", startupTarget: "preferences", config: "replacement" }]) {
    assert.throws(() => parseManualCommand(JSON.stringify(command)), TypeError);
  }
  const calls = [];
  const result = await processManualCommands(lines([
    { command: "restart", pc: "b", startupTarget: "preferences" },
    { command: "finish", verdict: "pending", observations: [] },
    { command: "restart", pc: "a", startupTarget: "workspace" },
  ]), { restartPC: async (...args) => calls.push(args), recordFinish: async () => {} });
  assert.deepEqual(calls, [["b", "preferences"]]);
  assert.equal(result.manual, "pending");
});

test("manual restart verifies the retired Runner and requires explicit capture of the new generation", async () => {
  const context = { paths: { workspace: "fixture-a" } }, sink = {}, driver = { generation: 1 };
  let verified = 0;
  const runner = { identity: { runner_id: "independent-runner" }, verifyExited: async () => { verified++; return { pass: true }; } };
  const scenario = { id: "manual", environment: { MOYAI_BASE_URL: "local" } };
  const result = { runtime: { generation: 2 }, driver: { generation: 2 }, restart: { zero_before_relaunch: true } };
  let received;
  const host = { restart: async args => { received = args; await args.beforeRelaunch(); return result; } };
  const pc = { context, sink, driver, runner, scenario, host, runtime: { generation: 1 } };
  assert.equal(await restartManualPC(pc, "preferences"), result.restart);
  assert.equal(received.context, context); assert.equal(received.sink, sink); assert.equal(received.driver, driver);
  assert.equal(received.scenario.startupTarget, "preferences");
  assert.equal(received.scenario.environment, scenario.environment);
  assert.equal(pc.driver, result.driver); assert.equal(pc.runtime, result.runtime);
  assert.equal(pc.runner, runner); assert.equal(pc.host, host);
  assert.equal(verified, 1); assert.equal(pc.captureRequested, true);
  assert.equal(scenario.startupTarget, undefined);
  await assert.rejects(() => restartManualPC(null, "preferences"), /not ready/);
  await assert.rejects(() => restartManualPC(pc, "invalid"), /startupTarget/);
  await assert.rejects(() => restartManualPC(pc, "workspace"), /capture-runner/);
  pc.captureRequested = false;
  host.restart = async () => { throw new Error("generation failed"); };
  await assert.rejects(() => restartManualPC(pc, "workspace"), /generation failed/);
  assert.equal(pc.driver, result.driver); assert.equal(pc.runtime, result.runtime);
});

test("manual restart refuses relaunch while the exact previous Runner is live or unverifiable", async () => {
  for (const result of [{ pass: false }, null]) {
    let relaunched = false;
    const pc = { runtime: {}, driver: {}, runner: { identity: { runner_id: "old" },
      verifyExited: async () => { if (result === null) throw new Error("owner unreadable"); return result; } },
      host: { restart: async args => { await args.beforeRelaunch(); relaunched = true; } } };
    await assert.rejects(() => restartManualPC(pc, "preferences"), result === null ? /owner unreadable/ : /Runner/);
    assert.equal(relaunched, false);
    assert.notEqual(pc.captureRequested, true);
  }
});

test("manual finish cannot certify a restarted Runner using the previous owner's absence", async () => {
  const retired = { pass: true, already_exited: true, process_id: 22 };
  const pc = { name: "b", runtime: { desktop_process_id: 44 }, captureRequested: true,
    runner: { identity: { process_id: 22 }, quiesce: async () => retired } };
  const result = await quiesceManualPC(pc, { finished: true });
  assert.equal(result.input, "fail");
  assert.equal(result.resources[0].runner.unobserved_runner, true);
  assert.equal(retired.pass, true);
});

test("manual finish accepts a recaptured generation and preserves uncaptured failure evidence", async () => {
  const result = { pass: true, already_exited: true, process_id: 45 };
  const pc = { name: "a", runtime: { desktop_process_id: 44 }, captureRequested: false,
    runner: { identity: { process_id: 45 }, quiesce: async () => result } };
  assert.equal((await quiesceManualPC(pc, { finished: true })).input, "pass");
  const uncaptured = { name: "b", runtime: {}, captureRequested: true,
    runner: { identity: null, quiesce: async () => { throw new Error("capture failed"); } } };
  const failed = await quiesceManualPC(uncaptured, { finished: true });
  assert.equal(failed.input, "fail");
  assert.equal(failed.resources[0].runner.error, "capture failed");
  assert.equal(failed.resources[0].runner.unobserved_runner, true);
});

test("EOF, malformed requests and callback errors remain failures for common cleanup", async () => {
  let called = false;
  const callbacks = { startB: async () => { called = true; throw new Error("capture failed"); }, captureRunner: async () => {}, recordFinish: async () => {} };
  await assert.rejects(() => processManualCommands(lines([]), callbacks), error => error.code === "manual-session-input-ended");
  await assert.rejects(() => processManualCommands(lines([{ command: "unknown" }]), callbacks), TypeError);
  assert.equal(called, false);
  await assert.rejects(() => processManualCommands(lines([{ command: "start-b" }, { command: "finish", verdict: "pass", observations: ["must not run"] }]), callbacks), /capture failed/);
});

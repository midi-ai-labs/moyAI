import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { parseManualArguments, parseManualCommand, processManualCommands } from "../manual_session.mjs";

const keys = ["binary", "hub-binary", "runner-binary", "runner-test-binary", "artifact-parent"];
const args = keys.flatMap(key => [`--${key}`, path.resolve("manual-fixture", key)]);

test("manual entry requires every explicit absolute artifact path without duplicate options", () => {
  assert.equal(Object.keys(parseManualArguments(args)).length, 5);
  for (const invalid of [args.slice(0, -2), [...args, "--binary", path.resolve("other")], [...args, "--unknown", path.resolve("other")], ["--binary", "relative", ...args.slice(2)]]) {
    assert.throws(() => parseManualArguments(invalid), TypeError);
  }
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

test("EOF, malformed requests and callback errors remain failures for common cleanup", async () => {
  let called = false;
  const callbacks = { startB: async () => { called = true; throw new Error("capture failed"); }, captureRunner: async () => {}, recordFinish: async () => {} };
  await assert.rejects(() => processManualCommands(lines([]), callbacks), error => error.code === "manual-session-input-ended");
  await assert.rejects(() => processManualCommands(lines([{ command: "unknown" }]), callbacks), TypeError);
  assert.equal(called, false);
  await assert.rejects(() => processManualCommands(lines([{ command: "start-b" }, { command: "finish", verdict: "pass", observations: ["must not run"] }]), callbacks), /capture failed/);
});

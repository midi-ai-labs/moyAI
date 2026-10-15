import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import { manualCase3Stages, manualCase3CliResult, manualCase3VerifyCli } from "../scenarios/manual_case3.mjs";

const spec = name => readFile(new URL(`../../manual_ST/${name}/spec.md`, import.meta.url), "utf8");
const result = exit => ({ outcome: { root_exit_code: exit } });

test("Case3 preserves the callable API accepted by Case1 without assuming its name", async () => {
  assert.match(await spec("case1"), /四則演算を行う callable API/);
  const stages = manualCase3Stages(await spec("case3"));
  for (const stage of stages.slice(1)) {
    assert.doesNotMatch(stage.prompt, /既存の\s*`calculate\(left, operator, right\)`/);
    assert.match(stage.prompt, /既存の callable API/);
    assert.match(stage.prompt, /引数.*意味/);
  }
});

test("usage guidance checked by the CLI oracle is disclosed in both canonical requests", async () => {
  for (const stage of manualCase3Stages(await spec("case3")).slice(1)) {
    assert.match(stage.prompt, /メッセージ[^\n]*usage[^\n]*使用方法[^\n]*使い方/);
    assert.match(stage.prompt, /stderr/);
    assert.match(stage.prompt, /exit code 1/);
    assert.match(stage.prompt, /log 10/);
    assert.match(stage.prompt, /unsupported function exit code 2 を期待する test は作らない/);
  }
});

async function exercise(failAt = null) {
  const row = {}, calls = [], budget = { marker: "same budget" };
  const outputs = ["5", "8", "0", "1", "4"];
  const failures = await manualCase3VerifyCli({ context: {}, sink: {}, options: {}, owner: "test", stem: "case3", row, observationBudget: budget }, async args => {
    const index = calls.length;
    calls.push(args);
    assert.equal(args.observationBudget, budget);
    const failed = index === failAt;
    return { result: result(index < 5 ? 0 : failed ? 2 : 1),
      stdout: index < 5 ? `${failed ? "wrong" : outputs[index]}\n` : "", stderr: index < 5 ? "" : "usage: calculator <left> <operator> <right> or <function> <value>" };
  });
  return { row, calls, failures };
}

test("Case3 saves all seven CLI results including log after an earlier failed condition", async () => {
  for (const failAt of [0, 5]) {
    const value = await exercise(failAt);
    assert.equal(value.calls.length, 7);
    assert.deepEqual(value.row.external_cli.at(-1).argv, ["log", "10"]);
    assert.equal(value.row.external_cli.at(-1).expected.exit, 1);
    assert.equal(value.row.external_cli.at(-1).pass, true);
    assert.equal(value.row.external_cli[failAt].pass, false);
    assert.deepEqual(value.failures, ["stage3:external-cli-contract-failed"]);
  }
});

test("all successful CLI conditions keep exact labels, command forms and stdout results", async () => {
  const value = await exercise();
  assert.deepEqual(value.failures, []);
  assert.deepEqual(value.calls.map(call => call.label), Array.from({ length: 7 }, (_, i) => `case3-cli-${i + 1}`));
  assert.deepEqual(value.calls.map(call => call.args), [
    ["-X", "utf8", "calculator.py", "2", "+", "3"], ["-X", "utf8", "calculator.py", "2", "pow", "3"],
    ["-X", "utf8", "calculator.py", "sin", "0"], ["-X", "utf8", "calculator.py", "cos", "0"],
    ["-X", "utf8", "calculator.py", "sqrt", "16"], ["-X", "utf8", "calculator.py", "8", "+"],
    ["-X", "utf8", "calculator.py", "log", "10"],
  ]);
  assert.ok(value.row.external_cli.every(row => row.pass));
});

test("usage guidance still rejects wrong exit, wrong stream and missing guidance", () => {
  for (const guidance of ["usage: calculator", "使用方法: calculator", "使い方: calculator"])
    assert.equal(manualCase3CliResult(result(1), "", guidance, { exit: 1 }), true);
  for (const [exit, stdout, stderr] of [[2, "", "usage: calculator"], [1, "usage: calculator", ""], [1, "", ""], [1, "", "unsupported function log"]])
    assert.equal(manualCase3CliResult(result(exit), stdout, stderr, { exit: 1 }), false);
  assert.equal(manualCase3CliResult(result(0), "5.0\n", "", { exit: 0, suffix: "5" }), false);
});

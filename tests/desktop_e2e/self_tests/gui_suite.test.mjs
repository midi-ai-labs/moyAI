import assert from "node:assert/strict";
import test from "node:test";
import { mkdtemp, readFile, writeFile, mkdir, rm } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import crypto from "node:crypto";
import { GUI_SUITES, parseSuiteArguments, summarizeExecution, suiteDecision, executeSuiteCases, readSealedExecution, runSuite } from "../run_suite.mjs";
import { createScenario } from "../scenario_registry.mjs";
import { classifyExecution } from "../core/execution.mjs";
import { verifyGui, GUI_CHECK_STAGES, desktopCompilerArtifact } from "../../../scripts/verify-gui.mjs";

const tempParent = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../../../project_sandbox/gui-suite-self-tests");
async function tempDirectory(prefix) { await mkdir(tempParent, { recursive: true }); return mkdtemp(path.join(tempParent, prefix)); }
async function removeTemp(root) {
  assert.equal(path.dirname(path.resolve(root)), tempParent);
  await rm(root, { recursive: true, force: true });
}

const makeResult = (manual = "not_required", overrides = {}) => ({
  ...classifyExecution({ preflight: "pass", acquisition: "pass", oracle: "pass", cleanup: "pass", manual, ...overrides }),
  scenario_id: "run.stop", execution_id: "e2e-suite-fixture-1234", elapsed_ms: 15,
  diagnostics: [{ code: "owned-failure-code", message: "secret-message", evidence: { token: "secret-token" } }],
});
const manifest = { scenario_id: "run.stop", execution_id: "e2e-suite-fixture-1234", binary: { sha256: "a".repeat(64), path: "private-path" }, harness: { tree_sha256: "b".repeat(64) }, token: "secret-manifest" };
const row = (manual = "not_required", overrides = {}) => summarizeExecution(makeResult(manual, overrides), manifest);

test("GUI suite options reject omissions, duplicates, unknown modes and silent empty selection", () => {
  assert.equal(parseSuiteArguments([]).suite, "smoke");
  assert.equal(parseSuiteArguments(["--suite", "regression", "--automation-only"]).automationOnly, true);
  for (const input of [["--suite"], ["--suite", "all"], ["--list", "--list"], ["--grep", "nothing"], ["smoke"]]) assert.throws(() => parseSuiteArguments(input));
  assert.equal(suiteDecision([], []).exit_code, 1);
});

test("default smoke binds existing automatic scenarios without physical providers or review gates", () => {
  assert.equal(new Set(GUI_SUITES.regression).size, GUI_SUITES.regression.length);
  for (const id of GUI_SUITES.smoke) {
    assert.ok(GUI_SUITES.regression.includes(id));
    const scenario = createScenario(id);
    assert.equal(scenario.id, id);
    assert.equal(scenario.productOracle, "pass");
    assert.equal(scenario.manualGate, "not_required");
  }
  for (const id of GUI_SUITES.regression) assert.equal(createScenario(id).id, id);
});

test("manual pending remains review_required, with nonzero default and explicit automation-only opt-in", () => {
  const pending = row("pending");
  assert.equal(pending.classification, "manual_pending");
  assert.equal(suiteDecision([pending], ["run.stop"]).exit_code, 3);
  const automated = suiteDecision([pending], ["run.stop"], true);
  assert.equal(automated.exit_code, 0);
  assert.equal(automated.status, "review_required");
  assert.equal(automated.pending_manual_cases, 1);
});

test("product, manual, acquisition and cleanup failures cannot pass automation-only", () => {
  for (const input of [{ oracle: "fail" }, { manual: "fail" }, { acquisition: "not_run" }, { cleanup: "fail" }, { preflight: "blocked" }]) {
    assert.notEqual(suiteDecision([row(input.manual ?? "not_required", input)], ["run.stop"], true).exit_code, 0);
  }
  assert.equal(suiteDecision([row()], ["run.stop", "run.next-turn"]).status, "failed");
  assert.equal(suiteDecision([row()], ["run.next-turn"]).status, "failed");
});

test("public summary excludes arbitrary messages, evidence, paths and extra manifest fields", () => {
  const result = row();
  const text = JSON.stringify(result);
  assert.ok(!text.includes("secret"));
  assert.ok(!text.includes("private-path"));
  assert.deepEqual(result.diagnostic_codes, ["owned-failure-code"]);
  assert.throws(() => summarizeExecution({ ...makeResult("fail"), classification: "pass" }, manifest));
});

test("suite stops on failed exact cleanup and never launches subsequent GUI cases", async () => {
  const called = [];
  const outcome = await executeSuiteCases({ ids: ["run.stop", "run.next-turn"], binary: "owned.exe", artifactParent: "owned",
    execute: async args => { called.push(args); return {}; }, readResult: async () => row("not_required", { cleanup: "fail" }),
  });
  assert.equal(called.length, 1);
  assert.equal(outcome.length, 1);
  assert.equal(outcome[0].classification, "harness_ng");
});

test("sealed result and manifest fingerprints are checked before publishing automation success", async () => {
  const root = await tempDirectory("moyai-suite-seal-");
  try {
    await mkdir(path.join(root, "evidence"));
    const resultBytes = JSON.stringify(makeResult());
    const manifestBytes = JSON.stringify(manifest);
    const hash = text => crypto.createHash("sha256").update(text).digest("hex");
    await writeFile(path.join(root, "evidence/result.json"), resultBytes);
    await writeFile(path.join(root, "evidence/execution.json"), manifestBytes);
    await writeFile(path.join(root, "evidence/seal.json"), JSON.stringify({ result_sha256: hash(resultBytes), files: [{ relative_path: "execution.json", sha256: hash(manifestBytes) }] }));
    assert.equal((await readSealedExecution({ execution_root: root })).automation, "pass");
    await writeFile(path.join(root, "evidence/result.json"), JSON.stringify(makeResult("fail")));
    await assert.rejects(readSealedExecution({ execution_root: root }), /hash mismatch/);
  } finally { await removeTemp(root); }
});

test("verification builds in order and stops before GUI when any prerequisite fails", async () => {
  const parent = await tempDirectory("moyai-gui-checks-");
  try {
    const calls = [];
    const result = await verifyGui({ artifactParent: parent }, {
      run: async stage => { calls.push(stage.name); return { exitCode: stage.name === "frontend-build" ? 7 : 0 }; },
      gui: async () => assert.fail("failed frontend build must not run stale binary"), output: () => {},
    });
    assert.deepEqual(calls, GUI_CHECK_STAGES.slice(0, 3).map(stage => stage.name));
    assert.equal(result.exitCode, 1);
    assert.equal(result.report.status, "failed");
    assert.ok(!result.report.gui);
  } finally { await removeTemp(parent); }
});

test("verification preserves review_required and its exit after actual GUI", async () => {
  const parent = await tempDirectory("moyai-gui-review-");
  const compilerBinary = path.join(parent, "custom-target", "moyai-desktop.exe");
  try {
    const result = await verifyGui({ artifactParent: parent, suite: "regression" }, {
      run: async () => ({ exitCode: 0, binary: compilerBinary }), output: () => {}, gui: async options => {
        assert.equal(options.binary, compilerBinary, "run the compiler-reported executable, including a custom target directory");
        return { exitCode: 3, summary: { status: "review_required", automation: "pass", executed_cases: 16, expected_cases: 16, pending_manual_cases: 3 } };
      },
    });
    assert.equal(result.exitCode, 3);
    assert.equal(result.report.status, "review_required");
  } finally { await removeTemp(parent); }
});

test("unexpected suite error leaves failed public report without diagnostic contents", async () => {
  const parent = await tempDirectory("moyai-gui-error-");
  try {
    const result = await runSuite({ artifactParent: parent }, { execute: async () => { throw new Error("secret-command-content"); }, output: () => {} });
    assert.equal(result.exitCode, 1);
    const report = await readFile(path.join(result.root, "public-summary.json"), "utf8");
    assert.ok(!report.includes("secret-command-content"));
    assert.equal(result.summary.executed_cases, 0);
  } finally { await removeTemp(parent); }
});

test("Cargo artifact selection honors the compiler output rather than a default target path", () => {
  const binary = path.join(tempParent, "alternate-build", "moyai-desktop.exe");
  const item = { reason: "compiler-artifact", target: { name: "moyai-desktop", kind: ["bin"] }, executable: binary };
  assert.equal(desktopCompilerArtifact(`warning text\n${JSON.stringify(item)}\n`), binary);
  assert.throws(() => desktopCompilerArtifact("Finished dev build"));
  assert.throws(() => desktopCompilerArtifact(`${JSON.stringify(item)}\n${JSON.stringify({ ...item, executable: path.join(tempParent, "other.exe") })}`));
});

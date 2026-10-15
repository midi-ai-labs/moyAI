import assert from "node:assert/strict";
import test from "node:test";
import { DesktopE2eError } from "../core/execution.mjs";
import { executeCase52FinalStages, normalizeCase52Options, runFinalEvaluator } from "../scenarios/case5_2.mjs";

const options = normalizeCase52Options({ fixture_source: "C:/fixture", provider_base_url: "http://127.0.0.1:1234/v1",
  provider_profile: "openai_compatible", main_model: "main", side_model: "side" });
const names = ["README.md", "basic_design.md", "detail_design.md", "evidence_matrix.md", "cancel_contract.md"];
const fault = label => new DesktopE2eError("harness", "test-injected-failure", label);

function exercise({ publicExit = 0, hiddenExit = 0, missingDocument = false, stopAt = null, bounds = null } = {}) {
  const calls = [], saved = new Map(), events = [];
  const failure = fault(stopAt ?? "unused-failure");
  const step = name => { calls.push(name); if (name === stopAt) throw failure; };
  const manifest = stage => ({ stage, diff: { added: [], modified: [], deleted: [] },
    documents: names.map(name => ({ name, exists: true, bytes: missingDocument && name === "cancel_contract.md" ? 0 : 10 })) });
  const sink = {
    async writeJson(name, value) { step(`write:${name}`); saved.set(name, structuredClone(value)); return { path: name, sha256: "evidence-sha" }; },
    async record(name, value) { step(`record:${name}`); events.push({ name, value: structuredClone(value) }); },
  };
  const state = { baseline: {}, python: { path: "pinned-python" }, oracle: { path: "fixed-oracle", sha256: "oracle-sha", size_bytes: 100 },
    restorationStages: { restart: "pass", stage4: "running", hidden_oracle: "not_run", stage5: "not_run" },
    acceptedEnd: false, sidePromptInput: { text: "fixed-side-prompt" }, sideProviderSamples: [],
    providerProfileExact: true, providerComparabilityDeviations: [], providerHostFingerprintSamples: [], performance: {} };
  const context = { paths: { workspace: "C:/fixture/workspace", logs: "C:/fixture/logs" }, manifest: { started_at: new Date().toISOString() } };
  const evaluatorIo = {
    async runWorkspaceStableEvaluator(args) {
      step("public-evaluator");
      const result = await args.evaluate();
      step("public-workspace-integrity");
      return { result, afterManifest: manifest(args.afterStage), workspaceDiff: {} };
    },
    async runPythonTest({ name }) {
      step(name);
      return { exit_code: name === "stage4-public-suite" ? publicExit : hiddenExit,
        outcome: { timed_out: bounds === `${name}:timeout`, output_limit_exceeded: bounds === `${name}:output` } };
    },
    async assertOracleIdentity(_identity, label) { step(label); return { sha256: "oracle-sha" }; },
    async storeStageManifest({ stage }) { step(stage); return manifest(stage); },
    async assertEvaluatorWorkspaceStable({ label }) { step(`integrity:${label}`); return {}; },
  };
  const side = { owner_session_id: "session", chat_id: "side-chat", messages: [] };
  const stage = number => ({ stage: `stage${number}`, sessionId: "session", turnId: `turn-${number}`, elapsed_ms: 1, terminal: { side_chat: side } });
  const args = { context, cdp: {}, input: {}, sink, options, state, stage1: stage(1), stage2: stage(2), stage3: stage(3), stage4: stage(4),
    restoredProjection: { side_chat: side }, initialShell: {}, restartShell: {}, restarted: { restart: {} }, transcript: {} };
  const io = {
    ...evaluatorIo,
    runFinalEvaluator: args => runFinalEvaluator(args, evaluatorIo),
    async configureSideChat() { step("configure-side"); },
    async providerMustKeepSideUnloaded({ name }) { step(name); },
    async executeCase52SideChatStage(args) {
      step("stage5"); assert.equal(args.sessionId, "session"); assert.equal(args.promptInput, state.sidePromptInput);
      return { stage: "stage5", elapsed_ms: 1, binding: { provider_profile: "openai_compatible", model: "side", owner_session_id: "session", chat_id: "side-chat" },
        question: "fixed-side-prompt", answer: "answer", completedSurface: { projection: { side_chat: { ...side, messages: [{ role: "user" }, { role: "assistant" }] } } } };
    },
    async verifyCase52Safety({ name }) { step(`safety:${name}`); return { safety: {}, finalProvider: { models: [] } }; },
  };
  return { run: () => executeCase52FinalStages(args, io), calls, saved, events, state, failure };
}

test("Stage4 artifact NG reaches Stage5 and summary, then retains its product failure", async t => {
  for (const [config, expectedFailure, hiddenStatus] of [
    [{ hiddenExit: 1 }, "hidden-oracle-failed", "failed"],
    [{ publicExit: 2 }, "public-suite-failed", "pass"],
    [{ publicExit: 5 }, "public-suite-failed", "pass"],
    [{ missingDocument: true }, "required-documents-incomplete", "pass"],
  ]) await t.test(expectedFailure + JSON.stringify(config), async () => {
    const value = exercise(config);
    await assert.rejects(value.run(), error => error.owner === "product" && error.code === "case5_2-final-evaluation"
      && error.evidence.failures.includes(expectedFailure) && error.evidence.summary.path === "case5_2/summary.json");
    assert.equal(value.calls.filter(name => name === "stage5").length, 1);
    assert.ok(value.calls.indexOf("write:case5_2/evaluation.json") < value.calls.indexOf("stage5"));
    assert.ok(value.calls.indexOf("safety:stage4-pre-side-chat") < value.calls.indexOf("stage5"));
    assert.ok(value.calls.indexOf("integrity:stage5-side-chat-read-only") < value.calls.indexOf("safety:final"));
    const summary = value.saved.get("case5_2/summary.json");
    assert.deepEqual(summary.evaluation, value.saved.get("case5_2/evaluation.json"));
    assert.equal(summary.stage5.answer, "answer");
    assert.equal(value.state.restorationStages.hidden_oracle, hiddenStatus);
    assert.equal(value.state.restorationStages.stage5, "pass");
    assert.equal(value.state.acceptedEnd, false);
    assert.equal(value.events.some(event => event.name === "case5_2-accepted"), false);
  });
});

test("successful final stages preserve the pending manual verdict and acceptance order", async () => {
  const value = exercise();
  assert.deepEqual(await value.run(), { acquisition: "pass", oracle: "pass", manual: "pending" });
  assert.equal(value.state.acceptedEnd, true);
  assert.equal(value.saved.get("case5_2/summary.json").quality_adjudication, "manual_rubric_pending");
  assert.deepEqual(value.calls.slice(-2), ["write:case5_2/summary.json", "record:case5_2-accepted"]);
});

test("execution, integrity, evidence and safety failures still stop before Side input", async t => {
  for (const stopAt of ["stage4-public-suite", "public-workspace-integrity", "pre-hidden-evaluator", "stage4-hidden-oracle",
    "post-hidden-evaluator", "stage4-post-evaluator", "integrity:stage4-hidden-oracle", "write:case5_2/evaluation.json",
    "record:case5_2-final-evaluation", "safety:stage4-pre-side-chat"]) await t.test(stopAt, async () => {
    const value = exercise({ hiddenExit: 1, stopAt });
    await assert.rejects(value.run(), error => error === value.failure);
    assert.equal(value.calls.includes("stage5"), false);
    assert.equal(value.state.acceptedEnd, false);
    assert.equal(value.saved.has("case5_2/summary.json"), false);
  });
});

test("external test timeout and output bounds are not deferred as artifact NG", async t => {
  for (const name of ["stage4-public-suite", "stage4-hidden-oracle"]) for (const bound of ["timeout", "output"]) {
    await t.test(`${name}:${bound}`, async () => {
      const value = exercise({ bounds: `${name}:${bound}` });
      await assert.rejects(value.run(), error => error.owner === "product" && error.code === "case5_2-external-test-bounds");
      assert.equal(value.calls.includes("stage5"), false);
      assert.equal(value.saved.has("case5_2/evaluation.json"), false);
    });
  }
});

test("later Side, read-only, safety or summary failures retain the already saved artifact NG", async t => {
  for (const stopAt of ["stage5", "integrity:stage5-side-chat-read-only", "safety:final", "write:case5_2/summary.json"]) {
    await t.test(stopAt, async () => {
      const value = exercise({ hiddenExit: 1, stopAt });
      await assert.rejects(value.run(), error => error === value.failure);
      assert.equal(value.saved.get("case5_2/evaluation.json").hidden_oracle_pass, false);
      assert.equal(value.events.some(event => event.name === "case5_2-final-evaluation"), true);
      assert.equal(value.events.some(event => event.name === "case5_2-accepted"), false);
      assert.equal(value.state.acceptedEnd, false);
    });
  }
});

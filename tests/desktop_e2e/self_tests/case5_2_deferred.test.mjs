import assert from "node:assert/strict";
import test from "node:test";
import { ManualObservationBudget } from "../core/deadline.mjs";
import { normalizeCase52Options, executeCase52NormalStages, case52RestorationSettings } from "../scenarios/case5_2.mjs";

const options = normalizeCase52Options({ fixture_source: "C:/fixture", provider_base_url: "http://127.0.0.1:1234/v1",
  provider_profile: "openai_compatible", main_model: "main", side_model: "side" });
const documents = ["README.md", "basic_design.md", "detail_design.md", "evidence_matrix.md", "cancel_contract.md"];
function manifest(stage) {
  const names = stage === "stage1" ? documents.slice(0, 4) : documents;
  return { stage, baseline_aggregate_sha256: "seed", evidence_matrix_rows: 25,
    documents: names.map(name => ({ name, exists: true, bytes: 10 })),
    files: names.map(name => ({ path: name, sha256: `digest-${name}`, bytes: 10 })),
    diff: { added: names, modified: [], deleted: [] } };
}
function terminal() {
  const fields = { "model.base_url": options.providerBaseUrl, "model.model": "main", "model.api_key_env": "", "model.provider_profile": "openai_compatible",
    "model.request_timeout_ms": "3600000", "model.max_retries": "0", "model.context_window": "131072", "model.supports_tools": "true",
    "model.parallel_tool_calls": "false", "permissions.access_mode": "auto_review", "multi_agent.enabled": "false", "docling.enabled": "false", "mcp.enabled": "false" };
  return { config_fields: Object.entries(fields).map(([key, value]) => ({ key, value })), provider_effective_base_url: options.providerBaseUrl,
    provider_effective_model_id: "main", provider_effective_api_key_env: "", provider_effective_context_window: "131072", provider_effective_profile: "openai_compatible",
    session_settings: { available: true, base_url: options.providerBaseUrl, model: "main", api_key_env: "", provider_profile: "openai_compatible",
      access_mode: "auto_review", context_window: "", context_window_inherited: true } };
}
function exercise({ suiteExit = 0, invalidScope = false, timedOut = false } = {}) {
  const calls = [], references = [];
  let clock = 0;
  const budget = new ManualObservationBudget({ timeoutMs: 1000, now: () => clock });
  const state = { baseline: { stage: "baseline" }, promptInputs: { stage1: { text: "research" }, stage2: { text: "design" }, stage3: { text: "implement" } }, python: { path: "pinned-python" } };
  const io = {
    async executeStage(args) {
      calls.push(`stage:${args.stage.id}`); references.push({ session: args.expectedSessionId, stage: args.referenceManifest.stage });
      assert.equal(args.observationBudget, budget);
      if (args.stage.id === "stage1") budget.beginOnce();
      clock += 100;
      return { stage: args.stage.id, sessionId: "saved-session", turnId: `turn-${args.stage.id}`, terminal: terminal() };
    },
    async configureSideChat(args) { calls.push("side-configure"); assert.equal(args.sessionId, "saved-session"); },
    async providerMustKeepSideUnloaded(args) { calls.push(args.name); },
    async storeStageManifest({ stage }) { calls.push(`manifest:${stage}`); const value = manifest(stage); if (invalidScope && stage === "stage2") value.diff.modified.push("frontend/client.ts"); return value; },
    async runStage3PublicSuite() { calls.push("public-suite"); return { exit_code: suiteExit, outcome: { timed_out: timedOut, output_limit_exceeded: false } }; },
    async runWorkspaceStableEvaluator(args) {
      calls.push("stable-evaluator"); assert.equal(args.beforeManifest.stage, "stage3");
      const result = await args.evaluate(); clock += 100;
      return { result, afterManifest: manifest(args.afterStage) };
    },
  };
  const run = () => executeCase52NormalStages({ context: {}, cdp: {}, input: {}, sink: {}, options, state, observationBudget: budget }, io);
  return { run, calls, references, budget, advance: ms => { clock += ms; } };
}

test("normal Case52 runs exactly three owned stages and the stable public suite without recovery effects", async () => {
  const value = exercise();
  const completed = await value.run();
  assert.deepEqual(value.calls, ["stage:stage1", "side-configure", "side-configured-no-request", "manifest:stage1",
    "stage:stage2", "manifest:stage2", "stage:stage3", "manifest:stage3", "stable-evaluator", "public-suite", "stage3-terminal"]);
  assert.deepEqual(value.references, [{ session: null, stage: "baseline" }, { session: "saved-session", stage: "stage1" }, { session: "saved-session", stage: "stage2" }]);
  assert.equal(completed.stage3PostEvaluatorManifest.stage, "stage3-post-evaluator");
  assert.equal(completed.stage3.sessionId, "saved-session");
  assert.equal(Object.hasOwn(completed, "stage4"), false);
});

test("normal Stage3 retains public-suite and document-scope failures instead of accepting a checkpoint", async () => {
  for (const [options, code] of [[{ suiteExit: 1 }, "case5_2-stage3-public-suite"], [{ timedOut: true }, "case5_2-external-test-bounds"],
    [{ invalidScope: true }, "case5_2-stage2-scope"]]) {
    const value = exercise(options);
    await assert.rejects(value.run(), error => error.owner === "product" && error.code === code);
    assert.equal(value.calls.includes("stage3-terminal"), false);
  }
});

test("intervening cases consume the original finite deadline without resetting the Case52 budget", async () => {
  const value = exercise(); await value.run();
  const before = value.budget.snapshot();
  value.advance(700);
  assert.equal(value.budget.expired, true);
  assert.equal(value.budget.snapshot().deadline_ms, before.deadline_ms);
  assert.equal(value.budget.snapshot().started_at_ms, before.started_at_ms);
  assert.throws(() => value.budget.assertRemaining("restoration"), /exhausted/);
});

test("restoration Settings carry Stage3 public Side defaults with secrets redacted", () => {
  const projection = { config_fields: [{ key: "side_chat.model", value: "stage3-side" }, { key: "side_chat.api_key", value: "", sensitive: true }] };
  assert.deepEqual(case52RestorationSettings(projection), [
    { key: "side_chat.model", value: "stage3-side", explicit: false }, { key: "side_chat.api_key", value: "", explicit: false }]);
  assert.throws(() => case52RestorationSettings({ config_fields: [{ key: "side_chat.api_key", value: "private-credential", sensitive: true }] }),
    error => !error.message.includes("private-credential") && error.message.includes("redacted"));
  assert.throws(() => case52RestorationSettings({ config_fields: [projection.config_fields[0], projection.config_fields[0]] }), /unique/);
});

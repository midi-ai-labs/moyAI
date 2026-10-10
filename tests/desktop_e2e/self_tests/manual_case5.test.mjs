import assert from "node:assert/strict";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import test from "node:test";
import { copyCase52CleanSeed, inventoryCase52CleanSeed } from "../core/clean_seed.mjs";
import { createManualCase5Scenario, normalizeManualCase5Options, manualCase5Input, manualCase5ScopeFailures, MANUAL_CASE5_COPY_RULE } from "../scenarios/manual_case5.mjs";
import { createManualCase6Scenario } from "../scenarios/manual_case6.mjs";

const RAW = { provider_base_url: "http://provider.invalid/v1", model: "exact-model", python_executable: process.execPath, approval_mode: "operator", fixture_source: process.cwd() };
const file = (name, hash = "same") => ({ path: name, sha256: hash, size_bytes: 3 });

test("case5 preserves canonical repository request/task and requires an explicit portable fixture", async () => {
  const input = manualCase5Input(await readFile(new URL("../../manual_ST/case5/spec.md", import.meta.url), "utf8"));
  assert.equal(input.prompt, "current directory の `task.md` に従って manual ST の case5 を実施してください。\n作業対象は current directory 以下のみです。");
  assert.match(input.task, /既存の実装コード、設定、テストは変更しない/);
  assert.equal(normalizeManualCase5Options(RAW).fixtureSource, process.cwd());
  assert.throws(() => normalizeManualCase5Options({ ...RAW, fixture_source: "relative" }), /absolute/);
  const scenario = createManualCase5Scenario(RAW);
  assert.equal(scenario.id, "manual.case5"); assert.equal(scenario.manualGate, "pending");
});

test("case5 docs-only gate requires three physical nonempty documents and preserves source/config/tests/task", () => {
  const before = [file("backend/app.py"), file("backend/tests/test_app.py"), file("backend/.env.example"), file("task.md")];
  const after = [...before, ...["README.md", "basic_design.md", "detail_design.md"].map(name => file(name)), file("__pycache__/app.pyc")];
  assert.deepEqual(manualCase5ScopeFailures(before, after), []);
  assert.deepEqual(manualCase5ScopeFailures(before, [...after, file("backend/config.py")]), ["docs-only-scope:backend/config.py"]);
  assert.deepEqual(manualCase5ScopeFailures(before, after.map(row => row.path === "task.md" ? file(row.path, "changed") : row)), ["docs-only-scope:task.md"]);
  assert.deepEqual(manualCase5ScopeFailures(before, after.filter(row => row.path !== "README.md")), ["missing-or-empty-document:README.md"]);
  assert.deepEqual(manualCase5ScopeFailures(before, after.map(row => row.path === "README.md" ? { path: row.path, symbolic_link: true } : row)), ["missing-or-empty-document:README.md"]);
});

test("case5 observation override validates a bounded turn horizon", () => {
  assert.equal(normalizeManualCase5Options(RAW).observationTimeoutMs, 3_600_000);
  assert.equal(normalizeManualCase5Options({ ...RAW, observation_timeout_ms: 7_200_000 }).observationTimeoutMs, 7_200_000);
  for (const value of [0, -1, 7_200_001, 1.5, "7200000", null, true, Number.MAX_SAFE_INTEGER + 1]) {
    assert.throws(() => normalizeManualCase5Options({ ...RAW, observation_timeout_ms: value }), /observation_timeout_ms/);
  }
});

test("case5 reuses physical clean-seed copy while excluding secrets/cache and preserving config examples", async context => {
  const parent = fileURLToPath(new URL("../../../../project_sandbox/manual-st-harness-self-tests/", import.meta.url));
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(path.join(parent, "case5-copy-"));
  context.after(async () => {
    assert.equal(path.dirname(root), parent.replace(/[\\/]$/, ""));
    await rm(root, { recursive: true, force: true });
  });
  const source = path.join(root, "source"), destination = path.join(root, "destination");
  await mkdir(source); await mkdir(destination);
  for (const name of ["backend/app.py", "backend/tests/test_app.py", "backend/.env.example", "backend/.env.production", "frontend/.env", ".git/config", ".moyai/config.toml", "frontend/node_modules/pkg/index.js", "examples/sample.json", "data/sample.csv"]) {
    const target = path.join(source, ...name.split("/"));
    await mkdir(path.dirname(target), { recursive: true });
    await writeFile(target, "synthetic fixture\n", { flag: "wx" });
  }
  const copied = await copyCase52CleanSeed({ source, destination, copyRule: MANUAL_CASE5_COPY_RULE });
  assert.deepEqual(copied.files.map(row => row.path), ["backend/.env.example", "backend/app.py", "backend/tests/test_app.py", "data/sample.csv", "examples/sample.json"]);
  assert.deepEqual(copied.copy_rule, MANUAL_CASE5_COPY_RULE);
  assert.equal(copied.aggregate_sha256, (await inventoryCase52CleanSeed(source, { copyRule: MANUAL_CASE5_COPY_RULE })).aggregate_sha256);
  assert.deepEqual(await readdir(destination), ["backend", "data", "examples", "frontend"]);
  const legacy = await inventoryCase52CleanSeed(source);
  assert.equal(legacy.copy_rule.id, "case5_2-clean-seed.v1");
  assert.equal(legacy.files.some(row => row.path === "backend/.env.production"), true);
  await mkdir(path.join(source, "frontend"), { recursive: true });
  let defaultCase5Config = null;
  for (const [name, expected] of [["case5", 60 * 60 * 1000], ["case5-custom", 120 * 60 * 1000], ["case6", 30 * 60 * 1000]]) {
    const fixture = path.join(root, name);
    await mkdir(fixture);
    const paths = Object.fromEntries(["workspace", "config", "data", "prefs", "webview", "logs"].map(key => [key, path.join(fixture, key)]));
    await Promise.all(Object.values(paths).map(directory => mkdir(directory)));
    paths.config_file = path.join(paths.config, "config.toml"); paths.prefs_file = path.join(paths.prefs, "desktop.toml");
    const records = [];
    const sink = { record: async (...args) => records.push(args), writeJson: async file => ({ path: file }) };
    const isCase5 = name.startsWith("case5");
    const scenario = isCase5 ? createManualCase5Scenario({ ...RAW, fixture_source: source,
      ...(name === "case5-custom" ? { observation_timeout_ms: expected } : {}) }) : createManualCase6Scenario(Object.fromEntries(Object.entries(RAW).filter(([key]) => key !== "fixture_source")));
    await scenario.prepare({ context: { root: fixture, paths }, sink, phase: "prepared" });
    assert.equal(records.find(row => row[0] === `${isCase5 ? "case5" : name}-input`)[1].observation_timeout_ms, expected);
    const configuration = await readFile(paths.config_file, "utf8");
    assert.doesNotMatch(configuration, /request_timeout|stream_idle|temperature|reasoning_effort|observation_timeout/);
    if (name === "case5") defaultCase5Config = configuration;
    if (name === "case5-custom") assert.equal(configuration, defaultCase5Config);
  }
});

import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { DesktopE2eError, classifyExecution } from "../core/execution.mjs";
import { manualLivePrompt } from "../drivers/manual_live_session.mjs";
import { createManualCase7Scenario, normalizeManualCase7Options, manualCase7FixtureConfig,
  manualCase7SourceInventory, prepareManualCase7Workspace, manualCase7ArtifactFailures,
  manualCase7SourceFailures, manualCase7ReadinessDecision } from "../scenarios/manual_case7.mjs";

const TASK_ROOT = fileURLToPath(new URL("../../../../project_sandbox/manual-case7-adapter-selftest-20261003/", import.meta.url));
const RAW = { provider_base_url: "http://provider.invalid/v1", model: "exact-model", python_executable: process.execPath,
  api_key_env: "TASK_LLM_KEY", approval_mode: "operator", fixture_source: path.resolve("fixture"), docling_base_url: "http://docling.invalid:8123" };
const names = Array.from({ length: 15 }, (_, index) => `文書-${index + 1}.docx`).concat("表.xlsx");
const files = names.map(name => ({ path: name, sha256: `hash:${name}`, size_bytes: 7 }));
const markdown = sizes => {
  let offset = 0;
  return sizes.map((size, index) => `## Batch ${index + 1}\n${names.slice(offset, offset += size).map(name => `### ${name}\n説明と要点\n`).join("\n")}`).join("\n");
};

async function withFixture(run) {
  await mkdir(TASK_ROOT, { recursive: true });
  const root = await mkdtemp(path.join(TASK_ROOT, "case7-"));
  const source = path.join(root, "source");
  const workspace = path.join(root, "workspace");
  await mkdir(source); await mkdir(workspace);
  try {
    for (const name of names) await writeFile(path.join(source, name), `fixture bytes ${name}`);
    await run({ source, workspace, root });
  } finally {
    assert.equal(path.dirname(root), path.resolve(TASK_ROOT));
    await rm(root, { recursive: true, force: true });
  }
}

test("Case7 requires explicit physical-source and credential-free Docling options without changing provider inputs", () => {
  const options = normalizeManualCase7Options({ ...RAW, docling_base_url: " http://docling.invalid:8123/api/ " });
  assert.equal(options.doclingBaseUrl, "http://docling.invalid:8123/api");
  assert.equal(options.fixtureSource, RAW.fixture_source);
  assert.equal(options.apiKeyEnv, "TASK_LLM_KEY");
  const config = manualCase7FixtureConfig(options);
  assert.match(config, /\[docling\]\nenabled = true\nbase_url = "http:\/\/docling.invalid:8123\/api"/);
  assert.match(config, /provider_profile = "openai_compatible"/);
  assert.match(config, /api_key_env = "TASK_LLM_KEY"/);
  assert.match(config, /context_window = 131072/);
  assert.match(config, /access_mode = "default"/);
  assert.doesNotMatch(config, /temperature|top_p|reasoning|api_key\s*=|extra_body/);
  for (const override of [{ fixture_source: "relative" }, { docling_base_url: undefined }, { docling_base_url: "file:///tmp/socket" },
    { docling_base_url: "http://user:password@host" }, { docling_base_url: "http://host?key=hidden" }, { docling_base_url: "http://host#fragment" }, { api_key: "hidden" }]) assert.throws(() => normalizeManualCase7Options({ ...RAW, ...override }));
});

test("Case7 keeps the canonical request and common lifecycle with manual quality review pending", async () => {
  const spec = await readFile(new URL("../../manual_ST/case7/spec.md", import.meta.url), "utf8");
  assert.match(manualLivePrompt(spec), /structured document の内容確認には `docling_convert`/);
  const scenario = createManualCase7Scenario(RAW);
  assert.equal(scenario.id, "manual.case7");
  assert.equal(scenario.manualGate, "pending");
  assert.equal(scenario.databaseRequired, true);
  for (const name of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) assert.equal(typeof scenario[name], "function");
});

test("Case7 fresh copy records hashes and preserves all 16 original bytes with docs.md absent", async () => {
  await withFixture(async ({ source, workspace }) => {
    const records = [];
    const sink = { writeJson: async (...args) => records.push(args), record: async (...args) => records.push(args) };
    const options = normalizeManualCase7Options({ ...RAW, fixture_source: source });
    const before = await manualCase7SourceInventory(source);
    const prepared = await prepareManualCase7Workspace({ context: { paths: { workspace } }, sink, options, owner: "self-test:case7", stem: "case7", phase: "prepared" });
    assert.deepEqual(prepared, before);
    assert.deepEqual(await manualCase7SourceInventory(source), before);
    assert.deepEqual(await manualCase7SourceInventory(workspace), before);
    assert.equal((await readdir(workspace)).length, 16);
    assert.equal(records[0][1].docs_md_initially_absent, true);
    assert.equal(records[0][1].original_sources, "read-only");
    assert.equal(records[0][1].target_count, 16);
    assert.deepEqual(records[0][1].type_counts, { docx: 15, xlsx: 1 });
    await assert.rejects(prepareManualCase7Workspace({ context: { paths: { workspace } }, sink, options, owner: "self-test:case7", stem: "case7" }), error => error instanceof DesktopE2eError && error.owner === "harness");
  });
});

test("Case7 common preparation records a thirty-minute observation bound and explicit Docling config", async () => {
  await withFixture(async ({ source, workspace, root }) => {
    const paths = { workspace };
    for (const name of ["config", "data", "prefs", "webview"]) {
      paths[name] = path.join(root, name);
      await mkdir(paths[name]);
    }
    paths.config_file = path.join(paths.config, "config.toml");
    paths.prefs_file = path.join(paths.prefs, "desktop.toml");
    const records = [];
    const sink = { writeJson: async (...args) => records.push(args), record: async (...args) => records.push(args) };
    const scenario = createManualCase7Scenario({ ...RAW, fixture_source: source });
    await scenario.prepare({ context: { root, paths }, sink, phase: "prepared" });
    const evidence = records.find(row => row[0] === "case7-input")[1];
    assert.equal(evidence.observation_timeout_ms, 30 * 60 * 1000);
    assert.equal(evidence.requests.length, 1);
    assert.match(await readFile(paths.config_file, "utf8"), /\[docling\]\nenabled = true\nbase_url = "http:\/\/docling.invalid:8123"/);
    assert.equal((await readdir(workspace)).includes("docs.md"), false);
  });
});

test("Case7 derives target inventory from operator input and blocks only an empty set", async () => {
  await withFixture(async ({ source }) => {
    await rm(path.join(source, "表.xlsx"));
    assert.equal((await manualCase7SourceInventory(source)).length, 15);
    for (const name of names.slice(0, 14)) await rm(path.join(source, name));
    assert.deepEqual((await manualCase7SourceInventory(source)).map(file => file.path), [names[14]]);
    await rm(path.join(source, names[14]));
    await assert.rejects(manualCase7SourceInventory(source), error => error instanceof DesktopE2eError && error.owner === "environment" && error.code === "case7-fixture-empty");
  });
});

test("Case7 artifact requires real per-file headings in sequential batches of at most five", () => {
  assert.deepEqual(manualCase7ArtifactFailures(markdown([5, 5, 5, 1]), files), []);
  assert.deepEqual(manualCase7ArtifactFailures(markdown([5, 5, 5, 1]).replace("### 文書-1.docx", "### `文書-1.docx`").replace("## Batch 1", "## Batch 1: 5ファイル"), files), []);
  assert.ok(manualCase7ArtifactFailures(markdown([6, 5, 5]), files).includes("batch-headings-or-maximum-five-files"));
  assert.ok(manualCase7ArtifactFailures(`\x60\x60\x60md\n${markdown([5, 5, 5, 1])}\n\x60\x60\x60`, files).includes("missing-or-duplicate-file-heading:文書-1.docx"));
  assert.ok(manualCase7ArtifactFailures(`${markdown([5, 5, 5, 1])}\n### 文書-1.docx\n重複`, files).includes("missing-or-duplicate-file-heading:文書-1.docx"));
  assert.ok(manualCase7ArtifactFailures(`### 文書-1.docx\n${markdown([5, 5, 5, 1]).replace("### 文書-1.docx", "")}`, files).includes("file-outside-batch:文書-1.docx"));
  assert.ok(manualCase7ArtifactFailures(markdown([5, 5, 5, 1]).replace("### 表.xlsx", "## 補足\n### 表.xlsx"), files).includes("file-outside-batch:表.xlsx"));
});

test("Case7 compares both copied sources and original source inventory independently of docs output", () => {
  assert.deepEqual(manualCase7SourceFailures(files, [...files, { path: "docs.md", sha256: "output", size_bytes: 20 }], files, files), []);
  const changed = files.map((file, index) => index === 0 ? { ...file, sha256: "changed" } : file);
  assert.deepEqual(manualCase7SourceFailures(files, changed, files, files), [`source-changed:${files[0].path}`]);
  assert.deepEqual(manualCase7SourceFailures(files, files, files, changed), ["original-source-content-changed"]);
  assert.deepEqual(manualCase7SourceFailures(files, files.slice(1), files, files), [`source-changed:${files[0].path}`]);
  assert.deepEqual(manualCase7SourceFailures(files, files, files, { error: "missing directory" }), ["original-source-inventory-changed"]);
});

test("actual readiness distinguishes effective ready, unavailable environment and owner/config mismatch", () => {
  const baseUrl = "http://docling.invalid:8123";
  const target = { workspacePath: "workspace", sessionId: null, configGeneration: "17" };
  const surface = { projection: { config_target: { ...target }, config_fields: [{ key: "docling.enabled", value: "true" }, { key: "docling.base_url", value: baseUrl }],
    docling_readiness: { status: "ready", endpoint: `${baseUrl}/ready`, httpStatus: 204, message: "HTTP 204" }, pending_async_operations: [] },
    settings: { docling_readiness: { status: "ready", aria_busy: "false", status_count: 1, status_visible: true, button: { count: 1, visible: true, enabled: true } } } };
  assert.equal(manualCase7ReadinessDecision(surface, target, baseUrl), "ready");
  assert.equal(manualCase7ReadinessDecision(surface, { ...target, configGeneration: "18" }, baseUrl), "mismatch");
  for (const status of [503, null]) {
    const unavailable = structuredClone(surface);
    Object.assign(unavailable.projection.docling_readiness, { status: "unavailable", httpStatus: status });
    assert.equal(manualCase7ReadinessDecision(unavailable, target, baseUrl), "environment_blocked");
  }
  const checking = structuredClone(surface); checking.projection.docling_readiness.status = "checking";
  assert.equal(manualCase7ReadinessDecision(checking, target, baseUrl), "pending");
  const wrong = structuredClone(surface); wrong.projection.docling_readiness.endpoint = "http://different.invalid/ready";
  assert.equal(manualCase7ReadinessDecision(wrong, target, baseUrl), "mismatch");
  const inconsistent = structuredClone(surface); inconsistent.projection.docling_readiness.httpStatus = 503;
  assert.equal(manualCase7ReadinessDecision(inconsistent, target, baseUrl), "mismatch");
  assert.equal(manualCase7ReadinessDecision({ ...surface, visible_fatal_count: 1 }, target, baseUrl), "mismatch");
  assert.equal(classifyExecution({ preflight: "blocked", acquisition: "not_run", oracle: "not_run", manual: "not_run", cleanup: "pass" }).classification, "environment_blocked");
});

import path from "node:path";
import { fileURLToPath } from "node:url";
import { lstat, writeFile } from "node:fs/promises";
import { DesktopE2eError } from "../core/execution.mjs";
import { CASE5_2_CLEAN_SEED_COPY_RULE, copyCase52CleanSeed, inventoryCase52CleanSeed } from "../core/clean_seed.mjs";
import { createManualTextCase } from "./manual_text_case.mjs";
import { normalizeManualLiveOptions, manualLiveSection, manualLiveManifestDiff } from "../drivers/manual_live_session.mjs";

const OUTPUTS = ["README.md", "basic_design.md", "detail_design.md"];
const SPEC = fileURLToPath(new URL("../../manual_ST/case5/spec.md", import.meta.url));
export const MANUAL_CASE5_COPY_RULE = Object.freeze({ ...CASE5_2_CLEAN_SEED_COPY_RULE,
  id: "case5-clean-seed.v1", excluded_directory_names: Object.freeze([...CASE5_2_CLEAN_SEED_COPY_RULE.excluded_directory_names, ".git", ".moyai"]),
  excluded_file_names: Object.freeze([...CASE5_2_CLEAN_SEED_COPY_RULE.excluded_file_names, ".git"]),
  excluded_file_prefixes: Object.freeze([".env."]), preserved_file_names: Object.freeze([".env.example"]),
});

export function normalizeManualCase5Options(raw) {
  const { fixture_source, ...connection } = raw;
  if (typeof fixture_source !== "string" || !path.isAbsolute(fixture_source) || fixture_source.includes("\0")) throw new TypeError("case5 fixture_source must be an absolute repository fixture path");
  return Object.freeze({ ...normalizeManualLiveOptions(connection), fixtureSource: path.resolve(fixture_source) });
}
export function manualCase5Input(spec) { return { prompt: manualLiveSection(spec, "Canonical user request"), task: manualLiveSection(spec, "Canonical task.md", "markdown") }; }
export function manualCase5ScopeFailures(before, after) {
  const failures = manualLiveManifestDiff(before, after).filter(name => !OUTPUTS.includes(name)).map(name => `docs-only-scope:${name}`);
  for (const name of OUTPUTS) if (!after.some(item => item.path === name && item.sha256 && item.size_bytes > 0 && !item.symbolic_link)) failures.push(`missing-or-empty-document:${name}`);
  return failures;
}

export function createManualCase5Scenario(raw = {}) {
  const options = normalizeManualCase5Options(raw);
  let seed = null;
  return createManualTextCase({ id: "manual.case5", options, specPath: SPEC, outputs: OUTPUTS,
    observationTimeoutMs: 30 * 60 * 1000,
    stages: spec => [{ name: "stage1", prompt: manualCase5Input(spec).prompt }],
    async prepareWorkspace({ context, sink, options, spec, phase, owner }) {
      for (const name of ["backend", "frontend", "examples", "data"]) {
        const item = await lstat(path.join(options.fixtureSource, name));
        if (!item.isDirectory() || item.isSymbolicLink()) throw new DesktopE2eError("environment", "case5-fixture-layout", "repository fixture lacks a physical required directory", { name });
      }
      seed = await copyCase52CleanSeed({ source: options.fixtureSource, destination: context.paths.workspace, copyRule: MANUAL_CASE5_COPY_RULE });
      if (!seed.files.some(row => /(^|\/)tests\//.test(row.path)) || !seed.files.some(row => /(^|\/)(?:config[^/]*|pyproject\.toml|package\.json|\.env\.example)$/.test(row.path))) throw new DesktopE2eError("environment", "case5-fixture-tests-config", "repository fixture must retain actual tests and configuration evidence", {});
      await writeFile(path.join(context.paths.workspace, "task.md"), manualCase5Input(spec).task, { encoding: "utf8", flag: "wx" });
      const evidence = await sink.writeJson("case5/fixture-copy.json", seed);
      await sink.record("case5-fixture-copy", { evidence, source: seed.source, aggregate_sha256: seed.aggregate_sha256, file_count: seed.file_count }, { phase, owner });
    },
    async checkStage({ context, row, baseline, generated, options }) {
      const failures = manualCase5ScopeFailures(baseline, generated);
      const current = await inventoryCase52CleanSeed(context.paths.workspace, { copyRule: MANUAL_CASE5_COPY_RULE });
      const convert = inventory => inventory.files.map(item => ({ path: item.path, sha256: item.sha256, size_bytes: item.bytes }));
      failures.push(...manualLiveManifestDiff(convert(seed), convert(current)).filter(name => !OUTPUTS.includes(name)).map(name => `source-config-test-scope:${name}`));
      const source = await inventoryCase52CleanSeed(options.fixtureSource, { copyRule: MANUAL_CASE5_COPY_RULE });
      row.fixture_source_unchanged = source.aggregate_sha256 === seed.aggregate_sha256;
      if (!row.fixture_source_unchanged) failures.push("fixture-source-changed");
      return failures;
    }, manualReview: ["Spot-check claims in all three documents against concrete source, config, tests and samples; confirm coverage, unknowns and cross-document consistency.",
      "Read public tool/transcript evidence for focus, workspace scope and long-context continuity. If compaction threshold was reached, review request diagnostics and canonical compaction lineage; absent diagnostics remain unverified."] });
}

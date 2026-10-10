import path from "node:path";
import { fileURLToPath } from "node:url";
import { constants } from "node:fs";
import { copyFile, lstat } from "node:fs/promises";
import { DesktopE2eError } from "../core/execution.mjs";
import { createManualTextCase } from "./manual_text_case.mjs";
import { normalizeManualLiveOptions, manualLiveSection, manualLiveManifestDiff,
  manualLiveExternalProcess, manualLiveUnittestResult } from "../drivers/manual_live_session.mjs";

const OUTPUTS = ["calculator.py", "test_calculator.py", "docs/calculator-design.md"];
const SPEC = fileURLToPath(new URL("../../manual_ST/case3/spec.md", import.meta.url));

export function normalizeManualCase3Options(raw) {
  const { fixture_source, ...connection } = raw;
  if (typeof fixture_source !== "string" || !path.isAbsolute(fixture_source) || fixture_source.includes("\0")) throw new TypeError("case3 fixture_source must identify the accepted Case1 workspace by absolute path");
  return Object.freeze({ ...normalizeManualLiveOptions(connection), fixtureSource: path.resolve(fixture_source) });
}

export function manualCase3Stages(spec) { return [1, 2, 3].map(number => ({ name: `stage${number}`, prompt: manualLiveSection(spec, `Stage ${number} request`) })); }
export function manualCase3DocsOnlyFailures(before, after, stage) {
  return manualLiveManifestDiff(before, after).filter(name => name !== "docs/calculator-design.md").map(name => `${stage}:docs-only-scope:${name}`);
}
export function manualCase3CliResult(result, stdout, stderr, { exit, suffix = null }) {
  const lastLine = stdout.trimEnd().split(/\r?\n/).at(-1);
  return result?.outcome?.root_exit_code === exit && (suffix === null
    ? /usage|使用方法|使い方/i.test(stderr)
    : lastLine === suffix || lastLine?.endsWith(` ${suffix}`) || lastLine?.endsWith(`=${suffix}`) || lastLine?.endsWith(`: ${suffix}`));
}

export function createManualCase3Scenario(raw = {}) {
  const options = normalizeManualCase3Options(raw);
  return createManualTextCase({ id: "manual.case3", options, specPath: SPEC, stages: manualCase3Stages, outputs: OUTPUTS,
    async prepareWorkspace({ context, sink, options, owner, stem, phase }) {
      for (const name of OUTPUTS.slice(0, 2)) {
        const source = path.join(options.fixtureSource, name);
        const item = await lstat(source);
        if (!item.isFile() || item.isSymbolicLink()) throw new DesktopE2eError("environment", "case3-invalid-fixture", "Case1 baseline must contain physical source/test files", { source });
        await copyFile(source, path.join(context.paths.workspace, name), constants.COPYFILE_EXCL);
      }
      const result = await manualLiveExternalProcess({ context, sink, options, owner, stem, phase, label: "case3-baseline-unittest", args: ["-m", "unittest"] });
      if (!manualLiveUnittestResult(result.result, result.stdout, result.stderr).pass) throw new DesktopE2eError("environment", "case3-baseline-unittest", "accepted Case1 fixture does not pass its existing tests", {});
    },
    async checkStage({ context, sink, options, owner, stem, step, row, previous, generated, observationBudget }) {
      const failures = step.name === "stage3" ? [] : manualCase3DocsOnlyFailures(previous, generated, step.name);
      if (!generated.some(item => item.path === "docs/calculator-design.md")) failures.push(`${step.name}:missing-design`);
      if (failures.length) return failures;
      if (step.name !== "stage2") {
        const result = await manualLiveExternalProcess({ context, sink, options, owner, stem, label: `case3-${step.name}-unittest`, args: ["-m", "unittest"], observationBudget });
        row.external_unittest = manualLiveUnittestResult(result.result, result.stdout, result.stderr);
        if (!row.external_unittest.pass) return [`${step.name}:external-unittest-failed-or-empty`];
      }
      if (step.name === "stage3") {
        row.external_cli = [];
        for (const [argv, expected] of [ [["2", "+", "3"], { exit: 0, suffix: "5" }], [["2", "pow", "3"], { exit: 0, suffix: "8" }],
          [["sin", "0"], { exit: 0, suffix: "0" }], [["cos", "0"], { exit: 0, suffix: "1" }], [["sqrt", "16"], { exit: 0, suffix: "4" }],
          [["8", "+"], { exit: 1 }], [["log", "10"], { exit: 1 }] ]) {
          const result = await manualLiveExternalProcess({ context, sink, options, owner, stem, label: `case3-cli-${row.external_cli.length + 1}`, args: ["-X", "utf8", "calculator.py", ...argv], observationBudget });
          const pass = manualCase3CliResult(result.result, result.stdout, result.stderr, expected);
          row.external_cli.push({ argv, expected, pass, exit_code: result.result.outcome.root_exit_code });
          if (!pass) return ["stage3:external-cli-contract-failed"];
        }
      }
      return [];
    }, manualReview: ["Read all three turns and document/source/test changes; confirm docs-only stages and the existing API argument meaning were preserved.",
      "Confirm agent-side verification, design/code/test agreement and workspace scope from public transcript evidence; missing exact tool or request detail remains unverified."] });
}

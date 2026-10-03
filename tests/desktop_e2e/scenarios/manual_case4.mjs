import path from "node:path";
import { fileURLToPath } from "node:url";
import { writeFile } from "node:fs/promises";
import { createManualTextCase } from "./manual_text_case.mjs";
import { normalizeManualLiveOptions, manualLivePrompt, manualLiveSection,
  manualLiveExternalProcess, manualLiveUnittestResult } from "../drivers/manual_live_session.mjs";

const SPEC = fileURLToPath(new URL("../../manual_ST/case4/spec.md", import.meta.url));
const OUTPUTS = ["calculator.py", "design.md", "scientific_calculator.py", "test_calculator.py", "test_integration.py"];
export function manualCase4Input(spec) { return { prompt: manualLivePrompt(spec), task: manualLiveSection(spec, "Canonical task.md", "markdown") }; }
export function createManualCase4Scenario(raw = {}) {
  const options = normalizeManualLiveOptions(raw);
  return createManualTextCase({ id: "manual.case4", options, specPath: SPEC,
    stages: spec => [{ name: "stage1", prompt: manualCase4Input(spec).prompt }], outputs: OUTPUTS,
    async prepareWorkspace({ context, spec }) { await writeFile(path.join(context.paths.workspace, "task.md"), manualCase4Input(spec).task, { flag: "wx" }); },
    async checkStage({ context, sink, options, owner, stem, row, generated }) {
      const missing = OUTPUTS.filter(name => !generated.some(item => item.path === name));
      if (missing.length) return missing.map(name => `missing-${name}`);
      row.external_verification = [];
      for (const [label, args] of [["case4-unittest", ["-m", "unittest"]], ["case4-integration", ["-m", "unittest", "test_integration", "-v"]]]) {
        const result = await manualLiveExternalProcess({ context, sink, options, owner, stem, label, args });
        const verification = manualLiveUnittestResult(result.result, result.stdout, result.stderr);
        row.external_verification.push({ label, ...verification });
        if (!verification.pass) return [`${label}-failed-or-empty`];
      }
      return [];
    }, manualReview: ["Read intermediate tool/file-change evidence for Step1–5 order and compare the design, basic/scientific implementation and tests.",
      "Confirm both agent-side verification commands and workspace scope from public evidence; external tests alone do not prove the whole task."] });
}

import crypto from "node:crypto";
import path from "node:path";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { classifyExecution } from "./core/execution.mjs";
import { runCli, freshExecutionId } from "./run_scenario.mjs";

const repository = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const smoke = Object.freeze([
  "input.pointer-keyboard", "input.command-palette-insertion", "prompt-review.raw-interaction",
  "settings.mcp-peer-controls", "settings.preferences-config", "run.stop", "run.next-turn",
]);
export const GUI_SUITES = Object.freeze({
  smoke,
  regression: Object.freeze([...smoke,
    "navigation.workspace-controls", "navigation.modal-keyboard-controls", "navigation.external-sidebar-stop",
    "settings.initial-field-controls", "settings.session-field-controls", "settings.temporary-apply-controls",
    "side-chat.session", "provider.chat-tool-continuation", "provider.responses-progress",
  ]),
});

export function parseSuiteArguments(argv) {
  const result = { suite: "smoke", automationOnly: false, list: false };
  const values = new Map([["--suite", "suite"], ["--binary", "binary"], ["--artifact-parent", "artifactParent"]]);
  const flags = new Map([["--automation-only", "automationOnly"], ["--list", "list"]]);
  const seen = new Set();
  for (let i = 0; i < argv.length; i++) {
    const name = argv[i];
    if (seen.has(name)) throw new TypeError(`duplicate option: ${name}`);
    seen.add(name);
    if (flags.has(name)) result[flags.get(name)] = true;
    else if (values.has(name)) {
      const value = argv[++i];
      if (!value || value.startsWith("--")) throw new TypeError(`missing value: ${name}`);
      result[values.get(name)] = value;
    } else throw new TypeError(`unknown option: ${name}`);
  }
  if (!Object.hasOwn(GUI_SUITES, result.suite)) throw new TypeError(`unknown GUI suite: ${result.suite}`);
  return result;
}

// Only documented classification/identity data leaves the local execution tree.
// In particular, diagnostics.message/evidence, DOM, config, token and paths do not.
export function summarizeExecution(result, manifest) {
  const classified = classifyExecution(result.inputs);
  if (classified.classification !== result.classification) throw new Error("inconsistent sealed classification");
  if (manifest.scenario_id !== result.scenario_id || manifest.execution_id !== result.execution_id) {
    throw new Error("sealed execution identity mismatch");
  }
  return {
    scenario: result.scenario_id,
    execution_id: result.execution_id,
    classification: result.classification,
    automation: ["pass", "manual_pending"].includes(result.classification) ? "pass" : "fail",
    manual: result.inputs.manual,
    inputs: classified.inputs,
    elapsed_ms: result.elapsed_ms,
    binary_sha256: manifest.binary.sha256,
    harness_sha256: manifest.harness.tree_sha256,
    diagnostic_codes: (result.diagnostics ?? []).map(row => row.code)
      .filter(code => typeof code === "string" && /^[a-z0-9][a-z0-9._-]{0,127}$/.test(code)),
  };
}

export async function readSealedExecution(outcome) {
  const root = path.resolve(outcome.execution_root);
  const bytes = await readFile(path.join(root, "evidence/result.json"));
  const seal = JSON.parse(await readFile(path.join(root, "evidence/seal.json"), "utf8"));
  const digest = crypto.createHash("sha256").update(bytes).digest("hex");
  if (seal.result_sha256 !== digest) throw new Error("sealed result hash mismatch");
  const result = JSON.parse(bytes);
  const manifest = JSON.parse(await readFile(path.join(root, "evidence/execution.json"), "utf8"));
  const manifestSeal = seal.files.find(row => row.relative_path === "execution.json");
  const manifestBytes = await readFile(path.join(root, "evidence/execution.json"));
  if (manifestSeal?.sha256 !== crypto.createHash("sha256").update(manifestBytes).digest("hex")) {
    throw new Error("sealed manifest hash mismatch");
  }
  return summarizeExecution(result, manifest);
}

export function suiteDecision(rows, expected, automationOnly = false) {
  const complete = expected.length > 0 && rows.length === expected.length
    && expected.every((id, index) => rows[index]?.scenario === id);
  const automationPassed = complete && rows.every(row => row.automation === "pass");
  const reviewRequired = rows.some(row => row.manual === "pending" || row.manual === "not_run");
  const status = !automationPassed ? "failed" : reviewRequired ? "review_required" : "pass";
  const blocked = rows.some(row => row.classification === "environment_blocked");
  return {
    status, automation: automationPassed ? "pass" : "fail",
    manual_review: reviewRequired ? "pending" : "not_required_by_selected_cases",
    expected_cases: expected.length, executed_cases: rows.length,
    passed_automation_cases: rows.filter(row => row.automation === "pass").length,
    pending_manual_cases: rows.filter(row => ["pending", "not_run"].includes(row.manual)).length,
    exit_code: !automationPassed ? (blocked ? 2 : 1) : reviewRequired && !automationOnly ? 3 : 0,
  };
}

export async function executeSuiteCases({ ids, binary, artifactParent, execute = runCli, readResult = readSealedExecution, onCase = () => {} }) {
  const rows = [];
  for (const id of ids) {
    const outcome = await execute(["--scenario", id, "--binary", binary, "--artifact-parent", artifactParent]);
    const row = await readResult(outcome);
    if (row.scenario !== id) throw new Error("runner returned a different scenario");
    rows.push(row);
    await onCase(row, rows);
    // Never start a second GUI on a failed/uncertain host. The common runner is
    // still the sole process/profile/SQLite/admission cleanup owner.
    if (row.automation !== "pass") break;
  }
  return rows;
}

export async function runSuite(options = {}, { execute, readResult, output = console.log } = {}) {
  const suite = options.suite ?? "smoke";
  if (!Object.hasOwn(GUI_SUITES, suite)) throw new TypeError(`unknown GUI suite: ${suite}`);
  const ids = GUI_SUITES[suite];
  if (options.list) {
    output(JSON.stringify({ suite, cases: ids, scope: "Actual Tauri GUI automation; no live LLM or physical Hub peer." }, null, 2));
    return { exitCode: 0, root: null };
  }
  const binary = path.resolve(options.binary ?? path.join(repository, "target/debug/moyai-desktop.exe"));
  const parent = path.resolve(options.artifactParent ?? path.join(repository, "../project_sandbox/desktop-gui"));
  await mkdir(parent, { recursive: true });
  const root = path.join(parent, `suite-${freshExecutionId()}`);
  await mkdir(root);
  const started = new Date().toISOString();
  const rows = [];
  let failure = null;
  const report = async () => {
    const decision = suiteDecision(rows, ids, options.automationOnly === true);
    if (failure !== null) Object.assign(decision, { status: "failed", automation: "fail", exit_code: 1 });
    const summary = {
      schema_version: "desktop-gui-suite.v1", product: "moyai-desktop", suite,
      phase: "actual_gui_automation", started_at: started, updated_at: new Date().toISOString(),
      build: "prebuilt_binary; source-to-binary correspondence is established only by verify:gui build stages",
      automation_only_requested: options.automationOnly === true,
      ...decision, failure_code: failure, cases: rows,
      visual_review: "Not performed by this command; per-case manual verdicts remain unchanged.",
      outside_scope: ["physical peers", "live model quality", "IME and OS-specific manual acceptance", "release qualification"],
    };
    await writeFile(path.join(root, "public-summary.json"), `${JSON.stringify(summary, null, 2)}\n`);
    const lines = ["# Desktop GUI automation", "", `Suite: ${suite}`, `Status: ${summary.status}`,
      `Automation: ${summary.automation}; manual review: ${summary.manual_review}`, "",
      "| Scenario | Sealed result | Automation | Manual | Seconds |", "|---|---|---|---|---:|",
      ...rows.map(row => `| ${row.scenario} | ${row.classification} | ${row.automation} | ${row.manual} | ${(row.elapsed_ms / 1000).toFixed(1)} |`),
      "", "This report does not replace manual visual/IME/OS or release acceptance. Original sealed evidence stays under each execution directory.", ""];
    await writeFile(path.join(root, "RESULTS.md"), lines.join("\n"));
    return summary;
  };
  await report();
  try {
    await executeSuiteCases({ ids, binary, artifactParent: root, execute, readResult,
      onCase: async row => { rows.push(row); await report(); output(`${row.scenario}: automation=${row.automation}, sealed=${row.classification}, manual=${row.manual}`); },
    });
  } catch (error) {
    failure = "suite-execution-error";
    // Keep arbitrary exception text local; CI uploads only public-summary.json.
    await writeFile(path.join(root, "runner-error.log"), `${error.stack ?? error}\n`);
  }
  const summary = await report();
  output(`GUI suite: ${summary.status}; automation ${summary.passed_automation_cases}/${summary.expected_cases}; manual pending ${summary.pending_manual_cases}`);
  output(`Report: ${path.join(root, "RESULTS.md")}`);
  return { exitCode: summary.exit_code, root, summary };
}

if (path.resolve(process.argv[1] ?? "") === fileURLToPath(import.meta.url)) {
  try { process.exitCode = (await runSuite(parseSuiteArguments(process.argv.slice(2)))).exitCode; }
  catch { console.error("GUI suite could not start. Check options, dependencies and artifact directory."); process.exitCode = 1; }
}

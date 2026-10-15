import path from "node:path";
import { spawn } from "node:child_process";
import { mkdir, readFile, writeFile, open } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { parseSuiteArguments, runSuite } from "../tests/desktop_e2e/run_suite.mjs";
import { freshExecutionId } from "../tests/desktop_e2e/run_scenario.mjs";

const repository = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
export const GUI_CHECK_STAGES = Object.freeze([
  { name: "frontend-unit", tool: "npm", args: ["run", "test:desktop-web"] },
  { name: "harness-self-tests", tool: "npm", args: ["run", "test:desktop-e2e-harness"] },
  { name: "frontend-build", tool: "npm", args: ["run", "build:desktop-web"] },
  { name: "rust-build", tool: "cargo", args: ["build", "--locked", "--bins", "--features", "tauri-desktop", "--message-format=json-render-diagnostics"] },
]);

export function desktopCompilerArtifact(log) {
  const executables = new Set();
  for (const line of log.split(/\r?\n/)) {
    let message;
    try { message = JSON.parse(line); } catch { continue; }
    if (message.reason === "compiler-artifact" && message.target?.name === "moyai-desktop"
      && message.target.kind?.includes("bin") && typeof message.executable === "string") {
      if (!path.isAbsolute(message.executable)) throw new Error("Cargo executable is not absolute");
      executables.add(path.normalize(message.executable));
    }
  }
  if (executables.size !== 1) throw new Error("Cargo did not report exactly one Desktop executable");
  return [...executables][0];
}

async function runStage(stage, root) {
  const log = await open(path.join(root, `${stage.name}.log`), "wx");
  try {
    let command = stage.tool;
    let args = stage.args;
    if (command === "npm") {
      if (!process.env.npm_execpath) throw new Error("Run via npm run verify:gui so the current npm CLI is reused.");
      command = process.execPath;
      args = [process.env.npm_execpath, ...args];
    }
    const exitCode = await new Promise((resolve, reject) => {
      const child = spawn(command, args, { cwd: repository, windowsHide: true, shell: false, stdio: ["ignore", log.fd, log.fd] });
      child.once("error", reject);
      child.once("close", code => resolve(Number.isInteger(code) ? code : 1));
    });
    await log.sync();
    return { exitCode, binary: stage.name === "rust-build" && exitCode === 0
      ? desktopCompilerArtifact(await readFile(path.join(root, `${stage.name}.log`), "utf8")) : null };
  } finally { await log.close(); }
}

export async function verifyGui(options, { run = runStage, gui = runSuite, output = console.log, artifactRoot } = {}) {
  if (options.binary) throw new TypeError("verify:gui builds its own binary; use test:gui for a prebuilt binary");
  if (options.list) return gui(options, { output });
  const parent = path.resolve(options.artifactParent ?? path.join(repository, "../project_sandbox/desktop-gui"));
  await mkdir(parent, { recursive: true });
  const root = artifactRoot ?? path.join(parent, `checks-${freshExecutionId()}`);
  await mkdir(root);
  const report = { schema_version: "desktop-gui-checks.v1", product: "moyai-desktop", suite: options.suite ?? "smoke",
    phase: "unit-build-gui", status: "running", started_at: new Date().toISOString(), stages: [],
    counts_are_separate: "Unit and harness self-tests are not product GUI case counts.", visual_review: "not performed" };
  const save = () => writeFile(path.join(root, "public-summary.json"), `${JSON.stringify(report, null, 2)}\n`);
  await save();
  let exitCode = 1;
  let binary = null;
  try {
    for (const stage of GUI_CHECK_STAGES) {
      report.active_stage = stage.name;
      output(`GUI check: ${stage.name}`);
      const start = Date.now();
      const result = await run(stage, root);
      const code = result.exitCode;
      report.stages.push({ name: stage.name, status: code === 0 ? "pass" : "fail", exit_code: code, elapsed_ms: Date.now() - start });
      await save();
      if (code !== 0) { report.status = "failed"; return { exitCode: 1, root, report }; }
      if (stage.name === "rust-build") {
        if (typeof result.binary !== "string" || !path.isAbsolute(result.binary)) throw new Error("Missing built Desktop artifact");
        binary = result.binary;
      }
    }
    output("GUI check: actual Tauri scenarios");
    report.active_stage = "actual-gui";
    const start = Date.now();
    const outcome = await gui({ ...options, binary, artifactParent: root }, { output });
    report.stages.push({ name: "actual-gui", status: outcome.summary.status, exit_code: outcome.exitCode, elapsed_ms: Date.now() - start });
    report.gui = { status: outcome.summary.status, automation: outcome.summary.automation,
      executed_cases: outcome.summary.executed_cases, expected_cases: outcome.summary.expected_cases,
      pending_manual_cases: outcome.summary.pending_manual_cases };
    report.status = outcome.summary.status;
    exitCode = outcome.exitCode;
    return { exitCode, root, report };
  } catch (error) {
    report.status = "failed";
    report.failure_code = "verification-stage-error";
    await writeFile(path.join(root, "runner-error.log"), `${error.stack ?? error}\n`);
    return { exitCode: 1, root, report };
  } finally {
    report.finished_at = new Date().toISOString();
    await save();
    output(`GUI check: ${report.status}. Public report: ${path.join(root, "public-summary.json")}`);
  }
}

if (path.resolve(process.argv[1] ?? "") === fileURLToPath(import.meta.url)) {
  try { process.exitCode = (await verifyGui(parseSuiteArguments(process.argv.slice(2)))).exitCode; }
  catch { console.error("GUI checks could not start. Run npm run verify:gui with a supported suite."); process.exitCode = 1; }
}

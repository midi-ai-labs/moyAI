import crypto from "node:crypto";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

import { executeDesktopScenario } from "./core/desktop_execution.mjs";
import { createDesktopRunContext } from "./core/run_context.mjs";
import { WindowsTauriHost } from "./drivers/windows_tauri_host.mjs";
import { createScenario, scenarioIds } from "./scenario_registry.mjs";

const harnessRoot = path.dirname(fileURLToPath(import.meta.url));
const ALLOWED_ARGUMENTS = new Set(["binary", "artifact-parent", "execution-id", "scenario"]);

export function parseArguments(argv) {
  const result = {};
  for (let index = 0; index < argv.length; index += 1) {
    const token = argv[index];
    if (!token.startsWith("--")) throw new TypeError(`unexpected argument: ${token}`);
    const name = token.slice(2);
    if (!ALLOWED_ARGUMENTS.has(name)) throw new TypeError(`unknown argument: --${name}`);
    if (Object.hasOwn(result, name)) throw new TypeError(`duplicate argument: --${name}`);
    const value = argv[index + 1];
    if (!value || value.startsWith("--")) throw new TypeError(`missing value for --${name}`);
    result[name] = value;
    index += 1;
  }
  return result;
}

export function freshExecutionId() {
  const stamp = new Date().toISOString().replace(/[-:]/g, "").replace(/\.\d{3}Z$/, "z").toLowerCase();
  return `e2e-${stamp}-${crypto.randomUUID().slice(0, 8)}`;
}

export async function runCli(argv = process.argv.slice(2)) {
  const args = parseArguments(argv);
  if (!args.binary) throw new TypeError("--binary is required");
  if (!args["artifact-parent"]) throw new TypeError("--artifact-parent is required");
  const scenario = createScenario(args.scenario ?? "shell.baseline");
  const prepared = await createDesktopRunContext({
    artifactParent: args["artifact-parent"],
    binary: args.binary,
    executionId: args["execution-id"] ?? freshExecutionId(),
    scenarioId: scenario.id,
    harnessRoot,
  });
  return executeDesktopScenario({
    context: prepared.context,
    scenario,
    host: new WindowsTauriHost(),
    sink: prepared.sink,
  });
}

if (path.resolve(process.argv[1] ?? "") === fileURLToPath(import.meta.url)) {
  const outcome = await runCli();
  process.stdout.write(`${JSON.stringify(outcome, null, 2)}\n`);
  process.exitCode = outcome.result.classification === "pass"
    ? 0
    : outcome.result.classification === "environment_blocked"
      ? 2
      : 1;
}

export { scenarioIds };

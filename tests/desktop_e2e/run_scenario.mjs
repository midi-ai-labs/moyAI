import crypto from "node:crypto";
import path from "node:path";
import process from "node:process";
import { readFile, stat } from "node:fs/promises";
import { fileURLToPath } from "node:url";

import { executeDesktopScenario } from "./core/desktop_execution.mjs";
import { createDesktopRunContext } from "./core/run_context.mjs";
import { WindowsTauriHost } from "./drivers/windows_tauri_host.mjs";
import { createScenario, scenarioIds } from "./scenario_registry.mjs";

const harnessRoot = path.dirname(fileURLToPath(import.meta.url));
const ALLOWED_ARGUMENTS = new Set(["binary", "artifact-parent", "execution-id", "scenario", "scenario-config"]);

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

export async function readScenarioConfig(candidate) {
  if (candidate === undefined) return { options: {}, identity: null };
  const exact = path.resolve(candidate);
  const item = await stat(exact);
  if (!item.isFile()) throw new TypeError(`scenario config is not a file: ${exact}`);
  const bytes = await readFile(exact);
  let options;
  try { options = JSON.parse(bytes.toString("utf8")); }
  catch (error) { throw new TypeError(`scenario config is not valid JSON: ${error.message}`); }
  if (options === null || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("scenario config must contain one JSON object");
  }
  return {
    options,
    identity: {
      path: exact,
      sha256: crypto.createHash("sha256").update(bytes).digest("hex"),
      size_bytes: bytes.byteLength,
    },
  };
}

export async function runCli(argv = process.argv.slice(2)) {
  const args = parseArguments(argv);
  if (!args.binary) throw new TypeError("--binary is required");
  if (!args["artifact-parent"]) throw new TypeError("--artifact-parent is required");
  const scenarioConfig = await readScenarioConfig(args["scenario-config"]);
  const scenario = createScenario(args.scenario ?? "shell.baseline", scenarioConfig.options);
  const prepared = await createDesktopRunContext({
    artifactParent: args["artifact-parent"],
    binary: args.binary,
    executionId: args["execution-id"] ?? freshExecutionId(),
    scenarioId: scenario.id,
    scenarioConfig: scenarioConfig.identity,
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

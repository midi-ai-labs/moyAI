import path from "node:path";
import { readFile, stat } from "node:fs/promises";
import { createHash } from "node:crypto";
import { createInterface } from "node:readline";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createDesktopRunContext, createCompanionContext } from "./core/run_context.mjs";
import { executeDesktopScenario } from "./core/desktop_execution.mjs";
import { DesktopE2eError } from "./core/execution.mjs";
import { WindowsTauriHost } from "./drivers/windows_tauri_host.mjs";
import { prepareDesktopFixture } from "./scenarios/fixture.mjs";
import { requestGracefulExit } from "./scenarios/shell_baseline.mjs";
import { startSharedWorkflowProvider } from "./drivers/shared_work_runner_fixture.mjs";
import { freshExecutionId } from "./run_scenario.mjs";

const directory = path.dirname(fileURLToPath(import.meta.url));
const workspace = path.resolve(directory, "../../..");
const ID = "manual.shared-work-isolation";
const ARGUMENTS = ["binary", "hub-binary", "runner-binary", "runner-test-binary", "artifact-parent"];
const MAX_COMMAND_BYTES = 65_536;

export function parseManualArguments(argv) {
  const options = {};
  for (let index = 0; index < argv.length; index += 2) {
    const key = argv[index]?.slice(2), value = argv[index + 1];
    if (!argv[index]?.startsWith("--") || !ARGUMENTS.includes(key) || Object.hasOwn(options, key)) throw new TypeError("Unknown or duplicate manual-session option");
    if (typeof value !== "string" || !path.isAbsolute(value)) throw new TypeError("Manual-session paths must be explicit and absolute");
    options[key] = path.resolve(value);
  }
  if (ARGUMENTS.some(key => !options[key])) throw new TypeError("All five manual-session paths are required");
  return options;
}

export function parseManualCommand(line) {
  if (typeof line !== "string" || Buffer.byteLength(line, "utf8") > MAX_COMMAND_BYTES) throw new TypeError("Manual command exceeds its bound");
  let value;
  try { value = JSON.parse(line); } catch { throw new TypeError("Manual command must be JSON"); }
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new TypeError("Manual command must be an object");
  const exact = keys => Object.keys(value).length === keys.length && keys.every(key => Object.hasOwn(value, key));
  if (value.command === "start-b" && exact(["command"])) return value;
  if (value.command === "capture-runner" && exact(["command", "pc"]) && ["a", "b"].includes(value.pc)) return value;
  if (value.command === "finish" && exact(["command", "verdict", "observations"])
    && ["pass", "fail", "pending"].includes(value.verdict) && Array.isArray(value.observations)
    && value.observations.length <= 128 && value.observations.every(row => typeof row === "string" && row.length > 0 && row.length <= 4096)
    && (value.verdict !== "pass" || value.observations.length > 0)) return value;
  throw new TypeError("Unknown manual command or invalid fields");
}

export async function processManualCommands(lines, { startB, captureRunner, recordFinish }) {
  for await (const line of lines) {
    const command = parseManualCommand(line);
    if (command.command === "start-b") await startB();
    else if (command.command === "capture-runner") await captureRunner(command.pc);
    else {
      await recordFinish(command);
      return { acquisition: "pass", oracle: "not_required", manual: command.verdict };
    }
  }
  throw new DesktopE2eError("harness", "manual-session-input-ended", "Manual session input ended before an explicit finish");
}

function pcSummary(pc) {
  return { pc: pc.name, runtime: pc.runtime, paths: pc.context.paths,
    execution_root: pc.context.root, suggested_execution_root: pc.context.paths.workspace };
}

export async function runManualSession(options, { input = process.stdin, output = value => process.stdout.write(`${JSON.stringify(value)}\n`) } = {}) {
  for (const name of ARGUMENTS.filter(key => key !== "artifact-parent")) {
    if (!(await stat(options[name])).isFile()) throw new TypeError("Manual-session binary is not a file");
  }
  const { createManagedExecutionRunner } = await import("./drivers/managed_execution_runner.mjs");
  const { startHubServer } = await import(pathToFileURL(path.join(workspace, "moyAI-Hub/tests/browser/hub_server.mjs")));
  const prepared = await createDesktopRunContext({ artifactParent: options["artifact-parent"], binary: options.binary,
    executionId: freshExecutionId(), scenarioId: ID, desktopIsolation: "fixture", harnessRoot: directory });
  const lines = createInterface({ input, crlfDelay: Infinity });
  const commands = lines[Symbol.asyncIterator]();
  const environment = { MOYAI_DESKTOP_E2E_RUNNER: options["runner-test-binary"] };
  const pcs = new Map();
  let provider = null, hub = null, sharedClose = null, finished = false;
  const createPC = name => {
    const pc = { name, context: null, sink: null, runtime: null, runner: null, closed: null, captureRequested: false };
    pcs.set(name, pc);
    return pc;
  };
  const a = createPC("a");
  async function preparePC(pc, args) {
    pc.context = args.context; pc.sink = args.sink;
    await prepareDesktopFixture({ ...args, owner: ID, sentinelName: null, sentinelText: "",
      configText: `[model]\nbase_url = ${JSON.stringify(provider.baseUrl)}\nmodel = "shared-workflow"\nprovider_profile = "openai_compatible"\nmax_retries = 0\n[multi_agent]\nenabled = false\n` });
    pc.runner = createManagedExecutionRunner({ context: pc.context, sink: pc.sink,
      runnerBinary: options["runner-binary"], runnerTestBinary: options["runner-test-binary"] });
  }
  async function closePC(pc) {
    try { pc.closed ??= pc.runner ? await pc.runner.quiesce() : { pass: true, not_started: true }; }
    catch (error) { pc.closed = { pass: false, error: error.message }; }
    if (pc.runtime && !pc.runner?.identity && (pc.captureRequested || !finished)) {
      pc.closed = { ...pc.closed, pass: false, unobserved_runner: true };
    }
    return { input: pc.closed.pass ? "pass" : "fail", resources: [{ pc: pc.name, runner: pc.closed }] };
  }
  const scenarioFor = pc => ({ id: ID, databaseRequired: true, environment,
    prepare: args => preparePC(pc, args), requestGracefulExit,
    quiesce: () => closePC(pc), cleanup: async () => ({ input: pc.closed?.pass ? "pass" : "fail", resources: [] }) });
  const scenario = { ...scenarioFor(a), productOracle: "not_required", manualGate: "pending",
    async prepare(args) {
      for (const name of ["hub-binary", "runner-binary", "runner-test-binary"]) {
        const bytes = await readFile(options[name]);
        await args.sink.record("manual-fixture-build", { kind: name, path: options[name], size_bytes: bytes.length, sha256: createHash("sha256").update(bytes).digest("hex") }, { phase: args.phase, owner: ID });
      }
      provider = await startSharedWorkflowProvider();
      provider.releaseChild();
      await preparePC(a, args);
      hub = await startHubServer({ dataDirectory: path.join(args.context.root, "hub/data"), binary: options["hub-binary"],
        record: (kind, value) => args.sink.record(kind, value, { owner: "hub-server" }) });
    },
    async execute(refs) {
      a.runtime = refs.runtime;
      output({ event: "ready", execution_id: refs.context.executionId, ...pcSummary(a), hub_url: hub.url, network_port: hub.networkPort,
        provider_url: provider.baseUrl, model: "shared-workflow", manual_task_prompt: "desktop-transfer-child: create the controlled result file" });
      return processManualCommands(commands, {
        async startB() {
          if (pcs.has("b")) throw new Error("Desktop B already belongs to this session");
          const b = createPC("b");
          const companion = await refs.host.openCompanion({ context: await createCompanionContext(refs.context, "desktop-b"), scenario: scenarioFor(b), sink: refs.sink });
          b.runtime = companion.runtime;
          output({ event: "b-ready", ...pcSummary(b) });
        },
        async captureRunner(name) {
          const pc = pcs.get(name);
          if (!pc?.runtime) throw new Error("Requested Desktop is not ready");
          pc.captureRequested = true;
          const captured = await pc.runner.capture(pc.runtime.desktop_process_id);
          output({ event: "capture", pc: name, ...captured });
        },
        async recordFinish(command) {
          await refs.sink.record("manual-gui-observations", { verdict: command.verdict, observations: command.observations,
            source: "Human-directed GUI operations; no scripted GUI acquisition or product API seeding", provider: { request_count: provider.requests.length, failures: provider.failures } }, { phase: "executing", owner: ID });
          finished = true;
        },
      });
    },
    async quiesce() {
      const own = await closePC(a);
      const failures = [], resources = [...own.resources];
      if (own.input !== "pass") failures.push("runner-a");
      try { const result = hub ? await hub.close() : { pass: true }; resources.push({ kind: "hub", ...result }); if (!result.pass) failures.push("hub"); }
      catch { failures.push("hub"); }
      try { await provider?.close(); } catch { failures.push("provider"); }
      sharedClose = { input: failures.length ? "fail" : "pass", resources, failures };
      return sharedClose;
    },
    async cleanup() { return { input: sharedClose?.input ?? "fail", resources: [] }; },
  };
  try {
    const result = await executeDesktopScenario({ ...prepared, scenario, host: new WindowsTauriHost() });
    output({ event: "finished", execution_root: result.execution_root, classification: result.result.classification,
      inputs: result.result.inputs, cleanup: result.result.cleanup, diagnostics: result.result.diagnostics.map(row => ({ owner: row.owner, code: row.code, message: row.message })), result_sha256: result.seal.result_sha256 });
    return result;
  } finally { lines.close(); }
}

if (path.resolve(process.argv[1] ?? "") === fileURLToPath(import.meta.url)) {
  try {
    const result = await runManualSession(parseManualArguments(process.argv.slice(2)));
    process.exitCode = result.result.classification === "pass" ? 0 : result.result.classification === "manual_pending" ? 3 : 1;
  } catch (error) {
    process.stdout.write(`${JSON.stringify({ event: "error", code: error.code ?? "manual-session-error", message: error.message })}\n`);
    process.exitCode = 1;
  }
}

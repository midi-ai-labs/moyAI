import { execFile } from "node:child_process";
import { promisify } from "node:util";
import path from "node:path";
import { desktopLaunchEnvironment } from "../core/desktop_isolation.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { waitForObservation } from "../core/deadline.mjs";
import { invokeWindowsProcess } from "./windows_process.mjs";

const executeFile = promisify(execFile);

function isBusyPipe(error) {
  return typeof error?.stderr === "string" && error.stderr.trimEnd().endsWith("(os error 231)");
}

// Observe a Runner started by Desktop; this owner never launches a replacement.
// Call capture after GUI consent, then quiesce from the common scenario resource
// release so its exact process is settled before the host audits SQLite.
export function createManagedExecutionRunner({ context, sink, runnerBinary, runnerTestBinary, expectedParentProcessId }, {
  execute = executeFile, processCommand = invokeWindowsProcess, observe = waitForObservation,
} = {}) {
  const executable = runnerBinary ?? path.join(path.dirname(context.binary), "moyai-runner.exe");
  if (!path.isAbsolute(executable) || !runnerTestBinary || !path.isAbsolute(runnerTestBinary)) throw new TypeError("Managed execution requires absolute Runner CLI and test-host paths");
  const env = context.desktopIsolation === "fixture"
    ? desktopLaunchEnvironment({ context, processTemp: path.join(path.dirname(context.paths.config), "temp") })
    : { ...globalThis.process.env, MOYAI_CONFIG_PATH: context.paths.config_file, MOYAI_DATA_DIR: context.paths.data };
  let identity = null, ownerPath = null, settlement = null, captureAttempted = false, captureParentProcessId;
  async function command(args) {
    const result = await execute(executable, args, { env, windowsHide: true, timeout: 12000, maxBuffer: 2 * 1024 * 1024 });
    return JSON.parse(result.stdout);
  }
  async function commandWhenAvailable(args) {
    const { value: { result } } = await observe({
      label: `Managed Runner accepts ${args[0]}`, timeoutMs: 25000, retrySampleErrors: false,
      // Desktop may be querying the single-instance pipe immediately after GUI
      // consent. ERROR_PIPE_BUSY precedes delivery; other failures remain fatal.
      sample: async () => {
        try { return { result: await command(args) }; }
        catch (error) {
          if (isBusyPipe(error)) return null;
          throw error;
        }
      },
      accept: value => value !== null,
    });
    return result;
  }
  async function capture(parentProcessId = expectedParentProcessId) {
    captureAttempted = true;
    captureParentProcessId = parentProcessId;
    const result = await commandWhenAvailable(["identity"]);
    const current = result?.identity;
    if (!current?.runner_id) throw new DesktopE2eError("product", "device-execution-mismatch", "The managed execution host did not expose its authenticated identity");
    const owner = await processCommand("Capture", { ProcessId: current.process_id, ExpectedExecutable: runnerTestBinary, ExpectedParentProcessId: parentProcessId });
    const artifact = await sink.writeJson(`owners/managed-execution-${current.runner_id}.json`, owner);
    ownerPath = path.join(sink.root, ...artifact.relative_path.split("/"));
    identity = current;
    return { identity: structuredClone(identity), owner };
  }
  async function settle() {
    // A failed capture is not evidence of absence. Reacquire the same scoped IPC
    // and expected Desktop parent before cleanup; never discover arbitrary hosts.
    if (identity === null) {
      if (!captureAttempted) return { pass: true, not_started: true };
      await capture(captureParentProcessId);
    }
    let normal = false;
    const wait = (label, sample) => observe({ label, sample, accept: Boolean, timeoutMs: 25000, retrySampleErrors: false });
    try {
      await commandWhenAvailable(["shutdown", "--runner", identity.runner_id]);
      await wait("Managed Runner closes authenticated IPC", () => command(["identity"]).then(() => false, error => !isBusyPipe(error)));
      await wait("Managed Runner process exits", () => processCommand("Capture", { ProcessId: identity.process_id, ExpectedExecutable: runnerTestBinary }).then(() => false, () => true));
      normal = true;
    } catch {}
    const stopped = await processCommand("StopOwner", { ExecutionRoot: context.root, OwnerPath: ownerPath });
    return { pass: normal && !stopped.stopped, normal_shutdown: normal, forced: stopped.stopped, process_id: identity.process_id };
  }
  return Object.freeze({
    command,
    capture,
    get identity() { return identity === null ? null : structuredClone(identity); },
    quiesce() { settlement ??= settle(); return settlement; },
  });
}

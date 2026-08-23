import crypto from "node:crypto";
import path from "node:path";
import process from "node:process";
import { spawn, spawnSync } from "node:child_process";
import { createReadStream } from "node:fs";
import { fileURLToPath } from "node:url";
import { lstat, mkdir, realpath, stat } from "node:fs/promises";

const bridge = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "windows_external_process.ps1");
const LABEL = /^[a-z0-9][a-z0-9._-]{2,95}$/;
const DECIMAL_TICKS = /^\d+$/;
const DEFAULT_CLEANUP_TIMEOUT_MS = 10_000;
const DEFAULT_MAX_OUTPUT_BYTES = 16 * 1024 * 1024;
const WRAPPER_DIAGNOSTIC_LIMIT_BYTES = 1024 * 1024;
const WRAPPER_OVERHEAD_MS = 30_000;
const WRAPPER_KILL_GRACE_MS = 5_000;
const SUPERVISOR_IDENTITY_TIMEOUT_MS = 10_000;

function invariant(condition, message) {
  if (!condition) throw new TypeError(message);
}

function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}

function pathEqual(left, right) {
  return path.resolve(left).toLowerCase() === path.resolve(right).toLowerCase();
}

function isSameOrDescendant(root, candidate) {
  const relative = path.relative(root, candidate);
  return relative === "" || (!path.isAbsolute(relative) && relative !== ".." && !relative.startsWith(`..${path.sep}`));
}

async function physicalPath(candidate, { kind, label }) {
  invariant(typeof candidate === "string" && candidate.length > 0, `${label} must be a path string`);
  const absolute = path.resolve(candidate);
  const item = await lstat(absolute);
  if (item.isSymbolicLink() || (kind === "file" ? !item.isFile() : !item.isDirectory())) {
    throw new TypeError(`${label} is not a physical ${kind}: ${absolute}`);
  }
  return { absolute, physical: await realpath(absolute), item };
}

async function absentExecutionFile(root, candidate, label) {
  invariant(typeof candidate === "string" && candidate.length > 0, `${label} must be a path string`);
  const absolute = path.resolve(candidate);
  if (!isSameOrDescendant(root, absolute) || pathEqual(root, absolute)) {
    throw new TypeError(`${label} must be a file below the execution root: ${absolute}`);
  }
  const parent = await physicalPath(path.dirname(absolute), { kind: "directory", label: `${label} parent` });
  if (!isSameOrDescendant(root, parent.physical) || !pathEqual(parent.absolute, parent.physical)) {
    throw new TypeError(`${label} parent is not a physical execution directory: ${parent.absolute}`);
  }
  try {
    await lstat(absolute);
    throw new TypeError(`${label} already exists: ${absolute}`);
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
  return absolute;
}

function normalizeArguments(value) {
  invariant(Array.isArray(value), "external process arguments must be an array");
  const argumentsCopy = value.map((entry) => {
    invariant(typeof entry === "string" && !entry.includes("\0"), "external process arguments must be NUL-free strings");
    return entry;
  });
  invariant(Buffer.byteLength(JSON.stringify(argumentsCopy), "utf8") <= 512 * 1024, "external process arguments are too large");
  return argumentsCopy;
}

function normalizeEnvironment(value) {
  invariant(value !== null && typeof value === "object" && !Array.isArray(value), "external process environment must be an object");
  const normalized = {};
  const seen = new Set();
  for (const [key, entry] of Object.entries(value)) {
    invariant(typeof key === "string" && key.length > 0 && !key.includes("=") && !key.includes("\0"), `invalid environment key: ${key}`);
    if (entry === undefined) continue;
    invariant(typeof entry === "string" && !entry.includes("\0"), `invalid environment value: ${key}`);
    const folded = key.toLowerCase();
    invariant(!seen.has(folded), `duplicate case-insensitive environment key: ${key}`);
    seen.add(folded);
    normalized[key] = entry;
  }
  return Object.fromEntries(Object.entries(normalized).sort(([left], [right]) => {
    const leftFolded = left.toLowerCase();
    const rightFolded = right.toLowerCase();
    if (leftFolded < rightFolded) return -1;
    if (leftFolded > rightFolded) return 1;
    return left < right ? -1 : left > right ? 1 : 0;
  }));
}

function collectBounded(stream, limitBytes, onOverflow) {
  const chunks = [];
  let bytes = 0;
  let overflow = false;
  stream.on("data", (chunk) => {
    bytes += chunk.length;
    if (!overflow && bytes <= limitBytes) chunks.push(Buffer.from(chunk));
    else if (!overflow) {
      overflow = true;
      onOverflow();
    }
  });
  return () => ({ bytes, overflow, text: Buffer.concat(chunks).toString("utf8") });
}

function parseWrapperEnvelopes(text) {
  const values = text.split(/\r?\n/).map((line) => line.trim()).filter(Boolean).map((line) => {
    try { return JSON.parse(line); }
    catch (error) { throw new Error(`Windows external-process wrapper emitted invalid JSON: ${errorMessage(error)}`); }
  });
  const owners = values.filter((value) => value?.kind === "owner");
  const results = values.filter((value) => value?.kind === "result");
  const unknown = values.filter((value) => value?.kind !== "owner" && value?.kind !== "result");
  if (owners.length > 1 || results.length > 1 || unknown.length > 0) {
    throw new Error("Windows external-process wrapper emitted an invalid envelope sequence");
  }
  return { owner: owners[0] ?? null, result: results[0] ?? null, values };
}

async function fileIdentity(candidate) {
  const item = await stat(candidate);
  if (!item.isFile()) throw new Error(`external process output is not a file: ${candidate}`);
  const hash = crypto.createHash("sha256");
  await new Promise((resolve, reject) => {
    const stream = createReadStream(candidate);
    stream.on("data", (chunk) => hash.update(chunk));
    stream.once("error", reject);
    stream.once("end", resolve);
  });
  return { path: path.resolve(candidate), sha256: hash.digest("hex"), size_bytes: item.size };
}

async function optionalFileIdentity(candidate) {
  try { return await fileIdentity(candidate); }
  catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

function pidExists(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    if (error?.code === "ESRCH") return false;
    return true;
  }
}

async function waitForPidAbsent(pid, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (!pidExists(pid)) return true;
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  return !pidExists(pid);
}

export class WindowsExternalProcessError extends Error {
  constructor(code, message, evidence = null) {
    super(message);
    invariant(typeof code === "string" && /^[a-z0-9][a-z0-9._-]+$/.test(code), "invalid external-process error code");
    this.name = "WindowsExternalProcessError";
    this.code = code;
    this.evidence = evidence === null ? null : structuredClone(evidence);
  }
}

function captureSupervisorIdentity(powershell, expectedExecutable, wrapperEnvironment) {
  const script = `
$ErrorActionPreference = "Stop"
$process = [Diagnostics.Process]::GetProcessById(${process.pid})
try {
  $identity = [ordered]@{
    process_id = $process.Id
    process_start_time_utc_ticks = $process.StartTime.ToUniversalTime().Ticks.ToString([Globalization.CultureInfo]::InvariantCulture)
    executable_path = [IO.Path]::GetFullPath($process.MainModule.FileName)
  }
  [Console]::Out.Write((ConvertTo-Json -InputObject $identity -Compress))
} finally {
  $process.Dispose()
}
`;
  const encoded = Buffer.from(script, "utf16le").toString("base64");
  const captured = spawnSync(powershell, [
    "-NoLogo",
    "-NoProfile",
    "-NonInteractive",
    "-EncodedCommand",
    encoded,
  ], {
    cwd: path.dirname(expectedExecutable),
    env: wrapperEnvironment,
    windowsHide: true,
    encoding: "utf8",
    timeout: SUPERVISOR_IDENTITY_TIMEOUT_MS,
    killSignal: "SIGKILL",
    maxBuffer: WRAPPER_DIAGNOSTIC_LIMIT_BYTES,
  });
  let identity = null;
  try { identity = JSON.parse(captured.stdout ?? ""); }
  catch { /* reported through the exact contract below */ }
  const accepted = captured.error === undefined
    && captured.status === 0
    && identity?.process_id === process.pid
    && typeof identity?.process_start_time_utc_ticks === "string"
    && DECIMAL_TICKS.test(identity.process_start_time_utc_ticks)
    && pathEqual(identity?.executable_path ?? "", expectedExecutable);
  if (!accepted) {
    throw new WindowsExternalProcessError(
      "external-supervisor-identity",
      "Windows external-process supervisor identity could not be captured exactly",
      {
        expected_process_id: process.pid,
        expected_executable_path: expectedExecutable,
        probe_status: captured.status,
        probe_signal: captured.signal,
        probe_error: captured.error === undefined ? null : errorMessage(captured.error),
        probe_stdout: captured.stdout ?? "",
        probe_stderr: captured.stderr ?? "",
      },
    );
  }
  return Object.freeze({
    process_id: identity.process_id,
    process_start_time_utc_ticks: identity.process_start_time_utc_ticks,
    executable_path: path.resolve(identity.executable_path),
  });
}

export async function runWindowsExternalProcess({
  executionRoot,
  executable,
  args = [],
  cwd,
  env = process.env,
  stdoutPath,
  stderrPath,
  timeoutMs,
  cleanupTimeoutMs = DEFAULT_CLEANUP_TIMEOUT_MS,
  maxOutputBytes = DEFAULT_MAX_OUTPUT_BYTES,
  label,
  powershell = "pwsh.exe",
  wrapperTimeoutMs = undefined,
}) {
  if (process.platform !== "win32") {
    throw new WindowsExternalProcessError("windows-required", "Windows external-process Job ownership requires win32");
  }
  invariant(typeof label === "string" && LABEL.test(label), "external process label is invalid");
  invariant(Number.isInteger(timeoutMs) && timeoutMs >= 1 && timeoutMs <= 86_400_000, "external process timeoutMs is invalid");
  invariant(Number.isInteger(cleanupTimeoutMs) && cleanupTimeoutMs >= 100 && cleanupTimeoutMs <= 60_000, "external process cleanupTimeoutMs is invalid");
  invariant(Number.isSafeInteger(maxOutputBytes) && maxOutputBytes >= 1024 && maxOutputBytes <= 1024 * 1024 * 1024, "external process maxOutputBytes is invalid");
  invariant(typeof powershell === "string" && powershell.length > 0 && !powershell.includes("\0"), "PowerShell executable is invalid");
  const effectiveWrapperTimeout = wrapperTimeoutMs ?? timeoutMs + cleanupTimeoutMs + WRAPPER_OVERHEAD_MS;
  invariant(Number.isInteger(effectiveWrapperTimeout) && effectiveWrapperTimeout >= 100 && effectiveWrapperTimeout <= 86_500_000, "external process wrapperTimeoutMs is invalid");

  const root = await physicalPath(executionRoot, { kind: "directory", label: "execution root" });
  const command = await physicalPath(executable, { kind: "file", label: "external executable" });
  const working = await physicalPath(cwd, { kind: "directory", label: "external working directory" });
  if (!isSameOrDescendant(root.physical, working.physical)) {
    throw new TypeError(`external working directory escaped the execution root: ${working.physical}`);
  }
  const exactStdout = await absentExecutionFile(root.physical, stdoutPath, "external stdout");
  const exactStderr = await absentExecutionFile(root.physical, stderrPath, "external stderr");
  if (pathEqual(exactStdout, exactStderr)) throw new TypeError("external stdout and stderr paths must be distinct");
  const targetArguments = normalizeArguments(args);
  const targetEnvironment = normalizeEnvironment(env);
  const executableIdentity = await fileIdentity(command.physical);
  const supervisorExecutable = await physicalPath(process.execPath, { kind: "file", label: "supervisor executable" });
  const wrapperTemporary = path.join(path.dirname(exactStdout), `${label}.wrapper-temp`);
  if (pathEqual(wrapperTemporary, exactStdout) || pathEqual(wrapperTemporary, exactStderr)) {
    throw new TypeError("wrapper temporary directory must be distinct from external output paths");
  }
  await mkdir(wrapperTemporary, { recursive: false });
  const wrapperTemporaryIdentity = await physicalPath(wrapperTemporary, { kind: "directory", label: "wrapper temporary directory" });
  const wrapperEnvironment = normalizeEnvironment(Object.fromEntries([
    ...Object.entries(process.env).filter(([key]) => !new Set(["temp", "tmp", "tmpdir"]).has(key.toLowerCase())),
    ["TEMP", wrapperTemporaryIdentity.physical],
    ["TMP", wrapperTemporaryIdentity.physical],
    ["TMPDIR", wrapperTemporaryIdentity.physical],
  ]));
  const supervisor = captureSupervisorIdentity(powershell, supervisorExecutable.physical, wrapperEnvironment);
  const argumentsBase64 = Buffer.from(JSON.stringify(targetArguments), "utf8").toString("base64");
  const environmentJson = JSON.stringify(targetEnvironment);
  const environmentBase64 = Buffer.from(environmentJson, "utf8").toString("base64");
  const environmentIdentity = {
    sha256: crypto.createHash("sha256").update(environmentJson, "utf8").digest("hex"),
    key_count: Object.keys(targetEnvironment).length,
    keys: Object.keys(targetEnvironment),
  };
  const wrapperArguments = [
    "-NoLogo",
    "-NoProfile",
    "-NonInteractive",
    "-File",
    bridge,
    "-ExecutionRoot", root.physical,
    "-Executable", command.physical,
    "-WorkingDirectory", working.physical,
    "-StdoutPath", exactStdout,
    "-StderrPath", exactStderr,
    "-ArgumentsBase64", argumentsBase64,
    "-EnvironmentBase64", environmentBase64,
    "-SupervisorProcessId", String(supervisor.process_id),
    "-SupervisorExecutable", supervisor.executable_path,
    "-SupervisorStartTimeUtcTicks", supervisor.process_start_time_utc_ticks,
    "-TimeoutMs", String(timeoutMs),
    "-CleanupTimeoutMs", String(cleanupTimeoutMs),
    "-MaxOutputBytes", String(maxOutputBytes),
  ];
  invariant(wrapperArguments.reduce((total, entry) => total + entry.length + 3, 0) <= 28_000, "external process wrapper command line is too large");
  const started = Date.now();
  const wrapper = spawn(powershell, wrapperArguments, {
    cwd: root.physical,
    env: wrapperEnvironment,
    windowsHide: true,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let diagnosticOverflow = false;
  let wrapperTimedOut = false;
  let wrapperTerminationReason = null;
  let wrapperAsyncError = null;
  const wrapperKillAttempts = [];
  let scheduleWrapperEscalation = () => {};
  const attemptWrapperKill = (signal) => {
    let accepted = false;
    let failure = null;
    try {
      if (wrapper.exitCode === null) accepted = wrapper.kill(signal);
    } catch (error) {
      failure = errorMessage(error);
    }
    wrapperKillAttempts.push({ signal, accepted, failure, elapsed_ms: Date.now() - started });
  };
  const terminateWrapper = (reason) => {
    wrapperTerminationReason ??= reason;
    if (reason === "timeout") wrapperTimedOut = true;
    attemptWrapperKill("SIGTERM");
    scheduleWrapperEscalation();
  };
  const stdout = collectBounded(wrapper.stdout, WRAPPER_DIAGNOSTIC_LIMIT_BYTES, () => {
    diagnosticOverflow = true;
    terminateWrapper("diagnostic-overflow");
  });
  const stderr = collectBounded(wrapper.stderr, WRAPPER_DIAGNOSTIC_LIMIT_BYTES, () => {
    diagnosticOverflow = true;
    terminateWrapper("diagnostic-overflow");
  });
  let wrapperOutcome;
  try {
    wrapperOutcome = await new Promise((resolve, reject) => {
      let settled = false;
      let escalationTimer = null;
      const timer = setTimeout(() => terminateWrapper("timeout"), effectiveWrapperTimeout);
      const finish = (callback, value) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        if (escalationTimer !== null) clearTimeout(escalationTimer);
        callback(value);
      };
      scheduleWrapperEscalation = () => {
        if (settled || escalationTimer !== null) return;
        escalationTimer = setTimeout(() => {
          attemptWrapperKill("SIGKILL");
          wrapper.stdout.destroy();
          wrapper.stderr.destroy();
          wrapper.unref();
          finish(resolve, {
            code: wrapper.exitCode,
            signal: wrapper.signalCode,
            close_observed: false,
            forced_bounded_return: true,
          });
        }, WRAPPER_KILL_GRACE_MS);
      };
      if (wrapperTerminationReason !== null) scheduleWrapperEscalation();
      wrapper.once("error", (error) => {
        if (wrapperTerminationReason === null) {
          finish(reject, error);
          return;
        }
        wrapperAsyncError = errorMessage(error);
        scheduleWrapperEscalation();
      });
      wrapper.once("close", (code, signal) => finish(resolve, {
        code,
        signal,
        close_observed: true,
        forced_bounded_return: false,
      }));
    });
  } catch (error) {
    throw new WindowsExternalProcessError("external-wrapper-spawn", "Windows external-process wrapper could not be supervised", {
      label,
      wrapper_process_id: wrapper.pid ?? null,
      error: errorMessage(error),
      kill_attempts: wrapperKillAttempts,
    });
  }
  const wrapperStdout = stdout();
  const wrapperStderr = stderr();
  let envelopes = { owner: null, result: null, values: [] };
  let envelopeError = null;
  try { envelopes = parseWrapperEnvelopes(wrapperStdout.text); }
  catch (error) { envelopeError = error; }
  const ownerEnvelope = envelopes.owner;
  const owner = ownerEnvelope?.owner ?? null;
  const observedSupervisor = ownerEnvelope?.supervisor ?? null;
  const ownerAccepted = ownerEnvelope?.wrapper_process_id === wrapper.pid
    && ownerEnvelope?.job?.assigned_at_creation === true
    && ownerEnvelope?.job?.assigned_before_resume === true
    && ownerEnvelope?.job?.kill_on_close === true
    && Number.isInteger(owner?.process_id)
    && owner.process_id > 0
    && typeof owner?.process_start_time_utc_ticks === "string"
    && DECIMAL_TICKS.test(owner.process_start_time_utc_ticks)
    && pathEqual(owner?.executable_path ?? "", command.physical)
    && owner?.parent_process_id === wrapper.pid
    && observedSupervisor?.process_id === supervisor.process_id
    && observedSupervisor?.process_start_time_utc_ticks === supervisor.process_start_time_utc_ticks
    && pathEqual(observedSupervisor?.executable_path ?? "", supervisor.executable_path);
  const rootAbsentAfterWrapperExit = ownerAccepted && wrapperOutcome.close_observed
    ? await waitForPidAbsent(owner.process_id)
    : null;
  const wrapperFailureEvidence = {
    label,
    wrapper_process_id: wrapper.pid,
    wrapper_exit_code: wrapperOutcome.code,
    wrapper_signal: wrapperOutcome.signal,
    wrapper_timed_out: wrapperTimedOut,
    diagnostic_overflow: diagnosticOverflow,
    termination_reason: wrapperTerminationReason,
    kill_attempts: wrapperKillAttempts,
    asynchronous_error: wrapperAsyncError,
    close_observed: wrapperOutcome.close_observed,
    forced_bounded_return: wrapperOutcome.forced_bounded_return,
    elapsed_ms: Date.now() - started,
    expected_supervisor: supervisor,
    owner: ownerEnvelope,
    protocol_error: envelopeError === null ? null : errorMessage(envelopeError),
    stdout: wrapperStdout,
    stderr: wrapperStderr,
    root_absent_after_wrapper_exit: rootAbsentAfterWrapperExit,
    output_files: {
      stdout: wrapperOutcome.close_observed ? await optionalFileIdentity(exactStdout) : null,
      stderr: wrapperOutcome.close_observed ? await optionalFileIdentity(exactStderr) : null,
    },
  };
  if (wrapperTimedOut || diagnosticOverflow) {
    const code = !wrapperOutcome.close_observed
      ? "external-wrapper-kill-timeout"
      : ownerAccepted && rootAbsentAfterWrapperExit !== true
        ? "external-wrapper-cleanup"
        : wrapperTimedOut
          ? "external-wrapper-timeout"
          : "external-wrapper-output-overflow";
    throw new WindowsExternalProcessError(
      code,
      "Windows external-process wrapper did not complete its bounded ownership protocol",
      wrapperFailureEvidence,
    );
  }
  if (envelopeError !== null) {
    throw new WindowsExternalProcessError("external-wrapper-protocol", errorMessage(envelopeError), wrapperFailureEvidence);
  }
  if (!ownerAccepted) {
    throw new WindowsExternalProcessError("external-owner-identity", "Windows external-process root identity was not exact", wrapperFailureEvidence);
  }
  if (wrapperOutcome.code !== 0 || envelopes.result === null) {
    if (rootAbsentAfterWrapperExit !== true) {
      throw new WindowsExternalProcessError("external-wrapper-cleanup", "Windows external-process root absence was not proven after wrapper failure", wrapperFailureEvidence);
    }
    throw new WindowsExternalProcessError("external-wrapper-failed", "Windows external-process wrapper failed before an exact zero result", wrapperFailureEvidence);
  }
  const result = envelopes.result.result;
  const resultAccepted = envelopes.result.wrapper_process_id === wrapper.pid
    && result !== null
    && typeof result === "object"
    && result.job_assigned_at_creation === true
    && result.job_kill_on_close === true
    && result.supervisor_lost === false
    && result.supervisor?.process_id === supervisor.process_id
    && result.supervisor?.process_start_time_utc_ticks === supervisor.process_start_time_utc_ticks
    && pathEqual(result.supervisor?.executable_path ?? "", supervisor.executable_path)
    && result.descendant_zero === true
    && result.active_processes_after_cleanup === 0
    && Array.isArray(result.residual_process_ids_after_cleanup)
    && result.residual_process_ids_after_cleanup.length === 0
    && Array.isArray(result.observed_process_ids)
    && result.observed_process_ids.includes(owner.process_id);
  if (!resultAccepted) {
    throw new WindowsExternalProcessError("external-descendant-zero", "Windows external-process Job did not prove exact descendant zero", {
      ...wrapperFailureEvidence,
      result: envelopes.result,
    });
  }
  const output = {
    stdout: await fileIdentity(exactStdout),
    stderr: await fileIdentity(exactStderr),
  };
  if (output.stdout.size_bytes !== result.stdout_bytes || output.stderr.size_bytes !== result.stderr_bytes) {
    throw new WindowsExternalProcessError("external-output-identity", "Windows external-process output identity drifted after Job settlement", {
      result,
      output,
    });
  }
  return {
    schema_version: "desktop-e2e.windows-external-process.v1",
    label,
    command: {
      executable: executableIdentity,
      args: targetArguments,
      cwd: working.physical,
      environment: environmentIdentity,
    },
    supervisor: structuredClone(supervisor),
    owner: structuredClone(owner),
    job: {
      assigned_at_creation: true,
      assigned_before_resume: true,
      kill_on_close: true,
      observed_process_ids: structuredClone(result.observed_process_ids),
      residual_process_ids_before_termination: structuredClone(result.residual_process_ids_before_termination),
      residual_process_ids_after_cleanup: [],
      descendant_zero: true,
    },
    outcome: structuredClone(result),
    output,
    wrapper: {
      process_id: wrapper.pid,
      exit_code: wrapperOutcome.code,
      signal: wrapperOutcome.signal,
      close_observed: wrapperOutcome.close_observed,
      temporary_directory: wrapperTemporaryIdentity.physical,
      elapsed_ms: Date.now() - started,
    },
  };
}

export const WINDOWS_EXTERNAL_PROCESS_DEFAULTS = Object.freeze({
  cleanup_timeout_ms: DEFAULT_CLEANUP_TIMEOUT_MS,
  max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
});

import path from "node:path";
import { spawn } from "node:child_process";
import { lstat } from "node:fs/promises";
import { pathToFileURL } from "node:url";

async function exists(candidate) {
  try {
    return await lstat(candidate);
  } catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

async function assertOwnedFile(executionRoot, candidate) {
  const root = path.resolve(executionRoot);
  const target = path.resolve(candidate);
  const relative = path.relative(root, target);
  if (relative === "" || path.isAbsolute(relative) || relative === ".." || relative.startsWith(`..${path.sep}`)) {
    throw new Error(`SQLite path escaped execution root: ${target}`);
  }
  let current = root;
  for (const part of relative.split(path.sep)) {
    current = path.join(current, part);
    const item = await exists(current);
    if (item?.isSymbolicLink()) throw new Error(`SQLite path contains a symbolic link: ${current}`);
  }
  return target;
}

function runSqlite(argumentsList, timeoutMs = 20_000) {
  return new Promise((resolve, reject) => {
    const child = spawn("sqlite3.exe", argumentsList, { windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    const timer = setTimeout(() => {
      child.kill();
      reject(new Error("sqlite3 command timed out"));
    }, timeoutMs);
    child.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.once("exit", (code) => {
      clearTimeout(timer);
      resolve({ exit_code: code, stdout: stdout.trim(), stderr: stderr.trim() });
    });
  });
}

export async function auditClosedSqlite({ executionRoot, database, required = false }) {
  const exact = await assertOwnedFile(executionRoot, database);
  const databaseItem = await exists(exact);
  if (databaseItem === null) {
    return {
      present: false,
      path: exact,
      checkpoint: null,
      quick_check: null,
      foreign_key_violation_count: 0,
      sidecars: [],
      required,
      pass: !required,
    };
  }
  if (!databaseItem.isFile()) throw new Error(`SQLite path is not a file: ${exact}`);
  const checkpoint = await runSqlite(["-batch", "-bail", "-noheader", exact, "PRAGMA wal_checkpoint(TRUNCATE);"]);
  const checkpointParts = checkpoint.stdout.split("|");
  if (checkpoint.exit_code !== 0 || checkpointParts.length !== 3) {
    throw new Error(`SQLite checkpoint failed: ${checkpoint.stderr || checkpoint.stdout}`);
  }
  const checkpointNumbers = checkpointParts.map((value) => Number(value));
  if (checkpointNumbers.some((value) => !Number.isInteger(value))) throw new Error("SQLite checkpoint returned a malformed tuple");
  const immutableUri = `${pathToFileURL(exact).href}?mode=ro&immutable=1`;
  const quick = await runSqlite(["-batch", "-bail", "-noheader", immutableUri, "PRAGMA quick_check;"]);
  const foreign = await runSqlite(["-batch", "-bail", "-noheader", immutableUri, "PRAGMA foreign_key_check;"]);
  const violations = foreign.stdout.length === 0 ? 0 : foreign.stdout.split(/\r?\n/).filter(Boolean).length;
  const sidecars = [];
  for (const suffix of ["-wal", "-shm", "-journal"]) {
    const sidecar = `${exact}${suffix}`;
    const item = await exists(sidecar);
    if (item !== null) sidecars.push({ path: sidecar, size_bytes: item.size, symbolic_link: item.isSymbolicLink() });
  }
  const pass = checkpointNumbers[0] === 0
    && checkpointNumbers[1] === 0
    && checkpointNumbers[2] === 0
    && quick.exit_code === 0
    && quick.stdout === "ok"
    && foreign.exit_code === 0
    && violations === 0
    && sidecars.length === 0;
  return {
    present: true,
    path: exact,
    checkpoint: {
      busy: checkpointNumbers[0],
      log_frames: checkpointNumbers[1],
      checkpointed_frames: checkpointNumbers[2],
      raw: checkpoint.stdout,
    },
    quick_check: quick.stdout,
    foreign_key_violation_count: violations,
    sidecars,
    required,
    pass,
  };
}

import assert from "node:assert/strict";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawn } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";

import { auditClosedSqlite } from "../drivers/sqlite_cleanup.mjs";

function sqlite(argumentsList) {
  return new Promise((resolve, reject) => {
    const child = spawn("sqlite3.exe", argumentsList, { windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
    let stderr = "";
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.once("error", reject);
    child.once("exit", (code) => code === 0 ? resolve() : reject(new Error(stderr)));
  });
}

test("closed execution-owned SQLite is checkpointed and audited once", async (context) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "moyai-e2e-sqlite-"));
  context.after(() => rm(root, { recursive: true, force: true }));
  const database = path.join(root, "data.sqlite3");
  await sqlite([database, "PRAGMA journal_mode=WAL; CREATE TABLE sample(id INTEGER PRIMARY KEY); INSERT INTO sample DEFAULT VALUES;"]);
  const result = await auditClosedSqlite({ executionRoot: root, database });
  assert.equal(result.present, true);
  assert.equal(result.pass, true);
  assert.equal(result.quick_check, "ok");
  assert.equal(result.foreign_key_violation_count, 0);
  assert.deepEqual(result.sidecars, []);
});

test("absent optional SQLite is an explicit clean result and path escape is rejected", async (context) => {
  const root = await mkdtemp(path.join(os.tmpdir(), "moyai-e2e-sqlite-"));
  context.after(() => rm(root, { recursive: true, force: true }));
  const absent = await auditClosedSqlite({ executionRoot: root, database: path.join(root, "absent.sqlite3") });
  assert.equal(absent.present, false);
  assert.equal(absent.pass, true);
  const required = await auditClosedSqlite({ executionRoot: root, database: path.join(root, "required.sqlite3"), required: true });
  assert.equal(required.present, false);
  assert.equal(required.pass, false);
  await assert.rejects(() => auditClosedSqlite({ executionRoot: root, database: path.join(root, "..", "escaped.sqlite3") }), /escaped execution root/);
});

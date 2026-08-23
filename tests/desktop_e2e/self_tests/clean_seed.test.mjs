import assert from "node:assert/strict";
import crypto from "node:crypto";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import {
  access,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";

import {
  CASE5_2_CLEAN_SEED_COPY_RULE,
  copyCase52CleanSeed,
  inventoryCase52CleanSeed,
} from "../core/clean_seed.mjs";

async function put(root, relativePath, text) {
  const candidate = path.join(root, ...relativePath.split("/"));
  await mkdir(path.dirname(candidate), { recursive: true });
  await writeFile(candidate, text, { flag: "wx" });
}

async function temporaryTree(context) {
  const root = await mkdtemp(path.join(os.tmpdir(), "moyai-e2e-clean-seed-"));
  context.after(() => rm(root, { recursive: true, force: true }));
  const source = path.join(root, "source");
  const destination = path.join(root, "destination");
  await mkdir(source);
  await mkdir(destination);
  return { root, source, destination };
}

test("case5_2 clean seed produces a deterministic baseline manifest and preserves evidence inputs", async (context) => {
  const { root, source, destination } = await temporaryTree(context);
  await put(source, "README.md", "readme\n");
  await put(source, ".env.example", "PUBLIC_SAMPLE=1\n");
  await put(source, "backend/app.py", "print('app')\n");
  await put(source, "backend/data/runtime.sqlite3", "runtime\n");
  await put(source, "backend/__pycache__/app.pyc", "cache\n");
  await put(source, "backend/.venv/lib/site.py", "virtualenv\n");
  await put(source, "backend/backend.egg-info/PKG-INFO", "generated\n");
  await put(source, "backend/backend-1.0.dist-info/METADATA", "generated distribution metadata\n");
  await put(source, "backend/app.pyo", "compiled\n");
  await put(source, "frontend/.env.local", "SECRET=1\n");
  await put(source, "frontend/node_modules/pkg/index.js", "dependency\n");
  await put(source, "frontend/.next/bundle.js", "build\n");
  await put(source, "frontend/dist/app.js", "distribution\n");
  await put(source, "tests/test_cancel.py", "def test_cancel(): pass\n");
  await put(source, "test-results/result.json", "{}\n");
  await put(source, "examples/templates/request.json", "{\"id\": 1}\n");
  await put(source, "task.md", "old task\n");

  const result = await copyCase52CleanSeed({ source, destination });
  const sourceInventory = await inventoryCase52CleanSeed(source);
  const expectedPaths = [
    ".env.example",
    "README.md",
    "backend/app.py",
    "examples/templates/request.json",
    "tests/test_cancel.py",
  ];
  assert.deepEqual(result.files.map((entry) => entry.path), expectedPaths);
  assert.equal(result.file_count, expectedPaths.length);
  assert.equal(result.byte_count, result.files.reduce((total, entry) => total + entry.bytes, 0));
  const aggregate = result.files.map((entry) => `${entry.path}\0${entry.sha256}\0${entry.bytes}\n`).join("");
  assert.equal(result.aggregate_sha256, crypto.createHash("sha256").update(aggregate).digest("hex"));
  assert.equal(result.schema_version, "desktop-e2e.clean-seed.v1");
  assert.equal(result.copy_rule.id, CASE5_2_CLEAN_SEED_COPY_RULE.id);
  assert.deepEqual(result.copy_rule.excluded_directory_suffixes, [".egg-info", ".dist-info"]);
  assert.equal(sourceInventory.aggregate_sha256, result.aggregate_sha256);
  assert.deepEqual(sourceInventory.files, result.files);
  assert.equal(Object.hasOwn(sourceInventory, "destination"), false);
  assert.equal(await readFile(path.join(destination, "examples", "templates", "request.json"), "utf8"), "{\"id\": 1}\n");
  await assert.rejects(access(path.join(destination, "backend", "data")), /ENOENT/);
  await assert.rejects(access(path.join(destination, "backend", "backend-1.0.dist-info")), /ENOENT/);
  await assert.rejects(access(path.join(destination, "frontend", ".env.local")), /ENOENT/);

  const secondDestination = path.join(root, "destination-2");
  await mkdir(secondDestination);
  const repeated = await copyCase52CleanSeed({ source, destination: secondDestination });
  assert.equal(repeated.aggregate_sha256, result.aggregate_sha256);
  assert.deepEqual(repeated.files, result.files);
});

test("clean seed requires an already-existing empty disjoint destination", async (context) => {
  const { root, source, destination } = await temporaryTree(context);
  await put(source, "source.txt", "source\n");
  await put(destination, "occupied.txt", "occupied\n");
  await assert.rejects(
    copyCase52CleanSeed({ source, destination }),
    /must already exist and be empty/,
  );

  const missing = path.join(root, "missing");
  await assert.rejects(copyCase52CleanSeed({ source, destination: missing }), /ENOENT/);

  const nested = path.join(source, "empty-destination");
  await mkdir(nested);
  await assert.rejects(
    copyCase52CleanSeed({ source, destination: nested }),
    /must be disjoint directories/,
  );
});

test("clean seed rejects a traversed symlink or junction before copying any file", async (context) => {
  const { root, source, destination } = await temporaryTree(context);
  const outside = path.join(root, "outside");
  await mkdir(outside);
  await put(source, "a-first.txt", "must not be copied\n");
  await put(outside, "escaped.txt", "outside\n");
  await symlink(outside, path.join(source, "z-linked"), process.platform === "win32" ? "junction" : "dir");

  await assert.rejects(
    copyCase52CleanSeed({ source, destination }),
    /symbolic link or reparse traversal/,
  );
  assert.deepEqual(await readdir(destination), []);

  const linkedDestination = path.join(root, "linked-destination");
  await symlink(destination, linkedDestination, process.platform === "win32" ? "junction" : "dir");
  await assert.rejects(
    copyCase52CleanSeed({ source: outside, destination: linkedDestination }),
    /not a physical directory/,
  );
});

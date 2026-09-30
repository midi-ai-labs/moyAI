import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { mkdir, mkdtemp, rm, symlink, writeFile } from "node:fs/promises";
import { inside, sameFilePath, validateManifest } from "../fixtures/three_node_acceptance.mjs";
import { collectGeneratedSources } from "../scenarios/three_node_source_review.mjs";

test("component containment accepts equivalent normal and verbatim Windows paths", () => {
  for (const root of [String.raw`C:\fixture\worker`, String.raw`\\server\share\worker`]) {
    const file = path.win32.join(root, "worker.py");
    for (const parent of [root, path.win32.toNamespacedPath(root)]) {
      for (const child of [file, path.win32.toNamespacedPath(file)]) assert.equal(inside(parent, child), true, `${parent} -> ${child}`);
      assert.equal(inside(parent, parent), false);
      assert.equal(inside(parent, path.win32.join(`${root}-sibling`, "worker.py")), false);
      assert.equal(inside(parent, path.win32.join(root, "..", "outside.py")), false);
    }
  }
  assert.equal(inside(String.raw`\\server\share\worker`, String.raw`\\?\UNC\server\other-share\worker\worker.py`), false);
  assert.equal(inside(String.raw`C:\fixture\worker`, String.raw`\\?\D:\fixture\worker\worker.py`), false);
  assert.equal(inside(String.raw`C:\fixture\worker`, "worker.py"), false);
});

test("file identity equates Windows namespace and case without equating siblings or UNC shares", () => {
  assert.equal(sameFilePath(String.raw`C:\fixture\api\app.py`, String.raw`\\?\c:\FIXTURE\api\APP.py`), true);
  assert.equal(sameFilePath(String.raw`\\server\share\api\app.py`, String.raw`\\?\UNC\SERVER\SHARE\api\app.py`), true);
  assert.equal(sameFilePath(String.raw`C:\fixture\api\app.py`, String.raw`\\?\C:\fixture\api-sibling\app.py`), false);
  assert.equal(sameFilePath(String.raw`\\server\share\api\app.py`, String.raw`\\?\UNC\server\other\api\app.py`), false);
  assert.equal(sameFilePath("app.py", "app.py"), false);
});

async function sourceFixture(t) {
  const parent = fileURLToPath(new URL("../../../../project_sandbox/multi-environment-20260928/", import.meta.url));
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(path.join(parent, "source-path-test-"));
  t.after(async () => {
    if (path.dirname(path.resolve(root)) !== path.resolve(parent)) throw new Error("Fixture cleanup escaped the task directory");
    await rm(root, { recursive: true, force: true });
  });
  const environments = Object.fromEntries(["api", "worker", "database"].map(role => [role, { environment_id: role, directory: path.join(root, role) }]));
  const components = {};
  for (const [role, environment] of Object.entries(environments)) {
    await mkdir(environment.directory);
    const entrypoint = path.join(environment.directory, "app.run");
    await writeFile(entrypoint, `source for ${role}`);
    components[role] = { environment_id: role, base_url: "http://127.0.0.1:8000/", entrypoint: path.toNamespacedPath(entrypoint) };
  }
  const database = path.join(environments.database.directory, "jobs.json");
  // Mutable DB contents must not become review source merely because the manifest
  // uses the equivalent Rust verbatim path. The size also catches accidental reads.
  await writeFile(database, " ".repeat(2 * 1024 * 1024 + 1));
  components.database.database_file = path.toNamespacedPath(database);
  await writeFile(path.join(environments.api.directory, "acceptance.json"), JSON.stringify({ version: 1, components, ui_url: "http://127.0.0.1:8000/", control_url: "http://127.0.0.1:8000/control" }));
  return { root, environments, database };
}

test("source inventory includes verbatim entrypoints with unknown extensions and excludes the exact database", async t => {
  const fixture = await sourceFixture(t);
  const { files } = await collectGeneratedSources(fixture.environments);
  assert.equal(files.length, 4);
  assert.equal(files.filter(file => path.basename(file.path) === "app.run").length, 3);
  assert.equal(files.some(file => sameFilePath(file.path, fixture.database)), false);
});

test("equivalent path spelling does not permit a junction outside its role folder", async t => {
  const fixture = await sourceFixture(t);
  const outside = path.join(fixture.root, "outside");
  await mkdir(outside);
  await writeFile(path.join(outside, "source.py"), "outside source");
  await symlink(outside, path.join(fixture.environments.api.directory, "escape"), process.platform === "win32" ? "junction" : "dir");
  await assert.rejects(collectGeneratedSources(fixture.environments), /escaped its role folder/);
});

test("source inventory still rejects symlinks even when their target stays inside the role folder", async t => {
  const fixture = await sourceFixture(t);
  const nested = path.join(fixture.environments.api.directory, "nested");
  await mkdir(nested);
  await writeFile(path.join(nested, "source.py"), "nested source");
  await symlink(nested, path.join(fixture.environments.api.directory, "alias"), process.platform === "win32" ? "junction" : "dir");
  await assert.rejects(collectGeneratedSources(fixture.environments), /escaped its role folder/);
});

test("manifest paths may use the equivalent verbatim role folder but cannot enter a sibling", () => {
  const environments = Object.fromEntries(["api", "worker", "database"].map(role => [role, { environment_id: role, directory: path.win32.join("C:\\fixture", role) }]));
  const manifest = { version: 1, components: Object.fromEntries(Object.entries(environments).map(([role, environment]) => [role, {
    environment_id: role, base_url: "http://127.0.0.1:8000/", entrypoint: path.win32.toNamespacedPath(path.win32.join(environment.directory, "app.py")),
    ...(role === "database" ? { database_file: path.win32.toNamespacedPath(path.win32.join(environment.directory, "jobs.json")) } : {}),
  }])), ui_url: "http://127.0.0.1:8000/", control_url: "http://127.0.0.1:8000/control" };
  assert.equal(validateManifest(manifest, environments), manifest);
  manifest.components.worker.entrypoint = String.raw`\\?\C:\fixture\worker-sibling\app.py`;
  assert.throws(() => validateManifest(manifest, environments), /not deployed/);
});

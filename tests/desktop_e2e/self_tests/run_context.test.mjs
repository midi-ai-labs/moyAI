import assert from "node:assert/strict";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { access, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

import { createDesktopRunContext } from "../core/run_context.mjs";

const harnessRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

test("run context validates identity before writes and seals its harness source manifest", async (context) => {
  const parent = await mkdtemp(path.join(os.tmpdir(), "moyai-e2e-context-"));
  context.after(() => rm(parent, { recursive: true, force: true }));
  const binary = path.join(parent, "fixture.exe");
  await writeFile(binary, "fixture-binary", { flag: "wx" });
  const escaped = path.join(parent, "escaped");
  await assert.rejects(
    () => createDesktopRunContext({
      artifactParent: escaped,
      binary,
      executionId: "../escaped",
      scenarioId: "shell.baseline",
      harnessRoot,
    }),
    /invalid execution id/,
  );
  await assert.rejects(() => access(escaped), /ENOENT/);

  const prepared = await createDesktopRunContext({
    artifactParent: path.join(parent, "artifacts"),
    binary,
    executionId: "e2e-20260822-context-test",
    scenarioId: "shell.baseline",
    harnessRoot,
    now: () => "2026-08-22T00:00:00.000Z",
  });
  const rootManifest = JSON.parse(await readFile(path.join(prepared.context.root, "execution.json"), "utf8"));
  const sealedManifest = JSON.parse(await readFile(path.join(prepared.sink.root, "execution.json"), "utf8"));
  assert.deepEqual(sealedManifest, rootManifest);
  assert.match(rootManifest.harness.tree_sha256, /^[a-f0-9]{64}$/);
  assert.equal(rootManifest.harness.files.some((entry) => entry.relative_path === "core/desktop_execution.mjs"), true);
  assert.equal(prepared.context.sealedManifest.relative_path, "execution.json");
});

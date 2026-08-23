import crypto from "node:crypto";
import path from "node:path";
import { lstat, mkdir, readFile, readdir, stat, writeFile } from "node:fs/promises";

import { assertExecutionIdentity } from "./execution.mjs";
import { EvidenceSink } from "./evidence_sink.mjs";

function sha256(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

async function sourceInventory(root) {
  const absolute = path.resolve(root);
  const files = [];
  async function visit(directory) {
    const entries = await readdir(directory, { withFileTypes: true });
    entries.sort((left, right) => left.name.localeCompare(right.name));
    for (const entry of entries) {
      const candidate = path.join(directory, entry.name);
      if (entry.isSymbolicLink()) throw new Error(`harness source contains a symbolic link: ${candidate}`);
      if (entry.isDirectory()) await visit(candidate);
      else if (entry.isFile()) {
        const bytes = await readFile(candidate);
        files.push({
          relative_path: path.relative(absolute, candidate).replaceAll("\\", "/"),
          sha256: sha256(bytes),
          size_bytes: bytes.byteLength,
        });
      }
    }
  }
  const item = await lstat(absolute);
  if (!item.isDirectory() || item.isSymbolicLink()) throw new Error(`harness source root is not a physical directory: ${absolute}`);
  await visit(absolute);
  const aggregate = files.map((entry) => `${entry.relative_path}\0${entry.sha256}\0${entry.size_bytes}\n`).join("");
  return { root: absolute, tree_sha256: sha256(Buffer.from(aggregate, "utf8")), files };
}

export async function createDesktopRunContext({
  artifactParent,
  binary,
  executionId,
  scenarioId,
  scenarioConfig = null,
  harnessRoot,
  now = () => new Date().toISOString(),
}) {
  assertExecutionIdentity(executionId, scenarioId);
  const exactBinary = path.resolve(binary);
  const binaryItem = await stat(exactBinary);
  if (!binaryItem.isFile()) throw new TypeError(`Desktop binary is not a file: ${exactBinary}`);
  const binaryBytes = await readFile(exactBinary);
  const harness = await sourceInventory(harnessRoot);
  const exactParent = path.resolve(artifactParent);
  await mkdir(exactParent, { recursive: true });
  const root = path.resolve(exactParent, executionId);
  if (path.dirname(root).toLowerCase() !== exactParent.toLowerCase()) throw new Error(`execution root escaped artifact parent: ${root}`);
  await mkdir(root, { recursive: false });
  const directories = Object.fromEntries(
    ["workspace", "config", "data", "prefs", "webview", "logs"].map((name) => [name, path.join(root, name)]),
  );
  for (const directory of Object.values(directories)) await mkdir(directory, { recursive: false });
  const sink = await EvidenceSink.create(path.join(root, "evidence"));
  const paths = {
    ...directories,
    config_file: path.join(directories.config, "config.toml"),
    prefs_file: path.join(directories.prefs, "desktop.toml"),
    database: path.join(directories.data, "moyai.sqlite3"),
    stdout: path.join(directories.logs, "desktop.stdout.log"),
    stderr: path.join(directories.logs, "desktop.stderr.log"),
  };
  const manifest = {
    schema_version: "desktop-e2e.execution.v1",
    execution_id: executionId,
    scenario_id: scenarioId,
    scenario_config: scenarioConfig === null ? null : structuredClone(scenarioConfig),
    started_at: now(),
    binary: { path: exactBinary, sha256: sha256(binaryBytes), size_bytes: binaryItem.size },
    harness,
    root,
    paths,
    driver: { kind: "external-webview2-cdp", remote_debugging_port: 0, target: { type: "page", title: "moyAI" } },
  };
  const manifestBytes = Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`, "utf8");
  await writeFile(path.join(root, "execution.json"), manifestBytes, { flag: "wx" });
  const sealedManifest = await sink.writeBytes("execution.json", manifestBytes);
  return {
    context: { executionId, scenarioId, root, binary: exactBinary, binaryItem, paths, manifest, sealedManifest },
    sink,
  };
}

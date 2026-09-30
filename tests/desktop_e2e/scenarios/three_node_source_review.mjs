import { createHash } from "node:crypto";
import { readdir, readFile, realpath, writeFile } from "node:fs/promises";
import path from "node:path";
import { inside, sameFilePath, validateManifest } from "../fixtures/three_node_acceptance.mjs";
import { wait } from "./hub_browser_enrollment.mjs";

const hash = bytes => createHash("sha256").update(bytes).digest("hex");
const sourceExtensions = new Set([".js", ".mjs", ".cjs", ".ts", ".tsx", ".py", ".ps1", ".cmd", ".bat", ".json", ".html", ".css", ".sql", ".toml", ".yaml", ".yml", ".md", ".txt", ".rs", ".go", ".cs", ".java"]);

export async function collectGeneratedSources(environments) {
  const manifestPath = path.join(environments.api.directory, "acceptance.json");
  const manifest = validateManifest(JSON.parse((await readFile(manifestPath, "utf8")).replace(/^\uFEFF/, "")), environments);
  const files = [];
  for (const [role, environment] of Object.entries(environments)) {
    const root = await realpath(environment.directory);
    async function walk(directory, depth) {
      if (depth > 10) throw new Error("Generated source review exceeded the directory depth bound");
      for (const item of await readdir(directory, { withFileTypes: true })) {
        if (item.name.startsWith(".moyai") || [".git", "__pycache__", "node_modules", ".venv"].includes(item.name)) continue;
        const candidate = path.join(directory, item.name);
        if (item.isSymbolicLink() || !inside(root, await realpath(candidate))) throw new Error("Generated source escaped its role folder");
        if (sameFilePath(candidate, manifest.components.database.database_file)) continue;
        if (item.isDirectory()) { await walk(candidate, depth + 1); continue; }
        if (!item.isFile() || (!sourceExtensions.has(path.extname(candidate).toLowerCase()) && !sameFilePath(candidate, manifest.components[role].entrypoint))) continue;
        const bytes = await readFile(candidate);
        if (bytes.length > 2 * 1024 * 1024 || files.length >= 1000) throw new Error("Generated source review exceeded its file bound");
        files.push({ role, path: candidate, size_bytes: bytes.length, sha256: hash(bytes) });
      }
    }
    await walk(root, 0);
  }
  for (const role of ["api", "worker", "database"]) if (!files.some(file => sameFilePath(file.path, manifest.components[role].entrypoint))) throw new Error("Generated entrypoint is absent from source review");
  return { manifest, files };
}

export async function reviewGeneratedSources({ context, environments, sink }) {
  const { manifest, files } = await collectGeneratedSources(environments);
  const review = { manifest, environments, files, required_checks: [
    "Each component and its supervisor execute only in the assigned environment; cross-component data travels over the declared network API.",
    "The process command entrypoints match their role folders, including relative path working directories.",
    "Jobs, transitions, results and errors are durably stored by the DB service; clients do not fake persistence with caches.",
    "Failure injection arms exactly one job and throws after the real RUNNING transition; the normal Worker handler persists FAILED and its reason while DB remains healthy. The Web UI displays FAILED and a reason derived from the stored error; verify the later failure screenshot against this source.",
    "No direct test-control UPDATE fabricates FAILED, no input sentinel, no precomputed fixed result, no source supplied by the harness.",
    "Normal conversation stop owns all generated processes, including restarted Worker/DB descendants.",
  ] };
  const requestHash = hash(Buffer.from(JSON.stringify(review)));
  const requestPath = path.join(context.root, "live-source-review.json"), decisionPath = path.join(context.root, "live-source-review-decision.json");
  await writeFile(requestPath, JSON.stringify({ ...review, request_sha256: requestHash }, null, 2), { flag: "wx" });
  const decision = await wait("Agent audits generated code and records exact source hashes", async () => {
    try { return JSON.parse(await readFile(decisionPath, "utf8")); } catch (error) { if (error.code === "ENOENT") return null; throw error; }
  }, Boolean, 900000);
  if (decision.request_sha256 !== requestHash || decision.decision !== "approve" || !Array.isArray(decision.checked) || decision.checked.length !== review.required_checks.length
    || decision.checked.some((value, index) => value !== index + 1) || typeof decision.notes !== "string" || !decision.notes.trim()) throw new Error("Generated source audit did not approve every stated acceptance condition");
  for (const file of files) if (hash(await readFile(file.path)) !== file.sha256) throw new Error("Generated source changed after review");
  await sink.record("three-node-generated-source-reviewed", { requestPath, decisionPath, request_sha256: requestHash, files, notes: decision.notes }, { phase: "executing", owner: "three-node-acceptance" });
  return { request_sha256: requestHash, files };
}

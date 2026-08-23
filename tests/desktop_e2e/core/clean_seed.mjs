import crypto from "node:crypto";
import path from "node:path";
import {
  lstat,
  mkdir,
  readFile,
  readdir,
  realpath,
  writeFile,
} from "node:fs/promises";

function deepFreeze(value) {
  if (value && typeof value === "object" && !Object.isFrozen(value)) {
    Object.freeze(value);
    for (const nested of Object.values(value)) deepFreeze(nested);
  }
  return value;
}

export const CASE5_2_CLEAN_SEED_COPY_RULE = deepFreeze({
  id: "case5_2-clean-seed.v1",
  comparison: "case-insensitive-path-segments",
  excluded_directory_names: [
    "node_modules",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".cache",
    ".venv",
    "venv",
    ".virtualenv",
    "virtualenv",
    ".next",
    ".test-dist",
    "playwright-report",
    "test-results",
    "build",
    "dist",
    "coverage",
    "htmlcov",
    "target",
  ],
  excluded_directory_suffixes: [".egg-info", ".dist-info"],
  excluded_relative_directories: ["backend/data"],
  excluded_file_names: [
    ".env",
    ".env.local",
    ".coverage",
    "coverage.xml",
    "next-env.d.ts",
    "task.md",
  ],
  excluded_file_suffixes: [".pyc", ".pyo"],
  preserved_evidence: [".env.example", "examples/templates/**"],
  link_policy: "reject-traversed-symbolic-links-and-realpath-escapes",
  destination_policy: "pre-existing-empty-physical-directory-create-new-only",
});

function sha256(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

function compareText(left, right) {
  if (left < right) return -1;
  if (left > right) return 1;
  return 0;
}

function portableRelative(root, candidate) {
  return path.relative(root, candidate).replaceAll("\\", "/");
}

function isSameOrDescendant(root, candidate) {
  const relative = path.relative(root, candidate);
  return relative === "" || (!path.isAbsolute(relative) && relative !== ".." && !relative.startsWith(`..${path.sep}`));
}

function exclusionFor(relativePath, kind) {
  const normalized = relativePath.replaceAll("\\", "/").toLowerCase();
  const segments = normalized.split("/");
  const excludedDirectoryNames = new Set(CASE5_2_CLEAN_SEED_COPY_RULE.excluded_directory_names);
  if (segments.some((segment) => excludedDirectoryNames.has(segment))) return "excluded-directory-name";
  if (segments.some((segment) => CASE5_2_CLEAN_SEED_COPY_RULE.excluded_directory_suffixes.some((suffix) => segment.endsWith(suffix)))) {
    return "excluded-directory-suffix";
  }
  if (CASE5_2_CLEAN_SEED_COPY_RULE.excluded_relative_directories.some(
    (directory) => normalized === directory || normalized.startsWith(`${directory}/`),
  )) return "excluded-relative-directory";
  if (kind === "file") {
    const leaf = segments.at(-1);
    if (CASE5_2_CLEAN_SEED_COPY_RULE.excluded_file_names.includes(leaf)) return "excluded-file-name";
    if (CASE5_2_CLEAN_SEED_COPY_RULE.excluded_file_suffixes.some((suffix) => leaf.endsWith(suffix))) {
      return "excluded-file-suffix";
    }
  }
  return null;
}

async function physicalDirectory(candidate, label) {
  const absolute = path.resolve(candidate);
  const item = await lstat(absolute);
  if (!item.isDirectory() || item.isSymbolicLink()) {
    throw new TypeError(`${label} is not a physical directory: ${absolute}`);
  }
  return { absolute, physical: await realpath(absolute) };
}

async function assertEmptyDirectory(candidate, label) {
  const directory = await physicalDirectory(candidate, label);
  const entries = await readdir(directory.physical);
  if (entries.length !== 0) throw new Error(`${label} must already exist and be empty: ${directory.physical}`);
  return directory;
}

async function inspectSource(sourceRoot) {
  const directories = [];
  const files = [];

  async function visit(directory) {
    const entries = await readdir(directory, { withFileTypes: true });
    entries.sort((left, right) => compareText(left.name, right.name));
    for (const entry of entries) {
      const candidate = path.join(directory, entry.name);
      const relativePath = portableRelative(sourceRoot, candidate);
      const item = await lstat(candidate);
      if (entry.isSymbolicLink() || item.isSymbolicLink()) {
        throw new Error(`clean seed source contains a symbolic link or reparse traversal: ${candidate}`);
      }
      if (!item.isDirectory() && !item.isFile()) {
        throw new Error(`clean seed source contains an unsupported filesystem entry: ${candidate}`);
      }
      const physical = await realpath(candidate);
      if (!isSameOrDescendant(sourceRoot, physical)) {
        throw new Error(`clean seed source realpath escaped its root: ${candidate} -> ${physical}`);
      }
      const kind = item.isDirectory() ? "directory" : "file";
      if (exclusionFor(relativePath, kind) !== null) continue;
      if (item.isDirectory()) {
        directories.push(relativePath);
        await visit(physical);
        continue;
      }
      const bytes = await readFile(physical);
      if (!Number.isSafeInteger(bytes.byteLength)) throw new Error(`clean seed file is too large to inventory safely: ${candidate}`);
      files.push({
        source_path: physical,
        path: relativePath,
        sha256: sha256(bytes),
        bytes: bytes.byteLength,
      });
    }
  }

  await visit(sourceRoot);
  directories.sort((left, right) => {
    const depth = left.split("/").length - right.split("/").length;
    return depth === 0 ? compareText(left, right) : depth;
  });
  files.sort((left, right) => compareText(left.path, right.path));
  return { directories, files };
}

function publicCopyRule() {
  return JSON.parse(JSON.stringify(CASE5_2_CLEAN_SEED_COPY_RULE));
}

function publicInventory(sourceDirectory, inventory, destination = undefined) {
  const files = inventory.files.map(({ path: relativePath, sha256: digest, bytes }) => ({
    path: relativePath,
    sha256: digest,
    bytes,
  }));
  const byteCount = files.reduce((total, entry) => total + entry.bytes, 0);
  if (!Number.isSafeInteger(byteCount)) throw new Error("clean seed byte count exceeds the safe integer range");
  const aggregate = files.map((entry) => `${entry.path}\0${entry.sha256}\0${entry.bytes}\n`).join("");
  return {
    schema_version: "desktop-e2e.clean-seed.v1",
    source: sourceDirectory.physical,
    ...(destination === undefined ? {} : { destination }),
    copy_rule: publicCopyRule(),
    file_count: files.length,
    byte_count: byteCount,
    aggregate_sha256: sha256(Buffer.from(aggregate, "utf8")),
    files,
  };
}

export async function inventoryCase52CleanSeed(source) {
  if (typeof source !== "string" || source.length === 0) throw new TypeError("clean seed source must be a path string");
  const sourceDirectory = await physicalDirectory(source, "clean seed source");
  const inventory = await inspectSource(sourceDirectory.physical);
  return publicInventory(sourceDirectory, inventory);
}

/**
 * Copy a case5_2 clean seed into a run context's already-created empty workspace.
 * Validation and source inventory complete before the first destination write.
 */
export async function copyCase52CleanSeed({ source, destination }) {
  if (typeof source !== "string" || source.length === 0) throw new TypeError("clean seed source must be a path string");
  if (typeof destination !== "string" || destination.length === 0) {
    throw new TypeError("clean seed destination must be a path string");
  }

  const sourceDirectory = await physicalDirectory(source, "clean seed source");
  const destinationDirectory = await assertEmptyDirectory(destination, "clean seed destination");
  if (
    isSameOrDescendant(sourceDirectory.physical, destinationDirectory.physical)
    || isSameOrDescendant(destinationDirectory.physical, sourceDirectory.physical)
  ) {
    throw new Error("clean seed source and destination must be disjoint directories");
  }

  const inventory = await inspectSource(sourceDirectory.physical);
  const recheckedDestination = await assertEmptyDirectory(destinationDirectory.physical, "clean seed destination");
  if (recheckedDestination.physical !== destinationDirectory.physical) {
    throw new Error("clean seed destination identity changed during validation");
  }

  for (const relativePath of inventory.directories) {
    await mkdir(path.join(destinationDirectory.physical, ...relativePath.split("/")), { recursive: false });
  }
  for (const entry of inventory.files) {
    const item = await lstat(entry.source_path);
    if (!item.isFile() || item.isSymbolicLink()) {
      throw new Error(`clean seed source file identity changed during copy: ${entry.path}`);
    }
    const physical = await realpath(entry.source_path);
    if (physical !== entry.source_path || !isSameOrDescendant(sourceDirectory.physical, physical)) {
      throw new Error(`clean seed source file escaped during copy: ${entry.path}`);
    }
    const bytes = await readFile(entry.source_path);
    if (bytes.byteLength !== entry.bytes || sha256(bytes) !== entry.sha256) {
      throw new Error(`clean seed source changed during copy: ${entry.path}`);
    }
    await writeFile(path.join(destinationDirectory.physical, ...entry.path.split("/")), bytes, { flag: "wx" });
  }

  return publicInventory(sourceDirectory, inventory, destinationDirectory.physical);
}

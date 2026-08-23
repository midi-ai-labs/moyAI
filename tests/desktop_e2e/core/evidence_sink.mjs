import crypto from "node:crypto";
import path from "node:path";
import { mkdir, lstat, readFile, writeFile } from "node:fs/promises";

function sha256(buffer) {
  return crypto.createHash("sha256").update(buffer).digest("hex");
}

function safeKind(value) {
  const kind = String(value ?? "").trim().toLowerCase().replace(/[^a-z0-9._-]+/g, "-").replace(/^-+|-+$/g, "");
  if (!/^[a-z0-9][a-z0-9._-]{1,95}$/.test(kind)) throw new TypeError(`invalid evidence kind: ${value}`);
  return kind;
}

function normalizedRelative(value) {
  if (typeof value !== "string" || value.length === 0) throw new TypeError("evidence path must be non-empty");
  if (path.isAbsolute(value) || path.win32.isAbsolute(value)) throw new TypeError(`absolute evidence path is forbidden: ${value}`);
  const normalized = value.replaceAll("\\", "/");
  const parts = normalized.split("/");
  if (parts.some((part) => part === "" || part === "." || part === "..")) {
    throw new TypeError(`unsafe evidence path: ${value}`);
  }
  for (const part of parts) {
    if (/[<>:"|?*\u0000-\u001f]/.test(part) || /[ .]$/.test(part)) throw new TypeError(`unsafe Windows evidence path: ${value}`);
    const stem = part.split(".")[0].toUpperCase();
    if (/^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])$/.test(stem)) throw new TypeError(`reserved Windows evidence path: ${value}`);
  }
  return normalized;
}

async function assertNoSymlinkChain(root, targetDirectory) {
  const relative = path.relative(root, targetDirectory);
  if (relative === "") return;
  let current = root;
  for (const part of relative.split(path.sep)) {
    current = path.join(current, part);
    try {
      const item = await lstat(current);
      if (item.isSymbolicLink()) throw new Error(`evidence path contains a symbolic link: ${current}`);
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
      return;
    }
  }
}

export class EvidenceSink {
  #sequence = 0;
  #sealed = false;
  #files = [];

  static async create(root) {
    const absolute = path.resolve(root);
    await mkdir(absolute, { recursive: false });
    await mkdir(path.join(absolute, "events"), { recursive: false });
    return new EvidenceSink(absolute);
  }

  constructor(root) {
    this.root = path.resolve(root);
  }

  get sealed() {
    return this.#sealed;
  }

  get eventCount() {
    return this.#sequence;
  }

  get fileInventory() {
    return this.#files.map((entry) => ({ ...entry }));
  }

  async #write(relativePath, bytes) {
    if (this.#sealed) throw new Error("evidence sink is already sealed");
    const relative = normalizedRelative(relativePath);
    const output = path.resolve(this.root, ...relative.split("/"));
    const boundary = `${this.root}${path.sep}`.toLowerCase();
    if (!output.toLowerCase().startsWith(boundary)) throw new Error(`evidence path escaped root: ${relative}`);
    const directory = path.dirname(output);
    await assertNoSymlinkChain(this.root, directory);
    await mkdir(directory, { recursive: true });
    await assertNoSymlinkChain(this.root, directory);
    await writeFile(output, bytes, { flag: "wx" });
    const stored = await readFile(output);
    const entry = { relative_path: relative, sha256: sha256(stored), size_bytes: stored.byteLength };
    this.#files.push(entry);
    return { ...entry };
  }

  async writeJson(relativePath, value) {
    const bytes = Buffer.from(`${JSON.stringify(value, null, 2)}\n`, "utf8");
    return this.#write(relativePath, bytes);
  }

  async writeBytes(relativePath, value) {
    const bytes = Buffer.isBuffer(value) ? value : Buffer.from(value);
    return this.#write(relativePath, bytes);
  }

  async record(kind, payload, meta = {}) {
    const normalizedKind = safeKind(kind);
    const sequence = this.#sequence + 1;
    const event = {
      schema_version: "desktop-e2e.event.v1",
      sequence,
      kind: normalizedKind,
      at: meta.at ?? new Date().toISOString(),
      phase: meta.phase ?? null,
      owner: meta.owner ?? null,
      payload: structuredClone(payload),
    };
    const name = `events/${String(sequence).padStart(6, "0")}-${normalizedKind}.json`;
    const identity = await this.writeJson(name, event);
    this.#sequence = sequence;
    return { event, identity };
  }

  async seal(result) {
    if (this.#sealed) throw new Error("evidence sink is already sealed");
    const resultIdentity = await this.writeJson("result.json", result);
    const inventory = this.fileInventory.sort((left, right) => left.relative_path.localeCompare(right.relative_path));
    const aggregateInput = inventory
      .map((entry) => `${entry.relative_path}\0${entry.sha256}\0${entry.size_bytes}\n`)
      .join("");
    const seal = {
      schema_version: "desktop-e2e.seal.v1",
      event_count: this.#sequence,
      result_sha256: resultIdentity.sha256,
      evidence_tree_sha256: sha256(Buffer.from(aggregateInput, "utf8")),
      files: inventory,
    };
    await this.#write("seal.json", Buffer.from(`${JSON.stringify(seal, null, 2)}\n`, "utf8"));
    this.#sealed = true;
    return structuredClone(seal);
  }
}

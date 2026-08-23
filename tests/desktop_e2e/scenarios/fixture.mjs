import path from "node:path";
import crypto from "node:crypto";
import { constants } from "node:fs";
import { copyFile, readFile, writeFile } from "node:fs/promises";

async function fileIdentity(candidate) {
  const bytes = await readFile(candidate);
  return {
    path: candidate,
    sha256: crypto.createHash("sha256").update(bytes).digest("hex"),
    size_bytes: bytes.byteLength,
  };
}

export function validFixtureSentinelName(value) {
  if (typeof value !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._-]{2,95}$/.test(value) || value.endsWith(".")) {
    return false;
  }
  const stem = value.split(".", 1)[0].toUpperCase();
  return !/^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])$/.test(stem);
}

export async function prepareDesktopFixture({
  context,
  sink,
  phase,
  owner,
  configSourcePath = null,
  configText = null,
  configMode = "present",
  sentinelName = "E2E_FIXTURE.txt",
  sentinelText = "moyAI Desktop E2E fixture.\n",
}) {
  if (configMode !== "present" && configMode !== "absent") {
    throw new TypeError(`unsupported fixture config mode: ${configMode}`);
  }
  if (configMode === "present" && (configSourcePath === null) === (configText === null)) {
    throw new TypeError("present fixture requires exactly one config source");
  }
  if (configMode === "absent" && (configSourcePath !== null || configText !== null)) {
    throw new TypeError("absent fixture cannot provide a config source");
  }
  if (sentinelName !== null && !validFixtureSentinelName(sentinelName)) {
    throw new TypeError(`invalid fixture sentinel name: ${sentinelName}`);
  }
  if (sentinelName === null && sentinelText !== "") {
    throw new TypeError("fixture without a sentinel requires empty sentinel text");
  }
  const sentinel = sentinelName === null ? null : path.join(context.paths.workspace, sentinelName);
  if (sentinel !== null) await writeFile(sentinel, sentinelText, { flag: "wx" });
  if (configMode === "absent") {
    // The missing path is the product input. Do not create a placeholder.
  } else if (configSourcePath !== null) {
    await copyFile(configSourcePath, context.paths.config_file, constants.COPYFILE_EXCL);
  } else {
    await writeFile(context.paths.config_file, configText, { flag: "wx" });
  }
  await writeFile(
    context.paths.prefs_file,
    `last_workspace = ${JSON.stringify(context.paths.workspace)}\nwindow_opacity_percent = 100\ndeleted_project_roots = []\n`,
    { flag: "wx" },
  );
  await sink.record("fixture-prepared", {
    workspace: context.paths.workspace,
    config: context.paths.config_file,
    data: context.paths.data,
    prefs: context.paths.prefs_file,
    webview: context.paths.webview,
    identities: {
      sentinel: sentinel === null ? null : await fileIdentity(sentinel),
      config: configMode === "present" ? await fileIdentity(context.paths.config_file) : null,
      preferences: await fileIdentity(context.paths.prefs_file),
    },
    config_mode: configMode,
  }, { phase, owner });
}

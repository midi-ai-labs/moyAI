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
  sentinelName = "E2E_FIXTURE.txt",
  sentinelText = "moyAI Desktop E2E fixture.\n",
}) {
  if ((configSourcePath === null) === (configText === null)) {
    throw new TypeError("exactly one fixture config source is required");
  }
  if (!validFixtureSentinelName(sentinelName)) {
    throw new TypeError(`invalid fixture sentinel name: ${sentinelName}`);
  }
  const sentinel = path.join(context.paths.workspace, sentinelName);
  await writeFile(sentinel, sentinelText, { flag: "wx" });
  if (configSourcePath !== null) {
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
      sentinel: await fileIdentity(sentinel),
      config: await fileIdentity(context.paths.config_file),
      preferences: await fileIdentity(context.paths.prefs_file),
    },
  }, { phase, owner });
}

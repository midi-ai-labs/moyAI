import path from "node:path";
import { createHash } from "node:crypto";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { readFile, writeFile } from "node:fs/promises";
import { desktopLaunchEnvironment, prepareDesktopFixtureEnvironment } from "../core/desktop_isolation.mjs";

const execute = promisify(execFile);

export async function assertNoLocalRunner(context) {
  if (context.desktopIsolation !== "fixture") throw new TypeError("Runner absence requires an isolated Desktop machine");
  const env = desktopLaunchEnvironment({ context, processTemp: path.join(path.dirname(context.paths.config), "temp") });
  let unavailable = false;
  try {
    await execute(path.join(path.dirname(context.binary), "moyai-runner.exe"), ["identity"], { env, windowsHide: true, timeout: 15000 });
  } catch (error) {
    if (error.code !== 1 || !error.stderr?.includes("Local Runner is unavailable")) throw error;
    unavailable = true;
  }
  if (!unavailable) throw new Error("Local-only Hub authentication unexpectedly started a Runner");
}

// Seed only the isolated machine's historical publication and empty journal.
// Enrollment, current permission and generation must still go through the GUI.
export async function preparePreviousPublication(context) {
  if (context.desktopIsolation !== "fixture") throw new TypeError("Previous publication requires an isolated Desktop machine");
  const { registry } = await prepareDesktopFixtureEnvironment(context);
  const { stdout } = await execute("pwsh.exe", ["-NoProfile", "-NonInteractive", "-Command",
    "[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value"], { windowsHide: true, timeout: 10000 });
  const sid = stdout.trim();
  if (!/^S-1-(?:\d+-)*\d+$/.test(sid)) throw new Error("Current Windows SID was not identified");
  const hub = "previous-hub", device = "previous-device";
  const journal = path.join(context.paths.data, `runner-shared-${createHash("sha256").update(`${hub}|${device}`).digest("hex")}.sqlite3`);
  await writeFile(journal, "", { flag: "wx" });
  await execute("sqlite3.exe", ["-batch", "-bail", journal,
    "CREATE TABLE runner_identity(singleton INTEGER PRIMARY KEY,hub_id TEXT NOT NULL,device_id TEXT NOT NULL);" +
    "CREATE TABLE runner_attempts(attempt_id TEXT PRIMARY KEY,generation INTEGER NOT NULL,entry_json TEXT NOT NULL);" +
    "INSERT INTO runner_identity VALUES(1,'previous-hub','previous-device'); PRAGMA user_version=1;"], { windowsHide: true, timeout: 10000 });
  const registryFile = path.join(registry, "registry.json");
  await writeFile(registryFile, JSON.stringify([{ hub_id: hub, device_id: device, environment_id: "", directory: "",
    config_path: context.paths.config_file.replaceAll("\\", "/"), scope: { kind: "device" }, operator_sid: sid }]), { flag: "wx" });
  return { registryFile, journal, journalHash: createHash("sha256").update(await readFile(journal)).digest("hex") };
}

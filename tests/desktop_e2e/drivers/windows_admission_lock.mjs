import path from "node:path";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

const bridge = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "windows_admission_lock.ps1");
const MUTEX_NAME = "Local\\moyAI.desktop-e2e.admission.v1";

function waitForExit(child, timeoutMs) {
  if (child.exitCode !== null) return Promise.resolve(child.exitCode);
  return new Promise((resolve, reject) => {
    const onExit = (code) => {
      clearTimeout(timer);
      resolve(code);
    };
    const timer = setTimeout(() => {
      child.removeListener("exit", onExit);
      reject(new Error("Desktop E2E admission helper did not exit"));
    }, timeoutMs);
    child.once("exit", onExit);
  });
}

export async function acquireDesktopAdmission({ timeoutMs = 10_000 } = {}) {
  if (process.platform !== "win32") throw new Error("Desktop E2E admission is only available on win32");
  const child = spawn("pwsh.exe", [
    "-NoLogo", "-NoProfile", "-NonInteractive", "-File", bridge, "-Name", MUTEX_NAME,
  ], { windowsHide: true, stdio: ["pipe", "pipe", "pipe"] });
  child.stderr.setEncoding("utf8");
  let stderr = "";
  child.stderr.on("data", (chunk) => { stderr += chunk; });
  const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
  let timer;
  let first;
  try {
    first = await Promise.race([
      new Promise((resolve, reject) => {
        lines.once("line", resolve);
        child.once("error", reject);
        child.once("exit", (code) => reject(new Error(`Desktop E2E admission helper exited before acquisition (${code}): ${stderr.trim()}`)));
      }),
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error("Desktop E2E admission acquisition timed out")), timeoutMs);
      }),
    ]);
  } catch (error) {
    child.kill();
    throw error;
  } finally {
    clearTimeout(timer);
    lines.close();
  }
  let identity;
  try { identity = JSON.parse(first); }
  catch (error) {
    child.kill();
    throw new Error(`Desktop E2E admission helper returned malformed identity: ${error.message}`);
  }
  if (identity?.acquired !== true) {
    child.stdin.end();
    await waitForExit(child, 5_000).catch(() => child.kill());
    const error = new Error("another Desktop E2E execution owns the machine admission mutex");
    error.code = "desktop-e2e-admission-busy";
    error.evidence = identity;
    throw error;
  }
  let released = false;
  return {
    identity: structuredClone(identity),
    async release() {
      if (released) return;
      released = true;
      child.stdin.end("release\n");
      const code = await waitForExit(child, 10_000);
      if (code !== 0) throw new Error(`Desktop E2E admission helper exited with ${code}: ${stderr.trim()}`);
    },
  };
}

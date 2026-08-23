import path from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const bridge = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "windows_process.ps1");

function collect(stream) {
  let value = "";
  stream.setEncoding("utf8");
  stream.on("data", (chunk) => { value += chunk; });
  return () => value;
}

export async function invokeWindowsProcess(action, parameters = {}, { timeoutMs = 30_000 } = {}) {
  if (process.platform !== "win32") throw new Error("Windows process adapter is only available on win32");
  const args = ["-NoLogo", "-NoProfile", "-NonInteractive", "-File", bridge, "-Action", action];
  for (const [name, value] of Object.entries(parameters)) {
    if (value === null || value === undefined || value === "") continue;
    args.push(`-${name}`, String(value));
  }
  const child = spawn("pwsh.exe", args, { windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
  const stdout = collect(child.stdout);
  const stderr = collect(child.stderr);
  const result = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      child.kill();
      reject(new Error(`Windows process adapter ${action} timed out`));
    }, timeoutMs);
    child.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.once("exit", (code) => {
      clearTimeout(timer);
      resolve({ code, stdout: stdout(), stderr: stderr() });
    });
  });
  if (result.code !== 0) throw new Error(`Windows process adapter ${action} failed: ${result.stderr.trim() || result.stdout.trim()}`);
  const text = result.stdout.trim();
  return text.length === 0 ? null : JSON.parse(text);
}

export function waitForChildExit(child, timeoutMs) {
  if (child.exitCode !== null) return Promise.resolve(true);
  return new Promise((resolve) => {
    const onExit = () => {
      clearTimeout(timer);
      resolve(true);
    };
    const timer = setTimeout(() => {
      child.removeListener("exit", onExit);
      resolve(false);
    }, timeoutMs);
    child.once("exit", onExit);
  });
}

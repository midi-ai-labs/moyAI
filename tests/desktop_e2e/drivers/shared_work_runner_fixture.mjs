import { createServer } from "node:http";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { open, readFile, writeFile, mkdir } from "node:fs/promises";
import path from "node:path";
import { createHash } from "node:crypto";
import { waitForObservation } from "../core/deadline.mjs";

const execute = promisify(execFile);
export async function startSharedWorkflowProvider() {
  const requests = [], failures = [], sockets = new Set();
  let release;
  const childGate = new Promise(resolve => { release = resolve; });
  const server = createServer(async (req, res) => {
    try {
      if (req.url === "/v1/models") { res.setHeader("Content-Type", "application/json"); res.end(JSON.stringify({ data: [{ id: "shared-workflow" }] })); return; }
      if (req.url !== "/v1/chat/completions" || req.method !== "POST") throw new Error("Unexpected provider route");
      const chunks = []; let length = 0;
      for await (const bytes of req) { length += bytes.length; if (length > 4 * 1024 * 1024) throw new Error("Provider fixture request bound"); chunks.push(bytes); }
      const request = JSON.parse(Buffer.concat(chunks)); requests.push(request);
      const messages = request.messages, task = JSON.stringify(messages.filter(m => m.role === "user"));
      const tool = id => messages.filter(m => m.role === "tool" && m.tool_call_id === id);
      const call = (id, name, args) => ({ role: "assistant", tool_calls: [{ index: 0, id, type: "function", function: { name, arguments: JSON.stringify(args) } }] });
      let delta, finish = "stop";
      if (task.includes("desktop-followup")) {
        if (tool("parent-child").length !== 1) throw new Error("Continuation lost or duplicated the original child result");
        delta = { role: "assistant", content: "追加依頼でも元の子の結果を一度だけ参照しました。" };
      } else if (task.includes("desktop-transfer-child")) {
        if (tool("child-output").length) delta = { role: "assistant", content: "solver result: result.txt に解析結果を保存しました。" };
        else if (tool("child-approval").length) {
          delta = call("child-output", "apply_patch", { patch_text: "*** Begin Patch\n*** Add File: result.txt\n+shared solver result 日本語\n*** End Patch" }); finish = "tool_calls";
        }
        else {
          await childGate;
          delta = call("child-approval", "shell", { command: "[System.IO.File]::WriteAllText((Join-Path (Get-Location) 'approved-marker.txt'), 'approved')", sandbox_permissions: "require_escalated", justification: "Write only the controlled shared fixture marker after explicit approval." }); finish = "tool_calls";
        }
      } else if (tool("parent-child").length) {
        if (tool("parent-child").length !== 1 || !tool("parent-child")[0].content.includes("solver result")) throw new Error("Parent did not consume the child exactly once");
        delta = { role: "assistant", content: "親は子の解析結果を一度だけ受け取りました。" };
      } else {
        if (!request.tools.some(t => t.function?.name === "shared_delegate")) throw new Error("Shared delegation tool is absent");
        delta = call("parent-child", "shared_delegate", { environment_id: "solver", title: "子の解析と成果保存", prompt: "desktop-transfer-child: create the controlled result file" }); finish = "tool_calls";
      }
      res.writeHead(200, { "Content-Type": "text/event-stream" });
      for (const [d, end] of [[delta, null], [{}, finish]]) res.write(`data: ${JSON.stringify({ id: "shared-workflow", object: "chat.completion.chunk", choices: [{ index: 0, delta: d, finish_reason: end }] })}\n\n`);
      res.end("data: [DONE]\n\n");
    } catch (error) { failures.push(error.message); res.writeHead(500, { "Content-Type": "application/json" }); res.end(JSON.stringify({ error: error.message })); }
  });
  server.on("connection", socket => { sockets.add(socket); socket.on("close", () => sockets.delete(socket)); });
  await new Promise((resolve, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", resolve); });
  return { baseUrl: `http://127.0.0.1:${server.address().port}/v1`, requests, failures, releaseChild: release,
    async close() { release(); const closed = new Promise(resolve => server.close(resolve)); for (const s of sockets) s.destroy(); await closed; } };
}

export async function startSharedWorkflowRunner({ context, deviceId, hubId, runnerBinary, runnerTestBinary, sink }) {
  if (!runnerTestBinary || !path.isAbsolute(runnerTestBinary)) throw new TypeError("The combined fixture requires an explicit current Runner libtest binary for isolated machine resource registration");
  const root = path.join(context.root, "shared-runner"); await mkdir(root);
  const parent = path.join(root, "analysis"), child = path.join(root, "solver"); await mkdir(parent); await mkdir(child);
  const settings = path.join(root, "shared-settings.json");
  await writeFile(settings, JSON.stringify({ version: 1, hub_id: hubId, device_id: deviceId,
    resource_scope: { kind: "device" }, environments: [
      { environment_id: "analysis", directory: parent, access_mode: "default", allowed_child_environments: ["solver"] },
      { environment_id: "solver", directory: child, access_mode: "default", allowed_child_environments: [] },
    ] }, null, 2), { flag: "wx" });
  const logPath = path.join(root, "runner.log"), log = await open(logPath, "wx");
  const env = { ...process.env, MOYAI_CONFIG_PATH: context.paths.config_file, MOYAI_DATA_DIR: context.paths.data };
  const registry = path.join(root, "isolated-machine-registry"); await mkdir(registry);
  const args = ["--exact", "runner::shared::process_fixture::isolated_runner_process", "--ignored", "--nocapture", "--test-threads=1"];
  const testBytes = await readFile(runnerTestBinary), cliBytes = await readFile(runnerBinary);
  const processChild = spawn(runnerTestBinary, args, { env: { ...env, MOYAI_TEST_RESOURCE_REGISTRY: registry, MOYAI_TEST_SHARED_SETTINGS: settings }, windowsHide: true, stdio: ["ignore", log.fd, log.fd] });
  let exited = false, exitCode = null; const exit = new Promise(resolve => { processChild.once("exit", code => { exited = true; exitCode = code; resolve(); }); });
  await new Promise((resolve, reject) => { processChild.once("spawn", resolve); processChild.once("error", reject); });
  const command = async args => JSON.parse((await execute(runnerBinary, args, { env, windowsHide: true, timeout: 10000, maxBuffer: 1024 * 1024 })).stdout);
  let incarnation;
  try {
    incarnation = (await waitForObservation({ label: "Actual Runner IPC starts", timeoutMs: 30000, pollMs: 300, retrySampleErrors: true,
      sample: async () => { if (exited) throw new Error(`Runner exited: ${await readFile(logPath, "utf8")}`); return command(["identity"]); }, accept: value => Boolean(value.identity?.runner_id) })).value.identity.runner_id;
  } catch (error) { processChild.kill(); await exit; await log.close(); throw error; }
  await sink.record("shared-actual-runner-started", { executable: runnerTestBinary, sha256: createHash("sha256").update(testBytes).digest("hex"), size_bytes: testBytes.length,
    cli: { executable: runnerBinary, sha256: createHash("sha256").update(cliBytes).digest("hex"), size_bytes: cliBytes.length }, args,
    process_id: processChild.pid, runner_id: incarnation, device_id: deviceId, settings, config: context.paths.config_file, data: context.paths.data,
    scope: "Actual RunnerHost/LocalListener/SharedWorker in cfg(test) process; only the machine registry is injected into the execution directory. Product binaries have no registry override.", registry }, { phase: "executing", owner: "shared-work-runner" });
  return { incarnation, root, parent, child, command,
    async close() {
      let forced = false;
      if (!exited) {
        try { await command(["shutdown", "--runner", incarnation]); let timer; try { await Promise.race([exit, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error("Runner shutdown deadline")), 25000); })]); } finally { clearTimeout(timer); } }
        catch { forced = true; processChild.kill(); await exit; }
      }
      await log.close();
      return { pass: !forced && exited && exitCode === 0, forced, exited, exit_code: exitCode, process_id: processChild.pid, log_path: logPath };
    } };
}

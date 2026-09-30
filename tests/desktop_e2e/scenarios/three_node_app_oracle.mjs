import { readFile, realpath, stat } from "node:fs/promises";
import { createHash } from "node:crypto";
import path from "node:path";
import { ANALYSIS_INPUT, analysisJobId, sameAnalysisJobId, resultAccepted, validateManifest, loopbackUrl, inside } from "../fixtures/three_node_acceptance.mjs";
import { invokeWindowsProcess } from "../drivers/windows_process.mjs";
import { wait } from "./hub_browser_enrollment.mjs";

export async function acceptanceJson(url, body) {
  const response = await fetch(loopbackUrl(url), { redirect: "error", signal: AbortSignal.timeout(15000),
    ...(body === undefined ? {} : { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) }) });
  if (!response.ok) throw new Error(`Acceptance HTTP ${response.status}: ${url}`);
  return response.json();
}

export async function submitAnalysisThroughUi(page, baseUrl, { readJob = acceptanceJson } = {}) {
  const displayed = page.getByTestId("job-id");
  const readId = async () => await displayed.count() === 0 ? null : (await displayed.textContent()).trim() || null;
  const previousId = await readId();
  await page.getByTestId("text-input").fill(ANALYSIS_INPUT);
  await page.getByTestId("submit-job").click();
  const id = await wait("Web submission displays a new job ID", readId, value => value !== null && value !== previousId);
  const job = await readJob(new URL(`/jobs/${encodeURIComponent(id)}`, baseUrl).href);
  if (!sameAnalysisJobId(job.id, id)) throw new Error("Web submission and API returned different job IDs");
  return job;
}

export function commandIdentifiesEntrypoint(command, entrypoint) {
  if (typeof command !== "string") return false;
  // Both absolute and workdir-relative interpreter arguments are legitimate.
  // Source review additionally checks the generated supervisor's working folder.
  const name = path.basename(entrypoint).toLowerCase().replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`(?:^|[\\s"'\\\\/])${name}(?:$|[\\s"'])`, "i").test(command);
}
export function listenerMatchesUrl(listener, value) {
  const url = new URL(value), port = Number(url.port || 80);
  const addresses = url.hostname === "[::1]" ? ["::1", "::"]
    : url.hostname === "localhost" ? ["127.0.0.1", "::1", "0.0.0.0", "::"] : ["127.0.0.1", "0.0.0.0", "::"];
  return listener.port === port && addresses.includes(listener.address);
}
async function runnerDescendant(pid, pc, processCommand) {
  if (!Number.isSafeInteger(pid) || pid <= 0) throw new Error("Invalid component process id");
  const chain = []; let current = pid;
  for (let depth = 0; depth < 12 && current !== pc.runner.identity.process_id; depth++) {
    const row = await processCommand("Capture", { ProcessId: current }); chain.push(row);
    current = row.parent_process_id;
  }
  if (!chain.length || current !== pc.runner.identity.process_id) throw new Error("Component is not descended from its assigned environment Runner");
  return chain;
}
export async function captureComponent(role, pid, pc, sink, generation, component, { processCommand = invokeWindowsProcess } = {}) {
  const chain = await runnerDescendant(pid, pc, processCommand);
  // The declared startup file may supervise the actual Worker/DB process.
  // Only this leaf's verified ancestry may identify it, never a sibling.
  const startupIndex = chain.findIndex(row => commandIdentifiesEntrypoint(row.command_line, component.entrypoint));
  if (startupIndex === -1) throw new Error(`${role} process command does not identify its entrypoint`);
  const artifact = await sink.writeJson(`owners/app-${role}-${generation}-${pid}.json`, chain[0]);
  const ownerPath = path.join(sink.root, ...artifact.relative_path.split("/"));
  const startup = chain[startupIndex];
  const startupArtifact = await sink.writeJson(`owners/app-${role}-${generation}-${pid}-startup-${startup.process_id}.json`, startup);
  const startupProcess = { role: `${role} startup`, pid: startup.process_id, chain: chain.slice(startupIndex),
    ownerPath: path.join(sink.root, ...startupArtifact.relative_path.split("/")) };
  // The role's control endpoint may belong to a supervisor while a distinct
  // worker child performs analysis. Both must belong to this exact Runner.
  const runnerArtifact = await sink.writeJson(`owners/app-${role}-${generation}-${pid}-runner.json`, pc.runnerOwner.owner);
  const runnerOwnerPath = path.join(sink.root, ...runnerArtifact.relative_path.split("/"));
  const listeners = await processCommand("ObserveTreeListeners", { ExecutionRoot: pc.context.root, OwnerPath: runnerOwnerPath });
  const serving = listeners.filter(row => listenerMatchesUrl(row, component.base_url));
  if (!serving.length) throw new Error(`${role} declared URL is not served within its assigned Runner tree`);
  const listenerProcesses = [];
  for (const listenerPid of new Set(serving.map(row => row.process_id))) {
    const listenerChain = await runnerDescendant(listenerPid, pc, processCommand);
    const listenerArtifact = await sink.writeJson(`owners/app-${role}-${generation}-${pid}-listener-${listenerPid}.json`, listenerChain[0]);
    listenerProcesses.push({ role: `${role} listener`, pid: listenerPid, chain: listenerChain,
      ownerPath: path.join(sink.root, ...listenerArtifact.relative_path.split("/")) });
  }
  return { role, pid, chain, startup_process: startupProcess, listeners: serving, listener_processes: listenerProcesses, ownerPath };
}
export function applicationProcessOwners(components) {
  const owners = new Map();
  for (const component of Object.values(components)) {
    for (const owner of [component, ...(component.startup_process ? [component.startup_process] : []), ...(component.listener_processes ?? [])]) {
      const identity = `${owner.pid}:${owner.chain[0].process_start_time_utc_ticks}`;
      if (!owners.has(identity)) owners.set(identity, owner);
    }
  }
  return [...owners.values()];
}
export async function componentExited(owner, context) {
  return !(await invokeWindowsProcess("ObserveOwner", { ExecutionRoot: context.root, OwnerPath: owner.ownerPath })).live;
}

export async function verifyThreeNodeApplication({ context, environments, pcs, sink, browser }) {
  const manifestPath = path.join(environments.api.directory, "acceptance.json");
  const manifestBytes = await readFile(manifestPath);
  const manifest = validateManifest(JSON.parse(manifestBytes.toString("utf8").replace(/^\uFEFF/, "")), environments);
  for (const [role, component] of Object.entries(manifest.components)) {
    if (!(await stat(component.entrypoint)).isFile() || !inside(await realpath(environments[role].directory), await realpath(component.entrypoint))) throw new Error("Component entrypoint escaped its actual role folder");
  }
  if (!(await stat(manifest.components.database.database_file)).isFile()) throw new Error("Missing durable database file");
  if (!inside(await realpath(environments.database.directory), await realpath(manifest.components.database.database_file))) throw new Error("Database real path escaped its role folder");
  const api = route => new URL(route, manifest.components.api.base_url).href;
  const control = action => acceptanceJson(manifest.control_url, action);
  const running = await control(), owners = {};
  for (const role of ["api", "worker", "database"]) owners[role] = await captureComponent(role, running[role]?.pid, pcs[role], sink, "initial", manifest.components[role]);
  if (new Set(Object.values(owners).map(row => row.pid)).size !== 3) throw new Error("Components share an actual process");
  const page = await browser.newPage();
  const checkpoints = [];
  const record = async (name, value) => { checkpoints.push({ name, value }); await sink.record("three-node-app-check", { name, value }, { phase: "executing", owner: "three-node-acceptance" }); };
  async function captureUiResult(status, name) {
    const result = page.getByTestId("job-result");
    await result.filter({ hasText: status }).waitFor();
    const text = await result.innerText();
    const screenshot = await sink.writeBytes(`screenshots/${name}.png`, await page.screenshot({ fullPage: true }));
    return { status, text, screenshot: screenshot.relative_path };
  }
  async function completed(id) {
    if (analysisJobId(id) === null) throw new Error("Analysis response has no valid job ID");
    return wait(`Analysis ${id} completes with exact independent oracle`, () => acceptanceJson(api(`/jobs/${encodeURIComponent(id)}`)), job => {
      if (job.status === "FAILED") throw new Error(`Unexpected FAILED analysis ${id}: ${JSON.stringify(job.error)}`);
      if (job.status === "COMPLETED" && !resultAccepted(job)) throw new Error(`Incorrect analysis result: ${JSON.stringify(job)}`);
      return sameAnalysisJobId(job.id, id) && resultAccepted(job);
    }, 60000);
  }
  try {
    await page.goto(manifest.ui_url);
    const submitted = await submitAnalysisThroughUi(page, manifest.components.api.base_url);
    const initial = await completed(submitted.id);
    await record("web-ui-submit-completed", { ...initial, ui: await captureUiResult("COMPLETED", "generated-app-completed") });

    await control({ action: "worker_stop" });
    await wait("Worker actually exits while UI/API and DB survive", () => componentExited(owners.worker, context), Boolean);
    const stopped = await control();
    if (stopped.worker.running !== false || stopped.api.pid !== owners.api.pid || stopped.database.pid !== owners.database.pid) throw new Error("Worker stop disturbed UI/API or DB");
    const pending = await submitAnalysisThroughUi(page, manifest.components.api.base_url);
    if (analysisJobId(pending.id) === null || pending.status !== "PENDING") throw new Error("Stopped Worker did not leave accepted job PENDING");
    await record("worker-stopped-ui-pending", { job: pending, ui: await captureUiResult("PENDING", "generated-app-worker-stopped-pending") });
    const pendingAt = Date.now();
    await wait("Queued job stays PENDING with Worker stopped", async () => {
      const job = await acceptanceJson(api(`/jobs/${encodeURIComponent(pending.id)}`));
      if (!sameAnalysisJobId(job.id, pending.id) || job.status !== "PENDING") throw new Error("Queued job changed without Worker");
      return { job, elapsed_ms: Date.now() - pendingAt };
    }, row => row.elapsed_ms >= 2000);
    await control({ action: "worker_start" });
    const restarted = await wait("Worker restarts as a distinct actual process", control, value => value.worker?.running && value.worker.pid !== owners.worker.pid);
    owners.worker = await captureComponent("worker", restarted.worker.pid, pcs.worker, sink, "restart", manifest.components.worker);
    await record("worker-stop-queue-restart-same-job", await completed(pending.id));

    await control({ action: "worker_stop" });
    await wait("Worker exits before arming exactly one failure", () => componentExited(owners.worker, context), Boolean);
    const failing = await submitAnalysisThroughUi(page, manifest.components.api.base_url);
    if (analysisJobId(failing.id) === null || failing.status !== "PENDING") throw new Error("Failure injection job must first be durably queued");
    await page.getByTestId("job-result").filter({ hasText: "PENDING" }).waitFor();
    await control({ action: "worker_fail_job", job_id: failing.id });
    await control({ action: "worker_start" });
    const failureWorker = await wait("Failure-injection Worker process starts", control, value => value.worker?.running && value.worker.pid !== owners.worker.pid);
    owners.worker = await captureComponent("worker", failureWorker.worker.pid, pcs.worker, sink, "failure-check", manifest.components.worker);
    const failed = await wait("Real job handler persists FAILED and reason", () => acceptanceJson(api(`/jobs/${encodeURIComponent(failing.id)}`)), value => sameAnalysisJobId(value.id, failing.id) && value.status === "FAILED" && Boolean(value.error), 60000);
    const failedUi = await captureUiResult("FAILED", "generated-app-failed-reason");
    if ((await control()).database.pid !== owners.database.pid) throw new Error("Failure case stopped DB instead of failing a job");
    const afterFailure = await acceptanceJson(api("/jobs"), { text: ANALYSIS_INPUT });
    await record("one-job-failure-then-normal-job", { failed, ui: failedUi, normal: await completed(afterFailure.id), reason_display_review: "Match the visible reason with the stored error using this screenshot and the reviewed UI source; error representation is not fixed." });

    const oldDb = owners.database;
    await control({ action: "database_restart" });
    await wait("Original DB process exits", () => componentExited(oldDb, context), Boolean);
    const restartedDb = await wait("DB runs under its own Runner after restart", control, value => value.database?.running && value.database.pid !== oldDb.pid);
    owners.database = await captureComponent("database", restartedDb.database.pid, pcs.database, sink, "restart", manifest.components.database);
    const persisted = await Promise.all([initial.id, pending.id, afterFailure.id].map(completed));
    const persistedFailure = await acceptanceJson(api(`/jobs/${encodeURIComponent(failing.id)}`));
    if (!sameAnalysisJobId(persistedFailure.id, failing.id) || persistedFailure.status !== "FAILED" || !persistedFailure.error) throw new Error("DB restart lost the failed job");
    await record("database-restart-preserves-history", { completed: persisted, failed: persistedFailure });
    return { manifest, manifestPath, manifestSha256: createHash("sha256").update(manifestBytes).digest("hex"), owners, checkpoints };
  } finally { await page.close(); }
}

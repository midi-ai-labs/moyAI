import crypto from "node:crypto";
import path from "node:path";
import { spawn } from "node:child_process";
import { lstat, mkdir, open, readFile } from "node:fs/promises";

import { DesktopE2eError, exactCleanupPassed } from "../core/execution.mjs";
import { CdpClient, assertLocalTargetEndpoint, discoverDevToolsEndpoint, waitForExactCdpTarget } from "./cdp.mjs";
import { auditClosedSqlite } from "./sqlite_cleanup.mjs";
import { acquireDesktopAdmission } from "./windows_admission_lock.mjs";
import { invokeWindowsProcess, waitForChildExit } from "./windows_process.mjs";
import { runWindowsExternalProcess } from "./windows_external_process.mjs";
import { desktopFixtureRoot, desktopLaunchEnvironment, desktopOwnersMatch, normalizeDesktopIsolation, prepareDesktopFixtureEnvironment } from "../core/desktop_isolation.mjs";

function arrayValue(value) {
  if (value === null || value === undefined) return [];
  return Array.isArray(value) ? value : [value];
}

function waitForSpawn(child) {
  return new Promise((resolve, reject) => {
    child.once("spawn", resolve);
    child.once("error", reject);
  });
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

async function waitForProfileZero(context, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  let rows = [];
  do {
    rows = arrayValue(await invokeWindowsProcess("Profile", { ExecutionRoot: context.root, ProfilePath: context.paths.webview }));
    if (rows.length === 0) return [];
    await delay(100);
  } while (Date.now() < deadline);
  return rows;
}

async function fileIdentity(candidate) {
  const bytes = await readFile(candidate);
  return { path: candidate, sha256: crypto.createHash("sha256").update(bytes).digest("hex"), size_bytes: bytes.byteLength };
}

async function optionalFileIdentity(candidate) {
  try { return await fileIdentity(candidate); }
  catch (error) {
    if (error?.code === "ENOENT") return null;
    throw error;
  }
}

const SCENARIO_ENVIRONMENT_KEY = /^MOYAI_[A-Z0-9_]{2,95}$/;
const HARNESS_OWNED_ENVIRONMENT = new Set([
  "MOYAI_CONFIG_PATH",
  "MOYAI_DATA_DIR",
  "MOYAI_DESKTOP_PREFS_PATH",
  "MOYAI_DESKTOP_E2E_ROOT",
]);

export function normalizeScenarioEnvironment(value) {
  if (value === undefined || value === null) return {};
  if (typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("scenario environment must be an object");
  }
  const normalized = {};
  for (const [key, entry] of Object.entries(value)) {
    if (!SCENARIO_ENVIRONMENT_KEY.test(key) || HARNESS_OWNED_ENVIRONMENT.has(key)) {
      throw new TypeError(`scenario environment key is not allowed: ${key}`);
    }
    if (typeof entry !== "string" || entry.includes("\0")) {
      throw new TypeError(`scenario environment value is invalid: ${key}`);
    }
    normalized[key] = entry;
  }
  return normalized;
}

export function desktopLaunchArguments(context, joinConfigPath = null) {
  const args = ["--dir", context.paths.workspace];
  if (joinConfigPath === null || joinConfigPath === undefined) return args;
  if (typeof joinConfigPath !== "string" || joinConfigPath.includes("\0") || joinConfigPath.length > 32767
    || !path.win32.isAbsolute(joinConfigPath)) throw new TypeError("joinConfigPath must be a bounded absolute Windows path");
  const relative = path.win32.relative(context.paths.workspace, joinConfigPath);
  if (!relative || relative === ".." || relative.startsWith("..\\") || path.win32.isAbsolute(relative)) {
    throw new TypeError("joinConfigPath must belong to the scenario workspace");
  }
  return [...args, "--join-config", joinConfigPath];
}

export function selectActiveCleanupDriver(currentDriver, initialDriver, { restartBegan }) {
  if (typeof restartBegan !== "boolean") throw new TypeError("restartBegan must be boolean");
  return currentDriver ?? (restartBegan ? null : initialDriver);
}

function skippedClosedStore(database, required, reason) {
  return {
    present: null,
    path: database,
    checkpoint: null,
    quick_check: null,
    foreign_key_violation_count: null,
    sidecars: [],
    required,
    pass: false,
    skipped_reason: reason,
  };
}

export async function releaseResourcesThenAuditClosedStore({
  context,
  scenario,
  inputs,
  desktopExited,
  profileRows,
  releaseScenarioResources,
  auditSqlite = auditClosedSqlite,
}) {
  if (profileRows !== null && !Array.isArray(profileRows)) throw new TypeError("profileRows must be an array or null");
  if (typeof releaseScenarioResources !== "function") throw new TypeError("releaseScenarioResources callback is required");
  const required = scenario.databaseRequired === true && inputs.acquisition === "pass";
  const scenarioQuiesce = await releaseScenarioResources();
  if (desktopExited !== true || profileRows === null || profileRows.length !== 0) {
    return {
      scenarioQuiesce,
      sqlite: skippedClosedStore(
        context.paths.database,
        required,
        profileRows === null ? "profile-owner-not-sampled" : "desktop-or-profile-owner-not-zero",
      ),
    };
  }
  if (scenarioQuiesce?.input !== "pass") {
    return {
      scenarioQuiesce,
      sqlite: skippedClosedStore(context.paths.database, required, "scenario-resources-not-quiesced"),
    };
  }
  const sqlite = await auditSqlite({
    executionRoot: context.root,
    database: context.paths.database,
    required,
  });
  return { scenarioQuiesce, sqlite };
}

export class WindowsTauriHost {
  #admission = null;
  #desktop = null;
  #desktopOwnerPath = null;
  #driver = null;
  #generation = 0;
  #restartBegan = false;
  #lastEndpoint = null;
  #logPaths = [];
  #stdoutHandle = null;
  #stderrHandle = null;
  #processTemp = null;
  #launchEnvironment = null;
  #duplicateCount = 0;
  #activeContext = null;
  #preservedDesktops = null;
  #desktopOwner = null;
  #parentHost = null;
  #companions = [];

  async #checkDesktops(owned = []) {
    if (this.#preservedDesktops === null) return null;
    const observed = arrayValue(await invokeWindowsProcess("ListDesktop"));
    if (!desktopOwnersMatch(this.#preservedDesktops, observed, owned)) {
      throw new DesktopE2eError("environment", "desktop-singleton-race", "Desktop ownership changed after preflight", {
        preserved_desktops: this.#preservedDesktops, expected_owned: owned, observed_desktops: observed,
      });
    }
    return observed;
  }

  async preflight({ context, sink, phase }) {
    if (process.platform !== "win32") {
      throw new DesktopE2eError("environment", "windows-required", "actual Desktop qualification requires Windows");
    }
    try {
      if (this.#parentHost === null) this.#admission = await acquireDesktopAdmission();
      else if (this.#parentHost.#admission === null) throw new Error("companion parent has no active admission");
    } catch (error) {
      if (error?.code === "desktop-e2e-admission-busy") {
        throw new DesktopE2eError("environment", error.code, error.message, error.evidence ?? null);
      }
      throw error;
    }
    if (this.#admission !== null) await sink.record("admission-acquired", this.#admission.identity, { phase, owner: "process-ledger" });
    const existing = arrayValue(await invokeWindowsProcess("ListDesktop"));
    this.#preservedDesktops = existing;
    const isolation = normalizeDesktopIsolation(context.desktopIsolation);
    await sink.record("desktop-preexisting-owners", { desktop_isolation: isolation, processes: existing }, { phase, owner: "process-ledger" });
    if (existing.length > 0 && isolation !== "fixture") {
      throw new DesktopE2eError(
        "environment",
        "desktop-already-running",
        "an existing moyai-desktop.exe prevents exact single-instance qualification",
        { processes: existing },
      );
    }
    await sink.record("preflight-pass", {
      binary: context.manifest.binary,
      harness_tree_sha256: context.manifest.harness.tree_sha256,
      sealed_manifest: context.sealedManifest,
      existing_desktop_count: existing.length,
      desktop_isolation: isolation,
    }, { phase, owner: "run-context" });
  }

  async launch({ context, scenario, sink, phase }) {
    this.#activeContext = context;
    const launchStartedAt = new Date().toISOString();
    this.#generation += 1;
    await this.#checkDesktops();
    const fixture = normalizeDesktopIsolation(context.desktopIsolation) === "fixture";
    const fixtureRoot = fixture ? (await prepareDesktopFixtureEnvironment(context)).root : null;
    const processTemp = fixture ? path.join(fixtureRoot, "temp") : path.join(context.paths.logs, "desktop-temp");
    if (this.#processTemp !== processTemp) {
      try { await mkdir(processTemp, { recursive: false }); } catch (error) { if (!fixture || error.code !== "EEXIST") throw error; }
      this.#processTemp = processTemp;
    }
    const processTempItem = await lstat(processTemp);
    if (!processTempItem.isDirectory() || processTempItem.isSymbolicLink()) {
      throw new DesktopE2eError("harness", "desktop-temp-owner-invalid", "Desktop process temp owner is not a physical directory", {
        process_temp: processTemp,
      });
    }
    const stdout = this.#generation === 1
      ? context.paths.stdout
      : path.join(context.paths.logs, `desktop.g${this.#generation}.stdout.log`);
    const stderr = this.#generation === 1
      ? context.paths.stderr
      : path.join(context.paths.logs, `desktop.g${this.#generation}.stderr.log`);
    this.#stdoutHandle = await open(stdout, "wx");
    this.#stderrHandle = await open(stderr, "wx");
    this.#logPaths.push(stdout, stderr);
    this.#launchEnvironment = desktopLaunchEnvironment({ context, scenarioEnvironment: normalizeScenarioEnvironment(scenario.environment), processTemp });
    this.#desktop = spawn(context.binary, desktopLaunchArguments(context, scenario.joinConfigPath), {
      cwd: context.paths.workspace,
      env: this.#launchEnvironment,
      windowsHide: false,
      stdio: ["ignore", this.#stdoutHandle.fd, this.#stderrHandle.fd],
    });
    await waitForSpawn(this.#desktop);
    const owner = await invokeWindowsProcess("Capture", {
      ProcessId: this.#desktop.pid,
      ExpectedExecutable: context.binary,
      ExpectedParentProcessId: process.pid,
    });
    this.#desktopOwner = owner;
    await this.#checkDesktops([owner]);
    const ownerName = this.#generation === 1 ? "owners/desktop.json" : `owners/desktop.g${this.#generation}.json`;
    const identity = await sink.writeJson(ownerName, owner);
    this.#desktopOwnerPath = path.join(sink.root, ...identity.relative_path.split("/"));
    await sink.record("desktop-launched", {
      generation: this.#generation,
      owner,
      stdout,
      stderr,
      process_temp: processTemp,
      desktop_isolation: normalizeDesktopIsolation(context.desktopIsolation),
      fixture_root: fixtureRoot,
      preserved_desktops: this.#preservedDesktops,
      launch_started_at: launchStartedAt,
    }, { phase, owner: "desktop-app" });
    return {
      generation: this.#generation,
      desktop_process_id: this.#desktop.pid,
      desktop_owner: owner,
      desktop_owner_path: this.#desktopOwnerPath,
      launch_started_at: launchStartedAt,
    };
  }

  async attach({ context, sink, runtime, phase }) {
    const endpoint = await discoverDevToolsEndpoint(context.paths.webview, { afterEndpoint: this.#lastEndpoint });
    const target = await waitForExactCdpTarget(
      endpoint.port,
      (candidate) => candidate.type === "page" && candidate.title === "moyAI" && candidate.url === "http://tauri.localhost/",
    );
    const profileRows = arrayValue(await invokeWindowsProcess("Profile", { ExecutionRoot: context.root, ProfilePath: context.paths.webview }));
    if (profileRows.length === 0) throw new Error("no execution-owned WebView2 process was observed");
    assertLocalTargetEndpoint(target, endpoint.port);
    const driver = await CdpClient.connect(target.webSocketDebuggerUrl);
    try {
      await sink.record("desktop-attached", {
        endpoint: { port: endpoint.port, active_port_file: endpoint.active_port_file },
        target: { id: target.id, type: target.type, title: target.title, url: target.url, webSocketDebuggerUrl: target.webSocketDebuggerUrl },
        desktop_process_id: runtime.desktop_process_id,
        profile_processes: profileRows,
      }, { phase, owner: "cdp-driver" });
      this.#lastEndpoint = endpoint;
      this.#driver = driver;
      return driver;
    } catch (error) {
      driver.close();
      throw error;
    }
  }

  async launchDuplicate({ context, sink, phase = "executing", joinConfigPath = null }) {
    if (this.#desktop === null || this.#driver === null || this.#launchEnvironment === null) {
      throw new DesktopE2eError("harness", "duplicate-owner-missing", "duplicate launch requires an attached live Desktop");
    }
    const label = `desktop-duplicate-${++this.#duplicateCount}`;
    const stdout = path.join(context.paths.logs, `${label}.stdout.log`);
    const stderr = path.join(context.paths.logs, `${label}.stderr.log`);
    this.#logPaths.push(stdout, stderr);
    // The common Job owner settles even a broken duplicate and its descendants.
    // Reuse the live generation's config, data, WebView profile and temp owners.
    const processResult = await runWindowsExternalProcess({
      executionRoot: context.root, executable: context.binary,
      args: desktopLaunchArguments(context, joinConfigPath), cwd: context.paths.workspace,
      env: this.#launchEnvironment, stdoutPath: stdout, stderrPath: stderr,
      timeoutMs: 10_000, label,
    });
    const observed = await this.#checkDesktops([this.#desktopOwner, ...this.#companions.map(child => child.host.#desktopOwner).filter(Boolean)]);
    const result = {
      process: processResult,
      stdout: await readFile(stdout, "utf8"), stderr: await readFile(stderr, "utf8"),
      desktops: normalizeDesktopIsolation(context.desktopIsolation) === "fixture"
        ? observed.filter(row => row.process_id === this.#desktopOwner.process_id) : observed,
      observed_desktops: observed,
    };
    await sink.record("desktop-duplicate-launched", result, { phase, owner: "desktop-app" });
    return result;
  }

  async #closeCurrentLogs() {
    let primary = null;
    try { await this.#stdoutHandle?.close(); } catch (error) { primary ??= error; }
    try { await this.#stderrHandle?.close(); } catch (error) { primary ??= error; }
    this.#stdoutHandle = null;
    this.#stderrHandle = null;
    if (primary !== null) throw primary;
  }

  async restart({ context, nextContext = context, scenario, sink, driver, phase = "executing" }) {
    if (this.#parentHost !== null || this.#companions.length) throw new Error("simultaneous companion sessions must finish through common cleanup before restart");
    if (normalizeDesktopIsolation(nextContext.desktopIsolation) !== normalizeDesktopIsolation(context.desktopIsolation)) throw new Error("Desktop isolation cannot change across restart");
    if (nextContext.root !== context.root || nextContext.binary !== context.binary || nextContext.paths.logs !== context.paths.logs) {
      throw new DesktopE2eError("harness", "restart-context-owner-drift", "Another Desktop fixture must keep the same execution, binary, and log owner");
    }
    for (const name of ["workspace", "config", "data", "prefs", "webview"]) {
      const relative = path.relative(context.root, nextContext.paths[name]);
      if (!relative || relative.startsWith("..") || path.isAbsolute(relative)) throw new DesktopE2eError("harness", "restart-context-outside-execution", "Desktop fixture paths must remain inside the execution");
    }
    const activeDriver = this.#driver ?? driver;
    if (activeDriver === null || this.#desktop === null) {
      throw new DesktopE2eError("harness", "restart-owner-missing", "Desktop restart requires an attached live generation");
    }
    this.#restartBegan = true;
    const previous = {
      generation: this.#generation,
      desktop_process_id: this.#desktop.pid,
      desktop_owner_path: this.#desktopOwnerPath,
      endpoint: this.#lastEndpoint === null ? null : {
        port: this.#lastEndpoint.port,
        browser_path: this.#lastEndpoint.browser_path,
      },
    };
    const gracefulExit = await scenario.requestGracefulExit(activeDriver);
    if (gracefulExit?.requested !== true) {
      throw new DesktopE2eError("harness", "restart-graceful-exit-not-requested", "restart did not acquire a graceful Desktop exit", { previous, graceful_exit: gracefulExit });
    }
    activeDriver.close();
    this.#driver = null;
    const desktopExited = await waitForChildExit(this.#desktop, 20_000);
    const profileRows = await waitForProfileZero(context);
    if (!desktopExited || profileRows.length !== 0) {
      throw new DesktopE2eError("harness", "restart-generation-did-not-settle", "Desktop generation did not converge to exact zero before restart", {
        previous,
        graceful_exit: gracefulExit,
        desktop_exited: desktopExited,
        profile_processes: profileRows,
      });
    }
    await this.#closeCurrentLogs();
    this.#desktop = null;
    this.#desktopOwnerPath = null;
    this.#desktopOwner = null;
    await this.#checkDesktops();
    await sink.record("desktop-restart-boundary", {
      previous,
      graceful_exit: gracefulExit,
      desktop_exited: true,
      profile_webviews_remaining: 0,
    }, { phase, owner: "desktop-app" });
    await sink.record("desktop-next-fixture", { paths: nextContext.paths, previous_paths: context.paths }, { phase, owner: "run-context" });
    const runtime = await this.launch({ context: nextContext, scenario, sink, phase });
    const nextDriver = await this.attach({ context: nextContext, scenario, sink, runtime, phase });
    return {
      runtime,
      driver: nextDriver,
      restart: {
        previous_generation: previous.generation,
        next_generation: runtime.generation,
        previous_desktop_process_id: previous.desktop_process_id,
        next_desktop_process_id: runtime.desktop_process_id,
        graceful_exit: gracefulExit,
        zero_before_relaunch: true,
      },
    };
  }

  async #settleCurrent({ context, scenario, driver, inputs }) {
    context = this.#activeContext ?? context;
    const activeDriver = selectActiveCleanupDriver(this.#driver, driver, { restartBegan: this.#restartBegan });
    let gracefulExit = { requested: false, reason: activeDriver === null ? "not-attached" : "not-requested" };
    const cleanup = {
      admission_released: false,
      desktop_exited: this.#desktop === null,
      profile_webviews_remaining: null,
      sqlite: null,
      forced_desktop: false,
      forced_profile_process_ids: [],
      scenario_quiesce: null,
      logs: [],
    };
    let primary = null;
    if (activeDriver !== null) {
      try { gracefulExit = await scenario.requestGracefulExit(activeDriver); }
      catch (error) { gracefulExit = { requested: false, reason: error.message }; }
      try { activeDriver.close(); }
      catch (error) { primary ??= error; }
      this.#driver = null;
    }
    try {
      if (this.#desktop !== null) {
        cleanup.desktop_exited = await waitForChildExit(this.#desktop, 20_000);
      }
      if (!cleanup.desktop_exited && this.#desktopOwnerPath !== null) {
        await invokeWindowsProcess("StopOwner", { ExecutionRoot: context.root, OwnerPath: this.#desktopOwnerPath });
        cleanup.forced_desktop = true;
        cleanup.desktop_exited = await waitForChildExit(this.#desktop, 10_000);
      } else if (!cleanup.desktop_exited && this.#desktop !== null) {
        this.#desktop.kill();
        cleanup.forced_desktop = true;
        cleanup.desktop_exited = await waitForChildExit(this.#desktop, 10_000);
      }
    } catch (error) {
      primary ??= error;
    }
    let profileRows = null;
    try {
      profileRows = await waitForProfileZero(context);
      if (profileRows.length > 0) {
        const stopped = await invokeWindowsProcess("StopProfile", { ExecutionRoot: context.root, ProfilePath: context.paths.webview });
        cleanup.forced_profile_process_ids = arrayValue(stopped?.stopped_process_ids);
        profileRows = arrayValue(await invokeWindowsProcess("Profile", { ExecutionRoot: context.root, ProfilePath: context.paths.webview }));
      }
      cleanup.profile_webviews_remaining = profileRows.length;
    } catch (error) {
      primary ??= error;
      profileRows = null;
      cleanup.profile_webviews_remaining = null;
    }
    try {
      cleanup.preserved_desktops = await this.#checkDesktops(cleanup.desktop_exited ? [] : [this.#desktopOwner].filter(Boolean));
      cleanup.preserved_desktops_intact = this.#preservedDesktops === null ? null : true;
    } catch (error) { primary ??= error; cleanup.preserved_desktops_intact = false; }
    return { context, scenario, driver, inputs, cleanup, gracefulExit, primary, profileRows };
  }

  async #finishCleanup(settled, releaseScenarioResources) {
    const { context, scenario, inputs, cleanup, gracefulExit, profileRows } = settled;
    let primary = settled.primary;
    let scenarioReleaseAttempted = false;
    const orderedScenarioRelease = async () => {
      scenarioReleaseAttempted = true;
      return releaseScenarioResources();
    };
    try {
      const storageSettlement = await releaseResourcesThenAuditClosedStore({
        context,
        scenario,
        inputs,
        desktopExited: cleanup.desktop_exited,
        profileRows,
        releaseScenarioResources: orderedScenarioRelease,
      });
      cleanup.scenario_quiesce = storageSettlement.scenarioQuiesce;
      cleanup.sqlite = storageSettlement.sqlite;
    } catch (error) {
      primary ??= error;
      if (!scenarioReleaseAttempted) {
        try { cleanup.scenario_quiesce = await orderedScenarioRelease(); }
        catch (releaseError) { primary ??= releaseError; }
      }
      cleanup.sqlite ??= skippedClosedStore(
        context.paths.database,
        scenario.databaseRequired === true && inputs.acquisition === "pass",
        "closed-store-audit-stage-failed",
      );
    }
    try { await this.#closeCurrentLogs(); }
    catch (error) { primary ??= error; }
    try {
      const identities = await Promise.all(this.#logPaths.map((candidate) => optionalFileIdentity(candidate)));
      cleanup.logs = identities.filter((identity) => identity !== null);
    } catch (error) {
      primary ??= error;
    }
    cleanup.admission_scope = this.#parentHost === null ? "execution" : "borrowed-from-parent";
    cleanup.admission_released = this.#parentHost !== null;
    if (primary !== null) {
      throw new DesktopE2eError("harness", "exact-cleanup-failed", primary.message, { graceful_exit: gracefulExit, cleanup });
    }
    return {
      gracefulExit,
      cleanup,
      input: exactCleanupPassed({ acquisition: inputs.acquisition, gracefulExit, cleanup }) ? "pass" : "fail",
    };
  }

  async openCompanion({ context, scenario, sink, phase = "executing" }) {
    if (this.#parentHost !== null || this.#admission === null || this.#desktopOwner === null || normalizeDesktopIsolation(this.#activeContext?.desktopIsolation) !== "fixture") throw new Error("companion requires an attached fixture root host with admission");
    if (context.root !== this.#activeContext.root || context.binary !== this.#activeContext.binary || normalizeDesktopIsolation(context.desktopIsolation) !== "fixture" || !context.desktopName) throw new Error("companion execution context does not match its parent");
    const childRoot = desktopFixtureRoot(context), primaryRoot = desktopFixtureRoot(this.#activeContext);
    if (childRoot === primaryRoot || this.#companions.some(child => desktopFixtureRoot(child.context) === childRoot)) throw new Error("companion must have a distinct fixture root");
    await this.#checkDesktops([this.#desktopOwner, ...this.#companions.map(child => child.host.#desktopOwner).filter(Boolean)]);
    const childSink = sink.scope(`companions/${context.desktopName}`);
    await childSink.writeJson("execution-context.json", { desktop_isolation: "fixture", execution_id: context.executionId, desktop_name: context.desktopName, root: context.root, paths: context.paths, binary: context.manifest.binary });
    const host = new WindowsTauriHost(); host.#parentHost = this;
    const child = { context, scenario, sink: childSink, host, runtime: null, driver: null, inputs: { acquisition: "not_run" } };
    this.#companions.push(child);
    await host.preflight({ context, sink: childSink, phase });
    await scenario.prepare({ context, sink: childSink, phase });
    child.runtime = await host.launch({ context, scenario, sink: childSink, phase });
    child.driver = await host.attach({ context, sink: childSink, runtime: child.runtime, phase });
    child.inputs.acquisition = "pass";
    return { context, host, driver: child.driver, runtime: child.runtime, sink: childSink };
  }

  async cleanup({ context, scenario, driver, inputs, releaseScenarioResources }) {
    if (this.#parentHost !== null) throw new Error("companion cleanup is owned by its parent host");
    if (typeof releaseScenarioResources !== "function") throw new TypeError("releaseScenarioResources callback is required");
    // Every Desktop reaches process/profile zero before any shared resource is
    // released or any of their databases is audited. Reverse order preserves
    // the exact owner set observed when each companion joined.
    const children = [];
    for (const child of [...this.#companions].reverse()) children.push({ child, settled: await child.host.#settleCurrent(child) });
    const settled = await this.#settleCurrent({ context, scenario, driver, inputs });
    const childReleases = [];
    for (const { child } of children) {
      try {
        const outcome = await child.scenario.quiesce({ ...child, phase: "cleaning" });
        childReleases.push(outcome);
      } catch (error) { childReleases.push({ input: "fail", resources: [], error: error.message }); }
    }
    let release;
    try { release = await releaseScenarioResources(); }
    catch (error) { release = { input: "fail", resources: [], error: error.message }; }
    const companionResults = [];
    for (const [index, { child, settled: childSettled }] of children.entries()) {
      let outcome;
      try { outcome = await child.host.#finishCleanup(childSettled, async () => childReleases[index]?.input === "pass" && release?.input === "pass" ? childReleases[index] : { input: "fail", resources: childReleases[index]?.resources ?? [] }); }
      catch (error) { outcome = { input: "fail", error: error.message, ...error.evidence }; }
      try {
        const scenarioCleanup = await child.scenario.cleanup({ ...child, phase: "cleaning" });
        if (scenarioCleanup?.input !== "pass") outcome.input = "fail";
        outcome.scenario_cleanup = scenarioCleanup;
      } catch (error) { outcome.input = "fail"; outcome.scenario_cleanup_error = error.message; }
      companionResults.push({ desktop_name: child.context.desktopName, ...outcome });
      try { await child.sink.record("companion-cleanup", companionResults.at(-1), { phase: "cleaning", owner: "process-ledger" }); }
      catch (error) { companionResults.at(-1).input = "fail"; companionResults.at(-1).evidence_error = error.message; }
    }
    let result, failure;
    try { result = await this.#finishCleanup(settled, async () => release); }
    catch (error) { failure = error; result = { gracefulExit: error.evidence?.graceful_exit ?? settled.gracefulExit, cleanup: error.evidence?.cleanup ?? settled.cleanup, input: "fail" }; }
    result.cleanup.companions = companionResults;
    try {
      if (this.#admission !== null) await this.#admission.release();
      result.cleanup.admission_released = true;
    } catch (error) { failure ??= error; }
    result.input = failure === undefined && companionResults.every(row => row.input === "pass") && exactCleanupPassed({ acquisition: inputs.acquisition, gracefulExit: result.gracefulExit, cleanup: result.cleanup }) ? "pass" : "fail";
    if (failure) throw new DesktopE2eError("harness", "exact-cleanup-failed", failure.message, { graceful_exit: result.gracefulExit, cleanup: result.cleanup });
    return result;
  }
}

import crypto from "node:crypto";
import path from "node:path";
import { spawn } from "node:child_process";
import { open, readFile } from "node:fs/promises";

import { DesktopE2eError, exactCleanupPassed } from "../core/execution.mjs";
import { CdpClient, assertLocalTargetEndpoint, discoverDevToolsEndpoint, waitForExactCdpTarget } from "./cdp.mjs";
import { auditClosedSqlite } from "./sqlite_cleanup.mjs";
import { acquireDesktopAdmission } from "./windows_admission_lock.mjs";
import { invokeWindowsProcess, waitForChildExit } from "./windows_process.mjs";

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

  async preflight({ context, sink, phase }) {
    if (process.platform !== "win32") {
      throw new DesktopE2eError("environment", "windows-required", "actual Desktop qualification requires Windows");
    }
    try {
      this.#admission = await acquireDesktopAdmission();
    } catch (error) {
      if (error?.code === "desktop-e2e-admission-busy") {
        throw new DesktopE2eError("environment", error.code, error.message, error.evidence ?? null);
      }
      throw error;
    }
    await sink.record("admission-acquired", this.#admission.identity, { phase, owner: "process-ledger" });
    const existing = arrayValue(await invokeWindowsProcess("ListDesktop"));
    if (existing.length > 0) {
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
      existing_desktop_count: 0,
    }, { phase, owner: "run-context" });
  }

  async launch({ context, sink, phase }) {
    this.#generation += 1;
    const stdout = this.#generation === 1
      ? context.paths.stdout
      : path.join(context.paths.logs, `desktop.g${this.#generation}.stdout.log`);
    const stderr = this.#generation === 1
      ? context.paths.stderr
      : path.join(context.paths.logs, `desktop.g${this.#generation}.stderr.log`);
    this.#stdoutHandle = await open(stdout, "wx");
    this.#stderrHandle = await open(stderr, "wx");
    this.#logPaths.push(stdout, stderr);
    this.#desktop = spawn(context.binary, ["--dir", context.paths.workspace], {
      cwd: context.paths.workspace,
      env: {
        ...process.env,
        MOYAI_CONFIG_PATH: context.paths.config_file,
        MOYAI_DATA_DIR: context.paths.data,
        MOYAI_DESKTOP_PREFS_PATH: context.paths.prefs_file,
        WEBVIEW2_USER_DATA_FOLDER: context.paths.webview,
        WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-port=0",
        RUST_BACKTRACE: "1",
      },
      windowsHide: false,
      stdio: ["ignore", this.#stdoutHandle.fd, this.#stderrHandle.fd],
    });
    await waitForSpawn(this.#desktop);
    const owner = await invokeWindowsProcess("Capture", {
      ProcessId: this.#desktop.pid,
      ExpectedExecutable: context.binary,
      ExpectedParentProcessId: process.pid,
    });
    const desktops = arrayValue(await invokeWindowsProcess("ListDesktop"));
    const exactSingleton = desktops.length === 1
      && desktops[0].process_id === owner.process_id
      && desktops[0].process_start_time_utc_ticks === owner.process_start_time_utc_ticks
      && path.resolve(desktops[0].executable_path).toLowerCase() === path.resolve(owner.executable_path).toLowerCase();
    if (!exactSingleton) {
      throw new DesktopE2eError("environment", "desktop-singleton-race", "Desktop ownership changed between preflight and launch", {
        expected_owner: owner,
        observed_desktops: desktops,
      });
    }
    const ownerName = this.#generation === 1 ? "owners/desktop.json" : `owners/desktop.g${this.#generation}.json`;
    const identity = await sink.writeJson(ownerName, owner);
    this.#desktopOwnerPath = path.join(sink.root, ...identity.relative_path.split("/"));
    await sink.record("desktop-launched", { generation: this.#generation, owner, stdout, stderr }, { phase, owner: "desktop-app" });
    return {
      generation: this.#generation,
      desktop_process_id: this.#desktop.pid,
      desktop_owner: owner,
      desktop_owner_path: this.#desktopOwnerPath,
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

  async #closeCurrentLogs() {
    let primary = null;
    try { await this.#stdoutHandle?.close(); } catch (error) { primary ??= error; }
    try { await this.#stderrHandle?.close(); } catch (error) { primary ??= error; }
    this.#stdoutHandle = null;
    this.#stderrHandle = null;
    if (primary !== null) throw primary;
  }

  async restart({ context, scenario, sink, driver, phase = "executing" }) {
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
    await sink.record("desktop-restart-boundary", {
      previous,
      graceful_exit: gracefulExit,
      desktop_exited: true,
      profile_webviews_remaining: 0,
    }, { phase, owner: "desktop-app" });
    const runtime = await this.launch({ context, scenario, sink, phase });
    const nextDriver = await this.attach({ context, scenario, sink, runtime, phase });
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

  async cleanup({ context, scenario, driver, inputs, releaseScenarioResources }) {
    if (typeof releaseScenarioResources !== "function") throw new TypeError("releaseScenarioResources callback is required");
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
    if (this.#admission === null) cleanup.admission_released = true;
    else {
      try {
        await this.#admission.release();
        cleanup.admission_released = true;
      } catch (error) {
        primary ??= error;
      }
    }
    if (primary !== null) {
      throw new DesktopE2eError("harness", "exact-cleanup-failed", primary.message, { graceful_exit: gracefulExit, cleanup });
    }
    return {
      gracefulExit,
      cleanup,
      input: exactCleanupPassed({ acquisition: inputs.acquisition, gracefulExit, cleanup }) ? "pass" : "fail",
    };
  }
}

import path from "node:path";
import { mkdir, realpath, stat } from "node:fs/promises";
import { writeFileSync } from "node:fs";

import { runWindowsExternalProcess } from "./windows_external_process.mjs";
import { CdpClient, discoverDevToolsEndpoint, waitForExactCdpTarget, assertLocalTargetEndpoint } from "./cdp.mjs";
import { invokeWindowsProcess } from "./windows_process.mjs";
import { snapshotOwnedTopLevelWindows, selectSingleOwnedRootWindow, closeOwnedWindowForCleanup, TAURI_MAIN_WINDOW_CLASS } from "./windows_native_input.mjs";
import { waitForObservation } from "../core/deadline.mjs";

/** Hub and its gateway share the common external-process Job, not the Desktop singleton. */
export class HubTauriResource {
  #run = null;
  #outcome = null;
  #owner = null;
  #ownerPath = null;
  #context = null;
  #profile = null;
  #data = null;
  #cdp = null;
  #close = null;

  get driver() { if (!this.#cdp) throw new Error("Hub WebView is not attached"); return this.#cdp; }

  async start({ context, sink, binary, generation = 1, timeoutMs = 900_000 }) {
    if (this.#context) throw new Error("Hub resource may only start once");
    if (![1, 2].includes(generation)) throw new TypeError("Hub generation must be 1 or 2");
    this.#context = context;
    const root = path.join(context.root, generation === 1 ? "hub" : "hub-restart");
    const data = path.join(context.root, "hub", "data");
    this.#data = data;
    this.#profile = path.join(root, "webview");
    await mkdir(root);
    if (generation === 1) await mkdir(data);
    else if (path.resolve(await realpath(data)).toLowerCase() !== path.resolve(data).toLowerCase()) {
      throw new Error("Hub restart data must remain in the original physical execution directory");
    }
    await mkdir(this.#profile);
    this.#ownerPath = path.join(root, "owner.json");
    let notifyOwner, rejectOwner;
    const ready = new Promise((resolve, reject) => { notifyOwner = resolve; rejectOwner = reject; });
    this.#run = runWindowsExternalProcess({
      executionRoot: context.root, executable: binary, cwd: root,
      env: { ...process.env, MOYAI_HUB_DATA_DIR: data, WEBVIEW2_USER_DATA_FOLDER: this.#profile,
        WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-port=0" },
      stdoutPath: path.join(root, "stdout.log"), stderrPath: path.join(root, "stderr.log"),
      timeoutMs, cleanupTimeoutMs: 10_000, label: "hub-and-gateway",
      onOwner: (envelope) => {
        writeFileSync(this.#ownerPath, JSON.stringify(envelope.owner, null, 2), { flag: "wx" });
        this.#owner = envelope.owner;
        notifyOwner(envelope);
      },
    }).then((result) => {
      this.#outcome = { result, error: null };
      if (!this.#owner) rejectOwner(new Error("Hub exited before its owner was acquired"));
      return this.#outcome;
    }, (error) => {
      this.#outcome = { result: null, error: { message: error.message, code: error.code, evidence: error.evidence } };
      rejectOwner(error);
      return this.#outcome;
    });
    const envelope = await ready;
    await sink.record("hub-process-started", envelope, { phase: "executing", owner: "hub-resource" });
    const endpoint = await discoverDevToolsEndpoint(this.#profile, { timeoutMs: 45_000 });
    const target = await waitForExactCdpTarget(endpoint.port, (candidate) => candidate.type === "page"
      && ["http://tauri.localhost/", "https://tauri.localhost/", "tauri://localhost/"].includes(candidate.url), { timeoutMs: 20_000 });
    this.#cdp = await CdpClient.connect(assertLocalTargetEndpoint(target, endpoint.port));
    await this.#cdp.call("Runtime.enable");
    await sink.record("hub-webview-attached", { endpoint, target }, { phase: "executing", owner: "hub-resource" });
    return this;
  }

  close() {
    this.#close ??= this.#settle();
    return this.#close;
  }

  async #settle() {
    const failures = [];
    let forced = false;
    if (this.#owner && !this.#outcome) {
      try {
        const snapshot = await snapshotOwnedTopLevelWindows({ executionRoot: this.#context.root,
          ownerPath: this.#ownerPath, expectedOwner: this.#owner });
        const candidate = selectSingleOwnedRootWindow(snapshot, this.#owner, { expectedClassName: TAURI_MAIN_WINDOW_CLASS });
        await closeOwnedWindowForCleanup({ executionRoot: this.#context.root, ownerPath: this.#ownerPath, candidate });
        await waitForObservation({ label: "Hub and gateway Job settlement", timeoutMs: 15_000, pollMs: 100,
          sample: () => this.#outcome, accept: (value) => value !== null });
      } catch (error) {
        failures.push(error.message);
        forced = true;
        await invokeWindowsProcess("StopOwner", { ExecutionRoot: this.#context.root, OwnerPath: this.#ownerPath });
      }
    }
    this.#cdp?.close();
    if (this.#run) await this.#run;
    const rows = this.#profile ? await invokeWindowsProcess("Profile", { ExecutionRoot: this.#context.root, ProfilePath: this.#profile }) : [];
    const profileRows = rows === null ? [] : Array.isArray(rows) ? rows : [rows];
    let uncertainMarker = false;
    if (this.#data) {
      try { await stat(path.join(this.#data, "gateway-uncertain.json")); uncertainMarker = true; }
      catch (error) { if (error.code !== "ENOENT") failures.push(error.message); }
    }
    const result = this.#outcome?.result;
    const pass = failures.length === 0 && this.#outcome?.error === null && result?.job.descendant_zero === true
      && result.outcome.root_exit_code === 0 && !result.outcome.timed_out && !result.outcome.tree_termination_requested
      && profileRows.length === 0 && !forced && !uncertainMarker;
    return { input: pass ? "pass" : "fail", kind: "actual-hub-and-gateway", forced, failures,
      profile_process_count: profileRows.length, gateway_uncertain_marker_present: uncertainMarker, ...this.#outcome };
  }
}

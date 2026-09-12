import crypto from "node:crypto";
import http from "node:http";
import path from "node:path";
import { readFile } from "node:fs/promises";

import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { WebviewInput, assertTrustedProbeSequence, assertTrustedTextInsertion } from "../drivers/webview_input.mjs";
import { createManagedShellProviderScript, startScriptedProvider } from "../drivers/scripted_provider.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { providerChatToolContinuationFixtureConfig } from "./provider_chat_tool_continuation.mjs";
import { classifyAcquiredObservationFailure, quiesceProviderResource } from "./provider_restart.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";

const OWNER = "scenario:shell.managed-lifecycle";
export const MANAGED_SHELL_PROMPT = "Start the finite managed loopback fixture with shell_start exactly once, then reply only MANAGED_SHELL_STARTED. Leave it running for the application shutdown check.";
export const MANAGED_SHELL_RESPONSE = "MANAGED_SHELL_STARTED";
const LIFETIME_MS = 60_000;
const locator = (action) => ({ selector: `button[data-action="${action}"]`, identity: { tag: "BUTTON", action } });
const PROMPT = { selector: "section.composer textarea#prompt", identity: { tag: "TEXTAREA", id: "prompt" } };
const SEND = locator("send");
const FILE = { selector: '#titlebar-file-menu-trigger[data-action="show-file-menu"]', identity: { tag: "BUTTON", id: "titlebar-file-menu-trigger", action: "show-file-menu" } };
const EXIT = { selector: '#titlebar-file-menu button[data-action="exit-app"]', identity: { tag: "BUTTON", action: "exit-app" } };

export function managedShellServerCommand(portFile, marker) {
  if (!path.isAbsolute(portFile) || !/^[a-f0-9]{32}$/.test(marker)) throw new TypeError("An absolute port file and fixture marker are required");
  const quoted = portFile.replaceAll("'", "''");
  return `$ErrorActionPreference = 'Stop'
$fixtureListener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$fixtureClock = [Diagnostics.Stopwatch]::StartNew()
try {
  $fixtureListener.Start()
  $fixtureReady = @{ port = $fixtureListener.LocalEndpoint.Port; pid = $PID; marker = '${marker}'; started_at = [DateTime]::UtcNow.ToString('o'); lifetime_ms = ${LIFETIME_MS} }
  [IO.File]::WriteAllText('${quoted}', ($fixtureReady | ConvertTo-Json -Compress), [Text.UTF8Encoding]::new($false))
  $fixtureBody = [Text.Encoding]::UTF8.GetBytes('${marker}')
  $fixtureHeader = [Text.Encoding]::ASCII.GetBytes("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 32\r\nConnection: close\r\n\r\n")
  while ($fixtureClock.ElapsedMilliseconds -lt ${LIFETIME_MS}) {
    if (-not $fixtureListener.Pending()) { [Threading.Thread]::Sleep(20); continue }
    $fixtureClient = $fixtureListener.AcceptTcpClient()
    try {
      $fixtureStream = $fixtureClient.GetStream()
      $fixtureStream.ReadTimeout = 1000
      $fixtureStream.WriteTimeout = 1000
      $fixtureBuffer = [byte[]]::new(4096)
      [void]$fixtureStream.Read($fixtureBuffer, 0, $fixtureBuffer.Length)
      $fixtureStream.Write($fixtureHeader, 0, $fixtureHeader.Length)
      $fixtureStream.Write($fixtureBody, 0, $fixtureBody.Length)
      $fixtureStream.Flush()
    } finally { $fixtureClient.Dispose() }
  }
} finally { $fixtureListener.Stop() }`;
}

export function managedShellFixtureConfig(baseUrl) {
  // This isolated fixture checks lifecycle; permission decision behavior has its
  // own scenarios. No user's saved configuration or OS policy is changed.
  return providerChatToolContinuationFixtureConfig(baseUrl).replace('access_mode = "default"', 'access_mode = "full_access"');
}

export function probeManagedShell(port, marker) {
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new TypeError("Invalid fixture port");
  return new Promise((resolve) => {
    const started = Date.now();
    let settled = false;
    const finish = (value) => {
      if (settled) return;
      settled = true;
      resolve({ ...value, started_at_ms: started, finished_at_ms: Date.now() });
    };
    const request = http.get({ hostname: "127.0.0.1", port, path: "/health", agent: false }, (response) => {
      let body = "";
      response.on("data", (chunk) => { body += chunk; if (body.length > 256) request.destroy(new Error("Oversize fixture response")); });
      response.on("end", () => finish({ ready: response.statusCode === 200 && body === marker, refused: false, status: response.statusCode, body }));
      response.on("error", (error) => finish({ ready: false, refused: false, error: error.code ?? error.message }));
    });
    request.setTimeout(500, () => request.destroy(Object.assign(new Error("HTTP timeout"), { code: "ETIMEDOUT" })));
    request.on("error", (error) => finish({ ready: false, refused: error.code === "ECONNREFUSED", error: error.code ?? error.message }));
  });
}

export function shutdownBeforeNaturalExpiry(ready, observation, submittedAtMs) {
  const started = Date.parse(ready?.started_at);
  return Number.isFinite(started) && ready?.lifetime_ms === LIFETIME_MS
    && Number.isSafeInteger(submittedAtMs) && submittedAtMs > 0 && submittedAtMs <= started
    && observation?.refused === true && observation.finished_at_ms >= started
    && observation.finished_at_ms < submittedAtMs + LIFETIME_MS - 5000;
}

async function waitForProduct(args) {
  try { return await waitForObservation(args); }
  catch (error) { throw classifyAcquiredObservationFailure(error, { code: "managed-shell-observation-failed", message: args.label }); }
}

export function managedShellExitMenuReady(surface) {
  return surface?.overlay === "file_menu" && surface.expanded === "true"
    && surface.exit_count === 1 && surface.exit_visible === true && surface.exit_enabled === true;
}

async function observeExitMenu(cdp) {
  return cdp.evaluate(`(async () => {
    const state = await window.__TAURI_INTERNALS__.invoke('desktop_state');
    const trigger = document.querySelector('#titlebar-file-menu-trigger');
    const exits = document.querySelectorAll('#titlebar-file-menu button[data-action="exit-app"]');
    const exit = exits.length === 1 ? exits[0] : null;
    const bounds = exit?.getBoundingClientRect();
    const style = exit ? getComputedStyle(exit) : null;
    return { overlay: state.overlay, expanded: trigger?.getAttribute('aria-expanded') ?? null,
      exit_count: exits.length, exit_visible: exit !== null && bounds.width > 0 && bounds.height > 0
        && style.display !== 'none' && style.visibility !== 'hidden' && Number(style.opacity) !== 0,
      exit_enabled: exit !== null && !exit.matches(':disabled') && exit.getAttribute('aria-disabled') !== 'true'
        && exit.closest('[inert]') === null };
  })()`);
}

async function observeTerminal(cdp) {
  return cdp.evaluate(`(async () => {
    const projection = await window.__TAURI_INTERNALS__.invoke('desktop_state');
    const assistants = Array.from(document.querySelectorAll('main.conversation #thread article.message.assistant .markdown-body'));
    return { status: projection.run_status_key, busy: projection.busy, can_submit: projection.can_submit,
      overlay: projection.overlay, draft: projection.draft_prompt,
      assistants: assistants.map((row) => row.innerText.trim()),
      errors: document.querySelectorAll('.fatal, .ui-error-notice, main.conversation #thread article.message.error').length };
  })()`);
}

async function trustedClick(input, target) {
  const sequence = (await input.snapshotProbe()).sequence;
  const acquired = await input.click(target);
  const probe = assertTrustedProbeSequence(await input.snapshotProbe(sequence), {
    afterSequence: sequence,
    expected: [
      { type: "pointerdown", identity: target.identity, button: 0, buttons: 1 },
      { type: "pointerup", identity: target.identity, button: 0, buttons: 0 },
      { type: "click", identity: target.identity, button: 0, buttons: 0 },
    ],
  });
  return { acquired, probe };
}

export function createShellManagedLifecycleScenario() {
  const state = { provider: null, acceptedLedger: null, ready: null, sink: null, marker: crypto.randomBytes(16).toString("hex"),
    portFile: null, submittedAtMs: null, terminalAccepted: false, inputClean: true, guiExit: null, quiesce: null };
  return Object.freeze({
    id: "shell.managed-lifecycle", productOracle: "pass", manualGate: "not_required", databaseRequired: true,
    async prepare({ context, sink, phase }) {
      state.sink = sink;
      state.portFile = path.join(context.paths.workspace, "managed-shell-port.json");
      state.provider = await startScriptedProvider({
        expectedPrompt: MANAGED_SHELL_PROMPT,
        script: createManagedShellProviderScript({ taskPrompt: MANAGED_SHELL_PROMPT,
          command: managedShellServerCommand(state.portFile, state.marker), workdir: context.paths.workspace,
          responseText: MANAGED_SHELL_RESPONSE, timeoutMs: LIFETIME_MS }),
      });
      await prepareDesktopFixture({ context, sink, phase, owner: OWNER, configText: managedShellFixtureConfig(state.provider.baseUrl) });
      await sink.record("scripted-provider-started", state.provider.resourceObservation(), { phase, owner: OWNER });
    },
    async execute({ context, driver: cdp, sink }) {
      await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: OWNER, screenshotStem: "managed-shell-ready" });
      const input = new WebviewInput(cdp, { probeId: "managed-shell" });
      try {
        await input.installProbe();
        await trustedClick(input, PROMPT);
        const sequence = (await input.snapshotProbe()).sequence;
        await input.insertText(PROMPT, MANAGED_SHELL_PROMPT);
        const typed = assertTrustedTextInsertion(await input.snapshotProbe(sequence), { afterSequence: sequence, identity: PROMPT.identity, text: MANAGED_SHELL_PROMPT });
        state.submittedAtMs = Date.now();
        const send = await trustedClick(input, SEND);
        const terminal = await waitForProduct({ label: "managed shell normal agent completion", timeoutMs: 20_000, pollMs: 50,
          retrySampleErrors: false, sample: () => observeTerminal(cdp),
          accept: (value) => value.status === "completed" && value.busy === false && value.can_submit === true
            && value.errors === 0 && value.assistants.length === 1 && value.assistants[0] === MANAGED_SHELL_RESPONSE });
        state.acceptedLedger = structuredClone(state.provider.requestLedger);
        if (state.acceptedLedger.length !== 2 || state.acceptedLedger.some((row) => row.contract?.pass !== true || row.response_phase !== "completed")) {
          throw new DesktopE2eError("product", "managed-shell-wire-mismatch", "Expected exactly one shell_start and its continuation", state.acceptedLedger);
        }
        const ready = await waitForProduct({ label: "managed HTTP survives completed turn", timeoutMs: 5000, pollMs: 50, retrySampleErrors: false,
          sample: async () => {
            let record;
            try { record = JSON.parse(await readFile(state.portFile, "utf8")); }
            catch (error) { if (error.code === "ENOENT" || error instanceof SyntaxError) return null; throw error; }
            if (record.marker !== state.marker || !Number.isInteger(record.pid) || record.pid <= 0 || record.lifetime_ms !== LIFETIME_MS) {
              throw new DesktopE2eError("product", "managed-shell-ready-identity-mismatch", "Port file did not identify the owned fixture", record);
            }
            return { record, http: await probeManagedShell(record.port, state.marker) };
          }, accept: (value) => value?.http?.ready === true });
        state.ready = ready.value.record;
        state.terminalAccepted = true;
        await sink.record("managed-shell-survives-turn", { typed, send, submitted_at_ms: state.submittedAtMs, terminal: terminal.value, ready: ready.value, ledger: state.acceptedLedger }, { phase: "executing", owner: OWNER });
        await captureScenarioScreenshot({ cdp, sink, name: "managed-shell-turn-completed-http-alive", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } finally {
        try { await input.cleanup(); } catch (error) { state.inputClean = false; throw error; }
      }
    },
    async requestGracefulExit(cdp) {
      if (!state.terminalAccepted) return requestGracefulExit(cdp);
      const input = new WebviewInput(cdp, { probeId: "managed-shell-exit" });
      let stage = "http-before-exit";
      let menuEvidence = null;
      try {
        const before = await probeManagedShell(state.ready.port, state.marker);
        if (!before.ready || Date.now() >= state.submittedAtMs + LIFETIME_MS - 15_000) {
          throw new Error("Managed fixture is not alive early enough to distinguish GUI exit from expiry");
        }
        await input.installProbe();
        stage = "file-menu-trigger";
        const menu = await trustedClick(input, FILE);
        stage = "file-menu-settlement";
        const settled = await waitForProduct({ label: "File menu exposes visible Exit action", timeoutMs: 5000, pollMs: 25,
          retrySampleErrors: false, sample: () => observeExitMenu(cdp), accept: managedShellExitMenuReady });
        menuEvidence = { menu, surface: settled.value };
        await state.sink.record("managed-shell-file-menu-ready", menuEvidence, { phase: "cleaning", owner: OWNER });
        // Remove the DOM probe while the page still exists. The final click uses
        // trusted CDP input; host cleanup alone owns process waiting and forcing.
        await input.removeProbe();
        stage = "exit-click";
        const exit = await input.click(EXIT);
        state.guiExit = { requested: true, before, menu: menuEvidence, exit, requested_at_ms: Date.now() };
        await state.sink.record("managed-shell-gui-exit", state.guiExit, { phase: "cleaning", owner: OWNER });
        return { requested: true, reason: null, input_kind: "browser_trusted" };
      } catch (error) {
        state.guiExit = { requested: false, stage, error: error.message, code: error.code ?? null, evidence: error.evidence ?? null, menu: menuEvidence };
        return requestGracefulExit(cdp);
      } finally {
        try { await input.cleanup(); } catch { state.inputClean = false; }
      }
    },
    async quiesce({ inputs, sink }) {
      if (state.quiesce !== null) return structuredClone(state.quiesce);
      const provider = await quiesceProviderResource({ provider: state.provider, acceptedLedger: state.acceptedLedger, inputs });
      let stopped = null;
      if (state.terminalAccepted) {
        stopped = await probeManagedShell(state.ready.port, state.marker);
        await sink.record("managed-shell-after-desktop-exit", { ready: state.ready, submitted_at_ms: state.submittedAtMs, gui_exit: state.guiExit, http: stopped }, { phase: "cleaning", owner: OWNER });
      }
      const failed = state.terminalAccepted && (state.guiExit?.requested !== true || !shutdownBeforeNaturalExpiry(state.ready, stopped, state.submittedAtMs));
      state.quiesce = { input: provider.input === "pass" && state.inputClean ? "pass" : "fail",
        resources: [...provider.resources, { kind: "managed-shell-http", ready: state.ready, after_desktop_exit: stopped, gui_exit: state.guiExit }],
        productFailure: provider.productFailure ?? (failed ? { code: "managed-shell-gui-shutdown-unproved",
          message: "The owned HTTP listener did not refuse connections after GUI Exit before natural expiry", evidence: { ready: state.ready, stopped, gui_exit: state.guiExit } } : null) };
      return structuredClone(state.quiesce);
    },
    async cleanup() {
      return { input: state.quiesce?.input === "pass" && state.inputClean ? "pass" : "fail",
        resources: [{ kind: "managed-shell-lifecycle-verification", quiesced: state.quiesce !== null, input_clean: state.inputClean }] };
    },
  });
}

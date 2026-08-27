import { isDeepStrictEqual } from "node:util";

import { waitForObservation } from "../core/deadline.mjs";
import { canonicalU64, canonicalUlid } from "../core/canonical_identity.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import {
  DesktopCommandProbe,
  assertExactDesktopCommandSequence,
} from "../drivers/desktop_command_probe.mjs";
import {
  SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_MAX_RESPONSES,
  createPermissionRestartGuardianProviderScript,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";
import { WebviewInput, assertTrustedProbeSequence } from "../drivers/webview_input.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import {
  classifyAcquiredObservationFailure,
  observeProviderTurnSurface,
  quiesceProviderResource,
} from "./provider_restart.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";

const OWNER = "scenario:permission.restart-guardian";
const PRESSURE_KEY = "moyai.desktop-e2e.permission-restart-guardian-pressure.v1";
const PRESSURE_WORKERS = 8;
const PRESSURE_MINIMUM_COMPLETED = 512;
const PRESSURE_MAX_ITERATIONS_PER_WORKER = 4096;
const RESTART_STABILITY_MS = 300;

export const PERMISSION_RESTART_GUARDIAN_SEED_PROMPT = "seed guardian authority";
export const PERMISSION_RESTART_GUARDIAN_SEED_RESPONSE = "GUARDIAN_SEED_OK";
export const PERMISSION_RESTART_GUARDIAN_TASK_PROMPT = "run guardian fixture";
export const PERMISSION_RESTART_GUARDIAN_COMMAND = "Write-Output MOYAI_GUARDIAN_OK";
export const PERMISSION_RESTART_GUARDIAN_JUSTIFICATION = "exercise bounded automatic permission review";
export const PERMISSION_RESTART_GUARDIAN_RESPONSE = "GUARDIAN_FLOW_OK";

const PERMISSION_RESTART_GUARDIAN_ROLES = Object.freeze([
  "guardian_seed",
  "guardian_tool_initial",
  "guardian_review",
  "guardian_continuation",
]);

const PROMPT = Object.freeze({
  selector: "section.composer textarea#prompt",
  identity: { tag: "TEXTAREA", id: "prompt" },
});
const SEND = Object.freeze({
  selector: 'section.composer button[data-action="send"]',
  identity: { tag: "BUTTON", action: "send" },
});

function productFailure(code, message, evidence) {
  return new DesktopE2eError("product", code, message, evidence);
}

function harnessFailure(code, message, evidence) {
  return new DesktopE2eError("harness", code, message, evidence);
}

function errorObservation(error) {
  return {
    name: error?.name ?? "Error",
    code: error?.code ?? null,
    message: error?.message ?? String(error),
    evidence: error?.evidence ?? null,
  };
}

export function permissionRestartGuardianFixtureConfig(baseUrl) {
  return `[model]
base_url = ${JSON.stringify(baseUrl)}
model = "e2e/scripted-responses"
provider_profile = "lm_studio"
connect_timeout_ms = 1000
request_timeout_ms = 30000
max_retries = 0
context_window = 65536
supports_tools = true
supports_images = false
parallel_tool_calls = false

[permissions]
access_mode = "auto_review"

[multi_agent]
enabled = false
mode = "explicit_request_only"
max_concurrent_agents = 2
max_concurrent_model_requests = 1

[docling]
enabled = false

[mcp]
enabled = false
`;
}

function responseRows(ledger) {
  return Array.isArray(ledger) ? ledger.filter((row) => row?.route === "responses") : [];
}

function acceptedMetadataRow(row) {
  return (row?.route === "models" || row?.route === "lm_studio_models")
    && row.method === "GET"
    && row.response_phase === "completed"
    && row.response_status === 200;
}

function rejectedMetadataRow(row) {
  return (row?.route !== "models" && row?.route !== "lm_studio_models")
    || row?.method !== "GET"
    || row?.response_phase === "rejected"
    || (row?.response_status !== null && row.response_status !== 200);
}

function acceptedResponseRow(row, role, phase = "completed") {
  return row?.route === "responses"
    && row.method === "POST"
    && row.pathname === "/v1/responses"
    && row.query_present === false
    && row.contract?.pass === true
    && row.contract.role === role
    && row.response_phase === phase
    && row.response_status === (phase === "completed" ? 200 : null);
}

export function exactPermissionRestartGuardianLedger(ledger, expectedRoles, {
  heldRole = null,
} = {}) {
  if (!Array.isArray(ledger) || !Array.isArray(expectedRoles)) return false;
  if (ledger.some((row) => row?.route !== "responses" && !acceptedMetadataRow(row))) return false;
  const rows = responseRows(ledger);
  return rows.length === expectedRoles.length
    && rows.every((row, index) => acceptedResponseRow(
      row,
      expectedRoles[index],
      expectedRoles[index] === heldRole ? "held" : "completed",
    ));
}

export function permissionRestartGuardianReviewObserved(ledger) {
  if (!Array.isArray(ledger)
    || ledger.some((row) => row?.route !== "responses" && !acceptedMetadataRow(row))) return false;
  const rows = responseRows(ledger);
  return rows.length >= 3
    && rows.length <= PERMISSION_RESTART_GUARDIAN_ROLES.length
    && rows.every((row, index) => acceptedResponseRow(
      row,
      PERMISSION_RESTART_GUARDIAN_ROLES[index],
    ));
}

function providerOrSurfaceFailed(surface, ledger, maximumResponses) {
  const rows = responseRows(ledger);
  return !Array.isArray(ledger)
    || rows.length > maximumResponses
    || ledger.some((row) => row?.route !== "responses" && rejectedMetadataRow(row))
    || rows.some((row) => row?.contract?.pass === false
      || row?.response_phase === "rejected"
      || (row?.response_status !== null && row.response_status !== 200))
    || surface?.visible_fatal_count > 0
    || surface?.visible_recoverable_error_count > 0
    || surface?.projection?.startup?.status === "failed"
    || ["failed", "cancelled", "incomplete"].includes(surface?.projection?.run_status_key);
}

function primaryConversation(projection) {
  const rows = Array.isArray(projection?.transcript_rows) ? projection.transcript_rows : [];
  return {
    users: rows.filter((row) => row?.row_kind === "user").map((row) => row.body),
    assistants: rows.filter((row) => row?.row_kind === "assistant").map((row) => row.body),
    completedSummaries: rows.filter((row) => row?.row_kind === "work_summary_completed").length,
    errors: rows.filter((row) => row?.row_kind === "error").map((row) => row.body),
  };
}

function idleOwner(projection) {
  const expected = projection?.run_target?.expectedState;
  if (expected?.kind !== "idle"
    || !canonicalUlid(expected.latestTurnId)
    || !canonicalU64(expected.admissionRevision)
    || !canonicalUlid(projection?.run_target?.sessionId)
    || projection.run_target.sessionId !== projection?.draft_target?.sessionId) return null;
  return {
    sessionId: projection.run_target.sessionId,
    turnId: expected.latestTurnId,
    admissionRevision: expected.admissionRevision,
  };
}

function settledSurface(surface) {
  const projection = surface?.projection;
  return projection?.run_status_key === "completed"
    && projection?.task_activity_state === "idle"
    && projection?.busy === false
    && projection?.agent_tree_active === false
    && projection?.post_run_refresh_pending === false
    && projection?.background_mutation_pending === false
    && projection?.async_polling_required === false
    && Array.isArray(projection?.pending_async_operations)
    && projection.pending_async_operations.length === 0
    && projection?.navigation_loading === false
    && projection?.provider_loading === false
    && projection?.overlay === "none"
    && projection?.confirmation_visible === false
    && projection?.confirmation_id === null
    && projection?.composer_submit_mode === "new_request"
    && projection?.can_submit === true
    && idleOwner(projection) !== null
    && surface?.prompt?.visible === true
    && surface.prompt.enabled === true
    && surface?.send?.count === 1
    && surface.send.visible === true
    && surface.send.enabled === false
    && surface?.visible_fatal_count === 0
    && surface?.visible_recoverable_error_count === 0;
}

export function permissionRestartGuardianTerminalFailures(surface, seedOwner = null) {
  const projection = surface?.projection;
  const owner = idleOwner(projection);
  const conversation = primaryConversation(projection);
  const failures = [];
  if (!settledSurface(surface)) failures.push("terminal-surface-not-settled");
  if (!isDeepStrictEqual(conversation.users, [
    PERMISSION_RESTART_GUARDIAN_SEED_PROMPT,
    PERMISSION_RESTART_GUARDIAN_TASK_PROMPT,
  ])) failures.push("canonical-user-authority-conversation-mismatch");
  if (!isDeepStrictEqual(conversation.assistants, [
    PERMISSION_RESTART_GUARDIAN_SEED_RESPONSE,
    PERMISSION_RESTART_GUARDIAN_RESPONSE,
  ])) failures.push("canonical-assistant-conversation-mismatch");
  if (conversation.completedSummaries !== 2) failures.push("completed-summary-count-mismatch");
  if (conversation.errors.length !== 0) failures.push("canonical-error-row-present");
  if (typeof projection?.status_message === "string"
    && /storage is busy|guardian request failed/i.test(projection.status_message)) {
    failures.push("guardian-storage-failure-visible");
  }
  if (seedOwner !== null && (owner === null
    || owner.sessionId !== seedOwner.sessionId
    || owner.turnId === seedOwner.turnId
    || BigInt(owner.admissionRevision) !== BigInt(seedOwner.admissionRevision) + 1n)) {
    failures.push("post-restart-turn-owner-mismatch");
  }
  return [...new Set(failures)];
}

function seedTerminalFailures(surface) {
  const conversation = primaryConversation(surface?.projection);
  const failures = [];
  if (!settledSurface(surface)) failures.push("seed-terminal-not-settled");
  if (!isDeepStrictEqual(conversation.users, [PERMISSION_RESTART_GUARDIAN_SEED_PROMPT])) {
    failures.push("seed-user-mismatch");
  }
  if (!isDeepStrictEqual(conversation.assistants, [PERMISSION_RESTART_GUARDIAN_SEED_RESPONSE])) {
    failures.push("seed-assistant-mismatch");
  }
  if (conversation.completedSummaries !== 1 || conversation.errors.length !== 0) {
    failures.push("seed-terminal-history-mismatch");
  }
  return failures;
}

export function createPermissionRestartGuardianStableRestartDecision(
  seedOwner,
  minimumStableMs = RESTART_STABILITY_MS,
  now = () => Date.now(),
) {
  if (!Number.isFinite(minimumStableMs) || minimumStableMs <= 0) {
    throw new TypeError("permission Guardian restart stability must be positive");
  }
  let acceptedSince = null;
  return ({ surface, ledger }) => {
    if (providerOrSurfaceFailed(surface, ledger, 1)) {
      acceptedSince = null;
      return "fail";
    }
    const owner = idleOwner(surface?.projection);
    if (!exactPermissionRestartGuardianLedger(ledger, ["guardian_seed"])
      || seedTerminalFailures(surface).length !== 0) {
      acceptedSince = null;
      return settledSurface(surface) ? "fail" : "pending";
    }
    if (owner === null || !isDeepStrictEqual(owner, seedOwner)) {
      acceptedSince = null;
      return "fail";
    }
    const observedAt = now();
    acceptedSince ??= observedAt;
    return observedAt - acceptedSince >= minimumStableMs ? "pass" : "pending";
  };
}

function keyCode(character) {
  if (/^[a-z]$/.test(character)) return `Key${character.toUpperCase()}`;
  if (character === " ") return "Space";
  throw new TypeError(`unsupported permission restart Guardian character: ${character}`);
}

function expectedTypedEvents(text) {
  return Array.from(text).flatMap((character) => [
    { type: "keydown", identity: PROMPT.identity, key: character, code: keyCode(character) },
    { type: "input", identity: PROMPT.identity, inputType: "insertText", data: character },
    { type: "keyup", identity: PROMPT.identity, key: character, code: keyCode(character) },
  ]);
}

async function trustedClick(input, locator) {
  const start = (await input.snapshotProbe()).sequence;
  const target = await input.click(locator);
  const snapshot = await input.snapshotProbe(start);
  return {
    target,
    probe: assertTrustedProbeSequence(snapshot, {
      afterSequence: start,
      expected: [
        { type: "pointerdown", identity: locator.identity, button: 0, buttons: 1 },
        { type: "pointerup", identity: locator.identity, button: 0, buttons: 0 },
        { type: "click", identity: locator.identity, button: 0, buttons: 0 },
      ],
    }),
    sequence: snapshot.sequence,
  };
}

async function trustedType(input, text) {
  await trustedClick(input, PROMPT);
  const start = (await input.snapshotProbe()).sequence;
  await input.typeText(text);
  return assertTrustedProbeSequence(await input.snapshotProbe(start), {
    afterSequence: start,
    expected: expectedTypedEvents(text),
  });
}

async function waitForReadyComposer(cdp, prompt = "") {
  try {
    return (await waitForObservation({
      label: "permission restart Guardian ready composer",
      timeoutMs: 15_000,
      pollMs: 50,
      sample: () => observeProviderTurnSurface(cdp),
      accept: (surface) => surface?.projection?.can_submit === true
        && surface?.projection?.composer_submit_mode === "new_request"
        && surface?.prompt?.count === 1
        && surface.prompt.visible === true
        && surface.prompt.enabled === true
        && surface.prompt.value === prompt
        && surface?.send?.count === 1
        && surface.send.visible === true
        && surface.send.enabled === (prompt.length > 0)
        && surface?.visible_fatal_count === 0
        && surface?.visible_recoverable_error_count === 0,
      retrySampleErrors: false,
    })).value;
  } catch (error) {
    throw classifyAcquiredObservationFailure(error, {
      code: "permission-guardian-composer-not-ready",
      message: "the acquired Desktop composer did not expose the exact enabled Send state",
    });
  }
}

async function waitForExactCommand(commands, afterSequence, expected) {
  let observed;
  try {
    observed = await waitForObservation({
      label: "permission restart Guardian submit command",
      timeoutMs: 10_000,
      pollMs: 16,
      sample: () => commands.snapshot(afterSequence),
      accept: (snapshot) => snapshot.calls.length >= 1,
      retrySampleErrors: false,
    });
  } catch (error) {
    throw classifyAcquiredObservationFailure(error, {
      code: "permission-guardian-submit-command-missing",
      message: "the trusted Send action did not issue one exact submit_prompt command",
    });
  }
  return assertExactDesktopCommandSequence(observed.value, {
    afterSequence,
    expected: [expected],
  });
}

async function submitTrustedPrompt({ cdp, input, commands, prompt }) {
  await waitForReadyComposer(cdp);
  const typed = await trustedType(input, prompt);
  const ready = await waitForReadyComposer(cdp, prompt);
  const expected = {
    command: "submit_prompt",
    args: {
      text: prompt,
      expectedTarget: ready.projection.draft_target,
      expectedRunTarget: ready.projection.run_target,
    },
  };
  const start = (await commands.snapshot()).sequence;
  const click = await trustedClick(input, SEND);
  const command = await waitForExactCommand(commands, start, expected);
  return { typed, click, command, expected, commandStart: start };
}

async function exactSubmitLifetime(commands, submit) {
  return assertExactDesktopCommandSequence(await commands.snapshot(submit.commandStart), {
    afterSequence: submit.commandStart,
    expected: [submit.expected],
  });
}

async function waitForProductStage({ label, timeoutMs = 30_000, sample, decide, code, message }) {
  let decision = "pending";
  let observed;
  try {
    observed = await waitForObservation({
      label,
      timeoutMs,
      pollMs: 50,
      sample,
      accept: (value) => {
        decision = decide(value);
        return decision !== "pending";
      },
      retrySampleErrors: false,
    });
  } catch (error) {
    throw classifyAcquiredObservationFailure(error, { code, message });
  }
  if (decision === "fail") throw productFailure(code, message, observed.value);
  return observed.value;
}

function startPressureExpression() {
  return `(async () => {
    const key = Symbol.for(${JSON.stringify(PRESSURE_KEY)});
    if (globalThis[key] !== undefined) return { installed: false, reason: 'pressure-owner-exists' };
    const invoke = window.__TAURI_INTERNALS__?.invoke;
    if (typeof invoke !== 'function') return { installed: false, reason: 'tauri-invoke-unavailable' };
    const state = {
      workers: ${PRESSURE_WORKERS}, minimumCompleted: ${PRESSURE_MINIMUM_COMPLETED},
      maxIterationsPerWorker: ${PRESSURE_MAX_ITERATIONS_PER_WORKER},
      started: 0, completed: 0, inflight: 0, errors: [],
      stopRequested: false, exhausted: false, settled: false, promise: null,
    };
    const worker = async () => {
      let iterations = 0;
      while (!state.stopRequested && iterations < state.maxIterationsPerWorker) {
        iterations += 1;
        state.started += 1;
        state.inflight += 1;
        try { await invoke('desktop_state'); state.completed += 1; }
        catch (error) { state.errors.push(String(error)); }
        finally { state.inflight -= 1; }
        await new Promise((resolve) => setTimeout(resolve, 0));
      }
      if (!state.stopRequested && iterations >= state.maxIterationsPerWorker) state.exhausted = true;
    };
    state.promise = Promise.all(Array.from({ length: state.workers }, worker))
      .finally(() => { state.settled = true; });
    Object.defineProperty(globalThis, key, { configurable: true, value: state });
    await Promise.resolve();
    return {
      installed: true, workers: state.workers, minimumCompleted: state.minimumCompleted,
      maxIterationsPerWorker: state.maxIterationsPerWorker,
      started: state.started, completed: state.completed, inflight: state.inflight,
    };
  })()`;
}

function pressureSnapshotExpression({ stop = false, settle = false, remove = false } = {}) {
  return `(async () => {
    const key = Symbol.for(${JSON.stringify(PRESSURE_KEY)});
    const state = globalThis[key];
    if (!state) return { found: false };
    if (${stop ? "true" : "false"}) state.stopRequested = true;
    if (${settle ? "true" : "false"}) await state.promise;
    const snapshot = {
      found: true, workers: state.workers, minimumCompleted: state.minimumCompleted,
      maxIterationsPerWorker: state.maxIterationsPerWorker,
      started: state.started, completed: state.completed, inflight: state.inflight,
      errors: [...state.errors], stopRequested: state.stopRequested,
      exhausted: state.exhausted, settled: state.settled,
    };
    if (${remove ? "true" : "false"}) delete globalThis[key];
    return snapshot;
  })()`;
}

export function permissionRestartGuardianPressureFailures(snapshot) {
  const failures = [];
  if (snapshot?.found !== true) failures.push("pressure-owner-missing");
  if (snapshot?.workers !== PRESSURE_WORKERS
    || snapshot?.minimumCompleted !== PRESSURE_MINIMUM_COMPLETED
    || snapshot?.maxIterationsPerWorker !== PRESSURE_MAX_ITERATIONS_PER_WORKER) {
    failures.push("pressure-shape-mismatch");
  }
  if (snapshot?.stopRequested !== true || snapshot?.settled !== true || snapshot?.inflight !== 0) {
    failures.push("pressure-not-settled");
  }
  if (!Number.isInteger(snapshot?.started)
    || snapshot.started < PRESSURE_MINIMUM_COMPLETED
    || snapshot.started > PRESSURE_WORKERS * PRESSURE_MAX_ITERATIONS_PER_WORKER
    || snapshot?.completed !== snapshot.started) {
    failures.push("pressure-cardinality-mismatch");
  }
  if (snapshot?.exhausted !== false) failures.push("pressure-exhausted-before-stop");
  if (!Array.isArray(snapshot?.errors) || snapshot.errors.length !== 0) {
    failures.push("pressure-command-error");
  }
  return failures;
}

export function permissionRestartGuardianPressureOverlapFailures(snapshot) {
  const failures = [];
  if (snapshot?.found !== true) failures.push("pressure-owner-missing");
  if (snapshot?.workers !== PRESSURE_WORKERS
    || snapshot?.minimumCompleted !== PRESSURE_MINIMUM_COMPLETED
    || snapshot?.maxIterationsPerWorker !== PRESSURE_MAX_ITERATIONS_PER_WORKER) {
    failures.push("pressure-shape-mismatch");
  }
  if (!Number.isInteger(snapshot?.completed)
    || snapshot.completed < PRESSURE_MINIMUM_COMPLETED) failures.push("pressure-not-warmed");
  if (snapshot?.stopRequested !== false
    || snapshot?.settled !== false
    || snapshot?.exhausted !== false
    || !Number.isInteger(snapshot?.inflight)
    || snapshot.inflight <= 0) failures.push("pressure-not-active-at-guardian-review");
  if (!Array.isArray(snapshot?.errors) || snapshot.errors.length !== 0) {
    failures.push("pressure-command-error");
  }
  return failures;
}

async function startDesktopStatePressure(cdp) {
  const installed = await cdp.evaluate(startPressureExpression());
  if (installed?.installed !== true) {
    throw harnessFailure(
      "permission-guardian-pressure-install",
      "Desktop state pressure owner could not be installed in the actual WebView",
      installed,
    );
  }
  return (await waitForObservation({
    label: "permission Guardian Desktop state pressure active",
    timeoutMs: 10_000,
    pollMs: 10,
    sample: () => cdp.evaluate(pressureSnapshotExpression()),
    accept: (snapshot) => snapshot?.found === true
      && snapshot.completed >= PRESSURE_MINIMUM_COMPLETED
      && snapshot.inflight > 0
      && snapshot.stopRequested === false
      && snapshot.exhausted === false
      && snapshot.settled === false
      && Array.isArray(snapshot.errors)
      && snapshot.errors.length === 0,
    retrySampleErrors: false,
  })).value;
}

async function settleDesktopStatePressure(cdp) {
  return cdp.evaluate(pressureSnapshotExpression({ stop: true, settle: true, remove: true }));
}

async function settleProbes(state, input, commands, primaryError) {
  const outcome = { input: null, commands: null, failures: [] };
  if (input !== null) {
    try { outcome.input = await input.cleanup(); }
    catch (error) { outcome.failures.push({ owner: "webview-input", error: errorObservation(error) }); }
  }
  if (commands !== null) {
    try { outcome.commands = await commands.remove(); }
    catch (error) { outcome.failures.push({ owner: "desktop-command-probe", error: errorObservation(error) }); }
  }
  state.probeOutcomes.push(outcome);
  if (outcome.failures.length > 0 && primaryError === null) {
    throw harnessFailure(
      "permission-guardian-probe-cleanup",
      "permission restart Guardian input or command probe did not settle",
      outcome,
    );
  }
}

export function createPermissionRestartGuardianScenario() {
  const state = {
    provider: null,
    acceptedLedger: null,
    quiesceOutcome: null,
    probeOutcomes: [],
    pressureOutcome: null,
  };
  return Object.freeze({
    id: "permission.restart-guardian",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    requestGracefulExit,
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({
        script: createPermissionRestartGuardianProviderScript({
          seedPrompt: PERMISSION_RESTART_GUARDIAN_SEED_PROMPT,
          seedResponseText: PERMISSION_RESTART_GUARDIAN_SEED_RESPONSE,
          taskPrompt: PERMISSION_RESTART_GUARDIAN_TASK_PROMPT,
          command: PERMISSION_RESTART_GUARDIAN_COMMAND,
          justification: PERMISSION_RESTART_GUARDIAN_JUSTIFICATION,
          responseText: PERMISSION_RESTART_GUARDIAN_RESPONSE,
        }),
        responseBehavior: "hold_until_release",
      });
      await prepareDesktopFixture({
        context,
        sink,
        phase,
        owner: OWNER,
        configText: permissionRestartGuardianFixtureConfig(state.provider.baseUrl),
        sentinelName: "E2E_PERMISSION_RESTART_GUARDIAN.txt",
        sentinelText: "moyAI Desktop E2E restart and Guardian storage fixture.\n",
      });
      await sink.record("permission-guardian-provider-started", state.provider.resourceObservation(), {
        phase,
        owner: OWNER,
      });
    },
    async execute({ context, driver: firstCdp, host, sink }) {
      const provider = state.provider;
      if (provider === null) throw new Error("permission Guardian scripted provider was not prepared");
      await acquireInteractiveShell({ context, driver: firstCdp, sink }, {
        evidenceOwner: OWNER,
        screenshotStem: "permission-guardian-shell-ready",
      });

      let seedInput = new WebviewInput(firstCdp, { probeId: "permission-guardian-seed" });
      let seedCommands = new DesktopCommandProbe(firstCdp, {
        probeId: "permission-guardian-seed-commands",
        commands: ["submit_prompt", "cancel_run"],
      });
      let seedError = null;
      let seedProbesSettled = false;
      try {
        await seedInput.installProbe();
        await seedCommands.install();
        const submit = await submitTrustedPrompt({
          cdp: firstCdp,
          input: seedInput,
          commands: seedCommands,
          prompt: PERMISSION_RESTART_GUARDIAN_SEED_PROMPT,
        });
        const seedTerminal = await waitForProductStage({
          label: "permission Guardian seed terminal",
          sample: async () => ({
            surface: await observeProviderTurnSurface(firstCdp),
            ledger: provider.requestLedger,
          }),
          decide: ({ surface, ledger }) => {
            if (providerOrSurfaceFailed(surface, ledger, 1)) return "fail";
            if (!exactPermissionRestartGuardianLedger(ledger, ["guardian_seed"])) return "pending";
            const failures = seedTerminalFailures(surface);
            return failures.length === 0 ? "pass" : settledSurface(surface) ? "fail" : "pending";
          },
          code: "permission-guardian-seed-terminal",
          message: "the GUI seed turn did not reach one exact canonical terminal",
        });
        const seedOwner = idleOwner(seedTerminal.surface.projection);
        if (seedOwner === null) throw productFailure(
          "permission-guardian-seed-owner",
          "the GUI seed terminal did not expose one durable idle owner",
          seedTerminal,
        );
        const seedCommandLifetime = await exactSubmitLifetime(seedCommands, submit);
        const screenshot = await captureScenarioScreenshot({
          cdp: firstCdp,
          sink,
          name: "permission-guardian-seed-terminal",
          owner: OWNER,
        });
        await sink.record("permission-guardian-seed-completed", {
          submit,
          command_lifetime: seedCommandLifetime,
          owner: seedOwner,
          ledger: provider.requestLedger,
          screenshot,
        }, { phase: "executing", owner: OWNER });

        seedProbesSettled = true;
        await settleProbes(state, seedInput, seedCommands, null);
        seedInput = null;
        seedCommands = null;

        const restarted = await host.restart({
          context,
          scenario: this,
          sink,
          driver: firstCdp,
          phase: "executing",
        });
        await acquireInteractiveShell({ context, driver: restarted.driver, sink }, {
          evidenceOwner: OWNER,
          screenshotStem: "permission-guardian-restarted-shell",
        });
        const restartDecision = createPermissionRestartGuardianStableRestartDecision(seedOwner);
        const restored = await waitForProductStage({
          label: "permission Guardian restart parity",
          sample: async () => ({
            surface: await observeProviderTurnSurface(restarted.driver),
            ledger: provider.requestLedger,
          }),
          decide: restartDecision,
          code: "permission-guardian-restart-parity",
          message: "Desktop restart changed the seed authority owner or conversation",
        });

        const input = new WebviewInput(restarted.driver, { probeId: "permission-guardian-task" });
        const commands = new DesktopCommandProbe(restarted.driver, {
          probeId: "permission-guardian-task-commands",
          commands: ["submit_prompt", "cancel_run"],
        });
        let taskError = null;
        let pressureStarted = false;
        let taskProbesSettled = false;
        try {
          await input.installProbe();
          await commands.install();
          const submit = await submitTrustedPrompt({
            cdp: restarted.driver,
            input,
            commands,
            prompt: PERMISSION_RESTART_GUARDIAN_TASK_PROMPT,
          });
          const held = await waitForProductStage({
            label: "permission Guardian held elevated tool response",
            timeoutMs: 15_000,
            sample: async () => ({
              surface: await observeProviderTurnSurface(restarted.driver),
              ledger: provider.requestLedger,
            }),
            decide: ({ surface, ledger }) => {
              if (providerOrSurfaceFailed(surface, ledger, 2)) return "fail";
              if (exactPermissionRestartGuardianLedger(
                ledger,
                ["guardian_seed", "guardian_tool_initial"],
                { heldRole: "guardian_tool_initial" },
              )) return "pass";
              return responseRows(ledger).length >= 2 ? "fail" : "pending";
            },
            code: "permission-guardian-tool-hold",
            message: "the restart turn did not reach the exact elevated tool response barrier",
          });
          pressureStarted = true;
          const pressureActive = await startDesktopStatePressure(restarted.driver);
          const release = provider.releaseScriptRole("guardian_tool_initial");
          const guardianOverlap = await waitForProductStage({
            label: "permission Guardian review observed during continuous Desktop storage pressure",
            timeoutMs: 30_000,
            sample: async () => ({
              surface: await observeProviderTurnSurface(restarted.driver),
              ledger: provider.requestLedger,
              pressure: await restarted.driver.evaluate(pressureSnapshotExpression()),
            }),
            decide: ({ surface, ledger, pressure }) => {
              if (providerOrSurfaceFailed(
                surface,
                ledger,
                SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_MAX_RESPONSES,
              )) return "fail";
              if (pressure?.settled === true || pressure?.exhausted === true
                || (Array.isArray(pressure?.errors) && pressure.errors.length > 0)) return "fail";
              return permissionRestartGuardianReviewObserved(ledger)
                && permissionRestartGuardianPressureOverlapFailures(pressure).length === 0
                ? "pass"
                : "pending";
            },
            code: "permission-guardian-pressure-overlap",
            message: "the Guardian authority request was not observed while continuous Desktop state pressure remained active",
          });
          state.pressureOutcome = await settleDesktopStatePressure(restarted.driver);
          pressureStarted = false;
          const pressureFailures = permissionRestartGuardianPressureFailures(state.pressureOutcome);
          if (pressureFailures.length > 0) throw harnessFailure(
            "permission-guardian-pressure-drift",
            "the bounded actual-WebView Desktop state pressure did not complete exactly",
            { failures: pressureFailures, pressure: state.pressureOutcome },
          );
          const terminal = await waitForProductStage({
            label: "permission Guardian canonical terminal under Desktop storage pressure",
            timeoutMs: 45_000,
            sample: async () => ({
              surface: await observeProviderTurnSurface(restarted.driver),
              ledger: provider.requestLedger,
            }),
            decide: ({ surface, ledger }) => {
              if (providerOrSurfaceFailed(
                surface,
                ledger,
                SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_MAX_RESPONSES,
              )) return "fail";
              if (!exactPermissionRestartGuardianLedger(
                ledger,
                PERMISSION_RESTART_GUARDIAN_ROLES,
              )) return "pending";
              const failures = permissionRestartGuardianTerminalFailures(surface, seedOwner);
              return failures.length === 0 ? "pass" : settledSurface(surface) ? "fail" : "pending";
            },
            code: "permission-guardian-terminal",
            message: "the restarted AutoReview turn did not complete under bounded Desktop storage pressure",
          });
          const taskCommandLifetime = await exactSubmitLifetime(commands, submit);
          const screenshot = await captureScenarioScreenshot({
            cdp: restarted.driver,
            sink,
            name: "permission-guardian-terminal",
            owner: OWNER,
          });
          state.acceptedLedger = structuredClone(provider.requestLedger);
          await sink.record("permission-guardian-completed", {
            restart: restarted.restart,
            restored_owner: idleOwner(restored.surface.projection),
            submit,
            command_lifetime: taskCommandLifetime,
            held,
            pressure_active: pressureActive,
            guardian_overlap: guardianOverlap,
            pressure: state.pressureOutcome,
            provider_release: release,
            terminal_owner: idleOwner(terminal.surface.projection),
            provider_ledger: state.acceptedLedger,
            screenshot,
          }, { phase: "executing", owner: OWNER });
          taskProbesSettled = true;
          await settleProbes(state, input, commands, null);
        } catch (error) {
          taskError = error;
          if (pressureStarted) {
            try { state.pressureOutcome = await settleDesktopStatePressure(restarted.driver); }
            catch (pressureError) {
              state.pressureOutcome = { failure: errorObservation(pressureError) };
            }
          }
          if (!taskProbesSettled) {
            taskProbesSettled = true;
            await settleProbes(state, input, commands, taskError);
          }
          throw error;
        }
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        seedError = error;
        throw error;
      } finally {
        if (!seedProbesSettled) {
          seedProbesSettled = true;
          await settleProbes(state, seedInput, seedCommands, seedError);
        }
      }
    },
    async quiesce({ inputs }) {
      if (state.quiesceOutcome !== null) return structuredClone(state.quiesceOutcome);
      state.quiesceOutcome = await quiesceProviderResource({
        provider: state.provider,
        acceptedLedger: state.acceptedLedger,
        inputs,
      });
      return structuredClone(state.quiesceOutcome);
    },
    async cleanup() {
      const pressurePass = state.pressureOutcome === null
        || permissionRestartGuardianPressureFailures(state.pressureOutcome).length === 0;
      const pass = state.quiesceOutcome?.input === "pass"
        && state.probeOutcomes.every((outcome) => outcome.failures.length === 0)
        && pressurePass;
      return {
        input: pass ? "pass" : "fail",
        resources: [{
          kind: "permission-restart-guardian-verification",
          quiesce_input: state.quiesceOutcome?.input ?? null,
          probe_outcomes: state.probeOutcomes,
          pressure: state.pressureOutcome,
        }],
      };
    },
  });
}

import { isDeepStrictEqual } from "node:util";

import { canonicalU64, canonicalUlid } from "../core/canonical_identity.mjs";
import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import {
  DesktopCommandProbe,
  assertExactDesktopCommandSequence,
} from "../drivers/desktop_command_probe.mjs";
import { DesktopStatePollBarrier } from "../drivers/desktop_state_poll_barrier.mjs";
import {
  SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_MAX_RESPONSES,
  createToolErrorRecoveryProviderScript,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";
import {
  WebviewInput,
  assertTrustedProbeSequence,
  assertTrustedTextInsertion,
} from "../drivers/webview_input.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";
import {
  classifyAcquiredObservationFailure,
  providerRestartFixtureConfig,
  quiesceProviderResource,
} from "./provider_restart.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";

const OWNER = "scenario:history.terminal-reconcile";
const POLL_BARRIER_ID = "history-terminal-reconcile";
export const HISTORY_TERMINAL_RECONCILE_PROMPT = "exercise one deterministic tool failure";
export const HISTORY_TERMINAL_RECONCILE_RESPONSE = "TOOL_ERROR_RECOVERY_COMPLETE";
export const HISTORY_TERMINAL_RECONCILE_STREAMED_PREFIX = "TOOL_ERROR_RECOVERY_";
export const HISTORY_TERMINAL_RECONCILE_MISSING_PATH = "__moyai_history_terminal_missing__.txt";

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

function selectedSessionRow(projection) {
  const rows = projection?.selected_project_index >= 0
    ? projection?.session_rows
    : projection?.chat_session_rows;
  if (!Array.isArray(rows) || !Number.isInteger(projection?.selected_session_index)) return null;
  return projection.selected_session_index >= 0
    ? rows[projection.selected_session_index] ?? null
    : null;
}

export function terminalReconcilePrimaryRows(projection) {
  if (!Array.isArray(projection?.transcript_rows)) return null;
  return projection.transcript_rows
    .filter((row) => ["user", "error", "assistant"].includes(row?.row_kind))
    .map((row) => ({
      kind: row.row_kind,
      identity: typeof row.stable_history_identity === "string"
        && row.stable_history_identity.length > 0
        ? row.stable_history_identity
        : null,
      body: typeof row.body === "string" ? row.body : null,
    }));
}

export function exactToolErrorRecoveryLedger(ledger) {
  const roles = ["tool_error_initial", "tool_error_continuation"];
  return Array.isArray(ledger)
    && ledger.length === SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_MAX_RESPONSES
    && ledger.every((row, index) => row?.method === "POST"
      && row.pathname === "/v1/responses"
      && row.query_present === false
      && row.contract?.pass === true
      && row.contract.role === roles[index]
      && row.response_phase === "completed"
      && row.response_status === 200);
}

export function heldInitialProviderDecision(ledger, resource) {
  if (!Array.isArray(ledger) || ledger.length === 0) return "pending";
  if (ledger.length !== 1) return "fail";
  const row = ledger[0];
  if (row?.contract === null || row?.contract === undefined) {
    return row?.response_status === null || row?.response_status === undefined ? "pending" : "fail";
  }
  const exactRequest = row.method === "POST"
    && row.pathname === "/v1/responses"
    && row.query_present === false
    && row.route === "responses"
    && row.contract.pass === true
    && row.contract.role === "tool_error_initial";
  if (!exactRequest || row.response_status !== null) return "fail";
  if (row.response_phase !== "held") return row.response_phase === "completed" ? "fail" : "pending";
  return resource?.active_request_count === 1
    && resource.request_count === 1
    && resource.accepted_response_count === 1
    && resource.successful_response_count === 0
    && resource.script_kind === "tool_error_recovery"
    && resource.scripted_responses_request_count === 1
    && resource.scripted_responses_maximum === SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_MAX_RESPONSES
    && isDeepStrictEqual(resource.scripted_response_roles, ["tool_error_initial"])
    && resource.response_release_controlled === true
    && resource.response_release_count === 0
    && resource.response_release_cleanup_count === 0
    ? "pass"
    : "fail";
}

export function exactWaitingPollBarrier(snapshot) {
  return snapshot?.found === true
    && snapshot.barrier_id === POLL_BARRIER_ID
    && snapshot.phase === "waiting"
    && snapshot.intercepted_count === 1
    && snapshot.bypass_count === 0
    && snapshot.captured === null;
}

function settledTerminal(projection) {
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
    && projection?.turn_page_admission_open === true
    && projection?.provider_loading === false
    && projection?.overlay === "none"
    && projection?.confirmation_visible === false
    && projection?.draft_prompt === ""
    && projection?.composer_submit_mode === "new_request"
    && projection?.can_submit === true;
}

export function terminalReconcileOwner(projection) {
  const row = selectedSessionRow(projection);
  const expected = projection?.run_target?.expectedState;
  if (row === null
    || expected?.kind !== "idle"
    || !canonicalUlid(expected.latestTurnId)
    || !canonicalU64(expected.admissionRevision)
    || row.session_id !== projection?.run_target?.sessionId
    || row.admission_revision !== expected.admissionRevision
    || !Number.isSafeInteger(projection?.turn_page_offset)
    || !Number.isSafeInteger(projection?.turn_page_limit)
    || !Number.isSafeInteger(projection?.turn_page_total)) return null;
  return {
    sessionId: row.session_id,
    turnId: expected.latestTurnId,
    admissionRevision: expected.admissionRevision,
    page: {
      offset: projection.turn_page_offset,
      limit: projection.turn_page_limit,
      total: projection.turn_page_total,
      hasMore: projection.turn_page_has_more,
    },
  };
}

export function terminalReconcileProjectionFailures(projection) {
  const failures = [];
  if (!settledTerminal(projection)) failures.push("terminal-not-settled");
  const owner = terminalReconcileOwner(projection);
  if (owner === null) failures.push("terminal-owner-invalid");
  else if (owner.page.offset !== 0
    || owner.page.limit <= 0
    || owner.page.total <= 0
    || owner.page.hasMore !== false) {
    failures.push("terminal-page-owner-invalid");
  }
  const primary = terminalReconcilePrimaryRows(projection);
  if (!Array.isArray(primary)
    || !isDeepStrictEqual(primary.map((row) => row.kind), ["user", "error", "assistant"])) {
    failures.push("terminal-primary-order-mismatch");
  } else {
    if (primary[0].body !== HISTORY_TERMINAL_RECONCILE_PROMPT
      || typeof primary[0].identity !== "string") {
      failures.push("terminal-user-mismatch");
    }
    if (typeof primary[1].body !== "string" || primary[1].body.trim().length === 0) {
      failures.push("terminal-durable-error-missing");
    }
    if (primary[2].body !== HISTORY_TERMINAL_RECONCILE_RESPONSE) {
      failures.push("terminal-assistant-not-canonical");
    }
  }
  const summaries = Array.isArray(projection?.transcript_rows)
    ? projection.transcript_rows.filter((row) => row?.row_kind === "work_summary_completed")
    : [];
  if (summaries.length !== 1
    || typeof summaries[0]?.stable_history_identity !== "string"
    || summaries[0].stable_history_identity.length === 0) {
    failures.push("terminal-work-summary-invalid");
  }
  return [...new Set(failures)];
}

export function terminalReconcileDomFailures(surface, projection) {
  const failures = [];
  const primary = terminalReconcilePrimaryRows(projection);
  const expectedError = primary?.find((row) => row.kind === "error")?.body ?? null;
  if (!isDeepStrictEqual(surface?.users, [HISTORY_TERMINAL_RECONCILE_PROMPT])) {
    failures.push("terminal-user-dom-mismatch");
  }
  if (!Array.isArray(surface?.errors)
    || surface.errors.length !== 1
    || typeof surface.errors[0] !== "string"
    || surface.errors[0].trim().length === 0
    || typeof expectedError !== "string") {
    failures.push("terminal-error-dom-missing");
  } else if (surface.errors[0] !== expectedError.replaceAll("`", "")) {
    failures.push("terminal-error-dom-mismatch");
  }
  if (!isDeepStrictEqual(surface?.assistants, [HISTORY_TERMINAL_RECONCILE_RESPONSE])) {
    failures.push("terminal-assistant-dom-mismatch");
  }
  if (surface?.primary_visible !== true
    || surface?.prompt?.count !== 1
    || surface.prompt.value !== ""
    || surface.prompt.visible !== true
    || surface.prompt.enabled !== true
    || surface?.send?.count !== 1
    || surface.send.visible !== true
    || surface.send.enabled !== false
    || surface.send.title !== "依頼文を入力してください"
    || surface.send.aria_label !== "依頼文を入力してください"
    || surface?.visible_fatal_count !== 0
    || surface?.visible_recoverable_error_count !== 0) {
    failures.push("terminal-shell-dom-invalid");
  }
  return [...new Set(failures)];
}

export function terminalReconcileRestartFailures(before, after) {
  const failures = [];
  const beforeOwner = terminalReconcileOwner(before);
  const afterOwner = terminalReconcileOwner(after);
  if (beforeOwner === null || afterOwner === null || !isDeepStrictEqual(afterOwner, beforeOwner)) {
    failures.push("restart-owner-drift");
  }
  const beforePrimary = terminalReconcilePrimaryRows(before);
  const afterPrimary = terminalReconcilePrimaryRows(after);
  if (!Array.isArray(beforePrimary)
    || !Array.isArray(afterPrimary)
    || !isDeepStrictEqual(afterPrimary, beforePrimary)) {
    failures.push("restart-primary-history-drift");
  }
  return failures;
}

async function observeTerminalSurface(cdp) {
  return cdp.evaluate(`(async () => {
    const invoke = window.__TAURI_INTERNALS__?.invoke;
    if (typeof invoke !== 'function') throw new Error('tauri-invoke-unavailable');
    const projection = await invoke('desktop_state');
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    const visible = (element) => {
      if (!(element instanceof HTMLElement) || !element.isConnected) return false;
      const style = getComputedStyle(element);
      const rect = element.getBoundingClientRect();
      return style.display !== 'none' && style.visibility !== 'hidden' && Number(style.opacity) !== 0
        && rect.width > 0 && rect.height > 0;
    };
    const enabled = (element) => element instanceof HTMLElement
      && !element.matches(':disabled')
      && element.getAttribute('aria-disabled') !== 'true'
      && element.closest('[inert]') === null;
    const bodies = (selector) => Array.from(document.querySelectorAll(selector))
      .filter(visible)
      .map((row) => row.querySelector('.markdown-body')?.innerText?.trim() ?? '');
    const prompt = document.querySelector('section.composer textarea#prompt');
    const send = document.querySelector('section.composer button[data-action="send"]');
    const primaryArticles = Array.from(document.querySelectorAll(
      '#thread article.message.user, #thread article.message.error, #thread article.message.assistant'
    ));
    return {
      projection,
      users: bodies('#thread article.message.user'),
      errors: bodies('#thread article.message.error'),
      assistants: bodies('#thread article.message.assistant'),
      primary_visible: primaryArticles.length === 3 && primaryArticles.every(visible),
      prompt: {
        count: document.querySelectorAll('section.composer textarea#prompt').length,
        value: prompt instanceof HTMLTextAreaElement ? prompt.value : null,
        visible: visible(prompt),
        enabled: enabled(prompt),
      },
      send: {
        count: document.querySelectorAll('section.composer button[data-action="send"]').length,
        visible: visible(send),
        enabled: enabled(send),
        title: send instanceof HTMLElement ? send.getAttribute('title') : null,
        aria_label: send instanceof HTMLElement ? send.getAttribute('aria-label') : null,
      },
      visible_fatal_count: Array.from(document.querySelectorAll('.fatal')).filter(visible).length,
      visible_recoverable_error_count: Array.from(document.querySelectorAll('.ui-error-notice')).filter(visible).length,
    };
  })()`);
}

async function waitForProductStage({ label, timeoutMs, sample, decide, code, message }) {
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

function terminalDecision(sample) {
  const ledger = sample?.ledger;
  if (Array.isArray(ledger) && (ledger.length > SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_MAX_RESPONSES
    || ledger.some((row) => row?.contract?.pass === false
      || (row?.response_status !== null && row.response_status !== 200)))) return "fail";
  const projection = sample?.surface?.projection;
  if (["failed", "cancelled", "incomplete"].includes(projection?.run_status_key)
    || projection?.startup?.status === "failed") return "fail";
  if (!exactToolErrorRecoveryLedger(ledger) || !settledTerminal(projection)) return "pending";
  if (terminalReconcileProjectionFailures(projection).length > 0) return "fail";
  return terminalReconcileDomFailures(sample.surface, projection).length === 0 ? "pass" : "pending";
}

function createStableRestartDecision(expectedProjection, minimumStableMs = 300) {
  let acceptedSince = null;
  return (sample) => {
    const base = terminalDecision(sample);
    if (base !== "pass"
      || terminalReconcileRestartFailures(expectedProjection, sample?.surface?.projection).length > 0) {
      acceptedSince = null;
      return base === "fail" ? "fail" : "pending";
    }
    const now = Date.now();
    acceptedSince ??= now;
    return now - acceptedSince >= minimumStableMs ? "pass" : "pending";
  };
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

async function trustedInsertPrompt(input) {
  const click = await trustedClick(input, PROMPT);
  const start = click.sequence;
  const inserted = await input.insertText(PROMPT, HISTORY_TERMINAL_RECONCILE_PROMPT);
  const snapshot = await input.snapshotProbe(start);
  return {
    click,
    inserted,
    probe: assertTrustedTextInsertion(snapshot, {
      afterSequence: start,
      identity: PROMPT.identity,
      text: HISTORY_TERMINAL_RECONCILE_PROMPT,
    }),
  };
}

async function waitForExactCommand(commands, afterSequence, expected) {
  const observed = await waitForObservation({
    label: "history terminal reconcile submit command",
    timeoutMs: 10_000,
    pollMs: 16,
    sample: () => commands.snapshot(afterSequence),
    accept: (snapshot) => snapshot.calls.length >= 1,
    retrySampleErrors: false,
  });
  return assertExactDesktopCommandSequence(observed.value, {
    afterSequence,
    expected: [expected],
  });
}

async function waitForBarrierWaiting(barrier) {
  const observed = await waitForObservation({
    label: "history terminal reconcile frontend poll barrier",
    timeoutMs: 10_000,
    pollMs: 16,
    sample: () => barrier.snapshot(),
    accept: (snapshot) => snapshot.phase === "waiting",
    retrySampleErrors: false,
  });
  if (!exactWaitingPollBarrier(observed.value)) {
    throw harnessFailure(
      "history-terminal-reconcile-poll-barrier",
      "the frontend poll barrier did not own exactly one untouched Desktop state request",
      observed.value,
    );
  }
  return observed.value;
}

async function settleProbeResources(state, input, commands, barrier, primaryError) {
  const outcome = { barrier: null, input: null, commands: null, failures: [] };
  if (barrier !== null) {
    try { outcome.barrier = await barrier.remove(); }
    catch (error) { outcome.failures.push({ owner: "desktop-state-poll-barrier", error: errorObservation(error) }); }
  }
  if (input !== null) {
    try { outcome.input = await input.cleanup(); }
    catch (error) { outcome.failures.push({ owner: "webview-input", error: errorObservation(error) }); }
  }
  if (commands !== null) {
    try { outcome.commands = await commands.remove(); }
    catch (error) { outcome.failures.push({ owner: "desktop-command-probe", error: errorObservation(error) }); }
  }
  state.probeOutcome = outcome;
  if (primaryError === null && outcome.failures.length > 0) {
    throw harnessFailure(
      "history-terminal-reconcile-probe-cleanup",
      "history terminal reconciliation probes did not settle",
      outcome,
    );
  }
}

export function createHistoryTerminalReconcileScenario() {
  const state = {
    provider: null,
    acceptedLedger: null,
    quiesceOutcome: null,
    probeOutcome: null,
  };
  return Object.freeze({
    id: "history.terminal-reconcile",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    requestGracefulExit,
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({
        expectedPrompt: HISTORY_TERMINAL_RECONCILE_PROMPT,
        responseBehavior: "hold_until_release",
        script: createToolErrorRecoveryProviderScript({
          missingPath: HISTORY_TERMINAL_RECONCILE_MISSING_PATH,
          responseText: HISTORY_TERMINAL_RECONCILE_RESPONSE,
          streamedPrefix: HISTORY_TERMINAL_RECONCILE_STREAMED_PREFIX,
        }),
      });
      await prepareDesktopFixture({
        context,
        sink,
        phase,
        owner: OWNER,
        configText: providerRestartFixtureConfig(state.provider.baseUrl, { supportsTools: true }),
        sentinelName: "E2E_HISTORY_TERMINAL_RECONCILE.txt",
        sentinelText: "moyAI Desktop E2E terminal canonical Error reconciliation fixture.\n",
      });
      await sink.record("history-terminal-reconcile-provider-started", state.provider.resourceObservation(), {
        phase,
        owner: OWNER,
      });
    },
    async execute({ context, driver: firstCdp, host, sink }) {
      const provider = state.provider;
      if (provider === null) throw new Error("history terminal reconciliation provider was not prepared");
      await acquireInteractiveShell({ context, driver: firstCdp, sink }, {
        evidenceOwner: OWNER,
        screenshotStem: "history-terminal-reconcile-shell-ready",
      });
      if (provider.requestLedger.length !== 0) {
        throw productFailure(
          "history-terminal-reconcile-cold-start-wire",
          "Desktop contacted the tool-error provider before trusted Send",
          { ledger: provider.requestLedger },
        );
      }

      const input = new WebviewInput(firstCdp, { probeId: "history-terminal-reconcile" });
      const commands = new DesktopCommandProbe(firstCdp, {
        probeId: "history-terminal-reconcile",
        commands: ["submit_prompt", "cancel_run"],
      });
      const barrier = new DesktopStatePollBarrier(firstCdp, {
        barrierId: POLL_BARRIER_ID,
      });
      let primaryError = null;
      let probesSettled = false;
      try {
        await input.installProbe();
        await commands.install();
        await barrier.install();
        const typed = await trustedInsertPrompt(input);
        const ready = await waitForObservation({
          label: "history terminal reconcile typed composer",
          timeoutMs: 10_000,
          pollMs: 50,
          sample: () => observeTerminalSurface(firstCdp),
          accept: (surface) => surface?.prompt?.value === HISTORY_TERMINAL_RECONCILE_PROMPT
            && surface.prompt.visible === true
            && surface.prompt.enabled === true
            && surface?.send?.visible === true
            && surface.send.enabled === true
            && surface?.projection?.can_submit === true,
          retrySampleErrors: false,
        });
        const expectedCommand = {
          command: "submit_prompt",
          args: {
            text: HISTORY_TERMINAL_RECONCILE_PROMPT,
            expectedTarget: ready.value.projection.draft_target,
            expectedRunTarget: ready.value.projection.run_target,
          },
        };
        const barrierInstalled = await barrier.arm();
        const commandStart = (await commands.snapshot()).sequence;
        const sent = await trustedClick(input, SEND);
        const command = await waitForExactCommand(commands, commandStart, expectedCommand);
        const initialHeld = await waitForProductStage({
          label: "history terminal reconcile held initial provider response",
          timeoutMs: 10_000,
          sample: async () => ({
            ledger: provider.requestLedger,
            resource: provider.resourceObservation(),
          }),
          decide: ({ ledger, resource }) => heldInitialProviderDecision(ledger, resource),
          code: "history-terminal-reconcile-provider-initial",
          message: "the initial failed-tool provider request did not reach its exact release barrier",
        });
        const barrierWaiting = await waitForBarrierWaiting(barrier);
        const providerRelease = provider.releaseResponse(0);
        await waitForProductStage({
          label: "history terminal reconcile provider completion without frontend polls",
          timeoutMs: 30_000,
          sample: async () => ({ ledger: provider.requestLedger }),
          decide: ({ ledger }) => {
            if (exactToolErrorRecoveryLedger(ledger)) return "pass";
            return Array.isArray(ledger)
              && (ledger.length > SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_MAX_RESPONSES
                || ledger.some((row) => row?.contract?.pass === false
                  || (row?.response_status !== null && row.response_status !== 200)))
              ? "fail"
              : "pending";
          },
          code: "history-terminal-reconcile-provider-flow",
          message: "the bounded failed-tool provider flow did not complete exactly once",
        });
        const barrierHeldAfterProvider = await barrier.snapshot();
        if (!exactWaitingPollBarrier(barrierHeldAfterProvider)) {
          throw harnessFailure(
            "history-terminal-reconcile-poll-barrier-drift",
            "the frontend poll barrier changed before the failed-tool terminal completed",
            barrierHeldAfterProvider,
          );
        }
        const resumed = await barrier.resumeFresh();
        if (resumed.delivered !== true) {
          throw harnessFailure(
            "history-terminal-reconcile-poll-resume",
            "the held frontend Desktop state request did not receive a fresh backend response",
            resumed,
          );
        }
        const terminal = await waitForProductStage({
          label: "history terminal reconcile canonical GUI terminal",
          timeoutMs: 30_000,
          sample: async () => ({
            surface: await observeTerminalSurface(firstCdp),
            ledger: provider.requestLedger,
          }),
          decide: terminalDecision,
          code: "history-terminal-reconcile-live-drift",
          message: "the live terminal GUI did not reconcile its canonical durable Error and full Assistant",
        });
        const terminalScreenshot = await captureScenarioScreenshot({
          cdp: firstCdp,
          sink,
          name: "history-terminal-reconcile-live-terminal",
          owner: OWNER,
        });
        await sink.record("history-terminal-reconcile-live-terminal", {
          typed,
          sent,
          command,
          barrier: {
            installed: barrierInstalled,
            waiting: barrierWaiting,
            held_after_provider: barrierHeldAfterProvider,
            resumed,
          },
          provider_initial_held: initialHeld,
          provider_release: providerRelease,
          owner: terminalReconcileOwner(terminal.surface.projection),
          primary_rows: terminalReconcilePrimaryRows(terminal.surface.projection),
          provider_ledger: terminal.ledger,
          screenshot: terminalScreenshot,
        }, { phase: "executing", owner: OWNER });

        probesSettled = true;
        await settleProbeResources(state, input, commands, barrier, null);
        const restarted = await host.restart({
          context,
          scenario: this,
          sink,
          driver: firstCdp,
          phase: "executing",
        });
        await acquireInteractiveShell({ context, driver: restarted.driver, sink }, {
          evidenceOwner: OWNER,
          screenshotStem: "history-terminal-reconcile-restarted-shell",
        });
        const restartDecision = createStableRestartDecision(terminal.surface.projection);
        const restored = await waitForProductStage({
          label: "history terminal reconcile stable restart parity",
          timeoutMs: 30_000,
          sample: async () => ({
            surface: await observeTerminalSurface(restarted.driver),
            ledger: provider.requestLedger,
          }),
          decide: restartDecision,
          code: "history-terminal-reconcile-restart-drift",
          message: "Desktop restart changed the canonical failed-tool conversation",
        });
        const restartScreenshot = await captureScenarioScreenshot({
          cdp: restarted.driver,
          sink,
          name: "history-terminal-reconcile-restarted-terminal",
          owner: OWNER,
        });
        state.acceptedLedger = structuredClone(provider.requestLedger);
        await sink.record("history-terminal-reconcile-completed", {
          restart: restarted.restart,
          owner: terminalReconcileOwner(restored.surface.projection),
          primary_rows: terminalReconcilePrimaryRows(restored.surface.projection),
          continuity_failures: terminalReconcileRestartFailures(
            terminal.surface.projection,
            restored.surface.projection,
          ),
          accepted_provider_ledger: state.acceptedLedger,
          screenshot: restartScreenshot,
        }, { phase: "executing", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        if (!probesSettled) {
          probesSettled = true;
          await settleProbeResources(state, input, commands, barrier, primaryError);
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
      const pass = state.quiesceOutcome?.input === "pass"
        && state.probeOutcome?.failures?.length === 0;
      return {
        input: pass ? "pass" : "fail",
        resources: [{
          kind: "history-terminal-reconcile-verification",
          quiesce_input: state.quiesceOutcome?.input ?? null,
          probe_outcome: state.probeOutcome,
        }],
      };
    },
  });
}

import { isDeepStrictEqual } from "node:util";

import { waitForObservation } from "../core/deadline.mjs";
import { canonicalU64, canonicalUlid } from "../core/canonical_identity.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import {
  DesktopCommandProbe,
  assertExactDesktopCommandSequence,
} from "../drivers/desktop_command_probe.mjs";
import { DesktopStatePollBarrier } from "../drivers/desktop_state_poll_barrier.mjs";
import { WebviewInput, assertTrustedProbeSequence } from "../drivers/webview_input.mjs";
import { startScriptedProvider } from "../drivers/scripted_provider.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import {
  classifyAcquiredObservationFailure,
  providerRestartFixtureConfig,
  quiesceProviderResource,
  relevantProviderHistory,
} from "./provider_restart.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import { captureScenarioScreenshot, selectedNavigationIdentity } from "./observations.mjs";

const OWNER = "scenario:run.next-turn";
export const RUN_NEXT_TURN_FIRST_PROMPT = "complete first turn";
export const RUN_NEXT_TURN_FIRST_RESPONSE = "FIRST_TURN_OK";
export const RUN_NEXT_TURN_SECOND_PROMPT = "complete second turn";
export const RUN_NEXT_TURN_SECOND_RESPONSE = "SECOND_TURN_OK";

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

function errorObservation(error) {
  return {
    name: error?.name ?? "Error",
    code: error?.code ?? null,
    message: error?.message ?? String(error),
    evidence: error?.evidence ?? null,
  };
}

function exactNextRevision(previous, next) {
  return canonicalU64(previous)
    && canonicalU64(next)
    && BigInt(next) === BigInt(previous) + 1n;
}

function acceptedResponse(row, phase) {
  return row?.method === "POST"
    && row?.pathname === "/v1/responses"
    && row?.query_present === false
    && row?.contract?.pass === true
    && row?.response_phase === phase
    && row?.response_status === (phase === "completed" ? 200 : null);
}

export function exactRunNextTurnLedger(ledger, phases) {
  return Array.isArray(ledger)
    && Array.isArray(phases)
    && ledger.length === phases.length
    && ledger.every((row, index) => acceptedResponse(row, phases[index]));
}

function exactHistory(projection, turnCount) {
  const history = relevantProviderHistory(projection);
  const prompts = [RUN_NEXT_TURN_FIRST_PROMPT, RUN_NEXT_TURN_SECOND_PROMPT].slice(0, turnCount);
  const responses = [RUN_NEXT_TURN_FIRST_RESPONSE, RUN_NEXT_TURN_SECOND_RESPONSE].slice(0, turnCount);
  return history.rows.length === turnCount * 3
    && isDeepStrictEqual(history.rows.map((row) => row.kind), Array.from(
      { length: turnCount },
      () => ["user", "work_summary_completed", "assistant"],
    ).flat())
    && isDeepStrictEqual(history.users, prompts)
    && isDeepStrictEqual(history.assistants, responses)
    && history.completed_summaries === turnCount;
}

function exactIdleOwner(projection) {
  const expected = projection?.run_target?.expectedState;
  return expected?.kind === "idle"
    && canonicalUlid(expected.latestTurnId)
    && canonicalU64(expected.admissionRevision)
    && projection?.run_target?.sessionId !== null
    && projection.run_target.sessionId === projection?.draft_target?.sessionId;
}

function capturedPendingIdleOwner(projection) {
  const expected = projection?.run_target?.expectedState;
  const latestTurnMatchesFence = expected?.latestTurnId === null
    ? expected?.admissionRevision === "0"
    : canonicalUlid(expected?.latestTurnId);
  return expected?.kind === "idle"
    && latestTurnMatchesFence
    && canonicalU64(expected?.admissionRevision)
    && projection?.run_target?.sessionId !== null
    && projection.run_target.sessionId === projection?.draft_target?.sessionId;
}

function settledComposer(surface) {
  const projection = surface?.projection;
  return projection?.run_status_key === "completed"
    && projection?.task_activity_state === "idle"
    && projection?.busy === false
    && projection?.post_run_refresh_pending === false
    && projection?.background_mutation_pending === false
    && projection?.async_polling_required === false
    && Array.isArray(projection?.pending_async_operations)
    && projection.pending_async_operations.length === 0
    && projection?.composer_submit_mode === "new_request"
    && projection?.can_submit === true
    && exactIdleOwner(projection)
    && surface?.composer?.count === 1
    && surface.composer.visible === true
    && isDeepStrictEqual(surface.composer.run_target, projection.run_target)
    && surface?.prompt?.count === 1
    && surface.prompt.visible === true
    && surface.prompt.enabled === true
    && surface?.send?.count === 1
    && surface.send.visible === true
    && surface?.visible_fatal_count === 0
    && surface?.visible_recoverable_error_count === 0;
}

export function pendingOwnerRaceFailures({ pendingProjection, settledProjection, dom, commandSnapshot }) {
  const failures = [];
  if (pendingProjection?.post_run_refresh_pending !== true
    || pendingProjection?.run_status_key !== "completed"
    || pendingProjection?.composer_submit_mode !== "blocked"
    || pendingProjection?.can_submit !== false
    || !capturedPendingIdleOwner(pendingProjection)) {
    failures.push("pending-projection-did-not-close-new-request");
  }
  if (settledProjection?.post_run_refresh_pending !== false
    || settledProjection?.run_status_key !== "completed"
    || settledProjection?.composer_submit_mode !== "new_request"
    || settledProjection?.can_submit !== true
    || !exactIdleOwner(settledProjection)) {
    failures.push("settled-backend-owner-invalid");
  }
  if (!canonicalU64(pendingProjection?.projection_revision)
    || !canonicalU64(settledProjection?.projection_revision)
    || BigInt(settledProjection.projection_revision) <= BigInt(pendingProjection.projection_revision)) {
    failures.push("pending-projection-not-older-than-settled-backend");
  }
  if (pendingProjection?.run_target?.sessionId !== settledProjection?.run_target?.sessionId
    || isDeepStrictEqual(pendingProjection?.run_target, settledProjection?.run_target)) {
    failures.push("pending-and-settled-run-owners-did-not-drift-in-one-session");
  }
  if (dom?.composer?.count !== 1
    || dom.composer.visible !== true
    || !isDeepStrictEqual(dom.composer.run_target, pendingProjection?.run_target)
    || dom?.prompt?.count !== 1
    || dom.prompt.visible !== true
    || dom.prompt.enabled !== true
    || dom.prompt.value !== RUN_NEXT_TURN_SECOND_PROMPT
    || dom?.send?.count !== 1
    || dom.send.visible !== true
    || dom.send.enabled !== false
    || dom.send.action !== "send"
    || dom.visible_fatal_count !== 0
    || dom.visible_recoverable_error_count !== 0) {
    failures.push("pending-projection-was-not-rendered-as-a-blocked-composer");
  }
  if (commandSnapshot?.found !== true
    || commandSnapshot.dropped_through !== 0
    || commandSnapshot.sequence !== 0
    || !Array.isArray(commandSnapshot.calls)
    || commandSnapshot.calls.length !== 0) {
    failures.push("pending-composer-emitted-a-run-command");
  }
  return failures;
}

export function firstTurnAcquisitionFailures(sample) {
  const failures = [];
  const projection = sample?.surface?.projection;
  const expected = projection?.run_target?.expectedState;
  if (!exactRunNextTurnLedger(sample?.ledger, ["held"])) {
    failures.push("provider-first-request-not-held");
  }
  if (projection?.run_status_key !== "running" || projection?.busy !== true) {
    failures.push("first-turn-not-running");
  }
  if (expected?.kind !== "turn"
    || !canonicalUlid(expected?.turnId)
    || !canonicalU64(expected?.admissionRevision)
    || !canonicalUlid(projection?.run_target?.sessionId)
    || projection.run_target.sessionId !== projection?.draft_target?.sessionId) {
    failures.push("first-turn-owner-invalid");
  }
  if (sample?.surface?.visible_fatal_count > 0 || sample?.surface?.visible_recoverable_error_count > 0) {
    failures.push("first-turn-error-visible");
  }
  return failures;
}

export function nextTurnAcquisitionFailures(sample, firstTerminal) {
  const failures = [];
  const projection = sample?.surface?.projection;
  const expected = projection?.run_target?.expectedState;
  const firstExpected = firstTerminal?.run_target?.expectedState;
  if (!exactRunNextTurnLedger(sample?.ledger, ["completed", "held"])) {
    failures.push("provider-second-request-not-held");
  }
  if (projection?.run_status_key !== "running" || projection?.busy !== true) {
    failures.push("second-turn-not-running");
  }
  if (expected?.kind !== "turn"
    || !canonicalUlid(expected.turnId)
    || expected.turnId === firstExpected?.latestTurnId
    || !exactNextRevision(firstExpected?.admissionRevision, expected.admissionRevision)) {
    failures.push("second-turn-owner-not-next-revision");
  }
  if (projection?.run_target?.sessionId !== firstTerminal?.run_target?.sessionId
    || projection?.draft_target?.sessionId !== firstTerminal?.draft_target?.sessionId) {
    failures.push("second-turn-session-drift");
  }
  if (sample?.surface?.visible_fatal_count > 0 || sample?.surface?.visible_recoverable_error_count > 0) {
    failures.push("second-turn-error-visible");
  }
  return failures;
}

export function nextTurnTerminalFailures(sample, firstTerminal, secondTurn) {
  const failures = [];
  const projection = sample?.surface?.projection;
  const expected = projection?.run_target?.expectedState;
  const secondExpected = secondTurn?.run_target?.expectedState;
  if (!exactRunNextTurnLedger(sample?.ledger, ["completed", "completed"])) {
    failures.push("provider-two-turn-ledger-not-complete");
  }
  if (!settledComposer(sample?.surface) || !exactHistory(projection, 2)) {
    failures.push("second-terminal-not-settled");
  }
  if (projection?.draft_prompt !== "" || sample?.surface?.prompt?.value !== "") {
    failures.push("second-terminal-draft-not-cleared");
  }
  if (expected?.latestTurnId !== secondExpected?.turnId
    || expected?.admissionRevision !== secondExpected?.admissionRevision
    || projection?.run_target?.sessionId !== firstTerminal?.run_target?.sessionId) {
    failures.push("second-terminal-owner-drift");
  }
  return failures;
}

async function observeRunNextTurnSurface(cdp) {
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
    const composer = document.querySelector('section.composer');
    let composerRunTarget = null;
    try { composerRunTarget = JSON.parse(composer?.getAttribute('data-run-target') ?? 'null'); }
    catch { composerRunTarget = { parse_error: true }; }
    const prompt = composer?.querySelector('textarea#prompt') ?? null;
    const send = composer?.querySelector('button[data-action="send"]') ?? null;
    return {
      projection,
      selected_navigation: Array.from(document.querySelectorAll(
        'button.nav-row[aria-current="page"][data-action="session"], button.nav-row[aria-current="page"][data-action="chat-session"]'
      )).map((row) => ({
        action: row instanceof HTMLElement ? (row.dataset.action ?? null) : null,
        focus_key: row instanceof HTMLElement ? (row.dataset.focusKey ?? null) : null,
        visible: visible(row),
      })),
      composer: {
        count: document.querySelectorAll('section.composer').length,
        visible: visible(composer),
        run_target: composerRunTarget,
      },
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
      },
      visible_fatal_count: Array.from(document.querySelectorAll('.fatal')).filter(visible).length,
      visible_recoverable_error_count: Array.from(document.querySelectorAll('.ui-error-notice')).filter(visible).length,
    };
  })()`);
}

async function observeRunNextTurnDom(cdp) {
  return cdp.evaluate(`(() => {
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
    const composer = document.querySelector('section.composer');
    let composerRunTarget = null;
    try { composerRunTarget = JSON.parse(composer?.getAttribute('data-run-target') ?? 'null'); }
    catch { composerRunTarget = { parse_error: true }; }
    const prompt = composer?.querySelector('textarea#prompt') ?? null;
    const send = composer?.querySelector('button[data-action="send"]') ?? null;
    return {
      composer: {
        count: document.querySelectorAll('section.composer').length,
        visible: visible(composer),
        run_target: composerRunTarget,
      },
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
        action: send instanceof HTMLElement ? (send.dataset.action ?? null) : null,
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
      pollMs: 75,
      retrySampleErrors: false,
      sample,
      accept: (value) => {
        decision = decide(value);
        return decision !== "pending";
      },
    });
  } catch (error) {
    throw classifyAcquiredObservationFailure(error, { code, message });
  }
  if (decision === "fail") throw productFailure(code, message, { observation: observed });
  return observed;
}

function keyCode(character) {
  if (/^[a-z]$/.test(character)) return `Key${character.toUpperCase()}`;
  if (character === " ") return "Space";
  throw new TypeError(`unsupported run.next-turn character: ${character}`);
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
  const probe = assertTrustedProbeSequence(snapshot, {
    afterSequence: start,
    expected: [
      { type: "pointerdown", identity: locator.identity, button: 0, buttons: 1 },
      { type: "pointerup", identity: locator.identity, button: 0, buttons: 0 },
      { type: "click", identity: locator.identity, button: 0, buttons: 0 },
    ],
  });
  return { target, probe, sequence: snapshot.sequence };
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

function firstHeldDecision(sample) {
  if (sample?.ledger?.length > 1) return "fail";
  if (sample?.surface?.visible_fatal_count > 0 || sample?.surface?.visible_recoverable_error_count > 0) return "fail";
  return firstTurnAcquisitionFailures(sample).length === 0 ? "pass" : "pending";
}

async function settleResources(state, input, commands, pollBarrier, primaryError) {
  const outcome = { input: null, command_probe: null, poll_barrier: null, failures: [] };
  try { outcome.poll_barrier = await pollBarrier.remove(); }
  catch (error) { outcome.failures.push({ owner: "desktop-state-poll-barrier", error: errorObservation(error) }); }
  try { outcome.input = await input.cleanup(); }
  catch (error) { outcome.failures.push({ owner: "webview-input", error: errorObservation(error) }); }
  try { outcome.command_probe = await commands.remove(); }
  catch (error) { outcome.failures.push({ owner: "desktop-command-probe", error: errorObservation(error) }); }
  state.resourceOutcome = outcome;
  if (outcome.failures.length > 0 && primaryError === null) {
    throw new DesktopE2eError(
      "harness",
      "run-next-turn-resource-cleanup-failed",
      "run.next-turn input and command probes did not settle",
      outcome,
    );
  }
}

export function createRunNextTurnScenario() {
  const state = {
    provider: null,
    acceptedLedger: null,
    quiesceOutcome: null,
    resourceOutcome: null,
  };
  return Object.freeze({
    id: "run.next-turn",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    requestGracefulExit,
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({
        turns: [
          { prompt: RUN_NEXT_TURN_FIRST_PROMPT, responseText: RUN_NEXT_TURN_FIRST_RESPONSE },
          { prompt: RUN_NEXT_TURN_SECOND_PROMPT, responseText: RUN_NEXT_TURN_SECOND_RESPONSE },
        ],
        orderedConversation: true,
        responseBehavior: "hold_until_release",
      });
      await prepareDesktopFixture({
        context,
        sink,
        phase,
        owner: OWNER,
        configText: providerRestartFixtureConfig(state.provider.baseUrl),
        sentinelName: "E2E_RUN_NEXT_TURN.txt",
        sentinelText: "moyAI Desktop E2E next-turn owner settlement fixture.\n",
      });
      await sink.record("scripted-provider-started", state.provider.resourceObservation(), { phase, owner: OWNER });
    },
    async execute({ context, driver: cdp, sink }) {
      const provider = state.provider;
      if (provider === null) throw new Error("scripted provider was not prepared");
      await acquireInteractiveShell({ context, driver: cdp, sink }, {
        evidenceOwner: OWNER,
        screenshotStem: "run-next-turn-shell-ready",
      });
      if (provider.requestLedger.length !== 0) {
        throw productFailure("run-next-turn-cold-start-request", "Desktop contacted the provider before trusted Send", {
          ledger: provider.requestLedger,
        });
      }

      const input = new WebviewInput(cdp, { probeId: "run-next-turn" });
      const commands = new DesktopCommandProbe(cdp, {
        probeId: "run-next-turn-commands",
        commands: ["submit_prompt", "cancel_run"],
      });
      const pollBarrier = new DesktopStatePollBarrier(cdp, {
        barrierId: "run-next-turn-terminal",
      });
      let primaryError = null;
      try {
        await input.installProbe();
        const firstTyping = await trustedType(input, RUN_NEXT_TURN_FIRST_PROMPT);
        const firstSend = await trustedClick(input, SEND);
        const firstHeld = await waitForProductStage({
          label: "run.next-turn first Turn acquisition",
          timeoutMs: 30_000,
          sample: async () => ({ surface: await observeRunNextTurnSurface(cdp), ledger: provider.requestLedger }),
          decide: firstHeldDecision,
          code: "run-next-turn-first-acquisition-mismatch",
          message: "the first trusted Send did not acquire one held Turn",
        });
        await commands.install();
        await pollBarrier.install();
        await pollBarrier.arm();
        const waitingPoll = await waitForObservation({
          label: "run.next-turn frontend poll barrier acquisition",
          timeoutMs: 5_000,
          pollMs: 25,
          retrySampleErrors: false,
          sample: () => pollBarrier.snapshot(),
          accept: (snapshot) => snapshot.phase === "waiting" && snapshot.intercepted_count === 1,
        });
        const firstRelease = provider.releaseResponse(0);
        const pendingCapture = await pollBarrier.capturePending({ timeoutMs: 15_000, pollMs: 5 });
        const pendingProjection = structuredClone(pendingCapture.projection);
        const backendSettlement = await waitForObservation({
          label: "run.next-turn backend durable owner settlement behind held frontend poll",
          timeoutMs: 30_000,
          pollMs: 10,
          retrySampleErrors: false,
          sample: async () => ({ projection: await pollBarrier.sampleBackend(), ledger: provider.requestLedger }),
          accept: (sample) => exactRunNextTurnLedger(sample?.ledger, ["completed"])
            && sample?.projection?.post_run_refresh_pending === false
            && sample.projection.run_status_key === "completed"
            && sample.projection.composer_submit_mode === "new_request"
            && sample.projection.can_submit === true
            && sample.projection.draft_prompt === ""
            && exactIdleOwner(sample.projection)
            && exactHistory(sample.projection, 1)
            && sample.projection.run_target.sessionId === pendingProjection.run_target?.sessionId
            && !isDeepStrictEqual(sample.projection.run_target, pendingProjection.run_target),
        });
        const firstProjection = structuredClone(backendSettlement.value.projection);
        const pendingRelease = await pollBarrier.releaseCaptured({ rearm: true });
        const pendingDom = await waitForProductStage({
          label: "run.next-turn stale pending projection blocked in the actual composer",
          timeoutMs: 5_000,
          sample: () => observeRunNextTurnDom(cdp),
          decide: (dom) => {
            if (!isDeepStrictEqual(dom?.composer?.run_target, pendingProjection.run_target)) return "pending";
            return dom?.composer?.count === 1
              && dom.composer.visible === true
              && dom?.prompt?.count === 1
              && dom.prompt.visible === true
              && dom.prompt.enabled === true
              && dom.prompt.value === ""
              && dom?.send?.count === 1
              && dom.send.visible === true
              && dom.send.enabled === false
              && dom.send.action === "send"
              && dom.visible_fatal_count === 0
              && dom.visible_recoverable_error_count === 0
              ? "pass"
              : "fail";
          },
          code: "run-next-turn-pending-composer-open",
          message: "the stale terminal-refresh projection left the actual GUI Send action open",
        });
        const secondTyping = await trustedType(input, RUN_NEXT_TURN_SECOND_PROMPT);
        const typedPendingDom = await waitForObservation({
          label: "run.next-turn blocked pending composer preserves the next draft",
          timeoutMs: 5_000,
          pollMs: 25,
          retrySampleErrors: false,
          sample: () => observeRunNextTurnDom(cdp),
          accept: (dom) => dom?.prompt?.value === RUN_NEXT_TURN_SECOND_PROMPT,
        });
        const blockedCommandSnapshot = await commands.snapshot();
        const pendingFailures = pendingOwnerRaceFailures({
          pendingProjection,
          settledProjection: firstProjection,
          dom: typedPendingDom.value,
          commandSnapshot: blockedCommandSnapshot,
        });
        if (pendingFailures.length > 0) {
          throw productFailure(
            "run-next-turn-pending-owner-race",
            "the controlled stale terminal-refresh projection did not remain blocked",
            {
              failures: pendingFailures,
              pending_projection: pendingProjection,
              settled_projection: firstProjection,
              dom: typedPendingDom.value,
              commands: blockedCommandSnapshot,
            },
          );
        }
        const pendingScreenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "run-next-turn-pending-blocked",
          owner: OWNER,
        });
        const freshResume = await pollBarrier.resumeFresh();
        const secondReady = await waitForObservation({
          label: "run.next-turn second trusted prompt readiness",
          timeoutMs: 10_000,
          pollMs: 50,
          retrySampleErrors: false,
          sample: () => observeRunNextTurnSurface(cdp),
          accept: (surface) => settledComposer(surface)
            && exactHistory(surface.projection, 1)
            && surface.prompt.value === RUN_NEXT_TURN_SECOND_PROMPT
            && surface.send.enabled === true
            && surface.projection.draft_prompt === ""
            && surface.projection.run_target.sessionId === firstProjection.run_target.sessionId,
        });
        const firstScreenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "run-next-turn-first-completed",
          owner: OWNER,
        });
        const expectedCommand = {
          command: "submit_prompt",
          args: {
            text: RUN_NEXT_TURN_SECOND_PROMPT,
            expectedTarget: structuredClone(secondReady.value.projection.draft_target),
            expectedRunTarget: structuredClone(secondReady.value.projection.run_target),
          },
        };
        const commandStart = (await commands.snapshot()).sequence;
        const secondSend = await trustedClick(input, SEND);
        const commandObservation = await waitForObservation({
          label: "run.next-turn exact second submit command",
          timeoutMs: 10_000,
          pollMs: 25,
          retrySampleErrors: false,
          sample: () => commands.snapshot(commandStart),
          accept: (snapshot) => snapshot.calls.length >= 1,
        });
        const exactCommand = assertExactDesktopCommandSequence(commandObservation.value, {
          afterSequence: commandStart,
          expected: [expectedCommand],
        });

        const secondTurn = await waitForProductStage({
          label: "run.next-turn revision+1 Turn acquisition",
          timeoutMs: 30_000,
          sample: async () => ({ surface: await observeRunNextTurnSurface(cdp), ledger: provider.requestLedger }),
          decide: (sample) => {
            if (sample?.ledger?.length > 2 || sample?.surface?.visible_fatal_count > 0
              || sample?.surface?.visible_recoverable_error_count > 0) return "fail";
            return nextTurnAcquisitionFailures(sample, firstProjection).length === 0 ? "pass" : "pending";
          },
          code: "run-next-turn-second-acquisition-mismatch",
          message: "the second trusted Send did not acquire one same-session revision+1 Turn",
        });
        const secondProjection = structuredClone(secondTurn.value.surface.projection);
        const secondRunningScreenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "run-next-turn-second-running",
          owner: OWNER,
        });
        const secondRelease = provider.releaseResponse(1);
        const terminal = await waitForProductStage({
          label: "run.next-turn second normal terminal",
          timeoutMs: 45_000,
          sample: async () => ({ surface: await observeRunNextTurnSurface(cdp), ledger: provider.requestLedger }),
          decide: (sample) => {
            if (sample?.ledger?.length > 2 || sample?.surface?.visible_fatal_count > 0
              || sample?.surface?.visible_recoverable_error_count > 0) return "fail";
            return nextTurnTerminalFailures(sample, firstProjection, secondProjection).length === 0
              ? "pass"
              : "pending";
          },
          code: "run-next-turn-second-terminal-mismatch",
          message: "the second Turn did not complete with the exact two-turn history and settled owner",
        });
        const finalCommand = assertExactDesktopCommandSequence(await commands.snapshot(commandStart), {
          afterSequence: commandStart,
          expected: [expectedCommand],
        });
        const terminalScreenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "run-next-turn-second-completed",
          owner: OWNER,
        });
        state.acceptedLedger = structuredClone(terminal.value.ledger);
        await sink.record("run-next-turn-completed", {
          input_kind: "browser_trusted",
          first: {
            typing: firstTyping,
            send: firstSend,
            running_owner: firstHeld.value.surface.projection.run_target,
            release: firstRelease,
            terminal_owner: firstProjection.run_target,
            selected_navigation: selectedNavigationIdentity(firstProjection),
            screenshot: firstScreenshot,
            pending_race: {
              waiting_poll: waitingPoll.value,
              capture: {
                projection_revision: pendingProjection.projection_revision,
                post_run_refresh_pending: pendingProjection.post_run_refresh_pending,
                run_status_key: pendingProjection.run_status_key,
                composer_submit_mode: pendingProjection.composer_submit_mode,
                can_submit: pendingProjection.can_submit,
                run_target: pendingProjection.run_target,
                bypass_count: pendingCapture.bypass_count,
              },
              backend_settlement_elapsed_ms: backendSettlement.elapsed_ms,
              backend_settlement: {
                projection_revision: firstProjection.projection_revision,
                post_run_refresh_pending: firstProjection.post_run_refresh_pending,
                run_status_key: firstProjection.run_status_key,
                composer_submit_mode: firstProjection.composer_submit_mode,
                can_submit: firstProjection.can_submit,
                run_target: firstProjection.run_target,
              },
              release: pendingRelease,
              initial_dom: pendingDom.value,
              typed_dom: typedPendingDom.value,
              blocked_commands: blockedCommandSnapshot,
              screenshot: pendingScreenshot,
              fresh_resume: freshResume,
            },
          },
          second: {
            typing: secondTyping,
            send: secondSend,
            expected_command: expectedCommand,
            command: exactCommand,
            final_command_snapshot: finalCommand,
            running_owner: secondProjection.run_target,
            release: secondRelease,
            terminal_owner: terminal.value.surface.projection.run_target,
            running_screenshot: secondRunningScreenshot,
            terminal_screenshot: terminalScreenshot,
          },
          provider_ledger: state.acceptedLedger,
          provider_resource: provider.resourceObservation(),
        }, { phase: "executing", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        await settleResources(state, input, commands, pollBarrier, primaryError);
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
      const quiesced = state.quiesceOutcome !== null;
      const pass = quiesced
        && state.quiesceOutcome.input === "pass"
        && state.resourceOutcome?.failures?.length === 0;
      return {
        input: pass ? "pass" : "fail",
        resources: [{
          kind: "run-next-turn-verification",
          quiesced,
          quiesce_input: state.quiesceOutcome?.input ?? null,
          interaction_resources: state.resourceOutcome,
        }],
      };
    },
  });
}

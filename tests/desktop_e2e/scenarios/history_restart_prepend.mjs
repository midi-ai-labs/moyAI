import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import {
  classifyRestartTurnPage,
  restartPreviousPageTransitionFailures,
  restartTurnPageMetadata,
  selectedRestartSessionRow,
} from "../core/history_restart_contract.mjs";
import { waitForSemanticTargetSettlement } from "../core/semantic_target_settlement.mjs";
import {
  DesktopCommandProbe,
  assertExactDesktopCommandSequence,
} from "../drivers/desktop_command_probe.mjs";
import {
  SCRIPTED_PROVIDER_MAX_TURNS,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";
import {
  WebviewInput,
  assertTrustedProbeSequence,
  assertTrustedTextInsertion,
} from "../drivers/webview_input.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import {
  providerRestartFixtureConfig,
  quiesceProviderResource,
} from "./provider_restart.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";

const OWNER = "scenario:history.restart-prepend";
const FILTER_TEXT = "load-previous-turn-page";
const FIXTURE_TURN_TIMEOUT_MS = 30_000;

const SHOW_COMMAND_PALETTE = Object.freeze({
  selector: 'section.composer button[data-action="show-command-palette"]',
  identity: { tag: "BUTTON", action: "show-command-palette" },
});
const COMMAND_PALETTE_SEARCH = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="command-palette-dialog-title"] input#local-search',
  identity: { tag: "INPUT", id: "local-search" },
});
const PREVIOUS_TURN_PAGE = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="command-palette-dialog-title"] button[data-action="load-previous-turn-page"]',
  identity: { tag: "BUTTON", action: "load-previous-turn-page" },
});

export function historyRestartFixtureTurns() {
  return Array.from({ length: SCRIPTED_PROVIDER_MAX_TURNS }, (_, index) => ({
    prompt: `history fixture prompt ${String(index + 1).padStart(2, "0")}`,
    responseText: `HISTORY_FIXTURE_RESPONSE_${String(index + 1).padStart(2, "0")}`,
  }));
}

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

function latestBody(projection, kind) {
  const rows = Array.isArray(projection?.transcript_rows) ? projection.transcript_rows : [];
  return rows.filter((row) => row?.row_kind === kind).at(-1)?.body ?? null;
}

export function historyConversationRows(projection) {
  if (!Array.isArray(projection?.transcript_rows)) return null;
  const rows = [];
  for (const row of projection.transcript_rows) {
    if (!["user", "assistant", "error"].includes(row?.row_kind)) continue;
    if (typeof row.body !== "string") return null;
    rows.push({
      row_kind: row.row_kind,
      stable_history_identity: typeof row.stable_history_identity === "string"
        && row.stable_history_identity.length > 0
        ? row.stable_history_identity
        : null,
      body: row.body,
    });
  }
  return rows;
}

export function historyPrependTranscriptFailures({ before, after }) {
  if (!Array.isArray(before) || before.length < 1 || !Array.isArray(after)) {
    return ["history-prepend-transcript-invalid"];
  }
  const failures = [];
  if (after.length <= before.length) failures.push("history-prepend-transcript-not-expanded");
  const suffix = after.slice(Math.max(0, after.length - before.length));
  if (JSON.stringify(suffix) !== JSON.stringify(before)) {
    failures.push("history-prepend-transcript-suffix-drift");
  }
  return failures;
}

export function historyFixtureConversationFailures(projection, turns) {
  const rows = historyConversationRows(projection);
  if (!Array.isArray(turns) || turns.length < 1 || rows === null) {
    return ["history-fixture-conversation-invalid"];
  }
  const expected = turns.flatMap((turn) => [
    { row_kind: "user", body: turn.prompt },
    { row_kind: "assistant", body: turn.responseText },
  ]);
  const failures = [];
  if (rows.length !== expected.length) {
    failures.push("history-fixture-conversation-count-mismatch");
  }
  if (expected.some((row, index) => row.row_kind !== rows[index]?.row_kind
    || row.body !== rows[index]?.body)) {
    failures.push("history-fixture-conversation-order-mismatch");
  }
  const userIdentities = rows
    .filter((row) => row.row_kind === "user")
    .map((row) => row.stable_history_identity);
  if (userIdentities.some((identity) => identity === null)
    || new Set(userIdentities).size !== userIdentities.length) {
    failures.push("history-fixture-user-identity-invalid");
  }
  return [...new Set(failures)];
}

export function exactHistoryFixtureLedger(ledger, expectedCount) {
  return Array.isArray(ledger)
    && ledger.length === expectedCount
    && ledger.every((row) => row?.method === "POST"
      && row.pathname === "/v1/responses"
      && row.response_phase === "completed"
      && row.response_status === 200
      && row.contract?.pass === true);
}

function fixtureLedgerDecision(ledger, expectedCount) {
  if (!Array.isArray(ledger) || ledger.length > expectedCount) return "fail";
  for (const row of ledger) {
    if (row?.method !== "POST" || row.pathname !== "/v1/responses") return "fail";
    if (row.response_status !== null && row.response_status !== 200) return "fail";
    if (row.response_status === 200
      && (row.response_phase !== "completed" || row.contract?.pass !== true)) return "fail";
  }
  return exactHistoryFixtureLedger(ledger, expectedCount) ? "pass" : "pending";
}

export function historyFixtureTurnDecision(
  { projection, ledger },
  { prompt, responseText, expectedResponseCount, expectedSessionId = null } = {},
) {
  const ledgerDecision = fixtureLedgerDecision(ledger, expectedResponseCount);
  if (ledgerDecision === "fail") return "fail";
  if (["failed", "cancelled", "incomplete"].includes(projection?.run_status_key)) return "fail";
  const settled = projection?.run_status_key === "completed"
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
  if (!settled || ledgerDecision !== "pass") return "pending";
  const row = selectedRestartSessionRow(projection);
  const expectedState = projection?.run_target?.expectedState;
  const page = restartTurnPageMetadata(projection);
  const exact = row !== null
    && (expectedSessionId === null || row.session_id === expectedSessionId)
    && expectedState?.kind === "idle"
    && typeof expectedState.latestTurnId === "string"
    && expectedState.latestTurnId.length > 0
    && expectedState.admissionRevision === row.admission_revision
    && page !== null
    && page.has_more === false
    && latestBody(projection, "user") === prompt
    && latestBody(projection, "assistant") === responseText;
  return exact ? "pass" : "fail";
}

export function historyFixtureThresholdReached(projection) {
  const page = restartTurnPageMetadata(projection);
  return page !== null
    && page.has_more === false
    && page.total > page.limit * 2;
}

export function historyRestartOwner(projection) {
  const row = selectedRestartSessionRow(projection);
  const expectedState = projection?.run_target?.expectedState;
  const page = restartTurnPageMetadata(projection);
  if (row === null
    || expectedState?.kind !== "idle"
    || typeof expectedState.latestTurnId !== "string"
    || expectedState.latestTurnId.length === 0
    || typeof expectedState.admissionRevision !== "string"
    || expectedState.admissionRevision.length === 0
    || row.admission_revision !== expectedState.admissionRevision
    || page === null) return null;
  return {
    sessionId: row.session_id,
    turnId: expectedState.latestTurnId,
    admissionRevision: expectedState.admissionRevision,
    total: page.total,
    limit: page.limit,
  };
}

export function expectedPreviousTurnPageCommand(projection) {
  const row = selectedRestartSessionRow(projection);
  const project = projection?.selected_project_index >= 0
    ? projection?.project_rows?.[projection.selected_project_index] ?? null
    : null;
  if (row === null
    || !Number.isInteger(projection?.selected_session_index)
    || projection.selected_session_index < 0
    || !Number.isSafeInteger(projection?.turn_page_offset)
    || projection.turn_page_offset <= 0) return null;
  return {
    command: "load_previous_turn_page",
    args: {
      index: projection.selected_session_index,
      expectedTarget: {
        workspacePath: projection.workspace_path,
        ownerProjectId: project?.project_id ?? null,
        ownerSessionId: row.session_id,
        rowId: row.session_id,
      },
      expectedOffset: projection.turn_page_offset,
    },
  };
}

async function desktopProjection(cdp) {
  return cdp.evaluate(`(async () => {
    const invoke = window.__TAURI_INTERNALS__?.invoke;
    if (typeof invoke !== 'function') throw new Error('tauri-invoke-unavailable');
    return invoke('desktop_state');
  })()`);
}

async function submitFixturePrompt(cdp, projection, prompt) {
  const args = {
    text: prompt,
    expectedTarget: projection.draft_target,
    expectedRunTarget: projection.run_target,
  };
  return cdp.evaluate(`(async () => {
    const invoke = window.__TAURI_INTERNALS__?.invoke;
    if (typeof invoke !== 'function') throw new Error('tauri-invoke-unavailable');
    return invoke('submit_prompt', ${JSON.stringify(args)});
  })()`);
}

async function waitForFixtureTurn({ cdp, provider, turn, expectedResponseCount, sessionId }) {
  let terminalDecision = "pending";
  const observed = await waitForObservation({
    label: `history fixture turn ${expectedResponseCount}`,
    timeoutMs: FIXTURE_TURN_TIMEOUT_MS,
    pollMs: 50,
    sample: async () => ({
      projection: await desktopProjection(cdp),
      ledger: provider.requestLedger,
    }),
    accept: (sample) => {
      terminalDecision = historyFixtureTurnDecision(sample, {
        ...turn,
        expectedResponseCount,
        expectedSessionId: sessionId,
      });
      return terminalDecision !== "pending";
    },
    retrySampleErrors: false,
  });
  if (terminalDecision === "fail") {
    throw productFailure(
      "history-fixture-turn-mismatch",
      "the deterministic history fixture turn did not settle to one exact completed response",
      observed.value,
    );
  }
  return observed.value;
}

async function waitForRestartPage({
  cdp,
  provider,
  owner,
  expectedResponseCount,
  previousOffset = null,
  overlay,
}) {
  let classified = null;
  let wireFailure = null;
  const observed = await waitForObservation({
    label: previousOffset === null
      ? "history latest page after restart"
      : "history previous page settlement",
    timeoutMs: 60_000,
    pollMs: 50,
    sample: async () => ({
      projection: await desktopProjection(cdp),
      ledger: provider.requestLedger,
    }),
    accept: ({ projection, ledger }) => {
      if (!exactHistoryFixtureLedger(ledger, expectedResponseCount)) {
        wireFailure = { expected_response_count: expectedResponseCount, ledger };
        return true;
      }
      classified = classifyRestartTurnPage(projection, {
        expectedSessionId: owner.sessionId,
        expectedTurnId: owner.turnId,
        expectedAdmissionRevision: owner.admissionRevision,
        expectedTotal: owner.total,
        expectedLimit: owner.limit,
        requireLatestSuffix: previousOffset === null,
      });
      if (classified.decision === "fail") return true;
      if (projection?.overlay !== overlay || classified.decision === "pending") return false;
      return previousOffset === null || classified.metadata.offset !== previousOffset;
    },
    retrySampleErrors: false,
  });
  if (wireFailure !== null) {
    throw productFailure(
      "history-restart-provider-wire-drift",
      "history restart or prepend changed the accepted deterministic provider ledger",
      wireFailure,
    );
  }
  if (classified?.decision === "fail") {
    throw productFailure(
      "history-restart-page-owner-drift",
      "the restarted bounded history page changed its durable owner",
      { classified, projection: observed.value.projection },
    );
  }
  return { ...observed.value, classified };
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

async function insertPaletteFilter(input) {
  const click = await trustedClick(input, COMMAND_PALETTE_SEARCH);
  const start = click.sequence;
  const inserted = await input.insertText(COMMAND_PALETTE_SEARCH, FILTER_TEXT);
  const snapshot = await input.snapshotProbe(start);
  return {
    click,
    inserted,
    probe: assertTrustedTextInsertion(snapshot, {
      afterSequence: start,
      identity: COMMAND_PALETTE_SEARCH.identity,
      text: FILTER_TEXT,
    }),
  };
}

async function closePalette(input, cdp) {
  const start = (await input.snapshotProbe()).sequence;
  await input.pressKey("Escape");
  const snapshot = await input.snapshotProbe(start);
  const probe = assertTrustedProbeSequence(snapshot, {
    afterSequence: start,
    expected: [
      { type: "keydown", key: "Escape", code: "Escape" },
      { type: "keyup", key: "Escape", code: "Escape" },
    ],
  });
  const closed = await waitForObservation({
    label: "history command palette close",
    timeoutMs: 10_000,
    pollMs: 50,
    sample: () => desktopProjection(cdp),
    accept: (projection) => projection?.overlay === "none",
    retrySampleErrors: false,
  });
  return { probe, projectionRevision: closed.value.projection_revision };
}

async function waitForPreviousTarget(input) {
  let observed;
  try {
    observed = await waitForSemanticTargetSettlement({
      input,
      locator: PREVIOUS_TURN_PAGE,
      label: "history previous-page semantic target",
      timeoutMs: 10_000,
      pollMs: 16,
    });
  } catch (error) {
    if (error?.code !== "observation-timeout" || error?.evidence?.last_error) throw error;
    throw productFailure(
      "history-previous-target-timeout",
      "the previous-page action did not reappear after the page projection settled",
      error.evidence,
    );
  }
  if (observed.value.classified.decision === "fail") {
    throw productFailure(
      "history-previous-target-ambiguous",
      "the previous-page semantic action resolved to an ambiguous DOM owner",
      observed.value,
    );
  }
  return observed.value;
}

async function waitForExactCommand(commands, afterSequence, expected) {
  const observed = await waitForObservation({
    label: "history previous-page Desktop command",
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

async function settleProbeResources(state, input, commands, primaryError) {
  let cleanupError = null;
  if (input !== null) {
    try { await input.cleanup(); }
    catch (error) {
      state.inputCleanupFailure = errorObservation(error);
      cleanupError ??= error;
    }
  }
  if (commands !== null) {
    try { await commands.remove(); }
    catch (error) {
      state.commandCleanupFailure = errorObservation(error);
      cleanupError ??= error;
    }
  }
  if (primaryError === null && cleanupError !== null) {
    throw harnessFailure(
      "history-probe-cleanup-failed",
      "history WebView input or Desktop command probe cleanup did not settle",
      {
        input: state.inputCleanupFailure,
        command: state.commandCleanupFailure,
      },
    );
  }
}

export function createHistoryRestartPrependScenario() {
  const state = {
    provider: null,
    acceptedLedger: null,
    quiesceOutcome: null,
    inputCleanupFailure: null,
    commandCleanupFailure: null,
  };
  return Object.freeze({
    id: "history.restart-prepend",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    requestGracefulExit,
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({
        turns: historyRestartFixtureTurns(),
        orderedConversation: true,
      });
      await prepareDesktopFixture({
        context,
        sink,
        phase,
        owner: OWNER,
        configText: providerRestartFixtureConfig(state.provider.baseUrl),
        sentinelName: "E2E_HISTORY_RESTART_PREPEND.txt",
        sentinelText: "moyAI Desktop E2E bounded history restart/prepend fixture.\n",
      });
      await sink.record("history-scripted-provider-started", state.provider.resourceObservation(), {
        phase,
        owner: OWNER,
      });
    },
    async execute({ context, driver: firstCdp, host, sink }) {
      const provider = state.provider;
      if (provider === null) throw new Error("history scripted provider was not prepared");
      await acquireInteractiveShell({ context, driver: firstCdp, sink }, {
        evidenceOwner: OWNER,
        screenshotStem: "history-shell-ready",
      });
      if (provider.requestLedger.length !== 0) {
        throw productFailure(
          "history-cold-start-provider-request",
          "Desktop contacted the history fixture provider before an explicit fixture turn",
          { ledger: provider.requestLedger },
        );
      }

      const fixtureRows = [];
      let projection = await desktopProjection(firstCdp);
      let sessionId = null;
      for (const [index, turn] of historyRestartFixtureTurns().entries()) {
        await submitFixturePrompt(firstCdp, projection, turn.prompt);
        const completed = await waitForFixtureTurn({
          cdp: firstCdp,
          provider,
          turn,
          expectedResponseCount: index + 1,
          sessionId,
        });
        projection = completed.projection;
        const owner = historyRestartOwner(projection);
        if (owner === null) {
          throw productFailure(
            "history-fixture-owner-missing",
            "a completed fixture turn did not expose its durable restart owner",
            { index: index + 1, projection },
          );
        }
        sessionId ??= owner.sessionId;
        fixtureRows.push({
          turn: index + 1,
          session_id: owner.sessionId,
          latest_turn_id: owner.turnId,
          admission_revision: owner.admissionRevision,
          turn_page_total: owner.total,
          turn_page_limit: owner.limit,
          turn_page_offset: projection.turn_page_offset,
        });
        if (historyFixtureThresholdReached(projection)) break;
      }
      if (!historyFixtureThresholdReached(projection)) {
        throw harnessFailure(
          "history-fixture-page-bound",
          "the bounded deterministic fixture could not create two previous-page transitions",
          { maximum_turns: SCRIPTED_PROVIDER_MAX_TURNS, fixture_rows: fixtureRows },
        );
      }
      const owner = historyRestartOwner(projection);
      if (owner === null) throw new Error("settled history fixture owner disappeared");
      const acceptedBeforeRestart = structuredClone(provider.requestLedger);
      const beforeScreenshot = await captureScenarioScreenshot({
        cdp: firstCdp,
        sink,
        name: "history-fixture-completed",
        owner: OWNER,
      });
      await sink.record("history-fixture-ready", {
        setup_input_kind: "direct_tauri_fixture_setup",
        provider_request_count: acceptedBeforeRestart.length,
        owner,
        turns: fixtureRows,
        screenshot: beforeScreenshot,
      }, { phase: "executing", owner: OWNER });

      const restarted = await host.restart({
        context,
        scenario: this,
        sink,
        driver: firstCdp,
        phase: "executing",
      });
      await acquireInteractiveShell({ context, driver: restarted.driver, sink }, {
        evidenceOwner: OWNER,
        screenshotStem: "history-restarted-shell-ready",
      });
      if (!exactHistoryFixtureLedger(provider.requestLedger, acceptedBeforeRestart.length)) {
        throw productFailure(
          "history-restart-provider-wire-drift",
          "Desktop restart changed the accepted deterministic provider ledger",
          { before: acceptedBeforeRestart, after: provider.requestLedger },
        );
      }
      const restored = await waitForRestartPage({
        cdp: restarted.driver,
        provider,
        owner,
        expectedResponseCount: acceptedBeforeRestart.length,
        overlay: "none",
      });

      const input = new WebviewInput(restarted.driver, { probeId: "history-restart-prepend" });
      const commands = new DesktopCommandProbe(restarted.driver, {
        probeId: "history-restart-prepend",
        commands: ["load_previous_turn_page"],
      });
      let primaryError = null;
      let resourcesSettled = false;
      try {
        await input.installProbe();
        await commands.install();
        const showPalette = await trustedClick(input, SHOW_COMMAND_PALETTE);
        await waitForObservation({
          label: "history command palette open",
          timeoutMs: 10_000,
          pollMs: 50,
          sample: () => desktopProjection(restarted.driver),
          accept: (value) => value?.overlay === "command_palette",
          retrySampleErrors: false,
        });
        const filter = await insertPaletteFilter(input);

        const pages = [];
        let current = restored;
        const maximumPages = Math.ceil(current.classified.metadata.offset / owner.limit);
        while (current.classified.decision === "page_needed") {
          if (pages.length >= maximumPages) {
            throw productFailure(
              "history-prepend-page-bound",
              "history prepend required more transitions than canonical metadata permits",
              { maximum_pages: maximumPages, pages, current },
            );
          }
          const before = current.classified.metadata;
          const beforeConversation = historyConversationRows(current.projection);
          const target = await waitForPreviousTarget(input);
          const expectedCommand = expectedPreviousTurnPageCommand(current.projection);
          if (expectedCommand === null) {
            throw productFailure(
              "history-prepend-command-owner-missing",
              "the ready previous-page action had no exact row mutation target",
              { projection: current.projection },
            );
          }
          const commandStart = (await commands.snapshot()).sequence;
          const click = await trustedClick(input, PREVIOUS_TURN_PAGE);
          const command = await waitForExactCommand(commands, commandStart, expectedCommand);
          const settled = await waitForRestartPage({
            cdp: restarted.driver,
            provider,
            owner,
            expectedResponseCount: acceptedBeforeRestart.length,
            previousOffset: before.offset,
            overlay: "command_palette",
          });
          const after = settled.classified.metadata;
          const afterConversation = historyConversationRows(settled.projection);
          const failures = [
            ...restartPreviousPageTransitionFailures({ before, after }),
            ...historyPrependTranscriptFailures({
              before: beforeConversation,
              after: afterConversation,
            }),
          ];
          const evidence = {
            page: pages.length + 1,
            before,
            after,
            transcript_rows_before: beforeConversation?.length ?? null,
            transcript_rows_after: afterConversation?.length ?? null,
            target,
            click,
            command,
            failures,
          };
          await sink.record("history-restart-prepend", evidence, {
            phase: "executing",
            owner: OWNER,
          });
          if (failures.length > 0) {
            throw productFailure(
              "history-prepend-transition-drift",
              "the previous-page transition did not prepend the exact canonical range",
              evidence,
            );
          }
          pages.push({ before, after });
          current = settled;
        }
        if (pages.length < 2 || current.classified.metadata.offset !== 0) {
          throw productFailure(
            "history-prepend-coverage-incomplete",
            "the lightweight scenario did not exercise at least two semantic target settlements through offset zero",
            { pages, final: current.classified },
          );
        }
        const conversationFailures = historyFixtureConversationFailures(
          current.projection,
          historyRestartFixtureTurns().slice(0, acceptedBeforeRestart.length),
        );
        if (conversationFailures.length > 0) {
          throw productFailure(
            "history-prepend-final-conversation-drift",
            "the fully prepended transcript did not restore the exact deterministic conversation",
            {
              failures: conversationFailures,
              expected_turn_count: acceptedBeforeRestart.length,
              observed_conversation_row_count: historyConversationRows(current.projection)?.length ?? null,
            },
          );
        }
        const close = await closePalette(input, restarted.driver);
        const finalScreenshot = await captureScenarioScreenshot({
          cdp: restarted.driver,
          sink,
          name: "history-prepend-completed",
          owner: OWNER,
        });
        state.acceptedLedger = structuredClone(provider.requestLedger);
        await sink.record("history-restart-prepend-completed", {
          restart: restarted.restart,
          owner,
          show_palette: showPalette,
          filter,
          pages,
          close,
          final_projection_revision: current.projection.projection_revision,
          accepted_provider_ledger: state.acceptedLedger,
          screenshot: finalScreenshot,
        }, { phase: "executing", owner: OWNER });
        resourcesSettled = true;
        await settleProbeResources(state, input, commands, null);
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        if (!resourcesSettled) {
          resourcesSettled = true;
          await settleProbeResources(state, input, commands, primaryError);
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
        && state.inputCleanupFailure === null
        && state.commandCleanupFailure === null;
      return {
        input: pass ? "pass" : "fail",
        resources: [{
          kind: "history-restart-prepend-verification",
          quiesce_input: state.quiesceOutcome?.input ?? null,
          input_cleanup_failure: state.inputCleanupFailure,
          command_cleanup_failure: state.commandCleanupFailure,
        }],
      };
    },
  });
}

import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { WebviewInput, assertTrustedProbeSequence } from "../drivers/webview_input.mjs";
import {
  SCRIPTED_PROVIDER_MODEL_ID,
  SCRIPTED_PROVIDER_RESPONSE,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import {
  captureScenarioScreenshot,
  selectedNavigationIdentity,
} from "./observations.mjs";

const OWNER = "scenario:provider.restart";
export const PROVIDER_RESTART_PROMPT = "return only main-ok";
export const RESTORED_STATE_STABILITY_MS = 500;
const SHOW_PROVIDER = Object.freeze({
  selector: 'aside.sidebar button[data-action="show-provider"][title="LLM URL"]',
  identity: { tag: "BUTTON", action: "show-provider" },
});
const LOAD_MODELS = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="provider-dialog-title"] button[data-action="load-provider-models"]',
  identity: { tag: "BUTTON", action: "load-provider-models" },
});
const CLOSE_PROVIDER = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="provider-dialog-title"] button[data-action="close-overlay"]',
  identity: { tag: "BUTTON", action: "close-overlay" },
});
const PROMPT_TARGET = Object.freeze({
  selector: "textarea#prompt",
  identity: { tag: "TEXTAREA", id: "prompt" },
});
const SEND = Object.freeze({
  selector: 'button[data-action="send"]',
  identity: { tag: "BUTTON", action: "send" },
});

export function providerRestartFixtureConfig(baseUrl, { supportsTools = false } = {}) {
  if (typeof supportsTools !== "boolean") {
    throw new TypeError("supportsTools must be boolean");
  }
  return `[model]
base_url = ${JSON.stringify(baseUrl)}
model = ${JSON.stringify(SCRIPTED_PROVIDER_MODEL_ID)}
provider_metadata_mode = "openai_compatible_only"
provider_api_mode = "responses"
connect_timeout_ms = 1000
request_timeout_ms = 30000
max_retries = 0
context_window = 65536
max_output_tokens = 1024
supports_tools = ${supportsTools}
supports_images = false
parallel_tool_calls = false

[model.extra_body_json]

[permissions]
access_mode = "default"

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

function sameValue(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

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

export function relevantProviderHistory(projection) {
  const rows = Array.isArray(projection?.transcript_rows) ? projection.transcript_rows : [];
  const normalized = rows.map((row) => ({
    identity: typeof row?.stable_history_identity === "string" && row.stable_history_identity.length > 0
      ? row.stable_history_identity
      : null,
    kind: row?.row_kind ?? null,
    title: row?.title ?? null,
    body: row?.body ?? null,
  }));
  return {
    rows: normalized,
    users: normalized.filter((row) => row.kind === "user").map((row) => row.body),
    assistants: normalized.filter((row) => row.kind === "assistant").map((row) => row.body),
    completed_summaries: normalized.filter((row) => row.kind === "work_summary_completed").length,
    user_identities: normalized.filter((row) => row.kind === "user").map((row) => row.identity),
    assistant_identities: normalized.filter((row) => row.kind === "assistant").map((row) => row.identity),
    completed_summary_identities: normalized
      .filter((row) => row.kind === "work_summary_completed")
      .map((row) => row.identity),
  };
}

export function exactFreshProviderHistory(history, {
  expectedPrompt = PROVIDER_RESTART_PROMPT,
  expectedResponse = SCRIPTED_PROVIDER_RESPONSE,
} = {}) {
  const durableIdentities = [history?.user_identities?.[0], history?.completed_summary_identities?.[0]];
  return Array.isArray(history?.rows)
    && history.rows.length === 3
    && sameValue(history.rows.map((row) => row.kind), ["user", "work_summary_completed", "assistant"])
    && sameValue(history.users, [expectedPrompt])
    && sameValue(history.assistants, [expectedResponse])
    && history.completed_summaries === 1
    && history.user_identities.length === 1
    && history.assistant_identities.length === 1
    && history.completed_summary_identities.length === 1
    && durableIdentities.every((identity) => typeof identity === "string" && identity.length > 0)
    && new Set(durableIdentities).size === durableIdentities.length;
}

export function settledCompletedProviderTurn(projection, expectations = {}) {
  const history = relevantProviderHistory(projection);
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
    && projection?.navigation_admission_open === true
    && projection?.provider_loading === false
    && projection?.overlay === "none"
    && projection?.confirmation_visible === false
    && projection?.confirmation_id === null
    && projection?.confirmation == null
    && projection?.draft_prompt === ""
    && projection?.composer_submit_mode === "new_request"
    && projection?.can_submit === true
    && exactFreshProviderHistory(history, expectations);
}

function catalogRowAccepted(row) {
  return row?.method === "GET"
    && row?.pathname === "/v1/models"
    && row?.response_status === 200;
}

function responseRowAccepted(row) {
  return row?.method === "POST"
    && row?.pathname === "/v1/responses"
    && row?.response_status === 200
    && row?.contract?.pass === true;
}

export function exactProviderCatalogLedger(ledger) {
  return Array.isArray(ledger) && ledger.length === 1 && catalogRowAccepted(ledger[0]);
}

export function exactProviderTurnLedger(ledger) {
  return Array.isArray(ledger)
    && ledger.length === 2
    && catalogRowAccepted(ledger[0])
    && responseRowAccepted(ledger[1]);
}

export function classifyAcquiredObservationFailure(error, { code, message }) {
  if (
    error?.code === "observation-timeout"
    && error?.evidence !== null
    && typeof error.evidence === "object"
    && error.evidence.last_value !== null
    && error.evidence.last_error === null
  ) {
    return productFailure(code, message, { observation: error.evidence });
  }
  return error;
}

async function waitForAcquiredProductStage({ label, timeoutMs, sample, decide, code, message }) {
  let observed;
  let terminalDecision = "pending";
  try {
    observed = await waitForObservation({
      label,
      timeoutMs,
      pollMs: 100,
      sample,
      accept: (value) => {
        terminalDecision = decide(value);
        return terminalDecision !== "pending";
      },
    });
  } catch (error) {
    throw classifyAcquiredObservationFailure(error, { code, message });
  }
  if (terminalDecision === "fail") {
    throw productFailure(code, message, { observation: observed });
  }
  return observed;
}

async function invokeDesktopProjection(cdp) {
  return cdp.evaluate(`(async () => {
    const invoke = window.__TAURI_INTERNALS__?.invoke;
    if (typeof invoke !== 'function') throw new Error('tauri-invoke-unavailable');
    return invoke('desktop_state');
  })()`);
}

export async function observeProviderTurnSurface(cdp) {
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
    const rows = (selector) => Array.from(document.querySelectorAll(selector)).map((row) => ({
      history_identity: row.getAttribute('data-history-identity'),
      text: (row.querySelector('.markdown-body')?.innerText ?? '').trim(),
      visible: visible(row),
    }));
    const selected = Array.from(document.querySelectorAll(
      'button.nav-row[aria-current="page"][data-action="session"], button.nav-row[aria-current="page"][data-action="chat-session"]'
    )).map((row) => ({
      action: row instanceof HTMLElement ? (row.dataset.action ?? null) : null,
      focus_key: row instanceof HTMLElement ? (row.dataset.focusKey ?? null) : null,
      visible: visible(row),
    }));
    const prompt = document.querySelector('textarea#prompt');
    const send = document.querySelector('button[data-action="send"]');
    return {
      projection,
      thread_count: document.querySelectorAll('main.conversation #thread').length,
      users: rows('main.conversation #thread article.message.user'),
      assistants: rows('main.conversation #thread article.message.assistant'),
      completed_summaries: rows('main.conversation #thread article.message.work-summary.work_summary_completed'),
      selected_navigation: selected,
      prompt: {
        count: document.querySelectorAll('textarea#prompt').length,
        value: prompt instanceof HTMLTextAreaElement ? prompt.value : null,
        visible: visible(prompt),
        enabled: prompt instanceof HTMLTextAreaElement && !prompt.disabled && !prompt.readOnly,
      },
      send: {
        count: document.querySelectorAll('button[data-action="send"]').length,
        visible: visible(send),
        enabled: send instanceof HTMLButtonElement && !send.disabled,
      },
      visible_fatal_count: Array.from(document.querySelectorAll('.fatal')).filter(visible).length,
      visible_recoverable_error_count: Array.from(document.querySelectorAll('.ui-error-notice')).filter(visible).length,
      visible_dialog_count: Array.from(document.querySelectorAll('[role="dialog"], [role="alertdialog"]')).filter(visible).length,
      visible_modal_backdrop_count: Array.from(document.querySelectorAll('.modal-backdrop')).filter(visible).length,
    };
  })()`);
}

function exactIdentityForKind(history, kind) {
  const identities = history.rows.filter((row) => row.kind === kind).map((row) => row.identity);
  return identities.length === 1 ? identities[0] : null;
}

export function providerTurnDomAccepted(surface, history, identity, {
  expectedPrompt = PROVIDER_RESTART_PROMPT,
  expectedResponse = SCRIPTED_PROVIDER_RESPONSE,
} = {}) {
  const expectedAction = identity?.project_id === null ? "chat-session" : "session";
  const expectedFocusKey = typeof identity?.session_id === "string"
    ? `${expectedAction}:${identity.session_id}:select`
    : null;
  const userIdentity = exactIdentityForKind(history, "user");
  const assistantIdentity = exactIdentityForKind(history, "assistant");
  const summaryIdentity = exactIdentityForKind(history, "work_summary_completed");
  return surface?.thread_count === 1
    && surface?.users?.length === 1
    && surface.users[0].visible === true
    && surface.users[0].text === expectedPrompt
    && surface.users[0].history_identity === userIdentity
    && surface?.assistants?.length === 1
    && surface.assistants[0].visible === true
    && surface.assistants[0].text === expectedResponse
    && surface.assistants[0].history_identity === assistantIdentity
    && surface?.completed_summaries?.length === 1
    && surface.completed_summaries[0].visible === true
    && surface.completed_summaries[0].history_identity === summaryIdentity
    && surface?.selected_navigation?.length === 1
    && surface.selected_navigation[0].visible === true
    && surface.selected_navigation[0].action === expectedAction
    && surface.selected_navigation[0].focus_key === expectedFocusKey
    && surface?.prompt?.count === 1
    && surface.prompt.value === ""
    && surface.prompt.visible === true
    && surface.prompt.enabled === true
    && surface?.visible_fatal_count === 0
    && surface?.visible_recoverable_error_count === 0
    && surface?.visible_dialog_count === 0
    && surface?.visible_modal_backdrop_count === 0;
}

function catalogDecision({ projection, ledger }) {
  if (!Array.isArray(ledger)) return "fail";
  if (ledger.length > 1) return "fail";
  if (ledger.length === 1) {
    const row = ledger[0];
    if (row?.method !== "GET" || row?.pathname !== "/v1/models") return "fail";
    if (row.response_status !== null && row.response_status !== 200) return "fail";
  }
  if (projection?.provider_loading === false && projection?.provider_status?.kind === "error") return "fail";
  return exactProviderCatalogLedger(ledger)
    && projection?.provider_loading === false
    && projection?.provider_status?.kind === "success"
    && sameValue(projection?.provider_model_ids, [SCRIPTED_PROVIDER_MODEL_ID])
    ? "pass"
    : "pending";
}

function terminalDecision({ surface, ledger }) {
  if (!Array.isArray(ledger) || ledger.length > 2) return "fail";
  if (ledger.length >= 1 && !catalogRowAccepted(ledger[0])) return "fail";
  if (ledger.length === 2) {
    const row = ledger[1];
    if (row?.method !== "POST" || row?.pathname !== "/v1/responses") return "fail";
    if (row.response_status !== null && !responseRowAccepted(row)) return "fail";
  }
  if (
    surface?.visible_fatal_count > 0
    || surface?.visible_recoverable_error_count > 0
    || ["failed", "cancelled", "incomplete"].includes(surface?.projection?.run_status_key)
  ) return "fail";
  const projection = surface?.projection;
  const history = relevantProviderHistory(projection);
  const identity = selectedNavigationIdentity(projection);
  return exactProviderTurnLedger(ledger)
    && settledCompletedProviderTurn(projection)
    && providerTurnDomAccepted(surface, history, identity)
    ? "pass"
    : "pending";
}

function restartDecision(sample, expected) {
  if (!exactProviderTurnLedger(sample?.ledger)) return sample?.ledger?.length > 2 ? "fail" : "pending";
  const surface = sample?.surface;
  if (
    surface?.visible_fatal_count > 0
    || surface?.visible_recoverable_error_count > 0
    || surface?.projection?.startup?.status === "failed"
  ) return "fail";
  const projection = surface?.projection;
  const identity = selectedNavigationIdentity(projection);
  const history = relevantProviderHistory(projection);
  return settledCompletedProviderTurn(projection)
    && sameValue(identity, expected.identity)
    && sameValue(history, expected.history)
    && providerTurnDomAccepted(surface, history, identity)
    ? "pass"
    : "pending";
}

export function createStableRestartDecision({
  expected,
  minimumStableMs = RESTORED_STATE_STABILITY_MS,
  now = () => Date.now(),
}) {
  if (!Number.isFinite(minimumStableMs) || minimumStableMs <= 0) {
    throw new TypeError("minimum stable duration must be positive");
  }
  let continuouslyAcceptedSince = null;
  return (sample) => {
    const decision = restartDecision(sample, expected);
    if (decision !== "pass") {
      continuouslyAcceptedSince = null;
      return decision;
    }
    const observedAt = now();
    if (continuouslyAcceptedSince === null) {
      continuouslyAcceptedSince = observedAt;
      return "pending";
    }
    return observedAt - continuouslyAcceptedSince >= minimumStableMs ? "pass" : "pending";
  };
}

function keyCode(character) {
  if (/^[a-z]$/.test(character)) return `Key${character.toUpperCase()}`;
  if (/^[0-9]$/.test(character)) return `Digit${character}`;
  if (character === " ") return "Space";
  if (character === "-") return "Minus";
  throw new TypeError(`unsupported provider scenario character: ${character}`);
}

function expectedTypedEvents(text) {
  return Array.from(text).flatMap((character) => [
    { type: "keydown", identity: PROMPT_TARGET.identity, key: character, code: keyCode(character) },
    { type: "input", identity: PROMPT_TARGET.identity, inputType: "insertText", data: character },
    { type: "keyup", identity: PROMPT_TARGET.identity, key: character, code: keyCode(character) },
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

async function acquireAndRecordTrustedClick({ input, locator, action, sink }) {
  const acquisition = await trustedClick(input, locator);
  await sink.record("trusted-provider-action-acquired", {
    action,
    input_kind: "browser_trusted",
    ...acquisition,
  }, { phase: "executing", owner: OWNER });
  return acquisition;
}

export async function quiesceProviderResource({ provider, acceptedLedger, inputs }) {
  if (provider === null) {
    return {
      input: "pass",
      resources: [{ kind: "scripted-provider", started: false, closed: true }],
      productFailure: null,
    };
  }
  let closeObservation = null;
  let closeFailure = null;
  try { closeObservation = await provider.close(); }
  catch (error) { closeFailure = errorObservation(error); }
  const finalLedger = provider.requestLedger;
  const closePass = closeFailure === null
    && closeObservation?.pass === true
    && closeObservation?.forced_connection_count === 0;
  const acceptedMissing = inputs?.oracle === "pass" && acceptedLedger === null;
  const ledgerDrift = acceptedLedger !== null && !sameValue(finalLedger, acceptedLedger);
  return {
    input: closePass && !acceptedMissing ? "pass" : "fail",
    resources: [{
      kind: "scripted-provider",
      close: closeObservation,
      close_failure: closeFailure,
      accepted_ledger: acceptedLedger,
      final_ledger: finalLedger,
    }],
    productFailure: ledgerDrift ? {
      code: "provider-post-terminal-wire-drift",
      message: "provider wire activity changed after the accepted restored terminal",
      evidence: { accepted_ledger: acceptedLedger, final_ledger: finalLedger },
    } : null,
  };
}

export function createProviderRestartScenario() {
  const state = {
    provider: null,
    acceptedLedger: null,
    quiesceOutcome: null,
    inputCleanupFailure: null,
  };
  return Object.freeze({
    id: "provider.restart",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    requestGracefulExit,
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({ expectedPrompt: PROVIDER_RESTART_PROMPT });
      await prepareDesktopFixture({
        context,
        sink,
        phase,
        owner: OWNER,
        configText: providerRestartFixtureConfig(state.provider.baseUrl),
        sentinelName: "E2E_PROVIDER_RESTART.txt",
        sentinelText: "moyAI Desktop E2E scripted provider and restart fixture.\n",
      });
      await sink.record("scripted-provider-started", state.provider.resourceObservation(), { phase, owner: OWNER });
    },
    async execute({ context, driver: firstCdp, host, sink }) {
      const provider = state.provider;
      if (provider === null) throw new Error("scripted provider was not prepared");
      await acquireInteractiveShell({ context, driver: firstCdp, sink }, {
        evidenceOwner: OWNER,
        screenshotStem: "provider-shell-ready",
      });
      if (provider.requestLedger.length !== 0) {
        throw productFailure("provider-cold-start-request", "Desktop contacted the provider before an explicit action", { ledger: provider.requestLedger });
      }

      const input = new WebviewInput(firstCdp, { probeId: "provider-workflow" });
      let inputCleanupAttempted = false;
      let primaryError = null;
      try {
        await input.installProbe();
        const show = await acquireAndRecordTrustedClick({
          input,
          locator: SHOW_PROVIDER,
          action: "show-provider",
          sink,
        });
        await waitForAcquiredProductStage({
          label: "provider overlay after trusted activation",
          timeoutMs: 10_000,
          sample: () => invokeDesktopProjection(firstCdp),
          decide: (projection) => projection?.overlay === "provider" && projection?.provider_loading === false ? "pass" : "pending",
          code: "provider-overlay-did-not-open",
          message: "trusted provider activation did not open the provider surface",
        });
        const load = await acquireAndRecordTrustedClick({
          input,
          locator: LOAD_MODELS,
          action: "load-provider-models",
          sink,
        });
        const catalog = await waitForAcquiredProductStage({
          label: "scripted provider catalog",
          timeoutMs: 30_000,
          sample: async () => ({ projection: await invokeDesktopProjection(firstCdp), ledger: provider.requestLedger }),
          decide: catalogDecision,
          code: "provider-catalog-contract-mismatch",
          message: "the acquired model-load action did not settle to the exact catalog contract",
        });
        await sink.record("provider-catalog-acquired", {
          show,
          load,
          projection_revision: catalog.value.projection.projection_revision,
          model_ids: catalog.value.projection.provider_model_ids,
          ledger: catalog.value.ledger,
        }, { phase: "executing", owner: OWNER });

        await acquireAndRecordTrustedClick({
          input,
          locator: CLOSE_PROVIDER,
          action: "close-provider",
          sink,
        });
        await waitForAcquiredProductStage({
          label: "provider overlay close",
          timeoutMs: 10_000,
          sample: () => invokeDesktopProjection(firstCdp),
          decide: (projection) => projection?.overlay === "none" ? "pass" : "pending",
          code: "provider-overlay-did-not-close",
          message: "trusted provider close did not restore the main surface",
        });
        await acquireAndRecordTrustedClick({
          input,
          locator: PROMPT_TARGET,
          action: "focus-prompt",
          sink,
        });
        const typeStart = (await input.snapshotProbe()).sequence;
        await input.typeText(PROVIDER_RESTART_PROMPT);
        const typedSnapshot = await input.snapshotProbe(typeStart);
        const trustedTyping = assertTrustedProbeSequence(typedSnapshot, {
          afterSequence: typeStart,
          expected: expectedTypedEvents(PROVIDER_RESTART_PROMPT),
        });
        const promptValue = await firstCdp.evaluate(`(() => document.querySelector('textarea#prompt')?.value ?? null)()`);
        if (promptValue !== PROVIDER_RESTART_PROMPT) {
          throw productFailure("provider-prompt-input-drift", "trusted keyboard input did not produce the exact provider prompt", {
            expected: PROVIDER_RESTART_PROMPT,
            actual: promptValue,
          });
        }
        const send = await acquireAndRecordTrustedClick({
          input,
          locator: SEND,
          action: "send-provider-prompt",
          sink,
        });
        const completed = await waitForAcquiredProductStage({
          label: "scripted Responses turn completion and rendered DOM",
          timeoutMs: 90_000,
          sample: async () => ({ surface: await observeProviderTurnSurface(firstCdp), ledger: provider.requestLedger }),
          decide: terminalDecision,
          code: "provider-turn-contract-mismatch",
          message: "the acquired send action did not produce the exact terminal provider turn and visible DOM",
        });
        const beforeProjection = completed.value.surface.projection;
        const beforeIdentity = selectedNavigationIdentity(beforeProjection);
        const beforeHistory = relevantProviderHistory(beforeProjection);
        const completedScreenshot = await captureScenarioScreenshot({ cdp: firstCdp, sink, name: "provider-turn-completed", owner: OWNER });
        await sink.record("scripted-provider-turn", {
          input_kind: "browser_trusted",
          typing_probe: trustedTyping,
          send,
          provider_ledger: completed.value.ledger,
          identity: beforeIdentity,
          history: beforeHistory,
          dom: completed.value.surface,
          screenshot: completedScreenshot,
        }, { phase: "executing", owner: OWNER });

        try {
          await input.cleanup();
          inputCleanupAttempted = true;
        } catch (error) {
          inputCleanupAttempted = true;
          state.inputCleanupFailure = errorObservation(error);
          throw new DesktopE2eError("harness", "provider-input-cleanup-failed", "provider WebView input did not settle before restart", state.inputCleanupFailure);
        }
        const restarted = await host.restart({ context, scenario: this, sink, driver: firstCdp, phase: "executing" });
        const expected = { identity: beforeIdentity, history: beforeHistory };
        const restored = await waitForAcquiredProductStage({
          label: "completed provider turn restored after Desktop restart",
          timeoutMs: 60_000,
          sample: async () => ({ surface: await observeProviderTurnSurface(restarted.driver), ledger: provider.requestLedger }),
          decide: (sample) => restartDecision(sample, expected),
          code: "restart-restore-mismatch",
          message: "the attached second Desktop generation did not restore the exact completed turn",
        });
        const stableDecision = createStableRestartDecision({ expected });
        const stable = await waitForAcquiredProductStage({
          label: "later ordinary restored provider state",
          timeoutMs: 10_000,
          sample: async () => ({ surface: await observeProviderTurnSurface(restarted.driver), ledger: provider.requestLedger }),
          decide: stableDecision,
          code: "restart-restore-not-stable",
          message: "the later ordinary Desktop observation did not preserve the restored provider turn",
        });
        const afterProjection = stable.value.surface.projection;
        const afterIdentity = selectedNavigationIdentity(afterProjection);
        const afterHistory = relevantProviderHistory(afterProjection);
        const restartScreenshot = await captureScenarioScreenshot({ cdp: restarted.driver, sink, name: "provider-turn-restored", owner: OWNER });
        state.acceptedLedger = structuredClone(stable.value.ledger);
        await sink.record("desktop-restart-restored", {
          restart: restarted.restart,
          first_restored: restored.value.surface,
          later_restored: stable.value.surface,
          identity: afterIdentity,
          history: afterHistory,
          accepted_provider_ledger: state.acceptedLedger,
          screenshot: restartScreenshot,
        }, { phase: "executing", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        if (!inputCleanupAttempted) {
          try { await input.cleanup(); }
          catch (error) {
            state.inputCleanupFailure = errorObservation(error);
            if (primaryError === null) {
              throw new DesktopE2eError("harness", "provider-input-cleanup-failed", "provider WebView input cleanup did not settle", state.inputCleanupFailure);
            }
          }
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
      const quiesced = state.quiesceOutcome !== null;
      const pass = quiesced
        && state.quiesceOutcome.input === "pass"
        && state.inputCleanupFailure === null;
      return {
        input: pass ? "pass" : "fail",
        resources: [{
          kind: "provider-restart-verification",
          quiesced,
          quiesce_input: state.quiesceOutcome?.input ?? null,
          input_cleanup_failure: state.inputCleanupFailure,
        }],
      };
    },
  });
}

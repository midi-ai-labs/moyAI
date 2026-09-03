import { isDeepStrictEqual } from "node:util";
import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { waitForSemanticTargetSettlement } from "../core/semantic_target_settlement.mjs";
import { WebviewInput, assertTrustedProbeSequence } from "../drivers/webview_input.mjs";
import {
  SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_PROMPT as ALPHA_PROMPT,
  SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_RESPONSE as ALPHA_RESPONSE,
  SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_BETA_PROMPT as BETA_PROMPT,
  SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_BETA_RESPONSE as BETA_RESPONSE,
  SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_QUESTION as QUESTION,
  SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_SIDE_RESPONSE as SIDE_RESPONSE,
  createSideChatSessionProviderScript,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { captureScenarioScreenshot, selectedNavigationIdentity } from "./observations.mjs";
import { providerRestartFixtureConfig, quiesceProviderResource, settledCompletedProviderTurn } from "./provider_restart.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import { executeCase52SideChatStage } from "./case5_2_side_chat.mjs";
import {
  hoveredProjectActionDecision,
  projectNewSessionLocator,
  projectRowHoverLocator,
  projectSessionSelectionLocator,
} from "./settings_session.mjs";
import {
  configureSideChat,
  observeSideChatQuoteSurface,
  surfaceHasNoErrors,
  trustedClick,
  trustedInsert,
  waitForProductStage,
} from "./side_chat_quote.mjs";

const OWNER = "scenario:side-chat.session";
export const SIDE_SESSION_MAIN_DRAFT = "メイン側の未送信メモは変更しない";
export const SIDE_SESSION_UNSENT_DRAFT = "次回は公開までの確認事項を相談する";
const MAIN_PROMPT = { selector: "section.composer textarea#prompt", identity: { tag: "TEXTAREA", id: "prompt" } };
const MAIN_SEND = { selector: 'section.composer button[data-action="send"]', identity: { tag: "BUTTON", action: "send" } };
const SHOW_SIDE = { selector: 'button[data-action="show-side-chat-pane"]', identity: { tag: "BUTTON", action: "show-side-chat-pane" } };
const SIDE_PROMPT = { selector: "aside.side-chat-pane textarea#side-chat-prompt", identity: { tag: "TEXTAREA", id: "side-chat-prompt" } };
const ROLES = ["side_session_alpha", "side_session_beta", "side_session_consult"];
const PLACEHOLDER_SESSION_TITLES = new Set(["新規チャット", "new session", "new chat"]);

function fail(code, evidence) {
  throw new DesktopE2eError("product", `side-chat-session-${code}`, `Side Chat session check failed: ${code}`, evidence);
}

export function sideSessionLedgerMatches(ledger, count) {
  return Number.isInteger(count) && count >= 0 && count <= ROLES.length
    && Array.isArray(ledger) && ledger.length === count
    && ledger.every((row, index) => row.method === "POST" && row.pathname === "/v1/responses"
      && row.contract?.role === ROLES[index] && row.contract.pass === true
      && row.response_status === 200 && row.response_phase === "completed");
}

export function sideSessionMainSnapshot(projection) {
  return {
    session_id: projection?.draft_target?.sessionId ?? null,
    title: projection?.current_session_label ?? null,
    primary_rows: (projection?.transcript_rows ?? [])
      .filter((row) => ["user", "assistant", "error"].includes(row.row_kind))
      .map((row) => ({ id: row.stable_history_identity, kind: row.row_kind, body: row.body })),
  };
}

export function sideSessionNavigationSummary(projection, sessions) {
  const rows = [...(projection?.session_rows ?? []), ...(projection?.chat_session_rows ?? [])];
  return (sessions ?? []).map((session) => {
    const matches = rows.filter((row) => row.session_id === session?.session_id);
    const row = matches[0];
    return {
      session_id: session?.session_id ?? null,
      expected_title: session?.title ?? null,
      expected_label: typeof session?.title === "string" && typeof session?.session_id === "string"
        ? `${session.title} [完了] ${session.session_id.slice(0, 8)}`
        : null,
      occurrence_count: matches.length,
      title: row?.title ?? null,
      status: row?.status ?? null,
      loaded_status: row?.loaded_status ?? null,
      active_turn_id: row?.active_turn_id ?? null,
      active_turn_sequence_no: row?.active_turn_sequence_no ?? null,
      interrupt_target: row?.interrupt_target ?? null,
      pending_permission_requests: row?.pending_permission_requests ?? null,
      pending_user_input_requests: row?.pending_user_input_requests ?? null,
      label: row?.label ?? null,
    };
  });
}

export function sideSessionTerminalNavigationMatches(projection, sessions) {
  const summary = sideSessionNavigationSummary(projection, sessions);
  return summary.length > 0 && summary.every((row) =>
    typeof row.session_id === "string"
    && typeof row.expected_title === "string" && row.expected_title.trim().length > 0
    && !PLACEHOLDER_SESSION_TITLES.has(row.expected_title.trim().toLocaleLowerCase("en-US"))
    && row.occurrence_count === 1 && row.title === row.expected_title
    && row.label === row.expected_label
    && row.status === "completed" && row.loaded_status === "idle"
    && row.active_turn_id === null && row.active_turn_sequence_no === null
    && row.interrupt_target === null
    && row.pending_permission_requests === 0 && row.pending_user_input_requests === 0);
}

function selectedTerminalNavigationMatches(projection, session) {
  const [summary] = sideSessionNavigationSummary(projection, [session]);
  return sideSessionTerminalNavigationMatches(projection, [session])
    && projection?.current_session_label === session?.title
    && projection?.selected_session_title === summary?.label;
}

function mainMatches(surface, snapshot, draft) {
  const projection = surface?.projection;
  return typeof snapshot?.session_id === "string"
    && isDeepStrictEqual(sideSessionMainSnapshot(projection), snapshot)
    && selectedNavigationIdentity(projection).session_id === snapshot.session_id
    && selectedTerminalNavigationMatches(projection, snapshot)
    && projection.run_status_key === "completed" && projection.task_activity_state === "idle"
    && projection.busy === false && projection.navigation_loading === false
    && projection.post_run_refresh_pending === false && projection.can_cancel_run === false
    && isDeepStrictEqual(surface.main.primary_rows, snapshot.primary_rows)
    && surface.main.prompt_value === draft && surfaceHasNoErrors(surface);
}

function currentTerminalMainMatches(surface, draft) {
  return mainMatches(surface, sideSessionMainSnapshot(surface?.projection), draft);
}

export function sideSessionBindingSnapshot(side) {
  return {
    owner_session_id: side?.owner_session_id ?? null,
    chat_id: side?.chat_id ?? null,
    generation: side?.generation ?? null,
    provider_profile: side?.provider_profile ?? null,
    base_url: side?.base_url ?? null,
    model: side?.model ?? null,
    context_as_of_append_position: side?.context_as_of_append_position ?? null,
    draft_revision: side?.draft_revision ?? null,
    draft_text: side?.draft_text ?? null,
    messages: (side?.messages ?? []).map(({ id, role, content }) => ({ id, role, content })),
  };
}

export function sideSessionRestored(surface, { main, binding, mainDraft }) {
  const side = surface?.projection?.side_chat;
  return typeof binding?.chat_id === "string" && binding?.owner_session_id === main?.session_id
    && mainMatches(surface, main, mainDraft)
    && isDeepStrictEqual(sideSessionBindingSnapshot(side), binding)
    && side.configured === true && side.status === "completed" && side.last_error === ""
    && side.context_scope === "owner_session" && side.context_truncated === false
    && side.can_send === true && side.can_cancel === false
    && surface.side.pane_count === 1 && surface.side.pane_visible === true
    && surface.side.owner_session_id === main.session_id
    && surface.side.prompt_value === binding.draft_text
    && isDeepStrictEqual(surface.side.messages, binding.messages)
    && surface.side.pending_count === 0;
}

export function sideSessionBetaIsolated(surface, beta) {
  const side = surface?.projection?.side_chat;
  return mainMatches(surface, beta, "") && side?.configured === false
    && side.owner_session_id === beta.session_id && side.status === "idle" && side.last_error === ""
    && side.can_send === false && side.can_cancel === false && side.draft_quote === null
    && side.chat_id === null && side.messages.length === 0
    && side.draft_text === "" && surface.side.messages.length === 0
    && surface.side.pane_count === 1 && surface.side.pane_visible === true
    && surface.side.setup_visible === true && surface.side.prompt_value === null;
}

export function sideSessionDraftSaved(surface, completedBinding, text) {
  const current = sideSessionBindingSnapshot(surface?.projection?.side_chat);
  return /^\d+$/.test(current.draft_revision ?? "") && /^\d+$/.test(completedBinding?.draft_revision ?? "")
    && BigInt(current.draft_revision) > BigInt(completedBinding.draft_revision)
    && isDeepStrictEqual(current, { ...completedBinding, draft_text: text, draft_revision: current.draft_revision })
    && surface.projection.side_chat.status === "completed" && surface.projection.side_chat.last_error === ""
    && surface.side.prompt_value === text && isDeepStrictEqual(surface.side.messages, completedBinding.messages);
}

async function waitSurface(cdp, label, accept) {
  return (await waitForProductStage({
    label, sample: () => observeSideChatQuoteSurface(cdp), accept,
    code: "side-chat-session-state", message: label,
  })).value;
}

async function createRoot({ cdp, input, provider, prompt, response, count }) {
  await trustedInsert(input, MAIN_PROMPT, prompt);
  await trustedClick(input, MAIN_SEND);
  return waitSurface(cdp, `completed independent session ${count}`, (surface) =>
    settledCompletedProviderTurn(surface.projection, { expectedPrompt: prompt, expectedResponse: response })
      && sideSessionLedgerMatches(provider.requestLedger, count) && surfaceHasNoErrors(surface));
}

async function openSidePane(cdp, input) {
  const before = await observeSideChatQuoteSurface(cdp);
  if (!before.side.pane_visible) await trustedClick(input, SHOW_SIDE);
  return waitSurface(cdp, "selected session Side Chat pane is visible", (surface) =>
    surface.side.pane_count === 1 && surface.side.pane_visible && surfaceHasNoErrors(surface));
}

export async function waitForSideSessionSelection(input, sessionId) {
  let settled;
  try {
    settled = await waitForSemanticTargetSettlement({
      input, locator: projectSessionSelectionLocator(sessionId),
      label: "exact session navigation is enabled in the rendered sidebar",
    });
  } catch (error) {
    if (error?.code !== "observation-timeout" || error?.evidence?.last_error) throw error;
    fail("navigation-readiness", error.evidence);
  }
  if (settled.value.classified.decision !== "pass") fail("navigation-owner", settled.value);
  return settled;
}

async function selectSession(cdp, input, snapshot, draft) {
  const locator = projectSessionSelectionLocator(snapshot.session_id);
  await waitForSideSessionSelection(input, snapshot.session_id);
  await trustedClick(input, locator);
  return waitSurface(cdp, "exact selected session and main history restored", (surface) =>
    mainMatches(surface, snapshot, draft));
}

async function closeProbes(state, input, commands, primaryError) {
  const outcome = { failures: [] };
  for (const [kind, resource, close] of [["input", input, "cleanup"], ["commands", commands, "remove"]]) {
    if (resource === null) continue;
    try { outcome[kind] = await resource[close](); }
    catch (error) { outcome.failures.push({ kind, message: error.message }); }
  }
  state.resources.push(outcome);
  if (primaryError === null && outcome.failures.length > 0) {
    throw new DesktopE2eError("harness", "side-chat-session-cleanup", "Side Chat session probes did not settle", outcome);
  }
}

export function createSideChatSessionScenario() {
  const state = { provider: null, acceptedLedger: null, quiesceOutcome: null, resources: [] };
  return Object.freeze({
    id: "side-chat.session", productOracle: "pass", manualGate: "not_required", databaseRequired: true,
    requestGracefulExit,
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({ script: createSideChatSessionProviderScript() });
      await prepareDesktopFixture({
        context, sink, phase, owner: OWNER,
        configText: providerRestartFixtureConfig(state.provider.baseUrl),
        sentinelName: "E2E_SIDE_CHAT_SESSION.txt",
        sentinelText: "Session-bound Side Chat isolation fixture.\n",
      });
      await sink.record("side-chat-session-provider-started", state.provider.resourceObservation(), { phase, owner: OWNER });
    },
    async execute({ context, driver: initialCdp, host, sink }) {
      const provider = state.provider;
      let cdp = initialCdp;
      let input = null;
      let commands = null;
      let primaryError = null;
      try {
        await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: OWNER, screenshotStem: "side-session-ready" });
        if (provider.requestLedger.length !== 0) fail("implicit-provider-request", provider.requestLedger);
        input = new WebviewInput(cdp, { probeId: "side-session-input" });
        await input.installProbe();
        await createRoot({ cdp, input, provider, prompt: ALPHA_PROMPT, response: ALPHA_RESPONSE, count: 1 });
        const alphaSurface = await waitSurface(cdp, "session A canonical terminal navigation settled", (surface) =>
          currentTerminalMainMatches(surface, ""));
        const alpha = sideSessionMainSnapshot(alphaSurface.projection);
        const projectId = selectedNavigationIdentity(alphaSurface.projection).project_id;
        const hover = projectRowHoverLocator(projectId);
        const hoverStart = (await input.snapshotProbe()).sequence;
        await input.hover(hover);
        assertTrustedProbeSequence(await input.snapshotProbe(hoverStart), {
          afterSequence: hoverStart, expected: [{ type: "pointermove", identity: hover.identity, buttons: 0 }],
        });
        const newSession = projectNewSessionLocator(projectId);
        const hoverReady = await waitForObservation({
          label: "project new-session action after trusted hover", timeoutMs: 2_000, pollMs: 16,
          retrySampleErrors: false, sample: () => input.observeExactTarget(newSession),
          accept: (sample) => hoveredProjectActionDecision(sample.observation, newSession).decision !== "pending",
        });
        if (hoveredProjectActionDecision(hoverReady.value.observation, newSession).decision !== "pass") {
          throw new DesktopE2eError("harness", "side-session-new-session-target", "Project action is not actionable", hoverReady.value);
        }
        await trustedClick(input, newSession);
        await waitSurface(cdp, "fresh session B owner", (surface) => surface.projection.draft_target.sessionId === null
          && surface.projection.thread_empty && surface.main.prompt_value === "" && surface.projection.navigation_loading === false);
        await createRoot({ cdp, input, provider, prompt: BETA_PROMPT, response: BETA_RESPONSE, count: 2 });
        const betaSurface = await waitSurface(cdp, "session B canonical terminal navigation settled", (surface) =>
          currentTerminalMainMatches(surface, ""));
        const beta = sideSessionMainSnapshot(betaSurface.projection);
        if (beta.session_id === alpha.session_id || selectedNavigationIdentity(betaSurface.projection).project_id !== projectId) {
          fail("distinct-project-session-owners", { alpha, beta, projectId });
        }
        await selectSession(cdp, input, alpha, "");
        const configuration = await configureSideChat({ cdp, input, providerBaseUrl: provider.baseUrl, ownerSessionId: alpha.session_id });
        await trustedInsert(input, MAIN_PROMPT, SIDE_SESSION_MAIN_DRAFT);
        const consultation = await executeCase52SideChatStage({
          cdp,
          input,
          sink,
          sessionId: alpha.session_id,
          providerProfile: "openai_responses",
          providerBaseUrl: provider.baseUrl,
          model: "e2e/scripted-responses",
          promptInput: { text: QUESTION },
          timeoutMs: 30_000,
          evidenceName: "side-session-alpha-consulted",
        });
        const completed = consultation.completedSurface;
        if (!isDeepStrictEqual(
          completed.projection.side_chat.messages.map(({ role, content }) => [role, content]),
          [["user", QUESTION], ["assistant", SIDE_RESPONSE]],
        ) || !sideSessionLedgerMatches(provider.requestLedger, 3)) {
          fail("consultation-response", { side: completed.projection.side_chat, ledger: provider.requestLedger });
        }
        const send = consultation.send;
        const commandEvidence = consultation.commandEvidence;
        const sentArgs = commandEvidence.calls[0].args;
        const contextEvidence = provider.requestLedger.at(-1).contract.owner_context;
        if (contextEvidence.owner_session_id !== alpha.session_id
          || contextEvidence.as_of_append_position !== sentArgs.expectedOwnerAppendPosition
          || !isDeepStrictEqual(contextEvidence.source_history_item_ids, alpha.primary_rows.map((row) => row.id))) {
          fail("outbound-owner-context", { alpha, expected_append: sentArgs.expectedOwnerAppendPosition, contextEvidence });
        }
        const completedScreenshot = consultation.terminal_screenshot;
        const completedBinding = sideSessionBindingSnapshot(completed.projection.side_chat);
        await trustedInsert(input, SIDE_PROMPT, SIDE_SESSION_UNSENT_DRAFT);
        const draftSaved = await waitSurface(cdp, "session A unsent Side draft persisted", (surface) =>
          mainMatches(surface, alpha, SIDE_SESSION_MAIN_DRAFT)
          && sideSessionDraftSaved(surface, completedBinding, SIDE_SESSION_UNSENT_DRAFT)
          && sideSessionLedgerMatches(provider.requestLedger, 3));
        const binding = sideSessionBindingSnapshot(draftSaved.projection.side_chat);
        await selectSession(cdp, input, beta, "");
        await openSidePane(cdp, input);
        const betaIsolated = await waitSurface(cdp, "session B has no A Side binding, history, or draft", (surface) =>
          sideSessionBetaIsolated(surface, beta)
          && sideSessionTerminalNavigationMatches(surface.projection, [alpha, beta]));
        const betaScreenshot = await captureScenarioScreenshot({ cdp, sink, name: "side-session-beta-isolated", owner: OWNER });
        await selectSession(cdp, input, alpha, SIDE_SESSION_MAIN_DRAFT);
        await openSidePane(cdp, input);
        const restored = await waitSurface(cdp, "session A Side binding and draft restored after B", (surface) =>
          sideSessionRestored(surface, { main: alpha, binding, mainDraft: SIDE_SESSION_MAIN_DRAFT })
          && sideSessionTerminalNavigationMatches(surface.projection, [alpha, beta]));
        const restoredScreenshot = await captureScenarioScreenshot({ cdp, sink, name: "side-session-alpha-restored", owner: OWNER });
        if (!sideSessionLedgerMatches(provider.requestLedger, 3)) fail("navigation-provider-replay", provider.requestLedger);
        await sink.record("side-chat-session-selection-completed", {
          alpha, beta, configuration, send, command_evidence: commandEvidence, owner_context: contextEvidence,
          binding, completed: completed.projection.side_chat, beta_isolated: betaIsolated.projection.side_chat,
          restored: restored.projection.side_chat,
          navigation: {
            beta_selected: sideSessionNavigationSummary(betaIsolated.projection, [alpha, beta]),
            alpha_restored: sideSessionNavigationSummary(restored.projection, [alpha, beta]),
          },
          screenshots: [completedScreenshot, betaScreenshot, restoredScreenshot],
        }, { phase: "executing", owner: OWNER });
        const firstInput = input;
        const firstCommands = commands;
        input = null;
        commands = null;
        await closeProbes(state, firstInput, firstCommands, null);
        const restarted = await host.restart({ context, scenario: this, sink, driver: cdp, phase: "executing" });
        cdp = restarted.driver;
        await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: OWNER, screenshotStem: "side-session-restarted" });
        input = new WebviewInput(cdp, { probeId: "side-session-restarted-input" });
        await input.installProbe();
        await selectSession(cdp, input, alpha, "");
        await openSidePane(cdp, input);
        let stableSince = null;
        const reopened = await waitSurface(cdp, "stable Side restoration after Desktop restart and explicit session A reopen", (surface) => {
          const pass = sideSessionRestored(surface, { main: alpha, binding, mainDraft: "" })
            && sideSessionTerminalNavigationMatches(surface.projection, [alpha, beta])
            && sideSessionLedgerMatches(provider.requestLedger, 3);
          if (!pass) { stableSince = null; return false; }
          stableSince ??= performance.now();
          return performance.now() - stableSince >= 500;
        });
        const restartScreenshot = await captureScenarioScreenshot({ cdp, sink, name: "side-session-alpha-restarted-restored", owner: OWNER });
        state.acceptedLedger = provider.requestLedger;
        await sink.record("side-chat-session-completed", {
          restart: restarted.restart, alpha, beta, binding,
          restored: sideSessionBindingSnapshot(reopened.projection.side_chat),
          navigation: sideSessionNavigationSummary(reopened.projection, [alpha, beta]),
          provider_ledger: state.acceptedLedger, screenshot: restartScreenshot,
          main_draft_restart_contract: "frontend-local main draft is not persisted; Side draft is persisted",
        }, { phase: "executing", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        await closeProbes(state, input, commands, primaryError);
      }
    },
    async quiesce({ inputs }) {
      state.quiesceOutcome ??= await quiesceProviderResource({ provider: state.provider, acceptedLedger: state.acceptedLedger, inputs });
      return structuredClone(state.quiesceOutcome);
    },
    async cleanup() {
      return {
        input: state.quiesceOutcome?.input === "pass" && state.resources.every((outcome) => outcome.failures.length === 0) ? "pass" : "fail",
        resources: [{ kind: "side-chat-session-verification", quiesce: state.quiesceOutcome, probes: state.resources }],
      };
    },
  });
}

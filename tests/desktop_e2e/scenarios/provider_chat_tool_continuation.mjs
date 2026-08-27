import { isDeepStrictEqual } from "node:util";

import { waitForObservation } from "../core/deadline.mjs";
import { canonicalU64, canonicalUlid } from "../core/canonical_identity.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import {
  DesktopCommandProbe,
  assertExactDesktopCommandSequence,
} from "../drivers/desktop_command_probe.mjs";
import {
  SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_MAX_RESPONSES,
  SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT,
  SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_RESPONSE,
  SCRIPTED_PROVIDER_MODEL_ID,
  createChatToolContinuationProviderScript,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";
import {
  WebviewInput,
  assertTrustedProbeSequence,
  assertTrustedTextInsertion,
} from "../drivers/webview_input.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import {
  classifyAcquiredObservationFailure,
  quiesceProviderResource,
} from "./provider_restart.mjs";
import { captureScenarioScreenshot, selectedNavigationIdentity } from "./observations.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";

const OWNER = "scenario:provider.chat-tool-continuation";
const CONTROL_TOKENS = Object.freeze(["<|im_start|>", "<|im_end|>"]);
const CHAT_ROLES = Object.freeze(["chat_tool_initial", "chat_continuation"]);
const TOOL_PROGRESS = "ツール: 1件開始 / 1件完了 / 0件拒否 / 0件キャンセル / 0件失敗";

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

export function providerChatToolContinuationFixtureConfig(baseUrl) {
  return `[model]
base_url = ${JSON.stringify(baseUrl)}
model = ${JSON.stringify(SCRIPTED_PROVIDER_MODEL_ID)}
provider_profile = "openai_compatible"
connect_timeout_ms = 1000
request_timeout_ms = 30000
max_retries = 0
context_window = 65536
supports_tools = true
supports_images = false
parallel_tool_calls = false

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

function sha256Identity(value) {
  return typeof value === "string" && /^[a-f0-9]{64}$/u.test(value);
}

function exactRoleEvidence(row, index) {
  const evidence = row?.contract?.role_evidence;
  if (index === 0) {
    return evidence?.message_count === 2
      && isDeepStrictEqual(evidence.message_roles, ["system", "user"])
      && evidence.system_content_non_empty === true
      && sha256Identity(evidence.system_content_sha256)
      && evidence.user_prompt_matches === true
      && sha256Identity(evidence.user_content_sha256)
      && evidence.assistant_content_absent === false
      && evidence.current_time_call_matches === false
      && evidence.tool_output_shape_matches === false
      && evidence.tool_output_size_bytes === null
      && evidence.tool_output_sha256 === null;
  }
  return evidence?.message_count === 4
    && isDeepStrictEqual(evidence.message_roles, ["system", "user", "assistant", "tool"])
    && evidence.system_content_non_empty === true
    && sha256Identity(evidence.system_content_sha256)
    && evidence.user_prompt_matches === true
    && sha256Identity(evidence.user_content_sha256)
    && evidence.assistant_content_absent === true
    && evidence.current_time_call_matches === true
    && evidence.tool_output_shape_matches === true
    && Number.isSafeInteger(evidence.tool_output_size_bytes)
    && evidence.tool_output_size_bytes > 0
    && evidence.tool_output_size_bytes <= 512
    && sha256Identity(evidence.tool_output_sha256);
}

function acceptedChatRow(row, index, phase) {
  const contract = row?.contract;
  const tools = contract?.tools;
  return row?.route === "chat_completions"
    && row.method === "POST"
    && row.pathname === "/v1/chat/completions"
    && row.query_present === false
    && row.response_phase === phase
    && row.response_status === (phase === "completed" ? 200 : null)
    && contract?.pass === true
    && contract.role === CHAT_ROLES[index]
    && contract.model_matches === true
    && contract.top_level_keys_match === true
    && contract.stream_true === true
    && contract.include_usage_true === true
    && contract.n_one === true
    && contract.client_generation_fields_absent === true
    && Array.isArray(contract.client_generation_fields_present)
    && contract.client_generation_fields_present.length === 0
    && contract.max_tokens_absent === true
    && contract.parallel_tool_calls_false === true
    && tools?.pass === true
    && tools.unique_tool_names === true
    && tools.current_time_present === true
    && tools.current_time_schema_matches === true
    && exactRoleEvidence(row, index);
}

export function exactChatToolContinuationLedger(ledger, phases) {
  return Array.isArray(ledger)
    && Array.isArray(phases)
    && phases.length === SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_MAX_RESPONSES
    && ledger.length === SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_MAX_RESPONSES
    && ledger.every((row, index) => acceptedChatRow(row, index, phases[index]))
    && ledger[0].contract.role_evidence.system_content_sha256
      === ledger[1].contract.role_evidence.system_content_sha256
    && ledger[0].contract.role_evidence.user_content_sha256
      === ledger[1].contract.role_evidence.user_content_sha256;
}

function impossibleLedgerPrefix(ledger) {
  if (!Array.isArray(ledger) || ledger.length > SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_MAX_RESPONSES) {
    return true;
  }
  return ledger.some((row, index) => row?.route !== "chat_completions"
    || row?.method !== "POST"
    || row?.pathname !== "/v1/chat/completions"
    || row?.query_present !== false
    || row?.contract?.pass === false
    || (typeof row?.contract?.role === "string" && row.contract.role !== CHAT_ROLES[index])
    || ["rejected", "peer_closed"].includes(row?.response_phase)
    || (row?.response_status !== null && row?.response_status !== 200));
}

function rowsOfKind(projection, kind) {
  const rows = Array.isArray(projection?.transcript_rows) ? projection.transcript_rows : [];
  return rows.filter((row) => row?.row_kind === kind);
}

function controlTokenLeaks(surface) {
  const projectionRows = Array.isArray(surface?.projection?.transcript_rows)
    ? surface.projection.transcript_rows
    : [];
  const leaks = projectionRows.flatMap((row, rowIndex) => {
    if (typeof row?.body !== "string") return [];
    const markers = CONTROL_TOKENS.filter((marker) => row.body.includes(marker));
    return markers.length === 0 ? [] : [{ owner: "desktop_state", row_index: rowIndex, markers }];
  });
  if (typeof surface?.thread_text === "string") {
    const markers = CONTROL_TOKENS.filter((marker) => surface.thread_text.includes(marker));
    if (markers.length > 0) leaks.push({ owner: "dom", markers });
  }
  return leaks;
}

function currentTimeFromToolStatus(projection) {
  if (typeof projection?.tool_status_text !== "string") return null;
  const match = projection.tool_status_text.match(
    /^ツール:\r?\n- Current time \[completed\] local: ([^\r\n]+)\r?\nutc: ([^\r\n]+)\r?\ntimezone: ([^\r\n]+)\r?\nunix_ms: ([0-9]+)$/u,
  );
  if (match === null
    || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}[+-]\d{2}:\d{2}$/u.test(match[1])
    || !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/u.test(match[2])
    || !/^[+-]\d{2}:\d{2}$/u.test(match[3])) return null;
  return { local: match[1], utc: match[2], timezone: match[3], unixMs: match[4] };
}

function currentTimeFromCompletedSummary(value) {
  if (typeof value !== "string") return null;
  const terminalRows = Array.from(value.matchAll(/^- \[(完了|失敗|拒否|キャンセル)\] ([^\r\n]+)$/gmu));
  if (terminalRows.length !== 1
    || terminalRows[0][1] !== "完了"
    || terminalRows[0][2] !== "Current time") return null;
  const matches = Array.from(value.matchAll(
    /^- \[完了\] Current time\r?\n {2}出力: local: ([^ ]+) utc: ([^ ]+) timezone: ([^ ]+) unix_ms: ([0-9]+)$/gmu,
  ));
  return matches.length === 1
    ? { local: matches[0][1], utc: matches[0][2], timezone: matches[0][3], unixMs: matches[0][4] }
    : null;
}

function exactCurrentTimeProjection(projection) {
  return currentTimeFromToolStatus(projection) !== null
    && projection?.latest_tool_summary === "ツール:"
    && typeof projection?.progress_text === "string"
    && projection.progress_text.includes(TOOL_PROGRESS);
}

function blockingSurfaceFailure(surface) {
  return surface?.visible_fatal_count > 0
    || surface?.visible_recoverable_error_count > 0
    || surface?.visible_validation_error_count > 0
    || surface?.visible_transcript_error_count > 0
    || surface?.projection?.startup?.status === "failed"
    || ["failed", "cancelled", "incomplete"].includes(surface?.projection?.run_status_key);
}

function exactTurnOwner(projection, kind) {
  const expected = projection?.run_target?.expectedState;
  if (expected?.kind !== kind
    || !canonicalUlid(projection?.run_target?.sessionId)
    || projection.run_target.sessionId !== projection?.draft_target?.sessionId
    || !canonicalU64(expected.admissionRevision)) return null;
  const turnId = kind === "turn" ? expected.turnId : expected.latestTurnId;
  return canonicalUlid(turnId)
    ? { sessionId: projection.run_target.sessionId, turnId, admissionRevision: expected.admissionRevision }
    : null;
}

function workSummaryTurnId(identity) {
  if (typeof identity !== "string") return null;
  const match = identity.match(/^turn:([0-9A-HJKMNP-TV-Z]{26}):work-summary$/u);
  return match !== null && canonicalUlid(match[1]) ? match[1] : null;
}

export function chatToolContinuationHeldFailures(sample) {
  const surface = sample?.surface;
  const projection = surface?.projection;
  const owner = exactTurnOwner(projection, "turn");
  const users = rowsOfKind(projection, "user");
  const assistants = rowsOfKind(projection, "assistant");
  const errors = rowsOfKind(projection, "error");
  const runningSummaries = rowsOfKind(projection, "work_summary_running");
  const completedSummaries = rowsOfKind(projection, "work_summary_completed");
  const failures = [];
  if (!exactChatToolContinuationLedger(sample?.ledger, ["completed", "held"])) {
    failures.push("chat-continuation-request-not-exactly-held");
  }
  if (projection?.run_status_key !== "running"
    || projection?.task_activity_state !== "running"
    || projection?.busy !== true
    || projection?.agent_tree_active !== false
    || owner === null) failures.push("held-run-owner-not-active");
  if (!isDeepStrictEqual(users.map((row) => row.body), [SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT])
    || !canonicalUlid(users[0]?.stable_history_identity)) failures.push("held-user-not-canonical");
  if (assistants.length !== 0 || surface?.assistants?.length !== 0) {
    failures.push("held-assistant-row-present");
  }
  if (errors.length !== 0 || surface?.errors?.length !== 0 || blockingSurfaceFailure(surface)) {
    failures.push("held-error-present");
  }
  if (runningSummaries.length !== 1
    || completedSummaries.length !== 0
    || workSummaryTurnId(runningSummaries[0]?.stable_history_identity) !== owner?.turnId
    || surface?.running_summaries?.length !== 1
    || surface?.completed_summaries?.length !== 0) failures.push("held-work-summary-not-running");
  if (controlTokenLeaks(surface).length > 0) failures.push("chat-template-control-token-visible");
  return [...new Set(failures)];
}

function terminalSettled(surface) {
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
    && projection?.confirmation == null
    && projection?.draft_prompt === ""
    && projection?.composer_submit_mode === "new_request"
    && projection?.can_submit === true
    && exactTurnOwner(projection, "idle") !== null;
}

export function chatToolContinuationTerminalFailures(sample, heldTime = null) {
  const surface = sample?.surface;
  const projection = surface?.projection;
  const owner = exactTurnOwner(projection, "idle");
  const rows = Array.isArray(projection?.transcript_rows) ? projection.transcript_rows : [];
  const users = rowsOfKind(projection, "user");
  const assistants = rowsOfKind(projection, "assistant");
  const errors = rowsOfKind(projection, "error");
  const runningSummaries = rowsOfKind(projection, "work_summary_running");
  const completedSummaries = rowsOfKind(projection, "work_summary_completed");
  const toolTime = currentTimeFromToolStatus(projection);
  const summaryTime = currentTimeFromCompletedSummary(completedSummaries[0]?.body);
  const failures = [];
  if (!exactChatToolContinuationLedger(sample?.ledger, ["completed", "completed"])) {
    failures.push("chat-tool-ledger-not-terminal");
  }
  if (!terminalSettled(surface) || blockingSurfaceFailure(surface)) failures.push("terminal-surface-not-settled");
  if (!isDeepStrictEqual(rows.map((row) => row?.row_kind), [
    "user",
    "work_summary_completed",
    "assistant",
  ])) failures.push("terminal-history-order-mismatch");
  if (!isDeepStrictEqual(users.map((row) => row.body), [SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT])
    || !canonicalUlid(users[0]?.stable_history_identity)) failures.push("terminal-user-not-canonical");
  if (!isDeepStrictEqual(assistants.map((row) => row.body), [SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_RESPONSE])
    || (assistants[0]?.stable_history_identity ?? null) !== null) {
    failures.push("terminal-assistant-not-exact");
  }
  if (errors.length !== 0 || surface?.errors?.length !== 0) failures.push("terminal-error-present");
  if (runningSummaries.length !== 0
    || completedSummaries.length !== 1
    || workSummaryTurnId(completedSummaries[0]?.stable_history_identity) !== owner?.turnId) {
    failures.push("terminal-work-summary-not-canonical");
  }
  if (toolTime === null
    || summaryTime === null
    || !isDeepStrictEqual(summaryTime, toolTime)
    || (heldTime !== null && !isDeepStrictEqual(toolTime, heldTime))) {
    failures.push("terminal-current-time-evidence-mismatch");
  }
  if (!exactCurrentTimeProjection(projection)) failures.push("terminal-tool-projection-mismatch");
  if (controlTokenLeaks(surface).length > 0) failures.push("chat-template-control-token-visible");

  const expectedUserIdentity = users[0]?.stable_history_identity ?? null;
  const expectedSummaryIdentity = completedSummaries[0]?.stable_history_identity ?? null;
  if (surface?.thread_count !== 1
    || surface?.users?.length !== 1
    || surface.users[0].visible !== true
    || surface.users[0].text !== SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT
    || surface.users[0].history_identity !== expectedUserIdentity
    || surface?.assistants?.length !== 1
    || surface.assistants[0].visible !== true
    || surface.assistants[0].text !== SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_RESPONSE
    || surface.assistants[0].history_identity !== null
    || surface?.completed_summaries?.length !== 1
    || surface.completed_summaries[0].visible !== true
    || surface.completed_summaries[0].history_identity !== expectedSummaryIdentity
    || surface?.running_summaries?.length !== 0) failures.push("terminal-dom-history-mismatch");
  if (surface?.prompt?.count !== 1
    || surface.prompt.value !== ""
    || surface.prompt.visible !== true
    || surface.prompt.enabled !== true
    || surface?.send?.count !== 1
    || surface.send.visible !== true
    || surface.send.enabled !== false
    || surface.send.title !== "依頼文を入力してください"
    || surface.send.aria_label !== "依頼文を入力してください") failures.push("terminal-composer-dom-invalid");
  return [...new Set(failures)];
}

async function observeChatToolContinuationSurface(cdp) {
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
    const rows = (selector) => Array.from(document.querySelectorAll(selector)).map((row) => ({
      history_identity: row.getAttribute('data-history-identity'),
      text: (row.querySelector('.markdown-body')?.innerText ?? '').trim(),
      visible: visible(row),
    }));
    const summaryRows = (selector) => Array.from(document.querySelectorAll(selector)).map((row) => ({
      history_identity: row.getAttribute('data-history-identity'),
      text: (row.textContent ?? '').trim(),
      visible: visible(row),
    }));
    const thread = document.querySelector('main.conversation #thread');
    const prompt = document.querySelector('section.composer textarea#prompt');
    const send = document.querySelector('section.composer button[data-action="send"]');
    return {
      projection,
      thread_count: document.querySelectorAll('main.conversation #thread').length,
      thread_text: thread instanceof HTMLElement ? thread.innerText : null,
      users: rows('main.conversation #thread article.message.user'),
      assistants: rows('main.conversation #thread article.message.assistant'),
      errors: rows('main.conversation #thread article.message.error'),
      running_summaries: summaryRows('main.conversation #thread article.message.work-summary.work_summary_running'),
      completed_summaries: summaryRows('main.conversation #thread article.message.work-summary.work_summary_completed'),
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
      visible_validation_error_count: Array.from(document.querySelectorAll('.validation.error')).filter(visible).length,
      visible_transcript_error_count: Array.from(document.querySelectorAll(
        'main.conversation #thread article.message.error'
      )).filter(visible).length,
    };
  })()`);
}

async function waitForProductStage({ label, sample, decide, code, message }) {
  let decision = "pending";
  let observed;
  try {
    observed = await waitForObservation({
      label,
      timeoutMs: 20_000,
      pollMs: 50,
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

async function trustedPromptInput(input) {
  const focus = await trustedClick(input, PROMPT);
  const start = (await input.snapshotProbe()).sequence;
  const insertion = await input.insertText(PROMPT, SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT);
  const probe = assertTrustedTextInsertion(await input.snapshotProbe(start), {
    afterSequence: start,
    identity: PROMPT.identity,
    text: SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT,
  });
  return { focus, insertion, probe };
}

async function settleResources(state, input, commands, primaryError) {
  const outcome = { input: null, command_probe: null, failures: [] };
  try { outcome.input = await input.cleanup(); }
  catch (error) { outcome.failures.push({ owner: "webview-input", error: errorObservation(error) }); }
  try { outcome.command_probe = await commands.remove(); }
  catch (error) { outcome.failures.push({ owner: "desktop-command-probe", error: errorObservation(error) }); }
  state.resourceOutcome = outcome;
  if (outcome.failures.length > 0 && primaryError === null) {
    throw new DesktopE2eError(
      "harness",
      "provider-chat-tool-continuation-resource-cleanup-failed",
      "Chat tool-continuation input and command probes did not settle",
      outcome,
    );
  }
}

export function createProviderChatToolContinuationScenario() {
  const state = {
    provider: null,
    acceptedLedger: null,
    quiesceOutcome: null,
    resourceOutcome: null,
  };
  return Object.freeze({
    id: "provider.chat-tool-continuation",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    requestGracefulExit,
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({
        expectedPrompt: SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT,
        responseBehavior: "hold_until_release",
        script: createChatToolContinuationProviderScript(),
      });
      await prepareDesktopFixture({
        context,
        sink,
        phase,
        owner: OWNER,
        configText: providerChatToolContinuationFixtureConfig(state.provider.baseUrl),
        sentinelName: "E2E_PROVIDER_CHAT_TOOL_CONTINUATION.txt",
        sentinelText: "moyAI Desktop E2E Chat tool-continuation fixture.\n",
      });
      await sink.record("scripted-provider-started", state.provider.resourceObservation(), { phase, owner: OWNER });
    },
    async execute({ context, driver: cdp, sink }) {
      const provider = state.provider;
      if (provider === null) throw new Error("scripted Chat provider was not prepared");
      await acquireInteractiveShell({ context, driver: cdp, sink }, {
        evidenceOwner: OWNER,
        screenshotStem: "provider-chat-tool-shell-ready",
      });
      if (provider.requestLedger.length !== 0) {
        throw productFailure(
          "provider-chat-tool-cold-start-request",
          "Desktop contacted the Chat provider before trusted Send",
          { ledger: provider.requestLedger },
        );
      }

      const input = new WebviewInput(cdp, { probeId: "provider-chat-tool-continuation" });
      const commands = new DesktopCommandProbe(cdp, {
        probeId: "provider-chat-tool-continuation-commands",
        commands: ["submit_prompt", "cancel_run"],
      });
      let primaryError = null;
      try {
        await input.installProbe();
        await commands.install();
        const typed = await trustedPromptInput(input);
        const ready = await observeChatToolContinuationSurface(cdp);
        if (ready.prompt.value !== SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT) {
          throw productFailure(
            "provider-chat-tool-prompt-drift",
            "trusted text insertion did not produce the exact Chat tool prompt",
            { expected: SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT, surface: ready },
          );
        }
        const expectedCommand = {
          command: "submit_prompt",
          args: {
            text: SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT,
            expectedTarget: structuredClone(ready.projection.draft_target),
            expectedRunTarget: structuredClone(ready.projection.run_target),
          },
        };
        const commandStart = (await commands.snapshot()).sequence;
        const send = await trustedClick(input, SEND);
        const commandObservation = await waitForObservation({
          label: "provider.chat-tool-continuation exact submit command",
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

        const held = await waitForProductStage({
          label: "held Chat Completions tool continuation",
          sample: async () => ({
            surface: await observeChatToolContinuationSurface(cdp),
            ledger: provider.requestLedger,
          }),
          decide: (sample) => {
            if (impossibleLedgerPrefix(sample?.ledger)
              || blockingSurfaceFailure(sample?.surface)
              || controlTokenLeaks(sample?.surface).length > 0
              || sample?.surface?.assistants?.length > 0
              || rowsOfKind(sample?.surface?.projection, "assistant").length > 0) return "fail";
            return chatToolContinuationHeldFailures(sample).length === 0 ? "pass" : "pending";
          },
          code: "provider-chat-tool-held-contract-mismatch",
          message: "the Chat continuation did not remain held behind a clean tool-only projection",
        });
        const heldCommand = assertExactDesktopCommandSequence(await commands.snapshot(commandStart), {
          afterSequence: commandStart,
          expected: [expectedCommand],
        });
        const heldTime = currentTimeFromToolStatus(held.value.surface.projection);
        const heldScreenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "provider-chat-tool-continuation-held",
          owner: OWNER,
        });
        const release = provider.releaseScriptRole("chat_continuation");

        const terminal = await waitForProductStage({
          label: "terminal Chat Completions tool continuation",
          sample: async () => ({
            surface: await observeChatToolContinuationSurface(cdp),
            ledger: provider.requestLedger,
          }),
          decide: (sample) => {
            if (impossibleLedgerPrefix(sample?.ledger)
              || blockingSurfaceFailure(sample?.surface)
              || controlTokenLeaks(sample?.surface).length > 0) return "fail";
            return chatToolContinuationTerminalFailures(sample, heldTime).length === 0
              ? "pass"
              : "pending";
          },
          code: "provider-chat-tool-terminal-contract-mismatch",
          message: "the released Chat continuation did not settle to one exact canonical assistant response",
        });
        const finalCommand = assertExactDesktopCommandSequence(await commands.snapshot(commandStart), {
          afterSequence: commandStart,
          expected: [expectedCommand],
        });
        const terminalScreenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "provider-chat-tool-continuation-completed",
          owner: OWNER,
        });
        state.acceptedLedger = structuredClone(terminal.value.ledger);
        await sink.record("provider-chat-tool-continuation-completed", {
          input_kind: "browser_trusted",
          typed,
          send,
          expected_command: expectedCommand,
          command: exactCommand,
          held_command: heldCommand,
          final_command: finalCommand,
          held: {
            ledger: held.value.ledger,
            time: heldTime,
            projection_revision: held.value.surface.projection.projection_revision,
            selected_navigation: selectedNavigationIdentity(held.value.surface.projection),
            screenshot: heldScreenshot,
          },
          release,
          terminal: {
            ledger: state.acceptedLedger,
            projection_revision: terminal.value.surface.projection.projection_revision,
            selected_navigation: selectedNavigationIdentity(terminal.value.surface.projection),
            stable_history_identities: terminal.value.surface.projection.transcript_rows.map((row) => ({
              kind: row.row_kind,
              identity: row.stable_history_identity,
            })),
            screenshot: terminalScreenshot,
          },
          provider_resource: provider.resourceObservation(),
        }, { phase: "executing", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        await settleResources(state, input, commands, primaryError);
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
          kind: "provider-chat-tool-continuation-verification",
          quiesced,
          quiesce_input: state.quiesceOutcome?.input ?? null,
          interaction_resources: state.resourceOutcome,
        }],
      };
    },
  });
}

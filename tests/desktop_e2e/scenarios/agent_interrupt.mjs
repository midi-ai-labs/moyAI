import { waitForObservation } from "../core/deadline.mjs";
import {
  canonicalU64,
  canonicalUlid,
  canonicalWorkspace,
} from "../core/canonical_identity.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { WebviewInput, assertTrustedProbeSequence } from "../drivers/webview_input.mjs";
import {
  DesktopCommandProbe,
  assertExactDesktopCommandSequence,
} from "../drivers/desktop_command_probe.mjs";
import {
  SCRIPTED_PROVIDER_AGENT_INTERRUPT_MESSAGE,
  SCRIPTED_PROVIDER_AGENT_INTERRUPT_ROOT_RESPONSE,
  SCRIPTED_PROVIDER_AGENT_INTERRUPT_TASK_NAME,
  SCRIPTED_PROVIDER_MODEL_ID,
  createAgentInterruptProviderScript,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import {
  classifyAcquiredObservationFailure,
  quiesceProviderResource,
} from "./provider_restart.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import {
  captureScenarioScreenshot,
  invokeDesktopCommandOutcome,
} from "./observations.mjs";

const OWNER = "scenario:agent.interrupt";
export const AGENT_INTERRUPT_PROMPT = "delegate exact child interrupt";
export const AGENT_INTERRUPT_PATH = `/root/${SCRIPTED_PROVIDER_AGENT_INTERRUPT_TASK_NAME}`;

const PROMPT = Object.freeze({
  selector: "section.composer textarea#prompt",
  identity: { tag: "TEXTAREA", id: "prompt" },
});
const SEND = Object.freeze({
  selector: 'section.composer button[data-action="send"]',
  identity: { tag: "BUTTON", action: "send" },
});
const OUTPUT_AGENT_TRIGGER = Object.freeze({
  selector: 'aside.artifact-pane[data-pane-mode="output"] button.output-agent-trigger[data-action="show-agent-pane"]',
  identity: { tag: "BUTTON", action: "show-agent-pane" },
});
const AGENT_LIST_CARD = Object.freeze({
  selector: `aside#sub-agent-inspector button.sub-agent-list-card[data-action="show-agent-pane"][data-agent-path="${AGENT_INTERRUPT_PATH}"]`,
  identity: { tag: "BUTTON", action: "show-agent-pane" },
});
const INTERRUPT = Object.freeze({
  selector: `aside#sub-agent-inspector section.agent-execution[data-agent-path="${AGENT_INTERRUPT_PATH}"] button.agent-interrupt[data-action="interrupt-agent"][data-agent-path="${AGENT_INTERRUPT_PATH}"]`,
  identity: { tag: "BUTTON", action: "interrupt-agent" },
});

function sameValue(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

function exactKeys(value, expected) {
  return value !== null
    && typeof value === "object"
    && !Array.isArray(value)
    && sameValue(Object.keys(value).sort(), [...expected].sort());
}

export function exactAgentInterruptTarget(target, expected = undefined) {
  const exact = exactKeys(target, [
    "admissionRevision",
    "agentPath",
    "childSessionId",
    "expectedTurnId",
    "rootSessionId",
    "workspacePath",
  ])
    && canonicalWorkspace(target.workspacePath)
    && canonicalUlid(target.rootSessionId)
    && target.agentPath === AGENT_INTERRUPT_PATH
    && canonicalUlid(target.childSessionId)
    && canonicalUlid(target.expectedTurnId)
    && canonicalU64(target.admissionRevision);
  if (!exact || expected === undefined) return exact;
  return sameValue(target, expected);
}

function exactIdleRunExpectedState(value) {
  return exactKeys(value, ["admissionRevision", "kind", "latestTurnId"])
    && value.kind === "idle"
    && canonicalUlid(value.latestTurnId)
    && canonicalU64(value.admissionRevision);
}

function responseRows(ledger) {
  return Array.isArray(ledger) ? ledger.filter((row) => row?.route === "responses") : [];
}

function acceptedRole(rows, role, phase, status) {
  const matches = rows.filter((row) => row?.contract?.role === role);
  return matches.length === 1
    && matches[0].method === "POST"
    && matches[0].pathname === "/v1/responses"
    && matches[0].query_present === false
    && matches[0].contract.pass === true
    && matches[0].response_phase === phase
    && matches[0].response_status === status;
}

export function exactAgentInterruptHeldLedger(ledger) {
  const rows = responseRows(ledger);
  return Array.isArray(ledger)
    && ledger.length === 3
    && rows.length === 3
    && acceptedRole(rows, "root_initial", "completed", 200)
    && acceptedRole(rows, "root_continuation", "completed", 200)
    && acceptedRole(rows, "child_held", "held", null);
}

export function exactAgentInterruptTerminalLedger(ledger) {
  const rows = responseRows(ledger);
  return Array.isArray(ledger)
    && ledger.length === 3
    && rows.length === 3
    && acceptedRole(rows, "root_initial", "completed", 200)
    && acceptedRole(rows, "root_continuation", "completed", 200)
    && acceptedRole(rows, "child_held", "peer_closed", null);
}

function sessionRowFor(projection, sessionId) {
  return [
    ...(Array.isArray(projection?.session_rows) ? projection.session_rows : []),
    ...(Array.isArray(projection?.chat_session_rows) ? projection.chat_session_rows : []),
  ].find((row) => row?.session_id === sessionId) ?? null;
}

function rowHasNoActiveTurn(row) {
  return row !== null && (row.active_turn_id === undefined || row.active_turn_id === null);
}

function rootCoreHistory(projection) {
  return (Array.isArray(projection?.transcript_rows) ? projection.transcript_rows : [])
    .filter((row) => ["user", "assistant", "work_summary_completed"].includes(row?.row_kind))
    .map((row) => ({
      row_kind: row.row_kind,
      stable_history_identity: row.stable_history_identity ?? null,
      title: row.title,
      body: row.body,
    }));
}

function exactCompletedRootHistory(projection) {
  const history = rootCoreHistory(projection);
  const durableIdentities = [history[0]?.stable_history_identity, history[1]?.stable_history_identity];
  return history.length === 3
    && sameValue(history.map((row) => row.row_kind), [
      "user",
      "work_summary_completed",
      "assistant",
    ])
    && history[0].body === AGENT_INTERRUPT_PROMPT
    && history[2].body === SCRIPTED_PROVIDER_AGENT_INTERRUPT_ROOT_RESPONSE
    && durableIdentities.every((identity) => typeof identity === "string" && identity.length > 0)
    && new Set(durableIdentities).size === durableIdentities.length;
}

function captureRootOwner(projection, target) {
  return {
    workspacePath: projection.workspace_path,
    sessionId: target.rootSessionId,
    expectedState: structuredClone(projection.run_target.expectedState),
    history: rootCoreHistory(projection),
  };
}

function exactRootOwner(projection, owner) {
  const expectedState = projection?.run_target?.expectedState;
  const row = sessionRowFor(projection, owner.sessionId);
  return projection?.workspace_path === owner.workspacePath
    && projection?.run_status_key === "completed"
    && projection?.busy === false
    && exactIdleRunExpectedState(expectedState)
    && sameValue(expectedState, owner.expectedState)
    && projection?.run_target?.sessionId === owner.sessionId
    && row?.status === "completed"
    && row?.loaded_status === "idle"
    && rowHasNoActiveTurn(row)
    && row?.admission_revision === owner.expectedState.admissionRevision
    && sameValue(rootCoreHistory(projection), owner.history);
}

function childExecutionTarget(target) {
  return {
    workspacePath: target.workspacePath,
    rootSessionId: target.rootSessionId,
    agentPath: target.agentPath,
    childSessionId: target.childSessionId,
  };
}

function exactInterruptedChildExecution(execution, target) {
  const rows = Array.isArray(execution?.transcript_rows) ? execution.transcript_rows : [];
  const system = rows.filter((row) => row?.row_kind === "system");
  const cancelled = rows.filter((row) => row?.row_kind === "work_summary_cancelled");
  const total = execution?.turn_page_total;
  return execution?.workspace_path === target.workspacePath
    && execution?.root_session_id === target.rootSessionId
    && execution?.agent_path === target.agentPath
    && execution?.session_id === target.childSessionId
    && execution?.task_name === SCRIPTED_PROVIDER_AGENT_INTERRUPT_TASK_NAME
    && Number.isInteger(total)
    && total >= rows.length
    && execution?.turn_page_offset === 0
    && execution?.turn_page_end === total
    && execution?.turn_page_has_previous === false
    && system.some((row) => row.body === `Message Type: NEW_TASK\nTask name: ${AGENT_INTERRUPT_PATH}\nSender: /root\nPayload:\n${SCRIPTED_PROVIDER_AGENT_INTERRUPT_MESSAGE}`)
    && cancelled.length === 1
    && rows.every((row) => !["assistant", "work_summary_completed", "work_summary_failed", "error"].includes(row?.row_kind));
}

function agentInterruptHeldStateFailures(sample, expectedTarget = undefined) {
  const failures = [];
  const surface = sample?.surface;
  const projection = surface?.projection;
  const rows = projection?.agent_activity_rows;
  const row = Array.isArray(rows) && rows.length === 1 ? rows[0] : null;
  const target = row?.interrupt_target;
  if (!exactAgentInterruptTarget(target)) failures.push("child-interrupt-target-not-canonical");
  if (expectedTarget !== undefined && !exactAgentInterruptTarget(target, expectedTarget)) {
    failures.push("child-interrupt-target-drifted");
  }
  if (!row
    || row.agent_path !== AGENT_INTERRUPT_PATH
    || row.session_id !== target?.childSessionId
    || row.status !== "running"
    || row.active_turn_id !== target?.expectedTurnId) {
    failures.push("exact-child-not-running");
  }
  const rootExpected = projection?.run_target?.expectedState;
  const rootRow = sessionRowFor(projection, target?.rootSessionId);
  if (projection?.run_status_key !== "completed"
    || projection?.busy !== false
    || projection?.agent_tree_active !== true
    || projection?.task_activity_state !== "running"
    || projection?.async_polling_required !== true
    || projection?.can_cancel_run !== false
    || !exactIdleRunExpectedState(rootExpected)
    || projection?.run_target?.sessionId !== target?.rootSessionId
    || rootRow?.status !== "completed"
    || rootRow?.loaded_status !== "idle"
    || !rowHasNoActiveTurn(rootRow)
    || rootRow?.admission_revision !== rootExpected?.admissionRevision
    || !exactCompletedRootHistory(projection)) {
    failures.push("root-not-independently-completed");
  }
  if (surface?.history_agent_card?.count !== 1
    || surface?.history_agent_card?.status_key !== "running") {
    failures.push("semantic-child-history-missing");
  }
  if (!exactAgentInterruptHeldLedger(sample?.ledger)) failures.push("provider-three-role-flow-not-held");
  if (sample?.provider?.active_request_count !== 1
    || sample?.provider?.accepted_response_count !== 3
    || sample?.provider?.successful_response_count !== 2
    || sample?.provider?.scripted_responses_request_count !== 3) {
    failures.push("provider-child-request-not-in-flight");
  }
  if (projection?.overlay !== "none"
    || projection?.confirmation_visible !== false
    || surface?.visible_fatal_count !== 0
    || surface?.visible_recoverable_error_count !== 0) {
    failures.push("error-or-blocking-overlay-visible");
  }
  return failures;
}

export function agentInterruptInFlightFailures(sample) {
  const failures = agentInterruptHeldStateFailures(sample);
  const trigger = sample?.surface?.output_agent_trigger;
  if (trigger?.count !== 1 || trigger?.visible !== true || trigger?.enabled !== true) {
    failures.push("canonical-agent-pane-route-not-interactable");
  }
  return failures;
}

export function agentInterruptListFailures(sample, expectedTarget = undefined) {
  const failures = agentInterruptHeldStateFailures(sample, expectedTarget);
  const surface = sample?.surface;
  const card = surface?.agent_list_card;
  if (surface?.agent_inspector?.count !== 1
    || surface?.agent_inspector?.visible !== true
    || surface?.agent_inspector?.agent_path !== null
    || card?.count !== 1
    || card?.visible !== true
    || card?.enabled !== true
    || card?.status_key !== "running") {
    failures.push("exact-child-list-route-not-interactable");
  }
  return failures;
}

export function agentInterruptControlFailures(sample, expectedTarget = undefined) {
  const failures = agentInterruptHeldStateFailures(sample, expectedTarget);
  const surface = sample?.surface;
  const row = Array.isArray(surface?.projection?.agent_activity_rows)
    && surface.projection.agent_activity_rows.length === 1
    ? surface.projection.agent_activity_rows[0]
    : null;
  if (surface?.agent_inspector?.count !== 1
    || surface?.agent_inspector?.visible !== true
    || surface?.agent_inspector?.agent_path !== row?.agent_path
    || surface?.interrupt_button?.count !== 1
    || surface?.interrupt_button?.visible !== true
    || surface?.interrupt_button?.enabled !== true) {
    failures.push("exact-child-interrupt-control-not-interactable");
  }
  return failures;
}

export function agentInterruptTerminalFailures(sample, expectedTarget, rootOwner) {
  const failures = [];
  const surface = sample?.surface;
  const projection = surface?.projection;
  const rows = projection?.agent_activity_rows;
  const row = Array.isArray(rows) && rows.length === 1 ? rows[0] : null;
  if (!row
    || row.agent_path !== expectedTarget.agentPath
    || row.session_id !== expectedTarget.childSessionId
    || row.status !== "interrupted"
    || row.active_turn_id !== null
    || row.interrupt_target !== null
    || row.result_preview !== "Interrupted") {
    failures.push("durable-agent-interrupted-terminal-missing");
  }
  if (!exactRootOwner(projection, rootOwner)) failures.push("root-or-newer-turn-was-affected");
  if (projection?.agent_tree_active !== false
    || projection?.task_activity_state !== "idle"
    || projection?.busy !== false
    || projection?.post_run_refresh_pending !== false
    || projection?.background_mutation_pending !== false
    || projection?.async_polling_required !== false
    || !Array.isArray(projection?.pending_async_operations)
    || projection.pending_async_operations.length !== 0
    || projection?.navigation_loading !== false) {
    failures.push("agent-tree-owner-not-idle");
  }
  if (surface?.interrupt_button?.count !== 0
    || surface?.history_agent_card?.count !== 1
    || surface?.history_agent_card?.status_key !== "interrupted"
    || surface?.agent_inspector?.count !== 1
    || surface?.agent_inspector?.visible !== true
    || surface?.agent_inspector?.agent_path !== expectedTarget.agentPath
    || surface?.agent_inspector?.status_text !== "中断") {
    failures.push("interrupted-child-not-visible-or-admission-not-cleared");
  }
  if (sample?.child_execution_outcome?.ok !== true
    || !exactInterruptedChildExecution(sample.child_execution_outcome.value, expectedTarget)) {
    failures.push("durable-child-cancelled-history-missing");
  }
  if (!exactAgentInterruptTerminalLedger(sample?.ledger)) failures.push("provider-request-replayed-or-child-completed");
  if (sample?.provider?.active_request_count !== 0
    || sample?.provider?.accepted_response_count !== 3
    || sample?.provider?.successful_response_count !== 2
    || sample?.provider?.scripted_responses_request_count !== 3) {
    failures.push("provider-resource-not-settled");
  }
  if (projection?.overlay !== "none"
    || projection?.confirmation_visible !== false
    || surface?.visible_dialog_count !== 0
    || surface?.visible_modal_backdrop_count !== 0
    || surface?.visible_fatal_count !== 0
    || surface?.visible_recoverable_error_count !== 0) {
    failures.push("error-or-blocking-overlay-visible");
  }
  return failures;
}

export function agentInterruptFixtureConfig(baseUrl) {
  return `[model]
base_url = ${JSON.stringify(baseUrl)}
model = ${JSON.stringify(SCRIPTED_PROVIDER_MODEL_ID)}
provider_metadata_mode = "openai_compatible_only"
provider_api_mode = "responses"
reasoning_summary = "none"
connect_timeout_ms = 1000
request_timeout_ms = 120000
max_retries = 0
context_window = 65536
max_output_tokens = 1024
supports_tools = true
supports_reasoning = false
supports_images = false
parallel_tool_calls = false

[model.extra_body_json]

[permissions]
access_mode = "full_access"

[multi_agent]
enabled = true
mode = "explicit_request_only"
max_concurrent_agents = 2
max_concurrent_model_requests = 2

[docling]
enabled = false

[mcp]
enabled = false
`;
}

async function observeAgentInterruptSurface(cdp) {
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
    const path = ${JSON.stringify(AGENT_INTERRUPT_PATH)};
    const interruptButtons = Array.from(document.querySelectorAll(
      'aside#sub-agent-inspector section.agent-execution button.agent-interrupt[data-action="interrupt-agent"]'
    )).filter((button) => button instanceof HTMLElement
      && button.dataset.agentPath === path
      && button.closest('section.agent-execution')?.dataset.agentPath === path);
    const interrupt = interruptButtons.length === 1 ? interruptButtons[0] : null;
    const historyCards = Array.from(document.querySelectorAll(
      'article.message.work-summary button.work-summary-agent-card[data-action="show-agent-pane"]'
    )).filter((button) => button instanceof HTMLElement && button.dataset.agentPath === path);
    const historyCard = historyCards.length === 1 ? historyCards[0] : null;
    const outputTriggers = Array.from(document.querySelectorAll(
      'aside.artifact-pane[data-pane-mode="output"] button.output-agent-trigger[data-action="show-agent-pane"]'
    ));
    const outputTrigger = outputTriggers.length === 1 ? outputTriggers[0] : null;
    const listCards = Array.from(document.querySelectorAll(
      'aside#sub-agent-inspector button.sub-agent-list-card[data-action="show-agent-pane"]'
    )).filter((button) => button instanceof HTMLElement && button.dataset.agentPath === path);
    const listCard = listCards.length === 1 ? listCards[0] : null;
    const inspectors = Array.from(document.querySelectorAll(
      'aside#sub-agent-inspector[data-pane-mode="sub-agents"]'
    ));
    const inspector = inspectors.length === 1 ? inspectors[0] : null;
    const executions = inspector
      ? Array.from(inspector.querySelectorAll('section.agent-execution'))
        .filter((candidate) => candidate instanceof HTMLElement && candidate.dataset.agentPath === path)
      : [];
    const execution = executions.length === 1 ? executions[0] : null;
    const prompt = document.querySelector('section.composer textarea#prompt');
    const send = document.querySelector('section.composer button[data-action="send"]');
    return {
      projection,
      prompt: {
        count: document.querySelectorAll('section.composer textarea#prompt').length,
        value: prompt instanceof HTMLTextAreaElement ? prompt.value : null,
        visible: visible(prompt),
        enabled: enabled(prompt),
      },
      send_enabled: enabled(send),
      interrupt_button: {
        count: interruptButtons.length,
        visible: visible(interrupt),
        enabled: enabled(interrupt),
      },
      output_agent_trigger: {
        count: outputTriggers.length,
        visible: visible(outputTrigger),
        enabled: enabled(outputTrigger),
      },
      agent_list_card: {
        count: listCards.length,
        visible: visible(listCard),
        enabled: enabled(listCard),
        status_key: listCard instanceof HTMLElement
          ? Array.from(listCard.classList)
            .find((name) => name.startsWith('agent-status-'))
            ?.slice('agent-status-'.length) ?? null
          : null,
      },
      history_agent_card: {
        count: historyCards.length,
        visible: visible(historyCard),
        status_key: historyCard instanceof HTMLElement
          ? Array.from(historyCard.classList)
            .find((name) => name.startsWith('agent-status-'))
            ?.slice('agent-status-'.length) ?? null
          : null,
        status_text: historyCard?.querySelector('.agent-status-label')?.textContent?.trim() ?? null,
      },
      agent_inspector: {
        count: inspectors.length,
        visible: visible(inspector),
        agent_path: execution instanceof HTMLElement ? execution.dataset.agentPath ?? null : null,
        status_text: execution?.querySelector('.agent-execution-meta > span')?.textContent?.trim() ?? null,
      },
      visible_fatal_count: Array.from(document.querySelectorAll('.fatal')).filter(visible).length,
      visible_recoverable_error_count: Array.from(document.querySelectorAll('.ui-error-notice')).filter(visible).length,
      visible_dialog_count: Array.from(document.querySelectorAll('[role="dialog"], [role="alertdialog"]')).filter(visible).length,
      visible_modal_backdrop_count: Array.from(document.querySelectorAll('.modal-backdrop')).filter(visible).length,
    };
  })()`);
}

async function observeAgentInterruptSample(cdp, provider, executionTarget = null) {
  const surface = await observeAgentInterruptSurface(cdp);
  const childExecutionOutcome = executionTarget === null
    ? null
    : await invokeDesktopCommandOutcome(cdp, "load_agent_execution", {
      expectedTarget: executionTarget,
    });
  return {
    surface,
    ledger: provider.requestLedger,
    provider: provider.resourceObservation(),
    child_execution_outcome: childExecutionOutcome,
  };
}

function keyCode(character) {
  if (/^[a-z]$/.test(character)) return `Key${character.toUpperCase()}`;
  if (character === " ") return "Space";
  throw new TypeError(`unsupported agent.interrupt character: ${character}`);
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
  };
}

function productFailure(code, message, evidence) {
  return new DesktopE2eError("product", code, message, evidence);
}

function errorObservation(error) {
  return {
    owner: error instanceof DesktopE2eError ? error.owner : "harness",
    code: error?.code ?? "unclassified-error",
    message: error?.message ?? String(error),
    evidence: error?.evidence ?? null,
  };
}

async function waitForProductStage({ label, timeoutMs, sample, decide, code, message }) {
  let decision = "pending";
  let observed;
  try {
    observed = await waitForObservation({
      label,
      timeoutMs,
      pollMs: 100,
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

function impossibleProviderProgress(sample, terminal) {
  const ledger = sample?.ledger;
  if (!Array.isArray(ledger) || ledger.length > 3) return true;
  const knownRoles = [];
  for (const row of ledger) {
    if (row?.route !== "responses" || row?.method !== "POST" || row?.pathname !== "/v1/responses") return true;
    if (row.contract?.pass === false || row.response_phase === "rejected") return true;
    if (row.contract?.role !== null && row.contract?.role !== undefined) knownRoles.push(row.contract.role);
    if (row.response_status !== null && row.response_status !== 200) return true;
    if (row.contract?.role === "child_held"
      && ![null, "held", ...(terminal ? ["peer_closed"] : [])].includes(row.response_phase)) return true;
  }
  return new Set(knownRoles).size !== knownRoles.length;
}

function inFlightDecision(sample) {
  if (impossibleProviderProgress(sample, false)) return "fail";
  const projection = sample?.surface?.projection;
  if (sample?.surface?.visible_fatal_count > 0
    || sample?.surface?.visible_recoverable_error_count > 0
    || ["failed", "cancelled"].includes(projection?.run_status_key)) return "fail";
  return agentInterruptInFlightFailures(sample).length === 0 ? "pass" : "pending";
}

function listDecision(sample, expectedTarget) {
  if (impossibleProviderProgress(sample, false)) return "fail";
  if (sample?.surface?.visible_fatal_count > 0
    || sample?.surface?.visible_recoverable_error_count > 0) return "fail";
  return agentInterruptListFailures(sample, expectedTarget).length === 0 ? "pass" : "pending";
}

function controlDecision(sample, expectedTarget) {
  if (impossibleProviderProgress(sample, false)) return "fail";
  if (sample?.surface?.visible_fatal_count > 0
    || sample?.surface?.visible_recoverable_error_count > 0) return "fail";
  return agentInterruptControlFailures(sample, expectedTarget).length === 0 ? "pass" : "pending";
}

function terminalDecision(sample, expectedTarget, rootOwner) {
  if (impossibleProviderProgress(sample, true)) return "fail";
  const projection = sample?.surface?.projection;
  const child = Array.isArray(projection?.agent_activity_rows)
    ? projection.agent_activity_rows.find((row) => row?.agent_path === expectedTarget.agentPath)
    : null;
  if (sample?.surface?.visible_fatal_count > 0
    || sample?.surface?.visible_recoverable_error_count > 0
    || projection?.run_status_key !== "completed" && ["failed", "cancelled"].includes(projection?.run_status_key)
    || ["completed", "errored", "shutdown"].includes(child?.status)) return "fail";
  return agentInterruptTerminalFailures(sample, expectedTarget, rootOwner).length === 0 ? "pass" : "pending";
}

export function createAgentInterruptScenario() {
  const state = {
    provider: null,
    acceptedLedger: null,
    quiesceOutcome: null,
    inputCleanupFailure: null,
    commandProbeCleanupFailure: null,
  };
  return Object.freeze({
    id: "agent.interrupt",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    requestGracefulExit,
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({
        expectedPrompt: AGENT_INTERRUPT_PROMPT,
        script: createAgentInterruptProviderScript(),
      });
      await prepareDesktopFixture({
        context,
        sink,
        phase,
        owner: OWNER,
        configText: agentInterruptFixtureConfig(state.provider.baseUrl),
        sentinelName: "E2E_AGENT_INTERRUPT.txt",
        sentinelText: "moyAI Desktop E2E exact Sub Agent interrupt fixture.\n",
      });
      await sink.record("scripted-provider-started", state.provider.resourceObservation(), { phase, owner: OWNER });
    },
    async execute({ context, driver: cdp, sink }) {
      const provider = state.provider;
      if (provider === null) throw new Error("scripted provider was not prepared");
      await acquireInteractiveShell({ context, driver: cdp, sink }, {
        evidenceOwner: OWNER,
        screenshotStem: "agent-interrupt-shell-ready",
      });
      if (provider.requestLedger.length !== 0) {
        throw productFailure(
          "agent-interrupt-cold-start-request",
          "Desktop contacted the provider before the user submitted the delegated run",
          { ledger: provider.requestLedger },
        );
      }

      const input = new WebviewInput(cdp, { probeId: "agent-interrupt" });
      const commandProbe = new DesktopCommandProbe(cdp, {
        probeId: "agent-interrupt-command",
        commands: ["interrupt_agent"],
      });
      let primaryError = null;
      try {
        await input.installProbe();
        const promptClick = await trustedClick(input, PROMPT);
        const typeStart = (await input.snapshotProbe()).sequence;
        await input.typeText(AGENT_INTERRUPT_PROMPT);
        const typing = assertTrustedProbeSequence(await input.snapshotProbe(typeStart), {
          afterSequence: typeStart,
          expected: expectedTypedEvents(AGENT_INTERRUPT_PROMPT),
        });
        const typed = await waitForObservation({
          label: "agent.interrupt trusted prompt readiness",
          timeoutMs: 10_000,
          pollMs: 50,
          sample: () => observeAgentInterruptSurface(cdp),
          accept: (surface) => surface?.prompt?.value === AGENT_INTERRUPT_PROMPT
            && surface?.prompt?.visible === true
            && surface?.prompt?.enabled === true
            && surface?.send_enabled === true,
        });
        const send = await trustedClick(input, SEND);
        await sink.record("trusted-agent-run-submit-acquired", {
          input_kind: "browser_trusted",
          prompt_click: promptClick,
          typing,
          send,
          typed_projection_revision: typed.value.projection.projection_revision,
        }, { phase: "executing", owner: OWNER });

        const inFlight = await waitForProductStage({
          label: "completed root and one exact held child interrupt target",
          timeoutMs: 60_000,
          sample: () => observeAgentInterruptSample(cdp, provider),
          decide: inFlightDecision,
          code: "agent-interrupt-in-flight-contract-mismatch",
          message: "the delegated run did not settle to a completed root and one exact interruptible held child",
        });
        const target = structuredClone(inFlight.value.surface.projection.agent_activity_rows[0].interrupt_target);
        const rootOwner = captureRootOwner(inFlight.value.surface.projection, target);
        const outputAgentTrigger = await trustedClick(input, OUTPUT_AGENT_TRIGGER);
        await waitForProductStage({
          label: "exact held child in canonical agent list",
          timeoutMs: 10_000,
          sample: () => observeAgentInterruptSample(cdp, provider),
          decide: (sample) => listDecision(sample, target),
          code: "agent-interrupt-list-contract-mismatch",
          message: "the canonical Sub Agent route did not expose one exact held child",
        });
        const agentListCard = await trustedClick(input, AGENT_LIST_CARD);
        const control = await waitForProductStage({
          label: "exact held child inspector interrupt control",
          timeoutMs: 10_000,
          sample: () => observeAgentInterruptSample(cdp, provider),
          decide: (sample) => controlDecision(sample, target),
          code: "agent-interrupt-control-contract-mismatch",
          message: "the exact held child card did not open one interactable interrupt control",
        });
        const beforeScreenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "agent-interrupt-child-in-flight",
          owner: OWNER,
        });
        await sink.record("agent-interrupt-in-flight", {
          interrupt_target: target,
          root_owner: rootOwner,
          output_agent_trigger: outputAgentTrigger,
          agent_list_card: agentListCard,
          agent_activity_rows: control.value.surface.projection.agent_activity_rows,
          surface: control.value.surface,
          provider_ledger: control.value.ledger,
          provider_resource: control.value.provider,
          screenshot: beforeScreenshot,
        }, { phase: "executing", owner: OWNER });

        await commandProbe.install();
        const interrupt = await trustedClick(input, INTERRUPT);
        await cdp.evaluate("new Promise((resolve) => setTimeout(resolve, 50))");
        const commandAcquisition = assertExactDesktopCommandSequence(
          await commandProbe.snapshot(),
          {
            expected: [{ command: "interrupt_agent", args: { expectedTarget: target } }],
          },
        );
        await sink.record("trusted-agent-interrupt-acquired", {
          input_kind: "browser_trusted",
          exact_activation_count: 1,
          interrupt_target: target,
          interrupt,
          command_acquisition: commandAcquisition,
        }, { phase: "executing", owner: OWNER });

        const executionTarget = childExecutionTarget(target);
        const terminal = await waitForProductStage({
          label: "durable AgentInterrupted child and idle agent tree",
          timeoutMs: 60_000,
          sample: () => observeAgentInterruptSample(cdp, provider, executionTarget),
          decide: (sample) => terminalDecision(sample, target, rootOwner),
          code: "agent-interrupt-terminal-contract-mismatch",
          message: "the accepted child interrupt did not settle durably without replay or root/newer-turn drift",
        });
        const afterScreenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "agent-interrupt-child-terminal",
          owner: OWNER,
        });
        const terminalCommandAcquisition = assertExactDesktopCommandSequence(
          await commandProbe.snapshot(),
          {
            expected: [{ command: "interrupt_agent", args: { expectedTarget: target } }],
          },
        );
        state.acceptedLedger = structuredClone(terminal.value.ledger);
        await sink.record("agent-interrupt-terminal", {
          interrupt_target: target,
          root_owner: rootOwner,
          projection_revision: terminal.value.surface.projection.projection_revision,
          agent_activity_rows: terminal.value.surface.projection.agent_activity_rows,
          child_execution: terminal.value.child_execution_outcome.value,
          provider_ledger: state.acceptedLedger,
          provider_resource: terminal.value.provider,
          command_acquisition: terminalCommandAcquisition,
          surface: terminal.value.surface,
          screenshot: afterScreenshot,
        }, { phase: "executing", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        let cleanupError = null;
        try { await input.cleanup(); }
        catch (error) {
          state.inputCleanupFailure = errorObservation(error);
          cleanupError = new DesktopE2eError(
            "harness",
            "agent-interrupt-input-cleanup-failed",
            "agent.interrupt WebView input did not settle exactly",
            state.inputCleanupFailure,
          );
        }
        try { await commandProbe.remove(); }
        catch (error) {
          state.commandProbeCleanupFailure = errorObservation(error);
          cleanupError ??= new DesktopE2eError(
              "harness",
              "agent-interrupt-command-probe-cleanup-failed",
              "agent.interrupt Desktop command probe did not restore exactly",
              state.commandProbeCleanupFailure,
            );
        }
        if (primaryError === null && cleanupError !== null) throw cleanupError;
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
        && state.inputCleanupFailure === null
        && state.commandProbeCleanupFailure === null;
      return {
        input: pass ? "pass" : "fail",
        resources: [{
          kind: "agent-interrupt-verification",
          quiesced,
          quiesce_input: state.quiesceOutcome?.input ?? null,
          input_cleanup_failure: state.inputCleanupFailure,
          command_probe_cleanup_failure: state.commandProbeCleanupFailure,
        }],
      };
    },
  });
}

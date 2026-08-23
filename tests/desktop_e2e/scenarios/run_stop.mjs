import { waitForObservation } from "../core/deadline.mjs";
import {
  canonicalU64,
  canonicalUlid,
  canonicalWorkspace,
} from "../core/canonical_identity.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { WebviewInput, assertTrustedProbeSequence } from "../drivers/webview_input.mjs";
import { startScriptedProvider } from "../drivers/scripted_provider.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import {
  classifyAcquiredObservationFailure,
  providerRestartFixtureConfig,
  quiesceProviderResource,
} from "./provider_restart.mjs";
import { exactRunExpectedState } from "./prompt_review_cancel.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";

const OWNER = "scenario:run.stop";
export const RUN_STOP_PROMPT = "wait until user stop";

const PROMPT = Object.freeze({
  selector: "section.composer textarea#prompt",
  identity: { tag: "TEXTAREA", id: "prompt" },
});
const SEND = Object.freeze({
  selector: 'section.composer button[data-action="send"]',
  identity: { tag: "BUTTON", action: "send" },
});
const STOP = Object.freeze({
  selector: 'section.run-strip button[data-action="cancel-run"][aria-label="実行停止"]',
  identity: { tag: "BUTTON", action: "cancel-run" },
});

function sameValue(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

export function exactTurnStopTarget(target, expected = undefined) {
  const exact = target !== null
    && typeof target === "object"
    && !Array.isArray(target)
    && target.kind === "turn"
    && sameValue(Object.keys(target).sort(), [
      "admissionRevision",
      "kind",
      "rootEpoch",
      "sessionId",
      "turnId",
      "workspacePath",
    ])
    && canonicalWorkspace(target.workspacePath)
    && canonicalUlid(target.sessionId)
    && canonicalUlid(target.turnId)
    && canonicalU64(target.admissionRevision)
    && canonicalU64(target.rootEpoch);
  if (!exact || expected === undefined) return exact;
  return target.workspacePath === expected.workspacePath
    && target.sessionId === expected.sessionId
    && target.turnId === expected.turnId
    && target.admissionRevision === expected.admissionRevision
    && target.rootEpoch === expected.rootEpoch;
}

function acceptedResponseRow(row, phase) {
  return row?.method === "POST"
    && row?.pathname === "/v1/responses"
    && row?.query_present === false
    && row?.contract?.pass === true
    && row?.response_phase === phase
    && row?.response_status === null;
}

export function exactHeldRunStopLedger(ledger) {
  return Array.isArray(ledger) && ledger.length === 1 && acceptedResponseRow(ledger[0], "held");
}

export function exactStoppedRunStopLedger(ledger) {
  return Array.isArray(ledger) && ledger.length === 1 && acceptedResponseRow(ledger[0], "peer_closed");
}

function sessionRowFor(projection, sessionId) {
  if (typeof sessionId !== "string") return null;
  return [
    ...(Array.isArray(projection?.session_rows) ? projection.session_rows : []),
    ...(Array.isArray(projection?.chat_session_rows) ? projection.chat_session_rows : []),
  ].find((row) => row?.session_id === sessionId) ?? null;
}

function transcriptSummary(projection) {
  const rows = Array.isArray(projection?.transcript_rows) ? projection.transcript_rows : [];
  return {
    users: rows.filter((row) => row?.row_kind === "user"),
    assistants: rows.filter((row) => row?.row_kind === "assistant"),
    cancelled: rows.filter((row) => row?.row_kind === "work_summary_cancelled"),
  };
}

function visibleTaskIndicator(observation, state, minimumSize) {
  return observation?.count === 1
    && observation?.state === state
    && observation?.visible === true
    && cssNumber(observation?.opacity) > 0
    && Number.isFinite(observation?.width)
    && Number.isFinite(observation?.height)
    && observation.width >= minimumSize
    && observation.height >= minimumSize;
}

function cssNumber(value) {
  if (typeof value === "number") return Number.isFinite(value) ? value : Number.NaN;
  if (typeof value !== "string") return Number.NaN;
  const parsed = Number.parseFloat(value);
  return Number.isFinite(parsed) ? parsed : Number.NaN;
}

function cssTimeMs(value) {
  if (typeof value !== "string") return Number.NaN;
  const first = value.split(",", 1)[0]?.trim() ?? "";
  if (first.endsWith("ms")) return cssNumber(first);
  if (first.endsWith("s")) return cssNumber(first) * 1000;
  return Number.NaN;
}

function cssColorHasPaint(value) {
  if (typeof value !== "string") return false;
  const color = value.trim().toLowerCase();
  if (color.length === 0 || color === "transparent") return false;
  const slashAlpha = /\/\s*([0-9.]+%?)/.exec(color)?.[1];
  const functional = /^(?:rgb|rgba)\((.*)\)$/.exec(color)?.[1];
  let alpha = slashAlpha;
  if (alpha === undefined && functional?.includes(",")) {
    const components = functional.split(",").map((component) => component.trim());
    if (components.length >= 4) alpha = components[3];
  }
  if (alpha === undefined) return true;
  const parsed = alpha.endsWith("%")
    ? cssNumber(alpha) / 100
    : cssNumber(alpha);
  return Number.isFinite(parsed) && parsed > 0.01;
}

function borderSideHasPaint(style, side) {
  return cssNumber(style?.[`border_${side}_width`]) >= 1
    && !["none", "hidden", ""].includes(style?.[`border_${side}_style`] ?? "")
    && cssColorHasPaint(style?.[`border_${side}_color`]);
}

function pseudoHasPaint(pseudo) {
  if (cssNumber(pseudo?.opacity) <= 0) return false;
  const hasArea = cssNumber(pseudo?.width) > 0 && cssNumber(pseudo?.height) > 0;
  const content = typeof pseudo?.content === "string" ? pseudo.content.trim() : "";
  const hasText = !["", '""', "none", "normal"].includes(content)
    && cssColorHasPaint(pseudo?.color);
  const hasBorder = ["top", "right", "bottom", "left"]
    .some((side) => borderSideHasPaint(pseudo, side));
  return hasText || (hasArea && (cssColorHasPaint(pseudo?.background_color) || hasBorder));
}

function indicatorHasPaint(observation) {
  if (cssNumber(observation?.opacity) <= 0) return false;
  return cssColorHasPaint(observation?.background_color)
    || ["top", "right", "bottom", "left"]
      .some((side) => borderSideHasPaint(observation, side))
    || pseudoHasPaint(observation?.before)
    || pseudoHasPaint(observation?.after);
}

function runningIndicatorHasShape(observation) {
  const minimumDimension = Math.min(cssNumber(observation?.width), cssNumber(observation?.height));
  const circular = Number.isFinite(minimumDimension)
    && minimumDimension > 0
    && cssNumber(observation?.border_radius) >= minimumDimension * 0.45;
  const completeRing = ["top", "right", "bottom", "left"]
    .every((side) => borderSideHasPaint(observation, side));
  const centerDot = cssNumber(observation?.after?.width) >= 3
    && cssNumber(observation?.after?.height) >= 3
    && pseudoHasPaint(observation?.after);
  return circular && completeRing && centerDot;
}

function runningIndicatorAnimationIsMeaningful(observation, reducedMotion) {
  if (reducedMotion) return observation?.animation_name === "none";
  const durationMs = cssTimeMs(observation?.animation_duration);
  return observation?.animation_name === "moyai-task-activity-running"
    && Number.isFinite(durationMs)
    && durationMs >= 500
    && durationMs <= 5000
    && observation?.animation_iteration_count === "infinite"
    && observation?.animation_play_state === "running";
}

function selectedIndicatorOwnsTarget(observation, target) {
  return observation?.row_selected === true
    && observation?.row_aria_current === "page"
    && ["session", "chat-session"].includes(observation?.row_action)
    && observation?.row_session_id === target?.sessionId
    && observation?.row_focus_key
      === `${observation?.row_action}:${target?.sessionId}:select`;
}

export function runStopInFlightFailures(sample) {
  const failures = [];
  const surface = sample?.surface;
  const projection = surface?.projection;
  const target = projection?.stop_target;
  if (!exactTurnStopTarget(target)) failures.push("turn-stop-target-not-canonical");
  const expectedState = projection?.run_target?.expectedState;
  if (!exactRunExpectedState(expectedState)
    || expectedState.kind !== "turn"
    || expectedState.turnId !== target?.turnId
    || expectedState.admissionRevision !== target?.admissionRevision) {
    failures.push("run-owner-and-stop-target-differ");
  }
  const row = sessionRowFor(projection, target?.sessionId);
  if (!row
    || row.admission_revision !== target?.admissionRevision
    || row.active_turn_id !== target?.turnId
    || !sameValue(row.interrupt_target, target)) {
    failures.push("session-row-stop-owner-differs");
  }
  if (projection?.run_status_key !== "running"
    || projection?.busy !== true
    || projection?.task_activity_state !== "running"
    || projection?.can_cancel_run !== true) {
    failures.push("run-not-in-flight");
  }
  if (surface?.stop_button?.count !== 1
    || surface?.stop_button?.visible !== true
    || surface?.stop_button?.enabled !== true) {
    failures.push("semantic-stop-not-interactable");
  }
  const taskActivity = surface?.task_activity;
  if (!visibleTaskIndicator(taskActivity?.run_strip, "running", 17)
    || taskActivity?.run_strip?.small !== false
    || taskActivity?.run_strip?.aria_hidden !== "true"
    || taskActivity?.run_label !== "実行中") {
    failures.push("central-running-indicator-not-visible");
  }
  if (!visibleTaskIndicator(taskActivity?.selected_sidebar, "running", 17)
    || taskActivity?.selected_sidebar?.small !== false
    || taskActivity?.selected_sidebar?.aria_label !== "実行中") {
    failures.push("selected-running-indicator-not-visible");
  }
  if (!selectedIndicatorOwnsTarget(taskActivity?.selected_sidebar, target)) {
    failures.push("selected-running-indicator-owner-mismatch");
  }
  if (!indicatorHasPaint(taskActivity?.run_strip)) {
    failures.push("central-running-indicator-not-painted");
  }
  if (!indicatorHasPaint(taskActivity?.selected_sidebar)) {
    failures.push("selected-running-indicator-not-painted");
  }
  if (!runningIndicatorHasShape(taskActivity?.run_strip)) {
    failures.push("central-running-indicator-shape-mismatch");
  }
  if (!runningIndicatorHasShape(taskActivity?.selected_sidebar)) {
    failures.push("selected-running-indicator-shape-mismatch");
  }
  const reducedMotion = taskActivity?.prefers_reduced_motion === true;
  if (!runningIndicatorAnimationIsMeaningful(taskActivity?.run_strip, reducedMotion)
    || !runningIndicatorAnimationIsMeaningful(taskActivity?.selected_sidebar, reducedMotion)) {
    failures.push("running-indicator-animation-mismatch");
  }
  if (!exactHeldRunStopLedger(sample?.ledger)) failures.push("provider-request-not-held-exactly-once");
  if (sample?.provider?.active_request_count !== 1
    || sample?.provider?.accepted_response_count !== 1
    || sample?.provider?.successful_response_count !== 0) {
    failures.push("provider-resource-not-in-flight");
  }
  if (projection?.overlay !== "none" || projection?.confirmation_visible !== false) {
    failures.push("blocking-overlay-visible");
  }
  if (surface?.visible_fatal_count !== 0 || surface?.visible_recoverable_error_count !== 0) {
    failures.push("error-overlay-visible");
  }
  return failures;
}

export function runStopTerminalFailures(sample, expectedTarget) {
  const failures = [];
  const surface = sample?.surface;
  const projection = surface?.projection;
  const expectedState = projection?.run_target?.expectedState;
  if (projection?.run_status_key !== "cancelled"
    || projection?.status_code !== "user_stopped") {
    failures.push("durable-user-stop-terminal-missing");
  }
  if (projection?.task_activity_state !== "idle"
    || projection?.busy !== false
    || projection?.agent_tree_active !== false
    || projection?.post_run_refresh_pending !== false
    || projection?.background_mutation_pending !== false
    || projection?.async_polling_required !== false
    || !Array.isArray(projection?.pending_async_operations)
    || projection.pending_async_operations.length !== 0
    || projection?.navigation_loading !== false) {
    failures.push("run-owner-not-idle");
  }
  if (projection?.can_cancel_run !== false
    || projection?.stop_target !== null
    || surface?.stop_button?.count !== 0) {
    failures.push("stop-admission-not-cleared");
  }
  if (surface?.task_activity?.total_count !== 0
    || surface?.task_activity?.visible_count !== 0
    || surface?.task_activity?.run_strip?.count !== 0
    || surface?.task_activity?.selected_sidebar?.count !== 0) {
    failures.push("terminal-task-indicator-not-cleared");
  }
  if (!exactRunExpectedState(expectedState, {
    kind: "idle",
    latestTurnId: expectedTarget.turnId,
    admissionRevision: expectedTarget.admissionRevision,
  })) {
    failures.push("terminal-idle-owner-drift");
  }
  const row = sessionRowFor(projection, expectedTarget.sessionId);
  if (!row
    || row.status !== "cancelled"
    || row.loaded_status !== "idle"
    || row.admission_revision !== expectedTarget.admissionRevision
    || row.active_turn_id != null
    || row.interrupt_target != null) {
    failures.push("terminal-session-row-drift");
  }
  const history = transcriptSummary(projection);
  if (history.users.length !== 1
    || history.users[0]?.body !== RUN_STOP_PROMPT
    || history.cancelled.length !== 1
    || history.assistants.length !== 0) {
    failures.push("user-stop-history-not-exact");
  }
  if (surface?.user_rows?.length !== 1
    || surface.user_rows[0]?.text !== RUN_STOP_PROMPT
    || surface.user_rows[0]?.visible !== true
    || surface?.cancelled_rows?.length !== 1
    || surface.cancelled_rows[0]?.visible !== true
    || surface?.assistant_count !== 0) {
    failures.push("user-stop-dom-not-exact");
  }
  if (!exactStoppedRunStopLedger(sample?.ledger)) failures.push("provider-request-replayed-or-completed");
  if (sample?.provider?.active_request_count !== 0
    || sample?.provider?.accepted_response_count !== 1
    || sample?.provider?.successful_response_count !== 0) {
    failures.push("provider-request-not-cancelled");
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

async function observeRunStopSurface(cdp) {
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
    const styleObservation = (style) => ({
      display: style?.display ?? null,
      opacity: style?.opacity ?? null,
      color: style?.color ?? null,
      background_color: style?.backgroundColor ?? null,
      border_radius: style?.borderRadius ?? null,
      border_top_width: style?.borderTopWidth ?? null,
      border_top_style: style?.borderTopStyle ?? null,
      border_top_color: style?.borderTopColor ?? null,
      border_right_width: style?.borderRightWidth ?? null,
      border_right_style: style?.borderRightStyle ?? null,
      border_right_color: style?.borderRightColor ?? null,
      border_bottom_width: style?.borderBottomWidth ?? null,
      border_bottom_style: style?.borderBottomStyle ?? null,
      border_bottom_color: style?.borderBottomColor ?? null,
      border_left_width: style?.borderLeftWidth ?? null,
      border_left_style: style?.borderLeftStyle ?? null,
      border_left_color: style?.borderLeftColor ?? null,
      animation_name: style?.animationName ?? null,
      animation_duration: style?.animationDuration ?? null,
      animation_iteration_count: style?.animationIterationCount ?? null,
      animation_play_state: style?.animationPlayState ?? null,
      transform: style?.transform ?? null,
    });
    const pseudoObservation = (element, pseudo) => {
      if (!(element instanceof HTMLElement)) return styleObservation(null);
      const style = getComputedStyle(element, pseudo);
      return {
        ...styleObservation(style),
        content: style.content,
        width: style.width,
        height: style.height,
      };
    };
    const indicatorObservation = (elements) => {
      const element = elements.length === 1 && elements[0] instanceof HTMLElement
        ? elements[0]
        : null;
      const style = element === null ? null : getComputedStyle(element);
      const rect = element === null ? null : element.getBoundingClientRect();
      const row = element?.closest('.nav-row-wrap') ?? null;
      const rowButton = row?.querySelector(
        '.nav-row[data-action="session"], .nav-row[data-action="chat-session"]'
      ) ?? null;
      const rowAction = rowButton?.getAttribute('data-action') ?? null;
      const rowFocusKey = rowButton?.getAttribute('data-focus-key') ?? null;
      const rowPrefix = rowAction === null ? null : rowAction + ':';
      const rowSuffix = ':select';
      const rowSessionId = rowPrefix !== null
        && typeof rowFocusKey === 'string'
        && rowFocusKey.startsWith(rowPrefix)
        && rowFocusKey.endsWith(rowSuffix)
        ? rowFocusKey.slice(rowPrefix.length, -rowSuffix.length)
        : null;
      return {
        ...styleObservation(style),
        count: elements.length,
        state: element?.dataset.taskActivity ?? null,
        visible: visible(element),
        width: rect?.width ?? null,
        height: rect?.height ?? null,
        small: element?.classList.contains('small') ?? null,
        aria_hidden: element?.getAttribute('aria-hidden') ?? null,
        aria_label: element?.getAttribute('aria-label') ?? null,
        before: pseudoObservation(element, '::before'),
        after: pseudoObservation(element, '::after'),
        row_action: rowAction,
        row_focus_key: rowFocusKey,
        row_session_id: rowSessionId,
        row_selected: row?.classList.contains('selected') ?? false,
        row_aria_current: rowButton?.getAttribute('aria-current') ?? null,
      };
    };
    const stopButtons = Array.from(document.querySelectorAll(
      'section.run-strip button[data-action="cancel-run"][aria-label="実行停止"]'
    ));
    const stop = stopButtons.length === 1 ? stopButtons[0] : null;
    const prompt = document.querySelector('section.composer textarea#prompt');
    const send = document.querySelector('section.composer button[data-action="send"]');
    const allTaskIndicators = Array.from(document.querySelectorAll('.task-activity-indicator'));
    const runStripIndicators = Array.from(document.querySelectorAll(
      'section.run-strip .task-activity-indicator'
    ));
    const selectedSidebarIndicators = Array.from(document.querySelectorAll(
      'aside.sidebar .nav-row-wrap.selected .task-activity-indicator'
    ));
    const rows = (selector) => Array.from(document.querySelectorAll(selector)).map((row) => ({
      text: (row.querySelector('.markdown-body')?.innerText ?? '').trim(),
      visible: visible(row),
    }));
    return {
      projection,
      prompt: {
        count: document.querySelectorAll('section.composer textarea#prompt').length,
        value: prompt instanceof HTMLTextAreaElement ? prompt.value : null,
        visible: visible(prompt),
        enabled: enabled(prompt),
      },
      send_enabled: enabled(send),
      stop_button: {
        count: stopButtons.length,
        visible: visible(stop),
        enabled: enabled(stop),
      },
      task_activity: {
        total_count: allTaskIndicators.length,
        visible_count: allTaskIndicators.filter(visible).length,
        prefers_reduced_motion: matchMedia('(prefers-reduced-motion: reduce)').matches,
        run_label: (document.querySelector('section.run-strip > strong')?.textContent ?? '').trim(),
        run_strip: indicatorObservation(runStripIndicators),
        selected_sidebar: indicatorObservation(selectedSidebarIndicators),
      },
      user_rows: rows('main.conversation #thread article.message.user'),
      cancelled_rows: rows('main.conversation #thread article.message.work-summary.work_summary_cancelled'),
      assistant_count: document.querySelectorAll('main.conversation #thread article.message.assistant').length,
      visible_fatal_count: Array.from(document.querySelectorAll('.fatal')).filter(visible).length,
      visible_recoverable_error_count: Array.from(document.querySelectorAll('.ui-error-notice')).filter(visible).length,
      visible_dialog_count: Array.from(document.querySelectorAll('[role="dialog"], [role="alertdialog"]')).filter(visible).length,
      visible_modal_backdrop_count: Array.from(document.querySelectorAll('.modal-backdrop')).filter(visible).length,
    };
  })()`);
}

function keyCode(character) {
  if (/^[a-z]$/.test(character)) return `Key${character.toUpperCase()}`;
  if (character === " ") return "Space";
  throw new TypeError(`unsupported run.stop character: ${character}`);
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

function inFlightDecision(sample) {
  const projection = sample?.surface?.projection;
  const ledger = sample?.ledger;
  if (!Array.isArray(ledger) || ledger.length > 1) return "fail";
  if (ledger.length === 1 && (
    ledger[0]?.method !== "POST"
    || ledger[0]?.pathname !== "/v1/responses"
    || ledger[0]?.contract?.pass === false
    || ledger[0]?.response_status !== null
    || ![null, "held"].includes(ledger[0]?.response_phase)
  )) return "fail";
  if (sample?.surface?.visible_fatal_count > 0
    || sample?.surface?.visible_recoverable_error_count > 0
    || ["completed", "cancelled", "failed"].includes(projection?.run_status_key)) return "fail";
  return runStopInFlightFailures(sample).length === 0 ? "pass" : "pending";
}

function terminalDecision(sample, expectedTarget) {
  const ledger = sample?.ledger;
  if (!Array.isArray(ledger) || ledger.length > 1) return "fail";
  if (ledger.length === 1 && (
    ledger[0]?.method !== "POST"
    || ledger[0]?.pathname !== "/v1/responses"
    || ledger[0]?.contract?.pass !== true
    || ledger[0]?.response_status !== null
    || !["held", "peer_closed"].includes(ledger[0]?.response_phase)
  )) return "fail";
  const projection = sample?.surface?.projection;
  if (sample?.surface?.visible_fatal_count > 0
    || sample?.surface?.visible_recoverable_error_count > 0
    || ["completed", "failed"].includes(projection?.run_status_key)) return "fail";
  return runStopTerminalFailures(sample, expectedTarget).length === 0 ? "pass" : "pending";
}

export function createRunStopScenario() {
  const state = {
    provider: null,
    acceptedLedger: null,
    quiesceOutcome: null,
    inputCleanupFailure: null,
  };
  return Object.freeze({
    id: "run.stop",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    requestGracefulExit,
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({
        expectedPrompt: RUN_STOP_PROMPT,
        responseBehavior: "hold_until_peer_close",
      });
      await prepareDesktopFixture({
        context,
        sink,
        phase,
        owner: OWNER,
        configText: providerRestartFixtureConfig(state.provider.baseUrl),
        sentinelName: "E2E_RUN_STOP.txt",
        sentinelText: "moyAI Desktop E2E durable User Stop fixture.\n",
      });
      await sink.record("scripted-provider-started", state.provider.resourceObservation(), { phase, owner: OWNER });
    },
    async execute({ context, driver: cdp, sink }) {
      const provider = state.provider;
      if (provider === null) throw new Error("scripted provider was not prepared");
      await acquireInteractiveShell({ context, driver: cdp, sink }, {
        evidenceOwner: OWNER,
        screenshotStem: "run-stop-shell-ready",
      });
      if (provider.requestLedger.length !== 0) {
        throw productFailure("run-stop-cold-start-request", "Desktop contacted the provider before the user submitted a run", {
          ledger: provider.requestLedger,
        });
      }

      const input = new WebviewInput(cdp, { probeId: "run-stop" });
      let primaryError = null;
      try {
        await input.installProbe();
        const promptClick = await trustedClick(input, PROMPT);
        const typeStart = (await input.snapshotProbe()).sequence;
        await input.typeText(RUN_STOP_PROMPT);
        const typeSnapshot = await input.snapshotProbe(typeStart);
        const typing = assertTrustedProbeSequence(typeSnapshot, {
          afterSequence: typeStart,
          expected: expectedTypedEvents(RUN_STOP_PROMPT),
        });
        const typed = await waitForObservation({
          label: "run.stop trusted prompt readiness",
          timeoutMs: 10_000,
          pollMs: 50,
          sample: () => observeRunStopSurface(cdp),
          accept: (surface) => surface?.prompt?.value === RUN_STOP_PROMPT
            && surface?.prompt?.visible === true
            && surface?.prompt?.enabled === true
            && surface?.send_enabled === true,
        });
        const send = await trustedClick(input, SEND);
        await sink.record("trusted-run-submit-acquired", {
          input_kind: "browser_trusted",
          prompt_click: promptClick,
          typing,
          send,
          typed_projection_revision: typed.value.projection.projection_revision,
        }, { phase: "executing", owner: OWNER });

        const inFlight = await waitForProductStage({
          label: "one held provider request and exact Turn Stop owner",
          timeoutMs: 45_000,
          sample: async () => ({
            surface: await observeRunStopSurface(cdp),
            ledger: provider.requestLedger,
            provider: provider.resourceObservation(),
          }),
          decide: inFlightDecision,
          code: "run-stop-in-flight-contract-mismatch",
          message: "the submitted run did not settle to one held provider request with an exact Turn Stop target",
        });
        const stopTarget = structuredClone(inFlight.value.surface.projection.stop_target);
        const beforeScreenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "run-stop-in-flight",
          owner: OWNER,
        });
        await sink.record("run-stop-in-flight", {
          stop_target: stopTarget,
          run_expected_state: inFlight.value.surface.projection.run_target.expectedState,
          task_activity: inFlight.value.surface.task_activity,
          session_rows: inFlight.value.surface.projection.session_rows,
          chat_session_rows: inFlight.value.surface.projection.chat_session_rows,
          provider_ledger: inFlight.value.ledger,
          provider_resource: inFlight.value.provider,
          screenshot: beforeScreenshot,
        }, { phase: "executing", owner: OWNER });

        const stop = await trustedClick(input, STOP);
        await sink.record("trusted-run-stop-acquired", {
          input_kind: "browser_trusted",
          exact_activation_count: 1,
          stop_target: stopTarget,
          stop,
        }, { phase: "executing", owner: OWNER });

        const terminal = await waitForProductStage({
          label: "durable UserStop terminal and idle projection",
          timeoutMs: 45_000,
          sample: async () => ({
            surface: await observeRunStopSurface(cdp),
            ledger: provider.requestLedger,
            provider: provider.resourceObservation(),
          }),
          decide: (sample) => terminalDecision(sample, stopTarget),
          code: "run-stop-terminal-contract-mismatch",
          message: "trusted Stop did not settle through refresh/poll to durable UserStop and Idle without provider replay",
        });
        const afterScreenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "run-stop-user-stopped",
          owner: OWNER,
        });
        state.acceptedLedger = structuredClone(terminal.value.ledger);
        await sink.record("run-stop-terminal", {
          stop_target: stopTarget,
          projection_revision: terminal.value.surface.projection.projection_revision,
          status_code: terminal.value.surface.projection.status_code,
          run_status_key: terminal.value.surface.projection.run_status_key,
          run_expected_state: terminal.value.surface.projection.run_target.expectedState,
          provider_ledger: state.acceptedLedger,
          provider_resource: terminal.value.provider,
          surface: terminal.value.surface,
          screenshot: afterScreenshot,
        }, { phase: "executing", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        try { await input.cleanup(); }
        catch (error) {
          state.inputCleanupFailure = errorObservation(error);
          if (primaryError === null) {
            throw new DesktopE2eError(
              "harness",
              "run-stop-input-cleanup-failed",
              "run.stop WebView input did not settle exactly",
              state.inputCleanupFailure,
            );
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
          kind: "run-stop-verification",
          quiesced,
          quiesce_input: state.quiesceOutcome?.input ?? null,
          input_cleanup_failure: state.inputCleanupFailure,
        }],
      };
    },
  });
}

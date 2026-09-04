import crypto from "node:crypto";
import { isDeepStrictEqual } from "node:util";

import { DesktopE2eError } from "../core/execution.mjs";
import {
  DesktopCommandProbe,
  assertExactDesktopCommandSequence,
} from "../drivers/desktop_command_probe.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";
import {
  observeSideChatQuoteSurface,
  surfaceHasNoErrors,
  trustedClick,
  trustedInsert,
  waitForProductStage,
} from "./side_chat_quote.mjs";

const OWNER = "scenario:case5_2.stage5";
const DEFAULT_TIMEOUT_MS = 3_600_000;
const MAX_OBSERVATION_ATTEMPT_MS = 55_000;
const PROGRESS_EVIDENCE_MS = 60_000;
const POLL_MS = 500;
const COMMAND_ADMISSION_TIMEOUT_MS = 10_000;
const REQUEST_START_TIMEOUT_MS = 30_000;
const LEAK_STOP_TIMEOUT_MS = 120_000;
const TERMINAL_DOM_SETTLE_GRACE_MS = 5_000;
const CONTROL_TOKENS = Object.freeze(["<|im_start|>", "<|im_end|>"]);
const EVIDENCE_NAME = /^[a-z0-9][a-z0-9._-]{2,80}$/;

const SHOW_SIDE = Object.freeze({
  selector: 'button[data-action="show-side-chat-pane"]',
  identity: { tag: "BUTTON", action: "show-side-chat-pane" },
});
const SIDE_PROMPT = Object.freeze({
  selector: 'aside.side-chat-pane[data-pane-mode="side-chat"] textarea#side-chat-prompt',
  identity: { tag: "TEXTAREA", id: "side-chat-prompt" },
});
const SIDE_SEND = Object.freeze({
  selector: 'aside.side-chat-pane[data-pane-mode="side-chat"] button[data-action="send-side-chat"]',
  identity: { tag: "BUTTON", action: "send-side-chat" },
});
const SIDE_STOP = Object.freeze({
  selector: 'aside.side-chat-pane[data-pane-mode="side-chat"] button[data-action="cancel-side-chat"]',
  identity: { tag: "BUTTON", action: "cancel-side-chat" },
});

function productFailure(code, message, evidence) {
  return new DesktopE2eError("product", code, message, evidence);
}

function sha256(value) {
  return crypto.createHash("sha256").update(value, "utf8").digest("hex");
}

function textIdentity(value) {
  return {
    bytes: Buffer.byteLength(value, "utf8"),
    sha256: sha256(value),
  };
}

function primaryRows(projection) {
  return (projection?.transcript_rows ?? [])
    .filter((row) => ["user", "assistant", "error"].includes(row?.row_kind))
    .map((row) => ({
      id: row.stable_history_identity ?? null,
      kind: row.row_kind,
      body: row.body,
    }));
}

function canonicalRows(projection) {
  return structuredClone(projection?.transcript_rows ?? []);
}

function selectedSessionRow(projection) {
  const rows = projection?.selected_project_index >= 0
    ? projection?.session_rows
    : projection?.chat_session_rows;
  return Number.isInteger(projection?.selected_session_index)
    && projection.selected_session_index >= 0
    ? rows?.[projection.selected_session_index] ?? null
    : null;
}

export function case52Stage5MainSnapshot(surface) {
  const projection = surface?.projection;
  const selected = selectedSessionRow(projection);
  return {
    session_id: projection?.draft_target?.sessionId ?? null,
    selected_session_id: selected?.session_id ?? null,
    primary_rows: primaryRows(projection),
    canonical_rows: canonicalRows(projection),
    visible_primary_rows: structuredClone(surface?.main?.primary_rows ?? []),
    main_draft: surface?.main?.prompt_value ?? null,
    active_turn_id: selected?.active_turn_id ?? null,
    admission_revision: selected?.admission_revision ?? null,
    latest_turn_id: selected?.latest_turn_id ?? null,
    turn_page_total: projection?.turn_page_total ?? null,
    turn_page_limit: projection?.turn_page_limit ?? null,
  };
}

export function case52Stage5MainMatches(surface, baseline) {
  const projection = surface?.projection;
  return baseline !== null && typeof baseline === "object"
    && projection?.draft_target?.sessionId === baseline.session_id
    && projection?.run_status_key === "completed"
    && projection?.task_activity_state === "idle"
    && projection?.busy === false
    && projection?.agent_tree_active === false
    && projection?.post_run_refresh_pending === false
    && projection?.navigation_loading === false
    && isDeepStrictEqual(case52Stage5MainSnapshot(surface), baseline);
}

function binding(side) {
  return {
    owner_session_id: side?.owner_session_id ?? null,
    chat_id: side?.chat_id ?? null,
    generation: side?.generation ?? null,
    draft_revision: side?.draft_revision ?? null,
    context_as_of_append_position: side?.context_as_of_append_position ?? null,
    provider_profile: side?.provider_profile ?? null,
    base_url: side?.base_url ?? null,
    model: side?.model ?? null,
    messages: (side?.messages ?? []).map(({ id, role, content }) => ({ id, role, content })),
  };
}

export function case52Stage5Ready(surface, expected) {
  const side = surface?.projection?.side_chat;
  return case52Stage5MainMatches(surface, expected?.main)
    && expected?.main?.session_id === expected.session_id
    && expected.main.selected_session_id === expected.session_id
    && side?.configured === true
    && side.owner_session_id === expected.session_id
    && typeof side.chat_id === "string" && side.chat_id.length > 0
    && side.provider_profile === expected.provider_profile
    && side.base_url === expected.provider_base_url
    && side.model === expected.model
    && side.context_scope === "owner_session"
    && typeof side.context_as_of_append_position === "string"
    && /^(0|[1-9][0-9]*)$/.test(side.context_as_of_append_position)
    && side.status === "idle"
    && side.last_error === ""
    && side.can_send === true
    && side.can_cancel === false
    && side.draft_text === ""
    && side.draft_quote === null
    && Array.isArray(side.messages) && side.messages.length === 0
    && surface?.side?.pane_count === 1
    && surface.side.pane_visible === true
    && surface.side.owner_session_id === expected.session_id
    && surface.side.prompt_value === ""
    && surface.side.pending_count === 0
    && surface.side.messages.length === 0
    && surfaceHasNoErrors(surface);
}

function answerFromSide(side) {
  return side?.messages?.findLast?.((message) => message?.role === "assistant")?.content ?? "";
}

function controlTokenLeaks(side) {
  return (side?.messages ?? []).flatMap((message, index) => {
    if (message?.role !== "assistant" || typeof message.content !== "string") return [];
    const markers = CONTROL_TOKENS.filter((marker) => message.content.includes(marker));
    return markers.length === 0 ? [] : [{
      message_index: index,
      message_id: message.id ?? null,
      markers,
      content: textIdentity(message.content),
    }];
  });
}

function displayedMessagesMatch(rendered, canonical) {
  return Array.isArray(rendered)
    && Array.isArray(canonical)
    && rendered.length === canonical.length
    && rendered.every((message, index) => {
      const source = canonical[index];
      return message?.id === source?.id
        && message?.role === source?.role
        && typeof message.content === "string"
        && message.content.trim().length > 0
        && typeof source?.content === "string"
        && source.content.trim().length > 0;
    });
}

function case52Stage5CanonicalTerminalMatches(surface, expected) {
  const side = surface?.projection?.side_chat;
  const answer = answerFromSide(side);
  return case52Stage5MainMatches(surface, expected?.main)
    && side?.configured === true
    && side.owner_session_id === expected.session_id
    && side.chat_id === expected.chat_id
    && side.generation !== expected.generation
    && side.generation !== null && side.generation !== undefined
    && side.context_scope === "owner_session"
    && side.context_as_of_append_position === expected.context_as_of_append_position
    && side.provider_profile === expected.provider_profile
    && side.base_url === expected.provider_base_url
    && side.model === expected.model
    && side.status === "completed"
    && side.last_error === ""
    && side.can_send === true
    && side.can_cancel === false
    && side.draft_text === ""
    && side.draft_quote === null
    && side.messages.length === 2
    && side.messages[0]?.role === "user"
    && side.messages[0]?.content === expected.question
    && side.messages[1]?.role === "assistant"
    && answer.trim().length > 0
    && controlTokenLeaks(side).length === 0;
}

function case52Stage5TerminalDomMatches(surface) {
  const side = surface?.projection?.side_chat;
  return surface?.side?.pane_count === 1
    && surface.side.pane_visible === true
    && surface.side.owner_session_id === side?.owner_session_id
    && surface.side.prompt_value === ""
    && surface.side.prompt_visible === true
    && surface.side.prompt_enabled === true
    && surface.side.send_visible === true
    && surface.side.send_enabled === false
    && surface.side.stop_visible === true
    && surface.side.stop_enabled === false
    && surface.side.pending_count === 0
    && displayedMessagesMatch(surface.side.messages, side.messages)
    && surfaceHasNoErrors(surface);
}

export function case52Stage5TerminalMatches(surface, expected) {
  return case52Stage5CanonicalTerminalMatches(surface, expected)
    && case52Stage5TerminalDomMatches(surface);
}

export function case52Stage5ActiveContextMatches(surface, expected) {
  const side = surface?.projection?.side_chat;
  const truncated = side?.context_truncated;
  return side?.status === "running"
    && side.owner_session_id === expected?.session_id
    && side.chat_id === expected?.chat_id
    && side.context_scope === "owner_session"
    && side.context_as_of_append_position === expected?.as_of_append_position
    && typeof truncated === "boolean"
    && surface?.side?.pane_visible === true
    && surface.side.owner_session_id === expected.session_id
    && surface.side.truncated_count === (truncated ? 1 : 0);
}

function minimizedMain(baseline) {
  return {
    session_id: baseline.session_id,
    active_turn_id: baseline.active_turn_id,
    admission_revision: baseline.admission_revision,
    latest_turn_id: baseline.latest_turn_id,
    selected_session_id: baseline.selected_session_id,
    turn_page_total: baseline.turn_page_total,
    turn_page_limit: baseline.turn_page_limit,
    main_draft: textIdentity(baseline.main_draft),
    primary_rows: baseline.primary_rows.map((row) => ({
      id: row.id,
      kind: row.kind,
      body: textIdentity(row.body),
    })),
    canonical_rows: baseline.canonical_rows.map((row) => ({
      id: row?.stable_history_identity ?? null,
      kind: row?.row_kind ?? null,
      identity: textIdentity(JSON.stringify(row)),
    })),
    visible_primary_rows: baseline.visible_primary_rows.map((row) => ({
      id: row.id,
      kind: row.kind,
      body: textIdentity(row.body),
    })),
  };
}

function expectedCancelCommand(side) {
  return {
    command: "cancel_side_chat",
    args: {
      ownerSessionId: side.owner_session_id,
      chatId: side.chat_id,
      expectedGeneration: side.generation,
    },
  };
}

function stage5ControlTokenLeakFailure(evidence) {
  const message = "stage5 exposed an OpenAI chat-template control token in an assistant transcript body";
  const observedProductFailure = {
    owner: "product",
    code: "case5_2-provider-control-token-leak",
    message,
    evidence: structuredClone(evidence),
  };
  const errors = [
    evidence.projection_error,
    evidence.screenshot_error,
    evidence.stop_error,
    evidence.command_error,
    evidence.record_error,
  ];
  const cancelSettled = evidence.cancel_required === false
    ? evidence.visible_stop_count === 0 && evidence.stop === null
    : evidence.visible_stop_count === 1 && evidence.stop !== null && evidence.terminal !== null;
  if (errors.some((error) => error !== null)
    || evidence.projection === null
    || evidence.screenshot === null
    || evidence.command_evidence === null
    || !cancelSettled) {
    return new DesktopE2eError(
      "harness",
      "case5_2-provider-control-token-leak-stop",
      `${message}, but its required evidence and conditional Side Cancel did not settle exactly`,
      { observed_product_failure: observedProductFailure, ...evidence },
    );
  }
  return productFailure(observedProductFailure.code, observedProductFailure.message, evidence);
}

async function stopStage5ControlTokenLeak({
  cdp,
  input,
  sink,
  dependencies,
  commandProbe,
  commandStart,
  expectedSubmit,
  evidenceName,
  main,
  beforeSend,
  surface,
  leaks,
  remainingMs,
}) {
  const side = surface.projection.side_chat;
  const cancelRequired = side.status === "running";
  const cancelExpected = cancelRequired ? expectedCancelCommand(side) : null;
  let projectionIdentity = null;
  let projectionError = null;
  try {
    projectionIdentity = await sink.writeJson(
      `case5_2/projections/${evidenceName}-provider-control-token-leak.json`,
      { stage: "stage5", leaks, main: minimizedMain(main), side: minimizedLeakSide(side) },
    );
  } catch (error) { projectionError = errorObservation(error); }
  let screenshot = null;
  let screenshotError = null;
  try {
    screenshot = await dependencies.screenshot({
      cdp,
      sink,
      name: `${evidenceName}-provider-control-token-leak`,
      owner: OWNER,
    });
  } catch (error) { screenshotError = errorObservation(error); }
  let stop = null;
  let stopError = null;
  let terminal = cancelRequired ? null : surface;
  let visibleStopCount = 0;
  if (cancelRequired) {
    if (side.can_cancel === true && surface.side.stop_visible === true && surface.side.stop_enabled === true) {
      try {
        stop = await dependencies.click(input, SIDE_STOP);
        visibleStopCount = 1;
        terminal = (await dependencies.waitForStage({
          label: "case5_2 Stage5 cancelled terminal after provider control-token leak",
          timeoutMs: Math.min(LEAK_STOP_TIMEOUT_MS, Math.max(1, remainingMs)),
          sample: dependencies.observe,
          accept: (candidate) => {
            const current = candidate?.projection?.side_chat;
            return case52Stage5MainMatches(candidate, main)
              && current?.owner_session_id === beforeSend.owner_session_id
              && current?.chat_id === beforeSend.chat_id
              && current?.context_scope === "owner_session"
              && current?.context_as_of_append_position === beforeSend.context_as_of_append_position
              && current?.status === "cancelled"
              && current?.can_cancel === false;
          },
          code: "case5_2-stage5-provider-control-token-leak-stop-terminal",
          message: "Stage5 Side Cancel did not settle without changing the Main owner",
        })).value;
      } catch (error) { stopError = errorObservation(error); }
    } else {
      stopError = {
        name: "Error",
        code: "case5_2-stage5-side-cancel-unavailable",
        message: "running Stage5 leak did not expose one enabled Side Cancel",
        evidence: { can_cancel: side.can_cancel, dom: surface.side },
      };
    }
  }
  let commandEvidence = null;
  let commandError = null;
  try {
    commandEvidence = assertExactDesktopCommandSequence(await commandProbe.snapshot(commandStart), {
      afterSequence: commandStart,
      expected: cancelRequired ? [expectedSubmit, cancelExpected] : [expectedSubmit],
    });
  } catch (error) { commandError = errorObservation(error); }
  const evidence = {
    stage: "stage5",
    leaks,
    projection: projectionIdentity,
    projection_error: projectionError,
    screenshot,
    screenshot_error: screenshotError,
    cancel_required: cancelRequired,
    visible_stop_count: visibleStopCount,
    stop,
    stop_error: stopError,
    terminal: terminal === null ? null : minimizedLeakSide(terminal.projection.side_chat),
    command_evidence: commandEvidence,
    command_error: commandError,
    record_error: null,
  };
  try {
    await sink.record("case5_2-provider-control-token-leak", evidence, { phase: "executing", owner: OWNER });
  } catch (error) { evidence.record_error = errorObservation(error); }
  throw stage5ControlTokenLeakFailure(evidence);
}

function minimizedSide(side) {
  return {
    ...binding(side),
    status: side.status,
    context_scope: side.context_scope,
    context_truncated: side.context_truncated,
    messages: side.messages.map((message) => ({
      id: message.id,
      role: message.role,
      content: textIdentity(message.content),
    })),
  };
}

function minimizedTerminalDom(side) {
  return {
    pane_count: side?.pane_count ?? null,
    pane_visible: side?.pane_visible ?? null,
    owner_session_id: side?.owner_session_id ?? null,
    prompt_value: textIdentity(side?.prompt_value ?? ""),
    prompt_visible: side?.prompt_visible ?? null,
    prompt_enabled: side?.prompt_enabled ?? null,
    send_visible: side?.send_visible ?? null,
    send_enabled: side?.send_enabled ?? null,
    stop_visible: side?.stop_visible ?? null,
    stop_enabled: side?.stop_enabled ?? null,
    pending_count: side?.pending_count ?? null,
    messages: (side?.messages ?? []).map((message) => ({
      id: message?.id ?? null,
      role: message?.role ?? null,
      content: textIdentity(message?.content ?? ""),
    })),
  };
}

function terminalDomSettleFailure(surface, expected, elapsedMs) {
  return productFailure(
    "case5_2-stage5-terminal-dom-settle-timeout",
    "Stage5 Side Chat persisted a valid completed answer, but its rendered messages or final controls did not settle",
    {
      grace_ms: TERMINAL_DOM_SETTLE_GRACE_MS,
      elapsed_ms: elapsedMs,
      expected: {
        session_id: expected.session_id,
        chat_id: expected.chat_id,
        provider_profile: expected.provider_profile,
        provider_base_url: expected.provider_base_url,
        model: expected.model,
        question: textIdentity(expected.question),
      },
      observed: minimizedSide(surface?.projection?.side_chat),
      dom: minimizedTerminalDom(surface?.side),
    },
  );
}

function minimizedLeakSide(side) {
  return {
    owner_session_id: side?.owner_session_id ?? null,
    chat_id: side?.chat_id ?? null,
    generation: side?.generation ?? null,
    draft_revision: side?.draft_revision ?? null,
    context_as_of_append_position: side?.context_as_of_append_position ?? null,
    status: side?.status ?? null,
    context_scope: side?.context_scope ?? null,
    context_truncated: side?.context_truncated ?? null,
    messages: (side?.messages ?? []).map((message) => ({
      id: message?.id ?? null,
      role: message?.role ?? null,
      content: textIdentity(message?.content ?? ""),
    })),
  };
}

function errorObservation(error) {
  return {
    name: error?.name ?? "Error",
    code: error?.code ?? null,
    message: error?.message ?? String(error),
    evidence: error?.evidence ?? null,
  };
}

export function classifyCase52Stage5CommandError(error, phase) {
  if (typeof phase !== "string" || phase.length === 0) {
    throw new TypeError("case5_2 Stage5 command phase is required");
  }
  if (!new Set([
    "desktop-command-probe-cardinality",
    "desktop-command-probe-call-mismatch",
  ]).has(error?.code)) return error;
  return productFailure(
    "case5_2-stage5-command-mismatch",
    `Stage5 ${phase} command sequence did not match the exact Side-only contract`,
    { phase, command_error: errorObservation(error) },
  );
}

function assertStage5CommandSequence(snapshot, options, phase) {
  try {
    return assertExactDesktopCommandSequence(snapshot, options);
  } catch (error) {
    throw classifyCase52Stage5CommandError(error, phase);
  }
}

async function attemptBeforeDeadline(operation, timeoutMs, label) {
  let timer;
  try {
    return await Promise.race([
      operation(),
      new Promise((_, reject) => {
        timer = setTimeout(() => {
          const error = new Error(`${label} exceeded ${timeoutMs}ms`);
          error.code = "case5_2-stage5-observation-attempt-timeout";
          reject(error);
        }, timeoutMs);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

function validateArguments(options) {
  if (options === null || typeof options !== "object") throw new TypeError("case5_2 Stage5 options are required");
  for (const key of ["cdp", "input", "sink"]) {
    if (options[key] === null || typeof options[key] !== "object") throw new TypeError(`case5_2 Stage5 ${key} is required`);
  }
  for (const key of ["sessionId", "providerProfile", "providerBaseUrl", "model"]) {
    if (typeof options[key] !== "string" || options[key].trim().length === 0) throw new TypeError(`case5_2 Stage5 ${key} is required`);
  }
  if (typeof options.promptInput?.text !== "string" || options.promptInput.text.trim().length === 0) {
    throw new TypeError("case5_2 Stage5 promptInput.text is required");
  }
  const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
  if (!Number.isInteger(timeoutMs) || timeoutMs < 1_000 || timeoutMs > DEFAULT_TIMEOUT_MS) {
    throw new TypeError("case5_2 Stage5 timeoutMs must be between 1000 and 3600000");
  }
  const evidenceName = options.evidenceName ?? "case5_2-stage5";
  if (!EVIDENCE_NAME.test(evidenceName)) throw new TypeError("case5_2 Stage5 evidenceName is invalid");
  if (options.commandProbe !== undefined && options.commandProbe !== null
    && (typeof options.commandProbe !== "object"
      || typeof options.commandProbe.snapshot !== "function")) {
    throw new TypeError("case5_2 Stage5 commandProbe must expose snapshot when provided");
  }
  return { timeoutMs, evidenceName };
}

function defaultDependencies(cdp) {
  return {
    now: () => Date.now(),
    sleep: (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds)),
    observe: () => observeSideChatQuoteSurface(cdp),
    waitForStage: waitForProductStage,
    click: trustedClick,
    insert: trustedInsert,
    createCommandProbe: () => new DesktopCommandProbe(cdp, {
      probeId: "case5-2-stage5-side-chat-commands",
      commands: ["submit_side_chat", "cancel_side_chat", "submit_prompt", "cancel_run"],
    }),
    screenshot: captureScenarioScreenshot,
  };
}

export async function executeCase52SideChatStage(options, injected = {}) {
  const { timeoutMs, evidenceName } = validateArguments(options);
  const {
    cdp, input, sink, sessionId, providerProfile, providerBaseUrl, model, promptInput,
    commandProbe: suppliedCommandProbe = null,
  } = options;
  const submittedQuestion = promptInput.text.trim();
  const dependencies = { ...defaultDependencies(cdp), ...injected };
  const started = dependencies.now();
  const deadline = started + timeoutMs;
  let commandProbe = suppliedCommandProbe;
  let ownsCommandProbe = false;
  let primaryError = null;
  let cleanupError = null;
  let outcome = null;
  try {
    let initial = await dependencies.observe();
    if (!initial?.side?.pane_visible) {
      await dependencies.click(input, SHOW_SIDE);
      initial = (await dependencies.waitForStage({
        label: "case5_2 Stage5 Side Chat pane",
        timeoutMs: Math.min(30_000, Math.max(1, deadline - dependencies.now())),
        sample: dependencies.observe,
        accept: (surface) => surface?.side?.pane_count === 1 && surface.side.pane_visible === true,
        code: "case5_2-stage5-side-pane",
        message: "Stage5 did not expose the selected session Side Chat pane",
      })).value;
    }
    const main = case52Stage5MainSnapshot(initial);
    const expectedReady = {
      main,
      session_id: sessionId,
      provider_profile: providerProfile,
      provider_base_url: providerBaseUrl,
      model,
    };
    if (!case52Stage5Ready(initial, expectedReady)) {
      throw productFailure(
        "case5_2-stage5-not-ready",
        "Stage5 must start with one configured, idle, empty Side Chat bound to the selected session",
        { expected: expectedReady, observed: { main: minimizedMain(main), side: initial?.projection?.side_chat ?? null } },
      );
    }
    await dependencies.insert(input, SIDE_PROMPT, promptInput.text);
    const drafted = (await dependencies.waitForStage({
      label: "case5_2 Stage5 Side question draft",
      timeoutMs: Math.min(30_000, Math.max(1, deadline - dependencies.now())),
      sample: dependencies.observe,
      accept: (surface) => case52Stage5MainMatches(surface, main)
        && surface?.projection?.side_chat?.draft_text === promptInput.text
        && surface.projection.side_chat.draft_quote === null
        && surface.side.prompt_value === promptInput.text
        && surface.side.send_enabled === true
        && surfaceHasNoErrors(surface),
      code: "case5_2-stage5-question-draft",
      message: "Stage5 Side question did not settle without changing the Main owner",
    })).value;
    const beforeSend = drafted.projection.side_chat;
    const expectedCommand = {
      command: "submit_side_chat",
      args: {
        ownerSessionId: beforeSend.owner_session_id,
        chatId: beforeSend.chat_id,
        expectedGeneration: beforeSend.generation,
        expectedDraftRevision: beforeSend.draft_revision,
        expectedOwnerAppendPosition: beforeSend.context_as_of_append_position,
        quote: null,
        text: submittedQuestion,
      },
    };
    if (commandProbe === null) {
      commandProbe = dependencies.createCommandProbe();
      await commandProbe.install();
      ownsCommandProbe = true;
    }
    const commandStart = (await commandProbe.snapshot()).sequence;
    const sendStarted = dependencies.now();
    const send = await dependencies.click(input, SIDE_SEND);
    const admittedCommand = await dependencies.waitForStage({
      label: "case5_2 Stage5 exact Side submit command",
      timeoutMs: Math.min(COMMAND_ADMISSION_TIMEOUT_MS, Math.max(1, deadline - dependencies.now())),
      sample: () => commandProbe.snapshot(commandStart),
      accept: (snapshot) => Array.isArray(snapshot?.calls) && snapshot.calls.length >= 1,
      code: "case5_2-stage5-submit-command",
      message: "trusted Stage5 Side Send did not invoke one bounded submit_side_chat command",
    });
    const admissionCommandEvidence = assertStage5CommandSequence(admittedCommand.value, {
      afterSequence: commandStart,
      expected: [expectedCommand],
    }, "admission");
    const expectedTerminal = {
      main,
      session_id: sessionId,
      chat_id: beforeSend.chat_id,
      provider_profile: providerProfile,
      provider_base_url: providerBaseUrl,
      model,
      question: submittedQuestion,
      generation: beforeSend.generation,
      context_as_of_append_position: beforeSend.context_as_of_append_position,
    };
    let activeContextObservation = null;
    let firstProgressLatencyMs = null;
    let terminal = null;
    let terminalDomSettleStarted = null;
    let terminalDomSettleMs = 0;
    let lastTerminalDomMismatch = null;
    let nextProgressAt = sendStarted + PROGRESS_EVIDENCE_MS;
    while (dependencies.now() < deadline) {
      if (terminalDomSettleStarted !== null
        && dependencies.now() - terminalDomSettleStarted >= TERMINAL_DOM_SETTLE_GRACE_MS) {
        throw terminalDomSettleFailure(
          lastTerminalDomMismatch,
          expectedTerminal,
          Math.max(0, dependencies.now() - terminalDomSettleStarted),
        );
      }
      const terminalDomDeadline = terminalDomSettleStarted === null
        ? deadline
        : Math.min(deadline, terminalDomSettleStarted + TERMINAL_DOM_SETTLE_GRACE_MS);
      const remaining = Math.max(1, terminalDomDeadline - dependencies.now());
      const surface = await attemptBeforeDeadline(
        dependencies.observe,
        Math.min(MAX_OBSERVATION_ATTEMPT_MS, remaining),
        "case5_2 Stage5 Side Chat observation",
      );
      if (!case52Stage5MainMatches(surface, main)) {
        throw productFailure(
          "case5_2-stage5-main-owner-drift",
          "Stage5 Side Chat changed the selected Main session, canonical history, page owner, or draft",
          { expected: minimizedMain(main), observed: minimizedMain(case52Stage5MainSnapshot(surface)) },
        );
      }
      const side = surface?.projection?.side_chat;
      if (side?.owner_session_id !== sessionId || side?.chat_id !== beforeSend.chat_id) {
        throw productFailure(
          "case5_2-stage5-side-owner-drift",
          "Stage5 Side Chat left the exact selected session conversation",
          { expected: binding(beforeSend), observed: binding(side) },
        );
      }
      if (side.context_scope !== "owner_session"
        || side.context_as_of_append_position !== beforeSend.context_as_of_append_position) {
        throw productFailure(
          "case5_2-stage5-context-fence-drift",
          "Stage5 Side Chat changed its exact owner-session context fence",
          { expected: binding(beforeSend), observed: binding(side) },
        );
      }
      if (side.status === "running" && activeContextObservation === null) {
        if (!case52Stage5ActiveContextMatches(surface, {
          session_id: sessionId,
          chat_id: beforeSend.chat_id,
          as_of_append_position: beforeSend.context_as_of_append_position,
        })) {
          throw productFailure(
            "case5_2-stage5-active-context",
            "Stage5 running state did not expose the exact owner-session context fence and truncation projection",
            { expected_owner: binding(beforeSend), observed: minimizedSide(side), dom: surface.side },
          );
        }
        activeContextObservation = {
          observed_at_elapsed_ms: Math.max(0, dependencies.now() - sendStarted),
          scope: side.context_scope ?? null,
          as_of_append_position: side.context_as_of_append_position ?? null,
          truncated: side.context_truncated ?? null,
          dom_metadata: structuredClone(surface.side.metadata ?? []),
          dom_truncated_count: surface.side.truncated_count ?? null,
        };
      }
      const answer = answerFromSide(side);
      if (answer.length > 0 && firstProgressLatencyMs === null) {
        firstProgressLatencyMs = Math.max(0, dependencies.now() - sendStarted);
      }
      const leaks = controlTokenLeaks(side);
      if (leaks.length > 0) {
        await stopStage5ControlTokenLeak({
          cdp,
          input,
          sink,
          dependencies,
          commandProbe,
          commandStart,
          expectedSubmit: expectedCommand,
          evidenceName,
          main,
          beforeSend,
          surface,
          leaks,
          remainingMs: Math.max(1, deadline - dependencies.now()),
        });
      }
      if (["failed", "cancelled"].includes(side?.status) || (side?.last_error ?? "") !== "") {
        throw productFailure(
          "case5_2-stage5-terminal-failure",
          "Stage5 Side Chat reached a failure terminal instead of answering",
          { side: minimizedSide(side), last_error: side?.last_error ?? null },
        );
      }
      if (case52Stage5TerminalMatches(surface, expectedTerminal)) {
        terminal = surface;
        firstProgressLatencyMs ??= Math.max(0, dependencies.now() - sendStarted);
        terminalDomSettleMs = terminalDomSettleStarted === null
          ? 0
          : Math.max(0, dependencies.now() - terminalDomSettleStarted);
        break;
      }
      if (side.status === "completed") {
        if (!case52Stage5CanonicalTerminalMatches(surface, expectedTerminal)) {
          throw productFailure(
            "case5_2-stage5-terminal-mismatch",
            "Stage5 Side Chat completed with malformed persisted question, answer, binding, or generation data",
            { expected: expectedTerminal, observed: minimizedSide(side), dom: minimizedTerminalDom(surface.side) },
          );
        }
        terminalDomSettleStarted ??= dependencies.now();
        lastTerminalDomMismatch = surface;
        if (dependencies.now() - terminalDomSettleStarted >= TERMINAL_DOM_SETTLE_GRACE_MS) {
          throw terminalDomSettleFailure(
            surface,
            expectedTerminal,
            Math.max(0, dependencies.now() - terminalDomSettleStarted),
          );
        }
      } else if (terminalDomSettleStarted !== null) {
        throw productFailure(
          "case5_2-stage5-terminal-mismatch",
          "Stage5 Side Chat regressed after persisting a completed canonical answer",
          { expected: expectedTerminal, observed: minimizedSide(side), dom: minimizedTerminalDom(surface.side) },
        );
      }
      if (side.status === "idle" && dependencies.now() - sendStarted >= REQUEST_START_TIMEOUT_MS) {
        throw productFailure(
          "case5_2-stage5-request-not-started",
          "admitted Stage5 Side Chat request remained idle without progress",
          { elapsed_ms: dependencies.now() - sendStarted, admission_command_evidence: admissionCommandEvidence },
        );
      }
      if (dependencies.now() >= nextProgressAt) {
        await sink.record("case5_2-stage5-progress", {
          elapsed_ms: Math.max(0, dependencies.now() - sendStarted),
          side: minimizedSide(side),
          active_context_observation: activeContextObservation,
          first_progress_latency_ms: firstProgressLatencyMs,
        }, { phase: "executing", owner: OWNER });
        nextProgressAt += PROGRESS_EVIDENCE_MS;
      }
      const sleepDeadline = terminalDomSettleStarted === null
        ? deadline
        : Math.min(deadline, terminalDomSettleStarted + TERMINAL_DOM_SETTLE_GRACE_MS);
      await dependencies.sleep(Math.min(POLL_MS, Math.max(1, sleepDeadline - dependencies.now())));
    }
    if (terminal === null) {
      if (terminalDomSettleStarted !== null) {
        throw terminalDomSettleFailure(
          lastTerminalDomMismatch,
          expectedTerminal,
          Math.max(0, dependencies.now() - terminalDomSettleStarted),
        );
      }
      throw productFailure(
        "case5_2-stage5-timeout",
        `Stage5 Side Chat did not complete within ${timeoutMs}ms`,
        { active_context_observation: activeContextObservation, first_progress_latency_ms: firstProgressLatencyMs },
      );
    }
    const commandEvidence = assertStage5CommandSequence(await commandProbe.snapshot(commandStart), {
      afterSequence: commandStart,
      expected: [expectedCommand],
    }, "terminal");
    const answer = answerFromSide(terminal.projection.side_chat);
    const terminalScreenshot = await dependencies.screenshot({
      cdp,
      sink,
      name: `${evidenceName}-terminal`,
      owner: OWNER,
    });
    const elapsedMs = Math.max(0, dependencies.now() - started);
    const savedEvidence = {
      schema_version: "desktop-e2e.case5_2-stage5-side-chat.v1",
      stage: "stage5",
      elapsed_ms: elapsedMs,
      first_progress_latency_ms: firstProgressLatencyMs,
      question: { text: submittedQuestion, ...textIdentity(submittedQuestion) },
      question_input: {
        ...textIdentity(promptInput.text),
        trimmed_for_submission: submittedQuestion !== promptInput.text,
      },
      answer: { text: answer, ...textIdentity(answer) },
      main: minimizedMain(main),
      binding: binding(terminal.projection.side_chat),
      side: minimizedSide(terminal.projection.side_chat),
      active_context_observation: activeContextObservation,
      terminal_dom_settle_ms: terminalDomSettleMs,
      terminal_context_note: "terminal context_truncated=false is not evidence that the active request fit without truncation",
      command_evidence: commandEvidence,
      admission_command_evidence: admissionCommandEvidence,
      screenshot: terminalScreenshot,
    };
    const projectionIdentity = await sink.writeJson(
      `case5_2/projections/${evidenceName}-terminal.json`,
      savedEvidence,
    );
    await sink.record("case5_2-stage5-side-chat-terminal", {
      ...savedEvidence,
      projection: projectionIdentity,
    }, { phase: "executing", owner: OWNER });
    outcome = {
      stage: "stage5",
      elapsed_ms: elapsedMs,
      send,
      commandEvidence,
      completedSurface: terminal,
      mainBaseline: main,
      binding: binding(terminal.projection.side_chat),
      question: submittedQuestion,
      answer,
      active_context_observation: activeContextObservation,
      first_progress_latency_ms: firstProgressLatencyMs,
      terminal_dom_settle_ms: terminalDomSettleMs,
      terminal_screenshot: terminalScreenshot,
      projection: projectionIdentity,
    };
  } catch (error) {
    primaryError = error;
  } finally {
    if (ownsCommandProbe && commandProbe !== null) {
      try { await commandProbe.remove(); }
      catch (error) { cleanupError = error; }
    }
  }
  if (primaryError !== null) {
    if (cleanupError !== null) {
      const prior = primaryError.evidence !== null && typeof primaryError.evidence === "object"
        ? primaryError.evidence
        : { primary_evidence: primaryError.evidence ?? null };
      primaryError.evidence = { ...prior, command_probe_cleanup_error: errorObservation(cleanupError) };
    }
    throw primaryError;
  }
  if (cleanupError !== null) {
    throw new DesktopE2eError(
      "harness",
      "case5_2-stage5-command-probe-cleanup",
      "Stage5 completed but its command probe did not settle",
      errorObservation(cleanupError),
    );
  }
  return outcome;
}

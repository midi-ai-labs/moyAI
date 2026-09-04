import crypto from "node:crypto";
import http from "node:http";

export const SCRIPTED_PROVIDER_MODEL_ID = "e2e/scripted-responses";
export const SCRIPTED_PROVIDER_PROMPT = "return only MAIN_OK";
export const SCRIPTED_PROVIDER_RESPONSE = "MAIN_OK";
export const SCRIPTED_PROVIDER_MAX_BODY_BYTES = 1_048_576;
export const SCRIPTED_PROVIDER_MAX_OUTPUT_TOKENS = 1_024;
export const SCRIPTED_PROVIDER_MAX_TURNS = 64;
export const SCRIPTED_PROVIDER_AGENT_INTERRUPT_KIND = "agent_interrupt";
export const SCRIPTED_PROVIDER_AGENT_INTERRUPT_MAX_RESPONSES = 3;
export const SCRIPTED_PROVIDER_AGENT_INTERRUPT_TASK_NAME = "interrupt_target";
export const SCRIPTED_PROVIDER_AGENT_INTERRUPT_MESSAGE = "remain active until the user interrupts this exact child";
export const SCRIPTED_PROVIDER_AGENT_INTERRUPT_ROOT_RESPONSE = "ROOT_OK";
export const SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_KIND = "tool_error_recovery";
export const SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_MAX_RESPONSES = 2;
export const SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_KIND = "permission_restart_guardian";
export const SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_MAX_RESPONSES = 4;
export const SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_API_MODES = Object.freeze([
  "responses",
  "chat_completions",
]);
export const SCRIPTED_PROVIDER_PERMISSION_TEMP_ESCALATION_KIND = "permission_temp_escalation";
export const SCRIPTED_PROVIDER_PERMISSION_TEMP_ESCALATION_MAX_RESPONSES = 4;
export const SCRIPTED_PROVIDER_PERMISSION_TEMP_ESCALATION_MAX_GUARDIAN_DELAY_MS = 300_000;
export const SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_KIND = "chat_tool_continuation";
export const SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_MAX_RESPONSES = 2;
export const SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT =
  "Use current_time exactly once with {}. After the tool result, reply only CHAT_TOOL_CONTINUATION_OK.";
export const SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_RESPONSE = "CHAT_TOOL_CONTINUATION_OK";
export const SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_CALL_ID = "call_chat_current_time";
export const SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_KIND = "side_chat_quote";
export const SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_MAX_RESPONSES = 4;
export const SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_MAIN_PROMPT =
  "Create the requested quote artifact, then return the exact settled quote response.";
export const SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_TRANSCRIPT_SELECTION =
  "TRANSCRIPT_QUOTE_SOURCE_7F3A9C";
export const SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_MAIN_RESPONSE =
  `Settled assistant source: ${SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_TRANSCRIPT_SELECTION}`;
export const SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_ARTIFACT_PATH =
  "side-chat-quote-source.txt";
export const SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_ARTIFACT_CONTENT =
  "canonical side chat artifact source\n";
export const SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_FIRST_RESPONSE =
  "SIDE_CHAT_TRANSCRIPT_QUOTE_OK";
export const SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_KIND = "side_chat_session";
export const SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_MAX_RESPONSES = 4;
export const SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_PROMPT =
  "セッションAの決定事項: テーマカラーは青、公開日は金曜日です。この内容を記録して要約してください。";
export const SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_RESPONSE =
  "セッションA: テーマカラーは青、公開日は金曜日です。";
export const SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_BETA_PROMPT =
  "セッションBの決定事項: テーマカラーは赤、公開日は月曜日です。この内容を記録して要約してください。";
export const SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_BETA_RESPONSE =
  "セッションB: テーマカラーは赤、公開日は月曜日です。";
export const SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_MAIN_AFTER_CONFIG_PROMPT =
  "Side Chat設定後のメインチャット確認です。サイド用の追加指示を参照せず、この文だけを確認してください。";
export const SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_MAIN_AFTER_CONFIG_RESPONSE =
  "メインチャットはSide Chat設定後も独立しています。";
export const SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_QUESTION =
  "このセッションで決めたテーマカラーと公開日を教えてください。";
export const SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_SIDE_RESPONSE =
  "このセッションの決定事項は、テーマカラーが青、公開日が金曜日です。";
export const SCRIPTED_PROVIDER_RESPONSES_COMPACTION_KIND = "responses_compaction_retry";
export const SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT = 8;
export const SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES =
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT + 3;
export const SCRIPTED_PROVIDER_RESPONSES_COMPACTION_SATURATION_PROMPT_TOKENS = 13_000;
export const SCRIPTED_PROVIDER_RESPONSES_COMPACTION_SATURATION_TOTAL_TOKENS = 31_129;
export const SCRIPTED_PROVIDER_RESPONSES_COMPACTION_SENTINEL =
  "E2E_RESPONSES_COMPACTION_RETRY.txt";
export const SCRIPTED_PROVIDER_RESPONSES_COMPACTION_PROMPT =
  `Read ${SCRIPTED_PROVIDER_RESPONSES_COMPACTION_SENTINEL} one line at a time with read offsets 1 through ${SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT} and limit 1. After every line is observed, reply only RESPONSES_COMPACTION_RETRY_OK.`;
export const SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RESPONSE =
  "RESPONSES_COMPACTION_RETRY_OK";
export const SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RAW_REASONING_SENTINEL =
  "E2E_PRIVATE_REASONING_MUST_NOT_REACH_TRANSCRIPT_7F3A9C";
export const SCRIPTED_PROVIDER_RESPONSES_COMPACTION_CHECKPOINT = `## Objective and exact contract
Complete the deterministic bounded compaction GUI regression and return its exact final response.

## Observed changes and remaining state
The bounded read units selected by this checkpoint were observed; later read units and the final response remain pending.

## Exact failures and retry guards
The larger compaction request exhausted host-owned reasoning without answer text and must not be repeated unchanged.

## Open interactions and ownership boundaries
The provider owns generation behavior while moyAI owns semantic-unit selection, canonical compaction, and tool replay.

## Next falsifying actions
Resume from this checkpoint, retain complete read call/output units, and return the exact requested final response.

## Evidence coverage
The selected read units and smaller retry were observed; canonical commit, remaining context, and final GUI completion still require verification.`;
export const SCRIPTED_PROVIDER_COMPACTION_HEADINGS = Object.freeze([
  "## Objective and exact contract",
  "## Observed changes and remaining state",
  "## Exact failures and retry guards",
  "## Open interactions and ownership boundaries",
  "## Next falsifying actions",
  "## Evidence coverage",
]);
export const SCRIPTED_PROVIDER_COMPACTION_SUMMARY_PREFIX =
  "Another language model started to solve this problem and produced a summary of its thinking process. You also have access to the state of the tools that were used by that language model. Use this to build on the work that has already been done and avoid duplicating work. Here is the summary produced by the other language model, use the information in this summary to assist with your own analysis:";
export const SCRIPTED_PROVIDER_RESPONSE_BEHAVIORS = Object.freeze([
  "complete",
  "hold_until_release",
  "hold_until_peer_close",
]);
export const SCRIPTED_PROVIDER_RESPONSE_PACING_LIMITS = Object.freeze({
  maximum_cadence_ms: 5_000,
  maximum_delta_count: 32,
  maximum_total_duration_ms: 30_000,
});

const LOOPBACK_HOST = "127.0.0.1";
const FETCH_FORBIDDEN_PORTS = new Set([
  1, 7, 9, 11, 13, 15, 17, 19, 20, 21, 22, 23, 25, 37, 42, 43, 53, 69, 77, 79,
  87, 95, 101, 102, 103, 104, 109, 110, 111, 113, 115, 117, 119, 123, 135, 137,
  139, 143, 161, 179, 389, 427, 465, 512, 513, 514, 515, 526, 530, 531, 532, 540,
  548, 554, 556, 563, 587, 601, 636, 989, 990, 993, 995, 1_719, 1_720, 1_723,
  2_049, 3_659, 4_045, 5_060, 5_061, 6_000, 6_566, 6_665, 6_666, 6_667, 6_668,
  6_669, 6_697, 10_080,
]);
const MAX_FETCH_SAFE_BIND_ATTEMPTS = 16;

export function scriptedProviderPortIsFetchSafe(port) {
  return Number.isInteger(port)
    && port >= 1
    && port <= 65_535
    && !FETCH_FORBIDDEN_PORTS.has(port);
}

function sha256(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

function positiveInteger(value, name) {
  if (!Number.isSafeInteger(value) || value <= 0) throw new TypeError(`${name} must be a positive safe integer`);
  return value;
}

function nonNegativeInteger(value, name) {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new TypeError(`${name} must be a non-negative safe integer`);
  }
  return value;
}

function optionalHttpStatus(value, name) {
  if (value === null || value === undefined) return null;
  if (!Number.isInteger(value) || value < 200 || value > 599) {
    throw new TypeError(`${name} must be null or an HTTP status from 200 through 599`);
  }
  return value;
}

function nonEmptyString(value, name) {
  if (typeof value !== "string" || value.trim().length === 0) throw new TypeError(`${name} must be a non-empty string`);
  return value;
}

function scriptedTurns(value, fallbackPrompt, fallbackResponseText) {
  if (value === null || value === undefined) {
    return Object.freeze([Object.freeze({ prompt: fallbackPrompt, responseText: fallbackResponseText })]);
  }
  if (!Array.isArray(value) || value.length < 1 || value.length > SCRIPTED_PROVIDER_MAX_TURNS) {
    throw new TypeError(`scripted provider turns must contain 1 through ${SCRIPTED_PROVIDER_MAX_TURNS} entries`);
  }
  return Object.freeze(value.map((turn, index) => {
    if (!exactKeys(turn, ["prompt", "responseText"])) {
      throw new TypeError(`scripted provider turn ${index} must use its exact schema`);
    }
    return Object.freeze({
      prompt: nonEmptyString(turn.prompt, `turns[${index}].prompt`),
      responseText: nonEmptyString(turn.responseText, `turns[${index}].responseText`),
    });
  }));
}

function responseBytes(value) {
  return Buffer.from(typeof value === "string" ? value : `${JSON.stringify(value)}\n`, "utf8");
}

function responseBehavior(value) {
  if (!SCRIPTED_PROVIDER_RESPONSE_BEHAVIORS.includes(value)) {
    throw new TypeError(`unknown scripted provider response behavior: ${value}`);
  }
  return value;
}

function responsePacing(value) {
  if (value === null || value === undefined) return null;
  if (!exactKeys(value, ["cadenceMs", "deltaCount"])) {
    throw new TypeError("scripted provider responsePacing must use its exact schema");
  }
  const cadenceMs = positiveInteger(value.cadenceMs, "responsePacing.cadenceMs");
  const deltaCount = positiveInteger(value.deltaCount, "responsePacing.deltaCount");
  if (cadenceMs > SCRIPTED_PROVIDER_RESPONSE_PACING_LIMITS.maximum_cadence_ms) {
    throw new TypeError(
      `responsePacing.cadenceMs must not exceed ${SCRIPTED_PROVIDER_RESPONSE_PACING_LIMITS.maximum_cadence_ms}`,
    );
  }
  if (deltaCount < 2 || deltaCount > SCRIPTED_PROVIDER_RESPONSE_PACING_LIMITS.maximum_delta_count) {
    throw new TypeError(
      `responsePacing.deltaCount must be between 2 and ${SCRIPTED_PROVIDER_RESPONSE_PACING_LIMITS.maximum_delta_count}`,
    );
  }
  const totalDurationMs = cadenceMs * (deltaCount + 1);
  if (!Number.isSafeInteger(totalDurationMs)
    || totalDurationMs > SCRIPTED_PROVIDER_RESPONSE_PACING_LIMITS.maximum_total_duration_ms) {
    throw new TypeError(
      `responsePacing total duration must not exceed ${SCRIPTED_PROVIDER_RESPONSE_PACING_LIMITS.maximum_total_duration_ms}ms`,
    );
  }
  return Object.freeze({ cadenceMs, deltaCount, totalDurationMs });
}

function exactKeys(value, expected) {
  return value !== null
    && typeof value === "object"
    && !Array.isArray(value)
    && JSON.stringify(Object.keys(value).sort()) === JSON.stringify([...expected].sort());
}

function agentInterruptScript(value) {
  if (value === null || value === undefined) return null;
  const expectedKeys = ["childMessage", "childTaskName", "kind", "rootResponseText"];
  if (!exactKeys(value, expectedKeys) || value.kind !== SCRIPTED_PROVIDER_AGENT_INTERRUPT_KIND) {
    throw new TypeError("agent interrupt scripted provider mode must use its exact schema");
  }
  const childTaskName = nonEmptyString(value.childTaskName, "script.childTaskName");
  if (!/^[a-z0-9_]+$/.test(childTaskName)) {
    throw new TypeError("script.childTaskName must use lowercase letters, digits, and underscores");
  }
  return Object.freeze({
    kind: value.kind,
    childTaskName,
    childMessage: nonEmptyString(value.childMessage, "script.childMessage"),
    rootResponseText: nonEmptyString(value.rootResponseText, "script.rootResponseText"),
  });
}

function normalizedRelativePath(value, name) {
  const path = nonEmptyString(value, name);
  const segments = path.split("/");
  if (path.includes("\\")
    || path.startsWith("/")
    || /^[a-zA-Z]:/.test(path)
    || segments.some((segment) => segment.length === 0 || segment === "." || segment === "..")) {
    throw new TypeError(`${name} must be a normalized relative path without traversal`);
  }
  return path;
}

function knownMissingRelativePath(value, name) {
  return normalizedRelativePath(value, name);
}

function toolErrorRecoveryScript(value) {
  const expectedKeys = ["kind", "missingPath", "responseText", "streamedPrefix"];
  if (!exactKeys(value, expectedKeys) || value.kind !== SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_KIND) {
    throw new TypeError("tool error recovery scripted provider mode must use its exact schema");
  }
  const responseText = nonEmptyString(value.responseText, "script.responseText");
  const streamedPrefix = nonEmptyString(value.streamedPrefix, "script.streamedPrefix");
  if (streamedPrefix === responseText || !responseText.startsWith(streamedPrefix)) {
    throw new TypeError("script.streamedPrefix must be a strict prefix of script.responseText");
  }
  return Object.freeze({
    kind: value.kind,
    missingPath: knownMissingRelativePath(value.missingPath, "script.missingPath"),
    responseText,
    streamedPrefix,
  });
}

function permissionRestartGuardianScript(value) {
  const expectedKeys = [
    "apiMode",
    "command",
    "justification",
    "kind",
    "responseText",
    "seedPrompt",
    "seedResponseText",
    "taskPrompt",
  ];
  if (!exactKeys(value, expectedKeys)
    || value.kind !== SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_KIND) {
    throw new TypeError("permission restart Guardian scripted provider mode must use its exact schema");
  }
  if (!SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_API_MODES.includes(value.apiMode)) {
    throw new TypeError("permission restart Guardian script.apiMode must use a supported wire mode");
  }
  return Object.freeze({
    kind: value.kind,
    apiMode: value.apiMode,
    seedPrompt: nonEmptyString(value.seedPrompt, "script.seedPrompt"),
    seedResponseText: nonEmptyString(value.seedResponseText, "script.seedResponseText"),
    taskPrompt: nonEmptyString(value.taskPrompt, "script.taskPrompt"),
    command: nonEmptyString(value.command, "script.command"),
    justification: nonEmptyString(value.justification, "script.justification"),
    responseText: nonEmptyString(value.responseText, "script.responseText"),
  });
}

function permissionTempEscalationScript(value) {
  const expectedKeys = [
    "command",
    "guardianDelayMs",
    "justification",
    "kind",
    "responseText",
    "taskPrompt",
  ];
  if (!exactKeys(value, expectedKeys)
    || value.kind !== SCRIPTED_PROVIDER_PERMISSION_TEMP_ESCALATION_KIND) {
    throw new TypeError("permission TEMP escalation scripted provider mode must use its exact schema");
  }
  const guardianDelayMs = nonNegativeInteger(
    value.guardianDelayMs,
    "script.guardianDelayMs",
  );
  if (guardianDelayMs > SCRIPTED_PROVIDER_PERMISSION_TEMP_ESCALATION_MAX_GUARDIAN_DELAY_MS) {
    throw new TypeError(
      `script.guardianDelayMs must not exceed ${SCRIPTED_PROVIDER_PERMISSION_TEMP_ESCALATION_MAX_GUARDIAN_DELAY_MS}`,
    );
  }
  return Object.freeze({
    kind: value.kind,
    taskPrompt: nonEmptyString(value.taskPrompt, "script.taskPrompt"),
    command: nonEmptyString(value.command, "script.command"),
    justification: nonEmptyString(value.justification, "script.justification"),
    responseText: nonEmptyString(value.responseText, "script.responseText"),
    guardianDelayMs,
  });
}

function chatToolContinuationScript(value) {
  const expectedKeys = ["kind"];
  if (!exactKeys(value, expectedKeys)
    || value.kind !== SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_KIND) {
    throw new TypeError("Chat tool continuation scripted provider mode must use its exact schema");
  }
  return Object.freeze({ kind: value.kind });
}

function sideChatQuoteScript(value) {
  const expectedKeys = ["kind"];
  if (!exactKeys(value, expectedKeys)
    || value.kind !== SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_KIND) {
    throw new TypeError("Side Chat quote scripted provider mode must use its exact schema");
  }
  return Object.freeze({ kind: value.kind });
}

function sideChatSessionScript(value) {
  if (!exactKeys(value, ["expectedConsultSystemPromptMarker", "kind"])
    || value.kind !== SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_KIND) {
    throw new TypeError("Side Chat session scripted provider mode must use its exact schema");
  }
  return Object.freeze({
    kind: value.kind,
    expectedConsultSystemPromptMarker: nonEmptyString(
      value.expectedConsultSystemPromptMarker,
      "script.expectedConsultSystemPromptMarker",
    ),
  });
}

function responsesCompactionScript(value) {
  const expectedKeys = [
    "checkpointText",
    "inputByteThreshold",
    "kind",
    "responseText",
    "sentinelPath",
    "toolCallCount",
  ];
  if (!exactKeys(value, expectedKeys)
    || value.kind !== SCRIPTED_PROVIDER_RESPONSES_COMPACTION_KIND) {
    throw new TypeError("Responses compaction retry scripted provider mode must use its exact schema");
  }
  const toolCallCount = positiveInteger(value.toolCallCount, "script.toolCallCount");
  if (toolCallCount < 3 || toolCallCount > 16) {
    throw new TypeError("script.toolCallCount must be between 3 and 16");
  }
  const inputByteThreshold = positiveInteger(
    value.inputByteThreshold,
    "script.inputByteThreshold",
  );
  if (inputByteThreshold >= SCRIPTED_PROVIDER_MAX_BODY_BYTES) {
    throw new TypeError("script.inputByteThreshold must be below the provider body limit");
  }
  return Object.freeze({
    kind: value.kind,
    sentinelPath: normalizedRelativePath(value.sentinelPath, "script.sentinelPath"),
    toolCallCount,
    inputByteThreshold,
    checkpointText: nonEmptyString(value.checkpointText, "script.checkpointText"),
    responseText: nonEmptyString(value.responseText, "script.responseText"),
  });
}

function providerScript(value) {
  if (value === null || value === undefined) return null;
  if (value?.kind === SCRIPTED_PROVIDER_AGENT_INTERRUPT_KIND) return agentInterruptScript(value);
  if (value?.kind === SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_KIND) return toolErrorRecoveryScript(value);
  if (value?.kind === SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_KIND) {
    return permissionRestartGuardianScript(value);
  }
  if (value?.kind === SCRIPTED_PROVIDER_PERMISSION_TEMP_ESCALATION_KIND) {
    return permissionTempEscalationScript(value);
  }
  if (value?.kind === SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_KIND) {
    return chatToolContinuationScript(value);
  }
  if (value?.kind === SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_KIND) {
    return sideChatQuoteScript(value);
  }
  if (value?.kind === SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_KIND) {
    return sideChatSessionScript(value);
  }
  if (value?.kind === SCRIPTED_PROVIDER_RESPONSES_COMPACTION_KIND) {
    return responsesCompactionScript(value);
  }
  throw new TypeError("scripted provider mode must use a known kind and its exact schema");
}

function scriptedResponseMaximum(script) {
  if (script?.kind === SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_KIND) {
    return SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_MAX_RESPONSES;
  }
  if (script?.kind === SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_KIND) {
    return SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_MAX_RESPONSES;
  }
  if (script?.kind === SCRIPTED_PROVIDER_PERMISSION_TEMP_ESCALATION_KIND) {
    return SCRIPTED_PROVIDER_PERMISSION_TEMP_ESCALATION_MAX_RESPONSES;
  }
  if (script?.kind === SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_KIND) {
    return SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_MAX_RESPONSES;
  }
  if (script?.kind === SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_KIND) {
    return SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_MAX_RESPONSES;
  }
  if (script?.kind === SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_KIND) {
    return SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_MAX_RESPONSES;
  }
  if (script?.kind === SCRIPTED_PROVIDER_RESPONSES_COMPACTION_KIND) {
    return script.toolCallCount + 3;
  }
  return SCRIPTED_PROVIDER_AGENT_INTERRUPT_MAX_RESPONSES;
}

export function createAgentInterruptProviderScript({
  childTaskName = SCRIPTED_PROVIDER_AGENT_INTERRUPT_TASK_NAME,
  childMessage = SCRIPTED_PROVIDER_AGENT_INTERRUPT_MESSAGE,
  rootResponseText = SCRIPTED_PROVIDER_AGENT_INTERRUPT_ROOT_RESPONSE,
} = {}) {
  return agentInterruptScript({
    kind: SCRIPTED_PROVIDER_AGENT_INTERRUPT_KIND,
    childTaskName,
    childMessage,
    rootResponseText,
  });
}

export function createToolErrorRecoveryProviderScript({
  missingPath,
  responseText,
  streamedPrefix,
} = {}) {
  return toolErrorRecoveryScript({
    kind: SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_KIND,
    missingPath,
    responseText,
    streamedPrefix,
  });
}

export function createPermissionRestartGuardianProviderScript({
  apiMode = "responses",
  seedPrompt,
  seedResponseText,
  taskPrompt,
  command,
  justification,
  responseText,
} = {}) {
  return permissionRestartGuardianScript({
    kind: SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_KIND,
    apiMode,
    seedPrompt,
    seedResponseText,
    taskPrompt,
    command,
    justification,
    responseText,
  });
}

export function createPermissionTempEscalationProviderScript({
  taskPrompt,
  command,
  justification,
  responseText,
  guardianDelayMs = 0,
} = {}) {
  return permissionTempEscalationScript({
    kind: SCRIPTED_PROVIDER_PERMISSION_TEMP_ESCALATION_KIND,
    taskPrompt,
    command,
    justification,
    responseText,
    guardianDelayMs,
  });
}

export function createChatToolContinuationProviderScript() {
  return chatToolContinuationScript({
    kind: SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_KIND,
  });
}

export function createSideChatQuoteProviderScript() {
  return sideChatQuoteScript({ kind: SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_KIND });
}

export function createSideChatSessionProviderScript({ expectedConsultSystemPromptMarker } = {}) {
  return sideChatSessionScript({
    kind: SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_KIND,
    expectedConsultSystemPromptMarker,
  });
}

export function createResponsesCompactionProviderScript({
  sentinelPath = SCRIPTED_PROVIDER_RESPONSES_COMPACTION_SENTINEL,
  toolCallCount = SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT,
  inputByteThreshold,
  checkpointText = SCRIPTED_PROVIDER_RESPONSES_COMPACTION_CHECKPOINT,
  responseText = SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RESPONSE,
} = {}) {
  return responsesCompactionScript({
    kind: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_KIND,
    sentinelPath,
    toolCallCount,
    inputByteThreshold,
    checkpointText,
    responseText,
  });
}

function writeResponse(response, status, contentType, value, extraHeaders = {}) {
  const bytes = responseBytes(value);
  response.sendDate = false;
  response.writeHead(status, {
    "cache-control": "no-store",
    connection: "close",
    "content-length": String(bytes.byteLength),
    "content-type": contentType,
    ...extraHeaders,
  });
  response.end(bytes);
}

function fixedError(response, status, code, extraHeaders = {}) {
  writeResponse(response, status, "application/json; charset=utf-8", { error: code }, extraHeaders);
}

function requestHeaderObservation(headers) {
  const names = Object.keys(headers).map((name) => name.toLowerCase()).sort();
  const declared = headers["content-length"];
  const declaredLength = typeof declared === "string" && /^\d+$/.test(declared)
    ? Number(declared)
    : null;
  const contentType = headers["content-type"];
  return {
    header_names_sha256: sha256(Buffer.from(names.join("\n"), "utf8")),
    authorization: Object.hasOwn(headers, "authorization") ? "<redacted>" : null,
    content_type_is_application_json: typeof contentType === "string"
      && contentType.split(";", 1)[0].trim().toLowerCase() === "application/json",
    declared_content_length: Number.isSafeInteger(declaredLength) ? declaredLength : null,
  };
}

function readBoundedBody(request, maximumBytes) {
  return new Promise((resolve, reject) => {
    const digest = crypto.createHash("sha256");
    const chunks = [];
    let sizeBytes = 0;
    let limitExceeded = false;
    request.on("data", (chunk) => {
      const bytes = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      sizeBytes += bytes.byteLength;
      digest.update(bytes);
      if (!limitExceeded && sizeBytes <= maximumBytes) chunks.push(bytes);
      else {
        limitExceeded = true;
        chunks.length = 0;
      }
    });
    request.once("end", () => {
      resolve({
        bytes: limitExceeded ? null : Buffer.concat(chunks, sizeBytes),
        size_bytes: sizeBytes,
        sha256: digest.digest("hex"),
        limit_exceeded: limitExceeded,
      });
    });
    request.once("aborted", () => reject(new Error("scripted provider request body was aborted")));
    request.once("error", reject);
  });
}

function decodeJsonBody(body) {
  if (body.limit_exceeded || body.bytes === null) {
    return { utf8_valid: null, json_valid: false, value: null };
  }
  let text;
  try {
    text = new TextDecoder("utf-8", { fatal: true }).decode(body.bytes);
  } catch {
    return { utf8_valid: false, json_valid: false, value: null };
  }
  try {
    return { utf8_valid: true, json_valid: true, value: JSON.parse(text) };
  } catch {
    return { utf8_valid: true, json_valid: false, value: null };
  }
}

function exactUserInput(body) {
  if (!Array.isArray(body?.input) || body.input.length !== 1) return null;
  const [message] = body.input;
  if (message?.type !== "message" || message?.role !== "user") return null;
  if (!Array.isArray(message.content) || message.content.length !== 1) return null;
  const [content] = message.content;
  if (content?.type !== "input_text" || typeof content.text !== "string") return null;
  return content.text;
}

const EXPECTED_RESPONSES_KEYS = Object.freeze([
  "input",
  "instructions",
  "model",
  "store",
  "stream",
]);
export const SCRIPTED_PROVIDER_CLIENT_GENERATION_KEYS = Object.freeze([
  "chat_template_kwargs",
  "enable_thinking",
  "extra_body",
  "extra_body_json",
  "frequency_penalty",
  "max_output_tokens",
  "max_tokens",
  "min_p",
  "num_ctx",
  "presence_penalty",
  "reasoning",
  "reasoning_effort",
  "reasoning_summary",
  "seed",
  "stop",
  "stop_sequences",
  "temperature",
  "top_k",
  "top_p",
]);
const FORBIDDEN_RESPONSES_KEYS = Object.freeze([
  ...SCRIPTED_PROVIDER_CLIENT_GENERATION_KEYS,
  "messages",
  "parallel_tool_calls",
  "previous_response_id",
  "tool_choice",
  "tools",
]);
const TOOL_RESPONSES_KEYS = Object.freeze([
  "input",
  "instructions",
  "model",
  "parallel_tool_calls",
  "store",
  "stream",
  "tool_choice",
  "tools",
]);
const AGENT_INTERRUPT_SPAWN_CALL_ID = "call_agent_interrupt_spawn";
const AGENT_INTERRUPT_SPAWN_ITEM_ID = "fc_agent_interrupt_spawn";
const TOOL_ERROR_RECOVERY_READ_CALL_ID = "call_tool_error_recovery_read";
const TOOL_ERROR_RECOVERY_READ_ITEM_ID = "fc_tool_error_recovery_read";
const SIDE_CHAT_QUOTE_WRITE_CALL_ID = "call_side_chat_quote_write";
const SIDE_CHAT_QUOTE_WRITE_ITEM_ID = "fc_side_chat_quote_write";
const PERMISSION_RESTART_GUARDIAN_SHELL_CALL_ID = "call_permission_restart_guardian_shell";
const PERMISSION_RESTART_GUARDIAN_SHELL_ITEM_ID = "fc_permission_restart_guardian_shell";
const PERMISSION_TEMP_ESCALATION_RESTRICTED_CALL_ID =
  "call_permission_temp_escalation_restricted";
const PERMISSION_TEMP_ESCALATION_RESTRICTED_ITEM_ID =
  "fc_permission_temp_escalation_restricted";
const PERMISSION_TEMP_ESCALATION_ELEVATED_CALL_ID =
  "call_permission_temp_escalation_elevated";
const PERMISSION_TEMP_ESCALATION_ELEVATED_ITEM_ID =
  "fc_permission_temp_escalation_elevated";
const PERMISSION_RESTART_GUARDIAN_ALLOW = Object.freeze({
  decision: "allow",
  rationale: "bounded deterministic fixture command",
});
const GUARDIAN_RESPONSES_KEYS = Object.freeze([
  "input",
  "instructions",
  "model",
  "store",
  "stream",
]);
const GUARDIAN_CHAT_KEYS = Object.freeze([
  "messages",
  "model",
  "n",
  "stream",
  "stream_options",
]);
const GUARDIAN_CHAT_SAMPLING_KEYS = Object.freeze([
  "frequency_penalty",
  "min_p",
  "presence_penalty",
  "seed",
  "stop",
  "stop_sequences",
  "temperature",
  "top_k",
  "top_p",
]);
const GUARDIAN_CHAT_REASONING_KEYS = Object.freeze([
  "chat_template_kwargs",
  "enable_thinking",
  "reasoning",
  "reasoning_effort",
  "reasoning_summary",
]);
const CHAT_TOOL_CONTINUATION_KEYS = Object.freeze([
  "messages",
  "model",
  "n",
  "parallel_tool_calls",
  "stream",
  "stream_options",
  "tools",
]);
const CHAT_TOOL_OUTPUT_MAX_BYTES = 512;

function clientGenerationContract(body) {
  const present = SCRIPTED_PROVIDER_CLIENT_GENERATION_KEYS.filter((key) => Object.hasOwn(body ?? {}, key));
  return {
    client_generation_fields_present: present,
    client_generation_fields_absent: present.length === 0,
  };
}

function requestContract(body, modelId, expectedPrompt) {
  const model = typeof body?.model === "string" ? body.model : null;
  const inputText = exactUserInput(body);
  const instructions = typeof body?.instructions === "string" ? body.instructions : null;
  const topLevelKeys = body !== null && typeof body === "object" && !Array.isArray(body)
    ? Object.keys(body).sort()
    : [];
  const forbiddenFieldsPresent = FORBIDDEN_RESPONSES_KEYS.filter((key) => Object.hasOwn(body ?? {}, key));
  const generation = clientGenerationContract(body);
  const contract = {
    ...generation,
    model_sha256: model === null ? null : sha256(Buffer.from(model, "utf8")),
    input_text_sha256: inputText === null ? null : sha256(Buffer.from(inputText, "utf8")),
    instructions_sha256: instructions === null ? null : sha256(Buffer.from(instructions, "utf8")),
    top_level_keys: topLevelKeys,
    forbidden_fields_present: forbiddenFieldsPresent,
    model_matches: model === modelId,
    input_matches: inputText === expectedPrompt,
    instructions_non_empty: instructions !== null && instructions.trim().length > 0,
    top_level_keys_match: JSON.stringify(topLevelKeys) === JSON.stringify(EXPECTED_RESPONSES_KEYS),
    max_output_tokens_absent: !Object.hasOwn(body ?? {}, "max_output_tokens"),
    stream_true: body?.stream === true,
    store_false: body?.store === false,
  };
  return {
    ...contract,
    pass: contract.client_generation_fields_absent
      && contract.model_matches
      && contract.input_matches
      && contract.instructions_non_empty
      && contract.top_level_keys_match
      && contract.max_output_tokens_absent
      && contract.stream_true
      && contract.store_false,
  };
}

function exactOutputText(item) {
  if (!exactKeys(item, ["content", "role", "type"])
    || item.type !== "message"
    || item.role !== "assistant"
    || !Array.isArray(item.content)
    || item.content.length !== 1) return null;
  const [content] = item.content;
  if (!exactKeys(content, ["text", "type"])
    || content.type !== "output_text"
    || typeof content.text !== "string") return null;
  return content.text;
}

function orderedConversationInputContract(input, turns, currentTurnIndex) {
  const expectedCount = currentTurnIndex * 2 + 1;
  const items = Array.isArray(input) ? input : [];
  const roles = items.map((item) => typeof item?.role === "string" ? item.role : null);
  const textHashes = items.map((item) => {
    const text = item?.role === "assistant" ? exactOutputText(item) : exactInputText(item);
    return text === null ? null : sha256(Buffer.from(text, "utf8"));
  });
  let matches = items.length === expectedCount;
  for (let index = 0; matches && index < currentTurnIndex; index += 1) {
    matches = exactInputText(items[index * 2]) === turns[index].prompt
      && exactOutputText(items[index * 2 + 1]) === turns[index].responseText;
  }
  matches = matches
    && exactInputText(items[expectedCount - 1]) === turns[currentTurnIndex].prompt;
  return {
    input_count: items.length,
    expected_input_count: expectedCount,
    roles,
    text_sha256: textHashes,
    matches,
  };
}

function orderedConversationRequestContract(
  body,
  modelId,
  turns,
  currentTurnIndex,
) {
  const model = typeof body?.model === "string" ? body.model : null;
  const instructions = typeof body?.instructions === "string" ? body.instructions : null;
  const topLevelKeys = body !== null && typeof body === "object" && !Array.isArray(body)
    ? Object.keys(body).sort()
    : [];
  const forbiddenFieldsPresent = FORBIDDEN_RESPONSES_KEYS.filter((key) => Object.hasOwn(body ?? {}, key));
  const conversation = orderedConversationInputContract(body?.input, turns, currentTurnIndex);
  const generation = clientGenerationContract(body);
  const contract = {
    ...generation,
    ordered_conversation: conversation,
    model_sha256: model === null ? null : sha256(Buffer.from(model, "utf8")),
    instructions_sha256: instructions === null ? null : sha256(Buffer.from(instructions, "utf8")),
    top_level_keys: topLevelKeys,
    forbidden_fields_present: forbiddenFieldsPresent,
    model_matches: model === modelId,
    instructions_non_empty: instructions !== null && instructions.trim().length > 0,
    top_level_keys_match: JSON.stringify(topLevelKeys) === JSON.stringify(EXPECTED_RESPONSES_KEYS),
    max_output_tokens_absent: !Object.hasOwn(body ?? {}, "max_output_tokens"),
    stream_true: body?.stream === true,
    store_false: body?.store === false,
  };
  return {
    ...contract,
    pass: contract.client_generation_fields_absent
      && conversation.matches
      && contract.model_matches
      && contract.instructions_non_empty
      && contract.top_level_keys_match
      && contract.max_output_tokens_absent
      && contract.stream_true
      && contract.store_false,
  };
}

function exactInputText(item) {
  if (!exactKeys(item, ["content", "role", "type"])
    || item.type !== "message"
    || item.role !== "user"
    || !Array.isArray(item.content)
    || item.content.length !== 1) return null;
  const [content] = item.content;
  if (!exactKeys(content, ["text", "type"])
    || content.type !== "input_text"
    || typeof content.text !== "string") return null;
  return content.text;
}

function spawnToolSchemaPass(tool) {
  const parameters = tool?.parameters;
  const properties = parameters?.properties;
  return exactKeys(tool, ["description", "name", "parameters", "type"])
    && tool.type === "function"
    && tool.name === "spawn_agent"
    && typeof tool.description === "string"
    && tool.description.trim().length > 0
    && exactKeys(parameters, ["additionalProperties", "properties", "required", "type"])
    && parameters.type === "object"
    && parameters.additionalProperties === false
    && Array.isArray(parameters.required)
    && JSON.stringify(parameters.required) === JSON.stringify(["task_name", "message"])
    && exactKeys(properties, ["fork_turns", "message", "task_name"])
    && properties.task_name?.type === "string"
    && properties.message?.type === "string"
    && properties.fork_turns?.type === "string";
}

function toolsContract(tools) {
  const names = Array.isArray(tools)
    ? tools.map((tool) => typeof tool?.name === "string" ? tool.name : null)
    : [];
  const genericShapePass = Array.isArray(tools)
    && tools.length > 0
    && tools.every((tool) => exactKeys(tool, ["description", "name", "parameters", "type"])
      && tool.type === "function"
      && typeof tool.name === "string"
      && tool.name.length > 0
      && typeof tool.description === "string"
      && tool.description.length > 0
      && tool.parameters !== null
      && typeof tool.parameters === "object"
      && !Array.isArray(tool.parameters));
  const uniqueNames = names.every((name) => name !== null) && new Set(names).size === names.length;
  const spawnTools = Array.isArray(tools) ? tools.filter((tool) => tool?.name === "spawn_agent") : [];
  const pass = genericShapePass
    && uniqueNames
    && spawnTools.length === 1
    && spawnToolSchemaPass(spawnTools[0]);
  return {
    tool_count: Array.isArray(tools) ? tools.length : null,
    tool_names_sha256: names.every((name) => name !== null)
      ? sha256(Buffer.from([...names].sort().join("\n"), "utf8"))
      : null,
    unique_tool_names: uniqueNames,
    spawn_agent_present: spawnTools.length === 1,
    spawn_agent_schema_matches: spawnTools.length === 1 && spawnToolSchemaPass(spawnTools[0]),
    pass,
  };
}

function readToolSchemaPass(tool) {
  const parameters = tool?.parameters;
  const properties = parameters?.properties;
  return exactKeys(tool, ["description", "name", "parameters", "type"])
    && tool.type === "function"
    && tool.name === "read"
    && typeof tool.description === "string"
    && tool.description.trim().length > 0
    && exactKeys(parameters, ["properties", "required", "type"])
    && parameters.type === "object"
    && Array.isArray(parameters.required)
    && JSON.stringify(parameters.required) === JSON.stringify(["path"])
    && exactKeys(properties, ["limit", "offset", "path"])
    && properties.path?.type === "string"
    && properties.offset?.type === "integer"
    && properties.limit?.type === "integer";
}

function readToolsContract(tools) {
  const names = Array.isArray(tools)
    ? tools.map((tool) => typeof tool?.name === "string" ? tool.name : null)
    : [];
  const genericShapePass = Array.isArray(tools)
    && tools.length > 0
    && tools.every((tool) => exactKeys(tool, ["description", "name", "parameters", "type"])
      && tool.type === "function"
      && typeof tool.name === "string"
      && tool.name.length > 0
      && typeof tool.description === "string"
      && tool.description.length > 0
      && tool.parameters !== null
      && typeof tool.parameters === "object"
      && !Array.isArray(tool.parameters));
  const uniqueNames = names.every((name) => name !== null) && new Set(names).size === names.length;
  const readTools = Array.isArray(tools) ? tools.filter((tool) => tool?.name === "read") : [];
  const pass = genericShapePass
    && uniqueNames
    && readTools.length === 1
    && readToolSchemaPass(readTools[0]);
  return {
    tool_count: Array.isArray(tools) ? tools.length : null,
    tool_names_sha256: names.every((name) => name !== null)
      ? sha256(Buffer.from([...names].sort().join("\n"), "utf8"))
      : null,
    unique_tool_names: uniqueNames,
    read_present: readTools.length === 1,
    read_schema_matches: readTools.length === 1 && readToolSchemaPass(readTools[0]),
    pass,
  };
}

function writeToolSchemaPass(tool) {
  const parameters = tool?.parameters;
  const properties = parameters?.properties;
  return exactKeys(tool, ["description", "name", "parameters", "type"])
    && tool.type === "function"
    && tool.name === "write"
    && typeof tool.description === "string"
    && tool.description.trim().length > 0
    && exactKeys(parameters, ["properties", "required", "type"])
    && parameters.type === "object"
    && Array.isArray(parameters.required)
    && JSON.stringify(parameters.required) === JSON.stringify(["path", "content"])
    && exactKeys(properties, ["content", "path"])
    && properties.path?.type === "string"
    && properties.content?.type === "string";
}

function writeToolsContract(tools) {
  const names = Array.isArray(tools)
    ? tools.map((tool) => typeof tool?.name === "string" ? tool.name : null)
    : [];
  const genericShapePass = Array.isArray(tools)
    && tools.length > 0
    && tools.every((tool) => exactKeys(tool, ["description", "name", "parameters", "type"])
      && tool.type === "function"
      && typeof tool.name === "string"
      && tool.name.length > 0
      && typeof tool.description === "string"
      && tool.description.length > 0
      && tool.parameters !== null
      && typeof tool.parameters === "object"
      && !Array.isArray(tool.parameters));
  const uniqueNames = names.every((name) => name !== null) && new Set(names).size === names.length;
  const writeTools = Array.isArray(tools) ? tools.filter((tool) => tool?.name === "write") : [];
  return {
    tool_count: Array.isArray(tools) ? tools.length : null,
    tool_names_sha256: names.every((name) => name !== null)
      ? sha256(Buffer.from([...names].sort().join("\n"), "utf8"))
      : null,
    unique_tool_names: uniqueNames,
    write_present: writeTools.length === 1,
    write_schema_matches: writeTools.length === 1 && writeToolSchemaPass(writeTools[0]),
    pass: genericShapePass
      && uniqueNames
      && writeTools.length === 1
      && writeToolSchemaPass(writeTools[0]),
  };
}

function shellToolSchemaPass(tool) {
  const parameters = tool?.parameters;
  const properties = parameters?.properties;
  return exactKeys(tool, ["description", "name", "parameters", "type"])
    && tool.type === "function"
    && tool.name === "shell"
    && typeof tool.description === "string"
    && tool.description.trim().length > 0
    && exactKeys(parameters, ["properties", "required", "type"])
    && parameters.type === "object"
    && Array.isArray(parameters.required)
    && JSON.stringify(parameters.required) === JSON.stringify(["command"])
    && exactKeys(properties, [
      "command",
      "description",
      "justification",
      "sandbox_permissions",
      "timeout_ms",
      "workdir",
    ])
    && properties.command?.type === "string"
    && properties.description?.type === "string"
    && properties.justification?.type === "string"
    && properties.timeout_ms?.type === "integer"
    && properties.workdir?.type === "string"
    && properties.sandbox_permissions?.type === "string"
    && JSON.stringify(properties.sandbox_permissions.enum)
      === JSON.stringify(["use_default", "require_escalated"]);
}

function shellToolsContract(tools) {
  const names = Array.isArray(tools)
    ? tools.map((tool) => typeof tool?.name === "string" ? tool.name : null)
    : [];
  const genericShapePass = Array.isArray(tools)
    && tools.length > 0
    && tools.every((tool) => exactKeys(tool, ["description", "name", "parameters", "type"])
      && tool.type === "function"
      && typeof tool.name === "string"
      && tool.name.length > 0
      && typeof tool.description === "string"
      && tool.description.length > 0
      && tool.parameters !== null
      && typeof tool.parameters === "object"
      && !Array.isArray(tool.parameters));
  const uniqueNames = names.every((name) => name !== null) && new Set(names).size === names.length;
  const shellTools = Array.isArray(tools) ? tools.filter((tool) => tool?.name === "shell") : [];
  const pass = genericShapePass
    && uniqueNames
    && shellTools.length === 1
    && shellToolSchemaPass(shellTools[0]);
  return {
    tool_count: Array.isArray(tools) ? tools.length : null,
    tool_names_sha256: names.every((name) => name !== null)
      ? sha256(Buffer.from([...names].sort().join("\n"), "utf8"))
      : null,
    unique_tool_names: uniqueNames,
    shell_present: shellTools.length === 1,
    shell_schema_matches: shellTools.length === 1 && shellToolSchemaPass(shellTools[0]),
    pass,
  };
}

function currentTimeToolSchemaPass(tool) {
  const definition = tool?.function;
  const parameters = definition?.parameters;
  return exactKeys(tool, ["function", "type"])
    && tool.type === "function"
    && exactKeys(definition, ["description", "name", "parameters"])
    && definition.name === "current_time"
    && typeof definition.description === "string"
    && definition.description.trim().length > 0
    && exactKeys(parameters, ["properties", "type"])
    && parameters.type === "object"
    && exactKeys(parameters.properties, []);
}

function chatToolsContract(tools) {
  const functions = Array.isArray(tools) ? tools.map((tool) => tool?.function) : [];
  const names = functions.map((definition) => typeof definition?.name === "string"
    ? definition.name
    : null);
  const genericShapePass = Array.isArray(tools)
    && tools.length > 0
    && tools.every((tool) => exactKeys(tool, ["function", "type"])
      && tool.type === "function"
      && exactKeys(tool.function, ["description", "name", "parameters"])
      && typeof tool.function.name === "string"
      && tool.function.name.length > 0
      && typeof tool.function.description === "string"
      && tool.function.description.length > 0
      && tool.function.parameters !== null
      && typeof tool.function.parameters === "object"
      && !Array.isArray(tool.function.parameters));
  const uniqueNames = names.every((name) => name !== null) && new Set(names).size === names.length;
  const currentTimeTools = Array.isArray(tools)
    ? tools.filter((tool) => tool?.function?.name === "current_time")
    : [];
  return {
    tool_count: Array.isArray(tools) ? tools.length : null,
    tool_names_sha256: names.every((name) => name !== null)
      ? sha256(Buffer.from([...names].sort().join("\n"), "utf8"))
      : null,
    unique_tool_names: uniqueNames,
    current_time_present: currentTimeTools.length === 1,
    current_time_schema_matches: currentTimeTools.length === 1
      && currentTimeToolSchemaPass(currentTimeTools[0]),
    pass: genericShapePass
      && uniqueNames
      && currentTimeTools.length === 1
      && currentTimeToolSchemaPass(currentTimeTools[0]),
  };
}

function chatShellToolSchemaPass(tool) {
  return exactKeys(tool, ["function", "type"])
    && tool.type === "function"
    && shellToolSchemaPass({ ...tool.function, type: tool.type });
}

function chatShellToolsContract(tools) {
  const functions = Array.isArray(tools) ? tools.map((tool) => tool?.function) : [];
  const names = functions.map((definition) => typeof definition?.name === "string"
    ? definition.name
    : null);
  const uniqueNames = names.every((name) => name !== null) && new Set(names).size === names.length;
  const shellTools = Array.isArray(tools)
    ? tools.filter((tool) => tool?.function?.name === "shell")
    : [];
  const pass = Array.isArray(tools)
    && tools.length > 0
    && tools.every((tool) => exactKeys(tool, ["function", "type"])
      && tool.type === "function"
      && tool.function !== null
      && typeof tool.function === "object"
      && !Array.isArray(tool.function))
    && uniqueNames
    && shellTools.length === 1
    && chatShellToolSchemaPass(shellTools[0]);
  return {
    tool_count: Array.isArray(tools) ? tools.length : null,
    tool_names_sha256: names.every((name) => name !== null)
      ? sha256(Buffer.from([...names].sort().join("\n"), "utf8"))
      : null,
    unique_tool_names: uniqueNames,
    shell_present: shellTools.length === 1,
    shell_schema_matches: shellTools.length === 1 && chatShellToolSchemaPass(shellTools[0]),
    pass,
  };
}

function exactChatTextMessage(message, role) {
  return exactKeys(message, ["content", "role"])
    && message.role === role
    && typeof message.content === "string"
    ? message.content
    : null;
}

function currentTimeToolOutputShape(value) {
  if (typeof value !== "string") return false;
  const sizeBytes = Buffer.byteLength(value, "utf8");
  if (sizeBytes < 1 || sizeBytes > CHAT_TOOL_OUTPUT_MAX_BYTES) return false;
  return /^local: \d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}[+-]\d{2}:\d{2}\r?\nutc: \d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z\r?\ntimezone: [+-]\d{2}:\d{2}\r?\nunix_ms: \d{10,16}$/.test(value);
}

function chatToolContinuationRole(body) {
  const messages = Array.isArray(body?.messages) ? body.messages : [];
  const systemText = exactChatTextMessage(messages[0], "system");
  const userText = exactChatTextMessage(messages[1], "user");
  const assistant = messages[2];
  const tool = messages[3];
  const toolCall = Array.isArray(assistant?.tool_calls) && assistant.tool_calls.length === 1
    ? assistant.tool_calls[0]
    : null;
  const callMatches = exactKeys(assistant, ["role", "tool_calls"])
    && assistant.role === "assistant"
    && !Object.hasOwn(assistant, "content")
    && exactKeys(toolCall, ["function", "id", "type"])
    && toolCall.id === SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_CALL_ID
    && toolCall.type === "function"
    && exactKeys(toolCall.function, ["arguments", "name"])
    && toolCall.function.name === "current_time"
    && toolCall.function.arguments === "{}";
  const toolOutput = exactKeys(tool, ["content", "role", "tool_call_id"])
    && tool.role === "tool"
    && tool.tool_call_id === SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_CALL_ID
    && typeof tool.content === "string"
    ? tool.content
    : null;
  const initial = messages.length === 2
    && systemText !== null
    && systemText.trim().length > 0
    && userText === SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT;
  const continuation = messages.length === 4
    && systemText !== null
    && systemText.trim().length > 0
    && userText === SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT
    && callMatches
    && currentTimeToolOutputShape(toolOutput);
  return {
    role: initial ? "chat_tool_initial" : continuation ? "chat_continuation" : null,
    evidence: {
      message_count: messages.length,
      message_roles: messages.map((message) => typeof message?.role === "string" ? message.role : null),
      system_content_sha256: systemText === null ? null : sha256(Buffer.from(systemText, "utf8")),
      system_content_non_empty: systemText !== null && systemText.trim().length > 0,
      user_content_sha256: userText === null ? null : sha256(Buffer.from(userText, "utf8")),
      user_prompt_matches: userText === SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_PROMPT,
      assistant_content_absent: assistant !== null
        && typeof assistant === "object"
        && !Array.isArray(assistant)
        && !Object.hasOwn(assistant, "content"),
      current_time_call_matches: callMatches,
      tool_output_shape_matches: currentTimeToolOutputShape(toolOutput),
      tool_output_size_bytes: toolOutput === null ? null : Buffer.byteLength(toolOutput, "utf8"),
      tool_output_sha256: toolOutput === null ? null : sha256(Buffer.from(toolOutput, "utf8")),
    },
  };
}

function chatToolContinuationRequestContract(body, modelId, script) {
  const model = typeof body?.model === "string" ? body.model : null;
  const topLevelKeys = body !== null && typeof body === "object" && !Array.isArray(body)
    ? Object.keys(body).sort()
    : [];
  const classified = chatToolContinuationRole(body);
  const tools = chatToolsContract(body?.tools);
  const generation = clientGenerationContract(body);
  const contract = {
    ...generation,
    script_kind: script.kind,
    role: classified.role,
    role_evidence: classified.evidence,
    model_sha256: model === null ? null : sha256(Buffer.from(model, "utf8")),
    top_level_keys: topLevelKeys,
    model_matches: model === modelId,
    top_level_keys_match: JSON.stringify(topLevelKeys) === JSON.stringify(CHAT_TOOL_CONTINUATION_KEYS),
    stream_true: body?.stream === true,
    include_usage_true: exactKeys(body?.stream_options, ["include_usage"])
      && body.stream_options.include_usage === true,
    n_one: body?.n === 1,
    max_tokens_absent: !Object.hasOwn(body ?? {}, "max_tokens"),
    parallel_tool_calls_false: body?.parallel_tool_calls === false,
    tools,
  };
  return {
    ...contract,
    pass: contract.client_generation_fields_absent
      && contract.role !== null
      && contract.model_matches
      && contract.top_level_keys_match
      && contract.stream_true
      && contract.include_usage_true
      && contract.n_one
      && contract.max_tokens_absent
      && contract.parallel_tool_calls_false
      && contract.tools.pass,
  };
}

function agentInterruptExpected(script) {
  const childPath = `/root/${script.childTaskName}`;
  const spawnArguments = JSON.stringify({
    task_name: script.childTaskName,
    message: script.childMessage,
    fork_turns: "none",
  });
  return {
    childPath,
    childEnvelope: `Message Type: NEW_TASK\nTask name: ${childPath}\nSender: /root\nPayload:\n${script.childMessage}`,
    spawnArguments,
    spawnOutput: JSON.stringify({ task_name: childPath }),
  };
}

function agentInterruptRole(body, expectedPrompt, script) {
  const input = Array.isArray(body?.input) ? body.input : [];
  const expected = agentInterruptExpected(script);
  const itemTypes = input.map((item) => typeof item?.type === "string" ? item.type : null);
  const firstInputText = exactInputText(input[0]);
  const call = input[1];
  const output = input[2];
  const rootInitial = input.length === 1 && firstInputText === expectedPrompt;
  const child = input.length === 1 && firstInputText === expected.childEnvelope;
  const rootContinuation = input.length === 3
    && firstInputText === expectedPrompt
    && exactKeys(call, ["arguments", "call_id", "name", "type"])
    && call.type === "function_call"
    && call.call_id === AGENT_INTERRUPT_SPAWN_CALL_ID
    && call.name === "spawn_agent"
    && call.arguments === expected.spawnArguments
    && exactKeys(output, ["call_id", "output", "type"])
    && output.type === "function_call_output"
    && output.call_id === AGENT_INTERRUPT_SPAWN_CALL_ID
    && output.output === expected.spawnOutput;
  const role = rootInitial
    ? "root_initial"
    : child
      ? "child_held"
      : rootContinuation
        ? "root_continuation"
        : null;
  return {
    role,
    evidence: {
      input_count: input.length,
      input_item_types: itemTypes,
      first_input_text_sha256: firstInputText === null
        ? null
        : sha256(Buffer.from(firstInputText, "utf8")),
      expected_prompt_matches: firstInputText === expectedPrompt,
      child_envelope_matches: firstInputText === expected.childEnvelope,
      spawn_call_matches: rootContinuation,
    },
  };
}

function agentInterruptRequestContract(body, modelId, expectedPrompt, script) {
  const model = typeof body?.model === "string" ? body.model : null;
  const instructions = typeof body?.instructions === "string" ? body.instructions : null;
  const topLevelKeys = body !== null && typeof body === "object" && !Array.isArray(body)
    ? Object.keys(body).sort()
    : [];
  const tools = toolsContract(body?.tools);
  const classified = agentInterruptRole(body, expectedPrompt, script);
  const generation = clientGenerationContract(body);
  const contract = {
    ...generation,
    script_kind: script.kind,
    role: classified.role,
    role_evidence: classified.evidence,
    model_sha256: model === null ? null : sha256(Buffer.from(model, "utf8")),
    instructions_sha256: instructions === null ? null : sha256(Buffer.from(instructions, "utf8")),
    top_level_keys: topLevelKeys,
    model_matches: model === modelId,
    instructions_non_empty: instructions !== null && instructions.trim().length > 0,
    top_level_keys_match: JSON.stringify(topLevelKeys) === JSON.stringify(TOOL_RESPONSES_KEYS),
    max_output_tokens_absent: !Object.hasOwn(body ?? {}, "max_output_tokens"),
    stream_true: body?.stream === true,
    store_false: body?.store === false,
    tool_choice_auto: body?.tool_choice === "auto",
    parallel_tool_calls_false: body?.parallel_tool_calls === false,
    tools,
  };
  return {
    ...contract,
    pass: contract.client_generation_fields_absent
      && contract.role !== null
      && contract.model_matches
      && contract.instructions_non_empty
      && contract.top_level_keys_match
      && contract.max_output_tokens_absent
      && contract.stream_true
      && contract.store_false
      && contract.tool_choice_auto
      && contract.parallel_tool_calls_false
      && contract.tools.pass,
  };
}

function toolErrorRecoveryExpected(script) {
  return {
    readArguments: JSON.stringify({ path: script.missingPath }),
  };
}

function toolErrorRecoveryRole(body, expectedPrompt, script) {
  const input = Array.isArray(body?.input) ? body.input : [];
  const expected = toolErrorRecoveryExpected(script);
  const itemTypes = input.map((item) => typeof item?.type === "string" ? item.type : null);
  const firstInputText = exactInputText(input[0]);
  const call = input[1];
  const output = input[2];
  const initial = input.length === 1 && firstInputText === expectedPrompt;
  const callMatches = exactKeys(call, ["arguments", "call_id", "name", "type"])
    && call.type === "function_call"
    && call.call_id === TOOL_ERROR_RECOVERY_READ_CALL_ID
    && call.name === "read"
    && call.arguments === expected.readArguments;
  const outputText = exactKeys(output, ["call_id", "output", "type"])
    && output.type === "function_call_output"
    && output.call_id === TOOL_ERROR_RECOVERY_READ_CALL_ID
    && typeof output.output === "string"
    && output.output.trim().length > 0
    ? output.output
    : null;
  const continuation = input.length === 3
    && firstInputText === expectedPrompt
    && callMatches
    && outputText !== null;
  return {
    role: initial ? "tool_error_initial" : continuation ? "tool_error_continuation" : null,
    evidence: {
      input_count: input.length,
      input_item_types: itemTypes,
      first_input_text_sha256: firstInputText === null
        ? null
        : sha256(Buffer.from(firstInputText, "utf8")),
      expected_prompt_matches: firstInputText === expectedPrompt,
      read_call_matches: callMatches,
      tool_output_non_empty: outputText !== null,
      tool_output_size_bytes: outputText === null ? null : Buffer.byteLength(outputText, "utf8"),
      tool_output_sha256: outputText === null ? null : sha256(Buffer.from(outputText, "utf8")),
    },
  };
}

function toolErrorRecoveryRequestContract(body, modelId, expectedPrompt, script) {
  const model = typeof body?.model === "string" ? body.model : null;
  const instructions = typeof body?.instructions === "string" ? body.instructions : null;
  const topLevelKeys = body !== null && typeof body === "object" && !Array.isArray(body)
    ? Object.keys(body).sort()
    : [];
  const tools = readToolsContract(body?.tools);
  const classified = toolErrorRecoveryRole(body, expectedPrompt, script);
  const generation = clientGenerationContract(body);
  const contract = {
    ...generation,
    script_kind: script.kind,
    role: classified.role,
    role_evidence: classified.evidence,
    model_sha256: model === null ? null : sha256(Buffer.from(model, "utf8")),
    instructions_sha256: instructions === null ? null : sha256(Buffer.from(instructions, "utf8")),
    top_level_keys: topLevelKeys,
    model_matches: model === modelId,
    instructions_non_empty: instructions !== null && instructions.trim().length > 0,
    top_level_keys_match: JSON.stringify(topLevelKeys) === JSON.stringify(TOOL_RESPONSES_KEYS),
    max_output_tokens_absent: !Object.hasOwn(body ?? {}, "max_output_tokens"),
    stream_true: body?.stream === true,
    store_false: body?.store === false,
    tool_choice_auto: body?.tool_choice === "auto",
    parallel_tool_calls_false: body?.parallel_tool_calls === false,
    tools,
  };
  return {
    ...contract,
    pass: contract.client_generation_fields_absent
      && contract.role !== null
      && contract.model_matches
      && contract.instructions_non_empty
      && contract.top_level_keys_match
      && contract.max_output_tokens_absent
      && contract.stream_true
      && contract.store_false
      && contract.tool_choice_auto
      && contract.parallel_tool_calls_false
      && contract.tools.pass,
  };
}

function sideChatQuoteDraft(selectedText) {
  return `> Side Chat 引用\n> ${selectedText}`;
}

function sideChatXmlEntitiesV1BodyIsCanonical(text) {
  if (typeof text !== "string") return false;
  const reserved = new Set(["<", ">", "[", "]", "\"", "'"]);
  for (let index = 0; index < text.length;) {
    const codePoint = text.codePointAt(index);
    const character = String.fromCodePoint(codePoint);
    if (character === "&") {
      const entity = text.slice(index).match(
        /^&(?:amp|lt|gt|quot|apos);|^&#x[0-9A-F]+;/u,
      );
      if (entity === null) return false;
      index += entity[0].length;
      continue;
    }
    if (reserved.has(character)
      || (codePoint >= 0x00 && codePoint <= 0x08)
      || codePoint === 0x0B
      || codePoint === 0x0C
      || (codePoint >= 0x0E && codePoint <= 0x1F)
      || (codePoint >= 0x7F && codePoint <= 0x9F)
      || (codePoint >= 0xD800 && codePoint <= 0xDFFF)) {
      return false;
    }
    index += character.length;
  }
  return true;
}

function sideChatXmlEntitiesV1Encode(text) {
  let encoded = "";
  for (const character of text) {
    const codePoint = character.codePointAt(0);
    if (character === "&") encoded += "&amp;";
    else if (character === "<") encoded += "&lt;";
    else if (character === ">") encoded += "&gt;";
    else if (character === "\"") encoded += "&quot;";
    else if (character === "'") encoded += "&apos;";
    else if (character === "[") encoded += "&#x5B;";
    else if (character === "]") encoded += "&#x5D;";
    else if (character === "\t" || character === "\n" || character === "\r") {
      encoded += character;
    } else if ((codePoint >= 0x00 && codePoint <= 0x1F)
      || (codePoint >= 0x7F && codePoint <= 0x9F)) {
      encoded += `&#x${codePoint.toString(16).toUpperCase()};`;
    } else {
      encoded += character;
    }
  }
  return encoded;
}

function sideChatQuoteEvidenceUnits(text) {
  const unitPattern = /\n<evidence_unit kind="([a-z_]+)" source_history_item_ids="([0-9A-Za-z]+(?:,[0-9A-Za-z]+)*)">\n([\s\S]*?)\n<\/evidence_unit>\n/uy;
  const units = [];
  const allSourceIds = new Set();
  let offset = 0;
  while (offset < text.length) {
    unitPattern.lastIndex = offset;
    const unit = unitPattern.exec(text);
    if (unit === null || unit.index !== offset) return null;
    const sourceHistoryItemIds = unit[2].split(",");
    if (!sideChatXmlEntitiesV1BodyIsCanonical(unit[3])
      || new Set(sourceHistoryItemIds).size !== sourceHistoryItemIds.length
      || sourceHistoryItemIds.some((id) => allSourceIds.has(id))) {
      return null;
    }
    sourceHistoryItemIds.forEach((id) => allSourceIds.add(id));
    units.push({
      kind: unit[1],
      sourceHistoryItemIds,
      body: unit[3],
    });
    offset = unitPattern.lastIndex;
  }
  return units.length > 0 ? units : null;
}

function sideChatQuoteEvidenceProfile(units, sourceKind, historyItemId) {
  const selectedUnitIndex = sourceKind === "artifact"
    ? 1
    : sourceKind === "transcript"
      ? 2
      : null;
  const unitKinds = units?.map((unit) => unit.kind) ?? [];
  const unitSourceCounts = units?.map((unit) => unit.sourceHistoryItemIds.length) ?? [];
  const unitBodySha256 = units?.map((unit) => sha256(Buffer.from(unit.body, "utf8"))) ?? [];
  const selectedUnit = selectedUnitIndex === null ? null : units?.[selectedUnitIndex] ?? null;
  const expectedKinds = ["owner_user", "owner_tool", "owner_assistant"];
  const expectedSourceCounts = [1, 3, 1];
  const knownBodiesMatch = units?.[0]?.body
      === sideChatXmlEntitiesV1Encode(SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_MAIN_PROMPT)
    && units?.[2]?.body
      === sideChatXmlEntitiesV1Encode(SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_MAIN_RESPONSE);
  const pass = units !== null
    && units.length === expectedKinds.length
    && JSON.stringify(unitKinds) === JSON.stringify(expectedKinds)
    && JSON.stringify(unitSourceCounts) === JSON.stringify(expectedSourceCounts)
    && knownBodiesMatch
    && selectedUnit?.sourceHistoryItemIds.at(-1) === historyItemId;
  return {
    pass,
    selected_unit_index: selectedUnitIndex,
    selected_unit_kind: selectedUnit?.kind ?? null,
    selected_unit_source_history_item_ids: selectedUnit?.sourceHistoryItemIds ?? null,
    unit_kinds: unitKinds,
    unit_source_history_item_counts: unitSourceCounts,
    unit_body_sha256: unitBodySha256,
    known_bodies_match: knownBodiesMatch,
  };
}

export function sideChatQuoteOwnerContext(text, sourceKind, selectedText) {
  if (typeof text !== "string"
    || typeof sourceKind !== "string"
    || typeof selectedText !== "string") {
    return { pass: false };
  }
  const envelope = text.match(
    /^<side_chat_owner_context>\nscope: owner_session\nowner_session_id: ([0-9A-Za-z]+)\nas_of_append_position: (\d+)\ntruncated: (false)\ncontent_encoding: (xml_entities_v1)\n\n<selected_quote source_kind="([a-z_]+)" source_history_item_id="([0-9A-Za-z]+)" source_append_position="(\d+)">\n([\s\S]*?)\n<\/selected_quote>\n\n<canonical_evidence>\n([\s\S]*)<\/canonical_evidence>\n<\/side_chat_owner_context>$/u,
  );
  if (envelope === null) return { pass: false };
  const ownerId = envelope[1];
  const asOf = envelope[2];
  const truncated = envelope[3];
  const contentEncoding = envelope[4];
  const quoteKind = envelope[5];
  const historyItemId = envelope[6];
  const quotePosition = envelope[7];
  const quoteText = envelope[8];
  const units = sideChatQuoteEvidenceUnits(envelope[9]);
  const evidenceProfile = sideChatQuoteEvidenceProfile(units, quoteKind, historyItemId);
  const canonicalSourceHistoryItemIds = evidenceProfile.pass
    ? evidenceProfile.selected_unit_source_history_item_ids
    : null;
  const quoteTextCanonical = sideChatXmlEntitiesV1BodyIsCanonical(quoteText);
  const pass = quoteTextCanonical
    && quoteKind === sourceKind
    && quotePosition === asOf
    && quoteText === sideChatXmlEntitiesV1Encode(selectedText)
    && evidenceProfile.pass;
  return {
    pass,
    owner_session_id_sha256: sha256(Buffer.from(ownerId, "utf8")),
    as_of_append_position: asOf,
    truncated,
    content_encoding: contentEncoding,
    source_kind: quoteKind,
    source_history_item_id: historyItemId,
    source_append_position: quotePosition,
    selected_text_sha256: sha256(Buffer.from(quoteText, "utf8")),
    canonical_evidence_present: units !== null,
    canonical_source_history_item_ids: canonicalSourceHistoryItemIds,
    canonical_evidence_profile: evidenceProfile,
  };
}

export function sideChatSessionOwnerContext(text) {
  if (typeof text !== "string") return { pass: false };
  const envelope = text.match(
    /^<side_chat_owner_context>\nscope: owner_session\nowner_session_id: ([0-7][0-9A-HJKMNP-TV-Z]{25})\nas_of_append_position: ([1-9]\d*)\ntruncated: false\ncontent_encoding: xml_entities_v1\n\n<canonical_evidence>\n([\s\S]*)<\/canonical_evidence>\n<\/side_chat_owner_context>$/u,
  );
  if (envelope === null || BigInt(envelope[2]) > 9_223_372_036_854_775_807n) {
    return { pass: false };
  }
  const units = sideChatQuoteEvidenceUnits(envelope[3]);
  const expectedKinds = ["owner_user", "owner_assistant"];
  const expectedBodies = [
    SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_PROMPT,
    SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_RESPONSE,
  ].map(sideChatXmlEntitiesV1Encode);
  const canonicalId = /^[0-7][0-9A-HJKMNP-TV-Z]{25}$/u;
  const pass = units !== null
    && units.length === expectedKinds.length
    && units.every((unit, index) => unit.kind === expectedKinds[index]
      && unit.sourceHistoryItemIds.length === 1
      && canonicalId.test(unit.sourceHistoryItemIds[0])
      && unit.body === expectedBodies[index]);
  return {
    pass,
    owner_session_id: envelope[1],
    as_of_append_position: envelope[2],
    truncated: false,
    content_encoding: "xml_entities_v1",
    unit_kinds: units?.map((unit) => unit.kind) ?? [],
    source_history_item_ids: units?.flatMap((unit) => unit.sourceHistoryItemIds) ?? [],
    unit_body_sha256: units?.map((unit) => sha256(Buffer.from(unit.body, "utf8"))) ?? [],
  };
}

function sideChatSessionRequestContract(body, modelId, script) {
  const input = Array.isArray(body?.input) ? body.input : [];
  const firstText = exactInputText(input[0]);
  const instructions = typeof body?.instructions === "string" ? body.instructions : "";
  const ownerContext = sideChatSessionOwnerContext(firstText);
  const role = input.length === 1 && firstText === SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_PROMPT
    ? "side_session_alpha"
    : input.length === 1 && firstText === SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_BETA_PROMPT
      ? "side_session_beta"
      : input.length === 1
          && firstText === SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_MAIN_AFTER_CONFIG_PROMPT
        ? "side_session_main_after_config"
        : input.length === 2 && ownerContext.pass
            && exactInputText(input[1]) === SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_QUESTION
          ? "side_session_consult"
          : null;
  // Reuse the common strict tool-less wire contract; only the two-message
  // owner-context input differs from its one-user-input predicate.
  const wire = requestContract(body, modelId, firstText);
  const inputMatches = role !== null;
  const serializedBody = JSON.stringify(body);
  const foreignSessionAbsent = role !== "side_session_consult" || ![
    SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_BETA_PROMPT,
    SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_BETA_RESPONSE,
    SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_MAIN_AFTER_CONFIG_PROMPT,
    SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_MAIN_AFTER_CONFIG_RESPONSE,
  ].some((text) => serializedBody.includes(text));
  const consultSystemPromptMarkerCount = instructions
    .split(script.expectedConsultSystemPromptMarker).length - 1;
  const consultSystemPromptMarkerPresent = consultSystemPromptMarkerCount > 0;
  const consultSystemPromptMarkerExactlyOnce = consultSystemPromptMarkerCount === 1;
  return {
    ...wire,
    script_kind: script.kind,
    role,
    input_count: input.length,
    input_matches: inputMatches,
    owner_context: ownerContext,
    foreign_session_absent: foreignSessionAbsent,
    consult_system_prompt_marker_present: consultSystemPromptMarkerPresent,
    consult_system_prompt_marker_exactly_once: consultSystemPromptMarkerExactlyOnce,
    pass: wire.client_generation_fields_absent
      && wire.model_matches
      && inputMatches
      && foreignSessionAbsent
      && (role === "side_session_consult"
        ? consultSystemPromptMarkerExactlyOnce
        : !consultSystemPromptMarkerPresent)
      && wire.instructions_non_empty
      && wire.top_level_keys_match
      && wire.max_output_tokens_absent
      && wire.stream_true
      && wire.store_false,
  };
}

function sideChatQuoteExpectedWrite() {
  return {
    arguments: JSON.stringify({
      path: SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_ARTIFACT_PATH,
      content: SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_ARTIFACT_CONTENT,
    }),
  };
}

function sideChatQuoteRole(body) {
  const input = Array.isArray(body?.input) ? body.input : [];
  const inputTypes = input.map((item) => typeof item?.type === "string" ? item.type : null);
  const firstText = exactInputText(input[0]);
  const expectedWrite = sideChatQuoteExpectedWrite();
  const writeCall = input[1];
  const writeOutput = input[2];
  const writeCallMatches = exactKeys(writeCall, ["arguments", "call_id", "name", "type"])
    && writeCall.type === "function_call"
    && writeCall.call_id === SIDE_CHAT_QUOTE_WRITE_CALL_ID
    && writeCall.name === "write"
    && writeCall.arguments === expectedWrite.arguments;
  const writeOutputText = exactKeys(writeOutput, ["call_id", "output", "type"])
    && writeOutput.type === "function_call_output"
    && writeOutput.call_id === SIDE_CHAT_QUOTE_WRITE_CALL_ID
    && typeof writeOutput.output === "string"
    && writeOutput.output.trim().length > 0
    ? writeOutput.output
    : null;

  const mainInitial = input.length === 1
    && firstText === SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_MAIN_PROMPT;
  const mainContinuation = input.length === 3
    && firstText === SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_MAIN_PROMPT
    && writeCallMatches
    && writeOutputText !== null;

  const transcriptContext = sideChatQuoteOwnerContext(
    firstText,
    "transcript",
    SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_TRANSCRIPT_SELECTION,
  );
  const artifactContext = sideChatQuoteOwnerContext(
    firstText,
    "artifact",
    SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_ARTIFACT_PATH,
  );
  const transcriptDraft = sideChatQuoteDraft(
    SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_TRANSCRIPT_SELECTION,
  );
  const artifactDraft = sideChatQuoteDraft(
    SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_ARTIFACT_PATH,
  );
  const sideTranscript = input.length === 2
    && transcriptContext.pass
    && exactInputText(input[1]) === transcriptDraft;
  const sideArtifact = input.length === 4
    && artifactContext.pass
    && exactInputText(input[1]) === transcriptDraft
    && exactOutputText(input[2]) === SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_FIRST_RESPONSE
    && exactInputText(input[3]) === artifactDraft;
  const role = mainInitial
    ? "side_quote_main_initial"
    : mainContinuation
      ? "side_quote_main_continuation"
      : sideTranscript
        ? "side_quote_transcript"
        : sideArtifact
          ? "side_quote_artifact_held"
          : null;
  return {
    role,
    evidence: {
      input_count: input.length,
      input_item_types: inputTypes,
      first_input_text_sha256: firstText === null ? null : sha256(Buffer.from(firstText, "utf8")),
      main_prompt_matches: firstText === SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_MAIN_PROMPT,
      write_call_matches: writeCallMatches,
      write_output_non_empty: writeOutputText !== null,
      transcript_context: transcriptContext,
      artifact_context: artifactContext,
      transcript_draft_matches: exactInputText(input.at(-1)) === transcriptDraft,
      artifact_draft_matches: exactInputText(input.at(-1)) === artifactDraft,
      prior_side_response_matches: exactOutputText(input[2])
        === SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_FIRST_RESPONSE,
    },
  };
}

function sideChatQuoteRequestContract(body, modelId, script) {
  const model = typeof body?.model === "string" ? body.model : null;
  const instructions = typeof body?.instructions === "string" ? body.instructions : null;
  const topLevelKeys = body !== null && typeof body === "object" && !Array.isArray(body)
    ? Object.keys(body).sort()
    : [];
  const classified = sideChatQuoteRole(body);
  const mainRole = classified.role === "side_quote_main_initial"
    || classified.role === "side_quote_main_continuation";
  const sideRole = classified.role === "side_quote_transcript"
    || classified.role === "side_quote_artifact_held";
  const tools = writeToolsContract(body?.tools);
  const generation = clientGenerationContract(body);
  const contract = {
    ...generation,
    script_kind: script.kind,
    role: classified.role,
    role_evidence: classified.evidence,
    model_sha256: model === null ? null : sha256(Buffer.from(model, "utf8")),
    instructions_sha256: instructions === null ? null : sha256(Buffer.from(instructions, "utf8")),
    top_level_keys: topLevelKeys,
    model_matches: model === modelId,
    instructions_non_empty: instructions !== null && instructions.trim().length > 0,
    top_level_keys_match: mainRole
      ? JSON.stringify(topLevelKeys) === JSON.stringify(TOOL_RESPONSES_KEYS)
      : sideRole && JSON.stringify(topLevelKeys) === JSON.stringify(EXPECTED_RESPONSES_KEYS),
    max_output_tokens_absent: !Object.hasOwn(body ?? {}, "max_output_tokens"),
    stream_true: body?.stream === true,
    store_false: body?.store === false,
    tool_choice_matches: mainRole ? body?.tool_choice === "auto" : !Object.hasOwn(body ?? {}, "tool_choice"),
    parallel_tool_calls_matches: mainRole
      ? body?.parallel_tool_calls === false
      : !Object.hasOwn(body ?? {}, "parallel_tool_calls"),
    tools,
    tools_match: mainRole ? tools.pass : sideRole && !Object.hasOwn(body ?? {}, "tools"),
  };
  return {
    ...contract,
    pass: contract.client_generation_fields_absent
      && contract.role !== null
      && contract.model_matches
      && contract.instructions_non_empty
      && contract.top_level_keys_match
      && contract.max_output_tokens_absent
      && contract.stream_true
      && contract.store_false
      && contract.tool_choice_matches
      && contract.parallel_tool_calls_matches
      && contract.tools_match,
  };
}

function responsesCompactionReadExpected(script, callIndex) {
  return {
    callId: `call_responses_compaction_read_${callIndex}`,
    itemId: `fc_responses_compaction_read_${callIndex}`,
    arguments: JSON.stringify({
      path: script.sentinelPath,
      offset: callIndex,
      limit: 1,
    }),
  };
}

function responsesCompactionReadPair(input, itemIndex, script, callIndex) {
  const call = input[itemIndex];
  const output = input[itemIndex + 1];
  const expected = responsesCompactionReadExpected(script, callIndex);
  const callMatches = exactKeys(call, ["arguments", "call_id", "name", "type"])
    && call.type === "function_call"
    && call.call_id === expected.callId
    && call.name === "read"
    && call.arguments === expected.arguments;
  const outputText = exactKeys(output, ["call_id", "output", "type"])
    && output.type === "function_call_output"
    && output.call_id === expected.callId
    && typeof output.output === "string"
    && output.output.trim().length > 0
    ? output.output
    : null;
  return {
    pass: callMatches && outputText !== null,
    signature: callMatches && outputText !== null
      ? sha256(Buffer.from(JSON.stringify([call, output]), "utf8"))
      : null,
    output_size_bytes: outputText === null ? null : Buffer.byteLength(outputText, "utf8"),
    output_sha256: outputText === null ? null : sha256(Buffer.from(outputText, "utf8")),
  };
}

function compactionPromptText(item) {
  const text = exactInputText(item);
  if (text === null || !text.startsWith("# Context checkpoint compaction\n")) return null;
  const headings = Array.from(text.matchAll(/^## .+$/gmu), (match) => match[0]);
  return JSON.stringify(headings) === JSON.stringify(SCRIPTED_PROVIDER_COMPACTION_HEADINGS)
    ? text
    : null;
}

function responsesCompactionReadPrefix(
  input,
  expectedPrompt,
  script,
  { requireCompactionPrompt = false } = {},
) {
  const items = Array.isArray(input) ? input : [];
  const sourceEnd = requireCompactionPrompt ? items.length - 1 : items.length;
  const terminalText = requireCompactionPrompt ? compactionPromptText(items.at(-1)) : null;
  const terminalMatches = !requireCompactionPrompt
    ? true
    : sourceEnd >= 1 && terminalText !== null;
  const source = sourceEnd >= 0 ? items.slice(0, sourceEnd) : [];
  const firstPromptMatches = exactInputText(source[0]) === expectedPrompt;
  const pairItemCount = source.length - 1;
  const pairCount = pairItemCount >= 0 && pairItemCount % 2 === 0
    ? pairItemCount / 2
    : null;
  const pairs = pairCount === null
    ? []
    : Array.from({ length: pairCount }, (_, index) => (
      responsesCompactionReadPair(source, 1 + index * 2, script, index + 1)
    ));
  const matches = terminalMatches
    && firstPromptMatches
    && pairCount !== null
    && pairCount <= script.toolCallCount
    && pairs.every((pair) => pair.pass);
  return {
    matches,
    input_count: items.length,
    input_item_types: items.map((item) => typeof item?.type === "string" ? item.type : null),
    source_read_count: pairCount,
    source_unit_count: pairCount === null ? null : pairCount + 1,
    source_unit_signatures: matches
      ? [
        sha256(Buffer.from(JSON.stringify(source[0]), "utf8")),
        ...pairs.map((pair) => pair.signature),
      ]
      : [],
    read_output_size_bytes: pairs.map((pair) => pair.output_size_bytes),
    read_output_sha256: pairs.map((pair) => pair.output_sha256),
    first_prompt_matches: firstPromptMatches,
    terminal_prompt_matches: terminalMatches,
    terminal_prompt_sha256: terminalText === null
      ? null
      : sha256(Buffer.from(terminalText, "utf8")),
  };
}

function responsesCompactionCheckpointMessage(script) {
  return `${SCRIPTED_PROVIDER_COMPACTION_SUMMARY_PREFIX}\n${script.checkpointText}`;
}

function responsesCompactionResumedInput(input, expectedPrompt, script) {
  const items = Array.isArray(input) ? input : [];
  const promptMatches = exactInputText(items[0]) === expectedPrompt;
  const checkpointMatches = exactInputText(items[1]) === responsesCompactionCheckpointMessage(script);
  const pairItemCount = items.length - 2;
  const pairCount = pairItemCount >= 0 && pairItemCount % 2 === 0
    ? pairItemCount / 2
    : null;
  let firstReadIndex = null;
  const pairs = [];
  if (pairCount !== null && pairCount > 0) {
    const firstCallId = items[2]?.call_id;
    const match = typeof firstCallId === "string"
      ? firstCallId.match(/^call_responses_compaction_read_([0-9]+)$/u)
      : null;
    firstReadIndex = match === null ? null : Number(match[1]);
    if (Number.isSafeInteger(firstReadIndex)) {
      for (let index = 0; index < pairCount; index += 1) {
        pairs.push(responsesCompactionReadPair(
          items,
          2 + index * 2,
          script,
          firstReadIndex + index,
        ));
      }
    }
  }
  const matches = promptMatches
    && checkpointMatches
    && pairCount !== null
    && pairCount > 0
    && Number.isSafeInteger(firstReadIndex)
    && firstReadIndex >= 2
    && firstReadIndex + pairCount - 1 === script.toolCallCount
    && pairs.every((pair) => pair.pass);
  return {
    matches,
    input_count: items.length,
    input_item_types: items.map((item) => typeof item?.type === "string" ? item.type : null),
    first_prompt_matches: promptMatches,
    checkpoint_matches: checkpointMatches,
    checkpoint_sha256: checkpointMatches
      ? sha256(Buffer.from(responsesCompactionCheckpointMessage(script), "utf8"))
      : null,
    remaining_read_count: pairCount,
    first_remaining_read_index: firstReadIndex,
    read_output_size_bytes: pairs.map((pair) => pair.output_size_bytes),
    read_output_sha256: pairs.map((pair) => pair.output_sha256),
  };
}

function responsesCompactionRole(body, expectedPrompt, script) {
  const input = Array.isArray(body?.input) ? body.input : [];
  const inputSizeBytes = Buffer.byteLength(JSON.stringify(input), "utf8");
  const hasTools = Array.isArray(body?.tools) && body.tools.length > 0;
  if (hasTools && exactInputText(input[1]) === responsesCompactionCheckpointMessage(script)) {
    const resumed = responsesCompactionResumedInput(input, expectedPrompt, script);
    return {
      role: resumed.matches ? "resumed_final" : null,
      evidence: {
        input_size_bytes: inputSizeBytes,
        input_byte_threshold: script.inputByteThreshold,
        ...resumed,
      },
    };
  }
  if (hasTools) {
    const prefix = responsesCompactionReadPrefix(input, expectedPrompt, script);
    const nextIndex = prefix.source_read_count === null ? null : prefix.source_read_count + 1;
    return {
      role: prefix.matches && nextIndex <= script.toolCallCount
        ? `read_${nextIndex}`
        : null,
      evidence: {
        input_size_bytes: inputSizeBytes,
        input_byte_threshold: script.inputByteThreshold,
        ...prefix,
      },
    };
  }
  const source = responsesCompactionReadPrefix(input, expectedPrompt, script, {
    requireCompactionPrompt: true,
  });
  const oversized = inputSizeBytes > script.inputByteThreshold;
  return {
    role: source.matches
      ? oversized ? "compaction_empty" : "compaction_valid"
      : null,
    evidence: {
      input_size_bytes: inputSizeBytes,
      input_byte_threshold: script.inputByteThreshold,
      input_exceeds_threshold: oversized,
      bounded_prefix_candidate: source.source_read_count !== null
        && source.source_read_count < script.toolCallCount,
      ...source,
    },
  };
}

function responsesCompactionRequestContract(body, modelId, expectedPrompt, script) {
  const model = typeof body?.model === "string" ? body.model : null;
  const instructions = typeof body?.instructions === "string" ? body.instructions : null;
  const topLevelKeys = body !== null && typeof body === "object" && !Array.isArray(body)
    ? Object.keys(body).sort()
    : [];
  const classified = responsesCompactionRole(body, expectedPrompt, script);
  const generation = clientGenerationContract(body);
  const toolBearing = classified.role?.startsWith("read_") === true
    || classified.role === "resumed_final";
  const expectedKeys = toolBearing ? TOOL_RESPONSES_KEYS : EXPECTED_RESPONSES_KEYS;
  const tools = toolBearing ? readToolsContract(body?.tools) : null;
  const contract = {
    ...generation,
    script_kind: script.kind,
    role: classified.role,
    role_evidence: classified.evidence,
    model_sha256: model === null ? null : sha256(Buffer.from(model, "utf8")),
    instructions_sha256: instructions === null ? null : sha256(Buffer.from(instructions, "utf8")),
    top_level_keys: topLevelKeys,
    model_matches: model === modelId,
    instructions_non_empty: instructions !== null && instructions.trim().length > 0,
    top_level_keys_match: JSON.stringify(topLevelKeys) === JSON.stringify(expectedKeys),
    max_output_tokens_absent: !Object.hasOwn(body ?? {}, "max_output_tokens"),
    stream_true: body?.stream === true,
    store_false: body?.store === false,
    compaction_tools_absent: toolBearing
      ? null
      : !Object.hasOwn(body ?? {}, "tools")
        && !Object.hasOwn(body ?? {}, "tool_choice")
        && !Object.hasOwn(body ?? {}, "parallel_tool_calls"),
    tool_choice_auto: toolBearing ? body?.tool_choice === "auto" : null,
    parallel_tool_calls_false: toolBearing ? body?.parallel_tool_calls === false : null,
    tools,
  };
  return {
    ...contract,
    pass: contract.client_generation_fields_absent
      && contract.role !== null
      && contract.model_matches
      && contract.instructions_non_empty
      && contract.top_level_keys_match
      && contract.max_output_tokens_absent
      && contract.stream_true
      && contract.store_false
      && (toolBearing
        ? contract.tool_choice_auto && contract.parallel_tool_calls_false && contract.tools.pass
        : contract.compaction_tools_absent),
  };
}

function permissionRestartGuardianExpected(script) {
  return {
    shellArguments: JSON.stringify({
      command: script.command,
      sandbox_permissions: "require_escalated",
      justification: script.justification,
    }),
  };
}

function permissionRestartGuardianMainRole(body, script) {
  const input = Array.isArray(body?.input) ? body.input : [];
  const expected = permissionRestartGuardianExpected(script);
  const seedUser = exactInputText(input[0]);
  const seedAssistant = exactOutputText(input[1]);
  const taskUser = exactInputText(input[2]);
  const call = input[3];
  const output = input[4];
  const seed = input.length === 1 && seedUser === script.seedPrompt;
  const toolInitial = input.length === 3
    && seedUser === script.seedPrompt
    && seedAssistant === script.seedResponseText
    && taskUser === script.taskPrompt;
  const callMatches = exactKeys(call, ["arguments", "call_id", "name", "type"])
    && call.type === "function_call"
    && call.call_id === PERMISSION_RESTART_GUARDIAN_SHELL_CALL_ID
    && call.name === "shell"
    && call.arguments === expected.shellArguments;
  const toolOutput = exactKeys(output, ["call_id", "output", "type"])
    && output.type === "function_call_output"
    && output.call_id === PERMISSION_RESTART_GUARDIAN_SHELL_CALL_ID
    && typeof output.output === "string"
    && output.output.trim().length > 0
    ? output.output
    : null;
  const outputMatches = toolOutput !== null
    && toolOutput.includes(`Command: ${script.command}`)
    && toolOutput.includes("Exit code: 0");
  const continuation = input.length === 5
    && seedUser === script.seedPrompt
    && seedAssistant === script.seedResponseText
    && taskUser === script.taskPrompt
    && callMatches
    && outputMatches;
  const role = seed
    ? "guardian_seed"
    : toolInitial
      ? "guardian_tool_initial"
      : continuation
        ? "guardian_continuation"
        : null;
  return {
    role,
    evidence: {
      input_count: input.length,
      input_item_types: input.map((item) => typeof item?.type === "string" ? item.type : null),
      seed_prompt_sha256: seedUser === null ? null : sha256(Buffer.from(seedUser, "utf8")),
      seed_response_sha256: seedAssistant === null
        ? null
        : sha256(Buffer.from(seedAssistant, "utf8")),
      task_prompt_sha256: taskUser === null ? null : sha256(Buffer.from(taskUser, "utf8")),
      seed_prompt_matches: seedUser === script.seedPrompt,
      seed_response_matches: seedAssistant === script.seedResponseText,
      task_prompt_matches: taskUser === script.taskPrompt,
      shell_call_matches: callMatches,
      tool_output_non_empty: toolOutput !== null,
      tool_output_matches: outputMatches,
      tool_output_size_bytes: toolOutput === null ? null : Buffer.byteLength(toolOutput, "utf8"),
      tool_output_sha256: toolOutput === null ? null : sha256(Buffer.from(toolOutput, "utf8")),
    },
  };
}

function permissionGuardianPayloadContract(inputText, script) {
  let payload = null;
  let taskContext = null;
  try {
    payload = JSON.parse(inputText);
    taskContext = typeof payload?.task_context === "string"
      ? JSON.parse(payload.task_context)
      : null;
  } catch {
    // Invalid or wrapped evidence fails the exact Guardian request contract below.
  }
  const payloadKeysMatch = exactKeys(payload, [
    "action_evidence",
    "permission_request",
    "recent_committed_response",
    "task_context",
    "trusted_world_state",
  ]);
  const authority = Array.isArray(taskContext?.canonical_user_authority)
    ? taskContext.canonical_user_authority
    : [];
  const authorityTexts = authority.map((item) => typeof item?.text === "string" ? item.text : null);
  const authorityIds = authority.map((item) => typeof item?.history_item_id === "string"
    ? item.history_item_id
    : null);
  const authorityMatches = exactKeys(taskContext, ["authority_session_id", "canonical_user_authority"])
    && typeof taskContext.authority_session_id === "string"
    && taskContext.authority_session_id.length > 0
    && authority.length === 2
    && authority.every((item) => exactKeys(item, ["history_item_id", "kind", "text"])
      && item.kind === "user_turn"
      && typeof item.history_item_id === "string"
      && item.history_item_id.length > 0)
    && authorityTexts[0] === script.seedPrompt
    && authorityTexts[1] === script.taskPrompt
    && new Set(authorityIds).size === authorityIds.length;
  const recent = payload?.recent_committed_response;
  const toolRequest = recent?.tool_request;
  const expected = permissionRestartGuardianExpected(script);
  const recentMatches = exactKeys(recent, [
    "assistant_text",
    "prior_committed_tool_results",
    "response_id",
    "tool_request",
  ])
    && typeof recent.response_id === "string"
    && recent.response_id.length > 0
    && typeof recent.assistant_text === "string"
    && Array.isArray(recent.prior_committed_tool_results)
    && recent.prior_committed_tool_results.length === 0
    && exactKeys(toolRequest, ["arguments_json", "call_id", "tool_name"])
    && toolRequest.call_id === PERMISSION_RESTART_GUARDIAN_SHELL_CALL_ID
    && toolRequest.tool_name === "shell"
    && toolRequest.arguments_json === expected.shellArguments;
  const permission = payload?.permission_request;
  const permissionMatches = exactKeys(permission, [
    "access",
    "details",
    "outside_workspace",
    "risks",
    "summary",
    "targets",
  ])
    && permission.access === "shell"
    && typeof permission.summary === "string"
    && permission.summary.trim().length > 0
    && Array.isArray(permission.details)
    && permission.details.some((detail) => detail === `Requested sandbox elevation: ${script.justification}`)
    && Array.isArray(permission.targets)
    && permission.targets.length > 0
    && permission.targets.every((target) => typeof target === "string" && target.length > 0)
    && permission.outside_workspace === true
    && Array.isArray(permission.risks)
    && permission.risks.length === 0;
  const evidenceMatches = exactKeys(payload?.action_evidence, ["kind"])
    && payload.action_evidence.kind === "permission_request";
  const worldStateMatches = payload?.trusted_world_state !== null
    && typeof payload?.trusted_world_state === "object"
    && !Array.isArray(payload.trusted_world_state)
    && Object.keys(payload.trusted_world_state).length > 0;
  return {
    json_valid: payload !== null,
    payload_keys_match: payloadKeysMatch,
    authority_count: authority.length,
    authority_kind_hashes: authority.map((item) => typeof item?.kind === "string"
      ? sha256(Buffer.from(item.kind, "utf8"))
      : null),
    authority_text_hashes: authorityTexts.map((text) => text === null
      ? null
      : sha256(Buffer.from(text, "utf8"))),
    authority_identity_hashes: authorityIds.map((id) => id === null
      ? null
      : sha256(Buffer.from(id, "utf8"))),
    authority_matches: authorityMatches,
    recent_committed_response_matches: recentMatches,
    permission_request_matches: permissionMatches,
    action_evidence_matches: evidenceMatches,
    trusted_world_state_matches: worldStateMatches,
    pass: payloadKeysMatch
      && authorityMatches
      && recentMatches
      && permissionMatches
      && evidenceMatches
      && worldStateMatches,
  };
}

function permissionRestartGuardianReviewRole(body, script) {
  const input = Array.isArray(body?.input) ? body.input : [];
  const inputText = input.length === 1 ? exactInputText(input[0]) : null;
  const payload = inputText === null
    ? permissionGuardianPayloadContract("", script)
    : permissionGuardianPayloadContract(inputText, script);
  return {
    role: input.length === 1 && payload.pass ? "guardian_review" : null,
    evidence: {
      input_count: input.length,
      input_text_size_bytes: inputText === null ? null : Buffer.byteLength(inputText, "utf8"),
      input_text_sha256: inputText === null ? null : sha256(Buffer.from(inputText, "utf8")),
      payload,
    },
  };
}

function permissionRestartGuardianRequestContract(
  body,
  modelId,
  script,
) {
  const guardianShape = Object.hasOwn(body ?? {}, "reasoning")
    || !Object.hasOwn(body ?? {}, "tools");
  const classified = guardianShape
    ? permissionRestartGuardianReviewRole(body, script)
    : permissionRestartGuardianMainRole(body, script);
  const model = typeof body?.model === "string" ? body.model : null;
  const instructions = typeof body?.instructions === "string" ? body.instructions : null;
  const topLevelKeys = body !== null && typeof body === "object" && !Array.isArray(body)
    ? Object.keys(body).sort()
    : [];
  const expectedKeys = guardianShape ? GUARDIAN_RESPONSES_KEYS : TOOL_RESPONSES_KEYS;
  const tools = guardianShape ? null : shellToolsContract(body?.tools);
  const generation = clientGenerationContract(body);
  const common = {
    ...generation,
    script_kind: script.kind,
    role: classified.role,
    role_evidence: classified.evidence,
    model_sha256: model === null ? null : sha256(Buffer.from(model, "utf8")),
    instructions_sha256: instructions === null ? null : sha256(Buffer.from(instructions, "utf8")),
    top_level_keys: topLevelKeys,
    model_matches: model === modelId,
    instructions_non_empty: instructions !== null && instructions.trim().length > 0,
    top_level_keys_match: JSON.stringify(topLevelKeys) === JSON.stringify(expectedKeys),
    stream_true: body?.stream === true,
    store_false: body?.store === false,
  };
  if (guardianShape) {
    const guardian = {
      ...common,
      guardian_instructions_match: instructions?.includes("independent permission guardian") === true,
      max_output_tokens_absent: !Object.hasOwn(body ?? {}, "max_output_tokens"),
      reasoning_absent: !Object.hasOwn(body ?? {}, "reasoning"),
      tools_absent: !Object.hasOwn(body ?? {}, "tools")
        && !Object.hasOwn(body ?? {}, "tool_choice")
        && !Object.hasOwn(body ?? {}, "parallel_tool_calls"),
    };
    return {
      ...guardian,
      pass: guardian.client_generation_fields_absent
        && guardian.role === "guardian_review"
        && guardian.model_matches
        && guardian.instructions_non_empty
        && guardian.guardian_instructions_match
        && guardian.top_level_keys_match
        && guardian.max_output_tokens_absent
        && guardian.reasoning_absent
        && guardian.tools_absent
        && guardian.stream_true
        && guardian.store_false,
    };
  }
  const task = {
    ...common,
    max_output_tokens_absent: !Object.hasOwn(body ?? {}, "max_output_tokens"),
    tool_choice_auto: body?.tool_choice === "auto",
    parallel_tool_calls_false: body?.parallel_tool_calls === false,
    tools,
  };
  return {
    ...task,
    pass: task.client_generation_fields_absent
      && task.role !== null
      && task.model_matches
      && task.instructions_non_empty
      && task.top_level_keys_match
      && task.max_output_tokens_absent
      && task.tool_choice_auto
      && task.parallel_tool_calls_false
      && task.tools.pass
      && task.stream_true
      && task.store_false,
  };
}

function permissionRestartGuardianChatMainRole(body, script) {
  const messages = Array.isArray(body?.messages) ? body.messages : [];
  const systemText = exactChatTextMessage(messages[0], "system");
  const seedUser = exactChatTextMessage(messages[1], "user");
  const seedAssistant = exactChatTextMessage(messages[2], "assistant");
  const taskUser = exactChatTextMessage(messages[3], "user");
  const assistant = messages[4];
  const tool = messages[5];
  const expected = permissionRestartGuardianExpected(script);
  const toolCall = Array.isArray(assistant?.tool_calls) && assistant.tool_calls.length === 1
    ? assistant.tool_calls[0]
    : null;
  const callMatches = exactKeys(assistant, ["role", "tool_calls"])
    && assistant.role === "assistant"
    && exactKeys(toolCall, ["function", "id", "type"])
    && toolCall.id === PERMISSION_RESTART_GUARDIAN_SHELL_CALL_ID
    && toolCall.type === "function"
    && exactKeys(toolCall.function, ["arguments", "name"])
    && toolCall.function.name === "shell"
    && toolCall.function.arguments === expected.shellArguments;
  const toolOutput = exactKeys(tool, ["content", "role", "tool_call_id"])
    && tool.role === "tool"
    && tool.tool_call_id === PERMISSION_RESTART_GUARDIAN_SHELL_CALL_ID
    && typeof tool.content === "string"
    && tool.content.trim().length > 0
    ? tool.content
    : null;
  const outputMatches = toolOutput !== null
    && toolOutput.includes(`Command: ${script.command}`)
    && toolOutput.includes("Exit code: 0");
  const commonHistoryMatches = systemText !== null
    && systemText.trim().length > 0
    && seedUser === script.seedPrompt;
  const seed = messages.length === 2 && commonHistoryMatches;
  const toolInitial = messages.length === 4
    && commonHistoryMatches
    && seedAssistant === script.seedResponseText
    && taskUser === script.taskPrompt;
  const continuation = messages.length === 6
    && commonHistoryMatches
    && seedAssistant === script.seedResponseText
    && taskUser === script.taskPrompt
    && callMatches
    && outputMatches;
  const role = seed
    ? "guardian_seed"
    : toolInitial
      ? "guardian_tool_initial"
      : continuation
        ? "guardian_continuation"
        : null;
  return {
    role,
    evidence: {
      message_count: messages.length,
      message_roles: messages.map((message) => typeof message?.role === "string"
        ? message.role
        : null),
      system_content_sha256: systemText === null
        ? null
        : sha256(Buffer.from(systemText, "utf8")),
      system_content_non_empty: systemText !== null && systemText.trim().length > 0,
      seed_prompt_sha256: seedUser === null ? null : sha256(Buffer.from(seedUser, "utf8")),
      seed_response_sha256: seedAssistant === null
        ? null
        : sha256(Buffer.from(seedAssistant, "utf8")),
      task_prompt_sha256: taskUser === null ? null : sha256(Buffer.from(taskUser, "utf8")),
      seed_prompt_matches: seedUser === script.seedPrompt,
      seed_response_matches: seedAssistant === script.seedResponseText,
      task_prompt_matches: taskUser === script.taskPrompt,
      assistant_content_absent: assistant !== null
        && typeof assistant === "object"
        && !Array.isArray(assistant)
        && !Object.hasOwn(assistant, "content"),
      shell_call_matches: callMatches,
      tool_output_non_empty: toolOutput !== null,
      tool_output_matches: outputMatches,
      tool_output_size_bytes: toolOutput === null ? null : Buffer.byteLength(toolOutput, "utf8"),
      tool_output_sha256: toolOutput === null ? null : sha256(Buffer.from(toolOutput, "utf8")),
    },
  };
}

function permissionRestartGuardianChatReviewRole(body, script) {
  const messages = Array.isArray(body?.messages) ? body.messages : [];
  const systemText = exactChatTextMessage(messages[0], "system");
  const inputText = exactChatTextMessage(messages[1], "user");
  const payload = inputText === null
    ? permissionGuardianPayloadContract("", script)
    : permissionGuardianPayloadContract(inputText, script);
  const guardianInstructionsMatch = systemText?.includes("independent permission guardian") === true;
  return {
    role: messages.length === 2 && guardianInstructionsMatch && payload.pass
      ? "guardian_review"
      : null,
    evidence: {
      message_count: messages.length,
      message_roles: messages.map((message) => typeof message?.role === "string"
        ? message.role
        : null),
      system_content_sha256: systemText === null
        ? null
        : sha256(Buffer.from(systemText, "utf8")),
      guardian_instructions_match: guardianInstructionsMatch,
      input_text_size_bytes: inputText === null ? null : Buffer.byteLength(inputText, "utf8"),
      input_text_sha256: inputText === null ? null : sha256(Buffer.from(inputText, "utf8")),
      payload,
    },
  };
}

function permissionRestartGuardianChatRequestContract(body, modelId, script) {
  const messages = Array.isArray(body?.messages) ? body.messages : [];
  const firstSystem = exactChatTextMessage(messages[0], "system");
  const guardianShape = firstSystem?.includes("independent permission guardian") === true;
  const classified = guardianShape
    ? permissionRestartGuardianChatReviewRole(body, script)
    : permissionRestartGuardianChatMainRole(body, script);
  const model = typeof body?.model === "string" ? body.model : null;
  const topLevelKeys = body !== null && typeof body === "object" && !Array.isArray(body)
    ? Object.keys(body).sort()
    : [];
  const expectedKeys = guardianShape ? GUARDIAN_CHAT_KEYS : CHAT_TOOL_CONTINUATION_KEYS;
  const generation = clientGenerationContract(body);
  const common = {
    ...generation,
    script_kind: script.kind,
    api_mode: script.apiMode,
    role: classified.role,
    role_evidence: classified.evidence,
    model_sha256: model === null ? null : sha256(Buffer.from(model, "utf8")),
    top_level_keys: topLevelKeys,
    model_matches: model === modelId,
    top_level_keys_match: JSON.stringify(topLevelKeys) === JSON.stringify(expectedKeys),
    stream_true: body?.stream === true,
    include_usage_true: exactKeys(body?.stream_options, ["include_usage"])
      && body.stream_options.include_usage === true,
    n_one: body?.n === 1,
    max_tokens_absent: !Object.hasOwn(body ?? {}, "max_tokens")
      && !Object.hasOwn(body ?? {}, "max_output_tokens"),
  };
  if (guardianShape) {
    const samplingFieldsPresent = GUARDIAN_CHAT_SAMPLING_KEYS
      .filter((key) => Object.hasOwn(body ?? {}, key));
    const reasoningFieldsPresent = GUARDIAN_CHAT_REASONING_KEYS
      .filter((key) => Object.hasOwn(body ?? {}, key));
    const guardian = {
      ...common,
      sampling_fields_present: samplingFieldsPresent,
      sampling_absent: samplingFieldsPresent.length === 0,
      reasoning_fields_present: reasoningFieldsPresent,
      reasoning_absent: reasoningFieldsPresent.length === 0,
      tools_absent: !Object.hasOwn(body ?? {}, "tools")
        && !Object.hasOwn(body ?? {}, "tool_choice")
        && !Object.hasOwn(body ?? {}, "parallel_tool_calls"),
    };
    return {
      ...guardian,
      pass: guardian.client_generation_fields_absent
        && guardian.role === "guardian_review"
        && guardian.model_matches
        && guardian.top_level_keys_match
        && guardian.stream_true
        && guardian.include_usage_true
        && guardian.n_one
        && guardian.max_tokens_absent
        && guardian.sampling_absent
        && guardian.reasoning_absent
        && guardian.tools_absent,
    };
  }
  const task = {
    ...common,
    parallel_tool_calls_false: body?.parallel_tool_calls === false,
    tools: chatShellToolsContract(body?.tools),
  };
  return {
    ...task,
    pass: task.client_generation_fields_absent
      && task.role !== null
      && task.model_matches
      && task.top_level_keys_match
      && task.stream_true
      && task.include_usage_true
      && task.n_one
      && task.max_tokens_absent
      && task.parallel_tool_calls_false
      && task.tools.pass,
  };
}

function permissionTempEscalationExpected(script) {
  return {
    restrictedArguments: JSON.stringify({
      command: script.command,
      sandbox_permissions: "use_default",
    }),
    elevatedArguments: JSON.stringify({
      command: script.command,
      sandbox_permissions: "require_escalated",
      justification: script.justification,
    }),
  };
}

function permissionTempEscalationCallMatches(item, callId, argumentsJson) {
  return exactKeys(item, ["arguments", "call_id", "name", "type"])
    && item.type === "function_call"
    && item.call_id === callId
    && item.name === "shell"
    && item.arguments === argumentsJson;
}

function permissionTempEscalationOutput(item, callId) {
  return exactKeys(item, ["call_id", "output", "type"])
    && item.type === "function_call_output"
    && item.call_id === callId
    && typeof item.output === "string"
    && item.output.trim().length > 0
    ? item.output
    : null;
}

function permissionTempFailureOutputContract(output, script) {
  const text = typeof output === "string" ? output : "";
  const lines = text.split(/\r?\n/u);
  const noteCount = text.split("Sandbox note:").length - 1;
  const contract = {
    output_non_empty: text.trim().length > 0,
    command_matches: lines.includes(`Command: ${script.command}`),
    host_non_success: text.includes("Tool outcome (host projection): non-success"),
    shell_tool: text.includes('tool: "shell"'),
    completed_lifecycle: text.includes("lifecycle_status: completed"),
    hint_kind: text.includes("kind: workspace_write_effect_temp_access_denied"),
    automatic_retry_false: text.includes("automatic_retry: false"),
    exit_code_one: text.includes("exit_code: 1"),
    guidance_present: text.includes("guidance: Sandbox note:"),
    exact_escalation_hint: text.includes("sandbox_permissions=require_escalated"),
    no_project_workaround_hint: text.includes(
      "do not change project files solely to bypass this sandbox restriction",
    ),
    single_note: noteCount === 1,
    same_line_windows_signature: /^.*permissionerror: \[winerror 5\].*moyai-sandbox-effect-.*$/imu
      .test(text),
  };
  return {
    ...contract,
    output_size_bytes: output === null ? null : Buffer.byteLength(text, "utf8"),
    output_sha256: output === null ? null : sha256(Buffer.from(text, "utf8")),
    pass: Object.values(contract).every((value) => value === true),
  };
}

function permissionTempSuccessOutputContract(output, script) {
  const text = typeof output === "string" ? output : "";
  const lines = text.split(/\r?\n/u);
  const contract = {
    output_non_empty: text.trim().length > 0,
    command_matches: lines.includes(`Command: ${script.command}`),
    exit_code_zero: text.includes("Exit code: 0"),
    pytest_passed: /(?:^|\n)1 passed(?:\s|$)/u.test(text),
    effect_temp_absent: !text.includes("moyai-sandbox-effect-"),
    sandbox_note_absent: !text.includes("Sandbox note:"),
  };
  return {
    ...contract,
    output_size_bytes: output === null ? null : Buffer.byteLength(text, "utf8"),
    output_sha256: output === null ? null : sha256(Buffer.from(text, "utf8")),
    pass: Object.values(contract).every((value) => value === true),
  };
}

function permissionTempEscalationMainRole(body, script) {
  const input = Array.isArray(body?.input) ? body.input : [];
  const expected = permissionTempEscalationExpected(script);
  const taskUser = exactInputText(input[0]);
  const restrictedCall = input[1];
  const restrictedOutputItem = input[2];
  const elevatedCall = input[3];
  const elevatedOutputItem = input[4];
  const restrictedCallMatches = permissionTempEscalationCallMatches(
    restrictedCall,
    PERMISSION_TEMP_ESCALATION_RESTRICTED_CALL_ID,
    expected.restrictedArguments,
  );
  const restrictedOutput = permissionTempEscalationOutput(
    restrictedOutputItem,
    PERMISSION_TEMP_ESCALATION_RESTRICTED_CALL_ID,
  );
  const restrictedOutputContract = permissionTempFailureOutputContract(restrictedOutput, script);
  const elevatedCallMatches = permissionTempEscalationCallMatches(
    elevatedCall,
    PERMISSION_TEMP_ESCALATION_ELEVATED_CALL_ID,
    expected.elevatedArguments,
  );
  const elevatedOutput = permissionTempEscalationOutput(
    elevatedOutputItem,
    PERMISSION_TEMP_ESCALATION_ELEVATED_CALL_ID,
  );
  const elevatedOutputContract = permissionTempSuccessOutputContract(elevatedOutput, script);
  const initial = input.length === 1 && taskUser === script.taskPrompt;
  const escalation = input.length === 3
    && taskUser === script.taskPrompt
    && restrictedCallMatches
    && restrictedOutputContract.pass;
  const continuation = input.length === 5
    && taskUser === script.taskPrompt
    && restrictedCallMatches
    && restrictedOutputContract.pass
    && elevatedCallMatches
    && elevatedOutputContract.pass;
  const role = initial
    ? "temp_initial"
    : escalation
      ? "temp_escalation"
      : continuation
        ? "temp_continuation"
        : null;
  return {
    role,
    evidence: {
      input_count: input.length,
      input_item_types: input.map((item) => typeof item?.type === "string" ? item.type : null),
      task_prompt_sha256: taskUser === null ? null : sha256(Buffer.from(taskUser, "utf8")),
      task_prompt_matches: taskUser === script.taskPrompt,
      restricted_call_matches: restrictedCallMatches,
      restricted_output: restrictedOutputContract,
      elevated_call_matches: elevatedCallMatches,
      elevated_output: elevatedOutputContract,
    },
  };
}

function permissionTempGuardianPayloadContract(inputText, script) {
  let payload = null;
  let taskContext = null;
  try {
    payload = JSON.parse(inputText);
    taskContext = typeof payload?.task_context === "string"
      ? JSON.parse(payload.task_context)
      : null;
  } catch {
    // Invalid or wrapped evidence fails the exact Guardian request contract below.
  }
  const payloadKeysMatch = exactKeys(payload, [
    "action_evidence",
    "permission_request",
    "recent_committed_response",
    "task_context",
    "trusted_world_state",
  ]);
  const authority = Array.isArray(taskContext?.canonical_user_authority)
    ? taskContext.canonical_user_authority
    : [];
  const authorityTexts = authority.map((item) => typeof item?.text === "string" ? item.text : null);
  const authorityIds = authority.map((item) => typeof item?.history_item_id === "string"
    ? item.history_item_id
    : null);
  const authorityMatches = exactKeys(taskContext, ["authority_session_id", "canonical_user_authority"])
    && typeof taskContext.authority_session_id === "string"
    && taskContext.authority_session_id.length > 0
    && authority.length === 1
    && authority.every((item) => exactKeys(item, ["history_item_id", "kind", "text"])
      && item.kind === "user_turn"
      && typeof item.history_item_id === "string"
      && item.history_item_id.length > 0)
    && authorityTexts[0] === script.taskPrompt
    && new Set(authorityIds).size === authorityIds.length;
  const recent = payload?.recent_committed_response;
  const toolRequest = recent?.tool_request;
  const expected = permissionTempEscalationExpected(script);
  const recentMatches = exactKeys(recent, [
    "assistant_text",
    "prior_committed_tool_results",
    "response_id",
    "tool_request",
  ])
    && typeof recent.response_id === "string"
    && recent.response_id.length > 0
    && typeof recent.assistant_text === "string"
    && Array.isArray(recent.prior_committed_tool_results)
    && recent.prior_committed_tool_results.length === 0
    && exactKeys(toolRequest, ["arguments_json", "call_id", "tool_name"])
    && toolRequest.call_id === PERMISSION_TEMP_ESCALATION_ELEVATED_CALL_ID
    && toolRequest.tool_name === "shell"
    && toolRequest.arguments_json === expected.elevatedArguments;
  const permission = payload?.permission_request;
  const permissionMatches = exactKeys(permission, [
    "access",
    "details",
    "outside_workspace",
    "risks",
    "summary",
    "targets",
  ])
    && permission.access === "shell"
    && typeof permission.summary === "string"
    && permission.summary.trim().length > 0
    && Array.isArray(permission.details)
    && permission.details.some((detail) => (
      detail === `Requested sandbox elevation: ${script.justification}`
    ))
    && Array.isArray(permission.targets)
    && permission.targets.length > 0
    && permission.targets.every((target) => typeof target === "string" && target.length > 0)
    && permission.outside_workspace === true
    && Array.isArray(permission.risks)
    && permission.risks.length === 0;
  const evidenceMatches = exactKeys(payload?.action_evidence, ["kind"])
    && payload.action_evidence.kind === "permission_request";
  const worldStateMatches = payload?.trusted_world_state !== null
    && typeof payload?.trusted_world_state === "object"
    && !Array.isArray(payload.trusted_world_state)
    && Object.keys(payload.trusted_world_state).length > 0;
  return {
    json_valid: payload !== null,
    payload_keys_match: payloadKeysMatch,
    authority_count: authority.length,
    authority_text_hashes: authorityTexts.map((text) => text === null
      ? null
      : sha256(Buffer.from(text, "utf8"))),
    authority_identity_hashes: authorityIds.map((id) => id === null
      ? null
      : sha256(Buffer.from(id, "utf8"))),
    authority_matches: authorityMatches,
    recent_committed_response_matches: recentMatches,
    permission_request_matches: permissionMatches,
    action_evidence_matches: evidenceMatches,
    trusted_world_state_matches: worldStateMatches,
    pass: payloadKeysMatch
      && authorityMatches
      && recentMatches
      && permissionMatches
      && evidenceMatches
      && worldStateMatches,
  };
}

function permissionTempEscalationGuardianRole(body, script) {
  const input = Array.isArray(body?.input) ? body.input : [];
  const inputText = input.length === 1 ? exactInputText(input[0]) : null;
  const payload = permissionTempGuardianPayloadContract(inputText ?? "", script);
  return {
    role: input.length === 1 && payload.pass ? "temp_guardian" : null,
    evidence: {
      input_count: input.length,
      input_text_size_bytes: inputText === null ? null : Buffer.byteLength(inputText, "utf8"),
      input_text_sha256: inputText === null ? null : sha256(Buffer.from(inputText, "utf8")),
      payload,
    },
  };
}

function permissionTempEscalationRequestContract(body, modelId, script) {
  const guardianShape = Object.hasOwn(body ?? {}, "reasoning")
    || !Object.hasOwn(body ?? {}, "tools");
  const classified = guardianShape
    ? permissionTempEscalationGuardianRole(body, script)
    : permissionTempEscalationMainRole(body, script);
  const model = typeof body?.model === "string" ? body.model : null;
  const instructions = typeof body?.instructions === "string" ? body.instructions : null;
  const topLevelKeys = body !== null && typeof body === "object" && !Array.isArray(body)
    ? Object.keys(body).sort()
    : [];
  const expectedKeys = guardianShape ? GUARDIAN_RESPONSES_KEYS : TOOL_RESPONSES_KEYS;
  const tools = guardianShape ? null : shellToolsContract(body?.tools);
  const generation = clientGenerationContract(body);
  const common = {
    ...generation,
    script_kind: script.kind,
    role: classified.role,
    role_evidence: classified.evidence,
    model_sha256: model === null ? null : sha256(Buffer.from(model, "utf8")),
    instructions_sha256: instructions === null ? null : sha256(Buffer.from(instructions, "utf8")),
    top_level_keys: topLevelKeys,
    model_matches: model === modelId,
    instructions_non_empty: instructions !== null && instructions.trim().length > 0,
    top_level_keys_match: JSON.stringify(topLevelKeys) === JSON.stringify(expectedKeys),
    stream_true: body?.stream === true,
    store_false: body?.store === false,
  };
  if (guardianShape) {
    const guardian = {
      ...common,
      guardian_instructions_match: instructions?.includes("independent permission guardian") === true,
      max_output_tokens_absent: !Object.hasOwn(body ?? {}, "max_output_tokens"),
      reasoning_absent: !Object.hasOwn(body ?? {}, "reasoning"),
      tools_absent: !Object.hasOwn(body ?? {}, "tools")
        && !Object.hasOwn(body ?? {}, "tool_choice")
        && !Object.hasOwn(body ?? {}, "parallel_tool_calls"),
    };
    return {
      ...guardian,
      pass: guardian.client_generation_fields_absent
        && guardian.role === "temp_guardian"
        && guardian.model_matches
        && guardian.instructions_non_empty
        && guardian.guardian_instructions_match
        && guardian.top_level_keys_match
        && guardian.max_output_tokens_absent
        && guardian.reasoning_absent
        && guardian.tools_absent
        && guardian.stream_true
        && guardian.store_false,
    };
  }
  const task = {
    ...common,
    max_output_tokens_absent: !Object.hasOwn(body ?? {}, "max_output_tokens"),
    tool_choice_auto: body?.tool_choice === "auto",
    parallel_tool_calls_false: body?.parallel_tool_calls === false,
    tools,
  };
  return {
    ...task,
    pass: task.client_generation_fields_absent
      && task.role !== null
      && task.model_matches
      && task.instructions_non_empty
      && task.top_level_keys_match
      && task.max_output_tokens_absent
      && task.tool_choice_auto
      && task.parallel_tool_calls_false
      && task.tools.pass
      && task.stream_true
      && task.store_false,
  };
}

function catalog(modelId, supportsTools = false) {
  return {
    object: "list",
    data: [{
      id: modelId,
      object: "model",
      owned_by: "moyai-desktop-e2e",
      context_window: 65_536,
      max_output_tokens: 1_024,
      max_parallel_predictions: 1,
      capabilities: { tools: supportsTools, reasoning: false, vision: false },
    }],
  };
}

function lmStudioCatalog(modelId, supportsTools = false) {
  return {
    models: [{
      key: modelId,
      type: "llm",
      display_name: "moyAI Desktop E2E scripted model",
      loaded_instances: [{
        id: `${modelId}:loaded`,
        context_length: 65_536,
        max_prediction_tokens: 1_024,
        max_parallel_predictions: 1,
      }],
      capabilities: {
        trained_for_tool_use: supportsTools,
        reasoning: false,
        vision: false,
      },
    }],
  };
}

function responsesEvents(responseText, {
  itemId = "msg_main_ok",
  responseId = "resp_main_ok",
  streamedText = responseText,
  streamedDeltas = null,
} = {}) {
  const deltas = streamedDeltas === null ? [streamedText] : streamedDeltas;
  const item = {
    type: "message",
    id: itemId,
    role: "assistant",
    content: [{ type: "output_text", text: responseText }],
  };
  return [
    ...deltas.map((delta) => ({
      type: "response.output_text.delta",
      item_id: item.id,
      output_index: 0,
      delta,
    })),
    { type: "response.output_item.done", output_index: 0, item },
    {
      type: "response.completed",
      response: {
        id: responseId,
        output: [item],
        usage: {
          input_tokens: 4,
          output_tokens: 2,
          total_tokens: 6,
          output_tokens_details: { reasoning_tokens: 0 },
        },
      },
    },
  ];
}

function responsesSse(responseText, options = {}) {
  const events = responsesEvents(responseText, options);
  return events.map((event) => `data: ${JSON.stringify(event)}\n\n`).join("");
}

function splitPacedResponseText(responseText, deltaCount) {
  const characters = Array.from(responseText);
  if (characters.length < deltaCount) {
    throw new TypeError(
      `paced response text must contain at least ${deltaCount} Unicode characters`,
    );
  }
  return Array.from({ length: deltaCount }, (_, index) => {
    const start = Math.floor((index * characters.length) / deltaCount);
    const end = Math.floor(((index + 1) * characters.length) / deltaCount);
    return characters.slice(start, end).join("");
  });
}

function elapsedMonotonicMs(startedAt) {
  return Number((process.hrtime.bigint() - startedAt) / 1_000_000n);
}

function waitForPacingIntervalOrClose(milliseconds, closePromise) {
  return new Promise((resolve) => {
    let settled = false;
    const finish = (value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(value);
    };
    const timer = setTimeout(() => finish("elapsed"), milliseconds);
    closePromise.then(() => finish("closed"));
  });
}

async function waitForGuardianResponseDelay(response, row, configuredDelayMs) {
  const startedAt = process.hrtime.bigint();
  const observation = {
    schema_version: "desktop-e2e.scripted-provider-guardian-delay.v1",
    configured_delay_ms: configuredDelayMs,
    delay_elapsed_ms: null,
    delay_completed: false,
    peer_close_observed: false,
    headers_sent_elapsed_ms: null,
  };
  row.response_delay = observation;
  let resolveClose;
  const closePromise = new Promise((resolve) => { resolveClose = resolve; });
  const markPeerClose = () => {
    observation.peer_close_observed = true;
    resolveClose();
  };
  response.once("close", markPeerClose);
  response.once("error", markPeerClose);
  row.response_phase = "delaying";
  let outcome = "elapsed";
  while (outcome === "elapsed") {
    const remainingMs = configuredDelayMs - elapsedMonotonicMs(startedAt);
    if (remainingMs <= 0) break;
    outcome = await waitForPacingIntervalOrClose(remainingMs, closePromise);
  }
  observation.delay_elapsed_ms = elapsedMonotonicMs(startedAt);
  response.off("close", markPeerClose);
  response.off("error", markPeerClose);
  if (outcome !== "elapsed" || response.destroyed || response.socket?.destroyed === true) {
    observation.peer_close_observed = true;
    row.response_phase = "peer_closed";
    return null;
  }
  observation.delay_completed = true;
  return { observation, startedAt };
}

async function writePacedResponses(response, row, responseText, pacing, options = {}) {
  const startedAt = process.hrtime.bigint();
  const deltas = splitPacedResponseText(responseText, pacing.deltaCount);
  const events = responsesEvents(responseText, { ...options, streamedDeltas: deltas });
  const observation = {
    schema_version: "desktop-e2e.scripted-provider-response-stream.v1",
    cadence_ms: pacing.cadenceMs,
    delta_count: pacing.deltaCount,
    configured_total_duration_ms: pacing.totalDurationMs,
    expected_event_count: events.length,
    headers_sent_elapsed_ms: null,
    events: [],
    terminal_sent: false,
    terminal_elapsed_ms: null,
    response_finished: false,
    response_finished_elapsed_ms: null,
    peer_close_observed: false,
    peer_close_elapsed_ms: null,
    peer_closed_before_terminal: false,
  };
  row.response_stream = observation;

  let resolveClose;
  let resolveFinish;
  const closePromise = new Promise((resolve) => { resolveClose = resolve; });
  const finishPromise = new Promise((resolve) => { resolveFinish = resolve; });
  const markPeerClose = () => {
    if (observation.response_finished || observation.peer_close_observed) return;
    observation.peer_close_observed = true;
    observation.peer_close_elapsed_ms = elapsedMonotonicMs(startedAt);
    observation.peer_closed_before_terminal = !observation.terminal_sent;
    resolveClose();
  };
  response.once("close", markPeerClose);
  response.once("error", markPeerClose);
  response.once("finish", () => {
    observation.response_finished = true;
    observation.response_finished_elapsed_ms = elapsedMonotonicMs(startedAt);
    resolveFinish();
  });

  response.sendDate = false;
  response.writeHead(200, {
    "cache-control": "no-store",
    connection: "close",
    "content-type": "text/event-stream",
  });
  response.flushHeaders();
  row.response_phase = "streaming";
  row.response_status = 200;
  observation.headers_sent_elapsed_ms = elapsedMonotonicMs(startedAt);

  for (let index = 0; index < events.length; index += 1) {
    if (index > 0) {
      const outcome = await waitForPacingIntervalOrClose(pacing.cadenceMs, closePromise);
      if (outcome === "closed") {
        row.response_phase = "peer_closed";
        return false;
      }
    }
    if (response.destroyed || response.socket?.destroyed === true) {
      markPeerClose();
      row.response_phase = "peer_closed";
      return false;
    }
    const event = events[index];
    const bytes = Buffer.from(`data: ${JSON.stringify(event)}\n\n`, "utf8");
    response.write(bytes);
    const elapsedMs = elapsedMonotonicMs(startedAt);
    observation.events.push({
      sequence: index + 1,
      event_type: event.type,
      elapsed_ms: elapsedMs,
      size_bytes: bytes.byteLength,
    });
    if (event.type === "response.completed") {
      observation.terminal_sent = true;
      observation.terminal_elapsed_ms = elapsedMs;
    }
  }

  response.end();
  const outcome = await Promise.race([
    finishPromise.then(() => "finished"),
    closePromise.then(() => "closed"),
  ]);
  if (outcome !== "finished") {
    row.response_phase = "peer_closed";
    return false;
  }
  row.response_phase = "completed";
  return true;
}

function chatCompletionSse(chunks) {
  return chunks.map((chunk) => `data: ${JSON.stringify(chunk)}\n\n`).join("");
}

function permissionRestartGuardianChatTextSse(modelId, id, text, {
  promptTokens,
  completionTokens,
}) {
  return chatCompletionSse([
    {
      id,
      object: "chat.completion.chunk",
      model: modelId,
      choices: [{
        index: 0,
        delta: { content: text },
        finish_reason: "stop",
      }],
    },
    {
      id,
      object: "chat.completion.chunk",
      model: modelId,
      choices: [],
      usage: {
        prompt_tokens: promptTokens,
        completion_tokens: completionTokens,
        total_tokens: promptTokens + completionTokens,
      },
    },
  ]);
}

function permissionRestartGuardianChatShellSse(modelId, script) {
  const id = "chatcmpl_permission_restart_guardian_shell";
  return chatCompletionSse([
    {
      id,
      object: "chat.completion.chunk",
      model: modelId,
      choices: [{
        index: 0,
        delta: {
          tool_calls: [{
            index: 0,
            id: PERMISSION_RESTART_GUARDIAN_SHELL_CALL_ID,
            type: "function",
            function: {
              name: "shell",
              arguments: permissionRestartGuardianExpected(script).shellArguments,
            },
          }],
        },
        finish_reason: "tool_calls",
      }],
    },
    {
      id,
      object: "chat.completion.chunk",
      model: modelId,
      choices: [],
      usage: { prompt_tokens: 24, completion_tokens: 8, total_tokens: 32 },
    },
  ]);
}

function chatToolContinuationCallSse(modelId) {
  const common = {
    id: "chatcmpl_chat_tool_initial",
    object: "chat.completion.chunk",
    model: modelId,
  };
  return chatCompletionSse([
    {
      ...common,
      choices: [{
        index: 0,
        delta: { content: "\n\n<|im_" },
        finish_reason: null,
      }],
    },
    {
      ...common,
      choices: [{
        index: 0,
        delta: {
          content: "start|>",
          tool_calls: [{
            index: 0,
            id: SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_CALL_ID,
            type: "function",
            function: { name: "current_time", arguments: "{}" },
          }],
        },
        finish_reason: "tool_calls",
      }],
    },
    {
      ...common,
      choices: [],
      usage: { prompt_tokens: 8, completion_tokens: 6, total_tokens: 14 },
    },
  ]);
}

function chatToolContinuationFinalSse(modelId) {
  return chatCompletionSse([
    {
      id: "chatcmpl_chat_continuation",
      object: "chat.completion.chunk",
      model: modelId,
      choices: [{
        index: 0,
        delta: { content: SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_RESPONSE },
        finish_reason: "stop",
      }],
    },
    {
      id: "chatcmpl_chat_continuation",
      object: "chat.completion.chunk",
      model: modelId,
      choices: [],
      usage: { prompt_tokens: 16, completion_tokens: 4, total_tokens: 20 },
    },
  ]);
}

function agentInterruptSpawnSse(script) {
  const item = {
    type: "function_call",
    id: AGENT_INTERRUPT_SPAWN_ITEM_ID,
    call_id: AGENT_INTERRUPT_SPAWN_CALL_ID,
    name: "spawn_agent",
    arguments: agentInterruptExpected(script).spawnArguments,
  };
  const events = [
    { type: "response.output_item.done", output_index: 0, item },
    {
      type: "response.completed",
      response: {
        id: "resp_agent_interrupt_spawn",
        output: [item],
        usage: {
          input_tokens: 8,
          output_tokens: 6,
          total_tokens: 14,
          output_tokens_details: { reasoning_tokens: 0 },
        },
      },
    },
  ];
  return events.map((event) => `data: ${JSON.stringify(event)}\n\n`).join("");
}

function toolErrorRecoveryReadSse(script) {
  const item = {
    type: "function_call",
    id: TOOL_ERROR_RECOVERY_READ_ITEM_ID,
    call_id: TOOL_ERROR_RECOVERY_READ_CALL_ID,
    name: "read",
    arguments: toolErrorRecoveryExpected(script).readArguments,
  };
  const events = [
    { type: "response.output_item.done", output_index: 0, item },
    {
      type: "response.completed",
      response: {
        id: "resp_tool_error_recovery_read",
        output: [item],
        usage: {
          input_tokens: 8,
          output_tokens: 6,
          total_tokens: 14,
          output_tokens_details: { reasoning_tokens: 0 },
        },
      },
    },
  ];
  return events.map((event) => `data: ${JSON.stringify(event)}\n\n`).join("");
}

function sideChatQuoteWriteSse() {
  const item = {
    type: "function_call",
    id: SIDE_CHAT_QUOTE_WRITE_ITEM_ID,
    call_id: SIDE_CHAT_QUOTE_WRITE_CALL_ID,
    name: "write",
    arguments: sideChatQuoteExpectedWrite().arguments,
  };
  const events = [
    { type: "response.output_item.done", output_index: 0, item },
    {
      type: "response.completed",
      response: {
        id: "resp_side_chat_quote_write",
        output: [item],
        usage: {
          input_tokens: 8,
          output_tokens: 6,
          total_tokens: 14,
          output_tokens_details: { reasoning_tokens: 0 },
        },
      },
    },
  ];
  return events.map((event) => `data: ${JSON.stringify(event)}\n\n`).join("");
}

function responsesCompactionReadSse(script, callIndex) {
  const expected = responsesCompactionReadExpected(script, callIndex);
  const item = {
    type: "function_call",
    id: expected.itemId,
    call_id: expected.callId,
    name: "read",
    arguments: expected.arguments,
  };
  const events = [
    { type: "response.output_item.done", output_index: 0, item },
    {
      type: "response.completed",
      response: {
        id: `resp_responses_compaction_read_${callIndex}`,
        output: [item],
        usage: {
          input_tokens: 8,
          output_tokens: 6,
          total_tokens: 14,
          output_tokens_details: { reasoning_tokens: 0 },
        },
      },
    },
  ];
  return events.map((event) => `data: ${JSON.stringify(event)}\n\n`).join("");
}

function responsesCompactionReasoningOnlyEvents() {
  const completionTokens = SCRIPTED_PROVIDER_RESPONSES_COMPACTION_SATURATION_TOTAL_TOKENS
    - SCRIPTED_PROVIDER_RESPONSES_COMPACTION_SATURATION_PROMPT_TOKENS;
  const item = {
    type: "reasoning",
    id: "rs_responses_compaction_saturated",
    summary: [],
  };
  return [
    { type: "response.output_item.added", output_index: 0, item },
    {
      type: "response.reasoning_text.delta",
      item_id: item.id,
      output_index: 0,
      delta: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RAW_REASONING_SENTINEL,
    },
    {
      type: "response.reasoning_text.done",
      item_id: item.id,
      output_index: 0,
      text: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RAW_REASONING_SENTINEL,
    },
    { type: "response.output_item.done", output_index: 0, item },
    {
      type: "response.completed",
      response: {
        id: "resp_responses_compaction_saturated",
        output: [item],
        usage: {
          input_tokens: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_SATURATION_PROMPT_TOKENS,
          output_tokens: completionTokens,
          total_tokens: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_SATURATION_TOTAL_TOKENS,
          output_tokens_details: { reasoning_tokens: completionTokens },
        },
      },
    },
  ];
}

function responsesCompactionReasoningOnlySse() {
  return responsesCompactionReasoningOnlyEvents()
    .map((event) => `data: ${JSON.stringify(event)}\n\n`)
    .join("");
}

async function writeHeldResponsesCompactionReasoning(response, row, releasePromise) {
  const startedAt = process.hrtime.bigint();
  const events = responsesCompactionReasoningOnlyEvents();
  const heldEventCount = 2;
  const observation = {
    schema_version: "desktop-e2e.scripted-provider-held-stream.v1",
    hold_after_event_type: events[heldEventCount - 1].type,
    events: [],
    release_observed: false,
    release_elapsed_ms: null,
    terminal_sent: false,
    response_finished: false,
    peer_close_observed: false,
  };
  row.response_stream = observation;

  let resolveClose;
  let resolveFinish;
  const closePromise = new Promise((resolve) => { resolveClose = resolve; });
  const finishPromise = new Promise((resolve) => { resolveFinish = resolve; });
  const markPeerClose = () => {
    if (observation.response_finished || observation.peer_close_observed) return;
    observation.peer_close_observed = true;
    resolveClose();
  };
  response.once("close", markPeerClose);
  response.once("error", markPeerClose);
  response.once("finish", () => {
    observation.response_finished = true;
    resolveFinish();
  });

  response.sendDate = false;
  response.writeHead(200, {
    "cache-control": "no-store",
    connection: "close",
    "content-type": "text/event-stream",
  });
  response.flushHeaders();
  row.response_phase = "held";
  row.response_status = 200;

  const writeEvent = (event, index) => {
    const bytes = Buffer.from(`data: ${JSON.stringify(event)}\n\n`, "utf8");
    response.write(bytes);
    observation.events.push({
      sequence: index + 1,
      event_type: event.type,
      elapsed_ms: elapsedMonotonicMs(startedAt),
      size_bytes: bytes.byteLength,
    });
  };
  events.slice(0, heldEventCount).forEach(writeEvent);

  const releaseOutcome = await Promise.race([
    releasePromise.then(() => "released"),
    closePromise.then(() => "closed"),
  ]);
  if (releaseOutcome !== "released") {
    row.response_phase = "peer_closed";
    return false;
  }
  observation.release_observed = true;
  observation.release_elapsed_ms = elapsedMonotonicMs(startedAt);
  events.slice(heldEventCount).forEach((event, index) => {
    writeEvent(event, heldEventCount + index);
    if (event.type === "response.completed") observation.terminal_sent = true;
  });
  response.end();
  const finishOutcome = await Promise.race([
    finishPromise.then(() => "finished"),
    closePromise.then(() => "closed"),
  ]);
  if (finishOutcome !== "finished") {
    row.response_phase = "peer_closed";
    return false;
  }
  row.response_phase = "completed";
  return true;
}

function permissionRestartGuardianShellSse(script) {
  const item = {
    type: "function_call",
    id: PERMISSION_RESTART_GUARDIAN_SHELL_ITEM_ID,
    call_id: PERMISSION_RESTART_GUARDIAN_SHELL_CALL_ID,
    name: "shell",
    arguments: permissionRestartGuardianExpected(script).shellArguments,
  };
  const events = [
    { type: "response.output_item.done", output_index: 0, item },
    {
      type: "response.completed",
      response: {
        id: "resp_permission_restart_guardian_shell",
        output: [item],
        usage: {
          input_tokens: 8,
          output_tokens: 6,
          total_tokens: 14,
          output_tokens_details: { reasoning_tokens: 0 },
        },
      },
    },
  ];
  return events.map((event) => `data: ${JSON.stringify(event)}\n\n`).join("");
}

function permissionTempEscalationShellSse(script, elevated) {
  const expected = permissionTempEscalationExpected(script);
  const item = {
    type: "function_call",
    id: elevated
      ? PERMISSION_TEMP_ESCALATION_ELEVATED_ITEM_ID
      : PERMISSION_TEMP_ESCALATION_RESTRICTED_ITEM_ID,
    call_id: elevated
      ? PERMISSION_TEMP_ESCALATION_ELEVATED_CALL_ID
      : PERMISSION_TEMP_ESCALATION_RESTRICTED_CALL_ID,
    name: "shell",
    arguments: elevated ? expected.elevatedArguments : expected.restrictedArguments,
  };
  const responseStem = elevated ? "elevated" : "restricted";
  const events = [
    { type: "response.output_item.done", output_index: 0, item },
    {
      type: "response.completed",
      response: {
        id: `resp_permission_temp_escalation_${responseStem}`,
        output: [item],
        usage: {
          input_tokens: 8,
          output_tokens: 6,
          total_tokens: 14,
          output_tokens_details: { reasoning_tokens: 0 },
        },
      },
    },
  ];
  return events.map((event) => `data: ${JSON.stringify(event)}\n\n`).join("");
}

function parsedTarget(request) {
  const target = new URL(request.url ?? "/", `http://${LOOPBACK_HOST}`);
  return {
    pathname: target.pathname,
    query_present: target.search.length > 0,
    exact_target: target.search.length === 0 && target.hash.length === 0,
  };
}

function routeFor(target) {
  if (!target.exact_target) return "unknown";
  if (target.pathname === "/v1/models") return "models";
  if (target.pathname === "/api/v1/models") return "lm_studio_models";
  if (target.pathname === "/v1/responses") return "responses";
  if (target.pathname === "/v1/chat/completions") return "chat_completions";
  if (target.pathname === "/ready") return "docling_readiness";
  return "unknown";
}

function writeEmptyResponse(response, status) {
  response.sendDate = false;
  response.writeHead(status, {
    "cache-control": "no-store",
    connection: "close",
    "content-length": "0",
  });
  response.end();
}

function closeServer(server, sockets) {
  return new Promise((resolve, reject) => {
    let forcedConnectionCount = 0;
    let serverCloseReturned = false;
    let settled = false;
    const complete = (callback) => {
      if (settled) return;
      settled = true;
      clearInterval(settlementPoll);
      clearTimeout(forceTimer);
      clearTimeout(hardTimer);
      callback();
    };
    const settleIfClosed = () => {
      if (serverCloseReturned && sockets.size === 0) {
        complete(() => resolve(forcedConnectionCount));
      }
    };
    const settlementPoll = setInterval(settleIfClosed, 5);
    const forceTimer = setTimeout(() => {
      forcedConnectionCount = sockets.size;
      for (const socket of sockets) socket.destroy();
      server.closeAllConnections?.();
    }, 250);
    const hardTimer = setTimeout(() => {
      complete(() => reject(new Error("scripted provider did not close within its resource deadline")));
    }, 5_000);
    server.close((error) => {
      if (error) {
        complete(() => reject(error));
        return;
      }
      serverCloseReturned = true;
      settleIfClosed();
    });
    server.closeIdleConnections?.();
  });
}

function waitForPeerClose(response) {
  if (response.destroyed || response.socket?.destroyed === true) return Promise.resolve();
  return new Promise((resolve) => response.once("close", resolve));
}

function listenOnEphemeralLoopback(server) {
  return new Promise((resolve, reject) => {
    const onError = (error) => {
      server.off("listening", onListening);
      reject(error);
    };
    const onListening = () => {
      server.off("error", onError);
      resolve();
    };
    server.once("error", onError);
    server.once("listening", onListening);
    server.listen({ host: LOOPBACK_HOST, port: 0, exclusive: true });
  });
}

function closeEphemeralListener(server) {
  return new Promise((resolve, reject) => {
    server.close((error) => {
      if (error) reject(error);
      else resolve();
    });
  });
}

export class ScriptedProvider {
  #server;
  #sockets = new Set();
  #ledger = [];
  #acceptedRoles = new Set();
  #activeRequestCount = 0;
  #acceptedResponseCount = 0;
  #successfulResponseCount = 0;
  #scriptedResponsesRequestCount = 0;
  #address = null;
  #closed = false;
  #closePromise = null;
  #closeObservation = null;
  #doclingReadinessStatus;
  #doclingReadinessRequestCount = 0;
  #doclingReadinessReleased = false;
  #doclingReadinessReleasedByCleanup = false;
  #releaseDoclingReadiness;
  #doclingReadinessRelease;
  #responseReleases = [];
  #scriptRoleRelease = null;

  constructor({
    modelId = SCRIPTED_PROVIDER_MODEL_ID,
    expectedPrompt = SCRIPTED_PROVIDER_PROMPT,
    responseText = SCRIPTED_PROVIDER_RESPONSE,
    maxBodyBytes = SCRIPTED_PROVIDER_MAX_BODY_BYTES,
    responseBehavior: configuredResponseBehavior = "complete",
    responsePacing: configuredResponsePacing = null,
    doclingReadinessStatus = null,
    turns = null,
    orderedConversation = false,
    script = null,
  } = {}) {
    this.modelId = nonEmptyString(modelId, "modelId");
    this.expectedPrompt = nonEmptyString(expectedPrompt, "expectedPrompt");
    this.responseText = nonEmptyString(responseText, "responseText");
    this.turns = scriptedTurns(turns, this.expectedPrompt, this.responseText);
    if (typeof orderedConversation !== "boolean") {
      throw new TypeError("orderedConversation must be boolean");
    }
    if (orderedConversation && (turns === null || this.turns.length < 2)) {
      throw new TypeError("orderedConversation requires at least two explicit turns");
    }
    this.orderedConversation = orderedConversation;
    this.maxBodyBytes = positiveInteger(maxBodyBytes, "maxBodyBytes");
    this.responseBehavior = responseBehavior(configuredResponseBehavior);
    this.responsePacing = responsePacing(configuredResponsePacing);
    this.#doclingReadinessStatus = optionalHttpStatus(doclingReadinessStatus, "doclingReadinessStatus");
    this.#doclingReadinessRelease = new Promise((resolve) => { this.#releaseDoclingReadiness = resolve; });
    this.script = providerScript(script);
    if (this.responsePacing !== null && this.responseBehavior !== "complete") {
      throw new TypeError("paced Responses require complete response behavior");
    }
    if (this.responsePacing !== null && this.script !== null) {
      throw new TypeError("paced Responses cannot use a scripted provider mode");
    }
    if (this.responsePacing !== null) {
      for (const turn of this.turns) {
        splitPacedResponseText(turn.responseText, this.responsePacing.deltaCount);
      }
    }
    const releaseHeldToolErrorInitial = this.script?.kind === SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_KIND
      && this.responseBehavior === "hold_until_release";
    const releaseHeldGuardianToolInitial = this.script?.kind
      === SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_KIND
      && this.responseBehavior === "hold_until_release";
    const releaseHeldTempEscalation = this.script?.kind
      === SCRIPTED_PROVIDER_PERMISSION_TEMP_ESCALATION_KIND
      && this.responseBehavior === "hold_until_release";
    const releaseHeldChatContinuation = this.script?.kind
      === SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_KIND
      && this.responseBehavior === "hold_until_release";
    const releaseHeldResponsesCompaction = this.script?.kind
      === SCRIPTED_PROVIDER_RESPONSES_COMPACTION_KIND
      && this.responseBehavior === "hold_until_release";
    if (this.script?.kind === SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_KIND
      && !releaseHeldChatContinuation) {
      throw new TypeError("Chat tool continuation script requires hold_until_release behavior");
    }
    if (this.script !== null
      && this.responseBehavior !== "complete"
      && !releaseHeldToolErrorInitial
      && !releaseHeldGuardianToolInitial
      && !releaseHeldTempEscalation
      && !releaseHeldChatContinuation
      && !releaseHeldResponsesCompaction) {
      throw new TypeError("scripted provider mode owns its response lifecycle");
    }
    if (this.script !== null && turns !== null) {
      throw new TypeError("scripted provider mode cannot use ordinary turns");
    }
    if (this.responseBehavior === "hold_until_release") {
      if (releaseHeldGuardianToolInitial
        || releaseHeldTempEscalation
        || releaseHeldChatContinuation
        || releaseHeldResponsesCompaction) {
        let release;
        const promise = new Promise((resolve) => { release = resolve; });
        this.#scriptRoleRelease = {
          role: releaseHeldGuardianToolInitial
            ? "guardian_tool_initial"
            : releaseHeldTempEscalation
              ? "temp_escalation"
              : releaseHeldChatContinuation ? "chat_continuation" : "compaction_empty",
          promise,
          release,
          released: false,
          releasedByCleanup: false,
        };
      } else {
        const releaseCount = releaseHeldToolErrorInitial ? 1 : this.turns.length;
        this.#responseReleases = Array.from({ length: releaseCount }, () => {
          let release;
          const promise = new Promise((resolve) => { release = resolve; });
          return { promise, release, released: false, releasedByCleanup: false };
        });
      }
    }
    if (this.responseBehavior === "hold_until_peer_close" && this.turns.length !== 1) {
      throw new TypeError("held scripted provider mode requires exactly one ordinary turn");
    }
    this.#server = http.createServer((request, response) => {
      const ledgerIndex = this.#ledger.length;
      this.#activeRequestCount += 1;
      void this.#handleRequest(request, response)
        .catch(() => {
          const row = this.#ledger[ledgerIndex];
          if (row && row.response_status === null) row.response_status = 500;
          if (!response.headersSent) fixedError(response, 500, "fixture_internal_error");
          else response.destroy();
        })
        .finally(() => { this.#activeRequestCount -= 1; });
    });
    this.#server.maxHeadersCount = 64;
    this.#server.requestTimeout = 5_000;
    this.#server.headersTimeout = 5_000;
    this.#server.keepAliveTimeout = 1;
    this.#server.on("connection", (socket) => {
      this.#sockets.add(socket);
      socket.once("close", () => this.#sockets.delete(socket));
    });
  }

  get baseUrl() {
    if (this.#address === null) throw new Error("scripted provider has not started");
    return `http://${LOOPBACK_HOST}:${this.#address.port}`;
  }

  get requestLedger() {
    return structuredClone(this.#ledger);
  }

  async start() {
    if (this.#address !== null || this.#closed) throw new Error("scripted provider can only start once");
    for (let attempt = 1; attempt <= MAX_FETCH_SAFE_BIND_ATTEMPTS; attempt += 1) {
      await listenOnEphemeralLoopback(this.#server);
      const address = this.#server.address();
      if (address === null || typeof address === "string" || address.address !== LOOPBACK_HOST) {
        await closeServer(this.#server, this.#sockets);
        throw new Error("scripted provider did not bind the exact IPv4 loopback owner");
      }
      if (scriptedProviderPortIsFetchSafe(address.port)) {
        this.#address = { host: LOOPBACK_HOST, port: address.port, family: "IPv4" };
        return this;
      }
      await closeEphemeralListener(this.#server);
    }
    throw new Error(
      `scripted provider did not acquire a Fetch-safe loopback port in ${MAX_FETCH_SAFE_BIND_ATTEMPTS} attempts`,
    );
  }

  resourceObservation() {
    return {
      schema_version: "desktop-e2e.scripted-provider-resource.v1",
      address: this.#address === null ? null : structuredClone(this.#address),
      base_url: this.#address === null ? null : this.baseUrl,
      listening: this.#server.listening,
      closed: this.#closed,
      active_request_count: this.#activeRequestCount,
      open_connection_count: this.#sockets.size,
      request_count: this.#ledger.length,
      accepted_response_count: this.#acceptedResponseCount,
      successful_response_count: this.#successfulResponseCount,
      script_kind: this.script?.kind ?? (this.orderedConversation
        ? "ordered_conversation_turns"
        : this.turns.length === 1 ? "single_turn" : "ordered_turns"),
      scripted_responses_request_count: this.#scriptedResponsesRequestCount,
      scripted_responses_maximum: this.script === null
        ? this.turns.length
        : scriptedResponseMaximum(this.script),
      scripted_response_roles: [...this.#acceptedRoles],
      docling_readiness_configured: this.#doclingReadinessStatus !== null,
      docling_readiness_status: this.#doclingReadinessStatus,
      docling_readiness_request_count: this.#doclingReadinessRequestCount,
      docling_readiness_released: this.#doclingReadinessReleased,
      docling_readiness_released_by_cleanup: this.#doclingReadinessReleasedByCleanup,
      response_release_controlled: this.responseBehavior === "hold_until_release",
      response_release_count: this.#responseReleases.filter((release) => release.released).length,
      response_release_cleanup_count: this.#responseReleases
        .filter((release) => release.releasedByCleanup).length,
      script_role_release: this.#scriptRoleRelease === null ? null : {
        role: this.#scriptRoleRelease.role,
        released: this.#scriptRoleRelease.released,
        released_by_cleanup: this.#scriptRoleRelease.releasedByCleanup,
      },
      paced_response_configured: this.responsePacing !== null,
      paced_response_pacing: this.responsePacing === null ? null : {
        cadence_ms: this.responsePacing.cadenceMs,
        delta_count: this.responsePacing.deltaCount,
        configured_total_duration_ms: this.responsePacing.totalDurationMs,
      },
      paced_response_streams: this.#ledger
        .filter((row) => row.response_stream !== null)
        .map((row) => ({
          request_sequence: row.sequence,
          response_phase: row.response_phase,
          response_status: row.response_status,
          ...structuredClone(row.response_stream),
        })),
    };
  }

  releaseResponse(turnIndex) {
    if (this.responseBehavior !== "hold_until_release") {
      throw new Error("scripted response release is not configured");
    }
    if (!Number.isInteger(turnIndex) || turnIndex < 0 || turnIndex >= this.#responseReleases.length) {
      throw new TypeError("scripted response turn index is invalid");
    }
    const release = this.#responseReleases[turnIndex];
    if (release.released) throw new Error("scripted response was already released");
    const rows = this.#ledger.filter((row) => row.route === "responses" && row.contract?.pass === true);
    const row = rows[turnIndex];
    if (row?.response_phase !== "held" || row?.response_status !== null) {
      throw new Error("scripted response release requires one exact held request");
    }
    release.released = true;
    release.release();
    return { released: true, turn_index: turnIndex, request: structuredClone(row) };
  }

  releaseScriptRole(role) {
    const release = this.#scriptRoleRelease;
    if (release === null) throw new Error("scripted role response release is not configured");
    if (role !== release.role) throw new Error("scripted role response release target is invalid");
    if (release.released) throw new Error("scripted role response was already released");
    const expectedRoute = role === "chat_continuation"
      || (this.script?.kind === SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_KIND
        && this.script.apiMode === "chat_completions")
      ? "chat_completions"
      : "responses";
    const rows = this.#ledger.filter((row) => row.route === expectedRoute
      && row.contract?.pass === true
      && row.contract?.role === role);
    const expectedHeldStatus = role === "compaction_empty" ? 200 : null;
    if (rows.length !== 1
      || rows[0].response_phase !== "held"
      || rows[0].response_status !== expectedHeldStatus) {
      throw new Error("scripted role response release requires one exact held request");
    }
    release.released = true;
    release.release();
    return { released: true, role, request: structuredClone(rows[0]) };
  }

  releaseDoclingReadiness() {
    if (this.#doclingReadinessStatus === null) {
      throw new Error("scripted Docling readiness is not configured");
    }
    if (this.#doclingReadinessReleased) throw new Error("scripted Docling readiness was already released");
    const rows = this.#ledger.filter((row) => row.route === "docling_readiness");
    if (rows.length !== 1 || rows[0].response_phase !== "held" || rows[0].response_status !== null) {
      throw new Error("scripted Docling readiness release requires one exact held request");
    }
    this.#doclingReadinessReleased = true;
    this.#releaseDoclingReadiness();
    return {
      released: true,
      response_status: this.#doclingReadinessStatus,
      request: structuredClone(rows[0]),
    };
  }

  async close() {
    if (this.#closeObservation !== null) return structuredClone(this.#closeObservation);
    if (this.#closePromise !== null) return structuredClone(await this.#closePromise);
    if (this.#address === null) throw new Error("scripted provider cannot close before it starts");
    const before = this.resourceObservation();
    this.#closePromise = (async () => {
      if (this.#doclingReadinessStatus !== null && !this.#doclingReadinessReleased) {
        this.#doclingReadinessReleased = true;
        this.#doclingReadinessReleasedByCleanup = true;
        this.#releaseDoclingReadiness();
      }
      for (const release of this.#responseReleases) {
        if (release.released) continue;
        release.released = true;
        release.releasedByCleanup = true;
        release.release();
      }
      if (this.#scriptRoleRelease !== null && !this.#scriptRoleRelease.released) {
        this.#scriptRoleRelease.released = true;
        this.#scriptRoleRelease.releasedByCleanup = true;
        this.#scriptRoleRelease.release();
      }
      const forcedConnectionCount = await closeServer(this.#server, this.#sockets);
      this.#closed = true;
      const after = this.resourceObservation();
      const observation = {
        schema_version: "desktop-e2e.scripted-provider-close.v1",
        before,
        after,
        forced_connection_count: forcedConnectionCount,
        pass: before.listening === true
          && after.listening === false
          && after.closed === true
          && after.active_request_count === 0
          && after.open_connection_count === 0,
      };
      this.#closeObservation = observation;
      return observation;
    })();
    return structuredClone(await this.#closePromise);
  }

  async #handleRequest(request, response) {
    const target = parsedTarget(request);
    const route = routeFor(target);
    const row = {
      schema_version: "desktop-e2e.scripted-provider-request.v1",
      sequence: this.#ledger.length + 1,
      method: request.method ?? null,
      pathname: target.pathname,
      query_present: target.query_present,
      route,
      request_headers: requestHeaderObservation(request.headers),
      body: null,
      contract: null,
      response_phase: null,
      response_status: null,
      response_stream: null,
      response_delay: null,
    };
    this.#ledger.push(row);

    if (route === "unknown") {
      row.response_phase = "rejected";
      row.response_status = 404;
      fixedError(response, 404, "not_found");
      request.resume();
      return;
    }
    const chatCompletionsScriptMode = this.script?.kind
      === SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_KIND
      || (this.script?.kind === SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_KIND
        && this.script.apiMode === "chat_completions");
    if ((route === "chat_completions" && !chatCompletionsScriptMode)
      || (route === "responses" && chatCompletionsScriptMode)) {
      row.response_phase = "rejected";
      row.response_status = 404;
      fixedError(response, 404, "not_found");
      request.resume();
      return;
    }
    if (route === "docling_readiness") {
      if (request.method !== "GET") {
        row.response_phase = "rejected";
        row.response_status = 405;
        fixedError(response, 405, "method_not_allowed", { allow: "GET" });
        request.resume();
        return;
      }
      if (this.#doclingReadinessStatus === null) {
        row.response_phase = "rejected";
        row.response_status = 404;
        fixedError(response, 404, "docling_readiness_not_configured");
        request.resume();
        return;
      }
      if (this.#doclingReadinessRequestCount !== 0) {
        row.response_phase = "rejected";
        row.response_status = 409;
        fixedError(response, 409, "docling_readiness_already_consumed");
        request.resume();
        return;
      }
      this.#doclingReadinessRequestCount += 1;
      row.contract = { pass: true, expected_method: "GET", expected_pathname: "/ready" };
      row.response_phase = "held";
      request.resume();
      await this.#doclingReadinessRelease;
      row.response_phase = "completed";
      row.response_status = this.#doclingReadinessStatus;
      writeEmptyResponse(response, this.#doclingReadinessStatus);
      return;
    }
    if (route === "models" || route === "lm_studio_models") {
      if (request.method !== "GET") {
        row.response_phase = "rejected";
        row.response_status = 405;
        fixedError(response, 405, "method_not_allowed", { allow: "GET" });
        request.resume();
        return;
      }
      row.response_phase = "completed";
      row.response_status = 200;
      writeResponse(
        response,
        200,
        "application/json; charset=utf-8",
        route === "lm_studio_models"
          ? lmStudioCatalog(this.modelId, this.script !== null)
          : catalog(this.modelId, this.script !== null),
      );
      request.resume();
      return;
    }
    if (request.method !== "POST") {
      row.response_phase = "rejected";
      row.response_status = 405;
      fixedError(response, 405, "method_not_allowed", { allow: "POST" });
      request.resume();
      return;
    }

    const body = await readBoundedBody(request, this.maxBodyBytes);
    const decoded = decodeJsonBody(body);
    row.body = {
      size_bytes: body.size_bytes,
      sha256: body.sha256,
      limit_exceeded: body.limit_exceeded,
      utf8_valid: decoded.utf8_valid,
      json_valid: decoded.json_valid,
    };
    if (body.limit_exceeded) {
      row.response_phase = "rejected";
      row.response_status = 413;
      fixedError(response, 413, "request_body_too_large");
      return;
    }
    if (!row.request_headers.content_type_is_application_json) {
      row.response_phase = "rejected";
      row.response_status = 415;
      fixedError(response, 415, "content_type_not_application_json");
      return;
    }
    if (!decoded.utf8_valid || !decoded.json_valid) {
      row.response_phase = "rejected";
      row.response_status = 400;
      fixedError(response, 400, "invalid_json");
      return;
    }
    if (this.script !== null) {
      this.#scriptedResponsesRequestCount += 1;
      if (this.script.kind === SCRIPTED_PROVIDER_AGENT_INTERRUPT_KIND) {
        row.contract = agentInterruptRequestContract(
          decoded.value,
          this.modelId,
          this.expectedPrompt,
          this.script,
        );
      } else if (this.script.kind === SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_KIND) {
        row.contract = toolErrorRecoveryRequestContract(
          decoded.value,
          this.modelId,
          this.expectedPrompt,
          this.script,
        );
      } else if (this.script.kind === SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_KIND) {
        row.contract = chatToolContinuationRequestContract(
          decoded.value,
          this.modelId,
          this.script,
        );
      } else if (this.script.kind === SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_KIND) {
        row.contract = sideChatQuoteRequestContract(
          decoded.value,
          this.modelId,
          this.script,
        );
      } else if (this.script.kind === SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_KIND) {
        row.contract = sideChatSessionRequestContract(
          decoded.value,
          this.modelId,
          this.script,
        );
      } else if (this.script.kind === SCRIPTED_PROVIDER_RESPONSES_COMPACTION_KIND) {
        row.contract = responsesCompactionRequestContract(
          decoded.value,
          this.modelId,
          this.expectedPrompt,
          this.script,
        );
      } else if (this.script.kind === SCRIPTED_PROVIDER_PERMISSION_TEMP_ESCALATION_KIND) {
        row.contract = permissionTempEscalationRequestContract(
          decoded.value,
          this.modelId,
          this.script,
        );
      } else {
        row.contract = this.script.apiMode === "chat_completions"
          ? permissionRestartGuardianChatRequestContract(
            decoded.value,
            this.modelId,
            this.script,
          )
          : permissionRestartGuardianRequestContract(
            decoded.value,
            this.modelId,
            this.script,
          );
      }
      if (this.#scriptedResponsesRequestCount > scriptedResponseMaximum(this.script)) {
        row.response_phase = "rejected";
        row.response_status = 409;
        fixedError(response, 409, "scripted_response_request_limit_exceeded");
        return;
      }
      if (this.script.kind === SCRIPTED_PROVIDER_AGENT_INTERRUPT_KIND) {
        await this.#handleAgentInterruptResponse(response, row);
      } else if (this.script.kind === SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_KIND) {
        await this.#handleToolErrorRecoveryResponse(response, row);
      } else if (this.script.kind === SCRIPTED_PROVIDER_CHAT_TOOL_CONTINUATION_KIND) {
        await this.#handleChatToolContinuationResponse(response, row);
      } else if (this.script.kind === SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_KIND) {
        await this.#handleSideChatQuoteResponse(response, row);
      } else if (this.script.kind === SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_KIND) {
        await this.#handleSideChatSessionResponse(response, row);
      } else if (this.script.kind === SCRIPTED_PROVIDER_RESPONSES_COMPACTION_KIND) {
        await this.#handleResponsesCompactionResponse(response, row);
      } else if (this.script.kind === SCRIPTED_PROVIDER_PERMISSION_TEMP_ESCALATION_KIND) {
        await this.#handlePermissionTempEscalationResponse(response, row);
      } else {
        await this.#handlePermissionRestartGuardianResponse(response, row);
      }
      return;
    }

    if (this.#acceptedResponseCount >= this.turns.length) {
      row.response_phase = "rejected";
      row.response_status = 409;
      fixedError(response, 409, "successful_response_already_consumed");
      return;
    }
    const turn = this.turns[this.#acceptedResponseCount];
    row.contract = this.orderedConversation
      ? orderedConversationRequestContract(
        decoded.value,
        this.modelId,
        this.turns,
        this.#acceptedResponseCount,
      )
      : requestContract(decoded.value, this.modelId, turn.prompt);
    if (!row.contract.pass) {
      row.response_phase = "rejected";
      row.response_status = 422;
      fixedError(response, 422, "request_contract_mismatch");
      return;
    }
    const turnIndex = this.#acceptedResponseCount;
    this.#acceptedResponseCount += 1;
    if (this.responseBehavior === "hold_until_peer_close") {
      row.response_phase = "held";
      await waitForPeerClose(response);
      row.response_phase = "peer_closed";
      return;
    }
    if (this.responseBehavior === "hold_until_release") {
      row.response_phase = "held";
      await this.#responseReleases[turnIndex].promise;
    }

    const responseOptions = {
      itemId: turnIndex === 0 ? "msg_main_ok" : `msg_main_ok_${turnIndex + 1}`,
      responseId: turnIndex === 0 ? "resp_main_ok" : `resp_main_ok_${turnIndex + 1}`,
    };
    if (this.responsePacing !== null) {
      const completed = await writePacedResponses(
        response,
        row,
        turn.responseText,
        this.responsePacing,
        responseOptions,
      );
      if (completed) this.#successfulResponseCount += 1;
      return;
    }

    this.#successfulResponseCount += 1;
    row.response_phase = "completed";
    row.response_status = 200;
    writeResponse(response, 200, "text/event-stream", responsesSse(
      turn.responseText,
      responseOptions,
    ));
  }

  async #handleAgentInterruptResponse(response, row) {
    if (!row.contract.pass) {
      row.response_phase = "rejected";
      row.response_status = 422;
      fixedError(response, 422, "request_contract_mismatch");
      return;
    }
    const role = row.contract.role;
    if (this.#acceptedRoles.has(role)) {
      row.response_phase = "rejected";
      row.response_status = 409;
      fixedError(response, 409, "script_role_already_consumed");
      return;
    }
    if (role !== "root_initial" && !this.#acceptedRoles.has("root_initial")) {
      row.response_phase = "rejected";
      row.response_status = 409;
      fixedError(response, 409, "script_role_prerequisite_missing");
      return;
    }

    this.#acceptedRoles.add(role);
    this.#acceptedResponseCount += 1;
    if (role === "child_held") {
      row.response_phase = "held";
      await waitForPeerClose(response);
      row.response_phase = "peer_closed";
      return;
    }

    this.#successfulResponseCount += 1;
    row.response_phase = "completed";
    row.response_status = 200;
    const payload = role === "root_initial"
      ? agentInterruptSpawnSse(this.script)
      : responsesSse(this.script.rootResponseText, {
        itemId: "msg_agent_interrupt_root_done",
        responseId: "resp_agent_interrupt_root_done",
      });
    writeResponse(response, 200, "text/event-stream", payload);
  }

  async #handleToolErrorRecoveryResponse(response, row) {
    if (!row.contract.pass) {
      row.response_phase = "rejected";
      row.response_status = 422;
      fixedError(response, 422, "request_contract_mismatch");
      return;
    }
    const role = row.contract.role;
    if (this.#acceptedRoles.has(role)) {
      row.response_phase = "rejected";
      row.response_status = 409;
      fixedError(response, 409, "script_role_already_consumed");
      return;
    }
    if (role !== "tool_error_initial" && !this.#acceptedRoles.has("tool_error_initial")) {
      row.response_phase = "rejected";
      row.response_status = 409;
      fixedError(response, 409, "script_role_prerequisite_missing");
      return;
    }

    this.#acceptedRoles.add(role);
    this.#acceptedResponseCount += 1;
    if (role === "tool_error_initial" && this.responseBehavior === "hold_until_release") {
      row.response_phase = "held";
      await this.#responseReleases[0].promise;
    }
    this.#successfulResponseCount += 1;
    row.response_phase = "completed";
    row.response_status = 200;
    const payload = role === "tool_error_initial"
      ? toolErrorRecoveryReadSse(this.script)
      : responsesSse(this.script.responseText, {
        itemId: "msg_tool_error_recovery_done",
        responseId: "resp_tool_error_recovery_done",
        streamedText: this.script.streamedPrefix,
      });
    writeResponse(response, 200, "text/event-stream", payload);
  }

  async #handleSideChatSessionResponse(response, row) {
    if (!row.contract.pass) {
      row.response_phase = "rejected";
      row.response_status = 422;
      fixedError(response, 422, "request_contract_mismatch");
      return;
    }
    const role = row.contract.role;
    const roles = [
      "side_session_alpha",
      "side_session_beta",
      "side_session_main_after_config",
      "side_session_consult",
    ];
    if (this.#acceptedRoles.has(role)) {
      row.response_phase = "rejected";
      row.response_status = 409;
      fixedError(response, 409, "script_role_already_consumed");
      return;
    }
    if (roles[this.#acceptedResponseCount] !== role) {
      row.response_phase = "rejected";
      row.response_status = 409;
      fixedError(response, 409, "script_role_prerequisite_missing");
      return;
    }
    this.#acceptedRoles.add(role);
    this.#acceptedResponseCount += 1;
    this.#successfulResponseCount += 1;
    row.response_phase = "completed";
    row.response_status = 200;
    const responseText = {
      side_session_alpha: SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_ALPHA_RESPONSE,
      side_session_beta: SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_BETA_RESPONSE,
      side_session_main_after_config: SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_MAIN_AFTER_CONFIG_RESPONSE,
      side_session_consult: SCRIPTED_PROVIDER_SIDE_CHAT_SESSION_SIDE_RESPONSE,
    }[role];
    writeResponse(response, 200, "text/event-stream", responsesSse(responseText, {
      itemId: `msg_${role}`,
      responseId: `resp_${role}`,
    }));
  }

  async #handleSideChatQuoteResponse(response, row) {
    if (!row.contract.pass) {
      row.response_phase = "rejected";
      row.response_status = 422;
      fixedError(response, 422, "request_contract_mismatch");
      return;
    }
    const role = row.contract.role;
    if (this.#acceptedRoles.has(role)) {
      row.response_phase = "rejected";
      row.response_status = 409;
      fixedError(response, 409, "script_role_already_consumed");
      return;
    }
    const prerequisite = {
      side_quote_main_initial: null,
      side_quote_main_continuation: "side_quote_main_initial",
      side_quote_transcript: "side_quote_main_continuation",
      side_quote_artifact_held: "side_quote_transcript",
    }[role];
    if (prerequisite === undefined
      || (prerequisite !== null && !this.#acceptedRoles.has(prerequisite))) {
      row.response_phase = "rejected";
      row.response_status = 409;
      fixedError(response, 409, "script_role_prerequisite_missing");
      return;
    }

    this.#acceptedRoles.add(role);
    this.#acceptedResponseCount += 1;
    if (role === "side_quote_artifact_held") {
      row.response_phase = "held";
      await waitForPeerClose(response);
      row.response_phase = "peer_closed";
      return;
    }

    this.#successfulResponseCount += 1;
    row.response_phase = "completed";
    row.response_status = 200;
    const payload = role === "side_quote_main_initial"
      ? sideChatQuoteWriteSse()
      : role === "side_quote_main_continuation"
        ? responsesSse(SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_MAIN_RESPONSE, {
          itemId: "msg_side_chat_quote_main_done",
          responseId: "resp_side_chat_quote_main_done",
        })
        : responsesSse(SCRIPTED_PROVIDER_SIDE_CHAT_QUOTE_FIRST_RESPONSE, {
          itemId: "msg_side_chat_quote_transcript_done",
          responseId: "resp_side_chat_quote_transcript_done",
        });
    writeResponse(response, 200, "text/event-stream", payload);
  }

  async #handleChatToolContinuationResponse(response, row) {
    if (!row.contract.pass) {
      row.response_phase = "rejected";
      row.response_status = 422;
      fixedError(response, 422, "request_contract_mismatch");
      return;
    }
    const role = row.contract.role;
    if (this.#acceptedRoles.has(role)) {
      row.response_phase = "rejected";
      row.response_status = 409;
      fixedError(response, 409, "script_role_already_consumed");
      return;
    }
    if (role === "chat_continuation" && !this.#acceptedRoles.has("chat_tool_initial")) {
      row.response_phase = "rejected";
      row.response_status = 409;
      fixedError(response, 409, "script_role_prerequisite_missing");
      return;
    }

    this.#acceptedRoles.add(role);
    this.#acceptedResponseCount += 1;
    if (role === "chat_continuation") {
      if (this.#scriptRoleRelease?.role !== role) {
        throw new Error("Chat tool continuation release owner is missing");
      }
      row.response_phase = "held";
      await this.#scriptRoleRelease.promise;
    }
    this.#successfulResponseCount += 1;
    row.response_phase = "completed";
    row.response_status = 200;
    const payload = role === "chat_tool_initial"
      ? chatToolContinuationCallSse(this.modelId)
      : chatToolContinuationFinalSse(this.modelId);
    writeResponse(response, 200, "text/event-stream", payload);
  }

  async #handleResponsesCompactionResponse(response, row) {
    if (!row.contract.pass) {
      row.response_phase = "rejected";
      row.response_status = 422;
      fixedError(response, 422, "request_contract_mismatch");
      return;
    }
    const role = row.contract.role;
    if (this.#acceptedRoles.has(role)) {
      row.response_phase = "rejected";
      row.response_status = 409;
      fixedError(response, 409, "script_role_already_consumed");
      return;
    }
    const readMatch = role.match(/^read_([0-9]+)$/u);
    let prerequisites = [];
    if (readMatch !== null) {
      const callIndex = Number(readMatch[1]);
      prerequisites = callIndex === 1 ? [] : [`read_${callIndex - 1}`];
    } else if (role === "compaction_empty") {
      prerequisites = [`read_${this.script.toolCallCount}`];
    } else if (role === "compaction_valid") {
      prerequisites = ["compaction_empty"];
      const oversized = this.#ledger.find((candidate) => (
        candidate.contract?.role === "compaction_empty" && candidate.contract?.pass === true
      ));
      const first = oversized?.contract?.role_evidence;
      const retry = row.contract.role_evidence;
      const prefixLength = retry?.source_unit_signatures?.length;
      const alignedStrictPrefix = Number.isSafeInteger(prefixLength)
        && prefixLength > 0
        && prefixLength < (first?.source_unit_signatures?.length ?? 0)
        && JSON.stringify(retry.source_unit_signatures)
          === JSON.stringify(first.source_unit_signatures.slice(0, prefixLength));
      row.contract.retry_alignment = {
        pass: alignedStrictPrefix,
        strict_prefix: alignedStrictPrefix,
        first_source_unit_count: first?.source_unit_count ?? null,
        retry_source_unit_count: retry?.source_unit_count ?? null,
        first_input_size_bytes: first?.input_size_bytes ?? null,
        retry_input_size_bytes: retry?.input_size_bytes ?? null,
      };
      if (!alignedStrictPrefix) {
        row.response_phase = "rejected";
        row.response_status = 422;
        fixedError(response, 422, "compaction_retry_not_aligned_prefix");
        return;
      }
    } else if (role === "resumed_final") {
      prerequisites = ["compaction_valid"];
      const accepted = this.#ledger.find((candidate) => (
        candidate.contract?.role === "compaction_valid" && candidate.contract?.pass === true
      ));
      const summarizedReadCount = accepted?.contract?.role_evidence?.source_read_count;
      const resumed = row.contract.role_evidence;
      const replayMatches = Number.isSafeInteger(summarizedReadCount)
        && resumed?.first_remaining_read_index === summarizedReadCount + 1
        && resumed?.remaining_read_count === this.script.toolCallCount - summarizedReadCount;
      row.contract.compaction_replay = {
        pass: replayMatches,
        summarized_read_count: summarizedReadCount ?? null,
        first_remaining_read_index: resumed?.first_remaining_read_index ?? null,
        remaining_read_count: resumed?.remaining_read_count ?? null,
      };
      if (!replayMatches) {
        row.response_phase = "rejected";
        row.response_status = 422;
        fixedError(response, 422, "compaction_replay_mismatch");
        return;
      }
    }
    if (prerequisites.some((required) => !this.#acceptedRoles.has(required))) {
      row.response_phase = "rejected";
      row.response_status = 409;
      fixedError(response, 409, "script_role_prerequisite_missing");
      return;
    }

    this.#acceptedRoles.add(role);
    this.#acceptedResponseCount += 1;
    if (role === "compaction_empty" && this.responseBehavior === "hold_until_release") {
      if (this.#scriptRoleRelease?.role !== role) {
        throw new Error("Responses compaction reasoning release owner is missing");
      }
      const completed = await writeHeldResponsesCompactionReasoning(
        response,
        row,
        this.#scriptRoleRelease.promise,
      );
      if (completed) this.#successfulResponseCount += 1;
      return;
    }
    this.#successfulResponseCount += 1;
    row.response_phase = "completed";
    row.response_status = 200;
    let payload;
    if (readMatch !== null) {
      payload = responsesCompactionReadSse(this.script, Number(readMatch[1]));
    } else if (role === "compaction_empty") {
      payload = responsesCompactionReasoningOnlySse();
    } else if (role === "compaction_valid") {
      payload = responsesSse(this.script.checkpointText, {
        itemId: "msg_responses_compaction_checkpoint",
        responseId: "resp_responses_compaction_checkpoint",
      });
    } else {
      payload = responsesSse(this.script.responseText, {
        itemId: "msg_responses_compaction_done",
        responseId: "resp_responses_compaction_done",
      });
    }
    writeResponse(response, 200, "text/event-stream", payload);
  }

  async #handlePermissionTempEscalationResponse(response, row) {
    if (!row.contract.pass) {
      row.response_phase = "rejected";
      row.response_status = 422;
      fixedError(response, 422, "request_contract_mismatch");
      return;
    }
    const role = row.contract.role;
    if (this.#acceptedRoles.has(role)) {
      row.response_phase = "rejected";
      row.response_status = 409;
      fixedError(response, 409, "script_role_already_consumed");
      return;
    }
    const prerequisites = {
      temp_initial: [],
      temp_escalation: ["temp_initial"],
      temp_guardian: ["temp_initial", "temp_escalation"],
      temp_continuation: ["temp_initial", "temp_escalation", "temp_guardian"],
    };
    if (!Array.isArray(prerequisites[role])
      || prerequisites[role].some((required) => !this.#acceptedRoles.has(required))) {
      row.response_phase = "rejected";
      row.response_status = 409;
      fixedError(response, 409, "script_role_prerequisite_missing");
      return;
    }

    this.#acceptedRoles.add(role);
    this.#acceptedResponseCount += 1;
    if (role === "temp_escalation" && this.responseBehavior === "hold_until_release") {
      if (this.#scriptRoleRelease?.role !== role) {
        throw new Error("permission TEMP escalation release owner is missing");
      }
      row.response_phase = "held";
      await this.#scriptRoleRelease.promise;
    }
    const guardianDelay = role === "temp_guardian"
      ? await waitForGuardianResponseDelay(response, row, this.script.guardianDelayMs)
      : null;
    if (role === "temp_guardian" && guardianDelay === null) return;
    this.#successfulResponseCount += 1;
    row.response_phase = "completed";
    row.response_status = 200;
    let payload;
    if (role === "temp_initial") {
      payload = permissionTempEscalationShellSse(this.script, false);
    } else if (role === "temp_escalation") {
      payload = permissionTempEscalationShellSse(this.script, true);
    } else if (role === "temp_guardian") {
      payload = responsesSse(JSON.stringify(PERMISSION_RESTART_GUARDIAN_ALLOW), {
        itemId: "msg_permission_temp_escalation_allow",
        responseId: "resp_permission_temp_escalation_allow",
      });
    } else {
      payload = responsesSse(this.script.responseText, {
        itemId: "msg_permission_temp_escalation_done",
        responseId: "resp_permission_temp_escalation_done",
      });
    }
    if (guardianDelay !== null) {
      guardianDelay.observation.headers_sent_elapsed_ms = elapsedMonotonicMs(
        guardianDelay.startedAt,
      );
    }
    writeResponse(response, 200, "text/event-stream", payload);
  }

  async #handlePermissionRestartGuardianResponse(response, row) {
    if (!row.contract.pass) {
      row.response_phase = "rejected";
      row.response_status = 422;
      fixedError(response, 422, "request_contract_mismatch");
      return;
    }
    const role = row.contract.role;
    if (this.#acceptedRoles.has(role)) {
      row.response_phase = "rejected";
      row.response_status = 409;
      fixedError(response, 409, "script_role_already_consumed");
      return;
    }
    const prerequisites = {
      guardian_seed: [],
      guardian_tool_initial: ["guardian_seed"],
      guardian_review: ["guardian_seed", "guardian_tool_initial"],
      guardian_continuation: ["guardian_seed", "guardian_tool_initial", "guardian_review"],
    };
    if (!Array.isArray(prerequisites[role])
      || prerequisites[role].some((required) => !this.#acceptedRoles.has(required))) {
      row.response_phase = "rejected";
      row.response_status = 409;
      fixedError(response, 409, "script_role_prerequisite_missing");
      return;
    }

    this.#acceptedRoles.add(role);
    this.#acceptedResponseCount += 1;
    if (role === "guardian_tool_initial" && this.responseBehavior === "hold_until_release") {
      if (this.#scriptRoleRelease?.role !== role) {
        throw new Error("permission restart Guardian release owner is missing");
      }
      row.response_phase = "held";
      await this.#scriptRoleRelease.promise;
    }
    this.#successfulResponseCount += 1;
    row.response_phase = "completed";
    row.response_status = 200;
    let payload;
    if (this.script.apiMode === "chat_completions" && role === "guardian_seed") {
      payload = permissionRestartGuardianChatTextSse(
        this.modelId,
        "chatcmpl_permission_restart_guardian_seed",
        this.script.seedResponseText,
        { promptTokens: 12, completionTokens: 4 },
      );
    } else if (this.script.apiMode === "chat_completions" && role === "guardian_tool_initial") {
      payload = permissionRestartGuardianChatShellSse(this.modelId, this.script);
    } else if (this.script.apiMode === "chat_completions" && role === "guardian_review") {
      payload = permissionRestartGuardianChatTextSse(
        this.modelId,
        "chatcmpl_permission_restart_guardian_allow",
        JSON.stringify(PERMISSION_RESTART_GUARDIAN_ALLOW),
        { promptTokens: 40, completionTokens: 8 },
      );
    } else if (this.script.apiMode === "chat_completions") {
      payload = permissionRestartGuardianChatTextSse(
        this.modelId,
        "chatcmpl_permission_restart_guardian_done",
        this.script.responseText,
        { promptTokens: 32, completionTokens: 4 },
      );
    } else if (role === "guardian_seed") {
      payload = responsesSse(this.script.seedResponseText, {
        itemId: "msg_permission_restart_guardian_seed",
        responseId: "resp_permission_restart_guardian_seed",
      });
    } else if (role === "guardian_tool_initial") {
      payload = permissionRestartGuardianShellSse(this.script);
    } else if (role === "guardian_review") {
      payload = responsesSse(JSON.stringify(PERMISSION_RESTART_GUARDIAN_ALLOW), {
        itemId: "msg_permission_restart_guardian_allow",
        responseId: "resp_permission_restart_guardian_allow",
      });
    } else {
      payload = responsesSse(this.script.responseText, {
        itemId: "msg_permission_restart_guardian_done",
        responseId: "resp_permission_restart_guardian_done",
      });
    }
    writeResponse(response, 200, "text/event-stream", payload);
  }
}

export async function startScriptedProvider(options = {}) {
  return new ScriptedProvider(options).start();
}

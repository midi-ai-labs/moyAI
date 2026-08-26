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
export const SCRIPTED_PROVIDER_RESPONSE_BEHAVIORS = Object.freeze([
  "complete",
  "hold_until_release",
  "hold_until_peer_close",
]);

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

function knownMissingRelativePath(value, name) {
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
  return Object.freeze({
    kind: value.kind,
    seedPrompt: nonEmptyString(value.seedPrompt, "script.seedPrompt"),
    seedResponseText: nonEmptyString(value.seedResponseText, "script.seedResponseText"),
    taskPrompt: nonEmptyString(value.taskPrompt, "script.taskPrompt"),
    command: nonEmptyString(value.command, "script.command"),
    justification: nonEmptyString(value.justification, "script.justification"),
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
  throw new TypeError("scripted provider mode must use a known kind and its exact schema");
}

function scriptedResponseMaximum(script) {
  if (script?.kind === SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_KIND) {
    return SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_MAX_RESPONSES;
  }
  if (script?.kind === SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_KIND) {
    return SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_MAX_RESPONSES;
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
  seedPrompt,
  seedResponseText,
  taskPrompt,
  command,
  justification,
  responseText,
} = {}) {
  return permissionRestartGuardianScript({
    kind: SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_KIND,
    seedPrompt,
    seedResponseText,
    taskPrompt,
    command,
    justification,
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
  "max_output_tokens",
  "model",
  "store",
  "stream",
]);
const FORBIDDEN_RESPONSES_KEYS = Object.freeze([
  "extra_body_json",
  "frequency_penalty",
  "max_tokens",
  "messages",
  "parallel_tool_calls",
  "presence_penalty",
  "previous_response_id",
  "reasoning",
  "reasoning_effort",
  "reasoning_summary",
  "seed",
  "stop",
  "temperature",
  "tool_choice",
  "tools",
  "top_k",
  "top_p",
]);
const TOOL_RESPONSES_KEYS = Object.freeze([
  "input",
  "instructions",
  "max_output_tokens",
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
const PERMISSION_RESTART_GUARDIAN_SHELL_CALL_ID = "call_permission_restart_guardian_shell";
const PERMISSION_RESTART_GUARDIAN_SHELL_ITEM_ID = "fc_permission_restart_guardian_shell";
const PERMISSION_RESTART_GUARDIAN_ALLOW = Object.freeze({
  decision: "allow",
  rationale: "bounded deterministic fixture command",
});
const GUARDIAN_RESPONSES_KEYS = Object.freeze([
  "input",
  "instructions",
  "max_output_tokens",
  "model",
  "reasoning",
  "store",
  "stream",
]);

function requestContract(body, modelId, expectedPrompt, expectedMaxOutputTokens) {
  const model = typeof body?.model === "string" ? body.model : null;
  const inputText = exactUserInput(body);
  const instructions = typeof body?.instructions === "string" ? body.instructions : null;
  const topLevelKeys = body !== null && typeof body === "object" && !Array.isArray(body)
    ? Object.keys(body).sort()
    : [];
  const forbiddenFieldsPresent = FORBIDDEN_RESPONSES_KEYS.filter((key) => Object.hasOwn(body ?? {}, key));
  const contract = {
    model_sha256: model === null ? null : sha256(Buffer.from(model, "utf8")),
    input_text_sha256: inputText === null ? null : sha256(Buffer.from(inputText, "utf8")),
    instructions_sha256: instructions === null ? null : sha256(Buffer.from(instructions, "utf8")),
    top_level_keys: topLevelKeys,
    forbidden_fields_present: forbiddenFieldsPresent,
    model_matches: model === modelId,
    input_matches: inputText === expectedPrompt,
    instructions_non_empty: instructions !== null && instructions.trim().length > 0,
    top_level_keys_match: JSON.stringify(topLevelKeys) === JSON.stringify(EXPECTED_RESPONSES_KEYS),
    max_output_tokens_matches: body?.max_output_tokens === expectedMaxOutputTokens,
    stream_true: body?.stream === true,
    store_false: body?.store === false,
  };
  return {
    ...contract,
    pass: contract.model_matches
      && contract.input_matches
      && contract.instructions_non_empty
      && contract.top_level_keys_match
      && contract.max_output_tokens_matches
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
  expectedMaxOutputTokens,
) {
  const model = typeof body?.model === "string" ? body.model : null;
  const instructions = typeof body?.instructions === "string" ? body.instructions : null;
  const topLevelKeys = body !== null && typeof body === "object" && !Array.isArray(body)
    ? Object.keys(body).sort()
    : [];
  const forbiddenFieldsPresent = FORBIDDEN_RESPONSES_KEYS.filter((key) => Object.hasOwn(body ?? {}, key));
  const conversation = orderedConversationInputContract(body?.input, turns, currentTurnIndex);
  const contract = {
    ordered_conversation: conversation,
    model_sha256: model === null ? null : sha256(Buffer.from(model, "utf8")),
    instructions_sha256: instructions === null ? null : sha256(Buffer.from(instructions, "utf8")),
    top_level_keys: topLevelKeys,
    forbidden_fields_present: forbiddenFieldsPresent,
    model_matches: model === modelId,
    instructions_non_empty: instructions !== null && instructions.trim().length > 0,
    top_level_keys_match: JSON.stringify(topLevelKeys) === JSON.stringify(EXPECTED_RESPONSES_KEYS),
    max_output_tokens_matches: body?.max_output_tokens === expectedMaxOutputTokens,
    stream_true: body?.stream === true,
    store_false: body?.store === false,
  };
  return {
    ...contract,
    pass: conversation.matches
      && contract.model_matches
      && contract.instructions_non_empty
      && contract.top_level_keys_match
      && contract.max_output_tokens_matches
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

function agentInterruptRequestContract(body, modelId, expectedPrompt, expectedMaxOutputTokens, script) {
  const model = typeof body?.model === "string" ? body.model : null;
  const instructions = typeof body?.instructions === "string" ? body.instructions : null;
  const topLevelKeys = body !== null && typeof body === "object" && !Array.isArray(body)
    ? Object.keys(body).sort()
    : [];
  const tools = toolsContract(body?.tools);
  const classified = agentInterruptRole(body, expectedPrompt, script);
  const contract = {
    script_kind: script.kind,
    role: classified.role,
    role_evidence: classified.evidence,
    model_sha256: model === null ? null : sha256(Buffer.from(model, "utf8")),
    instructions_sha256: instructions === null ? null : sha256(Buffer.from(instructions, "utf8")),
    top_level_keys: topLevelKeys,
    model_matches: model === modelId,
    instructions_non_empty: instructions !== null && instructions.trim().length > 0,
    top_level_keys_match: JSON.stringify(topLevelKeys) === JSON.stringify(TOOL_RESPONSES_KEYS),
    max_output_tokens_matches: body?.max_output_tokens === expectedMaxOutputTokens,
    stream_true: body?.stream === true,
    store_false: body?.store === false,
    tool_choice_auto: body?.tool_choice === "auto",
    parallel_tool_calls_false: body?.parallel_tool_calls === false,
    tools,
  };
  return {
    ...contract,
    pass: contract.role !== null
      && contract.model_matches
      && contract.instructions_non_empty
      && contract.top_level_keys_match
      && contract.max_output_tokens_matches
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

function toolErrorRecoveryRequestContract(body, modelId, expectedPrompt, expectedMaxOutputTokens, script) {
  const model = typeof body?.model === "string" ? body.model : null;
  const instructions = typeof body?.instructions === "string" ? body.instructions : null;
  const topLevelKeys = body !== null && typeof body === "object" && !Array.isArray(body)
    ? Object.keys(body).sort()
    : [];
  const tools = readToolsContract(body?.tools);
  const classified = toolErrorRecoveryRole(body, expectedPrompt, script);
  const contract = {
    script_kind: script.kind,
    role: classified.role,
    role_evidence: classified.evidence,
    model_sha256: model === null ? null : sha256(Buffer.from(model, "utf8")),
    instructions_sha256: instructions === null ? null : sha256(Buffer.from(instructions, "utf8")),
    top_level_keys: topLevelKeys,
    model_matches: model === modelId,
    instructions_non_empty: instructions !== null && instructions.trim().length > 0,
    top_level_keys_match: JSON.stringify(topLevelKeys) === JSON.stringify(TOOL_RESPONSES_KEYS),
    max_output_tokens_matches: body?.max_output_tokens === expectedMaxOutputTokens,
    stream_true: body?.stream === true,
    store_false: body?.store === false,
    tool_choice_auto: body?.tool_choice === "auto",
    parallel_tool_calls_false: body?.parallel_tool_calls === false,
    tools,
  };
  return {
    ...contract,
    pass: contract.role !== null
      && contract.model_matches
      && contract.instructions_non_empty
      && contract.top_level_keys_match
      && contract.max_output_tokens_matches
      && contract.stream_true
      && contract.store_false
      && contract.tool_choice_auto
      && contract.parallel_tool_calls_false
      && contract.tools.pass,
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
  expectedMaxOutputTokens,
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
  const common = {
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
      max_output_tokens_matches: body?.max_output_tokens === Math.min(expectedMaxOutputTokens, 512),
      reasoning_none: exactKeys(body?.reasoning, ["effort"])
        && body.reasoning.effort === "none",
      tools_absent: !Object.hasOwn(body ?? {}, "tools")
        && !Object.hasOwn(body ?? {}, "tool_choice")
        && !Object.hasOwn(body ?? {}, "parallel_tool_calls"),
    };
    return {
      ...guardian,
      pass: guardian.role === "guardian_review"
        && guardian.model_matches
        && guardian.instructions_non_empty
        && guardian.guardian_instructions_match
        && guardian.top_level_keys_match
        && guardian.max_output_tokens_matches
        && guardian.reasoning_none
        && guardian.tools_absent
        && guardian.stream_true
        && guardian.store_false,
    };
  }
  const task = {
    ...common,
    max_output_tokens_matches: body?.max_output_tokens === expectedMaxOutputTokens,
    tool_choice_auto: body?.tool_choice === "auto",
    parallel_tool_calls_false: body?.parallel_tool_calls === false,
    tools,
  };
  return {
    ...task,
    pass: task.role !== null
      && task.model_matches
      && task.instructions_non_empty
      && task.top_level_keys_match
      && task.max_output_tokens_matches
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

function responsesSse(responseText, {
  itemId = "msg_main_ok",
  responseId = "resp_main_ok",
  streamedText = responseText,
} = {}) {
  const item = {
    type: "message",
    id: itemId,
    role: "assistant",
    content: [{ type: "output_text", text: responseText }],
  };
  const events = [
    {
      type: "response.output_text.delta",
      item_id: item.id,
      output_index: 0,
      delta: streamedText,
    },
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
  return events.map((event) => `data: ${JSON.stringify(event)}\n\n`).join("");
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
    expectedMaxOutputTokens = SCRIPTED_PROVIDER_MAX_OUTPUT_TOKENS,
    responseBehavior: configuredResponseBehavior = "complete",
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
    this.expectedMaxOutputTokens = positiveInteger(expectedMaxOutputTokens, "expectedMaxOutputTokens");
    this.responseBehavior = responseBehavior(configuredResponseBehavior);
    this.#doclingReadinessStatus = optionalHttpStatus(doclingReadinessStatus, "doclingReadinessStatus");
    this.#doclingReadinessRelease = new Promise((resolve) => { this.#releaseDoclingReadiness = resolve; });
    this.script = providerScript(script);
    const releaseHeldToolErrorInitial = this.script?.kind === SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_KIND
      && this.responseBehavior === "hold_until_release";
    const releaseHeldGuardianToolInitial = this.script?.kind
      === SCRIPTED_PROVIDER_PERMISSION_RESTART_GUARDIAN_KIND
      && this.responseBehavior === "hold_until_release";
    if (this.script !== null
      && this.responseBehavior !== "complete"
      && !releaseHeldToolErrorInitial
      && !releaseHeldGuardianToolInitial) {
      throw new TypeError("scripted provider mode owns its response lifecycle");
    }
    if (this.script !== null && turns !== null) {
      throw new TypeError("scripted provider mode cannot use ordinary turns");
    }
    if (this.responseBehavior === "hold_until_release") {
      if (releaseHeldGuardianToolInitial) {
        let release;
        const promise = new Promise((resolve) => { release = resolve; });
        this.#scriptRoleRelease = {
          role: "guardian_tool_initial",
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
    const rows = this.#ledger.filter((row) => row.route === "responses"
      && row.contract?.pass === true
      && row.contract?.role === role);
    if (rows.length !== 1 || rows[0].response_phase !== "held" || rows[0].response_status !== null) {
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
    };
    this.#ledger.push(row);

    if (route === "unknown") {
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
          this.expectedMaxOutputTokens,
          this.script,
        );
      } else if (this.script.kind === SCRIPTED_PROVIDER_TOOL_ERROR_RECOVERY_KIND) {
        row.contract = toolErrorRecoveryRequestContract(
          decoded.value,
          this.modelId,
          this.expectedPrompt,
          this.expectedMaxOutputTokens,
          this.script,
        );
      } else {
        row.contract = permissionRestartGuardianRequestContract(
          decoded.value,
          this.modelId,
          this.expectedMaxOutputTokens,
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
        this.expectedMaxOutputTokens,
      )
      : requestContract(decoded.value, this.modelId, turn.prompt, this.expectedMaxOutputTokens);
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

    this.#successfulResponseCount += 1;
    row.response_phase = "completed";
    row.response_status = 200;
    writeResponse(response, 200, "text/event-stream", responsesSse(turn.responseText, {
      itemId: turnIndex === 0 ? "msg_main_ok" : `msg_main_ok_${turnIndex + 1}`,
      responseId: turnIndex === 0 ? "resp_main_ok" : `resp_main_ok_${turnIndex + 1}`,
    }));
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
    if (role === "guardian_seed") {
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

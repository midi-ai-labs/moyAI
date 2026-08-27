import { isDeepStrictEqual } from "node:util";

import { canonicalU64, canonicalUlid } from "../core/canonical_identity.mjs";
import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import {
  DesktopCommandProbe,
  assertExactDesktopCommandSequence,
} from "../drivers/desktop_command_probe.mjs";
import {
  SCRIPTED_PROVIDER_MODEL_ID,
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_CHECKPOINT,
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_KIND,
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES,
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_PROMPT,
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RAW_REASONING_SENTINEL,
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RESPONSE,
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_SENTINEL,
  SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT,
  createResponsesCompactionProviderScript,
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
  observeProviderTurnSurface,
  quiesceProviderResource,
} from "./provider_restart.mjs";
import { captureScenarioScreenshot, selectedNavigationIdentity } from "./observations.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";

const OWNER = "scenario:provider.responses-compaction-retry";
export const PROVIDER_RESPONSES_COMPACTION_CONTEXT_WINDOW = 32_768;
export const PROVIDER_RESPONSES_COMPACTION_INPUT_BYTE_THRESHOLD = 32_000;
export const PROVIDER_RESPONSES_COMPACTION_SENTINEL_LINE_BYTES = 12_000;
const EXPECTED_ROLES = Object.freeze([
  ...Array.from(
    { length: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT },
    (_, index) => `read_${index + 1}`,
  ),
  "compaction_empty",
  "compaction_valid",
  "resumed_final",
]);
const HELD_ROLE_COUNT = SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT + 1;
const HELD_ROLES = Object.freeze(EXPECTED_ROLES.slice(0, HELD_ROLE_COUNT));
const REASONING_STREAM_EVENT_TYPES = Object.freeze([
  "response.output_item.added",
  "response.reasoning_text.delta",
  "response.reasoning_text.done",
  "response.output_item.done",
  "response.completed",
]);
const REASONING_HOLD_STABILITY_MS = 250;
const TOOL_PROGRESS = `ツール: ${SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT}件開始 / ${SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT}件完了 / 0件拒否 / 0件キャンセル / 0件失敗`;
const CONTROL_TOKENS = Object.freeze(["<|im_start|>", "<|im_end|>"]);

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

function sha256Identity(value) {
  return typeof value === "string" && /^[a-f0-9]{64}$/u.test(value);
}

export function providerResponsesCompactionFixtureConfig(baseUrl) {
  return `[model]
base_url = ${JSON.stringify(baseUrl)}
model = ${JSON.stringify(SCRIPTED_PROVIDER_MODEL_ID)}
provider_profile = "openai_responses"
connect_timeout_ms = 1000
request_timeout_ms = 30000
max_retries = 0
context_window = ${PROVIDER_RESPONSES_COMPACTION_CONTEXT_WINDOW}
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

export function providerResponsesCompactionSentinelText() {
  return `${Array.from(
    { length: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT },
    (_, index) => {
      const prefix = `bounded-read-${index + 1}:`;
      return prefix.padEnd(PROVIDER_RESPONSES_COMPACTION_SENTINEL_LINE_BYTES, String(index + 1));
    },
  ).join("\n")}\n`;
}

function commonAcceptedContract(row, role, { responsePhase = "completed" } = {}) {
  const contract = row?.contract;
  return row?.route === "responses"
    && row.method === "POST"
    && row.pathname === "/v1/responses"
    && row.query_present === false
    && row.response_phase === responsePhase
    && row.response_status === 200
    && contract?.pass === true
    && contract.role === role
    && contract.model_matches === true
    && contract.instructions_non_empty === true
    && contract.top_level_keys_match === true
    && contract.stream_true === true
    && contract.store_false === true
    && contract.max_output_tokens_absent === true
    && contract.client_generation_fields_absent === true
    && isDeepStrictEqual(contract.client_generation_fields_present, [])
    && Number.isSafeInteger(contract.role_evidence?.input_size_bytes)
    && contract.role_evidence.input_size_bytes > 0
    && contract.role_evidence.input_byte_threshold
      === PROVIDER_RESPONSES_COMPACTION_INPUT_BYTE_THRESHOLD;
}

function acceptedToolContract(row, role) {
  const contract = row?.contract;
  return commonAcceptedContract(row, role)
    && contract.tool_choice_auto === true
    && contract.parallel_tool_calls_false === true
    && contract.compaction_tools_absent === null
    && contract.tools?.pass === true
    && contract.tools.unique_tool_names === true
    && contract.tools.read_present === true
    && contract.tools.read_schema_matches === true;
}

function acceptedReadRow(row, index) {
  const evidence = row?.contract?.role_evidence;
  const expectedTypes = [
    "message",
    ...Array.from({ length: index }, () => ["function_call", "function_call_output"]).flat(),
  ];
  return acceptedToolContract(row, `read_${index + 1}`)
    && evidence?.matches === true
    && evidence.first_prompt_matches === true
    && evidence.terminal_prompt_matches === true
    && evidence.terminal_prompt_sha256 === null
    && evidence.source_read_count === index
    && evidence.source_unit_count === index + 1
    && evidence.input_count === 1 + index * 2
    && isDeepStrictEqual(evidence.input_item_types, expectedTypes)
    && Array.isArray(evidence.source_unit_signatures)
    && evidence.source_unit_signatures.length === index + 1
    && evidence.source_unit_signatures.every(sha256Identity)
    && Array.isArray(evidence.read_output_size_bytes)
    && evidence.read_output_size_bytes.length === index
    && evidence.read_output_size_bytes.every((size) => (
      Number.isSafeInteger(size) && size >= PROVIDER_RESPONSES_COMPACTION_SENTINEL_LINE_BYTES
    ))
    && Array.isArray(evidence.read_output_sha256)
    && evidence.read_output_sha256.length === index
    && evidence.read_output_sha256.every(sha256Identity);
}

function acceptedCompactionRows(first, retry) {
  const firstEvidence = first?.contract?.role_evidence;
  const retryEvidence = retry?.contract?.role_evidence;
  const alignment = retry?.contract?.retry_alignment;
  return commonAcceptedContract(first, "compaction_empty")
    && commonAcceptedContract(retry, "compaction_valid")
    && first.contract.compaction_tools_absent === true
    && retry.contract.compaction_tools_absent === true
    && first.contract.tools === null
    && retry.contract.tools === null
    && firstEvidence?.matches === true
    && retryEvidence?.matches === true
    && firstEvidence.terminal_prompt_matches === true
    && retryEvidence.terminal_prompt_matches === true
    && sha256Identity(firstEvidence.terminal_prompt_sha256)
    && retryEvidence.terminal_prompt_sha256 === firstEvidence.terminal_prompt_sha256
    && firstEvidence.input_exceeds_threshold === true
    && retryEvidence.input_exceeds_threshold === false
    && firstEvidence.input_size_bytes > PROVIDER_RESPONSES_COMPACTION_INPUT_BYTE_THRESHOLD
    && retryEvidence.input_size_bytes <= PROVIDER_RESPONSES_COMPACTION_INPUT_BYTE_THRESHOLD
    && firstEvidence.bounded_prefix_candidate === true
    && retryEvidence.bounded_prefix_candidate === true
    && Number.isSafeInteger(firstEvidence.source_read_count)
    && Number.isSafeInteger(retryEvidence.source_read_count)
    && firstEvidence.source_read_count < SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT
    && retryEvidence.source_read_count < firstEvidence.source_read_count
    && exactReasoningStream(first, { held: false })
    && alignment?.pass === true
    && alignment.strict_prefix === true
    && alignment.first_source_unit_count === firstEvidence.source_unit_count
    && alignment.retry_source_unit_count === retryEvidence.source_unit_count
    && alignment.first_input_size_bytes === firstEvidence.input_size_bytes
    && alignment.retry_input_size_bytes === retryEvidence.input_size_bytes;
}

function acceptedFinalRow(row, retry) {
  const evidence = row?.contract?.role_evidence;
  const replay = row?.contract?.compaction_replay;
  const summarizedReadCount = retry?.contract?.role_evidence?.source_read_count;
  return acceptedToolContract(row, "resumed_final")
    && evidence?.matches === true
    && evidence.first_prompt_matches === true
    && evidence.checkpoint_matches === true
    && sha256Identity(evidence.checkpoint_sha256)
    && replay?.pass === true
    && replay.summarized_read_count === summarizedReadCount
    && replay.first_remaining_read_index === summarizedReadCount + 1
    && replay.remaining_read_count
      === SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT - summarizedReadCount
    && evidence.first_remaining_read_index === replay.first_remaining_read_index
    && evidence.remaining_read_count === replay.remaining_read_count;
}

function readOutputIdentities(evidence, expectedCount) {
  const sizes = evidence?.read_output_size_bytes;
  const hashes = evidence?.read_output_sha256;
  if (!Number.isSafeInteger(expectedCount)
    || expectedCount < 0
    || !Array.isArray(sizes)
    || !Array.isArray(hashes)
    || sizes.length !== expectedCount
    || hashes.length !== expectedCount) return null;
  const identities = sizes.map((sizeBytes, index) => ({
    size_bytes: sizeBytes,
    sha256: hashes[index],
  }));
  return identities.every((identity) => (
    Number.isSafeInteger(identity.size_bytes)
      && identity.size_bytes >= PROVIDER_RESPONSES_COMPACTION_SENTINEL_LINE_BYTES
      && sha256Identity(identity.sha256)
  )) ? identities : null;
}

function exactReasoningStream(row, { held }) {
  const stream = row?.response_stream;
  const expectedEvents = held
    ? REASONING_STREAM_EVENT_TYPES.slice(0, 2)
    : REASONING_STREAM_EVENT_TYPES;
  return stream?.schema_version === "desktop-e2e.scripted-provider-held-stream.v1"
    && stream.hold_after_event_type === "response.reasoning_text.delta"
    && Array.isArray(stream.events)
    && isDeepStrictEqual(stream.events.map((event) => event?.event_type), expectedEvents)
    && stream.events.every((event, index) => (
      event?.sequence === index + 1
        && Number.isSafeInteger(event?.elapsed_ms)
        && event.elapsed_ms >= 0
        && Number.isSafeInteger(event?.size_bytes)
        && event.size_bytes > 0
    ))
    && stream.release_observed === !held
    && (held
      ? stream.release_elapsed_ms === null
      : Number.isSafeInteger(stream.release_elapsed_ms) && stream.release_elapsed_ms >= 0)
    && stream.terminal_sent === !held
    && stream.response_finished === !held
    && stream.peer_close_observed === false;
}

function exactHeldToolOutputIdentity(readRows, first) {
  const normalPrefixes = readRows.map((row, index) => (
    readOutputIdentities(row?.contract?.role_evidence, index)
  ));
  if (normalPrefixes.some((prefix) => prefix === null)) return false;
  const normalSeven = normalPrefixes.at(-1);
  const firstEvidence = first?.contract?.role_evidence;
  const firstPrefix = readOutputIdentities(firstEvidence, firstEvidence?.source_read_count);
  return Array.isArray(normalSeven)
    && firstPrefix !== null
    && new Set(normalSeven.map((identity) => identity.sha256)).size === normalSeven.length
    && normalPrefixes.every((prefix, index) => (
      isDeepStrictEqual(prefix, normalSeven.slice(0, index))
    ))
    && isDeepStrictEqual(firstPrefix, normalSeven.slice(0, firstEvidence.source_read_count));
}

function exactToolOutputIdentity(readRows, first, retry, final) {
  const normalPrefixes = readRows.map((row, index) => (
    readOutputIdentities(row?.contract?.role_evidence, index)
  ));
  if (normalPrefixes.some((prefix) => prefix === null)) return false;
  const firstEvidence = first?.contract?.role_evidence;
  const retryEvidence = retry?.contract?.role_evidence;
  const finalEvidence = final?.contract?.role_evidence;
  const firstPrefix = readOutputIdentities(firstEvidence, firstEvidence?.source_read_count);
  const retryPrefix = readOutputIdentities(retryEvidence, retryEvidence?.source_read_count);
  const finalSuffix = readOutputIdentities(finalEvidence, finalEvidence?.remaining_read_count);
  const normalSeven = normalPrefixes.at(-1);
  const lastRead = finalSuffix?.at(-1);
  if (firstPrefix === null
    || retryPrefix === null
    || finalSuffix === null
    || !Array.isArray(normalSeven)
    || lastRead === undefined) return false;
  const complete = [...normalSeven, lastRead];
  const finalStart = finalEvidence?.first_remaining_read_index;
  return complete.length === SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT
    && new Set(complete.map((identity) => identity.sha256)).size
      === SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT
    && normalPrefixes.every((prefix, index) => (
      isDeepStrictEqual(prefix, complete.slice(0, index))
    ))
    && isDeepStrictEqual(firstPrefix, complete.slice(0, firstEvidence.source_read_count))
    && isDeepStrictEqual(retryPrefix, complete.slice(0, retryEvidence.source_read_count))
    && Number.isSafeInteger(finalStart)
    && isDeepStrictEqual(finalSuffix, complete.slice(finalStart - 1));
}

function stableInstructionIdentity(ledger) {
  const identities = ledger.map((row) => row?.contract?.instructions_sha256);
  return identities.every(sha256Identity) && new Set(identities).size === 1;
}

function exactHeldCompactionLedger(ledger) {
  if (!Array.isArray(ledger)
    || ledger.length !== HELD_ROLE_COUNT
    || !isDeepStrictEqual(ledger.map((row) => row?.contract?.role), HELD_ROLES)
    || !stableInstructionIdentity(ledger)) return false;
  const readRows = ledger.slice(0, SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT);
  const first = ledger.at(-1);
  const evidence = first?.contract?.role_evidence;
  return readRows.every((row, index) => acceptedReadRow(row, index))
    && commonAcceptedContract(first, "compaction_empty", { responsePhase: "held" })
    && first.contract.compaction_tools_absent === true
    && first.contract.tools === null
    && evidence?.matches === true
    && evidence.terminal_prompt_matches === true
    && sha256Identity(evidence.terminal_prompt_sha256)
    && evidence.input_exceeds_threshold === true
    && evidence.input_size_bytes > PROVIDER_RESPONSES_COMPACTION_INPUT_BYTE_THRESHOLD
    && evidence.bounded_prefix_candidate === true
    && Number.isSafeInteger(evidence.source_read_count)
    && evidence.source_read_count < SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT
    && exactHeldToolOutputIdentity(readRows, first)
    && exactReasoningStream(first, { held: true });
}

export function exactResponsesCompactionLedger(ledger) {
  if (!Array.isArray(ledger)
    || ledger.length !== SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES
    || !isDeepStrictEqual(ledger.map((row) => row?.contract?.role), EXPECTED_ROLES)
    || !stableInstructionIdentity(ledger)) return false;
  const readRows = ledger.slice(0, SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT);
  const first = ledger[SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT];
  const retry = ledger[SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT + 1];
  const final = ledger[SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT + 2];
  return readRows.every((row, index) => acceptedReadRow(row, index))
    && acceptedCompactionRows(first, retry)
    && acceptedFinalRow(final, retry)
    && exactToolOutputIdentity(readRows, first, retry, final);
}

function impossibleLedgerPrefix(ledger) {
  if (!Array.isArray(ledger)
    || ledger.length > SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES) return true;
  return ledger.some((row, index) => row?.route !== "responses"
    || row?.method !== "POST"
    || row?.pathname !== "/v1/responses"
    || row?.query_present !== false
    || row?.contract?.pass === false
    || (typeof row?.contract?.role === "string" && row.contract.role !== EXPECTED_ROLES[index])
    || ["rejected", "peer_closed"].includes(row?.response_phase)
    || (row?.response_status !== null && row.response_status !== 200));
}

function rowsOfKind(projection, kind) {
  const rows = Array.isArray(projection?.transcript_rows) ? projection.transcript_rows : [];
  return rows.filter((row) => row?.row_kind === kind);
}

function exactIdleTurnOwner(projection) {
  const expected = projection?.run_target?.expectedState;
  return expected?.kind === "idle"
    && canonicalUlid(projection?.run_target?.sessionId)
    && projection.run_target.sessionId === projection?.draft_target?.sessionId
    && canonicalUlid(expected.latestTurnId)
    && canonicalU64(expected.admissionRevision);
}

function terminalSettled(projection) {
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
    && exactIdleTurnOwner(projection);
}

function transcriptMarkerLeak(surface, markers) {
  const projectionRows = Array.isArray(surface?.projection?.transcript_rows)
    ? surface.projection.transcript_rows
    : [];
  const domRows = Array.isArray(surface?.all_transcript_rows)
    ? surface.all_transcript_rows
    : [];
  return markers.some((marker) => [...projectionRows, ...domRows].some((row) => {
    const serialized = JSON.stringify(row);
    return typeof serialized === "string" && serialized.includes(marker);
  }));
}

function controlTokenLeak(surface) {
  return transcriptMarkerLeak(surface, CONTROL_TOKENS);
}

function rawReasoningLeak(surface) {
  return transcriptMarkerLeak(
    surface,
    [SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RAW_REASONING_SENTINEL],
  );
}

function blockingSurfaceFailure(surface) {
  return surface?.visible_fatal_count > 0
    || surface?.visible_recoverable_error_count > 0
    || surface?.projection?.startup?.status === "failed"
    || rowsOfKind(surface?.projection, "error").length > 0
    || ["failed", "cancelled", "incomplete"].includes(surface?.projection?.run_status_key)
    || controlTokenLeak(surface)
    || rawReasoningLeak(surface);
}

function exactHeldProviderResource(resource) {
  return resource?.script_kind === SCRIPTED_PROVIDER_RESPONSES_COMPACTION_KIND
    && resource.request_count === HELD_ROLE_COUNT
    && resource.active_request_count === 1
    && resource.open_connection_count === 1
    && resource.accepted_response_count === HELD_ROLE_COUNT
    && resource.successful_response_count === SCRIPTED_PROVIDER_RESPONSES_COMPACTION_TOOL_CALL_COUNT
    && resource.scripted_responses_request_count === HELD_ROLE_COUNT
    && resource.scripted_responses_maximum === SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES
    && isDeepStrictEqual(resource.scripted_response_roles, HELD_ROLES)
    && resource.response_release_controlled === true
    && resource.response_release_count === 0
    && resource.response_release_cleanup_count === 0
    && isDeepStrictEqual(resource.script_role_release, {
      role: "compaction_empty",
      released: false,
      released_by_cleanup: false,
    });
}

function exactReleasedProviderResource(resource) {
  return resource?.script_kind === SCRIPTED_PROVIDER_RESPONSES_COMPACTION_KIND
    && resource.request_count === SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES
    && resource.active_request_count === 0
    && resource.accepted_response_count === SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES
    && resource.successful_response_count === SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES
    && resource.scripted_responses_request_count
      === SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES
    && resource.scripted_responses_maximum
      === SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES
    && isDeepStrictEqual(resource.scripted_response_roles, EXPECTED_ROLES)
    && resource.response_release_controlled === true
    && resource.response_release_count === 0
    && resource.response_release_cleanup_count === 0
    && isDeepStrictEqual(resource.script_role_release, {
      role: "compaction_empty",
      released: true,
      released_by_cleanup: false,
    });
}

export function responsesCompactionReasoningHeldFailures(sample) {
  const surface = sample?.surface;
  const projection = surface?.projection;
  const failures = [];
  if (!exactHeldCompactionLedger(sample?.ledger)) failures.push("reasoning-stream-not-exactly-held");
  if (!exactHeldProviderResource(sample?.resource)) failures.push("reasoning-release-owner-not-exact");
  if (projection?.run_status_key !== "running"
    || projection?.task_activity_state !== "running"
    || projection?.busy !== true
    || projection?.run_phase !== "Provider応答受信中") failures.push("held-run-not-provider-active");
  if (!Array.isArray(projection?.transcript_rows)
    || !Array.isArray(surface?.all_transcript_rows)) failures.push("held-transcript-rows-not-observed");
  if (blockingSurfaceFailure(surface)) failures.push("held-surface-blocking-failure");
  if (rawReasoningLeak(surface)) failures.push("raw-reasoning-visible-in-flight");
  return [...new Set(failures)];
}

export function responsesCompactionReasoningHeldDecision(sample) {
  const ledger = sample?.ledger;
  if (impossibleLedgerPrefix(ledger) || blockingSurfaceFailure(sample?.surface)) return "fail";
  if (!Array.isArray(ledger) || ledger.length < HELD_ROLE_COUNT) return "pending";
  if (ledger.length > HELD_ROLE_COUNT) return "fail";
  const phase = ledger.at(-1)?.response_phase;
  if (["completed", "rejected", "peer_closed"].includes(phase)) return "fail";
  if (sample?.resource?.script_role_release?.released === true) return "fail";
  return responsesCompactionReasoningHeldFailures(sample).length === 0 ? "pass" : "pending";
}

export function responsesCompactionTerminalFailures(sample) {
  const surface = sample?.surface;
  const projection = surface?.projection;
  const users = rowsOfKind(projection, "user");
  const assistants = rowsOfKind(projection, "assistant");
  const compactions = rowsOfKind(projection, "system").filter((row) => (
    row?.title === "システム - Context Compaction"
  ));
  const summaries = rowsOfKind(projection, "work_summary_completed");
  const failures = [];
  if (!exactResponsesCompactionLedger(sample?.ledger)) failures.push("wire-ledger-not-exact");
  if (!exactReleasedProviderResource(sample?.resource)) failures.push("reasoning-release-not-exact");
  if (!terminalSettled(projection) || blockingSurfaceFailure(surface)) {
    failures.push("terminal-surface-not-settled");
  }
  if (users.length !== 1
    || users[0]?.body !== SCRIPTED_PROVIDER_RESPONSES_COMPACTION_PROMPT
    || !canonicalUlid(users[0]?.stable_history_identity)) failures.push("canonical-user-not-exact");
  if (assistants.length !== 1
    || assistants[0]?.body !== SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RESPONSE) {
    failures.push("final-assistant-not-exact");
  }
  if (compactions.length !== 1
    || compactions[0]?.body !== `圧縮しました\n\n${SCRIPTED_PROVIDER_RESPONSES_COMPACTION_CHECKPOINT}`) {
    failures.push("canonical-compaction-not-exact");
  }
  if (summaries.length !== 1) failures.push("completed-work-summary-not-exact");
  if (!Array.isArray(surface?.all_transcript_rows)) failures.push("all-dom-rows-not-observed");
  if (rawReasoningLeak(surface)) failures.push("raw-reasoning-visible");
  if (typeof projection?.progress_text !== "string"
    || !projection.progress_text.includes(`モデル要求: ${SCRIPTED_PROVIDER_RESPONSES_COMPACTION_MAX_RESPONSES}`)
    || !projection.progress_text.includes(TOOL_PROGRESS)
    || !projection.progress_text.includes("圧縮: 1")) failures.push("progress-counts-not-exact");
  if (surface?.thread_count !== 1
    || surface?.users?.length !== 1
    || surface.users[0]?.visible !== true
    || surface.users[0]?.text !== SCRIPTED_PROVIDER_RESPONSES_COMPACTION_PROMPT
    || surface?.assistants?.length !== 1
    || surface.assistants[0]?.visible !== true
    || surface.assistants[0]?.text !== SCRIPTED_PROVIDER_RESPONSES_COMPACTION_RESPONSE
    || surface?.completed_summaries?.length !== 1
    || surface.completed_summaries[0]?.visible !== true) failures.push("terminal-dom-not-exact");
  const identity = selectedNavigationIdentity(projection);
  const expectedAction = identity?.project_id === null ? "chat-session" : "session";
  const expectedFocusKey = typeof identity?.session_id === "string"
    ? `${expectedAction}:${identity.session_id}:select`
    : null;
  if (surface?.selected_navigation?.length !== 1
    || surface.selected_navigation[0]?.visible !== true
    || surface.selected_navigation[0]?.action !== expectedAction
    || surface.selected_navigation[0]?.focus_key !== expectedFocusKey) {
    failures.push("selected-navigation-not-exact");
  }
  if (surface?.prompt?.count !== 1
    || surface.prompt.value !== ""
    || surface.prompt.visible !== true
    || surface.prompt.enabled !== true) failures.push("terminal-composer-not-exact");
  return [...new Set(failures)];
}

export function responsesCompactionTerminalDecision(sample) {
  if (impossibleLedgerPrefix(sample?.ledger) || blockingSurfaceFailure(sample?.surface)) return "fail";
  return responsesCompactionTerminalFailures(sample).length === 0 ? "pass" : "pending";
}

async function waitForProductStage({
  label,
  sample,
  decide,
  code,
  message,
  stabilityMs = 0,
}) {
  let decision = "pending";
  let stableSince = null;
  let observed;
  try {
    observed = await waitForObservation({
      label,
      timeoutMs: 60_000,
      pollMs: 50,
      retrySampleErrors: false,
      sample,
      accept: (value) => {
        decision = decide(value);
        if (decision === "fail") return true;
        if (decision !== "pass") {
          stableSince = null;
          return false;
        }
        if (stableSince === null) stableSince = Date.now();
        return Date.now() - stableSince >= stabilityMs;
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
  return { target, probe };
}

async function trustedPromptInput(input) {
  const focus = await trustedClick(input, PROMPT);
  const start = (await input.snapshotProbe()).sequence;
  const insertion = await input.insertText(PROMPT, SCRIPTED_PROVIDER_RESPONSES_COMPACTION_PROMPT);
  const probe = assertTrustedTextInsertion(await input.snapshotProbe(start), {
    afterSequence: start,
    identity: PROMPT.identity,
    text: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_PROMPT,
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
      "provider-responses-compaction-resource-cleanup-failed",
      "Responses compaction input and command probes did not settle",
      outcome,
    );
  }
}

export function createProviderResponsesCompactionRetryScenario() {
  const state = {
    provider: null,
    acceptedLedger: null,
    quiesceOutcome: null,
    resourceOutcome: null,
  };
  return Object.freeze({
    id: "provider.responses-compaction-retry",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    requestGracefulExit,
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({
        expectedPrompt: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_PROMPT,
        responseBehavior: "hold_until_release",
        script: createResponsesCompactionProviderScript({
          inputByteThreshold: PROVIDER_RESPONSES_COMPACTION_INPUT_BYTE_THRESHOLD,
        }),
      });
      await prepareDesktopFixture({
        context,
        sink,
        phase,
        owner: OWNER,
        configText: providerResponsesCompactionFixtureConfig(state.provider.baseUrl),
        sentinelName: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_SENTINEL,
        sentinelText: providerResponsesCompactionSentinelText(),
      });
      await sink.record("scripted-provider-started", state.provider.resourceObservation(), {
        phase,
        owner: OWNER,
      });
    },
    async execute({ context, driver: cdp, sink }) {
      const provider = state.provider;
      if (provider === null) throw new Error("scripted Responses compaction provider was not prepared");
      await acquireInteractiveShell({ context, driver: cdp, sink }, {
        evidenceOwner: OWNER,
        screenshotStem: "provider-responses-compaction-shell-ready",
      });
      if (provider.requestLedger.length !== 0) {
        throw productFailure(
          "provider-responses-compaction-cold-start-request",
          "Desktop contacted the compaction provider before trusted Send",
          { ledger: provider.requestLedger },
        );
      }

      const input = new WebviewInput(cdp, { probeId: "provider-responses-compaction-retry" });
      const commands = new DesktopCommandProbe(cdp, {
        probeId: "provider-responses-compaction-retry-commands",
        commands: ["submit_prompt", "cancel_run"],
      });
      let primaryError = null;
      try {
        await input.installProbe();
        await commands.install();
        const typed = await trustedPromptInput(input);
        const ready = await observeProviderTurnSurface(cdp);
        if (ready.prompt.value !== SCRIPTED_PROVIDER_RESPONSES_COMPACTION_PROMPT) {
          throw productFailure(
            "provider-responses-compaction-prompt-drift",
            "trusted text insertion did not produce the exact compaction prompt",
            { expected: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_PROMPT, surface: ready },
          );
        }
        const expectedCommand = {
          command: "submit_prompt",
          args: {
            text: SCRIPTED_PROVIDER_RESPONSES_COMPACTION_PROMPT,
            expectedTarget: structuredClone(ready.projection.draft_target),
            expectedRunTarget: structuredClone(ready.projection.run_target),
          },
        };
        const commandStart = (await commands.snapshot()).sequence;
        const send = await trustedClick(input, SEND);
        const commandObservation = await waitForObservation({
          label: "provider.responses-compaction-retry exact submit command",
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
          label: "raw reasoning delta held without projection or DOM disclosure",
          sample: async () => ({
            surface: await observeProviderTurnSurface(cdp),
            ledger: provider.requestLedger,
            resource: provider.resourceObservation(),
          }),
          decide: responsesCompactionReasoningHeldDecision,
          code: "provider-responses-compaction-reasoning-disclosure",
          message: "the raw reasoning delta was not held privately across the runtime projection and DOM",
          stabilityMs: REASONING_HOLD_STABILITY_MS,
        });
        await sink.record("provider-responses-compaction-reasoning-held", {
          stability_ms: REASONING_HOLD_STABILITY_MS,
          failures: responsesCompactionReasoningHeldFailures(held.value),
          provider_ledger: held.value.ledger,
          provider_resource: held.value.resource,
          projection: held.value.surface.projection,
          dom_transcript_rows: held.value.surface.all_transcript_rows,
        }, { phase: "executing", owner: OWNER });
        const reasoningRelease = provider.releaseScriptRole("compaction_empty");
        await sink.record("provider-responses-compaction-reasoning-released", reasoningRelease, {
          phase: "executing",
          owner: OWNER,
        });

        const terminal = await waitForProductStage({
          label: "bounded reasoning-only Responses compaction retry and GUI completion",
          sample: async () => ({
            surface: await observeProviderTurnSurface(cdp),
            ledger: provider.requestLedger,
            resource: provider.resourceObservation(),
          }),
          decide: responsesCompactionTerminalDecision,
          code: "provider-responses-compaction-terminal-mismatch",
          message: "the short Responses run did not preserve bounded retry, canonical compaction, and terminal GUI contracts",
        });
        const finalCommand = assertExactDesktopCommandSequence(await commands.snapshot(commandStart), {
          afterSequence: commandStart,
          expected: [expectedCommand],
        });
        const screenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "provider-responses-compaction-completed",
          owner: OWNER,
        });
        state.acceptedLedger = structuredClone(terminal.value.ledger);
        await sink.record("provider-responses-compaction-completed", {
          input_kind: "browser_trusted",
          typed,
          send,
          expected_command: expectedCommand,
          command: exactCommand,
          final_command: finalCommand,
          reasoning_hold: {
            stability_ms: REASONING_HOLD_STABILITY_MS,
            failures: responsesCompactionReasoningHeldFailures(held.value),
            request_sequence: held.value.ledger.at(-1)?.sequence ?? null,
            response_stream: held.value.ledger.at(-1)?.response_stream ?? null,
          },
          reasoning_release: reasoningRelease,
          failures: responsesCompactionTerminalFailures(terminal.value),
          provider_ledger: state.acceptedLedger,
          provider_resource: provider.resourceObservation(),
          projection: terminal.value.surface.projection,
          screenshot,
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
          kind: "provider-responses-compaction-retry-verification",
          quiesced,
          quiesce_input: state.quiesceOutcome?.input ?? null,
          interaction_resources: state.resourceOutcome,
        }],
      };
    },
  });
}

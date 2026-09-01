import crypto from "node:crypto";
import { spawn } from "node:child_process";
import path from "node:path";
import { isDeepStrictEqual } from "node:util";
import { readdir, readFile, writeFile } from "node:fs/promises";

import { waitForObservation } from "../core/deadline.mjs";
import { canonicalU64, canonicalUlid } from "../core/canonical_identity.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import {
  DesktopCommandProbe,
  assertExactDesktopCommandSequence,
} from "../drivers/desktop_command_probe.mjs";
import {
  SCRIPTED_PROVIDER_PERMISSION_TEMP_ESCALATION_MAX_RESPONSES,
  createPermissionTempEscalationProviderScript,
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
import { captureScenarioScreenshot } from "./observations.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";

const OWNER = "scenario:permission.temp-escalation";
const FIXTURE_SENTINEL = "E2E_PERMISSION_TEMP_ESCALATION.txt";
const PYTEST_FIXTURE = "test_e2e_owner_only_temp.py";
const PYTEST_FIXTURE_TEXT = `def test_cpython_owner_only_temp(tmp_path):
    assert tmp_path.is_dir()
`;
const LIVE_REQUEST_TIMEOUT_MS = 180_000;
const LIVE_TURN_TIMEOUT_MS = 720_000;
const LIVE_CAPTURE_DIRECTORY = "request-capture";
export const PERMISSION_TEMP_ESCALATION_TERMINAL_TIMEOUT_MS = 150_000;

export const PERMISSION_TEMP_ESCALATION_PROMPT = "run the owner only temp regression";
export const PERMISSION_TEMP_ESCALATION_COMMAND =
  "python -B -X utf8 -m pytest -p no:cacheprovider -q --tb=short test_e2e_owner_only_temp.py";
export const PERMISSION_TEMP_ESCALATION_JUSTIFICATION =
  "retry the exact trusted pytest command outside the workspace-write sandbox";
export const PERMISSION_TEMP_ESCALATION_RESPONSE = "PERMISSION_TEMP_ESCALATION_OK";
export const PERMISSION_TEMP_ESCALATION_REQUEST_TIMEOUT_MS = 120_000;
export const PERMISSION_TEMP_ESCALATION_GUARDIAN_DELAY_MS = 95_000;
export const PERMISSION_TEMP_ESCALATION_TERMINAL_CONFIRMATION_MS = 500;
export const PERMISSION_TEMP_ESCALATION_LIVE_RESPONSE = "PERMISSION_TEMP_ESCALATION_LIVE_OK";
export const PERMISSION_TEMP_ESCALATION_LIVE_PROMPT = [
  "Perform this exact bounded permission regression using shell only.",
  `First call shell with exactly ${JSON.stringify(PERMISSION_TEMP_ESCALATION_COMMAND)} and sandbox_permissions=use_default.`,
  "Do not set TMP, TEMP, or TMPDIR; do not use --basetemp; do not change project files or use another tool to bypass the sandbox.",
  "Only if the host result contains kind workspace_write_effect_temp_access_denied and explicitly instructs a new elevated call, call shell a second time with the exact same command, sandbox_permissions=require_escalated, and the exact justification below.",
  `Justification: ${PERMISSION_TEMP_ESCALATION_JUSTIFICATION}`,
  `After the elevated command reports one passing test, reply only ${PERMISSION_TEMP_ESCALATION_LIVE_RESPONSE}.`,
].join("\n");

const PERMISSION_TEMP_ESCALATION_ROLES = Object.freeze([
  "temp_initial",
  "temp_escalation",
  "temp_guardian",
  "temp_continuation",
]);
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

function renderedDomTextMatchesAuthority(observed, authority) {
  if (typeof observed !== "string" || typeof authority !== "string") return false;
  const normalize = (value) => value.replace(/[\t\n\f\r ]+/gu, " ").trim();
  return normalize(observed) === normalize(authority);
}

export function permissionTempEscalationFixtureConfig(baseUrl) {
  return `[model]
base_url = ${JSON.stringify(baseUrl)}
model = "e2e/scripted-responses"
provider_profile = "lm_studio"
connect_timeout_ms = 1000
request_timeout_ms = ${PERMISSION_TEMP_ESCALATION_REQUEST_TIMEOUT_MS}
max_retries = 0
context_window = 65536
supports_tools = true
supports_images = false
parallel_tool_calls = false

[permissions]
access_mode = "auto_review"

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

function canonicalLiveBaseUrl(value) {
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new TypeError(
      "manual.permission-temp-escalation-lm-studio provider_base_url must be a non-empty string",
    );
  }
  let url;
  try { url = new URL(value.trim()); }
  catch (error) {
    throw new TypeError(
      `manual.permission-temp-escalation-lm-studio provider_base_url is invalid: ${error.message}`,
    );
  }
  if (!new Set(["http:", "https:"]).has(url.protocol)
    || url.username.length > 0
    || url.password.length > 0
    || url.search.length > 0
    || url.hash.length > 0) {
    throw new TypeError(
      "manual.permission-temp-escalation-lm-studio provider_base_url must be one credential-free HTTP(S) endpoint without query or fragment",
    );
  }
  return url.toString().replace(/\/$/u, "");
}

function canonicalLiveModel(value) {
  if (typeof value !== "string") {
    throw new TypeError("manual.permission-temp-escalation-lm-studio model must be a string");
  }
  const normalized = value.trim();
  if (normalized.length === 0
    || Buffer.byteLength(normalized, "utf8") > 1024
    || /[\u0000-\u001f\u007f]/u.test(normalized)) {
    throw new TypeError(
      "manual.permission-temp-escalation-lm-studio model must be a non-empty bounded model ID without control characters",
    );
  }
  return normalized;
}

export function normalizePermissionTempEscalationLmStudioOptions(options) {
  if (options === null || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError(
      "manual.permission-temp-escalation-lm-studio requires one scenario config object",
    );
  }
  const allowed = new Set(["provider_base_url", "model"]);
  const unknown = Object.keys(options).filter((key) => !allowed.has(key));
  if (unknown.length > 0) {
    throw new TypeError(
      `unknown manual.permission-temp-escalation-lm-studio option: ${unknown.join(",")}`,
    );
  }
  return Object.freeze({
    providerBaseUrl: canonicalLiveBaseUrl(options.provider_base_url),
    model: canonicalLiveModel(options.model),
  });
}

export function permissionTempEscalationLmStudioFixtureConfig(options) {
  return `[model]
base_url = ${JSON.stringify(options.providerBaseUrl)}
model = ${JSON.stringify(options.model)}
provider_profile = "lm_studio"
provider_metadata_mode = "lm_studio_native_required"
provider_api_mode = "responses"
connect_timeout_ms = 10000
request_timeout_ms = ${LIVE_REQUEST_TIMEOUT_MS}
max_retries = 0
context_window = 32768
supports_tools = true
supports_images = false
parallel_tool_calls = false
max_parallel_predictions = 1

[permissions]
access_mode = "auto_review"

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

function responseRows(ledger) {
  return Array.isArray(ledger) ? ledger.filter((row) => row?.route === "responses") : [];
}

function acceptedMetadataRow(row) {
  return (row?.route === "models" || row?.route === "lm_studio_models")
    && row.method === "GET"
    && row.response_phase === "completed"
    && row.response_status === 200;
}

function acceptedResponseRow(row, role, phase) {
  return row?.route === "responses"
    && row.method === "POST"
    && row.pathname === "/v1/responses"
    && row.query_present === false
    && row.contract?.pass === true
    && row.contract.role === role
    && row.response_phase === phase
    && row.response_status === (phase === "completed" ? 200 : null);
}

export function exactPermissionTempEscalationLedger(ledger, expectedRoles, {
  heldRole = null,
  guardianDelayMs = null,
} = {}) {
  if (!Array.isArray(ledger) || !Array.isArray(expectedRoles)) return false;
  if (ledger.some((row) => row?.route !== "responses" && !acceptedMetadataRow(row))) return false;
  const rows = responseRows(ledger);
  return rows.length === expectedRoles.length
    && rows.every((row, index) => acceptedResponseRow(
      row,
      expectedRoles[index],
      expectedRoles[index] === heldRole ? "held" : "completed",
    ))
    && (guardianDelayMs === null || rows.some((row) => (
      row?.contract?.role === "temp_guardian"
      && row?.response_delay?.schema_version
        === "desktop-e2e.scripted-provider-guardian-delay.v1"
      && row.response_delay.configured_delay_ms === guardianDelayMs
      && row.response_delay.delay_completed === true
      && row.response_delay.peer_close_observed === false
      && Number.isSafeInteger(row.response_delay.delay_elapsed_ms)
      && row.response_delay.delay_elapsed_ms >= guardianDelayMs
      && Number.isSafeInteger(row.response_delay.headers_sent_elapsed_ms)
      && row.response_delay.headers_sent_elapsed_ms >= row.response_delay.delay_elapsed_ms
    )));
}

function providerOrSurfaceFailed(surface, ledger) {
  const rows = responseRows(ledger);
  return !Array.isArray(ledger)
    || rows.length > SCRIPTED_PROVIDER_PERMISSION_TEMP_ESCALATION_MAX_RESPONSES
    || ledger.some((row) => row?.route !== "responses" && !acceptedMetadataRow(row))
    || rows.some((row) => row?.contract?.pass === false
      || row?.response_phase === "rejected"
      || row?.response_phase === "peer_closed"
      || (row?.response_status !== null && row.response_status !== 200))
    || surface?.visible_fatal_count > 0
    || surface?.visible_recoverable_error_count > 0
    || surface?.visible_dialog_count > 0
    || surface?.visible_modal_backdrop_count > 0
    || surface?.projection?.startup?.status === "failed"
    || ["failed", "cancelled", "incomplete"].includes(surface?.projection?.run_status_key);
}

function rowsOfKind(projection, kind) {
  const rows = Array.isArray(projection?.transcript_rows) ? projection.transcript_rows : [];
  return rows.filter((row) => row?.row_kind === kind);
}

function exactOwner(projection, kind) {
  const expected = projection?.run_target?.expectedState;
  if (expected?.kind !== kind
    || !canonicalUlid(projection?.run_target?.sessionId)
    || projection.run_target.sessionId !== projection?.draft_target?.sessionId
    || !canonicalU64(expected.admissionRevision)) return null;
  const turnId = kind === "turn" ? expected.turnId : expected.latestTurnId;
  return canonicalUlid(turnId)
    ? {
      sessionId: projection.run_target.sessionId,
      turnId,
      admissionRevision: expected.admissionRevision,
    }
    : null;
}

function exactRestrictedHintEvidence(ledger) {
  const row = responseRows(ledger).find((candidate) => candidate?.contract?.role === "temp_escalation");
  const evidence = row?.contract?.role_evidence?.restricted_output;
  return evidence?.pass === true
    && evidence.command_matches === true
    && evidence.host_non_success === true
    && evidence.shell_tool === true
    && evidence.completed_lifecycle === true
    && evidence.hint_kind === true
    && evidence.automatic_retry_false === true
    && evidence.exit_code_one === true
    && evidence.guidance_present === true
    && evidence.exact_escalation_hint === true
    && evidence.no_project_workaround_hint === true
    && evidence.single_note === true
    && evidence.same_line_windows_signature === true
    && Number.isSafeInteger(evidence.output_size_bytes)
    && evidence.output_size_bytes > 0
    && /^[a-f0-9]{64}$/u.test(evidence.output_sha256 ?? "");
}

const SQLITE_TERMINATION_TIMEOUT_MS = 5_000;

export function runReadOnlySqlite(
  database,
  query,
  timeoutMs = 10_000,
  {
    spawnProcess = spawn,
    terminationTimeoutMs = SQLITE_TERMINATION_TIMEOUT_MS,
  } = {},
) {
  return new Promise((resolve, reject) => {
    const child = spawnProcess(
      "sqlite3.exe",
      ["-batch", "-bail", "-readonly", "-json", database, query],
      { windowsHide: true, stdio: ["ignore", "pipe", "pipe"] },
    );
    let stdout = "";
    let stderr = "";
    let settled = false;
    let timedOut = false;
    let spawnError = null;
    let timer = null;
    let terminationTimer = null;
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    const finish = (action) => {
      if (settled) return;
      settled = true;
      if (timer !== null) clearTimeout(timer);
      if (terminationTimer !== null) clearTimeout(terminationTimer);
      action();
    };
    timer = setTimeout(() => {
      timedOut = true;
      let killAccepted = false;
      let killError = null;
      terminationTimer = setTimeout(() => finish(() => reject(new Error(
        killError === null && killAccepted
          ? "read-only permission TEMP SQLite query did not close after termination"
          : "read-only permission TEMP SQLite query could not be terminated",
        { cause: killError ?? undefined },
      ))), terminationTimeoutMs);
      try {
        killAccepted = child.kill();
      } catch (error) {
        killError = error;
      }
    }, timeoutMs);
    child.once("error", (error) => { spawnError = error; });
    child.once("close", (exitCode) => finish(() => {
      if (timedOut) {
        reject(new Error("read-only permission TEMP SQLite query timed out"));
      } else if (spawnError !== null) {
        reject(spawnError);
      } else {
        resolve({
          exit_code: exitCode,
          stdout: stdout.trim(),
          stderr: stderr.trim(),
        });
      }
    }));
  });
}

export function permissionTempEscalationPersistenceFailures(evidence, owner) {
  const failures = [];
  const rows = evidence?.rows;
  if (evidence?.schema_version !== "desktop-e2e.permission-temp-persistence.v1"
    || evidence?.read_only !== true
    || !Array.isArray(rows)) {
    return ["persistence-evidence-malformed"];
  }
  if (!canonicalUlid(owner?.sessionId) || !canonicalUlid(owner?.turnId)) {
    return ["persistence-owner-malformed"];
  }
  if (rows.length !== 2) return ["persistence-tool-output-cardinality-mismatch"];
  const expectedKeys = [
    "history_item_id",
    "session_id",
    "turn_id",
    "sequence_no",
    "kind",
    "call_id",
    "status",
    "success",
    "metadata_success",
    "tool_metadata_success",
    "sandbox_failure_hint",
  ];
  for (const [index, row] of rows.entries()) {
    if (!exactObjectKeys(row, expectedKeys)
      || !canonicalUlid(row.history_item_id)
      || !canonicalUlid(row.call_id)
      || row.session_id !== owner.sessionId
      || row.turn_id !== owner.turnId
      || !Number.isSafeInteger(row.sequence_no)
      || row.sequence_no < 0
      || row.kind !== "tool_output"
      || row.status !== "completed") {
      failures.push(`persistence-tool-output-${index}-identity-mismatch`);
    }
  }
  if (!(rows[0].success === 0
    && rows[0].metadata_success === 0
    && rows[0].tool_metadata_success === 0
    && rows[0].sandbox_failure_hint === "workspace_write_effect_temp_access_denied")) {
    failures.push("persistence-restricted-success-projection-mismatch");
  }
  if (!(rows[1].success === 1
    && rows[1].metadata_success === 1
    && rows[1].tool_metadata_success === 1
    && rows[1].sandbox_failure_hint === null)) {
    failures.push("persistence-elevated-success-projection-mismatch");
  }
  if (!(rows[0].sequence_no < rows[1].sequence_no)) {
    failures.push("persistence-tool-output-order-mismatch");
  }
  return [...new Set(failures)];
}

export async function readPermissionTempEscalationPersistence({ database, owner }) {
  if (typeof database !== "string" || !path.isAbsolute(database)) {
    throw new TypeError("permission TEMP persistence database must be an absolute path");
  }
  if (!canonicalUlid(owner?.sessionId) || !canonicalUlid(owner?.turnId)) {
    throw new TypeError("permission TEMP persistence owner must contain canonical session and turn IDs");
  }
  const result = await runReadOnlySqlite(database, `
SELECT
  id AS history_item_id,
  session_id,
  turn_id,
  sequence_no,
  json_extract(payload_json, '$.kind') AS kind,
  json_extract(payload_json, '$.call_id') AS call_id,
  json_extract(payload_json, '$.status') AS status,
  json_extract(payload_json, '$.success') AS success,
  json_extract(payload_json, '$.metadata.success') AS metadata_success,
  json_extract(payload_json, '$.metadata.tool_metadata.success') AS tool_metadata_success,
  json_extract(payload_json, '$.metadata.tool_metadata.sandbox_failure_hint') AS sandbox_failure_hint
FROM protocol_history_items
WHERE session_id = '${owner.sessionId}'
  AND turn_id = '${owner.turnId}'
  AND json_extract(payload_json, '$.kind') = 'tool_output'
ORDER BY sequence_no ASC;
`);
  if (result.exit_code !== 0 || result.stderr.length > 0) {
    throw new Error(`read-only permission TEMP SQLite query failed: ${result.stderr || result.stdout}`);
  }
  let rows;
  try { rows = JSON.parse(result.stdout.length === 0 ? "[]" : result.stdout); }
  catch (error) {
    throw new Error(`read-only permission TEMP SQLite query returned malformed JSON: ${error.message}`);
  }
  if (!Array.isArray(rows)) {
    throw new Error("read-only permission TEMP SQLite query did not return a JSON array");
  }
  return {
    schema_version: "desktop-e2e.permission-temp-persistence.v1",
    read_only: true,
    rows,
  };
}

export function permissionTempEscalationHeldFailures(sample) {
  const surface = sample?.surface;
  const projection = surface?.projection;
  const users = rowsOfKind(projection, "user");
  const assistants = rowsOfKind(projection, "assistant");
  const errors = rowsOfKind(projection, "error");
  const running = rowsOfKind(projection, "work_summary_running");
  const completed = rowsOfKind(projection, "work_summary_completed");
  const failures = [];
  if (!exactPermissionTempEscalationLedger(
    sample?.ledger,
    ["temp_initial", "temp_escalation"],
    { heldRole: "temp_escalation" },
  )) failures.push("restricted-escalation-request-not-exactly-held");
  if (!exactRestrictedHintEvidence(sample?.ledger)) failures.push("restricted-host-hint-not-exact");
  if (projection?.run_status_key !== "running"
    || projection?.task_activity_state !== "running"
    || projection?.busy !== true
    || projection?.agent_tree_active !== false
    || exactOwner(projection, "turn") === null) failures.push("held-run-owner-not-active");
  if (!isDeepStrictEqual(users.map((row) => row.body), [PERMISSION_TEMP_ESCALATION_PROMPT])) {
    failures.push("held-user-authority-mismatch");
  }
  if (assistants.length !== 0 || errors.length !== 0) failures.push("held-primary-error-or-answer-present");
  if (running.length !== 1 || completed.length !== 0) {
    failures.push("held-work-summary-not-exact");
  }
  if (surface?.users?.length !== 1
    || surface.users[0].visible !== true
    || surface.users[0].text !== PERMISSION_TEMP_ESCALATION_PROMPT
    || surface?.assistants?.length !== 0) failures.push("held-dom-conversation-mismatch");
  if (providerOrSurfaceFailed(surface, sample?.ledger)) failures.push("held-surface-or-provider-failed");
  return [...new Set(failures)];
}

export function permissionTempEscalationHeldDecision(sample) {
  if (providerOrSurfaceFailed(sample?.surface, sample?.ledger)) return "fail";
  return permissionTempEscalationHeldFailures(sample).length === 0 ? "pass" : "pending";
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
    && projection?.draft_prompt === ""
    && projection?.composer_submit_mode === "new_request"
    && projection?.can_submit === true
    && exactOwner(projection, "idle") !== null
    && surface?.prompt?.count === 1
    && surface.prompt.value === ""
    && surface.prompt.visible === true
    && surface.prompt.enabled === true
    && surface?.send?.count === 1
    && surface.send.visible === true
    && surface.send.enabled === false
    && surface?.visible_fatal_count === 0
    && surface?.visible_recoverable_error_count === 0
    && surface?.visible_dialog_count === 0
    && surface?.visible_modal_backdrop_count === 0;
}

function exactTwoShellLifecycleHistory(projection, completedSummary) {
  const body = completedSummary?.body ?? "";
  const lifecycleRows = body.match(
    /^- \[(?:待機|実行中|完了|拒否|キャンセル|失敗)\] /gmu,
  ) ?? [];
  const pendingRows = body.match(/^- \[待機\] shell$/gmu) ?? [];
  const completedRows = body.match(/^- \[完了\] /gmu) ?? [];
  const nonSuccessRows = body.match(/^- \[(?:実行中|拒否|キャンセル|失敗)\] /gmu) ?? [];
  const toolStatusText = projection?.tool_status_text ?? "";
  return body.includes("- コマンド/ツール: 4件")
    && lifecycleRows.length === 4
    && pendingRows.length === 2
    && completedRows.length === 2
    && nonSuccessRows.length === 0
    && (toolStatusText.match(/\[completed\]/gu) ?? []).length === 2
    && !/\[(?:pending|running|declined|cancelled|failed)\]/u.test(toolStatusText)
    && (projection?.progress_text ?? "").includes(
      "ツール: 2件開始 / 2件完了 / 0件拒否 / 0件キャンセル / 0件失敗",
    );
}

export function permissionTempEscalationTerminalFailures(sample) {
  const surface = sample?.surface;
  const projection = surface?.projection;
  const users = rowsOfKind(projection, "user");
  const assistants = rowsOfKind(projection, "assistant");
  const errors = rowsOfKind(projection, "error");
  const running = rowsOfKind(projection, "work_summary_running");
  const completed = rowsOfKind(projection, "work_summary_completed");
  const failures = [];
  if (!exactPermissionTempEscalationLedger(sample?.ledger, PERMISSION_TEMP_ESCALATION_ROLES, {
    guardianDelayMs: PERMISSION_TEMP_ESCALATION_GUARDIAN_DELAY_MS,
  })) {
    failures.push("provider-role-ledger-mismatch");
  }
  if (!exactRestrictedHintEvidence(sample?.ledger)) failures.push("restricted-host-hint-not-exact");
  const success = responseRows(sample?.ledger).find(
    (row) => row?.contract?.role === "temp_continuation",
  )?.contract?.role_evidence?.elevated_output;
  if (success?.pass !== true
    || success.command_matches !== true
    || success.exit_code_zero !== true
    || success.pytest_passed !== true
    || success.effect_temp_absent !== true
    || success.sandbox_note_absent !== true) failures.push("elevated-pytest-success-not-exact");
  if (!terminalSettled(surface)) failures.push("terminal-surface-not-settled");
  if (!isDeepStrictEqual(users.map((row) => row.body), [PERMISSION_TEMP_ESCALATION_PROMPT])) {
    failures.push("terminal-user-authority-mismatch");
  }
  if (!isDeepStrictEqual(assistants.map((row) => row.body), [PERMISSION_TEMP_ESCALATION_RESPONSE])) {
    failures.push("terminal-assistant-response-mismatch");
  }
  if (errors.length !== 0 || running.length !== 0 || completed.length !== 1) {
    failures.push("terminal-history-cardinality-mismatch");
  }
  if (completed.length === 1 && !exactTwoShellLifecycleHistory(projection, completed[0])) {
    failures.push("terminal-tool-history-mismatch");
  }
  if (surface?.users?.length !== 1
    || surface.users[0].visible !== true
    || surface.users[0].text !== PERMISSION_TEMP_ESCALATION_PROMPT
    || surface?.assistants?.length !== 1
    || surface.assistants[0].visible !== true
    || surface.assistants[0].text !== PERMISSION_TEMP_ESCALATION_RESPONSE
    || surface?.completed_summaries?.length !== 1
    || surface.completed_summaries[0].visible !== true) failures.push("terminal-dom-conversation-mismatch");
  if (providerOrSurfaceFailed(surface, sample?.ledger)) failures.push("terminal-surface-or-provider-failed");
  return [...new Set(failures)];
}

function createConfirmedTerminalDecision({
  failuresOf,
  surfaceOf,
  providerFailed,
  domOnlyFailures,
  confirmationMs,
  now,
}) {
  let confirmedSince = null;
  let confirmedSignature = null;
  const reset = () => {
    confirmedSince = null;
    confirmedSignature = null;
  };
  return (sample) => {
    if (providerFailed(sample)) {
      reset();
      return "fail";
    }
    const failures = failuresOf(sample);
    if (failures.length === 0) {
      reset();
      return "pass";
    }
    if (!terminalSettled(surfaceOf(sample))) {
      reset();
      return "pending";
    }
    const confirmedFailures = failures.filter((failure) => !domOnlyFailures.has(failure));
    if (confirmedFailures.length === 0) {
      reset();
      return "pending";
    }
    const signature = JSON.stringify([...confirmedFailures].sort());
    const observedAt = now();
    if (confirmedSignature !== signature) {
      confirmedSignature = signature;
      confirmedSince = observedAt;
      return "pending";
    }
    return observedAt - confirmedSince >= confirmationMs ? "fail" : "pending";
  };
}

export function createPermissionTempEscalationTerminalDecision({
  confirmationMs = PERMISSION_TEMP_ESCALATION_TERMINAL_CONFIRMATION_MS,
  now = () => Date.now(),
} = {}) {
  if (!Number.isSafeInteger(confirmationMs) || confirmationMs <= 0) {
    throw new TypeError("permission TEMP terminal confirmation must be a positive safe integer");
  }
  if (typeof now !== "function") throw new TypeError("permission TEMP terminal clock must be a function");
  return createConfirmedTerminalDecision({
    failuresOf: permissionTempEscalationTerminalFailures,
    surfaceOf: (sample) => sample?.surface,
    providerFailed: (sample) => providerOrSurfaceFailed(sample?.surface, sample?.ledger),
    domOnlyFailures: new Set(["terminal-dom-conversation-mismatch"]),
    confirmationMs,
    now,
  });
}

export function permissionTempEscalationTerminalFailureEvidence(sample) {
  return {
    ...sample,
    terminal_oracle_failures: permissionTempEscalationTerminalFailures(sample),
  };
}

function exactObjectKeys(value, expected) {
  return value !== null
    && typeof value === "object"
    && !Array.isArray(value)
    && JSON.stringify(Object.keys(value).sort()) === JSON.stringify([...expected].sort());
}

function responsesInputText(item) {
  if (!exactObjectKeys(item, ["content", "role", "type"])
    || item.type !== "message"
    || item.role !== "user"
    || !Array.isArray(item.content)
    || item.content.length !== 1) return null;
  const content = item.content[0];
  return exactObjectKeys(content, ["text", "type"])
    && content.type === "input_text"
    && typeof content.text === "string"
    ? content.text
    : null;
}

function liveShellArguments(item, expectedCallId = null) {
  if (!exactObjectKeys(item, ["arguments", "call_id", "name", "type"])
    || item.type !== "function_call"
    || item.name !== "shell"
    || typeof item.call_id !== "string"
    || item.call_id.length === 0
    || (expectedCallId !== null && item.call_id !== expectedCallId)
    || typeof item.arguments !== "string") return null;
  let args;
  try { args = JSON.parse(item.arguments); }
  catch { return null; }
  return { callId: item.call_id, argumentsJson: item.arguments, args };
}

function liveToolOutput(item, callId) {
  return exactObjectKeys(item, ["call_id", "output", "type"])
    && item.type === "function_call_output"
    && item.call_id === callId
    && typeof item.output === "string"
    && item.output.trim().length > 0
    ? item.output
    : null;
}

function exactLiveShellArgumentKeys(args, requiredKeys) {
  return exactObjectKeys(args, requiredKeys)
    || (exactObjectKeys(args, [...requiredKeys, "description"])
      && typeof args.description === "string");
}

function exactLiveRestrictedArguments(call) {
  return call !== null
    && exactLiveShellArgumentKeys(call.args, ["command", "sandbox_permissions"])
    && call.args.command === PERMISSION_TEMP_ESCALATION_COMMAND
    && call.args.sandbox_permissions === "use_default";
}

function exactLiveElevatedArguments(call) {
  return call !== null
    && exactLiveShellArgumentKeys(
      call.args,
      ["command", "justification", "sandbox_permissions"],
    )
    && call.args.command === PERMISSION_TEMP_ESCALATION_COMMAND
    && call.args.sandbox_permissions === "require_escalated"
    && call.args.justification === PERMISSION_TEMP_ESCALATION_JUSTIFICATION;
}

function liveRestrictedOutputPass(output) {
  if (typeof output !== "string") return false;
  const lines = output.split(/\r?\n/u);
  return lines.includes(`Command: ${PERMISSION_TEMP_ESCALATION_COMMAND}`)
    && output.includes("Tool outcome (host projection): non-success")
    && output.includes('tool: "shell"')
    && output.includes("lifecycle_status: completed")
    && output.includes("kind: workspace_write_effect_temp_access_denied")
    && output.includes("automatic_retry: false")
    && output.includes("exit_code: 1")
    && output.includes("guidance: Sandbox note:")
    && output.includes("sandbox_permissions=require_escalated")
    && output.includes("do not change project files solely to bypass this sandbox restriction")
    && output.split("Sandbox note:").length - 1 === 1
    && /^.*permissionerror: \[winerror 5\].*moyai-sandbox-effect-.*$/imu.test(output);
}

function liveElevatedOutputPass(output) {
  if (typeof output !== "string") return false;
  return output.split(/\r?\n/u).includes(`Command: ${PERMISSION_TEMP_ESCALATION_COMMAND}`)
    && output.includes("Exit code: 0")
    && /(?:^|\n)1 passed(?:\s|$)/u.test(output)
    && !output.includes("moyai-sandbox-effect-")
    && !output.includes("Sandbox note:");
}

const LIVE_FORBIDDEN_WIRE_KEYS = Object.freeze([
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
const LIVE_TASK_KEYS = Object.freeze([
  "input",
  "instructions",
  "model",
  "parallel_tool_calls",
  "store",
  "stream",
  "tool_choice",
  "tools",
]);
const LIVE_GUARDIAN_KEYS = Object.freeze([
  "input",
  "instructions",
  "model",
  "store",
  "stream",
]);

function liveGuardianPayloadPass(body, elevatedCall) {
  const input = Array.isArray(body?.input) ? body.input : [];
  const inputText = input.length === 1 ? responsesInputText(input[0]) : null;
  let payload = null;
  let context = null;
  try {
    payload = JSON.parse(inputText);
    context = JSON.parse(payload.task_context);
  } catch {
    return false;
  }
  const authority = context?.canonical_user_authority;
  const recent = payload?.recent_committed_response;
  const request = recent?.tool_request;
  const permission = payload?.permission_request;
  return exactObjectKeys(payload, [
    "action_evidence",
    "permission_request",
    "recent_committed_response",
    "task_context",
    "trusted_world_state",
  ])
    && exactObjectKeys(context, ["authority_session_id", "canonical_user_authority"])
    && typeof context.authority_session_id === "string"
    && context.authority_session_id.length > 0
    && Array.isArray(authority)
    && authority.length === 1
    && exactObjectKeys(authority[0], ["history_item_id", "kind", "text"])
    && authority[0].kind === "user_turn"
    && authority[0].text === PERMISSION_TEMP_ESCALATION_LIVE_PROMPT
    && exactObjectKeys(recent, [
      "assistant_text",
      "prior_committed_tool_results",
      "response_id",
      "tool_request",
    ])
    && Array.isArray(recent.prior_committed_tool_results)
    && recent.prior_committed_tool_results.length === 0
    && exactObjectKeys(request, ["arguments_json", "call_id", "tool_name"])
    && request.call_id === elevatedCall.callId
    && request.tool_name === "shell"
    && request.arguments_json === elevatedCall.argumentsJson
    && exactObjectKeys(permission, [
      "access",
      "details",
      "outside_workspace",
      "risks",
      "summary",
      "targets",
    ])
    && permission.access === "shell"
    && Array.isArray(permission.details)
    && permission.details.includes(
      `Requested sandbox elevation: ${PERMISSION_TEMP_ESCALATION_JUSTIFICATION}`,
    )
    && permission.outside_workspace === true
    && Array.isArray(permission.risks)
    && permission.risks.length === 0
    && exactObjectKeys(payload.action_evidence, ["kind"])
    && payload.action_evidence.kind === "permission_request";
}

export function permissionTempEscalationLiveCaptureContract(captures, options) {
  const failures = [];
  if (!Array.isArray(captures) || captures.length !== 4) {
    return { roles: [], failures: ["live-request-count-mismatch"] };
  }
  for (const capture of captures) {
    const body = capture.body;
    if (body?.model !== options.model
      || typeof body?.instructions !== "string"
      || body.instructions.trim().length === 0
      || body.store !== false
      || body.stream !== true
      || LIVE_FORBIDDEN_WIRE_KEYS.some((key) => Object.hasOwn(body ?? {}, key))) {
      failures.push("live-request-common-contract-mismatch");
    }
  }
  const [initial, escalation, guardian, continuation] = captures.map((capture) => capture.body);
  const initialInput = Array.isArray(initial?.input) ? initial.input : [];
  const escalationInput = Array.isArray(escalation?.input) ? escalation.input : [];
  const continuationInput = Array.isArray(continuation?.input) ? continuation.input : [];
  if (!exactObjectKeys(initial, LIVE_TASK_KEYS)
    || initial.tool_choice !== "auto"
    || initial.parallel_tool_calls !== false
    || !Array.isArray(initial.tools)
    || initial.tools.filter((tool) => tool?.name === "shell").length !== 1
    || initialInput.length !== 1
    || responsesInputText(initialInput[0]) !== PERMISSION_TEMP_ESCALATION_LIVE_PROMPT) {
    failures.push("live-initial-request-mismatch");
  }
  const restrictedCall = liveShellArguments(escalationInput[1]);
  const restrictedOutput = restrictedCall === null
    ? null
    : liveToolOutput(escalationInput[2], restrictedCall.callId);
  if (!exactObjectKeys(escalation, LIVE_TASK_KEYS)
    || escalationInput.length !== 3
    || responsesInputText(escalationInput[0]) !== PERMISSION_TEMP_ESCALATION_LIVE_PROMPT
    || !exactLiveRestrictedArguments(restrictedCall)
    || !liveRestrictedOutputPass(restrictedOutput)) {
    failures.push("live-restricted-projection-mismatch");
  }
  const elevatedCall = liveShellArguments(continuationInput[3]);
  const elevatedOutput = elevatedCall === null
    ? null
    : liveToolOutput(continuationInput[4], elevatedCall.callId);
  const replayedRestrictedCall = liveShellArguments(
    continuationInput[1],
    restrictedCall?.callId ?? "",
  );
  const replayedRestrictedOutput = restrictedCall === null
    ? null
    : liveToolOutput(continuationInput[2], restrictedCall.callId);
  if (!exactObjectKeys(continuation, LIVE_TASK_KEYS)
    || continuationInput.length !== 5
    || responsesInputText(continuationInput[0]) !== PERMISSION_TEMP_ESCALATION_LIVE_PROMPT
    || !exactLiveRestrictedArguments(replayedRestrictedCall)
    || replayedRestrictedCall?.argumentsJson !== restrictedCall?.argumentsJson
    || !liveRestrictedOutputPass(replayedRestrictedOutput)
    || replayedRestrictedOutput !== restrictedOutput
    || !exactLiveElevatedArguments(elevatedCall)
    || elevatedCall?.callId === restrictedCall?.callId
    || !liveElevatedOutputPass(elevatedOutput)) {
    failures.push("live-elevated-continuation-mismatch");
  }
  if (!exactObjectKeys(guardian, LIVE_GUARDIAN_KEYS)
    || typeof guardian.instructions !== "string"
    || !guardian.instructions.includes("independent permission guardian")
    || elevatedCall === null
    || !liveGuardianPayloadPass(guardian, elevatedCall)) {
    failures.push("live-guardian-request-mismatch");
  }
  return {
    roles: ["live_initial", "live_escalation", "live_guardian", "live_continuation"],
    failures: [...new Set(failures)],
    restricted_output_sha256: restrictedOutput === null
      ? null
      : sha256(Buffer.from(restrictedOutput, "utf8")),
    elevated_output_sha256: elevatedOutput === null
      ? null
      : sha256(Buffer.from(elevatedOutput, "utf8")),
  };
}

export function parsePermissionTempEscalationCaptureJson(bytes, { file, kind }) {
  try {
    return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  } catch (error) {
    throw productFailure(
      "permission-temp-live-request-capture-json",
      kind === "metadata"
        ? "one prepared request capture metadata file is not valid UTF-8 JSON"
        : "one prepared request capture body is not valid UTF-8 JSON",
      { file, kind, cause: errorObservation(error) },
    );
  }
}

async function readPermissionTempEscalationCaptureBytes(directory, file, kind) {
  try {
    return await readFile(path.join(directory, file));
  } catch (error) {
    throw harnessFailure(
      "permission-temp-live-request-capture-read",
      "one prepared request capture file could not be read",
      { file, kind, cause: errorObservation(error) },
    );
  }
}

async function readLiveRequestCaptures(directory, options) {
  let entries;
  try { entries = await readdir(directory, { withFileTypes: true }); }
  catch (error) {
    throw productFailure(
      "permission-temp-live-request-capture-missing",
      "the live LM Studio run did not create its prepared request capture directory",
      { directory, cause: errorObservation(error) },
    );
  }
  const invalid = entries.filter((entry) => !entry.isFile()
    || (!entry.name.endsWith(".metadata.json") && !entry.name.endsWith(".request.json")));
  if (invalid.length > 0) throw productFailure(
    "permission-temp-live-request-capture-entry",
    "the prepared request capture directory contains an unexpected entry",
    { entries: invalid.map((entry) => entry.name).sort() },
  );
  const metadataFiles = entries
    .map((entry) => entry.name)
    .filter((name) => name.endsWith(".metadata.json"))
    .sort();
  const requestFiles = new Set(entries
    .map((entry) => entry.name)
    .filter((name) => name.endsWith(".request.json")));
  const captures = [];
  for (const metadataFile of metadataFiles) {
    const metadata = parsePermissionTempEscalationCaptureJson(
      await readPermissionTempEscalationCaptureBytes(directory, metadataFile, "metadata"),
      { file: metadataFile, kind: "metadata" },
    );
    if (!exactObjectKeys(metadata, [
      "api_mode",
      "capture_stage",
      "captured_at_unix_ms",
      "endpoint_path",
      "process_id",
      "request_body_bytes",
      "request_body_file",
      "request_id",
      "schema_version",
      "sequence",
      "transport",
    ])
      || metadata.schema_version !== 2
      || metadata.transport !== "http"
      || metadata.capture_stage !== "prepared"
      || metadata.api_mode !== "responses"
      || metadata.endpoint_path !== "v1/responses"
      || !Number.isInteger(metadata.captured_at_unix_ms)
      || !Number.isInteger(metadata.process_id)
      || !Number.isInteger(metadata.sequence)
      || typeof metadata.request_id !== "string"
      || metadata.request_id.length === 0) throw productFailure(
      "permission-temp-live-request-capture-metadata",
      "one prepared request capture did not use the exact Responses metadata contract",
      { metadata_file: metadataFile },
    );
    const requestFile = metadata?.request_body_file;
    const expected = `${metadataFile.slice(0, -".metadata.json".length)}.request.json`;
    if (requestFile !== expected || !requestFiles.delete(requestFile)) throw productFailure(
      "permission-temp-live-request-capture-pair",
      "one prepared request capture metadata file did not own its exact request body",
      { metadata_file: metadataFile, request_file: requestFile ?? null, expected },
    );
    const requestBytes = await readPermissionTempEscalationCaptureBytes(
      directory,
      requestFile,
      "request",
    );
    if (metadata.request_body_bytes !== requestBytes.byteLength) throw productFailure(
      "permission-temp-live-request-capture-size",
      "one prepared request capture byte owner did not match its metadata",
      { metadata_file: metadataFile, metadata_bytes: metadata.request_body_bytes, actual_bytes: requestBytes.byteLength },
    );
    const body = parsePermissionTempEscalationCaptureJson(
      requestBytes,
      { file: requestFile, kind: "request" },
    );
    captures.push({ metadata, body, requestBodySha256: sha256(requestBytes) });
  }
  if (requestFiles.size > 0) throw productFailure(
    "permission-temp-live-request-capture-orphan",
    "the prepared request capture directory contains an orphan request body",
    { orphan_files: [...requestFiles].sort() },
  );
  const classified = permissionTempEscalationLiveCaptureContract(captures, options);
  const metadataSequencePass = captures.length === 4
    && captures.every((capture, index) => index === 0
      || (capture.metadata.process_id === captures[0].metadata.process_id
        && capture.metadata.sequence === captures[index - 1].metadata.sequence + 1
        && capture.metadata.captured_at_unix_ms
          >= captures[index - 1].metadata.captured_at_unix_ms));
  if (!metadataSequencePass) classified.failures.push("live-request-metadata-sequence-mismatch");
  const guardianStartedAt = captures[2]?.metadata?.captured_at_unix_ms;
  const continuationStartedAt = captures[3]?.metadata?.captured_at_unix_ms;
  const guardianToContinuationMs = Number.isInteger(guardianStartedAt)
    && Number.isInteger(continuationStartedAt)
    && continuationStartedAt >= guardianStartedAt
    ? continuationStartedAt - guardianStartedAt
    : null;
  const evidence = {
    schema_version: "desktop-e2e.permission-temp-live-captures.v1",
    capture_count: captures.length,
    roles: classified.roles,
    failures: [...new Set(classified.failures)],
    restricted_output_sha256: classified.restricted_output_sha256,
    elevated_output_sha256: classified.elevated_output_sha256,
    guardian_to_continuation_ms: guardianToContinuationMs,
    request_timeout_ms: LIVE_REQUEST_TIMEOUT_MS,
    request_sequences: captures.map((capture) => capture.metadata?.sequence ?? null),
    request_body_hashes: captures.map((capture) => capture.requestBodySha256),
  };
  if (evidence.failures.length > 0 || guardianToContinuationMs === null) throw productFailure(
    "permission-temp-live-request-contract",
    "the live LM Studio request chain did not match restricted failure, exact elevation, Guardian, and success",
    evidence,
  );
  return evidence;
}

export function permissionTempEscalationLiveFinalFailures(observation) {
  const failures = [];
  if (!Array.isArray(observation?.workspace_failures)
    || observation.workspace_failures.length > 0) {
    failures.push("live-final-workspace-drift");
  }
  if (observation?.terminal_workspace_matches !== true) {
    failures.push("live-final-terminal-workspace-mismatch");
  }
  if (observation?.prepared_request_capture_failure !== null) {
    failures.push("live-final-request-capture-read-failed");
  }
  if (observation?.prepared_request_capture_matches !== true) {
    failures.push("live-final-request-capture-drift");
  }
  return failures;
}

export function permissionTempEscalationLiveCleanupFailures({
  quiesceInput,
  quiesceFinalObservation,
  cleanupFinalObservation,
}) {
  const failures = [];
  if (quiesceInput !== "pass") failures.push("live-cleanup-quiesce-failed");
  if (quiesceFinalObservation === null
    || cleanupFinalObservation === null
    || !isDeepStrictEqual(cleanupFinalObservation, quiesceFinalObservation)) {
    failures.push("live-cleanup-final-observation-drift");
  }
  return failures;
}

export function permissionTempEscalationCaptureFailureObservation(error) {
  if (!(error instanceof DesktopE2eError) || error.owner !== "product") throw error;
  return errorObservation(error);
}

async function observePermissionTempEscalationLiveFinal(context, state, options) {
  const workspace = await workspaceSnapshot(context.paths.workspace);
  const workspaceFailures = permissionTempEscalationWorkspaceFailures(
    workspace,
    state.workspaceBaseline,
  );
  let requestCapture = null;
  let requestCaptureFailure = null;
  try {
    requestCapture = await readLiveRequestCaptures(state.requestCaptureDirectory, options);
  } catch (error) {
    requestCaptureFailure = permissionTempEscalationCaptureFailureObservation(error);
  }
  const observation = {
    workspace,
    workspace_failures: workspaceFailures,
    terminal_workspace_matches: state.terminalWorkspace !== null
      && isDeepStrictEqual(workspace, state.terminalWorkspace),
    prepared_request_capture: requestCapture,
    prepared_request_capture_failure: requestCaptureFailure,
    prepared_request_capture_matches: state.requestCaptureEvidence !== null
      && requestCapture !== null
      && isDeepStrictEqual(requestCapture, state.requestCaptureEvidence),
  };
  return {
    ...observation,
    failures: permissionTempEscalationLiveFinalFailures(observation),
  };
}

export function permissionTempEscalationLiveTerminalFailures(surface) {
  const projection = surface?.projection;
  const users = rowsOfKind(projection, "user");
  const assistants = rowsOfKind(projection, "assistant");
  const errors = rowsOfKind(projection, "error");
  const running = rowsOfKind(projection, "work_summary_running");
  const completed = rowsOfKind(projection, "work_summary_completed");
  const failures = [];
  if (!terminalSettled(surface)) failures.push("live-terminal-surface-not-settled");
  if (!isDeepStrictEqual(users.map((row) => row.body), [PERMISSION_TEMP_ESCALATION_LIVE_PROMPT])) {
    failures.push("live-terminal-user-authority-mismatch");
  }
  if (!isDeepStrictEqual(assistants.map((row) => row.body), [PERMISSION_TEMP_ESCALATION_LIVE_RESPONSE])) {
    failures.push("live-terminal-assistant-response-mismatch");
  }
  if (errors.length !== 0 || running.length !== 0 || completed.length !== 1) {
    failures.push("live-terminal-history-cardinality-mismatch");
  }
  if (completed.length === 1 && !exactTwoShellLifecycleHistory(projection, completed[0])) {
    failures.push("live-terminal-tool-history-mismatch");
  }
  if (surface?.users?.length !== 1
    || surface.users[0].visible !== true
    // observeProviderTurnSurface reads innerText.trim(): trim only removes the
    // edges, while innerText reflects CSS whitespace collapse inside the row.
    // Keep raw canonical authority exact above and normalize only this DOM view.
    || !renderedDomTextMatchesAuthority(
      surface.users[0].text,
      PERMISSION_TEMP_ESCALATION_LIVE_PROMPT,
    )
    || surface?.assistants?.length !== 1
    || surface.assistants[0].visible !== true
    || surface.assistants[0].text !== PERMISSION_TEMP_ESCALATION_LIVE_RESPONSE
    || surface?.completed_summaries?.length !== 1
    || surface.completed_summaries[0].visible !== true) failures.push("live-terminal-dom-mismatch");
  return [...new Set(failures)];
}

export function createPermissionTempEscalationLiveTerminalDecision({
  confirmationMs = PERMISSION_TEMP_ESCALATION_TERMINAL_CONFIRMATION_MS,
  now = () => Date.now(),
} = {}) {
  if (!Number.isSafeInteger(confirmationMs) || confirmationMs <= 0) {
    throw new TypeError("live permission TEMP terminal confirmation must be a positive safe integer");
  }
  if (typeof now !== "function") {
    throw new TypeError("live permission TEMP terminal clock must be a function");
  }
  return createConfirmedTerminalDecision({
    failuresOf: permissionTempEscalationLiveTerminalFailures,
    surfaceOf: (surface) => surface,
    providerFailed: (surface) => surface?.visible_fatal_count > 0
      || surface?.visible_recoverable_error_count > 0
      || surface?.visible_dialog_count > 0
      || surface?.visible_modal_backdrop_count > 0
      || surface?.projection?.startup?.status === "failed"
      || ["failed", "cancelled", "incomplete"].includes(
        surface?.projection?.run_status_key,
      ),
    domOnlyFailures: new Set(["live-terminal-dom-mismatch"]),
    confirmationMs,
    now,
  });
}

export function permissionTempEscalationLiveTerminalFailureEvidence(surface) {
  return {
    surface,
    terminal_oracle_failures: permissionTempEscalationLiveTerminalFailures(surface),
  };
}

function sha256(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

async function workspaceSnapshot(workspace) {
  const entries = (await readdir(workspace, { withFileTypes: true }))
    .sort((left, right) => left.name.localeCompare(right.name, "en"));
  const rows = [];
  for (const entry of entries) {
    const kind = entry.isFile() ? "file" : entry.isDirectory() ? "directory" : "other";
    const bytes = entry.isFile() ? await readFile(`${workspace}\\${entry.name}`) : null;
    rows.push({
      name: entry.name,
      kind,
      size_bytes: bytes?.byteLength ?? null,
      sha256: bytes === null ? null : sha256(bytes),
    });
  }
  return { schema_version: "desktop-e2e.workspace-snapshot.v1", entries: rows };
}

export function permissionTempEscalationWorkspaceFailures(snapshot, baseline) {
  const failures = [];
  const names = snapshot?.entries?.map((entry) => entry.name) ?? [];
  if (!isDeepStrictEqual(names, [FIXTURE_SENTINEL, PYTEST_FIXTURE].sort())) {
    failures.push("workspace-top-level-drift");
  }
  if (snapshot?.entries?.some((entry) => entry.kind !== "file")) {
    failures.push("workspace-temp-workaround-present");
  }
  if (!isDeepStrictEqual(snapshot, baseline)) failures.push("workspace-fixture-mutated");
  return [...new Set(failures)];
}

function keyCode(character) {
  if (/^[a-z]$/u.test(character)) return `Key${character.toUpperCase()}`;
  if (character === " ") return "Space";
  throw new TypeError(`unsupported permission TEMP escalation character: ${character}`);
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
    sequence: snapshot.sequence,
  };
}

async function trustedType(input, text) {
  await trustedClick(input, PROMPT);
  const start = (await input.snapshotProbe()).sequence;
  await input.typeText(text);
  return assertTrustedProbeSequence(await input.snapshotProbe(start), {
    afterSequence: start,
    expected: expectedTypedEvents(text),
  });
}

async function trustedInsert(input, text) {
  await trustedClick(input, PROMPT);
  const start = (await input.snapshotProbe()).sequence;
  const insertion = await input.insertText(PROMPT, text);
  const probe = assertTrustedTextInsertion(await input.snapshotProbe(start), {
    afterSequence: start,
    identity: PROMPT.identity,
    text,
  });
  return { insertion, probe };
}

async function waitForReadyComposer(cdp, prompt = "") {
  try {
    return (await waitForObservation({
      label: "permission TEMP escalation ready composer",
      timeoutMs: 15_000,
      pollMs: 50,
      sample: () => observeProviderTurnSurface(cdp),
      accept: (surface) => surface?.projection?.can_submit === true
        && surface?.projection?.composer_submit_mode === "new_request"
        && surface?.prompt?.count === 1
        && surface.prompt.visible === true
        && surface.prompt.enabled === true
        && surface.prompt.value === prompt
        && surface?.send?.count === 1
        && surface.send.visible === true
        && surface.send.enabled === (prompt.length > 0)
        && surface?.visible_fatal_count === 0
        && surface?.visible_recoverable_error_count === 0,
      retrySampleErrors: false,
    })).value;
  } catch (error) {
    throw classifyAcquiredObservationFailure(error, {
      code: "permission-temp-composer-not-ready",
      message: "the acquired Desktop composer did not expose the exact enabled Send state",
    });
  }
}

async function submitTrustedPrompt({
  cdp,
  input,
  commands,
  prompt = PERMISSION_TEMP_ESCALATION_PROMPT,
  insertion = false,
}) {
  await waitForReadyComposer(cdp);
  const typed = insertion ? await trustedInsert(input, prompt) : await trustedType(input, prompt);
  const ready = await waitForReadyComposer(cdp, prompt);
  const expected = {
    command: "submit_prompt",
    args: {
      text: prompt,
      expectedTarget: ready.projection.draft_target,
      expectedRunTarget: ready.projection.run_target,
    },
  };
  const commandStart = (await commands.snapshot()).sequence;
  const click = await trustedClick(input, SEND);
  const observed = await waitForObservation({
    label: "permission TEMP escalation submit command",
    timeoutMs: 10_000,
    pollMs: 16,
    sample: () => commands.snapshot(commandStart),
    accept: (snapshot) => snapshot.calls.length >= 1,
    retrySampleErrors: false,
  });
  const command = assertExactDesktopCommandSequence(observed.value, {
    afterSequence: commandStart,
    expected: [expected],
  });
  return { typed, click, command, expected, commandStart };
}

async function waitForProductStage({
  label,
  timeoutMs,
  sample,
  decide,
  code,
  message,
  failureEvidence = (value) => value,
}) {
  let decision = "pending";
  let observed;
  try {
    observed = await waitForObservation({
      label,
      timeoutMs,
      pollMs: 50,
      sample,
      accept: (value) => {
        decision = decide(value);
        return decision !== "pending";
      },
      retrySampleErrors: false,
    });
  } catch (error) {
    if (error?.code === "observation-timeout"
      && error?.evidence !== null
      && typeof error?.evidence === "object"
      && error.evidence.last_value !== null) {
      error.evidence.last_value = failureEvidence(error.evidence.last_value);
    }
    throw classifyAcquiredObservationFailure(error, { code, message });
  }
  if (decision === "fail") {
    throw productFailure(code, message, failureEvidence(observed.value));
  }
  return observed.value;
}

async function requirePermissionTempEscalationPersistence(context, terminal) {
  const owner = exactOwner(terminal?.surface?.projection ?? terminal?.projection, "idle");
  if (owner === null) {
    throw productFailure(
      "permission-temp-persistence-owner",
      "the terminal projection did not retain the canonical session and turn owner for SQLite verification",
      terminal,
    );
  }
  let evidence;
  try {
    evidence = await readPermissionTempEscalationPersistence({
      database: context.paths.database,
      owner,
    });
  } catch (error) {
    throw harnessFailure(
      "permission-temp-persistence-query",
      "the read-only SQLite oracle could not inspect canonical permission TEMP history",
      errorObservation(error),
    );
  }
  const failures = permissionTempEscalationPersistenceFailures(evidence, owner);
  if (failures.length > 0) {
    throw productFailure(
      "permission-temp-persistence-contract",
      "canonical permission TEMP history did not persist the exact restricted and elevated success projections",
      { failures, owner, evidence },
    );
  }
  return { owner, ...evidence, failures };
}

async function bestEffortPermissionTempEscalationPersistence(context, terminal) {
  try {
    return {
      collection: "pass",
      ...await requirePermissionTempEscalationPersistence(context, terminal),
    };
  } catch (error) {
    return {
      collection: "fail",
      failures: ["persistence-best-effort-collection-failed"],
      error: errorObservation(error),
    };
  }
}

async function settleProbes(state, input, commands, primaryError) {
  const outcome = { input: null, commands: null, failures: [] };
  try { outcome.input = await input.cleanup(); }
  catch (error) { outcome.failures.push({ owner: "webview-input", error: errorObservation(error) }); }
  try { outcome.commands = await commands.remove(); }
  catch (error) { outcome.failures.push({ owner: "desktop-command-probe", error: errorObservation(error) }); }
  state.probeOutcomes.push(outcome);
  if (outcome.failures.length > 0 && primaryError === null) {
    throw harnessFailure(
      "permission-temp-probe-cleanup",
      "permission TEMP escalation input or command probe did not settle",
      outcome,
    );
  }
}

export function createPermissionTempEscalationScenario() {
  const state = {
    provider: null,
    acceptedLedger: null,
    quiesceOutcome: null,
    probeOutcomes: [],
    workspaceBaseline: null,
    persistenceEvidence: null,
  };
  return Object.freeze({
    id: "permission.temp-escalation",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    requestGracefulExit,
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({
        script: createPermissionTempEscalationProviderScript({
          taskPrompt: PERMISSION_TEMP_ESCALATION_PROMPT,
          command: PERMISSION_TEMP_ESCALATION_COMMAND,
          justification: PERMISSION_TEMP_ESCALATION_JUSTIFICATION,
          responseText: PERMISSION_TEMP_ESCALATION_RESPONSE,
          guardianDelayMs: PERMISSION_TEMP_ESCALATION_GUARDIAN_DELAY_MS,
        }),
        responseBehavior: "hold_until_release",
      });
      await prepareDesktopFixture({
        context,
        sink,
        phase,
        owner: OWNER,
        configText: permissionTempEscalationFixtureConfig(state.provider.baseUrl),
        sentinelName: FIXTURE_SENTINEL,
        sentinelText: "moyAI Desktop E2E permission TEMP escalation fixture.\n",
      });
      await writeFile(`${context.paths.workspace}\\${PYTEST_FIXTURE}`, PYTEST_FIXTURE_TEXT, {
        flag: "wx",
      });
      state.workspaceBaseline = await workspaceSnapshot(context.paths.workspace);
      await sink.record("permission-temp-provider-started", {
        provider: state.provider.resourceObservation(),
        workspace: state.workspaceBaseline,
      }, { phase, owner: OWNER });
    },
    async execute({ context, driver: cdp, sink }) {
      const provider = state.provider;
      if (provider === null || state.workspaceBaseline === null) {
        throw new Error("permission TEMP escalation fixture was not prepared");
      }
      await acquireInteractiveShell({ context, driver: cdp, sink }, {
        evidenceOwner: OWNER,
        screenshotStem: "permission-temp-shell-ready",
      });
      const input = new WebviewInput(cdp, { probeId: "permission-temp-escalation" });
      const commands = new DesktopCommandProbe(cdp, {
        probeId: "permission-temp-escalation-commands",
        commands: ["submit_prompt", "cancel_run"],
      });
      let primaryError = null;
      let probesSettled = false;
      try {
        await input.installProbe();
        await commands.install();
        const submit = await submitTrustedPrompt({ cdp, input, commands });
        const held = await waitForProductStage({
          label: "permission TEMP exact restricted failure projection",
          timeoutMs: 30_000,
          sample: async () => ({
            surface: await observeProviderTurnSurface(cdp),
            ledger: provider.requestLedger,
          }),
          decide: permissionTempEscalationHeldDecision,
          code: "permission-temp-restricted-projection",
          message: "the restricted pytest failure did not reach the exact host hint and held escalation barrier",
        });
        const release = provider.releaseScriptRole("temp_escalation");
        const restrictedWorkspace = await workspaceSnapshot(context.paths.workspace);
        const restrictedWorkspaceFailures = permissionTempEscalationWorkspaceFailures(
          restrictedWorkspace,
          state.workspaceBaseline,
        );
        if (restrictedWorkspaceFailures.length > 0) throw productFailure(
          "permission-temp-restricted-workspace-drift",
          "the restricted pytest run changed the fixture or created a workspace TEMP workaround",
          { failures: restrictedWorkspaceFailures, baseline: state.workspaceBaseline, observed: restrictedWorkspace },
        );
        const heldScreenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "permission-temp-escalation-released",
          owner: OWNER,
        });
        const terminalDecision = createPermissionTempEscalationTerminalDecision();
        let terminal;
        try {
          terminal = await waitForProductStage({
            label: "permission TEMP elevated pytest terminal",
            timeoutMs: PERMISSION_TEMP_ESCALATION_TERMINAL_TIMEOUT_MS,
            sample: async () => ({
              surface: await observeProviderTurnSurface(cdp),
              ledger: provider.requestLedger,
            }),
            decide: terminalDecision,
            code: "permission-temp-terminal",
            message: "the exact elevated pytest retry did not reach one canonical GUI terminal",
            failureEvidence: permissionTempEscalationTerminalFailureEvidence,
          });
        } catch (error) {
          if (error?.code === "permission-temp-terminal" && state.persistenceEvidence === null) {
            const terminalEvidence = error?.evidence?.surface !== undefined
              ? error.evidence
              : error?.evidence?.observation?.last_value;
            state.persistenceEvidence = await bestEffortPermissionTempEscalationPersistence(
              context,
              terminalEvidence,
            );
          }
          throw error;
        }
        const terminalScreenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "permission-temp-terminal",
          owner: OWNER,
        });
        state.persistenceEvidence = await requirePermissionTempEscalationPersistence(
          context,
          terminal,
        );
        const finalWorkspace = await workspaceSnapshot(context.paths.workspace);
        const finalWorkspaceFailures = permissionTempEscalationWorkspaceFailures(
          finalWorkspace,
          state.workspaceBaseline,
        );
        if (finalWorkspaceFailures.length > 0) throw productFailure(
          "permission-temp-final-workspace-drift",
          "the elevated pytest retry changed the fixture or created a workspace TEMP workaround",
          { failures: finalWorkspaceFailures, baseline: state.workspaceBaseline, observed: finalWorkspace },
        );
        const commandLifetime = assertExactDesktopCommandSequence(
          await commands.snapshot(submit.commandStart),
          { afterSequence: submit.commandStart, expected: [submit.expected] },
        );
        state.acceptedLedger = structuredClone(provider.requestLedger);
        await sink.record("permission-temp-escalation-completed", {
          submit,
          command_lifetime: commandLifetime,
          held,
          provider_release: release,
          terminal,
          canonical_persistence: state.persistenceEvidence,
          workspace: finalWorkspace,
          provider_ledger: state.acceptedLedger,
          screenshots: { held: heldScreenshot, terminal: terminalScreenshot },
        }, { phase: "executing", owner: OWNER });
        probesSettled = true;
        await settleProbes(state, input, commands, null);
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        if (!probesSettled) {
          probesSettled = true;
          await settleProbes(state, input, commands, primaryError);
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
        && state.probeOutcomes.every((outcome) => outcome.failures.length === 0);
      return {
        input: pass ? "pass" : "fail",
        resources: [{
          kind: "permission-temp-escalation-verification",
          quiesce_input: state.quiesceOutcome?.input ?? null,
          canonical_persistence: state.persistenceEvidence,
          probe_outcomes: state.probeOutcomes,
        }],
      };
    },
  });
}

export function createPermissionTempEscalationLmStudioScenario(rawOptions = {}) {
  const options = normalizePermissionTempEscalationLmStudioOptions(rawOptions);
  const state = {
    requestCaptureDirectory: null,
    requestCaptureEvidence: null,
    workspaceBaseline: null,
    terminalWorkspace: null,
    persistenceEvidence: null,
    probeOutcomes: [],
    quiesceOutcome: null,
    quiesceFinalObservation: null,
  };
  return Object.freeze({
    id: "manual.permission-temp-escalation-lm-studio",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    requestGracefulExit,
    get environment() {
      return state.requestCaptureDirectory === null
        ? {}
        : { MOYAI_HTTP_REQUEST_CAPTURE_DIR: state.requestCaptureDirectory };
    },
    async prepare({ context, sink, phase }) {
      state.requestCaptureDirectory = path.join(context.root, LIVE_CAPTURE_DIRECTORY);
      await prepareDesktopFixture({
        context,
        sink,
        phase,
        owner: OWNER,
        configText: permissionTempEscalationLmStudioFixtureConfig(options),
        sentinelName: FIXTURE_SENTINEL,
        sentinelText: "moyAI Desktop E2E live LM Studio permission TEMP escalation fixture.\n",
      });
      await writeFile(path.join(context.paths.workspace, PYTEST_FIXTURE), PYTEST_FIXTURE_TEXT, {
        flag: "wx",
      });
      state.workspaceBaseline = await workspaceSnapshot(context.paths.workspace);
      await sink.record("permission-temp-live-input", {
        provider_base_url: options.providerBaseUrl,
        model: options.model,
        provider_profile: "lm_studio",
        prompt_sha256: sha256(Buffer.from(PERMISSION_TEMP_ESCALATION_LIVE_PROMPT, "utf8")),
        command: PERMISSION_TEMP_ESCALATION_COMMAND,
        request_timeout_ms: LIVE_REQUEST_TIMEOUT_MS,
        turn_timeout_ms: LIVE_TURN_TIMEOUT_MS,
        prepared_request_capture_directory: state.requestCaptureDirectory,
        external_provider_owned_by_scenario: false,
        external_model_lifecycle: "already-loaded-unmanaged",
      }, { phase, owner: OWNER });
    },
    async execute({ context, driver: cdp, sink }) {
      if (state.workspaceBaseline === null || state.requestCaptureDirectory === null) {
        throw new Error("live LM Studio permission TEMP escalation fixture was not prepared");
      }
      await acquireInteractiveShell({ context, driver: cdp, sink }, {
        evidenceOwner: OWNER,
        screenshotStem: "permission-temp-live-shell-ready",
      });
      const input = new WebviewInput(cdp, { probeId: "permission-temp-live" });
      const commands = new DesktopCommandProbe(cdp, {
        probeId: "permission-temp-live-commands",
        commands: ["submit_prompt", "cancel_run"],
      });
      let primaryError = null;
      let probesSettled = false;
      try {
        await input.installProbe();
        await commands.install();
        const submit = await submitTrustedPrompt({
          cdp,
          input,
          commands,
          prompt: PERMISSION_TEMP_ESCALATION_LIVE_PROMPT,
          insertion: true,
        });
        const terminalDecision = createPermissionTempEscalationLiveTerminalDecision();
        let terminal;
        try {
          terminal = await waitForProductStage({
            label: "live LM Studio permission TEMP escalation terminal",
            timeoutMs: LIVE_TURN_TIMEOUT_MS,
            sample: () => observeProviderTurnSurface(cdp),
            decide: terminalDecision,
            code: "permission-temp-live-terminal",
            message: "the live LM Studio turn did not complete the exact restricted failure, Guardian allow, and elevated retry",
            failureEvidence: permissionTempEscalationLiveTerminalFailureEvidence,
          });
        } catch (error) {
          if (error?.code === "permission-temp-live-terminal"
            && state.persistenceEvidence === null) {
            const terminalEvidence = error?.evidence?.surface ?? error?.evidence
              ?.observation?.last_value?.surface ?? error?.evidence?.observation?.last_value;
            state.persistenceEvidence = await bestEffortPermissionTempEscalationPersistence(
              context,
              terminalEvidence,
            );
          }
          throw error;
        }
        const screenshot = await captureScenarioScreenshot({
          cdp,
          sink,
          name: "permission-temp-live-terminal",
          owner: OWNER,
        });
        state.persistenceEvidence = await requirePermissionTempEscalationPersistence(
          context,
          terminal,
        );
        state.requestCaptureEvidence = await readLiveRequestCaptures(
          state.requestCaptureDirectory,
          options,
        );
        state.terminalWorkspace = await workspaceSnapshot(context.paths.workspace);
        const workspaceFailures = permissionTempEscalationWorkspaceFailures(
          state.terminalWorkspace,
          state.workspaceBaseline,
        );
        if (workspaceFailures.length > 0) throw productFailure(
          "permission-temp-live-workspace-drift",
          "the live LM Studio permission flow changed the fixture or created a workspace TEMP workaround",
          { failures: workspaceFailures, baseline: state.workspaceBaseline, observed: state.terminalWorkspace },
        );
        const commandLifetime = assertExactDesktopCommandSequence(
          await commands.snapshot(submit.commandStart),
          { afterSequence: submit.commandStart, expected: [submit.expected] },
        );
        await sink.record("permission-temp-live-completed", {
          submit,
          command_lifetime: commandLifetime,
          terminal,
          canonical_persistence: state.persistenceEvidence,
          workspace: state.terminalWorkspace,
          prepared_request_capture: state.requestCaptureEvidence,
          screenshot,
          provider_resource: {
            kind: "external-lm-studio-provider",
            owned_by_scenario: false,
            lifecycle: "already-loaded-unmanaged",
            cleanup_action: "none",
          },
        }, { phase: "executing", owner: OWNER });
        probesSettled = true;
        await settleProbes(state, input, commands, null);
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        if (!probesSettled) {
          probesSettled = true;
          await settleProbes(state, input, commands, primaryError);
        }
      }
    },
    async quiesce({ context, inputs }) {
      if (state.quiesceOutcome !== null) return structuredClone(state.quiesceOutcome);
      const resourcesPass = state.probeOutcomes.every((outcome) => outcome.failures.length === 0);
      state.quiesceFinalObservation = await observePermissionTempEscalationLiveFinal(
        context,
        state,
        options,
      );
      const postExecutionFailure = inputs.acquisition === "pass"
        && inputs.oracle !== "fail"
        && state.quiesceFinalObservation.failures.length > 0
        ? {
          code: "permission-temp-live-post-execution-drift",
          message: "the live LM Studio permission flow changed after its terminal evidence was accepted",
          evidence: state.quiesceFinalObservation,
        }
        : null;
      state.quiesceOutcome = {
        input: resourcesPass ? "pass" : "fail",
        resources: [{
          kind: "external-lm-studio-provider",
          provider_base_url: options.providerBaseUrl,
          model: options.model,
          owned_by_scenario: false,
          lifecycle: "already-loaded-unmanaged",
          cleanup_action: "none",
          probe_outcomes: structuredClone(state.probeOutcomes),
          final_observation: state.quiesceFinalObservation,
        }],
        productFailure: postExecutionFailure,
      };
      return structuredClone(state.quiesceOutcome);
    },
    async cleanup({ context }) {
      const finalObservation = await observePermissionTempEscalationLiveFinal(
        context,
        state,
        options,
      );
      const cleanupFailures = permissionTempEscalationLiveCleanupFailures({
        quiesceInput: state.quiesceOutcome?.input ?? null,
        quiesceFinalObservation: state.quiesceFinalObservation,
        cleanupFinalObservation: finalObservation,
      });
      return {
        input: cleanupFailures.length === 0 ? "pass" : "fail",
        resources: [{
          kind: "permission-temp-live-verification",
          prepared_request_capture: state.requestCaptureEvidence,
          canonical_persistence: state.persistenceEvidence,
          quiesce_final_observation: state.quiesceFinalObservation,
          cleanup_final_observation: finalObservation,
          cleanup_failures: cleanupFailures,
          quiesce_input: state.quiesceOutcome?.input ?? null,
        }],
      };
    },
  });
}

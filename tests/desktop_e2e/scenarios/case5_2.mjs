import crypto from "node:crypto";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { isDeepStrictEqual } from "node:util";
import {
  lstat,
  mkdir,
  open,
  readFile,
  readdir,
  realpath,
  stat,
  writeFile,
} from "node:fs/promises";

import {
  case52EvaluatorFailures,
  case52RestartPreviousPageTransitionFailures,
  case52Stage1ManifestFailures,
  case52Stage2ManifestFailures,
  classifyCase52NonConvergence,
  classifyCase52NormalTerminal,
  classifyCase52RestartContinuity,
  classifyCase52RestartTurnPage,
} from "../case5_2_predicates.mjs";
import { inventoryCase52CleanSeed, copyCase52CleanSeed } from "../core/clean_seed.mjs";
import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { waitForSemanticTargetSettlement } from "../core/semantic_target_settlement.mjs";
import {
  DesktopCommandProbe,
  assertExactDesktopCommandSequence,
} from "../drivers/desktop_command_probe.mjs";
import {
  WebviewInput,
  assertTrustedProbeSequence,
  assertTrustedTextInsertion,
} from "../drivers/webview_input.mjs";
import {
  WindowsExternalProcessError,
  runWindowsExternalProcess,
} from "../drivers/windows_external_process.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import {
  captureScenarioScreenshot,
  invokeDesktopCommand,
  selectedNavigationIdentity,
} from "./observations.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import { executeCase52SideChatStage } from "./case5_2_side_chat.mjs";

const OWNER = "scenario:manual.case5_2";
const scenarioDirectory = path.dirname(fileURLToPath(import.meta.url));
const caseDirectory = path.resolve(scenarioDirectory, "..", "..", "manual_ST", "case5_2");
const QUALITY_CONTEXT_WINDOW = 131_072;
const QUALITY_REQUEST_TIMEOUT_MS = 3_600_000;
const STAGE_TIMEOUT_MS = 12 * 60 * 60 * 1000;
const STAGE_POLL_MS = 5_000;
const MANIFEST_POLL_MS = 30_000;
const PROGRESS_EVIDENCE_MS = 5 * 60 * 1000;
const RESTORE_STABILITY_MS = 500;
const MAX_CAPTURE_BYTES = 16 * 1024 * 1024;
const MAX_CAPTURE_SAMPLE_BYTES = 128 * 1024;
const PROVIDER_CLEANUP_TIMEOUT_MS = 120_000;
const PROVIDER_CLEANUP_POLL_MS = 1_000;
const PROVIDER_CLEANUP_STABLE_SAMPLES = 2;
const LM_STUDIO_PROFILE = "lm_studio";
const OPENAI_COMPATIBLE_PROFILE = "openai_compatible";
const EXECUTION_OWNED_LIFECYCLE = "execution-owned";
const EXTERNAL_UNMANAGED_LIFECYCLE = "external-unmanaged";
const GUI_CONNECTION_BASELINE_BASE_URL = "http://127.0.0.1:9";
const GUI_CONNECTION_BASELINE_MODEL = "moyai-case5-2-before-gui-save";
const MAIN_API_KEY_ENV = "";

const PROMPT = Object.freeze({
  selector: "section.composer textarea#prompt",
  identity: { tag: "TEXTAREA", id: "prompt" },
});
const SEND_NEW_REQUEST = Object.freeze({
  selector: 'section.composer button[data-action="send"][title="送信"][aria-label="送信"]',
  identity: { tag: "BUTTON", action: "send" },
});
const EXPORT_TRANSCRIPT = Object.freeze({
  selector: 'header.topbar button[data-action="export-transcript"]',
  identity: { tag: "BUTTON", action: "export-transcript" },
});
const STOP = Object.freeze({
  selector: 'section.run-strip button[data-action="cancel-run"][aria-label="実行停止"]',
  identity: { tag: "BUTTON", action: "cancel-run" },
});
const SHOW_SETTINGS = Object.freeze({
  selector: 'aside.sidebar button.settings[data-action="show-config"][title="設定"]',
  identity: { tag: "BUTTON", action: "show-config" },
});
const SHOW_COMMAND_PALETTE = Object.freeze({
  selector: 'section.composer button[data-action="show-command-palette"]',
  identity: { tag: "BUTTON", action: "show-command-palette" },
});
const COMMAND_PALETTE_SEARCH = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="command-palette-dialog-title"] input#local-search',
  identity: { tag: "INPUT", id: "local-search" },
});
const PREVIOUS_TURN_PAGE = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="command-palette-dialog-title"] button[data-action="load-previous-turn-page"]',
  identity: { tag: "BUTTON", action: "load-previous-turn-page" },
});
const MAIN_BASE_URL = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] input.settings-control[data-config-key="model.base_url"]',
  identity: { tag: "INPUT", configKey: "model.base_url" },
});
const MAIN_PROVIDER_PROFILE = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] select.settings-control[data-config-key="model.provider_profile"]',
  identity: { tag: "SELECT", configKey: "model.provider_profile" },
});
const MAIN_MANUAL_DETAILS = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] details[data-details-key="main-provider-manual-model"] > summary',
  identity: { tag: "DETAILS", detailsKey: "main-provider-manual-model" },
});
const MAIN_MANUAL_MODEL = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] input#main-provider-model-manual[data-config-key="model.model"]',
  identity: { tag: "INPUT", id: "main-provider-model-manual", configKey: "model.model" },
});
const MAIN_API_KEY = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] input.settings-control[data-config-key="model.api_key_env"]',
  identity: { tag: "INPUT", configKey: "model.api_key_env" },
});
const SAVE_GLOBAL_CONFIG = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] button[data-action="save-global-config"]',
  identity: { tag: "BUTTON", action: "save-global-config" },
});
const SIDE_SETTINGS_NAV = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] nav.settings-nav a[href="#settings-side-chat"]',
  identity: { tag: "A", href: "#settings-side-chat" },
});
const SIDE_BASE_URL = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] input#side-chat-base-url[data-config-key="side_chat.base_url"]',
  identity: { tag: "INPUT", id: "side-chat-base-url", configKey: "side_chat.base_url" },
});
const SIDE_PROVIDER_PROFILE = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] select#side-chat-provider-profile[data-config-key="side_chat.provider_profile"]',
  identity: { tag: "SELECT", id: "side-chat-provider-profile", configKey: "side_chat.provider_profile" },
});
const SIDE_MANUAL_DETAILS = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] details[data-details-key="side-chat-manual-model"] > summary',
  identity: { tag: "DETAILS", detailsKey: "side-chat-manual-model" },
});
const SIDE_MANUAL_MODEL = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] input#side-chat-model-manual[data-config-key="side_chat.model"]',
  identity: { tag: "INPUT", id: "side-chat-model-manual", configKey: "side_chat.model" },
});
const SHOW_SIDE_CHAT = Object.freeze({
  selector: 'button[data-action="show-side-chat-pane"]',
  identity: { tag: "BUTTON", action: "show-side-chat-pane" },
});
const REQUEST_DELETE_SIDE_CHAT = Object.freeze({
  selector: 'aside.side-chat-pane[data-pane-mode="side-chat"] button[data-action="request-delete-side-chat"]',
  identity: { tag: "BUTTON", action: "request-delete-side-chat" },
});
const CONFIRM_DELETE_SIDE_CHAT = Object.freeze({
  selector: '[role="alertdialog"][aria-labelledby="side-chat-delete-title"] button[data-action="confirm-delete-side-chat"]',
  identity: { tag: "BUTTON", action: "confirm-delete-side-chat" },
});
const CLOSE_SETTINGS = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] button[data-action="close-overlay"]',
  identity: { tag: "BUTTON", action: "close-overlay" },
});

const STAGES = Object.freeze([
  { id: "stage1", promptFile: "stage1-research.txt", minimumSummaries: 1 },
  { id: "stage2", promptFile: "stage2-design.txt", minimumSummaries: 1 },
  { id: "stage3", promptFile: "stage3-implement.txt", minimumSummaries: 1 },
  { id: "stage4", promptFile: "stage4-regression.txt", minimumSummaries: 1 },
]);
const SIDE_CHAT_STAGE = Object.freeze({ id: "stage5", promptFile: "stage5-side-chat.txt" });
const REQUIRED_DOCUMENTS = Object.freeze([
  "README.md",
  "basic_design.md",
  "detail_design.md",
  "evidence_matrix.md",
  "cancel_contract.md",
]);

function sameValue(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function elapsedSince(startedAt) {
  const started = Date.parse(startedAt);
  return Number.isFinite(started) ? Math.max(0, Date.now() - started) : null;
}

function sha256(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

export function normalizeCase52PromptText(value) {
  if (typeof value !== "string") throw new TypeError("case5_2 prompt text must be a string");
  return value.replaceAll("\r\n", "\n").replaceAll("\r", "\n");
}

function errorObservation(error) {
  return {
    name: error?.name ?? "Error",
    code: error?.code ?? null,
    message: error?.message ?? String(error),
    evidence: error?.evidence ?? null,
  };
}

function productFailure(code, message, evidence) {
  return new DesktopE2eError("product", code, message, evidence);
}

export function classifyCase52MainPreferencesObservationError(error, action) {
  if (typeof action !== "string" || action.length === 0) {
    throw new TypeError("case5_2 Main Preferences observation action is required");
  }
  if (error?.code !== "observation-timeout"
    || error?.evidence?.last_error !== null
    || !Object.hasOwn(error?.evidence ?? {}, "last_value")) return error;
  const evidence = error?.evidence !== null && typeof error?.evidence === "object"
    ? error.evidence
    : {};
  return productFailure(
    "case5_2-main-preferences-observation-timeout",
    `Main Preferences did not settle after trusted ${action}`,
    {
      action,
      label: evidence.label ?? null,
      attempts: evidence.attempts ?? null,
      elapsed_ms: evidence.elapsed_ms ?? null,
      last_value: evidence.last_value ?? null,
      last_error: evidence.last_error ?? null,
    },
  );
}

export function classifyCase52MainSaveCommandError(error) {
  if (!new Set([
    "desktop-command-probe-cardinality",
    "desktop-command-probe-call-mismatch",
  ]).has(error?.code)) return error;
  return productFailure(
    "case5_2-main-provider-save-command",
    "trusted Main Preferences Save invoked an unexpected product command",
    errorObservation(error),
  );
}

export function classifyCase52SideScreenshotObservationError(error, action) {
  if (typeof action !== "string" || action.length === 0) {
    throw new TypeError("case5_2 Side screenshot observation action is required");
  }
  if (error?.code !== "observation-timeout"
    || error?.evidence?.last_error !== null
    || !Object.hasOwn(error?.evidence ?? {}, "last_value")) return error;
  return productFailure(
    "case5_2-side-screenshot-observation-timeout",
    `Side Chat Settings did not become screenshot-ready after trusted ${action}`,
    {
      action,
      label: error.evidence.label ?? null,
      attempts: error.evidence.attempts ?? null,
      elapsed_ms: error.evidence.elapsed_ms ?? null,
      last_value: error.evidence.last_value,
      last_error: null,
    },
  );
}

export function case52ProviderControlTokenLeakFailure(stage, evidence) {
  if (typeof stage !== "string" || stage.length === 0) {
    throw new TypeError("case5_2 provider control-token leak stage is required");
  }
  if (evidence === null || typeof evidence !== "object" || Array.isArray(evidence)) {
    throw new TypeError("case5_2 provider control-token leak evidence is required");
  }
  const message = `${stage} exposed an OpenAI chat-template control token in an assistant transcript body`;
  const observedProductFailure = {
    owner: "product",
    code: "case5_2-provider-control-token-leak",
    message,
    evidence: structuredClone(evidence),
  };
  const acquisitionErrors = [
    evidence.projection_error ?? null,
    evidence.screenshot_error ?? null,
    evidence.stop_error ?? null,
    evidence.record_error ?? null,
  ];
  const failStopSettledExactly = acquisitionErrors.every((error) => error === null)
    && evidence.projection !== null
    && evidence.projection !== undefined
    && evidence.screenshot !== null
    && evidence.screenshot !== undefined
    && evidence.visible_stop_count === 1
    && evidence.terminal !== null
    && typeof evidence.terminal === "object";
  if (!failStopSettledExactly) {
    return new DesktopE2eError(
      "harness",
      "case5_2-provider-control-token-leak-stop",
      `${message}, but the required evidence and visible Stop did not settle exactly`,
      {
        observed_product_failure: observedProductFailure,
        projection_error: evidence.projection_error ?? null,
        screenshot_error: evidence.screenshot_error ?? null,
        visible_stop_count: evidence.visible_stop_count ?? null,
        stop_error: evidence.stop_error ?? null,
        record_error: evidence.record_error ?? null,
        terminal: evidence.terminal ?? null,
      },
    );
  }
  return productFailure(
    observedProductFailure.code,
    observedProductFailure.message,
    observedProductFailure.evidence,
  );
}

async function waitForSideScreenshotObservation({ action, ...options }) {
  try {
    return await waitForObservation({ ...options, retrySampleErrors: false });
  } catch (error) {
    throw classifyCase52SideScreenshotObservationError(error, action);
  }
}

async function waitForMainPreferencesObservation({ action, ...options }) {
  try {
    return await waitForObservation({ ...options, retrySampleErrors: false });
  } catch (error) {
    throw classifyCase52MainPreferencesObservationError(error, action);
  }
}

function canonicalProviderBaseUrl(value, providerProfile) {
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new TypeError("case5_2 provider_base_url must be a non-empty string");
  }
  const url = new URL(value.trim());
  const allowedPaths = providerProfile === OPENAI_COMPATIBLE_PROFILE
    ? new Set(["/v1", "/v1/"])
    : new Set(["", "/"]);
  if (!new Set(["http:", "https:"]).has(url.protocol)
    || url.username.length > 0
    || url.password.length > 0
    || url.search.length > 0
    || url.hash.length > 0
    || !allowedPaths.has(url.pathname)) {
    const expected = providerProfile === OPENAI_COMPATIBLE_PROFILE
      ? "one credential-free HTTP(S) /v1 base URL"
      : "one credential-free HTTP(S) origin";
    throw new TypeError(`case5_2 provider_base_url must be ${expected}`);
  }
  return url.toString().replace(/\/$/, "");
}

function modelIdentity(value, name) {
  if (typeof value !== "string"
    || value.length === 0
    || value.length > 255
    || /[\u0000-\u001f\u007f\s]/.test(value)) {
    throw new TypeError(`case5_2 ${name} is invalid`);
  }
  return value;
}

export function case52ExtraBodyEvidence() {
  return {
    configured: false,
    environment_key: null,
    compact_json_sha256: null,
    compact_json_size_bytes: 0,
    generation_fields: [],
    allowlist_profile: null,
  };
}

export function case52EvidenceOptions(options) {
  return { ...options, extra_body_json: case52ExtraBodyEvidence() };
}

export function normalizeCase52Options(options) {
  if (options === null || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("manual.case5_2 requires one scenario config object");
  }
  const allowed = new Set([
    "fixture_source",
    "provider_base_url",
    "provider_profile",
    "provider_lifecycle",
    "configure_main_via_gui",
    "main_model",
    "side_model",
    "expected_main_variant",
    "expected_side_variant",
  ]);
  const unknown = Object.keys(options).filter((key) => !allowed.has(key));
  if (unknown.length > 0) throw new TypeError(`unknown manual.case5_2 option: ${unknown.join(",")}`);
  if (typeof options.fixture_source !== "string" || !path.isAbsolute(options.fixture_source)) {
    throw new TypeError("manual.case5_2 fixture_source must be an absolute path");
  }
  const providerProfile = options.provider_profile === undefined
    ? LM_STUDIO_PROFILE
    : options.provider_profile;
  if (!new Set([LM_STUDIO_PROFILE, OPENAI_COMPATIBLE_PROFILE]).has(providerProfile)) {
    throw new TypeError("manual.case5_2 provider_profile must be lm_studio or openai_compatible");
  }
  if (options.configure_main_via_gui !== undefined
    && typeof options.configure_main_via_gui !== "boolean") {
    throw new TypeError("manual.case5_2 configure_main_via_gui must be boolean");
  }
  if (options.provider_lifecycle !== undefined
    && !new Set([EXECUTION_OWNED_LIFECYCLE, EXTERNAL_UNMANAGED_LIFECYCLE]).has(options.provider_lifecycle)) {
    throw new TypeError("manual.case5_2 provider_lifecycle must be execution-owned or external-unmanaged");
  }
  if (Object.hasOwn(options, "configure_main_via_gui") && providerProfile !== LM_STUDIO_PROFILE) {
    throw new TypeError("manual.case5_2 configure_main_via_gui is supported only by lm_studio");
  }
  const common = {
    fixtureSource: path.resolve(options.fixture_source),
    providerBaseUrl: canonicalProviderBaseUrl(options.provider_base_url, providerProfile),
    providerProfile,
    mainModel: modelIdentity(options.main_model, "main_model"),
    configureMainViaGui: options.configure_main_via_gui ?? false,
  };
  if (providerProfile === OPENAI_COMPATIBLE_PROFILE) {
    const legacyFields = ["side_model", "expected_main_variant", "expected_side_variant"]
      .filter((key) => Object.hasOwn(options, key));
    if (legacyFields.length > 0) {
      throw new TypeError(`manual.case5_2 openai_compatible does not accept LM Studio fields: ${legacyFields.join(",")}`);
    }
    if (options.provider_lifecycle !== undefined
      && options.provider_lifecycle !== EXTERNAL_UNMANAGED_LIFECYCLE) {
      throw new TypeError("manual.case5_2 openai_compatible supports only provider_lifecycle external-unmanaged");
    }
    return Object.freeze({
      ...common,
      sideModel: common.mainModel,
      expectedMainVariant: null,
      expectedSideVariant: null,
      providerLifecycle: EXTERNAL_UNMANAGED_LIFECYCLE,
      scenarioConfigProfile: "openai-compatible-v1",
    });
  }
  return Object.freeze({
    ...common,
    sideModel: modelIdentity(options.side_model, "side_model"),
    expectedMainVariant: modelIdentity(options.expected_main_variant, "expected_main_variant"),
    expectedSideVariant: modelIdentity(options.expected_side_variant, "expected_side_variant"),
    providerLifecycle: options.provider_lifecycle ?? EXECUTION_OWNED_LIFECYCLE,
    scenarioConfigProfile: options.provider_profile === undefined
      ? "legacy-lm-studio-six-field"
      : options.provider_lifecycle === EXTERNAL_UNMANAGED_LIFECYCLE
        ? "lm-studio-native-external-unmanaged"
        : "lm-studio-native",
  });
}

export function case52FixtureConfig(options) {
  const initialBaseUrl = options.configureMainViaGui
    ? GUI_CONNECTION_BASELINE_BASE_URL
    : options.providerBaseUrl;
  const initialModel = options.configureMainViaGui
    ? GUI_CONNECTION_BASELINE_MODEL
    : options.mainModel;
  const providerConnection = options.providerProfile === OPENAI_COMPATIBLE_PROFILE
    ? `provider_profile = "openai_compatible"`
    : `provider_metadata_mode = "lm_studio_native_required"
provider_api_mode = "responses"`;
  return `[model]
base_url = ${JSON.stringify(initialBaseUrl)}
model = ${JSON.stringify(initialModel)}
${providerConnection}
connect_timeout_ms = 10000
request_timeout_ms = ${QUALITY_REQUEST_TIMEOUT_MS}
max_retries = 0
context_window = ${QUALITY_CONTEXT_WINDOW}
supports_tools = true
supports_images = true
parallel_tool_calls = false
max_parallel_predictions = 1
[permissions]
access_mode = "auto_review"

[multi_agent]
enabled = false
mode = "explicit_request_only"
max_concurrent_agents = 4
max_concurrent_model_requests = 1

[docling]
enabled = false

[mcp]
enabled = false
`;
}

async function fileIdentity(candidate, { includeBytes = false } = {}) {
  const exact = path.resolve(candidate);
  const item = await stat(exact);
  if (!item.isFile()) throw new TypeError(`required case5_2 input is not a file: ${exact}`);
  const bytes = await readFile(exact);
  return {
    path: exact,
    sha256: sha256(bytes),
    size_bytes: bytes.byteLength,
    ...(includeBytes ? { bytes } : {}),
  };
}

function samePhysicalFile(left, right) {
  return left.dev === right.dev
    && left.ino === right.ino
    && left.size === right.size
    && left.isFile()
    && right.isFile()
    && !left.isSymbolicLink()
    && !right.isSymbolicLink();
}

function comparableWindowsPath(candidate) {
  const exact = path.resolve(candidate);
  return process.platform === "win32" ? exact.toLowerCase() : exact;
}

export async function case52PhysicalFileIdentity(candidate) {
  const exact = path.resolve(candidate);
  const before = await lstat(exact);
  if (!before.isFile() || before.isSymbolicLink()) {
    throw new TypeError(`required case5_2 sealed input is not a physical file: ${exact}`);
  }
  const [physicalPath, physicalParent] = await Promise.all([
    realpath(exact),
    realpath(path.dirname(exact)),
  ]);
  const expectedPhysicalPath = path.join(physicalParent, path.basename(exact));
  if (comparableWindowsPath(physicalPath) !== comparableWindowsPath(expectedPhysicalPath)) {
    throw new TypeError(`required case5_2 sealed input escaped its physical parent: ${exact}`);
  }
  const bytes = await readFile(exact);
  const after = await lstat(exact);
  if (!samePhysicalFile(before, after) || bytes.byteLength !== after.size) {
    throw new TypeError(`required case5_2 sealed input changed while its identity was captured: ${exact}`);
  }
  return {
    path: exact,
    physical_path: physicalPath,
    physical_parent: physicalParent,
    device: after.dev,
    inode: after.ino,
    sha256: sha256(bytes),
    size_bytes: bytes.byteLength,
  };
}

export async function assertCase52PhysicalFileIdentity(identity, label = "sealed-input") {
  const current = await case52PhysicalFileIdentity(identity.path);
  const fields = ["physical_path", "physical_parent", "device", "inode", "sha256", "size_bytes"];
  const changed = fields.filter((field) => current[field] !== identity[field]);
  if (changed.length > 0) {
    throw new DesktopE2eError(
      "harness",
      "case5_2-oracle-identity",
      `${label} hidden oracle physical identity changed after it was sealed`,
      { expected: identity, actual: current, changed_fields: changed },
    );
  }
  return current;
}

function manifestAggregate(files) {
  const input = files.map((entry) => `${entry.path}\0${entry.sha256}\0${entry.bytes}\n`).join("");
  return sha256(Buffer.from(input, "utf8"));
}

function baselineManifest(seed, taskIdentity) {
  const files = [
    ...seed.files,
    { path: "task.md", sha256: taskIdentity.sha256, bytes: taskIdentity.size_bytes },
  ].sort((left, right) => left.path.localeCompare(right.path));
  return {
    schema_version: "desktop-e2e.case5_2-baseline.v1",
    source: seed.source,
    destination: seed.destination,
    copy_rule: seed.copy_rule,
    source_seed: {
      file_count: seed.file_count,
      byte_count: seed.byte_count,
      aggregate_sha256: seed.aggregate_sha256,
    },
    file_count: files.length,
    byte_count: files.reduce((total, entry) => total + entry.bytes, 0),
    aggregate_sha256: manifestAggregate(files),
    files,
  };
}

function providerEndpoint(baseUrl, pathname) {
  return new URL(pathname.replace(/^\/+/, ""), `${baseUrl}/`).toString();
}

async function providerJson(baseUrl, pathname, { method = "GET", body = undefined, timeoutMs = 300_000 } = {}) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  const started = Date.now();
  try {
    const response = await fetch(providerEndpoint(baseUrl, pathname), {
      method,
      headers: body === undefined ? undefined : { "content-type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: controller.signal,
    });
    const text = await response.text();
    let value = null;
    try { value = text.length === 0 ? null : JSON.parse(text); }
    catch { throw new Error(`provider returned non-JSON HTTP ${response.status}`); }
    if (!response.ok) {
      throw new Error(`provider returned HTTP ${response.status}: ${text.slice(0, 1000)}`);
    }
    return { endpoint: response.url, status: response.status, elapsed_ms: Date.now() - started, value };
  } finally {
    clearTimeout(timer);
  }
}

function exactCatalogRow(snapshot, key) {
  const rows = snapshot?.v1?.value?.models;
  if (!Array.isArray(rows)) throw new Error("LM Studio v1 model catalog is invalid");
  const matches = rows.filter((row) => row?.key === key);
  if (matches.length !== 1) throw new Error(`LM Studio catalog does not contain exactly one ${key}`);
  return matches[0];
}

async function providerSnapshot(options) {
  if (options.providerProfile === OPENAI_COMPATIBLE_PROFILE) {
    const models = await providerJson(options.providerBaseUrl, "/models");
    return {
      captured_at: new Date().toISOString(),
      provider_profile: options.providerProfile,
      models,
    };
  }
  const [v1, v0] = await Promise.all([
    providerJson(options.providerBaseUrl, "/api/v1/models"),
    providerJson(options.providerBaseUrl, "/api/v0/models"),
  ]);
  return { captured_at: new Date().toISOString(), v1, v0 };
}

function openAiContextCapacity(model) {
  const candidates = [
    ["max_model_len", model?.max_model_len],
    ["max_context_length", model?.max_context_length],
    ["context_length", model?.context_length],
    ["context_window", model?.context_window],
  ].filter(([, value]) => Number.isInteger(value) && value > 0)
    .map(([field, value]) => ({ field, value }));
  const distinct = [...new Set(candidates.map((entry) => entry.value))];
  return {
    reported: candidates.length > 0,
    candidates,
    effective: distinct.length === 1 ? distinct[0] : null,
    conflict: distinct.length > 1,
  };
}

export function case52ProviderModelState(snapshot, options) {
  if (options.providerProfile === OPENAI_COMPATIBLE_PROFILE) {
    const rows = snapshot?.models?.value?.data;
    if (!Array.isArray(rows)) throw new Error("OpenAI-compatible /v1/models catalog is invalid");
    const matches = rows.filter((row) => row?.id === options.mainModel);
    const main = matches.length === 1 ? matches[0] : null;
    return {
      main,
      side: main,
      main_match_count: matches.length,
      context_capacity: openAiContextCapacity(main),
    };
  }
  const main = exactCatalogRow(snapshot, options.mainModel);
  const side = exactCatalogRow(snapshot, options.sideModel);
  const v0Rows = snapshot?.v0?.value?.data;
  const mainV0 = Array.isArray(v0Rows) ? v0Rows.find((row) => row?.id === options.mainModel) ?? null : null;
  const sideV0 = Array.isArray(v0Rows) ? v0Rows.find((row) => row?.id === options.sideModel) ?? null : null;
  return { main, side, main_v0: mainV0, side_v0: sideV0 };
}

function externalProvider(options) {
  return options.providerLifecycle === EXTERNAL_UNMANAGED_LIFECYCLE
    || options.providerProfile === OPENAI_COMPATIBLE_PROFILE;
}

function externalLmStudio(options) {
  return options.providerProfile === LM_STUDIO_PROFILE && externalProvider(options);
}

export function case52LmStudioLoadedContext(state) {
  const instances = state?.main?.loaded_instances;
  const instance = Array.isArray(instances) && instances.length === 1 ? instances[0] : null;
  const candidates = [
    ["loaded_instances[0].config.context_length", instance?.config?.context_length],
    ["loaded_instances[0].context_length", instance?.context_length],
    ["main_v0.loaded_context_length", state?.main_v0?.loaded_context_length],
  ].filter(([, value]) => Number.isInteger(value) && value > 0)
    .map(([field, value]) => ({ field, value }));
  const distinct = [...new Set(candidates.map((entry) => entry.value))];
  return {
    reported: candidates.length > 0,
    candidates,
    effective: distinct.length === 1 ? distinct[0] : null,
    conflict: distinct.length > 1,
  };
}

function volatileHostFingerprintField(key) {
  const value = key.toLowerCase();
  return value === "captured_at"
    || /(?:^|_)timestamp(?:s|_ms)?$/.test(value)
    || /(?:^|_)(?:captured|created|updated|loaded|unloaded|observed|last_used)_at$/.test(value)
    || /(?:^|_)elapsed(?:_ms|_seconds)?$/.test(value);
}

function stableHostFingerprintValue(value, prefix, excludedFields) {
  if (Array.isArray(value)) {
    return value.map((entry, index) => stableHostFingerprintValue(entry, `${prefix}[${index}]`, excludedFields));
  }
  if (value === null || typeof value !== "object") return value;
  const result = {};
  for (const key of Object.keys(value).sort()) {
    const field = prefix.length === 0 ? key : `${prefix}.${key}`;
    if (volatileHostFingerprintField(key)) {
      excludedFields.push(field);
      continue;
    }
    result[key] = stableHostFingerprintValue(value[key], field, excludedFields);
  }
  return result;
}

export function case52ProviderHostFingerprint(snapshot, options) {
  if (options.providerProfile !== LM_STUDIO_PROFILE) {
    throw new TypeError("case5_2 host fingerprint is defined only for LM Studio");
  }
  const models = case52ProviderModelState(snapshot, options);
  const excludedFields = [];
  const value = stableHostFingerprintValue({
    provider_profile: options.providerProfile,
    main_model: options.mainModel,
    side_model: options.sideModel,
    main: models.main,
    side: models.side,
    main_v0: models.main_v0,
    side_v0: models.side_v0,
  }, "", excludedFields);
  const bytes = Buffer.from(JSON.stringify(value), "utf8");
  return {
    schema_version: "desktop-e2e.case5_2-provider-host-fingerprint.v1",
    sha256: sha256(bytes),
    size_bytes: bytes.byteLength,
    excluded_fields: excludedFields.sort(),
    value,
  };
}

export function case52ExternalLmStudioObservation(snapshot, options, expectedHostFingerprint = null) {
  if (!externalLmStudio(options)) {
    throw new TypeError("case5_2 external LM Studio observation requires the external-unmanaged lifecycle");
  }
  const models = case52ProviderModelState(snapshot, options);
  const context = case52LmStudioLoadedContext(models);
  const hostFingerprint = case52ProviderHostFingerprint(snapshot, options);
  const expectedSha256 = typeof expectedHostFingerprint === "string"
    ? expectedHostFingerprint
    : expectedHostFingerprint?.sha256 ?? null;
  const failures = providerCatalogFailures(models, options, { mainLoaded: true });
  if (!context.reported) failures.push("main-loaded-context-unreported");
  if (context.conflict) failures.push("main-loaded-context-conflict");
  if (Number.isInteger(context.effective) && context.effective < QUALITY_CONTEXT_WINDOW) {
    failures.push("main-loaded-context-below-local-budget");
  }
  if (expectedSha256 !== null && hostFingerprint.sha256 !== expectedSha256) {
    failures.push("provider-host-fingerprint-drift");
  }
  return {
    snapshot,
    models,
    host_context: {
      requested: null,
      applied: null,
      reported_loaded: context.effective,
      metadata: context,
    },
    host_fingerprint: hostFingerprint,
    expected_host_fingerprint_sha256: expectedSha256,
    host_fingerprint_stable: expectedSha256 === null ? null : hostFingerprint.sha256 === expectedSha256,
    lifecycle_actions: {
      load_attempted: false,
      unload_attempted: false,
      unload_authorized: false,
    },
    ownership_contract: "external-unmanaged-observation-only",
    failures: [...new Set(failures)],
  };
}

function providerCatalogFailures(state, options, { mainLoaded, expectedLoadedContext = null }) {
  const failures = [];
  if (options.providerProfile === OPENAI_COMPATIBLE_PROFILE) {
    if (state.main_match_count !== 1 || state.main?.id !== options.mainModel) {
      failures.push("main-model-exact-catalog-mismatch");
    }
    if (state.context_capacity?.conflict === true) failures.push("main-context-capacity-conflict");
    if (Number.isInteger(state.context_capacity?.effective)
      && state.context_capacity.effective < QUALITY_CONTEXT_WINDOW) {
      failures.push("main-context-capacity-below-requested");
    }
    return failures;
  }
  if (state.main?.selected_variant !== options.expectedMainVariant) failures.push("main-variant-mismatch");
  if (state.side?.selected_variant !== options.expectedSideVariant) failures.push("side-variant-mismatch");
  const mainInstances = state.main?.loaded_instances;
  const sideInstances = state.side?.loaded_instances;
  if (!Array.isArray(mainInstances) || (mainLoaded ? mainInstances.length !== 1 : mainInstances.length !== 0)) {
    failures.push("main-load-state-mismatch");
  }
  if (mainLoaded && mainInstances?.[0]?.id !== options.mainModel) failures.push("main-instance-id-mismatch");
  if (mainLoaded && expectedLoadedContext !== null
    && mainInstances?.[0]?.config?.context_length !== expectedLoadedContext) {
    failures.push("main-instance-context-mismatch");
  }
  if (!Array.isArray(sideInstances) || sideInstances.length !== 0) failures.push("side-model-loaded");
  if (state.main_v0?.state !== (mainLoaded ? "loaded" : "not-loaded")) failures.push("main-v0-state-mismatch");
  if (state.side_v0?.state !== "not-loaded") failures.push("side-v0-state-mismatch");
  return failures;
}

export function case52ProviderCleanupPlan(providerState) {
  const failures = [];
  let unexpectedSideLoaded = false;
  const instances = new Map();
  for (const row of [
    { role: "main", values: providerState?.main?.loaded_instances },
    { role: "side", values: providerState?.side?.loaded_instances },
  ]) {
    if (!Array.isArray(row.values)) {
      failures.push(`${row.role}-loaded-instances-invalid`);
      continue;
    }
    if (row.role === "side" && row.values.length > 0) unexpectedSideLoaded = true;
    for (const value of row.values) {
      if (typeof value?.id !== "string" || value.id.length === 0) {
        failures.push(`${row.role}-instance-id-invalid`);
        continue;
      }
      const current = instances.get(value.id) ?? { instance_id: value.id, roles: [] };
      if (!current.roles.includes(row.role)) current.roles.push(row.role);
      instances.set(value.id, current);
    }
  }
  return {
    instances: [...instances.values()].sort((left, right) => left.instance_id.localeCompare(right.instance_id)),
    unexpected_side_loaded: unexpectedSideLoaded,
    failures: [...new Set(failures)].sort(),
  };
}

export async function loadMainProvider({ options, sink, state, phase, providerIo = {} }) {
  const capture = providerIo.capture ?? (async () => {
    const snapshot = await providerSnapshot(options);
    return { snapshot, models: case52ProviderModelState(snapshot, options) };
  });
  if (externalLmStudio(options)) {
    const baselineCapture = await capture();
    const baseline = case52ExternalLmStudioObservation(baselineCapture.snapshot, options);
    const confirmationCapture = await capture();
    const confirmation = case52ExternalLmStudioObservation(
      confirmationCapture.snapshot,
      options,
      baseline.host_fingerprint,
    );
    const failures = [
      ...baseline.failures.map((failure) => `baseline:${failure}`),
      ...confirmation.failures.map((failure) => `confirmation:${failure}`),
    ];
    const effectiveContext = confirmation.host_context.reported_loaded;
    const deviations = [
      "provider-lifecycle-external-unmanaged",
      ...(effectiveContext === QUALITY_CONTEXT_WINDOW ? [] : ["provider-context-capacity-not-exact"]),
    ];
    const evidence = {
      baseline,
      confirmation,
      failures,
      ownership_contract: "external-unmanaged-observation-only",
      lifecycle_actions: {
        load_attempted: false,
        unload_attempted: false,
        unload_authorized: false,
      },
      comparability_deviations: deviations,
    };
    await sink.writeJson("case5_2/provider/external-lm-studio-preflight.json", evidence);
    await sink.record("case5_2-provider-external-lm-studio-preflight", evidence, { phase, owner: OWNER });
    if (failures.length > 0) {
      throw new DesktopE2eError(
        "environment",
        "case5_2-provider-preflight",
        "external-unmanaged LM Studio did not retain the exact loaded Main, unloaded Side, variants, context, and host fingerprint",
        evidence,
      );
    }
    state.providerEffectiveContext = effectiveContext;
    state.providerProfileExact = deviations.length === 0;
    state.providerComparabilityDeviations = deviations;
    state.acceptedProviderLoad = structuredClone(evidence);
    state.providerExternalPreflightObserved = true;
    state.providerHostFingerprint = structuredClone(baseline.host_fingerprint);
    state.providerHostFingerprintSamples ??= [];
    state.providerHostFingerprintSamples.push(
      { name: "preflight-baseline", fingerprint: structuredClone(baseline.host_fingerprint) },
      { name: "preflight-confirmation", fingerprint: structuredClone(confirmation.host_fingerprint) },
    );
    return;
  }
  if (options.providerProfile === OPENAI_COMPATIBLE_PROFILE) {
    const captured = await capture();
    const snapshot = captured.snapshot;
    const models = captured.models ?? case52ProviderModelState(snapshot, options);
    const failures = providerCatalogFailures(models, options, { mainLoaded: true });
    const effectiveContext = models.context_capacity.effective;
    const deviations = [
      "provider-lifecycle-external-unmanaged",
      "provider-wire-openai-compatible-chat-completions",
      ...(models.context_capacity.reported ? [] : ["provider-context-capacity-unreported"]),
      ...(Number.isInteger(effectiveContext) && effectiveContext !== QUALITY_CONTEXT_WINDOW
        ? ["provider-context-capacity-not-exact"]
        : []),
    ];
    const evidence = {
      snapshot,
      models,
      context: {
        requested: null,
        applied: null,
        reported: effectiveContext,
        metadata: models.context_capacity,
      },
      failures,
      ownership_contract: "external-unmanaged-observation-only",
      lifecycle_actions: { load_attempted: false, unload_attempted: false, unload_authorized: false },
      comparability_deviations: deviations,
    };
    await sink.writeJson("case5_2/provider/external-preflight.json", evidence);
    await sink.record("case5_2-provider-external-preflight", evidence, { phase, owner: OWNER });
    if (failures.length > 0) {
      throw new DesktopE2eError(
        "environment",
        "case5_2-provider-preflight",
        "OpenAI-compatible provider did not expose the exact main model with sufficient reported context capacity",
        evidence,
      );
    }
    state.providerEffectiveContext = effectiveContext;
    state.providerProfileExact = deviations.length === 0;
    state.providerComparabilityDeviations = deviations;
    state.acceptedProviderLoad = structuredClone(evidence);
    state.providerExternalPreflightObserved = true;
    return;
  }
  const before = await providerSnapshot(options);
  const beforeState = case52ProviderModelState(before, options);
  const beforeFailures = providerCatalogFailures(beforeState, options, { mainLoaded: false });
  await sink.writeJson("case5_2/provider/before-load.json", { snapshot: before, models: beforeState, failures: beforeFailures });
  if (beforeFailures.length > 0) {
    throw new DesktopE2eError("environment", "case5_2-provider-preflight", "case5_2 provider preflight did not start from both selected models unloaded", {
      failures: beforeFailures,
      models: beforeState,
    });
  }
  state.providerLoadAttempted = true;
  const request = { model: options.mainModel, context_length: QUALITY_CONTEXT_WINDOW, echo_load_config: true };
  const issueLoad = providerIo.load ?? ((body) => providerJson(options.providerBaseUrl, "/api/v1/models/load", {
    method: "POST",
    body,
  }));
  const response = await issueLoad(request);
  state.providerLoadResponseObserved = true;
  state.mainProviderInstanceId = typeof response.value?.instance_id === "string"
    ? response.value.instance_id
    : null;
  state.providerOwned = state.mainProviderInstanceId === options.mainModel && response.value?.status === "loaded";
  const responseContext = response.value?.load_config?.context_length;
  const after = await providerSnapshot(options);
  const afterState = case52ProviderModelState(after, options);
  const afterFailures = providerCatalogFailures(afterState, options, {
    mainLoaded: true,
    expectedLoadedContext: Number.isInteger(responseContext) ? responseContext : null,
  });
  if (!Number.isInteger(responseContext)) afterFailures.push("main-response-context-missing");
  if (Number.isInteger(responseContext) && responseContext < QUALITY_CONTEXT_WINDOW) {
    afterFailures.push("main-response-context-below-requested");
  }
  const context = {
    requested: QUALITY_CONTEXT_WINDOW,
    applied: Number.isInteger(responseContext) ? responseContext : null,
    exact: responseContext === QUALITY_CONTEXT_WINDOW,
  };
  const evidence = {
    request,
    response,
    snapshot: after,
    models: afterState,
    context,
    failures: [...new Set(afterFailures)],
    ownership_contract: "exclusive-preflight-unloaded-execution-owned-load",
  };
  await sink.writeJson("case5_2/provider/load.json", evidence);
  await sink.record("case5_2-provider-loaded", evidence, { phase, owner: OWNER });
  if (!state.providerOwned || evidence.failures.length > 0) {
    throw new DesktopE2eError("environment", "case5_2-provider-load", "Qwen main provider did not reach the exact loaded state while Gemma stayed unloaded", evidence);
  }
  state.providerEffectiveContext = responseContext;
  state.providerProfileExact = context.exact;
  state.providerComparabilityDeviations = context.exact
    ? []
    : ["provider-context-capacity-not-exact"];
  state.acceptedProviderLoad = structuredClone(evidence);
}

export async function unloadMainProvider({ options, state, providerIo = {} }) {
  if (externalProvider(options)) {
    const capture = providerIo.capture ?? (async () => {
      const snapshot = await providerSnapshot(options);
      return { snapshot, models: case52ProviderModelState(snapshot, options) };
    });
    let captured = null;
    let error = null;
    let failures = [];
    try {
      captured = await capture();
      captured = {
        ...captured,
        models: captured.models ?? case52ProviderModelState(captured.snapshot, options),
      };
      if (externalLmStudio(options)) {
        const observation = case52ExternalLmStudioObservation(
          captured.snapshot,
          options,
          state.providerHostFingerprint,
        );
        captured = { ...captured, observation };
        failures = observation.failures;
      } else {
        failures = providerCatalogFailures(captured.models, options, { mainLoaded: true });
      }
    } catch (caught) {
      error = errorObservation(caught);
      failures = ["provider-final-snapshot-failed"];
    }
    if (state.providerExternalPreflightObserved !== true) {
      failures.push("provider-external-preflight-unobserved");
    }
    const pass = error === null && failures.length === 0;
    return {
      input: pass ? "pass" : "fail",
      resources: [{
        kind: options.providerProfile === LM_STUDIO_PROFILE
          ? "lm-studio-external-model"
          : "openai-compatible-external-model",
        provider_profile: options.providerProfile,
        lifecycle: EXTERNAL_UNMANAGED_LIFECYCLE,
        main_model: options.mainModel,
        side_model: options.sideModel,
        load_attempted: false,
        unload_attempted: false,
        unload_authorized: false,
        final_snapshot: captured?.snapshot ?? null,
        models: captured?.models ?? null,
        host_context: captured?.observation?.host_context ?? null,
        host_fingerprint: captured?.observation?.host_fingerprint ?? null,
        expected_host_fingerprint_sha256: captured?.observation?.expected_host_fingerprint_sha256 ?? null,
        host_fingerprint_stable: captured?.observation?.host_fingerprint_stable ?? null,
        comparability_deviations: state.providerComparabilityDeviations ?? [],
        failures: [...new Set(failures)],
        error,
      }],
      productFailure: null,
    };
  }
  if (!state.providerLoadAttempted) {
    return {
      input: "pass",
      resources: [{
        kind: "lm-studio-model",
        main_model: options.mainModel,
        side_model: options.sideModel,
        load_attempted: false,
        unload: null,
        failures: [],
        error: null,
      }],
      productFailure: null,
    };
  }
  const capture = providerIo.capture ?? (async () => {
    const snapshot = await providerSnapshot(options);
    return { snapshot, models: case52ProviderModelState(snapshot, options) };
  });
  const unloadInstance = providerIo.unload ?? ((instanceId) => providerJson(options.providerBaseUrl, "/api/v1/models/unload", {
    method: "POST",
    body: { instance_id: instanceId },
  }));
  const wait = providerIo.delay ?? delay;
  const now = providerIo.now ?? (() => Date.now());
  const timeoutMs = providerIo.timeoutMs ?? PROVIDER_CLEANUP_TIMEOUT_MS;
  const pollMs = providerIo.pollMs ?? PROVIDER_CLEANUP_POLL_MS;
  const stableSamplesRequired = providerIo.stableSamples ?? PROVIDER_CLEANUP_STABLE_SAMPLES;
  if (!Number.isInteger(timeoutMs) || timeoutMs < 1
    || !Number.isInteger(pollMs) || pollMs < 0
    || !Number.isInteger(stableSamplesRequired) || stableSamplesRequired < 2) {
    throw new TypeError("case5_2 provider cleanup bounds are invalid");
  }

  const startedAt = now();
  const deadline = startedAt + timeoutMs;
  const observations = [];
  const unload = [];
  let before = null;
  let beforeState = null;
  let after = null;
  let afterState = null;
  let stableSamples = 0;
  let stableZero = false;
  let unexpectedSideLoaded = false;
  let acceptedFallbackPending = typeof state.mainProviderInstanceId === "string"
    && state.mainProviderInstanceId.length > 0;

  while (now() <= deadline) {
    const iteration = observations.length + 1;
    let captured = null;
    let plan = null;
    let catalogFailures = [];
    let captureError = null;
    try {
      captured = await capture();
      if (before === null) {
        before = captured.snapshot;
        beforeState = captured.models;
      }
      after = captured.snapshot;
      afterState = captured.models;
      plan = case52ProviderCleanupPlan(captured.models);
      catalogFailures = providerCatalogFailures(captured.models, options, { mainLoaded: false });
      const sideV0Active = typeof captured.models?.side_v0?.state === "string"
        && captured.models.side_v0.state !== "not-loaded";
      unexpectedSideLoaded ||= plan.unexpected_side_loaded || sideV0Active;
    } catch (error) {
      captureError = errorObservation(error);
    }

    const candidates = new Map();
    if (acceptedFallbackPending) {
      candidates.set(state.mainProviderInstanceId, {
        instance_id: state.mainProviderInstanceId,
        roles: ["main"],
        sources: ["accepted-load-response"],
      });
      acceptedFallbackPending = false;
    }
    for (const entry of plan?.instances ?? []) {
      const current = candidates.get(entry.instance_id) ?? {
        instance_id: entry.instance_id,
        roles: [],
        sources: [],
      };
      current.roles = [...new Set([...current.roles, ...entry.roles])].sort();
      current.sources = [...new Set([...current.sources, "provider-catalog"])].sort();
      candidates.set(entry.instance_id, current);
    }

    const exactZero = captureError === null
      && plan?.instances.length === 0
      && plan.failures.length === 0
      && catalogFailures.length === 0
      && candidates.size === 0;
    stableSamples = exactZero ? stableSamples + 1 : 0;
    observations.push({
      iteration,
      captured_at: new Date().toISOString(),
      snapshot: captured?.snapshot ?? null,
      models: captured?.models ?? null,
      plan,
      catalog_failures: catalogFailures,
      capture_error: captureError,
      exact_zero: exactZero,
      stable_samples: stableSamples,
      unload_candidates: [...candidates.values()],
    });

    for (const entry of candidates.values()) {
      try {
        const response = await unloadInstance(entry.instance_id);
        unload.push({ iteration, ...entry, response, error: null });
      } catch (error) {
        unload.push({ iteration, ...entry, response: null, error: errorObservation(error) });
      }
    }

    if (stableSamples >= stableSamplesRequired && state.providerLoadResponseObserved === true) {
      stableZero = true;
      break;
    }
    if (now() >= deadline) break;
    await wait(Math.min(pollMs, Math.max(0, deadline - now())));
  }

  const finalObservation = observations.at(-1) ?? null;
  const failures = stableZero ? [] : [
    "provider-cleanup-stable-zero-not-observed",
    ...(state.providerLoadResponseObserved === true ? [] : ["provider-load-response-unobserved"]),
    ...(finalObservation?.plan?.failures ?? []),
    ...(finalObservation?.catalog_failures ?? []),
    ...(finalObservation?.capture_error === null ? [] : ["provider-final-snapshot-failed"]),
  ];
  state.providerOwned = false;
  return {
    input: stableZero ? "pass" : "fail",
    resources: [{
      kind: "lm-studio-model",
      main_model: options.mainModel,
      side_model: options.sideModel,
      accepted_main_instance_id: state.mainProviderInstanceId,
      load_response_observed: state.providerLoadResponseObserved === true,
      before,
      before_models: beforeState,
      unload,
      after,
      models: afterState,
      observations,
      stable_zero: stableZero,
      stable_samples_required: stableSamplesRequired,
      stable_samples_observed: stableSamples,
      cleanup_timeout_ms: timeoutMs,
      cleanup_elapsed_ms: Math.max(0, now() - startedAt),
      failures,
      error: finalObservation?.capture_error ?? null,
    }],
    productFailure: unexpectedSideLoaded ? {
      code: "case5_2-side-model-loaded",
      message: "Gemma Side Chat model became loaded even though no Side Chat request was authorized",
      evidence: {
        observations,
        unload,
        after_models: afterState,
        failures,
      },
    } : null,
  };
}

async function runOwnedExternalProcess({ context, executable, args, cwd, env, timeoutMs, state, label }) {
  const outputRoot = path.join(context.paths.logs, "external-process-output");
  await mkdir(outputRoot, { recursive: true });
  const stdoutPath = path.join(outputRoot, `${label}.stdout.raw`);
  const stderrPath = path.join(outputRoot, `${label}.stderr.raw`);
  state.externalProcessOwner.started += 1;
  state.externalProcessOwner.in_flight += 1;
  try {
    const owned = await runWindowsExternalProcess({
      executionRoot: context.root,
      executable,
      args,
      cwd,
      env,
      stdoutPath,
      stderrPath,
      timeoutMs,
      maxOutputBytes: MAX_CAPTURE_BYTES,
      label,
    });
    const stdoutCapture = await readCase52ExternalOutput(stdoutPath, owned.output.stdout);
    const stderrCapture = await readCase52ExternalOutput(stderrPath, owned.output.stderr);
    state.externalProcessOwner.settled += 1;
    return {
      ...owned,
      executable: owned.command.executable.path,
      args: owned.command.args,
      cwd: owned.command.cwd,
      exit_code: owned.outcome.root_exit_code,
      signal: null,
      elapsed_ms: owned.outcome.elapsed_ms,
      stdout: stdoutCapture.bytes,
      stderr: stderrCapture.bytes,
      capture: {
        stdout: stdoutCapture.identity,
        stderr: stderrCapture.identity,
      },
    };
  } catch (error) {
    const failure = { label, ...errorObservation(error) };
    state.externalProcessOwner.failures.push(failure);
    throw new DesktopE2eError(
      "harness",
      "case5_2-external-process-owner",
      `${label} did not complete the reusable Windows Job ownership contract`,
      failure,
    );
  } finally {
    state.externalProcessOwner.in_flight -= 1;
  }
}

export async function readCase52ExternalOutput(candidate, expected) {
  const item = await lstat(candidate);
  if (!item.isFile() || item.isSymbolicLink() || item.size !== expected.size_bytes) {
    throw new WindowsExternalProcessError(
      "external-output-readback",
      "external process output size or physical identity changed after Windows Job settlement",
      { path: candidate, expected, observed_size_bytes: item.size },
    );
  }
  if (item.size <= MAX_CAPTURE_BYTES) {
    const bytes = await readFile(candidate);
    const digest = sha256(bytes);
    if (digest !== expected.sha256 || bytes.byteLength !== expected.size_bytes) {
      throw new WindowsExternalProcessError(
        "external-output-readback",
        "external process output changed after Windows Job settlement",
        { path: candidate, expected, observed: { sha256: digest, size_bytes: bytes.byteLength } },
      );
    }
    return {
      bytes,
      identity: {
        raw: expected,
        sample_kind: "full",
        sample_size_bytes: bytes.byteLength,
      },
    };
  }
  const half = Math.floor(MAX_CAPTURE_SAMPLE_BYTES / 2);
  const head = Buffer.alloc(half);
  const tail = Buffer.alloc(half);
  const handle = await open(candidate, "r");
  try {
    const opened = await handle.stat();
    if (!samePhysicalFile(item, opened)) {
      throw new WindowsExternalProcessError(
        "external-output-readback",
        "external process output physical identity changed before bounded readback",
        { path: candidate, expected, observed_size_bytes: opened.size },
      );
    }
    const digest = crypto.createHash("sha256");
    const stream = handle.createReadStream({ autoClose: false, start: 0 });
    for await (const chunk of stream) digest.update(chunk);
    const observedSha256 = digest.digest("hex");
    if (observedSha256 !== expected.sha256) {
      throw new WindowsExternalProcessError(
        "external-output-readback",
        "external process output SHA-256 changed before bounded readback",
        { path: candidate, expected, observed: { sha256: observedSha256, size_bytes: opened.size } },
      );
    }
    const headRead = await handle.read(head, 0, head.byteLength, 0);
    const tailRead = await handle.read(tail, 0, tail.byteLength, Math.max(0, item.size - tail.byteLength));
    const settled = await lstat(candidate);
    if (!samePhysicalFile(item, settled)) {
      throw new WindowsExternalProcessError(
        "external-output-readback",
        "external process output physical identity changed during bounded readback",
        { path: candidate, expected, observed_size_bytes: settled.size },
      );
    }
    const marker = Buffer.from(`\n...[bounded sample; raw bytes=${item.size}; sha256=${expected.sha256}]...\n`, "utf8");
    const bytes = Buffer.concat([
      head.subarray(0, headRead.bytesRead),
      marker,
      tail.subarray(0, tailRead.bytesRead),
    ]);
    return {
      bytes,
      identity: {
        raw: expected,
        sample_kind: "head-tail",
        sample_size_bytes: bytes.byteLength,
        head_bytes: headRead.bytesRead,
        tail_bytes: tailRead.bytesRead,
      },
    };
  } finally {
    await handle.close();
  }
}

async function ownedProcessEnvironment(context, label, extra = {}) {
  if (typeof label !== "string" || !/^[a-z0-9][a-z0-9._-]{1,95}$/.test(label)) {
    throw new TypeError(`invalid external process label: ${label}`);
  }
  const parent = path.join(context.paths.logs, "external-temp");
  await mkdir(parent, { recursive: true });
  const parentItem = await lstat(parent);
  if (!parentItem.isDirectory() || parentItem.isSymbolicLink()) {
    throw new DesktopE2eError("harness", "case5_2-temp-owner", "external process temp parent is not a physical directory", { parent });
  }
  const temporary = path.join(parent, label);
  await mkdir(temporary, { recursive: false });
  const temporaryItem = await lstat(temporary);
  if (!temporaryItem.isDirectory() || temporaryItem.isSymbolicLink()) {
    throw new DesktopE2eError("harness", "case5_2-temp-owner", "external process temp owner is not a physical directory", { temporary });
  }
  const reserved = new Set(["temp", "tmp", "tmpdir"]);
  const override = Object.keys(extra).find((key) => reserved.has(key.toLowerCase()));
  if (override !== undefined) {
    throw new TypeError(`external process caller cannot override execution-owned ${override}`);
  }
  return {
    ...process.env,
    ...extra,
    TEMP: temporary,
    TMP: temporary,
    TMPDIR: temporary,
  };
}

async function assertExecutableIdentity(identity, label) {
  const current = await fileIdentity(identity.path);
  if (current.sha256 !== identity.sha256 || current.size_bytes !== identity.size_bytes) {
    throw new DesktopE2eError(
      "environment",
      "case5_2-executable-drift",
      `${label} executable identity changed after case5_2 preflight`,
      { expected: identity, actual: current },
    );
  }
  return current;
}

async function assertOracleIdentity(identity, label) {
  return assertCase52PhysicalFileIdentity(identity, label);
}

async function resolveExecutable(name, state, context) {
  const systemRoot = process.env.SystemRoot;
  if (typeof systemRoot !== "string" || systemRoot.length === 0) throw new Error("SystemRoot is unavailable");
  const where = path.join(systemRoot, "System32", "where.exe");
  const lookup = await runOwnedExternalProcess({
    context,
    executable: where,
    args: [name],
    cwd: context.root,
    env: await ownedProcessEnvironment(context, `resolve-${name.toLowerCase().replace(/[^a-z0-9._-]+/g, "-")}`),
    timeoutMs: 10_000,
    state,
    label: `resolve-${name}`,
  });
  if (lookup.exit_code !== 0) throw new DesktopE2eError("environment", "case5_2-executable-missing", `required executable is unavailable: ${name}`);
  const candidates = lookup.stdout.toString("utf8").split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
  if (candidates.length === 0) throw new DesktopE2eError("environment", "case5_2-executable-missing", `required executable is unavailable: ${name}`);
  const identity = await fileIdentity(candidates[0]);
  return { ...identity, candidates };
}

function ignoredRuntimePath(relativePath) {
  const normalized = relativePath.replaceAll("\\", "/");
  const segments = normalized.toLowerCase().split("/");
  if (segments.some((segment) => new Set([
    ".moyai",
    "__pycache__",
    ".pytest_cache",
    ".venv",
    "node_modules",
    ".next",
    ".test-dist",
  ]).has(segment))) return true;
  return normalized.toLowerCase().startsWith("backend/data/")
    || normalized.toLowerCase().endsWith(".pyc")
    || normalized.toLowerCase().endsWith(".pyo");
}

async function workspaceFiles(workspace) {
  const exactRoot = await realpath(workspace);
  const files = [];
  async function visit(directory) {
    const entries = await readdir(directory, { withFileTypes: true });
    entries.sort((left, right) => left.name.localeCompare(right.name));
    for (const entry of entries) {
      const candidate = path.join(directory, entry.name);
      const relative = path.relative(exactRoot, candidate).replaceAll("\\", "/");
      const item = await lstat(candidate);
      if (entry.isSymbolicLink() || item.isSymbolicLink()) {
        throw productFailure("case5_2-workspace-link", "case5_2 workspace contains a symbolic link or junction", { path: relative });
      }
      if (entry.isDirectory()) {
        if (!ignoredRuntimePath(`${relative}/placeholder`)) await visit(candidate);
        continue;
      }
      if (!entry.isFile()) {
        throw productFailure("case5_2-workspace-entry", "case5_2 workspace contains an unsupported filesystem entry", { path: relative });
      }
      if (ignoredRuntimePath(relative)) continue;
      const bytes = await readFile(candidate);
      files.push({ path: relative, sha256: sha256(bytes), bytes: bytes.byteLength });
    }
  }
  await visit(exactRoot);
  return files.sort((left, right) => left.path.localeCompare(right.path));
}

function fileMap(files) {
  return new Map(files.map((entry) => [entry.path, entry]));
}

function manifestDiff(baseline, current) {
  const before = fileMap(baseline.files);
  const after = fileMap(current);
  return {
    modified: [...before.entries()]
      .filter(([name, row]) => after.has(name) && after.get(name).sha256 !== row.sha256)
      .map(([name]) => name)
      .sort(),
    deleted: [...before.keys()].filter((name) => !after.has(name)).sort(),
    added: [...after.keys()].filter((name) => !before.has(name)).sort(),
  };
}

export function case52EvaluatorWorkspaceDiff(before, after) {
  if (!Array.isArray(before?.files) || !Array.isArray(after?.files)) {
    throw new TypeError("case5_2 evaluator workspace manifests require file arrays");
  }
  return manifestDiff({ files: before.files }, after.files);
}

async function assertEvaluatorWorkspaceStable({ sink, before, after, label }) {
  const diff = case52EvaluatorWorkspaceDiff(before, after);
  const stable = diff.modified.length === 0 && diff.deleted.length === 0 && diff.added.length === 0;
  await sink.record("case5_2-evaluator-workspace-stability", {
    label,
    before_stage: before.stage,
    after_stage: after.stage,
    stable,
    diff,
  }, { phase: "executing", owner: OWNER });
  if (!stable) {
    throw productFailure(
      "case5_2-evaluator-workspace-mutation",
      `${label} external evaluator changed product workspace source or documents`,
      { before_stage: before.stage, after_stage: after.stage, diff },
    );
  }
  return diff;
}

async function documentRows(workspace) {
  return Promise.all(REQUIRED_DOCUMENTS.map(async (name) => {
    const candidate = path.join(workspace, name);
    try {
      const item = await stat(candidate);
      return { name, exists: item.isFile(), bytes: item.isFile() ? item.size : 0 };
    } catch (error) {
      if (error?.code === "ENOENT") return { name, exists: false, bytes: 0 };
      throw error;
    }
  }));
}

async function evidenceMatrixRows(workspace) {
  try {
    const content = await readFile(path.join(workspace, "evidence_matrix.md"), "utf8");
    const rows = content.split(/\r?\n/).filter((line) => /^\s*\|/.test(line) && !/^\s*\|\s*[-:]/.test(line));
    return Math.max(0, rows.length - 1);
  } catch (error) {
    if (error?.code === "ENOENT") return 0;
    throw error;
  }
}

async function captureWorkspaceManifest({ context, baseline, stage }) {
  const files = await workspaceFiles(context.paths.workspace);
  return {
    schema_version: "desktop-e2e.case5_2-stage-manifest.v1",
    stage,
    captured_at: new Date().toISOString(),
    workspace: context.paths.workspace,
    baseline_aggregate_sha256: baseline.aggregate_sha256,
    files,
    diff: manifestDiff(baseline, files),
    documents: await documentRows(context.paths.workspace),
    evidence_matrix_rows: await evidenceMatrixRows(context.paths.workspace),
  };
}

async function storeStageManifest({ context, sink, baseline, stage }) {
  const manifest = await captureWorkspaceManifest({ context, baseline, stage });
  const identity = await sink.writeJson(`case5_2/manifests/${stage}.json`, manifest);
  await sink.record("case5_2-stage-manifest", { stage, identity, diff: manifest.diff, documents: manifest.documents, evidence_matrix_rows: manifest.evidence_matrix_rows }, {
    phase: "executing",
    owner: OWNER,
  });
  return manifest;
}

export function case52ForbiddenWorkspacePaths(files) {
  const forbidden = [];
  for (const relative of files) {
    const normalized = relative.replaceAll("\\", "/").toLowerCase();
    const segments = normalized.split("/");
    if (segments.some((segment) => new Set([
      "node_modules",
      ".venv",
      "venv",
      ".virtualenv",
      "virtualenv",
      ".eggs",
      "site-packages",
      "pip-wheel-metadata",
      "__pypackages__",
      ".tox",
      ".nox",
    ]).has(segment) || segment.endsWith(".egg-info") || segment.endsWith(".dist-info"))) {
      forbidden.push(relative);
    }
    const leaf = segments.at(-1);
    if (leaf === ".env" || leaf === ".env.local") forbidden.push(relative);
  }
  return [...new Set(forbidden)].sort();
}

async function allWorkspacePaths(workspace) {
  const root = await realpath(workspace);
  const result = [];
  async function visit(directory) {
    const entries = await readdir(directory, { withFileTypes: true });
    for (const entry of entries) {
      const candidate = path.join(directory, entry.name);
      const relative = path.relative(root, candidate).replaceAll("\\", "/");
      result.push(relative);
      if (entry.isDirectory() && !entry.isSymbolicLink()) await visit(candidate);
    }
  }
  await visit(root);
  return result.sort();
}

function dependencyManifestPath(relativePath) {
  const leaf = relativePath.replaceAll("\\", "/").split("/").at(-1).toLowerCase();
  return leaf === "pyproject.toml"
    || leaf === "poetry.lock"
    || leaf === "uv.lock"
    || leaf === "package.json"
    || leaf === "package-lock.json"
    || leaf === "pnpm-lock.yaml"
    || leaf === "yarn.lock"
    || /^requirements(?:[-_.].*)?\.txt$/.test(leaf);
}

function finalScopeFailures(manifest, forbidden) {
  const changed = [...manifest.diff.modified, ...manifest.diff.deleted, ...manifest.diff.added];
  const failures = [];
  const frontend = changed.filter((name) => name.toLowerCase().startsWith("frontend/"));
  const dependencies = changed.filter(dependencyManifestPath);
  if (frontend.length > 0) failures.push({ kind: "frontend-changed", paths: frontend });
  if (dependencies.length > 0) failures.push({ kind: "dependency-manifest-changed", paths: dependencies });
  if (forbidden.length > 0) failures.push({ kind: "forbidden-runtime-or-dependency-path", paths: forbidden });
  return failures;
}

async function externalRootInventory(root) {
  const exact = path.resolve(root);
  let physical;
  try { physical = await realpath(exact); }
  catch (error) {
    if (error?.code === "ENOENT") return { root: exact, present: false, entries: [], aggregate_sha256: sha256(Buffer.alloc(0)) };
    throw error;
  }
  const entries = [];
  async function visit(directory) {
    const children = await readdir(directory, { withFileTypes: true });
    children.sort((left, right) => left.name.localeCompare(right.name));
    for (const child of children) {
      const candidate = path.join(directory, child.name);
      const relative = path.relative(physical, candidate).replaceAll("\\", "/");
      if (relative.toLowerCase().split("/").includes("__pycache__") || /\.(?:pyc|pyo)$/i.test(relative)) continue;
      const item = await lstat(candidate);
      if (child.isSymbolicLink() || item.isSymbolicLink()) {
        entries.push({ path: relative, kind: "link", size_bytes: item.size, mtime_ms: item.mtimeMs });
      } else if (child.isDirectory()) {
        entries.push({ path: `${relative}/`, kind: "directory", size_bytes: 0, mtime_ms: item.mtimeMs });
        await visit(candidate);
      } else if (child.isFile()) {
        entries.push({ path: relative, kind: "file", size_bytes: item.size, mtime_ms: item.mtimeMs });
      }
    }
  }
  await visit(physical);
  const aggregate = entries.map((entry) => `${entry.path}\0${entry.kind}\0${entry.size_bytes}\0${entry.mtime_ms}\n`).join("");
  return { root: physical, present: true, entry_count: entries.length, aggregate_sha256: sha256(Buffer.from(aggregate, "utf8")), entries };
}

async function pythonSiteRoots(python, context, state) {
  await assertExecutableIdentity(python, "python-site-roots");
  const result = await runOwnedExternalProcess({
    context,
    executable: python.path,
    args: ["-X", "utf8", "-c", "import json,site; print(json.dumps({'user': site.getusersitepackages(), 'system': site.getsitepackages()}))"],
    cwd: context.paths.logs,
    env: await ownedProcessEnvironment(context, "python-site-roots", {
      PYTHONDONTWRITEBYTECODE: "1",
      PYTHONPYCACHEPREFIX: path.join(context.paths.logs, "python-site-probe-cache"),
    }),
    timeoutMs: 30_000,
    state,
    label: "python-site-roots",
  });
  if (result.exit_code !== 0) throw new DesktopE2eError("environment", "case5_2-python-site-probe", "Python site-package roots could not be resolved", { stderr: result.stderr.toString("utf8") });
  const parsed = JSON.parse(result.stdout.toString("utf8"));
  const roots = [parsed.user, ...(Array.isArray(parsed.system) ? parsed.system : [])]
    .filter((value) => typeof value === "string" && path.isAbsolute(value));
  return [...new Set(roots.map((value) => path.resolve(value)))];
}

async function externalRootsSnapshot(roots) {
  return Promise.all(roots.map((root) => externalRootInventory(root)));
}

function externalRootDrift(before, after) {
  const previous = new Map(before.map((entry) => [entry.root.toLowerCase(), entry]));
  const current = new Map(after.map((entry) => [entry.root.toLowerCase(), entry]));
  const keys = [...new Set([...previous.keys(), ...current.keys()])].sort();
  return keys.flatMap((key) => {
    const left = previous.get(key) ?? null;
    const right = current.get(key) ?? null;
    return left?.present === right?.present && left?.aggregate_sha256 === right?.aggregate_sha256
      ? []
      : [{ root: right?.root ?? left?.root ?? key, before: left, after: right }];
  });
}

function selectedSessionRow(projection) {
  const rows = projection?.selected_project_index >= 0 ? projection?.session_rows : projection?.chat_session_rows;
  return Number.isInteger(projection?.selected_session_index) && projection.selected_session_index >= 0
    ? rows?.[projection.selected_session_index] ?? null
    : null;
}

async function desktopProjection(cdp) {
  return invokeDesktopCommand(cdp, "desktop_state");
}

async function trustedClick(input, locator, { stableHitSamples = 1 } = {}) {
  const start = (await input.snapshotProbe()).sequence;
  const target = await input.click(locator, { stableHitSamples });
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

async function recordTrustedClick({ input, locator, action, sink, stableHitSamples = 1 }) {
  const acquisition = await trustedClick(input, locator, { stableHitSamples });
  await sink.record("case5_2-trusted-action", { action, input_kind: "browser_trusted", ...acquisition }, {
    phase: "executing",
    owner: OWNER,
  });
  return acquisition;
}

async function exactDomValue(cdp, selector) {
  return cdp.evaluate(`(() => {
    const nodes = document.querySelectorAll(${JSON.stringify(selector)});
    const node = nodes.length === 1 ? nodes[0] : null;
    return {
      count: nodes.length,
      value: node instanceof HTMLInputElement || node instanceof HTMLTextAreaElement || node instanceof HTMLSelectElement
        ? node.value
        : null,
      active: node !== null && document.activeElement === node,
    };
  })()`);
}

async function newRequestComposerSurface(cdp) {
  return cdp.evaluate(`(() => {
    const composers = document.querySelectorAll("section.composer");
    const prompts = document.querySelectorAll("section.composer textarea#prompt");
    const sends = document.querySelectorAll('section.composer button[data-action="send"]');
    const composer = composers.length === 1 && composers[0] instanceof HTMLElement
      ? composers[0]
      : null;
    const prompt = prompts.length === 1 && prompts[0] instanceof HTMLTextAreaElement
      ? prompts[0]
      : null;
    const send = sends.length === 1 && sends[0] instanceof HTMLButtonElement
      ? sends[0]
      : null;
    const runTargetText = composer?.getAttribute("data-run-target") ?? null;
    let renderedRunTarget = null;
    let runTargetParseError = null;
    if (runTargetText !== null) {
      try { renderedRunTarget = JSON.parse(runTargetText); }
      catch (error) { runTargetParseError = String(error); }
    }
    return {
      composer_count: composers.length,
      prompt_count: prompts.length,
      prompt_value: prompt?.value ?? null,
      prompt_disabled: prompt?.disabled ?? null,
      send_count: sends.length,
      send_disabled: send?.disabled ?? null,
      send_title: send?.title ?? null,
      send_aria_label: send?.getAttribute("aria-label") ?? null,
      run_strip_count: document.querySelectorAll("section.run-strip").length,
      visible_stop_count: document.querySelectorAll('section.run-strip button[data-action="cancel-run"]').length,
      rendered_run_target: renderedRunTarget,
      run_target_parse_error: runTargetParseError,
    };
  })()`);
}

export function case52NewRequestComposerSurfaceReady(surface, expectedPrompt, expectedRunTarget) {
  if (typeof expectedPrompt !== "string") {
    throw new TypeError("case5_2 expected composer prompt must be a string");
  }
  if (expectedRunTarget === null || typeof expectedRunTarget !== "object" || Array.isArray(expectedRunTarget)) {
    throw new TypeError("case5_2 expected run target must be an object");
  }
  return surface?.composer_count === 1
    && surface.prompt_count === 1
    && surface.prompt_value === expectedPrompt
    && surface.prompt_disabled === false
    && surface.send_count === 1
    && surface.send_disabled === false
    && surface.send_title === "送信"
    && surface.send_aria_label === "送信"
    && surface.run_strip_count === 0
    && surface.visible_stop_count === 0
    && surface.run_target_parse_error === null
    && isDeepStrictEqual(surface.rendered_run_target, expectedRunTarget);
}

async function transcriptExports(workspace) {
  const directory = path.join(workspace, ".moyai", "transcript-exports");
  try {
    const directoryItem = await lstat(directory);
    if (!directoryItem.isDirectory() || directoryItem.isSymbolicLink()) {
      throw productFailure("case5_2-transcript-export-owner", "transcript export path is not a physical directory", { directory });
    }
    const entries = await readdir(directory, { withFileTypes: true });
    const files = [];
    for (const entry of entries) {
      if (!entry.isFile() || entry.isSymbolicLink() || !entry.name.toLowerCase().endsWith(".md")) continue;
      const candidate = path.join(directory, entry.name);
      const bytes = await readFile(candidate);
      files.push({ path: candidate, name: entry.name, bytes });
    }
    return files.sort((left, right) => left.name.localeCompare(right.name));
  } catch (error) {
    if (error?.code === "ENOENT") return [];
    throw error;
  }
}

async function exportCanonicalTranscript({ context, input, sink, sessionId, prompts }) {
  const before = await transcriptExports(context.paths.workspace);
  if (before.length !== 0) {
    throw productFailure("case5_2-transcript-export-not-fresh", "fresh case5_2 workspace already contained a transcript export", {
      paths: before.map((entry) => entry.path),
    });
  }
  const action = await recordTrustedClick({
    input,
    locator: EXPORT_TRANSCRIPT,
    action: "export-final-transcript",
    sink,
  });
  const observed = await waitForObservation({
    label: "case5_2 canonical transcript export",
    timeoutMs: 10_000,
    pollMs: 100,
    sample: () => transcriptExports(context.paths.workspace),
    accept: (files) => files.length === 1,
  });
  const [file] = observed.value;
  const text = file.bytes.toString("utf8").replaceAll("\r\n", "\n");
  const visiblePromptStages = Object.entries(prompts)
    .filter(([, prompt]) => text.includes(prompt.text.replaceAll("\r\n", "\n").trim()))
    .map(([stage]) => stage);
  if (!text.includes(sessionId) || !visiblePromptStages.includes("stage4")) {
    throw productFailure("case5_2-transcript-export-content", "visible transcript export did not preserve the same session and final prompt", {
      session_id: sessionId,
      visible_prompt_stages: visiblePromptStages,
      export_path: file.path,
    });
  }
  const evidence = await sink.writeBytes(`case5_2/transcript/${file.name}`, file.bytes);
  await sink.record("case5_2-transcript-exported", {
    action,
    source_path: file.path,
    evidence,
    session_id: sessionId,
    visible_prompt_stages: visiblePromptStages,
    elapsed_ms: observed.elapsed_ms,
  }, { phase: "executing", owner: OWNER });
  return { source_path: file.path, evidence, session_id: sessionId };
}

async function insertExactText({ cdp, input, locator, text, action, sink }) {
  await recordTrustedClick({ input, locator, action: `${action}-focus`, sink });
  const start = (await input.snapshotProbe()).sequence;
  const inserted = await input.insertText(locator, text);
  const snapshot = await input.snapshotProbe(start);
  const probe = assertTrustedTextInsertion(snapshot, {
    afterSequence: start,
    identity: locator.identity,
    text,
  });
  const dom = await exactDomValue(cdp, locator.selector);
  if (dom.count !== 1 || dom.value !== text) {
    throw productFailure("case5_2-text-input-drift", "trusted browser text insertion did not produce the byte-identical requested value", {
      action,
      expected_sha256: sha256(Buffer.from(text, "utf8")),
      expected_bytes: Buffer.byteLength(text, "utf8"),
      dom,
    });
  }
  await sink.record("case5_2-trusted-text", {
    action,
    input_kind: "browser_trusted",
    inserted,
    probe,
    value_sha256: sha256(Buffer.from(text, "utf8")),
    value_bytes: Buffer.byteLength(text, "utf8"),
  }, { phase: "executing", owner: OWNER });
  return { inserted, probe, dom };
}

async function replaceExactText({ cdp, input, locator, text, action, sink }) {
  const initial = await exactDomValue(cdp, locator.selector);
  if (initial.count !== 1) throw new Error(`${action} target cardinality drifted`);
  if (initial.value === text) return { changed: false, initial };
  await recordTrustedClick({ input, locator, action: `${action}-focus`, sink });
  await input.keyDown("Control");
  try { await input.pressKey("a"); }
  finally { await input.keyUp("Control"); }
  const start = (await input.snapshotProbe()).sequence;
  const inserted = await input.insertText(locator, text);
  const snapshot = await input.snapshotProbe(start);
  const probe = assertTrustedTextInsertion(snapshot, {
    afterSequence: start,
    identity: locator.identity,
    text,
  });
  const final = await exactDomValue(cdp, locator.selector);
  if (final.count !== 1 || final.value !== text) {
    throw productFailure("case5_2-settings-input-drift", "trusted Settings input did not produce the exact value", { action, initial, final });
  }
  await sink.record("case5_2-trusted-settings-text", { action, initial, final, inserted, probe }, { phase: "executing", owner: OWNER });
  return { changed: true, initial, final, inserted, probe };
}

function mainConnectionDesired(options) {
  return {
    baseUrl: options.providerBaseUrl,
    model: options.mainModel,
    providerProfile: options.providerProfile,
    apiKeyEnv: MAIN_API_KEY_ENV,
  };
}

function configValues(projection, overrides = {}) {
  const fields = Array.isArray(projection?.config_fields) ? projection.config_fields : [];
  return fields.map((field) => ({
    key: field.key,
    text: Object.hasOwn(overrides, field.key) ? overrides[field.key] : field.value,
  }));
}

export function case52ExpectedMainGlobalSave(surface, options) {
  const target = surface?.projection?.config_target;
  if (target === null || typeof target !== "object") {
    throw new TypeError("case5_2 Main Preferences save requires one config target");
  }
  const desired = mainConnectionDesired(options);
  return {
    command: "save_global_config",
    args: {
      values: configValues(surface.projection, {
        "model.base_url": desired.baseUrl,
        "model.model": desired.model,
        "model.provider_profile": desired.providerProfile,
        "model.api_key_env": desired.apiKeyEnv,
      }),
      expectedTarget: structuredClone(target),
    },
  };
}

export function case52ExpectedSideGlobalSave(surface, options) {
  const target = surface?.projection?.config_target;
  if (target === null || typeof target !== "object") {
    throw new TypeError("case5_2 Side Chat Settings save requires one config target");
  }
  return {
    command: "save_global_config",
    args: {
      values: configValues(surface.projection, {
        "side_chat.base_url": options.providerBaseUrl,
        "side_chat.model": options.sideModel,
        "side_chat.provider_profile": options.providerProfile,
      }),
      expectedTarget: structuredClone(target),
    },
  };
}

async function observeMainSettings(cdp) {
  return cdp.evaluate(`(async () => {
    const invoke = window.__TAURI_INTERNALS__?.invoke;
    if (typeof invoke !== 'function') throw new Error('tauri-invoke-unavailable');
    const projection = await invoke('desktop_state');
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    const visible = (node) => {
      if (!(node instanceof HTMLElement) || !node.isConnected) return false;
      const style = getComputedStyle(node);
      const rect = node.getBoundingClientRect();
      return style.display !== 'none' && style.visibility !== 'hidden' && Number(style.opacity) !== 0
        && rect.width > 0 && rect.height > 0 && node.closest('[hidden], [inert], [aria-hidden="true"]') === null;
    };
    const one = (selector) => {
      const nodes = document.querySelectorAll(selector);
      return { count: nodes.length, node: nodes.length === 1 ? nodes[0] : null };
    };
    const control = (selector) => {
      const found = one(selector);
      const node = found.node;
      return {
        count: found.count,
        visible: visible(node),
        enabled: (node instanceof HTMLInputElement || node instanceof HTMLSelectElement)
          && !node.disabled && (!(node instanceof HTMLInputElement) || !node.readOnly),
        value: node instanceof HTMLInputElement || node instanceof HTMLSelectElement ? node.value : null,
        options: node instanceof HTMLSelectElement ? Array.from(node.options).map((option) => option.value) : [],
      };
    };
    const button = (selector) => {
      const found = one(selector);
      return {
        count: found.count,
        visible: visible(found.node),
        enabled: found.node instanceof HTMLButtonElement
          && !found.node.disabled
          && found.node.getAttribute('aria-disabled') !== 'true',
      };
    };
    const settings = one('[role="dialog"][aria-labelledby="config-dialog-title"]');
    const details = one('[role="dialog"][aria-labelledby="config-dialog-title"] details[data-details-key="main-provider-manual-model"]');
    return {
      projection,
      settings: { count: settings.count, visible: visible(settings.node) },
      base: control(${JSON.stringify(MAIN_BASE_URL.selector)}),
      profile: control(${JSON.stringify(MAIN_PROVIDER_PROFILE.selector)}),
      manual: control(${JSON.stringify(MAIN_MANUAL_MODEL.selector)}),
      api_key_env: control(${JSON.stringify(MAIN_API_KEY.selector)}),
      details: {
        count: details.count,
        visible: visible(details.node),
        open: details.node instanceof HTMLDetailsElement ? details.node.open : null,
      },
      dirty: Array.from(document.querySelectorAll('[role="dialog"][aria-labelledby="config-dialog-title"] .dirty-badge.visible')).filter(visible).length === 1,
      save: button(${JSON.stringify(SAVE_GLOBAL_CONFIG.selector)}),
      close: button(${JSON.stringify(CLOSE_SETTINGS.selector)}),
      fatal_count: Array.from(document.querySelectorAll('.fatal')).filter(visible).length,
      recoverable_error_count: Array.from(document.querySelectorAll('.ui-error-notice')).filter(visible).length,
      validation_error_count: Array.from(document.querySelectorAll('.validation.error')).filter(visible).length,
    };
  })()`);
}

function mainSettingsErrorFree(surface) {
  return surface?.fatal_count === 0
    && surface?.recoverable_error_count === 0
    && surface?.validation_error_count === 0;
}

function exactMainSettingsControls(surface, desired, { dirty }) {
  return surface?.projection?.overlay === "config"
    && surface?.settings?.count === 1
    && surface.settings.visible === true
    && surface?.base?.count === 1
    && surface.base.visible === true
    && surface.base.enabled === true
    && surface.base.value === desired.baseUrl
    && surface?.profile?.count === 1
    && surface.profile.visible === true
    && surface.profile.enabled === true
    && surface.profile.value === desired.providerProfile
    && surface.profile.options.includes(desired.providerProfile)
    && surface?.manual?.count === 1
    && surface.manual.enabled === true
    && surface.manual.value === desired.model
    && surface?.api_key_env?.count === 1
    && surface.api_key_env.visible === true
    && surface.api_key_env.enabled === true
    && surface.api_key_env.value === desired.apiKeyEnv
    && surface?.dirty === dirty
    && surface?.save?.count === 1
    && surface.save.visible === true
    && surface.save.enabled === dirty
    && surface?.close?.count === 1
    && surface.close.visible === true
    && mainSettingsErrorFree(surface);
}

function sameConfigTargetOwner(current, baseline) {
  return current?.workspacePath === baseline?.workspacePath
    && current?.sessionId === baseline?.sessionId;
}

function advancedConfigTarget(current, baseline) {
  return sameConfigTargetOwner(current, baseline)
    && typeof current?.configGeneration === "string"
    && current.configGeneration.length > 0
    && current.configGeneration !== baseline?.configGeneration;
}

export function case52MainProviderSelectionKeys(value) {
  if (value === LM_STUDIO_PROFILE) return ["Home"];
  if (value === OPENAI_COMPATIBLE_PROFILE) return ["Home", "ArrowDown"];
  throw new TypeError(`unsupported case5_2 Main provider profile selection: ${value}`);
}

async function selectMainProviderProfile({ cdp, input, sink, expected }) {
  const initial = await exactDomValue(cdp, MAIN_PROVIDER_PROFILE.selector);
  if (initial.count !== 1) throw new Error("Main provider profile target cardinality drifted");
  const alternate = expected === LM_STUDIO_PROFILE ? OPENAI_COMPATIBLE_PROFILE : LM_STUDIO_PROFILE;
  if (initial.value !== expected) {
    throw productFailure("case5_2-main-provider-profile-baseline", "Main provider profile did not begin at the expected neutral-fixture value", {
      expected,
      initial,
    });
  }
  const select = async ({ keys, value, action }) => {
    const focused = await recordTrustedClick({ input, locator: MAIN_PROVIDER_PROFILE, action: `${action}-focus`, sink });
    const start = (await input.snapshotProbe()).sequence;
    for (const key of keys) await input.pressKey(key);
    await input.pressKey("Enter");
    const snapshot = await input.snapshotProbe(start);
    const probe = assertTrustedProbeSequence(snapshot, {
      afterSequence: start,
      expected: [{ type: "change", identity: MAIN_PROVIDER_PROFILE.identity }],
    });
    const settled = await waitForMainPreferencesObservation({
      action,
      label: `${action} exact value`,
      timeoutMs: 10_000,
      pollMs: 100,
      sample: () => exactDomValue(cdp, MAIN_PROVIDER_PROFILE.selector),
      accept: (observation) => observation.count === 1 && observation.value === value,
    });
    const evidence = { action, keys, value, focused, probe, final: settled.value };
    await sink.record("case5_2-trusted-main-provider-profile", evidence, { phase: "executing", owner: OWNER });
    return evidence;
  };
  const selections = [await select({
    keys: case52MainProviderSelectionKeys(alternate),
    value: alternate,
    action: "main-provider-profile-alternate",
  })];
  selections.push(await select({
    keys: case52MainProviderSelectionKeys(expected),
    value: expected,
    action: "main-provider-profile-final",
  }));
  return { initial, alternate, selections, final: await exactDomValue(cdp, MAIN_PROVIDER_PROFILE.selector) };
}

export async function settleCase52MainCommandProbe({
  commandProbe,
  sink,
  cleanupFailures,
  primaryError = null,
}) {
  if (commandProbe === null || typeof commandProbe?.remove !== "function") {
    throw new TypeError("case5_2 Main command probe is required");
  }
  if (sink === null || typeof sink?.record !== "function") {
    throw new TypeError("case5_2 Main command probe evidence sink is required");
  }
  if (!Array.isArray(cleanupFailures)) {
    throw new TypeError("case5_2 scenario cleanup failures must be an array");
  }
  let removal;
  try {
    removal = await commandProbe.remove();
    if (removal?.removed !== true
      || removal.probe_id !== commandProbe.probeId
      || !Number.isInteger(removal.sequence)
      || removal.sequence < 0) {
      throw Object.assign(
        new Error("Main Preferences command probe removal result was not exact"),
        {
          code: "desktop-command-probe-remove",
          evidence: removal ?? null,
        },
      );
    }
  } catch (error) {
    const failure = {
      owner: "desktop-command-probe",
      label: "main-provider-command-probe",
      ...errorObservation(error),
    };
    cleanupFailures.push(failure);
    if (primaryError !== null) {
      return {
        removal: null,
        cleanup_failure: structuredClone(failure),
        primary_error: errorObservation(primaryError),
      };
    }
    throw new DesktopE2eError(
      "harness",
      "case5_2-main-provider-command-probe-cleanup",
      "Main Preferences command probe removal did not settle exactly",
      failure,
    );
  }
  try {
    await sink.record("case5_2-main-provider-command-probe-settled", removal, { phase: "executing", owner: OWNER });
  } catch (error) {
    const failure = {
      owner: "evidence-sink",
      label: "main-provider-command-probe-settlement-evidence",
      ...errorObservation(error),
    };
    cleanupFailures.push(failure);
    if (primaryError !== null) {
      return {
        removal,
        cleanup_failure: structuredClone(failure),
        primary_error: errorObservation(primaryError),
      };
    }
    throw new DesktopE2eError(
      "harness",
      "case5_2-main-provider-command-probe-cleanup",
      "Main Preferences command probe settlement evidence was not recorded",
      failure,
    );
  }
  return {
    removal,
    cleanup_failure: null,
    primary_error: primaryError === null ? null : errorObservation(primaryError),
  };
}

async function configureMainConnectionViaGui({ cdp, input, sink, options, state }) {
  const desired = mainConnectionDesired(options);
  const commandProbe = new DesktopCommandProbe(cdp, {
    probeId: "case5-2-main-preferences",
    commands: ["save_global_config"],
  });
  let primaryError = null;
  try {
    await commandProbe.install();
    await recordTrustedClick({ input, locator: SHOW_SETTINGS, action: "open-main-settings", sink });
    const baseline = await waitForMainPreferencesObservation({
      action: "opening Main Preferences",
      label: "case5_2 Main Preferences baseline",
      timeoutMs: 30_000,
      pollMs: 100,
      sample: () => observeMainSettings(cdp),
      accept: (surface) => surface?.projection?.overlay === "config"
        && surface?.base?.value === GUI_CONNECTION_BASELINE_BASE_URL
        && surface?.profile?.value === options.providerProfile
        && surface?.api_key_env?.value === MAIN_API_KEY_ENV
        && surface?.dirty === false
        && mainSettingsErrorFree(surface),
    });
    const baselineTarget = structuredClone(baseline.value.projection.config_target);
    const profile = await selectMainProviderProfile({
      cdp,
      input,
      sink,
      expected: options.providerProfile,
    });
    await replaceExactText({
      cdp,
      input,
      locator: MAIN_BASE_URL,
      text: options.providerBaseUrl,
      action: "main-provider-base-url",
      sink,
    });
    let surface = await observeMainSettings(cdp);
    if (surface?.details?.open !== true) {
      await recordTrustedClick({ input, locator: MAIN_MANUAL_DETAILS, action: "open-main-provider-manual-model", sink });
      surface = (await waitForMainPreferencesObservation({
        action: "opening the Main manual-model controls",
        label: "case5_2 Main manual model input",
        timeoutMs: 10_000,
        pollMs: 100,
        sample: () => observeMainSettings(cdp),
        accept: (value) => value?.details?.open === true
          && value?.manual?.visible === true
          && value?.manual?.enabled === true,
      })).value;
    }
    await replaceExactText({
      cdp,
      input,
      locator: MAIN_MANUAL_MODEL,
      text: options.mainModel,
      action: "main-provider-model",
      sink,
    });
    const dirty = await waitForMainPreferencesObservation({
      action: "editing the Main connection draft",
      label: "case5_2 exact dirty Main Preferences",
      timeoutMs: 10_000,
      pollMs: 100,
      sample: () => observeMainSettings(cdp),
      accept: (value) => exactMainSettingsControls(value, desired, { dirty: true }),
    });
    const dirtyScreenshot = await captureScenarioScreenshot({
      cdp,
      sink,
      name: "case5_2-main-provider-preferences-dirty",
      owner: OWNER,
    });
    const expectedSave = case52ExpectedMainGlobalSave(dirty.value, options);
    const commandStart = (await commandProbe.snapshot()).sequence;
    await recordTrustedClick({ input, locator: SAVE_GLOBAL_CONFIG, action: "save-main-provider-global-config", sink });
    const saved = await waitForMainPreferencesObservation({
      action: "saving the Main connection",
      label: "case5_2 saved Main Preferences",
      timeoutMs: 60_000,
      pollMs: 100,
      sample: () => observeMainSettings(cdp),
      accept: (value) => exactMainSettingsControls(value, desired, { dirty: false })
        && advancedConfigTarget(value?.projection?.config_target, baselineTarget)
        && mainConfigurationFailures(value.projection, options).length === 0
        && configField(value.projection, "model.api_key_env") === MAIN_API_KEY_ENV,
    });
    const commandSnapshot = await waitForMainPreferencesObservation({
      action: "saving the Main connection command",
      label: "case5_2 exact Main Preferences save command",
      timeoutMs: 10_000,
      pollMs: 50,
      sample: () => commandProbe.snapshot(commandStart),
      accept: (snapshot) => snapshot.calls.length >= 1,
    });
    let saveCommand;
    try {
      saveCommand = assertExactDesktopCommandSequence(commandSnapshot.value, {
        afterSequence: commandStart,
        expected: [expectedSave],
      });
    } catch (error) {
      throw classifyCase52MainSaveCommandError(error);
    }
    const savedScreenshot = await captureScenarioScreenshot({
      cdp,
      sink,
      name: "case5_2-main-provider-preferences-saved",
      owner: OWNER,
    });
    await sink.record("case5_2-main-provider-preferences-saved", {
      input_kind: "browser_trusted",
      desired,
      baseline: baseline.value,
      profile_selection: profile,
      dirty: dirty.value,
      saved: saved.value,
      save_command: saveCommand,
      screenshots: { dirty: dirtyScreenshot, saved: savedScreenshot },
    }, { phase: "executing", owner: OWNER });
    await recordTrustedClick({ input, locator: CLOSE_SETTINGS, action: "close-main-settings", sink });
    await waitForMainPreferencesObservation({
      action: "closing Main Preferences",
      label: "case5_2 Main Preferences closed",
      timeoutMs: 30_000,
      pollMs: 100,
      sample: () => desktopProjection(cdp),
      accept: (projection) => projection?.overlay === "none"
        && projection?.confirmation_visible === false
        && projection?.confirmation == null,
    });
  } catch (error) {
    primaryError = error;
    throw error;
  } finally {
    await settleCase52MainCommandProbe({
      commandProbe,
      sink,
      cleanupFailures: state.scenarioCleanupFailures,
      primaryError,
    });
  }
}

async function observeSideSettings(cdp) {
  return cdp.evaluate(`(async () => {
    const invoke = window.__TAURI_INTERNALS__?.invoke;
    if (typeof invoke !== 'function') throw new Error('tauri-invoke-unavailable');
    const projection = await invoke('desktop_state');
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    const visible = (node) => {
      if (!(node instanceof HTMLElement) || !node.isConnected) return false;
      const style = getComputedStyle(node);
      const rect = node.getBoundingClientRect();
      return style.display !== 'none' && style.visibility !== 'hidden' && Number(style.opacity) !== 0
        && rect.width > 0 && rect.height > 0 && node.closest('[hidden], [inert], [aria-hidden="true"]') === null;
    };
    const viewportVisible = (node) => {
      if (!visible(node)) return false;
      const rect = node.getBoundingClientRect();
      return rect.bottom > 0 && rect.right > 0 && rect.top < innerHeight && rect.left < innerWidth;
    };
    const one = (selector) => {
      const nodes = document.querySelectorAll(selector);
      return { count: nodes.length, node: nodes.length === 1 ? nodes[0] : null };
    };
    const settings = one('[role="dialog"][aria-labelledby="config-dialog-title"]');
    const section = one('[role="dialog"][aria-labelledby="config-dialog-title"] section#settings-side-chat');
    const profile = one('select#side-chat-provider-profile[data-config-key="side_chat.provider_profile"]');
    const base = one('input#side-chat-base-url[data-config-key="side_chat.base_url"]');
    const manual = one('input#side-chat-model-manual[data-config-key="side_chat.model"]');
    const details = one('details[data-details-key="side-chat-manual-model"]');
    const save = one('[role="dialog"][aria-labelledby="config-dialog-title"] button[data-action="save-global-config"]');
    const sidePane = one('aside.side-chat-pane[data-pane-mode="side-chat"]');
    const deleteTrigger = one('aside.side-chat-pane[data-pane-mode="side-chat"] button[data-action="request-delete-side-chat"]');
    const deleteDialog = one('[role="alertdialog"][aria-labelledby="side-chat-delete-title"]');
    const deleteConfirm = one('[role="alertdialog"][aria-labelledby="side-chat-delete-title"] button[data-action="confirm-delete-side-chat"]');
    return {
      projection,
      settings: { count: settings.count, visible: visible(settings.node), viewport_visible: viewportVisible(settings.node) },
      section: { count: section.count, visible: visible(section.node), viewport_visible: viewportVisible(section.node) },
      profile: { count: profile.count, visible: visible(profile.node), viewport_visible: viewportVisible(profile.node), value: profile.node instanceof HTMLSelectElement ? profile.node.value : null, enabled: profile.node instanceof HTMLSelectElement && !profile.node.disabled, options: profile.node instanceof HTMLSelectElement ? Array.from(profile.node.options).map((option) => option.value) : [] },
      base: { count: base.count, visible: visible(base.node), viewport_visible: viewportVisible(base.node), value: base.node instanceof HTMLInputElement ? base.node.value : null, enabled: base.node instanceof HTMLInputElement && !base.node.disabled && !base.node.readOnly },
      manual: { count: manual.count, visible: visible(manual.node), viewport_visible: viewportVisible(manual.node), value: manual.node instanceof HTMLInputElement ? manual.node.value : null, enabled: manual.node instanceof HTMLInputElement && !manual.node.disabled && !manual.node.readOnly },
      details: { count: details.count, visible: visible(details.node), viewport_visible: viewportVisible(details.node), open: details.node instanceof HTMLDetailsElement ? details.node.open : null },
      dirty: Array.from(document.querySelectorAll('[role="dialog"][aria-labelledby="config-dialog-title"] .dirty-badge.visible')).filter(visible).length === 1,
      save: { count: save.count, visible: visible(save.node), enabled: save.node instanceof HTMLButtonElement && !save.node.disabled && save.node.getAttribute('aria-disabled') !== 'true' },
      side_pane: { count: sidePane.count, visible: visible(sidePane.node) },
      delete_trigger: { count: deleteTrigger.count, visible: visible(deleteTrigger.node), enabled: deleteTrigger.node instanceof HTMLButtonElement && !deleteTrigger.node.disabled },
      delete_dialog: { count: deleteDialog.count, visible: visible(deleteDialog.node) },
      delete_confirm: { count: deleteConfirm.count, visible: visible(deleteConfirm.node), enabled: deleteConfirm.node instanceof HTMLButtonElement && !deleteConfirm.node.disabled },
      fatal_count: Array.from(document.querySelectorAll('.fatal')).filter(visible).length,
      recoverable_error_count: Array.from(document.querySelectorAll('.ui-error-notice')).filter(visible).length,
      validation_error_count: Array.from(document.querySelectorAll('.validation.error')).filter(visible).length,
    };
  })()`);
}

function exactSideChatProjection(projection, options, sessionId) {
  const side = projection?.side_chat;
  return side?.configured === true
    && side.deleting === false
    && side.owner_session_id === sessionId
    && typeof side.chat_id === "string"
    && side.chat_id.length > 0
    && side.base_url === options.providerBaseUrl
    && side.model === options.sideModel
    && side.provider_profile === options.providerProfile
    && side.status === "idle"
    && side.phase === ""
    && side.last_error === ""
    && side.draft_text === ""
    && Array.isArray(side.messages)
    && side.messages.length === 0
    && side.can_send === true
    && side.can_cancel === false;
}

export function case52SideScreenshotSurfaceReady(surface, options, sessionId) {
  return exactSideChatProjection(surface?.projection, options, sessionId)
    && surface?.settings?.visible === true
    && surface.settings.viewport_visible === true
    && surface?.section?.visible === true
    && surface.section.viewport_visible === true
    && surface?.details?.open === true
    && surface?.profile?.visible === true
    && surface.profile.viewport_visible === true
    && surface.profile.value === options.providerProfile
    && surface?.base?.visible === true
    && surface.base.viewport_visible === true
    && surface.base.value === options.providerBaseUrl
    && surface?.manual?.visible === true
    && surface.manual.viewport_visible === true
    && surface.manual.value === options.sideModel;
}

async function waitSettingsOverlay(cdp, expected) {
  return waitForObservation({
    label: `case5_2 Settings overlay ${expected}`,
    timeoutMs: 30_000,
    pollMs: 100,
    sample: () => desktopProjection(cdp),
    accept: (projection) => projection?.overlay === expected
      && projection?.confirmation_visible === false
      && projection?.confirmation == null,
  });
}

async function openSideSettings({ cdp, input, sink }) {
  await recordTrustedClick({ input, locator: SHOW_SETTINGS, action: "open-settings", sink });
  await waitSettingsOverlay(cdp, "config");
  await recordTrustedClick({ input, locator: SIDE_SETTINGS_NAV, action: "navigate-side-chat-settings", sink });
  return waitForSideScreenshotObservation({
    action: "navigating to Side Chat Settings",
    label: "visible Side Chat Settings section",
    timeoutMs: 30_000,
    pollMs: 100,
    sample: () => observeSideSettings(cdp),
    accept: (surface) => surface.settings.count === 1
      && surface.settings.visible === true
      && surface.section.count === 1
      && surface.section.visible === true
      && surface.fatal_count === 0
      && surface.recoverable_error_count === 0,
  });
}

async function closeSettings({ cdp, input, sink }) {
  await recordTrustedClick({ input, locator: CLOSE_SETTINGS, action: "close-settings", sink });
  await waitSettingsOverlay(cdp, "none");
}

async function trustedSelectSideProviderProfile({ cdp, input, sink, options }) {
  const initial = await exactDomValue(cdp, SIDE_PROVIDER_PROFILE.selector);
  if (initial.count !== 1) throw new Error("Side Chat provider profile target cardinality drifted");
  if (initial.value === options.providerProfile) {
    const evidence = {
      changed: false,
      expected: options.providerProfile,
      initial,
      final: initial,
    };
    await sink.record("case5_2-side-provider-profile-observed", evidence, { phase: "executing", owner: OWNER });
    return evidence;
  }
  const selections = [];
  const select = async (key, expected, action) => {
    await recordTrustedClick({ input, locator: SIDE_PROVIDER_PROFILE, action: `${action}-focus`, sink });
    const start = (await input.snapshotProbe()).sequence;
    await input.pressKey(key);
    await input.pressKey("Enter");
    const snapshot = await input.snapshotProbe(start);
    const probe = assertTrustedProbeSequence(snapshot, {
      afterSequence: start,
      expected: [{ type: "change", identity: SIDE_PROVIDER_PROFILE.identity }],
    });
    const settled = await waitForObservation({
      label: `${action} exact value`,
      timeoutMs: 10_000,
      pollMs: 100,
      sample: () => exactDomValue(cdp, SIDE_PROVIDER_PROFILE.selector),
      accept: (value) => value.count === 1 && value.value === expected,
    });
    const evidence = { action, expected, probe, final: settled.value };
    selections.push(evidence);
    await sink.record("case5_2-trusted-side-provider-profile", evidence, { phase: "executing", owner: OWNER });
  };
  if (options.providerProfile === OPENAI_COMPATIBLE_PROFILE) {
    await select("o", OPENAI_COMPATIBLE_PROFILE, "side-chat-provider-profile-openai-compatible");
  } else {
    await select("l", LM_STUDIO_PROFILE, "side-chat-provider-profile-lm-studio");
  }
  const final = await exactDomValue(cdp, SIDE_PROVIDER_PROFILE.selector);
  if (final.count !== 1 || final.value !== options.providerProfile) {
    throw productFailure("case5_2-side-chat-provider-profile", "trusted Side Chat provider profile selection did not settle exactly", {
      expected: options.providerProfile,
      initial,
      selections,
      final,
    });
  }
  return { initial, selections, final };
}

export async function configureSideChat({
  cdp,
  input,
  sink,
  options,
  sessionId,
  evidenceName = "case5_2-side-chat-configured",
}) {
  const before = await desktopProjection(cdp);
  const prior = before?.side_chat?.configured === true ? structuredClone(before.side_chat) : null;
  const replaceBinding = prior !== null && !exactSideChatProjection(before, options, sessionId);
  const commandProbe = new DesktopCommandProbe(cdp, {
    probeId: "case5-2-side-settings",
    commands: ["save_global_config", "delete_side_chat", "ensure_side_chat"],
  });
  const expectedCommands = [];
  let primaryError = null;
  try {
    await commandProbe.install();
    const opened = await openSideSettings({ cdp, input, sink });
    const providerProfile = await trustedSelectSideProviderProfile({ cdp, input, sink, options });
    await replaceExactText({ cdp, input, locator: SIDE_BASE_URL, text: options.providerBaseUrl, action: "side-chat-base-url", sink });
    let surface = await observeSideSettings(cdp);
    if (surface.details.open !== true) {
      await recordTrustedClick({
        input,
        locator: SIDE_MANUAL_DETAILS,
        action: "open-side-chat-manual-model",
        sink,
        stableHitSamples: 3,
      });
      surface = (await waitForObservation({
        label: "Side Chat manual model input",
        timeoutMs: 10_000,
        pollMs: 100,
        sample: () => observeSideSettings(cdp),
        accept: (value) => value.details.open === true && value.manual.visible === true && value.manual.enabled === true,
      })).value;
    }
    await replaceExactText({ cdp, input, locator: SIDE_MANUAL_MODEL, text: options.sideModel, action: "side-chat-model", sink });
    const committable = await waitForObservation({
      label: "global Side Chat Settings save admission",
      timeoutMs: 10_000,
      pollMs: 100,
      sample: () => observeSideSettings(cdp),
      accept: (value) => value.profile.value === options.providerProfile
        && value.base.value === options.providerBaseUrl
        && value.manual.value === options.sideModel
        && value.save.count === 1 && value.save.visible === true
        && (value.dirty === false || value.save.enabled === true),
    });
    let saved = committable;
    if (committable.value.dirty) {
      const expectedSave = case52ExpectedSideGlobalSave(committable.value, options);
      expectedCommands.push(expectedSave);
      const baselineTarget = structuredClone(committable.value.projection.config_target);
      await recordTrustedClick({ input, locator: SAVE_GLOBAL_CONFIG, action: "save-global-side-chat-settings", sink });
      saved = await waitForObservation({
        label: "global Side Chat Settings saved",
        timeoutMs: 60_000,
        pollMs: 100,
        sample: () => observeSideSettings(cdp),
        accept: (value) => value.dirty === false
          && value.profile.value === options.providerProfile
          && value.base.value === options.providerBaseUrl
          && value.manual.value === options.sideModel
          && configField(value.projection, "side_chat.provider_profile") === options.providerProfile
          && configField(value.projection, "side_chat.base_url") === options.providerBaseUrl
          && configField(value.projection, "side_chat.model") === options.sideModel
          && advancedConfigTarget(value.projection.config_target, baselineTarget)
          && value.fatal_count === 0
          && value.recoverable_error_count === 0
          && value.validation_error_count === 0,
      });
    }
    await closeSettings({ cdp, input, sink });

    let deleted = null;
    if (replaceBinding) {
      let deleteSurface = await observeSideSettings(cdp);
      if (!deleteSurface.side_pane.visible) {
        await recordTrustedClick({ input, locator: SHOW_SIDE_CHAT, action: "show-existing-side-chat-for-replacement", sink });
        deleteSurface = (await waitForObservation({
          label: "existing Side Chat visible before replacement",
          timeoutMs: 10_000,
          pollMs: 100,
          sample: () => observeSideSettings(cdp),
          accept: (value) => value.side_pane.visible === true && value.delete_trigger.enabled === true,
        })).value;
      }
      const expectedDelete = {
        command: "delete_side_chat",
        args: {
          ownerSessionId: prior.owner_session_id,
          chatId: prior.chat_id,
          expectedGeneration: prior.generation,
        },
      };
      expectedCommands.push(expectedDelete);
      await recordTrustedClick({ input, locator: REQUEST_DELETE_SIDE_CHAT, action: "request-side-chat-replacement", sink });
      await waitForObservation({
        label: "Side Chat replacement confirmation",
        timeoutMs: 10_000,
        pollMs: 100,
        sample: () => observeSideSettings(cdp),
        accept: (value) => value.delete_dialog.visible === true && value.delete_confirm.enabled === true,
      });
      await recordTrustedClick({ input, locator: CONFIRM_DELETE_SIDE_CHAT, action: "confirm-side-chat-replacement", sink });
      deleted = await waitForObservation({
        label: "old Side Chat snapshot deleted before replacement",
        timeoutMs: 30_000,
        pollMs: 100,
        sample: () => desktopProjection(cdp),
        accept: (projection) => projection?.side_chat?.configured === false
          && projection.side_chat.owner_session_id === sessionId
          && projection.side_chat.chat_id === null
          && projection.side_chat.deleting === false,
      });
    }

    let materialized = null;
    if (prior === null || replaceBinding) {
      const ensureTarget = await desktopProjection(cdp);
      const expectedEnsure = {
        command: "ensure_side_chat",
        args: {
          ownerSessionId: sessionId,
          expectedConfigGeneration: ensureTarget.config_target.configGeneration,
        },
      };
      expectedCommands.push(expectedEnsure);
      await recordTrustedClick({ input, locator: SHOW_SIDE_CHAT, action: "materialize-side-chat-from-global-defaults", sink });
      materialized = await waitForObservation({
        label: "selected-session Side Chat snapshot materialized from global defaults",
        timeoutMs: 30_000,
        pollMs: 100,
        sample: () => observeSideSettings(cdp),
        accept: (value) => exactSideChatProjection(value.projection, options, sessionId)
          && value.side_pane.visible === true
          && value.fatal_count === 0
          && value.recoverable_error_count === 0,
      });
    }

    const commandObservation = await waitForObservation({
      label: "exact global Side Chat save and snapshot commands",
      timeoutMs: 10_000,
      pollMs: 50,
      sample: () => commandProbe.snapshot(),
      accept: (snapshot) => snapshot.calls.length >= expectedCommands.length,
    });
    const commands = assertExactDesktopCommandSequence(commandObservation.value, { expected: expectedCommands });

    await openSideSettings({ cdp, input, sink });
    surface = await observeSideSettings(cdp);
    if (surface.details.open !== true) {
      await recordTrustedClick({
        input,
        locator: SIDE_MANUAL_DETAILS,
        action: "show-materialized-side-chat-manual-model",
        sink,
        stableHitSamples: 3,
      });
    }
    const screenshotSurface = await waitForSideScreenshotObservation({
      action: "showing the materialized Side Chat global defaults",
      label: "visible global Side Chat Settings and selected-session snapshot",
      timeoutMs: 10_000,
      pollMs: 100,
      sample: () => observeSideSettings(cdp),
      accept: (value) => case52SideScreenshotSurfaceReady(value, options, sessionId),
    });
    const screenshot = await captureScenarioScreenshot({ cdp, sink, name: evidenceName, owner: OWNER });
    await sink.record("case5_2-side-chat-configured", {
      owner_session_id: sessionId,
      base_url: options.providerBaseUrl,
      model: options.sideModel,
      provider_profile: options.providerProfile,
      provider_profile_selection: providerProfile,
      prior_binding: prior,
      replaced_binding: replaceBinding,
      opened_surface: opened.value,
      saved_surface: saved.value,
      deleted_surface: deleted?.value ?? null,
      materialized_surface: materialized?.value ?? null,
      command_evidence: commands,
      screenshot_surface: screenshotSurface.value,
      screenshot,
    }, { phase: "executing", owner: OWNER });
    await closeSettings({ cdp, input, sink });
    const projection = await desktopProjection(cdp);
    if (!exactSideChatProjection(projection, options, sessionId)) {
      throw productFailure("case5_2-side-chat-persistence", "the selected-session Side Chat snapshot did not remain attached after global Settings closed", { projection: projection.side_chat });
    }
    return { prior, replaceBinding, commands, projection: projection.side_chat };
  } catch (error) {
    primaryError = error;
    throw error;
  } finally {
    try {
      const removal = await commandProbe.remove();
      await sink.record("case5_2-side-chat-command-probe-settled", removal, { phase: "executing", owner: OWNER });
    } catch (error) {
      if (primaryError === null) {
        throw new DesktopE2eError(
          "harness",
          "case5_2-side-chat-command-probe-cleanup",
          "Side Chat Settings command probe did not settle",
          errorObservation(error),
        );
      }
    }
  }
}

async function verifyRestoredSideChat({ cdp, input, sink, options, sessionId }) {
  const initial = await desktopProjection(cdp);
  if (!exactSideChatProjection(initial, options, sessionId)) {
    throw productFailure("case5_2-side-chat-restart", "Side Chat configuration was not restored for the same Project Chat after Desktop restart", { side_chat: initial.side_chat });
  }
  const visible = await openSideSettings({ cdp, input, sink });
  if (!exactSideChatProjection(visible.value.projection, options, sessionId)
    || visible.value.base.value !== options.providerBaseUrl
    || visible.value.profile.value !== options.providerProfile) {
    throw productFailure("case5_2-side-chat-restart-settings", "reopened Settings did not display the persisted global Side Chat defaults beside the restored session snapshot", { surface: visible.value });
  }
  if (visible.value.details.open !== true) {
    await recordTrustedClick({
      input,
      locator: SIDE_MANUAL_DETAILS,
      action: "reopen-side-chat-manual-model",
      sink,
      stableHitSamples: 3,
    });
  }
  const exact = await waitForSideScreenshotObservation({
    action: "showing the restored Side Chat model",
    label: "restarted Side Chat Settings exact values",
    timeoutMs: 10_000,
    pollMs: 100,
    sample: () => observeSideSettings(cdp),
    accept: (value) => case52SideScreenshotSurfaceReady(value, options, sessionId),
  });
  const screenshot = await captureScenarioScreenshot({ cdp, sink, name: "case5_2-side-chat-restored", owner: OWNER });
  await sink.record("case5_2-side-chat-restored", { surface: exact.value, screenshot }, { phase: "executing", owner: OWNER });
  await closeSettings({ cdp, input, sink });
}

function configField(projection, key) {
  const rows = Array.isArray(projection?.config_fields) ? projection.config_fields.filter((row) => row?.key === key) : [];
  return rows.length === 1 ? rows[0].value : null;
}

function mainConfigurationFailures(projection, options, { sessionRequired = false } = {}) {
  const failures = [];
  const expectedFields = new Map([
    ["model.base_url", options.providerBaseUrl],
    ["model.model", options.mainModel],
    ["model.provider_profile", options.providerProfile],
    ["model.request_timeout_ms", String(QUALITY_REQUEST_TIMEOUT_MS)],
    ["model.max_retries", "0"],
    ["model.context_window", String(QUALITY_CONTEXT_WINDOW)],
    ["model.supports_tools", "true"],
    ["model.parallel_tool_calls", "false"],
    ["permissions.access_mode", "auto_review"],
    ["multi_agent.enabled", "false"],
    ["docling.enabled", "false"],
    ["mcp.enabled", "false"],
  ]);
  for (const [key, expected] of expectedFields) {
    const actual = configField(projection, key);
    if (actual !== expected) failures.push({ key, expected, actual });
  }
  for (const [key, expected, actual] of [
    ["provider_effective_base_url", options.providerBaseUrl, projection?.provider_effective_base_url],
    ["provider_effective_model_id", options.mainModel, projection?.provider_effective_model_id],
    ["provider_effective_context_window", String(QUALITY_CONTEXT_WINDOW), projection?.provider_effective_context_window],
    ["provider_effective_profile", options.providerProfile, projection?.provider_effective_profile],
  ]) {
    if (actual !== expected) failures.push({ key, expected, actual });
  }
  if (sessionRequired) {
    const settings = projection?.session_settings;
    for (const [key, expected, actual] of [
      ["session_settings.available", true, settings?.available],
      ["session_settings.base_url", options.providerBaseUrl, settings?.base_url],
      ["session_settings.model", options.mainModel, settings?.model],
      ["session_settings.provider_profile", options.providerProfile, settings?.provider_profile],
      ["session_settings.access_mode", "auto_review", settings?.access_mode],
      ["session_settings.context_window", "", settings?.context_window],
      ["session_settings.context_window_inherited", true, settings?.context_window_inherited],
    ]) {
      if (actual !== expected) failures.push({ key, expected, actual });
    }
  }
  return failures;
}

function historyRows(projection) {
  return Array.isArray(projection?.transcript_rows) ? structuredClone(projection.transcript_rows) : [];
}

function normalizedRepeatKey(row) {
  const value = [row?.title, row?.body, row?.step].filter((entry) => typeof entry === "string" && entry.trim().length > 0).join("\n").trim();
  return value.length > 0 ? value.replace(/\s+/g, " ").slice(0, 2000) : null;
}

function repetitionObservation(projection, expectedPrompt) {
  const rows = Array.isArray(projection?.transcript_rows) ? projection.transcript_rows : [];
  let userIndex = -1;
  for (let index = rows.length - 1; index >= 0; index -= 1) {
    if (rows[index]?.row_kind === "user" && rows[index]?.body === expectedPrompt) {
      userIndex = index;
      break;
    }
  }
  const current = userIndex >= 0 ? rows.slice(userIndex + 1) : [];
  const counts = new Map();
  const readCounts = new Map();
  for (const row of current) {
    if (!new Set(["tool", "editing", "work_summary_running"]).has(row?.row_kind)) continue;
    const key = normalizedRepeatKey(row);
    if (key === null) continue;
    counts.set(key, (counts.get(key) ?? 0) + 1);
    if (/(?:read|読み|inspect|get-content|source range|line range)/i.test(key)) {
      readCounts.set(key, (readCounts.get(key) ?? 0) + 1);
    }
  }
  const ranked = (map) => [...map.entries()].sort((left, right) => right[1] - left[1] || left[0].localeCompare(right[0]));
  return {
    repeated_next_action_count: ranked(counts)[0]?.[1] ?? 0,
    repeated_source_read_count: ranked(readCounts)[0]?.[1] ?? 0,
    repeated_next_actions: ranked(counts).slice(0, 5).map(([key, count]) => ({ key, count })),
    repeated_source_reads: ranked(readCounts).slice(0, 5).map(([key, count]) => ({ key, count })),
  };
}

export function case52ProviderControlTokenLeaks(projection) {
  const markers = ["<|im_start|>", "<|im_end|>"];
  const rows = Array.isArray(projection?.transcript_rows) ? projection.transcript_rows : [];
  return rows.flatMap((row, rowIndex) => {
    if (row?.row_kind !== "assistant" || typeof row.body !== "string") return [];
    const detected = markers.filter((marker) => row.body.includes(marker));
    if (detected.length === 0) return [];
    const firstOffset = Math.min(...detected.map((marker) => row.body.indexOf(marker)));
    return [{
      row_index: rowIndex,
      stable_history_identity: row.stable_history_identity ?? null,
      markers: detected,
      body_sha256: sha256(Buffer.from(row.body, "utf8")),
      body_size_bytes: Buffer.byteLength(row.body, "utf8"),
      bounded_excerpt: row.body.slice(Math.max(0, firstOffset - 120), firstOffset + 240),
    }];
  });
}

export function case52ProviderControlTokenLeakEvidence(projection, leaks) {
  const session = selectedSessionRow(projection);
  return {
    schema_version: "desktop-e2e.case5_2-provider-control-token-leak.v1",
    run_status_key: projection?.run_status_key ?? null,
    run_phase: projection?.run_phase ?? null,
    task_activity_state: projection?.task_activity_state ?? null,
    busy: projection?.busy ?? null,
    agent_tree_active: projection?.agent_tree_active ?? null,
    selected_navigation: selectedNavigationIdentity(projection),
    selected_session: session === null ? null : {
      session_id: session.session_id ?? null,
      status: session.status ?? null,
      loaded_status: session.loaded_status ?? null,
      active_turn_id: session.active_turn_id ?? null,
      admission_revision: session.admission_revision ?? null,
    },
    leaks: structuredClone(leaks),
  };
}

function requiredArtifactCount(stageId, manifest, reference) {
  const current = fileMap(manifest.files);
  if (stageId === "stage1") {
    return ["README.md", "basic_design.md", "detail_design.md", "evidence_matrix.md"]
      .filter((name) => (current.get(name)?.bytes ?? 0) > 0 && manifest.diff.added.includes(name)).length;
  }
  if (stageId === "stage2") {
    return (current.get("cancel_contract.md")?.bytes ?? 0) > 0 ? 1 : 0;
  }
  const before = fileMap(reference.files);
  return [...current.entries()].filter(([name, row]) => {
    if (REQUIRED_DOCUMENTS.includes(name) || name === "task.md") return false;
    return !before.has(name) || before.get(name).sha256 !== row.sha256;
  }).length;
}

async function stopNonConvergentRun({ cdp, input, sink, stage, evidence }) {
  const before = await captureScenarioScreenshot({ cdp, sink, name: `case5_2-${stage}-nonconvergent`, owner: OWNER });
  const stop = await recordTrustedClick({ input, locator: STOP, action: `${stage}-visible-stop`, sink });
  const terminal = await waitForObservation({
    label: `${stage} interrupted terminal after visible Stop`,
    timeoutMs: 120_000,
    pollMs: 250,
    sample: () => desktopProjection(cdp),
    accept: (projection) => projection?.run_status_key === "cancelled"
      && projection?.task_activity_state === "idle"
      && projection?.busy === false
      && projection?.agent_tree_active === false,
  });
  const after = await captureScenarioScreenshot({ cdp, sink, name: `case5_2-${stage}-interrupted`, owner: OWNER });
  const record = { stage, evidence, stop, terminal: terminal.value, screenshots: { before, after } };
  await sink.record("case5_2-nonconvergence-stop", record, { phase: "executing", owner: OWNER });
  throw productFailure("case5_2-nonconvergent", `${stage} met the specified non-convergence cutoff and was visibly stopped without steering`, record);
}

async function stopProviderControlTokenLeak({ cdp, input, sink, stage, projection, leaks }) {
  const minimizedProjection = case52ProviderControlTokenLeakEvidence(projection, leaks);
  let projectionIdentity = null;
  let projectionError = null;
  try {
    projectionIdentity = await sink.writeJson(
      `case5_2/projections/${stage}-provider-control-token-leak.json`,
      minimizedProjection,
    );
  } catch (error) {
    projectionError = errorObservation(error);
  }
  let before = null;
  let screenshotError = null;
  try {
    before = await captureScenarioScreenshot({
      cdp,
      sink,
      name: `case5_2-${stage}-provider-control-token-leak`,
      owner: OWNER,
    });
  } catch (error) {
    screenshotError = errorObservation(error);
  }
  let stop = null;
  let stopError = null;
  let terminal = null;
  try {
    stop = await recordTrustedClick({ input, locator: STOP, action: `${stage}-provider-control-token-leak-visible-stop`, sink });
    terminal = (await waitForObservation({
      label: `${stage} interrupted terminal after provider control-token leak`,
      timeoutMs: 120_000,
      pollMs: 250,
      sample: () => desktopProjection(cdp),
      accept: (value) => value?.run_status_key === "cancelled"
        && value?.task_activity_state === "idle"
        && value?.busy === false
        && value?.agent_tree_active === false,
    })).value;
  } catch (error) {
    stopError = errorObservation(error);
  }
  const evidence = {
    stage,
    leaks,
    projection: projectionIdentity,
    projection_error: projectionError,
    screenshot: before,
    screenshot_error: screenshotError,
    visible_stop_count: stop === null ? 0 : 1,
    stop,
    stop_error: stopError,
    terminal,
    record_error: null,
  };
  try {
    await sink.record("case5_2-provider-control-token-leak", evidence, { phase: "executing", owner: OWNER });
  } catch (error) {
    evidence.record_error = errorObservation(error);
  }
  throw case52ProviderControlTokenLeakFailure(stage, evidence);
}

async function waitForStageTerminal({ context, cdp, input, sink, baseline, referenceManifest, stage, expectedSessionId, expectedTurnId, expectedPrompt }) {
  const started = Date.now();
  let nextManifestAt = started;
  let nextProgressAt = started + PROGRESS_EVIDENCE_MS;
  let monitor = {
    required_artifact_count: 0,
    repeated_next_action_count: 0,
    repeated_source_read_count: 0,
    repeated_next_actions: [],
    repeated_source_reads: [],
  };
  let lastProjection = null;
  while (Date.now() - started < STAGE_TIMEOUT_MS) {
    const projection = await desktopProjection(cdp);
    lastProjection = projection;
    const providerControlTokenLeaks = case52ProviderControlTokenLeaks(projection);
    if (providerControlTokenLeaks.length > 0) {
      await stopProviderControlTokenLeak({
        cdp,
        input,
        sink,
        stage: stage.id,
        projection,
        leaks: providerControlTokenLeaks,
      });
    }
    const classified = classifyCase52NormalTerminal(projection, {
      expectedSessionId,
      expectedTurnId,
      expectedPrompt,
      minimumCompletedSummaryCount: stage.minimumSummaries,
    });
    if (classified.decision === "pass") {
      return { projection, elapsed_ms: Date.now() - started, terminal_failures: [], monitor };
    }
    if (classified.decision === "fail") {
      throw productFailure("case5_2-stage-terminal", `${stage.id} reached a non-success terminal or immutable owner drift`, {
        stage: stage.id,
        failures: classified.failures,
        projection,
      });
    }
    const now = Date.now();
    if (now >= nextManifestAt) {
      const manifest = await captureWorkspaceManifest({ context, baseline, stage: `${stage.id}-progress` });
      const repeated = repetitionObservation(projection, expectedPrompt);
      monitor = {
        required_artifact_count: requiredArtifactCount(stage.id, manifest, referenceManifest),
        ...repeated,
      };
      const cutoff = classifyCase52NonConvergence({
        elapsedMs: now - started,
        requiredArtifactCount: monitor.required_artifact_count,
        repeatedNextActionCount: monitor.repeated_next_action_count,
        repeatedSourceReadCount: monitor.repeated_source_read_count,
      });
      if (cutoff.stop) {
        await stopNonConvergentRun({ cdp, input, sink, stage: stage.id, evidence: { elapsed_ms: now - started, monitor, cutoff } });
      }
      nextManifestAt = now + MANIFEST_POLL_MS;
    }
    if (now >= nextProgressAt) {
      await sink.record("case5_2-stage-progress", {
        stage: stage.id,
        elapsed_ms: now - started,
        run_status_key: projection.run_status_key,
        run_phase: projection.run_phase,
        run_active_step: projection.run_active_step,
        token_meter_label: projection.token_meter_label,
        task_activity_state: projection.task_activity_state,
        monitor,
      }, { phase: "executing", owner: OWNER });
      nextProgressAt = now + PROGRESS_EVIDENCE_MS;
    }
    await delay(STAGE_POLL_MS);
  }
  throw productFailure("case5_2-stage-timeout", `${stage.id} did not reach a normal terminal before the bounded observation ceiling`, {
    elapsed_ms: Date.now() - started,
    last_projection: lastProjection,
    monitor,
  });
}

function case52AdmissionRevision(value) {
  if (typeof value !== "string" || !/^(0|[1-9]\d*)$/.test(value)) return null;
  try {
    const revision = BigInt(value);
    return revision <= 18_446_744_073_709_551_615n ? revision : null;
  } catch {
    return null;
  }
}

export function case52NewTurnAcquisitionAccepted(projection, {
  expectedSessionId = null,
  previousExpectedState = null,
} = {}) {
  const row = selectedSessionRow(projection);
  const expectedState = projection?.run_target?.expectedState;
  const sessionAccepted = typeof row?.session_id === "string"
    && row.session_id.length > 0
    && (expectedSessionId === null || row.session_id === expectedSessionId);
  const rowRevision = case52AdmissionRevision(row?.admission_revision);
  const turnRevision = case52AdmissionRevision(expectedState?.admissionRevision);
  const turnAccepted = row?.status === "running"
    && row.loaded_status === "active"
    && typeof row.active_turn_id === "string"
    && row.active_turn_id.length > 0
    && expectedState?.kind === "turn"
    && expectedState.turnId === row.active_turn_id
    && rowRevision !== null
    && turnRevision === rowRevision;
  if (!sessionAccepted || !turnAccepted || previousExpectedState === null) {
    return sessionAccepted && turnAccepted;
  }

  const previousRevision = case52AdmissionRevision(previousExpectedState?.admissionRevision);
  return previousExpectedState?.kind === "idle"
    && (previousExpectedState.latestTurnId === null
      || (typeof previousExpectedState.latestTurnId === "string"
        && previousExpectedState.latestTurnId.length > 0
        && previousExpectedState.latestTurnId !== row.active_turn_id))
    && previousRevision !== null
    && rowRevision === previousRevision + 1n;
}

async function waitForTurnAcquisition(cdp, { expectedSessionId, previousExpectedState }) {
  return waitForObservation({
    label: "case5_2 trusted Send turn acquisition",
    timeoutMs: 30_000,
    pollMs: 100,
    sample: () => desktopProjection(cdp),
    accept: (projection) => case52NewTurnAcquisitionAccepted(projection, {
      expectedSessionId,
      previousExpectedState,
    }),
  });
}

async function executeStage({ context, cdp, input, sink, baseline, referenceManifest, stage, promptInput, expectedSessionId }) {
  const prompt = promptInput.text;
  const wirePrompt = prompt.trim();
  const sourceIdentity = {
    path: promptInput.path,
    sha256: promptInput.sha256,
    size_bytes: promptInput.size_bytes,
    gui_text_sha256: promptInput.gui_text_sha256,
    gui_text_size_bytes: promptInput.gui_text_size_bytes,
    line_endings_normalized: promptInput.line_endings_normalized,
  };
  const before = await desktopProjection(cdp);
  const beforeIdentity = selectedNavigationIdentity(before);
  if (expectedSessionId !== null && beforeIdentity.session_id !== expectedSessionId) {
    throw productFailure("case5_2-session-before-send", `${stage.id} did not begin on the exact original Project Chat`, {
      expected_session_id: expectedSessionId,
      identity: beforeIdentity,
    });
  }
  await insertExactText({ cdp, input, locator: PROMPT, text: prompt, action: `${stage.id}-prompt`, sink });
  let consecutiveOwnerMatches = 0;
  let lastMatchingRunTarget = null;
  const composerSettlement = await waitForObservation({
    label: `case5_2 ${stage.id} GUI new-request composer settlement`,
    timeoutMs: 30_000,
    pollMs: 100,
    sample: async () => {
      const projection = await desktopProjection(cdp);
      const surface = await newRequestComposerSurface(cdp);
      return {
        projection,
        backend_run_target: projection?.run_target ?? null,
        backend_draft_target: projection?.draft_target ?? null,
        surface,
      };
    },
    accept: (sample) => {
      const expectedRunTarget = sample?.backend_run_target;
      const ready = expectedRunTarget !== null
        && typeof expectedRunTarget === "object"
        && !Array.isArray(expectedRunTarget)
        && case52NewRequestComposerSurfaceReady(sample?.surface, prompt, expectedRunTarget);
      if (!ready) {
        consecutiveOwnerMatches = 0;
        lastMatchingRunTarget = null;
        return false;
      }
      consecutiveOwnerMatches = lastMatchingRunTarget !== null
        && isDeepStrictEqual(lastMatchingRunTarget, expectedRunTarget)
        ? consecutiveOwnerMatches + 1
        : 1;
      lastMatchingRunTarget = structuredClone(expectedRunTarget);
      return consecutiveOwnerMatches >= 2;
    },
  });
  const settledProjection = composerSettlement.value.projection;
  const settledRunTarget = composerSettlement.value.backend_run_target;
  const settledDraftTarget = composerSettlement.value.backend_draft_target;
  const settledIdentity = selectedNavigationIdentity(settledProjection);
  if (expectedSessionId !== null && settledIdentity.session_id !== expectedSessionId) {
    throw productFailure("case5_2-session-before-send", `${stage.id} changed Project Chat while settling the composer owner`, {
      expected_session_id: expectedSessionId,
      identity: settledIdentity,
    });
  }
  const previousExpectedState = expectedSessionId === null
    ? null
    : settledRunTarget?.expectedState ?? null;
  await sink.record("case5_2-composer-settled", {
    stage: stage.id,
    elapsed_ms: composerSettlement.elapsed_ms,
    consecutive_owner_matches: consecutiveOwnerMatches,
    prompt_value_sha256: sourceIdentity.gui_text_sha256,
    prompt_value_bytes: sourceIdentity.gui_text_size_bytes,
    backend_run_target: settledRunTarget,
    backend_draft_target: settledDraftTarget,
    surface: {
      ...composerSettlement.value.surface,
      prompt_value: null,
    },
  }, { phase: "executing", owner: OWNER });
  const sendCommandProbe = new DesktopCommandProbe(cdp, {
    probeId: `case5-2-${stage.id}-send`,
    commands: ["submit_prompt", "cancel_run"],
  });
  await sendCommandProbe.install();
  let send;
  let sendCommand;
  try {
    const commandStart = (await sendCommandProbe.snapshot()).sequence;
    send = await recordTrustedClick({
      input,
      locator: SEND_NEW_REQUEST,
      action: `${stage.id}-send`,
      sink,
      stableHitSamples: 3,
    });
    const commandObservation = await waitForObservation({
      label: `case5_2 ${stage.id} exact trusted Send command`,
      timeoutMs: 10_000,
      pollMs: 50,
      sample: () => sendCommandProbe.snapshot(commandStart),
      accept: (snapshot) => snapshot.calls.length >= 1,
    });
    sendCommand = assertExactDesktopCommandSequence(commandObservation.value, {
      afterSequence: commandStart,
      expected: [{
        command: "submit_prompt",
        args: {
          text: prompt,
          expectedTarget: settledDraftTarget,
          expectedRunTarget: settledRunTarget,
        },
      }],
    });
    await sink.record("case5_2-send-command", {
      stage: stage.id,
      command: "submit_prompt",
      sequence: sendCommand.last_sequence,
      text_sha256: sourceIdentity.gui_text_sha256,
      text_size_bytes: sourceIdentity.gui_text_size_bytes,
      expected_target: settledDraftTarget,
      expected_run_target: settledRunTarget,
      cancel_run_count: 0,
    }, { phase: "executing", owner: OWNER });
  } finally {
    await sendCommandProbe.remove();
  }
  const acquired = await waitForTurnAcquisition(cdp, { expectedSessionId, previousExpectedState });
  const acquiredProjection = acquired.value;
  const row = selectedSessionRow(acquiredProjection);
  const sessionId = row.session_id;
  const turnId = row.active_turn_id;
  const identity = selectedNavigationIdentity(acquiredProjection);
  if (identity.project_id === null || identity.session_id !== sessionId) {
    throw productFailure("case5_2-project-chat-owner", `${stage.id} did not acquire a Project Chat session`, { identity, row });
  }
  await sink.record("case5_2-stage-started", {
    stage: stage.id,
    prompt_source: sourceIdentity,
    composer_value_sha256: sourceIdentity.gui_text_sha256,
    composer_value_bytes: sourceIdentity.gui_text_size_bytes,
    wire_prompt_sha256: sha256(Buffer.from(wirePrompt, "utf8")),
    session_id: sessionId,
    turn_id: turnId,
    previous_expected_state: previousExpectedState,
    identity,
    send,
    send_command_sequence: sendCommand?.last_sequence ?? null,
    acquisition_elapsed_ms: acquired.elapsed_ms,
  }, { phase: "executing", owner: OWNER });
  const terminal = await waitForStageTerminal({
    context,
    cdp,
    input,
    sink,
    baseline,
    referenceManifest,
    stage,
    expectedSessionId: sessionId,
    expectedTurnId: turnId,
    expectedPrompt: wirePrompt,
  });
  const terminalIdentity = selectedNavigationIdentity(terminal.projection);
  const screenshot = await captureScenarioScreenshot({ cdp, sink, name: `case5_2-${stage.id}-terminal`, owner: OWNER });
  const projectionIdentity = await sink.writeJson(`case5_2/projections/${stage.id}-terminal.json`, terminal.projection);
  await sink.record("case5_2-stage-terminal", {
    stage: stage.id,
    session_id: sessionId,
    turn_id: turnId,
    elapsed_ms: terminal.elapsed_ms,
    identity: terminalIdentity,
    monitor: terminal.monitor,
    projection: projectionIdentity,
    screenshot,
  }, { phase: "executing", owner: OWNER });
  return {
    stage: stage.id,
    sessionId,
    turnId,
    wirePrompt,
    terminal: terminal.projection,
    history: historyRows(terminal.projection),
    elapsed_ms: terminal.elapsed_ms,
    screenshot,
  };
}

async function storeProcessEvidence({ sink, name, result }) {
  const stdout = await sink.writeBytes(`case5_2/external/${name}.stdout.txt`, result.stdout);
  const stderr = await sink.writeBytes(`case5_2/external/${name}.stderr.txt`, result.stderr);
  const summary = {
    schema_version: result.schema_version,
    command: result.command,
    owner: result.owner,
    job: result.job,
    outcome: result.outcome,
    output: result.output,
    capture: result.capture,
    wrapper: result.wrapper,
    exit_code: result.exit_code,
    signal: result.signal,
    elapsed_ms: result.elapsed_ms,
    stdout,
    stderr,
  };
  await sink.writeJson(`case5_2/external/${name}.json`, summary);
  return summary;
}

async function runPythonTest({ context, sink, state, python, name, cwd, args, extraEnv = {} }) {
  await assertExecutableIdentity(python, name);
  const cacheRoot = path.join(context.paths.logs, `python-cache-${name}`);
  const result = await runOwnedExternalProcess({
    context,
    executable: python.path,
    args,
    cwd,
    env: await ownedProcessEnvironment(context, name, {
      PYTHONDONTWRITEBYTECODE: "1",
      PYTHONPYCACHEPREFIX: cacheRoot,
      ...extraEnv,
    }),
    timeoutMs: 30 * 60 * 1000,
    state,
    label: name,
  });
  return storeProcessEvidence({ sink, name, result });
}

function assertPythonTestWithinBounds(name, evidence) {
  if (evidence.outcome.timed_out || evidence.outcome.output_limit_exceeded) {
    throw productFailure(
      "case5_2-external-test-bounds",
      `${name} exceeded its external test time or output bound`,
      evidence,
    );
  }
}

async function runStage3PublicSuite({ context, sink, state, python }) {
  const result = await runPythonTest({
    context,
    sink,
    state,
    python,
    name: "stage3-public-suite",
    cwd: path.join(context.paths.workspace, "backend"),
    args: ["-X", "utf8", "-m", "pytest", "-p", "no:cacheprovider", "-q"],
  });
  await sink.record("case5_2-stage3-public-suite", result, { phase: "executing", owner: OWNER });
  return result;
}

export async function settleCase52WorkspaceEvaluator({
  label,
  evaluate,
  captureAfter,
  assertStable,
  recordRecovery,
}) {
  let result = null;
  let afterManifest = null;
  let workspaceDiff = null;
  let primaryError = null;
  const integrityErrors = [];
  try {
    result = await evaluate();
  } catch (error) {
    primaryError = error;
  }
  try {
    afterManifest = await captureAfter();
  } catch (error) {
    integrityErrors.push({ step: "post-evaluator-workspace-manifest", error, observation: errorObservation(error) });
  }
  if (afterManifest !== null) {
    try {
      workspaceDiff = await assertStable(afterManifest);
    } catch (error) {
      integrityErrors.push({ step: "post-evaluator-workspace-stability", error, observation: errorObservation(error) });
    }
  }
  if (primaryError !== null || integrityErrors.length > 0) {
    const recoveryEvidence = {
      label,
      primary_error: primaryError === null ? null : errorObservation(primaryError),
      integrity_errors: integrityErrors.map(({ step, observation }) => ({ step, ...observation })),
      after_manifest: afterManifest,
      workspace_diff: workspaceDiff,
    };
    try { await recordRecovery(recoveryEvidence); }
    catch { /* Preserve the evaluator or integrity failure that triggered recovery. */ }
    if (primaryError !== null) throw primaryError;
    throw integrityErrors[0].error;
  }
  return { result, afterManifest, workspaceDiff };
}

async function runWorkspaceStableEvaluator({
  context,
  sink,
  baseline,
  beforeManifest,
  afterStage,
  label,
  evaluate,
}) {
  return settleCase52WorkspaceEvaluator({
    label,
    evaluate,
    captureAfter: () => storeStageManifest({ context, sink, baseline, stage: afterStage }),
    assertStable: (afterManifest) => assertEvaluatorWorkspaceStable({
      sink,
      before: beforeManifest,
      after: afterManifest,
      label,
    }),
    recordRecovery: (evidence) => sink.record("case5_2-evaluator-recovery", evidence, {
      phase: "executing",
      owner: OWNER,
    }),
  });
}

async function runFinalEvaluator({
  context,
  sink,
  state,
  python,
  baseline,
  preEvaluationManifest,
  oracleIdentity,
}) {
  const publicEvaluation = await runWorkspaceStableEvaluator({
    context,
    sink,
    baseline,
    beforeManifest: preEvaluationManifest,
    afterStage: "stage4-post-public-evaluator",
    label: "stage4-public-suite",
    evaluate: () => runPythonTest({
      context,
      sink,
      state,
      python,
      name: "stage4-public-suite",
      cwd: path.join(context.paths.workspace, "backend"),
      args: ["-X", "utf8", "-m", "pytest", "-p", "no:cacheprovider", "-q"],
    }),
  });
  const publicSuite = publicEvaluation.result;
  const postPublicManifest = publicEvaluation.afterManifest;
  const publicWorkspaceDiff = publicEvaluation.workspaceDiff;
  assertPythonTestWithinBounds("stage4-public-suite", publicSuite);
  const oracleBefore = await assertOracleIdentity(oracleIdentity, "pre-hidden-evaluator");
  let hiddenOracle = null;
  let oracleAfter = null;
  let finalManifest = null;
  let hiddenWorkspaceDiff = null;
  let primaryHiddenError = null;
  const integrityErrors = [];
  try {
    hiddenOracle = await runPythonTest({
      context,
      sink,
      state,
      python,
      name: "stage4-hidden-oracle",
      cwd: context.paths.logs,
      args: ["-X", "utf8", "-m", "pytest", "-p", "no:cacheprovider", "-q", oracleIdentity.path],
      extraEnv: { CASE5_2_WORKSPACE: context.paths.workspace },
    });
  } catch (error) {
    primaryHiddenError = error;
  }
  try {
    oracleAfter = await assertOracleIdentity(oracleIdentity, "post-hidden-evaluator");
  } catch (error) {
    integrityErrors.push({ step: "post-hidden-oracle-identity", error, observation: errorObservation(error) });
  }
  try {
    finalManifest = await storeStageManifest({
      context,
      sink,
      baseline,
      stage: "stage4-post-evaluator",
    });
  } catch (error) {
    integrityErrors.push({ step: "post-hidden-workspace-manifest", error, observation: errorObservation(error) });
  }
  if (finalManifest !== null) {
    try {
      hiddenWorkspaceDiff = await assertEvaluatorWorkspaceStable({
        sink,
        before: postPublicManifest,
        after: finalManifest,
        label: "stage4-hidden-oracle",
      });
    } catch (error) {
      integrityErrors.push({ step: "post-hidden-workspace-stability", error, observation: errorObservation(error) });
    }
  }
  if (primaryHiddenError !== null || integrityErrors.length > 0) {
    const recoveryEvidence = {
      primary_error: primaryHiddenError === null ? null : errorObservation(primaryHiddenError),
      integrity_errors: integrityErrors.map(({ step, observation }) => ({ step, ...observation })),
      oracle_after: oracleAfter,
      final_manifest: finalManifest,
      hidden_workspace_diff: hiddenWorkspaceDiff,
    };
    try {
      await sink.record("case5_2-hidden-evaluator-recovery", recoveryEvidence, { phase: "executing", owner: OWNER });
    } catch {
      // Preserve the first evaluator/integrity failure even if secondary evidence persistence also fails.
    }
    if (primaryHiddenError !== null) throw primaryHiddenError;
    throw integrityErrors[0].error;
  }
  assertPythonTestWithinBounds("stage4-hidden-oracle", hiddenOracle);
  const report = {
    schema_version: "desktop-e2e.case5_2-evaluation.v1",
    workspace: context.paths.workspace,
    evaluated_at: new Date().toISOString(),
    oracle: {
      path: oracleIdentity.path,
      sha256: oracleIdentity.sha256,
      size_bytes: oracleIdentity.size_bytes,
      pre_evaluator: oracleBefore,
      post_evaluator: oracleAfter,
    },
    public_suite: publicSuite,
    hidden_oracle: hiddenOracle,
    pre_evaluation_manifest: {
      stage: preEvaluationManifest.stage,
      diff: preEvaluationManifest.diff,
      documents: preEvaluationManifest.documents,
    },
    evaluator_workspace_diff: {
      public_suite: publicWorkspaceDiff,
      hidden_oracle: hiddenWorkspaceDiff,
    },
    documents: finalManifest.documents,
    diff: finalManifest.diff,
    all_required_documents: finalManifest.documents.every((row) => row.exists === true && row.bytes > 0),
    public_suite_pass: publicSuite.exit_code === 0,
    hidden_oracle_pass: hiddenOracle.exit_code === 0,
  };
  const identity = await sink.writeJson("case5_2/evaluation.json", report);
  await sink.record("case5_2-final-evaluation", { identity, report }, { phase: "executing", owner: OWNER });
  const failures = case52EvaluatorFailures(report);
  if (failures.length > 0) {
    throw productFailure("case5_2-final-evaluation", "case5_2 public suite, hidden oracle, or required-document gate failed", { failures, report });
  }
  return { report, finalManifest };
}

async function stableRestartProjection({ cdp, sessionId, turnId, prompt, beforeHistory }) {
  let acceptedSince = null;
  let observed;
  try {
    observed = await waitForObservation({
      label: "case5_2 same-session restart continuity",
      timeoutMs: 60_000,
      pollMs: 100,
      sample: async () => {
        const projection = await desktopProjection(cdp);
        return {
          projection,
          classified: classifyCase52RestartContinuity(projection, {
            beforeSessionId: sessionId,
            expectedTurnId: turnId,
            expectedPrompt: prompt,
            beforeHistory,
            minimumCompletedSummaryCount: 1,
          }),
        };
      },
      accept: ({ classified }) => {
        if (classified.decision === "fail") return true;
        if (classified.decision !== "pass") {
          acceptedSince = null;
          return false;
        }
        const now = Date.now();
        acceptedSince ??= now;
        return now - acceptedSince >= RESTORE_STABILITY_MS;
      },
      retrySampleErrors: false,
    });
  } catch (error) {
    if (error?.code !== "observation-timeout" || error?.evidence?.last_error) throw error;
    throw productFailure(
      "case5_2-restart-timeout",
      "restarted Project Chat did not settle to the same normal-terminal session before the product deadline",
      error.evidence,
    );
  }
  if (observed.value.classified.decision === "fail") {
    throw productFailure(
      "case5_2-restart-continuity",
      "restarted Project Chat reached a settled state with session, history, or terminal-owner drift",
      {
        failures: observed.value.classified.failures,
        terminal_failures: observed.value.classified.terminal_failures,
        continuity_failures: observed.value.classified.continuity_failures,
        projection: observed.value.projection,
      },
    );
  }
  return {
    ...observed,
    value: observed.value.projection,
    classified: observed.value.classified,
  };
}

async function waitForRestartTurnPage({
  cdp,
  sessionId,
  turnId,
  admissionRevision,
  total,
  limit,
  requireLatestSuffix,
  previousOffset = null,
  allowCommandPalette = false,
}) {
  let observed;
  try {
    observed = await waitForObservation({
      label: requireLatestSuffix
        ? "case5_2 restarted latest bounded turn page"
        : "case5_2 previous turn page settlement",
      timeoutMs: 60_000,
      pollMs: 100,
      sample: async () => {
        const projection = await desktopProjection(cdp);
        const terminal = classifyCase52NormalTerminal(projection, {
          expectedSessionId: sessionId,
          expectedTurnId: turnId,
          expectedPrompt: null,
          minimumCompletedSummaryCount: 1,
          allowedOverlay: allowCommandPalette ? "command_palette" : "none",
        });
        const page = classifyCase52RestartTurnPage(projection, {
          expectedSessionId: sessionId,
          expectedTurnId: turnId,
          expectedAdmissionRevision: admissionRevision,
          expectedTotal: total,
          expectedLimit: limit,
          requireLatestSuffix,
        });
        return { projection, terminal, page };
      },
      accept: ({ terminal, page }) => {
        if (terminal.decision === "fail" || page.decision === "fail") return true;
        if (terminal.decision !== "pass" || page.decision === "pending") return false;
        return previousOffset === null || page.metadata.offset !== previousOffset;
      },
      retrySampleErrors: false,
    });
  } catch (error) {
    if (error?.code !== "observation-timeout" || error?.evidence?.last_error) throw error;
    throw productFailure(
      "case5_2-restart-turn-page-timeout",
      "restarted Project Chat turn-page owner did not settle before the product deadline",
      error.evidence,
    );
  }
  if (observed.value.terminal.decision === "fail" || observed.value.page.decision === "fail") {
    throw productFailure(
      "case5_2-restart-turn-page-owner",
      "restarted Project Chat bounded turn-page owner drifted",
      {
        terminal: observed.value.terminal,
        page: observed.value.page,
        projection: observed.value.projection,
      },
    );
  }
  return observed;
}

async function closeCommandPaletteWithTrustedEscape({ cdp, input, sink }) {
  const start = (await input.snapshotProbe()).sequence;
  await input.pressKey("Escape");
  const snapshot = await input.snapshotProbe(start);
  const probe = assertTrustedProbeSequence(snapshot, {
    afterSequence: start,
    expected: [
      { type: "keydown", key: "Escape", code: "Escape" },
      { type: "keyup", key: "Escape", code: "Escape" },
    ],
  });
  const closed = await waitForObservation({
    label: "case5_2 restart history command palette closed",
    timeoutMs: 10_000,
    pollMs: 100,
    sample: () => desktopProjection(cdp),
    accept: (projection) => projection?.overlay === "none",
    retrySampleErrors: false,
  });
  await sink.record("case5_2-trusted-action", {
    action: "close-restart-history-command-palette",
    input_kind: "browser_trusted",
    probe,
    projection_revision: closed.value.projection_revision,
  }, { phase: "executing", owner: OWNER });
}

export async function waitForCase52RestartHistoryTarget({ input }) {
  let observed;
  try {
    observed = await waitForSemanticTargetSettlement({
      input,
      locator: PREVIOUS_TURN_PAGE,
      label: "case5_2 restart history semantic target",
      timeoutMs: 10_000,
      pollMs: 16,
    });
  } catch (error) {
    if (error?.code !== "observation-timeout" || error?.evidence?.last_error) throw error;
    throw productFailure(
      "case5_2-restart-history-target-timeout",
      "restarted Project Chat semantic previous-page action did not reappear after page settlement",
      error.evidence,
    );
  }
  if (observed.value.classified.decision === "fail") {
    throw productFailure(
      "case5_2-restart-history-target",
      "restarted Project Chat semantic previous-page action was ambiguous after page settlement",
      observed.value,
    );
  }
  return observed;
}

async function expandRestartHistory({
  cdp,
  input,
  sink,
  sessionId,
  turnId,
  admissionRevision,
  total,
  limit,
}) {
  const initial = await waitForRestartTurnPage({
    cdp,
    sessionId,
    turnId,
    admissionRevision,
    total,
    limit,
    requireLatestSuffix: true,
  });
  if (initial.value.page.decision === "ready") {
    return { projection: initial.value.projection, pages: [], initial: initial.value.page.metadata };
  }

  await recordTrustedClick({
    input,
    locator: SHOW_COMMAND_PALETTE,
    action: "open-restart-history-command-palette",
    sink,
  });
  await waitForObservation({
    label: "case5_2 restart history command palette",
    timeoutMs: 10_000,
    pollMs: 100,
    sample: () => desktopProjection(cdp),
    accept: (projection) => projection?.overlay === "command_palette",
    retrySampleErrors: false,
  });
  await replaceExactText({
    cdp,
    input,
    locator: COMMAND_PALETTE_SEARCH,
    text: "load-previous-turn-page",
    action: "filter-restart-history-command",
    sink,
  });

  const pages = [];
  let current = initial.value;
  const maximumPages = Math.ceil(current.page.metadata.offset / current.page.metadata.limit);
  while (current.page.decision === "page_needed") {
    if (pages.length >= maximumPages) {
      throw productFailure(
        "case5_2-restart-turn-page-bound",
        "restarted Project Chat required more previous-page transitions than its canonical metadata permits",
        { maximum_pages: maximumPages, pages, current },
      );
    }
    const before = current.page.metadata;
    await waitForCase52RestartHistoryTarget({ input });
    const action = await recordTrustedClick({
      input,
      locator: PREVIOUS_TURN_PAGE,
      action: `restart-history-previous-page-${pages.length + 1}`,
      sink,
    });
    const settled = await waitForRestartTurnPage({
      cdp,
      sessionId,
      turnId,
      admissionRevision,
      total,
      limit,
      requireLatestSuffix: false,
      previousOffset: before.offset,
      allowCommandPalette: true,
    });
    const after = settled.value.page.metadata;
    const failures = case52RestartPreviousPageTransitionFailures({ before, after });
    const evidence = {
      page: pages.length + 1,
      before,
      after,
      failures,
      action,
    };
    await sink.record("case5_2-restart-history-prepend", evidence, {
      phase: "executing",
      owner: OWNER,
    });
    if (failures.length > 0) {
      throw productFailure(
        "case5_2-restart-turn-page-transition",
        "restarted Project Chat previous-page transition drifted from the exact bounded range",
        evidence,
      );
    }
    pages.push({ before, after });
    current = settled.value;
  }
  await closeCommandPaletteWithTrustedEscape({ cdp, input, sink });
  return { projection: current.projection, pages, initial: initial.value.page.metadata };
}

export async function providerMustKeepSideUnloaded({ options, sink, state, name, providerIo = {} }) {
  const capture = providerIo.capture ?? (async () => {
    const snapshot = await providerSnapshot(options);
    return { snapshot, models: case52ProviderModelState(snapshot, options) };
  });
  const captured = await capture();
  const snapshot = captured.snapshot;
  const models = captured.models ?? case52ProviderModelState(snapshot, options);
  if (externalLmStudio(options)) {
    const observation = case52ExternalLmStudioObservation(
      snapshot,
      options,
      state.providerHostFingerprint,
    );
    const sample = {
      name,
      captured_at: snapshot.captured_at,
      selected_side_model: options.sideModel,
      side_loaded_instance_ids: Array.isArray(models.side?.loaded_instances)
        ? models.side.loaded_instances.map((entry) => entry?.id ?? null)
        : null,
      side_v0_state: models.side_v0?.state ?? null,
      main_loaded_instance_ids: Array.isArray(models.main?.loaded_instances)
        ? models.main.loaded_instances.map((entry) => entry?.id ?? null)
        : null,
      reported_loaded_context: observation.host_context.reported_loaded,
      host_fingerprint_sha256: observation.host_fingerprint.sha256,
      host_fingerprint_stable: observation.host_fingerprint_stable,
      lifecycle_actions: structuredClone(observation.lifecycle_actions),
    };
    state.sideProviderSamples.push(sample);
    state.providerHostFingerprintSamples ??= [];
    state.providerHostFingerprintSamples.push({
      name,
      fingerprint: structuredClone(observation.host_fingerprint),
    });
    const identity = await sink.writeJson(`case5_2/provider/${name}.json`, observation);
    if (observation.failures.length > 0) {
      throw productFailure(
        "case5_2-provider-runtime-drift",
        "external-unmanaged LM Studio host state drifted during the scored run; no repair action was issued",
        { name, failures: observation.failures, observation, identity },
      );
    }
    return { snapshot, models, failures: observation.failures, identity, observation };
  }
  if (options.providerProfile === OPENAI_COMPATIBLE_PROFILE) {
    state.sideProviderSamples.push({
      name,
      captured_at: snapshot.captured_at,
      selected_side_model: options.sideModel,
      same_as_main_model: options.sideModel === options.mainModel,
      exact_model_present: models.main_match_count === 1,
      context_capacity: models.context_capacity,
      provider_load_state: "not-observable-external-unmanaged",
    });
    const failures = providerCatalogFailures(models, options, { mainLoaded: true });
    const identity = await sink.writeJson(`case5_2/provider/${name}.json`, { snapshot, models, failures });
    if (failures.length > 0) {
      throw productFailure(
        "case5_2-provider-runtime-drift",
        "OpenAI-compatible external provider model availability drifted during the scored run",
        { name, failures, models, identity },
      );
    }
    return { snapshot, models, failures, identity };
  }
  state.sideProviderSamples.push({
    name,
    captured_at: snapshot.captured_at,
    loaded_instance_ids: Array.isArray(models.side?.loaded_instances)
      ? models.side.loaded_instances.map((entry) => entry?.id ?? null)
      : null,
    v0_state: models.side_v0?.state ?? null,
  });
  const failures = providerCatalogFailures(models, options, {
    mainLoaded: true,
    expectedLoadedContext: state.providerEffectiveContext,
  });
  const identity = await sink.writeJson(`case5_2/provider/${name}.json`, { snapshot, models, failures });
  if (failures.length > 0) {
    throw productFailure("case5_2-provider-runtime-drift", "provider model ownership drifted during the scored run", { name, failures, models, identity });
  }
  return { snapshot, models, failures, identity };
}

export function case52SideProviderSummary(options, samples) {
  const providerSamples = structuredClone(samples);
  return {
    selected_model_unloaded_samples: options.providerProfile === LM_STUDIO_PROFILE
      ? structuredClone(samples)
      : [],
    selected_model_provider_samples: providerSamples,
  };
}

export function case52LegacySideSummaryV1({ stage4SideChat, restoredSideChat, providerSummary }) {
  return {
    side_chat: structuredClone(stage4SideChat),
    side_chat_request_observation: {
      trusted_side_send_action_count: "not-derived-from-event-ledger",
      persisted_message_count_at_restart_restore: restoredSideChat?.messages?.length ?? null,
      persisted_message_count_at_stage4_terminal: stage4SideChat?.messages?.length ?? null,
      ...structuredClone(providerSummary),
      provider_generation_request_zero: "unverified-no-traffic-ledger",
    },
  };
}

export function case52ProviderSummaryEvidence(options, state) {
  const executionOwnedLmStudio = options.providerProfile === LM_STUDIO_PROFILE
    && !externalProvider(options);
  const unmanagedLmStudio = externalLmStudio(options);
  return {
    provider_requested_context: executionOwnedLmStudio ? QUALITY_CONTEXT_WINDOW : null,
    provider_applied_context: executionOwnedLmStudio ? state.providerEffectiveContext ?? null : null,
    provider_reported_context_capacity: options.providerProfile === OPENAI_COMPATIBLE_PROFILE
      ? state.providerEffectiveContext ?? null
      : null,
    provider_reported_loaded_context: unmanagedLmStudio
      ? state.providerEffectiveContext ?? null
      : null,
    provider_lifecycle_actions: {
      load_attempted: executionOwnedLmStudio && state.providerLoadAttempted === true,
      unload_attempted: false,
      unload_authorized: executionOwnedLmStudio,
    },
    provider_host_fingerprint: unmanagedLmStudio
      ? structuredClone(state.providerHostFingerprint ?? null)
      : null,
    provider_host_fingerprint_samples: unmanagedLmStudio
      ? structuredClone(state.providerHostFingerprintSamples ?? [])
      : [],
  };
}

async function cleanupWebviewInput(input, state, label) {
  if (input === null) return;
  if (state.inputCleanupAttempts.has(input)) return;
  state.inputCleanupAttempts.add(input);
  try {
    await input.cleanup();
  } catch (error) {
    const failure = { label, ...errorObservation(error) };
    state.inputCleanupFailures.push(failure);
    throw new DesktopE2eError("harness", "case5_2-input-cleanup", `${label} WebView input did not settle exactly`, failure);
  }
}

export function createCase52Scenario(rawOptions = {}) {
  const options = normalizeCase52Options(rawOptions);
  const state = {
    baseline: null,
    seed: null,
    promptInputs: null,
    sidePromptInput: null,
    oracle: null,
    python: null,
    externalRoots: [],
    externalBaseline: [],
    providerLoadAttempted: false,
    providerLoadResponseObserved: false,
    providerOwned: false,
    providerExternalPreflightObserved: false,
    mainProviderInstanceId: null,
    acceptedProviderLoad: null,
    providerEffectiveContext: null,
    providerProfileExact: false,
    providerComparabilityDeviations: [],
    providerHostFingerprint: null,
    providerHostFingerprintSamples: [],
    sideProviderSamples: [],
    externalProcessOwner: {
      started: 0,
      settled: 0,
      in_flight: 0,
      failures: [],
    },
    inputCleanupAttempts: new WeakSet(),
    inputCleanupFailures: [],
    scenarioCleanupFailures: [],
    quiesceOutcome: null,
    acceptedEnd: false,
    performance: {
      launch_to_ready_ms: null,
      reopen_to_ready_ms: null,
    },
  };
  return Object.freeze({
    id: "manual.case5_2",
    productOracle: "pass",
    manualGate: "pending",
    databaseRequired: true,
    requestGracefulExit,
    environment: {},
    async prepare({ context, sink, phase }) {
      const task = await fileIdentity(path.join(caseDirectory, "task.md"), { includeBytes: true });
      const oracleSource = await fileIdentity(path.join(caseDirectory, "oracle", "test_cancel_contract.py"), { includeBytes: true });
      const promptInputs = {};
      for (const stage of STAGES) {
        const input = await fileIdentity(path.join(caseDirectory, stage.promptFile), { includeBytes: true });
        const sourceText = input.bytes.toString("utf8");
        const text = normalizeCase52PromptText(sourceText);
        promptInputs[stage.id] = {
          path: input.path,
          sha256: input.sha256,
          size_bytes: input.size_bytes,
          gui_text_sha256: sha256(Buffer.from(text, "utf8")),
          gui_text_size_bytes: Buffer.byteLength(text, "utf8"),
          line_endings_normalized: text !== sourceText,
          text,
        };
      }
      state.promptInputs = promptInputs;
      const sideInput = await fileIdentity(path.join(caseDirectory, SIDE_CHAT_STAGE.promptFile), { includeBytes: true });
      const sideSourceText = sideInput.bytes.toString("utf8");
      const sideText = normalizeCase52PromptText(sideSourceText);
      state.sidePromptInput = {
        path: sideInput.path,
        sha256: sideInput.sha256,
        size_bytes: sideInput.size_bytes,
        gui_text_sha256: sha256(Buffer.from(sideText, "utf8")),
        gui_text_size_bytes: Buffer.byteLength(sideText, "utf8"),
        line_endings_normalized: sideText !== sideSourceText,
        text: sideText,
      };
      const immutableDirectory = path.join(context.paths.logs, "immutable-inputs");
      await mkdir(immutableDirectory, { recursive: false });
      const immutableOraclePath = path.join(immutableDirectory, `test_cancel_contract-${oracleSource.sha256}.py`);
      await writeFile(immutableOraclePath, oracleSource.bytes, { flag: "wx" });
      const immutableOracle = await case52PhysicalFileIdentity(immutableOraclePath);
      state.oracle = {
        ...immutableOracle,
        source_path: oracleSource.path,
        source_sha256: oracleSource.sha256,
        source_size_bytes: oracleSource.size_bytes,
      };

      state.seed = await copyCase52CleanSeed({ source: options.fixtureSource, destination: context.paths.workspace });
      await writeFile(path.join(context.paths.workspace, "task.md"), task.bytes, { flag: "wx" });
      state.baseline = baselineManifest(state.seed, task);
      const baselineIdentity = await sink.writeJson("case5_2/baseline-manifest.json", state.baseline);

      await prepareDesktopFixture({
        context,
        sink,
        phase,
        owner: OWNER,
        configText: case52FixtureConfig(options),
        sentinelName: null,
        sentinelText: "",
      });

      state.python = await resolveExecutable("python.exe", state, context);
      state.externalRoots = await pythonSiteRoots(state.python, context, state);
      state.externalBaseline = await externalRootsSnapshot(state.externalRoots);
      const externalIdentity = await sink.writeJson("case5_2/external/python-environment-before.json", {
        python: state.python,
        roots: state.externalBaseline,
      });

      await loadMainProvider({ options, sink, state, phase });
      const providerEvidence = case52ProviderSummaryEvidence(options, state);
      await sink.record("case5_2-prepared", {
        options: case52EvidenceOptions(options),
        provider_contract: {
          profile: options.providerProfile,
          lifecycle: options.providerLifecycle,
          scenario_config_profile: options.scenarioConfigProfile,
          extra_body: case52ExtraBodyEvidence(),
          comparability_deviations: state.providerComparabilityDeviations,
          lifecycle_actions: providerEvidence.provider_lifecycle_actions,
          host_fingerprint: providerEvidence.provider_host_fingerprint,
        },
        quality_profile: {
          context_window: QUALITY_CONTEXT_WINDOW,
          provider_num_ctx: providerEvidence.provider_requested_context,
          provider_requested_context: providerEvidence.provider_requested_context,
          provider_applied_context: providerEvidence.provider_applied_context,
          provider_reported_context_capacity: providerEvidence.provider_reported_context_capacity,
          provider_reported_loaded_context: providerEvidence.provider_reported_loaded_context,
          lifecycle_actions: providerEvidence.provider_lifecycle_actions,
          provider_profile_exact: state.providerProfileExact,
          output_limit_owner: "provider",
          request_timeout_ms: QUALITY_REQUEST_TIMEOUT_MS,
          max_retries: 0,
          access_mode: "auto_review",
          multi_agent_enabled: false,
          mcp_enabled: false,
          docling_enabled: false,
        },
        baseline: baselineIdentity,
        source_seed: state.seed,
        task: { path: task.path, sha256: task.sha256, size_bytes: task.size_bytes },
        prompts: Object.fromEntries(Object.entries(promptInputs).map(([key, value]) => [key, {
          path: value.path,
          sha256: value.sha256,
          size_bytes: value.size_bytes,
          gui_text_sha256: value.gui_text_sha256,
          gui_text_size_bytes: value.gui_text_size_bytes,
          line_endings_normalized: value.line_endings_normalized,
        }])),
        side_chat_prompt: {
          stage: SIDE_CHAT_STAGE.id,
          path: state.sidePromptInput.path,
          sha256: state.sidePromptInput.sha256,
          size_bytes: state.sidePromptInput.size_bytes,
          gui_text_sha256: state.sidePromptInput.gui_text_sha256,
          gui_text_size_bytes: state.sidePromptInput.gui_text_size_bytes,
          line_endings_normalized: state.sidePromptInput.line_endings_normalized,
        },
        oracle: state.oracle,
        executable_identity: { python: state.python },
        external_environment: externalIdentity,
      }, { phase, owner: OWNER });
    },
    async execute({ context, driver: firstCdp, host, runtime: firstRuntime, sink }) {
      if (state.baseline === null || state.promptInputs === null || state.sidePromptInput === null
        || state.python === null || state.oracle === null) {
        throw new Error("manual.case5_2 was not prepared");
      }
      let activeInput = null;
      let activeCdp = firstCdp;
      let primaryError = null;
      try {
        const initialShell = await acquireInteractiveShell({ context, driver: activeCdp, sink }, {
          evidenceOwner: OWNER,
          screenshotStem: "case5_2-main-shell-ready",
        });
        state.performance.launch_to_ready_ms = elapsedSince(firstRuntime.launch_started_at);
        activeInput = new WebviewInput(activeCdp, { probeId: "case5-2-generation-1", maxProbeEvents: 65_536 });
        await activeInput.installProbe();
        if (options.configureMainViaGui) {
          await configureMainConnectionViaGui({
            cdp: activeCdp,
            input: activeInput,
            sink,
            options,
            state,
          });
        }
        const initialProjection = await desktopProjection(activeCdp);
        const initialConfigFailures = mainConfigurationFailures(initialProjection, options);
        if (initialConfigFailures.length > 0) {
          throw productFailure("case5_2-quality-config", "Desktop effective state did not match the frozen case5_2 Quality profile", {
            failures: initialConfigFailures,
            projection: initialProjection,
          });
        }
        if (initialProjection?.side_chat?.configured === true) {
          throw productFailure("case5_2-side-chat-not-fresh", "fresh case5_2 data unexpectedly contained a Side Chat binding", { side_chat: initialProjection.side_chat });
        }
        await providerMustKeepSideUnloaded({ options, sink, state, name: "desktop-ready" });

        const stage1 = await executeStage({
          context,
          cdp: activeCdp,
          input: activeInput,
          sink,
          baseline: state.baseline,
          referenceManifest: state.baseline,
          stage: STAGES[0],
          promptInput: state.promptInputs.stage1,
          expectedSessionId: null,
        });
        const stage1ConfigFailures = mainConfigurationFailures(stage1.terminal, options, { sessionRequired: true });
        if (stage1ConfigFailures.length > 0) {
          throw productFailure("case5_2-session-quality-config", "Stage 1 Project Chat did not inherit the exact frozen Main LLM and Quality profile", {
            failures: stage1ConfigFailures,
            session_id: stage1.sessionId,
          });
        }
        await configureSideChat({ cdp: activeCdp, input: activeInput, sink, options, sessionId: stage1.sessionId });
        await providerMustKeepSideUnloaded({ options, sink, state, name: "side-configured-no-request" });
        const stage1Manifest = await storeStageManifest({ context, sink, baseline: state.baseline, stage: "stage1" });
        const stage1Failures = case52Stage1ManifestFailures(stage1Manifest);
        if (stage1Failures.length > 0) {
          throw productFailure("case5_2-stage1-scope", "Stage 1 did not produce exactly the required four repository documents", { failures: stage1Failures, manifest: stage1Manifest });
        }

        const stage2 = await executeStage({
          context,
          cdp: activeCdp,
          input: activeInput,
          sink,
          baseline: state.baseline,
          referenceManifest: stage1Manifest,
          stage: STAGES[1],
          promptInput: state.promptInputs.stage2,
          expectedSessionId: stage1.sessionId,
        });
        const stage2Manifest = await storeStageManifest({ context, sink, baseline: state.baseline, stage: "stage2" });
        const stage2Failures = case52Stage2ManifestFailures(stage1Manifest, stage2Manifest);
        if (stage2Failures.length > 0) {
          throw productFailure("case5_2-stage2-scope", "Stage 2 did not preserve Stage 1 and add only cancel_contract.md", { failures: stage2Failures, manifest: stage2Manifest });
        }

        const stage3 = await executeStage({
          context,
          cdp: activeCdp,
          input: activeInput,
          sink,
          baseline: state.baseline,
          referenceManifest: stage2Manifest,
          stage: STAGES[2],
          promptInput: state.promptInputs.stage3,
          expectedSessionId: stage1.sessionId,
        });
        const stage3Manifest = await storeStageManifest({ context, sink, baseline: state.baseline, stage: "stage3" });
        const stage3Evaluation = await runWorkspaceStableEvaluator({
          context,
          sink,
          baseline: state.baseline,
          beforeManifest: stage3Manifest,
          afterStage: "stage3-post-evaluator",
          label: "stage3-public-suite",
          evaluate: () => runStage3PublicSuite({ context, sink, state, python: state.python }),
        });
        const stage3Suite = stage3Evaluation.result;
        const stage3PostEvaluatorManifest = stage3Evaluation.afterManifest;
        assertPythonTestWithinBounds("stage3-public-suite", stage3Suite);
        if (stage3Suite.exit_code !== 0) {
          throw productFailure("case5_2-stage3-public-suite", "Stage 3 external backend suite failed", stage3Suite);
        }
        await providerMustKeepSideUnloaded({ options, sink, state, name: "stage3-terminal" });

        await cleanupWebviewInput(activeInput, state, "generation-1-before-restart");
        activeInput = null;
        const restarted = await host.restart({ context, scenario: this, sink, driver: activeCdp, phase: "executing" });
        activeCdp = restarted.driver;
        const restartShell = await acquireInteractiveShell({ context, driver: activeCdp, sink }, {
          evidenceOwner: OWNER,
          screenshotStem: "case5_2-restart-shell-ready",
        });
        state.performance.reopen_to_ready_ms = elapsedSince(restarted.runtime.launch_started_at);
        activeInput = new WebviewInput(activeCdp, { probeId: "case5-2-generation-2", maxProbeEvents: 65_536 });
        await activeInput.installProbe();
        const stage3Owner = stage3.terminal?.run_target?.expectedState;
        if (stage3Owner?.kind !== "idle"
          || typeof stage3Owner.admissionRevision !== "string"
          || !Number.isSafeInteger(stage3.terminal?.turn_page_total)
          || stage3.terminal.turn_page_total < 1
          || !Number.isSafeInteger(stage3.terminal?.turn_page_limit)
          || stage3.terminal.turn_page_limit < 1) {
          throw productFailure(
            "case5_2-stage3-restart-owner",
            "Stage 3 terminal projection did not expose a reusable durable restart owner",
            {
              expected_state: stage3Owner ?? null,
              turn_page_total: stage3.terminal?.turn_page_total ?? null,
              turn_page_limit: stage3.terminal?.turn_page_limit ?? null,
            },
          );
        }
        const expandedRestartHistory = await expandRestartHistory({
          cdp: activeCdp,
          input: activeInput,
          sink,
          sessionId: stage1.sessionId,
          turnId: stage3.turnId,
          admissionRevision: stage3Owner.admissionRevision,
          total: stage3.terminal.turn_page_total,
          limit: stage3.terminal.turn_page_limit,
        });
        const restored = await stableRestartProjection({
          cdp: activeCdp,
          sessionId: stage1.sessionId,
          turnId: stage3.turnId,
          prompt: stage3.wirePrompt,
          beforeHistory: stage3.history,
        });
        const restoredProjection = restored.value;
        const restoreConfigFailures = mainConfigurationFailures(restoredProjection, options, { sessionRequired: true });
        if (restoreConfigFailures.length > 0) {
          throw productFailure("case5_2-restart-quality-config", "restarted Project Chat did not retain the exact Main LLM Quality profile", { failures: restoreConfigFailures });
        }
        const restartScreenshot = await captureScenarioScreenshot({ cdp: activeCdp, sink, name: "case5_2-stage3-restored", owner: OWNER });
        const restartProjectionIdentity = await sink.writeJson("case5_2/projections/restart-restored.json", restoredProjection);
        await sink.record("case5_2-restart-restored", {
          restart: restarted.restart,
          session_id: stage1.sessionId,
          turn_id: stage3.turnId,
          identity: selectedNavigationIdentity(restoredProjection),
          history_prefix_rows: stage3.history.length,
          restored_history_rows: historyRows(restoredProjection).length,
          bounded_history: {
            initial: expandedRestartHistory.initial,
            previous_pages_loaded: expandedRestartHistory.pages.length,
            transitions: expandedRestartHistory.pages,
          },
          stability_ms: RESTORE_STABILITY_MS,
          projection: restartProjectionIdentity,
          screenshot: restartScreenshot,
        }, { phase: "executing", owner: OWNER });

        await verifyRestoredSideChat({ cdp: activeCdp, input: activeInput, sink, options, sessionId: stage1.sessionId });
        await providerMustKeepSideUnloaded({ options, sink, state, name: "restart-side-restored-no-request" });

        const stage4 = await executeStage({
          context,
          cdp: activeCdp,
          input: activeInput,
          sink,
          baseline: state.baseline,
          referenceManifest: stage3PostEvaluatorManifest,
          stage: STAGES[3],
          promptInput: state.promptInputs.stage4,
          expectedSessionId: stage1.sessionId,
        });
        if (!exactSideChatProjection(stage4.terminal, options, stage1.sessionId)) {
          throw productFailure("case5_2-side-chat-final-drift", "Side Chat binding or empty persisted state drifted during Stage 4", {
            side_chat: stage4.terminal.side_chat,
          });
        }
        const finalConfigFailures = mainConfigurationFailures(stage4.terminal, options, { sessionRequired: true });
        if (finalConfigFailures.length > 0) {
          throw productFailure("case5_2-final-quality-config", "final Project Chat effective state drifted from the frozen Main LLM Quality profile", {
            failures: finalConfigFailures,
          });
        }
        const transcript = await exportCanonicalTranscript({
          context,
          input: activeInput,
          sink,
          sessionId: stage1.sessionId,
          prompts: state.promptInputs,
        });
        const stage4Manifest = await storeStageManifest({ context, sink, baseline: state.baseline, stage: "stage4" });
        const evaluated = await runFinalEvaluator({
          context,
          sink,
          state,
          python: state.python,
          baseline: state.baseline,
          preEvaluationManifest: stage4Manifest,
          oracleIdentity: state.oracle,
        });
        const evaluation = evaluated.report;
        const stage4FinalManifest = evaluated.finalManifest;

        const stage5Options = options.sideModel === options.mainModel
          ? options
          : Object.freeze({ ...options, sideModel: options.mainModel });
        if (stage5Options !== options) {
          await configureSideChat({
            cdp: activeCdp,
            input: activeInput,
            sink,
            options: stage5Options,
            sessionId: stage1.sessionId,
            evidenceName: "case5_2-side-chat-stage5-reconfigured",
          });
        }
        await providerMustKeepSideUnloaded({ options, sink, state, name: "stage5-before-side-send" });
        const stage5 = await executeCase52SideChatStage({
          cdp: activeCdp,
          input: activeInput,
          sink,
          sessionId: stage1.sessionId,
          providerProfile: stage5Options.providerProfile,
          providerBaseUrl: stage5Options.providerBaseUrl,
          model: stage5Options.sideModel,
          promptInput: state.sidePromptInput,
          timeoutMs: QUALITY_REQUEST_TIMEOUT_MS,
        });
        const stage5Manifest = await storeStageManifest({
          context,
          sink,
          baseline: state.baseline,
          stage: "stage5-post-side-chat",
        });
        await assertEvaluatorWorkspaceStable({
          sink,
          before: stage4FinalManifest,
          after: stage5Manifest,
          label: "stage5-side-chat-read-only",
        });

        const allPaths = await allWorkspacePaths(context.paths.workspace);
        const forbidden = case52ForbiddenWorkspacePaths(allPaths);
        const scopeFailures = finalScopeFailures(stage5Manifest, forbidden);
        const seedFinal = await inventoryCase52CleanSeed(options.fixtureSource);
        const seedUnchanged = seedFinal.aggregate_sha256 === state.seed.aggregate_sha256
          && sameValue(seedFinal.files, state.seed.files);
        const externalFinal = await externalRootsSnapshot(state.externalRoots);
        const externalDrift = externalRootDrift(state.externalBaseline, externalFinal);
        const finalProvider = await providerMustKeepSideUnloaded({ options, sink, state, name: "final-terminal" });
        const safety = {
          scope_failures: scopeFailures,
          forbidden_workspace_paths: forbidden,
          fixture_seed_unchanged: seedUnchanged,
          fixture_seed_before: {
            aggregate_sha256: state.seed.aggregate_sha256,
            file_count: state.seed.file_count,
            byte_count: state.seed.byte_count,
          },
          fixture_seed_after: {
            aggregate_sha256: seedFinal.aggregate_sha256,
            file_count: seedFinal.file_count,
            byte_count: seedFinal.byte_count,
          },
          external_python_environment_drift: externalDrift,
        };
        await sink.writeJson("case5_2/safety-final.json", { safety, external_environment_after: externalFinal });
        if (scopeFailures.length > 0 || !seedUnchanged || externalDrift.length > 0) {
          throw productFailure("case5_2-safety-scope", "case5_2 detected forbidden frontend/dependency/workspace-external/fixture mutation", safety);
        }

        const providerEvidence = case52ProviderSummaryEvidence(options, state);
        const legacySideSummary = case52LegacySideSummaryV1({
          stage4SideChat: stage4.terminal.side_chat,
          restoredSideChat: restoredProjection.side_chat,
          providerSummary: case52SideProviderSummary(options, state.sideProviderSamples),
        });
        const summary = {
          schema_version: "desktop-e2e.case5_2-summary.v1",
          options: case52EvidenceOptions(options),
          session_id: stage1.sessionId,
          stages: [stage1, stage2, stage3, stage4].map((item) => ({
            stage: item.stage,
            turn_id: item.turnId,
            elapsed_ms: item.elapsed_ms,
          })),
          stage5: {
            stage: stage5.stage,
            elapsed_ms: stage5.elapsed_ms,
            provider_profile: stage5.binding.provider_profile,
            model: stage5.binding.model,
            owner_session_id: stage5.binding.owner_session_id,
            chat_id: stage5.binding.chat_id,
            question: stage5.question,
            answer: stage5.answer,
            active_context_observation: stage5.active_context_observation,
            first_progress_latency_ms: stage5.first_progress_latency_ms,
            command_evidence: stage5.commandEvidence,
            terminal_screenshot: stage5.terminal_screenshot,
            terminal_side_chat: stage5.completedSurface.projection.side_chat,
            request_observation: {
              trusted_side_send_action_count: 1,
              trusted_main_submit_action_count: 0,
              trusted_main_cancel_action_count: 0,
              trusted_side_cancel_action_count: 0,
              persisted_message_count: stage5.completedSurface.projection.side_chat.messages.length,
              provider_generation_request_count: "unverified-no-traffic-ledger",
            },
          },
          restart: restarted.restart,
          ...legacySideSummary,
          transcript,
          evaluation,
          safety,
          ...providerEvidence,
          provider_profile_exact: state.providerProfileExact,
          provider_profile: options.providerProfile,
          provider_lifecycle: options.providerLifecycle,
          provider_extra_body: case52ExtraBodyEvidence(),
          provider_comparability_deviations: state.providerComparabilityDeviations,
          provider_effective_load_config: state.acceptedProviderLoad?.response?.value?.load_config ?? null,
          provider_final: finalProvider.models,
          quality_adjudication: "manual_rubric_pending",
          performance: {
            ...state.performance,
            initial_shell_observation_ms: initialShell.readiness?.elapsed_ms ?? null,
            restart_shell_observation_ms: restartShell.readiness?.elapsed_ms ?? null,
            side_chat_stage5_ms: stage5.elapsed_ms,
            total_elapsed_ms: elapsedSince(context.manifest.started_at),
          },
        };
        const summaryIdentity = await sink.writeJson("case5_2/summary.json", summary);
        await sink.record("case5_2-accepted", { summary: summaryIdentity, session_id: stage1.sessionId }, { phase: "executing", owner: OWNER });
        state.acceptedEnd = true;
        return { acquisition: "pass", oracle: "pass", manual: "pending" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        if (activeInput !== null) {
          try { await cleanupWebviewInput(activeInput, state, "active-generation-finally"); }
          catch (error) {
            if (primaryError === null) throw error;
          }
        }
      }
    },
    async quiesce() {
      if (state.quiesceOutcome !== null) return structuredClone(state.quiesceOutcome);
      const provider = await unloadMainProvider({ options, state });
      const external = structuredClone(state.externalProcessOwner);
      const externalPass = external.in_flight === 0
        && external.started === external.settled
        && external.failures.length === 0;
      state.quiesceOutcome = {
        input: provider.input === "pass" && externalPass ? "pass" : "fail",
        resources: [...provider.resources, {
          kind: "windows-job-external-process-owner",
          contract: "every invocation settles with exact root and descendant zero before return",
          ...external,
        }],
        productFailure: provider.productFailure,
      };
      return structuredClone(state.quiesceOutcome);
    },
    async cleanup() {
      const quiesced = state.quiesceOutcome !== null;
      const pass = quiesced
        && state.quiesceOutcome.input === "pass"
        && state.inputCleanupFailures.length === 0
        && state.scenarioCleanupFailures.length === 0;
      return {
        input: pass ? "pass" : "fail",
        resources: [{
          kind: "manual-case5_2-verification",
          accepted_end: state.acceptedEnd,
          quiesced,
          quiesce_input: state.quiesceOutcome?.input ?? null,
          input_cleanup_failures: state.inputCleanupFailures,
          scenario_cleanup_failures: state.scenarioCleanupFailures,
        }],
      };
    },
  });
}

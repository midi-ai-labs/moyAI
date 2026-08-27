import crypto from "node:crypto";
import path from "node:path";
import { readFile, readdir } from "node:fs/promises";

import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import {
  DesktopCommandProbe,
  assertExactDesktopCommandSequence,
} from "../drivers/desktop_command_probe.mjs";
import {
  WebviewInput,
  assertTrustedProbeSequence,
  assertTrustedTextInsertion,
} from "../drivers/webview_input.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";
import { classifyAcquiredObservationFailure } from "./provider_restart.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";

const OWNER = "scenario:manual.provider-lm-studio-thinking";
const PROFILE = "lm_studio";
const ALTERNATE_PROFILE = "openai_compatible";
const BASELINE_BASE_URL = "http://127.0.0.1:9";
const BASELINE_MODEL = "moyai-e2e-lm-studio-before-save";
const API_KEY_ENV = "";
const SENTINEL_NAME = "E2E_LM_STUDIO_THINKING.txt";
const SENTINEL_TEXT = "moyAI Desktop E2E LM Studio thinking fixture.\n";
const LIVE_TURN_TIMEOUT_MS = 420_000;
const SETTINGS_STABILITY_MS = 500;
const CONTROL_TOKENS = Object.freeze(["<|im_start|>", "<|im_end|>", "<think>", "</think>"]);
export const LM_STUDIO_THINKING_HOST_OWNED_CONFIG_KEYS = Object.freeze([
  "model.max_output_tokens",
  "model.temperature",
  "model.top_p",
  "model.top_k",
  "model.presence_penalty",
  "model.frequency_penalty",
  "model.seed",
  "model.stop_sequences",
  "model.extra_body_json",
  "model.supports_reasoning",
  "model.reasoning_effort",
  "model.reasoning_summary",
  "model.chat_completions_reasoning_parameters",
]);
export const LM_STUDIO_THINKING_FORBIDDEN_WIRE_KEYS = Object.freeze([
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
const LM_STUDIO_THINKING_ALLOWED_RESPONSES_KEYS = Object.freeze([
  "input",
  "instructions",
  "model",
  "parallel_tool_calls",
  "store",
  "stream",
  "tool_choice",
  "tools",
]);
const REQUEST_CAPTURE_DIRECTORY_NAME = "request-capture";

export const LM_STUDIO_THINKING_ARTIFACT_NAME = "THINKING_SMOKE.md";
export const LM_STUDIO_THINKING_ARTIFACT_CONTENT = "# LM Studio thinking smoke\n\nPlan, patch, and tool projection verified.\n";
export const LM_STUDIO_THINKING_PLAN_STEPS = Object.freeze([
  "THINKING_SMOKE.md を作成する",
  "成果物と計画の完了を確認する",
]);
export const LM_STUDIO_THINKING_FINAL = "THINKING_SMOKE.md を作成し、計画を完了しました。";
export const LM_STUDIO_THINKING_PROMPT = `接続・host既定thinking・tool projection の確認です。moyAIからsamplingやthinking設定は変更しません。次の順序と引数を厳守してください。
1. built-in の update_plan を1回呼び、2 stepを「${LM_STUDIO_THINKING_PLAN_STEPS[0]}」=in_progress、「${LM_STUDIO_THINKING_PLAN_STEPS[1]}」=pending としてください。
2. built-in の apply_patch をちょうど1回だけ呼び、current directory直下へ ${LM_STUDIO_THINKING_ARTIFACT_NAME} を新規作成してください。UTF-8本文は次の囲み内とbyte-identicalにし、末尾改行を1つ付けてください。
---BEGIN---
${LM_STUDIO_THINKING_ARTIFACT_CONTENT}---END---
3. built-in の update_plan をもう1回だけ呼び、同じ2 stepを両方completedにしてください。
4. 最終回答は「${LM_STUDIO_THINKING_FINAL}」の日本語1文だけにしてください。
read、write、shell、current_time、その他のツールは使わず、apply_patch以外でファイルを変更しないでください。`;

const SHOW_SETTINGS = Object.freeze({
  selector: 'aside.sidebar button.settings[data-action="show-config"][title="設定"]',
  identity: { tag: "BUTTON", action: "show-config" },
});
const BASE_URL = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] input.settings-control[data-config-key="model.base_url"]',
  identity: { tag: "INPUT", configKey: "model.base_url" },
});
const PROFILE_SELECT = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] select.settings-control[data-config-key="model.provider_profile"]',
  identity: { tag: "SELECT", configKey: "model.provider_profile" },
});
const MODEL_DETAILS = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] details[data-details-key="main-provider-manual-model"] > summary',
  identity: { tag: "DETAILS", detailsKey: "main-provider-manual-model" },
});
const MODEL_MANUAL = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] input#main-provider-model-manual[data-config-key="model.model"]',
  identity: { tag: "INPUT", id: "main-provider-model-manual", configKey: "model.model" },
});
const SAVE_GLOBAL_CONFIG = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] button[data-action="save-global-config"]',
  identity: { tag: "BUTTON", action: "save-global-config" },
});
const CLOSE_SETTINGS = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] button[data-action="close-overlay"]',
  identity: { tag: "BUTTON", action: "close-overlay" },
});
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

function canonicalBaseUrl(value) {
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new TypeError("manual.provider-lm-studio-thinking provider_base_url must be a non-empty string");
  }
  let url;
  try { url = new URL(value.trim()); }
  catch (error) { throw new TypeError(`manual.provider-lm-studio-thinking provider_base_url is invalid: ${error.message}`); }
  if (!new Set(["http:", "https:"]).has(url.protocol)
    || url.username.length > 0
    || url.password.length > 0
    || url.search.length > 0
    || url.hash.length > 0) {
    throw new TypeError("manual.provider-lm-studio-thinking provider_base_url must be one credential-free HTTP(S) endpoint without query or fragment");
  }
  return url.toString().replace(/\/$/u, "");
}

function canonicalModel(value) {
  if (typeof value !== "string") {
    throw new TypeError("manual.provider-lm-studio-thinking model must be a string");
  }
  const normalized = value.trim();
  if (normalized.length === 0
    || Buffer.byteLength(normalized, "utf8") > 1024
    || /[\u0000-\u001f\u007f]/u.test(normalized)) {
    throw new TypeError("manual.provider-lm-studio-thinking model must be a non-empty bounded model ID without control characters");
  }
  return normalized;
}

export function normalizeLmStudioThinkingOptions(options) {
  if (options === null || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("manual.provider-lm-studio-thinking requires one scenario config object");
  }
  const allowed = new Set(["provider_base_url", "model"]);
  const unknown = Object.keys(options).filter((key) => !allowed.has(key));
  if (unknown.length > 0) {
    throw new TypeError(`unknown manual.provider-lm-studio-thinking option: ${unknown.join(",")}`);
  }
  return Object.freeze({
    providerBaseUrl: canonicalBaseUrl(options.provider_base_url),
    model: canonicalModel(options.model),
  });
}

export function lmStudioThinkingFixtureConfig() {
  return `[model]
base_url = ${JSON.stringify(BASELINE_BASE_URL)}
model = ${JSON.stringify(BASELINE_MODEL)}
provider_profile = ${JSON.stringify(PROFILE)}
provider_metadata_mode = "lm_studio_native_required"
provider_api_mode = "responses"
connect_timeout_ms = 10000
request_timeout_ms = ${LIVE_TURN_TIMEOUT_MS}
max_retries = 0
context_window = 32768
supports_tools = true
supports_images = false
parallel_tool_calls = false
max_parallel_predictions = 1

[permissions]
access_mode = "default"

[format]
default_newline = "lf"
ensure_trailing_newline = true

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

function desiredConnection(options) {
  return {
    baseUrl: options.providerBaseUrl,
    model: options.model,
    providerProfile: PROFILE,
    apiKeyEnv: API_KEY_ENV,
  };
}

function fieldValue(projection, key) {
  const rows = Array.isArray(projection?.config_fields) ? projection.config_fields : [];
  const matches = rows.filter((row) => row?.key === key);
  return matches.length === 1 ? matches[0].value : null;
}

function configValues(projection, overrides = {}) {
  const fields = Array.isArray(projection?.config_fields) ? projection.config_fields : [];
  return fields.map((field) => ({
    key: field.key,
    text: Object.hasOwn(overrides, field.key) ? overrides[field.key] : field.value,
  }));
}

function projectedHostOwnedGenerationKeys(projection) {
  const fields = Array.isArray(projection?.config_fields) ? projection.config_fields : [];
  return [...new Set(fields
    .map((field) => field?.key)
    .filter((key) => LM_STUDIO_THINKING_HOST_OWNED_CONFIG_KEYS.includes(key)))]
    .sort();
}

export function expectedLmStudioThinkingGlobalSave(surface, options) {
  const projection = surface?.projection;
  if (!projection?.config_target) throw new TypeError("LM Studio thinking save expectation requires a config target");
  const projectedHostOwnedKeys = projectedHostOwnedGenerationKeys(projection);
  if (projectedHostOwnedKeys.length > 0) {
    throw new TypeError(`LM Studio thinking save projection contains host-owned generation fields: ${projectedHostOwnedKeys.join(",")}`);
  }
  const desired = desiredConnection(options);
  return {
    command: "save_global_config",
    args: {
      values: configValues(projection, {
        "model.base_url": desired.baseUrl,
        "model.model": desired.model,
        "model.provider_profile": desired.providerProfile,
        "model.api_key_env": desired.apiKeyEnv,
      }),
      expectedTarget: structuredClone(projection.config_target),
    },
  };
}

export async function observeLmStudioThinkingSurface(cdp) {
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
        && rect.width > 0 && rect.height > 0 && element.closest('[hidden], [inert], [aria-hidden="true"]') === null;
    };
    const matches = (selector) => Array.from(document.querySelectorAll(selector));
    const one = (selector) => {
      const rows = matches(selector);
      const node = rows.length === 1 ? rows[0] : null;
      return { count: rows.length, node, visible: visible(node) };
    };
    const control = (selector) => {
      const found = one(selector);
      const node = found.node;
      return {
        count: found.count,
        visible: found.visible,
        enabled: (node instanceof HTMLInputElement || node instanceof HTMLSelectElement || node instanceof HTMLTextAreaElement)
          && !node.disabled && !node.readOnly,
        value: node instanceof HTMLInputElement || node instanceof HTMLSelectElement || node instanceof HTMLTextAreaElement
          ? node.value
          : null,
        options: node instanceof HTMLSelectElement ? Array.from(node.options).map((option) => option.value) : [],
      };
    };
    const button = (selector) => {
      const found = one(selector);
      return {
        count: found.count,
        visible: found.visible,
        enabled: found.node instanceof HTMLButtonElement
          && !found.node.disabled
          && found.node.getAttribute('aria-disabled') !== 'true',
      };
    };
    const text = (selector) => {
      const found = one(selector);
      return {
        count: found.count,
        visible: found.visible,
        text: found.node instanceof HTMLElement ? found.node.innerText.trim() : null,
      };
    };
    const transcript = (selector) => matches(selector).map((row) => ({
      text: (row.querySelector('.markdown-body')?.innerText ?? '').trim(),
      visible: visible(row),
    }));
    const settingsDialog = one('[role="dialog"][aria-labelledby="config-dialog-title"]');
    const hostOwnedConfigKeyCounts = Object.fromEntries(${JSON.stringify(LM_STUDIO_THINKING_HOST_OWNED_CONFIG_KEYS)}.map((key) => [
      key,
      settingsDialog.node instanceof HTMLElement
        ? Array.from(settingsDialog.node.querySelectorAll('[data-config-key]'))
          .filter((node) => node.getAttribute('data-config-key') === key).length
        : 0,
    ]));
    const details = one('[role="dialog"][aria-labelledby="config-dialog-title"] details[data-details-key="main-provider-manual-model"]');
    const planSection = one('aside.artifact-pane[data-pane-mode="output"] section.output-plan-section[aria-labelledby="output-plan-heading"]');
    const planItems = matches('aside.artifact-pane[data-pane-mode="output"] section.output-plan-section ol.plan-list > li').map((row) => ({
      status: row.getAttribute('data-plan-status'),
      step: (row.querySelector('.plan-step-copy')?.textContent ?? '').trim(),
      status_text: (row.querySelector('.plan-step-status')?.textContent ?? '').trim(),
      visible: visible(row),
    }));
    const artifactRows = matches('aside.artifact-pane[data-pane-mode="output"] button.artifact-row').map((row) => ({
      label: (row.querySelector('.artifact-row-copy b')?.textContent ?? '').trim(),
      path: (row.querySelector('.artifact-row-copy small')?.textContent ?? '').trim(),
      visible: visible(row),
    }));
    return {
      projection,
      settings: {
        dialog: { count: settingsDialog.count, visible: settingsDialog.visible },
        host_owned_config_key_counts: hostOwnedConfigKeyCounts,
        base_url: control(${JSON.stringify(BASE_URL.selector)}),
        profile: control(${JSON.stringify(PROFILE_SELECT.selector)}),
        model_details: { count: details.count, visible: details.visible, open: details.node instanceof HTMLDetailsElement && details.node.open },
        model: control(${JSON.stringify(MODEL_MANUAL.selector)}),
        dirty: matches('[role="dialog"][aria-labelledby="config-dialog-title"] .dirty-badge.visible').filter(visible).length === 1,
        save: button(${JSON.stringify(SAVE_GLOBAL_CONFIG.selector)}),
        close: button(${JSON.stringify(CLOSE_SETTINGS.selector)}),
      },
      prompt: control(${JSON.stringify(PROMPT.selector)}),
      send: button(${JSON.stringify(SEND.selector)}),
      assistants: transcript('main.conversation #thread article.message.assistant'),
      reasoning_summaries: transcript('main.conversation #thread article.message.reasoning_summary'),
      plan_dom: {
        count: planSection.count,
        visible: planSection.visible,
        heading: text('aside.artifact-pane[data-pane-mode="output"] #output-plan-heading'),
        count_text: text('aside.artifact-pane[data-pane-mode="output"] section.output-plan-section .output-section-count'),
        items: planItems,
      },
      artifact_dom: {
        heading: text('aside.artifact-pane[data-pane-mode="output"] #output-files-heading'),
        rows: artifactRows,
      },
      terminal_dom: {
        topbar_title: text('header.topbar h1'),
        topbar_status: text('header.topbar .status-line > span'),
        visible_run_strip_count: matches('section.run-strip').filter(visible).length,
        visible_task_activity_indicator_count: matches('.task-activity-indicator').filter(visible).length,
        visible_selected_activity_row_count: matches('aside.sidebar .nav-row-wrap.selected[data-task-activity-row]').filter(visible).length,
      },
      visible_fatal_count: matches('.fatal').filter(visible).length,
      visible_recoverable_error_count: matches('.ui-error-notice').filter(visible).length,
      visible_validation_error_count: matches('.validation.error').filter(visible).length,
      visible_transcript_error_count: matches('main.conversation #thread article.message.error').filter(visible).length,
    };
  })()`);
}

function surfaceErrorFree(surface) {
  return surface?.visible_fatal_count === 0
    && surface?.visible_recoverable_error_count === 0
    && surface?.visible_validation_error_count === 0
    && surface?.visible_transcript_error_count === 0;
}

function exactSettings(surface, desired, { dirty, detailsOpen = true } = {}) {
  return surface?.projection?.overlay === "config"
    && surface?.settings?.dialog?.count === 1
    && surface.settings.dialog.visible === true
    && projectedHostOwnedGenerationKeys(surface.projection).length === 0
    && LM_STUDIO_THINKING_HOST_OWNED_CONFIG_KEYS.every((key) => (
      surface.settings.host_owned_config_key_counts?.[key] === 0
    ))
    && surface.settings.base_url.count === 1
    && surface.settings.base_url.visible === true
    && surface.settings.base_url.enabled === true
    && surface.settings.base_url.value === desired.baseUrl
    && surface.settings.profile.count === 1
    && surface.settings.profile.visible === true
    && surface.settings.profile.enabled === true
    && surface.settings.profile.value === desired.providerProfile
    && surface.settings.profile.options.includes(PROFILE)
    && surface.settings.model_details.count === 1
    && surface.settings.model_details.open === detailsOpen
    && surface.settings.model.count === 1
    && (!detailsOpen || surface.settings.model.visible === true)
    && surface.settings.model.enabled === true
    && surface.settings.model.value === desired.model
    && surface.settings.dirty === dirty
    && surface.settings.save.count === 1
    && surface.settings.save.visible === true
    && surface.settings.save.enabled === dirty
    && surface.settings.close.count === 1
    && surface.settings.close.visible === true
    && surfaceErrorFree(surface);
}

function requestCaptureFailure(code, evidence = {}) {
  return { code, ...evidence };
}

function exactObjectKeys(value, expected) {
  return value !== null
    && typeof value === "object"
    && !Array.isArray(value)
    && JSON.stringify(Object.keys(value).sort()) === JSON.stringify([...expected].sort());
}

export function lmStudioThinkingRequestCaptureFailures(captures, options) {
  if (!Array.isArray(captures)) throw new TypeError("LM Studio thinking request captures must be an array");
  const failures = [];
  if (captures.length === 0) {
    failures.push(requestCaptureFailure("request-capture-empty"));
    return failures;
  }
  const requestIds = new Set();
  const sequences = new Set();
  for (const [index, capture] of captures.entries()) {
    const metadata = capture?.metadata;
    const body = capture?.body;
    const context = { capture_index: index, metadata_file: capture?.metadata_file ?? null };
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
    ])) {
      failures.push(requestCaptureFailure("request-capture-metadata-shape-mismatch", context));
      continue;
    }
    if (metadata.schema_version !== 2
      || metadata.transport !== "http"
      || metadata.capture_stage !== "prepared") {
      failures.push(requestCaptureFailure("request-capture-metadata-contract-mismatch", context));
    }
    if (metadata.api_mode !== "responses" || metadata.endpoint_path !== "v1/responses") {
      failures.push(requestCaptureFailure("request-capture-target-mismatch", {
        ...context,
        api_mode: metadata.api_mode,
        endpoint_path: metadata.endpoint_path,
      }));
    }
    const expectedStem = Number.isInteger(metadata.captured_at_unix_ms)
      && Number.isInteger(metadata.process_id)
      && Number.isInteger(metadata.sequence)
      && typeof metadata.api_mode === "string"
      ? `${String(metadata.captured_at_unix_ms).padStart(20, "0")}-${String(metadata.process_id).padStart(10, "0")}-${String(metadata.sequence).padStart(10, "0")}-${metadata.api_mode}`
      : null;
    if (expectedStem === null
      || capture?.metadata_file !== `${expectedStem}.metadata.json`
      || capture?.request_body_file !== `${expectedStem}.request.json`) {
      failures.push(requestCaptureFailure("request-capture-filename-owner-mismatch", context));
    }
    if (typeof metadata.request_id !== "string" || metadata.request_id.length === 0
      || requestIds.has(metadata.request_id)) {
      failures.push(requestCaptureFailure("request-capture-request-id-invalid", context));
    } else {
      requestIds.add(metadata.request_id);
    }
    const sequenceOwner = `${metadata.process_id}:${metadata.sequence}`;
    if (!Number.isInteger(metadata.process_id) || metadata.process_id < 1
      || !Number.isInteger(metadata.sequence) || metadata.sequence < 0
      || sequences.has(sequenceOwner)) {
      failures.push(requestCaptureFailure("request-capture-sequence-invalid", context));
    } else {
      sequences.add(sequenceOwner);
    }
    if (!Number.isInteger(metadata.captured_at_unix_ms) || metadata.captured_at_unix_ms < 1
      || !Number.isInteger(metadata.request_body_bytes) || metadata.request_body_bytes < 1
      || metadata.request_body_bytes !== capture?.request_body_bytes) {
      failures.push(requestCaptureFailure("request-capture-byte-metadata-mismatch", {
        ...context,
        metadata_bytes: metadata.request_body_bytes,
        actual_bytes: capture?.request_body_bytes ?? null,
      }));
    }
    if (metadata.request_body_file !== capture?.request_body_file) {
      failures.push(requestCaptureFailure("request-capture-body-owner-mismatch", context));
    }
    if (body === null || typeof body !== "object" || Array.isArray(body)) {
      failures.push(requestCaptureFailure("request-capture-body-shape-mismatch", context));
      continue;
    }
    const forbiddenKeys = LM_STUDIO_THINKING_FORBIDDEN_WIRE_KEYS.filter((key) => Object.hasOwn(body, key));
    if (forbiddenKeys.length > 0) {
      failures.push(requestCaptureFailure("request-capture-generation-override-present", {
        ...context,
        forbidden_keys: forbiddenKeys,
      }));
    }
    const unexpectedKeys = Object.keys(body)
      .filter((key) => !LM_STUDIO_THINKING_ALLOWED_RESPONSES_KEYS.includes(key))
      .sort();
    if (unexpectedKeys.length > 0) {
      failures.push(requestCaptureFailure("request-capture-unexpected-top-level-key", {
        ...context,
        unexpected_keys: unexpectedKeys,
      }));
    }
    if (body.model !== options.model
      || typeof body.instructions !== "string"
      || body.instructions.trim().length === 0
      || !Array.isArray(body.input)
      || body.input.length === 0
      || body.store !== false
      || body.stream !== true) {
      failures.push(requestCaptureFailure("request-capture-structural-contract-mismatch", {
        ...context,
        model_matches: body.model === options.model,
        instructions_non_empty: typeof body.instructions === "string" && body.instructions.trim().length > 0,
        input_count: Array.isArray(body.input) ? body.input.length : null,
        store: body.store ?? null,
        stream: body.stream ?? null,
      }));
    }
  }
  return failures;
}

function decodeJsonCapture(bytes, label) {
  let text;
  try {
    text = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch (error) {
    throw productFailure("lm-studio-thinking-request-capture-invalid-utf8", `${label} is not valid UTF-8`, {
      cause: errorObservation(error),
    });
  }
  try {
    return JSON.parse(text);
  } catch (error) {
    throw productFailure("lm-studio-thinking-request-capture-invalid-json", `${label} is not valid JSON`, {
      cause: errorObservation(error),
    });
  }
}

export async function inspectLmStudioThinkingRequestCaptures(directory, options) {
  if (typeof directory !== "string" || !path.isAbsolute(directory)) {
    throw new TypeError("LM Studio thinking request capture directory must be absolute");
  }
  let entries;
  try {
    entries = await readdir(directory, { withFileTypes: true });
  } catch (error) {
    if (error?.code === "ENOENT") {
      throw productFailure(
        "lm-studio-thinking-request-capture-missing",
        "the configured prepared-request capture directory was not created",
        { directory },
      );
    }
    throw new DesktopE2eError(
      "harness",
      "lm-studio-thinking-request-capture-read-failed",
      `the prepared-request capture directory could not be read: ${error.message}`,
      { directory, cause: errorObservation(error) },
    );
  }
  const files = entries.filter((entry) => entry.isFile()).map((entry) => entry.name).sort();
  const invalidEntries = entries.filter((entry) => !entry.isFile()
    || (!entry.name.endsWith(".metadata.json") && !entry.name.endsWith(".request.json")))
    .map((entry) => entry.name)
    .sort();
  if (invalidEntries.length > 0) {
    throw productFailure(
      "lm-studio-thinking-request-capture-entry-mismatch",
      "the prepared-request capture directory contains an unexpected entry",
      { invalid_entries: invalidEntries },
    );
  }
  const metadataFiles = files.filter((name) => name.endsWith(".metadata.json"));
  const requestFiles = new Set(files.filter((name) => name.endsWith(".request.json")));
  if (metadataFiles.length === 0) {
    if (requestFiles.size > 0) {
      throw productFailure(
        "lm-studio-thinking-request-capture-orphan",
        "prepared-request capture contains request bodies without committed metadata",
        { orphan_request_files: [...requestFiles].sort() },
      );
    }
    throw productFailure(
      "lm-studio-thinking-request-capture-empty",
      "the completed live turn produced no prepared Responses request capture",
      { files },
    );
  }
  const captures = [];
  const claimedRequestFiles = new Set();
  for (const metadataFile of metadataFiles) {
    let metadataBytes;
    try {
      metadataBytes = await readFile(path.join(directory, metadataFile));
    } catch (error) {
      throw new DesktopE2eError(
        "harness",
        "lm-studio-thinking-request-capture-read-failed",
        `prepared-request metadata could not be read: ${error.message}`,
        { metadata_file: metadataFile, cause: errorObservation(error) },
      );
    }
    const metadata = decodeJsonCapture(metadataBytes, `request capture metadata ${metadataFile}`);
    const expectedBodyFile = `${metadataFile.slice(0, -".metadata.json".length)}.request.json`;
    const requestBodyFile = metadata?.request_body_file;
    if (typeof requestBodyFile !== "string"
      || path.basename(requestBodyFile) !== requestBodyFile
      || requestBodyFile !== expectedBodyFile
      || !requestFiles.has(requestBodyFile)
      || claimedRequestFiles.has(requestBodyFile)) {
      throw productFailure(
        "lm-studio-thinking-request-capture-pair-mismatch",
        "prepared-request metadata does not own one exact adjacent request body",
        { metadata_file: metadataFile, request_body_file: requestBodyFile ?? null, expected_body_file: expectedBodyFile },
      );
    }
    claimedRequestFiles.add(requestBodyFile);
    let bodyBytes;
    try {
      bodyBytes = await readFile(path.join(directory, requestBodyFile));
    } catch (error) {
      throw new DesktopE2eError(
        "harness",
        "lm-studio-thinking-request-capture-read-failed",
        `prepared-request body could not be read: ${error.message}`,
        { request_body_file: requestBodyFile, cause: errorObservation(error) },
      );
    }
    captures.push({
      metadata_file: metadataFile,
      request_body_file: requestBodyFile,
      metadata,
      body: decodeJsonCapture(bodyBytes, `request capture body ${requestBodyFile}`),
      request_body_bytes: bodyBytes.byteLength,
      request_body_sha256: crypto.createHash("sha256").update(bodyBytes).digest("hex"),
    });
  }
  const orphanRequestFiles = [...requestFiles].filter((name) => !claimedRequestFiles.has(name)).sort();
  if (orphanRequestFiles.length > 0) {
    throw productFailure(
      "lm-studio-thinking-request-capture-orphan",
      "prepared-request capture contains an uncommitted or unowned request body",
      { orphan_request_files: orphanRequestFiles },
    );
  }
  const failures = lmStudioThinkingRequestCaptureFailures(captures, options);
  const evidence = {
    schema_version: "desktop-e2e.lm-studio-thinking-prepared-outbound-body.v1",
    evidence_kind: "exact-prepared-outbound-body",
    capture_stage: "prepared",
    network_attempt_proven: false,
    provider_receipt_proven: false,
    capture_count: captures.length,
    forbidden_wire_keys: [...LM_STUDIO_THINKING_FORBIDDEN_WIRE_KEYS],
    captures: captures.map((capture) => ({
      metadata_file: capture.metadata_file,
      request_body_file: capture.request_body_file,
      request_id: capture.metadata.request_id ?? null,
      process_id: capture.metadata.process_id ?? null,
      sequence: capture.metadata.sequence ?? null,
      api_mode: capture.metadata.api_mode ?? null,
      endpoint_path: capture.metadata.endpoint_path ?? null,
      request_body_bytes: capture.request_body_bytes,
      request_body_sha256: capture.request_body_sha256,
      top_level_keys: capture.body !== null && typeof capture.body === "object" && !Array.isArray(capture.body)
        ? Object.keys(capture.body).sort()
        : null,
      input_count: Array.isArray(capture.body?.input) ? capture.body.input.length : null,
      instructions_sha256: typeof capture.body?.instructions === "string"
        ? crypto.createHash("sha256").update(capture.body.instructions).digest("hex")
        : null,
      model_matches: capture.body?.model === options.model,
    })),
    failures,
  };
  if (failures.length > 0) {
    throw productFailure(
      "lm-studio-thinking-request-capture-contract-mismatch",
      "the exact prepared Responses request did not inherit the host generation settings",
      evidence,
    );
  }
  return evidence;
}

function persistedConnectionReady(surface, desired) {
  const projection = surface?.projection;
  return fieldValue(projection, "model.base_url") === desired.baseUrl
    && fieldValue(projection, "model.model") === desired.model
    && fieldValue(projection, "model.provider_profile") === PROFILE
    && fieldValue(projection, "model.api_key_env") === API_KEY_ENV
    && fieldValue(projection, "model.supports_tools") === "true"
    && fieldValue(projection, "permissions.access_mode") === "default"
    && projection?.provider_effective_base_url === desired.baseUrl
    && projection?.provider_effective_model_id === desired.model
    && projection?.provider_effective_profile === PROFILE
    && projection?.provider_effective_api_key_env === API_KEY_ENV;
}

export function restoredLmStudioThinkingConnectionReady(surface, options) {
  const desired = desiredConnection(options);
  return exactSettings(surface, desired, { dirty: false })
    && persistedConnectionReady(surface, desired);
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

function savedConnectionReady(surface, options, baselineTarget) {
  const desired = desiredConnection(options);
  return exactSettings(surface, desired, { dirty: false, detailsOpen: false })
    && persistedConnectionReady(surface, desired)
    && advancedConfigTarget(surface?.projection?.config_target, baselineTarget);
}

export function lmStudioThinkingControlTokenLeaks(projection) {
  const rows = Array.isArray(projection?.transcript_rows) ? projection.transcript_rows : [];
  return rows.flatMap((row, rowIndex) => {
    if (!["assistant", "reasoning_summary"].includes(row?.row_kind) || typeof row.body !== "string") return [];
    const markers = CONTROL_TOKENS.filter((marker) => row.body.includes(marker));
    return markers.length === 0 ? [] : [{ row_index: rowIndex, markers }];
  });
}

function exactPlan(plan, statuses) {
  return plan?.steps?.length === 2
    && plan.steps.every((step, index) => step?.step === LM_STUDIO_THINKING_PLAN_STEPS[index]
      && step?.status === statuses[index]);
}

function exactPlanDom(planDom, statuses) {
  const labels = statuses.map((status) => status === "completed" ? "完了" : status === "in_progress" ? "進行中" : "未着手");
  return planDom?.count === 1
    && planDom.visible === true
    && planDom.heading?.count === 1
    && planDom.heading.visible === true
    && planDom.heading.text === "計画"
    && planDom.count_text?.count === 1
    && planDom.count_text.visible === true
    && planDom.count_text.text === "2件"
    && Array.isArray(planDom.items)
    && planDom.items.length === 2
    && planDom.items.every((item, index) => item.visible === true
      && item.step === LM_STUDIO_THINKING_PLAN_STEPS[index]
      && item.status === statuses[index]
      && item.status_text === labels[index]);
}

export function lmStudioThinkingActivePlanAccepted(surface) {
  return surfaceErrorFree(surface)
    && lmStudioThinkingControlTokenLeaks(surface?.projection).length === 0
    && exactPlan(surface?.projection?.plan, ["in_progress", "pending"])
    && exactPlanDom(surface?.plan_dom, ["in_progress", "pending"]);
}

function transcriptRowsOfKind(projection, kind) {
  const rows = Array.isArray(projection?.transcript_rows) ? projection.transcript_rows : [];
  return rows.filter((row) => row?.row_kind === kind);
}

function exactToolProjection(projection) {
  if (typeof projection?.tool_status_text !== "string") return false;
  const lines = projection.tool_status_text.split(/\r?\n/u).filter((line) => line.startsWith("- "));
  const planLines = lines.filter((line) => /^- Plan updated \[completed\](?: |$)/u.test(line));
  const patchLines = lines.filter((line) => /^- Applied 1 change\(s\) \[completed\](?: |$)/u.test(line));
  return lines.length === 3
    && planLines.length === 2
    && patchLines.length === 1
    && typeof projection.progress_text === "string"
    && projection.progress_text.includes("ツール: 3件開始 / 3件完了 / 0件拒否 / 0件キャンセル / 0件失敗");
}

function exactArtifactProjection(surface) {
  const projection = surface?.projection;
  const artifacts = Array.isArray(projection?.artifact_rows) ? projection.artifact_rows : [];
  const changes = Array.isArray(projection?.file_change_rows) ? projection.file_change_rows : [];
  return artifacts.length === 1
    && artifacts[0]?.label === LM_STUDIO_THINKING_ARTIFACT_NAME
    && artifacts[0]?.path === LM_STUDIO_THINKING_ARTIFACT_NAME
    && artifacts[0]?.action === "追加"
    && changes.length === 1
    && changes[0]?.label === LM_STUDIO_THINKING_ARTIFACT_NAME
    && changes[0]?.path === LM_STUDIO_THINKING_ARTIFACT_NAME
    && changes[0]?.action === "追加"
    && surface?.artifact_dom?.heading?.count === 1
    && surface.artifact_dom.heading.visible === true
    && surface.artifact_dom.heading.text === "ファイル"
    && Array.isArray(surface?.artifact_dom?.rows)
    && surface.artifact_dom.rows.length === 1
    && surface.artifact_dom.rows[0]?.label === LM_STUDIO_THINKING_ARTIFACT_NAME
    && surface.artifact_dom.rows[0]?.path === LM_STUDIO_THINKING_ARTIFACT_NAME
    && surface.artifact_dom.rows[0]?.visible === true;
}

function exactArtifactIdentity(identity) {
  return identity?.name === LM_STUDIO_THINKING_ARTIFACT_NAME
    && identity?.content === LM_STUDIO_THINKING_ARTIFACT_CONTENT
    && identity?.size_bytes === Buffer.byteLength(LM_STUDIO_THINKING_ARTIFACT_CONTENT, "utf8")
    && identity?.sha256 === crypto.createHash("sha256").update(LM_STUDIO_THINKING_ARTIFACT_CONTENT).digest("hex")
    && identity?.workspace_entries?.length === 2
    && identity.workspace_entries[0] === SENTINEL_NAME
    && identity.workspace_entries[1] === LM_STUDIO_THINKING_ARTIFACT_NAME;
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
    && projection?.can_submit === true;
}

export function lmStudioThinkingTerminalFailures(surface, artifactIdentity) {
  const projection = surface?.projection;
  const assistants = transcriptRowsOfKind(projection, "assistant");
  const users = transcriptRowsOfKind(projection, "user");
  const errors = transcriptRowsOfKind(projection, "error");
  const failures = [];
  if (!surfaceErrorFree(surface) || errors.length !== 0) failures.push("terminal-error-present");
  if (!terminalSettled(projection)) failures.push("terminal-surface-not-settled");
  if (lmStudioThinkingControlTokenLeaks(projection).length > 0) failures.push("provider-control-token-visible");
  if (users.length !== 1 || users[0]?.body !== LM_STUDIO_THINKING_PROMPT) failures.push("terminal-user-not-exact");
  if (assistants.length !== 1 || assistants[0]?.body !== LM_STUDIO_THINKING_FINAL) failures.push("terminal-assistant-not-exact");
  if (surface?.assistants?.length !== 1
    || surface.assistants[0]?.visible !== true
    || surface.assistants[0]?.text !== LM_STUDIO_THINKING_FINAL) failures.push("terminal-assistant-dom-not-exact");
  if (!exactPlan(projection?.plan, ["completed", "completed"])
    || !exactPlanDom(surface?.plan_dom, ["completed", "completed"])) failures.push("terminal-plan-not-completed");
  if (!exactToolProjection(projection)) failures.push("terminal-tool-projection-mismatch");
  if (!exactArtifactProjection(surface)) failures.push("terminal-artifact-projection-mismatch");
  if (!exactArtifactIdentity(artifactIdentity)) failures.push("terminal-artifact-bytes-mismatch");
  if (surface?.terminal_dom?.visible_run_strip_count !== 0
    || surface?.terminal_dom?.visible_task_activity_indicator_count !== 0
    || surface?.terminal_dom?.visible_selected_activity_row_count !== 0) failures.push("terminal-activity-dom-present");
  return [...new Set(failures)];
}

function lmStudioThinkingCanonicalTerminalFailures(surface, artifactIdentity) {
  const projection = surface?.projection;
  const assistants = transcriptRowsOfKind(projection, "assistant");
  const users = transcriptRowsOfKind(projection, "user");
  const errors = transcriptRowsOfKind(projection, "error");
  const failures = [];
  if (!surfaceErrorFree(surface) || errors.length !== 0) failures.push("terminal-error-present");
  if (lmStudioThinkingControlTokenLeaks(projection).length > 0) failures.push("provider-control-token-visible");
  if (users.length !== 1 || users[0]?.body !== LM_STUDIO_THINKING_PROMPT) failures.push("terminal-user-not-exact");
  if (assistants.length !== 1 || assistants[0]?.body !== LM_STUDIO_THINKING_FINAL) failures.push("terminal-assistant-not-exact");
  if (!exactPlan(projection?.plan, ["completed", "completed"])) failures.push("terminal-plan-not-completed");
  if (!exactToolProjection(projection)) failures.push("terminal-tool-projection-mismatch");
  if (!exactArtifactIdentity(artifactIdentity)) failures.push("terminal-artifact-bytes-mismatch");
  return [...new Set(failures)];
}

export function lmStudioThinkingTerminalDecision(surface, artifactIdentity) {
  if (!surfaceErrorFree(surface) || lmStudioThinkingControlTokenLeaks(surface?.projection).length > 0) return "fail";
  if (surface?.projection?.run_status_key !== "completed") return "pending";
  if (!terminalSettled(surface.projection)) return "pending";
  if (lmStudioThinkingCanonicalTerminalFailures(surface, artifactIdentity).length > 0) return "fail";
  return lmStudioThinkingTerminalFailures(surface, artifactIdentity).length === 0 ? "pass" : "pending";
}

async function artifactIdentity(workspace) {
  const entries = await readdir(workspace, { withFileTypes: true });
  if (entries.some((entry) => !entry.isFile())) {
    return { workspace_entries: entries.map((entry) => entry.name).sort(), unsupported_entry: true };
  }
  const workspaceEntries = entries.map((entry) => entry.name).sort();
  const artifactEntry = entries.find((entry) => entry.isFile() && (
    process.platform === "win32"
      ? entry.name.toLowerCase() === LM_STUDIO_THINKING_ARTIFACT_NAME.toLowerCase()
      : entry.name === LM_STUDIO_THINKING_ARTIFACT_NAME
  ));
  if (!artifactEntry) {
    return { workspace_entries: workspaceEntries, error: { message: "artifact entry is missing" } };
  }
  let bytes;
  try { bytes = await readFile(path.join(workspace, artifactEntry.name)); }
  catch (error) {
    return { workspace_entries: workspaceEntries, error: errorObservation(error) };
  }
  return {
    name: artifactEntry.name,
    content: bytes.toString("utf8"),
    size_bytes: bytes.byteLength,
    sha256: crypto.createHash("sha256").update(bytes).digest("hex"),
    workspace_entries: workspaceEntries,
  };
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

async function exactDomValue(cdp, selector) {
  return cdp.evaluate(`(() => {
    const rows = document.querySelectorAll(${JSON.stringify(selector)});
    const node = rows.length === 1 ? rows[0] : null;
    return {
      count: rows.length,
      value: node instanceof HTMLInputElement || node instanceof HTMLTextAreaElement || node instanceof HTMLSelectElement
        ? node.value
        : null,
    };
  })()`);
}

async function trustedReplaceText({ cdp, input, locator, text }) {
  const initial = await exactDomValue(cdp, locator.selector);
  if (initial.count !== 1) throw new Error("trusted text replacement target cardinality drifted");
  if (initial.value === text) return { changed: false, initial };
  const click = await trustedClick(input, locator);
  const start = (await input.snapshotProbe()).sequence;
  await input.keyDown("Control");
  try { await input.pressKey("a"); }
  finally { await input.keyUp("Control"); }
  await input.pressKey("Backspace");
  let insertion = null;
  if (text.length > 0) {
    const insertionStart = (await input.snapshotProbe()).sequence;
    const inserted = await input.insertText(locator, text);
    insertion = assertTrustedTextInsertion(await input.snapshotProbe(insertionStart), {
      afterSequence: insertionStart,
      identity: locator.identity,
      text,
    });
    insertion = { inserted, probe: insertion };
  }
  const final = await waitForObservation({
    label: "exact trusted replacement value",
    timeoutMs: 10_000,
    pollMs: 50,
    retrySampleErrors: false,
    sample: () => exactDomValue(cdp, locator.selector),
    accept: (value) => value.count === 1 && value.value === text,
  });
  return { changed: true, initial, click, start, insertion, final: final.value };
}

async function trustedSelectProfile({ cdp, input, value }) {
  const locator = PROFILE_SELECT;
  const initial = await exactDomValue(cdp, locator.selector);
  if (initial.count !== 1) throw new Error("provider profile target cardinality drifted");
  if (initial.value === value) return { changed: false, initial };
  const click = await trustedClick(input, locator);
  const start = (await input.snapshotProbe()).sequence;
  const keys = value === PROFILE ? ["Home"] : ["Home", "ArrowDown"];
  for (const key of keys) await input.pressKey(key);
  await input.pressKey("Enter");
  const snapshot = await input.snapshotProbe(start);
  const probe = assertTrustedProbeSequence(snapshot, {
    afterSequence: start,
    expected: [{ type: "change", identity: locator.identity }],
  });
  const final = await waitForObservation({
    label: `provider profile ${value}`,
    timeoutMs: 10_000,
    pollMs: 50,
    retrySampleErrors: false,
    sample: () => exactDomValue(cdp, locator.selector),
    accept: (observation) => observation.count === 1 && observation.value === value,
  });
  return { changed: true, initial, click, keys, probe, final: final.value };
}

async function waitForProductStage({
  label,
  timeoutMs = 20_000,
  sample,
  decide,
  code,
  message,
  acquiredTimeoutIsProduct = false,
}) {
  let decision = "pending";
  let observed;
  try {
    observed = await waitForObservation({
      label,
      timeoutMs,
      pollMs: 100,
      retrySampleErrors: false,
      sample,
      accept: (value) => {
        decision = decide(value);
        return decision !== "pending";
      },
    });
  } catch (error) {
    if (acquiredTimeoutIsProduct) {
      throw classifyAcquiredObservationFailure(error, { code, message });
    }
    throw new DesktopE2eError("harness", code, `${message}: ${error.message}`, { cause: errorObservation(error) });
  }
  if (decision === "fail") throw productFailure(code, message, { observation: observed.value });
  return observed.value;
}

async function openModelDetails(cdp, input) {
  const before = await observeLmStudioThinkingSurface(cdp);
  if (before.settings.model_details.open) return { changed: false };
  const click = await trustedClick(input, MODEL_DETAILS);
  const surface = await waitForProductStage({
    label: "LM Studio manual model control",
    sample: () => observeLmStudioThinkingSurface(cdp),
    decide: (value) => !surfaceErrorFree(value)
      ? "fail"
      : value?.settings?.model_details?.open === true && value?.settings?.model?.visible === true
        ? "pass"
        : "pending",
    code: "lm-studio-thinking-model-control-not-visible",
    message: "Preferences did not expose the manual model ID control",
  });
  return { changed: true, click, surface };
}

async function settleResources(state, { input = null, commands = null, generation, primaryError = null }) {
  const outcome = { generation, input: null, command_probe: null, failures: [] };
  if (input !== null) {
    try { outcome.input = await input.cleanup(); }
    catch (error) { outcome.failures.push({ owner: "webview-input", error: errorObservation(error) }); }
  }
  if (commands !== null) {
    try { outcome.command_probe = await commands.remove(); }
    catch (error) { outcome.failures.push({ owner: "desktop-command-probe", error: errorObservation(error) }); }
  }
  state.generationResources.push(outcome);
  if (outcome.failures.length > 0 && primaryError === null) {
    throw new DesktopE2eError(
      "harness",
      "lm-studio-thinking-resource-cleanup-failed",
      "LM Studio thinking input and command probes did not settle",
      outcome,
    );
  }
  return outcome;
}

function stableRestoredDecision(options, now = () => Date.now()) {
  let acceptedSince = null;
  return (surface) => {
    if (!surfaceErrorFree(surface)) return "fail";
    if (!restoredLmStudioThinkingConnectionReady(surface, options)) {
      acceptedSince = null;
      return "pending";
    }
    const observedAt = now();
    if (acceptedSince === null) {
      acceptedSince = observedAt;
      return "pending";
    }
    return observedAt - acceptedSince >= SETTINGS_STABILITY_MS ? "pass" : "pending";
  };
}

export function createLmStudioThinkingScenario(rawOptions = {}) {
  const options = normalizeLmStudioThinkingOptions(rawOptions);
  const state = {
    generationResources: [],
    artifact: null,
    requestCaptureDirectory: null,
    requestCaptureEvidence: null,
    quiesceOutcome: null,
  };
  return Object.freeze({
    id: "manual.provider-lm-studio-thinking",
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
      state.requestCaptureDirectory = path.join(context.root, REQUEST_CAPTURE_DIRECTORY_NAME);
      await prepareDesktopFixture({
        context,
        sink,
        phase,
        owner: OWNER,
        configText: lmStudioThinkingFixtureConfig(),
        sentinelName: SENTINEL_NAME,
        sentinelText: SENTINEL_TEXT,
      });
      await sink.record("lm-studio-thinking-input", {
        provider_base_url: options.providerBaseUrl,
        model: options.model,
        provider_profile: PROFILE,
        prompt: LM_STUDIO_THINKING_PROMPT,
        artifact_name: LM_STUDIO_THINKING_ARTIFACT_NAME,
        artifact_sha256: crypto.createHash("sha256").update(LM_STUDIO_THINKING_ARTIFACT_CONTENT).digest("hex"),
        external_provider_owned_by_scenario: false,
        external_model_lifecycle: "already-loaded-unmanaged",
        prepared_request_capture: {
          environment_key: "MOYAI_HTTP_REQUEST_CAPTURE_DIR",
          directory: state.requestCaptureDirectory,
          evidence_kind: "exact-prepared-outbound-body",
          expected_api_mode: "responses",
          expected_endpoint_path: "v1/responses",
          network_attempt_proven: false,
          provider_receipt_proven: false,
        },
      }, { phase, owner: OWNER });
    },
    async execute({ context, driver: firstCdp, host, sink }) {
      let firstInput = null;
      let firstCommands = null;
      let secondInput = null;
      let secondCommands = null;
      let firstSettled = false;
      let secondSettled = false;
      let primaryError = null;
      try {
        await acquireInteractiveShell({ context, driver: firstCdp, sink }, {
          evidenceOwner: OWNER,
          screenshotStem: "lm-studio-thinking-shell-ready",
        });
        firstInput = new WebviewInput(firstCdp, { probeId: "lm-studio-thinking-g1" });
        firstCommands = new DesktopCommandProbe(firstCdp, {
          probeId: "lm-studio-thinking-g1-commands",
          commands: ["save_global_config"],
        });
        await firstInput.installProbe();
        await firstCommands.install();
        await trustedClick(firstInput, SHOW_SETTINGS);
        const baseline = await waitForProductStage({
          label: "LM Studio thinking baseline Preferences",
          sample: () => observeLmStudioThinkingSurface(firstCdp),
          decide: (surface) => !surfaceErrorFree(surface)
            ? "fail"
            : surface?.projection?.overlay === "config"
              && surface?.settings?.base_url?.value === BASELINE_BASE_URL
              && surface?.settings?.profile?.value === PROFILE
              && surface?.settings?.dirty === false
              ? "pass"
              : "pending",
          code: "lm-studio-thinking-preferences-baseline-mismatch",
          message: "Preferences did not open with the isolated LM Studio baseline",
        });
        const baselineTarget = structuredClone(baseline.projection.config_target);
        const alternateProfile = await trustedSelectProfile({ cdp: firstCdp, input: firstInput, value: ALTERNATE_PROFILE });
        const selectedProfile = await trustedSelectProfile({ cdp: firstCdp, input: firstInput, value: PROFILE });
        const baseUrl = await trustedReplaceText({ cdp: firstCdp, input: firstInput, locator: BASE_URL, text: options.providerBaseUrl });
        await openModelDetails(firstCdp, firstInput);
        const model = await trustedReplaceText({ cdp: firstCdp, input: firstInput, locator: MODEL_MANUAL, text: options.model });
        const desired = desiredConnection(options);
        const dirty = await waitForProductStage({
          label: "dirty LM Studio thinking Preferences",
          sample: () => observeLmStudioThinkingSurface(firstCdp),
          decide: (surface) => !surfaceErrorFree(surface)
            ? "fail"
            : exactSettings(surface, desired, { dirty: true }) ? "pass" : "pending",
          code: "lm-studio-thinking-preferences-draft-mismatch",
          message: "trusted GUI input did not produce the exact LM Studio Preferences draft",
        });
        const expectedSave = expectedLmStudioThinkingGlobalSave(dirty, options);
        const commandStart = (await firstCommands.snapshot()).sequence;
        const save = await trustedClick(firstInput, SAVE_GLOBAL_CONFIG);
        const saved = await waitForProductStage({
          label: "saved LM Studio thinking Preferences",
          timeoutMs: 60_000,
          sample: () => observeLmStudioThinkingSurface(firstCdp),
          decide: (surface) => !surfaceErrorFree(surface)
            ? "fail"
            : savedConnectionReady(surface, options, baselineTarget) ? "pass" : "pending",
          code: "lm-studio-thinking-global-save-mismatch",
          message: "Preferences did not atomically persist the LM Studio connection",
        });
        const commandSnapshot = await waitForObservation({
          label: "exact LM Studio global save command",
          timeoutMs: 10_000,
          pollMs: 50,
          retrySampleErrors: false,
          sample: () => firstCommands.snapshot(commandStart),
          accept: (snapshot) => snapshot.calls.length >= 1,
        });
        const saveCommand = assertExactDesktopCommandSequence(commandSnapshot.value, {
          afterSequence: commandStart,
          expected: [expectedSave],
        });
        const savedScreenshot = await captureScenarioScreenshot({
          cdp: firstCdp,
          sink,
          name: "lm-studio-thinking-preferences-saved",
          owner: OWNER,
        });
        await sink.record("lm-studio-thinking-preferences-saved", {
          input_kind: "browser_trusted",
          alternate_profile: alternateProfile,
          selected_profile: selectedProfile,
          base_url: baseUrl,
          model,
          save,
          save_command: saveCommand,
          surface: saved,
          screenshot: savedScreenshot,
        }, { phase: "executing", owner: OWNER });
        await trustedClick(firstInput, CLOSE_SETTINGS);
        await waitForProductStage({
          label: "LM Studio Preferences close",
          sample: () => observeLmStudioThinkingSurface(firstCdp),
          decide: (surface) => !surfaceErrorFree(surface)
            ? "fail"
            : surface?.projection?.overlay === "none" ? "pass" : "pending",
          code: "lm-studio-thinking-preferences-close-failed",
          message: "saved Preferences did not close cleanly",
        });
        firstSettled = true;
        await settleResources(state, { input: firstInput, commands: firstCommands, generation: 1 });

        const restarted = await host.restart({ context, scenario: this, sink, driver: firstCdp, phase: "executing" });
        await acquireInteractiveShell({ context, driver: restarted.driver, sink }, {
          evidenceOwner: OWNER,
          screenshotStem: "lm-studio-thinking-restarted-shell-ready",
        });
        secondInput = new WebviewInput(restarted.driver, { probeId: "lm-studio-thinking-g2" });
        secondCommands = new DesktopCommandProbe(restarted.driver, {
          probeId: "lm-studio-thinking-g2-commands",
          commands: ["submit_prompt", "cancel_run"],
        });
        await secondInput.installProbe();
        await secondCommands.install();
        await trustedClick(secondInput, SHOW_SETTINGS);
        await openModelDetails(restarted.driver, secondInput);
        const restored = await waitForProductStage({
          label: "stable restored LM Studio thinking Preferences",
          timeoutMs: 30_000,
          sample: () => observeLmStudioThinkingSurface(restarted.driver),
          decide: stableRestoredDecision(options),
          code: "lm-studio-thinking-restart-persistence-mismatch",
          message: "restart did not stably restore the LM Studio connection without client-owned thinking controls",
        });
        const restoredScreenshot = await captureScenarioScreenshot({
          cdp: restarted.driver,
          sink,
          name: "lm-studio-thinking-preferences-restored",
          owner: OWNER,
        });
        await trustedClick(secondInput, CLOSE_SETTINGS);
        await waitForProductStage({
          label: "restored LM Studio Preferences close",
          sample: () => observeLmStudioThinkingSurface(restarted.driver),
          decide: (surface) => !surfaceErrorFree(surface)
            ? "fail"
            : surface?.projection?.overlay === "none"
              && surface?.prompt?.visible === true
              && surface.prompt.enabled === true
              ? "pass"
              : "pending",
          code: "lm-studio-thinking-restored-preferences-close-failed",
          message: "restored Preferences did not return to the interactive composer",
        });
        const promptInput = await trustedReplaceText({
          cdp: restarted.driver,
          input: secondInput,
          locator: PROMPT,
          text: LM_STUDIO_THINKING_PROMPT,
        });
        const ready = await observeLmStudioThinkingSurface(restarted.driver);
        if (ready.prompt.value !== LM_STUDIO_THINKING_PROMPT || ready.send.enabled !== true) {
          throw productFailure("lm-studio-thinking-prompt-not-ready", "trusted input did not produce the exact submit-ready prompt", { surface: ready });
        }
        const expectedSubmit = {
          command: "submit_prompt",
          args: {
            text: LM_STUDIO_THINKING_PROMPT,
            expectedTarget: structuredClone(ready.projection.draft_target),
            expectedRunTarget: structuredClone(ready.projection.run_target),
          },
        };
        const submitStart = (await secondCommands.snapshot()).sequence;
        const send = await trustedClick(secondInput, SEND);
        const submitted = await waitForObservation({
          label: "exact LM Studio thinking submit command",
          timeoutMs: 10_000,
          pollMs: 25,
          retrySampleErrors: false,
          sample: () => secondCommands.snapshot(submitStart),
          accept: (snapshot) => snapshot.calls.length >= 1,
        });
        const submitCommand = assertExactDesktopCommandSequence(submitted.value, {
          afterSequence: submitStart,
          expected: [expectedSubmit],
        });
        const activePlan = await waitForProductStage({
          label: "visible running LM Studio thinking plan",
          timeoutMs: LIVE_TURN_TIMEOUT_MS,
          sample: () => observeLmStudioThinkingSurface(restarted.driver),
          decide: (surface) => {
            if (!surfaceErrorFree(surface) || lmStudioThinkingControlTokenLeaks(surface?.projection).length > 0) return "fail";
            if (lmStudioThinkingActivePlanAccepted(surface)) return "pass";
            return ["completed", "failed", "cancelled", "stopped"].includes(surface?.projection?.run_status_key)
              ? "fail"
              : "pending";
          },
          code: "lm-studio-thinking-active-plan-mismatch",
          message: "the exact in-progress two-step plan was not visibly projected before terminal",
        });
        const activePlanScreenshot = await captureScenarioScreenshot({
          cdp: restarted.driver,
          sink,
          name: "lm-studio-thinking-plan-running",
          owner: OWNER,
        });
        let terminal = null;
        terminal = await waitForProductStage({
          label: "terminal LM Studio thinking artifact",
          timeoutMs: LIVE_TURN_TIMEOUT_MS,
          sample: async () => {
            const surface = await observeLmStudioThinkingSurface(restarted.driver);
            const artifact = surface?.projection?.run_status_key === "completed"
              ? await artifactIdentity(context.paths.workspace)
              : null;
            return { surface, artifact };
          },
          decide: (sample) => lmStudioThinkingTerminalDecision(sample.surface, sample.artifact),
          code: "lm-studio-thinking-terminal-mismatch",
          message: "the LM Studio turn did not settle with the exact completed plan, patch, artifact, and final response",
          acquiredTimeoutIsProduct: true,
        });
        state.artifact = terminal.artifact;
        state.requestCaptureEvidence = await inspectLmStudioThinkingRequestCaptures(
          state.requestCaptureDirectory,
          options,
        );
        const finalCommands = assertExactDesktopCommandSequence(await secondCommands.snapshot(submitStart), {
          afterSequence: submitStart,
          expected: [expectedSubmit],
        });
        const terminalScreenshot = await captureScenarioScreenshot({
          cdp: restarted.driver,
          sink,
          name: "lm-studio-thinking-completed",
          owner: OWNER,
        });
        await sink.record("lm-studio-thinking-completed", {
          input_kind: "browser_trusted",
          restart: restarted.restart,
          restored,
          restored_screenshot: restoredScreenshot,
          prompt_input: promptInput,
          send,
          submit_command: submitCommand,
          final_commands: finalCommands,
          active_plan: activePlan,
          active_plan_screenshot: activePlanScreenshot,
          terminal: terminal.surface,
          artifact: state.artifact,
          prepared_request_capture: state.requestCaptureEvidence,
          terminal_screenshot: terminalScreenshot,
          provider_resource: {
            kind: "external-lm-studio-provider",
            owned_by_scenario: false,
            lifecycle: "already-loaded-unmanaged",
            load_action: "none",
            unload_action: "none",
          },
        }, { phase: "executing", owner: OWNER });
        secondSettled = true;
        await settleResources(state, { input: secondInput, commands: secondCommands, generation: 2 });
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        if (!firstSettled && (firstInput !== null || firstCommands !== null)) {
          firstSettled = true;
          await settleResources(state, { input: firstInput, commands: firstCommands, generation: 1, primaryError });
        }
        if (!secondSettled && (secondInput !== null || secondCommands !== null)) {
          secondSettled = true;
          await settleResources(state, { input: secondInput, commands: secondCommands, generation: 2, primaryError });
        }
      }
    },
    async quiesce() {
      if (state.quiesceOutcome !== null) return structuredClone(state.quiesceOutcome);
      const resourcesPass = state.generationResources.every((resource) => resource.failures.length === 0);
      state.quiesceOutcome = {
        input: resourcesPass ? "pass" : "fail",
        resources: [{
          kind: "external-lm-studio-provider",
          provider_base_url: options.providerBaseUrl,
          model: options.model,
          owned_by_scenario: false,
          lifecycle: "already-loaded-unmanaged",
          cleanup_action: "none",
          generation_resources: structuredClone(state.generationResources),
        }],
      };
      return structuredClone(state.quiesceOutcome);
    },
    async cleanup({ context }) {
      const artifact = await artifactIdentity(context.paths.workspace);
      const artifactPass = exactArtifactIdentity(artifact);
      const quiescePass = state.quiesceOutcome?.input === "pass";
      const requestCapturePass = state.requestCaptureEvidence?.failures?.length === 0
        && state.requestCaptureEvidence?.capture_count > 0;
      return {
        input: artifactPass && quiescePass && requestCapturePass ? "pass" : "fail",
        resources: [{
          kind: "lm-studio-thinking-artifact-verification",
          artifact,
          artifact_matches_terminal: state.artifact !== null
            && artifact.sha256 === state.artifact.sha256
            && artifact.size_bytes === state.artifact.size_bytes,
          prepared_request_capture: state.requestCaptureEvidence,
          quiesce_input: state.quiesceOutcome?.input ?? null,
        }],
      };
    },
  });
}

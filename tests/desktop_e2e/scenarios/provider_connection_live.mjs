import crypto from "node:crypto";
import path from "node:path";
import { lstat, readFile } from "node:fs/promises";

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
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";

const OWNER = "scenario:manual.provider-openai-compatible";
const PROFILE = "openai_compatible";
const EMPTY_API_KEY_ENV = "";
const BASELINE_BASE_URL = "http://127.0.0.1:9/v1";
const BASELINE_MODEL = "moyai-e2e-provider-before-save";
const BASELINE_PROFILE = "lm_studio";
const BASELINE_API_KEY_ENV = "MOYAI_E2E_UNUSED_PROVIDER_KEY";
const SENTINEL_NAME = "E2E_PROVIDER_OPENAI_COMPATIBLE.txt";
const SETTINGS_STABILITY_MS = 500;
const LIVE_TURN_TIMEOUT_MS = 420_000;

export const PROVIDER_OPENAI_COMPATIBLE_PROMPT = "接続確認です。必ず built-in の current_time ツールを引数 {} でちょうど1回だけ呼び出してください。その結果に含まれる local、utc、timezone の値をそのまま使い、回答を必ず「接続確認完了：local=... / utc=... / timezone=... です。」という日本語の1文にしてください（... はそれぞれの実値に置き換えてください）。ファイル操作、shell、他のツールは使わないでください。";

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
const API_KEY_ENV = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] input.settings-control[data-config-key="model.api_key_env"]',
  identity: { tag: "INPUT", configKey: "model.api_key_env" },
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

function sameValue(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

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

function canonicalProviderBaseUrl(value) {
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new TypeError("manual.provider-openai-compatible provider_base_url must be a non-empty string");
  }
  let url;
  try { url = new URL(value.trim()); }
  catch (error) { throw new TypeError(`manual.provider-openai-compatible provider_base_url is invalid: ${error.message}`); }
  if (!new Set(["http:", "https:"]).has(url.protocol)
    || url.username.length > 0
    || url.password.length > 0
    || url.search.length > 0
    || url.hash.length > 0) {
    throw new TypeError("manual.provider-openai-compatible provider_base_url must be one credential-free HTTP(S) endpoint without query or fragment");
  }
  return url.toString().replace(/\/$/, "");
}

function canonicalModel(value) {
  if (typeof value !== "string") {
    throw new TypeError("manual.provider-openai-compatible model must be a string");
  }
  const normalized = value.trim();
  if (normalized.length === 0
    || Buffer.byteLength(normalized, "utf8") > 1024
    || /[\u0000-\u001f\u007f]/u.test(normalized)) {
    throw new TypeError("manual.provider-openai-compatible model must be a non-empty bounded model ID without control characters");
  }
  return normalized;
}

export function normalizeProviderConnectionLiveOptions(options) {
  if (options === null || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("manual.provider-openai-compatible requires one scenario config object");
  }
  const allowed = new Set(["provider_base_url", "model"]);
  const unknown = Object.keys(options).filter((key) => !allowed.has(key));
  if (unknown.length > 0) {
    throw new TypeError(`unknown manual.provider-openai-compatible option: ${unknown.join(",")}`);
  }
  return Object.freeze({
    providerBaseUrl: canonicalProviderBaseUrl(options.provider_base_url),
    model: canonicalModel(options.model),
  });
}

export function providerConnectionLiveFixtureConfig() {
  return `[model]
base_url = ${JSON.stringify(BASELINE_BASE_URL)}
model = ${JSON.stringify(BASELINE_MODEL)}
provider_profile = ${JSON.stringify(BASELINE_PROFILE)}
api_key_env = ${JSON.stringify(BASELINE_API_KEY_ENV)}
reasoning_summary = "none"
connect_timeout_ms = 10000
request_timeout_ms = 180000
max_retries = 0
context_window = 32768
max_output_tokens = 1024
supports_tools = true
supports_reasoning = false
supports_images = false
parallel_tool_calls = false

[model.extra_body_json]

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

function desiredConnection(options) {
  return {
    baseUrl: options.providerBaseUrl,
    model: options.model,
    providerProfile: PROFILE,
    apiKeyEnv: EMPTY_API_KEY_ENV,
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

export function expectedProviderConnectionGlobalSave(surface, options) {
  const projection = surface?.projection;
  if (!projection?.config_target) throw new TypeError("provider connection save expectation requires a config target");
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

export async function observeProviderConnectionLiveSurface(cdp) {
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
    const transcriptRows = (selector) => matches(selector).map((row) => ({
      text: (row.querySelector('.markdown-body')?.innerText ?? '').trim(),
      visible: visible(row),
    }));
    const completedSummaries = matches('main.conversation #thread article.message.work-summary.work_summary_completed').map((row) => {
      const summary = row.querySelector('details > summary');
      const body = row.querySelector('.work-summary-body');
      return {
        title: summary instanceof HTMLElement ? summary.innerText.trim() : null,
        body: body instanceof HTMLElement ? (body.textContent ?? '').trim() : null,
        visible: visible(row),
        summary_visible: visible(summary),
      };
    });
    const details = one('[role="dialog"][aria-labelledby="config-dialog-title"] details[data-details-key="main-provider-manual-model"]');
    const assistants = transcriptRows('main.conversation #thread article.message.assistant');
    return {
      projection,
      settings: {
        dialog: (() => {
          const found = one('[role="dialog"][aria-labelledby="config-dialog-title"]');
          return { count: found.count, visible: found.visible };
        })(),
        base_url: control(${JSON.stringify(BASE_URL.selector)}),
        profile: control(${JSON.stringify(PROFILE_SELECT.selector)}),
        model_details: { count: details.count, visible: details.visible, open: details.node instanceof HTMLDetailsElement && details.node.open },
        model: control(${JSON.stringify(MODEL_MANUAL.selector)}),
        api_key_env: control(${JSON.stringify(API_KEY_ENV.selector)}),
        dirty: matches('[role="dialog"][aria-labelledby="config-dialog-title"] .dirty-badge.visible').filter(visible).length === 1,
        save: button(${JSON.stringify(SAVE_GLOBAL_CONFIG.selector)}),
        close: button(${JSON.stringify(CLOSE_SETTINGS.selector)}),
      },
      prompt: control(${JSON.stringify(PROMPT.selector)}),
      send: button(${JSON.stringify(SEND.selector)}),
      assistants,
      completed_summaries: completedSummaries,
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

function exactSettingsControls(surface, expected, { dirty, detailsOpen = true }) {
  return surface?.projection?.overlay === "config"
    && surface?.settings?.dialog?.count === 1
    && surface.settings.dialog.visible === true
    && surface.settings.base_url.count === 1
    && surface.settings.base_url.visible === true
    && surface.settings.base_url.enabled === true
    && surface.settings.base_url.value === expected.baseUrl
    && surface.settings.profile.count === 1
    && surface.settings.profile.visible === true
    && surface.settings.profile.enabled === true
    && surface.settings.profile.value === expected.providerProfile
    && surface.settings.profile.options.includes(PROFILE)
    && surface.settings.model_details.count === 1
    && surface.settings.model_details.open === detailsOpen
    && surface.settings.model.count === 1
    && (!detailsOpen || surface.settings.model.visible === true)
    && surface.settings.model.enabled === true
    && surface.settings.model.value === expected.model
    && surface.settings.api_key_env.count === 1
    && surface.settings.api_key_env.visible === true
    && surface.settings.api_key_env.enabled === true
    && surface.settings.api_key_env.value === expected.apiKeyEnv
    && surface.settings.dirty === dirty
    && surface.settings.save.count === 1
    && surface.settings.save.visible === true
    && surface.settings.save.enabled === dirty
    && surface.settings.close.count === 1
    && surface.settings.close.visible === true
    && surfaceErrorFree(surface);
}

function persistedConnectionReady(surface, desired) {
  return fieldValue(surface?.projection, "model.base_url") === desired.baseUrl
    && fieldValue(surface?.projection, "model.model") === desired.model
    && fieldValue(surface?.projection, "model.provider_profile") === desired.providerProfile
    && fieldValue(surface?.projection, "model.api_key_env") === desired.apiKeyEnv
    && fieldValue(surface?.projection, "model.supports_tools") === "true"
    && surface?.projection?.provider_effective_base_url === desired.baseUrl
    && surface?.projection?.provider_effective_model_id === desired.model
    && surface?.projection?.provider_effective_profile === desired.providerProfile
    && surface?.projection?.provider_effective_api_key_env === desired.apiKeyEnv;
}

export function restoredProviderConnectionReady(surface, options) {
  const desired = desiredConnection(options);
  return exactSettingsControls(surface, desired, { dirty: false })
    && persistedConnectionReady(surface, desired);
}

export function savedProviderConnectionReady(surface, options, baselineTarget) {
  const desired = desiredConnection(options);
  return exactSettingsControls(surface, desired, { dirty: false, detailsOpen: false })
    && persistedConnectionReady(surface, desired)
    && advancedConfigTarget(surface?.projection?.config_target, baselineTarget);
}

export function parseCurrentTimeWorkSummary(value) {
  if (typeof value !== "string") return null;
  const terminalRows = Array.from(value.matchAll(/^- \[(完了|失敗|拒否|キャンセル)\] ([^\r\n]+)$/gmu));
  if (terminalRows.length !== 1
    || terminalRows[0][1] !== "完了"
    || terminalRows[0][2] !== "Current time") return null;
  const matches = Array.from(value.matchAll(
    /^- \[完了\] Current time\r?\n {2}出力: local: ([^ ]+) utc: ([^ ]+) timezone: ([^ ]+) unix_ms: ([0-9]+)$/gmu,
  ));
  if (matches.length !== 1) return null;
  return {
    local: matches[0][1],
    utc: matches[0][2],
    timezone: matches[0][3],
    unixMs: matches[0][4],
  };
}

function finalAssistant(projection) {
  const rows = Array.isArray(projection?.transcript_rows) ? projection.transcript_rows : [];
  return rows.filter((row) => row?.row_kind === "assistant" && typeof row.body === "string").at(-1)?.body.trim() ?? "";
}

function completedWorkSummaries(projection) {
  const rows = Array.isArray(projection?.transcript_rows) ? projection.transcript_rows : [];
  return rows.filter((row) => row?.row_kind === "work_summary_completed");
}

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&");
}

export function currentTimeFromCompletedProjection(projection) {
  const summaries = completedWorkSummaries(projection);
  return summaries.length === 1
    ? parseCurrentTimeWorkSummary(summaries[0].body)
    : null;
}

function assistantUsesTime(assistant, time) {
  const expected = `接続確認完了：local=${time.local} / utc=${time.utc} / timezone=${time.timezone} です。`;
  return assistant === expected;
}

function completedSummaryDomUsesTime(summary, time) {
  if (typeof summary?.body !== "string") return false;
  const renderedTime = new RegExp(
    `local:\\s*${escapeRegExp(time.local)}\\s+utc:\\s*${escapeRegExp(time.utc)}\\s+timezone:\\s*${escapeRegExp(time.timezone)}\\s+unix_ms:\\s*${escapeRegExp(time.unixMs)}(?:$|\\s)`,
    "u",
  );
  return renderedTime.test(summary.body);
}

function terminalToolProjectionUsesTime(projection, time) {
  const expectedToolStatus = `ツール:\n- Current time [completed] local: ${time.local}\nutc: ${time.utc}\ntimezone: ${time.timezone}\nunix_ms: ${time.unixMs}`;
  return projection?.tool_status_text === expectedToolStatus
    && projection?.latest_tool_summary === "ツール:"
    && typeof projection?.progress_text === "string"
    && projection.progress_text.includes(
      "ツール: 1件開始 / 1件完了 / 0件拒否 / 0件キャンセル / 0件失敗",
    );
}

function liveCurrentTimeProjectionAccepted(surface) {
  const projection = surface?.projection;
  const summaries = completedWorkSummaries(projection);
  const time = currentTimeFromCompletedProjection(projection);
  if (time === null || !surfaceErrorFree(surface)) return false;
  const assistant = finalAssistant(projection);
  return projection?.run_status_key === "completed"
    && projection?.task_activity_state === "idle"
    && projection?.busy === false
    && projection?.agent_tree_active === false
    && projection?.post_run_refresh_pending === false
    && projection?.background_mutation_pending === false
    && projection?.async_polling_required === false
    && Array.isArray(projection?.pending_async_operations)
    && projection.pending_async_operations.length === 0
    && projection?.overlay === "none"
    && projection?.confirmation_visible === false
    && projection?.confirmation_id === null
    && projection?.confirmation == null
    && projection?.draft_prompt === ""
    && projection?.can_submit === true
    && /\[完了\]/u.test(projection?.selected_session_title ?? "")
    && projection?.status_message === "実行完了"
    && summaries.length === 1
    && terminalToolProjectionUsesTime(projection, time)
    && assistantUsesTime(assistant, time);
}

function liveCurrentTimeDomAccepted(surface, time) {
  const projection = surface?.projection;
  const visibleAssistants = Array.isArray(surface?.assistants)
    ? surface.assistants.filter((row) => row?.visible === true)
    : [];
  const visibleAssistant = visibleAssistants.length === 1
    && assistantUsesTime(visibleAssistants[0].text, time);
  const completedSummary = surface?.completed_summaries?.length === 1
    ? surface.completed_summaries[0]
    : null;
  const canonicalSummary = completedWorkSummaries(projection)[0];
  return visibleAssistant
    && completedSummary?.visible === true
    && completedSummary.summary_visible === true
    && completedSummary.title === canonicalSummary?.title
    && completedSummaryDomUsesTime(completedSummary, time)
    && surface?.terminal_dom?.topbar_title?.count === 1
    && surface.terminal_dom.topbar_title.visible === true
    && surface.terminal_dom.topbar_title.text === projection?.selected_session_title
    && /\[完了\]/u.test(surface.terminal_dom.topbar_title.text)
    && surface.terminal_dom.topbar_status?.count === 1
    && surface.terminal_dom.topbar_status.visible === true
    && surface.terminal_dom.topbar_status.text === projection?.status_message
    && surface.terminal_dom.topbar_status.text === "実行完了"
    && surface.terminal_dom.visible_run_strip_count === 0
    && surface.terminal_dom.visible_task_activity_indicator_count === 0
    && surface.terminal_dom.visible_selected_activity_row_count === 0;
}

export function liveCurrentTimeTerminalAccepted(surface) {
  if (!liveCurrentTimeProjectionAccepted(surface)) return false;
  const time = currentTimeFromCompletedProjection(surface.projection);
  return liveCurrentTimeDomAccepted(surface, time);
}

export function liveCurrentTimeTerminalDecision(surface) {
  if (surface && !surfaceErrorFree(surface)) return "fail";
  const projection = surface?.projection;
  const status = projection?.run_status_key;
  if (status === "failed" || status === "cancelled") return "fail";
  if (status === "completed") {
    const refreshPending = projection?.post_run_refresh_pending === true
      || projection?.background_mutation_pending === true
      || projection?.async_polling_required === true
      || (Array.isArray(projection?.pending_async_operations)
        && projection.pending_async_operations.length > 0);
    if (refreshPending) return "pending";
    if (!liveCurrentTimeProjectionAccepted(surface)) return "fail";
    return liveCurrentTimeTerminalAccepted(surface) ? "pass" : "pending";
  }
  return "pending";
}

function classifyObservationFailure(error, code, message) {
  if (error?.code === "observation-timeout" && error?.evidence?.last_error === null) {
    return productFailure(code, message, error.evidence);
  }
  return error;
}

async function waitForProductStage({ label, timeoutMs = 30_000, sample, decide, code, message }) {
  let terminal = "pending";
  let observed;
  try {
    observed = await waitForObservation({
      label,
      timeoutMs,
      pollMs: 100,
      retrySampleErrors: false,
      sample,
      accept: (value) => {
        terminal = decide(value);
        return terminal !== "pending";
      },
    });
  } catch (error) {
    throw classifyObservationFailure(error, code, message);
  }
  if (terminal === "fail") throw productFailure(code, message, observed.value);
  return observed;
}

function trustedClickEvents(locator) {
  return [
    { type: "pointerdown", identity: locator.identity, button: 0, buttons: 1 },
    { type: "pointerup", identity: locator.identity, button: 0, buttons: 0 },
    { type: "click", identity: locator.identity, button: 0, buttons: 0 },
  ];
}

async function trustedClick(input, locator) {
  const start = (await input.snapshotProbe()).sequence;
  const target = await input.click(locator);
  const snapshot = await input.snapshotProbe(start);
  const probe = assertTrustedProbeSequence(snapshot, {
    afterSequence: start,
    expected: trustedClickEvents(locator),
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

async function waitForDomValue(cdp, locator, expected) {
  return waitForObservation({
    label: `exact DOM value ${locator.identity.configKey ?? locator.identity.id ?? "control"}`,
    timeoutMs: 10_000,
    pollMs: 50,
    retrySampleErrors: false,
    sample: () => exactDomValue(cdp, locator.selector),
    accept: (value) => value.count === 1 && value.value === expected,
  });
}

async function trustedReplaceText({ cdp, input, locator, text }) {
  const initial = await exactDomValue(cdp, locator.selector);
  if (initial.count !== 1) throw new Error("trusted text replacement target cardinality drifted");
  if (initial.value === text) return { changed: false, initial };
  const click = await trustedClick(input, locator);
  const clearStart = click.sequence;
  await input.keyDown("Control");
  try { await input.pressKey("a"); }
  finally { await input.keyUp("Control"); }
  await input.pressKey("Backspace");
  const cleared = await input.snapshotProbe(clearStart);
  const clearProbe = assertTrustedProbeSequence(cleared, {
    afterSequence: clearStart,
    expected: [
      { type: "keydown", identity: locator.identity, key: "Control", code: "ControlLeft" },
      { type: "keydown", identity: locator.identity, key: "a", code: "KeyA" },
      { type: "keyup", identity: locator.identity, key: "a", code: "KeyA" },
      { type: "keyup", identity: locator.identity, key: "Control", code: "ControlLeft" },
      { type: "keydown", identity: locator.identity, key: "Backspace", code: "Backspace" },
      { type: "keyup", identity: locator.identity, key: "Backspace", code: "Backspace" },
    ],
  });
  let insertion = null;
  if (text.length > 0) {
    const insertStart = cleared.sequence;
    const inserted = await input.insertText(locator, text);
    const snapshot = await input.snapshotProbe(insertStart);
    insertion = {
      inserted,
      probe: assertTrustedTextInsertion(snapshot, {
        afterSequence: insertStart,
        identity: locator.identity,
        text,
      }),
    };
  }
  const final = await waitForDomValue(cdp, locator, text);
  return { changed: true, initial, click, clear_probe: clearProbe, insertion, final: final.value };
}

export function assertTrustedProviderProfileSelection(snapshot, afterSequence) {
  return assertTrustedProbeSequence(snapshot, {
    afterSequence,
    expected: [
      { type: "change", identity: PROFILE_SELECT.identity },
    ],
  });
}

async function trustedSelectOpenAiCompatible({ cdp, input }) {
  const initial = await exactDomValue(cdp, PROFILE_SELECT.selector);
  if (initial.count !== 1) throw new Error("provider profile target cardinality drifted");
  if (initial.value === PROFILE) return { changed: false, initial };
  const click = await trustedClick(input, PROFILE_SELECT);
  const start = click.sequence;
  await input.pressKey("o");
  await input.pressKey("Enter");
  const snapshot = await input.snapshotProbe(start);
  const probe = assertTrustedProviderProfileSelection(snapshot, start);
  const final = await waitForDomValue(cdp, PROFILE_SELECT, PROFILE);
  return { changed: true, initial, click, probe, final: final.value };
}

async function openModelDetails(cdp, input) {
  const before = await observeProviderConnectionLiveSurface(cdp);
  if (before.settings.model_details.open) return { changed: false };
  const click = await trustedClick(input, MODEL_DETAILS);
  const settled = await waitForProductStage({
    label: "manual provider model ID control",
    sample: () => observeProviderConnectionLiveSurface(cdp),
    decide: (surface) => !surfaceErrorFree(surface)
      ? "fail"
      : surface?.settings?.model_details?.open === true && surface?.settings?.model?.visible === true
        ? "pass"
        : "pending",
    code: "provider-live-model-control-not-visible",
    message: "Preferences did not expose the manual model ID control after trusted activation",
  });
  return { changed: true, click, surface: settled.value };
}

function baselineConnection() {
  return {
    baseUrl: BASELINE_BASE_URL,
    model: BASELINE_MODEL,
    providerProfile: BASELINE_PROFILE,
    apiKeyEnv: BASELINE_API_KEY_ENV,
  };
}

function sameConfigTargetOwner(current, baseline) {
  return current?.workspacePath === baseline?.workspacePath
    && current?.sessionId === baseline?.sessionId
    && typeof current?.configGeneration === "string"
    && current.configGeneration.length > 0;
}

function advancedConfigTarget(current, baseline) {
  return sameConfigTargetOwner(current, baseline)
    && current.configGeneration !== baseline.configGeneration;
}

function stableRestoredDecision(options, now = () => Date.now()) {
  let acceptedSince = null;
  return (surface) => {
    if (surface && !surfaceErrorFree(surface)) return "fail";
    if (!restoredProviderConnectionReady(surface, options)) {
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

async function waitForCommands(probe, afterSequence, expected) {
  const observed = await waitForObservation({
    label: "exact provider global save command",
    timeoutMs: 10_000,
    pollMs: 50,
    retrySampleErrors: false,
    sample: () => probe.snapshot(afterSequence),
    accept: (snapshot) => snapshot.calls.length >= expected.length,
  });
  return assertExactDesktopCommandSequence(observed.value, { afterSequence, expected });
}

async function settleGenerationResources(state, { input = null, commandProbe = null, generation, primaryError = null }) {
  const outcome = { generation, input: null, command_probe: null, failures: [] };
  if (input !== null) {
    try { outcome.input = await input.cleanup(); }
    catch (error) { outcome.failures.push({ owner: "webview-input", error: errorObservation(error) }); }
  }
  if (commandProbe !== null) {
    try { outcome.command_probe = await commandProbe.remove(); }
    catch (error) { outcome.failures.push({ owner: "desktop-command-probe", error: errorObservation(error) }); }
  }
  state.generationResources.push(outcome);
  if (outcome.failures.length > 0 && primaryError === null) {
    throw new DesktopE2eError(
      "harness",
      "provider-live-generation-resource-cleanup-failed",
      "live provider WebView input/command probes did not settle at the generation boundary",
      outcome,
    );
  }
  return outcome;
}

async function physicalFileIdentity(candidate) {
  const item = await lstat(candidate);
  if (!item.isFile() || item.isSymbolicLink()) {
    throw new Error(`workspace sentinel is not one physical file: ${candidate}`);
  }
  const bytes = await readFile(candidate);
  return {
    path: path.resolve(candidate),
    size_bytes: bytes.byteLength,
    sha256: crypto.createHash("sha256").update(bytes).digest("hex"),
  };
}

export function createProviderConnectionLiveScenario(rawOptions = {}) {
  const options = normalizeProviderConnectionLiveOptions(rawOptions);
  const state = {
    sentinelBaseline: null,
    generationResources: [],
    quiesceOutcome: null,
  };
  return Object.freeze({
    id: "manual.provider-openai-compatible",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    requestGracefulExit,
    async prepare({ context, sink, phase }) {
      await prepareDesktopFixture({
        context,
        sink,
        phase,
        owner: OWNER,
        configText: providerConnectionLiveFixtureConfig(),
        sentinelName: SENTINEL_NAME,
        sentinelText: "moyAI Desktop E2E OpenAI-compatible provider smoke fixture.\n",
      });
      state.sentinelBaseline = await physicalFileIdentity(path.join(context.paths.workspace, SENTINEL_NAME));
      await sink.record("provider-live-input", {
        provider_base_url: options.providerBaseUrl,
        model: options.model,
        provider_profile: PROFILE,
        api_key_env: EMPTY_API_KEY_ENV,
        prompt: PROVIDER_OPENAI_COMPATIBLE_PROMPT,
        workspace_sentinel: state.sentinelBaseline,
        external_provider_owned_by_scenario: false,
      }, { phase, owner: OWNER });
    },
    async execute({ context, driver: firstCdp, host, sink }) {
      let firstInput = null;
      let firstCommands = null;
      let secondInput = null;
      let firstSettled = false;
      let secondSettled = false;
      let primaryError = null;
      try {
        await acquireInteractiveShell({ context, driver: firstCdp, sink }, {
          evidenceOwner: OWNER,
          screenshotStem: "provider-live-shell-ready",
        });
        firstInput = new WebviewInput(firstCdp, { probeId: "provider-live-g1" });
        firstCommands = new DesktopCommandProbe(firstCdp, {
          probeId: "provider-live-g1",
          commands: ["save_global_config"],
        });
        await firstInput.installProbe();
        await firstCommands.install();

        await trustedClick(firstInput, SHOW_SETTINGS);
        await waitForProductStage({
          label: "baseline provider Preferences",
          sample: () => observeProviderConnectionLiveSurface(firstCdp),
          decide: (surface) => !surfaceErrorFree(surface)
            ? "fail"
            : surface?.projection?.overlay === "config"
              && surface?.settings?.base_url?.value === BASELINE_BASE_URL
              && surface?.settings?.profile?.value === BASELINE_PROFILE
              && surface?.settings?.api_key_env?.value === BASELINE_API_KEY_ENV
              && surface?.settings?.dirty === false
              ? "pass"
              : "pending",
          code: "provider-live-preferences-baseline-mismatch",
          message: "Preferences did not open with the isolated provider baseline",
        });
        await openModelDetails(firstCdp, firstInput);
        const editable = await waitForProductStage({
          label: "complete baseline provider Preferences controls",
          sample: () => observeProviderConnectionLiveSurface(firstCdp),
          decide: (surface) => !surfaceErrorFree(surface)
            ? "fail"
            : exactSettingsControls(surface, baselineConnection(), { dirty: false })
              ? "pass"
              : "pending",
          code: "provider-live-preferences-controls-mismatch",
          message: "Preferences did not expose the complete connection controls",
        });
        const saveTarget = structuredClone(editable.value.projection.config_target);

        const inputEvidence = {
          base_url: await trustedReplaceText({ cdp: firstCdp, input: firstInput, locator: BASE_URL, text: options.providerBaseUrl }),
          profile: await trustedSelectOpenAiCompatible({ cdp: firstCdp, input: firstInput }),
          model: await trustedReplaceText({ cdp: firstCdp, input: firstInput, locator: MODEL_MANUAL, text: options.model }),
          api_key_env: await trustedReplaceText({ cdp: firstCdp, input: firstInput, locator: API_KEY_ENV, text: EMPTY_API_KEY_ENV }),
        };
        const desired = desiredConnection(options);
        const dirty = await waitForProductStage({
          label: "dirty OpenAI-compatible Preferences",
          sample: () => observeProviderConnectionLiveSurface(firstCdp),
          decide: (surface) => !surfaceErrorFree(surface)
            ? "fail"
            : exactSettingsControls(surface, desired, { dirty: true })
              ? "pass"
              : "pending",
          code: "provider-live-preferences-draft-mismatch",
          message: "trusted GUI input did not produce the exact OpenAI-compatible Preferences draft",
        });
        const dirtyScreenshot = await captureScenarioScreenshot({
          cdp: firstCdp,
          sink,
          name: "provider-openai-compatible-preferences-dirty",
          owner: OWNER,
        });
        const expectedSave = expectedProviderConnectionGlobalSave(dirty.value, options);
        const commandStart = (await firstCommands.snapshot()).sequence;
        await trustedClick(firstInput, SAVE_GLOBAL_CONFIG);
        const saved = await waitForProductStage({
          label: "saved OpenAI-compatible Preferences",
          timeoutMs: 60_000,
          sample: () => observeProviderConnectionLiveSurface(firstCdp),
          decide: (surface) => !surfaceErrorFree(surface)
            ? "fail"
            : savedProviderConnectionReady(surface, options, saveTarget)
              ? "pass"
              : "pending",
          code: "provider-live-global-save-mismatch",
          message: "Preferences did not atomically persist the OpenAI-compatible connection",
        });
        const saveCommand = await waitForCommands(firstCommands, commandStart, [expectedSave]);
        const savedScreenshot = await captureScenarioScreenshot({
          cdp: firstCdp,
          sink,
          name: "provider-openai-compatible-preferences-saved",
          owner: OWNER,
        });
        await sink.record("provider-live-preferences-saved", {
          input_kind: "browser_trusted",
          inputs: inputEvidence,
          dirty: dirty.value,
          saved: saved.value,
          save_command: saveCommand,
          screenshots: { dirty: dirtyScreenshot, saved: savedScreenshot },
        }, { phase: "executing", owner: OWNER });

        await trustedClick(firstInput, CLOSE_SETTINGS);
        await waitForProductStage({
          label: "clean Preferences close before restart",
          sample: () => observeProviderConnectionLiveSurface(firstCdp),
          decide: (surface) => !surfaceErrorFree(surface)
            ? "fail"
            : surface?.projection?.overlay === "none" ? "pass" : "pending",
          code: "provider-live-preferences-close-failed",
          message: "saved Preferences did not close cleanly",
        });
        firstSettled = true;
        await settleGenerationResources(state, {
          input: firstInput,
          commandProbe: firstCommands,
          generation: 1,
        });

        const restarted = await host.restart({ context, scenario: this, sink, driver: firstCdp, phase: "executing" });
        await acquireInteractiveShell({ context, driver: restarted.driver, sink }, {
          evidenceOwner: OWNER,
          screenshotStem: "provider-live-restarted-shell-ready",
        });
        secondInput = new WebviewInput(restarted.driver, { probeId: "provider-live-g2" });
        await secondInput.installProbe();
        await trustedClick(secondInput, SHOW_SETTINGS);
        await waitForProductStage({
          label: "restarted provider Preferences opened",
          sample: () => observeProviderConnectionLiveSurface(restarted.driver),
          decide: (surface) => !surfaceErrorFree(surface)
            ? "fail"
            : surface?.projection?.overlay === "config" ? "pass" : "pending",
          code: "provider-live-restart-preferences-open-failed",
          message: "Preferences did not open after the exact Desktop restart",
        });
        await openModelDetails(restarted.driver, secondInput);
        const restored = await waitForProductStage({
          label: "stable persisted OpenAI-compatible Preferences after restart",
          timeoutMs: 30_000,
          sample: () => observeProviderConnectionLiveSurface(restarted.driver),
          decide: stableRestoredDecision(options),
          code: "provider-live-restart-persistence-mismatch",
          message: "restart did not stably restore the saved OpenAI-compatible connection",
        });
        const restoredScreenshot = await captureScenarioScreenshot({
          cdp: restarted.driver,
          sink,
          name: "provider-openai-compatible-preferences-restored",
          owner: OWNER,
        });
        await sink.record("provider-live-preferences-restored", {
          restart: restarted.restart,
          stable_for_ms: SETTINGS_STABILITY_MS,
          surface: restored.value,
          screenshot: restoredScreenshot,
        }, { phase: "executing", owner: OWNER });

        await trustedClick(secondInput, CLOSE_SETTINGS);
        await waitForProductStage({
          label: "restored Preferences clean close",
          sample: () => observeProviderConnectionLiveSurface(restarted.driver),
          decide: (surface) => !surfaceErrorFree(surface)
            ? "fail"
            : surface?.projection?.overlay === "none"
              && surface?.prompt?.count === 1
              && surface.prompt.visible === true
              && surface.prompt.enabled === true
              ? "pass"
              : "pending",
          code: "provider-live-restored-preferences-close-failed",
          message: "restored Preferences did not return to the interactive composer",
        });
        const promptInput = await trustedReplaceText({
          cdp: restarted.driver,
          input: secondInput,
          locator: PROMPT,
          text: PROVIDER_OPENAI_COMPATIBLE_PROMPT,
        });
        await waitForProductStage({
          label: "current_time prompt ready",
          sample: () => observeProviderConnectionLiveSurface(restarted.driver),
          decide: (surface) => !surfaceErrorFree(surface)
            ? "fail"
            : surface?.prompt?.value === PROVIDER_OPENAI_COMPATIBLE_PROMPT
              && surface?.send?.count === 1
              && surface.send.visible === true
              && surface.send.enabled === true
              ? "pass"
              : "pending",
          code: "provider-live-prompt-not-ready",
          message: "trusted GUI input did not produce a submit-ready current_time prompt",
        });
        const send = await trustedClick(secondInput, SEND);
        const terminal = await waitForProductStage({
          label: "OpenAI-compatible current_time terminal",
          timeoutMs: LIVE_TURN_TIMEOUT_MS,
          sample: () => observeProviderConnectionLiveSurface(restarted.driver),
          decide: liveCurrentTimeTerminalDecision,
          code: "provider-live-current-time-mismatch",
          message: "the OpenAI-compatible provider did not complete exactly one current_time tool round with a visible Japanese answer",
        });
        const time = currentTimeFromCompletedProjection(terminal.value.projection);
        const assistant = finalAssistant(terminal.value.projection);
        const terminalScreenshot = await captureScenarioScreenshot({
          cdp: restarted.driver,
          sink,
          name: "provider-openai-compatible-current-time-completed",
          owner: OWNER,
        });
        await sink.record("provider-live-current-time-completed", {
          input_kind: "browser_trusted",
          prompt_input: promptInput,
          send,
          provider_profile: terminal.value.projection.provider_effective_profile,
          provider_base_url: terminal.value.projection.provider_effective_base_url,
          model: terminal.value.projection.provider_effective_model_id,
          api_key_env: terminal.value.projection.provider_effective_api_key_env,
          current_time: time,
          assistant,
          surface: terminal.value,
          screenshot: terminalScreenshot,
        }, { phase: "executing", owner: OWNER });

        secondSettled = true;
        await settleGenerationResources(state, { input: secondInput, generation: 2 });
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        if (!firstSettled && (firstInput !== null || firstCommands !== null)) {
          firstSettled = true;
          try {
            await settleGenerationResources(state, {
              input: firstInput,
              commandProbe: firstCommands,
              generation: 1,
              primaryError,
            });
          } catch (error) {
            if (primaryError === null) throw error;
          }
        }
        if (!secondSettled && secondInput !== null) {
          secondSettled = true;
          try {
            await settleGenerationResources(state, {
              input: secondInput,
              generation: 2,
              primaryError,
            });
          } catch (error) {
            if (primaryError === null) throw error;
          }
        }
      }
    },
    async quiesce() {
      if (state.quiesceOutcome !== null) return structuredClone(state.quiesceOutcome);
      const resourcesPass = state.generationResources.every((resource) => resource.failures.length === 0);
      state.quiesceOutcome = {
        input: resourcesPass ? "pass" : "fail",
        resources: [{
          kind: "external-openai-compatible-provider",
          provider_base_url: options.providerBaseUrl,
          model: options.model,
          owned_by_scenario: false,
          cleanup_action: "none",
          generation_resources: structuredClone(state.generationResources),
        }],
      };
      return structuredClone(state.quiesceOutcome);
    },
    async cleanup({ context }) {
      let current = null;
      let failure = null;
      if (state.sentinelBaseline !== null) {
        try { current = await physicalFileIdentity(path.join(context.paths.workspace, SENTINEL_NAME)); }
        catch (error) { failure = errorObservation(error); }
      }
      const sentinelPass = state.sentinelBaseline === null
        || (failure === null && sameValue(current, state.sentinelBaseline));
      const quiescePass = state.quiesceOutcome?.input === "pass";
      return {
        input: sentinelPass && quiescePass ? "pass" : "fail",
        resources: [{
          kind: "provider-live-workspace-sentinel",
          baseline: state.sentinelBaseline,
          current,
          failure,
          unchanged: sentinelPass,
          quiesce_input: state.quiesceOutcome?.input ?? null,
        }],
      };
    },
  });
}

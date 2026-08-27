import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import {
  DesktopCommandProbe,
  assertExactDesktopCommandSequence,
} from "../drivers/desktop_command_probe.mjs";
import {
  SCRIPTED_PROVIDER_MODEL_ID,
  startScriptedProvider,
} from "../drivers/scripted_provider.mjs";
import { WebviewInput, assertTrustedProbeSequence } from "../drivers/webview_input.mjs";
import {
  TAURI_MAIN_WINDOW_CLASS,
  dragExactOwnedWindow,
  exactOwnedWindowDragObserved,
  selectSingleOwnedRootWindow,
  snapshotOwnedTopLevelWindows,
} from "../drivers/windows_native_input.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import {
  captureScenarioScreenshot,
  selectedNavigationIdentity,
} from "./observations.mjs";
import { quiesceProviderResource } from "./provider_restart.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";

const OWNER = "scenario:settings.preferences";
export const SETTINGS_RESTORE_STABILITY_MS = 500;
export const PROVIDER_CONTEXT_BEFORE = "65536";
export const PROVIDER_CONTEXT_AFTER = "65537";
export const SETTINGS_PROVIDER_PROFILE = "openai_responses";
export const SETTINGS_PROVIDER_API_KEY_ENV = "";
export const PROVIDER_PROFILE_OPTIONS = Object.freeze([
  "lm_studio",
  "openai_compatible",
  "openai_responses",
  "lm_studio_chat_completions",
]);

const SHOW_PROVIDER = Object.freeze({
  selector: 'aside.sidebar button[data-action="show-provider"][title="LLM URL"]',
  identity: { tag: "BUTTON", action: "show-provider" },
});
const PROVIDER_CONTEXT = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="provider-dialog-title"] input#provider-context-window',
  identity: { tag: "INPUT", id: "provider-context-window" },
});
const SAVE_PROVIDER_GLOBAL = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="provider-dialog-title"] button[data-action="save-provider-global"]',
  identity: { tag: "BUTTON", action: "save-provider-global" },
});
const CLOSE_PROVIDER = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="provider-dialog-title"] button[data-action="close-overlay"]',
  identity: { tag: "BUTTON", action: "close-overlay" },
});
const SHOW_SETTINGS = Object.freeze({
  selector: 'aside.sidebar button.settings[data-action="show-config"][title="設定"]',
  identity: { tag: "BUTTON", action: "show-config" },
});
const SETTINGS_TOOLS = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] nav.settings-nav a[href="#settings-tools"]',
  identity: { tag: "A", href: "#settings-tools" },
});
const DOCLING_TOGGLE = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] label.settings-toggle[data-config-key="docling.enabled"]',
  identity: { tag: "LABEL", configKey: "docling.enabled" },
  forwardedClickIdentity: { tag: "INPUT", configKey: "docling.enabled" },
});
const CLOSE_SETTINGS = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] button[data-action="close-overlay"]',
  identity: { tag: "BUTTON", action: "close-overlay" },
});
const SAVE_GLOBAL_CONFIG = Object.freeze({
  selector: '[role="dialog"][aria-labelledby="config-dialog-title"] button[data-action="save-global-config"]',
  identity: { tag: "BUTTON", action: "save-global-config" },
});
const CANCEL_DIRTY_CLOSE = Object.freeze({
  selector: '[role="alertdialog"][aria-labelledby="settings-close-confirm-title"] button[data-action="cancel-local-confirm"]',
  identity: { tag: "BUTTON", action: "cancel-local-confirm" },
});
const CONFIRM_DISCARD_CLOSE = Object.freeze({
  selector: '[role="alertdialog"][aria-labelledby="settings-close-confirm-title"] button[data-action="confirm-settings-discard-close"]',
  identity: { tag: "BUTTON", action: "confirm-settings-discard-close" },
});

function sameValue(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
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

function sameConfigOwner(left, right) {
  return left?.workspacePath === right?.workspacePath
    && left?.sessionId === right?.sessionId;
}

function advancedConfigGeneration(current, baseline) {
  return sameConfigOwner(current, baseline)
    && typeof current?.configGeneration === "string"
    && current.configGeneration.length > 0
    && current.configGeneration !== baseline?.configGeneration;
}

function errorFree(surface) {
  return surface?.visible_fatal_count === 0
    && surface?.visible_recoverable_error_count === 0
    && surface?.visible_validation_error_count === 0;
}

function networkStillZero(ledger) {
  return Array.isArray(ledger) && ledger.length === 0;
}

export function settingsPreferencesFixtureConfig(baseUrl) {
  return `[model]
base_url = ${JSON.stringify(baseUrl)}
model = ${JSON.stringify(SCRIPTED_PROVIDER_MODEL_ID)}
provider_profile = ${JSON.stringify(SETTINGS_PROVIDER_PROFILE)}
connect_timeout_ms = 1000
request_timeout_ms = 30000
max_retries = 0
context_window = ${PROVIDER_CONTEXT_BEFORE}
supports_tools = false
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
base_url = ${JSON.stringify(baseUrl)}
timeout_ms = 1000

[mcp]
enabled = false
`;
}

export async function observeSettingsPreferencesSurface(cdp) {
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
    const identity = (element) => ({
      tag: element instanceof Element ? element.tagName.toUpperCase() : '',
      id: element instanceof Element && element.id ? element.id : null,
      action: element instanceof HTMLElement ? (element.dataset.action ?? null) : null,
      focusKey: element instanceof HTMLElement ? (element.dataset.focusKey ?? null) : null,
      configKey: element instanceof HTMLElement ? (element.dataset.configKey ?? null) : null,
      href: element instanceof HTMLAnchorElement ? element.getAttribute('href') : null,
    });
    const rows = (selector) => Array.from(document.querySelectorAll(selector));
    const one = (selector) => {
      const matches = rows(selector);
      const node = matches.length === 1 ? matches[0] : null;
      return { count: matches.length, visible: visible(node), node };
    };
    const input = (selector) => {
      const found = one(selector);
      return {
        count: found.count,
        visible: found.visible,
        enabled: found.node instanceof HTMLInputElement && !found.node.disabled && !found.node.readOnly,
        value: found.node instanceof HTMLInputElement ? found.node.value : null,
        checked: found.node instanceof HTMLInputElement && found.node.type === 'checkbox' ? found.node.checked : null,
      };
    };
    const select = (selector) => {
      const found = one(selector);
      return {
        count: found.count,
        visible: found.visible,
        enabled: found.node instanceof HTMLSelectElement && !found.node.disabled,
        value: found.node instanceof HTMLSelectElement ? found.node.value : null,
        options: found.node instanceof HTMLSelectElement
          ? Array.from(found.node.options).map((option) => option.value)
          : [],
      };
    };
    const button = (selector) => {
      const found = one(selector);
      return {
        count: found.count,
        visible: found.visible,
        enabled: found.node instanceof HTMLButtonElement && !found.node.disabled && found.node.getAttribute('aria-disabled') !== 'true',
      };
    };
    const providerDialog = one('[role="dialog"][aria-labelledby="provider-dialog-title"]');
    const settingsDialog = one('[role="dialog"][aria-labelledby="config-dialog-title"]');
    const closeConfirmation = one('[role="alertdialog"][aria-labelledby="settings-close-confirm-title"]');
    const doclingLabel = one('label.settings-toggle[data-config-key="docling.enabled"]');
    const doclingReadinessStatus = one('[role="dialog"][aria-labelledby="config-dialog-title"] #docling-readiness-status[data-settings-live-region="docling-readiness"]');
    const titlebarDrag = one('.app-titlebar .titlebar-drag[data-drag-region]');
    const dragRect = titlebarDrag.node instanceof HTMLElement ? titlebarDrag.node.getBoundingClientRect() : null;
    return {
      projection,
      selected_navigation: (() => {
        const selected = rows('button.nav-row[aria-current="page"][data-action="session"], button.nav-row[aria-current="page"][data-action="chat-session"]');
        return selected.map(identity);
      })(),
      provider: {
        dialog_count: providerDialog.count,
        dialog_visible: providerDialog.visible,
        profile: select('[role="dialog"][aria-labelledby="provider-dialog-title"] #provider-profile'),
        api_key_env: input('[role="dialog"][aria-labelledby="provider-dialog-title"] #provider-api-key-env'),
        context: input('[role="dialog"][aria-labelledby="provider-dialog-title"] #provider-context-window'),
        max_output_tokens: input('[role="dialog"][aria-labelledby="provider-dialog-title"] #provider-max-output-tokens'),
        save: button('[role="dialog"][aria-labelledby="provider-dialog-title"] button[data-action="save-provider-global"]'),
        load_models: button('[role="dialog"][aria-labelledby="provider-dialog-title"] button[data-action="load-provider-models"]'),
        close: button('[role="dialog"][aria-labelledby="provider-dialog-title"] button[data-action="close-overlay"]'),
      },
      settings: {
        dialog_count: settingsDialog.count,
        dialog_visible: settingsDialog.visible,
        dialog_inert: settingsDialog.node instanceof HTMLElement && settingsDialog.node.closest('[inert]') !== null,
        profile: select('[role="dialog"][aria-labelledby="config-dialog-title"] .settings-control[data-config-key="model.provider_profile"]'),
        api_key_env: input('[role="dialog"][aria-labelledby="config-dialog-title"] .settings-control[data-config-key="model.api_key_env"]'),
        context: input('[role="dialog"][aria-labelledby="config-dialog-title"] .settings-control[data-config-key="model.context_window"]'),
        max_output_tokens: input('[role="dialog"][aria-labelledby="config-dialog-title"] .settings-control[data-config-key="model.max_output_tokens"]'),
        docling: input('[role="dialog"][aria-labelledby="config-dialog-title"] input.settings-control[data-config-key="docling.enabled"]'),
        docling_label: {
          count: doclingLabel.count,
          visible: doclingLabel.visible,
          text: doclingLabel.node instanceof HTMLElement ? doclingLabel.node.innerText.trim() : null,
        },
        docling_readiness: {
          button: button('[role="dialog"][aria-labelledby="config-dialog-title"] button[data-action="check-docling-readiness"][aria-controls="docling-readiness-status"]'),
          status_count: doclingReadinessStatus.count,
          status_visible: doclingReadinessStatus.visible,
          status: doclingReadinessStatus.node instanceof HTMLElement
            ? doclingReadinessStatus.node.dataset.doclingReadinessStatus ?? null
            : null,
          aria_busy: doclingReadinessStatus.node instanceof HTMLElement
            ? doclingReadinessStatus.node.getAttribute('aria-busy')
            : null,
          text: doclingReadinessStatus.node instanceof HTMLElement ? doclingReadinessStatus.node.innerText.trim() : null,
        },
        dirty_badge_visible: rows('[role="dialog"][aria-labelledby="config-dialog-title"] .dirty-badge.visible').filter(visible).length === 1,
        save: button('[role="dialog"][aria-labelledby="config-dialog-title"] button[data-action="save-global-config"]'),
        discard: button('[role="dialog"][aria-labelledby="config-dialog-title"] button[data-action="discard-config-draft"]:not([hidden])'),
        close: button('[role="dialog"][aria-labelledby="config-dialog-title"] button[data-action="close-overlay"]'),
      },
      close_confirmation: {
        count: closeConfirmation.count,
        visible: closeConfirmation.visible,
        cancel: button('[role="alertdialog"][aria-labelledby="settings-close-confirm-title"] button[data-action="cancel-local-confirm"]'),
        discard_close: button('[role="alertdialog"][aria-labelledby="settings-close-confirm-title"] button[data-action="confirm-settings-discard-close"]'),
      },
      titlebar: {
        drag_count: titlebarDrag.count,
        drag_visible: titlebarDrag.visible,
        drag_rect: dragRect === null ? null : {
          left: dragRect.left,
          top: dragRect.top,
          width: dragRect.width,
          height: dragRect.height,
        },
        screen: {
          x: window.screenX,
          y: window.screenY,
          outer_width: window.outerWidth,
          outer_height: window.outerHeight,
          avail_left: window.screen.availLeft,
          avail_top: window.screen.availTop,
          avail_width: window.screen.availWidth,
          avail_height: window.screen.availHeight,
        },
      },
      active: identity(document.activeElement),
      visible_dialog_count: rows('[role="dialog"], [role="alertdialog"]').filter(visible).length,
      visible_backdrop_count: rows('.modal-backdrop').filter(visible).length,
      visible_fatal_count: rows('.fatal').filter(visible).length,
      visible_recoverable_error_count: rows('.ui-error-notice').filter(visible).length,
      visible_validation_error_count: rows('.validation.error').filter(visible).length,
    };
  })()`);
}

export function shellReadyForSettingsDrag(surface, ledger) {
  const rect = surface?.titlebar?.drag_rect;
  return networkStillZero(ledger)
    && errorFree(surface)
    && surface?.projection?.overlay === "none"
    && surface?.visible_dialog_count === 0
    && surface?.visible_backdrop_count === 0
    && surface?.titlebar?.drag_count === 1
    && surface?.titlebar?.drag_visible === true
    && Number.isFinite(rect?.left)
    && Number.isFinite(rect?.top)
    && Number.isFinite(rect?.width)
    && rect.width >= 32
    && Number.isFinite(rect?.height)
    && rect.height >= 16;
}

export function settingsTriggerRestoredShellReady(surface, ledger) {
  return shellReadyForSettingsDrag(surface, ledger)
    && surface?.active?.tag === "BUTTON"
    && surface.active.action === "show-config";
}

export function providerEditorReady(surface, ledger, expectedContext = PROVIDER_CONTEXT_BEFORE) {
  return networkStillZero(ledger)
    && errorFree(surface)
    && surface?.projection?.overlay === "provider"
    && surface?.provider?.dialog_count === 1
    && surface.provider.dialog_visible === true
    && surface.provider.profile.count === 1
    && surface.provider.profile.visible === true
    && surface.provider.profile.enabled === true
    && surface.provider.profile.value === SETTINGS_PROVIDER_PROFILE
    && sameValue(surface.provider.profile.options, PROVIDER_PROFILE_OPTIONS)
    && surface.provider.api_key_env.count === 1
    && surface.provider.api_key_env.visible === true
    && surface.provider.api_key_env.enabled === true
    && surface.provider.api_key_env.value === SETTINGS_PROVIDER_API_KEY_ENV
    && surface.provider.context.count === 1
    && surface.provider.context.visible === true
    && surface.provider.context.enabled === true
    && surface.provider.context.value === expectedContext
    && surface.provider.max_output_tokens.count === 0
    && surface.provider.max_output_tokens.visible === false
    && surface.provider.save.count === 1
    && surface.provider.save.visible === true
    && surface.provider.load_models.count === 1
    && surface.provider.close.count === 1
    && surface?.visible_dialog_count === 1;
}

export function preferencesReady(surface, ledger, { contextWindow, doclingEnabled }) {
  return networkStillZero(ledger)
    && errorFree(surface)
    && surface?.projection?.overlay === "config"
    && surface?.settings?.dialog_count === 1
    && surface.settings.dialog_visible === true
    && surface.settings.profile.count === 1
    && surface.settings.profile.visible === true
    && surface.settings.profile.enabled === true
    && surface.settings.profile.value === SETTINGS_PROVIDER_PROFILE
    && sameValue(surface.settings.profile.options, PROVIDER_PROFILE_OPTIONS)
    && surface.settings.api_key_env.count === 1
    && surface.settings.api_key_env.visible === true
    && surface.settings.api_key_env.enabled === true
    && surface.settings.api_key_env.value === SETTINGS_PROVIDER_API_KEY_ENV
    && surface.settings.context.count === 1
    && surface.settings.context.value === contextWindow
    && surface.settings.max_output_tokens.count === 0
    && surface.settings.max_output_tokens.visible === false
    && surface.settings.docling.count === 1
    && surface.settings.docling.checked === doclingEnabled
    && surface.settings.dirty_badge_visible === false
    && surface.settings.save.count === 1
    && surface.settings.save.enabled === false
    && surface.settings.discard.count === 0
    && surface.settings.close.count === 1
    && surface?.close_confirmation?.count === 0
    && surface?.visible_dialog_count === 1
    && fieldValue(surface.projection, "model.context_window") === contextWindow
    && fieldValue(surface.projection, "model.provider_profile") === SETTINGS_PROVIDER_PROFILE
    && fieldValue(surface.projection, "model.api_key_env") === SETTINGS_PROVIDER_API_KEY_ENV
    && fieldValue(surface.projection, "docling.enabled") === String(doclingEnabled);
}

export function dirtyDoclingPreferencesReady(surface, ledger, expectedTarget, expectedPersisted = false) {
  return networkStillZero(ledger)
    && errorFree(surface)
    && surface?.projection?.overlay === "config"
    && sameValue(surface?.projection?.config_target, expectedTarget)
    && surface?.settings?.dialog_count === 1
    && surface.settings.docling.count === 1
    && surface.settings.docling.checked === true
    && surface.settings.dirty_badge_visible === true
    && surface.settings.save.enabled === true
    && surface.settings.discard.count === 1
    && surface.settings.discard.enabled === true
    && surface?.close_confirmation?.count === 0
    && fieldValue(surface.projection, "docling.enabled") === String(expectedPersisted);
}

export function dirtyCloseGuardReady(surface, ledger, expectedTarget) {
  return networkStillZero(ledger)
    && errorFree(surface)
    && surface?.projection?.overlay === "config"
    && sameValue(surface?.projection?.config_target, expectedTarget)
    && surface?.settings?.dialog_count === 1
    && surface.settings.dialog_inert === true
    && surface.settings.docling.checked === true
    && surface.settings.dirty_badge_visible === true
    && surface?.close_confirmation?.count === 1
    && surface.close_confirmation.visible === true
    && surface.close_confirmation.cancel.count === 1
    && surface.close_confirmation.cancel.enabled === true
    && surface.close_confirmation.discard_close.count === 1
    && surface.close_confirmation.discard_close.enabled === true
    && surface?.visible_dialog_count === 2;
}

export function savedPreferencesReady(surface, ledger, baselineTarget) {
  return networkStillZero(ledger)
    && errorFree(surface)
    && surface?.projection?.overlay === "config"
    && advancedConfigGeneration(surface?.projection?.config_target, baselineTarget)
    && fieldValue(surface.projection, "model.context_window") === PROVIDER_CONTEXT_AFTER
    && fieldValue(surface.projection, "docling.enabled") === "true"
    && surface?.settings?.context?.value === PROVIDER_CONTEXT_AFTER
    && surface?.settings?.docling?.checked === true
    && surface?.settings?.dirty_badge_visible === false
    && surface?.settings?.save?.enabled === false
    && surface?.close_confirmation?.count === 0;
}

export function createStablePreferencesDecision({
  expectedContext = PROVIDER_CONTEXT_AFTER,
  expectedDocling = true,
  minimumStableMs = SETTINGS_RESTORE_STABILITY_MS,
  now = () => Date.now(),
} = {}) {
  if (!Number.isFinite(minimumStableMs) || minimumStableMs <= 0) throw new TypeError("minimum stable duration must be positive");
  let acceptedSince = null;
  return ({ surface, ledger }) => {
    const accepted = preferencesReady(surface, ledger, {
      contextWindow: expectedContext,
      doclingEnabled: expectedDocling,
    });
    if (!accepted) {
      acceptedSince = null;
      if (!networkStillZero(ledger) || (surface && !errorFree(surface))) return "fail";
      return "pending";
    }
    const observedAt = now();
    if (acceptedSince === null) {
      acceptedSince = observedAt;
      return "pending";
    }
    return observedAt - acceptedSince >= minimumStableMs ? "pass" : "pending";
  };
}

export function expectedProviderGlobalSave(surface, contextWindow = PROVIDER_CONTEXT_AFTER) {
  const projection = surface?.projection;
  const selectedModelId = projection?.provider_model_ids?.[projection?.provider_selected_index];
  if (!projection?.config_target || typeof selectedModelId !== "string" || selectedModelId.length === 0) {
    throw new TypeError("provider save expectation requires one selected model and config target");
  }
  return {
    command: "save_provider_global",
    args: {
      input: {
        baseUrl: projection.provider_base_url,
        providerProfile: projection.provider_profile,
        apiKeyEnv: projection.provider_api_key_env,
        contextWindow,
        selectedModelId,
      },
      expectedTarget: structuredClone(projection.config_target),
      draftValues: configValues(projection),
    },
  };
}

export function expectedResetThenClose(surface) {
  const projection = surface?.projection;
  if (!projection?.config_target) throw new TypeError("dirty close expectation requires a config target");
  return [
    {
      command: "reset_config_draft",
      args: {
        values: configValues(projection),
        expectedTarget: structuredClone(projection.config_target),
      },
    },
    { command: "close_overlay", args: {} },
  ];
}

export function expectedGlobalSave(surface) {
  const projection = surface?.projection;
  if (!projection?.config_target) throw new TypeError("global save expectation requires a config target");
  return {
    command: "save_global_config",
    args: {
      values: configValues(projection, { "docling.enabled": "true" }),
      expectedTarget: structuredClone(projection.config_target),
    },
  };
}

function classifyObservationFailure(error, code, message) {
  if (error?.code === "observation-timeout" && error?.evidence?.last_error === null) {
    return productFailure(code, message, error.evidence);
  }
  return error;
}

async function waitForProductStage({ label, timeoutMs = 10_000, sample, decide, code, message }) {
  let terminal = "pending";
  let observed;
  try {
    observed = await waitForObservation({
      label,
      timeoutMs,
      pollMs: 75,
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

function surfaceDecision(predicate) {
  return (sample) => {
    if (!networkStillZero(sample?.ledger) || (sample?.surface && !errorFree(sample.surface))) return "fail";
    return predicate(sample?.surface, sample?.ledger) ? "pass" : "pending";
  };
}

export function trustedClickProbeEvents(locator) {
  const expected = [
    { type: "pointerdown", identity: locator.identity, button: 0, buttons: 1 },
    { type: "pointerup", identity: locator.identity, button: 0, buttons: 0 },
    { type: "click", identity: locator.identity, button: 0, buttons: 0 },
  ];
  if (locator.forwardedClickIdentity !== undefined) {
    expected.push({
      type: "click",
      identity: locator.forwardedClickIdentity,
      button: 0,
      buttons: 0,
    });
  }
  return expected;
}

async function trustedClick(input, locator) {
  const start = (await input.snapshotProbe()).sequence;
  const target = await input.click(locator);
  const snapshot = await input.snapshotProbe(start);
  const probe = assertTrustedProbeSequence(snapshot, {
    afterSequence: start,
    expected: trustedClickProbeEvents(locator),
  });
  return { target, probe, sequence: snapshot.sequence };
}

function digitEvents(text, identity) {
  return Array.from(text).flatMap((character) => [
    { type: "keydown", identity, key: character, code: `Digit${character}` },
    { type: "input", identity, inputType: "insertText", data: character },
    { type: "keyup", identity, key: character, code: `Digit${character}` },
  ]);
}

async function trustedReplaceDigits(input, locator, text) {
  if (!/^\d+$/.test(text)) throw new TypeError("trusted replacement requires decimal digits");
  const click = await trustedClick(input, locator);
  const start = click.sequence;
  await input.keyDown("Control");
  await input.pressKey("a");
  await input.keyUp("Control");
  await input.typeText(text);
  const snapshot = await input.snapshotProbe(start);
  const probe = assertTrustedProbeSequence(snapshot, {
    afterSequence: start,
    expected: [
      { type: "keydown", identity: locator.identity, key: "Control", code: "ControlLeft" },
      { type: "keydown", identity: locator.identity, key: "a", code: "KeyA" },
      { type: "keyup", identity: locator.identity, key: "a", code: "KeyA" },
      { type: "keyup", identity: locator.identity, key: "Control", code: "ControlLeft" },
      ...digitEvents(text, locator.identity),
    ],
  });
  return { click, probe, sequence: snapshot.sequence };
}

async function trustedEscape(input, identity) {
  const start = (await input.snapshotProbe()).sequence;
  await input.pressKey("Escape");
  const snapshot = await input.snapshotProbe(start);
  const probe = assertTrustedProbeSequence(snapshot, {
    afterSequence: start,
    expected: [{ type: "keydown", identity, key: "Escape", code: "Escape" }],
  });
  return { probe, sequence: snapshot.sequence };
}

async function waitForCommands(probe, afterSequence, expected, label) {
  const observed = await waitForObservation({
    label,
    timeoutMs: 10_000,
    pollMs: 50,
    retrySampleErrors: false,
    sample: () => probe.snapshot(afterSequence),
    accept: (snapshot) => snapshot.calls.length >= expected.length,
  });
  return assertExactDesktopCommandSequence(observed.value, { afterSequence, expected });
}

async function assertNoCommandsStable(probe, afterSequence, minimumStableMs = 250) {
  const deadline = Date.now() + minimumStableMs;
  let latest;
  do {
    latest = await probe.snapshot(afterSequence);
    assertExactDesktopCommandSequence(latest, { afterSequence, expected: [] });
    await delay(50);
  } while (Date.now() < deadline);
  return latest;
}

function chooseDragDelta(screen) {
  const rightSpace = screen.avail_left + screen.avail_width - (screen.x + screen.outer_width);
  const bottomSpace = screen.avail_top + screen.avail_height - (screen.y + screen.outer_height);
  return {
    x: rightSpace >= 80 ? 64 : -64,
    y: bottomSpace >= 56 ? 36 : -36,
  };
}

async function settleGenerationResources(state, input, commandProbe, generation, primaryError) {
  const outcome = { generation, input: null, command_probe: null, failures: [] };
  try { outcome.input = await input.cleanup(); }
  catch (error) { outcome.failures.push({ owner: "webview-input", error: errorObservation(error) }); }
  try { outcome.command_probe = await commandProbe.remove(); }
  catch (error) { outcome.failures.push({ owner: "desktop-command-probe", error: errorObservation(error) }); }
  state.generationResources.push(outcome);
  if (outcome.failures.length > 0 && primaryError === null) {
    throw new DesktopE2eError(
      "harness",
      "settings-generation-resource-cleanup-failed",
      "Settings WebView input/command probes did not settle before the generation boundary",
      outcome,
    );
  }
  return outcome;
}

export function createSettingsPreferencesScenario() {
  const state = {
    provider: null,
    acceptedLedger: null,
    quiesceOutcome: null,
    generationResources: [],
    drag: null,
  };
  return Object.freeze({
    id: "settings.preferences",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    requestGracefulExit,
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({ expectedPrompt: "settings-network-must-remain-unused" });
      await prepareDesktopFixture({
        context,
        sink,
        phase,
        owner: OWNER,
        configText: settingsPreferencesFixtureConfig(state.provider.baseUrl),
        sentinelName: "E2E_SETTINGS_PREFERENCES.txt",
        sentinelText: "moyAI Desktop E2E Preferences fixture.\n",
      });
      await sink.record("settings-zero-network-fixture", state.provider.resourceObservation(), { phase, owner: OWNER });
    },
    async execute({ context, runtime, driver: firstCdp, host, sink }) {
      const provider = state.provider;
      if (provider === null) throw new Error("Settings scripted provider was not prepared");
      await acquireInteractiveShell({ context, driver: firstCdp, sink }, {
        evidenceOwner: OWNER,
        screenshotStem: "settings-shell-ready",
      });
      const firstInput = new WebviewInput(firstCdp, { probeId: "settings-preferences-g1" });
      const firstCommands = new DesktopCommandProbe(firstCdp, {
        probeId: "settings-preferences-g1",
        commands: [
          "start_window_drag",
          "save_provider_global",
          "reset_config_draft",
          "save_global_config",
          "close_overlay",
        ],
      });
      let firstSettled = false;
      let primaryError = null;
      let secondInput = null;
      let secondCommands = null;
      let secondSettled = false;
      try {
        await firstInput.installProbe();
        await firstCommands.install();
        const shellSurface = await observeSettingsPreferencesSurface(firstCdp);
        if (!shellReadyForSettingsDrag(shellSurface, provider.requestLedger)) {
          throw productFailure("settings-shell-contract-mismatch", "Settings qualification shell was not error-free and idle", {
            surface: shellSurface,
            ledger: provider.requestLedger,
          });
        }
        const initialIdentity = selectedNavigationIdentity(shellSurface.projection);

        const nativeOwner = {
          executionRoot: context.root,
          ownerPath: runtime.desktop_owner_path,
          expectedOwner: runtime.desktop_owner,
        };
        const nativeSnapshot = await snapshotOwnedTopLevelWindows(nativeOwner);
        const mainWindow = selectSingleOwnedRootWindow(nativeSnapshot, runtime.desktop_owner, {
          expectedClassName: TAURI_MAIN_WINDOW_CLASS,
        });
        const dragRect = shellSurface.titlebar.drag_rect;
        const dragDelta = chooseDragDelta(shellSurface.titlebar.screen);
        const dragCommandStart = (await firstCommands.snapshot()).sequence;
        const drag = await dragExactOwnedWindow({
          ...nativeOwner,
          candidate: mainWindow,
          clientOffsetX: Math.round(dragRect.left + Math.min(dragRect.width / 2, 160)),
          clientOffsetY: Math.round(dragRect.top + dragRect.height / 2),
          deltaX: dragDelta.x,
          deltaY: dragDelta.y,
        });
        state.drag = structuredClone(drag);
        if (!exactOwnedWindowDragObserved(drag)) {
          throw productFailure("settings-titlebar-drag-did-not-move-window", "trusted native titlebar drag did not move the exact Desktop window", drag);
        }
        const dragCommand = await waitForCommands(
          firstCommands,
          dragCommandStart,
          [{ command: "start_window_drag", args: {} }],
          "exact titlebar drag command",
        );
        const afterDrag = await observeSettingsPreferencesSurface(firstCdp);
        if (!shellReadyForSettingsDrag(afterDrag, provider.requestLedger)
          || !sameValue(selectedNavigationIdentity(afterDrag.projection), initialIdentity)) {
          throw productFailure("settings-titlebar-drag-state-drift", "titlebar drag changed the idle Desktop state", { before: shellSurface, after: afterDrag });
        }
        await sink.record("settings-titlebar-drag-acquired", {
          input_kind: "windows_sendinput_exact_hwnd",
          native_snapshot: nativeSnapshot,
          candidate: mainWindow,
          drag,
          command: dragCommand,
        }, { phase: "executing", owner: OWNER });

        await trustedClick(firstInput, SHOW_PROVIDER);
        const providerOpened = await waitForProductStage({
          label: "provider editor opened without catalog traffic",
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(firstCdp), ledger: provider.requestLedger }),
          decide: surfaceDecision((surface, ledger) => providerEditorReady(surface, ledger)),
          code: "settings-provider-editor-not-ready",
          message: "provider editor did not open in its exact offline state",
        });
        const providerBefore = providerOpened.value.surface;
        const providerTarget = structuredClone(providerBefore.projection.config_target);
        const providerExpected = expectedProviderGlobalSave(providerBefore);
        const providerCommandStart = (await firstCommands.snapshot()).sequence;
        const providerTyping = await trustedReplaceDigits(firstInput, PROVIDER_CONTEXT, PROVIDER_CONTEXT_AFTER);
        const providerDirty = await waitForProductStage({
          label: "provider context edit ready to save",
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(firstCdp), ledger: provider.requestLedger }),
          decide: surfaceDecision((surface, ledger) => providerEditorReady(surface, ledger, PROVIDER_CONTEXT_AFTER)
            && surface.provider.save.enabled === true),
          code: "settings-provider-edit-not-ready",
          message: "trusted provider context edit did not produce a saveable offline draft",
        });
        await trustedClick(firstInput, SAVE_PROVIDER_GLOBAL);
        const providerSaved = await waitForProductStage({
          label: "provider-only global save settled",
          timeoutMs: 30_000,
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(firstCdp), ledger: provider.requestLedger }),
          decide: surfaceDecision((surface, ledger) => providerEditorReady(surface, ledger, PROVIDER_CONTEXT_AFTER)
            && fieldValue(surface.projection, "model.context_window") === PROVIDER_CONTEXT_AFTER
            && advancedConfigGeneration(surface.projection.config_target, providerTarget)),
          code: "settings-provider-save-did-not-settle",
          message: "provider-specific global save did not persist the trusted context edit",
        });
        const providerCommand = await waitForCommands(firstCommands, providerCommandStart, [providerExpected], "provider global save command");
        const providerSavedScreenshot = await captureScenarioScreenshot({ cdp: firstCdp, sink, name: "settings-provider-saved", owner: OWNER });
        await sink.record("settings-provider-save-acquired", {
          typing: providerTyping,
          dirty: providerDirty.value.surface,
          saved: providerSaved.value.surface,
          command: providerCommand,
          provider_ledger: provider.requestLedger,
          screenshot: providerSavedScreenshot,
        }, { phase: "executing", owner: OWNER });
        const providerCloseStart = (await firstCommands.snapshot()).sequence;
        await trustedClick(firstInput, CLOSE_PROVIDER);
        await waitForProductStage({
          label: "provider editor clean close",
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(firstCdp), ledger: provider.requestLedger }),
          decide: surfaceDecision((surface) => surface?.projection?.overlay === "none" && surface?.visible_dialog_count === 0),
          code: "settings-provider-close-failed",
          message: "explicit provider close did not restore the shell",
        });
        await waitForCommands(firstCommands, providerCloseStart, [{ command: "close_overlay", args: {} }], "provider close command");

        await trustedClick(firstInput, SHOW_SETTINGS);
        const preferencesOpened = await waitForProductStage({
          label: "clean Preferences opened",
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(firstCdp), ledger: provider.requestLedger }),
          decide: surfaceDecision((surface, ledger) => preferencesReady(surface, ledger, {
            contextWindow: PROVIDER_CONTEXT_AFTER,
            doclingEnabled: false,
          })),
          code: "settings-preferences-not-ready",
          message: "Preferences did not open with the persisted provider edit and clean Docling state",
        });
        await trustedClick(firstInput, SETTINGS_TOOLS);
        await waitForProductStage({
          label: "Docling toggle visible after trusted Settings navigation",
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(firstCdp), ledger: provider.requestLedger }),
          decide: surfaceDecision((surface) => surface?.settings?.docling_label?.count === 1
            && surface.settings.docling_label.visible === true),
          code: "settings-docling-toggle-not-visible",
          message: "trusted Settings navigation did not expose the stable Docling toggle",
        });
        const dirtyTarget = structuredClone(preferencesOpened.value.surface.projection.config_target);
        await trustedClick(firstInput, DOCLING_TOGGLE);
        const dirty = await waitForProductStage({
          label: "dirty Docling Preferences",
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(firstCdp), ledger: provider.requestLedger }),
          decide: surfaceDecision((surface, ledger) => dirtyDoclingPreferencesReady(surface, ledger, dirtyTarget)),
          code: "settings-docling-draft-not-dirty",
          message: "trusted Docling toggle did not produce the exact dirty Preferences state",
        });

        const dirtyGuardStart = (await firstCommands.snapshot()).sequence;
        await trustedClick(firstInput, CLOSE_SETTINGS);
        const explicitGuard = await waitForProductStage({
          label: "dirty Preferences explicit close guard",
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(firstCdp), ledger: provider.requestLedger }),
          decide: surfaceDecision((surface, ledger) => dirtyCloseGuardReady(surface, ledger, dirtyTarget)),
          code: "settings-explicit-close-guard-missing",
          message: "explicit close did not guard the dirty Preferences draft",
        });
        await assertNoCommandsStable(firstCommands, dirtyGuardStart);
        const guardScreenshot = await captureScenarioScreenshot({ cdp: firstCdp, sink, name: "settings-dirty-close-guard", owner: OWNER });
        await trustedClick(firstInput, CANCEL_DIRTY_CLOSE);
        const cancelledGuard = await waitForProductStage({
          label: "dirty Preferences close cancellation",
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(firstCdp), ledger: provider.requestLedger }),
          decide: surfaceDecision((surface, ledger) => dirtyDoclingPreferencesReady(surface, ledger, dirtyTarget)
            && surface?.active?.action === "close-overlay"),
          code: "settings-close-cancel-lost-draft",
          message: "cancelling dirty close did not preserve the exact Preferences draft",
        });
        await assertNoCommandsStable(firstCommands, dirtyGuardStart);

        await trustedEscape(firstInput, CLOSE_SETTINGS.identity);
        const escapeGuard = await waitForProductStage({
          label: "dirty Preferences Escape guard",
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(firstCdp), ledger: provider.requestLedger }),
          decide: surfaceDecision((surface, ledger) => dirtyCloseGuardReady(surface, ledger, dirtyTarget)),
          code: "settings-escape-close-guard-missing",
          message: "Escape did not route through the dirty Preferences close guard",
        });
        await assertNoCommandsStable(firstCommands, dirtyGuardStart);
        const resetThenClose = expectedResetThenClose(escapeGuard.value.surface);
        await trustedClick(firstInput, CONFIRM_DISCARD_CLOSE);
        await waitForProductStage({
          label: "dirty Preferences discard and close",
          timeoutMs: 30_000,
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(firstCdp), ledger: provider.requestLedger }),
          decide: surfaceDecision((surface, ledger) => settingsTriggerRestoredShellReady(surface, ledger)),
          code: "settings-discard-close-did-not-settle",
          message: "confirmed dirty discard did not reset then close Preferences and restore the Settings trigger focus",
        });
        const discardCommands = await waitForCommands(firstCommands, dirtyGuardStart, resetThenClose, "dirty Preferences reset then close commands");

        await trustedClick(firstInput, SHOW_SETTINGS);
        const reopenedClean = await waitForProductStage({
          label: "Preferences reopened after discard",
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(firstCdp), ledger: provider.requestLedger }),
          decide: surfaceDecision((surface, ledger) => preferencesReady(surface, ledger, {
            contextWindow: PROVIDER_CONTEXT_AFTER,
            doclingEnabled: false,
          })),
          code: "settings-discard-was-not-exact",
          message: "reopened Preferences retained a discarded Docling edit",
        });
        await trustedClick(firstInput, SETTINGS_TOOLS);
        await waitForProductStage({
          label: "Docling toggle visible for saved edit",
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(firstCdp), ledger: provider.requestLedger }),
          decide: surfaceDecision((surface) => surface?.settings?.docling_label?.visible === true),
          code: "settings-docling-toggle-not-visible-after-reopen",
          message: "Docling toggle was not actionable after reopening Preferences",
        });
        const globalSaveTarget = structuredClone(reopenedClean.value.surface.projection.config_target);
        await trustedClick(firstInput, DOCLING_TOGGLE);
        const saveable = await waitForProductStage({
          label: "saveable Docling Preferences",
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(firstCdp), ledger: provider.requestLedger }),
          decide: surfaceDecision((surface, ledger) => dirtyDoclingPreferencesReady(surface, ledger, globalSaveTarget)),
          code: "settings-docling-second-edit-not-dirty",
          message: "second trusted Docling edit did not become saveable",
        });
        const globalExpected = expectedGlobalSave(saveable.value.surface);
        const globalCommandStart = (await firstCommands.snapshot()).sequence;
        await trustedClick(firstInput, SAVE_GLOBAL_CONFIG);
        const globalSaved = await waitForProductStage({
          label: "global Preferences save",
          timeoutMs: 30_000,
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(firstCdp), ledger: provider.requestLedger }),
          decide: surfaceDecision((surface, ledger) => savedPreferencesReady(surface, ledger, globalSaveTarget)),
          code: "settings-global-save-did-not-settle",
          message: "global Save did not persist Docling while preserving the provider edit",
        });
        const globalCommand = await waitForCommands(firstCommands, globalCommandStart, [globalExpected], "global Preferences save command");
        const savedScreenshot = await captureScenarioScreenshot({ cdp: firstCdp, sink, name: "settings-preferences-saved", owner: OWNER });
        const cleanCloseStart = (await firstCommands.snapshot()).sequence;
        await trustedClick(firstInput, CLOSE_SETTINGS);
        await waitForProductStage({
          label: "clean Preferences explicit close",
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(firstCdp), ledger: provider.requestLedger }),
          decide: surfaceDecision((surface) => surface?.projection?.overlay === "none"
            && surface?.visible_dialog_count === 0
            && surface?.visible_backdrop_count === 0),
          code: "settings-clean-explicit-close-failed",
          message: "explicit close did not close clean Preferences exactly",
        });
        const cleanCloseCommand = await waitForCommands(firstCommands, cleanCloseStart, [{ command: "close_overlay", args: {} }], "clean Preferences close command");
        await sink.record("settings-preferences-first-generation", {
          explicit_guard: explicitGuard.value.surface,
          cancelled_guard: cancelledGuard.value.surface,
          escape_guard: escapeGuard.value.surface,
          discard_commands: discardCommands,
          global_saved: globalSaved.value.surface,
          global_command: globalCommand,
          clean_close_command: cleanCloseCommand,
          provider_ledger: provider.requestLedger,
          screenshots: { guard: guardScreenshot, saved: savedScreenshot },
        }, { phase: "executing", owner: OWNER });

        firstSettled = true;
        await settleGenerationResources(state, firstInput, firstCommands, 1, null);
        const restarted = await host.restart({ context, scenario: this, sink, driver: firstCdp, phase: "executing" });
        await acquireInteractiveShell({ context, driver: restarted.driver, sink }, {
          evidenceOwner: OWNER,
          screenshotStem: "settings-restarted-shell-ready",
        });
        if (!networkStillZero(provider.requestLedger)) {
          throw productFailure("settings-cold-start-network-request", "restart contacted the provider or enabled Docling implicitly", {
            ledger: provider.requestLedger,
          });
        }
        secondInput = new WebviewInput(restarted.driver, { probeId: "settings-preferences-g2" });
        secondCommands = new DesktopCommandProbe(restarted.driver, {
          probeId: "settings-preferences-g2",
          commands: ["close_overlay"],
        });
        await secondInput.installProbe();
        await secondCommands.install();
        await trustedClick(secondInput, SHOW_SETTINGS);
        const stableDecision = createStablePreferencesDecision();
        const restored = await waitForProductStage({
          label: "persisted Preferences after exact Desktop restart",
          timeoutMs: 30_000,
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(restarted.driver), ledger: provider.requestLedger }),
          decide: stableDecision,
          code: "settings-restart-persistence-mismatch",
          message: "restart did not stably restore provider and Docling settings without network activity",
        });
        const restoredScreenshot = await captureScenarioScreenshot({ cdp: restarted.driver, sink, name: "settings-preferences-restored", owner: OWNER });
        const restoredCloseStart = (await secondCommands.snapshot()).sequence;
        await trustedClick(secondInput, CLOSE_SETTINGS);
        await waitForProductStage({
          label: "restored clean Preferences close",
          sample: async () => ({ surface: await observeSettingsPreferencesSurface(restarted.driver), ledger: provider.requestLedger }),
          decide: surfaceDecision((surface) => surface?.projection?.overlay === "none" && surface?.visible_dialog_count === 0),
          code: "settings-restored-close-failed",
          message: "restored clean Preferences did not close exactly",
        });
        const restoredCloseCommand = await waitForCommands(secondCommands, restoredCloseStart, [{ command: "close_overlay", args: {} }], "restored Preferences close command");
        state.acceptedLedger = structuredClone(provider.requestLedger);
        await sink.record("settings-preferences-restored", {
          restart: restarted.restart,
          surface: restored.value.surface,
          stable_for_ms: SETTINGS_RESTORE_STABILITY_MS,
          accepted_provider_docling_ledger: state.acceptedLedger,
          close_command: restoredCloseCommand,
          screenshot: restoredScreenshot,
        }, { phase: "executing", owner: OWNER });
        secondSettled = true;
        await settleGenerationResources(state, secondInput, secondCommands, 2, null);
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        if (!firstSettled) {
          firstSettled = true;
          try { await settleGenerationResources(state, firstInput, firstCommands, 1, primaryError); }
          catch (error) { if (primaryError === null) throw error; }
        }
        if (secondInput !== null && secondCommands !== null && !secondSettled) {
          secondSettled = true;
          try { await settleGenerationResources(state, secondInput, secondCommands, 2, primaryError); }
          catch (error) { if (primaryError === null) throw error; }
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
      const resourcesPass = state.generationResources.every((resource) => resource.failures.length === 0);
      const dragPass = state.drag === null
        || (state.drag.button_release_verified === true && state.drag.cursor_restore_succeeded === true);
      const quiescePass = state.quiesceOutcome?.input === "pass";
      return {
        input: resourcesPass && dragPass && quiescePass ? "pass" : "fail",
        resources: [{
          kind: "settings-preferences-verification",
          generation_resources: state.generationResources,
          native_drag_release_verified: state.drag?.button_release_verified ?? null,
          native_cursor_restore_succeeded: state.drag?.cursor_restore_succeeded ?? null,
          quiesce_input: state.quiesceOutcome?.input ?? null,
        }],
      };
    },
  });
}

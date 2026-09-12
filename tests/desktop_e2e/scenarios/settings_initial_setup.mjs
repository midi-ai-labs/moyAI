import crypto from "node:crypto";
import path from "node:path";
import { readFile, stat, writeFile } from "node:fs/promises";

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
  captureOwnedWindowPng,
  closeOwnedWindowForCleanup,
  probeExactOwnedWindow,
  selectFileInOwnedNativeDialog,
  selectFreshOwnedRootWindow,
  snapshotOwnedTopLevelWindows,
} from "../drivers/windows_native_input.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";
import { quiesceProviderResource } from "./provider_restart.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";

const OWNER = "scenario:settings.initial-setup";
export const INITIAL_SETUP_STEPS = Object.freeze([
  "start",
  "provider",
  "model",
  "permissions",
  "tools",
  "finish",
]);
export const INITIAL_SETUP_RESTART_STABILITY_MS = 500;
export const INITIAL_SETUP_PROVIDER_PROFILE = "openai_responses";
export const INITIAL_SETUP_PROVIDER_API_KEY_ENV = "MOYAI_E2E_UNUSED_API_KEY";
export const INITIAL_SETUP_IMPORT_GENERATION = "1";
export const INITIAL_SETUP_SECRET_SENTINEL = "E2E_INITIAL_SETUP_SECRET_DO_NOT_EXPOSE_7F2D9C";
export const INITIAL_SETUP_PROVIDER_PROFILE_OPTIONS = Object.freeze([
  "lm_studio",
  "openai_compatible",
  "openai_responses",
  "lm_studio_chat_completions",
]);
export const INITIAL_SETUP_HOST_OWNED_CONFIG_KEYS = Object.freeze([
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

const NEXT = Object.freeze({
  selector: '[data-surface="initial-setup"] button[data-action="initial-setup-next"]',
  identity: { tag: "BUTTON", action: "initial-setup-next" },
});
const FINISH = Object.freeze({
  selector: '[data-surface="initial-setup"] button[data-action="finish-initial-setup"]',
  identity: { tag: "BUTTON", action: "finish-initial-setup" },
});
const IMPORT_CONFIG = Object.freeze({
  selector: '[data-surface="initial-setup"] button[data-action="import-config-toml"]',
  identity: { tag: "BUTTON", action: "import-config-toml" },
});
const MODEL_ADVANCED = Object.freeze({
  selector: '[data-surface="initial-setup"] details[data-details-key="initial-setup-model-advanced"] > summary',
  identity: { tag: "DETAILS", detailsKey: "initial-setup-model-advanced" },
});
const NATIVE_FILE_DIALOG_CLASS = "#32770";

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

function configValues(projection, overrides = {}) {
  if (!Array.isArray(projection?.config_fields)) {
    throw new TypeError("Initial Setup command expectation requires projected config fields");
  }
  const projectedKeys = new Set(projection.config_fields.map((field) => field.key));
  const unknownKeys = Object.keys(overrides).filter((key) => !projectedKeys.has(key));
  if (unknownKeys.length > 0) {
    throw new TypeError(`Initial Setup import overrides contain unknown fields: ${unknownKeys.join(", ")}`);
  }
  return projection.config_fields.map((field) => ({
    key: field.key,
    text: Object.hasOwn(overrides, field.key) ? overrides[field.key] : field.value,
  }));
}

export function initialSetupImportConfig(baseUrl, secret = INITIAL_SETUP_SECRET_SENTINEL) {
  return `[model]
base_url = ${JSON.stringify(baseUrl)}
model = ${JSON.stringify(SCRIPTED_PROVIDER_MODEL_ID)}
provider_profile = ${JSON.stringify(INITIAL_SETUP_PROVIDER_PROFILE)}
api_key_env = ${JSON.stringify(INITIAL_SETUP_PROVIDER_API_KEY_ENV)}
extra_headers = { Authorization = ${JSON.stringify(`Bearer ${secret}`)} }
connect_timeout_ms = 10000
request_timeout_ms = 120000
max_retries = 0
context_window = 65536
supports_tools = true
supports_images = false
parallel_tool_calls = false
max_parallel_predictions = 1

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
timeout_ms = 120000

[mcp]
enabled = false
`;
}

export function initialSetupImportedPublicOverrides(baseUrl) {
  return Object.freeze({
    "model.base_url": baseUrl,
    "model.model": SCRIPTED_PROVIDER_MODEL_ID,
    "model.provider_profile": INITIAL_SETUP_PROVIDER_PROFILE,
    "model.api_key_env": INITIAL_SETUP_PROVIDER_API_KEY_ENV,
    "model.extra_headers_json": "",
    "model.connect_timeout_ms": "10000",
    "model.request_timeout_ms": "120000",
    "model.max_retries": "0",
    "model.context_window": "65536",
    "model.supports_tools": "true",
    "model.supports_images": "false",
    "model.parallel_tool_calls": "false",
    "model.max_parallel_predictions": "1",
    "permissions.access_mode": "default",
    "multi_agent.enabled": "false",
    "multi_agent.mode": "explicit_request_only",
    "multi_agent.max_concurrent_agents": "2",
    "multi_agent.max_concurrent_model_requests": "1",
    "docling.enabled": "false",
    "docling.base_url": baseUrl,
    "docling.timeout_ms": "120000",
    "mcp.enabled": "false",
  });
}

export function expectedInitialSetupFinishCommand(surface, importGeneration = null, valueOverrides = {}) {
  const projection = surface?.projection;
  const expectedConfigTarget = projection?.config_target;
  const expectedSetupTarget = projection?.startup?.setup_target;
  if (!expectedConfigTarget || !expectedSetupTarget) {
    throw new TypeError("Initial Setup finish expectation requires both exact mutation targets");
  }
  return {
    command: "finish_initial_setup",
    args: {
      values: configValues(projection, valueOverrides),
      expectedConfigTarget: structuredClone(expectedConfigTarget),
      expectedSetupTarget: structuredClone(expectedSetupTarget),
      importGeneration,
    },
  };
}

export async function observeInitialSetupSurface(cdp) {
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
    const rows = (selector) => Array.from(document.querySelectorAll(selector));
    const one = (selector) => {
      const matches = rows(selector);
      const node = matches.length === 1 ? matches[0] : null;
      return { count: matches.length, visible: visible(node), node };
    };
    const button = (selector) => {
      const found = one(selector);
      const rect = found.node?.getBoundingClientRect();
      const labelRange = document.createRange();
      if (found.node) labelRange.selectNodeContents(found.node);
      const labelRects = found.node ? Array.from(labelRange.getClientRects()) : [];
      return {
        count: found.count,
        visible: found.visible,
        label_line_count: labelRects.length,
        label_contained: Boolean(rect) && labelRects.length > 0 && labelRects.every(label =>
          label.left >= rect.left - 1 && label.right <= rect.right + 1
          && label.top >= rect.top - 1 && label.bottom <= rect.bottom + 1),
        enabled: found.node instanceof HTMLButtonElement
          && !found.node.disabled
          && found.node.getAttribute('aria-disabled') !== 'true',
      };
    };
    const input = (selector) => {
      const found = one(selector);
      return {
        count: found.count,
        visible: found.visible,
        enabled: found.node instanceof HTMLInputElement && !found.node.disabled,
        value: found.node instanceof HTMLInputElement ? found.node.value : null,
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
    const wizard = one('[data-surface="initial-setup"]');
    const importHelp = one('[data-surface="initial-setup"] #initial-setup-import-help[data-settings-passive="initial-setup-import-source"]');
    const sensitiveExtraHeaders = one('[data-surface="initial-setup"] textarea.settings-control[data-config-key="model.extra_headers_json"][data-sensitive-config="true"]');
    const sensitiveStatus = sensitiveExtraHeaders.node instanceof HTMLElement
      ? sensitiveExtraHeaders.node.closest('.settings-field')?.querySelector('.settings-sensitive-status') ?? null
      : null;
    const rect = wizard.node instanceof HTMLElement ? wizard.node.getBoundingClientRect() : null;
    const hostOwnedConfigKeyCounts = Object.fromEntries(${JSON.stringify(INITIAL_SETUP_HOST_OWNED_CONFIG_KEYS)}.map((key) => [
      key,
      rows('[data-surface="initial-setup"] .settings-control[data-config-key="' + key + '"]').length,
    ]));
    const stepRows = rows('[data-surface="initial-setup"] [data-step]').map((node) => ({
      step: node instanceof HTMLElement ? (node.dataset.step ?? null) : null,
      visible: visible(node),
      current: node.getAttribute('aria-current'),
    }));
    return {
      projection,
      wizard: {
        count: wizard.count,
        visible: wizard.visible,
        current_step: wizard.node instanceof HTMLElement ? (wizard.node.dataset.currentStep ?? null) : null,
        rect: rect === null ? null : {
          left: rect.left,
          top: rect.top,
          width: rect.width,
          height: rect.height,
        },
        step_rows: stepRows,
        next: button('[data-surface="initial-setup"] button[data-action="initial-setup-next"]'),
        back: button('[data-surface="initial-setup"] button[data-action="initial-setup-back"]'),
        finish: button('[data-surface="initial-setup"] button[data-action="finish-initial-setup"]'),
        import_config: button('[data-surface="initial-setup"] button[data-action="import-config-toml"]'),
        import_source: {
          count: importHelp.count,
          visible: importHelp.visible,
          text: importHelp.node instanceof HTMLElement ? importHelp.node.innerText.trim() : null,
        },
        provider: {
          profile: select('[data-surface="initial-setup"] .settings-control[data-config-key="model.provider_profile"]'),
          api_key_env: input('[data-surface="initial-setup"] .settings-control[data-config-key="model.api_key_env"]'),
        },
        host_owned_config_key_counts: hostOwnedConfigKeyCounts,
        sensitive_extra_headers: {
          count: sensitiveExtraHeaders.count,
          visible: sensitiveExtraHeaders.visible,
          value: sensitiveExtraHeaders.node instanceof HTMLTextAreaElement ? sensitiveExtraHeaders.node.value : null,
          configured: sensitiveExtraHeaders.node instanceof HTMLElement ? sensitiveExtraHeaders.node.dataset.sensitiveConfigured ?? null : null,
          placeholder: sensitiveExtraHeaders.node instanceof HTMLTextAreaElement ? sensitiveExtraHeaders.node.getAttribute('placeholder') : null,
          status_text: sensitiveStatus instanceof HTMLElement ? sensitiveStatus.innerText.trim() : null,
          status_visible: visible(sensitiveStatus),
        },
      },
      secret_exposure: {
        projection: JSON.stringify(projection).includes(${JSON.stringify(INITIAL_SETUP_SECRET_SENTINEL)}),
        dom: document.documentElement.innerHTML.includes(${JSON.stringify(INITIAL_SETUP_SECRET_SENTINEL)}),
      },
      viewport: { width: document.documentElement.clientWidth, height: document.documentElement.clientHeight },
      visible_shell_count: rows('.app-frame > .shell').filter(visible).length,
      visible_dialog_count: rows('[role="dialog"], [role="alertdialog"], [data-modal]').filter(visible).length,
      visible_backdrop_count: rows('.modal-backdrop').filter(visible).length,
      visible_fatal_count: rows('.fatal').filter(visible).length,
      visible_recoverable_error_count: rows('.ui-error-notice').filter(visible).length,
      visible_validation_error_count: rows('.validation.error').filter(visible).length,
    };
  })()`);
}

function errorFree(surface) {
  return surface?.visible_fatal_count === 0
    && surface?.visible_recoverable_error_count === 0
    && surface?.visible_validation_error_count === 0;
}

export function initialSetupStepReady(surface, ledger, expectedStep, expectedWorkspace = null) {
  const target = surface?.projection?.startup?.setup_target;
  const rect = surface?.wizard?.rect;
  const viewport = surface?.viewport;
  const primary = expectedStep === "finish" ? surface?.wizard?.finish : surface?.wizard?.next;
  const providerFieldsReady = expectedStep !== "provider" || (
    surface?.wizard?.provider?.profile?.count === 1
    && surface.wizard.provider.profile.visible === true
    && surface.wizard.provider.profile.enabled === true
    && surface.wizard.provider.profile.value === INITIAL_SETUP_PROVIDER_PROFILE
    && sameValue(surface.wizard.provider.profile.options, INITIAL_SETUP_PROVIDER_PROFILE_OPTIONS)
    && surface.wizard.provider.api_key_env.count === 1
    && surface.wizard.provider.api_key_env.visible === true
    && surface.wizard.provider.api_key_env.enabled === true
    && surface.wizard.provider.api_key_env.value === INITIAL_SETUP_PROVIDER_API_KEY_ENV
  );
  return INITIAL_SETUP_STEPS.includes(expectedStep)
    && Array.isArray(ledger)
    && ledger.length === 0
    && errorFree(surface)
    && surface?.projection?.overlay === "initial_setup"
    && surface?.projection?.startup?.initial_setup_required === true
    && surface.projection.startup.initial_setup_reason === "config_missing"
    && surface.projection.startup.action_overlay === "initial_setup"
    && target !== null
    && typeof target?.globalConfigPath === "string"
    && target.globalConfigPath.length > 0
    && /^\d+$/.test(target?.setupGeneration ?? "")
    && (expectedWorkspace === null || target.workspacePath === expectedWorkspace)
    && surface?.wizard?.count === 1
    && surface.wizard.visible === true
    && surface.wizard.current_step === expectedStep
    && sameValue(surface.wizard.step_rows.map((row) => row.step), INITIAL_SETUP_STEPS)
    && surface.wizard.step_rows.filter((row) => row.current === "step").length <= 1
    && Number.isFinite(rect?.left)
    && Math.abs(rect.left) <= 1
    && Number.isFinite(rect?.top)
    && rect.top >= 0
    && rect.top <= 64
    && Number.isFinite(rect?.width)
    && rect.width >= viewport?.width - 2
    && Number.isFinite(rect?.height)
    && rect.top + rect.height >= viewport?.height - 2
    && surface.visible_shell_count === 0
    && surface.visible_dialog_count === 0
    && surface.visible_backdrop_count === 0
    && primary?.count === 1 && primary.visible && primary.enabled
    && primary.label_contained === true && primary.label_line_count === 1
    && (expectedStep !== "start"
      || (surface.wizard.import_config.count === 1 && surface.wizard.import_config.visible))
    && INITIAL_SETUP_HOST_OWNED_CONFIG_KEYS.every((key) => (
      surface.wizard.host_owned_config_key_counts?.[key] === 0
    ))
    && providerFieldsReady;
}

export function createStableInitialSetupClosedDecision({
  expectedWorkspace,
  minimumStableMs = INITIAL_SETUP_RESTART_STABILITY_MS,
  now = () => Date.now(),
} = {}) {
  if (!Number.isFinite(minimumStableMs) || minimumStableMs <= 0) {
    throw new TypeError("minimum stable duration must be positive");
  }
  let acceptedSince = null;
  return ({ surface, ledger }) => {
    const accepted = Array.isArray(ledger)
      && ledger.length === 0
      && errorFree(surface)
      && surface?.projection?.workspace_path === expectedWorkspace
      && surface?.projection?.startup?.status === "ready"
      && surface.projection.startup.initial_setup_required === false
      && surface.projection.startup.setup_target === null
      && surface.projection.startup.action_overlay === "none"
      && surface?.projection?.overlay === "none"
      && surface?.secret_exposure?.projection === false
      && surface?.secret_exposure?.dom === false
      && surface?.wizard?.count === 0
      && surface?.visible_shell_count === 1
      && surface?.visible_dialog_count === 0;
    if (!accepted) {
      acceptedSince = null;
      if (!Array.isArray(ledger) || ledger.length > 0 || (surface && !errorFree(surface))) return "fail";
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

export function importedSecretStagedReady(surface, sourcePath) {
  const matches = Array.isArray(surface?.projection?.config_fields)
    ? surface.projection.config_fields.filter((field) => field?.key === "model.extra_headers_json")
    : [];
  const sensitive = matches.length === 1 ? matches[0] : null;
  return errorFree(surface)
    && surface?.projection?.overlay === "initial_setup"
    && surface?.wizard?.current_step === "start"
    && surface?.wizard?.import_source?.count === 1
    && surface.wizard.import_source.visible === true
    && typeof surface.wizard.import_source.text === "string"
    && surface.wizard.import_source.text.includes(sourcePath)
    && sensitive?.sensitive === true
    && sensitive?.configured === false
    && sensitive?.value === ""
    && surface?.secret_exposure?.projection === false
    && surface?.secret_exposure?.dom === false;
}

export function importedSecretEditorReady(surface) {
  const field = surface?.wizard?.sensitive_extra_headers;
  return surface?.wizard?.current_step === "model"
    && field?.count === 1
    && field.visible === true
    && field.value === ""
    && field.configured === "true"
    && field.placeholder === "設定済み（値は非表示）"
    && field.status_text === "設定済み・値は非表示"
    && field.status_visible === true
    && surface?.secret_exposure?.projection === false
    && surface?.secret_exposure?.dom === false;
}

function nativeOwner(context, runtime) {
  return {
    executionRoot: context.root,
    ownerPath: runtime.desktop_owner_path,
    expectedOwner: runtime.desktop_owner,
  };
}

async function waitForFreshFileDialog(owner, before, expectedOwner) {
  return waitForObservation({
    label: "Initial Setup exact native TOML file dialog",
    timeoutMs: 30_000,
    pollMs: 100,
    retrySampleErrors: false,
    sample: async () => {
      const after = await snapshotOwnedTopLevelWindows(owner);
      try {
        const candidate = selectFreshOwnedRootWindow(before, after, expectedOwner, {
          expectedClassName: NATIVE_FILE_DIALOG_CLASS,
        });
        return { acquired: true, after, candidate };
      } catch (error) {
        if (error?.code === "native-window-cardinality" && error?.evidence?.fresh_windows?.length === 0) {
          return { acquired: false, after, candidate: null };
        }
        throw error;
      }
    },
    accept: (value) => value.acquired === true,
  });
}

async function waitForNativeWindowDestroyed(owner, candidate) {
  return waitForObservation({
    label: `Initial Setup native file dialog ${candidate.hwnd} destroyed`,
    timeoutMs: 10_000,
    pollMs: 100,
    retrySampleErrors: false,
    sample: () => probeExactOwnedWindow({ ...owner, candidate }),
    accept: (value) => value.live === false,
  });
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

async function waitForStep(cdp, provider, context, step) {
  return waitForObservation({
    label: `Initial Setup ${step} step`,
    timeoutMs: 10_000,
    pollMs: 75,
    retrySampleErrors: false,
    sample: async () => ({ surface: await observeInitialSetupSurface(cdp), ledger: provider.requestLedger }),
    accept: ({ surface, ledger }) => initialSetupStepReady(surface, ledger, step, context.paths.workspace),
  });
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

async function settleResources(state, input, commandProbe, generation, primaryError) {
  const outcome = { generation, input: null, command_probe: null, failures: [] };
  try { outcome.input = await input.cleanup(); }
  catch (error) { outcome.failures.push({ owner: "webview-input", error: errorObservation(error) }); }
  try { outcome.command_probe = await commandProbe.remove(); }
  catch (error) { outcome.failures.push({ owner: "desktop-command-probe", error: errorObservation(error) }); }
  state.resources.push(outcome);
  if (outcome.failures.length > 0 && primaryError === null) {
    throw new DesktopE2eError(
      "harness",
      "initial-setup-resource-cleanup-failed",
      "Initial Setup input and command probes did not settle",
      outcome,
    );
  }
}

export function createSettingsInitialSetupScenario() {
  const state = {
    provider: null,
    importPath: null,
    nativeOwner: null,
    nativeCandidate: null,
    acceptedLedger: null,
    quiesceOutcome: null,
    resources: [],
  };
  return Object.freeze({
    id: "settings.initial-setup",
    productOracle: "pass",
    manualGate: "not_required",
    databaseRequired: true,
    requestGracefulExit,
    get environment() {
      if (state.provider === null) return {};
      return {
        MOYAI_BASE_URL: state.provider.baseUrl,
        MOYAI_MODEL: SCRIPTED_PROVIDER_MODEL_ID,
        MOYAI_PROVIDER_PROFILE: INITIAL_SETUP_PROVIDER_PROFILE,
        MOYAI_CONTEXT_WINDOW: "65536",
        MOYAI_DOCLING_ENABLED: "true",
        MOYAI_DOCLING_BASE_URL: state.provider.baseUrl,
      };
    },
    async prepare({ context, sink, phase }) {
      state.provider = await startScriptedProvider({ expectedPrompt: "initial-setup-network-must-remain-unused" });
      await prepareDesktopFixture({
        context,
        sink,
        phase,
        owner: OWNER,
        configMode: "absent",
        sentinelName: "E2E_INITIAL_SETUP.txt",
        sentinelText: "moyAI Desktop E2E missing-config Initial Setup fixture.\n",
      });
      state.importPath = path.join(context.paths.workspace, "E2E_INITIAL_SETUP_IMPORT.toml");
      await writeFile(
        state.importPath,
        initialSetupImportConfig(state.provider.baseUrl),
        { encoding: "utf8", flag: "wx" },
      );
      await sink.record("initial-setup-zero-network-fixture", {
        provider: state.provider.resourceObservation(),
        environment: Object.keys(this.environment).sort(),
        import_source_path: state.importPath,
      }, { phase, owner: OWNER });
    },
    async execute({ context, runtime, driver: firstCdp, host, sink }) {
      const provider = state.provider;
      if (provider === null) throw new Error("Initial Setup scripted provider was not prepared");
      if (state.importPath === null) throw new Error("Initial Setup import fixture was not prepared");
      state.nativeOwner = nativeOwner(context, runtime);
      await firstCdp.call("Runtime.enable");
      await firstCdp.call("DOM.enable");
      await firstCdp.call("Accessibility.enable");
      const input = new WebviewInput(firstCdp, { probeId: "settings-initial-setup-g1" });
      const commands = new DesktopCommandProbe(firstCdp, {
        probeId: "settings-initial-setup-g1",
        commands: ["load_initial_setup_config_toml", "finish_initial_setup"],
      });
      let settled = false;
      let primaryError = null;
      let nativeCleanupError = null;
      try {
        await input.installProbe();
        await commands.install();
        const observations = [];
        const initialStart = (await waitForStep(firstCdp, provider, context, "start")).value.surface;
        observations.push(initialStart);

        const nativeBefore = await snapshotOwnedTopLevelWindows(state.nativeOwner);
        const importCommandStart = (await commands.snapshot()).sequence;
        const importActivation = await trustedClick(input, IMPORT_CONFIG);
        const acquired = await waitForFreshFileDialog(state.nativeOwner, nativeBefore, runtime.desktop_owner);
        state.nativeCandidate = acquired.value.candidate;
        const nativeSelection = await selectFileInOwnedNativeDialog({
          ...state.nativeOwner,
          candidate: state.nativeCandidate,
          selectedPath: state.importPath,
        });
        let nativeDialogScreenshot = null;
        const nativeDialogLive = await probeExactOwnedWindow({
          ...state.nativeOwner,
          candidate: state.nativeCandidate,
        });
        if (nativeDialogLive.live === true) {
          const nativeCapture = await captureOwnedWindowPng({
            ...state.nativeOwner,
            candidate: state.nativeCandidate,
          });
          if (nativeCapture.available) {
            nativeDialogScreenshot = await sink.writeBytes(
              "screenshots/initial-setup-file-dialog-after-selection.png",
              nativeCapture.bytes,
            );
          }
        }
        const nativeDestroyed = await waitForNativeWindowDestroyed(state.nativeOwner, state.nativeCandidate);
        state.nativeCandidate = null;
        const imported = await waitForObservation({
          label: "Initial Setup imported secret remains staged outside the persisted projection",
          timeoutMs: 30_000,
          pollMs: 75,
          retrySampleErrors: false,
          sample: async () => ({ surface: await observeInitialSetupSurface(firstCdp), ledger: provider.requestLedger }),
          accept: ({ surface, ledger }) => Array.isArray(ledger)
            && ledger.length === 0
            && importedSecretStagedReady(surface, state.importPath),
        });
        const importCommand = await waitForCommands(commands, importCommandStart, [{
          command: "load_initial_setup_config_toml",
          args: {
            expectedConfigTarget: structuredClone(initialStart.projection.config_target),
            expectedSetupTarget: structuredClone(initialStart.projection.startup.setup_target),
          },
        }], "Initial Setup exact TOML import command");
        observations.push(imported.value.surface);
        const importedScreenshot = await captureScenarioScreenshot({
          cdp: firstCdp,
          sink,
          name: "initial-setup-imported-secret-redacted",
          owner: OWNER,
        });
        await sink.record("initial-setup-secret-imported", {
          input_kind: "browser_trusted_activation_then_windows_uia_file_selection",
          activation: importActivation,
          native_before: nativeBefore,
          native_after: acquired.value.after,
          native_candidate: acquired.value.candidate,
          native_selection: nativeSelection,
          native_dialog_after_selection_screenshot: nativeDialogScreenshot,
          native_destroyed: nativeDestroyed.value,
          import_command: importCommand,
          public_sensitive_field: imported.value.surface.projection.config_fields.find((field) => field.key === "model.extra_headers_json"),
          projection_secret_exposed: imported.value.surface.secret_exposure.projection,
          dom_secret_exposed: imported.value.surface.secret_exposure.dom,
          screenshot: importedScreenshot,
        }, { phase: "executing", owner: OWNER });
        for (const step of INITIAL_SETUP_STEPS.slice(1)) {
          if (step === "finish") {
            await trustedClick(input, NEXT);
            observations.push((await waitForStep(firstCdp, provider, context, step)).value.surface);
            break;
          }
          await trustedClick(input, NEXT);
          const observed = await waitForStep(firstCdp, provider, context, step);
          observations.push(observed.value.surface);
          if (step === "model") {
            await trustedClick(input, MODEL_ADVANCED);
            const sensitiveEditor = await waitForObservation({
              label: "Initial Setup configured sensitive editor is visibly redacted",
              timeoutMs: 10_000,
              pollMs: 75,
              retrySampleErrors: false,
              sample: () => observeInitialSetupSurface(firstCdp),
              accept: importedSecretEditorReady,
            });
            observations.push(sensitiveEditor.value);
          }
          if (step === "provider") {
            const escapeStart = (await input.snapshotProbe()).sequence;
            await input.pressKey("Escape");
            const escape = assertTrustedProbeSequence(await input.snapshotProbe(escapeStart), {
              afterSequence: escapeStart,
              expected: [{ type: "keydown", identity: NEXT.identity, key: "Escape", code: "Escape" }],
            });
            const unchanged = await waitForStep(firstCdp, provider, context, "provider");
            observations.push({ ...unchanged.value.surface, escape_probe: escape });
          }
        }

        const finishSurface = observations.at(-1);
        const wizardScreenshot = await captureScenarioScreenshot({
          cdp: firstCdp,
          sink,
          name: "initial-setup-finish-review",
          owner: OWNER,
        });
        const expectedCommand = expectedInitialSetupFinishCommand(
          finishSurface,
          INITIAL_SETUP_IMPORT_GENERATION,
          initialSetupImportedPublicOverrides(provider.baseUrl),
        );
        const commandStart = (await commands.snapshot()).sequence;
        const finishActivation = await trustedClick(input, FINISH);
        const shell = await acquireInteractiveShell({ context, driver: firstCdp, sink }, {
          evidenceOwner: OWNER,
          screenshotStem: "initial-setup-finished-shell",
        });
        const command = await waitForCommands(
          commands,
          commandStart,
          [expectedCommand],
          "Initial Setup exact Finish transaction",
        );
        if (provider.requestLedger.length !== 0) {
          throw productFailure(
            "initial-setup-implicit-network",
            "Initial Setup navigation or Finish contacted the provider or Docling implicitly",
            { ledger: provider.requestLedger },
          );
        }
        const configBytes = await readFile(context.paths.config_file);
        const configStat = await stat(context.paths.config_file);
        const persistedText = configBytes.toString("utf8");
        const persistedSecretCount = persistedText.split(INITIAL_SETUP_SECRET_SENTINEL).length - 1;
        if (persistedSecretCount !== 1) {
          throw productFailure(
            "initial-setup-imported-secret-not-preserved",
            "Finish did not hydrate and persist the exact Rust-owned imported secret once",
            { persisted_secret_count: persistedSecretCount },
          );
        }
        if (JSON.stringify(command).includes(INITIAL_SETUP_SECRET_SENTINEL)
          || JSON.stringify(shell.observation).includes(INITIAL_SETUP_SECRET_SENTINEL)) {
          throw productFailure(
            "initial-setup-imported-secret-exposed",
            "the imported secret crossed a public command or Desktop projection boundary",
            { command_secret_exposed: JSON.stringify(command).includes(INITIAL_SETUP_SECRET_SENTINEL) },
          );
        }
        const finishScreenshot = await captureScenarioScreenshot({
          cdp: firstCdp,
          sink,
          name: "initial-setup-finished",
          owner: OWNER,
        });
        await sink.record("initial-setup-finished", {
          steps: observations.map((surface) => ({
            step: surface.wizard.current_step,
            projection_revision: surface.projection.projection_revision,
            setup_target: surface.projection.startup.setup_target,
            escape_probe: surface.escape_probe ?? null,
          })),
          finish_activation: finishActivation,
          command,
          shell: shell.observation,
          config: {
            path: context.paths.config_file,
            size_bytes: configStat.size,
            sha256: crypto.createHash("sha256").update(configBytes).digest("hex"),
            imported_secret_preserved_once: persistedSecretCount === 1,
          },
          provider_docling_ledger: provider.requestLedger,
          screenshots: {
            wizard_finish_review: wizardScreenshot,
            finished_shell: finishScreenshot,
          },
        }, { phase: "executing", owner: OWNER });

        settled = true;
        await settleResources(state, input, commands, 1, null);
        const restarted = await host.restart({ context, scenario: this, sink, driver: firstCdp, phase: "executing" });
        await acquireInteractiveShell({ context, driver: restarted.driver, sink }, {
          evidenceOwner: OWNER,
          screenshotStem: "initial-setup-restarted-shell",
        });
        const stableDecision = createStableInitialSetupClosedDecision({ expectedWorkspace: context.paths.workspace });
        const restored = await waitForObservation({
          label: "Initial Setup remains completed after exact restart",
          timeoutMs: 30_000,
          pollMs: 75,
          retrySampleErrors: false,
          sample: async () => ({
            surface: await observeInitialSetupSurface(restarted.driver),
            ledger: provider.requestLedger,
          }),
          accept: (sample) => stableDecision(sample) === "pass",
        });
        const restartScreenshot = await captureScenarioScreenshot({
          cdp: restarted.driver,
          sink,
          name: "initial-setup-not-shown-after-restart",
          owner: OWNER,
        });
        const restoredSensitive = restored.value.surface.projection.config_fields.filter(
          (field) => field?.key === "model.extra_headers_json",
        );
        if (restoredSensitive.length !== 1
          || restoredSensitive[0].sensitive !== true
          || restoredSensitive[0].configured !== true
          || restoredSensitive[0].value !== "") {
          throw productFailure(
            "initial-setup-restarted-secret-projection-mismatch",
            "restart did not preserve the configured-but-redacted sensitive field contract",
            { public_sensitive_fields: restoredSensitive },
          );
        }
        state.acceptedLedger = structuredClone(provider.requestLedger);
        await sink.record("initial-setup-restart-persistence", {
          restart: restarted.restart,
          surface: restored.value.surface,
          stable_for_ms: INITIAL_SETUP_RESTART_STABILITY_MS,
          accepted_provider_docling_ledger: state.acceptedLedger,
          screenshot: restartScreenshot,
        }, { phase: "executing", owner: OWNER });
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) {
        primaryError = error;
        throw error;
      } finally {
        if (state.nativeCandidate !== null && state.nativeOwner !== null) {
          try {
            const live = await probeExactOwnedWindow({
              ...state.nativeOwner,
              candidate: state.nativeCandidate,
            });
            if (live.live === true) {
              const close = await closeOwnedWindowForCleanup({
                ...state.nativeOwner,
                candidate: state.nativeCandidate,
              });
              await sink.record("initial-setup-native-dialog-cleanup", {
                candidate: state.nativeCandidate,
                close,
              }, { phase: "cleaning", owner: OWNER });
            }
          } catch (cleanupError) {
            nativeCleanupError = cleanupError;
          } finally {
            state.nativeCandidate = null;
          }
        }
        if (!settled) {
          settled = true;
          try { await settleResources(state, input, commands, 1, primaryError); }
          catch (error) { if (primaryError === null) throw error; }
        }
        if (nativeCleanupError !== null && primaryError === null) {
          throw new DesktopE2eError(
            "harness",
            "initial-setup-native-dialog-cleanup-failed",
            "Initial Setup native file dialog did not settle",
            errorObservation(nativeCleanupError),
          );
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
      const resourcesPass = state.resources.every((resource) => resource.failures.length === 0);
      const quiescePass = state.quiesceOutcome?.input === "pass";
      return {
        input: resourcesPass && quiescePass ? "pass" : "fail",
        resources: [{
          kind: "settings-initial-setup-verification",
          generation_resources: state.resources,
          quiesce_input: state.quiesceOutcome?.input ?? null,
        }],
      };
    },
  });
}

import crypto from "node:crypto";
import { readFile, stat } from "node:fs/promises";

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
export const INITIAL_SETUP_PROVIDER_API_KEY_ENV = "";
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

function configValues(projection) {
  if (!Array.isArray(projection?.config_fields)) {
    throw new TypeError("Initial Setup command expectation requires projected config fields");
  }
  return projection.config_fields.map((field) => ({ key: field.key, text: field.value }));
}

function configFieldValue(projection, key) {
  const matches = Array.isArray(projection?.config_fields)
    ? projection.config_fields.filter((field) => field?.key === key)
    : [];
  return matches.length === 1 ? matches[0].value : null;
}

export function expectedInitialSetupFinishCommand(surface) {
  const projection = surface?.projection;
  const expectedConfigTarget = projection?.config_target;
  const expectedSetupTarget = projection?.startup?.setup_target;
  if (!expectedConfigTarget || !expectedSetupTarget) {
    throw new TypeError("Initial Setup finish expectation requires both exact mutation targets");
  }
  return {
    command: "finish_initial_setup",
    args: {
      values: configValues(projection),
      expectedConfigTarget: structuredClone(expectedConfigTarget),
      expectedSetupTarget: structuredClone(expectedSetupTarget),
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
      return {
        count: found.count,
        visible: found.visible,
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
        provider: {
          profile: select('[data-surface="initial-setup"] .settings-control[data-config-key="model.provider_profile"]'),
          api_key_env: input('[data-surface="initial-setup"] .settings-control[data-config-key="model.api_key_env"]'),
        },
        host_owned_config_key_counts: hostOwnedConfigKeyCounts,
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
    && configFieldValue(surface.projection, "model.provider_profile") === INITIAL_SETUP_PROVIDER_PROFILE
    && configFieldValue(surface.projection, "model.api_key_env") === INITIAL_SETUP_PROVIDER_API_KEY_ENV
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
    && (expectedStep === "finish"
      ? surface.wizard.finish.count === 1 && surface.wizard.finish.visible && surface.wizard.finish.enabled
      : surface.wizard.next.count === 1 && surface.wizard.next.visible && surface.wizard.next.enabled)
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
      await sink.record("initial-setup-zero-network-fixture", {
        provider: state.provider.resourceObservation(),
        environment: Object.keys(this.environment).sort(),
      }, { phase, owner: OWNER });
    },
    async execute({ context, driver: firstCdp, host, sink }) {
      const provider = state.provider;
      if (provider === null) throw new Error("Initial Setup scripted provider was not prepared");
      await firstCdp.call("Runtime.enable");
      await firstCdp.call("DOM.enable");
      await firstCdp.call("Accessibility.enable");
      const input = new WebviewInput(firstCdp, { probeId: "settings-initial-setup-g1" });
      const commands = new DesktopCommandProbe(firstCdp, {
        probeId: "settings-initial-setup-g1",
        commands: ["finish_initial_setup"],
      });
      let settled = false;
      let primaryError = null;
      try {
        await input.installProbe();
        await commands.install();
        const observations = [];
        observations.push((await waitForStep(firstCdp, provider, context, "start")).value.surface);
        for (const step of INITIAL_SETUP_STEPS.slice(1)) {
          if (step === "finish") {
            await trustedClick(input, NEXT);
            observations.push((await waitForStep(firstCdp, provider, context, step)).value.surface);
            break;
          }
          await trustedClick(input, NEXT);
          const observed = await waitForStep(firstCdp, provider, context, step);
          observations.push(observed.value.surface);
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
        const expectedCommand = expectedInitialSetupFinishCommand(finishSurface);
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
        if (!settled) {
          settled = true;
          try { await settleResources(state, input, commands, 1, primaryError); }
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

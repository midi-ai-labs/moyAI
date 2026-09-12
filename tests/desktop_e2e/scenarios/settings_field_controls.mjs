import { readFile } from "node:fs/promises";
import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { DesktopCommandProbe } from "../drivers/desktop_command_probe.mjs";
import { WebviewInput, assertTrustedProbeSequence, assertTrustedTextInsertion } from "../drivers/webview_input.mjs";
import { startScriptedProvider } from "../drivers/scripted_provider.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import { quiesceProviderResource } from "./provider_restart.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";
import { settingsPreferencesFixtureConfig, trustedClickProbeEvents, trustedToggleFocusedSettingsCheckbox } from "./settings_preferences.mjs";
import { SETTINGS_CONTROL_PLAN, SESSION_CONTROL_PLAN, assertSettingsInventory, controlValidValue, controlInvalidValues, publicFieldMatches } from "./settings_control_plan.mjs";

const GLOBAL = '[role="dialog"][aria-labelledby="config-dialog-title"]';
const SESSION = '[data-modal="session-settings"]';
const INITIAL = '[data-surface="initial-setup"]';
const SCOPE = { global: GLOBAL, session: SESSION, initial: INITIAL };
const SHOW_GLOBAL = { selector: 'aside.sidebar button.settings[data-action="show-config"][title="設定"]', identity: { tag: "BUTTON", action: "show-config" } };
const SHOW_SESSION = { selector: 'button[data-action="show-session-settings"][data-session-settings-trigger="model"]', identity: { tag: "BUTTON", action: "show-session-settings", sessionSettingsTrigger: "model" } };
const ROOT_PROMPT = "create settings audit root";
const ROOT_RESPONSE = "SETTINGS_AUDIT_READY";
function failure(code, message, evidence) { return new DesktopE2eError("product", code, message, evidence); }
function action(scope, name) { return { selector: `${SCOPE[scope]} button[data-action="${name}"]`, identity: { tag: "BUTTON", action: name } }; }

export async function observeFieldControls(cdp, scope = "global") {
  return cdp.evaluate(`(async () => {
    const projection = await window.__TAURI_INTERNALS__.invoke('desktop_state');
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    const panel = document.querySelector(${JSON.stringify(SCOPE[scope])});
    const visible = el => Boolean(el?.isConnected && el.getBoundingClientRect().width > 0 && el.getBoundingClientRect().height > 0 && getComputedStyle(el).visibility !== 'hidden');
    return { projection, panel: visible(panel), step: panel?.dataset.currentStep ?? null,
      dirty: panel?.querySelector('.dirty-badge')?.classList.contains('visible') ?? false,
      controls: [...(panel?.querySelectorAll('input.settings-control, textarea.settings-control, select.settings-control, .session-settings-control') ?? [])].map(el => ({
        key: el.dataset.configKey ?? el.dataset.sessionSetting, tag: el.tagName, id: el.id, type: el.type,
        value: el.type === 'checkbox' ? String(el.checked) : el.value, disabled: el.disabled,
        invalid: el.getAttribute('aria-invalid') === 'true', active: document.activeElement === el,
        options: el.tagName === 'SELECT' ? [...el.options].map(option => option.value) : [],
        sensitiveConfigured: el.dataset.sensitiveConfigured ?? null,
        closedDetails: [...(function*(){ for(let node=el.parentElement; node; node=node.parentElement) if(node.tagName==='DETAILS' && !node.open) yield node.dataset.detailsKey; })()].reverse(),
      })),
      buttons: [...(panel?.querySelectorAll('button[data-action]') ?? [])].map(el => ({ action: el.dataset.action, disabled: el.disabled, visible: visible(el) })),
      validation: panel?.querySelector('#settings-validation, #session-settings-status')?.textContent?.trim() ?? null,
      error_count: [...document.querySelectorAll('.fatal, .ui-error-notice')].filter(visible).length };
  })()`);
}

async function wait(cdp, scope, label, accept, timeoutMs = 15_000) {
  try { return (await waitForObservation({ label, timeoutMs, pollMs: 50, retrySampleErrors: false,
    sample: () => observeFieldControls(cdp, scope), accept })).value; }
  catch (error) {
    if (error?.code !== "observation-timeout" || error.evidence?.last_error) throw error;
    throw failure("settings-field-state-mismatch", label, error.evidence);
  }
}

function fieldControl(surface, key, modelSelect = false) {
  const found = surface.controls.filter(control => control.key === key && (modelSelect ? control.tag === "SELECT" : control.type !== "select-one" || !/^(model\.model|side_chat\.model)$/.test(key)));
  if (found.length !== 1) throw failure("settings-control-cardinality", `Expected one ${key} control`, found);
  return found[0];
}

function locatorFor(scope, control) {
  const attr = scope === "session" ? "data-session-setting" : "data-config-key";
  return { selector: `${SCOPE[scope]} ${control.tag.toLowerCase()}[${attr}="${control.key}"]${control.id ? `[id="${control.id}"]` : ""}`,
    identity: { tag: control.tag, ...(scope === "session" ? { sessionSetting: control.key } : { configKey: control.key }), ...(control.id ? { id: control.id } : {}) } };
}

async function click(input, locator) {
  const before = (await input.snapshotProbe()).sequence;
  await input.click(locator);
  return assertTrustedProbeSequence(await input.snapshotProbe(before), { afterSequence: before, expected: trustedClickProbeEvents(locator) });
}

// A bounded traversal uses ordinary Tab and exact current-owner reads. It never
// changes focus, element values, scroll or DOM via evaluate().
async function focusThroughTab(input, cdp, locator) {
  const before = (await input.snapshotProbe()).sequence;
  for (let steps = 0; steps <= 220; steps += 1) {
    const read = await cdp.evaluate(`(() => { const nodes=document.querySelectorAll(${JSON.stringify(locator.selector)}); const el=nodes.length===1?nodes[0]:null;
      const closed=el?.tagName==='SUMMARY' ? el.parentElement?.parentElement?.closest('details:not([open])') : el?.closest('details:not([open])');
      return { count:nodes.length, active:el===document.activeElement, disabled:el?.disabled ?? false, closed:Boolean(closed) }; })()`);
    if (read.count !== 1 || read.disabled || read.closed) throw failure("settings-control-unavailable", "Settings keyboard target is unavailable", { locator, read });
    if (read.active) {
      const probe = await input.snapshotProbe(before);
      if (steps) assertTrustedProbeSequence(probe, { afterSequence: before, expected: Array.from({ length: steps }, () => [{ type: "keydown", key: "Tab" }, { type: "keyup", key: "Tab" }]).flat() });
      return { steps, probe };
    }
    if (steps < 220) await input.pressKey("Tab");
  }
  throw new DesktopE2eError("harness", "settings-tab-limit", "Settings target was not reached by bounded Tab traversal", { locator });
}

async function acquireControl(input, cdp, scope, key, { modelSelect = false } = {}) {
  if (scope === "global") {
    const section = /^side_chat\./.test(key) ? "side-chat" : /^permissions\./.test(key) ? "permissions"
      : /^multi_agent\./.test(key) ? "agents" : /^(shell|docling|mcp)\./.test(key) ? "tools"
        : /^(inspection|file_guard)\./.test(key) ? "files"
          : ["model.connect_timeout_ms", "model.max_retries", "model.max_parallel_predictions", "model.extra_headers_json"].includes(key) ? "advanced"
            : ["model.base_url", "model.model", "model.system_prompt", "model.provider_profile", "model.api_key_env"].includes(key) ? "provider" : "model";
    const href = `#settings-${section}`;
    await click(input, { selector: `${GLOBAL} nav.settings-nav a[href="${href}"]`, identity: { tag: "A", href } });
  }
  let control = fieldControl(await observeFieldControls(cdp, scope), key, modelSelect);
  for (const detailsKey of control.closedDetails) {
    if (!detailsKey) throw failure("settings-details-owner-missing", "Advanced control has no stable details owner", { key });
    const summary = { selector: `${SCOPE[scope]} details[data-details-key="${detailsKey}"] > summary`, identity: { tag: "DETAILS", detailsKey } };
    await focusThroughTab(input, cdp, summary);
    await input.pressKey(" ");
  }
  control = fieldControl(await observeFieldControls(cdp, scope), key, modelSelect);
  const locator = locatorFor(scope, control);
  await focusThroughTab(input, cdp, locator);
  return { control, locator };
}

async function replace(input, cdp, scope, key, value) {
  const { locator, control } = await acquireControl(input, cdp, scope, key);
  if (control.tag === "SELECT") {
    if (!control.options.includes(value)) throw failure("settings-option-missing", "Selected option must exist in actual GUI", { key, value, options: control.options });
    const start = (await input.snapshotProbe()).sequence;
    await input.pressKey("Home");
    for (let index = 0; index < control.options.indexOf(value); index += 1) await input.pressKey("ArrowDown");
    await input.pressKey("Tab");
    const probe = await input.snapshotProbe(start);
    const events = probe.events.filter(event => ["keydown", "keyup", "input", "change"].includes(event.type));
    if (!events.length || events.some(event => !event.isTrusted)) throw failure("settings-select-input-untrusted", "Native select did not receive trusted input", { key, probe });
  } else if (control.type === "checkbox") {
    if (control.value !== value) await trustedToggleFocusedSettingsCheckbox(input, locator);
  } else {
    await input.keyDown("Control");
    try { await input.pressKey("a"); } finally { await input.keyUp("Control"); }
    if (value === "") await input.pressKey("Backspace");
    else {
      const start = (await input.snapshotProbe()).sequence;
      await input.insertText(locator, value);
      assertTrustedTextInsertion(await input.snapshotProbe(start), { afterSequence: start, identity: locator.identity, text: value });
    }
  }
  return wait(cdp, scope, `${key} displays the entered value`, surface => fieldControl(surface, key).value === value);
}

async function activate(input, cdp, scope, name) {
  const locator = action(scope, name);
  await focusThroughTab(input, cdp, locator);
  return click(input, locator);
}

function currentFields(surface) { return new Map(surface.projection.config_fields.map(field => [field.key, field])); }
function expectedFieldsMatch(surface, expected) {
  const fields = currentFields(surface);
  return [...expected].every(([key, value]) => publicFieldMatches(fields.get(key), value, SETTINGS_CONTROL_PLAN.find(row => row.key === key)));
}

function expectedEditorsMatch(surface, expected) {
  return [...expected].every(([key, value]) => {
    const row = SETTINGS_CONTROL_PLAN.find(row => row.key === key);
    const editor = fieldControl(surface, key);
    return row.sensitive ? editor.value === "" && editor.sensitiveConfigured === "true" : editor.value === value;
  });
}

async function validateAndEnter({ input, cdp, scope, row, field, value, sink, owner }) {
  for (const invalid of controlInvalidValues(row, field)) {
    await replace(input, cdp, scope, row.key, invalid);
    const invalidSurface = await wait(cdp, scope, `${row.key}: invalid input is marked`, surface => fieldControl(surface, row.key).invalid);
    // No commit is sent while an invalid editor is visible; a disabled button is
    // required on Global. Wizard/Session guards may reject at their action boundary.
    if (scope === "global" && !invalidSurface.buttons.find(button => button.action === "save-global-config")?.disabled) {
      throw failure("settings-invalid-save-enabled", "Global save must be disabled for an invalid editor", { key: row.key, invalidSurface });
    }
    await sink.record("settings-field-invalid", { scope, key: row.key, invalid, validation: invalidSurface.validation }, { phase: "executing", owner });
  }
  const surface = await replace(input, cdp, scope, row.key, value);
  if (fieldControl(surface, row.key).invalid) throw failure("settings-valid-value-rejected", "A valid fixture input remains invalid", { key: row.key, value, surface });
  await sink.record("settings-field-edited", { scope, key: row.key, value, invalid_cases: controlInvalidValues(row, field).length }, { phase: "executing", owner });
}

async function openGlobal(input, cdp) {
  await click(input, SHOW_GLOBAL);
  return wait(cdp, "global", "Global Settings opens", surface => surface.panel && surface.projection.overlay === "config");
}

async function globalSaveAndReopen(input, cdp, expected, commands, sink, owner) {
  const before = (await commands.snapshot()).sequence;
  await activate(input, cdp, "global", "save-global-config");
  const saved = await wait(cdp, "global", "Settings save commits expected fields", surface => surface.panel && expectedFieldsMatch(surface, expected) && expectedEditorsMatch(surface, expected) && !surface.dirty && surface.error_count === 0);
  const calls = (await commands.snapshot(before)).calls;
  if (calls.filter(call => call.command === "save_global_config").length !== 1) throw failure("settings-save-command-count", "One save gesture must produce one save command", calls);
  await activate(input, cdp, "global", "close-overlay");
  await wait(cdp, "global", "Clean Settings closes", surface => !surface.panel && surface.projection.overlay === "none");
  const reopened = await openGlobal(input, cdp);
  if (!expectedFieldsMatch(reopened, expected) || !expectedEditorsMatch(reopened, expected)) throw failure("settings-save-reopen-mismatch", "Reopened settings disagree with committed fields", { expected: [...expected], reopened });
  await sink.record("settings-fields-saved-reopened", { keys: [...expected.keys()], projection: saved.projection.config_fields, save_calls: calls }, { phase: "executing", owner });
}

async function exerciseGlobal({ input, cdp, commands, context, sink, owner, provider }) {
  const initial = await openGlobal(input, cdp);
  assertSettingsInventory(initial.projection.config_fields);
  const fields = currentFields(initial);
  const expected = new Map();
  // Enable Docling through its own control before editing dependent fields.
  const ordered = [...SETTINGS_CONTROL_PLAN].sort((left, right) => Number(right.key === "docling.enabled") - Number(left.key === "docling.enabled"));
  for (const row of ordered) {
    const value = controlValidValue(row, fields.get(row.key), { baseUrl: provider.baseUrl });
    await validateAndEnter({ input, cdp, scope: "global", row, field: fields.get(row.key), value, sink, owner });
    expected.set(row.key, value);
    if (["model.system_prompt", "side_chat.max_retries", "permissions.access_mode", "multi_agent.max_concurrent_model_requests", "model.extra_headers_json", "inspection.include_hidden_by_default", "file_guard.structured_document_extensions", "docling.headers_json", "mcp.servers_json"].includes(row.key)) {
      await captureScenarioScreenshot({ cdp, sink, name: `settings-global-${row.key.replaceAll(".", "-")}`, owner });
    }
  }
  await globalSaveAndReopen(input, cdp, expected, commands, sink, owner);
  for (const row of SETTINGS_CONTROL_PLAN.filter(row => row.kind === "enum" || row.kind === "boolean")) {
    const values = row.options ?? ["false", "true"];
    for (const value of values) {
      await replace(input, cdp, "global", row.key, value);
      expected.set(row.key, value);
      await globalSaveAndReopen(input, cdp, expected, commands, sink, owner);
      await sink.record("settings-option-saved-reopened", { scope: "global", key: row.key, value }, { phase: "executing", owner });
    }
  }
  const bytesBeforeDiscard = await readFile(context.paths.config_file);
  for (const row of ordered.filter(row => row.key !== "docling.enabled")) {
    const value = row.options ? row.options.find(option => option !== expected.get(row.key))
      : controlValidValue(row, fields.get(row.key), { baseUrl: provider.baseUrl, variant: 2 });
    await replace(input, cdp, "global", row.key, value);
  }
  await replace(input, cdp, "global", "docling.enabled", "false");
  await activate(input, cdp, "global", "discard-config-draft");
  const discarded = await wait(cdp, "global", "Discard restores all saved fields", surface => expectedFieldsMatch(surface, expected) && expectedEditorsMatch(surface, expected) && !surface.dirty);
  if (!(await readFile(context.paths.config_file)).equals(bytesBeforeDiscard)) throw failure("settings-discard-wrote-config", "Discard must preserve config file bytes", {});
  await sink.record("settings-all-fields-discarded", { keys: [...expected.keys()], config_bytes_unchanged: true, projection: discarded.projection.config_fields }, { phase: "executing", owner });
  await captureScenarioScreenshot({ cdp, sink, name: "settings-field-controls-saved", owner });
  await activate(input, cdp, "global", "close-overlay");
  await wait(cdp, "global", "Field audit finishes at the idle shell", surface => !surface.panel && surface.projection.overlay === "none");
}

async function exerciseInitial({ input, cdp, commands, context, sink, owner, provider }) {
  const first = await wait(cdp, "initial", "Initial Setup starts without config", surface => surface.panel && surface.step === "start");
  assertSettingsInventory(first.projection.config_fields);
  const fields = currentFields(first);
  const expected = new Map();
  const steps = ["provider", "model", "permissions", "tools", "finish"];
  await activate(input, cdp, "initial", "initial-setup-next");
  for (const step of steps) {
    await wait(cdp, "initial", `Initial Setup enters ${step}`, surface => surface.step === step);
    const rows = SETTINGS_CONTROL_PLAN.filter(row => row.initialStep === step)
      .sort((left, right) => Number(right.key === "docling.enabled") - Number(left.key === "docling.enabled"));
    for (const row of rows) {
      const value = controlValidValue(row, fields.get(row.key), { baseUrl: provider.baseUrl });
      await validateAndEnter({ input, cdp, scope: "initial", row, field: fields.get(row.key), value, sink, owner });
      const variants = row.options ?? (row.kind === "boolean" ? ["false", "true"] : []);
      for (const option of variants) {
        await replace(input, cdp, "initial", row.key, option);
        await sink.record("settings-option-selected", { scope: "initial", key: row.key, value: option, persistence: "finish transaction pending" }, { phase: "executing", owner });
      }
      expected.set(row.key, variants.length ? variants.at(-1) : value);
    }
    // Back and Next are real wizard navigation. They preserve the current step's
    // draft; unlike Global discard, the wizard intentionally has no cancel action.
    await activate(input, cdp, "initial", "initial-setup-back");
    const priorStep = steps.indexOf(step) === 0 ? "start" : steps[steps.indexOf(step) - 1];
    await wait(cdp, "initial", "Wizard Back preserves earlier owner", surface => surface.step === priorStep);
    await activate(input, cdp, "initial", "initial-setup-next");
    const revisited = await wait(cdp, "initial", `Wizard returns to ${step}`, surface => surface.step === step);
    for (const row of rows) {
      if (fieldControl(revisited, row.key).value !== expected.get(row.key)) throw failure("settings-wizard-draft-lost", "Back/Next lost an entered draft", { step, key: row.key, revisited });
    }
    await captureScenarioScreenshot({ cdp, sink, name: `settings-field-controls-initial-${step}`, owner });
    if (step !== "finish") await activate(input, cdp, "initial", "initial-setup-next");
  }
  const before = (await commands.snapshot()).sequence;
  await activate(input, cdp, "initial", "finish-initial-setup");
  await wait(cdp, "initial", "Initial Setup persists all edited fields", surface => !surface.panel && !surface.projection.startup.initial_setup_required && expectedFieldsMatch(surface, expected));
  const calls = (await commands.snapshot(before)).calls;
  if (calls.filter(call => call.command === "finish_initial_setup").length !== 1) throw failure("settings-wizard-finish-count", "Wizard Finish must commit once", calls);
  const reopened = await openGlobal(input, cdp);
  if (!expectedFieldsMatch(reopened, expected) || !expectedEditorsMatch(reopened, expected)) throw failure("settings-wizard-global-readback", "Global Settings must display the wizard's saved values", reopened);
  await sink.record("settings-initial-all-fields-saved", { keys: [...expected.keys()], saved_file_bytes: (await readFile(context.paths.config_file)).length,
    draft_options_selected: true, all_options_persisted: false, final_values_saved_reopened: true, calls }, { phase: "executing", owner });
  await activate(input, cdp, "global", "close-overlay");
}

function sessionValuesMatch(surface, expected) {
  return SESSION_CONTROL_PLAN.every(row => {
    const value = expected.get(row.key);
    return surface.projection.session_settings[row.projectionKey] === value
      && (!surface.panel || fieldControl(surface, row.key).value === value);
  });
}

async function sessionSaveAndReopen(input, cdp, expected, commands, sink, owner) {
  const before = (await commands.snapshot()).sequence;
  await activate(input, cdp, "session", "apply-session-settings");
  await wait(cdp, "session", "Session Settings applies exact fields", surface => sessionValuesMatch(surface, expected) && surface.error_count === 0);
  const calls = (await commands.snapshot(before)).calls;
  if (calls.filter(call => call.command === "apply_session_settings").length !== 1) throw failure("settings-session-apply-count", "One Session Apply must produce one command", calls);
  if ((await observeFieldControls(cdp, "session")).panel) await activate(input, cdp, "session", "close-overlay");
  await wait(cdp, "session", "Session Settings closes cleanly", surface => !surface.panel && surface.projection.overlay === "none");
  await click(input, SHOW_SESSION);
  await wait(cdp, "session", "Session Settings reopens saved fields", surface => surface.panel && sessionValuesMatch(surface, expected));
  await sink.record("settings-session-fields-saved-reopened", { values: [...expected], calls }, { phase: "executing", owner });
}

async function exerciseSession({ input, cdp, commands, context, sink, owner, provider }) {
  const prompt = { selector: "textarea#prompt", identity: { tag: "TEXTAREA", id: "prompt" } };
  await click(input, prompt);
  const start = (await input.snapshotProbe()).sequence;
  await input.insertText(prompt, ROOT_PROMPT);
  assertTrustedTextInsertion(await input.snapshotProbe(start), { afterSequence: start, identity: prompt.identity, text: ROOT_PROMPT });
  await click(input, { selector: 'button[data-action="send"]', identity: { tag: "BUTTON", action: "send" } });
  await wait(cdp, "session", "Scripted root establishes a Session Settings owner", surface => !surface.projection.busy && surface.projection.session_settings.available
    && JSON.stringify(surface.projection.transcript_rows).includes(ROOT_RESPONSE), 90_000);
  const requestCount = provider.requestLedger.length;
  if (!requestCount) throw failure("settings-session-root-request-missing", "Session setup must use its declared scripted request", {});
  await click(input, SHOW_SESSION);
  const first = await wait(cdp, "session", "Session Settings opens", surface => surface.panel);
  const globalConfigBefore = await readFile(context.paths.config_file);
  const baseline = new Map(SESSION_CONTROL_PLAN.map(row => [row.key, first.projection.session_settings[row.projectionKey]]));
  const expected = new Map();
  for (const row of SESSION_CONTROL_PLAN) {
    const field = { value: baseline.get(row.key), min_value: 1, max_value: 4294967295 };
    const value = controlValidValue(row, field, { baseUrl: provider.baseUrl });
    await validateAndEnter({ input, cdp, scope: "session", row, field, value, sink, owner });
    expected.set(row.key, value);
  }
  // Discard all six edited controls together, with independent file readback.
  await activate(input, cdp, "session", "discard-session-settings");
  await wait(cdp, "session", "Session discard restores every baseline field", surface => sessionValuesMatch(surface, baseline));
  for (const row of SESSION_CONTROL_PLAN) await replace(input, cdp, "session", row.key, expected.get(row.key));
  await sessionSaveAndReopen(input, cdp, expected, commands, sink, owner);
  for (const row of SESSION_CONTROL_PLAN.filter(row => row.kind === "enum")) {
    for (const value of row.options) {
      await replace(input, cdp, "session", row.key, value);
      expected.set(row.key, value);
      await sessionSaveAndReopen(input, cdp, expected, commands, sink, owner);
    }
  }
  await replace(input, cdp, "session", "context-window", "");
  expected.set("context-window", "");
  await sessionSaveAndReopen(input, cdp, expected, commands, sink, owner);
  const inherited = await observeFieldControls(cdp, "session");
  if (!inherited.projection.session_settings.context_window_inherited) throw failure("settings-session-inheritance-missing", "Empty context must restore inheritance", inherited);
  if (!(await readFile(context.paths.config_file)).equals(globalConfigBefore)) throw failure("settings-session-global-changed", "Session controls must leave Global config file unchanged", {});
  if (provider.requestLedger.length !== requestCount) throw failure("settings-session-unrequested-network", "Editing Session Settings must not send another provider request", provider.requestLedger);
  await captureScenarioScreenshot({ cdp, sink, name: "settings-session-field-controls-saved", owner });
  await activate(input, cdp, "session", "close-overlay");
  await sink.record("settings-session-control-audit", { controls: SESSION_CONTROL_PLAN.map(row => row.key), global_config_bytes_unchanged: true,
    initial_root_provider_requests: requestCount, field_edit_provider_requests: 0, inheritance_reapplied: true }, { phase: "executing", owner });
}

function createScenario(scope, options = {}) {
  const id = options.id ?? `settings.${scope === "global" ? "field-controls" : `${scope}-field-controls`}`;
  const owner = `scenario:${id}`;
  let provider = null, acceptedLedger = null, quiesceOutcome = null;
  const resources = [];
  return Object.freeze({ id, productOracle: "pass", manualGate: "not_required", databaseRequired: true, requestGracefulExit,
    async prepare({ context, sink, phase }) {
      provider = await startScriptedProvider({ expectedPrompt: ROOT_PROMPT, responseText: ROOT_RESPONSE });
      await prepareDesktopFixture({ context, sink, phase, owner,
        ...(scope === "initial" ? { configMode: "absent" } : { configText: settingsPreferencesFixtureConfig(provider.baseUrl) }),
        sentinelName: "E2E_SETTINGS_FIELD_CONTROLS.txt", sentinelText: "Isolated settings control audit. No user files or live provider.\n" });
      await sink.record("settings-field-control-plan", { scope, plan: scope === "session" ? SESSION_CONTROL_PLAN : SETTINGS_CONTROL_PLAN,
        claims: "trusted WebView input, public projection/file readback; no physical IME or whole-app coverage claim" }, { phase, owner });
    },
    async execute(args) {
      const { context, driver: cdp, sink } = args;
      if (scope !== "initial") await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: owner, screenshotStem: "settings-field-controls-shell" });
      const input = new WebviewInput(cdp, { probeId: id, maxProbeEvents: 8192 });
      const commands = new DesktopCommandProbe(cdp, { probeId: id, commands: ["save_global_config", "apply_session_config", "apply_session_settings", "finish_initial_setup", "submit", "send_prompt"] });
      let primary = null;
      try {
        await input.installProbe(); await commands.install();
        const exercise = options.exercise ?? { global: exerciseGlobal, initial: exerciseInitial, session: exerciseSession }[scope];
        await exercise({ ...args, scenario: this, input, cdp, commands, context, sink, owner, provider });
        if (scope !== "session" && provider.requestLedger.some(row => !options.catalogRequests || !["models", "lm_studio_models"].includes(row.route))) throw failure("settings-audit-unrequested-network", "Settings must not start undeclared provider/Docling/MCP requests", provider.requestLedger);
        acceptedLedger = structuredClone(provider.requestLedger);
        return { acquisition: "pass", oracle: "pass", manual: "not_required" };
      } catch (error) { primary = error; throw error; }
      finally {
        const failures = [];
        try { resources.push({ kind: "webview-input", result: await input.cleanup() }); } catch (error) { failures.push(String(error)); }
        try { resources.push({ kind: "command-probe", result: await commands.remove() }); } catch (error) { failures.push(String(error)); }
        if (failures.length) { resources.push({ kind: "cleanup-failures", failures }); if (primary === null) throw new Error(failures.join("; ")); }
      }
    },
    async quiesce({ inputs }) {
      if (quiesceOutcome === null) quiesceOutcome = await quiesceProviderResource({ provider, acceptedLedger, inputs });
      return structuredClone(quiesceOutcome);
    },
    async cleanup() { return { input: resources.some(resource => resource.failures?.length) || quiesceOutcome?.input !== "pass" ? "fail" : "pass", resources }; },
  });
}

export function createSettingsFieldControlsScenario() { return createScenario("global"); }
export function createInitialSettingsFieldControlsScenario() { return createScenario("initial"); }
export function createSessionSettingsFieldControlsScenario() { return createScenario("session"); }

// Shared settings fixture/lifecycle for additional real controls, with the same
// input, projection and exact cleanup owners as the field traversal.
export function createSettingsControlScenario(scope, options) { return createScenario(scope, options); }
export { activate, click, focusThroughTab, openGlobal, replace, wait, acquireControl, exerciseSession };

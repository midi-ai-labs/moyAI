import { DesktopE2eError } from "../core/execution.mjs";
import { DesktopCommandProbe } from "./desktop_command_probe.mjs";
import { WebviewInput } from "./webview_input.mjs";
import { SETTINGS_CONTROL_PLAN } from "../scenarios/settings_control_plan.mjs";
import { activate, observeFieldControls, openGlobal, replace, wait } from "../scenarios/settings_field_controls.mjs";

const OWNER = "scenario:manual.suite";
const editable = new Set(SETTINGS_CONTROL_PLAN.map(row => row.key));
const inheritedApproveFields = new Map([
  "base_url", "model", "provider_profile", "api_key_env", "context_window",
  "request_timeout_ms", "connect_timeout_ms", "max_retries",
].map(key => [`approve.${key}`, `model.${key}`]));
const failure = (code, message, evidence) => new DesktopE2eError("harness", `manual-suite-config-${code}`, message, evidence);

function publicFields(projection) {
  if (!Array.isArray(projection?.config_fields)) throw failure("inventory", "Public configuration fields are unavailable", {});
  const fields = new Map();
  for (const row of projection.config_fields) {
    if (typeof row?.key !== "string" || typeof row.value !== "string" || fields.has(row.key)) {
      throw failure("inventory", "Public configuration fields have invalid or duplicate keys", {});
    }
    if (row.sensitive && row.value !== "") throw failure("redaction", "Sensitive public configuration must remain redacted", { key: row.key });
    fields.set(row.key, { key: row.key, value: row.value, sensitive: row.sensitive === true, configured: row.configured });
  }
  return fields;
}

export function manualSuiteConfigPlan(projection, expectedFields) {
  if (!Array.isArray(expectedFields)) throw new TypeError("expectedFields must be an array of public key/value pairs");
  const baseline = publicFields(projection);
  const expected = new Map([...baseline].map(([key, row]) => [key, { ...row }]));
  const approveConfigured = projection.approve_model_configured;
  const seen = new Set(), explicitKeys = new Set(), changes = [], verified = [];
  for (const pair of expectedFields) {
    if (typeof pair?.key !== "string" || typeof pair.value !== "string" || seen.has(pair.key)) {
      throw new TypeError("expectedFields must contain unique string key/value pairs");
    }
    if (pair.explicit !== undefined && typeof pair.explicit !== "boolean") throw new TypeError("expectedFields explicit provenance must be boolean");
    seen.add(pair.key);
    if (pair.explicit !== false) explicitKeys.add(pair.key);
    const current = baseline.get(pair.key);
    if (!current) throw failure("unobservable", "Expected configuration has no public field", { key: pair.key });
    if (current.sensitive && pair.value !== "") throw failure("sensitive-input", "Expected fields must not contain sensitive values", { key: pair.key });
    if (current.value !== pair.value && !(approveConfigured === false && inheritedApproveFields.has(pair.key))) {
      if (!editable.has(pair.key)) throw failure("unmapped", "Changed field has no qualified Settings control", { key: pair.key });
      changes.push({ key: pair.key, value: pair.value });
    }
    expected.get(pair.key).value = pair.value;
    verified.push({ key: pair.key, value: pair.value });
  }
  // An absent Approve connection projects Main's values; it is not an independent draft.
  if (approveConfigured === false) {
    for (const [approveKey, mainKey] of inheritedApproveFields) {
      const current = baseline.get(approveKey);
      if (!current) continue;
      const main = expected.get(mainKey);
      if (!main) throw failure("unobservable", "Inherited Approve configuration has no public field for Main", { key: mainKey });
      const requested = expected.get(approveKey);
      if (explicitKeys.has(approveKey) && requested.value !== main.value) {
        throw failure("unmapped", "Explicit Approve expectation conflicts with Main and has no qualified Settings control", { key: approveKey });
      }
      requested.value = main.value;
    }
  }
  // Enable dependent Docling controls first; restore their values before disabling them.
  changes.sort((a, b) => priority(a) - priority(b));
  return { baseline: [...baseline.values()], expected: [...expected.values()], changes,
    verified: verified.map(pair => ({ key: pair.key, value: expected.get(pair.key).value })), approveConfigured };
}

function priority(pair) { return pair.key === "docling.enabled" ? pair.value === "true" ? -1 : 1 : 0; }

export function manualSuiteConfigFailures(projection, plan, { baseline = false } = {}) {
  const actual = publicFields(projection), expected = new Map((baseline ? plan.baseline : plan.expected).map(row => [row.key, row]));
  const failures = [];
  for (const key of new Set([...actual.keys(), ...expected.keys()])) {
    const a = actual.get(key), e = expected.get(key);
    if (!a || !e || a.value !== e.value || a.sensitive !== e.sensitive || (e.sensitive && a.configured !== e.configured)) failures.push(key);
  }
  if (typeof plan.approveConfigured === "boolean" && projection.approve_model_configured !== plan.approveConfigured) failures.push("approve_model_configured");
  return failures;
}

export async function applyManualSuiteConfig({ driver, sink, expectedFields, stem }) {
  if (typeof stem !== "string" || !/^[a-zA-Z0-9_.-]+$/.test(stem)) throw new TypeError("manual suite config requires an evidence stem");
  const initial = await observeFieldControls(driver);
  if (initial.projection.busy !== false || initial.projection.confirmation_visible !== false || initial.projection.overlay !== "none") {
    throw failure("not-idle", "Configuration changes require an idle shell without an overlay", {});
  }
  const plan = manualSuiteConfigPlan(initial.projection, expectedFields);
  const result = { applied: [], verified: plan.verified };
  if (plan.changes.length === 0) {
    const mismatches = manualSuiteConfigFailures(initial.projection, plan);
    if (mismatches.length) throw failure("state-mismatch", "Current configuration does not match its expected public values", { keys: mismatches });
    await sink.record(`${stem}-suite-config`, result, { phase: "executing", owner: OWNER });
    return result;
  }
  const input = new WebviewInput(driver, { probeId: `${stem}-suite-config` });
  const commands = new DesktopCommandProbe(driver, { probeId: `${stem}-suite-config`, commands: ["save_global_config"] });
  let primary = null;
  try {
    await input.installProbe();
    await commands.install();
    const opened = await openGlobal(input, driver);
    const drift = manualSuiteConfigFailures(opened.projection, plan, { baseline: true });
    if (opened.dirty || drift.length) throw failure("draft-or-drift", "Settings changed before the suite could edit them", { keys: drift, dirty: opened.dirty });
    for (const pair of plan.changes) await replace(input, driver, "global", pair.key, pair.value);
    const draft = await observeFieldControls(driver);
    const invalid = draft.controls.filter(row => row.invalid).map(row => row.key);
    if (invalid.length) throw failure("invalid", "Settings rejected the expected configuration", { keys: [...new Set(invalid)] });
    const beforeSave = await commands.snapshot();
    await activate(input, driver, "global", "save-global-config");
    await wait(driver, "global", "Manual suite configuration is saved", surface => surface.panel && !surface.dirty && surface.error_count === 0 && manualSuiteConfigFailures(surface.projection, plan).length === 0);
    const calls = (await commands.snapshot(beforeSave.sequence)).calls;
    if (calls.length !== 1 || calls[0].command !== "save_global_config") throw failure("save-count", "One Settings Save must dispatch exactly once", { count: calls.length });
    await activate(input, driver, "global", "close-overlay");
    await wait(driver, "global", "Manual suite returns to the configured shell", surface => !surface.panel && surface.projection.overlay === "none" && manualSuiteConfigFailures(surface.projection, plan).length === 0);
    result.applied = plan.changes;
    await sink.record(`${stem}-suite-config`, result, { phase: "executing", owner: OWNER });
    return result;
  } catch (error) { primary = error; throw error; }
  finally {
    const errors = [];
    try { await commands.remove(); } catch (error) { errors.push(String(error)); }
    try { await input.cleanup(); } catch (error) { errors.push(String(error)); }
    if (errors.length) {
      await sink.record(`${stem}-suite-config-cleanup`, { errors }, { phase: "executing", owner: OWNER });
      if (primary === null) throw failure("cleanup", "Suite configuration input probes did not settle", { errors });
    }
  }
}

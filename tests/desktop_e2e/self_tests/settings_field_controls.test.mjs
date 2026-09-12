import assert from "node:assert/strict";
import test from "node:test";
import { validateConfigInput } from "../../../ui/desktop-web/src/utils.ts";
import { validateInitialSetupStep } from "../../../ui/desktop-web/src/initial_setup_state.ts";
import { SETTINGS_CONTROL_PLAN, SESSION_CONTROL_PLAN, assertSettingsInventory, controlValidValue, controlInvalidValues, publicFieldMatches } from "../scenarios/settings_control_plan.mjs";
import { createSettingsFieldControlsScenario, createInitialSettingsFieldControlsScenario, createSessionSettingsFieldControlsScenario } from "../scenarios/settings_field_controls.mjs";

function descriptor(row) {
  return { key: row.key, value: row.kind === "boolean" ? "true" : row.options?.[0] ?? "1",
    value_type: ["headers", "servers"].includes(row.kind) ? "json" : ["url", "model", "prompt", "env", "extensions"].includes(row.kind) ? "string" : row.kind,
    required: ["url", "model", "integer", "enum", "boolean"].includes(row.kind),
    min_value: row.kind === "integer" ? 1 : null, max_value: row.kind === "integer" ? 255 : null,
    sensitive: row.sensitive === true, configured: true, options: [...(row.options ?? [])] };
}

test("control inventory rejects missing fields, added fields, or option drift", () => {
  const fields = SETTINGS_CONTROL_PLAN.map(descriptor);
  assert.equal(assertSettingsInventory(fields), true);
  assert.throws(() => assertSettingsInventory(fields.slice(1)), /inventory drift/);
  assert.throws(() => assertSettingsInventory([...fields, { ...fields[0], key: "unexpected.field" }]), /inventory drift/);
  const changed = structuredClone(fields);
  changed.find(field => field.value_type === "enum").options.reverse();
  assert.throws(() => assertSettingsInventory(changed), /options drift/);
});

test("field values and invalid probes obey real Desktop validation", () => {
  for (const row of SETTINGS_CONTROL_PLAN) {
    const field = descriptor(row);
    const valid = controlValidValue(row, field, { baseUrl: "http://127.0.0.1:19471/v1" });
    assert.equal(validateConfigInput(field, valid, [{ key: "docling.enabled", text: "true" }]).ok, true, row.key);
    for (const invalid of controlInvalidValues(row, field)) {
      assert.equal(validateConfigInput(field, invalid, [{ key: "docling.enabled", text: "true" }]).ok, false, `${row.key}: ${invalid}`);
    }
  }
});

test("Initial field partition follows real wizard validation step ownership", () => {
  for (const row of SETTINGS_CONTROL_PLAN) {
    const field = { ...descriptor(row), required: true };
    for (const step of ["provider", "model", "permissions", "tools", "finish"]) {
      const result = validateInitialSetupStep(step, [field], [{ key: field.key, text: "" }]);
      assert.equal(result.ok, step !== "finish" && step !== row.initialStep, `${row.key} in ${step}`);
    }
  }
});

test("sensitive readback requires configured plus redaction and rejects leaked or absent values", () => {
  const row = SETTINGS_CONTROL_PLAN.find(row => row.kind === "headers");
  assert.equal(publicFieldMatches({ sensitive: true, configured: true, value: "" }, "fixture", row), true);
  assert.equal(publicFieldMatches({ sensitive: true, configured: false, value: "" }, "fixture", row), false);
  assert.equal(publicFieldMatches({ sensitive: true, configured: true, value: "fixture" }, "fixture", row), false);
  assert.equal(publicFieldMatches({ value: "before" }, "after", { sensitive: false }), false);
});

test("numeric fixture changes a value already at its maximum instead of claiming an unchanged save", () => {
  const row = SETTINGS_CONTROL_PLAN.find(row => row.key === "side_chat.request_timeout_ms");
  const field = { ...descriptor(row), value: "3600000", max_value: 3600000 };
  assert.equal(controlValidValue(row, field), "3599999");
  assert.equal(validateConfigInput(field, controlValidValue(row, field)).ok, true);
});

test("disabled MCP fixture is a complete explicit HTTP server entry without an enabled remote endpoint", () => {
  const row = SETTINGS_CONTROL_PLAN.find(row => row.kind === "servers");
  const [entry] = JSON.parse(controlValidValue(row, descriptor(row), { baseUrl: "http://127.0.0.1:19471" }));
  assert.deepEqual(entry, { id: "audit-1", enabled: false, transport: "http", base_url: "http://127.0.0.1:19471/mcp", timeout_ms: 1000, headers: {} });
});

test("scenario factories share lifecycle contract and separate GUI surfaces", () => {
  for (const [factory, id] of [[createSettingsFieldControlsScenario, "settings.field-controls"], [createInitialSettingsFieldControlsScenario, "settings.initial-field-controls"], [createSessionSettingsFieldControlsScenario, "settings.session-field-controls"]]) {
    const scenario = factory();
    assert.equal(scenario.id, id);
    for (const method of ["prepare", "execute", "requestGracefulExit", "quiesce", "cleanup"]) assert.equal(typeof scenario[method], "function");
  }
  assert.equal(new Set(SETTINGS_CONTROL_PLAN.map(row => row.key)).size, 43);
  assert.equal(SETTINGS_CONTROL_PLAN.filter(row => row.kind === "enum").reduce((sum, row) => sum + row.options.length, 0), 13);
  assert.equal(SETTINGS_CONTROL_PLAN.filter(row => row.kind === "boolean").length, 8);
  assert.equal(SESSION_CONTROL_PLAN.length, 6);
});

import assert from "node:assert/strict";
import test from "node:test";
import { applyManualSuiteConfig, manualSuiteConfigFailures, manualSuiteConfigPlan } from "../drivers/manual_suite_config.mjs";
import { manualSuiteExpectedFields } from "../scenarios/manual_suite.mjs";

const field = (key, value, extra = {}) => ({ key, value, sensitive: false, ...extra });
const projection = rows => ({ busy: false, confirmation_visible: false, overlay: "none", config_fields: rows });

test("suite applies only differences and retains all unspecified public values", () => {
  const before = projection([field("model.model", "one"), field("model.request_timeout_ms", "3600000"), field("docling.enabled", "false")]);
  const plan = manualSuiteConfigPlan(before, [{ key: "model.model", value: "two" }]);
  assert.deepEqual(plan.changes, [{ key: "model.model", value: "two" }]);
  assert.deepEqual(manualSuiteConfigFailures(projection([field("model.model", "two"), ...before.config_fields.slice(1)]), plan), []);
  assert.deepEqual(manualSuiteConfigFailures(projection([field("model.model", "two"), field("model.request_timeout_ms", "1"), field("docling.enabled", "false")]), plan), ["model.request_timeout_ms"]);
  assert.deepEqual(manualSuiteConfigFailures(projection([...before.config_fields, field("new.field", "value")]), plan), ["model.model", "new.field"]);
});

test("unmapped no-op is verified, but a change or absent field fails before GUI edits", () => {
  const before = projection([field("future.public_field", "same")]);
  assert.deepEqual(manualSuiteConfigPlan(before, [{ key: "future.public_field", value: "same" }]).changes, []);
  assert.throws(() => manualSuiteConfigPlan(before, [{ key: "future.public_field", value: "changed" }]), /qualified Settings control/);
  assert.throws(() => manualSuiteConfigPlan(before, [{ key: "approve.model", value: "same" }]), /no public field/);
});

test("duplicate, malformed, and sensitive expectations cannot be silently skipped", () => {
  const before = projection([field("model.model", "one"), field("model.extra_headers_json", "", { sensitive: true, configured: true })]);
  assert.throws(() => manualSuiteConfigPlan(before, [{ key: "model.model", value: "one" }, { key: "model.model", value: "two" }]), /unique/);
  assert.throws(() => manualSuiteConfigPlan(before, [{ key: "model.model", value: 1 }]), /string/);
  assert.throws(() => manualSuiteConfigPlan(before, [{ key: "model.extra_headers_json", value: "secret" }]), /sensitive values/);
  assert.throws(() => manualSuiteConfigPlan(projection([field("model.model", "one"), field("model.model", "two")]), []), /duplicate/);
  const plan = manualSuiteConfigPlan(before, [{ key: "model.extra_headers_json", value: "" }]);
  assert.deepEqual(plan.changes, []);
  assert.deepEqual(manualSuiteConfigFailures(projection([before.config_fields[0], field("model.extra_headers_json", "", { sensitive: true, configured: false })]), plan), ["model.extra_headers_json"]);
  assert.throws(() => manualSuiteConfigFailures(projection([field("model.extra_headers_json", "leaked", { sensitive: true })]), plan), /redacted/);
});

test("Docling enable precedes dependent edits and disable follows restoration", () => {
  const before = projection([field("docling.enabled", "false"), field("docling.base_url", "http://old")]);
  const enabled = manualSuiteConfigPlan(before, [{ key: "docling.base_url", value: "http://new" }, { key: "docling.enabled", value: "true" }]);
  assert.deepEqual(enabled.changes.map(row => row.key), ["docling.enabled", "docling.base_url"]);
  const disabled = manualSuiteConfigPlan(projection([field("docling.enabled", "true"), field("docling.base_url", "http://new")]), [{ key: "docling.enabled", value: "false" }, { key: "docling.base_url", value: "http://old" }]);
  assert.deepEqual(disabled.changes.map(row => row.key), ["docling.base_url", "docling.enabled"]);
});

test("no-op uses only public reads and records verified pairs without installing mutation probes", async () => {
  const reads = [], records = [];
  const p = projection([field("model.model", "one"), field("future.public_field", "same")]);
  const driver = { evaluate: async script => { reads.push(script); return { projection: p }; }, call: async () => { throw new Error("must not dispatch input"); } };
  const expectedFields = [{ key: "model.model", value: "one" }, { key: "future.public_field", value: "same" }];
  const result = await applyManualSuiteConfig({ driver, sink: { record: async (...args) => records.push(args) }, expectedFields, stem: "case1" });
  assert.deepEqual(result, { applied: [], verified: expectedFields });
  assert.equal(reads.length, 1);
  assert.equal(records.length, 1);
});

test("busy and unqualified changes are rejected without input dispatch", async () => {
  const expectedFields = [{ key: "unknown.field", value: "new" }];
  const driver = { evaluate: async () => ({ projection: projection([field("unknown.field", "old")]) }), call: async () => { throw new Error("must not dispatch input"); } };
  await assert.rejects(applyManualSuiteConfig({ driver, sink: {}, expectedFields, stem: "case1" }), /qualified Settings control/);
  driver.evaluate = async () => ({ projection: { ...projection([]), busy: true } });
  await assert.rejects(applyManualSuiteConfig({ driver, sink: {}, expectedFields: [], stem: "case1" }), /idle shell/);
});

test("unconfigured Approve follows Main without editing its derived fields", () => {
  const before = { ...projection([
    field("model.request_timeout_ms", "30000"), field("approve.request_timeout_ms", "30000"),
    field("side_chat.request_timeout_ms", "3600000"),
  ]), approve_model_configured: false };
  const expectedFields = [{ key: "model.request_timeout_ms", value: "31000" }, { key: "approve.request_timeout_ms", value: "30000", explicit: false }];
  const plan = manualSuiteConfigPlan(before, expectedFields);
  assert.deepEqual(plan.changes, [{ key: "model.request_timeout_ms", value: "31000" }]);
  const saved = { ...before, config_fields: [field("model.request_timeout_ms", "31000"), field("approve.request_timeout_ms", "31000"), before.config_fields[2]] };
  assert.deepEqual(manualSuiteConfigFailures(saved, plan), []);
  assert.deepEqual(plan.verified, [{ key: "model.request_timeout_ms", value: "31000" }, { key: "approve.request_timeout_ms", value: "31000" }]);
  assert.deepEqual(manualSuiteConfigFailures(before, plan, { baseline: true }), []);
  assert.deepEqual(manualSuiteConfigFailures({ ...saved, approve_model_configured: true }, plan), ["approve_model_configured"]);
  assert.deepEqual(manualSuiteConfigFailures({ ...saved, config_fields: [saved.config_fields[0], before.config_fields[1], before.config_fields[2]] }, plan), ["approve.request_timeout_ms"]);
  assert.deepEqual(manualSuiteConfigFailures({ ...saved, config_fields: [...saved.config_fields.slice(0, 2), field("side_chat.request_timeout_ms", "31000")] }, plan), ["side_chat.request_timeout_ms"]);
  const plannedApprove = manualSuiteConfigPlan(before, [{ key: "approve.request_timeout_ms", value: "31000" }, { key: "model.request_timeout_ms", value: "31000" }]);
  assert.deepEqual(plannedApprove.changes, [{ key: "model.request_timeout_ms", value: "31000" }]);
  assert.deepEqual(manualSuiteConfigFailures(saved, plannedApprove), []);
  assert.throws(() => manualSuiteConfigPlan(before, [{ key: "model.request_timeout_ms", value: "31000" }, { key: "approve.request_timeout_ms", value: "32000" }]), /qualified Settings control/);
});

test("all public unconfigured Approve values derive from their planned Main values", () => {
  const values = {
    base_url: ["http://old", "http://new"], model: ["old", "new"],
    provider_profile: ["openai_responses", "openai_compatible"], api_key_env: ["", "NEW_KEY"],
    context_window: ["131072", "65536"], request_timeout_ms: ["30000", "31000"],
    connect_timeout_ms: ["1000", "2000"], max_retries: ["0", "1"],
  };
  const rows = Object.entries(values).flatMap(([key, [oldValue]]) => [field(`model.${key}`, oldValue), field(`approve.${key}`, oldValue)]);
  const before = { ...projection(rows), approve_model_configured: false };
  const expectedFields = Object.entries(values).flatMap(([key, [oldValue, newValue]]) => [{ key: `approve.${key}`, value: oldValue, explicit: false }, { key: `model.${key}`, value: newValue }]);
  const plan = manualSuiteConfigPlan(before, expectedFields);
  assert.equal(plan.changes.length, 8);
  assert.ok(plan.changes.every(pair => pair.key.startsWith("model.")));
  const saved = { ...before, config_fields: Object.entries(values).flatMap(([key, [, newValue]]) => [field(`model.${key}`, newValue), field(`approve.${key}`, newValue)]) };
  assert.deepEqual(manualSuiteConfigFailures(saved, plan), []);
  assert.throws(() => manualSuiteConfigPlan({ ...before, config_fields: rows.filter(row => row.key !== "model.max_retries") }, []), /no public field/);
});

test("configured Approve remains independent and its qualified controls can change", () => {
  const before = { ...projection([field("model.request_timeout_ms", "30000"), field("approve.request_timeout_ms", "60000")]), approve_model_configured: true };
  const plan = manualSuiteConfigPlan(before, [{ key: "model.request_timeout_ms", value: "31000" }, { key: "approve.request_timeout_ms", value: "60000" }]);
  const saved = { ...before, config_fields: [field("model.request_timeout_ms", "31000"), before.config_fields[1]] };
  assert.deepEqual(manualSuiteConfigFailures(saved, plan), []);
  assert.deepEqual(manualSuiteConfigFailures({ ...saved, approve_model_configured: false }, plan), ["approve_model_configured"]);
  assert.deepEqual(manualSuiteConfigFailures({ ...saved, config_fields: [saved.config_fields[0], field("approve.request_timeout_ms", "31000")] }, plan), ["approve.request_timeout_ms"]);
  const approveEdit = manualSuiteConfigPlan(before, [{ key: "approve.request_timeout_ms", value: "61000" }]);
  assert.deepEqual(approveEdit.changes, [{ key: "approve.request_timeout_ms", value: "61000" }]);
  assert.deepEqual(manualSuiteConfigFailures({ ...before, config_fields: [before.config_fields[0], field("approve.request_timeout_ms", "61000")] }, approveEdit), []);
});

test("a no-op cannot accept incorrect inherited Approve values without GUI dispatch", async () => {
  const p = { ...projection([field("model.request_timeout_ms", "31000"), field("approve.request_timeout_ms", "30000")]), approve_model_configured: false };
  const driver = { evaluate: async () => ({ projection: p }), call: async () => { throw new Error("must not dispatch input"); } };
  await assert.rejects(applyManualSuiteConfig({ driver, sink: {}, expectedFields: [], stem: "case1" }), /does not match/);
});

test("baseline Approve provenance survives A to B to C and another save without a Main change", () => {
  const rows = (timeout, docling) => [field("model.request_timeout_ms", timeout), field("approve.request_timeout_ms", timeout), field("docling.enabled", docling)];
  const baseline = rows("30000", "false");
  const atA = { ...projection(baseline), approve_model_configured: false };
  const first = manualSuiteExpectedFields(baseline, new Map([["model.request_timeout_ms", "31000"], ["docling.enabled", "true"]]));
  const planB = manualSuiteConfigPlan(atA, first);
  const atB = { ...atA, config_fields: planB.expected };
  assert.deepEqual(manualSuiteConfigFailures(atB, planB), []);
  const doclingOff = manualSuiteExpectedFields(baseline, new Map([["model.request_timeout_ms", "31000"], ["docling.enabled", "false"]]));
  const planOff = manualSuiteConfigPlan(atB, doclingOff);
  assert.deepEqual(planOff.changes, [{ key: "docling.enabled", value: "false" }]);
  assert.deepEqual(manualSuiteConfigFailures({ ...atB, config_fields: planOff.expected }, planOff), []);
  const next = manualSuiteExpectedFields(baseline, new Map([["model.request_timeout_ms", "32000"], ["docling.enabled", "false"]]));
  const planC = manualSuiteConfigPlan(atB, next);
  assert.deepEqual(planC.changes, [{ key: "model.request_timeout_ms", value: "32000" }, { key: "docling.enabled", value: "false" }]);
  assert.deepEqual(manualSuiteConfigFailures({ ...atB, config_fields: planC.expected }, planC), []);
});

test("explicit absent-Approve expectations cannot contradict planned Main even if they equal current values", () => {
  const before = { ...projection([field("model.request_timeout_ms", "31000"), field("approve.request_timeout_ms", "31000")]), approve_model_configured: false };
  for (const metadata of [{}, { explicit: true }]) {
    assert.throws(() => manualSuiteConfigPlan(before, [{ key: "model.request_timeout_ms", value: "32000" }, { key: "approve.request_timeout_ms", value: "31000", ...metadata }]), /qualified Settings control/);
  }
  assert.throws(() => manualSuiteConfigPlan(before, [{ key: "approve.request_timeout_ms", value: "31000", explicit: "false" }]), /boolean/);
});

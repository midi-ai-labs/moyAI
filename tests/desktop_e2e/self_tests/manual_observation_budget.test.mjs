import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { ManualObservationBudget, normalizeManualObservationTimeout } from "../core/deadline.mjs";
import { ManualLiveSession, normalizeManualLiveOptions, manualLiveFixtureConfig } from "../drivers/manual_live_session.mjs";
import { manualCase3Stages, normalizeManualCase3Options } from "../scenarios/manual_case3.mjs";
import { createManualCase1Scenario } from "../scenarios/manual_case1.mjs";
import { createManualCase2Scenario, normalizeManualCase2Options } from "../scenarios/manual_case2.mjs";
import { createManualCase4Scenario } from "../scenarios/manual_case4.mjs";
import { createManualCase6Scenario } from "../scenarios/manual_case6.mjs";
import { normalizeManualCase5Options } from "../scenarios/manual_case5.mjs";
import { normalizeManualCase7Options } from "../scenarios/manual_case7.mjs";

const RAW = { provider_base_url: "http://provider.invalid/v1", model: "exact-model", python_executable: process.execPath };

test("manual observation option accepts both integer boundaries and rejects invalid or unbounded budgets", () => {
  assert.equal(normalizeManualObservationTimeout(undefined), undefined);
  for (const value of [1, 7_200_000]) {
    assert.equal(normalizeManualObservationTimeout(value), value);
    assert.equal(normalizeManualLiveOptions({ ...RAW, observation_timeout_ms: value }).observationTimeoutMs, value);
  }
  for (const value of [0, -1, 7_200_001, 1.5, "7200000", null, true, Infinity, NaN, Number.MAX_SAFE_INTEGER + 1]) {
    assert.throws(() => normalizeManualLiveOptions({ ...RAW, observation_timeout_ms: value }), /observation_timeout_ms/);
  }
  assert.equal(normalizeManualLiveOptions(RAW).observationTimeoutMs, undefined);
  assert.equal(manualLiveFixtureConfig(normalizeManualLiveOptions(RAW)),
    manualLiveFixtureConfig(normalizeManualLiveOptions({ ...RAW, observation_timeout_ms: 7_200_000 })));
  for (const normalize of [
    raw => normalizeManualCase2Options({ ...raw, image_source: `${process.execPath}.png` }),
    raw => normalizeManualCase3Options({ ...raw, fixture_source: process.cwd() }),
    raw => normalizeManualCase5Options({ ...raw, fixture_source: process.cwd() }),
    raw => normalizeManualCase7Options({ ...raw, fixture_source: process.cwd(), docling_base_url: "http://docling.invalid" }),
  ]) assert.equal(normalize({ ...RAW, observation_timeout_ms: 7_200_000 }).observationTimeoutMs, 7_200_000);
});

test("case deadline begins once and bounds later checks without extending the observation period", () => {
  let now = 100;
  const budget = new ManualObservationBudget({ timeoutMs: 1000, now: () => now });
  now = 10_000;
  assert.equal(budget.remainingMs(), 1000);
  assert.equal(budget.expired, false);
  assert.equal(budget.snapshot().deadline_ms, null);
  budget.beginOnce();
  now += 700;
  assert.equal(budget.beginOnce().deadline_ms, 11_000);
  assert.equal(budget.assertRemaining("verification", 120_000), 300);
  assert.equal(budget.assertRemaining("short check", 50), 50);
  now = 11_000;
  assert.equal(budget.remainingMs(), 0);
  assert.equal(budget.expired, true);
  assert.throws(() => budget.assertRemaining("next stage"), error => error.code === "manual-observation-timeout"
    && error.evidence.started_at_ms === 10_000 && error.evidence.deadline_ms === 11_000);
});

test("manual fixture input evidence preserves case defaults and uses the explicit 120-minute override", async context => {
  const parent = fileURLToPath(new URL("../../../../project_sandbox/manual-st-harness-self-tests/", import.meta.url));
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(path.join(parent, "defaults-"));
  context.after(async () => { assert.equal(path.dirname(root), path.resolve(parent)); await rm(root, { recursive: true }); });
  const image = path.join(root, "reference.png");
  await writeFile(image, "fixture image bytes");
  for (const [name, create, expected] of [
    ["case1", createManualCase1Scenario, 15 * 60 * 1000],
    ["case2", raw => createManualCase2Scenario({ ...raw, image_source: image }, { prepareCapture: async () => {} }), 30 * 60 * 1000],
    ["case4", createManualCase4Scenario, 60 * 60 * 1000],
    ["case6", createManualCase6Scenario, 30 * 60 * 1000],
  ]) {
    const configs = [];
    for (const override of [undefined, 7_200_000]) {
      const fixture = path.join(root, `${name}-${override ?? "default"}`);
      await mkdir(fixture);
      const paths = Object.fromEntries(["workspace", "config", "data", "prefs", "webview", "logs"].map(key => [key, path.join(fixture, key)]));
      await Promise.all(Object.values(paths).map(directory => mkdir(directory)));
      paths.config_file = path.join(paths.config, "config.toml"); paths.prefs_file = path.join(paths.prefs, "desktop.toml");
      const records = [];
      const sink = { writeJson: async name => ({ path: name }), writeBytes: async name => ({ path: name }), record: async (name, data) => records.push({ name, data }) };
      const scenario = create({ ...RAW, ...(override === undefined ? {} : { observation_timeout_ms: override }) });
      await scenario.prepare({ context: { root: fixture, paths }, sink, phase: "prepared" });
      assert.equal(records.find(row => row.name === `${name}-input`).data.observation_timeout_ms, override ?? expected);
      configs.push(await readFile(paths.config_file, "utf8"));
    }
    assert.equal(configs[0], configs[1], `${name}: observation override must not change product configuration`);
  }
});

function fakeSession({ clock, workspace = process.cwd(), approval = false, review = async () => ({ status: "decided", decision: "approve" }), budgetMs = 1000 }) {
  const sessionId = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
  const turns = ["01ARZ3NDEKTSV4RRFFQ69G5FAW", "01ARZ3NDEKTSV4RRFFQ69G5FAX", "01ARZ3NDEKTSV4RRFFQ69G5FAY", "01ARZ3NDEKTSV4RRFFQ69G5FAZ"];
  const events = [], calls = [], records = [], documents = [];
  let turn = 0, value = "", mode = "completed";
  const surface = () => {
    const p = { confirmation_visible: mode === "approval", confirmation_id: mode === "approval" ? "42" : null,
      confirmation: mode === "approval" ? { summary: "Run tests", details: ["python -m unittest"], remote: null } : null,
      busy: mode === "approval", post_run_refresh_pending: false, background_mutation_pending: false,
      async_polling_required: false, pending_async_operations: [], run_status_key: mode === "cancelled" ? "cancelled" : "completed",
      task_activity_state: mode === "approval" ? "running" : "idle", composer_submit_mode: "new_request", can_submit: true,
      draft_target: { sessionId }, run_target: { workspacePath: workspace, sessionId,
        expectedState: { kind: "idle", latestTurnId: turns[turn], admissionRevision: String(turn) } },
      stop_target: { kind: "turn", workspacePath: workspace, sessionId, turnId: turns[turn], admissionRevision: String(turn) } };
    return { projection: p, composer: { count: 1, visible: true, run_target: structuredClone(p.run_target) },
      session_usage: { count: 0, visible: false, text: null, title: null, state: null },
      visible_dialog_count: mode === "approval" ? 1 : 0, visible_modal_backdrop_count: mode === "approval" ? 1 : 0,
      visible_fatal_count: 0, visible_recoverable_error_count: 0,
      prompt: { count: 1, visible: true, enabled: true, value }, send: { count: 1, visible: true, enabled: true } };
  };
  const emit = event => events.push({ sequence: events.length + 1, isTrusted: true, ...event });
  const input = {
    snapshotProbe: async (after = 0) => ({ found: true, sequence: events.length, dropped_through: 0, events: events.filter(row => row.sequence > after) }),
    click: async (locator, options) => {
      options?.beforeDispatch?.();
      emit({ type: "click", ...locator.identity });
      if (locator.identity.action === "send") { turn++; mode = approval ? "approval" : "completed"; }
      if (locator.identity.action === "approve-permission") {
        calls.push({ sequence: calls.length + 1, command: "answer_permission", args: { confirmationId: "42", decision: "approved" } }); mode = "completed";
      }
      if (locator.identity.action === "cancel-run") {
        calls.push({ sequence: calls.length + 1, command: "cancel_run", args: { expectedTarget: structuredClone(surface().projection.stop_target) } }); mode = "cancelled";
      }
      return { identity: locator.identity };
    },
    insertText: async (locator, text) => { value = text; emit({ type: "input", ...locator.identity, inputType: "insertText", data: text }); },
    cleanup: async () => ({}),
  };
  const sink = { writeBytes: async name => ({ path: name }),
    writeJson: async (name, data) => { documents.push({ name, data }); return { path: name }; },
    record: async (name, data) => records.push({ name, data }) };
  const live = new ManualLiveSession({ context: { paths: { workspace } }, sink,
    driver: { evaluate: async () => surface(), screenshot: async () => Buffer.from("test screenshot") },
    options: { approvalMode: "operator", observationTimeoutMs: budgetMs }, owner: "scenario:manual.case3", stem: "case3", now: () => clock.now,
    operatorReview: review });
  live.input = input;
  live.commands = { snapshot: async (after = 0) => ({ sequence: calls.length, calls: calls.filter(row => row.sequence > after) }), remove: async () => ({}) };
  return { live, records, events, calls, documents };
}

test("Case3 canonical stages share the first Send budget, including verification between stages", async () => {
  const stages = manualCase3Stages(await readFile(new URL("../../manual_ST/case3/spec.md", import.meta.url), "utf8"));
  const clock = { now: 5000 };
  const { live, records, events } = fakeSession({ clock });
  await live.send(stages[0].prompt, { stage: stages[0].name });
  clock.now += 400;
  await live.send(stages[1].prompt, { stage: stages[1].name });
  clock.now += 400;
  await live.send(stages[2].prompt, { stage: stages[2].name });
  assert.deepEqual(records.filter(row => row.name.endsWith("-prompt-sent")).map(row => row.data.observation_budget.remaining_ms), [1000, 600, 200]);
  assert.equal(live.observationBudget.snapshot().deadline_ms, 6000);
  clock.now = 6000;
  const before = events.length;
  await assert.rejects(live.send("do not send a fourth request"), error => error.code === "manual-observation-timeout");
  assert.equal(events.length, before);
});

test("native target acquisition precedes the first deadline and cannot dispatch a later Send after expiry", async () => {
  const clock = { now: 5000 };
  const { live, events } = fakeSession({ clock });
  const click = live.input.click;
  live.input.click = async (locator, options) => {
    if (locator.identity.action === "send") clock.now += 600;
    return click(locator, options);
  };
  await live.send("first request");
  assert.equal(live.observationBudget.snapshot().started_at_ms, 5600);
  assert.equal(live.observationBudget.snapshot().deadline_ms, 6600);
  clock.now = 6500;
  await assert.rejects(live.send("second request"), error => error.code === "manual-observation-timeout");
  assert.equal(events.filter(row => row.type === "click" && row.action === "send").length, 1);
});

test("operator review consumes the case budget and an expired approval uses the existing Stop instead", async context => {
  const parent = fileURLToPath(new URL("../../../../project_sandbox/manual-st-harness-self-tests/", import.meta.url));
  await mkdir(parent, { recursive: true });
  const workspace = await mkdtemp(path.join(parent, "budget-"));
  context.after(async () => { assert.equal(path.dirname(workspace), path.resolve(parent)); await rm(workspace, { recursive: true }); });
  for (const elapsed of [700, 1000]) {
    const clock = { now: 5000 };
    const reviewTimeouts = [];
    const { live, calls, documents } = fakeSession({ clock, workspace, approval: true, review: async (_request, options) => {
      reviewTimeouts.push(options.timeoutMs); clock.now += elapsed; return { status: "decided", decision: "approve" };
    } });
    const result = await live.send("canonical request");
    assert.deepEqual(reviewTimeouts, [1000]);
    assert.equal(live.observationBudget.remainingMs(), 1000 - elapsed);
    assert.equal(result.incomplete, elapsed === 1000);
    assert.equal(calls[0].command, elapsed === 1000 ? "cancel_run" : "answer_permission");
    if (elapsed === 1000) assert.equal(result.incompleteReason, "observation-timeout");
    await live.close();
    assert.equal(documents.find(row => row.name.endsWith("observation-budget.json")).data.status,
      elapsed === 1000 ? "timed_out_incomplete" : "within_budget");
  }
});

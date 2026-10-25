import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { manualLiveFixtureConfig } from "../drivers/manual_live_session.mjs";
import {
  normalizeManualCase5HubArguments, parseManualCase5HubRunnerArguments,
  normalizeManualCase5HubConfiguration, takeManualCase5HubCredentials,
  validateManualCase5HubBuild, manualCase5HubRequiresClosedAudit, manualCase5HubTerminalReady,
} from "../drivers/manual_case5_hub_runner.mjs";

const root = path.resolve("case5-runner-test");
const argumentsValue = { taskRoot: root, packageRoot: path.join(root, "package"), scenarioConfigPath: path.join(root, "scenario.json") };
const configuration = { fixture_source: root, provider_base_url: "http://provider.invalid/v1", model: "main-model", api_key_env: "CASE5_UPSTREAM_KEY",
  side_model: "side-model", side_api_key_env: "CASE5_UPSTREAM_KEY", approve_model: "review-model", approve_api_key_env: "CASE5_UPSTREAM_KEY",
  python_executable: process.execPath, access_mode: "auto_review", approval_mode: "operator" };

test("Hub Case5 runner requires explicit portable paths and rejects ambiguous CLI arguments", () => {
  assert.deepEqual(normalizeManualCase5HubArguments(argumentsValue), argumentsValue);
  assert.deepEqual(parseManualCase5HubRunnerArguments(["--task-root", root, "--package-root", argumentsValue.packageRoot, "--scenario-config", argumentsValue.scenarioConfigPath]), argumentsValue);
  assert.deepEqual(parseManualCase5HubRunnerArguments(["--task-root", root, "--package-root", argumentsValue.packageRoot, "--scenario-config", argumentsValue.scenarioConfigPath, "--capture-http-requests", "true"]), { ...argumentsValue, captureHttpRequests: true });
  assert.throws(() => normalizeManualCase5HubArguments({ ...argumentsValue, captureHttpRequests: "true" }), /boolean/);
  assert.throws(() => parseManualCase5HubRunnerArguments(["--capture-http-requests", "yes"]), /true or false/);
  assert.throws(() => normalizeManualCase5HubArguments({ ...argumentsValue, taskRoot: "relative" }), /absolute/);
  assert.throws(() => normalizeManualCase5HubArguments({ ...argumentsValue, packageRoot: `${root}\0` }), /absolute/);
  assert.throws(() => parseManualCase5HubRunnerArguments(["--task-root", root, "--task-root", root]), /unique/);
  assert.throws(() => parseManualCase5HubRunnerArguments(["--scenario", "manual.case1"]), /unique/);
  assert.throws(() => parseManualCase5HubRunnerArguments(["--task-root", root]), /absolute/);
});

test("Hub Case5 accepts a separately installed Hub through an explicit absolute root", () => {
  const hubRoot = path.join(root, "machine", "Hub");
  assert.deepEqual(parseManualCase5HubRunnerArguments([
    "--task-root", root, "--package-root", argumentsValue.packageRoot,
    "--scenario-config", argumentsValue.scenarioConfigPath, "--hub-root", hubRoot,
  ]), { ...argumentsValue, hubRoot });
  assert.throws(() => normalizeManualCase5HubArguments({ ...argumentsValue, hubRoot: "relative" }), /absolute/);
});

test("Hub Case5 keeps explicit role credentials, operator owner and no case deadline", () => {
  const options = normalizeManualCase5HubConfiguration(configuration);
  assert.equal(options.observationTimeoutMs, undefined);
  assert.equal(options.accessMode, "auto_review");
  assert.throws(() => normalizeManualCase5HubConfiguration({ ...configuration, observation_timeout_ms: 7_200_000 }), /unbounded/);
  assert.throws(() => normalizeManualCase5HubConfiguration({ ...configuration, approval_mode: "stop_on_request" }), /operator/);
  const environment = { CASE5_UPSTREAM_KEY: "credential-canary", MOYAI_HTTP_REQUEST_CAPTURE_DIR: root, UNRELATED: "preserved" };
  assert.deepEqual(takeManualCase5HubCredentials(options, environment), { main: "credential-canary", side_chat: "credential-canary", approve: "credential-canary" });
  assert.deepEqual(environment, { UNRELATED: "preserved" });
  assert.throws(() => takeManualCase5HubCredentials(options, {}), /missing or invalid/);
});

test("Hub Case5 carries an explicit fixture context capacity without adding role or deadline overrides", () => {
  const raw = { ...configuration, context_window: 1_000_000 };
  const options = normalizeManualCase5HubConfiguration(raw);
  assert.equal(options.contextWindow, 1_000_000);
  assert.equal(options.scenarioRaw.context_window, 1_000_000);
  assert.equal(options.observationTimeoutMs, undefined);
  const sections = manualLiveFixtureConfig(options).split(/(?=^\[)/m);
  assert.match(sections.find(section => section.startsWith("[model]")), /context_window = 1000000\n/);
  for (const [sectionName, model] of [["side_chat", "side-model"], ["approve", "review-model"]]) {
    const section = sections.find(section => section.startsWith(`[${sectionName}]`));
    assert.match(section, new RegExp(`model = "${model}"`));
    assert.match(section, /api_key_env = "CASE5_UPSTREAM_KEY"/);
    assert.doesNotMatch(section, /context_window/);
  }
  assert.match(sections.find(section => section.startsWith("[permissions]")), /access_mode = "auto_review"/);
  assert.equal(normalizeManualCase5HubConfiguration(configuration).contextWindow, undefined);
});

test("Hub Case5 keeps provider path prefixes and independent role credentials", () => {
  const raw = { ...configuration, provider_base_url: "https://provider.invalid/api/v1/",
    hub_side_provider_base_url: "http://local-provider.invalid:8119/v1",
    hub_approve_provider_base_url: "http://local-provider.invalid:8119/v1/",
    side_api_key_env: "LOCAL_KEY", approve_api_key_env: "LOCAL_KEY" };
  const options = normalizeManualCase5HubConfiguration(raw);
  assert.equal(options.providerBaseUrl, "https://provider.invalid/api/v1");
  assert.equal(options.sideProviderBaseUrl, "http://local-provider.invalid:8119/v1");
  assert.equal(options.approveProviderBaseUrl, options.sideProviderBaseUrl);
  assert.equal(Object.hasOwn(options.scenarioRaw, "hub_side_provider_base_url"), false);
  assert.equal(Object.hasOwn(options.scenarioRaw, "hub_approve_provider_base_url"), false);
  const environment = { case5_upstream_key: "remote-credential", LOCAL_KEY: "local-credential", KEEP: "retained" };
  assert.deepEqual(takeManualCase5HubCredentials(options, environment), { main: "remote-credential", side_chat: "local-credential", approve: "local-credential" });
  assert.deepEqual(environment, { KEEP: "retained" });
  const missing = { LOCAL_KEY: "local-credential", KEEP: "retained" };
  assert.throws(() => takeManualCase5HubCredentials(options, missing), /missing or invalid/);
  assert.deepEqual(missing, { KEEP: "retained" });
  const anonymous = normalizeManualCase5HubConfiguration({ ...raw, side_api_key_env: "", approve_api_key_env: "" });
  assert.deepEqual(takeManualCase5HubCredentials(anonymous, { CASE5_UPSTREAM_KEY: "remote-credential" }), { main: "remote-credential", side_chat: null, approve: null });
  for (const key of ["provider_base_url", "hub_side_provider_base_url", "hub_approve_provider_base_url"]) {
    for (const endpoint of ["https://user:private@provider.invalid/api/v1", "https://provider.invalid/api/v1?key=private", "https://provider.invalid/api/v1#private", "https://provider.invalid/api/v2", "invalid-private"]) {
      assert.throws(() => normalizeManualCase5HubConfiguration({ ...raw, [key]: endpoint }), error => {
        assert.equal(String(error.stack).includes("private"), false);
        return error instanceof TypeError;
      });
    }
  }
});

test("Hub Case5 verifies the exact Hub executable against recorded build metadata", () => {
  const identity = { version: "0.1.0", git_commit: "a".repeat(40), sha256: "b".repeat(64), source_clean: true };
  assert.deepEqual(validateManualCase5HubBuild(identity, identity.sha256), identity);
  assert.throws(() => validateManualCase5HubBuild(identity, "c".repeat(64)), /does not match/);
  assert.throws(() => validateManualCase5HubBuild({ ...identity, git_commit: "unknown" }, identity.sha256), /does not match/);
});

test("Hub Case5 marks a truncated first page and later pages for closed canonical audit", () => {
  const rows = [{ kind: "user" }, { kind: "assistant" }];
  assert.equal(manualCase5HubRequiresClosedAudit({ turn_page_offset: 0, turn_page_total: 2, transcript_rows: rows }), false);
  assert.equal(manualCase5HubRequiresClosedAudit({ turn_page_offset: 0, turn_page_total: 8, transcript_rows: rows }), true);
  assert.equal(manualCase5HubRequiresClosedAudit({ turn_page_offset: 6, turn_page_total: 8, transcript_rows: rows }), true);
  assert.equal(manualCase5HubRequiresClosedAudit({ transcript_rows: rows }), true);
});

function completedSurface() {
  const target = { sessionId: "01ARZ3NDEKTSV4RRFFQ69G5FAV", expectedState: { kind: "idle", latestTurnId: "01ARZ3NDEKTSV4RRFFQ69G5FAW" } };
  return { visible_fatal_count: 0, visible_recoverable_error_count: 0, composer: { count: 1, visible: true, run_target: target }, session_usage: { count: 0 },
    projection: { hub: { status: "connected", main_mode: "hub", main_confirmation: "confirmed", active_main: null, active_side_chat: null, active_approve: null },
      busy: false, post_run_refresh_pending: false, background_mutation_pending: false, pending_async_operations: [], task_activity_state: "idle", agent_tree_active: false,
      run_status_key: "completed", run_target: target, draft_target: { sessionId: target.sessionId }, composer_submit_mode: "new_request", can_submit: true,
      confirmation_visible: false, navigation_loading: false, async_polling_required: true } };
}

test("Hub Case5 allows heartbeat polling only when exact run, Gateway roles and rendered UI are idle", () => {
  assert.equal(manualCase5HubTerminalReady(completedSurface()), true);
  for (const [key, value] of [["busy", true], ["navigation_loading", true], ["background_mutation_pending", true], ["pending_async_operations", ["refresh"]], ["run_status_key", "running"]]) {
    const surface = completedSurface(); surface.projection[key] = value;
    assert.equal(manualCase5HubTerminalReady(surface), false, key);
  }
  const active = completedSurface(); active.projection.hub.active_main = "request";
  assert.equal(manualCase5HubTerminalReady(active), false);
  const changedOwner = completedSurface(); changedOwner.projection.draft_target.sessionId = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
  assert.equal(manualCase5HubTerminalReady(changedOwner), false);
  const staleDom = completedSurface(); staleDom.composer.run_target = null;
  assert.equal(manualCase5HubTerminalReady(staleDom), false);
  assert.equal(manualCase5HubTerminalReady(null), false);
});

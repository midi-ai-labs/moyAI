import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { onboardingImplementationReply, INPUT_NAME, SCRIPT_NAME, RESULT_NAME } from "../fixtures/onboarding_implementation.mjs";
import { createOnboardingWinAbScenario, implementedArtifactsAccepted, approvalVisibleBeforeScroll, latestArtifactVersion, completedRequestorIdentityAccepted, ordinaryChatDraftOwnerReady,
  takeOnboardingUpstreamApiKey, prepareOnboardingHubModels, onboardingHubModelsAccepted, navigateOnboardingApproval } from "../scenarios/onboarding_winab.mjs";

test("B's draft input waits for the new chat workspace and the rendered owner, not just a visible textarea", () => {
  const workspace = path.resolve("isolated/quick-chat-workspace");
  const runTarget = { workspacePath: workspace, sessionId: null, runtimeOwnerToken: "idle:1" };
  const surface = { projection: { workspace_path: workspace, overlay: "none", hub_project_open: false,
    navigation_loading: false, busy: false, background_mutation_pending: false, post_run_refresh_pending: false,
    composer_submit_mode: "new_request", can_submit: true, run_target: runTarget },
    composer: { count: 1, visible: true, run_target: structuredClone(runTarget) }, prompt: { count: 1, visible: true, enabled: true },
    visible_fatal_count: 0, visible_recoverable_error_count: 0 };
  assert.equal(ordinaryChatDraftOwnerReady(surface, workspace), true);
  surface.composer.run_target = { ...runTarget, runtimeOwnerToken: "prior-owner" };
  assert.equal(ordinaryChatDraftOwnerReady(surface, workspace), false);
  surface.composer.run_target = structuredClone(runTarget);
  surface.projection.navigation_loading = true;
  assert.equal(ordinaryChatDraftOwnerReady(surface, workspace), false);
  surface.projection.navigation_loading = false;
  assert.equal(ordinaryChatDraftOwnerReady(surface, path.resolve("old-workspace")), false);
});

test("completed requestor identity comes from the matching status summary, not the detail DTO", () => {
  const expected = { job_id: "job-a", user_id: "actor-a", display_name: "WinA (操作PC)" };
  const requestor = { user_id: expected.user_id, display_name: expected.display_name };
  const detail = { id: expected.job_id, state: "succeeded" };
  const job = { ...detail, requestor };
  const shared = { detail, status: { jobs: [job] } };
  assert.equal(completedRequestorIdentityAccepted(shared, expected), true);
  for (const changed of [
    { ...job, id: "another-job" },
    { ...job, state: "running" },
    { ...job, requestor: { ...requestor, user_id: "another-actor" } },
    { ...job, requestor: { ...requestor, display_name: "Previous PC name" } },
  ]) assert.equal(completedRequestorIdentityAccepted({ ...shared, status: { jobs: [changed] } }, expected), false);
  assert.equal(completedRequestorIdentityAccepted({ detail, status: null }, expected), false);
  assert.equal(completedRequestorIdentityAccepted({ ...shared, detail: { ...detail, id: "another-job" } }, expected), false);
  assert.equal(completedRequestorIdentityAccepted({ detail: { ...detail, requestor }, status: { jobs: [] } }, expected), false);
});
test("live onboarding requires an explicit credential-free endpoint and model before starting any resource", () => {
  const liveProvider = { provider_base_url: "http://localhost:8119/v1", model: "test-model" };
  assert.equal(createOnboardingWinAbScenario({ liveProvider }).id, "onboarding.win-a-to-win-b");
  assert.equal(createOnboardingWinAbScenario({ liveProvider, expectProviderFailure: true }).id, "onboarding.win-a-to-win-b");
  assert.throws(() => createOnboardingWinAbScenario({ expectProviderFailure: true }), TypeError);
  assert.throws(() => createOnboardingWinAbScenario({ liveProvider, expectProviderFailure: "true" }), TypeError);
  for (const patch of [{ provider_base_url: "http://secret:password@localhost:8119" }, { model: "" }, { model: "bad\nmodel" }, { unexpected: true }]) {
    assert.throws(() => createOnboardingWinAbScenario({ liveProvider: { ...liveProvider, ...patch } }), TypeError);
  }
  for (const patch of [{ api_key_env: "MAIN_TEST_KEY" }, { side_model: "Gemma" }, { side_api_key_env: "SIDE_TEST_KEY" }]) {
    assert.throws(() => createOnboardingWinAbScenario({ liveProvider: { ...liveProvider, ...patch } }), TypeError);
  }
});
test("onboarding consumes a private child credential before preparation and keeps the live schema strict", () => {
  const liveProvider = { provider_base_url: "http://localhost:8119/v1", model: "agent-model" };
  const name = "MOYAI_E2E_SYNTHETIC_UPSTREAM_KEY", secret = "synthetic-onboarding-credential-never-real";
  assert.equal(takeOnboardingUpstreamApiKey(undefined, {}), null);
  const environment = { [name.toLowerCase()]: secret, KEEP: "unrelated" };
  assert.equal(takeOnboardingUpstreamApiKey(name, environment), secret);
  assert.deepEqual(environment, { KEEP: "unrelated" });
  for (const name of ["", "KEY-NAME", "KEY\nNAME", 3]) assert.throws(() => takeOnboardingUpstreamApiKey(name, {}), TypeError);
  for (const value of [undefined, "", "  ", "invalid\ncredential"]) {
    const environment = { [name]: value };
    assert.throws(() => takeOnboardingUpstreamApiKey(name, environment), /missing or invalid/);
    assert.equal(Object.hasOwn(environment, name), false);
  }
  assert.throws(() => createOnboardingWinAbScenario({ liveProvider, upstreamApiKeyEnv: name }), /missing or invalid/);
  for (const patch of [{ upstreamApiKeyEnv: name }, { sideModel: "chat" }, { approveModel: "chat" }]) {
    assert.throws(() => createOnboardingWinAbScenario(patch), /require explicit live/);
  }
  for (const patch of [{ sideModel: "" }, { approveModel: "bad\nmodel" }, { unknownOption: true }]) {
    assert.throws(() => createOnboardingWinAbScenario({ liveProvider, ...patch }), TypeError);
  }
  process.env[name] = secret;
  try {
    const scenario = createOnboardingWinAbScenario({ liveProvider, upstreamApiKeyEnv: name, sideModel: "chat-model", approveModel: "chat-model" });
    assert.equal(Object.hasOwn(process.env, name), false);
    assert.equal(JSON.stringify(scenario).includes(secret), false);
  } finally { delete process.env[name]; }
});

function modelPreparationFixture({ failKeyInput = false, deploymentPatch = {}, initialKey = "", retainKeyAfterRegister = false, metadataTools = false, omitRequestedTools = false } = {}) {
  const fields = { "#logical-model": "", "#api-key": initialKey }, keyInputs = [], discoveries = [], registered = [], selectedStandards = [];
  const snapshot = { store: { catalog: { models: [], team_default_model_id: null, team_default_side_model_id: null, team_default_approve_model_id: null } }, deployments: [] };
  let saveCount = 0;
  const locator = (selector, label = null) => ({
    filter: () => locator(selector),
    getByRole: (role, options) => { assert.equal(role, "option"); assert.equal(options.exact, true); return locator(selector, options.name); },
    waitFor: async () => {},
    getAttribute: async name => { assert.equal(name, "value"); return snapshot.store.catalog.models.find(model => model.label === label)?.id ?? null; },
    fill: async value => {
      if (selector === "#api-key") {
        if (failKeyInput) throw new Error(`Playwright fill call log contains ${value}`, { cause: { value } });
        keyInputs.push(value);
      }
      fields[selector] = value;
    },
    selectOption: async value => {
      assert.notEqual(selector, "#logical-model", "the unused grouping select is inside closed registration options");
      fields[selector] = value;
      if (selector.startsWith("#team-standard-")) selectedStandards.push({ selector, value });
    },
    setChecked: async value => { fields[selector] = value; },
    isEnabled: async () => true,
    click: async () => {
      if (selector === "#discover") discoveries.push({ endpoint: fields["#endpoint"], credential: fields["#api-key"] || null });
      if (selector === "#register") {
        assert.equal(fields["#logical-model"], "", "each distinct provider model must create a new logical model");
        const id = `logical-${registered.length + 1}`;
        registered.push({ id, model: fields["#model"], tools: fields["#allow-tools"] });
        snapshot.store.catalog.models.push({ id, label: fields["#label"], capabilities: metadataTools || fields["#allow-tools"] && !omitRequestedTools ? ["tools"] : [] });
        const endpoint = new URL(fields["#endpoint"]);
        endpoint.pathname = endpoint.pathname.replace(/\/+$/, "").replace(/(?:\/v1)+$/, "");
        snapshot.deployments.push({ deployment: { logical_model_id: id, enabled: true,
          provider_model: fields["#model"], provider_profile: fields["#profile"], endpoint: endpoint.href.replace(/\/+$/, ""), ...deploymentPatch } });
        if (!retainKeyAfterRegister) fields["#api-key"] = "";
      }
      if (selector === "#team-standard-save") {
        saveCount++;
        snapshot.store.catalog.team_default_model_id = fields["#team-standard-model"];
        snapshot.store.catalog.team_default_side_model_id = fields["#team-standard-side-model"];
        snapshot.store.catalog.team_default_approve_model_id = fields["#team-standard-approve-model"] ?? null;
      }
    },
  });
  return { page: { locator }, hub: { command: async name => { assert.equal(name, "hub_snapshot"); return structuredClone(snapshot); } },
    keyInputs, discoveries, registered, selectedStandards, saves: () => saveCount };
}

test("public Hub preparation preserves the new-model default in closed options and saves each distinct role model", async () => {
  const providerBaseUrl = "http://localhost:8119/v1", secret = "synthetic-onboarding-credential-never-real";
  for (const providerBaseUrl of ["http://localhost:8119", "http://localhost:8119/v1", "http://localhost:8119/v1/"]) {
    const scripted = modelPreparationFixture();
    assert.deepEqual(await prepareOnboardingHubModels({ ...scripted, providerBaseUrl, mainModel: "scripted-model" }),
      { main: "logical-1", side_chat: "logical-1", approve: null });
    assert.deepEqual(scripted.registered, [{ id: "logical-1", model: "scripted-model", tools: true }]);
    assert.deepEqual(scripted.keyInputs, [""]);
    assert.equal(scripted.selectedStandards.length, 2);
    assert.equal(scripted.saves(), 1);
  }

  const live = modelPreparationFixture();
  const expected = await prepareOnboardingHubModels({ ...live, providerBaseUrl, mainModel: "agent-model", sideModel: "chat-model", approveModel: "chat-model", apiKey: secret });
  assert.deepEqual(expected, { main: "logical-1", side_chat: "logical-2", approve: "logical-2" });
  assert.deepEqual(live.registered, [{ id: "logical-1", model: "agent-model", tools: true }, { id: "logical-2", model: "chat-model", tools: false }]);
  assert.deepEqual(live.keyInputs, [secret, secret], "Hub clears the key after each successful model registration");
  assert.equal(live.discoveries.every(value => value.credential === secret), true);
  assert.deepEqual(live.selectedStandards, [
    { selector: "#team-standard-model", value: "logical-1" },
    { selector: "#team-standard-side-model", value: "logical-2" },
    { selector: "#team-standard-approve-model", value: "logical-2" },
  ]);
  assert.equal(live.saves(), 1);
  assert.equal(JSON.stringify(expected).includes(secret), false);

  const three = modelPreparationFixture();
  assert.deepEqual(await prepareOnboardingHubModels({ ...three, providerBaseUrl, mainModel: "agent", sideModel: "side", approveModel: "approve" }),
    { main: "logical-1", side_chat: "logical-2", approve: "logical-3" });
  assert.deepEqual(three.registered.map(value => value.tools), [true, false, false]);
});

test("role providers isolate host credentials and share only the same endpoint, model and credential", async () => {
  const providerBaseUrl = "https://cloud.invalid/api/v1", localUrl = "http://localhost:8119/v1";
  const secret = "synthetic-main-host-key", otherSecret = "synthetic-other-host-key";
  const local = { providerBaseUrl: localUrl, apiKey: null };
  const fixture = modelPreparationFixture({ initialKey: "synthetic-stale-key", retainKeyAfterRegister: true });
  const expected = await prepareOnboardingHubModels({ ...fixture, providerBaseUrl, mainModel: "same-model", sideModel: "same-model", approveModel: "same-model", apiKey: secret,
    roleProviders: { side_chat: local, approve: { ...local, providerBaseUrl: `${localUrl}/` } } });
  assert.deepEqual(expected, { main: "logical-1", side_chat: "logical-2", approve: "logical-2" });
  assert.deepEqual(fixture.discoveries, [{ endpoint: providerBaseUrl, credential: secret }, { endpoint: localUrl, credential: null }]);
  assert.deepEqual(fixture.keyInputs, [secret, ""], "an anonymous host must clear the prior host's key even if the form retains it");
  assert.equal(fixture.saves(), 1);
  assert.equal(JSON.stringify(expected).includes(secret), false);

  const credentials = modelPreparationFixture();
  assert.deepEqual(await prepareOnboardingHubModels({ ...credentials, providerBaseUrl: localUrl, mainModel: "same-model", sideModel: "same-model", approveModel: "same-model", apiKey: secret,
    roleProviders: { side_chat: { providerBaseUrl: localUrl, apiKey: otherSecret }, approve: { providerBaseUrl: localUrl, apiKey: otherSecret } } }),
  { main: "logical-1", side_chat: "logical-2", approve: "logical-2" });
  assert.deepEqual(credentials.keyInputs, [secret, otherSecret]);
});

test("explicit role endpoints use their own credential or anonymous mode without inheriting the legacy key", async () => {
  const fixture = modelPreparationFixture();
  const secret = "synthetic-legacy-host-key", mainKey = "synthetic-explicit-main-key";
  const expected = await prepareOnboardingHubModels({ ...fixture, providerBaseUrl: "http://legacy.invalid/v1", mainModel: "main", sideModel: "side", approveModel: "approve", apiKey: secret,
    roleProviders: { main: { providerBaseUrl: "https://cloud.invalid/api/v1", apiKey: mainKey }, side_chat: { providerBaseUrl: "http://localhost:8119/v1" } } });
  assert.deepEqual(fixture.discoveries, [
    { endpoint: "https://cloud.invalid/api/v1", credential: mainKey },
    { endpoint: "http://localhost:8119/v1", credential: null },
    { endpoint: "http://legacy.invalid/v1", credential: secret },
  ]);
  assert.deepEqual(expected, { main: "logical-1", side_chat: "logical-2", approve: "logical-3" });
});

test("provider metadata may retain tools for auxiliary roles while Main must be tools-capable", async () => {
  const options = { providerBaseUrl: "http://localhost:8119/v1", mainModel: "agent", sideModel: "chat", approveModel: "chat" };
  const metadata = modelPreparationFixture({ metadataTools: true });
  assert.deepEqual(await prepareOnboardingHubModels({ ...metadata, ...options }), { main: "logical-1", side_chat: "logical-2", approve: "logical-2" });
  const missingMainTools = modelPreparationFixture({ omitRequestedTools: true });
  await assert.rejects(prepareOnboardingHubModels({ ...missingMainTools, ...options }), /requested model and capabilities/);
  assert.equal(missingMainTools.saves(), 0);
});

test("private key input errors discard the original value and cause, and registration mismatch stops before role save", async () => {
  const providerBaseUrl = "http://localhost:8119/v1", secret = "synthetic-onboarding-credential-never-real";
  const failed = modelPreparationFixture({ failKeyInput: true });
  await assert.rejects(prepareOnboardingHubModels({ ...failed, providerBaseUrl, mainModel: "agent", apiKey: secret }), error => {
    assert.equal(error.code, "onboarding-private-key-input");
    assert.equal(error.owner, "harness");
    assert.equal(error.cause, undefined);
    assert.equal(`${error.stack}\n${JSON.stringify(error)}`.includes(secret), false);
    return true;
  });
  assert.equal(failed.registered.length, 0);
  assert.equal(failed.saves(), 0);
  for (const deploymentPatch of [{ provider_model: "different-model" }, { endpoint: "http://localhost:8120" }, { provider_profile: "native" }]) {
    const mismatch = modelPreparationFixture({ deploymentPatch });
    await assert.rejects(prepareOnboardingHubModels({ ...mismatch, providerBaseUrl, mainModel: "agent" }), /requested model and capabilities/);
    assert.equal(mismatch.saves(), 0);
  }
});

test("B adopts Main and Side defaults and explicitly saves the optional Approve logical ID", () => {
  const expected = { main: "agent-id", side_chat: "chat-id", approve: "chat-id" };
  const projection = { status: "connected" };
  for (const [role, id] of Object.entries(expected)) Object.assign(projection, {
    [`${role}_mode`]: "hub", [`${role}_uses_default`]: role !== "approve", [`${role}_confirmation`]: "confirmed",
    [`${role}_review`]: { selection: { preferred_model_id: id, allowed_model_ids: [id] } },
  });
  assert.equal(onboardingHubModelsAccepted(projection, expected), true);
  for (const patch of [
    { side_chat_review: null }, { side_chat_uses_default: false }, { side_chat_confirmation: "unconfirmed" },
    { side_chat_review: { selection: { preferred_model_id: "agent-id", allowed_model_ids: ["agent-id"] } } },
    { approve_review: null }, { approve_uses_default: true }, { approve_confirmation: "unconfirmed" },
    { approve_review: projection.main_review }, { status: "disconnected" },
  ]) assert.equal(onboardingHubModelsAccepted({ ...projection, ...patch }, expected), false);
  assert.equal(onboardingHubModelsAccepted({ ...projection, approve_review: null }, { ...expected, approve: null }), true);
});
test("implementation plan executes its created script and reads actual output before answering", () => {
  const messages = [{ role: "user", content: `.moyai-shared-inputs-example/${INPUT_NAME}` }];
  const next = (id, content) => { if (id) messages.push({ role: "tool", tool_call_id: id, content }); return onboardingImplementationReply(messages); };
  assert.equal(next().delta.tool_calls[0].function.name, "read");
  const patch = next("implementation-read-input", "value\n10\n20\n30").delta.tool_calls[0];
  assert.match(JSON.parse(patch.function.arguments).patch_text, /Import-Csv/);
  assert.equal(next("implementation-script", "success").delta.tool_calls[0].function.name, "shared_publish_artifact");
  assert.equal(next("implementation-publish-script-first", "Saved shared artifact").delta.tool_calls[0].function.name, "apply_patch");
  assert.equal(next("implementation-script-final", "success").delta.tool_calls[0].function.name, "shared_publish_artifact");
  const shell = next("implementation-publish-script-final", "Saved shared artifact").delta.tool_calls[0];
  assert.equal(shell.function.name, "shell"); assert.match(JSON.parse(shell.function.arguments).command, /-File .\/Summarize-Numbers.ps1/);
  assert.equal(next("implementation-execute", "success").delta.tool_calls[0].function.name, "read");
  assert.equal(next("implementation-read-result", "Count: 3\nSum: 60").delta.tool_calls[0].function.name, "shared_publish_artifact");
  assert.equal(next("implementation-publish-result", "Saved shared artifact onboarding-result.md").finish, "stop");
  assert.throws(() => onboardingImplementationReply([...messages.slice(0, -2), { role: "tool", tool_call_id: "implementation-read-result", content: "error" }]), /real script/);
  assert.throws(() => onboardingImplementationReply([...messages.slice(0, -1), { role: "tool", tool_call_id: "implementation-publish-result", content: "error" }]), /not published/);
});
test("the GUI saves the newest published artifact rather than the first matching name", () => {
  const first = { id: "old", kind: "artifact", name: SCRIPT_NAME, version: 1 };
  const latest = { id: "new", kind: "artifact", name: SCRIPT_NAME, version: 2 };
  const input = { id: "input", kind: "input", name: SCRIPT_NAME, version: 9 };
  const unrelated = { id: "other", kind: "artifact", name: RESULT_NAME, version: 8 };
  const assets = [first, input, latest, unrelated];
  assert.equal(latestArtifactVersion(assets, SCRIPT_NAME), latest);
  assert.equal(latestArtifactVersion([...assets].reverse(), SCRIPT_NAME), latest);
  assert.deepEqual(assets, [first, input, latest, unrelated]);
});
test("artifact oracle requires both Hub and B execution hashes to match A's saved files", () => {
  const files = [SCRIPT_NAME, RESULT_NAME].map(name => ({ name, hub_sha256: "a".repeat(64), saved_sha256: "a".repeat(64), execution_sha256: "a".repeat(64), text: "Count: 3\r\nSum: 60\r\n" }));
  assert.equal(implementedArtifactsAccepted(files), true);
  assert.equal(implementedArtifactsAccepted(files.slice(1)), false);
  assert.equal(implementedArtifactsAccepted(files.map((file, index) => index ? file : { ...file, execution_sha256: "b".repeat(64) })), false);
});
test("approval discovery requires the enabled action to be reachable before scrolling", () => {
  const visible = { count: 1, visible: true, enabled: true, center_in_viewport: true, center_in_scroll_clip: true, center_hit: true };
  assert.equal(approvalVisibleBeforeScroll(visible), true);
  for (const patch of [{ count: 0 }, { count: 2 }, { visible: false }, { enabled: false }, { center_in_viewport: false }, { center_in_scroll_clip: false }, { center_hit: false }]) assert.equal(approvalVisibleBeforeScroll({ ...visible, ...patch }), false);
});

function approvalNavigationFixture({ noticePatch = {}, afterPatch = {} } = {}) {
  const visible = { count: 1, visible: true, enabled: true, center_in_viewport: true, center_in_scroll_clip: true, center_hit: true };
  const notice = { ...visible, ...noticePatch }, after = { ...visible, ...afterPatch }, calls = [], records = [];
  const focusObservations = [true, 4, false, { count: 1, focused: true }];
  let clicked = false, events = [];
  const input = {
    observeExactTarget: async target => {
      calls.push({ kind: "observe", target });
      if (target.identity.tag === "A") {
        assert.equal(target.identity.href, "#shared-approval-title");
        return { observation: structuredClone(notice) };
      }
      assert.equal(clicked, true, "the approval action is measured after the real notice click path");
      assert.equal(target.identity.action, "shared-approve");
      assert.equal(target.selector, '.shared-work button[data-action="shared-approve"][data-value="approval-current"]');
      return { observation: structuredClone(after) };
    },
    snapshotProbe: async () => ({ found: true, sequence: events.length, dropped_through: 0, events }),
    click: async (target, options) => {
      assert.equal(target.identity.tag, "A");
      assert.equal(target.identity.href, "#shared-approval-title");
      assert.equal(options.stableHitSamples, 3);
      calls.push({ kind: "click", target }); clicked = true;
      events = ["pointerdown", "pointerup", "click"].map((type, index) => ({
        type, sequence: index + 1, isTrusted: true, ...target.identity, button: 0, buttons: index === 0 ? 1 : 0,
      }));
    },
  };
  const driver = { evaluate: async () => {
    assert.notEqual(focusObservations.length, 0, "no DOM scroll or additional script mutation is allowed");
    return focusObservations.shift();
  } };
  const sink = { record: async (kind, payload) => { records.push({ kind, payload: structuredClone(payload) }); } };
  return { input, driver, sink, visible, after, calls, records };
}

test("an already visible approval action needs no notification navigation", async () => {
  const fixture = approvalNavigationFixture();
  assert.equal(await navigateOnboardingApproval(fixture, "approval-current", fixture.visible), null);
  assert.deepEqual(fixture.calls, []);
  assert.deepEqual(fixture.records, []);
});

test("approval navigation preserves the off-screen observation and proves the same action after a trusted notice click", async () => {
  const fixture = approvalNavigationFixture();
  const before = { ...fixture.visible, center_in_viewport: false, center_in_scroll_clip: false, center_hit: false };
  const original = structuredClone(before);
  const result = await navigateOnboardingApproval(fixture, "approval-current", before);
  assert.deepEqual(before, original);
  assert.equal(approvalVisibleBeforeScroll(before), false);
  assert.deepEqual(result, { notice_before_navigation: fixture.visible, observation_after_navigation: fixture.after });
  assert.deepEqual(fixture.calls.map(call => call.kind), ["observe", "observe", "click", "observe"]);
  assert.deepEqual(fixture.records.map(row => row.kind), ["onboarding-approval-notice-before-navigation",
    "hub-browser-desktop-trusted-click", "onboarding-approval-after-navigation"]);
  assert.equal(fixture.records[2].payload.id, "approval-current");
});

test("an absent, ambiguous or obstructed approval notice is rejected before clicking", async () => {
  for (const noticePatch of [{ count: 0 }, { count: 2 }, { visible: false }, { enabled: false },
    { center_in_viewport: false }, { center_in_scroll_clip: false }, { center_hit: false }]) {
    const fixture = approvalNavigationFixture({ noticePatch });
    await assert.rejects(navigateOnboardingApproval(fixture, "approval-current", { ...fixture.visible, center_hit: false }),
      error => error.owner === "product" && error.code === "onboarding-winab-mismatch");
    assert.equal(fixture.calls.some(call => call.kind === "click"), false);
    assert.equal(fixture.records.length, 1, "retain the failed notice observation");
  }
});

test("a successful notice click alone never accepts a missing or still clipped approval action", async () => {
  for (const afterPatch of [{ count: 0 }, { count: 2 }, { enabled: false }, { center_in_viewport: false },
    { center_in_scroll_clip: false }, { center_hit: false }]) {
    const fixture = approvalNavigationFixture({ afterPatch });
    await assert.rejects(navigateOnboardingApproval(fixture, "approval-current", { ...fixture.visible, center_hit: false }, 20),
      error => error.owner === "product" && error.code === "hub-browser-enrollment-mismatch"
        && error.evidence.last_value.observation.count === fixture.after.count);
    assert.equal(fixture.calls.filter(call => call.kind === "click").length, 1);
    assert.equal(fixture.records.some(row => row.kind === "onboarding-approval-after-navigation"), false);
  }
});

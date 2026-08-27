import assert from "node:assert/strict";
import test from "node:test";

import {
  beginSessionSettingsMutation,
  clearSessionSettings,
  createSessionSettingsState,
  discardSessionSettingsDraft,
  finishSessionSettingsMutation,
  reconcileSessionSettings,
  sameSessionSettingsTarget,
  sessionSettingsApplyEnabled,
  sessionSettingsMutationPending,
  updateSessionSettingsDraft,
  validateSessionSettingsDraft,
  type SessionSettingsDraft,
  type SessionSettingsTarget,
} from "../src/session_settings_state.ts";

const TARGET: SessionSettingsTarget = {
  workspacePath: "C:/workspace-a",
  rootSessionId: "root-a",
  settingsRevision: "9007199254740993",
  configGeneration: "9007199254740994",
  runtimeOwnerToken: "runtime-a",
};

const VALUES: SessionSettingsDraft = {
  baseUrl: "http://127.0.0.1:1234/v1",
  model: "qwen-local",
  providerProfile: "openai_compatible",
  apiKeyEnv: "",
  contextWindow: "32768",
  accessMode: "default",
};

test("session draft owns baseline, dirty state, and typed local validation for one exact root", () => {
  const state = createSessionSettingsState();
  assert.equal(reconcileSessionSettings(state, TARGET, VALUES), false);
  assert.deepEqual(state.owner, TARGET);
  assert.deepEqual(state.baseline, VALUES);
  assert.deepEqual(state.draft, VALUES);
  assert.notEqual(state.baseline, VALUES);
  assert.notEqual(state.draft, VALUES);
  assert.equal(state.dirty, false);
  assert.equal(state.validation?.ok, true);
  assert.equal(sessionSettingsApplyEnabled(state), false);

  assert.equal(updateSessionSettingsDraft(state, TARGET, "model", "next-model"), true);
  assert.equal(state.dirty, true);
  assert.equal(state.validation?.ok, true);
  assert.equal(sessionSettingsApplyEnabled(state), true);

  assert.equal(updateSessionSettingsDraft(state, TARGET, "baseUrl", "file:///provider.sock"), true);
  assert.equal(state.validation?.invalidField, "baseUrl");
  assert.equal(sessionSettingsApplyEnabled(state), false);
  assert.equal(updateSessionSettingsDraft(state, TARGET, "baseUrl", VALUES.baseUrl), true);
  assert.equal(updateSessionSettingsDraft(state, TARGET, "model", VALUES.model), true);
  assert.equal(state.dirty, false);
  assert.equal(sessionSettingsApplyEnabled(state), false);
});

test("validation accepts an inherited local context budget, Rust integer bounds, and the three access modes", () => {
  assert.equal(validateSessionSettingsDraft({ ...VALUES, contextWindow: "" }).ok, true);
  assert.equal(validateSessionSettingsDraft({ ...VALUES, contextWindow: "0" }).invalidField, "contextWindow");
  assert.equal(
    validateSessionSettingsDraft({ ...VALUES, contextWindow: "4294967296" }).invalidField,
    "contextWindow",
  );
  assert.equal(validateSessionSettingsDraft({ ...VALUES, accessMode: "auto_review" }).ok, true);
  assert.equal(validateSessionSettingsDraft({ ...VALUES, accessMode: "full_access" }).ok, true);
  assert.equal(validateSessionSettingsDraft({
    ...VALUES,
    accessMode: "unrestricted" as SessionSettingsDraft["accessMode"],
  }).invalidField, "accessMode");
});

test("clearing a numeric override remains a valid dirty payload for Rust to restore inheritance", () => {
  const state = createSessionSettingsState();
  reconcileSessionSettings(state, TARGET, VALUES);
  assert.equal(updateSessionSettingsDraft(state, TARGET, "contextWindow", ""), true);
  assert.equal(state.validation?.ok, true);
  assert.equal(sessionSettingsApplyEnabled(state), true);
  const request = beginSessionSettingsMutation(state, TARGET);
  assert.ok(request);
  assert.equal(request.draft.contextWindow, "");
});

test("stale input targets cannot edit another root or another settings revision", () => {
  const state = createSessionSettingsState();
  reconcileSessionSettings(state, TARGET, VALUES);
  assert.equal(updateSessionSettingsDraft(
    state,
    { ...TARGET, rootSessionId: "root-b" },
    "model",
    "must-not-leak",
  ), false);
  assert.equal(updateSessionSettingsDraft(
    state,
    { ...TARGET, settingsRevision: "9007199254740994" },
    "model",
    "must-not-leak",
  ), false);
  assert.equal(state.draft?.model, VALUES.model);
  assert.equal(sameSessionSettingsTarget(TARGET, { ...TARGET }), true);
  assert.equal(sameSessionSettingsTarget(TARGET, { ...TARGET, runtimeOwnerToken: "runtime-b" }), false);
});

test("same-root polling retains a dirty draft while a different root cannot inherit it", () => {
  const state = createSessionSettingsState();
  reconcileSessionSettings(state, TARGET, VALUES);
  updateSessionSettingsDraft(state, TARGET, "model", "browser-draft");
  const dirtyRevision = state.draftRevision;

  assert.equal(reconcileSessionSettings(state, { ...TARGET }, {
    ...VALUES,
    model: "poll-value-without-a-new-revision",
  }), true);
  assert.equal(state.draft?.model, "browser-draft");
  assert.equal(state.draftRevision, dirtyRevision);

  const runtimeTarget = { ...TARGET, runtimeOwnerToken: "tree:41" };
  assert.equal(reconcileSessionSettings(state, runtimeTarget, VALUES), true);
  assert.deepEqual(state.owner, runtimeTarget);
  assert.equal(state.draft?.model, "browser-draft");
  assert.equal(state.dirty, true);
  assert.equal(state.draftRevision, dirtyRevision);

  const revisedTarget = { ...TARGET, settingsRevision: "9007199254740994" };
  assert.equal(reconcileSessionSettings(state, revisedTarget, {
    ...VALUES,
    model: "server-rebased",
  }), true);
  assert.deepEqual(state.owner, runtimeTarget, "a changed canonical baseline leaves the draft stale");
  assert.equal(state.dirty, true);
  assert.equal(state.draft?.model, "browser-draft");
  assert.equal(state.baseline?.model, VALUES.model);

  const otherRoot = {
    ...revisedTarget,
    rootSessionId: "root-b",
    settingsRevision: "1",
    runtimeOwnerToken: "runtime-b",
  };
  assert.equal(reconcileSessionSettings(state, otherRoot, {
    ...VALUES,
    model: "root-b-model",
  }), false);
  assert.equal(state.dirty, false);
  assert.equal(state.draft?.model, "root-b-model");
  assert.notEqual(state.draft?.model, "browser-draft");
});

test("mutation settlement is fenced by active token, exact target, and draft revision", () => {
  const state = createSessionSettingsState();
  reconcileSessionSettings(state, TARGET, VALUES);
  assert.equal(beginSessionSettingsMutation(state, TARGET), null, "a clean draft is not applied");

  updateSessionSettingsDraft(state, TARGET, "model", "next-model");
  const first = beginSessionSettingsMutation(state, TARGET);
  assert.ok(first);
  assert.equal(first.token, 1n);
  assert.equal(Object.isFrozen(first.target), true);
  assert.equal(Object.isFrozen(first.draft), true);
  assert.equal(sessionSettingsMutationPending(state), true);
  assert.equal(beginSessionSettingsMutation(state, TARGET), null, "the lane is single-flight");

  updateSessionSettingsDraft(state, TARGET, "model", "edited-during-apply");
  assert.equal(finishSessionSettingsMutation(state, first, {
    succeeded: true,
    target: { ...TARGET, settingsRevision: "9007199254740994" },
    values: { ...VALUES, model: "next-model" },
  }), false);
  assert.equal(state.draft?.model, "edited-during-apply");
  assert.equal(state.dirty, true);
  assert.equal(sessionSettingsMutationPending(state), false);

  const second = beginSessionSettingsMutation(state, TARGET);
  assert.ok(second);
  assert.equal(second.token, 2n);
  assert.equal(finishSessionSettingsMutation(state, second, {
    succeeded: true,
    target: { ...TARGET, rootSessionId: "root-b", settingsRevision: "1" },
    values: { ...VALUES, model: "wrong-root" },
  }), false);
  assert.equal(state.draft?.model, "edited-during-apply");

  const third = beginSessionSettingsMutation(state, TARGET);
  assert.ok(third);
  const committedTarget = {
    ...TARGET,
    settingsRevision: "9007199254740994",
    configGeneration: "9007199254740995",
  };
  const canonicalValues = {
    ...VALUES,
    baseUrl: "http://127.0.0.1:1234/v1",
    model: "edited-during-apply",
    accessMode: "full_access" as const,
  };
  assert.equal(finishSessionSettingsMutation(state, third, {
    succeeded: true,
    target: committedTarget,
    values: canonicalValues,
  }), true);
  assert.deepEqual(state.owner, committedTarget);
  assert.deepEqual(state.baseline, canonicalValues);
  assert.deepEqual(state.draft, canonicalValues);
  assert.equal(state.dirty, false);
  assert.equal(state.validation?.ok, true);
  assert.equal(finishSessionSettingsMutation(state, third, {
    succeeded: true,
    target: committedTarget,
    values: canonicalValues,
  }), false, "a settled token is stale");
});

test("accepted failures preserve the draft, and discard/clear cannot leak it", () => {
  const state = createSessionSettingsState();
  reconcileSessionSettings(state, TARGET, VALUES);
  updateSessionSettingsDraft(state, TARGET, "accessMode", "auto_review");
  const request = beginSessionSettingsMutation(state, TARGET);
  assert.ok(request);
  assert.equal(discardSessionSettingsDraft(state, TARGET), false, "discard waits for the active owner");
  assert.equal(finishSessionSettingsMutation(state, request, {
    succeeded: false,
    target: { ...TARGET },
    values: VALUES,
  }), true);
  assert.equal(state.draft?.accessMode, "auto_review");
  assert.equal(state.dirty, true);
  assert.equal(discardSessionSettingsDraft(state, TARGET), true);
  assert.deepEqual(state.draft, VALUES);
  assert.equal(state.dirty, false);

  clearSessionSettings(state);
  assert.equal(state.owner, null);
  assert.equal(state.baseline, null);
  assert.equal(state.draft, null);
  assert.equal(state.validation, null);
});

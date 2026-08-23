import assert from "node:assert/strict";
import test from "node:test";

import {
  INITIAL_SETUP_STEPS,
  advanceInitialSetupStep,
  beginInitialSetupFinish,
  createInitialSetupState,
  finishInitialSetup,
  initialSetupDiffSummary,
  initialSetupFinishPending,
  reconcileInitialSetupOwner,
  retreatInitialSetupStep,
  validateInitialSetupStep,
} from "../src/initial_setup_state.ts";
import type {
  ConfigFieldProjection,
  ConfigMutationTarget,
  InitialSetupMutationTarget,
} from "../src/types.ts";
import type { ConfigFieldValue } from "../src/utils.ts";

const SETUP_TARGET: InitialSetupMutationTarget = {
  workspacePath: "C:/workspace-a",
  globalConfigPath: "C:/config/moyai.toml",
  setupGeneration: "41",
};

const CONFIG_TARGET: ConfigMutationTarget = {
  workspacePath: "C:/workspace-a",
  sessionId: null,
  configGeneration: "7",
};

const FIELDS: ConfigFieldProjection[] = [
  field("model.base_url", "http://127.0.0.1:1234/v1", "string", true),
  field(
    "model.provider_metadata_mode",
    "lm_studio_native_required",
    "enum",
    true,
    ["lm_studio_native_required", "openai_compatible_only"],
  ),
  field("model.context_window", "32768", "integer", true, [], 1, 4_294_967_295),
  field("model.max_output_tokens", "4096", "integer", true, [], 0, 4_294_967_295),
  field("model.model", "qwen-local", "string", true),
  field(
    "permissions.access_mode",
    "default",
    "enum",
    true,
    ["default", "auto_review", "full_access"],
  ),
  field("docling.enabled", "false", "boolean", true),
  field("docling.base_url", "http://127.0.0.1:5001", "string", true),
  field("mcp.enabled", "false", "boolean", true),
  field("mcp.servers_json", "", "json", false),
  field("inspection.default_max_depth", "4", "integer", true, [], 0, null),
];

test("wizard navigation has six stable steps and only local step validation gates Next", () => {
  assert.deepEqual(INITIAL_SETUP_STEPS, [
    "start",
    "provider",
    "model",
    "permissions",
    "tools",
    "finish",
  ]);
  const state = createInitialSetupState();
  assert.equal(reconcileInitialSetupOwner(state, SETUP_TARGET), false);

  assert.equal(advanceInitialSetupStep(state, FIELDS, values()).ok, true);
  assert.equal(state.step, "provider");

  const invalidProvider = values({ "model.base_url": "file:///private/provider.sock" });
  assert.deepEqual(validateInitialSetupStep("provider", FIELDS, invalidProvider), {
    ok: false,
    invalidKey: "model.base_url",
    message: "URL は http:// または https:// で始めてください。",
  });
  assert.equal(advanceInitialSetupStep(state, FIELDS, invalidProvider).ok, false);
  assert.equal(state.step, "provider");
  assert.equal(advanceInitialSetupStep(state, FIELDS, values()).ok, true);
  assert.equal(state.step, "model");

  assert.equal(advanceInitialSetupStep(
    state,
    FIELDS,
    values({ "model.model": "" }),
  ).invalidKey, "model.model");
  assert.equal(state.step, "model");
  assert.equal(advanceInitialSetupStep(state, FIELDS, values()).ok, true);
  assert.equal(state.step, "permissions");

  assert.equal(advanceInitialSetupStep(
    state,
    FIELDS,
    values({ "permissions.access_mode": "unrestricted" }),
  ).invalidKey, "permissions.access_mode");
  assert.equal(advanceInitialSetupStep(state, FIELDS, values()).ok, true);
  assert.equal(state.step, "tools");

  assert.equal(
    advanceInitialSetupStep(
      state,
      FIELDS,
      values({ "docling.enabled": "false", "docling.base_url": "not-in-use" }),
    ).ok,
    true,
    "disabled optional tools do not require readiness or an active endpoint",
  );
  assert.equal(state.step, "finish");
  assert.equal(retreatInitialSetupStep(state), true);
  assert.equal(state.step, "tools");
});

test("finish validates the complete local config without any network diagnostic input", () => {
  assert.deepEqual(
    validateInitialSetupStep(
      "tools",
      FIELDS,
      values({ "docling.enabled": "true", "docling.base_url": "not-a-url" }),
    ),
    {
      ok: false,
      invalidKey: "docling.base_url",
      message: "URL として解釈できません。",
    },
  );
  assert.deepEqual(
    validateInitialSetupStep(
      "finish",
      FIELDS,
      values({ "inspection.default_max_depth": "" }),
    ),
    {
      ok: false,
      invalidKey: "inspection.default_max_depth",
      message: "値を入力してください。",
    },
    "Finish catches required local fields outside the preceding visible step",
  );
  assert.equal(validateInitialSetupStep("finish", FIELDS, values()).ok, true);
});

test("owner rebase resets navigation and invalidates an in-flight finish without reusing its token", () => {
  const state = createInitialSetupState();
  reconcileInitialSetupOwner(state, SETUP_TARGET);
  state.step = "finish";
  const request = beginInitialSetupFinish(
    state,
    SETUP_TARGET,
    CONFIG_TARGET,
    11n,
    FIELDS,
    values(),
  );
  assert.ok(request);
  assert.equal(request.token, 1n);
  assert.equal(initialSetupFinishPending(state), true);
  assert.equal(reconcileInitialSetupOwner(state, { ...SETUP_TARGET }), true);
  assert.equal(state.step, "finish");

  const rebased = { ...SETUP_TARGET, setupGeneration: "42" };
  assert.equal(reconcileInitialSetupOwner(state, rebased), false);
  assert.equal(state.step, "start");
  assert.equal(initialSetupFinishPending(state), false);
  assert.equal(finishInitialSetup(state, request, CONFIG_TARGET, 11n, {
    succeeded: true,
    setupTarget: null,
    configTarget: { ...CONFIG_TARGET, configGeneration: "8" },
  }), false);

  state.step = "finish";
  const next = beginInitialSetupFinish(
    state,
    rebased,
    CONFIG_TARGET,
    12n,
    FIELDS,
    values(),
  );
  assert.ok(next);
  assert.equal(next.token, 2n);
});

test("finish is single-flight and accepts only the unchanged external draft revision", () => {
  const state = createInitialSetupState();
  reconcileInitialSetupOwner(state, SETUP_TARGET);
  state.step = "finish";

  assert.equal(beginInitialSetupFinish(
    state,
    SETUP_TARGET,
    CONFIG_TARGET,
    4n,
    FIELDS,
    values({ "model.model": "" }),
  ), null);
  const request = beginInitialSetupFinish(
    state,
    SETUP_TARGET,
    CONFIG_TARGET,
    4n,
    FIELDS,
    values(),
  );
  assert.ok(request);
  assert.equal(beginInitialSetupFinish(
    state,
    SETUP_TARGET,
    CONFIG_TARGET,
    4n,
    FIELDS,
    values(),
  ), null);
  assert.equal(finishInitialSetup(state, request, CONFIG_TARGET, 5n, {
    succeeded: true,
    setupTarget: null,
    configTarget: { ...CONFIG_TARGET, configGeneration: "8" },
  }), false);
  assert.deepEqual(state.owner, SETUP_TARGET);

  const retry = beginInitialSetupFinish(
    state,
    SETUP_TARGET,
    CONFIG_TARGET,
    5n,
    FIELDS,
    values(),
  );
  assert.ok(retry);
  const committedConfigTarget = { ...CONFIG_TARGET, configGeneration: "8" };
  assert.equal(finishInitialSetup(state, retry, CONFIG_TARGET, 5n, {
    succeeded: true,
    setupTarget: null,
    configTarget: committedConfigTarget,
  }), true);
  assert.equal(state.owner, null, "a completed setup has no remaining blocking owner");
  assert.equal(finishInitialSetup(state, retry, CONFIG_TARGET, 5n, {
    succeeded: true,
    setupTarget: null,
    configTarget: committedConfigTarget,
  }), false, "a settled token is never accepted twice");
});

test("finish rejects a response for another config owner or a success that remains blocking", () => {
  const state = createInitialSetupState();
  reconcileInitialSetupOwner(state, SETUP_TARGET);
  state.step = "finish";
  const request = beginInitialSetupFinish(
    state,
    SETUP_TARGET,
    CONFIG_TARGET,
    1n,
    FIELDS,
    values(),
  );
  assert.ok(request);
  assert.equal(finishInitialSetup(state, request, CONFIG_TARGET, 1n, {
    succeeded: true,
    setupTarget: null,
    configTarget: {
      ...CONFIG_TARGET,
      workspacePath: "C:/workspace-b",
      configGeneration: "8",
    },
  }), false);
  assert.deepEqual(state.owner, SETUP_TARGET);

  const retry = beginInitialSetupFinish(
    state,
    SETUP_TARGET,
    CONFIG_TARGET,
    1n,
    FIELDS,
    values(),
  );
  assert.ok(retry);
  assert.equal(finishInitialSetup(state, retry, CONFIG_TARGET, 1n, {
    succeeded: true,
    setupTarget: { ...SETUP_TARGET, setupGeneration: "42" },
    configTarget: { ...CONFIG_TARGET, configGeneration: "8" },
  }), false);
  assert.deepEqual(state.owner, SETUP_TARGET);
});

test("failed finish is current only when both setup and config targets remain exact", () => {
  const state = createInitialSetupState();
  reconcileInitialSetupOwner(state, SETUP_TARGET);
  state.step = "finish";
  const staleConfigRequest = beginInitialSetupFinish(
    state,
    SETUP_TARGET,
    CONFIG_TARGET,
    1n,
    FIELDS,
    values(),
  );
  assert.ok(staleConfigRequest);
  assert.equal(finishInitialSetup(
    state,
    staleConfigRequest,
    { ...CONFIG_TARGET, configGeneration: "8" },
    1n,
    {
      succeeded: false,
      setupTarget: SETUP_TARGET,
      configTarget: CONFIG_TARGET,
    },
  ), false, "a poll that rebased config makes the failure stale");

  const currentRequest = beginInitialSetupFinish(
    state,
    SETUP_TARGET,
    CONFIG_TARGET,
    1n,
    FIELDS,
    values(),
  );
  assert.ok(currentRequest);
  assert.equal(finishInitialSetup(state, currentRequest, CONFIG_TARGET, 1n, {
    succeeded: false,
    setupTarget: { ...SETUP_TARGET },
    configTarget: { ...CONFIG_TARGET },
  }), true);
  assert.deepEqual(state.owner, SETUP_TARGET);
});

test("finish diff is derived from the external config draft and baseline in schema order", () => {
  const baseline = values();
  const draft = values({
    "model.model": "new-model",
    "permissions.access_mode": "auto_review",
  });
  assert.deepEqual(initialSetupDiffSummary(FIELDS, baseline, draft), [
    { key: "model.model", before: "qwen-local", after: "new-model" },
    { key: "permissions.access_mode", before: "default", after: "auto_review" },
  ]);

  const state = createInitialSetupState();
  assert.equal("draft" in state, false, "wizard navigation does not duplicate config draft values");
  assert.equal("baseline" in state, false, "wizard navigation does not duplicate config baseline values");
});

function values(overrides: Record<string, string> = {}): ConfigFieldValue[] {
  return FIELDS.map((item) => ({
    key: item.key,
    text: overrides[item.key] ?? item.value,
  }));
}

function field(
  key: string,
  value: string,
  valueType: ConfigFieldProjection["value_type"],
  required: boolean,
  options: string[] = [],
  minValue: number | null = null,
  maxValue: number | null = null,
): ConfigFieldProjection {
  return {
    key,
    value,
    env_override: null,
    value_type: valueType,
    required,
    min_value: minValue,
    max_value: maxValue,
    options,
  };
}

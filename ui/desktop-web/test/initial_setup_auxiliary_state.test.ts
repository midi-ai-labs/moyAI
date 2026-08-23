import assert from "node:assert/strict";
import test from "node:test";

import {
  replaceCompleteConfigDraft,
  type ConfigMutationOwner,
} from "../src/config_mutation.ts";
import {
  beginInitialSetupAuxiliaryRequest,
  createInitialSetupAuxiliaryState,
  finishInitialSetupAuxiliaryRequest,
  initialSetupAuxiliaryPendingKind,
  initialSetupDoclingReadinessVisible,
  initialSetupImportedSourcePath,
  reconcileInitialSetupAuxiliaryState,
  recordInitialSetupDoclingReadinessOwner,
  recordInitialSetupImportedSource,
} from "../src/initial_setup_auxiliary_state.ts";

const setupTarget = {
  workspacePath: "C:/workspace",
  globalConfigPath: "C:/config/config.toml",
  setupGeneration: "3",
};
const configTarget = {
  workspacePath: "C:/workspace",
  sessionId: "session-a",
  configGeneration: "7",
};

test("wizard auxiliary operations are single-flight and exact-target/revision fenced", () => {
  const state = createInitialSetupAuxiliaryState();
  const request = beginInitialSetupAuxiliaryRequest(
    state,
    "import",
    setupTarget,
    configTarget,
    4n,
  );
  assert.ok(request);
  assert.equal(initialSetupAuxiliaryPendingKind(state), "import");
  assert.equal(beginInitialSetupAuxiliaryRequest(
    state,
    "docling_readiness",
    setupTarget,
    configTarget,
    4n,
    "http://127.0.0.1:5001/ready",
  ), null);

  assert.equal(finishInitialSetupAuxiliaryRequest(
    state,
    request,
    setupTarget,
    configTarget,
    5n,
  ), false, "a draft revision change rejects the result");
  assert.equal(initialSetupAuxiliaryPendingKind(state), null, "the stale exact request still releases its lane");

  const changedOwner = beginInitialSetupAuxiliaryRequest(
    state,
    "import",
    setupTarget,
    configTarget,
    5n,
  );
  assert.ok(changedOwner);
  assert.equal(finishInitialSetupAuxiliaryRequest(
    state,
    changedOwner,
    { ...setupTarget, setupGeneration: "4" },
    configTarget,
    5n,
  ), false);
});

test("Docling readiness remains visible only for the admitted draft endpoint and revision", () => {
  const state = createInitialSetupAuxiliaryState();
  const endpoint = "http://127.0.0.1:5001/ready";
  const request = beginInitialSetupAuxiliaryRequest(
    state,
    "docling_readiness",
    setupTarget,
    configTarget,
    9n,
    endpoint,
  );
  assert.ok(request);
  assert.equal(finishInitialSetupAuxiliaryRequest(
    state,
    request,
    setupTarget,
    configTarget,
    9n,
  ), true);
  assert.equal(recordInitialSetupDoclingReadinessOwner(state, request), true);
  assert.equal(initialSetupDoclingReadinessVisible(
    state,
    setupTarget,
    configTarget,
    9n,
    endpoint,
  ), true);
  assert.equal(initialSetupDoclingReadinessVisible(
    state,
    setupTarget,
    configTarget,
    10n,
    endpoint,
  ), false, "an ABA draft edit cannot revive an older diagnostic");
  assert.equal(initialSetupDoclingReadinessVisible(
    state,
    setupTarget,
    configTarget,
    9n,
    "http://127.0.0.1:5002/ready",
  ), false);
});

test("wizard auxiliary presentation is discarded at a setup or config owner barrier", () => {
  const state = createInitialSetupAuxiliaryState();
  recordInitialSetupImportedSource(
    state,
    setupTarget,
    configTarget,
    "C:/imported/config.toml",
  );
  assert.equal(initialSetupImportedSourcePath(state, setupTarget, configTarget), "C:/imported/config.toml");
  reconcileInitialSetupAuxiliaryState(
    state,
    setupTarget,
    { ...configTarget, configGeneration: "8" },
  );
  assert.equal(initialSetupImportedSourcePath(state, setupTarget, configTarget), null);
});

test("read-only import adopts exactly one complete draft without partial mutation", () => {
  const owner: ConfigMutationOwner = {
    configDirty: false,
    configDraftValues: new Map(),
    configDraftBaselineValues: new Map(),
    configDraftTarget: null,
    configDraftRevision: 2n,
    nextConfigMutationGeneration: 1n,
    activeConfigMutationGeneration: null,
  };
  const baseline = [
    { key: "model.model", text: "model-a" },
    { key: "docling.enabled", text: "false" },
  ];
  assert.equal(replaceCompleteConfigDraft(
    owner,
    configTarget,
    baseline,
    [{ key: "model.model", text: "model-b" }],
  ), false);
  assert.equal(owner.configDraftRevision, 2n);
  assert.equal(owner.configDirty, false);

  assert.equal(replaceCompleteConfigDraft(
    owner,
    configTarget,
    baseline,
    [
      { key: "model.model", text: "model-b" },
      { key: "docling.enabled", text: "true" },
    ],
  ), true);
  assert.equal(owner.configDraftRevision, 3n);
  assert.equal(owner.configDirty, true);
  assert.deepEqual(Array.from(owner.configDraftValues), [
    ["model.model", "model-b"],
    ["docling.enabled", "true"],
  ]);
});

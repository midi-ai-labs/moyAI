import assert from "node:assert/strict";
import test from "node:test";

import {
  commandConflictState,
  commandErrorInfo,
  commandInternalState,
} from "../src/command_error.ts";
import {
  beginConfigMutation,
  configDraftAppliesTo,
  configCommandValues,
  configMutationValues,
  discardConfigDraft,
  finishConfigMutation,
  reconcileConfigDraftTarget,
  replaceCompleteConfigDraft,
  updateConfigDraftValue,
} from "../src/config_mutation.ts";
import {
  confirmationFocusIsMeaningful,
  confirmationFocusSelectors,
  isRegularModalOverlay,
  localModalIdentity,
  modalIdentity,
  modalIsOpen,
  nextDialogFocusIndex,
  overlayPrimaryFocusSelectors,
} from "../src/modal_state.ts";
import {
  configCommitEnabled,
  navigationIsIdle,
  quickChatDeleteAction,
  sessionRowCapabilities,
  sessionRowActionAvailable,
} from "../src/navigation_state.ts";
import {
  appliedProjectionRevision,
  projectionUpdateAccepted,
} from "../src/projection_state.ts";
import {
  rowMutationArgs,
  rowMutationTargetStillMatches,
} from "../src/row_target.ts";
import type { DesktopWebState } from "../src/types.ts";
import { humanizeError } from "../src/utils.ts";

function dirtyDraft(draftTarget = target()) {
  return {
    configDirty: true,
    configDraftValues: new Map([["model.model", "draft-value"]]),
    configDraftBaselineValues: new Map([["model.model", "baseline-value"]]),
    configDraftTarget: draftTarget,
    configDraftRevision: 1n,
    nextConfigMutationGeneration: 1n,
    activeConfigMutationGeneration: null as bigint | null,
  };
}

function target(
  workspacePath = "C:/workspace",
  sessionId: string | null = "session-a",
  configGeneration = "1",
) {
  return { workspacePath, sessionId, configGeneration };
}

test("failed config apply retains dirty state and drafts", () => {
  const draft = dirtyDraft();
  const request = beginConfigMutation(draft, target());

  assert.equal(finishConfigMutation(draft, request, false, target(), target()), true);

  assert.equal(draft.configDirty, true);
  assert.deepEqual(Array.from(draft.configDraftValues), [["model.model", "draft-value"]]);
});

test("config commands send changed Approve fields and complete ordinary Main and Sub values", () => {
  const draft = dirtyDraft();
  const baseline = [
    { key: "model.model", text: "main" },
    { key: "side_chat.model", text: "sub" },
    { key: "approve.base_url", text: "https://old.example/v1" },
    { key: "approve.api_key_env", text: "PROVIDER_KEY" },
    { key: "approve.model", text: "judge" },
  ];
  draft.configDraftValues.clear();
  draft.configDraftBaselineValues.clear();
  for (const { key, text } of baseline) {
    draft.configDraftValues.set(key, text);
    draft.configDraftBaselineValues.set(key, text);
  }
  const changed = baseline.map((value) => value.key === "approve.base_url"
    ? { ...value, text: "https://new.example/v1" } : value);
  assert.deepEqual(configCommandValues(draft, target(), changed), changed.slice(0, 3));
  assert.deepEqual(configCommandValues(draft, target("C:/other"), changed), baseline.slice(0, 2));
  const explicitCredential = changed.map((value) => value.key === "approve.api_key_env"
    ? { ...value, text: "ANOTHER_KEY" } : value);
  assert.deepEqual(configCommandValues(draft, target(), explicitCredential), explicitCredential.slice(0, 4));
});

test("Main and Sub commands omit untouched credentials and accept explicit same-key reentry", () => {
  for (const role of ["model", "side_chat"]) {
    const baseline = [{ key: `${role}.base_url`, text: "https://old.example/v1" },
      { key: `${role}.provider_profile`, text: "openai_compatible" },
      { key: `${role}.api_key_env`, text: "OLD_PROVIDER_KEY" },
      { key: "model.extra_headers_json", text: "" }];
    for (const [key, text] of [[`${role}.base_url`, "https://new.example/v1"],
      [`${role}.provider_profile`, "lm_studio"]]) {
      const draft = approvalInputDraft();
      updateConfigDraftValue(draft, target(), baseline, key, text);
      assert.equal(approvalPayload(draft).some(value => value.key === `${role}.api_key_env`), false);
      assert.equal(approvalPayload(draft).some(value => value.key === "model.extra_headers_json"), false);
      updateConfigDraftValue(draft, target(), baseline, `${role}.api_key_env`, "OLD_PROVIDER_KEY");
      assert.deepEqual(approvalPayload(draft).find(value => value.key === `${role}.api_key_env`),
        { key: `${role}.api_key_env`, text: "OLD_PROVIDER_KEY" });
    }
    const draft = approvalInputDraft();
    updateConfigDraftValue(draft, target(), baseline, "model.model", "new-model");
    assert.equal(approvalPayload(draft).some(value => value.key === `${role}.api_key_env`), false);
  }
});

const approvalDraftBaseline = [
  { key: "model.model", text: "main" },
  { key: "approve.base_url", text: "https://old.example/v1" },
  { key: "approve.provider_profile", text: "openai_compatible" },
  { key: "approve.api_key_env", text: "PROVIDER_KEY" },
];

function approvalInputDraft() {
  return { ...dirtyDraft(), configDirty: false, configDraftTarget: null,
    configDraftValues: new Map<string, string>(), configDraftBaselineValues: new Map<string, string>(),
    configDraftEditedKeys: new Set<string>() };
}

function approvalPayload(draft: ReturnType<typeof approvalInputDraft>) {
  return configCommandValues(draft, target(), configMutationValues(draft, target()) ?? []);
}

test("imported Main and Sub connections retain their explicitly imported same-key reference", () => {
  for (const role of ["model", "side_chat"]) {
    const draft = dirtyDraft();
    const baseline = [{ key: `${role}.base_url`, text: "https://old.example.test/v1" },
      { key: `${role}.api_key_env`, text: "OLD_KEY" }];
    const imported = baseline.map(value => value.key.endsWith(".base_url")
      ? { ...value, text: "https://imported.example.test/v1" } : value);
    assert.equal(replaceCompleteConfigDraft(draft, target(), baseline, imported), true);
    assert.equal(configCommandValues(draft, target(), imported).find(value => value.key.endsWith(".api_key_env"))?.text, "OLD_KEY");
  }
});

test("editing an imported connection does not reuse its earlier credential input", () => {
  for (const role of ["model", "side_chat"]) {
    for (const [connectionKey, connectionValue] of [[`${role}.base_url`, "https://other.example.test/v1"],
      [`${role}.provider_profile`, "openai_responses"]]) {
      const draft = approvalInputDraft();
      const baseline = [{ key: `${role}.base_url`, text: "https://old.example.test/v1" },
        { key: `${role}.provider_profile`, text: "openai_compatible" },
        { key: `${role}.api_key_env`, text: "OLD_KEY" }];
      const imported = baseline.map(value => value.key.endsWith(".base_url")
        ? { ...value, text: "https://imported.example.test/v1" }
        : value.key.endsWith(".api_key_env") ? { ...value, text: "IMPORTED_KEY" } : value);
      assert.equal(replaceCompleteConfigDraft(draft, target(), baseline, imported), true);
      updateConfigDraftValue(draft, target(), imported, connectionKey, connectionValue);
      assert.equal(approvalPayload(draft).some(value => value.key === `${role}.api_key_env`), false,
        "Import grants the credential to its imported target, not a subsequently edited target");
      updateConfigDraftValue(draft, target(), imported, `${role}.api_key_env`, "IMPORTED_KEY");
      assert.equal(approvalPayload(draft).find(value => value.key === `${role}.api_key_env`)?.text, "IMPORTED_KEY");
    }
  }
});

test("model-only and canonical URL edits preserve the imported credential intent", () => {
  const draft = approvalInputDraft();
  const baseline = [{ key: "model.base_url", text: "https://old.example.test/v1" },
    { key: "model.model", text: "old-model" }, { key: "model.api_key_env", text: "OLD_KEY" }];
  const imported = [{ key: "model.base_url", text: "https://imported.example.test/v1" },
    { key: "model.model", text: "imported-model" }, { key: "model.api_key_env", text: "IMPORTED_KEY" }];
  assert.equal(replaceCompleteConfigDraft(draft, target(), baseline, imported), true);
  updateConfigDraftValue(draft, target(), imported, "model.model", "next-model");
  updateConfigDraftValue(draft, target(), imported, "model.base_url", " https://imported.example.test/v1/ ");
  assert.equal(approvalPayload(draft).find(value => value.key === "model.api_key_env")?.text, "IMPORTED_KEY");
});

test("Approve same-key reentry is explicit only while its connection changes and survives polling", () => {
  for (const [connectionKey, connectionValue] of [
    ["approve.base_url", "https://new.example/v1"],
    ["approve.provider_profile", "lm_studio"],
  ]) {
    const draft = approvalInputDraft();
    updateConfigDraftValue(draft, target(), approvalDraftBaseline, connectionKey, connectionValue);
    assert.equal(approvalPayload(draft).some(value => value.key === "approve.api_key_env"), false);
    updateConfigDraftValue(draft, target(), approvalDraftBaseline, "approve.api_key_env", "");
    updateConfigDraftValue(draft, target(), approvalDraftBaseline, "approve.api_key_env", "PROVIDER_KEY");
    assert.equal(reconcileConfigDraftTarget(draft, target()), true);
    assert.deepEqual(approvalPayload(draft).find(value => value.key === "approve.api_key_env"),
      { key: "approve.api_key_env", text: "PROVIDER_KEY" });
    const before = approvalPayload(draft);
    updateConfigDraftValue(draft, target(), approvalDraftBaseline.map(value => ({ ...value, text: "stale-poll" })),
      "model.model", "new-main");
    assert.deepEqual(approvalPayload(draft).filter(value => value.key.startsWith("approve.")),
      before.filter(value => value.key.startsWith("approve.")));
  }
  const sameConnection = approvalInputDraft();
  updateConfigDraftValue(sameConnection, target(), approvalDraftBaseline, "model.model", "new-main");
  updateConfigDraftValue(sameConnection, target(), approvalDraftBaseline, "approve.api_key_env", "PROVIDER_KEY");
  assert.equal(approvalPayload(sameConnection).some(value => value.key === "approve.api_key_env"), false);
});

test("Approve input intent is discarded at clean, import, target, and successful settlement boundaries", () => {
  const draft = approvalInputDraft();
  updateConfigDraftValue(draft, target(), approvalDraftBaseline, "approve.api_key_env", "");
  updateConfigDraftValue(draft, target(), approvalDraftBaseline, "approve.api_key_env", "PROVIDER_KEY");
  assert.equal(draft.configDirty, false);
  assert.equal(draft.configDraftEditedKeys.size, 0);
  updateConfigDraftValue(draft, target(), approvalDraftBaseline, "approve.base_url", "https://new.example/v1");
  assert.equal(approvalPayload(draft).some(value => value.key === "approve.api_key_env"), false);
  updateConfigDraftValue(draft, target(), approvalDraftBaseline, "approve.api_key_env", "PROVIDER_KEY");
  const imported = approvalDraftBaseline.map(value => value.key === "approve.base_url"
    ? { ...value, text: "https://import.example/v1" } : value);
  assert.equal(replaceCompleteConfigDraft(draft, target(), approvalDraftBaseline, imported), true);
  assert.equal(draft.configDraftEditedKeys.size, 0);
  assert.equal(approvalPayload(draft).some(value => value.key === "approve.api_key_env"), false);
  updateConfigDraftValue(draft, target(), approvalDraftBaseline, "approve.api_key_env", "PROVIDER_KEY");
  reconcileConfigDraftTarget(draft, target("C:/other"));
  assert.equal(draft.configDraftEditedKeys.size, 0);
  updateConfigDraftValue(draft, target(), approvalDraftBaseline, "approve.base_url", "https://new.example/v1");
  updateConfigDraftValue(draft, target(), approvalDraftBaseline, "approve.api_key_env", "PROVIDER_KEY");
  const request = beginConfigMutation(draft, target());
  finishConfigMutation(draft, request, true, target(), target());
  assert.equal(draft.configDraftEditedKeys.size, 0);
});

test("pending save and a failed settlement preserve newer Approve input intent", () => {
  const draft = approvalInputDraft();
  updateConfigDraftValue(draft, target(), approvalDraftBaseline, "approve.base_url", "https://new.example/v1");
  const request = beginConfigMutation(draft, target());
  updateConfigDraftValue(draft, target(), approvalDraftBaseline, "approve.api_key_env", "");
  updateConfigDraftValue(draft, target(), approvalDraftBaseline, "approve.api_key_env", "PROVIDER_KEY");
  finishConfigMutation(draft, request, true, target(), target());
  assert.equal(approvalPayload(draft).find(value => value.key === "approve.api_key_env")?.text, "PROVIDER_KEY");
  const retry = beginConfigMutation(draft, target());
  finishConfigMutation(draft, retry, false, target(), target());
  assert.equal(approvalPayload(draft).find(value => value.key === "approve.api_key_env")?.text, "PROVIDER_KEY");
  discardConfigDraft(draft);
  assert.equal(draft.configDraftEditedKeys.size, 0);
});

test("cancelled or invalid config import retains dirty state and drafts", () => {
  const cancelled = dirtyDraft();
  const invalid = dirtyDraft(target("C:/provider"));
  const cancelledRequest = beginConfigMutation(cancelled, target());
  const invalidRequest = beginConfigMutation(invalid, target("C:/provider"));

  finishConfigMutation(cancelled, cancelledRequest, false, target(), target());
  finishConfigMutation(invalid, invalidRequest, false, target("C:/provider"), target("C:/provider"));

  assert.equal(cancelled.configDirty, true);
  assert.equal(invalid.configDirty, true);
  assert.equal(cancelled.configDraftValues.get("model.model"), "draft-value");
  assert.equal(invalid.configDraftValues.get("model.model"), "draft-value");
});

test("successful config apply, save, or import clears dirty state and drafts", () => {
  for (const operation of ["apply", "save", "import"]) {
    const draft = dirtyDraft();
    const request = beginConfigMutation(draft, target());

    finishConfigMutation(draft, request, true, target(), target());

    assert.equal(draft.configDirty, false, operation);
    assert.equal(draft.configDraftValues.size, 0, operation);
  }
});

test("config mutation accepts only the latest generation and preserves newer drafts", () => {
  const draft = dirtyDraft();
  const stale = beginConfigMutation(draft, target());
  const latest = beginConfigMutation(draft, target());

  assert.equal(finishConfigMutation(draft, stale, true, target(), target()), false);
  assert.equal(draft.configDirty, true);

  updateConfigDraftValue(
    draft,
    target(),
    [{ key: "model.model", text: "draft-value" }],
    "model.model",
    "newer-value",
  );
  assert.equal(finishConfigMutation(draft, latest, true, target(), target()), true);
  assert.equal(draft.configDirty, true);
  assert.equal(draft.configDraftValues.get("model.model"), "newer-value");
});

test("config mutation rejects a response after workspace, session, or generation changes", () => {
  const draft = dirtyDraft();
  const request = beginConfigMutation(draft, target());

  assert.equal(finishConfigMutation(draft, request, true, target(), target("C:/other")), false);
  assert.equal(draft.configDirty, true);
  assert.equal(draft.activeConfigMutationGeneration, null);

  for (const changedTarget of [target("C:/workspace", "session-b"), target("C:/workspace", "session-a", "2")]) {
    const nextDraft = dirtyDraft();
    const nextRequest = beginConfigMutation(nextDraft, target());
    assert.equal(finishConfigMutation(nextDraft, nextRequest, true, target(), changedTarget), false);
    assert.equal(nextDraft.configDirty, true);
  }
});

test("config draft is discarded at a target barrier and cannot reappear after ABA navigation", () => {
  const draft = dirtyDraft();
  const targetA = target();
  const targetB = target("C:/workspace", "session-b");

  assert.equal(configDraftAppliesTo(draft, targetA), true);
  assert.equal(reconcileConfigDraftTarget(draft, targetB), false);
  assert.equal(draft.configDirty, false);
  assert.equal(draft.configDraftValues.size, 0);
  assert.equal(draft.configDraftTarget, null);

  assert.equal(reconcileConfigDraftTarget(draft, targetA), true);
  assert.equal(configDraftAppliesTo(draft, targetA), false, "the abandoned A draft must not return");
});

test("config mutation admission drops a draft owned by another target", () => {
  const draft = dirtyDraft();
  const nextTarget = target("C:/workspace", "session-b", "2");

  const request = beginConfigMutation(draft, nextTarget);

  assert.deepEqual(request.target, nextTarget);
  assert.equal(draft.configDirty, false);
  assert.equal(draft.configDraftValues.size, 0);
  assert.equal(draft.configDraftTarget, null);
});

test("config draft edit binds to its creation target and same-target failures retain it", () => {
  const draft = dirtyDraft();
  const current = target("C:/workspace", "session-a", "2");

  reconcileConfigDraftTarget(draft, current);
  updateConfigDraftValue(
    draft,
    current,
    [{ key: "model.model", text: "draft-value" }],
    "model.model",
    "generation-two",
  );
  const request = beginConfigMutation(draft, current);

  assert.equal(finishConfigMutation(draft, request, false, current, current), true);
  assert.equal(configDraftAppliesTo(draft, current), true);
  assert.equal(draft.configDraftValues.get("model.model"), "generation-two");
});

test("config mutation payload survives closing settings and remains draft-owned", () => {
  const draft = {
    configDirty: false,
    configDraftValues: new Map<string, string>(),
    configDraftBaselineValues: new Map<string, string>(),
    configDraftTarget: null,
    configDraftRevision: 0n,
    nextConfigMutationGeneration: 1n,
    activeConfigMutationGeneration: null as bigint | null,
  };
  const current = target();

  updateConfigDraftValue(
    draft,
    current,
    [
      { key: "model.model", text: "original" },
      { key: "permissions.access_mode", text: "default" },
    ],
    "model.model",
    "edited-after-close",
  );

  assert.deepEqual(configMutationValues(draft, current), [
    { key: "model.model", text: "edited-after-close" },
    { key: "permissions.access_mode", text: "default" },
  ]);
  const request = beginConfigMutation(draft, current);
  assert.equal(finishConfigMutation(draft, request, false, current, current), true);
  assert.equal(configMutationValues(draft, current)?.[0].text, "edited-after-close");
});

test("row mutation args retain stable owner and reject an index reused by another row", () => {
  const state = rowState("session-a", ["session-a", "session-b"]);
  const args = rowMutationArgs(state, 1, state.session_rows[1].session_id);
  assert.ok(args);
  assert.equal(args.expectedTarget.rowId, "session-b");
  assert.equal(args.expectedTarget.ownerSessionId, "session-a");

  state.session_rows[1].session_id = "session-c";
  assert.equal(
    rowMutationTargetStillMatches(state, args.expectedTarget, state.session_rows[1].session_id),
    false,
  );

  state.session_rows[1].session_id = "session-b";
  state.session_rows[0].session_id = "session-new-owner";
  assert.equal(
    rowMutationTargetStillMatches(state, args.expectedTarget, state.session_rows[1].session_id),
    false,
  );
});

test("row payload admission is independent from palette selected-session admission", () => {
  const state = rowState("missing-owner", ["external-running"]);
  Object.assign(state, {
    busy: false,
    background_mutation_pending: false,
    navigation_loading: false,
  });
  assert.equal(
    sessionRowActionAvailable(state.session_rows.length, state.selected_session_index, -1),
    false,
    "palette needs a selected session",
  );
  assert.equal(
    sessionRowActionAvailable(state.session_rows.length, state.selected_session_index, 0),
    true,
    "the visible external row owns its own admission payload",
  );
  assert.equal(
    rowMutationArgs(state, 0, state.session_rows[0].session_id)?.expectedTarget.ownerSessionId,
    null,
  );
});

test("session row capabilities use row state rather than the global archived-search flag", () => {
  assert.deepEqual(sessionRowCapabilities("active", false), {
    rejoinAction: "rejoin-session",
    secondaryAction: "interrupt-session",
    rollbackAction: "",
    deleteAction: "",
  });
  for (const loadedStatus of ["idle", "not_loaded", "system_error"]) {
    assert.deepEqual(sessionRowCapabilities(loadedStatus, true), {
      rejoinAction: "",
      secondaryAction: "unarchive-session",
      rollbackAction: "rollback-session",
      deleteAction: "delete-session",
    }, loadedStatus);
  }
  assert.deepEqual(sessionRowCapabilities("active", true), {
    rejoinAction: "rejoin-session",
    secondaryAction: "unarchive-session",
    rollbackAction: "",
    deleteAction: "",
  });
  assert.equal(quickChatDeleteAction("active"), "");
  assert.equal(quickChatDeleteAction("not_loaded"), "delete-chat-session");
});

test("settings commit requires a draft except during initial setup", () => {
  assert.equal(configCommitEnabled(false, false, false), false);
  assert.equal(configCommitEnabled(false, true, false), true);
  assert.equal(configCommitEnabled(true, false, false), true);
  assert.equal(configCommitEnabled(true, true, true), false);
});

test("typed conflict carries a refresh projection while other errors stay outside conflict recovery", () => {
  const state = rowState("session-a", ["session-a"]);
  state.projection_revision = "8";
  const conflict = { kind: "conflict", message: "row changed", state };

  assert.equal(commandConflictState(conflict), state);
  assert.equal(commandConflictState(JSON.stringify(conflict))?.projection_revision, "8");
  assert.equal(commandConflictState({ kind: "internal", message: "bug", state }), null);
  assert.equal(commandInternalState({ kind: "internal", message: "bug", state }), state);
  assert.equal(commandInternalState(conflict), null);
  assert.equal(commandConflictState("transport closed"), null);
});

test("successful config settlement accepts the correlated post-mutation target that polling already applied", () => {
  const draft = dirtyDraft();
  const request = beginConfigMutation(draft, target());
  const settled = target("C:/workspace", "session-a", "2");

  assert.equal(finishConfigMutation(draft, request, true, settled, settled), true);
  assert.equal(draft.configDirty, false);
  assert.equal(draft.configDraftValues.size, 0);
});

test("config settlement cannot clear a draft after a target newer than its response won", () => {
  const draft = dirtyDraft();
  const request = beginConfigMutation(draft, target());
  const settled = target("C:/workspace", "session-a", "2");
  const newer = target("C:/workspace", "session-a", "3");

  assert.equal(finishConfigMutation(draft, request, true, settled, newer), false);
  assert.equal(draft.configDirty, true);
});

test("unknown and storage errors with provider model access keywords stay generic", () => {
  const message = "storage connection refused while loading model 404: access denied";
  for (const error of [
    message,
    { kind: "internal", category: "storage", code: "storage_failure", message },
    { kind: "internal", category: "unknown", code: "unknown", message },
  ]) {
    const human = humanizeError(error);
    assert.equal(human.title, "処理に失敗しました");
    assert.equal(human.details, message);
  }
});

test("typed command error codes select guidance without inspecting the message", () => {
  const opaque = "opaque diagnostic";
  assert.equal(humanizeError({ code: "provider_transport", message: opaque }).title, "AIに接続できません");
  assert.equal(humanizeError({ code: "model_unavailable", message: opaque }).title, "指定したモデルが見つかりません");
  assert.equal(humanizeError({ code: "image_unsupported", message: opaque }).title, "このモデルは画像入力に対応していません");
  assert.equal(humanizeError({ code: "permission_policy_denied", message: opaque }).title, "操作が許可されませんでした");
  assert.deepEqual(commandErrorInfo(JSON.stringify({
    kind: "internal",
    category: "runtime",
    code: "runtime_failure",
    message: opaque,
  })), {
    kind: "internal",
    category: "runtime",
    code: "runtime_failure",
    message: opaque,
  });
});

test("team setup failure has visible installation guidance without promoting arbitrary process output", () => {
  const diagnostic = "private process stderr: a-token-or-private-path";
  const human = humanizeError({ kind: "internal", category: "runtime", code: "team_setup_unavailable", message: diagnostic });
  assert.equal(human.title, "チーム管理を起動できません");
  assert.match(human.hint, /Hub同梱版を導入/);
  assert.match(human.hint, /既存の管理PCでHubを起動/);
  assert.ok(!human.hint.includes(diagnostic));
  assert.equal(human.details, diagnostic);
});

test("a repeated-click conflict wins over the earlier command response by Rust revision", () => {
  const firstClickState = rowState("session-a", ["session-a"]);
  firstClickState.projection_revision = "21";
  const repeatedClickState = rowState("session-a", ["session-a"]);
  repeatedClickState.projection_revision = "22";
  const conflictState = commandConflictState({
    kind: "conflict",
    message: "row changed",
    state: repeatedClickState,
  });
  assert.ok(conflictState);

  let revision = "0";
  assert.equal(projectionUpdateAccepted(revision, conflictState.projection_revision, false), true);
  revision = appliedProjectionRevision(revision, conflictState.projection_revision);
  assert.equal(
    projectionUpdateAccepted(revision, firstClickState.projection_revision, false),
    false,
    "a delayed success from the first click cannot roll back the conflict refresh",
  );
});

test("regular modal detection excludes menu popovers and contains focus cyclically", () => {
  assert.equal(isRegularModalOverlay("provider"), true);
  const hubMain = { hub_project_open: true, confirmation_visible: false, overlay: "none" };
  assert.equal(modalIsOpen(hubMain, false), false, "a Hub project does not trap ordinary main focus");
  assert.equal(modalIsOpen({ ...hubMain, overlay: "hub" }, false), true, "its connection popup still owns modal focus");
  assert.equal(isRegularModalOverlay("shortcuts"), true);
  assert.equal(isRegularModalOverlay("about"), true);
  assert.equal(isRegularModalOverlay("file_menu"), false);
  assert.equal(modalIsOpen({ confirmation_visible: false, overlay: "config" }, false), true);
  assert.equal(modalIsOpen({ confirmation_visible: false, overlay: "none" }, true), true);
  assert.equal(modalIsOpen({ confirmation_visible: false, overlay: "none" }, false), false);

  assert.equal(nextDialogFocusIndex(-1, 3, false), 0);
  assert.equal(nextDialogFocusIndex(2, 3, false), 0);
  assert.equal(nextDialogFocusIndex(0, 3, true), 2);
  assert.equal(nextDialogFocusIndex(1, 3, true), 0);
  assert.equal(nextDialogFocusIndex(-1, 0, false), -1);

  assert.deepEqual(overlayPrimaryFocusSelectors("shortcuts"), [
    ".modal button:not(:disabled)",
    ".modal[role='dialog']",
  ]);
  assert.deepEqual(overlayPrimaryFocusSelectors("about"), [
    ".modal button:not(:disabled)",
    ".modal[role='dialog']",
  ]);
  assert.deepEqual(overlayPrimaryFocusSelectors("provider"), ["#provider-url"]);
  assert.deepEqual(overlayPrimaryFocusSelectors("none"), []);
});

test("local modal identity preserves the exact side-chat delete owner and local-confirm priority", () => {
  const sideTarget = {
    ownerSessionId: "session:a",
    chatId: "side:b",
    expectedGeneration: "7",
  };
  assert.equal(
    localModalIdentity(false, sideTarget),
    'side-chat-delete:["session:a","side:b","7"]',
  );
  assert.equal(
    modalIsOpen(
      { confirmation_visible: false, overlay: "none" },
      localModalIdentity(false, sideTarget) !== null,
    ),
    true,
  );
  assert.equal(localModalIdentity(true, sideTarget), "local-confirm");
  assert.equal(localModalIdentity(false, null), null);
});

test("permission modal identity changes by request without changing outer modal lifecycle", () => {
  const requestA = { confirmation_visible: true, confirmation_id: "A", overlay: "none" };
  const requestB = { confirmation_visible: true, confirmation_id: "B", overlay: "none" };
  assert.equal(modalIdentity(requestA), "permission:A");
  assert.equal(modalIdentity(requestB), "permission:B");
  assert.notEqual(modalIdentity(requestA), modalIdentity(requestB));
  assert.equal(modalIdentity({ confirmation_visible: false, confirmation_id: null, overlay: "config" }), "config");
  assert.equal(modalIsOpen(requestA, false), true);
  assert.equal(modalIsOpen(requestB, false), true);
});

test("pending permission focus targets the live status instead of disabled actions", () => {
  assert.deepEqual(confirmationFocusSelectors(true), [".permission-decision-status"]);
  assert.deepEqual(confirmationFocusSelectors(false), [
    ".modal-actions button[autofocus]:not(:disabled)",
    ".modal-actions button:not(:disabled)",
    ".permission-decision-status",
  ]);
  assert.equal(confirmationFocusIsMeaningful(true, true), true);
  assert.equal(confirmationFocusIsMeaningful(false, true), false);
  assert.equal(confirmationFocusIsMeaningful(false, false), true);
});

test("navigation admission consumes the single Rust capability projection", () => {
  assert.equal(navigationIsIdle({ navigation_admission_open: true }), true);
  assert.equal(navigationIsIdle({ navigation_admission_open: false }), false);
});

test("reverting every field to its baseline automatically clears dirty state", () => {
  const draft = {
    configDirty: false,
    configDraftValues: new Map<string, string>(),
    configDraftBaselineValues: new Map<string, string>(),
    configDraftTarget: null,
    configDraftRevision: 0n,
    nextConfigMutationGeneration: 1n,
    activeConfigMutationGeneration: null as bigint | null,
  };
  const baseline = [{ key: "model.model", text: "model-a" }];

  updateConfigDraftValue(draft, target(), baseline, "model.model", "model-b");
  assert.equal(draft.configDirty, true);
  updateConfigDraftValue(draft, target(), baseline, "model.model", "model-a");

  assert.equal(draft.configDirty, false);
  assert.equal(draft.configDraftTarget, null);
  assert.equal(draft.configDraftValues.size, 0);
  assert.equal(draft.configDraftBaselineValues.size, 0);
});

test("discard resets an invalid settings draft without committing it", () => {
  const draft = dirtyDraft();

  discardConfigDraft(draft);

  assert.equal(draft.configDirty, false);
  assert.equal(draft.configDraftTarget, null);
  assert.equal(draft.configDraftValues.size, 0);
  assert.equal(draft.configDraftBaselineValues.size, 0);
});

function rowState(ownerSessionId: string, sessionIds: string[]): DesktopWebState {
  return {
    workspace_path: "C:/workspace",
    project_rows: [{ project_id: "project-a", label: "A", path: "C:/workspace" }],
    selected_project_index: 0,
    session_rows: sessionIds.map((sessionId) => ({
      session_id: sessionId,
      loaded_status: "idle",
      archived: false,
    })),
    selected_session_index: sessionIds.indexOf(ownerSessionId),
  } as DesktopWebState;
}

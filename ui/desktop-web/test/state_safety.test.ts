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
  configMutationValues,
  discardConfigDraft,
  finishConfigMutation,
  reconcileConfigDraftTarget,
  updateConfigDraftValue,
} from "../src/config_mutation.ts";
import {
  confirmationFocusSelectors,
  isRegularModalOverlay,
  modalIdentity,
  modalIsOpen,
  nextDialogFocusIndex,
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
  assert.equal(humanizeError({ code: "provider_transport", message: opaque }).title, "LLM provider に接続できません");
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
  assert.equal(isRegularModalOverlay("shortcuts"), true);
  assert.equal(isRegularModalOverlay("file_menu"), false);
  assert.equal(modalIsOpen({ confirmation_visible: false, overlay: "config" }, false), true);
  assert.equal(modalIsOpen({ confirmation_visible: false, overlay: "none" }, true), true);
  assert.equal(modalIsOpen({ confirmation_visible: false, overlay: "none" }, false), false);

  assert.equal(nextDialogFocusIndex(-1, 3, false), 0);
  assert.equal(nextDialogFocusIndex(2, 3, false), 0);
  assert.equal(nextDialogFocusIndex(0, 3, true), 2);
  assert.equal(nextDialogFocusIndex(1, 3, true), 0);
  assert.equal(nextDialogFocusIndex(-1, 0, false), -1);
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

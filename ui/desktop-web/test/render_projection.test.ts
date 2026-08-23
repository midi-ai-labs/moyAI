import assert from "node:assert/strict";
import test from "node:test";

import {
  createDesktopRenderModel,
  desktopRenderModelChanged,
  desktopRenderRequired,
  type DesktopRenderLocalPresentation,
} from "../src/render_projection.ts";
import type { DesktopViewState } from "../src/types.ts";

function view(): DesktopViewState {
  return {
    projection_revision: "1",
    status_code: "plain",
    token_meter_title: "10 / 100",
    transcript_rows: [{
      row_kind: "assistant",
      stable_history_identity: "history:1",
      step: "",
      title: "Assistant",
      body: "before",
      file_changes: [],
    }],
    current_turn_agent_activity_rows: [],
  } as DesktopViewState;
}

function localPresentation(): DesktopRenderLocalPresentation {
  return {
    artifactPane: {
      collapsed: false,
      mode: "output",
      selectedAgentPath: null,
      selectedAgentExecution: null,
    },
    attachmentTrayOpen: false,
    configMutationPending: false,
    sideChat: {
      draft: "",
      setupBaseUrl: "",
      setupModel: "",
      catalog: {
        status: "idle",
        source: "none",
        ownerSessionId: null,
        baseUrl: "",
        models: [],
        error: "",
      },
      catalogLoadEnabled: false,
      mutationPending: false,
      operationsOpen: true,
      deleteConfirmation: null,
    },
    modal: {
      localConfirmation: null,
      localDecisionPending: false,
      localDecisionError: "",
      permissionDecision: null,
    },
    recoverableError: null,
    windowMaximized: false,
  };
}

test("ordering revision alone does not invalidate the rendered projection", () => {
  const previousView = view();
  const previous = createDesktopRenderModel(previousView, localPresentation());
  const next = createDesktopRenderModel(
    { ...previousView, projection_revision: "2" },
    localPresentation(),
  );

  assert.equal(desktopRenderModelChanged(previous, next), false);
  assert.equal(desktopRenderRequired(previous, next, false), false);
});

test("same-length transcript content changes invalidate the rendered projection", () => {
  const previousView = view();
  const previous = createDesktopRenderModel(previousView, localPresentation());
  const next = createDesktopRenderModel({
    ...previousView,
    projection_revision: "2",
    transcript_rows: [{ ...previousView.transcript_rows[0], body: "after" }],
  }, localPresentation());

  assert.equal(desktopRenderModelChanged(previous, next), true);
});

test("previously omitted scalar and nested fields invalidate the rendered projection", () => {
  const previousView = view();
  const previous = createDesktopRenderModel(previousView, localPresentation());

  assert.equal(
    desktopRenderModelChanged(
      previous,
      createDesktopRenderModel(
        { ...previousView, status_code: "user_stopped" },
        localPresentation(),
      ),
    ),
    true,
  );
  assert.equal(
    desktopRenderModelChanged(
      previous,
      createDesktopRenderModel(
        { ...previousView, token_meter_title: "20 / 100" },
        localPresentation(),
      ),
    ),
    true,
  );
  assert.equal(
    desktopRenderModelChanged(
      previous,
      createDesktopRenderModel({
        ...previousView,
        current_turn_agent_activity_rows: [{
          agent_path: "/root/child",
          session_id: "child-session",
          task_name: "child",
          task_preview: "work",
          status: "running",
          current_activity: "editing",
          result_preview: "",
          started_order: 1,
          updated: true,
          active_turn_id: "turn-1",
          interrupt_target: {
            workspacePath: "C:/workspace",
            rootSessionId: "root-session",
            agentPath: "/root/worker",
            childSessionId: "agent-session",
            expectedTurnId: "turn-1",
            admissionRevision: "1",
          },
        }],
      }, localPresentation()),
    ),
    true,
  );
  const futureViewField = Object.assign({ ...previousView }, {
    future_render_field: "automatically included",
  }) as DesktopViewState;
  assert.equal(
    desktopRenderModelChanged(
      previous,
      createDesktopRenderModel(futureViewField, localPresentation()),
    ),
    true,
  );
});

test("local presentation values participate in the same render identity", () => {
  const state = view();
  const previous = createDesktopRenderModel(state, localPresentation());
  const changedValues: DesktopRenderLocalPresentation[] = [
    { ...localPresentation(), attachmentTrayOpen: true },
    { ...localPresentation(), configMutationPending: true },
    {
      ...localPresentation(),
      artifactPane: { ...localPresentation().artifactPane, collapsed: true },
    },
    {
      ...localPresentation(),
      sideChat: { ...localPresentation().sideChat, draft: "local side-chat draft" },
    },
    {
      ...localPresentation(),
      modal: { ...localPresentation().modal, localDecisionError: "failed" },
    },
    { ...localPresentation(), windowMaximized: true },
  ];

  for (const local of changedValues) {
    assert.equal(
      desktopRenderModelChanged(previous, createDesktopRenderModel(state, local)),
      true,
    );
  }
});

test("the local model snapshots values and rejects non-presentation baggage", () => {
  const sourceView = view();
  const mutableSideChat = { ...localPresentation().sideChat };
  const source = Object.assign({ ...localPresentation(), sideChat: mutableSideChat }, {
    focusContinuation: { marker: "must-not-render" },
    mutableOwners: new Map([["marker", "must-not-render"]]),
    pendingRequest: Promise.resolve("must-not-render"),
  });
  const model = createDesktopRenderModel(sourceView, source);
  const comparisonKey = model.comparisonKey;

  mutableSideChat.draft = "mutated after snapshot";
  sourceView.status_code = "user_stopped";
  sourceView.transcript_rows[0].body = "nested mutation after snapshot";
  sourceView.transcript_rows.push({
    ...sourceView.transcript_rows[0],
    stable_history_identity: "history:2",
  });

  assert.equal(model.local.sideChat.draft, "");
  assert.equal(model.view.status_code, "plain");
  assert.equal(model.view.transcript_rows.length, 1);
  assert.equal(model.view.transcript_rows[0].body, "before");
  assert.equal(model.comparisonKey, comparisonKey);
  assert.equal(model.comparisonKey.includes("must-not-render"), false);
  assert.equal(Object.isFrozen(model), true);
  assert.equal(Object.isFrozen(model.view), true);
  assert.equal(Object.isFrozen(model.view.transcript_rows), true);
  assert.equal(Object.isFrozen(model.view.transcript_rows[0]), true);
  assert.equal(Object.isFrozen(model.local.sideChat), true);
});

test("force render is an explicit override for imperative render-phase work", () => {
  const state = view();
  const previous = createDesktopRenderModel(state, localPresentation());
  const identical = createDesktopRenderModel(state, localPresentation());

  assert.equal(desktopRenderRequired(previous, identical, false), false);
  assert.equal(desktopRenderRequired(previous, identical, true), true);
  assert.equal(desktopRenderRequired(null, identical, false), true);
});

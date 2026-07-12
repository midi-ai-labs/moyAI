import assert from "node:assert/strict";
import test from "node:test";

import { actionById, paletteActions, type ActionContext } from "../src/actions.ts";
import { updateConfigDraftValue } from "../src/config_mutation.ts";
import {
  InteractionLifecycle,
  shouldBeginKeyboardInteraction,
  shouldBeginPointerInteraction,
} from "../src/interaction_lifecycle.ts";
import { globalShortcutAction } from "../src/keyboard_shortcut.ts";
import { autoRefreshAllowed } from "../src/polling_state.ts";
import {
  renderArtifactPane,
  renderOverlay,
  renderSidebar,
  renderThreadContent,
  renderTopbar,
  setRenderContext,
} from "../src/render.ts";
import type { DesktopWebState } from "../src/types.ts";
import { createUiLocalState } from "../src/ui_state.ts";
import { validateConfigInput } from "../src/utils.ts";
import {
  acknowledgeDraftMutation,
  captureDraftMutation,
  configDraftEditOpen,
  deriveUiCapabilities,
  projectViewState,
  reconcileUiDrafts,
  rejectDraftMutation,
  sessionSearchMutationTarget,
} from "../src/view_state.ts";

function projection(overrides: Partial<DesktopWebState> = {}): DesktopWebState {
  return {
    projection_revision: "1",
    workspace_path: "C:/workspace",
    selected_session_title: "Session A",
    current_session_label: "Session A",
    status_message: "Ready",
    status_detail: "",
    access_label: "default",
    access_target: {
      workspacePath: "C:/workspace",
      sessionId: "session-a",
      configGeneration: 1,
      accessMode: "default",
      runtimeOwnerToken: "idle:0",
      configOwnerMutationOpen: true,
    },
    access_mode_mutation_enabled: true,
    config_owner_mutation_open: true,
    config_draft_dirty: false,
    config_draft_discard_enabled: false,
    config_draft_commit_enabled: false,
    model_label: "model-a",
    provider_label: "Local",
    selected_project_index: 0,
    selected_session_index: 0,
    project_rows: [{ project_id: "project-a", label: "Project A", path: "C:/workspace" }],
    session_rows: [{
      session_id: "session-a",
      label: "Session A",
      title: "Session A",
      status: "idle",
      loaded_status: "idle",
      archived: false,
      memory_mode: "enabled",
      pending_permission_requests: 0,
      pending_user_input_requests: 0,
      short_id: "session-a",
    }],
    chat_session_rows: [],
    draft_prompt: "server prompt",
    image_input: "server.png",
    workspace_input: "C:/workspace",
    review_draft_text: "server review",
    local_search_text: "",
    session_search_text: "",
    overlay: "none",
    startup: {
      status: "ready",
      title: "Ready",
      message: "",
      detail: "",
      action_overlay: "none",
      initial_setup_required: false,
      checks: [],
    },
    composer_commit_generation: "0",
    draft_target: { workspacePath: "C:/workspace", sessionId: "session-a" },
    busy: false,
    navigation_loading: false,
    navigation_admission_open: true,
    background_mutation_pending: false,
    agent_tree_active: false,
    can_submit: true,
    enhance_enabled: true,
    image_input_enabled: true,
    send_enhanced_enabled: true,
    send_raw_enabled: true,
    provider_base_url: "http://127.0.0.1:1234",
    provider_metadata_mode: "openai_compatible_only",
    provider_catalog_base_url: "http://127.0.0.1:1234",
    provider_catalog_metadata_mode: "openai_compatible_only",
    provider_context_window: "131072",
    provider_max_output_tokens: "8192",
    provider_model_ids: ["model-a"],
    provider_models: ["Model A"],
    provider_selected_index: 0,
    provider_status: { kind: "idle", title: "Typed idle", hint: "Typed hint", details: "" },
    provider_selected_model_summary: [],
    provider_loading: false,
    provider_apply_enabled: true,
    config_target: { workspacePath: "C:/workspace", sessionId: "session-a", configGeneration: 1 },
    config_fields: [{
      key: "model.model",
      value: "model-a",
      env_override: null,
      value_type: "string",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    }],
    selected_config_index: 0,
    config_value_text: "model-a",
    config_feedback_text: "",
    attached_images: [],
    transcript_rows: [],
    thread_empty: true,
    artifact_rows: [],
    selected_artifact_index: -1,
    artifact_preview_available: false,
    artifact_preview_text: "",
    file_change_rows: [],
    agent_activity_rows: [],
    progress_text: "",
    tool_status_text: "",
    session_search_include_archived: false,
    history_export_enabled: true,
    ...overrides,
  } as DesktopWebState;
}

test("local drafts produce a view without mutating the Rust projection", () => {
  const state = projection();
  const original = structuredClone(state);
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, state, null);

  ui.drafts.prompt = "local prompt";
  ui.drafts.imageInput = "local.png";
  ui.drafts.provider.contextWindow = "65536";
  const view = projectViewState(state, ui);

  assert.equal(view.draft_prompt, "local prompt");
  assert.equal(view.image_input, "local.png");
  assert.equal(view.provider_context_window, "65536");
  assert.deepEqual(state, original);
});

test("all composer actions preserve the authoritative Rust admission gate", () => {
  const ui = createUiLocalState();
  const state = projection({ draft_prompt: "" });
  reconcileUiDrafts(ui, null, state, null);
  ui.drafts.prompt = "work";

  assert.equal(deriveUiCapabilities(state, ui).canSubmit, true);
  assert.equal(deriveUiCapabilities(state, ui).canEnhance, true);

  const treeActive = projection({
    busy: false,
    agent_tree_active: true,
    can_submit: false,
    enhance_enabled: false,
  });
  const capabilities = deriveUiCapabilities(treeActive, ui);
  assert.equal(capabilities.canSubmit, false);
  assert.equal(capabilities.canEnhance, false);
  assert.equal(capabilities.canReviewUncommitted, false);
  assert.equal(capabilities.canUseImageInput, true, "draft attachments remain available for the next prompt");
});

test("local typing cannot reopen the hidden Rust finalizing gate", () => {
  const ui = createUiLocalState();
  const finalizing = projection({
    busy: false,
    agent_tree_active: false,
    can_submit: false,
    enhance_enabled: false,
  });
  reconcileUiDrafts(ui, null, finalizing, null);
  ui.drafts.prompt = "typed while the previous run is finalizing";

  const capabilities = deriveUiCapabilities(finalizing, ui);
  assert.equal(capabilities.canSubmit, false);
  assert.equal(capabilities.canEnhance, false);
  assert.equal(capabilities.canReviewUncommitted, false);
});

test("a run-start command is single-flight in local capability projection", () => {
  const ui = createUiLocalState();
  const state = projection();
  reconcileUiDrafts(ui, null, state, null);
  ui.drafts.prompt = "submit once";
  ui.runStartMutationPending = true;

  const capabilities = deriveUiCapabilities(state, ui);
  assert.equal(capabilities.canSubmit, false);
  assert.equal(capabilities.canEnhance, false);
  assert.equal(capabilities.canReviewUncommitted, false);
  assert.equal(capabilities.canSendEnhancedReview, false);
  assert.equal(capabilities.canSendRawReview, false);
  assert.equal(projectViewState(state, ui).background_mutation_pending, true);
});

test("draft ownership resets only when its durable target changes", () => {
  const ui = createUiLocalState();
  const initial = projection();
  reconcileUiDrafts(ui, null, initial, null);
  ui.drafts.prompt = "unsaved local text";

  const poll = projection({ projection_revision: "2", status_message: "poll" });
  reconcileUiDrafts(ui, initial, poll, null);
  assert.equal(ui.drafts.prompt, "unsaved local text");

  const switched = projection({
    projection_revision: "3",
    selected_session_title: "Session B",
    session_rows: [{ session_id: "session-b", label: "Session B" }],
    draft_target: { workspacePath: "C:/workspace", sessionId: "session-b" },
    draft_prompt: "",
    config_target: { workspacePath: "C:/workspace", sessionId: "session-b", configGeneration: 1 },
  });
  reconcileUiDrafts(ui, poll, switched, null);
  assert.equal(ui.drafts.prompt, "");
});

test("an action response does not clear text entered after dispatch", () => {
  const ui = createUiLocalState();
  const initial = projection({ draft_prompt: "first" });
  reconcileUiDrafts(ui, null, initial, null);
  const snapshot = captureDraftMutation(ui, "submit_prompt");

  ui.drafts.prompt = "next request";
  ui.drafts.composerRevision += 1;
  const response = projection({ projection_revision: "2", draft_prompt: "" });
  acknowledgeDraftMutation(ui, response, "submit_prompt", snapshot);
  reconcileUiDrafts(ui, initial, response, snapshot);

  assert.equal(ui.drafts.prompt, "next request");
});

test("run command acknowledgement waits for durable admission before clearing the draft", () => {
  const ui = createUiLocalState();
  const initial = projection({ draft_prompt: "sent request" });
  reconcileUiDrafts(ui, null, initial, null);
  const snapshot = captureDraftMutation(ui, "submit_prompt");
  const response = projection({
    projection_revision: "2",
    draft_prompt: "sent request",
    busy: true,
    can_submit: false,
  });

  acknowledgeDraftMutation(ui, response, "submit_prompt", snapshot);
  reconcileUiDrafts(ui, initial, response, snapshot);
  assert.equal(ui.drafts.prompt, "sent request");

  const newerPoll = projection({
    projection_revision: "3",
    composer_commit_generation: "1",
    draft_prompt: "",
    image_input: "",
    busy: true,
    can_submit: false,
  });
  reconcileUiDrafts(ui, initial, newerPoll, null);

  assert.equal(ui.drafts.prompt, "");
  assert.equal(ui.drafts.pendingRunSubmission, null);
});

test("new-session binding preserves follow-up text typed after send", () => {
  const ui = createUiLocalState();
  const initial = projection({
    draft_prompt: "first request",
    draft_target: { workspacePath: "C:/workspace", sessionId: null },
  });
  reconcileUiDrafts(ui, null, initial, null);
  const snapshot = captureDraftMutation(ui, "submit_prompt");
  ui.drafts.prompt = "follow-up typed while running";
  ui.drafts.composerRevision += 1;

  const response = projection({
    projection_revision: "2",
    draft_prompt: "",
    draft_target: { workspacePath: "C:/workspace", sessionId: null },
    busy: true,
  });
  acknowledgeDraftMutation(ui, response, "submit_prompt", snapshot);
  reconcileUiDrafts(ui, initial, response, snapshot);
  const bound = projection({
    projection_revision: "3",
    composer_commit_generation: "1",
    draft_prompt: "",
    draft_target: { workspacePath: "C:/workspace", sessionId: "created-session" },
    busy: true,
  });
  reconcileUiDrafts(ui, response, bound, null);

  assert.equal(ui.drafts.prompt, "follow-up typed while running");
  assert.equal(ui.drafts.composerOwner, "C:/workspace\u0000created-session");
  assert.equal(ui.drafts.pendingRunSubmission, null);
});

test("new-session binding is registered before a newer poll can beat the command response", () => {
  const ui = createUiLocalState();
  const initial = projection({
    draft_prompt: "first request",
    draft_target: { workspacePath: "C:/workspace", sessionId: null },
  });
  reconcileUiDrafts(ui, null, initial, null);
  const snapshot = captureDraftMutation(ui, "submit_prompt");
  ui.drafts.prompt = "follow-up before command response";
  ui.drafts.composerRevision += 1;

  const boundPoll = projection({
    projection_revision: "3",
    composer_commit_generation: "1",
    draft_prompt: "",
    draft_target: { workspacePath: "C:/workspace", sessionId: "created-before-response" },
    busy: true,
  });
  reconcileUiDrafts(ui, initial, boundPoll, null);
  const olderResponse = projection({
    projection_revision: "2",
    draft_prompt: "",
    draft_target: { workspacePath: "C:/workspace", sessionId: null },
    busy: true,
  });
  acknowledgeDraftMutation(ui, olderResponse, "submit_prompt", snapshot);

  assert.equal(ui.drafts.prompt, "follow-up before command response");
  assert.equal(ui.drafts.composerOwner, "C:/workspace\u0000created-before-response");
});

test("failed run start releases an unconsumed pending submission", () => {
  const ui = createUiLocalState();
  const initial = projection({ draft_target: { workspacePath: "C:/workspace", sessionId: null } });
  reconcileUiDrafts(ui, null, initial, null);
  const snapshot = captureDraftMutation(ui, "submit_prompt");
  assert.notEqual(ui.drafts.pendingRunSubmission, null);

  rejectDraftMutation(ui, "submit_prompt", snapshot);
  assert.equal(ui.drafts.pendingRunSubmission, null);
});

test("pre-admission runtime failure preserves the retryable prompt and image", () => {
  const ui = createUiLocalState();
  const initial = projection({ draft_prompt: "retry me", image_input: "diagram.png" });
  reconcileUiDrafts(ui, null, initial, null);
  const snapshot = captureDraftMutation(ui, "submit_prompt");
  const launched = projection({
    projection_revision: "2",
    draft_prompt: "retry me",
    image_input: "diagram.png",
    busy: true,
    can_submit: false,
  });
  acknowledgeDraftMutation(ui, launched, "submit_prompt", snapshot);
  reconcileUiDrafts(ui, initial, launched, snapshot);

  const failed = projection({
    projection_revision: "3",
    draft_prompt: "retry me",
    image_input: "diagram.png",
    busy: false,
    can_submit: true,
    run_status_key: "failed",
  });
  reconcileUiDrafts(ui, launched, failed, null);

  assert.equal(ui.drafts.prompt, "retry me");
  assert.equal(ui.drafts.imageInput, "diagram.png");
  assert.equal(ui.drafts.pendingRunSubmission, null);
  assert.equal(ui.drafts.composerCommitGeneration, "0");
});

test("successful admission clears only the dispatched image and prompt revisions", () => {
  const ui = createUiLocalState();
  const initial = projection({ draft_prompt: "sent", image_input: "sent.png" });
  reconcileUiDrafts(ui, null, initial, null);
  captureDraftMutation(ui, "submit_prompt");
  ui.drafts.prompt = "next";
  ui.drafts.imageInput = "next.png";
  ui.drafts.composerRevision += 1;
  ui.drafts.imageRevision += 1;

  const admitted = projection({
    projection_revision: "2",
    composer_commit_generation: "1",
    draft_prompt: "",
    image_input: "",
    busy: true,
    can_submit: false,
  });
  reconcileUiDrafts(ui, initial, admitted, null);

  assert.equal(ui.drafts.prompt, "next");
  assert.equal(ui.drafts.imageInput, "next.png");
  assert.equal(ui.drafts.pendingRunSubmission, null);
});

test("review draft is retained until the reviewed run is durably admitted", () => {
  const ui = createUiLocalState();
  const initial = projection({
    overlay: "prompt_review",
    review_draft_text: "edited enhanced request",
  });
  reconcileUiDrafts(ui, null, initial, null);
  const snapshot = captureDraftMutation(ui, "send_prompt_review");
  const launchAccepted = projection({
    projection_revision: "2",
    overlay: "prompt_review",
    review_draft_text: "edited enhanced request",
    busy: true,
    can_submit: false,
  });
  acknowledgeDraftMutation(ui, launchAccepted, "send_prompt_review", snapshot);
  reconcileUiDrafts(ui, initial, launchAccepted, snapshot);
  assert.equal(ui.drafts.reviewDraft, "edited enhanced request");

  const admitted = projection({
    projection_revision: "3",
    composer_commit_generation: "1",
    overlay: "none",
    review_draft_text: "",
    busy: true,
    can_submit: false,
  });
  reconcileUiDrafts(ui, launchAccepted, admitted, null);
  assert.equal(ui.drafts.reviewDraft, "");
});

test("same-owner navigation acknowledgement cannot clear an unsaved composer draft", () => {
  const ui = createUiLocalState();
  const initial = projection({ draft_prompt: "server" });
  reconcileUiDrafts(ui, null, initial, null);
  ui.drafts.prompt = "unsaved local";
  ui.drafts.composerRevision += 1;

  const snapshot = captureDraftMutation(ui, "select_project");
  const response = projection({ projection_revision: "2", draft_prompt: "server" });
  acknowledgeDraftMutation(ui, response, "select_project", snapshot);
  reconcileUiDrafts(ui, initial, response, snapshot);

  assert.equal(snapshot, null);
  assert.equal(ui.drafts.prompt, "unsaved local");
});

test("interaction lifecycle holds one newest projection across pointer, keyboard, and IME", () => {
  const lifecycle = new InteractionLifecycle<number>((current, candidate) => candidate > current);
  lifecycle.beginPointer(7);
  lifecycle.beginKey("Enter");
  lifecycle.beginComposition();

  assert.equal(lifecycle.defer(2, false, true), true);
  assert.equal(lifecycle.defer(1, false, true), true);
  assert.equal(lifecycle.endPointer(7), null);
  assert.equal(lifecycle.endKey("Enter"), null);
  assert.deepEqual(lifecycle.endComposition(), { deferred: 2, renderCurrent: false });
  assert.equal(lifecycle.active, false);
});

test("interaction lifecycle cancellation recovers a missing compositionend", () => {
  const lifecycle = new InteractionLifecycle<number>((_current, _candidate) => true);
  lifecycle.beginComposition();
  lifecycle.defer(4, false, true);

  assert.deepEqual(lifecycle.cancel(), { deferred: 4, renderCurrent: false });
  assert.equal(lifecycle.active, false);
});

test("non-control text selection, scrolling, and document keyboard reading start the lifecycle", () => {
  assert.equal(shouldBeginPointerInteraction(0, true), true);
  assert.equal(shouldBeginPointerInteraction(2, true), false);
  assert.equal(shouldBeginKeyboardInteraction(false, "PageDown", true, false), true);
  assert.equal(shouldBeginKeyboardInteraction(false, "ArrowDown", true, false), true);
  assert.equal(shouldBeginKeyboardInteraction(true, "KeyA", true, false), false);
  assert.equal(shouldBeginKeyboardInteraction(false, "Enter", true, true), false);
});

test("frontend config validation matches integer and floating-point field shapes", () => {
  const integer = {
    key: "multi_agent.max_concurrent_agents",
    value: "4",
    env_override: null,
    value_type: "integer",
    required: false,
    min_value: 1,
    max_value: null,
    options: [],
  };
  const number = { ...integer, key: "model.temperature", value_type: "number", min_value: null };
  assert.equal(validateConfigInput(integer, "1.5").ok, false);
  assert.equal(validateConfigInput(integer, "0").ok, false);
  assert.equal(validateConfigInput(integer, "4").ok, true);
  assert.equal(validateConfigInput(number, "0.2").ok, true);
  assert.equal(validateConfigInput(number, "NaN").ok, false);
});

test("global action shortcuts ignore key-repeat activation", () => {
  const f8 = { key: "F8", ctrlKey: false, metaKey: false, repeat: false };
  assert.equal(globalShortcutAction(f8), "toggle-access");
  assert.equal(globalShortcutAction({ ...f8, repeat: true }), null);
  assert.equal(globalShortcutAction({ key: "Enter", ctrlKey: true, metaKey: false, repeat: false }), "send");
  assert.equal(globalShortcutAction({ key: "Enter", ctrlKey: true, metaKey: false, repeat: true }), null);
});

test("permission visibility does not stop runtime polling", () => {
  assert.equal(autoRefreshAllowed({ navigation_loading: false, confirmation_visible: true }, false), true);
  assert.equal(autoRefreshAllowed({ navigation_loading: false, confirmation_visible: true }, true), false);
  assert.equal(autoRefreshAllowed({ navigation_loading: true, confirmation_visible: true }, true), true);
});

test("workspace browser submits its local draft with the authoritative draft owner", async () => {
  const state = projection({ workspace_input: "D:/next-workspace" });
  let invocation: { name: string; args?: Record<string, unknown> } | null = null;
  const context = {
    mutate: async (name: string, args?: Record<string, unknown>) => {
      invocation = { name, args };
    },
  } as unknown as ActionContext;

  await actionById("browse-workspace")?.run(state, context, { index: -1, value: "" });

  assert.deepEqual(invocation, {
    name: "browse_workspace",
    args: {
      text: "D:/next-workspace",
      expectedTarget: { workspacePath: "C:/workspace", sessionId: "session-a" },
    },
  });
});

test("search and attachment actions carry their authoritative owners", async () => {
  const state = projection({ attached_images: ["C:/workspace/reference.png"] });
  const invocations: Array<{ name: string; args?: Record<string, unknown> }> = [];
  const context = {
    mutate: async (name: string, args?: Record<string, unknown>) => {
      invocations.push({ name, args });
    },
  } as unknown as ActionContext;

  await actionById("toggle-session-archived-search")?.run(state, context, { index: -1, value: "" });
  await actionById("clear-images")?.run(state, context, { index: -1, value: "" });
  await actionById("toggle-access")?.run(state, context, { index: -1, value: "" });

  assert.deepEqual(sessionSearchMutationTarget(state), {
    workspacePath: "C:/workspace",
    projectId: "project-a",
  });
  assert.deepEqual(invocations, [
    {
      name: "set_session_search_include_archived",
      args: {
        includeArchived: true,
        expectedTarget: { workspacePath: "C:/workspace", projectId: "project-a" },
      },
    },
    {
      name: "clear_images",
      args: { expectedTarget: { workspacePath: "C:/workspace", sessionId: "session-a" } },
    },
    {
      name: "toggle_access_mode",
      args: {
        expectedTarget: {
          workspacePath: "C:/workspace",
          sessionId: "session-a",
          configGeneration: 1,
          accessMode: "default",
          runtimeOwnerToken: "idle:0",
          configOwnerMutationOpen: true,
        },
      },
    },
  ]);
  assert.equal(
    actionById("toggle-access")?.enabled?.(
      projection({ access_mode_mutation_enabled: false }),
      { index: -1, value: "" },
    ),
    false,
  );
  const exportDisabled = projection({ history_export_enabled: false });
  assert.equal(
    actionById("export-history")?.enabled(exportDisabled, { index: -1, value: "" }),
    false,
  );
});

test("stable row identity and selected-state semantics survive list reordering", () => {
  const state = projection();
  const sidebar = renderSidebar(state);
  assert.match(sidebar, /data-focus-key="project:project-a:select" aria-current="page"/);
  assert.match(sidebar, /data-focus-key="session:session-a:select" aria-current="page"/);
  const childOnlyActive = renderSidebar(projection({ busy: false, agent_tree_active: true }));
  assert.match(childOnlyActive, /data-focus-key="session:session-a:select"[^>]*>[\s\S]*?busy-spinner/);

  const artifact = renderArtifactPane(projection({
    artifact_rows: [{ label: "report", path: "C:/workspace/report.md", kind: "file", action: "created" }],
    selected_artifact_index: 0,
  }));
  assert.match(artifact, /data-focus-key="artifact:C:\/workspace\/report\.md" aria-current="true"/);
});

test("access-mode control consumes the Rust mutation capability", () => {
  assert.match(
    renderTopbar(projection({ access_mode_mutation_enabled: true })),
    /data-action="toggle-access"[^>]*aria-disabled="false"(?![^>]*\sdisabled(?:\s|>|=))[^>]*>/,
  );
  assert.match(
    renderTopbar(projection({ access_mode_mutation_enabled: false })),
    /data-action="toggle-access"[^>]*aria-disabled="true"[^>]*\sdisabled>/,
  );
});

test("config commit capability never gates unrelated workspace or window actions", () => {
  const clean = projection({ config_draft_commit_enabled: false });
  for (const id of ["browse-workspace", "toggle-maximize-window"]) {
    assert.equal(actionById(id)?.enabled?.(clean, { index: -1, value: "" }), true, id);
  }
  assert.equal(
    actionById("apply-session-config")?.enabled?.(clean, { index: -1, value: "" }),
    false,
  );
  assert.equal(
    actionById("save-global-config")?.enabled?.(clean, { index: -1, value: "" }),
    false,
  );
});

test("a closed dirty settings draft blocks every external config owner mutation", () => {
  const ui = createUiLocalState();
  const fields = [
    {
      key: "model.model",
      value: "model-a",
      env_override: null,
      value_type: "string",
      required: false,
      min_value: null,
      max_value: null,
      options: [],
    },
    {
      key: "permissions.access_mode",
      value: "default",
      env_override: null,
      value_type: "enum",
      required: false,
      min_value: null,
      max_value: null,
      options: ["default", "auto_review", "full_access"],
    },
  ];
  const before = projection({ config_fields: fields });
  reconcileUiDrafts(ui, null, before, null);
  updateConfigDraftValue(
    ui,
    before.config_target,
    before.config_fields.map((field) => ({ key: field.key, text: field.value })),
    "model.model",
    "unsaved-model-draft",
  );

  const dirtyView = projectViewState(before, ui);

  assert.equal(dirtyView.config_target.configGeneration, before.config_target.configGeneration);
  assert.equal(ui.configDirty, true);
  assert.equal(dirtyView.config_owner_mutation_open, false);
  assert.equal(dirtyView.access_target.configOwnerMutationOpen, false);
  assert.equal(dirtyView.access_mode_mutation_enabled, false);
  assert.equal(dirtyView.provider_apply_enabled, false);
  assert.equal(
    dirtyView.config_fields.find((field) => field.key === "model.model")?.value,
    "unsaved-model-draft",
  );
  assert.equal(
    dirtyView.config_fields.find((field) => field.key === "permissions.access_mode")?.value,
    "default",
    "the settings draft remains authoritative until its own Apply/Save",
  );

  for (const id of ["toggle-access", "apply-provider-session", "save-provider-global"]) {
    assert.equal(actionById(id)?.enabled?.(dirtyView, { index: -1, value: "" }), false, id);
    assert.equal(paletteActions(dirtyView).some((action) => action.id === id), false, id);
  }
  assert.equal(
    paletteActions(dirtyView).some((action) => action.id === "load-provider-models"),
    true,
    "catalog loading does not replace the config owner",
  );

  setRenderContext({
    artifactPaneCollapsed: false,
    attachmentTrayOpen: false,
    configDirty: true,
    configMutationPending: false,
    configOwnerMutationOpen: false,
    configDraftEditOpen: true,
    configDraftDiscardOpen: true,
    configDraftCommitOpen: true,
  });
  const setup = {
    ...dirtyView,
    overlay: "config",
    startup: {
      ...dirtyView.startup,
      initial_setup_required: true,
      action_overlay: "config",
    },
  };
  const dirtySettings = renderOverlay(setup);
  assert.match(dirtySettings, /data-action="import-config-toml" disabled>/);
  assert.match(dirtySettings, /data-action="discard-config-draft"(?![^>]*hidden)/);
  setRenderContext({
    artifactPaneCollapsed: false,
    attachmentTrayOpen: false,
    configDirty: false,
    configMutationPending: false,
    configOwnerMutationOpen: true,
    configDraftEditOpen: true,
    configDraftDiscardOpen: false,
    configDraftCommitOpen: false,
  });

  const after = projection({
    access_label: "auto_review",
    access_target: { ...before.access_target, accessMode: "auto_review" },
    config_target: { ...before.config_target },
    config_fields: fields.map((field) => field.key === "permissions.access_mode"
      ? { ...field, value: "auto_review" }
      : field),
  });

  const cleanUi = createUiLocalState();
  reconcileUiDrafts(cleanUi, null, after, null);
  assert.equal(
    projectViewState(after, cleanUi).config_fields
      .find((field) => field.key === "permissions.access_mode")?.value,
    "auto_review",
    "a clean settings view consumes the new Rust baseline",
  );
});

test("local config and run-start mutations close external config owner admission", async () => {
  const state = projection({ access_mode_mutation_enabled: true });
  const configPending = createUiLocalState();
  reconcileUiDrafts(configPending, null, state, null);
  updateConfigDraftValue(
    configPending,
    state.config_target,
    state.config_fields.map((field) => ({ key: field.key, text: field.value })),
    "model.model",
    "pending-model",
  );
  configPending.activeConfigMutationGeneration = 1;
  const configPendingView = projectViewState(state, configPending);
  assert.equal(configPendingView.access_mode_mutation_enabled, false);
  assert.equal(configPendingView.provider_apply_enabled, false);
  assert.equal(configPendingView.config_owner_mutation_open, false);
  assert.equal(configPendingView.config_draft_discard_enabled, false);
  assert.equal(configPendingView.config_draft_commit_enabled, false);
  assert.equal(configDraftEditOpen(configPending), false);
  for (const id of ["discard-config-draft", "apply-session-config", "save-global-config"]) {
    assert.equal(actionById(id)?.enabled?.(
      configPendingView,
      { index: -1, value: "" },
    ), false, id);
  }
  let rerenders = 0;
  await actionById("discard-config-draft")?.run(
    configPendingView,
    { uiState: configPending, rerender: () => { rerenders += 1; } } as unknown as ActionContext,
    { index: -1, value: "" },
  );
  assert.equal(configPending.configDirty, true);
  assert.equal(rerenders, 0);

  let configCommands = 0;
  await actionById("apply-session-config")?.run(
    configPendingView,
    {
      uiState: configPending,
      mutate: async () => { configCommands += 1; },
    } as unknown as ActionContext,
    { index: -1, value: "" },
  );
  assert.equal(configCommands, 0, "direct repeated dispatch is rejected before a second request");

  setRenderContext({
    artifactPaneCollapsed: false,
    attachmentTrayOpen: false,
    configDirty: true,
    configMutationPending: true,
    configOwnerMutationOpen: false,
    configDraftEditOpen: false,
    configDraftDiscardOpen: false,
    configDraftCommitOpen: false,
  });
  const pendingHtml = renderOverlay({ ...configPendingView, overlay: "config" });
  assert.match(pendingHtml, /data-action="discard-config-draft"[^>]*disabled/);
  assert.match(pendingHtml, /data-action="apply-session-config" disabled/);
  assert.match(pendingHtml, /data-action="save-global-config" disabled/);
  assert.match(pendingHtml, /class="settings-control"[^>]*disabled/);
  setRenderContext({
    artifactPaneCollapsed: false,
    attachmentTrayOpen: false,
    configDirty: false,
    configMutationPending: false,
    configOwnerMutationOpen: true,
    configDraftEditOpen: true,
    configDraftDiscardOpen: false,
    configDraftCommitOpen: false,
  });

  configPending.activeConfigMutationGeneration = null;
  const resumed = projectViewState(state, configPending);
  assert.equal(configDraftEditOpen(configPending), true);
  assert.equal(resumed.config_draft_discard_enabled, true);
  assert.equal(resumed.config_draft_commit_enabled, true);

  const runPending = createUiLocalState();
  reconcileUiDrafts(runPending, null, state, null);
  runPending.runStartMutationPending = true;
  const runPendingView = projectViewState(state, runPending);
  assert.equal(runPendingView.access_mode_mutation_enabled, false);
  assert.equal(runPendingView.provider_apply_enabled, false);
  assert.equal(runPendingView.config_owner_mutation_open, false);
});

test("hidden settings draft can be reopened, discarded, and release external mutations", async () => {
  const raw = projection({ overlay: "none" });
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, raw, null);
  updateConfigDraftValue(
    ui,
    raw.config_target,
    raw.config_fields.map((field) => ({ key: field.key, text: field.value })),
    "model.model",
    "invalid-hidden-draft",
  );
  assert.equal(projectViewState(raw, ui).config_owner_mutation_open, false);

  const reopened = { ...raw, overlay: "config" };
  reconcileUiDrafts(ui, raw, reopened, null);
  const reopenedView = projectViewState(reopened, ui);
  assert.equal(reopenedView.config_draft_dirty, true);
  assert.equal(actionById("discard-config-draft")?.enabled?.(
    reopenedView,
    { index: -1, value: "" },
  ), true);
  let rerenders = 0;
  await actionById("discard-config-draft")?.run(
    reopenedView,
    { uiState: ui, rerender: () => { rerenders += 1; } } as unknown as ActionContext,
    { index: -1, value: "" },
  );

  const cleanView = projectViewState(reopened, ui);
  assert.equal(ui.configDirty, false);
  assert.equal(cleanView.config_owner_mutation_open, true);
  assert.equal(cleanView.access_mode_mutation_enabled, true);
  assert.equal(cleanView.provider_apply_enabled, true);
  assert.equal(rerenders, 1);
});

test("external config mutation roundtrip prevents a settings draft from starting", async () => {
  const state = projection({ overlay: "config" });
  state.startup.initial_setup_required = true;
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, state, null);
  ui.externalConfigMutationPending = true;

  const pending = projectViewState(state, ui);
  assert.equal(configDraftEditOpen(ui), false);
  assert.equal(pending.config_owner_mutation_open, false);
  assert.equal(pending.access_mode_mutation_enabled, false);
  assert.equal(pending.provider_apply_enabled, false);
  assert.equal(pending.config_draft_commit_enabled, false);
  assert.equal(ui.configDirty, false);
  let configCommands = 0;
  await actionById("apply-session-config")?.run(
    pending,
    {
      uiState: ui,
      getViewState: () => pending,
      mutate: async () => { configCommands += 1; },
    } as unknown as ActionContext,
    { index: -1, value: "" },
  );
  assert.equal(configCommands, 0);

  setRenderContext({
    artifactPaneCollapsed: false,
    attachmentTrayOpen: false,
    configDirty: false,
    configMutationPending: false,
    configOwnerMutationOpen: false,
    configDraftEditOpen: false,
    configDraftDiscardOpen: false,
    configDraftCommitOpen: false,
  });
  assert.match(
    renderOverlay(pending),
    /class="settings-control"[^>]*disabled/,
  );
  setRenderContext({
    artifactPaneCollapsed: false,
    attachmentTrayOpen: false,
    configDirty: false,
    configMutationPending: false,
    configOwnerMutationOpen: true,
    configDraftEditOpen: true,
    configDraftDiscardOpen: false,
    configDraftCommitOpen: false,
  });
});

test("provider overlay consumes typed status and exposes control selection semantics", () => {
  const html = renderOverlay(projection({ overlay: "provider" }));
  assert.match(html, /<label class="field-label" for="provider-url">/);
  assert.match(html, /data-mode="openai_compatible_only" aria-pressed="true"/);
  assert.match(html, /data-focus-key="provider-model:model-a" aria-pressed="true"/);
  assert.match(html, /Typed idle/);
  assert.doesNotMatch(html, /処理に失敗しました/);

  const invalid = renderOverlay(projection({
    overlay: "provider",
    provider_base_url: "",
    provider_context_window: "0",
  }));
  assert.match(invalid, /data-action="load-provider-models" disabled>モデル読込<\/button>/);
});

test("provider apply requires Rust catalog evidence owned by the local URL and mode", () => {
  const withoutCatalog = projection({
    provider_apply_enabled: false,
    provider_catalog_base_url: null,
    provider_catalog_metadata_mode: null,
  });
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, withoutCatalog, null);
  assert.equal(projectViewState(withoutCatalog, ui).provider_apply_enabled, false);

  const loaded = projection({
    provider_apply_enabled: true,
    provider_catalog_base_url: "http://127.0.0.1:1234",
    provider_catalog_metadata_mode: "openai_compatible_only",
  });
  reconcileUiDrafts(ui, withoutCatalog, loaded, null);
  assert.equal(projectViewState(loaded, ui).provider_apply_enabled, true);

  ui.drafts.provider.baseUrl = "http://127.0.0.1:1234/v1/";
  ui.drafts.provider.contextWindow = "65536";
  assert.equal(
    projectViewState(loaded, ui).provider_apply_enabled,
    true,
    "normalized /v1 URLs and local limit changes keep ownership of the loaded catalog",
  );
  ui.drafts.provider.baseUrl = "http://127.0.0.1:4321";
  assert.equal(projectViewState(loaded, ui).provider_apply_enabled, false);
  ui.drafts.provider.baseUrl = "http://127.0.0.1:1234";
  ui.drafts.provider.metadataMode = "lm_studio_native_required";
  assert.equal(projectViewState(loaded, ui).provider_apply_enabled, false);

  const unownedEvidence = projection({
    provider_apply_enabled: true,
    provider_catalog_base_url: null,
    provider_catalog_metadata_mode: null,
  });
  const unownedUi = createUiLocalState();
  reconcileUiDrafts(unownedUi, null, unownedEvidence, null);
  assert.equal(
    projectViewState(unownedEvidence, unownedUi).provider_apply_enabled,
    false,
    "frontend validity cannot replace Rust catalog ownership evidence",
  );
});

test("settings enum controls render only Rust-projected option values", () => {
  const html = renderOverlay(projection({
    overlay: "config",
    config_fields: [{
      key: "multi_agent.mode",
      value: "future_mode",
      env_override: null,
      value_type: "enum",
      required: false,
      min_value: null,
      max_value: null,
      options: ["explicit_request_only", "future_mode"],
    }],
  }));
  assert.match(html, /<option value="future_mode" selected>future_mode<\/option>/);
  assert.doesNotMatch(html, /<option value="proactive"/);
});

test("canonical row_kind selects specialized transcript rendering over legacy kind", () => {
  const html = renderThreadContent(projection({
    thread_empty: false,
    transcript_rows: [{
      row_kind: "work_summary_running",
      kind: "assistant",
      step: "1",
      title: "Work",
      body: "running",
      file_changes: [],
    }],
  }));
  assert.match(html, /message work-summary work_summary_running/);
  assert.match(html, /<details[^>]+open>/);
});

import assert from "node:assert/strict";
import test from "node:test";

import { actionById, paletteActions, type ActionContext } from "../src/actions.ts";
import {
  discardConfigDraft,
  sameConfigMutationTarget,
  updateConfigDraftValue,
} from "../src/config_mutation.ts";
import {
  InteractionLifecycle,
  installInteractionEventGate,
  shouldBeginKeyboardInteraction,
  shouldBeginPointerInteraction,
} from "../src/interaction_lifecycle.ts";
import { wireEvents } from "../src/events.ts";
import { globalShortcutAction } from "../src/keyboard_shortcut.ts";
import { autoRefreshAllowed, runtimePollingRequired } from "../src/polling_state.ts";
import {
  renderArtifactPane,
  renderComposer,
  renderOverlay,
  renderSidebar,
  renderThreadContent,
  renderTopbar,
  setRenderContext,
} from "../src/render.ts";
import type { DesktopViewState, DesktopWebState, RunMutationTarget } from "../src/types.ts";
import { createUiLocalState } from "../src/ui_state.ts";
import { displayAccessLabel, validateConfigInput } from "../src/utils.ts";
import {
  acknowledgeDraftMutation,
  captureDraftMutation,
  configDraftEditOpen,
  deriveUiCapabilities,
  mutationAdmissionOpen,
  projectViewState,
  reconcileUiDrafts,
  rejectDraftMutation,
  sessionSearchMutationTarget,
} from "../src/view_state.ts";

function projection(overrides: Partial<DesktopViewState> = {}): DesktopViewState {
  return {
    projection_revision: "1",
    workspace_path: "C:/workspace",
    selected_session_title: "Session A",
    current_session_label: "Session A",
    status_message: "Ready",
    status_detail: "",
    status_code: "plain",
    access_label: "default",
    access_target: {
      workspacePath: "C:/workspace",
      sessionId: "session-a",
      configGeneration: "1",
      accessMode: "default",
      runtimeOwnerToken: "idle:0",
    },
    config_draft_capabilities: {
      clean: {
        dirty: false,
        edit_enabled: true,
        discard_enabled: false,
        commit_enabled: false,
        external_owner_mutation_open: true,
        access_mode_mutation_enabled: true,
      },
      dirty: {
        dirty: true,
        edit_enabled: true,
        discard_enabled: true,
        commit_enabled: true,
        external_owner_mutation_open: false,
        access_mode_mutation_enabled: false,
      },
    },
    config_draft: {
      dirty: false,
      edit_enabled: true,
      discard_enabled: false,
      commit_enabled: false,
      external_owner_mutation_open: true,
      access_mode_mutation_enabled: true,
    },
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
    draft_target: { workspacePath: "C:/workspace", sessionId: "session-a", ownerGeneration: 1 },
    busy: false,
    navigation_loading: false,
    navigation_admission_open: true,
    background_mutation_pending: false,
    agent_tree_active: false,
    composer_submit_mode: "new_request",
    can_submit: true,
    can_cancel_run: false,
    run_target: {
      workspacePath: "C:/workspace",
      sessionId: "session-a",
      runtimeOwnerToken: "idle:0",
    } satisfies RunMutationTarget,
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
    config_target: {
      workspacePath: "C:/workspace",
      sessionId: "session-a",
      configGeneration: "1",
    },
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
    plan: null,
    session_search_include_archived: false,
    history_export_enabled: true,
    ...overrides,
  } as DesktopViewState;
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
  assert.equal(mutationAdmissionOpen(ui, "submit_prompt"), true);
  assert.equal(mutationAdmissionOpen(ui, "enhance_prompt"), true);

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

test("active root steering keeps an enabled send control labeled as additional instruction", () => {
  for (const agentTreeActive of [false, true]) {
    const rendered = renderComposer(projection({
      busy: true,
      agent_tree_active: agentTreeActive,
      composer_submit_mode: "steer",
      can_submit: true,
      enhance_enabled: false,
      draft_prompt: "追加指示",
      token_meter_label: "",
      token_meter_title: "",
    }));

    assert.match(
      rendered,
      /data-action="send" title="実行中のタスクへ追加指示を送信" aria-label="実行中のタスクへ追加指示を送信"/,
      agentTreeActive ? "active child retention" : "active root",
    );
    assert.doesNotMatch(rendered, /実行中は送信できません|Sub Agentの完了または停止後に送信できます/);
  }
});

test("active root steering with an empty draft asks for input instead of claiming steering is blocked", () => {
  for (const agentTreeActive of [false, true]) {
    const rendered = renderComposer(projection({
      busy: true,
      agent_tree_active: agentTreeActive,
      composer_submit_mode: "steer",
      can_submit: false,
      enhance_enabled: false,
      draft_prompt: "",
      token_meter_label: "",
      token_meter_title: "",
    }));

    assert.match(
      rendered,
      /data-action="send" title="依頼文を入力してください" aria-label="依頼文を入力してください"[^>]*disabled/,
      agentTreeActive ? "active child retention" : "active root",
    );
    assert.doesNotMatch(rendered, /実行中は送信できません|Sub Agentの完了または停止後に送信できます/);
  }
});

test("the installed prompt input handler refreshes the steer send title and aria label", () => {
  class FakeHtmlElement {
    hidden = false;
  }
  class FakePrompt extends FakeHtmlElement {
    value = "";
    scrollHeight = 24;
    style = { height: "", overflowY: "" };
    private readonly listeners = new Map<string, (event: { currentTarget: FakePrompt }) => void>();

    addEventListener(name: string, listener: (event: { currentTarget: FakePrompt }) => void): void {
      this.listeners.set(name, listener);
    }

    input(value: string): void {
      this.value = value;
      this.listeners.get("input")?.({ currentTarget: this });
    }
  }
  class FakeButton extends FakeHtmlElement {
    disabled = true;
    title = "依頼文を入力してください";
    readonly attributes = new Map<string, string>([["aria-label", this.title]]);

    setAttribute(name: string, value: string): void {
      this.attributes.set(name, value);
    }

    getAttribute(name: string): string | null {
      return this.attributes.get(name) ?? null;
    }
  }

  const prompt = new FakePrompt();
  const send = new FakeButton();
  const fakeDocument = {
    activeElement: null,
    addEventListener: () => undefined,
    querySelector: (selector: string) => {
      if (selector === "#prompt") return prompt;
      if (selector === '[data-action="send"]') return send;
      return null;
    },
    querySelectorAll: () => [],
  };
  const fakeWindow = {
    getComputedStyle: () => ({ maxHeight: "200" }),
    localStorage: { getItem: () => null, setItem: () => undefined },
    addEventListener: () => undefined,
  };
  const previousGlobals = new Map(
    ["document", "window", "HTMLElement", "HTMLInputElement", "HTMLTextAreaElement", "HTMLSelectElement", "HTMLButtonElement", "Element"]
      .map((name) => [name, Object.getOwnPropertyDescriptor(globalThis, name)] as const),
  );
  const defineGlobal = (name: string, value: unknown) => {
    Object.defineProperty(globalThis, name, { configurable: true, writable: true, value });
  };

  try {
    defineGlobal("document", fakeDocument);
    defineGlobal("window", fakeWindow);
    defineGlobal("HTMLElement", FakeHtmlElement);
    defineGlobal("HTMLInputElement", FakeHtmlElement);
    defineGlobal("HTMLTextAreaElement", FakePrompt);
    defineGlobal("HTMLSelectElement", FakeHtmlElement);
    defineGlobal("HTMLButtonElement", FakeButton);
    defineGlobal("Element", FakeHtmlElement);

    const rustProjection = projection({
      busy: true,
      agent_tree_active: true,
      composer_submit_mode: "steer",
      can_submit: true,
      enhance_enabled: false,
      draft_prompt: "",
    });
    const ui = createUiLocalState();
    reconcileUiDrafts(ui, null, rustProjection, null);
    const view = projectViewState(rustProjection, ui);
    const context = {
      uiState: ui,
      getProjection: () => rustProjection,
    } as unknown as ActionContext;

    wireEvents(view, context);
    prompt.input("追加指示");

    assert.equal(send.disabled, false);
    assert.equal(send.title, "実行中のタスクへ追加指示を送信");
    assert.equal(send.getAttribute("aria-label"), "実行中のタスクへ追加指示を送信");
  } finally {
    for (const [name, descriptor] of previousGlobals) {
      if (descriptor) Object.defineProperty(globalThis, name, descriptor);
      else delete (globalThis as Record<string, unknown>)[name];
    }
  }
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
  assert.equal(mutationAdmissionOpen(ui, "enhance_prompt"), false);
  const view = projectViewState(state, ui);
  assert.equal(view.background_mutation_pending, true);
  assert.equal(view.busy, true, "the local request remains visibly pending");
  assert.equal(
    actionById("cancel-run")?.enabled?.(view, { index: -1, value: "" }),
    false,
    "only Rust may publish the cancel capability",
  );
  const admitted = projectViewState(projection({ can_cancel_run: true }), ui);
  assert.equal(actionById("cancel-run")?.enabled?.(admitted, { index: -1, value: "" }), true);
});

test("an external config owner mutation immediately rejects submit and review dispatch", () => {
  const ui = createUiLocalState();
  const state = projection();
  reconcileUiDrafts(ui, null, state, null);
  ui.drafts.prompt = "must wait for the access owner";
  ui.drafts.reviewDraft = "review must wait too";
  ui.externalConfigMutationPending = true;

  const capabilities = deriveUiCapabilities(state, ui);
  assert.equal(capabilities.canSubmit, false);
  assert.equal(capabilities.canEnhance, false);
  assert.equal(capabilities.canReviewUncommitted, false);
  assert.equal(capabilities.canSendEnhancedReview, false);
  assert.equal(capabilities.canSendRawReview, false);
  for (const mutation of [
    "submit_prompt",
    "review_uncommitted",
    "send_prompt_review",
    "enhance_prompt",
  ]) {
    assert.equal(mutationAdmissionOpen(ui, mutation), false, mutation);
  }
  assert.equal(
    mutationAdmissionOpen(ui, "cancel_run"),
    true,
    "Stop is independent from access/config persistence",
  );
  assert.equal(
    mutationAdmissionOpen(ui, "desktop_state"),
    true,
    "polling remains available while the config owner settles",
  );
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
    draft_target: { workspacePath: "C:/workspace", sessionId: "session-b", ownerGeneration: 2 },
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
    draft_target: { workspacePath: "C:/workspace", sessionId: null, ownerGeneration: 1 },
  });
  reconcileUiDrafts(ui, null, initial, null);
  const snapshot = captureDraftMutation(ui, "submit_prompt");
  ui.drafts.prompt = "follow-up typed while running";
  ui.drafts.composerRevision += 1;

  const response = projection({
    projection_revision: "2",
    draft_prompt: "",
    draft_target: { workspacePath: "C:/workspace", sessionId: null, ownerGeneration: 1 },
    busy: true,
  });
  acknowledgeDraftMutation(ui, response, "submit_prompt", snapshot);
  reconcileUiDrafts(ui, initial, response, snapshot);
  const bound = projection({
    projection_revision: "3",
    composer_commit_generation: "1",
    draft_prompt: "",
    draft_target: { workspacePath: "C:/workspace", sessionId: "created-session", ownerGeneration: 1 },
    busy: true,
  });
  reconcileUiDrafts(ui, response, bound, null);

  assert.equal(ui.drafts.prompt, "follow-up typed while running");
  assert.equal(ui.drafts.composerOwner, "C:/workspace\u00001\u0000created-session");
  assert.equal(ui.drafts.pendingRunSubmission, null);
});

test("new-session binding is registered before a newer poll can beat the command response", () => {
  const ui = createUiLocalState();
  const initial = projection({
    draft_prompt: "first request",
    draft_target: { workspacePath: "C:/workspace", sessionId: null, ownerGeneration: 1 },
  });
  reconcileUiDrafts(ui, null, initial, null);
  const snapshot = captureDraftMutation(ui, "submit_prompt");
  ui.drafts.prompt = "follow-up before command response";
  ui.drafts.composerRevision += 1;

  const boundPoll = projection({
    projection_revision: "3",
    composer_commit_generation: "1",
    draft_prompt: "",
    draft_target: { workspacePath: "C:/workspace", sessionId: "created-before-response", ownerGeneration: 1 },
    busy: true,
  });
  reconcileUiDrafts(ui, initial, boundPoll, null);
  const olderResponse = projection({
    projection_revision: "2",
    draft_prompt: "",
    draft_target: { workspacePath: "C:/workspace", sessionId: null, ownerGeneration: 2 },
    busy: true,
  });
  acknowledgeDraftMutation(ui, olderResponse, "submit_prompt", snapshot);

  assert.equal(ui.drafts.prompt, "follow-up before command response");
  assert.equal(ui.drafts.composerOwner, "C:/workspace\u00001\u0000created-before-response");
});

test("same-owner generation reset discards stale local composer text", () => {
  const ui = createUiLocalState();
  const initial = projection({
    draft_prompt: "server draft",
    draft_target: { workspacePath: "C:/workspace", sessionId: null, ownerGeneration: 4 },
  });
  reconcileUiDrafts(ui, null, initial, null);
  ui.drafts.prompt = "stale local draft";
  ui.drafts.composerRevision += 1;

  const reset = projection({
    projection_revision: "2",
    draft_prompt: "",
    draft_target: { workspacePath: "C:/workspace", sessionId: null, ownerGeneration: 5 },
  });
  reconcileUiDrafts(ui, initial, reset, null);

  assert.equal(ui.drafts.prompt, "");
});

test("failed run start releases an unconsumed pending submission", () => {
  const ui = createUiLocalState();
  const initial = projection({
    draft_target: { workspacePath: "C:/workspace", sessionId: null, ownerGeneration: 1 },
  });
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

test("same-overlay prompt enhancement completion hydrates an untouched local review draft", () => {
  const ui = createUiLocalState();
  const enhancing = projection({
    overlay: "prompt_review",
    review_draft_text: "",
    review_status_text: "Enhancing",
  });
  reconcileUiDrafts(ui, null, enhancing, null);

  const reviewing = projection({
    projection_revision: "2",
    overlay: "prompt_review",
    review_draft_text: "enhanced request",
    review_status_text: "Reviewing",
  });
  reconcileUiDrafts(ui, enhancing, reviewing, null);

  assert.equal(ui.drafts.reviewDraft, "enhanced request");
  assert.equal(projectViewState(reviewing, ui).review_draft_text, "enhanced request");
});

test("late same-owner enhancement cannot overwrite a local review edit", () => {
  const ui = createUiLocalState();
  const enhancing = projection({
    overlay: "prompt_review",
    review_draft_text: "",
    review_status_text: "Enhancing",
  });
  reconcileUiDrafts(ui, null, enhancing, null);
  ui.drafts.reviewDraft = "user refinement";
  ui.drafts.reviewRevision += 1;

  const staleCompletion = projection({
    projection_revision: "2",
    overlay: "prompt_review",
    review_draft_text: "late enhanced request",
    review_status_text: "Reviewing",
  });
  reconcileUiDrafts(ui, enhancing, staleCompletion, null);

  assert.equal(ui.drafts.reviewDraft, "user refinement");
  assert.equal(projectViewState(staleCompletion, ui).review_draft_text, "user refinement");
});

test("prompt review owner change replaces an old owner's local edit", () => {
  const ui = createUiLocalState();
  const ownerA = projection({
    overlay: "prompt_review",
    review_draft_text: "owner A review",
    draft_target: { workspacePath: "C:/workspace", sessionId: "session-a", ownerGeneration: 1 },
  });
  reconcileUiDrafts(ui, null, ownerA, null);
  ui.drafts.reviewDraft = "owner A local edit";
  ui.drafts.reviewRevision += 1;

  const ownerB = projection({
    projection_revision: "2",
    overlay: "prompt_review",
    review_draft_text: "owner B review",
    draft_target: { workspacePath: "C:/workspace", sessionId: "session-b", ownerGeneration: 2 },
  });
  reconcileUiDrafts(ui, ownerA, ownerB, null);

  assert.equal(ui.drafts.reviewDraft, "owner B review");
  assert.equal(projectViewState(ownerB, ui).review_draft_text, "owner B review");
});

test("stale prompt review acknowledgement cannot cross an owner change", () => {
  const ui = createUiLocalState();
  const ownerA = projection({
    overlay: "prompt_review",
    review_draft_text: "owner A review",
    draft_target: { workspacePath: "C:/workspace", sessionId: "session-a", ownerGeneration: 1 },
  });
  reconcileUiDrafts(ui, null, ownerA, null);
  const staleSnapshot = captureDraftMutation(ui, "cancel_prompt_review");

  const ownerB = projection({
    projection_revision: "3",
    overlay: "prompt_review",
    review_draft_text: "owner B review",
    draft_target: { workspacePath: "C:/workspace", sessionId: "session-b", ownerGeneration: 2 },
  });
  reconcileUiDrafts(ui, ownerA, ownerB, null);

  const staleResponse = projection({
    projection_revision: "2",
    overlay: "none",
    review_draft_text: "",
    draft_target: { workspacePath: "C:/workspace", sessionId: "session-a", ownerGeneration: 1 },
  });
  acknowledgeDraftMutation(ui, staleResponse, "cancel_prompt_review", staleSnapshot);

  assert.equal(ui.drafts.reviewDraft, "owner B review");
  assert.equal(projectViewState(ownerB, ui).review_draft_text, "owner B review");
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

class FakeInteractionEventTarget {
  private readonly listeners = new Map<string, Set<EventListenerOrEventListenerObject>>();

  addEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject | null,
    _options?: boolean | AddEventListenerOptions,
  ): void {
    if (!listener) return;
    const listeners = this.listeners.get(type) ?? new Set<EventListenerOrEventListenerObject>();
    listeners.add(listener);
    this.listeners.set(type, listeners);
  }

  removeEventListener(
    type: string,
    listener: EventListenerOrEventListenerObject | null,
    _options?: boolean | EventListenerOptions,
  ): void {
    if (listener) this.listeners.get(type)?.delete(listener);
  }

  dispatch(type: string, values: Record<string, unknown> = {}): void {
    const event = { type, ...values } as unknown as Event;
    for (const listener of Array.from(this.listeners.get(type) ?? [])) {
      if (typeof listener === "function") listener.call(this, event);
      else listener.handleEvent(event);
    }
  }
}

class FakeInteractionWindow extends FakeInteractionEventTarget {
  private now = 0;
  private nextTimerId = 1;
  private readonly timers = new Map<number, { at: number; handler: TimerHandler; args: unknown[] }>();

  setTimeout(handler: TimerHandler, timeout = 0, ...args: unknown[]): number {
    const id = this.nextTimerId++;
    this.timers.set(id, { at: this.now + Math.max(0, timeout), handler, args });
    return id;
  }

  clearTimeout(id: number | undefined): void {
    if (id !== undefined) this.timers.delete(id);
  }

  advanceBy(milliseconds: number): void {
    const target = this.now + milliseconds;
    while (true) {
      const next = Array.from(this.timers.entries())
        .filter(([, timer]) => timer.at <= target)
        .sort((left, right) => left[1].at - right[1].at || left[0] - right[0])[0];
      if (!next) break;
      const [id, timer] = next;
      this.timers.delete(id);
      this.now = timer.at;
      if (typeof timer.handler !== "function") throw new Error("string timers are not supported by the fake clock");
      timer.handler(...timer.args);
    }
    this.now = target;
  }
}

class FakeInteractionDocument extends FakeInteractionEventTarget {
  hidden = false;
  body: FakeInteractionElement | null = null;
  documentElement: FakeInteractionElement | null = null;
}

class FakeInteractionElement extends FakeInteractionEventTarget {
  disabled = false;
  projection = "revision-1";
  replacements = 0;
  readonly kind: "html" | "body" | "div" | "input";
  readonly parentElement: FakeInteractionElement | null;

  constructor(
    kind: "html" | "body" | "div" | "input",
    parentElement: FakeInteractionElement | null = null,
  ) {
    super();
    this.kind = kind;
    this.parentElement = parentElement;
  }

  contains(target: Node | null): boolean {
    let current = target as unknown as FakeInteractionElement | null;
    while (current) {
      if (current === this) return true;
      current = current.parentElement;
    }
    return false;
  }

  closest<E extends Element = Element>(selectors: string): E | null {
    if (selectors === ":disabled") {
      return (this.disabled ? this : null) as unknown as E | null;
    }
    if (selectors === "[data-action]") return null;
    return (this.kind === "input" ? this : null) as unknown as E | null;
  }

  matches(selectors: string): boolean {
    return selectors === ":disabled" && this.disabled;
  }

  setPointerCapture(_pointerId: number): void {}

  applyProjection(revision: number): void {
    this.projection = `revision-${revision}`;
    this.replacements += 1;
  }
}

interface InteractionGateHarness {
  documentTarget: FakeInteractionDocument;
  windowTarget: FakeInteractionWindow;
  documentElement: FakeInteractionElement;
  body: FakeInteractionElement;
  appRoot: FakeInteractionElement;
  input: FakeInteractionElement;
  unrelated: FakeInteractionElement;
  disabledInput: FakeInteractionElement;
  lifecycle: InteractionLifecycle<number>;
  applied: number[];
  queueProjection: (revision: number) => void;
  dispose: () => void;
}

function withInteractionGate(run: (harness: InteractionGateHarness) => void): void {
  const elementDescriptor = Object.getOwnPropertyDescriptor(globalThis, "Element");
  Object.defineProperty(globalThis, "Element", {
    configurable: true,
    writable: true,
    value: FakeInteractionElement,
  });
  const documentTarget = new FakeInteractionDocument();
  const windowTarget = new FakeInteractionWindow();
  const documentElement = new FakeInteractionElement("html");
  const body = new FakeInteractionElement("body", documentElement);
  const appRoot = new FakeInteractionElement("div", body);
  const input = new FakeInteractionElement("input", appRoot);
  const unrelated = new FakeInteractionElement("div", body);
  const disabledInput = new FakeInteractionElement("input", appRoot);
  disabledInput.disabled = true;
  documentTarget.documentElement = documentElement;
  documentTarget.body = body;
  const lifecycle = new InteractionLifecycle<number>((current, candidate) => candidate > current);
  const applied: number[] = [];
  const applyProjection = (revision: number): void => {
    appRoot.applyProjection(revision);
    applied.push(revision);
  };
  const dispose = installInteractionEventGate({
    documentTarget: documentTarget as unknown as Document,
    windowTarget: windowTarget as unknown as Window,
    appRoot: appRoot as unknown as Element,
    lifecycle,
    finish: (release) => {
      if (release?.deferred !== null && release?.deferred !== undefined) {
        applyProjection(release.deferred);
      }
    },
  });

  try {
    run({
      documentTarget,
      windowTarget,
      documentElement,
      body,
      appRoot,
      input,
      unrelated,
      disabledInput,
      lifecycle,
      applied,
      queueProjection: (revision) => {
        if (!lifecycle.defer(revision, false, true)) applyProjection(revision);
      },
      dispose,
    });
  } finally {
    dispose();
    if (elementDescriptor) Object.defineProperty(globalThis, "Element", elementDescriptor);
    else delete (globalThis as Record<string, unknown>).Element;
  }
}

test("interaction lifecycle holds one newest projection across pointer, keyboard, and IME", () => {
  const lifecycle = new InteractionLifecycle<number>((current, candidate) => candidate > current);
  lifecycle.beginPointer(7);
  lifecycle.beginKey("Enter");
  lifecycle.beginComposition();
  const endPointer = lifecycle.capturePointerEnd(7);
  const endKey = lifecycle.captureKeyEnd("Enter");
  const endComposition = lifecycle.captureCompositionEnd();

  assert.equal(lifecycle.defer(2, false, true), true);
  assert.equal(lifecycle.defer(1, false, true), true);
  assert.equal(endPointer?.(), null);
  assert.equal(endKey?.(), null);
  assert.deepEqual(endComposition?.(), { deferred: 2, renderCurrent: false });
  assert.equal(lifecycle.active, false);
});

test("a paused IME keeps the DOM stable until compositionend releases only the newest projection", () => {
  withInteractionGate(({ documentTarget, windowTarget, appRoot, lifecycle, applied, queueProjection }) => {
    documentTarget.dispatch("compositionstart");
    queueProjection(2);
    queueProjection(3);

    windowTarget.advanceBy(60_000);
    assert.equal(lifecycle.active, true);
    assert.equal(appRoot.projection, "revision-1");
    assert.equal(appRoot.replacements, 0);

    documentTarget.dispatch("compositionend");
    assert.equal(appRoot.projection, "revision-1", "the compositionend event settles after its input event turn");
    windowTarget.advanceBy(0);
    assert.equal(lifecycle.active, false);
    assert.equal(appRoot.projection, "revision-3");
    assert.deepEqual(applied, [3]);
    windowTarget.advanceBy(60_000);
    assert.deepEqual(applied, [3]);
  });
});

test("a stationary pointer keeps the DOM stable until lost capture releases only the newest projection", () => {
  withInteractionGate(({ documentTarget, windowTarget, appRoot, input, lifecycle, applied, queueProjection }) => {
    documentTarget.dispatch("pointerdown", { target: input, button: 0, pointerId: 7 });
    queueProjection(2);
    queueProjection(4);

    windowTarget.advanceBy(60_000);
    assert.equal(lifecycle.active, true);
    assert.equal(appRoot.projection, "revision-1");
    assert.equal(appRoot.replacements, 0);

    documentTarget.dispatch("lostpointercapture", { target: input, pointerId: 7 });
    windowTarget.advanceBy(0);
    assert.equal(lifecycle.active, false);
    assert.equal(appRoot.projection, "revision-4");
    assert.deepEqual(applied, [4]);
    windowTarget.advanceBy(60_000);
    assert.deepEqual(applied, [4]);
  });
});

test("a held key keeps the DOM stable until window blur explicitly recovers the lifecycle", () => {
  withInteractionGate(({ documentTarget, windowTarget, appRoot, input, lifecycle, applied, queueProjection }) => {
    documentTarget.dispatch("keydown", {
      target: input,
      code: "ArrowDown",
      isComposing: false,
    });
    queueProjection(5);

    windowTarget.advanceBy(60_000);
    assert.equal(lifecycle.active, true);
    assert.equal(appRoot.projection, "revision-1");

    windowTarget.dispatch("blur");
    assert.equal(lifecycle.active, false);
    assert.equal(appRoot.projection, "revision-5");
    assert.deepEqual(applied, [5]);
  });
});

test("document visibility loss explicitly recovers a missing compositionend", () => {
  withInteractionGate(({ documentTarget, appRoot, lifecycle, applied, queueProjection }) => {
    documentTarget.dispatch("compositionstart");
    queueProjection(6);
    documentTarget.hidden = true;

    documentTarget.dispatch("visibilitychange");

    assert.equal(lifecycle.active, false);
    assert.equal(appRoot.projection, "revision-6");
    assert.deepEqual(applied, [6]);
  });
});

test("installed keyboard events admit BODY and HTML reading operations but reject unrelated and disabled targets", () => {
  withInteractionGate(({
    documentTarget,
    windowTarget,
    documentElement,
    body,
    unrelated,
    disabledInput,
    lifecycle,
  }) => {
    documentTarget.dispatch("keydown", { target: body, code: "PageDown", isComposing: false });
    assert.equal(lifecycle.active, true, "BODY owns document-level reading keys");
    documentTarget.dispatch("keyup", { target: body, code: "PageDown" });
    windowTarget.advanceBy(0);
    assert.equal(lifecycle.active, false);

    documentTarget.dispatch("keydown", { target: documentElement, code: "ArrowDown", isComposing: false });
    assert.equal(lifecycle.active, true, "HTML owns document-level reading keys");
    windowTarget.dispatch("blur");
    assert.equal(lifecycle.active, false);

    documentTarget.dispatch("keydown", { target: unrelated, code: "ArrowDown", isComposing: false });
    assert.equal(lifecycle.active, false, "an unrelated element outside #app is not a document owner");
    documentTarget.dispatch("keydown", { target: disabledInput, code: "Enter", isComposing: false });
    assert.equal(lifecycle.active, false, "disabled controls remain outside keyboard admission");
  });
});

test("delayed pointer termination cannot end a newer generation with the same pointer id", () => {
  for (const termination of ["pointerup", "lostpointercapture"] as const) {
    withInteractionGate(({ documentTarget, windowTarget, appRoot, input, lifecycle, applied, queueProjection }) => {
      documentTarget.dispatch("pointerdown", { target: input, button: 0, pointerId: 9 });
      queueProjection(2);
      documentTarget.dispatch(termination, { target: input, pointerId: 9 });
      documentTarget.dispatch("pointerdown", { target: input, button: 0, pointerId: 9 });
      queueProjection(3);

      windowTarget.advanceBy(0);
      assert.equal(lifecycle.active, true, termination);
      assert.equal(appRoot.projection, "revision-1", termination);
      assert.deepEqual(applied, [], termination);

      documentTarget.dispatch("pointerup", { target: input, pointerId: 9 });
      windowTarget.advanceBy(0);
      assert.equal(lifecycle.active, false, termination);
      assert.equal(appRoot.projection, "revision-3", termination);
      assert.deepEqual(applied, [3], termination);
    });
  }
});

test("delayed keyup cannot end a newer generation with the same key code", () => {
  withInteractionGate(({ documentTarget, windowTarget, appRoot, input, lifecycle, applied, queueProjection }) => {
    documentTarget.dispatch("keydown", { target: input, code: "ArrowDown", isComposing: false });
    queueProjection(2);
    documentTarget.dispatch("keyup", { target: input, code: "ArrowDown" });
    documentTarget.dispatch("keydown", { target: input, code: "ArrowDown", isComposing: false });
    queueProjection(4);

    windowTarget.advanceBy(0);
    assert.equal(lifecycle.active, true);
    assert.equal(appRoot.projection, "revision-1");
    assert.deepEqual(applied, []);

    documentTarget.dispatch("keyup", { target: input, code: "ArrowDown" });
    windowTarget.advanceBy(0);
    assert.equal(lifecycle.active, false);
    assert.equal(appRoot.projection, "revision-4");
    assert.deepEqual(applied, [4]);
  });
});

test("duplicate delayed compositionend callbacks cannot end a newer composition", () => {
  withInteractionGate(({ documentTarget, windowTarget, appRoot, lifecycle, applied, queueProjection }) => {
    documentTarget.dispatch("compositionstart");
    queueProjection(2);
    documentTarget.dispatch("compositionend");
    documentTarget.dispatch("compositionend");
    documentTarget.dispatch("compositionstart");
    queueProjection(5);

    windowTarget.advanceBy(0);
    assert.equal(lifecycle.active, true);
    assert.equal(appRoot.projection, "revision-1");
    assert.deepEqual(applied, []);

    documentTarget.dispatch("compositionend");
    windowTarget.advanceBy(0);
    assert.equal(lifecycle.active, false);
    assert.equal(appRoot.projection, "revision-5");
    assert.deepEqual(applied, [5]);
  });
});

test("double termination releases a deferred projection exactly once", () => {
  withInteractionGate(({ documentTarget, windowTarget, input, lifecycle, applied, queueProjection }) => {
    documentTarget.dispatch("pointerdown", { target: input, button: 0, pointerId: 11 });
    queueProjection(6);
    documentTarget.dispatch("pointerup", { target: input, pointerId: 11 });
    documentTarget.dispatch("lostpointercapture", { target: input, pointerId: 11 });

    windowTarget.advanceBy(0);
    assert.equal(lifecycle.active, false);
    assert.deepEqual(applied, [6]);
    windowTarget.dispatch("blur");
    assert.deepEqual(applied, [6]);
  });
});

test("blur invalidates a queued end before the same owner begins again", () => {
  withInteractionGate(({ documentTarget, windowTarget, appRoot, input, lifecycle, applied, queueProjection }) => {
    documentTarget.dispatch("pointerdown", { target: input, button: 0, pointerId: 13 });
    queueProjection(2);
    documentTarget.dispatch("pointerup", { target: input, pointerId: 13 });
    windowTarget.dispatch("blur");
    assert.deepEqual(applied, [2]);

    documentTarget.dispatch("pointerdown", { target: input, button: 0, pointerId: 13 });
    queueProjection(7);
    windowTarget.advanceBy(0);
    assert.equal(lifecycle.active, true);
    assert.equal(appRoot.projection, "revision-2");
    assert.deepEqual(applied, [2]);

    documentTarget.dispatch("pointercancel", { target: input, pointerId: 13 });
    assert.equal(lifecycle.active, false);
    assert.equal(appRoot.projection, "revision-7");
    assert.deepEqual(applied, [2, 7]);
  });
});

test("disposing the installed gate drops deferred state and cancels queued end callbacks", () => {
  withInteractionGate(({ documentTarget, windowTarget, appRoot, input, lifecycle, applied, queueProjection, dispose }) => {
    documentTarget.dispatch("pointerdown", { target: input, button: 0, pointerId: 15 });
    queueProjection(8);
    documentTarget.dispatch("pointerup", { target: input, pointerId: 15 });

    dispose();
    windowTarget.advanceBy(0);

    assert.equal(lifecycle.active, false);
    assert.equal(appRoot.projection, "revision-1");
    assert.deepEqual(applied, []);
  });
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

test("access modes use the Codex-aligned Japanese labels", () => {
  assert.deepEqual(
    ["default", "auto_review", "full_access"].map(displayAccessLabel),
    ["承認を求める", "代理で承認", "フルアクセス"],
  );
});

test("permission visibility does not stop runtime polling", () => {
  assert.equal(autoRefreshAllowed({ navigation_loading: false, confirmation_visible: true }, false), true);
  assert.equal(autoRefreshAllowed({ navigation_loading: false, confirmation_visible: true }, true), false);
  assert.equal(autoRefreshAllowed({ navigation_loading: true, confirmation_visible: true }, true), true);
});

test("run admission polls for the Rust owner before the start command responds", () => {
  assert.equal(runtimePollingRequired(false, false), false);
  assert.equal(runtimePollingRequired(true, false), true);
  assert.equal(runtimePollingRequired(false, true), true);
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
      expectedTarget: {
        workspacePath: "C:/workspace",
        sessionId: "session-a",
        ownerGeneration: 1,
      },
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
    prepareConfigSnapshot: () => state.config_fields.map((field) => ({
      key: field.key,
      text: field.value,
    })),
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
      args: {
        expectedTarget: {
          workspacePath: "C:/workspace",
          sessionId: "session-a",
          ownerGeneration: 1,
        },
      },
    },
    {
      name: "toggle_access_mode",
      args: {
        expectedTarget: {
          workspacePath: "C:/workspace",
          sessionId: "session-a",
          configGeneration: "1",
          accessMode: "default",
          runtimeOwnerToken: "idle:0",
        },
        draftValues: [{ key: "model.model", text: "model-a" }],
      },
    },
  ]);
  assert.equal(
    actionById("toggle-access")?.enabled?.(
      projection({
        config_draft: {
          ...projection().config_draft,
          access_mode_mutation_enabled: false,
        },
      }),
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
  const enabled = projection();
  const disabled = projection({
    config_draft: {
      ...projection().config_draft,
      access_mode_mutation_enabled: false,
    },
  });
  assert.match(
    renderTopbar(enabled),
    /data-action="toggle-access"[^>]*aria-disabled="false"(?![^>]*\sdisabled(?:\s|>|=))[^>]*>/,
  );
  assert.match(
    renderTopbar(disabled),
    /data-action="toggle-access"[^>]*aria-disabled="true"[^>]*\sdisabled>/,
  );
});

test("config commit capability never gates unrelated workspace or window actions", () => {
  const clean = projection();
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
  assert.equal(dirtyView.config_draft.external_owner_mutation_open, false);
  assert.equal(dirtyView.config_draft.access_mode_mutation_enabled, false);
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
    access_label: "full_access",
    access_target: { ...before.access_target, accessMode: "full_access" },
    config_target: { ...before.config_target },
    config_fields: fields.map((field) => field.key === "permissions.access_mode"
      ? { ...field, value: "full_access" }
      : field),
  });

  const cleanUi = createUiLocalState();
  reconcileUiDrafts(cleanUi, null, after, null);
  assert.equal(
    projectViewState(after, cleanUi).config_fields
      .find((field) => field.key === "permissions.access_mode")?.value,
    "full_access",
    "a clean settings view consumes the new Rust baseline",
  );
});

test("local config and run-start mutations close external config owner admission", async () => {
  const state = projection();
  const configPending = createUiLocalState();
  reconcileUiDrafts(configPending, null, state, null);
  updateConfigDraftValue(
    configPending,
    state.config_target,
    state.config_fields.map((field) => ({ key: field.key, text: field.value })),
    "model.model",
    "pending-model",
  );
  configPending.activeConfigMutationGeneration = 1n;
  const configPendingView = projectViewState(state, configPending);
  assert.equal(configPendingView.config_draft.access_mode_mutation_enabled, false);
  assert.equal(configPendingView.provider_apply_enabled, false);
  assert.equal(configPendingView.config_draft.external_owner_mutation_open, false);
  assert.equal(configPendingView.config_draft.discard_enabled, false);
  assert.equal(configPendingView.config_draft.commit_enabled, false);
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
  const rustOwnedDirty = projection();
  const resumed = projectViewState(rustOwnedDirty, configPending);
  assert.equal(configDraftEditOpen(configPending), true);
  assert.equal(resumed.config_draft.discard_enabled, true);
  assert.equal(resumed.config_draft.commit_enabled, true);

  const runPending = createUiLocalState();
  reconcileUiDrafts(runPending, null, rustOwnedDirty, null);
  updateConfigDraftValue(
    runPending,
    rustOwnedDirty.config_target,
    rustOwnedDirty.config_fields.map((field) => ({ key: field.key, text: field.value })),
    "model.model",
    "run-pending-model",
  );
  runPending.runStartMutationPending = true;
  const runPendingView = projectViewState(rustOwnedDirty, runPending);
  assert.equal(runPendingView.config_draft.access_mode_mutation_enabled, false);
  assert.equal(runPendingView.provider_apply_enabled, false);
  assert.equal(runPendingView.config_draft.external_owner_mutation_open, false);
  assert.equal(runPendingView.config_draft.commit_enabled, false);
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
  assert.equal(projectViewState(raw, ui).config_draft.external_owner_mutation_open, false);

  const reopened = {
    ...raw,
    overlay: "config",
  };
  reconcileUiDrafts(ui, raw, reopened, null);
  const reopenedView = projectViewState(reopened, ui);
  assert.equal(reopenedView.config_draft.dirty, true);
  assert.equal(actionById("discard-config-draft")?.enabled?.(
    reopenedView,
    { index: -1, value: "" },
  ), true);
  discardConfigDraft(ui);

  const rustOwnedClean = {
    ...reopened,
  };
  const cleanView = projectViewState(rustOwnedClean, ui);
  assert.equal(ui.configDirty, false);
  assert.equal(cleanView.config_draft.external_owner_mutation_open, true);
  assert.equal(cleanView.config_draft.access_mode_mutation_enabled, true);
  assert.equal(cleanView.provider_apply_enabled, true);
});

test("external config mutation roundtrip prevents a settings draft from starting", async () => {
  const state = projection({ overlay: "config" });
  state.startup.initial_setup_required = true;
  const ui = createUiLocalState();
  reconcileUiDrafts(ui, null, state, null);
  ui.externalConfigMutationPending = true;

  const pending = projectViewState(state, ui);
  assert.equal(configDraftEditOpen(ui), false);
  assert.equal(pending.config_draft.external_owner_mutation_open, false);
  assert.equal(pending.config_draft.access_mode_mutation_enabled, false);
  assert.equal(pending.provider_apply_enabled, false);
  assert.equal(pending.config_draft.commit_enabled, false);
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

test("canonical row_kind selects specialized transcript rendering", () => {
  const html = renderThreadContent(projection({
    thread_empty: false,
    transcript_rows: [{
      row_kind: "work_summary_running",
      step: "1",
      title: "Work",
      body: "running",
      file_changes: [],
    }],
  }));
  assert.match(html, /message work-summary work_summary_running/);
  assert.match(html, /<details[^>]+open>/);
});

test("config owner generation remains exact beyond JavaScript's safe integer range", () => {
  const current = {
    ...projection().config_target,
    configGeneration: "9007199254740993",
  };
  const newerFence = {
    ...current,
    configGeneration: "9007199254740994",
  };

  assert.equal(sameConfigMutationTarget(current, { ...current }), true);
  assert.equal(sameConfigMutationTarget(current, newerFence), false);
});

test("run target mirrors the exact Rust Stop owner projection", () => {
  const target = projection().run_target;

  assert.deepEqual(
    Object.keys(target).sort(),
    ["runtimeOwnerToken", "sessionId", "workspacePath"],
  );
  assert.equal("ownerGeneration" in target, false);
});

test("incomplete canonical turn is rendered as nonterminal evidence", () => {
  const html = renderThreadContent(projection({
    thread_empty: false,
    transcript_rows: [{
      row_kind: "work_summary_incomplete",
      step: "1",
      title: "状態未確定の作業履歴",
      body: "### 作業サマリ\n- 結果: この turn の完了状態は未確定です。",
      file_changes: [],
    }],
  }));
  assert.match(html, /message work-summary work_summary_incomplete/);
  assert.match(html, /状態未確定/);
  assert.match(html, /<details[^>]+open>/);
});

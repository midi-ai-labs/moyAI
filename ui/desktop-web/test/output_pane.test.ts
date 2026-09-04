import assert from "node:assert/strict";
import test from "node:test";

import { actionById, type ActionContext } from "../src/actions.ts";
import { renderArtifactPane, renderTopbar } from "../src/render.ts";
import {
  DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION,
  type DesktopRenderLocalPresentation,
} from "../src/render_projection.ts";
import type { DesktopViewState, DesktopWebState } from "../src/types.ts";
import { createUiLocalState, setArtifactPaneCollapsed } from "../src/ui_state.ts";

function useOutputPane(mode: "output" | "agents" = "output"): DesktopRenderLocalPresentation {
  return {
    ...DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION,
    artifactPane: {
      ...DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION.artifactPane,
      collapsed: false,
      mode,
    },
  };
}

function outputState(): DesktopWebState {
  const artifactPath = "C:/workspace/深い&folder/<report>.md";
  return {
    draft_target: { workspacePath: "C:/workspace", sessionId: "session-a", ownerGeneration: "1" },
    side_chat: {
      configured: false,
      deleting: false,
      chat_id: null,
      owner_session_id: "session-a",
      model: "",
      system_prompt: "",
      base_url: "",
      status: "idle",
      phase: "idle",
      last_error: "",
      generation: "0",
      draft_text: "",
      draft_quote: null,
      draft_revision: "0",
      context_scope: "owner_session",
      context_as_of_append_position: null,
      context_truncated: false,
      messages: [],
      can_send: false,
      can_cancel: false,
    },
    navigation_admission_open: true,
    busy: true,
    agent_tree_active: true,
    agent_activity_rows: [],
    artifact_rows: [{
      label: "長いレポート",
      path: artifactPath,
      kind: "file",
      action: "created",
    }],
    selected_artifact_index: 0,
    artifact_preview_available: true,
    artifact_preview_text: "preview\ncontent",
    progress_text: "フェーズ: 検証",
    tool_status_text: "- read [completed]",
    plan: {
      explanation: "長い日本語と English explanation を確認する",
      steps: [
        { step: "owner を確認する", status: "completed" },
        { step: "長い日本語と an_unbroken_value_that_must_wrap を検証する", status: "in_progress" },
        { step: "GUIを確認する", status: "pending" },
      ],
    },
  } as DesktopWebState;
}

test("output pane uses one document-flow scroll owner with semantic ordered sections", () => {
  const local = useOutputPane();
  const html = renderArtifactPane(outputState(), local);

  assert.equal(html.match(/class="output-scroll"/g)?.length, 1);
  assert.match(html, /class="output-scroll"[^>]*role="region"[^>]*aria-label="出力内容"[^>]*tabindex="0"/);
  assert.match(html, /<aside class="artifact-pane" data-pane-mode="output" aria-labelledby="output-pane-heading">/);
  assert.match(html, /<h2 id="output-pane-heading">出力<\/h2>/);
  assert.match(html, /<h3 id="output-plan-heading">計画<\/h3>/);
  assert.match(html, /<h3 id="output-files-heading">ファイル<\/h3>/);
  assert.match(html, /<h3 id="output-preview-heading">プレビュー<\/h3>/);
  assert.match(html, /<h3 id="output-activity-heading">進捗／ツール<\/h3>/);
  assert.match(html, /aria-label="完全な実行履歴への導線"/);
  assert.match(html, /完全な詳細はcanonical会話履歴に残ります/);
  assert.match(html, /data-action="export-transcript"/);
  assert.match(html, /<ol class="plan-list">/);
  assert.match(html, /<ul class="artifact-list">/);
  assert.match(html, /class="plan-step-status">進行中<\/span><span class="plan-step-copy">/);

  const scrollStart = html.indexOf('<div class="output-scroll"');
  const agentSection = html.indexOf('class="output-agent-section"', scrollStart);
  const planSection = html.indexOf('class="output-file-section output-plan-section"', scrollStart);
  const fileSection = html.indexOf('aria-labelledby="output-files-heading"', scrollStart);
  const previewSection = html.indexOf('class="preview output-preview-section"', scrollStart);
  const activitySection = html.indexOf('class="activity output-activity-section"', scrollStart);
  assert.ok(scrollStart >= 0);
  assert.ok(scrollStart < agentSection && agentSection < planSection);
  assert.ok(planSection < fileSection && fileSection < previewSection && previewSection < activitySection);
});

test("artifact row exposes the complete path while keeping selected and focus identity", () => {
  const local = useOutputPane();
  const html = renderArtifactPane(outputState(), local);

  assert.match(html, /data-focus-key="artifact:C:\/workspace\/深い&amp;folder\/&lt;report&gt;\.md" aria-current="true"/);
  assert.match(html, /title="C:\/workspace\/深い&amp;folder\/&lt;report&gt;\.md"/);
  assert.match(html, /aria-label="長いレポート: C:\/workspace\/深い&amp;folder\/&lt;report&gt;\.md"/);
  assert.match(html, /class="artifact-row-copy"/);
});

test("Sub Agent inspector keeps its independent pane contract", () => {
  const local = useOutputPane("agents");
  const html = renderArtifactPane(outputState(), local);

  assert.match(html, /class="artifact-pane agent-inspector-pane"/);
  assert.doesNotMatch(html, /class="output-scroll"/);
  assert.doesNotMatch(html, /output-pane-heading/);
});

test("the topbar keeps a visible responsive route to the right pane", () => {
  const local = useOutputPane();
  const state = {
    selected_project_index: -1,
    selected_session_title: "Session",
    status_detail: "",
    status_message: "Ready",
    history_export_enabled: true,
    navigation_admission_open: true,
    navigation_loading: false,
    background_mutation_pending: false,
    config_draft: { access_mode_mutation_enabled: true },
    access_label: "default",
    provider_label: "LM Studio",
    model_label: "model",
    workspace_path: "C:/workspace",
  } as DesktopViewState;

  const expanded = renderTopbar(state, local);
  assert.match(expanded, /class="icon-button responsive-output-toggle"/);
  assert.match(expanded, /data-action="toggle-artifact-pane"[^>]*aria-expanded="true"/);

  const collapsed = {
    ...local,
    artifactPane: { ...local.artifactPane, collapsed: true },
  };
  assert.match(
    renderTopbar(state, collapsed),
    /aria-label="右ペインを表示"[^>]*aria-expanded="false"/,
  );
});

test("right-pane toggle requests deterministic focus inside the drawer and back on its trigger", async () => {
  const uiState = createUiLocalState();
  setArtifactPaneCollapsed(uiState, true);
  let rerenders = 0;
  const context = {
    uiState,
    rerender: () => { rerenders += 1; },
  } as ActionContext;
  const action = actionById("toggle-artifact-pane");
  assert.ok(action);

  await action.run(outputState(), context, { index: -1, value: "" });
  assert.equal(uiState.artifactPaneCollapsed, false);
  assert.equal(uiState.artifactPaneFocusAfterRender, "content");

  await action.run(outputState(), context, { index: -1, value: "" });
  assert.equal(uiState.artifactPaneCollapsed, true);
  assert.equal(uiState.artifactPaneFocusAfterRender, "trigger");
  assert.equal(rerenders, 2);
});

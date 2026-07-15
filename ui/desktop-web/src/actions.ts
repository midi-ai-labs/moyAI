import { command } from "./api.ts";
import {
  beginConfigMutation,
  configMutationPending,
  discardConfigDraft,
  finishConfigMutation,
  type ConfigValueInput,
} from "./config_mutation.ts";
import { finishLocalDecision, type PermissionReviewDecision } from "./decision_state.ts";
import {
  navigationIsIdle,
  sessionActionIndex,
  sessionRowActionAvailable,
} from "./navigation_state.ts";
import { rowMutationArgs } from "./row_target.ts";
import { runCanBeCancelled } from "./run_control.ts";
import type { ConfigMutationTarget, DesktopWebState, ProjectRow, SessionRow } from "./types.ts";
import {
  openAgentPane,
  setArtifactPaneCollapsed,
  showOutputPane,
  type UiLocalState,
} from "./ui_state.ts";
import {
  composerCapabilities,
  configDraftCommitOpen,
  configDraftDiscardOpen,
  draftMutationTarget,
  providerCapabilities,
  providerDraftPayload,
} from "./view_state.ts";

export type ActionMenu = "file" | "edit" | "view" | "help";

export interface ActionPayload {
  index: number;
  value: string;
}

export interface ActionContext {
  desktopWindow: {
    hide: () => Promise<void>;
    minimize: () => Promise<void>;
    toggleMaximize: () => Promise<void>;
    startDragging: () => Promise<void>;
  };
  uiState: UiLocalState;
  getProjection: () => DesktopWebState | null;
  getViewState: () => DesktopWebState | null;
  acceptProjection: (state: DesktopWebState, render?: boolean) => void;
  rerender: () => void;
  mutate: (name: string, args?: Record<string, unknown>) => Promise<void>;
  recoverCommandConflict: (error: unknown) => boolean;
  reportError: (error: unknown) => void;
  prepareConfigMutation: (target: ConfigMutationTarget) => ConfigValueInput[] | null;
  submitPermissionDecision: (decision: PermissionReviewDecision) => Promise<void>;
  setWindowMaximized: (maximized: boolean) => void;
}

export interface ActionDefinition {
  id: string;
  label: string;
  shortcut?: string;
  menu?: ActionMenu;
  palette?: boolean;
  enabled?: (state: DesktopWebState, payload: ActionPayload) => boolean;
  run: (state: DesktopWebState, context: ActionContext, payload: ActionPayload) => void | Promise<void>;
}

function always(): boolean {
  return true;
}

async function runWithoutRender(name: string, context: ActionContext): Promise<void> {
  context.acceptProjection(await command<DesktopWebState>(name), false);
}

function selectedSessionAvailable(state: DesktopWebState): boolean {
  return state.selected_session_index >= 0 && state.session_rows[state.selected_session_index] !== undefined;
}

function selectedArtifactAvailable(state: DesktopWebState): boolean {
  return state.selected_artifact_index >= 0
    && state.artifact_rows[state.selected_artifact_index] !== undefined;
}

function targetSessionIndex(state: DesktopWebState, payload: ActionPayload): number {
  return sessionActionIndex(state.selected_session_index, payload.index);
}

function targetSessionAvailable(state: DesktopWebState, payload: ActionPayload): boolean {
  const index = targetSessionIndex(state, payload);
  return sessionRowActionAvailable(
    state.session_rows.length,
    state.selected_session_index,
    payload.index,
  ) && state.session_rows[index] !== undefined;
}

function targetSessionNotBusy(state: DesktopWebState, payload: ActionPayload): boolean {
  return targetSessionAvailable(state, payload) && navigationIsIdle(state);
}

function targetSessionRow(state: DesktopWebState, payload: ActionPayload): SessionRow | undefined {
  return state.session_rows[targetSessionIndex(state, payload)];
}

function targetSessionActive(state: DesktopWebState, payload: ActionPayload): boolean {
  return targetSessionNotBusy(state, payload) && targetSessionRow(state, payload)?.loaded_status === "active";
}

function targetSessionInactive(state: DesktopWebState, payload: ActionPayload): boolean {
  return targetSessionNotBusy(state, payload) && targetSessionRow(state, payload)?.loaded_status !== "active";
}

function targetSessionArchiveable(state: DesktopWebState, payload: ActionPayload): boolean {
  const row = targetSessionRow(state, payload);
  return targetSessionInactive(state, payload) && row?.archived === false;
}

function targetSessionRestorable(state: DesktopWebState, payload: ActionPayload): boolean {
  const row = targetSessionRow(state, payload);
  return targetSessionNotBusy(state, payload) && row?.archived === true;
}

function targetQuickChatDeleteAvailable(state: DesktopWebState, payload: ActionPayload): boolean {
  const row = state.chat_session_rows[payload.index];
  return Boolean(row) && row.loaded_status !== "active" && navigationIsIdle(state);
}

async function runConfigMutation(
  name: "apply_session_config" | "save_global_config",
  context: ActionContext,
): Promise<void> {
  if (configMutationPending(context.uiState)) return;
  const current = context.getViewState();
  if (!current) return;
  if (!configDraftCommitOpen(context.uiState, current.startup.initial_setup_required)) return;
  const values = context.prepareConfigMutation(current.config_target);
  if (!values) return;
  const request = beginConfigMutation(context.uiState, current.config_target);
  context.rerender();
  let state: DesktopWebState;
  let succeeded: boolean;
  try {
    [state, succeeded] = await command<[DesktopWebState, boolean]>(name, {
      values,
      expectedTarget: request.target,
    });
  } catch (error) {
    const finished = finishConfigMutation(context.uiState, request, false, context.getViewState()?.config_target ?? null);
    if (context.recoverCommandConflict(error)) return;
    if (!finished) return;
    context.rerender();
    context.reportError(error);
    return;
  }
  if (!finishConfigMutation(context.uiState, request, succeeded, context.getViewState()?.config_target ?? null)) return;
  context.acceptProjection(state);
}

async function runSessionRowMutation(
  name: string,
  state: DesktopWebState,
  context: ActionContext,
  payload: ActionPayload,
): Promise<void> {
  const index = targetSessionIndex(state, payload);
  const args = rowMutationArgs(state, index, state.session_rows[index]?.session_id);
  if (args) await context.mutate(name, args);
}

async function runTurnPageMutation(
  name: "load_previous_turn_page" | "load_next_turn_page",
  state: DesktopWebState,
  context: ActionContext,
): Promise<void> {
  const index = state.selected_session_index;
  const args = rowMutationArgs(state, index, state.session_rows[index]?.session_id);
  if (args) {
    await context.mutate(name, { ...args, expectedOffset: state.turn_page_offset });
  }
}

function canSubmit(state: DesktopWebState): boolean {
  return state.can_submit;
}

export const ACTIONS: ActionDefinition[] = [
  {
    id: "send",
    label: "送信",
    shortcut: "Ctrl+Enter",
    palette: true,
    enabled: canSubmit,
    run: (state, context) => context.mutate("submit_prompt", {
      text: state.draft_prompt,
      expectedTarget: draftMutationTarget(state),
    }),
  },
  {
    id: "cancel-run",
    label: "実行停止",
    palette: true,
    enabled: runCanBeCancelled,
    run: (_state, context) => context.mutate("cancel_run"),
  },
  {
    id: "refresh",
    label: "更新",
    menu: "view",
    palette: true,
    enabled: always,
    run: (_state, context) => context.mutate("refresh_desktop"),
  },
  {
    id: "new-chat",
    label: "新しいチャット",
    shortcut: "Ctrl+N",
    menu: "file",
    palette: true,
    enabled: navigationIsIdle,
    run: (_state, context) => context.mutate("new_chat"),
  },
  {
    id: "show-command-palette",
    label: "コマンドパレット",
    shortcut: "Ctrl+K",
    menu: "edit",
    palette: true,
    enabled: always,
    run: (_state, context) => context.mutate("show_command_palette"),
  },
  {
    id: "show-provider",
    label: "LLM / Provider 設定",
    menu: "view",
    palette: true,
    enabled: always,
    run: (_state, context) => context.mutate("show_provider_editor"),
  },
  {
    id: "show-config",
    label: "設定",
    menu: "view",
    palette: true,
    enabled: always,
    run: (_state, context) => context.mutate("show_config_editor"),
  },
  {
    id: "show-shortcuts",
    label: "ショートカット",
    menu: "help",
    palette: true,
    enabled: always,
    run: (_state, context) => context.mutate("show_shortcuts"),
  },
  {
    id: "create-project-from-picker",
    label: "プロジェクトを追加",
    menu: "file",
    palette: true,
    enabled: navigationIsIdle,
    run: (_state, context) => context.mutate("create_project_from_picker"),
  },
  {
    id: "open-workspace-folder",
    label: "現在のフォルダーを開く",
    menu: "file",
    palette: true,
    enabled: always,
    run: (_state, context) => runWithoutRender("open_workspace_folder", context),
  },
  {
    id: "show-workspace-picker",
    label: "ワークスペースを切り替え",
    palette: true,
    enabled: navigationIsIdle,
    run: (_state, context) => context.mutate("show_workspace_picker"),
  },
  {
    id: "enhance-prompt",
    label: "プロンプトを推敲",
    menu: "edit",
    palette: true,
    enabled: (state) => state.enhance_enabled,
    run: (state, context) => context.mutate("enhance_prompt", {
      text: state.draft_prompt,
      expectedTarget: draftMutationTarget(state),
    }),
  },
  {
    id: "review-uncommitted",
    label: "未コミット差分をレビュー",
    palette: true,
    enabled: (state) => composerCapabilities(state, state.draft_prompt).canReviewUncommitted,
    run: (state, context) => context.mutate("review_uncommitted", {
      text: state.draft_prompt,
      expectedTarget: draftMutationTarget(state),
    }),
  },
  {
    id: "toggle-access",
    label: "アクセスモード切替",
    shortcut: "F8",
    palette: true,
    enabled: (state) => state.access_mode_mutation_enabled,
    run: (state, context) => context.mutate("toggle_access_mode", {
      expectedTarget: state.access_target,
    }),
  },
  {
    id: "discard-config-draft",
    label: "設定の変更を破棄",
    enabled: (state) => state.config_draft_discard_enabled,
    run: (_state, context) => {
      if (!configDraftDiscardOpen(context.uiState)) return;
      discardConfigDraft(context.uiState);
      context.rerender();
    },
  },
  {
    id: "toggle-session-archived-search",
    label: "アーカイブ済みを含める",
    shortcut: "Ctrl+I",
    palette: true,
    enabled: navigationIsIdle,
    run: (state, context) => context.mutate("set_session_search_include_archived", {
      includeArchived: !state.session_search_include_archived,
      expectedTarget: {
        workspacePath: state.workspace_path,
        projectId: state.project_rows[state.selected_project_index]?.project_id ?? null,
      },
    }),
  },
  {
    id: "export-transcript",
    label: "表示中 Transcript を Markdown 保存",
    shortcut: "F9",
    palette: true,
    enabled: (state) => state.history_export_enabled && navigationIsIdle(state),
    run: (state, context, payload) => runSessionRowMutation("export_transcript_markdown", state, context, payload),
  },
  {
    id: "export-history",
    label: "選択セッション履歴を Markdown 保存",
    palette: true,
    enabled: (state) => state.history_export_enabled && selectedSessionAvailable(state) && navigationIsIdle(state),
    run: (state, context, payload) => runSessionRowMutation("export_history_markdown", state, context, payload),
  },
  {
    id: "rejoin-session",
    label: "実行中セッションに再参加",
    palette: true,
    enabled: targetSessionActive,
    run: (state, context, payload) => runSessionRowMutation("rejoin_session", state, context, payload),
  },
  {
    id: "archive-session",
    label: "セッションをアーカイブ",
    palette: true,
    enabled: targetSessionArchiveable,
    run: (state, context, payload) => requestLocalArchiveState("archive_session", targetSessionIndex(state, payload), state, context),
  },
  {
    id: "unarchive-session",
    label: "セッションを復元",
    palette: true,
    enabled: targetSessionRestorable,
    run: (state, context, payload) => requestLocalArchiveState("unarchive_session", targetSessionIndex(state, payload), state, context),
  },
  {
    id: "rollback-session",
    label: "最新 turn を戻す",
    palette: true,
    enabled: targetSessionInactive,
    run: (state, context, payload) => requestLocalRollback(targetSessionIndex(state, payload), state, context),
  },
  {
    id: "fork-session",
    label: "セッションを fork",
    palette: true,
    enabled: targetSessionInactive,
    run: (state, context, payload) => runSessionRowMutation("fork_session", state, context, payload),
  },
  {
    id: "interrupt-session",
    label: "実行中セッションを interrupt",
    palette: true,
    enabled: targetSessionActive,
    run: (state, context, payload) => runSessionRowMutation("interrupt_session", state, context, payload),
  },
  {
    id: "delete-session",
    label: "セッションを削除",
    enabled: targetSessionInactive,
    run: (state, context, payload) => requestLocalDelete("session", targetSessionIndex(state, payload), state, context),
  },
  {
    id: "delete-chat-session",
    label: "チャットを削除",
    enabled: targetQuickChatDeleteAvailable,
    run: (state, context, payload) => requestLocalDelete("chat_session", payload.index, state, context),
  },
  {
    id: "delete-project",
    label: "プロジェクトを削除",
    enabled: navigationIsIdle,
    run: (state, context, payload) => requestLocalDelete("project", payload.index, state, context),
  },
  {
    id: "load-previous-turn-page",
    label: "前の履歴ページ",
    palette: true,
    enabled: (state) => navigationIsIdle(state) && state.turn_page_offset > 0,
    run: (state, context) => runTurnPageMutation("load_previous_turn_page", state, context),
  },
  {
    id: "load-next-turn-page",
    label: "次の履歴ページ",
    palette: true,
    enabled: (state) => navigationIsIdle(state) && state.turn_page_has_more,
    run: (state, context) => runTurnPageMutation("load_next_turn_page", state, context),
  },
  {
    id: "toggle-artifact-pane",
    label: "アーティファクトペイン切替",
    palette: true,
    enabled: always,
    run: (_state, context) => {
      const collapsing = !context.uiState.artifactPaneCollapsed;
      if (collapsing && context.uiState.artifactPaneMode === "agents") {
        showOutputPane(context.uiState);
      }
      setArtifactPaneCollapsed(context.uiState, collapsing);
      context.rerender();
    },
  },
  {
    id: "show-agent-pane",
    label: "Sub Agent履歴を表示",
    enabled: (state) => state.agent_activity_rows.length > 0,
    run: (state, context, payload) => {
      if (openAgentPane(context.uiState, state, payload.value)) context.rerender();
    },
  },
  {
    id: "show-output-pane",
    label: "出力ペインに戻る",
    enabled: always,
    run: (_state, context) => {
      showOutputPane(context.uiState);
      context.rerender();
    },
  },
  {
    id: "open-artifact-folder",
    label: "アーティファクトフォルダーを開く",
    palette: true,
    enabled: (state) => selectedArtifactAvailable(state) && navigationIsIdle(state),
    run: (state, context) => {
      const index = state.selected_artifact_index;
      const args = rowMutationArgs(state, index, state.artifact_rows[index]?.path);
      return args ? context.mutate("open_artifact_folder", args) : undefined;
    },
  },
  {
    id: "load-provider-models",
    label: "Provider モデル読込",
    palette: true,
    enabled: (state) => providerCapabilities(state).canLoadProviderModels,
    run: (state, context) => context.mutate(
      "load_provider_models",
      providerDraftPayload(
        context.uiState.drafts.provider,
        state.config_target,
        state.config_owner_mutation_open,
      ),
    ),
  },
  {
    id: "apply-provider-session",
    label: "Provider 設定を UI セッションに適用",
    palette: true,
    enabled: (state) => state.provider_apply_enabled,
    run: (state, context) => context.mutate(
      "apply_provider_session",
      providerDraftPayload(
        context.uiState.drafts.provider,
        state.config_target,
        state.config_owner_mutation_open,
      ),
    ),
  },
  {
    id: "save-provider-global",
    label: "Provider 設定をファイルに保存",
    palette: true,
    enabled: (state) => state.provider_apply_enabled,
    run: (state, context) => context.mutate(
      "save_provider_global",
      providerDraftPayload(
        context.uiState.drafts.provider,
        state.config_target,
        state.config_owner_mutation_open,
      ),
    ),
  },
  {
    id: "apply-session-config",
    label: "編集中の設定を UI セッションに適用",
    palette: true,
    enabled: (state) => state.config_draft_commit_enabled,
    run: (_state, context) => runConfigMutation("apply_session_config", context),
  },
  {
    id: "save-global-config",
    label: "編集中の設定を設定ファイルに保存",
    palette: true,
    enabled: (state) => state.config_draft_commit_enabled,
    run: (_state, context) => runConfigMutation("save_global_config", context),
  },
  {
    id: "set-provider-mode",
    label: "Provider mode 切替",
    enabled: always,
    run: (_state, context, payload) => {
      if (payload.value !== "lm_studio_native_required" && payload.value !== "openai_compatible_only") return;
      context.uiState.drafts.provider.metadataMode = payload.value;
      context.uiState.drafts.providerRevision += 1;
      context.rerender();
    },
  },
  {
    id: "select-provider-model",
    label: "Provider model 選択",
    enabled: always,
    run: (state, context, payload) => {
      const modelId = state.provider_model_ids[payload.index];
      if (!modelId) return;
      context.uiState.drafts.provider.selectedModelId = modelId;
      context.uiState.drafts.providerRevision += 1;
      context.rerender();
    },
  },
  {
    id: "switch-workspace",
    label: "ワークスペース切替",
    palette: true,
    enabled: navigationIsIdle,
    run: (state, context) => context.mutate("switch_workspace", {
      text: state.workspace_input,
      expectedTarget: draftMutationTarget(state),
    }),
  },
  {
    id: "browse-workspace",
    label: "ワークスペース参照",
    palette: true,
    enabled: always,
    run: (state, context) => context.mutate("browse_workspace", {
      text: state.workspace_input,
      expectedTarget: draftMutationTarget(state),
    }),
  },
  { id: "open-typed-path", label: "入力パスを開く", palette: true, enabled: always, run: (state, context) => context.mutate("open_typed_path", { text: state.workspace_input, expectedTarget: draftMutationTarget(state) }) },
  { id: "open-global-config-folder", label: "設定フォルダーを開く", palette: true, enabled: always, run: (_state, context) => runWithoutRender("open_global_config_folder", context) },
  { id: "set-image", label: "画像を添付", palette: true, enabled: (state) => state.image_input_enabled, run: (state, context) => context.mutate("attach_image", { text: state.image_input, expectedTarget: draftMutationTarget(state) }) },
  { id: "browse-image", label: "画像を参照", palette: true, enabled: (state) => state.image_input_enabled, run: (state, context) => context.mutate("browse_image", { expectedTarget: draftMutationTarget(state) }) },
  { id: "clear-images", label: "添付を解除", palette: true, enabled: (state) => state.attached_images.length > 0, run: (state, context) => context.mutate("clear_images", { expectedTarget: draftMutationTarget(state) }) },
  { id: "approve-permission", label: "確認した操作を実行", enabled: (state) => state.confirmation_visible, run: (_state, context) => context.submitPermissionDecision("approved") },
  { id: "abort-permission", label: "操作を実行せず指示を変更", enabled: (state) => state.confirmation_visible, run: (_state, context) => context.submitPermissionDecision("abort") },
  { id: "minimize-window", label: "最小化", enabled: always, run: () => command("minimize_window") },
  {
    id: "toggle-maximize-window",
    label: "最大化／元のサイズに戻す",
    enabled: always,
    run: async (_state, context) => context.setWindowMaximized(await command<boolean>("toggle_maximize_window")),
  },
  { id: "close-window", label: "閉じる", enabled: always, run: (_state, context) => command("hide_to_tray").catch(() => context.desktopWindow.hide()) },
];

export const ACTION_BY_ID = new Map(ACTIONS.map((action) => [action.id, action]));

export function actionById(id: string): ActionDefinition | undefined {
  return ACTION_BY_ID.get(id);
}

const NO_ACTION_PAYLOAD: ActionPayload = { index: -1, value: "" };

export function actionEnabled(
  action: ActionDefinition,
  state: DesktopWebState,
  payload: ActionPayload = NO_ACTION_PAYLOAD,
): boolean {
  return action.enabled ? action.enabled(state, payload) : true;
}

export function menuActions(menu: ActionMenu, state: DesktopWebState): ActionDefinition[] {
  return ACTIONS.filter((action) => action.menu === menu && actionEnabled(action, state));
}

export function shortcutActions(): ActionDefinition[] {
  return ACTIONS.filter((action) => action.shortcut);
}

export function paletteActions(
  state: DesktopWebState,
  configDraftAvailable = false,
): ActionDefinition[] {
  const query = state.local_search_text.trim().toLowerCase();
  return ACTIONS.filter((action) => action.palette)
    .filter((action) => {
      if (!query) return true;
      return action.label.toLowerCase().includes(query) || action.id.toLowerCase().includes(query);
    })
    .filter((action) =>
      configDraftAvailable
      || (action.id !== "apply-session-config" && action.id !== "save-global-config")
    )
    .filter((action) => actionEnabled(action, state));
}

export async function dispatchRegisteredAction(
  id: string,
  state: DesktopWebState,
  context: ActionContext,
  payload: ActionPayload,
): Promise<boolean> {
  const action = actionById(id);
  if (!action) {
    return false;
  }
  if (!actionEnabled(action, state, payload)) {
    return true;
  }
  await action.run(state, context, payload);
  return true;
}

function requestLocalArchiveState(
  kind: "archive_session" | "unarchive_session",
  index: number,
  state: DesktopWebState,
  context: ActionContext,
): void {
  if (!navigationIsIdle(state)) return;
  const row = state.session_rows[index];
  if (!row) return;
  const target = rowMutationArgs(state, index, row.session_id);
  if (!target) return;
  finishLocalDecision(context.uiState);
  context.uiState.pendingLocalConfirmation = {
    kind,
    index,
    title: row.label,
    detail: row.session_id,
    expectedTarget: target.expectedTarget,
  };
  context.rerender();
}

function requestLocalDelete(
  kind: "project" | "session" | "chat_session",
  index: number,
  state: DesktopWebState,
  context: ActionContext,
): void {
  if (!navigationIsIdle(state)) return;
  const row =
    kind === "project" ? state.project_rows[index] : kind === "chat_session" ? state.chat_session_rows[index] : state.session_rows[index];
  if (!row) return;
  const rowId = kind === "project" ? (row as ProjectRow).project_id : (row as SessionRow).session_id;
  const target = rowMutationArgs(state, index, rowId);
  if (!target) return;
  finishLocalDecision(context.uiState);
  context.uiState.pendingLocalConfirmation = {
    kind,
    index,
    title: row.label,
    detail: kind === "project" ? (row as ProjectRow).path : (row as SessionRow).session_id,
    expectedTarget: target.expectedTarget,
  };
  context.rerender();
}

function requestLocalRollback(index: number, state: DesktopWebState, context: ActionContext): void {
  if (!navigationIsIdle(state)) return;
  const row = state.session_rows[index];
  if (!row || row.loaded_status === "active") return;
  const target = rowMutationArgs(state, index, row.session_id);
  if (!target) return;
  finishLocalDecision(context.uiState);
  context.uiState.pendingLocalConfirmation = {
    kind: "rollback_session",
    index,
    title: row.label,
    detail: row.session_id,
    expectedTarget: target.expectedTarget,
  };
  context.rerender();
}

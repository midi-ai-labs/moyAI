import { command } from "./api";
import type { DesktopWebState, ProjectRow, SessionRow } from "./types";
import { setArtifactPaneCollapsed, type UiLocalState } from "./ui_state";
import { validateConfigInput } from "./utils";

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
  getCurrentState: () => DesktopWebState | null;
  setCurrentState: (state: DesktopWebState) => void;
  render: (state: DesktopWebState) => void;
  mutate: (name: string, args?: Record<string, unknown>) => Promise<void>;
  renderError: (message: string) => void;
  flushProviderInputMutations: () => Promise<void>;
  flushConfigInputMutations: () => Promise<void>;
}

export interface ActionDefinition {
  id: string;
  label: string;
  shortcut?: string;
  menu?: ActionMenu;
  palette?: boolean;
  enabled?: (state: DesktopWebState) => boolean;
  run: (state: DesktopWebState, context: ActionContext, payload: ActionPayload) => void | Promise<void>;
}

function always(): boolean {
  return true;
}

function selectedSessionAvailable(state: DesktopWebState): boolean {
  return state.selected_session_index >= 0;
}

function targetSessionIndex(state: DesktopWebState, payload: ActionPayload): number {
  return payload.index >= 0 ? payload.index : state.selected_session_index;
}

function selectedSessionNotBusy(state: DesktopWebState): boolean {
  return selectedSessionAvailable(state) && !state.busy;
}

function canSubmit(state: DesktopWebState): boolean {
  return state.can_submit;
}

function canCancel(state: DesktopWebState): boolean {
  return state.busy || state.confirmation_visible;
}

export const ACTIONS: ActionDefinition[] = [
  {
    id: "send",
    label: "送信",
    shortcut: "Ctrl+Enter",
    palette: true,
    enabled: canSubmit,
    run: (_state, context) => context.mutate("submit_prompt"),
  },
  {
    id: "cancel-run",
    label: "実行停止",
    palette: true,
    enabled: canCancel,
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
    enabled: always,
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
    enabled: always,
    run: (_state, context) => context.mutate("create_project_from_picker"),
  },
  {
    id: "open-workspace-folder",
    label: "現在のフォルダーを開く",
    menu: "file",
    palette: true,
    enabled: always,
    run: (_state, context) => context.mutate("open_workspace_folder"),
  },
  {
    id: "show-workspace-picker",
    label: "ワークスペースを切り替え",
    palette: true,
    enabled: (state) => !state.busy,
    run: (_state, context) => context.mutate("show_workspace_picker"),
  },
  {
    id: "enhance-prompt",
    label: "プロンプトを推敲",
    menu: "edit",
    palette: true,
    enabled: (state) => state.enhance_enabled,
    run: (_state, context) => context.mutate("enhance_prompt"),
  },
  {
    id: "review-uncommitted",
    label: "未コミット差分をレビュー",
    palette: true,
    enabled: (state) => !state.busy && state.draft_prompt.trim().length > 0,
    run: (_state, context) => context.mutate("review_uncommitted"),
  },
  {
    id: "toggle-access",
    label: "アクセスモード切替",
    shortcut: "F8",
    palette: true,
    enabled: (state) => !state.busy,
    run: (_state, context) => context.mutate("toggle_access_mode"),
  },
  {
    id: "toggle-session-archived-search",
    label: "アーカイブ済みを含める",
    shortcut: "Ctrl+I",
    palette: true,
    enabled: always,
    run: (state, context) => context.mutate("set_session_search_include_archived", { includeArchived: !state.session_search_include_archived }),
  },
  {
    id: "export-transcript",
    label: "表示中 Transcript を Markdown 保存",
    shortcut: "F9",
    palette: true,
    enabled: (state) => state.history_export_enabled,
    run: (_state, context) => context.mutate("export_transcript_markdown"),
  },
  {
    id: "export-history",
    label: "選択セッション履歴を Markdown 保存",
    palette: true,
    enabled: selectedSessionAvailable,
    run: (_state, context) => context.mutate("export_history_markdown"),
  },
  {
    id: "rejoin-session",
    label: "実行中セッションに再参加",
    palette: true,
    enabled: selectedSessionAvailable,
    run: (state, context, payload) => context.mutate("rejoin_session", { index: targetSessionIndex(state, payload) }),
  },
  {
    id: "archive-session",
    label: "セッションをアーカイブ",
    palette: true,
    enabled: selectedSessionNotBusy,
    run: (state, context, payload) => requestLocalArchiveState("archive_session", targetSessionIndex(state, payload), state, context),
  },
  {
    id: "unarchive-session",
    label: "セッションを復元",
    palette: true,
    enabled: selectedSessionNotBusy,
    run: (state, context, payload) => requestLocalArchiveState("unarchive_session", targetSessionIndex(state, payload), state, context),
  },
  {
    id: "rollback-session",
    label: "最新 turn を戻す",
    palette: true,
    enabled: selectedSessionNotBusy,
    run: (state, context, payload) => requestLocalRollback(targetSessionIndex(state, payload), state, context),
  },
  {
    id: "fork-session",
    label: "セッションを fork",
    palette: true,
    enabled: selectedSessionNotBusy,
    run: (state, context, payload) => context.mutate("fork_session", { index: targetSessionIndex(state, payload) }),
  },
  {
    id: "compact-session",
    label: "セッション履歴を compact",
    palette: true,
    enabled: selectedSessionNotBusy,
    run: (state, context, payload) => context.mutate("compact_session", { index: targetSessionIndex(state, payload) }),
  },
  {
    id: "interrupt-session",
    label: "実行中セッションを interrupt",
    palette: true,
    enabled: selectedSessionAvailable,
    run: (state, context, payload) => context.mutate("interrupt_session", { index: targetSessionIndex(state, payload) }),
  },
  {
    id: "enable-session-memory",
    label: "セッション memory を有効化",
    palette: true,
    enabled: selectedSessionNotBusy,
    run: (state, context, payload) => context.mutate("enable_session_memory", { index: targetSessionIndex(state, payload) }),
  },
  {
    id: "disable-session-memory",
    label: "セッション memory を無効化",
    palette: true,
    enabled: selectedSessionNotBusy,
    run: (state, context, payload) => context.mutate("disable_session_memory", { index: targetSessionIndex(state, payload) }),
  },
  {
    id: "show-session-settings",
    label: "セッション設定を開く",
    palette: true,
    enabled: selectedSessionAvailable,
    run: (_state, context) => context.mutate("show_provider_editor"),
  },
  {
    id: "delete-session",
    label: "セッションを削除",
    enabled: selectedSessionNotBusy,
    run: (state, context, payload) => requestLocalDelete("session", targetSessionIndex(state, payload), state, context),
  },
  {
    id: "delete-chat-session",
    label: "チャットを削除",
    enabled: (state) => !state.busy,
    run: (state, context, payload) => requestLocalDelete("chat_session", payload.index, state, context),
  },
  {
    id: "delete-project",
    label: "プロジェクトを削除",
    enabled: (state) => !state.busy,
    run: (state, context, payload) => requestLocalDelete("project", payload.index, state, context),
  },
  {
    id: "load-previous-turn-page",
    label: "前の履歴ページ",
    palette: true,
    enabled: (state) => !state.busy && state.turn_page_offset > 0,
    run: (_state, context) => context.mutate("load_previous_turn_page"),
  },
  {
    id: "load-next-turn-page",
    label: "次の履歴ページ",
    palette: true,
    enabled: (state) => !state.busy && state.turn_page_has_more,
    run: (_state, context) => context.mutate("load_next_turn_page"),
  },
  {
    id: "toggle-artifact-pane",
    label: "アーティファクトペイン切替",
    palette: true,
    enabled: always,
    run: (state, context) => {
      setArtifactPaneCollapsed(context.uiState, !context.uiState.artifactPaneCollapsed);
      context.render(state);
    },
  },
  {
    id: "open-artifact-folder",
    label: "アーティファクトフォルダーを開く",
    palette: true,
    enabled: always,
    run: (_state, context) => context.mutate("open_artifact_folder"),
  },
  {
    id: "load-provider-models",
    label: "Provider モデル読込",
    palette: true,
    enabled: (state) => !state.provider_loading,
    run: async (_state, context) => {
      await context.flushProviderInputMutations();
      await context.mutate("load_provider_models");
    },
  },
  {
    id: "apply-provider-session",
    label: "Provider 設定を UI セッションに適用",
    palette: true,
    enabled: (state) => state.provider_apply_enabled,
    run: async (_state, context) => {
      await context.flushProviderInputMutations();
      await context.mutate("apply_provider_session");
    },
  },
  {
    id: "save-provider-global",
    label: "Provider 設定をファイルに保存",
    palette: true,
    enabled: (state) => state.provider_apply_enabled,
    run: async (_state, context) => {
      await context.flushProviderInputMutations();
      await context.mutate("save_provider_global");
    },
  },
  {
    id: "apply-session-config",
    label: "設定を UI セッションに適用",
    palette: true,
    enabled: always,
    run: async (state, context) => {
      await context.flushConfigInputMutations();
      const result = validateConfigInput(state.config_field_title, state.config_value_text);
      if (!result.ok) return;
      context.uiState.configDirty = false;
      await context.mutate("apply_session_config");
    },
  },
  {
    id: "save-global-config",
    label: "設定ファイルに保存",
    palette: true,
    enabled: always,
    run: async (state, context) => {
      await context.flushConfigInputMutations();
      const result = validateConfigInput(state.config_field_title, state.config_value_text);
      if (!result.ok) return;
      context.uiState.configDirty = false;
      await context.mutate("save_global_config");
    },
  },
  {
    id: "set-provider-mode",
    label: "Provider mode 切替",
    enabled: always,
    run: (_state, context, payload) => context.mutate("set_provider_metadata_mode", { mode: payload.value }),
  },
  {
    id: "select-provider-model",
    label: "Provider model 選択",
    enabled: always,
    run: (_state, context, payload) => context.mutate("select_provider_model", { index: payload.index }),
  },
  {
    id: "select-config",
    label: "設定項目選択",
    enabled: always,
    run: (_state, context, payload) => {
      context.uiState.configDirty = false;
      return context.mutate("set_config_selection", { index: payload.index });
    },
  },
  {
    id: "switch-workspace",
    label: "ワークスペース切替",
    palette: true,
    enabled: (state) => !state.busy,
    run: (_state, context) => context.mutate("switch_workspace"),
  },
  { id: "browse-workspace", label: "ワークスペース参照", palette: true, enabled: always, run: (_state, context) => context.mutate("browse_workspace") },
  { id: "open-typed-path", label: "入力パスを開く", palette: true, enabled: always, run: (_state, context) => context.mutate("open_typed_path") },
  { id: "open-global-config-folder", label: "設定フォルダーを開く", palette: true, enabled: always, run: (_state, context) => context.mutate("open_global_config_folder") },
  { id: "set-image", label: "画像を添付", palette: true, enabled: (state) => state.image_input_enabled, run: (_state, context) => context.mutate("attach_image") },
  { id: "browse-image", label: "画像を参照", palette: true, enabled: (state) => state.image_input_enabled, run: (_state, context) => context.mutate("browse_image") },
  { id: "clear-images", label: "添付を解除", palette: true, enabled: (state) => state.attached_images.length > 0, run: (_state, context) => context.mutate("clear_images") },
  { id: "allow", label: "確認を許可", enabled: (state) => state.confirmation_visible, run: (_state, context) => context.mutate("answer_permission", { allow: true }) },
  { id: "deny", label: "確認を拒否", enabled: (state) => state.confirmation_visible, run: (_state, context) => context.mutate("answer_permission", { allow: false }) },
  { id: "minimize-window", label: "最小化", enabled: always, run: (_state, context) => context.desktopWindow.minimize() },
  { id: "toggle-maximize-window", label: "最大化", enabled: always, run: (_state, context) => context.desktopWindow.toggleMaximize() },
  { id: "close-window", label: "閉じる", enabled: always, run: (_state, context) => command("hide_to_tray").catch(() => context.desktopWindow.hide()) },
];

export const ACTION_BY_ID = new Map(ACTIONS.map((action) => [action.id, action]));

export function actionById(id: string): ActionDefinition | undefined {
  return ACTION_BY_ID.get(id);
}

export function actionEnabled(action: ActionDefinition, state: DesktopWebState): boolean {
  return action.enabled ? action.enabled(state) : true;
}

export function menuActions(menu: ActionMenu, state: DesktopWebState): ActionDefinition[] {
  return ACTIONS.filter((action) => action.menu === menu && actionEnabled(action, state));
}

export function shortcutActions(): ActionDefinition[] {
  return ACTIONS.filter((action) => action.shortcut);
}

export function paletteActions(state: DesktopWebState): ActionDefinition[] {
  const query = state.local_search_text.trim().toLowerCase();
  return ACTIONS.filter((action) => action.palette)
    .filter((action) => {
      if (!query) return true;
      return action.label.toLowerCase().includes(query) || action.id.toLowerCase().includes(query);
    })
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
  if (!actionEnabled(action, state)) {
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
  if (state.busy) return;
  const row = state.session_rows[index];
  if (!row) return;
  context.uiState.pendingLocalConfirmation = {
    kind,
    index,
    title: row.label,
    detail: row.session_id,
  };
  context.render(state);
}

function requestLocalDelete(
  kind: "project" | "session" | "chat_session",
  index: number,
  state: DesktopWebState,
  context: ActionContext,
): void {
  if (state.busy) return;
  const row =
    kind === "project" ? state.project_rows[index] : kind === "chat_session" ? state.chat_session_rows[index] : state.session_rows[index];
  if (!row) return;
  context.uiState.pendingLocalConfirmation = {
    kind,
    index,
    title: row.label,
    detail: kind === "project" ? (row as ProjectRow).path : (row as SessionRow).session_id,
  };
  context.render(state);
}

function requestLocalRollback(index: number, state: DesktopWebState, context: ActionContext): void {
  if (state.busy) return;
  const row = state.session_rows[index];
  if (!row || row.loaded_status === "active") return;
  context.uiState.pendingLocalConfirmation = {
    kind: "rollback_session",
    index,
    title: row.label,
    detail: row.session_id,
  };
  context.render(state);
}

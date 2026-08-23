import { command } from "./api.ts";
import { snapshotAgentInterruptTarget } from "./agent_interrupt_contract.ts";
import { snapshotPromptReviewMutationTarget } from "./composer_target_contract.ts";
import {
  beginConfigMutation,
  configMutationPending,
  finishConfigMutation,
  type ConfigValueInput,
} from "./config_mutation.ts";
import {
  beginLocalDecision,
  failLocalDecision,
  finishLocalDecision,
  type PermissionReviewDecision,
} from "./decision_state.ts";
import { turnPageLoadPending } from "./history_navigation.ts";
import {
  navigationIsIdle,
  sessionActionIndex,
  sessionRowActionAvailable,
} from "./navigation_state.ts";
import { rowMutationArgs } from "./row_target.ts";
import {
  beginQuickChatDeleteFocusContinuation,
} from "./quick_chat_delete_focus_continuation.ts";
import type { DesktopRenderModel } from "./render_projection.ts";
import { runCanBeCancelled } from "./run_control.ts";
import type {
  ConfigMutationTarget,
  DesktopViewState,
  DesktopWebState,
  ProjectRow,
  RowMutationTarget,
  SessionRow,
  SideChatCatalogResult,
} from "./types.ts";
import {
  beginSideChatCatalogLoad,
  failSideChatCatalogLoad,
  finishSideChatCatalogLoad,
  openAgentPane,
  openSideChatPane,
  rebaseSideChatDraftAfterConfigure,
  setArtifactPaneCollapsed,
  showAgentList,
  showOutputPane,
  sideChatConfigurationOpen,
  sideChatDeleteConfirmationStillTargets,
  sideChatDraftForState,
  sideChatMutationPending,
  sideChatOperationsOpen,
  sideChatOwnerSessionId,
  type UiLocalState,
} from "./ui_state.ts";
import {
  composerCapabilities,
  beginProviderCatalogRequest,
  draftMutationTarget,
  providerCapabilities,
  providerDraftPayload,
} from "./view_state.ts";
import { validateSideChatProviderSettings } from "./utils.ts";

export type ActionMenu = "file" | "edit" | "view" | "help";

export interface ActionPayload {
  index: number;
  value: string;
  activationSource?: "shortcut";
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
  getViewState: () => DesktopViewState | null;
  getRenderModel: () => DesktopRenderModel | null;
  acceptProjection: (state: DesktopWebState, render?: boolean) => void;
  rerender: () => void;
  mutate: (
    name: string,
    args?: Record<string, unknown>,
    activationSource?: ActionPayload["activationSource"],
  ) => Promise<void>;
  insertCommandFromPalette: (state: DesktopViewState, index: number) => Promise<void>;
  invalidateCommandPaletteInsertion: () => void;
  recoverCommandConflict: (error: unknown) => boolean;
  reportError: (error: unknown) => void;
  prepareConfigMutation: (target: ConfigMutationTarget) => ConfigValueInput[] | null;
  prepareConfigSnapshot: (target: ConfigMutationTarget) => ConfigValueInput[] | null;
  submitPermissionDecision: (decision: PermissionReviewDecision) => Promise<void>;
  submitRunStop: (state: DesktopViewState) => Promise<void>;
  setWindowMaximized: (maximized: boolean) => void;
  loadAgentExecution: (state: DesktopWebState, agentPath: string) => Promise<void>;
  loadPreviousAgentExecutionPage: (state: DesktopWebState, agentPath: string) => Promise<void>;
  loadSideChatModels: (args: {
    ownerSessionId: string;
    baseUrl: string;
    expectedConfigGeneration: string;
  }) => Promise<SideChatCatalogResult>;
  jumpToHistoryAnchor: (anchorId: string) => void;
}

export interface ActionDefinition {
  id: string;
  label: string;
  shortcut?: string;
  menu?: ActionMenu;
  palette?: boolean;
  enabled: (model: DesktopRenderModel, payload: ActionPayload) => boolean;
  run: (state: DesktopViewState, context: ActionContext, payload: ActionPayload) => void | Promise<void>;
}

interface ActionSourceDefinition extends Omit<ActionDefinition, "enabled"> {
  enabled?: (
    state: DesktopViewState,
    payload: ActionPayload,
    model: DesktopRenderModel,
  ) => boolean;
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

function targetSessionInterruptable(state: DesktopWebState, payload: ActionPayload): boolean {
  return targetSessionActive(state, payload)
    && Boolean(targetSessionRow(state, payload)?.interrupt_target);
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
  if (!current.config_draft.commit_enabled) return;
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
    const finished = finishConfigMutation(
      context.uiState,
      request,
      false,
      request.target,
      context.getViewState()?.config_target ?? null,
    );
    if (context.recoverCommandConflict(error)) return;
    if (!finished) return;
    context.rerender();
    context.reportError(error);
    return;
  }
  if (!finishConfigMutation(
    context.uiState,
    request,
    succeeded,
    state.config_target,
    context.getViewState()?.config_target ?? null,
  )) return;
  context.acceptProjection(state);
}

async function resetConfigDraft(context: ActionContext): Promise<void> {
  if (configMutationPending(context.uiState)) return;
  const current = context.getViewState();
  if (!current?.config_draft.discard_enabled) return;
  const values = context.prepareConfigSnapshot(current.config_target);
  if (!values) return;
  const request = beginConfigMutation(context.uiState, current.config_target);
  context.rerender();
  let state: DesktopWebState;
  try {
    state = await command<DesktopWebState>("reset_config_draft", {
      values,
      expectedTarget: request.target,
    });
  } catch (error) {
    const currentTarget = context.getViewState()?.config_target ?? null;
    const finished = finishConfigMutation(
      context.uiState,
      request,
      false,
      request.target,
      currentTarget,
    );
    if (context.recoverCommandConflict(error)) return;
    if (!finished) return;
    context.uiState.settingsActionFocusContinuation = {
      target: currentTarget ?? request.target,
      primaryAction: "discard-config-draft",
      fallbackAction: "close-overlay",
    };
    context.rerender();
    context.reportError(error);
    return;
  }
  if (!finishConfigMutation(
    context.uiState,
    request,
    true,
    state.config_target,
    context.getViewState()?.config_target ?? null,
  )) return;
  context.uiState.settingsActionFocusContinuation = {
    target: { ...state.config_target },
    primaryAction: "discard-config-draft",
    fallbackAction: "close-overlay",
  };
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
  if (!args) return;
  if (name === "interrupt_session") {
    const expectedStopTarget = state.session_rows[index]?.interrupt_target ?? null;
    if (!expectedStopTarget) return;
    await context.mutate(name, { ...args, expectedStopTarget });
    return;
  }
  await context.mutate(name, args);
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

function sideChatCommandTarget(state: DesktopWebState): {
  ownerSessionId: string;
  chatId: string;
  expectedGeneration: string;
} | null {
  const ownerSessionId = sideChatOwnerSessionId(state);
  const chatId = state.side_chat.chat_id;
  if (!ownerSessionId || !chatId) return null;
  return { ownerSessionId, chatId, expectedGeneration: state.side_chat.generation };
}

async function loadSideChatModels(state: DesktopWebState, context: ActionContext): Promise<void> {
  const request = beginSideChatCatalogLoad(context.uiState, state);
  if (!request) return;
  context.rerender();
  try {
    const result = await context.loadSideChatModels({
      ownerSessionId: request.ownerSessionId,
      baseUrl: request.baseUrl,
      expectedConfigGeneration: request.configGeneration,
    });
    const settlement = finishSideChatCatalogLoad(
      context.uiState,
      context.getProjection(),
      request,
      result,
    );
    if (settlement.localStateChanged) {
      context.rerender();
    }
  } catch (error) {
    context.recoverCommandConflict(error);
    const settlement = failSideChatCatalogLoad(
      context.uiState,
      context.getProjection(),
      request,
      sideChatCatalogErrorMessage(error),
    );
    if (settlement.localStateChanged) {
      context.rerender();
    }
  }
}

function sideChatCatalogErrorMessage(error: unknown): string {
  if (typeof error === "string") return error;
  if (error && typeof error === "object" && "message" in error) {
    const message = (error as { message?: unknown }).message;
    if (typeof message === "string") return message;
  }
  return "モデル一覧を読み込めませんでした。";
}

async function configureSideChat(state: DesktopWebState, context: ActionContext): Promise<void> {
  const ownerSessionId = sideChatOwnerSessionId(state);
  const draft = sideChatDraftForState(context.uiState, state);
  if (
    !ownerSessionId
    || !draft
    || !sideChatOperationsOpen(context.uiState)
    || !sideChatConfigurationOpen(state)
    || sideChatMutationPending(context.uiState, ownerSessionId)
  ) return;
  const baseUrl = draft.setupBaseUrl.trim();
  const model = draft.setupModel.trim();
  if (!validateSideChatProviderSettings(baseUrl, model).ok) return;
  if (
    state.side_chat.configured
    && baseUrl === state.side_chat.base_url.trim()
    && model === state.side_chat.model.trim()
  ) return;
  context.uiState.sideChatMutations.set(ownerSessionId, {
    kind: "configure",
    chatId: state.side_chat.chat_id,
    generation: state.side_chat.generation,
  });
  const expectedConfigGeneration = state.config_target.configGeneration;
  const setupRevision = draft.setupRevision;
  context.rerender();
  try {
    await context.mutate("configure_side_chat", {
      ownerSessionId,
      baseUrl,
      model,
      expectedConfigGeneration,
    });
    const current = context.getProjection();
    if (current && draft.setupRevision === setupRevision) {
      rebaseSideChatDraftAfterConfigure(
        context.uiState,
        current,
        ownerSessionId,
        baseUrl,
        model,
      );
    }
  } finally {
    context.uiState.sideChatMutations.delete(ownerSessionId);
    context.rerender();
  }
}

async function submitSideChat(state: DesktopWebState, context: ActionContext): Promise<void> {
  const initialTarget = sideChatCommandTarget(state);
  const initialDraft = sideChatDraftForState(context.uiState, state);
  if (
    !initialTarget
    || !initialDraft
    || !sideChatOperationsOpen(context.uiState)
    || state.side_chat.deleting
    || !state.side_chat.can_send
    || context.uiState.sideChatDeleteConfirmation !== null
    || sideChatMutationPending(context.uiState, initialTarget.ownerSessionId)
  ) return;
  if (!initialDraft.text.trim()) return;
  if (initialDraft.saveTimer !== null) {
    window.clearTimeout(initialDraft.saveTimer);
    initialDraft.saveTimer = null;
  }
  context.uiState.sideChatMutations.set(initialTarget.ownerSessionId, {
    kind: "send",
    chatId: initialTarget.chatId,
    generation: initialTarget.expectedGeneration,
  });
  context.rerender();

  if (initialDraft.savePromise) await initialDraft.savePromise;

  const ready = context.getProjection();
  const target = ready ? sideChatCommandTarget(ready) : null;
  const draft = ready ? sideChatDraftForState(context.uiState, ready) : null;
  if (
    !ready
    || !target
    || !draft
    || target.ownerSessionId !== initialTarget.ownerSessionId
    || target.chatId !== initialTarget.chatId
    || target.expectedGeneration !== initialTarget.expectedGeneration
    || !sideChatOperationsOpen(context.uiState)
    || ready.side_chat.deleting
    || !ready.side_chat.can_send
  ) {
    context.uiState.sideChatMutations.delete(initialTarget.ownerSessionId);
    context.rerender();
    return;
  }
  const text = draft.text.trim();
  if (!text) {
    context.uiState.sideChatMutations.delete(initialTarget.ownerSessionId);
    context.rerender();
    return;
  }
  const revision = draft.revision;
  const expectedDraftRevision = draft.persistedRevision;
  await context.mutate("submit_side_chat", {
    ...target,
    expectedDraftRevision,
    text,
  });
  context.uiState.sideChatMutations.delete(target.ownerSessionId);
  const current = context.getProjection();
  const currentDraft = current ? sideChatDraftForState(context.uiState, current) : null;
  const accepted = current?.side_chat.owner_session_id === target.ownerSessionId
    && current.side_chat.chat_id === target.chatId
    && current.side_chat.generation !== target.expectedGeneration;
  if (accepted && currentDraft && current) {
    currentDraft.persistedText = current.side_chat.draft_text;
    currentDraft.persistedRevision = current.side_chat.draft_revision;
    if (currentDraft.revision === revision) currentDraft.text = "";
  } else if (
    currentDraft
    && current
    && current.side_chat.owner_session_id === target.ownerSessionId
    && current.side_chat.chat_id === target.chatId
    && current.side_chat.draft_revision !== expectedDraftRevision
  ) {
    currentDraft.persistedText = current.side_chat.draft_text;
    currentDraft.persistedRevision = current.side_chat.draft_revision;
  }
  context.rerender();
}

export async function persistSideChatDraft(
  state: DesktopWebState,
  context: ActionContext,
): Promise<void> {
  const ownerSessionId = sideChatOwnerSessionId(state);
  const chatId = state.side_chat.chat_id;
  const draft = sideChatDraftForState(context.uiState, state);
  if (
    !ownerSessionId
    || !chatId
    || !draft
    || !sideChatOperationsOpen(context.uiState)
    || draft.chatId !== chatId
    || state.side_chat.deleting
  ) return;
  draft.saveTimer = null;
  if (draft.saveInFlight) {
    draft.saveQueued = true;
    if (draft.savePromise) await draft.savePromise;
    return;
  }
  if (draft.text === draft.persistedText) return;

  draft.saveInFlight = true;
  draft.saveQueued = false;
  const savePromise = persistSideChatDraftLoop(ownerSessionId, chatId, draft, context);
  draft.savePromise = savePromise;
  try {
    await savePromise;
  } finally {
    if (draft.savePromise === savePromise) draft.savePromise = null;
    draft.saveInFlight = false;
    draft.saveQueued = false;
  }
}

async function persistSideChatDraftLoop(
  ownerSessionId: string,
  chatId: string,
  draft: NonNullable<ReturnType<typeof sideChatDraftForState>>,
  context: ActionContext,
): Promise<void> {
  while (draft.text !== draft.persistedText) {
    const currentBeforeSave = context.getProjection();
    if (
      !currentBeforeSave
      || !sideChatOperationsOpen(context.uiState)
      || currentBeforeSave.side_chat.deleting
      || sideChatOwnerSessionId(currentBeforeSave) !== ownerSessionId
      || currentBeforeSave.side_chat.chat_id !== chatId
    ) return;

    const text = draft.text;
    const localRevision = draft.revision;
    const expectedDraftRevision = draft.persistedRevision;
    draft.saveQueued = false;
    await context.mutate("save_side_chat_draft", {
      ownerSessionId,
      chatId,
      expectedDraftRevision,
      text,
    });

    const current = context.getProjection();
    const sameTarget = current?.side_chat.owner_session_id === ownerSessionId
      && current.side_chat.chat_id === chatId;
    // `mutate` deliberately accepts a typed conflict projection without throwing it back to this
    // local owner. A changed server revision alone can therefore mean that another process won the
    // CAS. Only acknowledge this write when the durable text is exactly the value we submitted;
    // otherwise retain the user's local edit for an explicit retry.
    const settled = sameTarget && current !== null && current.side_chat.draft_text === text;
    if (settled && current) {
      draft.persistedText = current.side_chat.draft_text;
      draft.persistedRevision = current.side_chat.draft_revision;
      if (draft.revision === localRevision) draft.text = current.side_chat.draft_text;
    } else if (
      sameTarget
      && current
      && current.side_chat.draft_revision !== expectedDraftRevision
    ) {
      // Preserve the losing local edit, but rebase its durable CAS owner. We intentionally do not
      // auto-retry here: the next user edit/blur is the explicit decision to save over the value
      // observed from another process.
      draft.persistedText = current.side_chat.draft_text;
      draft.persistedRevision = current.side_chat.draft_revision;
    }
    const followUpRequired = settled
      && (draft.saveQueued || draft.revision !== localRevision)
      && draft.text !== draft.persistedText;
    if (!followUpRequired) return;
  }
}

async function cancelSideChat(state: DesktopWebState, context: ActionContext): Promise<void> {
  const target = sideChatCommandTarget(state);
  if (
    !target
    || !sideChatOperationsOpen(context.uiState)
    || state.side_chat.deleting
    || !state.side_chat.can_cancel
    || context.uiState.sideChatDeleteConfirmation !== null
    || sideChatMutationPending(context.uiState, target.ownerSessionId)
  ) return;
  context.uiState.sideChatMutations.set(target.ownerSessionId, {
    kind: "cancel",
    chatId: target.chatId,
    generation: target.expectedGeneration,
  });
  context.rerender();
  await context.mutate("cancel_side_chat", target);
  context.uiState.sideChatMutations.delete(target.ownerSessionId);
  context.rerender();
}

function requestDeleteSideChat(state: DesktopWebState, context: ActionContext): void {
  const target = sideChatCommandTarget(state);
  if (
    !target
    || !sideChatOperationsOpen(context.uiState)
    || state.side_chat.deleting
    || sideChatMutationPending(context.uiState, target.ownerSessionId)
  ) return;
  context.uiState.sideChatDeleteConfirmation = target;
  context.rerender();
}

function cancelDeleteSideChat(state: DesktopWebState, context: ActionContext): void {
  const confirmation = context.uiState.sideChatDeleteConfirmation;
  if (
    !sideChatDeleteConfirmationStillTargets(confirmation, state)
    || !sideChatOperationsOpen(context.uiState)
    || sideChatMutationPending(context.uiState, confirmation?.ownerSessionId ?? null)
  ) return;
  context.uiState.sideChatDeleteConfirmation = null;
  context.rerender();
}

async function confirmDeleteSideChat(state: DesktopWebState, context: ActionContext): Promise<void> {
  const confirmation = context.uiState.sideChatDeleteConfirmation;
  const currentTarget = sideChatCommandTarget(state);
  if (
    !confirmation
    || !currentTarget
    || !sideChatOperationsOpen(context.uiState)
    || state.side_chat.deleting
    || confirmation.ownerSessionId !== currentTarget.ownerSessionId
    || confirmation.chatId !== currentTarget.chatId
    || confirmation.expectedGeneration !== currentTarget.expectedGeneration
    || sideChatMutationPending(context.uiState, confirmation.ownerSessionId)
  ) return;
  context.uiState.sideChatMutations.set(confirmation.ownerSessionId, {
    kind: "delete",
    chatId: confirmation.chatId,
    generation: confirmation.expectedGeneration,
  });
  context.rerender();
  try {
    await context.mutate("delete_side_chat", { ...confirmation });
  } finally {
    context.uiState.sideChatMutations.delete(confirmation.ownerSessionId);
    context.rerender();
  }
  const current = context.getProjection();
  if (
    !current
    || current.side_chat.deleting
    || current.side_chat.owner_session_id !== confirmation.ownerSessionId
    || current.side_chat.chat_id !== confirmation.chatId
  ) {
    context.uiState.sideChatDeleteConfirmation = null;
    if (
      !current
      || current.side_chat.owner_session_id !== confirmation.ownerSessionId
      || current.side_chat.chat_id !== confirmation.chatId
    ) {
      context.uiState.sideChatDrafts.delete(confirmation.ownerSessionId);
    }
  }
  context.rerender();
}

const ACTION_DEFINITIONS = [
  {
    id: "send",
    label: "送信",
    shortcut: "Ctrl+Enter",
    palette: true,
    enabled: canSubmit,
    run: (state, context) => context.mutate("submit_prompt", {
      text: state.draft_prompt,
      expectedTarget: draftMutationTarget(state),
      expectedRunTarget: state.run_target,
    }),
  },
  {
    id: "cancel-run",
    label: "実行停止",
    palette: true,
    enabled: (state, _payload, model) => runCanBeCancelled(state)
      && model.local.modal.permissionDecision?.phase !== "submitting",
    run: (state, context) => context.submitRunStop(state),
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
    run: (_state, context, payload) => context.mutate(
      "new_chat",
      undefined,
      payload.activationSource,
    ),
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
    id: "show-about",
    label: "moyAIについて",
    menu: "help",
    palette: true,
    enabled: always,
    run: (_state, context) => context.mutate("show_about"),
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
      expectedRunTarget: state.run_target,
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
      expectedRunTarget: state.run_target,
    }),
  },
  {
    id: "toggle-access",
    label: "アクセスモード切替",
    shortcut: "F8",
    palette: true,
    enabled: (state, _payload, model) => state.config_draft.access_mode_mutation_enabled
      && !model.local.configMutationPending,
    run: (state, context) => context.mutate("toggle_access_mode", {
      expectedTarget: state.access_target,
      draftValues: context.prepareConfigSnapshot(state.config_target) ?? [],
    }),
  },
  {
    id: "discard-config-draft",
    label: "設定の変更を破棄",
    enabled: (state, _payload, model) => state.config_draft.discard_enabled
      && !model.local.configMutationPending,
    run: (_state, context) => resetConfigDraft(context),
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
    enabled: targetSessionInterruptable,
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
    label: "以前の履歴を読み込む",
    palette: true,
    enabled: (state) => state.turn_page_admission_open && !turnPageLoadPending(state) && state.turn_page_offset > 0,
    run: (state, context) => runTurnPageMutation("load_previous_turn_page", state, context),
  },
  {
    id: "load-next-turn-page",
    label: "新しい履歴へ移動",
    enabled: (state) => state.turn_page_admission_open && !turnPageLoadPending(state) && state.turn_page_has_more,
    run: (state, context) => runTurnPageMutation("load_next_turn_page", state, context),
  },
  {
    id: "toggle-artifact-pane",
    label: "アーティファクトペイン切替",
    palette: true,
    enabled: always,
    run: (_state, context) => {
      const collapsing = !context.uiState.artifactPaneCollapsed;
      if (collapsing && context.uiState.artifactPaneMode !== "output") {
        showOutputPane(context.uiState);
      }
      setArtifactPaneCollapsed(context.uiState, collapsing);
      context.uiState.artifactPaneFocusAfterRender = collapsing ? "trigger" : "content";
      context.rerender();
    },
  },
  {
    id: "show-side-chat-pane",
    label: "サイドチャットを表示",
    palette: true,
    enabled: (state) => sideChatOwnerSessionId(state) !== null,
    run: (state, context) => {
      if (!openSideChatPane(context.uiState, state)) return;
      sideChatDraftForState(context.uiState, state);
      context.rerender();
    },
  },
  {
    id: "load-side-chat-models",
    label: "Side Chat モデル読込",
    enabled: (state, _payload, model) => sideChatConfigurationOpen(state)
      && model.local.sideChat.catalogLoadEnabled,
    run: (state, context) => loadSideChatModels(state, context),
  },
  {
    id: "configure-side-chat",
    label: "サイドチャットモデルを設定",
    enabled: (state, _payload, model) => {
      const validation = validateSideChatProviderSettings(
        model.local.sideChat.setupBaseUrl,
        model.local.sideChat.setupModel,
      );
      return sideChatConfigurationOpen(state)
        && model.local.sideChat.operationsOpen
        && !model.local.sideChat.mutationPending
        && validation.ok
        && (!state.side_chat.configured
          || model.local.sideChat.setupBaseUrl.trim() !== state.side_chat.base_url.trim()
          || model.local.sideChat.setupModel.trim() !== state.side_chat.model.trim());
    },
    run: (state, context) => configureSideChat(state, context),
  },
  {
    id: "send-side-chat",
    label: "サイドチャットへ送信",
    enabled: (state, _payload, model) => !state.side_chat.deleting
      && state.side_chat.can_send
      && sideChatCommandTarget(state) !== null
      && model.local.sideChat.operationsOpen
      && !model.local.sideChat.mutationPending
      && model.local.sideChat.deleteConfirmation === null
      && model.local.sideChat.draft.trim().length > 0,
    run: (state, context) => submitSideChat(state, context),
  },
  {
    id: "cancel-side-chat",
    label: "サイドチャットを停止",
    enabled: (state, _payload, model) => !state.side_chat.deleting
      && state.side_chat.can_cancel
      && sideChatCommandTarget(state) !== null
      && model.local.sideChat.operationsOpen
      && !model.local.sideChat.mutationPending
      && model.local.sideChat.deleteConfirmation === null,
    run: (state, context) => cancelSideChat(state, context),
  },
  {
    id: "request-delete-side-chat",
    label: "サイドチャットを削除",
    enabled: (state, _payload, model) => !state.side_chat.deleting
      && sideChatCommandTarget(state) !== null
      && model.local.sideChat.operationsOpen
      && !model.local.sideChat.mutationPending,
    run: (state, context) => requestDeleteSideChat(state, context),
  },
  {
    id: "cancel-delete-side-chat",
    label: "サイドチャットの削除をキャンセル",
    enabled: (state, _payload, model) => !state.side_chat.deleting
      && sideChatCommandTarget(state) !== null
      && model.local.sideChat.operationsOpen
      && !model.local.sideChat.mutationPending
      && sideChatDeleteConfirmationStillTargets(model.local.sideChat.deleteConfirmation, state),
    run: (state, context) => cancelDeleteSideChat(state, context),
  },
  {
    id: "confirm-delete-side-chat",
    label: "サイドチャットを削除する",
    enabled: (state, _payload, model) => !state.side_chat.deleting
      && sideChatCommandTarget(state) !== null
      && model.local.sideChat.operationsOpen
      && !model.local.sideChat.mutationPending
      && sideChatDeleteConfirmationStillTargets(model.local.sideChat.deleteConfirmation, state),
    run: (state, context) => confirmDeleteSideChat(state, context),
  },
  {
    id: "show-agent-pane",
    label: "Sub Agent履歴を表示",
    enabled: (state) => state.agent_activity_rows.length > 0,
    run: async (state, context, payload) => {
      if (!openAgentPane(context.uiState, state, payload.value)) return;
      context.rerender();
      const selectedAgentPath = context.uiState.selectedAgentPath;
      if (selectedAgentPath) await context.loadAgentExecution(state, selectedAgentPath);
    },
  },
  {
    id: "show-agent-list",
    label: "Sub Agent一覧に戻る",
    enabled: always,
    run: (_state, context) => {
      showAgentList(context.uiState);
      context.rerender();
    },
  },
  {
    id: "interrupt-agent",
    label: "Sub Agentを停止",
    enabled: (state, payload) => state.agent_activity_rows.some(
      (row) => row.agent_path === payload.value
        && snapshotAgentInterruptTarget(row.interrupt_target) !== null,
    ),
    run: async (state, context, payload) => {
      const row = state.agent_activity_rows.find((candidate) => candidate.agent_path === payload.value);
      const expectedTarget = snapshotAgentInterruptTarget(row?.interrupt_target);
      if (!expectedTarget) return;
      await context.mutate("interrupt_agent", {
        expectedTarget,
      });
    },
  },
  {
    id: "show-output-pane",
    label: "出力ペインに戻る",
    enabled: always,
    run: (_state, context) => {
      showOutputPane(context.uiState, true);
      context.rerender();
    },
  },
  {
    id: "jump-history-anchor",
    label: "会話履歴へ移動",
    enabled: always,
    run: (_state, context, payload) => context.jumpToHistoryAnchor(payload.value),
  },
  {
    id: "load-previous-agent-execution-page",
    label: "以前の実行履歴",
    enabled: always,
    run: async (state, context, payload) => {
      await context.loadPreviousAgentExecutionPage(state, payload.value);
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
    enabled: (state, _payload, model) => providerCapabilities(state).canLoadProviderModels
      && !model.local.configMutationPending,
    run: (state, context) => {
      const request = beginProviderCatalogRequest(context.uiState, state);
      if (!request) return;
      context.rerender();
      return context.mutate(
        "load_provider_models",
        providerDraftPayload(
          context.uiState.drafts.provider,
          state.config_target,
        ),
      );
    },
  },
  {
    id: "apply-provider-session",
    label: "Provider 設定を UI セッションに適用",
    palette: true,
    enabled: (state, _payload, model) => state.provider_apply_enabled
      && !model.local.configMutationPending,
    run: (state, context) => {
      const draftValues = context.prepareConfigSnapshot(state.config_target);
      if (!draftValues) return;
      return context.mutate(
        "apply_provider_session",
        providerDraftPayload(context.uiState.drafts.provider, state.config_target, draftValues),
      );
    },
  },
  {
    id: "save-provider-global",
    label: "Provider 設定をファイルに保存",
    palette: true,
    enabled: (state, _payload, model) => state.provider_apply_enabled
      && !model.local.configMutationPending,
    run: (state, context) => {
      const draftValues = context.prepareConfigSnapshot(state.config_target);
      if (!draftValues) return;
      return context.mutate(
        "save_provider_global",
        providerDraftPayload(context.uiState.drafts.provider, state.config_target, draftValues),
      );
    },
  },
  {
    id: "apply-session-config",
    label: "編集中の設定を UI セッションに適用",
    palette: true,
    enabled: (state, _payload, model) => state.config_draft.commit_enabled
      && !model.local.configMutationPending,
    run: (_state, context) => runConfigMutation("apply_session_config", context),
  },
  {
    id: "save-global-config",
    label: "編集中の設定を設定ファイルに保存",
    palette: true,
    enabled: (state, _payload, model) => state.config_draft.commit_enabled
      && !model.local.configMutationPending,
    run: (_state, context) => runConfigMutation("save_global_config", context),
  },
  {
    id: "set-provider-mode",
    label: "Provider mode 切替",
    enabled: (_state, payload) => payload.value === "lm_studio_native_required"
      || payload.value === "openai_compatible_only",
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
    enabled: (state, payload) => state.provider_model_ids[payload.index] !== undefined,
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
  { id: "open-user-data-folder", label: "データフォルダーを開く", palette: true, enabled: always, run: (_state, context) => runWithoutRender("open_user_data_folder", context) },
  { id: "set-image", label: "画像を添付", palette: true, enabled: (state) => state.image_input_enabled, run: (state, context) => context.mutate("attach_image", { text: state.image_input, expectedTarget: draftMutationTarget(state) }) },
  { id: "browse-image", label: "画像を参照", palette: true, enabled: (state) => state.image_input_enabled, run: (state, context) => context.mutate("browse_image", { expectedTarget: draftMutationTarget(state) }) },
  { id: "clear-images", label: "添付を解除", palette: true, enabled: (state) => state.attached_images.length > 0, run: (state, context) => context.mutate("clear_images", { expectedTarget: draftMutationTarget(state) }) },
  { id: "approve-permission", label: "確認した操作を実行", enabled: (state, _payload, model) => state.confirmation_visible && model.local.modal.permissionDecision?.phase !== "submitting", run: (_state, context) => context.submitPermissionDecision("approved") },
  { id: "abort-permission", label: "操作を実行せず指示を変更", enabled: (state, _payload, model) => state.confirmation_visible && model.local.modal.permissionDecision?.phase !== "submitting", run: (_state, context) => context.submitPermissionDecision("abort") },
  {
    id: "toggle-attachment-tray",
    label: "添付トレイ切替",
    enabled: (state) => state.image_input_enabled,
    run: (_state, context) => {
      context.uiState.attachmentTrayOpen = !context.uiState.attachmentTrayOpen;
      context.rerender();
    },
  },
  {
    id: "dismiss-ui-error",
    label: "エラーを閉じる",
    enabled: (_state, _payload, model) => model.local.recoverableError !== null,
    run: (_state, context) => {
      context.uiState.recoverableError = null;
      context.rerender();
    },
  },
  {
    id: "new-project-session",
    label: "プロジェクトに新しいセッションを作成",
    enabled: (state, payload) => navigationIsIdle(state)
      && indexedRowMutationAvailable(state, payload.index, state.project_rows[payload.index]?.project_id),
    run: (state, context, payload) => runIndexedMutation(
      "new_project_session",
      payload.index,
      state.project_rows[payload.index]?.project_id,
      state,
      context,
    ),
  },
  {
    id: "project",
    label: "プロジェクトを選択",
    enabled: (state, payload) => navigationIsIdle(state)
      && indexedRowMutationAvailable(state, payload.index, state.project_rows[payload.index]?.project_id),
    run: (state, context, payload) => runIndexedMutation(
      "select_project",
      payload.index,
      state.project_rows[payload.index]?.project_id,
      state,
      context,
    ),
  },
  {
    id: "session",
    label: "セッションを選択",
    enabled: (state, payload) => navigationIsIdle(state)
      && indexedRowMutationAvailable(state, payload.index, state.session_rows[payload.index]?.session_id),
    run: (state, context, payload) => runIndexedMutation(
      "select_session",
      payload.index,
      state.session_rows[payload.index]?.session_id,
      state,
      context,
    ),
  },
  {
    id: "chat-session",
    label: "チャットを選択",
    enabled: (state, payload) => navigationIsIdle(state)
      && indexedRowMutationAvailable(state, payload.index, state.chat_session_rows[payload.index]?.session_id),
    run: (state, context, payload) => runIndexedMutation(
      "select_chat_session",
      payload.index,
      state.chat_session_rows[payload.index]?.session_id,
      state,
      context,
    ),
  },
  {
    id: "cancel-local-confirm",
    label: "確認をキャンセル",
    enabled: (_state, _payload, model) => model.local.modal.localConfirmation !== null
      && !model.local.modal.localDecisionPending,
    run: (_state, context) => {
      if (context.uiState.localConfirmationDecisionPending) return;
      context.uiState.pendingLocalConfirmation = null;
      finishLocalDecision(context.uiState);
      context.rerender();
    },
  },
  {
    id: "confirm-local-delete",
    label: "削除を確認",
    enabled: (_state, _payload, model) => Boolean(
      model.local.modal.localConfirmation
      && !model.local.modal.localDecisionPending
      && (model.local.modal.localConfirmation.kind === "project"
        || model.local.modal.localConfirmation.kind === "session"
        || model.local.modal.localConfirmation.kind === "chat_session"),
    ),
    run: (_state, context) => confirmLocalDelete(context),
  },
  {
    id: "confirm-local-archive-state",
    label: "アーカイブ状態の変更を確認",
    enabled: (_state, _payload, model) => Boolean(
      model.local.modal.localConfirmation
      && !model.local.modal.localDecisionPending
      && (model.local.modal.localConfirmation.kind === "archive_session"
        || model.local.modal.localConfirmation.kind === "unarchive_session"),
    ),
    run: (_state, context) => confirmLocalArchiveState(context),
  },
  {
    id: "confirm-local-rollback",
    label: "ロールバックを確認",
    enabled: (_state, _payload, model) => model.local.modal.localConfirmation?.kind === "rollback_session"
      && !model.local.modal.localDecisionPending,
    run: (_state, context) => confirmLocalRollback(context),
  },
  {
    id: "artifact",
    label: "アーティファクトを選択",
    enabled: (state, payload) => indexedRowMutationAvailable(
      state,
      payload.index,
      state.artifact_rows[payload.index]?.path,
    ),
    run: (state, context, payload) => runIndexedMutation(
      "select_artifact",
      payload.index,
      state.artifact_rows[payload.index]?.path,
      state,
      context,
    ),
  },
  {
    id: "remove-image",
    label: "添付画像を削除",
    enabled: (state, payload) => indexedRowMutationAvailable(
      state,
      payload.index,
      state.attached_images[payload.index],
    ),
    run: (state, context, payload) => runIndexedMutation(
      "remove_image",
      payload.index,
      state.attached_images[payload.index],
      state,
      context,
    ),
  },
  {
    id: "send-review-enhanced",
    label: "改善した依頼文を送信",
    enabled: (state) => state.send_enhanced_enabled
      && snapshotPromptReviewMutationTarget(state.review_target) !== null,
    run: async (state, context) => {
      const expectedTarget = snapshotPromptReviewMutationTarget(state.review_target);
      if (!state.send_enhanced_enabled || !expectedTarget) return;
      await context.mutate("send_prompt_review", {
        enhanced: true,
        text: state.review_draft_text,
        expectedTarget,
        expectedRunTarget: state.run_target,
      });
    },
  },
  {
    id: "send-review-raw",
    label: "元の依頼文を送信",
    enabled: (state) => state.send_raw_enabled
      && snapshotPromptReviewMutationTarget(state.review_target) !== null,
    run: async (state, context) => {
      const expectedTarget = snapshotPromptReviewMutationTarget(state.review_target);
      if (!state.send_raw_enabled || !expectedTarget) return;
      await context.mutate("send_prompt_review", {
        enhanced: false,
        text: state.review_draft_text,
        expectedTarget,
        expectedRunTarget: state.run_target,
      });
    },
  },
  {
    id: "cancel-review",
    label: "Prompt Reviewをキャンセル",
    enabled: (state) => snapshotPromptReviewMutationTarget(state.review_target) !== null,
    run: async (state, context) => {
      const expectedTarget = snapshotPromptReviewMutationTarget(state.review_target);
      if (!expectedTarget) return;
      await context.mutate("cancel_prompt_review", { expectedTarget });
    },
  },
  { id: "show-file-menu", label: "ファイルメニューを表示", enabled: always, run: (_state, context) => context.mutate("show_file_menu") },
  { id: "show-edit-menu", label: "編集メニューを表示", enabled: always, run: (_state, context) => context.mutate("show_edit_menu") },
  { id: "show-view-menu", label: "表示メニューを表示", enabled: always, run: (_state, context) => context.mutate("show_view_menu") },
  { id: "show-help-menu", label: "ヘルプメニューを表示", enabled: always, run: (_state, context) => context.mutate("show_help_menu") },
  {
    id: "close-overlay",
    label: "画面を閉じる",
    enabled: (state) => !startupSetupRequired(state)
      && (state.overlay !== "prompt_review"
        || snapshotPromptReviewMutationTarget(state.review_target) !== null),
    run: async (state, context) => {
      if (startupSetupRequired(state)) return;
      if (state.overlay === "prompt_review") {
        const expectedTarget = snapshotPromptReviewMutationTarget(state.review_target);
        if (!expectedTarget) return;
        await context.mutate("cancel_prompt_review", { expectedTarget });
        return;
      }
      await context.mutate("close_overlay");
    },
  },
  {
    id: "import-config-toml",
    label: "TOML設定をインポート",
    enabled: (state, _payload, model) => state.config_draft.external_owner_mutation_open
      && !model.local.configMutationPending,
    run: (state, context) => importConfigToml(state, context),
  },
  {
    id: "insert-command",
    label: "コマンドを挿入",
    enabled: (state, payload) => state.command_rows[payload.index] !== undefined,
    run: (state, context, payload) => context.insertCommandFromPalette(state, payload.index),
  },
  { id: "minimize-window", label: "最小化", enabled: always, run: () => command("minimize_window") },
  {
    id: "toggle-maximize-window",
    label: "最大化／元のサイズに戻す",
    enabled: always,
    run: async (_state, context) => context.setWindowMaximized(await command<boolean>("toggle_maximize_window")),
  },
  { id: "close-window", label: "閉じる", enabled: always, run: (_state, context) => command("hide_to_tray").catch(() => context.desktopWindow.hide()) },
] satisfies readonly ActionSourceDefinition[];

export type ActionId = typeof ACTION_DEFINITIONS[number]["id"];

/** The sole normalized GUI action registry. Every surface and dispatcher consumes this shape. */
export const ACTIONS: readonly ActionDefinition[] = Object.freeze(
  ACTION_DEFINITIONS.map((definition) => ({
    ...definition,
    enabled: (model: DesktopRenderModel, payload: ActionPayload) =>
      definition.enabled?.(model.view as DesktopViewState, payload, model) ?? true,
  })),
);

export const ACTION_IDS: readonly ActionId[] = Object.freeze(
  ACTION_DEFINITIONS.map((action) => action.id),
);

export const ACTION_BY_ID = new Map<ActionId, ActionDefinition>();
for (const action of ACTIONS) {
  if (ACTION_BY_ID.has(action.id)) {
    throw new Error(`Duplicate GUI action: ${action.id}`);
  }
  ACTION_BY_ID.set(action.id as ActionId, action);
}

export function actionById(id: string): ActionDefinition | undefined {
  return ACTION_BY_ID.get(id as ActionId);
}

const NO_ACTION_PAYLOAD: ActionPayload = { index: -1, value: "" };

export function actionEnabled(
  action: ActionDefinition,
  model: DesktopRenderModel,
  payload: ActionPayload = NO_ACTION_PAYLOAD,
): boolean {
  return action.enabled(model, payload);
}

export function actionEnabledById(
  id: string,
  model: DesktopRenderModel,
  payload: ActionPayload = NO_ACTION_PAYLOAD,
): boolean {
  const action = actionById(id);
  return action ? actionEnabled(action, model, payload) : false;
}

export function menuActions(menu: ActionMenu, model: DesktopRenderModel): ActionDefinition[] {
  return ACTIONS.filter((action) => action.menu === menu && actionEnabled(action, model));
}

export function shortcutActions(): ActionDefinition[] {
  return ACTIONS.filter((action) => action.shortcut);
}

export function paletteActions(
  model: DesktopRenderModel,
): ActionDefinition[] {
  const state = model.view;
  const query = state.local_search_text.trim().toLowerCase();
  return ACTIONS.filter((action) => action.palette)
    .filter((action) => {
      if (!query) return true;
      return action.label.toLowerCase().includes(query) || action.id.toLowerCase().includes(query);
    })
    .filter((action) => actionEnabled(action, model));
}

export async function dispatchAction(
  id: string,
  context: ActionContext,
  payload: ActionPayload,
): Promise<boolean> {
  const action = actionById(id);
  if (!action) return false;
  const model = context.getRenderModel();
  if (!model || !actionEnabled(action, model, payload)) return true;
  await action.run(model.view as DesktopViewState, context, payload);
  return true;
}

function indexedRowMutationAvailable(
  state: DesktopWebState,
  index: number,
  rowId: string | null | undefined,
): boolean {
  return rowMutationArgs(state, index, rowId) !== null;
}

async function runIndexedMutation(
  name: string,
  index: number,
  rowId: string | null | undefined,
  state: DesktopWebState,
  context: ActionContext,
): Promise<void> {
  const args = rowMutationArgs(state, index, rowId);
  if (args) await context.mutate(name, args);
}

function startupSetupRequired(state: DesktopWebState): boolean {
  return state.startup.initial_setup_required && state.startup.action_overlay === state.overlay;
}

export function beginConfigImportMutation(
  uiState: UiLocalState,
  target: ConfigMutationTarget,
  rerender: () => void,
) {
  uiState.externalConfigMutationPending = true;
  const request = beginConfigMutation(uiState, target);
  rerender();
  return request;
}

async function importConfigToml(state: DesktopViewState, context: ActionContext): Promise<void> {
  if (!state.config_draft.external_owner_mutation_open) return;
  try {
    const request = beginConfigImportMutation(
      context.uiState,
      state.config_target,
      context.rerender,
    );
    let nextState: DesktopWebState;
    let imported: boolean;
    try {
      [nextState, imported] = await command<[DesktopWebState, boolean]>("import_global_config_toml", {
        draftValues: context.prepareConfigSnapshot(state.config_target) ?? [],
        expectedTarget: request.target,
      });
    } catch (error) {
      const finished = finishConfigMutation(
        context.uiState,
        request,
        false,
        request.target,
        context.getViewState()?.config_target ?? null,
      );
      if (context.recoverCommandConflict(error)) return;
      if (!finished) return;
      context.rerender();
      context.reportError(error);
      return;
    }
    if (!finishConfigMutation(
      context.uiState,
      request,
      imported,
      nextState.config_target,
      context.getViewState()?.config_target ?? null,
    )) return;
    context.acceptProjection(nextState);
  } finally {
    context.uiState.externalConfigMutationPending = false;
    context.rerender();
  }
}

async function confirmLocalDelete(context: ActionContext): Promise<void> {
  const pending = context.uiState.pendingLocalConfirmation;
  if (!pending || context.uiState.localConfirmationDecisionPending) return;
  if (pending.kind === "project") {
    await runLocalConfirmationMutation(context, "delete_project", pending.index, pending.expectedTarget);
  } else if (pending.kind === "chat_session") {
    const projection = context.getProjection();
    const continuation = projection
      ? beginQuickChatDeleteFocusContinuation(projection, pending.index, pending.expectedTarget)
      : null;
    context.uiState.quickChatDeleteFocusContinuation = continuation;
    await runLocalConfirmationMutation(
      context,
      "delete_chat_session",
      pending.index,
      pending.expectedTarget,
      () => {
        context.uiState.quickChatDeleteFocusContinuation = null;
      },
    );
  } else {
    await runLocalConfirmationMutation(context, "delete_session", pending.index, pending.expectedTarget);
  }
}

async function confirmLocalArchiveState(context: ActionContext): Promise<void> {
  const pending = context.uiState.pendingLocalConfirmation;
  if (
    !pending
    || context.uiState.localConfirmationDecisionPending
    || (pending.kind !== "archive_session" && pending.kind !== "unarchive_session")
  ) return;
  await runLocalConfirmationMutation(
    context,
    pending.kind === "archive_session" ? "archive_session" : "unarchive_session",
    pending.index,
    pending.expectedTarget,
  );
}

async function confirmLocalRollback(context: ActionContext): Promise<void> {
  const pending = context.uiState.pendingLocalConfirmation;
  if (!pending || context.uiState.localConfirmationDecisionPending || pending.kind !== "rollback_session") return;
  await runLocalConfirmationMutation(context, "rollback_session", pending.index, pending.expectedTarget);
}

async function runLocalConfirmationMutation(
  context: ActionContext,
  name: string,
  index: number,
  expectedTarget: RowMutationTarget,
  onFailure: () => void = () => undefined,
): Promise<void> {
  if (!beginLocalDecision(context.uiState, context.uiState.pendingLocalConfirmation !== null)) {
    onFailure();
    return;
  }
  context.rerender();
  try {
    const nextState = await command<DesktopWebState>(name, { index, expectedTarget });
    finishLocalDecision(context.uiState);
    context.uiState.pendingLocalConfirmation = null;
    context.acceptProjection(nextState);
  } catch (error) {
    onFailure();
    failLocalDecision(context.uiState, "処理を開始できませんでした。もう一度お試しください。");
    if (context.recoverCommandConflict(error)) {
      context.rerender();
      return;
    }
    context.rerender();
    context.reportError(error);
  }
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

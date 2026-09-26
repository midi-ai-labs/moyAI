import { convertFileSrc } from "@tauri-apps/api/core";
import {
  actionById,
  actionEnabledById,
  menuActions,
  paletteActions,
  shortcutActions,
  type ActionDefinition,
  type ActionMenu,
  type ActionPayload,
} from "./actions.ts";
import { icon } from "./icons.ts";
import { aiConnectionManaged, renderManagedAiConnection, renderHubOverlay } from "./hub_render.ts";
import { renderDeviceConnectionReset } from "./device_network_render.ts";
import { renderSharedWork } from "./shared_work_render.ts";
import { renderConversationInput, renderConversationMain } from "./conversation_surface.ts";
import { sharedConversationRows } from "./shared_work_state.ts";
import { renderMcpHistoryOverlay } from "./mcp_history_render.ts";
import { renderMcpActivityStrip } from "./mcp_activity.ts";
import { receiverBlocksLocalSend, renderReceiverActivity } from "./receiver_activity.ts";
import { renderOriginWork } from "./origin_work.ts";
import { hubExecutionRoute } from "./hub_state.ts";
import { transcriptAnchors, turnPageLoadPending } from "./history_navigation.ts";
import { renderMarkdown } from "./markdown.ts";
import { renderEarlierHistoryTrigger, renderTranscriptRows } from "./render_transcript.ts";
import { latestLocalRevisionSource, localRevisionActionEnabled } from "./local_revision_source.ts";
import { navigationIsIdle, quickChatDeleteAction, sessionRowCapabilities } from "./navigation_state.ts";
import {
  renderAgentInspector,
  renderInlineAgentActivity,
  renderSubAgentSummaryTrigger,
} from "./render_agent_activity.ts";
import { agentDisplayName, stableAgentVisual } from "./agent_activity.ts";
import { runCanBeCancelled, runSurfaceActive } from "./run_control.ts";
import {
  classifyTaskActivity,
  renderTaskActivityBadge,
  renderTaskActivityIndicator,
  taskActivityStateForSessionRow,
} from "./task_activity_indicator.ts";
import { titlebarMenuPopupRole } from "./titlebar_interaction.ts";
import {
  initialSetupSteps,
  validateInitialSetupStep,
  type InitialSetupStep,
} from "./initial_setup_state.ts";
import {
  DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION,
  createDesktopRenderModel,
  type DesktopRenderLocalPresentation,
  type DesktopRenderModel,
} from "./render_projection.ts";
import type {
  ConfigFieldProjection,
  DesktopViewState,
  DesktopWebState,
  PendingTurnInput,
  ProjectRow,
  SessionRow,
  TaskActivityState,
} from "./types.ts";
import {
  sideChatModelOptionLabel,
  sideChatModelOptions,
  sideChatOwnerSessionId,
  type SideChatCatalogView,
} from "./ui_state.ts";
import {
  configCommitControlState,
  displayAccessLabel,
  escapeHtml,
  fileName,
  goalSlashCommandHint,
  providerOverlayFeedback,
  shortenPath,
  validateConfigFieldValues,
  validateConfigInput,
  USER_CONFIGURED_SYSTEM_PROMPT_MAX_CHARS,
} from "./utils.ts";
import { normalizeProviderBaseUrl, providerCapabilities } from "./view_state.ts";

import { renderConfirmation, renderLocalConfirmation } from "./render_overlays.ts";

export type { LocalConfirmation } from "./render_overlays.ts";
export { renderConfirmation, renderLocalConfirmation };

const splashLogoUrl = new URL("../../../logo/fabicon/android-chrome-512x512.png", import.meta.url).href;

const TYPED_CONFIG_KEYS: readonly string[] = Object.freeze([
  "model.base_url",
  "model.model",
  "model.provider_profile",
  "model.api_key_env",
  "model.system_prompt",
  "model.context_window",
  "model.max_output_tokens",
  "model.request_timeout_ms",
  "model.supports_tools",
  "model.supports_images",
  "model.parallel_tool_calls",
  "side_chat.base_url",
  "side_chat.model",
  "side_chat.provider_profile",
  "side_chat.system_prompt",
  "side_chat.context_window",
  "side_chat.request_timeout_ms",
  "side_chat.connect_timeout_ms",
  "side_chat.max_retries",
  "permissions.access_mode",
  "multi_agent.enabled",
  "multi_agent.mode",
  "multi_agent.max_concurrent_agents",
  "multi_agent.max_concurrent_model_requests",
  "shell.hide_windows",
  "inspection.default_max_depth",
  "inspection.default_max_entries_per_dir",
  "inspection.max_extensions_reported",
  "inspection.include_hidden_by_default",
  "file_guard.max_inline_read_bytes",
  "file_guard.large_file_warning_bytes",
  "file_guard.blocked_read_extensions",
  "file_guard.structured_document_extensions",
  "docling.enabled",
  "docling.base_url",
  "docling.timeout_ms",
  "docling.api_key_env",
  "docling.headers_json",
  "mcp.enabled",
  "mcp.servers_json",
]);

const HOST_OWNED_MODEL_KEYS = new Set([
  "model.max_output_tokens",
  "model.temperature",
  "model.top_p",
  "model.top_k",
  "model.presence_penalty",
  "model.frequency_penalty",
  "model.seed",
  "model.stop_sequences",
  "model.extra_body_json",
  "model.supports_reasoning",
  "model.reasoning_effort",
  "model.reasoning_summary",
  "model.chat_completions_reasoning_parameters",
]);

const INITIAL_SETUP_PROVIDER_KEYS = new Set([
  "model.base_url",
  "model.provider_profile",
  "model.api_key_env",
  "model.context_window",
  "model.max_output_tokens",
]);
const PROVIDER_PROFILE_LABELS: Readonly<Record<string, string>> = Object.freeze({
  lm_studio: "LM Studio (Responses API)",
  openai_compatible: "OpenAI-compatible (Chat Completions)",
  openai_responses: "OpenAI Responses API",
  lm_studio_chat_completions: "LM Studio (Chat Completions)",
});
const INITIAL_SETUP_MODEL_PRIMARY_KEYS = new Set([
  "model.model",
  "model.supports_tools",
  "model.supports_images",
  "model.parallel_tool_calls",
]);
const INITIAL_SETUP_TOOL_PRIMARY_KEYS = new Set([
  "docling.enabled",
  "docling.base_url",
  "docling.timeout_ms",
  "docling.api_key_env",
  "mcp.enabled",
  "mcp.servers_json",
]);

export interface DesktopMarkupOptions {
  readonly backgroundInert: boolean;
  readonly taskActivityDelay: string;
  readonly mcpActivityDelay?: string;
}

/** Pure Desktop markup composition from one explicit immutable render model. */
export function renderDesktopMarkup(
  model: DesktopRenderModel,
  options: DesktopMarkupOptions,
): string {
  const state = model.view;
  const local = model.local;
  const localConfirmationPending = local.modal.localConfirmation !== null;
  const settingsClosePending = local.modal.localConfirmation?.kind === "settings_close"
    || local.modal.localConfirmation?.kind === "session_settings_close";
  const sideChatDeletePending = local.sideChat.deleteConfirmation !== null;
  const localModalObscuresOverlay = (localConfirmationPending && !settingsClosePending)
    || sideChatDeletePending;
  if (state.hub_project_open === true) {
    return applyActionAvailabilityToButtons(`<div class="app-frame hub-project-frame" style="--window-opacity: ${state.window_opacity_percent / 100}">${renderTitlebar(local.windowMaximized, options.backgroundInert, state.overlay)}<div class="shell hub-project-shell" ${options.backgroundInert ? 'inert aria-hidden="true"' : ""}>${renderSidebar(state, local.sharedWork)}${renderSharedWork(local.sharedWork, renderReceiverActivity(local.deviceNetwork, local.sharedWork))}</div></div>
      ${state.confirmation_visible ? renderConfirmation(state, local.modal.permissionDecision) : ""}
      ${!state.confirmation_visible && !localModalObscuresOverlay && state.overlay !== "none" ? renderOverlay(state, local, model) : ""}
      ${!state.confirmation_visible && local.modal.localConfirmation ? renderLocalConfirmation(local.modal.localConfirmation, local.modal.localDecisionPending, local.modal.localDecisionError) : ""}
      ${!state.confirmation_visible && !localConfirmationPending && sideChatDeletePending ? renderSideChatDeleteConfirmation(state, local) : ""}
      ${options.backgroundInert ? "" : renderRecoverableError(local.recoverableError)}`, model);
  }
  if (startupSetupRequired(state) && state.overlay === "initial_setup") {
    const setupMarkup = `
      <div class="app-frame initial-setup-frame" style="--window-opacity: ${state.window_opacity_percent / 100}">
        ${renderTitlebar(local.windowMaximized, true, "")}
        ${renderInitialSetupWizard(state, local)}
      </div>
      ${state.confirmation_visible ? renderConfirmation(state, local.modal.permissionDecision) : ""}
    `;
    return applyActionAvailabilityToButtons(setupMarkup, model);
  }
  const markup = `
    <div class="app-frame ${local.artifactPane.collapsed ? "artifact-collapsed" : ""} ${!local.artifactPane.collapsed && local.artifactPane.mode === "side_chat" ? "side-chat-open" : ""}" style="--window-opacity: ${state.window_opacity_percent / 100}; --task-activity-delay: ${options.taskActivityDelay}; --mcp-activity-delay: ${options.mcpActivityDelay ?? "0ms"}">
      ${renderTitlebar(local.windowMaximized, options.backgroundInert, state.overlay)}
      <div class="shell" ${options.backgroundInert ? 'inert aria-hidden="true"' : ""}>
        ${renderSidebar(state, local.sharedWork)}
        ${renderConversationMain({
          topbar: renderTopbar(state, local),
          activity: `${renderRunStatusStrip(state)}${renderReceiverActivity(local.deviceNetwork, local.sharedWork)}${renderOriginWork(local.deviceNetwork, state.draft_target.sessionId, state.can_cancel_run)}`,
          thread: renderThreadContent(state, local),
          composer: renderComposer(state, local),
        })}
        ${renderArtifactPane(state, local)}
      </div>
    </div>
    ${state.confirmation_visible ? renderConfirmation(state, local.modal.permissionDecision) : ""}
    ${!state.confirmation_visible && !localModalObscuresOverlay && state.overlay !== "none" ? renderOverlay(state, local, model) : ""}
    ${
      !state.confirmation_visible && local.modal.localConfirmation
        ? renderLocalConfirmation(
            local.modal.localConfirmation,
            local.modal.localDecisionPending,
            local.modal.localDecisionError,
          )
        : ""
    }
    ${
      !state.confirmation_visible && !localConfirmationPending && sideChatDeletePending
        ? renderSideChatDeleteConfirmation(state, local)
        : ""
    }
    ${options.backgroundInert ? "" : renderRecoverableError(local.recoverableError)}
  `;
  return applyActionAvailabilityToButtons(markup, model);
}

/**
 * Normalizes button availability once at the final markup boundary. Surface owners still decide
 * whether a control exists and what it says; the action registry alone decides whether it can run.
 */
function applyActionAvailabilityToButtons(html: string, model: DesktopRenderModel): string {
  return html.replace(/<button\b[^>]*>/gi, (tag) => {
    const action = htmlAttribute(tag, "data-action");
    if (!action) return tag;
    const payload: ActionPayload = {
      index: Number(htmlAttribute(tag, "data-index") ?? "-1"),
      value: htmlAttribute(tag, "data-agent-path")
        ?? htmlAttribute(tag, "data-history-target")
        ?? htmlAttribute(tag, "data-provider-profile")
        ?? htmlAttribute(tag, "data-mode")
        ?? htmlAttribute(tag, "data-value")
        ?? "",
    };
    const enabled = actionEnabledById(action, model, payload);
    const normalized = tag
      .replace(/\sdisabled(?:=(?:"[^"]*"|'[^']*'|[^\s>]+))?/gi, "")
      .replace(/\saria-disabled=(?:"[^"]*"|'[^']*'|[^\s>]+)/gi, "");
    return normalized.replace(/>$/, `${enabled ? "" : " disabled"} aria-disabled="${String(!enabled)}">`);
  });
}

function htmlAttribute(tag: string, name: string): string | null {
  const match = new RegExp(`\\b${name}=(?:"([^"]*)"|'([^']*)')`, "i").exec(tag);
  const value = match?.[1] ?? match?.[2];
  return value === undefined ? null : decodeHtmlAttribute(value);
}

function decodeHtmlAttribute(value: string): string {
  return value
    .replaceAll("&quot;", '"')
    .replaceAll("&#39;", "'")
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&amp;", "&");
}

export function renderStartupSplash(state: DesktopWebState, elapsedMs: number, minVisibleMs: number): string {
  const remainingMs = Math.max(0, minVisibleMs - elapsedMs);
  const progressLabel =
    remainingMs > 0
      ? "起動中"
      : state.startup.status === "ready"
        ? "準備完了"
        : "確認が必要";
  return `
    <div class="splash-screen">
      <div class="splash-core">
        <img class="splash-logo" src="${splashLogoUrl}" alt="moyAI" />
        <div class="splash-title">${escapeHtml(state.startup.title)}</div>
        <div class="splash-message">${escapeHtml(state.startup.message)}</div>
        <div class="splash-progress" aria-label="${escapeHtml(progressLabel)}">
          <span></span>
        </div>
        <div class="splash-detail">${escapeHtml(state.startup.detail)}</div>
        <div class="splash-checks">
          ${state.startup.checks
            .map(
              (check) => `
                <div class="splash-check ${check.status}">
                  <span class="splash-check-status">${startupCheckMark(check.status)}</span>
                  <span class="splash-check-label">${escapeHtml(check.label)}</span>
                  <span class="splash-check-message">${escapeHtml(check.message)}</span>
                </div>
              `,
            )
            .join("")}
        </div>
      </div>
    </div>
  `;
}

function startupCheckMark(status: string): string {
  if (status === "pass") return "OK";
  if (status === "warning") return "!";
  if (status === "fail") return "NG";
  return "…";
}

const INITIAL_SETUP_STEP_LABELS: Readonly<Record<InitialSetupStep, string>> = {
  start: "使い方を選ぶ",
  provider: "AIへの接続",
  model: "モデルを選ぶ",
  permissions: "操作の承認",
  tools: "任意ツール",
  finish: "確認して始める",
};

function renderInitialSetupWizard(
  state: DesktopViewState,
  local: Readonly<DesktopRenderLocalPresentation>,
): string {
  const step = local.initialSetup.step;
  const steps: readonly InitialSetupStep[] = step === "start" ? ["start"] : initialSetupSteps(local.initialSetup.guided ?? false);
  const execution = state.startup.onboarding_intent === "execution";
  const stepIndex = steps.indexOf(step);
  const values = state.config_fields.map((field) => ({ key: field.key, text: field.value }));
  const validation = validateInitialSetupStep(step, state.config_fields, values);
  const auxiliaryKind = local.initialSetup.auxiliaryPendingKind;
  const pending = local.initialSetup.finishPending
    || local.configMutationPending
    || auxiliaryKind !== null;
  const pendingMessage = local.initialSetup.finishPending || local.configMutationPending
    ? "設定を保存しています…"
    : auxiliaryKind === "import"
      ? "TOML設定を読み込んでいます…"
      : auxiliaryKind === "docling_readiness"
        ? "Doclingの接続確認を開始しています…"
        : auxiliaryKind === "purpose" ? "利用目的を保存しています…" : "";
  return `
    <main class="initial-setup-shell" data-surface="initial-setup" data-current-step="${step}" aria-labelledby="initial-setup-title" aria-busy="${String(pending)}">
      <aside class="initial-setup-progress" aria-label="初期設定の進行状況">
        <div class="initial-setup-brand">
          <span>moyAI</span>
          <strong>初回設定</strong>
        </div>
        <ol>
          ${steps.map((candidate, index) => `
            <li data-step="${candidate}" data-step-state="${index < stepIndex ? "complete" : index === stepIndex ? "current" : "upcoming"}" aria-label="${index + 1}. ${escapeHtml(INITIAL_SETUP_STEP_LABELS[candidate])}" ${index === stepIndex ? 'aria-current="step"' : ""}>
              <span>${index + 1}</span>
              <strong>${escapeHtml(INITIAL_SETUP_STEP_LABELS[candidate])}</strong>
            </li>
          `).join("")}
        </ol>
        <p>${step === "start" ? "選んだ使い方に必要な設定をご案内します。" : execution ? "①このPCのAIを設定 → ②Hubへ接続 → ③保存先と実行を許可 → ④管理者がプロジェクトへ割り当てます。" : "接続テストは必要なときに実行できます。未接続でも設定を保存できます。"}</p>
      </aside>
      <section class="initial-setup-workspace">
        <header class="initial-setup-header">
          <div>
            <small>${step === "start" ? "ようこそ moyAI へ" : `STEP ${stepIndex + 1} / ${steps.length}`}</small>
            <h1 id="initial-setup-title">${escapeHtml(INITIAL_SETUP_STEP_LABELS[step])}</h1>
          </div>
          <span class="initial-setup-reason">${escapeHtml(initialSetupReasonLabel(state.startup.initial_setup_reason))}</span>
        </header>
        <div class="initial-setup-content" data-step-panel="${step}">
          ${renderInitialSetupStep(state, local, step)}
        </div>
        <div id="settings-validation" class="initial-setup-validation validation ${validation.ok ? "ok" : "error"}" data-settings-live-region="initial-setup-validation" role="status" aria-live="polite">
          ${escapeHtml(pending ? pendingMessage : validation.ok ? validation.message : `${validation.invalidKey}: ${validation.message}`)}
        </div>
        ${state.status_code === "initial_setup_preferences_save_failed" ? `<div class="initial-setup-status">
          <div class="validation error" role="alert" aria-live="assertive"><strong>${escapeHtml(state.status_message)}</strong></div>
        </div>` : ""}
        ${renderInitialSetupRecoverableError(local.recoverableError)}
        <footer class="initial-setup-actions">
          <button data-action="initial-setup-back" ${step === "start" ? "hidden" : ""}>前へ</button>
          <span>${step === "finish" ? execution ? "保存後、Hubへの接続とこのPCの実行設定へ進みます。" : "保存するとチャット画面を開きます。" : "入力内容は最後の画面で保存します。"}</span>
          ${step === "finish"
            ? `<button id="initial-setup-primary" class="send wide-send" data-action="finish-initial-setup">${pending ? "保存しています…" : execution ? "AI設定を保存してPCの接続へ" : "設定を保存してmoyAIを開く"}</button>`
            : `<button id="initial-setup-primary" class="${step === "start" ? "" : "send"}" data-action="initial-setup-next">${step === "start" ? "すべての設定を確認" : "次へ"}</button>`}
        </footer>
      </section>
    </main>
  `;
}

function renderInitialSetupRecoverableError(
  error: DesktopRenderLocalPresentation["recoverableError"],
): string {
  return renderSettingsRecoverableError(
    error,
    "initial-setup-recoverable-error",
    "initial-setup-error-notice",
  );
}

function renderSettingsRecoverableError(
  error: DesktopRenderLocalPresentation["recoverableError"],
  identity: string,
  extraClass = "",
): string {
  const visible = error !== null;
  return `
    <aside id="${identity}" class="ui-error-notice ${extraClass}" data-settings-passive="${identity}" data-settings-preserve-focused-region role="alert" aria-live="assertive" aria-atomic="true" ${visible ? "" : 'hidden aria-hidden="true"'}>
      <div>
        <strong>${visible ? escapeHtml(error.title) : ""}</strong>
        <span>${visible ? escapeHtml(error.hint) : ""}</span>
        ${visible && error.details.trim().length > 0 ? `<details data-details-key="${identity}-details"><summary data-focus-key="${identity}-summary">技術詳細</summary><pre>${escapeHtml(error.details)}</pre></details>` : ""}
      </div>
      <button class="icon-button" data-action="dismiss-ui-error" title="閉じる" aria-label="エラー通知を閉じる" ${visible ? "" : "hidden"}>×</button>
    </aside>
  `;
}

function initialSetupReasonLabel(
  reason: DesktopWebState["startup"]["initial_setup_reason"],
): string {
  if (reason === "config_missing") return "初回設定が未完了です";
  if (reason === "setup_unfinished") return "前回の初回設定を再開";
  if (reason === "provider_invalid") return "AIの接続設定を修正してください";
  if (reason === "optional_tool_invalid") return "追加ツールの設定を修正してください";
  return "ローカル設定を確認してください";
}

function renderInitialSetupStep(
  state: DesktopViewState,
  local: Readonly<DesktopRenderLocalPresentation>,
  step: InitialSetupStep,
): string {
  if (step === "start") return renderInitialSetupStartStep(state, local);
  if (step === "provider") return renderInitialSetupProviderStep(state, local);
  if (step === "model") return renderInitialSetupModelStep(state, local);
  if (step === "permissions") return renderInitialSetupPermissionsStep(state);
  if (step === "tools") return renderInitialSetupToolsStep(state, local);
  return renderInitialSetupFinishStep(state, local);
}

function renderInitialSetupStartStep(
  state: DesktopViewState,
  local: Readonly<DesktopRenderLocalPresentation>,
): string {
  const configPath = state.startup.global_config_path ?? state.startup.setup_target?.globalConfigPath ?? "";
  const importedSourcePath = local.initialSetup.importedSourcePath;
  const importing = local.initialSetup.auxiliaryPendingKind === "import";
  return `
    <section class="initial-setup-section" aria-labelledby="initial-setup-start-heading">
      <div class="initial-setup-intro">
        <h2 id="initial-setup-start-heading">moyAIをどう使いますか</h2>
        <p>使い方を選んでください。後から変更・追加できます。途中で終了した場合、選んだ使い方から再開できますが、未保存の入力はやり直しになります。</p>
        ${state.startup.onboarding_intent === "hosting" ? '<p role="status">Hubの管理画面を開き、チームの設定を続けられます。設定後は「チームに参加する」から、このPCも接続できます。</p>' : ""}
      </div>
      <dl class="initial-setup-path">
        <dt>保存先</dt>
        <dd title="${escapeHtml(configPath)}">${escapeHtml(configPath || "保存先を取得できませんでした")}</dd>
      </dl>
      <div class="initial-setup-choice-row">
        <div>
          <strong>チームの仕事をこのPCで実行する</strong>
          <p>接続ファイルでHubに参加します。参加承認後、このPCの実行許可とプロジェクトの作業フォルダーを設定します。AIはHubに登録されたモデルを使います。</p>
          <button id="initial-setup-execution" data-action="initial-setup-execution">チームの仕事をこのPCで実行する</button>
        </div>
        <div>
          <strong>自分のPCで使う</strong>
          <p>AIの接続先とモデルを選んで、チャットを始めます。作業フォルダーは後から選べます。</p>
          <button id="initial-setup-personal" data-action="initial-setup-personal">自分のPCで使う</button>
        </div>
        <div>
          <strong>チームに参加する</strong>
          <p>管理者から受け取った接続ファイルで参加します。依頼と結果の閲覧は、このPCにAIを設定せずに利用できます。</p>
          <button id="initial-setup-shared-work" data-action="initial-setup-team">チームに参加する</button>
        </div>
        <div>
          <strong>チーム環境を用意する</strong>
          <p>Hubの管理画面で、利用者・PC・プロジェクトを登録します。仕事の閲覧や実行の許可は、管理画面で別途設定します。</p>
          <button data-action="initial-setup-hosting">チーム環境を用意する</button>
        </div>
        <div>
          <strong>以前の設定ファイルを使う</strong>
          <p>選んだTOMLファイルの内容を設定欄に読み込みます。最後に保存するまで、現在の設定は変わりません。</p>
          <button data-action="import-config-toml" aria-describedby="initial-setup-import-help" aria-busy="${String(importing)}">${importing ? "読み込んでいます…" : "TOML設定を選択"}</button>
          <small id="initial-setup-import-help" class="settings-field-help" data-settings-passive="initial-setup-import-source">${importedSourcePath
            ? `読込元: ${escapeHtml(importedSourcePath)}。内容はまだ保存されていません。`
            : "ファイル選択をキャンセルすると、入力内容はそのまま残ります。"}</small>
        </div>
      </div>
      <details data-details-key="initial-setup-model-relay">
        <summary>チームのAIだけを、このPCのローカル作業で使う</summary>
        <p>このPCのチャットから、チームで共有しているAIを利用します。</p>
        <button id="initial-setup-hub" data-action="initial-setup-hub">Hubの共通設定で始める</button>
        <small class="settings-field-help">管理者がこのPCを承認すると、利用できるモデルを読み込みます。</small>
      </details>
    </section>
  `;
}

function renderInitialSetupProviderStep(state: DesktopViewState, local: Readonly<DesktopRenderLocalPresentation>): string {
  return `
    <section class="initial-setup-section" aria-labelledby="initial-setup-provider-heading">
      <div class="initial-setup-intro">
        <h2 id="initial-setup-provider-heading">使うAIの接続先</h2>
        <p>AIのURLと接続方式を入力してください。不明な場合は、AIを管理する担当者に確認してください。まだ接続できなくても設定を保存できます。</p>
      </div>
      <div class="settings-grid-two initial-setup-form-grid">
        ${renderConfigTextField(state, "model.base_url", "接続先URL", "url", "", { initialSetup: true })}
        ${renderConfigEnumField(state, "model.provider_profile", "接続方式", PROVIDER_PROFILE_LABELS, { initialSetup: true })}
        ${renderConfigTextField(
          state,
          "model.api_key_env",
          "APIキーの環境変数名（任意）",
          "text",
          "APIキーそのものではなく、環境変数名（例: OPENAI_API_KEY）を入力します。認証不要なら空欄です。",
          { initialSetup: true },
        )}
        ${local.initialSetup.guided ? '<details data-details-key="initial-setup-context-budget"><summary>入力の整理に使う設定</summary>' : ""}
        ${renderConfigTextField(
          state,
          "model.context_window",
          "moyAIの入力整理上限",
          "number",
          "moyAIが会話を整理する際の上限です。AI側の設定値は変更しません。",
          { initialSetup: true },
        )}
        ${local.initialSetup.guided ? '</details>' : ""}
      </div>
      <div class="initial-setup-note">「次へ」では設定を入力するだけで、AIへの接続は行いません。</div>
    </section>
  `;
}

function renderInitialSetupModelStep(state: DesktopViewState, local: Readonly<DesktopRenderLocalPresentation>): string {
  const modelField = configField(state, "model.model");
  const currentModel = modelField?.field.value ?? "";
  const options = state.provider_model_ids.map((id, index) => ({
    id,
    label: state.provider_models[index] ?? id,
  }));
  if (currentModel && !options.some((option) => option.id === currentModel)) {
    options.unshift({ id: currentModel, label: `${currentModel}（現在の入力）` });
  }
  const describedBy = modelField
    ? configFieldDescriptionIds(modelField.field, [], false)
    : "settings-validation";
  const advancedFields = state.config_fields.filter((field) => (
    field.key.startsWith("model.")
    && !INITIAL_SETUP_PROVIDER_KEYS.has(field.key)
    && !INITIAL_SETUP_MODEL_PRIMARY_KEYS.has(field.key)
    && !HOST_OWNED_MODEL_KEYS.has(field.key)
  ));
  return `
    <section class="initial-setup-section" aria-labelledby="initial-setup-model-heading">
      <div class="settings-section-head initial-setup-intro">
        <div>
          <h2 id="initial-setup-model-heading">使用するモデル</h2>
          <p>モデル一覧から選ぶか、モデルIDを入力してください。回答の長さや思考の設定には、AIサーバー側の設定を使います。</p>
        </div>
        <button data-action="load-provider-models">${state.provider_loading ? "読込中…" : "モデル一覧を読み込む"}</button>
      </div>
      ${modelField ? `
        <div class="settings-grid-two initial-setup-form-grid">
          <div class="settings-field">
            <label for="initial-setup-model-select">モデル候補</label>
            <select id="initial-setup-model-select" class="settings-control" data-main-provider-model-control data-config-index="${modelField.index}" data-config-key="model.model" aria-describedby="${describedBy}" ${options.length > 0 ? "" : "disabled"}>
              ${options.map((option) => `<option value="${escapeHtml(option.id)}" ${option.id === currentModel ? "selected" : ""}>${escapeHtml(option.label)}</option>`).join("")}
            </select>
            <small class="settings-field-help">接続先から読み込んだモデルの一覧です。</small>
          </div>
          <div class="settings-field">
            <label for="initial-setup-model-manual">モデルID</label>
            <input id="initial-setup-model-manual" class="settings-control" data-main-provider-model-control data-config-index="${modelField.index}" data-config-key="model.model" value="${escapeHtml(currentModel)}" autocomplete="off" spellcheck="false" aria-describedby="${describedBy}" />
            ${renderConfigFieldHelp(modelField.field, "一覧にないモデルIDも入力できます。", true)}
          </div>
        </div>
      ` : renderMissingConfigField("model.model")}
      ${local.initialSetup.guided ? '<details data-details-key="initial-setup-capabilities"><summary>AIの機能を確認・変更</summary>' : ""}
      <div class="settings-toggle-grid initial-setup-capabilities">
        ${renderConfigToggleField(state, "model.supports_tools", "ツール利用", { initialSetup: true })}
        ${renderConfigToggleField(state, "model.supports_images", "画像入力", { initialSetup: true })}
        ${renderConfigToggleField(state, "model.parallel_tool_calls", "ツールの並列呼び出し", { initialSetup: true })}
      </div>
      ${local.initialSetup.guided ? '</details>' : ""}
      ${renderInitialSetupAdvancedSection(
        state,
        "model",
        advancedFields,
        "initial-setup-model-advanced",
        "モデルの詳細設定",
      )}
      <div class="provider-status ${state.provider_status.kind === "success" ? "ok" : state.provider_status.kind}" data-settings-live-region="initial-setup-provider-status" role="status" aria-live="polite">
        <strong>${escapeHtml(state.provider_status.title)}</strong>
        <p>${escapeHtml(state.provider_status.hint)}</p>
      </div>
    </section>
  `;
}

function renderInitialSetupPermissionsStep(state: DesktopViewState): string {
  return `
    <section class="initial-setup-section" aria-labelledby="initial-setup-permissions-heading">
      <div class="initial-setup-intro">
        <h2 id="initial-setup-permissions-heading">ツール実行の承認方法</h2>
        <p>新しいチャットで使う承認方法です。チャットを開いた後は「このチャットの設定」で個別に変更できます。</p>
      </div>
      ${renderConfigEnumField(state, "permissions.access_mode", "承認方法", {
        default: "承認を求める",
        auto_review: "代理で承認",
        full_access: "フルアクセス",
      }, { initialSetup: true })}
      <div class="initial-setup-permission-guide">
        <div><strong>承認を求める</strong><span>ファイルの変更など、承認が必要な操作を人が判断します。</span></div>
        <div><strong>代理で承認</strong><span>別のAIが操作を審査します。代理で判断できない場合は、あなたに確認します。禁止された操作は実行せず停止します。</span></div>
        <div><strong>フルアクセス</strong><span>操作ごとの確認を省き、このPCの現在のユーザー権限で実行します。</span></div>
      </div>
    </section>
  `;
}

function renderInitialSetupToolsStep(
  state: DesktopViewState,
  local: Readonly<DesktopRenderLocalPresentation>,
): string {
  const doclingEnabled = configField(state, "docling.enabled")?.field.value.trim().toLowerCase() === "true";
  const dependencyOptions: ConfigFieldRenderOptions = {
    disabled: !doclingEnabled,
    initialSetup: true,
  };
  const advancedFields = state.config_fields.filter((field) => (
    (field.key.startsWith("docling.") || field.key.startsWith("mcp."))
    && !INITIAL_SETUP_TOOL_PRIMARY_KEYS.has(field.key)
  ));
  return `
    <section class="initial-setup-section" aria-labelledby="initial-setup-tools-heading">
      <div class="initial-setup-intro">
        <h2 id="initial-setup-tools-heading">追加ツール（任意）</h2>
        <p>文書変換や外部ツールが必要な場合に設定してください。後から「設定」で追加できます。</p>
      </div>
      <div class="initial-setup-tool-band">
        <div class="settings-section-head compact">
          <div><h3>Docling</h3><p>PDF・Wordなどの文書をAIが読める形式に変換します。</p></div>
          <div class="settings-tool-actions">
            ${renderConfigToggleField(state, "docling.enabled", "Doclingを有効化", { initialSetup: true })}
            <button data-action="check-docling-readiness" aria-controls="docling-readiness-status" aria-busy="${String(local.initialSetup.auxiliaryPendingKind === "docling_readiness")}">${local.initialSetup.auxiliaryPendingKind === "docling_readiness" ? "確認中…" : "Doclingへの接続を試す"}</button>
            <span class="settings-field-help">入力した接続先を試します。設定はまだ保存しません。</span>
          </div>
        </div>
        <div class="settings-grid-two">
          ${renderConfigTextField(state, "docling.base_url", "Doclingの接続先URL", "url", "", dependencyOptions)}
          ${renderConfigTextField(state, "docling.timeout_ms", "待ち時間の上限（ms）", "number", "", dependencyOptions)}
          ${renderConfigTextField(state, "docling.api_key_env", "APIキーの環境変数名", "text", "", dependencyOptions)}
        </div>
        ${renderDoclingReadiness(
          state,
          local.initialSetup.auxiliaryPendingKind === "docling_readiness",
          {
            allowDirtyDraft: true,
            projectedResultVisible: local.initialSetup.doclingReadinessVisible,
          },
        )}
      </div>
      <div class="initial-setup-tool-band">
        <div class="settings-section-head compact">
          <div><h3>MCP</h3><p>ここで登録したHTTP接続のMCPサーバーを利用します。</p></div>
          ${renderConfigToggleField(state, "mcp.enabled", "MCPを有効化", { initialSetup: true })}
        </div>
        <details data-details-key="initial-setup-mcp-advanced">
          <summary>MCPサーバーの詳細設定</summary>
          ${renderConfigJsonField(state, "mcp.servers_json", "MCPサーバー設定（JSON）", { initialSetup: true })}
        </details>
      </div>
      ${renderInitialSetupAdvancedSection(
        state,
        "tools",
        advancedFields,
        "initial-setup-tools-advanced",
        "追加ツールの詳細設定",
      )}
    </section>
  `;
}

function renderInitialSetupFinishStep(
  state: DesktopViewState,
  local: Readonly<DesktopRenderLocalPresentation>,
): string {
  const warnings = [
    ...state.startup.checks.filter((check) => check.status !== "pass").map((check) => check.message),
    ...(state.provider_status.kind === "warning" || state.provider_status.kind === "error"
      ? [state.provider_status.hint]
      : []),
    ...(local.initialSetup.doclingReadinessVisible
      && state.docling_readiness.status === "unavailable"
      ? [state.docling_readiness.message]
      : []),
  ].filter((message, index, all) => message.trim().length > 0 && all.indexOf(message) === index);
  const advancedFields = state.config_fields.filter((field) => (
    !field.key.startsWith("model.")
    && !field.key.startsWith("docling.")
    && !field.key.startsWith("mcp.")
    && field.key !== "permissions.access_mode"
  ));
  return `
    <section class="initial-setup-section" aria-labelledby="initial-setup-finish-heading">
      <div class="initial-setup-intro">
        <h2 id="initial-setup-finish-heading">保存内容を確認</h2>
        <p>${state.startup.onboarding_intent === "execution" ? "AI設定を保存し、Hubへの接続へ進みます。仕事を実行する保存先と承認方法は、その後にこのPCで設定します。" : "設定を保存してチャットを開きます。AIに接続できない場合は、後から「設定」で接続先を変更できます。"}</p>
      </div>
      ${local.initialSetup.guided && state.startup.onboarding_intent !== "execution" ? renderInitialSetupPermissionsStep(state) : ""}
      ${local.initialSetup.guided ? `<details data-details-key="initial-setup-optional-tools" ${validateInitialSetupStep("tools", state.config_fields, state.config_fields.map(field => ({ key: field.key, text: field.value }))).ok ? "" : "open"}><summary>任意ツール（後から設定できます）</summary>${renderInitialSetupToolsStep(state, local)}</details>` : ""}
      <div class="initial-setup-review-grid" data-settings-live-region="initial-setup-review">
        <section aria-labelledby="initial-setup-diff-heading">
          <h3 id="initial-setup-diff-heading">変更予定</h3>
          ${local.initialSetup.differences.length === 0
            ? '<p class="initial-setup-empty-review">既定値をそのまま保存します。</p>'
            : `<dl class="initial-setup-diff">${local.initialSetup.differences.map((entry) => `
                <div><dt>${escapeHtml(entry.key)}</dt><dd><del>${escapeHtml(entry.before || "(未設定)")}</del><ins>${escapeHtml(entry.after || "(未設定)")}</ins></dd></div>
              `).join("")}</dl>`}
        </section>
        <section aria-labelledby="initial-setup-warning-heading">
          <h3 id="initial-setup-warning-heading">接続と設定の状態</h3>
          ${warnings.length === 0
            ? '<p class="initial-setup-empty-review">保存できます。AIや追加ツールに接続できない場合は、使用時にお知らせします。</p>'
            : `<ul class="initial-setup-warnings">${warnings.map((warning) => `<li>${escapeHtml(warning)}</li>`).join("")}</ul>`}
        </section>
      </div>
      <dl class="initial-setup-path compact">
        <dt>保存先</dt><dd>${escapeHtml(state.startup.global_config_path ?? state.startup.setup_target?.globalConfigPath ?? "")}</dd>
      </dl>
      ${renderInitialSetupAdvancedSection(
        state,
        "finish",
        advancedFields,
        "initial-setup-finish-advanced",
        "その他の詳細設定",
      )}
    </section>
  `;
}

function renderInitialSetupAdvancedSection(
  state: DesktopViewState,
  step: InitialSetupStep,
  fields: readonly ConfigFieldProjection[],
  detailsKey: string,
  title: string,
): string {
  if (fields.length === 0) return "";
  const validation = validateInitialSetupStep(
    step,
    state.config_fields,
    state.config_fields.map((field) => ({ key: field.key, text: field.value })),
  );
  const invalidKey = validation.ok ? null : validation.invalidKey;
  const invalidHere = invalidKey !== null && fields.some((field) => field.key === invalidKey);
  const alertId = `${detailsKey}-validation`;
  return `
    <details class="initial-setup-advanced" data-details-key="${detailsKey}" ${invalidHere ? "open" : ""}>
      <summary>${escapeHtml(title)} <span>${fields.length}項目</span></summary>
      <p class="settings-field-help">必要な項目だけ変更してください。入力できる値は各項目の説明を参照してください。</p>
      ${invalidHere ? `<p id="${alertId}" class="initial-setup-advanced-error" role="alert">${escapeHtml(invalidKey)}: ${escapeHtml(validation.message)}。下の該当項目を修正してください。</p>` : ""}
      <div class="settings-grid-two initial-setup-advanced-grid">
        ${fields.map((field) => renderInitialSetupTypedField(state, field)).join("")}
      </div>
    </details>
  `;
}

function renderInitialSetupTypedField(
  state: DesktopViewState,
  field: ConfigFieldProjection,
): string {
  const options: ConfigFieldRenderOptions = { initialSetup: true };
  if (field.key === "model.system_prompt" || field.key === "side_chat.system_prompt") {
    return renderConfigMultilineField(
      state,
      field.key,
      field.key,
      `組み込みの指示に追加します。空欄は追加なしです。${USER_CONFIGURED_SYSTEM_PROMPT_MAX_CHARS.toLocaleString("ja-JP")}文字以内。`,
      options,
    );
  }
  if (field.value_type === "boolean") {
    return renderConfigToggleField(state, field.key, field.key, options);
  }
  if (field.value_type === "enum" || field.options.length > 0) {
    return renderConfigEnumField(
      state,
      field.key,
      field.key,
      Object.fromEntries(field.options.map((value) => [value, value])),
      options,
    );
  }
  if (field.value_type === "json") {
    return renderConfigJsonField(state, field.key, field.key, options);
  }
  return renderConfigTextField(
    state,
    field.key,
    field.key,
    field.value_type === "integer" || field.value_type === "number" ? "number" : "text",
    "",
    options,
  );
}

export function renderTitlebar(maximized = false, applicationCommandsInert = false, activeOverlay = ""): string {
  const maximizeLabel = maximized ? "元のサイズに戻す" : "最大化";
  const applicationCommandsState = applicationCommandsInert ? ' inert aria-hidden="true"' : "";
  const menuTrigger = (menu: ActionMenu, label: string): string => {
    const menuId = `titlebar-${menu}-menu`;
    const popupRole = titlebarMenuPopupRole(menu);
    return `<button id="${menuId}-trigger" data-action="show-${menu}-menu" aria-label="${label}メニュー" aria-haspopup="${popupRole}" aria-expanded="${activeOverlay === `${menu}_menu`}" aria-controls="${menuId}">${label}</button>`;
  };
  return `
    <header class="app-titlebar">
      <div class="titlebar-left">
        <span class="app-brand" data-drag-region>moyAI</span>
        <nav class="titlebar-menu" aria-label="アプリケーションメニュー"${applicationCommandsState}>
          ${menuTrigger("file", "ファイル")}
          ${menuTrigger("edit", "編集")}
          ${menuTrigger("view", "表示")}
          ${menuTrigger("help", "ヘルプ")}
        </nav>
      </div>
      <div class="titlebar-drag" data-drag-region></div>
      <div class="titlebar-controls">
        <button type="button" data-window-control data-action="minimize-window" title="最小化" aria-label="最小化"><span class="window-control-icon minimize-icon" aria-hidden="true"></span></button>
        <button type="button" data-window-control data-action="toggle-maximize-window" title="${maximizeLabel}" aria-label="${maximizeLabel}" aria-pressed="${maximized}"><span class="window-control-icon maximize-icon ${maximized ? "restore" : ""}" aria-hidden="true"></span></button>
        <button type="button" data-window-control data-action="close-window" title="トレイに格納します。完全終了は「ファイル」→「moyAIを終了」" aria-label="ウィンドウを閉じる（トレイに格納）"><span class="window-control-icon close-icon" aria-hidden="true"></span></button>
      </div>
    </header>
  `;
}

export function synchronizeTitlebarMenuState(
  titlebar: HTMLElement,
  activeOverlay: string,
  applicationCommandsInert: boolean,
): void {
  const applicationCommands = titlebar.querySelector<HTMLElement>(".titlebar-menu");
  applicationCommands?.toggleAttribute("inert", applicationCommandsInert);
  if (applicationCommandsInert) {
    applicationCommands?.setAttribute("aria-hidden", "true");
  } else {
    applicationCommands?.removeAttribute("aria-hidden");
  }
  for (const menu of ["file", "edit", "view", "help"] as const) {
    titlebar
      .querySelector<HTMLElement>(`#titlebar-${menu}-menu-trigger`)
      ?.setAttribute("aria-expanded", String(activeOverlay === `${menu}_menu`));
  }
}

function renderProjectRowWithSessions(state: DesktopWebState, row: ProjectRow, index: number): string {
  const selected = index === state.selected_project_index;
  const projectRow = renderProjectRow(
    row,
    selected,
    index,
    !navigationIsIdle(state),
  );
  if (!selected) {
    return projectRow;
  }
  const sessionRows = renderProjectSessionRows(state);
  return `${projectRow}${sessionRows}`;
}

function renderProjectRow(row: ProjectRow, selected: boolean, index: number, actionsDisabled: boolean): string {
  const disabled = actionsDisabled ? ' disabled aria-disabled="true"' : "";
  return `
    <div class="nav-row-wrap project-row ${selected ? "selected" : ""}">
      <button class="nav-row" data-action="project" data-index="${index}" data-focus-key="project:${escapeHtml(row.project_id)}:select"${selected ? ' aria-current="page"' : ""}${disabled}>
        <span class="nav-title">${escapeHtml(row.label)}</span>
        <small>${escapeHtml(row.path)}</small>
      </button>
      <button class="row-action add-session" data-action="new-project-session" data-index="${index}" data-focus-key="project:${escapeHtml(row.project_id)}:new-session" title="このプロジェクトで新しい開発チャット" aria-label="このプロジェクトで新しい開発チャット"${disabled}>${icon("plus")}</button>
      <button class="row-action danger" data-action="delete-project" data-index="${index}" data-focus-key="project:${escapeHtml(row.project_id)}:delete" title="削除" aria-label="削除"${disabled}>${icon("x")}</button>
    </div>
  `;
}

function selectedProjectDisplayLabel(
  state: Pick<DesktopViewState, "project_rows" | "selected_project_index" | "workspace_path">,
): string {
  const selected = state.project_rows[state.selected_project_index];
  return selected?.label.trim() || shortenPath(state.workspace_path);
}

function renderProjectSessionRows(state: DesktopWebState): string {
  if (state.selected_project_index < 0) {
    return "";
  }
  const searchDisabled = !navigationIsIdle(state)
    ? ' disabled aria-disabled="true"'
    : "";
  const search = `
    <div class="session-search">
      <input id="session-search" value="${escapeHtml(state.session_search_text)}" placeholder="チャット検索" aria-label="チャット検索"${searchDisabled} />
      <button class="${state.session_search_include_archived ? "selected" : ""}" data-action="toggle-session-archived-search" title="アーカイブ済みを含める" aria-label="アーカイブ済みを含める"${searchDisabled}>${icon("archive")}</button>
    </div>
  `;
  const rows = state.session_rows
    .map((row, index) => {
      const capabilities = sessionRowCapabilities(
        row.loaded_status,
        row.archived,
      );
      const selected = index === state.selected_session_index;
      const selectedActivityState = runSurfaceActive(state) && selected
        ? state.task_activity_state
        : "idle";
      const taskActivityState = taskActivityStateForSessionRow(row, selectedActivityState);
      return renderNavRow(
        sessionRowTitle(row, taskActivityState),
        sessionRowSubtitle(row, "開発チャット", taskActivityState),
        selected,
        "session",
        index,
        capabilities.rejoinAction,
        capabilities.secondaryAction,
        capabilities.rollbackAction,
        capabilities.deleteAction,
        taskActivityState,
        !navigationIsIdle(state),
        `session:${row.session_id}`,
      );
    })
    .join("");
  const activeFallback = rows.length === 0 ? renderActiveProjectSessionPlaceholder(state) : "";
  const currentChatHint = state.session_search_text.trim().length > 0
    && (state.selected_session_index >= 0 || activeFallback.length > 0)
    ? '<p class="empty-row">検索結果にかかわらず、現在開いているチャットも表示しています。</p>'
    : "";
  return rows.length > 0 || activeFallback.length > 0 || state.session_search_text.trim().length > 0
    ? `<div class="project-session-list">${search}${currentChatHint}${rows}${activeFallback}</div>`
    : `<div class="project-session-list">${search}</div>`;
}

function renderActiveProjectSessionPlaceholder(state: DesktopWebState): string {
  if (!runSurfaceActive(state)) {
    return "";
  }
  const label = activeSessionLabel(state);
  if (!label) {
    return "";
  }
  const activity = classifyTaskActivity(state.task_activity_state);
  const activityAttribute = activity
    ? ` data-task-activity-row="${activity.state}"`
    : "";
  return `
    <div class="nav-row-wrap selected project-session-placeholder"${activityAttribute}>
      <div class="nav-row">
        <span class="nav-title">${renderTaskActivityIndicator(state.task_activity_state, { decorative: true })}<span>${escapeHtml(label)}</span></span>
        <small>${activity ? `${activity.label} · ` : ""}開発チャット</small>
      </div>
    </div>
  `;
}

function activeSessionLabel(state: DesktopWebState): string {
  const candidates = [state.current_session_label, state.selected_session_title];
  for (const candidate of candidates) {
    const label = candidate.trim();
    if (label.length > 0 && label !== "新規チャット" && label !== "セッション未選択") {
      return label;
    }
  }
  return "";
}

function renderChatRows(state: DesktopWebState): string {
  if (state.chat_session_rows.length === 0) {
    return '<div class="empty">チャットはありません</div>';
  }
  const selectedChatSessionId =
    state.selected_project_index < 0 && state.selected_session_index >= 0
      ? state.chat_session_rows[state.selected_session_index]?.session_id
      : undefined;
  return state.chat_session_rows
    .map((row, index) => {
      const selected = row.session_id === selectedChatSessionId;
      const selectedActivityState = state.selected_project_index < 0
        && runSurfaceActive(state)
        && selected
        ? state.task_activity_state
        : "idle";
      const taskActivityState = taskActivityStateForSessionRow(row, selectedActivityState);
      return renderNavRow(
        sessionRowTitle(row, taskActivityState),
        sessionRowSubtitle(row, "通常チャット", taskActivityState),
        selected,
        "chat-session",
        index,
        "",
        "",
        "",
        quickChatDeleteAction(row.loaded_status),
        taskActivityState,
        !navigationIsIdle(state),
        `chat-session:${row.session_id}`,
      );
    })
    .join("");
}

function sessionRowTitle(row: SessionRow, taskActivityState: TaskActivityState): string {
  if (taskActivityState === "idle") return row.label;
  const title = row.title.trim();
  if (!title) return row.label;
  const shortId = row.short_id.trim();
  return shortId ? `${title} ${shortId}` : title;
}

function sessionRowSubtitle(
  row: SessionRow,
  fallback: string,
  taskActivityState: TaskActivityState,
): string {
  const activity = classifyTaskActivity(taskActivityState);
  if (activity) {
    const turn =
      typeof row.active_turn_sequence_no === "number"
        ? `turn ${row.active_turn_sequence_no}`
        : row.active_turn_id
          ? `turn ${row.active_turn_id.slice(0, 8)}`
          : "active turn";
    return `${activity.label} · ${turn}`;
  }
  if (row.loaded_status === "system_error") {
    return "状態取得エラー";
  }
  return fallback;
}

export function renderSidebar(state: DesktopWebState, shared?: import("./shared_work_state.ts").SharedWorkPresentation): string {
  const navigationDisabled = !navigationIsIdle(state);
  const hub = shared?.conceal ? null : shared?.projection;
  const localState = state.hub_project_open ? { ...state, selected_project_index: -1, selected_session_index: -1 } : state;
  return `
    <aside class="sidebar">
      <div class="window-actions">
        <button class="icon-button" data-action="show-shortcuts" title="ショートカット" aria-label="ショートカット">${icon("keyboard")}</button>
        <button class="icon-button" data-action="refresh" title="更新" aria-label="更新">${icon("refresh")}</button>
      </div>
      <button class="rail-item" data-action="show-hub" title="Hubに接続してモデルを確認">
        <span class="rail-icon">${icon("plug")}</span><span>moyAI Hub</span>
      </button>
      <div class="rail-section row-heading">
        <span>プロジェクト</span>
        <button class="tiny-button icon-only" data-action="create-project-from-picker" title="このPCにローカルプロジェクトを作成" aria-label="このPCにローカルプロジェクトを作成" ${navigationDisabled ? "disabled" : ""}>${icon("folder-plus")}</button>
      </div>
      <div class="row-list project-list">
        ${hub?.projects_stale && hub.projects.length ? hub.projects.map(project => `<div class="hub-project-row hub-project-stale"><div class="hub-project-heading"><button class="rail-item" type="button" disabled title="Hubから最新の参加状況を確認できません"><span class="rail-icon">${icon("folder")}</span><span>${escapeHtml(project.label)}</span><small class="project-source-badge">MCP · 未更新</small></button></div></div>`).join("") : hub?.principal ? hub.projects.map(project => {
          const selected = state.hub_project_open === true && hub.selected_project_id === project.id;
          const confirmation = shared?.confirmation?.kind === "leave_project" && shared.confirmation.projectId === project.id;
          const chats = selected ? sharedConversationRows(hub.status?.jobs ?? [], hub.selected_job_id, hub.conversations, hub.selected_conversation_id) : [];
          return `<div class="hub-project-row"><div class="hub-project-heading"><button class="rail-item ${selected ? "active" : ""}" data-action="open-hub-project" data-value="${escapeHtml(project.id)}" title="Hubで共有するプロジェクト"><span class="rail-icon">${icon("folder")}</span><span>${escapeHtml(project.label)}</span><small class="project-source-badge">MCP</small></button><button class="tiny-button icon-only" data-action="shared-request-leave-project" data-value="${escapeHtml(project.id)}" title="プロジェクトから離脱" aria-label="${escapeHtml(project.label)}から離脱" ${hub.leave_pending_project_id === project.id ? "disabled" : ""}>${icon("x")}</button></div>${hub.leave_pending_project_id === project.id ? '<p class="hub-project-pending" role="status">離脱処理待ちです。確認できるまで、このPCから新しい仕事は開始しません。</p>' : ""}${confirmation ? `<div class="hub-project-confirmation" role="group" aria-label="プロジェクトから離脱"><p>このPCが「${escapeHtml(project.label)}」から離脱します。他のPC、共有チャット、各PCのファイルは残ります。</p><button data-action="shared-confirm-confirmation">離脱する</button><button data-action="shared-cancel-confirmation">戻る</button></div>` : ""}${selected ? `<div class="hub-project-chats">${chats.map(chat => `<div class="hub-chat-row"><button data-action="${hub.conversations ? "shared-select-conversation" : "shared-detail"}" data-value="${escapeHtml(hub.conversations ? chat.conversationId : chat.jobId ?? "")}" class="${chat.selected ? "active" : ""}" title="${escapeHtml(chat.title)}">${escapeHtml(chat.title)}${chat.deletePending ? " · 削除処理中" : ""}</button>${hub.conversations ? `<button class="tiny-button icon-only" data-action="shared-start-rename-conversation" data-value="${escapeHtml(chat.conversationId)}" title="チャット名を変更" aria-label="${escapeHtml(chat.title)}の名前を変更" ${chat.deletePending || chat.canRename === false ? "disabled" : ""}>${icon("edit")}</button><button class="tiny-button icon-only" data-action="shared-request-delete-conversation" data-value="${escapeHtml(chat.conversationId)}" title="共有チャットを削除" aria-label="${escapeHtml(chat.title)}を削除" ${chat.deletePending || chat.canDelete === false ? "disabled" : ""}>${icon("x")}</button>` : ""}</div>`).join("")}${hub.status?.next_before && !hub.conversations ? '<button data-action="shared-next-jobs">以前のチャット</button>' : ""}<button data-action="shared-new-conversation">＋ 新しいチャット</button></div>` : ""}</div>`;
        }).join("") : hub?.hub_url ? `<button class="rail-item" data-action="show-shared-work">${hub.connected ? "Hubのプロジェクトを確認" : "Hubへの参加状況"}</button>` : ""}
        ${state.project_rows
          .map((row, index) => renderProjectRowWithSessions(localState, row, index))
          .join("")}
      </div>
      <div class="rail-section row-heading">
        <span class="section-label">このPCのチャット</span>
        <button class="tiny-button icon-only" data-action="new-chat" data-value="local" data-focus-key="quick-chat:new-session" title="このPCで新しいチャット" aria-label="このPCで新しいチャット" ${navigationDisabled ? "disabled" : ""}>${icon("plus")}</button>
      </div>
      <div class="row-list chat-list">${renderChatRows(localState)}</div>
      <button class="settings" data-action="show-config" title="設定"><span class="rail-icon">${icon("settings")}</span><span>設定</span></button>
    </aside>
  `;
}

export function renderTopbar(
  state: DesktopViewState,
  local: Readonly<DesktopRenderLocalPresentation> = DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION,
): string {
  const workspaceLabel =
    state.selected_project_index >= 0 ? selectedProjectDisplayLabel(state) : "プロジェクトなし";
  const projectContextAction = state.selected_project_index >= 0 ? "open-workspace-folder" : "create-project-from-picker";
  const exportDisabled = !state.history_export_enabled || !navigationIsIdle(state);
  const exportTitle = exportDisabled ? "保存できる表示中の履歴がありません" : "表示中の履歴をMarkdown保存";
  const sessionSettingsAvailable = state.session_settings?.available === true
    && state.session_settings.target !== null;
  const hubRoute = hubExecutionRoute(state.hub, "main");
  const statusMessage = state.status_code === "user_stopped"
    ? "実行を停止しました。"
    : state.status_message;
  const modelSettingsAction = hubRoute ? "show-config" : sessionSettingsAvailable ? "show-session-settings" : "show-provider";
  const accessSettingsAction = sessionSettingsAvailable ? "show-session-settings" : "toggle-access";
  const accessSettingsEnabled = sessionSettingsAvailable
    || state.config_draft.access_mode_mutation_enabled;
  return `
    <header class="topbar">
      <div class="title-row">
        <div class="title-copy">
          <h1>${escapeHtml(state.selected_session_index < 0 ? "新しいチャット" : state.selected_session_title)}</h1>
          <div class="status-line ${state.status_detail.trim().length > 0 ? "has-detail" : ""}">
            <span>${escapeHtml(statusMessage)}</span>
            ${
              state.status_detail.trim().length > 0
                ? `<details data-details-key="status-detail">
                    <summary>詳細</summary>
                    <pre>${escapeHtml(state.status_detail)}</pre>
                  </details>`
                : ""
            }
          </div>
          ${state.status_code === "user_stopped" && hubRoute ? '<p class="hub-stop-explanation">Hubの実行枠は中継接続の終了時に解放されます。</p>' : ""}
        </div>
        <div class="chips">
          <button data-action="${projectContextAction}" title="${escapeHtml(state.workspace_path)}">${escapeHtml(workspaceLabel)}</button>
          <button data-action="${modelSettingsAction}" ${sessionSettingsAvailable && !hubRoute ? 'data-session-settings-trigger="model"' : ""} title="${escapeHtml(hubRoute ? "Hubの送信先とモデル選択を確認" : sessionSettingsAvailable ? "このチャットの接続先とモデル" : state.provider_label)}">
            <span>${escapeHtml(hubRoute?.modelLabel ?? state.model_label)}</span><small>${escapeHtml(hubRoute?.endpointLabel ?? state.provider_label)}</small>
          </button>
          <button data-action="${accessSettingsAction}" ${sessionSettingsAvailable ? 'data-session-settings-trigger="access"' : ""} title="${sessionSettingsAvailable ? "このチャットの承認方法" : "権限モードを切り替え（承認を求める → 代理で承認 → フルアクセス）"}" aria-disabled="${String(!accessSettingsEnabled)}" ${accessSettingsEnabled ? "" : "disabled"}>${escapeHtml(displayAccessLabel(state.access_label))}</button>
          <button class="icon-button" data-action="export-transcript" title="${exportTitle}" aria-label="${exportTitle}" ${exportDisabled ? "disabled" : ""}>${icon("download")}</button>
          <button class="icon-button responsive-output-toggle" data-action="toggle-artifact-pane" data-focus-key="artifact-pane-toggle" title="${local.artifactPane.collapsed ? "右ペインを表示" : "右ペインを閉じる"}" aria-label="${local.artifactPane.collapsed ? "右ペインを表示" : "右ペインを閉じる"}" aria-expanded="${local.artifactPane.collapsed ? "false" : "true"}">${icon("folder")}</button>
        </div>
      </div>
    </header>
  `;
}

export function renderRunStatusStrip(state: DesktopWebState): string {
  const activityBadge = renderTaskActivityBadge(state.task_activity_state);
  const mcpActivity = renderMcpActivityStrip(state.mcp_activity);
  if (!activityBadge) return mcpActivity;
  const canCancel = runCanBeCancelled(state);
  const step = hubExecutionRoute(state.hub, "main")?.phaseLabel || state.run_active_step.trim() || state.status_message;
  const toolLine = state.latest_tool_summary.trim() || "ツール待機中";
  return `
    <section class="run-strip${canCancel ? " has-stop" : ""}">
      ${activityBadge}
      <span>${escapeHtml(step)}</span>
      <small>${escapeHtml(toolLine)}</small>
      ${canCancel ? `<button class="run-stop-button danger" data-action="cancel-run" title="メインチャットの実行を停止" aria-label="メインチャットを停止">${icon("square")}<span>メインチャットを停止</span></button>` : ""}
    </section>
    ${mcpActivity}
  `;
}

export function renderThreadContent(
  state: DesktopWebState,
  local: Readonly<DesktopRenderLocalPresentation> = DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION,
): string {
  const selectedTranscriptAgentPath = local.artifactPane.mode === "agents"
    ? local.artifactPane.selectedAgentPath
    : null;
  const representedAgentPaths = new Set(
    state.transcript_rows
      .filter((row) => row.row_kind.startsWith("sub_agent_"))
      .map((row) => row.title.trim()),
  );
  if (state.transcript_rows.some((row) => row.row_kind.startsWith("work_summary"))) {
    state.current_turn_agent_activity_rows.forEach((row) => representedAgentPaths.add(row.agent_path));
  }
  const unmatchedAgentRows = state.agent_activity_rows.filter(
    (row) => !representedAgentPaths.has(row.agent_path),
  );
  const currentAgentPaths = new Set(
    state.current_turn_agent_activity_rows.map((row) => row.agent_path),
  );
  const agentActivity = unmatchedAgentRows.length > 0
    ? renderInlineAgentActivity(
      {
        ...state,
        agent_activity_rows: unmatchedAgentRows,
        agent_tree_active: state.agent_tree_active
          && unmatchedAgentRows.some((row) => currentAgentPaths.has(row.agent_path)),
      },
      selectedTranscriptAgentPath,
    )
    : "";
  const pendingInputs = visiblePendingTurnInputs(state);
  const pending = renderPendingTurnInputs(pendingInputs);
  const revisionSource = latestLocalRevisionSource(state);
  if (
    (state.thread_empty || state.selected_session_index < 0)
    && state.file_change_rows.length === 0
    && pendingInputs.length === 0
  ) {
    return `${renderEmptyThread(state)}${agentActivity}`;
  }
  const earlier = renderEarlierHistoryTrigger(
    state.turn_page_offset,
    !state.turn_page_admission_open || turnPageLoadPending(state),
  );
  const transcript = state.thread_empty
    ? ""
    : renderTranscriptRows(state.transcript_rows, {
      includeRail: true,
      agentActivityRows: state.agent_activity_rows,
      currentTurnAgentActivityRows: state.current_turn_agent_activity_rows,
      includeLiveAgentFallback: state.current_turn_agent_activity_rows.length > 0,
      selectedAgentPath: selectedTranscriptAgentPath,
      // Keep the newest response marker stable across streaming and the terminal
      // projection that seals the same response.
      stableLatestAssistant: true,
      sideChatQuoteOwnerSessionId: sideChatOwnerSessionId(state),
      editableUserHistoryId: revisionSource && localRevisionActionEnabled(state, local.localMessageEdit.pending, revisionSource.historyItemId)
        ? revisionSource.historyItemId : null,
    });
  return `${local.localMessageEdit.error ? `<p class="local-edit-feedback" role="status">${escapeHtml(local.localMessageEdit.error)}</p>` : ""}${earlier}${agentActivity}${transcript}${pending}`;
}

export function visiblePendingTurnInputs(
  state: Pick<DesktopWebState, "pending_turn_inputs" | "transcript_rows">,
): PendingTurnInput[] {
  const delivered = new Set(
    state.transcript_rows
      .map((row) => row.stable_history_identity?.trim())
      .filter((identity): identity is string => Boolean(identity)),
  );
  return state.pending_turn_inputs.filter((input) => !delivered.has(input.id));
}

export function renderPendingTurnInputs(inputs: readonly PendingTurnInput[]): string {
  if (inputs.length === 0) return "";
  return `
    <section class="pending-turn-inputs" aria-label="モデルへの送信待ち" aria-live="polite">
      <div class="pending-turn-inputs-heading">
        <strong>モデルへの送信待ち</strong>
        <small>現在の応答が終わると、順番に引き渡されます</small>
      </div>
      ${inputs.map((input) => `
        <article class="message user pending-turn-input"
          data-pending-input-id="${escapeHtml(input.id)}"
          data-history-identity="${escapeHtml(input.id)}"
          data-turn-id="${escapeHtml(input.turn_id)}">
          <div class="message-body">
            <div class="markdown-body">${renderMarkdown(input.text)}</div>
            ${input.image_count > 0
              ? `<small class="pending-turn-input-images">添付画像 ${input.image_count}件</small>`
              : ""}
          </div>
        </article>
      `).join("")}
    </section>
  `;
}

function renderEmptyThread(state: DesktopWebState): string {
  return `
    <div class="empty-thread">
      <span class="empty-eyebrow">moyAI Desktop <b>LYNX</b></span>
      <h2>${state.selected_project_index >= 0 ? "このプロジェクトで何を作りますか？" : "何に取り組みますか？"}</h2>
      <p>相談、調査、コードの作成。<br>下の入力欄から、やりたいことを伝えてください。</p>
      ${state.selected_project_index >= 0 ? `<div class="empty-status"><span>${escapeHtml(selectedProjectDisplayLabel(state))}</span></div>` : ""}
      ${state.model_label.trim().length === 0 ? '<p class="empty-setup-hint">上のモデル欄で接続先を設定できます。</p>' : ""}
    </div>
  `;
}

export function composerSendTitle(
  state: Pick<DesktopWebState, "composer_submit_mode" | "navigation_loading" | "busy">,
  prompt: string,
): string {
  if (state.composer_submit_mode !== "blocked") {
    if (prompt.trim().length === 0) return "依頼文を入力してください";
    return state.composer_submit_mode === "steer"
      ? "実行中のタスクへ追加指示を送信"
      : "送信";
  }
  return state.navigation_loading
      ? "画面の切り替え完了後に送信できます"
      : state.busy
        ? "実行中は送信できません"
        : prompt.trim().length === 0
          ? "依頼文を入力してください"
          : "現在は送信できません";
}

export function renderComposer(
  state: DesktopWebState,
  local: Readonly<DesktopRenderLocalPresentation> = DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION,
): string {
  const projectContextAction = state.selected_project_index >= 0 ? "open-workspace-folder" : "create-project-from-picker";
  const receiverBlocked = receiverBlocksLocalSend(local.deviceNetwork);
  const receiverUnknown = receiverBlocked && Boolean(local.deviceNetwork.receiverActivity?.unavailable);
  const sendTitle = receiverBlocked
    ? receiverUnknown ? "このPCの実行状態を確認できません" : "このPCの受信作業が終了してから送信できます"
    : composerSendTitle(state, state.draft_prompt);
  const hubRoute = hubExecutionRoute(state.hub, "main");
  const enhanceTitle = state.navigation_loading
    ? "画面の切り替え後に依頼文を整えられます"
    : state.busy
      ? "依頼文を整える操作は、実行が終わってから使えます"
      : state.draft_prompt.trim().length === 0
        ? "依頼文を入力してください"
        : hubRoute && !state.enhance_enabled
          ? "Hubの接続・モデル確認と、実行中の処理を確認してください"
          : hubRoute ? "メインチャットで選んだHubモデルを使って、依頼文を整えます" : "依頼文を整える";
  const controlsVisible = local.attachmentTrayOpen || state.image_input.trim().length > 0;
  const trayVisible = controlsVisible || state.attached_images.length > 0;
  const goalHint = goalSlashCommandHint(state.draft_prompt);
  return `
    <section class="composer ${goalHint ? "goal-command" : ""}" data-run-target="${escapeHtml(JSON.stringify(state.run_target))}">
      ${trayVisible ? renderAttachmentTray(state, controlsVisible) : ""}
      ${renderConversationInput({ id: "prompt", value: state.draft_prompt, label: "moyAIへの依頼", placeholder: "moyAI に依頼する", attributes: `aria-describedby="goal-command-hint" ${state.navigation_loading ? "disabled" : ""}` })}
      <div class="goal-command-hint" id="goal-command-hint" ${goalHint ? "" : "hidden"}>
        <span class="goal-command-badge">/goal</span>
        <span data-goal-command-help>${escapeHtml(goalHint ?? "")}</span>
      </div>
      <div class="composer-actions">
        <button class="add-button icon-only" data-action="toggle-attachment-tray" title="画像添付" aria-label="画像添付" ${state.image_input_enabled ? "" : "disabled"}>${icon("plus")}</button>
        <button class="icon-only" data-action="show-command-palette" title="検索 / コマンド" aria-label="検索 / コマンド">${icon("more")}</button>
        <button class="composer-text-action" data-action="enhance-prompt" title="${enhanceTitle}" aria-labelledby="enhance-prompt-label" ${state.enhance_enabled ? "" : "disabled"}>${icon("sparkles")}<span id="enhance-prompt-label">依頼を整える</span></button>
        <button class="send composer-text-action" data-action="send" title="${sendTitle}" aria-label="${sendTitle}" ${state.can_submit && !receiverBlocked ? "" : "disabled"}><span>送信</span>${icon("send")}</button>
      </div>
      <div class="composer-meta">
          <button data-action="${projectContextAction}" title="${escapeHtml(state.workspace_path)}">${state.selected_project_index >= 0 ? "プロジェクトで作業" : "プロジェクトを選択"}</button>
        ${renderTokenMeter(state)}
        ${renderSessionUsage(state)}
      </div>
      ${hubRoute?.blockedReason ? `<p class="composer-route-notice" role="status">${escapeHtml(hubRoute.blockedReason)} <button data-action="show-hub">Hub設定を開く</button></p>` : ""}
      ${receiverBlocked ? `<p class="composer-route-notice" role="status">${receiverUnknown ? "このPCの実行状態を確認できません。実行機能と接続を確認してください。" : "このPCに受信した仕事またはアプリが残っています。停止が確認されると送信できます。"}入力は保持されます。</p>` : ""}
    </section>
  `;
}

function renderTokenMeter(state: DesktopWebState): string {
  const label = state.token_meter_label.trim();
  if (label.length === 0) {
    return "";
  }
  const level = state.token_meter_level.trim() || "unknown";
  return `
    <span class="token-meter ${escapeHtml(level)}" title="${escapeHtml(state.token_meter_title)}" aria-label="${escapeHtml(state.token_meter_title)}">
      <span class="token-meter-dot"></span>
      <span>${escapeHtml(label)}</span>
    </span>
  `;
}

function renderAttachmentTray(state: DesktopWebState, controlsVisible: boolean): string {
  return `
    <div class="attachment-tray ${controlsVisible ? "expanded" : "compact"}">
      <div class="attachment-row">
        ${renderAttachedImages(state)}
      </div>
      ${
        controlsVisible
          ? `<div class="attachment-controls">
              <input id="image-input" value="${escapeHtml(state.image_input)}" placeholder="画像ファイルのパス" ${state.image_input_enabled ? "" : "disabled"} />
              <button class="icon-only" data-action="set-image" title="画像を添付" aria-label="画像を添付" ${state.image_input_enabled ? "" : "disabled"}>${icon("upload")}</button>
              <button class="icon-only" data-action="browse-image" title="画像を参照" aria-label="画像を参照" ${state.image_input_enabled ? "" : "disabled"}>${icon("folder")}</button>
              <button class="icon-only" data-action="clear-images" title="添付を解除" aria-label="添付を解除" ${state.attached_images.length > 0 ? "" : "disabled"}>${icon("x")}</button>
            </div>`
          : ""
      }
    </div>
  `;
}

function renderAttachedImages(state: DesktopWebState): string {
  if (state.attached_images.length === 0) {
    return '<span class="attachment-empty">画像は未添付です</span>';
  }
  return state.attached_images
    .map((path, index) => {
      const thumbnail = attachmentThumbnailSrc(path);
      return `
        <button class="thumb image-thumb" data-action="remove-image" data-index="${index}" data-focus-key="attachment:${escapeHtml(path)}" title="${escapeHtml(path)}" aria-label="添付画像を削除: ${escapeHtml(fileName(path))}">
          ${
            thumbnail
              ? `<img src="${escapeHtml(thumbnail)}" alt="" loading="lazy" />`
              : `<span class="thumb-fallback">${icon("image")}</span>`
          }
          <span>${escapeHtml(fileName(path))}</span><b>×</b>
        </button>`;
    })
    .join("");
}

function attachmentThumbnailSrc(path: string): string {
  try {
    return convertFileSrc(path);
  } catch {
    return "";
  }
}

function planStepStatusLabel(status: "pending" | "in_progress" | "completed"): string {
  if (status === "completed") return "完了";
  if (status === "in_progress") return "進行中";
  return "未着手";
}

export function renderPlanProjection(state: DesktopWebState): string {
  const plan = state.plan;
  if (!plan || (plan.steps.length === 0 && !(plan.explanation ?? "").trim())) return "";
  return `
    <section class="output-file-section output-plan-section" aria-labelledby="output-plan-heading">
      <div class="output-section-heading">
        <h3 id="output-plan-heading">計画</h3>
        <span class="output-section-count">${plan.steps.length}件</span>
      </div>
      ${plan.explanation?.trim() ? `<p class="plan-explanation">${escapeHtml(plan.explanation.trim())}</p>` : ""}
      <ol class="plan-list">
        ${plan.steps
          .map(
            (step) => `<li data-plan-status="${step.status}"><span class="plan-step-row"><span class="plan-step-status">${escapeHtml(planStepStatusLabel(step.status))}</span><span class="plan-step-copy">${escapeHtml(step.step)}</span></span></li>`,
          )
          .join("")}
      </ol>
    </section>
  `;
}

export function renderArtifactPane(
  state: DesktopWebState,
  local: Readonly<DesktopRenderLocalPresentation> = DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION,
): string {
  if (local.artifactPane.collapsed) {
    return `
      <aside class="artifact-pane collapsed">
        <button class="pin" data-action="toggle-artifact-pane" title="出力を表示" aria-label="出力を表示">${icon("folder")}</button>
      </aside>
    `;
  }
  if (local.artifactPane.mode === "side_chat") {
    return renderSideChatPane(state, local);
  }
  if (local.artifactPane.mode === "agents") {
    const selectedAgent = state.agent_activity_rows.find(
      (row) => row.agent_path === local.artifactPane.selectedAgentPath,
    );
    const visual = selectedAgent ? stableAgentVisual(selectedAgent.agent_path) : null;
    return `
      <aside id="sub-agent-inspector" class="artifact-pane agent-inspector-pane" data-pane-mode="sub-agents" aria-label="サブエージェント履歴">
        <div class="pane-title agent-pane-title">
          <button class="agent-pane-back" data-action="${selectedAgent ? "show-agent-list" : "show-output-pane"}"
            data-focus-key="agent-pane-back" aria-label="${selectedAgent ? "サブエージェント一覧に戻る" : "出力パネルに戻る"}">‹ <span>${selectedAgent ? "一覧" : "出力"}</span></button>
          ${selectedAgent && visual
            ? `<span class="agent-pane-identity agent-tone-${visual.tone}"><span class="agent-symbol" aria-hidden="true">${visual.glyph}</span><strong>${escapeHtml(agentDisplayName(selectedAgent))}</strong></span>`
            : "<strong>サブエージェント</strong>"}
          <div class="pane-actions">
            ${renderSideChatTrigger(state)}
            <button class="pin" data-action="toggle-artifact-pane" title="サブエージェントペインを閉じる" aria-label="サブエージェントペインを閉じる">${icon("x")}</button>
          </div>
        </div>
        ${renderAgentInspector(
          state,
          local.artifactPane.selectedAgentPath,
          local.artifactPane.selectedAgentExecution,
        )}
      </aside>
    `;
  }
  const hasPreview = state.artifact_preview_available;
  const artifactNavigationBlocked = !navigationIsIdle(state);
  const artifactFolderDisabled = state.selected_artifact_index < 0
    || state.artifact_rows[state.selected_artifact_index] === undefined
    || artifactNavigationBlocked;
  const artifactFolderDisabledAttrs = artifactFolderDisabled
    ? ` disabled aria-disabled="true" title="${artifactNavigationBlocked ? "画面の切り替え完了後に開けます" : "成果物を選択してください"}"`
    : ' title="アーティファクトのフォルダーを開く"';
  const hasActivity = state.busy && (state.progress_text.trim().length > 0 || state.tool_status_text.trim().length > 0);
  const activityHistoryRoute = renderActivityHistoryRoute(state);
  return `
    <aside class="artifact-pane" data-pane-mode="output" aria-labelledby="output-pane-heading">
      <div class="pane-title">
        <h2 id="output-pane-heading">出力</h2>
        <div class="pane-actions">
          ${renderSideChatTrigger(state)}
          <button class="pin" data-action="toggle-artifact-pane" title="出力ペインを閉じる" aria-label="出力ペインを閉じる">${icon("x")}</button>
          <button class="pin" data-action="open-artifact-folder"${artifactFolderDisabledAttrs} aria-label="アーティファクトのフォルダーを開く">${icon("folder")}</button>
        </div>
      </div>
      <div class="output-scroll" data-focus-key="artifact-pane-content" role="region" aria-label="出力内容" tabindex="0">
        ${renderSubAgentSummaryTrigger(state)}
        ${renderPlanProjection(state)}
        <section class="output-file-section" aria-labelledby="output-files-heading">
          <div class="output-section-heading">
            <h3 id="output-files-heading">ファイル</h3>
            <span class="output-section-count">${state.artifact_rows.length}件</span>
          </div>
          <ul class="artifact-list">
            ${
              state.artifact_rows.length === 0
                ? '<li class="empty artifact-empty">生成ファイル、開いたファイル、変更履歴がここに表示されます</li>'
                : state.artifact_rows
                    .map(
                      (row, index) => `
                        <li>
                          <button class="artifact-row ${index === state.selected_artifact_index ? "selected" : ""}"
                            data-action="artifact" data-index="${index}" data-focus-key="artifact:${escapeHtml(row.path)}"${index === state.selected_artifact_index ? ' aria-current="true"' : ""} title="${escapeHtml(row.path)}" aria-label="${escapeHtml(`${row.label}: ${row.path}`)}" ${artifactNavigationBlocked ? 'disabled aria-disabled="true"' : ""}>
                            <span class="file-icon" aria-hidden="true">▣</span>
                            <span class="artifact-row-copy"><b>${escapeHtml(row.label)}</b><small>${escapeHtml(row.path)}</small></span>
                          </button>
                        </li>`
                    )
                    .join("")
            }
          </ul>
        </section>
        ${
          hasPreview
            ? `<section class="preview output-preview-section" aria-labelledby="output-preview-heading">
                <div class="preview-tabs">
                  <h3 id="output-preview-heading">プレビュー</h3>
                  <button data-action="open-artifact-folder" ${artifactFolderDisabled ? "disabled aria-disabled=\"true\"" : ""}>開く</button>
                </div>
                <pre>${escapeHtml(state.artifact_preview_text)}</pre>
              </section>`
            : ""
        }
        ${
          hasActivity
            ? `<section class="activity output-activity-section" aria-labelledby="output-activity-heading">
                <div class="output-section-heading">
                  <h3 id="output-activity-heading">進捗／ツール</h3>
                </div>
                <div class="output-activity-group">
                  <h4>進捗</h4>
                  <pre>${escapeHtml(state.progress_text)}</pre>
                </div>
                <div class="output-activity-group">
                  <h4>ツール</h4>
                  <pre>${escapeHtml(state.tool_status_text)}</pre>
                </div>
                ${activityHistoryRoute}
              </section>`
            : ""
        }
      </div>
    </aside>
  `;
}

function renderActivityHistoryRoute(state: DesktopWebState): string {
  const anchors = transcriptAnchors(state.transcript_rows ?? [], { stableLatestAssistant: true });
  const target = [...anchors].reverse().find((anchor) => (
    anchor.row.row_kind === "tool"
    || anchor.row.row_kind === "editing"
    || anchor.row.row_kind === "error"
    || anchor.row.row_kind.startsWith("work_summary")
  )) ?? anchors.at(-1);
  const exportDisabled = !state.history_export_enabled || !navigationIsIdle(state);
  return `
    <div class="output-activity-history-route" aria-label="完全な実行履歴への導線">
      <p>この一覧は要確認項目と直近分を表示しています。会話履歴の該当箇所を開いて詳細を確認できます。以前の履歴は会話欄から読み込めます。</p>
      <div>
        ${target ? `<button type="button" data-action="jump-history-anchor" data-history-target="${escapeHtml(target.id)}">会話履歴で詳細を開く</button>` : ""}
        <button type="button" data-action="export-transcript" ${exportDisabled ? 'disabled aria-disabled="true" title="実行完了後にMarkdown保存できます"' : 'title="会話履歴をMarkdown形式で保存"'}>履歴をMarkdown保存</button>
      </div>
    </div>
  `;
}

function renderSessionUsage(state: DesktopWebState): string {
  const label = state.session_usage_label?.trim() ?? "";
  if (label.length === 0) return "";
  const usageState = state.session_usage_state?.trim() || "missing";
  return `
    <span class="session-usage ${escapeHtml(usageState)}" title="${escapeHtml(state.session_usage_title ?? "")}" aria-label="${escapeHtml(state.session_usage_title ?? label)}">
      <span class="session-usage-mark" aria-hidden="true">Σ</span>
      <span>${escapeHtml(label)}</span>
    </span>
  `;
}

function renderSideChatTrigger(state: DesktopWebState): string {
  const hasOwner = state.draft_target.sessionId !== null;
  const unavailable = hasOwner
    ? ""
    : ' disabled aria-disabled="true" title="チャットを選択してから開いてください"';
  return `<button class="compact-button side-chat-trigger" data-action="show-side-chat-pane"${unavailable}>サイドチャット</button>`;
}

function renderSideChatPane(
  state: DesktopWebState,
  local: Readonly<DesktopRenderLocalPresentation>,
): string {
  const side = state.side_chat;
  const hubRoute = hubExecutionRoute(state.hub, "side_chat");
  const ownerSessionId = side.owner_session_id ?? state.draft_target.sessionId;
  const targetAvailable = ownerSessionId !== null && side.chat_id !== null;
  const statusLabel = sideChatStatusLabel(side.status);
  const phase = side.phase.trim();
  const statusDetail = side.deleting
    ? "削除処理中"
    : hubRoute?.phaseLabel ?? (phase && phase !== side.status ? `${statusLabel} · ${phase}` : statusLabel);
  if (!side.configured) {
    return `
      <aside class="artifact-pane side-chat-pane" data-pane-mode="side-chat" aria-labelledby="side-chat-heading" aria-busy="${side.deleting}">
        <div class="pane-title side-chat-pane-title">
          <button class="agent-pane-back" data-action="show-output-pane" aria-label="出力パネルに戻る">‹ <span>出力</span></button>
          <h2 id="side-chat-heading">サイドチャット</h2>
          <button class="pin" data-action="toggle-artifact-pane" title="サイドチャットを隠す" aria-label="サイドチャットを隠す">${icon("x")}</button>
        </div>
        <div class="side-chat-setup" data-focus-key="artifact-pane-content" role="region" aria-label="サイドチャット設定案内" tabindex="0">
          <p>「設定」の「サイドチャット」で、新しいサイドチャットに使う既定値を変更できます。</p>
          <p>「設定」のAIの接続で、サイドチャット用のモデルを確認してください。会話の指示・履歴・下書きは保持します。</p>
          ${hubRoute ? '<p>Hubを使う場合も、サイドチャット用の指示と会話容量を「設定」で指定してください。AIモデルはHub画面で選びます。</p>' : ""}
          ${side.deleting ? renderSideChatDeletePending() : ""}
          ${renderSideChatFeedback(side)}
          <button class="wide-send" data-action="show-config" ${!local.sideChat.operationsOpen || local.sideChat.mutationPending || side.deleting ? "disabled" : ""}>設定を開く</button>
        </div>
      </aside>
    `;
  }

  const canSend = targetAvailable
    && side.can_send
    && local.sideChat.draft.trim().length > 0
    && local.sideChat.operationsOpen
    && !side.deleting
    && !local.sideChat.mutationPending
    && local.sideChat.deleteConfirmation === null;
  const canCancel = targetAvailable
    && side.can_cancel
    && local.sideChat.operationsOpen
    && !side.deleting
    && !local.sideChat.mutationPending
    && local.sideChat.deleteConfirmation === null;
  const canDelete = targetAvailable
    && local.sideChat.operationsOpen
    && !side.deleting
    && !local.sideChat.mutationPending;
  return `
    <aside class="artifact-pane side-chat-pane" data-pane-mode="side-chat" aria-labelledby="side-chat-heading" data-side-chat-owner="${escapeHtml(ownerSessionId ?? "")}" aria-busy="${side.deleting}">
      <div class="pane-title side-chat-pane-title">
        <button class="agent-pane-back" data-action="show-output-pane" aria-label="出力パネルに戻る">‹ <span>出力</span></button>
        <h2 id="side-chat-heading">サイドチャット</h2>
        <div class="pane-actions">
          <button class="pin danger-pin" data-action="request-delete-side-chat" data-focus-key="side-chat-delete-trigger" title="サイドチャットを削除" aria-label="サイドチャットを削除" aria-haspopup="dialog" aria-controls="side-chat-delete-dialog" aria-expanded="${local.sideChat.deleteConfirmation !== null}" ${canDelete ? "" : "disabled"}>${icon("x")}</button>
          <button class="pin" data-action="toggle-artifact-pane" title="サイドチャットを隠す" aria-label="サイドチャットを隠す">${icon("folder")}</button>
        </div>
      </div>
      <div class="side-chat-meta">
        <strong title="${escapeHtml(hubRoute?.modelLabel ?? side.model)}">${escapeHtml(hubRoute?.modelLabel ?? (side.model || "モデル未設定"))}</strong>
        <span class="side-chat-status ${side.deleting ? "side-chat-status-deleting" : `side-chat-status-${escapeHtml(side.status)}`}" role="status">${escapeHtml(statusDetail)}</span>
        <small title="${escapeHtml(hubRoute?.endpointLabel ?? side.base_url)}">${escapeHtml(hubRoute?.endpointLabel ?? side.base_url)}</small>
      </div>
      ${renderSideChatContextMetadata(side)}
      ${renderSideChatFeedback(side)}
      ${side.direct_provider_capture ? `<div class="side-chat-route-notice" role="status"><p>${escapeHtml(side.direct_provider_capture.reason)}</p><p>${escapeHtml(side.direct_provider_capture.model || "モデル未設定")} · ${escapeHtml(side.direct_provider_capture.base_url || "接続先未設定")} · ${escapeHtml(side.direct_provider_capture.provider_profile)}</p><button data-action="capture-side-chat-direct-provider" ${side.direct_provider_capture.enabled && local.sideChat.operationsOpen && !local.sideChat.mutationPending ? "" : "disabled"}>この会話に直接接続の設定を適用</button> <button data-action="show-config">設定を開く</button></div>` : ""}
      ${hubRoute?.blockedReason ? `<p class="side-chat-route-notice" role="status">${escapeHtml(hubRoute.blockedReason)} <button data-action="show-hub">Hub設定を開く</button></p>` : ""}
      ${side.deleting ? renderSideChatDeletePending() : ""}
      <div class="side-chat-scroll" data-focus-key="artifact-pane-content" role="log" aria-label="サイドチャット履歴" tabindex="0">
        ${side.messages.length === 0
          ? '<p class="side-chat-empty">質問を入力すると、ここに会話が表示されます。</p>'
          : side.messages.map(renderSideChatMessage).join("")}
      </div>
      <div class="side-chat-composer">
        ${renderPendingSideChatQuote(local.sideChat.pendingQuote)}
        <label class="sr-only" for="side-chat-prompt">サイドチャットへの質問</label>
        <textarea id="side-chat-prompt" aria-describedby="side-chat-context-description" placeholder="サイドチャットに質問" ${targetAvailable && local.sideChat.operationsOpen && !side.deleting && local.sideChat.deleteConfirmation === null ? "" : "disabled"}>${escapeHtml(local.sideChat.draft)}</textarea>
        <div class="side-chat-composer-actions">
          <small>${side.deleting ? "削除の完了を待っています" : "Ctrl+Enterで送信"}</small>
          ${side.status === "running" && !side.deleting ? `<button class="run-stop-button danger" data-action="cancel-side-chat" ${canCancel ? "" : "disabled"}>${icon("square")}<span>Sideを停止</span></button>` : ""}
          <button class="send" data-action="send-side-chat" title="サイドチャットへ送信" aria-label="サイドチャットへ送信" ${canSend ? "" : "disabled"}>${icon("send")}</button>
        </div>
      </div>
    </aside>
  `;
}

function renderSideChatFeedback(side: DesktopWebState["side_chat"]): string {
  if (!side.last_error.trim()) return "";
  // last_error can also contain a later draft/storage failure, even on a stopped turn.
  if (side.status === "cancelled" && side.last_error === "run stopped by user") {
    return side.deleting ? ""
      : '<p class="side-chat-notice" role="status">サイドチャットの実行を停止しました。</p>';
  }
  return `<p class="side-chat-error" role="alert">${escapeHtml(side.last_error)}</p>`;
}

function renderSideChatContextMetadata(side: DesktopWebState["side_chat"]): string {
  const scope = side.context_scope === "owner_session" ? "このタスクの履歴" : "参照範囲未確定";
  const asOf = side.context_as_of_append_position === null
    ? "履歴位置なし"
    : `履歴位置 ${side.context_as_of_append_position}`;
  return `
    <div class="side-chat-context-meta" id="side-chat-context-description">
      <span>参照: ${escapeHtml(scope)}</span>
      <span>${escapeHtml(asOf)}</span>
      ${side.context_truncated
        ? '<strong class="side-chat-context-truncated">長い履歴の一部を省略</strong>'
        : ""}
    </div>
  `;
}

function renderPendingSideChatQuote(
  quote: Readonly<NonNullable<DesktopRenderLocalPresentation["sideChat"]["pendingQuote"]>> | null,
): string {
  if (!quote) return "";
  const sourceLabel = quote.sourceKind === "artifact" ? "作業結果" : "会話";
  return `
    <section class="side-chat-pending-quote" aria-label="送信時に参照する引用">
      <div><strong>${sourceLabel}から引用</strong><small>履歴位置 ${escapeHtml(quote.sourceAppendPosition ?? "-")}</small></div>
      <blockquote>${escapeHtml(quote.selectedText)}</blockquote>
      <small>下書きを手動で編集すると、引用元との関連付けは解除されます。</small>
    </section>
  `;
}

export function renderSideChatDeleteConfirmation(
  state: DesktopWebState,
  local: Readonly<DesktopRenderLocalPresentation> = DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION,
): string {
  const side = state.side_chat;
  const targetAvailable = sideChatOwnerSessionId(state) !== null && side.chat_id !== null;
  if (local.sideChat.deleteConfirmation === null || !targetAvailable || side.deleting) return "";
  const pending = local.sideChat.mutationPending;
  const controlsDisabled = pending || !local.sideChat.operationsOpen;
  return `
    <div class="modal-backdrop" data-local-modal="side-chat-delete">
      <section id="side-chat-delete-dialog" class="modal confirmation side-chat-delete-confirmation" data-modal role="alertdialog" aria-modal="true" aria-labelledby="side-chat-delete-title" aria-describedby="side-chat-delete-detail" tabindex="-1" ${pending ? 'aria-busy="true"' : ""}>
        <div class="modal-header">
          <h2 id="side-chat-delete-title">サイドチャットを削除しますか？</h2>
        </div>
        <p id="side-chat-delete-detail" class="confirm-summary">${side.status === "running"
          ? "実行中のサイドチャットを停止して、保存された履歴を削除します。"
          : "保存されたサイドチャット履歴を削除します。"} この操作は元に戻せません。メインチャットとワークスペースのファイルは変更しません。</p>
        ${pending ? '<div id="side-chat-delete-status" class="permission-decision-status" role="status" aria-live="polite" tabindex="-1">削除を確定しています…</div>' : ""}
        <div class="modal-actions side-chat-delete-actions">
          <button data-action="cancel-delete-side-chat" autofocus ${controlsDisabled ? "disabled" : ""}>キャンセル</button>
          <button class="danger-button" data-action="confirm-delete-side-chat" ${controlsDisabled ? "disabled" : ""}>${pending ? "削除しています…" : side.status === "running" ? "停止して削除" : "削除"}</button>
        </div>
      </section>
    </div>
  `;
}

function renderSideChatDeletePending(): string {
  return `
    <section class="side-chat-delete-pending" role="status" aria-live="polite">
      <strong>サイドチャットを削除しています</strong>
      <p>実行の停止と保存データの削除を確定しています。完了するとこのサイドチャットは閉じます。</p>
    </section>
  `;
}

function renderSideChatMessage(message: DesktopWebState["side_chat"]["messages"][number]): string {
  const label = message.role === "user" ? "あなた" : message.role === "assistant" ? "サイドチャット" : "エラー";
  const body = message.role === "error"
    ? `<div class="side-chat-message-error">${escapeHtml(message.content)}</div>`
    : `<div class="markdown-body">${renderMarkdown(message.content)}</div>`;
  return `
    <article class="side-chat-message side-chat-message-${escapeHtml(message.role)}" data-side-chat-message-id="${escapeHtml(message.id)}">
      <small>${label}</small>
      ${body}
    </article>
  `;
}

function sideChatStatusLabel(status: DesktopWebState["side_chat"]["status"]): string {
  if (status === "running") return "実行中";
  if (status === "completed") return "完了";
  if (status === "failed") return "失敗";
  if (status === "cancelled") return "停止済み";
  return "待機中";
}

export function renderOverlay(
  state: DesktopViewState,
  local: Readonly<DesktopRenderLocalPresentation> = DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION,
  renderModel: DesktopRenderModel = createDesktopRenderModel(
    state as DesktopViewState,
    local as DesktopRenderLocalPresentation,
  ),
): string {
  if (state.overlay === "provider") return renderProviderOverlay(state, local);
  if (state.overlay === "config") return renderConfigOverlay(state, local);
  if (state.overlay === "hub") return renderHubOverlay(local.hub, local.deviceNetwork, local.sharedWork);
  if (state.overlay === "mcp_history") return renderMcpHistoryOverlay(local.mcpHistory, Boolean(state.mcp_publish?.profiles.length));
  if (state.overlay === "session_settings") return renderSessionSettingsOverlay(state, local);
  if (state.overlay === "workspace") return renderWorkspaceOverlay(state);
  if (state.overlay === "prompt_review") return renderPromptReviewOverlay(state);
  if (state.overlay === "command_palette") return renderCommandPalette(state, renderModel);
  if (state.overlay === "shortcuts") return renderShortcuts();
  if (state.overlay === "about") return renderAboutOverlay(state);
  if (state.overlay === "project_menu") return "";
  if (state.overlay === "file_menu") return renderMenuPopover("file", menuActions("file", renderModel));
  if (state.overlay === "edit_menu") return renderMenuPopover("edit", menuActions("edit", renderModel));
  if (state.overlay === "view_menu") {
    return renderMenuPopover(
      "view",
      menuActions("view", renderModel),
      `
        <div class="menu-slider" data-modal>
          <label class="field-label" for="opacity-input">ウィンドウ透過率</label>
          <input id="opacity-input" type="range" min="50" max="100" value="${state.window_opacity_percent}" aria-valuetext="${state.window_opacity_percent}%" />
        </div>
      `
    );
  }
  if (state.overlay === "help_menu") return renderMenuPopover("help", menuActions("help", renderModel));
  return "";
}

function renderAboutOverlay(state: DesktopViewState): string {
  const about = state.about;
  return `
    <div class="modal-backdrop" data-action="close-overlay">
      <section class="modal side about-modal" data-modal role="dialog" aria-modal="true" aria-labelledby="about-dialog-title" aria-describedby="about-dialog-description" tabindex="-1">
        <div class="modal-header">
          <h2 id="about-dialog-title">${escapeHtml(about.product_name)}について</h2>
          <button class="icon-button" data-action="close-overlay" title="閉じる" aria-label="閉じる">${icon("x")}</button>
        </div>
        <div class="about-content" id="about-dialog-description">
          <img class="about-logo" src="${splashLogoUrl}" alt="" aria-hidden="true" />
          <strong class="about-product">${escapeHtml(about.product_name)}</strong>
          <dl class="about-metadata">
            <div><dt>バージョン</dt><dd>${escapeHtml(about.version)}</dd></div>
            <div><dt>コードネーム</dt><dd>${escapeHtml(about.codename)}</dd></div>
            <div><dt>ライセンス</dt><dd>${escapeHtml(about.license_identifier)}</dd></div>
          </dl>
          <p class="about-copyright">${escapeHtml(about.copyright_notice)}</p>
        </div>
        <div class="modal-actions">
          <button data-action="close-overlay" autofocus>OK</button>
        </div>
      </section>
    </div>
  `;
}

function renderSessionSettingsOverlay(
  state: DesktopViewState,
  local: Readonly<DesktopRenderLocalPresentation>,
): string {
  const projection = state.session_settings;
  const draft = local.sessionSettings.draft;
  const target = projection.target;
  const pending = local.sessionSettings.mutationPending || local.configMutationPending;
  const closeConfirmationPending = local.modal.localConfirmation?.kind === "session_settings_close";
  if (!projection.available || target === null || draft === null) {
    return `
      <div class="modal-backdrop" ${closeConfirmationPending ? 'inert aria-hidden="true"' : ""}>
        <section class="modal settings-modal session-settings-modal" data-modal="session-settings" data-surface="session-settings" role="dialog" aria-modal="true" aria-labelledby="session-settings-dialog-title" tabindex="-1">
          <div class="settings-header">
            <div>
              <h2 id="session-settings-dialog-title">チャットの設定</h2>
              <p>${escapeHtml(projection.unavailable_reason || "メインチャットを開くと設定できます。")}</p>
            </div>
            <button class="icon-button" data-action="close-overlay" title="閉じる" aria-label="閉じる">${icon("x")}</button>
          </div>
          ${renderSettingsRecoverableError(
            local.recoverableError,
            "session-settings-recoverable-error",
            "session-settings-error-notice",
          )}
          <div class="session-settings-unavailable" role="status">${escapeHtml(projection.unavailable_reason || "設定を変更するメインチャットを開いてください。")}</div>
        </section>
      </div>
    `;
  }

  const validation = local.sessionSettings.validation;
  const managed = aiConnectionManaged(local.hub, local.deviceNetwork);
  const staleTarget = local.sessionSettings.availability.staleTarget === true;
  const providerDisabled = managed || pending || staleTarget || !projection.provider_mutation_enabled;
  const contextWindowDisabled = pending || staleTarget || !projection.provider_mutation_enabled;
  const accessDisabled = pending || staleTarget || !projection.access_mutation_enabled;
  const fieldInvalid = (field: keyof NonNullable<typeof validation>["fields"]): boolean =>
    validation?.fields[field].ok === false;
  const providerAvailabilityHelp = managed ? "接続先とモデルは共通設定の「AIの接続」で管理します。"
    : projection.provider_mutation_enabled
    ? "このチャットだけに適用します。共通設定は変わりません。"
    : "実行中に変更できるのは承認方法だけです。接続先・モデル・入力整理上限は、実行が終わってから変更してください。";
  const inheritedHelp = (inherited: boolean, label: string): string => inherited
    ? `共通設定の値を使用中です。数値を入力すると、このチャットだけの${label}を設定できます。`
    : `このチャットだけの${label}です。空欄にして適用すると、共通設定の値を使います。`;
  const statusKind = validation?.ok === false
    ? "error"
    : local.sessionSettings.availability.enabled || !local.sessionSettings.dirty
      ? "ok"
      : "warning";
  return `
    <div class="modal-backdrop" ${closeConfirmationPending ? 'inert aria-hidden="true"' : ""}>
      <section class="modal settings-modal session-settings-modal" data-modal="session-settings" data-surface="session-settings" data-root-session-id="${escapeHtml(target.rootSessionId)}" role="dialog" aria-modal="true" aria-labelledby="session-settings-dialog-title" aria-describedby="session-settings-scope-help session-settings-status" aria-busy="${String(pending)}" tabindex="-1">
        <div class="settings-header session-settings-header">
          <div>
            <div class="session-settings-title-line">
              <h2 id="session-settings-dialog-title">チャットの設定</h2>
              <span class="session-scope-badge" data-session-scope="root-only">このチャットだけ</span>
            </div>
            <p id="session-settings-scope-help">このチャットと、そのサブエージェントに適用します。次の依頼にも同じ設定を使います。</p>
          </div>
          <div class="settings-header-actions">
            <span class="dirty-badge session-settings-dirty ${local.sessionSettings.dirty ? "visible" : ""}" data-settings-passive="session-settings-dirty-badge">未適用</span>
            <button class="icon-button" data-action="close-overlay" title="閉じる" aria-label="閉じる" aria-haspopup="${local.sessionSettings.dirty ? "alertdialog" : "false"}">${icon("x")}</button>
          </div>
        </div>
        ${renderSettingsRecoverableError(
          local.recoverableError,
          "session-settings-recoverable-error",
          "session-settings-error-notice",
        )}
        <div class="session-settings-content">
          <section class="session-settings-group" aria-labelledby="session-settings-provider-title">
            <div class="session-settings-group-heading">
              <div>
                <h3 id="session-settings-provider-title">AIの接続先とモデル</h3>
                <p data-settings-passive="session-provider-availability">${escapeHtml(providerAvailabilityHelp)}</p>
              </div>
              <span class="session-settings-lock" data-settings-passive="session-provider-lock" ${projection.provider_mutation_enabled ? "hidden" : ""}>実行中は固定</span>
            </div>
            ${managed ? `<p>Hubのモデルを使用します。</p><button data-action="open-preferences-from-session-settings">AIの接続を開く</button>` : ""}
            <div class="settings-grid-two">
              <div class="settings-field" ${managed ? "hidden" : ""}>
                <label for="session-settings-base-url">接続先URL</label>
                <input id="session-settings-base-url" class="session-settings-control" data-session-setting="base-url" type="url" value="${escapeHtml(draft.baseUrl)}" autocomplete="off" spellcheck="false" aria-describedby="session-settings-base-url-help session-settings-status" ${fieldInvalid("baseUrl") ? 'aria-invalid="true"' : ""} ${providerDisabled ? "disabled" : ""} />
                <small id="session-settings-base-url-help" class="settings-field-help">このチャットで使うAIの接続先です。</small>
              </div>
              <div class="settings-field" ${managed ? "hidden" : ""}>
                <label for="session-settings-provider-profile">接続方式</label>
                <select id="session-settings-provider-profile" class="session-settings-control" data-session-setting="provider-profile" aria-describedby="session-settings-provider-profile-help session-settings-status" ${fieldInvalid("providerProfile") ? 'aria-invalid="true"' : ""} ${providerDisabled ? "disabled" : ""}>
                  ${Object.entries(PROVIDER_PROFILE_LABELS).map(([value, label]) => `<option value="${escapeHtml(value)}" ${draft.providerProfile === value ? "selected" : ""}>${escapeHtml(label)}</option>`).join("")}
                </select>
                <small id="session-settings-provider-profile-help" class="settings-field-help">モデル一覧と生成APIを一つの接続方式として保存します。</small>
              </div>
              <div class="settings-field" ${managed ? "hidden" : ""}>
                <label for="session-settings-api-key-env">APIキーの環境変数名（任意）</label>
                <input id="session-settings-api-key-env" class="session-settings-control" data-session-setting="api-key-env" value="${escapeHtml(draft.apiKeyEnv)}" autocomplete="off" spellcheck="false" placeholder="OPENAI_API_KEY" aria-describedby="session-settings-api-key-env-help session-settings-status" ${fieldInvalid("apiKeyEnv") ? 'aria-invalid="true"' : ""} ${providerDisabled ? "disabled" : ""} />
                <small id="session-settings-api-key-env-help" class="settings-field-help">秘密値ではなく、moyAI起動時に設定済みの環境変数名を入力します。</small>
              </div>
              <div class="settings-field" ${managed ? "hidden" : ""}>
                <label for="session-settings-model">モデル</label>
                <input id="session-settings-model" class="session-settings-control" data-session-setting="model" value="${escapeHtml(draft.model)}" autocomplete="off" spellcheck="false" aria-describedby="session-settings-model-help session-settings-status" ${fieldInvalid("model") ? 'aria-invalid="true"' : ""} ${providerDisabled ? "disabled" : ""} />
                <small id="session-settings-model-help" class="settings-field-help">このsessionで使用するモデル IDです。</small>
              </div>
              <div class="settings-field">
                <label for="session-settings-context-window">moyAIの入力整理上限 <span class="inherited-badge" data-settings-passive="session-context-inherited-badge" ${projection.context_window_inherited ? "" : "hidden"}>継承中</span></label>
                <input id="session-settings-context-window" class="session-settings-control" data-session-setting="context-window" inputmode="numeric" value="${escapeHtml(draft.contextWindow)}" placeholder="共通設定を継承" aria-describedby="session-settings-context-window-help session-settings-status" ${fieldInvalid("contextWindow") ? 'aria-invalid="true"' : ""} ${contextWindowDisabled ? "disabled" : ""} />
                <small id="session-settings-context-window-help" class="settings-field-help" data-settings-passive="session-context-inherited-help">${escapeHtml(inheritedHelp(projection.context_window_inherited, "moyAI内の入力整理上限"))} AI側の設定値は変更しません。</small>
              </div>
            </div>
          </section>
          <section class="session-settings-group" aria-labelledby="session-settings-access-title">
            <div class="session-settings-group-heading">
              <div>
                <h3 id="session-settings-access-title">承認方法</h3>
                <p>このチャットとサブエージェントの操作を、誰が承認するかを選びます。実行中に変更した場合は、次に承認が必要になる操作から適用します。すでに表示中の承認依頼や開始済みの操作には適用しません。</p>
              </div>
            </div>
            <div class="settings-field session-settings-access-field">
              <label for="session-settings-access-mode">承認方法</label>
              <select id="session-settings-access-mode" class="session-settings-control" data-session-setting="access-mode" aria-describedby="session-settings-access-help session-settings-status" ${fieldInvalid("accessMode") ? 'aria-invalid="true"' : ""} ${accessDisabled ? "disabled" : ""}>
                <option value="default" ${draft.accessMode === "default" ? "selected" : ""}>承認を求める</option>
                <option value="auto_review" ${draft.accessMode === "auto_review" ? "selected" : ""}>代理で承認</option>
                <option value="full_access" ${draft.accessMode === "full_access" ? "selected" : ""}>フルアクセス</option>
              </select>
              <small id="session-settings-access-help" class="settings-field-help">共通設定の既定値は変更しません。</small>
            </div>
          </section>
        </div>
        <div class="session-settings-footer">
          <div id="session-settings-status" class="validation ${statusKind}" data-settings-live-region="session-settings-status" role="status" aria-live="polite">${escapeHtml(pending ? "チャットの設定を適用しています…" : local.sessionSettings.availability.reason)}</div>
          <div class="session-settings-actions">
            <button data-action="open-preferences-from-session-settings">共通設定を開く</button>
            <span class="session-settings-primary-actions">
              <button data-action="discard-session-settings" ${local.sessionSettings.dirty ? "" : "hidden"}>変更を破棄</button>
              <button class="send wide-send" data-action="apply-session-settings">${pending ? "適用しています…" : "このチャットに適用"}</button>
            </span>
          </div>
        </div>
      </section>
    </div>
  `;
}

function renderProviderOverlay(
  state: DesktopViewState,
  local: Readonly<DesktopRenderLocalPresentation>,
): string {
  const selectedSummary = state.provider_selected_model_summary.length > 0 ? state.provider_selected_model_summary : ["モデルの詳細情報はまだ読み込んでいません。"];
  const providerFeedback = providerOverlayFeedback(state.provider_base_url, state.provider_status);
  const setupRequired = startupSetupRequired(state);
  return `
    <div class="modal-backdrop">
      <section class="modal wide ${setupRequired ? "setup-modal" : ""}" data-modal role="dialog" aria-modal="true" aria-labelledby="provider-dialog-title" tabindex="-1">
        <div class="modal-header">
          <h2 id="provider-dialog-title">${setupRequired ? "初期設定" : "AIの接続設定"}</h2>
          ${setupRequired ? "" : `<button class="icon-button" data-action="close-overlay" title="閉じる" aria-label="閉じる">${icon("x")}</button>`}
        </div>
        ${setupRequired ? renderInitialSetupStatus(state, local) : ""}
        <label class="field-label" for="provider-url">接続先URL</label>
        <input id="provider-url" value="${escapeHtml(state.provider_base_url)}" aria-describedby="provider-url-help provider-status" aria-invalid="${!providerFeedback.baseUrl.ok}" />
        <small id="provider-url-help" class="provider-url-help">http:// または https:// で始まる接続先を入力してください。ユーザー名・パスワード、?以降の条件、#以降の位置指定は含められません。</small>
        <label class="field-label" for="provider-profile">接続方式</label>
        <select id="provider-profile" aria-describedby="provider-profile-help">
          ${Object.entries(PROVIDER_PROFILE_LABELS).map(([value, label]) => `<option value="${escapeHtml(value)}" ${state.provider_profile === value ? "selected" : ""}>${escapeHtml(label)}</option>`).join("")}
        </select>
        <small id="provider-profile-help" class="provider-url-help">AIサーバーに合う接続方式を選んでください。oMLXにはOpenAI-compatible (Chat Completions)を選びます。</small>
        <label class="field-label" for="provider-api-key-env">APIキーの環境変数名（任意）</label>
        <input id="provider-api-key-env" value="${escapeHtml(state.provider_api_key_env)}" placeholder="OPENAI_API_KEY" autocomplete="off" spellcheck="false" aria-describedby="provider-api-key-env-help" />
        <small id="provider-api-key-env-help" class="provider-url-help">APIキーそのものではなく、起動環境に設定した環境変数名を入力します。認証不要なら空欄です。</small>
        <div class="provider-limit-grid">
          <div>
            <label class="field-label" for="provider-context-window">moyAIの入力整理上限</label>
            <input id="provider-context-window" inputmode="numeric" value="${escapeHtml(state.provider_context_window)}" />
            <small class="provider-url-help">moyAIが会話を整理する際の上限です。AI側の設定値は変更しません。</small>
          </div>
        </div>
        <div class="split-actions">
          <button data-action="load-provider-models" ${providerCapabilities(state).canLoadProviderModels ? "" : "disabled"}>${state.provider_loading ? "読込中" : "モデル読込"}</button>
          ${setupRequired
            ? `<span class="setup-completion-actions" role="group" aria-label="初期設定の完了方法" aria-describedby="initial-setup-action-help">
                <button class="setup-secondary-action" data-action="apply-provider-session" ${state.provider_apply_enabled ? "" : "disabled"}>設定ファイルに保存せず適用</button>
                <button class="setup-primary-action" data-action="save-provider-global" ${state.provider_apply_enabled ? "" : "disabled"}>設定を保存して開始</button>
                <button class="setup-secondary-action" data-action="import-config-toml" ${state.config_draft.external_owner_mutation_open ? "" : "disabled"}>TOML設定を読み込む</button>
              </span>`
            : `<button data-action="apply-provider-session" ${state.provider_apply_enabled ? "" : "disabled"}>設定ファイルに保存せず適用</button>
               <button data-action="save-provider-global" ${state.provider_apply_enabled ? "" : "disabled"}>設定ファイルに保存</button>`}
        </div>
        <div class="select-list">
          ${state.provider_models
            .map(
              (model, index) => `
                <button class="${index === state.provider_selected_index ? "selected" : ""}" data-action="select-provider-model" data-index="${index}" data-focus-key="provider-model:${escapeHtml(state.provider_model_ids[index] ?? model)}" aria-pressed="${index === state.provider_selected_index}">
                  ${escapeHtml(model)}
                </button>`
            )
            .join("")}
        </div>
        <details class="provider-details" data-details-key="provider-model-details">
          <summary>モデル詳細</summary>
          <div class="provider-summary">
            ${selectedSummary
              .map((line) => {
                const [label, ...rest] = line.split(": ");
                return `<div><span>${escapeHtml(label)}</span><strong>${escapeHtml(rest.join(": ") || line)}</strong></div>`;
              })
              .join("")}
          </div>
        </details>
        ${renderProviderStatus(providerFeedback.status, "provider-status")}
      </section>
    </div>
  `;
}

function renderProviderStatus(status: DesktopWebState["provider_status"], id: string): string {
  return `<div id="${id}" class="provider-status ${status.kind === "success" ? "ok" : status.kind}" data-settings-passive="${id}" data-settings-preserve-focused-region role="status" aria-live="polite">
    <strong data-provider-status-title>${escapeHtml(status.title)}</strong>
    <p data-provider-status-hint>${escapeHtml(status.hint)}</p>
    <details data-details-key="${id}-details" ${status.details.trim().length > 0 ? "" : "hidden"}>
      <summary>技術詳細</summary><pre data-provider-status-details>${escapeHtml(status.details)}</pre>
    </details>
  </div>`;
}

function renderInitialSetupStatus(
  state: DesktopWebState,
  local: Readonly<DesktopRenderLocalPresentation>,
): string {
  const guidance = `<p class="setup-message">${escapeHtml(state.startup.message)} ${escapeHtml(state.startup.detail)}</p>
    <p id="initial-setup-action-help" class="setup-action-help">「設定を保存して開始」で共通の設定ファイルに保存します。「設定ファイルに保存せず適用」では、このファイルは変わりません。どちらの場合も、新しく作るチャットには使用する接続先・モデルなどが保存されます。TOMLのファイル選択をキャンセルしても設定は変わりません。この画面は、保存または適用が完了するまで閉じません。</p>`;
  if (local.configMutationPending) {
    return `<div class="initial-setup-status">${guidance}<div class="validation" role="status" aria-live="polite">設定を確認しています…</div></div>`;
  }
  if (state.status_code !== "config_import_failed") {
    return `<div class="initial-setup-status">${guidance}</div>`;
  }
  return `<div class="initial-setup-status">${guidance}
    <div class="validation error" role="alert" aria-live="assertive">
      <strong>${escapeHtml(state.status_message)}</strong>
      ${state.status_detail.trim().length > 0
        ? `<details><summary>詳細</summary><pre>${escapeHtml(state.status_detail)}</pre></details>`
        : ""}
    </div></div>`;
}

function renderSideChatSettings(
  state: DesktopViewState,
  local: Readonly<DesktopRenderLocalPresentation>,
): string {
  const catalogLoading = local.sideChat.catalog.status === "loading";
  const managed = aiConnectionManaged(local.hub, local.deviceNetwork);
  return `
    <section id="settings-side-chat" class="settings-section" aria-labelledby="settings-side-chat-title" aria-describedby="side-chat-settings-help" aria-busy="${catalogLoading ? "true" : "false"}">
      <div class="settings-section-head">
        <div>
          <h3 id="settings-side-chat-title">サイドチャット</h3>
          <p id="side-chat-settings-help">新しく開くサイドチャットの既定値です。文字のみの会話で、ツールは使用しません。既存の会話の設定は変わりません。</p>
        </div>
        ${managed ? "" : `<button data-action="load-side-chat-models" aria-controls="side-chat-model side-chat-model-catalog-status" aria-disabled="${local.sideChat.catalogLoadEnabled ? "false" : "true"}" ${local.sideChat.catalogLoadEnabled ? "" : "disabled"}>${catalogLoading ? "読込中…" : "モデル読込"}</button>`}
      </div>
      <details class="settings-scope-details" data-details-key="side-chat-settings-scope" ${managed ? "hidden" : ""}>
        <summary id="side-chat-settings-scope-toggle">既存の会話に新しい設定を使うには</summary>
        <p>サイドチャットは、開いた時点のモデルとプロンプトを保持します。新しい設定を適用するには、上の適用または保存を押した後、サイドチャットを削除して開き直します。削除すると、その会話の履歴と下書きも失われます。</p>
      </details>
      ${managed ? renderManagedAiConnection(local.hub, "side_chat", local.deviceNetwork) : ""}
      <div class="settings-grid-two">
        ${managed ? "" : `
        ${renderConfigEnumField(state, "side_chat.provider_profile", "接続方式", PROVIDER_PROFILE_LABELS, { controlId: "side-chat-provider-profile" })}
        ${renderConfigTextField(state, "side_chat.base_url", "接続先URL", "url", "メインチャットとは別の接続先です。", { controlId: "side-chat-base-url" })}
        ${renderSideChatModelField(state, local.sideChat.catalog)}
        `}
        ${renderConfigMultilineField(
          state,
          "side_chat.system_prompt",
          "追加システムプロンプト（任意）",
          `組み込みの指示に追加します。空欄は追加なしです。${USER_CONFIGURED_SYSTEM_PROMPT_MAX_CHARS.toLocaleString("ja-JP")}文字以内。`,
          { controlId: "side-chat-system-prompt" },
        )}
        ${renderConfigTextField(state, "side_chat.context_window", "入力の整理上限（トークン）", "number", "サイドチャットの入力整理に使います。モデル側の設定は変えません。")}
        ${renderConfigTextField(state, "side_chat.request_timeout_ms", "応答の進捗を待つ時間（ms）", "number", "応答開始までと、受信中に進捗が止まった場合の待ち時間です。")}
        ${renderConfigTextField(state, "side_chat.connect_timeout_ms", "接続を待つ時間（ms）", "number", "モデルの接続先への待ち時間です。")}
        ${renderConfigTextField(state, "side_chat.max_retries", "再試行の上限回数", "number")}
      </div>
      ${managed ? "" : `<p id="side-chat-model-catalog-status" class="side-chat-model-catalog-status ${local.sideChat.catalog.status === "error" ? "error" : ""}" data-settings-live-region="side-chat-model-catalog-status" role="status" aria-live="polite">${escapeHtml(sideChatCatalogStatusText(local.sideChat.catalog))}</p>`}
    </section>
  `;
}

export function sideChatCatalogStatusText(catalog: SideChatCatalogView): string {
  switch (catalog.status) {
    case "loading":
      return "モデル一覧を読み込んでいます…";
    case "error":
      return catalog.error || "モデル一覧を読み込めませんでした。";
    case "ready":
      return catalog.source === "main"
        ? `メインチャットで読み込み済みの${catalog.models.length}件から選択できます。`
        : `${catalog.models.length}件のモデルから選択できます。`;
    case "idle":
      return "「モデル読込」で候補を取得できます。一覧にないモデルIDは直接入力できます。";
  }
}

function renderConfigOverlay(
  state: DesktopViewState,
  local: Readonly<DesktopRenderLocalPresentation>,
): string {
  const setupRequired = startupSetupRequired(state);
  const managed = aiConnectionManaged(local.hub, local.deviceNetwork);
  const title = setupRequired ? "初期設定" : "設定";
  const configValidation = validateConfigFieldValues(state.config_fields);
  const configCommitState = configCommitControlState(state.config_draft.commit_enabled, configValidation.ok);
  const configCommitAttributes = `${configCommitState.disabled ? "disabled " : ""}aria-disabled="${configCommitState.ariaDisabled}"`;
  const doclingEnabled = configField(state, "docling.enabled")?.field.value.trim().toLowerCase() === "true";
  const doclingDependencyOptions: ConfigFieldRenderOptions = {
    disabled: !doclingEnabled,
    descriptionIds: doclingEnabled ? [] : ["docling-disabled-help"],
  };
  const settingsClosePending = local.modal.localConfirmation?.kind === "settings_close";
  const validationKind = configValidation.ok ? "ok" : "error";
  const validationText = configValidation.ok
    ? state.config_draft.dirty
      ? "未保存の設定があります。適用、保存、または変更を破棄してから別画面の設定を変更できます。"
      : "入力形式は問題ありません。"
    : `${configValidation.invalidKey}: ${configValidation.message}`;
  return `
    <div class="modal-backdrop" ${settingsClosePending ? "inert aria-hidden=\"true\"" : ""}>
      <section class="modal settings-modal ${setupRequired ? "setup-modal" : ""}" data-modal role="dialog" aria-modal="true" aria-labelledby="config-dialog-title" aria-busy="${String(local.configMutationPending)}" tabindex="-1">
        <div class="settings-header">
          <div>
            <h2 id="config-dialog-title">${escapeHtml(title)}</h2>
            <p>${setupRequired ? "起動に必要な設定を確認します。" : "共通の設定ファイルに保存するか、保存せずに適用できます。新しく作るチャットには、使用する接続先・モデルなどが保存されます。"}</p>
          </div>
          <div class="settings-header-actions">
            <span class="dirty-badge ${state.config_draft.dirty ? "visible" : ""}">変更あり</span>
            <button data-action="discard-config-draft" ${state.config_draft.dirty ? "" : "hidden"} ${state.config_draft.discard_enabled ? "" : "disabled"}>変更を破棄</button>
            ${setupRequired
              ? `<span class="setup-completion-actions" role="group" aria-label="初期設定の完了方法" aria-describedby="initial-setup-action-help">
                  <button class="setup-secondary-action" data-action="apply-session-config" ${configCommitAttributes}>設定ファイルに保存せず適用</button>
                  <button class="setup-primary-action" data-action="save-global-config" ${configCommitAttributes}>設定を保存して開始</button>
                  <button class="setup-secondary-action" data-action="import-config-toml" ${state.config_draft.external_owner_mutation_open ? "" : "disabled"}>TOML設定を読み込む</button>
                </span>`
              : `<button data-action="apply-session-config" ${configCommitAttributes}>設定ファイルに保存せず適用</button>
                 <button data-action="save-global-config" ${configCommitAttributes}>設定ファイルに保存</button>
                 <button class="icon-button" data-action="close-overlay" title="閉じる" aria-label="閉じる">${icon("x")}</button>`}
          </div>
        </div>
        <div class="settings-status-stack">
          ${setupRequired ? renderInitialSetupStatus(state, local) : ""}
          <div id="settings-validation" class="validation ${validationKind}" role="status" aria-live="polite">${escapeHtml(validationText)}</div>
        </div>
        <div class="settings-layout">
          <nav class="settings-nav" aria-label="設定カテゴリ">
            <span class="settings-nav-group" role="heading" aria-level="3">共通設定</span>
            <a href="#settings-provider">AIの接続・メイン</a>
            <a class="settings-nav-subitem" href="#settings-model">入力の上限・モデル機能</a>
            <a href="#settings-side-chat">サイドチャット</a>
            <a href="#settings-permissions">権限</a>
            <a href="#settings-agents">エージェント</a>
            <a href="#settings-tools">ツール</a>
            <a href="#settings-files">ファイル</a>
            <a href="#settings-advanced">詳細設定</a>
            <span class="settings-nav-group" role="heading" aria-level="3">チャットごとの設定</span>
            <a href="#settings-session-scope">現在のチャット</a>
            <span class="settings-nav-group" role="heading" aria-level="3">画面設定</span>
            <a href="#settings-desktop">ウィンドウ</a>
            <button data-action="open-global-config-folder">設定フォルダーを開く</button>
            <button data-action="open-user-data-folder">データフォルダーを開く</button>
          </nav>
          <div class="settings-content">
            <section id="settings-provider" class="settings-section" aria-labelledby="settings-provider-title" aria-describedby="main-provider-settings-help" aria-busy="${state.provider_loading ? "true" : "false"}">
              <div class="settings-section-head">
                <div>
                  <h3 id="settings-provider-title">AIの接続・メインチャット</h3>
                  <p id="main-provider-settings-help">メインチャットの共通の既定値です。いまの会話だけを変更する場合は「現在のチャット」を開いてください。</p>
                </div>
                ${managed ? "" : `<button data-action="load-provider-models" aria-controls="main-provider-model main-provider-model-catalog-status" title="入力中の接続先からモデル一覧を取得" ${state.config_draft.edit_enabled && !state.provider_loading ? "" : "disabled"}>モデル読込</button>`}
              </div>
              ${managed ? renderManagedAiConnection(local.hub, "main", local.deviceNetwork) : `
              <div class="settings-grid-two">
                ${renderConfigTextField(state, "model.base_url", "接続先URL", "url", "このURLと接続方式に対応するモデルを使います。")}
                ${renderMainProviderModelField(state)}
              </div>
              ${renderProviderStatus(mainProviderCatalogStatus(state), "main-provider-model-catalog-status")}
              <div class="settings-grid-two">
                ${renderConfigEnumField(state, "model.provider_profile", "接続方式", PROVIDER_PROFILE_LABELS)}
                ${renderConfigTextField(
                  state,
                  "model.api_key_env",
                  "APIキーの環境変数名（任意）",
                  "text",
                  "キーの値ではなく、起動前に設定した環境変数名を入力します（例: OPENAI_API_KEY）。認証不要なら空欄です。",
                )}
              </div>`}
              <div class="settings-grid-two">
                ${renderConfigMultilineField(
                  state,
                  "model.system_prompt",
                  "追加システムプロンプト（任意）",
                  `組み込みの指示に追加します。空欄は追加なしです。${USER_CONFIGURED_SYSTEM_PROMPT_MAX_CHARS.toLocaleString("ja-JP")}文字以内。`,
                )}
              </div>
              ${renderDeviceConnectionReset(local.deviceNetwork)}
              <div id="settings-model" class="settings-subsection" aria-labelledby="settings-model-title" aria-describedby="settings-model-help">
                <h4 id="settings-model-title">入力の上限・モデル機能</h4>
                <p id="settings-model-help">入力の整理と利用する機能を設定します。回答の長さや思考設定はモデルのホスト側で管理します。</p>
                <div class="settings-grid-two">
                  ${renderConfigTextField(
                    state,
                    "model.context_window",
                    "入力の整理上限（トークン）",
                    "number",
                    "moyAI内の入力整理に使います。モデル側の設定は変えません。",
                  )}
                  ${renderConfigTextField(
                    state,
                    "model.request_timeout_ms",
                    "応答の進捗を待つ時間（ms）",
                    "number",
                    "応答開始までと、受信中に進捗が止まった場合の待ち時間です。応答が続いている間の総時間は制限せず、モデル側の設定も変えません。",
                  )}
                </div>
                <div class="settings-toggle-grid">
                  ${renderConfigToggleField(state, "model.supports_tools", "ツール利用")}
                  ${renderConfigToggleField(state, "model.supports_images", "画像入力")}
                  ${renderConfigToggleField(state, "model.parallel_tool_calls", "ツールの並列呼び出し")}
                </div>
              </div>
            </section>
            ${renderSideChatSettings(state, local)}
            <section id="settings-permissions" class="settings-section" aria-labelledby="settings-permissions-title" aria-describedby="settings-permissions-help">
              <div>
                <h3 id="settings-permissions-title">権限</h3>
                <p id="settings-permissions-help">ツール実行時の承認方法を設定します。</p>
              </div>
              ${renderConfigEnumField(state, "permissions.access_mode", "承認の方法", {
                default: "承認を求める",
                auto_review: "代理で承認",
                full_access: "フルアクセス",
              })}
            </section>
            <section id="settings-agents" class="settings-section" aria-labelledby="settings-agents-title" aria-describedby="settings-agents-help">
              <div class="settings-section-head">
                <div>
                  <h3 id="settings-agents-title">エージェント</h3>
                  <p id="settings-agents-help">サブエージェントと同じ作業場所を共有します。変更は次の実行から有効です。</p>
                </div>
                ${renderConfigToggleField(state, "multi_agent.enabled", "サブエージェントを使う")}
              </div>
              ${renderConfigEnumField(state, "multi_agent.mode", "起動モード", {
                explicit_request_only: "明示依頼時のみ",
                proactive: "必要に応じて自動",
              })}
              <div class="settings-grid-two">
                ${renderConfigTextField(state, "multi_agent.max_concurrent_agents", "同時エージェント数（メインを含む）", "number")}
                ${renderConfigTextField(state, "multi_agent.max_concurrent_model_requests", "モデルへの同時要求数", "number")}
              </div>
              <p class="settings-hint">ローカルLLMでは同時要求数1を推奨します。推論を順番に行っても、各エージェントの会話と独立したレビューを保てます。</p>
            </section>
            <section id="settings-tools" class="settings-section" aria-labelledby="settings-tools-title" aria-describedby="settings-tools-help">
              <div>
                <h3 id="settings-tools-title">ツール</h3>
                <p id="settings-tools-help">コマンド実行、文書変換、外部ツールの接続を設定します。</p>
              </div>
              <div class="settings-subsection">
                <div class="settings-section-head compact">
                  <div>
                    <h4>コマンド実行</h4>
                    <p>Windowsでコマンドを実行するときの補助ウィンドウの表示を設定します。</p>
                  </div>
                  ${renderConfigToggleField(state, "shell.hide_windows", "補助ウィンドウを隠す")}
                </div>
              </div>
              <div class="settings-subsection">
                <div class="settings-section-head compact">
                  <div>
                    <h4>Docling</h4>
                    <p>PDF・Wordなどの文書を変換します。無効にすると、エージェントはこのツールを使用しません。</p>
                  </div>
                  <div class="settings-tool-actions">
                    ${renderConfigToggleField(state, "docling.enabled", "Docling を有効化")}
                    <button data-action="check-docling-readiness" aria-controls="docling-readiness-status" aria-busy="${String(local.doclingReadinessRequestPending)}" ${local.doclingReadinessRequestPending ? "disabled" : ""}>Doclingへの接続を試す</button>
                  </div>
                </div>
                <p id="docling-disabled-help" class="settings-disabled-help" role="status" aria-live="polite" ${doclingEnabled ? "hidden" : ""}>Doclingがオフのため、接続設定は変更できません。「Docling を有効化」をオンにすると編集できます。</p>
                <div class="settings-docling-dependent" data-docling-dependent aria-disabled="${String(!doclingEnabled)}">
                  <div class="settings-grid-two">
                    ${renderConfigTextField(state, "docling.base_url", "Doclingの接続先URL", "url", "", doclingDependencyOptions)}
                    ${renderConfigTextField(state, "docling.timeout_ms", "待ち時間の上限（ms）", "number", "", doclingDependencyOptions)}
                    ${renderConfigTextField(state, "docling.api_key_env", "APIキーの環境変数名", "text", "", doclingDependencyOptions)}
                  </div>
                  <details class="settings-docling-advanced" data-details-key="settings-docling-advanced">
                    <summary>Doclingの接続ヘッダー（詳細）</summary>
                    ${renderConfigJsonField(state, "docling.headers_json", "接続ヘッダー（JSON）", doclingDependencyOptions)}
                  </details>
                </div>
                ${renderDoclingReadiness(state, local.doclingReadinessRequestPending)}
              </div>
              <div class="settings-subsection">
                <div class="settings-section-head compact">
                  <div>
                    <h4>MCP</h4>
                    <p>ここで登録したHTTP接続のMCPサーバーを使います。</p>
                  </div>
                  ${renderConfigToggleField(state, "mcp.enabled", "有効")}
                </div>
                ${renderConfigJsonField(state, "mcp.servers_json", "MCPサーバー設定（JSON）")}
                <p>moyAI同士の連携はHubのプロジェクトで管理します。以前の個別接続による記録は<button data-action="show-mcp-history">過去の連携履歴</button>から確認できます。</p>
              </div>
            </section>
            <section id="settings-files" class="settings-section" aria-labelledby="settings-files-title" aria-describedby="settings-files-help">
              <div>
                <h3 id="settings-files-title">ファイル</h3>
                <p id="settings-files-help">作業場所の調査範囲と、ファイルの読み取り上限を設定します。</p>
              </div>
              <div class="settings-grid-two">
                ${renderConfigTextField(state, "inspection.default_max_depth", "フォルダーを調べる深さ", "number")}
                ${renderConfigTextField(state, "inspection.default_max_entries_per_dir", "フォルダーごとの項目数", "number")}
                ${renderConfigTextField(state, "inspection.max_extensions_reported", "報告する拡張子の種類数", "number")}
                ${renderConfigTextField(state, "file_guard.max_inline_read_bytes", "直接読み取る上限（バイト）", "number")}
                ${renderConfigTextField(state, "file_guard.large_file_warning_bytes", "大きなファイルの警告基準（バイト）", "number")}
                ${renderConfigTextField(state, "file_guard.blocked_read_extensions", "読み取りを制限する拡張子")}
                ${renderConfigTextField(state, "file_guard.structured_document_extensions", "文書として変換する拡張子")}
              </div>
              ${renderConfigToggleField(state, "inspection.include_hidden_by_default", "隠しファイルも調査する")}
            </section>
            <section id="settings-advanced" class="settings-section" aria-labelledby="settings-advanced-title" aria-describedby="settings-advanced-help">
              <div>
                <h3 id="settings-advanced-title">詳細設定</h3>
                <p id="settings-advanced-help">その他の設定を直接編集します。各項目の入力形式と条件を確認してください。</p>
              </div>
              <details data-details-key="settings-advanced-fields">
                <summary>その他の設定項目を表示</summary>
                <div class="settings-raw-grid">
                  ${state.config_fields
                    .map((field, index) => ({ field, index }))
                    .filter(({ field }) => (
                      !TYPED_CONFIG_KEYS.includes(field.key)
                      && !HOST_OWNED_MODEL_KEYS.has(field.key)
                    ))
                    .map(({ field, index }) => renderRawConfigField(state, field, index))
                    .join("")}
                </div>
              </details>
            </section>
            <section id="settings-session-scope" class="settings-section settings-scope-card" aria-labelledby="settings-session-scope-title">
              <div>
                <h3 id="settings-session-scope-title">現在のチャット</h3>
                <p>いまのメインチャットだけの接続先、モデル、入力上限、権限を変更します。共通設定は変わりません。</p>
              </div>
              <button data-action="show-session-settings" ${state.session_settings.available && !state.config_draft.dirty && !local.configMutationPending ? "" : "disabled"}>このチャットの設定を開く</button>
              ${state.config_draft.dirty ? '<small class="settings-field-help">共通設定の変更を適用、保存、または破棄してから開いてください。</small>' : ""}
            </section>
            <section id="settings-desktop" class="settings-section" aria-labelledby="settings-desktop-title">
              <div>
                <h3 id="settings-desktop-title">画面設定</h3>
                <p>このPCの画面表示を設定します。共通の設定ファイルとは別に保存されます。</p>
              </div>
              <div class="settings-field">
                <label for="opacity-input">ウィンドウ透過率</label>
                <input id="opacity-input" class="desktop-preference-control" type="range" min="50" max="100" value="${state.window_opacity_percent}" aria-valuetext="${state.window_opacity_percent}%" />
                <small class="settings-field-help">再起動しても維持されます。設定ファイルの読み込みでは変更されません。</small>
              </div>
            </section>
          </div>
        </div>
      </section>
    </div>
  `;
}

function configField(state: DesktopWebState, key: string): { field: ConfigFieldProjection; index: number } | null {
  const index = state.config_fields.findIndex((field) => field.key === key);
  if (index < 0) return null;
  return { field: state.config_fields[index], index };
}

function configFieldDomToken(key: string): string {
  return Array.from(key, (character) => character.codePointAt(0)!.toString(16)).join("-");
}

function configFieldControlId(key: string): string {
  return `settings-config-control-${configFieldDomToken(key)}`;
}

function configFieldHelpId(key: string): string {
  return `settings-config-help-${configFieldDomToken(key)}`;
}

function configFieldSectionHelpId(key: string): string {
  if (["model.base_url", "model.model", "model.provider_profile", "model.api_key_env"].includes(key)) {
    return "main-provider-settings-help";
  }
  if (key.startsWith("model.")) return "settings-model-help";
  if (key.startsWith("side_chat.")) return "side-chat-settings-help";
  if (key.startsWith("permissions.")) return "settings-permissions-help";
  if (key.startsWith("multi_agent.")) return "settings-agents-help";
  if (key.startsWith("shell.") || key.startsWith("docling.") || key.startsWith("mcp.")) {
    return "settings-tools-help";
  }
  if (key.startsWith("inspection.") || key.startsWith("file_guard.")) {
    return "settings-files-help";
  }
  return "settings-advanced-help";
}

function configFieldDescriptionIds(
  field: ConfigFieldProjection,
  extraIds: string[] = [],
  includeSectionHelp = true,
): string {
  return [...new Set([
    configFieldHelpId(field.key),
    ...(includeSectionHelp ? [configFieldSectionHelpId(field.key)] : []),
    "settings-validation",
    ...extraIds,
  ])].join(" ");
}

function configFieldHelpText(field: ConfigFieldProjection, explicitHelp = "", includeTechnical = true): string {
  const typeLabel = {
    string: "文字列",
    boolean: "オン / オフ",
    integer: "整数",
    number: "数値",
    json: "JSON",
    enum: "選択式",
  }[field.value_type] ?? field.value_type;
  const parts = [
    explicitHelp.trim(),
    ...(includeTechnical ? [`設定キー: ${field.key}。`, `形式: ${typeLabel}。`] : []),
  ].filter((part) => part.length > 0);
  if (field.min_value !== null && field.max_value !== null) {
    parts.push(`範囲: ${field.min_value}以上${field.max_value}以下。`);
  } else if (field.min_value !== null) {
    parts.push(`範囲: ${field.min_value}以上。`);
  } else if (field.max_value !== null) {
    parts.push(`範囲: ${field.max_value}以下。`);
  }
  if (includeTechnical && field.options.length > 0) parts.push(`選択肢: ${field.options.join(" / ")}。`);
  if (field.required) parts.push("必須入力です。");
  if (includeTechnical && field.env_override) parts.push(`環境変数: ${field.env_override}。`);
  if (field.sensitive) {
    parts.push(field.configured
      ? "機密値は設定済みです。現在値は表示されず、空欄のまま保存すると保持されます。"
      : "機密値は未設定です。入力した値は保存後に再表示されません。");
  }
  return parts.join(" ");
}

function renderConfigFieldHelp(field: ConfigFieldProjection, explicitHelp = "", inlineTechnical = false): string {
  const helpId = configFieldHelpId(field.key);
  const help = configFieldHelpText(field, explicitHelp, inlineTechnical);
  const guidance = `<small id="${helpId}" class="settings-field-help"${help ? "" : " hidden"}>${escapeHtml(help)}</small>`;
  if (inlineTechnical) return guidance;
  return `${guidance}
    <details class="settings-field-technical" data-details-key="${helpId}-technical">
      <summary id="${helpId}-technical-toggle" aria-label="${escapeHtml(field.key)} の技術詳細">技術詳細</summary>
      <p>${escapeHtml(configFieldHelpText(field))}</p>
    </details>`;
}

function sensitiveConfigInputAttributes(field: ConfigFieldProjection): string {
  if (!field.sensitive) return "";
  const placeholder = field.configured ? "設定済み（値は非表示）" : "未設定";
  return ` data-sensitive-config="true" data-sensitive-configured="${String(field.configured)}" placeholder="${placeholder}" autocomplete="off"`;
}

function renderSensitiveConfigStatus(field: ConfigFieldProjection): string {
  if (!field.sensitive) return "";
  return `<small class="settings-sensitive-status ${field.configured ? "configured" : "missing"}">${field.configured ? "設定済み・値は非表示" : "未設定"}</small>`;
}

function configFieldValidationAttribute(
  state: DesktopViewState,
  field: ConfigFieldProjection,
): string {
  const values = state.config_fields.map(({ key, value }) => ({ key, text: value }));
  return validateConfigInput(field, field.value, values).ok ? "" : ' aria-invalid="true"';
}

function renderMissingConfigField(key: string): string {
  return `<div class="settings-field missing"><label>${escapeHtml(key)}</label><small>未対応の設定項目です。</small></div>`;
}

interface ConfigFieldRenderOptions {
  disabled?: boolean;
  descriptionIds?: readonly string[];
  initialSetup?: boolean;
  controlId?: string;
}

function renderConfigTextField(
  state: DesktopViewState,
  key: string,
  label: string,
  type = "text",
  help = "",
  options: ConfigFieldRenderOptions = {},
): string {
  const found = configField(state, key);
  if (!found) return renderMissingConfigField(key);
  const inputMode = type === "number" ? ' inputmode="numeric"' : "";
  const controlId = options.controlId ?? configFieldControlId(found.field.key);
  return `
    <div class="settings-field">
      <label for="${controlId}">${escapeHtml(label)}${options.initialSetup ? renderEnvBadge(found.field) : ""}</label>
      <input id="${controlId}" class="settings-control" data-config-index="${found.index}" data-config-key="${escapeHtml(key)}" type="${type === "number" ? "text" : type}"${inputMode}${sensitiveConfigInputAttributes(found.field)} value="${escapeHtml(found.field.value)}" aria-describedby="${configFieldDescriptionIds(found.field, [...(options.descriptionIds ?? [])], !options.initialSetup)}"${configFieldValidationAttribute(state, found.field)} ${state.config_draft.edit_enabled && !options.disabled ? "" : "disabled"} />
      ${renderSensitiveConfigStatus(found.field)}
      ${renderConfigFieldHelp(found.field, help, options.initialSetup)}
    </div>
  `;
}

function renderMainProviderModelField(state: DesktopViewState): string {
  const found = configField(state, "model.model");
  if (!found) return renderMissingConfigField("model.model");
  const currentModel = found.field.value.trim();
  const catalogAvailable = mainProviderCatalogMatchesSettings(state)
    && state.provider_model_ids.length > 0;
  const options = catalogAvailable
    ? state.provider_model_ids.map((id, index) => ({
      id,
      label: state.provider_models[index] ?? id,
    }))
    : [];
  const currentInCatalog = options.some((option) => option.id === currentModel);
  const controlsEnabled = state.config_draft.edit_enabled;
  const describedBy = configFieldDescriptionIds(found.field, ["main-provider-model-catalog-status"]);
  return `
    <div class="settings-field main-provider-model-field">
      <label for="main-provider-model">モデル</label>
      <select id="main-provider-model" class="settings-control" data-main-provider-model-control data-config-index="${found.index}" data-config-key="model.model" aria-describedby="${describedBy}"${configFieldValidationAttribute(state, found.field)} ${controlsEnabled && options.length > 0 ? "" : "disabled"}>
        ${!currentInCatalog ? '<option value="" selected disabled>モデルを読み込んで選択してください</option>' : ""}
        ${options.map((option) => `<option value="${escapeHtml(option.id)}" ${option.id === currentModel ? "selected" : ""}>${escapeHtml(option.label)}</option>`).join("")}
      </select>
      <details class="side-chat-manual-model main-provider-manual-model" data-details-key="main-provider-manual-model">
        <summary>一覧にないモデルIDを入力</summary>
        <label for="main-provider-model-manual">モデルID</label>
        <input id="main-provider-model-manual" class="settings-control" data-main-provider-model-control data-config-index="${found.index}" data-config-key="model.model" value="${escapeHtml(found.field.value)}" autocomplete="off" spellcheck="false" aria-describedby="${describedBy}"${configFieldValidationAttribute(state, found.field)} ${controlsEnabled ? "" : "disabled"} />
      </details>
      <small data-settings-live-region="main-provider-manual-status">${!currentInCatalog && currentModel ? `候補に含まれない設定中のID: ${escapeHtml(currentModel)}。モデルを選択すると置き換わります。` : ""}</small>
      ${renderConfigFieldHelp(found.field)}
    </div>
  `;
}

function renderSideChatModelField(
  state: DesktopViewState,
  catalog: SideChatCatalogView,
): string {
  const found = configField(state, "side_chat.model");
  if (!found) return renderMissingConfigField("side_chat.model");
  const currentModel = found.field.value.trim();
  const options = sideChatModelOptions(catalog, currentModel);
  const controlsEnabled = state.config_draft.edit_enabled;
  const describedBy = configFieldDescriptionIds(found.field, ["side-chat-model-catalog-status"]);
  const invalid = configFieldValidationAttribute(state, found.field);
  return `
    <div class="settings-field side-chat-model-field">
      <label for="side-chat-model">モデル</label>
      <select id="side-chat-model" class="settings-control" data-side-chat-model-control data-config-index="${found.index}" data-config-key="side_chat.model" aria-describedby="${describedBy}"${invalid} ${controlsEnabled && options.length > 0 ? "" : "disabled"}>
        ${currentModel.length === 0 ? '<option value="" selected disabled>モデルを選択してください</option>' : ""}
        ${options.map((option) => `<option value="${escapeHtml(option.id)}" ${option.id === currentModel ? "selected" : ""}>${escapeHtml(sideChatModelOptionLabel(option))}</option>`).join("")}
      </select>
      <details class="side-chat-manual-model" data-details-key="side-chat-manual-model">
        <summary>一覧にないモデルIDを入力</summary>
        <label for="side-chat-model-manual">モデルID</label>
        <input id="side-chat-model-manual" class="settings-control" data-side-chat-model-control data-config-index="${found.index}" data-config-key="side_chat.model" value="${escapeHtml(found.field.value)}" autocomplete="off" spellcheck="false" aria-describedby="${describedBy}"${invalid} ${controlsEnabled ? "" : "disabled"} />
      </details>
      ${renderConfigFieldHelp(found.field, "候補から選択するか、モデルIDを直接入力できます。")}
    </div>
  `;
}

function mainProviderCatalogMatchesSettings(state: DesktopViewState): boolean {
  const baseUrl = configField(state, "model.base_url")?.field.value ?? "";
  const providerProfile = configField(state, "model.provider_profile")?.field.value ?? "";
  const apiKeyEnv = configField(state, "model.api_key_env")?.field.value.trim() || null;
  return state.provider_catalog_base_url !== null
    && normalizeProviderBaseUrl(baseUrl) === normalizeProviderBaseUrl(state.provider_catalog_base_url)
    && providerProfile === state.provider_catalog_profile
    && apiKeyEnv === state.provider_catalog_api_key_env;
}

function mainProviderCatalogStatus(state: DesktopViewState): DesktopWebState["provider_status"] {
  if (state.provider_loading) return { kind: "loading", title: "メインチャットのモデル一覧を読み込んでいます…", hint: "", details: "" };
  const status = providerOverlayFeedback(state.provider_base_url, state.provider_status).status;
  if (status.kind === "error" || status.kind === "warning") return status;
  if (mainProviderCatalogMatchesSettings(state) && state.provider_model_ids.length > 0) {
    return { kind: "success", title: `${state.provider_model_ids.length}件のメインチャットモデルから選択できます。`, hint: "", details: "" };
  }
  return { kind: "idle", title: "「モデル読込」で入力中のURLと接続方式に対応する候補を取得できます。", hint: "保存済み・手入力のモデルIDは候補に追加しません。", details: "" };
}

function renderDoclingReadiness(
  state: DesktopViewState,
  localRequestPending: boolean,
  options: {
    allowDirtyDraft?: boolean;
    projectedResultVisible?: boolean;
  } = {},
): string {
  const enabled = configField(state, "docling.enabled")?.field.value.trim().toLowerCase() === "true";
  const readiness = state.docling_readiness;
  const dirtyBlocksReadiness = state.config_draft.dirty && !options.allowDirtyDraft;
  const projectedResultVisible = options.projectedResultVisible ?? true;
  const effectiveStatus = localRequestPending
    ? "checking"
    : projectedResultVisible
      ? readiness.status
      : "idle";
  const status = !enabled || dirtyBlocksReadiness ? "idle" : effectiveStatus;
  const title = !enabled
    ? "Docling は無効です"
    : dirtyBlocksReadiness
      ? "未保存の設定があります"
      : effectiveStatus === "checking"
        ? "Docling の接続を確認しています…"
        : effectiveStatus === "ready"
          ? "Docling を利用できます"
          : effectiveStatus === "unavailable"
            ? "Docling に接続できません"
            : "Docling は未確認です";
  const message = !enabled
    ? "有効化して設定を保存すると、明示的に接続確認できます。"
    : dirtyBlocksReadiness
      ? "変更を設定ファイルに保存してから「Doclingへの接続を試す」を押してください。"
      : localRequestPending
        ? "接続確認を開始しています。"
        : projectedResultVisible
          ? readiness.message
          : "入力中の設定では、まだ接続テストを行っていません。";
  const technical = !localRequestPending
    && !dirtyBlocksReadiness
    && projectedResultVisible
    && readiness.endpoint.trim().length > 0
    ? `${readiness.endpoint}${readiness.httpStatus === null ? "" : ` · HTTP ${readiness.httpStatus}`}`
    : "";
  return `
    <div id="docling-readiness-status" class="settings-readiness ${status}" data-settings-live-region="docling-readiness" data-docling-readiness-status="${status}" role="status" aria-live="polite" aria-busy="${String(status === "checking")}">
      <strong>${escapeHtml(title)}</strong>
      <span>${escapeHtml(message)}</span>
      ${technical ? `<small>${escapeHtml(technical)}</small>` : ""}
    </div>
  `;
}

function renderConfigJsonField(
  state: DesktopViewState,
  key: string,
  label: string,
  options: ConfigFieldRenderOptions = {},
): string {
  const found = configField(state, key);
  if (!found) return renderMissingConfigField(key);
  const controlId = configFieldControlId(found.field.key);
  return `
    <div class="settings-field wide">
      <label for="${controlId}">${escapeHtml(label)}${options.initialSetup ? renderEnvBadge(found.field) : ""}</label>
      <textarea id="${controlId}" class="settings-control settings-json" data-config-index="${found.index}" data-config-key="${escapeHtml(key)}"${sensitiveConfigInputAttributes(found.field)} aria-describedby="${configFieldDescriptionIds(found.field, [...(options.descriptionIds ?? [])], !options.initialSetup)}"${configFieldValidationAttribute(state, found.field)} ${state.config_draft.edit_enabled && !options.disabled ? "" : "disabled"}>${escapeHtml(found.field.value)}</textarea>
      ${renderSensitiveConfigStatus(found.field)}
      ${renderConfigFieldHelp(found.field, "", options.initialSetup)}
    </div>
  `;
}

function renderConfigMultilineField(
  state: DesktopViewState,
  key: string,
  label: string,
  help = "",
  options: ConfigFieldRenderOptions = {},
): string {
  const found = configField(state, key);
  if (!found) return renderMissingConfigField(key);
  const controlId = options.controlId ?? configFieldControlId(found.field.key);
  return `
    <div class="settings-field wide">
      <label for="${controlId}">${escapeHtml(label)}${options.initialSetup ? renderEnvBadge(found.field) : ""}</label>
      <textarea id="${controlId}" class="settings-control settings-system-prompt" data-config-index="${found.index}" data-config-key="${escapeHtml(key)}"${sensitiveConfigInputAttributes(found.field)} aria-describedby="${configFieldDescriptionIds(found.field, [...(options.descriptionIds ?? [])], !options.initialSetup)}"${configFieldValidationAttribute(state, found.field)} ${state.config_draft.edit_enabled && !options.disabled ? "" : "disabled"}>${escapeHtml(found.field.value)}</textarea>
      ${renderSensitiveConfigStatus(found.field)}
      ${renderConfigFieldHelp(found.field, help, options.initialSetup)}
    </div>
  `;
}

function renderConfigToggleField(
  state: DesktopViewState,
  key: string,
  label: string,
  options: ConfigFieldRenderOptions = {},
): string {
  const found = configField(state, key);
  if (!found) return renderMissingConfigField(key);
  const checked = found.field.value.trim().toLowerCase() === "true" ? "checked" : "";
  const controlId = options.controlId ?? configFieldControlId(found.field.key);
  return `
    <div class="settings-toggle-field">
      <label class="settings-toggle" for="${controlId}" data-config-key="${escapeHtml(key)}">
        <input id="${controlId}" class="settings-control" data-config-index="${found.index}" data-config-key="${escapeHtml(key)}" type="checkbox" ${checked} aria-describedby="${configFieldDescriptionIds(found.field, [...(options.descriptionIds ?? [])], !options.initialSetup)}"${configFieldValidationAttribute(state, found.field)} ${state.config_draft.edit_enabled && !options.disabled ? "" : "disabled"} />
        <span class="toggle-ui"></span>
        <span>${escapeHtml(label)}${options.initialSetup ? renderEnvBadge(found.field) : ""}</span>
      </label>
      ${renderConfigFieldHelp(found.field, "", options.initialSetup)}
    </div>
  `;
}

function renderConfigEnumField(
  state: DesktopViewState,
  key: string,
  label: string,
  optionLabels: Record<string, string>,
  renderOptions: ConfigFieldRenderOptions = {},
): string {
  const found = configField(state, key);
  if (!found) return renderMissingConfigField(key);
  const options = found.field.options.length > 0 ? found.field.options : [found.field.value];
  const controlId = renderOptions.controlId ?? configFieldControlId(found.field.key);
  return `
    <div class="settings-field wide">
      <label for="${controlId}">${escapeHtml(label)}${renderOptions.initialSetup ? renderEnvBadge(found.field) : ""}</label>
      <select id="${controlId}" class="settings-control" data-config-index="${found.index}" data-config-key="${escapeHtml(key)}" aria-describedby="${configFieldDescriptionIds(found.field, [...(renderOptions.descriptionIds ?? [])], !renderOptions.initialSetup)}"${configFieldValidationAttribute(state, found.field)} ${state.config_draft.edit_enabled && !renderOptions.disabled ? "" : "disabled"}>
        ${options.map((value) => `<option value="${escapeHtml(value)}" ${found.field.value === value ? "selected" : ""}>${escapeHtml(optionLabels[value] ?? value)}</option>`).join("")}
      </select>
      ${renderConfigFieldHelp(found.field, "", renderOptions.initialSetup)}
    </div>
  `;
}

function renderRawConfigField(
  state: DesktopViewState,
  field: ConfigFieldProjection,
  index: number,
): string {
  const controlId = configFieldControlId(field.key);
  return `
    <div class="settings-field raw">
      <label for="${controlId}">${escapeHtml(field.key)}${renderEnvBadge(field)}</label>
      <textarea id="${controlId}" class="settings-control settings-raw-value" data-config-index="${index}" data-config-key="${escapeHtml(field.key)}"${sensitiveConfigInputAttributes(field)} aria-describedby="${configFieldDescriptionIds(field)}"${configFieldValidationAttribute(state, field)} ${state.config_draft.edit_enabled ? "" : "disabled"}>${escapeHtml(field.value)}</textarea>
      ${renderSensitiveConfigStatus(field)}
      ${renderConfigFieldHelp(field, "", true)}
    </div>
  `;
}

function renderEnvBadge(field: ConfigFieldProjection): string {
  if (!field.env_override) return "";
  return ` <small class="env-badge">${escapeHtml(field.env_override)}</small>`;
}

function startupSetupRequired(state: DesktopWebState): boolean {
  return state.startup.initial_setup_required && state.startup.action_overlay === state.overlay;
}

function renderWorkspaceOverlay(state: DesktopWebState): string {
  return `
    <div class="modal-backdrop" data-action="close-overlay">
      <section class="modal wide" data-modal role="dialog" aria-modal="true" aria-labelledby="workspace-dialog-title" tabindex="-1">
        <h2 id="workspace-dialog-title">作業フォルダー</h2>
        <label class="field-label" for="workspace-input">パス</label>
        <input id="workspace-input" value="${escapeHtml(state.workspace_input)}" />
        <pre id="workspace-feedback" class="feedback" role="status">${escapeHtml(state.status_message)}</pre>
        <div class="split-actions">
          <button data-action="switch-workspace">切り替え</button>
          <button data-action="browse-workspace">参照</button>
          <button data-action="open-typed-path">入力パスを開く</button>
          <button data-action="open-workspace-folder">現在の場所を開く</button>
        </div>
      </section>
    </div>
  `;
}

function renderPromptReviewOverlay(state: DesktopWebState): string {
  return `
    <div class="modal-backdrop" data-action="close-overlay">
      <section class="modal wide" data-modal role="dialog" aria-modal="true" aria-labelledby="prompt-review-dialog-title" tabindex="-1">
        <h2 id="prompt-review-dialog-title">依頼文を整える</h2>
        <div class="review-grid">
          <pre>${escapeHtml(state.review_raw_text)}</pre>
          <label class="sr-only" for="review-draft">推敲文</label>
          <textarea id="review-draft">${escapeHtml(state.review_draft_text)}</textarea>
        </div>
        <pre class="feedback">${escapeHtml(state.review_status_text)}</pre>
        <div class="modal-actions">
          <button data-action="cancel-review">キャンセル</button>
          <button data-action="send-review-raw" ${state.send_raw_enabled ? "" : "disabled"}>原文で送信</button>
          <button class="send wide-send" data-action="send-review-enhanced" ${state.send_enhanced_enabled ? "" : "disabled"}>推敲文で送信</button>
        </div>
      </section>
    </div>
  `;
}

function renderCommandPalette(
  state: DesktopViewState,
  renderModel: DesktopRenderModel,
): string {
  const actions = paletteActions(renderModel);
  const query = state.local_search_text.trim().toLowerCase();
  // Match the search projection by name/path while keeping each command's original
  // index for the exact-target insertion command.
  const commands = state.command_rows.map((row, index) => ({ row, index }))
    .filter(({ row }) => !query || row.name.toLowerCase().includes(query) || row.path.toLowerCase().includes(query));
  return `
    <div class="modal-backdrop" data-action="close-overlay">
      <section class="modal command" data-modal role="dialog" aria-modal="true" aria-labelledby="command-palette-dialog-title" tabindex="-1">
        <h2 id="command-palette-dialog-title">コマンドパレット</h2>
        <label class="sr-only" for="local-search">操作、チャット、コマンドを検索</label>
        <input id="local-search" value="${escapeHtml(state.local_search_text)}" placeholder="操作、チャット、/コマンドを検索" />
        <pre class="feedback">${escapeHtml(state.local_search_results_text)}</pre>
        <div class="select-list compact">
          ${
            actions.length === 0 && commands.length === 0
              ? '<div class="empty">実行できる操作はありません</div>'
              : actions
                  .map(
                    (action) => `
                      <button data-action="${escapeHtml(action.id)}" data-focus-key="palette-action:${escapeHtml(action.id)}">
                        <span>${escapeHtml(action.label)}</span>${action.shortcut ? `<small>${escapeHtml(action.shortcut)}</small>` : ""}
                      </button>`
                  )
                  .join("")
          }
          ${commands
            .map(
              ({ row, index }) => `
                <button data-action="insert-command" data-index="${index}" data-focus-key="palette-command:${escapeHtml(row.path)}">
                  <span>/${escapeHtml(row.name)}</span><small>${escapeHtml(row.path)}</small>
                </button>`
            )
            .join("")}
        </div>
      </section>
    </div>
  `;
}

function renderShortcuts(): string {
  const rows: Array<[string, string]> = [["close-overlay", "Esc  閉じる"]];
  rows.push(...shortcutActions().map((action) => [action.id, `${action.shortcut ?? ""}  ${action.label}`] as [string, string]));
  return renderMenuOverlay("ショートカット", rows);
}

function renderMenuPopover(menu: ActionMenu, items: ActionDefinition[], extra = ""): string {
  const label = { file: "ファイル", edit: "編集", view: "表示", help: "ヘルプ" }[menu];
  const menuId = `titlebar-${menu}-menu`;
  const popupRole = titlebarMenuPopupRole(menu);
  const menuItemRole = popupRole === "menu" ? ' role="menuitem"' : "";
  return `
    <div class="menu-scrim" data-action="close-overlay">
      <section id="${menuId}" class="titlebar-popover ${menu}" data-modal data-titlebar-menu="${menu}" role="${popupRole}" aria-label="${label}メニュー" aria-labelledby="${menuId}-trigger">
        ${items
          .map(
            (action, index) => `
              <button data-action="${escapeHtml(action.id)}"${menu === "file" && action.id === "new-chat" ? ' data-focus-key="titlebar-menu:file:new-chat"' : ""} data-titlebar-menu-action${popupRole === "menu" ? ` tabindex="${index === 0 ? "0" : "-1"}"` : ""}${menuItemRole}>
                <span>${escapeHtml(action.label)}</span>
                ${action.shortcut ? `<small>${escapeHtml(action.shortcut)}</small>` : ""}
              </button>`
          )
          .join("")}
        ${extra}
      </section>
    </div>
  `;
}

function renderMenuOverlay(title: string, items: Array<[string, string]>): string {
  return `
    <div class="modal-backdrop" data-action="close-overlay">
      <section class="modal side" data-modal role="dialog" aria-modal="true" aria-labelledby="shortcuts-dialog-title" tabindex="-1">
        <h2 id="shortcuts-dialog-title">${escapeHtml(title)}</h2>
        <div class="select-list">
        ${items.map(([action, label]) => `<button data-action="${action}"${action === "new-chat" ? ' data-focus-key="shortcut-action:new-chat"' : ""}>${escapeHtml(label)}</button>`).join("")}
        </div>
      </section>
    </div>
  `;
}

function renderRecoverableError(
  error: DesktopRenderLocalPresentation["recoverableError"],
): string {
  if (!error) return "";
  return `
    <aside class="ui-error-notice" role="status" aria-live="polite">
      <div>
        <strong>${escapeHtml(error.title)}</strong>
        <span>${escapeHtml(error.hint)}</span>
        ${error.details.trim().length > 0 ? `<details data-details-key="recoverable-error-details"><summary data-focus-key="recoverable-error-summary">技術詳細</summary><pre>${escapeHtml(error.details)}</pre></details>` : ""}
      </div>
      <button class="icon-button" data-action="dismiss-ui-error" title="閉じる" aria-label="閉じる">×</button>
    </aside>`;
}

function renderNavRow(
  label: string,
  detail: string,
  selected: boolean,
  kind: string,
  index: number,
  rejoinAction: string,
  secondaryAction: string,
  rollbackAction: string,
  deleteAction: string,
  taskActivityState: TaskActivityState = "idle",
  mutationDisabled = false,
  focusKey = `${kind}:${index}`,
): string {
  const actionClass = `${rejoinAction ? "has-rejoin" : ""} ${secondaryAction ? "has-archive" : ""} ${rollbackAction ? "has-rollback" : ""}`.trim();
  const rejoinLabel = actionLabel(rejoinAction, "実行中のチャットを開く");
  const secondaryLabel = actionLabel(
    secondaryAction,
    secondaryAction === "interrupt-session"
      ? "実行中のチャットを停止"
      : secondaryAction === "unarchive-session"
        ? "復元"
        : "アーカイブ",
  );
  const secondaryIcon = secondaryAction === "interrupt-session" ? "square" : "archive";
  const rollbackLabel = actionLabel(rollbackAction, "最後の実行前に戻す");
  const deleteLabel = actionLabel(deleteAction, "削除");
  const disabled = mutationDisabled ? ' disabled aria-disabled="true"' : "";
  const activityAttribute = taskActivityState === "idle"
    ? ""
    : ` data-task-activity-row="${taskActivityState}"`;
  return `
    <div class="nav-row-wrap ${actionClass} ${selected ? "selected" : ""}"${activityAttribute}>
      <button class="nav-row" data-action="${kind}" data-index="${index}" data-focus-key="${escapeHtml(focusKey)}:select"${selected ? ' aria-current="page"' : ""}${disabled}>
        <span class="nav-title">${renderTaskActivityIndicator(taskActivityState, { small: !selected, decorative: true })}<span>${escapeHtml(label)}</span></span>
        <small>${escapeHtml(detail)}</small>
      </button>
      ${
        rejoinAction
          ? `<button class="row-action row-rejoin" data-action="${rejoinAction}" data-index="${index}" data-focus-key="${escapeHtml(focusKey)}:${escapeHtml(rejoinAction)}" title="${escapeHtml(rejoinLabel)}" aria-label="${escapeHtml(rejoinLabel)}"${disabled}>${icon("refresh")}</button>`
          : ""
      }
      ${
        secondaryAction
          ? `<button class="row-action ${secondaryAction === "interrupt-session" ? "row-interrupt" : "row-archive"}" data-action="${secondaryAction}" data-index="${index}" data-focus-key="${escapeHtml(focusKey)}:${escapeHtml(secondaryAction)}" title="${escapeHtml(secondaryLabel)}" aria-label="${escapeHtml(secondaryLabel)}"${disabled}>${icon(secondaryIcon)}</button>`
          : ""
      }
      ${
        rollbackAction
          ? `<button class="row-action row-rollback" data-action="${rollbackAction}" data-index="${index}" data-focus-key="${escapeHtml(focusKey)}:${escapeHtml(rollbackAction)}" title="${escapeHtml(rollbackLabel)}" aria-label="${escapeHtml(rollbackLabel)}"${disabled}>${icon("undo")}</button>`
          : ""
      }
      ${deleteAction ? `<button class="row-delete" data-action="${deleteAction}" data-index="${index}" data-focus-key="${escapeHtml(focusKey)}:${escapeHtml(deleteAction)}" title="${escapeHtml(deleteLabel)}" aria-label="${escapeHtml(deleteLabel)}"${disabled}>${icon("x")}</button>` : ""}
    </div>
  `;
}

function actionLabel(actionId: string, fallback: string): string {
  return actionId ? (actionById(actionId)?.label ?? fallback) : fallback;
}



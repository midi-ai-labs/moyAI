import { scenario as shellBaseline } from "./scenarios/shell_baseline.mjs";
import { createShellAboutScenario } from "./scenarios/shell_about.mjs";
import { createShellLynxScenario } from "./scenarios/shell_lynx.mjs";
import { createShellManagedLifecycleScenario } from "./scenarios/shell_managed_lifecycle.mjs";
import { createHubConnectionSettingsScenario } from "./scenarios/hub_connection_settings.mjs";
import { createHubBrowserEnrollmentScenario } from "./scenarios/hub_browser_enrollment.mjs";
import { createHubJoinRetryControlsScenario } from "./scenarios/hub_join_retry_controls.mjs";
import { createOutputHistoryNavigationScenario } from "./scenarios/output_history_navigation.mjs";
import { createMcpReceiverLiveScenario, createMcpReceiverStopScenario, createMcpReceiverPermissionScenario, createMcpHistoryPaginationScenario } from "./scenarios/mcp_receiver_live.mjs";
import { createReviewControlsScenario } from "./scenarios/review_controls.mjs";
import { createHubReceiverSettingsScenario } from "./scenarios/hub_receiver_settings.mjs";
import { createOutgoingControlsScenario } from "./scenarios/hub_outgoing_controls.mjs";
import { createNativeDialogCancelScenario } from "./scenarios/native_dialog_cancel.mjs";
import { createSessionManagementScenario } from "./scenarios/session_management.mjs";
import { createWorkspaceControlsScenario } from "./scenarios/workspace_controls.mjs";
import { createModalKeyboardControlsScenario } from "./scenarios/modal_keyboard_controls.mjs";
import { createRunningNavigationScenario } from "./scenarios/running_navigation_controls.mjs";
import { createMainSteerControlsScenario, createHistoryRailControlsScenario, createMainGoalQueryScenario, createShortcutRowControlsScenario, createPaletteRunControlsScenario } from "./scenarios/main_runtime_controls.mjs";
import { createPointerKeyboardScenario } from "./scenarios/pointer_keyboard.mjs";
import { createCommandPaletteInsertionScenario } from "./scenarios/command_palette_insertion.mjs";
import { createPromptReviewCancelScenario } from "./scenarios/prompt_review_cancel.mjs";
import { createPromptReviewSubmitScenario, createPromptReviewRawInteractionScenario } from "./scenarios/prompt_review_submit.mjs";
import { createProviderConnectionLiveScenario } from "./scenarios/provider_connection_live.mjs";
import { createLmStudioThinkingScenario } from "./scenarios/provider_lm_studio_thinking.mjs";
import {
  createPermissionRestartGuardianChatScenario,
  createPermissionRestartGuardianScenario,
} from "./scenarios/permission_restart_guardian.mjs";
import {
  createPermissionGuardianOpenAiCompatibleScenario,
  createPermissionTempEscalationLmStudioScenario,
  createPermissionTempEscalationScenario,
} from "./scenarios/permission_temp_escalation.mjs";
import { createProviderChatToolContinuationScenario } from "./scenarios/provider_chat_tool_continuation.mjs";
import { createProviderResponsesCompactionRetryScenario } from "./scenarios/provider_responses_compaction_retry.mjs";
import { createProviderResponsesProgressScenario } from "./scenarios/provider_responses_progress.mjs";
import { createProviderRestartScenario } from "./scenarios/provider_restart.mjs";
import { createRunNextTurnScenario } from "./scenarios/run_next_turn.mjs";
import { createRunStopScenario } from "./scenarios/run_stop.mjs";
import { createSideChatQuoteScenario } from "./scenarios/side_chat_quote.mjs";
import { createSideChatSessionScenario } from "./scenarios/side_chat_session.mjs";
import { createSettingsDoclingReadinessScenario } from "./scenarios/settings_docling_readiness.mjs";
import { createSettingsInitialSetupScenario } from "./scenarios/settings_initial_setup.mjs";
import { createInitialSetupHubScenario } from "./scenarios/settings_initial_setup_hub.mjs";
import { createSettingsPreferencesConfigScenario, createSettingsPreferencesScenario } from "./scenarios/settings_preferences.mjs";
import { createSettingsSessionScenario } from "./scenarios/settings_session.mjs";
import { createSettingsMcpPeerControlsScenario } from "./scenarios/settings_mcp_peers.mjs";
import { createMenuEntryControlsScenario, createPaletteEntryControlsScenario } from "./scenarios/shell_entry_controls.mjs";
import { createSettingsFieldControlsScenario, createInitialSettingsFieldControlsScenario, createSessionSettingsFieldControlsScenario } from "./scenarios/settings_field_controls.mjs";
import { createGlobalAdditionalControlsScenario, createInitialAdditionalControlsScenario, createSessionDiscardCloseScenario, createTemporaryApplyControlsScenario } from "./scenarios/settings_additional_controls.mjs";
import { createAgentInterruptScenario } from "./scenarios/agent_interrupt.mjs";
import { createExternalRejoinScenario, createExternalSidebarStopScenario, createExternalPaletteRejoinScenario } from "./scenarios/external_navigation_controls.mjs";
import { createCase52Scenario } from "./scenarios/case5_2.mjs";
import { createHistoryRestartPrependScenario } from "./scenarios/history_restart_prepend.mjs";
import { createHistoryTerminalReconcileScenario } from "./scenarios/history_terminal_reconcile.mjs";

const factories = new Map([
  [shellBaseline.id, () => shellBaseline],
  ["shell.about", createShellAboutScenario],
  ["shell.lynx", createShellLynxScenario],
  ["shell.managed-lifecycle", createShellManagedLifecycleScenario],
  ["hub.connection-settings", createHubConnectionSettingsScenario],
  ["hub.browser-enrollment", createHubBrowserEnrollmentScenario],
  ["hub.join-retry-controls", createHubJoinRetryControlsScenario],
  ["hub.receiver-settings-controls", createHubReceiverSettingsScenario],
  ["hub.outgoing-controls", createOutgoingControlsScenario],
  ["agent.interrupt", createAgentInterruptScenario],
  ["history.restart-prepend", createHistoryRestartPrependScenario],
  ["history.terminal-reconcile", createHistoryTerminalReconcileScenario],
  ["input.pointer-keyboard", createPointerKeyboardScenario],
  ["input.command-palette-insertion", createCommandPaletteInsertionScenario],
  ["mcp.receiver-live", createMcpReceiverLiveScenario],
  ["mcp.receiver-stop", createMcpReceiverStopScenario],
  ["mcp.history-pagination", createMcpHistoryPaginationScenario],
  ["mcp.receiver-approve", options => createMcpReceiverPermissionScenario({ ...options, decision: "approved" })],
  ["mcp.receiver-deny", options => createMcpReceiverPermissionScenario({ ...options, decision: "denied" })],
  ["mcp.receiver-abort", options => createMcpReceiverPermissionScenario({ ...options, decision: "abort" })],
  ["manual.case5_2", createCase52Scenario],
  ["manual.provider-openai-compatible", createProviderConnectionLiveScenario],
  ["manual.provider-lm-studio-thinking", createLmStudioThinkingScenario],
  ["manual.permission-guardian-openai-compatible", createPermissionGuardianOpenAiCompatibleScenario],
  ["manual.permission-temp-escalation-lm-studio", createPermissionTempEscalationLmStudioScenario],
  ["native-dialog.cancel", createNativeDialogCancelScenario],
  ["navigation.session-management", createSessionManagementScenario],
  ["navigation.workspace-controls", createWorkspaceControlsScenario],
  ["navigation.modal-keyboard-controls", createModalKeyboardControlsScenario],
  ["navigation.running-controls", createRunningNavigationScenario],
  ["navigation.external-rejoin", createExternalRejoinScenario],
  ["navigation.external-sidebar-stop", createExternalSidebarStopScenario],
  ["navigation.external-palette-rejoin", createExternalPaletteRejoinScenario],
  ["main.steer-controls", createMainSteerControlsScenario],
  ["history.rail-controls", createHistoryRailControlsScenario],
  ["main.goal-query", createMainGoalQueryScenario],
  ["prompt-review.entries-enhanced", () => createPromptReviewSubmitScenario({ choice: "enhanced", enhanceEntries: "menu-palette" })],
  ["navigation.shortcut-row-controls", createShortcutRowControlsScenario],
  ["main.palette-run-controls", createPaletteRunControlsScenario],
  ["output.history-navigation", createOutputHistoryNavigationScenario],
  ["prompt-review.cancel", createPromptReviewCancelScenario],
  ["prompt-review.raw-interaction", createPromptReviewRawInteractionScenario],
  ["review.uncommitted-controls", createReviewControlsScenario],
  ["prompt-review.submit-raw", () => createPromptReviewSubmitScenario({ choice: "raw" })],
  ["prompt-review.submit-enhanced", () => createPromptReviewSubmitScenario({ choice: "enhanced" })],
  ["permission.restart-guardian", createPermissionRestartGuardianScenario],
  ["permission.restart-guardian-chat", createPermissionRestartGuardianChatScenario],
  ["permission.temp-escalation", createPermissionTempEscalationScenario],
  ["provider.chat-tool-continuation", createProviderChatToolContinuationScenario],
  ["provider.responses-compaction-retry", createProviderResponsesCompactionRetryScenario],
  ["provider.responses-progress", createProviderResponsesProgressScenario],
  ["provider.restart", createProviderRestartScenario],
  ["settings.docling-readiness", createSettingsDoclingReadinessScenario],
  ["settings.initial-setup", createSettingsInitialSetupScenario],
  ["settings.initial-setup-hub", createInitialSetupHubScenario],
  ["settings.preferences", createSettingsPreferencesScenario],
  ["settings.preferences-config", createSettingsPreferencesConfigScenario],
  ["settings.session", createSettingsSessionScenario],
  ["settings.field-controls", createSettingsFieldControlsScenario],
  ["settings.additional-controls", createGlobalAdditionalControlsScenario],
  ["settings.initial-additional-controls", createInitialAdditionalControlsScenario],
  ["settings.session-discard-close", createSessionDiscardCloseScenario],
  ["settings.temporary-apply-controls", createTemporaryApplyControlsScenario],
  ["settings.initial-field-controls", createInitialSettingsFieldControlsScenario],
  ["settings.session-field-controls", createSessionSettingsFieldControlsScenario],
  ["settings.mcp-peer-controls", createSettingsMcpPeerControlsScenario],
  ["navigation.menu-entry-controls", createMenuEntryControlsScenario],
  ["navigation.palette-entry-controls", createPaletteEntryControlsScenario],
  ["run.next-turn", createRunNextTurnScenario],
  ["run.stop", createRunStopScenario],
  ["side-chat.quote", createSideChatQuoteScenario],
  ["side-chat.session", createSideChatSessionScenario],
]);
const configurableScenarios = new Set([
  "agent.interrupt",
  "navigation.external-rejoin",
  "navigation.external-sidebar-stop",
  "navigation.external-palette-rejoin",
  "hub.browser-enrollment",
  "hub.join-retry-controls",
  "hub.receiver-settings-controls",
  "hub.outgoing-controls",
  "mcp.receiver-live",
  "mcp.receiver-stop",
  "mcp.history-pagination",
  "mcp.receiver-approve",
  "mcp.receiver-deny",
  "mcp.receiver-abort",
  "manual.case5_2",
  "manual.provider-openai-compatible",
  "manual.provider-lm-studio-thinking",
  "manual.permission-guardian-openai-compatible",
  "manual.permission-temp-escalation-lm-studio",
]);

export const scenarioIds = Object.freeze([...factories.keys()]);

export function createScenario(id, options = {}) {
  if (options === null || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("Desktop E2E scenario options must be an object");
  }
  const factory = factories.get(id);
  if (!factory) throw new TypeError(`unknown Desktop E2E scenario: ${id}`);
  if (!configurableScenarios.has(id) && Object.keys(options).length > 0) {
    throw new TypeError(`Desktop E2E scenario does not accept options: ${id}`);
  }
  const scenario = factory(structuredClone(options));
  if (scenario?.id !== id) throw new TypeError(`scenario factory identity mismatch: expected ${id}, received ${scenario?.id}`);
  return scenario;
}

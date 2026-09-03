import { scenario as shellBaseline } from "./scenarios/shell_baseline.mjs";
import { createNativeDialogCancelScenario } from "./scenarios/native_dialog_cancel.mjs";
import { createPointerKeyboardScenario } from "./scenarios/pointer_keyboard.mjs";
import { createPromptReviewCancelScenario } from "./scenarios/prompt_review_cancel.mjs";
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
import { createSettingsPreferencesScenario } from "./scenarios/settings_preferences.mjs";
import { createSettingsSessionScenario } from "./scenarios/settings_session.mjs";
import { createAgentInterruptScenario } from "./scenarios/agent_interrupt.mjs";
import { createCase52Scenario } from "./scenarios/case5_2.mjs";
import { createHistoryRestartPrependScenario } from "./scenarios/history_restart_prepend.mjs";
import { createHistoryTerminalReconcileScenario } from "./scenarios/history_terminal_reconcile.mjs";

const factories = new Map([
  [shellBaseline.id, () => shellBaseline],
  ["agent.interrupt", createAgentInterruptScenario],
  ["history.restart-prepend", createHistoryRestartPrependScenario],
  ["history.terminal-reconcile", createHistoryTerminalReconcileScenario],
  ["input.pointer-keyboard", createPointerKeyboardScenario],
  ["manual.case5_2", createCase52Scenario],
  ["manual.provider-openai-compatible", createProviderConnectionLiveScenario],
  ["manual.provider-lm-studio-thinking", createLmStudioThinkingScenario],
  ["manual.permission-guardian-openai-compatible", createPermissionGuardianOpenAiCompatibleScenario],
  ["manual.permission-temp-escalation-lm-studio", createPermissionTempEscalationLmStudioScenario],
  ["native-dialog.cancel", createNativeDialogCancelScenario],
  ["prompt-review.cancel", createPromptReviewCancelScenario],
  ["permission.restart-guardian", createPermissionRestartGuardianScenario],
  ["permission.restart-guardian-chat", createPermissionRestartGuardianChatScenario],
  ["permission.temp-escalation", createPermissionTempEscalationScenario],
  ["provider.chat-tool-continuation", createProviderChatToolContinuationScenario],
  ["provider.responses-compaction-retry", createProviderResponsesCompactionRetryScenario],
  ["provider.responses-progress", createProviderResponsesProgressScenario],
  ["provider.restart", createProviderRestartScenario],
  ["settings.docling-readiness", createSettingsDoclingReadinessScenario],
  ["settings.initial-setup", createSettingsInitialSetupScenario],
  ["settings.preferences", createSettingsPreferencesScenario],
  ["settings.session", createSettingsSessionScenario],
  ["run.next-turn", createRunNextTurnScenario],
  ["run.stop", createRunStopScenario],
  ["side-chat.quote", createSideChatQuoteScenario],
  ["side-chat.session", createSideChatSessionScenario],
]);
const configurableScenarios = new Set([
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

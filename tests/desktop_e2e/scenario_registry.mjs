import { scenario as shellBaseline } from "./scenarios/shell_baseline.mjs";
import { createNativeDialogCancelScenario } from "./scenarios/native_dialog_cancel.mjs";
import { createPointerKeyboardScenario } from "./scenarios/pointer_keyboard.mjs";
import { createPromptReviewCancelScenario } from "./scenarios/prompt_review_cancel.mjs";
import { createProviderConnectionLiveScenario } from "./scenarios/provider_connection_live.mjs";
import { createProviderRestartScenario } from "./scenarios/provider_restart.mjs";
import { createRunStopScenario } from "./scenarios/run_stop.mjs";
import { createSettingsDoclingReadinessScenario } from "./scenarios/settings_docling_readiness.mjs";
import { createSettingsInitialSetupScenario } from "./scenarios/settings_initial_setup.mjs";
import { createSettingsPreferencesScenario } from "./scenarios/settings_preferences.mjs";
import { createSettingsSessionScenario } from "./scenarios/settings_session.mjs";
import { createAgentInterruptScenario } from "./scenarios/agent_interrupt.mjs";
import { createCase52Scenario } from "./scenarios/case5_2.mjs";
import { createHistoryRestartPrependScenario } from "./scenarios/history_restart_prepend.mjs";

const factories = new Map([
  [shellBaseline.id, () => shellBaseline],
  ["agent.interrupt", createAgentInterruptScenario],
  ["history.restart-prepend", createHistoryRestartPrependScenario],
  ["input.pointer-keyboard", createPointerKeyboardScenario],
  ["manual.case5_2", createCase52Scenario],
  ["manual.provider-openai-compatible", createProviderConnectionLiveScenario],
  ["native-dialog.cancel", createNativeDialogCancelScenario],
  ["prompt-review.cancel", createPromptReviewCancelScenario],
  ["provider.restart", createProviderRestartScenario],
  ["settings.docling-readiness", createSettingsDoclingReadinessScenario],
  ["settings.initial-setup", createSettingsInitialSetupScenario],
  ["settings.preferences", createSettingsPreferencesScenario],
  ["settings.session", createSettingsSessionScenario],
  ["run.stop", createRunStopScenario],
]);
const configurableScenarios = new Set(["manual.case5_2", "manual.provider-openai-compatible"]);

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

import { scenario as shellBaseline } from "./scenarios/shell_baseline.mjs";
import { createNativeDialogCancelScenario } from "./scenarios/native_dialog_cancel.mjs";
import { createPointerKeyboardScenario } from "./scenarios/pointer_keyboard.mjs";
import { createPromptReviewCancelScenario } from "./scenarios/prompt_review_cancel.mjs";
import { createProviderRestartScenario } from "./scenarios/provider_restart.mjs";
import { createRunStopScenario } from "./scenarios/run_stop.mjs";
import { createSettingsDoclingReadinessScenario } from "./scenarios/settings_docling_readiness.mjs";
import { createSettingsInitialSetupScenario } from "./scenarios/settings_initial_setup.mjs";
import { createSettingsPreferencesScenario } from "./scenarios/settings_preferences.mjs";
import { createSettingsSessionScenario } from "./scenarios/settings_session.mjs";
import { createAgentInterruptScenario } from "./scenarios/agent_interrupt.mjs";

const factories = new Map([
  [shellBaseline.id, () => shellBaseline],
  ["agent.interrupt", createAgentInterruptScenario],
  ["input.pointer-keyboard", createPointerKeyboardScenario],
  ["native-dialog.cancel", createNativeDialogCancelScenario],
  ["prompt-review.cancel", createPromptReviewCancelScenario],
  ["provider.restart", createProviderRestartScenario],
  ["settings.docling-readiness", createSettingsDoclingReadinessScenario],
  ["settings.initial-setup", createSettingsInitialSetupScenario],
  ["settings.preferences", createSettingsPreferencesScenario],
  ["settings.session", createSettingsSessionScenario],
  ["run.stop", createRunStopScenario],
]);

export const scenarioIds = Object.freeze([...factories.keys()]);

export function createScenario(id) {
  const factory = factories.get(id);
  if (!factory) throw new TypeError(`unknown Desktop E2E scenario: ${id}`);
  const scenario = factory();
  if (scenario?.id !== id) throw new TypeError(`scenario factory identity mismatch: expected ${id}, received ${scenario?.id}`);
  return scenario;
}

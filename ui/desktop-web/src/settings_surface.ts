import { sameConfigMutationTarget } from "./config_mutation.ts";
import type { FocusTargetCandidate } from "./focus_arbiter.ts";
import type { InitialSetupStep } from "./initial_setup_state.ts";
import type { ConfigMutationTarget, DesktopViewState, DesktopWebState } from "./types.ts";

export interface SettingsActionFocusContinuation {
  target: ConfigMutationTarget;
  primaryAction: string;
  fallbackAction: string | null;
}

const SETTINGS_SECTION_FRAGMENT = /^#(settings-[A-Za-z0-9_-]+)$/;

/**
 * Owns in-dialog Settings category navigation instead of leaving focus to the browser's
 * fragment-scroll behavior. Only a section inside the same Settings modal is eligible, so a
 * stale or malformed link cannot move focus into another surface. The live form subtree is not
 * replaced: browser-owned values, selections, and dirty state remain intact.
 */
export function activateSettingsSectionNavigation(anchor: HTMLAnchorElement): boolean {
  const targetId = settingsSectionTargetId(anchor.getAttribute("href"));
  const modal = anchor.closest<HTMLElement>(".settings-modal");
  const nav = anchor.closest<HTMLElement>(".settings-nav");
  const content = modal?.querySelector<HTMLElement>(".settings-content") ?? null;
  const section = targetId ? anchor.ownerDocument.getElementById(targetId) : null;
  if (
    !targetId
    || !modal
    || !nav
    || !content
    || !modal.contains(nav)
    || !(section instanceof HTMLElement)
    || !section.matches(".settings-section")
    || !content.contains(section)
  ) return false;

  section.scrollIntoView({ block: "start", inline: "nearest" });
  const editor = Array.from(
    section.querySelectorAll<HTMLElement>(".settings-control, .side-chat-settings-control"),
  ).find(settingsNavigationEditorIsAvailable);
  const summary = section.querySelector<HTMLElement>("summary");
  const focusTarget = editor ?? (summary && settingsNavigationTargetIsVisible(summary) ? summary : anchor);
  focusTarget.focus({ preventScroll: true });
  return focusTarget.ownerDocument.activeElement === focusTarget;
}

export function settingsSectionTargetId(href: string | null): string | null {
  return href?.match(SETTINGS_SECTION_FRAGMENT)?.[1] ?? null;
}

function settingsNavigationEditorIsAvailable(target: HTMLElement): boolean {
  return !settingsFocusTargetDisabled(target) && settingsNavigationTargetIsVisible(target);
}

function settingsNavigationTargetIsVisible(target: HTMLElement): boolean {
  return !target.hidden
    && target.closest("[hidden], [aria-hidden='true'], [inert], details:not([open])") === null;
}

/** Revalidates the exact Settings/config owner without querying or focusing the DOM. */
export function settingsActionFocusStillTargets(
  continuation: SettingsActionFocusContinuation,
  state: DesktopViewState,
): boolean {
  return state.overlay === "config"
    && !state.confirmation_visible
    && sameConfigMutationTarget(continuation.target, state.config_target);
}

/** Revalidates a request to close the exact Settings owner before any draft is discarded. */
export function settingsCloseTargetStillMatches(
  expectedTarget: ConfigMutationTarget,
  state: DesktopWebState,
): boolean {
  return state.overlay === "config"
    && !state.confirmation_visible
    && sameConfigMutationTarget(expectedTarget, state.config_target);
}

/** The action-specific fallback order, ending at the stable dialog owner. */
export function settingsActionFocusCandidateSelectors(
  continuation: SettingsActionFocusContinuation,
): readonly string[] {
  return [continuation.primaryAction, continuation.fallbackAction]
    .filter((action): action is string => action !== null)
    .map((action) => `[data-action="${action}"]`)
    .concat(".settings-modal");
}

export function settingsActionFocusCandidates(
  continuation: SettingsActionFocusContinuation,
  query: (selector: string) => HTMLElement | null,
): readonly FocusTargetCandidate[] {
  return settingsActionFocusCandidateSelectors(continuation).map((selector) => ({
    resolve: () => query(selector),
  }));
}

function settingsFocusTargetDisabled(target: HTMLElement): boolean {
  return ("disabled" in target && Boolean((target as HTMLButtonElement).disabled))
    || target.getAttribute("aria-disabled") === "true";
}

/**
 * Identifies the Settings form subtree that owns in-progress browser interaction.
 * Main config values are intentionally excluded: the live input nodes own unsaved edits.
 * Side-chat provider state is included because it changes the dedicated section's capability,
 * status, and committed-value display. Browser-owned side-chat inputs and the explicitly loaded
 * model catalog remain outside this identity and are synchronized onto the connected controls.
 */
export function settingsSurfaceIdentity(
  state: DesktopViewState | null,
  initialSetupStep?: InitialSetupStep,
): string | null {
  if (!state || state.confirmation_visible) return null;
  if (state.overlay === "initial_setup") {
    const target = state.startup.setup_target;
    if (!state.startup.initial_setup_required || target === null || initialSetupStep === undefined) {
      return null;
    }
    return JSON.stringify({
      surface: "initial_setup",
      target,
      configTarget: state.config_target,
      step: initialSetupStep,
    });
  }
  if (state.overlay === "session_settings") {
    const target = state.session_settings.target;
    if (!state.session_settings.available || target === null) return null;
    return JSON.stringify({
      surface: "session_settings",
      workspacePath: target.workspacePath,
      rootSessionId: target.rootSessionId,
    });
  }
  if (state.overlay !== "config") return null;
  return JSON.stringify({
    surface: "config",
    workspacePath: state.config_target.workspacePath,
    sessionId: state.config_target.sessionId,
    configGeneration: state.config_target.configGeneration,
    initialSetup: state.startup.initial_setup_required,
    editEnabled: state.config_draft.edit_enabled,
    sideChatTarget: {
      selectedSessionId: state.draft_target.sessionId,
      ownerSessionId: state.side_chat.owner_session_id,
      chatId: state.side_chat.chat_id,
      configured: state.side_chat.configured,
      deleting: state.side_chat.deleting,
      model: state.side_chat.model,
      baseUrl: state.side_chat.base_url,
      status: state.side_chat.status,
      lastError: state.side_chat.last_error,
      canSend: state.side_chat.can_send,
    },
    fields: state.config_fields.map((field) => ({
      key: field.key,
      envOverride: field.env_override,
      valueType: field.value_type,
      required: field.required,
      minValue: field.min_value,
      maxValue: field.max_value,
      options: field.options,
    })),
  });
}

/** A non-null owner for transient errors, including non-Settings and unavailable surfaces. */
export function settingsRecoverableErrorOwnerIdentity(
  state: DesktopViewState,
  initialSetupStep?: InitialSetupStep,
): string {
  return settingsSurfaceIdentity(state, initialSetupStep)
    ?? JSON.stringify({
      surface: state.overlay || "none",
      workspacePath: state.workspace_path,
      sessionId: state.draft_target.sessionId,
      unowned: true,
    });
}

export function sameSettingsSurface(
  previous: DesktopViewState | null,
  current: DesktopViewState,
  previousInitialSetupStep?: InitialSetupStep,
  currentInitialSetupStep?: InitialSetupStep,
): boolean {
  const previousIdentity = settingsSurfaceIdentity(previous, previousInitialSetupStep);
  return previousIdentity !== null
    && previousIdentity === settingsSurfaceIdentity(current, currentInitialSetupStep);
}

/**
 * Retains the connected Settings tree only while no local modal layer is entering or leaving.
 * A closing local modal must rebuild the outer markup once so its backdrop and the retained
 * Settings backdrop's inert/aria-hidden state cannot survive the transition.
 */
export function shouldRetainConnectedSettingsSurface(
  previous: DesktopViewState | null,
  current: DesktopViewState,
  previousLocalModalIdentity: string | null,
  currentLocalModalIdentity: string | null,
  previousInitialSetupStep?: InitialSetupStep,
  currentInitialSetupStep?: InitialSetupStep,
): boolean {
  return previousLocalModalIdentity === null
    && currentLocalModalIdentity === null
    && sameSettingsSurface(
      previous,
      current,
      previousInitialSetupStep,
      currentInitialSetupStep,
    );
}

type SettingsSurfaceControl = HTMLButtonElement | HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement;

/**
 * Synchronizes only action/control availability on a retained Settings tree.
 *
 * The connected controls remain the browser-side owner of values, checked state, selection,
 * focus, IME composition, scroll position, and details state. The newly rendered tree is used
 * only as the fresh availability model, with a config mutation additionally locking every
 * action and editor until its exact-target settlement completes.
 */
export function synchronizeRetainedSettingsSurface(
  currentModal: HTMLElement,
  nextModal: HTMLElement,
  configMutationPending: boolean,
  synchronizeValues = false,
): void {
  currentModal.setAttribute("aria-busy", String(configMutationPending));
  const currentControls = indexedSettingsSurfaceControls(currentModal);
  const nextControls = indexedSettingsSurfaceControls(nextModal);
  for (const [identity, current] of currentControls) {
    const next = nextControls.get(identity);
    if (!next) continue;
    const disabled = configMutationPending || next.disabled;
    current.disabled = disabled;
    current.hidden = next.hidden;
    synchronizeRetainedAvailabilityAnnotation(current, next, "aria-busy");
    synchronizeRetainedAvailabilityAnnotation(current, next, "aria-haspopup");
    if (synchronizeValues) {
      synchronizeRetainedControlValue(current, next);
      synchronizeRetainedAvailabilityAnnotation(current, next, "aria-invalid");
    }
    if (current.tagName === "BUTTON" && current.innerHTML !== next.innerHTML) {
      current.innerHTML = next.innerHTML;
    }
    if (disabled || next.getAttribute("aria-disabled") === "true") {
      current.setAttribute("aria-disabled", "true");
    } else if (next.hasAttribute("aria-disabled")) {
      current.setAttribute("aria-disabled", next.getAttribute("aria-disabled") ?? "false");
    } else {
      current.removeAttribute("aria-disabled");
    }
  }
  synchronizeRetainedAvailabilityAnnotation(
    currentModal.querySelector<HTMLElement>("[data-docling-dependent]"),
    nextModal.querySelector<HTMLElement>("[data-docling-dependent]"),
    "aria-disabled",
  );
  const currentDoclingHelp = currentModal.querySelector<HTMLElement>("#docling-disabled-help");
  const nextDoclingHelp = nextModal.querySelector<HTMLElement>("#docling-disabled-help");
  if (currentDoclingHelp && nextDoclingHelp) currentDoclingHelp.hidden = nextDoclingHelp.hidden;
  synchronizeRetainedLiveRegions(currentModal, nextModal);
  synchronizeRetainedPassiveRegions(currentModal, nextModal);
  synchronizeRetainedModelSelects(currentModal, nextModal, synchronizeValues);
}

function synchronizeRetainedPassiveRegions(
  currentModal: HTMLElement,
  nextModal: HTMLElement,
): void {
  const currentRegions = new Map(Array.from(
    currentModal.querySelectorAll<HTMLElement>("[data-settings-passive]"),
    (region) => [region.dataset.settingsPassive ?? "", region] as const,
  ));
  nextModal.querySelectorAll<HTMLElement>("[data-settings-passive]").forEach((next) => {
    const identity = next.dataset.settingsPassive ?? "";
    const current = currentRegions.get(identity);
    if (
      identity
      && current
      && !(
        current.hasAttribute("data-settings-preserve-focused-region")
        && current.contains(current.ownerDocument.activeElement)
      )
    ) current.replaceWith(next);
  });
}

function synchronizeRetainedControlValue(
  current: SettingsSurfaceControl,
  next: SettingsSurfaceControl,
): void {
  if (current.tagName === "INPUT" && next.tagName === "INPUT") {
    const currentInput = current as HTMLInputElement;
    const nextInput = next as HTMLInputElement;
    if (currentInput.type === "checkbox" || currentInput.type === "radio") {
      if (currentInput.checked !== nextInput.checked) currentInput.checked = nextInput.checked;
    } else if (currentInput.value !== nextInput.value) {
      currentInput.value = nextInput.value;
    }
    return;
  }
  if (current.tagName === "TEXTAREA" && next.tagName === "TEXTAREA") {
    const currentTextarea = current as HTMLTextAreaElement;
    const nextTextarea = next as HTMLTextAreaElement;
    if (currentTextarea.value !== nextTextarea.value) currentTextarea.value = nextTextarea.value;
    return;
  }
  if (current.tagName === "SELECT" && next.tagName === "SELECT") {
    const currentSelect = current as HTMLSelectElement;
    const nextSelect = next as HTMLSelectElement;
    if (currentSelect.value !== nextSelect.value) currentSelect.value = nextSelect.value;
  }
}

function synchronizeRetainedModelSelects(
  currentModal: HTMLElement,
  nextModal: HTMLElement,
  synchronizeValues: boolean,
): void {
  const currentSelects = new Map(Array.from(
    currentModal.querySelectorAll<HTMLSelectElement>("select[data-main-provider-model-control]"),
    (select) => [select.id, select] as const,
  ));
  for (const next of Array.from(
    nextModal.querySelectorAll<HTMLSelectElement>("select[data-main-provider-model-control]"),
  )) {
    const current = currentSelects.get(next.id);
    if (!current) continue;
    if (current === current.ownerDocument.activeElement) {
      queueFocusedModelSelectSynchronization(current, next, synchronizeValues);
      continue;
    }
    applyModelSelectSynchronization(current, next, synchronizeValues);
  }
}

interface PendingModelSelectSynchronization {
  options: HTMLOptionElement[];
  value: string;
  synchronizeValues: boolean;
}

const pendingModelSelectSynchronizations = new WeakMap<
  HTMLSelectElement,
  PendingModelSelectSynchronization
>();
const modelSelectBlurListeners = new WeakSet<HTMLSelectElement>();

function queueFocusedModelSelectSynchronization(
  current: HTMLSelectElement,
  next: HTMLSelectElement,
  synchronizeValues: boolean,
): void {
  pendingModelSelectSynchronizations.set(current, {
    options: Array.from(next.options, (option) => option.cloneNode(true) as HTMLOptionElement),
    value: next.value,
    synchronizeValues,
  });
  if (modelSelectBlurListeners.has(current)) return;
  modelSelectBlurListeners.add(current);
  current.addEventListener("blur", () => {
    const pending = pendingModelSelectSynchronizations.get(current);
    pendingModelSelectSynchronizations.delete(current);
    modelSelectBlurListeners.delete(current);
    if (!pending || !current.isConnected) return;
    applyModelSelectOptions(
      current,
      pending.options,
      pending.value,
      pending.synchronizeValues,
    );
  }, { once: true });
}

function applyModelSelectSynchronization(
  current: HTMLSelectElement,
  next: HTMLSelectElement,
  synchronizeValues: boolean,
): void {
  pendingModelSelectSynchronizations.delete(current);
  applyModelSelectOptions(
    current,
    Array.from(next.options, (option) => option.cloneNode(true) as HTMLOptionElement),
    next.value,
    synchronizeValues,
  );
}

function applyModelSelectOptions(
  current: HTMLSelectElement,
  nextOptions: readonly HTMLOptionElement[],
  nextValue: string,
  synchronizeValues: boolean,
): void {
  const browserOwnedValue = current.value;
  current.replaceChildren(...nextOptions);
  current.value = !synchronizeValues
    && Array.from(current.options).some((option) => option.value === browserOwnedValue)
    ? browserOwnedValue
    : nextValue;
}

function synchronizeRetainedLiveRegions(currentModal: HTMLElement, nextModal: HTMLElement): void {
  const nextRegions = new Map(Array.from(
    nextModal.querySelectorAll<HTMLElement>("[data-settings-live-region]"),
    (region) => [region.dataset.settingsLiveRegion ?? "", region] as const,
  ));
  currentModal.querySelectorAll<HTMLElement>("[data-settings-live-region]").forEach((current) => {
    const identity = current.dataset.settingsLiveRegion ?? "";
    const next = nextRegions.get(identity);
    if (identity && next && !current.contains(current.ownerDocument.activeElement)) {
      current.replaceWith(next);
    }
  });
}

function synchronizeRetainedAvailabilityAnnotation(
  current: HTMLElement | null,
  next: HTMLElement | null,
  attribute: string,
): void {
  if (!current || !next) return;
  const value = next.getAttribute(attribute);
  if (value === null) current.removeAttribute(attribute);
  else current.setAttribute(attribute, value);
}

function indexedSettingsSurfaceControls(root: HTMLElement): Map<string, SettingsSurfaceControl> {
  const indexed = new Map<string, SettingsSurfaceControl>();
  const occurrences = new Map<string, number>();
  for (const control of Array.from(
    root.querySelectorAll<SettingsSurfaceControl>("button, input, select, textarea"),
  )) {
    const baseIdentity = settingsSurfaceControlIdentity(control);
    if (!baseIdentity) continue;
    const occurrence = occurrences.get(baseIdentity) ?? 0;
    occurrences.set(baseIdentity, occurrence + 1);
    indexed.set(`${baseIdentity}:${occurrence}`, control);
  }
  return indexed;
}

function settingsSurfaceControlIdentity(control: SettingsSurfaceControl): string | null {
  const tag = control.tagName.toLowerCase();
  const id = control.getAttribute("id");
  if (id) return `${tag}#${id}`;
  const action = control.getAttribute("data-action");
  if (action) return `${tag}[data-action=${action}]`;
  const configKey = control.getAttribute("data-config-key");
  if (configKey) return `${tag}[data-config-key=${configKey}]`;
  const sideChatSetting = control.getAttribute("data-side-chat-setting");
  if (sideChatSetting) return `${tag}[data-side-chat-setting=${sideChatSetting}]`;
  const sessionSetting = control.getAttribute("data-session-setting");
  if (sessionSetting) return `${tag}[data-session-setting=${sessionSetting}]`;
  return null;
}

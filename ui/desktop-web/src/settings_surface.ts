import { sameConfigMutationTarget } from "./config_mutation.ts";
import type { FocusTargetCandidate } from "./focus_arbiter.ts";
import type { ConfigMutationTarget, DesktopViewState } from "./types.ts";

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
export function settingsSurfaceIdentity(state: DesktopViewState | null): string | null {
  if (!state || state.overlay !== "config" || state.confirmation_visible) return null;
  return JSON.stringify({
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

export function sameSettingsSurface(
  previous: DesktopViewState | null,
  current: DesktopViewState,
): boolean {
  const previousIdentity = settingsSurfaceIdentity(previous);
  return previousIdentity !== null && previousIdentity === settingsSurfaceIdentity(current);
}

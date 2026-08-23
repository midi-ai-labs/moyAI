export interface RefreshPromptFocusContinuation {
  owner: string;
  interactionGeneration: bigint;
}

export interface RefreshPromptFocusState {
  refreshPromptFocusInteractionGeneration: bigint;
  pendingRefreshPromptFocus: RefreshPromptFocusContinuation | null;
}

interface RefreshPointerInteraction {
  owner: string;
  targetsRefresh: boolean;
  promptFocused: boolean;
}

const wiredPromptInputs = new WeakSet<EventTarget>();

export function recordRefreshPointerInteraction(
  state: RefreshPromptFocusState,
  interaction: RefreshPointerInteraction,
): RefreshPromptFocusContinuation | null {
  state.refreshPromptFocusInteractionGeneration += 1n;
  state.pendingRefreshPromptFocus = interaction.targetsRefresh && interaction.promptFocused
    ? {
        owner: interaction.owner,
        interactionGeneration: state.refreshPromptFocusInteractionGeneration,
      }
    : null;
  return state.pendingRefreshPromptFocus;
}

export function invalidateRefreshPromptFocus(state: RefreshPromptFocusState): void {
  state.refreshPromptFocusInteractionGeneration += 1n;
  state.pendingRefreshPromptFocus = null;
}

export function takePendingRefreshPromptFocus(
  state: RefreshPromptFocusState,
  mutationName: string,
): RefreshPromptFocusContinuation | null {
  if (mutationName !== "refresh_desktop") return null;
  const continuation = state.pendingRefreshPromptFocus;
  state.pendingRefreshPromptFocus = null;
  return continuation;
}

export function refreshPromptFocusContinuationAccepted(
  continuation: RefreshPromptFocusContinuation | null,
  currentInteractionGeneration: bigint,
  mutationName: string | null,
  currentOwner: string,
  refreshStillOwnsFocus: boolean,
): boolean {
  return continuation !== null
    && mutationName === "refresh_desktop"
    && continuation.interactionGeneration === currentInteractionGeneration
    && continuation.owner === currentOwner
    && refreshStillOwnsFocus;
}

export function wireMainPromptInputOnce(
  prompt: HTMLTextAreaElement,
  listener: EventListener,
): boolean {
  if (wiredPromptInputs.has(prompt)) return false;
  wiredPromptInputs.add(prompt);
  prompt.addEventListener("input", listener);
  return true;
}

function synchronizeOptionalAttribute(
  current: HTMLTextAreaElement,
  next: HTMLTextAreaElement,
  name: string,
): void {
  const value = next.getAttribute(name);
  if (value === null) current.removeAttribute(name);
  else current.setAttribute(name, value);
}

/**
 * Keep the live Main editor node for the same stable session owner while adopting the
 * freshly rendered value and capabilities. Runtime-only data/style attached to the live
 * textarea deliberately remains on that node.
 */
export function retainConnectedMainPrompt(
  current: HTMLTextAreaElement | null,
  next: HTMLTextAreaElement | null,
  previousOwner: string | null,
  nextOwner: string,
): boolean {
  if (!current || !next || !current.isConnected || previousOwner !== nextOwner) return false;

  const nextValue = next.value;
  current.id = next.id;
  current.className = next.className;
  current.placeholder = next.placeholder;
  current.disabled = next.disabled;
  current.readOnly = next.readOnly;
  current.required = next.required;
  current.tabIndex = next.tabIndex;
  current.defaultValue = next.defaultValue;
  if (current.value !== nextValue) current.value = nextValue;
  for (const name of ["aria-describedby", "aria-label", "aria-invalid", "aria-busy"]) {
    synchronizeOptionalAttribute(current, next, name);
  }
  next.replaceWith(current);
  return true;
}

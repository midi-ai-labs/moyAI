import { rowMutationTarget, sameRowMutationTarget } from "./row_target.ts";
import { projectionRevisionAtLeast } from "./projection_state.ts";
import type { FocusTargetCandidate } from "./focus_arbiter.ts";
import type {
  CommandPaletteInsertionResult,
  DesktopWebState,
  DraftActionTarget,
  RowMutationTarget,
} from "./types.ts";
import { composerOwner } from "./view_state.ts";

export interface CommandPaletteDraftState {
  composerOwner: string;
  composerRevision: number;
  prompt: string;
}

export interface CommandPaletteInsertionRequest {
  requestId: number;
  interactionGeneration: bigint;
  projectionRevision: string;
  projectedDraftPrompt: string;
  composerCommitGeneration: string;
  owner: string;
  composerRevision: number;
  value: string;
  selectionStart: number;
  selectionEnd: number;
  promptScrollLeft: number;
  promptScrollTop: number;
  index: number;
  rowPath: string;
  expectedInsertionText: string;
  expectedTarget: RowMutationTarget;
  expectedDraftTarget: DraftActionTarget;
  prompt: HTMLTextAreaElement;
  focusOwner: Element;
}

export interface CommandPaletteInsertionSettlement {
  value: string;
  caret: number;
  focusContinuation: CommandPaletteInsertionFocusContinuation;
}

export interface CommandPaletteInsertionFocusContinuation {
  requestId: number;
  interactionGeneration: bigint;
  projectionRevision: string;
  owner: string;
  composerRevision: number;
  value: string;
  caret: number;
  promptScrollLeft: number;
  promptScrollTop: number;
  prompt: HTMLTextAreaElement;
}

export interface CommandPaletteInsertionSettlementInput {
  request: CommandPaletteInsertionRequest;
  activeRequest: CommandPaletteInsertionRequest | null;
  interactionGeneration: bigint;
  interactionIdle: boolean;
  projectionAccepted: boolean;
  currentState: DesktopWebState;
  response: CommandPaletteInsertionResult;
  drafts: CommandPaletteDraftState;
  prompt: HTMLTextAreaElement | null;
  focusOwned: boolean;
}

export interface CommandPaletteInsertionInteractionLifecycle {
  readonly active: boolean;
  whenIdle(): Promise<void>;
}

export class CommandPaletteInsertionAsyncOwner {
  private request: CommandPaletteInsertionRequest | null = null;

  get activeRequest(): CommandPaletteInsertionRequest | null {
    return this.request;
  }

  begin(request: CommandPaletteInsertionRequest): boolean {
    if (this.request) return false;
    this.request = request;
    return true;
  }

  finish(request: CommandPaletteInsertionRequest): boolean {
    if (this.request !== request) return false;
    this.request = null;
    return true;
  }
}

export interface CommandPaletteInsertionDispatchResult<Response, Settlement> {
  response: Response;
  settlement: Settlement | null;
}

export async function dispatchCommandPaletteInsertion<Response, Settlement>(
  owner: CommandPaletteInsertionAsyncOwner,
  request: CommandPaletteInsertionRequest,
  interaction: CommandPaletteInsertionInteractionLifecycle,
  invoke: () => Promise<Response>,
  settle: (response: Response) => Settlement | null,
): Promise<CommandPaletteInsertionDispatchResult<Response, Settlement> | null> {
  if (!owner.begin(request)) return null;
  try {
    const response = await invoke();
    await interaction.whenIdle();
    const settlement = !interaction.active && owner.activeRequest === request
      ? settle(response)
      : null;
    return { response, settlement };
  } finally {
    // A fast success or failure remains single-flight through the pointer/key
    // activation that dispatched it. This also lets recovery signals invalidate
    // local settlement before the continuation microtask runs.
    await interaction.whenIdle();
    owner.finish(request);
  }
}

export type CommandPaletteInsertionFocusResult =
  | "focused"
  | "already-focused"
  | "stale"
  | "owned"
  | "unavailable";

export function beginCommandPaletteInsertion(
  state: DesktopWebState,
  drafts: CommandPaletteDraftState,
  prompt: HTMLTextAreaElement,
  focusOwner: Element,
  index: number,
  requestId: number,
  interactionGeneration: bigint,
): CommandPaletteInsertionRequest | null {
  const row = state.command_rows[index];
  const selection = selectionRange(prompt.value, prompt.selectionStart, prompt.selectionEnd);
  const owner = composerOwner(state);
  if (
    state.overlay !== "command_palette"
    || state.confirmation_visible
    || !row
    || !prompt.isConnected
    || prompt.disabled
    || prompt.readOnly
    || !focusOwner.isConnected
    || drafts.composerOwner !== owner
    || drafts.prompt !== prompt.value
    || !selection
  ) {
    return null;
  }
  return {
    requestId,
    interactionGeneration,
    projectionRevision: state.projection_revision,
    projectedDraftPrompt: state.draft_prompt,
    composerCommitGeneration: state.composer_commit_generation,
    owner,
    composerRevision: drafts.composerRevision,
    value: drafts.prompt,
    selectionStart: selection.start,
    selectionEnd: selection.end,
    promptScrollLeft: prompt.scrollLeft,
    promptScrollTop: prompt.scrollTop,
    index,
    rowPath: row.path,
    expectedInsertionText: `/${row.name} `,
    expectedTarget: rowMutationTarget(state, row.path),
    expectedDraftTarget: { ...state.draft_target },
    prompt,
    focusOwner,
  };
}

export function settleCommandPaletteInsertion(
  input: CommandPaletteInsertionSettlementInput,
): CommandPaletteInsertionSettlement | null {
  const { request, currentState, response, drafts, prompt } = input;
  const currentRow = currentState.command_rows[request.index];
  const responseRow = response.state.command_rows[request.index];
  if (
    input.activeRequest !== request
    || input.interactionGeneration !== request.interactionGeneration
    || !input.interactionIdle
    || !input.projectionAccepted
    || !input.focusOwned
    || currentState.projection_revision !== request.projectionRevision
    || currentState.overlay !== "command_palette"
    || currentState.confirmation_visible
    || prompt !== request.prompt
    || !request.prompt.isConnected
    || request.prompt.disabled
    || request.prompt.readOnly
    || !request.focusOwner.isConnected
    || request.prompt.value !== request.value
    || request.prompt.selectionStart !== request.selectionStart
    || request.prompt.selectionEnd !== request.selectionEnd
    || drafts.composerOwner !== request.owner
    || drafts.composerRevision !== request.composerRevision
    || drafts.prompt !== request.value
    || composerOwner(currentState) !== request.owner
    || composerOwner(response.state) !== request.owner
    || !sameDraftActionTarget(currentState.draft_target, request.expectedDraftTarget)
    || !sameDraftActionTarget(response.state.draft_target, request.expectedDraftTarget)
    || !currentRow
    || !responseRow
    || currentRow.path !== request.rowPath
    || responseRow.path !== request.rowPath
    || !sameRowMutationTarget(
      request.expectedTarget,
      rowMutationTarget(currentState, currentRow.path),
    )
    || !sameRowMutationTarget(
      request.expectedTarget,
      rowMutationTarget(response.state, responseRow.path),
    )
    || response.insertionText !== request.expectedInsertionText
    || response.state.overlay !== "none"
    || response.state.confirmation_visible
    || response.state.draft_prompt !== request.projectedDraftPrompt
    || response.state.composer_commit_generation !== request.composerCommitGeneration
  ) {
    return null;
  }
  const replacement = replaceUtf16Selection(
    request.value,
    request.selectionStart,
    request.selectionEnd,
    response.insertionText,
  );
  if (!replacement) return null;
  const composerRevision = request.composerRevision + 1;
  return {
    value: replacement.value,
    caret: replacement.caret,
    focusContinuation: {
      requestId: request.requestId,
      interactionGeneration: request.interactionGeneration,
      projectionRevision: response.state.projection_revision,
      owner: request.owner,
      composerRevision,
      value: replacement.value,
      caret: replacement.caret,
      promptScrollLeft: request.promptScrollLeft,
      promptScrollTop: request.promptScrollTop,
      prompt: request.prompt,
    },
  };
}

export function replaceUtf16Selection(
  value: string,
  selectionStart: number,
  selectionEnd: number,
  insertionText: string,
): { value: string; caret: number } | null {
  if (!selectionRange(value, selectionStart, selectionEnd) || insertionText.length === 0) {
    return null;
  }
  return {
    value: value.slice(0, selectionStart) + insertionText + value.slice(selectionEnd),
    caret: selectionStart + insertionText.length,
  };
}

export function focusCommandPaletteInsertionIfCurrent(
  documentTarget: Document,
  continuation: CommandPaletteInsertionFocusContinuation,
  interactionGeneration: bigint,
  state: DesktopWebState,
  drafts: CommandPaletteDraftState,
): CommandPaletteInsertionFocusResult {
  if (!commandPaletteInsertionFocusStillCurrent(
    continuation,
    interactionGeneration,
    state,
    drafts,
  )) return "stale";
  const candidate = commandPaletteInsertionFocusCandidates(documentTarget, continuation)[0];
  const prompt = candidate?.resolve() as HTMLTextAreaElement | null | undefined;
  if (!prompt) return "unavailable";
  const active = documentTarget.activeElement;
  if (
    active !== prompt
    && active !== documentTarget.body
    && active !== documentTarget.documentElement
    && !elementOwnsPaletteReturnFocus(active)
  ) {
    return "owned";
  }
  const alreadyFocused = active === prompt;
  if (!alreadyFocused) prompt.focus({ preventScroll: true });
  if (documentTarget.activeElement !== prompt) return "unavailable";
  candidate?.settle?.(prompt);
  return alreadyFocused ? "already-focused" : "focused";
}

/** Revalidates the exact local draft owner without resolving or focusing a DOM node. */
export function commandPaletteInsertionFocusStillCurrent(
  continuation: CommandPaletteInsertionFocusContinuation,
  interactionGeneration: bigint,
  state: DesktopWebState,
  drafts: CommandPaletteDraftState,
): boolean {
  return interactionGeneration === continuation.interactionGeneration
    && projectionRevisionAtLeast(state.projection_revision, continuation.projectionRevision)
    && state.overlay === "none"
    && !state.confirmation_visible
    && composerOwner(state) === continuation.owner
    && drafts.composerOwner === continuation.owner
    && drafts.composerRevision === continuation.composerRevision
    && drafts.prompt === continuation.value;
}

/** Resolves only the exact textarea captured by the insertion owner. */
export function commandPaletteInsertionFocusTarget(
  documentTarget: Document,
  continuation: CommandPaletteInsertionFocusContinuation,
): HTMLTextAreaElement | null {
  const prompt = documentTarget.querySelector<HTMLTextAreaElement>("#prompt");
  return prompt === continuation.prompt
    && prompt.isConnected
    && !prompt.disabled
    && prompt.value === continuation.value
    ? prompt
    : null;
}

/** Restores caret and textarea scroll after focus has been settled by the shared owner. */
export function settleCommandPaletteInsertionSelectionAndScroll(
  continuation: CommandPaletteInsertionFocusContinuation,
  prompt: HTMLTextAreaElement,
): void {
  prompt.setSelectionRange(continuation.caret, continuation.caret);
  prompt.scrollLeft = continuation.promptScrollLeft;
  prompt.scrollTop = continuation.promptScrollTop;
}

export function commandPaletteInsertionFocusCandidates(
  documentTarget: Document,
  continuation: CommandPaletteInsertionFocusContinuation,
): readonly FocusTargetCandidate[] {
  return [{
    resolve: () => commandPaletteInsertionFocusTarget(documentTarget, continuation),
    settle: (target) => settleCommandPaletteInsertionSelectionAndScroll(
      continuation,
      target as HTMLTextAreaElement,
    ),
  }];
}

function selectionRange(
  value: string,
  selectionStart: number | null,
  selectionEnd: number | null,
): { start: number; end: number } | null {
  if (
    selectionStart === null
    || selectionEnd === null
    || !Number.isInteger(selectionStart)
    || !Number.isInteger(selectionEnd)
    || selectionStart < 0
    || selectionStart > selectionEnd
    || selectionEnd > value.length
    || !isUtf16Boundary(value, selectionStart)
    || !isUtf16Boundary(value, selectionEnd)
  ) {
    return null;
  }
  return { start: selectionStart, end: selectionEnd };
}

function isUtf16Boundary(value: string, index: number): boolean {
  if (index <= 0 || index >= value.length) return true;
  const previous = value.charCodeAt(index - 1);
  const current = value.charCodeAt(index);
  return !(previous >= 0xd800 && previous <= 0xdbff && current >= 0xdc00 && current <= 0xdfff);
}

function sameDraftActionTarget(
  left: DraftActionTarget,
  right: DraftActionTarget,
): boolean {
  return left.workspacePath === right.workspacePath
    && left.sessionId === right.sessionId
    && left.ownerGeneration === right.ownerGeneration;
}

function elementOwnsPaletteReturnFocus(active: Element | null): boolean {
  const closest = active && "closest" in active
    ? (active as Element).closest.bind(active)
    : null;
  return closest !== null && closest('[data-action="show-command-palette"]') !== null;
}

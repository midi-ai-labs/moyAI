import assert from "node:assert/strict";
import test from "node:test";

import {
  beginCommandPaletteInsertion,
  commandPaletteInsertionFocusCandidates,
  commandPaletteInsertionFocusStillCurrent,
  commandPaletteInsertionFocusTarget,
  focusCommandPaletteInsertionIfCurrent,
  replaceUtf16Selection,
  settleCommandPaletteInsertionSelectionAndScroll,
  settleCommandPaletteInsertion,
  type CommandPaletteDraftState,
  type CommandPaletteInsertionRequest,
  type CommandPaletteInsertionSettlementInput,
} from "../src/command_palette_insertion.ts";
import type {
  CommandPaletteInsertionResult,
  DesktopWebState,
} from "../src/types.ts";

const SOURCE_VALUE = "左😀選択右";
const PREFIX = "左😀";
const SELECTED = "選択";
const INSERTION_TEXT = "/case ";
const EXPECTED_VALUE = `左😀${INSERTION_TEXT}右`;

class FakePrompt {
  value: string;
  selectionStart: number | null;
  selectionEnd: number | null;
  scrollLeft = 12;
  scrollTop = 24;
  isConnected = true;
  disabled = false;
  readOnly = false;
  focusCalls = 0;
  documentTarget: FakeDocument | null = null;

  constructor(value: string, selectionStart: number, selectionEnd: number) {
    this.value = value;
    this.selectionStart = selectionStart;
    this.selectionEnd = selectionEnd;
  }

  focus(): void {
    this.focusCalls += 1;
    if (this.documentTarget) this.documentTarget.activeElement = this;
  }

  setSelectionRange(selectionStart: number, selectionEnd: number): void {
    this.selectionStart = selectionStart;
    this.selectionEnd = selectionEnd;
  }
}

class FakeDocument {
  readonly body = { surface: "body" };
  readonly documentElement = { surface: "html" };
  activeElement: unknown = this.body;
  readonly prompt: FakePrompt;

  constructor(prompt: FakePrompt) {
    this.prompt = prompt;
    prompt.documentTarget = this;
  }

  querySelector<T>(selector: string): T | null {
    return selector === "#prompt" ? this.prompt as T : null;
  }
}

interface StateOptions {
  revision?: string;
  overlay?: string;
  confirmationVisible?: boolean;
  ownerGeneration?: string;
  workspacePath?: string;
  projectId?: string;
  sessionId?: string | null;
  rowPath?: string;
  rowName?: string;
  draftPrompt?: string;
  composerCommitGeneration?: string;
}

function webState(options: StateOptions = {}): DesktopWebState {
  const workspacePath = options.workspacePath ?? "C:/workspace";
  const sessionId = options.sessionId === undefined ? "session-a" : options.sessionId;
  return {
    projection_revision: options.revision ?? "7",
    workspace_path: workspacePath,
    overlay: options.overlay ?? "command_palette",
    confirmation_visible: options.confirmationVisible ?? false,
    draft_prompt: options.draftPrompt ?? "server-owned-draft",
    composer_commit_generation: options.composerCommitGeneration ?? "11",
    draft_target: {
      workspacePath,
      sessionId,
      ownerGeneration: options.ownerGeneration ?? "3",
    },
    project_rows: [{
      project_id: options.projectId ?? "project-a",
      path: workspacePath,
      label: "Project A",
    }],
    selected_project_index: 0,
    session_rows: sessionId === null ? [] : [{ session_id: sessionId }],
    selected_session_index: sessionId === null ? -1 : 0,
    command_rows: [{
      name: options.rowName ?? "case",
      label: "Case",
      path: options.rowPath ?? "builtin:case",
    }],
  } as unknown as DesktopWebState;
}

function fakeElement(action = "insert-command"): Element {
  const element = {
    isConnected: true,
    closest(selector: string): unknown {
      return selector.includes(action) ? element : null;
    },
  };
  return element as unknown as Element;
}

interface Scenario {
  prompt: FakePrompt;
  focusOwner: Element;
  drafts: CommandPaletteDraftState;
  request: CommandPaletteInsertionRequest;
  response: CommandPaletteInsertionResult;
  input: CommandPaletteInsertionSettlementInput;
}

function scenario(): Scenario {
  const selectionStart = PREFIX.length;
  const selectionEnd = selectionStart + SELECTED.length;
  const prompt = new FakePrompt(SOURCE_VALUE, selectionStart, selectionEnd);
  const focusOwner = fakeElement();
  const requestState = webState();
  const drafts: CommandPaletteDraftState = {
    composerOwner: `${requestState.draft_target.workspacePath}\u0000${requestState.draft_target.ownerGeneration}\u0000${requestState.draft_target.sessionId}`,
    composerRevision: 5,
    prompt: SOURCE_VALUE,
  };
  const request = beginCommandPaletteInsertion(
    requestState,
    drafts,
    prompt as unknown as HTMLTextAreaElement,
    focusOwner,
    0,
    19,
    23n,
  );
  assert.ok(request);
  const response: CommandPaletteInsertionResult = {
    state: webState({ revision: "8", overlay: "none" }),
    insertionText: INSERTION_TEXT,
  };
  const input: CommandPaletteInsertionSettlementInput = {
    request,
    activeRequest: request,
    interactionGeneration: 23n,
    interactionIdle: true,
    projectionAccepted: true,
    currentState: requestState,
    response,
    drafts,
    prompt: prompt as unknown as HTMLTextAreaElement,
    focusOwned: true,
  };
  return { prompt, focusOwner, drafts, request, response, input };
}

test("UTF-16 replacement preserves emoji and both sides for collapsed and nonempty selections", () => {
  const selected = replaceUtf16Selection(
    SOURCE_VALUE,
    PREFIX.length,
    PREFIX.length + SELECTED.length,
    INSERTION_TEXT,
  );
  assert.deepEqual(selected, {
    value: EXPECTED_VALUE,
    caret: PREFIX.length + INSERTION_TEXT.length,
  });
  assert.equal(selected?.value.match(/\/case /gu)?.length, 1);

  const collapsed = replaceUtf16Selection(SOURCE_VALUE, PREFIX.length, PREFIX.length, INSERTION_TEXT);
  assert.deepEqual(collapsed, {
    value: `${PREFIX}${INSERTION_TEXT}${SELECTED}右`,
    caret: PREFIX.length + INSERTION_TEXT.length,
  });
  assert.equal(replaceUtf16Selection(SOURCE_VALUE, -1, 0, INSERTION_TEXT), null);
  assert.equal(replaceUtf16Selection(SOURCE_VALUE, 0, SOURCE_VALUE.length + 1, INSERTION_TEXT), null);
  assert.equal(
    replaceUtf16Selection(SOURCE_VALUE, 2, 2, INSERTION_TEXT),
    null,
    "a caret inside the emoji surrogate pair is not a valid UTF-16 selection boundary",
  );
  assert.equal(
    replaceUtf16Selection(SOURCE_VALUE, 1, 2, INSERTION_TEXT),
    null,
    "a selection ending inside the emoji surrogate pair is rejected",
  );
});

test("request and canonical response settle one owner-bound local insertion", () => {
  const current = scenario();
  assert.equal(current.request.projectionRevision, "7");
  assert.equal(current.request.projectedDraftPrompt, "server-owned-draft");
  assert.equal(current.request.composerCommitGeneration, "11");
  assert.equal(current.request.selectionStart, PREFIX.length);
  assert.equal(current.request.selectionEnd, PREFIX.length + SELECTED.length);
  assert.equal(current.request.expectedInsertionText, INSERTION_TEXT);
  assert.deepEqual(current.request.expectedDraftTarget, current.input.currentState.draft_target);
  assert.deepEqual(current.request.expectedTarget, {
    workspacePath: "C:/workspace",
    ownerProjectId: "project-a",
    ownerSessionId: "session-a",
    rowId: "builtin:case",
  });

  const settlement = settleCommandPaletteInsertion(current.input);
  assert.ok(settlement);
  assert.equal(settlement.value, EXPECTED_VALUE);
  assert.equal(settlement.caret, PREFIX.length + INSERTION_TEXT.length);
  assert.equal(settlement.focusContinuation.composerRevision, 6);
  assert.equal(current.drafts.prompt, SOURCE_VALUE, "settlement itself has no hidden write");
});

test("settlement rejects stale owners, revisions, rows, edits, interactions, focus, and duplicates", () => {
  const cases: Array<{
    name: string;
    mutate: (value: Scenario) => void;
  }> = [
    { name: "duplicate or already consumed request", mutate: (value) => { value.input.activeRequest = null; } },
    { name: "new user interaction", mutate: (value) => { value.input.interactionGeneration += 1n; } },
    { name: "active interaction lifecycle", mutate: (value) => { value.input.interactionIdle = false; } },
    { name: "stale response projection", mutate: (value) => { value.input.projectionAccepted = false; } },
    { name: "focus moved", mutate: (value) => { value.input.focusOwned = false; } },
    {
      name: "owner generation A-B-A still has a newer projection revision",
      mutate: (value) => { value.input.currentState = webState({ revision: "9", ownerGeneration: "3" }); },
    },
    { name: "current owner drift", mutate: (value) => { value.input.currentState = webState({ ownerGeneration: "4" }); } },
    {
      name: "response owner drift",
      mutate: (value) => { value.response.state = webState({ revision: "8", overlay: "none", ownerGeneration: "4" }); },
    },
    { name: "current row drift", mutate: (value) => { value.input.currentState = webState({ rowPath: "builtin:other" }); } },
    {
      name: "response row drift",
      mutate: (value) => { value.response.state = webState({ revision: "8", overlay: "none", rowPath: "builtin:other" }); },
    },
    { name: "row owner drift", mutate: (value) => { value.input.currentState = webState({ projectId: "project-b" }); } },
    { name: "local revision edit", mutate: (value) => { value.drafts.composerRevision += 1; } },
    { name: "local value edit", mutate: (value) => { value.drafts.prompt += "!"; } },
    { name: "live textarea edit", mutate: (value) => { value.prompt.value += "!"; } },
    { name: "selection edit", mutate: (value) => { value.prompt.selectionEnd = value.prompt.selectionStart; } },
    { name: "prompt disconnected", mutate: (value) => { value.prompt.isConnected = false; } },
    { name: "prompt disabled", mutate: (value) => { value.prompt.disabled = true; } },
    {
      name: "palette focus owner disconnected",
      mutate: (value) => { (value.focusOwner as unknown as { isConnected: boolean }).isConnected = false; },
    },
    {
      name: "different prompt node",
      mutate: (value) => {
        value.input.prompt = new FakePrompt(SOURCE_VALUE, PREFIX.length, PREFIX.length + SELECTED.length) as unknown as HTMLTextAreaElement;
      },
    },
    { name: "palette closed before response", mutate: (value) => { value.input.currentState = webState({ overlay: "none" }); } },
    {
      name: "confirmation appeared",
      mutate: (value) => { value.input.currentState = webState({ confirmationVisible: true }); },
    },
    { name: "noncanonical insertion text", mutate: (value) => { value.response.insertionText = "/other "; } },
    {
      name: "server edited the Rust draft",
      mutate: (value) => {
        value.response.state = webState({ revision: "8", overlay: "none", draftPrompt: INSERTION_TEXT });
      },
    },
    {
      name: "command unexpectedly committed or sent the draft",
      mutate: (value) => {
        value.response.state = webState({ revision: "8", overlay: "none", composerCommitGeneration: "12" });
      },
    },
    {
      name: "response kept the palette open",
      mutate: (value) => { value.response.state = webState({ revision: "8", overlay: "command_palette" }); },
    },
    {
      name: "response opened a confirmation",
      mutate: (value) => {
        value.response.state = webState({ revision: "8", overlay: "none", confirmationVisible: true });
      },
    },
  ];

  for (const candidate of cases) {
    const current = scenario();
    candidate.mutate(current);
    assert.equal(settleCommandPaletteInsertion(current.input), null, candidate.name);
  }
});

function settledFocusScenario(): {
  prompt: FakePrompt;
  documentTarget: FakeDocument;
  continuation: NonNullable<ReturnType<typeof settleCommandPaletteInsertion>>["focusContinuation"];
  state: DesktopWebState;
  drafts: CommandPaletteDraftState;
} {
  const current = scenario();
  const settlement = settleCommandPaletteInsertion(current.input);
  assert.ok(settlement);
  current.prompt.value = settlement.value;
  current.prompt.selectionStart = 0;
  current.prompt.selectionEnd = 0;
  current.drafts.prompt = settlement.value;
  current.drafts.composerRevision = settlement.focusContinuation.composerRevision;
  const documentTarget = new FakeDocument(current.prompt);
  return {
    prompt: current.prompt,
    documentTarget,
    continuation: settlement.focusContinuation,
    state: current.response.state,
    drafts: current.drafts,
  };
}

test("focus continuation places the caret after insertion and preserves textarea scroll", () => {
  const current = settledFocusScenario();
  assert.equal(
    focusCommandPaletteInsertionIfCurrent(
      current.documentTarget as unknown as Document,
      current.continuation,
      23n,
      current.state,
      current.drafts,
    ),
    "focused",
  );
  assert.equal(current.documentTarget.activeElement, current.prompt);
  assert.equal(current.prompt.focusCalls, 1);
  assert.equal(current.prompt.selectionStart, PREFIX.length + INSERTION_TEXT.length);
  assert.equal(current.prompt.selectionEnd, PREFIX.length + INSERTION_TEXT.length);
  assert.equal(current.prompt.scrollLeft, 12);
  assert.equal(current.prompt.scrollTop, 24);

  assert.equal(
    focusCommandPaletteInsertionIfCurrent(
      current.documentTarget as unknown as Document,
      current.continuation,
      23n,
      current.state,
      current.drafts,
    ),
    "already-focused",
  );
});

test("command palette currentness, target resolution, and selection settlement are independent", () => {
  const current = settledFocusScenario();
  assert.equal(commandPaletteInsertionFocusStillCurrent(
    current.continuation,
    23n,
    current.state,
    current.drafts,
  ), true);
  assert.equal(commandPaletteInsertionFocusTarget(
    current.documentTarget as unknown as Document,
    current.continuation,
  ), current.prompt);
  assert.equal(current.documentTarget.activeElement, current.documentTarget.body);

  current.prompt.selectionStart = 0;
  current.prompt.selectionEnd = 0;
  current.prompt.scrollLeft = 0;
  current.prompt.scrollTop = 0;
  settleCommandPaletteInsertionSelectionAndScroll(current.continuation, current.prompt as unknown as HTMLTextAreaElement);
  assert.deepEqual(
    [current.prompt.selectionStart, current.prompt.selectionEnd],
    [PREFIX.length + INSERTION_TEXT.length, PREFIX.length + INSERTION_TEXT.length],
  );
  assert.deepEqual([current.prompt.scrollLeft, current.prompt.scrollTop], [12, 24]);
  assert.equal(current.prompt.focusCalls, 0);

  const candidate = commandPaletteInsertionFocusCandidates(
    current.documentTarget as unknown as Document,
    current.continuation,
  )[0];
  assert.equal(candidate?.resolve(), current.prompt);
});

test("focus continuation accepts a newer same-owner poll revision before the animation frame", () => {
  const current = settledFocusScenario();
  assert.equal(focusCommandPaletteInsertionIfCurrent(
    current.documentTarget as unknown as Document,
    current.continuation,
    23n,
    webState({ revision: "9", overlay: "none" }),
    current.drafts,
  ), "focused");
  assert.equal(current.prompt.selectionStart, PREFIX.length + INSERTION_TEXT.length);
  assert.equal(current.prompt.selectionEnd, PREFIX.length + INSERTION_TEXT.length);
});

test("focus continuation fails closed after interaction, owner, revision, node, or focus drift", () => {
  {
    const current = settledFocusScenario();
    assert.equal(focusCommandPaletteInsertionIfCurrent(
      current.documentTarget as unknown as Document,
      current.continuation,
      24n,
      current.state,
      current.drafts,
    ), "stale");
  }
  {
    const current = settledFocusScenario();
    assert.equal(focusCommandPaletteInsertionIfCurrent(
      current.documentTarget as unknown as Document,
      current.continuation,
      23n,
      webState({ revision: "7", overlay: "none" }),
      current.drafts,
    ), "stale");
  }
  {
    const current = settledFocusScenario();
    current.drafts.composerRevision += 1;
    assert.equal(focusCommandPaletteInsertionIfCurrent(
      current.documentTarget as unknown as Document,
      current.continuation,
      23n,
      current.state,
      current.drafts,
    ), "stale");
  }
  {
    const current = settledFocusScenario();
    current.prompt.isConnected = false;
    assert.equal(focusCommandPaletteInsertionIfCurrent(
      current.documentTarget as unknown as Document,
      current.continuation,
      23n,
      current.state,
      current.drafts,
    ), "unavailable");
  }
  {
    const current = settledFocusScenario();
    current.documentTarget.activeElement = fakeElement("unrelated-control");
    assert.equal(focusCommandPaletteInsertionIfCurrent(
      current.documentTarget as unknown as Document,
      current.continuation,
      23n,
      current.state,
      current.drafts,
    ), "owned");
  }
});

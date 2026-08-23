import assert from "node:assert/strict";
import test from "node:test";

import {
  beginMainRunFocusContinuation,
  mainRunFocusSurface,
  pointerTargetsMainRunControl,
  reconcileMainRunFocusContinuation,
} from "../src/run_focus_continuation.ts";
import type { DesktopViewState, RunExpectedState } from "../src/types.ts";
import {
  runtimeOwnerToken,
  SESSION_A,
  SESSION_B,
  turnIdForRuntimeEpoch,
  WORKSPACE_A,
} from "./canonical_wire_fixture.ts";

function expectedState(ownerToken: string): RunExpectedState {
  const [ownerKind, epoch] = ownerToken.split(":");
  return ownerKind === "root"
    ? { kind: "turn", turnId: turnIdForRuntimeEpoch(epoch), admissionRevision: epoch }
    : {
      kind: "idle",
      latestTurnId: epoch === "0" ? null : turnIdForRuntimeEpoch(epoch),
      admissionRevision: epoch,
    };
}

function runState(
  runtimeOwnerToken: string,
  overrides: Partial<DesktopViewState> = {},
): DesktopViewState {
  return {
    busy: false,
    agent_tree_active: false,
    can_submit: true,
    can_cancel_run: false,
    confirmation_visible: false,
    navigation_loading: false,
    overlay: "none",
    post_run_refresh_pending: false,
    run_status_key: "idle",
    run_target: {
      workspacePath: WORKSPACE_A,
      sessionId: SESSION_A,
      runtimeOwnerToken,
      permissionConfirmationId: null,
      expectedState: expectedState(runtimeOwnerToken),
    },
    ...overrides,
  } as DesktopViewState;
}

class FakeElement {
  disabled = false;
  readonly dataset: Record<string, string> = {};
  private readonly owner: FakeDocument;
  readonly kind: "body" | "root" | "prompt" | "action" | "side" | "other";

  constructor(
    owner: FakeDocument,
    kind: "body" | "root" | "prompt" | "action" | "side" | "other",
    action = "",
  ) {
    this.owner = owner;
    this.kind = kind;
    if (action) this.dataset.action = action;
  }

  matches(selector: string): boolean {
    if (selector === "#prompt") return this.kind === "prompt";
    if (selector === ":disabled") return this.disabled;
    return false;
  }

  closest(selector: string): FakeElement | null {
    if (
      this.kind === "side"
      && selector.includes(".side-chat-pane")
    ) {
      return this;
    }
    if (selector === "[data-action]" && this.dataset.action) return this;
    return null;
  }

  focus(): void {
    this.owner.activeElement = this;
  }
}

class FakeDocument {
  readonly body = new FakeElement(this, "body");
  readonly documentElement = new FakeElement(this, "root");
  readonly prompt = new FakeElement(this, "prompt");
  readonly send = new FakeElement(this, "action", "send");
  readonly stop = new FakeElement(this, "action", "cancel-run");
  readonly sidePrompt = new FakeElement(this, "side");
  readonly other = new FakeElement(this, "other");
  activeElement: FakeElement | null = this.body;

  querySelector(selector: string): FakeElement | null {
    return selector === "#prompt" ? this.prompt : null;
  }
}

function asDocument(value: FakeDocument): Document {
  return value as unknown as Document;
}

function asElement(value: FakeElement): Element {
  return value as unknown as Element;
}

test("pointer Send retains one exact run owner until terminal focus returns to the main composer", () => {
  const dom = new FakeDocument();
  const idle = runState(runtimeOwnerToken("idle", 0n));
  const pending = runState(runtimeOwnerToken("idle", 0n), { busy: true });
  dom.activeElement = dom.prompt;

  // The delegated click owns Send before a later replacement leaves BODY.
  const activation = beginMainRunFocusContinuation(idle, "send");
  assert.ok(activation);
  dom.activeElement = dom.body;

  let decision = reconcileMainRunFocusContinuation(
    activation,
    idle,
    pending,
    mainRunFocusSurface(asDocument(dom)),
  );
  assert.equal(decision.focusPrompt, false);
  assert.deepEqual(decision.continuation, {
    workspacePath: WORKSPACE_A,
    sessionId: SESSION_A,
    mayAdoptCreatedSession: false,
    startingEpoch: "0",
    runEpoch: null,
  });

  // Replacing the focused Send button with the running surface leaves the
  // document unowned; the exact root generation is adopted from Rust.
  dom.activeElement = dom.body;
  const running = runState(runtimeOwnerToken("root", 1n), {
    busy: true,
    can_cancel_run: true,
    run_status_key: "running",
  });
  decision = reconcileMainRunFocusContinuation(
    decision.continuation,
    pending,
    running,
    mainRunFocusSurface(asDocument(dom)),
  );
  assert.equal(decision.continuation?.runEpoch, "1");
  assert.equal(decision.focusPrompt, false);

  // The running surface exposes Stop, then its canonical terminal projection
  // removes that transient control and returns the now-idle generation.
  assert.equal(pointerTargetsMainRunControl(asElement(dom.stop)), true);
  const terminal = runState(runtimeOwnerToken("idle", 1n), { run_status_key: "completed" });
  decision = reconcileMainRunFocusContinuation(
    decision.continuation,
    running,
    terminal,
    mainRunFocusSurface(asDocument(dom)),
  );
  assert.equal(decision.continuation, null);
  assert.equal(decision.focusPrompt, true);
});

test("click-time Send ownership survives the actual stale rendered-state admission sequence", () => {
  const dom = new FakeDocument();
  const noSessionTarget = {
    workspacePath: WORKSPACE_A,
    sessionId: null,
    runtimeOwnerToken: runtimeOwnerToken("idle", 0n),
    permissionConfirmationId: null,
    expectedState: expectedState(runtimeOwnerToken("idle", 0n)),
  };
  assert.deepEqual(noSessionTarget.expectedState, {
    kind: "idle",
    latestTurnId: null,
    admissionRevision: "0",
  });
  // Prompt input updates the local draft and live button without rerendering,
  // so the click sees can_submit=true while lastRenderedState remains false.
  const staleRendered = runState(runtimeOwnerToken("idle", 0n), {
    can_submit: false,
    run_target: noSessionTarget,
  });
  const freshClickState = runState(runtimeOwnerToken("idle", 0n), {
    can_submit: true,
    run_target: noSessionTarget,
  });
  const activation = beginMainRunFocusContinuation(freshClickState, "send");
  assert.ok(activation);

  // Native pointer focus still owns the old Send node when the optimistic
  // pending render reconciles. It must not rederive from staleRendered.
  dom.activeElement = dom.send;
  const pending = runState(runtimeOwnerToken("idle", 0n), {
    busy: true,
    can_submit: false,
    run_target: noSessionTarget,
  });
  let decision = reconcileMainRunFocusContinuation(
    activation,
    staleRendered,
    pending,
    mainRunFocusSurface(asDocument(dom)),
  );
  assert.deepEqual(decision.continuation, {
    workspacePath: WORKSPACE_A,
    sessionId: null,
    mayAdoptCreatedSession: true,
    startingEpoch: "0",
    runEpoch: null,
  });
  assert.equal(decision.focusPrompt, false);

  dom.activeElement = dom.body;
  const running = runState(runtimeOwnerToken("root", 1n), {
    busy: true,
    can_submit: false,
    can_cancel_run: true,
    run_status_key: "running",
    run_target: {
      ...noSessionTarget,
      sessionId: SESSION_B,
      runtimeOwnerToken: runtimeOwnerToken("root", 1n),
      expectedState: expectedState(runtimeOwnerToken("root", 1n)),
    },
  });
  decision = reconcileMainRunFocusContinuation(
    decision.continuation,
    pending,
    running,
    mainRunFocusSurface(asDocument(dom)),
  );
  assert.equal(decision.continuation?.sessionId, SESSION_B);
  assert.equal(decision.continuation?.runEpoch, "1");

  const terminal = runState(runtimeOwnerToken("idle", 1n), {
    run_status_key: "completed",
    run_target: {
      ...running.run_target,
      runtimeOwnerToken: runtimeOwnerToken("idle", 1n),
      expectedState: expectedState(runtimeOwnerToken("idle", 1n)),
    },
  });
  decision = reconcileMainRunFocusContinuation(
    decision.continuation,
    running,
    terminal,
    mainRunFocusSurface(asDocument(dom)),
  );
  assert.equal(decision.continuation, null);
  assert.equal(decision.focusPrompt, true);
});

test("a delegated Stop click also returns to the composer after the same run is cancelled", () => {
  const dom = new FakeDocument();
  const running = runState(runtimeOwnerToken("root", 12n), {
    busy: true,
    can_cancel_run: true,
    run_status_key: "running",
  });
  const cancelled = runState(runtimeOwnerToken("idle", 12n), { run_status_key: "cancelled" });
  const activation = beginMainRunFocusContinuation(running, "cancel-run");
  assert.ok(activation);
  dom.activeElement = dom.body;

  const decision = reconcileMainRunFocusContinuation(
    activation,
    running,
    cancelled,
    mainRunFocusSurface(asDocument(dom)),
  );
  assert.equal(decision.focusPrompt, true);
});

test("child-only tree activity can start a new main run and returns focus at root terminal", () => {
  const dom = new FakeDocument();
  const childOnly = runState(runtimeOwnerToken("tree", 30n), {
    agent_tree_active: true,
    can_submit: true,
  });
  const pending = runState(runtimeOwnerToken("tree", 30n), {
    busy: true,
    agent_tree_active: true,
    can_submit: false,
  });
  dom.activeElement = dom.send;
  const activation = beginMainRunFocusContinuation(childOnly, "send");
  assert.ok(activation);
  let decision = reconcileMainRunFocusContinuation(
    activation,
    childOnly,
    pending,
    mainRunFocusSurface(asDocument(dom)),
  );
  assert.equal(decision.continuation?.startingEpoch, "30");
  assert.equal(decision.continuation?.runEpoch, null);

  dom.activeElement = dom.body;
  const running = runState(runtimeOwnerToken("root", 31n), {
    busy: true,
    agent_tree_active: true,
    can_cancel_run: true,
    can_submit: false,
    run_status_key: "running",
  });
  decision = reconcileMainRunFocusContinuation(
    decision.continuation,
    pending,
    running,
    mainRunFocusSurface(asDocument(dom)),
  );
  assert.equal(decision.continuation?.runEpoch, "31");

  const rootTerminalWithDetachedChild = runState(runtimeOwnerToken("tree", 31n), {
    agent_tree_active: true,
    can_submit: true,
    run_status_key: "completed",
  });
  decision = reconcileMainRunFocusContinuation(
    decision.continuation,
    running,
    rootTerminalWithDetachedChild,
    mainRunFocusSurface(asDocument(dom)),
  );
  assert.equal(decision.continuation, null);
  assert.equal(decision.focusPrompt, true);
});

test("focus continuation is discarded when the user moves to Side Chat or another owner", () => {
  const dom = new FakeDocument();
  const idle = runState(runtimeOwnerToken("idle", 3n));
  const pending = runState(runtimeOwnerToken("idle", 3n), { busy: true });
  dom.activeElement = dom.send;
  const activation = beginMainRunFocusContinuation(idle, "send");
  assert.ok(activation);
  const armed = reconcileMainRunFocusContinuation(
    activation,
    idle,
    pending,
    mainRunFocusSurface(asDocument(dom)),
  );
  assert.ok(armed.continuation);

  const running = runState(runtimeOwnerToken("root", 4n), {
    busy: true,
    can_cancel_run: true,
    run_status_key: "running",
  });
  dom.activeElement = dom.sidePrompt;
  const moved = reconcileMainRunFocusContinuation(
    armed.continuation,
    pending,
    running,
    mainRunFocusSurface(asDocument(dom)),
  );
  assert.equal(moved.continuation, null);
  assert.equal(moved.focusPrompt, false);
  assert.equal(dom.activeElement, dom.sidePrompt);

  dom.activeElement = dom.body;
  const wrongWorkspace = reconcileMainRunFocusContinuation(
    armed.continuation,
    pending,
    runState(runtimeOwnerToken("root", 4n), {
      busy: true,
      run_target: {
        ...running.run_target,
        workspacePath: "C:/other",
      },
    }),
    mainRunFocusSurface(asDocument(dom)),
  );
  assert.equal(wrongWorkspace.continuation, null);
  assert.equal(wrongWorkspace.focusPrompt, false);
  assert.equal(pointerTargetsMainRunControl(asElement(dom.other)), false);
});

test("focused main controls never synthesize an activation owner", () => {
  const dom = new FakeDocument();
  const idle = runState(runtimeOwnerToken("idle", 40n));
  const pending = runState(runtimeOwnerToken("idle", 40n), { busy: true, can_submit: false });
  dom.activeElement = dom.send;
  let decision = reconcileMainRunFocusContinuation(
    null,
    idle,
    pending,
    mainRunFocusSurface(asDocument(dom)),
  );
  assert.equal(decision.continuation, null);
  assert.equal(decision.focusPrompt, false);

  const running = runState(runtimeOwnerToken("root", 41n), {
    busy: true,
    can_submit: false,
    can_cancel_run: true,
    run_status_key: "running",
  });
  dom.activeElement = dom.stop;
  decision = reconcileMainRunFocusContinuation(
    null,
    running,
    runState(runtimeOwnerToken("idle", 41n), { run_status_key: "completed" }),
    mainRunFocusSurface(asDocument(dom)),
  );
  assert.equal(decision.continuation, null);
  assert.equal(decision.focusPrompt, false);
});

test("Ctrl+Enter from the composer leaves focus ownership with the existing prompt", () => {
  const dom = new FakeDocument();
  const idle = runState(runtimeOwnerToken("idle", 20n));
  const pending = runState(runtimeOwnerToken("idle", 20n), { busy: true });
  dom.activeElement = dom.prompt;

  const decision = reconcileMainRunFocusContinuation(
    null,
    idle,
    pending,
    mainRunFocusSurface(asDocument(dom)),
  );
  assert.equal(decision.continuation, null);
  assert.equal(decision.focusPrompt, false);
  assert.equal(dom.activeElement, dom.prompt);
});

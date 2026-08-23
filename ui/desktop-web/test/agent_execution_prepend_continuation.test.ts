import assert from "node:assert/strict";
import test from "node:test";

import {
  agentExecutionPrependFocusCandidates,
  agentExecutionPreviousFocusKey,
  beginAgentExecutionPrependContinuation,
  reconcileAgentExecutionPrependContinuation,
  restoreAgentExecutionPrependViewport,
} from "../src/agent_execution_prepend_continuation.ts";
import { renderAgentInspector } from "../src/render_agent_activity.ts";
import type {
  AgentActivityRow,
  AgentExecutionExpectedTarget,
  AgentExecutionProjection,
  DesktopViewState,
} from "../src/types.ts";
import type { AgentExecutionCacheEntry, AgentExecutionRequest } from "../src/ui_state.ts";

const target: AgentExecutionExpectedTarget = {
  workspacePath: "C:/workspace",
  rootSessionId: "root-session",
  agentPath: "/root/history",
  childSessionId: "child-session",
};

function row(overrides: Partial<AgentActivityRow> = {}): AgentActivityRow {
  return {
    agent_path: target.agentPath,
    session_id: target.childSessionId,
    task_name: "History",
    task_preview: "History",
    status: "completed",
    current_activity: "",
    result_preview: "done",
    started_order: 1,
    updated: false,
    active_turn_id: null,
    interrupt_target: null,
    ...overrides,
  };
}

function projection(): AgentExecutionProjection {
  return {
    workspace_path: target.workspacePath,
    root_session_id: target.rootSessionId,
    agent_path: target.agentPath,
    session_id: target.childSessionId,
    task_name: "History",
    transcript_rows: [{
      row_kind: "assistant",
      step: "newer",
      title: "Assistant",
      body: "newer",
      file_changes: [],
    }],
    turn_page_offset: 80,
    turn_page_end: 160,
    turn_page_total: 160,
    turn_page_has_previous: true,
  };
}

function state(overrides: Partial<DesktopViewState> = {}): DesktopViewState {
  return {
    workspace_path: target.workspacePath,
    draft_target: {
      workspacePath: target.workspacePath,
      sessionId: target.rootSessionId,
      ownerGeneration: "7",
    },
    agent_activity_rows: [row()],
    agent_tree_active: false,
    ...overrides,
  } as DesktopViewState;
}

function entry(
  status: AgentExecutionCacheEntry["status"],
  generation = 11,
): AgentExecutionCacheEntry {
  return {
    status,
    generation,
    expectedTarget: target,
    projection: projection(),
    error: status === "error" ? "failed" : "",
  };
}

function request(): AgentExecutionRequest {
  return {
    cacheKey: "cache",
    generation: 11,
    ownerIdentity: "owner",
    expectedTarget: target,
    activityIdentity: "activity",
    operation: "prepend",
    expectedOffset: 80,
    expectedEnd: 160,
  };
}

class FakeElement {
  readonly dataset: Record<string, string> = {};
  readonly children: FakeElement[] = [];
  hidden = false;
  disabled = false;
  inert = false;
  scrollTop = 0;
  scrollHeight = 0;
  readonly kind: "root" | "body" | "section" | "scroll" | "anchor" | "trigger" | "other";
  top = 0;
  bottom = 0;
  private readonly owner: FakeDocument;
  private parent: FakeElement | null = null;

  constructor(
    owner: FakeDocument,
    kind: FakeElement["kind"],
    dataset: Record<string, string> = {},
  ) {
    this.owner = owner;
    this.kind = kind;
    Object.assign(this.dataset, dataset);
  }

  append(child: FakeElement): void {
    child.parent = this;
    this.children.push(child);
  }

  querySelector(selector: string): FakeElement | null {
    return this.querySelectorAll(selector)[0] ?? null;
  }

  querySelectorAll(selector: string): FakeElement[] {
    const descendants = this.children.flatMap((child) => [child, ...child.descendants()]);
    if (selector === ".agent-execution-scroll") {
      return descendants.filter((candidate) => candidate.kind === "scroll");
    }
    if (selector === "[data-history-anchor]") {
      return descendants.filter((candidate) => candidate.dataset.historyAnchor !== undefined);
    }
    if (selector === "[data-focus-key]") {
      return descendants.filter((candidate) => candidate.dataset.focusKey !== undefined);
    }
    return [];
  }

  getBoundingClientRect(): DOMRect {
    return { top: this.top, bottom: this.bottom } as DOMRect;
  }

  matches(selector: string): boolean {
    return selector === ":disabled" && this.disabled;
  }

  getAttribute(name: string): string | null {
    if (name === "aria-disabled") return this.disabled ? "true" : null;
    return null;
  }

  closest(selector: string): FakeElement | null {
    if (!selector.includes("[inert]") && !selector.includes("[aria-hidden='true']")) return null;
    let candidate: FakeElement | null = this;
    while (candidate) {
      if (candidate.inert) return candidate;
      candidate = candidate.parent;
    }
    return null;
  }

  focus(): void {
    this.owner.activeElement = this;
  }

  private descendants(): FakeElement[] {
    return this.children.flatMap((child) => [child, ...child.descendants()]);
  }
}

class FakeDocument {
  readonly body = new FakeElement(this, "body");
  readonly documentElement = new FakeElement(this, "root");
  readonly other = new FakeElement(this, "other");
  readonly section: FakeElement;
  readonly scroll: FakeElement;
  readonly trigger: FakeElement | null;
  activeElement: FakeElement | null;

  constructor(withTrigger = true, anchorTop = 140) {
    this.section = new FakeElement(this, "section", {
      agentPath: target.agentPath,
      focusKey: `agent-execution:${target.agentPath}`,
    });
    this.scroll = new FakeElement(this, "scroll");
    this.scroll.top = 100;
    this.scroll.bottom = 400;
    this.scroll.scrollTop = 300;
    this.scroll.scrollHeight = 900;
    const anchor = new FakeElement(this, "anchor", { historyAnchor: "stable-anchor" });
    anchor.top = anchorTop;
    anchor.bottom = anchorTop + 60;
    this.scroll.append(anchor);
    this.section.append(this.scroll);
    this.trigger = withTrigger
      ? new FakeElement(this, "trigger", {
        focusKey: agentExecutionPreviousFocusKey(target.agentPath),
      })
      : null;
    if (this.trigger) this.section.append(this.trigger);
    this.activeElement = this.trigger ?? this.body;
  }

  querySelectorAll(selector: string): FakeElement[] {
    if (selector === "section.agent-execution[data-focus-key][data-agent-path]") {
      return [this.section];
    }
    return [];
  }
}

function asDocument(value: FakeDocument): Document {
  return value as unknown as Document;
}

test("prepend activation captures the exact generation owner, visible anchor offset, and focus return", () => {
  const dom = new FakeDocument();
  const continuation = beginAgentExecutionPrependContinuation(asDocument(dom), request());
  assert.ok(continuation);
  assert.deepEqual(continuation.owner, { ...target, generation: 11 });
  assert.equal(continuation.viewport.candidates[0]?.id, "stable-anchor");
  assert.equal(continuation.viewport.candidates[0]?.offsetTop, 40);
  assert.equal(continuation.returnFocus, true);

  const loading = reconcileAgentExecutionPrependContinuation(
    continuation,
    state(),
    target.agentPath,
    entry("loading"),
  );
  assert.equal(loading.continuation, continuation);
  assert.equal(loading.restoreViewport, continuation);
  assert.equal(loading.restoreFocus, continuation);

  const settled = reconcileAgentExecutionPrependContinuation(
    continuation,
    state(),
    target.agentPath,
    entry("ready"),
  );
  assert.equal(settled.continuation, null);
  assert.equal(settled.restoreViewport, continuation);
  assert.equal(settled.restoreFocus, continuation);
});

test("prepend viewport restoration follows the stable suffix anchor instead of raw scrollTop", () => {
  const before = new FakeDocument();
  const continuation = beginAgentExecutionPrependContinuation(asDocument(before), request());
  assert.ok(continuation);

  const after = new FakeDocument(true, 260);
  after.scroll.scrollTop = 300;
  assert.equal(restoreAgentExecutionPrependViewport(asDocument(after), continuation), true);
  assert.equal(
    after.scroll.scrollTop,
    420,
    "the 120px prepend delta keeps the same anchor at its original 40px viewport offset",
  );
});

test("agent execution prepend exports trigger then exact-section candidates without focusing", () => {
  const before = new FakeDocument();
  const continuation = beginAgentExecutionPrependContinuation(asDocument(before), request());
  assert.ok(continuation);
  before.activeElement = before.body;

  const candidates = agentExecutionPrependFocusCandidates(asDocument(before), continuation);
  assert.equal(candidates.length, 2);
  assert.equal(candidates[0]?.resolve(), before.trigger);
  assert.equal(candidates[1]?.resolve(), before.section);
  assert.equal(before.activeElement, before.body);

  const firstPage = new FakeDocument(false);
  const firstPageCandidates = agentExecutionPrependFocusCandidates(
    asDocument(firstPage),
    continuation,
  );
  assert.equal(firstPageCandidates[0]?.resolve(), null);
  assert.equal(firstPageCandidates[1]?.resolve(), firstPage.section);
});

test("owner, generation, and error guards discard stale restoration", () => {
  const before = new FakeDocument();
  const continuation = beginAgentExecutionPrependContinuation(asDocument(before), request());
  assert.ok(continuation);

  const mismatches: Array<[DesktopViewState, string | null, AgentExecutionCacheEntry | null]> = [
    [state({ workspace_path: "C:/other" }), target.agentPath, entry("ready")],
    [state({ draft_target: { workspacePath: target.workspacePath, sessionId: "other-root", ownerGeneration: "8" } }), target.agentPath, entry("ready")],
    [state(), "/root/other", entry("ready")],
    [state({ agent_activity_rows: [row({ session_id: "other-child" })] }), target.agentPath, entry("ready")],
    [state(), target.agentPath, entry("ready", 12)],
    [state(), target.agentPath, entry("error")],
  ];
  for (const [nextState, selectedPath, cache] of mismatches) {
    assert.deepEqual(
      reconcileAgentExecutionPrependContinuation(
        continuation,
        nextState,
        selectedPath,
        cache,
      ),
      { continuation: null, restoreViewport: null, restoreFocus: null },
    );
  }

});

test("the rendered previous-page control exposes the stable focus key and loading semantics", () => {
  const ready = entry("ready");
  const html = renderAgentInspector(state(), target.agentPath, ready);
  assert.match(
    html,
    new RegExp(`data-focus-key="${agentExecutionPreviousFocusKey(target.agentPath)}"`),
  );

  const loading = renderAgentInspector(state(), target.agentPath, entry("loading"));
  assert.match(
    loading,
    /data-action="load-previous-agent-execution-page"[\s\S]*?disabled aria-disabled="true"/,
  );
});

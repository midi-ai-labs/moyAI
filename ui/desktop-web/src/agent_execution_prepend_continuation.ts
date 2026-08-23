import {
  captureViewportAnchor,
  restoreViewportAnchor,
  type ViewportAnchorSnapshot,
} from "./history_navigation.ts";
import type { FocusTargetCandidate } from "./focus_arbiter.ts";
import type {
  AgentExecutionExpectedTarget,
  AgentExecutionProjection,
  DesktopViewState,
} from "./types.ts";

type AgentExecutionPrependState = Pick<
  DesktopViewState,
  "agent_activity_rows" | "draft_target" | "workspace_path"
>;

interface AgentExecutionPrependRequest {
  generation: number;
  operation: "replace" | "prepend";
  expectedTarget: AgentExecutionExpectedTarget;
}

interface AgentExecutionPrependCacheEntry {
  status: "loading" | "ready" | "error";
  generation: number;
  expectedTarget: AgentExecutionExpectedTarget;
  projection: AgentExecutionProjection | null;
}

export interface AgentExecutionPrependOwner {
  workspacePath: string;
  rootSessionId: string;
  agentPath: string;
  childSessionId: string;
  generation: number;
}

export interface AgentExecutionPrependContinuation {
  owner: AgentExecutionPrependOwner;
  viewport: ViewportAnchorSnapshot;
  returnFocus: boolean;
}

export interface AgentExecutionPrependDecision {
  continuation: AgentExecutionPrependContinuation | null;
  restoreViewport: AgentExecutionPrependContinuation | null;
  restoreFocus: AgentExecutionPrependContinuation | null;
}

export function agentExecutionPreviousFocusKey(agentPath: string): string {
  return `agent-execution-previous:${agentPath}`;
}

export function beginAgentExecutionPrependContinuation(
  documentTarget: Document,
  request: AgentExecutionPrependRequest,
): AgentExecutionPrependContinuation | null {
  if (request.operation !== "prepend") return null;
  const section = agentExecutionSection(documentTarget, request.expectedTarget.agentPath);
  const scroll = section?.querySelector<HTMLElement>(".agent-execution-scroll") ?? null;
  if (!section || !scroll) return null;
  const viewport = captureViewportAnchor(scroll);
  if (!viewport) return null;

  const trigger = agentExecutionPreviousTrigger(section, request.expectedTarget.agentPath);
  return {
    owner: {
      workspacePath: request.expectedTarget.workspacePath,
      rootSessionId: request.expectedTarget.rootSessionId,
      agentPath: request.expectedTarget.agentPath,
      childSessionId: request.expectedTarget.childSessionId,
      generation: request.generation,
    },
    viewport,
    returnFocus: trigger !== null && documentTarget.activeElement === trigger,
  };
}

export function reconcileAgentExecutionPrependContinuation(
  current: AgentExecutionPrependContinuation | null,
  state: AgentExecutionPrependState,
  selectedAgentPath: string | null,
  execution: AgentExecutionPrependCacheEntry | null,
): AgentExecutionPrependDecision {
  if (!current || !agentExecutionPrependOwnerMatches(
    current,
    state,
    selectedAgentPath,
    execution,
  )) {
    return emptyAgentExecutionPrependDecision();
  }
  if (execution?.status === "error") return emptyAgentExecutionPrependDecision();
  if (execution?.status === "loading") {
    return {
      continuation: current,
      restoreViewport: current,
      restoreFocus: current.returnFocus ? current : null,
    };
  }
  if (execution?.status === "ready") {
    return {
      continuation: null,
      restoreViewport: current,
      restoreFocus: current.returnFocus ? current : null,
    };
  }
  return emptyAgentExecutionPrependDecision();
}

export function agentExecutionPrependOwnerMatches(
  continuation: AgentExecutionPrependContinuation,
  state: AgentExecutionPrependState,
  selectedAgentPath: string | null,
  execution: AgentExecutionPrependCacheEntry | null,
): boolean {
  const { owner } = continuation;
  const row = state.agent_activity_rows.find(
    (candidate) => candidate.agent_path === owner.agentPath,
  );
  if (
    state.workspace_path !== owner.workspacePath
    || state.draft_target.workspacePath !== owner.workspacePath
    || state.draft_target.sessionId !== owner.rootSessionId
    || selectedAgentPath !== owner.agentPath
    || row?.session_id !== owner.childSessionId
    || execution?.generation !== owner.generation
    || !sameExpectedTarget(execution.expectedTarget, owner)
  ) {
    return false;
  }
  const projection = execution.projection;
  return projection !== null
    && projection.workspace_path === owner.workspacePath
    && projection.root_session_id === owner.rootSessionId
    && projection.agent_path === owner.agentPath
    && projection.session_id === owner.childSessionId;
}

export function restoreAgentExecutionPrependViewport(
  documentTarget: Document,
  continuation: AgentExecutionPrependContinuation,
): boolean {
  const section = agentExecutionSection(documentTarget, continuation.owner.agentPath);
  const scroll = section?.querySelector<HTMLElement>(".agent-execution-scroll") ?? null;
  return scroll !== null && restoreViewportAnchor(scroll, continuation.viewport);
}

/** Resolve the same-agent trigger then its section fallback without moving focus. */
export function agentExecutionPrependFocusCandidates(
  documentTarget: Document,
  continuation: AgentExecutionPrependContinuation,
): readonly FocusTargetCandidate[] {
  if (!continuation.returnFocus) return [];
  return [
    {
      resolve: () => {
        const section = agentExecutionSection(documentTarget, continuation.owner.agentPath);
        return section
          ? agentExecutionPreviousTrigger(section, continuation.owner.agentPath)
          : null;
      },
    },
    {
      resolve: () => agentExecutionSection(documentTarget, continuation.owner.agentPath),
    },
  ];
}

function agentExecutionSection(
  documentTarget: Document,
  agentPath: string,
): HTMLElement | null {
  const focusKey = `agent-execution:${agentPath}`;
  return Array.from(
    documentTarget.querySelectorAll<HTMLElement>(
      "section.agent-execution[data-focus-key][data-agent-path]",
    ),
  ).find((candidate) => (
    candidate.dataset.focusKey === focusKey
    && candidate.dataset.agentPath === agentPath
  )) ?? null;
}

function agentExecutionPreviousTrigger(
  section: HTMLElement,
  agentPath: string,
): HTMLElement | null {
  const focusKey = agentExecutionPreviousFocusKey(agentPath);
  return Array.from(section.querySelectorAll<HTMLElement>("[data-focus-key]"))
    .find((candidate) => candidate.dataset.focusKey === focusKey) ?? null;
}

function sameExpectedTarget(
  target: AgentExecutionExpectedTarget,
  owner: AgentExecutionPrependOwner,
): boolean {
  return target.workspacePath === owner.workspacePath
    && target.rootSessionId === owner.rootSessionId
    && target.agentPath === owner.agentPath
    && target.childSessionId === owner.childSessionId;
}

function emptyAgentExecutionPrependDecision(): AgentExecutionPrependDecision {
  return { continuation: null, restoreViewport: null, restoreFocus: null };
}

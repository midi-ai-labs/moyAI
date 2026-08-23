import type { PermissionDecisionState } from "./decision_state.ts";
import type { LocalConfirmation } from "./render_overlays.ts";
import type { DesktopViewState } from "./types.ts";
import type {
  AgentExecutionCacheEntry,
  ArtifactPaneMode,
  SideChatCatalogView,
  SideChatDeleteConfirmation,
  UiRecoverableError,
} from "./ui_state.ts";

/**
 * Local presentation values consumed while producing Desktop markup.
 *
 * This deliberately contains values, not their mutable owners. Maps, promises,
 * DOM nodes, counters used only for async ownership, and focus continuations do
 * not belong in the render model.
 */
export interface DesktopRenderLocalPresentation {
  readonly artifactPane: {
    readonly collapsed: boolean;
    readonly mode: ArtifactPaneMode;
    readonly selectedAgentPath: string | null;
    readonly selectedAgentExecution: AgentExecutionCacheEntry | null;
  };
  readonly attachmentTrayOpen: boolean;
  readonly configMutationPending: boolean;
  readonly sideChat: {
    readonly draft: string;
    readonly setupBaseUrl: string;
    readonly setupModel: string;
    readonly catalog: SideChatCatalogView;
    readonly catalogLoadEnabled: boolean;
    readonly mutationPending: boolean;
    readonly operationsOpen: boolean;
    readonly deleteConfirmation: SideChatDeleteConfirmation | null;
  };
  readonly modal: {
    readonly localConfirmation: LocalConfirmation | null;
    readonly localDecisionPending: boolean;
    readonly localDecisionError: string;
    readonly permissionDecision: PermissionDecisionState | null;
  };
  readonly recoverableError: UiRecoverableError | null;
  readonly windowMaximized: boolean;
}

/**
 * The complete immutable input identity for a Desktop render pass.
 *
 * `view` is the reconciled TypeScript view projection. `comparisonKey` owns
 * automatic invalidation and intentionally ignores only the Rust ordering
 * revision. The model does not become a second state owner: it is a snapshot of
 * the values already owned by Rust or UiLocalState.
 */
export interface DesktopRenderModel {
  readonly view: Readonly<DesktopViewState>;
  readonly local: Readonly<DesktopRenderLocalPresentation>;
  readonly comparisonKey: string;
}

export const DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION: Readonly<DesktopRenderLocalPresentation>
  = snapshotLocalPresentation({
    artifactPane: {
      collapsed: false,
      mode: "output",
      selectedAgentPath: null,
      selectedAgentExecution: null,
    },
    attachmentTrayOpen: false,
    configMutationPending: false,
    sideChat: {
      draft: "",
      setupBaseUrl: "",
      setupModel: "",
      catalog: {
        status: "idle",
        source: "none",
        ownerSessionId: null,
        baseUrl: "",
        models: [],
        error: "",
      },
      catalogLoadEnabled: false,
      mutationPending: false,
      operationsOpen: true,
      deleteConfirmation: null,
    },
    modal: {
      localConfirmation: null,
      localDecisionPending: false,
      localDecisionError: "",
      permissionDecision: null,
    },
    recoverableError: null,
    windowMaximized: false,
  });

type RenderRelevantView = Omit<DesktopViewState, "projection_revision">;

export function createDesktopRenderModel(
  view: DesktopViewState,
  local: DesktopRenderLocalPresentation,
): DesktopRenderModel {
  const viewSnapshot = immutableSnapshot(view);
  const { projection_revision: _orderingRevision, ...renderRelevantView } = viewSnapshot;
  const localSnapshot = snapshotLocalPresentation(local);
  const comparisonKey = JSON.stringify({
    view: renderRelevantView satisfies RenderRelevantView,
    local: localSnapshot,
  });
  return Object.freeze({
    view: viewSnapshot,
    local: localSnapshot,
    comparisonKey,
  });
}

/** True for the first model or when any render input except revision changed. */
export function desktopRenderModelChanged(
  previous: DesktopRenderModel | null,
  next: DesktopRenderModel,
): boolean {
  return previous === null || previous.comparisonKey !== next.comparisonKey;
}

/**
 * `forceRender` is reserved for imperative render-phase work such as a timed
 * splash transition, focus continuation, or an explicit local rerender. Normal
 * projection polling should pass false and rely on the model comparison.
 */
export function desktopRenderRequired(
  previous: DesktopRenderModel | null,
  next: DesktopRenderModel,
  forceRender: boolean,
): boolean {
  return forceRender || desktopRenderModelChanged(previous, next);
}

function snapshotLocalPresentation(
  local: DesktopRenderLocalPresentation,
): Readonly<DesktopRenderLocalPresentation> {
  const snapshot: DesktopRenderLocalPresentation = {
    artifactPane: {
      collapsed: local.artifactPane.collapsed,
      mode: local.artifactPane.mode,
      selectedAgentPath: local.artifactPane.selectedAgentPath,
      selectedAgentExecution: local.artifactPane.selectedAgentExecution,
    },
    attachmentTrayOpen: local.attachmentTrayOpen,
    configMutationPending: local.configMutationPending,
    sideChat: {
      draft: local.sideChat.draft,
      setupBaseUrl: local.sideChat.setupBaseUrl,
      setupModel: local.sideChat.setupModel,
      catalog: local.sideChat.catalog,
      catalogLoadEnabled: local.sideChat.catalogLoadEnabled,
      mutationPending: local.sideChat.mutationPending,
      operationsOpen: local.sideChat.operationsOpen,
      deleteConfirmation: local.sideChat.deleteConfirmation,
    },
    modal: {
      localConfirmation: local.modal.localConfirmation,
      localDecisionPending: local.modal.localDecisionPending,
      localDecisionError: local.modal.localDecisionError,
      permissionDecision: local.modal.permissionDecision,
    },
    recoverableError: local.recoverableError,
    windowMaximized: local.windowMaximized,
  };
  return immutableSnapshot(snapshot);
}

function immutableSnapshot<T>(value: T): T {
  if (Array.isArray(value)) {
    return Object.freeze(value.map((entry) => immutableSnapshot(entry))) as T;
  }
  if (value !== null && typeof value === "object") {
    const result: Record<string, unknown> = {};
    for (const [key, nested] of Object.entries(value as Record<string, unknown>)) {
      result[key] = immutableSnapshot(nested);
    }
    return Object.freeze(result) as T;
  }
  return value;
}

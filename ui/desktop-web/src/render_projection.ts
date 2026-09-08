import type { PermissionDecisionState } from "./decision_state.ts";
import { createHubUiState, hubPresentation, type HubPresentation } from "./hub_state.ts";
import { createDeviceNetworkUiState, deviceNetworkPresentation, type DeviceNetworkPresentation } from "./device_network_state.ts";
import { createPublishUiState, publishPresentation, type PublishPresentation } from "./mcp_publish_state.ts";
import { createMcpHistoryUiState, mcpHistoryPresentation, type McpHistoryPresentation } from "./mcp_history_state.ts";
import { createMcpPeerState, mcpPeerPresentation, type McpPeerPresentation } from "./mcp_peer.ts";
import type { LocalConfirmation } from "./render_overlays.ts";
import type { DesktopViewState } from "./types.ts";
import type {
  InitialSetupAuxiliaryKind,
} from "./initial_setup_auxiliary_state.ts";
import type {
  InitialSetupDiffEntry,
  InitialSetupStep,
} from "./initial_setup_state.ts";
import type {
  SessionSettingsDraft,
  SessionSettingsValidation,
} from "./session_settings_state.ts";
import type {
  AgentExecutionCacheEntry,
  ArtifactPaneMode,
  SessionSettingsMutationAvailability,
  SideChatCatalogView,
  SideChatDeleteConfirmation,
  UiRecoverableError,
} from "./ui_state.ts";
import type { SideChatPendingQuote } from "./types.ts";

/**
 * Local presentation values consumed while producing Desktop markup.
 *
 * This deliberately contains values, not their mutable owners. Maps, promises,
 * DOM nodes, counters used only for async ownership, and focus continuations do
 * not belong in the render model.
 */
export interface DesktopRenderLocalPresentation {
  readonly hub: HubPresentation;
  readonly deviceNetwork: DeviceNetworkPresentation;
  readonly mcpPublish: PublishPresentation;
  readonly mcpHistory: McpHistoryPresentation;
  readonly mcpPeers: McpPeerPresentation;
  readonly artifactPane: {
    readonly collapsed: boolean;
    readonly mode: ArtifactPaneMode;
    readonly selectedAgentPath: string | null;
    readonly selectedAgentExecution: AgentExecutionCacheEntry | null;
  };
  readonly attachmentTrayOpen: boolean;
  readonly configMutationPending: boolean;
  readonly doclingReadinessRequestPending: boolean;
  readonly initialSetup: {
    readonly step: InitialSetupStep;
    readonly finishPending: boolean;
    readonly auxiliaryPendingKind: InitialSetupAuxiliaryKind | null;
    readonly importedSourcePath: string | null;
    readonly doclingReadinessVisible: boolean;
    readonly differences: readonly InitialSetupDiffEntry[];
  };
  readonly sessionSettings: {
    readonly draft: SessionSettingsDraft | null;
    readonly dirty: boolean;
    readonly validation: SessionSettingsValidation | null;
    readonly mutationPending: boolean;
    readonly availability: SessionSettingsMutationAvailability;
  };
  readonly sideChat: {
    readonly draft: string;
    readonly pendingQuote: SideChatPendingQuote | null;
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
    hub: hubPresentation(createHubUiState()),
    deviceNetwork: deviceNetworkPresentation(createDeviceNetworkUiState()),
    mcpPublish: publishPresentation(createPublishUiState()),
    mcpHistory: mcpHistoryPresentation(createMcpHistoryUiState()),
    mcpPeers: mcpPeerPresentation(createMcpPeerState()),
    artifactPane: {
      collapsed: false,
      mode: "output",
      selectedAgentPath: null,
      selectedAgentExecution: null,
    },
    attachmentTrayOpen: false,
    configMutationPending: false,
    doclingReadinessRequestPending: false,
    initialSetup: {
      step: "start",
      finishPending: false,
      auxiliaryPendingKind: null,
      importedSourcePath: null,
      doclingReadinessVisible: false,
      differences: [],
    },
    sessionSettings: {
      draft: null,
      dirty: false,
      validation: null,
      mutationPending: false,
      availability: {
        enabled: false,
        staleTarget: false,
        providerChanged: false,
        accessChanged: false,
        reason: "root sessionを選択すると変更できます。",
      },
    },
    sideChat: {
      draft: "",
      pendingQuote: null,
      catalog: {
        status: "idle",
        source: "none",
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
  const initialSetup = local.initialSetup ?? DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION.initialSetup;
  const sessionSettings = local.sessionSettings
    ?? DEFAULT_DESKTOP_RENDER_LOCAL_PRESENTATION.sessionSettings;
  const snapshot: DesktopRenderLocalPresentation = {
    hub: local.hub ?? hubPresentation(createHubUiState()),
    deviceNetwork: local.deviceNetwork ?? deviceNetworkPresentation(createDeviceNetworkUiState()),
    mcpPublish: local.mcpPublish ?? publishPresentation(createPublishUiState()),
    mcpHistory: local.mcpHistory ?? mcpHistoryPresentation(createMcpHistoryUiState()),
    mcpPeers: local.mcpPeers ?? mcpPeerPresentation(createMcpPeerState()),
    artifactPane: {
      collapsed: local.artifactPane.collapsed,
      mode: local.artifactPane.mode,
      selectedAgentPath: local.artifactPane.selectedAgentPath,
      selectedAgentExecution: local.artifactPane.selectedAgentExecution,
    },
    attachmentTrayOpen: local.attachmentTrayOpen,
    configMutationPending: local.configMutationPending,
    doclingReadinessRequestPending: local.doclingReadinessRequestPending ?? false,
    initialSetup: {
      step: initialSetup.step,
      finishPending: initialSetup.finishPending,
      auxiliaryPendingKind: initialSetup.auxiliaryPendingKind ?? null,
      importedSourcePath: initialSetup.importedSourcePath ?? null,
      doclingReadinessVisible: initialSetup.doclingReadinessVisible ?? false,
      differences: initialSetup.differences,
    },
    sessionSettings: {
      draft: sessionSettings.draft,
      dirty: sessionSettings.dirty,
      validation: sessionSettings.validation,
      mutationPending: sessionSettings.mutationPending,
      availability: sessionSettings.availability,
    },
    sideChat: {
      draft: local.sideChat.draft,
      pendingQuote: local.sideChat.pendingQuote,
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

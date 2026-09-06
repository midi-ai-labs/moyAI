import { command } from "./api.ts";
import type { ActionContext } from "./actions.ts";
import { loadDeviceNetwork } from "./device_network_actions.ts";
import {
  acceptHubProjection, hubCanSave, hubCanSetRouteMode, hubErrorText, hubSelectionFromDraft,
  type HubContext, type HubProjection, type HubRouteMode, type HubUiState,
} from "./hub_state.ts";

async function hubRequest(
  context: ActionContext,
  pending: NonNullable<HubUiState["pending"]>,
  name: string,
  args: Record<string, unknown>,
): Promise<void> {
  const local = context.uiState.hub;
  if (local.pending || context.getViewState()?.overlay !== "hub") return;
  const serial = ++local.requestSerial;
  const before = local.projection && structuredClone(local.projection);
  local.pending = pending;
  local.error = "";
  local.errorContext = null;
  context.rerender();
  try {
    const result = await command<HubProjection>(name, args);
    if (serial !== local.requestSerial) return;
    acceptHubProjection(local, result, {
      refreshTargets: pending === "refresh",
      savedContext: pending === "main" || pending === "side_chat" ? pending : undefined,
      connected: pending === "connect" && result.status === "connected",
      localSave: before && (pending === "main" || pending === "side_chat" || pending === "main_mode" || pending === "side_chat_mode")
        ? { before, context: pending === "main" || pending === "main_mode" ? "main" : "side_chat", kind: pending.endsWith("_mode") ? "mode" : "review" }
        : undefined,
    });
    if (pending === "connect" && result.status === "connected") {
      const token = document.querySelector<HTMLInputElement>("#hub-token");
      if (token) token.value = "";
    }
  } catch (error) {
    if (serial !== local.requestSerial || context.getViewState()?.overlay !== "hub") return;
    const message = hubErrorText(typeof error === "string" ? error : null)
      || "Hubの状態を読み込めませんでした。もう一度お試しください。";
    if (name !== "hub_projection") {
      try {
        // Rejected connection attempts may already have advanced the Rust owner. Keep the
        // request locked until it is reacquired so an immediate retry uses that fresh owner.
        const projection = await command<HubProjection>("hub_projection");
        if (serial === local.requestSerial && context.getViewState()?.overlay === "hub") {
          // Failure settlement is not the user's explicit review/rebase action.
          acceptHubProjection(local, projection);
        }
      } catch {
        // A failed local read must not replace the actionable original error or recurse.
      }
    }
    if (serial === local.requestSerial && context.getViewState()?.overlay === "hub") {
      local.error = message;
      local.errorContext = pending === "main" || pending === "main_mode" ? "main"
        : pending === "side_chat" || pending === "side_chat_mode" ? "side_chat" : "connection";
    }
  } finally {
    if (serial === local.requestSerial) { local.pending = null; context.rerender(); }
  }
}
export async function openHub(context: ActionContext): Promise<void> {
  await context.mutate("show_hub_editor");
  if (context.uiState.hub.tab === "models") await hubRequest(context, "load", "hub_projection", {});
  else await loadDeviceNetwork(context);
}
export async function selectHubTab(context: ActionContext, tab: "devices" | "models"): Promise<void> {
  if (context.uiState.hub.pending || context.uiState.deviceNetwork.pending || context.getViewState()?.overlay !== "hub") return;
  context.uiState.hub.tab = tab;
  context.rerender();
  if (tab === "models") await hubRequest(context, "load", "hub_projection", {});
  else await loadDeviceNetwork(context);
}
export async function connectHub(context: ActionContext): Promise<void> {
  const local = context.uiState.hub;
  const current = local.projection;
  if (!current) return;
  const token = document.querySelector<HTMLInputElement>("#hub-token")?.value ?? "";
  const endpoint = local.endpoint.trim();
  await hubRequest(context, "connect", "hub_connect", {
    endpoint: /^https?:\/\//i.test(endpoint) ? endpoint : `http://${endpoint}`,
    token,
    label: local.label.trim(),
    expectedSettingsRevision: current.settings_revision,
    expectedConnectionGeneration: current.connection_generation,
  });
}
export async function refreshHub(context: ActionContext): Promise<void> {
  const projection = context.uiState.hub.projection;
  if (!projection) return hubRequest(context, "load", "hub_projection", {});
  if (projection.status === "disconnected") return hubRequest(context, "refresh", "hub_projection", {});
  await hubRequest(context, "refresh", "hub_refresh", { expectedConnectionGeneration: projection.connection_generation });
}
export async function disconnectHub(context: ActionContext): Promise<void> {
  const projection = context.uiState.hub.projection;
  if (projection) await hubRequest(context, "disconnect", "hub_disconnect", {
    expectedConnectionGeneration: projection.connection_generation,
  });
}
export async function saveHubReview(context: ActionContext, channel: HubContext): Promise<void> {
  const local = context.uiState.hub;
  if (!hubCanSave(local, channel)) return;
  const selection = hubSelectionFromDraft(local.drafts[channel]);
  const target = local.drafts[channel].target;
  if (!selection || !target) return;
  await hubRequest(context, channel, "hub_save_review", { context: channel, selection, ...target });
}

export async function setHubRouteMode(context: ActionContext, channel: HubContext, mode: HubRouteMode): Promise<void> {
  const local = context.uiState.hub;
  if (!hubCanSetRouteMode(local, channel, mode) || !local.projection) return;
  await hubRequest(context, channel === "main" ? "main_mode" : "side_chat_mode", "hub_set_route_mode", {
    context: channel, mode,
    expectedSettingsRevision: local.projection.settings_revision,
    expectedConnectionGeneration: local.projection.connection_generation,
  });
}

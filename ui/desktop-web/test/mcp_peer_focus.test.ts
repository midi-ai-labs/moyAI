import assert from "node:assert/strict";
import test from "node:test";
import type { ActionContext } from "../src/actions.ts";
import { PostRenderFocusArbiter, type FocusTargetElement } from "../src/focus_arbiter.ts";
import { checkMcpPeer, editMcpPeerField, mutateMcpPeer, refreshMcpPeers } from "../src/mcp_peer.ts";
import { settingsActionFocusCandidates, settingsActionFocusStillTargets } from "../src/settings_surface.ts";
import type { DesktopWebState } from "../src/types.ts";
import { createUiLocalState } from "../src/ui_state.ts";

type PeerOperation = "add" | "remove" | "refresh" | "check";

async function withPeerFocus(run: (fixture: {
  context: ActionContext;
  operate: (operation: PeerOperation) => Promise<void>;
  settle: (value: unknown, rejected?: boolean) => void;
  activeName: () => string;
  moveToOtherEditor: () => void;
}) => Promise<void>): Promise<void> {
  const uiState = createUiLocalState();
  let view = { overlay: "config", confirmation_visible: false,
    config_target: { workspacePath: "C:/workspace", sessionId: "session-a", configGeneration: "7" },
  } as DesktopWebState;
  let settle!: (value: unknown, rejected?: boolean) => void;
  const response = new Promise((resolve, reject) => { settle = (value, rejected) => rejected ? reject(value) : resolve(value); });
  const token = { value: "private-bearer" };
  type Target = FocusTargetElement & { name: string; disabled: boolean };
  let active: Target;
  const target = (name: string): Target => ({ name, isConnected: true, disabled: false, focus() { active = this as Target; } });
  const body = target("body"), refresh = target("refresh"), initiatingButton = target("initiator"), close = target("close"), otherEditor = target("other-editor");
  active = initiatingButton;
  let callback: (() => void) | null = null;
  let commit = 0;
  const arbiter = new PostRenderFocusArbiter({ schedule(next: () => void) { callback = next; return 1; }, cancel() { callback = null; } }, {
    currentRenderCommit: () => commit, currentInteractionEpoch: () => 0n, interactionActive: () => false,
    activeElement: () => active, bodyElement: () => body, documentElement: () => null,
  });
  const render = () => {
    ++commit;
    refresh.disabled = initiatingButton.disabled = uiState.mcpPeers.pending !== null;
    // The native before-run established the browser's disabled-button blur to BODY.
    if (active.disabled) active = body;
    const continuation = uiState.settingsActionFocusContinuation;
    uiState.settingsActionFocusContinuation = null;
    if (!continuation) return;
    assert.equal(uiState.mcpPeers.pending, null, "focus continuation must settle after controls become available");
    arbiter.schedule({ renderCommit: commit, interactionEpoch: 0n, intents: [{
      source: "settings-action", priority: "explicit-transfer", claim: { kind: "unowned" },
      candidates: settingsActionFocusCandidates(continuation, selector => (selector === '[data-action="mcp-peer-refresh"]' ? refresh : close) as unknown as HTMLElement),
      isCurrent: () => settingsActionFocusStillTargets(continuation, view),
    }] });
    const scheduled = callback;
    callback = null;
    scheduled?.();
  };
  const context = { uiState, getViewState: () => view, rerender: render,
    acceptProjection: (next: DesktopWebState) => { view = next; render(); }, recoverCommandConflict: () => false,
  } as unknown as ActionContext;
  editMcpPeerField(uiState.mcpPeers, "id", "WinB");
  editMcpPeerField(uiState.mcpPeers, "base_url", "http://127.0.0.1:7332/mcp");
  const previous = new Map(["window", "document"].map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)]));
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke: () => response } } });
  Object.defineProperty(globalThis, "document", { configurable: true, value: { querySelector: (selector: string) => selector === "#mcp-peer-token" ? token : null } });
  try {
    await run({ context, settle, activeName: () => active.name, moveToOtherEditor: () => { active = otherEditor; },
      operate: operation => {
        if (operation === "check" || operation === "remove") uiState.mcpPeers.rows = [{ id: "WinB", base_url: uiState.mcpPeers.baseUrl, enabled: true, credential_configured: true, certificate_sha256: null }];
        if (operation === "refresh") return refreshMcpPeers(context);
        if (operation === "check") return checkMcpPeer(context, "WinB");
        return mutateMcpPeer(context, operation === "remove" ? "WinB" : undefined);
      },
    });
  } finally {
    arbiter.cancel();
    for (const [key, descriptor] of previous) {
      if (descriptor) Object.defineProperty(globalThis, key, descriptor);
      else delete (globalThis as Record<string, unknown>)[key];
    }
  }
}

test("failed peer add/remove restore dialog keyboard ownership after busy controls reenable", async () => {
  for (const operation of ["add", "remove"] as const) for (const rejected of [false, true]) {
    await withPeerFocus(async ({ context, operate, settle, activeName }) => {
      const pending = operate(operation);
      assert.equal(activeName(), "body");
      settle(rejected ? new Error("validation failure") : [context.getViewState(), false], rejected);
      await pending;
      assert.notEqual(context.uiState.mcpPeers.error, "");
      assert.equal(activeName(), "refresh", `${operation}/${rejected}: keyboard traversal must resume inside Settings`);
    });
  }
});

test("peer refresh and check restore keyboard ownership on success and failure", async () => {
  for (const operation of ["refresh", "check"] as const) for (const rejected of [false, true]) {
    await withPeerFocus(async ({ operate, settle, activeName }) => {
      const pending = operate(operation);
      assert.equal(activeName(), "body");
      settle(rejected ? new Error("connection failure") : operation === "refresh" ? { rows: [] } : { id: "WinB", tools: [{ name: "delegate_task" }] }, rejected);
      await pending;
      assert.equal(activeName(), "refresh", `${operation}/${rejected}`);
    });
  }
});

test("peer settlement does not steal an independently focused Settings editor", async () => {
  for (const operation of ["add", "remove", "refresh", "check"] as const) {
    await withPeerFocus(async ({ operate, settle, activeName, moveToOtherEditor }) => {
      const pending = operate(operation);
      moveToOtherEditor();
      settle(new Error("ordinary failure"), true);
      await pending;
      assert.equal(activeName(), "other-editor", operation);
    });
  }
});

test("peer settlement cannot restore focus after a serial, config target or surface change", async () => {
  for (const operation of ["add", "remove", "refresh", "check"] as const) for (const change of ["serial", "target", "surface"] as const) {
    await withPeerFocus(async ({ context, operate, settle, activeName }) => {
      const pending = operate(operation);
      if (change === "serial") ++context.uiState.mcpPeers.serial;
      else {
        const next = { ...context.getViewState() } as DesktopWebState;
        if (change === "target") next.config_target = { ...next.config_target, configGeneration: "8" };
        else next.overlay = "none";
        context.acceptProjection(next);
      }
      settle(new Error("late failure"), true);
      await pending;
      assert.equal(activeName(), "body", `${operation}/${change}`);
      assert.equal(context.uiState.settingsActionFocusContinuation, null);
    });
  }
});

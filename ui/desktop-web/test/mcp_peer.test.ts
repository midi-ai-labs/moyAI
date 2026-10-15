import assert from "node:assert/strict";
import test from "node:test";
import { createUiLocalState } from "../src/ui_state.ts";
import { checkMcpPeer, createMcpPeerState, editMcpPeerField, mcpPeerDraftValid, mcpPeerPresentation, mutateMcpPeer, renderMcpPeers } from "../src/mcp_peer.ts";
import type { ActionContext } from "../src/actions.ts";
import type { DesktopWebState } from "../src/types.ts";
import { DESKTOP_COMMAND_OBSERVER_SYMBOL } from "../src/api.ts";
import type { SettingsActionFocusContinuation } from "../src/settings_surface.ts";

test("peer form keeps credentials out of state and rejects duplicate names or credential-bearing URLs", () => {
  const local = createMcpPeerState();
  editMcpPeerField(local, "id", "WinB");
  editMcpPeerField(local, "base_url", "https://192.168.10.22:7332/mcp");
  editMcpPeerField(local, "token", "private-bearer");
  assert.equal(mcpPeerDraftValid(local), true);
  assert.doesNotMatch(JSON.stringify(local), /private-bearer/);
  const markup = renderMcpPeers(mcpPeerPresentation(local), false, true);
  assert.match(markup, /data-action="mcp-peer-add" disabled/);
  assert.match(markup, /data-settings-dom-value type="password"/);
  assert.match(markup, /先に設定を保存/);
  editMcpPeerField(local, "base_url", "https://user:secret@192.168.10.22/mcp");
  assert.equal(mcpPeerDraftValid(local), false);
  editMcpPeerField(local, "base_url", "https://192.168.10.22/mcp");
  local.rows = [{ id: "WinB", base_url: local.baseUrl, enabled: true, credential_configured: true, certificate_sha256: null }];
  assert.equal(mcpPeerDraftValid(local), false);
});

async function fixture(invoke: (name: string, args: Record<string, unknown>) => Promise<unknown>,
  run: (context: ActionContext, token: { value: string }) => Promise<void>): Promise<void> {
  const uiState = createUiLocalState();
  let view = { overlay: "config", config_target: { workspacePath: "C:/workspace", sessionId: "session-a", configGeneration: "7" } } as DesktopWebState;
  const token = { value: "private-bearer" };
  const content = { scrollLeft: 12, scrollTop: 880 };
  const context = { uiState, getViewState: () => view, rerender: () => {}, acceptProjection: (next: DesktopWebState) => { view = next; },
    recoverCommandConflict: () => false } as unknown as ActionContext;
  const previous = new Map(["window", "document"].map((key) => [key, Object.getOwnPropertyDescriptor(globalThis, key)]));
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke } } });
  Object.defineProperty(globalThis, "document", { configurable: true, value: { querySelector: (selector: string) => selector === ".settings-modal .settings-content" ? content : token } });
  try { await run(context, token); }
  finally { for (const [key, descriptor] of previous) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else delete (globalThis as Record<string, unknown>)[key]; } }
}

test("peer add uses the existing exact config owner and redacts its token from command observations", async () => {
  const calls: { name: string; args: Record<string, unknown> }[] = [];
  const observations: unknown[] = [];
  const observerKey = Symbol.for(DESKTOP_COMMAND_OBSERVER_SYMBOL);
  const global = globalThis as Record<PropertyKey, unknown>;
  const previous = global[observerKey];
  global[observerKey] = (event: unknown) => observations.push(event);
  try {
    await fixture(async (name, args) => {
      calls.push({ name, args });
      return name === "mcp_peer_add" ? [{ overlay: "config", config_target: { workspacePath: "C:/workspace", sessionId: "session-a", configGeneration: "8" } }, true] : { rows: [] };
    }, async (context, token) => {
      const local = context.uiState.mcpPeers;
      editMcpPeerField(local, "id", "WinB");
      editMcpPeerField(local, "base_url", "https://192.168.10.22:7332/mcp");
      editMcpPeerField(local, "certificate", "PUBLIC CERTIFICATE");
      await mutateMcpPeer(context);
      assert.deepEqual(calls[0].args, { peer: { id: "WinB", base_url: "https://192.168.10.22:7332/mcp", token: "private-bearer", trusted_certificate_pem: "PUBLIC CERTIFICATE", remote_agent: true },
        expectedTarget: { workspacePath: "C:/workspace", sessionId: "session-a", configGeneration: "7" } });
      assert.equal(context.uiState.activeConfigMutationGeneration, null);
      assert.equal(token.value, "");
      assert.equal(local.id, "");
      assert.doesNotMatch(JSON.stringify(observations), /private-bearer/);
      assert.doesNotMatch(JSON.stringify(mcpPeerPresentation(local)), /private-bearer/);
    });
  } finally { if (previous === undefined) delete global[observerKey]; else global[observerKey] = previous; }
});

test("peer save and remove attach Tools viewport continuity only to their accepted successful receipt", async () => {
  for (const remove of [false, true]) {
    const settled = { overlay: "config", config_target: { workspacePath: "C:/workspace", sessionId: "session-a", configGeneration: "8" } } as DesktopWebState;
    await fixture(async (name) => name === "mcp_peer_projection" ? { rows: [] } : [settled, true], async (context) => {
      const local = context.uiState.mcpPeers;
      editMcpPeerField(local, "id", "WinB"); editMcpPeerField(local, "base_url", "https://192.168.10.22/mcp");
      if (remove) local.rows = [{ id: "WinB", base_url: local.baseUrl, enabled: true, credential_configured: true, certificate_sha256: null }];
      let continuation: SettingsActionFocusContinuation | undefined;
      const accept = context.acceptProjection;
      context.acceptProjection = (state, render, received) => { continuation = received; accept(state, render); };
      await mutateMcpPeer(context, remove ? "WinB" : undefined);
      assert.deepEqual(continuation?.viewport, {
        sourceTarget: { workspacePath: "C:/workspace", sessionId: "session-a", configGeneration: "7" },
        scrollLeft: 12, scrollTop: 880,
      });
      assert.deepEqual(continuation?.target, settled.config_target);
      assert.equal(continuation?.primaryAction, "mcp-peer-refresh");
    });
  }
});

test("failed peer mutations retain the error and draft until deliberate editing or refresh", async () => {
  for (const remove of [false, true]) for (const outcome of ["rejected", "unsuccessful-receipt"]) {
    const calls: string[] = [];
    await fixture(async (name) => {
      calls.push(name);
      if (name === "mcp_peer_projection") return { rows: [] };
      if (outcome === "rejected") throw new Error("fixture validation rejection");
      return [{ overlay: "config", config_target: { workspacePath: "C:/workspace", sessionId: "session-a", configGeneration: "7" } }, false];
    }, async (context, token) => {
      const local = context.uiState.mcpPeers;
      editMcpPeerField(local, "id", "WinB");
      editMcpPeerField(local, "base_url", "http://127.0.0.1:7332/mcp");
      if (remove) local.rows = [{ id: "WinB", base_url: local.baseUrl, enabled: true, credential_configured: true, certificate_sha256: null }];
      await mutateMcpPeer(context, remove ? "WinB" : undefined);
      assert.notEqual(local.error, "", `${remove}/${outcome}: settled failure must remain visible`);
      assert.equal(local.pending, null);
      assert.equal(context.uiState.activeConfigMutationGeneration, null);
      assert.equal(local.id, "WinB");
      assert.equal(token.value, "private-bearer");
      assert.deepEqual(calls, [remove ? "mcp_peer_remove" : "mcp_peer_add"]);
      editMcpPeerField(local, "id", "WinC");
      assert.equal(local.error, "");
    });
  }
});

test("late peer receipt after Settings close and reopen cannot restore the old owner", async () => {
  let resolve!: (value: unknown) => void;
  const response = new Promise((done) => { resolve = done; });
  await fixture(async () => response, async (context) => {
    const local = context.uiState.mcpPeers;
    editMcpPeerField(local, "id", "WinB"); editMcpPeerField(local, "base_url", "https://192.168.10.22/mcp");
    let accepted = 0;
    context.acceptProjection = () => { ++accepted; };
    const operation = mutateMcpPeer(context);
    // main.ts invalidates this serial when the Settings surface closes.
    ++local.serial;
    local.pending = null;
    resolve([{ overlay: "config", config_target: { workspacePath: "C:/workspace", sessionId: "session-a", configGeneration: "8" } }, true]);
    await operation;
    assert.equal(accepted, 0);
  });
});

test("peer add preserves unrelated unsaved Settings and checks the actual delegate_task capability", async () => {
  const calls: string[] = [];
  await fixture(async (name) => { calls.push(name); return { id: "WinB", tools: [{ name: "delegate_task" }] }; }, async (context) => {
    const local = context.uiState.mcpPeers;
    editMcpPeerField(local, "id", "WinB"); editMcpPeerField(local, "base_url", "https://192.168.10.22/mcp");
    context.uiState.configDirty = true;
    await mutateMcpPeer(context);
    assert.deepEqual(calls, []);
    assert.equal(local.id, "WinB");
    local.rows = [{ id: "WinB", base_url: local.baseUrl, enabled: true, credential_configured: true, certificate_sha256: null }];
    await checkMcpPeer(context, "WinB");
    assert.match(local.checks.WinB, /エージェント受付を確認/);
  });
});

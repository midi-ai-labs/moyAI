import assert from "node:assert/strict";
import test from "node:test";
import { wireEvents } from "../src/events.ts";
import type { ActionContext } from "../src/actions.ts";
import type { DesktopViewState } from "../src/types.ts";
import { createUiLocalState } from "../src/ui_state.ts";

class SearchInput {
  value = "";
  listeners = new Map<string, (event: unknown) => void>();
  addEventListener(name: string, listener: (event: unknown) => void) { this.listeners.set(name, listener); }
  input(value: string) {
    this.value = value;
    this.listeners.get("input")?.({ currentTarget: this, isComposing: false });
  }
}

async function withSearchInput(run: (f: ReturnType<typeof fixture>) => Promise<void>) {
  const previous = ["document", "window"].map(name => [name, Object.getOwnPropertyDescriptor(globalThis, name)] as const);
  const f = fixture();
  try {
    Object.defineProperty(globalThis, "document", { configurable: true, value: {
      addEventListener() {}, querySelectorAll: () => [],
      querySelector: (selector: string) => selector === "#local-search" ? f.local : selector === "#session-search" ? f.session : null,
    } });
    Object.defineProperty(globalThis, "window", { configurable: true, value: {
      addEventListener() {},
      setTimeout: (callback: () => void) => { const id = ++f.nextTimer; f.timers.set(id, callback); return id; },
      clearTimeout: (id: number) => f.timers.delete(id),
      __TAURI_INTERNALS__: { invoke: async (name: string, args: Record<string, unknown>) => {
        f.calls.push({ name, args });
        return f.current;
      } },
    } });
    wireEvents(f.current, f.context);
    await run(f);
  } finally {
    await f.flush();
    for (const [name, descriptor] of previous) {
      if (descriptor) Object.defineProperty(globalThis, name, descriptor);
      else Reflect.deleteProperty(globalThis, name);
    }
  }
}

function fixture() {
  const f = {
    current: { overlay: "command_palette", workspace_path: "C:/original",
      draft_target: { workspacePath: "C:/original", sessionId: null, ownerGeneration: "1" },
      project_rows: [{ project_id: "project-a" }], selected_project_index: 0,
      side_chat: { owner_session_id: null, chat_id: null },
    } as DesktopViewState,
    local: new SearchInput(), session: new SearchInput(), nextTimer: 0,
    timers: new Map<number, () => void>(), calls: [] as Array<{ name: string; args: Record<string, unknown> }>,
    context: null as unknown as ActionContext,
    async flush() {
      const pending = [...f.timers.values()]; f.timers.clear(); pending.forEach(callback => callback());
      await new Promise<void>(resolve => setImmediate(resolve));
    },
  };
  f.context = { uiState: createUiLocalState(), getProjection: () => f.current, getViewState: () => f.current,
    acceptProjection() {}, recoverCommandConflict: () => false, reportError: (error: unknown) => { throw error; },
  } as unknown as ActionContext;
  return f;
}

test("a palette search is discarded before IPC after its dialog closes or draft owner changes", async () => {
  for (const change of ["dialog", "workspace", "generation"] as const) await withSearchInput(async f => {
    f.local.input("ワークスペースを切り替え");
    if (change === "dialog") f.current = { ...f.current, overlay: "workspace" };
    else if (change === "workspace") f.current = { ...f.current, workspace_path: "C:/alternate",
      draft_target: { ...f.current.draft_target, workspacePath: "C:/alternate" } };
    else f.current = { ...f.current, draft_target: { ...f.current.draft_target, ownerGeneration: "2" } };
    await f.flush();
    assert.deepEqual(f.calls, [], `${change}: obsolete search must not overwrite a successful navigation status in Rust`);
  });
});

test("current palette input still coalesces and delivers its exact text and draft owner", async () => {
  await withSearchInput(async f => {
    f.local.input("ワーク"); f.local.input("ワークスペース");
    await f.flush();
    assert.deepEqual(f.calls, [{ name: "set_local_search", args: {
      text: "ワークスペース", expectedTarget: f.current.draft_target,
    } }]);
  });
});

test("session search remains usable without a palette and drops a changed project owner", async () => {
  await withSearchInput(async f => {
    f.current = { ...f.current, overlay: "none" };
    f.session.input("first"); await f.flush();
    f.session.input("obsolete");
    f.current = { ...f.current, project_rows: [{ project_id: "project-b" }] } as DesktopViewState;
    await f.flush();
    assert.deepEqual(f.calls, [{ name: "set_session_search", args: {
      text: "first", expectedTarget: { workspacePath: "C:/original", projectId: "project-a" },
    } }]);
  });
});

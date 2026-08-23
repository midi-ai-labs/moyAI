import assert from "node:assert/strict";
import test from "node:test";

import { focusOverlayPrimary, handleSettingsNavigationClick } from "../src/events.ts";
import { PostRenderFocusArbiter } from "../src/focus_arbiter.ts";
import { overlayPrimaryFocusRequired } from "../src/modal_state.ts";
import { restoreScrollPosition } from "../src/scroll_state.ts";
import {
  sameSettingsSurface,
  settingsActionFocusCandidates,
  settingsActionFocusCandidateSelectors,
  settingsActionFocusStillTargets,
  settingsSectionTargetId,
  settingsSurfaceIdentity,
} from "../src/settings_surface.ts";
import type { DesktopViewState } from "../src/types.ts";
import { createUiLocalState } from "../src/ui_state.ts";

function settingsState(overrides: Partial<DesktopViewState> = {}): DesktopViewState {
  return {
    overlay: "config",
    confirmation_visible: false,
    config_target: {
      workspacePath: "C:/workspace-a",
      sessionId: "session-a",
      configGeneration: "7",
    },
    draft_target: {
      workspacePath: "C:/workspace-a",
      sessionId: "session-a",
      ownerGeneration: "1",
    },
    side_chat: {
      configured: false,
      deleting: false,
      chat_id: null,
      owner_session_id: "session-a",
      model: "",
      base_url: "http://127.0.0.1:1234/v1",
      status: "idle",
      phase: "",
      last_error: "",
      generation: "0",
      draft_text: "",
      draft_revision: "0",
      messages: [],
      can_send: false,
      can_cancel: false,
    },
    startup: { initial_setup_required: false },
    config_draft: {
      dirty: false,
      edit_enabled: true,
      discard_enabled: false,
      commit_enabled: true,
      external_owner_mutation_open: true,
      access_mode_mutation_enabled: false,
    },
    config_fields: [{
      key: "model.model",
      value: "draft-a",
      env_override: null,
      value_type: "string",
      required: true,
      min_value: null,
      max_value: null,
      options: [],
    }],
    ...overrides,
  } as DesktopViewState;
}

test("a slow new-session command keeps File, Command Palette, and Shortcuts focus unclaimed", () => {
  const ui = createUiLocalState();
  ui.activeNewSessionMutation = { mutationName: "new_chat", token: {} };
  ui.lastFocusedOverlay = "initiating-new-chat";

  for (const overlay of ["file_menu", "command_palette", "shortcuts"]) {
    focusOverlayPrimary(settingsState({ overlay }), ui);
    assert.equal(
      ui.lastFocusedOverlay,
      "initiating-new-chat",
      `${overlay} must not schedule a replacement primary focus while the typed owner is pending`,
    );
  }
});

test("pointer and keyboard Settings navigation focus the target editor without changing its draft", () => {
  class FakeElement {
    readonly ownerDocument: typeof fakeDocument;
    readonly name: string;
    readonly selectors = new Map<string, FakeElement[]>();
    parent: FakeElement | null = null;
    hidden = false;
    disabled = false;
    value = "";
    selectionStart = 0;
    selectionEnd = 0;
    scrollOptions: ScrollIntoViewOptions | null = null;
    focusOptions: FocusOptions | null = null;

    constructor(name: string) {
      this.name = name;
      this.ownerDocument = fakeDocument;
    }

    closest<T>(selector: string): T | null {
      if (selector === ".settings-modal") return (this.name === "modal" ? this : this.parent?.closest(selector)) as T | null;
      if (selector === ".settings-nav") return (this.name === "nav" ? this : this.parent?.closest(selector)) as T | null;
      if (selector.includes("details:not([open])")) return null;
      if (selector === ".settings-nav a[href^='#']") return (this.name === "anchor" ? this : this.parent?.closest(selector)) as T | null;
      return null;
    }

    contains(target: unknown): boolean {
      for (let current = target as FakeElement | null; current; current = current.parent) {
        if (current === this) return true;
      }
      return false;
    }

    matches(selector: string): boolean {
      return selector === ".settings-section" && this.name === "section";
    }

    querySelector<T>(selector: string): T | null {
      return (this.selectors.get(selector)?.[0] ?? null) as T | null;
    }

    querySelectorAll<T>(selector: string): T[] {
      return (this.selectors.get(selector) ?? []) as T[];
    }

    getAttribute(name: string): string | null {
      return name === "href" && this.name === "anchor" ? "#settings-side-chat" : null;
    }

    scrollIntoView(options: ScrollIntoViewOptions): void {
      this.scrollOptions = options;
    }

    focus(options: FocusOptions): void {
      this.focusOptions = options;
      fakeDocument.activeElement = this;
    }
  }

  const fakeDocument = {
    activeElement: null as FakeElement | null,
    elements: new Map<string, FakeElement>(),
    getElementById(id: string): FakeElement | null {
      return this.elements.get(id) ?? null;
    },
  };
  const modal = new FakeElement("modal");
  const nav = new FakeElement("nav");
  const content = new FakeElement("content");
  const section = new FakeElement("section");
  const anchor = new FakeElement("anchor");
  const editor = new FakeElement("editor");
  nav.parent = modal;
  content.parent = modal;
  section.parent = content;
  anchor.parent = nav;
  editor.parent = section;
  editor.value = "http://side.example/v1";
  editor.selectionStart = 7;
  editor.selectionEnd = 11;
  modal.selectors.set(".settings-content", [content]);
  section.selectors.set(".settings-control, .side-chat-settings-control", [editor]);
  fakeDocument.elements.set("settings-side-chat", section);

  const previousElement = Object.getOwnPropertyDescriptor(globalThis, "Element");
  const previousHTMLElement = Object.getOwnPropertyDescriptor(globalThis, "HTMLElement");
  try {
    Object.defineProperty(globalThis, "Element", {
      configurable: true,
      writable: true,
      value: FakeElement,
    });
    Object.defineProperty(globalThis, "HTMLElement", {
      configurable: true,
      writable: true,
      value: FakeElement,
    });
    for (const detail of [1, 0]) {
      let prevented = false;
      const event = {
        detail,
        target: anchor,
        preventDefault: () => { prevented = true; },
      } as unknown as MouseEvent;
      assert.equal(handleSettingsNavigationClick(event, settingsState()), true);
      assert.equal(prevented, true, detail === 0 ? "keyboard-generated click" : "pointer click");
      assert.equal(fakeDocument.activeElement, editor);
    }
    assert.deepEqual(section.scrollOptions, { block: "start", inline: "nearest" });
    assert.deepEqual(editor.focusOptions, { preventScroll: true });
    assert.equal(editor.value, "http://side.example/v1");
    assert.deepEqual([editor.selectionStart, editor.selectionEnd], [7, 11]);
    assert.equal(handleSettingsNavigationClick({
      target: anchor,
      preventDefault: () => assert.fail("non-Settings owner must stay native"),
    } as unknown as MouseEvent, settingsState({ overlay: "none" })), false);
  } finally {
    if (previousElement) Object.defineProperty(globalThis, "Element", previousElement);
    else delete (globalThis as Record<string, unknown>).Element;
    if (previousHTMLElement) Object.defineProperty(globalThis, "HTMLElement", previousHTMLElement);
    else delete (globalThis as Record<string, unknown>).HTMLElement;
  }
});

test("settings category fragments accept only canonical in-dialog section ids", () => {
  assert.equal(settingsSectionTargetId("#settings-provider"), "settings-provider");
  assert.equal(settingsSectionTargetId("#settings-side-chat"), "settings-side-chat");
  assert.equal(settingsSectionTargetId("#settings side-chat"), null);
  assert.equal(settingsSectionTargetId("https://example.test/#settings-provider"), null);
  assert.equal(settingsSectionTargetId("#outside"), null);
  assert.equal(settingsSectionTargetId(null), null);
});

test("settings surface preserves the live subtree only for the same exact owner and schema", () => {
  const before = settingsState();
  const polled = settingsState({
    config_fields: [{ ...before.config_fields[0], value: "draft-owned-by-the-live-input" }],
    config_draft: {
      ...before.config_draft,
      dirty: true,
      discard_enabled: true,
      external_owner_mutation_open: false,
    },
  });

  assert.equal(sameSettingsSurface(before, polled), true, "poll values do not replace browser-owned input state");
  assert.equal(sameSettingsSurface(before, settingsState({
    config_target: { ...before.config_target, configGeneration: "8" },
  })), false);
  assert.equal(sameSettingsSurface(before, settingsState({
    config_target: { ...before.config_target, workspacePath: "C:/workspace-b" },
  })), false);
  assert.equal(sameSettingsSurface(before, settingsState({
    config_target: { ...before.config_target, sessionId: "session-b" },
  })), false);
  assert.equal(sameSettingsSurface(before, settingsState({
    config_fields: [{ ...before.config_fields[0], key: "provider.base_url" }],
  })), false);
  assert.equal(sameSettingsSurface(before, settingsState({
    side_chat: {
      ...before.side_chat,
      configured: true,
      chat_id: "side-a",
      model: "gemma-side",
      can_send: true,
    },
  })), false, "configure response replaces the side settings section");
  const configured = settingsState({
    side_chat: {
      ...before.side_chat,
      configured: true,
      chat_id: "side-a",
      model: "gemma-side",
      can_send: true,
    },
  });
  assert.equal(sameSettingsSurface(configured, settingsState({
    side_chat: {
      ...configured.side_chat,
      status: "running",
      can_send: false,
      can_cancel: true,
    },
  })), false, "running capability replaces stale enabled controls");
  assert.equal(sameSettingsSurface(configured, settingsState({
    side_chat: { ...configured.side_chat, deleting: true, can_send: false },
  })), false, "deletion replaces stale enabled controls");
  assert.equal(settingsSurfaceIdentity(settingsState({ confirmation_visible: true })), null);
});

test("settings scroll restoration is instant and same-overlay polls do not refocus the first field", () => {
  let options: ScrollToOptions | null = null;
  const target = {
    style: { scrollBehavior: "smooth" },
    scrollTo: (next: ScrollToOptions) => {
      assert.equal(target.style.scrollBehavior, "auto");
      options = next;
    },
  };
  restoreScrollPosition(target, 14, 288);
  assert.deepEqual(options, { left: 14, top: 288, behavior: "auto" });
  assert.equal(target.style.scrollBehavior, "smooth");

  assert.equal(overlayPrimaryFocusRequired("config", "config:owner-a", "config:owner-a", false, false), false);
  assert.equal(overlayPrimaryFocusRequired("config", "config:owner-b", "config:owner-a", false, false), true);
  assert.equal(overlayPrimaryFocusRequired("config", "config:owner-a", "config:owner-a", true, false), true);
  assert.equal(overlayPrimaryFocusRequired("config", "config:owner-a", "config:owner-a", false, true), false);
});

test("settings action target fence and selector fallback chain resolve without focusing", () => {
  const current = settingsState();
  const continuation = {
    target: current.config_target,
    primaryAction: "discard-config-draft",
    fallbackAction: "close-overlay",
  };
  assert.equal(settingsActionFocusStillTargets(continuation, current), true);
  assert.equal(settingsActionFocusStillTargets(continuation, settingsState({
    config_target: { ...current.config_target, configGeneration: "8" },
  })), false);
  assert.deepEqual(settingsActionFocusCandidateSelectors(continuation), [
    '[data-action="discard-config-draft"]',
    '[data-action="close-overlay"]',
    ".settings-modal",
  ]);

  const resolved: string[] = [];
  const elements = new Map(settingsActionFocusCandidateSelectors(continuation).map((selector) => [
    selector,
    { selector, focus: () => assert.fail("candidate resolution must not focus") } as unknown as HTMLElement,
  ]));
  const candidates = settingsActionFocusCandidates(continuation, (selector) => {
    resolved.push(selector);
    return elements.get(selector) ?? null;
  });
  assert.equal(candidates.length, 3);
  assert.deepEqual(
    candidates.map((candidate) => (candidate.resolve() as unknown as { selector: string }).selector),
    settingsActionFocusCandidateSelectors(continuation),
  );
  assert.deepEqual(resolved, settingsActionFocusCandidateSelectors(continuation));
});

test("Settings ownership is acknowledged only after arbiter success or meaningful live focus", () => {
  class FakeElement {
    readonly isConnected = true;
    hidden = false;
    disabled = false;
    inert = false;
    acceptsFocus = true;
    focusCalls = 0;
    readonly name: string;
    protected readonly owner: FakeDocument;

    constructor(owner: FakeDocument, name: string) {
      this.owner = owner;
      this.name = name;
    }

    contains(target: unknown): boolean {
      return target === this || (this.name === "modal" && target === this.owner.field);
    }

    getAttribute(): string | null { return null; }
    matches(selector: string): boolean { return selector === ":disabled" && this.disabled; }
    closest(): FakeElement | null { return null; }
    focus(): void {
      this.focusCalls += 1;
      if (this.acceptsFocus) this.owner.activeElement = this;
    }
  }

  class FakeInput extends FakeElement {
    value = "draft-a";
    selectionStart = 0;
    selectionEnd = 0;

    setSelectionRange(start: number, end: number): void {
      this.selectionStart = start;
      this.selectionEnd = end;
    }
  }

  class FakeDocument {
    readonly body = new FakeElement(this, "body");
    readonly documentElement = new FakeElement(this, "html");
    readonly modal = new FakeElement(this, "modal");
    readonly field = new FakeInput(this, "field");
    activeElement: FakeElement = this.body;

    querySelector(selector: string): FakeElement | null {
      if (selector.startsWith(".modal[role=")) return this.modal;
      if (selector === ".settings-control") return this.field;
      return null;
    }
  }

  class ManualScheduler {
    private readonly callbacks = new Map<number, () => void>();
    private nextHandle = 1;

    schedule(callback: () => void): number {
      const handle = this.nextHandle++;
      this.callbacks.set(handle, callback);
      return handle;
    }

    cancel(handle: number): void {
      this.callbacks.delete(handle);
    }

    flush(): void {
      const callbacks = [...this.callbacks.values()];
      this.callbacks.clear();
      for (const callback of callbacks) callback();
    }
  }

  const fakeDocument = new FakeDocument();
  const globals = [
    "document",
    "HTMLElement",
    "HTMLInputElement",
    "HTMLTextAreaElement",
  ] as const;
  const previousGlobals = new Map(
    globals.map((name) => [name, Object.getOwnPropertyDescriptor(globalThis, name)] as const),
  );
  const defineGlobal = (name: string, value: unknown) => {
    Object.defineProperty(globalThis, name, { configurable: true, writable: true, value });
  };

  try {
    defineGlobal("document", fakeDocument);
    defineGlobal("HTMLElement", FakeElement);
    defineGlobal("HTMLInputElement", FakeInput);
    defineGlobal("HTMLTextAreaElement", FakeInput);

    const state = settingsState();
    const owner = `config:${settingsSurfaceIdentity(state)}`;
    const ui = createUiLocalState();
    ui.lastFocusedOverlay = "previous-owner";
    const scheduler = new ManualScheduler();
    const arbiter = new PostRenderFocusArbiter(scheduler, {
      currentRenderCommit: () => 1,
      currentInteractionEpoch: () => 1n,
      interactionActive: () => false,
      activeElement: () => fakeDocument.activeElement,
      bodyElement: () => fakeDocument.body,
      documentElement: () => fakeDocument.documentElement,
    });

    fakeDocument.field.acceptsFocus = false;
    const failingIntent = focusOverlayPrimary(state, ui);
    assert.ok(failingIntent);
    assert.equal(ui.lastFocusedOverlay, "previous-owner", "scheduling is not an acknowledgement");
    const failedResults: unknown[] = [];
    arbiter.schedule({
      renderCommit: 1,
      interactionEpoch: 1n,
      intents: [failingIntent],
      onResult: (result) => failedResults.push(result),
    });
    scheduler.flush();
    assert.deepEqual(failedResults, [{ kind: "unavailable", source: "modal-primary" }]);
    assert.equal(ui.lastFocusedOverlay, "previous-owner", "a refused focus attempt is not ownership");
    assert.ok(focusOverlayPrimary(state, ui), "the same owner remains eligible for a later retry");

    fakeDocument.field.acceptsFocus = true;
    const supersededIntent = focusOverlayPrimary(state, ui);
    assert.ok(supersededIntent);
    const supersededResults: unknown[] = [];
    arbiter.schedule({
      renderCommit: 1,
      interactionEpoch: 1n,
      intents: [supersededIntent],
      onResult: (result) => supersededResults.push(result),
    });
    arbiter.schedule({ renderCommit: 1, interactionEpoch: 1n, intents: [] });
    scheduler.flush();
    assert.deepEqual(supersededResults, [{ kind: "superseded", source: "modal-primary" }]);
    assert.equal(ui.lastFocusedOverlay, "previous-owner", "a superseded intent is not ownership");

    const successfulIntent = focusOverlayPrimary(state, ui);
    assert.ok(successfulIntent);
    const successfulResults: unknown[] = [];
    arbiter.schedule({
      renderCommit: 1,
      interactionEpoch: 1n,
      intents: [successfulIntent],
      onResult: (result) => successfulResults.push(result),
    });
    scheduler.flush();
    assert.deepEqual(successfulResults, [{ kind: "focused", source: "modal-primary" }]);
    assert.equal(ui.lastFocusedOverlay, owner);
    assert.deepEqual(
      [fakeDocument.field.selectionStart, fakeDocument.field.selectionEnd],
      [fakeDocument.field.value.length, fakeDocument.field.value.length],
    );

    const meaningfulUi = createUiLocalState();
    meaningfulUi.lastFocusedOverlay = "previous-owner";
    assert.equal(focusOverlayPrimary(state, meaningfulUi), null);
    assert.equal(
      meaningfulUi.lastFocusedOverlay,
      owner,
      "an already meaningful in-dialog focus is acknowledged without another focus request",
    );
  } finally {
    for (const [name, descriptor] of previousGlobals) {
      if (descriptor) Object.defineProperty(globalThis, name, descriptor);
      else delete (globalThis as Record<string, unknown>)[name];
    }
  }
});

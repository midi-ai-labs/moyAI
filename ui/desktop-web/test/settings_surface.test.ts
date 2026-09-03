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
  settingsCloseTargetStillMatches,
  settingsRecoverableErrorOwnerIdentity,
  settingsSectionTargetId,
  settingsSurfaceIdentity,
  shouldRetainConnectedSettingsSurface,
  synchronizeRetainedSettingsSurface,
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
      draft_quote: null,
      draft_revision: "0",
      context_scope: "owner_session",
      context_as_of_append_position: null,
      context_truncated: false,
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

test("initial setup retains one connected step only for the same exact setup owner", () => {
  const before = settingsState({
    overlay: "initial_setup",
    startup: {
      initial_setup_required: true,
      setup_target: {
        workspacePath: "C:/workspace-a",
        globalConfigPath: "C:/config/config.toml",
        setupGeneration: "3",
      },
    } as DesktopViewState["startup"],
  });
  const polled = settingsState({
    ...before,
    config_fields: [{ ...before.config_fields[0], value: "poll-must-not-replace-input" }],
  });

  assert.equal(sameSettingsSurface(before, polled, "provider", "provider"), true);
  assert.equal(
    shouldRetainConnectedSettingsSurface(before, polled, null, null, "provider", "provider"),
    true,
  );
  assert.equal(sameSettingsSurface(before, polled, "provider", "model"), false);
  assert.equal(sameSettingsSurface(before, settingsState({
    ...before,
    startup: {
      ...before.startup,
      setup_target: {
        ...before.startup.setup_target!,
        setupGeneration: "4",
      },
    },
  }), "provider", "provider"), false);
  assert.equal(settingsSurfaceIdentity(before), null, "step identity must be explicit");
});

test("recoverable errors distinguish global and unavailable modal owners without a null identity", () => {
  const globalOwner = settingsRecoverableErrorOwnerIdentity(settingsState({ overlay: "none" }));
  const unavailableOwner = settingsRecoverableErrorOwnerIdentity(settingsState({
    overlay: "session_settings",
    session_settings: {
      available: false,
      target: null,
    },
  } as Partial<DesktopViewState>));
  assert.notEqual(globalOwner, unavailableOwner);
  assert.match(globalOwner, /"surface":"none"/);
  assert.match(unavailableOwner, /"surface":"session_settings"/);
});

test("retained Settings rebuilds once when a local modal layer closes", () => {
  const before = settingsState();
  const current = settingsState();
  assert.equal(shouldRetainConnectedSettingsSurface(before, current, null, null), true);
  assert.equal(
    shouldRetainConnectedSettingsSurface(before, current, null, "local-confirm:settings-close"),
    false,
    "opening a local modal owns the outer markup",
  );
  assert.equal(
    shouldRetainConnectedSettingsSurface(before, current, "local-confirm:settings-close", null),
    false,
    "closing rebuilds away the alertdialog and clears the Settings backdrop inert state",
  );
  assert.equal(
    shouldRetainConnectedSettingsSurface(
      before,
      current,
      "local-confirm:settings-close",
      "local-confirm:settings-close",
    ),
    false,
  );
});

test("Settings close target fence rejects a changed owner or non-Settings surface", () => {
  const current = settingsState();
  assert.equal(settingsCloseTargetStillMatches(current.config_target, current), true);
  assert.equal(settingsCloseTargetStillMatches(current.config_target, settingsState({ overlay: "none" })), false);
  assert.equal(settingsCloseTargetStillMatches(current.config_target, settingsState({
    config_target: { ...current.config_target, configGeneration: "8" },
  })), false);
  assert.equal(settingsCloseTargetStillMatches(current.config_target, settingsState({
    config_target: { ...current.config_target, sessionId: "session-b" },
  })), false);
});

test("retained Settings availability synchronization preserves browser-owned draft interaction", () => {
  class FakeNode {
    readonly attributes = new Map<string, string>();
    hidden = false;

    getAttribute(name: string): string | null { return this.attributes.get(name) ?? null; }
    hasAttribute(name: string): boolean { return this.attributes.has(name); }
    setAttribute(name: string, value: string): void { this.attributes.set(name, value); }
    removeAttribute(name: string): void { this.attributes.delete(name); }
  }

  class FakeControl extends FakeNode {
    readonly tagName: string;
    readonly type: string;
    disabled = false;
    private storedValue = "";
    valueSetCount = 0;
    checked = false;
    selectionStart = 0;
    selectionEnd = 0;

    constructor(tagName: string, attributes: Record<string, string>) {
      super();
      this.tagName = tagName;
      this.type = attributes.type ?? "";
      for (const [name, value] of Object.entries(attributes)) this.attributes.set(name, value);
    }

    get value(): string { return this.storedValue; }
    set value(value: string) {
      this.storedValue = value;
      this.valueSetCount += 1;
      this.selectionStart = value.length;
      this.selectionEnd = value.length;
    }
  }

  class FakeLiveRegion extends FakeNode {
    readonly dataset: { settingsLiveRegion: string };
    readonly ownerDocument = { activeElement: null as FakeLiveRegion | null };
    replacedWith: FakeLiveRegion | null = null;

    constructor(identity: string) {
      super();
      this.dataset = { settingsLiveRegion: identity };
    }

    contains(target: unknown): boolean { return target === this; }
    replaceWith(next: FakeLiveRegion): void { this.replacedWith = next; }
  }

  class FakePassiveRegion extends FakeNode {
    readonly dataset: { settingsPassive: string };
    readonly ownerDocument = { activeElement: null as FakePassiveRegion | null };
    replacedWith: FakePassiveRegion | null = null;

    constructor(identity: string) {
      super();
      this.dataset = { settingsPassive: identity };
    }

    contains(target: unknown): boolean { return target === this; }
    replaceWith(next: FakePassiveRegion): void { this.replacedWith = next; }
  }

  class FakeModal extends FakeNode {
    readonly controls: FakeControl[];
    readonly liveRegions: FakeLiveRegion[];
    readonly passiveRegions: FakePassiveRegion[];
    readonly dependent = new FakeNode();
    readonly help = new FakeNode();

    constructor(
      controls: FakeControl[],
      liveRegions: FakeLiveRegion[] = [],
      passiveRegions: FakePassiveRegion[] = [],
    ) {
      super();
      this.controls = controls;
      this.liveRegions = liveRegions;
      this.passiveRegions = passiveRegions;
    }

    querySelectorAll<T>(selector: string): T[] {
      if (selector === "button, input, select, textarea") return this.controls as T[];
      if (selector === "select[data-main-provider-model-control]") return [];
      if (selector === "[data-settings-live-region]") return this.liveRegions as T[];
      if (selector === "[data-settings-passive]") return this.passiveRegions as T[];
      assert.fail(`unexpected selector: ${selector}`);
    }

    querySelector<T>(selector: string): T | null {
      if (selector === "[data-docling-dependent]") return this.dependent as T;
      if (selector === "#docling-disabled-help") return this.help as T;
      return null;
    }
  }

  const currentInput = new FakeControl("INPUT", { id: "docling-url" });
  currentInput.value = "編集中のURL";
  currentInput.selectionStart = 3;
  currentInput.selectionEnd = 6;
  currentInput.setAttribute("aria-invalid", "true");
  const currentToggle = new FakeControl("INPUT", {
    type: "checkbox",
    "data-config-key": "docling.enabled",
  });
  currentToggle.checked = true;
  const currentSave = new FakeControl("BUTTON", {
    "data-action": "save-global-config",
    "aria-busy": "false",
  });
  const currentClose = new FakeControl("BUTTON", {
    "data-action": "close-overlay",
    "aria-haspopup": "false",
  });
  const currentReadiness = new FakeLiveRegion("docling-readiness");
  const currentFocusedRegion = new FakeLiveRegion("focused-status");
  currentFocusedRegion.ownerDocument.activeElement = currentFocusedRegion;
  const currentPassive = new FakePassiveRegion("session-inheritance-help");
  const currentDirtyBadge = new FakePassiveRegion("session-settings-dirty-badge");
  const currentError = new FakePassiveRegion("session-settings-recoverable-error");
  currentError.setAttribute("data-settings-preserve-focused-region", "");
  currentError.ownerDocument.activeElement = currentError;
  const current = new FakeModal(
    [currentInput, currentToggle, currentSave, currentClose],
    [currentReadiness, currentFocusedRegion],
    [currentPassive, currentDirtyBadge, currentError],
  );
  current.dependent.setAttribute("aria-disabled", "false");

  const nextInput = new FakeControl("INPUT", { id: "docling-url" });
  nextInput.value = "poll値で上書きしてはいけない";
  const nextToggle = new FakeControl("INPUT", {
    type: "checkbox",
    "data-config-key": "docling.enabled",
  });
  nextToggle.checked = false;
  const nextSave = new FakeControl("BUTTON", {
    "data-action": "save-global-config",
    "aria-busy": "true",
  });
  nextSave.disabled = true;
  nextSave.hidden = true;
  const nextClose = new FakeControl("BUTTON", {
    "data-action": "close-overlay",
    "aria-haspopup": "alertdialog",
  });
  const nextReadiness = new FakeLiveRegion("docling-readiness");
  const nextFocusedRegion = new FakeLiveRegion("focused-status");
  const nextPassive = new FakePassiveRegion("session-inheritance-help");
  const nextDirtyBadge = new FakePassiveRegion("session-settings-dirty-badge");
  const nextError = new FakePassiveRegion("session-settings-recoverable-error");
  const next = new FakeModal(
    [nextInput, nextToggle, nextSave, nextClose],
    [nextReadiness, nextFocusedRegion],
    [nextPassive, nextDirtyBadge, nextError],
  );
  next.dependent.setAttribute("aria-disabled", "true");
  next.help.hidden = false;

  synchronizeRetainedSettingsSurface(
    current as unknown as HTMLElement,
    next as unknown as HTMLElement,
    true,
  );
  assert.equal(current.getAttribute("aria-busy"), "true");
  assert.equal(current.controls.every((control) => control.disabled), true);
  assert.equal(current.controls.every((control) => control.getAttribute("aria-disabled") === "true"), true);
  assert.equal(currentInput.value, "編集中のURL");
  assert.deepEqual([currentInput.selectionStart, currentInput.selectionEnd], [3, 6]);
  assert.equal(currentToggle.checked, true);
  assert.equal(currentReadiness.replacedWith, nextReadiness);
  assert.equal(currentFocusedRegion.replacedWith, null, "an active live region remains browser-owned");
  assert.equal(currentPassive.replacedWith, nextPassive, "keyed passive help follows the fresh projection");
  assert.equal(currentDirtyBadge.replacedWith, nextDirtyBadge, "the dirty badge follows local draft state");
  assert.equal(currentError.replacedWith, null, "focused error details remain browser-owned during a poll");
  assert.equal(currentSave.getAttribute("aria-busy"), "true", "an admitted Wizard action exposes busy state");
  assert.equal(currentClose.getAttribute("aria-haspopup"), "alertdialog", "dirty close announces its guard");

  nextSave.setAttribute("aria-busy", "false");
  nextClose.setAttribute("aria-haspopup", "false");

  synchronizeRetainedSettingsSurface(
    current as unknown as HTMLElement,
    next as unknown as HTMLElement,
    false,
  );
  assert.equal(current.getAttribute("aria-busy"), "false");
  assert.equal(currentInput.disabled, false);
  assert.equal(currentToggle.disabled, false);
  assert.equal(currentSave.disabled, true);
  assert.equal(currentSave.hidden, true);
  assert.equal(currentSave.getAttribute("aria-busy"), "false", "Wizard settlement clears stale busy state");
  assert.equal(currentClose.getAttribute("aria-haspopup"), "false", "clean settlement clears the close guard hint");
  assert.equal(current.dependent.getAttribute("aria-disabled"), "true");
  assert.equal(current.help.hidden, false);
  assert.equal(currentInput.value, "編集中のURL");
  assert.equal(currentToggle.checked, true);
  assert.equal(currentInput.getAttribute("aria-invalid"), "true");

  synchronizeRetainedSettingsSurface(
    current as unknown as HTMLElement,
    next as unknown as HTMLElement,
    false,
    true,
  );
  assert.equal(currentInput.value, "poll値で上書きしてはいけない", "a clean settlement adopts canonical text");
  assert.equal(currentToggle.checked, false, "a clean settlement adopts canonical checked state");
  assert.equal(currentInput.getAttribute("aria-invalid"), null, "a clean settlement clears stale invalid state");

  currentInput.selectionStart = 2;
  currentInput.selectionEnd = 5;
  const valueSetCount = currentInput.valueSetCount;
  synchronizeRetainedSettingsSurface(
    current as unknown as HTMLElement,
    next as unknown as HTMLElement,
    false,
    true,
  );
  assert.equal(currentInput.valueSetCount, valueSetCount, "an equal canonical poll does not assign value again");
  assert.deepEqual(
    [currentInput.selectionStart, currentInput.selectionEnd],
    [2, 5],
    "an equal canonical poll preserves browser selection",
  );
});

test("a focused provider model select applies the newest catalog exactly once on blur", () => {
  class FakeOption {
    readonly value: string;
    constructor(value: string) { this.value = value; }
    cloneNode(): FakeOption { return new FakeOption(this.value); }
  }
  class FakeSelect {
    readonly tagName = "SELECT";
    readonly id: string;
    readonly type = "";
    readonly attributes = new Map<string, string>();
    readonly ownerDocument: { activeElement: FakeSelect | null };
    hidden = false;
    disabled = false;
    isConnected = true;
    value: string;
    options: FakeOption[];
    private blur: (() => void) | null = null;

    constructor(
      id: string,
      values: string[],
      value: string,
      ownerDocument: { activeElement: FakeSelect | null },
    ) {
      this.id = id;
      this.options = values.map((candidate) => new FakeOption(candidate));
      this.value = value;
      this.ownerDocument = ownerDocument;
      this.attributes.set("id", id);
      this.attributes.set("data-main-provider-model-control", "");
    }
    getAttribute(name: string): string | null { return this.attributes.get(name) ?? null; }
    hasAttribute(name: string): boolean { return this.attributes.has(name); }
    setAttribute(name: string, value: string): void { this.attributes.set(name, value); }
    removeAttribute(name: string): void { this.attributes.delete(name); }
    addEventListener(name: string, listener: () => void): void {
      if (name === "blur") this.blur = listener;
    }
    replaceChildren(...options: FakeOption[]): void { this.options = options; }
    dispatchBlur(): void { this.blur?.(); }
  }
  class FakeModal {
    readonly select: FakeSelect;
    readonly attributes = new Map<string, string>();
    constructor(select: FakeSelect) { this.select = select; }
    setAttribute(name: string, value: string): void { this.attributes.set(name, value); }
    querySelectorAll<T>(selector: string): T[] {
      if (selector === "button, input, select, textarea") return [this.select] as T[];
      if (selector === "select[data-main-provider-model-control]") return [this.select] as T[];
      if (selector === "[data-settings-live-region]" || selector === "[data-settings-passive]") return [];
      assert.fail(`unexpected selector: ${selector}`);
    }
    querySelector<T>(): T | null { return null; }
  }

  const ownerDocument = { activeElement: null as FakeSelect | null };
  const currentSelect = new FakeSelect("initial-setup-model-select", ["model-a", "model-old"], "model-a", ownerDocument);
  const nextSelect = new FakeSelect("initial-setup-model-select", ["model-b", "model-new"], "model-b", { activeElement: null });
  ownerDocument.activeElement = currentSelect;
  synchronizeRetainedSettingsSurface(
    new FakeModal(currentSelect) as unknown as HTMLElement,
    new FakeModal(nextSelect) as unknown as HTMLElement,
    false,
  );
  assert.deepEqual(currentSelect.options.map((option) => option.value), ["model-a", "model-old"]);

  ownerDocument.activeElement = null;
  currentSelect.dispatchBlur();
  assert.deepEqual(currentSelect.options.map((option) => option.value), ["model-b", "model-new"]);
  assert.equal(currentSelect.value, "model-b");
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
    closest(selector: string): FakeElement | null {
      if (this === this.owner.field && selector.includes(".modal[role=")) return this.owner.modal;
      return null;
    }
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
    "Element",
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
    defineGlobal("Element", FakeElement);
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

import assert from "node:assert/strict";
import test from "node:test";

import { createRefreshPromptFocusIntent } from "../src/main_prompt_continuity.ts";

import {
  focusTargetEligible,
  PostRenderFocusArbiter,
  type FocusArbiterEnvironment,
  type FocusArbiterResult,
  type FocusArbiterScheduler,
  type FocusIntentPriority,
  type FocusIntentSource,
  type FocusTargetElement,
  type PostRenderFocusIntent,
} from "../src/focus_arbiter.ts";

class ManualScheduler implements FocusArbiterScheduler<number> {
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

interface TargetOptions {
  connected?: boolean;
  disabled?: boolean;
  ariaDisabled?: boolean;
  hidden?: boolean;
  ariaHidden?: boolean;
  inert?: boolean;
  inactiveAncestor?: boolean;
  acceptsFocus?: boolean;
}

function target(
  name: string,
  environment: MutableEnvironment,
  options: TargetOptions = {},
): FocusTargetElement & { readonly name: string; focusCalls: number } {
  const attributes = new Map<string, string>();
  if (options.ariaDisabled) attributes.set("aria-disabled", "true");
  if (options.ariaHidden) attributes.set("aria-hidden", "true");
  if (options.inert) attributes.set("inert", "");
  return {
    name,
    isConnected: options.connected ?? true,
    disabled: options.disabled ?? false,
    hidden: options.hidden ?? false,
    inert: options.inert ?? false,
    focusCalls: 0,
    matches(selector: string): boolean {
      return selector === ":disabled" && options.disabled === true;
    },
    getAttribute(attribute: string): string | null {
      return attributes.get(attribute) ?? null;
    },
    closest(): unknown | null {
      return options.inactiveAncestor ? this : null;
    },
    focus(): void {
      this.focusCalls += 1;
      if (options.acceptsFocus !== false) environment.active = this;
    },
  };
}

class MutableEnvironment implements FocusArbiterEnvironment {
  renderCommit = 1;
  interactionEpoch = 1n;
  activeInteraction = false;
  active: FocusTargetElement | null = null;
  body: FocusTargetElement | null = null;
  root: FocusTargetElement | null = null;

  currentRenderCommit(): number {
    return this.renderCommit;
  }

  currentInteractionEpoch(): bigint {
    return this.interactionEpoch;
  }

  interactionActive(): boolean {
    return this.activeInteraction;
  }

  activeElement(): FocusTargetElement | null {
    return this.active;
  }

  bodyElement(): FocusTargetElement | null {
    return this.body;
  }

  documentElement(): FocusTargetElement | null {
    return this.root;
  }
}

function intent(
  source: FocusIntentSource,
  priority: FocusIntentPriority,
  focusTarget: FocusTargetElement | null,
  overrides: Partial<PostRenderFocusIntent> = {},
): PostRenderFocusIntent {
  return {
    source,
    priority,
    claim: { kind: "unowned" },
    candidates: [{ resolve: () => focusTarget }],
    isCurrent: () => true,
    ...overrides,
  };
}

function scheduleAndFlush(
  arbiter: PostRenderFocusArbiter<number>,
  scheduler: ManualScheduler,
  intents: readonly PostRenderFocusIntent[],
  results: FocusArbiterResult[],
): void {
  arbiter.schedule({
    renderCommit: 1,
    interactionEpoch: 1n,
    intents,
    onResult: (result) => results.push(result),
  });
  scheduler.flush();
}

test("accepted pointer Refresh returns to the retained editor instead of restoring the Refresh snapshot", () => {
  const environment = new MutableEnvironment();
  const refresh = target("refresh", environment);
  const prompt = target("prompt", environment);
  environment.active = refresh;
  const scheduler = new ManualScheduler();
  const arbiter = new PostRenderFocusArbiter(scheduler, environment);
  const results: FocusArbiterResult[] = [];
  let settled = 0;

  scheduleAndFlush(arbiter, scheduler, [
    intent("focus-snapshot", "exact-restore", refresh),
    createRefreshPromptFocusIntent({
      prompt, resolvePrompt: () => prompt, refreshOwners: [refresh],
      settle: () => { settled += 1; }, isCurrent: () => true,
    }),
  ], results);

  assert.equal(environment.active, prompt);
  assert.equal(prompt.focusCalls, 1);
  assert.equal(refresh.focusCalls, 0);
  assert.equal(settled, 1);
  assert.deepEqual(results, [{ kind: "focused", source: "refresh-prompt" }]);
});

test("Refresh return preserves a later focus claim, stale owner, or replaced editor", () => {
  for (const change of ["focus-claim", "stale-owner", "replaced-editor", "later-interaction"] as const) {
    const environment = new MutableEnvironment();
    const refresh = target("refresh", environment);
    const prompt = target("prompt", environment);
    const other = target("other", environment);
    environment.active = change === "focus-claim" ? other : refresh;
    const scheduler = new ManualScheduler();
    const arbiter = new PostRenderFocusArbiter(scheduler, environment);
    const results: FocusArbiterResult[] = [];
    if (change === "later-interaction") environment.interactionEpoch = 2n;
    let settled = 0;
    const expectedKind = {
      "focus-claim": "owned", "stale-owner": "stale-intent",
      "replaced-editor": "unavailable", "later-interaction": "stale-interaction",
    }[change];

    scheduleAndFlush(arbiter, scheduler, [
      intent("focus-snapshot", "exact-restore", refresh),
      createRefreshPromptFocusIntent({
        prompt, resolvePrompt: () => change === "replaced-editor" ? other : prompt,
        refreshOwners: [refresh], settle: () => { settled += 1; },
        isCurrent: () => change !== "stale-owner",
      }),
    ], results);

    assert.equal(environment.active, change === "focus-claim" ? other : refresh, change);
    assert.equal(prompt.focusCalls + refresh.focusCalls + other.focusCalls, 0, change);
    assert.equal(settled, 0, change);
    assert.deepEqual(results, [{ kind: expectedKind, source: "refresh-prompt" }], change);
  }
});

test("explicit priority chooses one intent and performs at most one focus call", () => {
  const environment = new MutableEnvironment();
  environment.body = target("body", environment);
  environment.root = target("root", environment);
  environment.active = environment.body;
  const operation = target("operation", environment);
  const modal = target("modal", environment);
  const fallback = target("fallback", environment);
  const scheduler = new ManualScheduler();
  const arbiter = new PostRenderFocusArbiter(scheduler, environment);
  const results: FocusArbiterResult[] = [];

  scheduleAndFlush(arbiter, scheduler, [
    intent("composer-request", "fallback", fallback),
    intent("main-run", "operation-return", operation),
    intent("modal-primary", "modal-containment", modal, { claim: { kind: "force" } }),
  ], results);

  assert.equal(modal.focusCalls, 1);
  assert.equal(operation.focusCalls, 0);
  assert.equal(fallback.focusCalls, 0);
  assert.deepEqual(results, [{ kind: "focused", source: "modal-primary" }]);
});

test("a stale winning intent never falls through to a valid lower priority intent", () => {
  const environment = new MutableEnvironment();
  environment.body = target("body", environment);
  environment.active = environment.body;
  const stale = target("stale", environment);
  const lower = target("lower", environment);
  const scheduler = new ManualScheduler();
  const arbiter = new PostRenderFocusArbiter(scheduler, environment);
  const results: FocusArbiterResult[] = [];

  scheduleAndFlush(arbiter, scheduler, [
    intent("settings-action", "explicit-transfer", stale, { isCurrent: () => false }),
    intent("main-run", "operation-return", lower),
  ], results);

  assert.equal(stale.focusCalls, 0);
  assert.equal(lower.focusCalls, 0);
  assert.deepEqual(results, [{ kind: "stale-intent", source: "settings-action" }]);
});

test("an unavailable winning intent may use its own eligible fallback but not another intent", () => {
  const environment = new MutableEnvironment();
  environment.body = target("body", environment);
  environment.active = environment.body;
  const disconnected = target("disconnected", environment, { connected: false });
  const sameIntentFallback = target("same-intent-fallback", environment);
  const unrelated = target("unrelated", environment);
  const scheduler = new ManualScheduler();
  const arbiter = new PostRenderFocusArbiter(scheduler, environment);
  const results: FocusArbiterResult[] = [];

  scheduleAndFlush(arbiter, scheduler, [
    intent("titlebar-menu", "explicit-transfer", disconnected, {
      candidates: [
        { resolve: () => disconnected },
        { resolve: () => sameIntentFallback },
      ],
    }),
    intent("artifact-pane", "pane-navigation", unrelated),
  ], results);

  assert.equal(disconnected.focusCalls, 0);
  assert.equal(sameIntentFallback.focusCalls, 1);
  assert.equal(unrelated.focusCalls, 0);
  assert.deepEqual(results, [{ kind: "focused", source: "titlebar-menu" }]);

  environment.active = environment.body;
  const noFallbackResults: FocusArbiterResult[] = [];
  scheduleAndFlush(arbiter, scheduler, [
    intent("titlebar-menu", "explicit-transfer", disconnected),
    intent("artifact-pane", "pane-navigation", unrelated),
  ], noFallbackResults);
  assert.equal(unrelated.focusCalls, 0);
  assert.deepEqual(noFallbackResults, [{ kind: "unavailable", source: "titlebar-menu" }]);
});

test("render commit, interaction epoch, and active interaction are shared execution fences", () => {
  for (const [kind, mutate] of [
    ["stale-render", (environment: MutableEnvironment) => { environment.renderCommit = 2; }],
    ["stale-interaction", (environment: MutableEnvironment) => { environment.interactionEpoch = 2n; }],
    ["interaction-active", (environment: MutableEnvironment) => { environment.activeInteraction = true; }],
  ] as const) {
    const environment = new MutableEnvironment();
    environment.body = target("body", environment);
    environment.active = environment.body;
    const focusTarget = target("target", environment);
    const scheduler = new ManualScheduler();
    const arbiter = new PostRenderFocusArbiter(scheduler, environment);
    const results: FocusArbiterResult[] = [];
    arbiter.schedule({
      renderCommit: 1,
      interactionEpoch: 1n,
      intents: [intent("main-run", "operation-return", focusTarget)],
      onResult: (result) => results.push(result),
    });

    mutate(environment);
    scheduler.flush();

    assert.equal(focusTarget.focusCalls, 0, kind);
    assert.deepEqual(results, [{ kind, source: "main-run" }]);
  }
});

test("a newer scheduled commit supersedes the older callback even if cancellation is delayed", () => {
  class LeakyScheduler extends ManualScheduler {
    override cancel(_handle: number): void {
      // Simulates an animation-frame callback already queued by the host.
    }
  }
  const environment = new MutableEnvironment();
  environment.body = target("body", environment);
  environment.active = environment.body;
  const oldTarget = target("old", environment);
  const nextTarget = target("next", environment);
  const scheduler = new LeakyScheduler();
  const arbiter = new PostRenderFocusArbiter(scheduler, environment);
  const oldResults: FocusArbiterResult[] = [];
  const nextResults: FocusArbiterResult[] = [];

  arbiter.schedule({
    renderCommit: 1,
    interactionEpoch: 1n,
    intents: [intent("main-run", "operation-return", oldTarget)],
    onResult: (result) => oldResults.push(result),
  });
  arbiter.schedule({
    renderCommit: 1,
    interactionEpoch: 1n,
    intents: [intent("side-chat", "operation-return", nextTarget)],
    onResult: (result) => nextResults.push(result),
  });
  scheduler.flush();

  assert.deepEqual(oldResults, [{ kind: "superseded", source: "main-run" }]);
  assert.deepEqual(nextResults, [{ kind: "focused", source: "side-chat" }]);
  assert.equal(oldTarget.focusCalls, 0);
  assert.equal(nextTarget.focusCalls, 1);
});

test("common target eligibility rejects disconnected, disabled, hidden, aria-disabled, and inert targets", () => {
  const environment = new MutableEnvironment();
  const cases: Array<[string, TargetOptions]> = [
    ["disconnected", { connected: false }],
    ["disabled", { disabled: true }],
    ["aria-disabled", { ariaDisabled: true }],
    ["hidden", { hidden: true }],
    ["aria-hidden", { ariaHidden: true }],
    ["inert", { inert: true }],
    ["inactive ancestor", { inactiveAncestor: true }],
  ];
  for (const [name, options] of cases) {
    assert.equal(focusTargetEligible(target(name, environment, options)), false, name);
  }
  assert.equal(focusTargetEligible(target("eligible", environment)), true);
});

test("claim policy preserves meaningful focus and permits only an exact yielding owner", () => {
  const environment = new MutableEnvironment();
  environment.body = target("body", environment);
  const existingOwner = target("initiating-control", environment);
  const otherOwner = target("other-control", environment);
  const focusTarget = target("target", environment);
  const scheduler = new ManualScheduler();
  const arbiter = new PostRenderFocusArbiter(scheduler, environment);

  environment.active = otherOwner;
  const ownedResults: FocusArbiterResult[] = [];
  scheduleAndFlush(arbiter, scheduler, [
    intent("main-run", "operation-return", focusTarget),
  ], ownedResults);
  assert.deepEqual(ownedResults, [{ kind: "owned", source: "main-run" }]);
  assert.equal(focusTarget.focusCalls, 0);

  environment.active = existingOwner;
  const yieldResults: FocusArbiterResult[] = [];
  scheduleAndFlush(arbiter, scheduler, [
    intent("new-session", "explicit-transfer", focusTarget, {
      claim: { kind: "yield-from", owners: [existingOwner] },
    }),
  ], yieldResults);
  assert.deepEqual(yieldResults, [{ kind: "focused", source: "new-session" }]);
  assert.equal(focusTarget.focusCalls, 1);
});

test("an already-focused target settles selection without issuing another focus call", () => {
  const environment = new MutableEnvironment();
  const focusTarget = target("prompt", environment);
  environment.active = focusTarget;
  let settlements = 0;
  const scheduler = new ManualScheduler();
  const arbiter = new PostRenderFocusArbiter(scheduler, environment);
  const results: FocusArbiterResult[] = [];

  scheduleAndFlush(arbiter, scheduler, [
    intent("command-palette", "explicit-transfer", focusTarget, {
      candidates: [{
        resolve: () => focusTarget,
        settle: () => { settlements += 1; },
      }],
    }),
  ], results);

  assert.equal(focusTarget.focusCalls, 0);
  assert.equal(settlements, 1);
  assert.deepEqual(results, [{ kind: "already-focused", source: "command-palette" }]);
});

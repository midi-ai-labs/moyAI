import assert from "node:assert/strict";
import test from "node:test";

import {
  DESKTOP_COMMAND_OBSERVER_SYMBOL,
  command,
  isDesktopCommandName,
} from "../src/api.ts";
import type {
  PromptReviewMutationTarget,
  RunMutationTarget,
} from "../src/types.ts";
import {
  canonicalFixtureU64,
  runtimeOwnerToken,
  SESSION_A,
  TURN_A,
  TURN_B,
  WORKSPACE_A,
} from "./canonical_wire_fixture.ts";

test("Desktop command guard accepts registered wire names", () => {
  assert.equal(isDesktopCommandName("desktop_state"), true);
  assert.equal(isDesktopCommandName("exit_app"), true);
});

test("Desktop command guard rejects drift before invoking Tauri", async () => {
  assert.equal(isDesktopCommandName("desktop-state"), false);
  await assert.rejects(
    command("desktop-state"),
    /Unknown Desktop command: desktop-state/,
  );
});

test("Desktop command boundary forwards exact review and run expectations without reshaping them", async () => {
  const reviewTarget: PromptReviewMutationTarget = {
    workspacePath: WORKSPACE_A,
    sessionId: SESSION_A,
    ownerGeneration: canonicalFixtureU64(7n),
    requestId: canonicalFixtureU64(9_007_199_254_740_993n),
    expectedState: {
      kind: "turn",
      turnId: TURN_A,
      admissionRevision: canonicalFixtureU64(9_007_199_254_740_993n),
    },
  };
  const runTarget: RunMutationTarget = {
    workspacePath: WORKSPACE_A,
    sessionId: SESSION_A,
    runtimeOwnerToken: runtimeOwnerToken("root", 8n),
    permissionConfirmationId: null,
    expectedState: {
      kind: "turn",
      turnId: TURN_B,
      admissionRevision: canonicalFixtureU64(9_007_199_254_740_994n),
    },
  };
  const expectedArgs = {
    enhanced: true,
    text: "reviewed request",
    expectedTarget: reviewTarget,
    expectedRunTarget: runTarget,
  };
  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  let observed: { name: string; args: Record<string, unknown> } | null = null;
  let diagnostic: { name: string; args: Record<string, unknown> } | null = null;
  const observerKey = Symbol.for(DESKTOP_COMMAND_OBSERVER_SYMBOL);
  const globals = globalThis as Record<PropertyKey, unknown>;
  const previousObserver = Object.getOwnPropertyDescriptor(globalThis, observerKey);
  Object.defineProperty(globalThis, observerKey, {
    configurable: true,
    value: (value: { name: string; args: Record<string, unknown> }) => {
      diagnostic = structuredClone(value);
      value.args.enhanced = false;
    },
  });
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    writable: true,
    value: {
      __TAURI_INTERNALS__: {
        invoke: (name: string, args: Record<string, unknown>) => {
          observed = { name, args };
          return Promise.resolve(undefined);
        },
      },
    },
  });

  try {
    await command("send_prompt_review", expectedArgs);
    assert.deepEqual(observed, { name: "send_prompt_review", args: expectedArgs });
    assert.deepEqual(diagnostic, { name: "send_prompt_review", args: expectedArgs });
  } finally {
    if (previousWindow) Object.defineProperty(globalThis, "window", previousWindow);
    else delete (globalThis as Record<string, unknown>).window;
    if (previousObserver) Object.defineProperty(globalThis, observerKey, previousObserver);
    else delete globals[observerKey];
  }
});

test("Desktop command diagnostics cannot block delivery", async () => {
  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  const observerKey = Symbol.for(DESKTOP_COMMAND_OBSERVER_SYMBOL);
  const globals = globalThis as Record<PropertyKey, unknown>;
  const previousObserver = Object.getOwnPropertyDescriptor(globalThis, observerKey);
  let delivered = false;
  Object.defineProperty(globalThis, observerKey, {
    configurable: true,
    value: () => { throw new Error("diagnostic failure"); },
  });
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    writable: true,
    value: {
      __TAURI_INTERNALS__: {
        invoke: () => {
          delivered = true;
          return Promise.resolve(undefined);
        },
      },
    },
  });

  try {
    await command("exit_app");
    assert.equal(delivered, true);
  } finally {
    if (previousWindow) Object.defineProperty(globalThis, "window", previousWindow);
    else delete (globalThis as Record<string, unknown>).window;
    if (previousObserver) Object.defineProperty(globalThis, observerKey, previousObserver);
    else delete globals[observerKey];
  }
});

test("Hub credential reaches native connect but is redacted before command diagnostics", async () => {
  const globals = globalThis as Record<PropertyKey, unknown>;
  const observerKey = Symbol.for(DESKTOP_COMMAND_OBSERVER_SYMBOL);
  const previousWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  const previousObserver = Object.getOwnPropertyDescriptor(globalThis, observerKey);
  const deliveries: unknown[] = [];
  const observations: unknown[] = [];
  Object.defineProperty(globalThis, "window", { configurable: true, value: {
    __TAURI_INTERNALS__: { invoke: (name: string, args: unknown) => {
      deliveries.push({ name, args }); return Promise.resolve(undefined);
    } },
  } });
  Object.defineProperty(globalThis, observerKey, { configurable: true, value: (entry: unknown) => observations.push(entry) });
  try {
    const args = { endpoint: "http://127.0.0.1:9470", token: "private-bootstrap-token", label: "Desktop", expectedSettingsRevision: "0", expectedConnectionGeneration: "0" };
    await command("hub_connect", args);
    assert.deepEqual(deliveries, [{ name: "hub_connect", args }]);
    assert.deepEqual(observations, [{ name: "hub_connect", args: { ...args, token: "[redacted]" } }]);
    assert.doesNotMatch(JSON.stringify(observations), /private-bootstrap-token/);
  } finally {
    if (previousWindow) Object.defineProperty(globalThis, "window", previousWindow); else delete globals.window;
    if (previousObserver) Object.defineProperty(globalThis, observerKey, previousObserver); else delete globals[observerKey];
  }
});

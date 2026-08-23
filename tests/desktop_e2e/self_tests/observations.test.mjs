import assert from "node:assert/strict";
import test from "node:test";

import {
  invokeDesktopCommand,
  invokeDesktopCommandOutcome,
  normalizeDesktopCommandError,
} from "../scenarios/observations.mjs";

function cdpReturning(outcome) {
  return {
    async evaluate(source) {
      assert.match(source, /window\.__TAURI_INTERNALS__/);
      return structuredClone(outcome);
    },
  };
}

test("Desktop command outcome preserves typed conflicts for stale-target assertions", async () => {
  const state = {
    run_target: {
      expectedState: { kind: "idle", latestTurnId: null, admissionRevision: "0" },
    },
  };
  const conflict = {
    ok: false,
    error: { kind: "conflict", message: "stale run target", state },
    value: null,
  };
  assert.deepEqual(
    await invokeDesktopCommandOutcome(cdpReturning(conflict), "enhance_prompt", {
      text: "draft",
      expectedRunTarget: {
        expectedState: { kind: "idle", latestTurnId: "stale", admissionRevision: "1" },
      },
    }),
    conflict,
  );
  await assert.rejects(
    invokeDesktopCommand(cdpReturning(conflict), "enhance_prompt"),
    /Desktop command enhance_prompt failed: stale run target/,
  );
});

test("Desktop command errors normalize equivalent object and JSON-string Tauri rejections", () => {
  const state = { projection_revision: "8", draft_prompt: "" };
  const payload = { kind: "conflict", message: "stale run target", state };
  assert.deepEqual(normalizeDesktopCommandError(payload), payload);
  assert.deepEqual(normalizeDesktopCommandError(JSON.stringify(payload)), payload);
  assert.deepEqual(normalizeDesktopCommandError("plain bridge failure"), {
    kind: null,
    message: "plain bridge failure",
    state: null,
  });
  assert.deepEqual(normalizeDesktopCommandError("{malformed"), {
    kind: null,
    message: "{malformed",
    state: null,
  });
});

test("Desktop command outcome and value helpers share command-name validation", async () => {
  const success = { ok: true, error: null, value: { projection_revision: "4" } };
  assert.deepEqual(
    await invokeDesktopCommandOutcome(cdpReturning(success), "desktop_state"),
    success,
  );
  assert.deepEqual(
    await invokeDesktopCommand(cdpReturning(success), "desktop_state"),
    success.value,
  );
  await assert.rejects(
    invokeDesktopCommandOutcome(cdpReturning(success), "Desktop State"),
    /invalid Desktop command/,
  );
});

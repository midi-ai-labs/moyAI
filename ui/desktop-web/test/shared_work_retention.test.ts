import assert from "node:assert/strict";
import test from "node:test";
import { shouldRetainSharedWorkMain } from "../src/shared_work_retention.ts";

const main = { hub_project_open: true, overlay: "none", confirmation_visible: false };
test("Hub partial refresh yields the frame to newly appearing and departing modals", () => {
  assert.equal(shouldRetainSharedWorkMain(main, main, null, null, false), true);
  const permission = { ...main, confirmation_visible: true };
  assert.equal(shouldRetainSharedWorkMain(main, permission, null, null, true), false, "a new background-run approval must be mounted");
  assert.equal(shouldRetainSharedWorkMain(permission, main, null, null, false), false, "a resolved approval and inert frame must be removed");
  for (const [before, after] of [[null, "session-delete"], ["session-delete", null], ["session-delete", "session-delete"]]) {
    assert.equal(shouldRetainSharedWorkMain(main, main, before, after, after !== null), false, "local confirmation content and pending state use the full frame");
  }
  assert.equal(shouldRetainSharedWorkMain(main, main, null, null, true), false);
});

test("Hub partial refresh is limited to the same ordinary main surface", () => {
  for (const other of [null, { ...main, hub_project_open: false }, { ...main, overlay: "hub" }, { ...main, overlay: "config" }]) {
    assert.equal(shouldRetainSharedWorkMain(other, main, null, null, false), false);
    if (other) assert.equal(shouldRetainSharedWorkMain(main, other, null, null, false), false);
  }
});

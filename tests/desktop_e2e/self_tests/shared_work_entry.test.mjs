import assert from "node:assert/strict";
import test from "node:test";
import { sharedEntryReady } from "../scenarios/shared_work_entry.mjs";
test("shared entry requires an unfinished local setup and the dedicated shared surface", () => {
  const projection = { overlay: "shared_work", startup: { initial_setup_required: true }, busy: false };
  assert.equal(sharedEntryReady(projection), true);
  assert.equal(sharedEntryReady({ ...projection, startup: { initial_setup_required: false } }), false);
  assert.equal(sharedEntryReady({ ...projection, overlay: "none" }), false);
});

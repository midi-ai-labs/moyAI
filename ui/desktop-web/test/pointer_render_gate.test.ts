import assert from "node:assert/strict";
import test from "node:test";

import { PointerRenderGate } from "../src/pointer_render_gate.ts";
import {
  appliedProjectionRevision,
  deferredProjectionCandidatePreferred,
  isProjectionRevision,
  projectionUpdateAccepted,
} from "../src/projection_state.ts";
import { rowMutationArgs, rowMutationTargetStillMatches } from "../src/row_target.ts";
import type { DesktopWebState } from "../src/types.ts";

test("keeps only the newest render while a pointer action is active", () => {
  const gate = new PointerRenderGate<string>();

  assert.equal(gate.begin(7), true);
  assert.equal(gate.defer("first"), true);
  assert.equal(gate.defer("latest"), true);
  assert.equal(gate.end(7), "latest");
  assert.equal(gate.active, false);
});

test("pointer barrier keeps the highest Rust revision when responses arrive out of order", () => {
  const gate = new PointerRenderGate<{ revision: string; sequence: number }>((current, candidate) =>
    deferredProjectionCandidatePreferred(
      current.revision,
      candidate.revision,
      current.sequence,
      candidate.sequence,
    ),
  );

  gate.begin(8);
  gate.defer({ revision: "9007199254740992", sequence: 1 });
  gate.defer({ revision: "9007199254740991", sequence: 2 });
  gate.defer({ revision: "9007199254740992", sequence: 3 });

  assert.deepEqual(gate.end(8), { revision: "9007199254740992", sequence: 3 });
});

test("does not release an interaction for an unrelated pointer", () => {
  const gate = new PointerRenderGate<string>();

  gate.begin(3);
  gate.defer("pending");
  assert.equal(gate.end(4), null);
  assert.equal(gate.active, true);
  assert.equal(gate.cancel(), "pending");
});

test("does not defer rendering outside a pointer interaction", () => {
  const gate = new PointerRenderGate<string>();

  assert.equal(gate.defer("unused"), false);
  assert.equal(gate.cancel(), null);
});

test("a deferred render cannot retarget the row captured by the pointer action", () => {
  const gate = new PointerRenderGate<DesktopWebState>();
  const rendered = interactionState("session-a", "session-b");
  const refreshed = interactionState("session-a", "session-c");

  gate.begin(9);
  gate.defer(refreshed);
  const click = rowMutationArgs(rendered, 1, rendered.session_rows[1].session_id);
  const released = gate.end(9);

  assert.ok(click);
  assert.ok(released);
  assert.equal(
    rowMutationTargetStillMatches(released, click.expectedTarget, released.session_rows[1].session_id),
    false,
  );
});

test("Rust projection revision rejects old poll and text responses regardless of arrival order", () => {
  let revision = "0";
  assert.equal(projectionUpdateAccepted(revision, "12", false), true);
  revision = appliedProjectionRevision(revision, "12");

  assert.equal(projectionUpdateAccepted(revision, "10", false), false);
  assert.equal(projectionUpdateAccepted(revision, "11", false), false);
  assert.equal(projectionUpdateAccepted(revision, "13", false), true);
  revision = appliedProjectionRevision(revision, "13");
  assert.equal(revision, "13");
  assert.equal(projectionUpdateAccepted(revision, "13", true), true, "local rerender keeps the current object");
  assert.equal(projectionUpdateAccepted("14", "13", true), false, "an old current object cannot bypass the revision floor");
});

test("decimal revisions stay ordered across the JavaScript safe-integer boundary and u64 maximum", () => {
  const safeMax = "9007199254740991";
  const unsafeNext = "9007199254740992";
  const u64Max = "18446744073709551615";

  assert.equal(isProjectionRevision(safeMax), true);
  assert.equal(isProjectionRevision(unsafeNext), true);
  assert.equal(isProjectionRevision(u64Max), true);
  assert.equal(isProjectionRevision("18446744073709551616"), false);
  assert.equal(projectionUpdateAccepted(safeMax, unsafeNext, false), true);
  assert.equal(projectionUpdateAccepted(unsafeNext, u64Max, false), true);
  assert.equal(appliedProjectionRevision(unsafeNext, u64Max), u64Max);
});

function interactionState(ownerSessionId: string, targetSessionId: string): DesktopWebState {
  return {
    workspace_path: "C:/workspace",
    project_rows: [{ project_id: "project-a", label: "A", path: "C:/workspace" }],
    selected_project_index: 0,
    session_rows: [ownerSessionId, targetSessionId].map((sessionId) => ({ session_id: sessionId })),
    selected_session_index: 0,
  } as DesktopWebState;
}

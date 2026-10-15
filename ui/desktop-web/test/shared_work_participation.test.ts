import assert from "node:assert/strict";
import test from "node:test";
import type { ActionContext } from "../src/actions.ts";
import { confirmSharedConfirmation, requestSharedConfirmation, sharedWorkAction } from "../src/shared_work_actions.ts";
import { acceptSharedWork } from "../src/shared_work_state.ts";
import { sharedProjection, sharedUiFixture } from "./shared_work_fixture.ts";

function contextFor(local: ReturnType<typeof sharedUiFixture>): ActionContext {
  return { uiState: { sharedWork: local }, getViewState: () => ({ overlay: "none", hub_project_open: true }), rerender() {} } as unknown as ActionContext;
}
async function withInvoke(invoke: (name: string, args: any) => Promise<unknown>, body: () => Promise<void>) {
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: { invoke } } });
  try { await body(); } finally {
    if (original) Object.defineProperty(globalThis, "window", original);
    else delete (globalThis as Record<string, unknown>).window;
  }
}

test("same project rejoined between polls retires drafts and the old leave confirmation", async () => {
  const local = sharedUiFixture(), context = contextFor(local);
  local.prompt = "old participation draft";
  local.draft.followup = "old followup";
  requestSharedConfirmation(context, "leave_project", "project-a");
  const oldConfirmation = local.confirmation;
  assert.equal(oldConfirmation?.participationGeneration, 1);
  const projects = [{ ...local.projection!.projects[0], participation_generation: 2 }];
  // Deliberately keep the UI generation unchanged: non-selected projects do not
  // require a global generation change, and the confirmation has its own target.
  acceptSharedWork(local, sharedProjection({ revision: "2", projects }));
  assert.equal(local.prompt, "");
  assert.equal(local.draft.followup, "");
  assert.deepEqual(local.conversationDrafts, {});
  assert.equal(local.confirmation, null);
  local.confirmation = oldConfirmation;
  let calls = 0;
  await withInvoke(async () => { calls++; }, async () => {
    await confirmSharedConfirmation(context);
    await sharedWorkAction(context, "leave_project", "project-a");
  });
  assert.equal(calls, 0, "neither a retired confirmation nor an unbound action can leave the new participation");
  assert.equal(local.confirmation, null);
  assert.match(local.error, /参加状態が変わりました/);
});

test("unselected rejoin retires only its cached drafts and keeps the current project draft", () => {
  const local = sharedUiFixture();
  const a = local.projection!.projects[0];
  const b = { id: "project-b", label: "B", can_submit: true, participation_generation: 1 };
  local.projection!.projects = [a, b];
  local.prompt = "A old";
  acceptSharedWork(local, sharedProjection({ revision: "2", projects: [a, b], selected_project_id: b.id }));
  local.prompt = "B current";
  const rejoinedA = { ...a, participation_generation: 2 };
  acceptSharedWork(local, sharedProjection({ revision: "3", projects: [rejoinedA, b], selected_project_id: b.id }));
  assert.equal(local.prompt, "B current");
  assert.deepEqual(local.conversationDrafts, {});
  acceptSharedWork(local, sharedProjection({ revision: "4", projects: [rejoinedA, b] }));
  assert.equal(local.prompt, "");
  acceptSharedWork(local, sharedProjection({ revision: "5", projects: [rejoinedA, b], selected_project_id: b.id }));
  assert.equal(local.prompt, "B current");
});

test("unchanged membership through an unupdated projection preserves drafts and confirmation", () => {
  const local = sharedUiFixture();
  local.prompt = "resume after network recovery";
  requestSharedConfirmation(contextFor(local), "leave_project", "project-a");
  const confirmation = local.confirmation;
  acceptSharedWork(local, sharedProjection({ revision: "2", projects_stale: true }));
  assert.equal(local.prompt, "resume after network recovery");
  assert.deepEqual(local.confirmation, confirmation);
  acceptSharedWork(local, sharedProjection({ revision: "3" }));
  assert.equal(local.prompt, "resume after network recovery");
});

test("pending leave sends the captured participation and an old reply cannot restore the old owner", async () => {
  const local = sharedUiFixture(), context = contextFor(local);
  requestSharedConfirmation(context, "leave_project", "project-a");
  let sent: unknown;
  let finish!: (value: unknown) => void;
  await withInvoke(async (_name, args) => { sent = args; return new Promise(resolve => { finish = resolve; }); }, async () => {
    const leaving = confirmSharedConfirmation(context);
    assert.equal(local.pending, "leave_project");
    assert.deepEqual(sent, { expectedGeneration: "1", request: {
      kind: "leave_project", project_id: "project-a", expected_participation_generation: 1,
    } });
    acceptSharedWork(local, sharedProjection({ revision: "3", generation: "2",
      projects: [{ ...local.projection!.projects[0], participation_generation: 2 }] }));
    finish(sharedProjection({ revision: "2", projects: [], selected_project_id: null }));
    await leaving;
    assert.equal(local.projection!.generation, "2");
    assert.equal(local.projection!.projects[0].participation_generation, 2);
    assert.equal(local.pending, null);
  });
});

test("a pending sample response cannot refill a draft belonging to a new participation", async () => {
  const local = sharedUiFixture(), context = contextFor(local);
  let finish!: (value: unknown) => void;
  await withInvoke(async () => new Promise(resolve => { finish = resolve; }), async () => {
    const pending = sharedWorkAction(context, "prepare_sample");
    acceptSharedWork(local, sharedProjection({ revision: "3", generation: "2",
      projects: [{ ...local.projection!.projects[0], participation_generation: 2 }] }));
    finish(sharedProjection({ revision: "2", inputs: [{ id: "old-input", project_id: "project-a", job_id: null,
      kind: "input", name: "moyai-sample-numbers.csv", sha256: "a".repeat(64), byte_length: 10,
      created_at_ms: 1, version: 1, base_sha256: null, purged_at_ms: null }] }));
    await pending;
    assert.equal(local.prompt, "");
    assert.equal(local.title, "");
    assert.deepEqual(local.projection!.inputs, []);
    assert.equal(local.pending, null);
  });
});

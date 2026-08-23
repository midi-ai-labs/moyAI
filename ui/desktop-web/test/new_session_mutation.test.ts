import assert from "node:assert/strict";
import test from "node:test";

import {
  dispatchNewSessionMutation,
  repeatedNewSessionPointerActivation,
  type NewSessionMutationOwnerState,
} from "../src/new_session_mutation.ts";

class HeldInteraction {
  active = true;
  private releaseIdle: (() => void) | null = null;
  private readonly idle = new Promise<void>((resolve) => {
    this.releaseIdle = resolve;
  });

  whenIdle(): Promise<void> {
    return this.active ? this.idle : Promise.resolve();
  }

  release(): void {
    this.active = false;
    this.releaseIdle?.();
  }
}

test("new-chat and new-project-session share one owner through held activation release", async () => {
  const owner: NewSessionMutationOwnerState = { activeNewSessionMutation: null };
  const interaction = new HeldInteraction();
  const accepted: string[] = [];
  let commandCount = 0;
  let beginRenders = 0;
  const first = dispatchNewSessionMutation(
    owner,
    "new_chat",
    interaction,
    () => {
      beginRenders += 1;
      assert.equal(owner.activeNewSessionMutation?.mutationName, "new_chat");
    },
    async () => {
      commandCount += 1;
      return "quick-owner-2";
    },
    (response) => accepted.push(response),
    () => assert.fail("successful dispatch must not take the error release path"),
  );
  await Promise.resolve();
  assert.equal(owner.activeNewSessionMutation?.mutationName, "new_chat");
  assert.equal(beginRenders, 1, "the projected admission closes as soon as the owner begins");

  assert.equal(await dispatchNewSessionMutation(
    owner,
    "new_project_session",
    interaction,
    () => { beginRenders += 1; },
    async () => {
      commandCount += 1;
      return "project-owner-3";
    },
    (response) => accepted.push(response),
    () => undefined,
  ), false, "a stale second route cannot reach the command boundary");
  assert.equal(commandCount, 1);
  assert.equal(beginRenders, 1, "a rejected repeat cannot publish a second pending owner");
  assert.deepEqual(accepted, []);

  interaction.release();
  assert.equal(await first, true);
  assert.equal(commandCount, 1);
  assert.deepEqual(accepted, ["quick-owner-2"]);
  assert.equal(owner.activeNewSessionMutation, null);
});

test("new-session owner clears its exact token after a fast error and permits retry", async () => {
  const owner: NewSessionMutationOwnerState = { activeNewSessionMutation: null };
  const interaction = new HeldInteraction();
  let errorReleases = 0;
  const failed = dispatchNewSessionMutation(
    owner,
    "new_project_session",
    interaction,
    () => undefined,
    async () => {
      throw new Error("fast failure");
    },
    () => assert.fail("a failed command has no response to accept"),
    () => { errorReleases += 1; },
  );
  await Promise.resolve();
  assert.equal(owner.activeNewSessionMutation?.mutationName, "new_project_session");
  interaction.release();
  await assert.rejects(failed, /fast failure/);
  assert.equal(errorReleases, 1);
  assert.equal(owner.activeNewSessionMutation, null);

  assert.equal(await dispatchNewSessionMutation(
    owner,
    "new_chat",
    { active: false, whenIdle: async () => undefined },
    () => undefined,
    async () => "retry-owner",
    () => undefined,
    () => undefined,
  ), true);
  assert.equal(owner.activeNewSessionMutation, null);
});

test("held new-project-session activation cannot dispatch a second project command", async () => {
  const owner: NewSessionMutationOwnerState = { activeNewSessionMutation: null };
  const interaction = new HeldInteraction();
  let commandCount = 0;
  const first = dispatchNewSessionMutation(
    owner,
    "new_project_session",
    interaction,
    () => undefined,
    async () => {
      commandCount += 1;
      return "project-owner-11";
    },
    () => undefined,
    () => undefined,
  );
  await Promise.resolve();
  assert.equal(await dispatchNewSessionMutation(
    owner,
    "new_project_session",
    interaction,
    () => undefined,
    async () => {
      commandCount += 1;
      return "project-owner-12";
    },
    () => undefined,
    () => undefined,
  ), false);
  assert.equal(commandCount, 1);
  interaction.release();
  assert.equal(await first, true);
  assert.equal(commandCount, 1);
});

test("rapid pointer repeats are ignored only for the two exact new-session actions", () => {
  for (const action of ["new-chat", "new-project-session"]) {
    assert.equal(repeatedNewSessionPointerActivation(action, 1), false, action);
    assert.equal(repeatedNewSessionPointerActivation(action, 2), true, action);
    assert.equal(repeatedNewSessionPointerActivation(action, 3), true, action);
  }
  assert.equal(repeatedNewSessionPointerActivation("select-session", 2), false);
});

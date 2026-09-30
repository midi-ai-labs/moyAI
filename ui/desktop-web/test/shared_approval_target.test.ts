import assert from "node:assert/strict";
import test from "node:test";
import type { ActionContext } from "../src/actions.ts";
import { sharedWorkAction } from "../src/shared_work_actions.ts";
import { sharedUiFixture } from "./shared_work_fixture.ts";

test("a decision in the root conversation targets only the projected child approval", async () => {
  const local = sharedUiFixture();
  local.projection!.selected_job_id = "root-a";
  local.projection!.approval = { id: "approval-b", attempt_id: "attempt-b", status: "pending", can_decide: true,
    decision: null, expires_at_ms: 9999999999999,
    context: { job_id: "child-b", project_id: "project-a", root_id: "root-a", conversation_id: "root-a",
      job_title: "B work", controller_device_id: "device-a", controller_device_label: "WinA",
      execution_device_id: "device-b", execution_device_label: "WinB" },
    request: { access: "shell", summary: "Run", details: ["Command: echo test"], targets: [], outside_workspace: false, risks: [] } };
  const original = Object.getOwnPropertyDescriptor(globalThis, "window");
  const sent: unknown[] = [];
  Object.defineProperty(globalThis, "window", { configurable: true, value: { __TAURI_INTERNALS__: {
    invoke: async (_name: string, args: unknown) => { sent.push(args); return { ...local.projection, revision: "2" }; },
  } } });
  try {
    const context = { uiState: { sharedWork: local }, getViewState: () => ({ overlay: "none", hub_project_open: true }), rerender: () => {} } as unknown as ActionContext;
    await sharedWorkAction(context, "approve", "stale-approval");
    assert.equal(sent.length, 0);
    await sharedWorkAction(context, "approve", "approval-b");
    assert.deepEqual(sent, [{ expectedGeneration: "1", request: { kind: "decide", decision: "approve",
      project_id: "project-a", job_id: "child-b", approval_id: "approval-b" } }]);
    assert.equal(local.projection!.selected_job_id, "root-a");
  } finally {
    if (original) Object.defineProperty(globalThis, "window", original);
    else delete (globalThis as Record<string, unknown>).window;
  }
});

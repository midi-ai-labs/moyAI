import assert from "node:assert/strict";
import test from "node:test";
import type { ActionContext } from "../src/actions.ts";
import { PostRenderFocusArbiter, type FocusTargetElement } from "../src/focus_arbiter.ts";
import { InteractionLifecycle } from "../src/interaction_lifecycle.ts";
import { startSharedRename, startSharedRevision } from "../src/shared_work_actions.ts";
import { sharedEditorFocusIntent, sharedWorkPresentation } from "../src/shared_work_state.ts";
import { sharedProjection, sharedUiFixture } from "./shared_work_fixture.ts";

test("click and Enter edit starts focus the committed shared editor after interaction release", () => {
  for (const kind of ["rename", "revise"] as const) {
    for (const keyboard of [true, false]) {
      const local = sharedUiFixture();
      const job = { ...sharedProjection().status!.jobs[0], state: "cancelled", can_cancel: false, can_revise: true };
      local.projection = sharedProjection({ selected_job_id: "job-a", selected_conversation_id: "job-a",
        detail: { id: "job-a", project_id: "project-a", root_id: "job-a", parent_id: null,
          environment_id: "env-a", title: "old", input: { prompt: "old" }, result: null,
          state: "cancelled", awaiting_child_id: null, revision: 7, created_at_ms: 1, updated_at_ms: 2, can_revise: true },
        conversation_history: { project_id: "project-a", conversation_id: "job-a", snapshot: 1, next_before: null,
          jobs: [{ job, input: { prompt: "old" }, result: null, artifacts: [], more_artifacts: false }] } });
      const previous = sharedWorkPresentation(local);
      const lifecycle = new InteractionLifecycle<null>(() => true);
      let mounted = false;
      let focused: FocusTargetElement | null = null;
      let focusCalls = 0;
      let scheduled: (() => void) | null = null;
      const editor: FocusTargetElement = { isConnected: true, focus() { focused = editor; focusCalls += 1; } };
      const context = { uiState: { sharedWork: local }, getViewState: () => ({ overlay: "none", hub_project_open: true }),
        rerender() { if (!lifecycle.defer(null, true, true)) mounted = true; } } as unknown as ActionContext;
      if (keyboard) lifecycle.beginKey("Enter"); else lifecycle.beginPointer(1);
      if (kind === "rename") startSharedRename(context, "job-a"); else startSharedRevision(context, "job-a");
      assert.equal(mounted, false);
      assert.equal(focusCalls, 0);
      const release = keyboard ? lifecycle.captureKeyEnd("Enter")!() : lifecycle.capturePointerEnd(1)!();
      assert.equal(release?.renderCurrent, true);
      mounted = true;
      const current = sharedWorkPresentation(local);
      const intent = sharedEditorFocusIntent(previous, current, () => sharedWorkPresentation(local), selector => {
        assert.equal(selector, kind === "rename" ? "#shared-rename-title" : "#shared-revise-prompt");
        return mounted ? editor : null;
      });
      assert.ok(intent);
      const arbiter = new PostRenderFocusArbiter<number>({ schedule(callback) { scheduled = callback; return 1; }, cancel() {} }, {
        currentRenderCommit: () => 1, currentInteractionEpoch: () => 1n, interactionActive: () => lifecycle.active,
        activeElement: () => focused, bodyElement: () => null, documentElement: () => null,
      });
      arbiter.schedule({ renderCommit: 1, interactionEpoch: 1n, intents: [intent] });
      assert.equal(focusCalls, 0);
      (scheduled as unknown as () => void)();
      assert.equal(focusCalls, 1);
      assert.equal(sharedEditorFocusIntent(current, current, () => current, () => editor), null,
        "a poll preserving the open editor must not reclaim focus");
      local.projection = { ...local.projection!, generation: "2" };
      assert.equal(intent.isCurrent(), false, "the previous connection cannot claim the new editor");
    }
  }
});

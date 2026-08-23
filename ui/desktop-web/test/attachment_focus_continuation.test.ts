import assert from "node:assert/strict";
import test from "node:test";

import {
  attachmentFocusCandidates,
  beginAttachmentFocusContinuation,
  reconcileAttachmentFocusContinuation,
} from "../src/attachment_focus_continuation.ts";
import type { DesktopViewState } from "../src/types.ts";

function state(
  paths: string[],
  overrides: Partial<DesktopViewState> = {},
): DesktopViewState {
  return {
    workspace_path: "C:/workspace",
    project_rows: [{ project_id: "project-a", label: "Project A", path: "C:/workspace" }],
    selected_project_index: 0,
    session_rows: [{ session_id: "session-a" }],
    selected_session_index: 0,
    attached_images: paths,
    draft_target: {
      workspacePath: "C:/workspace",
      sessionId: "session-a",
      ownerGeneration: "7",
    },
    ...overrides,
  } as DesktopViewState;
}

function draftArgs(current: DesktopViewState): Record<string, unknown> {
  return { expectedTarget: current.draft_target };
}

function removalArgs(
  current: DesktopViewState,
  index: number,
): Record<string, unknown> {
  return {
    index,
    expectedTarget: {
      workspacePath: current.workspace_path,
      ownerProjectId: current.project_rows[current.selected_project_index]?.project_id ?? null,
      ownerSessionId: current.session_rows[current.selected_session_index]?.session_id ?? null,
      rowId: current.attached_images[index],
    },
  };
}

test("browse cancel reopens the exact owner tray while success closes it", () => {
  const before = state(["C:/workspace/one.png"]);
  const continuation = beginAttachmentFocusContinuation(before, "browse_image", draftArgs(before));
  assert.ok(continuation);

  const cancelled = reconcileAttachmentFocusContinuation(continuation, state([...before.attached_images]), "browse_image");
  assert.deepEqual(cancelled, {
    continuation: null,
    focusTarget: { kind: "browse" },
    trayOpen: true,
  });

  const succeeded = reconcileAttachmentFocusContinuation(
    continuation,
    state([...before.attached_images, "C:/workspace/two.png"]),
    "browse_image",
  );
  assert.deepEqual(succeeded, {
    continuation: null,
    focusTarget: { kind: "toggle" },
    trayOpen: false,
  });
});

test("attachment focus continuation is fenced by owner and exact mutation", () => {
  const before = state([]);
  const continuation = beginAttachmentFocusContinuation(before, "browse_image", draftArgs(before));
  assert.ok(continuation);

  const polling = reconcileAttachmentFocusContinuation(continuation, before, null);
  assert.equal(polling.continuation, continuation);
  assert.equal(polling.focusTarget, null);

  const wrongOwner = state([], {
    draft_target: { ...before.draft_target, sessionId: "session-b", ownerGeneration: "8" },
    session_rows: [{ session_id: "session-b" }],
  });
  assert.deepEqual(
    reconcileAttachmentFocusContinuation(continuation, wrongOwner, "browse_image"),
    { continuation: null, focusTarget: null, trayOpen: null },
  );
  assert.equal(
    beginAttachmentFocusContinuation(before, "browse_image", {
      expectedTarget: { ...before.draft_target, ownerGeneration: "6" },
    }),
    null,
  );
});

test("remove focuses the next then previous exact attachment and final removal uses the toggle", () => {
  const paths = ["C:/workspace/one.png", "C:/workspace/two.png", "C:/workspace/three.png"];
  const before = state(paths);
  const removeMiddle = beginAttachmentFocusContinuation(before, "remove_image", removalArgs(before, 1));
  assert.ok(removeMiddle);
  assert.deepEqual(
    reconcileAttachmentFocusContinuation(
      removeMiddle,
      state([paths[0], paths[2]]),
      "remove_image",
    ),
    {
      continuation: null,
      focusTarget: { kind: "attachment", path: paths[2] },
      trayOpen: null,
    },
  );

  const removeLast = beginAttachmentFocusContinuation(before, "remove_image", removalArgs(before, 2));
  assert.ok(removeLast);
  assert.deepEqual(
    reconcileAttachmentFocusContinuation(
      removeLast,
      state([paths[0], paths[1]]),
      "remove_image",
    ).focusTarget,
    { kind: "attachment", path: paths[1] },
  );

  const finalState = state([paths[0]]);
  const removeFinal = beginAttachmentFocusContinuation(finalState, "remove_image", removalArgs(finalState, 0));
  assert.ok(removeFinal);
  assert.deepEqual(
    reconcileAttachmentFocusContinuation(removeFinal, state([]), "remove_image"),
    { continuation: null, focusTarget: { kind: "toggle" }, trayOpen: false },
  );

  assert.equal(
    beginAttachmentFocusContinuation(before, "remove_image", {
      ...removalArgs(before, 1),
      expectedTarget: { ...(removalArgs(before, 1).expectedTarget as object), rowId: paths[0] },
    }),
    null,
  );
});

test("typed attach and clear completion choose a stable attachment toggle", () => {
  const empty = state([]);
  const attach = beginAttachmentFocusContinuation(empty, "attach_image", draftArgs(empty));
  assert.ok(attach);
  assert.deepEqual(
    reconcileAttachmentFocusContinuation(
      attach,
      state(["C:/workspace/one.png"]),
      "attach_image",
    ),
    { continuation: null, focusTarget: { kind: "toggle" }, trayOpen: false },
  );

  const populated = state(["C:/workspace/one.png"]);
  const clear = beginAttachmentFocusContinuation(populated, "clear_images", draftArgs(populated));
  assert.ok(clear);
  assert.deepEqual(
    reconcileAttachmentFocusContinuation(clear, state([]), "clear_images"),
    { continuation: null, focusTarget: { kind: "toggle" }, trayOpen: false },
  );
});

class FakeElement {
  readonly dataset: Record<string, string> = {};
  disabled = false;
  private readonly owner: FakeDocument;
  readonly key: string;

  constructor(owner: FakeDocument, key: string) {
    this.owner = owner;
    this.key = key;
    if (key.startsWith("attachment:")) this.dataset.focusKey = key;
  }

  matches(selector: string): boolean {
    return selector === ":disabled" && this.disabled;
  }

  focus(): void {
    this.owner.activeElement = this;
  }
}

class FakeDocument {
  readonly body = new FakeElement(this, "body");
  readonly documentElement = new FakeElement(this, "root");
  readonly prompt = new FakeElement(this, "prompt");
  readonly toggle = new FakeElement(this, "toggle");
  readonly browse = new FakeElement(this, "browse");
  readonly input = new FakeElement(this, "input");
  readonly attachments = [
    new FakeElement(this, "attachment:C:/workspace/one.png"),
    new FakeElement(this, "attachment:C:/workspace/two.png"),
  ];
  activeElement: FakeElement | null = this.body;

  querySelector(selector: string): FakeElement | null {
    if (selector === "#prompt") return this.prompt;
    if (selector === "#image-input") return this.input;
    if (selector === "[data-action='toggle-attachment-tray']") return this.toggle;
    if (selector === "[data-action='browse-image']") return this.browse;
    return null;
  }

  querySelectorAll(selector: string): FakeElement[] {
    return selector === "[data-focus-key]" ? this.attachments : [];
  }
}

test("attachment focus candidates preserve each mutation fallback order without focusing", () => {
  const dom = new FakeDocument();
  dom.activeElement = dom.body;

  assert.deepEqual(
    attachmentFocusCandidates(dom as unknown as Document, {
      kind: "attachment",
      path: "C:/workspace/two.png",
    }).map((candidate) => (candidate.resolve() as FakeElement | null)?.key ?? null),
    ["attachment:C:/workspace/two.png", "toggle", "prompt"],
  );
  assert.deepEqual(
    attachmentFocusCandidates(dom as unknown as Document, { kind: "browse" })
      .map((candidate) => (candidate.resolve() as FakeElement | null)?.key ?? null),
    ["browse", "input", "toggle", "prompt"],
  );
  assert.deepEqual(
    attachmentFocusCandidates(dom as unknown as Document, { kind: "toggle" })
      .map((candidate) => (candidate.resolve() as FakeElement | null)?.key ?? null),
    ["toggle", "prompt"],
  );
  assert.equal(dom.activeElement, dom.body);
});

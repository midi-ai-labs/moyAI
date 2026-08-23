import assert from "node:assert/strict";
import test from "node:test";

import * as agentExecutionPrepend from "../src/agent_execution_prepend_continuation.ts";
import * as attachment from "../src/attachment_focus_continuation.ts";
import * as quickChatDelete from "../src/quick_chat_delete_focus_continuation.ts";
import * as mainRun from "../src/run_focus_continuation.ts";
import * as settings from "../src/settings_surface.ts";
import * as sideChat from "../src/side_chat_focus_continuation.ts";

test("focus continuation modules expose candidates and ownership decisions, not focus writers", () => {
  const removedWriters: Array<[string, object, string]> = [
    ["attachment", attachment, "focusAttachmentTargetIfUnowned"],
    ["agent execution prepend", agentExecutionPrepend, "focusAgentExecutionPrependReturnIfUnowned"],
    ["quick-chat deletion", quickChatDelete, "focusQuickChatDeleteTargetIfUnowned"],
    ["main run", mainRun, "focusMainPromptIfUnowned"],
    ["side chat", sideChat, "focusSideChatPromptIfUnowned"],
    ["Settings action", settings, "restoreSettingsActionFocus"],
  ];

  for (const [label, moduleExports, writer] of removedWriters) {
    assert.equal(
      Object.hasOwn(moduleExports, writer),
      false,
      `${label} must delegate post-render focus writes to PostRenderFocusArbiter`,
    );
  }
});

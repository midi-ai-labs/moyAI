import assert from "node:assert/strict";
import test from "node:test";
import { ACTIONS } from "../src/actions.ts";
import { command, isDesktopCommandName } from "../src/api.ts";
import { renderMcpHistoryOverlay } from "../src/mcp_history_render.ts";

test("retired manual publication has no GUI actions or callable Desktop commands", async () => {
  assert.equal(ACTIONS.some(action => action.id === "show-mcp-publish" || action.id.startsWith("mcp-publish-")), false);
  for (const name of ["show_mcp_publish_editor", "mcp_publish_projection", "mcp_publish_save",
    "mcp_publish_delete", "mcp_publish_start", "mcp_publish_stop", "mcp_publish_issue_token",
    "mcp_publish_revoke_token", "mcp_publish_jobs", "mcp_publish_cancel_job",
    "mcp_publish_create_certificate", "mcp_publish_certificate"]) {
    assert.equal(isDesktopCommandName(name), false, name);
    await assert.rejects(command(name, {}), /Unknown Desktop command/);
  }
  for (const name of ["remote_job_cancel", "device_network_receiver", "mcp_history_list", "mcp_history_detail", "mcp_history_export", "mcp_history_stop"]) {
    assert.equal(isDesktopCommandName(name), true, name);
  }
});

test("retained legacy data is explained without offering restart or implicit authority migration", () => {
  const fresh = renderMcpHistoryOverlay(undefined, false);
  assert.doesNotMatch(fresh, /旧手動配信は廃止/);
  const legacy = renderMcpHistoryOverlay(undefined, true);
  assert.match(legacy, /旧手動配信は廃止/);
  assert.match(legacy, /設定・証明書・履歴は保持/);
  assert.match(legacy, /旧配信は再開しません/);
  assert.match(legacy, /対象と権限を確認/);
  for (const html of [fresh, legacy]) {
    assert.match(html, /data-action="show-hub"/);
    assert.doesNotMatch(html, /data-action="(?:show-mcp-publish|mcp-publish-)/);
  }
});

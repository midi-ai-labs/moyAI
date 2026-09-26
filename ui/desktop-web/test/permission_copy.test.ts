import assert from "node:assert/strict";
import test from "node:test";
import { permissionDetailLabel, permissionRiskLabel } from "../src/permission_copy.ts";
import { renderConfirmation } from "../src/render_overlays.ts";
import type { DesktopWebState } from "../src/types.ts";

test("local and shared risk representations convey the same potential effect", () => {
  for (const [local, shared] of [
    ["delete", "destructive_delete"], ["move/rename", "move_or_rename"],
    ["external connection/setup", "external_connection"],
    ["unclassified dynamic/indirect shell construct", "unclassified_shell"],
  ]) assert.equal(permissionRiskLabel(local), permissionRiskLabel(shared));
  assert.match(permissionRiskLabel("network"), /可能性/);
  assert.equal(permissionRiskLabel("future risk"), "future risk");
});

test("translated approval preserves exact command, Guardian explanation and original information", () => {
  const command = "Write-Output '<remote>'\nGet-Content ./projectbrief.md";
  const reason = "外部接続先の確認が必要です。";
  assert.equal(permissionDetailLabel(`Command: ${command}`), `実行コマンド: ${command}`);
  assert.equal(permissionDetailLabel("Canonical executable candidate (identity pinned): C:\\tools\\uv.exe"), "実行ファイル（確認済み）: C:\\tools\\uv.exe");
  const state = { confirmation_id: "review-a", confirmation: {
    summary: "Run shell command: preview", details: [`Command: ${command}`, "Workdir: C:\\work", `代理承認からの確認: ${reason}`],
    targets: ["C:\\work"], risks: ["network", "destructive_delete"], outside_workspace: false,
  } } as unknown as DesktopWebState;
  const html = renderConfirmation(state);
  assert.match(html, /ネットワーク通信を含む可能性/);
  assert.match(html, /削除を含む可能性/);
  assert.ok(html.includes(reason));
  assert.ok(html.indexOf(reason) < html.indexOf("実行コマンド:"), "reason must precede long operation details");
  assert.match(html, /コマンドを実行: preview/);
  assert.ok(html.includes("実行コマンド: Write-Output &#039;&lt;remote&gt;&#039;\nGet-Content ./projectbrief.md"));
  assert.match(html, /操作情報の原文/);
  assert.ok(html.includes("Command: Write-Output"));
  assert.doesNotMatch(html, /<remote>/);
});

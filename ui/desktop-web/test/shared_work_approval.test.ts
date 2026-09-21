import assert from "node:assert/strict";
import test from "node:test";
import { renderSharedWork } from "../src/shared_work_render.ts";
import type { SharedWorkProjection } from "../src/shared_work_state.ts";
import { escapeHtml } from "../src/utils.ts";
import { sharedUiFixture } from "./shared_work_fixture.ts";

type Request = NonNullable<SharedWorkProjection["approval"]>["request"];
function approval(request: Partial<Request> = {}) {
  const local = sharedUiFixture();
  local.projection!.approval = {
    id: "approval-a", attempt_id: "attempt-a", status: "pending", can_decide: true,
    decision: null, expires_at_ms: 9_999_999_999_999,
    request: {
      access: "shell", summary: "Run the CSV aggregation script", targets: ["C:/Approved", "C:/Windows/powershell.exe"],
      details: [
        "Command: & './Summarize-Numbers.ps1'\nGet-Content './onboarding-result.md'",
        "Workdir: C:/Approved",
        "Canonical executable candidate (identity pinned): C:/Windows/powershell.exe",
        "Workspace modes run this process in the native workspace-write OS sandbox; an approved elevation or Full Access runs it unrestricted under the current user account. The unelevated Windows backend uses advisory network controls rather than firewall enforcement.",
        "execution boundary: approval grants this process effect elevation outside the workspace-write OS sandbox",
      ],
      outside_workspace: false, risks: ["network", "external_connection", "unclassified_shell"],
      ...request,
    },
  };
  const html = renderSharedWork(local);
  const section = html.slice(html.indexOf('data-shared-region="approval"'));
  return { local, section, visible: section.slice(0, section.indexOf("<details")) };
}

test("shell approval presents the exact command and real permission effect before technical data", () => {
  const { local, section, visible } = approval();
  assert.match(visible, /実行コマンド/);
  assert.ok(visible.includes(escapeHtml("& './Summarize-Numbers.ps1'\nGet-Content './onboarding-result.md'")));
  assert.match(visible, /実行するフォルダー/);
  assert.match(visible, /C:\/Approved/);
  assert.match(visible, /作業フォルダー内に限定する保護を外して実行/);
  assert.match(visible, /対象以外のファイル操作や外部通信も可能/);
  assert.match(visible, /ネットワーク通信/);
  assert.match(visible, /コマンドの影響を自動では判定できません/);
  assert.doesNotMatch(visible, /Canonical executable|execution boundary|workspace-write|unclassified_shell|Run the CSV/);
  assert.ok(section.includes(escapeHtml(JSON.stringify(local.projection!.approval!.request, null, 2))), "original evidence is retained without translation or omission");
});

test("unrecognized details and risk categories stay visible and are escaped", () => {
  const { visible, section } = approval({
    details: ["Command: <script>alert(1)</script>", "New policy requirement: confirm <owner>"],
    risks: ["new_policy_<tag>"],
  });
  assert.match(visible, /New policy requirement: confirm &lt;owner&gt;/);
  assert.match(visible, /未対応の確認事項: new_policy_&lt;tag&gt;/);
  assert.match(visible, /&lt;script&gt;alert\(1\)&lt;\/script&gt;/);
  assert.doesNotMatch(section, /<script>|<owner>|<tag>/);
});

test("file access retains its specific scope without claiming process elevation", () => {
  const { visible } = approval({ access: "edit", summary: "設定ファイルを更新", details: ["変更箇所: timeout"], targets: ["C:/Other/settings.toml"], outside_workspace: true, risks: ["external_mutation"] });
  assert.match(visible, /設定ファイルを更新/);
  assert.match(visible, /C:\/Other\/settings.toml/);
  assert.match(visible, /作業フォルダー外への操作を含みます/);
  assert.match(visible, /外部システムの変更/);
  assert.doesNotMatch(visible, /保護を外して実行|外部通信も可能/);
});

test("missing command details do not imply that a shell request is safe or specified", () => {
  const { visible, section } = approval({ details: [], targets: [], risks: [] });
  assert.match(visible, /指定なし/);
  assert.match(visible, /具体的な操作内容が記載されていません/);
  assert.match(visible, /保護を外して実行/);
  assert.match(section, /Run the CSV aggregation script/);
});

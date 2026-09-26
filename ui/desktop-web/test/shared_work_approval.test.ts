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
  assert.match(visible, /影響を自動で判定できないコマンド/);
  assert.match(visible, /ネットワーク通信を含む可能性/);
  assert.match(visible, /実際に行う操作は、上のコマンドを確認/);
  assert.doesNotMatch(visible, /Canonical executable|execution boundary|workspace-write|unclassified_shell|Run the CSV/);
  assert.ok(section.includes(escapeHtml(JSON.stringify(local.projection!.approval!.request, null, 2))), "original evidence is retained without translation or omission");
});

test("Guardian reason precedes long shared commands and stays escaped", () => {
  const reason = "外部接続先 <target> の確認が必要です。";
  const { visible } = approval({ details: ["Command: echo test\n".repeat(64), `代理承認からの確認: ${reason}`] });
  assert.match(visible, /確認が必要な理由/);
  assert.ok(visible.indexOf(escapeHtml(reason)) < visible.indexOf("実行コマンド"));
  assert.doesNotMatch(visible, /<target>/);
});

test("unrecognized details and risk categories stay visible and are escaped", () => {
  const { visible, section } = approval({
    details: ["Command: <script>alert(1)</script>", "New policy requirement: confirm <owner>"],
    risks: ["new_policy_<tag>"],
  });
  assert.match(visible, /New policy requirement: confirm &lt;owner&gt;/);
  assert.match(visible, /操作前の確認事項: new_policy_&lt;tag&gt;/);
  assert.match(visible, /&lt;script&gt;alert\(1\)&lt;\/script&gt;/);
  assert.doesNotMatch(section, /<script>|<owner>|<tag>/);
});

test("file access retains its specific scope without claiming process elevation", () => {
  const { visible } = approval({ access: "edit", summary: "設定ファイルを更新", details: ["変更箇所: timeout"], targets: ["C:/Other/settings.toml"], outside_workspace: true, risks: ["external_mutation"] });
  assert.match(visible, /設定ファイルを更新/);
  assert.match(visible, /C:\/Other\/settings.toml/);
  assert.match(visible, /作業フォルダー外への操作を含みます/);
  assert.match(visible, /外部サービスのデータを変更する操作/);
  assert.doesNotMatch(visible, /保護を外して実行|外部通信も可能/);
});

test("missing command details do not imply that a shell request is safe or specified", () => {
  const { visible, section } = approval({ details: [], targets: [], risks: [] });
  assert.match(visible, /指定なし/);
  assert.match(visible, /具体的な操作内容が記載されていません/);
  assert.match(visible, /保護を外して実行/);
  assert.match(section, /Run the CSV aggregation script/);
});

test("stopping a retained app names the app and PC without describing it as a file edit", () => {
  const { local, visible, section } = approval({ access: "edit", summary: "この会話で起動したアプリを停止します。",
    targets: [], risks: [], details: ["操作: 起動中アプリの停止", "対象: WinB の起動中アプリ（ID: service-a）", "実行環境: env-b"] });
  assert.match(visible, /対象:<\/strong> WinB の起動中アプリ（ID: service-a）/);
  assert.match(visible, /実行環境: env-b/);
  assert.match(section, /操作種別: 起動中アプリの停止/);
  assert.doesNotMatch(section, /操作種別: ファイルの変更/);
  assert.doesNotMatch(visible, /指定なし|保護を外して実行|<pre>対象:/);
  assert.ok(section.includes(escapeHtml(JSON.stringify(local.projection!.approval!.request, null, 2))), "raw permission evidence remains unchanged");
});

test("descriptive approval details remain escaped and cannot replace actual path targets", () => {
  const { visible, section } = approval({ access: "edit", targets: ["C:/actual.txt"], risks: [],
    details: ["操作: <stop>", "対象: <server>", "未知: <keep>"] });
  assert.match(visible, /対象:<\/strong> C:\/actual.txt/);
  assert.match(visible, /対象: &lt;server&gt;|未知: &lt;keep&gt;/);
  assert.match(section, /操作種別: &lt;stop&gt;/);
  assert.doesNotMatch(section, /<stop>|<server>|<keep>/);
});

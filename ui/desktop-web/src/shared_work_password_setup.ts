import type { SharedWorkPresentation } from "./shared_work_state.ts";
import { sharedWorkActionEnabled } from "./shared_work_state.ts";
import { escapeHtml } from "./utils.ts";

export function renderPasswordSetup(local: SharedWorkPresentation): string {
  return `<div class="shared-password-setup"><div class="shared-login-fields"><label>本人設定コード<input id="shared-setup-code" data-shared-field="setupCode" type="password" autocomplete="off" value="${escapeHtml(local.setupCode)}" ${local.pending ? "disabled" : ""}></label><label>新しいパスワードの確認<input id="shared-setup-confirm" data-shared-field="setupPasswordConfirm" type="password" autocomplete="new-password" value="${escapeHtml(local.setupPasswordConfirm)}" ${local.pending ? "disabled" : ""}></label></div><button data-action="shared-setup-password" ${sharedWorkActionEnabled(local, "setup-password", "") ? "" : "disabled"}>パスワードを設定して利用する</button><p class="shared-secondary">コードは発行から24時間、一度だけ使えます。期限切れ・紛失時は管理者に再発行を依頼してください。</p></div>`;
}

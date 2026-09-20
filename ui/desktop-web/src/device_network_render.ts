import { escapeHtml } from "./utils.ts";
import { renderDeviceDiagnostic, deviceCanDiagnose } from "./device_network_diagnostics.ts";
import { renderDeviceExecution } from "./device_execution.ts";
import { createDeviceNetworkUiState, deviceCanJoin, deviceNetworkError, type DeviceNetworkPresentation } from "./device_network_state.ts";

const enrollmentText = { unconfigured: "設定ファイルを読み込んでください", not_enrolled: "参加申請の準備中", pending: "管理者の参加承認待ち", active: "Hubに接続済み", stopped: "管理者が利用を停止中", expired: "参加申請を更新中", disconnected: "Hubに未接続", revoked: "このPCの認証が失効しています", error: "接続を確認してください" };
export function renderDeviceNetwork(input?: DeviceNetworkPresentation): string {
  const local = input ?? createDeviceNetworkUiState(), p = local.projection;
  const enrollment = p?.enrollment ?? "unconfigured", member = Boolean(p?.device_id), busy = Boolean(local.pending);
  const error = local.error || (p?.error ? deviceNetworkError(p.error) : "");
  return `<div class="device-network-content"><p class="hub-scope-note">まず接続ファイルを読み込み、このPCの参加を申請します。依頼・結果を見る方は「ログイン・プロジェクトへ」、実行用のPCは下の「このPCで仕事を実行」へ進んでください。</p>
    <div id="device-network-feedback" class="hub-feedback" data-settings-passive="device-network-feedback" data-error="${Boolean(error)}" role="status" aria-live="polite" ${error || local.notice ? "" : "hidden"}>${escapeHtml(error || local.notice)}</div>
    <section class="device-network-card"><div class="hub-section-heading"><h3>Hubへの接続</h3><span data-settings-passive="device-network-enrollment" class="device-network-status ${enrollment === "active" ? "ready" : "muted"}">${enrollmentText[enrollment]}</span></div>
      <p class="hub-help" data-settings-passive="device-network-self">このPC: ${escapeHtml(p?.display_name || p?.local_hostname || "確認中")}</p>
      <div class="device-network-actions"><button id="device-network-import" data-action="device-network-import" ${busy ? "disabled" : ""}>${p?.hub_url ? "Hubの設定ファイルを変更" : "Hubの設定ファイルを読み込む"}</button><button id="device-network-open-shared" data-action="show-shared-work">ログイン・プロジェクトへ</button></div>
      <p class="hub-help">管理者から受け取った接続ファイル（.moyai-join または .toml）を読み込むと、このPCの参加を申請します。承認後にログインすれば、参加中のプロジェクトを利用できます。</p>
      <p class="hub-help" role="status" data-settings-passive="device-network-join-status">${enrollment === "pending" ? "参加申請を送信しました。管理者の承認後に自動で接続します。" : enrollment === "revoked" ? "このPCの登録状態を管理者に確認してください。" : "プロジェクトの参加者と、操作・実行に使うPCはHub管理者が設定します。"}</p>
      <details id="device-network-details" data-details-key="device-network-details"><summary>接続の詳細・診断</summary><p>登録済みPCは、同じHub・同じ公開CAの接続先変更だけに対応します。実行PCでは受付を一時停止し、実行中・状態不明の仕事がないことを確認してください。変更後も実行許可を保持し、受付は明示的に再開します。</p><div class="device-network-technical" data-settings-passive="device-network-details"><p>Hub: ${escapeHtml(p?.hub_url || "未設定")}</p><p>端末ID: ${escapeHtml(p?.device_id ?? "未登録")}</p></div><div class="device-network-actions"><button id="device-network-refresh" data-action="device-network-refresh" ${busy ? "disabled" : ""}>再接続・最新情報を取得</button><button id="device-network-diagnose-hub" data-action="device-network-diagnose-hub" ${deviceCanDiagnose(local, "hub") ? "" : "disabled"}>Hubへの接続を診断</button><button id="device-network-join" data-action="device-network-join" ${deviceCanJoin(local) ? "" : "disabled"}>参加申請を再試行</button></div>${renderDeviceDiagnostic(local, "hub")}<details data-details-key="device-network-gateway"><summary>このPCでHubのAIを使う場合の診断</summary><p>仕事を依頼・閲覧するだけのPCでは、この確認は不要です。直接接続するAIは、AIの接続設定から確認してください。</p><button data-action="device-network-diagnose-gateway" ${deviceCanDiagnose(local, "gateway") ? "" : "disabled"}>HubのAIへの接続を診断</button>${renderDeviceDiagnostic(local, "gateway")}</details></details>
    </section>
    ${renderDeviceExecution(local)}
    <details class="device-network-card" data-details-key="device-network-models"><summary>このPCのチャットでHubのモデルを利用</summary><p class="hub-help">ローカルで実行するチャットのAIモデルを選びます。プロジェクトの依頼先となるPCの設定は、Hub管理者が行います。</p><button id="device-network-open-models" data-action="hub-tab-models">モデルを選ぶ</button></details>
    <details class="device-network-leave" id="device-network-leave-details" data-details-key="device-network-leave-details" data-network-visible="member" ${member ? "" : "hidden"}><summary>接続を一時解除</summary><p class="hub-help">PCの登録を保持して接続を解除します。同じ設定で再接続できます。</p><label class="device-network-check"><input id="device-network-leave-confirmed" class="settings-control" type="checkbox" data-network-field="leave_confirmed" ${local.leaveConfirmed ? "checked" : ""}/><span>このPCのHub接続を一時解除する</span></label><button data-action="device-network-leave" ${!busy && p?.can_leave && local.leaveConfirmed ? "" : "disabled"}>接続を一時解除</button></details>
  </div>`;
}

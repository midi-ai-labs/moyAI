import { escapeHtml } from "./utils.ts";
import { renderDeviceDiagnostic, deviceCanDiagnose } from "./device_network_diagnostics.ts";
import { renderDeviceExecution } from "./device_execution.ts";
import { createDeviceNetworkUiState, deviceCanJoin, deviceNetworkError, devicePeerKey, type DeviceNetworkPresentation } from "./device_network_state.ts";
import type { SharedWorkPresentation } from "./shared_work_state.ts";

const enrollmentText = { unconfigured: "設定ファイルを読み込んでください", not_enrolled: "参加申請の準備中", pending: "管理者の参加承認待ち", active: "Hubに接続済み", stopped: "管理者が利用を停止中", expired: "参加申請を更新中", disconnected: "Hubに未接続", revoked: "このPCの認証が失効しています", error: "接続を確認してください" };
function connectionNextStep(enrollment: keyof typeof enrollmentText): string {
  if (enrollment === "pending") return "参加申請を送信しました。次はHub管理者が、このPCの参加を承認します。承認後に自動で接続するので、この画面のまま待てます。";
  if (enrollment === "active") return "Hubに接続できました。依頼・閲覧するPCは「Hubのプロジェクトへ」進んでください。このPCで仕事も実行する場合は、下の「このPCで仕事を実行」を設定します。";
  if (enrollment === "revoked" || enrollment === "stopped") return "Hub管理者にこのPCの登録状態を確認してください。接続設定をやり直す必要がある場合は「接続設定をリセット」を使えます。";
  if (enrollment === "not_enrolled" || enrollment === "expired") return "このPCの参加申請を準備しています。申請後はHub管理者の承認を待ちます。";
  if (enrollment === "disconnected" || enrollment === "error") return "「接続の詳細・診断」から再接続、またはHubへの接続診断を試してください。";
  return "まず管理者から接続ファイルを受け取り、「Hubの設定ファイルを読み込む」を押してください。";
}
export function renderDeviceNetwork(input?: DeviceNetworkPresentation, shared?: SharedWorkPresentation): string {
  const local = input ?? createDeviceNetworkUiState(), p = local.projection;
  const enrollment = p?.enrollment ?? "unconfigured", member = Boolean(p?.device_id), busy = Boolean(local.pending);
  const error = local.error || (p?.error ? deviceNetworkError(p.error) : "");
  return `<div class="device-network-content"><p class="hub-scope-note">接続ファイルを読み込む → Hub管理者が参加を承認 → プロジェクトを利用、の順で進めます。</p>
    <div id="device-network-feedback" class="hub-feedback" data-settings-passive="device-network-feedback" data-error="${Boolean(error)}" role="status" aria-live="polite" ${error || local.notice ? "" : "hidden"}>${escapeHtml(error || local.notice)}</div>
    <section class="device-network-card"><div class="hub-section-heading"><h3>Hubへの接続</h3><span data-settings-passive="device-network-enrollment" class="device-network-status ${enrollment === "active" ? "ready" : "muted"}">${enrollmentText[enrollment]}</span></div>
      <p class="hub-help" data-settings-passive="device-network-self">このPC: ${escapeHtml(p?.display_name || p?.local_hostname || "確認中")}</p>
      <div class="device-network-actions"><button id="device-network-import" data-action="device-network-import" ${busy ? "disabled" : ""}>${p?.hub_url ? "Hubの設定ファイルを変更" : "Hubの設定ファイルを読み込む"}</button><button id="device-network-open-shared" data-action="show-shared-work">Hubのプロジェクトへ</button></div>
      <p class="hub-help">管理者から受け取った接続ファイル（.moyai-join または .toml）を読み込むと、このPCの参加を申請します。プロジェクトの参加者と、操作・実行に使うPCはHub管理者が設定します。</p>
      <p class="hub-help" role="status" data-settings-passive="device-network-join-status">${connectionNextStep(enrollment)}</p>
      <details id="device-network-details" data-details-key="device-network-details"><summary>接続の詳細・診断</summary><p>登録済みPCは、同じHub・同じ公開CAの接続先変更だけに対応します。実行PCでは受付を一時停止し、実行中・状態不明の仕事がないことを確認してください。変更後も実行許可を保持し、受付は明示的に再開します。</p><div class="device-network-technical" data-settings-passive="device-network-details"><p>Hub: ${escapeHtml(p?.hub_url || "未設定")}</p><p>端末ID: ${escapeHtml(p?.device_id ?? "未登録")}</p></div><div class="device-network-actions"><button id="device-network-refresh" data-action="device-network-refresh" ${busy ? "disabled" : ""}>再接続・最新情報を取得</button><button id="device-network-diagnose-hub" data-action="device-network-diagnose-hub" ${deviceCanDiagnose(local, "hub") ? "" : "disabled"}>Hubへの接続を診断</button><button id="device-network-join" data-action="device-network-join" ${deviceCanJoin(local) ? "" : "disabled"}>参加申請を再試行</button></div>${renderDeviceDiagnostic(local, "hub")}<details data-details-key="device-network-gateway"><summary>このPCでHubのAIを使う場合の診断</summary><p>仕事を依頼・閲覧するだけのPCでは、この確認は不要です。直接接続するAIは、AIの接続設定から確認してください。</p><button data-action="device-network-diagnose-gateway" ${deviceCanDiagnose(local, "gateway") ? "" : "disabled"}>HubのAIへの接続を診断</button>${renderDeviceDiagnostic(local, "gateway")}</details></details>
    </section>
    ${renderDeviceConnectionReset(local)}
    ${renderSavedDevicePeers(local)}
    ${renderDeviceExecution(local, shared)}
    <details class="device-network-card" data-details-key="device-network-models"><summary>このPCのチャットでHubのモデルを利用</summary><p class="hub-help">ローカルで実行するチャットのAIモデルを選びます。プロジェクトの依頼先となるPCの設定は、Hub管理者が行います。</p><button id="device-network-open-models" data-action="hub-tab-models">モデルを選ぶ</button></details>
    <details class="device-network-leave" id="device-network-leave-details" data-details-key="device-network-leave-details" data-network-visible="member" ${member ? "" : "hidden"}><summary>接続を一時解除</summary><p class="hub-help">PCの登録を保持して接続を解除します。同じ設定で再接続できます。</p><label class="device-network-check"><input id="device-network-leave-confirmed" class="settings-control" type="checkbox" data-network-field="leave_confirmed" ${local.leaveConfirmed ? "checked" : ""}/><span>このPCのHub接続を一時解除する</span></label><button data-action="device-network-leave" ${!busy && p?.can_leave && local.leaveConfirmed ? "" : "disabled"}>接続を一時解除</button></details>
  </div>`;
}

function renderSavedDevicePeers(local: DeviceNetworkPresentation): string {
  const peers = local.projection?.peers.filter(peer => peer.selected) ?? [];
  return `<div data-settings-passive="device-network-saved-peers" data-settings-preserve-focused-region>${peers.length ? `<details class="device-network-card" data-details-key="device-network-saved-peers"><summary>以前の接続で保存した利用先（${peers.length}件）</summary><p class="hub-help">このPCに残った旧利用先を削除します。Hubの許可・履歴・実行中の仕事は変わりません。</p><div class="device-network-peers">${peers.map((peer, index) => { const key = devicePeerKey(peer); return `<section class="device-network-peer"><div><strong>${escapeHtml(peer.display_name || peer.device_id)}</strong><p class="device-network-technical">端末ID: ${escapeHtml(peer.device_id)}<br>公開対象: ${escapeHtml(peer.name || peer.profile_id)}<br>公開ID: ${escapeHtml(peer.profile_id)}</p><label class="device-network-check" for="device-network-delete-peer-${index}"><input id="device-network-delete-peer-${index}" type="checkbox" data-network-field="delete_peer" value="${escapeHtml(key)}" ${local.deletePeerKey === key ? "checked" : ""} ${local.pending ? "disabled" : ""}><span>この保存済み利用先を削除する</span></label><div class="device-network-actions"><button data-action="device-network-delete-peer" data-value="${escapeHtml(key)}" ${!local.pending && local.deletePeerKey === key ? "" : "disabled"}>この利用先を削除</button></div></div></section>`; }).join("")}</div></details>` : ""}</div>`;
}

export function renderDeviceConnectionReset(input?: DeviceNetworkPresentation): string {
  const local = input ?? createDeviceNetworkUiState(), p = local.projection, busy = Boolean(local.pending);
  const error = local.error || (p?.error ? deviceNetworkError(p.error) : "");
  return `<details class="device-network-card" id="device-network-reset-details" data-details-key="device-network-reset-details"><summary>接続設定をリセット</summary><p class="hub-help">Hubが使えない場合も、このPCだけで接続設定を解除できます。</p><p class="hub-help" id="device-network-reset-impact">このPCの登録・モデル選択・実行許可を解除します。履歴・成果物・作業フォルダー・手動AI設定は残ります。実行中の仕事には停止を要求しますが、停止や完了は確認できません。旧Hubの登録は残ります。</p><label class="device-network-check" for="device-network-reset-confirmed"><input aria-describedby="device-network-reset-impact" id="device-network-reset-confirmed" class="settings-control" type="checkbox" data-network-field="reset_confirmed" ${local.resetConfirmed ? "checked" : ""} ${busy ? "disabled" : ""}/><span>影響を確認し、接続設定をリセットする</span></label><button id="device-network-reset" data-action="device-network-reset" ${!busy && p && local.resetConfirmed ? "" : "disabled"}>接続設定をリセット</button><div id="device-network-reset-feedback" class="hub-feedback" data-settings-passive="device-network-reset-feedback" data-error="${Boolean(error)}" role="status" aria-live="polite" ${error || local.notice ? "" : "hidden"}>${escapeHtml(error || local.notice)}</div></details>`;
}

import { escapeHtml } from "./utils.ts";
import { renderDeviceNetworkJobs } from "./device_network_jobs.ts";
import { deviceCanDiagnose, deviceLocalIpv4Choices, renderDeviceDiagnostic } from "./device_network_diagnostics.ts";
import { createDeviceNetworkUiState, deviceCanJoin, deviceCanReceive, deviceCanSelect, deviceDraftCurrent,
  deviceNetworkError, devicePeerAvailability, devicePeerKey, deviceReceiverBindError, deviceReceiverConfirmed, deviceTargetKey, visibleDevicePeers,
  type DeviceNetworkPresentation } from "./device_network_state.ts";

const enrollmentText = { unconfigured: "共通設定を読み込んでください", not_enrolled: "参加申請の準備中", pending: "Hub管理者の承認待ち",
  active: "Hubに参加済み", stopped: "Hub管理者による利用停止中", expired: "参加申請を更新しています", disconnected: "Hubとの接続が切れています", revoked: "端末の認証が失効しています", error: "接続を確認してください" };
const accessHelp = { default: "確認が必要な操作は、この端末に表示する承認画面で判断します。依頼元には承認待ちと表示されます。",
  auto_review: "確認が必要な操作を受入端末のAIが審査します。人が確認する場合は「承認を求める」を選んでください。",
  full_access: "このPCの現在のユーザー権限で実行します。実行する内容を信頼できる端末だけに公開してください。" };
export function renderDeviceNetwork(input?: DeviceNetworkPresentation): string {
  const local = input ?? createDeviceNetworkUiState();
  const projection = local.projection;
  const enrollment = projection?.enrollment ?? "unconfigured";
  const busy = Boolean(local.pending);
  const active = enrollment === "active";
  const member = Boolean(projection?.device_id);
  const status = enrollmentText[enrollment];
  const peers = visibleDevicePeers(local);
  const targets = projection?.targets.filter(row => row.target.kind === "project" || row.target.kind === "temp") ?? [{ target: { kind: "temp" } as const, label: "Temp（一時作業用）" }];
  const receiver = projection?.receiver;
  const receiverStatus = receiver?.status === "receiving" ? "受付中" : receiver?.status === "starting" ? "受付を準備中"
    : receiver?.status === "stopping" ? "停止を確認中" : receiver?.status === "error" ? "受付できません"
      : receiver?.status === "paused" || receiver?.status === "stopped" ? "停止中" : "状態を確認しています";
  const error = local.error || (projection?.error ? deviceNetworkError(projection.error) : "");
  const feedback = error || local.notice;
  return `<div class="device-network-content">
    <p class="hub-scope-note">Hubに参加すると、許可された端末を検索してタスクの委任先に選べます。この端末でも、ほかの端末への依頼とタスクの受付を両方使えます。</p>
    <div id="device-network-feedback" class="hub-feedback" data-settings-passive="device-network-feedback" data-error="${Boolean(error)}" role="status" aria-live="polite" ${feedback ? "" : "hidden"}>${escapeHtml(feedback)}</div>
    <section class="device-network-card"><div class="hub-section-heading"><h3>Hubへの参加</h3><span data-settings-passive="device-network-enrollment" class="device-network-status ${active ? "ready" : "muted"}">${status}</span></div>
      <p class="hub-help" data-settings-passive="device-network-self">このPC: ${escapeHtml(projection?.local_hostname || "確認中")} · ${projection?.display_name ? `登録名: ${escapeHtml(projection.display_name)}` : "端末名とIPv4を自動で申請します。"}${projection?.local_ipv4 ? ` · 申請IPv4: ${escapeHtml(projection.local_ipv4)}` : ""}</p>
      <div class="device-network-actions"><button id="device-network-import" data-action="device-network-import" ${busy ? "disabled" : ""}>Hubの共通設定ファイルを読み込む</button><button id="device-network-refresh" data-action="device-network-refresh" ${busy ? "disabled" : ""}>${enrollment === "disconnected" ? "再接続" : "最新情報を取得"}</button></div>
      <p class="hub-help">管理者から受け取った共通TOML設定を読み込むと、この端末の参加を自動で申請します。名前や参加コードの入力は不要です。モデルや手動MCPの既存設定は置き換えません。</p>
      <button id="device-network-diagnose-hub" data-action="device-network-diagnose-hub" ${deviceCanDiagnose(local, "hub") ? "" : "disabled"}>Hubへの接続を診断</button>
      <details data-details-key="device-network-hub-diagnostic"><summary>Hub接続の診断結果</summary>${renderDeviceDiagnostic(local, "hub")}</details>
      <div data-network-visible="join" ${member ? "hidden" : ""}>
        <p class="hub-help" role="status" data-settings-passive="device-network-join-status">${enrollment === "pending" ? "Hubの管理画面で、この端末の参加申請を許可してください。承認後は自動で証明書を取得して接続します。再起動しても同じ申請を確認します。" : enrollment === "stopped" ? "Hub管理者が利用を停止しています。再許可を待っています。" : enrollment === "revoked" ? "この端末の資格が失効しています。自動で再申請はしません。Hub管理者へ確認してください。" : "共通設定の読込後、接続できなかった場合は最新情報を取得するか申請を再試行できます。"}</p>
        <button id="device-network-join" class="hub-primary" data-action="device-network-join" ${deviceCanJoin(local) ? "" : "disabled"}>参加申請を再試行</button>
      </div>
      <details id="device-network-details" data-details-key="device-network-details"><summary>接続の詳細</summary><div class="device-network-technical" data-settings-passive="device-network-details"><p>Hub: ${escapeHtml(projection?.hub_url || "未設定")}</p><p>端末ID: ${escapeHtml(projection?.device_id ?? "未登録")}</p>${projection?.request_id ? `<p>参加申請ID: ${escapeHtml(projection.request_id)}</p>` : ""}<p>受付先: ${escapeHtml(receiver?.endpoint ?? "停止中")}</p><p>通常はIP・ポート・証明書を自動設定します。受付IPとポートは下の詳細で指定できます。秘密鍵と一時的な接続資格情報は表示しません。</p></div></details>
    </section>
    <section class="device-network-card" data-network-visible="member" ${member ? "" : "hidden"} aria-labelledby="device-network-receiver-title">
      <div class="hub-section-heading"><h3 id="device-network-receiver-title">この端末でタスクを受け付ける</h3><span class="device-network-status" data-settings-passive="device-network-receiver-status">受付 ${receiver?.enabled ? "ON" : "OFF"} · ${receiverStatus}</span></div>
      <p class="hub-help">初回はTempでの受付を案内します。必要な公開対象と権限を確認して開始してください。受付を始めずに送信側だけとして使うこともできます。</p>
      <div class="device-network-fields"><label class="hub-field">公開する作業場所<div data-settings-passive="device-network-targets" data-settings-preserve-focused-region><select id="device-network-target" class="settings-control" data-network-field="target" ${busy || !receiver?.can_change ? "disabled" : ""}>${targets.map(row => `<option value="${escapeHtml(deviceTargetKey(row.target))}" ${JSON.stringify(row.target) === JSON.stringify(local.target) ? "selected" : ""}>${escapeHtml(row.label)}</option>`).join("")}</select></div></label>
        <label class="hub-field">実行権限<select id="device-network-access" class="settings-control" data-network-field="access" ${busy || !receiver?.can_change ? "disabled" : ""}><option value="default" ${local.accessMode === "default" ? "selected" : ""}>承認を求める</option><option value="auto_review" ${local.accessMode === "auto_review" ? "selected" : ""}>代理で承認</option><option value="full_access" ${local.accessMode === "full_access" ? "selected" : ""}>フルアクセス</option></select></label></div>
      <p class="hub-help" data-settings-passive="device-network-access-help">${accessHelp[local.accessMode]}</p>
      <details id="device-network-bind-details" data-details-key="device-network-bind-details"><summary>受付IPとポートの詳細</summary>
        <div class="device-network-fields"><label class="hub-field">受付IP（IPv4）<input id="device-network-bind-ip" class="settings-control" data-network-field="bind_ip" value="${escapeHtml(local.bindIp)}" list="device-network-local-ipv4" placeholder="自動（Hubへの通信経路）" autocomplete="off" spellcheck="false" aria-describedby="device-network-bind-help device-network-bind-error" ${busy || !receiver?.can_change ? "disabled" : ""}/></label>
        <label class="hub-field">受付ポート<input id="device-network-port" class="settings-control" data-network-field="port" value="${escapeHtml(local.port)}" placeholder="自動（7332、使用中なら空きポート）" inputmode="numeric" autocomplete="off" aria-describedby="device-network-bind-help device-network-bind-error" ${busy || !receiver?.can_change ? "disabled" : ""}/></label></div>
        <p id="device-network-bind-help" class="hub-help">空欄は自動設定です。複数のネットワークを使う場合は、依頼元から届くこのPCのIPv4を指定できます。固定ポートが使用中の場合は別のポートへ切り替えず、開始できなかった理由を表示します。変更は受付設定の保存時に適用します。</p>
        <p id="device-network-bind-error" class="hub-help" role="status" data-settings-passive="device-network-bind-error">${escapeHtml(deviceReceiverBindError(local))}</p>
        <datalist id="device-network-local-ipv4" data-settings-passive="device-network-local-ipv4">${deviceLocalIpv4Choices(local).map(ip => `<option value="${escapeHtml(ip)}"></option>`).join("")}</datalist>
        <button id="device-network-diagnose-receiver" data-action="device-network-diagnose-receiver" ${deviceCanDiagnose(local, "receiver") ? "" : "disabled"}>保存済みの受付を診断</button><p class="hub-help">入力中の変更は保存後に診断します。Windowsのファイアウォール設定は自動で変更しません。</p>
        ${renderDeviceDiagnostic(local, "receiver")}
      </details>
      <details id="device-network-background" data-details-key="device-network-background"><summary>起動時とウィンドウを隠した時の受付</summary><label class="device-network-check"><input id="device-network-start-on-launch" class="settings-control" type="checkbox" data-network-field="start_on_launch" ${local.startOnLaunch ? "checked" : ""} ${busy || !receiver?.can_change ? "disabled" : ""}/><span>次回のアプリ起動時に受付を開始する</span></label><label class="device-network-check"><input id="device-network-keep-hidden" class="settings-control" type="checkbox" data-network-field="keep_when_hidden" ${local.keepWhenHidden ? "checked" : ""} ${busy || !receiver?.can_change ? "disabled" : ""}/><span>ウィンドウを隠しても受付を続ける</span></label><p class="hub-help">既定はどちらもOFFです。ここで選択した内容も、公開対象と権限を確認して受付設定を保存すると適用します。</p></details>
      <details id="device-network-model-details" data-details-key="device-network-model-details"><summary>モデルの割当と互換設定</summary><label class="hub-field">受付タスクのモデル<select id="device-network-model" class="settings-control" data-network-field="model" ${busy || !receiver?.can_change ? "disabled" : ""}><option value="hub" ${local.modelMode === "hub" ? "selected" : ""}>Hubのモデル割当を利用</option><option value="direct" ${local.modelMode === "direct" ? "selected" : ""}>この端末のMain直接接続設定</option></select></label><p class="hub-help">Hub利用時のモデルは「モデル割当」タブで確認します。直接接続は既存構成との互換用です。</p><button data-action="hub-tab-models">モデル割当を確認</button></details>
      <label class="device-network-check"><input id="device-network-receiver-confirmed" class="settings-control" type="checkbox" data-network-field="receiver_confirmed" ${deviceReceiverConfirmed(local) ? "checked" : ""} ${busy || !receiver?.can_change || (receiver?.confirmed && deviceReceiverConfirmed(local) && !local.receiverConfirmed) ? "disabled" : ""}/><span>公開する作業場所と実行権限を確認しました。許可された端末からのタスクを、この範囲で受け付けます。同じ範囲で再開する場合は保存済みの確認を使います。</span></label>
      <p id="device-network-receiver-feedback" class="hub-help" data-settings-passive="device-network-receiver-feedback" role="status">${escapeHtml(local.dirty && !deviceDraftCurrent(local) ? "設定が更新されています。最新情報を取得し、入力中の内容を確認してください。" : receiver?.reason ? deviceNetworkError(receiver.reason) : (local.dirty ? "設定に未保存の変更があります。確認して受付設定を保存してください。" : "受付OFFと、実行中タスクの停止完了は別の状態です。"))}</p>
      <div class="device-network-actions"><button id="device-network-receiver-on" class="hub-primary" data-action="device-network-receiver-on" ${deviceCanReceive(local, true) ? "" : "disabled"}>${receiver?.enabled ? "確認して受付設定を保存" : "確認して受付ON"}</button><button id="device-network-receiver-off" data-action="device-network-receiver-off" ${deviceCanReceive(local, false) ? "" : "disabled"}>受付OFF</button></div>
    </section>
    <section class="device-network-card" data-network-visible="member" ${member ? "" : "hidden"} aria-labelledby="device-network-peers-title"><div class="hub-section-heading"><h3 id="device-network-peers-title">利用する端末を選ぶ</h3></div><p class="hub-help">Hubが接続を許可した端末だけを利用先にできます。利用ONは送信側の選択です。実際の利用可否は、相手の受付・接続状態と別に表示します。</p>
      <label class="hub-field">端末を検索<input id="device-network-search" class="settings-control" data-network-field="search" type="search" value="${escapeHtml(local.search)}" placeholder="端末名・公開対象・端末ID" /></label>
      <div class="device-network-peers" data-settings-passive="device-network-peer-list" data-settings-preserve-focused-region>${peers.length ? peers.map(peer => {
        const availability = devicePeerAvailability(peer); const key = devicePeerKey(peer);
        const saving = local.pending === "select" && local.selectionKey === key;
        const selectionError = local.selectionKey === key ? local.error : "";
        const selectionStatus = saving ? "保存しています…" : `${peer.selected ? "利用先に選択済み" : "利用先に選択していません"}${selectionError ? `。${selectionError} 表示は最後に確認した保存状態です。` : ""}`;
        const statusId = `device-network-selection-${encodeURIComponent(key)}`;
        return `<article class="device-network-peer"><div><div data-settings-passive="device-peer-name-${escapeHtml(key)}"><strong>${escapeHtml(peer.display_name)}</strong>${peer.name ? `<span>${escapeHtml(peer.name)}</span>` : ""}</div><p data-settings-passive="device-peer-availability-${escapeHtml(key)}" class="device-network-status ${availability.tone}">${availability.label}</p><p data-settings-passive="device-peer-reason-${escapeHtml(key)}" class="hub-help">${escapeHtml(peer.reason ? deviceNetworkError(peer.reason) : "相手の端末が公開対象と実行権限を管理します。接続は実際の依頼時に確認します。事前の確認には接続診断を使えます。")}</p><details data-details-key="device-peer-${escapeHtml(key)}"><summary>端末の識別情報</summary><code>${escapeHtml(peer.device_id)} / ${escapeHtml(peer.profile_id)}</code></details><details data-details-key="device-peer-diagnostic-${escapeHtml(key)}"><summary>この端末への診断結果</summary>${renderDeviceDiagnostic(local, "peer", key)}</details></div><div class="device-network-peer-actions"><button type="button" id="device-network-use-${encodeURIComponent(key)}" class="device-network-use-switch" role="switch" data-action="device-network-select" data-value="${escapeHtml(key)}" aria-checked="${peer.selected}" aria-busy="${saving}" aria-label="${escapeHtml([peer.display_name, peer.name, "この端末を利用"].filter(Boolean).join(" · "))}" aria-describedby="${statusId}" ${deviceCanSelect(local, key) ? "" : "disabled"}><span>この端末を利用</span><span class="device-network-switch-state" aria-hidden="true"><span class="device-network-switch-track"><span class="device-network-switch-thumb"></span></span><span>${peer.selected ? "ON" : "OFF"}</span></span></button><p id="${statusId}" class="hub-help device-network-selection-status" role="status" data-settings-passive="device-peer-selection-${escapeHtml(key)}" data-error="${Boolean(selectionError)}">${escapeHtml(selectionStatus)}</p><button id="device-network-diagnose-${encodeURIComponent(key)}" data-action="device-network-diagnose-peer" data-value="${escapeHtml(key)}" ${deviceCanDiagnose(local, "peer", key) ? "" : "disabled"}>この端末への接続を診断</button></div></article>`;
      }).join("") : '<p class="hub-empty">選択できる端末がありません。検索条件、Hubの接続許可、相手の受付設定を確認してください。</p>'}</div>
    </section>
    ${renderDeviceNetworkJobs(local)}
    <details class="device-network-leave" id="device-network-leave-details" data-details-key="device-network-leave-details" data-network-visible="member" ${member ? "" : "hidden"}><summary>接続を一時解除</summary><p class="hub-help">登録ID・公開対象・実行権限・利用先の選択は保持します。「再接続」で同じ端末として接続でき、受付は手動でONにします。手動MCPと直接接続の設定は別に管理されます。</p><label class="device-network-check"><input id="device-network-leave-confirmed" class="settings-control" type="checkbox" data-network-field="leave_confirmed" ${local.leaveConfirmed ? "checked" : ""}/><span>この端末のHub接続を一時解除する</span></label><button data-action="device-network-leave" ${!busy && projection?.can_leave && local.leaveConfirmed ? "" : "disabled"}>接続を一時解除</button></details>
  </div>`;
}

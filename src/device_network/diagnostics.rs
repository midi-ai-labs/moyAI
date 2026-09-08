//! User-requested, read-only connectivity checks. Never register jobs or change peers.

use super::{DeviceClient, DeviceError, DeviceNetworkService};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticScope {
    Hub,
    Receiver,
    Peer,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticStage {
    pub key: String,
    pub label: String,
    pub status: &'static str,
    pub detail: String,
    pub hint: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct DeviceDiagnostic {
    pub scope: DiagnosticScope,
    pub device_id: Option<String>,
    pub profile_id: Option<String>,
    pub revision: String,
    pub generation: String,
    pub checked_at: String,
    pub stages: Vec<DiagnosticStage>,
    /// IPv4 addresses actually observed on the selected local route.
    pub local_ipv4: Vec<String>,
}
impl DeviceDiagnostic {
    fn stage(
        &mut self,
        key: &str,
        label: &str,
        status: &'static str,
        detail: impl Into<String>,
        hint: Option<&str>,
    ) {
        self.stages.push(DiagnosticStage {
            key: key.into(),
            label: label.into(),
            status,
            detail: detail.into(),
            hint: hint.map(str::to_owned),
        });
    }
}

#[derive(Deserialize)]
struct HubDiagnostic {
    hub_id: String,
    gateway_endpoint: Option<String>,
    gateway_ready: bool,
    certificate: Option<ServerCertificateDiagnostic>,
    gateway_certificate: Option<ServerCertificateDiagnostic>,
}

#[derive(Deserialize)]
struct ServerCertificateDiagnostic {
    expires_at_ms: String,
    renew_after_ms: String,
    last_error: Option<String>,
}

impl DeviceDiagnostic {
    fn server_certificate(
        &mut self,
        key: &str,
        label: &str,
        certificate: Option<&ServerCertificateDiagnostic>,
        now: u64,
    ) {
        let Some(certificate) = certificate else {
            self.stage(
                key,
                label,
                "skipped",
                "証明書の更新情報を取得できません。",
                Some("Hubの証明書欄で更新状態を確認してください。"),
            );
            return;
        };
        let expiry = certificate.expires_at_ms.parse::<u64>().ok();
        let renew = certificate.renew_after_ms.parse::<u64>().ok();
        let valid = expiry.zip(renew).filter(|(expiry, renew)| renew <= expiry);
        let (status, detail, hint) = match valid {
            None => (
                "fail",
                "証明書の更新情報が不正です。",
                Some("Hubの証明書欄を確認してください。"),
            ),
            Some((expiry, _)) if expiry <= now => (
                "fail",
                "証明書の有効期限を過ぎています。",
                Some("Hubで証明書の更新結果を確認してください。"),
            ),
            Some(_) if certificate.last_error.is_some() => (
                "fail",
                "現在の証明書は有効ですが、自動更新に失敗しています。",
                Some("Hubの証明書欄で失敗理由と再試行結果を確認してください。"),
            ),
            Some((_, renew)) if renew <= now => (
                "skipped",
                "現在の証明書は有効です。自動更新の時期に入っています。",
                Some("Hubで自動更新の結果を確認してください。"),
            ),
            Some(_) => ("pass", "証明書は有効で、自動更新の失敗はありません。", None),
        };
        let expiry = valid
            .and_then(|(expiry, _)| i64::try_from(expiry).ok())
            .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
            .map(|date| format!(" 有効期限: {}", date.to_rfc3339()))
            .unwrap_or_default();
        self.stage(key, label, status, format!("{detail}{expiry}"), hint);
    }
}

impl DeviceClient {
    pub(crate) async fn inspect_peer(
        &self,
        device: &str,
        profile: &str,
        parent: Option<(&str, &str)>,
    ) -> Result<super::client::DeviceGrant, DeviceError> {
        self.request(
            "/v1/network/inspect",
            Some(&json!({"audience_device_id": device,"profile_id": profile,
                "parent_grant_id":parent.map(|(grant, _)| grant),
                "parent_job_id":parent.map(|(_, job)| job)})),
        )
        .await
    }
}

impl DeviceNetworkService {
    pub async fn diagnose(
        &self,
        scope: DiagnosticScope,
        device_id: Option<String>,
        profile_id: Option<String>,
        revision: &str,
        generation: &str,
    ) -> Result<DeviceDiagnostic, DeviceError> {
        if (scope == DiagnosticScope::Peer) != (device_id.is_some() && profile_id.is_some())
            || scope != DiagnosticScope::Peer && (device_id.is_some() || profile_id.is_some())
        {
            return Err(DeviceError::InvalidConfiguration);
        }
        let (settings, shared, client, endpoint, status) = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| DeviceError::Unavailable)?;
            state.check(revision, generation)?;
            let client = match &state.client {
                Some(client) => client.clone(),
                None => DeviceClient::new_from(
                    &state.shared,
                    state
                        .identity
                        .as_ref()
                        .zip(state.settings.certificate_pem.as_deref()),
                    state.settings.device_id.clone().unwrap_or_default(),
                    state.settings.receiver.bind_ip,
                )?,
            };
            (
                state.settings.clone(),
                state.shared.clone(),
                client,
                state.receiver_endpoint.clone(),
                state.receiver_status,
            )
        };
        let mut result = DeviceDiagnostic {
            scope,
            device_id,
            profile_id,
            revision: revision.into(),
            generation: generation.into(),
            checked_at: crate::runtime::SystemClock::now_ms().to_string(),
            stages: vec![],
            local_ipv4: vec![],
        };
        match client.route_ip().await {
            Ok(ip) => {
                result.local_ipv4.push(ip.to_string());
                result.stage("route", "ローカルIPv4", "pass", ip.to_string(), None);
                if scope == DiagnosticScope::Receiver {
                    let covered = settings.certificate_pem.as_deref().is_some_and(|cert| {
                        super::identity::certificate_covers_ip(cert, ip).unwrap_or(false)
                    });
                    result.stage(
                        "certificate",
                        "待受IPv4と証明書",
                        if covered { "pass" } else { "fail" },
                        if covered {
                            "現在のIPv4が端末証明書に含まれています。"
                        } else {
                            "現在のIPv4に対応する端末証明書がありません。"
                        },
                        (!covered)
                            .then_some("Hubへ再接続し、証明書の自動更新結果を確認してください。"),
                    );
                }
            }
            Err(_) => result.stage(
                "route",
                "ローカルIPv4",
                "fail",
                "HubへのIPv4経路を確認できません。",
                Some("ネットワーク接続と詳細の固定IPv4を確認してください。"),
            ),
        }
        if scope == DiagnosticScope::Receiver {
            if let Some(endpoint) = endpoint {
                let reachable = tcp(&endpoint).await;
                result.stage(
                    "listener",
                    "この端末の待受",
                    if reachable && status == "receiving" {
                        "pass"
                    } else {
                        "fail"
                    },
                    format!("{endpoint} / {status}"),
                    (!reachable).then_some("公開ONと待受ポートの使用状況を確認してください。"),
                );
            } else {
                result.stage(
                    "listener",
                    "この端末の待受",
                    "skipped",
                    "待受は開始されていません。",
                    Some("公開ONにすると待受を確認できます。"),
                );
            }
            result.stage("firewall", "別端末からの到達", "skipped", "この端末内の確認だけでは、別端末からの到達は確定できません。", Some("Windows セキュリティ → ファイアウォールとネットワーク保護で、moyAI Desktopの受信を使用中の社内ネットワークに許可してください。Hubの接続許可も確認し、依頼元でこの端末への診断を実行してください。"));
        } else {
            let reachable = tcp(&shared.hub_url).await;
            result.stage("hub_tcp", "HubへのTCP接続", if reachable {"pass"} else {"fail"}, shared.hub_url.clone(), (!reachable).then_some("Hubが起動していることと、Hub側の待受ポート・ファイアウォールを確認してください。"));
            if reachable {
                let probe: Result<HubDiagnostic, _> =
                    client.request("/v1/network/diagnostics", None).await;
                match probe {
                    Ok(probe) if Some(&probe.hub_id) == settings.hub_id.as_ref() => {
                        result.stage("hub_identity", "HubのTLS・端末認可", "pass", "登録したHubと端末資格を確認しました。", None);
                        let now = crate::runtime::SystemClock::now_ms() as u64;
                        result.server_certificate("hub_certificate", "Hub証明書の自動更新", probe.certificate.as_ref(), now);
                        if scope == DiagnosticScope::Hub {
                            result.server_certificate("gateway_certificate", "Gateway証明書の自動更新", probe.gateway_certificate.as_ref(), now);
                            if let Some(endpoint) = probe.gateway_endpoint.filter(|_| probe.gateway_ready) {
                                let http = client.http().acquire().await;
                                let tls_endpoint = reqwest::Url::parse(&endpoint).is_ok_and(|url| url.scheme() == "https" && url.username().is_empty() && url.password().is_none());
                                let ok = tls_endpoint && tokio::time::timeout(Duration::from_secs(5), http.http.get(&endpoint).send()).await.is_ok_and(|response| response.is_ok_and(|response| !response.status().is_redirection() && !response.status().is_server_error()));
                                result.stage("gateway", "モデルGatewayへのTLS接続", if ok {"pass"} else {"fail"}, endpoint, (!ok).then_some("HubでGatewayの起動状態と待受ポートを確認してください。"));
                            } else {
                                result.stage("gateway", "モデルGatewayへのTLS接続", "skipped", "HubのGatewayは待受していません。", Some("Hubでモデル配信を開始してください。"));
                            }
                        } else {
                            self.diagnose_peer(&client, &settings.hub_id, &mut result).await;
                        }
                    },
                    Ok(_) => result.stage("hub_identity", "HubのTLS・端末認可", "fail", "Hubの識別が登録情報と一致しません。", Some("共通設定と参加先Hubを確認してください。")),
                    Err(error) => result.stage("hub_identity", "HubのTLS・端末認可", "fail", error.to_string(), Some("共通config.tomlのHub証明書、端末の参加状態・失効状態を確認してください。")),
                }
            }
        }
        self.inner
            .state
            .lock()
            .map_err(|_| DeviceError::Unavailable)?
            .check(revision, generation)?;
        Ok(result)
    }

    async fn diagnose_peer(
        &self,
        client: &DeviceClient,
        hub_id: &Option<String>,
        result: &mut DeviceDiagnostic,
    ) {
        let device = result.device_id.as_deref().unwrap_or_default();
        let profile = result.profile_id.as_deref().unwrap_or_default();
        // Hold the current identity across grant issuance and the authenticated MCP exchange.
        let transport = client.http();
        let _identity = transport.acquire().await;
        let client = client.detached_http(_identity.http.clone());
        let grant = match client.inspect_peer(device, profile, None).await {
            Ok(grant)
                if Some(&grant.claims.hub_id) == hub_id.as_ref()
                    && grant.claims.actor_device_id == client.device_id
                    && grant.claims.audience_device_id == device
                    && grant.claims.profile_id == profile
                    && grant.peer.device_id == device
                    && grant.peer.profile_id == profile
                    && grant.peer.scope_id == grant.claims.scope_id
                    && grant.expires_at_ms > crate::runtime::SystemClock::now_ms() as u64 =>
            {
                grant
            }
            Ok(_) => {
                result.stage(
                    "peer_policy",
                    "端末間の接続許可",
                    "fail",
                    "診断応答の対象が一致しません。",
                    None,
                );
                return;
            }
            Err(error) => {
                result.stage(
                    "peer_policy",
                    "端末間の接続許可",
                    "fail",
                    error.to_string(),
                    Some("Hubの接続許可と、受入端末のオンライン・公開ONを確認してください。"),
                );
                return;
            }
        };
        result.stage(
            "peer_policy",
            "端末間の接続許可",
            "pass",
            "対象端末の診断が許可されています。",
            None,
        );
        let reachable = tcp(&grant.peer.endpoint).await;
        result.stage(
            "peer_tcp",
            "受入端末へのTCP接続",
            if reachable { "pass" } else { "fail" },
            grant.peer.endpoint.clone(),
            (!reachable).then_some(
                "受入端末でmoyAI Desktopの受信許可と表示された待受ポートを確認してください。",
            ),
        );
        if !reachable {
            return;
        }
        let tls = match self.peer_http(&grant.peer) {
            Ok(http) => tokio::time::timeout(
                Duration::from_secs(5),
                http.get(&grant.peer.endpoint).send(),
            )
            .await
            .is_ok_and(|response| {
                response.is_ok_and(|response| !response.status().is_redirection())
            }),
            Err(_) => false,
        };
        result.stage(
            "peer_tls",
            "受入端末のTLS・端末認証",
            if tls { "pass" } else { "fail" },
            if tls {
                "Hubが公開した端末証明書と一致しています。"
            } else {
                "受入端末の証明書認証を完了できません。"
            },
            (!tls).then_some("受入端末をHubへ再接続し、IP変更時の証明書更新を確認してください。"),
        );
        if !tls {
            return;
        }
        let mcp = tokio::time::timeout(
            Duration::from_secs(12),
            self.peer_operation(&grant, None, json!({}), || Ok(())),
        )
        .await
        .is_ok_and(|response| response.is_ok());
        result.stage(
            "mcp",
            "MCPの初期化・ツール一覧",
            if mcp { "pass" } else { "fail" },
            if mcp {
                "MCPが応答しました。作業は実行していません。"
            } else {
                "MCPの初期化またはツール一覧の取得に失敗しました。"
            },
            (!mcp).then_some("受入端末とHubの状態を再確認してください。"),
        );
    }
}

async fn tcp(endpoint: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(endpoint) else {
        return false;
    };
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
        return false;
    };
    tokio::time::timeout(
        Duration::from_secs(4),
        tokio::net::TcpStream::connect((host, port)),
    )
    .await
    .is_ok_and(|result| result.is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn current_tls_success_does_not_hide_failed_or_due_certificate_renewal() {
        let mut result = DeviceDiagnostic {
            scope: DiagnosticScope::Hub,
            device_id: None,
            profile_id: None,
            revision: "1".into(),
            generation: "1".into(),
            checked_at: "100".into(),
            stages: vec![],
            local_ipv4: vec![],
        };
        for (expiry, renew, error, status) in [
            ("300", "200", None, "pass"),
            ("300", "90", None, "skipped"),
            ("300", "90", Some("opaque-internal-error"), "fail"),
            ("99", "90", None, "fail"),
            ("invalid", "90", None, "fail"),
        ] {
            result.server_certificate(
                "certificate",
                "Certificate",
                Some(&ServerCertificateDiagnostic {
                    expires_at_ms: expiry.into(),
                    renew_after_ms: renew.into(),
                    last_error: error.map(str::to_owned),
                }),
                100,
            );
            let stage = result.stages.last().unwrap();
            assert_eq!(stage.status, status);
            assert!(!stage.detail.contains("opaque-internal-error"));
        }
    }
}

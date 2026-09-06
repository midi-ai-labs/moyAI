//! Local Desktop editing of explicitly configured remote-agent MCP connections.
//! The Tauri owner supplies config CAS and persistence; these helpers never save.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_rustls::rustls::pki_types::CertificateDer;
use tokio_rustls::rustls::pki_types::pem::PemObject;

use crate::config::{McpServerConfig, McpToolRouteConfig, McpTransportKind, ResolvedConfig};
use crate::mcp::{McpClient, McpOperationResult};
use crate::tool::ToolEffectClass;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpPeerDraft {
    pub id: String,
    pub base_url: String,
    pub token: String,
    pub trusted_certificate_pem: Option<String>,
    pub remote_agent: bool,
}

#[derive(Serialize)]
pub(crate) struct McpPeerProjection {
    pub rows: Vec<McpPeerRow>,
}

#[derive(Serialize)]
pub(crate) struct McpPeerRow {
    pub id: String,
    pub base_url: String,
    pub enabled: bool,
    pub credential_configured: bool,
    pub certificate_sha256: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct McpPeerCheck {
    pub id: String,
    pub tools: Vec<McpPeerTool>,
}

#[derive(Serialize)]
pub(crate) struct McpPeerTool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

fn valid_token(token: &str) -> bool {
    (32..=256).contains(&token.len())
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte))
}

pub(crate) fn add(config: &ResolvedConfig, peer: McpPeerDraft) -> Result<ResolvedConfig, String> {
    let id = peer.id.trim().to_string();
    if !peer.remote_agent {
        return Err("この画面ではエージェントの接続先を登録してください。".into());
    }
    if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
        return Err("接続先の名前を128バイト以内で入力してください。".into());
    }
    if config.mcp.servers.len() >= 32 {
        return Err("MCP接続先は32件まで登録できます。".into());
    }
    if config.mcp.servers.iter().any(|server| server.id == id) {
        return Err("同じ名前のMCP接続先が既にあります。別の名前を使ってください。".into());
    }
    if !valid_token(&peer.token) {
        return Err("接続先で発行したトークンを入力してください。".into());
    }
    let server = McpServerConfig {
        display_name: None,
        id,
        enabled: true,
        transport: McpTransportKind::Http,
        base_url: peer.base_url,
        timeout_ms: 30_000,
        tool_routes: [
            ("delegate_task", ToolEffectClass::Mutation),
            ("task_status", ToolEffectClass::Read),
            ("cancel_task", ToolEffectClass::Mutation),
        ]
        .into_iter()
        .map(|(name, effect)| McpToolRouteConfig {
            name: name.into(),
            effect,
        })
        .collect(),
        headers: BTreeMap::from([("Authorization".into(), format!("Bearer {}", peer.token))]),
        remote_agent: true,
        trusted_certificate_pem: peer.trusted_certificate_pem,
    };
    let mut validated = config.clone();
    validated.mcp.enabled = true;
    validated.mcp.servers.push(server);
    validated
        .normalize_and_validate_mcp_runtime()
        .map_err(|_| {
            "接続先のURLと証明書を確認してください。別端末への接続にはHTTPSが必要です。".to_string()
        })?;
    // Validate against all configured IDs without rewriting an unrelated server's
    // headers, opt-ins, endpoint spelling, or credential reference.
    let server = validated.mcp.servers.pop().expect("validated added server");
    let mut next = config.clone();
    next.mcp.enabled = true;
    next.mcp.servers.push(server);
    Ok(next)
}

pub(crate) fn remove(config: &ResolvedConfig, id: &str) -> Result<ResolvedConfig, String> {
    if !config
        .mcp
        .servers
        .iter()
        .any(|server| server.id == id && server.remote_agent)
    {
        return Err("削除するエージェント接続先が見つかりません。".into());
    }
    let mut next = config.clone();
    next.mcp
        .servers
        .retain(|server| !(server.id == id && server.remote_agent));
    Ok(next)
}

pub(crate) fn projection(config: &ResolvedConfig) -> McpPeerProjection {
    McpPeerProjection {
        rows: config
            .mcp
            .servers
            .iter()
            .filter(|server| server.remote_agent)
            .map(|server| {
                let mut authorization = server
                    .headers
                    .iter()
                    .filter(|(name, _)| name.eq_ignore_ascii_case("authorization"));
                let token = authorization
                    .next()
                    .and_then(|(_, value)| value.strip_prefix("Bearer "));
                McpPeerRow {
                    id: server.id.clone(),
                    base_url: crate::config::model::canonical_mcp_base_url(&server.base_url)
                        .unwrap_or_else(|_| "接続先URLを確認してください".into()),
                    enabled: config.mcp.enabled && server.enabled,
                    credential_configured: token.is_some_and(valid_token)
                        && authorization.next().is_none(),
                    certificate_sha256: server
                        .trusted_certificate_pem
                        .as_deref()
                        .filter(|pem| pem.len() <= 65_536)
                        .and_then(|pem| CertificateDer::from_pem_slice(pem.as_bytes()).ok())
                        .map(|certificate| format!("{:x}", Sha256::digest(certificate.as_ref()))),
                }
            })
            .collect(),
    }
}

pub(crate) async fn check(config: &ResolvedConfig, id: &str) -> Result<McpPeerCheck, String> {
    let server = config
        .mcp
        .servers
        .iter()
        .find(|server| server.id == id && server.remote_agent)
        .ok_or_else(|| "確認するエージェント接続先が見つかりません。".to_string())?;
    if !config.mcp.enabled || !server.enabled {
        return Err("この接続先は無効です。有効にしてから確認してください。".into());
    }
    let mut selected = config.clone();
    selected.mcp.servers = vec![server.clone()];
    selected
        .normalize_and_validate_mcp_runtime()
        .map_err(|_| "保存された接続先と証明書を確認してください。".to_string())?;
    let result = McpClient::new(selected.mcp)
        .list_tools(id, || Ok(()))
        .await
        .map_err(|_| {
            "接続を確認できません。接続先の起動状態、URL、証明書、トークンを確認してください。"
                .to_string()
        })?;
    match result {
        McpOperationResult::ToolsListed { tools, .. } => Ok(McpPeerCheck {
            id: id.to_string(),
            tools: tools
                .into_iter()
                .take(32)
                .map(|tool| McpPeerTool {
                    name: tool.name.chars().take(128).collect(),
                    description: tool
                        .description
                        .map(|text| text.chars().take(512).collect()),
                })
                .collect(),
        }),
        McpOperationResult::ToolCalled { .. } => Err("接続先の機能一覧を確認できません。".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn peer(id: &str) -> McpPeerDraft {
        McpPeerDraft {
            id: id.into(),
            base_url: "https://192.168.1.20:7332/mcp/".into(),
            token: "moyai_0123456789abcdef0123456789abcdef".into(),
            trusted_certificate_pem: None,
            remote_agent: true,
        }
    }

    #[test]
    fn add_and_remove_preserve_unrelated_mcp_configuration_and_secrets() {
        let mut config = ResolvedConfig::default();
        config.mcp.servers[0]
            .headers
            .insert("Authorization".into(), "Bearer existing-secret".into());
        let existing = serde_json::to_value(&config.mcp.servers).unwrap();
        let next = add(&config, peer("WinB")).unwrap();
        assert!(next.mcp.enabled);
        assert_eq!(
            serde_json::to_value(&next.mcp.servers[..config.mcp.servers.len()]).unwrap(),
            existing
        );
        assert_eq!(serde_json::to_value(&config.mcp.servers).unwrap(), existing);
        let added = next.mcp.servers.last().unwrap();
        assert_eq!(added.base_url, "https://192.168.1.20:7332/mcp");
        assert_eq!(added.timeout_ms, 30_000);
        assert_eq!(
            added
                .tool_routes
                .iter()
                .map(|route| (route.name.as_str(), route.effect))
                .collect::<Vec<_>>(),
            vec![
                ("delegate_task", ToolEffectClass::Mutation),
                ("task_status", ToolEffectClass::Read),
                ("cancel_task", ToolEffectClass::Mutation)
            ]
        );
        assert!(remove(&next, &config.mcp.servers[0].id).is_err());
        let removed = remove(&next, "WinB").unwrap();
        assert_eq!(serde_json::to_value(removed.mcp.servers).unwrap(), existing);
    }

    #[test]
    fn add_rejects_duplicate_ids_and_plaintext_remote_connections() {
        let config = ResolvedConfig::default();
        assert!(add(&config, peer(&config.mcp.servers[0].id)).is_err());
        let next = add(&config, peer("WinB")).unwrap();
        assert!(add(&next, peer(" WinB ")).is_err());
        for url in [
            "http://192.168.1.20:7332/mcp",
            "https://user:secret@example.test/mcp",
            "https://example.test/mcp?token=secret",
        ] {
            let mut invalid = peer("WinC");
            invalid.base_url = url.into();
            let error = add(&config, invalid).err().unwrap();
            assert!(!error.contains("secret"));
        }
        let mut local = peer("This PC");
        local.base_url = "http://127.0.0.1:7332/mcp".into();
        assert!(add(&config, local).is_ok());
        let mut invalid_certificate = peer("WinC");
        invalid_certificate.trusted_certificate_pem = Some("invalid certificate".into());
        assert!(add(&config, invalid_certificate).is_err());
    }

    #[test]
    fn projection_only_exposes_worker_metadata_and_redacts_invalid_credential_urls() {
        let config = ResolvedConfig::default();
        let mut next = add(&config, peer("WinB")).unwrap();
        let certificate = rcgen::generate_simple_self_signed(vec!["192.168.1.20".into()]).unwrap();
        let server = next.mcp.servers.last_mut().unwrap();
        server.trusted_certificate_pem = Some(certificate.cert.pem());
        server.base_url = "https://user:url-secret@example.test/mcp".into();
        let projected = projection(&next);
        assert_eq!(projected.rows.len(), 1);
        assert_eq!(projected.rows[0].id, "WinB");
        assert!(projected.rows[0].credential_configured);
        assert_eq!(
            projected.rows[0].certificate_sha256,
            Some(format!(
                "{:x}",
                Sha256::digest(certificate.cert.der().as_ref())
            ))
        );
        let serialized = serde_json::to_string(&projected).unwrap();
        for secret in [
            "url-secret",
            "0123456789abcdef",
            "BEGIN CERTIFICATE",
            "Authorization",
        ] {
            assert!(!serialized.contains(secret));
        }
        next.mcp.enabled = false;
        assert!(!projection(&next).rows[0].enabled);
    }

    #[tokio::test]
    async fn check_uses_saved_credentials_and_returns_bounded_tool_descriptions() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let app = axum::Router::new().route("/mcp", axum::routing::post(|headers: axum::http::HeaderMap, axum::Json(request): axum::Json<serde_json::Value>| async move {
            assert_eq!(headers["authorization"], "Bearer moyai_0123456789abcdef0123456789abcdef");
            assert_eq!(request["method"], "tools/list");
            axum::Json(json!({"jsonrpc":"2.0", "id": request["id"], "result":{"tools":[{"name":"delegate_task","description":"説明".repeat(400)},{"name":"task_status"},{"name":"cancel_task"},{"name":"unpublished_extra"}]}}))
        }));
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        let mut draft = peer("WinB");
        draft.base_url = format!("http://{address}/mcp");
        let mut config = add(&ResolvedConfig::default(), draft).unwrap();
        let result = check(&config, "WinB").await.unwrap();
        assert_eq!(result.id, "WinB");
        assert_eq!(result.tools.len(), 3);
        assert_eq!(
            result.tools[0]
                .description
                .as_ref()
                .unwrap()
                .chars()
                .count(),
            512
        );
        assert!(check(&config, "missing").await.is_err());
        config.mcp.enabled = false;
        assert!(check(&config, "WinB").await.is_err());
        stop.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }
}

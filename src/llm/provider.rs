use std::fmt;

use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::LlmError;

pub fn resolve_api_key_from_env(env_name: Option<&str>) -> Result<Option<String>, LlmError> {
    let Some(env_name) = crate::config::canonical_api_key_env_name(env_name).map_err(|_| {
        LlmError::Message("configured API-key environment variable name is invalid".to_string())
    })?
    else {
        return Ok(None);
    };
    let value = std::env::var_os(&env_name).ok_or_else(|| {
        LlmError::Message(format!(
            "configured API-key environment variable `{env_name}` is not set"
        ))
    })?;
    let value = value.into_string().map_err(|_| {
        LlmError::Message(format!(
            "configured API-key environment variable `{env_name}` is not valid Unicode"
        ))
    })?;
    let value = value.trim();
    if value.is_empty() {
        return Err(LlmError::Message(format!(
            "configured API-key environment variable `{env_name}` is empty"
        )));
    }
    Ok(Some(value.to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderRequestId(String);

impl ProviderRequestId {
    pub fn new() -> Self {
        Self(Ulid::new().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ProviderRequestId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ProviderRequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderPhase {
    #[serde(rename = "attempt_started")]
    AttemptStarted,
    #[serde(rename = "request_in_flight")]
    RequestInFlight,
    #[serde(rename = "headers_received")]
    HeadersReceived,
    #[serde(rename = "first_progress")]
    FirstProgress,
    #[serde(rename = "last_progress")]
    LastProgress,
    #[serde(rename = "provider_terminal")]
    ProviderTerminal,
}

impl ProviderPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AttemptStarted => "attempt_started",
            Self::RequestInFlight => "request_in_flight",
            Self::HeadersReceived => "headers_received",
            Self::FirstProgress => "first_progress",
            Self::LastProgress => "last_progress",
            Self::ProviderTerminal => "provider_terminal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderTerminalStatus {
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFailureKind {
    Connect,
    RequestTimeout,
    // Kept for decoding provider traces written before the unified request deadline.
    ResponseStartTimeout,
    StreamIdleTimeout,
    HttpStatus,
    Generation,
    Protocol,
    Decode,
    Cancelled,
    EventProjection,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderFailure {
    pub request_id: ProviderRequestId,
    pub endpoint: String,
    pub phase: ProviderPhase,
    pub attempt: u16,
    pub elapsed_ms: u64,
    pub kind: ProviderFailureKind,
    pub status: Option<u16>,
    pub code: Option<String>,
    pub message: String,
}

impl ProviderFailure {
    /// Returns the stable user-visible failure text.
    ///
    /// Provider request identifiers, endpoints, response bodies, and provider supplied error
    /// strings are runtime diagnostics. They must not be copied into durable terminals, canonical
    /// history, or exports.
    pub fn public_message(&self) -> String {
        match self.kind {
            ProviderFailureKind::Connect => {
                "AIの接続先に接続できませんでした。AIを提供するアプリが起動しているか、接続先URLとポート番号が正しいか、実行するPCから接続できるかを確認してください。ホスト名を使っている場合は、その名前が現在のIPアドレスを指しているか接続先の担当者に確認してください。"
                    .to_string()
            }
            ProviderFailureKind::RequestTimeout | ProviderFailureKind::ResponseStartTimeout => {
                "AIからの応答開始を待ちましたが、待ち時間の上限に達しました。接続先の動作状況と、設定の「応答の進捗を待つ時間」を確認してください。"
                    .to_string()
            }
            ProviderFailureKind::StreamIdleTimeout => {
                "AIからの応答が途中で途切れました。接続先の動作状況と通信状態を確認してから、もう一度依頼してください。"
                    .to_string()
            }
            ProviderFailureKind::HttpStatus => match self.status {
                Some(401 | 403) => format!(
                    "AIの接続先で認証または利用権限の確認に失敗しました（HTTP {}）。APIキーなどの認証設定と、その接続先を利用できる権限を確認してください。",
                    self.status.expect("matched status")
                ),
                Some(404) => {
                    "AIの接続先が見つかりませんでした（HTTP 404）。設定の接続方式と接続先URLを確認してください。"
                        .to_string()
                }
                Some(429) => {
                    "AIの接続先で利用回数などの上限に達しています（HTTP 429）。少し待ってからもう一度依頼してください。続く場合は、接続先の利用上限を確認してください。"
                        .to_string()
                }
                Some(status) => format!(
                    "AIの接続先が依頼を受け付けませんでした（HTTP {status}）。接続先の動作状況と、設定したモデル・接続方式を確認してください。"
                ),
                None => {
                    "AIの接続先が依頼を受け付けませんでした。接続先の動作状況と、設定したモデル・接続方式を確認してください。"
                        .to_string()
                }
            },
            ProviderFailureKind::Generation
                if self.code.as_deref() == Some("context_length_exceeded") =>
            {
                "会話や添付資料の量が、このモデルで一度に扱える上限を超えています。依頼に含める内容を減らすか、より多くの内容を扱えるモデルを選んでください。"
                    .to_string()
            }
            ProviderFailureKind::Generation => {
                "AIが応答を作成できませんでした。接続先でモデルが正常に動いているか確認してから、もう一度依頼してください。"
                    .to_string()
            }
            ProviderFailureKind::Protocol | ProviderFailureKind::Decode => {
                "AIからの応答を読み取れませんでした。設定の接続方式が、そのAIの接続方法に合っているか確認してください。"
                    .to_string()
            }
            ProviderFailureKind::Cancelled => "AIへの依頼を中止しました。".to_string(),
            ProviderFailureKind::EventProjection => {
                "AIからの応答を現在の会話に反映できませんでした。会話を開き直し、残っている内容を確認してから、もう一度依頼してください。"
                    .to_string()
            }
            ProviderFailureKind::Other => {
                "AIへの依頼に失敗しました。接続先の動作状況を確認してから、もう一度依頼してください。".to_string()
            }
        }
    }
}

impl fmt::Display for ProviderFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "provider request {} at {} failed during {:?} after {}ms ({:?})",
            self.request_id, self.endpoint, self.phase, self.elapsed_ms, self.kind
        )?;
        if let Some(status) = self.status {
            write!(formatter, " status={status}")?;
        }
        if let Some(code) = &self.code {
            write!(formatter, " code={code}")?;
        }
        write!(formatter, ": {}", self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ProviderFailure, ProviderFailureKind, ProviderPhase, ProviderRequestId,
        resolve_api_key_from_env,
    };

    #[test]
    fn public_provider_failure_omits_private_transport_and_payload_details() {
        let secret = "provider-payload-secret";
        let request_id = ProviderRequestId::new();
        let failure = ProviderFailure {
            request_id: request_id.clone(),
            endpoint: "https://provider.example/private-route".to_string(),
            phase: ProviderPhase::ProviderTerminal,
            attempt: 2,
            elapsed_ms: 125,
            kind: ProviderFailureKind::HttpStatus,
            status: Some(401),
            code: Some("credential-secret-code".to_string()),
            message: secret.to_string(),
        };

        let public = failure.public_message();
        assert!(public.contains("HTTP 401"));
        assert!(!public.contains(request_id.as_str()));
        assert!(!public.contains("provider.example"));
        assert!(!public.contains("credential-secret-code"));
        assert!(!public.contains(secret));
    }

    #[test]
    fn public_provider_failure_explains_the_failure_and_next_action_in_japanese() {
        let cases = [
            (ProviderFailureKind::Connect, None, None, "接続先URL"),
            (
                ProviderFailureKind::RequestTimeout,
                None,
                None,
                "応答の進捗を待つ時間",
            ),
            (
                ProviderFailureKind::ResponseStartTimeout,
                None,
                None,
                "応答の進捗を待つ時間",
            ),
            (
                ProviderFailureKind::StreamIdleTimeout,
                None,
                None,
                "通信状態",
            ),
            (ProviderFailureKind::HttpStatus, Some(401), None, "認証設定"),
            (ProviderFailureKind::HttpStatus, Some(403), None, "利用権限"),
            (
                ProviderFailureKind::HttpStatus,
                Some(404),
                None,
                "接続先URL",
            ),
            (ProviderFailureKind::HttpStatus, Some(429), None, "利用上限"),
            (ProviderFailureKind::HttpStatus, Some(500), None, "動作状況"),
            (ProviderFailureKind::HttpStatus, None, None, "接続方式"),
            (
                ProviderFailureKind::Generation,
                None,
                Some("context_length_exceeded"),
                "内容を減らす",
            ),
            (ProviderFailureKind::Generation, None, None, "モデルが正常"),
            (ProviderFailureKind::Protocol, None, None, "接続方式"),
            (ProviderFailureKind::Decode, None, None, "接続方式"),
            (ProviderFailureKind::Cancelled, None, None, "中止しました"),
            (
                ProviderFailureKind::EventProjection,
                None,
                None,
                "会話を開き直し",
            ),
            (ProviderFailureKind::Other, None, None, "動作状況"),
        ];
        for (kind, status, code, next_action) in cases {
            let failure = ProviderFailure {
                request_id: ProviderRequestId::new(),
                endpoint: "https://private-provider.example/private-route".to_string(),
                phase: ProviderPhase::ProviderTerminal,
                attempt: 2,
                elapsed_ms: 125,
                kind,
                status,
                code: Some(code.unwrap_or("private-error-code").to_string()),
                message: "private-provider-payload".to_string(),
            };
            let public = failure.public_message();
            assert!(public.contains(next_action), "{kind:?}: {public}");
            if let Some(status) = status {
                assert!(public.contains(&format!("HTTP {status}")));
            }
            assert!(!public.contains(failure.request_id.as_str()));
            assert!(!public.contains("private-"));
            assert!(!public.contains("context_length_exceeded"));
        }
    }

    #[test]
    fn configured_api_key_environment_fails_closed() {
        assert_eq!(resolve_api_key_from_env(None).expect("optional key"), None);
        assert!(resolve_api_key_from_env(Some(" ")).is_err());
        assert!(resolve_api_key_from_env(Some("INVALID-NAME")).is_err());
        let missing = format!("MOYAI_MISSING_API_KEY_{}", ulid::Ulid::new());
        let error = resolve_api_key_from_env(Some(&missing))
            .expect_err("configured missing key must fail closed");
        assert!(error.to_string().contains("is not set"));
    }

    #[test]
    fn provider_phase_accepts_only_the_current_wire_names() {
        let current = [
            (ProviderPhase::AttemptStarted, "attempt_started"),
            (ProviderPhase::RequestInFlight, "request_in_flight"),
            (ProviderPhase::HeadersReceived, "headers_received"),
            (ProviderPhase::FirstProgress, "first_progress"),
            (ProviderPhase::LastProgress, "last_progress"),
            (ProviderPhase::ProviderTerminal, "provider_terminal"),
        ];
        for (phase, name) in current {
            let encoded = serde_json::to_string(&phase).expect("serialize provider phase");
            assert_eq!(encoded, format!("\"{name}\""));
            assert_eq!(
                serde_json::from_str::<ProviderPhase>(&encoded)
                    .expect("deserialize current provider phase"),
                phase
            );
        }

        for retired in ["connect", "awaiting_headers", "stream_started"] {
            let encoded = format!("\"{retired}\"");
            assert!(
                serde_json::from_str::<ProviderPhase>(&encoded).is_err(),
                "retired provider phase `{retired}` must be rejected"
            );
        }
    }

    #[test]
    fn provider_generation_failure_kind_has_a_stable_wire_name() {
        let encoded = serde_json::to_string(&ProviderFailureKind::Generation)
            .expect("serialize generation failure kind");
        assert_eq!(encoded, "\"generation\"");
        assert_eq!(
            serde_json::from_str::<ProviderFailureKind>(&encoded)
                .expect("deserialize generation failure kind"),
            ProviderFailureKind::Generation
        );
    }

    #[test]
    fn unified_request_timeout_failure_kind_has_a_stable_wire_name() {
        let encoded = serde_json::to_string(&ProviderFailureKind::RequestTimeout)
            .expect("serialize request timeout failure kind");
        assert_eq!(encoded, "\"request_timeout\"");
        assert_eq!(
            serde_json::from_str::<ProviderFailureKind>(&encoded)
                .expect("deserialize request timeout failure kind"),
            ProviderFailureKind::RequestTimeout
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderPhaseEvent {
    pub request_id: ProviderRequestId,
    pub endpoint: String,
    pub phase: ProviderPhase,
    pub attempt: u16,
    pub elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_status: Option<ProviderTerminalStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<crate::session::TokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ProviderFailure>,
}

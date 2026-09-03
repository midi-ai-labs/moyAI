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
                "Could not connect to the model provider. Check that the provider is running and the connection settings are correct."
                    .to_string()
            }
            ProviderFailureKind::RequestTimeout | ProviderFailureKind::ResponseStartTimeout => {
                "The model provider did not start a response before the request deadline. Check the provider load or increase the response timeout."
                    .to_string()
            }
            ProviderFailureKind::StreamIdleTimeout => {
                "The model provider stopped sending response data. Check the provider load and try again."
                    .to_string()
            }
            ProviderFailureKind::HttpStatus => match self.status {
                Some(401 | 403) => format!(
                    "The model provider rejected authentication or authorization (HTTP {}). Check the configured credential.",
                    self.status.expect("matched status")
                ),
                Some(404) => {
                    "The configured model provider route was not found (HTTP 404). Check the connection type and model endpoint."
                        .to_string()
                }
                Some(429) => {
                    "The model provider is rate-limiting requests (HTTP 429). Wait briefly and try again."
                        .to_string()
                }
                Some(status) => format!(
                    "The model provider rejected the request (HTTP {status}). Check the provider and model settings."
                ),
                None => {
                    "The model provider rejected the request. Check the provider and model settings."
                        .to_string()
                }
            },
            ProviderFailureKind::Generation
                if self.code.as_deref() == Some("context_length_exceeded") =>
            {
                "The request exceeds the model context limit. Reduce the conversation context or select a model with a larger context window."
                    .to_string()
            }
            ProviderFailureKind::Generation => {
                "The model provider could not complete generation. Check the model state and try again."
                    .to_string()
            }
            ProviderFailureKind::Protocol | ProviderFailureKind::Decode => {
                "The model provider returned an unsupported or malformed response. Check the configured connection type."
                    .to_string()
            }
            ProviderFailureKind::Cancelled => "The model request was cancelled.".to_string(),
            ProviderFailureKind::EventProjection => {
                "The model response could not be applied to the current task. Reopen the task and try again."
                    .to_string()
            }
            ProviderFailureKind::Other => {
                "The model request failed. Check the provider state and try again.".to_string()
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

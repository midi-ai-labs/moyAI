use std::fmt;

use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::LlmError;

pub fn resolve_api_key_from_env(env_name: Option<&str>) -> Result<Option<String>, LlmError> {
    let Some(env_name) = env_name else {
        return Ok(None);
    };
    let env_name = env_name.trim();
    if env_name.is_empty()
        || !env_name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err(LlmError::Message(
            "configured API-key environment variable name is invalid".to_string(),
        ));
    }
    let value = std::env::var_os(env_name).ok_or_else(|| {
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
    ResponseStartTimeout,
    StreamIdleTimeout,
    HttpStatus,
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
    use super::{ProviderPhase, resolve_api_key_from_env};

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
    pub failure: Option<ProviderFailure>,
}

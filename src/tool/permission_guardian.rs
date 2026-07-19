use async_trait::async_trait;
use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tool::PermissionRequest;

/// Tool-specific, model-visible evidence for the exact action that would run after approval.
///
/// The Guardian runtime separately supplies the committed raw tool request. `PermissionRequest`
/// means that no additional derived evidence is needed beyond that raw request and the bounded
/// human projection. Tools whose execution depends on normalized values or configured targets
/// provide a dedicated variant here.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PermissionGuardianEvidence {
    PermissionRequest,
    McpListTools {
        server_id: String,
        configured_target: String,
        credential_present: bool,
    },
    McpCall {
        server_id: String,
        configured_target: String,
        credential_present: bool,
        tool_name: String,
        arguments: Value,
    },
    DoclingConvert {
        endpoint: String,
        source: DoclingSourceEvidence,
        from_formats: Vec<String>,
        to_formats: Vec<String>,
        do_ocr: Option<bool>,
        include_images: bool,
        page_range: Option<[u32; 2]>,
        credential_present: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DoclingSourceEvidence {
    LocalFile { path: Utf8PathBuf },
    SourceUrl { url: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum PermissionGuardianEvidenceState {
    Complete(PermissionGuardianEvidence),
    Incomplete { reason: String },
}

impl PermissionGuardianEvidenceState {
    pub fn permission_request() -> Self {
        Self::Complete(PermissionGuardianEvidence::PermissionRequest)
    }

    pub fn incomplete(reason: impl Into<String>) -> Self {
        Self::Incomplete {
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionGuardianDecision {
    Allow { rationale: String },
    Deny { rationale: String },
}

#[derive(Debug, thiserror::Error)]
pub enum PermissionGuardianError {
    #[error("guardian request failed: {0}")]
    Request(String),
    #[error("guardian returned an invalid decision: {0}")]
    InvalidDecision(String),
}

#[async_trait(?Send)]
pub trait PermissionGuardian {
    async fn review(
        &mut self,
        request: &PermissionRequest,
        evidence: &PermissionGuardianEvidence,
    ) -> Result<PermissionGuardianDecision, PermissionGuardianError>;
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GuardianDecisionWire {
    decision: GuardianDecisionKind,
    rationale: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum GuardianDecisionKind {
    Allow,
    Deny,
}

pub(crate) fn parse_guardian_decision(
    response: &str,
) -> Result<PermissionGuardianDecision, PermissionGuardianError> {
    let wire = serde_json::from_str::<GuardianDecisionWire>(response.trim()).map_err(|error| {
        PermissionGuardianError::InvalidDecision(format!(
            "expected one exact JSON object with decision and rationale: {error}"
        ))
    })?;
    let rationale = wire.rationale.trim().to_string();
    if rationale.is_empty() {
        return Err(PermissionGuardianError::InvalidDecision(
            "rationale must not be empty".to_string(),
        ));
    }
    Ok(match wire.decision {
        GuardianDecisionKind::Allow => PermissionGuardianDecision::Allow { rationale },
        GuardianDecisionKind::Deny => PermissionGuardianDecision::Deny { rationale },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exact_allow_and_deny_decisions() {
        assert_eq!(
            parse_guardian_decision(r#"{"decision":"allow","rationale":"scoped"}"#).expect("allow"),
            PermissionGuardianDecision::Allow {
                rationale: "scoped".to_string(),
            }
        );
        assert_eq!(
            parse_guardian_decision(r#"{"decision":"deny","rationale":"not authorized"}"#)
                .expect("deny"),
            PermissionGuardianDecision::Deny {
                rationale: "not authorized".to_string(),
            }
        );
    }

    #[test]
    fn rejects_wrappers_unknown_fields_and_empty_rationale() {
        for response in [
            r#"```json
{"decision":"allow","rationale":"scoped"}
```"#,
            r#"{"decision":"allow","rationale":"scoped","extra":true}"#,
            r#"{"decision":"allow","rationale":"  "}"#,
            r#"{"decision":"maybe","rationale":"unclear"}"#,
        ] {
            assert!(parse_guardian_decision(response).is_err(), "{response}");
        }
    }
}

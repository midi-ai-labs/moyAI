use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::llm::ModelToolCall;
use crate::tool::PermissionRequest;

pub(crate) const PERMISSION_RETRY_FAMILY_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PermissionRetryEffectKeys {
    pub family_version: u32,
    pub family_sha256: String,
    pub identity_sha256: String,
}

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

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PermissionRetryFamily {
    /// AutoReview grants one elevated-effect capability for a workspace authority. A denial
    /// fences every alternative elevated tool surface until a new canonical UserTurn or delivered
    /// SteerTurn advances authority. This deliberately over-blocks unrelated elevated work: a
    /// narrower command/path classifier would let a filesystem action be restated as shell, MCP,
    /// or a generated wrapper without crossing the deterministic fence.
    ElevatedEffect { workspace_root: String },
}

pub(crate) fn permission_retry_effect_keys(
    request: &PermissionRequest,
    evidence: &PermissionGuardianEvidence,
    committed_tool_request: &ModelToolCall,
    workspace_root: &Utf8Path,
) -> Result<PermissionRetryEffectKeys, PermissionGuardianError> {
    let family = PermissionRetryFamily::ElevatedEffect {
        workspace_root: normalized_permission_path(workspace_root),
    };
    let family_json = serde_json::to_vec(&family)
        .map_err(|error| PermissionGuardianError::Request(error.to_string()))?;
    let arguments = serde_json::from_str::<Value>(&committed_tool_request.arguments_json)
        .map(canonical_json_value)
        .map_err(|error| {
            PermissionGuardianError::Request(format!(
                "committed tool arguments are not valid JSON for retry identity: {error}"
            ))
        })?;
    let identity = serde_json::json!({
        "family_version": PERMISSION_RETRY_FAMILY_VERSION,
        "family": family,
        "tool_name": committed_tool_request.tool_name,
        "arguments": arguments,
        "permission": {
            "access": request.access,
            "outside_workspace": request.outside_workspace,
            "targets": request.targets.iter().map(|target| normalized_permission_path(target)).collect::<Vec<_>>(),
            "risks": request.risks,
        },
        "evidence": evidence,
    });
    let identity_json = serde_json::to_vec(&canonical_json_value(identity))
        .map_err(|error| PermissionGuardianError::Request(error.to_string()))?;
    Ok(PermissionRetryEffectKeys {
        family_version: PERMISSION_RETRY_FAMILY_VERSION,
        family_sha256: sha256_hex(&family_json),
        identity_sha256: sha256_hex(&identity_json),
    })
}

fn normalized_permission_path(path: &Utf8Path) -> String {
    crate::workspace::PathGuard::stable_identity_key(path)
}

fn canonical_json_value(value: Value) -> Value {
    match value {
        Value::Array(values) => {
            Value::Array(values.into_iter().map(canonical_json_value).collect())
        }
        Value::Object(values) => {
            let mut entries = values.into_iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            let mut canonical = serde_json::Map::new();
            for (key, value) in entries {
                canonical.insert(key, canonical_json_value(value));
            }
            Value::Object(canonical)
        }
        scalar => scalar,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
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
    #[error("permission review was cancelled")]
    Cancelled,
    #[error("guardian returned an invalid decision: {0}")]
    InvalidDecision(String),
    #[error("permission review exceeded its total deadline of {milliseconds} milliseconds")]
    TotalDeadline { milliseconds: u64 },
    #[error("permission retry fence blocked the equivalent elevated effect: {0}")]
    RetryFenced(String),
    #[error(
        "automatic permission admission failed before a durable retry fence could be established: {0}"
    )]
    UnfencedAdmission(String),
}

#[async_trait(?Send)]
pub trait PermissionGuardian {
    async fn review(
        &mut self,
        request: &PermissionRequest,
        evidence: &PermissionGuardianEvidence,
    ) -> Result<PermissionGuardianDecision, PermissionGuardianError>;

    fn take_approved_retry_lease(&mut self) -> Option<crate::storage::PermissionReviewLease> {
        None
    }
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
    use crate::workspace::AccessKind;

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

    #[test]
    fn elevated_effect_family_cannot_be_bypassed_by_tool_or_shell_spelling() {
        let workspace = Utf8Path::new("C:/workspace");
        let request = PermissionRequest {
            access: AccessKind::Shell,
            summary: "Run the requested test".to_string(),
            details: Vec::new(),
            targets: vec![workspace.to_path_buf()],
            outside_workspace: true,
            risks: Vec::new(),
            agent_path: None,
            agent_task_name: None,
        };
        let first = permission_retry_effect_keys(
            &request,
            &PermissionGuardianEvidence::PermissionRequest,
            &ModelToolCall {
                call_id: "first".to_string(),
                tool_name: "shell".to_string(),
                arguments_json: r#"{"command":"pytest -q","description":"test"}"#.to_string(),
            },
            workspace,
        )
        .expect("first keys");
        let mut cleanup_request = request.clone();
        cleanup_request.summary = "Remove debug residue".to_string();
        cleanup_request.risks = vec![crate::tool::PermissionRisk::DestructiveDelete];
        let workaround = permission_retry_effect_keys(
            &cleanup_request,
            &PermissionGuardianEvidence::PermissionRequest,
            &ModelToolCall {
                call_id: "second".to_string(),
                tool_name: "shell".to_string(),
                arguments_json: r#"{"description":"cleanup","command":"python cleanup.py"}"#
                    .to_string(),
            },
            workspace,
        )
        .expect("workaround keys");

        assert_eq!(first.family_version, PERMISSION_RETRY_FAMILY_VERSION);
        assert_eq!(first.family_sha256, workaround.family_sha256);
        assert_ne!(first.identity_sha256, workaround.identity_sha256);

        let mut native_request = request.clone();
        native_request.access = AccessKind::Edit;
        native_request.targets = vec![workspace.join(".pytest-tmp")];
        native_request.risks = vec![crate::tool::PermissionRisk::DestructiveDelete];
        let native_delete = permission_retry_effect_keys(
            &native_request,
            &PermissionGuardianEvidence::PermissionRequest,
            &ModelToolCall {
                call_id: "third".to_string(),
                tool_name: "apply_patch".to_string(),
                arguments_json: r#"{"path":".pytest-tmp","operation":"delete"}"#.to_string(),
            },
            workspace,
        )
        .expect("native delete keys");
        assert_eq!(first.family_sha256, native_delete.family_sha256);
        assert_ne!(first.identity_sha256, native_delete.identity_sha256);
    }

    #[test]
    fn exact_identity_canonicalizes_argument_object_order() {
        let workspace = Utf8Path::new("C:/workspace");
        let request = PermissionRequest {
            access: AccessKind::Shell,
            summary: "run".to_string(),
            details: Vec::new(),
            targets: vec![workspace.to_path_buf()],
            outside_workspace: true,
            risks: Vec::new(),
            agent_path: None,
            agent_task_name: None,
        };
        let keys = |call_id: &str, arguments_json: &str| {
            permission_retry_effect_keys(
                &request,
                &PermissionGuardianEvidence::PermissionRequest,
                &ModelToolCall {
                    call_id: call_id.to_string(),
                    tool_name: "shell".to_string(),
                    arguments_json: arguments_json.to_string(),
                },
                workspace,
            )
            .expect("keys")
        };

        assert_eq!(
            keys("a", r#"{"command":"pytest","timeout_ms":10}"#).identity_sha256,
            keys("b", r#"{"timeout_ms":10,"command":"pytest"}"#).identity_sha256,
        );
    }
}

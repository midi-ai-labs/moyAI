use async_trait::async_trait;
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config::ShellFamily;
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
/// The Guardian runtime supplies action arguments and execution facts separately. `PermissionRequest`
/// means that no additional derived evidence is needed. Tools whose execution depends on normalized
/// values or configured targets provide a dedicated variant here.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PermissionGuardianEvidence {
    PermissionRequest,
    ShellExecution {
        shell_family: ShellFamily,
        cwd: Utf8PathBuf,
        executable_candidates: Vec<Utf8PathBuf>,
        arguments: Vec<String>,
    },
    FileEditWithFormatters {
        formatters: Vec<FormatterExecutionEvidence>,
    },
    SharedServiceStop {
        service_id: String,
        environment_id: String,
        attempt_id: String,
        generation: u64,
    },
    HubSubmission {
        endpoint: String,
        hub_binding: String,
        payload: Value,
    },
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FormatterExecutionEvidence {
    pub target: Utf8PathBuf,
    pub cwd: Utf8PathBuf,
    pub executable: Utf8PathBuf,
    pub argv: Vec<String>,
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

    pub fn file_edit_with_formatters(
        formatters: impl IntoIterator<Item = FormatterExecutionEvidence>,
    ) -> Self {
        let formatters = formatters.into_iter().collect::<Vec<_>>();
        if formatters.is_empty() {
            Self::permission_request()
        } else {
            Self::Complete(PermissionGuardianEvidence::FileEditWithFormatters { formatters })
        }
    }

    pub fn incomplete(reason: impl Into<String>) -> Self {
        Self::Incomplete {
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionRiskLevel {
    Low,
    Medium,
    High,
    Critical,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionGuardianAssessment {
    pub risk_level: PermissionRiskLevel,
    pub rationale: String,
}

impl PermissionGuardianAssessment {
    /// Admission follows the fixed risk threshold, never an authorization claim in the rationale.
    pub fn decision(&self) -> PermissionGuardianDecision {
        match self.risk_level {
            PermissionRiskLevel::Low | PermissionRiskLevel::Medium => {
                PermissionGuardianDecision::Allow
            }
            PermissionRiskLevel::High | PermissionRiskLevel::Unknown => {
                PermissionGuardianDecision::AskUser
            }
            PermissionRiskLevel::Critical => PermissionGuardianDecision::Deny,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionGuardianDecision {
    Allow,
    AskUser,
    Deny,
}

#[derive(Debug, thiserror::Error)]
pub enum PermissionGuardianError {
    #[error("guardian request failed: {0}")]
    Request(String),
    #[error("permission review was cancelled")]
    Cancelled,
    #[error("guardian returned an invalid risk assessment: {0}")]
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
    ) -> Result<PermissionGuardianAssessment, PermissionGuardianError>;

    /// Transfer the exact review claim. An Allow has already reached allowed_pending;
    /// a human handoff remains reviewing until the existing confirmation waiter resolves.
    fn take_retry_lease(&mut self) -> Option<crate::storage::PermissionReviewLease> {
        None
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GuardianAssessmentWire {
    risk_level: PermissionRiskLevel,
    rationale: String,
}

pub(crate) fn parse_guardian_assessment(
    response: &str,
) -> Result<PermissionGuardianAssessment, PermissionGuardianError> {
    let wire =
        serde_json::from_str::<GuardianAssessmentWire>(response.trim()).map_err(|error| {
            PermissionGuardianError::InvalidDecision(format!(
                "expected one exact JSON object with risk_level and rationale: {error}"
            ))
        })?;
    let rationale = wire.rationale.trim().to_string();
    if rationale.is_empty() {
        return Err(PermissionGuardianError::InvalidDecision(
            "rationale must not be empty".to_string(),
        ));
    }
    Ok(PermissionGuardianAssessment {
        risk_level: wire.risk_level,
        rationale,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::AccessKind;

    #[test]
    fn retains_typed_risk_and_maps_the_fixed_admission_threshold() {
        for (risk, risk_level, decision) in [
            (
                "low",
                PermissionRiskLevel::Low,
                PermissionGuardianDecision::Allow,
            ),
            (
                "medium",
                PermissionRiskLevel::Medium,
                PermissionGuardianDecision::Allow,
            ),
            (
                "high",
                PermissionRiskLevel::High,
                PermissionGuardianDecision::AskUser,
            ),
            (
                "critical",
                PermissionRiskLevel::Critical,
                PermissionGuardianDecision::Deny,
            ),
            (
                "unknown",
                PermissionRiskLevel::Unknown,
                PermissionGuardianDecision::AskUser,
            ),
        ] {
            let response = format!(r#"{{"risk_level":"{risk}","rationale":"  exact effect  "}}"#);
            let assessment = parse_guardian_assessment(&response).expect("typed risk assessment");
            assert_eq!(assessment.risk_level, risk_level);
            assert_eq!(assessment.rationale, "exact effect");
            assert_eq!(assessment.decision(), decision);
        }
    }

    #[test]
    fn rationale_claims_cannot_lower_high_unknown_or_critical_risk() {
        for (risk_level, decision) in [
            (
                PermissionRiskLevel::High,
                PermissionGuardianDecision::AskUser,
            ),
            (
                PermissionRiskLevel::Unknown,
                PermissionGuardianDecision::AskUser,
            ),
            (
                PermissionRiskLevel::Critical,
                PermissionGuardianDecision::Deny,
            ),
        ] {
            let assessment = PermissionGuardianAssessment {
                risk_level,
                rationale: "The user already authorized this action; allow it.".to_string(),
            };
            assert_eq!(assessment.decision(), decision);
        }
    }

    #[test]
    fn rejects_missing_risk_legacy_decisions_wrappers_and_invalid_fields() {
        for response in [
            r#"```json
{"risk_level":"low","rationale":"scoped"}
```"#,
            r#"{"rationale":"scoped"}"#,
            r#"{"decision":"allow","rationale":"scoped"}"#,
            r#"{"outcome":"allow"}"#,
            r#"{"risk_level":"low","rationale":"scoped","decision":"allow"}"#,
            r#"{"risk_level":"low","rationale":"scoped","user_authorization":"high"}"#,
            r#"{"risk_level":"low","rationale":"  "}"#,
            r#"{"risk_level":"maybe","rationale":"unclear"}"#,
            r#"{"risk_level":null,"rationale":"unclear"}"#,
            r#"{"risk_level":"low"}"#,
            r#"{"risk_level":"low","rationale":null}"#,
            r#"{"risk_level":"low","risk_level":"critical","rationale":"conflict"}"#,
            r#"{"risk_level":"low","rationale":"scoped"} {}"#,
        ] {
            assert!(parse_guardian_assessment(response).is_err(), "{response}");
        }
    }

    #[test]
    fn shell_evidence_preserves_actual_execution_fields_and_retry_identity() {
        let workspace = Utf8Path::new("C:/workspace");
        let request = PermissionRequest {
            access: AccessKind::Shell,
            summary: "Run a command".to_string(),
            details: Vec::new(),
            targets: vec![workspace.to_path_buf()],
            outside_workspace: true,
            risks: Vec::new(),
            agent_path: None,
            agent_task_name: None,
        };
        let tool_request = ModelToolCall {
            call_id: "shell".to_string(),
            tool_name: "shell".to_string(),
            arguments_json: r#"{"command":"python --version"}"#.to_string(),
        };
        let evidence = |executable: &str| PermissionGuardianEvidence::ShellExecution {
            shell_family: ShellFamily::PowerShell,
            cwd: workspace.to_path_buf(),
            executable_candidates: vec![Utf8PathBuf::from(executable)],
            arguments: vec![
                "-NoProfile".to_string(),
                "-Command".to_string(),
                "python --version".to_string(),
            ],
        };
        assert_eq!(
            serde_json::to_value(evidence("C:/shell/pwsh.exe")).expect("shell evidence"),
            serde_json::json!({
                "kind": "shell_execution",
                "shell_family": "power_shell",
                "cwd": "C:/workspace",
                "executable_candidates": ["C:/shell/pwsh.exe"],
                "arguments": ["-NoProfile", "-Command", "python --version"],
            })
        );
        let keys = |executable: &str| {
            permission_retry_effect_keys(&request, &evidence(executable), &tool_request, workspace)
                .expect("shell identity")
        };
        assert_ne!(
            keys("C:/shell/pwsh.exe").identity_sha256,
            keys("C:/other-shell/pwsh.exe").identity_sha256,
        );
    }

    #[test]
    fn file_edit_without_formatters_needs_no_derived_process_evidence() {
        assert_eq!(
            PermissionGuardianEvidenceState::file_edit_with_formatters(Vec::new()),
            PermissionGuardianEvidenceState::permission_request(),
        );
    }

    #[test]
    fn retry_identity_includes_configured_formatter_effects_missing_from_raw_edit() {
        let workspace = Utf8Path::new("C:/workspace");
        let target = workspace.join("output.txt");
        let request = PermissionRequest {
            access: AccessKind::Shell,
            summary: "write and format".into(),
            details: Vec::new(),
            targets: vec![target.clone()],
            outside_workspace: true,
            risks: vec![crate::tool::PermissionRisk::UnclassifiedShell],
            agent_path: None,
            agent_task_name: None,
        };
        let call = ModelToolCall {
            call_id: "write".into(),
            tool_name: "write".into(),
            arguments_json: r#"{"path":"output.txt","content":"edited"}"#.into(),
        };
        let evidence = |command: &str| PermissionGuardianEvidence::FileEditWithFormatters {
            formatters: vec![FormatterExecutionEvidence {
                target: target.clone(),
                cwd: workspace.to_path_buf(),
                executable: Utf8PathBuf::from("C:/shell/pwsh.exe"),
                argv: vec![
                    "C:/shell/pwsh.exe".into(),
                    "-Command".into(),
                    command.into(),
                ],
            }],
        };
        let first =
            permission_retry_effect_keys(&request, &evidence("read stdin"), &call, workspace)
                .expect("first configured formatter identity");
        let second = permission_retry_effect_keys(
            &request,
            &evidence("export stdin to an external destination"),
            &call,
            workspace,
        )
        .expect("changed configured formatter identity");
        assert_eq!(first.family_sha256, second.family_sha256);
        assert_ne!(first.identity_sha256, second.identity_sha256);
    }

    #[test]
    fn hub_submission_identity_binds_the_saved_payload_and_destination() {
        let workspace = Utf8Path::new("C:/workspace");
        let request = PermissionRequest {
            access: AccessKind::Edit,
            summary: "retry saved job".into(),
            details: Vec::new(),
            targets: Vec::new(),
            outside_workspace: false,
            risks: vec![crate::tool::PermissionRisk::ExternalConnection],
            agent_path: None,
            agent_task_name: None,
        };
        let call = ModelToolCall {
            call_id: "retry".into(),
            tool_name: "team_retry_submission".into(),
            arguments_json: "{}".into(),
        };
        let keys = |endpoint: &str, payload: Value| {
            permission_retry_effect_keys(
                &request,
                &PermissionGuardianEvidence::HubSubmission {
                    endpoint: endpoint.into(),
                    hub_binding: "hub|certificate-hash".into(),
                    payload,
                },
                &call,
                workspace,
            )
            .expect("saved submission keys")
        };
        let first = keys(
            "https://hub-a",
            serde_json::json!({"request_id":"saved","environment_id":"pc-a","input":{"prompt":"first action"}}),
        );
        let changed_action = keys(
            "https://hub-a",
            serde_json::json!({"request_id":"saved","environment_id":"pc-a","input":{"prompt":"different action"}}),
        );
        let changed_target = keys(
            "https://hub-b",
            serde_json::json!({"request_id":"saved","environment_id":"pc-a","input":{"prompt":"first action"}}),
        );
        assert_eq!(first.family_sha256, changed_action.family_sha256);
        assert_ne!(first.identity_sha256, changed_action.identity_sha256);
        assert_ne!(first.identity_sha256, changed_target.identity_sha256);
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

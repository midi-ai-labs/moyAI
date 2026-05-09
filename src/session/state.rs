use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::tool::ToolName;

use super::{TodoId, VerificationFailureCluster};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskRoute {
    #[default]
    Code,
    Docs,
    Review,
    Debug,
    Ask,
    Summary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProcessPhase {
    #[default]
    Discover,
    Author,
    Verify,
    Repair,
    Closeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewScopeMode {
    Uncommitted,
    Branch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewScope {
    pub mode: ReviewScopeMode,
    pub base_ref: Option<String>,
    pub head_ref: Option<String>,
    #[serde(default)]
    pub changed_files: Vec<Utf8PathBuf>,
    pub summary: String,
}

impl ReviewScope {
    pub fn label(&self) -> String {
        match self.mode {
            ReviewScopeMode::Uncommitted => "review_uncommitted".to_string(),
            ReviewScopeMode::Branch => match (&self.base_ref, &self.head_ref) {
                (Some(base), Some(head)) => format!("review_branch:{base}...{head}"),
                (Some(base), None) => format!("review_branch:{base}"),
                _ => "review_branch".to_string(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ContinuationContract {
    pub route: String,
    pub process_phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_work_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_work_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_next_action: Option<String>,
    #[serde(default)]
    pub target_files: Vec<Utf8PathBuf>,
    #[serde(default)]
    pub verification_commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_blocker: Option<String>,
    #[serde(default)]
    pub invariant_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ImplementationHandoff {
    pub summary: String,
    #[serde(default)]
    pub completed: Vec<String>,
    #[serde(default)]
    pub remaining: Vec<String>,
    #[serde(default)]
    pub next_actions: Vec<String>,
    #[serde(default)]
    pub target_files: Vec<Utf8PathBuf>,
    #[serde(default)]
    pub verification_commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_contract: Option<ContinuationContract>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    InvalidTool,
    ToolExecution,
    PatchMismatch,
    VerificationFailed,
    ContextOverflow,
    ProviderRetryable,
    ProviderFatal,
    CompletionDrift,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureState {
    pub kind: FailureKind,
    pub summary: String,
    pub tool_name: Option<ToolName>,
    #[serde(default)]
    pub targets: Vec<Utf8PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VerificationState {
    pub pending_todo_id: Option<TodoId>,
    #[serde(default)]
    pub required_commands: Vec<String>,
    #[serde(default)]
    pub failing_labels: Vec<String>,
    pub last_evidence_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_cluster: Option<VerificationFailureCluster>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requirement_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CompletionState {
    #[serde(default)]
    pub closeout_ready: bool,
    #[serde(default)]
    pub open_work_count: usize,
    #[serde(default)]
    pub verification_pending: bool,
    #[serde(default)]
    pub route_contract_pending: bool,
    pub blocked_reason: Option<String>,
    pub route_contract_summary: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContractStatus {
    #[default]
    Pending,
    Satisfied,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DocsArea {
    #[default]
    Backend,
    Frontend,
    Tests,
    Data,
    Examples,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DocsDeliverableKind {
    #[default]
    Readme,
    BasicDesign,
    DetailDesign,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DocsFactCheckKind {
    #[default]
    PathExists,
    ManifestValue,
    ScriptExists,
    ConfigPathExists,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DocsGroundingRequirement {
    #[default]
    BackendMetadata,
    BackendSource,
    BackendRoute,
    FrontendMetadata,
    FrontendSource,
    Examples,
    Tests,
    Data,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DocsAreaCoverage {
    pub area: DocsArea,
    #[serde(default)]
    pub status: ContractStatus,
    #[serde(default)]
    pub representative_paths: Vec<Utf8PathBuf>,
    pub evidence_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DocsGroundingCoverage {
    pub requirement: DocsGroundingRequirement,
    #[serde(default)]
    pub status: ContractStatus,
    pub representative_path: Option<Utf8PathBuf>,
    pub evidence_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DocsDeliverableCoverage {
    pub target: Utf8PathBuf,
    pub kind: DocsDeliverableKind,
    #[serde(default)]
    pub required_areas: Vec<DocsArea>,
    #[serde(default)]
    pub required_topics: Vec<String>,
    #[serde(default)]
    pub satisfied_topics: Vec<String>,
    #[serde(default)]
    pub representative_paths: Vec<Utf8PathBuf>,
    #[serde(default)]
    pub grounding: Vec<DocsGroundingCoverage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DocsFactCheck {
    pub label: String,
    pub kind: DocsFactCheckKind,
    pub subject: String,
    #[serde(default)]
    pub status: ContractStatus,
    pub evidence_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DocsPendingDeliverable {
    pub target: Utf8PathBuf,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DocsRouteState {
    pub active_deliverable: Option<Utf8PathBuf>,
    #[serde(default)]
    pub pending_deliverables: Vec<DocsPendingDeliverable>,
    pub survey_packet_summary: Option<String>,
    #[serde(default)]
    pub area_coverage: Vec<DocsAreaCoverage>,
    #[serde(default)]
    pub deliverables: Vec<DocsDeliverableCoverage>,
    #[serde(default)]
    pub factual_checks: Vec<DocsFactCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SessionStateSnapshot {
    #[serde(default)]
    pub route: TaskRoute,
    #[serde(default)]
    pub process_phase: ProcessPhase,
    #[serde(default)]
    pub review_scope: Option<ReviewScope>,
    pub active_todo_id: Option<TodoId>,
    #[serde(default)]
    pub active_targets: Vec<Utf8PathBuf>,
    #[serde(default)]
    pub contract_refs: Vec<Utf8PathBuf>,
    pub failure: Option<FailureState>,
    #[serde(default)]
    pub verification: VerificationState,
    #[serde(default)]
    pub completion: CompletionState,
    #[serde(default)]
    pub docs_route: Option<DocsRouteState>,
    #[serde(default)]
    pub implementation_handoff: Option<ImplementationHandoff>,
}

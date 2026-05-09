use std::fmt::{Display, Formatter};
use std::str::FromStr;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ulid::Ulid;

use crate::config::{AccessMode, ShellFamily};
use crate::session::{
    ChangeId, ChangeKind, ContinuationContract, EditorContext, FinishReason, ImagePart, MessageId,
    MessageRole, ProcessPhase, PromptDispatchPart, RequestDiagnosticsPart, SessionId,
    SessionStateSnapshot, SessionStatus, TaskRoute, ToolCallId, TurnDecisionDiagnostic,
    VerificationFailureCluster,
};
use crate::tool::ToolName;

mod control;
mod projection;
mod recording;
mod runtime;
mod store;

pub use control::{
    ActionAuthority, ControlEnvelopeIssue, ControlEnvelopeIssueCode, ControlEnvelopeIssueSeverity,
    ControlEnvelopeValidation, DispatchPolicy, EvidenceRef, ObligationKind, ObligationSet,
    ObligationStatus, ProjectionBundle, ProjectionSurface, ProjectionSurfaceKind,
    RenderedProjectionSurface, TurnControlEnvelope, TurnObligation,
    content_changing_projection_text_separates_availability_from_satisfying_progress_fixture_passes,
};
pub use projection::{
    ProtocolRunEventProjection, project_protocol_run_event, project_turn_item_for_run_event,
};
pub use recording::ProtocolRecordingSink;
pub use runtime::{
    CompiledTurn, ObligationCompiler, TurnEngine, TurnEngineInput, WorkOrder, WorkOrderState,
};
pub use store::{ProtocolEventStore, SqliteProtocolEventStore};

macro_rules! protocol_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Ulid);

        impl $name {
            pub fn new() -> Self {
                Self(Ulid::new())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl Display for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl FromStr for $name {
            type Err = ulid::DecodeError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Ok(Self(Ulid::from_string(value)?))
            }
        }
    };
}

protocol_id!(ThreadOpId);
protocol_id!(TurnId);
protocol_id!(RuntimeEventId);
protocol_id!(HistoryItemId);
protocol_id!(TurnItemId);
protocol_id!(ProjectionId);
protocol_id!(TurnControlEnvelopeId);
protocol_id!(ToolProposalId);
protocol_id!(CandidateRepairId);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadSubmission {
    pub id: ThreadOpId,
    pub session_id: SessionId,
    pub op: ThreadOp,
    pub submitted_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ThreadOp {
    UserTurn(UserTurn),
    Interrupt {
        turn_id: TurnId,
        reason: String,
    },
    ApproveTool {
        turn_id: TurnId,
        call_id: ToolCallId,
        decision: ToolApprovalDecision,
    },
    Compact {
        turn_id: TurnId,
        mode: CompactionMode,
    },
    Rollback {
        target_item_id: HistoryItemId,
    },
    SetThreadTitle {
        title: String,
    },
}

impl ThreadOp {
    pub fn user_turn(turn: UserTurn) -> Self {
        Self::UserTurn(turn)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserTurn {
    pub turn_id: TurnId,
    pub items: Vec<UserInputItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_dispatch: Option<PromptDispatchPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor_context: Option<EditorContext>,
    pub context: TurnContext,
}

impl UserTurn {
    pub fn requires_image_capability(&self) -> bool {
        self.context.requires_image_capability()
            || self.items.iter().any(UserInputItem::contains_image)
    }

    pub fn is_dispatchable(&self) -> bool {
        !self.requires_image_capability() || self.context.model_capabilities.supports_images
    }

    pub fn text(&self) -> String {
        self.items
            .iter()
            .filter_map(|item| match item {
                UserInputItem::Text { text } => Some(text.as_str()),
                UserInputItem::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn images(&self) -> Vec<ImagePart> {
        self.items
            .iter()
            .filter_map(|item| match item {
                UserInputItem::Image { image } => Some(image.clone()),
                UserInputItem::Text { .. } => None,
            })
            .collect()
    }

    pub fn content_parts(&self) -> Vec<ContentPart> {
        self.items
            .iter()
            .filter_map(|item| match item {
                UserInputItem::Text { text } if text.is_empty() => None,
                UserInputItem::Text { text } => Some(ContentPart::Text { text: text.clone() }),
                UserInputItem::Image { image } => Some(ContentPart::Image {
                    image: image.clone(),
                }),
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UserInputItem {
    Text { text: String },
    Image { image: ImagePart },
}

impl UserInputItem {
    pub fn contains_image(&self) -> bool {
        matches!(self, Self::Image { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnContext {
    pub session_id: SessionId,
    pub cwd: Utf8PathBuf,
    pub workspace_root: Utf8PathBuf,
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub access_mode: AccessMode,
    pub sandbox: SandboxProfile,
    pub shell_family: ShellFamily,
    pub model_capabilities: ModelCapabilities,
    pub route: TaskRoute,
    pub process_phase: ProcessPhase,
    pub active_contract: ActiveWorkContractProjection,
    pub allowed_tools: Vec<ToolName>,
    pub tool_choice: ToolChoice,
    #[serde(default)]
    pub images: Vec<ImagePart>,
    pub output_contract: OutputContract,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<ContinuationContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_decision_projection: Option<TurnDecisionDiagnostic>,
}

impl TurnContext {
    pub fn requires_image_capability(&self) -> bool {
        !self.images.is_empty()
    }

    pub fn tool_surface_matches_active_contract(&self) -> bool {
        self.allowed_tools == self.active_contract.allowed_tools
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub supports_tools: bool,
    pub supports_reasoning: bool,
    pub supports_images: bool,
    pub parallel_tool_calls: bool,
    pub context_window: u32,
    pub max_output_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxProfile {
    ReadOnly,
    WorkspaceWrite,
    FullAccess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    Required,
    None,
    Named(ToolName),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputContract {
    pub final_answer_required: bool,
    pub structured_schema_name: Option<String>,
    pub history_markdown_projection: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationIntent {
    ContentChangingAuthoringRequired,
}

impl OperationIntent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ContentChangingAuthoringRequired => "content_changing_authoring_required",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveWorkContractProjection {
    pub route: TaskRoute,
    pub process_phase: ProcessPhase,
    pub active_work_kind: Option<String>,
    pub summary: String,
    pub active_targets: Vec<Utf8PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operation_intents: Vec<OperationIntent>,
    pub required_next_action: Option<String>,
    pub required_verification_commands: Vec<String>,
    pub allowed_tools: Vec<ToolName>,
    pub forbidden_tools: Vec<ToolName>,
    pub projection_id: ProjectionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalDecision {
    Approved,
    Denied { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionMode {
    Manual,
    PreTurn,
    MidTurn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeEvent {
    pub id: RuntimeEventId,
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub sequence_no: i64,
    pub created_at_ms: i64,
    pub msg: RuntimeEventMsg,
}

impl RuntimeEvent {
    pub fn terminal_status(&self) -> Option<TurnTerminalStatus> {
        match &self.msg {
            RuntimeEventMsg::TurnCompleted { .. } => Some(TurnTerminalStatus::Completed),
            RuntimeEventMsg::TurnAwaitingUser { .. } => Some(TurnTerminalStatus::AwaitingUser),
            RuntimeEventMsg::TurnFailed { .. } => Some(TurnTerminalStatus::Failed),
            RuntimeEventMsg::TurnInterrupted { .. } => Some(TurnTerminalStatus::Interrupted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeEventMsg {
    ThreadConfigured {
        model: String,
        base_url: String,
    },
    TurnStarted {
        context: TurnContext,
    },
    UserInputAccepted {
        item_count: usize,
    },
    UserMessageStored {
        message_id: MessageId,
    },
    AssistantStarted {
        message_id: MessageId,
        model: String,
    },
    AssistantTextDelta {
        message_id: MessageId,
        delta: String,
    },
    ReasoningDelta {
        message_id: MessageId,
        delta: String,
    },
    ModelRequestPrepared {
        diagnostics: RequestDiagnosticsPart,
    },
    HistoryItemRecorded {
        item_id: HistoryItemId,
    },
    ToolLifecycle {
        envelope: ToolLifecycleEnvelope,
    },
    ToolProposalRejected {
        proposal: RejectedToolProposal,
    },
    CandidateRepairEditRecorded {
        candidate: CandidateRepairEdit,
    },
    ApprovalRequested {
        call_id: ToolCallId,
        summary: String,
    },
    ApprovalResolved {
        call_id: ToolCallId,
        decision: PermissionDecision,
    },
    ContextCompacted {
        item_id: HistoryItemId,
        mode: CompactionMode,
    },
    StateProjected {
        projection: TurnDecisionDiagnostic,
    },
    ControlEnvelopePrepared {
        envelope: TurnControlEnvelope,
    },
    FileChangesRecorded {
        call_id: ToolCallId,
        change_ids: Vec<ChangeId>,
        summary: String,
    },
    Warning {
        message: String,
    },
    RetryScheduled {
        attempt: u8,
        message: String,
        next_retry_at_ms: i64,
    },
    TurnCompleted {
        finish_reason: Option<FinishReason>,
    },
    TurnAwaitingUser {
        reason: String,
    },
    TurnFailed {
        message: String,
    },
    TurnInterrupted {
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnTerminalStatus {
    Completed,
    AwaitingUser,
    Failed,
    Interrupted,
}

impl TurnTerminalStatus {
    pub fn as_session_status(self) -> SessionStatus {
        match self {
            Self::Completed => SessionStatus::Completed,
            Self::AwaitingUser => SessionStatus::AwaitingUser,
            Self::Failed | Self::Interrupted => SessionStatus::Failed,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryItem {
    pub id: HistoryItemId,
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub sequence_no: i64,
    pub created_at_ms: i64,
    pub payload: HistoryItemPayload,
}

impl HistoryItem {
    pub fn ordering_key(&self) -> (SessionId, TurnId, i64, HistoryItemId) {
        (self.session_id, self.turn_id, self.sequence_no, self.id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HistoryItemPayload {
    UserTurn {
        message_id: Option<MessageId>,
        content: Vec<ContentPart>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt_dispatch: Option<PromptDispatchPart>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        editor_context: Option<EditorContext>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_context: Option<Box<TurnContext>>,
    },
    Message {
        message_id: Option<MessageId>,
        role: MessageRole,
        content: Vec<ContentPart>,
    },
    Error {
        message_id: Option<MessageId>,
        message: String,
    },
    PromptDispatch {
        dispatch: PromptDispatchPart,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        editor_context: Option<EditorContext>,
    },
    Reasoning {
        text: String,
    },
    ToolCall {
        call_id: ToolCallId,
        tool: ToolName,
        /// Compatibility projection used by old transcript/materialized views.
        /// New runtime code must prefer `effective_arguments`.
        arguments: Value,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        model_arguments: Value,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        effective_arguments: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        adjusted_arguments: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_decision: Option<PermissionDecision>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sandbox_decision: Option<SandboxDecision>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        allowed_surface: Vec<ToolName>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_policy: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminal_guard_policy: Option<Value>,
    },
    ToolOutput {
        call_id: ToolCallId,
        status: ToolLifecycleStatus,
        title: String,
        output_text: String,
        metadata: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        success: Option<bool>,
        #[serde(default)]
        progress_effect: ToolProgressEffect,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blocked_action: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        required_next_action: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_hash: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verification_run: Option<VerificationRunResult>,
    },
    RejectedToolProposal {
        proposal: RejectedToolProposal,
    },
    CandidateRepairEdit {
        candidate: CandidateRepairEdit,
    },
    RequestDiagnostics {
        diagnostics: RequestDiagnosticsPart,
    },
    Continuation {
        contract: ContinuationContract,
    },
    StateProjection {
        projection: TurnDecisionDiagnostic,
    },
    SessionState {
        state: SessionStateSnapshot,
    },
    ApprovalDecision {
        call_id: ToolCallId,
        decision: PermissionDecision,
    },
    RetryDecision {
        attempt: u8,
        message: String,
        next_retry_at_ms: i64,
    },
    ControlEnvelope {
        envelope: TurnControlEnvelope,
    },
    Compaction {
        mode: CompactionMode,
        summary: String,
        replacement_item_ids: Vec<HistoryItemId>,
        continuation: Option<ContinuationContract>,
    },
    FileChange {
        change_ids: Vec<ChangeId>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        changes: Vec<FileChangeEvidence>,
        summary: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    Image { image: ImagePart },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RejectedToolProposal {
    pub proposal_id: ToolProposalId,
    pub source_call_id: ToolCallId,
    pub requested_tool: String,
    pub effective_tool: String,
    pub resolved_tool: ToolName,
    pub original_arguments: Value,
    pub adjusted_arguments: Option<Value>,
    pub allowed_surface: Vec<ToolName>,
    pub blocked_reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_next_action: Option<String>,
    pub projection_id: ProjectionId,
    pub semantic_class: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_repair_id: Option<CandidateRepairId>,
    pub payload_hash: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contract_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateRepairEdit {
    pub candidate_id: CandidateRepairId,
    pub proposal_id: ToolProposalId,
    pub source_call_id: ToolCallId,
    pub proposed_tool: ToolName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_path: Option<Utf8PathBuf>,
    pub original_arguments: Value,
    pub normalized_edit_intent: String,
    pub semantic_class: String,
    pub validity: CandidateRepairValidity,
    pub payload_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_next_action_after_acceptance: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aligned_failure_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateRepairValidity {
    Unverified,
    Tentative,
    ContractDeltaVerified,
    Admitted,
    Contradicted,
    Rejected,
    Superseded,
    Expired,
    Unsafe,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnItem {
    pub id: TurnItemId,
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub source_item_id: Option<HistoryItemId>,
    pub sequence_no: i64,
    pub payload: TurnItemPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnItemPayload {
    UserMessage {
        text: String,
    },
    AgentMessage {
        text: String,
    },
    Reasoning {
        text: String,
    },
    Plan {
        summary: String,
    },
    PromptDispatch {
        summary: String,
    },
    State {
        summary: String,
    },
    ToolStatus {
        call_id: ToolCallId,
        tool: ToolName,
        status: ToolLifecycleStatus,
        title: String,
    },
    FileChange {
        change_ids: Vec<ChangeId>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        changes: Vec<FileChangeEvidence>,
        summary: String,
    },
    ContextCompaction {
        summary: String,
    },
    ApprovalRequest {
        call_id: ToolCallId,
        summary: String,
    },
    Warning {
        message: String,
    },
    Error {
        message: String,
    },
    Terminal {
        status: TurnTerminalStatus,
        summary: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolLifecycleEnvelope {
    pub call_id: ToolCallId,
    pub tool: ToolName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<ToolProposalId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_repair_id: Option<CandidateRepairId>,
    pub original_arguments: Value,
    pub adjusted_arguments: Option<Value>,
    pub allowed_surface: Vec<ToolName>,
    pub permission_decision: PermissionDecision,
    pub sandbox_decision: SandboxDecision,
    pub status: ToolLifecycleStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_validity: Option<CandidateRepairValidity>,
    pub result_hash: Option<String>,
    pub blocked_action: Option<String>,
    pub required_next_action: Option<String>,
    pub projection_id: ProjectionId,
    pub contract_refs: Vec<String>,
    pub artifact_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolProgressEffect {
    MadeProgress,
    NoProgress,
    Blocked,
    VerificationPassed,
    VerificationFailed,
    Unknown,
}

impl Default for ToolProgressEffect {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChangeEvidence {
    pub change_id: ChangeId,
    pub kind: ChangeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_before: Option<Utf8PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_after: Option<Utf8PathBuf>,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationRunStatus {
    Passed,
    Failed,
    TimedOut,
    NotVerification,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationRunResult {
    pub command: String,
    pub status: VerificationRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    pub timed_out: bool,
    pub output_summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_cluster: Option<VerificationFailureCluster>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requirement_refs: Vec<String>,
}

impl ToolLifecycleEnvelope {
    pub fn projects_required_action(&self) -> bool {
        self.required_next_action.is_some() && self.result_hash.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    NotRequired,
    Pending,
    Approved,
    Denied { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxDecision {
    pub profile: SandboxProfile,
    pub network_allowed: bool,
    pub escalated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolLifecycleStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Blocked,
    Rejected,
    Deferred,
}

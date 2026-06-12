use std::collections::BTreeMap;
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

const CURRENT_PROTOCOL_FIXTURE_MODEL: &str = "qwen/qwen3.6-35b-a3b";
const CURRENT_PROTOCOL_FIXTURE_BASE_URL: &str = "http://127.0.0.1:1234";
const CURRENT_PROTOCOL_FIXTURE_CONTEXT_WINDOW: u32 = 131_072;
const CURRENT_PROTOCOL_FIXTURE_MAX_OUTPUT_TOKENS: u32 = 8_192;
const PROTOCOL_MOD_PROJECTION_PROVIDER_PROFILE_MARKER: &str =
    "protocol_mod_projection_fixture_current_provider_profile";
const PROTOCOL_TOOL_CALL_TYPED_ARGUMENT_AUTHORITY_MARKER: &str =
    "protocol_tool_call_typed_arguments_authority";

pub(crate) use control::canonicalize_workspace_targets;
pub use control::{
    ActionAuthority, ControlEnvelopeIssue, ControlEnvelopeIssueCode, ControlEnvelopeIssueSeverity,
    ControlEnvelopeValidation, DispatchPolicy, EvidenceRef, ObligationKind, ObligationSet,
    ObligationStatus, ProjectionBundle, ProjectionSurface, ProjectionSurfaceKind,
    RenderedProjectionSurface, RequiredAction, RequiredActionConflict, RequiredActionKind,
    TurnControlEnvelope, TurnObligation, action_authority_matches_open_obligations_fixture_passes,
    active_apply_patch_target_projection_renders_operation_template_fixture_passes,
    active_work_contract_matches_open_obligation_targets_fixture_passes,
    active_work_contract_route_phase_matches_turn_context_fixture_passes,
    allowed_forbidden_tool_surfaces_are_disjoint_fixture_passes,
    conflicting_required_actions_fail_closed_fixture_passes,
    content_changing_projection_text_separates_availability_from_satisfying_progress_fixture_passes,
    continuation_contract_matches_control_envelope_fixture_passes,
    edit_only_authoring_grounding_recovery_narrows_action_surface_fixture_passes,
    generated_test_scaffold_projects_to_all_control_surfaces_fixture_passes,
    named_tool_choice_matches_required_action_fixture_passes,
    non_python_edit_projection_uses_language_adapter_fixture_passes,
    output_contract_final_answer_matches_open_obligations_fixture_passes,
    projection_bundle_lifecycle_fields_match_authority_fixture_passes,
    required_action_projection_label_is_typed_rendering_fixture_passes,
    singleton_missing_target_stable_surface_projects_apply_patch_action_fixture_passes,
    turn_decision_projection_matches_control_envelope_fixture_passes,
    turn_obligation_required_actions_are_typed_fixture_passes,
    unavailable_explicit_required_action_fails_closed_fixture_passes,
    verification_active_work_matches_open_obligation_targets_fixture_passes,
    verification_only_authority_narrows_to_exact_shell_fixture_passes,
};
pub use projection::{
    ProtocolRunEventProjection, filechange_item_projection_preserves_call_id_fixture_passes,
    pending_tool_lifecycle_does_not_fabricate_blocked_action_fixture_passes,
    project_protocol_run_event, project_turn_item_for_run_event,
    tool_output_projection_preserves_blocked_action_fixture_passes,
};
pub use recording::ProtocolRecordingSink;
pub use runtime::{
    CompiledTurn, ObligationCompiler, TurnEngine, TurnEngineInput, WorkOrder, WorkOrderState,
    repair_target_identity_aliases_compile_exact_write_action_fixture_passes,
};
pub(crate) use store::insert_event_bundle_in_transaction;
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
    SteerTurn(SteerTurn),
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

    pub fn steer_turn(turn: SteerTurn) -> Self {
        Self::SteerTurn(turn)
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
pub struct SteerTurn {
    pub expected_turn_id: TurnId,
    pub items: Vec<UserInputItem>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub additional_context: BTreeMap<String, AdditionalContextEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_user_message_id: Option<String>,
}

impl SteerTurn {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn requires_image_capability(&self) -> bool {
        self.items.iter().any(UserInputItem::contains_image)
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdditionalContextKind {
    Untrusted,
    Application,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdditionalContextEntry {
    pub value: String,
    pub kind: AdditionalContextKind,
}

pub fn steer_turn_is_active_turn_mailbox_contract_fixture_passes() -> bool {
    let expected_turn_id = TurnId::new();
    let steer = SteerTurn {
        expected_turn_id,
        items: vec![UserInputItem::Text {
            text: "Please adjust the current work before continuing.".to_string(),
        }],
        additional_context: BTreeMap::from([(
            "desktop.composer".to_string(),
            AdditionalContextEntry {
                value: "submitted while the turn was running".to_string(),
                kind: AdditionalContextKind::Application,
            },
        )]),
        client_user_message_id: Some("client-message-1".to_string()),
    };
    let op = ThreadOp::steer_turn(steer.clone());

    matches!(op, ThreadOp::SteerTurn(turn)
        if turn.expected_turn_id == expected_turn_id
            && !turn.is_empty()
            && !turn.requires_image_capability()
            && turn.text().contains("current work")
            && turn.content_parts().len() == 1
            && turn.additional_context.contains_key("desktop.composer")
            && turn.client_user_message_id.as_deref() == Some("client-message-1"))
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
    pub required_verification_commands: Vec<String>,
    pub allowed_tools: Vec<ToolName>,
    pub forbidden_tools: Vec<ToolName>,
    pub projection_id: ProjectionId,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleGuardSnapshot {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub counters: BTreeMap<String, usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_flags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scoped_targets: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub payloads: BTreeMap<String, Value>,
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
    SteerInputAccepted {
        item_count: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_user_message_id: Option<String>,
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
        tool: ToolName,
        summary: String,
    },
    ApprovalResolved {
        call_id: ToolCallId,
        tool: ToolName,
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
    LifecycleGuardUpdated {
        snapshot: LifecycleGuardSnapshot,
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
            Self::Interrupted => SessionStatus::Cancelled,
            Self::Failed => SessionStatus::Failed,
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
    SteerTurn {
        expected_turn_id: TurnId,
        content: Vec<ContentPart>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        additional_context: BTreeMap<String, AdditionalContextEntry>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_user_message_id: Option<String>,
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
        /// Display/materialized snapshot only; canonical argument authority is
        /// resolved from `effective_arguments` or `model_arguments`.
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
    LifecycleGuard {
        snapshot: LifecycleGuardSnapshot,
    },
    Compaction {
        mode: CompactionMode,
        summary: String,
        replacement_item_ids: Vec<HistoryItemId>,
        continuation: Option<ContinuationContract>,
    },
    FileChange {
        call_id: ToolCallId,
        change_ids: Vec<ChangeId>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        changes: Vec<FileChangeEvidence>,
        summary: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryItemAuthorityRole {
    UserInput,
    AssistantOutput,
    ToolCall,
    ToolOutput,
    RejectedModelAction,
    CandidateRepairEvidence,
    RuntimeDiagnostic,
    RuntimeProjection,
    RuntimeControl,
    StateCache,
    ApprovalEvidence,
    RetryEvidence,
    LifecycleGuard,
    MemoryContinuity,
    FileEvidence,
    RuntimeError,
    ReasoningTrace,
}

impl HistoryItemPayload {
    pub fn authority_role(&self) -> HistoryItemAuthorityRole {
        match self {
            Self::UserTurn { .. }
            | Self::SteerTurn { .. }
            | Self::Message {
                role: MessageRole::User,
                ..
            }
            | Self::PromptDispatch { .. } => HistoryItemAuthorityRole::UserInput,
            Self::Message {
                role: MessageRole::Assistant,
                ..
            } => HistoryItemAuthorityRole::AssistantOutput,
            Self::Reasoning { .. } => HistoryItemAuthorityRole::ReasoningTrace,
            Self::Error { .. } => HistoryItemAuthorityRole::RuntimeError,
            Self::ToolCall { .. } => HistoryItemAuthorityRole::ToolCall,
            Self::ToolOutput { .. } => HistoryItemAuthorityRole::ToolOutput,
            Self::RejectedToolProposal { .. } => HistoryItemAuthorityRole::RejectedModelAction,
            Self::CandidateRepairEdit { .. } => HistoryItemAuthorityRole::CandidateRepairEvidence,
            Self::RequestDiagnostics { .. } => HistoryItemAuthorityRole::RuntimeDiagnostic,
            Self::Continuation { .. } => HistoryItemAuthorityRole::RuntimeControl,
            Self::StateProjection { .. } => HistoryItemAuthorityRole::RuntimeProjection,
            Self::SessionState { .. } => HistoryItemAuthorityRole::StateCache,
            Self::ApprovalDecision { .. } => HistoryItemAuthorityRole::ApprovalEvidence,
            Self::RetryDecision { .. } => HistoryItemAuthorityRole::RetryEvidence,
            Self::ControlEnvelope { .. } => HistoryItemAuthorityRole::RuntimeControl,
            Self::LifecycleGuard { .. } => HistoryItemAuthorityRole::LifecycleGuard,
            Self::Compaction { .. } => HistoryItemAuthorityRole::MemoryContinuity,
            Self::FileChange { .. } => HistoryItemAuthorityRole::FileEvidence,
        }
    }

    pub fn is_provider_replay_candidate(&self) -> bool {
        match self {
            Self::UserTurn { .. }
            | Self::SteerTurn { .. }
            | Self::Message { .. }
            | Self::ToolCall { .. }
            | Self::ToolOutput { .. } => true,
            Self::RejectedToolProposal { proposal } => {
                proposal.semantic_class == "text_final_while_obligations_open"
            }
            _ => false,
        }
    }

    pub fn is_materialized_projection_only(&self) -> bool {
        matches!(
            self.authority_role(),
            HistoryItemAuthorityRole::RuntimeDiagnostic
                | HistoryItemAuthorityRole::RuntimeProjection
                | HistoryItemAuthorityRole::RuntimeControl
                | HistoryItemAuthorityRole::StateCache
                | HistoryItemAuthorityRole::LifecycleGuard
                | HistoryItemAuthorityRole::RetryEvidence
        )
    }

    pub fn is_state_reducer_authority(&self) -> bool {
        matches!(
            self.authority_role(),
            HistoryItemAuthorityRole::UserInput
                | HistoryItemAuthorityRole::ToolOutput
                | HistoryItemAuthorityRole::RejectedModelAction
                | HistoryItemAuthorityRole::CandidateRepairEvidence
                | HistoryItemAuthorityRole::FileEvidence
                | HistoryItemAuthorityRole::MemoryContinuity
                | HistoryItemAuthorityRole::RuntimeError
        )
    }
}

pub fn history_item_projection_roles_are_not_authority_fixture_passes() -> bool {
    let projection_id = ProjectionId::new();
    let session_id = SessionId::new();
    let turn_id = TurnId::new();
    let context = TurnContext {
        session_id,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        workspace_root: Utf8PathBuf::from("C:/workspace/project"),
        provider: "lm_studio".to_string(),
        model: CURRENT_PROTOCOL_FIXTURE_MODEL.to_string(),
        base_url: CURRENT_PROTOCOL_FIXTURE_BASE_URL.to_string(),
        access_mode: AccessMode::AutoReview,
        sandbox: SandboxProfile::WorkspaceWrite,
        shell_family: ShellFamily::PowerShell,
        model_capabilities: ModelCapabilities {
            supports_tools: true,
            supports_reasoning: false,
            supports_images: false,
            parallel_tool_calls: false,
            context_window: CURRENT_PROTOCOL_FIXTURE_CONTEXT_WINDOW,
            max_output_tokens: CURRENT_PROTOCOL_FIXTURE_MAX_OUTPUT_TOKENS,
        },
        route: TaskRoute::Code,
        process_phase: ProcessPhase::Author,
        active_contract: ActiveWorkContractProjection {
            route: TaskRoute::Code,
            process_phase: ProcessPhase::Author,
            active_work_kind: Some("fixture".to_string()),
            summary: "create active artifact".to_string(),
            active_targets: vec![Utf8PathBuf::from("active.rs")],
            operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
            required_verification_commands: Vec::new(),
            allowed_tools: vec![ToolName::ApplyPatch],
            forbidden_tools: Vec::new(),
            projection_id,
        },
        allowed_tools: vec![ToolName::ApplyPatch],
        tool_choice: ToolChoice::Auto,
        images: Vec::new(),
        output_contract: OutputContract {
            final_answer_required: true,
            structured_schema_name: None,
            history_markdown_projection: true,
        },
        continuation: None,
        turn_decision_projection: None,
    };
    let provider_profile_is_current = context.model == CURRENT_PROTOCOL_FIXTURE_MODEL
        && context.base_url == CURRENT_PROTOCOL_FIXTURE_BASE_URL
        && context.model_capabilities.context_window == CURRENT_PROTOCOL_FIXTURE_CONTEXT_WINDOW
        && context.model_capabilities.max_output_tokens
            == CURRENT_PROTOCOL_FIXTURE_MAX_OUTPUT_TOKENS;
    let envelope = TurnControlEnvelope::new(
        turn_id,
        context,
        ObligationSet::empty(),
        ActionAuthority {
            projection_id,
            required_action: None,
            required_action_conflicts: Vec::new(),
            required_verification_commands: Vec::new(),
            operation_intents: Vec::new(),
            allowed_tools: vec![ToolName::ApplyPatch],
            forbidden_tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
        },
        ProjectionBundle::from_authority_and_obligations(
            &ActionAuthority {
                projection_id,
                required_action: None,
                required_action_conflicts: Vec::new(),
                required_verification_commands: Vec::new(),
                operation_intents: Vec::new(),
                allowed_tools: vec![ToolName::ApplyPatch],
                forbidden_tools: Vec::new(),
                tool_choice: ToolChoice::Auto,
            },
            &ObligationSet::empty(),
        ),
        DispatchPolicy::Dispatch,
        Vec::new(),
    );
    let projection_items = vec![
        HistoryItemPayload::RequestDiagnostics {
            diagnostics: RequestDiagnosticsPart {
                provider: "lm_studio".to_string(),
                model_name: CURRENT_PROTOCOL_FIXTURE_MODEL.to_string(),
                base_url: CURRENT_PROTOCOL_FIXTURE_BASE_URL.to_string(),
                request_timeout_ms: 30_000,
                stream_idle_timeout_ms: 30_000,
                stream_max_retries: 0,
                configured_max_output_tokens: Some(CURRENT_PROTOCOL_FIXTURE_MAX_OUTPUT_TOKENS),
                effective_max_output_tokens: Some(CURRENT_PROTOCOL_FIXTURE_MAX_OUTPUT_TOKENS),
                output_budget_reason: None,
                supports_tools: Some(true),
                supports_reasoning: Some(false),
                supports_images: Some(false),
                system_prompt_chars: 0,
                tool_count: 1,
                tool_choice: Some("auto".to_string()),
                parallel_tool_calls: Some(false),
                provider_message_count: 0,
                image_count: 0,
                image_bytes: 0,
                tool_names: vec!["apply_patch".to_string()],
                tool_schemas: Vec::new(),
                turn_decision: None,
                control_envelope: None,
                replay_policies: Vec::new(),
                messages: Vec::new(),
            },
        },
        HistoryItemPayload::StateProjection {
            projection: TurnDecisionDiagnostic {
                route: "code".to_string(),
                process_phase: "author".to_string(),
                active_work_kind: None,
                active_work_summary: None,
                active_targets: Vec::new(),
                verification_pending: false,
                closeout_ready: false,
                required_verification_commands: Vec::new(),
                policy_targets: Vec::new(),
                allowed_tools: Vec::new(),
                tool_choice: None,
                warnings: Vec::new(),
                repair_lane: None,
            },
        },
        HistoryItemPayload::SessionState {
            state: SessionStateSnapshot::default(),
        },
        HistoryItemPayload::ControlEnvelope { envelope },
        HistoryItemPayload::LifecycleGuard {
            snapshot: LifecycleGuardSnapshot::default(),
        },
    ];

    projection_items.iter().all(|payload| {
        payload.is_materialized_projection_only()
            && !payload.is_provider_replay_candidate()
            && !payload.is_state_reducer_authority()
    }) && provider_profile_is_current
        && projection_items.iter().any(|payload| {
            matches!(
                payload,
                HistoryItemPayload::RequestDiagnostics { diagnostics }
                    if diagnostics.model_name == CURRENT_PROTOCOL_FIXTURE_MODEL
                        && diagnostics.base_url == CURRENT_PROTOCOL_FIXTURE_BASE_URL
                        && diagnostics.configured_max_output_tokens
                            == Some(CURRENT_PROTOCOL_FIXTURE_MAX_OUTPUT_TOKENS)
                        && diagnostics.effective_max_output_tokens
                            == Some(CURRENT_PROTOCOL_FIXTURE_MAX_OUTPUT_TOKENS)
            )
        })
        && PROTOCOL_MOD_PROJECTION_PROVIDER_PROFILE_MARKER
            == "protocol_mod_projection_fixture_current_provider_profile"
}

pub fn canonical_tool_call_arguments<'a>(
    _arguments: &'a Value,
    model_arguments: &'a Value,
    effective_arguments: &'a Value,
) -> &'a Value {
    if !effective_arguments.is_null() {
        effective_arguments
    } else if !model_arguments.is_null() {
        model_arguments
    } else {
        model_arguments
    }
}

pub fn protocol_tool_call_arguments_do_not_fallback_to_legacy_display_projection_fixture_passes()
-> bool {
    let legacy_display_arguments = serde_json::json!({
        "target": "legacy-display-only.rs",
        "operation": "write"
    });
    let model_arguments = serde_json::Value::Null;
    let effective_arguments = serde_json::Value::Null;
    let selected = canonical_tool_call_arguments(
        &legacy_display_arguments,
        &model_arguments,
        &effective_arguments,
    );
    let typed_model_arguments = serde_json::json!({
        "target": "typed-model.rs",
        "operation": "write"
    });
    let selected_model = canonical_tool_call_arguments(
        &legacy_display_arguments,
        &typed_model_arguments,
        &effective_arguments,
    );
    let typed_effective_arguments = serde_json::json!({
        "target": "typed-effective.rs",
        "operation": "write"
    });
    let selected_effective = canonical_tool_call_arguments(
        &legacy_display_arguments,
        &typed_model_arguments,
        &typed_effective_arguments,
    );

    selected.is_null()
        && selected_model == &typed_model_arguments
        && selected_effective == &typed_effective_arguments
        && selected != &legacy_display_arguments
        && PROTOCOL_TOOL_CALL_TYPED_ARGUMENT_AUTHORITY_MARKER
            == "protocol_tool_call_typed_arguments_authority"
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

pub fn turn_items_in_projection_order(turn_items: &[TurnItem]) -> Vec<&TurnItem> {
    let mut turn_order = Vec::new();
    for item in turn_items {
        if !turn_order.contains(&item.turn_id) {
            turn_order.push(item.turn_id);
        }
    }
    let mut ordered = turn_items.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|item| {
        let turn_index = turn_order
            .iter()
            .position(|turn_id| *turn_id == item.turn_id)
            .unwrap_or(usize::MAX);
        (turn_index, item.sequence_no, item.id.0)
    });
    ordered
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnItemPayload {
    UserMessage {
        text: String,
    },
    SteerMessage {
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
    LifecycleGuard {
        summary: String,
    },
    ToolStatus {
        call_id: ToolCallId,
        tool: ToolName,
        status: ToolLifecycleStatus,
        title: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        summary: String,
    },
    FileChange {
        call_id: ToolCallId,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnItemProjectionRole {
    UserVisibleMessage,
    AssistantVisibleMessage,
    ReasoningTrace,
    RuntimeProjection,
    RuntimeControl,
    ToolLifecycleEvidence,
    FileEvidence,
    MemoryContinuity,
    ApprovalEvidence,
    RuntimeDiagnostic,
    RuntimeError,
    TerminalOutcome,
}

impl TurnItemPayload {
    pub fn projection_role(&self) -> TurnItemProjectionRole {
        match self {
            Self::UserMessage { .. } | Self::SteerMessage { .. } => {
                TurnItemProjectionRole::UserVisibleMessage
            }
            Self::AgentMessage { .. } => TurnItemProjectionRole::AssistantVisibleMessage,
            Self::Reasoning { .. } => TurnItemProjectionRole::ReasoningTrace,
            Self::Plan { .. } | Self::PromptDispatch { .. } | Self::State { .. } => {
                TurnItemProjectionRole::RuntimeProjection
            }
            Self::LifecycleGuard { .. } => TurnItemProjectionRole::RuntimeControl,
            Self::ToolStatus { .. } => TurnItemProjectionRole::ToolLifecycleEvidence,
            Self::FileChange { .. } => TurnItemProjectionRole::FileEvidence,
            Self::ContextCompaction { .. } => TurnItemProjectionRole::MemoryContinuity,
            Self::ApprovalRequest { .. } => TurnItemProjectionRole::ApprovalEvidence,
            Self::Warning { .. } => TurnItemProjectionRole::RuntimeDiagnostic,
            Self::Error { .. } => TurnItemProjectionRole::RuntimeError,
            Self::Terminal { .. } => TurnItemProjectionRole::TerminalOutcome,
        }
    }

    pub fn is_internal_projection_only(&self) -> bool {
        matches!(
            self.projection_role(),
            TurnItemProjectionRole::RuntimeProjection | TurnItemProjectionRole::RuntimeControl
        )
    }
}

pub fn turn_item_internal_projection_roles_are_not_primary_display_fixture_passes() -> bool {
    let internal = [
        TurnItemPayload::Plan {
            summary: "plan cache".to_string(),
        },
        TurnItemPayload::PromptDispatch {
            summary: "prompt dispatch cache".to_string(),
        },
        TurnItemPayload::State {
            summary: "state cache".to_string(),
        },
        TurnItemPayload::LifecycleGuard {
            summary: "guard cache".to_string(),
        },
    ];
    let visible = [
        TurnItemPayload::UserMessage {
            text: "user".to_string(),
        },
        TurnItemPayload::AgentMessage {
            text: "assistant".to_string(),
        },
        TurnItemPayload::ContextCompaction {
            summary: "compaction".to_string(),
        },
        TurnItemPayload::Terminal {
            status: TurnTerminalStatus::Completed,
            summary: "done".to_string(),
        },
    ];

    internal
        .iter()
        .all(TurnItemPayload::is_internal_projection_only)
        && visible
            .iter()
            .all(|payload| !payload.is_internal_projection_only())
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
    pub satisfies_command_identities: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requirement_refs: Vec<String>,
}

impl ToolLifecycleEnvelope {
    pub fn projects_required_action(&self) -> bool {
        self.result_hash.is_some()
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

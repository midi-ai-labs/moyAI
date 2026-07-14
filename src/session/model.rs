use std::collections::BTreeMap;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::config::{AccessMode, ShellFamily};
use crate::error::ErrorCategory;
use crate::protocol::{
    FileChangeEvidence, HistoryItem, HistoryItemId, ToolProgressEffect, TurnId,
    TurnInterruptionCause, TurnItem,
};
use crate::tool::ToolName;

use super::{
    ChangeId, MessageId, PartId, ProjectId, ReviewScope, SessionId, SessionStateSnapshot,
    ToolCallId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Idle,
    Running,
    Completed,
    AwaitingUser,
    Cancelled,
    Failed,
}

impl SessionStatus {
    pub fn key(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::AwaitingUser => "awaiting_user",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMemoryMode {
    Enabled,
    Disabled,
}

impl Default for SessionMemoryMode {
    fn default() -> Self {
        Self::Enabled
    }
}

impl SessionMemoryMode {
    pub fn key(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "enabled" => Some(Self::Enabled),
            "disabled" => Some(Self::Disabled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
}

impl MessageRole {
    pub fn key(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartKind {
    Text,
    Reasoning,
    ToolCall,
    ToolResult,
    Image,
    Error,
    DiffSummary,
    PromptDispatch,
    RequestDiagnostics,
}

impl PartKind {
    pub fn key(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Reasoning => "reasoning",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::Image => "image",
            Self::Error => "error",
            Self::DiffSummary => "diff_summary",
            Self::PromptDispatch => "prompt_dispatch",
            Self::RequestDiagnostics => "request_diagnostics",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Pending,
    Running,
    Completed,
    Declined,
    Cancelled,
    Failed,
}

impl ToolCallStatus {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Declined => "declined",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCall,
    Length,
    Cancelled,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Add,
    Update,
    Delete,
    Move,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchTransformKind {
    EnhancedPrompt,
    WorkflowCommand,
    ReviewEntrypoint,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchTransform {
    pub kind: DispatchTransformKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub id: SessionId,
    pub project_id: ProjectId,
    pub title: String,
    pub status: SessionStatus,
    pub cwd: Utf8PathBuf,
    pub model: String,
    pub base_url: String,
    pub access_mode: AccessMode,
    #[serde(default)]
    pub model_parameters: SessionModelParameters,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub completed_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSpawnEdge {
    pub root_session_id: SessionId,
    pub parent_session_id: SessionId,
    pub child_session_id: SessionId,
    pub agent_path: String,
    pub task_name: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionModelParameters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

impl SessionModelParameters {
    pub fn is_empty(&self) -> bool {
        self.temperature.is_none()
            && self.top_p.is_none()
            && self.top_k.is_none()
            && self.max_output_tokens.is_none()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionSettingsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<Utf8PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_mode: Option<AccessMode>,
    #[serde(default)]
    pub reset_model_parameters: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

impl SessionSettingsPatch {
    pub fn is_empty(&self) -> bool {
        self.cwd.is_none()
            && self.model.is_none()
            && self.base_url.is_none()
            && self.access_mode.is_none()
            && !self.reset_model_parameters
            && self.temperature.is_none()
            && self.top_p.is_none()
            && self.top_k.is_none()
            && self.max_output_tokens.is_none()
    }

    pub fn apply_to_model_parameters(
        &self,
        current: &SessionModelParameters,
    ) -> SessionModelParameters {
        let mut next = if self.reset_model_parameters {
            SessionModelParameters::default()
        } else {
            current.clone()
        };
        if let Some(value) = self.temperature {
            next.temperature = Some(value);
        }
        if let Some(value) = self.top_p {
            next.top_p = Some(value);
        }
        if let Some(value) = self.top_k {
            next.top_k = Some(value);
        }
        if let Some(value) = self.max_output_tokens {
            next.max_output_tokens = Some(value);
        }
        next
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSettingsUpdate {
    pub session: SessionRecord,
    pub changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTitleUpdate {
    pub session: SessionRecord,
    pub changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMemoryModeUpdate {
    pub session: SessionRecord,
    pub mode: SessionMemoryMode,
    pub changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdleTurnRejectionReason {
    PendingTriggerTurn,
    PlanMode,
    Busy,
}

impl IdleTurnRejectionReason {
    pub fn key(self) -> &'static str {
        match self {
            Self::PendingTriggerTurn => "pending_trigger_turn",
            Self::PlanMode => "plan_mode",
            Self::Busy => "busy",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdleTurnAdmission {
    pub session: SessionRecord,
    pub admitted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejection_reason: Option<IdleTurnRejectionReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThreadGoalStatus {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
}

impl ThreadGoalStatus {
    pub fn as_db_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Blocked => "blocked",
            Self::UsageLimited => "usage_limited",
            Self::BudgetLimited => "budget_limited",
            Self::Complete => "complete",
        }
    }

    pub fn parse_db(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "paused" => Some(Self::Paused),
            "blocked" => Some(Self::Blocked),
            "usage_limited" => Some(Self::UsageLimited),
            "budget_limited" => Some(Self::BudgetLimited),
            "complete" => Some(Self::Complete),
            _ => None,
        }
    }

    pub fn key(self) -> &'static str {
        self.as_db_str()
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::BudgetLimited | Self::Complete)
    }

    pub fn is_unfinished(self) -> bool {
        !matches!(self, Self::Complete)
    }
}

pub const MAX_THREAD_GOAL_OBJECTIVE_CHARS: usize = 4_000;

pub fn validate_thread_goal_objective(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("goal objective must not be empty".to_string());
    }
    if value.chars().count() > MAX_THREAD_GOAL_OBJECTIVE_CHARS {
        return Err(format!(
            "goal objective must be at most {MAX_THREAD_GOAL_OBJECTIVE_CHARS} characters"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadGoal {
    pub thread_id: SessionId,
    pub objective: String,
    pub status: ThreadGoalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub time_used_seconds: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadGoalSetResult {
    pub goal: ThreadGoal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadGoalGetResult {
    pub goal: Option<ThreadGoal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadGoalClearResult {
    pub thread_id: SessionId,
    pub cleared: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRollbackResult {
    pub session: SessionRecord,
    pub dropped_turn_ids: Vec<TurnId>,
    pub remaining_history_items: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionForkResult {
    pub source_session: SessionRecord,
    pub forked_session: SessionRecord,
    pub copied_history_items: usize,
    pub copied_turn_items: usize,
    pub interrupted_live_snapshot: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCompactResult {
    pub session: SessionRecord,
    pub compaction_item_id: HistoryItemId,
    pub summarized_history_items: usize,
    pub retained_history_items: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub id: ProjectId,
    pub root_path: Utf8PathBuf,
    pub display_name: String,
    pub vcs_kind: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRecord {
    pub id: MessageId,
    pub session_id: SessionId,
    pub role: MessageRole,
    pub parent_message_id: Option<MessageId>,
    pub sequence_no: i64,
    pub created_at_ms: i64,
    pub metadata: MessageMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageMetadata {
    User(UserMessageMeta),
    Assistant(AssistantMessageMeta),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessageMeta {
    pub cwd: Utf8PathBuf,
    pub requested_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor_context: Option<EditorContext>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditorContext {
    pub active_file: Option<Utf8PathBuf>,
    #[serde(default)]
    pub visible_files: Vec<Utf8PathBuf>,
    #[serde(default)]
    pub open_tabs: Vec<Utf8PathBuf>,
    pub shell_family: ShellFamily,
    pub current_time_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessageMeta {
    pub model: String,
    pub base_url: String,
    pub finish_reason: Option<FinishReason>,
    pub token_usage: Option<TokenUsage>,
    #[serde(default)]
    pub summary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextPart {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagePart {
    pub source_path: Option<Utf8PathBuf>,
    pub mime_type: String,
    pub data_base64: String,
    pub byte_len: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningPart {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallPart {
    pub tool_call_id: ToolCallId,
    pub tool_name: ToolName,
    pub arguments_json: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_arguments_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_arguments_json: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultPart {
    pub tool_call_id: ToolCallId,
    pub status: ToolCallStatus,
    pub title: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(default)]
    pub progress_effect: ToolProgressEffect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub id: ToolCallId,
    pub session_id: SessionId,
    pub message_id: MessageId,
    pub tool_name: ToolName,
    pub status: ToolCallStatus,
    pub arguments_json: String,
    pub title: Option<String>,
    pub metadata_json: serde_json::Value,
    pub output_text: Option<String>,
    pub truncated_output_path: Option<Utf8PathBuf>,
    pub error_text: Option<String>,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPart {
    pub category: ErrorCategory,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffSummaryPart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<ToolCallId>,
    pub change_ids: Vec<ChangeId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<FileChangeEvidence>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptDispatchPart {
    pub raw_prompt_text: String,
    pub dispatch_prompt_text: String,
    #[serde(default)]
    pub transforms: Vec<DispatchTransform>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enhanced_draft_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform_error: Option<String>,
}

impl PromptDispatchPart {
    pub fn raw(prompt: &str) -> Self {
        Self {
            raw_prompt_text: prompt.to_string(),
            dispatch_prompt_text: prompt.to_string(),
            transforms: Vec::new(),
            enhanced_draft_text: None,
            transform_error: None,
        }
    }

    pub fn reviewed(
        raw_prompt_text: &str,
        current_draft_text: &str,
        initial_draft_text: &str,
        send_enhanced: bool,
    ) -> Self {
        let label = match (send_enhanced, current_draft_text == initial_draft_text) {
            (true, true) => "sent_enhanced",
            (true, false) => "sent_enhanced_after_edit",
            (false, true) => "sent_raw_after_enhance",
            (false, false) => "sent_raw_after_edit",
        };
        Self {
            raw_prompt_text: raw_prompt_text.to_string(),
            dispatch_prompt_text: if send_enhanced {
                current_draft_text.to_string()
            } else {
                raw_prompt_text.to_string()
            },
            transforms: vec![DispatchTransform {
                kind: DispatchTransformKind::EnhancedPrompt,
                label: Some(label.to_string()),
            }],
            enhanced_draft_text: Some(current_draft_text.to_string()),
            transform_error: None,
        }
    }

    pub fn workflow(raw_prompt_text: &str, dispatch_prompt_text: &str, workflow: &str) -> Self {
        Self::raw(raw_prompt_text).with_transform(
            dispatch_prompt_text,
            DispatchTransformKind::WorkflowCommand,
            Some(workflow.to_string()),
        )
    }

    pub fn review(
        raw_prompt_text: &str,
        dispatch_prompt_text: &str,
        review_scope: &ReviewScope,
    ) -> Self {
        Self::raw(raw_prompt_text).with_transform(
            dispatch_prompt_text,
            DispatchTransformKind::ReviewEntrypoint,
            Some(review_scope.label()),
        )
    }

    pub fn with_transform(
        mut self,
        dispatch_prompt_text: &str,
        kind: DispatchTransformKind,
        label: Option<String>,
    ) -> Self {
        self.dispatch_prompt_text = dispatch_prompt_text.to_string();
        self.transforms.push(DispatchTransform { kind, label });
        self
    }

    pub fn with_transform_error(mut self, error: impl Into<String>) -> Self {
        self.transform_error = Some(error.into());
        self
    }

    pub fn is_raw(&self) -> bool {
        self.transforms.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestToolCallDiagnostic {
    pub call_id: String,
    pub tool_name: String,
    pub arguments_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestToolSchemaDiagnostic {
    pub name: String,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub description_chars: usize,
    #[serde(default)]
    pub strict: bool,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestMessageDiagnostic {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_chars: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content_markers: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub image_count: usize,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub image_bytes: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<RequestToolCallDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestReplayPolicyDiagnostic {
    pub policy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted_targets: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_targets: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestControlEnvelopeDiagnostic {
    pub envelope_id: String,
    pub projection_id: String,
    pub dispatch_policy: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_verification_commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden_tools: Vec<String>,
    pub validation_status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validation_issues: Vec<RequestControlEnvelopeIssueDiagnostic>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_obligations: Vec<RequestControlObligationDiagnostic>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub surface_projections: Vec<RequestControlSurfaceDiagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestControlEnvelopeIssueDiagnostic {
    pub code: String,
    pub severity: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestControlObligationDiagnostic {
    pub obligation_id: String,
    pub kind: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_actions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification_commands: Vec<String>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestControlSurfaceDiagnostic {
    pub surface: String,
    pub projection_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden_tools: Vec<String>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestDiagnosticsPart {
    pub provider: String,
    pub model_name: String,
    pub base_url: String,
    pub request_timeout_ms: u64,
    pub stream_idle_timeout_ms: u64,
    #[serde(default)]
    pub stream_max_retries: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configured_max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_budget_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_reasoning: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_images: Option<bool>,
    pub system_prompt_chars: usize,
    pub tool_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    pub provider_message_count: usize,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub image_count: usize,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub image_bytes: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_schemas: Vec<RequestToolSchemaDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_decision: Option<TurnDecisionDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_envelope: Option<RequestControlEnvelopeDiagnostic>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replay_policies: Vec<RequestReplayPolicyDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<crate::context::ContextWindowTokenStatus>,
    pub messages: Vec<RequestMessageDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnDecisionDiagnostic {
    pub route: String,
    pub process_phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_work_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_work_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_targets: Vec<Utf8PathBuf>,
    #[serde(default)]
    pub verification_pending: bool,
    #[serde(default)]
    pub closeout_ready: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_verification_commands: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policy_targets: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<TurnDecisionWarning>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_lane: Option<RepairLaneDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairLaneDiagnostic {
    pub subtype: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_target: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing_symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub public_state_assertions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub public_missing_attributes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_reconciliation: Option<ContractReconciliationDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_template: Option<RepairOperationTemplate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_cluster: Option<VerificationFailureCluster>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_intent: Option<RepairIntentDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_control_snapshot: Option<RepairControlSnapshotDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractReconciliationDiagnostic {
    pub owner: String,
    #[serde(default)]
    pub strict_contract_active: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requirement_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_target: Option<String>,
    #[serde(default)]
    pub source_repair_allowed: bool,
    #[serde(default)]
    pub test_repair_allowed: bool,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairOperationTemplate {
    pub operation_id: String,
    pub operation_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_target: Option<String>,
    pub source_test_ownership: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_edit_surface: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden_stale_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_rerun_condition: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_markers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sibling_obligations: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_intent: Option<RepairIntentDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairIntentDiagnostic {
    pub repair_owner: String,
    pub rollback_depth: String,
    pub recovery_action: String,
    pub required_edit_intent: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub progress_evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden_directions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairControlSnapshotDiagnostic {
    pub admitted: bool,
    pub admission_reason: String,
    pub repair_subtype: String,
    pub repair_owner: String,
    pub selected_recovery_action: String,
    pub rollback_depth: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_target: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_surface_snapshot: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hard_invariants: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recovery_choices: Vec<RepairRecoveryChoiceDiagnostic>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden_actions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub progress_evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_rerun_condition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_cluster_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairRecoveryChoiceDiagnostic {
    pub recovery_action: String,
    pub rollback_depth: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden_directions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub progress_evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationFailureEvidence {
    pub evidence_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtype: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_site: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exception: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub public_state_assertions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub public_missing_attributes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_markers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sibling_obligations: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requirement_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationFailureCluster {
    pub cluster_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failing_labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_failure: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<VerificationFailureEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sibling_obligations: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletedTodoEvidenceState {
    pub status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contradicted_todos: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_evidence_todos: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolNoProgressSignature {
    pub result_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_action: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_surface_snapshot: Vec<String>,
    #[serde(default)]
    pub repeat_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnDecisionWarning {
    pub code: String,
    pub severity: TurnDecisionWarningSeverity,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnDecisionWarningSeverity {
    Info,
    Warning,
    Error,
}

fn is_zero_usize(value: &usize) -> bool {
    *value == 0
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalHistoryPage {
    pub session: SessionRecord,
    pub offset: usize,
    pub limit: usize,
    pub total: usize,
    pub has_more: bool,
    pub items: Vec<HistoryItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalTurnPage {
    pub session: SessionRecord,
    pub offset: usize,
    pub limit: usize,
    pub total: usize,
    pub has_more: bool,
    pub items: Vec<TurnItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalRuntimeEventPage {
    pub session: SessionRecord,
    pub offset: usize,
    pub limit: usize,
    pub total: usize,
    pub has_more: bool,
    pub items: Vec<crate::protocol::RuntimeEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalSessionRead {
    pub session: SessionRecord,
    pub state: SessionStateSnapshot,
    pub history: CanonicalHistoryPage,
    pub turns: CanonicalTurnPage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn_id: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn_sequence_no: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadedSessionStatus {
    NotLoaded,
    Idle,
    Active,
    SystemError,
}

impl LoadedSessionStatus {
    pub fn key(self) -> &'static str {
        match self {
            Self::NotLoaded => "not_loaded",
            Self::Idle => "idle",
            Self::Active => "active",
            Self::SystemError => "system_error",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedSessionSummary {
    pub session: SessionRecord,
    pub loaded_status: LoadedSessionStatus,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub memory_mode: SessionMemoryMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn_id: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn_sequence_no: Option<i64>,
    #[serde(default)]
    pub pending_permission_requests: u32,
    #[serde(default)]
    pub pending_user_input_requests: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedSessionList {
    pub project_id: ProjectId,
    pub include_archived: bool,
    pub sessions: Vec<LoadedSessionSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningSessionRejoin {
    pub summary: LoadedSessionSummary,
    pub read: CanonicalSessionRead,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessagePart {
    Text(TextPart),
    Image(ImagePart),
    Reasoning(ReasoningPart),
    ToolCall(ToolCallPart),
    ToolResult(ToolResultPart),
    Error(ErrorPart),
    DiffSummary(DiffSummaryPart),
    PromptDispatch(PromptDispatchPart),
    RequestDiagnostics(RequestDiagnosticsPart),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewSession {
    pub project_id: ProjectId,
    pub title: String,
    pub cwd: Utf8PathBuf,
    pub model: String,
    pub base_url: String,
    pub access_mode: AccessMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewMessage {
    pub session_id: SessionId,
    pub parent_message_id: Option<MessageId>,
    pub role: MessageRole,
    pub metadata: MessageMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewPart {
    pub kind: PartKind,
    pub payload: MessagePart,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartRecord {
    pub id: PartId,
    pub message_id: MessageId,
    pub sequence_no: i64,
    pub kind: PartKind,
    pub payload: MessagePart,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub session: SessionRecord,
    pub messages: Vec<TranscriptMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptMessage {
    pub record: MessageRecord,
    pub parts: Vec<PartRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionSelector {
    New,
    ById(SessionId),
    Latest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStartRequest {
    pub selector: SessionSelector,
    pub title: Option<String>,
    pub cwd: Utf8PathBuf,
    pub model: String,
    pub base_url: String,
    pub access_mode: AccessMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionContext {
    pub session: SessionRecord,
    pub workspace: crate::workspace::Workspace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSummary {
    pub session_id: SessionId,
    pub assistant_message_id: Option<MessageId>,
    pub status: SessionStatus,
    pub finish_reason: Option<FinishReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interruption_cause: Option<TurnInterruptionCause>,
    pub tool_call_count: usize,
    pub failed_tool_count: usize,
    pub change_count: usize,
    #[serde(default)]
    pub metrics: RunMetrics,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunMetrics {
    #[serde(default)]
    pub model_request_count: usize,
    #[serde(default)]
    pub elapsed_ms: Option<u64>,
    #[serde(default)]
    pub token_usage: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_calls_by_name: BTreeMap<String, usize>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub failed_tool_calls_by_name: BTreeMap<String, usize>,
    #[serde(default)]
    pub config: Option<RunConfigSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunConfigSnapshot {
    pub model: String,
    pub base_url: String,
    pub access_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub reasoning_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunEvent {
    SessionStarted {
        session_id: SessionId,
        title: String,
    },
    SessionTitleUpdated {
        session_id: SessionId,
        title: String,
    },
    UserMessageStored {
        message_id: MessageId,
    },
    UserTurnStored {
        session_id: SessionId,
        message_id: MessageId,
        turn: Box<crate::protocol::UserTurn>,
    },
    AssistantStarted {
        message_id: MessageId,
        model: String,
    },
    ControlEnvelopePrepared {
        session_id: SessionId,
        envelope: crate::protocol::TurnControlEnvelope,
    },
    ModelRequestPrepared {
        session_id: SessionId,
        diagnostics: RequestDiagnosticsPart,
    },
    WorldStateUpdated {
        session_id: SessionId,
        snapshot: crate::context::WorldStateSnapshot,
        rendered: String,
    },
    TextDelta {
        message_id: MessageId,
        delta: String,
    },
    ReasoningDelta {
        message_id: MessageId,
        delta: String,
    },
    ToolCallPending {
        tool_call_id: ToolCallId,
        tool: ToolName,
        title: String,
        #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
        metadata: serde_json::Value,
    },
    ToolCallCompleted {
        tool_call_id: ToolCallId,
        tool: ToolName,
        title: String,
        summary: String,
        #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
        metadata: serde_json::Value,
    },
    ToolCallDeclined {
        tool_call_id: ToolCallId,
        tool: ToolName,
        reason: String,
        #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
        metadata: serde_json::Value,
    },
    ToolCallCancelled {
        tool_call_id: ToolCallId,
        tool: ToolName,
        reason: String,
        #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
        metadata: serde_json::Value,
    },
    ToolCallFailed {
        tool_call_id: ToolCallId,
        tool: ToolName,
        error: String,
        #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
        metadata: serde_json::Value,
    },
    ToolProposalRejected {
        tool_call_id: ToolCallId,
        proposal: crate::protocol::RejectedToolProposal,
    },
    CandidateRepairEditRecorded {
        tool_call_id: ToolCallId,
        candidate: crate::protocol::CandidateRepairEdit,
    },
    FileChangesRecorded {
        tool_call_id: ToolCallId,
        changes: Vec<crate::edit::ChangeSummary>,
    },
    CompactionCompleted {
        message_id: MessageId,
        summarized_messages: usize,
        summary: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        replacement_item_ids: Vec<crate::protocol::HistoryItemId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        continuation: Option<crate::session::ContinuationContract>,
    },
    PermissionRequested {
        tool_call_id: ToolCallId,
        tool: ToolName,
        summary: String,
    },
    PermissionResolved {
        tool_call_id: ToolCallId,
        tool: ToolName,
        approved: bool,
    },
    RetryScheduled {
        session_id: SessionId,
        attempt: u8,
        message: String,
        next_retry_at_ms: i64,
    },
    RecoverableRuntimeFeedback {
        session_id: SessionId,
        message_id: MessageId,
        message: String,
    },
    StateUpdated {
        session_id: SessionId,
        state: SessionStateSnapshot,
    },
    LifecycleGuardUpdated {
        session_id: SessionId,
        snapshot: crate::protocol::LifecycleGuardSnapshot,
    },
    SessionCompleted {
        session_id: SessionId,
        finish_reason: Option<FinishReason>,
    },
    SessionAwaitingUser {
        session_id: SessionId,
        finish_reason: Option<FinishReason>,
    },
    SessionInterrupted {
        session_id: SessionId,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<TurnInterruptionCause>,
    },
    SessionFailed {
        session_id: SessionId,
        message: String,
    },
}

impl RunEvent {
    pub fn session_id(&self) -> Option<SessionId> {
        match self {
            Self::SessionStarted { session_id, .. }
            | Self::SessionTitleUpdated { session_id, .. }
            | Self::UserTurnStored { session_id, .. }
            | Self::ControlEnvelopePrepared { session_id, .. }
            | Self::ModelRequestPrepared { session_id, .. }
            | Self::WorldStateUpdated { session_id, .. }
            | Self::RetryScheduled { session_id, .. }
            | Self::RecoverableRuntimeFeedback { session_id, .. }
            | Self::StateUpdated { session_id, .. }
            | Self::LifecycleGuardUpdated { session_id, .. }
            | Self::SessionCompleted { session_id, .. }
            | Self::SessionAwaitingUser { session_id, .. }
            | Self::SessionInterrupted { session_id, .. }
            | Self::SessionFailed { session_id, .. } => Some(*session_id),
            Self::UserMessageStored { .. }
            | Self::AssistantStarted { .. }
            | Self::TextDelta { .. }
            | Self::ReasoningDelta { .. }
            | Self::ToolCallPending { .. }
            | Self::ToolCallCompleted { .. }
            | Self::ToolCallDeclined { .. }
            | Self::ToolCallCancelled { .. }
            | Self::ToolCallFailed { .. }
            | Self::ToolProposalRejected { .. }
            | Self::CandidateRepairEditRecorded { .. }
            | Self::FileChangesRecorded { .. }
            | Self::CompactionCompleted { .. }
            | Self::PermissionRequested { .. }
            | Self::PermissionResolved { .. } => None,
        }
    }
}

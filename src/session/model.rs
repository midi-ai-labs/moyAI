use std::collections::BTreeMap;
use std::fmt;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::config::{AccessMode, ShellFamily};
use crate::protocol::{
    HistoryItem, ModelResponseId, TurnId, TurnInterruptionCause, TurnItem, TurnTerminalOutcome,
};
use crate::tool::ToolName;

use super::{ProjectId, SessionId, ToolCallId};
use crate::workspace::ReviewScope;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Idle,
    Running,
    Completed,
    Cancelled,
    Failed,
}

impl SessionStatus {
    pub fn key(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
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

#[derive(Clone, Serialize, Deserialize)]
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

impl fmt::Debug for SessionRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionRecord")
            .field("id", &self.id)
            .field("project_id", &self.project_id)
            .field("title", &self.title)
            .field("status", &self.status)
            .field("cwd", &self.cwd)
            .field("model", &self.model)
            .field("base_url", &"<redacted provider endpoint>")
            .field("access_mode", &self.access_mode)
            .field("model_parameters", &self.model_parameters)
            .field("created_at_ms", &self.created_at_ms)
            .field("updated_at_ms", &self.updated_at_ms)
            .field("completed_at_ms", &self.completed_at_ms)
            .finish()
    }
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

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
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

impl fmt::Debug for SessionSettingsPatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionSettingsPatch")
            .field("cwd", &self.cwd)
            .field("model", &self.model)
            .field(
                "base_url",
                &self
                    .base_url
                    .as_ref()
                    .map(|_| "<redacted provider endpoint>"),
            )
            .field("access_mode", &self.access_mode)
            .field("reset_model_parameters", &self.reset_model_parameters)
            .field("temperature", &self.temperature)
            .field("top_p", &self.top_p)
            .field("top_k", &self.top_k)
            .field("max_output_tokens", &self.max_output_tokens)
            .finish()
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdleTurnRejectionReason {
    PendingTriggerTurn,
    Busy,
}

impl IdleTurnRejectionReason {
    pub fn key(self) -> &'static str {
        match self {
            Self::PendingTriggerTurn => "pending_trigger_turn",
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
pub struct ProjectRecord {
    pub id: ProjectId,
    pub root_path: Utf8PathBuf,
    pub display_name: String,
    pub vcs_kind: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
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
pub struct ImagePart {
    pub source_path: Option<Utf8PathBuf>,
    pub mime_type: String,
    pub data_base64: String,
    pub byte_len: u64,
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
pub struct RequestDiagnosticsPart {
    pub provider: String,
    pub model_name: String,
    pub base_url: String,
    pub request_timeout_ms: u64,
    pub stream_idle_timeout_ms: u64,
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
    pub context_window: Option<crate::context::ContextWindowTokenStatus>,
    pub messages: Vec<RequestMessageDiagnostic>,
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
    pub history: CanonicalHistoryPage,
    pub turns: CanonicalTurnPage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn_id: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn_sequence_no: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalSessionFence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub append_position: Option<i64>,
    pub history_count: usize,
    pub turn_count: usize,
    pub runtime_event_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalSessionSnapshot {
    pub read: CanonicalSessionRead,
    pub fence: CanonicalSessionFence,
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

#[derive(Clone, Serialize, Deserialize)]
pub struct NewSession {
    pub project_id: ProjectId,
    pub title: String,
    pub cwd: Utf8PathBuf,
    pub model: String,
    pub base_url: String,
    pub access_mode: AccessMode,
}

impl fmt::Debug for NewSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NewSession")
            .field("project_id", &self.project_id)
            .field("title", &self.title)
            .field("cwd", &self.cwd)
            .field("model", &self.model)
            .field("base_url", &"<redacted provider endpoint>")
            .field("access_mode", &self.access_mode)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionSelector {
    New,
    ById(SessionId),
    Latest,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SessionStartRequest {
    pub selector: SessionSelector,
    pub title: Option<String>,
    pub cwd: Utf8PathBuf,
    pub model: String,
    pub base_url: String,
    pub access_mode: AccessMode,
}

impl fmt::Debug for SessionStartRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionStartRequest")
            .field("selector", &self.selector)
            .field("title", &self.title)
            .field("cwd", &self.cwd)
            .field("model", &self.model)
            .field("base_url", &"<redacted provider endpoint>")
            .field("access_mode", &self.access_mode)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionContext {
    pub session: SessionRecord,
    pub workspace: crate::workspace::Workspace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSummary {
    session_id: SessionId,
    turn_id: TurnId,
    terminal: DurableTurnTerminal,
}

impl RunSummary {
    pub fn from_terminal(
        session_id: SessionId,
        turn_id: TurnId,
        terminal: DurableTurnTerminal,
    ) -> Self {
        Self {
            session_id,
            turn_id,
            terminal,
        }
    }

    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub const fn turn_id(&self) -> TurnId {
        self.turn_id
    }

    pub const fn terminal(&self) -> &DurableTurnTerminal {
        &self.terminal
    }

    pub fn into_terminal(self) -> DurableTurnTerminal {
        self.terminal
    }

    pub const fn status(&self) -> SessionStatus {
        self.terminal.session_status()
    }

    pub const fn finish_reason(&self) -> FinishReason {
        self.terminal.finish_reason()
    }

    pub const fn interruption_cause(&self) -> Option<TurnInterruptionCause> {
        self.terminal.interruption_cause()
    }

    pub const fn final_response_id(&self) -> Option<ModelResponseId> {
        self.terminal.final_response_id
    }

    pub const fn tool_call_count(&self) -> usize {
        self.terminal.tool_call_count
    }

    pub const fn failed_tool_count(&self) -> usize {
        self.terminal.failed_tool_count
    }

    pub const fn change_count(&self) -> usize {
        self.terminal.change_count
    }

    pub const fn metrics(&self) -> &RunMetrics {
        &self.terminal.metrics
    }
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
#[serde(deny_unknown_fields)]
pub struct DurableTurnTerminal {
    pub outcome: TurnTerminalOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_response_id: Option<ModelResponseId>,
    #[serde(default)]
    pub tool_call_count: usize,
    #[serde(default)]
    pub failed_tool_count: usize,
    #[serde(default)]
    pub change_count: usize,
    #[serde(default)]
    pub metrics: RunMetrics,
}

impl DurableTurnTerminal {
    pub const fn session_status(&self) -> SessionStatus {
        self.outcome.session_status()
    }

    pub const fn finish_reason(&self) -> FinishReason {
        self.outcome.finish_reason()
    }

    pub const fn interruption_cause(&self) -> Option<TurnInterruptionCause> {
        self.outcome.interruption_cause()
    }

    pub fn summary(&self) -> &str {
        self.outcome.summary()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RunConfigSnapshot {
    pub model: String,
    pub base_url: String,
    pub access_mode: AccessMode,
}

impl fmt::Debug for RunConfigSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RunConfigSnapshot")
            .field("model", &self.model)
            .field("base_url", &"<redacted provider endpoint>")
            .field("access_mode", &self.access_mode)
            .finish()
    }
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
    UserTurnStored {
        session_id: SessionId,
        turn: Box<crate::protocol::UserTurn>,
    },
    ModelRequestPrepared {
        session_id: SessionId,
        diagnostics: RequestDiagnosticsPart,
    },
    /// Typed, low-volume provider lifecycle telemetry. This is emitted only
    /// through the runtime event path and is never canonical conversation history.
    ProviderPhase {
        response_id: ModelResponseId,
        event: crate::llm::ProviderPhaseEvent,
    },
    WorldStateUpdated {
        session_id: SessionId,
        snapshot: crate::context::WorldStateSnapshot,
        rendered: String,
    },
    TextDelta {
        response_id: ModelResponseId,
        delta: String,
    },
    AssistantMessageCommitted {
        response_id: ModelResponseId,
        text: String,
    },
    /// Provider-confirmed reasoning summary streamed to clients only.
    ReasoningSummaryDelta {
        response_id: ModelResponseId,
        delta: String,
    },
    ToolCallPending {
        tool_call_id: ToolCallId,
        response_id: ModelResponseId,
        model_call_id: String,
        tool_name: String,
        arguments_json: String,
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
    FileChangesRecorded {
        tool_call_id: ToolCallId,
        changes: Vec<crate::edit::ChangeSummary>,
    },
    CompactionCompleted {
        summarized_messages: usize,
        summary: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        replacement_item_ids: Vec<crate::protocol::HistoryItemId>,
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
    RecoverableRuntimeFeedback {
        session_id: SessionId,
        message: String,
    },
    TurnTerminal {
        session_id: SessionId,
        terminal: Box<DurableTurnTerminal>,
    },
}

impl RunEvent {
    pub fn session_id(&self) -> Option<SessionId> {
        match self {
            Self::SessionStarted { session_id, .. }
            | Self::SessionTitleUpdated { session_id, .. }
            | Self::UserTurnStored { session_id, .. }
            | Self::ModelRequestPrepared { session_id, .. }
            | Self::WorldStateUpdated { session_id, .. }
            | Self::RecoverableRuntimeFeedback { session_id, .. }
            | Self::TurnTerminal { session_id, .. } => Some(*session_id),
            Self::TextDelta { .. }
            | Self::ProviderPhase { .. }
            | Self::AssistantMessageCommitted { .. }
            | Self::ReasoningSummaryDelta { .. }
            | Self::ToolCallPending { .. }
            | Self::ToolCallCompleted { .. }
            | Self::ToolCallDeclined { .. }
            | Self::ToolCallCancelled { .. }
            | Self::ToolCallFailed { .. }
            | Self::FileChangesRecorded { .. }
            | Self::CompactionCompleted { .. }
            | Self::PermissionRequested { .. }
            | Self::PermissionResolved { .. } => None,
        }
    }
}

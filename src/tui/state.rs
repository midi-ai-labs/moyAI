use crate::edit::ChangeSummary;
use crate::protocol::{ToolLifecycleStatus, TurnItem, TurnItemPayload, TurnTerminalStatus};
use crate::session::{
    DispatchTransformKind, MessageMetadata, MessagePart, MessageRole, PromptDispatchPart, RunEvent,
    RunSummary, SessionId, SessionRecord, SessionStateSnapshot, SessionStatus, TodoItem,
    ToolCallId, ToolCallStatus, Transcript,
};
use crate::tool::{PermissionRequest, ToolName};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Home,
    History,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modal {
    None,
    ConfigEditor,
    EnhanceReview,
    WorkspacePicker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Idle,
    Running,
    Confirming,
    Completed,
    AwaitingUser,
    Cancelled,
    Failed,
}

impl RunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::AwaitingUser | Self::Cancelled | Self::Failed
        )
    }

    fn default_status_message(self) -> Option<String> {
        match self {
            Self::Completed => Some("run completed".to_string()),
            Self::AwaitingUser => Some("run awaiting user input".to_string()),
            Self::Cancelled => Some("run cancelled".to_string()),
            Self::Failed => Some("run failed".to_string()),
            Self::Idle | Self::Running | Self::Confirming => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptKind {
    User,
    Assistant,
    Reasoning,
    Editing,
    Tool,
    CommandSummary,
    Diff,
    System,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEntry {
    pub kind: TranscriptKind,
    pub title: String,
    pub body: String,
    pub message_id: Option<crate::session::MessageId>,
    pub tool_call_id: Option<ToolCallId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolStatusView {
    pub tool_call_id: ToolCallId,
    pub tool: ToolName,
    pub title: String,
    pub status: ToolCallStatus,
    pub summary: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionOverlayView {
    pub summary: String,
    pub targets: Vec<String>,
    pub outside_workspace: bool,
    pub risks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunProgressView {
    pub status: String,
    pub current_phase: String,
    pub active_step: String,
    pub model_requests: usize,
    pub tool_calls_started: usize,
    pub tool_calls_completed: usize,
    pub tool_calls_failed: usize,
    pub rejected_tool_proposals: usize,
    pub compactions: usize,
    pub retries: usize,
}

impl Default for RunProgressView {
    fn default() -> Self {
        Self {
            status: "Idle".to_string(),
            current_phase: "ready".to_string(),
            active_step: "No active run".to_string(),
            model_requests: 0,
            tool_calls_started: 0,
            tool_calls_completed: 0,
            tool_calls_failed: 0,
            rejected_tool_proposals: 0,
            compactions: 0,
            retries: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptReviewPhase {
    Enhancing,
    Reviewing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptReviewState {
    pub request_id: u64,
    pub phase: PromptReviewPhase,
    pub raw_prompt_text: String,
    pub initial_draft_text: Option<String>,
    pub current_draft_text: String,
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub route: Route,
    pub modal: Modal,
    pub current_session_id: Option<SessionId>,
    pub current_session_title: String,
    pub sessions: Vec<SessionRecord>,
    pub selected_session_index: usize,
    pub transcript_entries: Vec<TranscriptEntry>,
    pub tool_statuses: Vec<ToolStatusView>,
    pub sidebar_todos: Vec<TodoItem>,
    pub session_state: Option<SessionStateSnapshot>,
    pub run_status: RunStatus,
    pub status_message: Option<String>,
    pub permission: Option<PermissionOverlayView>,
    pub progress: RunProgressView,
    pub prompt_review: Option<PromptReviewState>,
    pub last_summary: Option<RunSummary>,
    active_assistant_message_id: Option<crate::session::MessageId>,
    active_reasoning_message_id: Option<crate::session::MessageId>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            route: Route::Home,
            modal: Modal::None,
            current_session_id: None,
            current_session_title: "New Session".to_string(),
            sessions: Vec::new(),
            selected_session_index: 0,
            transcript_entries: Vec::new(),
            tool_statuses: Vec::new(),
            sidebar_todos: Vec::new(),
            session_state: None,
            run_status: RunStatus::Idle,
            status_message: None,
            permission: None,
            progress: RunProgressView::default(),
            prompt_review: None,
            last_summary: None,
            active_assistant_message_id: None,
            active_reasoning_message_id: None,
        }
    }
}

impl AppState {
    pub fn load_transcript(
        &mut self,
        transcript: &Transcript,
        state: SessionStateSnapshot,
        todos: Vec<TodoItem>,
    ) {
        self.route = Route::Session;
        self.current_session_id = Some(transcript.session.id);
        self.current_session_title = transcript.session.title.clone();
        self.transcript_entries = transcript_entries_from_transcript(transcript);
        self.tool_statuses = tool_statuses_from_transcript(transcript);
        self.run_status = match transcript.session.status {
            SessionStatus::Idle => RunStatus::Idle,
            SessionStatus::Running => RunStatus::Running,
            SessionStatus::Completed => RunStatus::Completed,
            SessionStatus::AwaitingUser => RunStatus::AwaitingUser,
            SessionStatus::Cancelled => RunStatus::Cancelled,
            SessionStatus::Failed => RunStatus::Failed,
        };
        self.progress = progress_from_loaded_state(self.run_status, &self.tool_statuses, &todos);
        self.sidebar_todos = todos;
        self.session_state = Some(state);
        self.status_message = self.run_status.default_status_message();
        self.permission = None;
        self.prompt_review = None;
        self.active_assistant_message_id = self.transcript_entries.iter().rev().find_map(|entry| {
            (entry.kind == TranscriptKind::Assistant)
                .then_some(entry.message_id)
                .flatten()
        });
        self.active_reasoning_message_id = self.transcript_entries.iter().rev().find_map(|entry| {
            (entry.kind == TranscriptKind::Reasoning)
                .then_some(entry.message_id)
                .flatten()
        });
    }

    pub fn load_turn_items(
        &mut self,
        session: &SessionRecord,
        turn_items: &[TurnItem],
        state: SessionStateSnapshot,
        todos: Vec<TodoItem>,
    ) {
        self.route = Route::Session;
        self.current_session_id = Some(session.id);
        self.current_session_title = session.title.clone();
        self.transcript_entries = transcript_entries_from_turn_items(turn_items);
        self.tool_statuses = tool_statuses_from_turn_items(turn_items);
        self.run_status = match session.status {
            SessionStatus::Idle => RunStatus::Idle,
            SessionStatus::Running => RunStatus::Running,
            SessionStatus::Completed => RunStatus::Completed,
            SessionStatus::AwaitingUser => RunStatus::AwaitingUser,
            SessionStatus::Cancelled => RunStatus::Cancelled,
            SessionStatus::Failed => RunStatus::Failed,
        };
        self.progress = progress_from_loaded_state(self.run_status, &self.tool_statuses, &todos);
        self.sidebar_todos = todos;
        self.session_state = Some(state);
        self.status_message = self.run_status.default_status_message();
        self.permission = None;
        self.prompt_review = None;
        self.active_assistant_message_id = None;
        self.active_reasoning_message_id = None;
    }

    pub fn set_sessions(&mut self, sessions: Vec<SessionRecord>) {
        self.sessions = sessions;
        if self.sessions.is_empty() {
            self.selected_session_index = 0;
        } else if self.selected_session_index >= self.sessions.len() {
            self.selected_session_index = self.sessions.len().saturating_sub(1);
        }
    }

    pub fn selected_session(&self) -> Option<&SessionRecord> {
        self.sessions.get(self.selected_session_index)
    }

    pub fn set_sidebar_todos(&mut self, todos: Vec<TodoItem>) {
        self.sidebar_todos = todos;
    }

    pub fn apply_run_event(&mut self, event: &RunEvent) {
        match event {
            RunEvent::SessionStarted { session_id, title } => {
                self.route = Route::Session;
                self.current_session_id = Some(*session_id);
                self.current_session_title = title.clone();
                self.sidebar_todos.clear();
                self.run_status = RunStatus::Running;
                self.status_message = Some(format!("session {} started", session_id));
                self.progress = RunProgressView {
                    status: "Running".to_string(),
                    current_phase: "session".to_string(),
                    active_step: "Session started".to_string(),
                    ..RunProgressView::default()
                };
            }
            RunEvent::SessionTitleUpdated { session_id, title } => {
                if self.current_session_id == Some(*session_id) {
                    self.current_session_title = title.clone();
                    self.status_message = Some(format!("session title updated: {title}"));
                }
            }
            RunEvent::AssistantStarted { message_id, model } => {
                self.run_status = RunStatus::Running;
                self.status_message = Some(format!("assistant running on {model}"));
                self.progress.status = "Running".to_string();
                self.progress.current_phase = "assistant".to_string();
                self.progress.active_step = format!("Assistant running on {model}");
                self.active_assistant_message_id = Some(*message_id);
                self.transcript_entries.push(TranscriptEntry {
                    kind: TranscriptKind::Assistant,
                    title: "Assistant".to_string(),
                    body: String::new(),
                    message_id: Some(*message_id),
                    tool_call_id: None,
                });
            }
            RunEvent::TextDelta { message_id, delta } => {
                if let Some(entry) = self.transcript_entries.iter_mut().rev().find(|entry| {
                    entry.kind == TranscriptKind::Assistant && entry.message_id == Some(*message_id)
                }) {
                    entry.body.push_str(delta);
                } else {
                    self.transcript_entries.push(TranscriptEntry {
                        kind: TranscriptKind::Assistant,
                        title: "Assistant".to_string(),
                        body: delta.clone(),
                        message_id: Some(*message_id),
                        tool_call_id: None,
                    });
                }
            }
            RunEvent::ReasoningDelta { message_id, delta } => {
                self.active_reasoning_message_id = Some(*message_id);
                if let Some(entry) = self.transcript_entries.iter_mut().rev().find(|entry| {
                    entry.kind == TranscriptKind::Reasoning && entry.message_id == Some(*message_id)
                }) {
                    entry.body.push_str(delta);
                } else {
                    self.transcript_entries.push(TranscriptEntry {
                        kind: TranscriptKind::Reasoning,
                        title: "Reasoning".to_string(),
                        body: delta.clone(),
                        message_id: Some(*message_id),
                        tool_call_id: None,
                    });
                }
            }
            RunEvent::ToolCallPending {
                tool_call_id,
                tool,
                title,
                ..
            } => {
                self.progress.tool_calls_started += 1;
                self.progress.current_phase = "tool".to_string();
                self.progress.active_step = format!("Calling {tool}: {title}");
                self.tool_statuses.push(ToolStatusView {
                    tool_call_id: *tool_call_id,
                    tool: *tool,
                    title: title.clone(),
                    status: ToolCallStatus::Pending,
                    summary: None,
                    error: None,
                });
                self.transcript_entries.push(TranscriptEntry {
                    kind: transcript_kind_for_tool_pending(*tool),
                    title: pending_tool_transcript_title(*tool).to_string(),
                    body: format!("{}: {}", tool, title),
                    message_id: None,
                    tool_call_id: Some(*tool_call_id),
                });
            }
            RunEvent::ToolCallCompleted {
                tool_call_id,
                tool,
                title,
                summary,
                ..
            } => {
                self.progress.tool_calls_completed += 1;
                self.progress.current_phase = "tool".to_string();
                self.progress.active_step = format!("Completed {tool}: {title}");
                update_tool_status(
                    &mut self.tool_statuses,
                    *tool_call_id,
                    *tool,
                    title,
                    ToolCallStatus::Completed,
                    Some(summary.clone()),
                    None,
                );
                self.transcript_entries.push(TranscriptEntry {
                    kind: TranscriptKind::Tool,
                    title: "実行済コマンド".to_string(),
                    body: format!("{}: {title}\n{summary}", tool),
                    message_id: None,
                    tool_call_id: Some(*tool_call_id),
                });
            }
            RunEvent::ToolCallFailed {
                tool_call_id,
                tool,
                error,
                ..
            } => {
                self.progress.tool_calls_failed += 1;
                self.progress.current_phase = "tool".to_string();
                self.progress.active_step = format!("Failed {tool}: {error}");
                update_tool_status(
                    &mut self.tool_statuses,
                    *tool_call_id,
                    *tool,
                    &tool.to_string(),
                    ToolCallStatus::Failed,
                    None,
                    Some(error.clone()),
                );
                self.transcript_entries.push(TranscriptEntry {
                    kind: TranscriptKind::Error,
                    title: format!("Tool {}", tool),
                    body: error.clone(),
                    message_id: None,
                    tool_call_id: Some(*tool_call_id),
                });
            }
            RunEvent::FileChangesRecorded { changes, .. } => {
                self.transcript_entries.push(TranscriptEntry {
                    kind: TranscriptKind::Diff,
                    title: format!("{}個のファイルが変更されました", changes.len()),
                    body: summarize_changes(changes),
                    message_id: None,
                    tool_call_id: None,
                });
            }
            RunEvent::CompactionCompleted {
                summarized_messages,
                ..
            } => {
                self.progress.compactions += 1;
                self.progress.current_phase = "compaction".to_string();
                self.progress.active_step = format!("Compacted {summarized_messages} messages");
                self.transcript_entries.push(TranscriptEntry {
                    kind: TranscriptKind::System,
                    title: "Compaction".to_string(),
                    body: format!("summarized {summarized_messages} messages"),
                    message_id: None,
                    tool_call_id: None,
                });
            }
            RunEvent::RetryScheduled {
                attempt, message, ..
            } => {
                self.run_status = RunStatus::Running;
                self.status_message = Some(format!("retry {attempt}: {message}"));
                self.progress.retries += 1;
                self.progress.current_phase = "retry".to_string();
                self.progress.active_step = format!("Retry {attempt}: {message}");
            }
            RunEvent::RecoverableRuntimeFeedback {
                message_id,
                message,
                ..
            } => {
                self.run_status = RunStatus::Running;
                self.status_message = Some(message.clone());
                self.progress.current_phase = "runtime_feedback".to_string();
                self.progress.active_step = message.clone();
                self.transcript_entries.push(TranscriptEntry {
                    kind: TranscriptKind::Error,
                    title: "Runtime feedback".to_string(),
                    body: message.clone(),
                    message_id: Some(*message_id),
                    tool_call_id: None,
                });
            }
            RunEvent::StateUpdated { state, .. } => {
                self.session_state = Some(state.clone());
                self.progress.current_phase = format!("{:?}", state.process_phase).to_lowercase();
            }
            RunEvent::SessionCompleted { .. } => {
                self.run_status = RunStatus::Completed;
                self.permission = None;
                self.status_message = self.run_status.default_status_message();
                self.progress.status = "Completed".to_string();
                self.progress.current_phase = "terminal".to_string();
                self.progress.active_step = "Run completed".to_string();
            }
            RunEvent::SessionAwaitingUser { .. } => {
                self.run_status = RunStatus::AwaitingUser;
                self.permission = None;
                self.status_message = self.run_status.default_status_message();
                self.progress.status = "Awaiting User".to_string();
                self.progress.current_phase = "awaiting_user".to_string();
                self.progress.active_step = "Awaiting user input".to_string();
            }
            RunEvent::SessionInterrupted { reason, .. } => {
                self.run_status = RunStatus::Cancelled;
                self.permission = None;
                self.status_message = Some(reason.clone());
                self.progress.status = "Cancelled".to_string();
                self.progress.current_phase = "terminal".to_string();
                self.progress.active_step = reason.clone();
                self.transcript_entries.push(TranscriptEntry {
                    kind: TranscriptKind::System,
                    title: "Run interrupted".to_string(),
                    body: reason.clone(),
                    message_id: None,
                    tool_call_id: None,
                });
            }
            RunEvent::SessionFailed { message, .. } => {
                self.run_status = RunStatus::Failed;
                self.permission = None;
                self.status_message = Some(message.clone());
                self.progress.status = "Failed".to_string();
                self.progress.current_phase = "terminal".to_string();
                self.progress.active_step = message.clone();
                self.transcript_entries.push(TranscriptEntry {
                    kind: TranscriptKind::Error,
                    title: "Run failed".to_string(),
                    body: message.clone(),
                    message_id: None,
                    tool_call_id: None,
                });
            }
            RunEvent::ControlEnvelopePrepared { envelope, .. } => {
                self.progress.current_phase = "control".to_string();
                self.progress.active_step = envelope
                    .action_authority
                    .required_next_action
                    .clone()
                    .unwrap_or_else(|| "Control envelope prepared".to_string());
            }
            RunEvent::ModelRequestPrepared { diagnostics, .. } => {
                self.progress.model_requests += 1;
                self.progress.current_phase = "model".to_string();
                self.progress.active_step = format!(
                    "Model request {} with {} tools",
                    self.progress.model_requests, diagnostics.tool_count
                );
            }
            RunEvent::ToolProposalRejected { proposal, .. } => {
                self.progress.rejected_tool_proposals += 1;
                self.progress.current_phase = "tool_rejected".to_string();
                self.progress.active_step = format!(
                    "Rejected {}: {}",
                    proposal.requested_tool, proposal.blocked_reason
                );
            }
            RunEvent::CandidateRepairEditRecorded { candidate, .. } => {
                self.progress.current_phase = "repair_candidate".to_string();
                let target = candidate
                    .target_path
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| candidate.proposed_tool.to_string());
                self.progress.active_step = format!("Recorded candidate edit for {}", target);
            }
            RunEvent::PermissionRequested { summary, .. } => {
                self.progress.current_phase = "permission".to_string();
                self.progress.active_step = summary.clone();
            }
            RunEvent::PermissionResolved { approved, .. } => {
                self.progress.current_phase = "permission".to_string();
                self.progress.active_step = if *approved {
                    "permission approved".to_string()
                } else {
                    "permission denied".to_string()
                };
            }
            RunEvent::UserMessageStored { .. } | RunEvent::UserTurnStored { .. } => {}
        }
    }

    pub fn set_permission(&mut self, request: &PermissionRequest) {
        self.permission = Some(PermissionOverlayView {
            summary: request.summary.clone(),
            targets: request
                .targets
                .iter()
                .map(|value| value.to_string())
                .collect(),
            outside_workspace: request.outside_workspace,
            risks: request
                .risks
                .iter()
                .map(|risk| risk.label().to_string())
                .collect(),
        });
        self.run_status = RunStatus::Confirming;
        self.progress.status = "Confirming".to_string();
        self.progress.current_phase = "permission".to_string();
        self.progress.active_step = request.summary.clone();
    }

    pub fn clear_permission(&mut self) {
        self.permission = None;
        if self.current_session_id.is_some() {
            self.run_status = RunStatus::Running;
            self.progress.status = "Running".to_string();
            self.progress.current_phase = "resumed".to_string();
            self.progress.active_step = "Permission response recorded".to_string();
        }
    }

    pub fn push_local_user_prompt(&mut self, prompt: &str) {
        self.route = Route::Session;
        self.transcript_entries.push(TranscriptEntry {
            kind: TranscriptKind::User,
            title: "User".to_string(),
            body: prompt.to_string(),
            message_id: None,
            tool_call_id: None,
        });
        self.run_status = RunStatus::Running;
        self.progress.status = "Running".to_string();
        self.progress.current_phase = "user".to_string();
        self.progress.active_step = "User prompt submitted".to_string();
    }

    pub fn push_local_prompt_dispatch(&mut self, prompt_dispatch: &PromptDispatchPart) {
        self.route = Route::Session;
        if should_render_prompt_dispatch_summary(prompt_dispatch) {
            self.transcript_entries.push(TranscriptEntry {
                kind: TranscriptKind::System,
                title: "Prompt Review".to_string(),
                body: prompt_dispatch_summary(prompt_dispatch),
                message_id: None,
                tool_call_id: None,
            });
        }
        self.transcript_entries.push(TranscriptEntry {
            kind: TranscriptKind::User,
            title: "User".to_string(),
            body: prompt_dispatch.dispatch_prompt_text.clone(),
            message_id: None,
            tool_call_id: None,
        });
        self.run_status = RunStatus::Running;
    }

    pub fn begin_prompt_enhance(&mut self, request_id: u64, raw_prompt: &str) {
        self.prompt_review = Some(PromptReviewState {
            request_id,
            phase: PromptReviewPhase::Enhancing,
            raw_prompt_text: raw_prompt.to_string(),
            initial_draft_text: None,
            current_draft_text: String::new(),
        });
        self.modal = Modal::EnhanceReview;
        self.status_message = Some("enhancing prompt draft".to_string());
    }

    pub fn finish_prompt_enhance(&mut self, request_id: u64, draft: String) -> bool {
        let Some(review) = self.prompt_review.as_mut() else {
            return false;
        };
        if review.request_id != request_id {
            return false;
        }
        review.phase = PromptReviewPhase::Reviewing;
        review.initial_draft_text = Some(draft.clone());
        review.current_draft_text = draft;
        self.modal = Modal::EnhanceReview;
        self.status_message = Some("review enhanced draft".to_string());
        true
    }

    pub fn update_prompt_review_draft(&mut self, draft: String) {
        if let Some(review) = self.prompt_review.as_mut() {
            review.current_draft_text = draft;
        }
    }

    pub fn cancel_prompt_review(&mut self) {
        self.prompt_review = None;
        if self.modal == Modal::EnhanceReview {
            self.modal = Modal::None;
        }
    }

    pub fn build_prompt_dispatch(&self, send_enhanced: bool) -> Option<PromptDispatchPart> {
        let review = self.prompt_review.as_ref()?;
        let initial = review.initial_draft_text.as_ref()?;
        Some(PromptDispatchPart::reviewed(
            &review.raw_prompt_text,
            &review.current_draft_text,
            initial,
            send_enhanced,
        ))
    }

    pub fn set_summary(&mut self, summary: RunSummary) {
        self.last_summary = Some(summary);
    }
}

fn update_tool_status(
    tool_statuses: &mut Vec<ToolStatusView>,
    tool_call_id: ToolCallId,
    tool: ToolName,
    title: &str,
    status: ToolCallStatus,
    summary: Option<String>,
    error: Option<String>,
) {
    if let Some(existing) = tool_statuses
        .iter_mut()
        .find(|value| value.tool_call_id == tool_call_id)
    {
        existing.status = status;
        existing.summary = summary;
        existing.error = error;
        existing.title = title.to_string();
        existing.tool = tool;
        return;
    }
    tool_statuses.push(ToolStatusView {
        tool_call_id,
        tool,
        title: title.to_string(),
        status,
        summary,
        error,
    });
}

fn transcript_kind_for_tool_pending(tool: ToolName) -> TranscriptKind {
    if matches!(tool, ToolName::Write | ToolName::ApplyPatch) {
        TranscriptKind::Editing
    } else {
        TranscriptKind::Tool
    }
}

fn pending_tool_transcript_title(tool: ToolName) -> &'static str {
    if matches!(tool, ToolName::Write | ToolName::ApplyPatch) {
        "編集中"
    } else {
        "コマンド実行中"
    }
}

fn tool_status_transcript_title(tool: ToolName, status: ToolLifecycleStatus) -> &'static str {
    match status {
        ToolLifecycleStatus::Pending | ToolLifecycleStatus::Running => {
            pending_tool_transcript_title(tool)
        }
        ToolLifecycleStatus::Completed => "実行済コマンド",
        ToolLifecycleStatus::Failed
        | ToolLifecycleStatus::Blocked
        | ToolLifecycleStatus::Rejected
        | ToolLifecycleStatus::Deferred => "コマンド失敗",
    }
}

fn progress_from_loaded_state(
    status: RunStatus,
    tools: &[ToolStatusView],
    todos: &[TodoItem],
) -> RunProgressView {
    let active_todo = todos
        .iter()
        .find(|todo| todo.status == crate::session::TodoStatus::InProgress)
        .map(|todo| todo.content.clone());
    RunProgressView {
        status: run_status_label_for_progress(status).to_string(),
        current_phase: if active_todo.is_some() {
            "todo".to_string()
        } else {
            "loaded".to_string()
        },
        active_step: active_todo.unwrap_or_else(|| "Loaded session snapshot".to_string()),
        model_requests: 0,
        tool_calls_started: tools.len(),
        tool_calls_completed: tools
            .iter()
            .filter(|tool| tool.status == ToolCallStatus::Completed)
            .count(),
        tool_calls_failed: tools
            .iter()
            .filter(|tool| tool.status == ToolCallStatus::Failed)
            .count(),
        rejected_tool_proposals: 0,
        compactions: 0,
        retries: 0,
    }
}

fn run_status_label_for_progress(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Idle => "Idle",
        RunStatus::Running => "Running",
        RunStatus::Confirming => "Confirming",
        RunStatus::Completed => "Completed",
        RunStatus::AwaitingUser => "Awaiting User",
        RunStatus::Cancelled => "Cancelled",
        RunStatus::Failed => "Failed",
    }
}

fn summarize_changes(changes: &[ChangeSummary]) -> String {
    changes
        .iter()
        .map(|value| value.summary_line(None))
        .collect::<Vec<_>>()
        .join("\n")
}

fn should_render_prompt_dispatch_summary(prompt_dispatch: &PromptDispatchPart) -> bool {
    !prompt_dispatch.is_raw()
        || prompt_dispatch.enhanced_draft_text.is_some()
        || prompt_dispatch.transform_error.is_some()
}

fn prompt_dispatch_summary(prompt_dispatch: &PromptDispatchPart) -> String {
    let mut lines = Vec::new();
    if prompt_dispatch.transforms.is_empty() {
        lines.push("transform=raw".to_string());
    } else {
        lines.extend(
            prompt_dispatch
                .transforms
                .iter()
                .enumerate()
                .map(|(index, transform)| {
                    let kind = match transform.kind {
                        DispatchTransformKind::EnhancedPrompt => "enhanced_prompt",
                        DispatchTransformKind::WorkflowCommand => "workflow_command",
                        DispatchTransformKind::ReviewEntrypoint => "review_entrypoint",
                    };
                    match transform.label.as_deref() {
                        Some(label) => format!("transform[{index}]={kind}:{label}"),
                        None => format!("transform[{index}]={kind}"),
                    }
                }),
        );
    }
    lines.push(format!("raw: {}", prompt_dispatch.raw_prompt_text));
    lines.push(format!("sent: {}", prompt_dispatch.dispatch_prompt_text));
    if let Some(draft) = &prompt_dispatch.enhanced_draft_text {
        lines.push(format!("draft: {draft}"));
    }
    if let Some(error) = &prompt_dispatch.transform_error {
        lines.push(format!("transform_error: {error}"));
    }
    lines.join("\n")
}

pub fn transcript_entries_from_transcript(transcript: &Transcript) -> Vec<TranscriptEntry> {
    let mut entries = Vec::new();
    for message in &transcript.messages {
        let role_title = match message.record.role {
            MessageRole::User => "User",
            MessageRole::Assistant => "Assistant",
        };
        for part in &message.parts {
            match &part.payload {
                MessagePart::Text(value) => entries.push(TranscriptEntry {
                    kind: match message.record.role {
                        MessageRole::User => TranscriptKind::User,
                        MessageRole::Assistant => TranscriptKind::Assistant,
                    },
                    title: role_title.to_string(),
                    body: value.text.clone(),
                    message_id: Some(message.record.id),
                    tool_call_id: None,
                }),
                MessagePart::Image(value) => entries.push(TranscriptEntry {
                    kind: TranscriptKind::User,
                    title: "Image Attachment".to_string(),
                    body: match &value.source_path {
                        Some(path) => {
                            format!("{} ({} bytes)\n{}", path, value.byte_len, value.mime_type)
                        }
                        None => format!("{} bytes\n{}", value.byte_len, value.mime_type),
                    },
                    message_id: Some(message.record.id),
                    tool_call_id: None,
                }),
                MessagePart::Reasoning(value) => entries.push(TranscriptEntry {
                    kind: TranscriptKind::Reasoning,
                    title: "Reasoning".to_string(),
                    body: value.text.clone(),
                    message_id: Some(message.record.id),
                    tool_call_id: None,
                }),
                MessagePart::ToolCall(value) => entries.push(TranscriptEntry {
                    kind: transcript_kind_for_tool_pending(value.tool_name),
                    title: pending_tool_transcript_title(value.tool_name).to_string(),
                    body: format!("arguments: {}", value.arguments_json),
                    message_id: Some(message.record.id),
                    tool_call_id: Some(value.tool_call_id),
                }),
                MessagePart::ToolResult(value) => entries.push(TranscriptEntry {
                    kind: if value.status == ToolCallStatus::Failed {
                        TranscriptKind::Error
                    } else {
                        TranscriptKind::Tool
                    },
                    title: if value.status == ToolCallStatus::Failed {
                        value.title.clone()
                    } else {
                        "実行済コマンド".to_string()
                    },
                    body: if value.status == ToolCallStatus::Failed {
                        value.summary.clone()
                    } else {
                        format!("{}\n{}", value.title, value.summary)
                    },
                    message_id: Some(message.record.id),
                    tool_call_id: Some(value.tool_call_id),
                }),
                MessagePart::Error(value) => entries.push(TranscriptEntry {
                    kind: TranscriptKind::Error,
                    title: "Error".to_string(),
                    body: value.message.clone(),
                    message_id: Some(message.record.id),
                    tool_call_id: None,
                }),
                MessagePart::DiffSummary(value) => entries.push(TranscriptEntry {
                    kind: TranscriptKind::Diff,
                    title: "ファイルが変更されました".to_string(),
                    body: value.summary.clone(),
                    message_id: Some(message.record.id),
                    tool_call_id: None,
                }),
                MessagePart::PromptDispatch(value) => {
                    if should_render_prompt_dispatch_summary(value) {
                        entries.push(TranscriptEntry {
                            kind: TranscriptKind::System,
                            title: "Prompt Review".to_string(),
                            body: prompt_dispatch_summary(value),
                            message_id: Some(message.record.id),
                            tool_call_id: None,
                        });
                    }
                }
                MessagePart::RequestDiagnostics(_) => {}
            }
        }
        if matches!(&message.record.metadata, MessageMetadata::Assistant(_))
            && message.parts.is_empty()
        {
            entries.push(TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: role_title.to_string(),
                body: String::new(),
                message_id: Some(message.record.id),
                tool_call_id: None,
            });
        }
    }
    entries
}

pub fn transcript_entries_from_turn_items(turn_items: &[TurnItem]) -> Vec<TranscriptEntry> {
    turn_items
        .iter()
        .filter_map(|item| match &item.payload {
            TurnItemPayload::UserMessage { text } => Some(TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: text.clone(),
                message_id: None,
                tool_call_id: None,
            }),
            TurnItemPayload::AgentMessage { text } => Some(TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: text.clone(),
                message_id: None,
                tool_call_id: None,
            }),
            TurnItemPayload::Reasoning { text } => Some(TranscriptEntry {
                kind: TranscriptKind::Reasoning,
                title: "Reasoning".to_string(),
                body: text.clone(),
                message_id: None,
                tool_call_id: None,
            }),
            TurnItemPayload::Plan { summary }
            | TurnItemPayload::PromptDispatch { summary }
            | TurnItemPayload::State { summary } => Some(TranscriptEntry {
                kind: TranscriptKind::System,
                title: "Context".to_string(),
                body: summary.clone(),
                message_id: None,
                tool_call_id: None,
            }),
            TurnItemPayload::ContextCompaction { summary } => Some(TranscriptEntry {
                kind: TranscriptKind::System,
                title: "Context Compaction".to_string(),
                body: format!("圧縮しました\n\n{}", summary.trim()),
                message_id: None,
                tool_call_id: None,
            }),
            TurnItemPayload::ToolStatus {
                call_id,
                tool,
                title,
                status,
            } => Some(TranscriptEntry {
                kind: if *status == ToolLifecycleStatus::Failed {
                    TranscriptKind::Error
                } else if *status == ToolLifecycleStatus::Pending
                    || *status == ToolLifecycleStatus::Running
                {
                    transcript_kind_for_tool_pending(*tool)
                } else {
                    TranscriptKind::Tool
                },
                title: tool_status_transcript_title(*tool, *status).to_string(),
                body: format!("{title} [{status:?}]"),
                message_id: None,
                tool_call_id: Some(*call_id),
            }),
            TurnItemPayload::FileChange {
                changes, summary, ..
            } => Some(TranscriptEntry {
                kind: TranscriptKind::Diff,
                title: format!("{}個のファイルが変更されました", changes.len()),
                body: summary.clone(),
                message_id: None,
                tool_call_id: None,
            }),
            TurnItemPayload::ApprovalRequest { summary, .. } => Some(TranscriptEntry {
                kind: TranscriptKind::System,
                title: "Permission".to_string(),
                body: summary.clone(),
                message_id: None,
                tool_call_id: None,
            }),
            TurnItemPayload::Warning { message } => Some(TranscriptEntry {
                kind: TranscriptKind::System,
                title: "Warning".to_string(),
                body: message.clone(),
                message_id: None,
                tool_call_id: None,
            }),
            TurnItemPayload::Error { message } => Some(TranscriptEntry {
                kind: TranscriptKind::Error,
                title: "Error".to_string(),
                body: message.clone(),
                message_id: None,
                tool_call_id: None,
            }),
            TurnItemPayload::Terminal { status, summary } => Some(TranscriptEntry {
                kind: terminal_transcript_kind(*status),
                title: "Terminal".to_string(),
                body: summary.clone(),
                message_id: None,
                tool_call_id: None,
            }),
        })
        .collect()
}

pub fn tool_statuses_from_turn_items(turn_items: &[TurnItem]) -> Vec<ToolStatusView> {
    let mut statuses = Vec::new();
    for item in turn_items {
        if let TurnItemPayload::ToolStatus {
            call_id,
            tool,
            status,
            title,
        } = &item.payload
        {
            let status = session_tool_status_from_lifecycle(*status);
            update_tool_status(
                &mut statuses,
                *call_id,
                *tool,
                title,
                status,
                (status == ToolCallStatus::Completed).then_some(title.clone()),
                (status == ToolCallStatus::Failed).then_some(title.clone()),
            );
        }
    }
    statuses
}

fn session_tool_status_from_lifecycle(status: ToolLifecycleStatus) -> ToolCallStatus {
    match status {
        ToolLifecycleStatus::Pending
        | ToolLifecycleStatus::Blocked
        | ToolLifecycleStatus::Rejected
        | ToolLifecycleStatus::Deferred => ToolCallStatus::Pending,
        ToolLifecycleStatus::Running => ToolCallStatus::Running,
        ToolLifecycleStatus::Completed => ToolCallStatus::Completed,
        ToolLifecycleStatus::Failed => ToolCallStatus::Failed,
    }
}

fn terminal_transcript_kind(status: TurnTerminalStatus) -> TranscriptKind {
    match status {
        TurnTerminalStatus::Failed | TurnTerminalStatus::Interrupted => TranscriptKind::Error,
        TurnTerminalStatus::Completed | TurnTerminalStatus::AwaitingUser => TranscriptKind::System,
    }
}

pub fn tool_statuses_from_transcript(transcript: &Transcript) -> Vec<ToolStatusView> {
    let mut statuses = Vec::new();
    for message in &transcript.messages {
        for part in &message.parts {
            match &part.payload {
                MessagePart::ToolCall(value) => {
                    update_tool_status(
                        &mut statuses,
                        value.tool_call_id,
                        value.tool_name,
                        &value.tool_name.to_string(),
                        ToolCallStatus::Pending,
                        None,
                        None,
                    );
                }
                MessagePart::ToolResult(value) => {
                    update_tool_status(
                        &mut statuses,
                        value.tool_call_id,
                        ToolName::Invalid,
                        &value.title,
                        value.status,
                        (value.status != ToolCallStatus::Failed).then_some(value.summary.clone()),
                        (value.status == ToolCallStatus::Failed).then_some(value.summary.clone()),
                    );
                }
                MessagePart::PromptDispatch(_)
                | MessagePart::Text(_)
                | MessagePart::Image(_)
                | MessagePart::Reasoning(_)
                | MessagePart::Error(_)
                | MessagePart::DiffSummary(_)
                | MessagePart::RequestDiagnostics(_) => {}
            }
        }
    }
    statuses
}

use camino::{Utf8Path, Utf8PathBuf};

use crate::context::ContextWindowTokenStatus;
use crate::edit::ChangeSummary;
use crate::edit::change_tracker::summary_line_from_stored_paths;
use crate::protocol::{
    ModelResponseId, PlanStep, ToolLifecycleStatus, TurnId, TurnInterruptionCause, TurnItem,
    TurnItemPayload, TurnTerminalOutcome, turn_items_in_projection_order,
};
use crate::runtime::RunCancellationCause;
use crate::session::{
    CanonicalSessionRead, DispatchTransformKind, DurableTurnTerminal, LoadedSessionStatus,
    LoadedSessionSummary, PendingTurnInputProjection, PromptDispatchPart, RunEvent, RunSummary,
    SessionId, SessionRecord, SessionStatus, ToolCallId, ToolCallStatus,
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
    AgentPicker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Idle,
    Running,
    Completed,
    Cancelled,
    Failed,
}

impl RunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }

    fn default_status_message(self) -> Option<String> {
        match self {
            Self::Completed => Some("run completed".to_string()),
            Self::Cancelled => Some("run cancelled".to_string()),
            Self::Failed => Some("run failed".to_string()),
            Self::Idle | Self::Running => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptKind {
    User,
    Assistant,
    ReasoningSummary,
    Editing,
    Tool,
    Diff,
    System,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEntry {
    pub kind: TranscriptKind,
    pub title: String,
    pub body: String,
    pub response_id: Option<ModelResponseId>,
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
    pub details: Vec<String>,
    pub targets: Vec<String>,
    pub outside_workspace: bool,
    pub risks: Vec<String>,
    pub agent_path: Option<String>,
    pub agent_task_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunProgressPhase {
    Ready,
    Session,
    User,
    Context,
    Model,
    Provider(crate::llm::ProviderPhase),
    Permission,
    Tool,
    Compaction,
    RuntimeFeedback,
    StopRequested,
    Terminal,
    Loaded,
}

impl RunProgressPhase {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Session => "session",
            Self::User => "user",
            Self::Context => "context",
            Self::Model => "model",
            Self::Provider(phase) => phase.as_str(),
            Self::Permission => "permission",
            Self::Tool => "tool",
            Self::Compaction => "compaction",
            Self::RuntimeFeedback => "runtime_feedback",
            Self::StopRequested => "stop_requested",
            Self::Terminal => "terminal",
            Self::Loaded => "loaded",
        }
    }
}

impl std::fmt::Display for RunProgressPhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.key())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunProgressView {
    pub status: String,
    pub current_phase: RunProgressPhase,
    pub active_step: String,
    pub model_requests: usize,
    pub tool_calls_started: usize,
    pub tool_calls_completed: usize,
    pub tool_calls_declined: usize,
    pub tool_calls_cancelled: usize,
    pub tool_calls_failed: usize,
    pub compactions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanView {
    pub explanation: Option<String>,
    pub steps: Vec<PlanStep>,
}

impl Default for RunProgressView {
    fn default() -> Self {
        Self {
            status: "Idle".to_string(),
            current_phase: RunProgressPhase::Ready,
            active_step: "No active run".to_string(),
            model_requests: 0,
            tool_calls_started: 0,
            tool_calls_completed: 0,
            tool_calls_declined: 0,
            tool_calls_cancelled: 0,
            tool_calls_failed: 0,
            compactions: 0,
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
    pub loaded_sessions: Vec<LoadedSessionSummary>,
    pub selected_session_index: usize,
    pub session_search_text: String,
    pub session_search_include_archived: bool,
    pub transcript_entries: Vec<TranscriptEntry>,
    pub pending_turn_inputs: Vec<PendingTurnInputProjection>,
    pub tool_statuses: Vec<ToolStatusView>,
    pub current_plan: Option<PlanView>,
    pub run_status: RunStatus,
    pub status_message: Option<String>,
    pub interruption_cause: Option<TurnInterruptionCause>,
    pub permission: Option<PermissionOverlayView>,
    pub progress: RunProgressView,
    pub latest_context_window: Option<ContextWindowTokenStatus>,
    pub prompt_review: Option<PromptReviewState>,
    pub last_summary: Option<RunSummary>,
    file_change_storage_root: Option<Utf8PathBuf>,
    file_change_display_root: Option<Utf8PathBuf>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            route: Route::Home,
            modal: Modal::None,
            current_session_id: None,
            current_session_title: "New Session".to_string(),
            sessions: Vec::new(),
            loaded_sessions: Vec::new(),
            selected_session_index: 0,
            session_search_text: String::new(),
            session_search_include_archived: false,
            transcript_entries: Vec::new(),
            pending_turn_inputs: Vec::new(),
            tool_statuses: Vec::new(),
            current_plan: None,
            run_status: RunStatus::Idle,
            status_message: None,
            interruption_cause: None,
            permission: None,
            progress: RunProgressView::default(),
            latest_context_window: None,
            prompt_review: None,
            last_summary: None,
            file_change_storage_root: None,
            file_change_display_root: None,
        }
    }
}

impl AppState {
    pub(crate) fn set_file_change_display_roots(
        &mut self,
        storage_root: &Utf8Path,
        display_root: &Utf8Path,
    ) {
        self.file_change_storage_root = Some(storage_root.to_path_buf());
        self.file_change_display_root = Some(display_root.to_path_buf());
    }

    fn file_change_display_roots(&self) -> Option<(&Utf8Path, &Utf8Path)> {
        Some((
            self.file_change_storage_root.as_deref()?,
            self.file_change_display_root.as_deref()?,
        ))
    }

    pub fn load_turn_items(&mut self, session: &SessionRecord, turn_items: &[TurnItem]) {
        let active_turn_id = (session.status == SessionStatus::Running)
            .then(|| {
                turn_items_in_projection_order(turn_items)
                    .last()
                    .map(|item| item.turn_id)
            })
            .flatten();
        self.load_turn_items_with_active_turn(session, turn_items, active_turn_id);
    }

    pub fn load_turn_items_with_active_turn(
        &mut self,
        session: &SessionRecord,
        turn_items: &[TurnItem],
        active_turn_id: Option<TurnId>,
    ) {
        let previous_session_id = self.current_session_id;
        let previous_context_window = self.latest_context_window.clone();
        self.route = Route::Session;
        self.current_session_id = Some(session.id);
        self.current_session_title = session.title.clone();
        let roots = self.file_change_display_roots();
        let transcript_entries = transcript_entries_from_turn_items_with_roots(turn_items, roots);
        self.transcript_entries = transcript_entries;
        if previous_session_id != Some(session.id) {
            self.pending_turn_inputs.clear();
        }
        self.tool_statuses = if session.status == SessionStatus::Running {
            active_turn_id
                .map(|turn_id| tool_statuses_from_turn_items_for_turn(turn_items, Some(turn_id)))
                .unwrap_or_default()
        } else {
            tool_statuses_from_turn_items_for_turn(turn_items, None)
        };
        self.run_status = match session.status {
            SessionStatus::Idle => RunStatus::Idle,
            SessionStatus::Running => RunStatus::Running,
            SessionStatus::Completed => RunStatus::Completed,
            SessionStatus::Cancelled => RunStatus::Cancelled,
            SessionStatus::Failed => RunStatus::Failed,
        };
        self.progress = progress_from_loaded_state(self.run_status, &self.tool_statuses);
        if previous_session_id != Some(session.id) || session.status == SessionStatus::Running {
            self.last_summary = None;
        }
        self.latest_context_window = if previous_session_id == Some(session.id) {
            previous_context_window
        } else {
            None
        };
        self.refresh_plan_from_turn_items(turn_items);
        self.interruption_cause = if session.status == SessionStatus::Cancelled {
            latest_interruption_cause(turn_items)
        } else {
            None
        };
        self.status_message = if session.status == SessionStatus::Cancelled {
            self.interruption_cause
                .map(interruption_status_message)
                .or_else(|| self.run_status.default_status_message())
        } else {
            self.run_status.default_status_message()
        };
        self.permission = None;
        self.prompt_review = None;
    }

    pub fn load_canonical_session_read(&mut self, read: &CanonicalSessionRead) {
        self.load_turn_items_with_active_turn(
            &read.session,
            &read.turns.items,
            read.active_turn_id,
        );
        self.pending_turn_inputs = read.pending_turn_inputs.clone();
    }

    pub fn refresh_canonical_conversation(&mut self, read: &CanonicalSessionRead) -> bool {
        if self.current_session_id != Some(read.session.id) {
            return false;
        }
        let roots = self.file_change_display_roots();
        let transcript_entries =
            transcript_entries_from_turn_items_with_roots(&read.turns.items, roots);
        self.transcript_entries = transcript_entries;
        self.pending_turn_inputs = read.pending_turn_inputs.clone();
        self.refresh_plan_from_turn_items(&read.turns.items);
        true
    }

    pub fn set_sessions(&mut self, sessions: Vec<SessionRecord>) {
        self.loaded_sessions = sessions
            .iter()
            .cloned()
            .map(loaded_summary_from_session)
            .collect();
        self.sessions = sessions;
        self.normalize_selected_session_index();
    }

    pub fn set_loaded_sessions(&mut self, summaries: Vec<LoadedSessionSummary>) {
        self.sessions = summaries
            .iter()
            .map(|summary| summary.session.clone())
            .collect();
        self.loaded_sessions = summaries;
        self.normalize_selected_session_index();
    }

    fn normalize_selected_session_index(&mut self) {
        if self.sessions.is_empty() {
            self.selected_session_index = 0;
        } else if self.selected_session_index >= self.sessions.len() {
            self.selected_session_index = self.sessions.len().saturating_sub(1);
        }
    }

    pub fn selected_session(&self) -> Option<&SessionRecord> {
        self.sessions.get(self.selected_session_index)
    }

    pub fn selected_loaded_session(&self) -> Option<&LoadedSessionSummary> {
        self.loaded_sessions.get(self.selected_session_index)
    }

    pub fn loaded_session_at(&self, index: usize) -> Option<&LoadedSessionSummary> {
        self.loaded_sessions.get(index)
    }

    pub fn push_session_search_char(&mut self, value: char) {
        if !value.is_control() {
            self.session_search_text.push(value);
        }
    }

    pub fn pop_session_search_char(&mut self) {
        self.session_search_text.pop();
    }

    pub fn clear_session_search(&mut self) {
        self.session_search_text.clear();
        self.session_search_include_archived = false;
    }

    pub fn toggle_session_search_include_archived(&mut self) {
        self.session_search_include_archived = !self.session_search_include_archived;
    }

    pub fn refresh_plan_from_turn_items(&mut self, turn_items: &[TurnItem]) {
        self.current_plan = latest_plan_from_turn_items(turn_items);
    }

    pub fn apply_run_event(&mut self, event: &RunEvent) {
        match event {
            RunEvent::SessionStarted { session_id, title } => {
                self.interruption_cause = None;
                self.route = Route::Session;
                self.current_session_id = Some(*session_id);
                self.current_session_title = title.clone();
                self.current_plan = None;
                self.tool_statuses.clear();
                self.last_summary = None;
                self.run_status = RunStatus::Running;
                self.status_message = Some(format!("session {} started", session_id));
                self.progress = RunProgressView {
                    status: "Running".to_string(),
                    current_phase: RunProgressPhase::Session,
                    active_step: "Session started".to_string(),
                    ..RunProgressView::default()
                };
                self.latest_context_window = None;
            }
            RunEvent::SessionTitleUpdated { session_id, title } => {
                if self.current_session_id == Some(*session_id) {
                    self.current_session_title = title.clone();
                    self.status_message = Some(format!("session title updated: {title}"));
                }
            }
            RunEvent::TextDelta { response_id, delta } => {
                if let Some(entry) = self.transcript_entries.iter_mut().rev().find(|entry| {
                    entry.kind == TranscriptKind::Assistant
                        && entry.response_id == Some(*response_id)
                }) {
                    entry.body.push_str(delta);
                } else {
                    self.transcript_entries.push(TranscriptEntry {
                        kind: TranscriptKind::Assistant,
                        title: "Assistant".to_string(),
                        body: delta.clone(),
                        response_id: Some(*response_id),
                        tool_call_id: None,
                    });
                }
            }
            RunEvent::ProviderPhase { event, .. } => {
                self.progress.current_phase = RunProgressPhase::Provider(event.phase);
                self.progress.active_step = format!(
                    "Provider request {} {} via {} (attempt {}, {} ms)",
                    event.request_id,
                    event.phase.as_str(),
                    event.endpoint,
                    event.attempt,
                    event.elapsed_ms
                );
                if let Some(failure) = &event.failure {
                    self.status_message = Some(failure.to_string());
                }
            }
            RunEvent::AssistantMessageCommitted {
                response_id, text, ..
            } => {
                if let Some(entry) = self.transcript_entries.iter_mut().rev().find(|entry| {
                    entry.kind == TranscriptKind::Assistant
                        && entry.response_id == Some(*response_id)
                }) {
                    entry.body.clone_from(text);
                } else {
                    self.transcript_entries.push(TranscriptEntry {
                        kind: TranscriptKind::Assistant,
                        title: "Assistant".to_string(),
                        body: text.clone(),
                        response_id: Some(*response_id),
                        tool_call_id: None,
                    });
                }
            }
            RunEvent::ReasoningSummaryDelta { response_id, delta } => {
                if let Some(entry) = self.transcript_entries.iter_mut().rev().find(|entry| {
                    entry.kind == TranscriptKind::ReasoningSummary
                        && entry.response_id == Some(*response_id)
                }) {
                    entry.body.push_str(delta);
                } else {
                    self.transcript_entries.push(TranscriptEntry {
                        kind: TranscriptKind::ReasoningSummary,
                        title: "Reasoning Summary".to_string(),
                        body: delta.clone(),
                        response_id: Some(*response_id),
                        tool_call_id: None,
                    });
                }
            }
            RunEvent::ToolCallPending {
                tool_call_id,
                tool_name,
                ..
            } => {
                let tool = crate::tool::ToolName::parse(tool_name);
                self.progress.tool_calls_started += 1;
                self.progress.current_phase = RunProgressPhase::Tool;
                self.progress.active_step = format!("Calling {tool_name}");
                self.tool_statuses.push(ToolStatusView {
                    tool_call_id: *tool_call_id,
                    tool,
                    title: tool_name.clone(),
                    status: ToolCallStatus::Pending,
                    summary: None,
                    error: None,
                });
                self.transcript_entries.push(TranscriptEntry {
                    kind: transcript_kind_for_tool_pending(tool),
                    title: pending_tool_transcript_title(tool).to_string(),
                    body: tool_name.clone(),
                    response_id: None,
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
                self.progress.current_phase = RunProgressPhase::Tool;
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
                    response_id: None,
                    tool_call_id: Some(*tool_call_id),
                });
            }
            RunEvent::ToolCallDeclined {
                tool_call_id,
                tool,
                reason,
                ..
            } => {
                self.progress.tool_calls_declined += 1;
                self.progress.current_phase = RunProgressPhase::Tool;
                self.progress.active_step = format!("Declined {tool}: {reason}");
                update_tool_status(
                    &mut self.tool_statuses,
                    *tool_call_id,
                    *tool,
                    &tool.to_string(),
                    ToolCallStatus::Declined,
                    Some(reason.clone()),
                    None,
                );
                self.transcript_entries.push(TranscriptEntry {
                    kind: TranscriptKind::System,
                    title: format!("Tool {tool} declined"),
                    body: reason.clone(),
                    response_id: None,
                    tool_call_id: Some(*tool_call_id),
                });
            }
            RunEvent::ToolCallCancelled {
                tool_call_id,
                tool,
                reason,
                ..
            } => {
                self.progress.tool_calls_cancelled += 1;
                self.progress.current_phase = RunProgressPhase::Tool;
                self.progress.active_step = format!("Cancelled {tool}: {reason}");
                update_tool_status(
                    &mut self.tool_statuses,
                    *tool_call_id,
                    *tool,
                    &tool.to_string(),
                    ToolCallStatus::Cancelled,
                    Some(reason.clone()),
                    None,
                );
                self.transcript_entries.push(TranscriptEntry {
                    kind: TranscriptKind::System,
                    title: format!("Tool {tool} cancelled"),
                    body: reason.clone(),
                    response_id: None,
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
                self.progress.current_phase = RunProgressPhase::Tool;
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
                    response_id: None,
                    tool_call_id: Some(*tool_call_id),
                });
            }
            RunEvent::FileChangesRecorded { changes, .. } => {
                let roots = self.file_change_display_roots();
                let body = summarize_changes(changes, roots);
                self.transcript_entries.push(TranscriptEntry {
                    kind: TranscriptKind::Diff,
                    title: format!("{}個のファイルが変更されました", changes.len()),
                    body,
                    response_id: None,
                    tool_call_id: None,
                });
            }
            RunEvent::CompactionCompleted {
                summarized_messages,
                ..
            } => {
                self.progress.compactions += 1;
                self.progress.current_phase = RunProgressPhase::Compaction;
                self.progress.active_step = format!("Compacted {summarized_messages} messages");
                self.transcript_entries.push(TranscriptEntry {
                    kind: TranscriptKind::System,
                    title: "Compaction".to_string(),
                    body: format!("summarized {summarized_messages} messages"),
                    response_id: None,
                    tool_call_id: None,
                });
            }
            RunEvent::RecoverableRuntimeFeedback { message, .. } => {
                self.run_status = RunStatus::Running;
                self.status_message = Some(message.clone());
                self.progress.current_phase = RunProgressPhase::RuntimeFeedback;
                self.progress.active_step = message.clone();
                self.transcript_entries.push(TranscriptEntry {
                    kind: TranscriptKind::Error,
                    title: "Runtime feedback".to_string(),
                    body: message.clone(),
                    response_id: None,
                    tool_call_id: None,
                });
            }
            RunEvent::TurnTerminal { terminal, .. } => {
                self.apply_durable_terminal(terminal);
                match &terminal.outcome {
                    TurnTerminalOutcome::Completed => {}
                    TurnTerminalOutcome::Interrupted { .. } => {
                        self.transcript_entries.push(TranscriptEntry {
                            kind: TranscriptKind::System,
                            title: "Run interrupted".to_string(),
                            body: terminal.summary().to_string(),
                            response_id: None,
                            tool_call_id: None,
                        });
                    }
                    TurnTerminalOutcome::Failed { error } => {
                        self.transcript_entries.push(TranscriptEntry {
                            kind: TranscriptKind::Error,
                            title: "Run failed".to_string(),
                            body: error.clone(),
                            response_id: None,
                            tool_call_id: None,
                        });
                    }
                }
            }
            RunEvent::ModelRequestPrepared { diagnostics, .. } => {
                self.progress.model_requests += 1;
                self.latest_context_window = diagnostics.context_window.clone();
                self.progress.current_phase = RunProgressPhase::Model;
                self.progress.active_step = format!(
                    "Model request {} with {} tools",
                    self.progress.model_requests, diagnostics.tool_count
                );
            }
            RunEvent::WorldStateUpdated { snapshot, .. } => {
                self.progress.current_phase = RunProgressPhase::Context;
                self.progress.active_step =
                    format!("World state updated: {} sections", snapshot.section_count());
            }
            RunEvent::PermissionRequested { summary, .. } => {
                self.progress.current_phase = RunProgressPhase::Permission;
                self.progress.active_step = summary.clone();
            }
            RunEvent::PermissionResolved { approved, .. } => {
                self.progress.current_phase = RunProgressPhase::Permission;
                self.progress.active_step = if *approved {
                    "permission approved".to_string()
                } else {
                    "permission not approved".to_string()
                };
            }
            RunEvent::UserTurnStored { .. } => {}
        }
    }

    pub fn set_permission(&mut self, request: &PermissionRequest) {
        self.permission = Some(PermissionOverlayView {
            summary: request.summary.clone(),
            details: request.details.clone(),
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
            agent_path: request.agent_path.clone(),
            agent_task_name: request.agent_task_name.clone(),
        });
    }

    pub fn clear_permission(&mut self) {
        self.permission = None;
    }

    pub fn apply_durable_user_turn(&mut self, turn: &crate::protocol::UserTurn) {
        self.route = Route::Session;
        self.transcript_entries.push(TranscriptEntry {
            kind: TranscriptKind::User,
            title: "User".to_string(),
            body: turn.text(),
            response_id: None,
            tool_call_id: None,
        });
        self.run_status = RunStatus::Running;
        self.progress.status = "Running".to_string();
        self.progress.current_phase = RunProgressPhase::User;
        self.progress.active_step = "User input stored".to_string();
    }

    pub fn apply_durable_prompt_dispatch(&mut self, prompt_dispatch: &PromptDispatchPart) {
        self.route = Route::Session;
        if should_render_prompt_dispatch_summary(prompt_dispatch) {
            self.transcript_entries.push(TranscriptEntry {
                kind: TranscriptKind::System,
                title: "Prompt Review".to_string(),
                body: prompt_dispatch_summary(prompt_dispatch),
                response_id: None,
                tool_call_id: None,
            });
        }
        self.transcript_entries.push(TranscriptEntry {
            kind: TranscriptKind::User,
            title: "User".to_string(),
            body: prompt_dispatch.dispatch_prompt_text.clone(),
            response_id: None,
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

    pub(crate) fn apply_terminal_outcome_projection(&mut self, outcome: &TurnTerminalOutcome) {
        self.interruption_cause = outcome.interruption_cause();
        self.permission = None;
        self.progress.current_phase = RunProgressPhase::Terminal;
        self.progress.active_step = outcome.summary().to_string();
        match outcome {
            TurnTerminalOutcome::Completed => {
                self.run_status = RunStatus::Completed;
                self.status_message = self.run_status.default_status_message();
                self.progress.status = "Completed".to_string();
            }
            TurnTerminalOutcome::Interrupted { cause } => {
                self.run_status = RunStatus::Cancelled;
                self.status_message = Some(interruption_status_message(*cause));
                self.progress.status = "Cancelled".to_string();
            }
            TurnTerminalOutcome::Failed { error } => {
                self.run_status = RunStatus::Failed;
                self.status_message = Some(error.clone());
                self.progress.status = "Failed".to_string();
            }
        }
    }

    fn apply_durable_terminal(&mut self, terminal: &DurableTurnTerminal) {
        self.progress.model_requests = terminal.metrics.model_request_count;
        self.progress.tool_calls_started = terminal.tool_call_count;
        self.progress.tool_calls_failed = terminal.failed_tool_count;
        self.apply_terminal_outcome_projection(&terminal.outcome);
    }

    pub fn apply_run_summary(&mut self, summary: RunSummary) {
        self.apply_durable_terminal(summary.terminal());
        self.last_summary = Some(summary);
    }
}

fn latest_interruption_cause(turn_items: &[TurnItem]) -> Option<TurnInterruptionCause> {
    turn_items_in_projection_order(turn_items)
        .into_iter()
        .rev()
        .find(|item| matches!(item.payload, TurnItemPayload::Terminal { .. }))
        .and_then(|item| match &item.payload {
            TurnItemPayload::Terminal {
                outcome: TurnTerminalOutcome::Interrupted { cause },
            } => Some(*cause),
            _ => None,
        })
}

pub(crate) fn interruption_status_message(cause: TurnInterruptionCause) -> String {
    match cause {
        TurnInterruptionCause::ApprovalAborted => {
            "操作を実行せず、タスクを停止しました。続けるには指示を入力してください。".to_string()
        }
        TurnInterruptionCause::UserStop => "run stopped by user".to_string(),
        TurnInterruptionCause::AgentInterrupted => "agent interrupted".to_string(),
        TurnInterruptionCause::TreeStopped => "agent tree stopped".to_string(),
    }
}

pub(crate) fn permission_decision_pending_status_message() -> String {
    "操作に対する決定を送信しました。処理結果を待っています。".to_string()
}

pub(crate) fn run_cancellation_status_message(cause: &RunCancellationCause) -> String {
    match cause {
        RunCancellationCause::Interruption(cause) => interruption_status_message(*cause),
        RunCancellationCause::Failure(message) => message.clone(),
        RunCancellationCause::Superseded => "run superseded by a newer owner".to_string(),
    }
}

fn loaded_summary_from_session(session: SessionRecord) -> LoadedSessionSummary {
    LoadedSessionSummary {
        session,
        loaded_status: LoadedSessionStatus::NotLoaded,
        archived: false,
        active_turn_id: None,
        active_turn_sequence_no: None,
        pending_permission_requests: 0,
        pending_user_input_requests: 0,
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
        ToolLifecycleStatus::Declined => "実行しなかったコマンド",
        ToolLifecycleStatus::Cancelled => "キャンセルしたコマンド",
        ToolLifecycleStatus::Failed => "コマンド失敗",
    }
}

fn progress_from_loaded_state(status: RunStatus, tools: &[ToolStatusView]) -> RunProgressView {
    RunProgressView {
        status: run_status_label_for_progress(status).to_string(),
        current_phase: RunProgressPhase::Loaded,
        active_step: "Loaded canonical turn items".to_string(),
        model_requests: 0,
        tool_calls_started: tools.len(),
        tool_calls_completed: tools
            .iter()
            .filter(|tool| tool.status == ToolCallStatus::Completed)
            .count(),
        tool_calls_declined: tools
            .iter()
            .filter(|tool| tool.status == ToolCallStatus::Declined)
            .count(),
        tool_calls_cancelled: tools
            .iter()
            .filter(|tool| tool.status == ToolCallStatus::Cancelled)
            .count(),
        tool_calls_failed: tools
            .iter()
            .filter(|tool| tool.status == ToolCallStatus::Failed)
            .count(),
        compactions: 0,
    }
}

pub fn latest_plan_from_turn_items(turn_items: &[TurnItem]) -> Option<PlanView> {
    turn_items_in_projection_order(turn_items)
        .into_iter()
        .rev()
        .find_map(|item| match &item.payload {
            TurnItemPayload::Plan { explanation, plan } => Some(PlanView {
                explanation: explanation.clone(),
                steps: plan.clone(),
            }),
            _ => None,
        })
}

fn run_status_label_for_progress(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Idle => "Idle",
        RunStatus::Running => "Running",
        RunStatus::Completed => "Completed",
        RunStatus::Cancelled => "Cancelled",
        RunStatus::Failed => "Failed",
    }
}

fn summarize_changes(changes: &[ChangeSummary], roots: Option<(&Utf8Path, &Utf8Path)>) -> String {
    changes
        .iter()
        .map(|value| match roots {
            Some((storage_root, display_root)) => {
                value.summary_line_relative_to(storage_root, display_root)
            }
            None => value.summary_line(None),
        })
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

pub fn transcript_entries_from_turn_items(turn_items: &[TurnItem]) -> Vec<TranscriptEntry> {
    transcript_entries_from_turn_items_with_roots(turn_items, None)
}

pub(crate) fn transcript_entries_from_turn_items_relative_to(
    turn_items: &[TurnItem],
    storage_root: &Utf8Path,
    display_root: &Utf8Path,
) -> Vec<TranscriptEntry> {
    transcript_entries_from_turn_items_with_roots(turn_items, Some((storage_root, display_root)))
}

fn transcript_entries_from_turn_items_with_roots(
    turn_items: &[TurnItem],
    roots: Option<(&Utf8Path, &Utf8Path)>,
) -> Vec<TranscriptEntry> {
    turn_items_in_projection_order(turn_items)
        .into_iter()
        .filter(|item| !item.payload.is_internal_projection_only())
        .filter_map(|item| match &item.payload {
            TurnItemPayload::UserMessage { text } => Some(TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User".to_string(),
                body: text.clone(),
                response_id: None,
                tool_call_id: None,
            }),
            TurnItemPayload::SteerMessage { text } => Some(TranscriptEntry {
                kind: TranscriptKind::User,
                title: "User Steer".to_string(),
                body: text.clone(),
                response_id: None,
                tool_call_id: None,
            }),
            TurnItemPayload::AgentMessage { text } => Some(TranscriptEntry {
                kind: TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: text.clone(),
                response_id: None,
                tool_call_id: None,
            }),
            TurnItemPayload::InterAgentCommunication { communication } => Some(TranscriptEntry {
                kind: TranscriptKind::System,
                title: format!("Sub Agent · {}", communication.author),
                body: communication.content.clone(),
                response_id: None,
                tool_call_id: None,
            }),
            TurnItemPayload::Plan { .. }
            | TurnItemPayload::SubAgentActivity { .. }
            | TurnItemPayload::WorldState { .. } => None,
            TurnItemPayload::ContextCompaction { summary } => Some(TranscriptEntry {
                kind: TranscriptKind::System,
                title: "Context Compaction".to_string(),
                body: format!("圧縮しました\n\n{}", summary.trim()),
                response_id: None,
                tool_call_id: None,
            }),
            TurnItemPayload::ToolStatus {
                call_id,
                tool,
                title,
                status,
                summary,
            } => {
                let failed = *status == ToolLifecycleStatus::Failed;
                Some(TranscriptEntry {
                    kind: if failed {
                        TranscriptKind::Error
                    } else if *status == ToolLifecycleStatus::Pending
                        || *status == ToolLifecycleStatus::Running
                    {
                        transcript_kind_for_tool_pending(*tool)
                    } else {
                        TranscriptKind::Tool
                    },
                    title: if failed {
                        format!("Tool {tool}")
                    } else {
                        tool_status_transcript_title(*tool, *status).to_string()
                    },
                    body: if failed {
                        summary.clone()
                    } else if summary.trim().is_empty() {
                        format!("{title} [{status:?}]")
                    } else {
                        format!("{title} [{status:?}]\n{}", summary.trim())
                    },
                    response_id: None,
                    tool_call_id: Some(*call_id),
                })
            }
            TurnItemPayload::FileChange {
                call_id,
                changes,
                summary,
                ..
            } => Some(TranscriptEntry {
                kind: TranscriptKind::Diff,
                title: format!("{}個のファイルが変更されました", changes.len()),
                body: if changes.is_empty() {
                    summary.clone()
                } else if let Some((storage_root, display_root)) = roots {
                    changes
                        .iter()
                        .map(|change| {
                            summary_line_from_stored_paths(
                                change.kind,
                                change.path_before.as_ref(),
                                change.path_after.as_ref(),
                                storage_root,
                                display_root,
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("; ")
                } else {
                    summary.clone()
                },
                response_id: None,
                tool_call_id: Some(*call_id),
            }),
            TurnItemPayload::ApprovalRequest { summary, .. } => Some(TranscriptEntry {
                kind: TranscriptKind::System,
                title: "Permission".to_string(),
                body: summary.clone(),
                response_id: None,
                tool_call_id: None,
            }),
            TurnItemPayload::Warning { message } => Some(TranscriptEntry {
                kind: TranscriptKind::System,
                title: "Warning".to_string(),
                body: message.clone(),
                response_id: None,
                tool_call_id: None,
            }),
            TurnItemPayload::Error { message } => Some(TranscriptEntry {
                kind: TranscriptKind::Error,
                title: "Error".to_string(),
                body: message.clone(),
                response_id: None,
                tool_call_id: None,
            }),
            TurnItemPayload::Terminal { outcome } => Some(TranscriptEntry {
                kind: terminal_transcript_kind(outcome),
                title: "Terminal".to_string(),
                body: match outcome {
                    TurnTerminalOutcome::Interrupted { cause } => {
                        interruption_status_message(*cause)
                    }
                    TurnTerminalOutcome::Completed | TurnTerminalOutcome::Failed { .. } => {
                        outcome.summary().to_string()
                    }
                },
                response_id: None,
                tool_call_id: None,
            }),
        })
        .collect()
}

pub fn tool_statuses_from_turn_items(turn_items: &[TurnItem]) -> Vec<ToolStatusView> {
    tool_statuses_from_turn_items_for_turn(turn_items, None)
}

fn tool_statuses_from_turn_items_for_turn(
    turn_items: &[TurnItem],
    selected_turn_id: Option<TurnId>,
) -> Vec<ToolStatusView> {
    let mut statuses = Vec::new();
    for item in turn_items_in_projection_order(turn_items) {
        if selected_turn_id.is_some_and(|turn_id| turn_id != item.turn_id) {
            continue;
        }
        if let TurnItemPayload::ToolStatus {
            call_id,
            tool,
            status,
            title,
            summary,
        } = &item.payload
        {
            let status = session_tool_status_from_lifecycle(*status);
            update_tool_status(
                &mut statuses,
                *call_id,
                *tool,
                title,
                status,
                (status == ToolCallStatus::Completed).then_some(if summary.trim().is_empty() {
                    title.clone()
                } else {
                    summary.clone()
                }),
                (status == ToolCallStatus::Failed).then_some(if summary.trim().is_empty() {
                    title.clone()
                } else {
                    summary.clone()
                }),
            );
        }
    }
    statuses
}

pub fn tui_primary_transcript_omits_internal_projection_items_fixture_passes() -> bool {
    let turn_id = crate::protocol::TurnId::new();
    let session_id = SessionId::new();
    let items = vec![
        TurnItem {
            id: crate::protocol::TurnItemId::new(),
            session_id,
            turn_id,
            source_item_id: None,
            sequence_no: 1,
            payload: TurnItemPayload::UserMessage {
                text: "build the artifact".to_string(),
            },
        },
        TurnItem {
            id: crate::protocol::TurnItemId::new(),
            session_id,
            turn_id,
            source_item_id: None,
            sequence_no: 2,
            payload: TurnItemPayload::Plan {
                explanation: Some("internal plan cache".to_string()),
                plan: Vec::new(),
            },
        },
        TurnItem {
            id: crate::protocol::TurnItemId::new(),
            session_id,
            turn_id,
            source_item_id: None,
            sequence_no: 3,
            payload: TurnItemPayload::AgentMessage {
                text: "done".to_string(),
            },
        },
    ];
    let entries = transcript_entries_from_turn_items(&items);
    let rendered = entries
        .iter()
        .map(|entry| format!("{}:{}", entry.title, entry.body))
        .collect::<Vec<_>>()
        .join("\n");

    entries.len() == 2
        && matches!(entries.first(), Some(entry) if entry.kind == TranscriptKind::User)
        && matches!(entries.last(), Some(entry) if entry.kind == TranscriptKind::Assistant)
        && !rendered.contains("internal plan cache")
        && !rendered.contains("prompt dispatch cache")
}

pub fn tui_session_search_state_is_explicit_discovery_metadata_fixture_passes() -> bool {
    let mut state = AppState::default();
    state.push_session_search_char('n');
    state.push_session_search_char('e');
    state.push_session_search_char('e');
    state.push_session_search_char('d');
    if state.session_search_text != "need" || state.session_search_include_archived {
        return false;
    }
    state.toggle_session_search_include_archived();
    state.pop_session_search_char();
    if state.session_search_text != "nee" || !state.session_search_include_archived {
        return false;
    }
    state.clear_session_search();
    state.session_search_text.is_empty() && !state.session_search_include_archived
}

fn session_tool_status_from_lifecycle(status: ToolLifecycleStatus) -> ToolCallStatus {
    match status {
        ToolLifecycleStatus::Pending => ToolCallStatus::Pending,
        ToolLifecycleStatus::Running => ToolCallStatus::Running,
        ToolLifecycleStatus::Completed => ToolCallStatus::Completed,
        ToolLifecycleStatus::Declined => ToolCallStatus::Declined,
        ToolLifecycleStatus::Cancelled => ToolCallStatus::Cancelled,
        ToolLifecycleStatus::Failed => ToolCallStatus::Failed,
    }
}

fn terminal_transcript_kind(outcome: &TurnTerminalOutcome) -> TranscriptKind {
    match outcome {
        TurnTerminalOutcome::Failed { .. } => TranscriptKind::Error,
        TurnTerminalOutcome::Completed | TurnTerminalOutcome::Interrupted { .. } => {
            TranscriptKind::System
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    fn test_session(id: SessionId) -> SessionRecord {
        SessionRecord {
            id,
            project_id: crate::session::ProjectId::new(),
            title: "test".to_string(),
            status: SessionStatus::Completed,
            cwd: Utf8PathBuf::from("C:/workspace"),
            model: "model".to_string(),
            base_url: "http://local".to_string(),
            access_mode: crate::config::AccessMode::FullAccess,
            model_parameters: crate::session::SessionModelParameters::default(),
            created_at_ms: 1,
            updated_at_ms: 2,
            completed_at_ms: Some(2),
        }
    }

    fn canonical_read(
        session: &SessionRecord,
        turn_items: Vec<TurnItem>,
        pending_turn_inputs: Vec<PendingTurnInputProjection>,
        active_turn_id: Option<TurnId>,
    ) -> CanonicalSessionRead {
        CanonicalSessionRead {
            session: session.clone(),
            history: crate::session::CanonicalHistoryPage {
                session: session.clone(),
                offset: 0,
                limit: 0,
                total: 0,
                has_more: false,
                items: Vec::new(),
            },
            turns: crate::session::CanonicalTurnPage {
                session: session.clone(),
                offset: 0,
                limit: 50,
                total: turn_items.len(),
                has_more: false,
                items: turn_items,
            },
            pending_turn_inputs,
            turn_elapsed_ms: Default::default(),
            latest_turn_id: active_turn_id,
            active_turn_id,
            active_turn_sequence_no: active_turn_id.map(|_| 1),
        }
    }

    #[test]
    fn nested_authority_projects_canonical_and_live_change_paths_without_mutating_storage() {
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let tool_call_id = ToolCallId::new();
        let change_id = crate::session::ChangeId::new();
        let storage_root = Utf8PathBuf::from("C:/repo/aaa");
        let display_root = storage_root.join("bbb");
        let stored_path = Utf8PathBuf::from("bbb/ccc/file.rs");
        let turn_item = TurnItem {
            id: crate::protocol::TurnItemId::new(),
            session_id,
            turn_id,
            source_item_id: None,
            sequence_no: 1,
            payload: TurnItemPayload::FileChange {
                call_id: tool_call_id,
                change_ids: vec![change_id],
                changes: vec![crate::protocol::FileChangeEvidence {
                    change_id,
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(stored_path.clone()),
                    path_after: Some(stored_path.clone()),
                    summary: "Updated bbb/ccc/file.rs".to_string(),
                }],
                summary: "Updated bbb/ccc/file.rs".to_string(),
            },
        };

        assert_eq!(
            transcript_entries_from_turn_items(std::slice::from_ref(&turn_item))[0].body,
            "Updated bbb/ccc/file.rs",
            "unit contexts without roots keep the canonical stored summary"
        );

        let mut state = AppState::default();
        state.set_file_change_display_roots(&storage_root, &display_root);
        state.load_turn_items(&test_session(session_id), std::slice::from_ref(&turn_item));
        assert_eq!(state.transcript_entries[0].body, "Updated ccc/file.rs");

        let live_change = ChangeSummary {
            change_id,
            kind: crate::session::ChangeKind::Update,
            path_before: Some(stored_path.clone()),
            path_after: Some(stored_path.clone()),
        };
        state.apply_run_event(&RunEvent::FileChangesRecorded {
            tool_call_id,
            changes: vec![live_change],
        });
        assert_eq!(
            state.transcript_entries.last().expect("live change").body,
            "Updated ccc/file.rs"
        );
        assert_eq!(
            stored_path,
            Utf8PathBuf::from("bbb/ccc/file.rs"),
            "project-root-relative stored coordinates remain unchanged"
        );
    }

    #[test]
    fn tui_pending_steer_is_separate_and_delivery_uses_canonical_projection_once() {
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let input_id = crate::protocol::HistoryItemId::new();
        let mut session = test_session(session_id);
        session.status = SessionStatus::Running;
        session.completed_at_ms = None;
        let queued = canonical_read(
            &session,
            Vec::new(),
            vec![PendingTurnInputProjection {
                id: input_id,
                turn_id,
                text: "same text".to_string(),
                image_count: 0,
                accepted_at_ms: 3,
                client_user_message_id: None,
            }],
            Some(turn_id),
        );
        let mut state = AppState::default();

        state.load_canonical_session_read(&queued);
        assert!(state.transcript_entries.is_empty());
        assert_eq!(state.pending_turn_inputs[0].id, input_id);

        let delivered = canonical_read(
            &session,
            vec![TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id,
                turn_id,
                source_item_id: Some(input_id),
                sequence_no: 1,
                payload: TurnItemPayload::SteerMessage {
                    text: "same text".to_string(),
                },
            }],
            Vec::new(),
            Some(turn_id),
        );
        assert!(state.refresh_canonical_conversation(&delivered));
        assert!(state.pending_turn_inputs.is_empty());
        assert_eq!(
            state
                .transcript_entries
                .iter()
                .filter(|entry| entry.body == "same text")
                .count(),
            1
        );
    }

    #[test]
    fn tui_primary_transcript_omits_internal_projection_items() {
        assert!(super::tui_primary_transcript_omits_internal_projection_items_fixture_passes());
    }

    #[test]
    fn tui_session_search_state_is_explicit_discovery_metadata() {
        assert!(super::tui_session_search_state_is_explicit_discovery_metadata_fixture_passes());
    }

    #[test]
    fn permission_overlay_does_not_replace_the_root_run_lifecycle() {
        let session_id = SessionId::new();
        let request = PermissionRequest {
            access: crate::workspace::AccessKind::Shell,
            summary: "child permission".to_string(),
            details: Vec::new(),
            targets: Vec::new(),
            outside_workspace: false,
            risks: Vec::new(),
            agent_path: Some("/root/child".to_string()),
            agent_task_name: Some("child".to_string()),
        };
        let mut state = AppState {
            current_session_id: Some(session_id),
            run_status: RunStatus::Completed,
            ..AppState::default()
        };

        state.set_permission(&request);
        assert_eq!(state.run_status, RunStatus::Completed);
        state.clear_permission();
        assert_eq!(state.run_status, RunStatus::Completed);

        state.run_status = RunStatus::Running;
        state.set_permission(&request);
        assert_eq!(state.run_status, RunStatus::Running);
        state.clear_permission();
        assert_eq!(state.run_status, RunStatus::Running);
    }

    #[test]
    fn durable_summary_is_the_terminal_projection_owner_without_duplicate_transcript_rows() {
        let session_id = SessionId::new();
        let terminal = DurableTurnTerminal {
            outcome: TurnTerminalOutcome::Interrupted {
                cause: TurnInterruptionCause::UserStop,
            },
            final_response_id: None,
            tool_call_count: 7,
            failed_tool_count: 2,
            change_count: 3,
            metrics: crate::session::RunMetrics {
                model_request_count: 5,
                ..Default::default()
            },
        };
        let mut state = AppState {
            current_session_id: Some(session_id),
            run_status: RunStatus::Running,
            ..AppState::default()
        };

        state.apply_run_event(&RunEvent::TurnTerminal {
            session_id,
            terminal: Box::new(terminal.clone()),
        });
        let terminal_rows = state.transcript_entries.len();
        state.apply_run_summary(RunSummary::from_terminal(
            session_id,
            TurnId::new(),
            terminal,
        ));

        assert_eq!(state.run_status, RunStatus::Cancelled);
        assert_eq!(
            state.interruption_cause,
            Some(TurnInterruptionCause::UserStop)
        );
        assert_eq!(state.status_message.as_deref(), Some("run stopped by user"));
        assert_eq!(state.progress.status, "Cancelled");
        assert_eq!(state.progress.model_requests, 5);
        assert_eq!(state.progress.tool_calls_started, 7);
        assert_eq!(state.progress.tool_calls_failed, 2);
        assert_eq!(state.transcript_entries.len(), terminal_rows);
        assert!(state.last_summary.is_some());
    }

    #[test]
    fn pending_tool_projection_derives_typed_name_without_rewriting_raw_name() {
        let tool_call_id = ToolCallId::new();
        let mut state = AppState::default();

        state.apply_run_event(&RunEvent::ToolCallPending {
            tool_call_id,
            response_id: ModelResponseId::new(),
            model_call_id: "provider-call-1".to_string(),
            tool_name: "vendor.custom_tool".to_string(),
            arguments_json: r#"{"raw":"provider text"}"#.to_string(),
        });

        assert_eq!(state.tool_statuses.len(), 1);
        assert_eq!(state.tool_statuses[0].tool, ToolName::Invalid);
        assert_eq!(state.tool_statuses[0].title, "vendor.custom_tool");
        let transcript = state
            .transcript_entries
            .last()
            .expect("pending tool transcript entry");
        assert_eq!(transcript.body, "vendor.custom_tool");
        assert_eq!(transcript.tool_call_id, Some(tool_call_id));
    }

    #[test]
    fn failed_tool_reload_projection_matches_the_runtime_error_entry() {
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let tool_call_id = ToolCallId::new();
        let error = "tool edit error: no edit baseline exists".to_string();
        let mut runtime = AppState::default();

        runtime.apply_run_event(&RunEvent::ToolCallFailed {
            tool_call_id,
            tool: ToolName::Write,
            error: error.clone(),
            metadata: serde_json::Value::Null,
        });
        let loaded = transcript_entries_from_turn_items(&[TurnItem {
            id: crate::protocol::TurnItemId::new(),
            session_id,
            turn_id,
            source_item_id: None,
            sequence_no: 1,
            payload: TurnItemPayload::ToolStatus {
                call_id: tool_call_id,
                tool: ToolName::Write,
                status: ToolLifecycleStatus::Failed,
                title: "Tool failed".to_string(),
                summary: error,
            },
        }]);

        assert_eq!(loaded, vec![runtime.transcript_entries[0].clone()]);
        assert_eq!(loaded[0].kind, TranscriptKind::Error);
        assert_eq!(loaded[0].title, "Tool write");
    }

    #[test]
    fn provider_phase_projects_request_identity_endpoint_and_in_flight_phase() {
        let response_id = ModelResponseId::new();
        let request_id = crate::llm::ProviderRequestId::new();
        let mut state = AppState::default();

        state.apply_run_event(&RunEvent::ProviderPhase {
            response_id,
            event: crate::llm::ProviderPhaseEvent {
                request_id: request_id.clone(),
                endpoint: "http://external-host:1234".to_string(),
                phase: crate::llm::ProviderPhase::RequestInFlight,
                attempt: 1,
                elapsed_ms: 604,
                terminal_status: None,
                usage: None,
                failure: None,
            },
        });

        assert_eq!(
            state.progress.current_phase,
            RunProgressPhase::Provider(crate::llm::ProviderPhase::RequestInFlight)
        );
        assert!(state.progress.active_step.contains(request_id.as_str()));
        assert!(
            state
                .progress
                .active_step
                .contains("http://external-host:1234")
        );
        assert!(state.progress.active_step.contains("604 ms"));
    }

    #[test]
    fn canonical_plan_item_owns_loaded_plan_projection() {
        let session_id = SessionId::new();
        let items = vec![TurnItem {
            id: crate::protocol::TurnItemId::new(),
            session_id,
            turn_id: crate::protocol::TurnId::new(),
            source_item_id: None,
            sequence_no: 1,
            payload: TurnItemPayload::Plan {
                explanation: Some("Inspect before editing".to_string()),
                plan: vec![PlanStep {
                    step: "Read the state owner".to_string(),
                    status: crate::protocol::PlanStepStatus::InProgress,
                }],
            },
        }];
        let mut state = AppState::default();

        state.load_turn_items(&test_session(session_id), &items);

        let plan = state.current_plan.expect("canonical plan projection");
        assert_eq!(plan.explanation.as_deref(), Some("Inspect before editing"));
        assert_eq!(
            plan.steps,
            vec![PlanStep {
                step: "Read the state owner".to_string(),
                status: crate::protocol::PlanStepStatus::InProgress,
            }]
        );
        assert_eq!(state.progress.current_phase, RunProgressPhase::Loaded);
    }

    #[test]
    fn running_canonical_load_scopes_tools_and_summary_to_the_active_turn() {
        let session_id = SessionId::new();
        let old_turn = TurnId::new();
        let active_turn = TurnId::new();
        let tool_item = |turn_id, sequence_no, title: &str| TurnItem {
            id: crate::protocol::TurnItemId::new(),
            session_id,
            turn_id,
            source_item_id: None,
            sequence_no,
            payload: TurnItemPayload::ToolStatus {
                call_id: ToolCallId::new(),
                tool: ToolName::Shell,
                status: ToolLifecycleStatus::Completed,
                title: title.to_string(),
                summary: format!("{title} done"),
            },
        };
        let items = vec![
            tool_item(old_turn, 1, "old turn tool"),
            tool_item(active_turn, 2, "active turn tool"),
        ];
        let mut session = test_session(session_id);
        session.status = SessionStatus::Running;
        session.completed_at_ms = None;
        let mut state = AppState {
            current_session_id: Some(session_id),
            last_summary: Some(RunSummary::from_terminal(
                session_id,
                old_turn,
                crate::session::DurableTurnTerminal {
                    outcome: TurnTerminalOutcome::Completed,
                    final_response_id: None,
                    tool_call_count: 1,
                    failed_tool_count: 0,
                    change_count: 0,
                    metrics: Default::default(),
                },
            )),
            ..AppState::default()
        };

        state.load_turn_items_with_active_turn(&session, &items, Some(active_turn));

        assert_eq!(state.tool_statuses.len(), 1);
        assert_eq!(state.tool_statuses[0].title, "active turn tool");
        assert!(state.last_summary.is_none());
    }

    #[test]
    fn session_started_clears_previous_turn_tools_and_summary() {
        let session_id = SessionId::new();
        let old_turn = TurnId::new();
        let mut state = AppState::default();
        state.apply_run_event(&RunEvent::ToolCallPending {
            tool_call_id: ToolCallId::new(),
            response_id: ModelResponseId::new(),
            model_call_id: "old-call".to_string(),
            tool_name: "shell".to_string(),
            arguments_json: "{}".to_string(),
        });
        state.last_summary = Some(RunSummary::from_terminal(
            session_id,
            old_turn,
            crate::session::DurableTurnTerminal {
                outcome: TurnTerminalOutcome::Completed,
                final_response_id: None,
                tool_call_count: 1,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            },
        ));

        state.apply_run_event(&RunEvent::SessionStarted {
            session_id,
            title: "follow-up".to_string(),
        });

        assert!(state.tool_statuses.is_empty());
        assert!(state.last_summary.is_none());
    }

    #[test]
    fn latest_context_window_survives_same_session_reload() {
        let session_id = SessionId::new();
        let status = ContextWindowTokenStatus {
            source: crate::context::ActiveContextTokenSource::FullPreparedRequestEstimate,
            active_context_tokens: 2_100,
            full_context_window_limit: 131_072,
            configured_max_output_tokens: 8_192,
            overflow_margin_tokens: 1_024,
            tokens_until_limit: 119_756,
            token_limit_reached: false,
        };
        let mut state = AppState {
            current_session_id: Some(session_id),
            latest_context_window: Some(status.clone()),
            ..AppState::default()
        };

        state.load_turn_items(&test_session(session_id), &[]);

        assert_eq!(state.latest_context_window, Some(status));
    }

    #[test]
    fn latest_context_window_clears_on_different_session_reload() {
        let previous_session_id = SessionId::new();
        let next_session_id = SessionId::new();
        let mut state = AppState {
            current_session_id: Some(previous_session_id),
            latest_context_window: Some(ContextWindowTokenStatus {
                source: crate::context::ActiveContextTokenSource::FullPreparedRequestEstimate,
                active_context_tokens: 2_100,
                full_context_window_limit: 131_072,
                configured_max_output_tokens: 8_192,
                overflow_margin_tokens: 1_024,
                tokens_until_limit: 119_756,
                token_limit_reached: false,
            }),
            ..AppState::default()
        };

        state.load_turn_items(&test_session(next_session_id), &[]);

        assert_eq!(state.latest_context_window, None);
    }
}

use std::fs;
use std::io::{self, Stdout};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use tokio_util::sync::CancellationToken;
use tui_textarea::TextArea;
use unicode_width::UnicodeWidthChar;

use crate::app::{
    App, AppBootstrap, AppCommand, AppCommandOutcome, ReviewRequest, RunConfigInput, RunRequest,
    SessionSteerRequest,
};
use crate::cli::{
    ConfirmationOutcome, ConfirmationPrompt, EventRenderer, OutputMode, ReviewDecision,
    SharedConfirmationPrompt, TuiArgs,
};
use crate::config::{ConfigLoader, ResolvedConfig, ShellFamily};
use crate::error::{AppRunError, CliPromptError, CliRenderError};
use crate::protocol::{PlanStepStatus, ToolApprovalDecision, TurnInterruptionCause};
use crate::runtime::{RunCancelOutcome, RunCancellationCause, RunControl, SystemClock};
use crate::session::markdown::{
    canonical_markdown_export_read, canonical_session_read_to_markdown, history_markdown_file_name,
};
use crate::session::{
    EditorContext, LoadedSessionStatus, LoadedSessionSummary, PromptDispatchPart, RunEvent,
    RunSummary, SessionId, SessionRecord, SessionStatus,
};
use crate::tool::PermissionRequest;
use crate::workspace::project::normalize_path;

use super::config_editor::{ConfigEditorState, ConfigSaveScope};
use super::prompt_enhance::enhance_prompt;
use super::query::{latest_session, recent_sessions, search_sessions, session_view};
use super::reducer::reduce_run_event;
use super::state::{
    AppState, Modal, PlanView, PromptReviewPhase, Route, RunStatus, TranscriptEntry,
    TranscriptKind, interruption_status_message, latest_plan_from_turn_items,
    permission_decision_pending_status_message, run_cancellation_status_message,
};

type TerminalHandle = Terminal<CrosstermBackend<Stdout>>;
const TUI_RUNTIME_DRAIN_BUDGET: usize = 128;
const TUI_RUNTIME_MAILBOX_CAPACITY: usize = 256;

struct TuiRootRun {
    generation: u64,
    run_control: RunControl,
}

#[derive(Default)]
struct TuiRootRunLifecycle {
    active: Option<TuiRootRun>,
}

impl TuiRootRunLifecycle {
    fn begin(&mut self, generation: u64, run_control: RunControl) -> bool {
        if self.active.is_some() {
            return false;
        }
        self.active = Some(TuiRootRun {
            generation,
            run_control,
        });
        true
    }

    fn is_active(&self) -> bool {
        self.active.is_some()
    }

    fn request_cancel(&self) -> bool {
        let Some(active) = self.active.as_ref() else {
            return false;
        };
        match active
            .run_control
            .request_cancel(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop,
            )) {
            RunCancelOutcome::Applied | RunCancelOutcome::Deferred(_) => true,
            RunCancelOutcome::Rejected => false,
        }
    }

    fn finish(&mut self, generation: u64) -> Option<Option<RunCancellationCause>> {
        if self
            .active
            .as_ref()
            .is_none_or(|active| active.generation != generation)
        {
            return None;
        }
        self.active.take().map(|active| active.run_control.cause())
    }
}

fn commit_tui_effective_config(
    effective_config: &mut ResolvedConfig,
    candidate: ResolvedConfig,
    durable_access_ready: bool,
) -> bool {
    if !durable_access_ready {
        return false;
    }
    *effective_config = candidate;
    true
}

pub async fn run(app: App, args: TuiArgs) -> Result<(), AppRunError> {
    let mut terminal = setup_terminal().map_err(|error| AppRunError::Message(error.to_string()))?;
    let result = async {
        let mut controller = TuiController::new(app, args).await?;
        loop {
            controller.drain_runtime_messages().await?;
            terminal
                .draw(|frame| controller.render(frame))
                .map_err(|error| AppRunError::Message(error.to_string()))?;
            if controller.should_quit {
                break;
            }
            if event::poll(Duration::from_millis(50))
                .map_err(|error| AppRunError::Message(error.to_string()))?
            {
                if let Event::Key(key) =
                    event::read().map_err(|error| AppRunError::Message(error.to_string()))?
                {
                    controller.handle_key(key).await?;
                }
            }
        }
        Ok(())
    }
    .await;
    restore_terminal(&mut terminal).map_err(|error| AppRunError::Message(error.to_string()))?;
    result
}

struct PendingPermission {
    confirmation_id: u64,
    request: PermissionRequest,
    responder: mpsc::Sender<ReviewDecision>,
    run_control: RunControl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingComposerSubmissionId {
    RootRun(u64),
    Steer(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingComposerSubmission {
    id: PendingComposerSubmissionId,
    session_id: Option<SessionId>,
    draft_revision: u64,
    draft_text: String,
}

fn has_pending_steer_submission(submissions: &[PendingComposerSubmission]) -> bool {
    submissions
        .iter()
        .any(|pending| matches!(pending.id, PendingComposerSubmissionId::Steer(_)))
}

struct TuiController {
    app: App,
    state: AppState,
    composer: TextArea<'static>,
    review_editor: TextArea<'static>,
    workspace_picker: TextArea<'static>,
    config_editor: ConfigEditorState,
    base_config: ResolvedConfig,
    effective_config: ResolvedConfig,
    root_run_lifecycle: TuiRootRunLifecycle,
    next_root_run_generation: u64,
    next_steer_submission_id: u64,
    composer_draft_revision: u64,
    pending_composer_submissions: Vec<PendingComposerSubmission>,
    runtime_tx: mpsc::SyncSender<RuntimeMessage>,
    runtime_rx: mpsc::Receiver<RuntimeMessage>,
    pending_permission: Option<PendingPermission>,
    next_permission_request_id: Arc<AtomicU64>,
    preview_entries: Vec<TranscriptEntry>,
    preview_plan: Option<PlanView>,
    preview_turn_offset: usize,
    preview_turn_limit: usize,
    preview_turn_total: usize,
    preview_turn_has_more: bool,
    next_enhance_request_id: u64,
    pending_prompt_enhance: Option<(u64, CancellationToken)>,
    should_quit: bool,
    started_at: Instant,
}

impl TuiController {
    async fn new(app: App, args: TuiArgs) -> Result<Self, AppRunError> {
        let (runtime_tx, runtime_rx) = mpsc::sync_channel(TUI_RUNTIME_MAILBOX_CAPACITY);
        let sessions = recent_sessions(&app.session_service, app.workspace.project_id, 20).await?;
        let base_config = app.config.clone();
        let effective_config = base_config.clone();
        let mut controller = Self {
            app,
            state: AppState::default(),
            composer: build_composer(),
            review_editor: build_composer(),
            workspace_picker: build_composer(),
            config_editor: ConfigEditorState::from_config(&effective_config),
            base_config,
            effective_config,
            root_run_lifecycle: TuiRootRunLifecycle::default(),
            next_root_run_generation: 1,
            next_steer_submission_id: 1,
            composer_draft_revision: 0,
            pending_composer_submissions: Vec::new(),
            runtime_tx,
            runtime_rx,
            pending_permission: None,
            next_permission_request_id: Arc::new(AtomicU64::new(1)),
            preview_entries: Vec::new(),
            preview_plan: None,
            preview_turn_offset: 0,
            preview_turn_limit: 80,
            preview_turn_total: 0,
            preview_turn_has_more: false,
            next_enhance_request_id: 1,
            pending_prompt_enhance: None,
            should_quit: false,
            started_at: Instant::now(),
        };
        let summaries = controller.loaded_summaries_for(sessions).await?;
        controller.state.set_loaded_sessions(summaries);
        controller.refresh_preview().await?;

        match (args.session_id, args.continue_last) {
            (Some(session_id), false) => controller.open_session(session_id).await?,
            (None, true) => {
                if let Some(session) = latest_session(
                    &controller.app.session_service,
                    controller.app.workspace.project_id,
                )
                .await?
                {
                    controller.open_session(session.id).await?;
                }
            }
            _ => {}
        }
        if args.directory.is_none() && args.session_id.is_none() && !args.continue_last {
            controller.open_workspace_picker();
        }
        Ok(controller)
    }

    async fn handle_key(&mut self, key: KeyEvent) -> Result<(), AppRunError> {
        if key.kind == KeyEventKind::Release {
            return Ok(());
        }
        if is_stop_key(key)
            && (matches!(self.state.run_status, RunStatus::Running) || self.agent_tree_active())
        {
            return self.stop_current_run().await;
        }
        if self.pending_permission.is_some() {
            return self.handle_permission_key(key);
        }
        match self.state.modal {
            Modal::ConfigEditor => self.handle_config_editor_key(key).await,
            Modal::EnhanceReview => self.handle_enhance_review_key(key).await,
            Modal::WorkspacePicker => self.handle_workspace_picker_key(key).await,
            Modal::None => self.handle_main_key(key).await,
        }
    }

    async fn handle_main_key(&mut self, key: KeyEvent) -> Result<(), AppRunError> {
        if key_leaves_current_task(key, self.state.route)
            && self.reject_agent_tree_navigation("the current task")
        {
            return Ok(());
        }
        match key.code {
            KeyCode::Char('q')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.state.run_status != RunStatus::Running =>
            {
                self.quit_after_cancelling_prompt_enhance();
            }
            KeyCode::F(2) => {
                self.state.route = Route::History;
                self.refresh_sessions().await?;
            }
            KeyCode::F(3) => {
                self.config_editor = ConfigEditorState::from_config(&self.effective_config);
                self.state.modal = Modal::ConfigEditor;
            }
            KeyCode::F(1) => {
                self.state.route = Route::Home;
            }
            KeyCode::F(4) if self.state.run_status != RunStatus::Running => {
                self.open_workspace_picker();
            }
            KeyCode::F(6) if self.state.run_status != RunStatus::Running => {
                if self.state.route != Route::History {
                    self.start_prompt_enhance().await?;
                }
            }
            KeyCode::F(7)
                if self.state.run_status != RunStatus::Running
                    && self.state.route != Route::History =>
            {
                self.start_uncommitted_review().await?;
            }
            KeyCode::F(8) if self.state.run_status != RunStatus::Running => {
                self.toggle_access_mode().await?;
            }
            KeyCode::F(9) if self.state.run_status != RunStatus::Running => {
                self.export_history_markdown().await?;
            }
            KeyCode::Up => {
                if self.state.route == Route::History && !self.state.sessions.is_empty() {
                    self.state.selected_session_index =
                        self.state.selected_session_index.saturating_sub(1);
                    self.reset_preview_turn_page();
                    self.refresh_preview().await?;
                }
            }
            KeyCode::Down => {
                if self.state.route == Route::History
                    && self.state.selected_session_index + 1 < self.state.sessions.len()
                {
                    self.state.selected_session_index += 1;
                    self.reset_preview_turn_page();
                    self.refresh_preview().await?;
                }
            }
            KeyCode::PageUp if self.state.route == Route::History => {
                self.previous_preview_turn_page().await?;
            }
            KeyCode::PageDown if self.state.route == Route::History => {
                self.next_preview_turn_page().await?;
            }
            KeyCode::Backspace if self.state.route == Route::History => {
                self.state.pop_session_search_char();
                self.refresh_sessions().await?;
            }
            KeyCode::Esc if self.state.route == Route::History => {
                self.state.clear_session_search();
                self.refresh_sessions().await?;
            }
            KeyCode::Char('i')
                if self.state.route == Route::History
                    && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.state.toggle_session_search_include_archived();
                self.refresh_sessions().await?;
            }
            KeyCode::Char('a')
                if self.state.route == Route::History
                    && self.state.run_status != RunStatus::Running =>
            {
                self.archive_selected_session(true).await?;
            }
            KeyCode::Char('u')
                if self.state.route == Route::History
                    && self.state.run_status != RunStatus::Running =>
            {
                self.archive_selected_session(false).await?;
            }
            KeyCode::Char('r')
                if self.state.route == Route::History
                    && self.state.run_status != RunStatus::Running =>
            {
                self.rejoin_selected_session().await?;
            }
            KeyCode::Char('z')
                if self.state.route == Route::History
                    && self.state.run_status != RunStatus::Running =>
            {
                self.rollback_selected_session().await?;
            }
            KeyCode::Enter
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && ctrl_enter_available(self.state.route, self.state.run_status) =>
            {
                if self.state.route == Route::History {
                    self.open_or_rejoin_selected_history_session().await?;
                } else {
                    self.submit_composer_or_open_session().await?;
                }
            }
            KeyCode::F(5) => {
                let root = self.app.workspace.root.clone();
                self.open_path_in_file_manager(&root);
            }
            KeyCode::Enter => {}
            KeyCode::Char(value) if self.state.route == Route::History => {
                self.state.push_session_search_char(value);
                self.refresh_sessions().await?;
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.state.route != Route::History {
                    let previous = textarea_value(&self.composer);
                    self.composer.insert_newline();
                    self.record_composer_edit(&previous);
                }
            }
            _ => {
                if self.state.route != Route::History {
                    let previous = textarea_value(&self.composer);
                    let _ = self.composer.input(key);
                    self.record_composer_edit(&previous);
                }
            }
        }
        Ok(())
    }

    async fn submit_composer_or_open_session(&mut self) -> Result<(), AppRunError> {
        let prompt = self.composer.lines().join("\n").trim().to_string();
        if !prompt.is_empty() {
            self.launch_run(prompt.clone(), PromptDispatchPart::raw(&prompt))
                .await?;
        } else if let Some(session_id) = self.state.selected_session().map(|session| session.id) {
            if self.reject_agent_tree_navigation("session") {
                return Ok(());
            }
            self.open_session(session_id).await?;
        }
        Ok(())
    }

    async fn handle_workspace_picker_key(&mut self, key: KeyEvent) -> Result<(), AppRunError> {
        match key.code {
            KeyCode::Esc => self.state.modal = Modal::None,
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.submit_workspace_picker().await?
            }
            KeyCode::F(5) => {
                if let Some(path) = self.resolve_workspace_picker_path() {
                    self.open_path_in_file_manager(&path);
                }
            }
            KeyCode::Enter => {}
            _ => {
                let _ = self.workspace_picker.input(key);
            }
        }
        Ok(())
    }

    async fn handle_enhance_review_key(&mut self, key: KeyEvent) -> Result<(), AppRunError> {
        if key.code == KeyCode::Char('q')
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && self.state.run_status != RunStatus::Running
        {
            self.quit_after_cancelling_prompt_enhance();
            return Ok(());
        }
        let Some(prompt_review) = self.state.prompt_review.clone() else {
            self.state.modal = Modal::None;
            return Ok(());
        };
        if prompt_review.phase == PromptReviewPhase::Enhancing {
            if key.code == KeyCode::Esc {
                self.cancel_pending_prompt_enhance();
                self.state.status_message = Some("cancelled prompt enhancement".to_string());
            }
            return Ok(());
        }

        match key.code {
            KeyCode::Esc => {
                self.cancel_pending_prompt_enhance();
                self.state.status_message = Some("kept raw prompt in composer".to_string());
            }
            KeyCode::F(6) => {
                let Some(prompt_dispatch) = self.state.build_prompt_dispatch(true) else {
                    return Err(AppRunError::Message(
                        "enhanced draft is not ready yet".to_string(),
                    ));
                };
                self.cancel_pending_prompt_enhance();
                self.launch_run(
                    prompt_dispatch.dispatch_prompt_text.clone(),
                    prompt_dispatch,
                )
                .await?;
            }
            KeyCode::F(7) => {
                let Some(prompt_dispatch) = self.state.build_prompt_dispatch(false) else {
                    return Err(AppRunError::Message(
                        "enhanced draft is not ready yet".to_string(),
                    ));
                };
                self.cancel_pending_prompt_enhance();
                self.launch_run(
                    prompt_dispatch.dispatch_prompt_text.clone(),
                    prompt_dispatch,
                )
                .await?;
            }
            KeyCode::Enter => {}
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.review_editor.insert_newline();
                self.state
                    .update_prompt_review_draft(textarea_value(&self.review_editor));
            }
            _ => {
                let _ = self.review_editor.input(key);
                self.state
                    .update_prompt_review_draft(textarea_value(&self.review_editor));
            }
        }
        Ok(())
    }

    async fn handle_config_editor_key(&mut self, key: KeyEvent) -> Result<(), AppRunError> {
        match key.code {
            KeyCode::Esc => self.state.modal = Modal::None,
            KeyCode::Up => self.config_editor.move_selection(-1),
            KeyCode::Down => self.config_editor.move_selection(1),
            KeyCode::Backspace => self.config_editor.backspace(),
            KeyCode::Delete => self.config_editor.clear_selected(),
            KeyCode::F(2) => {
                let candidate = self
                    .config_editor
                    .build_resolved_config(&self.effective_config)
                    .map_err(AppRunError::Message)?;
                let durable_access_ready = self
                    .persist_current_session_access_mode(candidate.permissions.access_mode)
                    .await;
                if !commit_tui_effective_config(
                    &mut self.effective_config,
                    candidate,
                    durable_access_ready,
                ) {
                    return Ok(());
                }
                self.state.status_message = Some(if self.state.current_session_id.is_some() {
                    "applied session override and remembered access mode for this session; changes apply to turns admitted later"
                        .to_string()
                } else {
                    "applied temporary session override; changes apply to turns admitted later"
                        .to_string()
                });
            }
            KeyCode::F(3) => {
                let message = self
                    .config_editor
                    .save_scope(&self.app.workspace.root, ConfigSaveScope::Global)
                    .map_err(AppRunError::Message)?;
                self.reload_config().await?;
                self.state.status_message = Some(message);
            }
            KeyCode::Char(value) => self.config_editor.insert_char(value),
            _ => {}
        }
        Ok(())
    }

    fn handle_permission_key(&mut self, key: KeyEvent) -> Result<(), AppRunError> {
        if let Some(decision) = permission_decision_for_key(key) {
            self.answer_permission(decision)?;
        }
        Ok(())
    }

    fn answer_permission(&mut self, decision: ReviewDecision) -> Result<(), AppRunError> {
        if let Some(cause) = self
            .pending_permission
            .as_ref()
            .and_then(|pending| pending.run_control.cause())
        {
            self.pending_permission = None;
            self.state.status_message = Some(run_cancellation_status_message(&cause));
            return Ok(());
        }
        let Some(pending) = self.pending_permission.take() else {
            return Err(AppRunError::Message(
                "permission request is no longer current".to_string(),
            ));
        };
        let response_failure = pending.responder.send(decision).err().map(|error| {
            let failure =
                RunCancellationCause::Failure(format!("TUI permission response failed: {error}"));
            pending.run_control.cancel(failure.clone());
            pending.run_control.cause().unwrap_or(failure)
        });
        if let Some(cause) = response_failure {
            self.state.status_message = Some(run_cancellation_status_message(&cause));
        } else {
            self.state.status_message = Some(permission_decision_pending_status_message());
        }
        Ok(())
    }

    async fn stop_current_run(&mut self) -> Result<(), AppRunError> {
        let root_stop_accepted = self.root_run_lifecycle.request_cancel();
        let Some(session_id) = self.state.current_session_id else {
            if root_stop_accepted {
                self.state.status_message =
                    Some("stop requested before run admission completed".to_string());
                return Ok(());
            }
            self.state.status_message = Some("no active session to stop".to_string());
            return Ok(());
        };
        let tree_cancelled = self
            .app
            .run_service
            .cancel_agent_tree(session_id, TurnInterruptionCause::UserStop);
        if root_stop_accepted {
            // Stop dispatch is not a terminal transition. The admitted turn remains the owner
            // until its matching TurnTerminal/worker settlement is projected back to the TUI.
            self.state.status_message = Some("stop requested for active run".to_string());
            return Ok(());
        }
        match self
            .app
            .session_service
            .interrupt_running_session(session_id)
            .await
        {
            Ok(session) => {
                self.state.run_status = tui_run_status_for_session_status(session.status);
                self.state.status_message = Some(if session.status == SessionStatus::Cancelled {
                    "stop requested for active run".to_string()
                } else {
                    "stopped the active agent tree; root result was preserved".to_string()
                });
                self.refresh_sessions().await?;
            }
            Err(error) => {
                if tree_cancelled {
                    self.state.run_status = RunStatus::Cancelled;
                    self.state.status_message = Some("stopped the active agent tree".to_string());
                } else {
                    self.state.status_message = Some(format!("failed to stop active run: {error}"));
                }
            }
        }
        Ok(())
    }

    fn agent_tree_active(&self) -> bool {
        self.root_run_lifecycle.is_active()
            || self.state.current_session_id.is_some_and(|session_id| {
                self.app
                    .run_service
                    .agent_activity_records(session_id)
                    .iter()
                    .any(|record| {
                        matches!(
                            record.status,
                            crate::runtime::AgentStatus::PendingInit
                                | crate::runtime::AgentStatus::Running
                        )
                    })
            })
    }

    fn reject_agent_tree_navigation(&mut self, target: &str) -> bool {
        if !self.agent_tree_active() {
            return false;
        }
        self.state.status_message = Some(format!(
            "{target} cannot change while the agent tree is active; press Ctrl+X to stop it first"
        ));
        true
    }

    async fn toggle_access_mode(&mut self) -> Result<(), AppRunError> {
        let access_mode = self.effective_config.permissions.access_mode.next();
        let session_access_owner = self.state.current_session_id.is_some();
        if session_access_owner {
            if !self.persist_current_session_access_mode(access_mode).await {
                return Ok(());
            }
        } else if let Err(error) = ConfigEditorState::remember_global_access_mode(access_mode) {
            self.state.status_message = Some(format!(
                "access mode was not changed because it could not be remembered: {error}"
            ));
            return Ok(());
        } else {
            self.app.config.permissions.access_mode = access_mode;
            self.base_config.permissions.access_mode = access_mode;
        }
        self.apply_access_mode_owner(access_mode);
        self.state.status_message = Some(if session_access_owner {
            format!(
                "session access mode set to {} and remembered for this session; it applies to turns admitted later",
                access_mode.label()
            )
        } else {
            format!(
                "default access mode set to {} and remembered globally; it applies to turns admitted later",
                access_mode.label()
            )
        });
        Ok(())
    }

    async fn persist_current_session_access_mode(
        &mut self,
        access_mode: crate::config::AccessMode,
    ) -> bool {
        let Some(session_id) = self.state.current_session_id else {
            return true;
        };
        match self
            .app
            .session_service
            .update_root_session_access_mode(session_id, access_mode)
            .await
        {
            Ok(_) => {
                for session in &mut self.state.sessions {
                    if session.id == session_id {
                        session.access_mode = access_mode;
                    }
                }
                for summary in &mut self.state.loaded_sessions {
                    if summary.session.id == session_id {
                        summary.session.access_mode = access_mode;
                    }
                }
                true
            }
            Err(error) => {
                self.state.status_message = Some(format!(
                    "access mode was not changed because session settings could not be saved: {error}"
                ));
                false
            }
        }
    }

    fn apply_access_mode_owner(&mut self, access_mode: crate::config::AccessMode) {
        self.effective_config.permissions.access_mode = access_mode;
        self.config_editor = ConfigEditorState::from_config(&self.effective_config);
    }

    fn restore_global_access_mode_owner(&mut self) {
        self.apply_access_mode_owner(self.base_config.permissions.access_mode);
    }

    fn open_workspace_picker(&mut self) {
        if self.reject_agent_tree_navigation("workspace") {
            return;
        }
        self.workspace_picker = build_composer();
        self.workspace_picker
            .insert_str(self.app.workspace.cwd.as_str());
        self.state.modal = Modal::WorkspacePicker;
    }

    async fn submit_workspace_picker(&mut self) -> Result<(), AppRunError> {
        if self.reject_agent_tree_navigation("workspace") {
            self.state.modal = Modal::None;
            return Ok(());
        }
        let Some(requested) = self.resolve_workspace_picker_path() else {
            return Ok(());
        };

        let store = self.app.session_service.store.clone();
        let app = match AppBootstrap::rebuild_for_directory(&requested, store).await {
            Ok(value) => value,
            Err(error) => {
                self.state.status_message =
                    Some(format!("failed to load workspace {}: {error}", requested));
                return Ok(());
            }
        };
        self.cancel_pending_prompt_enhance();
        self.app = app;
        self.base_config = self.app.config.clone();
        self.effective_config = self.base_config.clone();
        self.root_run_lifecycle = TuiRootRunLifecycle::default();
        self.config_editor = ConfigEditorState::from_config(&self.effective_config);
        self.state = AppState::default();
        self.composer = build_composer();
        self.composer_draft_revision = 0;
        self.pending_composer_submissions.clear();
        self.review_editor = build_composer();
        self.workspace_picker = build_composer();
        self.pending_permission = None;
        self.preview_entries.clear();
        self.preview_plan = None;
        self.refresh_sessions().await?;
        self.state.status_message = Some(format!("workspace set to {}", self.app.workspace.root));
        self.state.modal = Modal::None;
        Ok(())
    }

    fn resolve_workspace_picker_path(&mut self) -> Option<camino::Utf8PathBuf> {
        let requested = textarea_value(&self.workspace_picker).trim().to_string();
        if requested.is_empty() {
            self.state.status_message = Some("workspace path is empty".to_string());
            return None;
        }
        let requested_input = camino::Utf8PathBuf::from(requested);
        let requested = match normalize_path(&self.app.workspace.cwd, &requested_input) {
            Ok(value) => value,
            Err(error) => {
                self.state.status_message = Some(format!("invalid workspace path: {error}"));
                return None;
            }
        };
        let metadata = match std::fs::metadata(requested.as_std_path()) {
            Ok(value) => value,
            Err(error) => {
                self.state.status_message = Some(format!(
                    "workspace path is not accessible: {} ({error})",
                    requested
                ));
                return None;
            }
        };
        if !metadata.is_dir() {
            self.state.status_message =
                Some(format!("workspace path is not a directory: {}", requested));
            return None;
        }
        Some(requested)
    }

    fn open_path_in_file_manager(&mut self, path: &camino::Utf8Path) {
        match open_path_with_platform_file_manager(path) {
            Ok(()) => {
                self.state.status_message = Some(format!("opened {} in file manager", path));
            }
            Err(error) => {
                self.state.status_message =
                    Some(format!("failed to open {} in file manager: {error}", path));
            }
        }
    }

    async fn start_prompt_enhance(&mut self) -> Result<(), AppRunError> {
        let raw_prompt = textarea_value(&self.composer).trim().to_string();
        if raw_prompt.is_empty() {
            return Ok(());
        }
        if self.agent_tree_active() {
            self.state.status_message = Some(
                "wait for the active agent tree to finish, or press Ctrl+X to stop it".to_string(),
            );
            return Ok(());
        }
        if self.pending_prompt_enhance.is_some() || self.state.prompt_review.is_some() {
            self.state.status_message =
                Some("prompt enhancement is already in progress or under review".to_string());
            return Ok(());
        }
        let request_id = self.next_enhance_request_id;
        let Some(next_request_id) = request_id.checked_add(1) else {
            self.state.status_message =
                Some("prompt enhancement request generation is exhausted".to_string());
            return Ok(());
        };
        self.next_enhance_request_id = next_request_id;
        let cancellation = CancellationToken::new();
        self.pending_prompt_enhance = Some((request_id, cancellation.clone()));
        self.state.begin_prompt_enhance(request_id, &raw_prompt);
        self.review_editor = build_composer();
        let runtime_tx = self.runtime_tx.clone();
        let config = self.effective_config.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build prompt enhance runtime");
            let result = runtime.block_on(async move {
                enhance_prompt(&config, &raw_prompt, cancellation)
                    .await
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::EnhanceFinished { request_id, result });
        });
        Ok(())
    }

    async fn start_uncommitted_review(&mut self) -> Result<(), AppRunError> {
        let prompt = textarea_value(&self.composer).trim().to_string();
        self.launch_run_with_options(
            prompt.clone(),
            PromptDispatchPart::raw(&prompt),
            Some(ReviewRequest::Uncommitted),
        )
        .await
    }

    async fn launch_run(
        &mut self,
        prompt: String,
        prompt_dispatch: PromptDispatchPart,
    ) -> Result<(), AppRunError> {
        self.launch_run_with_options(prompt, prompt_dispatch, None)
            .await
    }

    async fn launch_run_with_options(
        &mut self,
        prompt: String,
        prompt_dispatch: PromptDispatchPart,
        review_request: Option<ReviewRequest>,
    ) -> Result<(), AppRunError> {
        if review_request.is_none()
            && !prompt.trim().is_empty()
            && self.state.current_session_id.is_some()
            && matches!(self.state.run_status, RunStatus::Running)
        {
            self.launch_active_turn_steer(prompt).await?;
            return Ok(());
        }
        if self.agent_tree_active() {
            self.state.status_message = Some(
                "wait for the active agent tree to finish, or press Ctrl+X to stop it".to_string(),
            );
            return Ok(());
        }
        let run_generation = self.next_root_run_generation;
        let Some(next_generation) = run_generation.checked_add(1) else {
            self.state.status_message =
                Some("TUI run generation is exhausted; restart moyAI".to_string());
            return Ok(());
        };
        let run_control = RunControl::new();
        if !self
            .root_run_lifecycle
            .begin(run_generation, run_control.clone())
        {
            self.state.status_message = Some(
                "wait for the previous run to finish stopping before starting another".to_string(),
            );
            return Ok(());
        }
        self.next_root_run_generation = next_generation;
        let request = RunRequest {
            prompt: prompt.clone(),
            session_id: self.state.current_session_id,
            continue_last: false,
            title: None,
            cwd: self.app.workspace.cwd.clone(),
            config: RunConfigInput::Resolved(self.effective_config.clone()),
            output_mode: OutputMode::Human,
            show_reasoning_summary: true,
            prompt_dispatch: Some(prompt_dispatch.clone()),
            editor_context: Some(self.current_editor_context()),
            review_request,
            image_paths: Vec::new(),
            run_control,
            agent_confirmation: None,
            agent_context: None,
        };
        self.track_pending_composer_submission(
            PendingComposerSubmissionId::RootRun(run_generation),
            request.session_id,
        );
        self.state.status_message = Some("submitting user input...".to_string());
        let run_service = self.app.run_service.clone();
        let runtime_tx = self.runtime_tx.clone();
        let next_permission_request_id = self.next_permission_request_id.clone();
        std::thread::spawn(move || {
            let mut request = request;
            let root_run_control = request.run_control.clone();
            let mut renderer = TuiRenderer {
                tx: runtime_tx.clone(),
                root_run_generation: Some(run_generation),
            };
            let mut prompt = SharedConfirmationPrompt::new_with_root_control(
                TuiConfirmationPrompt {
                    tx: runtime_tx.clone(),
                    next_permission_request_id,
                },
                root_run_control,
            );
            request.agent_confirmation = Some(prompt.clone());
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build tui worker runtime");
            runtime.block_on(async move {
                let result = run_service
                    .execute(AppCommand::Run(request), &mut renderer, &mut prompt)
                    .await
                    .and_then(|outcome| match outcome {
                        AppCommandOutcome::Turn(summary) => Ok(summary),
                        AppCommandOutcome::ControlCompleted => Err(AppRunError::Message(
                            "an admitted TUI run completed as a control command".to_string(),
                        )),
                    })
                    .map_err(|error| error.to_string());
                publish_tui_run_finished(&runtime_tx, run_generation, result);
            });
        });
        Ok(())
    }

    async fn launch_active_turn_steer(&mut self, prompt: String) -> Result<(), AppRunError> {
        let Some(session_id) = self.state.current_session_id else {
            self.state.status_message =
                Some("running session is not available for steer".to_string());
            return Ok(());
        };
        if has_pending_steer_submission(&self.pending_composer_submissions) {
            self.state.status_message =
                Some("wait for the previous steer input to finish storing".to_string());
            return Ok(());
        }
        let submission_id = self.next_steer_submission_id;
        let Some(next_submission_id) = submission_id.checked_add(1) else {
            self.state.status_message =
                Some("TUI steer submission identity is exhausted; restart moyAI".to_string());
            return Ok(());
        };
        self.next_steer_submission_id = next_submission_id;
        self.track_pending_composer_submission(
            PendingComposerSubmissionId::Steer(submission_id),
            Some(session_id),
        );
        self.state.status_message = Some("storing steer input...".to_string());
        let run_service = self.app.run_service.clone();
        let runtime_tx = self.runtime_tx.clone();
        let next_permission_request_id = self.next_permission_request_id.clone();
        let cwd = self.app.workspace.cwd.clone();
        let accepted_prompt = prompt.clone();
        std::thread::spawn(move || {
            let mut renderer = TuiRenderer {
                tx: runtime_tx.clone(),
                root_run_generation: None,
            };
            let mut prompt_ui = TuiConfirmationPrompt {
                tx: runtime_tx.clone(),
                next_permission_request_id,
            };
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build tui steer runtime");
            let result = runtime
                .block_on(async move {
                    run_service
                        .execute(
                            AppCommand::SessionSteer(SessionSteerRequest {
                                session_id,
                                prompt,
                                cwd,
                                image_paths: Vec::new(),
                                client_user_message_id: Some(format!("tui-steer-{submission_id}")),
                            }),
                            &mut renderer,
                            &mut prompt_ui,
                        )
                        .await
                })
                .map(|_| ())
                .map_err(|error| error.to_string());
            let _ = runtime_tx.send(RuntimeMessage::SteerStored {
                submission_id,
                session_id,
                prompt: accepted_prompt,
                result,
            });
        });
        Ok(())
    }

    fn current_editor_context(&self) -> EditorContext {
        let shell_family = self
            .effective_config
            .shell
            .family
            .unwrap_or(if cfg!(windows) {
                ShellFamily::PowerShell
            } else {
                ShellFamily::Bash
            });
        let visible_files = self
            .current_visible_files()
            .into_iter()
            .take(8)
            .collect::<Vec<_>>();
        EditorContext {
            active_file: visible_files.first().cloned(),
            open_tabs: visible_files.clone(),
            visible_files,
            shell_family,
            current_time_ms: SystemClock::now_ms(),
        }
    }

    fn current_visible_files(&self) -> Vec<camino::Utf8PathBuf> {
        Vec::new()
    }

    fn record_composer_edit(&mut self, previous_text: &str) {
        if textarea_value(&self.composer) != previous_text {
            self.advance_composer_draft_revision();
        }
    }

    fn advance_composer_draft_revision(&mut self) {
        if let Some(next) = self.composer_draft_revision.checked_add(1) {
            self.composer_draft_revision = next;
        } else {
            // Safety first: an exhausted identity must never let an old acceptance clear a new draft.
            self.pending_composer_submissions.clear();
            self.composer_draft_revision = 0;
        }
    }

    fn track_pending_composer_submission(
        &mut self,
        id: PendingComposerSubmissionId,
        session_id: Option<SessionId>,
    ) {
        debug_assert!(
            self.pending_composer_submissions
                .iter()
                .all(|pending| pending.id != id),
            "TUI composer submission identities must be unique"
        );
        self.pending_composer_submissions
            .push(PendingComposerSubmission {
                id,
                session_id,
                draft_revision: self.composer_draft_revision,
                draft_text: textarea_value(&self.composer),
            });
    }

    fn bind_pending_composer_submission_session(
        &mut self,
        id: PendingComposerSubmissionId,
        session_id: SessionId,
    ) {
        if let Some(pending) = self
            .pending_composer_submissions
            .iter_mut()
            .find(|pending| pending.id == id)
            && pending.session_id.is_none()
        {
            pending.session_id = Some(session_id);
        }
    }

    fn settle_pending_composer_submission(
        &mut self,
        id: PendingComposerSubmissionId,
        session_id: SessionId,
        accepted: bool,
    ) -> bool {
        let Some(index) = self
            .pending_composer_submissions
            .iter()
            .position(|pending| {
                pending.id == id
                    && pending
                        .session_id
                        .is_none_or(|expected| expected == session_id)
            })
        else {
            return false;
        };
        let pending = self.pending_composer_submissions.remove(index);
        let should_clear = accepted
            && pending.draft_revision == self.composer_draft_revision
            && pending.draft_text == textarea_value(&self.composer);
        if should_clear {
            self.composer = build_composer();
            self.advance_composer_draft_revision();
        }
        true
    }

    fn discard_pending_composer_submission(&mut self, id: PendingComposerSubmissionId) {
        self.pending_composer_submissions
            .retain(|pending| pending.id != id);
    }

    async fn drain_runtime_messages(&mut self) -> Result<(), AppRunError> {
        for _ in 0..TUI_RUNTIME_DRAIN_BUDGET {
            let Ok(message) = self.runtime_rx.try_recv() else {
                break;
            };
            match message {
                RuntimeMessage::RunEvent {
                    root_run_generation,
                    event,
                } => {
                    if let (Some(run_generation), RunEvent::SessionStarted { session_id, .. }) =
                        (root_run_generation, &event)
                    {
                        self.bind_pending_composer_submission_session(
                            PendingComposerSubmissionId::RootRun(run_generation),
                            *session_id,
                        );
                    }
                    let live_refresh_session_id =
                        event.session_id().or(self.state.current_session_id);
                    reduce_run_event(&mut self.state, &event);
                    if let RunEvent::UserTurnStored { session_id, turn } = &event {
                        self.state.apply_durable_user_turn(turn);
                        if let Some(run_generation) = root_run_generation {
                            self.settle_pending_composer_submission(
                                PendingComposerSubmissionId::RootRun(run_generation),
                                *session_id,
                                true,
                            );
                        }
                    }
                    if live_event_requires_canonical_refresh(&event) {
                        if let Some(session_id) = live_refresh_session_id {
                            self.refresh_loaded_summary_for_session(session_id).await?;
                            if event_requires_plan_refresh(&event)
                                && self.state.current_session_id == Some(session_id)
                            {
                                let snapshot = self
                                    .app
                                    .session_service
                                    .canonical_latest_session_snapshot(
                                        session_id,
                                        1,
                                        crate::protocol::MAX_PROTOCOL_PAGE_LIMIT,
                                    )
                                    .await?;
                                self.state
                                    .refresh_plan_from_turn_items(&snapshot.read.turns.items);
                            }
                            if self.state.route == Route::History
                                && self
                                    .state
                                    .selected_session()
                                    .is_some_and(|session| session.id == session_id)
                            {
                                self.refresh_preview().await?;
                            }
                        }
                    }
                }
                RuntimeMessage::Finished {
                    run_generation,
                    result,
                } => {
                    let Some(cancellation_cause) = self.root_run_lifecycle.finish(run_generation)
                    else {
                        continue;
                    };
                    self.discard_pending_composer_submission(PendingComposerSubmissionId::RootRun(
                        run_generation,
                    ));
                    match result {
                        Ok(summary) => {
                            self.settle_pending_permission_after_root_success();
                            self.state.set_summary(summary);
                            self.refresh_sessions().await?;
                            if let Some(session_id) = self.state.current_session_id {
                                self.open_session(session_id).await?;
                            }
                        }
                        Err(message) => {
                            self.pending_permission = None;
                            self.state.run_status =
                                tui_terminal_error_status(cancellation_cause.as_ref());
                            self.state.status_message = Some(match cancellation_cause {
                                Some(RunCancellationCause::Interruption(cause)) => {
                                    interruption_status_message(cause)
                                }
                                Some(RunCancellationCause::Failure(failure)) => failure,
                                Some(RunCancellationCause::Superseded) | None => message,
                            });
                        }
                    }
                }
                RuntimeMessage::Permission {
                    confirmation_id,
                    request,
                    response,
                    run_control,
                } => {
                    let next = PendingPermission {
                        confirmation_id,
                        request: request.clone(),
                        responder: response,
                        run_control,
                    };
                    if let Some(previous) = self.pending_permission.replace(next) {
                        previous.run_control.fail(format!(
                            "TUI replaced unresolved permission confirmation {} with {}",
                            previous.confirmation_id, confirmation_id
                        ));
                    }
                }
                RuntimeMessage::PermissionCancelled { confirmation_id } => {
                    clear_cancelled_tui_permission(&mut self.pending_permission, confirmation_id);
                }
                RuntimeMessage::SteerStored {
                    submission_id,
                    session_id,
                    prompt,
                    result,
                } => match result {
                    Ok(()) => {
                        if self.state.current_session_id == Some(session_id) {
                            self.state.apply_durable_steer_prompt(&prompt);
                        }
                        self.settle_pending_composer_submission(
                            PendingComposerSubmissionId::Steer(submission_id),
                            session_id,
                            true,
                        );
                        self.refresh_loaded_summary_for_session(session_id).await?;
                        self.state.status_message =
                            Some("stored steer input for the active turn".to_string());
                    }
                    Err(message) => {
                        self.discard_pending_composer_submission(
                            PendingComposerSubmissionId::Steer(submission_id),
                        );
                        self.state.status_message =
                            Some(format!("failed to store steer input: {message}"));
                    }
                },
                RuntimeMessage::EnhanceFinished { request_id, result } => {
                    if !self.finish_pending_prompt_enhance(request_id) {
                        continue;
                    }
                    match result {
                        Ok(draft) => {
                            if self.state.finish_prompt_enhance(request_id, draft.clone()) {
                                self.review_editor = build_composer();
                                self.review_editor.insert_str(&draft);
                                self.state.update_prompt_review_draft(textarea_value(
                                    &self.review_editor,
                                ));
                            }
                        }
                        Err(message) => {
                            if self
                                .state
                                .prompt_review
                                .as_ref()
                                .map(|review| review.request_id == request_id)
                                .unwrap_or(false)
                            {
                                self.state.cancel_prompt_review();
                                self.state.status_message =
                                    Some(format!("prompt enhancement failed: {message}"));
                            }
                        }
                    }
                }
            }
            self.discard_terminal_pending_permission();
        }
        Ok(())
    }

    fn discard_terminal_pending_permission(&mut self) {
        if self
            .pending_permission
            .as_ref()
            .is_some_and(|pending| pending.run_control.cause().is_some())
        {
            self.pending_permission = None;
        }
    }

    fn settle_pending_permission_after_root_success(&mut self) {
        if !self.pending_permission.as_ref().is_some_and(|pending| {
            pending.request.agent_path.is_some() && pending.run_control.cause().is_none()
        }) {
            self.pending_permission = None;
        }
    }

    async fn open_session(&mut self, session_id: SessionId) -> Result<(), AppRunError> {
        let read = session_view(&self.app.session_service, session_id).await?;
        self.cancel_pending_prompt_enhance();
        self.apply_access_mode_owner(read.session.access_mode);
        self.state.load_turn_items(&read.session, &read.turns.items);
        self.state.modal = Modal::None;
        Ok(())
    }

    fn finish_pending_prompt_enhance(&mut self, request_id: u64) -> bool {
        if self
            .pending_prompt_enhance
            .as_ref()
            .is_none_or(|(active_request_id, _)| *active_request_id != request_id)
        {
            return false;
        }
        self.pending_prompt_enhance = None;
        true
    }

    fn cancel_pending_prompt_enhance(&mut self) {
        if let Some((_, cancellation)) = self.pending_prompt_enhance.take() {
            cancellation.cancel();
        }
        self.state.cancel_prompt_review();
    }

    fn quit_after_cancelling_prompt_enhance(&mut self) {
        self.cancel_pending_prompt_enhance();
        self.should_quit = true;
    }

    async fn open_or_rejoin_selected_history_session(&mut self) -> Result<(), AppRunError> {
        if self.reject_agent_tree_navigation("session") {
            return Ok(());
        }
        let Some(session_id) = self.state.selected_session().map(|session| session.id) else {
            self.state.status_message = Some("select a session first".to_string());
            return Ok(());
        };
        if self
            .state
            .selected_loaded_session()
            .is_some_and(|summary| summary.loaded_status == LoadedSessionStatus::Active)
        {
            return self.rejoin_session(session_id).await;
        }
        self.open_session(session_id).await
    }

    async fn rejoin_selected_session(&mut self) -> Result<(), AppRunError> {
        if self.reject_agent_tree_navigation("session") {
            return Ok(());
        }
        let Some(session_id) = self.state.selected_session().map(|session| session.id) else {
            self.state.status_message = Some("select a session first".to_string());
            return Ok(());
        };
        if !self
            .state
            .selected_loaded_session()
            .is_some_and(|summary| summary.loaded_status == LoadedSessionStatus::Active)
        {
            self.state.status_message =
                Some("selected session is not an active loaded session".to_string());
            return Ok(());
        }
        self.rejoin_session(session_id).await
    }

    async fn rejoin_session(&mut self, session_id: SessionId) -> Result<(), AppRunError> {
        // Rejoin first validates the durable active-turn owner with a minimal bounded read, then
        // hydrates the latest TUI page before live runtime events continue as deltas.
        self.app
            .session_service
            .rejoin_running_session(session_id, 0, 1, 0, 1)
            .await?;
        let read = session_view(&self.app.session_service, session_id).await?;
        self.cancel_pending_prompt_enhance();
        self.apply_access_mode_owner(read.session.access_mode);
        self.state.load_turn_items(&read.session, &read.turns.items);
        self.state.status_message = Some(format!("rejoined running session {session_id}"));
        self.state.modal = Modal::None;
        Ok(())
    }

    async fn export_history_markdown(&mut self) -> Result<(), AppRunError> {
        let session_id = if self.state.route == Route::History {
            self.state.selected_session().map(|session| session.id)
        } else {
            self.state.current_session_id
        };
        let Some(session_id) = session_id else {
            self.state.status_message = Some("select or open a session first".to_string());
            return Ok(());
        };
        let read = canonical_markdown_export_read(&self.app.session_service, session_id).await?;
        if read.history.items.is_empty() {
            self.state.status_message = Some("session has no history to export".to_string());
            return Ok(());
        }

        let file_name = history_markdown_file_name(&read.session.title, session_id);
        let export_path = self
            .app
            .workspace
            .root
            .join(".moyai")
            .join("history-exports")
            .join(file_name);
        if let Some(parent) = export_path.parent() {
            fs::create_dir_all(parent.as_std_path())
                .map_err(|error| AppRunError::Message(error.to_string()))?;
        }
        let markdown = canonical_session_read_to_markdown(&read);
        fs::write(export_path.as_std_path(), markdown)
            .map_err(|error| AppRunError::Message(error.to_string()))?;
        self.state.status_message = Some(format!("exported history markdown to {export_path}"));
        Ok(())
    }

    async fn archive_selected_session(&mut self, archived: bool) -> Result<(), AppRunError> {
        if self.reject_agent_tree_navigation("session") {
            return Ok(());
        }
        let Some(session_id) = self.state.selected_session().map(|session| session.id) else {
            self.state.status_message = Some("select a session first".to_string());
            return Ok(());
        };
        self.app
            .session_service
            .set_session_archived(session_id, archived)
            .await?;
        self.state.status_message = Some(if archived {
            format!("archived session {session_id}")
        } else {
            format!("unarchived session {session_id}")
        });
        if self.state.current_session_id == Some(session_id) && archived {
            self.state.current_session_id = None;
            self.state.current_session_title = "New Session".to_string();
            self.state.transcript_entries.clear();
            self.state.tool_statuses.clear();
            self.state.current_plan = None;
            self.state.run_status = RunStatus::Idle;
            self.restore_global_access_mode_owner();
        }
        self.refresh_sessions().await
    }

    async fn rollback_selected_session(&mut self) -> Result<(), AppRunError> {
        if self.reject_agent_tree_navigation("session") {
            return Ok(());
        }
        let Some(session_id) = self.state.selected_session().map(|session| session.id) else {
            self.state.status_message = Some("select a session first".to_string());
            return Ok(());
        };
        if self
            .state
            .selected_loaded_session()
            .is_some_and(|summary| summary.loaded_status == LoadedSessionStatus::Active)
        {
            self.state.status_message = Some("running sessions cannot be rolled back".to_string());
            return Ok(());
        }
        let rolled_back = self
            .app
            .session_service
            .rollback_session(session_id, 1)
            .await?;
        self.state.status_message = Some(format!(
            "rolled back {} turn(s) in session {session_id}",
            rolled_back.dropped_turn_ids.len()
        ));
        self.reset_preview_turn_page();
        self.refresh_sessions().await?;
        if self.state.current_session_id == Some(session_id) {
            self.open_session(session_id).await?;
        }
        Ok(())
    }

    async fn refresh_sessions(&mut self) -> Result<(), AppRunError> {
        self.reset_preview_turn_page();
        let sessions = search_sessions(
            &self.app.session_service,
            self.app.workspace.project_id,
            &self.state.session_search_text,
            self.state.session_search_include_archived,
            50,
        )
        .await?;
        let summaries = self.loaded_summaries_for(sessions).await?;
        self.state.set_loaded_sessions(summaries);
        self.refresh_preview().await
    }

    async fn loaded_summaries_for(
        &self,
        sessions: Vec<SessionRecord>,
    ) -> Result<Vec<LoadedSessionSummary>, AppRunError> {
        let mut summaries = Vec::with_capacity(sessions.len());
        for session in sessions {
            summaries.push(
                self.app
                    .session_service
                    .loaded_session_summary(session)
                    .await?,
            );
        }
        Ok(summaries)
    }

    async fn refresh_loaded_summary_for_session(
        &mut self,
        session_id: SessionId,
    ) -> Result<(), AppRunError> {
        let Some(index) = self
            .state
            .loaded_sessions
            .iter()
            .position(|summary| summary.session.id == session_id)
        else {
            return Ok(());
        };
        let session = self.app.session_service.get_session(session_id).await?;
        let summary = self
            .app
            .session_service
            .loaded_session_summary(session)
            .await?;
        self.state.sessions[index] = summary.session.clone();
        self.state.loaded_sessions[index] = summary;
        Ok(())
    }

    async fn refresh_preview(&mut self) -> Result<(), AppRunError> {
        self.preview_entries.clear();
        self.preview_plan = None;
        self.preview_turn_total = 0;
        self.preview_turn_has_more = false;
        if let Some(session) = self.state.selected_session() {
            let page = self
                .app
                .session_service
                .canonical_turn_page(
                    session.id,
                    self.preview_turn_offset,
                    self.preview_turn_limit,
                )
                .await?;
            self.preview_entries = super::state::transcript_entries_from_turn_items(&page.items);
            self.preview_plan = latest_plan_from_turn_items(&page.items);
            self.preview_turn_offset = page.offset;
            self.preview_turn_limit = page.limit;
            self.preview_turn_total = page.total;
            self.preview_turn_has_more = page.has_more;
        }
        Ok(())
    }

    fn reset_preview_turn_page(&mut self) {
        self.preview_turn_offset = 0;
    }

    async fn previous_preview_turn_page(&mut self) -> Result<(), AppRunError> {
        if self.preview_turn_offset == 0 {
            self.state.status_message = Some("earlier turn page is not available".to_string());
            return Ok(());
        }
        self.preview_turn_offset = self
            .preview_turn_offset
            .saturating_sub(self.preview_turn_limit);
        self.refresh_preview().await
    }

    async fn next_preview_turn_page(&mut self) -> Result<(), AppRunError> {
        if !self.preview_turn_has_more {
            self.state.status_message = Some("later turn page is not available".to_string());
            return Ok(());
        }
        self.preview_turn_offset = self
            .preview_turn_offset
            .saturating_add(self.preview_turn_limit);
        self.refresh_preview().await
    }

    async fn reload_config(&mut self) -> Result<(), AppRunError> {
        self.base_config = ConfigLoader::load(&self.app.workspace.root, None)
            .map_err(|error| AppRunError::Message(format!("failed to reload config: {error}")))?;
        self.effective_config = self.base_config.clone();
        self.config_editor = ConfigEditorState::from_config(&self.effective_config);
        Ok(())
    }

    fn render(&self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),
                Constraint::Min(8),
                Constraint::Length(7),
            ])
            .split(area);
        self.render_header(frame, chunks[0]);
        self.render_body(frame, chunks[1]);
        self.render_composer(frame, chunks[2]);
        match self.state.modal {
            Modal::ConfigEditor => self.render_config_editor(frame),
            Modal::EnhanceReview => self.render_enhance_review(frame),
            Modal::WorkspacePicker => self.render_workspace_picker(frame),
            Modal::None => {}
        }
        if self.pending_permission.is_some() {
            self.render_permission_overlay(frame);
        }
    }

    fn render_header(&self, frame: &mut Frame<'_>, area: Rect) {
        let (status, status_style) = self.status_badge();
        let title = if self.state.route == Route::History {
            "History".to_string()
        } else if self.state.route == Route::Home {
            "Home".to_string()
        } else if let Some(session_id) = self.state.current_session_id {
            format!("{session_id} {}", self.state.current_session_title)
        } else {
            "Home".to_string()
        };
        let mut lines = vec![
            Line::from(vec![
                Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(status, status_style),
            ]),
            Line::from(vec![
                Span::raw(format!("model={}  ", self.effective_config.model.model)),
                Span::raw(format!(
                    "base_url={}  ",
                    self.effective_config.model.base_url
                )),
                Span::raw(format!(
                    "access_mode={}  ",
                    self.effective_config.permissions.access_mode.as_str()
                )),
                Span::raw(format!(
                    "workspace={}",
                    truncate_middle(self.app.workspace.root.as_str(), 42)
                )),
            ]),
        ];
        if let Some(activity) = self.activity_line() {
            lines.push(Line::from(activity));
        }
        let header = Paragraph::new(Text::from(lines))
            .block(Block::default().borders(Borders::ALL).title("Session"));
        frame.render_widget(header, area);
    }

    fn render_body(&self, frame: &mut Frame<'_>, area: Rect) {
        if self.state.route == Route::History {
            self.render_history(frame, area);
            return;
        }
        let sections = if area.width >= 120 {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
                .split(area)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
                .split(area)
        };
        self.render_transcript(frame, sections[0]);
        self.render_sidebar(frame, sections[1]);
    }

    fn render_transcript(&self, frame: &mut Frame<'_>, area: Rect) {
        let lines = if self.state.transcript_entries.is_empty() {
            vec![Line::from(
                "No transcript yet. Type a prompt and press Ctrl+Enter.",
            )]
        } else {
            self.state
                .transcript_entries
                .iter()
                .flat_map(entry_to_lines)
                .collect::<Vec<_>>()
        };
        let block = Block::default().borders(Borders::ALL).title("Transcript");
        let inner = block.inner(area);
        let scroll = wrapped_line_scroll(&lines, inner.width, inner.height);
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .scroll((scroll, 0))
                .wrap(Wrap { trim: false })
                .block(block),
            area,
        );
    }

    fn render_sidebar(&self, frame: &mut Frame<'_>, area: Rect) {
        let sections = sidebar_sections(area);
        let tool_lines = if self.state.tool_statuses.is_empty() {
            vec![Line::from("No tool activity yet.")]
        } else {
            self.state
                .tool_statuses
                .iter()
                .rev()
                .take(8)
                .flat_map(tool_to_lines)
                .collect::<Vec<_>>()
        };
        frame.render_widget(
            Paragraph::new(Text::from(tool_lines))
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title("Tools")),
            sections[0],
        );
        let plan_lines = render_plan_lines(self.sidebar_plan());
        frame.render_widget(
            Paragraph::new(Text::from(plan_lines))
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title("Plan")),
            sections[1],
        );
    }

    fn render_composer(&self, frame: &mut Frame<'_>, area: Rect) {
        if self.state.route == Route::History {
            frame.render_widget(
                Paragraph::new(Text::from(vec![
                    Line::from(format!(
                        "History screen  search=`{}`  include_archived={}",
                        self.state.session_search_text,
                        self.state.session_search_include_archived
                    )),
                    Line::from("Up/Down で session を選択し、Ctrl+Enter で transcript / active rejoin を開きます。"),
                    Line::from("PageUp/PageDown で canonical turn page を移動します。z で最新 turn を戻します。"),
                    Line::from("文字入力で検索、Backspace で削除、Esc で検索解除、Ctrl+I で archived 検索を切り替えます。"),
                ]))
                .wrap(Wrap { trim: false })
                .block(
                    Block::default().borders(Borders::ALL).title(
                        "Ctrl+Enter=open/rejoin  r=rejoin active  z=rollback latest turn  PageUp/PageDown=turn page  a=archive  u=unarchive  Ctrl+I=include_archived  F1=home  F3=config  F4=workspace  Ctrl+Q=quit",
                    ),
                ),
                area,
            );
            return;
        }
        let help = if self.state.route == Route::Home {
            "Ctrl+Enter=send/open/steer  Ctrl+X=stop  F2=history  F3=config  F4=workspace  F5=explorer  F6=enhance  F7=review  F8=toggle_access  F9=export_md  Enter=ime  Ctrl+J=newline  Ctrl+Q=quit"
        } else {
            "Ctrl+Enter=send/steer  Ctrl+X=stop  F1=home  F2=history  F3=config  F4=workspace  F5=explorer  F6=enhance  F7=review  F8=toggle_access  F9=export_md  Enter=ime  Ctrl+J=newline  Ctrl+Q=quit"
        };
        frame.render_widget(Clear, area);
        let block = Block::default().borders(Borders::ALL).title(help);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let wrapped = wrap_textarea_for_display(&self.composer, inner.width as usize);
        let scroll = wrapped
            .cursor_row
            .saturating_sub(inner.height.saturating_sub(1) as usize);
        frame.render_widget(
            Paragraph::new(Text::from(wrapped.lines.clone()))
                .scroll((scroll.min(u16::MAX as usize) as u16, 0)),
            inner,
        );
        let cursor_row = wrapped.cursor_row.saturating_sub(scroll);
        if cursor_row < inner.height as usize {
            frame.set_cursor_position(Position {
                x: inner.x
                    + wrapped
                        .cursor_col
                        .min(inner.width.saturating_sub(1) as usize) as u16,
                y: inner.y + cursor_row as u16,
            });
        }
    }

    fn sidebar_plan(&self) -> Option<&PlanView> {
        if self.state.current_session_id.is_none() || self.state.route == Route::History {
            self.preview_plan.as_ref()
        } else {
            self.state.current_plan.as_ref()
        }
    }

    fn status_badge(&self) -> (String, Style) {
        match self.state.run_status {
            RunStatus::Idle => ("status=idle".to_string(), Style::default().fg(Color::Gray)),
            RunStatus::Running => (
                format!(
                    "status=[{}] running",
                    self.spinner_frame(&["|", "/", "-", "\\"])
                ),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            RunStatus::Completed => (
                "status=completed".to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            RunStatus::Cancelled => (
                "status=cancelled".to_string(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            RunStatus::Failed => (
                "status=failed".to_string(),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
        }
    }

    fn activity_line(&self) -> Option<String> {
        match self.state.run_status {
            RunStatus::Running => Some(format!(
                "activity=[{}] {}",
                self.spinner_frame(&["|", "/", "-", "\\"]),
                self.state
                    .status_message
                    .clone()
                    .unwrap_or_else(|| "assistant is running".to_string())
            )),
            _ => self.state.status_message.clone(),
        }
    }

    fn spinner_frame(&self, frames: &[&'static str]) -> &'static str {
        let index = ((self.started_at.elapsed().as_millis() / 150) % frames.len() as u128) as usize;
        frames[index]
    }

    fn render_history(&self, frame: &mut Frame<'_>, area: Rect) {
        let columns = Layout::default()
            .direction(if area.width >= 120 {
                Direction::Horizontal
            } else {
                Direction::Vertical
            })
            .constraints(if area.width >= 120 {
                [Constraint::Percentage(40), Constraint::Percentage(60)]
            } else {
                [Constraint::Percentage(42), Constraint::Percentage(58)]
            })
            .split(area);
        let items = self
            .state
            .sessions
            .iter()
            .enumerate()
            .map(|(index, session)| {
                let style = if index == self.state.selected_session_index {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                let loaded = self.state.loaded_session_at(index);
                ListItem::new(format!(
                    "{}  {:?}  {}",
                    session.title,
                    session.status,
                    history_loaded_status_label(loaded)
                ))
                .style(style)
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            List::new(items).block(Block::default().borders(Borders::ALL).title(format!(
                "History search=`{}` include_archived={}",
                self.state.session_search_text, self.state.session_search_include_archived
            ))),
            columns[0],
        );
        let preview_sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(10), Constraint::Min(8)])
            .split(columns[1]);
        frame.render_widget(
            Paragraph::new(Text::from(self.render_history_summary_lines()))
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title("Selection")),
            preview_sections[0],
        );
        let preview = if self.preview_entries.is_empty() {
            Text::from("No preview available.")
        } else {
            Text::from(
                self.preview_entries
                    .iter()
                    .rev()
                    .take(24)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .flat_map(entry_to_lines)
                    .collect::<Vec<_>>(),
            )
        };
        frame.render_widget(
            Paragraph::new(preview).wrap(Wrap { trim: false }).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Transcript Preview"),
            ),
            preview_sections[1],
        );
    }

    fn render_history_summary_lines(&self) -> Vec<Line<'static>> {
        let Some(session) = self.state.selected_session() else {
            return vec![Line::from("No session selected.")];
        };
        let mut lines = vec![
            Line::from(format!("title={}", session.title)),
            Line::from(format!("status={:?}", session.status)),
            Line::from(format!("model={}", session.model)),
            Line::from(format!(
                "workspace={}",
                truncate_middle(session.cwd.as_str(), 44)
            )),
            Line::from(format!("turn_page={}", self.preview_turn_page_label())),
        ];
        if let Some(loaded) = self.state.selected_loaded_session() {
            lines.push(Line::from(format!(
                "loaded={}",
                loaded_session_status_line(loaded)
            )));
        }
        let plan_lines = render_plan_lines(self.preview_plan.as_ref());
        if self.preview_plan.is_some() {
            lines.push(Line::from(""));
            lines.push(Line::from("plan:"));
            lines.extend(plan_lines.into_iter().take(5));
        }
        lines
    }

    fn preview_turn_page_label(&self) -> String {
        if self.preview_turn_total == 0 {
            return "empty".to_string();
        }
        let start = self.preview_turn_offset.saturating_add(1);
        let end = self
            .preview_turn_offset
            .saturating_add(self.preview_entries.len())
            .min(self.preview_turn_total);
        format!(
            "{}-{} / {}{}",
            start,
            end,
            self.preview_turn_total,
            if self.preview_turn_has_more {
                " has_more"
            } else {
                ""
            }
        )
    }

    fn render_config_editor(&self, frame: &mut Frame<'_>) {
        let area = centered_rect(86, 76, frame.area());
        frame.render_widget(Clear, area);
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(area);
        let items = self
            .config_editor
            .fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let env_badge = field
                    .key
                    .env_override()
                    .filter(|name| std::env::var(name).is_ok())
                    .map(|_| " [ENV]")
                    .unwrap_or("");
                let style = if index == self.config_editor.selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Green)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                ListItem::new(format!(
                    "{} = {}{}",
                    field.key.label(),
                    truncate_middle(&field.value, 28),
                    env_badge
                ))
                .style(style)
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            List::new(items).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Config Fields"),
            ),
            columns[0],
        );
        let selected = self.config_editor.selected_field();
        frame.render_widget(
            Paragraph::new(Text::from(vec![
                Line::from(format!("Field: {}", selected.key.label())),
                Line::from(""),
                Line::from(selected.value.clone()),
                Line::from(""),
                Line::from("Up/Down select field"),
                Line::from("Type edits current value, Backspace/Delete clear"),
                Line::from("F2 Apply Session  F3 Save Global"),
                Line::from("Blank value means inherit/remove"),
                Line::from(format!(
                    "Env override: {}",
                    selected
                        .key
                        .env_override()
                        .filter(|name| std::env::var(name).is_ok())
                        .unwrap_or("none")
                )),
            ]))
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title("Editor")),
            columns[1],
        );
    }

    fn render_enhance_review(&self, frame: &mut Frame<'_>) {
        let area = centered_rect(92, 82, frame.area());
        frame.render_widget(Clear, area);
        let Some(prompt_review) = self.state.prompt_review.as_ref() else {
            return;
        };
        if prompt_review.phase == PromptReviewPhase::Enhancing {
            frame.render_widget(
                Paragraph::new(Text::from(vec![
                    Line::from("Generating enhanced draft..."),
                    Line::from(""),
                    Line::from("Esc = cancel and keep raw prompt"),
                ]))
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Prompt Enhance"),
                ),
                area,
            );
            return;
        }

        let columns = if area.width >= 140 {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(area)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
                .split(area)
        };
        frame.render_widget(
            Paragraph::new(prompt_review.raw_prompt_text.clone())
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title("Raw Prompt")),
            columns[0],
        );
        let mut review_editor = self.review_editor.clone();
        review_editor.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title("Enhanced Draft  F6=send enhanced  F7=send raw  Esc=cancel"),
        );
        frame.render_widget(&review_editor, columns[1]);
    }

    fn render_workspace_picker(&self, frame: &mut Frame<'_>) {
        let area = centered_rect(84, 48, frame.area());
        frame.render_widget(Clear, area);
        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(6), Constraint::Min(3)])
            .split(area);
        frame.render_widget(
            Paragraph::new(Text::from(vec![
                Line::from("起動後に使う作業フォルダを入力し、Ctrl+Enter で切り替えてください。"),
                Line::from("相対パスは現在の workspace cwd 基準で解決します。"),
                Line::from(format!("Current cwd: {}", self.app.workspace.cwd)),
                Line::from(
                    "F5 で入力中 path を file manager で開けます。Enter は IME 確定に使えます。",
                ),
                Line::from("Esc で閉じると現在の workspace を維持します。"),
            ]))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Workspace Picker"),
            ),
            sections[0],
        );
        let mut workspace_picker = self.workspace_picker.clone();
        workspace_picker.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title("Path  Ctrl+Enter=switch  F5=explorer  Enter=ime  Esc=cancel"),
        );
        frame.render_widget(&workspace_picker, sections[1]);
    }

    fn render_permission_overlay(&self, frame: &mut Frame<'_>) {
        let area = centered_rect(70, 40, frame.area());
        frame.render_widget(Clear, area);
        if let Some(permission) = self
            .pending_permission
            .as_ref()
            .map(|pending| &pending.request)
        {
            let mut lines = vec![
                Line::from(permission.summary.clone()),
                Line::from(""),
                Line::from("Details:"),
            ];
            if let Some(identity) = tui_permission_agent_identity(
                permission.agent_path.as_deref(),
                permission.agent_task_name.as_deref(),
            ) {
                lines.insert(1, Line::from(format!("Requesting agent: {identity}")));
            }
            if permission.details.is_empty() {
                lines.push(Line::from("  (none)"));
            } else {
                lines.extend(
                    permission
                        .details
                        .iter()
                        .map(|detail| Line::from(format!("  {detail}"))),
                );
            }
            lines.extend([
                Line::from(""),
                Line::from(format!(
                    "Targets: {}",
                    if permission.targets.is_empty() {
                        "(none)".to_string()
                    } else {
                        permission
                            .targets
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                )),
                Line::from(format!(
                    "Outside workspace: {}",
                    permission.outside_workspace
                )),
                Line::from(format!(
                    "Risks: {}",
                    if permission.risks.is_empty() {
                        "none".to_string()
                    } else {
                        permission
                            .risks
                            .iter()
                            .map(|risk| risk.label())
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                )),
                Line::from(format!(
                    "Access mode: {}",
                    self.effective_config.permissions.access_mode.as_str()
                )),
                Line::from(""),
                Line::from("a = approve and run once"),
                Line::from("d / Esc = do not run; stop the requesting task for new instructions"),
                Line::from("Ctrl+X = stop the entire active agent tree"),
            ]);
            frame.render_widget(
                Paragraph::new(Text::from(lines))
                    .wrap(Wrap { trim: false })
                    .block(Block::default().borders(Borders::ALL).title("Confirmation")),
                area,
            );
        }
    }
}

impl Drop for TuiController {
    fn drop(&mut self) {
        if let Some((_, cancellation)) = self.pending_prompt_enhance.take() {
            cancellation.cancel();
        }
    }
}

#[derive(Debug)]
enum RuntimeMessage {
    RunEvent {
        root_run_generation: Option<u64>,
        event: RunEvent,
    },
    Finished {
        run_generation: u64,
        result: Result<RunSummary, String>,
    },
    Permission {
        confirmation_id: u64,
        request: PermissionRequest,
        response: mpsc::Sender<ReviewDecision>,
        run_control: RunControl,
    },
    PermissionCancelled {
        confirmation_id: u64,
    },
    SteerStored {
        submission_id: u64,
        session_id: SessionId,
        prompt: String,
        result: Result<(), String>,
    },
    EnhanceFinished {
        request_id: u64,
        result: Result<String, String>,
    },
}

fn live_event_requires_canonical_refresh(event: &RunEvent) -> bool {
    matches!(
        event,
        RunEvent::UserTurnStored { .. }
            | RunEvent::ModelRequestPrepared { .. }
            | RunEvent::WorldStateUpdated { .. }
            | RunEvent::ToolCallPending { .. }
            | RunEvent::ToolCallCompleted { .. }
            | RunEvent::ToolCallFailed { .. }
            | RunEvent::FileChangesRecorded { .. }
            | RunEvent::CompactionCompleted { .. }
            | RunEvent::PermissionRequested { .. }
            | RunEvent::PermissionResolved { .. }
            | RunEvent::RecoverableRuntimeFeedback { .. }
    )
}

fn event_requires_plan_refresh(event: &RunEvent) -> bool {
    matches!(
        event,
        RunEvent::ToolCallCompleted {
            tool: crate::tool::ToolName::UpdatePlan,
            ..
        }
    )
}

struct TuiRenderer {
    tx: mpsc::SyncSender<RuntimeMessage>,
    root_run_generation: Option<u64>,
}

impl EventRenderer for TuiRenderer {
    fn render(&mut self, event: &RunEvent) -> Result<(), CliRenderError> {
        self.tx
            .send(RuntimeMessage::RunEvent {
                root_run_generation: self.root_run_generation,
                event: event.clone(),
            })
            .map_err(|error| CliRenderError::Message(error.to_string()))
    }

    fn finish(&mut self, _summary: &RunSummary) -> Result<(), CliRenderError> {
        Ok(())
    }

    fn render_session_list(&mut self, _sessions: &[SessionRecord]) -> Result<(), CliRenderError> {
        Ok(())
    }

    fn render_loaded_sessions(
        &mut self,
        _loaded: &crate::session::LoadedSessionList,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }

    fn render_session_history_items(
        &mut self,
        _session: &SessionRecord,
        _history_items: &[crate::protocol::HistoryItem],
    ) -> Result<(), CliRenderError> {
        Ok(())
    }

    fn render_session_history_page(
        &mut self,
        _page: &crate::session::CanonicalHistoryPage,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }

    fn render_session_read(
        &mut self,
        _read: &crate::session::CanonicalSessionRead,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }

    fn render_session_rejoin(
        &mut self,
        _rejoin: &crate::session::RunningSessionRejoin,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }

    fn render_session_turn_page(
        &mut self,
        _page: &crate::session::CanonicalTurnPage,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }

    fn render_session_runtime_event_page(
        &mut self,
        _page: &crate::session::CanonicalRuntimeEventPage,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
}

fn publish_tui_run_finished(
    tx: &mpsc::SyncSender<RuntimeMessage>,
    run_generation: u64,
    result: Result<RunSummary, String>,
) {
    let _ = tx.send(RuntimeMessage::Finished {
        run_generation,
        result,
    });
}

fn tui_terminal_error_status(cause: Option<&RunCancellationCause>) -> RunStatus {
    if matches!(cause, Some(RunCancellationCause::Interruption(_))) {
        RunStatus::Cancelled
    } else {
        RunStatus::Failed
    }
}

fn tui_run_status_for_session_status(status: SessionStatus) -> RunStatus {
    match status {
        SessionStatus::Idle => RunStatus::Idle,
        SessionStatus::Running => RunStatus::Running,
        SessionStatus::Completed => RunStatus::Completed,
        SessionStatus::Cancelled => RunStatus::Cancelled,
        SessionStatus::Failed => RunStatus::Failed,
    }
}

struct TuiConfirmationPrompt {
    tx: mpsc::SyncSender<RuntimeMessage>,
    next_permission_request_id: Arc<AtomicU64>,
}

impl ConfirmationPrompt for TuiConfirmationPrompt {
    fn confirm(&mut self, request: &PermissionRequest) -> Result<ReviewDecision, CliPromptError> {
        let control = RunControl::new();
        self.confirm_with_control(request, &control)?
            .into_review_decision()
    }

    fn confirm_with_control(
        &mut self,
        request: &PermissionRequest,
        control: &RunControl,
    ) -> Result<ConfirmationOutcome, CliPromptError> {
        if control.is_cancelled() {
            return Ok(ConfirmationOutcome::Interrupted);
        }
        let (response_tx, response_rx) = mpsc::channel();
        let confirmation_id = self
            .next_permission_request_id
            .fetch_add(1, Ordering::Relaxed);
        self.tx
            .send(RuntimeMessage::Permission {
                confirmation_id,
                request: request.clone(),
                response: response_tx,
                run_control: control.clone(),
            })
            .map_err(|error| CliPromptError::Message(error.to_string()))?;
        loop {
            match response_rx.recv_timeout(Duration::from_millis(25)) {
                Ok(_) if control.is_cancelled() => {
                    return Ok(ConfirmationOutcome::Interrupted);
                }
                Ok(ReviewDecision::Approved) => {
                    return Ok(ConfirmationOutcome::Resolved(
                        ToolApprovalDecision::Approved,
                    ));
                }
                Ok(ReviewDecision::Denied) => {
                    return Ok(ConfirmationOutcome::Resolved(
                        ToolApprovalDecision::Denied {
                            reason: "permission denied by user".to_string(),
                        },
                    ));
                }
                Ok(ReviewDecision::Abort) => return Ok(ConfirmationOutcome::AbortRequested),
                Err(mpsc::RecvTimeoutError::Timeout) if control.is_cancelled() => {
                    let _ = self
                        .tx
                        .send(RuntimeMessage::PermissionCancelled { confirmation_id });
                    return Ok(ConfirmationOutcome::Interrupted);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if control.is_cancelled() {
                        let _ = self
                            .tx
                            .send(RuntimeMessage::PermissionCancelled { confirmation_id });
                        return Ok(ConfirmationOutcome::Interrupted);
                    }
                    let message = "TUI permission response channel disconnected".to_string();
                    control.fail(message.clone());
                    return Err(CliPromptError::Message(message));
                }
            }
        }
    }
}

fn tui_permission_agent_identity(
    agent_path: Option<&str>,
    agent_task_name: Option<&str>,
) -> Option<String> {
    let path = agent_path?.trim();
    if path.is_empty() {
        return None;
    }
    let task_name = agent_task_name.unwrap_or_default().trim();
    Some(if task_name.is_empty() {
        path.to_string()
    } else {
        format!("{task_name} ({path})")
    })
}

fn clear_cancelled_tui_permission(
    pending: &mut Option<PendingPermission>,
    expected_confirmation_id: u64,
) -> bool {
    if pending.as_ref().map(|pending| pending.confirmation_id) != Some(expected_confirmation_id) {
        return false;
    }
    *pending = None;
    true
}

#[cfg(target_os = "windows")]
fn open_path_with_platform_file_manager(path: &camino::Utf8Path) -> io::Result<()> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{null, null_mut};

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            hwnd: *mut c_void,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show_command: i32,
        ) -> *mut c_void;
    }

    fn wide_null(value: &std::ffi::OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }

    let operation = wide_null(std::ffi::OsStr::new("open"));
    let file = wide_null(path.as_std_path().as_os_str());
    let result = unsafe {
        ShellExecuteW(
            null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            null(),
            null(),
            1,
        )
    } as isize;
    if result > 32 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "Windows ShellExecuteW failed with code {result}"
        )))
    }
}

#[cfg(target_os = "macos")]
fn open_path_with_platform_file_manager(path: &camino::Utf8Path) -> io::Result<()> {
    spawn_reaped_file_manager("/usr/bin/open", path)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_path_with_platform_file_manager(path: &camino::Utf8Path) -> io::Result<()> {
    let executable = ["/usr/bin/xdg-open", "/bin/xdg-open"]
        .into_iter()
        .find(|candidate| std::path::Path::new(candidate).is_file())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "xdg-open was not found"))?;
    spawn_reaped_file_manager(executable, path)
}

#[cfg(not(any(windows, unix)))]
fn open_path_with_platform_file_manager(_path: &camino::Utf8Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no trusted file manager launcher is available on this platform",
    ))
}

#[cfg(unix)]
fn spawn_reaped_file_manager(executable: &str, path: &camino::Utf8Path) -> io::Result<()> {
    let (sender, receiver) = std::sync::mpsc::sync_channel::<std::process::Child>(1);
    std::thread::Builder::new()
        .name("moyai-file-manager-reaper".to_string())
        .spawn(move || {
            if let Ok(mut child) = receiver.recv() {
                let _ = child.wait();
            }
        })?;
    let child = std::process::Command::new(executable)
        .arg(path.as_str())
        .spawn()?;
    sender.send(child).map_err(|error| {
        let mut child = error.0;
        let _ = child.kill();
        let _ = child.wait();
        io::Error::new(io::ErrorKind::BrokenPipe, "file manager reaper exited")
    })
}

fn setup_terminal() -> io::Result<TerminalHandle> {
    setup_terminal_resources(
        enable_raw_mode,
        || {
            let mut stdout = io::stdout();
            execute!(stdout, EnterAlternateScreen)
        },
        || Terminal::new(CrosstermBackend::new(io::stdout())),
        || {
            let mut stdout = io::stdout();
            execute!(stdout, LeaveAlternateScreen)
        },
        disable_raw_mode,
    )
}

fn setup_terminal_resources<T>(
    enable_raw: impl FnOnce() -> io::Result<()>,
    enter_alternate: impl FnOnce() -> io::Result<()>,
    construct: impl FnOnce() -> io::Result<T>,
    leave_alternate: impl FnOnce() -> io::Result<()>,
    disable_raw: impl FnOnce() -> io::Result<()>,
) -> io::Result<T> {
    enable_raw()?;
    if let Err(error) = enter_alternate() {
        let _ = leave_alternate();
        let _ = disable_raw();
        return Err(error);
    }
    match construct() {
        Ok(value) => Ok(value),
        Err(error) => {
            let _ = leave_alternate();
            let _ = disable_raw();
            Err(error)
        }
    }
}

fn restore_terminal(terminal: &mut TerminalHandle) -> io::Result<()> {
    restore_terminal_resources(
        terminal,
        disable_raw_mode,
        |terminal| execute!(terminal.backend_mut(), LeaveAlternateScreen),
        |terminal| terminal.show_cursor(),
    )
}

fn restore_terminal_resources<T>(
    terminal: &mut T,
    disable_raw: impl FnOnce() -> io::Result<()>,
    leave_alternate: impl FnOnce(&mut T) -> io::Result<()>,
    show_cursor: impl FnOnce(&mut T) -> io::Result<()>,
) -> io::Result<()> {
    let disable_result = disable_raw();
    let leave_result = leave_alternate(terminal);
    let cursor_result = show_cursor(terminal);
    disable_result.and(leave_result).and(cursor_result)
}

fn build_composer() -> TextArea<'static> {
    let mut textarea = TextArea::default();
    textarea.set_cursor_line_style(Style::default());
    textarea
}

fn is_stop_key(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('x') && key.modifiers.contains(KeyModifiers::CONTROL)
}

fn permission_decision_for_key(key: KeyEvent) -> Option<ReviewDecision> {
    match key.code {
        KeyCode::Char('a') => Some(ReviewDecision::Approved),
        KeyCode::Char('d') | KeyCode::Esc => Some(ReviewDecision::Abort),
        _ => None,
    }
}

fn key_leaves_current_task(key: KeyEvent, route: Route) -> bool {
    if key.code == KeyCode::Char('q') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return true;
    }
    if matches!(key.code, KeyCode::F(1) | KeyCode::F(2) | KeyCode::F(4)) {
        return true;
    }
    route == Route::History
        && (matches!(key.code, KeyCode::Up | KeyCode::Down)
            || (key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::CONTROL))
            || matches!(
                key.code,
                KeyCode::Char('a') | KeyCode::Char('u') | KeyCode::Char('r') | KeyCode::Char('z')
            ))
}

fn ctrl_enter_available(route: Route, status: RunStatus) -> bool {
    !(route == Route::History && status == RunStatus::Running)
}

fn textarea_value(textarea: &TextArea<'_>) -> String {
    textarea.lines().join("\n")
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[derive(Debug, Clone)]
struct WrappedTextAreaView {
    lines: Vec<Line<'static>>,
    cursor_row: usize,
    cursor_col: usize,
}

fn entry_to_lines(entry: &TranscriptEntry) -> Vec<Line<'static>> {
    let title_style = match entry.kind {
        TranscriptKind::User => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        TranscriptKind::Assistant => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        TranscriptKind::ReasoningSummary => Style::default().fg(Color::Yellow),
        TranscriptKind::Editing => Style::default().fg(Color::Yellow),
        TranscriptKind::Tool => Style::default().fg(Color::Magenta),
        TranscriptKind::Diff => Style::default().fg(Color::Blue),
        TranscriptKind::System => Style::default().fg(Color::Gray),
        TranscriptKind::Error => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    };
    let mut lines = vec![Line::from(Span::styled(
        format!("[{}]", entry.title),
        title_style,
    ))];
    push_multiline_text(&mut lines, &entry.body);
    lines.push(Line::from(""));
    lines
}

fn tool_to_lines(tool: &super::state::ToolStatusView) -> Vec<Line<'static>> {
    let mut body = vec![Line::from(format!("{} {:?}", tool.title, tool.status))];
    if let Some(summary) = &tool.summary {
        push_multiline_text(&mut body, summary);
    }
    if let Some(error) = &tool.error {
        push_multiline_text(&mut body, error);
    }
    body.push(Line::from(""));
    body
}

fn sidebar_sections(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area)
}

fn history_loaded_status_label(summary: Option<&LoadedSessionSummary>) -> String {
    let Some(summary) = summary else {
        return "loaded=unknown".to_string();
    };
    match summary.loaded_status {
        LoadedSessionStatus::Active => {
            let turn = active_turn_label(summary);
            let pending = summary.pending_permission_requests + summary.pending_user_input_requests;
            if pending > 0 {
                format!("loaded=active pending={pending} {turn}")
            } else {
                format!("loaded=active {turn}")
            }
        }
        LoadedSessionStatus::Idle => "loaded=idle".to_string(),
        LoadedSessionStatus::NotLoaded => "loaded=not_loaded".to_string(),
        LoadedSessionStatus::SystemError => "loaded=system_error".to_string(),
    }
}

fn loaded_session_status_line(summary: &LoadedSessionSummary) -> String {
    match summary.loaded_status {
        LoadedSessionStatus::Active => {
            let mut parts = vec!["active".to_string(), active_turn_label(summary)];
            if summary.pending_permission_requests > 0 {
                parts.push(format!(
                    "permission_pending={}",
                    summary.pending_permission_requests
                ));
            }
            if summary.pending_user_input_requests > 0 {
                parts.push(format!(
                    "user_pending={}",
                    summary.pending_user_input_requests
                ));
            }
            parts.join(" ")
        }
        LoadedSessionStatus::Idle => "idle".to_string(),
        LoadedSessionStatus::NotLoaded => "not_loaded".to_string(),
        LoadedSessionStatus::SystemError => "system_error".to_string(),
    }
}

fn active_turn_label(summary: &LoadedSessionSummary) -> String {
    if let Some(sequence_no) = summary.active_turn_sequence_no {
        return format!("turn={sequence_no}");
    }
    summary
        .active_turn_id
        .map(|turn_id| turn_id.to_string().chars().take(8).collect::<String>())
        .map(|turn| format!("turn={turn}"))
        .unwrap_or_else(|| "turn=active".to_string())
}

fn truncate_middle(value: &str, max_len: usize) -> String {
    if display_width(value) <= max_len {
        return value.to_string();
    }
    let ellipsis = "...";
    if max_len <= ellipsis.len() {
        return ellipsis.chars().take(max_len).collect();
    }

    let visible_width = max_len.saturating_sub(ellipsis.len());
    let left_width = visible_width / 2;
    let right_width = visible_width.saturating_sub(left_width);
    let left = prefix_within_width(value, left_width);
    let right = suffix_within_width(value, right_width);
    format!("{left}{ellipsis}{right}")
}

fn prefix_within_width(value: &str, max_width: usize) -> &str {
    let mut width = 0usize;
    let mut end = 0usize;
    for (index, ch) in value.char_indices() {
        let char_width = display_char_width(ch);
        if width + char_width > max_width {
            break;
        }
        width += char_width;
        end = index + ch.len_utf8();
    }
    &value[..end]
}

fn suffix_within_width(value: &str, max_width: usize) -> &str {
    let mut width = 0usize;
    let mut start = value.len();
    for (index, ch) in value.char_indices().rev() {
        let char_width = display_char_width(ch);
        if width + char_width > max_width {
            break;
        }
        width += char_width;
        start = index;
    }
    &value[start..]
}

fn display_width(value: &str) -> usize {
    value.chars().map(display_char_width).sum()
}

fn display_char_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0).max(1)
}

fn wrap_textarea_for_display(textarea: &TextArea<'_>, width: usize) -> WrappedTextAreaView {
    let width = width.max(1);
    let cursor = textarea.cursor();
    let mut lines = Vec::new();
    let mut cursor_row = 0;
    let mut cursor_col = 0;
    let mut cursor_set = false;

    for (row_idx, line) in textarea.lines().iter().enumerate() {
        let chars = line.chars().collect::<Vec<_>>();
        if chars.is_empty() {
            if row_idx == cursor.0 && !cursor_set {
                cursor_row = lines.len();
                cursor_col = 0;
                cursor_set = true;
            }
            lines.push(Line::from(""));
            continue;
        }

        let mut current = String::new();
        let mut current_width = 0usize;
        for (char_idx, ch) in chars.iter().copied().enumerate() {
            if row_idx == cursor.0 && cursor.1 == char_idx && !cursor_set {
                cursor_row = lines.len();
                cursor_col = current_width;
                cursor_set = true;
            }

            let char_width = display_char_width(ch);
            if current_width > 0 && current_width + char_width > width {
                lines.push(Line::from(current.clone()));
                current.clear();
                current_width = 0;
                if row_idx == cursor.0 && cursor.1 == char_idx && !cursor_set {
                    cursor_row = lines.len();
                    cursor_col = 0;
                    cursor_set = true;
                }
            }

            current.push(ch);
            current_width += char_width;
        }

        if row_idx == cursor.0 && cursor.1 == chars.len() && !cursor_set {
            cursor_row = lines.len();
            cursor_col = current_width;
            cursor_set = true;
        }

        lines.push(Line::from(current));
        if row_idx == cursor.0 && cursor.1 == chars.len() && cursor_col >= width {
            cursor_row = lines.len();
            cursor_col = 0;
            lines.push(Line::from(""));
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(""));
    }
    if !cursor_set {
        cursor_row = lines.len().saturating_sub(1);
        cursor_col = lines
            .last()
            .map(|line| line.width())
            .unwrap_or_default()
            .min(width.saturating_sub(1));
    }

    WrappedTextAreaView {
        lines,
        cursor_row,
        cursor_col: cursor_col.min(width.saturating_sub(1)),
    }
}

fn wrapped_line_scroll(lines: &[Line<'_>], width: u16, height: u16) -> u16 {
    if width == 0 || height == 0 {
        return 0;
    }
    wrapped_text_height(lines, width as usize)
        .saturating_sub(height as usize)
        .min(u16::MAX as usize) as u16
}

fn wrapped_text_height(lines: &[Line<'_>], width: usize) -> usize {
    if width == 0 {
        return 0;
    }
    lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(width))
        .sum()
}

fn render_plan_lines(plan: Option<&PlanView>) -> Vec<Line<'static>> {
    let Some(plan) = plan else {
        return vec![Line::from("No plan yet.")];
    };
    let mut lines = Vec::new();
    if let Some(explanation) = plan
        .explanation
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        lines.push(Line::from(truncate_middle(explanation, 44)));
    }
    lines.extend(plan.steps.iter().take(8).map(|step| {
        Line::from(format!(
            "{} {}",
            plan_step_status_marker(step.status),
            truncate_middle(&step.step, 40)
        ))
    }));
    if lines.is_empty() {
        lines.push(Line::from("Plan is empty."));
    }
    lines
}

fn plan_step_status_marker(status: PlanStepStatus) -> &'static str {
    match status {
        PlanStepStatus::Pending => "[ ]",
        PlanStepStatus::InProgress => "[>]",
        PlanStepStatus::Completed => "[x]",
    }
}

fn push_multiline_text(lines: &mut Vec<Line<'static>>, text: &str) {
    if text.is_empty() {
        lines.push(Line::from(""));
        return;
    }
    for segment in text.lines() {
        lines.push(Line::from(segment.to_string()));
    }
}

#[cfg(test)]
mod key_tests {
    use camino::Utf8PathBuf;

    use super::*;

    async fn tui_controller_with_session(
        test_name: &str,
    ) -> (tempfile::TempDir, TuiController, SessionId) {
        use crate::session::{SessionSelector, SessionStartRequest};

        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        std::fs::create_dir_all(&root).expect("workspace");
        let data_dir =
            Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data root");
        let paths = crate::storage::StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        };
        let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
        sqlite.migrate().expect("migrate");
        let store = crate::storage::StoreBundle::new(sqlite);
        let app = AppBootstrap::rebuild_for_directory_as_workspace_root_with_config(
            &root,
            store,
            ResolvedConfig::default(),
        )
        .await
        .expect("app");
        let session = app
            .session_service
            .start_or_resume(
                SessionStartRequest {
                    selector: SessionSelector::New,
                    title: Some(test_name.to_string()),
                    cwd: app.workspace.cwd.clone(),
                    model: app.config.model.model.clone(),
                    base_url: app.config.model.base_url.clone(),
                    access_mode: crate::config::AccessMode::Default,
                },
                app.workspace.clone(),
            )
            .await
            .expect("session");
        let session_id = session.session.id;
        let mut controller = TuiController::new(
            app,
            TuiArgs {
                directory: Some(root),
                session_id: None,
                continue_last: false,
            },
        )
        .await
        .expect("controller");
        controller.state.current_session_id = Some(session_id);
        (temp, controller, session_id)
    }

    fn set_tui_access_mode_field(controller: &mut TuiController, value: &str) {
        let field = controller
            .config_editor
            .fields
            .iter_mut()
            .find(|field| field.key == crate::tui::config_editor::ConfigField::AccessMode)
            .expect("access mode field");
        field.value = value.to_string();
    }

    fn tui_access_mode_field(controller: &TuiController) -> &str {
        controller
            .config_editor
            .fields
            .iter()
            .find(|field| field.key == crate::tui::config_editor::ConfigField::AccessMode)
            .expect("access mode field")
            .value
            .as_str()
    }

    fn tui_run_config(controller: &TuiController) -> ResolvedConfig {
        controller.effective_config.clone()
    }

    fn append_tui_user_history(controller: &TuiController, session_id: SessionId, text: &str) {
        controller
            .app
            .store
            .protocol_event_store()
            .seed_history_item_for_test(&crate::protocol::HistoryItem {
                id: crate::protocol::HistoryItemId::new(),
                session_id,
                scope: crate::protocol::HistoryScope::Turn {
                    turn_id: crate::protocol::TurnId::new(),
                },
                sequence_no: 1,
                created_at_ms: 1,
                payload: crate::protocol::HistoryItemPayload::UserTurn {
                    content: vec![crate::protocol::ContentPart::Text {
                        text: text.to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                },
            })
            .expect("TUI history item");
    }

    fn test_tui_permission(summary: &str) -> PermissionRequest {
        PermissionRequest {
            access: crate::workspace::AccessKind::Shell,
            summary: summary.to_string(),
            details: Vec::new(),
            targets: vec![camino::Utf8PathBuf::from("C:/workspace")],
            outside_workspace: false,
            risks: Vec::new(),
            agent_path: Some(format!("/root/{summary}")),
            agent_task_name: Some(summary.to_string()),
        }
    }

    fn completed_tui_summary(session_id: SessionId) -> RunSummary {
        RunSummary::from_terminal(
            session_id,
            crate::protocol::TurnId::new(),
            crate::session::DurableTurnTerminal {
                outcome: crate::protocol::TurnTerminalOutcome::Completed,
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            },
        )
    }

    fn completed_turn_event(session_id: SessionId) -> RunEvent {
        RunEvent::TurnTerminal {
            session_id,
            terminal: Box::new(crate::session::DurableTurnTerminal {
                outcome: crate::protocol::TurnTerminalOutcome::Completed,
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        }
    }

    fn stored_user_turn_event(session_id: SessionId, text: &str) -> RunEvent {
        RunEvent::UserTurnStored {
            session_id,
            turn: Box::new(crate::protocol::UserTurn {
                turn_id: crate::protocol::TurnId::new(),
                items: vec![crate::protocol::UserInputItem::Text {
                    text: text.to_string(),
                }],
                prompt_dispatch: Some(PromptDispatchPart::raw(text)),
                editor_context: None,
            }),
        }
    }

    fn recv_tui_runtime_message(receiver: &mpsc::Receiver<RuntimeMessage>) -> RuntimeMessage {
        for _ in 0..200 {
            match receiver.try_recv() {
                Ok(message) => return message,
                Err(mpsc::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("TUI runtime channel disconnected")
                }
            }
        }
        panic!("timed out waiting for TUI runtime message")
    }

    #[test]
    fn running_session_accepts_ctrl_enter_for_steer() {
        assert!(ctrl_enter_available(Route::Session, RunStatus::Running));
        assert!(!ctrl_enter_available(Route::History, RunStatus::Running));
    }

    #[tokio::test]
    async fn ctrl_q_cancels_the_enhance_provider_request_before_quitting() {
        let (_temp, mut controller, _session_id) =
            tui_controller_with_session("enhance-ctrl-q-cancellation").await;
        controller.composer.insert_str("keep this raw prompt");
        let request_id = 41;
        let cancellation = CancellationToken::new();
        controller.pending_prompt_enhance = Some((request_id, cancellation.clone()));
        controller
            .state
            .begin_prompt_enhance(request_id, "keep this raw prompt");

        controller
            .handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL))
            .await
            .expect("Ctrl+Q while enhancing");

        assert!(cancellation.is_cancelled());
        assert!(controller.pending_prompt_enhance.is_none());
        assert!(controller.state.prompt_review.is_none());
        assert_eq!(controller.state.modal, Modal::None);
        assert!(controller.should_quit);
        assert_eq!(textarea_value(&controller.composer), "keep this raw prompt");

        controller
            .runtime_tx
            .send(RuntimeMessage::EnhanceFinished {
                request_id,
                result: Ok("stale enhanced prompt".to_string()),
            })
            .expect("stale enhance completion");
        controller
            .drain_runtime_messages()
            .await
            .expect("ignore stale enhance completion");
        assert!(controller.state.prompt_review.is_none());
    }

    #[tokio::test]
    async fn escape_cancels_prompt_enhance_but_keeps_the_tui_running() {
        let (_temp, mut controller, _session_id) =
            tui_controller_with_session("enhance-escape-cancellation").await;
        controller.composer.insert_str("keep this raw prompt");
        let request_id = 42;
        let cancellation = CancellationToken::new();
        controller.pending_prompt_enhance = Some((request_id, cancellation.clone()));
        controller
            .state
            .begin_prompt_enhance(request_id, "keep this raw prompt");

        controller
            .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .expect("Escape while enhancing");

        assert!(cancellation.is_cancelled());
        assert!(controller.pending_prompt_enhance.is_none());
        assert!(controller.state.prompt_review.is_none());
        assert_eq!(controller.state.modal, Modal::None);
        assert!(!controller.should_quit);
        assert_eq!(textarea_value(&controller.composer), "keep this raw prompt");
        assert_eq!(
            controller.state.status_message.as_deref(),
            Some("cancelled prompt enhancement")
        );
    }

    #[tokio::test]
    async fn tui_pre_admission_failure_preserves_composer_without_phantom_transcript() {
        let (_temp, mut controller, session_id) =
            tui_controller_with_session("pre-admission-composer-failure").await;
        controller.composer.insert_str("keep this draft");
        controller.track_pending_composer_submission(
            PendingComposerSubmissionId::RootRun(7),
            Some(session_id),
        );
        assert!(controller.root_run_lifecycle.begin(7, RunControl::new()));
        assert!(controller.state.transcript_entries.is_empty());

        controller
            .runtime_tx
            .send(RuntimeMessage::Finished {
                run_generation: 7,
                result: Err("failed before durable admission".to_string()),
            })
            .expect("pre-admission failure");
        controller
            .drain_runtime_messages()
            .await
            .expect("drain pre-admission failure");

        assert_eq!(textarea_value(&controller.composer), "keep this draft");
        assert!(controller.pending_composer_submissions.is_empty());
        assert!(controller.state.transcript_entries.is_empty());
    }

    #[tokio::test]
    async fn tui_durable_user_turn_acceptance_projects_and_clears_matching_draft() {
        let (_temp, mut controller, session_id) =
            tui_controller_with_session("durable-user-turn-composer-commit").await;
        controller.composer.insert_str("accepted draft");
        controller.track_pending_composer_submission(
            PendingComposerSubmissionId::RootRun(7),
            Some(session_id),
        );
        assert_eq!(textarea_value(&controller.composer), "accepted draft");
        assert!(controller.state.transcript_entries.is_empty());

        controller
            .runtime_tx
            .send(RuntimeMessage::RunEvent {
                root_run_generation: Some(7),
                event: stored_user_turn_event(session_id, "accepted draft"),
            })
            .expect("durable user turn");
        controller
            .drain_runtime_messages()
            .await
            .expect("drain durable user turn");

        assert_eq!(textarea_value(&controller.composer), "");
        assert!(controller.pending_composer_submissions.is_empty());
        assert!(controller.state.transcript_entries.iter().any(|entry| {
            entry.kind == TranscriptKind::User
                && entry.title == "User"
                && entry.body == "accepted draft"
        }));
    }

    #[tokio::test]
    async fn tui_durable_steer_acceptance_does_not_clear_a_newer_draft_identity() {
        let (_temp, mut controller, session_id) =
            tui_controller_with_session("durable-steer-composer-race").await;
        controller.composer.insert_str("steer draft");
        controller.track_pending_composer_submission(
            PendingComposerSubmissionId::Steer(11),
            Some(session_id),
        );

        let previous = textarea_value(&controller.composer);
        controller.composer.insert_str("x");
        controller.record_composer_edit(&previous);
        let previous = textarea_value(&controller.composer);
        let _ = controller
            .composer
            .input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        controller.record_composer_edit(&previous);
        assert_eq!(
            textarea_value(&controller.composer),
            "steer draft",
            "the newer draft has the same text but a different identity"
        );

        controller
            .runtime_tx
            .send(RuntimeMessage::SteerStored {
                submission_id: 11,
                session_id,
                prompt: "steer draft".to_string(),
                result: Ok(()),
            })
            .expect("durable steer");
        controller
            .drain_runtime_messages()
            .await
            .expect("drain durable steer");

        assert_eq!(textarea_value(&controller.composer), "steer draft");
        assert!(controller.pending_composer_submissions.is_empty());
        assert!(controller.state.transcript_entries.iter().any(|entry| {
            entry.kind == TranscriptKind::User
                && entry.title == "User Steer"
                && entry.body == "steer draft"
        }));
    }

    #[test]
    fn ctrl_x_is_the_explicit_stop_key() {
        assert!(is_stop_key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL,
        )));
        assert!(!is_stop_key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        )));
    }

    #[test]
    fn pending_tui_root_run_cancel_is_owned_until_matching_finish() {
        let control = RunControl::new();
        let mut lifecycle = TuiRootRunLifecycle::default();
        assert!(lifecycle.begin(7, control.clone()));
        assert!(lifecycle.is_active());

        assert!(lifecycle.request_cancel());
        assert_eq!(
            control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            ))
        );
        assert!(
            !lifecycle.begin(8, RunControl::new()),
            "a cancelled run remains the owner until its matching finish"
        );
        assert_eq!(lifecycle.finish(6), None);
        assert!(lifecycle.is_active());
        let cause = lifecycle
            .finish(7)
            .expect("matching generation owns settlement")
            .expect("stop cause");
        assert_eq!(
            cause,
            RunCancellationCause::Interruption(TurnInterruptionCause::UserStop)
        );
        assert!(!lifecycle.is_active());
        assert!(!lifecycle.request_cancel());
        assert_eq!(
            tui_terminal_error_status(Some(&cause)),
            RunStatus::Cancelled
        );
        assert_eq!(
            tui_terminal_error_status(Some(&RunCancellationCause::Interruption(
                TurnInterruptionCause::ApprovalAborted
            ))),
            RunStatus::Cancelled
        );
        assert_eq!(
            tui_terminal_error_status(Some(&RunCancellationCause::Failure(
                "provider failed".to_string()
            ))),
            RunStatus::Failed
        );
        assert_eq!(
            tui_terminal_error_status(Some(&RunCancellationCause::Superseded)),
            RunStatus::Failed
        );
        assert_eq!(tui_terminal_error_status(None), RunStatus::Failed);
    }

    #[test]
    fn deferred_stop_is_tui_cancel_pending_while_success_remains_authoritative() {
        let control = RunControl::new();
        let success = control
            .begin_success_commit()
            .expect("success commit reservation");
        let mut lifecycle = TuiRootRunLifecycle::default();
        assert!(lifecycle.begin(8, control.clone()));

        assert!(
            lifecycle.request_cancel(),
            "a deferred Stop is accepted as cancel-pending"
        );
        assert!(lifecycle.is_active());
        assert_eq!(control.cause(), None);

        assert!(success.seal());
        assert!(control.success_is_sealed());
        assert_eq!(control.cause(), None);
        assert_eq!(lifecycle.finish(8), Some(None));
        assert!(!lifecycle.is_active());

        let sealed = RunControl::new();
        assert!(sealed.seal_success());
        assert!(lifecycle.begin(9, sealed));
        assert!(
            !lifecycle.request_cancel(),
            "a rejected Stop is not reported as cancel-pending"
        );
        assert_eq!(lifecycle.finish(9), Some(None));
    }

    #[tokio::test]
    async fn tui_stop_routes_through_root_owner_and_keeps_pending_permission_until_event() {
        let (_temp, mut controller, _session_id) =
            tui_controller_with_session("root-owned-stop").await;
        let root_control = RunControl::new();
        assert!(controller.root_run_lifecycle.begin(7, root_control.clone()));
        let request = test_tui_permission("child-stop-routing");
        let (response, _receiver) = mpsc::channel();
        let child_control = RunControl::new();
        controller.pending_permission = Some(PendingPermission {
            confirmation_id: 42,
            request: request.clone(),
            responder: response,
            run_control: child_control.clone(),
        });

        controller.stop_current_run().await.expect("stop request");

        assert_eq!(
            root_control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            ))
        );
        assert_eq!(
            child_control.cause(),
            None,
            "the TUI Stop surface must not classify a pending child directly"
        );
        assert!(controller.pending_permission.is_some());
        assert!(controller.state.permission.is_none());

        assert!(child_control.interrupt(TurnInterruptionCause::TreeStopped));
        controller
            .runtime_tx
            .send(RuntimeMessage::PermissionCancelled {
                confirmation_id: 42,
            })
            .expect("permission cancellation event");
        controller
            .drain_runtime_messages()
            .await
            .expect("drain cancellation event");
        assert!(controller.pending_permission.is_none());
        assert!(controller.state.permission.is_none());
    }

    #[tokio::test]
    async fn tui_root_terminal_event_and_finished_keep_a_live_child_permission_visible() {
        let (_temp, mut controller, session_id) =
            tui_controller_with_session("live-child-after-root-success").await;
        append_tui_user_history(&controller, session_id, "root task");
        assert!(controller.root_run_lifecycle.begin(7, RunControl::new()));
        let request = test_tui_permission("live-child");
        let expected_path = request.agent_path.clone();
        let (response, receiver) = mpsc::channel();
        controller.pending_permission = Some(PendingPermission {
            confirmation_id: 42,
            request,
            responder: response,
            run_control: RunControl::new(),
        });

        controller
            .runtime_tx
            .send(RuntimeMessage::RunEvent {
                root_run_generation: Some(7),
                event: completed_turn_event(session_id),
            })
            .expect("terminal event");
        controller
            .drain_runtime_messages()
            .await
            .expect("terminal event drain");
        assert_eq!(
            controller
                .pending_permission
                .as_ref()
                .and_then(|pending| pending.request.agent_path.clone()),
            expected_path
        );
        assert!(controller.state.permission.is_none());

        controller
            .runtime_tx
            .send(RuntimeMessage::Finished {
                run_generation: 7,
                result: Ok(completed_tui_summary(session_id)),
            })
            .expect("root finish");
        controller
            .drain_runtime_messages()
            .await
            .expect("root finish drain");
        assert_eq!(
            controller
                .pending_permission
                .as_ref()
                .map(|pending| pending.confirmation_id),
            Some(42)
        );
        assert!(controller.state.permission.is_none());

        controller
            .answer_permission(ReviewDecision::Approved)
            .expect("answer surviving child permission");
        assert_eq!(receiver.try_recv(), Ok(ReviewDecision::Approved));
    }

    #[tokio::test]
    async fn tui_root_success_drops_a_terminal_child_permission_without_cancel_event() {
        let (_temp, mut controller, session_id) =
            tui_controller_with_session("terminal-child-after-root-success").await;
        append_tui_user_history(&controller, session_id, "root task");
        assert!(controller.root_run_lifecycle.begin(7, RunControl::new()));
        let request = test_tui_permission("terminal-child");
        let (response, receiver) = mpsc::channel();
        let child_control = RunControl::new();
        assert!(child_control.interrupt(TurnInterruptionCause::TreeStopped));
        controller.pending_permission = Some(PendingPermission {
            confirmation_id: 42,
            request,
            responder: response,
            run_control: child_control,
        });

        controller
            .runtime_tx
            .send(RuntimeMessage::RunEvent {
                root_run_generation: Some(7),
                event: completed_turn_event(session_id),
            })
            .expect("terminal event");
        controller
            .drain_runtime_messages()
            .await
            .expect("terminal event drain");
        assert!(controller.pending_permission.is_none());
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));

        controller
            .runtime_tx
            .send(RuntimeMessage::Finished {
                run_generation: 7,
                result: Ok(completed_tui_summary(session_id)),
            })
            .expect("root finish");
        controller
            .drain_runtime_messages()
            .await
            .expect("root finish drain");
        assert!(controller.pending_permission.is_none());
        assert!(controller.state.permission.is_none());
    }

    #[tokio::test]
    async fn tui_abort_permission_answer_preserves_an_existing_terminal_owner() {
        let causes = [
            RunCancellationCause::Interruption(TurnInterruptionCause::UserStop),
            RunCancellationCause::Failure("provider failed first".to_string()),
            RunCancellationCause::Superseded,
        ];

        for cause in causes {
            let (_temp, mut controller, _session_id) =
                tui_controller_with_session("late-permission-abort").await;
            let request = test_tui_permission("late-abort");
            let (response, receiver) = mpsc::channel();
            let run_control = RunControl::new();
            assert!(run_control.cancel(cause.clone()));
            controller.pending_permission = Some(PendingPermission {
                confirmation_id: 42,
                request: request.clone(),
                responder: response,
                run_control,
            });

            controller
                .answer_permission(ReviewDecision::Abort)
                .expect("late abort uses the existing terminal owner");
            assert!(controller.pending_permission.is_none());
            assert!(controller.state.permission.is_none());
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::TryRecvError::Disconnected)
            ));
            assert_eq!(
                controller.state.status_message,
                Some(run_cancellation_status_message(&cause))
            );
        }
    }

    #[tokio::test]
    async fn tui_permission_send_success_is_neutral_until_runtime_classifies_the_turn() {
        let (_temp, mut controller, _session_id) =
            tui_controller_with_session("permission-decision-pending").await;
        let request = test_tui_permission("abort-pending-classification");
        let (response, receiver) = mpsc::channel();
        controller.pending_permission = Some(PendingPermission {
            confirmation_id: 42,
            request: request.clone(),
            responder: response,
            run_control: RunControl::new(),
        });

        controller
            .answer_permission(ReviewDecision::Abort)
            .expect("send permission decision");

        assert_eq!(receiver.try_recv(), Ok(ReviewDecision::Abort));
        assert!(controller.pending_permission.is_none());
        assert!(controller.state.permission.is_none());
        assert_eq!(
            controller.state.status_message,
            Some(permission_decision_pending_status_message())
        );
    }

    #[tokio::test]
    async fn tui_permission_responder_failure_remains_an_operational_failure() {
        let (_temp, mut controller, _session_id) =
            tui_controller_with_session("permission-responder-failure").await;
        let request = test_tui_permission("disconnected-responder");
        let (response, receiver) = mpsc::channel();
        drop(receiver);
        let run_control = RunControl::new();
        controller.pending_permission = Some(PendingPermission {
            confirmation_id: 42,
            request: request.clone(),
            responder: response,
            run_control: run_control.clone(),
        });

        controller
            .answer_permission(ReviewDecision::Approved)
            .expect("surface records the responder failure");
        let cause = run_control.cause().expect("typed responder failure");
        assert!(matches!(
            &cause,
            RunCancellationCause::Failure(message)
                if message.contains("TUI permission response failed")
        ));
        assert!(controller.pending_permission.is_none());
        assert!(controller.state.permission.is_none());
        assert_eq!(
            controller.state.status_message,
            Some(run_cancellation_status_message(&cause))
        );
    }

    #[tokio::test]
    async fn tui_durable_only_tree_stop_preserves_completed_root_status() {
        use crate::session::{NewSession, SessionRepository as _};

        let (_temp, mut controller, root_session_id) =
            tui_controller_with_session("durable-only-stop").await;
        let repository = controller.app.store.session_repo();
        let root_turn = crate::protocol::TurnId::new();
        repository
            .admit_session_turn(root_session_id, root_turn)
            .await
            .expect("root admission")
            .expect("root admitted");
        append_tui_user_history(&controller, root_session_id, "run detached work");
        let root_target = repository
            .captured_running_terminal_target(root_session_id)
            .await
            .expect("capture root target")
            .expect("root running target");
        assert!(
            repository
                .terminalize_captured_running_session_with_protocol_event(
                    root_session_id,
                    &completed_turn_event(root_session_id),
                    root_target,
                )
                .await
                .expect("complete root")
        );
        let child = repository
            .create_session(NewSession {
                project_id: controller.app.workspace.project_id,
                title: "durable child".to_string(),
                cwd: controller.app.workspace.cwd.clone(),
                model: controller.app.config.model.model.clone(),
                base_url: controller.app.config.model.base_url.clone(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("child session");
        repository
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                child.id,
                "/root/durable_child",
                "durable_child",
            )
            .await
            .expect("spawn edge");
        repository
            .admit_session_turn(child.id, crate::protocol::TurnId::new())
            .await
            .expect("child admission")
            .expect("child admitted");
        controller
            .open_session(root_session_id)
            .await
            .expect("open root");
        assert_eq!(controller.state.run_status, RunStatus::Completed);
        assert!(!controller.root_run_lifecycle.is_active());

        controller.stop_current_run().await.expect("tree stop");

        assert_eq!(controller.state.run_status, RunStatus::Completed);
        assert_eq!(
            repository
                .get_session(root_session_id)
                .await
                .expect("preserved root")
                .status,
            SessionStatus::Completed
        );
        assert_eq!(
            repository
                .get_session(child.id)
                .await
                .expect("stopped child")
                .status,
            SessionStatus::Cancelled
        );
    }

    #[test]
    fn tui_renderer_defers_root_completion_until_worker_settlement() {
        let (tx, rx) = mpsc::sync_channel(TUI_RUNTIME_MAILBOX_CAPACITY);
        let mut renderer = TuiRenderer {
            tx: tx.clone(),
            root_run_generation: None,
        };
        let summary = completed_tui_summary(SessionId::new());

        renderer.finish(&summary).expect("renderer finish");
        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));

        publish_tui_run_finished(&tx, 9, Ok(summary.clone()));
        assert!(matches!(
            rx.try_recv().expect("worker settlement"),
            RuntimeMessage::Finished {
                run_generation: 9,
                result: Ok(received),
            } if received.session_id() == summary.session_id()
        ));
    }

    #[test]
    fn tui_permission_keys_approve_or_abort_without_reusing_tree_stop() {
        assert_eq!(
            permission_decision_for_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some(ReviewDecision::Approved)
        );
        assert_eq!(
            permission_decision_for_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE)),
            Some(ReviewDecision::Abort)
        );
        assert_eq!(
            permission_decision_for_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(ReviewDecision::Abort)
        );
        assert_eq!(
            permission_decision_for_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            None,
            "Ctrl+X remains the separate tree-wide stop action"
        );
    }

    #[test]
    fn task_navigation_keys_are_classified_without_blocking_session_steer() {
        assert!(key_leaves_current_task(
            KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE),
            Route::Session,
        ));
        assert!(key_leaves_current_task(
            KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE),
            Route::Session,
        ));
        assert!(key_leaves_current_task(
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
            Route::Session,
        ));
        assert!(key_leaves_current_task(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
            Route::History,
        ));
        assert!(!key_leaves_current_task(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
            Route::Session,
        ));
        assert!(!key_leaves_current_task(
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
            Route::Session,
        ));
    }

    #[test]
    fn tui_permission_identity_includes_task_name_and_agent_path() {
        assert_eq!(
            tui_permission_agent_identity(Some("/root/reviewer"), Some("reviewer")).as_deref(),
            Some("reviewer (/root/reviewer)")
        );
        assert_eq!(
            tui_permission_agent_identity(Some("/root/reviewer"), None).as_deref(),
            Some("/root/reviewer")
        );
    }

    #[test]
    fn cancelled_tui_permission_clears_by_id_and_advances_broker() {
        let (runtime_tx, runtime_rx) = mpsc::sync_channel(TUI_RUNTIME_MAILBOX_CAPACITY);
        let broker = SharedConfirmationPrompt::new(TuiConfirmationPrompt {
            tx: runtime_tx,
            next_permission_request_id: Arc::new(AtomicU64::new(11)),
        });

        let first_control = RunControl::new();
        let (first_done_tx, first_done_rx) = mpsc::sync_channel(1);
        let mut first_prompt = broker.clone();
        let first_wait_control = first_control.clone();
        std::thread::spawn(move || {
            let result = first_prompt
                .confirm_with_control(&test_tui_permission("first"), &first_wait_control);
            let _ = first_done_tx.send(result);
        });
        let (first_id, first_response, first_request_control) =
            match recv_tui_runtime_message(&runtime_rx) {
                RuntimeMessage::Permission {
                    confirmation_id,
                    response,
                    run_control,
                    ..
                } => (confirmation_id, response, run_control),
                _ => panic!("expected first TUI permission"),
            };
        first_control.interrupt(TurnInterruptionCause::UserStop);
        match recv_tui_runtime_message(&runtime_rx) {
            RuntimeMessage::PermissionCancelled { confirmation_id } => {
                assert_eq!(confirmation_id, first_id)
            }
            _ => panic!("expected TUI permission cancellation"),
        }
        assert_eq!(
            first_done_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("first TUI confirmation result")
                .expect("first TUI confirmation"),
            ConfirmationOutcome::Interrupted
        );

        let mut pending = Some(PendingPermission {
            confirmation_id: first_id,
            request: test_tui_permission("first"),
            responder: first_response,
            run_control: first_request_control,
        });
        assert!(!clear_cancelled_tui_permission(&mut pending, first_id + 1,));
        assert_eq!(
            pending.as_ref().map(|pending| pending.confirmation_id),
            Some(first_id)
        );
        assert!(clear_cancelled_tui_permission(&mut pending, first_id));
        assert!(pending.is_none());

        let (second_done_tx, second_done_rx) = mpsc::sync_channel(1);
        let mut second_prompt = broker;
        std::thread::spawn(move || {
            let result = second_prompt.confirm(&test_tui_permission("second"));
            let _ = second_done_tx.send(result);
        });
        let (second_id, second_response) = match recv_tui_runtime_message(&runtime_rx) {
            RuntimeMessage::Permission {
                confirmation_id,
                response,
                ..
            } => (confirmation_id, response),
            _ => panic!("expected second TUI permission"),
        };
        assert!(second_id > first_id);
        second_response
            .send(ReviewDecision::Approved)
            .expect("answer second TUI permission");
        assert_eq!(
            second_done_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("second TUI confirmation result")
                .expect("second TUI confirmation"),
            ReviewDecision::Approved
        );
    }

    #[test]
    fn tui_permission_abort_is_ticket_local_and_loses_to_existing_cause() {
        let (runtime_tx, runtime_rx) = mpsc::sync_channel(TUI_RUNTIME_MAILBOX_CAPACITY);
        let mut prompt = TuiConfirmationPrompt {
            tx: runtime_tx,
            next_permission_request_id: Arc::new(AtomicU64::new(31)),
        };
        let control = RunControl::new();
        let observer = control.clone();
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = prompt.confirm_with_control(&test_tui_permission("abort"), &control);
            let _ = done_tx.send(result);
        });
        let response = match recv_tui_runtime_message(&runtime_rx) {
            RuntimeMessage::Permission { response, .. } => response,
            _ => panic!("expected TUI permission"),
        };
        response
            .send(ReviewDecision::Abort)
            .expect("send ticket-local abort");
        assert_eq!(
            done_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("abort result")
                .expect("abort outcome"),
            ConfirmationOutcome::AbortRequested
        );
        assert_eq!(observer.cause(), None);

        let (runtime_tx, runtime_rx) = mpsc::sync_channel(TUI_RUNTIME_MAILBOX_CAPACITY);
        let mut prompt = TuiConfirmationPrompt {
            tx: runtime_tx,
            next_permission_request_id: Arc::new(AtomicU64::new(41)),
        };
        let control = RunControl::new();
        let observer = control.clone();
        let worker_control = control.clone();
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = prompt
                .confirm_with_control(&test_tui_permission("competing abort"), &worker_control);
            let _ = done_tx.send(result);
        });
        let response = match recv_tui_runtime_message(&runtime_rx) {
            RuntimeMessage::Permission { response, .. } => response,
            _ => panic!("expected competing TUI permission"),
        };
        assert!(control.fail("provider failed first"));
        response
            .send(ReviewDecision::Abort)
            .expect("send losing abort");
        assert_eq!(
            done_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("competing result")
                .expect("competing outcome"),
            ConfirmationOutcome::Interrupted
        );
        assert_eq!(
            observer.cause(),
            Some(RunCancellationCause::Failure(
                "provider failed first".to_string()
            ))
        );
    }

    #[tokio::test]
    async fn f2_session_apply_persists_access_before_committing_next_turn_config() {
        use crate::session::SessionRepository as _;

        let (_temp, mut controller, session_id) =
            tui_controller_with_session("f2-access-success").await;
        set_tui_access_mode_field(&mut controller, "full_access");

        controller
            .handle_config_editor_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE))
            .await
            .expect("F2 apply");

        assert_eq!(
            controller.effective_config.permissions.access_mode,
            crate::config::AccessMode::FullAccess
        );
        assert_eq!(
            controller
                .app
                .store
                .session_repo()
                .get_session(session_id)
                .await
                .expect("durable session")
                .access_mode,
            crate::config::AccessMode::FullAccess
        );

        assert_eq!(
            controller
                .app
                .session_service
                .get_session(session_id)
                .await
                .expect("reopened durable session")
                .access_mode,
            crate::config::AccessMode::FullAccess,
            "reopen reads the persisted session access mode"
        );
    }

    #[tokio::test]
    async fn archiving_current_tui_session_restores_global_access_owner_for_the_next_run() {
        let (_temp, mut controller, session_id) =
            tui_controller_with_session("archive-access-owner").await;
        let global_access_mode = crate::config::AccessMode::Default;
        let session_access_mode = crate::config::AccessMode::FullAccess;
        controller.app.config.permissions.access_mode = global_access_mode;
        controller.base_config.permissions.access_mode = global_access_mode;
        controller.apply_access_mode_owner(global_access_mode);
        controller
            .app
            .session_service
            .update_root_session_access_mode(session_id, session_access_mode)
            .await
            .expect("session access owner");
        append_tui_user_history(&controller, session_id, "archive current session");
        controller
            .open_session(session_id)
            .await
            .expect("open full-access session");
        assert_eq!(
            controller.effective_config.permissions.access_mode,
            session_access_mode
        );

        controller
            .archive_selected_session(true)
            .await
            .expect("archive current session");

        assert_eq!(controller.state.current_session_id, None);
        assert_eq!(
            controller.effective_config.permissions.access_mode,
            global_access_mode
        );
        assert_eq!(
            tui_access_mode_field(&controller),
            global_access_mode.as_str()
        );
        assert_eq!(
            controller.base_config.permissions.access_mode,
            global_access_mode
        );
        assert_eq!(
            controller.app.config.permissions.access_mode,
            global_access_mode
        );
        assert_eq!(
            tui_run_config(&controller).permissions.access_mode,
            global_access_mode
        );
    }

    #[tokio::test]
    async fn failed_current_tui_archive_preserves_session_access_owner() {
        let (_temp, mut controller, session_id) =
            tui_controller_with_session("archive-access-failure").await;
        let global_access_mode = crate::config::AccessMode::Default;
        let session_access_mode = crate::config::AccessMode::FullAccess;
        controller.app.config.permissions.access_mode = global_access_mode;
        controller.base_config.permissions.access_mode = global_access_mode;
        controller
            .app
            .session_service
            .update_root_session_access_mode(session_id, session_access_mode)
            .await
            .expect("session access owner");
        append_tui_user_history(&controller, session_id, "archive must fail");
        controller
            .open_session(session_id)
            .await
            .expect("open full-access session");
        controller
            .app
            .store
            .session_repo()
            .admit_session_turn(session_id, crate::protocol::TurnId::new())
            .await
            .expect("active session admission")
            .expect("active session admitted");

        assert!(controller.archive_selected_session(true).await.is_err());

        assert_eq!(controller.state.current_session_id, Some(session_id));
        assert_eq!(
            controller.effective_config.permissions.access_mode,
            session_access_mode
        );
        assert_eq!(
            tui_access_mode_field(&controller),
            session_access_mode.as_str()
        );
        assert_eq!(
            tui_run_config(&controller).permissions.access_mode,
            session_access_mode
        );
        assert_eq!(
            controller.base_config.permissions.access_mode,
            global_access_mode
        );
    }

    #[tokio::test]
    async fn rejoining_another_active_tui_root_applies_its_durable_access_owner() {
        use crate::session::{NewSession, SessionRepository as _};

        let (_temp, mut controller, session_a_id) =
            tui_controller_with_session("rejoin-access-session-a").await;
        let global_access_mode = crate::config::AccessMode::Default;
        let session_a_access_mode = crate::config::AccessMode::FullAccess;
        let session_b_access_mode = crate::config::AccessMode::Default;
        controller.app.config.permissions.access_mode = global_access_mode;
        controller.base_config.permissions.access_mode = global_access_mode;
        controller
            .app
            .session_service
            .update_root_session_access_mode(session_a_id, session_a_access_mode)
            .await
            .expect("session A access owner");
        append_tui_user_history(&controller, session_a_id, "session A context");
        controller
            .open_session(session_a_id)
            .await
            .expect("open session A");
        let session_a = controller
            .app
            .session_service
            .get_session(session_a_id)
            .await
            .expect("session A");
        let repository = controller.app.store.session_repo();
        let session_b = repository
            .create_session(NewSession {
                project_id: session_a.project_id,
                title: "active session B".to_string(),
                cwd: session_a.cwd,
                model: session_a.model,
                base_url: session_a.base_url,
                access_mode: session_b_access_mode,
            })
            .await
            .expect("session B");
        let turn_id = crate::protocol::TurnId::new();
        let admission = repository
            .admit_session_turn(session_b.id, turn_id)
            .await
            .expect("session B admission")
            .expect("session B admitted");
        repository
            .append_user_turn_with_protocol_bundle(
                session_b.id,
                admission.admission_id,
                &crate::protocol::UserTurn {
                    turn_id,
                    items: vec![crate::protocol::UserInputItem::Text {
                        text: "active session B context".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                },
                turn_id,
                0,
            )
            .await
            .expect("session B history");

        controller
            .rejoin_session(session_b.id)
            .await
            .expect("rejoin session B");

        assert_eq!(controller.state.current_session_id, Some(session_b.id));
        assert_eq!(
            controller.effective_config.permissions.access_mode,
            session_b_access_mode
        );
        assert_eq!(
            tui_access_mode_field(&controller),
            session_b_access_mode.as_str()
        );
        assert_eq!(
            tui_run_config(&controller).permissions.access_mode,
            session_b_access_mode
        );
        assert_eq!(
            repository
                .get_session(session_a_id)
                .await
                .expect("unchanged session A")
                .access_mode,
            session_a_access_mode
        );
        assert_eq!(
            repository
                .get_session(session_b.id)
                .await
                .expect("durable session B")
                .access_mode,
            session_b_access_mode
        );
    }

    #[tokio::test]
    async fn explicit_child_session_rejects_f2_and_f8_access_changes_without_live_drift() {
        use crate::session::{NewSession, SessionRepository as _};

        let (_temp, controller, root_session_id) =
            tui_controller_with_session("child-access-owner").await;
        let root_session = controller
            .app
            .session_service
            .get_session(root_session_id)
            .await
            .expect("root session");
        let repository = controller.app.store.session_repo();
        let child = repository
            .create_session(NewSession {
                project_id: root_session.project_id,
                title: "child".to_string(),
                cwd: root_session.cwd.clone(),
                model: root_session.model.clone(),
                base_url: root_session.base_url.clone(),
                access_mode: crate::config::AccessMode::Default,
            })
            .await
            .expect("child session");
        repository
            .insert_session_spawn_edge(
                root_session_id,
                root_session_id,
                child.id,
                "/root/child",
                "child",
            )
            .await
            .expect("child edge");
        controller
            .app
            .store
            .protocol_event_store()
            .seed_history_item_for_test(&crate::protocol::HistoryItem {
                id: crate::protocol::HistoryItemId::new(),
                session_id: child.id,
                scope: crate::protocol::HistoryScope::Turn {
                    turn_id: crate::protocol::TurnId::new(),
                },
                sequence_no: 1,
                created_at_ms: 1,
                payload: crate::protocol::HistoryItemPayload::UserTurn {
                    content: vec![crate::protocol::ContentPart::Text {
                        text: "child context".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                },
            })
            .expect("child history");
        let mut controller = TuiController::new(
            controller.app.clone(),
            TuiArgs {
                directory: Some(root_session.cwd),
                session_id: Some(child.id),
                continue_last: false,
            },
        )
        .await
        .expect("explicit child controller");
        set_tui_access_mode_field(&mut controller, "full_access");

        controller
            .handle_config_editor_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE))
            .await
            .expect("F2 rejection");
        assert_eq!(
            controller.effective_config.permissions.access_mode,
            crate::config::AccessMode::Default
        );
        controller.toggle_access_mode().await.expect("F8 rejection");
        assert_eq!(
            controller.effective_config.permissions.access_mode,
            crate::config::AccessMode::Default
        );
        assert_eq!(
            repository
                .get_session(child.id)
                .await
                .expect("durable child")
                .access_mode,
            crate::config::AccessMode::Default
        );
        assert!(
            controller
                .state
                .status_message
                .as_deref()
                .is_some_and(|message| message.contains("child agent session"))
        );
    }

    #[test]
    fn f2_session_apply_keeps_next_turn_config_when_persistence_fails() {
        let mut effective = ResolvedConfig::default();
        effective.permissions.access_mode = crate::config::AccessMode::Default;
        let mut candidate = effective.clone();
        candidate.permissions.access_mode = crate::config::AccessMode::FullAccess;

        assert!(!commit_tui_effective_config(
            &mut effective,
            candidate,
            false,
        ));
        assert_eq!(
            effective.permissions.access_mode,
            crate::config::AccessMode::Default
        );
    }

    #[test]
    fn active_turn_steer_submission_is_single_flight() {
        let submissions = vec![PendingComposerSubmission {
            id: PendingComposerSubmissionId::Steer(7),
            session_id: Some(SessionId::new()),
            draft_revision: 3,
            draft_text: "steer".to_string(),
        }];

        assert!(has_pending_steer_submission(&submissions));
        assert!(!has_pending_steer_submission(&[
            PendingComposerSubmission {
                id: PendingComposerSubmissionId::RootRun(7),
                session_id: None,
                draft_revision: 3,
                draft_text: "root".to_string(),
            }
        ]));
    }

    #[test]
    fn terminal_setup_rolls_back_partial_resource_acquisition() {
        let events = std::cell::RefCell::new(Vec::new());
        let result: io::Result<()> = setup_terminal_resources(
            || {
                events.borrow_mut().push("enable_raw");
                Ok(())
            },
            || {
                events.borrow_mut().push("enter_alt");
                Ok(())
            },
            || {
                events.borrow_mut().push("construct");
                Err(io::Error::other("construct failed"))
            },
            || {
                events.borrow_mut().push("leave_alt");
                Ok(())
            },
            || {
                events.borrow_mut().push("disable_raw");
                Ok(())
            },
        );

        assert!(result.is_err());
        assert_eq!(
            events.into_inner(),
            vec![
                "enable_raw",
                "enter_alt",
                "construct",
                "leave_alt",
                "disable_raw"
            ]
        );
    }

    #[test]
    fn terminal_setup_rolls_back_when_entering_alternate_screen_fails() {
        let events = std::cell::RefCell::new(Vec::new());
        let result: io::Result<()> = setup_terminal_resources(
            || {
                events.borrow_mut().push("enable_raw");
                Ok(())
            },
            || {
                events.borrow_mut().push("enter_alt");
                Err(io::Error::other("enter failed"))
            },
            || {
                events.borrow_mut().push("construct");
                Ok(())
            },
            || {
                events.borrow_mut().push("leave_alt");
                Ok(())
            },
            || {
                events.borrow_mut().push("disable_raw");
                Ok(())
            },
        );

        assert!(result.is_err());
        assert_eq!(
            events.into_inner(),
            vec!["enable_raw", "enter_alt", "leave_alt", "disable_raw"]
        );
    }

    #[test]
    fn terminal_restore_attempts_every_resource_after_an_earlier_failure() {
        let events = std::cell::RefCell::new(Vec::new());
        let result = restore_terminal_resources(
            &mut (),
            || {
                events.borrow_mut().push("disable_raw");
                Err(io::Error::other("disable failed"))
            },
            |_| {
                events.borrow_mut().push("leave_alt");
                Err(io::Error::other("leave failed"))
            },
            |_| {
                events.borrow_mut().push("show_cursor");
                Ok(())
            },
        );

        assert_eq!(
            result
                .expect_err("first cleanup error must be returned")
                .to_string(),
            "disable failed"
        );
        assert_eq!(
            events.into_inner(),
            vec!["disable_raw", "leave_alt", "show_cursor"]
        );
    }
}

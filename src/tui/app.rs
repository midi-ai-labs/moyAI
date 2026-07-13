use std::fs;
use std::io::{self, Stdout};
use std::process::Command as ProcessCommand;
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

use crate::app::{App, AppBootstrap, AppCommand, ReviewRequest, RunRequest, SessionSteerRequest};
use crate::cli::{
    ConfirmationPrompt, EventRenderer, OutputMode, SharedConfirmationPrompt, TuiArgs,
};
use crate::config::merge::apply_patch as apply_config_patch;
use crate::config::model::full_effective_override;
use crate::config::{ConfigLoader, ResolvedConfig, ShellFamily};
use crate::error::{AppRunError, CliPromptError, CliRenderError};
use crate::runtime::{LiveConfigOverrides, SystemClock, build_cancel_token};
use crate::session::markdown::{history_items_to_markdown, history_markdown_file_name};
use crate::session::{
    EditorContext, LoadedSessionStatus, LoadedSessionSummary, PromptDispatchPart, RunEvent,
    RunSummary, SessionId, SessionRecord, SessionStateSnapshot, TodoItem, TodoStatus,
};
use crate::tool::PermissionRequest;
use crate::workspace::project::normalize_path;

use super::config_editor::{ConfigEditorState, ConfigSaveScope};
use super::prompt_enhance::enhance_prompt;
use super::query::{latest_session, recent_sessions, search_sessions, session_view};
use super::reducer::reduce_run_event;
use super::state::{
    AppState, Modal, PromptReviewPhase, Route, RunStatus, TranscriptEntry, TranscriptKind,
};

type TerminalHandle = Terminal<CrosstermBackend<Stdout>>;

struct TuiRootRun {
    generation: u64,
    cancel: CancellationToken,
}

#[derive(Default)]
struct TuiRootRunLifecycle {
    active: Option<TuiRootRun>,
}

impl TuiRootRunLifecycle {
    fn begin(&mut self, generation: u64, cancel: CancellationToken) -> bool {
        if self.active.is_some() {
            return false;
        }
        self.active = Some(TuiRootRun { generation, cancel });
        true
    }

    fn is_active(&self) -> bool {
        self.active.is_some()
    }

    fn request_cancel(&self) -> bool {
        let Some(active) = self.active.as_ref() else {
            return false;
        };
        active.cancel.cancel();
        true
    }

    fn finish(&mut self, generation: u64) -> Option<bool> {
        if self
            .active
            .as_ref()
            .is_none_or(|active| active.generation != generation)
        {
            return None;
        }
        self.active
            .take()
            .map(|active| active.cancel.is_cancelled())
    }
}

fn commit_tui_effective_config(
    effective_config: &mut ResolvedConfig,
    live_config: &LiveConfigOverrides,
    candidate: ResolvedConfig,
    durable_access_ready: bool,
) -> bool {
    if !durable_access_ready {
        return false;
    }
    let access_mode = candidate.permissions.access_mode;
    *effective_config = candidate;
    live_config.set_access_mode(access_mode);
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

struct TuiController {
    app: App,
    state: AppState,
    composer: TextArea<'static>,
    review_editor: TextArea<'static>,
    workspace_picker: TextArea<'static>,
    config_editor: ConfigEditorState,
    base_config: ResolvedConfig,
    effective_config: ResolvedConfig,
    live_config: LiveConfigOverrides,
    root_run_lifecycle: TuiRootRunLifecycle,
    next_root_run_generation: u64,
    runtime_tx: tokio::sync::mpsc::UnboundedSender<RuntimeMessage>,
    runtime_rx: tokio::sync::mpsc::UnboundedReceiver<RuntimeMessage>,
    permission_response: Option<mpsc::Sender<bool>>,
    pending_permission_request: Option<PermissionRequest>,
    pending_permission_request_id: Option<u64>,
    next_permission_request_id: Arc<AtomicU64>,
    preview_entries: Vec<TranscriptEntry>,
    preview_todos: Vec<TodoItem>,
    preview_state: Option<SessionStateSnapshot>,
    preview_turn_offset: usize,
    preview_turn_limit: usize,
    preview_turn_total: usize,
    preview_turn_has_more: bool,
    next_enhance_request_id: u64,
    should_quit: bool,
    started_at: Instant,
}

impl TuiController {
    async fn new(app: App, args: TuiArgs) -> Result<Self, AppRunError> {
        let (runtime_tx, runtime_rx) = tokio::sync::mpsc::unbounded_channel();
        let sessions = recent_sessions(&app.session_service, app.workspace.project_id, 20).await?;
        let base_config = app.config.clone();
        let effective_config = base_config.clone();
        let live_config = LiveConfigOverrides::new(effective_config.permissions.access_mode);
        let mut controller = Self {
            app,
            state: AppState::default(),
            composer: build_composer(),
            review_editor: build_composer(),
            workspace_picker: build_composer(),
            config_editor: ConfigEditorState::from_config(&effective_config),
            base_config,
            effective_config,
            live_config,
            root_run_lifecycle: TuiRootRunLifecycle::default(),
            next_root_run_generation: 1,
            runtime_tx,
            runtime_rx,
            permission_response: None,
            pending_permission_request: None,
            pending_permission_request_id: None,
            next_permission_request_id: Arc::new(AtomicU64::new(1)),
            preview_entries: Vec::new(),
            preview_todos: Vec::new(),
            preview_state: None,
            preview_turn_offset: 0,
            preview_turn_limit: 80,
            preview_turn_total: 0,
            preview_turn_has_more: false,
            next_enhance_request_id: 1,
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
            && (matches!(
                self.state.run_status,
                RunStatus::Running | RunStatus::Confirming
            ) || self.agent_tree_active())
        {
            return self.stop_current_run().await;
        }
        if self.state.permission.is_some() {
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
                    && self.state.run_status != RunStatus::Running
                    && self.state.run_status != RunStatus::Confirming =>
            {
                self.should_quit = true;
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
            KeyCode::F(4)
                if self.state.run_status != RunStatus::Running
                    && self.state.run_status != RunStatus::Confirming =>
            {
                self.open_workspace_picker();
            }
            KeyCode::F(6) if self.state.run_status != RunStatus::Running => {
                if self.state.route != Route::History {
                    self.start_prompt_enhance().await?;
                }
            }
            KeyCode::F(7)
                if self.state.run_status != RunStatus::Running
                    && self.state.run_status != RunStatus::Confirming
                    && self.state.route != Route::History =>
            {
                self.start_uncommitted_review().await?;
            }
            KeyCode::F(8)
                if self.state.run_status != RunStatus::Running
                    && self.state.run_status != RunStatus::Confirming =>
            {
                self.toggle_access_mode().await?;
            }
            KeyCode::F(9)
                if self.state.run_status != RunStatus::Running
                    && self.state.run_status != RunStatus::Confirming =>
            {
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
                    && self.state.run_status != RunStatus::Running
                    && self.state.run_status != RunStatus::Confirming =>
            {
                self.archive_selected_session(true).await?;
            }
            KeyCode::Char('u')
                if self.state.route == Route::History
                    && self.state.run_status != RunStatus::Running
                    && self.state.run_status != RunStatus::Confirming =>
            {
                self.archive_selected_session(false).await?;
            }
            KeyCode::Char('r')
                if self.state.route == Route::History
                    && self.state.run_status != RunStatus::Running
                    && self.state.run_status != RunStatus::Confirming =>
            {
                self.rejoin_selected_session().await?;
            }
            KeyCode::Char('z')
                if self.state.route == Route::History
                    && self.state.run_status != RunStatus::Running
                    && self.state.run_status != RunStatus::Confirming =>
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
                    self.composer.insert_newline();
                }
            }
            _ => {
                if self.state.route != Route::History {
                    let _ = self.composer.input(key);
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
        let Some(prompt_review) = self.state.prompt_review.clone() else {
            self.state.modal = Modal::None;
            return Ok(());
        };
        if prompt_review.phase == PromptReviewPhase::Enhancing {
            if key.code == KeyCode::Esc {
                self.state.cancel_prompt_review();
                self.state.status_message = Some("cancelled prompt enhancement".to_string());
            }
            return Ok(());
        }

        match key.code {
            KeyCode::Esc => {
                self.state.cancel_prompt_review();
                self.state.status_message = Some("kept raw prompt in composer".to_string());
            }
            KeyCode::F(6) => {
                let Some(prompt_dispatch) = self.state.build_prompt_dispatch(true) else {
                    return Err(AppRunError::Message(
                        "enhanced draft is not ready yet".to_string(),
                    ));
                };
                self.state.cancel_prompt_review();
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
                self.state.cancel_prompt_review();
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
                let patch = self
                    .config_editor
                    .build_session_override()
                    .map_err(AppRunError::Message)?;
                let candidate = apply_config_patch(self.base_config.clone(), patch);
                let durable_access_ready = self
                    .persist_current_session_access_mode(candidate.permissions.access_mode)
                    .await;
                if !commit_tui_effective_config(
                    &mut self.effective_config,
                    &self.live_config,
                    candidate,
                    durable_access_ready,
                ) {
                    return Ok(());
                }
                self.state.status_message = Some(if self.state.current_session_id.is_some() {
                    "applied session override and remembered access mode for this session"
                        .to_string()
                } else {
                    "applied temporary session override".to_string()
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
        match key.code {
            KeyCode::Char('a') => self.answer_permission(true)?,
            KeyCode::Char('d') | KeyCode::Esc => self.answer_permission(false)?,
            _ => {}
        }
        Ok(())
    }

    fn answer_permission(&mut self, allow: bool) -> Result<(), AppRunError> {
        let request_already_ended = self
            .permission_response
            .take()
            .is_some_and(|response| response.send(allow).is_err());
        self.pending_permission_request = None;
        self.pending_permission_request_id = None;
        self.state.clear_permission();
        if request_already_ended {
            self.state.status_message =
                Some("permission request already ended before the response".to_string());
        }
        Ok(())
    }

    async fn stop_current_run(&mut self) -> Result<(), AppRunError> {
        if self.state.permission.is_some() {
            self.answer_permission(false)?;
        }
        let root_cancelled = self.root_run_lifecycle.request_cancel();
        let Some(session_id) = self.state.current_session_id else {
            if root_cancelled {
                self.state.run_status = RunStatus::Cancelled;
                self.state.status_message =
                    Some("stop requested before run admission completed".to_string());
                return Ok(());
            }
            self.state.status_message = Some("no active session to stop".to_string());
            return Ok(());
        };
        let tree_cancelled = self.app.run_service.cancel_agent_tree(session_id);
        match self
            .app
            .session_service
            .interrupt_running_session(session_id, "Stopped from TUI.".to_string())
            .await
        {
            Ok(_) => {
                self.state.run_status = RunStatus::Cancelled;
                self.state.status_message = Some("stop requested for active run".to_string());
            }
            Err(error) => {
                if root_cancelled || tree_cancelled {
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
                "session access mode set to {} and remembered for this session",
                access_mode.label()
            )
        } else {
            format!(
                "default access mode set to {} and remembered globally",
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

    fn sync_live_access_mode(&self) {
        self.live_config
            .set_access_mode(self.effective_config.permissions.access_mode);
    }

    fn apply_access_mode_owner(&mut self, access_mode: crate::config::AccessMode) {
        self.effective_config.permissions.access_mode = access_mode;
        self.sync_live_access_mode();
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
        self.app = app;
        self.base_config = self.app.config.clone();
        self.effective_config = self.base_config.clone();
        self.live_config = LiveConfigOverrides::new(self.effective_config.permissions.access_mode);
        self.root_run_lifecycle = TuiRootRunLifecycle::default();
        self.config_editor = ConfigEditorState::from_config(&self.effective_config);
        self.state = AppState::default();
        self.composer = build_composer();
        self.review_editor = build_composer();
        self.workspace_picker = build_composer();
        self.permission_response = None;
        self.pending_permission_request = None;
        self.pending_permission_request_id = None;
        self.preview_entries.clear();
        self.preview_todos.clear();
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
        let mut command = if cfg!(target_os = "windows") {
            ProcessCommand::new("explorer")
        } else if cfg!(target_os = "macos") {
            ProcessCommand::new("open")
        } else {
            ProcessCommand::new("xdg-open")
        };
        match command.arg(path.as_str()).spawn() {
            Ok(_) => {
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
        let request_id = self.next_enhance_request_id;
        self.next_enhance_request_id += 1;
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
                enhance_prompt(&config, &raw_prompt)
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
            self.launch_active_turn_steer(prompt, prompt_dispatch)
                .await?;
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
        let cancel = build_cancel_token();
        if !self
            .root_run_lifecycle
            .begin(run_generation, cancel.clone())
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
            model: self.effective_config.model.model.clone(),
            base_url: self.effective_config.model.base_url.clone(),
            config_override: Some(full_effective_override(&self.effective_config)),
            output_mode: OutputMode::Human,
            show_reasoning: true,
            prompt_dispatch: Some(prompt_dispatch.clone()),
            editor_context: Some(self.current_editor_context()),
            review_request,
            image_paths: Vec::new(),
            cancel,
            live_config: Some(self.live_config.clone()),
            agent_confirmation: None,
            agent_context: None,
        };
        self.state.push_local_prompt_dispatch(&prompt_dispatch);
        self.composer = build_composer();
        self.review_editor = build_composer();
        let run_service = self.app.run_service.clone();
        let runtime_tx = self.runtime_tx.clone();
        let next_permission_request_id = self.next_permission_request_id.clone();
        std::thread::spawn(move || {
            let mut request = request;
            let mut renderer = TuiRenderer {
                tx: runtime_tx.clone(),
            };
            let mut prompt = SharedConfirmationPrompt::new(TuiConfirmationPrompt {
                tx: runtime_tx.clone(),
                next_permission_request_id,
            });
            request.agent_confirmation = Some(prompt.clone());
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build tui worker runtime");
            runtime.block_on(async move {
                let result = run_service
                    .execute(AppCommand::Run(request), &mut renderer, &mut prompt)
                    .await
                    .map_err(|error| error.to_string());
                publish_tui_run_finished(&runtime_tx, run_generation, result);
            });
        });
        Ok(())
    }

    async fn launch_active_turn_steer(
        &mut self,
        prompt: String,
        prompt_dispatch: PromptDispatchPart,
    ) -> Result<(), AppRunError> {
        let Some(session_id) = self.state.current_session_id else {
            self.state.status_message =
                Some("running session is not available for steer".to_string());
            return Ok(());
        };
        self.state.push_local_prompt_dispatch(&prompt_dispatch);
        self.composer = build_composer();
        self.review_editor = build_composer();
        self.state.status_message = Some("stored steer input for the active turn".to_string());
        let run_service = self.app.run_service.clone();
        let runtime_tx = self.runtime_tx.clone();
        let next_permission_request_id = self.next_permission_request_id.clone();
        let cwd = self.app.workspace.cwd.clone();
        std::thread::spawn(move || {
            let mut renderer = TuiRenderer {
                tx: runtime_tx.clone(),
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
                                client_user_message_id: Some(format!(
                                    "tui-steer-{}",
                                    SystemClock::now_ms()
                                )),
                            }),
                            &mut renderer,
                            &mut prompt_ui,
                        )
                        .await
                })
                .map(|_| ())
                .map_err(|error| error.to_string());
            let _ = runtime_tx.send(RuntimeMessage::SteerStored(result));
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
        let mut files = Vec::new();
        if let Some(state) = self.state.session_state.as_ref() {
            files.extend(state.active_targets.iter().cloned());
        }
        if let Some(state) = self.preview_state.as_ref() {
            files.extend(state.active_targets.iter().cloned());
        }
        files.sort();
        files.dedup();
        files
    }

    async fn drain_runtime_messages(&mut self) -> Result<(), AppRunError> {
        while let Ok(message) = self.runtime_rx.try_recv() {
            match message {
                RuntimeMessage::RunEvent(event) => {
                    let live_refresh_session_id =
                        event.session_id().or(self.state.current_session_id);
                    reduce_run_event(&mut self.state, &event);
                    if live_event_requires_canonical_refresh(&event) {
                        if let Some(session_id) = live_refresh_session_id {
                            self.refresh_loaded_summary_for_session(session_id).await?;
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
                    if event_requires_sidebar_refresh(&event) {
                        self.refresh_current_session_todos().await?;
                    }
                }
                RuntimeMessage::Finished {
                    run_generation,
                    result,
                } => {
                    let Some(cancelled) = self.root_run_lifecycle.finish(run_generation) else {
                        continue;
                    };
                    match result {
                        Ok(summary) => {
                            let child_permission = self
                                .pending_permission_request
                                .as_ref()
                                .filter(|request| request.agent_path.is_some())
                                .cloned();
                            self.state.set_summary(summary);
                            self.refresh_sessions().await?;
                            if let Some(session_id) = self.state.current_session_id {
                                self.open_session(session_id).await?;
                            }
                            if let Some(request) = child_permission {
                                self.state.set_permission(&request);
                            }
                        }
                        Err(message) => {
                            self.state.run_status = tui_terminal_error_status(cancelled);
                            self.state.status_message = Some(if cancelled {
                                "run cancelled by user".to_string()
                            } else {
                                message
                            });
                        }
                    }
                }
                RuntimeMessage::Permission {
                    confirmation_id,
                    request,
                    response,
                } => {
                    self.permission_response = Some(response);
                    self.pending_permission_request = Some(request.clone());
                    self.pending_permission_request_id = Some(confirmation_id);
                    self.state.set_permission(&request);
                }
                RuntimeMessage::PermissionCancelled { confirmation_id } => {
                    if clear_cancelled_tui_permission(
                        &mut self.pending_permission_request,
                        &mut self.pending_permission_request_id,
                        &mut self.permission_response,
                        confirmation_id,
                    ) {
                        self.state.clear_permission();
                    }
                }
                RuntimeMessage::SteerStored(result) => match result {
                    Ok(()) => {
                        self.state.status_message =
                            Some("stored steer input for the active turn".to_string());
                    }
                    Err(message) => {
                        self.state.status_message =
                            Some(format!("failed to store steer input: {message}"));
                    }
                },
                RuntimeMessage::EnhanceFinished { request_id, result } => match result {
                    Ok(draft) => {
                        if self.state.finish_prompt_enhance(request_id, draft.clone()) {
                            self.review_editor = build_composer();
                            self.review_editor.insert_str(&draft);
                            self.state
                                .update_prompt_review_draft(textarea_value(&self.review_editor));
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
                },
            }
        }
        Ok(())
    }

    async fn open_session(&mut self, session_id: SessionId) -> Result<(), AppRunError> {
        let view = session_view(&self.app.session_service, session_id).await?;
        self.apply_access_mode_owner(view.session.access_mode);
        self.state
            .load_turn_items(&view.session, &view.turn_items, view.state, view.todos);
        self.state.modal = Modal::None;
        Ok(())
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
        let rejoin = self
            .app
            .session_service
            .rejoin_running_session(session_id, 0, 200, 0, 500)
            .await?;
        let todos = self.app.session_service.list_todos(session_id).await?;
        self.apply_access_mode_owner(rejoin.read.session.access_mode);
        self.state.load_turn_items(
            &rejoin.read.session,
            &rejoin.read.turns.items,
            rejoin.read.state,
            todos,
        );
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
        let session = self.app.session_service.get_session(session_id).await?;
        let history_items = self
            .app
            .session_service
            .canonical_history_items(session_id)
            .await?;
        if history_items.is_empty() {
            self.state.status_message = Some("session has no history to export".to_string());
            return Ok(());
        }

        let file_name = history_markdown_file_name(&session.title, session_id);
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
        let markdown = history_items_to_markdown(&session, &history_items);
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
            self.state.sidebar_todos.clear();
            self.state.session_state = None;
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
        self.preview_todos.clear();
        self.preview_state = None;
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
            self.preview_todos = self.app.session_service.list_todos(session.id).await?;
            self.preview_state = Some(self.app.session_service.load_state(session.id).await?);
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

    async fn refresh_current_session_todos(&mut self) -> Result<(), AppRunError> {
        if let Some(session_id) = self.state.current_session_id {
            self.state
                .set_sidebar_todos(self.app.session_service.list_todos(session_id).await?);
        }
        Ok(())
    }

    async fn reload_config(&mut self) -> Result<(), AppRunError> {
        self.base_config = ConfigLoader::load(&self.app.workspace.root, None)
            .map_err(|error| AppRunError::Message(format!("failed to reload config: {error}")))?;
        self.effective_config = self.base_config.clone();
        self.sync_live_access_mode();
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
        if self.state.permission.is_some() {
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
        let todo_lines = render_todo_lines(self.sidebar_todos(), self.state.session_state.as_ref());
        frame.render_widget(
            Paragraph::new(Text::from(todo_lines))
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Todo Progress"),
                ),
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

    fn sidebar_todos(&self) -> &[TodoItem] {
        if self.state.current_session_id.is_none() || self.state.route == Route::History {
            &self.preview_todos
        } else {
            &self.state.sidebar_todos
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
            RunStatus::Confirming => (
                format!(
                    "status=[{}] confirming",
                    self.spinner_frame(&[".", "o", "O", "o"])
                ),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            RunStatus::Completed => (
                "status=completed".to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            RunStatus::AwaitingUser => (
                "status=awaiting_user".to_string(),
                Style::default()
                    .fg(Color::LightYellow)
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
            RunStatus::Confirming => Some(format!(
                "activity=[{}] {}",
                self.spinner_frame(&[".", "o", "O", "o"]),
                self.state
                    .status_message
                    .clone()
                    .unwrap_or_else(|| "waiting for confirmation".to_string())
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
        if let Some(state) = self.preview_state.as_ref() {
            lines.push(Line::from(format!(
                "route={}",
                task_route_label(state.route)
            )));
            lines.push(Line::from(format!(
                "phase={}",
                process_phase_label(state.process_phase)
            )));
            lines.push(Line::from(format!(
                "open_work_count={}",
                state.completion.open_work_count
            )));
            if let Some(reason) = state.completion.blocked_reason.as_ref() {
                lines.push(Line::from(format!(
                    "blocked={}",
                    truncate_middle(reason, 44)
                )));
            }
            if let Some(summary) = state.completion.route_contract_summary.as_ref() {
                lines.push(Line::from(format!(
                    "docs_contract={}",
                    truncate_middle(summary, 44)
                )));
            }
        }
        if let Some(next_action) = self.preview_handoff_next_action() {
            lines.push(Line::from(format!(
                "next={}",
                truncate_middle(&next_action, 44)
            )));
        }
        let todo_lines = render_todo_lines(&self.preview_todos, self.preview_state.as_ref());
        if !todo_lines.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from("todo:"));
            lines.extend(todo_lines.into_iter().take(4));
        }
        lines
    }

    fn preview_handoff_next_action(&self) -> Option<String> {
        self.preview_state
            .as_ref()
            .and_then(|state| state.implementation_handoff.as_ref())
            .and_then(|handoff| handoff.next_actions.first().cloned())
            .or_else(|| preview_handoff_next_action(&self.preview_entries))
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
        if let Some(permission) = &self.state.permission {
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
                        permission.targets.join(", ")
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
                        permission.risks.join(", ")
                    }
                )),
                Line::from(format!(
                    "Access mode: {}",
                    self.effective_config.permissions.access_mode.as_str()
                )),
                Line::from(""),
                Line::from("a = allow once"),
                Line::from("d / Esc = reject"),
                Line::from("Ctrl+X = stop active run"),
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

#[derive(Debug)]
enum RuntimeMessage {
    RunEvent(RunEvent),
    Finished {
        run_generation: u64,
        result: Result<RunSummary, String>,
    },
    Permission {
        confirmation_id: u64,
        request: PermissionRequest,
        response: mpsc::Sender<bool>,
    },
    PermissionCancelled {
        confirmation_id: u64,
    },
    SteerStored(Result<(), String>),
    EnhanceFinished {
        request_id: u64,
        result: Result<String, String>,
    },
}

fn live_event_requires_canonical_refresh(event: &RunEvent) -> bool {
    matches!(
        event,
        RunEvent::UserTurnStored { .. }
            | RunEvent::ControlEnvelopePrepared { .. }
            | RunEvent::ModelRequestPrepared { .. }
            | RunEvent::WorldStateUpdated { .. }
            | RunEvent::ToolCallPending { .. }
            | RunEvent::ToolCallCompleted { .. }
            | RunEvent::ToolCallFailed { .. }
            | RunEvent::ToolProposalRejected { .. }
            | RunEvent::CandidateRepairEditRecorded { .. }
            | RunEvent::FileChangesRecorded { .. }
            | RunEvent::CompactionCompleted { .. }
            | RunEvent::PermissionRequested { .. }
            | RunEvent::PermissionResolved { .. }
            | RunEvent::RetryScheduled { .. }
            | RunEvent::RecoverableRuntimeFeedback { .. }
            | RunEvent::StateUpdated { .. }
            | RunEvent::LifecycleGuardUpdated { .. }
    )
}

struct TuiRenderer {
    tx: tokio::sync::mpsc::UnboundedSender<RuntimeMessage>,
}

impl EventRenderer for TuiRenderer {
    fn render(&mut self, event: &RunEvent) -> Result<(), CliRenderError> {
        self.tx
            .send(RuntimeMessage::RunEvent(event.clone()))
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

    fn render_session_show(
        &mut self,
        _transcript: &crate::session::Transcript,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }

    fn render_session_history_items(
        &mut self,
        _session: &SessionRecord,
        _history_items: &[crate::protocol::HistoryItem],
        _show_reasoning: bool,
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

    fn render_session_compact_result(
        &mut self,
        _result: &crate::session::SessionCompactResult,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }

    fn render_session_memory_mode_update(
        &mut self,
        _update: &crate::session::SessionMemoryModeUpdate,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
}

fn publish_tui_run_finished(
    tx: &tokio::sync::mpsc::UnboundedSender<RuntimeMessage>,
    run_generation: u64,
    result: Result<RunSummary, String>,
) {
    let _ = tx.send(RuntimeMessage::Finished {
        run_generation,
        result,
    });
}

fn tui_terminal_error_status(cancelled: bool) -> RunStatus {
    if cancelled {
        RunStatus::Cancelled
    } else {
        RunStatus::Failed
    }
}

struct TuiConfirmationPrompt {
    tx: tokio::sync::mpsc::UnboundedSender<RuntimeMessage>,
    next_permission_request_id: Arc<AtomicU64>,
}

impl ConfirmationPrompt for TuiConfirmationPrompt {
    fn confirm(&mut self, request: &PermissionRequest) -> Result<bool, CliPromptError> {
        self.confirm_with_cancel(request, &CancellationToken::new())
    }

    fn confirm_with_cancel(
        &mut self,
        request: &PermissionRequest,
        cancel: &CancellationToken,
    ) -> Result<bool, CliPromptError> {
        if cancel.is_cancelled() {
            return Ok(false);
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
            })
            .map_err(|error| CliPromptError::Message(error.to_string()))?;
        loop {
            match response_rx.recv_timeout(Duration::from_millis(25)) {
                Ok(allow) => return Ok(allow),
                Err(mpsc::RecvTimeoutError::Timeout) if cancel.is_cancelled() => {
                    let _ = self
                        .tx
                        .send(RuntimeMessage::PermissionCancelled { confirmation_id });
                    return Ok(false);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(CliPromptError::Message(
                        "TUI permission response channel disconnected".to_string(),
                    ));
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
    request: &mut Option<PermissionRequest>,
    pending_confirmation_id: &mut Option<u64>,
    response: &mut Option<mpsc::Sender<bool>>,
    expected_confirmation_id: u64,
) -> bool {
    if *pending_confirmation_id != Some(expected_confirmation_id) {
        return false;
    }
    *request = None;
    *pending_confirmation_id = None;
    *response = None;
    true
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
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()
}

fn build_composer() -> TextArea<'static> {
    let mut textarea = TextArea::default();
    textarea.set_cursor_line_style(Style::default());
    textarea
}

fn is_stop_key(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('x') && key.modifiers.contains(KeyModifiers::CONTROL)
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
    status != RunStatus::Confirming && !(route == Route::History && status == RunStatus::Running)
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
        TranscriptKind::Reasoning => Style::default().fg(Color::Yellow),
        TranscriptKind::Editing => Style::default().fg(Color::Yellow),
        TranscriptKind::Tool => Style::default().fg(Color::Magenta),
        TranscriptKind::CommandSummary => Style::default().fg(Color::Magenta),
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

fn task_route_label(route: crate::session::TaskRoute) -> &'static str {
    match route {
        crate::session::TaskRoute::Code => "code",
        crate::session::TaskRoute::Docs => "docs",
        crate::session::TaskRoute::Review => "review",
        crate::session::TaskRoute::Debug => "debug",
        crate::session::TaskRoute::Ask => "ask",
        crate::session::TaskRoute::Summary => "summary",
    }
}

fn process_phase_label(phase: crate::session::ProcessPhase) -> &'static str {
    match phase {
        crate::session::ProcessPhase::Discover => "discover",
        crate::session::ProcessPhase::Author => "author",
        crate::session::ProcessPhase::Verify => "verify",
        crate::session::ProcessPhase::Repair => "repair",
        crate::session::ProcessPhase::Closeout => "closeout",
    }
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

fn preview_handoff_next_action(entries: &[TranscriptEntry]) -> Option<String> {
    entries.iter().rev().find_map(|entry| {
        (entry.kind == TranscriptKind::Assistant)
            .then(|| extract_handoff_section_value(&entry.body, "次にやること"))
            .flatten()
    })
}

fn extract_handoff_section_value(body: &str, heading: &str) -> Option<String> {
    let lines = body.lines().collect::<Vec<_>>();
    for (index, raw_line) in lines.iter().enumerate() {
        let line = raw_line.trim();
        if let Some(value) = strip_handoff_heading_prefix(line, heading) {
            return Some(value.to_string());
        }
        if line == heading {
            let mut collected = Vec::new();
            for next_line in lines.iter().skip(index + 1) {
                let trimmed = next_line.trim();
                if trimmed.is_empty() {
                    if !collected.is_empty() {
                        break;
                    }
                    continue;
                }
                if is_handoff_heading(trimmed) {
                    break;
                }
                collected.push(trimmed.to_string());
            }
            if !collected.is_empty() {
                return Some(collected.join(" "));
            }
        }
    }
    None
}

fn strip_handoff_heading_prefix<'a>(line: &'a str, heading: &str) -> Option<&'a str> {
    [":", "："].into_iter().find_map(|separator| {
        line.strip_prefix(heading)
            .and_then(|rest| rest.strip_prefix(separator))
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

fn is_handoff_heading(line: &str) -> bool {
    ["完了したこと", "未完了", "次にやること"]
        .into_iter()
        .any(|heading| line == heading || strip_handoff_heading_prefix(line, heading).is_some())
}

fn render_todo_lines(
    todos: &[TodoItem],
    state: Option<&SessionStateSnapshot>,
) -> Vec<Line<'static>> {
    if todos.is_empty() {
        return vec![Line::from("No progress checklist yet.")];
    }

    let total = todos.len();
    let completed = todos
        .iter()
        .filter(|todo| todo.status == TodoStatus::Completed)
        .count();
    let blocked = todos
        .iter()
        .filter(|todo| todo.status == TodoStatus::Blocked)
        .count();
    let open = todos.iter().filter(|todo| todo.status.is_open()).count();
    let active_todo_id = state.and_then(|value| value.active_todo_id);

    let mut lines = vec![Line::from(format!(
        "completed={completed}/{total}  open={open}  blocked={blocked}"
    ))];

    if let Some(active_todo_id) = active_todo_id {
        if let Some(active) = todos.iter().find(|todo| todo.id == active_todo_id) {
            lines.push(Line::from(format!(
                "active={} {}",
                todo_status_marker(active.status, true),
                truncate_middle(&active.content, 42)
            )));
            if let Some(state) = state {
                if !state.active_targets.is_empty() {
                    let targets = state
                        .active_targets
                        .iter()
                        .map(|value| value.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    lines.push(Line::from(format!(
                        "targets={}",
                        truncate_middle(&targets, 42)
                    )));
                }
            }
        }
    }

    let mut ordered = todos.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|todo| todo_sort_key(todo, active_todo_id));
    for todo in ordered.into_iter().take(5) {
        lines.push(Line::from(format!(
            "{} {} [{}]",
            todo_status_marker(todo.status, active_todo_id == Some(todo.id)),
            truncate_middle(&todo.content, 36),
            format!("{:?}", todo.kind).to_lowercase()
        )));
        if todo.status == TodoStatus::Blocked && !todo.blocked_by.is_empty() {
            lines.push(Line::from(format!(
                "blocked={}",
                truncate_middle(&todo.blocked_by.join(", "), 42)
            )));
        }
    }

    if let Some(state) = state {
        lines.extend(render_docs_route_contract_lines(state));
    }

    lines
}

fn render_docs_route_contract_lines(state: &SessionStateSnapshot) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let Some(docs) = state.docs_route.as_ref() else {
        return lines;
    };
    if let Some(summary) = state.completion.route_contract_summary.as_ref() {
        lines.push(Line::from(format!(
            "docs_contract={}",
            truncate_middle(summary, 42)
        )));
    }
    if let Some(missing_area) = docs
        .area_coverage
        .iter()
        .find(|coverage| coverage.status == crate::session::ContractStatus::Pending)
    {
        let suffix = missing_area
            .representative_paths
            .first()
            .map(|path| format!(" ({})", truncate_middle(path.as_str(), 24)))
            .unwrap_or_default();
        lines.push(Line::from(format!(
            "missing_area={}{}",
            docs_area_label(missing_area.area),
            suffix
        )));
    }
    if let Some(missing_fact) = docs
        .factual_checks
        .iter()
        .find(|check| check.status == crate::session::ContractStatus::Pending)
    {
        lines.push(Line::from(format!(
            "pending_fact={}",
            truncate_middle(&missing_fact.subject, 32)
        )));
    }
    lines
}

fn docs_area_label(area: crate::session::DocsArea) -> &'static str {
    match area {
        crate::session::DocsArea::Backend => "backend",
        crate::session::DocsArea::Frontend => "frontend",
        crate::session::DocsArea::Tests => "tests",
        crate::session::DocsArea::Data => "data",
        crate::session::DocsArea::Examples => "examples",
    }
}

fn todo_sort_key(todo: &TodoItem, active_todo_id: Option<crate::session::TodoId>) -> (u8, u8) {
    let active_rank = if active_todo_id == Some(todo.id) {
        0
    } else {
        1
    };
    let status_rank = match todo.status {
        TodoStatus::InProgress => 0,
        TodoStatus::Blocked => 1,
        TodoStatus::Pending => 2,
        TodoStatus::Completed => 3,
        TodoStatus::Cancelled => 4,
    };
    (active_rank, status_rank)
}

fn todo_status_marker(status: TodoStatus, active: bool) -> &'static str {
    if active {
        return "[>]";
    }
    match status {
        TodoStatus::Pending => "[ ]",
        TodoStatus::InProgress => "[~]",
        TodoStatus::Blocked => "[!]",
        TodoStatus::Completed => "[x]",
        TodoStatus::Cancelled => "[-]",
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

fn event_requires_sidebar_refresh(event: &RunEvent) -> bool {
    !matches!(
        event,
        RunEvent::TextDelta { .. }
            | RunEvent::ReasoningDelta { .. }
            | RunEvent::WorldStateUpdated { .. }
            | RunEvent::ToolProposalRejected { .. }
            | RunEvent::CandidateRepairEditRecorded { .. }
            | RunEvent::PermissionRequested { .. }
            | RunEvent::RetryScheduled { .. }
    )
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
        let app = AppBootstrap::rebuild_for_directory_as_workspace_root(&root, store)
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
        apply_config_patch(
            controller.base_config.clone(),
            full_effective_override(&controller.effective_config),
        )
    }

    fn append_tui_user_history(controller: &TuiController, session_id: SessionId, text: &str) {
        use crate::protocol::ProtocolEventStore as _;

        controller
            .app
            .store
            .protocol_event_store()
            .append_history_item(&crate::protocol::HistoryItem {
                id: crate::protocol::HistoryItemId::new(),
                session_id,
                turn_id: crate::protocol::TurnId::new(),
                sequence_no: 1,
                created_at_ms: 1,
                payload: crate::protocol::HistoryItemPayload::Message {
                    message_id: None,
                    role: crate::session::MessageRole::User,
                    content: vec![crate::protocol::ContentPart::Text {
                        text: text.to_string(),
                    }],
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

    fn recv_tui_runtime_message(
        receiver: &mut tokio::sync::mpsc::UnboundedReceiver<RuntimeMessage>,
    ) -> RuntimeMessage {
        for _ in 0..200 {
            match receiver.try_recv() {
                Ok(message) => return message,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
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
        assert!(!ctrl_enter_available(Route::Session, RunStatus::Confirming));
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
        let token = CancellationToken::new();
        let mut lifecycle = TuiRootRunLifecycle::default();
        assert!(lifecycle.begin(7, token.clone()));
        assert!(lifecycle.is_active());

        assert!(lifecycle.request_cancel());
        assert!(token.is_cancelled());
        assert!(
            !lifecycle.begin(8, CancellationToken::new()),
            "a cancelled run remains the owner until its matching finish"
        );
        assert_eq!(lifecycle.finish(6), None);
        assert!(lifecycle.is_active());
        assert_eq!(lifecycle.finish(7), Some(true));
        assert!(!lifecycle.is_active());
        assert!(!lifecycle.request_cancel());
        assert_eq!(tui_terminal_error_status(true), RunStatus::Cancelled);
        assert_eq!(tui_terminal_error_status(false), RunStatus::Failed);
    }

    #[test]
    fn tui_renderer_defers_root_completion_until_worker_settlement() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = TuiRenderer { tx: tx.clone() };
        let summary = RunSummary {
            session_id: SessionId::new(),
            assistant_message_id: None,
            status: crate::session::SessionStatus::Completed,
            finish_reason: Some(crate::session::FinishReason::Stop),
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        };

        renderer.finish(&summary).expect("renderer finish");
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));

        publish_tui_run_finished(&tx, 9, Ok(summary.clone()));
        assert!(matches!(
            rx.try_recv().expect("worker settlement"),
            RuntimeMessage::Finished {
                run_generation: 9,
                result: Ok(received),
            } if received.session_id == summary.session_id
        ));
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
        let (runtime_tx, mut runtime_rx) = tokio::sync::mpsc::unbounded_channel();
        let broker = SharedConfirmationPrompt::new(TuiConfirmationPrompt {
            tx: runtime_tx,
            next_permission_request_id: Arc::new(AtomicU64::new(11)),
        });

        let first_cancel = CancellationToken::new();
        let (first_done_tx, first_done_rx) = mpsc::sync_channel(1);
        let mut first_prompt = broker.clone();
        let first_wait_cancel = first_cancel.clone();
        std::thread::spawn(move || {
            let result =
                first_prompt.confirm_with_cancel(&test_tui_permission("first"), &first_wait_cancel);
            let _ = first_done_tx.send(result);
        });
        let (first_id, first_response) = match recv_tui_runtime_message(&mut runtime_rx) {
            RuntimeMessage::Permission {
                confirmation_id,
                response,
                ..
            } => (confirmation_id, response),
            _ => panic!("expected first TUI permission"),
        };
        first_cancel.cancel();
        match recv_tui_runtime_message(&mut runtime_rx) {
            RuntimeMessage::PermissionCancelled { confirmation_id } => {
                assert_eq!(confirmation_id, first_id)
            }
            _ => panic!("expected TUI permission cancellation"),
        }
        assert!(
            !first_done_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("first TUI confirmation result")
                .expect("first TUI confirmation")
        );

        let mut pending_request = Some(test_tui_permission("first"));
        let mut pending_id = Some(first_id);
        let mut pending_response = Some(first_response);
        assert!(!clear_cancelled_tui_permission(
            &mut pending_request,
            &mut pending_id,
            &mut pending_response,
            first_id + 1,
        ));
        assert_eq!(pending_id, Some(first_id));
        assert!(clear_cancelled_tui_permission(
            &mut pending_request,
            &mut pending_id,
            &mut pending_response,
            first_id,
        ));

        let (second_done_tx, second_done_rx) = mpsc::sync_channel(1);
        let mut second_prompt = broker;
        std::thread::spawn(move || {
            let result = second_prompt.confirm(&test_tui_permission("second"));
            let _ = second_done_tx.send(result);
        });
        let (second_id, second_response) = match recv_tui_runtime_message(&mut runtime_rx) {
            RuntimeMessage::Permission {
                confirmation_id,
                response,
                ..
            } => (confirmation_id, response),
            _ => panic!("expected second TUI permission"),
        };
        assert!(second_id > first_id);
        second_response
            .send(true)
            .expect("answer second TUI permission");
        assert!(
            second_done_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("second TUI confirmation result")
                .expect("second TUI confirmation")
        );
    }

    #[tokio::test]
    async fn f2_session_apply_persists_access_before_committing_effective_and_live_state() {
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
            controller.live_config.access_mode(),
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
        assert_eq!(controller.live_config.access_mode(), global_access_mode);
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
            .admit_session_run(session_id)
            .await
            .expect("active session admission")
            .expect("active session admitted");

        assert!(controller.archive_selected_session(true).await.is_err());

        assert_eq!(controller.state.current_session_id, Some(session_id));
        assert_eq!(
            controller.effective_config.permissions.access_mode,
            session_access_mode
        );
        assert_eq!(controller.live_config.access_mode(), session_access_mode);
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
        use crate::protocol::ProtocolEventStore as _;
        use crate::session::{NewSession, SessionRepository as _};

        let (_temp, mut controller, session_a_id) =
            tui_controller_with_session("rejoin-access-session-a").await;
        let global_access_mode = crate::config::AccessMode::Default;
        let session_a_access_mode = crate::config::AccessMode::FullAccess;
        let session_b_access_mode = crate::config::AccessMode::AutoReview;
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
        controller
            .app
            .store
            .protocol_event_store()
            .append_history_item(&crate::protocol::HistoryItem {
                id: crate::protocol::HistoryItemId::new(),
                session_id: session_b.id,
                turn_id,
                sequence_no: 1,
                created_at_ms: 1,
                payload: crate::protocol::HistoryItemPayload::Message {
                    message_id: None,
                    role: crate::session::MessageRole::User,
                    content: vec![crate::protocol::ContentPart::Text {
                        text: "active session B context".to_string(),
                    }],
                },
            })
            .expect("session B history");
        let admission_id = repository
            .admit_session_run(session_b.id)
            .await
            .expect("session B admission")
            .expect("session B admitted");
        assert!(
            repository
                .activate_admitted_turn(session_b.id, &admission_id, turn_id)
                .await
                .expect("activate session B turn")
        );

        controller
            .rejoin_session(session_b.id)
            .await
            .expect("rejoin session B");

        assert_eq!(controller.state.current_session_id, Some(session_b.id));
        assert_eq!(
            controller.effective_config.permissions.access_mode,
            session_b_access_mode
        );
        assert_eq!(controller.live_config.access_mode(), session_b_access_mode);
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
        use crate::protocol::ProtocolEventStore as _;
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
            .append_history_item(&crate::protocol::HistoryItem {
                id: crate::protocol::HistoryItemId::new(),
                session_id: child.id,
                turn_id: crate::protocol::TurnId::new(),
                sequence_no: 1,
                created_at_ms: 1,
                payload: crate::protocol::HistoryItemPayload::Message {
                    message_id: None,
                    role: crate::session::MessageRole::User,
                    content: vec![crate::protocol::ContentPart::Text {
                        text: "child context".to_string(),
                    }],
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
        assert_eq!(
            controller.live_config.access_mode(),
            crate::config::AccessMode::Default
        );

        controller.toggle_access_mode().await.expect("F8 rejection");
        assert_eq!(
            controller.effective_config.permissions.access_mode,
            crate::config::AccessMode::Default
        );
        assert_eq!(
            controller.live_config.access_mode(),
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
    fn f2_session_apply_keeps_effective_and_live_state_when_persistence_fails() {
        let mut effective = ResolvedConfig::default();
        effective.permissions.access_mode = crate::config::AccessMode::Default;
        let live = LiveConfigOverrides::new(crate::config::AccessMode::Default);
        let mut candidate = effective.clone();
        candidate.permissions.access_mode = crate::config::AccessMode::FullAccess;

        assert!(!commit_tui_effective_config(
            &mut effective,
            &live,
            candidate,
            false,
        ));
        assert_eq!(
            effective.permissions.access_mode,
            crate::config::AccessMode::Default
        );
        assert_eq!(live.access_mode(), crate::config::AccessMode::Default);
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
}

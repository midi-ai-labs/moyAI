use std::io::{self, Stdout};
use std::process::Command as ProcessCommand;
use std::sync::mpsc;
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
use tui_textarea::TextArea;
use unicode_width::UnicodeWidthChar;

use crate::app::{App, AppBootstrap, AppCommand, ReviewRequest, RunRequest, SessionSteerRequest};
use crate::cli::{ConfirmationPrompt, EventRenderer, OutputMode, TuiArgs};
use crate::config::merge::apply_patch as apply_config_patch;
use crate::config::{ConfigLoader, ResolvedConfig, ShellFamily};
use crate::error::{AppRunError, CliPromptError, CliRenderError};
use crate::runtime::{SystemClock, build_cancel_token};
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
    runtime_tx: tokio::sync::mpsc::UnboundedSender<RuntimeMessage>,
    runtime_rx: tokio::sync::mpsc::UnboundedReceiver<RuntimeMessage>,
    permission_response: Option<mpsc::Sender<bool>>,
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
        let mut controller = Self {
            app,
            state: AppState::default(),
            composer: build_composer(),
            review_editor: build_composer(),
            workspace_picker: build_composer(),
            config_editor: ConfigEditorState::from_config(&effective_config),
            base_config,
            effective_config,
            runtime_tx,
            runtime_rx,
            permission_response: None,
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
                self.toggle_access_mode();
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
                    && self.state.run_status != RunStatus::Running
                    && self.state.run_status != RunStatus::Confirming =>
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
        } else if let Some(session) = self.state.selected_session() {
            self.open_session(session.id).await?;
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
                self.effective_config = apply_config_patch(self.base_config.clone(), patch);
                self.state.status_message = Some("applied session override".to_string());
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
        if let Some(response) = self.permission_response.take() {
            response
                .send(allow)
                .map_err(|error| AppRunError::Message(error.to_string()))?;
        }
        self.state.clear_permission();
        Ok(())
    }

    fn toggle_access_mode(&mut self) {
        self.effective_config.permissions.access_mode =
            self.effective_config.permissions.access_mode.next();
        self.config_editor = ConfigEditorState::from_config(&self.effective_config);
        self.state.status_message = Some(format!(
            "session access mode set to {}",
            self.effective_config.permissions.access_mode.label()
        ));
    }

    fn open_workspace_picker(&mut self) {
        self.workspace_picker = build_composer();
        self.workspace_picker
            .insert_str(self.app.workspace.cwd.as_str());
        self.state.modal = Modal::WorkspacePicker;
    }

    async fn submit_workspace_picker(&mut self) -> Result<(), AppRunError> {
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
        self.config_editor = ConfigEditorState::from_config(&self.effective_config);
        self.state = AppState::default();
        self.composer = build_composer();
        self.review_editor = build_composer();
        self.workspace_picker = build_composer();
        self.permission_response = None;
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
            cancel: build_cancel_token(),
        };
        self.state.push_local_prompt_dispatch(&prompt_dispatch);
        self.composer = build_composer();
        self.review_editor = build_composer();
        let run_service = self.app.run_service.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let mut renderer = TuiRenderer {
                tx: runtime_tx.clone(),
            };
            let mut prompt = TuiConfirmationPrompt {
                tx: runtime_tx.clone(),
            };
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build tui worker runtime");
            runtime.block_on(async move {
                if let Err(error) = run_service
                    .execute(AppCommand::Run(request), &mut renderer, &mut prompt)
                    .await
                {
                    let _ = runtime_tx.send(RuntimeMessage::Finished(Err(error.to_string())));
                }
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
        let cwd = self.app.workspace.cwd.clone();
        std::thread::spawn(move || {
            let mut renderer = TuiRenderer {
                tx: runtime_tx.clone(),
            };
            let mut prompt_ui = TuiConfirmationPrompt {
                tx: runtime_tx.clone(),
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
                RuntimeMessage::Finished(result) => match result {
                    Ok(summary) => {
                        self.state.set_summary(summary);
                        self.refresh_sessions().await?;
                        if let Some(session_id) = self.state.current_session_id {
                            self.open_session(session_id).await?;
                        }
                    }
                    Err(message) => {
                        self.state.run_status = RunStatus::Failed;
                        self.state.status_message = Some(message);
                    }
                },
                RuntimeMessage::Permission(request, response) => {
                    self.permission_response = Some(response);
                    self.state.set_permission(&request);
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
        self.state
            .load_turn_items(&view.session, &view.turn_items, view.state, view.todos);
        self.state.modal = Modal::None;
        Ok(())
    }

    async fn open_or_rejoin_selected_history_session(&mut self) -> Result<(), AppRunError> {
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

    async fn archive_selected_session(&mut self, archived: bool) -> Result<(), AppRunError> {
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
        }
        self.refresh_sessions().await
    }

    async fn rollback_selected_session(&mut self) -> Result<(), AppRunError> {
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
            "Ctrl+Enter=send/open  F2=history  F3=config  F4=workspace  F5=explorer  F6=enhance  F7=review  F8=toggle_access  Enter=ime  Ctrl+J=newline  Ctrl+Q=quit"
        } else {
            "Ctrl+Enter=send  F1=home  F2=history  F3=config  F4=workspace  F5=explorer  F6=enhance  F7=review  F8=toggle_access  Enter=ime  Ctrl+J=newline  Ctrl+Q=quit"
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
    Finished(Result<RunSummary, String>),
    Permission(PermissionRequest, mpsc::Sender<bool>),
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

    fn finish(&mut self, summary: &RunSummary) -> Result<(), CliRenderError> {
        self.tx
            .send(RuntimeMessage::Finished(Ok(summary.clone())))
            .map_err(|error| CliRenderError::Message(error.to_string()))
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
}

struct TuiConfirmationPrompt {
    tx: tokio::sync::mpsc::UnboundedSender<RuntimeMessage>,
}

impl ConfirmationPrompt for TuiConfirmationPrompt {
    fn confirm(&mut self, request: &PermissionRequest) -> Result<bool, CliPromptError> {
        let (response_tx, response_rx) = mpsc::channel();
        self.tx
            .send(RuntimeMessage::Permission(request.clone(), response_tx))
            .map_err(|error| CliPromptError::Message(error.to_string()))?;
        response_rx
            .recv()
            .map_err(|error| CliPromptError::Message(error.to_string()))
    }
}

fn setup_terminal() -> io::Result<TerminalHandle> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stdout))
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

fn textarea_value(textarea: &TextArea<'_>) -> String {
    textarea.lines().join("\n")
}

fn full_effective_override(config: &ResolvedConfig) -> crate::config::model::PartialResolvedConfig {
    crate::config::model::PartialResolvedConfig {
        model: Some(crate::config::model::PartialModelConfig {
            base_url: Some(config.model.base_url.clone()),
            model: Some(config.model.model.clone()),
            prompt_profile: Some(config.model.prompt_profile),
            provider_metadata_mode: Some(config.model.provider_metadata_mode),
            api_key_env: None,
            extra_headers: Some(config.model.extra_headers.clone()),
            request_timeout_ms: Some(config.model.request_timeout_ms),
            stream_idle_timeout_ms: Some(config.model.stream_idle_timeout_ms),
            connect_timeout_ms: Some(config.model.connect_timeout_ms),
            max_retries: Some(config.model.max_retries),
            stream_max_retries: Some(config.model.stream_max_retries),
            context_window: Some(config.model.context_window),
            max_output_tokens: Some(config.model.max_output_tokens),
            temperature: config.model.temperature,
            top_p: config.model.top_p,
            top_k: config.model.top_k,
            presence_penalty: config.model.presence_penalty,
            frequency_penalty: config.model.frequency_penalty,
            seed: config.model.seed,
            stop_sequences: Some(config.model.stop_sequences.clone()),
            supports_tools: Some(config.model.supports_tools),
            supports_reasoning: Some(config.model.supports_reasoning),
            supports_images: Some(config.model.supports_images),
            parallel_tool_calls: Some(config.model.parallel_tool_calls),
            max_parallel_predictions: Some(config.model.max_parallel_predictions),
            extra_body_json: config.model.extra_body_json.clone(),
        }),
        session: Some(crate::config::model::PartialSessionConfig {
            default_title_max_len: None,
            transcript_limit_messages: None,
            auto_resume_last: None,
            max_steps_per_turn: Some(config.session.max_steps_per_turn),
            overflow_margin_tokens: None,
        }),
        inspection: Some(crate::config::model::PartialInspectionConfig {
            default_max_depth: Some(config.inspection.default_max_depth),
            default_max_entries_per_dir: Some(config.inspection.default_max_entries_per_dir),
            max_extensions_reported: Some(config.inspection.max_extensions_reported),
            include_hidden_by_default: Some(config.inspection.include_hidden_by_default),
        }),
        file_guard: Some(crate::config::model::PartialFileGuardConfig {
            max_inline_read_bytes: Some(config.file_guard.max_inline_read_bytes),
            large_file_warning_bytes: Some(config.file_guard.large_file_warning_bytes),
            blocked_read_extensions: Some(config.file_guard.blocked_read_extensions.clone()),
            structured_document_extensions: Some(
                config.file_guard.structured_document_extensions.clone(),
            ),
        }),
        docling: Some(crate::config::model::PartialDoclingConfig {
            enabled: Some(config.docling.enabled),
            base_url: Some(config.docling.base_url.clone()),
            timeout_ms: Some(config.docling.timeout_ms),
            api_key_env: Some(config.docling.api_key_env.clone()),
            headers: Some(config.docling.headers.clone()),
        }),
        mcp: Some(crate::config::model::PartialMcpConfig {
            enabled: Some(config.mcp.enabled),
            servers: Some(config.mcp.servers.clone()),
        }),
        permissions: Some(crate::config::model::PartialPermissionsConfig {
            access_mode: Some(config.permissions.access_mode),
            additional_read_roots: Some(config.permissions.additional_read_roots.clone()),
            additional_write_roots: Some(config.permissions.additional_write_roots.clone()),
        }),
        ..Default::default()
    }
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
            | RunEvent::ToolProposalRejected { .. }
            | RunEvent::CandidateRepairEditRecorded { .. }
            | RunEvent::PermissionRequested { .. }
            | RunEvent::RetryScheduled { .. }
    )
}

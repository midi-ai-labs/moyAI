use std::process::Command as ProcessCommand;
use std::sync::mpsc;

use camino::{Utf8Path, Utf8PathBuf};

use crate::app::session_title::NEW_SESSION_PLACEHOLDER_TITLE;
use crate::app::{App, AppBootstrap, AppCommand, ReviewRequest, RunRequest};
use crate::cli::{ConfirmationPrompt, EventRenderer, OutputMode};
use crate::config::loader::{global_config_path, project_config_paths};
use crate::config::merge::apply_patch as apply_config_patch;
use crate::config::model::{PartialModelConfig, PartialPermissionsConfig, PartialResolvedConfig};
use crate::config::{ConfigLoader, ResolvedConfig, ShellFamily};
use crate::error::{AppRunError, CliPromptError, CliRenderError};
use crate::llm::{
    ProviderModelInfo, apply_provider_model_info_to_config, fetch_provider_model_infos,
    normalize_provider_base_url,
};
use crate::runtime::{SystemClock, build_cancel_token};
use crate::session::{
    EditorContext, ProjectId, ProjectRecord, RunEvent, RunSummary, SessionId, SessionRecord,
    SessionStatus, TodoItem, history_items_to_markdown, history_markdown_file_name,
};
use crate::tool::PermissionRequest;
use crate::workspace::project::normalize_path;
use tokio_util::sync::CancellationToken;

use super::args::{DesktopArgs, quick_chat_workspace_directory};
use super::models::DesktopTranscriptRow;
use super::navigation::NavigationRequestId;
use super::preferences::DesktopPreferences;
use super::query::{
    load_session_detail, load_snapshot, load_snapshot_continue_last, load_snapshot_for_selection,
};
use super::state::DesktopState;

enum RuntimeMessage {
    RunEvent(RunEvent),
    Finished(Result<RunSummary, String>),
    Permission(PermissionRequest, mpsc::Sender<bool>),
    EnhanceFinished {
        request_id: u64,
        result: Result<String, String>,
    },
    SnapshotLoaded(Result<super::models::DesktopSnapshot, String>),
    SessionLoaded {
        request_id: Option<NavigationRequestId>,
        session_id: SessionId,
        reason: SessionLoadReason,
        result: Result<LoadedSession, String>,
    },
    SessionDeleted {
        session_id: SessionId,
        result: Result<super::models::DesktopSnapshot, String>,
    },
    ProjectDeleted {
        project_id: ProjectId,
        project_root: Utf8PathBuf,
        result: Result<WorkspaceLoadResult, String>,
    },
    CurrentTodosLoaded {
        session_id: SessionId,
        result: Result<Vec<TodoItem>, String>,
    },
    ModelCatalogLoaded {
        requested_base_url: String,
        result: Result<Vec<ProviderModelInfo>, String>,
    },
    HistoryExported(Result<Utf8PathBuf, String>),
    WorkspaceSwitched {
        request_id: NavigationRequestId,
        result: Result<WorkspaceLoadResult, String>,
    },
    WorkspaceSwitchedForNewProjectSession {
        request_id: NavigationRequestId,
        result: Result<WorkspaceLoadResult, String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeMessageAsyncContract {
    RunStream,
    TerminalRun,
    ModalDecision,
    BackgroundOperation,
    NavigationOperation,
    ProviderOperation,
    StatusOnlyOperation,
}

impl RuntimeMessage {
    fn async_contract(&self) -> RuntimeMessageAsyncContract {
        match self {
            RuntimeMessage::RunEvent(_) => RuntimeMessageAsyncContract::RunStream,
            RuntimeMessage::Finished(_) => RuntimeMessageAsyncContract::TerminalRun,
            RuntimeMessage::Permission(_, _) => RuntimeMessageAsyncContract::ModalDecision,
            RuntimeMessage::EnhanceFinished { .. } => {
                RuntimeMessageAsyncContract::BackgroundOperation
            }
            RuntimeMessage::SnapshotLoaded(_) => RuntimeMessageAsyncContract::StatusOnlyOperation,
            RuntimeMessage::SessionLoaded { .. } => {
                RuntimeMessageAsyncContract::NavigationOperation
            }
            RuntimeMessage::SessionDeleted { .. } => {
                RuntimeMessageAsyncContract::BackgroundOperation
            }
            RuntimeMessage::ProjectDeleted { .. } => {
                RuntimeMessageAsyncContract::BackgroundOperation
            }
            RuntimeMessage::CurrentTodosLoaded { .. } => {
                RuntimeMessageAsyncContract::BackgroundOperation
            }
            RuntimeMessage::ModelCatalogLoaded { .. } => {
                RuntimeMessageAsyncContract::ProviderOperation
            }
            RuntimeMessage::HistoryExported(_) => RuntimeMessageAsyncContract::BackgroundOperation,
            RuntimeMessage::WorkspaceSwitched { .. }
            | RuntimeMessage::WorkspaceSwitchedForNewProjectSession { .. } => {
                RuntimeMessageAsyncContract::NavigationOperation
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionLoadReason {
    UserSelection,
    CurrentRefresh,
}

struct LoadedSession {
    session: crate::session::SessionRecord,
    transcript: crate::session::Transcript,
    turn_items: Vec<crate::protocol::TurnItem>,
    state: crate::session::SessionStateSnapshot,
    todos: Vec<TodoItem>,
}

#[derive(Clone)]
struct WorkspaceLoadResult {
    app: App,
    snapshot: super::models::DesktopSnapshot,
}

pub(crate) struct DesktopController {
    pub(crate) app: App,
    pub(crate) state: DesktopState,
    preferences: DesktopPreferences,
    persist_preferences_to_disk: bool,
    runtime_tx: tokio::sync::mpsc::UnboundedSender<RuntimeMessage>,
    runtime_rx: tokio::sync::mpsc::UnboundedReceiver<RuntimeMessage>,
    permission_response: Option<mpsc::Sender<bool>>,
    active_run_cancel: Option<CancellationToken>,
    next_enhance_request_id: u64,
}

impl DesktopController {
    pub(crate) async fn new(app: App, args: DesktopArgs) -> Result<Self, AppRunError> {
        Self::new_with_preferences_and_persistence(
            app,
            args,
            DesktopPreferences::load_or_default(),
            true,
        )
        .await
    }

    async fn new_with_preferences_and_persistence(
        mut app: App,
        args: DesktopArgs,
        mut preferences: DesktopPreferences,
        persist_preferences_to_disk: bool,
    ) -> Result<Self, AppRunError> {
        let (runtime_tx, runtime_rx) = tokio::sync::mpsc::unbounded_channel();
        if args.directory.is_some() {
            preferences.unmark_project_deleted(&app.workspace.root);
        } else {
            purge_deleted_project_roots(&app, &preferences)
                .await
                .map_err(AppRunError::Message)?;
            if preferences.is_project_deleted(&app.workspace.root) {
                let store = app.session_service.store.clone();
                let mut hidden_roots = preferences.deleted_project_roots.clone();
                hidden_roots.extend(internal_desktop_project_roots(
                    app.session_service.store.paths().data_dir.as_path(),
                ));
                let next_root = next_project_root_after_delete(
                    &app,
                    app.workspace.project_id,
                    &hidden_roots,
                    &app.workspace.root,
                )
                .await
                .map_err(AppRunError::Message)?
                .unwrap_or_else(|| {
                    quick_chat_workspace_directory().unwrap_or_else(|| {
                        fallback_workspace_after_project_delete(
                            &app.workspace.root,
                            &hidden_roots,
                            app.session_service.store.paths().data_dir.as_path(),
                        )
                    })
                });
                std::fs::create_dir_all(next_root.as_std_path()).map_err(|error| {
                    AppRunError::Message(format!(
                        "failed to create fallback workspace {} after deleted project restore: {error}",
                        next_root
                    ))
                })?;
                app = AppBootstrap::rebuild_for_directory(&next_root, store)
                    .await
                    .map_err(|error| {
                        AppRunError::Message(format!(
                            "failed to open fallback workspace {} after deleted project restore: {error}",
                            next_root
                        ))
                    })?;
            }
        }
        if let Some(session_id) = args.session_id {
            let session = app.session_service.get_session(session_id).await?;
            if session.cwd != app.workspace.cwd {
                let store = app.session_service.store.clone();
                app = AppBootstrap::rebuild_for_directory(&session.cwd, store)
                    .await
                    .map_err(|error| {
                        AppRunError::Message(format!(
                            "failed to open session workspace {}: {error}",
                            session.cwd
                        ))
                    })?;
            }
        }
        app.session_service
            .mark_stale_running_sessions(
                "Desktop started without an active worker for this run; marking the prior run interrupted.",
            )
            .await?;

        let snapshot = if args.continue_last {
            load_snapshot_continue_last(&app).await?
        } else {
            load_snapshot(&app, &args).await?
        };
        let effective_config =
            apply_preferences_override(&preferences, &app.workspace.root, app.config.clone());
        let mut state = DesktopState::new(snapshot, effective_config);
        state.workspace_input = app.workspace.cwd.to_string();
        state.begin_startup(
            args.global_config_existed_at_launch,
            global_config_path().ok(),
            &app.workspace.root,
        );
        if let Some(opacity) = preferences.window_opacity_percent {
            state.set_window_opacity_percent(opacity);
        }
        if let Some(session_id) = args.session_id.or_else(|| state.selected_session_id()) {
            let (session, transcript, turn_items, session_state, todos) =
                load_session_detail(&app, session_id).await?;
            state.load_open_session(&session, &transcript, &turn_items, session_state, todos);
        }
        let mut controller = Self {
            app,
            state,
            preferences,
            persist_preferences_to_disk,
            runtime_tx,
            runtime_rx,
            permission_response: None,
            active_run_cancel: None,
            next_enhance_request_id: 1,
        };
        controller.persist_preferences();
        if !controller
            .state
            .provider_config
            .provider_base_url_input
            .trim()
            .is_empty()
        {
            controller.load_provider_models();
        } else {
            controller
                .state
                .fail_startup_provider_model_load("LLM URL が未設定です。");
        }
        Ok(controller)
    }

    pub(crate) fn refresh_snapshot(&mut self) {
        let app = self.app.clone();
        let selected_session_id = self.state.selected_session_id();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop refresh runtime");
            let result = runtime.block_on(async move {
                load_snapshot_for_selection(&app, selected_session_id)
                    .await
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::SnapshotLoaded(result));
        });
    }

    fn spawn_snapshot_refresh_for_session(&mut self, session_id: SessionId) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop refresh runtime");
            let result = runtime.block_on(async move {
                load_snapshot_for_selection(&app, Some(session_id))
                    .await
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::SnapshotLoaded(result));
        });
    }

    pub(crate) fn open_selected_session(&mut self) {
        if let Some(session_id) = self.state.selected_session_id() {
            self.state
                .set_status_message(format!("opening session {session_id}..."));
            let request_id = self.state.begin_session_load(session_id);
            self.spawn_session_load(
                session_id,
                SessionLoadReason::UserSelection,
                Some(request_id),
            );
        }
    }

    pub(crate) fn open_selected_project(&mut self) {
        if self.state.is_busy() {
            self.state
                .set_status_message("project cannot change while a run is active");
            return;
        }
        let Some(path) = self.state.selected_project_path().map(Utf8PathBuf::from) else {
            self.state.set_status_message("select a project first");
            return;
        };
        if path == self.app.workspace.root {
            return;
        }
        self.state
            .set_status_message(format!("opening project {}...", path));
        let request_id = self.state.begin_workspace_load(path.clone(), None);
        self.spawn_workspace_load(path, request_id);
    }

    pub(crate) fn delete_selected_session(&mut self) {
        if self.state.is_busy() {
            self.state
                .set_status_message("chat cannot be deleted while a run is active");
            return;
        }
        let Some(session_id) = self.state.selected_session_id() else {
            self.state
                .set_status_message("select a chat before deleting");
            return;
        };
        self.state
            .set_status_message(format!("deleting chat {}...", session_id));
        self.state.begin_session_delete_mutation();
        self.spawn_session_delete(session_id);
    }

    pub(crate) fn delete_selected_project(&mut self) {
        if self.state.is_busy() {
            self.state
                .set_status_message("project cannot be deleted while a run is active");
            return;
        }
        let Some(project_id) = self.state.selected_project_id() else {
            self.state
                .set_status_message("select a project before deleting");
            return;
        };
        let Some(project_root) = self.state.selected_project_path().map(Utf8PathBuf::from) else {
            self.state
                .set_status_message("selected project path is not available");
            return;
        };
        self.state
            .set_status_message(format!("deleting project {}...", project_id));
        let mut hidden_roots = self.preferences.deleted_project_roots.clone();
        hidden_roots.extend(internal_desktop_project_roots(
            self.app.session_service.store.paths().data_dir.as_path(),
        ));
        if !hidden_roots.iter().any(|root| root == &project_root) {
            hidden_roots.push(project_root.clone());
        }
        self.state.begin_project_delete_mutation();
        self.spawn_project_delete(project_id, project_root, hidden_roots);
    }

    pub(crate) fn export_selected_history_markdown_auto(&mut self) {
        let Some(session_id) = self.state.selected_session_id() else {
            self.state
                .set_status_message("select a session before exporting history");
            return;
        };
        let default_file_name =
            history_markdown_file_name(&self.state.selected_session_title(), session_id);
        let export_path = self
            .app
            .workspace
            .root
            .join(".moyai")
            .join("history-exports")
            .join(default_file_name);
        self.export_selected_history_markdown_to_path(export_path);
    }

    pub(crate) fn export_open_transcript_markdown_auto(&mut self) {
        let Some(session_id) = self.state.selected_session_id() else {
            self.state
                .set_status_message("select a session before exporting transcript");
            return;
        };
        let detail = self.state.selected_detail();
        if detail.transcript_rows.is_empty() {
            self.state
                .set_status_message("open transcript has no rows to export");
            return;
        }
        let file_name =
            transcript_markdown_file_name(&self.state.selected_session_title(), session_id);
        let export_path = self
            .app
            .workspace
            .root
            .join(".moyai")
            .join("transcript-exports")
            .join(file_name);
        let markdown = open_transcript_rows_to_markdown(
            &self.state.selected_session_title(),
            &self.app.workspace.root,
            session_id,
            &self.state.provider_config.effective_config.model.base_url,
            &self.state.provider_config.effective_config.model.model,
            &detail.transcript_rows,
            &detail.file_changes,
        );
        let result = (|| {
            if let Some(parent) = export_path.parent() {
                std::fs::create_dir_all(parent.as_std_path()).map_err(|error| error.to_string())?;
            }
            std::fs::write(export_path.as_std_path(), markdown).map_err(|error| error.to_string())
        })();
        match result {
            Ok(()) => self
                .state
                .set_status_message(format!("saved transcript markdown to {}", export_path)),
            Err(error) => self
                .state
                .set_status_message(format!("transcript markdown export failed: {error}")),
        }
    }

    pub(crate) fn export_selected_history_markdown_to_path(&mut self, path: Utf8PathBuf) {
        let Some(session_id) = self.state.selected_session_id() else {
            self.state
                .set_status_message("select a session before exporting history");
            return;
        };
        self.state
            .set_status_message("exporting history markdown...");
        self.state.begin_history_export();
        self.spawn_history_markdown_export(session_id, normalize_markdown_export_path(path));
    }

    fn spawn_history_markdown_export(&self, session_id: SessionId, export_path: Utf8PathBuf) {
        let service = self.app.session_service.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop history-export runtime");
            let path = export_path.clone();
            let result = runtime
                .block_on(async move {
                    let session = service.get_session(session_id).await?;
                    let history_items = service.canonical_history_items(session_id).await?;
                    if history_items.is_empty() {
                        return Err(crate::error::SessionError::Message(
                            "canonical protocol history is empty".to_string(),
                        ));
                    }
                    Ok::<_, crate::error::SessionError>((session, history_items))
                })
                .map_err(|error| error.to_string())
                .and_then(|(session, history_items)| {
                    if let Some(parent) = export_path.parent() {
                        std::fs::create_dir_all(parent.as_std_path())
                            .map_err(|error| error.to_string())?;
                    }
                    let markdown = history_items_to_markdown(&session, &history_items);
                    std::fs::write(export_path.as_std_path(), markdown)
                        .map_err(|error| error.to_string())?;
                    Ok(path)
                });
            let _ = runtime_tx.send(RuntimeMessage::HistoryExported(result));
        });
    }

    fn spawn_session_load(
        &self,
        session_id: SessionId,
        reason: SessionLoadReason,
        request_id: Option<NavigationRequestId>,
    ) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session runtime");
            let result = runtime.block_on(async move {
                load_session_detail(&app, session_id)
                    .await
                    .map(
                        |(session, transcript, turn_items, state, todos)| LoadedSession {
                            session,
                            transcript,
                            turn_items,
                            state,
                            todos,
                        },
                    )
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::SessionLoaded {
                request_id,
                session_id,
                reason,
                result,
            });
        });
    }

    fn spawn_session_cancel_persist(&self, session_id: SessionId) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop cancel-persist runtime");
            let result = runtime.block_on(async move {
                app.session_service
                    .cancel_running_session(session_id, "run cancelled by user")
                    .await
                    .map_err(|error| error.to_string())?;
                load_session_detail(&app, session_id)
                    .await
                    .map(
                        |(session, transcript, turn_items, state, todos)| LoadedSession {
                            session,
                            transcript,
                            turn_items,
                            state,
                            todos,
                        },
                    )
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::SessionLoaded {
                request_id: None,
                session_id,
                reason: SessionLoadReason::CurrentRefresh,
                result,
            });
        });
    }

    fn spawn_session_delete(&self, session_id: SessionId) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session-delete runtime");
            let result = runtime.block_on(async move {
                app.session_service
                    .delete_session(session_id)
                    .await
                    .map_err(|error| error.to_string())?;
                load_snapshot_for_selection(&app, None)
                    .await
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::SessionDeleted { session_id, result });
        });
    }

    fn spawn_project_delete(
        &self,
        project_id: ProjectId,
        project_root: Utf8PathBuf,
        hidden_roots: Vec<Utf8PathBuf>,
    ) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        let project_root_for_thread = project_root.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop project-delete runtime");
            let result = runtime.block_on(async move {
                let deleted_was_current = project_id == app.workspace.project_id;
                app.session_service
                    .delete_project(project_id)
                    .await
                    .map_err(|error| error.to_string())?;
                let mut app = app;
                if deleted_was_current {
                    let remaining = app
                        .session_service
                        .list_projects(30)
                        .await
                        .map_err(|error| error.to_string())?;
                    let next_root = first_restorable_project_root(
                        &remaining,
                        project_id,
                        &hidden_roots,
                        &project_root_for_thread,
                    )
                    .unwrap_or_else(|| {
                        quick_chat_workspace_directory().unwrap_or_else(|| {
                            fallback_workspace_after_project_delete(
                                &project_root_for_thread,
                                &hidden_roots,
                                app.session_service.store.paths().data_dir.as_path(),
                            )
                        })
                    });
                    if let Some(parent) = next_root.parent() {
                        std::fs::create_dir_all(parent.as_std_path())
                            .map_err(|error| error.to_string())?;
                    }
                    std::fs::create_dir_all(next_root.as_std_path())
                        .map_err(|error| error.to_string())?;
                    let store = app.session_service.store.clone();
                    app = AppBootstrap::rebuild_for_directory(&next_root, store)
                        .await
                        .map_err(|error| error.to_string())?;
                }
                let snapshot = load_snapshot_for_selection(&app, None)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(WorkspaceLoadResult { app, snapshot })
            });
            let _ = runtime_tx.send(RuntimeMessage::ProjectDeleted {
                project_id,
                project_root,
                result,
            });
        });
    }

    fn spawn_current_todos_refresh(&self, session_id: SessionId) {
        let service = self.app.session_service.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop todo runtime");
            let result = runtime.block_on(async move {
                service
                    .list_todos(session_id)
                    .await
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::CurrentTodosLoaded { session_id, result });
        });
    }

    pub(crate) fn start_run(&mut self) {
        let prompt = self.state.composer.draft_prompt.trim().to_string();
        if prompt.is_empty() {
            return;
        }
        let prompt_dispatch = crate::session::PromptDispatchPart::raw(&prompt);
        self.launch_run_with_options(prompt, prompt_dispatch, None);
    }

    pub(crate) fn start_quick_chat(&mut self) {
        if self.state.is_busy() {
            self.state
                .set_status_message("new chat cannot start while a run is active");
            return;
        }
        let Some(root) = quick_chat_workspace_directory() else {
            self.state.start_new_chat();
            self.persist_preferences();
            return;
        };
        if self.is_quick_chat_workspace() {
            self.state.start_new_chat();
            self.persist_preferences();
            return;
        }
        if let Err(error) = std::fs::create_dir_all(root.as_std_path()) {
            self.state.set_status_message(format!(
                "failed to prepare quick chat workspace {}: {error}",
                root
            ));
            return;
        }
        self.state.hide_overlay();
        self.state
            .set_status_message("opening workspace-free quick chat...");
        let request_id = self.state.begin_workspace_load(root.clone(), None);
        self.spawn_workspace_load(root, request_id);
    }

    pub(crate) fn start_project_session(&mut self, index: usize) {
        if self.state.is_busy() {
            self.state
                .set_status_message("development chat cannot start while a run is active");
            return;
        }
        self.state.select_project(index);
        let Some(path) = self.state.selected_project_path().map(Utf8PathBuf::from) else {
            self.state
                .set_status_message("select a project before starting a development chat");
            return;
        };
        self.state.hide_overlay();
        if path == self.app.workspace.root {
            self.state.start_new_chat();
            self.state.set_status_message("new development chat ready");
            self.persist_preferences();
            return;
        }
        self.state.set_status_message(format!(
            "opening project {} for a new development chat...",
            path
        ));
        let request_id = self
            .state
            .begin_new_project_session_workspace_load(path.clone());
        self.spawn_workspace_load_for_new_project_session(path, request_id);
    }

    pub(crate) fn open_quick_chat_session(&mut self, index: usize) {
        if self.state.is_busy() {
            self.state
                .set_status_message("chat cannot change while a run is active");
            return;
        }
        let Some(session_id) = self
            .state
            .snapshot
            .chat_session_rows
            .get(index)
            .map(|row| row.session_id)
        else {
            self.state.set_status_message("select a chat first");
            return;
        };
        let Some(root) = quick_chat_workspace_directory() else {
            self.state
                .set_status_message("quick chat workspace is unavailable");
            return;
        };
        if self.is_quick_chat_workspace() {
            if let Some(row_index) = self
                .state
                .snapshot
                .session_rows
                .iter()
                .position(|row| row.session_id == session_id)
            {
                self.state.select_session(row_index);
                self.open_selected_session();
                return;
            }
        }
        self.state.hide_overlay();
        self.state
            .set_status_message(format!("opening chat {session_id}..."));
        let request_id = self
            .state
            .begin_workspace_load(root.clone(), Some(session_id));
        self.spawn_workspace_load_for_selection(root, Some(session_id), request_id);
    }

    pub(crate) fn delete_quick_chat_session(&mut self, index: usize) {
        if self.state.is_busy() {
            self.state
                .set_status_message("chat cannot be deleted while a run is active");
            return;
        }
        let Some(session_id) = self
            .state
            .snapshot
            .chat_session_rows
            .get(index)
            .map(|row| row.session_id)
        else {
            self.state
                .set_status_message("select a chat before deleting");
            return;
        };
        self.state
            .set_status_message(format!("deleting chat {}...", session_id));
        self.state.begin_session_delete_mutation();
        self.spawn_session_delete(session_id);
    }

    pub(crate) fn create_project_from_picker(&mut self) {
        if self.state.is_busy() {
            self.state
                .set_status_message("project cannot change while a run is active");
            return;
        }
        let start_dir = (!self.is_quick_chat_workspace()).then_some(&self.app.workspace.cwd);
        match pick_workspace_directory(start_dir) {
            Ok(Some(path)) => {
                self.state.hide_overlay();
                self.state
                    .set_status_message(format!("opening project workspace {}...", path));
                let request_id = self.state.begin_workspace_load(path.clone(), None);
                self.spawn_workspace_load(path, request_id);
            }
            Ok(None) => self.state.set_status_message("project creation cancelled"),
            Err(error) => self
                .state
                .set_status_message(format!("project creation failed: {error}")),
        }
    }

    pub(crate) fn start_review_uncommitted(&mut self) {
        let prompt = self.state.composer.draft_prompt.trim().to_string();
        let prompt_dispatch = crate::session::PromptDispatchPart::raw(&prompt);
        self.launch_run_with_options(prompt, prompt_dispatch, Some(ReviewRequest::Uncommitted));
    }

    pub(crate) fn start_prompt_enhance(&mut self) {
        let raw_prompt = self.state.composer.draft_prompt.trim().to_string();
        if raw_prompt.is_empty() || self.state.is_busy() {
            return;
        }
        let request_id = self.next_enhance_request_id;
        self.next_enhance_request_id += 1;
        self.state.begin_prompt_enhance(request_id, &raw_prompt);
        let runtime_tx = self.runtime_tx.clone();
        let config = self.state.provider_config.effective_config.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop enhance runtime");
            let result = runtime.block_on(async move {
                crate::tui::prompt_enhance::enhance_prompt(&config, &raw_prompt)
                    .await
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::EnhanceFinished { request_id, result });
        });
    }

    pub(crate) fn send_prompt_review(&mut self, send_enhanced: bool) {
        let Some(prompt_dispatch) = self.state.build_prompt_dispatch(send_enhanced) else {
            self.state
                .set_status_message("enhanced draft is not ready yet");
            return;
        };
        let prompt = prompt_dispatch.dispatch_prompt_text.clone();
        self.state.cancel_prompt_review();
        self.launch_run_with_options(prompt, prompt_dispatch, None);
    }

    pub(crate) fn load_provider_models(&mut self) {
        let normalized =
            normalize_provider_base_url(&self.state.provider_config.provider_base_url_input);
        if normalized.is_empty() {
            self.state.fail_provider_model_load("provider URL is empty");
            return;
        }
        self.state.begin_provider_model_load(normalized.clone());
        let runtime_tx = self.runtime_tx.clone();
        let config = self.state.provider_config.effective_config.clone();
        std::thread::spawn(move || {
            let request_base_url = normalized.clone();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop model-discovery runtime");
            let result = runtime.block_on(async move {
                fetch_provider_model_infos(&config, &request_base_url)
                    .await
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::ModelCatalogLoaded {
                requested_base_url: normalized,
                result,
            });
        });
    }

    pub(crate) fn apply_provider_session(&mut self) {
        let Some(config) = self.apply_provider_selection_to_effective_config() else {
            return;
        };
        self.state.reset_effective_config(config);
        self.preferences.set_workspace_override(
            &self.app.workspace.root,
            full_effective_override(&self.state.provider_config.effective_config),
        );
        self.persist_preferences();
        self.state
            .set_status_message("applied provider selection to this workspace session");
        self.state.hide_overlay();
    }

    pub(crate) fn save_provider_project(&mut self) {
        let Some(config) = self.apply_provider_selection_to_effective_config() else {
            return;
        };
        self.state.reset_effective_config(config);
        match self.state.provider_config.config_editor.save_scope(
            &self.app.workspace.root,
            crate::tui::config_editor::ConfigSaveScope::Project,
        ) {
            Ok(message) => {
                self.preferences
                    .clear_workspace_override(&self.app.workspace.root);
                self.persist_preferences();
                self.reload_config();
                self.state.set_status_message(message);
            }
            Err(error) => self
                .state
                .set_status_message(format!("config save failed: {error}")),
        }
    }

    pub(crate) fn save_provider_global(&mut self) {
        let Some(config) = self.apply_provider_selection_to_effective_config() else {
            return;
        };
        self.state.reset_effective_config(config);
        match self.state.provider_config.config_editor.save_scope(
            &self.app.workspace.root,
            crate::tui::config_editor::ConfigSaveScope::Global,
        ) {
            Ok(message) => {
                self.preferences
                    .clear_workspace_override(&self.app.workspace.root);
                self.persist_preferences();
                self.reload_config();
                self.state.set_status_message(message);
            }
            Err(error) => self
                .state
                .set_status_message(format!("config save failed: {error}")),
        }
    }

    pub(crate) fn apply_session_config(&mut self) {
        match self
            .state
            .provider_config
            .config_editor
            .build_session_override()
        {
            Ok(patch) => {
                let config = apply_config_patch(self.app.config.clone(), patch.clone());
                self.state.reset_effective_config(config);
                self.preferences
                    .set_workspace_override(&self.app.workspace.root, patch);
                self.persist_preferences();
                self.state.set_status_message("applied session override");
            }
            Err(error) => self
                .state
                .set_status_message(format!("config error: {error}")),
        }
    }

    pub(crate) fn toggle_access_mode_session(&mut self) {
        if self.state.is_busy() {
            self.state
                .set_status_message("access mode cannot change while a run is active");
            return;
        }

        let mut config = self.state.provider_config.effective_config.clone();
        config.permissions.access_mode = config.permissions.access_mode.next();
        let access_mode = config.permissions.access_mode;
        self.state.reset_effective_config(config);
        self.preferences.set_workspace_override(
            &self.app.workspace.root,
            full_effective_override(&self.state.provider_config.effective_config),
        );
        self.persist_preferences();
        self.state.set_status_message(format!(
            "session access mode set to {}",
            access_mode.label()
        ));
    }

    pub(crate) fn save_project_config(&mut self) {
        match self.state.provider_config.config_editor.save_scope(
            &self.app.workspace.root,
            crate::tui::config_editor::ConfigSaveScope::Project,
        ) {
            Ok(message) => {
                self.preferences
                    .clear_workspace_override(&self.app.workspace.root);
                self.persist_preferences();
                self.reload_config();
                self.state.set_status_message(message);
            }
            Err(error) => self
                .state
                .set_status_message(format!("config save failed: {error}")),
        }
    }

    pub(crate) fn save_global_config(&mut self) {
        match self.state.provider_config.config_editor.save_scope(
            &self.app.workspace.root,
            crate::tui::config_editor::ConfigSaveScope::Global,
        ) {
            Ok(message) => {
                self.preferences
                    .clear_workspace_override(&self.app.workspace.root);
                self.persist_preferences();
                self.reload_config();
                self.state.set_status_message(message);
            }
            Err(error) => self
                .state
                .set_status_message(format!("config save failed: {error}")),
        }
    }

    fn reload_config(&mut self) {
        match ConfigLoader::load(&self.app.workspace.root, None) {
            Ok(config) => {
                self.app.config = config.clone();
                let effective =
                    apply_preferences_override(&self.preferences, &self.app.workspace.root, config);
                self.state.reset_effective_config(effective);
                if !self
                    .state
                    .provider_config
                    .provider_base_url_input
                    .trim()
                    .is_empty()
                {
                    self.load_provider_models();
                }
            }
            Err(error) => self
                .state
                .set_status_message(format!("failed to reload config: {error}")),
        }
    }

    pub(crate) fn switch_workspace(&mut self) {
        if self.state.is_busy() {
            self.state
                .set_status_message("workspace cannot change while a run is active");
            return;
        }
        let Some(requested) = self.resolve_workspace_input() else {
            return;
        };
        let request_id = self.state.begin_workspace_load(requested.clone(), None);
        self.spawn_workspace_load(requested, request_id);
    }

    fn spawn_workspace_load(&self, requested: Utf8PathBuf, request_id: NavigationRequestId) {
        self.spawn_workspace_load_for_selection(requested, None, request_id);
    }

    fn spawn_workspace_load_for_new_project_session(
        &self,
        requested: Utf8PathBuf,
        request_id: NavigationRequestId,
    ) {
        let store = self.app.session_service.store.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop workspace runtime");
            let result = runtime.block_on(async move {
                let app = AppBootstrap::rebuild_for_directory(&requested, store)
                    .await
                    .map_err(|error| error.to_string())?;
                let snapshot = load_snapshot_for_selection(&app, None)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(WorkspaceLoadResult { app, snapshot })
            });
            let _ = runtime_tx
                .send(RuntimeMessage::WorkspaceSwitchedForNewProjectSession { request_id, result });
        });
    }

    fn spawn_workspace_load_for_selection(
        &self,
        requested: Utf8PathBuf,
        selected_session_id: Option<SessionId>,
        request_id: NavigationRequestId,
    ) {
        let store = self.app.session_service.store.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop workspace runtime");
            let result = runtime.block_on(async move {
                let app = AppBootstrap::rebuild_for_directory(&requested, store)
                    .await
                    .map_err(|error| error.to_string())?;
                let snapshot = load_snapshot_for_selection(&app, selected_session_id)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(WorkspaceLoadResult { app, snapshot })
            });
            let _ = runtime_tx.send(RuntimeMessage::WorkspaceSwitched { request_id, result });
        });
    }

    pub(crate) fn browse_workspace_dialog(&mut self) {
        let start_dir = if self.state.workspace_input.trim().is_empty() {
            Some(self.app.workspace.root.clone())
        } else {
            self.resolve_workspace_input()
                .or_else(|| Some(self.app.workspace.root.clone()))
        };
        match pick_workspace_directory(start_dir.as_ref()) {
            Ok(Some(path)) => {
                self.state.set_workspace_input(path.to_string());
                self.state
                    .set_status_message(format!("selected workspace {}", path));
            }
            Ok(None) => {}
            Err(error) => self
                .state
                .set_status_message(format!("workspace browse failed: {error}")),
        }
    }

    pub(crate) fn browse_image_dialog(&mut self) {
        match pick_image_file(Some(&self.app.workspace.cwd)) {
            Ok(Some(path)) => self.state.attach_image_path(path),
            Ok(None) => {}
            Err(error) => self
                .state
                .set_status_message(format!("image browse failed: {error}")),
        }
    }

    fn resolve_workspace_input(&mut self) -> Option<camino::Utf8PathBuf> {
        let requested = self.state.workspace_input.trim().to_string();
        if requested.is_empty() {
            self.state.set_status_message("workspace path is empty");
            return None;
        }
        let requested_input = camino::Utf8PathBuf::from(requested);
        let requested = match normalize_path(&self.app.workspace.cwd, &requested_input) {
            Ok(value) => value,
            Err(error) => {
                self.state
                    .set_status_message(format!("invalid workspace path: {error}"));
                return None;
            }
        };
        let metadata = match std::fs::metadata(requested.as_std_path()) {
            Ok(value) => value,
            Err(error) => {
                self.state.set_status_message(format!(
                    "workspace path is not accessible: {} ({error})",
                    requested
                ));
                return None;
            }
        };
        if !metadata.is_dir() {
            self.state
                .set_status_message(format!("workspace path is not a directory: {}", requested));
            return None;
        }
        Some(requested)
    }

    pub(crate) fn open_current_workspace_in_file_manager(&mut self) {
        let root = self.app.workspace.root.clone();
        self.open_path_in_file_manager(&root);
    }

    pub(crate) fn open_project_config_folder(&mut self) {
        let [primary, secondary] = project_config_paths(&self.app.workspace.root);
        let config_path = if secondary.exists() {
            secondary
        } else if primary.exists() {
            primary
        } else {
            primary
        };
        let Some(folder) = config_path.parent().map(camino::Utf8Path::to_path_buf) else {
            self.state
                .set_status_message("project config folder could not be resolved");
            return;
        };
        self.open_path_in_file_manager(&folder);
    }

    pub(crate) fn open_global_config_folder(&mut self) {
        let config_path = match global_config_path() {
            Ok(path) => path,
            Err(error) => {
                self.state
                    .set_status_message(format!("global config path failed: {error}"));
                return;
            }
        };
        let Some(folder) = config_path.parent().map(camino::Utf8Path::to_path_buf) else {
            self.state
                .set_status_message("global config folder could not be resolved");
            return;
        };
        if let Err(error) = std::fs::create_dir_all(folder.as_std_path()) {
            self.state.set_status_message(format!(
                "failed to create global config folder {}: {error}",
                folder
            ));
            return;
        }
        self.open_path_in_file_manager(&folder);
    }

    pub(crate) fn open_typed_path_in_file_manager(&mut self) {
        if let Some(path) = self.resolve_workspace_input() {
            self.open_path_in_file_manager(&path);
        }
    }

    pub(crate) fn open_selected_artifact_folder(&mut self) {
        let Some(path_text) = self.state.selected_artifact_path() else {
            self.state.set_status_message("select an artifact first");
            return;
        };
        let path = Utf8PathBuf::from(path_text);
        let absolute_path = if path.is_absolute() {
            path
        } else {
            self.app.workspace.root.join(path)
        };
        let folder = if absolute_path.is_dir() {
            absolute_path
        } else if let Some(parent) = absolute_path.parent() {
            parent.to_path_buf()
        } else {
            self.app.workspace.root.clone()
        };
        self.open_path_in_file_manager(&folder);
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
            Ok(_) => self
                .state
                .set_status_message(format!("opened {} in file manager", path)),
            Err(error) => self
                .state
                .set_status_message(format!("failed to open {} in file manager: {error}", path)),
        }
    }

    fn provider_selection_patch(&mut self) -> Option<PartialResolvedConfig> {
        let base_url =
            normalize_provider_base_url(&self.state.provider_config.provider_base_url_input);
        if base_url.is_empty() {
            self.state.set_status_message("provider URL is empty");
            return None;
        }
        let Some(model) = self.state.selected_provider_model() else {
            self.state
                .set_status_message("select one model before applying provider settings");
            return None;
        };
        let model = model.to_string();
        let mut hydrated_model_config = self.state.provider_config.effective_config.model.clone();
        hydrated_model_config.base_url = base_url.clone();
        hydrated_model_config.model = model.clone();
        if let Some(info) = self.state.selected_provider_model_info() {
            apply_provider_model_info_to_config(&mut hydrated_model_config, info);
        }
        Some(PartialResolvedConfig {
            model: Some(PartialModelConfig {
                base_url: Some(base_url),
                model: Some(model),
                context_window: Some(hydrated_model_config.context_window),
                max_output_tokens: Some(hydrated_model_config.max_output_tokens),
                supports_tools: Some(hydrated_model_config.supports_tools),
                supports_reasoning: Some(hydrated_model_config.supports_reasoning),
                supports_images: Some(hydrated_model_config.supports_images),
                parallel_tool_calls: Some(hydrated_model_config.parallel_tool_calls),
                max_parallel_predictions: Some(hydrated_model_config.max_parallel_predictions),
                extra_body_json: hydrated_model_config.extra_body_json.clone(),
                ..PartialModelConfig::default()
            }),
            ..PartialResolvedConfig::default()
        })
    }

    fn apply_provider_selection_to_effective_config(&mut self) -> Option<ResolvedConfig> {
        let patch = self.provider_selection_patch()?;
        Some(apply_config_patch(
            self.state.provider_config.effective_config.clone(),
            patch,
        ))
    }

    fn persist_preferences(&mut self) {
        if !self.persist_preferences_to_disk {
            return;
        }
        self.preferences.window_opacity_percent = Some(self.state.view.window_opacity_percent);
        if self.is_quick_chat_workspace() {
            self.preferences.last_workspace = None;
        } else {
            self.preferences.last_workspace = Some(self.app.workspace.root.clone());
        }
        if let Err(error) = self.preferences.save() {
            self.state
                .set_status_message(format!("failed to save desktop preferences: {error}"));
        }
    }

    fn is_quick_chat_workspace(&self) -> bool {
        is_quick_chat_workspace_path(&self.app.workspace.root)
    }

    pub(crate) fn answer_permission(&mut self, allow: bool) {
        if let Some(response) = self.permission_response.take() {
            if let Err(error) = response.send(allow) {
                self.state
                    .set_status_message(format!("failed to answer confirmation: {error}"));
            }
        }
        self.state.clear_permission();
    }

    pub(crate) fn cancel_active_run(&mut self) {
        let mut requested = false;
        if let Some(cancel) = &self.active_run_cancel {
            cancel.cancel();
            requested = true;
        }
        if let Some(response) = self.permission_response.take() {
            let _ = response.send(false);
            self.state.clear_permission();
            requested = true;
        }
        if requested {
            let session_id = self.state.app_state.current_session_id;
            self.state.mark_run_cancellation_requested(
                "run cancelled by user",
                "停止しました。現在の処理を中断しています。",
            );
            self.state.finish_agent_run();
            if let Some(session_id) = session_id {
                self.state.mark_post_run_refresh_pending();
                self.spawn_session_cancel_persist(session_id);
            }
        } else {
            self.state
                .set_status_message("停止できる実行中タスクはありません。");
        }
    }

    pub(crate) fn set_window_opacity_percent(&mut self, percent: i32) {
        self.state.set_window_opacity_percent(percent);
        self.persist_preferences();
    }

    fn launch_run_with_options(
        &mut self,
        prompt: String,
        prompt_dispatch: crate::session::PromptDispatchPart,
        review_request: Option<ReviewRequest>,
    ) {
        if self.active_run_cancel.is_some() {
            self.state.set_status_message(
                "前回の停止処理を片付けています。状態が更新されてから再度実行してください。",
            );
            return;
        }
        if prompt.trim().is_empty() && review_request.is_none() {
            return;
        }
        let image_paths = self.state.composer.image_attachment_paths.clone();
        if !image_paths.is_empty()
            && !self
                .state
                .provider_config
                .effective_config
                .model
                .supports_images
        {
            self.state.set_status_message(format!(
                "model `{}` does not advertise image support",
                self.state.provider_config.effective_config.model.model
            ));
            return;
        }
        let cancel = build_cancel_token();
        self.state.clear_post_run_refresh_pending();
        self.state.begin_agent_run();
        let request = RunRequest {
            prompt: prompt.clone(),
            session_id: self.state.app_state.current_session_id,
            continue_last: false,
            title: self
                .state
                .app_state
                .current_session_id
                .is_none()
                .then(|| NEW_SESSION_PLACEHOLDER_TITLE.to_string()),
            cwd: self.app.workspace.cwd.clone(),
            model: self
                .state
                .provider_config
                .effective_config
                .model
                .model
                .clone(),
            base_url: self
                .state
                .provider_config
                .effective_config
                .model
                .base_url
                .clone(),
            config_override: Some(full_effective_override(
                &self.state.provider_config.effective_config,
            )),
            output_mode: OutputMode::Human,
            show_reasoning: true,
            prompt_dispatch: Some(prompt_dispatch.clone()),
            editor_context: Some(self.current_editor_context()),
            review_request,
            image_paths,
            cancel: cancel.clone(),
        };
        self.active_run_cancel = Some(cancel);
        self.state.push_local_prompt_dispatch(&prompt_dispatch);
        self.state.composer.draft_prompt.clear();
        self.state.composer.image_attachment_paths.clear();
        self.state.composer.image_attachment_input.clear();
        let run_service = self.app.run_service.clone();
        let runtime_tx = self.runtime_tx.clone();
        let notification_title = request
            .title
            .clone()
            .unwrap_or_else(|| self.state.current_session_label());
        std::thread::spawn(move || {
            let mut renderer = DesktopRenderer {
                tx: runtime_tx.clone(),
                notification_title: notification_title.clone(),
                notified_terminal: false,
            };
            let mut prompt = DesktopConfirmationPrompt {
                tx: runtime_tx.clone(),
            };
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop worker runtime");
            runtime.block_on(async move {
                if let Err(error) = run_service
                    .execute(AppCommand::Run(request), &mut renderer, &mut prompt)
                    .await
                {
                    let error = error.to_string();
                    let notification_body = run_error_notification_body(
                        &notification_title,
                        &crate::tui::state::RunStatus::Failed,
                        &error,
                    );
                    send_windows_desktop_notification("moyAI", &notification_body);
                    let _ = runtime_tx.send(RuntimeMessage::Finished(Err(error)));
                }
            });
        });
    }

    fn current_editor_context(&self) -> EditorContext {
        let shell_family = self
            .state
            .provider_config
            .effective_config
            .shell
            .family
            .unwrap_or(if cfg!(windows) {
                ShellFamily::PowerShell
            } else {
                ShellFamily::Bash
            });
        let mut visible_files = self
            .state
            .app_state
            .session_state
            .as_ref()
            .map(|state| state.active_targets.clone())
            .unwrap_or_default();
        visible_files.sort();
        visible_files.dedup();
        let visible_files = visible_files.into_iter().take(8).collect::<Vec<_>>();
        EditorContext {
            active_file: visible_files.first().cloned(),
            open_tabs: visible_files.clone(),
            visible_files,
            shell_family,
            current_time_ms: SystemClock::now_ms(),
        }
    }

    fn apply_session_loaded_message(
        &mut self,
        request_id: Option<NavigationRequestId>,
        session_id: SessionId,
        reason: SessionLoadReason,
        result: Result<LoadedSession, String>,
    ) {
        match result {
            Ok(loaded) => {
                if self.session_load_is_blocked_by_active_run() {
                    return;
                }
                if !self.should_apply_loaded_session(request_id, session_id, reason) {
                    if reason == SessionLoadReason::CurrentRefresh {
                        self.state.clear_post_run_refresh_pending();
                    }
                    if let Some(request_id) = request_id {
                        self.state.finish_navigation(request_id);
                    }
                    return;
                }
                if let Some(request_id) = request_id {
                    self.state.finish_navigation(request_id);
                }
                self.state.load_open_session(
                    &loaded.session,
                    &loaded.transcript,
                    &loaded.turn_items,
                    loaded.state,
                    loaded.todos,
                );
                if reason == SessionLoadReason::CurrentRefresh {
                    self.state.clear_post_run_refresh_pending();
                    return;
                }
                self.state
                    .set_status_message(format!("opened session {}", session_id));
            }
            Err(error) => {
                if reason == SessionLoadReason::CurrentRefresh {
                    self.state.clear_post_run_refresh_pending();
                }
                if request_id
                    .map(|request_id| self.state.finish_navigation(request_id))
                    .unwrap_or(true)
                {
                    self.state.set_status_message(error);
                }
            }
        }
    }

    fn session_load_is_blocked_by_active_run(&self) -> bool {
        matches!(
            self.state.app_state.run_status,
            crate::tui::state::RunStatus::Running | crate::tui::state::RunStatus::Confirming
        )
    }

    fn refresh_current_session_after_terminal_run(&mut self) {
        self.refresh_snapshot();
        if let Some(session_id) = self.state.app_state.current_session_id {
            self.spawn_session_load(session_id, SessionLoadReason::CurrentRefresh, None);
        }
    }

    fn should_apply_loaded_session(
        &self,
        request_id: Option<NavigationRequestId>,
        session_id: SessionId,
        reason: SessionLoadReason,
    ) -> bool {
        match reason {
            SessionLoadReason::UserSelection => {
                request_id.is_some_and(|request_id| {
                    self.state
                        .is_current_session_navigation(request_id, session_id)
                }) && self.state.selected_session_id() == Some(session_id)
            }
            SessionLoadReason::CurrentRefresh => {
                self.state.app_state.current_session_id == Some(session_id)
            }
        }
    }

    fn apply_workspace_switched_message(
        &mut self,
        request_id: NavigationRequestId,
        result: Result<WorkspaceLoadResult, String>,
    ) {
        match result {
            Ok(loaded) => {
                if !self.state.is_current_navigation(request_id) {
                    return;
                }
                self.replace_workspace_from_load(loaded);
                if let Some(session_id) = self.state.selected_session_id() {
                    self.state
                        .set_status_message(format!("opening session {session_id}..."));
                    let request_id = self.state.begin_session_load(session_id);
                    self.spawn_session_load(
                        session_id,
                        SessionLoadReason::UserSelection,
                        Some(request_id),
                    );
                } else {
                    self.state.set_status_message(format!(
                        "workspace set to {}",
                        self.app.workspace.root
                    ));
                }
            }
            Err(error) => {
                if self.state.finish_navigation(request_id) {
                    self.state.set_status_message(error);
                }
            }
        }
    }

    fn apply_new_project_workspace_switched_message(
        &mut self,
        request_id: NavigationRequestId,
        result: Result<WorkspaceLoadResult, String>,
    ) {
        match result {
            Ok(loaded) => {
                if !self.state.is_current_navigation(request_id) {
                    return;
                }
                self.replace_workspace_from_load(loaded);
                self.state.start_new_chat();
                self.state.set_status_message("new development chat ready");
            }
            Err(error) => {
                if self.state.finish_navigation(request_id) {
                    self.state.set_status_message(error);
                }
            }
        }
    }

    fn replace_workspace_from_load(&mut self, loaded: WorkspaceLoadResult) {
        self.app = loaded.app.clone();
        if !self.is_quick_chat_workspace() {
            self.preferences
                .unmark_project_deleted(&self.app.workspace.root);
        }
        let effective = apply_preferences_override(
            &self.preferences,
            &self.app.workspace.root,
            self.app.config.clone(),
        );
        self.state = DesktopState::new(loaded.snapshot, effective);
        self.state.workspace_input = self.app.workspace.cwd.to_string();
        if let Some(opacity) = self.preferences.window_opacity_percent {
            self.state.set_window_opacity_percent(opacity);
        }
        self.persist_preferences();
        if !self
            .state
            .provider_config
            .provider_base_url_input
            .trim()
            .is_empty()
        {
            self.load_provider_models();
        }
    }

    pub(crate) fn drain_runtime_messages(&mut self) -> bool {
        let mut changed = false;
        while let Ok(message) = self.runtime_rx.try_recv() {
            changed = true;
            let _contract = message.async_contract();
            match message {
                RuntimeMessage::RunEvent(event) => {
                    if self
                        .active_run_cancel
                        .as_ref()
                        .is_some_and(|cancel| cancel.is_cancelled())
                        && !run_event_is_terminal(&event)
                    {
                        continue;
                    }
                    let refresh_session_id = match &event {
                        RunEvent::SessionStarted { session_id, .. }
                        | RunEvent::SessionTitleUpdated { session_id, .. } => Some(*session_id),
                        _ => None,
                    };
                    self.state.apply_run_event(&event);
                    if run_event_is_terminal(&event) {
                        self.state.mark_post_run_refresh_pending();
                    }
                    if event_requires_todo_refresh(&event) {
                        if let Some(session_id) = self.state.app_state.current_session_id {
                            self.state.begin_current_todo_refresh();
                            self.spawn_current_todos_refresh(session_id);
                        }
                    }
                    if let Some(session_id) = refresh_session_id {
                        self.spawn_snapshot_refresh_for_session(session_id);
                    }
                }
                RuntimeMessage::Finished(result) => match result {
                    Ok(summary) => {
                        self.active_run_cancel = None;
                        self.state.finish_agent_run();
                        self.state.mark_post_run_refresh_pending();
                        self.state.app_state.set_summary(summary);
                        self.refresh_current_session_after_terminal_run();
                    }
                    Err(error) => {
                        self.active_run_cancel = None;
                        self.state.finish_agent_run();
                        if !matches!(
                            self.state.app_state.run_status,
                            crate::tui::state::RunStatus::Cancelled
                        ) {
                            self.state.app_state.run_status = crate::tui::state::RunStatus::Failed;
                        }
                        self.state.set_status_message(error);
                        if self.state.app_state.current_session_id.is_some() {
                            self.state.mark_post_run_refresh_pending();
                            self.refresh_current_session_after_terminal_run();
                        } else {
                            self.state.clear_post_run_refresh_pending();
                        }
                    }
                },
                RuntimeMessage::Permission(request, response) => {
                    self.permission_response = Some(response);
                    self.state.set_permission(&request);
                }
                RuntimeMessage::EnhanceFinished { request_id, result } => match result {
                    Ok(draft) => {
                        if self.state.finish_prompt_enhance(request_id, draft) {
                            self.state.set_status_message("review enhanced draft");
                        }
                    }
                    Err(error) => {
                        if self.state.fail_prompt_enhance(request_id) {
                            self.state
                                .set_status_message(format!("prompt enhancement failed: {error}"));
                        }
                    }
                },
                RuntimeMessage::SnapshotLoaded(result) => match result {
                    Ok(snapshot) => self.state.replace_snapshot(snapshot),
                    Err(error) => self.state.set_status_message(error),
                },
                RuntimeMessage::SessionLoaded {
                    request_id,
                    session_id,
                    reason,
                    result,
                } => self.apply_session_loaded_message(request_id, session_id, reason, result),
                RuntimeMessage::SessionDeleted { session_id, result } => {
                    self.state.finish_session_delete_mutation();
                    match result {
                        Ok(snapshot) => {
                            let deleted_was_current =
                                self.state.app_state.current_session_id == Some(session_id);
                            self.state.replace_snapshot(snapshot);
                            if deleted_was_current {
                                if let Some(next_session_id) = self.state.selected_session_id() {
                                    self.state.set_status_message(format!(
                                        "deleted chat {}; opening {}...",
                                        session_id, next_session_id
                                    ));
                                    let request_id = self.state.begin_session_load(next_session_id);
                                    self.spawn_session_load(
                                        next_session_id,
                                        SessionLoadReason::UserSelection,
                                        Some(request_id),
                                    );
                                } else {
                                    self.state.start_new_chat();
                                    self.state
                                        .set_status_message(format!("deleted chat {}", session_id));
                                }
                            } else {
                                self.state
                                    .set_status_message(format!("deleted chat {}", session_id));
                            }
                        }
                        Err(error) => self
                            .state
                            .set_status_message(format!("chat delete failed: {error}")),
                    }
                }
                RuntimeMessage::ProjectDeleted {
                    project_id,
                    project_root,
                    result,
                } => {
                    self.state.finish_project_delete_mutation();
                    match result {
                        Ok(loaded) => {
                            let deleted_was_current = self.app.workspace.project_id == project_id;
                            self.preferences.mark_project_deleted(&project_root);
                            self.app = loaded.app.clone();
                            if !self.is_quick_chat_workspace() {
                                self.preferences
                                    .unmark_project_deleted(&self.app.workspace.root);
                            }
                            if deleted_was_current {
                                let effective = apply_preferences_override(
                                    &self.preferences,
                                    &self.app.workspace.root,
                                    self.app.config.clone(),
                                );
                                self.state = DesktopState::new(loaded.snapshot, effective);
                                self.state.workspace_input = self.app.workspace.cwd.to_string();
                                if let Some(opacity) = self.preferences.window_opacity_percent {
                                    self.state.set_window_opacity_percent(opacity);
                                }
                                self.persist_preferences();
                                if !self
                                    .state
                                    .provider_config
                                    .provider_base_url_input
                                    .trim()
                                    .is_empty()
                                {
                                    self.load_provider_models();
                                }
                            } else {
                                self.state.replace_snapshot(loaded.snapshot);
                                self.persist_preferences();
                            }
                            if let Some(next_session_id) = self.state.selected_session_id() {
                                self.state.set_status_message(format!(
                                    "deleted project {}; opening {}...",
                                    project_id, next_session_id
                                ));
                                let request_id = self.state.begin_session_load(next_session_id);
                                self.spawn_session_load(
                                    next_session_id,
                                    SessionLoadReason::UserSelection,
                                    Some(request_id),
                                );
                            } else {
                                self.state.start_new_chat();
                                self.state
                                    .set_status_message(format!("deleted project {}", project_id));
                            }
                        }
                        Err(error) => self
                            .state
                            .set_status_message(format!("project delete failed: {error}")),
                    }
                }
                RuntimeMessage::CurrentTodosLoaded { session_id, result } => match result {
                    Ok(todos) => {
                        self.state.finish_current_todo_refresh();
                        if self.state.app_state.current_session_id == Some(session_id) {
                            self.state.app_state.set_sidebar_todos(todos);
                        }
                    }
                    Err(error) => {
                        self.state.finish_current_todo_refresh();
                        self.state.set_status_message(error);
                    }
                },
                RuntimeMessage::ModelCatalogLoaded {
                    requested_base_url,
                    result,
                } => {
                    if normalize_provider_base_url(
                        &self.state.provider_config.provider_base_url_input,
                    ) != requested_base_url
                    {
                        continue;
                    }
                    match result {
                        Ok(models) => {
                            self.state.finish_startup_provider_model_load(&models);
                            self.state.finish_provider_model_load(models);
                        }
                        Err(error) => {
                            self.state.fail_startup_provider_model_load(error.clone());
                            self.state.fail_provider_model_load(error);
                        }
                    }
                }
                RuntimeMessage::HistoryExported(result) => match result {
                    Ok(path) => {
                        self.state.finish_history_export();
                        self.state
                            .set_status_message(format!("exported history markdown to {}", path));
                    }
                    Err(error) => {
                        self.state.finish_history_export();
                        self.state
                            .set_status_message(format!("history markdown export failed: {error}"));
                    }
                },
                RuntimeMessage::WorkspaceSwitched { request_id, result } => {
                    self.apply_workspace_switched_message(request_id, result)
                }
                RuntimeMessage::WorkspaceSwitchedForNewProjectSession { request_id, result } => {
                    self.apply_new_project_workspace_switched_message(request_id, result)
                }
            }
        }
        changed
    }
}

fn transcript_markdown_file_name(title: &str, session_id: SessionId) -> String {
    format!("{}-{}.md", markdown_file_stem(title), session_id)
}

fn markdown_file_stem(title: &str) -> String {
    let cleaned = title
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else if ch.is_whitespace() || matches!(ch, '.' | '/' | '\\' | ':' | '*') {
                '-'
            } else {
                ch
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let stem = cleaned.trim_matches('-');
    if stem.is_empty() {
        "transcript".to_string()
    } else {
        stem.chars().take(64).collect()
    }
}

fn open_transcript_rows_to_markdown(
    title: &str,
    workspace: &Utf8Path,
    session_id: SessionId,
    provider_base_url: &str,
    model: &str,
    rows: &[DesktopTranscriptRow],
    file_changes: &[super::models::DesktopFileChangeRow],
) -> String {
    let mut markdown = String::new();
    markdown.push_str("# ");
    markdown.push_str(&markdown_heading_text(title));
    markdown.push_str("\n\n");

    if let Some(user) = rows.iter().find(|row| row.kind == "user") {
        markdown.push_str("> ");
        markdown.push_str(&user.body.trim().replace('\n', "\n> "));
        markdown.push_str("\n\n");
    }

    let final_assistant_index = rows.iter().rposition(|row| row.kind == "assistant");
    let detail_rows = rows
        .iter()
        .enumerate()
        .filter(|(index, row)| Some(*index) != final_assistant_index && row.kind != "user")
        .collect::<Vec<_>>();
    if !detail_rows.is_empty() {
        markdown.push_str("<details><summary>");
        markdown.push_str(&format!("{} previous messages", detail_rows.len()));
        markdown.push_str("</summary>\n\n");
        for (_, row) in detail_rows {
            append_transcript_detail_row(&mut markdown, row);
        }
        markdown.push_str("</details>\n\n");
    }

    if let Some(index) = final_assistant_index {
        let body = rows[index].body.trim();
        if !body.is_empty() && !assistant_body_is_pseudo_tool_call_closeout(body) {
            markdown.push_str(body);
            markdown.push_str("\n\n");
        }
    }
    if final_assistant_index
        .and_then(|index| rows.get(index))
        .is_some_and(|row| assistant_body_is_pseudo_tool_call_closeout(row.body.trim()))
    {
        markdown.push_str("完了しました。\n\n");
    }

    if !file_changes.is_empty() {
        markdown.push_str("<details><summary>ファイル変更履歴</summary>\n\n");
        for change in file_changes {
            markdown.push_str("- ");
            markdown.push_str(&markdown_heading_text(&format!(
                "[{}] {}",
                change.action, change.path
            )));
            if !change.summary.trim().is_empty() {
                markdown.push_str(" - ");
                markdown.push_str(&markdown_heading_text(&change.summary));
            }
            markdown.push('\n');
        }
        markdown.push_str("\n</details>\n\n");
    }

    markdown.push_str("<details><summary>実行情報</summary>\n\n");
    markdown.push_str(&format!("- Workspace: `{}`\n", workspace));
    markdown.push_str(&format!("- Session: `{}`\n", session_id));
    markdown.push_str(&format!("- Provider: `{}`\n", provider_base_url));
    markdown.push_str(&format!("- Model: `{}`\n", model));
    markdown.push_str("</details>\n");
    markdown
}

fn assistant_body_is_pseudo_tool_call_closeout(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("<tool_call>")
        || lower.contains("<function=")
        || lower.contains("<parameter=command>")
}

fn append_transcript_detail_row(markdown: &mut String, row: &DesktopTranscriptRow) {
    match row.kind.as_str() {
        "assistant" => {
            let body = export_visible_body(&row.body);
            if !body.is_empty() {
                markdown.push_str("> ");
                markdown.push_str(&body.replace('\n', "\n> "));
                markdown.push_str("\n\n");
            }
        }
        "tool" | "editing" | "diff" | "summary" => {
            markdown.push_str("<details><summary>");
            markdown.push_str(&markdown_heading_text(&row.title));
            markdown.push_str("</summary>\n\n");
            let body = export_visible_body(&row.body);
            if body.is_empty() {
                markdown.push_str("_内容はありません。_\n\n");
            } else {
                markdown.push_str(&body);
                markdown.push_str("\n\n");
            }
            markdown.push_str("</details>\n\n");
        }
        _ => {
            markdown.push_str("> ");
            markdown.push_str(&markdown_heading_text(&row.title));
            if !row.body.trim().is_empty() {
                markdown.push_str("\n> ");
                markdown.push_str(&row.body.trim().replace('\n', "\n> "));
            }
            markdown.push_str("\n\n");
        }
    }
}

fn export_visible_body(body: &str) -> String {
    body.lines()
        .filter(|line| !line_contains_hidden_runtime_path(line))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn line_contains_hidden_runtime_path(line: &str) -> bool {
    let normalized = line.replace('\\', "/").to_ascii_lowercase();
    normalized.contains("/__pycache__/")
        || normalized.contains("__pycache__/")
        || normalized.contains(".pyc")
}

fn markdown_heading_text(value: &str) -> String {
    value
        .lines()
        .next()
        .unwrap_or("Transcript")
        .replace('#', "\\#")
        .trim()
        .to_string()
}

fn run_completion_notification_body(session_title: &str, summary: &RunSummary) -> String {
    let session_title = notification_session_title(session_title);
    let mut body = match summary.status {
        SessionStatus::Completed => format!("{session_title} が完了しました。"),
        SessionStatus::AwaitingUser => format!("{session_title} が確認待ちになりました。"),
        SessionStatus::Cancelled => format!("{session_title} を停止しました。"),
        SessionStatus::Failed => format!("{session_title} が失敗しました。"),
        SessionStatus::Running => format!("{session_title} は実行中です。"),
        SessionStatus::Idle => format!("{session_title} は待機状態です。"),
    };
    if summary.change_count > 0 {
        body.push_str(&format!(" 変更: {}件。", summary.change_count));
    }
    if summary.tool_call_count > 0 {
        body.push_str(&format!(" ツール: {}件", summary.tool_call_count));
        if summary.failed_tool_count > 0 {
            body.push_str(&format!(" / 失敗 {}件", summary.failed_tool_count));
        }
        body.push('。');
    }
    body
}

fn run_error_notification_body(
    session_title: &str,
    run_status: &crate::tui::state::RunStatus,
    error: &str,
) -> String {
    let session_title = notification_session_title(session_title);
    if matches!(run_status, crate::tui::state::RunStatus::Cancelled) {
        return format!("{session_title} を停止しました。");
    }
    let visible_error = error.lines().next().unwrap_or(error).trim();
    if visible_error.is_empty() {
        format!("{session_title} が失敗しました。")
    } else {
        format!("{session_title} が失敗しました: {visible_error}")
    }
}

fn run_terminal_event_notification_body(session_title: &str, event: &RunEvent) -> Option<String> {
    let session_title = notification_session_title(session_title);
    match event {
        RunEvent::SessionCompleted { .. } => Some(format!("{session_title} が完了しました。")),
        RunEvent::SessionAwaitingUser { .. } => {
            Some(format!("{session_title} が確認待ちになりました。"))
        }
        RunEvent::SessionInterrupted { reason, .. } => {
            let visible_reason = reason.lines().next().unwrap_or(reason).trim();
            if visible_reason.is_empty() {
                Some(format!("{session_title} を停止しました。"))
            } else {
                Some(format!("{session_title} を停止しました: {visible_reason}"))
            }
        }
        RunEvent::SessionFailed { message, .. } => {
            let visible_error = message.lines().next().unwrap_or(message).trim();
            if visible_error.is_empty() {
                Some(format!("{session_title} が失敗しました。"))
            } else {
                Some(format!("{session_title} が失敗しました: {visible_error}"))
            }
        }
        _ => None,
    }
}

fn notification_session_title(session_title: &str) -> String {
    let trimmed = session_title.trim();
    if trimmed.is_empty() || trimmed == "セッション未選択" || trimmed == "新規チャット"
    {
        "タスク".to_string()
    } else {
        trimmed.chars().take(48).collect()
    }
}

#[cfg(target_os = "windows")]
fn send_windows_desktop_notification(title: &str, body: &str) {
    if show_windows_notify_icon_balloon(title, body) {
        append_notification_debug_log(&format!(
            "native balloon queued title={title:?} body={body:?}"
        ));
        return;
    }
    append_notification_debug_log("native balloon unavailable; falling back to powershell");
    let script = windows_toast_script(title, body);
    let encoded = encode_powershell_command(&script);
    let powershell = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
    let powershell = if std::path::Path::new(powershell).exists() {
        powershell
    } else {
        "powershell.exe"
    };
    let parameters = format!(
        "-NoProfile -Sta -ExecutionPolicy Bypass -WindowStyle Hidden -EncodedCommand {encoded}"
    );
    append_notification_debug_log(&format!("launch title={title:?} body={body:?}"));
    let launched = unsafe { shell_execute_hidden(powershell, &parameters) };
    append_notification_debug_log(&format!("shell_execute launched={launched}"));
    if !launched {
        let fallback = ProcessCommand::new("cmd.exe")
            .args([
                "/C",
                "start",
                "",
                "/MIN",
                powershell,
                "-NoProfile",
                "-Sta",
                "-ExecutionPolicy",
                "Bypass",
                "-WindowStyle",
                "Hidden",
                "-EncodedCommand",
                &encoded,
            ])
            .spawn();
        append_notification_debug_log(&format!("fallback={fallback:?}"));
    }
}

#[cfg(not(target_os = "windows"))]
fn send_windows_desktop_notification(_title: &str, _body: &str) {}

#[cfg(target_os = "windows")]
fn windows_toast_script(title: &str, body: &str) -> String {
    let title = powershell_single_quoted(title);
    let body = powershell_single_quoted(body);
    format!(
        r#"
$ErrorActionPreference = 'SilentlyContinue'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
if ($env:MOYAI_NOTIFICATION_DEBUG_LOG) {{
  Add-Content -Encoding UTF8 -Path $env:MOYAI_NOTIFICATION_DEBUG_LOG -Value ('script-start ' + (Get-Date -Format o))
}}
$notify = New-Object System.Windows.Forms.NotifyIcon
$notify.Icon = [System.Drawing.SystemIcons]::Information
$notify.BalloonTipIcon = [System.Windows.Forms.ToolTipIcon]::Info
$notify.BalloonTipTitle = {title}
$notify.BalloonTipText = {body}
$notify.Visible = $true
$notify.ShowBalloonTip(7000)
Start-Sleep -Seconds 8
$notify.Dispose()
if ($env:MOYAI_NOTIFICATION_DEBUG_LOG) {{
  Add-Content -Encoding UTF8 -Path $env:MOYAI_NOTIFICATION_DEBUG_LOG -Value ('script-end ' + (Get-Date -Format o))
}}
"#
    )
}

#[cfg(target_os = "windows")]
fn append_notification_debug_log(message: &str) {
    if let Ok(path) = std::env::var("MOYAI_NOTIFICATION_DEBUG_LOG") {
        let timestamp = format!("{:?}", std::time::SystemTime::now());
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| {
                use std::io::Write as _;
                writeln!(file, "{timestamp} {message}")
            });
    }
}

#[cfg(target_os = "windows")]
fn show_windows_notify_icon_balloon(title: &str, body: &str) -> bool {
    let title = title.chars().take(63).collect::<String>();
    let body = body.chars().take(255).collect::<String>();
    std::thread::Builder::new()
        .name("moyai-notification".to_string())
        .spawn(move || unsafe {
            let result = show_windows_notify_icon_balloon_inner(&title, &body);
            append_notification_debug_log(&format!("native balloon result={result}"));
        })
        .is_ok()
}

#[cfg(target_os = "windows")]
unsafe fn show_windows_notify_icon_balloon_inner(title: &str, body: &str) -> bool {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{null, null_mut};

    const NIF_MESSAGE: u32 = 0x0000_0001;
    const NIF_ICON: u32 = 0x0000_0002;
    const NIF_TIP: u32 = 0x0000_0004;
    const NIF_INFO: u32 = 0x0000_0010;
    const NIM_ADD: u32 = 0x0000_0000;
    const NIM_MODIFY: u32 = 0x0000_0001;
    const NIM_DELETE: u32 = 0x0000_0002;
    const NIIF_INFO: u32 = 0x0000_0001;
    const WM_APP: u32 = 0x8000;
    const IDI_INFORMATION: usize = 32516;

    #[repr(C)]
    struct WndClassW {
        style: u32,
        lpfn_wnd_proc: Option<unsafe extern "system" fn(*mut c_void, u32, usize, isize) -> isize>,
        cb_cls_extra: i32,
        cb_wnd_extra: i32,
        h_instance: *mut c_void,
        h_icon: *mut c_void,
        h_cursor: *mut c_void,
        hbr_background: *mut c_void,
        lpsz_menu_name: *const u16,
        lpsz_class_name: *const u16,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    #[repr(C)]
    struct NotifyIconDataW {
        cb_size: u32,
        hwnd: *mut c_void,
        uid: u32,
        uflags: u32,
        ucallback_message: u32,
        hicon: *mut c_void,
        sztip: [u16; 128],
        dw_state: u32,
        dw_state_mask: u32,
        szinfo: [u16; 256],
        utimeout_or_version: u32,
        szinfo_title: [u16; 64],
        dw_info_flags: u32,
        guid_item: Guid,
        hballoon_icon: *mut c_void,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn RegisterClassW(lp_wnd_class: *const WndClassW) -> u16;
        fn CreateWindowExW(
            dw_ex_style: u32,
            lp_class_name: *const u16,
            lp_window_name: *const u16,
            dw_style: u32,
            x: i32,
            y: i32,
            n_width: i32,
            n_height: i32,
            hwnd_parent: *mut c_void,
            hmenu: *mut c_void,
            hinstance: *mut c_void,
            lp_param: *mut c_void,
        ) -> *mut c_void;
        fn DestroyWindow(hwnd: *mut c_void) -> i32;
        fn DefWindowProcW(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> isize;
        fn LoadIconW(hinstance: *mut c_void, lp_icon_name: *const u16) -> *mut c_void;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(lp_module_name: *const u16) -> *mut c_void;
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn Shell_NotifyIconW(dw_message: u32, lp_data: *mut NotifyIconDataW) -> i32;
    }

    unsafe extern "system" fn notification_wnd_proc(
        hwnd: *mut c_void,
        msg: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize {
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    fn wide_null(value: &str) -> Vec<u16> {
        std::ffi::OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn resource_id(value: usize) -> *const u16 {
        value as *const u16
    }

    fn copy_wide<const N: usize>(target: &mut [u16; N], value: &str) {
        for (slot, code_unit) in target
            .iter_mut()
            .take(N.saturating_sub(1))
            .zip(value.encode_utf16())
        {
            *slot = code_unit;
        }
    }

    let hinstance = unsafe { GetModuleHandleW(null()) };
    let class_name = wide_null("moyai_notification_window");
    let window_name = wide_null("moyAI");
    let wnd_class = WndClassW {
        style: 0,
        lpfn_wnd_proc: Some(notification_wnd_proc),
        cb_cls_extra: 0,
        cb_wnd_extra: 0,
        h_instance: hinstance,
        h_icon: null_mut(),
        h_cursor: null_mut(),
        hbr_background: null_mut(),
        lpsz_menu_name: null(),
        lpsz_class_name: class_name.as_ptr(),
    };
    let _ = unsafe { RegisterClassW(&wnd_class) };
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            window_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            (-3isize) as *mut c_void,
            null_mut(),
            hinstance,
            null_mut(),
        )
    };
    if hwnd.is_null() {
        return false;
    }

    let mut data = NotifyIconDataW {
        cb_size: std::mem::size_of::<NotifyIconDataW>() as u32,
        hwnd,
        uid: 1,
        uflags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        ucallback_message: WM_APP + 1,
        hicon: unsafe { LoadIconW(null_mut(), resource_id(IDI_INFORMATION)) },
        sztip: [0; 128],
        dw_state: 0,
        dw_state_mask: 0,
        szinfo: [0; 256],
        utimeout_or_version: 0,
        szinfo_title: [0; 64],
        dw_info_flags: NIIF_INFO,
        guid_item: Guid {
            data1: 0,
            data2: 0,
            data3: 0,
            data4: [0; 8],
        },
        hballoon_icon: null_mut(),
    };
    copy_wide(&mut data.sztip, "moyAI");
    let added = unsafe { Shell_NotifyIconW(NIM_ADD, &mut data) } != 0;
    if !added {
        let _ = unsafe { DestroyWindow(hwnd) };
        return false;
    }

    data.uflags = NIF_INFO;
    copy_wide(&mut data.szinfo_title, title);
    copy_wide(&mut data.szinfo, body);
    let modified = unsafe { Shell_NotifyIconW(NIM_MODIFY, &mut data) } != 0;
    std::thread::sleep(std::time::Duration::from_secs(8));
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &mut data) };
    let _ = unsafe { DestroyWindow(hwnd) };
    modified
}

#[cfg(target_os = "windows")]
fn encode_powershell_command(script: &str) -> String {
    use base64::Engine as _;
    let bytes = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[cfg(target_os = "windows")]
fn powershell_single_quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(target_os = "windows")]
unsafe fn shell_execute_hidden(file: &str, parameters: &str) -> bool {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{null, null_mut};

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            hwnd: *mut c_void,
            lp_operation: *const u16,
            lp_file: *const u16,
            lp_parameters: *const u16,
            lp_directory: *const u16,
            n_show_cmd: i32,
        ) -> *mut c_void;
    }

    fn wide_null(value: &str) -> Vec<u16> {
        std::ffi::OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    let operation = wide_null("open");
    let file = wide_null(file);
    let parameters = wide_null(parameters);
    let result = unsafe {
        ShellExecuteW(
            null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            parameters.as_ptr(),
            null(),
            0,
        )
    } as isize;
    result > 32
}

#[cfg(test)]
mod tests {
    use super::{
        RuntimeMessage, RuntimeMessageAsyncContract, fallback_workspace_after_project_delete,
        first_restorable_project_root, notification_session_title,
        open_transcript_rows_to_markdown, run_completion_notification_body,
        run_terminal_event_notification_body, transcript_markdown_file_name,
    };
    use crate::desktop::models::{DesktopFileChangeRow, DesktopTranscriptRow};
    use crate::session::{ProjectId, ProjectRecord, RunEvent, RunSummary, SessionStatus};
    use camino::{Utf8Path, Utf8PathBuf};

    fn project_record(id: ProjectId, root_path: &str) -> ProjectRecord {
        ProjectRecord {
            id,
            root_path: root_path.into(),
            display_name: root_path.to_string(),
            vcs_kind: "none".to_string(),
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    #[test]
    fn runtime_message_async_contract_classifies_representative_backflow_sources() {
        assert_eq!(
            RuntimeMessage::HistoryExported(Ok(Utf8PathBuf::from("C:/workspace/history.md")))
                .async_contract(),
            RuntimeMessageAsyncContract::BackgroundOperation
        );
        assert_eq!(
            RuntimeMessage::ModelCatalogLoaded {
                requested_base_url: "http://127.0.0.1:1234".to_string(),
                result: Ok(Vec::new()),
            }
            .async_contract(),
            RuntimeMessageAsyncContract::ProviderOperation
        );
        assert_eq!(
            RuntimeMessage::Finished(Err("failed".to_string())).async_contract(),
            RuntimeMessageAsyncContract::TerminalRun
        );
    }

    #[test]
    fn project_delete_selects_only_non_deleted_remaining_project() {
        let deleted_id = ProjectId::new();
        let hidden_id = ProjectId::new();
        let kept_id = ProjectId::new();
        let hidden_root = camino::Utf8PathBuf::from("C:/workspace/hidden");
        let deleted_root = Utf8Path::new("C:/workspace/deleted");
        let projects = vec![
            project_record(deleted_id, "C:/workspace/deleted"),
            project_record(hidden_id, "C:/workspace/hidden"),
            project_record(kept_id, "C:/workspace/kept"),
        ];

        let selected =
            first_restorable_project_root(&projects, deleted_id, &[hidden_root], deleted_root)
                .expect("kept project should be restorable");

        assert_eq!(selected, camino::Utf8PathBuf::from("C:/workspace/kept"));
    }

    #[test]
    fn project_delete_fallback_never_returns_deleted_or_hidden_root() {
        let deleted_root = Utf8Path::new("C:/workspace/deleted");
        let hidden_root = camino::Utf8PathBuf::from("C:/workspace/hidden");
        let data_dir = Utf8Path::new("C:/moyai-data");

        let fallback =
            fallback_workspace_after_project_delete(deleted_root, &[hidden_root.clone()], data_dir);

        assert_ne!(fallback.as_path(), deleted_root);
        assert_ne!(fallback, hidden_root);
    }

    #[test]
    fn open_transcript_markdown_keeps_visible_rows_and_metadata() {
        let session_id = crate::session::SessionId::new();
        let rows = vec![
            DesktopTranscriptRow {
                kind: "user".to_string(),
                step: "01".to_string(),
                title: "Prompt".to_string(),
                body: "Create a report.".to_string(),
            },
            DesktopTranscriptRow {
                kind: "assistant".to_string(),
                step: "02".to_string(),
                title: "Response".to_string(),
                body: "Done.\nSaved files.".to_string(),
            },
        ];

        let markdown = open_transcript_rows_to_markdown(
            "Session #1",
            &Utf8PathBuf::from("C:/workspace"),
            session_id,
            "http://localhost:1234",
            "local-model",
            &rows,
            &[],
        );

        assert!(markdown.contains("# Session \\#1"));
        assert!(markdown.contains("> Create a report."));
        assert!(markdown.contains("<details><summary>実行情報</summary>"));
        assert!(markdown.contains("- Provider: `http://localhost:1234`"));
        assert!(markdown.contains("Done.\nSaved files."));
        assert!(
            transcript_markdown_file_name("Session #1", session_id).ends_with(".md"),
            "transcript export should always use markdown extension"
        );
    }

    #[test]
    fn open_transcript_markdown_replaces_pseudo_tool_call_closeout() {
        let session_id = crate::session::SessionId::new();
        let rows = vec![
            DesktopTranscriptRow {
                kind: "user".to_string(),
                step: "01".to_string(),
                title: "Prompt".to_string(),
                body: "Create files.".to_string(),
            },
            DesktopTranscriptRow {
                kind: "assistant".to_string(),
                step: "02".to_string(),
                title: "Response".to_string(),
                body: "Now run this:\n<tool_call>\n<function=shell>\n</tool_call>".to_string(),
            },
            DesktopTranscriptRow {
                kind: "summary".to_string(),
                step: "03".to_string(),
                title: "File changes".to_string(),
                body: "Added README.md\nAdded __pycache__\\space_invader.cpython-313.pyc"
                    .to_string(),
            },
        ];
        let changes = vec![DesktopFileChangeRow {
            label: "README.md".to_string(),
            path: "README.md".to_string(),
            action: "追加".to_string(),
            summary: "Added README.md".to_string(),
        }];

        let markdown = open_transcript_rows_to_markdown(
            "Case2",
            &Utf8PathBuf::from("C:/workspace"),
            session_id,
            "http://localhost:1234",
            "local-model",
            &rows,
            &changes,
        );

        assert!(markdown.contains("完了しました。"));
        assert!(markdown.contains("ファイル変更履歴"));
        assert!(markdown.contains("README.md"));
        assert!(!markdown.contains("<tool_call>"));
        assert!(!markdown.contains("__pycache__"));
        assert!(!markdown.contains(".pyc"));
    }

    #[test]
    fn completion_notification_body_summarizes_terminal_run() {
        let summary = RunSummary {
            session_id: crate::session::SessionId::new(),
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
            tool_call_count: 3,
            failed_tool_count: 1,
            change_count: 2,
        };

        let body = run_completion_notification_body("  case2 GUI  ", &summary);

        assert!(body.contains("case2 GUI が完了しました。"));
        assert!(body.contains("変更: 2件"));
        assert!(body.contains("ツール: 3件 / 失敗 1件"));
        assert_eq!(notification_session_title(""), "タスク");
    }

    #[test]
    fn terminal_event_notification_body_uses_terminal_state() {
        let body = run_terminal_event_notification_body(
            "case2 GUI",
            &RunEvent::SessionInterrupted {
                session_id: crate::session::SessionId::new(),
                reason: "user requested stop\nsecond line".to_string(),
            },
        )
        .expect("terminal event should produce a notification");

        assert_eq!(body, "case2 GUI を停止しました: user requested stop");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_toast_script_quotes_notification_text() {
        let script = super::windows_toast_script("moy'AI", "done & \"quoted\"");

        assert!(script.contains("'moy''AI'"));
        assert!(script.contains("'done & \"quoted\"'"));
        assert!(script.contains("ShowBalloonTip"));
    }
}

struct DesktopRenderer {
    tx: tokio::sync::mpsc::UnboundedSender<RuntimeMessage>,
    notification_title: String,
    notified_terminal: bool,
}

impl EventRenderer for DesktopRenderer {
    fn render(&mut self, event: &RunEvent) -> Result<(), CliRenderError> {
        if let RunEvent::SessionTitleUpdated { title, .. } = event {
            self.notification_title = notification_session_title(title);
        }
        if !self.notified_terminal {
            if let Some(notification_body) =
                run_terminal_event_notification_body(&self.notification_title, event)
            {
                send_windows_desktop_notification("moyAI", &notification_body);
                self.notified_terminal = true;
            }
        }
        self.tx
            .send(RuntimeMessage::RunEvent(event.clone()))
            .map_err(|error| CliRenderError::Message(error.to_string()))
    }

    fn finish(&mut self, summary: &RunSummary) -> Result<(), CliRenderError> {
        if !self.notified_terminal {
            let notification_body =
                run_completion_notification_body(&self.notification_title, summary);
            send_windows_desktop_notification("moyAI", &notification_body);
            self.notified_terminal = true;
        }
        self.tx
            .send(RuntimeMessage::Finished(Ok(summary.clone())))
            .map_err(|error| CliRenderError::Message(error.to_string()))
    }

    fn render_session_list(&mut self, _sessions: &[SessionRecord]) -> Result<(), CliRenderError> {
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
        _transcript: &crate::session::Transcript,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
}

struct DesktopConfirmationPrompt {
    tx: tokio::sync::mpsc::UnboundedSender<RuntimeMessage>,
}

impl ConfirmationPrompt for DesktopConfirmationPrompt {
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

fn event_requires_todo_refresh(event: &RunEvent) -> bool {
    matches!(
        event,
        RunEvent::StateUpdated { .. }
            | RunEvent::ToolCallCompleted { .. }
            | RunEvent::ToolCallFailed { .. }
            | RunEvent::ToolProposalRejected { .. }
            | RunEvent::CandidateRepairEditRecorded { .. }
            | RunEvent::RecoverableRuntimeFeedback { .. }
            | RunEvent::SessionCompleted { .. }
            | RunEvent::SessionAwaitingUser { .. }
            | RunEvent::SessionInterrupted { .. }
            | RunEvent::SessionFailed { .. }
    )
}

fn run_event_is_terminal(event: &RunEvent) -> bool {
    matches!(
        event,
        RunEvent::SessionCompleted { .. }
            | RunEvent::SessionAwaitingUser { .. }
            | RunEvent::SessionInterrupted { .. }
            | RunEvent::SessionFailed { .. }
    )
}

fn apply_preferences_override(
    preferences: &DesktopPreferences,
    workspace_root: &camino::Utf8Path,
    base_config: ResolvedConfig,
) -> ResolvedConfig {
    match preferences.workspace_override(workspace_root) {
        Some(patch) => apply_config_patch(base_config, patch),
        None => base_config,
    }
}

async fn purge_deleted_project_roots(
    app: &App,
    preferences: &DesktopPreferences,
) -> Result<(), String> {
    if preferences.deleted_project_roots.is_empty() {
        return Ok(());
    }
    let projects = app
        .session_service
        .list_projects(200)
        .await
        .map_err(|error| error.to_string())?;
    for project in projects {
        if preferences.is_project_deleted(&project.root_path) {
            app.session_service
                .delete_project(project.id)
                .await
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

async fn next_project_root_after_delete(
    app: &App,
    deleted_project_id: ProjectId,
    hidden_roots: &[Utf8PathBuf],
    deleted_root: &Utf8Path,
) -> Result<Option<Utf8PathBuf>, String> {
    let projects = app
        .session_service
        .list_projects(30)
        .await
        .map_err(|error| error.to_string())?;
    Ok(first_restorable_project_root(
        &projects,
        deleted_project_id,
        hidden_roots,
        deleted_root,
    ))
}

fn first_restorable_project_root(
    projects: &[ProjectRecord],
    deleted_project_id: ProjectId,
    hidden_roots: &[Utf8PathBuf],
    deleted_root: &Utf8Path,
) -> Option<Utf8PathBuf> {
    projects
        .iter()
        .find(|project| {
            project.id != deleted_project_id
                && project.root_path != deleted_root
                && !hidden_roots.iter().any(|root| root == &project.root_path)
        })
        .map(|project| project.root_path.clone())
}

fn fallback_workspace_after_project_delete(
    deleted_root: &Utf8Path,
    hidden_roots: &[Utf8PathBuf],
    data_dir: &Utf8Path,
) -> Utf8PathBuf {
    let mut candidates = Vec::new();
    if let Some(quick_chat_workspace) = quick_chat_workspace_directory() {
        candidates.push(quick_chat_workspace);
    }
    candidates.push(data_dir.join("desktop-workspace"));
    candidates.push(data_dir.join("desktop-workspace-after-delete"));
    candidates
        .into_iter()
        .find(|candidate| {
            candidate != deleted_root && !hidden_roots.iter().any(|root| root == candidate)
        })
        .unwrap_or_else(|| data_dir.join("desktop-workspace-after-delete-2"))
}

fn is_quick_chat_workspace_path(path: &Utf8Path) -> bool {
    quick_chat_workspace_directory().as_deref() == Some(path)
}

fn internal_desktop_project_roots(data_dir: &Utf8Path) -> Vec<Utf8PathBuf> {
    [
        "quick-chat-workspace",
        "desktop-workspace",
        "desktop-workspace-after-delete",
        "desktop-workspace-after-delete-2",
    ]
    .into_iter()
    .map(|name| data_dir.join(name))
    .collect()
}

#[cfg(feature = "tauri-desktop")]
fn pick_workspace_directory(
    start_dir: Option<&camino::Utf8PathBuf>,
) -> Result<Option<camino::Utf8PathBuf>, String> {
    let mut dialog = rfd::FileDialog::new();
    if let Some(directory) = start_dir {
        dialog = dialog.set_directory(directory.as_std_path());
    }
    match dialog.pick_folder() {
        Some(path) => camino::Utf8PathBuf::from_path_buf(path)
            .map(Some)
            .map_err(|_| "selected directory path is not valid UTF-8".to_string()),
        None => Ok(None),
    }
}

#[cfg(not(feature = "tauri-desktop"))]
fn pick_workspace_directory(
    _start_dir: Option<&camino::Utf8PathBuf>,
) -> Result<Option<camino::Utf8PathBuf>, String> {
    Err("desktop folder picker requires the tauri-desktop feature".to_string())
}

#[cfg(feature = "tauri-desktop")]
fn pick_image_file(start_dir: Option<&Utf8Path>) -> Result<Option<Utf8PathBuf>, String> {
    let mut dialog =
        rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg", "webp", "gif"]);
    if let Some(directory) = start_dir {
        dialog = dialog.set_directory(directory.as_std_path());
    }
    match dialog.pick_file() {
        Some(path) => Utf8PathBuf::from_path_buf(path)
            .map(Some)
            .map_err(|_| "selected image path is not valid UTF-8".to_string()),
        None => Ok(None),
    }
}

#[cfg(not(feature = "tauri-desktop"))]
fn pick_image_file(_start_dir: Option<&Utf8Path>) -> Result<Option<Utf8PathBuf>, String> {
    Err("desktop image picker requires the tauri-desktop feature".to_string())
}

fn normalize_markdown_export_path(path: Utf8PathBuf) -> Utf8PathBuf {
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    {
        path
    } else {
        path.with_extension("md")
    }
}

fn full_effective_override(config: &ResolvedConfig) -> PartialResolvedConfig {
    PartialResolvedConfig {
        model: Some(PartialModelConfig {
            base_url: Some(config.model.base_url.clone()),
            model: Some(config.model.model.clone()),
            prompt_profile: Some(config.model.prompt_profile),
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
        permissions: Some(PartialPermissionsConfig {
            access_mode: Some(config.permissions.access_mode),
            additional_read_roots: Some(config.permissions.additional_read_roots.clone()),
            additional_write_roots: Some(config.permissions.additional_write_roots.clone()),
        }),
        agent: None,
        shell: None,
        format: None,
        instructions: None,
        workspace: None,
        tool_output: None,
        logging: None,
    }
}

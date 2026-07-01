use std::fs;
use std::io::Write;
use std::process::Command as ProcessCommand;
use std::sync::mpsc;

use camino::{Utf8Path, Utf8PathBuf};

use crate::app::session_title::NEW_SESSION_PLACEHOLDER_TITLE;
use crate::app::{App, AppBootstrap, AppCommand, ReviewRequest, RunRequest, SessionSteerRequest};
use crate::cli::{ConfirmationPrompt, EventRenderer, OutputMode};
use crate::config::loader::global_config_path;
use crate::config::merge::apply_patch as apply_config_patch;
use crate::config::model::{PartialModelConfig, PartialResolvedConfig, full_effective_override};
use crate::config::{ConfigLoader, ResolvedConfig, ShellFamily};
use crate::docling::{normalize_docling_base_url, probe_docling_readiness};
use crate::error::{AppRunError, CliPromptError, CliRenderError};
use crate::llm::{
    ModelAvailabilityReport, ProviderModelInfo, apply_provider_model_info_to_config,
    check_model_availability, extra_body_with_num_ctx, fetch_provider_model_infos,
    normalize_provider_base_url,
};
use crate::runtime::{LiveConfigOverrides, SystemClock, build_cancel_token};
use crate::session::markdown::{
    MarkdownExportEvent, MarkdownMetadataLine, MarkdownTerminalStatus,
    render_codex_turn_block_markdown,
};
use crate::session::{
    EditorContext, LoadedSessionStatus, ProjectId, ProjectRecord, RunEvent, RunSummary, SessionId,
    SessionMemoryMode, SessionRecord, SessionStatus, TodoItem, history_items_to_markdown,
    history_markdown_file_name,
};
use crate::tool::PermissionRequest;
use crate::workspace::project::normalize_path;
use tempfile::NamedTempFile;
use tokio_util::sync::CancellationToken;

use super::args::{DesktopArgs, quick_chat_workspace_directory};
use super::models::DesktopTranscriptRow;
use super::navigation::NavigationRequestId;
use super::preferences::DesktopPreferences;
use super::query::{
    DESKTOP_TURN_PAGE_LIMIT, LoadedSessionDetail, load_latest_session_detail, load_session_detail,
    load_snapshot, load_snapshot_continue_last, load_snapshot_for_selection,
    load_snapshot_for_session_search,
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
    SessionArchived {
        session_id: SessionId,
        archived: bool,
        result: Result<super::models::DesktopSnapshot, String>,
    },
    SessionRolledBack {
        session_id: SessionId,
        result: Result<DesktopRollbackLoaded, String>,
    },
    SessionOperationApplied {
        session_id: SessionId,
        result: Result<DesktopSessionOperationLoaded, String>,
    },
    TurnPageLoaded {
        session_id: SessionId,
        result: Result<LoadedSession, String>,
    },
    LiveSessionRefreshed {
        session_id: SessionId,
        result: Result<LoadedSession, String>,
    },
    SessionSearchLoaded(Result<super::models::DesktopSnapshot, String>),
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
        result: Result<DesktopProviderModelLoad, String>,
    },
    StartupDoclingChecked {
        requested_base_url: String,
        result: Result<(), String>,
    },
    SteerStored(Result<(), String>),
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

#[derive(Debug, Clone)]
struct DesktopProviderModelLoad {
    models: Vec<ProviderModelInfo>,
    availability_report: ModelAvailabilityReport,
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
            RuntimeMessage::SessionArchived { .. } => {
                RuntimeMessageAsyncContract::BackgroundOperation
            }
            RuntimeMessage::SessionRolledBack { .. } => {
                RuntimeMessageAsyncContract::BackgroundOperation
            }
            RuntimeMessage::SessionOperationApplied { .. } => {
                RuntimeMessageAsyncContract::BackgroundOperation
            }
            RuntimeMessage::TurnPageLoaded { .. } => {
                RuntimeMessageAsyncContract::NavigationOperation
            }
            RuntimeMessage::LiveSessionRefreshed { .. } => RuntimeMessageAsyncContract::RunStream,
            RuntimeMessage::SessionSearchLoaded(_) => {
                RuntimeMessageAsyncContract::NavigationOperation
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
            RuntimeMessage::StartupDoclingChecked { .. } => {
                RuntimeMessageAsyncContract::ProviderOperation
            }
            RuntimeMessage::SteerStored(_) => RuntimeMessageAsyncContract::BackgroundOperation,
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
    RunningRejoin,
    CurrentRefresh,
}

struct LoadedSession {
    session: crate::session::SessionRecord,
    transcript: crate::session::Transcript,
    turn_items: Vec<crate::protocol::TurnItem>,
    state: crate::session::SessionStateSnapshot,
    todos: Vec<TodoItem>,
    turn_page_offset: usize,
    turn_page_limit: usize,
    turn_page_total: usize,
    turn_page_has_more: bool,
}

#[derive(Clone)]
struct WorkspaceLoadResult {
    app: App,
    snapshot: super::models::DesktopSnapshot,
}

struct DesktopRollbackLoaded {
    snapshot: super::models::DesktopSnapshot,
    loaded: LoadedSession,
    dropped_turn_count: usize,
}

struct DesktopSessionOperationLoaded {
    snapshot: super::models::DesktopSnapshot,
    loaded: LoadedSession,
    message: String,
}

pub(crate) struct DesktopController {
    pub(crate) app: App,
    pub(crate) state: DesktopState,
    preferences: DesktopPreferences,
    persist_preferences_to_disk: bool,
    runtime_tx: tokio::sync::mpsc::UnboundedSender<RuntimeMessage>,
    runtime_rx: tokio::sync::mpsc::UnboundedReceiver<RuntimeMessage>,
    permission_response: Option<mpsc::Sender<bool>>,
    pending_permission_request: Option<PermissionRequest>,
    active_run_cancel: Option<CancellationToken>,
    active_live_config: Option<LiveConfigOverrides>,
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
        let mut state = DesktopState::new(snapshot, app.config.clone());
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
            let (
                session,
                transcript,
                turn_items,
                session_state,
                todos,
                turn_page_offset,
                turn_page_limit,
                turn_page_total,
                turn_page_has_more,
            ) = load_session_detail(&app, session_id).await?;
            state.load_open_session(
                &session,
                &transcript,
                &turn_items,
                session_state,
                todos,
                turn_page_offset,
                turn_page_limit,
                turn_page_total,
                turn_page_has_more,
            );
        }
        let mut controller = Self {
            app,
            state,
            preferences,
            persist_preferences_to_disk,
            runtime_tx,
            runtime_rx,
            permission_response: None,
            pending_permission_request: None,
            active_run_cancel: None,
            active_live_config: None,
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
        controller.check_startup_docling();
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

    pub(crate) fn rejoin_selected_session(&mut self) {
        if self.state.is_busy() {
            self.state.set_status_message(
                "running session cannot be rejoined while a local run is active",
            );
            return;
        }
        let Some(session_id) = self.state.selected_session_id() else {
            self.state
                .set_status_message("select a running session before rejoining");
            return;
        };
        let is_active_loaded = self
            .state
            .snapshot
            .session_rows
            .get(self.state.snapshot.selected_session_index)
            .is_some_and(|row| row.loaded_status == LoadedSessionStatus::Active);
        if !is_active_loaded {
            self.state
                .set_status_message("selected session is not an active loaded session");
            return;
        }
        self.state
            .set_status_message(format!("rejoining running session {session_id}..."));
        let request_id = self.state.begin_session_load(session_id);
        self.spawn_session_rejoin(session_id, Some(request_id));
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

    pub(crate) fn archive_selected_session(&mut self, archived: bool) {
        if self.state.is_busy() {
            self.state
                .set_status_message("chat archive state cannot change while a run is active");
            return;
        }
        let Some(session_id) = self.state.selected_session_id() else {
            self.state
                .set_status_message("select a chat before changing archive state");
            return;
        };
        self.state.set_status_message(if archived {
            format!("archiving chat {}...", session_id)
        } else {
            format!("unarchiving chat {}...", session_id)
        });
        self.state.begin_session_archive_mutation();
        self.spawn_session_archive(
            session_id,
            archived,
            self.state.view.session_search_text.clone(),
            self.state.view.session_search_include_archived,
        );
    }

    pub(crate) fn rollback_selected_session(&mut self) {
        if self.state.is_busy() {
            self.state
                .set_status_message("chat rollback cannot run while a local run is active");
            return;
        }
        let Some(session_id) = self.state.selected_session_id() else {
            self.state
                .set_status_message("select a chat before rolling back history");
            return;
        };
        let is_active_loaded = self
            .state
            .snapshot
            .session_rows
            .get(self.state.snapshot.selected_session_index)
            .is_some_and(|row| row.loaded_status == LoadedSessionStatus::Active);
        if is_active_loaded {
            self.state
                .set_status_message("running sessions cannot be rolled back");
            return;
        }
        self.state.set_status_message(format!(
            "rolling back latest turn in chat {}...",
            session_id
        ));
        self.state.begin_session_rollback_mutation();
        self.spawn_session_rollback(
            session_id,
            self.state.view.session_search_text.clone(),
            self.state.view.session_search_include_archived,
        );
    }

    pub(crate) fn fork_selected_session(&mut self) {
        if self.state.is_busy() {
            self.state
                .set_status_message("chat fork cannot run while a local run is active");
            return;
        }
        let Some(session_id) = self.state.selected_session_id() else {
            self.state
                .set_status_message("select a chat before forking");
            return;
        };
        let title = format!("{} fork", self.state.selected_session_title());
        self.state
            .set_status_message(format!("forking chat {}...", session_id));
        self.state.begin_session_maintenance_mutation();
        self.spawn_session_fork(session_id, Some(title));
    }

    pub(crate) fn compact_selected_session(&mut self) {
        if self.state.is_busy() {
            self.state
                .set_status_message("chat compact cannot run while a local run is active");
            return;
        }
        let Some(session_id) = self.state.selected_session_id() else {
            self.state
                .set_status_message("select a chat before compacting history");
            return;
        };
        self.state
            .set_status_message(format!("compacting chat {}...", session_id));
        self.state.begin_session_maintenance_mutation();
        self.spawn_session_compact(session_id, 20);
    }

    pub(crate) fn interrupt_selected_session(&mut self) {
        let Some(session_id) = self.state.selected_session_id() else {
            self.state
                .set_status_message("select a running chat before interrupting");
            return;
        };
        if self.state.app_state.current_session_id == Some(session_id) && self.state.is_busy() {
            self.cancel_active_run();
            return;
        }
        self.state
            .set_status_message(format!("interrupting running chat {}...", session_id));
        self.state.begin_session_maintenance_mutation();
        self.spawn_session_interrupt(session_id);
    }

    pub(crate) fn set_selected_session_memory_mode(&mut self, mode: SessionMemoryMode) {
        if self.state.is_busy() {
            self.state
                .set_status_message("chat memory mode cannot change while a local run is active");
            return;
        }
        let Some(session_id) = self.state.selected_session_id() else {
            self.state
                .set_status_message("select a chat before changing memory mode");
            return;
        };
        self.state.set_status_message(format!(
            "setting chat {} memory mode to {}...",
            session_id,
            mode.key()
        ));
        self.state.begin_session_maintenance_mutation();
        self.spawn_session_memory_mode(session_id, mode);
    }

    pub(crate) fn set_session_search(&mut self, text: String) {
        self.state.set_session_search_text(text.clone());
        self.state.begin_session_search();
        self.spawn_session_search(text, self.state.view.session_search_include_archived);
    }

    pub(crate) fn set_session_search_include_archived(&mut self, include_archived: bool) {
        self.state
            .set_session_search_include_archived(include_archived);
        self.state.begin_session_search();
        self.spawn_session_search(
            self.state.view.session_search_text.clone(),
            include_archived,
        );
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
        let result = (|| write_markdown_export_atomic(&export_path, &markdown))();
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

    pub(crate) fn load_previous_turn_page(&mut self) {
        let detail = self.state.selected_detail();
        if detail.turn_page_offset == 0 || detail.turn_page_limit == 0 {
            self.state
                .set_status_message("earlier turn page is not available");
            return;
        }
        let previous = detail
            .turn_page_offset
            .saturating_sub(detail.turn_page_limit);
        self.load_selected_turn_page(previous);
    }

    pub(crate) fn load_next_turn_page(&mut self) {
        let detail = self.state.selected_detail();
        if !detail.turn_page_has_more || detail.turn_page_limit == 0 {
            self.state
                .set_status_message("later turn page is not available");
            return;
        }
        self.load_selected_turn_page(
            detail
                .turn_page_offset
                .saturating_add(detail.turn_page_limit),
        );
    }

    fn load_selected_turn_page(&mut self, offset: usize) {
        if self.state.is_busy() {
            self.state
                .set_status_message("turn page cannot change while a local run is active");
            return;
        }
        let Some(session_id) = self.state.selected_session_id() else {
            self.state
                .set_status_message("select a session before changing turn page");
            return;
        };
        self.state
            .set_status_message(format!("loading turn page for session {session_id}..."));
        self.spawn_turn_page_load(session_id, offset, DESKTOP_TURN_PAGE_LIMIT);
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
                    let markdown = history_items_to_markdown(&session, &history_items);
                    write_markdown_export_atomic(&export_path, &markdown)?;
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
                let load_result = if reason == SessionLoadReason::CurrentRefresh {
                    load_latest_session_detail(&app, session_id).await
                } else {
                    load_session_detail(&app, session_id).await
                };
                load_result
                    .map(loaded_session_from_detail)
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

    fn spawn_turn_page_load(&self, session_id: SessionId, offset: usize, limit: usize) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop turn-page runtime");
            let result = runtime.block_on(async move {
                let page = app
                    .session_service
                    .canonical_turn_page(session_id, offset, limit)
                    .await
                    .map_err(|error| error.to_string())?;
                let transcript = app
                    .session_service
                    .canonical_transcript(session_id)
                    .await
                    .map_err(|error| error.to_string())?;
                let state = app
                    .session_service
                    .load_state(session_id)
                    .await
                    .map_err(|error| error.to_string())?;
                let todos = app
                    .session_service
                    .list_todos(session_id)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(LoadedSession {
                    session: page.session,
                    transcript,
                    turn_items: page.items,
                    state,
                    todos,
                    turn_page_offset: page.offset,
                    turn_page_limit: page.limit,
                    turn_page_total: page.total,
                    turn_page_has_more: page.has_more,
                })
            });
            let _ = runtime_tx.send(RuntimeMessage::TurnPageLoaded { session_id, result });
        });
    }

    fn spawn_live_session_refresh(&self, session_id: SessionId, offset: usize, limit: usize) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop live-session-refresh runtime");
            let result = runtime.block_on(async move {
                let page = app
                    .session_service
                    .canonical_turn_page(session_id, offset, limit)
                    .await
                    .map_err(|error| error.to_string())?;
                let transcript = app
                    .session_service
                    .canonical_transcript(session_id)
                    .await
                    .map_err(|error| error.to_string())?;
                let state = app
                    .session_service
                    .load_state(session_id)
                    .await
                    .map_err(|error| error.to_string())?;
                let todos = app
                    .session_service
                    .list_todos(session_id)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(LoadedSession {
                    session: page.session,
                    transcript,
                    turn_items: page.items,
                    state,
                    todos,
                    turn_page_offset: page.offset,
                    turn_page_limit: page.limit,
                    turn_page_total: page.total,
                    turn_page_has_more: page.has_more,
                })
            });
            let _ = runtime_tx.send(RuntimeMessage::LiveSessionRefreshed { session_id, result });
        });
    }

    fn spawn_latest_live_session_refresh(&self, session_id: SessionId) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop latest-session-refresh runtime");
            let result = runtime.block_on(async move {
                load_latest_session_detail(&app, session_id)
                    .await
                    .map(loaded_session_from_detail)
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::LiveSessionRefreshed { session_id, result });
        });
    }

    fn spawn_session_rejoin(&self, session_id: SessionId, request_id: Option<NavigationRequestId>) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session rejoin runtime");
            let result = runtime.block_on(async move {
                let rejoin = app
                    .session_service
                    .rejoin_running_session(session_id, 0, 200, 0, DESKTOP_TURN_PAGE_LIMIT)
                    .await
                    .map_err(|error| error.to_string())?;
                let transcript = app
                    .session_service
                    .canonical_transcript(session_id)
                    .await
                    .map_err(|error| error.to_string())?;
                let todos = app
                    .session_service
                    .list_todos(session_id)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(LoadedSession {
                    session: rejoin.read.session,
                    transcript,
                    turn_items: rejoin.read.turns.items,
                    state: rejoin.read.state,
                    todos,
                    turn_page_offset: rejoin.read.turns.offset,
                    turn_page_limit: rejoin.read.turns.limit,
                    turn_page_total: rejoin.read.turns.total,
                    turn_page_has_more: rejoin.read.turns.has_more,
                })
            });
            let _ = runtime_tx.send(RuntimeMessage::SessionLoaded {
                request_id,
                session_id,
                reason: SessionLoadReason::RunningRejoin,
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
                        |(
                            session,
                            transcript,
                            turn_items,
                            state,
                            todos,
                            turn_page_offset,
                            turn_page_limit,
                            turn_page_total,
                            turn_page_has_more,
                        )| LoadedSession {
                            session,
                            transcript,
                            turn_items,
                            state,
                            todos,
                            turn_page_offset,
                            turn_page_limit,
                            turn_page_total,
                            turn_page_has_more,
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
                run_storage_maintenance_after_delete(&app)?;
                load_snapshot_for_selection(&app, None)
                    .await
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::SessionDeleted { session_id, result });
        });
    }

    fn spawn_session_archive(
        &self,
        session_id: SessionId,
        archived: bool,
        query: String,
        include_archived: bool,
    ) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session-archive runtime");
            let result = runtime.block_on(async move {
                app.session_service
                    .set_session_archived(session_id, archived)
                    .await
                    .map_err(|error| error.to_string())?;
                load_snapshot_for_session_search(&app, &query, include_archived, None)
                    .await
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::SessionArchived {
                session_id,
                archived,
                result,
            });
        });
    }

    fn spawn_session_rollback(&self, session_id: SessionId, query: String, include_archived: bool) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session-rollback runtime");
            let result = runtime.block_on(async move {
                let rollback = app
                    .session_service
                    .rollback_session(session_id, 1)
                    .await
                    .map_err(|error| error.to_string())?;
                let snapshot = load_snapshot_for_session_search(
                    &app,
                    &query,
                    include_archived,
                    Some(session_id),
                )
                .await
                .map_err(|error| error.to_string())?;
                let (
                    session,
                    transcript,
                    turn_items,
                    state,
                    todos,
                    turn_page_offset,
                    turn_page_limit,
                    turn_page_total,
                    turn_page_has_more,
                ) = load_session_detail(&app, session_id)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(DesktopRollbackLoaded {
                    snapshot,
                    loaded: LoadedSession {
                        session,
                        transcript,
                        turn_items,
                        state,
                        todos,
                        turn_page_offset,
                        turn_page_limit,
                        turn_page_total,
                        turn_page_has_more,
                    },
                    dropped_turn_count: rollback.dropped_turn_ids.len(),
                })
            });
            let _ = runtime_tx.send(RuntimeMessage::SessionRolledBack { session_id, result });
        });
    }

    fn spawn_session_fork(&self, source_session_id: SessionId, title: Option<String>) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session-fork runtime");
            let result = runtime.block_on(async move {
                let fork = app
                    .session_service
                    .fork_session(source_session_id, title)
                    .await
                    .map_err(|error| error.to_string())?;
                let session_id = fork.forked_session.id;
                load_session_operation_projection(
                    &app,
                    session_id,
                    format!(
                        "forked chat {} to {} ({} history item(s), {} turn item(s))",
                        source_session_id,
                        session_id,
                        fork.copied_history_items,
                        fork.copied_turn_items
                    ),
                )
                .await
            });
            let session_id = result
                .as_ref()
                .map(|loaded| loaded.loaded.session.id)
                .unwrap_or(source_session_id);
            let _ = runtime_tx.send(RuntimeMessage::SessionOperationApplied { session_id, result });
        });
    }

    fn spawn_session_compact(&self, session_id: SessionId, keep_recent: usize) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session-compact runtime");
            let result = runtime.block_on(async move {
                let compact = app
                    .session_service
                    .compact_session(session_id, keep_recent)
                    .await
                    .map_err(|error| error.to_string())?;
                load_session_operation_projection(
                    &app,
                    session_id,
                    format!(
                        "compacted chat {}: summarized {} item(s), retained {} item(s)",
                        session_id,
                        compact.summarized_history_items,
                        compact.retained_history_items
                    ),
                )
                .await
            });
            let _ = runtime_tx.send(RuntimeMessage::SessionOperationApplied { session_id, result });
        });
    }

    fn spawn_session_interrupt(&self, session_id: SessionId) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session-interrupt runtime");
            let result = runtime.block_on(async move {
                app.session_service
                    .interrupt_running_session(
                        session_id,
                        "Desktop interrupt requested".to_string(),
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                load_session_operation_projection(
                    &app,
                    session_id,
                    format!("interrupted running chat {}", session_id),
                )
                .await
            });
            let _ = runtime_tx.send(RuntimeMessage::SessionOperationApplied { session_id, result });
        });
    }

    fn spawn_session_memory_mode(&self, session_id: SessionId, mode: SessionMemoryMode) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session-memory runtime");
            let result = runtime.block_on(async move {
                let update = app
                    .session_service
                    .update_session_memory_mode(session_id, mode)
                    .await
                    .map_err(|error| error.to_string())?;
                load_session_operation_projection(
                    &app,
                    session_id,
                    format!(
                        "set chat {} memory mode to {}",
                        session_id,
                        update.mode.key()
                    ),
                )
                .await
            });
            let _ = runtime_tx.send(RuntimeMessage::SessionOperationApplied { session_id, result });
        });
    }

    fn spawn_session_search(&self, query: String, include_archived: bool) {
        let app = self.app.clone();
        let selected_session_id = self.state.selected_session_id();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session-search runtime");
            let result = runtime.block_on(async move {
                load_snapshot_for_session_search(
                    &app,
                    &query,
                    include_archived,
                    selected_session_id,
                )
                .await
                .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::SessionSearchLoaded(result));
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
                run_storage_maintenance_after_delete(&app)?;
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
        let config = provider_catalog_probe_config(
            self.state.provider_config.effective_config.clone(),
            normalized.clone(),
            self.state.provider_config.provider_metadata_mode_input,
        );
        std::thread::spawn(move || {
            let request_base_url = normalized.clone();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop model-discovery runtime");
            let result = runtime.block_on(async move {
                let models = fetch_provider_model_infos(&config, &request_base_url)
                    .await
                    .map_err(|error| error.to_string())?;
                let availability_report =
                    check_model_availability(&config, None, Some(&request_base_url), false).await;
                Ok(DesktopProviderModelLoad {
                    models,
                    availability_report,
                })
            });
            let _ = runtime_tx.send(RuntimeMessage::ModelCatalogLoaded {
                requested_base_url: normalized,
                result,
            });
        });
    }

    pub(crate) fn check_startup_docling(&mut self) {
        let config = self.state.provider_config.effective_config.docling.clone();
        if !config.enabled {
            self.state.begin_startup_docling_check();
            return;
        }
        let normalized = normalize_docling_base_url(&config.base_url);
        if normalized.is_empty() {
            self.state
                .fail_startup_docling_check("Docling Serve URL が未設定です。");
            return;
        }
        if !self.state.begin_startup_docling_check() {
            return;
        }
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let requested_base_url = normalized.clone();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop docling-probe runtime");
            let result = runtime.block_on(async move {
                probe_docling_readiness(config)
                    .await
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::StartupDoclingChecked {
                requested_base_url,
                result,
            });
        });
    }

    pub(crate) fn apply_provider_session(&mut self) {
        let setup_overlay = self.state.view.startup_overlay_forced;
        let Some(config) = self.apply_provider_selection_to_effective_config() else {
            return;
        };
        self.state.reset_effective_config(config);
        self.state.mark_startup_config_reviewed();
        self.load_provider_models();
        self.check_startup_docling();
        self.state
            .set_status_message("applied provider selection to this UI session; checking provider");
        if !setup_overlay {
            self.state.hide_overlay();
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
                self.reload_config();
                self.state.mark_startup_config_reviewed();
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
                self.check_startup_docling();
                self.state
                    .set_status_message("applied override to this UI session");
            }
            Err(error) => self
                .state
                .set_status_message(format!("config error: {error}")),
        }
    }

    pub(crate) fn toggle_access_mode_session(&mut self) {
        let mut config = self.state.provider_config.effective_config.clone();
        config.permissions.access_mode = config.permissions.access_mode.next();
        let access_mode = config.permissions.access_mode;
        self.state.reset_effective_config(config);
        if let Some(live_config) = &self.active_live_config {
            live_config.set_access_mode(access_mode);
        }
        let auto_approved = self
            .pending_permission_request
            .as_ref()
            .is_some_and(|request| {
                crate::tool::context::access_mode_allows_permission(access_mode, request)
            });
        if auto_approved {
            if let Some(response) = self.permission_response.take() {
                let _ = response.send(true);
            }
            self.pending_permission_request = None;
            self.state.clear_permission();
        }
        let scope = if self.active_live_config.is_some() {
            "active run"
        } else {
            "UI session"
        };
        let suffix = if auto_approved {
            "; pending confirmation approved"
        } else {
            ""
        };
        self.state.set_status_message(format!(
            "{scope} access mode set to {}{suffix}",
            access_mode.label()
        ));
    }

    pub(crate) fn save_global_config(&mut self) {
        match self.state.provider_config.config_editor.save_scope(
            &self.app.workspace.root,
            crate::tui::config_editor::ConfigSaveScope::Global,
        ) {
            Ok(message) => {
                self.reload_config();
                self.state.mark_startup_config_reviewed();
                self.state.set_status_message(message);
            }
            Err(error) => self
                .state
                .set_status_message(format!("config save failed: {error}")),
        }
    }

    pub(crate) fn import_global_config_toml_dialog(&mut self) {
        match pick_config_toml_file() {
            Ok(Some(path)) => match import_global_config_toml(&path) {
                Ok(message) => {
                    self.reload_config();
                    self.state.mark_startup_config_reviewed();
                    self.state.set_status_message(message);
                }
                Err(error) => self
                    .state
                    .set_status_message(format!("config import failed: {error}")),
            },
            Ok(None) => {}
            Err(error) => self
                .state
                .set_status_message(format!("config import failed: {error}")),
        }
    }

    fn reload_config(&mut self) {
        match ConfigLoader::load(&self.app.workspace.root, None) {
            Ok(config) => {
                self.app.config = config.clone();
                self.state.reset_effective_config(config);
                if !self
                    .state
                    .provider_config
                    .provider_base_url_input
                    .trim()
                    .is_empty()
                {
                    self.load_provider_models();
                }
                self.check_startup_docling();
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
        hydrated_model_config.provider_metadata_mode =
            self.state.provider_config.provider_metadata_mode_input;
        if let Some(info) = self.state.selected_provider_model_info() {
            apply_provider_model_info_to_config(&mut hydrated_model_config, info);
        }
        let context_window = match parse_provider_limit_input(
            "context_window",
            &self.state.provider_config.provider_context_window_input,
        ) {
            Ok(value) => value,
            Err(message) => {
                self.state.set_status_message(message);
                return None;
            }
        };
        let max_output_tokens = match parse_provider_limit_input(
            "max_output_tokens",
            &self.state.provider_config.provider_max_output_tokens_input,
        ) {
            Ok(value) => value,
            Err(message) => {
                self.state.set_status_message(message);
                return None;
            }
        };
        hydrated_model_config.context_window = context_window;
        hydrated_model_config.max_output_tokens = max_output_tokens;
        hydrated_model_config.extra_body_json = Some(extra_body_with_num_ctx(
            hydrated_model_config.extra_body_json.clone(),
            context_window,
        ));
        Some(PartialResolvedConfig {
            model: Some(PartialModelConfig {
                base_url: Some(base_url),
                model: Some(model),
                provider_metadata_mode: Some(hydrated_model_config.provider_metadata_mode),
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
        self.pending_permission_request = None;
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
            self.pending_permission_request = None;
            self.state.clear_permission();
            requested = true;
        }
        if requested {
            self.active_live_config = None;
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
            if review_request.is_none() && !prompt.trim().is_empty() {
                self.launch_active_turn_steer(prompt, prompt_dispatch);
            } else {
                self.state.set_status_message(
                    "前回の停止処理を片付けています。状態が更新されてから再度実行してください。",
                );
            }
            return;
        }
        if review_request.is_none()
            && !prompt.trim().is_empty()
            && self.state.app_state.current_session_id.is_some()
            && matches!(
                self.state.app_state.run_status,
                crate::tui::state::RunStatus::Running
            )
        {
            self.launch_active_turn_steer(prompt, prompt_dispatch);
            return;
        }
        if prompt.trim().is_empty() && review_request.is_none() {
            return;
        }
        let image_paths = self.state.composer.image_attachment_paths.clone();
        let cancel = build_cancel_token();
        let live_config = LiveConfigOverrides::new(
            self.state
                .provider_config
                .effective_config
                .permissions
                .access_mode,
        );
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
            live_config: Some(live_config.clone()),
        };
        self.active_run_cancel = Some(cancel);
        self.active_live_config = Some(live_config);
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

    fn launch_active_turn_steer(
        &mut self,
        prompt: String,
        prompt_dispatch: crate::session::PromptDispatchPart,
    ) {
        let Some(session_id) = self.state.app_state.current_session_id else {
            self.state
                .set_status_message("実行中のセッションが見つからないため steer できません。");
            return;
        };
        let image_paths = self.state.composer.image_attachment_paths.clone();
        self.state.push_local_prompt_dispatch(&prompt_dispatch);
        self.state.composer.draft_prompt.clear();
        self.state.composer.image_attachment_paths.clear();
        self.state.composer.image_attachment_input.clear();
        self.state
            .set_status_message("実行中の turn に追加入力を送信しました。");
        let run_service = self.app.run_service.clone();
        let runtime_tx = self.runtime_tx.clone();
        let cwd = self.app.workspace.cwd.clone();
        std::thread::spawn(move || {
            let mut renderer = DesktopSteerRenderer;
            let mut prompt_ui = DesktopConfirmationPrompt {
                tx: runtime_tx.clone(),
            };
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop steer runtime");
            let result = runtime
                .block_on(async move {
                    run_service
                        .execute(
                            AppCommand::SessionSteer(SessionSteerRequest {
                                session_id,
                                prompt,
                                cwd,
                                image_paths,
                                client_user_message_id: Some(format!(
                                    "desktop-steer-{}",
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
                    loaded.turn_page_offset,
                    loaded.turn_page_limit,
                    loaded.turn_page_total,
                    loaded.turn_page_has_more,
                );
                if reason == SessionLoadReason::CurrentRefresh {
                    self.state.clear_post_run_refresh_pending();
                    return;
                }
                self.state.set_status_message(match reason {
                    SessionLoadReason::RunningRejoin => {
                        format!("rejoined running session {}", session_id)
                    }
                    SessionLoadReason::UserSelection => format!("opened session {}", session_id),
                    SessionLoadReason::CurrentRefresh => {
                        format!("refreshed session {}", session_id)
                    }
                });
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
            SessionLoadReason::UserSelection | SessionLoadReason::RunningRejoin => {
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
        self.state = DesktopState::new(loaded.snapshot, self.app.config.clone());
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
                    let live_refresh_session_id = event
                        .session_id()
                        .or(self.state.app_state.current_session_id);
                    self.state.apply_run_event(&event);
                    if live_event_requires_canonical_refresh(&event)
                        && live_refresh_session_id == self.state.app_state.current_session_id
                    {
                        if let Some(session_id) = live_refresh_session_id {
                            if run_event_is_terminal(&event) {
                                self.spawn_latest_live_session_refresh(session_id);
                            } else {
                                let detail = self.state.selected_detail();
                                let limit = if detail.turn_page_limit == 0 {
                                    DESKTOP_TURN_PAGE_LIMIT
                                } else {
                                    detail.turn_page_limit
                                };
                                self.spawn_live_session_refresh(
                                    session_id,
                                    detail.turn_page_offset,
                                    limit,
                                );
                            }
                        }
                    }
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
                        self.active_live_config = None;
                        self.pending_permission_request = None;
                        self.state.finish_agent_run();
                        self.state.mark_post_run_refresh_pending();
                        self.state.app_state.set_summary(summary);
                        self.refresh_current_session_after_terminal_run();
                    }
                    Err(error) => {
                        self.active_run_cancel = None;
                        self.active_live_config = None;
                        self.pending_permission_request = None;
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
                    self.pending_permission_request = Some(request.clone());
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
                RuntimeMessage::SessionArchived {
                    session_id,
                    archived,
                    result,
                } => {
                    self.state.finish_session_archive_mutation();
                    match result {
                        Ok(snapshot) => {
                            let archived_was_current = archived
                                && self.state.app_state.current_session_id == Some(session_id);
                            self.state.replace_snapshot(snapshot);
                            if archived_was_current {
                                if let Some(next_session_id) = self.state.selected_session_id() {
                                    self.state.set_status_message(format!(
                                        "archived chat {}; opening {}...",
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
                                    self.state.set_status_message(format!(
                                        "archived chat {}",
                                        session_id
                                    ));
                                }
                            } else {
                                self.state.set_status_message(if archived {
                                    format!("archived chat {}", session_id)
                                } else {
                                    format!("unarchived chat {}", session_id)
                                });
                            }
                        }
                        Err(error) => self
                            .state
                            .set_status_message(format!("chat archive failed: {error}")),
                    }
                }
                RuntimeMessage::SessionRolledBack { session_id, result } => {
                    self.state.finish_session_rollback_mutation();
                    match result {
                        Ok(rolled_back) => {
                            self.state.replace_snapshot(rolled_back.snapshot);
                            if self.state.selected_session_id() == Some(session_id)
                                && !self.session_load_is_blocked_by_active_run()
                            {
                                let loaded = rolled_back.loaded;
                                self.state.load_open_session(
                                    &loaded.session,
                                    &loaded.transcript,
                                    &loaded.turn_items,
                                    loaded.state,
                                    loaded.todos,
                                    loaded.turn_page_offset,
                                    loaded.turn_page_limit,
                                    loaded.turn_page_total,
                                    loaded.turn_page_has_more,
                                );
                            }
                            self.state.set_status_message(format!(
                                "rolled back {} turn(s) in chat {}",
                                rolled_back.dropped_turn_count, session_id
                            ));
                        }
                        Err(error) => self
                            .state
                            .set_status_message(format!("chat rollback failed: {error}")),
                    }
                }
                RuntimeMessage::SessionOperationApplied { session_id, result } => {
                    self.state.finish_session_maintenance_mutation();
                    match result {
                        Ok(applied) => {
                            self.state.replace_snapshot(applied.snapshot);
                            if self.state.selected_session_id() == Some(session_id)
                                && !self.session_load_is_blocked_by_active_run()
                            {
                                let loaded = applied.loaded;
                                self.state.load_open_session(
                                    &loaded.session,
                                    &loaded.transcript,
                                    &loaded.turn_items,
                                    loaded.state,
                                    loaded.todos,
                                    loaded.turn_page_offset,
                                    loaded.turn_page_limit,
                                    loaded.turn_page_total,
                                    loaded.turn_page_has_more,
                                );
                            }
                            self.state.set_status_message(applied.message);
                        }
                        Err(error) => self
                            .state
                            .set_status_message(format!("session operation failed: {error}")),
                    }
                }
                RuntimeMessage::SessionSearchLoaded(result) => {
                    self.state.finish_session_search();
                    match result {
                        Ok(snapshot) => self.state.replace_snapshot(snapshot),
                        Err(error) => self
                            .state
                            .set_status_message(format!("session search failed: {error}")),
                    }
                }
                RuntimeMessage::TurnPageLoaded { session_id, result } => match result {
                    Ok(loaded) => {
                        if self.state.selected_session_id() == Some(session_id)
                            && !self.session_load_is_blocked_by_active_run()
                        {
                            self.state.load_open_session(
                                &loaded.session,
                                &loaded.transcript,
                                &loaded.turn_items,
                                loaded.state,
                                loaded.todos,
                                loaded.turn_page_offset,
                                loaded.turn_page_limit,
                                loaded.turn_page_total,
                                loaded.turn_page_has_more,
                            );
                            self.state.set_status_message(format!(
                                "loaded turn page {}-{} of {}",
                                loaded.turn_page_offset.saturating_add(1),
                                loaded
                                    .turn_page_offset
                                    .saturating_add(loaded.turn_items.len()),
                                loaded.turn_page_total
                            ));
                        }
                    }
                    Err(error) => self
                        .state
                        .set_status_message(format!("turn page load failed: {error}")),
                },
                RuntimeMessage::LiveSessionRefreshed { session_id, result } => match result {
                    Ok(loaded) => {
                        if self.state.app_state.current_session_id == Some(session_id) {
                            if !self.session_load_is_blocked_by_active_run()
                                && loaded.turn_page_has_more
                            {
                                self.spawn_latest_live_session_refresh(session_id);
                                continue;
                            }
                            self.state.refresh_open_session_projection(
                                &loaded.session,
                                &loaded.transcript,
                                &loaded.turn_items,
                                loaded.state,
                                loaded.todos,
                                loaded.turn_page_offset,
                                loaded.turn_page_limit,
                                loaded.turn_page_total,
                                loaded.turn_page_has_more,
                            );
                        }
                    }
                    Err(error) => self
                        .state
                        .set_status_message(format!("live session refresh failed: {error}")),
                },
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
                                self.state =
                                    DesktopState::new(loaded.snapshot, self.app.config.clone());
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
                        Ok(load) => {
                            self.state
                                .finish_startup_provider_model_load(&load.availability_report);
                            self.state.finish_provider_model_load(load.models);
                        }
                        Err(error) => {
                            self.state.fail_startup_provider_model_load(error.clone());
                            self.state.fail_provider_model_load(error);
                        }
                    }
                }
                RuntimeMessage::StartupDoclingChecked {
                    requested_base_url,
                    result,
                } => {
                    let current = normalize_docling_base_url(
                        &self.state.provider_config.effective_config.docling.base_url,
                    );
                    if !self.state.provider_config.effective_config.docling.enabled
                        || current != requested_base_url
                    {
                        continue;
                    }
                    match result {
                        Ok(()) => {
                            self.state.finish_startup_docling_check(&requested_base_url);
                        }
                        Err(error) => {
                            self.state.fail_startup_docling_check(error.clone());
                            self.state.set_status_message(format!(
                                "Docling startup check failed: {error}"
                            ));
                        }
                    }
                }
                RuntimeMessage::SteerStored(result) => match result {
                    Ok(()) => {
                        self.state
                            .set_status_message("追加入力を実行中の turn に保存しました。");
                    }
                    Err(error) => {
                        self.state
                            .set_status_message(format!("追加入力の保存に失敗しました: {error}"));
                    }
                },
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

fn loaded_session_from_detail(
    (
        session,
        transcript,
        turn_items,
        state,
        todos,
        turn_page_offset,
        turn_page_limit,
        turn_page_total,
        turn_page_has_more,
    ): LoadedSessionDetail,
) -> LoadedSession {
    LoadedSession {
        session,
        transcript,
        turn_items,
        state,
        todos,
        turn_page_offset,
        turn_page_limit,
        turn_page_total,
        turn_page_has_more,
    }
}

async fn load_session_operation_projection(
    app: &App,
    session_id: SessionId,
    message: String,
) -> Result<DesktopSessionOperationLoaded, String> {
    let snapshot = load_snapshot_for_selection(app, Some(session_id))
        .await
        .map_err(|error| error.to_string())?;
    let (
        session,
        transcript,
        turn_items,
        state,
        todos,
        turn_page_offset,
        turn_page_limit,
        turn_page_total,
        turn_page_has_more,
    ) = load_session_detail(app, session_id)
        .await
        .map_err(|error| error.to_string())?;
    Ok(DesktopSessionOperationLoaded {
        snapshot,
        loaded: LoadedSession {
            session,
            transcript,
            turn_items,
            state,
            todos,
            turn_page_offset,
            turn_page_limit,
            turn_page_total,
            turn_page_has_more,
        },
        message,
    })
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
    let mut events = Vec::new();
    for row in rows {
        events.extend(markdown_events_for_transcript_row(row));
    }
    if !file_changes.is_empty() && !rows.iter().any(|row| row.kind == "file_changes") {
        events.push(MarkdownExportEvent::detail(
            "ファイル変更履歴",
            render_file_change_markdown_lines(file_changes),
        ));
    }
    let metadata = vec![
        MarkdownMetadataLine::new("Workspace", format!("`{workspace}`")),
        MarkdownMetadataLine::new("Session", format!("`{session_id}`")),
        MarkdownMetadataLine::new("Provider", format!("`{provider_base_url}`")),
        MarkdownMetadataLine::new("Model", format!("`{model}`")),
    ];
    render_codex_turn_block_markdown(title, &events, &metadata)
}

fn markdown_events_for_transcript_row(row: &DesktopTranscriptRow) -> Vec<MarkdownExportEvent> {
    match row.kind.as_str() {
        "user" => vec![MarkdownExportEvent::user(export_visible_body(&row.body))],
        "assistant" => vec![MarkdownExportEvent::assistant(export_visible_body(
            &row.body,
        ))],
        "file_changes" => vec![MarkdownExportEvent::detail(
            row.title.clone(),
            transcript_detail_body(row),
        )],
        "work_summary_failed" => vec![
            MarkdownExportEvent::detail(row.title.clone(), transcript_detail_body(row)),
            MarkdownExportEvent::terminal(
                MarkdownTerminalStatus::Failed,
                transcript_terminal_summary(row),
            ),
        ],
        "work_summary_cancelled" => vec![
            MarkdownExportEvent::detail(row.title.clone(), transcript_detail_body(row)),
            MarkdownExportEvent::terminal(
                MarkdownTerminalStatus::Interrupted,
                transcript_terminal_summary(row),
            ),
        ],
        "work_summary_awaiting_user" => vec![
            MarkdownExportEvent::detail(row.title.clone(), transcript_detail_body(row)),
            MarkdownExportEvent::terminal(
                MarkdownTerminalStatus::AwaitingUser,
                transcript_terminal_summary(row),
            ),
        ],
        "work_summary_completed" => vec![
            MarkdownExportEvent::detail(row.title.clone(), transcript_detail_body(row)),
            MarkdownExportEvent::terminal(
                transcript_terminal_status_from_body(row)
                    .unwrap_or(MarkdownTerminalStatus::Completed),
                transcript_terminal_summary(row),
            ),
        ],
        "tool" | "editing" | "diff" | "summary" => {
            vec![MarkdownExportEvent::detail(
                row.title.clone(),
                transcript_detail_body(row),
            )]
        }
        _ => vec![MarkdownExportEvent::detail(
            row.title.clone(),
            transcript_detail_body(row),
        )],
    }
}

fn transcript_detail_body(row: &DesktopTranscriptRow) -> String {
    match row.kind.as_str() {
        "file_changes" if !row.file_changes.is_empty() => {
            render_file_change_markdown_lines(&row.file_changes)
        }
        _ => {
            let body = export_visible_body(&row.body);
            if body.is_empty() {
                "_内容はありません。_".to_string()
            } else {
                body
            }
        }
    }
}

fn render_file_change_markdown_lines(changes: &[super::models::DesktopFileChangeRow]) -> String {
    let mut body = String::new();
    for change in changes {
        body.push_str("- ");
        body.push_str(&markdown_heading_text(&format!(
            "{} `{}`",
            codex_change_verb(&change.action),
            change.path
        )));
        if !change.summary.trim().is_empty() {
            body.push_str(" - ");
            body.push_str(&markdown_heading_text(&change.summary));
        }
        body.push('\n');
    }
    body
}

fn transcript_terminal_status_from_body(
    row: &DesktopTranscriptRow,
) -> Option<MarkdownTerminalStatus> {
    let lower = row.body.to_ascii_lowercase();
    if lower.contains("cancelled") || row.body.contains("停止しました") {
        Some(MarkdownTerminalStatus::Interrupted)
    } else if lower.contains("failed") || row.body.contains("失敗しました") {
        Some(MarkdownTerminalStatus::Failed)
    } else if lower.contains("awaiting user") || row.body.contains("確認待ち") {
        Some(MarkdownTerminalStatus::AwaitingUser)
    } else {
        None
    }
}

fn transcript_terminal_summary(row: &DesktopTranscriptRow) -> String {
    row.body
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("- 結果:")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| row.body.trim().to_string())
}

fn codex_change_verb(action: &str) -> &'static str {
    let normalized = action.trim().to_ascii_lowercase();
    if normalized.contains("add") || action.contains("追加") || action.contains("作成") {
        "Wrote"
    } else if normalized.contains("delete") || action.contains("削除") {
        "Deleted"
    } else {
        "Edited"
    }
}

fn export_visible_body(body: &str) -> String {
    body.trim().to_string()
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

fn provider_catalog_probe_config(
    mut config: ResolvedConfig,
    base_url: String,
    provider_metadata_mode: crate::config::ProviderMetadataMode,
) -> ResolvedConfig {
    config.model.base_url = base_url;
    config.model.provider_metadata_mode = provider_metadata_mode;
    config
}

#[cfg(test)]
mod tests {
    use super::{
        DesktopProviderModelLoad, RuntimeMessage, RuntimeMessageAsyncContract,
        fallback_workspace_after_project_delete, first_restorable_project_root,
        notification_session_title, open_transcript_rows_to_markdown,
        provider_catalog_probe_config, run_completion_notification_body,
        run_terminal_event_notification_body, transcript_markdown_file_name,
    };
    use crate::config::{ProviderMetadataMode, ResolvedConfig};
    use crate::desktop::models::DesktopTranscriptRowKind;
    use crate::desktop::models::{DesktopFileChangeRow, DesktopTranscriptRow};
    use crate::llm::{ModelAvailabilityReport, ModelAvailabilityStatus};
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
                result: Ok(DesktopProviderModelLoad {
                    models: Vec::new(),
                    availability_report: ModelAvailabilityReport {
                        gate: "model_availability".to_string(),
                        status: ModelAvailabilityStatus::Pass,
                        generated_by: "desktop_app_test".to_string(),
                        model: "qwen/qwen3.6-35b-a3b".to_string(),
                        base_url: "http://127.0.0.1:1234".to_string(),
                        provider_metadata_mode: ProviderMetadataMode::LmStudioNativeRequired,
                        v1_present: true,
                        native_present: true,
                        require_vision: false,
                        vision_capable: false,
                        vision_probe_passed: false,
                        vision_probes: Vec::new(),
                        tool_use_capable: Some(true),
                        capability_overrides: Vec::new(),
                        tool_call_probe_passed: true,
                        tool_call_probes: Vec::new(),
                        reasoning_capable: Some(false),
                        context: Some(131072),
                        max_output_tokens: Some(8192),
                        max_parallel_predictions: Some(1),
                        matched_model: None,
                        v1_models: Vec::new(),
                        native_models: Vec::new(),
                        openai_error: None,
                        native_error: None,
                        checked_at_ms: 0,
                    },
                }),
            }
            .async_contract(),
            RuntimeMessageAsyncContract::ProviderOperation
        );
        assert_eq!(
            RuntimeMessage::StartupDoclingChecked {
                requested_base_url: "http://127.0.0.1:8123".to_string(),
                result: Ok(()),
            }
            .async_contract(),
            RuntimeMessageAsyncContract::ProviderOperation
        );
        assert_eq!(
            RuntimeMessage::SteerStored(Ok(())).async_contract(),
            RuntimeMessageAsyncContract::BackgroundOperation
        );
        assert_eq!(
            RuntimeMessage::LiveSessionRefreshed {
                session_id: crate::session::SessionId::new(),
                result: Err("not loaded".to_string()),
            }
            .async_contract(),
            RuntimeMessageAsyncContract::RunStream
        );
        assert_eq!(
            RuntimeMessage::Finished(Err("failed".to_string())).async_contract(),
            RuntimeMessageAsyncContract::TerminalRun
        );
    }

    #[test]
    fn provider_catalog_probe_uses_current_provider_mode_input() {
        let mut config = ResolvedConfig::default();
        config.model.base_url = "http://old-provider:1234".to_string();
        config.model.provider_metadata_mode = ProviderMetadataMode::OpenAiCompatibleOnly;

        let probe_config = provider_catalog_probe_config(
            config,
            "http://127.0.0.1:8110".to_string(),
            ProviderMetadataMode::LmStudioNativeRequired,
        );

        assert_eq!(probe_config.model.base_url, "http://127.0.0.1:8110");
        assert_eq!(
            probe_config.model.provider_metadata_mode,
            ProviderMetadataMode::LmStudioNativeRequired
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
                row_kind: DesktopTranscriptRowKind::User,
                kind: "user".to_string(),
                step: "01".to_string(),
                title: "Prompt".to_string(),
                body: "Older request.".to_string(),
                file_changes: Vec::new(),
            },
            DesktopTranscriptRow {
                row_kind: DesktopTranscriptRowKind::Assistant,
                kind: "assistant".to_string(),
                step: "02".to_string(),
                title: "Previous response".to_string(),
                body: "Earlier answer.".to_string(),
                file_changes: Vec::new(),
            },
            DesktopTranscriptRow {
                row_kind: DesktopTranscriptRowKind::User,
                kind: "user".to_string(),
                step: "03".to_string(),
                title: "Prompt".to_string(),
                body: "Create a report.".to_string(),
                file_changes: Vec::new(),
            },
            DesktopTranscriptRow {
                row_kind: DesktopTranscriptRowKind::Assistant,
                kind: "assistant".to_string(),
                step: "04".to_string(),
                title: "Response".to_string(),
                body: "Done.\nSaved files.".to_string(),
                file_changes: Vec::new(),
            },
        ];

        let markdown = open_transcript_rows_to_markdown(
            "Session #1",
            &Utf8PathBuf::from("C:/workspace"),
            session_id,
            "http://127.0.0.1:1234",
            "qwen/qwen3.6-35b-a3b",
            &rows,
            &[],
        );

        assert!(markdown.contains("# Session \\#1"));
        assert!(
            markdown.find("> Older request.").unwrap()
                < markdown.find("> Create a report.").unwrap(),
            "visible transcript export should preserve chronological user turn blocks"
        );
        assert!(markdown.contains("> Create a report."));
        assert!(
            markdown.find("Earlier answer.").unwrap()
                < markdown.find("> Create a report.").unwrap(),
            "assistant closeout for an earlier turn should not be folded under the latest user request"
        );
        assert!(markdown.contains("<details><summary>実行情報</summary>"));
        assert!(markdown.contains("- Provider: `http://127.0.0.1:1234`"));
        assert!(markdown.contains("Done.\nSaved files."));
        assert!(
            transcript_markdown_file_name("Session #1", session_id).ends_with(".md"),
            "transcript export should always use markdown extension"
        );
    }

    #[test]
    fn open_transcript_markdown_preserves_visible_evidence() {
        let session_id = crate::session::SessionId::new();
        let rows = vec![
            DesktopTranscriptRow {
                row_kind: DesktopTranscriptRowKind::User,
                kind: "user".to_string(),
                step: "01".to_string(),
                title: "Prompt".to_string(),
                body: "Create files.".to_string(),
                file_changes: Vec::new(),
            },
            DesktopTranscriptRow {
                row_kind: DesktopTranscriptRowKind::Assistant,
                kind: "assistant".to_string(),
                step: "02".to_string(),
                title: "Response".to_string(),
                body: "Now run this:\n<tool_call>\n<function=shell>\n</tool_call>".to_string(),
                file_changes: Vec::new(),
            },
            DesktopTranscriptRow {
                row_kind: DesktopTranscriptRowKind::Summary,
                kind: "summary".to_string(),
                step: "03".to_string(),
                title: "File changes".to_string(),
                body: "Added README.md\nAdded __pycache__\\workflow.cpython-313.pyc".to_string(),
                file_changes: Vec::new(),
            },
        ];
        let changes = vec![DesktopFileChangeRow {
            label: "README.md".to_string(),
            path: "README.md".to_string(),
            kind: crate::session::ChangeKind::Add,
            action: "追加".to_string(),
            summary: "Added README.md".to_string(),
            tool_call_ids: vec![crate::session::ToolCallId::new()],
        }];

        let markdown = open_transcript_rows_to_markdown(
            "Case2",
            &Utf8PathBuf::from("C:/workspace"),
            session_id,
            "http://127.0.0.1:1234",
            "qwen/qwen3.6-35b-a3b",
            &rows,
            &changes,
        );

        assert!(markdown.contains("ファイル変更履歴"));
        assert!(markdown.contains("README.md"));
        assert!(markdown.contains("Now run this:"));
        assert!(markdown.contains("<tool_call>"));
        assert!(markdown.contains("__pycache__"));
        assert!(markdown.contains(".pyc"));
        assert!(
            !markdown.contains("完了しました。"),
            "Desktop open transcript Markdown export must not synthesize clean closeout text when visible assistant evidence contains a malformed pseudo tool-call"
        );
    }

    #[test]
    fn open_transcript_markdown_uses_terminal_outcome_for_cancelled_turn() {
        let session_id = crate::session::SessionId::new();
        let rows = vec![
            DesktopTranscriptRow {
                row_kind: DesktopTranscriptRowKind::User,
                kind: "user".to_string(),
                step: "01".to_string(),
                title: "Prompt".to_string(),
                body: "Update the implementation.".to_string(),
                file_changes: Vec::new(),
            },
            DesktopTranscriptRow {
                row_kind: DesktopTranscriptRowKind::Assistant,
                kind: "assistant".to_string(),
                step: "02".to_string(),
                title: "Response".to_string(),
                body: "テストの期待値を修正します。".to_string(),
                file_changes: Vec::new(),
            },
            DesktopTranscriptRow {
                row_kind: DesktopTranscriptRowKind::WorkSummaryCancelled,
                kind: "work_summary_cancelled".to_string(),
                step: "03".to_string(),
                title: "作業履歴 / 作業サマリ".to_string(),
                body: "### 作業サマリ\n- 結果: run cancelled by user".to_string(),
                file_changes: Vec::new(),
            },
        ];

        let markdown = open_transcript_rows_to_markdown(
            "Cancelled Session",
            &Utf8PathBuf::from("C:/workspace"),
            session_id,
            "http://127.0.0.1:1234",
            "qwen/qwen3.6-35b-a3b",
            &rows,
            &[],
        );

        assert!(markdown.contains("停止しました: run cancelled by user"));
        assert!(
            markdown.find("テストの期待値を修正します。").unwrap()
                < markdown
                    .find("停止しました: run cancelled by user")
                    .unwrap(),
            "intermediate assistant intent must remain folded before terminal outcome"
        );
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
            metrics: Default::default(),
        };

        let body = run_completion_notification_body("  vision GUI  ", &summary);

        assert!(body.contains("vision GUI が完了しました。"));
        assert!(body.contains("変更: 2件"));
        assert!(body.contains("ツール: 3件 / 失敗 1件"));
        assert_eq!(notification_session_title(""), "タスク");
    }

    #[test]
    fn terminal_event_notification_body_uses_terminal_state() {
        let body = run_terminal_event_notification_body(
            "vision GUI",
            &RunEvent::SessionInterrupted {
                session_id: crate::session::SessionId::new(),
                reason: "user requested stop\nsecond line".to_string(),
            },
        )
        .expect("terminal event should produce a notification");

        assert_eq!(body, "vision GUI を停止しました: user requested stop");
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

struct DesktopSteerRenderer;

impl EventRenderer for DesktopSteerRenderer {
    fn render(&mut self, _event: &RunEvent) -> Result<(), CliRenderError> {
        Ok(())
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
    let mut deleted = false;
    for project in projects {
        if preferences.is_project_deleted(&project.root_path) {
            app.session_service
                .delete_project(project.id)
                .await
                .map_err(|error| error.to_string())?;
            deleted = true;
        }
    }
    if deleted {
        run_storage_maintenance_after_delete(app)?;
    }
    Ok(())
}

fn run_storage_maintenance_after_delete(app: &App) -> Result<(), String> {
    app.store
        .cleanup_orphan_internal_files()
        .map_err(|error| error.to_string())?;
    app.store
        .checkpoint_and_vacuum()
        .map_err(|error| error.to_string())?;
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

fn parse_provider_limit_input(label: &str, value: &str) -> Result<u32, String> {
    let trimmed = value.trim();
    let parsed = trimmed
        .parse::<u32>()
        .map_err(|_| format!("{label} must be a positive integer"))?;
    if parsed == 0 {
        return Err(format!("{label} must be greater than 0"));
    }
    Ok(parsed)
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

#[cfg(feature = "tauri-desktop")]
fn pick_config_toml_file() -> Result<Option<Utf8PathBuf>, String> {
    match rfd::FileDialog::new()
        .add_filter("moyAI config.toml", &["toml"])
        .set_file_name("config.toml")
        .pick_file()
    {
        Some(path) => Utf8PathBuf::from_path_buf(path)
            .map(Some)
            .map_err(|_| "selected config path is not valid UTF-8".to_string()),
        None => Ok(None),
    }
}

#[cfg(not(feature = "tauri-desktop"))]
fn pick_config_toml_file() -> Result<Option<Utf8PathBuf>, String> {
    Err("desktop config picker requires the tauri-desktop feature".to_string())
}

fn import_global_config_toml(source: &Utf8Path) -> Result<String, String> {
    let file_name = source
        .file_name()
        .ok_or_else(|| "selected file has no file name".to_string())?;
    if !file_name.eq_ignore_ascii_case("config.toml") {
        return Err("select a file named config.toml".to_string());
    }
    let text = fs::read_to_string(source.as_std_path()).map_err(|error| error.to_string())?;
    toml::from_str::<PartialResolvedConfig>(&text).map_err(|error| error.to_string())?;
    let target = global_config_path().map_err(|error| error.to_string())?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent.as_std_path()).map_err(|error| error.to_string())?;
    }
    fs::write(target.as_std_path(), text).map_err(|error| error.to_string())?;
    Ok(format!("imported config.toml to {}", target))
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

fn write_markdown_export_atomic(path: &Utf8Path, markdown: &str) -> Result<(), String> {
    let Some(parent) = path.parent().filter(|parent| !parent.as_str().is_empty()) else {
        return Err(format!(
            "markdown export path must have a parent directory: {path}"
        ));
    };
    std::fs::create_dir_all(parent.as_std_path()).map_err(|error| error.to_string())?;
    let mut temp =
        NamedTempFile::new_in(parent.as_std_path()).map_err(|error| error.to_string())?;
    temp.write_all(markdown.as_bytes())
        .map_err(|error| error.to_string())?;
    temp.as_file_mut()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temp.persist(path.as_std_path())
        .map(|_| ())
        .map_err(|error| error.error.to_string())
}

pub fn desktop_open_transcript_markdown_preserves_visible_evidence_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let rows = vec![
        DesktopTranscriptRow {
            row_kind: super::models::DesktopTranscriptRowKind::User,
            kind: "user".to_string(),
            step: "01".to_string(),
            title: "Prompt".to_string(),
            body: "Create files.".to_string(),
            file_changes: Vec::new(),
        },
        DesktopTranscriptRow {
            row_kind: super::models::DesktopTranscriptRowKind::Assistant,
            kind: "assistant".to_string(),
            step: "02".to_string(),
            title: "Response".to_string(),
            body: "Now run this:\n<tool_call>\n<function=shell>\n</tool_call>".to_string(),
            file_changes: Vec::new(),
        },
        DesktopTranscriptRow {
            row_kind: super::models::DesktopTranscriptRowKind::Summary,
            kind: "summary".to_string(),
            step: "03".to_string(),
            title: "File changes".to_string(),
            body: "Added README.md\nAdded __pycache__\\workflow.cpython-313.pyc".to_string(),
            file_changes: Vec::new(),
        },
    ];
    let changes = vec![super::models::DesktopFileChangeRow {
        label: "README.md".to_string(),
        path: "README.md".to_string(),
        kind: crate::session::ChangeKind::Add,
        action: "追加".to_string(),
        summary: "Added README.md".to_string(),
        tool_call_ids: vec![crate::session::ToolCallId::new()],
    }];
    let markdown = open_transcript_rows_to_markdown(
        "Markdown evidence fixture",
        &Utf8PathBuf::from("C:/workspace"),
        session_id,
        "http://127.0.0.1:1234",
        "qwen/qwen3.6-35b-a3b",
        &rows,
        &changes,
    );
    markdown.contains("Now run this:")
        && markdown.contains("<tool_call>")
        && markdown.contains("__pycache__")
        && markdown.contains(".pyc")
        && markdown.contains("ファイル変更履歴")
        && markdown.contains("README.md")
        && !markdown.contains("完了しました。")
}

pub fn desktop_markdown_export_atomic_commit_fixture_passes() -> bool {
    let unique = format!(
        "moyai-desktop-markdown-export-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    );
    let Ok(root) = Utf8PathBuf::from_path_buf(std::env::temp_dir().join(unique)) else {
        return false;
    };
    let path = root.join("exports").join("history.md");
    let result = (|| -> Result<bool, String> {
        write_markdown_export_atomic(&path, "# Desktop export\n\ncanonical evidence\n")?;
        let content =
            std::fs::read_to_string(path.as_std_path()).map_err(|error| error.to_string())?;
        Ok(content == "# Desktop export\n\ncanonical evidence\n")
    })();
    let _ = std::fs::remove_dir_all(root.as_std_path());
    result.unwrap_or(false)
}

pub fn desktop_app_current_provider_profile_fixture_passes() -> bool {
    let report = ModelAvailabilityReport {
        gate: "model_availability".to_string(),
        status: crate::llm::ModelAvailabilityStatus::Pass,
        generated_by: "desktop_app_fixture".to_string(),
        model: "qwen/qwen3.6-35b-a3b".to_string(),
        base_url: "http://127.0.0.1:1234".to_string(),
        provider_metadata_mode: crate::config::ProviderMetadataMode::LmStudioNativeRequired,
        v1_present: true,
        native_present: true,
        require_vision: false,
        vision_capable: false,
        vision_probe_passed: false,
        vision_probes: Vec::new(),
        tool_use_capable: Some(true),
        capability_overrides: Vec::new(),
        tool_call_probe_passed: true,
        tool_call_probes: Vec::new(),
        reasoning_capable: Some(false),
        context: Some(131072),
        max_output_tokens: Some(8192),
        max_parallel_predictions: Some(1),
        matched_model: None,
        v1_models: Vec::new(),
        native_models: Vec::new(),
        openai_error: None,
        native_error: None,
        checked_at_ms: 0,
    };
    let session_id = SessionId::new();
    let rows = vec![DesktopTranscriptRow {
        row_kind: super::models::DesktopTranscriptRowKind::Assistant,
        kind: "assistant".to_string(),
        step: "01".to_string(),
        title: "Response".to_string(),
        body: "Provider profile evidence is preserved.".to_string(),
        file_changes: Vec::new(),
    }];
    let markdown = open_transcript_rows_to_markdown(
        "Provider profile fixture",
        &Utf8PathBuf::from("C:/workspace"),
        session_id,
        &report.base_url,
        &report.model,
        &rows,
        &[],
    );
    report.provider_metadata_mode == crate::config::ProviderMetadataMode::LmStudioNativeRequired
        && report.base_url == "http://127.0.0.1:1234"
        && report.model == "qwen/qwen3.6-35b-a3b"
        && report.context == Some(131072)
        && report.max_output_tokens == Some(8192)
        && markdown.contains("http://127.0.0.1:1234")
        && markdown.contains("qwen/qwen3.6-35b-a3b")
}

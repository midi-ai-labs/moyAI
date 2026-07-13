use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Component, Path};
use std::process::Command as ProcessCommand;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use camino::{Utf8Path, Utf8PathBuf};

use crate::app::session_title::NEW_SESSION_PLACEHOLDER_TITLE;
use crate::app::{
    AgentActivityRecord, App, AppBootstrap, AppCommand, ReviewRequest, RunRequest,
    SessionSteerRequest,
};
use crate::cli::{ConfirmationPrompt, EventRenderer, OutputMode, SharedConfirmationPrompt};
use crate::config::loader::global_config_path;
use crate::config::merge::apply_patch as apply_config_patch;
use crate::config::model::{PartialModelConfig, PartialResolvedConfig, full_effective_override};
use crate::config::{ConfigLoader, ProviderMetadataMode, ResolvedConfig, ShellFamily};
use crate::docling::{normalize_docling_base_url, probe_docling_readiness};
use crate::error::{AppRunError, CliPromptError, CliRenderError};
use crate::llm::{
    ModelAvailabilityReport, ProviderModelInfo, apply_provider_model_info_to_config,
    check_model_availability, extra_body_with_num_ctx, fetch_provider_model_infos,
    normalize_provider_base_url,
};
use crate::runtime::{AgentStatus, LiveConfigOverrides, SystemClock, build_cancel_token};
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
use crate::tui::config_editor::{ConfigEditorState, ConfigField};
use crate::workspace::project::normalize_path;
use tauri::Manager;
use tempfile::NamedTempFile;
use tokio_util::sync::CancellationToken;

use super::args::{DesktopArgs, quick_chat_workspace_directory};
use super::async_ops::{
    DesktopAsyncOperationId, LatestRequestId, LatestRequestTracker, SessionSearchRequestId,
    SessionSearchRequestTracker,
};
use super::models::{DesktopSnapshot, DesktopTranscriptRow};
use super::navigation::NavigationRequestId;
use super::preferences::DesktopPreferences;
use super::query::{
    DESKTOP_TURN_PAGE_LIMIT, LoadedSessionDetail, load_latest_session_detail, load_session_detail,
    load_snapshot, load_snapshot_continue_last, load_snapshot_for_selection,
    load_snapshot_for_session_search,
};
use super::state::DesktopState;
use super::web_model::{
    DesktopRuntimeProjection, DesktopWebState, access_runtime_allows_mutation,
    access_runtime_owner_token, agent_activity_projection, desktop_web_state,
    navigation_admission_blocker,
};

enum RuntimeMessage {
    RunEvent {
        run_generation: u64,
        event: RunEvent,
    },
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
    EnhanceFinished {
        request_id: u64,
        target: DraftRequestTarget,
        result: Result<String, String>,
    },
    SnapshotLoaded {
        request_id: LatestRequestId,
        target: SnapshotRequestTarget,
        result: Result<super::models::DesktopSnapshot, String>,
    },
    SessionLoaded {
        request_id: NavigationRequestId,
        session_id: SessionId,
        reason: SessionLoadReason,
        result: Result<LoadedSession, String>,
    },
    CurrentSessionRefreshed {
        request_id: LatestRequestId,
        target: SessionRefreshRequestTarget,
        result: Result<LoadedSession, String>,
    },
    SessionDeleted {
        target: SessionDeleteRequestTarget,
        result: Result<super::models::DesktopSnapshot, String>,
    },
    SessionArchived {
        target: SessionMutationRequestTarget,
        archived: bool,
        result: Result<super::models::DesktopSnapshot, String>,
    },
    SessionRolledBack {
        target: SessionMutationRequestTarget,
        result: Result<DesktopRollbackLoaded, String>,
    },
    SessionOperationApplied {
        target: SessionMutationRequestTarget,
        result: Result<DesktopSessionOperationLoaded, String>,
    },
    TurnPageLoaded {
        request_id: LatestRequestId,
        target: SessionPageRequestTarget,
        result: Result<LoadedSession, String>,
    },
    LiveSessionRefreshed {
        request_id: LatestRequestId,
        target: SessionRefreshRequestTarget,
        result: Result<LoadedSession, String>,
    },
    DurableAgentActivityRefreshed {
        request_id: LatestRequestId,
        target: SessionRefreshRequestTarget,
        result: Result<Vec<AgentActivityRecord>, String>,
    },
    SessionSearchLoaded {
        request_id: SessionSearchRequestId,
        result: Result<super::models::DesktopSnapshot, String>,
    },
    ProjectDeleted {
        target: ProjectDeleteRequestTarget,
        result: Result<WorkspaceLoadResult, String>,
    },
    CurrentTodosLoaded {
        request_id: LatestRequestId,
        target: SessionRefreshRequestTarget,
        result: Result<Vec<TodoItem>, String>,
    },
    ModelCatalogLoaded {
        request_id: LatestRequestId,
        target: ProviderCatalogRequestTarget,
        result: Result<Vec<ProviderModelInfo>, String>,
    },
    StartupProviderChecked {
        request_id: LatestRequestId,
        target: ProviderReadinessRequestTarget,
        report: ModelAvailabilityReport,
    },
    StartupDoclingChecked {
        request_id: LatestRequestId,
        target: DoclingRequestTarget,
        result: Result<(), String>,
    },
    HistoryExported {
        request_id: LatestRequestId,
        target: HistoryExportRequestTarget,
        result: Result<Utf8PathBuf, String>,
    },
    WorkspaceSwitched {
        request_id: NavigationRequestId,
        result: Result<WorkspaceLoadResult, String>,
    },
    WorkspaceSwitchedForNewProjectSession {
        request_id: NavigationRequestId,
        result: Result<WorkspaceLoadResult, String>,
    },
    AccessModePersisted {
        request_id: LatestRequestId,
        target: AccessModePersistenceTarget,
        phase: AccessModePersistencePhase,
        worker: Arc<AccessModePersistenceWorker>,
        result: Result<Utf8PathBuf, String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SnapshotRequestTarget {
    workspace_root: Utf8PathBuf,
    selected_session_id: Option<SessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AccessModePersistenceTarget {
    operation_id: DesktopAsyncOperationId,
    workspace_root: Utf8PathBuf,
    session_id: Option<SessionId>,
    config_generation: u64,
    root_run_generation: Option<u64>,
    runtime_owner_token: String,
    old_global_access_mode: crate::config::AccessMode,
    old_effective_access_mode: crate::config::AccessMode,
    access_mode: crate::config::AccessMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccessModePersistencePhase {
    InitialOwners,
    AdoptedSession { session_id: SessionId },
}

struct PendingAccessModeAdoption {
    request_id: LatestRequestId,
    target: AccessModePersistenceTarget,
    remembered_path: Utf8PathBuf,
    worker: Arc<AccessModePersistenceWorker>,
}

type CompareAndSetGlobalAccessMode = Box<
    dyn FnMut(
            crate::config::AccessMode,
            crate::config::AccessMode,
        ) -> Result<Option<Utf8PathBuf>, String>
        + Send,
>;
type PersistRootSessionAccessMode =
    Box<dyn FnOnce(SessionId, crate::config::AccessMode) -> Result<(), String> + Send>;

struct AccessModePersistenceWorker {
    compare_and_set_global: Mutex<CompareAndSetGlobalAccessMode>,
    persist_session: Mutex<Option<PersistRootSessionAccessMode>>,
}

impl AccessModePersistenceWorker {
    fn new<CompareAndSetGlobal, PersistSession>(
        compare_and_set_global: CompareAndSetGlobal,
        persist_session: PersistSession,
    ) -> Self
    where
        CompareAndSetGlobal: FnMut(
                crate::config::AccessMode,
                crate::config::AccessMode,
            ) -> Result<Option<Utf8PathBuf>, String>
            + Send
            + 'static,
        PersistSession:
            FnOnce(SessionId, crate::config::AccessMode) -> Result<(), String> + Send + 'static,
    {
        Self {
            compare_and_set_global: Mutex::new(Box::new(compare_and_set_global)),
            persist_session: Mutex::new(Some(Box::new(persist_session))),
        }
    }

    fn persist_initial_owners(
        &self,
        target: &AccessModePersistenceTarget,
    ) -> Result<Utf8PathBuf, String> {
        persist_desktop_access_mode_owners(
            target.old_global_access_mode,
            target.access_mode,
            target.session_id,
            |expected, access_mode| self.compare_and_set_global(expected, access_mode),
            |session_id, access_mode| self.persist_session(session_id, access_mode),
        )
    }

    fn persist_adopted_session(
        &self,
        target: &AccessModePersistenceTarget,
        session_id: SessionId,
        remembered_path: Utf8PathBuf,
    ) -> Result<Utf8PathBuf, String> {
        if let Err(session_error) = self.persist_session(session_id, target.access_mode) {
            return match self
                .compare_and_set_global(target.access_mode, target.old_global_access_mode)
            {
                Ok(Some(_)) => Err(format!(
                    "adopted session access mode update failed and the global field was restored: {session_error}"
                )),
                Ok(None) => Err(format!(
                    "adopted session access mode update failed; the global field changed again and was not overwritten: {session_error}"
                )),
                Err(rollback_error) => Err(format!(
                    "adopted session access mode update failed and global compensation failed: {session_error}; {rollback_error}"
                )),
            };
        }
        Ok(remembered_path)
    }

    fn compare_and_set_global(
        &self,
        expected: crate::config::AccessMode,
        access_mode: crate::config::AccessMode,
    ) -> Result<Option<Utf8PathBuf>, String> {
        let mut compare_and_set = self
            .compare_and_set_global
            .lock()
            .map_err(|_| "global access mode persistence lock was poisoned".to_string())?;
        compare_and_set(expected, access_mode)
    }

    fn persist_session(
        &self,
        session_id: SessionId,
        access_mode: crate::config::AccessMode,
    ) -> Result<(), String> {
        let persist_session = self
            .persist_session
            .lock()
            .map_err(|_| "session access mode persistence lock was poisoned".to_string())?
            .take()
            .ok_or_else(|| "session access mode persistence was already consumed".to_string())?;
        persist_session(session_id, access_mode)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DraftRequestTarget {
    workspace_root: Utf8PathBuf,
    session_id: Option<SessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionPageRequestTarget {
    workspace_root: Utf8PathBuf,
    session_id: SessionId,
    offset: usize,
    limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionRefreshRequestTarget {
    workspace_root: Utf8PathBuf,
    session_id: SessionId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionDeleteRequestTarget {
    workspace_root: Utf8PathBuf,
    project_id: ProjectId,
    session_id: SessionId,
    operation_id: DesktopAsyncOperationId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionMutationRequestTarget {
    workspace_root: Utf8PathBuf,
    project_id: ProjectId,
    session_id: SessionId,
    operation_id: DesktopAsyncOperationId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectDeleteRequestTarget {
    workspace_root: Utf8PathBuf,
    owner_project_id: ProjectId,
    project_id: ProjectId,
    project_root: Utf8PathBuf,
    operation_id: DesktopAsyncOperationId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HistoryExportRequestTarget {
    workspace_root: Utf8PathBuf,
    session_id: SessionId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderCatalogRequestTarget {
    base_url: String,
    metadata_mode: ProviderMetadataMode,
    config_generation: u64,
    selected_model_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderReadinessRequestTarget {
    base_url: String,
    model_id: String,
    metadata_mode: ProviderMetadataMode,
    supports_tools: bool,
    supports_reasoning: bool,
    supports_images: bool,
    parallel_tool_calls: bool,
    max_parallel_predictions: u32,
    config_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DoclingRequestTarget {
    base_url: String,
    config_generation: u64,
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

fn unique_background_request_admission_open(
    request_owner_pending: bool,
    state_owner_pending: bool,
) -> bool {
    !request_owner_pending && !state_owner_pending
}

impl RuntimeMessage {
    fn async_contract(&self) -> RuntimeMessageAsyncContract {
        match self {
            RuntimeMessage::RunEvent { .. } => RuntimeMessageAsyncContract::RunStream,
            RuntimeMessage::Finished { .. } => RuntimeMessageAsyncContract::TerminalRun,
            RuntimeMessage::Permission { .. } | RuntimeMessage::PermissionCancelled { .. } => {
                RuntimeMessageAsyncContract::ModalDecision
            }
            RuntimeMessage::EnhanceFinished { .. } => {
                RuntimeMessageAsyncContract::BackgroundOperation
            }
            RuntimeMessage::SnapshotLoaded { .. } => {
                RuntimeMessageAsyncContract::StatusOnlyOperation
            }
            RuntimeMessage::SessionLoaded { .. } => {
                RuntimeMessageAsyncContract::NavigationOperation
            }
            RuntimeMessage::CurrentSessionRefreshed { .. } => {
                RuntimeMessageAsyncContract::BackgroundOperation
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
            RuntimeMessage::LiveSessionRefreshed { .. }
            | RuntimeMessage::DurableAgentActivityRefreshed { .. } => {
                RuntimeMessageAsyncContract::RunStream
            }
            RuntimeMessage::SessionSearchLoaded { .. } => {
                RuntimeMessageAsyncContract::NavigationOperation
            }
            RuntimeMessage::ProjectDeleted { .. } => {
                RuntimeMessageAsyncContract::BackgroundOperation
            }
            RuntimeMessage::CurrentTodosLoaded { .. } => {
                RuntimeMessageAsyncContract::BackgroundOperation
            }
            RuntimeMessage::ModelCatalogLoaded { .. }
            | RuntimeMessage::StartupProviderChecked { .. } => {
                RuntimeMessageAsyncContract::ProviderOperation
            }
            RuntimeMessage::StartupDoclingChecked { .. } => {
                RuntimeMessageAsyncContract::ProviderOperation
            }
            RuntimeMessage::HistoryExported { .. } => {
                RuntimeMessageAsyncContract::BackgroundOperation
            }
            RuntimeMessage::AccessModePersisted { .. } => {
                RuntimeMessageAsyncContract::BackgroundOperation
            }
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
    agent_activity_records: Option<Vec<AgentActivityRecord>>,
}

type LoadedAgentActivityRecords = (SessionId, Vec<AgentActivityRecord>);

fn activity_records_for_projection(
    root_session_id: SessionId,
    live_records: Vec<AgentActivityRecord>,
    loaded_records: Option<&LoadedAgentActivityRecords>,
) -> Vec<AgentActivityRecord> {
    if !live_records.is_empty() {
        return live_records;
    }
    loaded_records
        .filter(|(session_id, _)| *session_id == root_session_id)
        .map(|(_, records)| records.clone())
        .unwrap_or_default()
}

fn agent_activity_records_are_active(records: &[AgentActivityRecord]) -> bool {
    records.iter().any(|record| {
        matches!(
            &record.status,
            AgentStatus::PendingInit | AgentStatus::Running
        )
    })
}

fn durable_agent_activity_refresh_required(
    live_records: &[AgentActivityRecord],
    projected_records: &[AgentActivityRecord],
    refresh_pending: bool,
    terminal_refresh_pending: bool,
) -> bool {
    live_records.is_empty()
        && agent_activity_records_are_active(projected_records)
        && !refresh_pending
        && !terminal_refresh_pending
}

fn durable_agent_activity_retry_allowed(failures: u8) -> bool {
    failures < 3
}

fn next_config_generation(current: u64) -> u64 {
    current.saturating_add(1)
}

fn commit_effective_config(
    state: &mut DesktopState,
    run_lifecycle: &DesktopRunLifecycle,
    config: ResolvedConfig,
) {
    let access_mode = config.permissions.access_mode;
    state.reset_effective_config(config);
    run_lifecycle.set_access_mode(access_mode);
}

fn desktop_run_config_override(config: &ResolvedConfig) -> PartialResolvedConfig {
    full_effective_override(config)
}

fn persist_desktop_access_mode_owners<CompareAndSetGlobal, PersistSession>(
    old_global_access_mode: crate::config::AccessMode,
    access_mode: crate::config::AccessMode,
    current_root_session_id: Option<SessionId>,
    mut compare_and_set_global: CompareAndSetGlobal,
    persist_session: PersistSession,
) -> Result<Utf8PathBuf, String>
where
    CompareAndSetGlobal: FnMut(
        crate::config::AccessMode,
        crate::config::AccessMode,
    ) -> Result<Option<Utf8PathBuf>, String>,
    PersistSession: FnOnce(SessionId, crate::config::AccessMode) -> Result<(), String>,
{
    let remembered_path = match compare_and_set_global(old_global_access_mode, access_mode) {
        Ok(Some(path)) => path,
        Ok(None) => {
            return Err(
                "global access mode changed before this update; reload configuration and try again"
                    .to_string(),
            );
        }
        Err(error) => return Err(format!("global access mode update failed: {error}")),
    };
    let Some(session_id) = current_root_session_id else {
        return Ok(remembered_path);
    };
    if let Err(session_error) = persist_session(session_id, access_mode) {
        return match compare_and_set_global(access_mode, old_global_access_mode) {
            Ok(Some(_)) => Err(format!(
                "session access mode update failed and the global field was restored: {session_error}"
            )),
            Ok(None) => Err(format!(
                "session access mode update failed; the global field changed again and was not overwritten: {session_error}"
            )),
            Err(rollback_error) => Err(format!(
                "session access mode update failed and global compensation failed: {session_error}; {rollback_error}"
            )),
        };
    }
    Ok(remembered_path)
}

fn session_search_result_can_apply(
    is_latest: bool,
    root_run_active: bool,
    agent_tree_active: bool,
) -> bool {
    is_latest && !root_run_active && !agent_tree_active
}

fn apply_session_search_result(
    state: &mut DesktopState,
    is_latest: bool,
    root_run_active: bool,
    agent_tree_active: bool,
    result: Result<DesktopSnapshot, String>,
) -> bool {
    if !session_search_result_can_apply(is_latest, root_run_active, agent_tree_active) {
        return false;
    }
    match result {
        Ok(snapshot) => state.replace_snapshot_preserving_current_owner(snapshot),
        Err(error) => state.set_status_message(format!("session search failed: {error}")),
    }
    true
}

fn finish_steer_submission(
    state: &mut DesktopState,
    prompt_dispatch: &crate::session::PromptDispatchPart,
    image_paths: &[Utf8PathBuf],
    result: Result<(), String>,
) -> bool {
    match result {
        Ok(()) => {
            state.push_local_prompt_dispatch(prompt_dispatch);
            state
                .composer
                .image_attachment_paths
                .retain(|path| !image_paths.contains(path));
            state.set_status_message("追加入力を実行中の turn に保存しました。");
            true
        }
        Err(error) => {
            state.set_status_message(format!("追加入力の保存に失敗しました: {error}"));
            false
        }
    }
}

fn finish_durable_agent_activity_refresh_request(
    tracker: &mut LatestRequestTracker<SessionRefreshRequestTarget>,
    request_id: LatestRequestId,
    target: &SessionRefreshRequestTarget,
    workspace_root: &Utf8Path,
    current_session_id: Option<SessionId>,
) -> bool {
    tracker.finish_if_current(request_id, target)
        && target.workspace_root == workspace_root
        && current_session_id == Some(target.session_id)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DesktopRootRunPhase {
    Running,
    Finalizing,
}

struct DesktopRootRun {
    generation: u64,
    cancel: CancellationToken,
    live_config: LiveConfigOverrides,
    phase: DesktopRootRunPhase,
}

struct PendingRootSubmission {
    run_generation: u64,
    owner_workspace_path: Utf8PathBuf,
    owner_session_id: Option<SessionId>,
    prompt_dispatch: crate::session::PromptDispatchPart,
    image_paths: Vec<Utf8PathBuf>,
    cancel_prompt_review_on_commit: bool,
}

#[derive(Default)]
struct DesktopRunLifecycle {
    root: Option<DesktopRootRun>,
}

impl DesktopRunLifecycle {
    fn begin(
        &mut self,
        generation: u64,
        cancel: CancellationToken,
        live_config: LiveConfigOverrides,
    ) {
        self.root = Some(DesktopRootRun {
            generation,
            cancel,
            live_config,
            phase: DesktopRootRunPhase::Running,
        });
    }

    fn root_is_active(&self) -> bool {
        self.root.is_some()
    }

    fn root_generation(&self) -> Option<u64> {
        self.root.as_ref().map(|run| run.generation)
    }

    fn owns(&self, generation: u64) -> bool {
        self.root
            .as_ref()
            .is_some_and(|run| run.generation == generation)
    }

    fn root_is_finalizing(&self) -> bool {
        self.root
            .as_ref()
            .is_some_and(|run| run.phase == DesktopRootRunPhase::Finalizing)
    }

    fn can_steer_root(&self) -> bool {
        self.root
            .as_ref()
            .is_some_and(|run| run.phase == DesktopRootRunPhase::Running)
    }

    fn cancellation_requested(&self) -> bool {
        self.root
            .as_ref()
            .is_some_and(|run| run.cancel.is_cancelled())
    }

    fn live_config(&self) -> Option<&LiveConfigOverrides> {
        self.root.as_ref().map(|run| &run.live_config)
    }

    fn set_access_mode(&self, access_mode: crate::config::AccessMode) {
        if let Some(live_config) = self.live_config() {
            live_config.set_access_mode(access_mode);
        }
    }

    fn request_cancel(&mut self) -> bool {
        let Some(run) = self.root.as_mut() else {
            return false;
        };
        run.cancel.cancel();
        run.phase = DesktopRootRunPhase::Finalizing;
        true
    }

    fn observe_terminal_event(&mut self) {
        if let Some(run) = self.root.as_mut() {
            run.phase = DesktopRootRunPhase::Finalizing;
        }
    }

    fn finish_root(&mut self) {
        self.root = None;
    }
}

fn advance_projection_revision(revision: &mut u64) -> Result<u64, String> {
    let next = revision
        .checked_add(1)
        .ok_or_else(|| "desktop projection revision exhausted u64 range".to_string())?;
    *revision = next;
    Ok(next)
}

fn projection_revision_text(revision: u64) -> String {
    revision.to_string()
}

fn attachment_authorizations_to_revoke(
    authorized: &BTreeSet<Utf8PathBuf>,
    desired: &BTreeSet<Utf8PathBuf>,
) -> Vec<Utf8PathBuf> {
    authorized.difference(desired).cloned().collect()
}

fn session_delete_target_matches(
    target: &SessionDeleteRequestTarget,
    workspace_root: &Utf8Path,
    project_id: ProjectId,
) -> bool {
    target.workspace_root == workspace_root && target.project_id == project_id
}

fn session_mutation_target_matches(
    target: &SessionMutationRequestTarget,
    workspace_root: &Utf8Path,
    project_id: ProjectId,
) -> bool {
    target.workspace_root == workspace_root && target.project_id == project_id
}

#[cfg(test)]
fn access_mode_persistence_target_matches(
    target: &AccessModePersistenceTarget,
    workspace_root: &Utf8Path,
    session_id: Option<SessionId>,
    config_generation: u64,
    runtime_owner_token: &str,
) -> bool {
    access_mode_persistence_target_relation(
        target,
        workspace_root,
        session_id,
        config_generation,
        runtime_owner_token,
    ) == AccessModePersistenceTargetRelation::Exact
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccessModePersistenceTargetRelation {
    Exact,
    AdoptedSession(SessionId),
    Stale,
}

fn access_mode_persistence_target_relation(
    target: &AccessModePersistenceTarget,
    workspace_root: &Utf8Path,
    session_id: Option<SessionId>,
    config_generation: u64,
    runtime_owner_token: &str,
) -> AccessModePersistenceTargetRelation {
    if target.workspace_root != workspace_root
        || target.config_generation != config_generation
        || target.runtime_owner_token != runtime_owner_token
    {
        return AccessModePersistenceTargetRelation::Stale;
    }
    match (target.session_id, session_id) {
        (target_session_id, current_session_id) if target_session_id == current_session_id => {
            AccessModePersistenceTargetRelation::Exact
        }
        (None, Some(session_id)) => AccessModePersistenceTargetRelation::AdoptedSession(session_id),
        _ => AccessModePersistenceTargetRelation::Stale,
    }
}

fn project_delete_target_matches(
    target: &ProjectDeleteRequestTarget,
    workspace_root: &Utf8Path,
    owner_project_id: ProjectId,
) -> bool {
    target.workspace_root == workspace_root && target.owner_project_id == owner_project_id
}

fn finish_session_delete_request(
    state: &mut DesktopState,
    target: &SessionDeleteRequestTarget,
    workspace_root: &Utf8Path,
    project_id: ProjectId,
) -> bool {
    session_delete_target_matches(target, workspace_root, project_id)
        && state.finish_session_delete_mutation(target.operation_id)
}

fn finish_history_export_request(
    tracker: &mut LatestRequestTracker<HistoryExportRequestTarget>,
    request_id: LatestRequestId,
    target: &HistoryExportRequestTarget,
    workspace_root: &Utf8Path,
) -> Option<bool> {
    if !tracker.finish_if_current(request_id, target) {
        return None;
    }
    Some(target.workspace_root == workspace_root)
}

fn finish_navigation_failure(
    state: &mut DesktopState,
    request_id: NavigationRequestId,
    error: impl Into<String>,
) -> bool {
    if !state.is_current_navigation(request_id) {
        return false;
    }
    state.restore_selected_session_to_current_owner();
    if !state.finish_navigation(request_id) {
        return false;
    }
    state.set_status_message(error);
    true
}

#[cfg(test)]
mod command_projection_owner_tests {
    use super::*;

    #[test]
    fn terminal_event_moves_root_run_to_finalizing_without_admitting_steer() {
        let cancel = CancellationToken::new();
        let live = LiveConfigOverrides::new(crate::config::AccessMode::Default);
        let live_observer = live.clone();
        let mut lifecycle = DesktopRunLifecycle::default();
        lifecycle.begin(7, cancel, live);
        assert!(lifecycle.owns(7));
        assert!(!lifecycle.owns(6));
        assert_eq!(lifecycle.root_generation(), Some(7));
        assert!(lifecycle.can_steer_root());
        lifecycle
            .live_config()
            .expect("active root live config")
            .set_access_mode(crate::config::AccessMode::AutoReview);
        assert_eq!(
            live_observer.access_mode(),
            crate::config::AccessMode::AutoReview,
            "the root and agent runtime share the live access override"
        );
        lifecycle.set_access_mode(crate::config::AccessMode::FullAccess);
        assert_eq!(
            live_observer.access_mode(),
            crate::config::AccessMode::FullAccess
        );

        lifecycle.observe_terminal_event();

        assert!(lifecycle.root_is_active());
        assert!(lifecycle.root_is_finalizing());
        assert!(!lifecycle.can_steer_root());
        assert_eq!(
            navigation_admission_blocker(
                false,
                false,
                false,
                false,
                lifecycle.root_is_finalizing(),
            ),
            Some("the current run is finalizing")
        );
        lifecycle.finish_root();
        assert!(!lifecycle.root_is_active());
        assert_eq!(lifecycle.root_generation(), None);
        assert_eq!(
            navigation_admission_blocker(
                false,
                false,
                false,
                false,
                lifecycle.root_is_finalizing(),
            ),
            None
        );
    }

    #[test]
    fn pre_admission_root_owns_cancellation_before_any_run_event() {
        let cancel = CancellationToken::new();
        let observer = cancel.clone();
        let mut lifecycle = DesktopRunLifecycle::default();
        lifecycle.begin(
            12,
            cancel,
            LiveConfigOverrides::new(crate::config::AccessMode::Default),
        );

        assert_eq!(lifecycle.root_generation(), Some(12));
        assert!(lifecycle.root_is_active());
        assert!(!observer.is_cancelled());
        assert!(lifecycle.request_cancel());
        assert!(observer.is_cancelled());
        assert!(lifecycle.root_is_finalizing());
    }

    #[test]
    fn settings_effective_config_commit_updates_active_live_access_owner() {
        let mut state = DesktopState::new(
            DesktopSnapshot {
                workspace_path: "C:/workspace".to_string(),
                provider_label: String::new(),
                model_label: String::new(),
                command_rows: Vec::new(),
                project_rows: Vec::new(),
                selected_project_index: 0,
                session_rows: Vec::new(),
                chat_session_rows: Vec::new(),
                session_details: Vec::new(),
                selected_session_index: 0,
            },
            ResolvedConfig::default(),
        );
        let live = LiveConfigOverrides::new(crate::config::AccessMode::Default);
        let mut lifecycle = DesktopRunLifecycle::default();
        lifecycle.begin(1, CancellationToken::new(), live.clone());
        let mut candidate = ResolvedConfig::default();
        candidate.permissions.access_mode = crate::config::AccessMode::FullAccess;

        commit_effective_config(&mut state, &lifecycle, candidate);

        assert_eq!(
            state
                .provider_config
                .effective_config
                .permissions
                .access_mode,
            crate::config::AccessMode::FullAccess
        );
        assert_eq!(live.access_mode(), crate::config::AccessMode::FullAccess);
    }

    #[test]
    fn cancelled_desktop_run_does_not_emit_a_failure_notification() {
        assert!(desktop_run_failure_notification_allowed(false));
        assert!(!desktop_run_failure_notification_allowed(true));
    }

    #[test]
    fn current_session_access_change_persists_global_then_session() {
        let session_id = SessionId::new();
        let remembered = Arc::new(std::sync::Mutex::new(Vec::new()));
        let persisted = Arc::new(std::sync::Mutex::new(Vec::new()));
        let result = persist_desktop_access_mode_owners(
            crate::config::AccessMode::Default,
            crate::config::AccessMode::FullAccess,
            Some(session_id),
            {
                let remembered = remembered.clone();
                move |expected, mode| {
                    remembered
                        .lock()
                        .expect("remembered")
                        .push((expected, mode));
                    Ok(Some(Utf8PathBuf::from("C:/config.toml")))
                }
            },
            {
                let persisted = persisted.clone();
                move |owner, mode| {
                    persisted.lock().expect("persisted").push((owner, mode));
                    Ok(())
                }
            },
        );

        assert_eq!(result, Ok(Utf8PathBuf::from("C:/config.toml")));
        assert_eq!(
            *remembered.lock().expect("remembered"),
            vec![(
                crate::config::AccessMode::Default,
                crate::config::AccessMode::FullAccess
            )]
        );
        assert_eq!(
            *persisted.lock().expect("persisted"),
            vec![(session_id, crate::config::AccessMode::FullAccess)]
        );
    }

    #[test]
    fn no_session_access_change_persists_only_global_owner() {
        let session_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let result = persist_desktop_access_mode_owners(
            crate::config::AccessMode::Default,
            crate::config::AccessMode::AutoReview,
            None,
            |expected, mode| {
                assert_eq!(expected, crate::config::AccessMode::Default);
                assert_eq!(mode, crate::config::AccessMode::AutoReview);
                Ok(Some(Utf8PathBuf::from("C:/config.toml")))
            },
            {
                let session_calls = session_calls.clone();
                move |_, _| {
                    session_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
        );

        assert_eq!(result, Ok(Utf8PathBuf::from("C:/config.toml")));
        assert_eq!(session_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn access_persistence_completion_requires_the_same_full_owner_target() {
        let session_id = SessionId::new();
        let target = AccessModePersistenceTarget {
            operation_id: DesktopAsyncOperationId::from_test_value(1),
            workspace_root: Utf8PathBuf::from("C:/workspace"),
            session_id: Some(session_id),
            config_generation: 7,
            root_run_generation: Some(4),
            runtime_owner_token: "root:4".to_string(),
            old_global_access_mode: crate::config::AccessMode::Default,
            old_effective_access_mode: crate::config::AccessMode::Default,
            access_mode: crate::config::AccessMode::FullAccess,
        };

        assert!(access_mode_persistence_target_matches(
            &target,
            Utf8Path::new("C:/workspace"),
            Some(session_id),
            7,
            "root:4",
        ));
        assert!(!access_mode_persistence_target_matches(
            &target,
            Utf8Path::new("C:/workspace"),
            Some(SessionId::new()),
            7,
            "root:4",
        ));
        assert!(!access_mode_persistence_target_matches(
            &target,
            Utf8Path::new("C:/workspace"),
            Some(session_id),
            8,
            "root:4",
        ));
        assert!(!access_mode_persistence_target_matches(
            &target,
            Utf8Path::new("C:/workspace"),
            Some(session_id),
            7,
            "root:5",
        ));

        let pre_admission_target = AccessModePersistenceTarget {
            session_id: None,
            ..target
        };
        assert_eq!(
            access_mode_persistence_target_relation(
                &pre_admission_target,
                Utf8Path::new("C:/workspace"),
                Some(session_id),
                7,
                "root:4",
            ),
            AccessModePersistenceTargetRelation::AdoptedSession(session_id)
        );
        assert_eq!(
            access_mode_persistence_target_relation(
                &pre_admission_target,
                Utf8Path::new("C:/workspace"),
                Some(session_id),
                8,
                "root:4",
            ),
            AccessModePersistenceTargetRelation::Stale
        );
    }

    #[test]
    fn global_access_failure_does_not_touch_the_current_session() {
        let session_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let result = persist_desktop_access_mode_owners(
            crate::config::AccessMode::Default,
            crate::config::AccessMode::FullAccess,
            Some(SessionId::new()),
            |_, _| Err("global failed".to_string()),
            {
                let session_calls = session_calls.clone();
                move |_, _| {
                    session_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
        );

        assert_eq!(
            result,
            Err("global access mode update failed: global failed".to_string())
        );
        assert_eq!(session_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn session_access_failure_compensates_the_global_field() {
        let remembered = Arc::new(std::sync::Mutex::new(Vec::new()));
        let result = persist_desktop_access_mode_owners(
            crate::config::AccessMode::Default,
            crate::config::AccessMode::FullAccess,
            Some(SessionId::new()),
            {
                let remembered = remembered.clone();
                move |expected, mode| {
                    remembered
                        .lock()
                        .expect("remembered")
                        .push((expected, mode));
                    Ok(Some(Utf8PathBuf::from("C:/config.toml")))
                }
            },
            |_, _| Err("session failed".to_string()),
        );

        assert_eq!(
            result,
            Err("session access mode update failed and the global field was restored: session failed"
                .to_string())
        );
        assert_eq!(
            *remembered.lock().expect("remembered"),
            vec![
                (
                    crate::config::AccessMode::Default,
                    crate::config::AccessMode::FullAccess
                ),
                (
                    crate::config::AccessMode::FullAccess,
                    crate::config::AccessMode::Default
                )
            ]
        );
    }

    #[test]
    fn adopted_session_access_failure_uses_the_same_cas_compensation() {
        let remembered = Arc::new(std::sync::Mutex::new(Vec::new()));
        let worker = AccessModePersistenceWorker::new(
            {
                let remembered = remembered.clone();
                move |expected, mode| {
                    remembered
                        .lock()
                        .expect("remembered")
                        .push((expected, mode));
                    Ok(Some(Utf8PathBuf::from("C:/config.toml")))
                }
            },
            |_, _| Err("adopted session failed".to_string()),
        );
        let target = AccessModePersistenceTarget {
            operation_id: DesktopAsyncOperationId::from_test_value(1),
            workspace_root: Utf8PathBuf::from("C:/workspace"),
            session_id: None,
            config_generation: 1,
            root_run_generation: Some(1),
            runtime_owner_token: "root:1".to_string(),
            old_global_access_mode: crate::config::AccessMode::Default,
            old_effective_access_mode: crate::config::AccessMode::Default,
            access_mode: crate::config::AccessMode::FullAccess,
        };

        let path = worker
            .persist_initial_owners(&target)
            .expect("global-only first phase");
        let error = worker
            .persist_adopted_session(&target, SessionId::new(), path)
            .expect_err("adopted session failure");

        assert!(error.contains("global field was restored"));
        assert_eq!(
            *remembered.lock().expect("remembered"),
            vec![
                (
                    crate::config::AccessMode::Default,
                    crate::config::AccessMode::FullAccess
                ),
                (
                    crate::config::AccessMode::FullAccess,
                    crate::config::AccessMode::Default
                )
            ]
        );
    }

    #[tokio::test]
    async fn desktop_current_session_access_is_durable_for_tui_reopen_and_rejects_child_owner() {
        use crate::session::{NewSession, SessionRepository as _};

        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        std::fs::create_dir_all(&root).expect("workspace");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
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
        let initial_access_mode = crate::config::AccessMode::Default;
        let args = DesktopArgs {
            directory: Some(root.clone()),
            session_id: None,
            continue_last: false,
            global_config_existed_at_launch: true,
        };
        let mut controller = DesktopController::new_with_preferences_and_persistence(
            app,
            args,
            DesktopPreferences::default(),
            false,
        )
        .await
        .expect("controller");
        let create_session = |title: &str| NewSession {
            project_id: controller.app.workspace.project_id,
            title: title.to_string(),
            cwd: root.clone(),
            model: controller.app.config.model.model.clone(),
            base_url: controller.app.config.model.base_url.clone(),
            access_mode: initial_access_mode,
        };
        let repository = controller.app.store.session_repo();
        let root_session = repository
            .create_session(create_session("root"))
            .await
            .expect("root session");
        let child_session = repository
            .create_session(create_session("child"))
            .await
            .expect("child session");
        repository
            .insert_session_spawn_edge(
                root_session.id,
                root_session.id,
                child_session.id,
                "/root/child",
                "child",
            )
            .await
            .expect("child edge");
        controller.app.config.permissions.access_mode = initial_access_mode;
        controller
            .state
            .provider_config
            .update_access_mode(initial_access_mode);
        controller.state.app_state.current_session_id = Some(root_session.id);
        let live = LiveConfigOverrides::new(initial_access_mode);
        controller
            .run_lifecycle
            .begin(1, CancellationToken::new(), live.clone());
        let expected_access_mode = initial_access_mode.next();
        let session_service = controller.app.session_service.clone();
        let persisted_service = session_service.clone();

        assert!(controller.start_access_mode_persistence(
            move |expected, access_mode| {
                assert_eq!(expected, initial_access_mode);
                assert_eq!(access_mode, expected_access_mode);
                Ok(Some(Utf8PathBuf::from("C:/config.toml")))
            },
            move |session_id, access_mode| {
                std::thread::spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| error.to_string())?;
                    runtime.block_on(async move {
                        persisted_service
                            .update_root_session_access_mode(session_id, access_mode)
                            .await
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                    })
                })
                .join()
                .map_err(|_| "session worker panicked".to_string())?
            },
        ));
        for _ in 0..200 {
            controller.drain_runtime_messages();
            if !controller.state.background_mutation_pending() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!controller.state.background_mutation_pending());

        let reopened = session_service
            .get_session(root_session.id)
            .await
            .expect("TUI durable reopen source");
        assert_eq!(reopened.access_mode, expected_access_mode);
        assert_eq!(
            controller
                .state
                .provider_config
                .effective_config
                .permissions
                .access_mode,
            reopened.access_mode
        );
        assert_eq!(live.access_mode(), reopened.access_mode);
        assert!(
            session_service
                .update_root_session_access_mode(child_session.id, expected_access_mode)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn pre_admission_access_change_persists_the_same_root_session_adopted_before_completion()
    {
        use crate::session::{NewSession, SessionRepository as _};

        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        std::fs::create_dir_all(&root).expect("workspace");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
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
        let initial_access_mode = crate::config::AccessMode::Default;
        let args = DesktopArgs {
            directory: Some(root.clone()),
            session_id: None,
            continue_last: false,
            global_config_existed_at_launch: true,
        };
        let mut controller = DesktopController::new_with_preferences_and_persistence(
            app,
            args,
            DesktopPreferences::default(),
            false,
        )
        .await
        .expect("controller");
        let session = controller
            .app
            .store
            .session_repo()
            .create_session(NewSession {
                project_id: controller.app.workspace.project_id,
                title: "adopted root".to_string(),
                cwd: root,
                model: controller.app.config.model.model.clone(),
                base_url: controller.app.config.model.base_url.clone(),
                access_mode: initial_access_mode,
            })
            .await
            .expect("session");
        controller
            .app
            .store
            .session_repo()
            .admit_session_run(session.id)
            .await
            .expect("active root admission")
            .expect("active root admitted");
        controller.app.config.permissions.access_mode = initial_access_mode;
        controller
            .state
            .provider_config
            .update_access_mode(initial_access_mode);
        let live = LiveConfigOverrides::new(initial_access_mode);
        controller
            .run_lifecycle
            .begin(1, CancellationToken::new(), live.clone());
        let expected_access_mode = initial_access_mode.next();
        let persisted_service = controller.app.session_service.clone();
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);

        assert!(controller.start_access_mode_persistence(
            move |expected, access_mode| {
                assert_eq!(expected, initial_access_mode);
                assert_eq!(access_mode, expected_access_mode);
                started_tx.send(()).expect("signal global worker");
                release_rx.recv().expect("release global worker");
                Ok(Some(Utf8PathBuf::from("C:/config.toml")))
            },
            move |session_id, access_mode| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())?;
                runtime.block_on(async move {
                    persisted_service
                        .update_root_session_access_mode(session_id, access_mode)
                        .await
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                })
            },
        ));
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("global worker started");
        controller
            .runtime_tx
            .send(RuntimeMessage::RunEvent {
                run_generation: 1,
                event: RunEvent::SessionStarted {
                    session_id: session.id,
                    title: session.title.clone(),
                },
            })
            .expect("session adoption event");
        controller.drain_runtime_messages();
        assert_eq!(
            controller.state.app_state.current_session_id,
            Some(session.id)
        );
        release_tx.send(()).expect("release global worker");
        for _ in 0..300 {
            controller.drain_runtime_messages();
            if !controller.state.background_mutation_pending() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        assert!(!controller.state.background_mutation_pending());
        assert_eq!(
            controller
                .app
                .session_service
                .get_session(session.id)
                .await
                .expect("durable adopted root")
                .access_mode,
            expected_access_mode
        );
        assert_eq!(
            controller
                .state
                .provider_config
                .effective_config
                .permissions
                .access_mode,
            expected_access_mode
        );
        assert_eq!(live.access_mode(), expected_access_mode);

        controller.next_root_run_generation = 2;
        controller.run_lifecycle.finish_root();
        let finished_root_target = AccessModePersistenceTarget {
            operation_id: DesktopAsyncOperationId::from_test_value(99),
            workspace_root: controller.app.workspace.root.clone(),
            session_id: None,
            config_generation: controller.state.provider_config.config_generation,
            root_run_generation: Some(1),
            runtime_owner_token: "root:1".to_string(),
            old_global_access_mode: initial_access_mode,
            old_effective_access_mode: initial_access_mode,
            access_mode: expected_access_mode,
        };
        assert_eq!(
            controller.access_mode_persistence_target_relation(&finished_root_target),
            AccessModePersistenceTargetRelation::AdoptedSession(session.id),
            "completion from the just-finished generation retains its exact admitted owner"
        );
        controller.next_root_run_generation = 3;
        assert_eq!(
            controller.access_mode_persistence_target_relation(&finished_root_target),
            AccessModePersistenceTargetRelation::Stale,
            "a newer root generation revokes the terminal completion grace"
        );
    }

    #[tokio::test]
    async fn pre_admission_access_change_waits_for_session_started_after_global_completion() {
        use crate::session::{NewSession, SessionRepository as _};

        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        std::fs::create_dir_all(&root).expect("workspace");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
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
        let initial_access_mode = crate::config::AccessMode::Default;
        let args = DesktopArgs {
            directory: Some(root.clone()),
            session_id: None,
            continue_last: false,
            global_config_existed_at_launch: true,
        };
        let mut controller = DesktopController::new_with_preferences_and_persistence(
            app,
            args,
            DesktopPreferences::default(),
            false,
        )
        .await
        .expect("controller");
        let session = controller
            .app
            .store
            .session_repo()
            .create_session(NewSession {
                project_id: controller.app.workspace.project_id,
                title: "late adopted root".to_string(),
                cwd: root,
                model: controller.app.config.model.model.clone(),
                base_url: controller.app.config.model.base_url.clone(),
                access_mode: initial_access_mode,
            })
            .await
            .expect("session");
        controller
            .app
            .store
            .session_repo()
            .admit_session_run(session.id)
            .await
            .expect("late active root admission")
            .expect("late active root admitted");
        controller.app.config.permissions.access_mode = initial_access_mode;
        controller
            .state
            .provider_config
            .update_access_mode(initial_access_mode);
        let live = LiveConfigOverrides::new(initial_access_mode);
        controller
            .run_lifecycle
            .begin(1, CancellationToken::new(), live.clone());
        let expected_access_mode = initial_access_mode.next();
        let persisted_service = controller.app.session_service.clone();

        assert!(controller.start_access_mode_persistence(
            move |expected, access_mode| {
                assert_eq!(expected, initial_access_mode);
                assert_eq!(access_mode, expected_access_mode);
                Ok(Some(Utf8PathBuf::from("C:/config.toml")))
            },
            move |session_id, access_mode| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())?;
                runtime.block_on(async move {
                    persisted_service
                        .update_root_session_access_mode(session_id, access_mode)
                        .await
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                })
            },
        ));
        for _ in 0..200 {
            controller.drain_runtime_messages();
            if controller.pending_access_mode_adoption.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(controller.pending_access_mode_adoption.is_some());
        assert!(controller.state.background_mutation_pending());
        assert_eq!(
            controller
                .state
                .provider_config
                .effective_config
                .permissions
                .access_mode,
            initial_access_mode,
            "the live owner is not committed until both durable owners succeed"
        );
        assert_eq!(live.access_mode(), initial_access_mode);

        controller
            .runtime_tx
            .send(RuntimeMessage::RunEvent {
                run_generation: 1,
                event: RunEvent::SessionStarted {
                    session_id: session.id,
                    title: session.title.clone(),
                },
            })
            .expect("late session adoption event");
        for _ in 0..300 {
            controller.drain_runtime_messages();
            if !controller.state.background_mutation_pending() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        assert!(!controller.state.background_mutation_pending());
        assert!(controller.pending_access_mode_adoption.is_none());
        assert_eq!(
            controller
                .app
                .session_service
                .get_session(session.id)
                .await
                .expect("durable late adopted root")
                .access_mode,
            expected_access_mode
        );
        assert_eq!(
            controller
                .state
                .provider_config
                .effective_config
                .permissions
                .access_mode,
            expected_access_mode
        );
        assert_eq!(live.access_mode(), expected_access_mode);
    }

    async fn empty_access_test_controller() -> (tempfile::TempDir, Utf8PathBuf, DesktopController) {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        std::fs::create_dir_all(&root).expect("workspace");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
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
        let args = DesktopArgs {
            directory: Some(root.clone()),
            session_id: None,
            continue_last: false,
            global_config_existed_at_launch: true,
        };
        let controller = DesktopController::new_with_preferences_and_persistence(
            app,
            args,
            DesktopPreferences::default(),
            false,
        )
        .await
        .expect("controller");
        (temp, root, controller)
    }

    #[tokio::test]
    async fn pending_access_adoption_without_session_started_settles_global_only_on_finished() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let initial_access_mode = crate::config::AccessMode::Default;
        let expected_access_mode = initial_access_mode.next();
        controller.app.config.permissions.access_mode = initial_access_mode;
        controller
            .state
            .provider_config
            .update_access_mode(initial_access_mode);
        let live = LiveConfigOverrides::new(initial_access_mode);
        controller
            .run_lifecycle
            .begin(1, CancellationToken::new(), live.clone());
        let session_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        assert!(controller.start_access_mode_persistence(
            move |expected, access_mode| {
                assert_eq!(expected, initial_access_mode);
                assert_eq!(access_mode, expected_access_mode);
                Ok(Some(Utf8PathBuf::from("C:/config.toml")))
            },
            {
                let session_calls = session_calls.clone();
                move |_, _| {
                    session_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            },
        ));
        for _ in 0..200 {
            controller.drain_runtime_messages();
            if controller.pending_access_mode_adoption.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(controller.pending_access_mode_adoption.is_some());
        assert!(controller.state.background_mutation_pending());
        assert_eq!(live.access_mode(), initial_access_mode);

        controller
            .runtime_tx
            .send(RuntimeMessage::Finished {
                run_generation: 1,
                result: Err("failed before session admission".to_string()),
            })
            .expect("pre-admission worker finish");
        controller.drain_runtime_messages();

        assert!(controller.pending_access_mode_adoption.is_none());
        assert!(!controller.state.background_mutation_pending());
        assert_eq!(controller.state.app_state.current_session_id, None);
        assert_eq!(session_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            controller.app.config.permissions.access_mode,
            expected_access_mode
        );
        assert_eq!(
            controller
                .state
                .provider_config
                .effective_config
                .permissions
                .access_mode,
            expected_access_mode
        );
        assert_eq!(live.access_mode(), expected_access_mode);
    }

    #[tokio::test]
    async fn delayed_adopted_access_completion_is_discarded_after_next_root_generation_starts() {
        use crate::session::{NewSession, SessionRepository as _};

        let (_temp, root, mut controller) = empty_access_test_controller().await;
        let reloaded_access_mode = controller.app.config.permissions.access_mode;
        let initial_access_mode = [
            crate::config::AccessMode::Default,
            crate::config::AccessMode::AutoReview,
            crate::config::AccessMode::FullAccess,
        ]
        .into_iter()
        .find(|access_mode| access_mode.next() != reloaded_access_mode)
        .expect("one transition differs from the reloaded access owner");
        let expected_access_mode = initial_access_mode.next();
        assert_ne!(expected_access_mode, reloaded_access_mode);
        controller.app.config.permissions.access_mode = initial_access_mode;
        controller
            .state
            .provider_config
            .update_access_mode(initial_access_mode);
        let session = controller
            .app
            .store
            .session_repo()
            .create_session(NewSession {
                project_id: controller.app.workspace.project_id,
                title: "delayed adopted root".to_string(),
                cwd: root,
                model: controller.app.config.model.model.clone(),
                base_url: controller.app.config.model.base_url.clone(),
                access_mode: initial_access_mode,
            })
            .await
            .expect("session");
        let session_id = session.id;
        let session_title = session.title.clone();
        controller.run_lifecycle.begin(
            1,
            CancellationToken::new(),
            LiveConfigOverrides::new(initial_access_mode),
        );
        let (adopted_started_tx, adopted_started_rx) = mpsc::sync_channel(1);
        let (release_adopted_tx, release_adopted_rx) = mpsc::sync_channel(1);

        assert!(controller.start_access_mode_persistence(
            move |expected, access_mode| {
                assert_eq!(expected, initial_access_mode);
                assert_eq!(access_mode, expected_access_mode);
                Ok(Some(Utf8PathBuf::from("C:/config.toml")))
            },
            move |persisted_session_id, access_mode| {
                assert_eq!(persisted_session_id, session_id);
                assert_eq!(access_mode, expected_access_mode);
                adopted_started_tx.send(()).expect("adopted worker started");
                release_adopted_rx.recv().expect("release adopted worker");
                Ok(())
            },
        ));
        for _ in 0..200 {
            controller.drain_runtime_messages();
            if controller.pending_access_mode_adoption.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let old_target = controller
            .pending_access_mode_adoption
            .as_ref()
            .expect("pending adopted owner")
            .target
            .clone();
        controller
            .runtime_tx
            .send(RuntimeMessage::RunEvent {
                run_generation: 1,
                event: RunEvent::SessionStarted {
                    session_id,
                    title: session_title,
                },
            })
            .expect("session adoption event");
        controller.drain_runtime_messages();
        adopted_started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("adopted worker dispatch");
        assert!(controller.pending_access_mode_adoption.is_none());
        assert!(controller.state.background_mutation_pending());

        controller
            .runtime_tx
            .send(RuntimeMessage::Finished {
                run_generation: 1,
                result: Err("first root finished".to_string()),
            })
            .expect("first root finish");
        controller.drain_runtime_messages();
        assert!(!controller.run_lifecycle.root_is_active());

        let next_live = LiveConfigOverrides::new(initial_access_mode);
        controller
            .run_lifecycle
            .begin(2, CancellationToken::new(), next_live.clone());
        controller.next_root_run_generation = 3;
        assert_eq!(
            controller.access_mode_persistence_target_relation(&old_target),
            AccessModePersistenceTargetRelation::Stale
        );

        release_adopted_tx.send(()).expect("release adopted worker");
        for _ in 0..200 {
            controller.drain_runtime_messages();
            if !controller.state.background_mutation_pending() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        assert!(!controller.state.background_mutation_pending());
        assert!(!controller.access_mode_persistence_requests.is_pending());
        assert_eq!(controller.run_lifecycle.root_generation(), Some(2));
        let current_access_mode = controller.app.config.permissions.access_mode;
        assert_eq!(current_access_mode, reloaded_access_mode);
        assert_ne!(current_access_mode, expected_access_mode);
        assert_eq!(
            controller
                .state
                .provider_config
                .effective_config
                .permissions
                .access_mode,
            current_access_mode
        );
        assert_eq!(next_live.access_mode(), current_access_mode);
    }

    #[tokio::test]
    async fn desktop_reopen_uses_durable_session_access_for_run_config_and_new_chat_uses_global() {
        use crate::protocol::ProtocolEventStore as _;
        use crate::session::{NewSession, SessionRepository as _};

        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        std::fs::create_dir_all(&root).expect("workspace");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
        let paths = crate::storage::StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        };
        let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
        sqlite.migrate().expect("migrate");
        let store = crate::storage::StoreBundle::new(sqlite);
        let mut app = AppBootstrap::rebuild_for_directory_as_workspace_root(&root, store)
            .await
            .expect("app");
        let global_access_mode = crate::config::AccessMode::Default;
        let session_access_mode = crate::config::AccessMode::FullAccess;
        app.config.permissions.access_mode = global_access_mode;
        let session = app
            .store
            .session_repo()
            .create_session(NewSession {
                project_id: app.workspace.project_id,
                title: "durable access".to_string(),
                cwd: root.clone(),
                model: app.config.model.model.clone(),
                base_url: app.config.model.base_url.clone(),
                access_mode: session_access_mode,
            })
            .await
            .expect("session");
        let turn_id = crate::protocol::TurnId::new();
        app.store
            .protocol_event_store()
            .append_history_item(&crate::protocol::HistoryItem {
                id: crate::protocol::HistoryItemId::new(),
                session_id: session.id,
                turn_id,
                sequence_no: 1,
                created_at_ms: 1,
                payload: crate::protocol::HistoryItemPayload::Message {
                    message_id: None,
                    role: crate::session::MessageRole::User,
                    content: vec![crate::protocol::ContentPart::Text {
                        text: "reopen".to_string(),
                    }],
                },
            })
            .expect("history item");
        let args = DesktopArgs {
            directory: Some(root),
            session_id: Some(session.id),
            continue_last: false,
            global_config_existed_at_launch: true,
        };
        let mut controller = DesktopController::new_with_preferences_and_persistence(
            app,
            args,
            DesktopPreferences::default(),
            false,
        )
        .await
        .expect("controller");

        assert_eq!(
            controller
                .state
                .provider_config
                .effective_config
                .permissions
                .access_mode,
            session_access_mode
        );
        let run_config = apply_config_patch(
            controller.app.config.clone(),
            desktop_run_config_override(&controller.state.provider_config.effective_config),
        );
        assert_eq!(run_config.permissions.access_mode, session_access_mode);

        controller.start_new_chat_with_global_access();
        assert_eq!(controller.state.app_state.current_session_id, None);
        assert_eq!(
            controller
                .state
                .provider_config
                .effective_config
                .permissions
                .access_mode,
            global_access_mode
        );
    }

    #[tokio::test]
    async fn archiving_the_only_current_session_restores_global_access_for_the_new_chat() {
        use crate::protocol::ProtocolEventStore as _;
        use crate::session::{NewSession, SessionRepository as _};

        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        std::fs::create_dir_all(&root).expect("workspace");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
        let paths = crate::storage::StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        };
        let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
        sqlite.migrate().expect("migrate");
        let store = crate::storage::StoreBundle::new(sqlite);
        let mut app = AppBootstrap::rebuild_for_directory_as_workspace_root(&root, store)
            .await
            .expect("app");
        let global_access_mode = crate::config::AccessMode::Default;
        let session_access_mode = crate::config::AccessMode::FullAccess;
        app.config.permissions.access_mode = global_access_mode;
        let session = app
            .store
            .session_repo()
            .create_session(NewSession {
                project_id: app.workspace.project_id,
                title: "only current session".to_string(),
                cwd: root.clone(),
                model: app.config.model.model.clone(),
                base_url: app.config.model.base_url.clone(),
                access_mode: session_access_mode,
            })
            .await
            .expect("session");
        app.store
            .protocol_event_store()
            .append_history_item(&crate::protocol::HistoryItem {
                id: crate::protocol::HistoryItemId::new(),
                session_id: session.id,
                turn_id: crate::protocol::TurnId::new(),
                sequence_no: 1,
                created_at_ms: 1,
                payload: crate::protocol::HistoryItemPayload::Message {
                    message_id: None,
                    role: crate::session::MessageRole::User,
                    content: vec![crate::protocol::ContentPart::Text {
                        text: "archive this session".to_string(),
                    }],
                },
            })
            .expect("history item");
        let args = DesktopArgs {
            directory: Some(root),
            session_id: Some(session.id),
            continue_last: false,
            global_config_existed_at_launch: true,
        };
        let mut controller = DesktopController::new_with_preferences_and_persistence(
            app,
            args,
            DesktopPreferences::default(),
            false,
        )
        .await
        .expect("controller");
        assert_eq!(
            controller
                .state
                .provider_config
                .effective_config
                .permissions
                .access_mode,
            session_access_mode
        );

        assert!(controller.archive_session(session.id, true));
        let live = LiveConfigOverrides::new(session_access_mode);
        controller
            .run_lifecycle
            .begin(41, CancellationToken::new(), live.clone());
        for _ in 0..300 {
            controller.drain_runtime_messages();
            if !controller.state.background_mutation_pending() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        assert!(!controller.state.background_mutation_pending());
        assert_eq!(controller.state.app_state.current_session_id, None);
        assert_eq!(controller.state.selected_session_id(), None);
        assert_eq!(
            controller
                .state
                .provider_config
                .effective_config
                .permissions
                .access_mode,
            global_access_mode
        );
        assert_eq!(live.access_mode(), global_access_mode);
        let run_config = apply_config_patch(
            controller.app.config.clone(),
            desktop_run_config_override(&controller.state.provider_config.effective_config),
        );
        assert_eq!(run_config.permissions.access_mode, global_access_mode);
    }

    #[tokio::test]
    async fn blocked_access_persistence_does_not_block_stop() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        std::fs::create_dir_all(&root).expect("workspace");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
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
        let args = DesktopArgs {
            directory: Some(root),
            session_id: None,
            continue_last: false,
            global_config_existed_at_launch: true,
        };
        let mut controller = DesktopController::new_with_preferences_and_persistence(
            app,
            args,
            DesktopPreferences::default(),
            false,
        )
        .await
        .expect("controller");
        let cancel = CancellationToken::new();
        let cancel_observer = cancel.clone();
        controller.run_lifecycle.begin(
            1,
            cancel,
            LiveConfigOverrides::new(crate::config::AccessMode::Default),
        );
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);

        assert!(controller.start_access_mode_persistence(
            move |_, _| {
                started_tx.send(()).expect("signal blocked persistence");
                release_rx.recv().expect("release blocked persistence");
                Err("simulated blocked global writer".to_string())
            },
            |_, _| Ok(()),
        ));
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("persistence worker started");

        controller.cancel_active_run();
        assert!(
            cancel_observer.is_cancelled(),
            "Stop must cancel the root before blocked persistence completes"
        );
        assert!(controller.state.background_mutation_pending());

        release_tx.send(()).expect("release persistence");
        for _ in 0..100 {
            controller.drain_runtime_messages();
            if !controller.state.background_mutation_pending() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!controller.state.background_mutation_pending());
    }

    #[tokio::test]
    async fn blocked_access_persistence_rejects_submit_review_and_steer_admission() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        std::fs::create_dir_all(&root).expect("workspace");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
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
        let args = DesktopArgs {
            directory: Some(root),
            session_id: None,
            continue_last: false,
            global_config_existed_at_launch: true,
        };
        let mut controller = DesktopController::new_with_preferences_and_persistence(
            app,
            args,
            DesktopPreferences::default(),
            false,
        )
        .await
        .expect("controller");
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);

        assert!(controller.start_access_mode_persistence(
            move |_, _| {
                started_tx.send(()).expect("signal blocked persistence");
                release_rx.recv().expect("release blocked persistence");
                Ok(Some(Utf8PathBuf::from("C:/config.toml")))
            },
            |_, _| Ok(()),
        ));
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("persistence worker started");
        assert!(controller.state.background_mutation_pending());

        let initial_generation = controller.next_root_run_generation;
        controller.state.composer.draft_prompt = "submit after access settles".to_string();
        assert!(!controller.start_run("submit after access settles".to_string()));
        assert!(!controller.start_review_uncommitted("review after access settles".to_string()));
        controller
            .state
            .begin_prompt_enhance(11, "enhance before review");
        assert!(
            controller
                .state
                .finish_prompt_enhance(11, "enhanced review draft".to_string())
        );
        assert!(!controller.send_prompt_review(true, "edited review draft".to_string()));
        assert_eq!(
            controller.state.composer.draft_prompt,
            "submit after access settles"
        );
        assert_eq!(
            controller.state.composer.review_draft_text,
            "edited review draft"
        );
        assert_eq!(controller.next_root_run_generation, initial_generation);
        assert!(!controller.run_lifecycle.root_is_active());

        controller.run_lifecycle.begin(
            77,
            CancellationToken::new(),
            LiveConfigOverrides::new(crate::config::AccessMode::Default),
        );
        assert!(!controller.start_run("steer after access settles".to_string()));
        assert_eq!(controller.run_lifecycle.root_generation(), Some(77));
        assert!(
            controller
                .state
                .app_state
                .status_message
                .as_deref()
                .is_some_and(|message| message.contains("owner mutation"))
        );
        controller.run_lifecycle.finish_root();

        release_tx.send(()).expect("release persistence");
        for _ in 0..100 {
            controller.drain_runtime_messages();
            if !controller.state.background_mutation_pending() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!controller.state.background_mutation_pending());
    }

    #[test]
    fn durable_activity_retry_is_bounded_and_config_generation_never_rewinds() {
        assert!(durable_agent_activity_retry_allowed(0));
        assert!(durable_agent_activity_retry_allowed(2));
        assert!(!durable_agent_activity_retry_allowed(3));
        assert!(!durable_agent_activity_retry_allowed(u8::MAX));

        assert_eq!(next_config_generation(1), 2);
        assert_eq!(next_config_generation(u64::MAX), u64::MAX);
    }

    #[test]
    fn failed_steer_preserves_request_draft_and_attachments() {
        let mut state = DesktopState::new(
            super::super::models::DesktopSnapshot {
                workspace_path: "C:/workspace".to_string(),
                provider_label: String::new(),
                model_label: String::new(),
                command_rows: Vec::new(),
                project_rows: Vec::new(),
                selected_project_index: 0,
                session_rows: Vec::new(),
                chat_session_rows: Vec::new(),
                session_details: Vec::new(),
                selected_session_index: 0,
            },
            ResolvedConfig::default(),
        );
        state.composer.draft_prompt = "keep this draft".to_string();
        let image = Utf8PathBuf::from("C:/workspace/reference.png");
        state.composer.image_attachment_paths.push(image.clone());

        assert!(!finish_steer_submission(
            &mut state,
            &crate::session::PromptDispatchPart::raw("follow-up"),
            std::slice::from_ref(&image),
            Err("terminal session".to_string()),
        ));
        assert_eq!(state.composer.draft_prompt, "keep this draft");
        assert_eq!(state.composer.image_attachment_paths, vec![image]);
        assert!(
            state
                .app_state
                .status_message
                .as_deref()
                .is_some_and(|message| message.contains("terminal session"))
        );
    }

    fn agent_record(
        session_id: SessionId,
        agent_path: &str,
        status: AgentStatus,
        result_preview: &str,
    ) -> AgentActivityRecord {
        AgentActivityRecord {
            agent_path: agent_path.to_string(),
            session_id,
            task_name: agent_path
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string(),
            task_preview: format!("task for {agent_path}"),
            status,
            current_activity: String::new(),
            result_preview: result_preview.to_string(),
            started_order: 1,
            updated: false,
        }
    }

    fn test_permission(summary: &str) -> PermissionRequest {
        PermissionRequest {
            access: crate::workspace::AccessKind::Shell,
            summary: summary.to_string(),
            details: Vec::new(),
            targets: vec![Utf8PathBuf::from("C:/workspace")],
            outside_workspace: false,
            risks: Vec::new(),
            agent_path: Some(format!("/root/{summary}")),
            agent_task_name: Some(summary.to_string()),
        }
    }

    fn recv_runtime_message(
        receiver: &mut tokio::sync::mpsc::UnboundedReceiver<RuntimeMessage>,
    ) -> RuntimeMessage {
        for _ in 0..200 {
            match receiver.try_recv() {
                Ok(message) => return message,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    panic!("desktop runtime channel disconnected")
                }
            }
        }
        panic!("timed out waiting for desktop runtime message")
    }

    #[test]
    fn child_only_agent_activity_blocks_desktop_navigation() {
        assert_eq!(
            navigation_admission_blocker(false, false, false, false, false),
            None
        );
        assert_eq!(
            navigation_admission_blocker(false, false, false, true, false),
            Some("the current agent tree is active")
        );
        assert_eq!(
            navigation_admission_blocker(false, false, false, false, true),
            Some("the current run is finalizing")
        );
    }

    #[test]
    fn session_search_started_while_idle_cannot_replace_root_after_run_or_tree_admission() {
        fn snapshot(session_id: SessionId, title: &str) -> DesktopSnapshot {
            DesktopSnapshot {
                workspace_path: "C:/workspace".to_string(),
                provider_label: String::new(),
                model_label: String::new(),
                command_rows: Vec::new(),
                project_rows: Vec::new(),
                selected_project_index: 0,
                session_rows: vec![super::super::models::DesktopSessionRow::from_parts(
                    session_id,
                    title,
                    SessionStatus::Idle,
                )],
                chat_session_rows: Vec::new(),
                session_details: Vec::new(),
                selected_session_index: 0,
            }
        }

        let selected_root = SessionId::new();
        let stale_search_root = SessionId::new();
        let mut state = DesktopState::new(
            snapshot(selected_root, "selected root"),
            ResolvedConfig::default(),
        );

        assert!(!apply_session_search_result(
            &mut state,
            true,
            true,
            false,
            Ok(snapshot(stale_search_root, "stale search result")),
        ));
        assert_eq!(state.selected_session_id(), Some(selected_root));

        assert!(!apply_session_search_result(
            &mut state,
            true,
            false,
            true,
            Ok(snapshot(stale_search_root, "stale tree result")),
        ));
        assert_eq!(state.selected_session_id(), Some(selected_root));
        assert!(!session_search_result_can_apply(false, false, false));
    }

    fn navigation_owner_state() -> (DesktopState, SessionId, SessionId) {
        let session_a = SessionId::new();
        let session_b = SessionId::new();
        let mut state = DesktopState::new(
            DesktopSnapshot {
                workspace_path: "C:/workspace-a".to_string(),
                provider_label: String::new(),
                model_label: String::new(),
                command_rows: Vec::new(),
                project_rows: vec![
                    super::super::models::DesktopProjectRow {
                        project_id: ProjectId::new(),
                        label: "project A".to_string(),
                        path: "C:/workspace-a".to_string(),
                    },
                    super::super::models::DesktopProjectRow {
                        project_id: ProjectId::new(),
                        label: "project B".to_string(),
                        path: "C:/workspace-b".to_string(),
                    },
                ],
                selected_project_index: 0,
                session_rows: vec![
                    super::super::models::DesktopSessionRow::from_parts(
                        session_a,
                        "session A",
                        SessionStatus::Idle,
                    ),
                    super::super::models::DesktopSessionRow::from_parts(
                        session_b,
                        "session B",
                        SessionStatus::Idle,
                    ),
                ],
                chat_session_rows: Vec::new(),
                session_details: Vec::new(),
                selected_session_index: 0,
            },
            ResolvedConfig::default(),
        );
        state.app_state.current_session_id = Some(session_a);
        state.rebind_composer_owner(Some(session_a));
        state.composer.draft_prompt = "draft owned by A".to_string();
        (state, session_a, session_b)
    }

    #[test]
    fn failed_session_navigation_restores_selected_and_draft_owner_to_a() {
        let (mut state, session_a, session_b) = navigation_owner_state();
        let attachment = Utf8PathBuf::from("C:/workspace-a/attachment.png");
        state
            .composer
            .image_attachment_paths
            .push(attachment.clone());
        state.select_session(1);
        assert_eq!(state.selected_session_id(), Some(session_b));
        let request_id = state.begin_session_load(session_b);

        assert!(finish_navigation_failure(
            &mut state,
            request_id,
            "session B failed to load",
        ));

        let projection = desktop_web_state(&state, &DesktopRuntimeProjection::default());
        assert_eq!(state.selected_session_id(), Some(session_a));
        assert!(projection.selected_session_title.starts_with("session A"));
        assert_eq!(
            projection.draft_target.session_id,
            Some(session_a.to_string())
        );
        assert_eq!(projection.draft_prompt, "draft owned by A");
        assert_eq!(state.composer.image_attachment_paths, vec![attachment]);
        assert!(!projection.navigation_loading);
    }

    #[test]
    fn failed_project_navigation_never_replaces_committed_workspace_owner() {
        let (mut state, session_a, _) = navigation_owner_state();
        let request_id = state.begin_workspace_load(Utf8PathBuf::from("C:/workspace-b"), None);

        assert_eq!(state.snapshot.workspace_path, "C:/workspace-a");
        assert_eq!(state.selected_project_path(), Some("C:/workspace-a"));
        assert!(finish_navigation_failure(
            &mut state,
            request_id,
            "project B failed to load",
        ));

        let projection = desktop_web_state(&state, &DesktopRuntimeProjection::default());
        assert_eq!(projection.workspace_path, "C:/workspace-a");
        assert_eq!(state.selected_project_path(), Some("C:/workspace-a"));
        assert_eq!(state.selected_session_id(), Some(session_a));
        assert_eq!(
            projection.draft_target.session_id,
            Some(session_a.to_string())
        );
        assert_eq!(projection.draft_prompt, "draft owned by A");
        assert!(!projection.navigation_loading);
    }

    #[test]
    fn non_selected_row_mutations_preserve_owner_a_on_success_and_failure() {
        let (mut state, session_a, session_b) = navigation_owner_state();
        let project_a = state.snapshot.project_rows[0].project_id;
        let project_b = state.snapshot.project_rows[1].project_id;

        let mut session_success = state.snapshot.clone();
        session_success
            .session_rows
            .iter_mut()
            .find(|row| row.session_id == session_b)
            .expect("session B")
            .archived = true;
        session_success
            .session_rows
            .retain(|row| row.session_id != session_a);
        let archive_id = state.begin_session_archive_mutation();
        assert!(state.finish_session_archive_mutation(archive_id));
        state.replace_snapshot_preserving_current_owner(session_success);
        assert_eq!(state.selected_session_id(), Some(session_a));
        assert_eq!(state.app_state.current_session_id, Some(session_a));

        let maintenance_id = state.begin_session_maintenance_mutation();
        assert!(state.finish_session_maintenance_mutation(maintenance_id));
        state.set_status_message("session B mutation failed");
        assert_eq!(state.selected_session_id(), Some(session_a));
        assert_eq!(state.app_state.current_session_id, Some(session_a));

        let mut project_success = state.snapshot.clone();
        project_success
            .project_rows
            .retain(|row| row.project_id != project_b);
        let delete_id = state.begin_project_delete_mutation();
        assert!(state.finish_project_delete_mutation(delete_id));
        state.replace_snapshot(project_success);
        assert_eq!(state.selected_project_id(), Some(project_a));
        assert_eq!(state.selected_session_id(), Some(session_a));
        assert_eq!(state.app_state.current_session_id, Some(session_a));

        let failed_delete_id = state.begin_project_delete_mutation();
        assert!(state.finish_project_delete_mutation(failed_delete_id));
        state.set_status_message("project B deletion failed");
        let projection = desktop_web_state(&state, &DesktopRuntimeProjection::default());
        assert_eq!(state.selected_project_id(), Some(project_a));
        assert_eq!(state.selected_session_id(), Some(session_a));
        assert_eq!(
            projection.draft_target.session_id,
            Some(session_a.to_string())
        );
        assert_eq!(projection.workspace_path, "C:/workspace-a");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn controller_async_owner_and_durable_submission_contracts_are_lossless() {
        use crate::session::{ProjectRepository as _, SessionRepository as _};

        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        std::fs::create_dir_all(&root).expect("workspace");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
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
        let project_b_root =
            Utf8PathBuf::from_path_buf(temp.path().join("workspace-b")).expect("utf8 project B");
        std::fs::create_dir_all(&project_b_root).expect("project B workspace");
        let project_b = ProjectId::new();
        app.store
            .project_repo()
            .upsert_project(project_b, &project_b_root, "project B", "none")
            .await
            .expect("project B");
        let args = DesktopArgs {
            directory: Some(root.clone()),
            session_id: None,
            continue_last: false,
            global_config_existed_at_launch: true,
        };
        let mut controller = DesktopController::new_with_preferences_and_persistence(
            app,
            args,
            DesktopPreferences::default(),
            false,
        )
        .await
        .expect("controller");
        let repo = controller.app.store.session_repo();
        let create = |title: &str| crate::session::NewSession {
            project_id: controller.app.workspace.project_id,
            title: title.to_string(),
            cwd: root.clone(),
            model: controller.app.config.model.model.clone(),
            base_url: controller.app.config.model.base_url.clone(),
            access_mode: controller.app.config.permissions.access_mode,
        };
        let session_a = repo.create_session(create("session A")).await.expect("A");
        let session_b = repo.create_session(create("session B")).await.expect("B");
        let snapshot = load_snapshot_for_selection(&controller.app, Some(session_a.id))
            .await
            .expect("snapshot");
        controller.state.replace_snapshot(snapshot);
        controller.state.app_state.current_session_id = Some(session_a.id);
        controller.state.app_state.current_session_title = session_a.title.clone();
        let stale_snapshot = controller.state.snapshot.clone();

        let search_operation = controller.state.begin_session_search();
        let search_request = controller.session_search_requests.begin(search_operation);
        let snapshot_target = SnapshotRequestTarget {
            workspace_root: root.clone(),
            selected_session_id: Some(session_a.id),
        };
        let snapshot_request = controller.snapshot_requests.begin(snapshot_target.clone());
        controller.state.begin_snapshot_refresh();

        assert!(controller.archive_session(session_b.id, true));
        for _ in 0..300 {
            controller.drain_runtime_messages();
            if !controller.state.background_mutation_pending() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!controller.state.background_mutation_pending());
        assert_eq!(
            controller.state.app_state.current_session_id,
            Some(session_a.id)
        );
        assert_eq!(controller.state.selected_session_id(), Some(session_a.id));
        assert!(
            !controller
                .state
                .snapshot
                .session_rows
                .iter()
                .any(|row| row.session_id == session_b.id)
        );

        controller
            .runtime_tx
            .send(RuntimeMessage::SessionSearchLoaded {
                request_id: search_request,
                result: Ok(stale_snapshot.clone()),
            })
            .expect("stale search");
        controller
            .runtime_tx
            .send(RuntimeMessage::SnapshotLoaded {
                request_id: snapshot_request,
                target: snapshot_target,
                result: Ok(stale_snapshot),
            })
            .expect("stale snapshot");
        controller.drain_runtime_messages();

        assert_eq!(
            controller.state.app_state.current_session_id,
            Some(session_a.id)
        );
        assert_eq!(controller.state.selected_session_id(), Some(session_a.id));
        assert!(
            !controller
                .state
                .snapshot
                .session_rows
                .iter()
                .any(|row| row.session_id == session_b.id)
        );
        assert!(
            !controller
                .state
                .pending_async_operation_keys()
                .iter()
                .any(|key| key == "session_search" || key == "snapshot_refresh")
        );

        assert!(
            controller
                .state
                .snapshot
                .project_rows
                .iter()
                .any(|row| row.project_id == project_b)
        );
        assert!(controller.delete_project(project_b));
        controller.app.config.model.model = "live-config-after-delete-dispatch".to_string();
        for _ in 0..300 {
            controller.drain_runtime_messages();
            if !controller.state.background_mutation_pending() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!controller.state.background_mutation_pending());
        assert!(!controller.state.navigation_loading());
        assert_eq!(controller.app.workspace.root, root);
        assert_eq!(
            controller.app.config.model.model,
            "live-config-after-delete-dispatch"
        );
        assert_eq!(
            controller.state.app_state.current_session_id,
            Some(session_a.id)
        );
        assert_eq!(controller.state.selected_session_id(), Some(session_a.id));
        assert!(
            !controller
                .state
                .snapshot
                .project_rows
                .iter()
                .any(|row| row.project_id == project_b)
        );
        assert!(
            controller
                .state
                .app_state
                .status_message
                .as_deref()
                .is_some_and(
                    |message| message.contains("deleted project") && !message.contains("opening")
                )
        );

        let retained_image = root.join("retained.png");
        controller
            .state
            .composer
            .image_attachment_paths
            .push(retained_image.clone());
        controller.state.begin_prompt_enhance(77, "raw review");
        assert!(
            controller
                .state
                .finish_prompt_enhance(77, "edited review".to_string())
        );
        let failed_generation = 900;
        controller.run_lifecycle.begin(
            failed_generation,
            CancellationToken::new(),
            LiveConfigOverrides::new(crate::config::AccessMode::Default),
        );
        controller.state.begin_agent_run();
        controller.pending_root_submission = Some(PendingRootSubmission {
            run_generation: failed_generation,
            owner_workspace_path: root.clone(),
            owner_session_id: Some(session_a.id),
            prompt_dispatch: crate::session::PromptDispatchPart::raw("retain on preflight error"),
            image_paths: vec![retained_image.clone()],
            cancel_prompt_review_on_commit: true,
        });
        assert!(
            controller
                .state
                .selected_detail()
                .transcript_rows
                .iter()
                .all(|row| row.row_kind != super::super::models::DesktopTranscriptRowKind::User)
        );
        controller
            .runtime_tx
            .send(RuntimeMessage::RunEvent {
                run_generation: failed_generation,
                event: RunEvent::SessionStarted {
                    session_id: session_a.id,
                    title: session_a.title.clone(),
                },
            })
            .expect("session start before durable user turn");
        controller.drain_runtime_messages();
        assert_eq!(controller.composer_commit_generation, 0);
        assert!(controller.pending_root_submission.is_some());
        assert_eq!(controller.state.composer.review_draft_text, "edited review");
        controller
            .runtime_tx
            .send(RuntimeMessage::Finished {
                run_generation: failed_generation,
                result: Err("preflight rejected".to_string()),
            })
            .expect("preflight failure");
        controller.drain_runtime_messages();
        assert_eq!(controller.composer_commit_generation, 0);
        assert_eq!(
            controller.state.composer.image_attachment_paths,
            vec![retained_image.clone()]
        );
        assert_eq!(
            desktop_web_state(&controller.state, &DesktopRuntimeProjection::default())
                .attached_images,
            vec![retained_image.to_string()]
        );
        assert!(
            controller
                .state
                .selected_detail()
                .transcript_rows
                .iter()
                .all(|row| row.row_kind != super::super::models::DesktopTranscriptRowKind::User)
        );
        assert!(controller.pending_root_submission.is_none());
        assert!(controller.state.app_state.prompt_review.is_some());
        assert_eq!(controller.state.composer.review_draft_text, "edited review");
        assert_eq!(
            controller.state.view.overlay,
            super::super::state::DesktopOverlay::PromptReview
        );

        let admitted_generation = failed_generation + 1;
        controller.run_lifecycle.begin(
            admitted_generation,
            CancellationToken::new(),
            LiveConfigOverrides::new(crate::config::AccessMode::Default),
        );
        controller.state.begin_agent_run();
        controller.pending_root_submission = Some(PendingRootSubmission {
            run_generation: admitted_generation,
            owner_workspace_path: root.clone(),
            owner_session_id: Some(session_a.id),
            prompt_dispatch: crate::session::PromptDispatchPart::raw("commit after admission"),
            image_paths: vec![retained_image.clone()],
            cancel_prompt_review_on_commit: true,
        });
        controller
            .runtime_tx
            .send(RuntimeMessage::RunEvent {
                run_generation: admitted_generation,
                event: RunEvent::SessionStarted {
                    session_id: session_a.id,
                    title: session_a.title.clone(),
                },
            })
            .expect("admission event");
        controller.drain_runtime_messages();
        assert_eq!(controller.composer_commit_generation, 0);
        assert!(controller.pending_root_submission.is_some());
        assert_eq!(
            controller.state.composer.image_attachment_paths,
            vec![retained_image.clone()]
        );
        assert!(controller.state.app_state.prompt_review.is_some());
        controller
            .runtime_tx
            .send(RuntimeMessage::RunEvent {
                run_generation: admitted_generation,
                event: RunEvent::UserMessageStored {
                    message_id: crate::session::MessageId::new(),
                },
            })
            .expect("durable user message");
        assert!(controller.run_lifecycle.request_cancel());
        controller.drain_runtime_messages();
        assert_eq!(controller.composer_commit_generation, 1);
        assert!(controller.state.composer.image_attachment_paths.is_empty());
        assert!(controller.pending_root_submission.is_none());
        assert!(controller.state.app_state.prompt_review.is_none());
        assert!(controller.state.composer.review_draft_text.is_empty());
        assert_eq!(
            controller.state.view.overlay,
            super::super::state::DesktopOverlay::None
        );
        controller
            .runtime_tx
            .send(RuntimeMessage::Finished {
                run_generation: admitted_generation,
                result: Err("test cleanup".to_string()),
            })
            .expect("cleanup");
        controller.drain_runtime_messages();

        let created_session_id = session_a.id;
        let submitted_image = root.join("submitted-first-run.png");
        let next_image = root.join("next-request.png");
        controller.state.app_state.current_session_id = None;
        controller.state.rebind_composer_owner(None);
        controller.state.composer.image_attachment_paths =
            vec![submitted_image.clone(), next_image.clone()];
        controller.pending_root_submission = Some(PendingRootSubmission {
            run_generation: 902,
            owner_workspace_path: root.clone(),
            owner_session_id: None,
            prompt_dispatch: crate::session::PromptDispatchPart::raw("first run"),
            image_paths: vec![submitted_image],
            cancel_prompt_review_on_commit: false,
        });
        controller.state.app_state.current_session_id = Some(created_session_id);
        assert!(controller.commit_pending_root_submission(902));
        assert_eq!(controller.composer_commit_generation, 2);
        assert_eq!(
            controller.state.composer.image_attachment_paths,
            vec![next_image.clone()]
        );
        controller
            .state
            .bind_composer_to_loaded_session(created_session_id);
        assert_eq!(
            controller.state.composer.image_attachment_paths,
            vec![next_image.clone()]
        );
        assert_eq!(
            controller.state.composer.image_attachment_paths,
            vec![next_image]
        );

        let blocker = controller.state.begin_project_delete_mutation();
        assert!(!controller.set_session_memory_mode(session_a.id, SessionMemoryMode::Enabled,));
        assert!(controller.state.finish_project_delete_mutation(blocker));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_access_mode_persistence_keeps_every_runtime_owner_and_permission_unchanged() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        std::fs::create_dir_all(&root).expect("workspace");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
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
        let args = DesktopArgs {
            directory: Some(root),
            session_id: None,
            continue_last: false,
            global_config_existed_at_launch: true,
        };
        let mut controller = DesktopController::new_with_preferences_and_persistence(
            app,
            args,
            DesktopPreferences::default(),
            false,
        )
        .await
        .expect("controller");
        let initial_access_mode = controller.app.config.permissions.access_mode;
        let live = LiveConfigOverrides::new(initial_access_mode);
        let live_observer = live.clone();
        controller
            .run_lifecycle
            .begin(1, CancellationToken::new(), live);
        let request = test_permission("pending permission");
        let (response, receiver) = mpsc::channel();
        controller.pending_permission_request = Some(request.clone());
        controller.pending_permission_request_id = Some(42);
        controller.permission_response = Some(response);
        controller.state.set_permission(42, &request);

        assert!(!controller.toggle_access_mode_with_persistence(
            |_, _| Err("simulated persistence failure".to_string()),
            |_, _| Ok(()),
        ));

        assert_eq!(
            controller.app.config.permissions.access_mode,
            initial_access_mode
        );
        assert_eq!(
            controller
                .state
                .provider_config
                .effective_config
                .permissions
                .access_mode,
            initial_access_mode
        );
        assert_eq!(live_observer.access_mode(), initial_access_mode);
        assert_eq!(controller.pending_permission_request_id, Some(42));
        assert!(controller.pending_permission_request.is_some());
        assert_eq!(controller.state.permission_request_id, Some(42));
        assert!(controller.state.app_state.permission.is_some());
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn durable_activity_fallback_is_session_scoped_and_live_rows_win() {
        let root_session_id = SessionId::new();
        let other_session_id = SessionId::new();
        let durable = (
            root_session_id,
            vec![
                agent_record(
                    SessionId::new(),
                    "/root/research",
                    AgentStatus::Completed(Some("research result".to_string())),
                    "research result",
                ),
                agent_record(
                    SessionId::new(),
                    "/root/review",
                    AgentStatus::Completed(Some("review result".to_string())),
                    "review result",
                ),
                agent_record(
                    SessionId::new(),
                    "/root/tests",
                    AgentStatus::Completed(Some("test result".to_string())),
                    "test result",
                ),
            ],
        );

        let restored = activity_records_for_projection(root_session_id, Vec::new(), Some(&durable));
        assert_eq!(restored.len(), 3);
        assert_eq!(
            restored
                .iter()
                .map(|record| record.agent_path.as_str())
                .collect::<Vec<_>>(),
            vec!["/root/research", "/root/review", "/root/tests"]
        );
        assert!(restored.iter().all(|record| {
            matches!(record.status, AgentStatus::Completed(Some(_)))
                && !record.result_preview.is_empty()
        }));
        assert!(
            activity_records_for_projection(other_session_id, Vec::new(), Some(&durable))
                .is_empty()
        );

        let live = agent_record(SessionId::new(), "/root/live", AgentStatus::Running, "");
        let selected =
            activity_records_for_projection(root_session_id, vec![live.clone()], Some(&durable));
        assert_eq!(selected, vec![live]);
        assert!(agent_activity_projection(selected).1);
    }

    #[test]
    fn durable_only_running_activity_is_active_and_requests_bounded_refresh() {
        let root_session_id = SessionId::new();
        let running = agent_record(SessionId::new(), "/root/research", AgentStatus::Running, "");
        let durable = (root_session_id, vec![running]);
        let live = Vec::new();
        let projected =
            activity_records_for_projection(root_session_id, live.clone(), Some(&durable));

        assert!(agent_activity_records_are_active(&projected));
        assert!(durable_agent_activity_refresh_required(
            &live, &projected, false, false,
        ));
        assert!(!durable_agent_activity_refresh_required(
            &live, &projected, true, false,
        ));
        assert!(!durable_agent_activity_refresh_required(
            &live, &projected, false, true,
        ));

        let local_live = vec![agent_record(
            SessionId::new(),
            "/root/live",
            AgentStatus::Running,
            "",
        )];
        assert!(!durable_agent_activity_refresh_required(
            &local_live,
            &local_live,
            false,
            false,
        ));
    }

    #[test]
    fn durable_activity_refresh_completion_is_latest_wins_and_session_scoped() {
        let session_id = SessionId::new();
        let target = SessionRefreshRequestTarget {
            workspace_root: Utf8PathBuf::from("C:/workspace-a"),
            session_id,
        };
        let mut tracker = LatestRequestTracker::default();
        let stale_request = tracker.begin(target.clone());
        let current_request = tracker.begin(target.clone());

        assert!(!finish_durable_agent_activity_refresh_request(
            &mut tracker,
            stale_request,
            &target,
            Utf8Path::new("C:/workspace-a"),
            Some(session_id),
        ));
        assert!(tracker.is_pending());
        assert!(!finish_durable_agent_activity_refresh_request(
            &mut tracker,
            current_request,
            &target,
            Utf8Path::new("C:/workspace-b"),
            Some(session_id),
        ));
        assert!(!tracker.is_pending());

        let wrong_session_request = tracker.begin(target.clone());
        assert!(!finish_durable_agent_activity_refresh_request(
            &mut tracker,
            wrong_session_request,
            &target,
            Utf8Path::new("C:/workspace-a"),
            Some(SessionId::new()),
        ));
        assert!(!tracker.is_pending());

        let accepted_request = tracker.begin(target.clone());
        assert!(finish_durable_agent_activity_refresh_request(
            &mut tracker,
            accepted_request,
            &target,
            Utf8Path::new("C:/workspace-a"),
            Some(session_id),
        ));
        assert!(!finish_durable_agent_activity_refresh_request(
            &mut tracker,
            accepted_request,
            &target,
            Utf8Path::new("C:/workspace-a"),
            Some(session_id),
        ));
    }

    #[test]
    fn cancelled_active_permission_clears_by_id_and_advances_broker() {
        let (runtime_tx, mut runtime_rx) = tokio::sync::mpsc::unbounded_channel();
        let broker = SharedConfirmationPrompt::new(DesktopConfirmationPrompt {
            tx: runtime_tx,
            next_permission_request_id: Arc::new(AtomicU64::new(41)),
        });

        let first_cancel = CancellationToken::new();
        let (first_done_tx, first_done_rx) = mpsc::sync_channel(1);
        let mut first_prompt = broker.clone();
        let first_wait_cancel = first_cancel.clone();
        std::thread::spawn(move || {
            let result =
                first_prompt.confirm_with_cancel(&test_permission("first"), &first_wait_cancel);
            let _ = first_done_tx.send(result);
        });

        let (first_id, first_response) = match recv_runtime_message(&mut runtime_rx) {
            RuntimeMessage::Permission {
                confirmation_id,
                response,
                ..
            } => (confirmation_id, response),
            _ => panic!("expected first desktop permission"),
        };
        first_cancel.cancel();
        match recv_runtime_message(&mut runtime_rx) {
            RuntimeMessage::PermissionCancelled { confirmation_id } => {
                assert_eq!(confirmation_id, first_id)
            }
            _ => panic!("expected desktop permission cancellation"),
        }
        assert!(
            !first_done_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("first confirmation result")
                .expect("first confirmation")
        );

        let mut pending_request = Some(test_permission("first"));
        let mut pending_id = Some(first_id);
        let mut pending_response = Some(first_response);
        assert!(!clear_cancelled_permission(
            &mut pending_request,
            &mut pending_id,
            &mut pending_response,
            first_id + 1,
        ));
        assert_eq!(pending_id, Some(first_id));
        assert!(clear_cancelled_permission(
            &mut pending_request,
            &mut pending_id,
            &mut pending_response,
            first_id,
        ));
        assert!(pending_id.is_none());

        let (second_done_tx, second_done_rx) = mpsc::sync_channel(1);
        let mut second_prompt = broker;
        std::thread::spawn(move || {
            let result = second_prompt.confirm(&test_permission("second"));
            let _ = second_done_tx.send(result);
        });
        let (second_id, second_response) = match recv_runtime_message(&mut runtime_rx) {
            RuntimeMessage::Permission {
                confirmation_id,
                response,
                ..
            } => (confirmation_id, response),
            _ => panic!("expected second desktop permission"),
        };
        assert!(second_id > first_id);
        second_response
            .send(true)
            .expect("answer second permission");
        assert!(
            second_done_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("second confirmation result")
                .expect("second confirmation")
        );
    }

    #[test]
    fn projection_revision_is_monotonic_at_the_command_owner() {
        let mut revision = 0;
        assert_eq!(advance_projection_revision(&mut revision), Ok(1));
        assert_eq!(advance_projection_revision(&mut revision), Ok(2));

        revision = (1_u64 << 53) - 1;
        assert_eq!(advance_projection_revision(&mut revision), Ok(1_u64 << 53));
        assert_eq!(
            projection_revision_text((1_u64 << 53) - 1),
            "9007199254740991"
        );
        assert_eq!(projection_revision_text(1_u64 << 53), "9007199254740992");
        assert_eq!(projection_revision_text(u64::MAX), "18446744073709551615");

        revision = u64::MAX;
        assert!(advance_projection_revision(&mut revision).is_err());
        assert_eq!(revision, u64::MAX);
    }

    #[test]
    fn attachment_authorization_diff_revokes_only_paths_no_longer_projected() {
        let first = Utf8PathBuf::from("C:/outside/first.png");
        let retained = Utf8PathBuf::from("C:/outside/retained.png");
        let mut authorized = BTreeSet::from([first.clone(), retained.clone()]);
        let desired = BTreeSet::from([retained]);

        assert_eq!(
            attachment_authorizations_to_revoke(&authorized, &desired),
            vec![first.clone()]
        );
        authorized.remove(&first);
        assert_eq!(
            attachment_authorizations_to_revoke(&authorized, &desired),
            Vec::<Utf8PathBuf>::new(),
            "a successful revoke must not be issued again"
        );

        let workspace_replacement =
            attachment_authorizations_to_revoke(&authorized, &BTreeSet::new());
        assert_eq!(
            workspace_replacement,
            authorized.into_iter().collect::<Vec<_>>()
        );
    }

    #[test]
    fn session_delete_completion_is_bound_to_request_and_workspace_identity() {
        let project_id = ProjectId::new();
        let mut state = DesktopState::new(
            super::super::models::DesktopSnapshot {
                workspace_path: "C:/workspace-a".to_string(),
                provider_label: String::new(),
                model_label: String::new(),
                command_rows: Vec::new(),
                project_rows: Vec::new(),
                selected_project_index: 0,
                session_rows: Vec::new(),
                chat_session_rows: Vec::new(),
                session_details: Vec::new(),
                selected_session_index: 0,
            },
            ResolvedConfig::default(),
        );
        let target = SessionDeleteRequestTarget {
            workspace_root: Utf8PathBuf::from("C:/workspace-a"),
            project_id,
            session_id: SessionId::new(),
            operation_id: state.begin_session_delete_mutation(),
        };

        assert!(session_delete_target_matches(
            &target,
            Utf8Path::new("C:/workspace-a"),
            project_id,
        ));
        assert!(!session_delete_target_matches(
            &target,
            Utf8Path::new("C:/workspace-b"),
            project_id,
        ));
        assert!(!session_delete_target_matches(
            &target,
            Utf8Path::new("C:/workspace-a"),
            ProjectId::new(),
        ));
        assert!(!finish_session_delete_request(
            &mut state,
            &target,
            Utf8Path::new("C:/workspace-b"),
            project_id,
        ));
        assert!(state.background_mutation_pending());
        assert!(finish_session_delete_request(
            &mut state,
            &target,
            Utf8Path::new("C:/workspace-a"),
            project_id,
        ));
        assert!(!state.background_mutation_pending());
        assert!(!finish_session_delete_request(
            &mut state,
            &target,
            Utf8Path::new("C:/workspace-a"),
            project_id,
        ));
    }

    #[test]
    fn history_export_completion_rejects_stale_request_workspace_and_repeat() {
        let session_id = SessionId::new();
        let target = HistoryExportRequestTarget {
            workspace_root: Utf8PathBuf::from("C:/workspace-a"),
            session_id,
        };
        let mut tracker = LatestRequestTracker::default();
        let stale_request = tracker.begin(target.clone());
        let current_request = tracker.begin(target.clone());

        assert_eq!(
            finish_history_export_request(
                &mut tracker,
                stale_request,
                &target,
                Utf8Path::new("C:/workspace-a"),
            ),
            None,
            "an older completion cannot settle the latest export owner"
        );
        assert_eq!(
            finish_history_export_request(
                &mut tracker,
                current_request,
                &target,
                Utf8Path::new("C:/workspace-b"),
            ),
            Some(false),
            "a current request from the replaced workspace cannot update status"
        );
        assert_eq!(
            finish_history_export_request(
                &mut tracker,
                current_request,
                &target,
                Utf8Path::new("C:/workspace-a"),
            ),
            None,
            "the same completion is consumed at most once"
        );
    }
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
    pending_permission_request_id: Option<u64>,
    next_permission_request_id: Arc<AtomicU64>,
    run_lifecycle: DesktopRunLifecycle,
    pending_root_submission: Option<PendingRootSubmission>,
    composer_commit_generation: u64,
    next_root_run_generation: u64,
    next_enhance_request_id: u64,
    session_search_requests: SessionSearchRequestTracker,
    snapshot_requests: LatestRequestTracker<SnapshotRequestTarget>,
    turn_page_requests: LatestRequestTracker<SessionPageRequestTarget>,
    live_session_refresh_requests: LatestRequestTracker<SessionRefreshRequestTarget>,
    current_session_refresh_requests: LatestRequestTracker<SessionRefreshRequestTarget>,
    current_todo_refresh_requests: LatestRequestTracker<SessionRefreshRequestTarget>,
    durable_agent_activity_refresh_requests: LatestRequestTracker<SessionRefreshRequestTarget>,
    history_export_requests: LatestRequestTracker<HistoryExportRequestTarget>,
    provider_catalog_requests: LatestRequestTracker<ProviderCatalogRequestTarget>,
    startup_provider_requests: LatestRequestTracker<ProviderReadinessRequestTarget>,
    startup_docling_requests: LatestRequestTracker<DoclingRequestTarget>,
    access_mode_persistence_requests: LatestRequestTracker<AccessModePersistenceTarget>,
    pending_access_mode_adoption: Option<PendingAccessModeAdoption>,
    projection_revision: u64,
    loaded_agent_activity_records: Option<LoadedAgentActivityRecords>,
    durable_agent_activity_refresh_failures: u8,
    attachment_asset_app: Option<tauri::AppHandle>,
    authorized_attachment_assets: BTreeSet<Utf8PathBuf>,
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
        let mut loaded_agent_activity_records = None;
        if let Some(session_id) = args.session_id.or_else(|| state.selected_session_id()) {
            let detail = load_session_detail(&app, session_id).await?;
            let activity_records = app
                .run_service
                .durable_agent_activity_records(session_id)
                .await?;
            state.load_open_session(
                &detail.session,
                &detail.transcript,
                &detail.turn_items,
                detail.state,
                detail.todos,
                detail.turn_page_offset,
                detail.turn_page_limit,
                detail.turn_page_total,
                detail.turn_page_has_more,
            );
            loaded_agent_activity_records = Some((session_id, activity_records));
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
            pending_permission_request_id: None,
            next_permission_request_id: Arc::new(AtomicU64::new(1)),
            run_lifecycle: DesktopRunLifecycle::default(),
            pending_root_submission: None,
            composer_commit_generation: 0,
            next_root_run_generation: 1,
            next_enhance_request_id: 1,
            session_search_requests: SessionSearchRequestTracker::default(),
            snapshot_requests: LatestRequestTracker::default(),
            turn_page_requests: LatestRequestTracker::default(),
            live_session_refresh_requests: LatestRequestTracker::default(),
            current_session_refresh_requests: LatestRequestTracker::default(),
            current_todo_refresh_requests: LatestRequestTracker::default(),
            durable_agent_activity_refresh_requests: LatestRequestTracker::default(),
            history_export_requests: LatestRequestTracker::default(),
            provider_catalog_requests: LatestRequestTracker::default(),
            startup_provider_requests: LatestRequestTracker::default(),
            startup_docling_requests: LatestRequestTracker::default(),
            access_mode_persistence_requests: LatestRequestTracker::default(),
            pending_access_mode_adoption: None,
            projection_revision: 0,
            loaded_agent_activity_records,
            durable_agent_activity_refresh_failures: 0,
            attachment_asset_app: None,
            authorized_attachment_assets: BTreeSet::new(),
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
        }
        controller.drive_startup_readiness();
        Ok(controller)
    }

    pub(crate) fn next_web_state(&mut self) -> Result<DesktopWebState, String> {
        self.reconcile_attachment_asset_authorizations()?;
        let revision = advance_projection_revision(&mut self.projection_revision)?;
        let mut runtime_projection = DesktopRuntimeProjection {
            root_run_finalizing: self.run_lifecycle.root_is_finalizing(),
            root_run_generation: self.run_lifecycle.root_generation(),
            last_root_run_epoch: self.last_root_run_epoch(),
            composer_commit_generation: self.composer_commit_generation,
            ..DesktopRuntimeProjection::default()
        };
        if let Some(root_session_id) = self.state.app_state.current_session_id {
            let live_records = self.app.run_service.agent_activity_records(root_session_id);
            let records = activity_records_for_projection(
                root_session_id,
                live_records.clone(),
                self.loaded_agent_activity_records.as_ref(),
            );
            let refresh_durable_activity = durable_agent_activity_refresh_required(
                &live_records,
                &records,
                self.durable_agent_activity_refresh_requests.is_pending(),
                self.state.post_run_refresh_pending(),
            ) && durable_agent_activity_retry_allowed(
                self.durable_agent_activity_refresh_failures,
            );
            let (rows, tree_active) = agent_activity_projection(records);
            runtime_projection.agent_activity_rows = rows;
            runtime_projection.agent_tree_active = tree_active;
            if tree_active {
                self.invalidate_session_search_requests();
            }
            if refresh_durable_activity {
                self.spawn_durable_agent_activity_refresh(root_session_id);
            }
        }
        let mut projection = desktop_web_state(&self.state, &runtime_projection);
        projection.projection_revision = projection_revision_text(revision);
        Ok(projection)
    }

    pub(crate) fn authorize_attachment_asset(
        &mut self,
        app: &tauri::AppHandle,
        path: &Utf8Path,
    ) -> Result<(), String> {
        self.attachment_asset_app = Some(app.clone());
        if self.authorized_attachment_assets.contains(path) {
            return Ok(());
        }
        app.asset_protocol_scope()
            .allow_file(path.as_std_path())
            .map_err(|error| format!("failed to allow attachment preview asset: {error}"))?;
        self.authorized_attachment_assets.insert(path.to_path_buf());
        Ok(())
    }

    fn reconcile_attachment_asset_authorizations(&mut self) -> Result<(), String> {
        let desired = self
            .state
            .composer
            .image_attachment_paths
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let missing = desired
            .difference(&self.authorized_attachment_assets)
            .cloned()
            .collect::<Vec<_>>();
        let stale =
            attachment_authorizations_to_revoke(&self.authorized_attachment_assets, &desired);
        if missing.is_empty() && stale.is_empty() {
            return Ok(());
        }
        let app = self
            .attachment_asset_app
            .clone()
            .ok_or_else(|| "attachment preview authorization owner is unavailable".to_string())?;
        for path in missing {
            app.asset_protocol_scope()
                .allow_file(path.as_std_path())
                .map_err(|error| format!("failed to allow attachment preview asset: {error}"))?;
            self.authorized_attachment_assets.insert(path);
        }
        for path in stale {
            app.asset_protocol_scope()
                .forbid_file(path.as_std_path())
                .map_err(|error| format!("failed to revoke attachment preview asset: {error}"))?;
            self.authorized_attachment_assets.remove(&path);
        }
        Ok(())
    }

    pub(crate) fn refresh_snapshot(&mut self) {
        if self.state.background_mutation_pending() {
            self.state
                .set_status_message("refresh cannot start while a background mutation is active");
            return;
        }
        let app = self.app.clone();
        let selected_session_id = self
            .state
            .selected_session_id()
            .or(self.state.app_state.current_session_id);
        let target = SnapshotRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            selected_session_id,
        };
        let request_id = self.snapshot_requests.begin(target.clone());
        self.state.begin_snapshot_refresh();
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
            let _ = runtime_tx.send(RuntimeMessage::SnapshotLoaded {
                request_id,
                target,
                result,
            });
        });
    }

    fn spawn_snapshot_refresh_for_session(&mut self, session_id: SessionId) {
        let app = self.app.clone();
        let target = SnapshotRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            selected_session_id: Some(session_id),
        };
        let request_id = self.snapshot_requests.begin(target.clone());
        self.state.begin_snapshot_refresh();
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
            let _ = runtime_tx.send(RuntimeMessage::SnapshotLoaded {
                request_id,
                target,
                result,
            });
        });
    }

    pub(crate) fn select_session_and_open(&mut self, index: usize) -> bool {
        if !self.ensure_navigation_admission("session") {
            return false;
        }
        let Some(session_id) = self
            .state
            .snapshot
            .session_rows
            .get(index)
            .map(|row| row.session_id)
        else {
            self.state
                .set_status_message("session selection is no longer available");
            return false;
        };
        self.invalidate_session_target_requests();
        self.state
            .set_status_message(format!("opening session {session_id}..."));
        let request_id = self.state.begin_session_load(session_id);
        self.spawn_session_load(session_id, SessionLoadReason::UserSelection, request_id);
        true
    }

    pub(crate) fn select_project_and_open(&mut self, index: usize) -> bool {
        if !self.ensure_navigation_admission("project") {
            return false;
        }
        let Some(path) = self
            .state
            .snapshot
            .project_rows
            .get(index)
            .map(|row| Utf8PathBuf::from(&row.path))
        else {
            self.state
                .set_status_message("project selection is no longer available");
            return false;
        };
        if path == self.app.workspace.root {
            return true;
        }
        self.invalidate_session_target_requests();
        self.state
            .set_status_message(format!("opening project {}...", path));
        let request_id = self.state.begin_workspace_load(path.clone(), None);
        self.spawn_workspace_load(path, request_id);
        true
    }

    pub(crate) fn rejoin_session_if_admitted(&mut self, index: usize) -> bool {
        if !self.ensure_navigation_admission("running session") {
            return false;
        }
        let Some(row) = self.state.snapshot.session_rows.get(index) else {
            self.state
                .set_status_message("session selection is no longer available");
            return false;
        };
        if row.loaded_status != LoadedSessionStatus::Active {
            self.state
                .set_status_message("selected session is not an active loaded session");
            return false;
        }
        let session_id = row.session_id;
        self.invalidate_session_target_requests();
        self.state
            .set_status_message(format!("rejoining running session {session_id}..."));
        let request_id = self.state.begin_session_load(session_id);
        self.spawn_session_rejoin(session_id, request_id);
        true
    }

    fn ensure_navigation_admission(&mut self, target: &str) -> bool {
        let agent_tree_active = self.current_agent_tree_active();
        let Some(reason) = navigation_admission_blocker(
            self.state.is_busy(),
            self.state.background_mutation_pending(),
            self.state.navigation_loading(),
            agent_tree_active,
            self.run_lifecycle.root_is_finalizing(),
        ) else {
            return true;
        };
        self.state
            .set_status_message(format!("{target} cannot change while {reason}"));
        false
    }

    fn current_agent_tree_active(&self) -> bool {
        agent_activity_records_are_active(&self.current_agent_activity_records())
    }

    fn current_agent_activity_records(&self) -> Vec<AgentActivityRecord> {
        let Some(session_id) = self.state.app_state.current_session_id else {
            return Vec::new();
        };
        activity_records_for_projection(
            session_id,
            self.app.run_service.agent_activity_records(session_id),
            self.loaded_agent_activity_records.as_ref(),
        )
    }

    fn invalidate_session_target_requests(&mut self) {
        self.invalidate_session_search_requests();
        self.snapshot_requests.clear();
        self.turn_page_requests.clear();
        self.live_session_refresh_requests.clear();
        self.current_session_refresh_requests.clear();
        self.current_todo_refresh_requests.clear();
        self.durable_agent_activity_refresh_requests.clear();
        self.history_export_requests.clear();
        self.state.finish_snapshot_refresh();
        self.state.finish_turn_page_load();
        self.state.finish_current_todo_refresh();
        self.state.finish_history_export();
    }

    fn invalidate_session_search_requests(&mut self) {
        for operation_id in self.session_search_requests.clear() {
            let _ = self.state.finish_session_search(operation_id);
        }
    }

    pub(crate) fn delete_session(&mut self, session_id: SessionId) -> bool {
        if !self.ensure_navigation_admission("chat deletion") {
            return false;
        }
        if !self
            .state
            .snapshot
            .session_rows
            .iter()
            .any(|row| row.session_id == session_id)
        {
            self.state
                .set_status_message("chat deletion target is no longer available");
            return false;
        }
        self.invalidate_session_target_requests();
        self.state
            .set_status_message(format!("deleting chat {}...", session_id));
        let operation_id = self.state.begin_session_delete_mutation();
        self.spawn_session_delete(session_id, operation_id);
        true
    }

    pub(crate) fn archive_session(&mut self, session_id: SessionId, archived: bool) -> bool {
        if !self.ensure_navigation_admission("chat archive state") {
            return false;
        }
        if !self
            .state
            .snapshot
            .session_rows
            .iter()
            .any(|row| row.session_id == session_id)
        {
            self.state
                .set_status_message("chat archive target is no longer available");
            return false;
        }
        self.invalidate_session_target_requests();
        self.state.set_status_message(if archived {
            format!("archiving chat {}...", session_id)
        } else {
            format!("unarchiving chat {}...", session_id)
        });
        let operation_id = self.state.begin_session_archive_mutation();
        let target = SessionMutationRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            project_id: self.app.workspace.project_id,
            session_id,
            operation_id,
        };
        self.spawn_session_archive(
            target,
            archived,
            self.state.view.session_search_text.clone(),
            self.state.view.session_search_include_archived,
        );
        true
    }

    pub(crate) fn rollback_session(&mut self, session_id: SessionId) -> bool {
        if !self.ensure_navigation_admission("chat rollback") {
            return false;
        }
        let Some(row) = self
            .state
            .snapshot
            .session_rows
            .iter()
            .find(|row| row.session_id == session_id)
        else {
            self.state
                .set_status_message("chat rollback target is no longer available");
            return false;
        };
        if row.loaded_status == LoadedSessionStatus::Active {
            self.state
                .set_status_message("running sessions cannot be rolled back");
            return false;
        }
        self.invalidate_session_target_requests();
        self.state.set_status_message(format!(
            "rolling back latest turn in chat {}...",
            session_id
        ));
        let operation_id = self.state.begin_session_rollback_mutation();
        let target = SessionMutationRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            project_id: self.app.workspace.project_id,
            session_id,
            operation_id,
        };
        self.spawn_session_rollback(
            target,
            self.state.view.session_search_text.clone(),
            self.state.view.session_search_include_archived,
        );
        true
    }

    pub(crate) fn fork_session(&mut self, session_id: SessionId) -> bool {
        if !self.ensure_navigation_admission("chat fork") {
            return false;
        }
        let Some(row) = self
            .state
            .snapshot
            .session_rows
            .iter()
            .find(|row| row.session_id == session_id)
        else {
            self.state
                .set_status_message("chat fork target is no longer available");
            return false;
        };
        let title = format!("{} fork", row.title);
        self.invalidate_session_target_requests();
        self.state
            .set_status_message(format!("forking chat {}...", session_id));
        let operation_id = self.state.begin_session_maintenance_mutation();
        let target = SessionMutationRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            project_id: self.app.workspace.project_id,
            session_id,
            operation_id,
        };
        self.spawn_session_fork(target, Some(title));
        true
    }

    pub(crate) fn interrupt_session(&mut self, session_id: SessionId) -> bool {
        if !self.ensure_navigation_admission("running chat interrupt") {
            return false;
        }
        if !self
            .state
            .snapshot
            .session_rows
            .iter()
            .any(|row| row.session_id == session_id)
        {
            self.state
                .set_status_message("running chat target is no longer available");
            return false;
        }
        self.invalidate_session_target_requests();
        if self.state.app_state.current_session_id == Some(session_id) && self.state.is_busy() {
            self.cancel_active_run();
            return true;
        }
        self.state
            .set_status_message(format!("interrupting running chat {}...", session_id));
        let operation_id = self.state.begin_session_maintenance_mutation();
        let target = SessionMutationRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            project_id: self.app.workspace.project_id,
            session_id,
            operation_id,
        };
        self.spawn_session_interrupt(target);
        true
    }

    pub(crate) fn set_session_memory_mode(
        &mut self,
        session_id: SessionId,
        mode: SessionMemoryMode,
    ) -> bool {
        if !self.ensure_navigation_admission("chat memory mode") {
            return false;
        }
        if !self
            .state
            .snapshot
            .session_rows
            .iter()
            .any(|row| row.session_id == session_id)
        {
            self.state
                .set_status_message("chat memory target is no longer available");
            return false;
        }
        self.invalidate_session_target_requests();
        self.state.set_status_message(format!(
            "setting chat {} memory mode to {}...",
            session_id,
            mode.key()
        ));
        let operation_id = self.state.begin_session_maintenance_mutation();
        let target = SessionMutationRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            project_id: self.app.workspace.project_id,
            session_id,
            operation_id,
        };
        self.spawn_session_memory_mode(target, mode);
        true
    }

    pub(crate) fn set_session_search(&mut self, text: String) -> bool {
        if !self.ensure_navigation_admission("session search") {
            return false;
        }
        self.state.set_session_search_text(text.clone());
        let operation_id = self.state.begin_session_search();
        let request_id = self.session_search_requests.begin(operation_id);
        self.spawn_session_search(
            text,
            self.state.view.session_search_include_archived,
            request_id,
        );
        true
    }

    pub(crate) fn set_session_search_include_archived(&mut self, include_archived: bool) -> bool {
        if !self.ensure_navigation_admission("session search") {
            return false;
        }
        self.state
            .set_session_search_include_archived(include_archived);
        let operation_id = self.state.begin_session_search();
        let request_id = self.session_search_requests.begin(operation_id);
        self.spawn_session_search(
            self.state.view.session_search_text.clone(),
            include_archived,
            request_id,
        );
        true
    }

    pub(crate) fn delete_project(&mut self, project_id: ProjectId) -> bool {
        if !self.ensure_navigation_admission("project deletion") {
            return false;
        }
        let Some(project_root) = self
            .state
            .snapshot
            .project_rows
            .iter()
            .find(|row| row.project_id == project_id)
            .map(|row| Utf8PathBuf::from(&row.path))
        else {
            self.state
                .set_status_message("project deletion target is no longer available");
            return false;
        };
        self.invalidate_session_target_requests();
        self.state
            .set_status_message(format!("deleting project {}...", project_id));
        let mut hidden_roots = self.preferences.deleted_project_roots.clone();
        hidden_roots.extend(internal_desktop_project_roots(
            self.app.session_service.store.paths().data_dir.as_path(),
        ));
        if !hidden_roots.iter().any(|root| root == &project_root) {
            hidden_roots.push(project_root.clone());
        }
        let operation_id = self.state.begin_project_delete_mutation();
        let target = ProjectDeleteRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            owner_project_id: self.app.workspace.project_id,
            project_id,
            project_root,
            operation_id,
        };
        self.spawn_project_delete(target, hidden_roots);
        true
    }

    pub(crate) fn export_history_markdown_auto(&mut self, session_id: SessionId) {
        let Some(title) = self
            .state
            .snapshot
            .session_rows
            .iter()
            .find(|row| row.session_id == session_id)
            .map(|row| row.label.clone())
        else {
            self.state
                .set_status_message("history export target is no longer available");
            return;
        };
        let default_file_name = history_markdown_file_name(&title, session_id);
        let export_path = self
            .app
            .workspace
            .root
            .join(".moyai")
            .join("history-exports")
            .join(default_file_name);
        self.export_history_markdown_to_path(session_id, export_path);
    }

    pub(crate) fn export_open_transcript_markdown_auto(&mut self) {
        if !self.state.can_export_history() {
            self.state.set_status_message(
                "transcript export cannot start while another operation is active",
            );
            return;
        }
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

    pub(crate) fn export_history_markdown_to_path(
        &mut self,
        session_id: SessionId,
        path: Utf8PathBuf,
    ) {
        if !self.state.can_export_history() {
            self.state.set_status_message(
                "history export cannot start while another operation is active",
            );
            return;
        }
        if !self
            .state
            .snapshot
            .session_rows
            .iter()
            .any(|row| row.session_id == session_id)
        {
            self.state
                .set_status_message("history export target is no longer available");
            return;
        }
        self.state
            .set_status_message("exporting history markdown...");
        let target = HistoryExportRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            session_id,
        };
        let request_id = self.history_export_requests.begin(target.clone());
        self.state.begin_history_export();
        self.spawn_history_markdown_export(
            session_id,
            normalize_markdown_export_path(path),
            request_id,
            target,
        );
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
        if !self.state.can_begin_navigation() {
            self.state
                .set_status_message("turn page cannot change while another operation is active");
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

    fn spawn_history_markdown_export(
        &self,
        session_id: SessionId,
        export_path: Utf8PathBuf,
        request_id: LatestRequestId,
        target: HistoryExportRequestTarget,
    ) {
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
            let _ = runtime_tx.send(RuntimeMessage::HistoryExported {
                request_id,
                target,
                result,
            });
        });
    }

    fn spawn_session_load(
        &self,
        session_id: SessionId,
        reason: SessionLoadReason,
        request_id: NavigationRequestId,
    ) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session runtime");
            let result = runtime.block_on(async move {
                let detail = load_session_detail(&app, session_id)
                    .await
                    .map_err(|error| error.to_string())?;
                loaded_session_from_detail_with_activity(&app, detail).await
            });
            let _ = runtime_tx.send(RuntimeMessage::SessionLoaded {
                request_id,
                session_id,
                reason,
                result,
            });
        });
    }

    fn spawn_current_session_refresh(&mut self, session_id: SessionId) {
        let app = self.app.clone();
        let target = SessionRefreshRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            session_id,
        };
        let request_id = self.current_session_refresh_requests.begin(target.clone());
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop current-session-refresh runtime");
            let result = runtime.block_on(async move {
                let detail = load_latest_session_detail(&app, session_id)
                    .await
                    .map_err(|error| error.to_string())?;
                loaded_session_from_detail_with_activity(&app, detail).await
            });
            let _ = runtime_tx.send(RuntimeMessage::CurrentSessionRefreshed {
                request_id,
                target,
                result,
            });
        });
    }

    fn spawn_turn_page_load(&mut self, session_id: SessionId, offset: usize, limit: usize) {
        let app = self.app.clone();
        let target = SessionPageRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            session_id,
            offset,
            limit,
        };
        let request_id = self.turn_page_requests.begin(target.clone());
        self.state.begin_turn_page_load();
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
                    agent_activity_records: None,
                })
            });
            let _ = runtime_tx.send(RuntimeMessage::TurnPageLoaded {
                request_id,
                target,
                result,
            });
        });
    }

    fn spawn_live_session_refresh(&mut self, session_id: SessionId, offset: usize, limit: usize) {
        let app = self.app.clone();
        let target = SessionRefreshRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            session_id,
        };
        let request_id = self.live_session_refresh_requests.begin(target.clone());
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
                    agent_activity_records: None,
                })
            });
            let _ = runtime_tx.send(RuntimeMessage::LiveSessionRefreshed {
                request_id,
                target,
                result,
            });
        });
    }

    fn spawn_latest_live_session_refresh(&mut self, session_id: SessionId) {
        let app = self.app.clone();
        let target = SessionRefreshRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            session_id,
        };
        let request_id = self.live_session_refresh_requests.begin(target.clone());
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop latest-session-refresh runtime");
            let result = runtime.block_on(async move {
                let detail = load_latest_session_detail(&app, session_id)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(loaded_session_from_detail(detail, None))
            });
            let _ = runtime_tx.send(RuntimeMessage::LiveSessionRefreshed {
                request_id,
                target,
                result,
            });
        });
    }

    fn spawn_durable_agent_activity_refresh(&mut self, session_id: SessionId) {
        let app = self.app.clone();
        let target = SessionRefreshRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            session_id,
        };
        let request_id = self
            .durable_agent_activity_refresh_requests
            .begin(target.clone());
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop durable-agent-activity runtime");
            let result = runtime.block_on(async move {
                app.run_service
                    .durable_agent_activity_records(session_id)
                    .await
                    .map_err(|error| error.to_string())
            });
            let _ = runtime_tx.send(RuntimeMessage::DurableAgentActivityRefreshed {
                request_id,
                target,
                result,
            });
        });
    }

    fn spawn_session_rejoin(&self, session_id: SessionId, request_id: NavigationRequestId) {
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
                let agent_activity_records = app
                    .run_service
                    .durable_agent_activity_records(session_id)
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
                    agent_activity_records: Some(agent_activity_records),
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

    fn spawn_session_cancel_persist(&mut self, session_id: SessionId) {
        let app = self.app.clone();
        let target = SessionRefreshRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            session_id,
        };
        let request_id = self.current_session_refresh_requests.begin(target.clone());
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
                let detail = load_session_detail(&app, session_id)
                    .await
                    .map_err(|error| error.to_string())?;
                loaded_session_from_detail_with_activity(&app, detail).await
            });
            let _ = runtime_tx.send(RuntimeMessage::CurrentSessionRefreshed {
                request_id,
                target,
                result,
            });
        });
    }

    fn spawn_session_delete(&self, session_id: SessionId, operation_id: DesktopAsyncOperationId) {
        let app = self.app.clone();
        let target = SessionDeleteRequestTarget {
            workspace_root: app.workspace.root.clone(),
            project_id: app.workspace.project_id,
            session_id,
            operation_id,
        };
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
            let _ = runtime_tx.send(RuntimeMessage::SessionDeleted { target, result });
        });
    }

    fn spawn_session_archive(
        &self,
        target: SessionMutationRequestTarget,
        archived: bool,
        query: String,
        include_archived: bool,
    ) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let session_id = target.session_id;
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
                target,
                archived,
                result,
            });
        });
    }

    fn spawn_session_rollback(
        &self,
        target: SessionMutationRequestTarget,
        query: String,
        include_archived: bool,
    ) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let session_id = target.session_id;
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
                let detail = load_session_detail(&app, session_id)
                    .await
                    .map_err(|error| error.to_string())?;
                let loaded = loaded_session_from_detail_with_activity(&app, detail).await?;
                Ok(DesktopRollbackLoaded {
                    snapshot,
                    loaded,
                    dropped_turn_count: rollback.dropped_turn_ids.len(),
                })
            });
            let _ = runtime_tx.send(RuntimeMessage::SessionRolledBack { target, result });
        });
    }

    fn spawn_session_fork(&self, target: SessionMutationRequestTarget, title: Option<String>) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let source_session_id = target.session_id;
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
            let _ = runtime_tx.send(RuntimeMessage::SessionOperationApplied { target, result });
        });
    }

    fn spawn_session_interrupt(&self, target: SessionMutationRequestTarget) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let session_id = target.session_id;
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
            let _ = runtime_tx.send(RuntimeMessage::SessionOperationApplied { target, result });
        });
    }

    fn spawn_session_memory_mode(
        &self,
        target: SessionMutationRequestTarget,
        mode: SessionMemoryMode,
    ) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let session_id = target.session_id;
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
            let _ = runtime_tx.send(RuntimeMessage::SessionOperationApplied { target, result });
        });
    }

    fn spawn_session_search(
        &self,
        query: String,
        include_archived: bool,
        request_id: SessionSearchRequestId,
    ) {
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
            let _ = runtime_tx.send(RuntimeMessage::SessionSearchLoaded { request_id, result });
        });
    }

    fn spawn_project_delete(
        &self,
        target: ProjectDeleteRequestTarget,
        hidden_roots: Vec<Utf8PathBuf>,
    ) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        let project_id = target.project_id;
        let project_root_for_thread = target.project_root.clone();
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
            let _ = runtime_tx.send(RuntimeMessage::ProjectDeleted { target, result });
        });
    }

    fn spawn_current_todos_refresh(&mut self, session_id: SessionId) {
        let service = self.app.session_service.clone();
        let target = SessionRefreshRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            session_id,
        };
        let request_id = self.current_todo_refresh_requests.begin(target.clone());
        self.state.begin_current_todo_refresh();
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
            let _ = runtime_tx.send(RuntimeMessage::CurrentTodosLoaded {
                request_id,
                target,
                result,
            });
        });
    }

    pub(crate) fn start_run(&mut self, prompt: String) -> bool {
        if self.state.navigation_loading() {
            self.state
                .set_status_message("wait for navigation to finish before sending");
            return false;
        }
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() {
            self.state.set_status_message("prompt is empty");
            return false;
        }
        let prompt_dispatch = crate::session::PromptDispatchPart::raw(&prompt);
        self.launch_run_with_options(prompt, prompt_dispatch, None, false)
    }

    pub(crate) fn start_quick_chat(&mut self) -> bool {
        if !self.ensure_navigation_admission("chat") {
            return false;
        }
        self.invalidate_session_target_requests();
        let Some(root) = quick_chat_workspace_directory() else {
            self.start_new_chat_with_global_access();
            self.persist_preferences();
            return true;
        };
        if self.is_quick_chat_workspace() {
            self.start_new_chat_with_global_access();
            self.persist_preferences();
            return true;
        }
        if let Err(error) = std::fs::create_dir_all(root.as_std_path()) {
            self.state.set_status_message(format!(
                "failed to prepare quick chat workspace {}: {error}",
                root
            ));
            return false;
        }
        self.state.hide_overlay();
        self.state
            .set_status_message("opening workspace-free quick chat...");
        let request_id = self.state.begin_workspace_load(root.clone(), None);
        self.spawn_workspace_load(root, request_id);
        true
    }

    pub(crate) fn start_project_session(&mut self, index: usize) -> bool {
        if !self.ensure_navigation_admission("project") {
            return false;
        }
        let Some(path) = self
            .state
            .snapshot
            .project_rows
            .get(index)
            .map(|row| Utf8PathBuf::from(&row.path))
        else {
            self.state
                .set_status_message("project selection is no longer available");
            return false;
        };
        self.invalidate_session_target_requests();
        self.state.hide_overlay();
        if path == self.app.workspace.root {
            self.state.select_project(index);
            self.start_new_chat_with_global_access();
            self.state.set_status_message("new development chat ready");
            self.persist_preferences();
            return true;
        }
        self.state.set_status_message(format!(
            "opening project {} for a new development chat...",
            path
        ));
        let request_id = self
            .state
            .begin_new_project_session_workspace_load(path.clone());
        self.spawn_workspace_load_for_new_project_session(path, request_id);
        true
    }

    pub(crate) fn open_quick_chat_session(&mut self, index: usize) -> bool {
        if !self.ensure_navigation_admission("chat") {
            return false;
        }
        let Some(session_id) = self
            .state
            .snapshot
            .chat_session_rows
            .get(index)
            .map(|row| row.session_id)
        else {
            self.state.set_status_message("select a chat first");
            return false;
        };
        let Some(root) = quick_chat_workspace_directory() else {
            self.state
                .set_status_message("quick chat workspace is unavailable");
            return false;
        };
        if self.is_quick_chat_workspace() {
            if let Some(row_index) = self
                .state
                .snapshot
                .session_rows
                .iter()
                .position(|row| row.session_id == session_id)
            {
                return self.select_session_and_open(row_index);
            }
        }
        self.invalidate_session_target_requests();
        self.state.hide_overlay();
        self.state
            .set_status_message(format!("opening chat {session_id}..."));
        let request_id = self
            .state
            .begin_workspace_load(root.clone(), Some(session_id));
        self.spawn_workspace_load_for_selection(root, Some(session_id), request_id);
        true
    }

    pub(crate) fn delete_quick_chat_session(&mut self, session_id: SessionId) -> bool {
        if !self.ensure_navigation_admission("quick chat deletion") {
            return false;
        }
        if !self
            .state
            .snapshot
            .chat_session_rows
            .iter()
            .any(|row| row.session_id == session_id)
        {
            self.state
                .set_status_message("quick-chat deletion target is no longer available");
            return false;
        }
        self.invalidate_session_target_requests();
        self.state
            .set_status_message(format!("deleting chat {}...", session_id));
        let operation_id = self.state.begin_session_delete_mutation();
        self.spawn_session_delete(session_id, operation_id);
        true
    }

    pub(crate) fn create_project_from_picker(&mut self) {
        if !self.ensure_navigation_admission("project") {
            return;
        }
        let start_dir = (!self.is_quick_chat_workspace()).then_some(&self.app.workspace.cwd);
        match pick_workspace_directory(start_dir) {
            Ok(Some(path)) => {
                self.invalidate_session_target_requests();
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

    pub(crate) fn start_review_uncommitted(&mut self, prompt: String) -> bool {
        let prompt = prompt.trim().to_string();
        let prompt_dispatch = crate::session::PromptDispatchPart::raw(&prompt);
        self.launch_run_with_options(
            prompt,
            prompt_dispatch,
            Some(ReviewRequest::Uncommitted),
            false,
        )
    }

    pub(crate) fn start_prompt_enhance(&mut self, raw_prompt: String) -> bool {
        let raw_prompt = raw_prompt.trim().to_string();
        if !unique_background_request_admission_open(false, self.state.prompt_enhance_pending()) {
            self.state
                .set_status_message("prompt enhancement is already in progress");
            return false;
        }
        if raw_prompt.is_empty()
            || self.state.is_busy()
            || self.state.navigation_loading()
            || self.current_agent_tree_active()
            || self.run_lifecycle.root_is_active()
        {
            self.state
                .set_status_message("prompt enhancement is not currently available");
            return false;
        }
        let request_id = self.next_enhance_request_id;
        let Some(next_request_id) = request_id.checked_add(1) else {
            self.state
                .set_status_message("prompt enhancement request generation is exhausted");
            return false;
        };
        self.next_enhance_request_id = next_request_id;
        let target = DraftRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            session_id: self.state.app_state.current_session_id,
        };
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
            let _ = runtime_tx.send(RuntimeMessage::EnhanceFinished {
                request_id,
                target,
                result,
            });
        });
        true
    }

    pub(crate) fn send_prompt_review(&mut self, send_enhanced: bool, review_draft: String) -> bool {
        if self.state.navigation_loading() {
            self.state
                .set_status_message("wait for navigation to finish before sending");
            return false;
        }
        if send_enhanced {
            self.state.set_review_draft(review_draft);
        }
        let Some(prompt_dispatch) = self.state.build_prompt_dispatch(send_enhanced) else {
            self.state
                .set_status_message("enhanced draft is not ready yet");
            return false;
        };
        let prompt = prompt_dispatch.dispatch_prompt_text.clone();
        self.launch_run_with_options(prompt, prompt_dispatch, None, true)
    }

    pub(crate) fn load_provider_models(&mut self) -> bool {
        if self.provider_model_load_pending() {
            self.state
                .set_status_message("provider model load is already in progress");
            return false;
        }
        let normalized =
            normalize_provider_base_url(&self.state.provider_config.provider_base_url_input);
        if normalized.is_empty() {
            self.state.fail_provider_model_load("provider URL is empty");
            return false;
        }
        let target = ProviderCatalogRequestTarget {
            base_url: normalized.clone(),
            metadata_mode: self.state.provider_config.provider_metadata_mode_input,
            config_generation: self.state.provider_config.config_generation,
            selected_model_id: self
                .state
                .provider_config
                .provider_selected_model_id_input
                .clone(),
        };
        let request_id = self.provider_catalog_requests.begin(target.clone());
        self.state.begin_provider_model_load(normalized.clone());
        let runtime_tx = self.runtime_tx.clone();
        let config = provider_catalog_probe_config(
            self.state.provider_config.effective_config.clone(),
            normalized.clone(),
            target.metadata_mode,
        );
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
                request_id,
                target,
                result,
            });
        });
        true
    }

    pub(crate) fn provider_model_load_pending(&self) -> bool {
        !unique_background_request_admission_open(
            self.provider_catalog_requests.is_pending(),
            self.state.provider_model_load_pending(),
        )
    }

    pub(crate) fn accept_provider_action_input(
        &mut self,
        base_url: String,
        metadata_mode: ProviderMetadataMode,
        context_window: String,
        max_output_tokens: String,
        selected_model_id: String,
    ) {
        let target_changed = self.state.accept_provider_action_input(
            base_url,
            metadata_mode,
            context_window,
            max_output_tokens,
            selected_model_id,
        );
        if target_changed {
            self.provider_catalog_requests.clear();
        }
    }

    pub(crate) fn drive_startup_readiness(&mut self) {
        self.start_startup_provider_readiness();
        self.start_startup_docling_readiness();
    }

    fn start_startup_provider_readiness(&mut self) {
        if !self.state.startup_provider_readiness_pending()
            || self.startup_provider_requests.is_pending()
        {
            return;
        }
        let config = self.state.provider_config.effective_config.clone();
        let normalized = normalize_provider_base_url(&config.model.base_url);
        let target = ProviderReadinessRequestTarget {
            base_url: normalized.clone(),
            model_id: config.model.model.clone(),
            metadata_mode: config.model.provider_metadata_mode,
            supports_tools: config.model.supports_tools,
            supports_reasoning: config.model.supports_reasoning,
            supports_images: config.model.supports_images,
            parallel_tool_calls: config.model.parallel_tool_calls,
            max_parallel_predictions: config.model.max_parallel_predictions,
            config_generation: self.state.provider_config.config_generation,
        };
        let request_id = self.startup_provider_requests.begin(target.clone());
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let mut config = config;
            config.model.base_url = normalized;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop provider-readiness runtime");
            let report = runtime.block_on(async move {
                check_model_availability(&config, None, None, false).await
            });
            let _ = runtime_tx.send(RuntimeMessage::StartupProviderChecked {
                request_id,
                target,
                report,
            });
        });
    }

    fn start_startup_docling_readiness(&mut self) {
        if !self.state.startup_docling_readiness_pending()
            || self.startup_docling_requests.is_pending()
        {
            return;
        }
        let config = self.state.provider_config.effective_config.docling.clone();
        let normalized = normalize_docling_base_url(&config.base_url);
        if normalized.is_empty() {
            self.state
                .fail_startup_docling_check("Docling Serve URL が未設定です。");
            return;
        }
        let target = DoclingRequestTarget {
            base_url: normalized.clone(),
            config_generation: self.state.provider_config.config_generation,
        };
        let request_id = self.startup_docling_requests.begin(target.clone());
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
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
                request_id,
                target,
                result,
            });
        });
    }

    fn reset_effective_config_without_network(&mut self, config: ResolvedConfig) {
        let catalog_was_pending =
            self.provider_catalog_requests.is_pending() || self.state.provider_model_load_pending();
        self.provider_catalog_requests.clear();
        self.startup_provider_requests.clear();
        self.startup_docling_requests.clear();
        if catalog_was_pending {
            self.state.cancel_provider_model_load();
        }
        commit_effective_config(&mut self.state, &self.run_lifecycle, config);
        self.state.retarget_startup_readiness_for_config_change();
    }

    fn sync_loaded_session_access_mode(&self, access_mode: crate::config::AccessMode) {
        self.run_lifecycle.set_access_mode(access_mode);
    }

    fn start_new_chat_with_global_access(&mut self) {
        self.state.start_new_chat();
        if self.state.app_state.current_session_id.is_none() {
            let access_mode = self.app.config.permissions.access_mode;
            self.state.provider_config.update_access_mode(access_mode);
            self.run_lifecycle.set_access_mode(access_mode);
        }
    }

    pub(crate) fn apply_provider_session(&mut self) -> bool {
        if !self.state.can_apply_provider_selection() {
            self.state.set_status_message(
                "load the current provider model list and select a model before applying",
            );
            return false;
        }
        let setup_overlay = self.state.view.startup_overlay_forced;
        let Some(config) = self.apply_provider_selection_to_effective_config() else {
            return false;
        };
        self.reset_effective_config_without_network(config);
        self.state.mark_startup_config_reviewed();
        self.state
            .set_status_message("applied provider selection to this UI session");
        if !setup_overlay {
            self.state.hide_overlay();
        }
        true
    }

    pub(crate) fn save_provider_global(&mut self) -> bool {
        if !self.state.can_apply_provider_selection() {
            self.state.set_status_message(
                "load the current provider model list and select a model before saving",
            );
            return false;
        }
        let Some(config) = self.apply_provider_selection_to_effective_config() else {
            return false;
        };
        if let Err(error) = self.stage_provider_config_for_persistence(&config) {
            self.state
                .set_status_message(format!("config save failed: {error}"));
            return false;
        }
        match self.state.provider_config.config_editor.save_scope(
            &self.app.workspace.root,
            crate::tui::config_editor::ConfigSaveScope::Global,
        ) {
            Ok(message) => {
                self.app.config = config.clone();
                if !self.reload_config() {
                    return false;
                }
                self.state.mark_startup_config_reviewed();
                self.state.set_status_message(message);
                true
            }
            Err(error) => {
                self.state
                    .set_status_message(format!("config save failed: {error}"));
                false
            }
        }
    }

    pub(crate) fn apply_session_config(&mut self) -> bool {
        match self
            .state
            .provider_config
            .config_editor
            .build_session_override()
        {
            Ok(patch) => {
                let config = apply_config_patch(self.app.config.clone(), patch.clone());
                self.reset_effective_config_without_network(config);
                self.state
                    .set_status_message("applied override to this UI session");
                true
            }
            Err(error) => {
                self.state
                    .set_status_message(format!("config error: {error}"));
                false
            }
        }
    }

    pub(crate) fn root_run_generation(&self) -> Option<u64> {
        self.run_lifecycle.root_generation()
    }

    fn last_root_run_epoch(&self) -> u64 {
        self.next_root_run_generation.saturating_sub(1)
    }

    pub(crate) fn access_mode_mutation_runtime_contract(&self) -> (String, bool) {
        let root_run_generation = self.root_run_generation();
        let agent_tree_active = self.current_agent_tree_active();
        (
            access_runtime_owner_token(
                root_run_generation,
                agent_tree_active,
                self.last_root_run_epoch(),
            ),
            !self.state.navigation_loading()
                && !self.state.background_mutation_pending()
                && access_runtime_allows_mutation(root_run_generation, agent_tree_active),
        )
    }

    pub(crate) fn access_mode_mutation_admission_open(&self) -> bool {
        self.access_mode_mutation_runtime_contract().1
    }

    fn access_mode_persistence_target_relation(
        &self,
        target: &AccessModePersistenceTarget,
    ) -> AccessModePersistenceTargetRelation {
        let (runtime_owner_token, _) = self.access_mode_mutation_runtime_contract();
        let relation = access_mode_persistence_target_relation(
            target,
            &self.app.workspace.root,
            self.state.app_state.current_session_id,
            self.state.provider_config.config_generation,
            &runtime_owner_token,
        );
        if relation != AccessModePersistenceTargetRelation::Stale {
            return relation;
        }
        self.access_mode_persistence_relation_after_root_finish(target)
    }

    fn access_mode_persistence_relation_after_root_finish(
        &self,
        target: &AccessModePersistenceTarget,
    ) -> AccessModePersistenceTargetRelation {
        let Some(root_run_generation) = target.root_run_generation else {
            return AccessModePersistenceTargetRelation::Stale;
        };
        if target.workspace_root != self.app.workspace.root
            || target.config_generation != self.state.provider_config.config_generation
            || self.root_run_generation().is_some()
            || self.last_root_run_epoch() != root_run_generation
            || target.runtime_owner_token != format!("root:{root_run_generation}")
        {
            return AccessModePersistenceTargetRelation::Stale;
        }
        match (target.session_id, self.state.app_state.current_session_id) {
            (target_session_id, current_session_id) if target_session_id == current_session_id => {
                AccessModePersistenceTargetRelation::Exact
            }
            (None, Some(session_id)) => {
                AccessModePersistenceTargetRelation::AdoptedSession(session_id)
            }
            _ => AccessModePersistenceTargetRelation::Stale,
        }
    }

    pub(crate) fn toggle_access_mode_remembered(&mut self) -> bool {
        let session_service = self.app.session_service.clone();
        let expected_access_mode = self
            .state
            .provider_config
            .effective_config
            .permissions
            .access_mode;
        self.start_access_mode_persistence(
            ConfigEditorState::compare_and_set_global_access_mode,
            move |session_id, access_mode| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())?;
                runtime.block_on(async move {
                    session_service
                        .compare_and_set_root_session_access_mode(
                            session_id,
                            expected_access_mode,
                            access_mode,
                        )
                        .await
                        .and_then(|updated| {
                            updated.map(|_| ()).ok_or_else(|| {
                                crate::error::SessionError::Message(format!(
                                    "session {session_id} access mode changed before this update"
                                ))
                            })
                        })
                        .map_err(|error| error.to_string())
                })
            },
        )
    }

    fn start_access_mode_persistence<CompareAndSetGlobal, PersistSession>(
        &mut self,
        compare_and_set_global: CompareAndSetGlobal,
        persist_session: PersistSession,
    ) -> bool
    where
        CompareAndSetGlobal: FnMut(
                crate::config::AccessMode,
                crate::config::AccessMode,
            ) -> Result<Option<Utf8PathBuf>, String>
            + Send
            + 'static,
        PersistSession:
            FnOnce(SessionId, crate::config::AccessMode) -> Result<(), String> + Send + 'static,
    {
        if !self.access_mode_mutation_admission_open() {
            self.state.set_status_message(
                "access mode cannot change while navigation or an owner mutation is active",
            );
            return false;
        }
        let old_effective_access_mode = self
            .state
            .provider_config
            .effective_config
            .permissions
            .access_mode;
        let access_mode = old_effective_access_mode.next();
        let (runtime_owner_token, _) = self.access_mode_mutation_runtime_contract();
        let target = AccessModePersistenceTarget {
            operation_id: self.state.begin_access_mode_persistence(),
            workspace_root: self.app.workspace.root.clone(),
            session_id: self.state.app_state.current_session_id,
            config_generation: self.state.provider_config.config_generation,
            root_run_generation: self.root_run_generation(),
            runtime_owner_token,
            old_global_access_mode: self.app.config.permissions.access_mode,
            old_effective_access_mode,
            access_mode,
        };
        let request_id = self.access_mode_persistence_requests.begin(target.clone());
        let runtime_tx = self.runtime_tx.clone();
        let worker_target = target.clone();
        let worker = Arc::new(AccessModePersistenceWorker::new(
            compare_and_set_global,
            persist_session,
        ));
        let initial_worker = worker.clone();
        std::thread::spawn(move || {
            let result = initial_worker.persist_initial_owners(&worker_target);
            let _ = runtime_tx.send(RuntimeMessage::AccessModePersisted {
                request_id,
                target: worker_target,
                phase: AccessModePersistencePhase::InitialOwners,
                worker: initial_worker,
                result,
            });
        });
        self.state
            .set_status_message(if target.session_id.is_some() {
                "saving access mode to global config and the current root session"
            } else {
                "saving access mode to global config"
            });
        true
    }

    fn spawn_adopted_session_access_persistence(
        &self,
        request_id: LatestRequestId,
        target: AccessModePersistenceTarget,
        session_id: SessionId,
        remembered_path: Utf8PathBuf,
        worker: Arc<AccessModePersistenceWorker>,
    ) {
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let result = worker.persist_adopted_session(&target, session_id, remembered_path);
            let _ = runtime_tx.send(RuntimeMessage::AccessModePersisted {
                request_id,
                target,
                phase: AccessModePersistencePhase::AdoptedSession { session_id },
                worker,
                result,
            });
        });
    }

    fn resume_pending_access_mode_adoption(&mut self, session_id: SessionId) -> bool {
        let Some(pending) = self.pending_access_mode_adoption.as_ref() else {
            return false;
        };
        let request_id = pending.request_id;
        let target = pending.target.clone();
        let request_is_current = self
            .access_mode_persistence_requests
            .is_current(request_id, &target);
        let operation_is_current = self
            .state
            .access_mode_persistence_is_current(target.operation_id);
        let relation = self.access_mode_persistence_target_relation(&target);
        if !request_is_current
            || !operation_is_current
            || relation != AccessModePersistenceTargetRelation::AdoptedSession(session_id)
        {
            let _ = self.pending_access_mode_adoption.take();
            let _ = self
                .access_mode_persistence_requests
                .finish_if_current(request_id, &target);
            let _ = self
                .state
                .finish_access_mode_persistence(target.operation_id);
            let _ = self.reload_config();
            self.state.set_status_message(
                "access mode owner changed before session admission; current configuration was reloaded",
            );
            return false;
        }
        let pending = self
            .pending_access_mode_adoption
            .take()
            .expect("pending access mode adoption checked above");
        self.spawn_adopted_session_access_persistence(
            pending.request_id,
            pending.target,
            session_id,
            pending.remembered_path,
            pending.worker,
        );
        self.state.set_status_message(
            "global access mode saved; saving the admitted current root session",
        );
        true
    }

    fn settle_pending_access_mode_without_session(&mut self) {
        let Some(pending) = self.pending_access_mode_adoption.take() else {
            return;
        };
        let relation = self.access_mode_persistence_target_relation(&pending.target);
        let request_is_current = self
            .access_mode_persistence_requests
            .finish_if_current(pending.request_id, &pending.target);
        let operation_is_current = self
            .state
            .finish_access_mode_persistence(pending.target.operation_id);
        let target_is_current = request_is_current
            && operation_is_current
            && relation == AccessModePersistenceTargetRelation::Exact;
        if !target_is_current {
            let _ = self.reload_config();
            return;
        }
        self.app.config.permissions.access_mode = pending.target.access_mode;
        self.state
            .provider_config
            .update_access_mode(pending.target.access_mode);
        self.run_lifecycle
            .set_access_mode(pending.target.access_mode);
        self.state.set_status_message(format!(
            "global config access mode set to {} and remembered in {}",
            pending.target.access_mode.label(),
            pending.remembered_path
        ));
    }

    #[cfg(test)]
    fn toggle_access_mode_with_persistence<CompareAndSetGlobal, PersistSession>(
        &mut self,
        compare_and_set_global: CompareAndSetGlobal,
        persist_session: PersistSession,
    ) -> bool
    where
        CompareAndSetGlobal: FnMut(
            crate::config::AccessMode,
            crate::config::AccessMode,
        ) -> Result<Option<Utf8PathBuf>, String>,
        PersistSession: FnOnce(SessionId, crate::config::AccessMode) -> Result<(), String>,
    {
        if !self.access_mode_mutation_admission_open() {
            self.state.set_status_message(
                "access mode cannot change while navigation or an owner mutation is active",
            );
            return false;
        }
        let access_mode = self
            .state
            .provider_config
            .effective_config
            .permissions
            .access_mode
            .next();
        let old_global_access_mode = self.app.config.permissions.access_mode;
        let current_root_session_id = self.state.app_state.current_session_id;
        let remembered_path = match persist_desktop_access_mode_owners(
            old_global_access_mode,
            access_mode,
            current_root_session_id,
            compare_and_set_global,
            persist_session,
        ) {
            Ok(path) => path,
            Err(error) => {
                let _ = self.reload_config();
                self.state.set_status_message(format!(
                    "access mode was not changed; configuration was reloaded: {error}"
                ));
                return false;
            }
        };
        if self.state.app_state.current_session_id != current_root_session_id {
            self.state.set_status_message(
                "access mode owner changed before commit; reload the current chat".to_string(),
            );
            return false;
        }
        self.app.config.permissions.access_mode = access_mode;
        self.state.provider_config.update_access_mode(access_mode);
        self.run_lifecycle.set_access_mode(access_mode);
        if let Some(session_id) = current_root_session_id {
            for session in &mut self.state.app_state.sessions {
                if session.id == session_id {
                    session.access_mode = access_mode;
                }
            }
            for summary in &mut self.state.app_state.loaded_sessions {
                if summary.session.id == session_id {
                    summary.session.access_mode = access_mode;
                }
            }
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
            self.pending_permission_request_id = None;
            self.state.clear_permission();
        }
        let scope = if current_root_session_id.is_some() {
            "global config and current root session"
        } else {
            "global config"
        };
        let suffix = if auto_approved {
            "; pending confirmation approved"
        } else {
            ""
        };
        self.state.set_status_message(format!(
            "{scope} access mode set to {} and remembered in {}{suffix}",
            access_mode.label(),
            remembered_path
        ));
        true
    }

    pub(crate) fn save_global_config(&mut self) -> bool {
        match self.state.provider_config.config_editor.save_scope(
            &self.app.workspace.root,
            crate::tui::config_editor::ConfigSaveScope::Global,
        ) {
            Ok(message) => {
                if !self.reload_config() {
                    return false;
                }
                self.state.mark_startup_config_reviewed();
                self.state.set_status_message(message);
                true
            }
            Err(error) => {
                self.state
                    .set_status_message(format!("config save failed: {error}"));
                false
            }
        }
    }

    pub(crate) fn pick_global_config_toml_dialog(&mut self) -> Option<Utf8PathBuf> {
        match pick_config_toml_file() {
            Ok(path) => path,
            Err(error) => {
                self.state
                    .set_status_message(format!("config import failed: {error}"));
                None
            }
        }
    }

    pub(crate) fn import_global_config_toml_path(&mut self, path: &Utf8Path) -> bool {
        match import_global_config_toml(path) {
            Ok(message) => {
                if !self.reload_config() {
                    return false;
                }
                self.state.mark_startup_config_reviewed();
                self.state.set_status_message(message);
                true
            }
            Err(error) => {
                self.state
                    .set_status_message(format!("config import failed: {error}"));
                false
            }
        }
    }

    fn reload_config(&mut self) -> bool {
        match ConfigLoader::load(&self.app.workspace.root, None) {
            Ok(config) => {
                self.app.config = config.clone();
                self.reset_effective_config_without_network(config);
                true
            }
            Err(error) => {
                self.state
                    .set_status_message(format!("failed to reload config: {error}"));
                false
            }
        }
    }

    pub(crate) fn switch_workspace(&mut self) -> bool {
        if !self.ensure_navigation_admission("workspace") {
            return false;
        }
        let Some(requested) = self.resolve_workspace_input() else {
            return false;
        };
        self.invalidate_session_target_requests();
        let request_id = self.state.begin_workspace_load(requested.clone(), None);
        self.spawn_workspace_load(requested, request_id);
        true
    }

    pub(crate) fn show_workspace_picker(&mut self) {
        if !self.ensure_navigation_admission("workspace") {
            return;
        }
        let path = self.app.workspace.root.to_string();
        self.state.show_workspace_picker(&path);
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

    pub(crate) fn browse_workspace_dialog(&mut self) -> Option<Utf8PathBuf> {
        let start_dir = if self.state.workspace_input.trim().is_empty() {
            Some(self.app.workspace.root.clone())
        } else {
            self.resolve_workspace_input()
                .or_else(|| Some(self.app.workspace.root.clone()))
        };
        match pick_workspace_directory(start_dir.as_ref()) {
            Ok(Some(path)) => {
                self.state
                    .set_status_message(format!("selected workspace {}", path));
                Some(path)
            }
            Ok(None) => None,
            Err(error) => {
                self.state
                    .set_status_message(format!("workspace browse failed: {error}"));
                None
            }
        }
    }

    pub(crate) fn prepare_image_attachment_from_input(&mut self) -> Option<Utf8PathBuf> {
        let input = self
            .state
            .composer
            .image_attachment_input
            .trim()
            .to_string();
        match normalize_image_attachment_path(&self.app.workspace.cwd, &input) {
            Ok(path) => Some(path),
            Err(error) => {
                self.state
                    .set_status_message(format!("image attachment failed: {error}"));
                None
            }
        }
    }

    pub(crate) fn browse_image_dialog(&mut self) -> Option<Utf8PathBuf> {
        match pick_image_file(Some(&self.app.workspace.cwd)) {
            Ok(Some(path)) => {
                match normalize_image_attachment_path(&self.app.workspace.cwd, path.as_str()) {
                    Ok(path) => Some(path),
                    Err(error) => {
                        self.state
                            .set_status_message(format!("image attachment failed: {error}"));
                        None
                    }
                }
            }
            Ok(None) => None,
            Err(error) => {
                self.state
                    .set_status_message(format!("image browse failed: {error}"));
                None
            }
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

    pub(crate) fn open_typed_path_in_file_manager(&mut self) -> bool {
        if let Some(path) = self.resolve_workspace_input() {
            self.open_path_in_file_manager(&path)
        } else {
            false
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

    fn open_path_in_file_manager(&mut self, path: &camino::Utf8Path) -> bool {
        let mut command = if cfg!(target_os = "windows") {
            ProcessCommand::new("explorer")
        } else if cfg!(target_os = "macos") {
            ProcessCommand::new("open")
        } else {
            ProcessCommand::new("xdg-open")
        };
        match command.arg(path.as_str()).spawn() {
            Ok(_) => {
                self.state
                    .set_status_message(format!("opened {} in file manager", path));
                true
            }
            Err(error) => {
                self.state.set_status_message(format!(
                    "failed to open {} in file manager: {error}",
                    path
                ));
                false
            }
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

    fn stage_provider_config_for_persistence(
        &mut self,
        config: &ResolvedConfig,
    ) -> Result<(), String> {
        let metadata_mode = match config.model.provider_metadata_mode {
            ProviderMetadataMode::LmStudioNativeRequired => "lm_studio_native_required",
            ProviderMetadataMode::OpenAiCompatibleOnly => "openai_compatible_only",
        };
        self.state.set_config_values_by_key(vec![
            (
                ConfigField::BaseUrl.label().to_string(),
                config.model.base_url.clone(),
            ),
            (
                ConfigField::Model.label().to_string(),
                config.model.model.clone(),
            ),
            (
                ConfigField::ProviderMetadataMode.label().to_string(),
                metadata_mode.to_string(),
            ),
            (
                ConfigField::ContextWindow.label().to_string(),
                config.model.context_window.to_string(),
            ),
            (
                ConfigField::MaxOutputTokens.label().to_string(),
                config.model.max_output_tokens.to_string(),
            ),
            (
                ConfigField::SupportsTools.label().to_string(),
                config.model.supports_tools.to_string(),
            ),
            (
                ConfigField::SupportsReasoning.label().to_string(),
                config.model.supports_reasoning.to_string(),
            ),
            (
                ConfigField::SupportsImages.label().to_string(),
                config.model.supports_images.to_string(),
            ),
            (
                ConfigField::ParallelToolCalls.label().to_string(),
                config.model.parallel_tool_calls.to_string(),
            ),
            (
                ConfigField::MaxParallelPredictions.label().to_string(),
                config.model.max_parallel_predictions.to_string(),
            ),
            (
                ConfigField::ExtraBodyJson.label().to_string(),
                config
                    .model
                    .extra_body_json
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            ),
        ])
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

    pub(crate) fn answer_permission(&mut self, confirmation_id: u64, allow: bool) -> bool {
        match resolve_pending_permission(
            &mut self.pending_permission_request_id,
            &mut self.permission_response,
            confirmation_id,
            allow,
        ) {
            Ok(false) => {
                self.state.set_status_message(
                    "confirmation changed before the answer was applied; review the current request",
                );
                false
            }
            Ok(true) => {
                self.pending_permission_request = None;
                self.state.clear_permission();
                true
            }
            Err(error) => {
                self.pending_permission_request = None;
                self.state.clear_permission();
                self.state
                    .set_status_message(format!("failed to answer confirmation: {error}"));
                false
            }
        }
    }

    pub(crate) fn cancel_active_run(&mut self) {
        let mut requested = false;
        let session_id = self.state.app_state.current_session_id;
        let root_run_active = self.run_lifecycle.root_is_active();
        let sub_agent_active = self.current_agent_tree_active();
        if self.run_lifecycle.request_cancel() {
            requested = true;
        }
        if root_run_active || sub_agent_active {
            if let Some(session_id) = session_id {
                let _ = self.app.run_service.cancel_agent_tree(session_id);
                requested = true;
            }
        }
        if let Some(response) = self.permission_response.take() {
            let _ = response.send(false);
            self.pending_permission_request = None;
            self.pending_permission_request_id = None;
            self.state.clear_permission();
            requested = true;
        }
        if requested {
            self.durable_agent_activity_refresh_requests.clear();
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

    fn advance_composer_commit_generation(&mut self) {
        self.composer_commit_generation = self.composer_commit_generation.saturating_add(1);
    }

    fn commit_pending_root_submission(&mut self, run_generation: u64) -> bool {
        let Some(pending) = self
            .pending_root_submission
            .take_if(|pending| pending.run_generation == run_generation)
        else {
            return false;
        };
        self.state
            .push_local_prompt_dispatch(&pending.prompt_dispatch);
        let current_session_id = self.state.app_state.current_session_id;
        if pending.owner_workspace_path.as_str() == self.state.snapshot.workspace_path
            && pending.owner_session_id.is_none()
            && current_session_id.is_some()
        {
            self.state.adopt_composer_owner(current_session_id);
        } else {
            self.state.rebind_composer_owner(current_session_id);
        }
        self.state
            .composer
            .image_attachment_paths
            .retain(|path| !pending.image_paths.contains(path));
        self.state.composer.image_attachment_input.clear();
        if pending.cancel_prompt_review_on_commit {
            self.state.cancel_prompt_review();
        }
        self.advance_composer_commit_generation();
        true
    }

    fn discard_pending_root_submission(&mut self, run_generation: u64) {
        let _ = self
            .pending_root_submission
            .take_if(|pending| pending.run_generation == run_generation);
    }

    fn launch_run_with_options(
        &mut self,
        prompt: String,
        prompt_dispatch: crate::session::PromptDispatchPart,
        review_request: Option<ReviewRequest>,
        cancel_prompt_review_on_commit: bool,
    ) -> bool {
        if self.state.background_mutation_pending() {
            self.state
                .set_status_message("wait for the current owner mutation to finish before sending");
            return false;
        }
        if self.state.navigation_loading() {
            self.state
                .set_status_message("wait for navigation to finish before starting a run");
            return false;
        }
        if self.run_lifecycle.root_is_active() {
            if self.run_lifecycle.can_steer_root()
                && review_request.is_none()
                && !prompt.trim().is_empty()
            {
                let accepted = self.launch_active_turn_steer(prompt, prompt_dispatch);
                if accepted && cancel_prompt_review_on_commit {
                    self.state.cancel_prompt_review();
                }
                return accepted;
            } else {
                self.state.set_status_message(
                    "前回の停止処理を片付けています。状態が更新されてから再度実行してください。",
                );
            }
            return false;
        }
        if self.current_agent_tree_active() {
            self.state.set_status_message(
                "Sub Agentの完了または停止後に、新しい依頼を送信できます。".to_string(),
            );
            return false;
        }
        if review_request.is_none()
            && !prompt.trim().is_empty()
            && self.state.app_state.current_session_id.is_some()
            && matches!(
                self.state.app_state.run_status,
                crate::tui::state::RunStatus::Running
            )
        {
            let accepted = self.launch_active_turn_steer(prompt, prompt_dispatch);
            if accepted && cancel_prompt_review_on_commit {
                self.state.cancel_prompt_review();
            }
            return accepted;
        }
        if prompt.trim().is_empty() && review_request.is_none() {
            return false;
        }
        self.invalidate_session_search_requests();
        let run_generation = self.next_root_run_generation;
        let Some(next_generation) = run_generation.checked_add(1) else {
            self.state
                .set_status_message("desktop run generation is exhausted; restart moyAI");
            return false;
        };
        self.next_root_run_generation = next_generation;
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
            config_override: Some(desktop_run_config_override(
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
            agent_confirmation: None,
            agent_context: None,
        };
        self.run_lifecycle
            .begin(run_generation, cancel, live_config);
        self.pending_root_submission = Some(PendingRootSubmission {
            run_generation,
            owner_workspace_path: self.app.workspace.root.clone(),
            owner_session_id: request.session_id,
            prompt_dispatch,
            image_paths: self.state.composer.image_attachment_paths.clone(),
            cancel_prompt_review_on_commit,
        });
        let run_service = self.app.run_service.clone();
        let runtime_tx = self.runtime_tx.clone();
        let next_permission_request_id = self.next_permission_request_id.clone();
        let notification_title = request
            .title
            .clone()
            .unwrap_or_else(|| self.state.current_session_label());
        std::thread::spawn(move || {
            let mut request = request;
            let worker_cancel = request.cancel.clone();
            let mut renderer = DesktopRenderer {
                tx: runtime_tx.clone(),
                run_generation,
                notification_title: notification_title.clone(),
                notified_terminal: false,
            };
            let mut prompt = SharedConfirmationPrompt::new(DesktopConfirmationPrompt {
                tx: runtime_tx.clone(),
                next_permission_request_id,
            });
            request.agent_confirmation = Some(prompt.clone());
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop worker runtime");
            runtime.block_on(async move {
                let result = run_service
                    .execute(AppCommand::Run(request), &mut renderer, &mut prompt)
                    .await
                    .map_err(|error| error.to_string());
                match &result {
                    Ok(summary) if !renderer.notified_terminal => {
                        let notification_body =
                            run_completion_notification_body(&renderer.notification_title, summary);
                        send_windows_desktop_notification("moyAI", &notification_body);
                    }
                    Err(error)
                        if !renderer.notified_terminal
                            && desktop_run_failure_notification_allowed(
                                worker_cancel.is_cancelled(),
                            ) =>
                    {
                        let notification_body = run_error_notification_body(
                            &renderer.notification_title,
                            &crate::tui::state::RunStatus::Failed,
                            error,
                        );
                        send_windows_desktop_notification("moyAI", &notification_body);
                    }
                    _ => {}
                }
                publish_desktop_run_finished(&runtime_tx, run_generation, result);
            });
        });
        true
    }

    fn launch_active_turn_steer(
        &mut self,
        prompt: String,
        prompt_dispatch: crate::session::PromptDispatchPart,
    ) -> bool {
        let Some(session_id) = self.state.app_state.current_session_id else {
            self.state
                .set_status_message("実行中のセッションが見つからないため steer できません。");
            return false;
        };
        let image_paths = self.state.composer.image_attachment_paths.clone();
        self.state
            .set_status_message("実行中の turn に追加入力を保存しています。");
        let run_service = self.app.run_service.clone();
        let runtime_tx = self.runtime_tx.clone();
        let next_permission_request_id = self.next_permission_request_id.clone();
        let cwd = self.app.workspace.cwd.clone();
        let stored_image_paths = image_paths.clone();
        let result = std::thread::spawn(move || {
            let mut renderer = DesktopSteerRenderer;
            let mut prompt_ui = DesktopConfirmationPrompt {
                tx: runtime_tx.clone(),
                next_permission_request_id,
            };
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop steer runtime");
            runtime
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
                .map_err(|error| error.to_string())
        })
        .join()
        .map_err(|_| "desktop steer worker panicked".to_string())
        .and_then(|result| result);
        let accepted = finish_steer_submission(
            &mut self.state,
            &prompt_dispatch,
            &stored_image_paths,
            result,
        );
        if accepted {
            self.advance_composer_commit_generation();
        }
        accepted
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
        request_id: NavigationRequestId,
        session_id: SessionId,
        reason: SessionLoadReason,
        result: Result<LoadedSession, String>,
    ) {
        match result {
            Ok(loaded) => {
                if self.session_load_is_blocked_by_active_run() {
                    self.state.finish_navigation(request_id);
                    return;
                }
                if !self
                    .state
                    .is_current_session_navigation(request_id, session_id)
                {
                    self.state.finish_navigation(request_id);
                    return;
                }
                self.state.finish_navigation(request_id);
                let access_mode = loaded.session.access_mode;
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
                self.sync_loaded_session_access_mode(access_mode);
                if let Some(records) = loaded.agent_activity_records {
                    self.loaded_agent_activity_records = Some((loaded.session.id, records));
                    self.durable_agent_activity_refresh_failures = 0;
                }
                self.state.set_status_message(match reason {
                    SessionLoadReason::RunningRejoin => {
                        format!("rejoined running session {}", session_id)
                    }
                    SessionLoadReason::UserSelection => format!("opened session {}", session_id),
                });
            }
            Err(error) => {
                finish_navigation_failure(&mut self.state, request_id, error);
            }
        }
    }

    fn apply_current_session_refreshed_message(
        &mut self,
        session_id: SessionId,
        result: Result<LoadedSession, String>,
    ) {
        if self.session_load_is_blocked_by_active_run()
            || self.state.app_state.current_session_id != Some(session_id)
        {
            self.state.clear_post_run_refresh_pending();
            return;
        }
        match result {
            Ok(loaded) => {
                let access_mode = loaded.session.access_mode;
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
                self.sync_loaded_session_access_mode(access_mode);
                if let Some(records) = loaded.agent_activity_records {
                    self.loaded_agent_activity_records = Some((loaded.session.id, records));
                    self.durable_agent_activity_refresh_failures = 0;
                }
                self.state.clear_post_run_refresh_pending();
            }
            Err(error) => {
                self.state.clear_post_run_refresh_pending();
                self.state.set_status_message(error);
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
            self.spawn_current_session_refresh(session_id);
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
                        request_id,
                    );
                } else {
                    self.state.set_status_message(format!(
                        "workspace set to {}",
                        self.app.workspace.root
                    ));
                }
            }
            Err(error) => {
                finish_navigation_failure(&mut self.state, request_id, error);
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
                self.start_new_chat_with_global_access();
                self.state.set_status_message("new development chat ready");
            }
            Err(error) => {
                finish_navigation_failure(&mut self.state, request_id, error);
            }
        }
    }

    fn replace_workspace_from_load(&mut self, loaded: WorkspaceLoadResult) {
        let next_config_generation =
            next_config_generation(self.state.provider_config.config_generation);
        self.invalidate_session_target_requests();
        self.provider_catalog_requests.clear();
        self.startup_provider_requests.clear();
        self.startup_docling_requests.clear();
        self.app = loaded.app.clone();
        if !self.is_quick_chat_workspace() {
            self.preferences
                .unmark_project_deleted(&self.app.workspace.root);
        }
        self.session_search_requests.clear();
        self.state = DesktopState::new(loaded.snapshot, self.app.config.clone());
        self.state.provider_config.config_generation = next_config_generation;
        self.loaded_agent_activity_records = None;
        self.durable_agent_activity_refresh_failures = 0;
        self.state.workspace_input = self.app.workspace.cwd.to_string();
        if let Some(opacity) = self.preferences.window_opacity_percent {
            self.state.set_window_opacity_percent(opacity);
        }
        self.persist_preferences();
    }

    fn snapshot_target_is_current(&self, target: &SnapshotRequestTarget) -> bool {
        if self.app.workspace.root != target.workspace_root {
            return false;
        }
        let selected_session_id = self.state.selected_session_id();
        selected_session_id == target.selected_session_id
            || (selected_session_id.is_none()
                && self.state.app_state.current_session_id == target.selected_session_id)
    }

    fn session_page_target_is_current(&self, target: &SessionPageRequestTarget) -> bool {
        self.app.workspace.root == target.workspace_root
            && self.state.selected_session_id() == Some(target.session_id)
    }

    fn live_session_target_is_current(&self, target: &SessionRefreshRequestTarget) -> bool {
        self.app.workspace.root == target.workspace_root
            && self.state.app_state.current_session_id == Some(target.session_id)
    }

    fn provider_catalog_target_is_current(&self, target: &ProviderCatalogRequestTarget) -> bool {
        normalize_provider_base_url(&self.state.provider_config.provider_base_url_input)
            == target.base_url
            && self.state.provider_config.provider_metadata_mode_input == target.metadata_mode
            && self.state.provider_config.config_generation == target.config_generation
            && self.state.provider_config.provider_selected_model_id_input
                == target.selected_model_id
    }

    fn provider_readiness_target_is_current(
        &self,
        target: &ProviderReadinessRequestTarget,
    ) -> bool {
        let model = &self.state.provider_config.effective_config.model;
        normalize_provider_base_url(&model.base_url) == target.base_url
            && model.model == target.model_id
            && model.provider_metadata_mode == target.metadata_mode
            && model.supports_tools == target.supports_tools
            && model.supports_reasoning == target.supports_reasoning
            && model.supports_images == target.supports_images
            && model.parallel_tool_calls == target.parallel_tool_calls
            && model.max_parallel_predictions == target.max_parallel_predictions
            && self.state.provider_config.config_generation == target.config_generation
    }

    pub(crate) fn drain_runtime_messages(&mut self) -> bool {
        let mut changed = false;
        while let Ok(message) = self.runtime_rx.try_recv() {
            changed = true;
            let _contract = message.async_contract();
            match message {
                RuntimeMessage::RunEvent {
                    run_generation,
                    event,
                } => {
                    if !self.run_lifecycle.owns(run_generation) {
                        continue;
                    }
                    if matches!(
                        &event,
                        RunEvent::UserTurnStored { .. } | RunEvent::UserMessageStored { .. }
                    ) {
                        self.commit_pending_root_submission(run_generation);
                    }
                    if self.run_lifecycle.cancellation_requested()
                        && !run_event_is_terminal(&event)
                        && !matches!(&event, RunEvent::SessionStarted { .. })
                    {
                        continue;
                    }
                    let refresh_session_id = match &event {
                        RunEvent::SessionStarted { session_id, .. }
                        | RunEvent::SessionTitleUpdated { session_id, .. } => Some(*session_id),
                        _ => None,
                    };
                    if matches!(&event, RunEvent::SessionStarted { .. }) {
                        self.durable_agent_activity_refresh_failures = 0;
                    }
                    let live_refresh_session_id = event
                        .session_id()
                        .or(self.state.app_state.current_session_id);
                    if run_event_is_terminal(&event) {
                        self.run_lifecycle.observe_terminal_event();
                    }
                    self.state.apply_run_event(&event);
                    if let RunEvent::SessionStarted { session_id, .. } = &event {
                        self.resume_pending_access_mode_adoption(*session_id);
                    }
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
                            self.spawn_current_todos_refresh(session_id);
                        }
                    }
                    if let Some(session_id) = refresh_session_id {
                        self.spawn_snapshot_refresh_for_session(session_id);
                    }
                }
                RuntimeMessage::Finished {
                    run_generation,
                    result,
                } => {
                    if !self.run_lifecycle.owns(run_generation) {
                        continue;
                    }
                    if self.state.app_state.current_session_id.is_none() {
                        self.settle_pending_access_mode_without_session();
                    }
                    match result {
                        Ok(summary) => {
                            self.commit_pending_root_submission(run_generation);
                            let preserve_child_permission = preserve_permission_after_root_finish(
                                self.pending_permission_request.as_ref(),
                            );
                            self.run_lifecycle.finish_root();
                            if !preserve_child_permission {
                                self.pending_permission_request = None;
                                self.pending_permission_request_id = None;
                                self.permission_response = None;
                                self.state.clear_permission();
                            }
                            self.state.finish_agent_run();
                            self.state.mark_post_run_refresh_pending();
                            self.state.app_state.set_summary(summary);
                            self.refresh_current_session_after_terminal_run();
                        }
                        Err(error) => {
                            self.discard_pending_root_submission(run_generation);
                            let preserve_child_permission = preserve_permission_after_root_finish(
                                self.pending_permission_request.as_ref(),
                            );
                            self.run_lifecycle.finish_root();
                            if !preserve_child_permission {
                                self.pending_permission_request = None;
                                self.pending_permission_request_id = None;
                                self.permission_response = None;
                                self.state.clear_permission();
                            }
                            self.state.finish_agent_run();
                            if !matches!(
                                self.state.app_state.run_status,
                                crate::tui::state::RunStatus::Cancelled
                            ) {
                                self.state.app_state.run_status =
                                    crate::tui::state::RunStatus::Failed;
                            }
                            self.state.set_status_message(error);
                            if self.state.app_state.current_session_id.is_some() {
                                self.state.mark_post_run_refresh_pending();
                                self.refresh_current_session_after_terminal_run();
                            } else {
                                self.state.clear_post_run_refresh_pending();
                            }
                        }
                    }
                }
                RuntimeMessage::Permission {
                    confirmation_id,
                    request,
                    response,
                } => {
                    self.pending_permission_request = Some(request.clone());
                    self.pending_permission_request_id = Some(confirmation_id);
                    self.permission_response = Some(response);
                    self.state.set_permission(confirmation_id, &request);
                }
                RuntimeMessage::PermissionCancelled { confirmation_id } => {
                    if clear_cancelled_permission(
                        &mut self.pending_permission_request,
                        &mut self.pending_permission_request_id,
                        &mut self.permission_response,
                        confirmation_id,
                    ) {
                        self.state.clear_permission();
                    }
                }
                RuntimeMessage::EnhanceFinished {
                    request_id,
                    target,
                    result,
                } => {
                    if target.workspace_root != self.app.workspace.root
                        || target.session_id != self.state.app_state.current_session_id
                    {
                        self.state.fail_prompt_enhance(request_id);
                        continue;
                    }
                    match result {
                        Ok(draft) => {
                            if self.state.finish_prompt_enhance(request_id, draft) {
                                self.state.set_status_message("review enhanced draft");
                            }
                        }
                        Err(error) => {
                            if self.state.fail_prompt_enhance(request_id) {
                                self.state.set_status_message(format!(
                                    "prompt enhancement failed: {error}"
                                ));
                            }
                        }
                    }
                }
                RuntimeMessage::SnapshotLoaded {
                    request_id,
                    target,
                    result,
                } => {
                    if !self
                        .snapshot_requests
                        .finish_if_current(request_id, &target)
                    {
                        continue;
                    }
                    self.state.finish_snapshot_refresh();
                    if !self.snapshot_target_is_current(&target) {
                        continue;
                    }
                    match result {
                        Ok(snapshot) => self
                            .state
                            .replace_snapshot_preserving_current_owner(snapshot),
                        Err(error) => self.state.set_status_message(error),
                    }
                }
                RuntimeMessage::SessionLoaded {
                    request_id,
                    session_id,
                    reason,
                    result,
                } => self.apply_session_loaded_message(request_id, session_id, reason, result),
                RuntimeMessage::CurrentSessionRefreshed {
                    request_id,
                    target,
                    result,
                } => {
                    if !self
                        .current_session_refresh_requests
                        .finish_if_current(request_id, &target)
                        || !self.live_session_target_is_current(&target)
                    {
                        continue;
                    }
                    self.apply_current_session_refreshed_message(target.session_id, result);
                }
                RuntimeMessage::SessionDeleted { target, result } => {
                    if !finish_session_delete_request(
                        &mut self.state,
                        &target,
                        &self.app.workspace.root,
                        self.app.workspace.project_id,
                    ) {
                        continue;
                    }
                    let session_id = target.session_id;
                    match result {
                        Ok(snapshot) => {
                            let deleted_was_current =
                                self.state.app_state.current_session_id == Some(session_id);
                            if deleted_was_current {
                                self.state.replace_snapshot(snapshot);
                            } else {
                                self.state
                                    .replace_snapshot_preserving_current_owner(snapshot);
                            }
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
                                        request_id,
                                    );
                                } else {
                                    self.start_new_chat_with_global_access();
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
                    target,
                    archived,
                    result,
                } => {
                    if !session_mutation_target_matches(
                        &target,
                        &self.app.workspace.root,
                        self.app.workspace.project_id,
                    ) || !self
                        .state
                        .finish_session_archive_mutation(target.operation_id)
                    {
                        continue;
                    }
                    let session_id = target.session_id;
                    match result {
                        Ok(snapshot) => {
                            let archived_was_current = archived
                                && self.state.app_state.current_session_id == Some(session_id);
                            if archived_was_current {
                                self.state.replace_snapshot(snapshot);
                            } else {
                                self.state
                                    .replace_snapshot_preserving_current_owner(snapshot);
                            }
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
                                        request_id,
                                    );
                                } else {
                                    self.start_new_chat_with_global_access();
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
                RuntimeMessage::SessionRolledBack { target, result } => {
                    if !session_mutation_target_matches(
                        &target,
                        &self.app.workspace.root,
                        self.app.workspace.project_id,
                    ) || !self
                        .state
                        .finish_session_rollback_mutation(target.operation_id)
                    {
                        continue;
                    }
                    let session_id = target.session_id;
                    match result {
                        Ok(rolled_back) => {
                            self.state
                                .replace_snapshot_preserving_current_owner(rolled_back.snapshot);
                            if self.state.app_state.current_session_id == Some(session_id)
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
                                if let Some(records) = loaded.agent_activity_records {
                                    self.loaded_agent_activity_records =
                                        Some((loaded.session.id, records));
                                }
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
                RuntimeMessage::SessionOperationApplied { target, result } => {
                    if !session_mutation_target_matches(
                        &target,
                        &self.app.workspace.root,
                        self.app.workspace.project_id,
                    ) || !self
                        .state
                        .finish_session_maintenance_mutation(target.operation_id)
                    {
                        continue;
                    }
                    match result {
                        Ok(applied) => {
                            let session_id = applied.loaded.session.id;
                            self.state
                                .replace_snapshot_preserving_current_owner(applied.snapshot);
                            if self.state.app_state.current_session_id == Some(session_id)
                                && !self.session_load_is_blocked_by_active_run()
                            {
                                let loaded = applied.loaded;
                                let access_mode = loaded.session.access_mode;
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
                                self.sync_loaded_session_access_mode(access_mode);
                                if let Some(records) = loaded.agent_activity_records {
                                    self.loaded_agent_activity_records =
                                        Some((loaded.session.id, records));
                                }
                            }
                            self.state.set_status_message(applied.message);
                        }
                        Err(error) => self
                            .state
                            .set_status_message(format!("session operation failed: {error}")),
                    }
                }
                RuntimeMessage::SessionSearchLoaded { request_id, result } => {
                    let Some(completion) = self.session_search_requests.finish(request_id) else {
                        continue;
                    };
                    let _ = self.state.finish_session_search(completion.operation_id);
                    let root_run_active =
                        self.run_lifecycle.root_is_active() || self.state.is_busy();
                    let agent_tree_active = self.current_agent_tree_active();
                    if !apply_session_search_result(
                        &mut self.state,
                        completion.is_latest,
                        root_run_active,
                        agent_tree_active,
                        result,
                    ) {
                        continue;
                    }
                }
                RuntimeMessage::TurnPageLoaded {
                    request_id,
                    target,
                    result,
                } => {
                    if !self
                        .turn_page_requests
                        .finish_if_current(request_id, &target)
                    {
                        continue;
                    }
                    self.state.finish_turn_page_load();
                    if !self.session_page_target_is_current(&target)
                        || self.session_load_is_blocked_by_active_run()
                    {
                        continue;
                    }
                    match result {
                        Ok(loaded) => {
                            let access_mode = loaded.session.access_mode;
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
                            self.sync_loaded_session_access_mode(access_mode);
                            if let Some(records) = loaded.agent_activity_records {
                                self.loaded_agent_activity_records =
                                    Some((loaded.session.id, records));
                            }
                            self.state.set_status_message(format!(
                                "loaded turn page {}-{} of {}",
                                loaded.turn_page_offset.saturating_add(1),
                                loaded
                                    .turn_page_offset
                                    .saturating_add(loaded.turn_items.len()),
                                loaded.turn_page_total
                            ));
                        }
                        Err(error) => self
                            .state
                            .set_status_message(format!("turn page load failed: {error}")),
                    }
                }
                RuntimeMessage::LiveSessionRefreshed {
                    request_id,
                    target,
                    result,
                } => {
                    if !self
                        .live_session_refresh_requests
                        .finish_if_current(request_id, &target)
                        || !self.live_session_target_is_current(&target)
                    {
                        continue;
                    }
                    match result {
                        Ok(loaded) => {
                            if !self.session_load_is_blocked_by_active_run()
                                && loaded.turn_page_has_more
                            {
                                self.spawn_latest_live_session_refresh(target.session_id);
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
                            if let Some(records) = loaded.agent_activity_records {
                                self.loaded_agent_activity_records =
                                    Some((loaded.session.id, records));
                            }
                        }
                        Err(error) => self
                            .state
                            .set_status_message(format!("live session refresh failed: {error}")),
                    }
                }
                RuntimeMessage::DurableAgentActivityRefreshed {
                    request_id,
                    target,
                    result,
                } => {
                    if !finish_durable_agent_activity_refresh_request(
                        &mut self.durable_agent_activity_refresh_requests,
                        request_id,
                        &target,
                        &self.app.workspace.root,
                        self.state.app_state.current_session_id,
                    ) {
                        continue;
                    }
                    match result {
                        Ok(records) => {
                            self.loaded_agent_activity_records = Some((target.session_id, records));
                            self.durable_agent_activity_refresh_failures = 0;
                        }
                        Err(error) => {
                            self.durable_agent_activity_refresh_failures = self
                                .durable_agent_activity_refresh_failures
                                .saturating_add(1)
                                .min(3);
                            if self.durable_agent_activity_refresh_failures >= 3 {
                                self.state.set_status_message(format!(
                                    "Sub Agent activity refresh failed after 3 attempts: {error}"
                                ));
                            }
                        }
                    }
                }
                RuntimeMessage::ProjectDeleted { target, result } => {
                    if !project_delete_target_matches(
                        &target,
                        &self.app.workspace.root,
                        self.app.workspace.project_id,
                    ) || !self
                        .state
                        .finish_project_delete_mutation(target.operation_id)
                    {
                        continue;
                    }
                    let project_id = target.project_id;
                    let project_root = target.project_root;
                    match result {
                        Ok(loaded) => {
                            let deleted_was_current = self.app.workspace.project_id == project_id;
                            self.preferences.mark_project_deleted(&project_root);
                            if deleted_was_current {
                                self.app = loaded.app.clone();
                            }
                            if !self.is_quick_chat_workspace() {
                                self.preferences
                                    .unmark_project_deleted(&self.app.workspace.root);
                            }
                            if deleted_was_current {
                                let next_config_generation = next_config_generation(
                                    self.state.provider_config.config_generation,
                                );
                                self.session_search_requests.clear();
                                self.snapshot_requests.clear();
                                self.turn_page_requests.clear();
                                self.live_session_refresh_requests.clear();
                                self.current_session_refresh_requests.clear();
                                self.current_todo_refresh_requests.clear();
                                self.durable_agent_activity_refresh_requests.clear();
                                self.history_export_requests.clear();
                                self.provider_catalog_requests.clear();
                                self.startup_provider_requests.clear();
                                self.startup_docling_requests.clear();
                                self.state =
                                    DesktopState::new(loaded.snapshot, self.app.config.clone());
                                self.state.provider_config.config_generation =
                                    next_config_generation;
                                self.loaded_agent_activity_records = None;
                                self.durable_agent_activity_refresh_failures = 0;
                                self.state.workspace_input = self.app.workspace.cwd.to_string();
                                if let Some(opacity) = self.preferences.window_opacity_percent {
                                    self.state.set_window_opacity_percent(opacity);
                                }
                                self.persist_preferences();
                            } else {
                                self.state
                                    .replace_snapshot_preserving_current_owner(loaded.snapshot);
                                self.persist_preferences();
                            }
                            if deleted_was_current {
                                if let Some(next_session_id) = self.state.selected_session_id() {
                                    self.state.set_status_message(format!(
                                        "deleted project {}; opening {}...",
                                        project_id, next_session_id
                                    ));
                                    let request_id = self.state.begin_session_load(next_session_id);
                                    self.spawn_session_load(
                                        next_session_id,
                                        SessionLoadReason::UserSelection,
                                        request_id,
                                    );
                                } else {
                                    self.start_new_chat_with_global_access();
                                    self.state.set_status_message(format!(
                                        "deleted project {}",
                                        project_id
                                    ));
                                }
                            } else {
                                self.state
                                    .set_status_message(format!("deleted project {}", project_id));
                            }
                        }
                        Err(error) => self
                            .state
                            .set_status_message(format!("project delete failed: {error}")),
                    }
                }
                RuntimeMessage::CurrentTodosLoaded {
                    request_id,
                    target,
                    result,
                } => {
                    if !self
                        .current_todo_refresh_requests
                        .finish_if_current(request_id, &target)
                        || !self.live_session_target_is_current(&target)
                    {
                        continue;
                    }
                    match result {
                        Ok(todos) => {
                            self.state.finish_current_todo_refresh();
                            self.state.app_state.set_sidebar_todos(todos);
                        }
                        Err(error) => {
                            self.state.finish_current_todo_refresh();
                            self.state.set_status_message(error);
                        }
                    }
                }
                RuntimeMessage::ModelCatalogLoaded {
                    request_id,
                    target,
                    result,
                } => {
                    if !self
                        .provider_catalog_requests
                        .finish_if_current(request_id, &target)
                    {
                        continue;
                    }
                    if !self.provider_catalog_target_is_current(&target) {
                        self.state.cancel_provider_model_load();
                        continue;
                    }
                    match result {
                        Ok(models) => self.state.finish_provider_model_load(models),
                        Err(error) => self.state.fail_provider_model_load(error),
                    }
                }
                RuntimeMessage::StartupProviderChecked {
                    request_id,
                    target,
                    report,
                } => {
                    if !self
                        .startup_provider_requests
                        .finish_if_current(request_id, &target)
                        || !self.provider_readiness_target_is_current(&target)
                    {
                        continue;
                    }
                    self.state.finish_startup_provider_model_load(&report);
                }
                RuntimeMessage::StartupDoclingChecked {
                    request_id,
                    target,
                    result,
                } => {
                    if !self
                        .startup_docling_requests
                        .finish_if_current(request_id, &target)
                    {
                        continue;
                    }
                    let current = normalize_docling_base_url(
                        &self.state.provider_config.effective_config.docling.base_url,
                    );
                    if !self.state.provider_config.effective_config.docling.enabled
                        || current != target.base_url
                        || self.state.provider_config.config_generation != target.config_generation
                    {
                        continue;
                    }
                    match result {
                        Ok(()) => {
                            self.state.finish_startup_docling_check(&target.base_url);
                        }
                        Err(error) => {
                            self.state.fail_startup_docling_check(error.clone());
                            self.state.set_status_message(format!(
                                "Docling startup check failed: {error}"
                            ));
                        }
                    }
                }
                RuntimeMessage::HistoryExported {
                    request_id,
                    target,
                    result,
                } => {
                    let Some(target_is_current) = finish_history_export_request(
                        &mut self.history_export_requests,
                        request_id,
                        &target,
                        &self.app.workspace.root,
                    ) else {
                        continue;
                    };
                    self.state.finish_history_export();
                    if !target_is_current {
                        continue;
                    }
                    match result {
                        Ok(path) => self
                            .state
                            .set_status_message(format!("exported history markdown to {}", path)),
                        Err(error) => self
                            .state
                            .set_status_message(format!("history markdown export failed: {error}")),
                    }
                }
                RuntimeMessage::AccessModePersisted {
                    request_id,
                    target,
                    phase,
                    worker,
                    result,
                } => {
                    if !self
                        .access_mode_persistence_requests
                        .is_current(request_id, &target)
                        || !self
                            .state
                            .access_mode_persistence_is_current(target.operation_id)
                    {
                        continue;
                    }
                    let target_relation = self.access_mode_persistence_target_relation(&target);
                    if let (
                        AccessModePersistencePhase::InitialOwners,
                        Ok(path),
                        AccessModePersistenceTargetRelation::AdoptedSession(session_id),
                    ) = (&phase, &result, target_relation)
                    {
                        self.spawn_adopted_session_access_persistence(
                            request_id,
                            target.clone(),
                            session_id,
                            path.clone(),
                            worker,
                        );
                        self.state.set_status_message(
                            "global access mode saved; saving the adopted current root session",
                        );
                        continue;
                    }
                    if matches!(phase, AccessModePersistencePhase::InitialOwners)
                        && target_relation == AccessModePersistenceTargetRelation::Exact
                        && target.session_id.is_none()
                        && target.root_run_generation.is_some()
                        && target.root_run_generation == self.root_run_generation()
                    {
                        if let Ok(path) = &result {
                            self.pending_access_mode_adoption = Some(PendingAccessModeAdoption {
                                request_id,
                                target,
                                remembered_path: path.clone(),
                                worker,
                            });
                            self.state.set_status_message(
                                "global access mode saved; waiting for current root session admission",
                            );
                            continue;
                        }
                    }
                    let request_is_current = self
                        .access_mode_persistence_requests
                        .finish_if_current(request_id, &target);
                    let operation_is_current = self
                        .state
                        .finish_access_mode_persistence(target.operation_id);
                    if !request_is_current || !operation_is_current {
                        continue;
                    }
                    let (target_is_current, committed_session_id) = match (phase, target_relation) {
                        (
                            AccessModePersistencePhase::InitialOwners,
                            AccessModePersistenceTargetRelation::Exact,
                        ) => (true, target.session_id),
                        (
                            AccessModePersistencePhase::AdoptedSession { session_id },
                            AccessModePersistenceTargetRelation::AdoptedSession(current_session_id),
                        ) if session_id == current_session_id => (true, Some(session_id)),
                        _ => (false, None),
                    };
                    match result {
                        Ok(path) if target_is_current => {
                            self.app.config.permissions.access_mode = target.access_mode;
                            self.state
                                .provider_config
                                .update_access_mode(target.access_mode);
                            self.run_lifecycle.set_access_mode(target.access_mode);
                            if let Some(session_id) = committed_session_id {
                                for session in &mut self.state.app_state.sessions {
                                    if session.id == session_id {
                                        session.access_mode = target.access_mode;
                                    }
                                }
                                for summary in &mut self.state.app_state.loaded_sessions {
                                    if summary.session.id == session_id {
                                        summary.session.access_mode = target.access_mode;
                                    }
                                }
                            }
                            let auto_approved = self
                                .pending_permission_request
                                .as_ref()
                                .is_some_and(|request| {
                                    crate::tool::context::access_mode_allows_permission(
                                        target.access_mode,
                                        request,
                                    )
                                });
                            if auto_approved {
                                if let Some(response) = self.permission_response.take() {
                                    let _ = response.send(true);
                                }
                                self.pending_permission_request = None;
                                self.pending_permission_request_id = None;
                                self.state.clear_permission();
                            }
                            let scope = if committed_session_id.is_some() {
                                "global config and current root session"
                            } else {
                                "global config"
                            };
                            let suffix = if auto_approved {
                                "; pending confirmation approved"
                            } else {
                                ""
                            };
                            self.state.set_status_message(format!(
                                "{scope} access mode set to {} and remembered in {}{suffix}",
                                target.access_mode.label(),
                                path
                            ));
                        }
                        Ok(_) => {
                            let _ = self.reload_config();
                            self.state.set_status_message(
                                "access mode was persisted for its original owner; current configuration was reloaded",
                            );
                        }
                        Err(error) => {
                            let _ = self.reload_config();
                            if target_is_current
                                && self.state.app_state.current_session_id.is_some()
                            {
                                self.state
                                    .provider_config
                                    .update_access_mode(target.old_effective_access_mode);
                                self.run_lifecycle
                                    .set_access_mode(target.old_effective_access_mode);
                            }
                            self.state.set_status_message(format!(
                                "access mode was not changed; configuration was reloaded: {error}"
                            ));
                        }
                    }
                }
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

fn desktop_run_failure_notification_allowed(cancelled: bool) -> bool {
    !cancelled
}

fn publish_desktop_run_finished(
    tx: &tokio::sync::mpsc::UnboundedSender<RuntimeMessage>,
    run_generation: u64,
    result: Result<RunSummary, String>,
) {
    let _ = tx.send(RuntimeMessage::Finished {
        run_generation,
        result,
    });
}

fn resolve_pending_permission(
    pending_confirmation_id: &mut Option<u64>,
    response: &mut Option<mpsc::Sender<bool>>,
    expected_confirmation_id: u64,
    allow: bool,
) -> Result<bool, String> {
    if *pending_confirmation_id != Some(expected_confirmation_id) {
        return Ok(false);
    }
    *pending_confirmation_id = None;
    let response = response
        .take()
        .ok_or_else(|| "confirmation response channel is unavailable".to_string())?;
    response.send(allow).map_err(|error| error.to_string())?;
    Ok(true)
}

fn clear_cancelled_permission(
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

fn preserve_permission_after_root_finish(request: Option<&PermissionRequest>) -> bool {
    request.is_some_and(|request| request.agent_path.is_some())
}

fn loaded_session_from_detail(
    detail: LoadedSessionDetail,
    agent_activity_records: Option<Vec<AgentActivityRecord>>,
) -> LoadedSession {
    LoadedSession {
        session: detail.session,
        transcript: detail.transcript,
        turn_items: detail.turn_items,
        state: detail.state,
        todos: detail.todos,
        turn_page_offset: detail.turn_page_offset,
        turn_page_limit: detail.turn_page_limit,
        turn_page_total: detail.turn_page_total,
        turn_page_has_more: detail.turn_page_has_more,
        agent_activity_records,
    }
}

async fn loaded_session_from_detail_with_activity(
    app: &App,
    detail: LoadedSessionDetail,
) -> Result<LoadedSession, String> {
    let session_id = detail.session.id;
    let agent_activity_records = app
        .run_service
        .durable_agent_activity_records(session_id)
        .await
        .map_err(|error| error.to_string())?;
    Ok(loaded_session_from_detail(
        detail,
        Some(agent_activity_records),
    ))
}

async fn load_session_operation_projection(
    app: &App,
    session_id: SessionId,
    message: String,
) -> Result<DesktopSessionOperationLoaded, String> {
    let snapshot = load_snapshot_for_selection(app, Some(session_id))
        .await
        .map_err(|error| error.to_string())?;
    let detail = load_session_detail(app, session_id)
        .await
        .map_err(|error| error.to_string())?;
    let agent_activity_records = app
        .run_service
        .durable_agent_activity_records(session_id)
        .await
        .map_err(|error| error.to_string())?;
    Ok(DesktopSessionOperationLoaded {
        snapshot,
        loaded: LoadedSession {
            session: detail.session,
            transcript: detail.transcript,
            turn_items: detail.turn_items,
            state: detail.state,
            todos: detail.todos,
            turn_page_offset: detail.turn_page_offset,
            turn_page_limit: detail.turn_page_limit,
            turn_page_total: detail.turn_page_total,
            turn_page_has_more: detail.turn_page_has_more,
            agent_activity_records: Some(agent_activity_records),
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
        DesktopRenderer, DoclingRequestTarget, HistoryExportRequestTarget,
        ProviderCatalogRequestTarget, ProviderReadinessRequestTarget, RuntimeMessage,
        RuntimeMessageAsyncContract, SessionRefreshRequestTarget,
        fallback_workspace_after_project_delete, first_restorable_project_root,
        normalize_image_attachment_path, notification_session_title,
        open_transcript_rows_to_markdown, provider_catalog_probe_config,
        publish_desktop_run_finished, resolve_pending_permission, run_completion_notification_body,
        run_terminal_event_notification_body, transcript_markdown_file_name,
        unique_background_request_admission_open,
    };
    use crate::cli::EventRenderer as _;
    use crate::config::{ProviderMetadataMode, ResolvedConfig};
    use crate::desktop::async_ops::LatestRequestTracker;
    use crate::desktop::models::DesktopTranscriptRowKind;
    use crate::desktop::models::{DesktopFileChangeRow, DesktopTranscriptRow};
    use crate::llm::{ModelAvailabilityReport, ModelAvailabilityStatus};
    use crate::session::{ProjectId, ProjectRecord, RunEvent, RunSummary, SessionStatus};
    use camino::{Utf8Path, Utf8PathBuf};
    use std::sync::mpsc;

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
    fn unique_background_request_admission_rejects_either_pending_owner() {
        assert!(unique_background_request_admission_open(false, false));
        assert!(!unique_background_request_admission_open(true, false));
        assert!(!unique_background_request_admission_open(false, true));
        assert!(!unique_background_request_admission_open(true, true));
    }

    #[test]
    fn runtime_message_async_contract_classifies_representative_backflow_sources() {
        let provider_target = ProviderCatalogRequestTarget {
            base_url: "http://127.0.0.1:1234".to_string(),
            metadata_mode: ProviderMetadataMode::LmStudioNativeRequired,
            config_generation: 1,
            selected_model_id: "qwen/qwen3.6-35b-a3b".to_string(),
        };
        let provider_request_id = LatestRequestTracker::default().begin(provider_target.clone());
        let history_target = HistoryExportRequestTarget {
            workspace_root: Utf8PathBuf::from("C:/workspace"),
            session_id: crate::session::SessionId::new(),
        };
        let history_request_id = LatestRequestTracker::default().begin(history_target.clone());
        assert_eq!(
            RuntimeMessage::HistoryExported {
                request_id: history_request_id,
                target: history_target,
                result: Ok(Utf8PathBuf::from("C:/workspace/history.md")),
            }
            .async_contract(),
            RuntimeMessageAsyncContract::BackgroundOperation
        );
        assert_eq!(
            RuntimeMessage::ModelCatalogLoaded {
                request_id: provider_request_id,
                target: provider_target,
                result: Ok(Vec::new()),
            }
            .async_contract(),
            RuntimeMessageAsyncContract::ProviderOperation
        );
        let readiness_target = ProviderReadinessRequestTarget {
            base_url: "http://127.0.0.1:1234".to_string(),
            model_id: "qwen/qwen3.6-35b-a3b".to_string(),
            metadata_mode: ProviderMetadataMode::LmStudioNativeRequired,
            supports_tools: true,
            supports_reasoning: false,
            supports_images: false,
            parallel_tool_calls: true,
            max_parallel_predictions: 1,
            config_generation: 1,
        };
        let readiness_request_id = LatestRequestTracker::default().begin(readiness_target.clone());
        assert_eq!(
            RuntimeMessage::StartupProviderChecked {
                request_id: readiness_request_id,
                target: readiness_target,
                report: ModelAvailabilityReport {
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
            }
            .async_contract(),
            RuntimeMessageAsyncContract::ProviderOperation
        );
        let docling_target = DoclingRequestTarget {
            base_url: "http://127.0.0.1:8123".to_string(),
            config_generation: 1,
        };
        let docling_request_id = LatestRequestTracker::default().begin(docling_target.clone());
        assert_eq!(
            RuntimeMessage::StartupDoclingChecked {
                request_id: docling_request_id,
                target: docling_target,
                result: Ok(()),
            }
            .async_contract(),
            RuntimeMessageAsyncContract::ProviderOperation
        );
        assert_eq!(
            {
                let target = SessionRefreshRequestTarget {
                    workspace_root: Utf8PathBuf::from("C:/workspace"),
                    session_id: crate::session::SessionId::new(),
                };
                let request_id = LatestRequestTracker::default().begin(target.clone());
                RuntimeMessage::LiveSessionRefreshed {
                    request_id,
                    target,
                    result: Err("not loaded".to_string()),
                }
            }
            .async_contract(),
            RuntimeMessageAsyncContract::RunStream
        );
        assert_eq!(
            RuntimeMessage::Finished {
                run_generation: 1,
                result: Err("failed".to_string()),
            }
            .async_contract(),
            RuntimeMessageAsyncContract::TerminalRun
        );
    }

    #[test]
    fn stale_permission_answer_id_is_rejected_without_consuming_current_request() {
        let (response_tx, response_rx) = mpsc::channel();
        let mut pending_confirmation_id = Some(42);
        let mut response = Some(response_tx);

        assert_eq!(
            resolve_pending_permission(&mut pending_confirmation_id, &mut response, 41, true,),
            Ok(false)
        );
        assert_eq!(pending_confirmation_id, Some(42));
        assert!(response.is_some());
        assert!(matches!(
            response_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        assert_eq!(
            resolve_pending_permission(&mut pending_confirmation_id, &mut response, 42, false,),
            Ok(true)
        );
        assert_eq!(
            response_rx.recv().expect("current decision response"),
            false
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
    fn image_attachment_normalization_allows_canonical_file_outside_workspace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let image = temp.path().join("outside.png");
        std::fs::write(&image, b"png fixture").expect("image fixture");
        let workspace = Utf8PathBuf::from_path_buf(workspace).expect("utf8 workspace");
        let image = Utf8PathBuf::from_path_buf(image).expect("utf8 image");

        let normalized = normalize_image_attachment_path(&workspace, image.as_str())
            .expect("outside image should be explicitly attachable");

        assert!(normalized.is_absolute());
        assert_eq!(normalized.extension(), Some("png"));
        assert!(normalized.is_file());
    }

    #[test]
    fn image_attachment_normalization_rejects_parent_traversal() {
        let error =
            normalize_image_attachment_path(Utf8Path::new("C:/workspace"), "../outside.png")
                .expect_err("parent traversal must be rejected before asset scoping");

        assert!(error.contains("parent-directory traversal"));
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
    fn desktop_renderer_defers_state_completion_until_worker_settlement() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = DesktopRenderer {
            tx: tx.clone(),
            run_generation: 12,
            notification_title: "test".to_string(),
            notified_terminal: false,
        };
        let summary = RunSummary {
            session_id: crate::session::SessionId::new(),
            assistant_message_id: None,
            status: SessionStatus::Completed,
            finish_reason: None,
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

        publish_desktop_run_finished(&tx, 12, Ok(summary.clone()));
        assert!(matches!(
            rx.try_recv().expect("worker settlement"),
            RuntimeMessage::Finished {
                run_generation: 12,
                result: Ok(received),
            } if received.session_id == summary.session_id
        ));
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
    run_generation: u64,
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
            .send(RuntimeMessage::RunEvent {
                run_generation: self.run_generation,
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
    next_permission_request_id: Arc<AtomicU64>,
}

impl ConfirmationPrompt for DesktopConfirmationPrompt {
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
            match response_rx.recv_timeout(std::time::Duration::from_millis(25)) {
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
                        "desktop permission response channel disconnected".to_string(),
                    ));
                }
            }
        }
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

fn normalize_image_attachment_path(base: &Utf8Path, input: &str) -> Result<Utf8PathBuf, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("enter an image path before attaching".to_string());
    }
    if Path::new(trimmed)
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err("parent-directory traversal is not allowed in image paths".to_string());
    }
    let requested = Utf8Path::new(trimmed);
    let normalized = normalize_path(base, requested).map_err(|error| error.to_string())?;
    let metadata = std::fs::metadata(normalized.as_std_path())
        .map_err(|error| format!("image path is not accessible: {error}"))?;
    if !metadata.is_file() {
        return Err("image path is not a file".to_string());
    }
    let canonical = std::fs::canonicalize(normalized.as_std_path())
        .map_err(|error| format!("image path could not be canonicalized: {error}"))?;
    let canonical = Utf8PathBuf::from_path_buf(canonical)
        .map_err(|_| "image path is not valid UTF-8".to_string())?;
    let extension = canonical
        .extension()
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| "image file extension is missing".to_string())?;
    if !matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif") {
        return Err(format!("unsupported image file extension: {extension}"));
    }
    Ok(canonical)
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
    let parent = target
        .parent()
        .ok_or_else(|| format!("global config path has no parent: {target}"))?;
    fs::create_dir_all(parent.as_std_path()).map_err(|error| error.to_string())?;
    let mut temp =
        NamedTempFile::new_in(parent.as_std_path()).map_err(|error| error.to_string())?;
    temp.write_all(text.as_bytes())
        .map_err(|error| error.to_string())?;
    temp.as_file_mut()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temp.persist(target.as_std_path())
        .map_err(|error| error.error.to_string())?;
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

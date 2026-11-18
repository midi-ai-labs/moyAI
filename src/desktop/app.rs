use std::collections::{BTreeSet, HashMap};
mod access_mode_persistence;
mod config_import;
mod file_manager;
mod history_export;
mod run_lifecycle;
mod session_loading;
mod session_operations;
mod session_search;
mod side_chat_commands;
mod workspace_switch;
use std::io::Write;
use std::path::{Component, Path};
use std::process::Command as ProcessCommand;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use crate::user_data::internal_desktop_project_roots;
use camino::{Utf8Path, Utf8PathBuf};

use crate::app::session_title::NEW_SESSION_PLACEHOLDER_TITLE;
use crate::app::{
    AgentActivityRecord, App, AppBootstrap, AppCommand, AppCommandOutcome,
    ExactRootExecutionStopOutcome, ReviewRequest, RunConfigInput, RunRequest, SessionSteerRequest,
};
use crate::cli::{
    ConfirmationOutcome, ConfirmationPrompt, EventRenderer, OutputMode, ReviewDecision,
    SharedConfirmationPrompt,
};
use crate::config::field::build_resolved_config_from_key_values;
use crate::config::loader::{
    acquire_global_config_write_lease, global_config_path, read_toml_utf8_bounded,
};
use crate::config::merge::apply_patch as apply_config_patch;
use crate::config::model::{PartialModelConfig, PartialResolvedConfig};
use crate::config::{
    ConfigField, ConfigLoader, ProviderProfile, ResolvedConfig, ShellFamily,
    sanitize_provider_endpoint,
};
use crate::error::{AppRunError, CliPromptError, CliRenderError, SessionError, StorageError};
use crate::llm::{
    ProviderModelInfo, apply_provider_model_info_to_config, fetch_provider_model_infos,
    normalize_provider_base_url,
};
#[cfg(test)]
use crate::protocol::TurnInterruptionCause;
use crate::protocol::{
    ProtocolEventStore as _, ProtocolPageRequest, RuntimeEvent, RuntimeEventMsg,
    ToolApprovalDecision, TurnId, UserInputItem, UserTurn,
};
use crate::runtime::{
    AgentStatus, LocalTaskExecutor, OwnedTaskHandle, RunCancelOutcome, RunCancellationCause,
    RunControl, SystemClock,
};
use crate::session::markdown::{
    MarkdownExportEvent, MarkdownMetadataLine, MarkdownTerminalStatus,
    canonical_markdown_export_read, render_codex_turn_block_markdown,
};
use crate::session::{
    ActiveTurnExpectation, EditorContext, LoadedSessionStatus, ProjectId, ProjectRecord, RunEvent,
    RunEventDurability, RunSummary, SessionId, SessionRecord, SessionSettingsPatch, SessionStatus,
    ThreadGoalClearResult, ThreadGoalGetResult, ThreadGoalSetResult,
    canonical_session_read_to_markdown, history_markdown_file_name,
};
use crate::storage::{SideChatBinding, SideChatId, SideChatProviderTarget, SideChatRouteKind};
use crate::tool::PermissionRequest;
use crate::tui::config_editor::{ConfigEditorState, GlobalConfigAdoptionPolicy};
use crate::workspace::project::normalize_path;
use tauri::Manager;
use tempfile::NamedTempFile;
use tokio_util::sync::CancellationToken;

use super::args::{DesktopArgs, quick_chat_workspace_directory};
use super::async_ops::{
    DesktopAsyncOperationId, LatestRequestId, LatestRequestTracker, SessionSearchDispatch,
    SessionSearchRequestId, SessionSearchRequestTracker,
};
use super::models::{DesktopSnapshot, DesktopTranscriptRow, DesktopTranscriptRowKind};
use super::navigation::NavigationRequestId;
use super::open_session::OpenSessionView;
use super::preferences::{DesktopOnboardingIntent, DesktopPreferences};
use super::query::{
    DESKTOP_HISTORY_PROJECTION_LIMIT, DESKTOP_TURN_PAGE_LIMIT, LoadedSessionDetail,
    load_latest_session_detail, load_session_detail, load_snapshot, load_snapshot_continue_last,
    load_snapshot_for_selection, load_snapshot_for_session_search,
};
use super::side_chat::{
    SideChatContextMetadata, SideChatQuoteRequest, SideChatRequestProfile, SideChatStreamEvent,
    decode_persisted_side_chat_draft, encode_persisted_side_chat_draft,
    execute_admitted_canonical_side_chat, prepare_side_chat_input, side_chat_system_prompt,
};
use super::state::{DesktopState, DesktopStatusCode};
#[cfg(test)]
use super::web_model::desktop_web_state;
use super::web_model::{
    DesktopRuntimeProjection, DesktopSideChatDraftQuoteProjection,
    DesktopSideChatMessageProjection, DesktopSideChatProjection, DesktopWebState,
    access_runtime_owner_terminal_settlement_matches, access_runtime_owner_token,
    agent_activity_projection, desktop_web_state_with_permission, navigation_admission_blocker,
};

const DESKTOP_RUNTIME_DRAIN_BUDGET: usize = 256;
const DESKTOP_RUNTIME_MAILBOX_CAPACITY: usize = 512;
const SIDE_CHAT_START_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const DESKTOP_SESSION_RUNTIME_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(100);
const DESKTOP_SESSION_RUNTIME_CURSOR_PAGE_LIMIT: usize = 1;
const DESKTOP_SESSION_RUNTIME_CURSOR_DRAIN_BUDGET: usize = 64;

enum RuntimeMessage {
    RunEvent {
        run_generation: u64,
        event: RunEvent,
    },
    CanonicalSessionEvent {
        listener_generation: u64,
        target: SessionRuntimeListenerTarget,
        event: RuntimeEvent,
    },
    Finished {
        run_generation: u64,
        result: Result<RunSummary, String>,
    },
    ControlFinished {
        run_generation: u64,
        result: DesktopControlResult,
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
    EnhanceFinished {
        request_id: u64,
        target: DraftRequestTarget,
        result: Result<String, String>,
    },
    SteerFinished {
        target: SteerSubmissionTarget,
        image_paths: Vec<Utf8PathBuf>,
        result: Result<(), String>,
    },
    SnapshotLoaded {
        request_id: LatestRequestId,
        target: SnapshotRequestTarget,
        result: Result<super::models::DesktopSnapshot, String>,
    },
    SessionLoaded {
        request_id: NavigationRequestId,
        target: SessionLoadRequestTarget,
        reason: SessionLoadReason,
        result: Result<SessionNavigationLoadResult, String>,
    },
    CurrentSessionRefreshed {
        request_id: LatestRequestId,
        target: SessionRefreshRequestTarget,
        purpose: CurrentSessionRefreshPurpose,
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
    ModelCatalogLoaded {
        request_id: LatestRequestId,
        target: ProviderCatalogRequestTarget,
        result: Result<Vec<ProviderModelInfo>, String>,
    },
    DoclingReadinessChecked {
        request_id: LatestRequestId,
        target: DoclingReadinessRequestTarget,
        result: Result<crate::docling::DoclingReadinessResult, String>,
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
        requested: Utf8PathBuf,
        result: Result<WorkspaceLoadResult, String>,
    },
    AccessModePersisted {
        request_id: LatestRequestId,
        target: AccessModePersistenceTarget,
        phase: AccessModePersistencePhase,
        worker: Arc<AccessModePersistenceWorker>,
        result: Result<AccessModePersistenceCommit, String>,
    },
    SideChatDelta {
        owner_session_id: SessionId,
        side_chat_id: String,
        run_generation: u64,
        delta: String,
    },
    SideChatPhase {
        owner_session_id: SessionId,
        side_chat_id: String,
        run_generation: u64,
        phase: String,
    },
    SideChatFinished {
        owner_session_id: SessionId,
        side_chat_id: String,
        run_generation: u64,
        result: Result<(), String>,
    },
}

#[derive(Clone)]
struct DesktopControlPlaneSender {
    tx: mpsc::Sender<RuntimeMessage>,
}

impl DesktopControlPlaneSender {
    fn send(&self, message: RuntimeMessage) -> Result<(), String> {
        self.tx
            .send(message)
            .map_err(|_| "desktop control mailbox is unavailable".to_string())
    }
}

#[cfg(test)]
fn test_desktop_control_plane() -> (DesktopControlPlaneSender, mpsc::Receiver<RuntimeMessage>) {
    let (tx, rx) = mpsc::channel();
    (DesktopControlPlaneSender { tx }, rx)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SnapshotRequestTarget {
    workspace_root: Utf8PathBuf,
    selected_session_id: Option<SessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionLoadRequestTarget {
    workspace_root: Utf8PathBuf,
    workspace_cwd: Utf8PathBuf,
    project_id: ProjectId,
    session_id: SessionId,
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

#[derive(Debug, Clone)]
struct AccessModePersistenceCommit {
    remembered_path: Utf8PathBuf,
    session: Option<SessionRecord>,
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
type PersistRootSessionAccessMode = Box<
    dyn FnOnce(SessionId, crate::config::AccessMode) -> Result<Option<SessionRecord>, String>
        + Send,
>;

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
        PersistSession: FnOnce(SessionId, crate::config::AccessMode) -> Result<Option<SessionRecord>, String>
            + Send
            + 'static,
    {
        Self {
            compare_and_set_global: Mutex::new(Box::new(compare_and_set_global)),
            persist_session: Mutex::new(Some(Box::new(persist_session))),
        }
    }

    fn persist_initial_owners(
        &self,
        target: &AccessModePersistenceTarget,
    ) -> Result<AccessModePersistenceCommit, String> {
        persist_desktop_access_mode_owners_canonical(
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
    ) -> Result<AccessModePersistenceCommit, String> {
        let session = match self.persist_session(session_id, target.access_mode) {
            Ok(session) => session,
            Err(session_error) => {
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
        };
        Ok(AccessModePersistenceCommit {
            remembered_path,
            session,
        })
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
    ) -> Result<Option<SessionRecord>, String> {
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
    owner_generation: u64,
    expected_active_turn: ActiveTurnExpectation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PromptReviewTarget {
    request_id: u64,
    workspace_root: Utf8PathBuf,
    composer_workspace_path: String,
    composer_session_id: Option<SessionId>,
    composer_owner_generation: u64,
    expected_active_turn: ActiveTurnExpectation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SteerSubmissionTarget {
    operation_id: DesktopAsyncOperationId,
    workspace_root: Utf8PathBuf,
    session_id: SessionId,
    expected_active_turn: ActiveTurnExpectation,
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
struct SessionRuntimeListenerTarget {
    workspace_root: Utf8PathBuf,
    session_id: SessionId,
    turn_id: TurnId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionSearchRequestTarget {
    query: String,
    include_archived: bool,
    selected_session_id: Option<SessionId>,
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
    workspace_authority_root: Utf8PathBuf,
    session_id: SessionId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderCatalogRequestTarget {
    base_url: String,
    profile: ProviderProfile,
    api_key_env: Option<String>,
    config_generation: u64,
    selected_model_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DoclingReadinessRequestTarget {
    owner: DoclingReadinessConfigOwner,
    base_url: String,
    config_generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoclingReadinessConfigOwner {
    EffectiveConfig,
    InitialSetupDraft,
}

#[derive(Debug)]
pub(crate) enum RootSessionSettingsApplyError {
    ActiveTree(String),
    Internal(String),
}

impl RootSessionSettingsApplyError {
    fn from_session_error(error: SessionError) -> Self {
        match error {
            matched @ SessionError::Storage(StorageError::SessionSettingsActiveTree { .. }) => {
                Self::ActiveTree(matched.to_string())
            }
            other => Self::Internal(other.to_string()),
        }
    }
}

impl std::fmt::Display for RootSessionSettingsApplyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ActiveTree(message) | Self::Internal(message) => formatter.write_str(message),
        }
    }
}

pub(crate) struct RootSessionSettingsPersistence {
    app: App,
    session_id: SessionId,
    expected_settings_revision: u64,
    current_access_mode: crate::config::AccessMode,
    patch: SessionSettingsPatch,
    access_only: bool,
    config_generation_delta: u64,
}

pub(crate) struct RootSessionSettingsPersistenceResult {
    pub(crate) update: crate::session::SessionSettingsUpdate,
    pub(crate) access_only: bool,
    pub(crate) config_generation_delta: u64,
}

pub(crate) enum RootSessionSettingsPersistenceOutcome {
    Applied(RootSessionSettingsPersistenceResult),
    Conflict { session: SessionRecord },
}

impl RootSessionSettingsPersistenceOutcome {
    pub(crate) fn canonical_session(&self) -> &SessionRecord {
        match self {
            Self::Applied(result) => &result.update.session,
            Self::Conflict { session, .. } => session,
        }
    }
}

impl RootSessionSettingsPersistence {
    pub(crate) fn access_only(&self) -> bool {
        self.access_only
    }

    pub(crate) fn execute_blocking(
        self,
    ) -> Result<RootSessionSettingsPersistenceOutcome, RootSessionSettingsApplyError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| RootSessionSettingsApplyError::Internal(error.to_string()))?;
        let access_only = self.access_only;
        let config_generation_delta = self.config_generation_delta;
        runtime.block_on(async move {
            let service = self.app.session_service.clone();
            let update = if access_only {
                service
                    .compare_and_set_root_session_access_mode_at_revision(
                        self.session_id,
                        self.expected_settings_revision,
                        self.current_access_mode,
                        self.patch
                            .access_mode
                            .expect("access-only patch retains the next mode"),
                    )
                    .await
            } else {
                service
                    .compare_and_set_root_session_settings(
                        self.session_id,
                        self.expected_settings_revision,
                        self.patch,
                    )
                    .await
            }
            .map_err(RootSessionSettingsApplyError::from_session_error)?;
            match update {
                Some(update) => Ok(RootSessionSettingsPersistenceOutcome::Applied(
                    RootSessionSettingsPersistenceResult {
                        update,
                        access_only,
                        config_generation_delta,
                    },
                )),
                None => service
                    .get_session(self.session_id)
                    .await
                    .map(|session| RootSessionSettingsPersistenceOutcome::Conflict { session })
                    .map_err(RootSessionSettingsApplyError::from_session_error),
            }
        })
    }
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
            RuntimeMessage::RunEvent { .. } | RuntimeMessage::CanonicalSessionEvent { .. } => {
                RuntimeMessageAsyncContract::RunStream
            }
            RuntimeMessage::Finished { .. } | RuntimeMessage::ControlFinished { .. } => {
                RuntimeMessageAsyncContract::TerminalRun
            }
            RuntimeMessage::Permission { .. } | RuntimeMessage::PermissionCancelled { .. } => {
                RuntimeMessageAsyncContract::ModalDecision
            }
            RuntimeMessage::EnhanceFinished { .. } | RuntimeMessage::SteerFinished { .. } => {
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
            RuntimeMessage::ModelCatalogLoaded { .. } => {
                RuntimeMessageAsyncContract::ProviderOperation
            }
            RuntimeMessage::DoclingReadinessChecked { .. } => {
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
            RuntimeMessage::SideChatDelta { .. } | RuntimeMessage::SideChatPhase { .. } => {
                RuntimeMessageAsyncContract::RunStream
            }
            RuntimeMessage::SideChatFinished { .. } => RuntimeMessageAsyncContract::TerminalRun,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionLoadReason {
    UserSelection,
    CreatedEditFork,
    RunningRejoin,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CurrentSessionRefreshPurpose {
    Refresh,
    StopRequestRefresh {
        root_admission_fence: u64,
        root_stop_attempt: Option<DesktopRootStopAttempt>,
        expected_active_turn: Option<ActiveTurnExpectation>,
        in_memory_stop_accepted: bool,
        durable_outcome: Option<ExactRootExecutionStopOutcome>,
    },
}

struct LoadedSession {
    read: crate::session::CanonicalSessionRead,
    agent_activity_records: Option<Vec<AgentActivityRecord>>,
}

struct SessionNavigationLoadResult {
    workspace: Option<WorkspaceLoadResult>,
    snapshot: Option<DesktopSnapshot>,
    loaded: LoadedSession,
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
            AgentStatus::PendingInit | AgentStatus::Running | AgentStatus::AwaitingDescendants
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

fn commit_effective_config(state: &mut DesktopState, config: ResolvedConfig) {
    state.reset_effective_config(config);
}

#[cfg(test)]
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

fn persist_desktop_access_mode_owners_canonical<CompareAndSetGlobal, PersistSession>(
    old_global_access_mode: crate::config::AccessMode,
    access_mode: crate::config::AccessMode,
    current_root_session_id: Option<SessionId>,
    mut compare_and_set_global: CompareAndSetGlobal,
    persist_session: PersistSession,
) -> Result<AccessModePersistenceCommit, String>
where
    CompareAndSetGlobal: FnMut(
        crate::config::AccessMode,
        crate::config::AccessMode,
    ) -> Result<Option<Utf8PathBuf>, String>,
    PersistSession:
        FnOnce(SessionId, crate::config::AccessMode) -> Result<Option<SessionRecord>, String>,
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
        return Ok(AccessModePersistenceCommit {
            remembered_path,
            session: None,
        });
    };
    let session = match persist_session(session_id, access_mode) {
        Ok(session) => session,
        Err(session_error) => {
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
    };
    Ok(AccessModePersistenceCommit {
        remembered_path,
        session,
    })
}

fn access_mode_display_label(access_mode: crate::config::AccessMode) -> &'static str {
    match access_mode {
        crate::config::AccessMode::Default => "承認を求める",
        crate::config::AccessMode::AutoReview => "代理で承認",
        crate::config::AccessMode::FullAccess => "フルアクセス",
    }
}

fn session_search_result_can_apply(is_latest: bool, root_run_active: bool) -> bool {
    is_latest && !root_run_active
}

fn apply_session_search_result(
    state: &mut DesktopState,
    is_latest: bool,
    root_run_active: bool,
    result: Result<DesktopSnapshot, String>,
) -> bool {
    if !session_search_result_can_apply(is_latest, root_run_active) {
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
    image_paths: &[Utf8PathBuf],
    result: Result<(), String>,
) -> bool {
    match result {
        Ok(()) => {
            state
                .composer
                .image_attachment_paths
                .retain(|path| !image_paths.contains(path));
            state.set_status_message("追加入力を送信待ちキューに保存しました。");
            true
        }
        Err(error) => {
            state.set_status_message(format!("追加入力の保存に失敗しました: {error}"));
            false
        }
    }
}

fn finish_steer_operation_if_current(
    state: &mut DesktopState,
    workspace_root: &Utf8Path,
    target: &SteerSubmissionTarget,
) -> bool {
    if target.workspace_root != workspace_root
        || state.app_state.current_session_id != Some(target.session_id)
    {
        return false;
    }
    state.finish_steer_submission(target.operation_id)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceRootMode {
    Discover,
    Fixed,
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
    run_control: RunControl,
    phase: DesktopRootRunPhase,
    worker: Option<OwnedTaskHandle>,
    next_stop_attempt_id: u64,
    active_stop_attempt_id: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DesktopRootStopAttempt {
    generation: u64,
    attempt_id: u64,
}

enum DesktopRootStopAttemptAdmission {
    Acquired {
        attempt: DesktopRootStopAttempt,
        run_control: RunControl,
    },
    AlreadyPending,
    NotOwned,
    Exhausted,
}

struct PendingRootSubmission {
    run_generation: u64,
    owner_workspace_path: Utf8PathBuf,
    owner_session_id: Option<SessionId>,
    prompt_dispatch: crate::session::PromptDispatchPart,
    image_paths: Vec<Utf8PathBuf>,
    prompt_review_to_cancel: Option<PromptReviewTarget>,
}

struct DesktopSessionRuntimeListener {
    generation: u64,
    target: SessionRuntimeListenerTarget,
    cancel: CancellationToken,
}

impl Drop for DesktopSessionRuntimeListener {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[derive(Default)]
struct DesktopRunLifecycle {
    root: Option<DesktopRootRun>,
}

impl DesktopRunLifecycle {
    fn begin(&mut self, generation: u64, run_control: RunControl) {
        self.root = Some(DesktopRootRun {
            generation,
            run_control,
            phase: DesktopRootRunPhase::Running,
            worker: None,
            next_stop_attempt_id: 1,
            active_stop_attempt_id: None,
        });
    }

    fn attach_worker(
        &mut self,
        generation: u64,
        worker: OwnedTaskHandle,
    ) -> Result<(), OwnedTaskHandle> {
        if worker.generation() != generation {
            return Err(worker);
        }
        let Some(run) = self
            .root
            .as_mut()
            .filter(|run| run.generation == generation && run.worker.is_none())
        else {
            return Err(worker);
        };
        run.worker = Some(worker);
        Ok(())
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

    fn root_scope_control(&self, generation: u64) -> Option<RunControl> {
        self.root
            .as_ref()
            .filter(|run| run.generation == generation)
            .map(|run| run.run_control.clone())
    }

    fn begin_stop_attempt(&mut self, generation: u64) -> DesktopRootStopAttemptAdmission {
        let Some(run) = self
            .root
            .as_mut()
            .filter(|run| run.generation == generation)
        else {
            return DesktopRootStopAttemptAdmission::NotOwned;
        };
        if run.active_stop_attempt_id.is_some() {
            return DesktopRootStopAttemptAdmission::AlreadyPending;
        }
        let attempt_id = run.next_stop_attempt_id;
        let Some(next_attempt_id) = attempt_id.checked_add(1) else {
            return DesktopRootStopAttemptAdmission::Exhausted;
        };
        run.next_stop_attempt_id = next_attempt_id;
        run.active_stop_attempt_id = Some(attempt_id);
        DesktopRootStopAttemptAdmission::Acquired {
            attempt: DesktopRootStopAttempt {
                generation,
                attempt_id,
            },
            run_control: run.run_control.clone(),
        }
    }

    fn finish_stop_attempt(&mut self, attempt: DesktopRootStopAttempt) -> bool {
        let Some(run) = self.root.as_mut().filter(|run| {
            run.generation == attempt.generation
                && run.active_stop_attempt_id == Some(attempt.attempt_id)
        }) else {
            return false;
        };
        run.active_stop_attempt_id = None;
        true
    }

    fn root_is_finalizing(&self) -> bool {
        self.root
            .as_ref()
            .is_some_and(|run| run.phase == DesktopRootRunPhase::Finalizing)
    }

    #[cfg(test)]
    fn can_steer_root(&self) -> bool {
        self.root
            .as_ref()
            .is_some_and(|run| run.phase == DesktopRootRunPhase::Running)
    }

    fn cancellation_requested(&self) -> bool {
        self.root
            .as_ref()
            .is_some_and(|run| run.run_control.is_cancelled())
    }

    fn observe_terminal_event(&mut self) {
        if let Some(run) = self.root.as_mut() {
            run.phase = DesktopRootRunPhase::Finalizing;
        }
    }

    fn finish_root(&mut self) {
        if let Some(mut root) = self.root.take()
            && let Some(worker) = root.worker.take()
        {
            worker.detach();
        }
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
    let runtime_owner_matches = target.runtime_owner_token == runtime_owner_token
        || access_runtime_owner_terminal_settlement_matches(
            &target.runtime_owner_token,
            runtime_owner_token,
        );
    if target.workspace_root != workspace_root
        || target.config_generation != config_generation
        || !runtime_owner_matches
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
    workspace_authority_root: &Utf8Path,
) -> Option<bool> {
    if !tracker.finish_if_current(request_id, target) {
        return None;
    }
    Some(target.workspace_authority_root == workspace_authority_root)
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

impl DesktopController {
    pub(crate) fn config_draft_mutation_admission_open(&self) -> bool {
        !self.run_lifecycle.root_is_active()
            && !self.state.is_busy()
            && !self.state.navigation_loading()
            && !self.state.background_mutation_pending()
    }
}

#[cfg(test)]
mod command_projection_owner_tests;

struct PendingPermission {
    confirmation_id: u64,
    request: PermissionRequest,
    responder: mpsc::Sender<ReviewDecision>,
    run_control: RunControl,
}

struct DesktopSideChatRun {
    side_chat_id: String,
    run_generation: u64,
    cancel: CancellationToken,
    worker: OwnedTaskHandle,
    phase: String,
    streamed_text: String,
    delete_after_finish: bool,
}

enum DesktopExitDrain {
    Running {
        worker: OwnedTaskHandle,
        receiver: mpsc::Receiver<Result<(), String>>,
    },
    Completed(Result<(), String>),
}

struct PendingInitialSetupConfigImport {
    generation: u64,
    workspace_root: Utf8PathBuf,
    session_id: Option<SessionId>,
    global_config_path: Option<Utf8PathBuf>,
    setup_generation: u64,
    config_generation: u64,
    config: ResolvedConfig,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct InitialSetupConfigPublicValue {
    pub key: String,
    pub text: String,
    pub sensitive: bool,
    pub configured: bool,
}

impl std::fmt::Debug for InitialSetupConfigPublicValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InitialSetupConfigPublicValue")
            .field("key", &self.key)
            .field("text_chars", &self.text.chars().count())
            .field("sensitive", &self.sensitive)
            .field("configured", &self.configured)
            .finish()
    }
}

impl DesktopSideChatRun {
    fn owns(&self, side_chat_id: &str, run_generation: u64) -> bool {
        self.side_chat_id == side_chat_id && self.run_generation == run_generation
    }
}

fn side_chat_request_profile(binding: &SideChatBinding) -> SideChatRequestProfile {
    SideChatRequestProfile {
        hub_route: None,
        base_url: binding.base_url.clone(),
        model: binding.model.clone(),
        provider_profile: binding.provider_profile,
        request_timeout_ms: binding.request_timeout_ms,
        connect_timeout_ms: binding.connect_timeout_ms,
        max_retries: binding.max_retries,
        context_window: binding.context_window,
        system_prompt: side_chat_system_prompt(&binding.system_prompt),
        // Use the Side binding's explicit reference; Main is a separate authority.
        api_key_env: binding.api_key_env.clone(),
        extra_headers: Default::default(),
    }
}

pub(crate) struct DesktopController {
    pub(crate) app: App,
    pub(crate) state: DesktopState,
    preferences: DesktopPreferences,
    persist_preferences_to_disk: bool,
    runtime_tx: mpsc::SyncSender<RuntimeMessage>,
    runtime_rx: mpsc::Receiver<RuntimeMessage>,
    control_tx: DesktopControlPlaneSender,
    control_rx: mpsc::Receiver<RuntimeMessage>,
    root_task_runtime: LocalTaskExecutor,
    side_chat_task_runtime: LocalTaskExecutor,
    pending_permission: Option<PendingPermission>,
    next_permission_request_id: Arc<AtomicU64>,
    run_lifecycle: DesktopRunLifecycle,
    exit_drain: Option<DesktopExitDrain>,
    prompt_review_workers: Vec<std::thread::JoinHandle<()>>,
    pending_root_submission: Option<PendingRootSubmission>,
    // Keep text-edit snapshots with their draft session when the user navigates away.
    // The active composer's copy is a projection of this draft, consumed on durable send.
    edit_image_drafts: HashMap<SessionId, (ProjectId, Vec<crate::session::ImagePart>)>,
    composer_commit_generation: u64,
    next_root_run_generation: u64,
    side_chat_runs: HashMap<SessionId, DesktopSideChatRun>,
    side_chat_contexts: HashMap<SessionId, SideChatContextMetadata>,
    side_chat_errors: HashMap<SessionId, String>,
    next_initial_setup_import_generation: u64,
    pending_initial_setup_config_import: Option<PendingInitialSetupConfigImport>,
    next_enhance_request_id: u64,
    next_session_runtime_listener_generation: u64,
    session_runtime_listener: Option<DesktopSessionRuntimeListener>,
    session_search_requests: SessionSearchRequestTracker<SessionSearchRequestTarget>,
    snapshot_requests: LatestRequestTracker<SnapshotRequestTarget>,
    turn_page_requests: LatestRequestTracker<SessionPageRequestTarget>,
    session_projection_refresh_requests: LatestRequestTracker<SessionRefreshRequestTarget>,
    durable_agent_activity_refresh_requests: LatestRequestTracker<SessionRefreshRequestTarget>,
    history_export_requests: LatestRequestTracker<HistoryExportRequestTarget>,
    provider_catalog_requests: LatestRequestTracker<ProviderCatalogRequestTarget>,
    docling_readiness_requests: LatestRequestTracker<DoclingReadinessRequestTarget>,
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
        mut args: DesktopArgs,
        mut preferences: DesktopPreferences,
        persist_preferences_to_disk: bool,
    ) -> Result<Self, AppRunError> {
        let (runtime_tx, runtime_rx) = mpsc::sync_channel(DESKTOP_RUNTIME_MAILBOX_CAPACITY);
        // Bootstrap identity, permission requests, and worker settlement share
        // one lossless FIFO. It is drained before the lossy runtime stream.
        let (control_channel_tx, control_rx) = mpsc::channel();
        let control_tx = DesktopControlPlaneSender {
            tx: control_channel_tx,
        };
        let root_task_runtime =
            LocalTaskExecutor::new("moyai-desktop-root-runtime").map_err(AppRunError::Message)?;
        let side_chat_task_runtime = LocalTaskExecutor::new("moyai-desktop-side-chat-runtime")
            .map_err(AppRunError::Message)?;
        let mut restore_notice = None;
        if args.directory.is_none()
            && args.session_id.is_none()
            && !args.continue_last
            && preferences.onboarding_intent.is_none()
            && let Some(directory) = preferences.last_workspace.as_ref()
            && !preferences
                .deleted_project_roots
                .iter()
                .any(|root| directory.starts_with(root))
        {
            if !directory.is_dir() {
                restore_notice = Some(format!(
                    "前回の作業フォルダーを開けませんでした。プロジェクト一覧から選び直してください: {directory}"
                ));
            } else {
                if let Some(session_id) = preferences.last_session_id {
                    match app.session_service.get_session(session_id).await {
                    Ok(session)
                        if crate::workspace::PathGuard::same_path_identity(
                            &session.cwd,
                            directory,
                        ) =>
                    {
                        match AppBootstrap::rebuild_for_session_with_process_runtime(
                            &session,
                            app.process_runtime.clone(),
                        )
                        .await
                        {
                            Ok(restored) => {
                                app = restored;
                                args.session_id = Some(session_id);
                            }
                            Err(error) => {
                                restore_notice = Some(format!(
                                    "前回の会話を開けませんでした。プロジェクトと作業フォルダーを確認してください: {error}"
                                ))
                            }
                        }
                    }
                    Ok(_) => {
                        restore_notice = Some(
                            "前回の会話の作業フォルダーが変わっているため、保存済みのフォルダーを開きます。".to_string(),
                        )
                    }
                    Err(error) => restore_notice = Some(format!("前回の会話を読み込めませんでした。保存済みの作業フォルダーを開きます: {error}")),
                }
                }
                if args.session_id.is_none() {
                    match AppBootstrap::rebuild_for_directory_with_process_runtime(
                        directory,
                        app.process_runtime.clone(),
                    )
                    .await
                    {
                        Ok(restored) => app = restored,
                        Err(error) => {
                            restore_notice = Some(format!(
                                "前回の作業フォルダーを開けませんでした。プロジェクト一覧から選び直してください: {error}"
                            ))
                        }
                    }
                }
            }
        }
        if args.directory.is_some() {
            preferences.unmark_project_deleted(&app.workspace.root);
        } else {
            purge_deleted_project_roots(&app, &preferences)
                .await
                .map_err(AppRunError::Message)?;
            if preferences.is_project_deleted(&app.workspace.root) {
                let process_runtime = app.process_runtime.clone();
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
                app = AppBootstrap::rebuild_for_directory_with_process_runtime(
                    &next_root,
                    process_runtime,
                )
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
            if session.project_id != app.workspace.project_id
                || session.cwd != app.workspace.cwd
                || app.workspace.authority_root() != session.cwd.as_path()
            {
                let process_runtime = app.process_runtime.clone();
                app = AppBootstrap::rebuild_for_session_with_process_runtime(
                    &session,
                    process_runtime,
                )
                .await
                .map_err(|error| {
                    AppRunError::Message(format!(
                        "failed to open session workspace {}: {error}",
                        session.cwd
                    ))
                })?;
                if app.workspace.project_id != session.project_id {
                    return Err(AppRunError::Message(format!(
                        "session {} cwd {} resolves to project {}, not its stored project {}",
                        session.id, session.cwd, app.workspace.project_id, session.project_id
                    )));
                }
            }
        }
        let mut snapshot = if args.continue_last {
            load_snapshot_continue_last(&app).await?
        } else {
            load_snapshot(&app, &args).await?
        };
        if let Some(session_id) = args.session_id.or_else(|| snapshot.selected_session_id()) {
            let session = app.session_service.get_session(session_id).await?;
            if session.project_id != app.workspace.project_id
                || session.cwd != app.workspace.cwd
                || app.workspace.authority_root() != session.cwd.as_path()
            {
                let process_runtime = app.process_runtime.clone();
                app = AppBootstrap::rebuild_for_session_with_process_runtime(
                    &session,
                    process_runtime,
                )
                .await
                .map_err(|error| {
                    AppRunError::Message(format!(
                        "failed to restore selected session workspace {}: {error}",
                        session.cwd
                    ))
                })?;
                if app.workspace.project_id != session.project_id {
                    return Err(AppRunError::Message(format!(
                        "session {} cwd {} resolves to project {}, not its stored project {}",
                        session.id, session.cwd, app.workspace.project_id, session.project_id
                    )));
                }
                snapshot = load_snapshot_for_selection(&app, Some(session_id)).await?;
            }
        }
        let mut state = DesktopState::new(snapshot, app.config.clone());
        state.set_file_change_display_roots(&app.workspace.root, app.workspace.authority_root());
        state.workspace_input = app.workspace.cwd.to_string();
        state.begin_startup(
            args.global_config_existed_at_launch,
            global_config_path().ok(),
            &app.workspace.root,
        );
        if let Some(intent) = preferences.onboarding_intent {
            state.startup.resume_onboarding(intent);
            state.apply_startup_overlay();
        }
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
            state.load_open_session(&detail.read);
            loaded_agent_activity_records = Some((session_id, activity_records));
        }
        let mut controller = Self {
            app,
            state,
            preferences,
            persist_preferences_to_disk,
            runtime_tx,
            runtime_rx,
            control_tx,
            control_rx,
            root_task_runtime,
            side_chat_task_runtime,
            pending_permission: None,
            next_permission_request_id: Arc::new(AtomicU64::new(1)),
            run_lifecycle: DesktopRunLifecycle::default(),
            exit_drain: None,
            prompt_review_workers: Vec::new(),
            pending_root_submission: None,
            edit_image_drafts: HashMap::new(),
            composer_commit_generation: 0,
            next_root_run_generation: 1,
            side_chat_runs: HashMap::new(),
            side_chat_contexts: HashMap::new(),
            side_chat_errors: HashMap::new(),
            next_initial_setup_import_generation: 1,
            pending_initial_setup_config_import: None,
            next_enhance_request_id: 1,
            next_session_runtime_listener_generation: 1,
            session_runtime_listener: None,
            session_search_requests: SessionSearchRequestTracker::default(),
            snapshot_requests: LatestRequestTracker::default(),
            turn_page_requests: LatestRequestTracker::default(),
            session_projection_refresh_requests: LatestRequestTracker::default(),
            durable_agent_activity_refresh_requests: LatestRequestTracker::default(),
            history_export_requests: LatestRequestTracker::default(),
            provider_catalog_requests: LatestRequestTracker::default(),
            docling_readiness_requests: LatestRequestTracker::default(),
            access_mode_persistence_requests: LatestRequestTracker::default(),
            pending_access_mode_adoption: None,
            projection_revision: 0,
            loaded_agent_activity_records,
            durable_agent_activity_refresh_failures: 0,
            attachment_asset_app: None,
            authorized_attachment_assets: BTreeSet::new(),
        };
        controller.reconcile_runtime_listener_with_open_session();
        if let Some(intent) = controller.preferences.onboarding_intent {
            controller.show_onboarding_surface(intent);
        }
        if let Some(notice) = restore_notice {
            controller.state.set_status_message(notice);
        }
        controller.persist_preferences();
        Ok(controller)
    }

    pub(crate) fn next_web_state(&mut self) -> Result<DesktopWebState, String> {
        self.discard_terminal_pending_permission();
        self.reconcile_pending_initial_setup_config_import_owner();
        self.reconcile_attachment_asset_authorizations()?;
        let revision = advance_projection_revision(&mut self.projection_revision)?;
        let mut runtime_projection = DesktopRuntimeProjection {
            root_run_finalizing: self.run_lifecycle.root_is_finalizing(),
            root_run_generation: self.run_lifecycle.root_generation(),
            last_root_run_epoch: self.last_root_run_epoch(),
            composer_commit_generation: self.composer_commit_generation,
            active_turn_expectation: self.current_active_turn_expectation(),
            ..DesktopRuntimeProjection::default()
        };
        runtime_projection.side_chat = self.side_chat_projection();
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
            let current_turn_records = records
                .iter()
                .filter(|record| record.is_current_turn)
                .cloned()
                .collect();
            let workspace_path = self.state.snapshot.workspace_path.as_str();
            let (rows, tree_active) =
                agent_activity_projection(workspace_path, root_session_id, records);
            let (current_turn_rows, _) =
                agent_activity_projection(workspace_path, root_session_id, current_turn_records);
            runtime_projection.agent_activity_rows = rows;
            runtime_projection.current_turn_agent_activity_rows = current_turn_rows;
            runtime_projection.agent_tree_active = tree_active;
            if refresh_durable_activity {
                self.spawn_durable_agent_activity_refresh(root_session_id);
            }
        }
        let mut projection = desktop_web_state_with_permission(
            &self.state,
            &runtime_projection,
            self.pending_permission
                .as_ref()
                .map(|pending| (pending.confirmation_id, &pending.request)),
        );
        projection.projection_revision = projection_revision_text(revision);
        projection.exit_requested = self.exit_drain.is_some();
        if let Some(network) = &self.state.device_network {
            let jobs = network.remote_jobs();
            if !projection.confirmation_visible
                && let Some(pending) = jobs.pending_approval()
            {
                projection.confirmation_visible = true;
                projection.confirmation_id = Some(pending.confirmation_id.to_string());
                projection.confirmation_text =
                    super::web_model::format_permission_confirmation_text(&pending.request);
                let mut permission =
                    super::web_model::DesktopPermissionProjection::from(&pending.request);
                permission.remote = Some(pending.context);
                projection.confirmation = Some(permission);
            }
            if jobs.has_active_jobs() {
                projection.async_polling_required = true;
                projection
                    .pending_async_operations
                    .push("remote_job".into());
            }
        }
        if !self.side_chat_runs.is_empty() {
            if let Some(hub) = &mut projection.hub {
                hub.can_change_side_chat_mode = false;
            }
            projection.async_polling_required = true;
            if !projection
                .pending_async_operations
                .iter()
                .any(|operation| operation == "side_chat")
            {
                projection
                    .pending_async_operations
                    .push("side_chat".to_string());
            }
        }
        Ok(projection)
    }

    pub(crate) fn current_active_turn_expectation(&self) -> ActiveTurnExpectation {
        let Some(session_id) = self.state.app_state.current_session_id else {
            return ActiveTurnExpectation::initial_idle();
        };
        let projected = self.state.app_state.active_turn_expectation;
        if matches!(
            self.state.app_state.run_status,
            crate::tui::state::RunStatus::Running
        ) {
            if let Some(turn_id) = self
                .session_runtime_listener
                .as_ref()
                .filter(|listener| {
                    listener.target.workspace_root == self.app.workspace.root
                        && listener.target.session_id == session_id
                })
                .map(|listener| listener.target.turn_id)
            {
                if projected.active_turn_id() == Some(turn_id) {
                    return projected;
                }
                if let Some(open_expectation) = self
                    .state
                    .open_session
                    .as_ref()
                    .filter(|open| open.session_id() == session_id)
                    .map(OpenSessionView::active_turn_expectation)
                    .filter(|expected| expected.active_turn_id() == Some(turn_id))
                {
                    return open_expectation;
                }
                if let ActiveTurnExpectation::Idle { revision, .. } = projected {
                    return ActiveTurnExpectation::Turn {
                        turn_id,
                        revision: revision + 1,
                    };
                }
            }
        }
        projected
    }

    pub(crate) fn hub_context_active(&self, context: crate::hub::HubReviewContext) -> bool {
        match context {
            crate::hub::HubReviewContext::Main => {
                self.run_lifecycle.root_is_active()
                    || self.current_agent_tree_active()
                    || self.state.prompt_enhance_pending()
            }
            crate::hub::HubReviewContext::SideChat => !self.side_chat_runs.is_empty(),
            crate::hub::HubReviewContext::Approve => {
                self.run_lifecycle.root_is_active() || self.current_agent_tree_active()
            }
        }
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
        if !unique_background_request_admission_open(
            self.snapshot_requests.is_pending(),
            self.state.snapshot_refresh_pending(),
        ) {
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
                .set_status_message("選択したチャットが見つかりません。一覧を更新してください。");
            return false;
        };
        self.invalidate_session_target_requests();
        self.state.set_status_message("会話を開いています…");
        let request_id = self.state.begin_session_load(session_id);
        self.spawn_session_load(session_id, SessionLoadReason::UserSelection, request_id);
        true
    }

    /// The edit-fork command has just created this exact durable session, so it is
    /// not yet present in the current sidebar snapshot for indexed selection.
    pub(crate) fn open_created_edit_fork(
        &mut self,
        session_id: SessionId,
        retained_images: Vec<crate::session::ImagePart>,
    ) -> bool {
        if !self.ensure_navigation_admission("edited session") {
            return false;
        }
        self.invalidate_session_target_requests();
        self.edit_image_drafts
            .insert(session_id, (self.app.workspace.project_id, retained_images));
        self.state.set_status_message("編集する会話を開いています…");
        let request_id = self.state.begin_session_load(session_id);
        self.spawn_session_load(session_id, SessionLoadReason::CreatedEditFork, request_id);
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
            self.state.set_status_message(
                "選択したプロジェクトが見つかりません。一覧を更新してください。",
            );
            return false;
        };
        if path == self.app.workspace.root {
            self.state.view.hub_project_open = false;
            self.state.hide_overlay();
            return true;
        }
        self.invalidate_session_target_requests();
        self.state
            .set_status_message(format!("プロジェクトを開いています: {}", path));
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
                .set_status_message("選択したチャットが見つかりません。一覧を更新してください。");
            return false;
        };
        if row.loaded_status != LoadedSessionStatus::Active {
            self.state.set_status_message(
                "選択したチャットでは実行中の処理が見つかりません。一覧を更新してください。",
            );
            return false;
        }
        let session_id = row.session_id;
        self.invalidate_session_target_requests();
        self.state
            .set_status_message("実行中のチャットを開いています…");
        let request_id = self.state.begin_session_load(session_id);
        self.spawn_session_rejoin(session_id, request_id);
        true
    }

    fn ensure_navigation_admission(&mut self, target: &str) -> bool {
        if !self.ensure_initial_setup_owner_replacement_admission(target) {
            return false;
        }
        if !self.ensure_unscoped_prompt_review_action(target) {
            return false;
        }
        let Some(reason) = navigation_admission_blocker(
            self.state.is_busy(),
            self.state.background_mutation_pending(),
            self.state.navigation_loading(),
            self.run_lifecycle.root_is_finalizing(),
        ) else {
            return true;
        };
        self.state
            .set_status_message(format!("{target} cannot change while {reason}"));
        false
    }

    fn ensure_initial_setup_owner_replacement_admission(&mut self, target: &str) -> bool {
        if !self.state.startup.requires_initial_setup() {
            return true;
        }
        self.state.set_status_message(format!(
            "{target} cannot replace the initial-setup owner; complete initial setup with Finish first"
        ));
        false
    }

    pub(crate) fn ensure_unscoped_prompt_review_action(&mut self, target: &str) -> bool {
        if self.state.app_state.prompt_review.is_none() {
            return true;
        }
        self.state.set_status_message(format!(
            "{target} cannot replace the active Prompt Review; send or cancel that exact review first"
        ));
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
        self.stop_session_runtime_listener();
        self.invalidate_session_search_requests();
        self.snapshot_requests.clear();
        self.turn_page_requests.clear();
        self.session_projection_refresh_requests.clear();
        self.durable_agent_activity_refresh_requests.clear();
        self.history_export_requests.clear();
        self.state.finish_snapshot_refresh();
        self.state.finish_turn_page_load();
        self.state.finish_history_export();
    }

    fn reconcile_runtime_listener_with_open_session(&mut self) {
        let target = self.state.open_session.as_ref().and_then(|open_session| {
            if open_session.session().status != SessionStatus::Running {
                return None;
            }
            Some(SessionRuntimeListenerTarget {
                workspace_root: self.app.workspace.root.clone(),
                session_id: open_session.session_id(),
                turn_id: open_session.active_turn_id()?,
            })
        });
        self.reconcile_session_runtime_listener(target);
    }

    fn reconcile_session_runtime_listener(&mut self, target: Option<SessionRuntimeListenerTarget>) {
        let Some(target) = target else {
            self.session_runtime_listener = None;
            return;
        };
        if self
            .session_runtime_listener
            .as_ref()
            .is_some_and(|listener| listener.target == target)
        {
            return;
        }

        self.session_runtime_listener = None;
        let generation = self.next_session_runtime_listener_generation;
        let Some(next_generation) = generation.checked_add(1) else {
            self.state.set_status_message(
                "desktop session listener generation is exhausted; restart moyAI",
            );
            return;
        };
        self.next_session_runtime_listener_generation = next_generation;
        let cancel = CancellationToken::new();
        spawn_desktop_session_runtime_listener(
            self.app.clone(),
            self.runtime_tx.clone(),
            generation,
            target.clone(),
            cancel.clone(),
        );
        self.session_runtime_listener = Some(DesktopSessionRuntimeListener {
            generation,
            target,
            cancel,
        });
    }

    fn stop_session_runtime_listener(&mut self) {
        self.reconcile_session_runtime_listener(None);
    }

    #[cfg(test)]
    fn start_run(&mut self, prompt: String) -> bool {
        self.start_run_at(prompt, self.current_active_turn_expectation())
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
            .set_status_message("新しいチャットを準備しています…");
        let request_id = self.state.begin_workspace_load(root.clone(), None);
        self.spawn_fixed_workspace_load(root, request_id);
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
            self.state.set_status_message(
                "選択したプロジェクトが見つかりません。一覧を更新してください。",
            );
            return false;
        };
        self.invalidate_session_target_requests();
        self.state.hide_overlay();
        if path == self.app.workspace.root {
            self.state.select_project(index);
            self.start_new_chat_with_global_access();
            self.state
                .set_status_message("新しいチャットを開始できます。");
            self.persist_preferences();
            return true;
        }
        self.state.set_status_message(format!(
            "新しいチャットの作業フォルダーを開いています: {}",
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
        self.state.set_status_message("チャットを開いています…");
        let request_id = self
            .state
            .begin_workspace_load(root.clone(), Some(session_id));
        self.spawn_workspace_load_for_selection(
            root,
            Some(session_id),
            request_id,
            WorkspaceRootMode::Fixed,
        );
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
                .set_status_message("削除するチャットが見つかりません。一覧を確認してください。");
            return false;
        }
        self.invalidate_session_target_requests();
        self.state.set_status_message("チャットを削除しています…");
        let operation_id = self.state.begin_session_delete_mutation();
        self.spawn_session_delete(session_id, operation_id);
        true
    }

    pub(crate) fn create_project_from_picker(&mut self) -> bool {
        if !self.ensure_navigation_admission("project") {
            return false;
        }
        let start_dir = (!self.is_quick_chat_workspace()).then_some(&self.app.workspace.cwd);
        match pick_workspace_directory(start_dir) {
            Ok(Some(path)) => self.open_picked_project_directory(path),
            Ok(None) => self.state.set_status_message("project creation cancelled"),
            Err(error) => self
                .state
                .set_status_message(format!("project creation failed: {error}")),
        }
        true
    }

    fn open_picked_project_directory(&mut self, path: Utf8PathBuf) {
        self.invalidate_session_target_requests();
        self.state.hide_overlay();
        self.state
            .set_status_message(format!("作業フォルダーを開いています: {}", path));
        // A folder selected for a new project owns the new chat's working
        // directory, even when discovery groups it with an existing Git project.
        let request_id = self
            .state
            .begin_new_project_session_workspace_load(path.clone());
        self.spawn_workspace_load_for_new_project_session(path, request_id);
    }

    pub(crate) fn start_review_uncommitted_at(
        &mut self,
        prompt: String,
        expected_active_turn: ActiveTurnExpectation,
    ) -> bool {
        if !matches!(expected_active_turn, ActiveTurnExpectation::Idle { .. }) {
            self.state
                .set_status_message("uncommitted review requires the captured idle run owner");
            return false;
        }
        let prompt = prompt.trim().to_string();
        let prompt_dispatch = crate::session::PromptDispatchPart::raw(&prompt);
        self.launch_run_with_options(
            prompt,
            prompt_dispatch,
            Some(ReviewRequest::Uncommitted),
            None,
            expected_active_turn,
        )
    }

    #[cfg(test)]
    fn start_review_uncommitted(&mut self, prompt: String) -> bool {
        self.start_review_uncommitted_at(prompt, self.current_active_turn_expectation())
    }

    pub(crate) fn start_prompt_enhance_at(
        &mut self,
        raw_prompt: String,
        expected_active_turn: ActiveTurnExpectation,
    ) -> bool {
        if self.exit_drain.is_some() {
            self.state
                .set_status_message("moyAIを終了しています。新しい依頼は送信できません。");
            return false;
        }
        if !self.ensure_unscoped_prompt_review_action("prompt enhancement") {
            return false;
        }
        let raw_prompt = raw_prompt.trim().to_string();
        if !unique_background_request_admission_open(false, self.state.prompt_enhance_pending()) {
            self.state
                .set_status_message("prompt enhancement is already in progress");
            return false;
        }
        if !matches!(expected_active_turn, ActiveTurnExpectation::Idle { .. })
            || raw_prompt.is_empty()
            || self.state.is_busy()
            || self.state.navigation_loading()
            || self.state.background_mutation_pending()
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
            owner_generation: self.state.composer.owner_generation(),
            expected_active_turn,
        };
        let cancellation = CancellationToken::new();
        let hub_route = match self
            .state
            .hub_connection
            .as_ref()
            .map(|hub| {
                hub.begin_preparation(crate::hub::HubReviewContext::Main, cancellation.clone())
            })
            .transpose()
        {
            Ok(route) => route.flatten(),
            Err(error) => {
                self.state.set_status_message(error.to_string());
                return false;
            }
        };
        self.state.begin_prompt_enhance_at(
            request_id,
            &raw_prompt,
            cancellation.clone(),
            expected_active_turn,
        );
        let runtime_tx = self.runtime_tx.clone();
        let config = self.state.provider_config.effective_config.clone();
        let session_service = self.app.session_service.clone();
        let target_session_id = target.session_id;
        let target_expected_active_turn = target.expected_active_turn;
        self.prompt_review_workers
            .retain(|worker| !worker.is_finished());
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop enhance runtime");
            let result = runtime.block_on(async move {
                let result = crate::tui::prompt_enhance::enhance_prompt_for_captured_idle(
                    &session_service,
                    target_session_id,
                    target_expected_active_turn,
                    || async {
                        if let Some(route) = &hub_route {
                            let turn_config = crate::config::ResolvedTurnConfig::from_effective(
                                &route.runtime_config(&config),
                            )
                            .map_err(|error| error.to_string())?;
                            crate::tui::prompt_enhance::enhance_prompt_with_client(
                                &turn_config,
                                route.client().as_ref(),
                                &raw_prompt,
                                cancellation,
                            )
                            .await
                            .map_err(|error| error.to_string())
                        } else {
                            crate::tui::prompt_enhance::enhance_prompt(
                                &config,
                                &raw_prompt,
                                cancellation,
                            )
                            .await
                            .map_err(|error| error.to_string())
                        }
                    },
                )
                .await;
                if let Some(route) = hub_route {
                    route.finish().await;
                }
                result
            });
            let _ = runtime_tx.send(RuntimeMessage::EnhanceFinished {
                request_id,
                target,
                result,
            });
        });
        self.prompt_review_workers.push(worker);
        true
    }

    #[cfg(test)]
    fn start_prompt_enhance(&mut self, raw_prompt: String) -> bool {
        self.start_prompt_enhance_at(raw_prompt, self.current_active_turn_expectation())
    }

    pub(crate) fn send_prompt_review_at(
        &mut self,
        review_request_id: u64,
        send_enhanced: bool,
        review_draft: String,
        expected_active_turn: ActiveTurnExpectation,
    ) -> bool {
        if self.state.navigation_loading() {
            self.state
                .set_status_message("wait for navigation to finish before sending");
            return false;
        }
        let Some(prompt_review_to_cancel) = self.prompt_review_target(review_request_id) else {
            self.state
                .set_status_message("enhanced draft target is no longer current");
            return false;
        };
        if prompt_review_to_cancel.expected_active_turn != expected_active_turn {
            self.state
                .set_status_message("the active run owner changed since this Prompt Review began");
            return false;
        }
        if !matches!(expected_active_turn, ActiveTurnExpectation::Idle { .. }) {
            self.state
                .set_status_message("Prompt Review send requires the captured idle run owner");
            return false;
        }
        // The editable review draft is frontend-owned until this atomic action.
        // Commit the exact submitted text for both choices so the durable
        // PromptDispatchPart preserves edit provenance even when the user sends
        // the original prompt rather than the enhanced text.
        let Some(prompt_dispatch) = self.state.build_prompt_dispatch_from_draft(
            review_request_id,
            review_draft,
            send_enhanced,
        ) else {
            self.state
                .set_status_message("enhanced draft is not ready yet");
            return false;
        };
        let prompt = prompt_dispatch.dispatch_prompt_text.clone();
        self.launch_run_with_options(
            prompt,
            prompt_dispatch,
            None,
            Some(prompt_review_to_cancel),
            expected_active_turn,
        )
    }

    #[cfg(test)]
    fn send_prompt_review(
        &mut self,
        review_request_id: u64,
        send_enhanced: bool,
        review_draft: String,
    ) -> bool {
        self.send_prompt_review_at(
            review_request_id,
            send_enhanced,
            review_draft,
            self.current_active_turn_expectation(),
        )
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
            profile: self.state.provider_config.provider_profile_input,
            api_key_env: non_empty_trimmed_owned(
                &self.state.provider_config.provider_api_key_env_input,
            ),
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
            target.profile,
            target.api_key_env.clone(),
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

    pub(crate) fn check_docling_readiness(&mut self) -> bool {
        self.start_docling_readiness_check(
            self.state.provider_config.effective_config.clone(),
            DoclingReadinessConfigOwner::EffectiveConfig,
        )
    }

    pub(crate) fn check_initial_setup_docling_readiness(
        &mut self,
        values: Vec<(String, String)>,
        expected_import_generation: Option<u64>,
    ) -> bool {
        let values = match self
            .hydrate_initial_setup_import_sensitive_values(values, expected_import_generation)
        {
            Ok(values) => values,
            Err(error) => {
                self.state
                    .fail_docling_readiness_check(format!("initial setup import error: {error}"));
                return false;
            }
        };
        let config = match build_resolved_config_from_key_values(self.state.global_config(), values)
        {
            Ok(config) => config,
            Err(error) => {
                self.state
                    .fail_docling_readiness_check(format!("initial setup config error: {error}"));
                return false;
            }
        };
        self.start_docling_readiness_check(config, DoclingReadinessConfigOwner::InitialSetupDraft)
    }

    fn start_docling_readiness_check(
        &mut self,
        mut config: ResolvedConfig,
        owner: DoclingReadinessConfigOwner,
    ) -> bool {
        if self.docling_readiness_requests.is_pending()
            || self.state.docling_readiness_check_pending()
        {
            self.state
                .set_status_message("Docling readiness check is already in progress");
            return false;
        }
        if let Err(error) = config.normalize_and_validate_docling_runtime() {
            self.state.fail_docling_readiness_check(error);
            return false;
        }
        if !config.docling.enabled {
            self.state.fail_docling_readiness_check(
                "Docling readiness check requires docling.enabled=true",
            );
            return false;
        }
        let target = DoclingReadinessRequestTarget {
            owner,
            base_url: config.docling.base_url.clone(),
            config_generation: self.state.provider_config.config_generation,
        };
        let request_id = self.docling_readiness_requests.begin(target.clone());
        self.state
            .begin_docling_readiness_check(crate::docling::endpoint(&target.base_url, "/ready"));
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop Docling readiness runtime");
            let result = runtime
                .block_on(crate::docling::DoclingClient::new(config.docling).check_readiness())
                .map_err(|error| error.to_string());
            let _ = runtime_tx.send(RuntimeMessage::DoclingReadinessChecked {
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
        profile: ProviderProfile,
        api_key_env: String,
        context_window: String,
        selected_model_id: String,
    ) {
        let target_changed = self.state.accept_provider_action_input(
            base_url,
            profile,
            api_key_env,
            context_window,
            selected_model_id,
        );
        if target_changed {
            self.provider_catalog_requests.clear();
        }
    }

    fn reset_effective_config_without_network(&mut self, config: ResolvedConfig) {
        let catalog_was_pending =
            self.provider_catalog_requests.is_pending() || self.state.provider_model_load_pending();
        self.provider_catalog_requests.clear();
        if catalog_was_pending {
            self.state.cancel_provider_model_load();
        }
        self.docling_readiness_requests.clear();
        self.state.cancel_docling_readiness_check();
        commit_effective_config(&mut self.state, config);
        self.state.refresh_startup_config_status();
    }

    fn adopt_global_config_without_network(&mut self, config: ResolvedConfig) {
        let catalog_was_pending =
            self.provider_catalog_requests.is_pending() || self.state.provider_model_load_pending();
        self.provider_catalog_requests.clear();
        if catalog_was_pending {
            self.state.cancel_provider_model_load();
        }
        self.docling_readiness_requests.clear();
        self.state.cancel_docling_readiness_check();
        self.state.replace_global_config(config);
        self.state.refresh_startup_config_status();
    }

    pub(crate) fn save_device_network_config(
        &mut self,
        shared: &crate::device_network::SharedHubConfig,
        validate_initial_setup: bool,
    ) -> Result<(), String> {
        let path =
            crate::config::loader::global_config_path().map_err(|_| "storage_error".to_string())?;
        let config = crate::tui::config_editor::save_device_network_config(
            &path,
            &self.state.global_config().device_network,
            shared,
            |candidate| {
                if validate_initial_setup
                    && super::startup::DesktopStartupState::begin(
                        true,
                        Some(path.clone()),
                        &self.app.workspace.root,
                        candidate,
                    )
                    .requires_initial_setup()
                {
                    return Err("initial_setup_incomplete".into());
                }
                Ok(())
            },
        )?;
        self.app.config = config.clone();
        self.adopt_global_config_without_network(config);
        Ok(())
    }

    /// The connection has its own completed commit. Failure here leaves the
    /// first-use latch pending without reporting that saved connection as lost.
    pub(crate) fn finish_initial_setup_after_device_network_persist(
        &mut self,
    ) -> Result<(), String> {
        self.persist_onboarding_intent(None)?;
        self.pending_initial_setup_config_import = None;
        self.state.complete_initial_setup_after_persist();
        self.state.show_hub_editor();
        Ok(())
    }

    fn start_new_chat_with_global_access(&mut self) {
        self.state.start_new_chat();
        self.state.view.hub_project_open = false;
        if self.state.app_state.current_session_id.is_none() {
            let access_mode = self.app.config.permissions.access_mode;
            self.state.provider_config.update_access_mode(access_mode);
        }
    }

    pub(crate) fn apply_provider_session(&mut self) -> bool {
        if !self.state.can_apply_provider_selection() {
            self.state.set_status_message(
                "enter a valid provider URL, connection type, local context budget, and model before applying",
            );
            return false;
        }
        let setup_overlay = self.state.view.startup_overlay_forced;
        let Some(config) = self.apply_provider_selection_to_effective_config() else {
            return false;
        };
        self.reset_effective_config_without_network(config);
        self.state.set_status_message(
            "AIの接続設定を一時的に適用しました。設定ファイルには保存していません。",
        );
        if !setup_overlay {
            self.state.hide_overlay();
        }
        true
    }

    pub(crate) fn save_provider_global(&mut self) -> bool {
        if !self.state.can_save_provider_selection_global() {
            self.state.set_status_message(
                "enter a valid provider URL, connection type, local context budget, and model before saving",
            );
            return false;
        }
        let Some(config) = self.apply_provider_selection_to_global_config() else {
            return false;
        };
        let candidate = match self.provider_config_persistence_candidate(&config) {
            Ok(candidate) => candidate,
            Err(error) => {
                self.state
                    .set_status_message(format!("config save failed: {error}"));
                return false;
            }
        };
        let save_result = candidate.save_global(
            &self.app.workspace.root,
            GlobalConfigAdoptionPolicy::StrictCurrentSchema,
        );
        match self.commit_global_config_save_result(save_result) {
            Ok(message) => {
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

    pub(crate) fn apply_session_config(&mut self, values: Vec<(String, String)>) -> bool {
        match build_resolved_config_from_key_values(
            &self.state.provider_config.effective_config,
            values,
        ) {
            Ok(config) => {
                self.reset_effective_config_without_network(config);
                self.state.set_status_message(
                    "設定を一時的に適用しました。設定ファイルには保存していません。",
                );
                true
            }
            Err(error) => {
                self.state
                    .set_status_message(format!("config error: {error}"));
                false
            }
        }
    }

    fn commit_global_config_save_result(
        &mut self,
        result: Result<crate::tui::config_editor::GlobalConfigSaveResult, String>,
    ) -> Result<String, String> {
        let saved = result?;
        self.app.config = saved.resolved_config.clone();
        self.adopt_global_config_without_network(saved.resolved_config);
        Ok(if saved.preserved_unknown_top_level_sections.is_empty() {
            "共通設定を保存しました。".to_string()
        } else {
            format!(
                "共通設定を保存しました。未対応の設定（{}セクション）はファイルに残し、適用していません。",
                saved.preserved_unknown_top_level_sections.len()
            )
        })
    }

    pub(crate) fn root_run_generation(&self) -> Option<u64> {
        self.run_lifecycle.root_generation()
    }

    pub(crate) fn root_run_admission_snapshot(
        &self,
        generation: u64,
    ) -> Option<crate::runtime::RootAdmissionSnapshot> {
        let Some(root_scope_control) = self.run_lifecycle.root_scope_control(generation) else {
            return None;
        };
        Some(root_scope_control.root_admission_snapshot())
    }

    pub(crate) fn last_root_run_epoch(&self) -> u64 {
        self.next_root_run_generation.saturating_sub(1)
    }

    pub(crate) fn pending_permission_confirmation_id(&self) -> Option<u64> {
        self.pending_permission
            .as_ref()
            .map(|pending| pending.confirmation_id)
    }

    pub(crate) fn save_global_config(&mut self, values: Vec<(String, String)>) -> bool {
        let candidate =
            match ConfigEditorState::from_config_values(self.state.global_config(), values) {
                Ok(candidate) => candidate,
                Err(error) => {
                    self.state
                        .set_status_message(format!("config save failed: {error}"));
                    return false;
                }
            };
        let save_result = candidate.save_global(
            &self.app.workspace.root,
            GlobalConfigAdoptionPolicy::StrictCurrentSchema,
        );
        match self.commit_global_config_save_result(save_result) {
            Ok(message) => {
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

    pub(crate) fn finish_initial_setup(
        &mut self,
        values: Vec<(String, String)>,
        expected_import_generation: Option<u64>,
    ) -> bool {
        let values = match self
            .hydrate_initial_setup_import_sensitive_values(values, expected_import_generation)
        {
            Ok(values) => values,
            Err(error) => {
                self.state
                    .set_status_message(format!("initial setup could not finish: {error}"));
                return false;
            }
        };
        let candidate = match ConfigEditorState::from_complete_config_values(
            self.state.global_config(),
            values,
        ) {
            Ok(candidate) => candidate,
            Err(error) => {
                self.state
                    .set_status_message(format!("initial setup could not finish: {error}"));
                return false;
            }
        };
        let candidate = match expected_import_generation.and_then(|generation| {
            self.pending_initial_setup_config_import
                .as_ref()
                .filter(|import| import.generation == generation)
                .and_then(|import| import.config.approve.clone())
        }) {
            Some(approve) => candidate.with_imported_approve(approve),
            None => candidate,
        };
        let save_result = candidate.save_global(
            &self.app.workspace.root,
            GlobalConfigAdoptionPolicy::StrictCurrentSchema,
        );
        let message = match self.commit_global_config_save_result(save_result) {
            Ok(message) => message,
            Err(error) => {
                self.state
                    .set_status_message(format!("initial setup could not finish: {error}"));
                return false;
            }
        };
        if let Err(error) = self.persist_onboarding_intent(None) {
            self.state
                .set_status_message(format!("初回案内を保存できませんでした: {error}"));
            return false;
        }
        self.pending_initial_setup_config_import = None;
        self.state.complete_initial_setup_after_persist();
        if self.state.startup.requires_initial_setup() {
            self.state.set_status_message(
                "initial setup still has a local validation error; review the highlighted step",
            );
            return false;
        }
        self.state
            .set_status_message(format!("initial setup completed; {message}"));
        true
    }

    pub(crate) fn prepare_root_session_settings_persistence(
        &mut self,
        expected_settings_revision: u64,
        patch: SessionSettingsPatch,
    ) -> Result<Option<RootSessionSettingsPersistence>, RootSessionSettingsApplyError> {
        let session_id = self.state.app_state.current_session_id.ok_or_else(|| {
            RootSessionSettingsApplyError::Internal(
                "session settings require a current root session".to_string(),
            )
        })?;
        let current = self
            .state
            .open_session
            .as_ref()
            .filter(|open_session| open_session.session_id() == session_id)
            .map(|open_session| open_session.session().clone())
            .ok_or_else(|| {
                RootSessionSettingsApplyError::Internal(
                    "session settings owner is no longer loaded".to_string(),
                )
            })?;
        if current.session_settings_revision != expected_settings_revision {
            return Ok(None);
        }

        let access_only = patch.access_mode.is_some()
            && patch.cwd.is_none()
            && patch.model.is_none()
            && patch.base_url.is_none()
            && !patch.reset_model_parameters
            && patch.context_window.is_none();
        let config_generation_delta = self
            .state
            .root_session_settings_config_generation_delta(&current, &patch);
        Ok(Some(RootSessionSettingsPersistence {
            app: self.app.clone(),
            session_id,
            expected_settings_revision,
            current_access_mode: current.access_mode,
            patch,
            access_only,
            config_generation_delta,
        }))
    }

    pub(crate) fn settle_root_session_settings_persistence(
        &mut self,
        result: RootSessionSettingsPersistenceResult,
    ) -> bool {
        let update = result.update;
        if !self
            .state
            .apply_persisted_root_session_record(update.session.clone())
        {
            return false;
        }
        self.state.view.overlay = super::state::DesktopOverlay::SessionSettings;
        self.state.set_status_message(if update.changed {
            if result.access_only {
                "承認方法を保存しました。次の承認判断から適用されます。表示中の承認依頼は変更しません。"
            } else {
                "チャットの設定を保存しました。AIの接続先とモデルは次の依頼から適用されます。"
            }
        } else {
            "チャットの設定は保存済みの内容と同じです。"
        });
        true
    }

    fn reload_config(&mut self) -> bool {
        self.reload_config_with_status_code(DesktopStatusCode::Plain)
    }

    fn reload_config_with_status_code(&mut self, failure_code: DesktopStatusCode) -> bool {
        match ConfigLoader::load(&self.app.workspace.root, None) {
            Ok(config) => {
                self.app.config = config.clone();
                self.adopt_global_config_without_network(config);
                true
            }
            Err(error) => {
                self.state.set_typed_status_message(
                    failure_code,
                    format!("failed to reload config: {error}"),
                );
                false
            }
        }
    }

    pub(crate) fn prepare_image_attachment_from_input(&self) -> Result<Utf8PathBuf, String> {
        let input = self
            .state
            .composer
            .image_attachment_input
            .trim()
            .to_string();
        normalize_image_attachment_path(&self.app.workspace.cwd, &input)
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

    fn provider_selection_patch(
        &mut self,
        baseline: &ResolvedConfig,
        input_matches_baseline: bool,
    ) -> Option<PartialResolvedConfig> {
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
        let baseline_model = &baseline.model;
        let connection_target_changed = normalize_provider_base_url(&baseline_model.base_url)
            != base_url
            || baseline_model.provider_profile != self.state.provider_config.provider_profile_input;
        let limit_only_update =
            input_matches_baseline && context_window != baseline_model.context_window;
        let mut hydrated_model_config = baseline_model.clone();
        hydrated_model_config.base_url = base_url.clone();
        hydrated_model_config.model = model.clone();
        hydrated_model_config.provider_profile = self.state.provider_config.provider_profile_input;
        hydrated_model_config.api_key_env =
            non_empty_trimmed_owned(&self.state.provider_config.provider_api_key_env_input);
        if connection_target_changed {
            hydrated_model_config.extra_headers.clear();
        }
        if self.state.provider_catalog_owns_current_target()
            && !limit_only_update
            && let Some(info) = self.state.selected_provider_model_info()
        {
            apply_provider_model_info_to_config(&mut hydrated_model_config, info);
        }
        hydrated_model_config.context_window = context_window;
        Some(PartialResolvedConfig {
            model: Some(PartialModelConfig {
                base_url: Some(base_url),
                model: Some(model),
                provider_profile: Some(hydrated_model_config.provider_profile),
                api_key_env: Some(hydrated_model_config.api_key_env.clone()),
                extra_headers: Some(hydrated_model_config.extra_headers.clone()),
                context_window: Some(hydrated_model_config.context_window),
                supports_tools: Some(hydrated_model_config.supports_tools),
                supports_images: Some(hydrated_model_config.supports_images),
                parallel_tool_calls: Some(hydrated_model_config.parallel_tool_calls),
                max_parallel_predictions: Some(hydrated_model_config.max_parallel_predictions),
                ..PartialModelConfig::default()
            }),
            ..PartialResolvedConfig::default()
        })
    }

    fn apply_provider_selection_to_effective_config(&mut self) -> Option<ResolvedConfig> {
        let baseline = self.state.provider_config.effective_config.clone();
        let input_matches_baseline = self.state.provider_input_matches_effective_target();
        let patch = self.provider_selection_patch(&baseline, input_matches_baseline)?;
        Some(apply_config_patch(baseline, patch))
    }

    fn apply_provider_selection_to_global_config(&mut self) -> Option<ResolvedConfig> {
        let baseline = self.state.global_config().clone();
        let input_matches_baseline = self.state.provider_input_matches_global_target();
        let patch = self.provider_selection_patch(&baseline, input_matches_baseline)?;
        Some(apply_config_patch(baseline, patch))
    }

    fn provider_config_persistence_candidate(
        &self,
        config: &ResolvedConfig,
    ) -> Result<ConfigEditorState, String> {
        ConfigEditorState::from_config_values(
            self.state.global_config(),
            vec![
                (
                    ConfigField::BaseUrl.label().to_string(),
                    config.model.base_url.clone(),
                ),
                (
                    ConfigField::Model.label().to_string(),
                    config.model.model.clone(),
                ),
                (
                    ConfigField::ProviderProfile.label().to_string(),
                    config.model.provider_profile.as_str().to_string(),
                ),
                (
                    ConfigField::ApiKeyEnv.label().to_string(),
                    config.model.api_key_env.clone().unwrap_or_default(),
                ),
                (
                    ConfigField::ExtraHeadersJson.label().to_string(),
                    ConfigField::ExtraHeadersJson.editor_value(config),
                ),
                (
                    ConfigField::ContextWindow.label().to_string(),
                    config.model.context_window.to_string(),
                ),
                (
                    ConfigField::SupportsTools.label().to_string(),
                    config.model.supports_tools.to_string(),
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
            ],
        )
    }

    fn persist_onboarding_intent(
        &mut self,
        intent: Option<DesktopOnboardingIntent>,
    ) -> Result<(), String> {
        let mut candidate = self.preferences.clone();
        candidate.onboarding_intent = intent;
        if self.persist_preferences_to_disk {
            candidate.save()?;
        }
        self.preferences = candidate;
        Ok(())
    }

    pub(crate) fn choose_initial_setup_purpose(
        &mut self,
        intent: DesktopOnboardingIntent,
    ) -> Result<(), String> {
        self.persist_onboarding_intent(Some(intent))?;
        // Choosing a route does not replace the config draft/import owner.
        self.state.startup.onboarding_intent = Some(intent);
        self.show_onboarding_surface(intent);
        Ok(())
    }

    fn show_onboarding_surface(&mut self, intent: DesktopOnboardingIntent) {
        if matches!(
            intent,
            DesktopOnboardingIntent::Team | DesktopOnboardingIntent::Execution
        ) && self.state.show_shared_work()
            && intent == DesktopOnboardingIntent::Execution
        {
            // The shared shell can connect before local AI setup is complete. Keep the
            // unfinished config owner until the Hub file is actually saved.
            self.state.show_hub_editor();
        }
    }

    fn persist_preferences(&mut self) {
        self.preferences.window_opacity_percent = Some(self.state.view.window_opacity_percent);
        self.preferences.last_workspace = self.workspace_path_for_preferences();
        self.preferences.last_session_id = self
            .preferences
            .last_workspace
            .as_ref()
            .and(self.state.app_state.current_session_id);
        if !self.persist_preferences_to_disk {
            return;
        }
        if let Err(error) = self.preferences.save() {
            self.state
                .set_status_message(format!("failed to save desktop preferences: {error}"));
        }
    }

    fn workspace_path_for_preferences(&self) -> Option<Utf8PathBuf> {
        (!self.is_quick_chat_workspace()).then(|| self.app.workspace.authority_root().to_path_buf())
    }

    fn is_quick_chat_workspace(&self) -> bool {
        is_quick_chat_workspace_path(&self.app.workspace.root)
    }

    pub(crate) fn answer_permission(
        &mut self,
        confirmation_id: u64,
        decision: ReviewDecision,
    ) -> PendingPermissionResolution {
        match resolve_pending_permission(&mut self.pending_permission, confirmation_id, decision) {
            PendingPermissionResolution::NotCurrent => PendingPermissionResolution::NotCurrent,
            PendingPermissionResolution::AlreadyTerminal(cause) => {
                self.state
                    .set_status_message(crate::tui::state::run_cancellation_status_message(&cause));
                PendingPermissionResolution::AlreadyTerminal(cause)
            }
            PendingPermissionResolution::AlreadySettled => {
                self.state
                    .set_status_message("permission request was already settled");
                PendingPermissionResolution::AlreadySettled
            }
            PendingPermissionResolution::Resolved => {
                self.state.set_status_message(
                    crate::tui::state::permission_decision_pending_status_message(),
                );
                PendingPermissionResolution::Resolved
            }
            PendingPermissionResolution::Failed(cause) => {
                self.state
                    .set_status_message(crate::tui::state::run_cancellation_status_message(&cause));
                PendingPermissionResolution::Failed(cause)
            }
        }
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

    fn settle_pending_permission_after_root_finish(&mut self) {
        if !preserve_permission_after_root_finish(self.pending_permission.as_ref()) {
            self.pending_permission = None;
        }
    }

    /// Close local admission once and let the existing Stop owners settle their
    /// durable turns. Reuse successful drains; retry failed stops at current owners.
    pub(crate) fn begin_exit(&mut self) -> Result<(), String> {
        if let Some(drain) = &self.exit_drain {
            if matches!(
                drain,
                DesktopExitDrain::Running { .. } | DesktopExitDrain::Completed(Ok(()))
            ) {
                return Ok(());
            }
        }
        // Presence is the admission fence; the receipt below owns the outcome.
        self.exit_drain = Some(DesktopExitDrain::Completed(Ok(())));
        self.state.cancel_prompt_review();
        for run in self.side_chat_runs.values_mut() {
            run.phase = "stop requested".to_string();
            run.cancel.cancel();
        }
        if let Some(generation) = self.run_lifecycle.root_generation()
            && !self.run_lifecycle.cancellation_requested()
            && !self.cancel_root_run_at_generation(generation)
        {
            let error = "実行中の依頼に停止を要求できませんでした。".to_string();
            self.exit_drain = Some(DesktopExitDrain::Completed(Err(error.clone())));
            return Err(error);
        }
        let runtime = self.app.process_runtime.agent_runtime();
        let (sender, receiver) = mpsc::channel();
        let worker = self.root_task_runtime.spawn(0, move || async move {
            let result = runtime.stop_process_trees().await;
            let _ = sender.send(result);
        });
        let worker = match worker {
            Ok(worker) => worker,
            Err(error) => {
                self.exit_drain = Some(DesktopExitDrain::Completed(Err(error.clone())));
                return Err(error);
            }
        };
        self.exit_drain = Some(DesktopExitDrain::Running { worker, receiver });
        Ok(())
    }

    /// Poll with a short controller lock; the root and side workers publish
    /// their normal FIFO settlement rather than being abandoned at process exit.
    pub(crate) fn local_exit_settled(&mut self) -> Result<bool, String> {
        self.drain_runtime_messages();
        self.prompt_review_workers
            .retain(|worker| !worker.is_finished());
        let drain = self
            .exit_drain
            .as_ref()
            .ok_or_else(|| "このPCの仕事の停止処理を開始できていません。".to_string())?;
        if let DesktopExitDrain::Running { worker, receiver } = drain {
            if !worker.is_finished() {
                return Ok(false);
            }
            let result = receiver
                .try_recv()
                .unwrap_or_else(|_| Err("このPCの仕事の停止結果を確認できません。".to_string()));
            if let Some(DesktopExitDrain::Running { worker, .. }) =
                self.exit_drain.replace(DesktopExitDrain::Completed(result))
            {
                worker.detach();
            }
        }
        if let Some(DesktopExitDrain::Completed(Err(error))) = &self.exit_drain {
            return Err(error.clone());
        }
        Ok(!self.run_lifecycle.root_is_active()
            && self.side_chat_runs.is_empty()
            && self.prompt_review_workers.is_empty())
    }

    pub(crate) fn cancel_exact_turn_at(
        &mut self,
        expected_turn_id: TurnId,
        expected_admission_revision: u64,
    ) {
        let Some(session_id) = self.state.app_state.current_session_id else {
            self.state
                .set_status_message("停止できる実行中タスクはありません。");
            return;
        };
        self.state.mark_post_run_refresh_pending();
        self.spawn_session_cancel_persist(
            session_id,
            ActiveTurnExpectation::Turn {
                turn_id: expected_turn_id,
                revision: expected_admission_revision,
            },
        );
        self.state
            .set_status_message("validating the exact task Stop target...");
    }

    pub(crate) fn set_window_opacity_percent(&mut self, percent: i32) {
        self.state.set_window_opacity_percent(percent);
        self.persist_preferences();
    }

    fn advance_composer_commit_generation(&mut self) {
        self.composer_commit_generation = self.composer_commit_generation.saturating_add(1);
    }

    fn root_submission_owner_workspace_path(&self) -> Utf8PathBuf {
        self.app.workspace.authority_root().to_path_buf()
    }

    fn prompt_review_target(&self, request_id: u64) -> Option<PromptReviewTarget> {
        self.state
            .app_state
            .prompt_review
            .as_ref()
            .filter(|review| review.request_id == request_id)?;
        Some(PromptReviewTarget {
            request_id,
            workspace_root: self.app.workspace.root.clone(),
            composer_workspace_path: self.state.snapshot.workspace_path.clone(),
            composer_session_id: self.state.app_state.current_session_id,
            composer_owner_generation: self.state.composer.owner_generation(),
            expected_active_turn: self.state.prompt_review_expected_active_turn(request_id)?,
        })
    }

    fn cancel_prompt_review_if_current(&mut self, target: &PromptReviewTarget) -> bool {
        if target.workspace_root != self.app.workspace.root
            || target.composer_owner_generation != self.state.composer.owner_generation()
            || self
                .state
                .prompt_review_expected_active_turn(target.request_id)
                != Some(target.expected_active_turn)
            || !self
                .state
                .composer
                .is_owned_by(&target.composer_workspace_path, target.composer_session_id)
        {
            return false;
        }
        self.state
            .cancel_prompt_review_if_current(target.request_id)
    }

    fn commit_pending_root_submission(
        &mut self,
        run_generation: u64,
        user_turn: Option<&crate::protocol::UserTurn>,
    ) -> bool {
        let Some(pending) = self
            .pending_root_submission
            .take_if(|pending| pending.run_generation == run_generation)
        else {
            return false;
        };
        if let Some(target) = pending.prompt_review_to_cancel.as_ref() {
            self.cancel_prompt_review_if_current(target);
        }
        self.state
            .apply_durable_prompt_dispatch(&pending.prompt_dispatch, user_turn);
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
        if let Some(session_id) = pending.owner_session_id {
            self.edit_image_drafts.remove(&session_id);
        }
        self.state.composer.retained_images.clear();
        self.advance_composer_commit_generation();
        true
    }

    fn discard_pending_root_submission(&mut self, run_generation: u64) {
        let _ = self
            .pending_root_submission
            .take_if(|pending| pending.run_generation == run_generation);
    }

    fn launch_active_turn_steer(
        &mut self,
        prompt: String,
        expected_active_turn: ActiveTurnExpectation,
    ) -> bool {
        if !matches!(expected_active_turn, ActiveTurnExpectation::Turn { .. }) {
            self.state
                .set_status_message("the captured session is not an active turn to steer");
            return false;
        }
        let Some(session_id) = self.state.app_state.current_session_id else {
            self.state
                .set_status_message("実行中のセッションが見つからないため steer できません。");
            return false;
        };
        if self.state.steer_submission_pending() {
            self.state
                .set_status_message("前の追加入力を保存しています。完了後に再度送信してください。");
            return false;
        }
        let image_paths = self.state.composer.image_attachment_paths.clone();
        let target = SteerSubmissionTarget {
            operation_id: self.state.begin_steer_submission(),
            workspace_root: self.app.workspace.root.clone(),
            session_id,
            expected_active_turn,
        };
        self.state
            .set_status_message("実行中の turn に追加入力を保存しています。");
        let run_service = self.app.run_service.clone();
        let control_tx = self.control_tx.clone();
        let next_permission_request_id = self.next_permission_request_id.clone();
        let cwd = self.app.workspace.cwd.clone();
        let worker_target = target.clone();
        let worker_image_paths = image_paths.clone();
        let steer_client_message_id = format!("desktop-steer-{}", target.operation_id.get());
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut renderer = DesktopSteerRenderer;
                let mut prompt_ui = DesktopConfirmationPrompt {
                    control: control_tx.clone(),
                    next_permission_request_id,
                };
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())?;
                runtime
                    .block_on(async move {
                        run_service
                            .execute(
                                AppCommand::SessionSteer(SessionSteerRequest {
                                    session_id,
                                    prompt,
                                    cwd,
                                    image_paths: worker_image_paths,
                                    client_user_message_id: Some(steer_client_message_id),
                                    expected_active_turn,
                                }),
                                &mut renderer,
                                &mut prompt_ui,
                            )
                            .await
                    })
                    .map_err(|error| error.to_string())
                    .and_then(|outcome| match outcome {
                        AppCommandOutcome::ControlCompleted => Ok(()),
                        AppCommandOutcome::Turn(_) => {
                            Err("steer command unexpectedly returned a terminal turn".to_string())
                        }
                    })
            }))
            .unwrap_or_else(|_| Err("desktop steer worker panicked".to_string()));
            let _ = control_tx.send(RuntimeMessage::SteerFinished {
                target: worker_target,
                image_paths,
                result,
            });
        });
        true
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
        let visible_files = Vec::new();
        EditorContext {
            active_file: visible_files.first().cloned(),
            open_tabs: visible_files.clone(),
            visible_files,
            shell_family,
            current_time_ms: SystemClock::now_ms(),
        }
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
            && self.state.provider_config.provider_profile_input == target.profile
            && non_empty_trimmed_owned(&self.state.provider_config.provider_api_key_env_input)
                == target.api_key_env
            && self.state.provider_config.config_generation == target.config_generation
            && self.state.provider_config.provider_selected_model_id_input
                == target.selected_model_id
    }

    fn docling_readiness_target_is_current(&self, target: &DoclingReadinessRequestTarget) -> bool {
        if self.state.provider_config.config_generation != target.config_generation {
            return false;
        }
        match target.owner {
            DoclingReadinessConfigOwner::EffectiveConfig => {
                self.state.provider_config.effective_config.docling.enabled
                    && crate::config::model::canonical_docling_base_url(
                        &self.state.provider_config.effective_config.docling.base_url,
                    )
                    .is_ok_and(|base_url| base_url == target.base_url)
            }
            DoclingReadinessConfigOwner::InitialSetupDraft => {
                self.state.startup.requires_initial_setup()
            }
        }
    }

    pub(crate) fn drain_runtime_messages(&mut self) -> bool {
        let mut changed = false;
        for _ in 0..DESKTOP_RUNTIME_DRAIN_BUDGET {
            let message = self
                .control_rx
                .try_recv()
                .ok()
                .or_else(|| self.runtime_rx.try_recv().ok());
            let Some(message) = message else {
                break;
            };
            changed = true;
            let _contract = message.async_contract();
            match message {
                RuntimeMessage::CanonicalSessionEvent {
                    listener_generation,
                    target,
                    event,
                } => {
                    self.apply_canonical_session_event(listener_generation, target, event);
                }
                RuntimeMessage::RunEvent {
                    run_generation,
                    event,
                } => {
                    self.apply_root_run_event(run_generation, event);
                }
                RuntimeMessage::Finished {
                    run_generation,
                    result,
                } => {
                    self.settle_root_finished(run_generation, result);
                }
                RuntimeMessage::ControlFinished {
                    run_generation,
                    result,
                } => self.settle_control_finished(run_generation, result),
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
                            "desktop replaced unresolved permission confirmation {} with {}",
                            previous.confirmation_id, confirmation_id
                        ));
                    }
                }
                RuntimeMessage::PermissionCancelled { confirmation_id } => {
                    clear_cancelled_permission(&mut self.pending_permission, confirmation_id);
                }
                RuntimeMessage::EnhanceFinished {
                    request_id,
                    target,
                    result,
                } => {
                    if target.workspace_root != self.app.workspace.root
                        || target.session_id != self.state.app_state.current_session_id
                        || target.owner_generation != self.state.composer.owner_generation()
                        || target.expected_active_turn != self.current_active_turn_expectation()
                        || self.state.prompt_review_expected_active_turn(request_id)
                            != Some(target.expected_active_turn)
                    {
                        self.state.fail_prompt_enhance(request_id);
                        continue;
                    }
                    match result {
                        Ok(draft) => {
                            if self.state.finish_prompt_enhance(request_id, draft) {
                                self.state.set_status_message("改善案を確認してください。");
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
                RuntimeMessage::SteerFinished {
                    target,
                    image_paths,
                    result,
                } => {
                    if !finish_steer_operation_if_current(
                        &mut self.state,
                        &self.app.workspace.root,
                        &target,
                    ) {
                        continue;
                    }
                    let accepted = finish_steer_submission(&mut self.state, &image_paths, result);
                    if accepted {
                        self.advance_composer_commit_generation();
                        // The acknowledgement says only that the queue owner
                        // accepted the input. Re-read the canonical snapshot:
                        // the runner may already have delivered it before this
                        // callback is processed.
                        self.spawn_latest_live_session_refresh(target.session_id);
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
                    target,
                    reason,
                    result,
                } => self.apply_session_loaded_message(request_id, target, reason, result),
                RuntimeMessage::CurrentSessionRefreshed {
                    request_id,
                    target,
                    purpose,
                    result,
                } => {
                    if let CurrentSessionRefreshPurpose::StopRequestRefresh {
                        root_stop_attempt: Some(attempt),
                        ..
                    } = purpose
                    {
                        self.run_lifecycle.finish_stop_attempt(attempt);
                    }
                    if !self
                        .session_projection_refresh_requests
                        .finish_if_current(request_id, &target)
                        || !self.live_session_target_is_current(&target)
                    {
                        continue;
                    }
                    let root_admission_is_current = match purpose {
                        CurrentSessionRefreshPurpose::Refresh => true,
                        CurrentSessionRefreshPurpose::StopRequestRefresh {
                            root_admission_fence,
                            ..
                        } => root_admission_fence == self.next_root_run_generation,
                    };
                    if !root_admission_is_current {
                        continue;
                    }
                    self.apply_current_session_refreshed_message(
                        target.session_id,
                        purpose,
                        result,
                    );
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
                            self.edit_image_drafts.remove(&session_id);
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
                                    self.state.set_status_message(
                                        "チャットを削除しました。別のチャットを開いています…",
                                    );
                                    let request_id = self.state.begin_session_load(next_session_id);
                                    self.spawn_session_load(
                                        next_session_id,
                                        SessionLoadReason::UserSelection,
                                        request_id,
                                    );
                                } else {
                                    self.start_new_chat_with_global_access();
                                    self.state.set_status_message("チャットを削除しました。");
                                }
                            } else {
                                self.state.set_status_message("チャットを削除しました。");
                            }
                        }
                        Err(error) => self
                            .state
                            .set_status_message(format!("チャットを削除できませんでした: {error}")),
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
                                    self.state.set_status_message(
                                        "チャットをアーカイブしました。別のチャットを開いています…",
                                    );
                                    let request_id = self.state.begin_session_load(next_session_id);
                                    self.spawn_session_load(
                                        next_session_id,
                                        SessionLoadReason::UserSelection,
                                        request_id,
                                    );
                                } else {
                                    self.start_new_chat_with_global_access();
                                    self.state
                                        .set_status_message("チャットをアーカイブしました。");
                                }
                            } else {
                                self.state.set_status_message(if archived {
                                    "チャットをアーカイブしました。"
                                } else {
                                    "チャットを復元しました。"
                                });
                            }
                        }
                        Err(error) => self.state.set_status_message(format!(
                            "チャットのアーカイブ状態を変更できませんでした: {error}"
                        )),
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
                                self.state.load_open_session(&loaded.read);
                                self.reconcile_runtime_listener_with_open_session();
                                if let Some(records) = loaded.agent_activity_records {
                                    self.loaded_agent_activity_records =
                                        Some((loaded.read.session.id, records));
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
                            let session_id = applied.loaded.read.session.id;
                            self.state
                                .replace_snapshot_preserving_current_owner(applied.snapshot);
                            if self.state.app_state.current_session_id == Some(session_id)
                                && !self.session_load_is_blocked_by_active_run()
                            {
                                let loaded = applied.loaded;
                                self.state.load_open_session(&loaded.read);
                                self.reconcile_runtime_listener_with_open_session();
                                if let Some(records) = loaded.agent_activity_records {
                                    self.loaded_agent_activity_records =
                                        Some((loaded.read.session.id, records));
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
                    if let Some(operation_id) = completion.operation_id {
                        let _ = self.state.finish_session_search(operation_id);
                    }
                    if let Some(dispatch) = completion.next_dispatch {
                        self.spawn_session_search(dispatch);
                    }
                    let root_run_active =
                        self.run_lifecycle.root_is_active() || self.state.is_busy();
                    if !apply_session_search_result(
                        &mut self.state,
                        completion.is_latest,
                        root_run_active,
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
                    if !self.session_page_target_is_current(&target) {
                        continue;
                    }
                    match result {
                        Ok(loaded) => {
                            let start = loaded.read.turns.offset.saturating_add(1);
                            let end = loaded
                                .read
                                .turns
                                .offset
                                .saturating_add(loaded.read.turns.items.len());
                            let total = loaded.read.turns.total;
                            if self.state.merge_open_session_history(&loaded.read) {
                                if let Some(records) = loaded.agent_activity_records {
                                    self.loaded_agent_activity_records =
                                        Some((loaded.read.session.id, records));
                                }
                                self.state.set_status_message(format!(
                                    "以前の履歴を読み込みました（全{total}件中 {start}〜{end}件）。"
                                ));
                            } else {
                                self.state.set_status_message(
                                    "earlier history no longer overlaps the open session"
                                        .to_string(),
                                );
                            }
                        }
                        Err(error) => self
                            .state
                            .set_status_message(format!("earlier history load failed: {error}")),
                    }
                }
                RuntimeMessage::LiveSessionRefreshed {
                    request_id,
                    target,
                    result,
                } => {
                    if !self
                        .session_projection_refresh_requests
                        .finish_if_current(request_id, &target)
                        || !self.live_session_target_is_current(&target)
                    {
                        continue;
                    }
                    match result {
                        Ok(loaded) => {
                            let has_more = loaded.read.turns.has_more;
                            self.state.refresh_open_session_projection(&loaded.read);
                            self.reconcile_runtime_listener_with_open_session();
                            if let Some(records) = loaded.agent_activity_records {
                                self.loaded_agent_activity_records =
                                    Some((loaded.read.session.id, records));
                            }
                            let catchup_needed = self
                                .state
                                .open_session
                                .as_ref()
                                .filter(|open_session| {
                                    open_session.session_id() == target.session_id
                                })
                                .is_some_and(|open_session| {
                                    open_session.loaded_turn_end()
                                        < open_session.stored_detail().turn_page_total
                                });
                            if has_more || catchup_needed {
                                self.spawn_latest_live_session_refresh(target.session_id);
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
                            self.edit_image_drafts
                                .retain(|_, (owner, _)| *owner != project_id);
                            let deleted_was_current = self.app.workspace.project_id == project_id;
                            if deleted_was_current
                                && !self.ensure_initial_setup_owner_replacement_admission(
                                    "project deletion completion",
                                )
                            {
                                continue;
                            }
                            self.preferences.mark_project_deleted(&project_root);
                            if deleted_was_current {
                                if !self.replace_workspace_from_load(loaded) {
                                    continue;
                                }
                            } else {
                                self.state
                                    .replace_snapshot_preserving_current_owner(loaded.snapshot);
                                self.persist_preferences();
                            }
                            if deleted_was_current {
                                if let Some(next_session_id) = self.state.selected_session_id() {
                                    self.state.set_status_message(
                                        "プロジェクトを削除しました。別のチャットを開いています…",
                                    );
                                    let request_id = self.state.begin_session_load(next_session_id);
                                    self.spawn_session_load(
                                        next_session_id,
                                        SessionLoadReason::UserSelection,
                                        request_id,
                                    );
                                } else {
                                    self.start_new_chat_with_global_access();
                                    self.state
                                        .set_status_message("プロジェクトを削除しました。");
                                }
                            } else {
                                self.state
                                    .set_status_message("プロジェクトを削除しました。");
                            }
                        }
                        Err(error) => self.state.set_status_message(format!(
                            "プロジェクトを削除できませんでした: {error}"
                        )),
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
                RuntimeMessage::DoclingReadinessChecked {
                    request_id,
                    target,
                    result,
                } => {
                    if !self
                        .docling_readiness_requests
                        .finish_if_current(request_id, &target)
                    {
                        continue;
                    }
                    if !self.docling_readiness_target_is_current(&target) {
                        self.state.cancel_docling_readiness_check();
                        continue;
                    }
                    match result {
                        Ok(result) => self.state.finish_docling_readiness_check(result),
                        Err(error) => self.state.fail_docling_readiness_check(error),
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
                        self.app.workspace.authority_root(),
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
                            .set_status_message(format!("履歴をMarkdownで保存しました: {}", path)),
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
                        Ok(commit),
                        AccessModePersistenceTargetRelation::AdoptedSession(session_id),
                    ) = (&phase, &result, target_relation)
                    {
                        self.spawn_adopted_session_access_persistence(
                            request_id,
                            target.clone(),
                            session_id,
                            commit.remembered_path.clone(),
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
                        if let Ok(commit) = &result {
                            self.pending_access_mode_adoption = Some(PendingAccessModeAdoption {
                                request_id,
                                target,
                                remembered_path: commit.remembered_path.clone(),
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
                    if !target_is_current {
                        continue;
                    }
                    match result {
                        Ok(commit) => {
                            let mut global_config = self.state.global_config().clone();
                            global_config.permissions.access_mode = target.access_mode;
                            self.app.config = global_config.clone();
                            self.state.replace_global_config(global_config);
                            if let Some(session) = commit.session {
                                if Some(session.id) != committed_session_id
                                    || !self.state.apply_persisted_root_session_record(session)
                                {
                                    self.state.set_status_message(
                                        "access mode was saved, but the current root-session owner changed; reopen the chat to refresh settings",
                                    );
                                    continue;
                                }
                            } else if committed_session_id.is_some() {
                                self.state.set_status_message(
                                    "access mode persistence did not return the canonical root-session record; reopen the chat to refresh settings",
                                );
                                continue;
                            }
                            let scope = if committed_session_id.is_some() {
                                "共通設定とこのチャット"
                            } else {
                                "共通設定"
                            };
                            let config_message = format!(
                                "{scope}の承認方法を「{}」に保存しました。次の承認判断から適用されます。表示中の承認依頼は変更しません。",
                                access_mode_display_label(target.access_mode)
                            );
                            self.state.set_status_message(config_message);
                        }
                        Err(error) => {
                            let _ = self.reload_config();
                            if self.state.app_state.current_session_id.is_some() {
                                self.state
                                    .provider_config
                                    .update_access_mode(target.old_effective_access_mode);
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
                RuntimeMessage::WorkspaceSwitchedForNewProjectSession {
                    request_id,
                    requested,
                    result,
                } => {
                    self.apply_new_project_workspace_switched_message(request_id, requested, result)
                }
                RuntimeMessage::SideChatDelta {
                    owner_session_id,
                    side_chat_id,
                    run_generation,
                    delta,
                } => {
                    let Some(run) = self.side_chat_runs.get_mut(&owner_session_id) else {
                        continue;
                    };
                    if !run.owns(&side_chat_id, run_generation) {
                        continue;
                    }
                    run.streamed_text.push_str(&delta);
                }
                RuntimeMessage::SideChatPhase {
                    owner_session_id,
                    side_chat_id,
                    run_generation,
                    phase,
                } => {
                    let Some(run) = self.side_chat_runs.get_mut(&owner_session_id) else {
                        continue;
                    };
                    if !run.owns(&side_chat_id, run_generation) {
                        continue;
                    }
                    run.phase = phase;
                }
                RuntimeMessage::SideChatFinished {
                    owner_session_id,
                    side_chat_id,
                    run_generation,
                    result,
                } => {
                    let owned = self
                        .side_chat_runs
                        .get(&owner_session_id)
                        .is_some_and(|run| run.owns(&side_chat_id, run_generation));
                    if !owned {
                        continue;
                    }
                    if let Some(run) = self.side_chat_runs.remove(&owner_session_id) {
                        let delete_after_finish = run.delete_after_finish;
                        let stop_requested = run.cancel.is_cancelled();
                        run.worker.detach();
                        let repository = self.app.store.side_chat_repo();
                        let durable_delete_requested = repository
                            .get_by_owner(owner_session_id)
                            .ok()
                            .flatten()
                            .is_some_and(|binding| {
                                binding.id.to_string() == side_chat_id
                                    && binding.delete_requested_at_ms.is_some()
                            });
                        if delete_after_finish || durable_delete_requested {
                            match repository.finalize_pending_deletions() {
                                Ok(_) => {
                                    self.side_chat_contexts.remove(&owner_session_id);
                                    self.side_chat_errors.remove(&owner_session_id);
                                }
                                Err(error) => {
                                    self.side_chat_errors.insert(
                                        owner_session_id,
                                        format!(
                                            "side chat stopped, but its history was not deleted: {error}"
                                        ),
                                    );
                                }
                            }
                        } else if let Err(error) = result {
                            self.side_chat_errors.insert(owner_session_id, error);
                        } else if stop_requested {
                            self.side_chat_errors.remove(&owner_session_id);
                        } else {
                            self.side_chat_errors.remove(&owner_session_id);
                        }
                    }
                }
            }
        }
        changed
    }
}

impl Drop for DesktopController {
    fn drop(&mut self) {
        self.state.cancel_prompt_review();
        for run in self.side_chat_runs.values() {
            run.cancel.cancel();
        }
    }
}

fn desktop_run_failure_notification_allowed(cause: Option<&RunCancellationCause>) -> bool {
    !matches!(cause, Some(RunCancellationCause::Interruption(_)))
}

fn publish_desktop_run_finished(
    control: &DesktopControlPlaneSender,
    run_generation: u64,
    result: Result<RunSummary, String>,
) {
    // Finished reports worker quiescence. The bootstrap bridge and this
    // settlement share one FIFO, so SessionStarted/UserTurnStored cannot be
    // overtaken even while the lossy runtime mailbox is saturated.
    let _ = control.send(RuntimeMessage::Finished {
        run_generation,
        result,
    });
}

fn resolve_pending_permission(
    pending: &mut Option<PendingPermission>,
    expected_confirmation_id: u64,
    decision: ReviewDecision,
) -> PendingPermissionResolution {
    resolve_pending_permission_after_take(pending, expected_confirmation_id, decision, |_| {})
}

fn resolve_pending_permission_after_take(
    pending: &mut Option<PendingPermission>,
    expected_confirmation_id: u64,
    decision: ReviewDecision,
    after_take: impl FnOnce(&PendingPermission),
) -> PendingPermissionResolution {
    if pending.as_ref().map(|pending| pending.confirmation_id) != Some(expected_confirmation_id) {
        return PendingPermissionResolution::NotCurrent;
    }
    let Some(pending) = pending.take() else {
        return PendingPermissionResolution::NotCurrent;
    };
    if let Some(cause) = pending.run_control.cause() {
        return PendingPermissionResolution::AlreadyTerminal(cause);
    }
    after_take(&pending);
    if let Err(error) = pending.responder.send(decision) {
        let failure =
            RunCancellationCause::Failure(format!("desktop permission response failed: {error}"));
        return match pending.run_control.request_cancel(failure.clone()) {
            RunCancelOutcome::Applied | RunCancelOutcome::Deferred(_) => {
                PendingPermissionResolution::Failed(failure)
            }
            RunCancelOutcome::Rejected => match pending.run_control.cause() {
                Some(cause) => PendingPermissionResolution::AlreadyTerminal(cause),
                None => PendingPermissionResolution::AlreadySettled,
            },
        };
    }
    PendingPermissionResolution::Resolved
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PendingPermissionResolution {
    Resolved,
    NotCurrent,
    AlreadyTerminal(RunCancellationCause),
    AlreadySettled,
    Failed(RunCancellationCause),
}

fn clear_cancelled_permission(
    pending: &mut Option<PendingPermission>,
    expected_confirmation_id: u64,
) -> bool {
    if pending.as_ref().map(|pending| pending.confirmation_id) != Some(expected_confirmation_id) {
        return false;
    }
    *pending = None;
    true
}

fn preserve_permission_after_root_finish(pending: Option<&PendingPermission>) -> bool {
    pending.is_some_and(|pending| {
        pending.request.agent_path.is_some() && pending.run_control.cause().is_none()
    })
}

async fn claim_exact_root_execution_stop(
    app: &App,
    session_id: SessionId,
    expected_active_turn: ActiveTurnExpectation,
) -> Result<(ExactRootExecutionStopOutcome, bool), String> {
    let ActiveTurnExpectation::Turn { turn_id, revision } = expected_active_turn else {
        return Ok((ExactRootExecutionStopOutcome::TargetChanged, false));
    };
    let outcome = app
        .run_service
        .cancel_exact_root_execution(session_id, turn_id, revision)
        .await
        .map_err(|error| error.to_string())?;
    let local_wake_applied = matches!(
        outcome,
        ExactRootExecutionStopOutcome::Applied { cancelled: true }
    );
    Ok((outcome, local_wake_applied))
}

fn loaded_session_from_detail(
    detail: LoadedSessionDetail,
    agent_activity_records: Option<Vec<AgentActivityRecord>>,
) -> LoadedSession {
    LoadedSession {
        read: detail.read,
        agent_activity_records,
    }
}

async fn loaded_session_from_detail_with_activity(
    app: &App,
    detail: LoadedSessionDetail,
) -> Result<LoadedSession, String> {
    let session_id = detail.read.session.id;
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

async fn load_session_navigation_result(
    app: App,
    session_id: SessionId,
    reason: SessionLoadReason,
) -> Result<SessionNavigationLoadResult, String> {
    let session = app
        .session_service
        .get_session(session_id)
        .await
        .map_err(|error| error.to_string())?;
    let workspace_changed = session.project_id != app.workspace.project_id
        || session.cwd != app.workspace.cwd
        || app.workspace.authority_root() != session.cwd.as_path();
    let aligned_app = if workspace_changed {
        let process_runtime = app.process_runtime.clone();
        let rebuilt =
            AppBootstrap::rebuild_for_session_with_process_runtime(&session, process_runtime)
                .await
                .map_err(|error| {
                    format!(
                        "failed to restore session workspace {} for {}: {error}",
                        session.cwd, session.id
                    )
                })?;
        if rebuilt.workspace.project_id != session.project_id {
            return Err(format!(
                "session {} cwd {} resolves to project {}, not its stored project {}",
                session.id, session.cwd, rebuilt.workspace.project_id, session.project_id
            ));
        }
        rebuilt
    } else {
        app
    };

    let loaded = match reason {
        SessionLoadReason::UserSelection | SessionLoadReason::CreatedEditFork => {
            let detail = load_session_detail(&aligned_app, session_id)
                .await
                .map_err(|error| error.to_string())?;
            loaded_session_from_detail_with_activity(&aligned_app, detail).await?
        }
        SessionLoadReason::RunningRejoin => {
            let rejoin = aligned_app
                .session_service
                .rejoin_running_session(session_id, 0, 200, 0, DESKTOP_TURN_PAGE_LIMIT)
                .await;
            let snapshot = aligned_app
                .session_service
                .canonical_latest_session_snapshot(
                    session_id,
                    DESKTOP_HISTORY_PROJECTION_LIMIT,
                    DESKTOP_TURN_PAGE_LIMIT,
                )
                .await
                .map_err(|error| error.to_string())?;
            if let Err(error) = rejoin
                && snapshot.read.session.status == SessionStatus::Running
            {
                return Err(error.to_string());
            }
            let agent_activity_records = aligned_app
                .run_service
                .durable_agent_activity_records(session_id)
                .await
                .map_err(|error| error.to_string())?;
            LoadedSession {
                read: snapshot.read,
                agent_activity_records: Some(agent_activity_records),
            }
        }
    };

    let snapshot = if !workspace_changed && reason == SessionLoadReason::CreatedEditFork {
        Some(
            load_snapshot_for_selection(&aligned_app, Some(session_id))
                .await
                .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    let workspace = if workspace_changed {
        let snapshot = load_snapshot_for_selection(&aligned_app, Some(session_id))
            .await
            .map_err(|error| error.to_string())?;
        Some(WorkspaceLoadResult {
            app: aligned_app,
            snapshot,
        })
    } else {
        None
    };
    Ok(SessionNavigationLoadResult {
        workspace,
        snapshot,
        loaded,
    })
}

async fn load_stopped_session_operation_projection(
    app: &App,
    session_id: SessionId,
    expected_active_turn: ActiveTurnExpectation,
    confirmation_timeout: std::time::Duration,
) -> Result<DesktopSessionOperationLoaded, String> {
    let ActiveTurnExpectation::Turn { turn_id, revision } = expected_active_turn else {
        return Err("停止する実行が見つかりません。".to_string());
    };
    let deadline = tokio::time::Instant::now() + confirmation_timeout;
    let message = loop {
        // Recording UserStop acknowledges a request, not the external owner's terminal.
        // Retain this operation until that exact admission changes; never resend Stop.
        let detail = load_session_detail(app, session_id)
            .await
            .map_err(|error| error.to_string())?;
        let read = &detail.read;
        if read.active_turn_id != Some(turn_id) || read.admission_revision != revision {
            break if read.active_turn_id.is_some() {
                format!("チャット {session_id} の実行が切り替わりました。新しい実行は継続中です。")
            } else {
                format!("チャット {session_id} の実行が終了しました。")
            };
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break format!(
                "チャット {session_id} の停止を受け付けました。終了は未確認です。一覧を更新して確認してください。"
            );
        }
        tokio::time::sleep_until(deadline.min(now + DESKTOP_SESSION_RUNTIME_POLL_INTERVAL)).await;
    };
    load_session_operation_projection(app, session_id, message).await
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
            read: detail.read,
            agent_activity_records: Some(agent_activity_records),
        },
        message,
    })
}

fn live_event_requires_canonical_refresh(event: &RunEvent) -> bool {
    matches!(
        event,
        RunEvent::UserTurnStored { .. }
            | RunEvent::ModelRequestPrepared { .. }
            | RunEvent::AssistantMessageCommitted { .. }
            | RunEvent::WorldStateUpdated { .. }
            | RunEvent::ToolCallPending { .. }
            | RunEvent::ToolCallCompleted { .. }
            | RunEvent::ToolCallFailed { .. }
            | RunEvent::FileChangesRecorded { .. }
            | RunEvent::CompactionCompleted { .. }
            | RunEvent::PermissionRequested { .. }
            | RunEvent::PermissionResolved { .. }
            | RunEvent::RecoverableRuntimeFeedback { .. }
            | RunEvent::TurnTerminal { .. }
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
    if !file_changes.is_empty()
        && !rows
            .iter()
            .any(|row| row.row_kind == DesktopTranscriptRowKind::FileChanges)
    {
        events.push(MarkdownExportEvent::detail(
            "ファイル変更履歴",
            render_file_change_markdown_lines(file_changes),
        ));
    }
    let metadata = vec![
        MarkdownMetadataLine::new("Workspace", format!("`{workspace}`")),
        MarkdownMetadataLine::new("Session", format!("`{session_id}`")),
        MarkdownMetadataLine::new(
            "Provider",
            format!("`{}`", sanitize_provider_endpoint(provider_base_url)),
        ),
        MarkdownMetadataLine::new("Model", format!("`{model}`")),
    ];
    render_codex_turn_block_markdown(title, &events, &metadata)
}

fn markdown_events_for_transcript_row(row: &DesktopTranscriptRow) -> Vec<MarkdownExportEvent> {
    match row.row_kind {
        DesktopTranscriptRowKind::User => {
            vec![MarkdownExportEvent::user(export_visible_body(&row.body))]
        }
        DesktopTranscriptRowKind::Assistant => vec![MarkdownExportEvent::assistant(
            export_visible_body(&row.body),
        )],
        DesktopTranscriptRowKind::FileChanges => vec![MarkdownExportEvent::detail(
            row.title.clone(),
            transcript_detail_body(row),
        )],
        DesktopTranscriptRowKind::WorkSummaryFailed => vec![
            MarkdownExportEvent::detail(row.title.clone(), transcript_detail_body(row)),
            MarkdownExportEvent::terminal(
                MarkdownTerminalStatus::Failed,
                transcript_terminal_summary(row),
            ),
        ],
        DesktopTranscriptRowKind::WorkSummaryCancelled => vec![
            MarkdownExportEvent::detail(row.title.clone(), transcript_detail_body(row)),
            MarkdownExportEvent::terminal(
                MarkdownTerminalStatus::Interrupted,
                transcript_terminal_summary(row),
            ),
        ],
        DesktopTranscriptRowKind::WorkSummaryCompleted => vec![
            MarkdownExportEvent::detail(row.title.clone(), transcript_detail_body(row)),
            MarkdownExportEvent::terminal(
                MarkdownTerminalStatus::Completed,
                transcript_terminal_summary(row),
            ),
        ],
        DesktopTranscriptRowKind::Tool
        | DesktopTranscriptRowKind::Editing
        | DesktopTranscriptRowKind::Diff => {
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
    match row.row_kind {
        DesktopTranscriptRowKind::FileChanges if !row.file_changes.is_empty() => {
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
    let mut body = match summary.status() {
        SessionStatus::Completed => format!("{session_title} が完了しました。"),
        SessionStatus::Cancelled => format!("{session_title} を停止しました。"),
        SessionStatus::Failed => format!("{session_title} が失敗しました。"),
        SessionStatus::Running => format!("{session_title} は実行中です。"),
        SessionStatus::Idle => format!("{session_title} は待機状態です。"),
    };
    if summary.change_count() > 0 {
        body.push_str(&format!(" 変更: {}件。", summary.change_count()));
    }
    if summary.tool_call_count() > 0 {
        body.push_str(&format!(" ツール: {}件", summary.tool_call_count()));
        if summary.failed_tool_count() > 0 {
            body.push_str(&format!(" / 失敗 {}件", summary.failed_tool_count()));
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
        RunEvent::TurnTerminal { terminal, .. } => match &terminal.outcome {
            crate::protocol::TurnTerminalOutcome::Completed => {
                Some(format!("{session_title} が完了しました。"))
            }
            crate::protocol::TurnTerminalOutcome::Interrupted { cause } => {
                let visible_reason = cause
                    .summary()
                    .lines()
                    .next()
                    .unwrap_or_else(|| cause.summary())
                    .trim();
                if visible_reason.is_empty() {
                    Some(format!("{session_title} を停止しました。"))
                } else {
                    Some(format!("{session_title} を停止しました: {visible_reason}"))
                }
            }
            crate::protocol::TurnTerminalOutcome::Failed { error } => {
                let visible_error = error.lines().next().unwrap_or(error).trim();
                if visible_error.is_empty() {
                    Some(format!("{session_title} が失敗しました。"))
                } else {
                    Some(format!("{session_title} が失敗しました: {visible_error}"))
                }
            }
        },
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
pub(super) fn send_windows_desktop_notification(title: &str, body: &str) {
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
pub(super) fn send_windows_desktop_notification(_title: &str, _body: &str) {}

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
            append_notification_debug_log(&format!(
                "native balloon result={result} title={title:?} body={body:?}"
            ));
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
    provider_profile: crate::config::ProviderProfile,
    api_key_env: Option<String>,
) -> ResolvedConfig {
    let connection_target_changed = normalize_provider_base_url(&config.model.base_url)
        != normalize_provider_base_url(&base_url)
        || config.model.provider_profile != provider_profile;
    if connection_target_changed {
        config.model.extra_headers.clear();
        config.model.extra_body_json = None;
    }
    config.model.base_url = base_url;
    config.model.provider_profile = provider_profile;
    config.model.api_key_env = api_key_env;
    config.model.clear_legacy_generation_settings();
    config
}

fn non_empty_trimmed_owned(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
mod tests;

fn spawn_desktop_session_runtime_listener(
    app: App,
    runtime_tx: mpsc::SyncSender<RuntimeMessage>,
    listener_generation: u64,
    target: SessionRuntimeListenerTarget,
    cancel: CancellationToken,
) {
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build desktop session-listener runtime");
        runtime.block_on(run_desktop_session_runtime_listener(
            app,
            runtime_tx,
            listener_generation,
            target,
            cancel,
        ));
    });
}

async fn run_desktop_session_runtime_listener(
    app: App,
    runtime_tx: mpsc::SyncSender<RuntimeMessage>,
    listener_generation: u64,
    target: SessionRuntimeListenerTarget,
    cancel: CancellationToken,
) {
    let event_store = app.store.protocol_event_store();
    let mut subscription = app.subscribe_session_runtime_events(target.session_id);
    let mut cursor = loop {
        match event_store.latest_runtime_event_page_for_session(
            target.session_id,
            crate::protocol::MAX_PROTOCOL_PAGE_LIMIT,
        ) {
            Ok(page) => {
                let latest_target_event = page
                    .items
                    .iter()
                    .rev()
                    .find(|event| event.turn_id == target.turn_id)
                    .cloned()
                    .into_iter()
                    .collect::<Vec<_>>();
                loop {
                    match deliver_desktop_session_runtime_events(
                        &runtime_tx,
                        listener_generation,
                        &target,
                        &latest_target_event,
                    ) {
                        DesktopSessionEventDelivery::Complete => break,
                        DesktopSessionEventDelivery::Terminal
                        | DesktopSessionEventDelivery::Disconnected => return,
                        DesktopSessionEventDelivery::MailboxFull => {
                            tokio::select! {
                                _ = cancel.cancelled() => return,
                                _ = tokio::time::sleep(DESKTOP_SESSION_RUNTIME_POLL_INTERVAL) => {}
                            }
                        }
                    }
                }
                break page.next_cursor;
            }
            Err(_) => {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(DESKTOP_SESSION_RUNTIME_POLL_INTERVAL) => {}
                }
            }
        }
    };

    loop {
        let wake = tokio::select! {
            _ = cancel.cancelled() => return,
            wake = tokio::time::timeout(
                DESKTOP_SESSION_RUNTIME_POLL_INTERVAL,
                subscription.recv(),
            ) => wake,
        };
        if matches!(wake, Ok(Err(_))) {
            // A bounded broadcast is only a wake-up path. If it lags or closes,
            // replace it and replay the canonical append stream from the last
            // durable cursor instead of making the listener permanently fail.
            subscription.resubscribe();
        }

        for _ in 0..DESKTOP_SESSION_RUNTIME_CURSOR_DRAIN_BUDGET {
            let page = match event_store.runtime_event_cursor_page_for_session(
                target.session_id,
                cursor,
                DESKTOP_SESSION_RUNTIME_CURSOR_PAGE_LIMIT,
            ) {
                Ok(page) => page,
                Err(_) => break,
            };
            let page_len = page.items.len();
            let next_cursor = page.next_cursor;
            match deliver_desktop_session_runtime_events(
                &runtime_tx,
                listener_generation,
                &target,
                &page.items,
            ) {
                DesktopSessionEventDelivery::Complete => {}
                DesktopSessionEventDelivery::Terminal
                | DesktopSessionEventDelivery::Disconnected => return,
                DesktopSessionEventDelivery::MailboxFull => {
                    // Keep the durable cursor unchanged. The next bounded read
                    // replays the same canonical page, including any terminal
                    // that did not fit in the Desktop mailbox.
                    break;
                }
            }
            if let Some(next_cursor) = next_cursor {
                if cursor.is_some_and(|cursor| next_cursor <= cursor) {
                    break;
                }
                cursor = Some(next_cursor);
            }
            if page_len < DESKTOP_SESSION_RUNTIME_CURSOR_PAGE_LIMIT {
                break;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DesktopSessionEventDelivery {
    Complete,
    Terminal,
    MailboxFull,
    Disconnected,
}

fn deliver_desktop_session_runtime_events(
    runtime_tx: &mpsc::SyncSender<RuntimeMessage>,
    listener_generation: u64,
    target: &SessionRuntimeListenerTarget,
    events: &[RuntimeEvent],
) -> DesktopSessionEventDelivery {
    for event in events {
        if event.session_id != target.session_id || event.turn_id != target.turn_id {
            continue;
        }
        let terminal = event.is_terminal();
        match runtime_tx.try_send(RuntimeMessage::CanonicalSessionEvent {
            listener_generation,
            target: target.clone(),
            event: event.clone(),
        }) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                return DesktopSessionEventDelivery::MailboxFull;
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                return DesktopSessionEventDelivery::Disconnected;
            }
        }
        if terminal {
            return DesktopSessionEventDelivery::Terminal;
        }
    }
    DesktopSessionEventDelivery::Complete
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DesktopRunEventDelivery {
    RuntimeMailbox,
    BootstrapControl,
    CanonicalCursor,
}

fn desktop_run_event_delivery(event: &RunEvent) -> DesktopRunEventDelivery {
    match event.durability() {
        RunEventDurability::RuntimeOnly => DesktopRunEventDelivery::RuntimeMailbox,
        RunEventDurability::Committed => match event {
            // Temporary bootstrap bridge: a new run needs these identities
            // before a session-scoped canonical cursor can be attached. The
            // durable admission receipt will replace this bridge.
            RunEvent::SessionStarted { .. } | RunEvent::UserTurnStored { .. } => {
                DesktopRunEventDelivery::BootstrapControl
            }
            _ => DesktopRunEventDelivery::CanonicalCursor,
        },
    }
}

#[derive(Debug, Clone)]
enum DesktopControlResult {
    GoalGet(ThreadGoalGetResult),
    GoalSet(ThreadGoalSetResult),
    GoalClear(ThreadGoalClearResult),
}

impl DesktopControlResult {
    fn description(&self) -> String {
        match self {
            Self::GoalGet(result) => result
                .goal
                .as_ref()
                .map(goal_control_description)
                .unwrap_or_else(|| "このセッションには Goal が設定されていません。".to_string()),
            Self::GoalSet(result) => goal_control_description(&result.goal),
            Self::GoalClear(result) => {
                if result.cleared {
                    "このセッションの Goal を削除しました。".to_string()
                } else {
                    "このセッションには Goal が設定されていません。".to_string()
                }
            }
        }
    }
}

fn goal_control_description(goal: &crate::session::ThreadGoal) -> String {
    use crate::session::ThreadGoalStatus;
    let status = match goal.status {
        ThreadGoalStatus::Active => "進行中",
        ThreadGoalStatus::Paused => "一時停止",
        ThreadGoalStatus::Blocked => "継続待ち",
        ThreadGoalStatus::UsageLimited => "利用上限に到達",
        ThreadGoalStatus::BudgetLimited => "予算上限に到達",
        ThreadGoalStatus::Complete => "完了",
    };
    format!(
        "Goal: {status}\n{}\n\n使用トークン: {} / {}\n経過時間: {} 秒",
        goal.objective,
        goal.tokens_used,
        goal.token_budget
            .map(|value| value.to_string())
            .unwrap_or_else(|| "上限なし".to_string()),
        goal.time_used_seconds
    )
}

struct DesktopRenderer {
    runtime_tx: mpsc::SyncSender<RuntimeMessage>,
    bootstrap_control: DesktopControlPlaneSender,
    run_generation: u64,
    notification_title: String,
    notified_terminal: bool,
    control_result: Option<DesktopControlResult>,
}

impl EventRenderer for DesktopRenderer {
    fn render_thread_goal_get(
        &mut self,
        result: &ThreadGoalGetResult,
    ) -> Result<(), CliRenderError> {
        self.control_result = Some(DesktopControlResult::GoalGet(result.clone()));
        Ok(())
    }

    fn render_thread_goal_set(
        &mut self,
        result: &ThreadGoalSetResult,
    ) -> Result<(), CliRenderError> {
        self.control_result = Some(DesktopControlResult::GoalSet(result.clone()));
        Ok(())
    }

    fn render_thread_goal_clear(
        &mut self,
        result: &ThreadGoalClearResult,
    ) -> Result<(), CliRenderError> {
        self.control_result = Some(DesktopControlResult::GoalClear(result.clone()));
        Ok(())
    }

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
        match desktop_run_event_delivery(event) {
            DesktopRunEventDelivery::RuntimeMailbox => {
                match self.runtime_tx.try_send(RuntimeMessage::RunEvent {
                    run_generation: self.run_generation,
                    event: event.clone(),
                }) {
                    Ok(()) | Err(mpsc::TrySendError::Full(_)) => Ok(()),
                    Err(mpsc::TrySendError::Disconnected(_)) => Err(CliRenderError::Message(
                        "desktop runtime stream is unavailable".to_string(),
                    )),
                }
            }
            DesktopRunEventDelivery::BootstrapControl => self
                .bootstrap_control
                .send(RuntimeMessage::RunEvent {
                    run_generation: self.run_generation,
                    event: event.clone(),
                })
                .map_err(CliRenderError::Message),
            DesktopRunEventDelivery::CanonicalCursor => Ok(()),
        }
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

struct DesktopConfirmationPrompt {
    control: DesktopControlPlaneSender,
    next_permission_request_id: Arc<AtomicU64>,
}

impl ConfirmationPrompt for DesktopConfirmationPrompt {
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
        self.control
            .send(RuntimeMessage::Permission {
                confirmation_id,
                request: request.clone(),
                response: response_tx,
                run_control: control.clone(),
            })
            .map_err(|error| {
                control.fail(error.clone());
                CliPromptError::Message(error)
            })?;
        send_windows_desktop_notification(
            "moyAI — 承認が必要です",
            "実行する操作の承認を待っています。moyAIでコマンドと対象を確認してください。",
        );
        loop {
            match response_rx.recv_timeout(std::time::Duration::from_millis(25)) {
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
                        .control
                        .send(RuntimeMessage::PermissionCancelled { confirmation_id });
                    return Ok(ConfirmationOutcome::Interrupted);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if control.is_cancelled() {
                        let _ = self
                            .control
                            .send(RuntimeMessage::PermissionCancelled { confirmation_id });
                        return Ok(ConfirmationOutcome::Interrupted);
                    }
                    let message = "desktop permission response channel disconnected".to_string();
                    control.fail(message.clone());
                    return Err(CliPromptError::Message(message));
                }
            }
        }
    }
}

fn run_event_is_terminal(event: &RunEvent) -> bool {
    matches!(event, RunEvent::TurnTerminal { .. })
}

fn desktop_terminal_status_message(event: &RunEvent) -> Option<String> {
    match event {
        RunEvent::TurnTerminal { terminal, .. } => terminal
            .interruption_cause()
            .map(crate::tui::state::interruption_status_message),
        _ => None,
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
fn pick_config_toml_file(start_dir: Option<&Utf8Path>) -> Result<Option<Utf8PathBuf>, String> {
    let mut dialog = rfd::FileDialog::new().add_filter("moyAI TOML config", &["toml"]);
    if let Some(directory) = start_dir {
        dialog = dialog.set_directory(directory.as_std_path());
    }
    match dialog.pick_file() {
        Some(path) => Utf8PathBuf::from_path_buf(path)
            .map(Some)
            .map_err(|_| "selected config path is not valid UTF-8".to_string()),
        None => Ok(None),
    }
}

#[cfg(not(feature = "tauri-desktop"))]
fn pick_config_toml_file(_start_dir: Option<&Utf8Path>) -> Result<Option<Utf8PathBuf>, String> {
    Err("desktop config picker requires the tauri-desktop feature".to_string())
}

fn import_global_config_toml(source: &Utf8Path) -> Result<String, String> {
    let target = global_config_path().map_err(|error| error.to_string())?;
    import_global_config_toml_to(source, &target)?;
    Ok(format!("imported config.toml to {}", target))
}

fn import_global_config_toml_to(source: &Utf8Path, target: &Utf8Path) -> Result<(), String> {
    validate_import_config_extension(source)?;
    let parent = target
        .parent()
        .ok_or_else(|| format!("global config path has no parent: {target}"))?;
    let _write_lease =
        acquire_global_config_write_lease(target).map_err(|error| error.to_string())?;
    let text = read_import_config_toml(source)?;
    let mut temp =
        NamedTempFile::new_in(parent.as_std_path()).map_err(|error| error.to_string())?;
    temp.write_all(text.as_bytes())
        .map_err(|error| error.to_string())?;
    temp.as_file_mut()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temp.persist(target.as_std_path())
        .map_err(|error| error.error.to_string())?;
    Ok(())
}

fn read_import_config_toml(source: &Utf8Path) -> Result<String, String> {
    validate_import_config_extension(source)?;
    let text = read_toml_utf8_bounded(source).map_err(public_config_import_error)?;
    ConfigLoader::validate_global_config_text(source, &text).map_err(public_config_import_error)?;
    Ok(text)
}

fn public_config_import_error(error: crate::error::ConfigError) -> String {
    match error {
        crate::error::ConfigError::Io(_) => "could not read the selected TOML config".to_string(),
        crate::error::ConfigError::Parse(_) | crate::error::ConfigError::ParseFile { .. } => {
            "the selected TOML config is invalid or does not match the current config schema"
                .to_string()
        }
        crate::error::ConfigError::Serialize(_) => {
            "the selected TOML config could not be processed".to_string()
        }
        crate::error::ConfigError::Message(message)
        | crate::error::ConfigError::Workspace(message) => message,
    }
}

fn validate_import_config_extension(source: &Utf8Path) -> Result<(), String> {
    if !source
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"))
    {
        return Err("select a .toml file".to_string());
    }
    Ok(())
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

pub(super) fn write_markdown_export_atomic(path: &Utf8Path, markdown: &str) -> Result<(), String> {
    let Some(parent) = path.parent().filter(|parent| !parent.as_str().is_empty()) else {
        return Err(format!(
            "markdown export path must have a parent directory: {path}"
        ));
    };
    std::fs::create_dir_all(parent.as_std_path()).map_err(|error| error.to_string())?;
    let parent = std::fs::canonicalize(parent.as_std_path()).map_err(|error| error.to_string())?;
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("markdown export path must have a file name: {path}"))?;
    let destination = parent.join(file_name);
    // tempfile's Windows persist uses Win32 directly, so retain canonicalize's
    // extended path prefix for both the temporary file and destination.
    let mut temp = NamedTempFile::new_in(&parent).map_err(|error| error.to_string())?;
    temp.write_all(markdown.as_bytes())
        .map_err(|error| error.to_string())?;
    temp.as_file_mut()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temp.persist(&destination)
        .map(|_| ())
        .map_err(|error| error.error.to_string())
}

pub fn desktop_open_transcript_markdown_preserves_visible_evidence_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let rows = vec![
        DesktopTranscriptRow {
            row_kind: super::models::DesktopTranscriptRowKind::User,
            stable_history_identity: None,
            step: "01".to_string(),
            title: "Prompt".to_string(),
            body: "Create files.".to_string(),
            file_changes: Vec::new(),
        },
        DesktopTranscriptRow {
            row_kind: super::models::DesktopTranscriptRowKind::Assistant,
            stable_history_identity: None,
            step: "02".to_string(),
            title: "Response".to_string(),
            body: "Now run this:\n<tool_call>\n<function=shell>\n</tool_call>".to_string(),
            file_changes: Vec::new(),
        },
        DesktopTranscriptRow {
            row_kind: super::models::DesktopTranscriptRowKind::Diff,
            stable_history_identity: None,
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
        "fixture-model",
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

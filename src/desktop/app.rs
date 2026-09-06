use std::collections::{BTreeSet, HashMap};
use std::io::Write;
use std::path::{Component, Path};
use std::process::Command as ProcessCommand;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

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
    canonical_session_read_to_markdown, history_markdown_file_name,
};
use crate::storage::{SideChatBinding, SideChatId, SideChatProviderTarget};
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
use super::preferences::DesktopPreferences;
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
            RuntimeMessage::Finished { .. } => RuntimeMessageAsyncContract::TerminalRun,
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
mod command_projection_owner_tests {
    use super::*;

    async fn build_test_app(root: &Utf8Path, store: crate::storage::StoreBundle) -> App {
        build_test_app_with_config(root, store, ResolvedConfig::default()).await
    }

    async fn build_test_app_with_config(
        root: &Utf8Path,
        store: crate::storage::StoreBundle,
        config: ResolvedConfig,
    ) -> App {
        AppBootstrap::rebuild_for_directory_as_workspace_root_with_config(root, store, config)
            .await
            .expect("app")
    }

    #[tokio::test]
    async fn startup_restores_nested_workspace_and_exports_history_inside_its_authority() {
        use crate::session::{NewSession, SessionRepository as _};

        let temp = tempfile::tempdir().expect("tempdir");
        let project_root =
            Utf8PathBuf::from_path_buf(temp.path().join("aaa")).expect("utf8 project root");
        let selected_directory = project_root.join("bbb");
        std::fs::create_dir_all(project_root.join(".git")).expect("git marker");
        std::fs::create_dir_all(selected_directory.join("ccc")).expect("selected directory");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
        let paths = crate::storage::StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        };
        let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
        sqlite.migrate().expect("migrate");
        let store = crate::storage::StoreBundle::new(sqlite);
        let app = build_test_app(&project_root, store).await;
        let session = app
            .store
            .session_repo()
            .create_session(NewSession {
                project_id: app.workspace.project_id,
                title: "nested workspace".to_string(),
                cwd: selected_directory.clone(),
                model: "session-saved-model".to_string(),
                base_url: "http://127.0.0.1:4555/v1".to_string(),
                access_mode: app.config.permissions.access_mode,
                provider_connection: None,
            })
            .await
            .expect("session");
        app.store
            .protocol_event_store()
            .seed_history_item_for_test(&crate::protocol::HistoryItem {
                id: crate::protocol::HistoryItemId::new(),
                session_id: session.id,
                scope: crate::protocol::HistoryScope::Turn {
                    turn_id: crate::protocol::TurnId::new(),
                },
                sequence_no: 1,
                created_at_ms: 1,
                payload: crate::protocol::HistoryItemPayload::UserTurn {
                    content: vec![crate::protocol::ContentPart::Text {
                        text: "export from the selected nested workspace".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                },
            })
            .expect("history item");
        let args = DesktopArgs {
            directory: Some(project_root.clone()),
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

        assert_eq!(controller.app.workspace.root, project_root);
        assert_eq!(controller.app.workspace.cwd, selected_directory);
        assert_eq!(
            controller.state.app_state.current_session_id,
            Some(session.id)
        );
        controller
            .state
            .provider_config
            .effective_config
            .model
            .model = "current-settings-model".to_string();
        controller
            .state
            .provider_config
            .effective_config
            .model
            .base_url = "http://127.0.0.1:4666/v1".to_string();

        let export_title = controller
            .state
            .snapshot
            .session_rows
            .iter()
            .find(|row| row.session_id == session.id)
            .expect("session row")
            .label
            .clone();
        let file_name = history_markdown_file_name(&export_title, session.id);
        let expected_export = selected_directory
            .join(".moyai")
            .join("history-exports")
            .join(&file_name);
        let ancestor_export = project_root
            .join(".moyai")
            .join("history-exports")
            .join(file_name);
        controller.export_history_markdown_auto(session.id);
        for _ in 0..200 {
            controller.drain_runtime_messages();
            if controller.state.can_export_history() && expected_export.is_file() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        assert!(
            expected_export.is_file(),
            "expected {expected_export}; status: {:?}",
            controller.state.app_state.status_message
        );
        assert!(
            !ancestor_export.exists(),
            "automatic export must not escape to the ancestor project root"
        );
        assert!(
            controller
                .state
                .app_state
                .status_message
                .as_deref()
                .is_some_and(|message| message.contains(expected_export.as_str()))
        );
        let canonical_history =
            std::fs::read_to_string(&expected_export).expect("canonical history markdown");
        assert!(canonical_history.contains("Model: session-saved-model"));
        assert!(canonical_history.contains("Base URL: http://127.0.0.1:4555/v1"));

        let transcript_file_name = transcript_markdown_file_name(&session.title, session.id);
        let expected_transcript = selected_directory
            .join(".moyai")
            .join("transcript-exports")
            .join(&transcript_file_name);
        let ancestor_transcript = project_root
            .join(".moyai")
            .join("transcript-exports")
            .join(transcript_file_name);
        controller.export_open_transcript_markdown_auto();
        let transcript =
            std::fs::read_to_string(&expected_transcript).expect("exported transcript markdown");
        assert!(
            !ancestor_transcript.exists(),
            "automatic transcript export must not escape to the ancestor project root"
        );
        assert!(transcript.contains(selected_directory.as_str()));
        assert!(transcript.contains("Provider: `http://127.0.0.1:4555/v1`"));
        assert!(transcript.contains("Model: `session-saved-model`"));
        assert!(!transcript.contains("http://127.0.0.1:4666/v1"));
        assert!(!transcript.contains("current-settings-model"));
    }

    #[tokio::test]
    async fn session_navigation_restores_the_sessions_nested_workspace_cwd() {
        use crate::session::{NewSession, SessionRepository as _};

        let temp = tempfile::tempdir().expect("tempdir");
        let project_root =
            Utf8PathBuf::from_path_buf(temp.path().join("aaa")).expect("utf8 project root");
        let selected_directory = project_root.join("bbb");
        std::fs::create_dir_all(project_root.join(".git")).expect("git marker");
        std::fs::create_dir_all(selected_directory.join("ccc")).expect("selected directory");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
        let paths = crate::storage::StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        };
        let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
        sqlite.migrate().expect("migrate");
        let store = crate::storage::StoreBundle::new(sqlite);
        let app = build_test_app(&project_root, store).await;
        let args = DesktopArgs {
            directory: Some(project_root.clone()),
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
                title: "nested workspace".to_string(),
                cwd: selected_directory.clone(),
                model: controller.app.config.model.model.clone(),
                base_url: controller.app.config.model.base_url.clone(),
                access_mode: controller.app.config.permissions.access_mode,
                provider_connection: None,
            })
            .await
            .expect("session");

        let request_id = controller.state.begin_session_load(session.id);
        controller.spawn_session_load(session.id, SessionLoadReason::UserSelection, request_id);
        for _ in 0..200 {
            controller.drain_runtime_messages();
            if !controller.state.navigation_loading() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        assert!(!controller.state.navigation_loading());
        assert_eq!(controller.app.workspace.root, project_root);
        assert_eq!(controller.app.workspace.cwd, selected_directory);
        assert_eq!(
            controller.state.app_state.current_session_id,
            Some(session.id)
        );
    }

    #[tokio::test]
    async fn nested_first_session_submission_adopts_the_authority_owned_composer() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root =
            Utf8PathBuf::from_path_buf(temp.path().join("aaa")).expect("utf8 project root");
        let selected_directory = project_root.join("bbb");
        std::fs::create_dir_all(project_root.join(".git")).expect("git marker");
        std::fs::create_dir_all(&selected_directory).expect("selected directory");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
        let paths = crate::storage::StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        };
        let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
        sqlite.migrate().expect("migrate");
        let store = crate::storage::StoreBundle::new(sqlite);
        let root_app = build_test_app(&project_root, store).await;
        let app = AppBootstrap::rebuild_for_directory_with_process_runtime(
            &selected_directory,
            root_app.process_runtime.clone(),
        )
        .await
        .expect("nested app");
        let args = DesktopArgs {
            directory: Some(selected_directory.clone()),
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
        let next_image = selected_directory.join("next-request.png");
        let owner_generation = controller.state.composer.owner_generation();
        let run_generation = 41;
        controller
            .state
            .composer
            .image_attachment_paths
            .push(next_image.clone());
        controller.pending_root_submission = Some(PendingRootSubmission {
            run_generation,
            owner_workspace_path: controller.root_submission_owner_workspace_path(),
            owner_session_id: None,
            prompt_dispatch: crate::session::PromptDispatchPart::raw("first nested run"),
            image_paths: Vec::new(),
            prompt_review_to_cancel: None,
        });
        let created_session_id = SessionId::new();
        controller.state.app_state.current_session_id = Some(created_session_id);

        assert_eq!(
            controller.root_submission_owner_workspace_path(),
            selected_directory
        );
        assert!(controller.commit_pending_root_submission(run_generation));
        assert_eq!(
            controller.state.composer.image_attachment_paths,
            vec![next_image]
        );
        assert_eq!(
            controller.state.composer.owner_generation(),
            owner_generation,
            "adopting the first durable session must not reset the authority-owned draft"
        );
        assert!(controller.state.composer.is_owned_by(
            controller.app.workspace.authority_root().as_str(),
            Some(created_session_id)
        ));
    }

    #[tokio::test]
    async fn nested_workspace_preference_preserves_the_selected_authority() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_root =
            Utf8PathBuf::from_path_buf(temp.path().join("aaa")).expect("utf8 project root");
        let selected_directory = project_root.join("bbb");
        std::fs::create_dir_all(project_root.join(".git")).expect("git marker");
        std::fs::create_dir_all(&selected_directory).expect("selected directory");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 data");
        let paths = crate::storage::StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        };
        let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
        sqlite.migrate().expect("migrate");
        let store = crate::storage::StoreBundle::new(sqlite);
        let root_app = build_test_app(&project_root, store).await;
        let app = AppBootstrap::rebuild_for_directory_with_process_runtime(
            &selected_directory,
            root_app.process_runtime.clone(),
        )
        .await
        .expect("nested app");
        let args = DesktopArgs {
            directory: Some(selected_directory.clone()),
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

        assert_eq!(controller.app.workspace.root, project_root);
        assert_eq!(
            controller.app.workspace.authority_root(),
            selected_directory
        );
        assert_eq!(
            controller.workspace_path_for_preferences(),
            Some(selected_directory)
        );
    }

    #[tokio::test]
    async fn workspace_replacement_keeps_the_process_publish_owner_and_fixed_target() {
        use crate::session::{NewSession, SessionRepository as _};

        let (temp, project_root, mut controller) = empty_access_test_controller().await;
        let session = controller
            .app
            .store
            .session_repo()
            .create_session(NewSession {
                project_id: controller.app.workspace.project_id,
                title: "Published main".to_string(),
                cwd: controller.app.workspace.cwd.clone(),
                model: "unused-by-MCP".to_string(),
                base_url: "http://127.0.0.1:9/v1".to_string(),
                access_mode: crate::config::AccessMode::Default,
                provider_connection: None,
            })
            .await
            .unwrap();
        let target = crate::mcp_publish::PublishTarget::Project {
            project_id: session.project_id,
            workspace_root: session.cwd,
        };
        let publish = crate::mcp_publish::PublishService::new(
            Utf8PathBuf::from_path_buf(temp.path().join("application-config/mcp-publish.json"))
                .unwrap(),
            controller.app.store.clone(),
            controller.app.config.clone(),
        );
        let initial = publish.refresh().await.unwrap();
        let saved = publish
            .save(
                None,
                crate::mcp_publish::PublishDraft {
                    label: "Fixed workspace read".to_string(),
                    mode: crate::mcp_publish::PublishMode::ReadTools {},
                    tls: None,
                    bind: "127.0.0.1:7332".parse().unwrap(),
                    target: target.clone(),
                    tools: vec![crate::tool::ToolName::Read],
                    max_concurrent_calls: 1,
                    background: crate::mcp_publish::PublishBackgroundPolicy::StopWhenWindowCloses,
                },
                &initial.revision,
                &initial.generation,
            )
            .await
            .unwrap();
        let profile_id = saved.profiles[0].profile.id;
        controller.state.mcp_publish = Some(publish.clone());
        assert!(controller.next_web_state().unwrap().mcp_publish.is_some());

        let replacement_root =
            Utf8PathBuf::from_path_buf(temp.path().join("different-workspace")).unwrap();
        std::fs::create_dir(&replacement_root).unwrap();
        let replacement_app = AppBootstrap::rebuild_for_directory_with_process_runtime(
            &replacement_root,
            controller.app.process_runtime.clone(),
        )
        .await
        .unwrap();
        let snapshot = load_snapshot_for_selection(&replacement_app, None)
            .await
            .unwrap();
        assert!(controller.replace_workspace_from_load(WorkspaceLoadResult {
            app: replacement_app,
            snapshot
        }));
        assert_ne!(controller.app.workspace.root, project_root);
        let projected = controller
            .next_web_state()
            .unwrap()
            .mcp_publish
            .expect("process publication remains visible after project switch");
        assert_eq!(projected.profiles[0].profile.id, profile_id);
        assert_eq!(projected.profiles[0].profile.target, target);

        // A command on the original managed service must still update the current
        // Desktop projection: copying configuration into a new owner is insufficient.
        let receipt = publish
            .issue_token(profile_id, &projected.revision, &projected.generation)
            .await
            .unwrap();
        let current = controller.next_web_state().unwrap().mcp_publish.unwrap();
        assert_eq!(current.revision, receipt.projection.revision);
        assert!(current.profiles[0].credential_configured);
        assert_eq!(current.profiles[0].profile.target, target);
        assert!(publish.shutdown().await);
    }

    #[tokio::test]
    async fn stale_session_navigation_cannot_replace_a_newer_workspace_with_reused_request_id() {
        let (temp, project_root, mut controller) = empty_access_test_controller().await;
        controller.state.hub_connection = Some(crate::hub::HubConnection::new(
            crate::hub::HubSettingsStore::new(
                Utf8PathBuf::from_path_buf(temp.path().join("hub-settings.json")).unwrap(),
            ),
        ));
        let session_id = SessionId::new();
        let stale_request_id = controller.state.begin_session_load(session_id);
        let stale_target = SessionLoadRequestTarget {
            workspace_root: controller.app.workspace.root.clone(),
            workspace_cwd: controller.app.workspace.cwd.clone(),
            project_id: controller.app.workspace.project_id,
            session_id,
        };
        let stale_loaded = loaded_test_session(
            &controller,
            &project_root,
            session_id,
            SessionStatus::Idle,
            None,
        );

        std::fs::create_dir_all(project_root.join(".git")).expect("git marker");
        let newer_cwd = project_root.join("nested");
        std::fs::create_dir_all(&newer_cwd).expect("newer cwd");
        let newer_app = AppBootstrap::rebuild_for_directory_with_process_runtime(
            &newer_cwd,
            controller.app.process_runtime.clone(),
        )
        .await
        .expect("newer app");
        let newer_snapshot = load_snapshot_for_selection(&newer_app, None)
            .await
            .expect("newer snapshot");
        controller.replace_workspace_from_load(WorkspaceLoadResult {
            app: newer_app,
            snapshot: newer_snapshot,
        });
        let reused_request_id = controller.state.begin_session_load(session_id);
        assert_eq!(reused_request_id, stale_request_id);

        controller.apply_session_loaded_message(
            stale_request_id,
            stale_target,
            SessionLoadReason::UserSelection,
            Ok(SessionNavigationLoadResult {
                workspace: None,
                loaded: stale_loaded,
            }),
        );

        assert_eq!(controller.app.workspace.cwd, newer_cwd);
        assert!(controller.state.navigation_loading());
        assert_eq!(controller.state.app_state.current_session_id, None);
        assert!(controller.next_web_state().unwrap().hub.is_some());
    }

    #[tokio::test]
    async fn initial_setup_latch_rejects_direct_and_async_workspace_owner_replacement() {
        let (temp, project_root, mut controller) = empty_access_test_controller().await;
        let replacement_root =
            Utf8PathBuf::from_path_buf(temp.path().join("replacement-workspace"))
                .expect("utf8 replacement root");
        std::fs::create_dir_all(replacement_root.join(".git")).expect("replacement workspace");
        let replacement_app = AppBootstrap::rebuild_for_directory_with_process_runtime(
            &replacement_root,
            controller.app.process_runtime.clone(),
        )
        .await
        .expect("replacement app");
        let replacement_snapshot = load_snapshot_for_selection(&replacement_app, None)
            .await
            .expect("replacement snapshot");
        let original_workspace_root = controller.app.workspace.root.clone();
        let original_workspace_cwd = controller.app.workspace.cwd.clone();
        let original_project_id = controller.app.workspace.project_id;
        let original_snapshot_workspace = controller.state.snapshot.workspace_path.clone();
        let original_workspace_input = controller.state.workspace_input.clone();

        controller.state.begin_startup(false, None, &project_root);
        let setup_generation = controller.state.startup.setup_generation;
        let setup_reason = controller.state.startup.initial_setup_reason;
        assert!(controller.state.startup.requires_initial_setup());
        assert!(controller.state.view.startup_overlay_forced);

        assert!(!controller.switch_workspace_to(replacement_root.to_string()));
        assert_eq!(controller.state.workspace_input, original_workspace_input);
        assert!(!controller.select_project_and_open(0));
        assert!(!controller.start_project_session(0));
        assert!(!controller.delete_project(original_project_id));
        assert!(!controller.select_session_and_open(0));
        assert!(!controller.start_quick_chat());
        assert!(!controller.create_project_from_picker());
        assert!(!controller.state.navigation_loading());
        assert!(!controller.state.background_mutation_pending());

        let request_id = controller
            .state
            .begin_workspace_load(replacement_root.clone(), None);
        controller.apply_workspace_switched_message(
            request_id,
            Ok(WorkspaceLoadResult {
                app: replacement_app.clone(),
                snapshot: replacement_snapshot.clone(),
            }),
        );
        assert!(
            !controller.state.navigation_loading(),
            "a rejected completion must settle its stale navigation operation"
        );

        let operation_id = controller.state.begin_project_delete_mutation();
        controller
            .runtime_tx
            .send(RuntimeMessage::ProjectDeleted {
                target: ProjectDeleteRequestTarget {
                    workspace_root: original_workspace_root.clone(),
                    owner_project_id: original_project_id,
                    project_id: original_project_id,
                    project_root: original_workspace_root.clone(),
                    operation_id,
                },
                result: Ok(WorkspaceLoadResult {
                    app: replacement_app,
                    snapshot: replacement_snapshot,
                }),
            })
            .expect("project completion");
        controller.drain_runtime_messages();

        assert_eq!(controller.app.workspace.root, original_workspace_root);
        assert_eq!(controller.app.workspace.cwd, original_workspace_cwd);
        assert_eq!(controller.app.workspace.project_id, original_project_id);
        assert_eq!(
            controller.state.snapshot.workspace_path,
            original_snapshot_workspace
        );
        assert_eq!(controller.state.startup.setup_generation, setup_generation);
        assert_eq!(controller.state.startup.initial_setup_reason, setup_reason);
        assert!(controller.state.startup.requires_initial_setup());
        assert!(controller.state.view.startup_overlay_forced);
        assert!(!controller.state.navigation_loading());
        assert!(!controller.state.background_mutation_pending());
        assert!(
            !controller
                .preferences
                .is_project_deleted(&original_workspace_root),
            "a rejected current-project completion must not mutate remembered owners"
        );
    }

    #[tokio::test]
    async fn desktop_cold_start_sends_no_provider_or_docling_requests() {
        use std::io::{Read as _, Write as _};
        use std::sync::atomic::{AtomicBool, AtomicUsize};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("probe listener");
        listener
            .set_nonblocking(true)
            .expect("nonblocking probe listener");
        let endpoint = format!(
            "http://{}",
            listener.local_addr().expect("listener address")
        );
        let request_count = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let server = {
            let request_count = request_count.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            request_count.fetch_add(1, Ordering::SeqCst);
                            let _ =
                                stream.set_read_timeout(Some(std::time::Duration::from_millis(50)));
                            let mut request = [0_u8; 4096];
                            let _ = stream.read(&mut request);
                            let _ = stream.write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                            );
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            })
        };

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
        let mut config = ResolvedConfig::default();
        config.model.base_url = endpoint.clone();
        config.model.model = "cold-start-model".to_string();
        config.docling.enabled = true;
        config.docling.base_url = endpoint;
        let app = build_test_app_with_config(&root, store, config).await;
        let args = DesktopArgs {
            directory: Some(root),
            session_id: None,
            continue_last: false,
            global_config_existed_at_launch: true,
        };

        let _controller = DesktopController::new_with_preferences_and_persistence(
            app,
            args,
            DesktopPreferences::default(),
            false,
        )
        .await
        .expect("controller");
        std::thread::sleep(std::time::Duration::from_millis(250));
        stop.store(true, Ordering::SeqCst);
        server.join().expect("probe server");

        assert_eq!(request_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn terminal_event_moves_root_run_to_finalizing_without_admitting_steer() {
        let cancel = RunControl::new();
        let mut lifecycle = DesktopRunLifecycle::default();
        lifecycle.begin(7, cancel);
        assert!(lifecycle.owns(7));
        assert!(!lifecycle.owns(6));
        assert_eq!(lifecycle.root_generation(), Some(7));
        assert!(lifecycle.can_steer_root());
        lifecycle.observe_terminal_event();

        assert!(lifecycle.root_is_active());
        assert!(lifecycle.root_is_finalizing());
        assert!(!lifecycle.can_steer_root());
        assert_eq!(
            navigation_admission_blocker(false, false, false, lifecycle.root_is_finalizing(),),
            Some("the current run is finalizing")
        );
        lifecycle.finish_root();
        assert!(!lifecycle.root_is_active());
        assert_eq!(lifecycle.root_generation(), None);
        assert_eq!(
            navigation_admission_blocker(false, false, false, lifecycle.root_is_finalizing(),),
            None
        );
    }

    #[test]
    fn root_stop_attempt_is_single_flight_and_stale_completion_cannot_release_a_new_attempt() {
        let mut lifecycle = DesktopRunLifecycle::default();
        let root_control = RunControl::new();
        lifecycle.begin(12, root_control.clone());

        let first = match lifecycle.begin_stop_attempt(12) {
            DesktopRootStopAttemptAdmission::Acquired {
                attempt,
                run_control,
            } => {
                assert!(run_control.same_owner(&root_control));
                attempt
            }
            _ => panic!("the exact generation must acquire its first Stop coordinator"),
        };
        assert!(matches!(
            lifecycle.begin_stop_attempt(12),
            DesktopRootStopAttemptAdmission::AlreadyPending
        ));
        assert!(!lifecycle.finish_stop_attempt(DesktopRootStopAttempt {
            generation: 11,
            attempt_id: first.attempt_id,
        }));
        assert!(lifecycle.finish_stop_attempt(first));

        let second = match lifecycle.begin_stop_attempt(12) {
            DesktopRootStopAttemptAdmission::Acquired { attempt, .. } => attempt,
            _ => panic!("the exact generation must admit a retry after settlement"),
        };
        assert_ne!(second.attempt_id, first.attempt_id);
        assert!(!lifecycle.finish_stop_attempt(first));
        assert!(matches!(
            lifecycle.begin_stop_attempt(12),
            DesktopRootStopAttemptAdmission::AlreadyPending
        ));
        assert!(lifecycle.finish_stop_attempt(second));

        lifecycle.begin(13, RunControl::new());
        assert!(matches!(
            lifecycle.begin_stop_attempt(12),
            DesktopRootStopAttemptAdmission::NotOwned
        ));
    }

    #[test]
    fn settings_effective_config_commit_updates_the_next_turn_owner() {
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
        let mut candidate = ResolvedConfig::default();
        candidate.permissions.access_mode = crate::config::AccessMode::FullAccess;

        commit_effective_config(&mut state, candidate);

        assert_eq!(
            state
                .provider_config
                .effective_config
                .permissions
                .access_mode,
            crate::config::AccessMode::FullAccess
        );
    }

    #[tokio::test]
    async fn settings_reject_invalid_constraints_before_effective_config_commit() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let generation = controller.state.provider_config.config_generation;

        for (field, value) in [
            (ConfigField::ContextWindow, "0"),
            (ConfigField::MaxParallelPredictions, "0"),
            (ConfigField::MultiAgentMaxAgents, "0"),
            (ConfigField::MultiAgentMaxModelRequests, "0"),
            (ConfigField::Temperature, "NaN"),
            (ConfigField::TopP, "inf"),
            (ConfigField::PresencePenalty, "-inf"),
            (ConfigField::FrequencyPenalty, "NaN"),
        ] {
            let original = field.editor_value(&controller.state.provider_config.effective_config);

            assert!(
                !controller
                    .apply_session_config(vec![(field.label().to_string(), value.to_string(),)]),
                "{} must be rejected",
                field.label(),
            );
            assert_eq!(
                field.editor_value(&controller.state.provider_config.effective_config),
                original,
                "{} changed despite the rejected commit",
                field.label(),
            );
            assert_eq!(
                controller.state.provider_config.config_generation, generation,
                "a rejected config must not advance the owner generation",
            );
        }
    }

    #[tokio::test]
    async fn failed_global_save_result_keeps_disk_commit_consumers_unchanged() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let original_app_model = controller.app.config.model.model.clone();
        let original_effective_model = controller
            .state
            .provider_config
            .effective_config
            .model
            .model
            .clone();
        let original_generation = controller.state.provider_config.config_generation;

        let error = controller
            .commit_global_config_save_result(Err("simulated preflight failure".to_string()))
            .expect_err("failed persistence must not commit runtime owners");

        assert_eq!(error, "simulated preflight failure");
        assert_eq!(controller.app.config.model.model, original_app_model);
        assert_eq!(
            controller
                .state
                .provider_config
                .effective_config
                .model
                .model,
            original_effective_model
        );
        assert_eq!(
            controller.state.provider_config.config_generation,
            original_generation
        );
    }

    #[tokio::test]
    async fn latest_docling_readiness_completion_wins_across_config_generation() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let mut config = controller.state.provider_config.effective_config.clone();
        config.docling.enabled = true;
        config.docling.base_url = "http://127.0.0.1:8123".to_string();
        controller.reset_effective_config_without_network(config.clone());
        let stale_target = DoclingReadinessRequestTarget {
            owner: DoclingReadinessConfigOwner::EffectiveConfig,
            base_url: config.docling.base_url.clone(),
            config_generation: controller.state.provider_config.config_generation,
        };
        let stale_request_id = controller
            .docling_readiness_requests
            .begin(stale_target.clone());
        controller
            .state
            .begin_docling_readiness_check(format!("{}/ready", stale_target.base_url));

        config.docling.base_url = "http://127.0.0.1:8124".to_string();
        controller.reset_effective_config_without_network(config);
        let latest_target = DoclingReadinessRequestTarget {
            owner: DoclingReadinessConfigOwner::EffectiveConfig,
            base_url: controller
                .state
                .provider_config
                .effective_config
                .docling
                .base_url
                .clone(),
            config_generation: controller.state.provider_config.config_generation,
        };
        let latest_request_id = controller
            .docling_readiness_requests
            .begin(latest_target.clone());
        controller
            .state
            .begin_docling_readiness_check(format!("{}/ready", latest_target.base_url));
        controller
            .runtime_tx
            .send(RuntimeMessage::DoclingReadinessChecked {
                request_id: stale_request_id,
                target: stale_target,
                result: Ok(crate::docling::DoclingReadinessResult {
                    endpoint: "http://127.0.0.1:8123/ready".to_string(),
                    http_status: 503,
                    ready: false,
                }),
            })
            .expect("stale readiness completion");
        controller
            .runtime_tx
            .send(RuntimeMessage::DoclingReadinessChecked {
                request_id: latest_request_id,
                target: latest_target,
                result: Ok(crate::docling::DoclingReadinessResult {
                    endpoint: "http://127.0.0.1:8124/ready".to_string(),
                    http_status: 204,
                    ready: true,
                }),
            })
            .expect("latest readiness completion");
        controller.drain_runtime_messages();

        assert_eq!(
            controller.state.docling_readiness.status,
            super::super::state::DesktopDoclingReadinessStatus::Ready
        );
        assert_eq!(
            controller.state.docling_readiness.endpoint,
            "http://127.0.0.1:8124/ready"
        );
        assert_eq!(controller.state.docling_readiness.http_status, Some(204));
        assert!(!controller.state.docling_readiness_check_pending());
    }

    #[tokio::test]
    async fn initial_setup_draft_docling_completion_does_not_require_an_effective_endpoint_match() {
        let (_temp, root, mut controller) = empty_access_test_controller().await;
        let mut effective = controller.state.provider_config.effective_config.clone();
        effective.docling.enabled = false;
        effective.docling.base_url = "http://127.0.0.1:8123".to_string();
        controller.reset_effective_config_without_network(effective.clone());
        controller.state.begin_startup(false, None, &root);
        assert!(controller.state.startup.requires_initial_setup());

        let target = DoclingReadinessRequestTarget {
            owner: DoclingReadinessConfigOwner::InitialSetupDraft,
            base_url: "http://127.0.0.1:8124".to_string(),
            config_generation: controller.state.provider_config.config_generation,
        };
        let request_id = controller.docling_readiness_requests.begin(target.clone());
        controller
            .state
            .begin_docling_readiness_check("http://127.0.0.1:8124/ready".to_string());
        controller
            .runtime_tx
            .send(RuntimeMessage::DoclingReadinessChecked {
                request_id,
                target,
                result: Ok(crate::docling::DoclingReadinessResult {
                    endpoint: "http://127.0.0.1:8124/ready".to_string(),
                    http_status: 204,
                    ready: true,
                }),
            })
            .expect("draft readiness completion");
        controller.drain_runtime_messages();

        assert_eq!(
            controller.state.docling_readiness.status,
            super::super::state::DesktopDoclingReadinessStatus::Ready
        );
        assert_eq!(
            controller.state.docling_readiness.endpoint,
            "http://127.0.0.1:8124/ready"
        );
        assert_eq!(
            controller
                .state
                .provider_config
                .effective_config
                .docling
                .enabled,
            effective.docling.enabled
        );
        assert_eq!(
            controller
                .state
                .provider_config
                .effective_config
                .docling
                .base_url,
            effective.docling.base_url
        );
    }

    #[tokio::test]
    async fn global_provider_candidate_does_not_mix_root_effective_overrides() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let mut global = controller.state.global_config().clone();
        global.model.base_url = "http://127.0.0.1:1234".to_string();
        global.model.model = "global-model".to_string();
        global.model.context_window = 32_768;
        global.model.max_output_tokens = 2_048;
        global.model.supports_tools = false;
        global.model.temperature = Some(0.2);
        global.permissions.access_mode = crate::config::AccessMode::Default;
        controller.app.config = global.clone();
        controller.state.replace_global_config(global.clone());

        let mut root_effective = global.clone();
        root_effective.model.base_url = "http://127.0.0.1:5678".to_string();
        root_effective.model.model = "root-model".to_string();
        root_effective.model.context_window = 131_072;
        root_effective.model.max_output_tokens = 8_192;
        root_effective.model.supports_tools = true;
        root_effective.model.temperature = Some(0.9);
        root_effective.permissions.access_mode = crate::config::AccessMode::FullAccess;
        controller.reset_effective_config_without_network(root_effective);
        assert!(controller.state.show_provider_editor());

        let candidate = controller
            .apply_provider_selection_to_global_config()
            .expect("global provider candidate");

        assert_eq!(candidate.model.base_url, global.model.base_url);
        assert_eq!(candidate.model.model, global.model.model);
        assert_eq!(candidate.model.context_window, global.model.context_window);
        assert_eq!(
            candidate.model.max_output_tokens,
            crate::config::DEFAULT_MODEL_MAX_OUTPUT_TOKENS
        );
        assert_eq!(candidate.model.supports_tools, global.model.supports_tools);
        assert_eq!(candidate.model.temperature, None);
        assert_eq!(
            candidate.permissions.access_mode,
            global.permissions.access_mode
        );
    }

    #[test]
    fn initial_setup_import_loads_a_complete_validated_draft_without_a_state_owner() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path =
            Utf8PathBuf::from_path_buf(temp.path().join("import.toml")).expect("UTF-8 import path");
        std::fs::write(
            path.as_std_path(),
            "[model]\nmodel = \"imported-model\"\nbase_url = \"http://127.0.0.1:1234\"\nextra_headers = { Authorization = \"INITIAL_SETUP_FILE_IMPORT_SECRET\" }\n",
        )
        .expect("write valid import");

        let config = DesktopController::load_initial_setup_config_toml_path(&path)
            .expect("validated initial-setup draft");
        assert_eq!(
            config
                .model
                .extra_headers
                .get("Authorization")
                .map(String::as_str),
            Some("INITIAL_SETUP_FILE_IMPORT_SECRET"),
            "the Rust-only staged owner receives the exact imported secret"
        );
        let public_secret = ConfigField::ExtraHeadersJson.public_value(&config);
        assert!(public_secret.sensitive);
        assert!(public_secret.configured);
        assert!(public_secret.value.is_empty());
        let values = ConfigField::ALL
            .into_iter()
            .filter(|field| !field.is_host_owned_generation())
            .map(|field| (field.label().to_string(), field.public_value(&config).value))
            .collect::<Vec<_>>();

        let current_gui_fields = ConfigField::ALL
            .into_iter()
            .filter(|field| !field.is_host_owned_generation())
            .collect::<Vec<_>>();
        assert_eq!(values.len(), current_gui_fields.len());
        for field in current_gui_fields {
            assert!(
                values.iter().any(|(key, _)| key == field.label()),
                "all current GUI ConfigField values must be projected: {}",
                field.label()
            );
        }
        for field in ConfigField::ALL
            .into_iter()
            .filter(|field| field.is_host_owned_generation())
        {
            assert!(
                values.iter().all(|(key, _)| key != field.label()),
                "host-owned generation field must not enter the imported GUI draft: {}",
                field.label()
            );
        }

        std::fs::write(
            path.as_std_path(),
            "[model]\nmodel = \"imported-model\"\n\n[unknown]\nflag = true\n",
        )
        .expect("write invalid import");
        assert!(
            DesktopController::load_initial_setup_config_toml_path(&path).is_err(),
            "unknown current-schema sections must fail closed"
        );
    }

    #[test]
    fn initial_setup_malformed_toml_error_never_echoes_source_secrets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("secret-import.toml"))
            .expect("UTF-8 import path");
        let secret = "INITIAL_SETUP_PARSE_SECRET_SENTINEL";
        std::fs::write(
            path.as_std_path(),
            format!("[model]\nextra_headers = {{ Authorization = \"{secret}\", broken = }}\n"),
        )
        .expect("write malformed import");

        let error = DesktopController::load_initial_setup_config_toml_path(&path)
            .expect_err("malformed TOML must fail closed");
        assert_eq!(
            error,
            "the selected TOML config is invalid or does not match the current config schema"
        );
        assert!(!error.contains(secret));
        assert!(!error.contains(path.as_str()));
        assert!(!error.contains("Authorization"));
    }

    #[tokio::test]
    async fn initial_setup_import_secrets_stay_in_one_generation_fenced_rust_owner() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let first_secret = "INITIAL_IMPORT_SECRET_ONE";
        let second_secret = "INITIAL_IMPORT_SECRET_TWO";
        let prompt_marker = "INITIAL_IMPORT_MAIN_PROMPT_MARKER";
        let mut first = ResolvedConfig::default();
        first.model.system_prompt = prompt_marker.to_string();
        first
            .model
            .extra_headers
            .insert("X-Import-Secret".to_string(), first_secret.to_string());
        first
            .docling
            .headers
            .insert("Authorization".to_string(), first_secret.to_string());
        let (first_generation, public_values) = controller
            .stage_initial_setup_config_import(first)
            .expect("stage first import");
        for key in [
            ConfigField::ExtraHeadersJson,
            ConfigField::DoclingHeadersJson,
        ] {
            let public = public_values
                .iter()
                .find(|value| value.key == key.label())
                .expect("sensitive public field");
            assert!(public.sensitive);
            assert!(public.configured);
            assert!(public.text.is_empty());
        }
        let prompt_public = public_values
            .iter()
            .find(|value| value.key == ConfigField::SystemPrompt.label())
            .expect("main system prompt public field");
        assert_eq!(prompt_public.text, prompt_marker);
        let public_debug = format!("{public_values:?}");
        assert!(!public_debug.contains(first_secret));
        assert!(!public_debug.contains(prompt_marker));
        assert!(public_debug.contains("text_chars"));

        let mut second = ResolvedConfig::default();
        second
            .model
            .extra_headers
            .insert("X-Import-Secret".to_string(), second_secret.to_string());
        let (second_generation, _) = controller
            .stage_initial_setup_config_import(second)
            .expect("stage replacement import");
        let redacted_values = ConfigField::ALL
            .into_iter()
            .filter(|field| !field.is_host_owned_generation())
            .map(|field| {
                (
                    field.label().to_string(),
                    field.public_value(controller.state.global_config()).value,
                )
            })
            .collect::<Vec<_>>();
        assert!(
            controller
                .hydrate_initial_setup_import_sensitive_values(
                    redacted_values.clone(),
                    Some(first_generation),
                )
                .is_err(),
            "a second import must invalidate the first raw owner"
        );
        let hydrated = controller
            .hydrate_initial_setup_import_sensitive_values(
                redacted_values.clone(),
                Some(second_generation),
            )
            .expect("current import generation");
        let headers = hydrated
            .iter()
            .find(|(key, _)| key == ConfigField::ExtraHeadersJson.label())
            .map(|(_, value)| value.as_str())
            .expect("hydrated headers");
        assert!(headers.contains(second_secret));
        assert!(!headers.contains(first_secret));

        let explicit = redacted_values
            .into_iter()
            .map(|(key, value)| {
                if key == ConfigField::ExtraHeadersJson.label() {
                    (key, "{\"X-Manual\":\"manual-value\"}".to_string())
                } else {
                    (key, value)
                }
            })
            .collect();
        let explicit = controller
            .hydrate_initial_setup_import_sensitive_values(explicit, Some(second_generation))
            .expect("explicit sensitive replacement");
        let headers = explicit
            .iter()
            .find(|(key, _)| key == ConfigField::ExtraHeadersJson.label())
            .map(|(_, value)| value.as_str())
            .expect("explicit headers");
        assert!(headers.contains("manual-value"));
        assert!(!headers.contains(second_secret));

        controller.state.provider_config.config_generation = controller
            .state
            .provider_config
            .config_generation
            .saturating_add(1);
        controller.reconcile_pending_initial_setup_config_import_owner();
        assert!(
            controller.pending_initial_setup_config_import.is_none(),
            "raw imported secrets must be dropped as soon as their exact config owner drifts"
        );
    }

    #[test]
    fn only_typed_interruptions_suppress_desktop_failure_notifications() {
        let interruption = RunCancellationCause::Interruption(TurnInterruptionCause::UserStop);
        let failure = RunCancellationCause::Failure("provider unavailable".to_string());
        let superseded = RunCancellationCause::Superseded;

        assert!(desktop_run_failure_notification_allowed(None));
        assert!(!desktop_run_failure_notification_allowed(Some(
            &interruption,
        )));
        assert!(desktop_run_failure_notification_allowed(Some(&failure)));
        assert!(desktop_run_failure_notification_allowed(Some(&superseded)));
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
            crate::config::AccessMode::FullAccess,
            None,
            |expected, mode| {
                assert_eq!(expected, crate::config::AccessMode::Default);
                assert_eq!(mode, crate::config::AccessMode::FullAccess);
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
    fn access_persistence_completion_accepts_only_the_same_owner_and_runtime_epoch() {
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
        for current in ["tree:4", "idle:4"] {
            assert!(
                access_mode_persistence_target_matches(
                    &target,
                    Utf8Path::new("C:/workspace"),
                    Some(session_id),
                    7,
                    current,
                ),
                "root-owned access persistence may settle after the same epoch crosses to {current}"
            );
        }
        assert!(!access_mode_persistence_target_matches(
            &target,
            Utf8Path::new("C:/other"),
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

        let tree_target = AccessModePersistenceTarget {
            runtime_owner_token: "tree:4".to_string(),
            ..target.clone()
        };
        for current in ["tree:4", "root:4", "idle:4"] {
            assert!(
                access_mode_persistence_target_matches(
                    &tree_target,
                    Utf8Path::new("C:/workspace"),
                    Some(session_id),
                    7,
                    current,
                ),
                "tree-owned access persistence may settle exactly or after the same child tree returns to {current}"
            );
        }
        for current in ["root:5", "tree:5", "idle:5"] {
            assert!(
                !access_mode_persistence_target_matches(
                    &tree_target,
                    Utf8Path::new("C:/workspace"),
                    Some(session_id),
                    7,
                    current,
                ),
                "a different runtime epoch must be rejected: {current}"
            );
        }

        let idle_target = AccessModePersistenceTarget {
            runtime_owner_token: "idle:4".to_string(),
            ..target.clone()
        };
        assert!(access_mode_persistence_target_matches(
            &idle_target,
            Utf8Path::new("C:/workspace"),
            Some(session_id),
            7,
            "idle:4",
        ));
        for current in ["root:4", "tree:4"] {
            assert!(
                !access_mode_persistence_target_matches(
                    &idle_target,
                    Utf8Path::new("C:/workspace"),
                    Some(session_id),
                    7,
                    current,
                ),
                "an idle capture cannot adopt a later active phase: {current}"
            );
        }

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
                7,
                "tree:4",
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

        let commit = worker
            .persist_initial_owners(&target)
            .expect("global-only first phase");
        let error = worker
            .persist_adopted_session(&target, SessionId::new(), commit.remembered_path)
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
        let app = build_test_app(&root, store).await;
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
            provider_connection: None,
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
        controller.run_lifecycle.begin(1, RunControl::new());
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
                            .map(|update| Some(update.session))
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
        assert!(
            session_service
                .update_root_session_access_mode(child_session.id, expected_access_mode)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn desktop_child_only_tree_can_change_root_access_and_settle_after_tree_finishes() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let root_session_id = SessionId::new();
        let child_session_id = SessionId::new();
        controller.state.app_state.current_session_id = Some(root_session_id);
        controller.loaded_agent_activity_records = Some((
            root_session_id,
            vec![agent_record(
                child_session_id,
                "/root/active_child",
                AgentStatus::Running,
                "",
            )],
        ));
        assert!(controller.current_agent_tree_active());
        assert!(!controller.run_lifecycle.root_is_active());
        assert!(controller.access_mode_mutation_admission_open());
        assert_eq!(
            controller.access_mode_mutation_runtime_contract().0,
            "tree:0"
        );

        let mut persisted = None;
        assert!(controller.toggle_access_mode_with_persistence(
            |expected, access_mode| {
                assert_eq!(expected, crate::config::AccessMode::Default);
                assert_eq!(access_mode, crate::config::AccessMode::AutoReview);
                Ok(Some(Utf8PathBuf::from("C:/config.toml")))
            },
            |session_id, access_mode| {
                persisted = Some((session_id, access_mode));
                Ok(())
            },
        ));
        assert_eq!(
            persisted,
            Some((root_session_id, crate::config::AccessMode::AutoReview))
        );

        let target = AccessModePersistenceTarget {
            operation_id: controller.state.begin_access_mode_persistence(),
            workspace_root: controller.app.workspace.root.clone(),
            session_id: Some(root_session_id),
            config_generation: controller.state.provider_config.config_generation,
            root_run_generation: None,
            runtime_owner_token: "tree:0".to_string(),
            old_global_access_mode: crate::config::AccessMode::Default,
            old_effective_access_mode: crate::config::AccessMode::Default,
            access_mode: crate::config::AccessMode::AutoReview,
        };
        controller.loaded_agent_activity_records = Some((root_session_id, Vec::new()));
        assert!(!controller.current_agent_tree_active());
        assert_eq!(
            controller.access_mode_mutation_runtime_contract().0,
            "idle:0"
        );
        assert_eq!(
            controller.access_mode_persistence_target_relation(&target),
            AccessModePersistenceTargetRelation::Exact,
            "tree:N to idle:N keeps the same root-session access owner"
        );

        controller.run_lifecycle.begin(1, RunControl::new());
        controller.next_root_run_generation = 2;
        assert_eq!(
            controller.access_mode_persistence_target_relation(&target),
            AccessModePersistenceTargetRelation::Stale,
            "a new root generation revokes the finished tree completion grace"
        );
    }

    #[tokio::test]
    async fn desktop_child_only_tree_does_not_block_new_root_prompt_admission() {
        use crate::session::{NewSession, SessionRepository as _};

        let (_temp, workspace_root, mut controller) = empty_access_test_controller().await;
        let root_session = controller
            .app
            .store
            .session_repo()
            .create_session(NewSession {
                project_id: controller.app.workspace.project_id,
                title: "independent root terminal".to_string(),
                cwd: workspace_root,
                model: controller.app.config.model.model.clone(),
                base_url: controller.app.config.model.base_url.clone(),
                access_mode: controller.app.config.permissions.access_mode,
                provider_connection: None,
            })
            .await
            .expect("root session");
        controller.state.app_state.current_session_id = Some(root_session.id);
        controller.state.app_state.current_session_title = root_session.title;
        controller.state.app_state.run_status = crate::tui::state::RunStatus::Completed;
        controller.loaded_agent_activity_records = Some((
            root_session.id,
            vec![agent_record(
                SessionId::new(),
                "/root/detached_child",
                AgentStatus::Running,
                "",
            )],
        ));

        assert!(controller.current_agent_tree_active());
        assert!(!controller.run_lifecycle.root_is_active());
        let admitted_generation = controller.next_root_run_generation;

        assert!(
            controller.start_run("continue with a new root turn".to_string()),
            "a descendant worker is not the root prompt admission owner"
        );
        assert_eq!(
            controller.run_lifecycle.root_generation(),
            Some(admitted_generation)
        );
        assert_eq!(
            controller
                .pending_root_submission
                .as_ref()
                .map(|pending| pending.run_generation),
            Some(admitted_generation)
        );
        assert_eq!(controller.next_root_run_generation, admitted_generation + 1);

        // This test owns no provider response. Dropping the exact worker owner aborts the
        // pre-admitted task before the controller's local executor is shut down.
        drop(controller.run_lifecycle.root.take());
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
        let app = build_test_app(&root, store).await;
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
                provider_connection: None,
            })
            .await
            .expect("session");
        controller
            .app
            .store
            .session_repo()
            .admit_session_turn(session.id, crate::protocol::TurnId::new())
            .await
            .expect("active root admission")
            .expect("active root admitted");
        controller.app.config.permissions.access_mode = initial_access_mode;
        controller
            .state
            .provider_config
            .update_access_mode(initial_access_mode);
        controller.run_lifecycle.begin(1, RunControl::new());
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
                        .map(|update| Some(update.session))
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
        let app = build_test_app(&root, store).await;
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
                provider_connection: None,
            })
            .await
            .expect("session");
        controller
            .app
            .store
            .session_repo()
            .admit_session_turn(session.id, crate::protocol::TurnId::new())
            .await
            .expect("late active root admission")
            .expect("late active root admitted");
        controller.app.config.permissions.access_mode = initial_access_mode;
        controller
            .state
            .provider_config
            .update_access_mode(initial_access_mode);
        controller.run_lifecycle.begin(1, RunControl::new());
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
                        .map(|update| Some(update.session))
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
            "the next-turn owner is not committed until both durable owners succeed"
        );

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
        let app = build_test_app(&root, store).await;
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
    async fn session_settings_cas_loser_returns_canonical_record_for_conflict_rebase() {
        use crate::session::{NewSession, SessionRepository as _};

        let (_temp, root, mut controller) = empty_access_test_controller().await;
        let initial = controller
            .app
            .store
            .session_repo()
            .create_session(NewSession {
                project_id: controller.app.workspace.project_id,
                title: "root".to_string(),
                cwd: root,
                model: "initial-model".to_string(),
                base_url: controller.app.config.model.base_url.clone(),
                access_mode: controller.app.config.permissions.access_mode,
                provider_connection: None,
            })
            .await
            .expect("root session");
        let loaded = load_latest_session_detail(&controller.app, initial.id)
            .await
            .expect("canonical root detail");
        controller.state.load_open_session(&loaded.read);

        let persistence = controller
            .prepare_root_session_settings_persistence(
                initial.session_settings_revision,
                SessionSettingsPatch {
                    model: Some("losing-model".to_string()),
                    ..SessionSettingsPatch::default()
                },
            )
            .expect("persistence request")
            .expect("matching in-memory revision");
        let winner = controller
            .app
            .session_service
            .compare_and_set_root_session_settings(
                initial.id,
                initial.session_settings_revision,
                SessionSettingsPatch {
                    model: Some("winning-model".to_string()),
                    ..SessionSettingsPatch::default()
                },
            )
            .await
            .expect("winning settings write")
            .expect("winning CAS update");
        let access_only = persistence.access_only();

        let outcome = tokio::task::spawn_blocking(move || persistence.execute_blocking())
            .await
            .expect("persistence worker")
            .expect("canonical conflict outcome");
        let RootSessionSettingsPersistenceOutcome::Conflict { session: canonical } = outcome else {
            panic!("the stale writer must return the canonical conflict owner");
        };
        assert!(!access_only);
        assert_eq!(canonical.id, initial.id);
        assert_eq!(canonical.model, "winning-model");
        assert_eq!(
            canonical.session_settings_revision,
            winner.session.session_settings_revision
        );

        assert!(
            controller
                .state
                .apply_persisted_root_session_record(canonical.clone())
        );
        let open = controller
            .state
            .open_session
            .as_ref()
            .expect("rebased open session")
            .session();
        assert_eq!(open.model, "winning-model");
        assert_eq!(
            open.session_settings_revision,
            winner.session.session_settings_revision
        );

        assert!(
            !controller
                .state
                .apply_persisted_root_session_record(initial),
            "a superseded persistence completion must not downgrade a newer canonical revision"
        );
        assert_eq!(
            controller
                .state
                .open_session
                .as_ref()
                .expect("newer open session remains")
                .session()
                .session_settings_revision,
            winner.session.session_settings_revision
        );
    }

    #[tokio::test]
    async fn initial_setup_session_apply_keeps_the_finish_only_startup_latch() {
        let (_temp, root, mut controller) = empty_access_test_controller().await;
        let persisted_model = controller.app.config.model.model.clone();
        controller.state.begin_startup(false, None, &root);
        assert!(controller.state.startup.requires_initial_setup());
        assert!(controller.state.view.startup_overlay_forced);
        assert_eq!(
            controller.state.view.overlay,
            super::super::state::DesktopOverlay::InitialSetup
        );

        let temporary_model = "temporary-initial-setup-model";
        let values =
            ConfigEditorState::from_config(&controller.state.provider_config.effective_config)
                .fields
                .into_iter()
                .map(|field| {
                    let value = if field.key == ConfigField::Model {
                        temporary_model.to_string()
                    } else {
                        field.value
                    };
                    (field.key.label().to_string(), value)
                })
                .collect();

        assert!(controller.apply_session_config(values));
        assert!(controller.state.startup.requires_initial_setup());
        assert!(controller.state.view.startup_overlay_forced);
        assert_eq!(
            controller.state.view.overlay,
            super::super::state::DesktopOverlay::InitialSetup
        );
        assert_eq!(
            controller
                .state
                .provider_config
                .effective_config
                .model
                .model,
            temporary_model
        );
        assert_eq!(
            controller.app.config.model.model, persisted_model,
            "session Apply must not replace the persisted global config owner"
        );
    }

    async fn side_chat_test_controller()
    -> (tempfile::TempDir, Utf8PathBuf, DesktopController, SessionId) {
        use crate::session::SessionRepository as _;

        let (temp, root, mut controller) = empty_access_test_controller().await;
        let session = controller
            .app
            .store
            .session_repo()
            .create_session(crate::session::NewSession {
                project_id: controller.app.workspace.project_id,
                title: "side chat owner".to_string(),
                cwd: root.clone(),
                model: controller.app.config.model.model.clone(),
                base_url: controller.app.config.model.base_url.clone(),
                access_mode: controller.app.config.permissions.access_mode,
                provider_connection: None,
            })
            .await
            .expect("side chat owner session");
        let snapshot = load_snapshot_for_selection(&controller.app, Some(session.id))
            .await
            .expect("owner snapshot");
        controller.state.replace_snapshot(snapshot);
        controller.state.app_state.current_session_id = Some(session.id);
        controller.state.app_state.current_session_title = session.title;
        (temp, root, controller, session.id)
    }

    #[tokio::test]
    async fn side_chat_configuration_owns_its_trimmed_system_prompt_without_main_inheritance() {
        let (_temp, _root, mut controller, owner_session_id) = side_chat_test_controller().await;
        controller
            .state
            .provider_config
            .effective_config
            .model
            .system_prompt = "MAIN_PRIVATE_PROMPT".to_string();

        controller
            .configure_side_chat(
                owner_session_id,
                "http://127.0.0.1:1234".to_string(),
                "side-model".to_string(),
                "  SIDE_PRIVATE_PROMPT  ".to_string(),
                ProviderProfile::OpenAiCompatible,
            )
            .expect("configure side chat with its own prompt");
        let binding = controller
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .expect("binding read")
            .expect("binding");
        assert_eq!(binding.system_prompt, "SIDE_PRIVATE_PROMPT");
        let projection = controller.side_chat_projection();
        assert_eq!(projection.system_prompt, "SIDE_PRIVATE_PROMPT");
        let debug = format!("{projection:?}");
        assert!(debug.contains("system_prompt_chars"));
        assert!(!debug.contains("SIDE_PRIVATE_PROMPT"));
        assert!(!debug.contains("MAIN_PRIVATE_PROMPT"));

        controller
            .configure_side_chat(
                owner_session_id,
                "http://127.0.0.1:1234".to_string(),
                "side-model".to_string(),
                "   ".to_string(),
                ProviderProfile::OpenAiCompatible,
            )
            .expect("clear side chat prompt");
        let binding = controller
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .expect("binding read")
            .expect("binding");
        assert!(binding.system_prompt.is_empty());
    }

    #[tokio::test]
    async fn global_side_chat_defaults_are_snapshotted_until_the_conversation_is_closed() {
        let (_temp, _root, mut controller, owner_session_id) = side_chat_test_controller().await;
        {
            let defaults = &mut controller.state.provider_config.effective_config.side_chat;
            defaults.base_url = "http://127.0.0.1:4111".to_string();
            defaults.model = "first-side-model".to_string();
            defaults.system_prompt = "FIRST_SIDE_PROMPT".to_string();
            defaults.provider_profile = ProviderProfile::OpenAiCompatible;
            defaults.context_window = 65_536;
        }

        controller
            .ensure_side_chat(owner_session_id)
            .expect("materialize global Side Chat defaults");
        let first = controller
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .expect("read first binding")
            .expect("first binding");
        assert_eq!(first.model, "first-side-model");
        assert_eq!(first.system_prompt, "FIRST_SIDE_PROMPT");
        assert_eq!(first.context_window, 65_536);

        controller
            .state
            .provider_config
            .effective_config
            .side_chat
            .model = "second-side-model".to_string();
        controller
            .state
            .provider_config
            .effective_config
            .side_chat
            .system_prompt = "SECOND_SIDE_PROMPT".to_string();
        controller
            .ensure_side_chat(owner_session_id)
            .expect("existing Side Chat remains stable");
        let unchanged = controller
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .expect("read stable binding")
            .expect("stable binding");
        assert_eq!(unchanged.id, first.id);
        assert_eq!(unchanged.model, "first-side-model");
        assert_eq!(unchanged.system_prompt, "FIRST_SIDE_PROMPT");

        controller
            .delete_side_chat(owner_session_id, first.id, first.request_generation)
            .expect("close first Side Chat");
        assert!(
            controller
                .app
                .store
                .side_chat_repo()
                .get_by_owner(owner_session_id)
                .expect("read deleted binding")
                .is_none()
        );
        controller
            .ensure_side_chat(owner_session_id)
            .expect("materialize updated global defaults");
        let replacement = controller
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .expect("read replacement binding")
            .expect("replacement binding");
        assert_ne!(replacement.id, first.id);
        assert_eq!(replacement.model, "second-side-model");
        assert_eq!(replacement.system_prompt, "SECOND_SIDE_PROMPT");
    }

    #[tokio::test]
    async fn side_chat_draft_quote_is_atomic_replaces_without_orphan_and_rehydrates_after_restart()
    {
        let (_temp, root, mut controller, owner_session_id) = side_chat_test_controller().await;
        controller
            .configure_side_chat(
                owner_session_id,
                "http://127.0.0.1:1234".to_string(),
                "side-model".to_string(),
                String::new(),
                ProviderProfile::OpenAiCompatible,
            )
            .expect("configure side chat");
        let initial = controller
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .expect("initial binding read")
            .expect("initial binding");
        let first_quote = SideChatQuoteRequest {
            source_kind: super::super::side_chat::SideChatQuoteSourceKind::Transcript,
            source_history_item_id: crate::protocol::HistoryItemId::new(),
            source_append_position: Some(41),
            selected_text: "first authority".to_string(),
        };
        controller
            .save_side_chat_draft(
                owner_session_id,
                initial.id,
                initial.draft_revision,
                "> Side Chat 引用\n> first authority\n\n".to_string(),
                Some(first_quote),
            )
            .expect("persist first typed quote");
        let after_first = controller
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .expect("first binding read")
            .expect("first binding");
        let second_quote = SideChatQuoteRequest {
            source_kind: super::super::side_chat::SideChatQuoteSourceKind::Artifact,
            source_history_item_id: crate::protocol::HistoryItemId::new(),
            source_append_position: Some(42),
            selected_text: "second authority".to_string(),
        };
        let second_text = "> Side Chat 引用\n> second authority\n\nExplain this.".to_string();
        controller
            .save_side_chat_draft(
                owner_session_id,
                after_first.id,
                after_first.draft_revision,
                second_text.clone(),
                Some(second_quote.clone()),
            )
            .expect("replace typed quote atomically");
        let after_second = controller
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .expect("second binding read")
            .expect("second binding");
        let decoded = decode_persisted_side_chat_draft(&after_second.persisted_draft)
            .expect("decode durable envelope");
        assert_eq!(decoded.text, second_text);
        assert_eq!(decoded.quote, Some(second_quote.clone()));
        assert!(!after_second.persisted_draft.contains("first authority"));

        let projected = controller.side_chat_projection();
        assert_eq!(projected.draft_text, second_text);
        assert!(
            !projected
                .draft_text
                .starts_with(super::super::side_chat::SIDE_CHAT_DRAFT_ENVELOPE_PREFIX)
        );
        assert_eq!(
            projected
                .draft_quote
                .as_ref()
                .map(|quote| quote.source_history_item_id.clone()),
            Some(second_quote.source_history_item_id.to_string())
        );

        let paths = controller.app.store.paths().clone();
        drop(controller);
        let sqlite = crate::storage::SqliteStore::open(&paths).expect("reopen sqlite");
        sqlite.migrate().expect("audit migrations on reopen");
        let store = crate::storage::StoreBundle::new(sqlite);
        let app = build_test_app(&root, store).await;
        let args = DesktopArgs {
            directory: Some(root),
            session_id: Some(owner_session_id),
            continue_last: false,
            global_config_existed_at_launch: true,
        };
        let mut reopened = DesktopController::new_with_preferences_and_persistence(
            app,
            args,
            DesktopPreferences::default(),
            false,
        )
        .await
        .expect("reopen controller");
        let restarted_projection = reopened.side_chat_projection();
        assert_eq!(restarted_projection.draft_text, second_text);
        assert_eq!(
            restarted_projection.draft_quote,
            Some(DesktopSideChatDraftQuoteProjection {
                source_kind: "artifact".to_string(),
                source_history_item_id: second_quote.source_history_item_id.to_string(),
                source_append_position: Some("42".to_string()),
                selected_text: "second authority".to_string(),
            })
        );

        reopened
            .save_side_chat_draft(
                owner_session_id,
                after_second.id,
                after_second.draft_revision,
                "plain text after manual edit".to_string(),
                None,
            )
            .expect("manual edit clears durable quote");
        let plain = reopened
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .expect("plain binding read")
            .expect("plain binding");
        assert_eq!(plain.persisted_draft, "plain text after manual edit");
        let plain_projection = reopened.side_chat_projection();
        assert_eq!(plain_projection.draft_text, "plain text after manual edit");
        assert_eq!(plain_projection.draft_quote, None);
    }

    #[tokio::test]
    async fn malformed_side_chat_draft_envelope_is_hidden_blocked_and_recoverable() {
        let (_temp, _root, mut controller, owner_session_id) = side_chat_test_controller().await;
        controller
            .configure_side_chat(
                owner_session_id,
                "http://127.0.0.1:9".to_string(),
                "never-contact-provider".to_string(),
                String::new(),
                ProviderProfile::OpenAiCompatible,
            )
            .expect("configure side chat");
        let initial = controller
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .expect("initial binding read")
            .expect("initial binding");
        let quote = SideChatQuoteRequest {
            source_kind: super::super::side_chat::SideChatQuoteSourceKind::Transcript,
            source_history_item_id: crate::protocol::HistoryItemId::new(),
            source_append_position: Some(7),
            selected_text: "must remain opaque".to_string(),
        };
        let mut malformed = encode_persisted_side_chat_draft(
            "must not reach the provider".to_string(),
            Some(&quote),
        )
        .expect("valid envelope before corruption");
        assert_eq!(malformed.pop(), Some('}'));
        let corrupt = controller
            .app
            .store
            .side_chat_repo()
            .update_draft(
                owner_session_id,
                initial.id,
                initial.draft_revision,
                malformed.clone(),
            )
            .expect("seed corrupt envelope")
            .binding;

        let projection = controller.side_chat_projection();
        assert_eq!(projection.draft_text, "");
        assert_eq!(projection.draft_quote, None);
        assert!(!projection.can_send);
        assert_eq!(
            projection.last_error,
            "the saved Side Chat draft is invalid; edit and save the draft to recover"
        );
        assert!(
            !projection
                .last_error
                .contains("must not reach the provider")
        );

        let error = controller
            .start_side_chat(
                owner_session_id,
                corrupt.id,
                corrupt.request_generation,
                corrupt.draft_revision,
                None,
                None,
                malformed,
            )
            .expect_err("malformed durable envelope must stop before provider transport");
        assert_eq!(
            error,
            "the saved Side Chat draft is invalid; edit and save the draft to recover"
        );
        assert!(!controller.side_chat_runs.contains_key(&owner_session_id));

        controller
            .save_side_chat_draft(
                owner_session_id,
                corrupt.id,
                corrupt.draft_revision,
                "recovered plain draft".to_string(),
                None,
            )
            .expect("overwrite corrupt envelope");
        let recovered = controller.side_chat_projection();
        assert_eq!(recovered.draft_text, "recovered plain draft");
        assert_eq!(recovered.draft_quote, None);
        assert!(recovered.can_send);
    }

    #[tokio::test]
    async fn stale_side_chat_owner_snapshot_is_rejected_before_hidden_admission_or_provider_post() {
        let (_temp, _root, mut controller, owner_session_id) = side_chat_test_controller().await;
        controller
            .configure_side_chat(
                owner_session_id,
                "http://127.0.0.1:9".to_string(),
                "never-contact-provider".to_string(),
                String::new(),
                ProviderProfile::OpenAiCompatible,
            )
            .expect("configure side chat");
        let owner_item = crate::protocol::HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: owner_session_id,
            scope: crate::protocol::HistoryScope::Turn {
                turn_id: TurnId::new(),
            },
            sequence_no: 0,
            created_at_ms: 0,
            payload: crate::protocol::HistoryItemPayload::UserTurn {
                content: vec![crate::protocol::ContentPart::Text {
                    text: "new canonical owner evidence".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
            },
        };
        controller
            .app
            .store
            .protocol_event_store()
            .seed_history_item_for_test(&owner_item)
            .expect("owner history");
        let current_fence = controller
            .app
            .store
            .protocol_event_store()
            .visit_active_history_pages_for_session(
                owner_session_id,
                crate::protocol::MAX_PROTOCOL_PAGE_LIMIT,
                &mut |_| Ok(()),
            )
            .expect("owner fence")
            .append_fence;
        controller.side_chat_contexts.insert(
            owner_session_id,
            SideChatContextMetadata {
                owner_session_id,
                scope: "owner_session",
                as_of_append_position: Some(current_fence.unwrap_or(0).saturating_sub(1)),
                truncated: true,
                owner_unit_count: 1,
                included_owner_unit_count: 1,
                quote_source_history_item_id: None,
            },
        );
        let idle_projection = controller.side_chat_projection();
        assert_eq!(
            idle_projection.context_as_of_append_position,
            current_fence.map(|position| position.to_string()),
            "an idle Side Chat must advertise the current owner fence for its next send"
        );
        assert!(
            !idle_projection.context_truncated,
            "metadata from a completed request must not make the next request look truncated"
        );
        let stale_fence = Some(current_fence.unwrap_or(0).saturating_add(1));
        let binding = controller
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .expect("binding read")
            .expect("binding");

        let error = controller
            .start_side_chat(
                owner_session_id,
                binding.id,
                binding.request_generation,
                binding.draft_revision,
                stale_fence,
                None,
                "must remain retryable".to_string(),
            )
            .expect_err("stale owner context must fail before provider transport");
        assert!(error.contains("owner context changed"));
        assert!(!controller.side_chat_runs.contains_key(&owner_session_id));
        let preserved = controller
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .expect("preserved binding read")
            .expect("preserved binding");
        assert_eq!(preserved.request_generation, binding.request_generation);
        assert_eq!(preserved.draft_revision, binding.draft_revision);
        assert!(preserved.persisted_draft.is_empty());
        let hidden = controller
            .app
            .session_service
            .get_session(binding.conversation_session_id)
            .await
            .expect("hidden session");
        assert_eq!(hidden.status, SessionStatus::Idle);
        assert!(
            controller
                .app
                .store
                .protocol_event_store()
                .list_history_items_for_session(binding.conversation_session_id)
                .expect("hidden history")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn side_chat_start_ack_owns_process_and_canonical_admission() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("side provider");
        listener
            .set_nonblocking(true)
            .expect("nonblocking side provider");
        let endpoint = format!(
            "http://{}",
            listener.local_addr().expect("provider address")
        );
        let (accepted_tx, accepted_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let (_temp, _root, mut controller, owner_session_id) = side_chat_test_controller().await;
        controller
            .configure_side_chat(
                owner_session_id,
                endpoint,
                "google/gemma-4-12b-qat".to_string(),
                String::new(),
                ProviderProfile::OpenAiCompatible,
            )
            .expect("configure side chat");
        let binding = controller
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .expect("binding read")
            .expect("binding");
        let provider = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = accepted_tx.send(());
                        let _ = release_rx.recv_timeout(std::time::Duration::from_secs(5));
                        drop(stream);
                        return;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if release_rx.try_recv().is_ok() || std::time::Instant::now() >= deadline {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) => panic!("side provider accept failed: {error}"),
                }
            }
        });

        controller
            .start_side_chat(
                owner_session_id,
                binding.id,
                binding.request_generation,
                binding.draft_revision,
                None,
                None,
                "canonical side question".to_string(),
            )
            .expect("admitted side chat start");
        accepted_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("provider connection");

        let projection = controller
            .app
            .store
            .side_chat_repo()
            .conversation_projection(owner_session_id)
            .expect("conversation projection")
            .expect("durable conversation");
        assert_eq!(projection.binding.request_generation, 1);
        assert_eq!(projection.binding.persisted_draft, "");
        assert_eq!(projection.status, SessionStatus::Running);
        assert!(projection.messages.iter().any(|message| {
            message.role == crate::storage::SideChatConversationRole::User
                && message.content == "canonical side question"
        }));
        assert!(
            controller
                .app
                .store
                .active_runs()
                .is_active(binding.conversation_session_id)
        );
        assert!(
            controller
                .app
                .store
                .try_acquire_run_process_lease(binding.conversation_session_id)
                .is_err()
        );

        let active_turn_id = controller
            .app
            .store
            .active_runs()
            .active_turn_id(binding.conversation_session_id)
            .expect("registered side turn");
        assert_eq!(
            controller.app.store.active_runs().cancel_turn(
                binding.conversation_session_id,
                active_turn_id,
                TurnInterruptionCause::UserStop,
            ),
            crate::runtime::ActiveRunInterruptOutcome::Applied,
            "the registry and provider request must share one cancellation owner"
        );
        let _ = release_tx.send(());
        for _ in 0..300 {
            controller.drain_runtime_messages();
            if !controller.side_chat_runs.contains_key(&owner_session_id) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!controller.side_chat_runs.contains_key(&owner_session_id));
        assert!(
            !controller
                .app
                .store
                .active_runs()
                .is_active(binding.conversation_session_id)
        );
        let _released_process_owner = controller
            .app
            .store
            .try_acquire_run_process_lease(binding.conversation_session_id)
            .expect("released side process owner");
        provider.join().expect("side provider thread");
    }

    #[tokio::test]
    async fn active_side_chat_rejects_stale_owner_actions_then_deletes_durably() {
        use crate::session::SessionRepository as _;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("side provider");
        listener
            .set_nonblocking(true)
            .expect("nonblocking side provider");
        let endpoint = format!(
            "http://{}",
            listener.local_addr().expect("provider address")
        );
        let (accepted_tx, accepted_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let (temp, _root, mut controller, owner_session_id) = side_chat_test_controller().await;
        controller.state.hub_connection = Some(crate::hub::HubConnection::new(
            crate::hub::HubSettingsStore::new(
                Utf8PathBuf::from_path_buf(temp.path().join("hub-settings.json")).unwrap(),
            ),
        ));
        let idle = controller
            .next_web_state()
            .expect("idle route availability");
        let idle_hub = idle.hub.expect("Hub route settings");
        assert_eq!(idle_hub.side_chat_mode, crate::hub::HubRouteMode::Direct);
        assert!(idle_hub.can_change_side_chat_mode);
        assert!(!controller.hub_context_active(crate::hub::HubReviewContext::SideChat));
        controller
            .configure_side_chat(
                owner_session_id,
                endpoint,
                "google/gemma-4-12b-qat".to_string(),
                String::new(),
                ProviderProfile::OpenAiCompatible,
            )
            .expect("configure side chat");
        let initial = controller
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .expect("binding read")
            .expect("binding");
        let conversation_session_id = initial.conversation_session_id;
        let provider = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = accepted_tx.send(());
                        let _ = release_rx.recv_timeout(std::time::Duration::from_secs(5));
                        drop(stream);
                        return;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if release_rx.try_recv().is_ok() || std::time::Instant::now() >= deadline {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) => panic!("side provider accept failed: {error}"),
                }
            }
        });

        controller
            .start_side_chat(
                owner_session_id,
                initial.id,
                initial.request_generation,
                initial.draft_revision,
                None,
                None,
                "delete this side conversation".to_string(),
            )
            .expect("admitted side chat start");
        accepted_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("provider connection");
        let admitted = controller
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .expect("admitted binding read")
            .expect("admitted binding");

        let other = controller
            .app
            .store
            .session_repo()
            .create_session(crate::session::NewSession {
                project_id: controller.app.workspace.project_id,
                title: "other selected session".to_string(),
                cwd: controller.app.workspace.root.clone(),
                model: controller.app.config.model.model.clone(),
                base_url: controller.app.config.model.base_url.clone(),
                access_mode: controller.app.config.permissions.access_mode,
                provider_connection: None,
            })
            .await
            .expect("other main session");
        let other_snapshot = load_snapshot_for_selection(&controller.app, Some(other.id))
            .await
            .expect("other owner snapshot");
        controller.state.replace_snapshot(other_snapshot);
        controller.state.app_state.current_session_id = Some(other.id);
        controller.state.app_state.current_session_title = other.title;

        let background = controller
            .next_web_state()
            .expect("background Side route availability");
        assert_ne!(background.side_chat.status, "running");
        let background_hub = background.hub.expect("Hub route settings");
        assert!(controller.hub_context_active(crate::hub::HubReviewContext::SideChat));
        assert_eq!(
            background_hub.can_change_side_chat_mode,
            !controller.hub_context_active(crate::hub::HubReviewContext::SideChat),
            "a background Direct Side run closes the same mode gate used by the command"
        );
        assert!(background_hub.can_change_main_mode);
        assert!(!controller.hub_context_active(crate::hub::HubReviewContext::Main));

        let cancel_error = controller
            .cancel_side_chat(owner_session_id, admitted.id, admitted.request_generation)
            .expect_err("stale owner must not stop an active side chat");
        assert!(cancel_error.contains("selected main session changed"));
        let delete_error = controller
            .delete_side_chat(owner_session_id, admitted.id, admitted.request_generation)
            .expect_err("stale owner must not tombstone an active side chat");
        assert!(delete_error.contains("selected main session changed"));
        assert!(
            !controller
                .side_chat_runs
                .get(&owner_session_id)
                .expect("active owner run")
                .cancel
                .is_cancelled(),
            "the stale Stop target must have no cancellation side effect"
        );
        assert_eq!(
            controller
                .app
                .store
                .side_chat_repo()
                .get_by_owner(owner_session_id)
                .expect("unchanged binding read")
                .expect("unchanged binding")
                .delete_requested_at_ms,
            None,
            "the stale delete target must not persist a tombstone"
        );

        let owner_snapshot = load_snapshot_for_selection(&controller.app, Some(owner_session_id))
            .await
            .expect("restore owner snapshot");
        controller.state.replace_snapshot(owner_snapshot);
        controller.state.app_state.current_session_id = Some(owner_session_id);
        controller.state.app_state.current_session_title = "side chat owner".to_string();

        controller
            .delete_side_chat(owner_session_id, admitted.id, admitted.request_generation)
            .expect("accept active side deletion");

        let deleting = controller
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .expect("deleting binding read")
            .expect("deleting binding");
        assert!(
            deleting.delete_requested_at_ms.is_some(),
            "the destructive confirmation must be durable before provider cancellation settles"
        );
        let active = controller
            .side_chat_runs
            .get(&owner_session_id)
            .expect("side run remains owned while deletion finalizes");
        assert!(active.delete_after_finish);
        assert!(active.cancel.is_cancelled());

        let _ = release_tx.send(());
        for _ in 0..300 {
            controller.drain_runtime_messages();
            if !controller.side_chat_runs.contains_key(&owner_session_id) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!controller.side_chat_runs.contains_key(&owner_session_id));
        let settled = controller
            .next_web_state()
            .expect("settled Side route availability");
        assert!(!controller.hub_context_active(crate::hub::HubReviewContext::SideChat));
        assert!(
            settled
                .hub
                .expect("Hub route settings")
                .can_change_side_chat_mode
        );
        assert!(
            controller
                .app
                .store
                .side_chat_repo()
                .get_by_owner(owner_session_id)
                .expect("deleted binding read")
                .is_none()
        );
        assert!(
            controller
                .app
                .session_service
                .get_session(conversation_session_id)
                .await
                .is_err(),
            "the hidden canonical side conversation must be deleted"
        );
        controller
            .app
            .session_service
            .get_session(owner_session_id)
            .await
            .expect("main owner remains available");
        provider.join().expect("side provider thread");
    }

    #[tokio::test]
    async fn side_chat_process_owner_failure_preserves_draft_and_generation() {
        let (_temp, _root, mut controller, owner_session_id) = side_chat_test_controller().await;
        controller
            .configure_side_chat(
                owner_session_id,
                "http://127.0.0.1:1234".to_string(),
                "google/gemma-4-12b-qat".to_string(),
                String::new(),
                ProviderProfile::OpenAiCompatible,
            )
            .expect("configure side chat");
        let repository = controller.app.store.side_chat_repo();
        let binding = repository
            .get_by_owner(owner_session_id)
            .expect("binding read")
            .expect("binding");
        let draft = repository
            .update_draft(
                owner_session_id,
                binding.id,
                binding.draft_revision,
                "keep this draft",
            )
            .expect("persist draft")
            .binding;
        let process_owner = controller
            .app
            .store
            .try_acquire_run_process_lease(binding.conversation_session_id)
            .expect("competing process owner");

        let error = controller
            .start_side_chat(
                owner_session_id,
                binding.id,
                binding.request_generation,
                draft.draft_revision,
                None,
                None,
                "must not be admitted".to_string(),
            )
            .expect_err("competing process owner must reject start");

        assert!(error.contains("another live process"));
        assert!(!controller.side_chat_runs.contains_key(&owner_session_id));
        let preserved = repository
            .get_by_owner(owner_session_id)
            .expect("preserved binding read")
            .expect("preserved binding");
        assert_eq!(preserved.request_generation, binding.request_generation);
        assert_eq!(preserved.persisted_draft, "keep this draft");
        assert_eq!(preserved.draft_revision, draft.draft_revision);
        let projection = repository
            .conversation_projection(owner_session_id)
            .expect("preserved conversation")
            .expect("conversation projection");
        assert_eq!(projection.status, SessionStatus::Idle);
        assert!(projection.messages.is_empty());
        drop(process_owner);
    }

    #[tokio::test]
    async fn side_chat_submit_rejects_stale_draft_revision_before_starting_a_worker() {
        let (_temp, _root, mut controller, owner_session_id) = side_chat_test_controller().await;
        controller
            .configure_side_chat(
                owner_session_id,
                "http://127.0.0.1:1234".to_string(),
                "google/gemma-4-12b-qat".to_string(),
                String::new(),
                ProviderProfile::OpenAiCompatible,
            )
            .expect("configure side chat");
        let repository = controller.app.store.side_chat_repo();
        let binding = repository
            .get_by_owner(owner_session_id)
            .expect("binding read")
            .expect("binding");
        let saved = repository
            .update_draft(
                owner_session_id,
                binding.id,
                binding.draft_revision,
                "newer durable draft",
            )
            .expect("save newer draft")
            .binding;

        let error = controller
            .start_side_chat(
                owner_session_id,
                binding.id,
                binding.request_generation,
                binding.draft_revision,
                None,
                None,
                "stale submitted text".to_string(),
            )
            .expect_err("stale draft revision must reject submit");

        assert!(error.contains("side chat draft changed"));
        assert!(!controller.side_chat_runs.contains_key(&owner_session_id));
        let preserved = repository
            .get_by_owner(owner_session_id)
            .expect("preserved binding read")
            .expect("preserved binding");
        assert_eq!(preserved.request_generation, binding.request_generation);
        assert_eq!(preserved.persisted_draft, "newer durable draft");
        assert_eq!(preserved.draft_revision, saved.draft_revision);
    }

    #[tokio::test]
    async fn side_chat_admission_failure_preserves_draft_and_generation() {
        let (_temp, _root, mut controller, owner_session_id) = side_chat_test_controller().await;
        controller
            .configure_side_chat(
                owner_session_id,
                "http://127.0.0.1:1234".to_string(),
                "google/gemma-4-12b-qat".to_string(),
                String::new(),
                ProviderProfile::OpenAiCompatible,
            )
            .expect("configure side chat");
        let side_chat_repository = controller.app.store.side_chat_repo();
        let binding = side_chat_repository
            .get_by_owner(owner_session_id)
            .expect("binding read")
            .expect("binding");
        let draft = side_chat_repository
            .update_draft(
                owner_session_id,
                binding.id,
                binding.draft_revision,
                "keep this draft",
            )
            .expect("persist draft")
            .binding;
        let blocking_turn_id = TurnId::new();
        let blocking_user_turn = UserTurn {
            turn_id: blocking_turn_id,
            items: vec![UserInputItem::Text {
                text: "existing request".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
        };
        let blocking_admission = controller
            .app
            .store
            .session_repo()
            .admit_session_turn_with_initial_user_turn(
                binding.conversation_session_id,
                blocking_turn_id,
                Some(&blocking_user_turn),
            )
            .await
            .expect("blocking admission")
            .expect("admitted blocker");

        let error = controller
            .start_side_chat(
                owner_session_id,
                binding.id,
                binding.request_generation,
                draft.draft_revision,
                None,
                None,
                "must not replace the active turn".to_string(),
            )
            .expect_err("active canonical turn must reject admission");

        assert!(error.contains("could not admit request generation"));
        assert!(!controller.side_chat_runs.contains_key(&owner_session_id));
        let preserved = side_chat_repository
            .get_by_owner(owner_session_id)
            .expect("preserved binding read")
            .expect("preserved binding");
        assert_eq!(preserved.request_generation, binding.request_generation);
        assert_eq!(preserved.persisted_draft, "keep this draft");
        assert_eq!(preserved.draft_revision, draft.draft_revision);
        let terminal = crate::session::DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Interrupted {
                cause: TurnInterruptionCause::UserStop,
            },
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        };
        controller
            .app
            .store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                binding.conversation_session_id,
                blocking_admission.admission_id,
                &RunEvent::TurnTerminal {
                    session_id: binding.conversation_session_id,
                    terminal: Box::new(terminal),
                },
                blocking_turn_id,
                None,
                None,
            )
            .await
            .expect("terminalize blocker");
    }

    #[tokio::test]
    async fn side_chat_ack_timeout_retains_worker_until_late_admission_is_terminal() {
        let (_temp, _root, mut controller, owner_session_id) = side_chat_test_controller().await;
        controller
            .configure_side_chat(
                owner_session_id,
                "http://127.0.0.1:1234".to_string(),
                "google/gemma-4-12b-qat".to_string(),
                String::new(),
                ProviderProfile::OpenAiCompatible,
            )
            .expect("configure side chat");
        let side_chat_repository = controller.app.store.side_chat_repo();
        let binding = side_chat_repository
            .get_by_owner(owner_session_id)
            .expect("binding read")
            .expect("binding");
        let saved_draft = side_chat_repository
            .update_draft(
                owner_session_id,
                binding.id,
                binding.draft_revision,
                "late draft",
            )
            .expect("persist draft")
            .binding;
        let database_path = controller.app.store.paths().database_path.clone();
        let blocker = rusqlite::Connection::open(database_path.as_std_path())
            .expect("blocking sqlite connection");
        blocker
            .execute_batch("BEGIN IMMEDIATE")
            .expect("hold sqlite writer");

        let error = controller
            .start_side_chat_with_ack_timeout(
                owner_session_id,
                binding.id,
                binding.request_generation,
                saved_draft.draft_revision,
                None,
                None,
                "late canonical question".to_string(),
                std::time::Duration::from_millis(50),
            )
            .expect_err("blocked admission must exceed the acknowledgement timeout");

        assert!(error.contains("did not acknowledge within 50ms"));
        let pending = controller
            .side_chat_runs
            .get(&owner_session_id)
            .expect("timed-out worker remains owned");
        assert!(pending.cancel.is_cancelled());
        assert_eq!(pending.run_generation, 1);
        blocker
            .execute_batch("COMMIT")
            .expect("release sqlite writer");

        for _ in 0..300 {
            controller.drain_runtime_messages();
            if !controller.side_chat_runs.contains_key(&owner_session_id) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!controller.side_chat_runs.contains_key(&owner_session_id));
        let projection = side_chat_repository
            .conversation_projection(owner_session_id)
            .expect("late conversation projection")
            .expect("late conversation");
        assert_eq!(projection.binding.request_generation, 1);
        assert_eq!(projection.binding.persisted_draft, "");
        assert_eq!(projection.status, SessionStatus::Cancelled);
        assert!(projection.messages.iter().any(|message| {
            message.role == crate::storage::SideChatConversationRole::User
                && message.content == "late canonical question"
        }));
        assert!(
            !controller
                .app
                .store
                .active_runs()
                .is_active(binding.conversation_session_id)
        );
        let _released_process_owner = controller
            .app
            .store
            .try_acquire_run_process_lease(binding.conversation_session_id)
            .expect("late side process owner released after terminal");
    }

    #[tokio::test]
    async fn provider_local_context_apply_does_not_require_a_catalog_reload() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let baseline = controller
            .state
            .provider_config
            .effective_config
            .model
            .clone();
        let next_context_window = baseline.context_window.saturating_add(1);

        controller.accept_provider_action_input(
            baseline.base_url.clone(),
            baseline.provider_profile,
            baseline.api_key_env.clone().unwrap_or_default(),
            next_context_window.to_string(),
            baseline.model.clone(),
        );

        assert_eq!(
            controller.state.provider_config.provider_loaded_base_url,
            None
        );
        assert!(controller.state.can_apply_provider_selection());
        assert!(controller.apply_provider_session());
        let applied = &controller.state.provider_config.effective_config.model;
        assert_eq!(applied.context_window, next_context_window);
        assert_eq!(applied.max_output_tokens, baseline.max_output_tokens);
        assert_eq!(applied.extra_body_json, None);
    }

    #[tokio::test]
    async fn provider_manual_target_apply_clears_hidden_provider_state_without_catalog_evidence() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        controller
            .state
            .provider_config
            .effective_config
            .model
            .extra_headers
            .insert(
                "Authorization".to_string(),
                "must-not-cross-hosts".to_string(),
            );
        controller
            .state
            .provider_config
            .effective_config
            .model
            .extra_body_json = Some(serde_json::json!({
            "api_key": "must-not-cross-hosts-in-body",
            "num_ctx": 4096
        }));
        let baseline = controller
            .state
            .provider_config
            .effective_config
            .model
            .clone();

        controller.accept_provider_action_input(
            "http://127.0.0.1:8119".to_string(),
            ProviderProfile::OpenAiCompatible,
            String::new(),
            baseline.context_window.to_string(),
            baseline.model,
        );

        assert!(!controller.state.provider_catalog_owns_current_target());
        assert!(controller.state.can_apply_provider_selection());
        assert!(controller.apply_provider_session());
        let applied = &controller.state.provider_config.effective_config.model;
        assert_eq!(applied.base_url, "http://127.0.0.1:8119");
        assert_eq!(applied.provider_profile, ProviderProfile::OpenAiCompatible);
        assert!(applied.extra_headers.is_empty());
        assert_eq!(applied.extra_body_json, None);
    }

    #[tokio::test]
    async fn provider_same_target_key_and_local_context_edits_drop_legacy_generation_state() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        controller
            .state
            .provider_config
            .effective_config
            .model
            .extra_headers
            .insert("X-Provider-Tenant".to_string(), "tenant-a".to_string());
        controller
            .state
            .provider_config
            .effective_config
            .model
            .extra_body_json = Some(serde_json::json!({
            "num_ctx": 8192,
            "legacy_provider_option": true
        }));
        let baseline = controller
            .state
            .provider_config
            .effective_config
            .model
            .clone();

        controller.accept_provider_action_input(
            baseline.base_url,
            baseline.provider_profile,
            "OPENAI_API_KEY".to_string(),
            baseline.context_window.saturating_add(1).to_string(),
            baseline.model,
        );

        assert!(controller.apply_provider_session());
        let applied = &controller.state.provider_config.effective_config.model;
        assert_eq!(
            applied
                .extra_headers
                .get("X-Provider-Tenant")
                .map(String::as_str),
            Some("tenant-a")
        );
        assert_eq!(applied.extra_body_json, None);
    }

    #[tokio::test]
    async fn provider_global_persistence_candidate_explicitly_clears_old_headers() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let mut global = controller.state.global_config().clone();
        global
            .model
            .extra_headers
            .insert("Authorization".to_string(), "old-host-secret".to_string());
        controller.state.replace_global_config(global.clone());
        let mut next = global;
        next.model.base_url = "http://127.0.0.1:8119/v1".to_string();
        next.model.provider_profile = ProviderProfile::OpenAiCompatible;
        next.model.extra_headers.clear();

        let candidate = controller
            .provider_config_persistence_candidate(&next)
            .expect("provider persistence candidate");
        let headers = candidate
            .fields
            .iter()
            .find(|field| field.key == ConfigField::ExtraHeadersJson)
            .expect("explicit extra headers field");
        assert_eq!(headers.value, "{}");
        assert!(
            headers.dirty,
            "the old header owner must be cleared on disk"
        );
    }

    #[tokio::test]
    async fn provider_global_persistence_does_not_reapply_legacy_host_generation_fields() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let mut global = controller.state.global_config().clone();
        global.model.max_output_tokens = 4_096;
        global.model.temperature = Some(0.2);
        global.model.top_p = Some(0.8);
        global.model.top_k = Some(40);
        global.model.supports_reasoning = true;
        global.model.extra_body_json = Some(serde_json::json!({
            "chat_template_kwargs": { "enable_thinking": false }
        }));
        controller.state.replace_global_config(global.clone());
        let mut provider_selection = global.clone();
        provider_selection.model.max_output_tokens = 8_192;
        provider_selection.model.temperature = Some(0.9);
        provider_selection.model.top_p = Some(0.95);
        provider_selection.model.top_k = Some(80);
        provider_selection.model.supports_reasoning = false;
        provider_selection.model.extra_body_json = None;

        let candidate = controller
            .provider_config_persistence_candidate(&provider_selection)
            .expect("provider persistence candidate");
        let resolved = candidate
            .build_resolved_config(&global)
            .expect("resolved provider persistence candidate");
        let defaults = ResolvedConfig::default();

        assert_eq!(
            resolved.model.max_output_tokens,
            defaults.model.max_output_tokens
        );
        assert_eq!(resolved.model.temperature, defaults.model.temperature);
        assert_eq!(resolved.model.top_p, defaults.model.top_p);
        assert_eq!(resolved.model.top_k, defaults.model.top_k);
        assert_eq!(
            resolved.model.supports_reasoning,
            defaults.model.supports_reasoning
        );
        assert_eq!(
            resolved.model.extra_body_json,
            defaults.model.extra_body_json
        );
    }

    #[tokio::test]
    async fn provider_local_context_apply_does_not_consume_stale_catalog_metadata() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let baseline = controller
            .state
            .provider_config
            .effective_config
            .model
            .clone();
        let stale_url = "http://127.0.0.1:4321".to_string();
        controller.accept_provider_action_input(
            stale_url.clone(),
            baseline.provider_profile,
            baseline.api_key_env.clone().unwrap_or_default(),
            baseline.context_window.to_string(),
            baseline.model.clone(),
        );
        controller
            .state
            .begin_provider_model_load(stale_url.clone());
        controller
            .state
            .finish_provider_model_load(vec![ProviderModelInfo {
                id: baseline.model.clone(),
                display_name: Some("stale catalog model".to_string()),
                context_window: Some(baseline.context_window.saturating_add(10)),
                max_output_tokens: Some(baseline.max_output_tokens.saturating_add(10)),
                supports_images: Some(!baseline.supports_images),
                supports_tools: Some(!baseline.supports_tools),
                supports_reasoning: Some(!baseline.supports_reasoning),
                max_parallel_predictions: Some(baseline.max_parallel_predictions.saturating_add(1)),
                load_state: crate::llm::ProviderModelLoadState::Unknown,
                source: "provider_catalog".to_string(),
            }]);
        assert!(controller.state.provider_catalog_owns_current_target());

        controller.state.show_provider_editor();
        assert!(!controller.state.provider_catalog_owns_current_target());
        controller.accept_provider_action_input(
            baseline.base_url.clone(),
            baseline.provider_profile,
            baseline.api_key_env.clone().unwrap_or_default(),
            baseline.context_window.saturating_add(1).to_string(),
            baseline.model.clone(),
        );

        assert!(controller.apply_provider_session());
        let applied = &controller.state.provider_config.effective_config.model;
        assert_eq!(applied.supports_images, baseline.supports_images);
        assert_eq!(applied.supports_tools, baseline.supports_tools);
        assert_eq!(applied.supports_reasoning, baseline.supports_reasoning);
        assert_eq!(
            applied.max_parallel_predictions,
            baseline.max_parallel_predictions
        );
    }

    #[tokio::test]
    async fn provider_local_context_apply_preserves_metadata_from_the_effective_config() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let baseline = controller
            .state
            .provider_config
            .effective_config
            .model
            .clone();
        controller
            .state
            .begin_provider_model_load(baseline.base_url.clone());
        controller
            .state
            .finish_provider_model_load(vec![ProviderModelInfo {
                id: baseline.model.clone(),
                display_name: Some("current catalog model".to_string()),
                context_window: Some(baseline.context_window),
                max_output_tokens: Some(baseline.max_output_tokens),
                supports_images: Some(!baseline.supports_images),
                supports_tools: Some(!baseline.supports_tools),
                supports_reasoning: Some(!baseline.supports_reasoning),
                max_parallel_predictions: Some(baseline.max_parallel_predictions.saturating_add(1)),
                load_state: crate::llm::ProviderModelLoadState::Unknown,
                source: "provider_catalog".to_string(),
            }]);
        assert!(controller.state.provider_catalog_owns_current_target());
        controller.accept_provider_action_input(
            baseline.base_url.clone(),
            baseline.provider_profile,
            baseline.api_key_env.clone().unwrap_or_default(),
            baseline.context_window.saturating_add(1).to_string(),
            baseline.model.clone(),
        );

        assert!(controller.apply_provider_session());
        let applied = &controller.state.provider_config.effective_config.model;
        assert_eq!(applied.supports_images, baseline.supports_images);
        assert_eq!(applied.supports_tools, baseline.supports_tools);
        assert_eq!(applied.supports_reasoning, baseline.supports_reasoning);
        assert_eq!(
            applied.max_parallel_predictions,
            baseline.max_parallel_predictions
        );
    }

    #[tokio::test]
    async fn finished_interruption_projects_cancelled_even_if_the_live_view_is_still_running() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let session_id = SessionId::new();
        controller.state.app_state.current_session_id = Some(session_id);
        controller.state.app_state.run_status = crate::tui::state::RunStatus::Running;
        controller.state.begin_agent_run();
        controller.run_lifecycle.begin(17, RunControl::new());
        let summary = RunSummary::from_terminal(
            session_id,
            crate::protocol::TurnId::new(),
            crate::session::DurableTurnTerminal {
                outcome: crate::protocol::TurnTerminalOutcome::Interrupted {
                    cause: crate::protocol::TurnInterruptionCause::UserStop,
                },
                final_response_id: None,
                tool_call_count: 4,
                failed_tool_count: 1,
                change_count: 0,
                metrics: crate::session::RunMetrics {
                    model_request_count: 3,
                    ..Default::default()
                },
            },
        );

        controller
            .runtime_tx
            .send(RuntimeMessage::Finished {
                run_generation: 17,
                result: Ok(summary),
            })
            .expect("durable interrupted finish");
        controller.drain_runtime_messages();

        assert!(!controller.run_lifecycle.root_is_active());
        assert_eq!(
            controller.state.app_state.run_status,
            crate::tui::state::RunStatus::Cancelled
        );
        assert_eq!(
            controller.state.app_state.interruption_cause,
            Some(crate::protocol::TurnInterruptionCause::UserStop)
        );
        assert_eq!(
            controller.state.status_code,
            super::super::state::DesktopStatusCode::UserStopped
        );
        assert_eq!(controller.state.app_state.progress.model_requests, 3);
        assert_eq!(controller.state.app_state.progress.tool_calls_started, 4);
        assert_eq!(controller.state.app_state.progress.tool_calls_failed, 1);
    }

    #[tokio::test]
    async fn rejoined_running_session_observes_an_exact_terminal_committed_by_another_store() {
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
        let app = build_test_app(&root, store).await;
        let session = app
            .store
            .session_repo()
            .create_session(NewSession {
                project_id: app.workspace.project_id,
                title: "cross-process owner".to_string(),
                cwd: root.clone(),
                model: app.config.model.model.clone(),
                base_url: app.config.model.base_url.clone(),
                access_mode: app.config.permissions.access_mode,
                provider_connection: None,
            })
            .await
            .expect("session");
        let turn_id = TurnId::new();
        let admission = app
            .store
            .session_repo()
            .admit_session_turn(session.id, turn_id)
            .await
            .expect("admission")
            .expect("turn admitted");
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
            controller.state.app_state.run_status,
            crate::tui::state::RunStatus::Running
        );
        assert!(
            controller
                .session_runtime_listener
                .as_ref()
                .is_some_and(|listener| {
                    listener.target.session_id == session.id && listener.target.turn_id == turn_id
                })
        );
        let listener = controller
            .session_runtime_listener
            .as_ref()
            .expect("listener")
            .generation;
        controller
            .runtime_tx
            .send(RuntimeMessage::CanonicalSessionEvent {
                listener_generation: listener.saturating_add(1),
                target: SessionRuntimeListenerTarget {
                    workspace_root: controller.app.workspace.root.clone(),
                    session_id: session.id,
                    turn_id,
                },
                event: RuntimeEvent {
                    id: crate::protocol::RuntimeEventId::new(),
                    session_id: session.id,
                    turn_id,
                    sequence_no: 99,
                    created_at_ms: 1,
                    msg: RuntimeEventMsg::TurnTerminal {
                        terminal: Box::new(crate::session::DurableTurnTerminal {
                            outcome: crate::protocol::TurnTerminalOutcome::Failed {
                                error: "stale listener must not win".to_string(),
                            },
                            final_response_id: None,
                            tool_call_count: 0,
                            failed_tool_count: 0,
                            change_count: 0,
                            metrics: Default::default(),
                        }),
                    },
                },
            })
            .expect("stale listener message");
        controller.drain_runtime_messages();
        assert_eq!(
            controller.state.app_state.run_status,
            crate::tui::state::RunStatus::Running,
            "a stale listener generation cannot terminate the selected run"
        );
        let stale_turn_id = TurnId::new();
        controller
            .runtime_tx
            .send(RuntimeMessage::CanonicalSessionEvent {
                listener_generation: listener,
                target: SessionRuntimeListenerTarget {
                    workspace_root: controller.app.workspace.root.clone(),
                    session_id: session.id,
                    turn_id: stale_turn_id,
                },
                event: RuntimeEvent {
                    id: crate::protocol::RuntimeEventId::new(),
                    session_id: session.id,
                    turn_id: stale_turn_id,
                    sequence_no: 1,
                    created_at_ms: 1,
                    msg: RuntimeEventMsg::TurnTerminal {
                        terminal: Box::new(crate::session::DurableTurnTerminal {
                            outcome: crate::protocol::TurnTerminalOutcome::Completed,
                            final_response_id: None,
                            tool_call_count: 0,
                            failed_tool_count: 0,
                            change_count: 0,
                            metrics: Default::default(),
                        }),
                    },
                },
            })
            .expect("stale turn message");
        controller.drain_runtime_messages();
        assert_eq!(
            controller.state.app_state.run_status,
            crate::tui::state::RunStatus::Running,
            "a stale listener turn cannot terminate the selected run"
        );

        let external_sqlite =
            crate::storage::SqliteStore::open(&paths).expect("external sqlite connection");
        let external_store = crate::storage::StoreBundle::new(external_sqlite);
        let terminal = crate::session::DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::UserStop,
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
        external_store
            .session_repo()
            .terminalize_admitted_turn_with_protocol_event(
                session.id,
                admission.admission_id,
                &RunEvent::TurnTerminal {
                    session_id: session.id,
                    terminal: Box::new(terminal.clone()),
                },
                turn_id,
                None,
                None,
            )
            .await
            .expect("external terminal commit");
        let canonical_terminal = external_store
            .protocol_event_store()
            .list_runtime_events(session.id, turn_id)
            .expect("canonical runtime events")
            .into_iter()
            .find(RuntimeEvent::is_terminal)
            .expect("canonical terminal");
        controller
            .app
            .session_event_hub
            .publisher()
            .publish(canonical_terminal.clone())
            .expect("live terminal wake");
        controller
            .app
            .session_event_hub
            .publisher()
            .publish(canonical_terminal)
            .expect("duplicate live wake is harmless");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while controller.state.app_state.run_status == crate::tui::state::RunStatus::Running
            && std::time::Instant::now() < deadline
        {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            controller.drain_runtime_messages();
        }

        assert_eq!(
            controller.state.app_state.run_status,
            crate::tui::state::RunStatus::Cancelled
        );
        assert_eq!(
            controller.state.app_state.interruption_cause,
            Some(crate::protocol::TurnInterruptionCause::UserStop)
        );
        assert_eq!(controller.state.app_state.progress.model_requests, 5);
        assert_eq!(controller.state.app_state.progress.tool_calls_started, 7);
        assert_eq!(controller.state.app_state.progress.tool_calls_failed, 2);
        assert!(controller.session_runtime_listener.is_none());
        assert_eq!(
            controller
                .state
                .app_state
                .transcript_entries
                .iter()
                .filter(|entry| entry.title == "Run interrupted")
                .count(),
            0,
            "the canonical refresh owns transcript rows; the lifecycle listener only projects state"
        );
    }

    fn loaded_test_session(
        controller: &DesktopController,
        root: &Utf8Path,
        session_id: SessionId,
        status: SessionStatus,
        cause: Option<TurnInterruptionCause>,
    ) -> LoadedSession {
        let session = SessionRecord {
            id: session_id,
            project_id: controller.app.workspace.project_id,
            title: "approval owner test".to_string(),
            status,
            cwd: root.to_path_buf(),
            model: controller.app.config.model.model.clone(),
            base_url: controller.app.config.model.base_url.clone(),
            access_mode: controller.app.config.permissions.access_mode,
            model_parameters: crate::session::SessionModelParameters::default(),
            provider_connection: None,
            session_settings_revision: 0,
            created_at_ms: 1,
            updated_at_ms: 2,
            completed_at_ms: matches!(
                status,
                SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed
            )
            .then_some(2),
        };
        let turn_items = cause
            .map(|cause| crate::protocol::TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id,
                turn_id: crate::protocol::TurnId::new(),
                source_item_id: None,
                sequence_no: 1,
                payload: crate::protocol::TurnItemPayload::Terminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Interrupted { cause },
                },
            })
            .into_iter()
            .collect::<Vec<_>>();
        let latest_turn_id = turn_items.last().map(|item| item.turn_id);
        LoadedSession {
            read: crate::session::CanonicalSessionRead {
                session: session.clone(),
                history: crate::session::CanonicalHistoryPage {
                    session: session.clone(),
                    offset: 0,
                    limit: usize::MAX,
                    total: 0,
                    has_more: false,
                    items: Vec::new(),
                },
                turns: crate::session::CanonicalTurnPage {
                    session,
                    offset: 0,
                    limit: DESKTOP_TURN_PAGE_LIMIT,
                    total: turn_items.len(),
                    has_more: false,
                    items: turn_items,
                },
                pending_turn_inputs: Vec::new(),
                turn_elapsed_ms: Default::default(),
                session_token_usage: Default::default(),
                active_turn_progress: None,
                latest_turn_id,
                active_turn_id: None,
                active_turn_sequence_no: None,
                admission_revision: u64::from(latest_turn_id.is_some()),
            },
            agent_activity_records: None,
        }
    }

    fn loaded_running_test_session(
        controller: &DesktopController,
        root: &Utf8Path,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> LoadedSession {
        let mut loaded =
            loaded_test_session(controller, root, session_id, SessionStatus::Running, None);
        loaded.read.latest_turn_id = Some(turn_id);
        loaded.read.active_turn_id = Some(turn_id);
        loaded.read.active_turn_sequence_no = Some(0);
        loaded.read.admission_revision = 1;
        loaded
    }

    #[tokio::test]
    async fn prompt_enhance_rejects_an_active_background_owner_mutation() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let operation_id = controller.state.begin_steer_submission();

        assert!(!controller.start_prompt_enhance("new review".to_string()));
        assert!(!controller.state.prompt_enhance_pending());
        assert!(controller.state.app_state.prompt_review.is_none());
        assert_eq!(
            controller.state.app_state.status_message.as_deref(),
            Some("prompt enhancement is not currently available")
        );

        assert!(controller.state.finish_steer_submission(operation_id));
    }

    #[tokio::test]
    async fn prompt_review_operations_require_the_captured_idle_run_owner() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let turn_target = ActiveTurnExpectation::Turn {
            turn_id: TurnId::new(),
            revision: 0,
        };

        assert!(
            !controller
                .start_prompt_enhance_at("do not trap this review".to_string(), turn_target,)
        );
        assert!(!controller.state.prompt_enhance_pending());
        assert!(controller.state.app_state.prompt_review.is_none());

        assert!(
            !controller
                .start_review_uncommitted_at("review current changes".to_string(), turn_target,)
        );
        assert!(!controller.run_lifecycle.root_is_active());
        assert_eq!(
            controller.state.app_state.status_message.as_deref(),
            Some("uncommitted review requires the captured idle run owner")
        );

        controller.state.begin_prompt_enhance_at(
            77,
            "seeded Turn review",
            CancellationToken::new(),
            turn_target,
        );
        assert!(
            controller
                .state
                .finish_prompt_enhance(77, "seeded enhanced review".to_string())
        );
        assert!(!controller.send_prompt_review_at(
            77,
            true,
            "seeded enhanced review".to_string(),
            turn_target,
        ));
        let seeded_review_target = controller
            .prompt_review_target(77)
            .expect("seeded review target");
        assert!(!controller.launch_run_with_options(
            "seeded enhanced review".to_string(),
            crate::session::PromptDispatchPart::raw("seeded enhanced review"),
            None,
            Some(seeded_review_target),
            turn_target,
        ));
        assert_eq!(
            controller
                .state
                .app_state
                .prompt_review
                .as_ref()
                .map(|review| review.request_id),
            Some(77),
            "a rejected Turn-owned review remains recoverable through exact cancel"
        );
        assert!(!controller.state.steer_submission_pending());
        assert!(!controller.run_lifecycle.root_is_active());
    }

    #[tokio::test]
    async fn post_run_refresh_blocks_every_new_root_entrypoint_at_central_admission() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let expected_active_turn = controller.current_active_turn_expectation();
        let next_root_run_generation = controller.next_root_run_generation;
        controller.state.mark_post_run_refresh_pending();

        assert!(!controller.start_run_at("new root prompt".to_string(), expected_active_turn,));
        assert!(!controller.start_review_uncommitted_at(
            "review current changes".to_string(),
            expected_active_turn,
        ));

        controller.state.begin_prompt_enhance_at(
            78,
            "review source",
            CancellationToken::new(),
            expected_active_turn,
        );
        assert!(
            controller
                .state
                .finish_prompt_enhance(78, "reviewed prompt".to_string())
        );
        assert!(!controller.send_prompt_review_at(
            78,
            true,
            "reviewed prompt".to_string(),
            expected_active_turn,
        ));

        assert_eq!(
            controller.state.app_state.status_message.as_deref(),
            Some("wait for the completed task to finish refreshing before sending")
        );
        assert!(controller.state.post_run_refresh_pending());
        assert!(!controller.run_lifecycle.root_is_active());
        assert_eq!(
            controller.next_root_run_generation,
            next_root_run_generation
        );
        assert_eq!(
            controller
                .state
                .app_state
                .prompt_review
                .as_ref()
                .map(|review| review.request_id),
            Some(78),
            "rejected review send keeps the exact review recoverable"
        );
    }

    #[tokio::test]
    async fn prompt_review_central_admission_preserves_owner_and_draft_for_every_mutation_class() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        controller
            .state
            .begin_prompt_enhance(78, "review source", CancellationToken::new());
        assert!(
            controller
                .state
                .finish_prompt_enhance(78, "review draft".to_string())
        );

        for mutation_class in [
            "provider model loading",
            "provider configuration",
            "configuration",
            "configuration import",
            "access mode",
            "workspace navigation",
            "workspace picker",
            "overlay replacement",
            "image attachment",
        ] {
            assert!(!controller.ensure_unscoped_prompt_review_action(mutation_class));
            assert_eq!(
                controller
                    .state
                    .app_state
                    .prompt_review
                    .as_ref()
                    .map(|review| (
                        review.request_id,
                        review.raw_prompt_text.as_str(),
                        review.current_draft_text.as_str(),
                    )),
                Some((78, "review source", "review draft")),
                "{mutation_class} must reject without consuming the review owner",
            );
            assert_eq!(
                controller.state.prompt_review_expected_active_turn(78),
                Some(ActiveTurnExpectation::initial_idle()),
            );
            assert_eq!(
                controller.state.view.overlay,
                super::super::state::DesktopOverlay::PromptReview,
            );
        }
    }

    #[tokio::test]
    async fn steer_settlement_has_no_prompt_review_cancellation_capability() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let session_id = SessionId::new();
        controller.state.app_state.current_session_id = Some(session_id);
        controller.state.rebind_composer_owner(Some(session_id));
        controller
            .state
            .begin_prompt_enhance(41, "review A", CancellationToken::new());
        assert!(
            controller
                .state
                .finish_prompt_enhance(41, "enhanced A".to_string())
        );
        let steer_target = SteerSubmissionTarget {
            operation_id: controller.state.begin_steer_submission(),
            workspace_root: controller.app.workspace.root.clone(),
            session_id,
            expected_active_turn: crate::session::ActiveTurnExpectation::initial_idle(),
        };

        controller.state.cancel_prompt_review();
        controller
            .state
            .begin_prompt_enhance(42, "review B", CancellationToken::new());
        assert!(
            controller
                .state
                .finish_prompt_enhance(42, "enhanced B".to_string())
        );
        controller
            .control_tx
            .send(RuntimeMessage::SteerFinished {
                target: steer_target,
                image_paths: Vec::new(),
                result: Ok(()),
            })
            .expect("stale review A steer settlement");

        controller.drain_runtime_messages();

        assert_eq!(
            controller
                .state
                .app_state
                .prompt_review
                .as_ref()
                .map(|review| (review.request_id, review.current_draft_text.as_str())),
            Some((42, "enhanced B"))
        );
        assert!(!controller.state.steer_submission_pending());
    }

    #[tokio::test]
    async fn stale_root_commit_cannot_cancel_a_newer_prompt_review() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        controller
            .state
            .begin_prompt_enhance(51, "review A", CancellationToken::new());
        assert!(
            controller
                .state
                .finish_prompt_enhance(51, "enhanced A".to_string())
        );
        let stale_review_target = controller
            .prompt_review_target(51)
            .expect("review A target");
        controller.pending_root_submission = Some(PendingRootSubmission {
            run_generation: 91,
            owner_workspace_path: controller.root_submission_owner_workspace_path(),
            owner_session_id: None,
            prompt_dispatch: crate::session::PromptDispatchPart::raw("review A"),
            image_paths: Vec::new(),
            prompt_review_to_cancel: Some(stale_review_target),
        });

        controller.state.cancel_prompt_review();
        controller
            .state
            .begin_prompt_enhance(52, "review B", CancellationToken::new());
        assert!(
            controller
                .state
                .finish_prompt_enhance(52, "enhanced B".to_string())
        );

        assert!(controller.commit_pending_root_submission(91));
        assert_eq!(
            controller
                .state
                .app_state
                .prompt_review
                .as_ref()
                .map(|review| (review.request_id, review.current_draft_text.as_str())),
            Some((52, "enhanced B"))
        );
    }

    #[tokio::test]
    async fn first_session_adoption_cancels_the_exact_submitted_prompt_review() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        controller
            .state
            .begin_prompt_enhance(61, "first review", CancellationToken::new());
        assert!(
            controller
                .state
                .finish_prompt_enhance(61, "first enhanced".to_string())
        );
        let review_target = controller
            .prompt_review_target(61)
            .expect("first-session review target");
        controller.pending_root_submission = Some(PendingRootSubmission {
            run_generation: 92,
            owner_workspace_path: controller.root_submission_owner_workspace_path(),
            owner_session_id: None,
            prompt_dispatch: crate::session::PromptDispatchPart::raw("first review"),
            image_paths: Vec::new(),
            prompt_review_to_cancel: Some(review_target),
        });
        let adopted_session_id = SessionId::new();
        controller.state.app_state.current_session_id = Some(adopted_session_id);

        assert!(controller.commit_pending_root_submission(92));
        assert!(controller.state.app_state.prompt_review.is_none());
        assert!(controller.state.composer.is_owned_by(
            &controller.state.snapshot.workspace_path,
            Some(adopted_session_id)
        ));
    }

    #[tokio::test]
    async fn accepted_running_live_refresh_attaches_one_exact_runtime_listener() {
        let (_temp, root, mut controller) = empty_access_test_controller().await;
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        controller.state.app_state.current_session_id = Some(session_id);
        let target = SessionRefreshRequestTarget {
            workspace_root: root.clone(),
            session_id,
        };
        let first_request = controller
            .session_projection_refresh_requests
            .begin(target.clone());
        controller
            .runtime_tx
            .send(RuntimeMessage::LiveSessionRefreshed {
                request_id: first_request,
                target: target.clone(),
                result: Ok(loaded_running_test_session(
                    &controller,
                    &root,
                    session_id,
                    turn_id,
                )),
            })
            .expect("first running refresh");
        controller.drain_runtime_messages();

        let listener = controller
            .session_runtime_listener
            .as_ref()
            .expect("accepted Running projection must attach its canonical cursor");
        assert_eq!(listener.target.session_id, session_id);
        assert_eq!(listener.target.turn_id, turn_id);
        let generation = listener.generation;
        let cancel = listener.cancel.clone();

        let duplicate_request = controller
            .session_projection_refresh_requests
            .begin(target.clone());
        controller
            .runtime_tx
            .send(RuntimeMessage::LiveSessionRefreshed {
                request_id: duplicate_request,
                target,
                result: Ok(loaded_running_test_session(
                    &controller,
                    &root,
                    session_id,
                    turn_id,
                )),
            })
            .expect("duplicate running refresh");
        controller.drain_runtime_messages();

        assert_eq!(
            controller
                .session_runtime_listener
                .as_ref()
                .map(|listener| listener.generation),
            Some(generation),
            "the same canonical target must reuse its listener generation"
        );
        assert!(!cancel.is_cancelled());
    }

    #[tokio::test]
    async fn admitted_user_turn_attaches_its_exact_cursor_without_waiting_for_refresh() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let run_generation = 27;
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        controller
            .run_lifecycle
            .begin(run_generation, RunControl::new());
        controller.state.begin_agent_run();
        controller
            .control_tx
            .send(RuntimeMessage::RunEvent {
                run_generation,
                event: RunEvent::SessionStarted {
                    session_id,
                    title: "new run".to_string(),
                },
            })
            .expect("session bootstrap");
        controller
            .control_tx
            .send(RuntimeMessage::RunEvent {
                run_generation,
                event: RunEvent::UserTurnStored {
                    session_id,
                    turn: Box::new(UserTurn {
                        turn_id,
                        items: vec![UserInputItem::Text {
                            text: "attach the durable cursor".to_string(),
                        }],
                        prompt_dispatch: None,
                        editor_context: None,
                    }),
                },
            })
            .expect("durable admission bootstrap");

        controller.drain_runtime_messages();

        assert!(
            controller
                .session_runtime_listener
                .as_ref()
                .is_some_and(|listener| {
                    listener.target.workspace_root == controller.app.workspace.root
                        && listener.target.session_id == session_id
                        && listener.target.turn_id == turn_id
                }),
            "UserTurnStored must close the listener gap before an async session refresh settles"
        );
    }

    #[tokio::test]
    async fn runtime_listener_generation_rejects_target_aba() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let session_id = SessionId::new();
        let first_turn_id = TurnId::new();
        let other_turn_id = TurnId::new();
        let first_target = SessionRuntimeListenerTarget {
            workspace_root: controller.app.workspace.root.clone(),
            session_id,
            turn_id: first_turn_id,
        };
        let other_target = SessionRuntimeListenerTarget {
            workspace_root: controller.app.workspace.root.clone(),
            session_id,
            turn_id: other_turn_id,
        };
        controller.state.app_state.current_session_id = Some(session_id);
        controller.state.app_state.run_status = crate::tui::state::RunStatus::Running;

        controller.reconcile_session_runtime_listener(Some(first_target.clone()));
        let first_generation = controller
            .session_runtime_listener
            .as_ref()
            .expect("first listener")
            .generation;
        let first_cancel = controller
            .session_runtime_listener
            .as_ref()
            .expect("first listener")
            .cancel
            .clone();
        controller.reconcile_session_runtime_listener(Some(other_target));
        assert!(first_cancel.is_cancelled());
        let other_cancel = controller
            .session_runtime_listener
            .as_ref()
            .expect("replacement listener")
            .cancel
            .clone();
        controller.reconcile_session_runtime_listener(Some(first_target.clone()));
        assert!(other_cancel.is_cancelled());
        let current_generation = controller
            .session_runtime_listener
            .as_ref()
            .expect("re-adopted first target")
            .generation;
        assert!(current_generation > first_generation);

        controller
            .runtime_tx
            .send(RuntimeMessage::CanonicalSessionEvent {
                listener_generation: first_generation,
                target: first_target,
                event: RuntimeEvent {
                    id: crate::protocol::RuntimeEventId::new(),
                    session_id,
                    turn_id: first_turn_id,
                    sequence_no: 1,
                    created_at_ms: 1,
                    msg: RuntimeEventMsg::TurnTerminal {
                        terminal: Box::new(crate::session::DurableTurnTerminal {
                            outcome: crate::protocol::TurnTerminalOutcome::Completed,
                            final_response_id: None,
                            tool_call_count: 0,
                            failed_tool_count: 0,
                            change_count: 0,
                            metrics: Default::default(),
                        }),
                    },
                },
            })
            .expect("stale ABA listener event");
        controller.drain_runtime_messages();

        assert_eq!(
            controller.state.app_state.run_status,
            crate::tui::state::RunStatus::Running
        );
        assert_eq!(
            controller
                .session_runtime_listener
                .as_ref()
                .map(|listener| listener.generation),
            Some(current_generation)
        );
    }

    #[tokio::test]
    async fn worker_finished_overtakes_a_stale_running_refresh_and_leaves_no_listener() {
        let (_temp, root, mut controller) = empty_access_test_controller().await;
        let run_generation = 28;
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        controller.state.app_state.current_session_id = Some(session_id);
        controller.state.begin_agent_run();
        controller
            .run_lifecycle
            .begin(run_generation, RunControl::new());
        let target = SessionRefreshRequestTarget {
            workspace_root: root.clone(),
            session_id,
        };
        let stale_request = controller
            .session_projection_refresh_requests
            .begin(target.clone());
        controller
            .runtime_tx
            .send(RuntimeMessage::LiveSessionRefreshed {
                request_id: stale_request,
                target,
                result: Ok(loaded_running_test_session(
                    &controller,
                    &root,
                    session_id,
                    turn_id,
                )),
            })
            .expect("queued stale running refresh");
        controller
            .control_tx
            .send(RuntimeMessage::Finished {
                run_generation,
                result: Ok(RunSummary::from_terminal(
                    session_id,
                    turn_id,
                    crate::session::DurableTurnTerminal {
                        outcome: crate::protocol::TurnTerminalOutcome::Completed,
                        final_response_id: None,
                        tool_call_count: 0,
                        failed_tool_count: 0,
                        change_count: 0,
                        metrics: Default::default(),
                    },
                )),
            })
            .expect("lossless worker settlement");

        controller.drain_runtime_messages();

        assert!(!controller.run_lifecycle.root_is_active());
        assert!(controller.session_runtime_listener.is_none());
        assert_eq!(
            controller.state.app_state.run_status,
            crate::tui::state::RunStatus::Completed
        );
    }

    #[tokio::test]
    async fn current_stop_request_refresh_applies_once_without_a_new_root_admission() {
        let (_temp, root, mut controller) = empty_access_test_controller().await;
        let session_id = SessionId::new();
        controller.state.app_state.current_session_id = Some(session_id);
        controller.state.app_state.run_status = crate::tui::state::RunStatus::Running;
        controller.state.mark_post_run_refresh_pending();
        let target = SessionRefreshRequestTarget {
            workspace_root: root.clone(),
            session_id,
        };
        let request_id = controller
            .session_projection_refresh_requests
            .begin(target.clone());
        let root_admission_fence = controller.next_root_run_generation;

        controller
            .runtime_tx
            .send(RuntimeMessage::CurrentSessionRefreshed {
                request_id,
                target: target.clone(),
                purpose: CurrentSessionRefreshPurpose::StopRequestRefresh {
                    root_admission_fence,
                    root_stop_attempt: None,
                    expected_active_turn: Some(ActiveTurnExpectation::initial_idle()),
                    in_memory_stop_accepted: true,
                    durable_outcome: Some(ExactRootExecutionStopOutcome::Applied {
                        cancelled: true,
                    }),
                },
                result: Ok(loaded_test_session(
                    &controller,
                    &root,
                    session_id,
                    SessionStatus::Completed,
                    None,
                )),
            })
            .expect("current Stop settlement");
        controller.drain_runtime_messages();

        assert!(!controller.session_projection_refresh_requests.is_pending());
        assert_eq!(
            controller.state.app_state.run_status,
            crate::tui::state::RunStatus::Completed
        );
        assert!(!controller.state.post_run_refresh_pending());
        let settled_status = controller.state.app_state.status_message.clone();

        controller
            .runtime_tx
            .send(RuntimeMessage::CurrentSessionRefreshed {
                request_id,
                target,
                purpose: CurrentSessionRefreshPurpose::StopRequestRefresh {
                    root_admission_fence,
                    root_stop_attempt: None,
                    expected_active_turn: Some(ActiveTurnExpectation::initial_idle()),
                    in_memory_stop_accepted: true,
                    durable_outcome: Some(ExactRootExecutionStopOutcome::Applied {
                        cancelled: true,
                    }),
                },
                result: Err("duplicate Stop settlement".to_string()),
            })
            .expect("duplicate Stop settlement");
        controller.drain_runtime_messages();

        assert_eq!(controller.state.app_state.status_message, settled_status);
        assert!(!controller.session_projection_refresh_requests.is_pending());
    }

    #[tokio::test]
    async fn running_stop_refresh_waits_for_the_matching_interrupted_summary() {
        let (_temp, root, mut controller) = empty_access_test_controller().await;
        let session_id = SessionId::new();
        let run_generation = controller.next_root_run_generation;
        controller.state.app_state.current_session_id = Some(session_id);
        controller.state.app_state.run_status = crate::tui::state::RunStatus::Running;
        controller.state.begin_agent_run();
        controller
            .run_lifecycle
            .begin(run_generation, RunControl::new());
        controller.state.mark_run_stop_requested(
            "run cancellation requested",
            "停止を要求しました。現在の処理を中断しています。",
        );
        controller.state.mark_post_run_refresh_pending();
        let target = SessionRefreshRequestTarget {
            workspace_root: root.clone(),
            session_id,
        };
        let request_id = controller
            .session_projection_refresh_requests
            .begin(target.clone());

        controller
            .runtime_tx
            .send(RuntimeMessage::CurrentSessionRefreshed {
                request_id,
                target,
                purpose: CurrentSessionRefreshPurpose::StopRequestRefresh {
                    root_admission_fence: controller.next_root_run_generation,
                    root_stop_attempt: None,
                    expected_active_turn: Some(ActiveTurnExpectation::initial_idle()),
                    in_memory_stop_accepted: true,
                    durable_outcome: Some(ExactRootExecutionStopOutcome::Applied {
                        cancelled: true,
                    }),
                },
                result: Ok(loaded_test_session(
                    &controller,
                    &root,
                    session_id,
                    SessionStatus::Running,
                    None,
                )),
            })
            .expect("Stop acknowledgement");
        controller.drain_runtime_messages();

        assert!(controller.run_lifecycle.root_is_active());
        assert_eq!(
            controller.state.app_state.run_status,
            crate::tui::state::RunStatus::Running
        );
        assert_eq!(controller.state.app_state.progress.status, "Stopping");
        assert_eq!(
            controller.state.app_state.status_message.as_deref(),
            Some("Stop request accepted; waiting for the matching terminal event")
        );

        let summary = RunSummary::from_terminal(
            session_id,
            crate::protocol::TurnId::new(),
            crate::session::DurableTurnTerminal {
                outcome: crate::protocol::TurnTerminalOutcome::Interrupted {
                    cause: crate::protocol::TurnInterruptionCause::UserStop,
                },
                final_response_id: None,
                tool_call_count: 1,
                failed_tool_count: 0,
                change_count: 0,
                metrics: crate::session::RunMetrics {
                    model_request_count: 2,
                    ..Default::default()
                },
            },
        );
        controller
            .runtime_tx
            .send(RuntimeMessage::Finished {
                run_generation,
                result: Ok(summary),
            })
            .expect("matching interrupted summary");
        controller.drain_runtime_messages();

        assert!(!controller.run_lifecycle.root_is_active());
        assert_eq!(
            controller.state.app_state.run_status,
            crate::tui::state::RunStatus::Cancelled
        );
        assert_eq!(
            controller.state.status_code,
            super::super::state::DesktopStatusCode::UserStopped
        );
    }

    #[tokio::test]
    async fn target_changed_stop_refresh_loads_b_without_marking_b_stopping() {
        let (_temp, root, mut controller) = empty_access_test_controller().await;
        let session_id = SessionId::new();
        let turn_a = TurnId::new();
        let turn_b = TurnId::new();
        controller.state.app_state.current_session_id = Some(session_id);
        controller.state.begin_agent_run();
        controller.state.app_state.run_status = crate::tui::state::RunStatus::Running;
        controller.state.mark_post_run_refresh_pending();
        let target = SessionRefreshRequestTarget {
            workspace_root: root.clone(),
            session_id,
        };
        let request_id = controller
            .session_projection_refresh_requests
            .begin(target.clone());

        controller
            .runtime_tx
            .send(RuntimeMessage::CurrentSessionRefreshed {
                request_id,
                target,
                purpose: CurrentSessionRefreshPurpose::StopRequestRefresh {
                    root_admission_fence: controller.next_root_run_generation,
                    root_stop_attempt: None,
                    expected_active_turn: Some(ActiveTurnExpectation::Turn {
                        turn_id: turn_a,
                        revision: 1,
                    }),
                    in_memory_stop_accepted: true,
                    durable_outcome: Some(ExactRootExecutionStopOutcome::TargetChanged),
                },
                result: Ok(loaded_running_test_session(
                    &controller,
                    &root,
                    session_id,
                    turn_b,
                )),
            })
            .expect("TargetChanged Stop refresh");
        controller.drain_runtime_messages();

        assert_eq!(
            controller.current_active_turn_expectation(),
            ActiveTurnExpectation::Turn {
                turn_id: turn_b,
                revision: 1,
            }
        );
        assert_eq!(
            controller.state.app_state.run_status,
            crate::tui::state::RunStatus::Running
        );
        assert_ne!(controller.state.app_state.progress.status, "Stopping");
        assert_eq!(
            controller.state.app_state.status_message.as_deref(),
            Some("the task changed before Stop was applied; the replacement was not stopped")
        );
    }

    #[tokio::test]
    async fn turn_a_stop_dispatches_after_the_controller_projects_terminal_latest_a() {
        let (_temp, root, mut controller) = empty_access_test_controller().await;
        let session_id = SessionId::new();
        let turn_a = TurnId::new();
        let mut loaded = loaded_test_session(
            &controller,
            &root,
            session_id,
            SessionStatus::Completed,
            None,
        );
        loaded.read.latest_turn_id = Some(turn_a);
        loaded.read.admission_revision = 1;
        controller.state.load_open_session(&loaded.read);
        assert_eq!(
            controller.current_active_turn_expectation(),
            ActiveTurnExpectation::Idle {
                latest_turn_id: Some(turn_a),
                revision: 1,
            }
        );

        controller.cancel_exact_turn_at(turn_a, 1);

        assert!(controller.session_projection_refresh_requests.is_pending());
        assert!(controller.state.post_run_refresh_pending());
        assert_eq!(
            controller.state.app_state.status_message.as_deref(),
            Some("validating the exact task Stop target...")
        );
    }

    #[tokio::test]
    async fn stop_request_refresh_from_before_the_next_root_admission_cannot_mutate_that_root() {
        let (_temp, root, mut controller) = empty_access_test_controller().await;
        let session_id = SessionId::new();
        controller.state.app_state.current_session_id = Some(session_id);
        let target = SessionRefreshRequestTarget {
            workspace_root: root.clone(),
            session_id,
        };
        let request_id = controller
            .session_projection_refresh_requests
            .begin(target.clone());
        let old_root_admission_fence = controller.next_root_run_generation;

        controller.next_root_run_generation = old_root_admission_fence + 1;
        controller
            .run_lifecycle
            .begin(old_root_admission_fence, RunControl::new());
        controller.state.begin_agent_run();
        controller.state.app_state.run_status = crate::tui::state::RunStatus::Running;
        controller.state.composer.draft_prompt = "new root draft".to_string();
        controller.state.begin_prompt_enhance(
            91,
            "new root review source",
            CancellationToken::new(),
        );
        assert!(
            controller
                .state
                .finish_prompt_enhance(91, "new root review".to_string())
        );
        controller.state.mark_post_run_refresh_pending();
        controller
            .state
            .set_status_message("new root terminal refresh pending");

        controller
            .runtime_tx
            .send(RuntimeMessage::CurrentSessionRefreshed {
                request_id,
                target,
                purpose: CurrentSessionRefreshPurpose::StopRequestRefresh {
                    root_admission_fence: old_root_admission_fence,
                    root_stop_attempt: None,
                    expected_active_turn: Some(ActiveTurnExpectation::initial_idle()),
                    in_memory_stop_accepted: true,
                    durable_outcome: Some(ExactRootExecutionStopOutcome::Applied {
                        cancelled: true,
                    }),
                },
                result: Ok(loaded_test_session(
                    &controller,
                    &root,
                    session_id,
                    SessionStatus::Completed,
                    None,
                )),
            })
            .expect("stale Stop settlement");
        controller.drain_runtime_messages();

        assert!(!controller.session_projection_refresh_requests.is_pending());
        assert_eq!(
            controller.run_lifecycle.root_generation(),
            Some(old_root_admission_fence)
        );
        assert_eq!(
            controller.state.app_state.run_status,
            crate::tui::state::RunStatus::Running
        );
        assert_eq!(controller.state.composer.draft_prompt, "new root draft");
        assert_eq!(
            controller
                .state
                .app_state
                .prompt_review
                .as_ref()
                .map(|review| review.current_draft_text.as_str()),
            Some("new root review")
        );
        assert!(controller.state.post_run_refresh_pending());
        assert_eq!(
            controller.state.app_state.status_message.as_deref(),
            Some("new root terminal refresh pending")
        );
    }

    #[tokio::test]
    async fn detached_child_permission_survives_root_finish_and_ordinary_refresh() {
        let (_temp, root, mut controller) = empty_access_test_controller().await;
        let session_id = SessionId::new();
        controller.state.app_state.current_session_id = Some(session_id);
        controller.state.app_state.run_status = crate::tui::state::RunStatus::Completed;
        let request = test_permission("detached-child");
        let expected_path = request.agent_path.clone();
        let expected_task = request.agent_task_name.clone();
        let (response, receiver) = mpsc::channel();
        controller.pending_permission = Some(PendingPermission {
            confirmation_id: 42,
            request,
            responder: response,
            run_control: RunControl::new(),
        });

        controller.settle_pending_permission_after_root_finish();
        controller.apply_current_session_refreshed_message(
            session_id,
            CurrentSessionRefreshPurpose::Refresh,
            Ok(loaded_test_session(
                &controller,
                &root,
                session_id,
                SessionStatus::Completed,
                None,
            )),
        );

        let projection = controller.next_web_state().expect("permission projection");
        assert_eq!(projection.confirmation_id.as_deref(), Some("42"));
        assert!(projection.confirmation_visible);
        let confirmation = projection.confirmation.expect("confirmation requester");
        assert_eq!(confirmation.agent_path, expected_path);
        assert_eq!(confirmation.agent_task_name, expected_task);
        assert_eq!(
            controller.state.app_state.run_status,
            crate::tui::state::RunStatus::Completed
        );

        assert_eq!(
            controller.answer_permission(42, ReviewDecision::Approved),
            PendingPermissionResolution::Resolved
        );
        assert_eq!(receiver.try_recv(), Ok(ReviewDecision::Approved));
    }

    #[tokio::test]
    async fn terminal_child_permission_does_not_survive_root_finish_without_cancel_event() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let request = test_permission("terminal-child");
        let (response, receiver) = mpsc::channel();
        let run_control = RunControl::new();
        assert!(run_control.interrupt(TurnInterruptionCause::TreeStopped));
        controller.pending_permission = Some(PendingPermission {
            confirmation_id: 42,
            request,
            responder: response,
            run_control,
        });

        controller.settle_pending_permission_after_root_finish();

        assert!(controller.pending_permission.is_none());
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        assert!(
            !controller
                .next_web_state()
                .expect("projection")
                .confirmation_visible
        );
    }

    #[tokio::test]
    async fn session_reselection_preserves_durable_interruption_presentation() {
        for cause in [
            TurnInterruptionCause::ApprovalAborted,
            TurnInterruptionCause::UserStop,
        ] {
            let (_temp, root, mut controller) = empty_access_test_controller().await;
            let session_id = SessionId::new();
            let request_id = controller.state.begin_session_load(session_id);
            let loaded = loaded_test_session(
                &controller,
                &root,
                session_id,
                SessionStatus::Cancelled,
                Some(cause),
            );
            let target = SessionLoadRequestTarget {
                workspace_root: controller.app.workspace.root.clone(),
                workspace_cwd: controller.app.workspace.cwd.clone(),
                project_id: controller.app.workspace.project_id,
                session_id,
            };

            controller.apply_session_loaded_message(
                request_id,
                target,
                SessionLoadReason::UserSelection,
                Ok(SessionNavigationLoadResult {
                    workspace: None,
                    loaded,
                }),
            );

            let expected = crate::tui::state::interruption_status_message(cause);
            assert_eq!(
                controller.state.app_state.status_message.as_deref(),
                Some(expected.as_str())
            );
            assert!(
                !controller
                    .state
                    .app_state
                    .status_message
                    .as_deref()
                    .is_some_and(|message| message.starts_with("opened session"))
            );
            let projection = controller.next_web_state().expect("rehydrated projection");
            assert_eq!(projection.status_message, expected);
        }
    }

    #[tokio::test]
    async fn abort_permission_answer_clears_the_modal_and_waits_for_new_instructions() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let request = test_permission("abort this operation");
        let (response, receiver) = mpsc::channel();
        let run_control = RunControl::new();
        let run_control_observer = run_control.clone();
        controller.pending_permission = Some(PendingPermission {
            confirmation_id: 42,
            request: request.clone(),
            responder: response,
            run_control,
        });

        assert_eq!(
            controller.answer_permission(42, ReviewDecision::Abort),
            PendingPermissionResolution::Resolved
        );
        assert_eq!(receiver.try_recv(), Ok(ReviewDecision::Abort));
        assert_eq!(run_control_observer.cause(), None);
        assert!(controller.pending_permission.is_none());
        assert_eq!(
            controller.state.app_state.status_message.as_deref(),
            Some(crate::tui::state::permission_decision_pending_status_message().as_str())
        );
    }

    #[tokio::test]
    async fn abort_permission_answer_preserves_an_existing_terminal_owner() {
        let causes = [
            RunCancellationCause::Interruption(TurnInterruptionCause::UserStop),
            RunCancellationCause::Failure("provider failed first".to_string()),
            RunCancellationCause::Superseded,
        ];

        for cause in causes {
            let (_temp, _root, mut controller) = empty_access_test_controller().await;
            let request = test_permission("late abort");
            let (response, receiver) = mpsc::channel();
            let run_control = RunControl::new();
            assert!(run_control.cancel(cause.clone()));
            controller.pending_permission = Some(PendingPermission {
                confirmation_id: 42,
                request: request.clone(),
                responder: response,
                run_control,
            });

            assert_eq!(
                controller.answer_permission(42, ReviewDecision::Abort),
                PendingPermissionResolution::AlreadyTerminal(cause.clone())
            );
            assert!(controller.pending_permission.is_none());
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::TryRecvError::Disconnected)
            ));
            assert_eq!(
                controller.state.app_state.status_message,
                Some(crate::tui::state::run_cancellation_status_message(&cause))
            );
        }
    }

    #[tokio::test]
    async fn permission_responder_failure_remains_an_operational_failure() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let request = test_permission("disconnected responder");
        let (response, receiver) = mpsc::channel();
        drop(receiver);
        let run_control = RunControl::new();
        controller.pending_permission = Some(PendingPermission {
            confirmation_id: 42,
            request: request.clone(),
            responder: response,
            run_control: run_control.clone(),
        });

        let outcome = controller.answer_permission(42, ReviewDecision::Approved);
        let PendingPermissionResolution::Failed(cause) = outcome else {
            panic!("disconnected responder must be a typed failure");
        };
        assert!(matches!(
            &cause,
            RunCancellationCause::Failure(message)
                if message.contains("desktop permission response failed")
        ));
        assert_eq!(run_control.cause(), Some(cause.clone()));
        assert!(controller.pending_permission.is_none());
        assert_eq!(
            controller.state.app_state.status_message,
            Some(crate::tui::state::run_cancellation_status_message(&cause))
        );
    }

    #[test]
    fn responder_failure_after_a_competing_terminal_claim_consumes_the_ticket() {
        for decision in [ReviewDecision::Approved, ReviewDecision::Abort] {
            let request = test_permission("post-take terminal race");
            let (response, receiver) = mpsc::channel();
            drop(receiver);
            let run_control = RunControl::new();
            let cause = RunCancellationCause::Failure(format!(
                "competing failure before {decision:?} delivery"
            ));
            let mut pending = Some(PendingPermission {
                confirmation_id: 42,
                request,
                responder: response,
                run_control: run_control.clone(),
            });

            let resolution =
                resolve_pending_permission_after_take(&mut pending, 42, decision, |_| {
                    assert!(run_control.cancel(cause.clone()));
                });

            assert_eq!(
                resolution,
                PendingPermissionResolution::AlreadyTerminal(cause.clone())
            );
            assert!(pending.is_none());
            assert_eq!(run_control.cause(), Some(cause));
        }
    }

    #[tokio::test]
    async fn synchronous_access_change_does_not_resolve_an_inflight_permission() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let initial_access_mode = crate::config::AccessMode::Default;
        controller.app.config.permissions.access_mode = initial_access_mode;
        controller
            .state
            .provider_config
            .update_access_mode(initial_access_mode);
        let request = test_permission("closed synchronous permission responder");
        let (response, receiver) = mpsc::channel();
        drop(receiver);
        let run_control = RunControl::new();
        controller.pending_permission = Some(PendingPermission {
            confirmation_id: 42,
            request: request.clone(),
            responder: response,
            run_control: run_control.clone(),
        });

        assert!(controller.toggle_access_mode_with_persistence(
            |expected, access_mode| {
                assert_eq!(expected, initial_access_mode);
                assert_eq!(access_mode, crate::config::AccessMode::AutoReview);
                Ok(Some(Utf8PathBuf::from("C:/config.toml")))
            },
            |_, _| Ok(()),
        ));

        assert_eq!(run_control.cause(), None);
        assert!(controller.pending_permission.is_some());
        let status = controller
            .state
            .app_state
            .status_message
            .as_deref()
            .expect("failure status");
        assert!(status.contains("next permission decision"));
        assert!(status.contains("already displayed confirmation is unchanged"));
    }

    #[tokio::test]
    async fn asynchronous_access_change_does_not_resolve_an_inflight_permission() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let initial_access_mode = crate::config::AccessMode::Default;
        controller.app.config.permissions.access_mode = initial_access_mode;
        controller
            .state
            .provider_config
            .update_access_mode(initial_access_mode);
        let request = test_permission("closed asynchronous permission responder");
        let (response, receiver) = mpsc::channel();
        drop(receiver);
        let run_control = RunControl::new();
        controller.pending_permission = Some(PendingPermission {
            confirmation_id: 42,
            request: request.clone(),
            responder: response,
            run_control: run_control.clone(),
        });

        assert!(controller.start_access_mode_persistence(
            move |expected, access_mode| {
                assert_eq!(expected, initial_access_mode);
                assert_eq!(access_mode, crate::config::AccessMode::AutoReview);
                Ok(Some(Utf8PathBuf::from("C:/config.toml")))
            },
            |_, _| Ok(None),
        ));
        for _ in 0..200 {
            controller.drain_runtime_messages();
            if !controller.state.background_mutation_pending() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        assert!(!controller.state.background_mutation_pending());
        assert_eq!(run_control.cause(), None);
        assert!(controller.pending_permission.is_some());
        let status = controller
            .state
            .app_state
            .status_message
            .as_deref()
            .expect("failure status");
        assert!(status.contains("next permission decision"));
        assert!(status.contains("already displayed confirmation is unchanged"));
    }

    #[tokio::test]
    async fn stop_routes_through_root_owner_and_waits_for_permission_cancellation_event() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let root_control = RunControl::new();
        controller.run_lifecycle.begin(1, root_control.clone());
        let request = test_permission("child stop routing");
        let (response, receiver) = mpsc::channel();
        let child_control = RunControl::new();
        controller.pending_permission = Some(PendingPermission {
            confirmation_id: 42,
            request: request.clone(),
            responder: response,
            run_control: child_control.clone(),
        });

        assert!(controller.cancel_root_run_at_generation(1));

        assert_eq!(
            root_control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            ))
        );
        assert_eq!(
            child_control.cause(),
            None,
            "the Desktop Stop surface must not classify a pending child directly"
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(controller.pending_permission.is_some());

        assert!(child_control.interrupt(TurnInterruptionCause::TreeStopped));
        controller
            .runtime_tx
            .send(RuntimeMessage::PermissionCancelled {
                confirmation_id: 42,
            })
            .expect("permission cancellation event");
        controller.drain_runtime_messages();
        assert!(controller.pending_permission.is_none());
    }

    #[tokio::test]
    async fn desktop_main_stop_targets_only_the_current_root_and_preserves_child_owners() {
        let (_temp, root, mut controller) = empty_access_test_controller().await;
        let session = controller
            .app
            .session_service
            .start_or_resume(
                crate::session::SessionStartRequest {
                    selector: crate::session::SessionSelector::New,
                    title: Some("desktop tree Stop".to_string()),
                    cwd: root,
                    model: controller.app.config.model.model.clone(),
                    base_url: controller.app.config.model.base_url.clone(),
                    access_mode: controller.app.config.permissions.access_mode,
                    provider_connection: Some(
                        crate::session::SessionProviderConnection::from_model_config(
                            &controller.app.config.model,
                        ),
                    ),
                },
                controller.app.workspace.clone(),
            )
            .await
            .expect("root session");
        let root_scope = RunControl::new();
        let runtime = controller.app.process_runtime.agent_runtime();
        let root_execution = runtime
            .begin_root(
                &session,
                Arc::new(
                    crate::config::ResolvedTurnConfig::capture(controller.app.config.clone())
                        .expect("valid test turn config"),
                ),
                SharedConfirmationPrompt::new(DesktopConfirmationPrompt {
                    control: controller.control_tx.clone(),
                    next_permission_request_id: controller.next_permission_request_id.clone(),
                }),
                root_scope.clone(),
            )
            .await
            .expect("root execution");
        let root_turn_id = TurnId::new();
        let root_admission_guard = root_scope
            .begin_root_admission(session.session.id, root_turn_id)
            .expect("publish root admission plan");
        let root_admission = controller
            .app
            .store
            .session_repo()
            .admit_session_turn(session.session.id, root_turn_id)
            .await
            .expect("root admission")
            .expect("root admitted");
        root_admission_guard
            .commit(root_admission.admission_revision)
            .expect("publish canonical root admission receipt");
        root_execution
            .context
            .bind_durable_turn_owner(
                root_admission.admission_id,
                root_turn_id,
                root_admission.admission_revision,
            )
            .expect("bind root owner");
        let tree_control = root_execution.context.tree_control_for_test();
        let (_, child_a) = tree_control
            .register_child(
                &crate::runtime::AgentPath::root(),
                "child_a",
                SessionId::new(),
                Some("already interrupted".to_string()),
            )
            .expect("child A");
        let (_, child_b) = tree_control
            .register_child(
                &crate::runtime::AgentPath::root(),
                "child_b",
                SessionId::new(),
                Some("held sibling".to_string()),
            )
            .expect("child B");
        let child_a_control = child_a.run_control();
        let child_b_control = child_b.run_control();
        tree_control
            .cancel_agent(&crate::runtime::AgentPath::try_from("/root/child_a").expect("path"))
            .expect("exact child A interrupt");

        controller.state.app_state.current_session_id = Some(session.session.id);
        controller.run_lifecycle.begin(44, root_scope.clone());
        controller.state.begin_agent_run();
        assert!(controller.cancel_root_run_at_generation(44));

        for _ in 0..200 {
            controller.drain_runtime_messages();
            if !controller.session_projection_refresh_requests.is_pending() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            !controller.session_projection_refresh_requests.is_pending(),
            "durable root Stop refresh must settle"
        );
        assert!(
            !tree_control.tree_is_cancelled(),
            "ordinary root Stop must not acquire the explicit tree-wide cancellation owner"
        );
        assert_eq!(
            root_execution.run_control().cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop,
            ))
        );
        assert_eq!(
            child_a_control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::AgentInterrupted,
            )),
            "Main Stop must not overwrite the prior exact child terminal owner"
        );
        assert_eq!(
            child_b_control.cause(),
            None,
            "ordinary root Stop must not classify an unaffected sibling"
        );
        let repository = controller.app.store.session_repo();
        assert!(
            repository
                .durable_terminal_for_turn(session.session.id, root_turn_id)
                .await
                .expect("pre-settlement root terminal lookup")
                .is_none(),
            "Stop records intent; the admitted owner remains the terminal writer"
        );
        let renewal = repository
            .renew_admitted_run_lease(
                session.session.id,
                root_admission.admission_id,
                root_turn_id,
            )
            .await
            .expect("root owner observes Stop intent");
        assert!(
            matches!(
                renewal,
                crate::storage::session_repo::RunAdmissionLeaseRenewalOutcome::InterruptRequested(
                    TurnInterruptionCause::UserStop
                )
            ),
            "unexpected root lease renewal after Stop: {renewal:?}"
        );
        let settlement = repository
            .settle_admitted_turn_with_protocol_event(
                session.session.id,
                root_admission.admission_id,
                &RunEvent::TurnTerminal {
                    session_id: session.session.id,
                    terminal: Box::new(crate::session::DurableTurnTerminal {
                        outcome: crate::protocol::TurnTerminalOutcome::Completed,
                        final_response_id: None,
                        tool_call_count: 0,
                        failed_tool_count: 0,
                        change_count: 0,
                        metrics: Default::default(),
                    }),
                },
                root_turn_id,
                None,
                None,
            )
            .await
            .expect("admitted root terminal settlement");
        assert_eq!(
            settlement.commit(),
            crate::storage::session_repo::AdmittedTerminalCommit::Applied
        );
        let settled_terminal = settlement
            .into_terminal()
            .expect("authoritative root terminal receipt");
        assert_eq!(
            settled_terminal.outcome,
            crate::protocol::TurnTerminalOutcome::Interrupted {
                cause: TurnInterruptionCause::UserStop,
            },
            "the committed Stop request overrides a later success proposal"
        );

        tree_control
            .complete_execution(
                child_a,
                crate::runtime::InactiveAgentStatus::Interrupted,
                None,
            )
            .expect("settle child A");
        tree_control
            .complete_execution(
                child_b,
                crate::runtime::InactiveAgentStatus::Completed(None),
                None,
            )
            .expect("settle child B");
        runtime.complete_root(
            root_execution,
            &Ok(RunSummary::from_terminal(
                session.session.id,
                root_turn_id,
                settled_terminal,
            )),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop,
            )),
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            runtime.wait_for_tree_quiescence(session.session.id),
        )
        .await
        .expect("tree quiescence timeout")
        .expect("tree quiescence");
    }

    #[tokio::test]
    async fn permission_without_an_exact_execution_owner_has_no_stop_capability() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let request = test_permission("detached child approval");
        let (response, receiver) = mpsc::channel();
        let run_control = RunControl::new();
        let observer = run_control.clone();
        controller.pending_permission = Some(PendingPermission {
            confirmation_id: 42,
            request,
            responder: response,
            run_control,
        });

        let projection = controller.next_web_state().expect("permission projection");
        assert!(!projection.can_cancel_run);
        assert!(projection.stop_target.is_none());
        assert!(projection.confirmation_visible);
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert_eq!(
            observer.cause(),
            None,
            "permission presence cannot mint an implicit UserStop owner"
        );
        assert!(controller.pending_permission.is_some());
    }

    #[tokio::test]
    async fn terminal_permission_projection_preserves_the_existing_first_writer() {
        for cause in [
            RunCancellationCause::Interruption(TurnInterruptionCause::ApprovalAborted),
            RunCancellationCause::Failure("provider failed first".to_string()),
        ] {
            let (_temp, _root, mut controller) = empty_access_test_controller().await;
            let (response, receiver) = mpsc::channel();
            let run_control = RunControl::new();
            assert!(run_control.cancel(cause.clone()));
            controller.pending_permission = Some(PendingPermission {
                confirmation_id: 42,
                request: test_permission("late Stop"),
                responder: response,
                run_control: run_control.clone(),
            });

            let projection = controller.next_web_state().expect("terminal projection");
            assert_eq!(run_control.cause(), Some(cause.clone()));
            assert!(!projection.can_cancel_run);
            assert!(projection.stop_target.is_none());
            assert!(!projection.confirmation_visible);
            assert!(matches!(
                receiver.try_recv(),
                Err(mpsc::TryRecvError::Disconnected)
            ));
        }
    }

    #[tokio::test]
    async fn ordinary_stop_does_not_target_a_detached_child_after_root_completion() {
        use crate::session::{NewSession, SessionRepository as _};

        let (_temp, workspace_root, mut controller) = empty_access_test_controller().await;
        let repository = controller.app.store.session_repo();
        let new_session = |title: &str| NewSession {
            project_id: controller.app.workspace.project_id,
            title: title.to_string(),
            cwd: workspace_root.clone(),
            model: controller.app.config.model.model.clone(),
            base_url: controller.app.config.model.base_url.clone(),
            access_mode: crate::config::AccessMode::Default,
            provider_connection: None,
        };
        let root = repository
            .create_session(new_session("durable root"))
            .await
            .expect("root session");
        let child = repository
            .create_session(new_session("durable child"))
            .await
            .expect("child session");
        repository
            .insert_session_spawn_edge(
                root.id,
                root.id,
                child.id,
                "/root/durable_child",
                "durable_child",
            )
            .await
            .expect("spawn edge");
        let root_turn = crate::protocol::TurnId::new();
        repository
            .admit_session_turn(root.id, root_turn)
            .await
            .expect("root admission")
            .expect("root admitted");
        controller
            .app
            .store
            .protocol_event_store()
            .seed_history_item_for_test(&crate::protocol::HistoryItem {
                id: crate::protocol::HistoryItemId::new(),
                session_id: root.id,
                scope: crate::protocol::HistoryScope::Turn { turn_id: root_turn },
                sequence_no: 1,
                created_at_ms: 1,
                payload: crate::protocol::HistoryItemPayload::UserTurn {
                    content: vec![crate::protocol::ContentPart::Text {
                        text: "run detached work".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                },
            })
            .expect("root history");
        let root_target = repository
            .captured_running_terminal_target(root.id)
            .await
            .expect("capture root target")
            .expect("root running target");
        assert!(
            repository
                .terminalize_captured_running_session_with_protocol_event(
                    root.id,
                    &RunEvent::TurnTerminal {
                        session_id: root.id,
                        terminal: Box::new(crate::session::DurableTurnTerminal {
                            outcome: crate::protocol::TurnTerminalOutcome::Completed,
                            final_response_id: None,
                            tool_call_count: 0,
                            failed_tool_count: 0,
                            change_count: 0,
                            metrics: Default::default(),
                        }),
                    },
                    root_target,
                )
                .await
                .expect("complete root")
        );
        repository
            .admit_session_turn(child.id, crate::protocol::TurnId::new())
            .await
            .expect("child admission")
            .expect("child admitted");
        controller.state.app_state.current_session_id = Some(root.id);
        controller.state.app_state.run_status = crate::tui::state::RunStatus::Completed;
        controller.loaded_agent_activity_records = Some((
            root.id,
            vec![agent_record(
                child.id,
                "/root/durable_child",
                AgentStatus::Running,
                "",
            )],
        ));
        assert!(controller.current_agent_tree_active());
        assert!(!controller.run_lifecycle.root_is_active());

        let projection = controller.next_web_state().expect("child-only projection");

        assert!(!controller.session_projection_refresh_requests.is_pending());
        assert!(!projection.can_cancel_run);
        assert!(projection.stop_target.is_none());
        assert_eq!(
            repository
                .get_session(root.id)
                .await
                .expect("preserved root")
                .status,
            SessionStatus::Completed
        );
        assert_eq!(
            repository
                .get_session(child.id)
                .await
                .expect("detached child")
                .status,
            SessionStatus::Running
        );
        controller.loaded_agent_activity_records = Some((
            root.id,
            vec![agent_record(
                child.id,
                "/root/stale-child",
                AgentStatus::Running,
                "",
            )],
        ));
        let stale_projection = controller
            .next_web_state()
            .expect("stale child-only projection");
        assert_eq!(
            controller.state.app_state.run_status,
            crate::tui::state::RunStatus::Completed,
            "a child-only Stop must not reclassify the preserved root"
        );
        assert!(!stale_projection.can_cancel_run);
        assert!(stale_projection.stop_target.is_none());
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
        controller.run_lifecycle.begin(1, RunControl::new());
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
                    Ok(None)
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
    }

    #[tokio::test]
    async fn delayed_adopted_access_completion_is_discarded_after_next_root_generation_starts() {
        use crate::session::{NewSession, SessionRepository as _};

        let (_temp, root, mut controller) = empty_access_test_controller().await;
        let reloaded_access_mode = controller.app.config.permissions.access_mode;
        let initial_access_mode = [
            crate::config::AccessMode::Default,
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
                provider_connection: None,
            })
            .await
            .expect("session");
        let session_id = session.id;
        let session_title = session.title.clone();
        controller.run_lifecycle.begin(1, RunControl::new());
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
                Ok(None)
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

        controller.run_lifecycle.begin(2, RunControl::new());
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
    }

    #[tokio::test]
    async fn desktop_reopen_uses_durable_session_access_for_run_config_and_new_chat_uses_global() {
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
        let mut app = build_test_app(&root, store).await;
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
                provider_connection: None,
            })
            .await
            .expect("session");
        let turn_id = crate::protocol::TurnId::new();
        app.store
            .protocol_event_store()
            .seed_history_item_for_test(&crate::protocol::HistoryItem {
                id: crate::protocol::HistoryItemId::new(),
                session_id: session.id,
                scope: crate::protocol::HistoryScope::Turn { turn_id },
                sequence_no: 1,
                created_at_ms: 1,
                payload: crate::protocol::HistoryItemPayload::UserTurn {
                    content: vec![crate::protocol::ContentPart::Text {
                        text: "reopen".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
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
        let run_config = controller.state.provider_config.effective_config.clone();
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
        let mut app = build_test_app(&root, store).await;
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
                provider_connection: None,
            })
            .await
            .expect("session");
        app.store
            .protocol_event_store()
            .seed_history_item_for_test(&crate::protocol::HistoryItem {
                id: crate::protocol::HistoryItemId::new(),
                session_id: session.id,
                scope: crate::protocol::HistoryScope::Turn {
                    turn_id: crate::protocol::TurnId::new(),
                },
                sequence_no: 1,
                created_at_ms: 1,
                payload: crate::protocol::HistoryItemPayload::UserTurn {
                    content: vec![crate::protocol::ContentPart::Text {
                        text: "archive this session".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
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
        controller.run_lifecycle.begin(41, RunControl::new());
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
        let run_config = controller.state.provider_config.effective_config.clone();
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
        let app = build_test_app(&root, store).await;
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
        let cancel = RunControl::new();
        let cancel_observer = cancel.clone();
        controller.run_lifecycle.begin(1, cancel);
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);

        assert!(controller.start_access_mode_persistence(
            move |_, _| {
                started_tx.send(()).expect("signal blocked persistence");
                release_rx.recv().expect("release blocked persistence");
                Err("simulated blocked global writer".to_string())
            },
            |_, _| Ok(None),
        ));
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("persistence worker started");

        assert!(controller.cancel_root_run_at_generation(1));
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
        let app = build_test_app(&root, store).await;
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
            |_, _| Ok(None),
        ));
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("persistence worker started");
        assert!(controller.state.background_mutation_pending());

        let initial_generation = controller.next_root_run_generation;
        controller.state.composer.draft_prompt = "submit after access settles".to_string();
        assert!(!controller.start_run("submit after access settles".to_string()));
        assert!(!controller.start_review_uncommitted("review after access settles".to_string()));
        controller.state.begin_prompt_enhance(
            11,
            "enhance before review",
            CancellationToken::new(),
        );
        assert!(
            controller
                .state
                .finish_prompt_enhance(11, "enhanced review draft".to_string())
        );
        assert!(!controller.send_prompt_review(11, true, "edited review draft".to_string()));
        assert_eq!(
            controller.state.composer.draft_prompt,
            "submit after access settles"
        );
        assert_eq!(
            controller
                .state
                .app_state
                .prompt_review
                .as_ref()
                .map(|review| review.current_draft_text.as_str()),
            Some("edited review draft")
        );
        assert_eq!(controller.next_root_run_generation, initial_generation);
        assert!(!controller.run_lifecycle.root_is_active());
        assert!(
            controller.state.cancel_prompt_review_if_current(11),
            "the failed exact review send retains its draft until the user cancels that owner"
        );

        controller.run_lifecycle.begin(77, RunControl::new());
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

    #[test]
    fn accepted_steer_does_not_append_a_phantom_transcript_row() {
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
        let image = Utf8PathBuf::from("C:/workspace/reference.png");
        state.composer.image_attachment_paths.push(image.clone());

        assert!(finish_steer_submission(
            &mut state,
            std::slice::from_ref(&image),
            Ok(()),
        ));
        assert!(state.app_state.transcript_entries.is_empty());
        assert!(state.composer.image_attachment_paths.is_empty());
        assert!(
            state
                .app_state
                .status_message
                .as_deref()
                .is_some_and(|message| message.contains("送信待ちキュー"))
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
            is_current_turn: false,
            active_turn_id: None,
            interrupt_target: None,
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

    fn recv_runtime_message(receiver: &mpsc::Receiver<RuntimeMessage>) -> RuntimeMessage {
        for _ in 0..200 {
            match receiver.try_recv() {
                Ok(message) => return message,
                Err(mpsc::TryRecvError::Empty) => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("desktop runtime channel disconnected")
                }
            }
        }
        panic!("timed out waiting for desktop runtime message")
    }

    #[test]
    fn child_only_agent_activity_does_not_block_desktop_navigation() {
        assert_eq!(
            navigation_admission_blocker(false, false, false, false),
            None
        );
        assert_eq!(
            navigation_admission_blocker(false, false, false, true),
            Some("the current run is finalizing")
        );
    }

    #[tokio::test]
    async fn child_only_agent_activity_keeps_in_flight_session_search_owned() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let root_session_id = SessionId::new();
        controller.state.app_state.current_session_id = Some(root_session_id);
        controller.loaded_agent_activity_records = Some((
            root_session_id,
            vec![agent_record(
                SessionId::new(),
                "/root/detached_search_worker",
                AgentStatus::Running,
                "",
            )],
        ));
        let operation_id = controller.state.begin_session_search();
        let request_id = controller
            .session_search_requests
            .begin(
                operation_id,
                SessionSearchRequestTarget {
                    query: "retained query".to_string(),
                    include_archived: false,
                    selected_session_id: Some(root_session_id),
                },
            )
            .dispatch
            .expect("initial search dispatch")
            .request_id;

        let projection = controller.next_web_state().expect("child-only projection");

        assert!(projection.agent_tree_active);
        assert!(projection.navigation_admission_open);
        assert!(projection.can_submit);
        assert!(
            controller
                .state
                .pending_async_operation_keys()
                .iter()
                .any(|key| key == "session_search"),
            "descendant liveness must not cancel the root-owned search"
        );
        assert!(
            controller
                .session_search_requests
                .finish(request_id)
                .is_some(),
            "the accepted search request must retain its completion owner"
        );
    }

    #[test]
    fn session_search_started_while_idle_cannot_replace_root_after_root_admission() {
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
            Ok(snapshot(stale_search_root, "stale search result")),
        ));
        assert_eq!(state.selected_session_id(), Some(selected_root));

        assert!(apply_session_search_result(
            &mut state,
            true,
            false,
            Ok(snapshot(stale_search_root, "child-safe search result")),
        ));
        assert_eq!(state.selected_session_id(), Some(stale_search_root));
        assert!(!session_search_result_can_apply(false, false));
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
        let app = build_test_app(&root, store).await;
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
            provider_connection: None,
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
        let search_request = controller
            .session_search_requests
            .begin(
                search_operation,
                SessionSearchRequestTarget {
                    query: "session".to_string(),
                    include_archived: false,
                    selected_session_id: Some(session_a.id),
                },
            )
            .dispatch
            .expect("first search dispatch")
            .request_id;
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
        controller
            .state
            .begin_prompt_enhance(77, "raw review", CancellationToken::new());
        assert!(
            controller
                .state
                .finish_prompt_enhance(77, "edited review".to_string())
        );
        let review_target = controller
            .prompt_review_target(77)
            .expect("review target before run admission");
        let failed_generation = 900;
        controller
            .run_lifecycle
            .begin(failed_generation, RunControl::new());
        controller.state.begin_agent_run();
        controller.pending_root_submission = Some(PendingRootSubmission {
            run_generation: failed_generation,
            owner_workspace_path: root.clone(),
            owner_session_id: Some(session_a.id),
            prompt_dispatch: crate::session::PromptDispatchPart::raw("retain on preflight error"),
            image_paths: vec![retained_image.clone()],
            prompt_review_to_cancel: Some(review_target.clone()),
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
        assert_eq!(
            controller
                .state
                .app_state
                .prompt_review
                .as_ref()
                .map(|review| review.current_draft_text.as_str()),
            Some("edited review")
        );
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
        assert_eq!(
            controller
                .state
                .app_state
                .prompt_review
                .as_ref()
                .map(|review| review.current_draft_text.as_str()),
            Some("edited review")
        );
        assert_eq!(
            controller.state.view.overlay,
            super::super::state::DesktopOverlay::PromptReview
        );

        let admitted_generation = failed_generation + 1;
        controller
            .run_lifecycle
            .begin(admitted_generation, RunControl::new());
        controller.state.begin_agent_run();
        controller.pending_root_submission = Some(PendingRootSubmission {
            run_generation: admitted_generation,
            owner_workspace_path: root.clone(),
            owner_session_id: Some(session_a.id),
            prompt_dispatch: crate::session::PromptDispatchPart::raw("commit after admission"),
            image_paths: vec![retained_image.clone()],
            prompt_review_to_cancel: Some(review_target),
        });
        let delayed_refresh_target = SessionRefreshRequestTarget {
            workspace_root: root.clone(),
            session_id: session_a.id,
        };
        let delayed_refresh_request = controller
            .session_projection_refresh_requests
            .begin(delayed_refresh_target.clone());
        let delayed_refresh = loaded_session_from_detail(
            load_latest_session_detail(&controller.app, session_a.id)
                .await
                .expect("delayed terminal refresh"),
            None,
        );
        controller
            .runtime_tx
            .send(RuntimeMessage::CurrentSessionRefreshed {
                request_id: delayed_refresh_request,
                target: delayed_refresh_target,
                purpose: CurrentSessionRefreshPurpose::Refresh,
                result: Ok(delayed_refresh),
            })
            .expect("terminal refresh queued before the next session event");
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
                event: RunEvent::UserTurnStored {
                    session_id: session_a.id,
                    turn: Box::new(crate::protocol::UserTurn {
                        turn_id: crate::protocol::TurnId::new(),
                        items: vec![crate::protocol::UserInputItem::Text {
                            text: "durable prompt".to_string(),
                        }],
                        prompt_dispatch: None,
                        editor_context: None,
                    }),
                },
            })
            .expect("durable user message");
        assert!(controller.cancel_root_run_at_generation(admitted_generation));
        controller.drain_runtime_messages();
        assert_eq!(controller.composer_commit_generation, 1);
        assert!(controller.state.composer.image_attachment_paths.is_empty());
        assert!(controller.pending_root_submission.is_none());
        assert!(controller.state.app_state.prompt_review.is_none());
        assert!(
            desktop_web_state(&controller.state, &DesktopRuntimeProjection::default())
                .review_draft_text
                .is_empty()
        );
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
            prompt_review_to_cancel: None,
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
        let app = build_test_app(&root, store).await;
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
        assert!(controller.reload_config());
        let initial_access_mode = controller.app.config.permissions.access_mode;
        controller.run_lifecycle.begin(1, RunControl::new());
        let request = test_permission("pending permission");
        let (response, receiver) = mpsc::channel();
        controller.pending_permission = Some(PendingPermission {
            confirmation_id: 42,
            request: request.clone(),
            responder: response,
            run_control: RunControl::new(),
        });

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
        assert_eq!(
            controller
                .pending_permission
                .as_ref()
                .map(|pending| pending.confirmation_id),
            Some(42)
        );
        assert_eq!(
            controller
                .pending_permission
                .as_ref()
                .map(|pending| pending.request.summary.as_str()),
            Some(request.summary.as_str())
        );
        let projection = controller.next_web_state().expect("permission projection");
        assert_eq!(projection.confirmation_id.as_deref(), Some("42"));
        assert!(projection.confirmation_visible);
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
        assert!(agent_activity_projection("C:/workspace", root_session_id, selected).1);
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
        let (control, runtime_rx) = test_desktop_control_plane();
        let broker = SharedConfirmationPrompt::new(DesktopConfirmationPrompt {
            control,
            next_permission_request_id: Arc::new(AtomicU64::new(41)),
        });

        let first_cancel = RunControl::new();
        let (first_done_tx, first_done_rx) = mpsc::sync_channel(1);
        let mut first_prompt = broker.clone();
        let first_wait_cancel = first_cancel.clone();
        std::thread::spawn(move || {
            let result =
                first_prompt.confirm_with_control(&test_permission("first"), &first_wait_cancel);
            let _ = first_done_tx.send(result);
        });

        let (first_id, first_response) = match recv_runtime_message(&runtime_rx) {
            RuntimeMessage::Permission {
                confirmation_id,
                response,
                ..
            } => (confirmation_id, response),
            _ => panic!("expected first desktop permission"),
        };
        first_cancel.interrupt(TurnInterruptionCause::UserStop);
        match recv_runtime_message(&runtime_rx) {
            RuntimeMessage::PermissionCancelled { confirmation_id } => {
                assert_eq!(confirmation_id, first_id)
            }
            _ => panic!("expected desktop permission cancellation"),
        }
        assert!(matches!(
            first_done_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("first confirmation result")
                .expect("first confirmation"),
            ConfirmationOutcome::Interrupted
        ));

        let mut pending = Some(PendingPermission {
            confirmation_id: first_id,
            request: test_permission("first"),
            responder: first_response,
            run_control: first_cancel,
        });
        assert!(!clear_cancelled_permission(&mut pending, first_id + 1));
        assert_eq!(
            pending.as_ref().map(|pending| pending.confirmation_id),
            Some(first_id)
        );
        assert!(clear_cancelled_permission(&mut pending, first_id));
        assert!(pending.is_none());

        let (second_done_tx, second_done_rx) = mpsc::sync_channel(1);
        let mut second_prompt = broker;
        std::thread::spawn(move || {
            let result = second_prompt.confirm(&test_permission("second"));
            let _ = second_done_tx.send(result);
        });
        let (second_id, second_response) = match recv_runtime_message(&runtime_rx) {
            RuntimeMessage::Permission {
                confirmation_id,
                response,
                ..
            } => (confirmation_id, response),
            _ => panic!("expected second desktop permission"),
        };
        assert!(second_id > first_id);
        second_response
            .send(ReviewDecision::Approved)
            .expect("answer second permission");
        assert_eq!(
            second_done_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("second confirmation result")
                .expect("second confirmation"),
            ReviewDecision::Approved
        );
    }

    #[test]
    fn disconnected_control_plane_rejects_bootstrap_and_permission() {
        let (runtime_tx, _runtime_rx) = mpsc::sync_channel(1);
        let (control_tx, control_rx) = mpsc::channel();
        drop(control_rx);
        let disconnected = DesktopControlPlaneSender { tx: control_tx };
        let mut renderer = DesktopRenderer {
            runtime_tx,
            bootstrap_control: disconnected.clone(),
            run_generation: 42,
            notification_title: "test".to_string(),
            notified_terminal: false,
        };
        let bootstrap_error = renderer
            .render(&RunEvent::SessionStarted {
                session_id: SessionId::new(),
                title: "unavailable".to_string(),
            })
            .expect_err("disconnected bootstrap must fail");
        assert!(
            bootstrap_error
                .to_string()
                .contains("control mailbox is unavailable")
        );

        let mut prompt = DesktopConfirmationPrompt {
            control: disconnected,
            next_permission_request_id: Arc::new(AtomicU64::new(1)),
        };
        let permission_error = prompt
            .confirm(&test_permission("disconnected"))
            .expect_err("disconnected permission must fail");
        assert!(
            permission_error
                .to_string()
                .contains("control mailbox is unavailable")
        );
    }

    #[test]
    fn desktop_permission_abort_is_ticket_local_and_loses_to_existing_cause() {
        let (control, runtime_rx) = test_desktop_control_plane();
        let mut prompt = DesktopConfirmationPrompt {
            control,
            next_permission_request_id: Arc::new(AtomicU64::new(61)),
        };
        let control = RunControl::new();
        let observer = control.clone();
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = prompt.confirm_with_control(&test_permission("abort"), &control);
            let _ = done_tx.send(result);
        });
        let response = match recv_runtime_message(&runtime_rx) {
            RuntimeMessage::Permission { response, .. } => response,
            _ => panic!("expected Desktop permission"),
        };
        response
            .send(ReviewDecision::Abort)
            .expect("send ticket-local abort");
        assert_eq!(
            done_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("abort result")
                .expect("abort outcome"),
            ConfirmationOutcome::AbortRequested
        );
        assert_eq!(observer.cause(), None);

        let (control, runtime_rx) = test_desktop_control_plane();
        let mut prompt = DesktopConfirmationPrompt {
            control,
            next_permission_request_id: Arc::new(AtomicU64::new(71)),
        };
        let control = RunControl::new();
        let observer = control.clone();
        let worker_control = control.clone();
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result =
                prompt.confirm_with_control(&test_permission("competing abort"), &worker_control);
            let _ = done_tx.send(result);
        });
        let response = match recv_runtime_message(&runtime_rx) {
            RuntimeMessage::Permission { response, .. } => response,
            _ => panic!("expected competing Desktop permission"),
        };
        assert!(control.fail("provider failed first"));
        response
            .send(ReviewDecision::Abort)
            .expect("send losing abort");
        assert_eq!(
            done_rx
                .recv_timeout(std::time::Duration::from_secs(1))
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
            workspace_authority_root: Utf8PathBuf::from("C:/repo/bbb"),
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
                Utf8Path::new("C:/repo/bbb"),
            ),
            None,
            "an older completion cannot settle the latest export owner"
        );
        assert_eq!(
            finish_history_export_request(
                &mut tracker,
                current_request,
                &target,
                Utf8Path::new("C:/repo/ccc"),
            ),
            Some(false),
            "a current request from a replaced sibling authority cannot update status"
        );
        assert_eq!(
            finish_history_export_request(
                &mut tracker,
                current_request,
                &target,
                Utf8Path::new("C:/repo/bbb"),
            ),
            None,
            "the same completion is consumed at most once"
        );
    }

    #[test]
    fn renderer_leaves_an_ordinary_committed_assistant_to_the_canonical_cursor() {
        let (runtime_tx, runtime_rx) = mpsc::sync_channel(1);
        let (control, control_rx) = test_desktop_control_plane();
        let mut renderer = DesktopRenderer {
            runtime_tx,
            bootstrap_control: control,
            run_generation: 77,
            notification_title: "test".to_string(),
            notified_terminal: false,
        };

        renderer
            .render(&RunEvent::AssistantMessageCommitted {
                response_id: crate::protocol::ModelResponseId::new(),
                text: "canonical only".to_string(),
            })
            .expect("committed assistant renderer observation");

        assert!(matches!(
            runtime_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(matches!(
            control_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn renderer_allows_runtime_only_delta_loss_when_the_mailbox_is_full() {
        let run_generation = 78;
        let response_id = crate::protocol::ModelResponseId::new();
        let (runtime_tx, runtime_rx) = mpsc::sync_channel(1);
        runtime_tx
            .try_send(RuntimeMessage::RunEvent {
                run_generation,
                event: RunEvent::TextDelta {
                    response_id,
                    delta: "retained".to_string(),
                },
            })
            .expect("fill runtime mailbox");
        let (control, control_rx) = test_desktop_control_plane();
        let mut renderer = DesktopRenderer {
            runtime_tx,
            bootstrap_control: control,
            run_generation,
            notification_title: "test".to_string(),
            notified_terminal: false,
        };

        renderer
            .render(&RunEvent::TextDelta {
                response_id,
                delta: "dropped".to_string(),
            })
            .expect("runtime-only saturation is lossy by contract");

        assert!(matches!(
            control_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(matches!(
            runtime_rx.recv().expect("retained delta"),
            RuntimeMessage::RunEvent {
                event: RunEvent::TextDelta { delta, .. },
                ..
            } if delta == "retained"
        ));
        assert!(matches!(
            runtime_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn bootstrap_and_worker_settlement_bypass_a_full_runtime_mailbox_in_fifo_order() {
        let run_generation = 79;
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let response_id = crate::protocol::ModelResponseId::new();
        let summary = RunSummary::from_terminal(
            session_id,
            turn_id,
            crate::session::DurableTurnTerminal {
                outcome: crate::protocol::TurnTerminalOutcome::Completed,
                final_response_id: Some(response_id),
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            },
        );
        let (runtime_tx, runtime_rx) = mpsc::sync_channel(1);
        runtime_tx
            .try_send(RuntimeMessage::RunEvent {
                run_generation,
                event: RunEvent::TextDelta {
                    response_id,
                    delta: "mailbox backlog".to_string(),
                },
            })
            .expect("fill runtime mailbox");
        let (control, control_rx) = test_desktop_control_plane();
        let mut renderer = DesktopRenderer {
            runtime_tx,
            bootstrap_control: control.clone(),
            run_generation,
            notification_title: "test".to_string(),
            notified_terminal: false,
        };

        renderer
            .render(&RunEvent::SessionStarted {
                session_id,
                title: "new session".to_string(),
            })
            .expect("lossless session bootstrap");
        renderer
            .render(&RunEvent::UserTurnStored {
                session_id,
                turn: Box::new(UserTurn {
                    turn_id,
                    items: vec![UserInputItem::Text {
                        text: "durable prompt".to_string(),
                    }],
                    prompt_dispatch: None,
                    editor_context: None,
                }),
            })
            .expect("lossless user-turn bootstrap");
        publish_desktop_run_finished(&control, run_generation, Ok(summary));

        assert!(matches!(
            control_rx.recv().expect("session bootstrap"),
            RuntimeMessage::RunEvent {
                run_generation: 79,
                event: RunEvent::SessionStarted { session_id: received, .. },
            } if received == session_id
        ));
        assert!(matches!(
            control_rx.recv().expect("user-turn bootstrap"),
            RuntimeMessage::RunEvent {
                run_generation: 79,
                event: RunEvent::UserTurnStored { session_id: received, .. },
            } if received == session_id
        ));
        assert!(matches!(
            control_rx.recv().expect("worker settlement"),
            RuntimeMessage::Finished {
                run_generation: 79,
                result: Ok(received),
            } if received.session_id() == session_id
        ));
        assert!(matches!(
            control_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(matches!(
            runtime_rx.recv().expect("unchanged runtime backlog"),
            RuntimeMessage::RunEvent {
                event: RunEvent::TextDelta { delta, .. },
                ..
            } if delta == "mailbox backlog"
        ));
    }

    #[tokio::test]
    async fn controller_drains_lossless_control_before_a_runtime_backlog() {
        let (_temp, _root, mut controller) = empty_access_test_controller().await;
        let run_generation = 80;
        let response_id = crate::protocol::ModelResponseId::new();
        let summary = RunSummary::from_terminal(
            SessionId::new(),
            TurnId::new(),
            crate::session::DurableTurnTerminal {
                outcome: crate::protocol::TurnTerminalOutcome::Completed,
                final_response_id: Some(response_id),
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            },
        );
        controller
            .run_lifecycle
            .begin(run_generation, RunControl::new());
        controller.state.begin_agent_run();
        for _ in 0..DESKTOP_RUNTIME_MAILBOX_CAPACITY {
            controller
                .runtime_tx
                .try_send(RuntimeMessage::RunEvent {
                    run_generation,
                    event: RunEvent::TextDelta {
                        response_id,
                        delta: String::new(),
                    },
                })
                .expect("fill runtime mailbox exactly");
        }
        publish_desktop_run_finished(&controller.control_tx, run_generation, Ok(summary));

        controller.drain_runtime_messages();

        assert!(
            !controller.run_lifecycle.root_is_active(),
            "control settlement must be admitted before the first bounded runtime drain"
        );
    }

    #[tokio::test]
    async fn steer_settlement_bypasses_a_full_runtime_mailbox() {
        let (_temp, root, mut controller) = empty_access_test_controller().await;
        let session_id = SessionId::new();
        controller.state.app_state.current_session_id = Some(session_id);
        let attachment = root.join("steer.png");
        controller
            .state
            .composer
            .image_attachment_paths
            .push(attachment.clone());
        let target = SteerSubmissionTarget {
            operation_id: controller.state.begin_steer_submission(),
            workspace_root: root,
            session_id,
            expected_active_turn: crate::session::ActiveTurnExpectation::initial_idle(),
        };
        let response_id = crate::protocol::ModelResponseId::new();
        for _ in 0..DESKTOP_RUNTIME_MAILBOX_CAPACITY {
            controller
                .runtime_tx
                .try_send(RuntimeMessage::RunEvent {
                    run_generation: 999,
                    event: RunEvent::TextDelta {
                        response_id,
                        delta: String::new(),
                    },
                })
                .expect("fill runtime mailbox exactly");
        }
        controller
            .control_tx
            .send(RuntimeMessage::SteerFinished {
                target,
                image_paths: vec![attachment],
                result: Ok(()),
            })
            .expect("publish steer settlement");

        controller.drain_runtime_messages();

        assert!(!controller.state.steer_submission_pending());
        assert!(controller.state.composer.image_attachment_paths.is_empty());
        assert_eq!(controller.composer_commit_generation, 1);
    }

    #[test]
    fn committed_assistant_requires_a_canonical_projection_refresh() {
        assert!(live_event_requires_canonical_refresh(
            &RunEvent::AssistantMessageCommitted {
                response_id: crate::protocol::ModelResponseId::new(),
                text: "canonical response".to_string(),
            }
        ));
    }
}

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
        // Side-chat provider credentials are deliberately isolated from the main task target.
        // A future credential surface must persist its own reference rather than inheriting the
        // main provider's environment variable or request headers.
        api_key_env: None,
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
    pending_root_submission: Option<PendingRootSubmission>,
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
        args: DesktopArgs,
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
            pending_root_submission: None,
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

    fn side_chat_projection(&self) -> DesktopSideChatProjection {
        let Some(owner_session_id) = self.state.app_state.current_session_id else {
            return DesktopSideChatProjection::default();
        };
        let current_owner_append_position = self
            .app
            .store
            .protocol_event_store()
            .canonical_snapshot_for_session(
                owner_session_id,
                ProtocolPageRequest::Latest { limit: 1 },
                ProtocolPageRequest::Latest { limit: 1 },
            )
            .ok()
            .and_then(|snapshot| snapshot.fence.append_position);
        let context_as_of_append_position =
            current_owner_append_position.map(|position| position.to_string());
        let context_truncated = false;
        let side_chat_defaults = &self.state.provider_config.effective_config.side_chat;
        let default_base_url = side_chat_defaults.base_url.clone();
        let default_model = side_chat_defaults.model.clone();
        let default_system_prompt = side_chat_defaults.system_prompt.clone();
        let default_profile = side_chat_defaults.provider_profile.as_str().to_string();
        let durable = match self
            .app
            .store
            .side_chat_repo()
            .conversation_projection(owner_session_id)
        {
            Ok(projection) => projection,
            Err(error) => {
                return DesktopSideChatProjection {
                    owner_session_id: Some(owner_session_id.to_string()),
                    model: default_model,
                    system_prompt: default_system_prompt,
                    base_url: default_base_url,
                    provider_profile: default_profile,
                    status: "failed".to_string(),
                    phase: "storage read failed".to_string(),
                    last_error: error.to_string(),
                    context_as_of_append_position,
                    context_truncated,
                    ..DesktopSideChatProjection::default()
                };
            }
        };
        let Some(durable) = durable else {
            return DesktopSideChatProjection {
                owner_session_id: Some(owner_session_id.to_string()),
                model: default_model,
                system_prompt: default_system_prompt,
                base_url: default_base_url,
                provider_profile: default_profile,
                status: "idle".to_string(),
                last_error: self
                    .side_chat_errors
                    .get(&owner_session_id)
                    .cloned()
                    .unwrap_or_default(),
                generation: "0".to_string(),
                context_as_of_append_position,
                context_truncated,
                ..DesktopSideChatProjection::default()
            };
        };
        let binding = durable.binding;
        let (draft_text, draft_quote, draft_error) =
            match decode_persisted_side_chat_draft(&binding.persisted_draft) {
                Ok(draft) => (
                    draft.text,
                    draft
                        .quote
                        .map(|quote| DesktopSideChatDraftQuoteProjection {
                            source_kind: quote.source_kind.as_str().to_string(),
                            source_history_item_id: quote.source_history_item_id.to_string(),
                            source_append_position: quote
                                .source_append_position
                                .map(|position| position.to_string()),
                            selected_text: quote.selected_text,
                        }),
                    None,
                ),
                Err(error) => (String::new(), None, Some(error)),
            };
        let draft_is_valid = draft_error.is_none();
        let mut messages = durable
            .messages
            .into_iter()
            .map(|message| DesktopSideChatMessageProjection {
                id: message.id.to_string(),
                sequence_no: usize::try_from(message.sequence_no).unwrap_or(usize::MAX),
                role: match message.role {
                    crate::storage::side_chat::SideChatConversationRole::User => "user",
                    crate::storage::side_chat::SideChatConversationRole::Assistant => "assistant",
                    crate::storage::side_chat::SideChatConversationRole::Error => "error",
                }
                .to_string(),
                content: message.content,
            })
            .collect::<Vec<_>>();
        let active = self
            .side_chat_runs
            .get(&owner_session_id)
            .filter(|run| run.side_chat_id == binding.id.to_string());
        let active_context = active.and_then(|_| self.side_chat_contexts.get(&owner_session_id));
        let context_as_of_append_position = active_context
            .and_then(|context| context.as_of_append_position)
            .or(current_owner_append_position)
            .map(|position| position.to_string());
        let context_truncated = active_context.is_some_and(|context| context.truncated);
        if let Some(run) = active
            && !run.streamed_text.is_empty()
        {
            let next_sequence = messages
                .last()
                .map_or(0, |message| message.sequence_no.saturating_add(1));
            messages.push(DesktopSideChatMessageProjection {
                id: format!("stream:{}:{}", binding.id, run.run_generation),
                sequence_no: next_sequence,
                role: "assistant".to_string(),
                content: run.streamed_text.clone(),
            });
        }
        let deleting = binding.delete_requested_at_ms.is_some();
        DesktopSideChatProjection {
            configured: true,
            deleting,
            chat_id: Some(binding.id.to_string()),
            owner_session_id: Some(owner_session_id.to_string()),
            model: binding.model,
            system_prompt: binding.system_prompt,
            base_url: binding.base_url,
            provider_profile: binding.provider_profile.as_str().to_string(),
            status: active
                .map(|_| "running".to_string())
                .unwrap_or_else(|| durable.status.key().to_string()),
            phase: active.map_or_else(
                || {
                    if deleting {
                        "deletion pending".to_string()
                    } else {
                        String::new()
                    }
                },
                |run| run.phase.clone(),
            ),
            last_error: draft_error
                .or_else(|| self.side_chat_errors.get(&owner_session_id).cloned())
                .or(durable.last_error)
                .unwrap_or_default(),
            generation: binding.request_generation.to_string(),
            draft_text,
            draft_quote,
            draft_revision: binding.draft_revision.to_string(),
            context_scope: "owner_session".to_string(),
            context_as_of_append_position,
            context_truncated,
            messages,
            can_send: draft_is_valid
                && !deleting
                && active.is_none()
                && durable.status != SessionStatus::Running,
            can_cancel: !deleting && active.is_some(),
        }
    }

    #[cfg(test)]
    pub(crate) fn configure_side_chat(
        &mut self,
        owner_session_id: SessionId,
        base_url: String,
        model: String,
        system_prompt: String,
        provider_profile: ProviderProfile,
    ) -> Result<(), String> {
        self.ensure_current_side_chat_owner(owner_session_id)?;
        if self.side_chat_runs.contains_key(&owner_session_id) {
            return Err("the side chat must be stopped before its provider is changed".to_string());
        }
        let mut side_chat_config = self
            .state
            .provider_config
            .effective_config
            .side_chat
            .clone();
        side_chat_config.base_url = base_url;
        side_chat_config.model = model;
        side_chat_config.system_prompt = system_prompt;
        side_chat_config.provider_profile = provider_profile;
        let target = SideChatProviderTarget::try_from(&side_chat_config)
            .map_err(|error| error.to_string())?;
        self.app
            .store
            .side_chat_repo()
            .configure(owner_session_id, target)
            .map_err(|error| error.to_string())?;
        self.side_chat_contexts.remove(&owner_session_id);
        self.side_chat_errors.remove(&owner_session_id);
        Ok(())
    }

    /// Materializes the current Global Side Chat defaults as a stable snapshot
    /// for this owner session. Existing conversations keep their captured
    /// provider identity until the user explicitly closes them.
    pub(crate) fn ensure_side_chat(&mut self, owner_session_id: SessionId) -> Result<(), String> {
        self.ensure_current_side_chat_owner(owner_session_id)?;
        let target = SideChatProviderTarget::try_from(
            &self.state.provider_config.effective_config.side_chat,
        )
        .map_err(|error| error.to_string())?;
        self.app
            .store
            .side_chat_repo()
            .ensure(owner_session_id, target)
            .map_err(|error| error.to_string())?;
        self.side_chat_contexts.remove(&owner_session_id);
        self.side_chat_errors.remove(&owner_session_id);
        Ok(())
    }

    pub(crate) fn save_side_chat_draft(
        &mut self,
        owner_session_id: SessionId,
        side_chat_id: SideChatId,
        expected_draft_revision: u64,
        text: String,
        quote: Option<SideChatQuoteRequest>,
    ) -> Result<(), String> {
        self.ensure_current_side_chat_owner(owner_session_id)?;
        let persisted_draft = encode_persisted_side_chat_draft(text, quote.as_ref())?;
        self.app
            .store
            .side_chat_repo()
            .update_draft(
                owner_session_id,
                side_chat_id,
                expected_draft_revision,
                persisted_draft,
            )
            .map_err(|error| error.to_string())?;
        self.side_chat_errors.remove(&owner_session_id);
        Ok(())
    }

    pub(crate) fn start_side_chat(
        &mut self,
        owner_session_id: SessionId,
        side_chat_id: SideChatId,
        expected_generation: u64,
        expected_draft_revision: u64,
        expected_owner_append_position: Option<i64>,
        quote: Option<SideChatQuoteRequest>,
        text: String,
    ) -> Result<(), String> {
        self.start_side_chat_with_ack_timeout(
            owner_session_id,
            side_chat_id,
            expected_generation,
            expected_draft_revision,
            expected_owner_append_position,
            quote,
            text,
            SIDE_CHAT_START_ACK_TIMEOUT,
        )
    }

    fn start_side_chat_with_ack_timeout(
        &mut self,
        owner_session_id: SessionId,
        side_chat_id: SideChatId,
        expected_generation: u64,
        expected_draft_revision: u64,
        expected_owner_append_position: Option<i64>,
        quote: Option<SideChatQuoteRequest>,
        text: String,
        admission_ack_timeout: std::time::Duration,
    ) -> Result<(), String> {
        self.ensure_current_side_chat_owner(owner_session_id)?;
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err("side chat text must not be empty".to_string());
        }
        if self.side_chat_runs.contains_key(&owner_session_id) {
            return Err("the side chat already has an active request".to_string());
        }
        let binding = self
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "the selected main session has no side chat".to_string())?;
        if binding.id != side_chat_id || binding.request_generation != expected_generation {
            return Err("the side chat target changed before the action".to_string());
        }
        if binding.draft_revision != expected_draft_revision {
            return Err(format!(
                "the side chat draft changed before the action: expected {expected_draft_revision}, current {}",
                binding.draft_revision
            ));
        }
        decode_persisted_side_chat_draft(&binding.persisted_draft)?;
        let provisional_generation = expected_generation
            .checked_add(1)
            .ok_or_else(|| "side chat request generation is exhausted".to_string())?;
        let conversation_session_id = binding.conversation_session_id;
        let preflight_context_window = binding.context_window;
        let preflight_system_prompt = side_chat_system_prompt(&binding.system_prompt);
        let preflight_provider_target = binding.provider_target();
        let side_chat_id_text = binding.id.to_string();
        let turn_id = TurnId::new();
        let question = text.clone();
        let user_turn = UserTurn {
            turn_id,
            items: vec![UserInputItem::Text { text }],
            prompt_dispatch: None,
            editor_context: None,
        };
        let run_control = RunControl::new();
        let cancel = run_control.token();
        let hub_route = self
            .state
            .hub_connection
            .as_ref()
            .map(|hub| hub.begin_turn(crate::hub::HubReviewContext::SideChat, cancel.clone()))
            .transpose()
            .map_err(|error| error.to_string())?
            .flatten();
        let worker_cancel = cancel.clone();
        let worker_run_control = run_control.clone();
        let store = self.app.store.clone();
        let stream_owner_session_id = owner_session_id;
        let stream_side_chat_id = side_chat_id_text.clone();
        let runtime_tx = self.runtime_tx.clone();
        let control_tx = self.control_tx.clone();
        let (worker_start_tx, worker_start_rx) = tokio::sync::oneshot::channel::<()>();
        let (admission_ack_tx, admission_ack_rx) =
            mpsc::sync_channel::<Result<(u64, SideChatContextMetadata), String>>(1);
        let worker =
            self.side_chat_task_runtime
                .spawn(provisional_generation, move || async move {
                    if worker_start_rx.await.is_err() {
                        return;
                    }
                    let publish_start_failure = |error: String| {
                        let _ = admission_ack_tx.send(Err(error.clone()));
                        let _ = control_tx.send(RuntimeMessage::SideChatFinished {
                            owner_session_id: stream_owner_session_id,
                            side_chat_id: stream_side_chat_id.clone(),
                            run_generation: provisional_generation,
                            result: Err(error),
                        });
                    };
                    let process_run_lease =
                        match store.try_acquire_run_process_lease(conversation_session_id) {
                            Ok(lease) => lease,
                            Err(error) => {
                                publish_start_failure(error.to_string());
                                return;
                            }
                        };
                    let active_run_lease = match store
                        .active_runs()
                        .try_start(conversation_session_id, worker_run_control)
                    {
                        Ok(lease) => lease,
                        Err(error) => {
                            drop(process_run_lease);
                            publish_start_failure(error.to_string());
                            return;
                        }
                    };
                    if let Err(error) = active_run_lease.set_turn_id(turn_id) {
                        drop(active_run_lease);
                        drop(process_run_lease);
                        publish_start_failure(error.to_string());
                        return;
                    }
                    let prepared = match prepare_side_chat_input(
                        &store,
                        stream_owner_session_id,
                        conversation_session_id,
                        expected_owner_append_position,
                        quote,
                        &question,
                        preflight_system_prompt,
                        preflight_context_window,
                    )
                    .await
                    {
                        Ok(prepared) => prepared,
                        Err(error) => {
                            drop(active_run_lease);
                            drop(process_run_lease);
                            publish_start_failure(error);
                            return;
                        }
                    };
                    let admitted = match store
                        .side_chat_repo()
                        .claim_and_admit_request(
                            stream_owner_session_id,
                            side_chat_id,
                            expected_generation,
                            expected_draft_revision,
                            preflight_provider_target,
                            turn_id,
                            &user_turn,
                        )
                        .await
                    {
                        Ok(admitted) => admitted,
                        Err(error) => {
                            drop(active_run_lease);
                            drop(process_run_lease);
                            publish_start_failure(error.to_string());
                            return;
                        }
                    };
                    let run_generation = admitted.generation;
                    let mut profile = side_chat_request_profile(&admitted.binding);
                    if let Some(route) = &hub_route {
                        profile.base_url = route.hub_endpoint().into();
                        profile.model = route.logical_model().into();
                        profile.provider_profile = crate::config::ProviderProfile::OpenAiCompatible;
                        profile.api_key_env = None;
                        profile.extra_headers.clear();
                        profile.hub_route = Some(route.clone());
                    }
                    profile.system_prompt = prepared.system_prompt.clone();
                    let admission_id = admitted.admission.admission_id;
                    if admission_ack_tx
                        .send(Ok((run_generation, prepared.context.clone())))
                        .is_err()
                    {
                        // The command owner disappeared after admission. Preserve canonical truth by
                        // settling the admitted turn instead of leaving an unowned Running session.
                        worker_cancel.cancel();
                    }
                    let result = execute_admitted_canonical_side_chat(
                        store,
                        conversation_session_id,
                        admission_id,
                        turn_id,
                        profile,
                        prepared.messages,
                        worker_cancel,
                        |event| {
                            let message = match event {
                                SideChatStreamEvent::TextDelta(delta) => {
                                    RuntimeMessage::SideChatDelta {
                                        owner_session_id: stream_owner_session_id,
                                        side_chat_id: stream_side_chat_id.clone(),
                                        run_generation,
                                        delta,
                                    }
                                }
                                SideChatStreamEvent::ProviderPhase(phase) => {
                                    RuntimeMessage::SideChatPhase {
                                        owner_session_id: stream_owner_session_id,
                                        side_chat_id: stream_side_chat_id.clone(),
                                        run_generation,
                                        phase,
                                    }
                                }
                            };
                            let _ = runtime_tx.try_send(message);
                        },
                    )
                    .await;
                    if let Some(route) = &hub_route {
                        route.finish().await;
                    }
                    drop(active_run_lease);
                    drop(process_run_lease);
                    let _ = control_tx.send(RuntimeMessage::SideChatFinished {
                        owner_session_id: stream_owner_session_id,
                        side_chat_id: stream_side_chat_id,
                        run_generation,
                        result,
                    });
                })?;
        if worker_start_tx.send(()).is_err() {
            drop(worker);
            return Err("side chat worker stopped before admission".to_string());
        }
        let (run_generation, phase, start_result) = match admission_ack_rx
            .recv_timeout(admission_ack_timeout)
        {
            Ok(Ok((run_generation, context))) if run_generation == provisional_generation => {
                self.side_chat_contexts.insert(owner_session_id, context);
                (run_generation, "request admitted".to_string(), Ok(()))
            }
            Ok(Ok((run_generation, _))) => {
                cancel.cancel();
                (
                    run_generation,
                    "stopping after an unexpected request generation".to_string(),
                    Err(format!(
                        "side chat admission returned generation {run_generation}, expected {provisional_generation}"
                    )),
                )
            }
            Ok(Err(error)) => {
                drop(worker);
                return Err(error);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                cancel.cancel();
                (
                    provisional_generation,
                    "start acknowledgement timed out; stop requested".to_string(),
                    Err(format!(
                        "side chat admission did not acknowledge within {}ms",
                        admission_ack_timeout.as_millis()
                    )),
                )
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                drop(worker);
                return Err("side chat worker stopped before acknowledging admission".to_string());
            }
        };
        self.side_chat_runs.insert(
            owner_session_id,
            DesktopSideChatRun {
                side_chat_id: side_chat_id_text,
                run_generation,
                cancel,
                worker,
                phase,
                streamed_text: String::new(),
                delete_after_finish: false,
            },
        );
        self.side_chat_errors.remove(&owner_session_id);
        start_result
    }

    pub(crate) fn hub_context_active(&self, context: crate::hub::HubReviewContext) -> bool {
        match context {
            crate::hub::HubReviewContext::Main => {
                self.run_lifecycle.root_is_active()
                    || self.current_agent_tree_active()
                    || self.state.prompt_enhance_pending()
            }
            crate::hub::HubReviewContext::SideChat => !self.side_chat_runs.is_empty(),
        }
    }

    pub(crate) fn cancel_side_chat(
        &mut self,
        owner_session_id: SessionId,
        side_chat_id: SideChatId,
        expected_generation: u64,
    ) -> Result<(), String> {
        self.ensure_current_side_chat_owner(owner_session_id)?;
        self.validate_side_chat_target(owner_session_id, side_chat_id, expected_generation)?;
        let run = self
            .side_chat_runs
            .get_mut(&owner_session_id)
            .ok_or_else(|| "the side chat no longer has an active request".to_string())?;
        if run.side_chat_id != side_chat_id.to_string() {
            return Err("the side chat request owner changed".to_string());
        }
        run.phase = "stop requested".to_string();
        run.cancel.cancel();
        self.side_chat_errors.remove(&owner_session_id);
        Ok(())
    }

    pub(crate) fn delete_side_chat(
        &mut self,
        owner_session_id: SessionId,
        side_chat_id: SideChatId,
        expected_generation: u64,
    ) -> Result<(), String> {
        self.ensure_current_side_chat_owner(owner_session_id)?;
        self.validate_side_chat_target(owner_session_id, side_chat_id, expected_generation)?;
        let repository = self.app.store.side_chat_repo();
        repository
            .request_delete(owner_session_id, side_chat_id, expected_generation)
            .map_err(|error| error.to_string())?;
        if let Some(run) = self.side_chat_runs.get_mut(&owner_session_id) {
            if run.side_chat_id != side_chat_id.to_string() {
                return Err("the side chat request owner changed".to_string());
            }
            run.delete_after_finish = true;
            run.phase = "stopping before deletion".to_string();
            run.cancel.cancel();
            self.side_chat_errors.remove(&owner_session_id);
            return Ok(());
        }
        repository
            .finalize_pending_deletions()
            .map_err(|error| error.to_string())?;
        self.side_chat_contexts.remove(&owner_session_id);
        self.side_chat_errors.remove(&owner_session_id);
        Ok(())
    }

    pub(crate) fn set_side_chat_command_error(
        &mut self,
        owner_session_id: SessionId,
        message: impl Into<String>,
    ) {
        self.side_chat_errors
            .insert(owner_session_id, message.into());
    }

    fn ensure_current_side_chat_owner(&self, owner_session_id: SessionId) -> Result<(), String> {
        if self.state.app_state.current_session_id != Some(owner_session_id) {
            return Err(
                "the selected main session changed before the side chat action".to_string(),
            );
        }
        Ok(())
    }

    fn validate_side_chat_target(
        &self,
        owner_session_id: SessionId,
        side_chat_id: SideChatId,
        expected_generation: u64,
    ) -> Result<(), String> {
        let binding = self
            .app
            .store
            .side_chat_repo()
            .get_by_owner(owner_session_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "the selected main session has no side chat".to_string())?;
        if binding.id != side_chat_id || binding.request_generation != expected_generation {
            return Err("the side chat target changed before the action".to_string());
        }
        Ok(())
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

    pub(crate) fn interrupt_session(
        &mut self,
        session_id: SessionId,
        expected_turn_id: TurnId,
        expected_admission_revision: u64,
    ) -> bool {
        let expected_active_turn = ActiveTurnExpectation::Turn {
            turn_id: expected_turn_id,
            revision: expected_admission_revision,
        };
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
            self.cancel_exact_turn_at(expected_turn_id, expected_admission_revision);
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
        self.spawn_session_interrupt(target, expected_active_turn);
        true
    }

    pub(crate) fn set_session_search(&mut self, text: String) -> bool {
        if !self.ensure_navigation_admission("session search") {
            return false;
        }
        self.state.set_session_search_text(text.clone());
        self.admit_session_search(text, self.state.view.session_search_include_archived);
        true
    }

    pub(crate) fn set_session_search_include_archived(&mut self, include_archived: bool) -> bool {
        if !self.ensure_navigation_admission("session search") {
            return false;
        }
        self.state
            .set_session_search_include_archived(include_archived);
        self.admit_session_search(
            self.state.view.session_search_text.clone(),
            include_archived,
        );
        true
    }

    fn admit_session_search(&mut self, query: String, include_archived: bool) {
        let operation_id = self.state.begin_session_search();
        let target = SessionSearchRequestTarget {
            query,
            include_archived,
            selected_session_id: self.state.selected_session_id(),
        };
        let admission = self.session_search_requests.begin(operation_id, target);
        if let Some(operation_id) = admission.superseded_operation_id {
            let _ = self.state.finish_session_search(operation_id);
        }
        if let Some(dispatch) = admission.dispatch {
            self.spawn_session_search(dispatch);
        }
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
            .authority_root()
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
        let Some(session) = self
            .state
            .open_session
            .as_ref()
            .filter(|open_session| open_session.session_id() == session_id)
            .map(|open_session| open_session.session().clone())
        else {
            self.state
                .set_status_message("transcript export target is no longer available");
            return;
        };
        let detail = self.state.selected_detail();
        if detail.transcript_rows.is_empty() {
            self.state
                .set_status_message("open transcript has no rows to export");
            return;
        }
        let file_name = transcript_markdown_file_name(&session.title, session.id);
        let export_path = session
            .cwd
            .join(".moyai")
            .join("transcript-exports")
            .join(file_name);
        let markdown = open_transcript_rows_to_markdown(
            &session.title,
            &session.cwd,
            session.id,
            &session.base_url,
            &session.model,
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
            workspace_authority_root: self.app.workspace.authority_root().to_path_buf(),
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
        let Some(next_offset) = self.state.next_turn_page_offset() else {
            self.state
                .set_status_message("later turn page is not available");
            return;
        };
        self.load_selected_turn_page(next_offset);
    }

    fn load_selected_turn_page(&mut self, offset: usize) {
        if self.state.turn_page_load_pending() {
            self.state
                .set_status_message("turn page load is already active");
            return;
        }
        if !self.state.can_begin_turn_page_load() {
            self.state
                .set_status_message("turn page cannot load while the current session is changing");
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
                    let read = canonical_markdown_export_read(&service, session_id).await?;
                    if read.history.items.is_empty() {
                        return Err(crate::error::SessionError::Message(
                            "canonical protocol history is empty".to_string(),
                        ));
                    }
                    Ok::<_, crate::error::SessionError>(read)
                })
                .map_err(|error| error.to_string())
                .and_then(|read| {
                    let markdown = canonical_session_read_to_markdown(&read);
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
        let target = SessionLoadRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            workspace_cwd: self.app.workspace.cwd.clone(),
            project_id: self.app.workspace.project_id,
            session_id,
        };
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session runtime");
            let result = runtime.block_on(load_session_navigation_result(app, session_id, reason));
            let _ = runtime_tx.send(RuntimeMessage::SessionLoaded {
                request_id,
                target,
                reason,
                result,
            });
        });
    }

    fn spawn_current_session_refresh(&mut self, session_id: SessionId) {
        self.spawn_current_session_refresh_page(
            session_id,
            CurrentSessionRefreshPurpose::Refresh,
            None,
        );
    }

    fn spawn_current_session_refresh_page(
        &mut self,
        session_id: SessionId,
        purpose: CurrentSessionRefreshPurpose,
        offset: Option<usize>,
    ) {
        let app = self.app.clone();
        let target = SessionRefreshRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            session_id,
        };
        let request_id = self
            .session_projection_refresh_requests
            .begin(target.clone());
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop current-session-refresh runtime");
            let result = runtime.block_on(async move {
                let detail = if let Some(offset) = offset {
                    let read = app
                        .session_service
                        .canonical_session_read(
                            session_id,
                            0,
                            DESKTOP_HISTORY_PROJECTION_LIMIT,
                            offset,
                            DESKTOP_TURN_PAGE_LIMIT,
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    LoadedSessionDetail { read }
                } else {
                    load_latest_session_detail(&app, session_id)
                        .await
                        .map_err(|error| error.to_string())?
                };
                loaded_session_from_detail_with_activity(&app, detail).await
            });
            let _ = runtime_tx.send(RuntimeMessage::CurrentSessionRefreshed {
                request_id,
                target,
                purpose,
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
                let read = app
                    .session_service
                    .canonical_session_read(
                        session_id,
                        0,
                        DESKTOP_HISTORY_PROJECTION_LIMIT,
                        offset,
                        limit,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(LoadedSession {
                    read,
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

    fn spawn_latest_live_session_refresh(&mut self, session_id: SessionId) {
        let app = self.app.clone();
        let contiguous_offset = self
            .state
            .open_session
            .as_ref()
            .filter(|open_session| open_session.session_id() == session_id)
            .map(OpenSessionView::loaded_turn_end);
        let target = SessionRefreshRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            session_id,
        };
        let request_id = self
            .session_projection_refresh_requests
            .begin(target.clone());
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop latest-session-refresh runtime");
            let result = runtime.block_on(async move {
                let detail = if let Some(offset) = contiguous_offset {
                    let read = app
                        .session_service
                        .canonical_session_read(
                            session_id,
                            0,
                            DESKTOP_HISTORY_PROJECTION_LIMIT,
                            offset,
                            DESKTOP_TURN_PAGE_LIMIT,
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    LoadedSessionDetail { read }
                } else {
                    load_latest_session_detail(&app, session_id)
                        .await
                        .map_err(|error| error.to_string())?
                };
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
        let target = SessionLoadRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            workspace_cwd: self.app.workspace.cwd.clone(),
            project_id: self.app.workspace.project_id,
            session_id,
        };
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session rejoin runtime");
            let result = runtime.block_on(load_session_navigation_result(
                app,
                session_id,
                SessionLoadReason::RunningRejoin,
            ));
            let _ = runtime_tx.send(RuntimeMessage::SessionLoaded {
                request_id,
                target,
                reason: SessionLoadReason::RunningRejoin,
                result,
            });
        });
    }

    fn spawn_session_cancel_persist(
        &mut self,
        session_id: SessionId,
        expected_active_turn: ActiveTurnExpectation,
    ) {
        let app = self.app.clone();
        let root_admission_fence = self.next_root_run_generation;
        let target = SessionRefreshRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            session_id,
        };
        let request_id = self
            .session_projection_refresh_requests
            .begin(target.clone());
        let control_tx = self.control_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop cancel-persist runtime");
            let (durable_outcome, in_memory_stop_accepted, result) = runtime.block_on(async move {
                let (durable_outcome, in_memory_stop_accepted) =
                    match claim_exact_root_execution_stop(&app, session_id, expected_active_turn)
                        .await
                    {
                        Ok(outcome) => outcome,
                        Err(error) => return (None, false, Err(error.to_string())),
                    };
                let detail = match load_session_detail(&app, session_id).await {
                    Ok(detail) => detail,
                    Err(error) => {
                        return (
                            Some(durable_outcome),
                            in_memory_stop_accepted,
                            Err(error.to_string()),
                        );
                    }
                };
                (
                    Some(durable_outcome),
                    in_memory_stop_accepted,
                    loaded_session_from_detail_with_activity(&app, detail).await,
                )
            });
            let _ = control_tx.send(RuntimeMessage::CurrentSessionRefreshed {
                request_id,
                target,
                purpose: CurrentSessionRefreshPurpose::StopRequestRefresh {
                    root_admission_fence,
                    root_stop_attempt: None,
                    expected_active_turn: Some(expected_active_turn),
                    in_memory_stop_accepted,
                    durable_outcome,
                },
                result,
            });
        });
    }

    fn spawn_root_session_cancel_persist(
        &mut self,
        generation: u64,
        session_id: SessionId,
        root_scope_control: RunControl,
        stop_plan: crate::app::run_service::RootExecutionStopPlan,
        root_stop_attempt: DesktopRootStopAttempt,
    ) {
        let app = self.app.clone();
        let root_admission_fence = self.next_root_run_generation;
        let target = SessionRefreshRequestTarget {
            workspace_root: self.app.workspace.root.clone(),
            session_id,
        };
        let request_id = self
            .session_projection_refresh_requests
            .begin(target.clone());
        let control_tx = self.control_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop root cancel-persist runtime");
            let (durable_outcome, in_memory_stop_accepted, expected_active_turn, result) = runtime
                .block_on(async move {
                    let claim = match app
                        .run_service
                        .claim_root_execution_stop(&root_scope_control, stop_plan)
                        .await
                    {
                        Ok(claim) => claim,
                        Err(error) => return (None, false, None, Err(error.to_string())),
                    };
                    let local_stop_accepted = claim
                        .local_stop
                        .is_some_and(crate::runtime::RootExecutionLocalStop::stop_accepted);
                    if claim
                        .session_id
                        .is_some_and(|claimed| claimed != session_id)
                    {
                        return (
                            claim.durable_outcome,
                            local_stop_accepted,
                            claim.expected_active_turn,
                            Err("the canonical root Stop plan changed sessions".to_string()),
                        );
                    }
                    let detail = match load_session_detail(&app, session_id).await {
                        Ok(detail) => detail,
                        Err(error) => {
                            return (
                                claim.durable_outcome,
                                local_stop_accepted,
                                claim.expected_active_turn,
                                Err(error.to_string()),
                            );
                        }
                    };
                    (
                        claim.durable_outcome,
                        local_stop_accepted,
                        claim.expected_active_turn,
                        loaded_session_from_detail_with_activity(&app, detail).await,
                    )
                });
            let _ = control_tx.send(RuntimeMessage::CurrentSessionRefreshed {
                request_id,
                target,
                purpose: CurrentSessionRefreshPurpose::StopRequestRefresh {
                    root_admission_fence,
                    root_stop_attempt: Some(root_stop_attempt),
                    expected_active_turn,
                    in_memory_stop_accepted,
                    durable_outcome,
                },
                result,
            });
        });
        debug_assert_eq!(self.run_lifecycle.root_generation(), Some(generation));
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

    fn spawn_session_interrupt(
        &self,
        target: SessionMutationRequestTarget,
        expected_active_turn: ActiveTurnExpectation,
    ) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let session_id = target.session_id;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session-interrupt runtime");
            let result = runtime.block_on(async move {
                let (outcome, _) =
                    claim_exact_root_execution_stop(&app, session_id, expected_active_turn).await?;
                if matches!(outcome, ExactRootExecutionStopOutcome::TargetChanged) {
                    return Err("the running chat changed before interrupt was applied".to_string());
                }
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

    fn spawn_session_search(&self, dispatch: SessionSearchDispatch<SessionSearchRequestTarget>) {
        let app = self.app.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let SessionSearchDispatch { request_id, target } = dispatch;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop session-search runtime");
            let result = runtime.block_on(async move {
                load_snapshot_for_session_search(
                    &app,
                    &target.query,
                    target.include_archived,
                    target.selected_session_id,
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
                    let process_runtime = app.process_runtime.clone();
                    app = AppBootstrap::rebuild_for_directory_with_process_runtime(
                        &next_root,
                        process_runtime,
                    )
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

    pub(crate) fn start_run_at(
        &mut self,
        prompt: String,
        expected_active_turn: ActiveTurnExpectation,
    ) -> bool {
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
        self.launch_run_with_options(prompt, prompt_dispatch, None, None, expected_active_turn)
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
            .set_status_message("opening workspace-free quick chat...");
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

    pub(crate) fn create_project_from_picker(&mut self) -> bool {
        if !self.ensure_navigation_admission("project") {
            return false;
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
        true
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
        if self
            .state
            .hub_connection
            .as_ref()
            .is_some_and(|hub| hub.projection_now().main_mode == crate::hub::HubRouteMode::Hub)
        {
            self.state
                .set_status_message("この送信先では依頼の整形は未対応です。");
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
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop enhance runtime");
            let result = runtime.block_on(async move {
                crate::tui::prompt_enhance::enhance_prompt_for_captured_idle(
                    &session_service,
                    target_session_id,
                    target_expected_active_turn,
                    || async move {
                        crate::tui::prompt_enhance::enhance_prompt(
                            &config,
                            &raw_prompt,
                            cancellation,
                        )
                        .await
                        .map_err(|error| error.to_string())
                    },
                )
                .await
            });
            let _ = runtime_tx.send(RuntimeMessage::EnhanceFinished {
                request_id,
                target,
                result,
            });
        });
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
        finish_setup: bool,
    ) -> Result<(), String> {
        let path =
            crate::config::loader::global_config_path().map_err(|_| "storage_error".to_string())?;
        let config = crate::tui::config_editor::save_device_network_config(
            &path,
            &self.state.global_config().device_network,
            shared,
            |candidate| {
                if finish_setup
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
        if finish_setup {
            self.pending_initial_setup_config_import = None;
            self.state.complete_initial_setup_after_persist();
            self.state.show_hub_editor();
        }
        Ok(())
    }

    fn start_new_chat_with_global_access(&mut self) {
        self.state.start_new_chat();
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
        self.state
            .set_status_message("applied provider selection to this UI session");
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
                self.state
                    .set_status_message("applied config to this UI session");
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
        Ok(saved.message)
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

    pub(crate) fn access_mode_mutation_runtime_contract(&self) -> (String, bool) {
        let root_run_generation = self.root_run_generation();
        let agent_tree_active = self.current_agent_tree_active();
        (
            access_runtime_owner_token(
                root_run_generation,
                agent_tree_active,
                self.last_root_run_epoch(),
            ),
            !self.state.navigation_loading() && !self.state.background_mutation_pending(),
        )
    }

    pub(crate) fn pending_permission_confirmation_id(&self) -> Option<u64> {
        self.pending_permission
            .as_ref()
            .map(|pending| pending.confirmation_id)
    }

    pub(crate) fn access_mode_mutation_admission_open(&self) -> bool {
        self.access_mode_mutation_runtime_contract().1
    }

    pub(crate) fn session_settings_turn_config_mutation_admission_open(&self) -> bool {
        self.config_draft_mutation_admission_open() && !self.current_agent_tree_active()
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
        let after_root = self.access_mode_persistence_relation_after_root_finish(target);
        if after_root != AccessModePersistenceTargetRelation::Stale {
            return after_root;
        }
        self.access_mode_persistence_relation_after_tree_finish(target)
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

    fn access_mode_persistence_relation_after_tree_finish(
        &self,
        target: &AccessModePersistenceTarget,
    ) -> AccessModePersistenceTargetRelation {
        if target.root_run_generation.is_some()
            || target.workspace_root != self.app.workspace.root
            || target.config_generation != self.state.provider_config.config_generation
            || self.root_run_generation().is_some()
            || self.current_agent_tree_active()
            || target.runtime_owner_token != format!("tree:{}", self.last_root_run_epoch())
        {
            return AccessModePersistenceTargetRelation::Stale;
        }
        match (target.session_id, self.state.app_state.current_session_id) {
            (target_session_id, current_session_id) if target_session_id == current_session_id => {
                AccessModePersistenceTargetRelation::Exact
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
                            updated.map(|update| Some(update.session)).ok_or_else(|| {
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
        PersistSession: FnOnce(SessionId, crate::config::AccessMode) -> Result<Option<SessionRecord>, String>
            + Send
            + 'static,
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
        let mut global_config = self.state.global_config().clone();
        global_config.permissions.access_mode = pending.target.access_mode;
        self.app.config = global_config.clone();
        self.state.replace_global_config(global_config);
        self.state.set_status_message(format!(
            "global config access mode set to {} and remembered in {}; it applies to the next permission decision; an already displayed confirmation is unchanged",
            access_mode_display_label(pending.target.access_mode),
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
            |session_id, access_mode| persist_session(session_id, access_mode),
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
        let scope = if current_root_session_id.is_some() {
            "global config and current root session"
        } else {
            "global config"
        };
        let config_message = format!(
            "{scope} access mode set to {} and remembered in {}; it applies to the next permission decision; an already displayed confirmation is unchanged",
            access_mode_display_label(access_mode),
            remembered_path
        );
        self.state.set_status_message(config_message);
        true
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
                "saved access mode for this root session; it applies to the next permission decision and does not rewrite an already pending request"
            } else {
                "saved settings for this root session; provider and model values apply to the next admitted turn"
            }
        } else {
            "session settings already match the saved root-session values"
        });
        true
    }

    pub(crate) fn pick_global_config_toml_dialog(&mut self) -> Option<Utf8PathBuf> {
        match pick_config_toml_file(None) {
            Ok(path) => path,
            Err(error) => {
                self.state.set_typed_status_message(
                    DesktopStatusCode::ConfigImportFailed,
                    format!("config import failed: {error}"),
                );
                None
            }
        }
    }

    pub(crate) fn pick_initial_setup_config_toml_dialog(
        start_dir: &Utf8Path,
    ) -> Result<Option<Utf8PathBuf>, String> {
        pick_config_toml_file(Some(start_dir))
    }

    pub(crate) fn load_initial_setup_config_toml_path(
        path: &Utf8Path,
    ) -> Result<ResolvedConfig, String> {
        validate_import_config_extension(path)?;
        let text = read_toml_utf8_bounded(path).map_err(public_config_import_error)?;
        ConfigLoader::resolve_global_config_text_without_environment(path, &text)
            .map_err(public_config_import_error)
    }

    pub(crate) fn stage_initial_setup_config_import(
        &mut self,
        config: ResolvedConfig,
    ) -> Result<(u64, Vec<InitialSetupConfigPublicValue>), String> {
        let generation = self.next_initial_setup_import_generation;
        self.next_initial_setup_import_generation = generation
            .checked_add(1)
            .ok_or_else(|| "initial setup import generation is exhausted".to_string())?;
        let values = ConfigField::ALL
            .into_iter()
            .filter(|field| !field.is_host_owned_generation())
            .map(|field| {
                let public = field.public_value(&config);
                InitialSetupConfigPublicValue {
                    key: field.label().to_string(),
                    text: public.value,
                    sensitive: public.sensitive,
                    configured: public.configured,
                }
            })
            .collect();
        self.pending_initial_setup_config_import = Some(PendingInitialSetupConfigImport {
            generation,
            workspace_root: self.app.workspace.authority_root().to_path_buf(),
            session_id: self.state.app_state.current_session_id,
            global_config_path: self.state.startup.global_config_path.clone(),
            setup_generation: self.state.startup.setup_generation,
            config_generation: self.state.provider_config.config_generation,
            config,
        });
        Ok((generation, values))
    }

    fn reconcile_pending_initial_setup_config_import_owner(&mut self) {
        let owner_changed = self
            .pending_initial_setup_config_import
            .as_ref()
            .is_some_and(|candidate| {
                candidate.workspace_root != self.app.workspace.authority_root()
                    || candidate.session_id != self.state.app_state.current_session_id
                    || candidate.global_config_path != self.state.startup.global_config_path
                    || candidate.setup_generation != self.state.startup.setup_generation
                    || candidate.config_generation != self.state.provider_config.config_generation
            });
        if owner_changed {
            self.pending_initial_setup_config_import = None;
        }
    }

    fn hydrate_initial_setup_import_sensitive_values(
        &self,
        mut values: Vec<(String, String)>,
        expected_import_generation: Option<u64>,
    ) -> Result<Vec<(String, String)>, String> {
        let Some(expected_generation) = expected_import_generation else {
            return Ok(values);
        };
        let candidate = self
            .pending_initial_setup_config_import
            .as_ref()
            .filter(|candidate| {
                candidate.generation == expected_generation
                    && candidate.workspace_root == self.app.workspace.authority_root()
                    && candidate.session_id == self.state.app_state.current_session_id
                    && candidate.global_config_path == self.state.startup.global_config_path
                    && candidate.setup_generation == self.state.startup.setup_generation
                    && candidate.config_generation == self.state.provider_config.config_generation
            })
            .ok_or_else(|| {
                "the imported configuration owner changed; import the TOML again".to_string()
            })?;
        for (key, text) in &mut values {
            let Some(field) = ConfigField::ALL
                .into_iter()
                .find(|field| field.label() == key.as_str())
            else {
                continue;
            };
            if field.is_sensitive() && text.trim().is_empty() {
                *text = field.editor_value(&candidate.config);
            }
        }
        Ok(values)
    }

    pub(crate) fn import_global_config_toml_path(&mut self, path: &Utf8Path) -> bool {
        match import_global_config_toml(path) {
            Ok(message) => {
                if !self.reload_config_with_status_code(DesktopStatusCode::ConfigImportFailed) {
                    return false;
                }
                self.state.set_status_message(message);
                true
            }
            Err(error) => {
                self.state.set_typed_status_message(
                    DesktopStatusCode::ConfigImportFailed,
                    format!("config import failed: {error}"),
                );
                false
            }
        }
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

    pub(crate) fn switch_workspace_to(&mut self, text: String) -> bool {
        if !self.ensure_navigation_admission("workspace") {
            return false;
        }
        self.state.set_workspace_input(text);
        self.begin_workspace_switch_from_input()
    }

    fn begin_workspace_switch_from_input(&mut self) -> bool {
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
        let path = self.app.workspace.cwd.to_string();
        self.state.show_workspace_picker(&path);
    }

    fn spawn_workspace_load(&self, requested: Utf8PathBuf, request_id: NavigationRequestId) {
        self.spawn_workspace_load_for_selection(
            requested,
            None,
            request_id,
            WorkspaceRootMode::Discover,
        );
    }

    fn spawn_fixed_workspace_load(&self, requested: Utf8PathBuf, request_id: NavigationRequestId) {
        self.spawn_workspace_load_for_selection(
            requested,
            None,
            request_id,
            WorkspaceRootMode::Fixed,
        );
    }

    fn spawn_workspace_load_for_new_project_session(
        &self,
        requested: Utf8PathBuf,
        request_id: NavigationRequestId,
    ) {
        let process_runtime = self.app.process_runtime.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop workspace runtime");
            let result = runtime.block_on(async move {
                let app = AppBootstrap::rebuild_for_directory_with_process_runtime(
                    &requested,
                    process_runtime,
                )
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
        root_mode: WorkspaceRootMode,
    ) {
        let process_runtime = self.app.process_runtime.clone();
        let runtime_tx = self.runtime_tx.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build desktop workspace runtime");
            let result = runtime.block_on(async move {
                let app = match root_mode {
                    WorkspaceRootMode::Discover => {
                        AppBootstrap::rebuild_for_directory_with_process_runtime(
                            &requested,
                            process_runtime,
                        )
                        .await
                    }
                    WorkspaceRootMode::Fixed => {
                        AppBootstrap::rebuild_for_directory_as_workspace_root_with_process_runtime(
                            &requested,
                            process_runtime,
                        )
                        .await
                    }
                }
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
            Some(self.app.workspace.cwd.clone())
        } else {
            self.resolve_workspace_input()
                .or_else(|| Some(self.app.workspace.cwd.clone()))
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
        let root = self.app.workspace.authority_root().to_path_buf();
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

    pub(crate) fn open_user_data_folder(&mut self) {
        let folder = self.app.store.paths().data_dir.clone();
        if let Err(error) = std::fs::create_dir_all(folder.as_std_path()) {
            self.state.set_status_message(format!(
                "failed to create user data folder {}: {error}",
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
            self.app.workspace.authority_root().join(path)
        };
        let folder = if absolute_path.is_dir() {
            absolute_path
        } else if let Some(parent) = absolute_path.parent() {
            parent.to_path_buf()
        } else {
            self.app.workspace.authority_root().to_path_buf()
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

    fn persist_preferences(&mut self) {
        if !self.persist_preferences_to_disk {
            return;
        }
        self.preferences.window_opacity_percent = Some(self.state.view.window_opacity_percent);
        self.preferences.last_workspace = self.workspace_path_for_preferences();
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

    pub(crate) fn cancel_root_run_at_generation(&mut self, generation: u64) -> bool {
        let (root_stop_attempt, root_scope_control) =
            match self.run_lifecycle.begin_stop_attempt(generation) {
                DesktopRootStopAttemptAdmission::Acquired {
                    attempt,
                    run_control,
                } => (attempt, run_control),
                DesktopRootStopAttemptAdmission::AlreadyPending => {
                    self.state
                        .set_status_message("the exact root task Stop is already being validated");
                    return true;
                }
                DesktopRootStopAttemptAdmission::NotOwned => return false,
                DesktopRootStopAttemptAdmission::Exhausted => {
                    self.state.set_status_message(
                        "root Stop attempt identity is exhausted; restart moyAI",
                    );
                    return false;
                }
            };
        let stop_plan = match self
            .app
            .run_service
            .seal_root_execution_for_stop(&root_scope_control)
        {
            Ok(crate::app::run_service::RootExecutionStopSealOutcome::Acquired(plan)) => plan,
            Ok(crate::app::run_service::RootExecutionStopSealOutcome::AlreadySealed) => {
                self.run_lifecycle.finish_stop_attempt(root_stop_attempt);
                self.state
                    .set_status_message("the exact root task Stop is already being validated");
                return true;
            }
            Ok(crate::app::run_service::RootExecutionStopSealOutcome::Rejected) => {
                self.run_lifecycle.finish_stop_attempt(root_stop_attempt);
                return false;
            }
            Err(error) => {
                self.run_lifecycle.finish_stop_attempt(root_stop_attempt);
                self.state.set_status_message(format!(
                    "failed to seal the exact root Stop target: {error}"
                ));
                return false;
            }
        };
        if !stop_plan.has_durable_owner() {
            let claim = self
                .app
                .run_service
                .claim_unadmitted_root_execution_stop(&root_scope_control, stop_plan);
            self.run_lifecycle.finish_stop_attempt(root_stop_attempt);
            return match claim {
                Ok(claim)
                    if claim
                        .local_stop
                        .is_some_and(crate::runtime::RootExecutionLocalStop::stop_accepted) =>
                {
                    self.durable_agent_activity_refresh_requests.clear();
                    self.state.mark_run_stop_requested(
                        "run cancellation requested",
                        "停止を要求しました。現在の処理を中断しています。",
                    );
                    true
                }
                Ok(_) => false,
                Err(error) => {
                    self.state.set_status_message(format!(
                        "failed to stop the pre-admission root task: {error}"
                    ));
                    false
                }
            };
        }
        let Some(session_id) = stop_plan.session_id_hint() else {
            self.run_lifecycle.finish_stop_attempt(root_stop_attempt);
            self.state
                .set_status_message("the sealed root Stop plan lost its durable session owner");
            return false;
        };
        self.state.mark_post_run_refresh_pending();
        self.spawn_root_session_cancel_persist(
            generation,
            session_id,
            root_scope_control,
            stop_plan,
            root_stop_attempt,
        );
        self.state
            .set_status_message("validating the exact root task Stop target...");
        true
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

    fn commit_pending_root_submission(&mut self, run_generation: u64) -> bool {
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
            .apply_durable_prompt_dispatch(&pending.prompt_dispatch);
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
        prompt_review_to_cancel: Option<PromptReviewTarget>,
        expected_active_turn: ActiveTurnExpectation,
    ) -> bool {
        if self.state.app_state.prompt_review.is_some() && prompt_review_to_cancel.is_none() {
            self.state.set_status_message(
                "the active Prompt Review must be sent or cancelled through its exact target",
            );
            return false;
        }
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
        if self.state.post_run_refresh_pending() {
            self.state.set_status_message(
                "wait for the completed task to finish refreshing before sending",
            );
            return false;
        }
        if matches!(expected_active_turn, ActiveTurnExpectation::Turn { .. }) {
            if review_request.is_none()
                && prompt_review_to_cancel.is_none()
                && !prompt.trim().is_empty()
            {
                return self.launch_active_turn_steer(prompt, expected_active_turn);
            }
            self.state
                .set_status_message("the captured active turn cannot admit this request");
            return false;
        }
        if self.run_lifecycle.root_is_active()
            || matches!(
                self.state.app_state.run_status,
                crate::tui::state::RunStatus::Running
            )
        {
            self.state.set_status_message(
                "the idle run owner changed before submission; refresh and try again",
            );
            return false;
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
        let run_control = RunControl::new();
        let hub_route = match self
            .state
            .hub_connection
            .as_ref()
            .map(|hub| hub.begin_turn(crate::hub::HubReviewContext::Main, run_control.token()))
            .transpose()
        {
            Ok(route) => route.flatten(),
            Err(error) => {
                self.state.set_status_message(error.to_string());
                return false;
            }
        };
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
            config: RunConfigInput::Resolved(self.state.provider_config.effective_config.clone()),
            output_mode: OutputMode::Human,
            show_reasoning_summary: true,
            prompt_dispatch: Some(prompt_dispatch.clone()),
            editor_context: Some(self.current_editor_context()),
            review_request,
            image_paths,
            run_control: run_control.clone(),
            session_access_mode_adoption: None,
            agent_confirmation: None,
            agent_context: None,
            admission_kind: crate::app::RunAdmissionKind::NewUserRun,
            expected_active_turn,
        };
        self.run_lifecycle.begin(run_generation, run_control);
        self.pending_root_submission = Some(PendingRootSubmission {
            run_generation,
            owner_workspace_path: self.root_submission_owner_workspace_path(),
            owner_session_id: request.session_id,
            prompt_dispatch,
            image_paths: self.state.composer.image_attachment_paths.clone(),
            prompt_review_to_cancel,
        });
        let run_service = hub_route.as_ref().map_or_else(
            || self.app.run_service.clone(),
            |route| Arc::new(self.app.run_service.with_hub_turn(route.clone())),
        );
        let runtime_tx = self.runtime_tx.clone();
        let control_tx = self.control_tx.clone();
        let next_permission_request_id = self.next_permission_request_id.clone();
        let notification_title = request
            .title
            .clone()
            .unwrap_or_else(|| self.state.current_session_label());
        let worker = self
            .root_task_runtime
            .spawn(run_generation, move || async move {
                let mut request = request;
                let worker_run_control = request.run_control.clone();
                let root_run_control = request.run_control.clone();
                let mut renderer = DesktopRenderer {
                    runtime_tx,
                    bootstrap_control: control_tx.clone(),
                    run_generation,
                    notification_title: notification_title.clone(),
                    notified_terminal: false,
                };
                let mut prompt = SharedConfirmationPrompt::new_with_root_control(
                    DesktopConfirmationPrompt {
                        control: control_tx.clone(),
                        next_permission_request_id,
                    },
                    root_run_control,
                );
                request.agent_confirmation = Some(prompt.clone());
                let result = run_service
                    .execute(AppCommand::Run(request), &mut renderer, &mut prompt)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|outcome| match outcome {
                        AppCommandOutcome::Turn(summary) => Ok(summary),
                        AppCommandOutcome::ControlCompleted => {
                            Err("run command completed without a terminal turn summary".to_string())
                        }
                    });
                if let Some(route) = &hub_route {
                    route.finish().await;
                }
                match &result {
                    Ok(summary) if !renderer.notified_terminal => {
                        let notification_body =
                            run_completion_notification_body(&renderer.notification_title, summary);
                        send_windows_desktop_notification("moyAI", &notification_body);
                    }
                    Err(error)
                        if !renderer.notified_terminal
                            && desktop_run_failure_notification_allowed(
                                worker_run_control.cause().as_ref(),
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
                publish_desktop_run_finished(&control_tx, run_generation, result);
            });
        let worker = match worker {
            Ok(worker) => worker,
            Err(error) => {
                self.discard_pending_root_submission(run_generation);
                self.run_lifecycle.finish_root();
                self.state.finish_agent_run();
                self.state.app_state.run_status = crate::tui::state::RunStatus::Failed;
                self.state
                    .set_status_message(format!("failed to start desktop run worker: {error}"));
                return false;
            }
        };
        if let Err(worker) = self.run_lifecycle.attach_worker(run_generation, worker) {
            worker.abort();
            self.discard_pending_root_submission(run_generation);
            self.run_lifecycle.finish_root();
            self.state.finish_agent_run();
            self.state.app_state.run_status = crate::tui::state::RunStatus::Failed;
            self.state
                .set_status_message("desktop run owner changed before worker attachment");
            return false;
        }
        true
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

    fn apply_session_loaded_message(
        &mut self,
        request_id: NavigationRequestId,
        target: SessionLoadRequestTarget,
        reason: SessionLoadReason,
        result: Result<SessionNavigationLoadResult, String>,
    ) {
        if target.workspace_root != self.app.workspace.root
            || target.workspace_cwd != self.app.workspace.cwd
            || target.project_id != self.app.workspace.project_id
            || !self
                .state
                .is_current_session_navigation(request_id, target.session_id)
        {
            return;
        }
        let session_id = target.session_id;
        match result {
            Ok(result) => {
                if self.session_load_is_blocked_by_active_run() {
                    self.state.finish_navigation(request_id);
                    return;
                }
                self.state.finish_navigation(request_id);
                if let Some(workspace) = result.workspace
                    && !self.replace_workspace_from_load(workspace)
                {
                    return;
                }
                let loaded = result.loaded;
                let loaded_status = loaded.read.session.status;
                self.state.load_open_session(&loaded.read);
                if let Some(records) = loaded.agent_activity_records {
                    self.loaded_agent_activity_records = Some((loaded.read.session.id, records));
                    self.durable_agent_activity_refresh_failures = 0;
                }
                if !self.state.status_code.is_terminal_interruption()
                    && !(reason == SessionLoadReason::RunningRejoin
                        && loaded_status != SessionStatus::Running)
                {
                    self.state.set_status_message(match reason {
                        SessionLoadReason::RunningRejoin => {
                            format!("rejoined running session {}", session_id)
                        }
                        SessionLoadReason::UserSelection => {
                            format!("opened session {}", session_id)
                        }
                    });
                }
                self.reconcile_runtime_listener_with_open_session();
            }
            Err(error) => {
                finish_navigation_failure(&mut self.state, request_id, error);
            }
        }
    }

    fn apply_current_session_refreshed_message(
        &mut self,
        session_id: SessionId,
        purpose: CurrentSessionRefreshPurpose,
        result: Result<LoadedSession, String>,
    ) {
        if (matches!(purpose, CurrentSessionRefreshPurpose::Refresh)
            && self.session_load_is_blocked_by_active_run())
            || self.state.app_state.current_session_id != Some(session_id)
        {
            self.state.clear_post_run_refresh_pending();
            return;
        }
        match result {
            Ok(loaded) => {
                let loaded_status = loaded.read.session.status;
                let loaded_active_turn_expectation = if loaded_status == SessionStatus::Running {
                    loaded
                        .read
                        .active_turn_id
                        .map(|turn_id| ActiveTurnExpectation::Turn {
                            turn_id,
                            revision: loaded.read.admission_revision,
                        })
                        .unwrap_or(ActiveTurnExpectation::Idle {
                            latest_turn_id: loaded.read.latest_turn_id,
                            revision: loaded.read.admission_revision,
                        })
                } else {
                    ActiveTurnExpectation::Idle {
                        latest_turn_id: loaded.read.latest_turn_id,
                        revision: loaded.read.admission_revision,
                    }
                };
                let page_has_more = loaded.read.turns.has_more;
                let history_is_contiguous = self
                    .state
                    .load_open_session_preserving_history(&loaded.read);
                self.reconcile_runtime_listener_with_open_session();
                if let Some(records) = loaded.agent_activity_records {
                    self.loaded_agent_activity_records = Some((loaded.read.session.id, records));
                    self.durable_agent_activity_refresh_failures = 0;
                }
                if !history_is_contiguous || page_has_more {
                    let next_offset = self
                        .state
                        .open_session
                        .as_ref()
                        .filter(|open_session| open_session.session_id() == session_id)
                        .map(OpenSessionView::loaded_turn_end);
                    if let Some(next_offset) = next_offset {
                        self.spawn_current_session_refresh_page(
                            session_id,
                            purpose,
                            Some(next_offset),
                        );
                        return;
                    }
                }
                self.state.clear_post_run_refresh_pending();
                if let CurrentSessionRefreshPurpose::StopRequestRefresh {
                    expected_active_turn,
                    in_memory_stop_accepted,
                    durable_outcome,
                    ..
                } = purpose
                {
                    let durable_stop_applied = matches!(
                        durable_outcome,
                        Some(ExactRootExecutionStopOutcome::Applied { .. })
                    );
                    let durable_target_changed = matches!(
                        durable_outcome,
                        Some(ExactRootExecutionStopOutcome::TargetChanged)
                    );
                    if durable_target_changed {
                        self.state.set_status_message(
                            "the task changed before Stop was applied; the replacement was not stopped",
                        );
                    } else if loaded_status == SessionStatus::Running
                        && matches!(
                            self.state.app_state.run_status,
                            crate::tui::state::RunStatus::Running
                        )
                        && Some(loaded_active_turn_expectation) == expected_active_turn
                        && (in_memory_stop_accepted || durable_stop_applied)
                    {
                        self.state.mark_run_stop_requested(
                            "run cancellation requested",
                            "Stop request accepted; waiting for the matching terminal event",
                        );
                    } else if matches!(
                        loaded_status,
                        SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed
                    ) {
                        self.run_lifecycle.observe_terminal_event();
                    }
                }
            }
            Err(error) => {
                self.state.clear_post_run_refresh_pending();
                if matches!(
                    purpose,
                    CurrentSessionRefreshPurpose::StopRequestRefresh { .. }
                ) {
                    self.state.set_status_message(format!(
                        "failed to confirm durable Stop request: {error}"
                    ));
                } else {
                    self.state.set_status_message(error);
                }
            }
        }
    }

    fn session_load_is_blocked_by_active_run(&self) -> bool {
        self.run_lifecycle.root_is_active()
            || matches!(
                self.state.app_state.run_status,
                crate::tui::state::RunStatus::Running
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
                if !self.replace_workspace_from_load(loaded) {
                    return;
                }
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
                    self.state
                        .set_status_message(format!("workspace set to {}", self.app.workspace.cwd));
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
                if !self.replace_workspace_from_load(loaded) {
                    return;
                }
                self.start_new_chat_with_global_access();
                self.state.set_status_message("new development chat ready");
            }
            Err(error) => {
                finish_navigation_failure(&mut self.state, request_id, error);
            }
        }
    }

    fn replace_workspace_from_load(&mut self, loaded: WorkspaceLoadResult) -> bool {
        if !self.ensure_initial_setup_owner_replacement_admission("workspace completion") {
            self.state.clear_navigation();
            return false;
        }
        let next_config_generation =
            next_config_generation(self.state.provider_config.config_generation);
        self.invalidate_session_target_requests();
        self.provider_catalog_requests.clear();
        self.docling_readiness_requests.clear();
        self.app = loaded.app.clone();
        if !self.is_quick_chat_workspace() {
            self.preferences
                .unmark_project_deleted(&self.app.workspace.root);
        }
        self.session_search_requests.clear();
        let hub_connection = self.state.hub_connection.clone();
        let mcp_publish = self.state.mcp_publish.clone();
        let device_network = self.state.device_network.clone();
        self.state = DesktopState::new(loaded.snapshot, self.app.config.clone());
        self.state.hub_connection = hub_connection;
        self.state.mcp_publish = mcp_publish;
        self.state.device_network = device_network;
        self.state.set_file_change_display_roots(
            &self.app.workspace.root,
            self.app.workspace.authority_root(),
        );
        self.state.provider_config.config_generation = next_config_generation;
        self.loaded_agent_activity_records = None;
        self.durable_agent_activity_refresh_failures = 0;
        self.state.workspace_input = self.app.workspace.cwd.to_string();
        if let Some(opacity) = self.preferences.window_opacity_percent {
            self.state.set_window_opacity_percent(opacity);
        }
        self.persist_preferences();
        true
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

    fn settle_root_finished(&mut self, run_generation: u64, result: Result<RunSummary, String>) {
        if !self.run_lifecycle.owns(run_generation) {
            return;
        }
        if self.state.app_state.current_session_id.is_none() {
            self.settle_pending_access_mode_without_session();
        }
        match result {
            Ok(summary) => {
                self.commit_pending_root_submission(run_generation);
                self.run_lifecycle.finish_root();
                self.settle_pending_permission_after_root_finish();
                self.state.finish_agent_run();
                self.state.mark_post_run_refresh_pending();
                self.state.apply_run_summary(summary);
                self.stop_session_runtime_listener();
                self.refresh_current_session_after_terminal_run();
            }
            Err(error) => {
                self.discard_pending_root_submission(run_generation);
                self.run_lifecycle.finish_root();
                self.settle_pending_permission_after_root_finish();
                self.state.finish_agent_run();
                if !matches!(
                    self.state.app_state.run_status,
                    crate::tui::state::RunStatus::Cancelled
                ) {
                    self.state.app_state.run_status = crate::tui::state::RunStatus::Failed;
                }
                if !self.state.status_code.is_terminal_interruption() {
                    self.state.set_status_message(error);
                }
                self.stop_session_runtime_listener();
                if self.state.app_state.current_session_id.is_some() {
                    self.state.mark_post_run_refresh_pending();
                    self.refresh_current_session_after_terminal_run();
                } else {
                    self.state.clear_post_run_refresh_pending();
                }
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
                    let listener_is_current =
                        self.session_runtime_listener
                            .as_ref()
                            .is_some_and(|listener| {
                                listener.generation == listener_generation
                                    && listener.target == target
                            });
                    if !listener_is_current
                        || target.workspace_root != self.app.workspace.root
                        || self.state.app_state.current_session_id != Some(target.session_id)
                        || event.session_id != target.session_id
                        || event.turn_id != target.turn_id
                    {
                        continue;
                    }
                    if let RuntimeEventMsg::TurnTerminal { terminal } = event.msg {
                        self.run_lifecycle.observe_terminal_event();
                        self.state.apply_run_summary(RunSummary::from_terminal(
                            target.session_id,
                            target.turn_id,
                            *terminal,
                        ));
                        self.state.finish_agent_run();
                        self.state.mark_post_run_refresh_pending();
                        self.stop_session_runtime_listener();
                        self.refresh_current_session_after_terminal_run();
                    } else if !self.session_projection_refresh_requests.is_pending() {
                        self.spawn_latest_live_session_refresh(target.session_id);
                    }
                }
                RuntimeMessage::RunEvent {
                    run_generation,
                    event,
                } => {
                    if !self.run_lifecycle.owns(run_generation) {
                        continue;
                    }
                    let admitted_listener_target = match &event {
                        RunEvent::UserTurnStored { session_id, turn } => {
                            Some(SessionRuntimeListenerTarget {
                                workspace_root: self.app.workspace.root.clone(),
                                session_id: *session_id,
                                turn_id: turn.turn_id,
                            })
                        }
                        _ => None,
                    };
                    if matches!(&event, RunEvent::UserTurnStored { .. }) {
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
                    if let Some(target) = admitted_listener_target
                        && self.state.app_state.current_session_id == Some(target.session_id)
                    {
                        // UserTurnStored is the durable admission receipt available on the
                        // bootstrap FIFO today. Attach its exact cursor immediately instead of
                        // waiting for a possibly reordered session snapshot refresh.
                        self.reconcile_session_runtime_listener(Some(target));
                    }
                    if let Some(message) = desktop_terminal_status_message(&event) {
                        self.state.set_status_message_preserving_code(message);
                    }
                    if let RunEvent::SessionStarted { session_id, .. } = &event {
                        self.resume_pending_access_mode_adoption(*session_id);
                    }
                    if live_event_requires_canonical_refresh(&event)
                        && live_refresh_session_id == self.state.app_state.current_session_id
                    {
                        if let Some(session_id) = live_refresh_session_id {
                            // The main transcript is a continuously merged suffix. Always refresh
                            // its latest bounded chunk so expanding older history never leaves live
                            // output pinned to an obsolete earlier offset.
                            self.spawn_latest_live_session_refresh(session_id);
                        }
                    }
                    if run_event_is_terminal(&event) {
                        self.state.mark_post_run_refresh_pending();
                    }
                    if let Some(session_id) = refresh_session_id {
                        self.spawn_snapshot_refresh_for_session(session_id);
                    }
                }
                RuntimeMessage::Finished {
                    run_generation,
                    result,
                } => {
                    self.settle_root_finished(run_generation, result);
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
                                    "loaded earlier history {start}-{end} of {total}"
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
                                "global config and current root session"
                            } else {
                                "global config"
                            };
                            let config_message = format!(
                                "{scope} access mode set to {} and remembered in {}; it applies to the next permission decision; an already displayed confirmation is unchanged",
                                access_mode_display_label(target.access_mode),
                                commit.remembered_path
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
                RuntimeMessage::WorkspaceSwitchedForNewProjectSession { request_id, result } => {
                    self.apply_new_project_workspace_switched_message(request_id, result)
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
        SessionLoadReason::UserSelection => {
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
    Ok(SessionNavigationLoadResult { workspace, loaded })
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
mod tests {
    use super::{
        DESKTOP_RUNTIME_MAILBOX_CAPACITY, DesktopRenderer, DesktopSessionEventDelivery,
        HistoryExportRequestTarget, PendingPermission, PendingPermissionResolution,
        ProviderCatalogRequestTarget, RuntimeMessage, RuntimeMessageAsyncContract,
        SessionRefreshRequestTarget, SessionRuntimeListenerTarget, SteerSubmissionTarget,
        deliver_desktop_session_runtime_events, desktop_terminal_status_message,
        fallback_workspace_after_project_delete, finish_steer_operation_if_current,
        first_restorable_project_root, import_global_config_toml_to,
        normalize_image_attachment_path, notification_session_title,
        open_transcript_rows_to_markdown, provider_catalog_probe_config,
        publish_desktop_run_finished, resolve_pending_permission, run_completion_notification_body,
        run_terminal_event_notification_body, test_desktop_control_plane,
        transcript_markdown_file_name, unique_background_request_admission_open,
    };
    use crate::cli::{EventRenderer as _, ReviewDecision};
    use crate::config::{ProviderProfile, ResolvedConfig};
    use crate::desktop::async_ops::LatestRequestTracker;
    use crate::desktop::models::DesktopTranscriptRowKind;
    use crate::desktop::models::{DesktopFileChangeRow, DesktopSnapshot, DesktopTranscriptRow};
    use crate::desktop::state::DesktopState;
    use crate::protocol::{RuntimeEvent, RuntimeEventMsg, TurnId};
    use crate::session::{ProjectId, ProjectRecord, RunEvent, RunSummary, SessionId};
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
    fn shared_session_projection_tracker_rejects_a_live_result_after_current_dispatch() {
        let target = SessionRefreshRequestTarget {
            workspace_root: "C:/workspace".into(),
            session_id: SessionId::new(),
        };
        let mut tracker = LatestRequestTracker::default();
        let stale_live_request = tracker.begin(target.clone());
        let current_request = tracker.begin(target.clone());

        assert!(tracker.finish_if_current(current_request, &target));
        assert!(!tracker.finish_if_current(stale_live_request, &target));
    }

    #[test]
    fn stale_steer_target_does_not_consume_current_operation() {
        let current_session_id = SessionId::new();
        let mut state = DesktopState::new(
            DesktopSnapshot {
                workspace_path: "C:/current".to_string(),
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
        state.app_state.current_session_id = Some(current_session_id);
        let operation_id = state.begin_steer_submission();
        let stale_target = SteerSubmissionTarget {
            operation_id,
            workspace_root: "C:/stale".into(),
            session_id: current_session_id,
            expected_active_turn: crate::session::ActiveTurnExpectation::initial_idle(),
        };

        assert!(!finish_steer_operation_if_current(
            &mut state,
            Utf8Path::new("C:/current"),
            &stale_target,
        ));
        assert!(state.steer_submission_pending());

        let current_target = SteerSubmissionTarget {
            operation_id,
            workspace_root: "C:/current".into(),
            session_id: current_session_id,
            expected_active_turn: crate::session::ActiveTurnExpectation::initial_idle(),
        };
        assert!(finish_steer_operation_if_current(
            &mut state,
            Utf8Path::new("C:/current"),
            &current_target,
        ));
        assert!(!state.steer_submission_pending());
        assert!(!finish_steer_operation_if_current(
            &mut state,
            Utf8Path::new("C:/current"),
            &current_target,
        ));
    }

    fn completed_run_summary(
        tool_call_count: usize,
        failed_tool_count: usize,
        change_count: usize,
    ) -> RunSummary {
        RunSummary::from_terminal(
            crate::session::SessionId::new(),
            crate::protocol::TurnId::new(),
            crate::session::DurableTurnTerminal {
                outcome: crate::protocol::TurnTerminalOutcome::Completed,
                final_response_id: None,
                tool_call_count,
                failed_tool_count,
                change_count,
                metrics: Default::default(),
            },
        )
    }

    fn interrupted_turn_event(
        session_id: crate::session::SessionId,
        cause: crate::protocol::TurnInterruptionCause,
    ) -> RunEvent {
        RunEvent::TurnTerminal {
            session_id,
            terminal: Box::new(crate::session::DurableTurnTerminal {
                outcome: crate::protocol::TurnTerminalOutcome::Interrupted { cause },
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        }
    }

    #[tokio::test]
    async fn fixed_workspace_load_does_not_rediscover_parent_git_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outer = Utf8PathBuf::from_path_buf(temp.path().join("outer")).expect("utf8 outer");
        let quick_chat_root = outer.join("data/quick-chat-workspace");
        std::fs::create_dir_all(outer.join(".git")).expect("parent git marker");
        std::fs::create_dir_all(&quick_chat_root).expect("quick chat root");

        let data_dir =
            Utf8PathBuf::from_path_buf(temp.path().join("storage")).expect("utf8 storage");
        let paths = crate::storage::StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        };
        let sqlite = crate::storage::SqliteStore::open(&paths).expect("sqlite");
        sqlite.migrate().expect("migrate");
        let store = crate::storage::StoreBundle::new(sqlite);

        let app = crate::app::AppBootstrap::rebuild_for_directory_as_workspace_root_with_config(
            &quick_chat_root,
            store,
            ResolvedConfig::default(),
        )
        .await
        .expect("fixed workspace load");

        assert_eq!(app.workspace.root, quick_chat_root);
        assert_eq!(app.workspace.cwd, quick_chat_root);
        assert_eq!(app.workspace.vcs, crate::workspace::VcsKind::None);
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
            profile: ProviderProfile::LmStudio,
            api_key_env: None,
            config_generation: 1,
            selected_model_id: "selected-model".to_string(),
        };
        let provider_request_id = LatestRequestTracker::default().begin(provider_target.clone());
        let history_target = HistoryExportRequestTarget {
            workspace_authority_root: Utf8PathBuf::from("C:/workspace"),
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
    fn full_desktop_mailbox_keeps_the_canonical_terminal_retryable() {
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let target = SessionRuntimeListenerTarget {
            workspace_root: Utf8PathBuf::from("C:/workspace"),
            session_id,
            turn_id,
        };
        let terminal_event = RuntimeEvent {
            id: crate::protocol::RuntimeEventId::new(),
            session_id,
            turn_id,
            sequence_no: 9,
            created_at_ms: 1,
            msg: RuntimeEventMsg::TurnTerminal {
                terminal: Box::new(crate::session::DurableTurnTerminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Completed,
                    final_response_id: None,
                    tool_call_count: 1,
                    failed_tool_count: 0,
                    change_count: 0,
                    metrics: Default::default(),
                }),
            },
        };
        let (tx, rx) = mpsc::sync_channel(1);
        tx.try_send(RuntimeMessage::CanonicalSessionEvent {
            listener_generation: 1,
            target: target.clone(),
            event: RuntimeEvent {
                id: crate::protocol::RuntimeEventId::new(),
                session_id,
                turn_id,
                sequence_no: 8,
                created_at_ms: 1,
                msg: RuntimeEventMsg::Warning {
                    message: "occupy mailbox".to_string(),
                },
            },
        })
        .expect("fill mailbox");

        assert_eq!(
            deliver_desktop_session_runtime_events(
                &tx,
                1,
                &target,
                std::slice::from_ref(&terminal_event),
            ),
            DesktopSessionEventDelivery::MailboxFull
        );
        let _ = rx.recv().expect("release mailbox slot");
        assert_eq!(
            deliver_desktop_session_runtime_events(
                &tx,
                1,
                &target,
                std::slice::from_ref(&terminal_event),
            ),
            DesktopSessionEventDelivery::Terminal
        );
        assert!(matches!(
            rx.recv().expect("retried terminal"),
            RuntimeMessage::CanonicalSessionEvent { event, .. }
                if event.id == terminal_event.id
        ));
    }

    #[test]
    fn stale_permission_answer_id_is_rejected_without_consuming_current_request() {
        let (response_tx, response_rx) = mpsc::channel();
        let run_control = crate::runtime::RunControl::new();
        let run_control_observer = run_control.clone();
        let mut pending = Some(PendingPermission {
            confirmation_id: 42,
            request: crate::tool::PermissionRequest {
                access: crate::workspace::AccessKind::Read,
                summary: "inspect the workspace".to_string(),
                details: Vec::new(),
                targets: Vec::new(),
                outside_workspace: false,
                risks: Vec::new(),
                agent_path: None,
                agent_task_name: None,
            },
            responder: response_tx,
            run_control,
        });

        assert_eq!(
            resolve_pending_permission(&mut pending, 41, ReviewDecision::Approved),
            PendingPermissionResolution::NotCurrent
        );
        assert_eq!(
            pending.as_ref().map(|pending| pending.confirmation_id),
            Some(42)
        );
        assert!(matches!(
            response_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert_eq!(run_control_observer.cause(), None);

        assert_eq!(
            resolve_pending_permission(&mut pending, 42, ReviewDecision::Abort),
            PendingPermissionResolution::Resolved
        );
        assert!(pending.is_none());
        assert_eq!(response_rx.try_recv(), Ok(ReviewDecision::Abort));
        assert_eq!(run_control_observer.cause(), None);
    }

    #[test]
    fn terminal_status_message_uses_the_typed_interruption_cause() {
        let session_id = crate::session::SessionId::new();
        let approval_abort = interrupted_turn_event(
            session_id,
            crate::protocol::TurnInterruptionCause::ApprovalAborted,
        );
        assert_eq!(
            desktop_terminal_status_message(&approval_abort),
            Some(crate::tui::state::interruption_status_message(
                crate::protocol::TurnInterruptionCause::ApprovalAborted
            ))
        );

        let explicit_stop =
            interrupted_turn_event(session_id, crate::protocol::TurnInterruptionCause::UserStop);
        assert_eq!(
            desktop_terminal_status_message(&explicit_stop),
            Some(crate::tui::state::interruption_status_message(
                crate::protocol::TurnInterruptionCause::UserStop
            ))
        );
    }

    #[test]
    fn provider_catalog_probe_uses_the_complete_connection_profile_input() {
        let mut config = ResolvedConfig::default();
        config.model.base_url = "http://old-provider:1234".to_string();
        config.model.provider_profile = ProviderProfile::OpenAiResponses;
        config.model.api_key_env = Some("OLD_PROVIDER_KEY".to_string());
        config.model.extra_headers.insert(
            "Authorization".to_string(),
            "Bearer must-not-cross-provider-targets".to_string(),
        );
        config.model.extra_body_json = Some(serde_json::json!({
            "api_key": "must-not-cross-provider-targets"
        }));

        let probe_config = provider_catalog_probe_config(
            config,
            "http://127.0.0.1:8110".to_string(),
            ProviderProfile::OpenAiCompatible,
            Some("OMLX_API_KEY".to_string()),
        );

        assert_eq!(probe_config.model.base_url, "http://127.0.0.1:8110");
        assert_eq!(
            probe_config.model.provider_profile,
            ProviderProfile::OpenAiCompatible
        );
        assert_eq!(
            probe_config.model.api_key_env.as_deref(),
            Some("OMLX_API_KEY")
        );
        assert!(probe_config.model.extra_headers.is_empty());
        assert_eq!(probe_config.model.extra_body_json, None);
    }

    #[test]
    fn provider_catalog_probe_preserves_headers_but_not_legacy_generation_state_for_same_target() {
        let mut config = ResolvedConfig::default();
        config.model.base_url = "http://127.0.0.1:8110/v1/".to_string();
        config.model.provider_profile = ProviderProfile::OpenAiCompatible;
        config.model.extra_headers.insert(
            "X-Provider-Route".to_string(),
            "same-target-route".to_string(),
        );
        config.model.extra_body_json = Some(serde_json::json!({"num_ctx": 8192}));

        let same_target = provider_catalog_probe_config(
            config.clone(),
            "http://127.0.0.1:8110".to_string(),
            ProviderProfile::OpenAiCompatible,
            None,
        );
        assert_eq!(same_target.model.extra_headers, config.model.extra_headers);
        assert_eq!(same_target.model.extra_body_json, None);

        let changed_profile = provider_catalog_probe_config(
            config,
            "http://127.0.0.1:8110".to_string(),
            ProviderProfile::OpenAiResponses,
            None,
        );
        assert!(changed_profile.model.extra_headers.is_empty());
        assert_eq!(changed_profile.model.extra_body_json, None);
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
    fn image_attachment_normalization_preserves_nonexistent_path_diagnostic() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 path");

        let error = normalize_image_attachment_path(&workspace, "missing-image.png")
            .expect_err("a missing image must remain rejected");

        assert!(error.contains("image path is not accessible"));
        assert!(!error.contains("the image path was not attached"));
    }

    #[test]
    fn image_attachment_normalization_preserves_unsupported_extension_diagnostic() {
        let temp = tempfile::tempdir().expect("tempdir");
        let unsupported = temp.path().join("not-an-image.txt");
        std::fs::write(&unsupported, b"not an image").expect("unsupported fixture");
        let workspace = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 path");
        let unsupported = Utf8PathBuf::from_path_buf(unsupported).expect("utf8 fixture");

        let error = normalize_image_attachment_path(&workspace, unsupported.as_str())
            .expect_err("an unsupported extension must remain rejected");

        assert_eq!(error, "unsupported image file extension: txt");
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
                stable_history_identity: None,
                step: "01".to_string(),
                title: "Prompt".to_string(),
                body: "Older request.".to_string(),
                file_changes: Vec::new(),
            },
            DesktopTranscriptRow {
                row_kind: DesktopTranscriptRowKind::Assistant,
                stable_history_identity: None,
                step: "02".to_string(),
                title: "Previous response".to_string(),
                body: "Earlier answer.".to_string(),
                file_changes: Vec::new(),
            },
            DesktopTranscriptRow {
                row_kind: DesktopTranscriptRowKind::User,
                stable_history_identity: None,
                step: "03".to_string(),
                title: "Prompt".to_string(),
                body: "Create a report.".to_string(),
                file_changes: Vec::new(),
            },
            DesktopTranscriptRow {
                row_kind: DesktopTranscriptRowKind::Assistant,
                stable_history_identity: None,
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
            "fixture-model",
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
                stable_history_identity: None,
                step: "01".to_string(),
                title: "Prompt".to_string(),
                body: "Create files.".to_string(),
                file_changes: Vec::new(),
            },
            DesktopTranscriptRow {
                row_kind: DesktopTranscriptRowKind::Assistant,
                stable_history_identity: None,
                step: "02".to_string(),
                title: "Response".to_string(),
                body: "Now run this:\n<tool_call>\n<function=shell>\n</tool_call>".to_string(),
                file_changes: Vec::new(),
            },
            DesktopTranscriptRow {
                row_kind: DesktopTranscriptRowKind::Diff,
                stable_history_identity: None,
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
            "fixture-model",
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
                stable_history_identity: None,
                step: "01".to_string(),
                title: "Prompt".to_string(),
                body: "Update the implementation.".to_string(),
                file_changes: Vec::new(),
            },
            DesktopTranscriptRow {
                row_kind: DesktopTranscriptRowKind::Assistant,
                stable_history_identity: None,
                step: "02".to_string(),
                title: "Response".to_string(),
                body: "テストの期待値を修正します。".to_string(),
                file_changes: Vec::new(),
            },
            DesktopTranscriptRow {
                row_kind: DesktopTranscriptRowKind::WorkSummaryCancelled,
                stable_history_identity: None,
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
            "fixture-model",
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
    fn open_transcript_markdown_exports_a_prior_assistant_before_a_cancelled_later_turn_once() {
        let session_id = crate::session::SessionId::new();
        let rows = vec![
            DesktopTranscriptRow {
                row_kind: DesktopTranscriptRowKind::User,
                stable_history_identity: None,
                step: "01".to_string(),
                title: "Prompt".to_string(),
                body: "first request".to_string(),
                file_changes: Vec::new(),
            },
            DesktopTranscriptRow {
                row_kind: DesktopTranscriptRowKind::WorkSummaryCompleted,
                stable_history_identity: None,
                step: "02".to_string(),
                title: "作業履歴 / 作業サマリ".to_string(),
                body: "### 作業サマリ\n- 結果: first turn completed".to_string(),
                file_changes: Vec::new(),
            },
            DesktopTranscriptRow {
                row_kind: DesktopTranscriptRowKind::Assistant,
                stable_history_identity: None,
                step: "03".to_string(),
                title: "Response".to_string(),
                body: "FIRST_OK".to_string(),
                file_changes: Vec::new(),
            },
            DesktopTranscriptRow {
                row_kind: DesktopTranscriptRowKind::User,
                stable_history_identity: None,
                step: "04".to_string(),
                title: "Prompt".to_string(),
                body: "second request".to_string(),
                file_changes: Vec::new(),
            },
            DesktopTranscriptRow {
                row_kind: DesktopTranscriptRowKind::WorkSummaryCancelled,
                stable_history_identity: None,
                step: "05".to_string(),
                title: "作業履歴 / 作業サマリ".to_string(),
                body: "### 作業サマリ\n- 結果: second turn cancelled".to_string(),
                file_changes: Vec::new(),
            },
        ];

        let markdown = open_transcript_rows_to_markdown(
            "Two-turn session",
            &Utf8PathBuf::from("C:/workspace"),
            session_id,
            "http://127.0.0.1:1234",
            "fixture-model",
            &rows,
            &[],
        );

        let first_user = markdown.find("> first request").unwrap();
        let assistant = markdown.find("FIRST_OK").unwrap();
        let second_user = markdown.find("> second request").unwrap();
        let cancelled = markdown
            .find("停止しました: second turn cancelled")
            .unwrap();
        assert!(first_user < assistant && assistant < second_user && second_user < cancelled);
        assert_eq!(markdown.matches("FIRST_OK").count(), 1);
    }

    #[test]
    fn completion_notification_body_summarizes_terminal_run() {
        let summary = completed_run_summary(3, 1, 2);

        let body = run_completion_notification_body("  vision GUI  ", &summary);

        assert!(body.contains("vision GUI が完了しました。"));
        assert!(body.contains("変更: 2件"));
        assert!(body.contains("ツール: 3件 / 失敗 1件"));
        assert_eq!(notification_session_title(""), "タスク");
    }

    #[test]
    fn desktop_renderer_defers_state_completion_until_worker_settlement() {
        let (tx, rx) = mpsc::sync_channel(DESKTOP_RUNTIME_MAILBOX_CAPACITY);
        let (control, control_rx) = test_desktop_control_plane();
        let mut renderer = DesktopRenderer {
            runtime_tx: tx,
            bootstrap_control: control.clone(),
            run_generation: 12,
            notification_title: "test".to_string(),
            notified_terminal: false,
        };
        let summary = completed_run_summary(0, 0, 0);

        renderer.finish(&summary).expect("renderer finish");
        assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));

        publish_desktop_run_finished(&control, 12, Ok(summary.clone()));
        assert!(matches!(
            control_rx.try_recv().expect("worker settlement"),
            RuntimeMessage::Finished {
                run_generation: 12,
                result: Ok(received),
            } if received.session_id() == summary.session_id()
        ));
    }

    #[test]
    fn unbounded_control_plane_preserves_multiple_completions_in_order() {
        let (control, control_rx) = test_desktop_control_plane();
        let owner_session_id = SessionId::new();
        control
            .send(RuntimeMessage::SideChatFinished {
                owner_session_id,
                side_chat_id: "first".to_string(),
                run_generation: 1001,
                result: Err("first settlement".to_string()),
            })
            .expect("first settlement");
        control
            .send(RuntimeMessage::SideChatFinished {
                owner_session_id,
                side_chat_id: "second".to_string(),
                run_generation: 1002,
                result: Err("second settlement".to_string()),
            })
            .expect("second settlement");

        assert!(matches!(
            control_rx.recv().expect("first completion"),
            RuntimeMessage::SideChatFinished {
                run_generation: 1001,
                ..
            }
        ));
        assert!(matches!(
            control_rx.recv().expect("second completion"),
            RuntimeMessage::SideChatFinished {
                run_generation: 1002,
                ..
            }
        ));
        assert!(matches!(
            control_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn terminal_event_notification_body_uses_terminal_outcome() {
        let body = run_terminal_event_notification_body(
            "vision GUI",
            &interrupted_turn_event(
                crate::session::SessionId::new(),
                crate::protocol::TurnInterruptionCause::UserStop,
            ),
        )
        .expect("terminal event should produce a notification");

        assert_eq!(body, "vision GUI を停止しました: run stopped by user");
    }

    #[test]
    fn config_import_accepts_toml_files_regardless_of_base_name() {
        let temp = tempfile::tempdir().expect("tempdir");
        let text = "[model]\nmodel = \"renamed-config-model\"\n";
        let target =
            Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).expect("utf8 target");

        for file_name in ["config(1).toml", "config_202608.toml", "CONFIG_BACKUP.TOML"] {
            let path = Utf8PathBuf::from_path_buf(temp.path().join(file_name)).expect("utf8 path");
            std::fs::write(path.as_std_path(), text).expect("config fixture");
            std::fs::write(target.as_std_path(), "sentinel").expect("target fixture");

            import_global_config_toml_to(&path, &target)
                .expect("renamed TOML config should be imported");
            assert_eq!(std::fs::read_to_string(target.as_std_path()).unwrap(), text);
        }
    }

    #[test]
    fn config_import_validates_canonical_and_legacy_response_timeout_together() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source =
            Utf8PathBuf::from_path_buf(temp.path().join("config(1).toml")).expect("utf8 source");
        let target =
            Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).expect("utf8 target");
        let legacy = "[model]\nstream_idle_timeout_ms = 3600000\n";
        std::fs::write(source.as_std_path(), legacy).expect("legacy import fixture");

        import_global_config_toml_to(&source, &target)
            .expect("unambiguous legacy timeout should remain importable");
        assert_eq!(
            std::fs::read_to_string(target.as_std_path()).expect("imported config"),
            legacy
        );

        let sentinel = "[model]\nrequest_timeout_ms = 1800000\n";
        std::fs::write(target.as_std_path(), sentinel).expect("target sentinel");
        std::fs::write(
            source.as_std_path(),
            "[model]\nrequest_timeout_ms = 3600000\nstream_idle_timeout_ms = 1800000\n",
        )
        .expect("ambiguous import fixture");

        let error = import_global_config_toml_to(&source, &target)
            .expect_err("mismatched timeout aliases must not replace the active config");
        assert!(error.contains("model.request_timeout_ms"));
        assert!(error.contains("model.stream_idle_timeout_ms"));
        assert_eq!(
            std::fs::read_to_string(target.as_std_path()).expect("unchanged target"),
            sentinel
        );
    }

    #[test]
    fn config_import_rejects_paths_without_a_toml_extension() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target =
            Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).expect("utf8 target");
        let sentinel = "[model]\nmodel = \"existing\"\n";

        for file_name in ["config_202608.txt", "config_backup", "config.toml.bak"] {
            let path = Utf8PathBuf::from_path_buf(temp.path().join(file_name)).expect("utf8 path");
            std::fs::write(path.as_std_path(), "[model]\nmodel = \"valid\"\n")
                .expect("config fixture");
            std::fs::write(target.as_std_path(), sentinel).expect("target fixture");

            assert_eq!(
                import_global_config_toml_to(&path, &target),
                Err("select a .toml file".to_string())
            );
            assert_eq!(
                std::fs::read_to_string(target.as_std_path()).unwrap(),
                sentinel
            );
        }
    }

    #[test]
    fn config_import_rejects_malformed_toml() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path =
            Utf8PathBuf::from_path_buf(temp.path().join("config_backup.toml")).expect("utf8 path");
        let target =
            Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).expect("utf8 target");
        let sentinel = "[model]\nmodel = \"existing\"\n";
        let secret = "GLOBAL_IMPORT_PARSE_SECRET_SENTINEL";
        std::fs::write(
            path.as_std_path(),
            format!("[model]\nextra_headers = {{ Authorization = \"{secret}\", broken = }}\n"),
        )
        .expect("config fixture");
        std::fs::write(target.as_std_path(), sentinel).expect("target fixture");

        let error = import_global_config_toml_to(&path, &target)
            .expect_err("malformed TOML must fail closed");
        assert_eq!(
            error,
            "the selected TOML config is invalid or does not match the current config schema"
        );
        assert!(!error.contains(secret));
        assert!(!error.contains("Authorization"));
        assert_eq!(
            std::fs::read_to_string(target.as_std_path()).unwrap(),
            sentinel
        );
    }

    #[test]
    fn config_import_rejects_semantically_invalid_config_before_replacing_target() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source =
            Utf8PathBuf::from_path_buf(temp.path().join("config(1).toml")).expect("utf8 source");
        let target =
            Utf8PathBuf::from_path_buf(temp.path().join("config.toml")).expect("utf8 target");
        let sentinel = "[model]\nmodel = \"existing\"\n";
        std::fs::write(
            source.as_std_path(),
            "[workspace]\nprotected_paths = [\"relative/path\"]\n",
        )
        .expect("semantic-invalid config fixture");
        std::fs::write(target.as_std_path(), sentinel).expect("target fixture");

        let error = import_global_config_toml_to(&source, &target)
            .expect_err("semantic-invalid config must not be imported");

        assert!(error.contains("workspace.protected_paths"));
        assert!(error.contains("absolute path"));
        assert_eq!(
            std::fs::read_to_string(target.as_std_path()).unwrap(),
            sentinel
        );
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

struct DesktopRenderer {
    runtime_tx: mpsc::SyncSender<RuntimeMessage>,
    bootstrap_control: DesktopControlPlaneSender,
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

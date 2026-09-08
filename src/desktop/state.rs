use crate::app::session_title::is_placeholder_session_title;
use crate::protocol::{HistoryItem, HistoryItemPayload, TurnInterruptionCause, TurnItemPayload};
use crate::session::{
    ActiveTurnExpectation, CanonicalSessionRead, ProjectId, PromptDispatchPart, SessionId,
    SessionStatus,
};
use crate::tui::state::{AppState, RunProgressPhase, RunStatus};

use super::async_ops::{
    DesktopAsyncOperationId, DesktopAsyncOperationKind, DesktopAsyncOperationRegistry,
};
use super::composer_state::DesktopComposerState;
use super::models::{DesktopSessionDetail, DesktopSnapshot};
use super::navigation::{DesktopNavigationState, NavigationRequestId, NavigationTarget};
use super::open_session::OpenSessionView;
use super::provider_config_state::{DesktopProviderConfigState, DesktopProviderStatusKind};
use super::query::build_session_detail_from_app_state_with_session;
use super::startup::DesktopStartupState;
use super::view_state::DesktopViewState;
use crate::config::ProviderProfile;
use crate::config::ResolvedConfig;
use crate::docling::DoclingReadinessResult;
use crate::llm::{ProviderModelInfo, ProviderModelLoadState, normalize_provider_base_url};
use tokio_util::sync::CancellationToken;

pub const MIN_WINDOW_OPACITY_PERCENT: i32 = 50;
pub const MAX_WINDOW_OPACITY_PERCENT: i32 = 100;
pub const DEFAULT_WINDOW_OPACITY_PERCENT: i32 = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopStatusCode {
    Plain,
    ProviderTransport,
    ModelUnavailable,
    ImageUnsupported,
    ImageAttachmentInvalid,
    PermissionPolicyDenied,
    ConfigImportFailed,
    ApprovalAborted,
    UserStopped,
    AgentInterrupted,
    TreeStopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopDoclingReadinessStatus {
    Idle,
    Checking,
    Ready,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopDoclingReadinessState {
    pub status: DesktopDoclingReadinessStatus,
    pub endpoint: String,
    pub http_status: Option<u16>,
    pub message: String,
}

impl Default for DesktopDoclingReadinessState {
    fn default() -> Self {
        Self {
            status: DesktopDoclingReadinessStatus::Idle,
            endpoint: String::new(),
            http_status: None,
            message: "Docling readiness has not been checked.".to_string(),
        }
    }
}

impl DesktopStatusCode {
    pub fn from_interruption(cause: TurnInterruptionCause) -> Self {
        match cause {
            TurnInterruptionCause::ApprovalAborted => Self::ApprovalAborted,
            TurnInterruptionCause::UserStop => Self::UserStopped,
            TurnInterruptionCause::AgentInterrupted => Self::AgentInterrupted,
            TurnInterruptionCause::TreeStopped => Self::TreeStopped,
        }
    }

    pub fn is_terminal_interruption(self) -> bool {
        matches!(
            self,
            Self::ApprovalAborted | Self::UserStopped | Self::AgentInterrupted | Self::TreeStopped
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopOverlay {
    None,
    InitialSetup,
    FileMenu,
    EditMenu,
    ViewMenu,
    HelpMenu,
    ProjectMenu,
    ConfigEditor,
    HubConnection,
    McpPublish,
    McpHistory,
    SessionSettings,
    ProviderEditor,
    WorkspacePicker,
    PromptReview,
    CommandPalette,
    KeyboardShortcuts,
    About,
}

#[derive(Debug, Clone)]
struct PromptReviewDesktopOwner {
    request_id: u64,
    expected_active_turn: ActiveTurnExpectation,
    cancellation: Option<CancellationToken>,
}

#[derive(Debug, Clone)]
pub struct DesktopState {
    pub snapshot: DesktopSnapshot,
    pub app_state: AppState,
    pub open_session: Option<OpenSessionView>,
    pub composer: DesktopComposerState,
    pub workspace_input: String,
    pub provider_config: DesktopProviderConfigState,
    pub docling_readiness: DesktopDoclingReadinessState,
    pub navigation: DesktopNavigationState,
    pub view: DesktopViewState,
    pub startup: DesktopStartupState,
    pub status_code: DesktopStatusCode,
    pub(crate) hub_connection: Option<crate::hub::HubConnection>,
    pub(crate) mcp_publish: Option<crate::mcp_publish::PublishService>,
    pub(crate) device_network: Option<crate::device_network::DeviceNetworkService>,
    global_config: ResolvedConfig,
    file_change_storage_root: Option<camino::Utf8PathBuf>,
    file_change_display_root: Option<camino::Utf8PathBuf>,
    prompt_review_owner: Option<PromptReviewDesktopOwner>,
}

fn root_session_settings_match(
    current: &crate::session::SessionRecord,
    persisted: &crate::session::SessionRecord,
) -> bool {
    current.id == persisted.id
        && current.project_id == persisted.project_id
        && current.cwd == persisted.cwd
        && current.model == persisted.model
        && current.base_url == persisted.base_url
        && current.access_mode == persisted.access_mode
        && current.model_parameters.context_window == persisted.model_parameters.context_window
        && current.session_settings_revision == persisted.session_settings_revision
}

fn merge_root_session_settings(
    current: &crate::session::SessionRecord,
    persisted: &crate::session::SessionRecord,
) -> Option<crate::session::SessionRecord> {
    if current.id != persisted.id
        || current.project_id != persisted.project_id
        || current.cwd != persisted.cwd
        || current.session_settings_revision > persisted.session_settings_revision
    {
        return None;
    }
    let mut merged = current.clone();
    merged.model.clone_from(&persisted.model);
    merged.base_url.clone_from(&persisted.base_url);
    merged.access_mode = persisted.access_mode;
    merged
        .model_parameters
        .clone_from(&persisted.model_parameters);
    merged.session_settings_revision = persisted.session_settings_revision;
    merged.updated_at_ms = merged.updated_at_ms.max(persisted.updated_at_ms);
    Some(merged)
}

impl DesktopState {
    pub fn new(snapshot: DesktopSnapshot, effective_config: ResolvedConfig) -> Self {
        let composer = DesktopComposerState::for_owner(snapshot.workspace_path.clone(), None);
        let global_config = effective_config.clone();
        Self {
            snapshot,
            hub_connection: None,
            mcp_publish: None,
            device_network: None,
            app_state: AppState::default(),
            open_session: None,
            composer,
            workspace_input: String::new(),
            provider_config: DesktopProviderConfigState::new(effective_config),
            docling_readiness: DesktopDoclingReadinessState::default(),
            navigation: DesktopNavigationState::default(),
            view: DesktopViewState::default(),
            startup: DesktopStartupState::ready(),
            status_code: DesktopStatusCode::Plain,
            global_config,
            file_change_storage_root: None,
            file_change_display_root: None,
            prompt_review_owner: None,
        }
        .with_provider_fields()
    }

    pub fn set_file_change_display_roots(
        &mut self,
        storage_root: &camino::Utf8Path,
        display_root: &camino::Utf8Path,
    ) {
        self.file_change_storage_root = Some(storage_root.to_path_buf());
        self.file_change_display_root = Some(display_root.to_path_buf());
        self.app_state
            .set_file_change_display_roots(storage_root, display_root);
    }

    fn reset_app_state_preserving_file_change_display_roots(&mut self) {
        let mut app_state = AppState::default();
        if let (Some(storage_root), Some(display_root)) = (
            self.file_change_storage_root.as_deref(),
            self.file_change_display_root.as_deref(),
        ) {
            app_state.set_file_change_display_roots(storage_root, display_root);
        }
        self.app_state = app_state;
    }

    pub fn begin_startup(
        &mut self,
        global_config_existed_at_launch: bool,
        global_config_path: Option<camino::Utf8PathBuf>,
        workspace_root: &camino::Utf8Path,
    ) {
        self.startup = DesktopStartupState::begin(
            global_config_existed_at_launch,
            global_config_path,
            workspace_root,
            &self.provider_config.effective_config,
        );
        self.apply_startup_overlay();
    }

    pub fn refresh_startup_config_status(&mut self) {
        self.startup
            .refresh_config(&self.provider_config.effective_config);
        self.apply_startup_overlay();
    }

    pub fn replace_snapshot(&mut self, mut snapshot: DesktopSnapshot) {
        let preferred = [
            self.selected_session_id(),
            self.app_state.current_session_id,
            snapshot.selected_session_id(),
        ];
        for session_id in preferred.into_iter().flatten() {
            if let Some(index) = snapshot
                .session_rows
                .iter()
                .position(|row| row.session_id == session_id)
            {
                snapshot.selected_session_index = index;
                break;
            }
        }
        self.snapshot = snapshot;
        self.clamp_artifact_selection();
    }

    pub fn replace_snapshot_preserving_current_owner(&mut self, mut snapshot: DesktopSnapshot) {
        if let Some(current_session_id) = self.app_state.current_session_id
            && !snapshot
                .session_rows
                .iter()
                .any(|row| row.session_id == current_session_id)
            && let Some(current_row) = self
                .snapshot
                .session_rows
                .iter()
                .find(|row| row.session_id == current_session_id)
                .cloned()
        {
            snapshot.session_rows.insert(0, current_row);
            if let Some(detail) = self.snapshot.detail_for(current_session_id).cloned() {
                snapshot.replace_detail(detail);
            }
        }
        self.replace_snapshot(snapshot);
    }

    pub fn select_session(&mut self, index: usize) {
        if index < self.snapshot.session_rows.len() {
            self.snapshot.selected_session_index = index;
            self.view.artifact_selected_index = 0;
        }
    }

    pub fn select_project(&mut self, index: usize) {
        if index < self.snapshot.project_rows.len() {
            let changed = self.snapshot.selected_project_index != index;
            self.snapshot.selected_project_index = index;
            self.snapshot.selected_session_index = 0;
            self.view.artifact_selected_index = 0;
            if changed {
                self.snapshot.session_rows.clear();
                self.snapshot.session_details.clear();
                self.reset_app_state_preserving_file_change_display_roots();
                self.open_session = None;
            }
        }
    }

    pub fn selected_project_index(&self) -> i32 {
        if self.snapshot.project_rows.is_empty()
            || self.snapshot.selected_project_index >= self.snapshot.project_rows.len()
        {
            -1
        } else {
            self.snapshot.selected_project_index as i32
        }
    }

    pub fn selected_project_path(&self) -> Option<&str> {
        self.snapshot.selected_project_path()
    }

    pub fn selected_project_id(&self) -> Option<ProjectId> {
        self.snapshot.selected_project_id()
    }

    pub fn selected_index(&self) -> i32 {
        if self.snapshot.session_rows.is_empty()
            || self.snapshot.selected_session_index >= self.snapshot.session_rows.len()
        {
            -1
        } else {
            self.snapshot.selected_session_index as i32
        }
    }

    pub fn selected_session_id(&self) -> Option<SessionId> {
        self.snapshot.selected_session_id()
    }

    pub fn rebind_composer_owner(&mut self, session_id: Option<SessionId>) -> bool {
        let changed = self
            .composer
            .rebind_owner(&self.snapshot.workspace_path, session_id);
        if changed && self.app_state.prompt_review.is_some() {
            self.cancel_prompt_review();
        }
        changed
    }

    pub fn adopt_composer_owner(&mut self, session_id: Option<SessionId>) {
        self.composer
            .adopt_owner(&self.snapshot.workspace_path, session_id);
    }

    pub fn bind_composer_to_loaded_session(&mut self, session_id: SessionId) {
        let adopts_created_session = self.app_state.current_session_id == Some(session_id)
            && self
                .composer
                .is_owned_by(&self.snapshot.workspace_path, None);
        if adopts_created_session {
            self.adopt_composer_owner(Some(session_id));
        } else {
            self.rebind_composer_owner(Some(session_id));
        }
    }

    pub fn restore_selected_session_to_current_owner(&mut self) {
        let Some(current_session_id) = self.app_state.current_session_id else {
            self.snapshot.selected_session_index = self.snapshot.session_rows.len();
            return;
        };
        self.snapshot.selected_session_index = self
            .snapshot
            .session_rows
            .iter()
            .position(|row| row.session_id == current_session_id)
            .unwrap_or(self.snapshot.session_rows.len());
    }

    pub fn begin_workspace_load(
        &mut self,
        path: camino::Utf8PathBuf,
        selected_session_id: Option<SessionId>,
    ) -> NavigationRequestId {
        self.clear_navigation_operations();
        let id = self
            .navigation
            .begin_workspace(path, selected_session_id, false);
        self.view
            .async_operations
            .begin_unique(DesktopAsyncOperationKind::WorkspaceLoad);
        id
    }

    pub fn begin_new_project_session_workspace_load(
        &mut self,
        path: camino::Utf8PathBuf,
    ) -> NavigationRequestId {
        self.clear_navigation_operations();
        let id = self.navigation.begin_workspace(path, None, true);
        self.view
            .async_operations
            .begin_unique(DesktopAsyncOperationKind::WorkspaceLoad);
        id
    }

    pub fn begin_session_load(&mut self, session_id: SessionId) -> NavigationRequestId {
        self.clear_navigation_operations();
        let id = self.navigation.begin_session(session_id);
        self.view
            .async_operations
            .begin_unique(DesktopAsyncOperationKind::SessionLoad);
        id
    }

    pub fn is_current_navigation(&self, request_id: NavigationRequestId) -> bool {
        self.navigation.is_current(request_id)
    }

    pub fn is_current_session_navigation(
        &self,
        request_id: NavigationRequestId,
        session_id: SessionId,
    ) -> bool {
        self.navigation.is_current_session(request_id, session_id)
    }

    pub fn finish_navigation(&mut self, request_id: NavigationRequestId) -> bool {
        let target = self
            .navigation
            .active()
            .filter(|request| request.id == request_id)
            .map(|request| request.target.clone());
        let finished = self.navigation.finish(request_id);
        if finished {
            if let Some(target) = target {
                match target {
                    NavigationTarget::Workspace { .. } => {
                        self.view
                            .async_operations
                            .finish_kind(DesktopAsyncOperationKind::WorkspaceLoad);
                    }
                    NavigationTarget::Session { .. } => {
                        self.view
                            .async_operations
                            .finish_kind(DesktopAsyncOperationKind::SessionLoad);
                    }
                }
            }
        }
        finished
    }

    pub fn clear_navigation(&mut self) {
        self.navigation.clear();
        self.clear_navigation_operations();
    }

    pub fn navigation_loading(&self) -> bool {
        self.view
            .async_operations
            .is_pending(DesktopAsyncOperationKind::WorkspaceLoad)
            || self
                .view
                .async_operations
                .is_pending(DesktopAsyncOperationKind::SessionLoad)
    }

    pub fn can_begin_navigation(&self) -> bool {
        !self.is_busy() && !self.background_mutation_pending() && !self.navigation_loading()
    }

    pub fn can_begin_turn_page_load(&self) -> bool {
        !self.background_mutation_pending()
            && !self.navigation_loading()
            && !self.turn_page_load_pending()
            && self.selected_session_id().is_some()
            && self.open_session.as_ref().is_some_and(|open_session| {
                Some(open_session.session_id()) == self.selected_session_id()
            })
    }

    pub fn begin_snapshot_refresh(&mut self) {
        self.view
            .async_operations
            .begin_unique(DesktopAsyncOperationKind::SnapshotRefresh);
    }

    pub fn snapshot_refresh_pending(&self) -> bool {
        self.view
            .async_operations
            .is_pending(DesktopAsyncOperationKind::SnapshotRefresh)
    }

    pub fn finish_snapshot_refresh(&mut self) {
        self.view
            .async_operations
            .finish_kind(DesktopAsyncOperationKind::SnapshotRefresh);
    }

    pub fn begin_turn_page_load(&mut self) {
        self.view
            .async_operations
            .begin_unique(DesktopAsyncOperationKind::TurnPageLoad);
    }

    pub fn finish_turn_page_load(&mut self) {
        self.view
            .async_operations
            .finish_kind(DesktopAsyncOperationKind::TurnPageLoad);
    }

    pub fn mark_post_run_refresh_pending(&mut self) {
        self.view
            .async_operations
            .begin_unique(DesktopAsyncOperationKind::TerminalRunRefresh);
    }

    pub fn begin_agent_run(&mut self) {
        self.view
            .async_operations
            .begin_unique(DesktopAsyncOperationKind::AgentRun);
    }

    pub fn finish_agent_run(&mut self) {
        self.view
            .async_operations
            .finish_kind(DesktopAsyncOperationKind::AgentRun);
    }

    pub fn clear_post_run_refresh_pending(&mut self) {
        self.view
            .async_operations
            .finish_kind(DesktopAsyncOperationKind::TerminalRunRefresh);
    }

    pub fn post_run_refresh_pending(&self) -> bool {
        self.view
            .async_operations
            .is_pending(DesktopAsyncOperationKind::TerminalRunRefresh)
    }

    pub fn begin_session_delete_mutation(&mut self) -> DesktopAsyncOperationId {
        self.view
            .async_operations
            .begin(DesktopAsyncOperationKind::SessionDelete)
    }

    pub fn finish_session_delete_mutation(
        &mut self,
        operation_id: DesktopAsyncOperationId,
    ) -> bool {
        self.view.async_operations.finish(operation_id)
    }

    pub fn begin_session_archive_mutation(&mut self) -> DesktopAsyncOperationId {
        self.view
            .async_operations
            .begin(DesktopAsyncOperationKind::SessionArchive)
    }

    pub fn finish_session_archive_mutation(
        &mut self,
        operation_id: DesktopAsyncOperationId,
    ) -> bool {
        self.view.async_operations.finish(operation_id)
    }

    pub fn begin_session_rollback_mutation(&mut self) -> DesktopAsyncOperationId {
        self.view
            .async_operations
            .begin(DesktopAsyncOperationKind::SessionRollback)
    }

    pub fn finish_session_rollback_mutation(
        &mut self,
        operation_id: DesktopAsyncOperationId,
    ) -> bool {
        self.view.async_operations.finish(operation_id)
    }

    pub fn begin_session_maintenance_mutation(&mut self) -> DesktopAsyncOperationId {
        self.view
            .async_operations
            .begin(DesktopAsyncOperationKind::SessionMaintenance)
    }

    pub fn begin_access_mode_persistence(&mut self) -> DesktopAsyncOperationId {
        self.view
            .async_operations
            .begin_unique(DesktopAsyncOperationKind::AccessModePersistence)
    }

    pub fn begin_session_settings_persistence(&mut self) -> DesktopAsyncOperationId {
        self.view
            .async_operations
            .begin_unique(DesktopAsyncOperationKind::SessionSettingsPersistence)
    }

    pub fn begin_steer_submission(&mut self) -> DesktopAsyncOperationId {
        self.view
            .async_operations
            .begin_unique(DesktopAsyncOperationKind::SteerSubmission)
    }

    pub fn finish_steer_submission(&mut self, operation_id: DesktopAsyncOperationId) -> bool {
        self.view.async_operations.finish(operation_id)
    }

    pub fn steer_submission_pending(&self) -> bool {
        self.view
            .async_operations
            .is_pending(DesktopAsyncOperationKind::SteerSubmission)
    }

    pub fn finish_access_mode_persistence(
        &mut self,
        operation_id: DesktopAsyncOperationId,
    ) -> bool {
        self.view.async_operations.finish(operation_id)
    }

    pub fn access_mode_persistence_is_current(
        &self,
        operation_id: DesktopAsyncOperationId,
    ) -> bool {
        self.view.async_operations.contains(operation_id)
    }

    pub fn finish_session_settings_persistence(
        &mut self,
        operation_id: DesktopAsyncOperationId,
    ) -> bool {
        self.view.async_operations.finish(operation_id)
    }

    pub fn finish_session_maintenance_mutation(
        &mut self,
        operation_id: DesktopAsyncOperationId,
    ) -> bool {
        self.view.async_operations.finish(operation_id)
    }

    pub fn begin_session_search(&mut self) -> DesktopAsyncOperationId {
        self.view
            .async_operations
            .begin(DesktopAsyncOperationKind::SessionSearch)
    }

    pub fn finish_session_search(&mut self, operation_id: DesktopAsyncOperationId) -> bool {
        self.view.async_operations.finish(operation_id)
    }

    pub fn begin_project_delete_mutation(&mut self) -> DesktopAsyncOperationId {
        self.view
            .async_operations
            .begin(DesktopAsyncOperationKind::ProjectDelete)
    }

    pub fn finish_project_delete_mutation(
        &mut self,
        operation_id: DesktopAsyncOperationId,
    ) -> bool {
        self.view.async_operations.finish(operation_id)
    }

    pub fn background_mutation_pending(&self) -> bool {
        self.view
            .async_operations
            .is_pending(DesktopAsyncOperationKind::ProjectDelete)
            || self
                .view
                .async_operations
                .is_pending(DesktopAsyncOperationKind::SessionDelete)
            || self
                .view
                .async_operations
                .is_pending(DesktopAsyncOperationKind::SessionArchive)
            || self
                .view
                .async_operations
                .is_pending(DesktopAsyncOperationKind::SessionRollback)
            || self
                .view
                .async_operations
                .is_pending(DesktopAsyncOperationKind::SessionMaintenance)
            || self
                .view
                .async_operations
                .is_pending(DesktopAsyncOperationKind::AccessModePersistence)
            || self
                .view
                .async_operations
                .is_pending(DesktopAsyncOperationKind::SessionSettingsPersistence)
            || self
                .view
                .async_operations
                .is_pending(DesktopAsyncOperationKind::SteerSubmission)
    }

    pub fn begin_history_export(&mut self) {
        self.view
            .async_operations
            .begin_unique(DesktopAsyncOperationKind::HistoryExport);
    }

    pub fn finish_history_export(&mut self) {
        self.view
            .async_operations
            .finish_kind(DesktopAsyncOperationKind::HistoryExport);
    }

    pub fn async_polling_required(&self) -> bool {
        self.view.async_operations.polling_required() || self.is_busy()
    }

    pub fn pending_async_operation_keys(&self) -> Vec<String> {
        self.view
            .async_operations
            .active_kinds()
            .into_iter()
            .map(|kind| kind.key().to_string())
            .collect()
    }

    pub fn async_operations(&self) -> &DesktopAsyncOperationRegistry {
        &self.view.async_operations
    }

    fn clear_navigation_operations(&mut self) {
        self.view
            .async_operations
            .finish_kind(DesktopAsyncOperationKind::WorkspaceLoad);
        self.view
            .async_operations
            .finish_kind(DesktopAsyncOperationKind::SessionLoad);
    }

    pub fn selected_session_title(&self) -> String {
        self.snapshot
            .session_rows
            .get(self.snapshot.selected_session_index)
            .map(|row| row.label.clone())
            .or_else(|| {
                self.app_state
                    .current_session_id
                    .map(|_| self.app_state.current_session_title.clone())
            })
            .unwrap_or_else(|| "セッション未選択".to_string())
    }

    pub fn current_session_label(&self) -> String {
        self.app_state
            .current_session_id
            .map(|_| self.app_state.current_session_title.clone())
            .unwrap_or_else(|| "新規チャット".to_string())
    }

    pub fn selected_detail(&self) -> DesktopSessionDetail {
        if let Some(selected_id) = self.selected_session_id() {
            if self.app_state.current_session_id == Some(selected_id) {
                if let Some(open_session) = self
                    .open_session
                    .as_ref()
                    .filter(|open_session| open_session.session_id() == selected_id)
                {
                    return open_session
                        .live_detail(&self.app_state, self.snapshot.detail_for(selected_id));
                }
                return build_session_detail_from_app_state_with_session(&self.app_state, None);
            }
            if let Some(detail) = self.snapshot.detail_for(selected_id) {
                return detail.clone();
            }
        }
        if self.app_state.current_session_id.is_some() {
            if let Some(open_session) = self.open_session.as_ref().filter(|open_session| {
                Some(open_session.session_id()) == self.app_state.current_session_id
            }) {
                open_session.live_detail(&self.app_state, None)
            } else {
                build_session_detail_from_app_state_with_session(&self.app_state, None)
            }
        } else {
            DesktopSessionDetail {
                session_id: SessionId::new(),
                thread_empty: true,
                transcript_text: "チャットはまだありません。".to_string(),
                transcript_rows: vec![crate::desktop::models::DesktopTranscriptRow {
                    row_kind: crate::desktop::models::DesktopTranscriptRowKind::EmptyPlaceholder,
                    stable_history_identity: None,
                    step: "00".to_string(),
                    title: "チャットはありません".to_string(),
                    body: if self.selected_project_id().is_some() {
                        "下の入力欄から依頼を送ると、このプロジェクトの最初のチャットが作成されます。".to_string()
                    } else {
                        "通常チャットとして開始できます。プロジェクト作業をする場合は、左のプロジェクト作成からフォルダを選択してください。".to_string()
                    },
                    file_changes: Vec::new(),
                }],
                turn_page_offset: 0,
                turn_page_limit: 0,
                turn_page_total: 0,
                turn_page_has_more: false,
                tool_status_text: "ツール実行はまだありません。".to_string(),
                progress_text: "待機中\nフェーズ: 準備完了\n手順: 実行中の作業はありません"
                    .to_string(),
                run_status_text: "待機中".to_string(),
                session_usage_label: "セッション累計: 未計測".to_string(),
                session_usage_title: "完了済みturnのcanonical terminal telemetryはまだありません。"
                    .to_string(),
                session_usage_state: "missing".to_string(),
                artifacts: Vec::new(),
                file_changes: Vec::new(),
                file_change_summary_text: "ファイル変更はまだありません。".to_string(),
                artifact_preview_available: false,
                artifact_preview_text: "アーティファクトは選択されていません。".to_string(),
            }
        }
    }

    pub fn selected_artifact_preview_text(&self) -> String {
        let detail = self.selected_detail();
        let Some(artifact) = detail.artifacts.get(self.view.artifact_selected_index) else {
            return detail.artifact_preview_text;
        };
        super::query::format_artifact_preview(Some(artifact), &detail.file_changes)
    }

    pub fn selected_artifact_path(&self) -> Option<String> {
        self.selected_detail()
            .artifacts
            .get(self.view.artifact_selected_index)
            .map(|artifact| artifact.path.clone())
    }

    pub fn select_artifact(&mut self, index: usize) {
        let detail = self.selected_detail();
        if index < detail.artifacts.len() {
            self.view.artifact_selected_index = index;
        }
    }

    pub fn selected_artifact_index(&self) -> i32 {
        if self.selected_detail().artifacts.is_empty() {
            -1
        } else {
            self.view.artifact_selected_index as i32
        }
    }

    pub fn current_run_status_text(&self) -> String {
        if self.app_state.current_session_id.is_some() {
            if let Some(open_session) = self.open_session.as_ref().filter(|open_session| {
                Some(open_session.session_id()) == self.app_state.current_session_id
            }) {
                open_session.live_detail(&self.app_state, None)
            } else {
                build_session_detail_from_app_state_with_session(&self.app_state, None)
            }
            .run_status_text
        } else {
            self.selected_detail().run_status_text
        }
    }

    fn image_attachment_mutation_admission_open(&mut self) -> bool {
        if self.app_state.prompt_review.is_none() {
            return true;
        }
        self.set_status_message(
            "image attachment cannot change while Prompt Review owns the composer draft",
        );
        false
    }

    pub fn set_image_attachment_input(&mut self, input: String) -> bool {
        if !self.image_attachment_mutation_admission_open() {
            return false;
        }
        self.composer.image_attachment_input = input;
        true
    }

    pub fn attach_image_path(&mut self, path: camino::Utf8PathBuf) -> bool {
        if !self.image_attachment_mutation_admission_open() {
            return false;
        }
        if self
            .composer
            .image_attachment_paths
            .iter()
            .any(|existing| existing == &path)
        {
            self.set_status_message("Image is already attached.");
            return true;
        }
        self.composer.image_attachment_paths.push(path);
        self.composer.image_attachment_input.clear();
        self.set_status_message("Image attached to the next prompt.");
        true
    }

    pub fn clear_image_attachments(&mut self) -> bool {
        if !self.image_attachment_mutation_admission_open() {
            return false;
        }
        self.composer.image_attachment_paths.clear();
        self.composer.image_attachment_input.clear();
        self.set_status_message("Image attachments cleared.");
        true
    }

    pub fn remove_image_attachment(&mut self, index: usize) -> bool {
        if !self.image_attachment_mutation_admission_open() {
            return false;
        }
        if index >= self.composer.image_attachment_paths.len() {
            self.set_status_message("Image attachment is no longer available.");
            return true;
        }
        let removed = self.composer.image_attachment_paths.remove(index);
        self.set_status_message(format!("Removed image attachment {}", removed));
        true
    }

    pub fn image_attachment_summary(&self) -> String {
        match self.composer.image_attachment_paths.len() {
            0 => "No images attached".to_string(),
            1 => format!("1 image: {}", self.composer.image_attachment_paths[0]),
            count => format!("{count} images attached"),
        }
    }

    pub fn set_workspace_input(&mut self, input: String) {
        self.workspace_input = input;
    }

    pub fn accept_provider_action_input(
        &mut self,
        base_url: String,
        profile: ProviderProfile,
        api_key_env: String,
        context_window: String,
        selected_model_id: String,
    ) -> bool {
        let normalized = normalize_provider_base_url(&base_url);
        let current_target_base_url = if self.provider_config.provider_loading {
            Some(normalize_provider_base_url(
                &self.provider_config.provider_base_url_input,
            ))
        } else {
            self.provider_config.provider_loaded_base_url.clone()
        };
        let target_changed = current_target_base_url.as_deref() != Some(normalized.as_str())
            || self.provider_config.provider_profile_input != profile
            || self.provider_config.provider_api_key_env_input.trim() != api_key_env.trim();
        self.provider_config.provider_base_url_input = base_url;
        self.provider_config.provider_profile_input = profile;
        self.provider_config.provider_api_key_env_input = api_key_env;
        self.provider_config.provider_context_window_input = context_window;
        self.provider_config.provider_selected_model_id_input = selected_model_id.clone();
        if let Some(index) = self
            .provider_config
            .provider_models
            .iter()
            .position(|model| model == &selected_model_id)
        {
            self.provider_config.provider_selected_index = index as i32;
        } else {
            self.provider_config.provider_selected_index = -1;
        }
        if target_changed {
            self.provider_config.provider_loading = false;
            self.provider_config.provider_loaded_base_url = None;
            self.provider_config.provider_loaded_profile = None;
            self.provider_config.provider_loaded_api_key_env = None;
            self.view
                .async_operations
                .finish_kind(DesktopAsyncOperationKind::ProviderModelCatalogLoad);
        }
        target_changed
    }

    pub fn load_open_session(&mut self, read: &CanonicalSessionRead) {
        let session = &read.session;
        self.apply_root_session_config(session);
        self.bind_composer_to_loaded_session(session.id);
        let open_session = match (
            self.file_change_storage_root.as_deref(),
            self.file_change_display_root.as_deref(),
        ) {
            (Some(storage_root), Some(display_root)) => {
                OpenSessionView::from_loaded_with_roots(read, storage_root, display_root)
            }
            _ => OpenSessionView::from_loaded(read),
        };
        self.app_state.load_canonical_session_read(read);
        self.status_code = self
            .app_state
            .interruption_cause
            .map(DesktopStatusCode::from_interruption)
            .unwrap_or(DesktopStatusCode::Plain);
        if let Some(context_window) = latest_context_window_from_history_items(&read.history.items)
        {
            self.app_state.latest_context_window = Some(context_window);
        }
        self.open_session = Some(open_session);
        if let Some(index) = self
            .snapshot
            .session_rows
            .iter()
            .position(|row| Some(row.session_id) == self.app_state.current_session_id)
        {
            self.snapshot.selected_session_index = index;
        }
        self.update_session_title_projection(session.id, &session.title);
        self.update_session_row_status(session.id, session.status);
        self.view.overlay = DesktopOverlay::None;
        self.apply_startup_overlay();
        self.view.artifact_selected_index = 0;
    }

    pub fn turn_page_load_pending(&self) -> bool {
        self.view
            .async_operations
            .is_pending(DesktopAsyncOperationKind::TurnPageLoad)
    }

    pub fn merge_open_session_history(&mut self, read: &CanonicalSessionRead) -> bool {
        let (session, turn_items, detail, active_turn_id, active_turn_expectation) = {
            let Some(open_session) = self
                .open_session
                .as_mut()
                .filter(|open_session| open_session.session_id() == read.session.id)
            else {
                return false;
            };
            if !open_session.merge_contiguous(read) {
                return false;
            }
            (
                open_session.session().clone(),
                open_session.turn_items().to_vec(),
                open_session.stored_detail().clone(),
                open_session.active_turn_id(),
                open_session.active_turn_expectation(),
            )
        };

        self.apply_root_session_config(&session);
        let preserve_current_projection = self.app_state.current_session_id == Some(session.id);
        if preserve_current_projection {
            self.app_state.refresh_plan_from_turn_items(&turn_items);
            self.app_state
                .reconcile_compactions_from_canonical_turn_items(
                    &turn_items,
                    active_turn_expectation.latest_turn_id(),
                );
        } else {
            self.app_state
                .load_turn_items_with_active_turn(&session, &turn_items, active_turn_id);
        }
        self.app_state.active_turn_expectation = active_turn_expectation;
        self.reconcile_current_active_turn_progress();
        self.status_code = self
            .app_state
            .interruption_cause
            .map(DesktopStatusCode::from_interruption)
            .unwrap_or(DesktopStatusCode::Plain);
        if let Some(context_window) = latest_context_window_from_history_items(&read.history.items)
        {
            self.app_state.latest_context_window = Some(context_window);
        }
        self.snapshot.replace_detail(detail);
        self.update_session_title_projection(session.id, &session.title);
        let row_status = if preserve_current_projection {
            session_status_from_run_status(self.app_state.run_status)
        } else {
            session.status
        };
        self.update_session_row_status(session.id, row_status);
        true
    }

    pub fn load_open_session_preserving_history(&mut self, read: &CanonicalSessionRead) -> bool {
        if self.merge_open_session_history(read) {
            self.apply_canonical_terminal_to_running_session(read);
            self.reconcile_current_terminal_tool_projection();
            return true;
        }
        let preserved = self
            .open_session
            .as_mut()
            .filter(|open_session| open_session.session_id() == read.session.id)
            .is_some_and(|open_session| {
                open_session.refresh_metadata_preserving_loaded_history(read)
            });
        if !preserved {
            self.load_open_session(read);
            return true;
        }
        let open_session = self
            .open_session
            .as_ref()
            .expect("preserved open session remains available");
        let session = open_session.session().clone();
        let detail = open_session.stored_detail().clone();
        self.apply_root_session_config(&session);
        if let Some(context_window) = latest_context_window_from_history_items(&read.history.items)
        {
            self.app_state.latest_context_window = Some(context_window);
        }
        self.snapshot.replace_detail(detail);
        self.update_session_title_projection(session.id, &session.title);
        self.update_session_row_status(session.id, session.status);
        self.apply_canonical_terminal_to_running_session(read);
        self.reconcile_current_terminal_tool_projection();
        false
    }

    fn reconcile_current_terminal_tool_projection(&mut self) -> bool {
        let Some((session_id, latest_turn_id, turn_items)) = self
            .open_session
            .as_ref()
            .filter(|open_session| {
                open_session.active_turn_id().is_none()
                    && matches!(
                        open_session.session().status,
                        SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed
                    )
            })
            .map(|open_session| {
                (
                    open_session.session_id(),
                    open_session.latest_turn_id(),
                    open_session.turn_items().to_vec(),
                )
            })
        else {
            return false;
        };
        self.app_state
            .reconcile_terminal_tool_projection(session_id, latest_turn_id, &turn_items)
    }

    fn apply_canonical_terminal_to_running_session(&mut self, read: &CanonicalSessionRead) -> bool {
        if self.app_state.current_session_id != Some(read.session.id)
            || !matches!(self.app_state.run_status, RunStatus::Running)
        {
            return false;
        }
        let Some(outcome) = read.turns.items.iter().rev().find_map(|item| {
            if let TurnItemPayload::Terminal { outcome } = &item.payload {
                Some(outcome.clone())
            } else {
                None
            }
        }) else {
            return false;
        };
        if outcome.session_status() != read.session.status {
            return false;
        }

        self.app_state.apply_terminal_outcome_projection(&outcome);
        self.status_code = self
            .app_state
            .interruption_cause
            .map(DesktopStatusCode::from_interruption)
            .unwrap_or(DesktopStatusCode::Plain);
        self.update_session_row_status(read.session.id, outcome.session_status());
        true
    }

    pub fn apply_run_summary(&mut self, summary: crate::session::RunSummary) {
        let session_id = summary.session_id();
        let session_status = summary.status();
        self.app_state.apply_run_summary(summary);
        self.status_code = self
            .app_state
            .interruption_cause
            .map(DesktopStatusCode::from_interruption)
            .unwrap_or(DesktopStatusCode::Plain);
        self.update_session_row_status(session_id, session_status);
    }

    pub fn next_turn_page_offset(&self) -> Option<usize> {
        let detail = self.selected_detail();
        if !detail.turn_page_has_more || detail.turn_page_limit == 0 {
            return None;
        }
        let selected_session_id = self.selected_session_id();
        let next_offset = self
            .open_session
            .as_ref()
            .filter(|open_session| Some(open_session.session_id()) == selected_session_id)
            .map(OpenSessionView::loaded_turn_end)
            .unwrap_or_else(|| {
                detail
                    .turn_page_offset
                    .saturating_add(detail.turn_page_limit)
            });
        (next_offset < detail.turn_page_total).then_some(next_offset)
    }

    pub fn refresh_open_session_projection(&mut self, read: &CanonicalSessionRead) {
        let mut open_session = self.open_session.take().filter(|open_session| {
            open_session.session_id() == read.session.id
                && open_session.session_id() == read.turns.session.id
        });
        let retained = open_session.as_mut().is_some_and(|open_session| {
            open_session.merge_contiguous(read)
                || open_session.refresh_metadata_preserving_loaded_history(read)
        });
        if !retained {
            open_session = Some(
                match (
                    self.file_change_storage_root.as_deref(),
                    self.file_change_display_root.as_deref(),
                ) {
                    (Some(storage_root), Some(display_root)) => {
                        OpenSessionView::from_loaded_with_roots(read, storage_root, display_root)
                    }
                    _ => OpenSessionView::from_loaded(read),
                },
            );
        }
        let open_session = open_session.expect("open session projection is always available");
        let session = open_session.session().clone();
        let turn_items = open_session.turn_items();
        let detail = open_session.stored_detail().clone();
        self.apply_root_session_config(&session);
        self.app_state.refresh_plan_from_turn_items(turn_items);
        if let Some(context_window) = latest_context_window_from_history_items(&read.history.items)
        {
            self.app_state.latest_context_window = Some(context_window);
        }
        self.open_session = Some(open_session);
        self.reconcile_current_active_turn_progress();
        self.snapshot.replace_detail(detail);
        self.update_session_title_projection(session.id, &session.title);
        self.update_session_row_status(session.id, session.status);
    }

    fn reconcile_current_active_turn_progress(&mut self) {
        if let Some(open_session) = &self.open_session
            && let Some(progress) = open_session.active_turn_progress()
        {
            self.app_state.reconcile_active_turn_progress(
                open_session.session_id(),
                progress,
                open_session.turn_items(),
            );
        }
    }

    pub fn apply_run_event(&mut self, event: &crate::session::RunEvent) {
        crate::tui::reducer::reduce_run_event(&mut self.app_state, event);
        self.status_code = match event {
            crate::session::RunEvent::TurnTerminal { terminal, .. } => terminal
                .interruption_cause()
                .map(DesktopStatusCode::from_interruption)
                .unwrap_or(DesktopStatusCode::Plain),
            _ => DesktopStatusCode::Plain,
        };
        match event {
            crate::session::RunEvent::SessionStarted { session_id, title } => {
                self.update_session_title_projection(*session_id, title);
                self.update_session_row_status(*session_id, SessionStatus::Running);
            }
            crate::session::RunEvent::SessionTitleUpdated { session_id, title } => {
                self.update_session_title_projection(*session_id, title);
            }
            crate::session::RunEvent::TurnTerminal {
                session_id,
                terminal,
            } => {
                self.update_session_row_status(*session_id, terminal.session_status());
            }
            _ => {}
        }
    }

    fn update_session_title_projection(&mut self, session_id: SessionId, title: &str) {
        let incoming_is_placeholder = is_placeholder_session_title(title);
        if self.app_state.current_session_id == Some(session_id)
            && (!incoming_is_placeholder
                || is_placeholder_session_title(&self.app_state.current_session_title))
        {
            self.app_state.current_session_title = title.to_string();
        }
        for row in self
            .snapshot
            .session_rows
            .iter_mut()
            .chain(self.snapshot.chat_session_rows.iter_mut())
        {
            if row.session_id == session_id {
                if incoming_is_placeholder && !is_placeholder_session_title(&row.title) {
                    continue;
                }
                row.set_title_preserving_status(title);
            }
        }
    }

    fn update_session_row_status(&mut self, session_id: SessionId, status: SessionStatus) {
        for row in self
            .snapshot
            .session_rows
            .iter_mut()
            .chain(self.snapshot.chat_session_rows.iter_mut())
        {
            if row.session_id == session_id {
                row.set_status(status);
            }
        }
    }

    pub fn mark_run_stop_requested(&mut self, reason: &str, status_message: &str) {
        // Dispatching Stop is not the durable terminal. Keep the run active until the matching
        // TurnTerminal projection arrives so navigation and new admission remain closed while the
        // worker settles cancellation.
        if !matches!(self.app_state.run_status, RunStatus::Running) {
            return;
        }
        self.app_state.status_message = Some(status_message.to_string());
        self.app_state.progress.status = "Stopping".to_string();
        self.app_state.progress.current_phase = RunProgressPhase::StopRequested;
        self.app_state.progress.active_step = reason.to_string();
    }

    pub fn apply_durable_prompt_dispatch(&mut self, prompt_dispatch: &PromptDispatchPart) {
        self.app_state
            .apply_durable_prompt_dispatch(prompt_dispatch);
    }

    pub fn begin_prompt_enhance_at(
        &mut self,
        request_id: u64,
        raw_prompt: &str,
        cancellation: CancellationToken,
        expected_active_turn: ActiveTurnExpectation,
    ) {
        self.cancel_prompt_enhance_transport();
        self.prompt_review_owner = Some(PromptReviewDesktopOwner {
            request_id,
            expected_active_turn,
            cancellation: Some(cancellation),
        });
        self.view
            .async_operations
            .begin_unique(DesktopAsyncOperationKind::PromptEnhance);
        self.app_state.begin_prompt_enhance(request_id, raw_prompt);
        self.view.overlay = DesktopOverlay::PromptReview;
    }

    #[cfg(test)]
    pub fn begin_prompt_enhance(
        &mut self,
        request_id: u64,
        raw_prompt: &str,
        cancellation: CancellationToken,
    ) {
        self.begin_prompt_enhance_at(
            request_id,
            raw_prompt,
            cancellation,
            ActiveTurnExpectation::initial_idle(),
        );
    }

    pub fn finish_prompt_enhance(&mut self, request_id: u64, draft: String) -> bool {
        let finished = self
            .app_state
            .finish_prompt_enhance(request_id, draft.clone());
        if finished {
            self.finish_prompt_enhance_transport(request_id);
            self.view
                .async_operations
                .finish_kind(DesktopAsyncOperationKind::PromptEnhance);
            self.view.overlay = DesktopOverlay::PromptReview;
        }
        finished
    }

    pub fn fail_prompt_enhance(&mut self, request_id: u64) -> bool {
        let active = self
            .app_state
            .prompt_review
            .as_ref()
            .is_some_and(|review| review.request_id == request_id);
        if active {
            self.cancel_prompt_review();
        } else if self.app_state.prompt_review.is_none() {
            self.view
                .async_operations
                .finish_kind(DesktopAsyncOperationKind::PromptEnhance);
        }
        active
    }

    pub fn cancel_prompt_review(&mut self) {
        self.cancel_prompt_enhance_transport();
        self.view
            .async_operations
            .finish_kind(DesktopAsyncOperationKind::PromptEnhance);
        self.app_state.cancel_prompt_review();
        if self.view.overlay == DesktopOverlay::PromptReview {
            self.view.overlay = DesktopOverlay::None;
        }
    }

    pub fn cancel_prompt_review_if_current(&mut self, request_id: u64) -> bool {
        if !self
            .app_state
            .prompt_review
            .as_ref()
            .is_some_and(|review| review.request_id == request_id)
        {
            return false;
        }
        self.cancel_prompt_review();
        true
    }

    pub fn build_prompt_dispatch(&self, send_enhanced: bool) -> Option<PromptDispatchPart> {
        self.app_state.build_prompt_dispatch(send_enhanced)
    }

    pub fn build_prompt_dispatch_from_draft(
        &mut self,
        request_id: u64,
        review_draft: String,
        send_enhanced: bool,
    ) -> Option<PromptDispatchPart> {
        self.app_state
            .build_prompt_dispatch_from_draft(request_id, review_draft, send_enhanced)
    }

    pub fn prompt_review_expected_active_turn(
        &self,
        request_id: u64,
    ) -> Option<ActiveTurnExpectation> {
        self.prompt_review_owner
            .as_ref()
            .filter(|owner| owner.request_id == request_id)
            .map(|owner| owner.expected_active_turn)
    }

    pub fn set_status_message(&mut self, message: impl Into<String>) {
        self.app_state.status_message = Some(message.into());
        self.status_code = DesktopStatusCode::Plain;
    }

    pub fn set_typed_status_message(
        &mut self,
        code: DesktopStatusCode,
        message: impl Into<String>,
    ) {
        self.app_state.status_message = Some(message.into());
        self.status_code = code;
    }

    pub fn set_status_message_preserving_code(&mut self, message: impl Into<String>) {
        self.app_state.status_message = Some(message.into());
    }

    pub fn start_new_chat(&mut self) {
        if !self.can_begin_navigation() {
            self.set_status_message("new chat cannot start while another operation is active");
            return;
        }
        self.snapshot.selected_session_index = self.snapshot.session_rows.len();
        self.cancel_prompt_review();
        self.composer
            .reset_owner(&self.snapshot.workspace_path, None);
        self.reset_app_state_preserving_file_change_display_roots();
        self.open_session = None;
        self.provider_config
            .replace_effective_config(self.global_config.clone());
        self.view.artifact_selected_index = 0;
        self.view.overlay = DesktopOverlay::None;
        self.set_status_message("new chat ready");
    }

    fn finish_prompt_enhance_transport(&mut self, request_id: u64) {
        if let Some(owner) = self
            .prompt_review_owner
            .as_mut()
            .filter(|owner| owner.request_id == request_id)
        {
            owner.cancellation = None;
        }
    }

    fn cancel_prompt_enhance_transport(&mut self) {
        if let Some(owner) = self.prompt_review_owner.take()
            && let Some(cancellation) = owner.cancellation
        {
            cancellation.cancel();
        }
    }

    pub fn reset_effective_config(&mut self, config: ResolvedConfig) {
        self.provider_config.replace_effective_config(config);
    }

    pub fn replace_global_config(&mut self, config: ResolvedConfig) {
        self.global_config = config;
        let effective = if self.startup.requires_initial_setup() {
            self.global_config.clone()
        } else {
            self.open_session
                .as_ref()
                .filter(|open_session| {
                    Some(open_session.session_id()) == self.app_state.current_session_id
                })
                .map(|open_session| {
                    resolved_config_for_root_session(&self.global_config, open_session.session())
                })
                .unwrap_or_else(|| self.global_config.clone())
        };
        self.provider_config.replace_effective_config(effective);
    }

    pub fn global_config(&self) -> &ResolvedConfig {
        &self.global_config
    }

    pub(crate) fn apply_persisted_root_session_record(
        &mut self,
        session: crate::session::SessionRecord,
    ) -> bool {
        let Some(open_session) = self.open_session.as_ref() else {
            return false;
        };
        if self.app_state.current_session_id != Some(session.id)
            || open_session.session_id() != session.id
            || open_session.session().project_id != session.project_id
            || open_session.session().cwd != session.cwd
            || open_session.session().session_settings_revision > session.session_settings_revision
            || self.app_state.sessions.iter().any(|current| {
                current.id == session.id
                    && (current.project_id != session.project_id
                        || current.cwd != session.cwd
                        || current.session_settings_revision > session.session_settings_revision)
            })
            || self.app_state.loaded_sessions.iter().any(|summary| {
                summary.session.id == session.id
                    && (summary.session.project_id != session.project_id
                        || summary.session.cwd != session.cwd
                        || summary.session.session_settings_revision
                            > session.session_settings_revision)
            })
        {
            return false;
        }
        let Some(merged_open_session) =
            merge_root_session_settings(open_session.session(), &session)
        else {
            return false;
        };
        if !self.open_session.as_mut().is_some_and(|open_session| {
            open_session.replace_session_record(merged_open_session.clone())
        }) {
            return false;
        }
        for current in &mut self.app_state.sessions {
            if let Some(merged) = merge_root_session_settings(current, &session) {
                *current = merged;
            }
        }
        for summary in &mut self.app_state.loaded_sessions {
            if let Some(merged) = merge_root_session_settings(&summary.session, &session) {
                summary.session = merged;
            }
        }
        self.apply_root_session_config(&merged_open_session);
        true
    }

    pub(crate) fn persisted_root_session_settings_are_projected(
        &self,
        session: &crate::session::SessionRecord,
    ) -> bool {
        self.app_state.current_session_id == Some(session.id)
            && self
                .open_session
                .as_ref()
                .filter(|open_session| open_session.session_id() == session.id)
                .is_some_and(|open_session| {
                    root_session_settings_match(open_session.session(), session)
                })
    }

    pub(crate) fn persisted_root_session_settings_and_effective_config_are_projected(
        &self,
        session: &crate::session::SessionRecord,
    ) -> bool {
        if !self.persisted_root_session_settings_are_projected(session) {
            return false;
        }
        let expected = resolved_config_for_root_session(&self.global_config, session);
        session_provider_settings_match(&self.provider_config.effective_config, &expected)
            && self
                .provider_config
                .effective_config
                .permissions
                .access_mode
                == expected.permissions.access_mode
    }

    pub(crate) fn root_session_settings_config_generation_delta(
        &self,
        current: &crate::session::SessionRecord,
        patch: &crate::session::SessionSettingsPatch,
    ) -> u64 {
        let mut next = current.clone();
        if let Some(cwd) = &patch.cwd {
            next.cwd.clone_from(cwd);
        }
        if let Some(model) = &patch.model {
            next.model.clone_from(model);
        }
        if let Some(base_url) = &patch.base_url {
            next.base_url.clone_from(base_url);
        }
        if let Some(provider_connection) = &patch.provider_connection {
            next.provider_connection = Some(provider_connection.clone());
        }
        if let Some(access_mode) = patch.access_mode {
            next.access_mode = access_mode;
        }
        next.model_parameters = patch.apply_to_model_parameters(&current.model_parameters);
        let expected = resolved_config_for_root_session(&self.global_config, &next);
        u64::from(!session_provider_settings_match(
            &self.provider_config.effective_config,
            &expected,
        ))
    }

    fn apply_root_session_config(&mut self, session: &crate::session::SessionRecord) {
        if self.startup.requires_initial_setup() {
            self.provider_config
                .replace_effective_config(self.global_config.clone());
            return;
        }
        let effective = resolved_config_for_root_session(&self.global_config, session);
        if session_provider_settings_match(&self.provider_config.effective_config, &effective) {
            self.provider_config.update_access_mode(session.access_mode);
        } else {
            self.provider_config.replace_effective_config(effective);
        }
    }

    fn prompt_review_owns_overlay(&self) -> bool {
        self.app_state.prompt_review.is_some() || self.prompt_review_owner.is_some()
    }

    fn begin_unscoped_overlay_transition(&mut self) -> bool {
        if self.prompt_review_owns_overlay() {
            self.view.overlay = DesktopOverlay::PromptReview;
            return false;
        }
        if self.startup.requires_initial_setup() {
            self.view.overlay = DesktopOverlay::InitialSetup;
            self.view.startup_overlay_forced = true;
            return false;
        }
        true
    }

    pub fn show_config_editor(&mut self) -> bool {
        if !self.begin_unscoped_overlay_transition() {
            return false;
        }
        self.view.startup_overlay_forced = false;
        self.view.overlay = DesktopOverlay::ConfigEditor;
        true
    }

    pub fn show_hub_editor(&mut self) -> bool {
        if !self.begin_unscoped_overlay_transition() {
            return false;
        }
        self.view.startup_overlay_forced = false;
        self.view.overlay = DesktopOverlay::HubConnection;
        true
    }

    pub fn show_mcp_publish_editor(&mut self) -> bool {
        if !self.begin_unscoped_overlay_transition() {
            return false;
        }
        self.view.startup_overlay_forced = false;
        self.view.overlay = DesktopOverlay::McpPublish;
        true
    }

    pub fn show_mcp_history(&mut self) -> bool {
        if !self.begin_unscoped_overlay_transition() {
            return false;
        }
        self.view.startup_overlay_forced = false;
        self.view.overlay = DesktopOverlay::McpHistory;
        true
    }

    pub fn show_session_settings(&mut self) -> bool {
        if !self.begin_unscoped_overlay_transition()
            || self.app_state.current_session_id.is_none()
            || self.open_session.as_ref().is_none_or(|open_session| {
                Some(open_session.session_id()) != self.app_state.current_session_id
            })
        {
            return false;
        }
        self.view.startup_overlay_forced = false;
        self.view.overlay = DesktopOverlay::SessionSettings;
        true
    }

    pub fn show_provider_editor(&mut self) -> bool {
        if !self.begin_unscoped_overlay_transition() {
            return false;
        }
        let global_config = self.global_config.clone();
        self.provider_config.provider_base_url_input = global_config.model.base_url.clone();
        self.provider_config.provider_profile_input = global_config.model.provider_profile;
        self.provider_config.provider_api_key_env_input =
            global_config.model.api_key_env.clone().unwrap_or_default();
        self.provider_config.provider_context_window_input =
            self.global_config.model.context_window.to_string();
        self.provider_config.provider_selected_model_id_input = global_config.model.model.clone();
        self.provider_config.provider_models = ensure_current_model(
            self.provider_config.provider_models.clone(),
            &global_config.model.model,
        );
        self.provider_config.provider_model_infos = ensure_current_model_info(
            self.provider_config.provider_model_infos.clone(),
            &global_config,
        );
        self.provider_config.provider_selected_index = self
            .provider_config
            .provider_models
            .iter()
            .position(|model| model == &global_config.model.model)
            .map(|index| index as i32)
            .unwrap_or(-1);
        self.view.startup_overlay_forced = false;
        self.view.overlay = DesktopOverlay::ProviderEditor;
        true
    }

    pub fn show_workspace_picker(&mut self, current_path: &str) -> bool {
        if !self.begin_unscoped_overlay_transition() {
            return false;
        }
        self.workspace_input = current_path.to_string();
        self.view.overlay = DesktopOverlay::WorkspacePicker;
        true
    }

    pub fn show_file_menu(&mut self) -> bool {
        if !self.begin_unscoped_overlay_transition() {
            return false;
        }
        self.view.overlay = DesktopOverlay::FileMenu;
        true
    }

    pub fn show_edit_menu(&mut self) -> bool {
        if !self.begin_unscoped_overlay_transition() {
            return false;
        }
        self.view.overlay = DesktopOverlay::EditMenu;
        true
    }

    pub fn show_view_menu(&mut self) -> bool {
        if !self.begin_unscoped_overlay_transition() {
            return false;
        }
        self.view.overlay = DesktopOverlay::ViewMenu;
        true
    }

    pub fn show_help_menu(&mut self) -> bool {
        if !self.begin_unscoped_overlay_transition() {
            return false;
        }
        self.view.overlay = DesktopOverlay::HelpMenu;
        true
    }

    pub fn show_about(&mut self) -> bool {
        if !self.begin_unscoped_overlay_transition() {
            return false;
        }
        self.view.overlay = DesktopOverlay::About;
        true
    }

    pub fn show_project_menu(&mut self) -> bool {
        if !self.begin_unscoped_overlay_transition() {
            return false;
        }
        self.view.overlay = DesktopOverlay::ProjectMenu;
        true
    }

    pub fn hide_overlay(&mut self) -> bool {
        if !self.begin_unscoped_overlay_transition() {
            if self.startup.requires_initial_setup() {
                self.set_status_message(
                    "初期設定が必要です。各ステップを確認し、Finish で設定を保存してください。",
                );
            }
            return false;
        }
        self.view.startup_overlay_forced = false;
        self.view.overlay = DesktopOverlay::None;
        true
    }

    pub(crate) fn complete_initial_setup_after_persist(&mut self) {
        self.startup.complete_after_persist();
        if !self.startup.requires_initial_setup()
            && let Some(session) = self
                .open_session
                .as_ref()
                .map(|open| open.session().clone())
        {
            self.apply_root_session_config(&session);
        }
        self.apply_startup_overlay();
    }

    pub fn begin_provider_model_load(&mut self, normalized_base_url: String) {
        self.provider_config.provider_base_url_input = normalized_base_url;
        self.provider_config.provider_loading = true;
        self.view
            .async_operations
            .begin_unique(DesktopAsyncOperationKind::ProviderModelCatalogLoad);
        self.provider_config.provider_loaded_base_url = None;
        self.provider_config.provider_loaded_profile = None;
        self.provider_config.provider_loaded_api_key_env = None;
        self.provider_config.set_status(
            DesktopProviderStatusKind::Loading,
            "Provider 状態",
            "詳細は必要な場合だけ展開してください。",
            "Loading models in the background...",
        );
    }

    pub fn begin_docling_readiness_check(&mut self, endpoint: String) {
        self.docling_readiness = DesktopDoclingReadinessState {
            status: DesktopDoclingReadinessStatus::Checking,
            endpoint,
            http_status: None,
            message: "Checking Docling /ready...".to_string(),
        };
        self.view
            .async_operations
            .begin_unique(DesktopAsyncOperationKind::DoclingReadinessCheck);
    }

    pub fn finish_docling_readiness_check(&mut self, result: DoclingReadinessResult) {
        self.view
            .async_operations
            .finish_kind(DesktopAsyncOperationKind::DoclingReadinessCheck);
        self.docling_readiness = DesktopDoclingReadinessState {
            status: if result.ready {
                DesktopDoclingReadinessStatus::Ready
            } else {
                DesktopDoclingReadinessStatus::Unavailable
            },
            endpoint: result.endpoint,
            http_status: Some(result.http_status),
            message: format!("Docling /ready returned HTTP {}.", result.http_status),
        };
    }

    pub fn fail_docling_readiness_check(&mut self, message: impl Into<String>) {
        self.view
            .async_operations
            .finish_kind(DesktopAsyncOperationKind::DoclingReadinessCheck);
        self.docling_readiness.status = DesktopDoclingReadinessStatus::Unavailable;
        self.docling_readiness.http_status = None;
        self.docling_readiness.message = message.into();
    }

    pub fn cancel_docling_readiness_check(&mut self) {
        self.view
            .async_operations
            .finish_kind(DesktopAsyncOperationKind::DoclingReadinessCheck);
        self.docling_readiness = DesktopDoclingReadinessState::default();
    }

    pub fn docling_readiness_check_pending(&self) -> bool {
        self.docling_readiness.status == DesktopDoclingReadinessStatus::Checking
            || self
                .view
                .async_operations
                .is_pending(DesktopAsyncOperationKind::DoclingReadinessCheck)
    }

    pub fn finish_provider_model_load(&mut self, infos: Vec<ProviderModelInfo>) {
        let normalized_base_url =
            normalize_provider_base_url(&self.provider_config.provider_base_url_input);
        let models = infos.iter().map(|info| info.id.clone()).collect::<Vec<_>>();
        self.provider_config.provider_models =
            ensure_current_model(models, &self.provider_config.effective_config.model.model);
        self.provider_config.provider_model_infos =
            ensure_current_model_infos(infos, &self.provider_config.effective_config);
        let desired_model_id = self
            .provider_config
            .provider_selected_model_id_input
            .clone();
        self.provider_config.provider_selected_index = self
            .provider_config
            .provider_models
            .iter()
            .position(|model| model == &desired_model_id)
            .or_else(|| {
                self.provider_config
                    .provider_models
                    .iter()
                    .position(|model| model == &self.provider_config.effective_config.model.model)
            })
            .map(|index| index as i32)
            .unwrap_or(-1);
        self.provider_config.provider_loaded_base_url = Some(normalized_base_url);
        self.provider_config.provider_loaded_profile =
            Some(self.provider_config.provider_profile_input);
        self.provider_config.provider_loaded_api_key_env = (!self
            .provider_config
            .provider_api_key_env_input
            .trim()
            .is_empty())
        .then(|| {
            self.provider_config
                .provider_api_key_env_input
                .trim()
                .to_string()
        });
        self.provider_config.provider_loading = false;
        self.view
            .async_operations
            .finish_kind(DesktopAsyncOperationKind::ProviderModelCatalogLoad);
        let details = self
            .selected_provider_model_info()
            .map(provider_model_summary)
            .unwrap_or_default();
        self.provider_config.set_status(
            DesktopProviderStatusKind::Success,
            "Provider 設定を読み込みました",
            "選択したモデルとBase URLをセッションまたは設定ファイルへ適用できます。",
            format!(
                "Loaded {} models. {}",
                self.provider_config.provider_models.len(),
                details
            )
            .trim()
            .to_string(),
        );
    }

    pub fn fail_provider_model_load(&mut self, message: impl Into<String>) {
        self.provider_config.provider_loading = false;
        self.view
            .async_operations
            .finish_kind(DesktopAsyncOperationKind::ProviderModelCatalogLoad);
        self.provider_config.provider_loaded_base_url = None;
        self.provider_config.provider_loaded_profile = None;
        self.provider_config.provider_loaded_api_key_env = None;
        let message = message.into();
        self.provider_config.set_status(
            DesktopProviderStatusKind::Error,
            "Providerモデル一覧を読み込めません",
            "Base URL と Provider の稼働状態を確認し、もう一度モデル一覧を読み込んでください。",
            message,
        );
        self.provider_config.provider_models = ensure_current_model(
            self.provider_config.provider_models.clone(),
            &self.provider_config.effective_config.model.model,
        );
        if self.provider_config.provider_selected_index < 0
            && !self.provider_config.provider_models.is_empty()
        {
            self.provider_config.provider_selected_index = 0;
        }
    }

    pub fn cancel_provider_model_load(&mut self) {
        self.provider_config.provider_loading = false;
        self.provider_config.provider_loaded_base_url = None;
        self.provider_config.provider_loaded_profile = None;
        self.provider_config.provider_loaded_api_key_env = None;
        self.view
            .async_operations
            .finish_kind(DesktopAsyncOperationKind::ProviderModelCatalogLoad);
        self.provider_config.set_status(
            DesktopProviderStatusKind::Idle,
            "Provider 設定を確認できます",
            "Base URL、mode、model を選択してセッションへ適用できます。",
            "",
        );
    }

    pub fn prompt_enhance_pending(&self) -> bool {
        self.view
            .async_operations
            .is_pending(DesktopAsyncOperationKind::PromptEnhance)
    }

    pub fn provider_model_load_pending(&self) -> bool {
        self.provider_config.provider_loading
            || self
                .view
                .async_operations
                .is_pending(DesktopAsyncOperationKind::ProviderModelCatalogLoad)
    }

    pub fn selected_provider_model(&self) -> Option<&str> {
        self.provider_config
            .provider_models
            .get(self.provider_config.provider_selected_index.max(0) as usize)
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
    }

    pub fn selected_provider_model_info(&self) -> Option<&ProviderModelInfo> {
        let selected = self.selected_provider_model()?;
        self.provider_config
            .provider_model_infos
            .iter()
            .find(|info| info.id == selected)
    }

    pub fn set_window_opacity_percent(&mut self, value: i32) {
        self.view.window_opacity_percent =
            value.clamp(MIN_WINDOW_OPACITY_PERCENT, MAX_WINDOW_OPACITY_PERCENT);
    }

    pub fn set_local_search_text(&mut self, text: String) {
        self.view.local_search_text = text;
    }

    pub fn set_session_search_text(&mut self, text: String) {
        self.view.session_search_text = text;
    }

    pub fn set_session_search_include_archived(&mut self, include_archived: bool) {
        self.view.session_search_include_archived = include_archived;
    }

    pub fn local_search_results_text(&self) -> String {
        let needle = self.view.local_search_text.trim().to_lowercase();
        if needle.is_empty() {
            return "プロジェクト、チャット、履歴、アーティファクト、コマンドを検索できます。"
                .to_string();
        }
        let mut lines = Vec::new();
        for row in &self.snapshot.project_rows {
            if row.label.to_lowercase().contains(&needle)
                || row.path.to_lowercase().contains(&needle)
            {
                lines.push(format!("プロジェクト: {}", row.label));
            }
        }
        for row in &self.snapshot.session_rows {
            if row.label.to_lowercase().contains(&needle) {
                lines.push(format!("チャット: {}", row.label));
            }
        }
        let detail = self.selected_detail();
        for line in detail.transcript_text.lines() {
            if line.to_lowercase().contains(&needle) {
                lines.push(format!("履歴: {}", truncate_for_search(line, 92)));
            }
        }
        for artifact in &detail.artifacts {
            if artifact.path.to_lowercase().contains(&needle)
                || artifact.label.to_lowercase().contains(&needle)
            {
                lines.push(format!(
                    "アーティファクト: {} [{}]",
                    artifact.path, artifact.action
                ));
            }
        }
        for command in &self.snapshot.command_rows {
            if command.name.to_lowercase().contains(&needle)
                || command.path.to_lowercase().contains(&needle)
            {
                lines.push(format!("コマンド: {} ({})", command.label, command.path));
            }
        }
        if lines.is_empty() {
            "一致する項目はありません。".to_string()
        } else {
            lines.into_iter().take(24).collect::<Vec<_>>().join("\n")
        }
    }

    pub fn show_command_palette(&mut self) -> bool {
        if !self.begin_unscoped_overlay_transition() {
            return false;
        }
        self.view.overlay = DesktopOverlay::CommandPalette;
        true
    }

    pub fn show_keyboard_shortcuts(&mut self) -> bool {
        if !self.begin_unscoped_overlay_transition() {
            return false;
        }
        self.view.overlay = DesktopOverlay::KeyboardShortcuts;
        true
    }

    pub fn select_command_from_palette(&mut self, index: usize) -> Option<String> {
        if self.view.overlay != DesktopOverlay::CommandPalette {
            return None;
        }
        let Some(command_name) = self
            .snapshot
            .command_rows
            .get(index)
            .map(|command| command.name.clone())
        else {
            self.set_status_message("command palette selection is no longer available");
            return None;
        };
        let insertion_text = format!("/{command_name} ");
        self.view.overlay = DesktopOverlay::None;
        self.set_status_message(format!("selected command /{command_name}"));
        Some(insertion_text)
    }

    pub fn is_busy(&self) -> bool {
        matches!(self.app_state.run_status, RunStatus::Running)
    }

    pub fn can_open_session(&self) -> bool {
        self.can_begin_navigation() && self.selected_session_id().is_some()
    }

    pub fn can_delete_session(&self) -> bool {
        !self.is_busy() && self.selected_session_id().is_some()
    }

    pub fn can_delete_project(&self) -> bool {
        !self.is_busy() && self.selected_project_id().is_some()
    }

    pub fn can_export_history(&self) -> bool {
        !self.is_busy()
            && !self.navigation_loading()
            && !self.background_mutation_pending()
            && !self
                .view
                .async_operations
                .is_pending(DesktopAsyncOperationKind::HistoryExport)
            && self.selected_session_id().is_some()
    }

    pub fn can_apply_provider_selection(&self) -> bool {
        self.provider_selection_input_is_complete()
    }

    pub(crate) fn can_save_provider_selection_global(&self) -> bool {
        self.provider_selection_input_is_complete()
    }

    fn provider_selection_input_is_complete(&self) -> bool {
        let normalized = normalize_provider_base_url(&self.provider_config.provider_base_url_input);
        let selected_model = self.selected_provider_model();
        !self.provider_config.provider_loading
            && !self
                .provider_config
                .provider_base_url_input
                .trim()
                .is_empty()
            && selected_model.is_some()
            && !normalized.is_empty()
    }

    pub fn provider_catalog_owns_current_target(&self) -> bool {
        let normalized = normalize_provider_base_url(&self.provider_config.provider_base_url_input);
        self.provider_config.provider_loaded_base_url.as_deref() == Some(normalized.as_str())
            && self.provider_config.provider_loaded_profile
                == Some(self.provider_config.provider_profile_input)
            && self.provider_config.provider_loaded_api_key_env.as_deref()
                == non_empty_trimmed(&self.provider_config.provider_api_key_env_input)
    }

    pub(crate) fn provider_input_matches_effective_target(&self) -> bool {
        let current_model = &self.provider_config.effective_config.model;
        normalize_provider_base_url(&current_model.base_url)
            == normalize_provider_base_url(&self.provider_config.provider_base_url_input)
            && self.provider_config.provider_profile_input == current_model.provider_profile
            && non_empty_trimmed(&self.provider_config.provider_api_key_env_input)
                == current_model.api_key_env.as_deref()
            && self.selected_provider_model() == Some(current_model.model.as_str())
    }

    pub(crate) fn provider_input_matches_global_target(&self) -> bool {
        let current_model = &self.global_config.model;
        normalize_provider_base_url(&current_model.base_url)
            == normalize_provider_base_url(&self.provider_config.provider_base_url_input)
            && self.provider_config.provider_profile_input == current_model.provider_profile
            && non_empty_trimmed(&self.provider_config.provider_api_key_env_input)
                == current_model.api_key_env.as_deref()
            && self.selected_provider_model() == Some(current_model.model.as_str())
    }

    fn with_provider_fields(mut self) -> Self {
        self.provider_config.provider_base_url_input =
            self.provider_config.effective_config.model.base_url.clone();
        self.provider_config.provider_profile_input =
            self.provider_config.effective_config.model.provider_profile;
        self.provider_config.provider_api_key_env_input = self
            .provider_config
            .effective_config
            .model
            .api_key_env
            .clone()
            .unwrap_or_default();
        self.provider_config.provider_context_window_input = self
            .provider_config
            .effective_config
            .model
            .context_window
            .to_string();
        self.provider_config.provider_loaded_base_url = None;
        self.provider_config.provider_loaded_profile = None;
        self.provider_config.provider_loaded_api_key_env = None;
        self
    }

    fn clamp_artifact_selection(&mut self) {
        let count = self.selected_detail().artifacts.len();
        if count == 0 {
            self.view.artifact_selected_index = 0;
        } else if self.view.artifact_selected_index >= count {
            self.view.artifact_selected_index = count - 1;
        }
    }

    fn apply_startup_overlay(&mut self) {
        if !self.begin_unscoped_overlay_transition() {
            return;
        }
        if let Some(overlay) = self.startup.action_overlay {
            self.view.overlay = overlay;
            self.view.startup_overlay_forced = true;
        } else if self.startup.status == super::startup::DesktopStartupStatus::Ready
            && self.view.startup_overlay_forced
        {
            self.view.startup_overlay_forced = false;
            self.view.overlay = DesktopOverlay::None;
        }
    }
}

pub(crate) fn resolved_config_for_root_session(
    global: &ResolvedConfig,
    session: &crate::session::SessionRecord,
) -> ResolvedConfig {
    crate::session::resolved_config_for_session(global, session)
}

fn non_empty_trimmed(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn session_provider_settings_match(current: &ResolvedConfig, next: &ResolvedConfig) -> bool {
    current.model.base_url == next.model.base_url
        && current.model.model == next.model.model
        && current.model.provider_profile == next.model.provider_profile
        && current.model.api_key_env == next.model.api_key_env
        && current.model.extra_headers == next.model.extra_headers
        && current.model.context_window == next.model.context_window
}

const fn session_status_from_run_status(status: RunStatus) -> SessionStatus {
    match status {
        RunStatus::Idle => SessionStatus::Idle,
        RunStatus::Running => SessionStatus::Running,
        RunStatus::Completed => SessionStatus::Completed,
        RunStatus::Cancelled => SessionStatus::Cancelled,
        RunStatus::Failed => SessionStatus::Failed,
    }
}

fn latest_context_window_from_history_items(
    items: &[HistoryItem],
) -> Option<crate::context::ContextWindowTokenStatus> {
    items.iter().rev().find_map(|item| match &item.payload {
        HistoryItemPayload::RequestDiagnostics { diagnostics } => {
            diagnostics.context_window.clone()
        }
        _ => None,
    })
}

fn truncate_for_search(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    let shortened = value.chars().take(keep).collect::<String>();
    format!("{shortened}…")
}

pub(crate) fn initial_provider_models(config: &ResolvedConfig) -> Vec<String> {
    ensure_current_model(Vec::new(), &config.model.model)
}

pub(crate) fn initial_provider_model_infos(config: &ResolvedConfig) -> Vec<ProviderModelInfo> {
    ensure_current_model_infos(Vec::new(), config)
}

pub(crate) fn ensure_current_model(mut models: Vec<String>, current_model: &str) -> Vec<String> {
    let current_model = current_model.trim();
    if !current_model.is_empty() && !models.iter().any(|model| model == current_model) {
        models.insert(0, current_model.to_string());
    }
    models
}

pub(crate) fn ensure_current_model_infos(
    mut infos: Vec<ProviderModelInfo>,
    config: &ResolvedConfig,
) -> Vec<ProviderModelInfo> {
    if !infos.iter().any(|info| info.id == config.model.model) {
        infos.insert(0, provider_info_from_config(config));
    }
    infos
}

fn ensure_current_model_info(
    infos: Vec<ProviderModelInfo>,
    config: &ResolvedConfig,
) -> Vec<ProviderModelInfo> {
    ensure_current_model_infos(infos, config)
}

fn provider_info_from_config(config: &ResolvedConfig) -> ProviderModelInfo {
    ProviderModelInfo {
        id: config.model.model.clone(),
        display_name: Some(config.model.model.clone()),
        context_window: Some(config.model.context_window),
        max_output_tokens: None,
        supports_images: Some(config.model.supports_images),
        supports_tools: Some(config.model.supports_tools),
        supports_reasoning: None,
        max_parallel_predictions: Some(config.model.max_parallel_predictions),
        load_state: ProviderModelLoadState::Unknown,
        source: "config".to_string(),
    }
}

pub fn provider_model_summary(info: &ProviderModelInfo) -> String {
    let mut parts = Vec::new();
    if let Some(context) = info.context_window {
        parts.push(format!("ctx={context}"));
    }
    if let Some(max_output) = info.max_output_tokens {
        parts.push(format!("max_pred={max_output}"));
    }
    if let Some(vision) = info.supports_images {
        parts.push(if vision { "vision" } else { "text-only" }.to_string());
    }
    if let Some(tools) = info.supports_tools {
        parts.push(if tools { "tools" } else { "no-tools" }.to_string());
    }
    if let Some(reasoning) = info.supports_reasoning {
        if reasoning {
            parts.push("reasoning".to_string());
        }
    }
    if let Some(parallel) = info.max_parallel_predictions.filter(|value| *value > 1) {
        parts.push(format!("parallel={parallel}"));
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use super::*;
    use crate::config::AccessMode;
    use crate::desktop::models::{DesktopCommandRow, DesktopProjectRow, DesktopSessionRow};
    use crate::session::ProjectId;
    use crate::session::{
        CanonicalHistoryPage, CanonicalTurnPage, RequestDiagnosticsPart, SessionModelParameters,
        SessionRecord,
    };

    fn snapshot(
        session_rows: Vec<DesktopSessionRow>,
        selected_session_index: usize,
    ) -> DesktopSnapshot {
        let project_id = ProjectId::new();
        DesktopSnapshot {
            workspace_path: "C:/workspace".to_string(),
            provider_label: "provider".to_string(),
            model_label: "model".to_string(),
            command_rows: Vec::new(),
            project_rows: vec![DesktopProjectRow {
                project_id,
                label: "workspace".to_string(),
                path: "C:/workspace".to_string(),
            }],
            selected_project_index: 0,
            chat_session_rows: Vec::new(),
            session_rows,
            session_details: Vec::new(),
            selected_session_index,
        }
    }

    fn session_row(session_id: SessionId, title: &str, status: SessionStatus) -> DesktopSessionRow {
        DesktopSessionRow::from_parts(session_id, title, status)
    }

    fn session_record(session_id: SessionId) -> SessionRecord {
        SessionRecord {
            id: session_id,
            project_id: ProjectId::new(),
            title: "opened".to_string(),
            status: SessionStatus::Completed,
            cwd: Utf8PathBuf::from("C:/workspace"),
            model: "model".to_string(),
            base_url: "http://local".to_string(),
            access_mode: AccessMode::FullAccess,
            model_parameters: SessionModelParameters::default(),
            provider_connection: None,
            session_settings_revision: 0,
            created_at_ms: 1,
            updated_at_ms: 2,
            completed_at_ms: Some(2),
        }
    }

    fn context_window_status(
        active_context_tokens: u32,
    ) -> crate::context::ContextWindowTokenStatus {
        crate::context::ContextWindowTokenStatus {
            source: crate::context::ActiveContextTokenSource::FullPreparedRequestEstimate,
            active_context_tokens,
            full_context_window_limit: 131_072,
            configured_max_output_tokens: None,
            overflow_margin_tokens: 1_024,
            tokens_until_limit: 121_856 - i64::from(active_context_tokens),
            token_limit_reached: false,
        }
    }

    fn canonical_read(
        session: &SessionRecord,
        history_items: Vec<HistoryItem>,
        turn_items: Vec<crate::protocol::TurnItem>,
    ) -> CanonicalSessionRead {
        let latest_turn_id = history_items
            .iter()
            .rev()
            .find_map(HistoryItem::turn_id)
            .or_else(|| turn_items.last().map(|item| item.turn_id));
        CanonicalSessionRead {
            session: session.clone(),
            history: CanonicalHistoryPage {
                session: session.clone(),
                offset: 0,
                limit: usize::MAX,
                total: history_items.len(),
                has_more: false,
                items: history_items,
            },
            turns: CanonicalTurnPage {
                session: session.clone(),
                offset: 0,
                limit: 50,
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
        }
    }

    fn turn_item(
        session_id: SessionId,
        turn_id: crate::protocol::TurnId,
        sequence_no: i64,
        payload: crate::protocol::TurnItemPayload,
    ) -> crate::protocol::TurnItem {
        crate::protocol::TurnItem {
            id: crate::protocol::TurnItemId::new(),
            session_id,
            turn_id,
            source_item_id: None,
            sequence_no,
            payload,
        }
    }

    fn assert_nested_file_change_projection(state: &mut DesktopState) {
        let session_id = SessionId::new();
        let turn_id = crate::protocol::TurnId::new();
        let tool_call_id = crate::session::ToolCallId::new();
        let change_id = crate::session::ChangeId::new();
        let stored_path = Utf8PathBuf::from("bbb/ccc/file.rs");
        let mut session = session_record(session_id);
        session.cwd = Utf8PathBuf::from("C:/workspace/aaa/bbb");
        let canonical_change = turn_item(
            session_id,
            turn_id,
            1,
            TurnItemPayload::FileChange {
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
        );
        let read = canonical_read(&session, Vec::new(), vec![canonical_change]);

        state.load_open_session(&read);

        assert_eq!(
            state
                .app_state
                .transcript_entries
                .first()
                .expect("canonical file change")
                .body,
            "Updated ccc/file.rs"
        );

        state
            .app_state
            .apply_run_event(&crate::session::RunEvent::FileChangesRecorded {
                tool_call_id,
                changes: vec![crate::edit::ChangeSummary {
                    change_id,
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(stored_path.clone()),
                    path_after: Some(stored_path.clone()),
                }],
            });

        assert_eq!(
            state
                .app_state
                .transcript_entries
                .last()
                .expect("live file change")
                .body,
            "Updated ccc/file.rs"
        );
        assert_eq!(
            stored_path,
            Utf8PathBuf::from("bbb/ccc/file.rs"),
            "project-root-relative stored coordinates remain unchanged"
        );
    }

    fn terminal_event(
        session_id: SessionId,
        outcome: crate::protocol::TurnTerminalOutcome,
    ) -> crate::session::RunEvent {
        crate::session::RunEvent::TurnTerminal {
            session_id,
            terminal: Box::new(crate::session::DurableTurnTerminal {
                outcome,
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            }),
        }
    }

    fn diagnostics_history_item(
        session: &SessionRecord,
        context_window: Option<crate::context::ContextWindowTokenStatus>,
    ) -> HistoryItem {
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            scope: crate::protocol::HistoryScope::Turn {
                turn_id: crate::protocol::TurnId::new(),
            },
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::RequestDiagnostics {
                diagnostics: RequestDiagnosticsPart {
                    provider: "openai_compat".to_string(),
                    model_name: session.model.clone(),
                    base_url: session.base_url.clone(),
                    request_timeout_ms: 30_000,
                    stream_idle_timeout_ms: 30_000,
                    configured_max_output_tokens: Some(8_192),
                    effective_max_output_tokens: Some(8_192),
                    output_budget_reason: None,
                    supports_tools: Some(true),
                    supports_reasoning: Some(false),
                    supports_images: Some(false),
                    system_prompt_chars: 0,
                    tool_count: 0,
                    tool_choice: Some("auto".to_string()),
                    parallel_tool_calls: Some(false),
                    provider_message_count: 0,
                    image_count: 0,
                    image_bytes: 0,
                    tool_names: Vec::new(),
                    tool_schemas: Vec::new(),
                    wire: None,
                    context_window,
                    messages: Vec::new(),
                },
            },
        }
    }

    #[tokio::test]
    async fn canonical_live_refresh_projects_progress_beyond_history_page_and_replay() {
        use crate::runtime::RunEventSink;
        use crate::session::{ProjectRepository, SessionRepository};
        struct NullSink;
        impl RunEventSink for NullSink {
            fn emit(
                &mut self,
                _: crate::session::RunEvent,
            ) -> Result<(), crate::error::RuntimeError> {
                Ok(())
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).unwrap();
        let paths = crate::storage::StoragePaths {
            data_dir: root.clone(),
            database_path: root.join("db.sqlite3"),
            truncation_dir: root.join("output"),
        };
        let sqlite = crate::storage::SqliteStore::open(&paths).unwrap();
        sqlite.migrate().unwrap();
        let store = crate::storage::StoreBundle::new(sqlite);
        let project_id = ProjectId::new();
        store
            .project_repo()
            .upsert_project(project_id, &root, "progress", "none")
            .await
            .unwrap();
        let session = store
            .session_repo()
            .create_session(crate::session::NewSession {
                project_id,
                title: "progress".into(),
                cwd: root,
                model: "fixture".into(),
                base_url: "http://127.0.0.1:9/v1".into(),
                access_mode: AccessMode::Default,
                provider_connection: None,
            })
            .await
            .unwrap();
        let turn_id = crate::protocol::TurnId::new();
        let admitted = store
            .session_repo()
            .admit_session_turn(session.id, turn_id)
            .await
            .unwrap()
            .unwrap();
        let service = crate::session::SessionService::new(store.clone());
        let initial = service
            .canonical_session_read(session.id, 0, 1, 0, 1)
            .await
            .unwrap();
        let mut state = DesktopState::new(
            snapshot(
                vec![session_row(
                    session.id,
                    &session.title,
                    SessionStatus::Running,
                )],
                0,
            ),
            ResolvedConfig::default(),
        );
        state.load_open_session(&initial);
        assert_eq!(state.app_state.progress.model_requests, 0);
        let HistoryItemPayload::RequestDiagnostics { diagnostics } =
            diagnostics_history_item(&session, None).payload
        else {
            unreachable!()
        };
        let mut null = NullSink;
        let mut sink = crate::protocol::ProtocolRecordingSink::new(
            store.protocol_event_store(),
            Some(session.id),
            turn_id,
            &mut null,
        )
        .with_admission_id(admitted.admission_id);
        for _ in 0..130 {
            sink.emit(crate::session::RunEvent::ModelRequestPrepared {
                session_id: session.id,
                diagnostics: diagnostics.clone(),
            })
            .unwrap();
        }
        let call_id = crate::session::ToolCallId::new();
        store
            .session_repo()
            .record_model_response_with_protocol_bundle(
                session.id,
                admitted.admission_id,
                turn_id,
                crate::storage::session_repo::ModelResponseWrite {
                    response_id: crate::protocol::ModelResponseId::new(),
                    assistant_text: Some("remote work is running".into()),
                    assistant_protocol_sequence_no: None,
                    tool_calls: vec![crate::storage::session_repo::PendingToolCallWrite {
                        id: call_id,
                        model_call_id: "remote-status".into(),
                        tool_name: "mcp_call".into(),
                        arguments_json: "{}".into(),
                        protocol_sequence_no: None,
                    }],
                },
            )
            .await
            .unwrap();
        let refreshed = service
            .canonical_session_read(session.id, 0, 1, 0, 32)
            .await
            .unwrap();
        assert_eq!(refreshed.history.items.len(), 1);
        assert!(refreshed.history.has_more);
        assert!(refreshed.turns.items.iter().any(|item| matches!(&item.payload, TurnItemPayload::ToolStatus { call_id: id, .. } if *id == call_id)));
        state.refresh_open_session_projection(&refreshed);
        assert!(state.open_session.as_ref().unwrap().turn_items().iter().any(
            |item| matches!(&item.payload, TurnItemPayload::AgentMessage { text } if text == "remote work is running")
        ), "the canonical history was accepted before progress is checked");
        assert_eq!(state.app_state.progress.model_requests, 130);
        assert_eq!(state.app_state.progress.tool_calls_started, 1);
        assert!(
            state
                .selected_detail()
                .tool_status_text
                .contains("mcp_call")
        );
        state.refresh_open_session_projection(&refreshed);
        assert_eq!(
            state.app_state.progress.model_requests, 130,
            "replayed canonical pages do not double count"
        );
        assert_eq!(state.app_state.progress.tool_calls_started, 1);
        state.refresh_open_session_projection(&initial);
        assert_eq!(
            state.app_state.progress.model_requests, 130,
            "an older same-turn page cannot roll progress back"
        );
        assert_eq!(state.app_state.progress.tool_calls_started, 1);

        let live_response = crate::protocol::ModelResponseId::new();
        state.apply_run_event(&crate::session::RunEvent::TextDelta {
            response_id: live_response,
            delta: "uncommitted live suffix".into(),
        });
        state.app_state.progress.current_phase = RunProgressPhase::StopRequested;
        state.app_state.progress.active_step = "stop is settling".into();
        store
            .session_repo()
            .complete_tool_call_with_protocol_bundle(
                session.id,
                admitted.admission_id,
                call_id,
                crate::tool::ToolName::McpCall,
                "remote reply",
                serde_json::json!({}),
                "receiver result",
                None,
                turn_id,
                None,
            )
            .await
            .unwrap()
            .unwrap();
        let completed = service
            .canonical_session_read(session.id, 0, 1, 2, 1)
            .await
            .unwrap();
        state.refresh_open_session_projection(&completed);
        assert_eq!(state.app_state.progress.tool_calls_started, 1);
        assert_eq!(
            state.app_state.progress.tool_calls_completed, 1,
            "latest lifecycle replaces pending without counting a second call"
        );
        assert_eq!(state.app_state.progress.model_requests, 130);
        assert_eq!(
            state.app_state.progress.current_phase,
            RunProgressPhase::StopRequested
        );
        assert!(
            state
                .app_state
                .transcript_entries
                .iter()
                .any(|entry| entry.body == "uncommitted live suffix")
        );
        state.refresh_open_session_projection(&refreshed);
        assert_eq!(state.app_state.progress.tool_calls_completed, 1);

        let mut reopened = DesktopState::new(
            snapshot(
                vec![session_row(
                    session.id,
                    &session.title,
                    SessionStatus::Running,
                )],
                0,
            ),
            ResolvedConfig::default(),
        );
        reopened.load_open_session(&completed);
        assert_eq!(reopened.app_state.progress.model_requests, 130);
        assert_eq!(reopened.app_state.progress.tool_calls_started, 1);
        assert_eq!(reopened.app_state.progress.tool_calls_completed, 1);
        assert!(
            !serde_json::to_string(&completed)
                .unwrap()
                .contains("active_turn_progress"),
            "the process-local projection does not alter stored/exported contracts"
        );
    }

    #[test]
    fn opening_a_canonical_terminal_session_reconciles_its_stale_navigation_row() {
        let session_id = SessionId::new();
        let stale_turn_id = crate::protocol::TurnId::new();
        let stale_row = DesktopSessionRow::from_parts_with_loaded(
            session_id,
            "new chat",
            SessionStatus::Running,
            crate::session::LoadedSessionStatus::Active,
            Some(stale_turn_id),
            Some(3),
            1,
            1,
            1,
        );
        let session = session_record(session_id);
        let read = canonical_read(&session, Vec::new(), Vec::new());
        let mut state = DesktopState::new(snapshot(vec![stale_row], 0), ResolvedConfig::default());

        state.load_open_session(&read);

        let row = &state.snapshot.session_rows[0];
        assert_eq!(row.title, session.title);
        assert_eq!(row.status, SessionStatus::Completed);
        assert_eq!(row.loaded_status, crate::session::LoadedSessionStatus::Idle);
        assert_eq!(row.active_turn_id, None);
        assert_eq!(row.active_turn_sequence_no, None);
        assert_eq!(row.pending_permission_requests, 0);
        assert_eq!(row.pending_user_input_requests, 0);
        assert_eq!(state.selected_session_title(), row.label);
        assert!(!state.selected_session_title().contains("[実行中]"));
    }

    #[test]
    fn canonical_projection_refresh_reconciles_the_current_session_title() {
        let session_id = SessionId::new();
        let session = session_record(session_id);
        let read = canonical_read(&session, Vec::new(), Vec::new());
        let mut state = DesktopState::new(
            snapshot(
                vec![DesktopSessionRow::from_parts(
                    session_id,
                    &session.title,
                    SessionStatus::Completed,
                )],
                0,
            ),
            ResolvedConfig::default(),
        );
        state.load_open_session(&read);
        state.app_state.current_session_title = "new chat".to_string();

        state.refresh_open_session_projection(&read);

        assert_eq!(state.current_session_label(), session.title);
        assert_eq!(
            state.selected_session_title(),
            state.snapshot.session_rows[0].label
        );
    }

    #[test]
    fn stale_placeholder_refresh_cannot_reverse_a_live_title_event() {
        let session_id = SessionId::new();
        let mut placeholder_session = session_record(session_id);
        placeholder_session.title = "新規チャット".to_string();
        let stale_read = canonical_read(&placeholder_session, Vec::new(), Vec::new());
        let mut state = DesktopState::new(
            snapshot(
                vec![DesktopSessionRow::from_parts(
                    session_id,
                    &placeholder_session.title,
                    SessionStatus::Completed,
                )],
                0,
            ),
            ResolvedConfig::default(),
        );
        state.load_open_session(&stale_read);
        state.apply_run_event(&crate::session::RunEvent::SessionTitleUpdated {
            session_id,
            title: "canonical title".to_string(),
        });

        state.refresh_open_session_projection(&stale_read);

        assert_eq!(state.current_session_label(), "canonical title");
        assert_eq!(state.snapshot.session_rows[0].title, "canonical title");
        assert!(
            state.snapshot.session_rows[0]
                .label
                .starts_with("canonical title [完了]")
        );
    }

    #[test]
    fn current_and_live_refreshes_preserve_the_expanded_turn_suffix() {
        let session_id = SessionId::new();
        let session = session_record(session_id);
        let first_turn = crate::protocol::TurnId::new();
        let second_turn = crate::protocol::TurnId::new();
        let third_turn = crate::protocol::TurnId::new();
        let items = vec![
            turn_item(
                session_id,
                first_turn,
                1,
                crate::protocol::TurnItemPayload::UserMessage {
                    text: "retained old request".to_string(),
                },
            ),
            turn_item(
                session_id,
                first_turn,
                2,
                crate::protocol::TurnItemPayload::AgentMessage {
                    text: "retained old answer".to_string(),
                },
            ),
            turn_item(
                session_id,
                second_turn,
                3,
                crate::protocol::TurnItemPayload::UserMessage {
                    text: "current request".to_string(),
                },
            ),
            turn_item(
                session_id,
                second_turn,
                4,
                crate::protocol::TurnItemPayload::AgentMessage {
                    text: "current answer".to_string(),
                },
            ),
            turn_item(
                session_id,
                third_turn,
                5,
                crate::protocol::TurnItemPayload::UserMessage {
                    text: "latest request".to_string(),
                },
            ),
            turn_item(
                session_id,
                third_turn,
                6,
                crate::protocol::TurnItemPayload::AgentMessage {
                    text: "latest answer".to_string(),
                },
            ),
        ];
        let mut expanded = canonical_read(&session, Vec::new(), items[..4].to_vec());
        expanded.turns.limit = 2;
        expanded.turns.total = 4;
        let mut current_refresh = canonical_read(&session, Vec::new(), items[3..5].to_vec());
        current_refresh.turns.offset = 3;
        current_refresh.turns.limit = 2;
        current_refresh.turns.total = 5;
        let mut live_refresh = canonical_read(&session, Vec::new(), items[4..].to_vec());
        live_refresh.turns.offset = 4;
        live_refresh.turns.limit = 2;
        live_refresh.turns.total = 6;
        let mut state = DesktopState::new(
            snapshot(
                vec![session_row(
                    session_id,
                    &session.title,
                    SessionStatus::Completed,
                )],
                0,
            ),
            ResolvedConfig::default(),
        );

        state.load_open_session(&expanded);
        state.load_open_session_preserving_history(&current_refresh);

        let current_items = state
            .open_session
            .as_ref()
            .expect("open session after current refresh")
            .turn_items();
        assert_eq!(current_items.len(), 5);
        assert_eq!(current_items[0].id, items[0].id);
        assert_eq!(current_items[4].id, items[4].id);
        assert!(
            state
                .app_state
                .transcript_entries
                .iter()
                .any(|entry| entry.body.contains("retained old request"))
        );

        state.refresh_open_session_projection(&live_refresh);

        let live_view = state
            .open_session
            .as_ref()
            .expect("open session after live refresh");
        assert_eq!(live_view.turn_items().len(), 6);
        assert_eq!(live_view.turn_items()[0].id, items[0].id);
        assert_eq!(live_view.turn_items()[5].id, items[5].id);
        assert!(
            live_view
                .stored_detail()
                .transcript_rows
                .iter()
                .any(|row| row.body.contains("retained old request"))
        );
        assert!(
            live_view
                .stored_detail()
                .transcript_rows
                .iter()
                .any(|row| row.body.contains("latest answer"))
        );
    }

    #[test]
    fn active_history_prepend_does_not_replace_the_live_runtime_suffix() {
        let session_id = SessionId::new();
        let mut session = session_record(session_id);
        session.status = SessionStatus::Running;
        session.completed_at_ms = None;
        let turn_id = crate::protocol::TurnId::new();
        let items = vec![
            turn_item(
                session_id,
                turn_id,
                1,
                crate::protocol::TurnItemPayload::UserMessage {
                    text: "older canonical request".to_string(),
                },
            ),
            turn_item(
                session_id,
                turn_id,
                2,
                crate::protocol::TurnItemPayload::AgentMessage {
                    text: "stored running answer".to_string(),
                },
            ),
        ];
        let mut suffix = canonical_read(&session, Vec::new(), items[1..].to_vec());
        suffix.turns.offset = 1;
        suffix.turns.total = 2;
        suffix.active_turn_id = Some(turn_id);
        let mut prepend = canonical_read(&session, Vec::new(), items);
        prepend.active_turn_id = Some(turn_id);
        let mut state = DesktopState::new(
            snapshot(
                vec![session_row(
                    session_id,
                    &session.title,
                    SessionStatus::Running,
                )],
                0,
            ),
            ResolvedConfig::default(),
        );
        state.load_open_session(&suffix);
        state
            .app_state
            .transcript_entries
            .push(crate::tui::state::TranscriptEntry {
                kind: crate::tui::state::TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "not-yet-reloaded live suffix".to_string(),
                response_id: None,
                tool_call_id: None,
            });

        assert!(state.merge_open_session_history(&prepend));

        assert!(
            state
                .app_state
                .transcript_entries
                .iter()
                .any(|entry| entry.body == "not-yet-reloaded live suffix")
        );
        assert!(
            state
                .open_session
                .as_ref()
                .expect("merged open session")
                .turn_items()
                .iter()
                .any(|item| item.id == prepend.turns.items[0].id)
        );
    }

    #[test]
    fn stale_running_prepend_cannot_reverse_an_observed_terminal_event() {
        let session_id = SessionId::new();
        let mut running_session = session_record(session_id);
        running_session.status = SessionStatus::Running;
        running_session.completed_at_ms = None;
        let turn_id = crate::protocol::TurnId::new();
        let items = vec![
            turn_item(
                session_id,
                turn_id,
                1,
                crate::protocol::TurnItemPayload::UserMessage {
                    text: "request".to_string(),
                },
            ),
            turn_item(
                session_id,
                turn_id,
                2,
                crate::protocol::TurnItemPayload::AgentMessage {
                    text: "answer".to_string(),
                },
            ),
        ];
        let mut suffix = canonical_read(&running_session, Vec::new(), items[1..].to_vec());
        suffix.turns.offset = 1;
        suffix.turns.total = 2;
        suffix.active_turn_id = Some(turn_id);
        let mut stale_prepend = canonical_read(&running_session, Vec::new(), items);
        stale_prepend.active_turn_id = Some(turn_id);
        let mut state = DesktopState::new(
            snapshot(
                vec![session_row(
                    session_id,
                    &running_session.title,
                    SessionStatus::Running,
                )],
                0,
            ),
            ResolvedConfig::default(),
        );
        state.load_open_session(&suffix);
        state.apply_run_event(&terminal_event(
            session_id,
            crate::protocol::TurnTerminalOutcome::Completed,
        ));

        assert!(state.merge_open_session_history(&stale_prepend));

        assert_eq!(state.app_state.run_status, RunStatus::Completed);
        assert_eq!(
            state.snapshot.session_rows[0].loaded_status,
            crate::session::LoadedSessionStatus::Idle,
        );
        assert!(!state.snapshot.session_rows[0].label.contains("[実行中]"));
    }

    #[test]
    fn canonical_terminal_refresh_settles_an_open_running_current_session() {
        let session_id = SessionId::new();
        let mut running_session = session_record(session_id);
        running_session.status = SessionStatus::Running;
        running_session.completed_at_ms = None;
        let turn_id = crate::protocol::TurnId::new();
        let user_item = turn_item(
            session_id,
            turn_id,
            1,
            crate::protocol::TurnItemPayload::UserMessage {
                text: "stop the rejoined run".to_string(),
            },
        );
        let mut running = canonical_read(&running_session, Vec::new(), vec![user_item.clone()]);
        running.active_turn_id = Some(turn_id);
        let mut state = DesktopState::new(
            snapshot(
                vec![session_row(
                    session_id,
                    &running_session.title,
                    SessionStatus::Running,
                )],
                0,
            ),
            ResolvedConfig::default(),
        );
        state.load_open_session(&running);
        state
            .app_state
            .transcript_entries
            .push(crate::tui::state::TranscriptEntry {
                kind: crate::tui::state::TranscriptKind::Assistant,
                title: "Assistant".to_string(),
                body: "live projection remains intact".to_string(),
                response_id: None,
                tool_call_id: None,
            });

        let mut cancelled_session = running_session.clone();
        cancelled_session.status = SessionStatus::Cancelled;
        cancelled_session.updated_at_ms += 1;
        cancelled_session.completed_at_ms = Some(cancelled_session.updated_at_ms);
        let terminal_item = turn_item(
            session_id,
            turn_id,
            2,
            crate::protocol::TurnItemPayload::Terminal {
                outcome: crate::protocol::TurnTerminalOutcome::Interrupted {
                    cause: crate::protocol::TurnInterruptionCause::UserStop,
                },
            },
        );
        let terminal = canonical_read(
            &cancelled_session,
            Vec::new(),
            vec![user_item, terminal_item],
        );

        assert!(state.load_open_session_preserving_history(&terminal));

        assert_eq!(state.app_state.run_status, RunStatus::Cancelled);
        assert_eq!(
            state.app_state.interruption_cause,
            Some(crate::protocol::TurnInterruptionCause::UserStop)
        );
        assert_eq!(state.status_code, DesktopStatusCode::UserStopped);
        assert_eq!(
            state.app_state.progress.current_phase,
            RunProgressPhase::Terminal
        );
        assert!(!state.is_busy());
        assert!(
            state
                .app_state
                .transcript_entries
                .iter()
                .any(|entry| entry.body == "live projection remains intact")
        );
        assert_eq!(
            state
                .open_session
                .as_ref()
                .expect("settled open session")
                .session()
                .status,
            SessionStatus::Cancelled
        );
        assert!(!state.snapshot.session_rows[0].label.contains("[実行中]"));
    }

    #[test]
    fn canonical_terminal_refresh_reconciles_the_latest_turn_tool_projection() {
        let session_id = SessionId::new();
        let turn_id = crate::protocol::TurnId::new();
        let call_id = crate::session::ToolCallId::new();
        let mut running_session = session_record(session_id);
        running_session.status = SessionStatus::Running;
        running_session.completed_at_ms = None;
        let user_item = turn_item(
            session_id,
            turn_id,
            1,
            TurnItemPayload::UserMessage {
                text: "read the current time".to_string(),
            },
        );
        let mut running = canonical_read(&running_session, Vec::new(), vec![user_item.clone()]);
        running.active_turn_id = Some(turn_id);
        running.active_turn_sequence_no = Some(1);
        let mut state = DesktopState::new(
            snapshot(
                vec![session_row(
                    session_id,
                    &running_session.title,
                    SessionStatus::Running,
                )],
                0,
            ),
            ResolvedConfig::default(),
        );
        state.load_open_session(&running);
        state.app_state.progress.compactions = 3;
        state.apply_run_summary(crate::session::RunSummary::from_terminal(
            session_id,
            turn_id,
            crate::session::DurableTurnTerminal {
                outcome: crate::protocol::TurnTerminalOutcome::Completed,
                final_response_id: None,
                tool_call_count: 1,
                failed_tool_count: 0,
                change_count: 0,
                metrics: crate::session::RunMetrics {
                    model_request_count: 2,
                    ..Default::default()
                },
            },
        ));
        assert_eq!(state.app_state.progress.tool_calls_completed, 0);

        let mut completed_session = running_session.clone();
        completed_session.status = SessionStatus::Completed;
        completed_session.updated_at_ms += 1;
        completed_session.completed_at_ms = Some(completed_session.updated_at_ms);
        let completed = canonical_read(
            &completed_session,
            Vec::new(),
            vec![
                user_item,
                turn_item(
                    session_id,
                    turn_id,
                    2,
                    TurnItemPayload::ToolStatus {
                        call_id,
                        tool: crate::tool::ToolName::CurrentTime,
                        status: crate::protocol::ToolLifecycleStatus::Completed,
                        title: "Current time".to_string(),
                        summary: "local: 2026-08-25T10:11:57+09:00".to_string(),
                    },
                ),
                turn_item(
                    session_id,
                    turn_id,
                    3,
                    TurnItemPayload::AgentMessage {
                        text: "local=2026-08-25T10:11:57+09:00".to_string(),
                    },
                ),
                turn_item(
                    session_id,
                    turn_id,
                    4,
                    TurnItemPayload::Terminal {
                        outcome: crate::protocol::TurnTerminalOutcome::Completed,
                    },
                ),
            ],
        );

        assert!(state.load_open_session_preserving_history(&completed));

        let detail = state.selected_detail();
        assert!(detail.progress_text.contains("モデル要求: 2"));
        assert!(detail.progress_text.contains("ツール: 1件開始 / 1件完了"));
        assert!(detail.tool_status_text.contains("[完了] Current time"));
        assert!(!detail.tool_status_text.contains("実行履歴はまだありません"));
        assert_eq!(state.app_state.progress.compactions, 3);
        assert_eq!(
            state.app_state.progress.current_phase,
            RunProgressPhase::Terminal
        );
    }

    #[test]
    fn terminal_reload_rebuilds_compaction_after_an_older_page_is_merged() {
        let session_id = SessionId::new();
        let turn_id = crate::protocol::TurnId::new();
        let session = session_record(session_id);
        let user_item = turn_item(
            session_id,
            turn_id,
            1,
            TurnItemPayload::UserMessage {
                text: "inspect the repository".to_string(),
            },
        );
        let compaction_item = turn_item(
            session_id,
            turn_id,
            2,
            TurnItemPayload::ContextCompaction {
                summary: "retained repository evidence".to_string(),
            },
        );
        let assistant_item = turn_item(
            session_id,
            turn_id,
            3,
            TurnItemPayload::AgentMessage {
                text: "done".to_string(),
            },
        );
        let terminal_item = turn_item(
            session_id,
            turn_id,
            4,
            TurnItemPayload::Terminal {
                outcome: crate::protocol::TurnTerminalOutcome::Completed,
            },
        );
        let mut latest_page =
            canonical_read(&session, Vec::new(), vec![assistant_item, terminal_item]);
        latest_page.turns.offset = 2;
        latest_page.turns.limit = 2;
        latest_page.turns.total = 4;
        latest_page.latest_turn_id = Some(turn_id);
        let mut state = DesktopState::new(
            snapshot(
                vec![session_row(
                    session_id,
                    &session.title,
                    SessionStatus::Completed,
                )],
                0,
            ),
            ResolvedConfig::default(),
        );

        state.load_open_session(&latest_page);
        assert_eq!(state.app_state.progress.compactions, 0);

        let mut older_page = canonical_read(&session, Vec::new(), vec![user_item, compaction_item]);
        older_page.turns.offset = 0;
        older_page.turns.limit = 2;
        older_page.turns.total = 4;
        older_page.turns.has_more = true;
        older_page.latest_turn_id = Some(turn_id);

        assert!(state.load_open_session_preserving_history(&older_page));
        assert_eq!(state.app_state.progress.compactions, 1);
        assert!(state.selected_detail().progress_text.contains("圧縮: 1"));
    }

    #[test]
    fn stale_terminal_page_cannot_reconcile_a_newer_terminal_summary() {
        let session_id = SessionId::new();
        let stale_turn_id = crate::protocol::TurnId::new();
        let current_turn_id = crate::protocol::TurnId::new();
        let session = session_record(session_id);
        let mut stale = canonical_read(
            &session,
            Vec::new(),
            vec![turn_item(
                session_id,
                stale_turn_id,
                1,
                TurnItemPayload::Terminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Completed,
                },
            )],
        );
        stale.latest_turn_id = Some(current_turn_id);
        let mut state = DesktopState::new(
            snapshot(
                vec![session_row(
                    session_id,
                    &session.title,
                    SessionStatus::Completed,
                )],
                0,
            ),
            ResolvedConfig::default(),
        );
        state.load_open_session(&stale);
        state.apply_run_summary(crate::session::RunSummary::from_terminal(
            session_id,
            current_turn_id,
            crate::session::DurableTurnTerminal {
                outcome: crate::protocol::TurnTerminalOutcome::Completed,
                final_response_id: None,
                tool_call_count: 1,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            },
        ));
        state.app_state.progress.tool_calls_completed = 41;

        assert!(!state.reconcile_current_terminal_tool_projection());
        assert_eq!(state.app_state.progress.tool_calls_completed, 41);
        assert!(state.app_state.tool_statuses.is_empty());
    }

    #[test]
    fn noncontiguous_terminal_refresh_keeps_the_expanded_prefix() {
        let session_id = SessionId::new();
        let session = session_record(session_id);
        let old_turn = crate::protocol::TurnId::new();
        let terminal_turn = crate::protocol::TurnId::new();
        let prefix_item = turn_item(
            session_id,
            old_turn,
            1,
            crate::protocol::TurnItemPayload::UserMessage {
                text: "expanded retained request".to_string(),
            },
        );
        let terminal_item = turn_item(
            session_id,
            terminal_turn,
            4,
            crate::protocol::TurnItemPayload::Terminal {
                outcome: crate::protocol::TurnTerminalOutcome::Completed,
            },
        );
        let mut expanded = canonical_read(&session, Vec::new(), vec![prefix_item.clone()]);
        expanded.turns.total = 3;
        expanded.turns.has_more = true;
        let mut terminal = canonical_read(&session, Vec::new(), vec![terminal_item]);
        terminal.turns.offset = 3;
        terminal.turns.total = 4;
        let mut state = DesktopState::new(
            snapshot(
                vec![session_row(
                    session_id,
                    &session.title,
                    SessionStatus::Completed,
                )],
                0,
            ),
            ResolvedConfig::default(),
        );
        state.load_open_session(&expanded);

        assert!(!state.load_open_session_preserving_history(&terminal));

        let view = state.open_session.as_ref().expect("preserved open session");
        assert_eq!(view.turn_items()[0].id, prefix_item.id);
        assert_eq!(view.stored_detail().turn_page_total, 4);
        assert!(
            view.stored_detail()
                .transcript_rows
                .iter()
                .any(|row| row.body.contains("expanded retained request"))
        );
    }

    #[test]
    fn next_turn_page_offset_advances_from_the_merged_loaded_end() {
        let session_id = SessionId::new();
        let session = session_record(session_id);
        let turn_id = crate::protocol::TurnId::new();
        let items = (1..=6)
            .map(|sequence_no| {
                turn_item(
                    session_id,
                    turn_id,
                    sequence_no,
                    crate::protocol::TurnItemPayload::AgentMessage {
                        text: format!("message {sequence_no}"),
                    },
                )
            })
            .collect::<Vec<_>>();
        let mut first = canonical_read(&session, Vec::new(), items[..2].to_vec());
        first.turns.limit = 2;
        first.turns.total = 6;
        first.turns.has_more = true;
        let mut second = canonical_read(&session, Vec::new(), items[2..4].to_vec());
        second.turns.offset = 2;
        second.turns.limit = 2;
        second.turns.total = 6;
        second.turns.has_more = true;
        let mut state = DesktopState::new(
            snapshot(
                vec![session_row(
                    session_id,
                    &session.title,
                    SessionStatus::Completed,
                )],
                0,
            ),
            ResolvedConfig::default(),
        );

        state.load_open_session(&first);
        assert_eq!(state.next_turn_page_offset(), Some(2));

        assert!(state.merge_open_session_history(&second));

        assert_eq!(state.next_turn_page_offset(), Some(4));
    }

    #[test]
    fn replace_snapshot_falls_back_from_deleted_selection_to_open_session() {
        let deleted = SessionId::new();
        let open = SessionId::new();
        let mut state = DesktopState::new(
            snapshot(
                vec![
                    session_row(deleted, "deleted", SessionStatus::Running),
                    session_row(open, "open", SessionStatus::Running),
                ],
                0,
            ),
            ResolvedConfig::default(),
        );
        state.app_state.current_session_id = Some(open);

        state.replace_snapshot(snapshot(
            vec![session_row(open, "open", SessionStatus::Running)],
            0,
        ));

        assert_eq!(state.selected_session_id(), Some(open));
    }

    #[test]
    fn load_open_session_restores_latest_context_window_from_canonical_history() {
        let session_id = SessionId::new();
        let session = session_record(session_id);
        let status = context_window_status(2_100);
        let read = canonical_read(
            &session,
            vec![diagnostics_history_item(&session, Some(status.clone()))],
            Vec::new(),
        );
        let mut state = DesktopState::new(
            snapshot(
                vec![session_row(session_id, "opened", SessionStatus::Completed)],
                0,
            ),
            ResolvedConfig::default(),
        );

        state.load_open_session(&read);

        assert_eq!(state.app_state.latest_context_window, Some(status));
    }

    #[test]
    fn canonical_history_context_window_uses_last_measured_diagnostics() {
        let session = session_record(SessionId::new());
        let earlier = context_window_status(1_200);
        let latest_measured = context_window_status(2_400);
        let items = vec![
            diagnostics_history_item(&session, Some(earlier)),
            diagnostics_history_item(&session, Some(latest_measured.clone())),
            diagnostics_history_item(&session, None),
        ];

        assert_eq!(
            latest_context_window_from_history_items(&items),
            Some(latest_measured)
        );
    }

    #[test]
    fn start_new_chat_clears_existing_session_selection() {
        let existing = SessionId::new();
        let mut state = DesktopState::new(
            snapshot(
                vec![session_row(existing, "existing", SessionStatus::Running)],
                0,
            ),
            ResolvedConfig::default(),
        );
        state.app_state.current_session_id = Some(existing);

        state.start_new_chat();

        assert_eq!(state.selected_session_id(), None);
        assert_eq!(state.snapshot.selected_session_index, 1);
        assert_eq!(state.selected_index(), -1);
        assert_eq!(state.current_session_label(), "新規チャット");
    }

    #[test]
    fn nested_file_change_roots_survive_start_new_chat_reset() {
        let storage_root = Utf8PathBuf::from("C:/workspace/aaa");
        let display_root = storage_root.join("bbb");
        let mut initial_snapshot = snapshot(Vec::new(), 0);
        initial_snapshot.workspace_path = display_root.to_string();
        let mut state = DesktopState::new(initial_snapshot, ResolvedConfig::default());
        state.set_file_change_display_roots(&storage_root, &display_root);

        state.start_new_chat();

        assert_nested_file_change_projection(&mut state);
    }

    #[test]
    fn nested_file_change_roots_survive_project_selection_reset() {
        let storage_root = Utf8PathBuf::from("C:/workspace/aaa");
        let display_root = storage_root.join("bbb");
        let mut initial_snapshot = snapshot(Vec::new(), 0);
        initial_snapshot.workspace_path = display_root.to_string();
        initial_snapshot.project_rows.push(DesktopProjectRow {
            project_id: ProjectId::new(),
            label: "other".to_string(),
            path: "C:/workspace/other".to_string(),
        });
        let mut state = DesktopState::new(initial_snapshot, ResolvedConfig::default());
        state.set_file_change_display_roots(&storage_root, &display_root);

        state.select_project(1);

        assert_nested_file_change_projection(&mut state);
    }

    #[test]
    fn startup_uses_config_only_and_catalog_remains_an_explicit_operation() {
        let mut config = ResolvedConfig::default();
        config.model.base_url = "http://127.0.0.1:1234".to_string();
        config.model.model = "configured-model".to_string();
        config.docling.enabled = false;
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), config.clone());

        state.begin_startup(true, None, camino::Utf8Path::new("C:/workspace"));
        assert_eq!(
            state.startup.status,
            super::super::startup::DesktopStartupStatus::Ready
        );
        assert!(!state.async_polling_required());
        assert!(
            !state
                .pending_async_operation_keys()
                .iter()
                .any(|key| key == "startup_readiness_check")
        );

        state.begin_provider_model_load(config.model.base_url.clone());
        assert!(state.provider_model_load_pending());
        assert!(state.async_polling_required());
        state.finish_provider_model_load(initial_provider_model_infos(&config));
        assert!(state.can_apply_provider_selection());
        let config_info = state
            .selected_provider_model_info()
            .expect("config-derived provider model");
        assert_eq!(config_info.load_state, ProviderModelLoadState::Unknown);
        assert_eq!(config_info.max_output_tokens, None);
        assert_eq!(config_info.supports_reasoning, None);
        assert_eq!(
            state.startup.status,
            super::super::startup::DesktopStartupStatus::Ready
        );
        assert!(!state.async_polling_required());
    }

    #[test]
    fn provider_apply_accepts_local_valid_manual_targets_without_reloading_catalog() {
        let mut config = ResolvedConfig::default();
        config.model.base_url = "http://127.0.0.1:1234".to_string();
        config.model.model = "catalog-model".to_string();
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), config.clone());
        assert!(state.can_apply_provider_selection());

        state.provider_config.provider_context_window_input = "65536".to_string();
        assert!(
            state.can_apply_provider_selection(),
            "local context edits keep the current provider target eligible"
        );

        state.provider_config.provider_base_url_input = "http://127.0.0.1:5678".to_string();
        assert!(state.can_apply_provider_selection());
        assert!(state.can_save_provider_selection_global());
        state.provider_config.provider_base_url_input = config.model.base_url.clone();
        state.provider_config.provider_profile_input = ProviderProfile::OpenAiCompatible;
        assert!(state.can_apply_provider_selection());
        assert!(state.can_save_provider_selection_global());
        state.provider_config.provider_profile_input = config.model.provider_profile;
        state.provider_config.provider_selected_model_id_input = "other-model".to_string();
        state
            .provider_config
            .provider_models
            .push("other-model".to_string());
        state.provider_config.provider_selected_index =
            (state.provider_config.provider_models.len() - 1) as i32;
        assert!(state.can_apply_provider_selection());
        assert!(state.can_save_provider_selection_global());

        let mut catalog_info = provider_info_from_config(&config);
        catalog_info.context_window = Some(262_144);
        catalog_info.source = "provider_catalog".to_string();
        state.provider_config.provider_selected_model_id_input = config.model.model.clone();
        state.begin_provider_model_load(config.model.base_url.clone());
        state.finish_provider_model_load(vec![catalog_info]);
        assert!(state.can_apply_provider_selection());
        assert!(state.provider_catalog_owns_current_target());
        assert_eq!(
            state.provider_config.provider_loaded_profile,
            Some(config.model.provider_profile)
        );
        state.provider_config.provider_profile_input = ProviderProfile::OpenAiCompatible;
        assert!(!state.provider_catalog_owns_current_target());
        state.provider_config.provider_profile_input = config.model.provider_profile;

        let mut same_target = config.clone();
        same_target.model.context_window = 65_536;
        state.reset_effective_config(same_target);
        let retained = state
            .selected_provider_model_info()
            .expect("selected provider metadata");
        assert_eq!(retained.source, "provider_catalog");
        assert_eq!(retained.context_window, Some(262_144));
        assert!(state.can_apply_provider_selection());

        let mut changed_target = config;
        changed_target.model.base_url = "http://127.0.0.1:5678".to_string();
        state.reset_effective_config(changed_target);
        assert_eq!(state.provider_config.provider_loaded_base_url, None);
        assert!(
            state.can_apply_provider_selection(),
            "the newly effective target is eligible without stale catalog evidence"
        );
    }

    #[test]
    fn provider_editor_projects_the_global_owner_when_root_effective_settings_differ() {
        let mut global = ResolvedConfig::default();
        global.model.base_url = "http://127.0.0.1:1234".to_string();
        global.model.model = "global-model".to_string();
        global.model.context_window = 32_768;
        global.model.max_output_tokens = 2_048;
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), global.clone());
        let mut root_effective = global.clone();
        root_effective.model.base_url = "http://127.0.0.1:5678".to_string();
        root_effective.model.model = "root-model".to_string();
        root_effective.model.context_window = 131_072;
        root_effective.model.max_output_tokens = 8_192;
        state.reset_effective_config(root_effective);

        assert!(state.show_provider_editor());

        assert_eq!(
            state.provider_config.provider_base_url_input,
            global.model.base_url
        );
        assert_eq!(
            state.provider_config.provider_selected_model_id_input,
            global.model.model
        );
        assert_eq!(
            state.provider_config.provider_context_window_input,
            global.model.context_window.to_string()
        );
        assert!(state.provider_input_matches_global_target());
        assert!(!state.provider_input_matches_effective_target());
        assert!(state.can_save_provider_selection_global());
        assert!(state.can_apply_provider_selection());
    }

    #[test]
    fn persisted_root_settings_merge_preserves_newer_non_settings_metadata() {
        let session_id = SessionId::new();
        let mut current = session_record(session_id);
        current.title = "newer runtime title".to_string();
        current.status = SessionStatus::Running;
        current.cwd = Utf8PathBuf::from("C:/workspace/current-root");
        current.model = "old-model".to_string();
        current.base_url = "http://old-provider".to_string();
        current.access_mode = AccessMode::Default;
        current.model_parameters.context_window = Some(32_768);
        current.session_settings_revision = 7;
        current.created_at_ms = 10;
        current.updated_at_ms = 90;
        current.completed_at_ms = None;
        let mut state = DesktopState::new(
            snapshot(
                vec![session_row(session_id, &current.title, current.status)],
                0,
            ),
            ResolvedConfig::default(),
        );
        state.load_open_session(&canonical_read(&current, Vec::new(), Vec::new()));

        let mut persisted = current.clone();
        persisted.project_id = ProjectId::new();
        persisted.title = "stale persisted title".to_string();
        persisted.status = SessionStatus::Completed;
        persisted.cwd = Utf8PathBuf::from("C:/workspace/stale-root");
        persisted.model = "saved-model".to_string();
        persisted.base_url = "http://saved-provider".to_string();
        persisted.access_mode = AccessMode::FullAccess;
        persisted.model_parameters.context_window = Some(65_536);
        persisted.session_settings_revision = 8;
        persisted.created_at_ms = 1;
        persisted.updated_at_ms = 120;
        persisted.completed_at_ms = Some(20);

        assert!(
            !state.persisted_root_session_settings_are_projected(&persisted),
            "a canonical cwd or project owner change is not this GUI write's projection"
        );
        assert!(
            !state.apply_persisted_root_session_record(persisted.clone()),
            "a cwd/project authority conflict requires a full reload and must not advance settings state"
        );
        let unchanged = state
            .open_session
            .as_ref()
            .expect("open session remains loaded")
            .session();
        assert_eq!(unchanged.cwd, current.cwd);
        assert_eq!(unchanged.model, current.model);
        assert_eq!(unchanged.session_settings_revision, 7);

        persisted.project_id = current.project_id;
        persisted.cwd = current.cwd.clone();
        assert!(state.apply_persisted_root_session_record(persisted.clone()));
        let projected = state
            .open_session
            .as_ref()
            .expect("open session remains loaded")
            .session();
        assert_eq!(projected.project_id, current.project_id);
        assert_eq!(projected.title, current.title);
        assert_eq!(projected.status, current.status);
        assert_eq!(projected.cwd, current.cwd);
        assert_eq!(projected.created_at_ms, current.created_at_ms);
        assert_eq!(projected.updated_at_ms, 120);
        assert_eq!(projected.completed_at_ms, current.completed_at_ms);
        assert_eq!(projected.model, persisted.model);
        assert_eq!(projected.base_url, persisted.base_url);
        assert_eq!(projected.access_mode, persisted.access_mode);
        assert_eq!(projected.model_parameters, persisted.model_parameters);
        assert_eq!(projected.session_settings_revision, 8);
        assert!(state.persisted_root_session_settings_are_projected(&persisted));

        let mut runtime_newer = projected.clone();
        runtime_newer.title = "runtime title after settings CAS".to_string();
        runtime_newer.status = SessionStatus::Completed;
        runtime_newer.updated_at_ms = 200;
        runtime_newer.completed_at_ms = Some(200);
        assert!(
            state
                .open_session
                .as_mut()
                .expect("open session remains loaded")
                .replace_session_record(runtime_newer.clone())
        );
        for current_projection in &mut state.app_state.sessions {
            if current_projection.id == session_id {
                *current_projection = runtime_newer.clone();
            }
        }

        let mut same_revision_conflict = persisted.clone();
        same_revision_conflict.cwd = current.cwd.clone();
        same_revision_conflict.model = "canonical-conflict-model".to_string();
        same_revision_conflict.title = "still stale".to_string();
        same_revision_conflict.updated_at_ms = 130;
        assert!(state.apply_persisted_root_session_record(same_revision_conflict.clone()));
        let projected = state
            .open_session
            .as_ref()
            .expect("open session remains loaded")
            .session();
        assert_eq!(projected.title, runtime_newer.title);
        assert_eq!(projected.status, runtime_newer.status);
        assert_eq!(projected.updated_at_ms, runtime_newer.updated_at_ms);
        assert_eq!(projected.completed_at_ms, runtime_newer.completed_at_ms);
        assert_eq!(projected.model, same_revision_conflict.model);
        assert!(state.persisted_root_session_settings_are_projected(&same_revision_conflict));

        let mut lower_revision = same_revision_conflict;
        lower_revision.session_settings_revision = 7;
        lower_revision.model = "older-model".to_string();
        assert!(!state.apply_persisted_root_session_record(lower_revision));
        assert_eq!(
            state
                .open_session
                .as_ref()
                .expect("open session remains loaded")
                .session()
                .model,
            "canonical-conflict-model"
        );
    }

    #[test]
    fn already_projected_settings_write_still_synchronizes_every_session_projection() {
        let session_id = SessionId::new();
        let mut current = session_record(session_id);
        current.model = "model-before-save".to_string();
        current.session_settings_revision = 11;
        current.updated_at_ms = 40;
        let mut state = DesktopState::new(
            snapshot(
                vec![session_row(session_id, &current.title, current.status)],
                0,
            ),
            ResolvedConfig::default(),
        );
        state.load_open_session(&canonical_read(&current, Vec::new(), Vec::new()));

        let mut listed = current.clone();
        listed.title = "newer list title".to_string();
        listed.updated_at_ms = 70;
        state.app_state.sessions = vec![listed.clone()];
        let mut loaded = current.clone();
        loaded.title = "newer loaded title".to_string();
        loaded.updated_at_ms = 80;
        state.app_state.loaded_sessions = vec![crate::session::LoadedSessionSummary {
            session: loaded.clone(),
            loaded_status: crate::session::LoadedSessionStatus::Idle,
            archived: false,
            active_turn_id: None,
            active_turn_sequence_no: None,
            admission_revision: 0,
            pending_permission_requests: 0,
            pending_user_input_requests: 0,
        }];

        let mut applied = current;
        applied.model = "model-after-save".to_string();
        applied.access_mode = AccessMode::Default;
        applied.model_parameters.max_output_tokens = Some(8_192);
        applied.session_settings_revision = 12;
        applied.updated_at_ms = 60;
        assert!(
            state
                .open_session
                .as_mut()
                .expect("open session remains loaded")
                .replace_session_record(applied.clone())
        );
        assert!(state.persisted_root_session_settings_are_projected(&applied));
        assert_eq!(state.app_state.sessions[0].session_settings_revision, 11);
        assert_eq!(
            state.app_state.loaded_sessions[0]
                .session
                .session_settings_revision,
            11
        );

        assert!(state.apply_persisted_root_session_record(applied));
        let listed = &state.app_state.sessions[0];
        assert_eq!(listed.model, "model-after-save");
        assert_eq!(listed.model_parameters.max_output_tokens, Some(8_192));
        assert_eq!(listed.session_settings_revision, 12);
        assert_eq!(listed.title, "newer list title");
        assert_eq!(listed.updated_at_ms, 70);
        let loaded = &state.app_state.loaded_sessions[0].session;
        assert_eq!(loaded.model, "model-after-save");
        assert_eq!(loaded.model_parameters.max_output_tokens, Some(8_192));
        assert_eq!(loaded.session_settings_revision, 12);
        assert_eq!(loaded.title, "newer loaded title");
        assert_eq!(loaded.updated_at_ms, 80);
    }

    #[test]
    fn settings_config_generation_delta_uses_effective_values_not_patch_presence() {
        let session_id = SessionId::new();
        let current = session_record(session_id);
        let mut global = ResolvedConfig::default();
        global.model.context_window = 32_768;
        let mut state = DesktopState::new(
            snapshot(
                vec![session_row(session_id, &current.title, current.status)],
                0,
            ),
            global,
        );
        state.load_open_session(&canonical_read(&current, Vec::new(), Vec::new()));

        let explicit_inherited_value = crate::session::SessionSettingsPatch {
            context_window: Some(32_768),
            ..crate::session::SessionSettingsPatch::default()
        };
        assert_eq!(
            state.root_session_settings_config_generation_delta(
                &current,
                &explicit_inherited_value,
            ),
            0,
            "None to explicit-global-equal changes durable ownership without replacing effective config"
        );

        let effective_change = crate::session::SessionSettingsPatch {
            context_window: Some(65_536),
            ..crate::session::SessionSettingsPatch::default()
        };
        assert_eq!(
            state.root_session_settings_config_generation_delta(&current, &effective_change),
            1
        );

        let access_only = crate::session::SessionSettingsPatch {
            access_mode: Some(AccessMode::Default),
            ..crate::session::SessionSettingsPatch::default()
        };
        assert_eq!(
            state.root_session_settings_config_generation_delta(&current, &access_only),
            0,
            "access mode updates the dedicated effective owner without replacing provider config"
        );
    }

    #[test]
    fn typed_terminal_outcome_matches_between_live_event_and_rehydrate() {
        for cause in [
            crate::protocol::TurnInterruptionCause::ApprovalAborted,
            crate::protocol::TurnInterruptionCause::UserStop,
        ] {
            let session_id = SessionId::new();
            let mut session = session_record(session_id);
            session.status = SessionStatus::Cancelled;

            let mut live = DesktopState::new(
                snapshot(
                    vec![session_row(
                        session_id,
                        &session.title,
                        SessionStatus::Running,
                    )],
                    0,
                ),
                ResolvedConfig::default(),
            );
            live.app_state.current_session_id = Some(session_id);
            live.apply_run_event(&terminal_event(
                session_id,
                crate::protocol::TurnTerminalOutcome::Interrupted { cause },
            ));

            let terminal = crate::protocol::TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id,
                turn_id: crate::protocol::TurnId::new(),
                source_item_id: None,
                sequence_no: 1,
                payload: crate::protocol::TurnItemPayload::Terminal {
                    outcome: crate::protocol::TurnTerminalOutcome::Interrupted { cause },
                },
            };
            let read = canonical_read(&session, Vec::new(), vec![terminal]);
            let mut rehydrated = DesktopState::new(
                snapshot(
                    vec![session_row(
                        session_id,
                        &session.title,
                        SessionStatus::Cancelled,
                    )],
                    0,
                ),
                ResolvedConfig::default(),
            );
            rehydrated.load_open_session(&read);

            assert_eq!(rehydrated.status_code, live.status_code);
            assert_eq!(
                rehydrated.app_state.status_message,
                live.app_state.status_message
            );
        }
    }

    #[test]
    fn provider_catalog_failure_uses_operation_type_not_error_keywords() {
        let messages = [
            "storage connection refused while loading model 404: access denied",
            "plain provider diagnostic",
        ];
        for message in messages {
            let mut state = DesktopState::new(snapshot(Vec::new(), 0), ResolvedConfig::default());
            state.begin_provider_model_load("http://127.0.0.1:1234".to_string());
            state.fail_provider_model_load(message);

            assert_eq!(
                state.provider_config.provider_status.title,
                "Providerモデル一覧を読み込めません"
            );
            assert_eq!(state.provider_config.provider_status.details, message);
            assert!(
                !state
                    .provider_config
                    .provider_status
                    .title
                    .contains("モデルが見つかりません")
            );
            assert!(
                !state
                    .provider_config
                    .provider_status
                    .title
                    .contains("許可されません")
            );
        }
    }

    #[test]
    fn startup_config_refresh_is_local_and_keeps_the_finish_only_latch() {
        let mut config = ResolvedConfig::default();
        config.model.base_url = String::new();
        config.model.model = "model-a".to_string();
        config.docling.enabled = false;
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), config.clone());
        state.begin_startup(true, None, camino::Utf8Path::new("C:/workspace"));
        assert_eq!(
            state.startup.status,
            super::super::startup::DesktopStartupStatus::RequiresProvider
        );
        assert!(!state.async_polling_required());

        config.model.base_url = "http://127.0.0.1:1234".to_string();
        config.model.model = "model-b".to_string();
        state.reset_effective_config(config.clone());
        state.refresh_startup_config_status();
        assert!(!state.provider_model_load_pending());
        assert_eq!(
            state.startup.status,
            super::super::startup::DesktopStartupStatus::RequiresProvider
        );
        assert!(!state.async_polling_required());

        state.complete_initial_setup_after_persist();
        assert_eq!(
            state.startup.status,
            super::super::startup::DesktopStartupStatus::Ready
        );
    }

    #[test]
    fn about_overlay_uses_the_rust_owner_and_closes_without_changing_run_state() {
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), ResolvedConfig::default());
        let run_status = state.app_state.run_status;

        state.show_help_menu();
        assert_eq!(state.view.overlay, DesktopOverlay::HelpMenu);

        state.show_about();
        assert_eq!(state.view.overlay, DesktopOverlay::About);
        assert_eq!(state.app_state.run_status, run_status);

        state.hide_overlay();
        assert_eq!(state.view.overlay, DesktopOverlay::None);
        assert_eq!(state.app_state.run_status, run_status);
    }

    #[test]
    fn mcp_history_navigation_preserves_the_local_chat_and_draft() {
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), ResolvedConfig::default());
        let session_id = SessionId::new();
        state.app_state.current_session_id = Some(session_id);
        state.composer.draft_prompt = "編集中の指示 😀".into();
        let generation = state.composer.owner_generation();
        let run_status = state.app_state.run_status;
        assert!(state.show_mcp_history());
        assert_eq!(state.view.overlay, DesktopOverlay::McpHistory);
        state.hide_overlay();
        assert_eq!(state.app_state.current_session_id, Some(session_id));
        assert_eq!(state.composer.draft_prompt, "編集中の指示 😀");
        assert_eq!(state.composer.owner_generation(), generation);
        assert_eq!(state.app_state.run_status, run_status);
    }

    #[test]
    fn command_palette_selection_returns_canonical_text_without_editing_the_draft() {
        let mut initial_snapshot = snapshot(Vec::new(), 0);
        initial_snapshot.command_rows.push(DesktopCommandRow {
            name: "case".to_string(),
            label: "Case".to_string(),
            path: "builtin:case".to_string(),
        });
        let mut state = DesktopState::new(initial_snapshot, ResolvedConfig::default());
        state.composer.draft_prompt = "left 😀 selected right".to_string();
        let run_status = state.app_state.run_status;
        let owner_generation = state.composer.owner_generation();
        state.show_command_palette();

        assert_eq!(
            state.select_command_from_palette(0),
            Some("/case ".to_string())
        );
        assert_eq!(state.composer.draft_prompt, "left 😀 selected right");
        assert_eq!(state.composer.owner_generation(), owner_generation);
        assert_eq!(state.app_state.run_status, run_status);
        assert_eq!(state.view.overlay, DesktopOverlay::None);

        state.show_command_palette();
        assert_eq!(state.select_command_from_palette(1), None);
        assert_eq!(state.composer.draft_prompt, "left 😀 selected right");
        assert_eq!(state.view.overlay, DesktopOverlay::CommandPalette);

        state.show_keyboard_shortcuts();
        assert_eq!(state.select_command_from_palette(0), None);
        assert_eq!(state.composer.draft_prompt, "left 😀 selected right");
        assert_eq!(state.view.overlay, DesktopOverlay::KeyboardShortcuts);
    }

    #[test]
    fn invalid_provider_startup_uses_the_blocking_initial_setup_owner() {
        let mut config = ResolvedConfig::default();
        config.model.base_url = String::new();
        config.model.model = "configured-model".to_string();
        config.docling.enabled = false;
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), config);

        state.begin_startup(true, None, camino::Utf8Path::new("C:/workspace"));

        assert_eq!(state.view.overlay, DesktopOverlay::InitialSetup);
        assert!(state.startup.requires_initial_setup());

        state.hide_overlay();

        assert_eq!(state.view.overlay, DesktopOverlay::InitialSetup);
        assert!(state.view.startup_overlay_forced);
        assert!(
            state
                .app_state
                .status_message
                .as_deref()
                .is_some_and(|message| message.contains("Finish"))
        );
    }

    #[test]
    fn missing_config_startup_overlay_remains_blocking() {
        let mut config = ResolvedConfig::default();
        config.model.base_url = "http://127.0.0.1:1234".to_string();
        config.model.model = "configured-model".to_string();
        config.docling.enabled = false;
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), config.clone());

        state.begin_startup(false, None, camino::Utf8Path::new("C:/workspace"));

        assert_eq!(
            state.startup.status,
            super::super::startup::DesktopStartupStatus::RequiresConfig
        );
        assert_eq!(state.view.overlay, DesktopOverlay::InitialSetup);
        assert!(state.startup.requires_initial_setup());

        state.hide_overlay();

        assert_eq!(state.view.overlay, DesktopOverlay::InitialSetup);
        assert!(state.view.startup_overlay_forced);
        assert!(
            state
                .app_state
                .status_message
                .as_deref()
                .is_some_and(|message| message.contains("Finish"))
        );
    }

    #[test]
    fn selecting_different_project_clears_stale_session_projection() {
        let existing = SessionId::new();
        let mut state = DesktopState::new(
            snapshot(
                vec![session_row(
                    existing,
                    "old project session",
                    SessionStatus::Running,
                )],
                0,
            ),
            ResolvedConfig::default(),
        );
        state.snapshot.project_rows.push(DesktopProjectRow {
            project_id: ProjectId::new(),
            label: "other workspace".to_string(),
            path: "C:/other-workspace".to_string(),
        });
        state.app_state.current_session_id = Some(existing);
        state.app_state.current_session_title = "old project session".to_string();

        state.select_project(1);

        assert_eq!(state.selected_project_index(), 1);
        assert!(state.snapshot.session_rows.is_empty());
        assert!(state.snapshot.session_details.is_empty());
        assert_eq!(state.selected_session_id(), None);
        assert_eq!(state.current_session_label(), "新規チャット");
        assert_eq!(state.selected_session_title(), "セッション未選択");
    }

    #[test]
    fn navigation_loading_tracks_workspace_and_session_loads() {
        let session_id = SessionId::new();
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), ResolvedConfig::default());
        assert!(!state.navigation_loading());

        let workspace_request =
            state.begin_workspace_load(camino::Utf8PathBuf::from("C:/workspace"), None);
        assert!(state.navigation_loading());
        assert!(state.is_current_navigation(workspace_request));
        state.finish_navigation(workspace_request);
        assert!(!state.navigation_loading());

        let session_request = state.begin_session_load(session_id);
        assert!(state.navigation_loading());
        assert!(state.is_current_session_navigation(session_request, session_id));
        let newer_request =
            state.begin_workspace_load(camino::Utf8PathBuf::from("C:/other-workspace"), None);
        assert!(state.navigation_loading());
        assert!(!state.finish_navigation(session_request));
        assert!(state.navigation_loading());
        assert!(state.finish_navigation(newer_request));
        assert!(!state.navigation_loading());
    }

    #[test]
    fn composer_attachments_clear_only_when_the_durable_owner_changes() {
        let session_a = SessionId::new();
        let session_b = SessionId::new();
        let attachment = camino::Utf8PathBuf::from("C:/workspace/owned-by-a.png");
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), ResolvedConfig::default());

        state
            .composer
            .image_attachment_paths
            .push(attachment.clone());
        state.app_state.current_session_id = Some(session_a);
        state.bind_composer_to_loaded_session(session_a);
        assert_eq!(
            state.composer.image_attachment_paths,
            vec![attachment.clone()],
            "the first durable session adopts the unowned new-chat draft"
        );
        assert!(!state.rebind_composer_owner(Some(session_a)));
        assert_eq!(
            state.composer.image_attachment_paths,
            vec![attachment.clone()]
        );

        assert!(state.rebind_composer_owner(Some(session_b)));
        assert!(state.composer.image_attachment_paths.is_empty());
    }

    #[test]
    fn stop_request_stays_non_terminal_until_turn_terminal_arrives() {
        let session_id = SessionId::new();
        let mut state = DesktopState::new(
            snapshot(
                vec![session_row(session_id, "Long task", SessionStatus::Running)],
                0,
            ),
            ResolvedConfig::default(),
        );
        state.app_state.current_session_id = Some(session_id);
        state.app_state.run_status = RunStatus::Running;

        state.mark_run_stop_requested("run cancellation requested", "停止を要求しました。");

        assert!(state.is_busy());
        assert_eq!(state.app_state.run_status, RunStatus::Running);
        assert_eq!(state.app_state.interruption_cause, None);
        assert_eq!(state.status_code, DesktopStatusCode::Plain);
        assert_eq!(
            state.app_state.progress.current_phase,
            RunProgressPhase::StopRequested
        );
        let short_id = session_id.to_string().chars().take(8).collect::<String>();
        assert_eq!(
            state.snapshot.session_rows[0].label,
            format!("Long task [実行中] {short_id}")
        );
    }

    #[test]
    fn terminal_run_event_updates_session_row_status_without_snapshot_refresh() {
        let session_id = SessionId::new();
        let mut state = DesktopState::new(
            snapshot(
                vec![session_row(
                    session_id,
                    "docx/xlsx要約",
                    SessionStatus::Running,
                )],
                0,
            ),
            ResolvedConfig::default(),
        );
        state.app_state.current_session_id = Some(session_id);
        state.app_state.current_session_title = "docx/xlsx要約".to_string();
        state.app_state.run_status = RunStatus::Running;

        state.apply_run_event(&terminal_event(
            session_id,
            crate::protocol::TurnTerminalOutcome::Completed,
        ));

        assert_eq!(state.app_state.run_status, RunStatus::Completed);
        let short_id = session_id.to_string().chars().take(8).collect::<String>();
        assert_eq!(
            state.snapshot.session_rows[0].label,
            format!("docx/xlsx要約 [完了] {short_id}")
        );
        assert_eq!(
            state.snapshot.session_rows[0].loaded_status,
            crate::session::LoadedSessionStatus::Idle
        );
        assert!(!state.selected_session_title().contains("[実行中]"));
    }

    #[test]
    fn history_export_admission_rejects_repeat_navigation_and_background_mutation() {
        let session_id = SessionId::new();
        let mut state = DesktopState::new(
            snapshot(
                vec![session_row(
                    session_id,
                    "exportable",
                    SessionStatus::Completed,
                )],
                0,
            ),
            ResolvedConfig::default(),
        );

        assert!(state.can_export_history());
        state.begin_history_export();
        assert!(
            !state.can_export_history(),
            "repeated export is not admitted"
        );
        state.finish_history_export();
        assert!(state.can_export_history());

        let navigation = state.begin_session_load(session_id);
        assert!(!state.can_export_history());
        assert!(state.finish_navigation(navigation));

        let delete = state.begin_session_delete_mutation();
        assert!(!state.can_export_history());
        assert!(state.finish_session_delete_mutation(delete));
        assert!(state.can_export_history());
    }

    #[test]
    fn post_run_refresh_pending_is_typed_until_current_detail_reload() {
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), ResolvedConfig::default());

        assert!(!state.post_run_refresh_pending());

        state.mark_post_run_refresh_pending();
        assert!(state.post_run_refresh_pending());

        state.clear_post_run_refresh_pending();
        assert!(!state.post_run_refresh_pending());
    }

    #[test]
    fn background_mutation_pending_is_reference_counted() {
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), ResolvedConfig::default());

        assert!(!state.background_mutation_pending());

        let session_delete_id = state.begin_session_delete_mutation();
        let project_delete_id = state.begin_project_delete_mutation();
        assert!(state.background_mutation_pending());

        assert!(state.finish_session_delete_mutation(session_delete_id));
        assert!(state.background_mutation_pending());

        assert!(state.finish_project_delete_mutation(project_delete_id));
        assert!(!state.background_mutation_pending());

        assert!(!state.finish_project_delete_mutation(project_delete_id));
        assert!(!state.background_mutation_pending());

        let steer_id = state.begin_steer_submission();
        assert!(state.steer_submission_pending());
        assert!(state.background_mutation_pending());
        assert!(state.finish_steer_submission(steer_id));
        assert!(!state.steer_submission_pending());
        assert!(!state.background_mutation_pending());

        let settings_id = state.begin_session_settings_persistence();
        assert!(state.background_mutation_pending());
        assert!(!state.can_begin_navigation());
        assert!(state.finish_session_settings_persistence(settings_id));
        assert!(!state.background_mutation_pending());
    }

    #[test]
    fn navigation_admission_rejects_run_background_mutation_and_navigation() {
        let session_id = SessionId::new();
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), ResolvedConfig::default());

        assert!(state.can_begin_navigation());

        state.app_state.run_status = RunStatus::Running;
        assert!(!state.can_begin_navigation());

        state.app_state.run_status = RunStatus::Completed;
        let session_delete_id = state.begin_session_delete_mutation();
        assert!(!state.can_begin_navigation());
        assert!(state.finish_session_delete_mutation(session_delete_id));

        let request_id = state.begin_session_load(session_id);
        assert!(!state.can_begin_navigation());
        assert!(state.finish_navigation(request_id));
        assert!(state.can_begin_navigation());
    }

    #[test]
    fn read_only_turn_page_admission_stays_open_during_the_owned_run() {
        let session_id = SessionId::new();
        let session = session_record(session_id);
        let mut state = DesktopState::new(
            snapshot(
                vec![session_row(
                    session_id,
                    &session.title,
                    SessionStatus::Completed,
                )],
                0,
            ),
            ResolvedConfig::default(),
        );
        state.load_open_session(&canonical_read(&session, Vec::new(), Vec::new()));
        state.app_state.run_status = RunStatus::Running;

        assert!(!state.can_begin_navigation());
        assert!(state.can_begin_turn_page_load());

        state.begin_turn_page_load();
        assert!(!state.can_begin_turn_page_load());
        state.finish_turn_page_load();
        assert!(state.can_begin_turn_page_load());

        let mutation_id = state.begin_session_delete_mutation();
        assert!(!state.can_begin_turn_page_load());
        assert!(state.finish_session_delete_mutation(mutation_id));
    }

    #[test]
    fn snapshot_and_turn_page_operations_keep_async_polling_alive() {
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), ResolvedConfig::default());

        assert!(!state.snapshot_refresh_pending());
        state.begin_snapshot_refresh();
        assert!(state.snapshot_refresh_pending());
        assert!(state.async_polling_required());
        assert!(
            state
                .pending_async_operation_keys()
                .contains(&"snapshot_refresh".to_string())
        );
        state.finish_snapshot_refresh();
        assert!(!state.snapshot_refresh_pending());

        state.begin_turn_page_load();
        assert!(state.turn_page_load_pending());
        assert!(state.async_polling_required());
        assert!(
            state
                .pending_async_operation_keys()
                .contains(&"turn_page_load".to_string())
        );
        state.finish_turn_page_load();
        assert!(!state.turn_page_load_pending());
        assert!(!state.async_polling_required());
    }

    #[test]
    fn async_registry_projects_use_case_polling_roots() {
        let session_id = SessionId::new();
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), ResolvedConfig::default());

        state.begin_workspace_load(camino::Utf8PathBuf::from("C:/workspace"), None);
        assert!(state.navigation_loading());
        assert!(state.async_polling_required());
        assert!(
            state
                .pending_async_operation_keys()
                .contains(&"workspace_load".to_string())
        );

        let session_request = state.begin_session_load(session_id);
        assert!(state.navigation_loading());
        assert!(
            !state
                .pending_async_operation_keys()
                .contains(&"workspace_load".to_string())
        );
        assert!(
            state
                .pending_async_operation_keys()
                .contains(&"session_load".to_string())
        );
        assert!(state.finish_navigation(session_request));
        assert!(!state.navigation_loading());

        state.mark_post_run_refresh_pending();
        state.begin_session_delete_mutation();
        state.begin_history_export();
        assert!(state.async_polling_required());
        assert!(state.post_run_refresh_pending());
        assert!(state.background_mutation_pending());
        assert!(
            state
                .pending_async_operation_keys()
                .contains(&"history_export".to_string())
        );
    }

    #[test]
    fn stale_prompt_enhance_result_does_not_clear_new_operation() {
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), ResolvedConfig::default());

        state.begin_prompt_enhance(1, "first", CancellationToken::new());
        assert!(state.prompt_enhance_pending());
        state.begin_prompt_enhance(2, "second", CancellationToken::new());

        assert!(!state.finish_prompt_enhance(1, "old draft".to_string()));
        assert!(
            state
                .pending_async_operation_keys()
                .contains(&"prompt_enhance".to_string())
        );
        assert!(!state.fail_prompt_enhance(1));
        assert!(
            state
                .pending_async_operation_keys()
                .contains(&"prompt_enhance".to_string())
        );

        assert!(state.finish_prompt_enhance(2, "new draft".to_string()));
        assert!(
            !state
                .pending_async_operation_keys()
                .contains(&"prompt_enhance".to_string())
        );
        assert!(!state.prompt_enhance_pending());
    }

    #[test]
    fn prompt_review_cancel_requires_the_current_request_identity() {
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), ResolvedConfig::default());
        state.begin_prompt_enhance(2, "current", CancellationToken::new());

        assert!(!state.cancel_prompt_review_if_current(1));
        assert_eq!(
            state
                .app_state
                .prompt_review
                .as_ref()
                .map(|review| review.request_id),
            Some(2)
        );
        assert_eq!(state.view.overlay, DesktopOverlay::PromptReview);
        assert!(state.cancel_prompt_review_if_current(2));
        assert!(state.app_state.prompt_review.is_none());
        assert_eq!(state.view.overlay, DesktopOverlay::None);
    }

    #[test]
    fn prompt_review_owner_blocks_every_attachment_mutation_at_the_state_boundary() {
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), ResolvedConfig::default());
        let original = camino::Utf8PathBuf::from("C:/workspace/original.png");
        assert!(state.attach_image_path(original.clone()));
        assert!(state.set_image_attachment_input("typed-before-review.png".to_string()));
        state.begin_prompt_enhance(31, "review owner", CancellationToken::new());

        assert!(!state.set_image_attachment_input("replacement.png".to_string()));
        assert!(
            !state.attach_image_path(camino::Utf8PathBuf::from("C:/workspace/replacement.png"))
        );
        assert!(!state.clear_image_attachments());
        assert!(!state.remove_image_attachment(0));

        assert_eq!(
            state.composer.image_attachment_input,
            "typed-before-review.png"
        );
        assert_eq!(state.composer.image_attachment_paths, vec![original]);
        assert_eq!(
            state
                .app_state
                .prompt_review
                .as_ref()
                .map(|review| review.request_id),
            Some(31)
        );
    }

    #[test]
    fn prompt_review_owner_is_the_state_level_overlay_transition_boundary() {
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), ResolvedConfig::default());
        state.begin_prompt_enhance(32, "review source", CancellationToken::new());
        assert!(state.finish_prompt_enhance(32, "review draft".to_string()));
        let original_provider_input = state.provider_config.provider_base_url_input.clone();
        let original_workspace_input = state.workspace_input.clone();

        assert!(!state.show_config_editor());
        assert!(!state.show_mcp_history());
        assert!(!state.show_provider_editor());
        assert!(!state.show_workspace_picker("C:/other"));
        assert!(!state.show_file_menu());
        assert!(!state.show_edit_menu());
        assert!(!state.show_view_menu());
        assert!(!state.show_help_menu());
        assert!(!state.show_about());
        assert!(!state.show_project_menu());
        assert!(!state.show_command_palette());
        assert!(!state.show_keyboard_shortcuts());
        assert!(!state.hide_overlay());
        state.startup.action_overlay = Some(DesktopOverlay::ConfigEditor);
        state.apply_startup_overlay();

        assert_eq!(state.view.overlay, DesktopOverlay::PromptReview);
        assert_eq!(
            state.provider_config.provider_base_url_input,
            original_provider_input
        );
        assert_eq!(state.workspace_input, original_workspace_input);
        assert_eq!(
            state.app_state.prompt_review.as_ref().map(|review| (
                review.request_id,
                review.raw_prompt_text.as_str(),
                review.current_draft_text.as_str(),
            )),
            Some((32, "review source", "review draft")),
        );
        assert_eq!(
            state.prompt_review_expected_active_turn(32),
            Some(ActiveTurnExpectation::initial_idle()),
        );

        assert!(state.cancel_prompt_review_if_current(32));
        assert!(state.show_config_editor());
        assert_eq!(state.view.overlay, DesktopOverlay::ConfigEditor);
    }

    #[test]
    fn same_owner_new_chat_cancels_prompt_enhance_and_advances_owner() {
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), ResolvedConfig::default());
        let cancellation = CancellationToken::new();
        let generation = state.composer.owner_generation();
        state.begin_prompt_enhance(3, "stale", cancellation.clone());

        state.start_new_chat();

        assert!(cancellation.is_cancelled());
        assert!(!state.prompt_enhance_pending());
        assert!(state.app_state.prompt_review.is_none());
        assert!(state.composer.owner_generation() > generation);
        assert!(!state.finish_prompt_enhance(3, "late".to_string()));
    }

    #[test]
    fn repeated_provider_input_preserves_pending_owner_and_target_change_invalidates_it() {
        let mut config = ResolvedConfig::default();
        config.model.base_url = "http://127.0.0.1:1234".to_string();
        config.model.model = "model-a".to_string();
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), config.clone());
        state.begin_provider_model_load(config.model.base_url.clone());
        assert!(state.provider_model_load_pending());

        assert!(!state.accept_provider_action_input(
            config.model.base_url.clone(),
            config.model.provider_profile,
            config.model.api_key_env.clone().unwrap_or_default(),
            config.model.context_window.to_string(),
            config.model.model.clone(),
        ));
        assert!(state.provider_model_load_pending());

        assert!(state.accept_provider_action_input(
            "http://127.0.0.1:5678".to_string(),
            config.model.provider_profile,
            config.model.api_key_env.clone().unwrap_or_default(),
            config.model.context_window.to_string(),
            config.model.model,
        ));
        assert!(!state.provider_model_load_pending());
    }

    #[test]
    fn generic_close_preserves_prompt_review_and_exact_cancel_is_terminal() {
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), ResolvedConfig::default());
        state.begin_prompt_enhance(7, "draft me", CancellationToken::new());
        assert_eq!(state.view.overlay, DesktopOverlay::PromptReview);

        assert!(!state.hide_overlay());
        assert_eq!(state.view.overlay, DesktopOverlay::PromptReview);
        assert!(state.app_state.prompt_review.is_some());
        assert!(state.cancel_prompt_review_if_current(7));

        assert_eq!(state.view.overlay, DesktopOverlay::None);
        assert!(state.app_state.prompt_review.is_none());
        assert!(
            !state
                .pending_async_operation_keys()
                .contains(&"prompt_enhance".to_string())
        );
        assert!(!state.finish_prompt_enhance(7, "late draft".to_string()));
        assert_eq!(state.view.overlay, DesktopOverlay::None);
        assert!(state.app_state.prompt_review.is_none());
    }

    #[test]
    fn closed_prompt_review_does_not_leak_async_owner_across_navigation() {
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), ResolvedConfig::default());
        state.begin_prompt_enhance(9, "draft before navigation", CancellationToken::new());
        assert!(state.cancel_prompt_review_if_current(9));
        let session_id = SessionId::new();
        let navigation_id = state.begin_session_load(session_id);

        assert!(state.is_current_session_navigation(navigation_id, session_id));
        assert!(!state.fail_prompt_enhance(9));
        assert!(state.navigation_loading());
        assert!(
            !state
                .pending_async_operation_keys()
                .contains(&"prompt_enhance".to_string())
        );
        assert_eq!(state.view.overlay, DesktopOverlay::None);
    }

    #[test]
    fn window_opacity_is_clamped_to_safe_visibility_range() {
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), ResolvedConfig::default());

        state.set_window_opacity_percent(0);
        assert_eq!(
            state.view.window_opacity_percent,
            MIN_WINDOW_OPACITY_PERCENT
        );

        state.set_window_opacity_percent(150);
        assert_eq!(
            state.view.window_opacity_percent,
            MAX_WINDOW_OPACITY_PERCENT
        );

        state.set_window_opacity_percent(75);
        assert_eq!(state.view.window_opacity_percent, 75);
    }
}

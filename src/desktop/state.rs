use crate::protocol::{
    HistoryItem, HistoryItemPayload, TurnInterruptionCause, TurnItemPayload, TurnTerminalOutcome,
};
use crate::session::{
    CanonicalSessionRead, ProjectId, PromptDispatchPart, SessionId, SessionStatus,
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
use crate::config::ProviderMetadataMode;
use crate::config::ResolvedConfig;
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
    PermissionPolicyDenied,
    ApprovalAborted,
    UserStopped,
    AgentInterrupted,
    TreeStopped,
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
    FileMenu,
    EditMenu,
    ViewMenu,
    HelpMenu,
    ProjectMenu,
    ConfigEditor,
    ProviderEditor,
    WorkspacePicker,
    PromptReview,
    CommandPalette,
    KeyboardShortcuts,
}

#[derive(Debug, Clone)]
pub struct DesktopState {
    pub snapshot: DesktopSnapshot,
    pub app_state: AppState,
    pub open_session: Option<OpenSessionView>,
    pub composer: DesktopComposerState,
    pub workspace_input: String,
    pub provider_config: DesktopProviderConfigState,
    pub navigation: DesktopNavigationState,
    pub view: DesktopViewState,
    pub startup: DesktopStartupState,
    pub status_code: DesktopStatusCode,
    prompt_enhance_cancellation: Option<(u64, CancellationToken)>,
}

impl DesktopState {
    pub fn new(snapshot: DesktopSnapshot, effective_config: ResolvedConfig) -> Self {
        let composer = DesktopComposerState::for_owner(snapshot.workspace_path.clone(), None);
        Self {
            snapshot,
            app_state: AppState::default(),
            open_session: None,
            composer,
            workspace_input: String::new(),
            provider_config: DesktopProviderConfigState::new(effective_config),
            navigation: DesktopNavigationState::default(),
            view: DesktopViewState::default(),
            startup: DesktopStartupState::ready(),
            status_code: DesktopStatusCode::Plain,
            prompt_enhance_cancellation: None,
        }
        .with_provider_fields()
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
                self.app_state = AppState::default();
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

    pub fn set_image_attachment_input(&mut self, input: String) {
        self.composer.image_attachment_input = input;
    }

    pub fn attach_image_path(&mut self, path: camino::Utf8PathBuf) {
        if self
            .composer
            .image_attachment_paths
            .iter()
            .any(|existing| existing == &path)
        {
            self.set_status_message("Image is already attached.");
            return;
        }
        self.composer.image_attachment_paths.push(path);
        self.composer.image_attachment_input.clear();
        self.set_status_message("Image attached to the next prompt.");
    }

    pub fn clear_image_attachments(&mut self) {
        self.composer.image_attachment_paths.clear();
        self.composer.image_attachment_input.clear();
        self.set_status_message("Image attachments cleared.");
    }

    pub fn remove_image_attachment(&mut self, index: usize) {
        if index >= self.composer.image_attachment_paths.len() {
            self.set_status_message("Image attachment is no longer available.");
            return;
        }
        let removed = self.composer.image_attachment_paths.remove(index);
        self.set_status_message(format!("Removed image attachment {}", removed));
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
        metadata_mode: ProviderMetadataMode,
        context_window: String,
        max_output_tokens: String,
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
            || self.provider_config.provider_metadata_mode_input != metadata_mode;
        self.provider_config.provider_base_url_input = base_url;
        self.provider_config.provider_metadata_mode_input = metadata_mode;
        self.provider_config.provider_context_window_input = context_window;
        self.provider_config.provider_max_output_tokens_input = max_output_tokens;
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
            self.view
                .async_operations
                .finish_kind(DesktopAsyncOperationKind::ProviderModelCatalogLoad);
        }
        target_changed
    }

    pub fn load_open_session(&mut self, read: &CanonicalSessionRead) {
        let session = &read.session;
        let turn_items = &read.turns.items;
        self.provider_config.update_access_mode(session.access_mode);
        self.bind_composer_to_loaded_session(session.id);
        let open_session = OpenSessionView::from_loaded(read);
        self.app_state
            .load_turn_items_with_active_turn(session, turn_items, read.active_turn_id);
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
        self.view.overlay = DesktopOverlay::None;
        self.view.artifact_selected_index = 0;
    }

    pub fn turn_page_load_pending(&self) -> bool {
        self.view
            .async_operations
            .is_pending(DesktopAsyncOperationKind::TurnPageLoad)
    }

    pub fn merge_open_session_history(&mut self, read: &CanonicalSessionRead) -> bool {
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
        let session = open_session.session().clone();
        let turn_items = open_session.turn_items().to_vec();
        let detail = open_session.stored_detail().clone();

        self.provider_config.update_access_mode(session.access_mode);
        let preserve_current_projection = self.app_state.current_session_id == Some(session.id);
        if preserve_current_projection {
            self.app_state.refresh_plan_from_turn_items(&turn_items);
        } else {
            self.app_state.load_turn_items_with_active_turn(
                &session,
                &turn_items,
                open_session.active_turn_id(),
            );
        }
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
        self.update_session_row_title(session.id, &session.title);
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
        self.provider_config.update_access_mode(session.access_mode);
        if let Some(context_window) = latest_context_window_from_history_items(&read.history.items)
        {
            self.app_state.latest_context_window = Some(context_window);
        }
        self.snapshot.replace_detail(detail);
        self.update_session_row_title(session.id, &session.title);
        self.update_session_row_status(session.id, session.status);
        self.apply_canonical_terminal_to_running_session(read);
        false
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

        let (run_status, progress_status, status_message) = match &outcome {
            TurnTerminalOutcome::Completed => (
                RunStatus::Completed,
                "Completed",
                "run completed".to_string(),
            ),
            TurnTerminalOutcome::Interrupted { cause } => (
                RunStatus::Cancelled,
                "Cancelled",
                crate::tui::state::interruption_status_message(*cause),
            ),
            TurnTerminalOutcome::Failed { error } => (RunStatus::Failed, "Failed", error.clone()),
        };
        self.app_state.run_status = run_status;
        self.app_state.status_message = Some(status_message);
        self.app_state.interruption_cause = outcome.interruption_cause();
        self.app_state.permission = None;
        self.app_state.progress.status = progress_status.to_string();
        self.app_state.progress.current_phase = RunProgressPhase::Terminal;
        self.app_state.progress.active_step = outcome.summary().to_string();
        self.status_code = self
            .app_state
            .interruption_cause
            .map(DesktopStatusCode::from_interruption)
            .unwrap_or(DesktopStatusCode::Plain);
        self.update_session_row_status(read.session.id, outcome.session_status());
        true
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
            open_session = Some(OpenSessionView::from_loaded(read));
        }
        let open_session = open_session.expect("open session projection is always available");
        let session = open_session.session().clone();
        let turn_items = open_session.turn_items();
        let detail = open_session.stored_detail().clone();
        self.app_state.refresh_plan_from_turn_items(turn_items);
        if let Some(context_window) = latest_context_window_from_history_items(&read.history.items)
        {
            self.app_state.latest_context_window = Some(context_window);
        }
        self.open_session = Some(open_session);
        self.snapshot.replace_detail(detail);
        self.update_session_row_title(session.id, &session.title);
        self.update_session_row_status(session.id, session.status);
    }

    pub fn apply_run_event(&mut self, event: &crate::session::RunEvent) {
        self.app_state.apply_run_event(event);
        self.status_code = match event {
            crate::session::RunEvent::TurnTerminal { terminal, .. } => terminal
                .interruption_cause()
                .map(DesktopStatusCode::from_interruption)
                .unwrap_or(DesktopStatusCode::Plain),
            _ => DesktopStatusCode::Plain,
        };
        match event {
            crate::session::RunEvent::SessionStarted { session_id, title } => {
                self.update_session_row_title(*session_id, title);
                self.update_session_row_status(*session_id, SessionStatus::Running);
            }
            crate::session::RunEvent::SessionTitleUpdated { session_id, title } => {
                self.update_session_row_title(*session_id, title);
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

    fn update_session_row_title(&mut self, session_id: SessionId, title: &str) {
        for row in self
            .snapshot
            .session_rows
            .iter_mut()
            .chain(self.snapshot.chat_session_rows.iter_mut())
        {
            if row.session_id == session_id {
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

    pub fn begin_prompt_enhance(
        &mut self,
        request_id: u64,
        raw_prompt: &str,
        cancellation: CancellationToken,
    ) {
        self.cancel_prompt_enhance_transport();
        self.prompt_enhance_cancellation = Some((request_id, cancellation));
        self.view
            .async_operations
            .begin_unique(DesktopAsyncOperationKind::PromptEnhance);
        self.app_state.begin_prompt_enhance(request_id, raw_prompt);
        self.composer.review_draft_text.clear();
        self.view.overlay = DesktopOverlay::PromptReview;
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
            self.composer.review_draft_text = draft;
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

    pub fn set_review_draft(&mut self, draft: String) {
        self.composer.review_draft_text = draft.clone();
        self.app_state.update_prompt_review_draft(draft);
    }

    pub fn cancel_prompt_review(&mut self) {
        self.cancel_prompt_enhance_transport();
        self.view
            .async_operations
            .finish_kind(DesktopAsyncOperationKind::PromptEnhance);
        self.app_state.cancel_prompt_review();
        self.composer.review_draft_text.clear();
        if self.view.overlay == DesktopOverlay::PromptReview {
            self.view.overlay = DesktopOverlay::None;
        }
    }

    pub fn build_prompt_dispatch(&self, send_enhanced: bool) -> Option<PromptDispatchPart> {
        self.app_state.build_prompt_dispatch(send_enhanced)
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
        self.app_state = AppState::default();
        self.open_session = None;
        self.view.artifact_selected_index = 0;
        self.view.overlay = DesktopOverlay::None;
        self.set_status_message("new chat ready");
    }

    fn finish_prompt_enhance_transport(&mut self, request_id: u64) {
        if self
            .prompt_enhance_cancellation
            .as_ref()
            .is_some_and(|(active_request_id, _)| *active_request_id == request_id)
        {
            self.prompt_enhance_cancellation = None;
        }
    }

    fn cancel_prompt_enhance_transport(&mut self) {
        if let Some((_, cancellation)) = self.prompt_enhance_cancellation.take() {
            cancellation.cancel();
        }
    }

    pub fn reset_effective_config(&mut self, config: ResolvedConfig) {
        self.provider_config.replace_effective_config(config);
    }

    pub fn show_config_editor(&mut self) {
        self.view.startup_overlay_forced = false;
        self.view.overlay = DesktopOverlay::ConfigEditor;
    }

    pub fn show_provider_editor(&mut self) {
        self.provider_config.provider_base_url_input =
            self.provider_config.effective_config.model.base_url.clone();
        self.provider_config.provider_metadata_mode_input = self
            .provider_config
            .effective_config
            .model
            .provider_metadata_mode;
        self.provider_config.provider_context_window_input = self
            .provider_config
            .effective_config
            .model
            .context_window
            .to_string();
        self.provider_config.provider_max_output_tokens_input = self
            .provider_config
            .effective_config
            .model
            .max_output_tokens
            .to_string();
        self.provider_config.provider_selected_model_id_input =
            self.provider_config.effective_config.model.model.clone();
        self.provider_config.provider_models = ensure_current_model(
            self.provider_config.provider_models.clone(),
            &self.provider_config.effective_config.model.model,
        );
        self.provider_config.provider_model_infos = ensure_current_model_info(
            self.provider_config.provider_model_infos.clone(),
            &self.provider_config.effective_config,
        );
        self.provider_config.provider_selected_index = self
            .provider_config
            .provider_models
            .iter()
            .position(|model| model == &self.provider_config.effective_config.model.model)
            .map(|index| index as i32)
            .unwrap_or(-1);
        self.view.startup_overlay_forced = false;
        self.view.overlay = DesktopOverlay::ProviderEditor;
    }

    pub fn show_workspace_picker(&mut self, current_path: &str) {
        self.workspace_input = current_path.to_string();
        self.view.overlay = DesktopOverlay::WorkspacePicker;
    }

    pub fn show_file_menu(&mut self) {
        self.view.overlay = DesktopOverlay::FileMenu;
    }

    pub fn show_edit_menu(&mut self) {
        self.view.overlay = DesktopOverlay::EditMenu;
    }

    pub fn show_view_menu(&mut self) {
        self.view.overlay = DesktopOverlay::ViewMenu;
    }

    pub fn show_help_menu(&mut self) {
        self.view.overlay = DesktopOverlay::HelpMenu;
    }

    pub fn show_project_menu(&mut self) {
        self.view.overlay = DesktopOverlay::ProjectMenu;
    }

    pub fn hide_overlay(&mut self) {
        if self.startup_requires_overlay(self.view.overlay) {
            self.set_status_message(
                "初期設定が必要です。保存または config.toml Import で設定を完了してください。",
            );
            return;
        }
        if self.view.overlay == DesktopOverlay::PromptReview {
            self.cancel_prompt_review();
            return;
        }
        self.view.startup_overlay_forced = false;
        self.view.overlay = DesktopOverlay::None;
    }

    pub fn mark_startup_config_reviewed(&mut self) {
        self.startup.mark_config_reviewed();
        self.apply_startup_overlay();
    }

    fn startup_requires_overlay(&self, overlay: DesktopOverlay) -> bool {
        self.startup.requires_initial_setup() && self.startup.action_overlay == Some(overlay)
    }

    pub fn begin_provider_model_load(&mut self, normalized_base_url: String) {
        self.provider_config.provider_base_url_input = normalized_base_url;
        self.provider_config.provider_loading = true;
        self.view
            .async_operations
            .begin_unique(DesktopAsyncOperationKind::ProviderModelCatalogLoad);
        self.provider_config.provider_loaded_base_url = None;
        self.provider_config.set_status(
            DesktopProviderStatusKind::Loading,
            "Provider 状態",
            "詳細は必要な場合だけ展開してください。",
            "Loading models in the background...",
        );
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

    pub fn show_command_palette(&mut self) {
        self.view.overlay = DesktopOverlay::CommandPalette;
    }

    pub fn show_keyboard_shortcuts(&mut self) {
        self.view.overlay = DesktopOverlay::KeyboardShortcuts;
    }

    pub fn insert_command_from_palette(&mut self, index: usize) {
        let Some(command) = self.snapshot.command_rows.get(index) else {
            self.set_status_message("command palette selection is no longer available");
            return;
        };
        self.composer.draft_prompt = format!("/{} ", command.name);
        self.view.overlay = DesktopOverlay::None;
        self.set_status_message(format!("inserted command /{}", command.name));
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
        let normalized = normalize_provider_base_url(&self.provider_config.provider_base_url_input);
        !self.provider_config.provider_loading
            && !self
                .provider_config
                .provider_base_url_input
                .trim()
                .is_empty()
            && self.selected_provider_model().is_some()
            && !normalized.is_empty()
            && self.provider_config.provider_loaded_base_url.as_deref() == Some(normalized.as_str())
    }

    fn with_provider_fields(mut self) -> Self {
        self.provider_config.provider_base_url_input =
            self.provider_config.effective_config.model.base_url.clone();
        self.provider_config.provider_metadata_mode_input = self
            .provider_config
            .effective_config
            .model
            .provider_metadata_mode;
        self.provider_config.provider_context_window_input = self
            .provider_config
            .effective_config
            .model
            .context_window
            .to_string();
        self.provider_config.provider_max_output_tokens_input = self
            .provider_config
            .effective_config
            .model
            .max_output_tokens
            .to_string();
        self.provider_config.provider_loaded_base_url = None;
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
        if let Some(overlay) = self.startup.action_overlay {
            self.view.overlay = overlay;
            if overlay == DesktopOverlay::ProviderEditor {
                self.show_provider_editor();
            } else if overlay == DesktopOverlay::ConfigEditor {
                self.show_config_editor();
            }
            self.view.startup_overlay_forced = true;
        } else if self.startup.status == super::startup::DesktopStartupStatus::Ready
            && self.view.startup_overlay_forced
        {
            self.view.startup_overlay_forced = false;
            self.view.overlay = DesktopOverlay::None;
        }
    }
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
        max_output_tokens: Some(config.model.max_output_tokens),
        supports_images: Some(config.model.supports_images),
        supports_tools: Some(config.model.supports_tools),
        supports_reasoning: Some(config.model.supports_reasoning),
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
    use crate::desktop::models::{DesktopProjectRow, DesktopSessionRow};
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
            configured_max_output_tokens: 8_192,
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
            turn_elapsed_ms: Default::default(),
            latest_turn_id,
            active_turn_id: None,
            active_turn_sequence_no: None,
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
    fn startup_uses_config_only_and_catalog_remains_an_explicit_operation() {
        let mut config = ResolvedConfig::default();
        config.model.base_url = "http://127.0.0.1:1234".to_string();
        config.model.model = "qwen/qwen3.6-35b-a3b".to_string();
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
        assert_eq!(
            state
                .selected_provider_model_info()
                .expect("config-derived provider model")
                .load_state,
            ProviderModelLoadState::Unknown
        );
        assert_eq!(
            state.startup.status,
            super::super::startup::DesktopStartupStatus::Ready
        );
        assert!(!state.async_polling_required());
    }

    #[test]
    fn provider_apply_requires_catalog_evidence_and_preserves_selected_metadata() {
        let mut config = ResolvedConfig::default();
        config.model.base_url = "http://127.0.0.1:1234".to_string();
        config.model.model = "catalog-model".to_string();
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), config.clone());
        assert!(!state.can_apply_provider_selection());

        let mut catalog_info = provider_info_from_config(&config);
        catalog_info.context_window = Some(262_144);
        catalog_info.source = "provider_catalog".to_string();
        state.begin_provider_model_load(config.model.base_url.clone());
        state.finish_provider_model_load(vec![catalog_info]);
        assert!(state.can_apply_provider_selection());

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
        assert!(!state.can_apply_provider_selection());
        assert_eq!(state.provider_config.provider_loaded_base_url, None);
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
    fn startup_config_refresh_is_local_and_never_creates_async_work() {
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
            super::super::startup::DesktopStartupStatus::Ready
        );
        assert!(!state.async_polling_required());
    }

    #[test]
    fn configured_provider_startup_overlay_can_be_closed() {
        let mut config = ResolvedConfig::default();
        config.model.base_url = String::new();
        config.model.model = "qwen/qwen3.6-35b-a3b".to_string();
        config.docling.enabled = false;
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), config);

        state.begin_startup(true, None, camino::Utf8Path::new("C:/workspace"));

        assert_eq!(state.view.overlay, DesktopOverlay::ProviderEditor);
        assert!(!state.startup.requires_initial_setup());

        state.hide_overlay();

        assert_eq!(state.view.overlay, DesktopOverlay::None);
        assert!(!state.view.startup_overlay_forced);
    }

    #[test]
    fn missing_config_startup_overlay_remains_blocking() {
        let mut config = ResolvedConfig::default();
        config.model.base_url = "http://127.0.0.1:1234".to_string();
        config.model.model = "qwen/qwen3.6-35b-a3b".to_string();
        config.docling.enabled = false;
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), config.clone());

        state.begin_startup(false, None, camino::Utf8Path::new("C:/workspace"));

        assert_eq!(
            state.startup.status,
            super::super::startup::DesktopStartupStatus::RequiresConfig
        );
        assert_eq!(state.view.overlay, DesktopOverlay::ConfigEditor);
        assert!(state.startup.requires_initial_setup());

        state.hide_overlay();

        assert_eq!(state.view.overlay, DesktopOverlay::ConfigEditor);
        assert!(state.view.startup_overlay_forced);
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
            config.model.provider_metadata_mode,
            config.model.context_window.to_string(),
            config.model.max_output_tokens.to_string(),
            config.model.model.clone(),
        ));
        assert!(state.provider_model_load_pending());

        assert!(state.accept_provider_action_input(
            "http://127.0.0.1:5678".to_string(),
            config.model.provider_metadata_mode,
            config.model.context_window.to_string(),
            config.model.max_output_tokens.to_string(),
            config.model.model,
        ));
        assert!(!state.provider_model_load_pending());
    }

    #[test]
    fn closing_prompt_review_is_terminal_and_late_result_cannot_reopen_it() {
        let mut state = DesktopState::new(snapshot(Vec::new(), 0), ResolvedConfig::default());
        state.begin_prompt_enhance(7, "draft me", CancellationToken::new());
        assert_eq!(state.view.overlay, DesktopOverlay::PromptReview);

        state.hide_overlay();

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
        state.hide_overlay();
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

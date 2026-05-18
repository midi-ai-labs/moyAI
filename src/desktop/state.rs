use crate::protocol::TurnItem;
use crate::session::{
    ProjectId, PromptDispatchPart, SessionId, SessionRecord, SessionStateSnapshot, SessionStatus,
    TodoItem, Transcript,
};
use crate::tool::PermissionRequest;
use crate::tui::state::{AppState, RunStatus};

use super::async_ops::{DesktopAsyncOperationKind, DesktopAsyncOperationRegistry};
use super::composer_state::DesktopComposerState;
use super::models::{DesktopSessionDetail, DesktopSnapshot};
use super::navigation::{DesktopNavigationState, NavigationRequestId, NavigationTarget};
use super::open_session::OpenSessionView;
use super::provider_config_state::DesktopProviderConfigState;
use super::query::build_session_detail_from_app_state_with_session;
use super::startup::DesktopStartupState;
use super::view_state::DesktopViewState;
use crate::config::ResolvedConfig;
use crate::llm::{ProviderModelInfo, normalize_provider_base_url};

pub const MIN_WINDOW_OPACITY_PERCENT: i32 = 50;
pub const MAX_WINDOW_OPACITY_PERCENT: i32 = 100;
pub const DEFAULT_WINDOW_OPACITY_PERCENT: i32 = 96;

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
}

impl DesktopState {
    pub fn new(snapshot: DesktopSnapshot, effective_config: ResolvedConfig) -> Self {
        Self {
            snapshot,
            app_state: AppState::default(),
            open_session: None,
            composer: DesktopComposerState::default(),
            workspace_input: String::new(),
            provider_config: DesktopProviderConfigState::new(effective_config),
            navigation: DesktopNavigationState::default(),
            view: DesktopViewState::default(),
            startup: DesktopStartupState::ready(),
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
        if self.startup.status == super::startup::DesktopStartupStatus::Loading {
            self.view
                .async_operations
                .begin_unique(DesktopAsyncOperationKind::StartupReadinessCheck);
        } else {
            self.view
                .async_operations
                .finish_kind(DesktopAsyncOperationKind::StartupReadinessCheck);
        }
        self.apply_startup_overlay();
    }

    pub fn finish_startup_provider_model_load(&mut self, infos: &[ProviderModelInfo]) {
        self.startup
            .complete_provider_catalog(&self.provider_config.effective_config, infos);
        self.sync_startup_readiness_operation();
        self.apply_startup_overlay();
    }

    pub fn fail_startup_provider_model_load(&mut self, message: impl Into<String>) {
        self.startup.fail_provider_catalog(message);
        self.sync_startup_readiness_operation();
        self.apply_startup_overlay();
    }

    pub fn begin_startup_docling_check(&mut self) -> bool {
        let should_probe = self
            .startup
            .begin_docling_check(&self.provider_config.effective_config);
        self.sync_startup_readiness_operation();
        self.apply_startup_overlay();
        should_probe
    }

    pub fn finish_startup_docling_check(&mut self, base_url: &str) {
        self.startup.complete_docling_check(base_url);
        self.sync_startup_readiness_operation();
        self.apply_startup_overlay();
    }

    pub fn fail_startup_docling_check(&mut self, message: impl Into<String>) {
        self.startup.fail_docling_check(message);
        self.sync_startup_readiness_operation();
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
        if self.snapshot.session_rows.is_empty() {
            -1
        } else {
            self.snapshot.selected_session_index as i32
        }
    }

    pub fn selected_session_id(&self) -> Option<SessionId> {
        self.snapshot.selected_session_id()
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

    pub fn begin_session_delete_mutation(&mut self) {
        self.view
            .async_operations
            .begin(DesktopAsyncOperationKind::SessionDelete);
    }

    pub fn finish_session_delete_mutation(&mut self) {
        self.view
            .async_operations
            .finish_one_kind(DesktopAsyncOperationKind::SessionDelete);
    }

    pub fn begin_project_delete_mutation(&mut self) {
        self.view
            .async_operations
            .begin(DesktopAsyncOperationKind::ProjectDelete);
    }

    pub fn finish_project_delete_mutation(&mut self) {
        self.view
            .async_operations
            .finish_one_kind(DesktopAsyncOperationKind::ProjectDelete);
    }

    pub fn background_mutation_pending(&self) -> bool {
        self.view
            .async_operations
            .is_pending(DesktopAsyncOperationKind::ProjectDelete)
            || self
                .view
                .async_operations
                .is_pending(DesktopAsyncOperationKind::SessionDelete)
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

    pub fn begin_current_todo_refresh(&mut self) {
        self.view
            .async_operations
            .begin(DesktopAsyncOperationKind::CurrentTodoRefresh);
    }

    pub fn finish_current_todo_refresh(&mut self) {
        self.view
            .async_operations
            .finish_one_kind(DesktopAsyncOperationKind::CurrentTodoRefresh);
    }

    pub fn async_polling_required(&self) -> bool {
        self.view.async_operations.polling_required()
            || self.is_busy()
            || self.app_state.permission.is_some()
            || self.startup.status == super::startup::DesktopStartupStatus::Loading
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

    fn sync_startup_readiness_operation(&mut self) {
        if self.startup.status == super::startup::DesktopStartupStatus::Loading {
            if !self
                .view
                .async_operations
                .is_pending(DesktopAsyncOperationKind::StartupReadinessCheck)
            {
                self.view
                    .async_operations
                    .begin_unique(DesktopAsyncOperationKind::StartupReadinessCheck);
            }
        } else {
            self.view
                .async_operations
                .finish_kind(DesktopAsyncOperationKind::StartupReadinessCheck);
        }
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
                transcript_text: "チャットはまだありません。".to_string(),
                transcript_rows: vec![crate::desktop::models::DesktopTranscriptRow {
                    kind: "system".to_string(),
                    step: "00".to_string(),
                    title: "チャットはありません".to_string(),
                    body: if self.selected_project_id().is_some() {
                        "下の入力欄から依頼を送ると、このプロジェクトの最初のチャットが作成されます。".to_string()
                    } else {
                        "通常チャットとして開始できます。プロジェクト作業をする場合は、左のプロジェクト作成からフォルダを選択してください。".to_string()
                    },
                    file_changes: Vec::new(),
                }],
                tool_status_text: "ツール実行はまだありません。".to_string(),
                progress_text: "待機中\nフェーズ: 準備完了\n手順: 実行中の作業はありません"
                    .to_string(),
                run_status_text: "待機中".to_string(),
                confirmation_text: String::new(),
                confirmation_visible: false,
                artifacts: Vec::new(),
                file_changes: Vec::new(),
                file_change_summary_text: "ファイル変更はまだありません。".to_string(),
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

    pub fn set_draft_prompt(&mut self, prompt: String) {
        self.composer.draft_prompt = prompt;
    }

    pub fn set_image_attachment_input(&mut self, input: String) {
        self.composer.image_attachment_input = input;
    }

    pub fn attach_image_from_input(&mut self) {
        let trimmed = self.composer.image_attachment_input.trim();
        if trimmed.is_empty() {
            self.set_status_message("Enter an image path before attaching.");
            return;
        }
        let path = camino::Utf8PathBuf::from(trimmed);
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

    pub fn set_provider_base_url_input(&mut self, input: String) {
        let normalized = normalize_provider_base_url(&input);
        self.provider_config.provider_base_url_input = input;
        self.provider_config.provider_loading = false;
        self.view
            .async_operations
            .finish_kind(DesktopAsyncOperationKind::ProviderModelCatalogLoad);
        if self.provider_config.provider_loaded_base_url.as_deref() != Some(normalized.as_str()) {
            self.provider_config.provider_loaded_base_url = None;
        }
        self.provider_config.provider_status_text =
            "Load the model list for this provider.".to_string();
    }

    pub fn load_open_session(
        &mut self,
        session: &SessionRecord,
        transcript: &Transcript,
        turn_items: &[TurnItem],
        state: SessionStateSnapshot,
        todos: Vec<TodoItem>,
    ) {
        let open_session = OpenSessionView::from_loaded(
            session,
            transcript,
            turn_items,
            state.clone(),
            todos.clone(),
        );
        if turn_items.is_empty() {
            self.app_state.load_transcript(transcript, state, todos);
        } else {
            self.app_state
                .load_turn_items(session, turn_items, state, todos);
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

    pub fn apply_run_event(&mut self, event: &crate::session::RunEvent) {
        self.app_state.apply_run_event(event);
        match event {
            crate::session::RunEvent::SessionStarted { session_id, title } => {
                self.update_session_row_title(*session_id, title);
                self.update_session_row_status(*session_id, SessionStatus::Running);
            }
            crate::session::RunEvent::SessionTitleUpdated { session_id, title } => {
                self.update_session_row_title(*session_id, title);
            }
            crate::session::RunEvent::SessionCompleted { session_id, .. } => {
                self.update_session_row_status(*session_id, SessionStatus::Completed);
            }
            crate::session::RunEvent::SessionAwaitingUser { session_id, .. } => {
                self.update_session_row_status(*session_id, SessionStatus::AwaitingUser);
            }
            crate::session::RunEvent::SessionInterrupted { session_id, .. } => {
                self.update_session_row_status(*session_id, SessionStatus::Cancelled);
            }
            crate::session::RunEvent::SessionFailed { session_id, .. } => {
                self.update_session_row_status(*session_id, SessionStatus::Failed);
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

    pub fn set_permission(&mut self, request: &PermissionRequest) {
        self.app_state.set_permission(request);
    }

    pub fn clear_permission(&mut self) {
        self.app_state.clear_permission();
    }

    pub fn mark_run_cancellation_requested(&mut self, reason: &str, status_message: &str) {
        self.app_state.run_status = RunStatus::Cancelled;
        self.app_state.permission = None;
        self.app_state.status_message = Some(status_message.to_string());
        self.app_state.progress.status = "Cancelled".to_string();
        self.app_state.progress.current_phase = "terminal".to_string();
        self.app_state.progress.active_step = reason.to_string();
        if let Some(session_id) = self.app_state.current_session_id {
            for row in self
                .snapshot
                .session_rows
                .iter_mut()
                .chain(self.snapshot.chat_session_rows.iter_mut())
            {
                if row.session_id == session_id {
                    row.set_status(SessionStatus::Cancelled);
                }
            }
        }
    }

    pub fn push_local_prompt_dispatch(&mut self, prompt_dispatch: &PromptDispatchPart) {
        self.app_state.push_local_prompt_dispatch(prompt_dispatch);
    }

    pub fn begin_prompt_enhance(&mut self, request_id: u64, raw_prompt: &str) {
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
        }
        active
    }

    pub fn set_review_draft(&mut self, draft: String) {
        self.composer.review_draft_text = draft.clone();
        self.app_state.update_prompt_review_draft(draft);
    }

    pub fn cancel_prompt_review(&mut self) {
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
    }

    pub fn start_new_chat(&mut self) {
        if self.is_busy() {
            self.set_status_message("new chat cannot start while a run is active");
            return;
        }
        self.snapshot.selected_session_index = self.snapshot.session_rows.len();
        self.app_state = AppState::default();
        self.open_session = None;
        self.composer.clear_request_inputs();
        self.view.artifact_selected_index = 0;
        self.view.overlay = DesktopOverlay::None;
        self.set_status_message("new chat ready");
    }

    pub fn reset_effective_config(&mut self, config: ResolvedConfig) {
        self.provider_config.replace_effective_config(config);
    }

    pub fn show_config_editor(&mut self) {
        self.provider_config.config_value_text = self
            .provider_config
            .config_editor
            .selected_field()
            .value
            .clone();
        self.view.overlay = DesktopOverlay::ConfigEditor;
    }

    pub fn show_provider_editor(&mut self) {
        self.provider_config.provider_base_url_input =
            self.provider_config.effective_config.model.base_url.clone();
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
        if self.provider_config.provider_status_text.is_empty() {
            self.provider_config.provider_status_text =
                "Load the model list for this provider.".to_string();
        }
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
        self.view.overlay = DesktopOverlay::None;
    }

    pub fn set_config_selection(&mut self, index: usize) {
        if index < self.provider_config.config_editor.fields.len() {
            self.provider_config.config_editor.selected = index;
            self.provider_config.config_value_text = self
                .provider_config
                .config_editor
                .selected_field()
                .value
                .clone();
        }
    }

    pub fn set_config_value(&mut self, value: String) {
        self.provider_config.config_value_text = value.clone();
        if let Some(field) = self
            .provider_config
            .config_editor
            .fields
            .get_mut(self.provider_config.config_editor.selected)
        {
            field.value = value;
        }
    }

    pub fn begin_provider_model_load(&mut self, normalized_base_url: String) {
        self.provider_config.provider_base_url_input = normalized_base_url;
        self.provider_config.provider_loading = true;
        self.view
            .async_operations
            .begin_unique(DesktopAsyncOperationKind::ProviderModelCatalogLoad);
        self.provider_config.provider_loaded_base_url = None;
        self.provider_config.provider_status_text =
            "Loading models in the background...".to_string();
    }

    pub fn finish_provider_model_load(&mut self, infos: Vec<ProviderModelInfo>) {
        let normalized_base_url =
            normalize_provider_base_url(&self.provider_config.provider_base_url_input);
        let models = infos.iter().map(|info| info.id.clone()).collect::<Vec<_>>();
        self.provider_config.provider_models =
            ensure_current_model(models, &self.provider_config.effective_config.model.model);
        self.provider_config.provider_model_infos =
            ensure_current_model_infos(infos, &self.provider_config.effective_config);
        self.provider_config.provider_selected_index = self
            .provider_config
            .provider_models
            .iter()
            .position(|model| model == &self.provider_config.effective_config.model.model)
            .map(|index| index as i32)
            .unwrap_or(-1);
        self.provider_config.provider_loaded_base_url = Some(normalized_base_url);
        self.provider_config.provider_loading = false;
        self.view
            .async_operations
            .finish_kind(DesktopAsyncOperationKind::ProviderModelCatalogLoad);
        self.provider_config.provider_status_text = format!(
            "Loaded {} models. {}",
            self.provider_config.provider_models.len(),
            self.selected_provider_model_info()
                .map(provider_model_summary)
                .unwrap_or_default()
        )
        .trim()
        .to_string();
    }

    pub fn fail_provider_model_load(&mut self, message: impl Into<String>) {
        self.provider_config.provider_loading = false;
        self.view
            .async_operations
            .finish_kind(DesktopAsyncOperationKind::ProviderModelCatalogLoad);
        self.provider_config.provider_loaded_base_url = None;
        self.provider_config.provider_status_text = message.into();
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

    pub fn set_provider_model_selection(&mut self, index: i32) {
        if index >= 0 && (index as usize) < self.provider_config.provider_models.len() {
            self.provider_config.provider_selected_index = index;
        }
    }

    pub fn set_provider_model_value(&mut self, value: &str) {
        let id = value.split("  [").next().unwrap_or(value).trim();
        if let Some(index) = self
            .provider_config
            .provider_models
            .iter()
            .position(|item| item == id)
        {
            self.provider_config.provider_selected_index = index as i32;
        }
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
        matches!(
            self.app_state.run_status,
            RunStatus::Running | RunStatus::Confirming
        )
    }

    pub fn can_submit_prompt(&self) -> bool {
        !self.is_busy() && !self.composer.draft_prompt.trim().is_empty()
    }

    pub fn can_open_session(&self) -> bool {
        !self.is_busy() && self.selected_session_id().is_some()
    }

    pub fn can_delete_session(&self) -> bool {
        !self.is_busy() && self.selected_session_id().is_some()
    }

    pub fn can_delete_project(&self) -> bool {
        !self.is_busy() && self.selected_project_id().is_some()
    }

    pub fn can_export_history(&self) -> bool {
        !self.is_busy() && self.selected_session_id().is_some()
    }

    pub fn can_apply_provider_selection(&self) -> bool {
        let normalized = normalize_provider_base_url(&self.provider_config.provider_base_url_input);
        !self.provider_config.provider_loading
            && !self
                .provider_config
                .provider_base_url_input
                .trim()
                .is_empty()
            && self.provider_config.provider_loaded_base_url.as_deref() == Some(normalized.as_str())
            && self.selected_provider_model().is_some()
    }

    fn with_provider_fields(mut self) -> Self {
        self.provider_config.provider_base_url_input =
            self.provider_config.effective_config.model.base_url.clone();
        self.provider_config.provider_loaded_base_url = Some(normalize_provider_base_url(
            &self.provider_config.effective_config.model.base_url,
        ));
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
        }
    }
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

fn ensure_current_model(mut models: Vec<String>, current_model: &str) -> Vec<String> {
    let current_model = current_model.trim();
    if !current_model.is_empty() && !models.iter().any(|model| model == current_model) {
        models.insert(0, current_model.to_string());
    }
    models
}

fn ensure_current_model_infos(
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
        loaded: false,
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
    use super::*;
    use crate::desktop::models::{DesktopProjectRow, DesktopSessionRow};
    use crate::session::ProjectId;

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
        assert_eq!(state.current_session_label(), "新規チャット");
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
    fn loaded_open_session_detail_keeps_elapsed_work_summary_title() {
        let project_id = ProjectId::new();
        let session_id = SessionId::new();
        let mut session = SessionRecord {
            id: session_id,
            project_id,
            title: "elapsed session".to_string(),
            status: crate::session::SessionStatus::Completed,
            cwd: camino::Utf8PathBuf::from("C:/workspace"),
            model: "model".to_string(),
            base_url: "http://localhost:1234".to_string(),
            created_at_ms: 1_000,
            updated_at_ms: 34_000,
            completed_at_ms: Some(34_000),
        };
        let turn_id = crate::protocol::TurnId::new();
        let turn_items = vec![
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id,
                turn_id,
                source_item_id: None,
                sequence_no: 1,
                payload: crate::protocol::TurnItemPayload::UserMessage {
                    text: "このworkspace内にある資料ってどんなものがありますか？".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id,
                turn_id,
                source_item_id: None,
                sequence_no: 2,
                payload: crate::protocol::TurnItemPayload::ToolStatus {
                    call_id: crate::session::ToolCallId::new(),
                    tool: crate::tool::ToolName::Shell,
                    status: crate::protocol::ToolLifecycleStatus::Completed,
                    title: "workspace scan".to_string(),
                },
            },
            TurnItem {
                id: crate::protocol::TurnItemId::new(),
                session_id,
                turn_id,
                source_item_id: None,
                sequence_no: 3,
                payload: crate::protocol::TurnItemPayload::AgentMessage {
                    text: "このワークスペースには資料があります。".to_string(),
                },
            },
        ];
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
        session.project_id = state.snapshot.project_rows[0].project_id;

        state.load_open_session(
            &session,
            &Transcript {
                session: session.clone(),
                messages: Vec::new(),
            },
            &turn_items,
            SessionStateSnapshot::default(),
            Vec::new(),
        );

        assert!(state.selected_detail().transcript_rows.iter().any(|row| {
            row.kind == "work_summary_completed" && row.title == "33s作業しました"
        }));
    }

    #[test]
    fn cancel_request_terminalizes_busy_projection_and_row_label() {
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

        state.mark_run_cancellation_requested("run cancelled by user", "停止しました。");

        assert!(!state.is_busy());
        assert_eq!(state.app_state.run_status, RunStatus::Cancelled);
        let short_id = session_id.to_string().chars().take(8).collect::<String>();
        assert_eq!(
            state.snapshot.session_rows[0].label,
            format!("Long task [停止済み] {short_id}")
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

        state.apply_run_event(&crate::session::RunEvent::SessionCompleted {
            session_id,
            finish_reason: None,
        });

        assert_eq!(state.app_state.run_status, RunStatus::Completed);
        let short_id = session_id.to_string().chars().take(8).collect::<String>();
        assert_eq!(
            state.snapshot.session_rows[0].label,
            format!("docx/xlsx要約 [完了] {short_id}")
        );
        assert!(!state.selected_session_title().contains("[実行中]"));
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

        state.begin_session_delete_mutation();
        state.begin_project_delete_mutation();
        assert!(state.background_mutation_pending());

        state.finish_session_delete_mutation();
        assert!(state.background_mutation_pending());

        state.finish_project_delete_mutation();
        assert!(!state.background_mutation_pending());

        state.finish_project_delete_mutation();
        assert!(!state.background_mutation_pending());
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

        state.begin_prompt_enhance(1, "first");
        state.begin_prompt_enhance(2, "second");

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

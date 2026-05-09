use crate::config::ResolvedConfig;
use crate::protocol::TurnItem;
use crate::session::{
    PromptDispatchPart, SessionId, SessionRecord, SessionStateSnapshot, TodoItem, Transcript,
};
use crate::tool::PermissionRequest;
use crate::tui::config_editor::ConfigEditorState;
use crate::tui::state::{AppState, RunStatus};

use super::models::{DesktopSessionDetail, DesktopSnapshot};
use super::query::build_session_detail_from_app_state;
use crate::llm::{ProviderModelInfo, normalize_provider_base_url};

pub const MIN_WINDOW_OPACITY_PERCENT: i32 = 55;
pub const MAX_WINDOW_OPACITY_PERCENT: i32 = 100;
pub const DEFAULT_WINDOW_OPACITY_PERCENT: i32 = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopOverlay {
    None,
    ConfigEditor,
    ProviderEditor,
    WorkspacePicker,
    PromptReview,
}

#[derive(Debug, Clone)]
pub struct DesktopState {
    pub snapshot: DesktopSnapshot,
    pub app_state: AppState,
    pub draft_prompt: String,
    pub image_attachment_input: String,
    pub image_attachment_paths: Vec<camino::Utf8PathBuf>,
    pub review_draft_text: String,
    pub workspace_input: String,
    pub overlay: DesktopOverlay,
    pub effective_config: ResolvedConfig,
    pub config_editor: ConfigEditorState,
    pub config_value_text: String,
    pub provider_base_url_input: String,
    pub provider_models: Vec<String>,
    pub provider_model_infos: Vec<ProviderModelInfo>,
    pub provider_selected_index: i32,
    pub provider_loaded_base_url: Option<String>,
    pub provider_status_text: String,
    pub provider_loading: bool,
    pub window_opacity_percent: i32,
}

impl DesktopState {
    pub fn new(snapshot: DesktopSnapshot, effective_config: ResolvedConfig) -> Self {
        let config_editor = ConfigEditorState::from_config(&effective_config);
        let config_value_text = config_editor.selected_field().value.clone();
        let provider_models = initial_provider_models(&effective_config);
        let provider_selected_index = provider_models
            .iter()
            .position(|model| model == &effective_config.model.model)
            .map(|index| index as i32)
            .unwrap_or(-1);
        let provider_model_infos = initial_provider_model_infos(&effective_config);
        Self {
            snapshot,
            app_state: AppState::default(),
            draft_prompt: String::new(),
            image_attachment_input: String::new(),
            image_attachment_paths: Vec::new(),
            review_draft_text: String::new(),
            workspace_input: String::new(),
            overlay: DesktopOverlay::None,
            effective_config,
            config_editor,
            config_value_text,
            provider_base_url_input: String::new(),
            provider_models,
            provider_model_infos,
            provider_selected_index,
            provider_loaded_base_url: None,
            provider_status_text: "Enter a provider URL, then load the model list.".to_string(),
            provider_loading: false,
            window_opacity_percent: DEFAULT_WINDOW_OPACITY_PERCENT,
        }
        .with_provider_fields()
    }

    pub fn replace_snapshot(&mut self, mut snapshot: DesktopSnapshot) {
        let preferred = self
            .selected_session_id()
            .or(self.app_state.current_session_id)
            .or_else(|| snapshot.selected_session_id());
        if let Some(session_id) = preferred {
            if let Some(index) = snapshot
                .session_rows
                .iter()
                .position(|row| row.session_id == session_id)
            {
                snapshot.selected_session_index = index;
            }
        }
        self.snapshot = snapshot;
    }

    pub fn select_session(&mut self, index: usize) {
        if index < self.snapshot.session_rows.len() {
            self.snapshot.selected_session_index = index;
        }
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
            .unwrap_or_else(|| "No session selected".to_string())
    }

    pub fn current_session_label(&self) -> String {
        self.app_state
            .current_session_id
            .map(|_| self.app_state.current_session_title.clone())
            .unwrap_or_else(|| "New Session".to_string())
    }

    pub fn selected_detail(&self) -> DesktopSessionDetail {
        if let Some(selected_id) = self.selected_session_id() {
            if self.app_state.current_session_id == Some(selected_id) {
                return build_session_detail_from_app_state(&self.app_state);
            }
            if let Some(detail) = self.snapshot.detail_for(selected_id) {
                return detail.clone();
            }
        }
        if self.app_state.current_session_id.is_some() {
            build_session_detail_from_app_state(&self.app_state)
        } else {
            DesktopSessionDetail {
                session_id: SessionId::new(),
                transcript_text: "No sessions available for this workspace.".to_string(),
                tool_status_text: "No tool activity recorded.".to_string(),
                progress_text: "Idle\nPhase: ready\nStep: No active run".to_string(),
                run_status_text: "Idle".to_string(),
                confirmation_text: String::new(),
                confirmation_visible: false,
            }
        }
    }

    pub fn current_run_status_text(&self) -> String {
        if self.app_state.current_session_id.is_some() {
            build_session_detail_from_app_state(&self.app_state).run_status_text
        } else {
            self.selected_detail().run_status_text
        }
    }

    pub fn set_draft_prompt(&mut self, prompt: String) {
        self.draft_prompt = prompt;
    }

    pub fn set_image_attachment_input(&mut self, input: String) {
        self.image_attachment_input = input;
    }

    pub fn attach_image_from_input(&mut self) {
        let trimmed = self.image_attachment_input.trim();
        if trimmed.is_empty() {
            self.set_status_message("Enter an image path before attaching.");
            return;
        }
        let path = camino::Utf8PathBuf::from(trimmed);
        if self
            .image_attachment_paths
            .iter()
            .any(|existing| existing == &path)
        {
            self.set_status_message("Image is already attached.");
            return;
        }
        self.image_attachment_paths.push(path);
        self.image_attachment_input.clear();
        self.set_status_message("Image attached to the next prompt.");
    }

    pub fn attach_image_path(&mut self, path: camino::Utf8PathBuf) {
        if self
            .image_attachment_paths
            .iter()
            .any(|existing| existing == &path)
        {
            self.set_status_message("Image is already attached.");
            return;
        }
        self.image_attachment_paths.push(path);
        self.image_attachment_input.clear();
        self.set_status_message("Image attached to the next prompt.");
    }

    pub fn clear_image_attachments(&mut self) {
        self.image_attachment_paths.clear();
        self.image_attachment_input.clear();
        self.set_status_message("Image attachments cleared.");
    }

    pub fn image_attachment_summary(&self) -> String {
        match self.image_attachment_paths.len() {
            0 => "No images attached".to_string(),
            1 => format!("1 image: {}", self.image_attachment_paths[0]),
            count => format!("{count} images attached"),
        }
    }

    pub fn set_workspace_input(&mut self, input: String) {
        self.workspace_input = input;
    }

    pub fn set_provider_base_url_input(&mut self, input: String) {
        let normalized = normalize_provider_base_url(&input);
        self.provider_base_url_input = input;
        self.provider_loading = false;
        if self.provider_loaded_base_url.as_deref() != Some(normalized.as_str()) {
            self.provider_loaded_base_url = None;
        }
        self.provider_status_text = "Load the model list for this provider.".to_string();
    }

    pub fn load_open_session(
        &mut self,
        session: &SessionRecord,
        transcript: &Transcript,
        turn_items: &[TurnItem],
        state: SessionStateSnapshot,
        todos: Vec<TodoItem>,
    ) {
        if turn_items.is_empty() {
            self.app_state.load_transcript(transcript, state, todos);
        } else {
            self.app_state
                .load_turn_items(session, turn_items, state, todos);
        }
        if let Some(index) = self
            .snapshot
            .session_rows
            .iter()
            .position(|row| Some(row.session_id) == self.app_state.current_session_id)
        {
            self.snapshot.selected_session_index = index;
        }
        self.overlay = DesktopOverlay::None;
    }

    pub fn apply_run_event(&mut self, event: &crate::session::RunEvent) {
        self.app_state.apply_run_event(event);
    }

    pub fn set_permission(&mut self, request: &PermissionRequest) {
        self.app_state.set_permission(request);
    }

    pub fn clear_permission(&mut self) {
        self.app_state.clear_permission();
    }

    pub fn push_local_prompt_dispatch(&mut self, prompt_dispatch: &PromptDispatchPart) {
        self.app_state.push_local_prompt_dispatch(prompt_dispatch);
    }

    pub fn begin_prompt_enhance(&mut self, request_id: u64, raw_prompt: &str) {
        self.app_state.begin_prompt_enhance(request_id, raw_prompt);
        self.review_draft_text.clear();
        self.overlay = DesktopOverlay::PromptReview;
    }

    pub fn finish_prompt_enhance(&mut self, request_id: u64, draft: String) -> bool {
        let finished = self
            .app_state
            .finish_prompt_enhance(request_id, draft.clone());
        if finished {
            self.review_draft_text = draft;
            self.overlay = DesktopOverlay::PromptReview;
        }
        finished
    }

    pub fn set_review_draft(&mut self, draft: String) {
        self.review_draft_text = draft.clone();
        self.app_state.update_prompt_review_draft(draft);
    }

    pub fn cancel_prompt_review(&mut self) {
        self.app_state.cancel_prompt_review();
        self.review_draft_text.clear();
        if self.overlay == DesktopOverlay::PromptReview {
            self.overlay = DesktopOverlay::None;
        }
    }

    pub fn build_prompt_dispatch(&self, send_enhanced: bool) -> Option<PromptDispatchPart> {
        self.app_state.build_prompt_dispatch(send_enhanced)
    }

    pub fn set_status_message(&mut self, message: impl Into<String>) {
        self.app_state.status_message = Some(message.into());
    }

    pub fn reset_effective_config(&mut self, config: ResolvedConfig) {
        self.effective_config = config.clone();
        self.config_editor = ConfigEditorState::from_config(&config);
        self.config_value_text = self.config_editor.selected_field().value.clone();
        self.provider_base_url_input = config.model.base_url.clone();
        self.provider_models = initial_provider_models(&config);
        self.provider_model_infos = initial_provider_model_infos(&config);
        self.provider_selected_index = self
            .provider_models
            .iter()
            .position(|model| model == &config.model.model)
            .map(|index| index as i32)
            .unwrap_or(-1);
        self.provider_loaded_base_url = Some(normalize_provider_base_url(&config.model.base_url));
        self.provider_loading = false;
        self.provider_status_text = "Load the model list for this provider.".to_string();
    }

    pub fn show_config_editor(&mut self) {
        self.config_value_text = self.config_editor.selected_field().value.clone();
        self.overlay = DesktopOverlay::ConfigEditor;
    }

    pub fn show_provider_editor(&mut self) {
        self.provider_base_url_input = self.effective_config.model.base_url.clone();
        self.provider_models = ensure_current_model(
            self.provider_models.clone(),
            &self.effective_config.model.model,
        );
        self.provider_model_infos =
            ensure_current_model_info(self.provider_model_infos.clone(), &self.effective_config);
        self.provider_selected_index = self
            .provider_models
            .iter()
            .position(|model| model == &self.effective_config.model.model)
            .map(|index| index as i32)
            .unwrap_or(-1);
        if self.provider_status_text.is_empty() {
            self.provider_status_text = "Load the model list for this provider.".to_string();
        }
        self.overlay = DesktopOverlay::ProviderEditor;
    }

    pub fn show_workspace_picker(&mut self, current_path: &str) {
        self.workspace_input = current_path.to_string();
        self.overlay = DesktopOverlay::WorkspacePicker;
    }

    pub fn hide_overlay(&mut self) {
        self.overlay = DesktopOverlay::None;
    }

    pub fn set_config_selection(&mut self, index: usize) {
        if index < self.config_editor.fields.len() {
            self.config_editor.selected = index;
            self.config_value_text = self.config_editor.selected_field().value.clone();
        }
    }

    pub fn set_config_value(&mut self, value: String) {
        self.config_value_text = value.clone();
        if let Some(field) = self
            .config_editor
            .fields
            .get_mut(self.config_editor.selected)
        {
            field.value = value;
        }
    }

    pub fn begin_provider_model_load(&mut self, normalized_base_url: String) {
        self.provider_base_url_input = normalized_base_url;
        self.provider_loading = true;
        self.provider_loaded_base_url = None;
        self.provider_status_text = "Loading models in the background...".to_string();
    }

    pub fn finish_provider_model_load(&mut self, infos: Vec<ProviderModelInfo>) {
        let normalized_base_url = normalize_provider_base_url(&self.provider_base_url_input);
        let models = infos.iter().map(|info| info.id.clone()).collect::<Vec<_>>();
        self.provider_models = ensure_current_model(models, &self.effective_config.model.model);
        self.provider_model_infos = ensure_current_model_infos(infos, &self.effective_config);
        self.provider_selected_index = self
            .provider_models
            .iter()
            .position(|model| model == &self.effective_config.model.model)
            .map(|index| index as i32)
            .unwrap_or(-1);
        self.provider_loaded_base_url = Some(normalized_base_url);
        self.provider_loading = false;
        self.provider_status_text = format!(
            "Loaded {} models. {}",
            self.provider_models.len(),
            self.selected_provider_model_info()
                .map(provider_model_summary)
                .unwrap_or_default()
        )
        .trim()
        .to_string();
    }

    pub fn fail_provider_model_load(&mut self, message: impl Into<String>) {
        self.provider_loading = false;
        self.provider_loaded_base_url = None;
        self.provider_status_text = message.into();
        self.provider_models = ensure_current_model(
            self.provider_models.clone(),
            &self.effective_config.model.model,
        );
        if self.provider_selected_index < 0 && !self.provider_models.is_empty() {
            self.provider_selected_index = 0;
        }
    }

    pub fn set_provider_model_selection(&mut self, index: i32) {
        if index >= 0 && (index as usize) < self.provider_models.len() {
            self.provider_selected_index = index;
        }
    }

    pub fn set_provider_model_value(&mut self, value: &str) {
        let id = value.split("  [").next().unwrap_or(value).trim();
        if let Some(index) = self.provider_models.iter().position(|item| item == id) {
            self.provider_selected_index = index as i32;
        }
    }

    pub fn selected_provider_model(&self) -> Option<&str> {
        self.provider_models
            .get(self.provider_selected_index.max(0) as usize)
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
    }

    pub fn selected_provider_model_info(&self) -> Option<&ProviderModelInfo> {
        let selected = self.selected_provider_model()?;
        self.provider_model_infos
            .iter()
            .find(|info| info.id == selected)
    }

    pub fn set_window_opacity_percent(&mut self, value: i32) {
        self.window_opacity_percent =
            value.clamp(MIN_WINDOW_OPACITY_PERCENT, MAX_WINDOW_OPACITY_PERCENT);
    }

    pub fn is_busy(&self) -> bool {
        matches!(
            self.app_state.run_status,
            RunStatus::Running | RunStatus::Confirming
        )
    }

    pub fn can_submit_prompt(&self) -> bool {
        !self.is_busy() && !self.draft_prompt.trim().is_empty()
    }

    pub fn can_open_session(&self) -> bool {
        !self.is_busy() && self.selected_session_id().is_some()
    }

    pub fn can_export_history(&self) -> bool {
        !self.is_busy() && self.selected_session_id().is_some()
    }

    pub fn can_apply_provider_selection(&self) -> bool {
        let normalized = normalize_provider_base_url(&self.provider_base_url_input);
        !self.provider_loading
            && !self.provider_base_url_input.trim().is_empty()
            && self.provider_loaded_base_url.as_deref() == Some(normalized.as_str())
            && self.selected_provider_model().is_some()
    }

    fn with_provider_fields(mut self) -> Self {
        self.provider_base_url_input = self.effective_config.model.base_url.clone();
        self.provider_loaded_base_url = Some(normalize_provider_base_url(
            &self.effective_config.model.base_url,
        ));
        self
    }
}

fn initial_provider_models(config: &ResolvedConfig) -> Vec<String> {
    ensure_current_model(Vec::new(), &config.model.model)
}

fn initial_provider_model_infos(config: &ResolvedConfig) -> Vec<ProviderModelInfo> {
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

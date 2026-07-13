use crate::config::{AccessMode, ProviderMetadataMode, ResolvedConfig};
use crate::llm::{ProviderModelInfo, normalize_provider_base_url};
use crate::tui::config_editor::{ConfigEditorState, ConfigField};

use super::state::{
    ensure_current_model, ensure_current_model_infos, initial_provider_model_infos,
    initial_provider_models,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopProviderStatusKind {
    Idle,
    Loading,
    Success,
    Warning,
    Error,
}

impl DesktopProviderStatusKind {
    pub fn key(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Loading => "loading",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopProviderStatus {
    pub kind: DesktopProviderStatusKind,
    pub title: String,
    pub hint: String,
    pub details: String,
}

impl DesktopProviderStatus {
    pub fn idle(title: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            kind: DesktopProviderStatusKind::Idle,
            title: title.into(),
            hint: hint.into(),
            details: String::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DesktopProviderConfigState {
    pub effective_config: ResolvedConfig,
    pub config_editor: ConfigEditorState,
    pub config_generation: u64,
    pub config_value_text: String,
    pub provider_base_url_input: String,
    pub provider_models: Vec<String>,
    pub provider_model_infos: Vec<ProviderModelInfo>,
    pub provider_selected_index: i32,
    pub provider_metadata_mode_input: ProviderMetadataMode,
    pub provider_context_window_input: String,
    pub provider_max_output_tokens_input: String,
    pub provider_selected_model_id_input: String,
    pub provider_loaded_base_url: Option<String>,
    pub provider_status: DesktopProviderStatus,
    pub provider_loading: bool,
}

impl DesktopProviderConfigState {
    pub fn new(effective_config: ResolvedConfig) -> Self {
        let config_editor = ConfigEditorState::from_config(&effective_config);
        let config_value_text = config_editor.selected_field().value.clone();
        let provider_models = initial_provider_models(&effective_config);
        let provider_selected_index = provider_models
            .iter()
            .position(|model| model == &effective_config.model.model)
            .map(|index| index as i32)
            .unwrap_or(-1);
        let provider_model_infos = initial_provider_model_infos(&effective_config);
        let provider_metadata_mode_input = effective_config.model.provider_metadata_mode;
        let provider_context_window_input = effective_config.model.context_window.to_string();
        let provider_max_output_tokens_input = effective_config.model.max_output_tokens.to_string();
        let provider_selected_model_id_input = effective_config.model.model.clone();
        let provider_status = DesktopProviderStatus::idle(
            "Provider 設定を確認できます",
            "Base URL、mode、model を選択してセッションへ適用できます。",
        );
        Self {
            effective_config,
            config_editor,
            config_generation: 1,
            config_value_text,
            provider_base_url_input: String::new(),
            provider_models,
            provider_model_infos,
            provider_selected_index,
            provider_metadata_mode_input,
            provider_context_window_input,
            provider_max_output_tokens_input,
            provider_selected_model_id_input,
            provider_loaded_base_url: None,
            provider_status,
            provider_loading: false,
        }
    }

    pub fn replace_effective_config(&mut self, config: ResolvedConfig) {
        let normalized_base_url = normalize_provider_base_url(&config.model.base_url);
        let preserve_loaded_catalog = !self.provider_loading
            && self.provider_loaded_base_url.as_deref() == Some(normalized_base_url.as_str())
            && self.provider_metadata_mode_input == config.model.provider_metadata_mode;
        let retained_models = preserve_loaded_catalog.then(|| self.provider_models.clone());
        let retained_model_infos =
            preserve_loaded_catalog.then(|| self.provider_model_infos.clone());
        let retained_status = preserve_loaded_catalog.then(|| self.provider_status.clone());
        self.config_generation = self.config_generation.saturating_add(1);
        self.effective_config = config.clone();
        self.config_editor = ConfigEditorState::from_config(&config);
        self.config_value_text = self.config_editor.selected_field().value.clone();
        self.provider_base_url_input = config.model.base_url.clone();
        self.provider_models = retained_models
            .map(|models| ensure_current_model(models, &config.model.model))
            .unwrap_or_else(|| initial_provider_models(&config));
        self.provider_model_infos = retained_model_infos
            .map(|infos| ensure_current_model_infos(infos, &config))
            .unwrap_or_else(|| initial_provider_model_infos(&config));
        self.provider_selected_index = self
            .provider_models
            .iter()
            .position(|model| model == &config.model.model)
            .map(|index| index as i32)
            .unwrap_or(-1);
        self.provider_metadata_mode_input = config.model.provider_metadata_mode;
        self.provider_context_window_input = config.model.context_window.to_string();
        self.provider_max_output_tokens_input = config.model.max_output_tokens.to_string();
        self.provider_selected_model_id_input = config.model.model.clone();
        self.provider_loaded_base_url = preserve_loaded_catalog.then_some(normalized_base_url);
        self.provider_loading = false;
        self.provider_status = retained_status.unwrap_or_else(|| {
            DesktopProviderStatus::idle(
                "Provider 設定を確認できます",
                "Base URL、mode、model を選択してセッションへ適用できます。",
            )
        });
    }

    pub fn update_access_mode(&mut self, access_mode: AccessMode) {
        self.effective_config.permissions.access_mode = access_mode;
        let value = access_mode.as_str().to_string();
        let mut selected_access_mode = false;
        if let Some((index, field)) = self
            .config_editor
            .fields
            .iter_mut()
            .enumerate()
            .find(|(_, field)| field.key == ConfigField::AccessMode)
        {
            field.value = value.clone();
            field.dirty = false;
            selected_access_mode = self.config_editor.selected == index;
        }
        if selected_access_mode {
            self.config_value_text = value;
        }
    }

    pub fn set_status(
        &mut self,
        kind: DesktopProviderStatusKind,
        title: impl Into<String>,
        hint: impl Into<String>,
        details: impl Into<String>,
    ) {
        let title = title.into();
        let hint = hint.into();
        let details = details.into();
        self.provider_status = DesktopProviderStatus {
            kind,
            title,
            hint,
            details,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_only_update_preserves_generation_catalog_and_unrelated_editor_drafts() {
        let mut state = DesktopProviderConfigState::new(ResolvedConfig::default());
        state.provider_base_url_input = state.effective_config.model.base_url.clone();
        state.provider_loaded_base_url =
            Some(normalize_provider_base_url(&state.provider_base_url_input));
        state.set_status(
            DesktopProviderStatusKind::Success,
            "catalog loaded",
            "catalog remains owned",
            "evidence",
        );
        let generation = state.config_generation;
        let loaded_base_url = state.provider_loaded_base_url.clone();
        let provider_status = state.provider_status.clone();
        let model_ids = state
            .provider_model_infos
            .iter()
            .map(|model| model.id.clone())
            .collect::<Vec<_>>();
        let model = state
            .config_editor
            .fields
            .iter_mut()
            .find(|field| field.key == ConfigField::Model)
            .expect("model field");
        model.value = "unsaved-model-draft".to_string();
        model.dirty = true;

        state.update_access_mode(AccessMode::AutoReview);

        assert_eq!(state.config_generation, generation);
        assert_eq!(state.provider_loaded_base_url, loaded_base_url);
        assert_eq!(state.provider_status, provider_status);
        assert_eq!(
            state
                .provider_model_infos
                .iter()
                .map(|model| model.id.clone())
                .collect::<Vec<_>>(),
            model_ids
        );
        assert_eq!(
            state.effective_config.permissions.access_mode,
            AccessMode::AutoReview
        );
        let model = state
            .config_editor
            .fields
            .iter()
            .find(|field| field.key == ConfigField::Model)
            .expect("model field");
        assert_eq!(model.value, "unsaved-model-draft");
        assert!(model.dirty);
        let access = state
            .config_editor
            .fields
            .iter()
            .find(|field| field.key == ConfigField::AccessMode)
            .expect("access field");
        assert_eq!(access.value, "auto_review");
        assert!(!access.dirty);
    }

    #[test]
    fn selected_access_editor_baseline_tracks_the_access_only_update() {
        let mut state = DesktopProviderConfigState::new(ResolvedConfig::default());
        state.config_editor.selected = state
            .config_editor
            .fields
            .iter()
            .position(|field| field.key == ConfigField::AccessMode)
            .expect("access field");
        state.config_value_text = "default".to_string();

        state.update_access_mode(AccessMode::FullAccess);

        assert_eq!(state.config_value_text, "full_access");
    }
}

use crate::config::ResolvedConfig;
use crate::llm::{ProviderModelInfo, normalize_provider_base_url};
use crate::tui::config_editor::ConfigEditorState;

use super::state::{initial_provider_model_infos, initial_provider_models};

#[derive(Debug, Clone)]
pub struct DesktopProviderConfigState {
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
        Self {
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
        }
    }

    pub fn replace_effective_config(&mut self, config: ResolvedConfig) {
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
}

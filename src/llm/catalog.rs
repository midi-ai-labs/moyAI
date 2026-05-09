use crate::config::ResolvedConfig;
use crate::error::LlmError;
use crate::llm::{ModelCapabilities, ModelProfile};

pub trait ModelCatalog: Send + Sync {
    fn default_model(&self) -> Result<ModelProfile, LlmError>;
    fn resolve(&self, requested: Option<&str>) -> Result<ModelProfile, LlmError>;
}

#[derive(Debug, Clone)]
pub struct ConfigModelCatalog {
    config: ResolvedConfig,
}

impl ConfigModelCatalog {
    pub fn new(config: ResolvedConfig) -> Self {
        Self { config }
    }

    fn build_profile(&self, name: String) -> ModelProfile {
        ModelProfile {
            name,
            context_window: self.config.model.context_window,
            max_output_tokens: self.config.model.max_output_tokens,
            capabilities: ModelCapabilities {
                supports_tools: self.config.model.supports_tools,
                supports_reasoning: self.config.model.supports_reasoning,
            },
        }
    }
}

impl ModelCatalog for ConfigModelCatalog {
    fn default_model(&self) -> Result<ModelProfile, LlmError> {
        Ok(self.build_profile(self.config.model.model.clone()))
    }

    fn resolve(&self, requested: Option<&str>) -> Result<ModelProfile, LlmError> {
        match requested {
            Some(model) if !model.trim().is_empty() => Ok(self.build_profile(model.to_string())),
            _ => self.default_model(),
        }
    }
}

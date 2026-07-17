use crate::config::ResolvedConfig;
use crate::error::LlmError;
use crate::llm::ModelProfile;
use crate::llm::model_policy::ModelPolicy;

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

    fn build_profile(&self) -> ModelProfile {
        ModelPolicy::from_config(&self.config)
            .transport_profile(self.config.model.provider_metadata_mode)
    }
}

impl ModelCatalog for ConfigModelCatalog {
    fn default_model(&self) -> Result<ModelProfile, LlmError> {
        Ok(self.build_profile())
    }

    fn resolve(&self, requested: Option<&str>) -> Result<ModelProfile, LlmError> {
        match requested {
            Some(model) if !model.trim().is_empty() && model.trim() == self.config.model.model => {
                self.default_model()
            }
            Some(model) if !model.trim().is_empty() => Err(LlmError::Message(format!(
                "model `{model}` has no explicit capability profile; configure it before resolution"
            ))),
            _ => self.default_model(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_does_not_inherit_capabilities_for_an_id_only_override() {
        let config = ResolvedConfig::default();
        let catalog = ConfigModelCatalog::new(config.clone());

        let profile = catalog.default_model().expect("configured profile");
        assert_eq!(profile.name, config.model.model);
        assert_eq!(
            profile.capabilities.supports_reasoning,
            config.model.supports_reasoning
        );
        let error = catalog
            .resolve(Some("unprofiled-model"))
            .expect_err("id-only override must fail closed");
        assert!(error.to_string().contains("no explicit capability profile"));
    }
}

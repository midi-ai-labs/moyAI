use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::agent::mode::CollaborationMode;
use crate::config::ResolvedConfig;
use crate::config::model::{
    ProviderApiMode, ProviderReasoningCapability, ReasoningEffort, ReasoningSummary,
};
use crate::error::AgentError;
use crate::llm::{ModelCapabilities, ModelProfile, ReasoningRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputModality {
    Text,
    Image,
}

#[derive(Debug, Clone)]
pub struct ModelPolicy {
    pub id: String,
    pub base_instructions: String,
    pub default_reasoning: Option<ReasoningEffort>,
    pub context_window: u32,
    pub auto_compact_token_limit: u32,
    pub max_output_tokens: u32,
    pub input_modalities: BTreeSet<InputModality>,
    pub supports_tools: bool,
    pub supports_parallel_tool_calls: bool,
}

impl ModelPolicy {
    pub fn from_config(config: &ResolvedConfig) -> Self {
        let mut input_modalities = BTreeSet::from([InputModality::Text]);
        if config.model.supports_images {
            input_modalities.insert(InputModality::Image);
        }
        let compact_margin = config
            .model
            .max_output_tokens
            .saturating_add(config.session.overflow_margin_tokens as u32)
            .saturating_add(config.model.context_window / 20);
        Self {
            id: config.model.model.clone(),
            base_instructions: format!(
                "{}\n\n{}",
                include_str!("../../assets/prompts/system.md").trim(),
                include_str!("../../assets/prompts/profile_default.md").trim()
            ),
            default_reasoning: config.model.reasoning_effort.clone(),
            context_window: config.model.context_window,
            auto_compact_token_limit: config.model.context_window.saturating_sub(compact_margin),
            max_output_tokens: config.model.max_output_tokens,
            input_modalities,
            supports_tools: config.model.supports_tools,
            supports_parallel_tool_calls: config.model.parallel_tool_calls,
        }
    }

    pub fn transport_profile(
        &self,
        provider_metadata_mode: crate::config::ProviderMetadataMode,
    ) -> ModelProfile {
        ModelProfile {
            name: self.id.clone(),
            context_window: self.context_window,
            max_output_tokens: self.max_output_tokens,
            provider_metadata_mode,
            capabilities: ModelCapabilities {
                supports_tools: self.supports_tools,
                supports_reasoning: self.default_reasoning.is_some(),
                supports_images: self.input_modalities.contains(&InputModality::Image),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub api_mode: ProviderApiMode,
    pub reasoning: ProviderReasoningCapability,
}

impl ProviderCapabilities {
    pub fn from_config(config: &ResolvedConfig) -> Self {
        let api_mode = config
            .model
            .provider_api_mode
            .resolved_for_provider_metadata_mode(config.model.provider_metadata_mode);
        let reasoning = match api_mode {
            ProviderApiMode::Auto => ProviderReasoningCapability::Unsupported,
            ProviderApiMode::ChatCompletions => config
                .model
                .chat_completions_reasoning_parameters
                .map(|parameters| ProviderReasoningCapability::ChatCompletions { parameters })
                .unwrap_or(ProviderReasoningCapability::Unsupported),
            ProviderApiMode::Responses => ProviderReasoningCapability::Responses {
                supports_summary: true,
                supports_previous_response_id: true,
            },
        };
        Self {
            api_mode,
            reasoning,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedTurnPolicy {
    pub model: ModelPolicy,
    pub provider: ProviderCapabilities,
    pub reasoning: Option<ReasoningRequest>,
}

impl ResolvedTurnPolicy {
    pub fn resolve(
        mode: &CollaborationMode,
        mut model: ModelPolicy,
        provider: ProviderCapabilities,
        reasoning_summary: ReasoningSummary,
    ) -> Result<Self, AgentError> {
        if let Some(model_override) = &mode.model_override {
            model.id.clone_from(model_override);
        }
        let effort = mode
            .reasoning_effort_override
            .clone()
            .or_else(|| model.default_reasoning.clone());
        let reasoning = ReasoningRequest {
            effort,
            summary: reasoning_summary,
        };
        let reasoning = (!reasoning.is_disabled()).then_some(reasoning);
        if reasoning.is_some()
            && matches!(provider.reasoning, ProviderReasoningCapability::Unsupported)
        {
            return Err(AgentError::Message(format!(
                "reasoning was requested for model `{}`, but the selected provider mode does not support it",
                model.id
            )));
        }
        Ok(Self {
            model,
            provider,
            reasoning,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::mode::{CollaborationMode, ModeKind};

    #[test]
    fn model_policy_does_not_branch_on_model_name_substrings() {
        let mut qwen = ResolvedConfig::default();
        qwen.model.model = "qwen/example".to_string();
        let mut other = qwen.clone();
        other.model.model = "other/example".to_string();
        assert_eq!(
            ModelPolicy::from_config(&qwen).base_instructions,
            ModelPolicy::from_config(&other).base_instructions
        );
    }

    #[test]
    fn turn_policy_uses_the_single_model_tool_capability_owner() {
        let config = ResolvedConfig::default();
        let resolved = ResolvedTurnPolicy::resolve(
            &CollaborationMode::resolve(ModeKind::Default),
            ModelPolicy::from_config(&config),
            ProviderCapabilities::from_config(&config),
            ReasoningSummary::None,
        )
        .expect("policy");
        assert_eq!(
            resolved.model.supports_parallel_tool_calls,
            config.model.parallel_tool_calls
        );
    }
}

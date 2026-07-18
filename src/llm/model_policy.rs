use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

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
    pub working_context_token_limit: u32,
    pub max_output_tokens: u32,
    pub input_modalities: BTreeSet<InputModality>,
    pub supports_tools: bool,
    pub supports_reasoning: bool,
    pub supports_parallel_tool_calls: bool,
}

impl ModelPolicy {
    pub fn from_config(config: &ResolvedConfig) -> Self {
        let mut input_modalities = BTreeSet::from([InputModality::Text]);
        if config.model.supports_images {
            input_modalities.insert(InputModality::Image);
        }
        let hard_request_limit = config.model.context_window.saturating_sub(
            config
                .model
                .max_output_tokens
                .saturating_add(config.session.overflow_margin_tokens as u32),
        );
        // Keep the normal working set below 40% of the advertised context.
        // For the default 131,072-token profile this triggers before a roughly
        // 52k-token full-history resend, while the hard request limit remains a
        // separate last-resort safety boundary.
        let working_context_target = config.model.context_window.saturating_mul(2) / 5;
        let compact_margin = config
            .model
            .max_output_tokens
            .saturating_add(config.session.overflow_margin_tokens as u32)
            .saturating_add(config.model.context_window / 20);
        let legacy_margin_limit = config.model.context_window.saturating_sub(compact_margin);
        Self {
            id: config.model.model.clone(),
            base_instructions: format!(
                "{}\n\n{}",
                include_str!("../../assets/prompts/system.md").trim(),
                include_str!("../../assets/prompts/profile_default.md").trim()
            ),
            default_reasoning: config.model.reasoning_effort.clone(),
            context_window: config.model.context_window,
            working_context_token_limit: working_context_target
                .min(hard_request_limit)
                .min(legacy_margin_limit),
            max_output_tokens: config.model.max_output_tokens,
            input_modalities,
            supports_tools: config.model.supports_tools,
            supports_reasoning: config.model.supports_reasoning,
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
                supports_reasoning: self.supports_reasoning,
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
        let api_mode = config.model.provider_api_mode;
        let reasoning = match api_mode {
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
        model: ModelPolicy,
        provider: ProviderCapabilities,
        reasoning_summary: ReasoningSummary,
    ) -> Result<Self, AgentError> {
        if let Some(model_override) = &mode.model_override {
            if model_override.trim() != model.id {
                return Err(AgentError::Message(format!(
                    "model override `{model_override}` has no explicit capability profile; configure that model before admitting the turn"
                )));
            }
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
        if reasoning.is_some() && !model.supports_reasoning {
            return Err(AgentError::Message(format!(
                "reasoning was requested for model `{}`, but its configured capability profile does not support reasoning",
                model.id
            )));
        }
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
        assert_eq!(
            resolved.model.supports_reasoning,
            config.model.supports_reasoning
        );
    }

    #[test]
    fn model_reasoning_capability_is_independent_from_default_reasoning_request() {
        let mut config = ResolvedConfig::default();
        config.model.supports_reasoning = true;
        config.model.reasoning_effort = None;
        assert!(
            ModelPolicy::from_config(&config)
                .transport_profile(config.model.provider_metadata_mode)
                .capabilities
                .supports_reasoning
        );

        config.model.supports_reasoning = false;
        config.model.reasoning_effort = Some(ReasoningEffort::High);
        assert!(
            !ModelPolicy::from_config(&config)
                .transport_profile(config.model.provider_metadata_mode)
                .capabilities
                .supports_reasoning
        );
    }

    #[test]
    fn unprofiled_mode_model_override_fails_closed() {
        let config = ResolvedConfig::default();
        let mut mode = CollaborationMode::resolve(ModeKind::Default);
        mode.model_override = Some("unprofiled-model".to_string());
        let error = ResolvedTurnPolicy::resolve(
            &mode,
            ModelPolicy::from_config(&config),
            ProviderCapabilities::from_config(&config),
            ReasoningSummary::None,
        )
        .expect_err("capabilities must not be inherited by an id-only override");
        assert!(error.to_string().contains("no explicit capability profile"));
    }

    #[test]
    fn default_working_context_triggers_before_fifty_two_k_resend() {
        let config = ResolvedConfig::default();
        let policy = ModelPolicy::from_config(&config);
        assert!(policy.working_context_token_limit <= 52_428);
        assert!(policy.working_context_token_limit < config.model.context_window);
    }
}

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
    /// Provider-advertised context window before Codex-style input headroom.
    pub context_window: u32,
    pub working_context_token_limit: u32,
    /// Effective full input limit after Codex's 95% headroom and any
    /// non-inverting configured overflow margin.
    pub effective_context_token_limit: u32,
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
        let context_limits = ContextWindowLimits::resolve(
            config.model.context_window,
            config.session.overflow_margin_tokens,
        );
        Self {
            id: config.model.model.clone(),
            base_instructions: format!(
                "{}\n\n{}",
                include_str!("../../assets/prompts/system.md").trim(),
                include_str!("../../assets/prompts/profile_default.md").trim()
            ),
            default_reasoning: config.model.reasoning_effort.clone(),
            context_window: config.model.context_window,
            working_context_token_limit: context_limits.working,
            effective_context_token_limit: context_limits.effective_full,
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
            // Transport admission receives the immutable effective input
            // window. The advertised value remains on ModelPolicy and in the
            // resolved config used for provider-specific `num_ctx`.
            context_window: self.effective_context_token_limit,
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

/// The single owner of the relationship between advertised, working, and hard
/// context limits.
///
/// Codex starts automatic compaction at 90% of the advertised context and
/// treats 95% as the effective full input window. A configured overflow margin
/// may lower the hard limit only when the result remains strictly above the
/// working limit. `max_output_tokens` is intentionally absent: it is a
/// generation cap, not an input reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContextWindowLimits {
    pub working: u32,
    pub effective_full: u32,
}

impl ContextWindowLimits {
    pub(crate) fn resolve(
        advertised_context_window: u32,
        configured_overflow_margin_tokens: usize,
    ) -> Self {
        let working = percentage_of(advertised_context_window, 90);
        let minimum_hard = working.saturating_add(1).min(advertised_context_window);
        let codex_effective_full = percentage_of(advertised_context_window, 95).max(minimum_hard);
        let configured_margin = configured_overflow_margin_tokens.min(u32::MAX as usize) as u32;
        let margin_limited = advertised_context_window.saturating_sub(configured_margin);
        let effective_full = if margin_limited > working {
            codex_effective_full.min(margin_limited)
        } else {
            // A fixed margin must never make the hard boundary precede
            // automatic compaction. Ignore that invalid cap and retain the
            // Codex effective window.
            codex_effective_full
        };
        Self {
            working,
            effective_full,
        }
    }
}

fn percentage_of(value: u32, percent: u32) -> u32 {
    (u64::from(value) * u64::from(percent) / 100).min(u64::from(u32::MAX)) as u32
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
    fn working_context_is_ninety_percent_independent_of_max_output() {
        let mut config = ResolvedConfig::default();
        config.model.context_window = 131_072;
        config.model.max_output_tokens = 65_536;
        let large_output = ModelPolicy::from_config(&config);
        config.model.max_output_tokens = 8_192;
        let small_output = ModelPolicy::from_config(&config);

        assert_eq!(large_output.working_context_token_limit, 117_964);
        assert_eq!(large_output.effective_context_token_limit, 124_518);
        assert_eq!(
            large_output.working_context_token_limit,
            small_output.working_context_token_limit
        );
        assert_eq!(
            large_output.effective_context_token_limit,
            small_output.effective_context_token_limit
        );
    }

    #[test]
    fn small_context_ignores_a_margin_that_would_precede_compaction() {
        let mut config = ResolvedConfig::default();
        config.model.context_window = 8_192;
        config.session.overflow_margin_tokens = 1_024;

        let policy = ModelPolicy::from_config(&config);

        assert_eq!(policy.working_context_token_limit, 7_372);
        assert_eq!(policy.effective_context_token_limit, 7_782);
        assert!(
            policy.working_context_token_limit < policy.effective_context_token_limit,
            "hard context limit must remain after automatic compaction"
        );
        assert_eq!(
            policy
                .transport_profile(config.model.provider_metadata_mode)
                .context_window,
            policy.effective_context_token_limit
        );
    }

    #[test]
    fn a_safe_configured_margin_can_lower_the_effective_full_limit() {
        let limits = ContextWindowLimits::resolve(16_384, 1_024);

        assert_eq!(limits.working, 14_745);
        assert_eq!(limits.effective_full, 15_360);
        assert!(limits.working < limits.effective_full);
    }

    #[test]
    fn percentage_limits_do_not_overflow_large_profiles() {
        let limits = ContextWindowLimits::resolve(u32::MAX, 0);

        assert_eq!(limits.working, 3_865_470_565);
        assert_eq!(limits.effective_full, 4_080_218_930);
        assert!(limits.working < limits.effective_full);
    }
}

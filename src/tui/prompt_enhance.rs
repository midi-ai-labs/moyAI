use tokio_util::sync::CancellationToken;

use crate::config::ResolvedConfig;
use crate::error::LlmError;
use crate::llm::{
    ChatRequest, ConfigModelCatalog, LlmClient, LlmEvent, LlmEventSink, ModelCatalog, ModelMessage,
    OpenAiCompatClient, apply_model_availability_report_to_config, check_model_availability,
};

const PROMPT_ENHANCER_SYSTEM_PROMPT: &str = "You rewrite a user's coding request into a clearer prompt for the same coding agent.\n\
Preserve the user's exact goal, constraints, file paths, environment details, and acceptance criteria.\n\
Do not add new requirements, features, tasks, tools, or assumptions.\n\
Keep the same language as the user's input unless the user explicitly asked to change language.\n\
Return only the rewritten prompt text with no preface, bullets, explanation, or markdown fences.";

pub async fn enhance_prompt(config: &ResolvedConfig, raw_prompt: &str) -> Result<String, LlmError> {
    let effective_config = prepare_prompt_enhance_config(config).await?;
    let api_key = effective_config
        .model
        .api_key_env
        .as_ref()
        .and_then(|value| std::env::var(value).ok());
    let client = OpenAiCompatClient::new(
        effective_config.model.connect_timeout_ms,
        effective_config.model.request_timeout_ms,
        effective_config.model.max_retries,
        api_key,
    )?;
    let model = ConfigModelCatalog::new(effective_config.clone()).resolve(None)?;
    let mut sink = PromptEnhanceSink::default();
    client
        .stream_chat(
            ChatRequest {
                model,
                base_url: effective_config.model.base_url.clone(),
                system_prompt: PROMPT_ENHANCER_SYSTEM_PROMPT.to_string(),
                messages: vec![ModelMessage::User {
                    content: raw_prompt.to_string(),
                }],
                tools: Vec::new(),
                tool_choice: None,
                parallel_tool_calls: false,
                timeout_ms: effective_config.model.request_timeout_ms,
                stream_idle_timeout_ms: effective_config.model.stream_idle_timeout_ms,
                stream_max_retries: effective_config.model.stream_max_retries,
                extra_headers: effective_config.model.extra_headers.clone(),
                temperature: effective_config.model.temperature,
                top_p: effective_config.model.top_p,
                top_k: effective_config.model.top_k,
                presence_penalty: effective_config.model.presence_penalty,
                frequency_penalty: effective_config.model.frequency_penalty,
                seed: effective_config.model.seed,
                stop_sequences: effective_config.model.stop_sequences.clone(),
                extra_body: effective_config.model.extra_body_json.clone(),
            },
            CancellationToken::new(),
            &mut sink,
        )
        .await?;
    let output = sink.output.trim().to_string();
    if output.is_empty() {
        return Err(LlmError::Message(
            "prompt enhancer returned an empty draft".to_string(),
        ));
    }
    Ok(output)
}

async fn prepare_prompt_enhance_config(
    config: &ResolvedConfig,
) -> Result<ResolvedConfig, LlmError> {
    let report = check_model_availability(config, None, None, false).await;
    hydrate_prompt_enhance_config_from_report(config, &report)
}

fn hydrate_prompt_enhance_config_from_report(
    config: &ResolvedConfig,
    report: &crate::llm::ModelAvailabilityReport,
) -> Result<ResolvedConfig, LlmError> {
    let mut effective_config = config.clone();
    apply_model_availability_report_to_config(&mut effective_config.model, report)?;
    Ok(effective_config)
}

#[derive(Default)]
struct PromptEnhanceSink {
    output: String,
}

impl LlmEventSink for PromptEnhanceSink {
    fn push(&mut self, event: LlmEvent) -> Result<(), LlmError> {
        match event {
            LlmEvent::TextDelta(delta) => {
                self.output.push_str(&delta);
            }
            LlmEvent::ReasoningDelta(_) => {}
            LlmEvent::ToolCallStart { .. }
            | LlmEvent::ToolCallArgsDelta { .. }
            | LlmEvent::Finished { .. } => {}
        }
        Ok(())
    }
}

pub(crate) fn prompt_enhance_sink_excludes_reasoning_delta_fixture_passes() -> bool {
    let mut sink = PromptEnhanceSink::default();
    sink.push(LlmEvent::ReasoningDelta("hidden plan".to_string()))
        .is_ok()
        && sink
            .push(LlmEvent::TextDelta("visible rewrite".to_string()))
            .is_ok()
        && sink.output == "visible rewrite"
}

pub(crate) fn prompt_enhance_model_preparation_uses_availability_report_fixture_passes() -> bool {
    let mut config = ResolvedConfig::default();
    config.model.supports_tools = false;
    let metadata_false_model = crate::llm::ProviderModelInfo {
        id: config.model.model.clone(),
        display_name: None,
        context_window: Some(config.model.context_window),
        max_output_tokens: Some(config.model.max_output_tokens),
        supports_images: None,
        supports_tools: Some(false),
        supports_reasoning: None,
        max_parallel_predictions: Some(config.model.max_parallel_predictions),
        loaded: true,
        source: "openai_compat".to_string(),
    };
    let report = crate::llm::ModelAvailabilityReport {
        gate: "model_availability".to_string(),
        status: crate::llm::ModelAvailabilityStatus::Pass,
        generated_by: "moyai_model_availability_v2".to_string(),
        model: metadata_false_model.id.clone(),
        base_url: config.model.base_url.clone(),
        provider_metadata_mode: config.model.provider_metadata_mode,
        v1_present: true,
        native_present: false,
        require_vision: false,
        vision_capable: false,
        vision_probe_passed: false,
        vision_probes: Vec::new(),
        tool_use_capable: Some(true),
        capability_overrides: vec![crate::llm::model_probe::ModelCapabilityOverride {
            capability: crate::llm::model_probe::ModelCapabilityKind::ToolUse,
            metadata_value: Some(false),
            effective_value: true,
            evidence_ref: "tool_call_probe_passed".to_string(),
        }],
        tool_call_probe_passed: true,
        tool_call_probes: Vec::new(),
        reasoning_capable: None,
        context: metadata_false_model.context_window,
        max_output_tokens: metadata_false_model.max_output_tokens,
        max_parallel_predictions: metadata_false_model.max_parallel_predictions,
        matched_model: Some(crate::llm::ProviderModelInfo {
            supports_tools: Some(true),
            ..metadata_false_model
        }),
        v1_models: vec![config.model.model.clone()],
        native_models: Vec::new(),
        openai_error: None,
        native_error: None,
        checked_at_ms: 0,
    };

    let Ok(effective_config) = hydrate_prompt_enhance_config_from_report(&config, &report) else {
        return false;
    };
    let Ok(model) = ConfigModelCatalog::new(effective_config).resolve(None) else {
        return false;
    };
    model.capabilities.supports_tools
}

#[cfg(test)]
mod tests {
    #[test]
    fn prompt_enhance_sink_excludes_reasoning_delta() {
        assert!(super::prompt_enhance_sink_excludes_reasoning_delta_fixture_passes());
    }
}

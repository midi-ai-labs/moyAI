use tokio_util::sync::CancellationToken;

use crate::config::{ResolvedConfig, ResolvedTurnConfig};
use crate::error::LlmError;
use crate::llm::model_policy::ProviderCapabilities;
use crate::llm::{
    ChatRequest, ConfigModelCatalog, LlmClient, LlmEvent, LlmEventSink, ModelCatalog, ModelMessage,
    OpenAiCompatClient, check_model_availability, resolve_api_key_from_env,
    validate_model_availability_report,
};

const PROMPT_ENHANCER_SYSTEM_PROMPT: &str = include_str!("../../assets/prompts/prompt-enhancer.md");

pub async fn enhance_prompt(
    config: &ResolvedConfig,
    raw_prompt: &str,
    cancellation: CancellationToken,
) -> Result<String, LlmError> {
    validate_prompt_enhance_readiness(config).await?;
    let turn_config = ResolvedTurnConfig::from_effective(config)
        .map_err(|error| LlmError::Message(error.to_string()))?;
    let runtime_config = turn_config.runtime_config();
    let provider_target = turn_config.provider();
    let api_key = resolve_api_key_from_env(runtime_config.model.api_key_env.as_deref())?;
    let client = OpenAiCompatClient::new(api_key);
    let model = ConfigModelCatalog::new(runtime_config.clone()).resolve(None)?;
    let provider_capabilities = ProviderCapabilities::from_config(runtime_config);
    let mut request = ChatRequest::new(
        provider_target.clone(),
        model,
        PROMPT_ENHANCER_SYSTEM_PROMPT.trim().to_string(),
        vec![ModelMessage::User {
            content: raw_prompt.to_string(),
        }],
        Vec::new(),
        None,
        provider_capabilities.reasoning,
        runtime_config.model.extra_headers.clone(),
    );
    request.temperature = runtime_config.model.temperature;
    request.top_p = runtime_config.model.top_p;
    request.top_k = runtime_config.model.top_k;
    request.presence_penalty = runtime_config.model.presence_penalty;
    request.frequency_penalty = runtime_config.model.frequency_penalty;
    request.seed = runtime_config.model.seed;
    request.stop_sequences = runtime_config.model.stop_sequences.clone();
    request.extra_body = runtime_config.model.extra_body_json.clone();
    let mut sink = PromptEnhanceSink::default();
    let summary = client.stream_chat(request, cancellation, &mut sink).await?;
    crate::llm::validate_toolless_text_response("prompt enhancer", &summary, sink.saw_tool_call)?;
    let output = sink.output.trim().to_string();
    if output.is_empty() {
        return Err(LlmError::Message(
            "prompt enhancer returned an empty draft".to_string(),
        ));
    }
    Ok(output)
}

async fn validate_prompt_enhance_readiness(config: &ResolvedConfig) -> Result<(), LlmError> {
    let report = check_model_availability(config, None, None, false).await;
    validate_model_availability_report(&config.model, &report, false)
}

#[derive(Default)]
struct PromptEnhanceSink {
    output: String,
    saw_tool_call: bool,
}

impl LlmEventSink for PromptEnhanceSink {
    fn push(&mut self, event: LlmEvent) -> Result<(), LlmError> {
        match event {
            LlmEvent::TextDelta(delta) => {
                self.output.push_str(&delta);
            }
            LlmEvent::ReasoningSummaryDelta(_) => {}
            LlmEvent::ToolCallStart { .. } | LlmEvent::ToolCallArgsDelta { .. } => {
                self.saw_tool_call = true;
            }
            LlmEvent::Finished { .. } => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {}

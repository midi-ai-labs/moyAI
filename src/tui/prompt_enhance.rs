use tokio_util::sync::CancellationToken;

use crate::config::ResolvedConfig;
use crate::config::model::{ProviderApiMode, ProviderReasoningCapability};
use crate::error::LlmError;
use crate::llm::{
    ChatRequest, ConfigModelCatalog, LlmClient, LlmEvent, LlmEventSink, ModelCatalog, ModelMessage,
    OpenAiCompatClient, check_model_availability, validate_model_availability_report,
};

const PROMPT_ENHANCER_SYSTEM_PROMPT: &str = "You rewrite a user's coding request into a clearer prompt for the same coding agent.\n\
Preserve the user's exact goal, constraints, file paths, environment details, and acceptance criteria.\n\
Do not add new requirements, features, tasks, tools, or assumptions.\n\
Keep the same language as the user's input unless the user explicitly asked to change language.\n\
Return only the rewritten prompt text with no preface, bullets, explanation, or markdown fences.";

pub async fn enhance_prompt(config: &ResolvedConfig, raw_prompt: &str) -> Result<String, LlmError> {
    validate_prompt_enhance_readiness(config).await?;
    let api_key = config
        .model
        .api_key_env
        .as_ref()
        .and_then(|value| std::env::var(value).ok());
    let client = OpenAiCompatClient::new(
        config.model.connect_timeout_ms,
        config.model.max_retries,
        api_key,
    )?;
    let model = ConfigModelCatalog::new(config.clone()).resolve(None)?;
    let mut sink = PromptEnhanceSink::default();
    let summary = client
        .stream_chat(
            ChatRequest {
                model,
                base_url: config.model.base_url.clone(),
                system_prompt: PROMPT_ENHANCER_SYSTEM_PROMPT.to_string(),
                messages: vec![ModelMessage::User {
                    content: raw_prompt.to_string(),
                }],
                tools: Vec::new(),
                provider_api_mode: ProviderApiMode::ChatCompletions,
                reasoning: None,
                reasoning_capability: ProviderReasoningCapability::Unsupported,
                responses_continuation: None,
                tool_choice: None,
                parallel_tool_calls: false,
                timeout_ms: config.model.request_timeout_ms,
                stream_idle_timeout_ms: config.model.stream_idle_timeout_ms,
                extra_headers: config.model.extra_headers.clone(),
                temperature: config.model.temperature,
                top_p: config.model.top_p,
                top_k: config.model.top_k,
                presence_penalty: config.model.presence_penalty,
                frequency_penalty: config.model.frequency_penalty,
                seed: config.model.seed,
                stop_sequences: config.model.stop_sequences.clone(),
                extra_body: config.model.extra_body_json.clone(),
            },
            CancellationToken::new(),
            &mut sink,
        )
        .await?;
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

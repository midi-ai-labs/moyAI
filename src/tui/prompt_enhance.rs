use tokio_util::sync::CancellationToken;

use crate::config::ResolvedConfig;
use crate::error::LlmError;
use crate::llm::{
    ChatRequest, ConfigModelCatalog, LlmClient, LlmEvent, LlmEventSink, ModelCatalog, ModelMessage,
    OpenAiCompatClient, ensure_openai_model_available,
};

const PROMPT_ENHANCER_SYSTEM_PROMPT: &str = "You rewrite a user's coding request into a clearer prompt for the same coding agent.\n\
Preserve the user's exact goal, constraints, file paths, environment details, and acceptance criteria.\n\
Do not add new requirements, features, tasks, tools, or assumptions.\n\
Keep the same language as the user's input unless the user explicitly asked to change language.\n\
Return only the rewritten prompt text with no preface, bullets, explanation, or markdown fences.";

pub async fn enhance_prompt(config: &ResolvedConfig, raw_prompt: &str) -> Result<String, LlmError> {
    ensure_openai_model_available(config).await?;
    let api_key = config
        .model
        .api_key_env
        .as_ref()
        .and_then(|value| std::env::var(value).ok());
    let client = OpenAiCompatClient::new(
        config.model.connect_timeout_ms,
        config.model.request_timeout_ms,
        config.model.max_retries,
        api_key,
    )?;
    let model = ConfigModelCatalog::new(config.clone()).resolve(None)?;
    let mut sink = PromptEnhanceSink::default();
    client
        .stream_chat(
            ChatRequest {
                model,
                base_url: config.model.base_url.clone(),
                system_prompt: PROMPT_ENHANCER_SYSTEM_PROMPT.to_string(),
                messages: vec![ModelMessage::User {
                    content: raw_prompt.to_string(),
                }],
                tools: Vec::new(),
                timeout_ms: config.model.request_timeout_ms,
                stream_idle_timeout_ms: config.model.stream_idle_timeout_ms,
                stream_max_retries: config.model.stream_max_retries,
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
    let output = sink.output.trim().to_string();
    if output.is_empty() {
        return Err(LlmError::Message(
            "prompt enhancer returned an empty draft".to_string(),
        ));
    }
    Ok(output)
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

#[cfg(test)]
mod tests {
    #[test]
    fn prompt_enhance_sink_excludes_reasoning_delta() {
        assert!(super::prompt_enhance_sink_excludes_reasoning_delta_fixture_passes());
    }
}

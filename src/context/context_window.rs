use serde::{Deserialize, Serialize};

use crate::llm::{ChatRequest, ModelContentPart, ModelMessage};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextWindowTokenStatus {
    pub active_context_tokens: u32,
    pub full_context_window_limit: u32,
    pub configured_max_output_tokens: u32,
    pub overflow_margin_tokens: u32,
    pub tokens_until_limit: i64,
    pub token_limit_reached: bool,
}

impl ContextWindowTokenStatus {
    pub fn for_request(request: &ChatRequest, overflow_margin_tokens: usize) -> Self {
        let active_context_tokens = estimate_request_tokens(request);
        let full_context_window_limit = request.model.context_window;
        let configured_max_output_tokens = request.model.max_output_tokens;
        let overflow_margin_tokens = overflow_margin_tokens.min(u32::MAX as usize) as u32;
        let reserved = configured_max_output_tokens.saturating_add(overflow_margin_tokens);
        let tokens_until_limit = i64::from(full_context_window_limit)
            - i64::from(active_context_tokens)
            - i64::from(reserved);
        Self {
            active_context_tokens,
            full_context_window_limit,
            configured_max_output_tokens,
            overflow_margin_tokens,
            tokens_until_limit,
            token_limit_reached: tokens_until_limit <= 0,
        }
    }
}

fn estimate_request_tokens(request: &ChatRequest) -> u32 {
    let chars = request.system_prompt.chars().count()
        + request.messages.iter().map(message_chars).sum::<usize>()
        + request
            .tools
            .iter()
            .map(|tool| {
                tool.name.chars().count()
                    + tool.description.chars().count()
                    + tool.input_schema.to_string().chars().count()
            })
            .sum::<usize>();
    estimate_tokens_from_chars(chars)
}

fn message_chars(message: &ModelMessage) -> usize {
    match message {
        ModelMessage::System { content }
        | ModelMessage::User { content }
        | ModelMessage::Assistant { content } => content.chars().count(),
        ModelMessage::UserParts { parts } => parts
            .iter()
            .map(|part| match part {
                ModelContentPart::Text { text } => text.chars().count(),
                ModelContentPart::Image { data_base64, .. } => data_base64.len() / 8,
            })
            .sum(),
        ModelMessage::AssistantToolCalls {
            content,
            tool_calls,
        } => {
            content.as_deref().map(str::len).unwrap_or_default()
                + tool_calls
                    .iter()
                    .map(|call| call.tool_name.len() + call.arguments_json.len())
                    .sum::<usize>()
        }
        ModelMessage::Tool { result, .. } => result.chars().count(),
    }
}

fn estimate_tokens_from_chars(chars: usize) -> u32 {
    chars.div_ceil(4).min(u32::MAX as usize) as u32
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::config::ProviderMetadataMode;
    use crate::config::model::{ProviderApiMode, ProviderReasoningCapability};
    use crate::llm::{ChatRequest, ModelCapabilities, ModelMessage, ModelProfile};

    #[test]
    fn context_window_status_marks_exhausted_window() {
        let request = ChatRequest {
            model: ModelProfile {
                name: "test".to_string(),
                context_window: 32,
                max_output_tokens: 16,
                provider_metadata_mode: ProviderMetadataMode::LmStudioNativeRequired,
                capabilities: ModelCapabilities {
                    supports_tools: true,
                    supports_reasoning: false,
                    supports_images: false,
                },
            },
            base_url: "http://localhost".to_string(),
            system_prompt: "x".repeat(256),
            messages: vec![ModelMessage::User {
                content: "hello".to_string(),
            }],
            tools: Vec::new(),
            provider_api_mode: ProviderApiMode::ChatCompletions,
            reasoning: None,
            reasoning_capability: ProviderReasoningCapability::Unsupported,
            responses_continuation: None,
            tool_choice: None,
            parallel_tool_calls: false,
            timeout_ms: 1,
            stream_idle_timeout_ms: 1,
            stream_max_retries: 0,
            extra_headers: BTreeMap::new(),
            temperature: None,
            top_p: None,
            top_k: None,
            presence_penalty: None,
            frequency_penalty: None,
            seed: None,
            stop_sequences: Vec::new(),
            extra_body: None,
        };

        let status = super::ContextWindowTokenStatus::for_request(&request, 4);

        assert!(status.active_context_tokens > 32);
        assert!(status.token_limit_reached);
    }
}

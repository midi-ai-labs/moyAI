use serde::{Deserialize, Serialize};

use crate::llm::{ChatRequest, ModelContentPart, ModelMessage};

const REQUEST_OVERHEAD_TOKENS: usize = 12;
const MESSAGE_OVERHEAD_TOKENS: usize = 8;
const TOOL_SCHEMA_OVERHEAD_TOKENS: usize = 16;
const MIN_IMAGE_RESERVATION_TOKENS: usize = 1_024;
const DECODED_IMAGE_BYTES_PER_RESERVED_TOKEN: usize = 128;

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
    let estimated = REQUEST_OVERHEAD_TOKENS
        .saturating_add(estimate_text_tokens(&request.system_prompt))
        .saturating_add(
            request
                .messages
                .iter()
                .map(message_tokens)
                .fold(0usize, usize::saturating_add),
        )
        .saturating_add(
            request
                .tools
                .iter()
                .map(|tool| {
                    TOOL_SCHEMA_OVERHEAD_TOKENS
                        .saturating_add(estimate_text_tokens(&tool.name))
                        .saturating_add(estimate_text_tokens(&tool.description))
                        .saturating_add(estimate_text_tokens(&tool.input_schema.to_string()))
                })
                .fold(0usize, usize::saturating_add),
        );
    estimated.min(u32::MAX as usize) as u32
}

fn message_tokens(message: &ModelMessage) -> usize {
    let content_tokens = match message {
        ModelMessage::System { content }
        | ModelMessage::User { content }
        | ModelMessage::Assistant { content } => estimate_text_tokens(content),
        ModelMessage::UserParts { parts } => parts
            .iter()
            .map(|part| match part {
                ModelContentPart::Text { text } => estimate_text_tokens(text),
                ModelContentPart::Image { data_base64, .. } => image_reservation(data_base64),
            })
            .fold(0usize, usize::saturating_add),
        ModelMessage::AssistantToolCalls {
            content,
            tool_calls,
        } => content
            .as_deref()
            .map(estimate_text_tokens)
            .unwrap_or_default()
            .saturating_add(
                tool_calls
                    .iter()
                    .map(|call| {
                        MESSAGE_OVERHEAD_TOKENS
                            .saturating_add(estimate_text_tokens(&call.call_id))
                            .saturating_add(estimate_text_tokens(&call.tool_name))
                            .saturating_add(estimate_text_tokens(&call.arguments_json))
                    })
                    .fold(0usize, usize::saturating_add),
            ),
        ModelMessage::Tool {
            call_id,
            tool_name,
            result,
            ..
        } => estimate_text_tokens(call_id)
            .saturating_add(estimate_text_tokens(tool_name))
            .saturating_add(estimate_text_tokens(result)),
    };
    MESSAGE_OVERHEAD_TOKENS.saturating_add(content_tokens)
}

/// Transport-neutral conservative estimate used before the provider can report usage.
/// ASCII is reserved at two bytes per token, while every non-ASCII scalar reserves
/// half of its UTF-8 width rounded up. This deliberately over-reserves ordinary prose
/// and avoids treating four CJK scalars as one token.
fn estimate_text_tokens(text: &str) -> usize {
    let mut ascii_bytes = 0usize;
    let mut non_ascii_tokens = 0usize;
    for character in text.chars() {
        if character.is_ascii() {
            ascii_bytes = ascii_bytes.saturating_add(1);
        } else {
            non_ascii_tokens = non_ascii_tokens.saturating_add(character.len_utf8().div_ceil(2));
        }
    }
    ascii_bytes.div_ceil(2).saturating_add(non_ascii_tokens)
}

/// Images do not enter the model as base64 text. Until a provider exposes an exact
/// image-token policy, reserve from decoded payload size with a fixed floor. This is
/// intentionally a safety reservation, not a claim about provider billing tokens.
fn image_reservation(data_base64: &str) -> usize {
    let padding = data_base64
        .as_bytes()
        .iter()
        .rev()
        .take_while(|byte| **byte == b'=')
        .count()
        .min(2);
    let decoded_bytes = data_base64
        .len()
        .div_ceil(4)
        .saturating_mul(3)
        .saturating_sub(padding);
    decoded_bytes
        .div_ceil(DECODED_IMAGE_BYTES_PER_RESERVED_TOKEN)
        .max(MIN_IMAGE_RESERVATION_TOKENS)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::config::model::{ProviderApiMode, ProviderReasoningCapability};
    use crate::config::{ProviderDeadlines, ProviderMetadataMode, ProviderTarget};
    use crate::llm::{
        ChatRequest, ModelCapabilities, ModelContentPart, ModelMessage, ModelProfile,
        ModelToolCall, ToolSchema,
    };

    #[test]
    fn context_window_status_marks_exhausted_window() {
        let model = ModelProfile {
            name: "test".to_string(),
            context_window: 32,
            max_output_tokens: 16,
            provider_metadata_mode: ProviderMetadataMode::LmStudioNativeRequired,
            capabilities: ModelCapabilities {
                supports_tools: true,
                supports_reasoning: false,
                supports_images: false,
            },
        };
        let provider = ProviderTarget::new(
            "http://localhost",
            &model.name,
            model.provider_metadata_mode,
            ProviderApiMode::ChatCompletions,
            ProviderDeadlines {
                response_start_timeout_ms: 1,
                stream_idle_timeout_ms: 1,
                connect_timeout_ms: 1,
                max_connect_retries: 0,
            },
        )
        .expect("provider target");
        let request = ChatRequest::new(
            provider,
            model,
            "x".repeat(256),
            vec![ModelMessage::User {
                content: "hello".to_string(),
            }],
            Vec::new(),
            None,
            ProviderReasoningCapability::Unsupported,
            BTreeMap::new(),
        );

        let status = super::ContextWindowTokenStatus::for_request(&request, 4);

        assert!(status.active_context_tokens > 32);
        assert!(status.token_limit_reached);
    }

    #[test]
    fn prepared_request_reserves_cjk_and_non_ascii_tool_payloads_conservatively() {
        let mut request = request_with(
            ProviderApiMode::ChatCompletions,
            vec![
                ModelMessage::User {
                    content: "資料を精読して計画を更新してください".repeat(16),
                },
                ModelMessage::AssistantToolCalls {
                    content: None,
                    tool_calls: vec![ModelToolCall {
                        call_id: "呼出し一".to_string(),
                        tool_name: "更新計画".to_string(),
                        arguments_json: serde_json::json!({
                            "説明": "長期タスクの状態を保持する",
                            "手順": ["調査", "実装", "検証"]
                        })
                        .to_string(),
                    }],
                },
            ],
            vec![ToolSchema {
                name: "更新計画".to_string(),
                description: "実行計画を永続化する".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {"説明": {"type": "string"}}
                }),
            }],
        );
        let estimated = super::estimate_request_tokens(&request);
        assert!(
            estimated > 500,
            "CJK request was under-reserved: {estimated}"
        );

        let margin = 37u32;
        request.model.context_window = estimated
            .saturating_add(request.model.max_output_tokens)
            .saturating_add(margin);
        let at_boundary = super::ContextWindowTokenStatus::for_request(&request, margin as usize);
        assert_eq!(at_boundary.tokens_until_limit, 0);
        assert!(at_boundary.token_limit_reached);

        request.model.context_window = request.model.context_window.saturating_add(1);
        let below_boundary =
            super::ContextWindowTokenStatus::for_request(&request, margin as usize);
        assert_eq!(below_boundary.tokens_until_limit, 1);
        assert!(!below_boundary.token_limit_reached);
    }

    #[test]
    fn mixed_text_and_image_reservation_is_transport_independent_and_bounded() {
        let decoded_size = 256 * 1024usize;
        let encoded = "A".repeat(decoded_size.div_ceil(3) * 4);
        let messages = vec![ModelMessage::UserParts {
            parts: vec![
                ModelContentPart::Text {
                    text: "画像を確認してください".to_string(),
                },
                ModelContentPart::Image {
                    mime_type: "image/png".to_string(),
                    data_base64: encoded,
                },
            ],
        }];
        let chat = request_with(
            ProviderApiMode::ChatCompletions,
            messages.clone(),
            Vec::new(),
        );
        let responses = request_with(ProviderApiMode::Responses, messages, Vec::new());

        let chat_estimate = super::estimate_request_tokens(&chat);
        let responses_estimate = super::estimate_request_tokens(&responses);
        assert_eq!(chat_estimate, responses_estimate);
        assert!(
            chat_estimate >= 2_000,
            "decoded image reservation was unexpectedly small: {chat_estimate}"
        );
    }

    fn request_with(
        api_mode: ProviderApiMode,
        messages: Vec<ModelMessage>,
        tools: Vec<ToolSchema>,
    ) -> ChatRequest {
        let model = ModelProfile {
            name: "test".to_string(),
            context_window: 32_768,
            max_output_tokens: 256,
            provider_metadata_mode: ProviderMetadataMode::OpenAiCompatibleOnly,
            capabilities: ModelCapabilities {
                supports_tools: true,
                supports_reasoning: false,
                supports_images: true,
            },
        };
        let provider = ProviderTarget::new(
            "http://localhost",
            &model.name,
            model.provider_metadata_mode,
            api_mode,
            ProviderDeadlines {
                response_start_timeout_ms: 1,
                stream_idle_timeout_ms: 1,
                connect_timeout_ms: 1,
                max_connect_retries: 0,
            },
        )
        .expect("provider target");
        ChatRequest::new(
            provider,
            model,
            "runtime prompt".to_string(),
            messages,
            tools,
            None,
            ProviderReasoningCapability::Unsupported,
            BTreeMap::new(),
        )
    }
}

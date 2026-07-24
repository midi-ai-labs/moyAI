use serde::{Deserialize, Serialize};

use crate::llm::{ChatRequest, ModelContentPart, ModelMessage};

const REQUEST_OVERHEAD_TOKENS: usize = 12;
const MESSAGE_OVERHEAD_TOKENS: usize = 8;
const TOOL_SCHEMA_OVERHEAD_TOKENS: usize = 16;
const MIN_IMAGE_RESERVATION_TOKENS: usize = 1_024;
const DECODED_IMAGE_BYTES_PER_RESERVED_TOKEN: usize = 128;
pub(crate) const COMPACTION_USER_MESSAGE_MAX_TOKENS: usize = 20_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActiveContextTokenSource {
    #[default]
    FullPreparedRequestEstimate,
    ProviderUsageWithLocalEstimate,
}

impl ActiveContextTokenSource {
    pub const fn key(self) -> &'static str {
        match self {
            Self::FullPreparedRequestEstimate => "full_prepared_request_estimate",
            Self::ProviderUsageWithLocalEstimate => "provider_usage_with_local_estimate",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextWindowTokenStatus {
    #[serde(default)]
    pub source: ActiveContextTokenSource,
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
        Self::from_active_context_tokens(
            request,
            overflow_margin_tokens,
            active_context_tokens,
            ActiveContextTokenSource::FullPreparedRequestEstimate,
        )
    }

    pub(crate) fn from_provider_usage(
        request: &ChatRequest,
        overflow_margin_tokens: usize,
        provider_total_tokens: u32,
        local_messages: &[ModelMessage],
    ) -> Self {
        let local_tokens = estimate_model_messages_tokens(local_messages);
        Self::from_active_context_tokens(
            request,
            overflow_margin_tokens,
            provider_total_tokens.saturating_add(local_tokens),
            ActiveContextTokenSource::ProviderUsageWithLocalEstimate,
        )
    }

    fn from_active_context_tokens(
        request: &ChatRequest,
        overflow_margin_tokens: usize,
        active_context_tokens: u32,
        source: ActiveContextTokenSource,
    ) -> Self {
        let full_context_window_limit = request.model.context_window;
        let configured_max_output_tokens = request.model.max_output_tokens;
        let overflow_margin_tokens = overflow_margin_tokens.min(u32::MAX as usize) as u32;
        let reserved = configured_max_output_tokens.saturating_add(overflow_margin_tokens);
        let tokens_until_limit = i64::from(full_context_window_limit)
            - i64::from(active_context_tokens)
            - i64::from(reserved);
        Self {
            source,
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
        | ModelMessage::Developer { content }
        | ModelMessage::Agent { content }
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

/// Codex-compatible coarse estimate used before the provider can report usage.
/// This is a byte-based lower bound rather than a tokenizer-accurate count.
pub(crate) fn estimate_text_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

pub(crate) fn estimate_model_messages_tokens(messages: &[ModelMessage]) -> u32 {
    messages
        .iter()
        .map(message_tokens)
        .fold(0usize, usize::saturating_add)
        .min(u32::MAX as usize) as u32
}

/// Keeps both ends of a durable user checkpoint while respecting the same
/// Codex-compatible estimator used for prepared requests.
pub(crate) fn truncate_text_to_estimated_tokens(text: &str, max_tokens: usize) -> String {
    if max_tokens == 0 {
        return String::new();
    }
    if estimate_text_tokens(text) <= max_tokens {
        return text.to_string();
    }

    const MARKER: &str = "\n[... compaction checkpoint truncated ...]\n";
    let marker_tokens = estimate_text_tokens(MARKER);
    if marker_tokens >= max_tokens {
        return prefix_within_estimated_tokens(text, max_tokens).to_string();
    }

    let content_tokens = max_tokens - marker_tokens;
    let head_budget = content_tokens.div_ceil(2);
    let tail_budget = content_tokens / 2;
    let head = prefix_within_estimated_tokens(text, head_budget);
    let tail_source = &text[head.len()..];
    let tail = suffix_within_estimated_tokens(tail_source, tail_budget);
    let truncated = format!("{head}{MARKER}{tail}");
    debug_assert!(estimate_text_tokens(&truncated) <= max_tokens);
    truncated
}

pub(crate) struct CompactionUserMessageBudget {
    remaining_tokens: usize,
    newest_first: Vec<String>,
}

impl CompactionUserMessageBudget {
    pub(crate) fn new() -> Self {
        Self {
            remaining_tokens: COMPACTION_USER_MESSAGE_MAX_TOKENS,
            newest_first: Vec::new(),
        }
    }

    /// Adds one candidate while walking messages from newest to oldest.
    /// Returns whether an older candidate can still contribute.
    pub(crate) fn push_newest(&mut self, message: String) -> bool {
        let message_tokens = estimate_text_tokens(&message);
        if message_tokens <= self.remaining_tokens {
            self.newest_first.push(message);
            self.remaining_tokens = self.remaining_tokens.saturating_sub(message_tokens);
            return self.remaining_tokens > 0;
        }
        let boundary = truncate_text_to_estimated_tokens(&message, self.remaining_tokens);
        if !boundary.is_empty() {
            self.newest_first.push(boundary);
        }
        self.remaining_tokens = 0;
        false
    }

    pub(crate) fn finish(mut self) -> Vec<String> {
        self.newest_first.reverse();
        self.newest_first
    }
}

pub(crate) fn bounded_compaction_user_messages(messages: &[String]) -> Vec<String> {
    let mut budget = CompactionUserMessageBudget::new();
    for message in messages.iter().rev() {
        if !budget.push_newest(message.clone()) {
            break;
        }
    }
    budget.finish()
}

fn prefix_within_estimated_tokens(text: &str, max_tokens: usize) -> &str {
    let boundaries = text
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(text.len()))
        .collect::<Vec<_>>();
    let mut low = 0usize;
    let mut high = boundaries.len();
    while low + 1 < high {
        let middle = low + (high - low) / 2;
        if estimate_text_tokens(&text[..boundaries[middle]]) <= max_tokens {
            low = middle;
        } else {
            high = middle;
        }
    }
    &text[..boundaries[low]]
}

fn suffix_within_estimated_tokens(text: &str, max_tokens: usize) -> &str {
    let boundaries = text
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(text.len()))
        .collect::<Vec<_>>();
    let mut low = 0usize;
    let mut high = boundaries.len();
    while low + 1 < high {
        let middle = low + (high - low) / 2;
        let start = boundaries[boundaries.len() - 1 - middle];
        if estimate_text_tokens(&text[start..]) <= max_tokens {
            low = middle;
        } else {
            high = middle;
        }
    }
    &text[boundaries[boundaries.len() - 1 - low]..]
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

    #[test]
    fn text_estimator_matches_codex_utf8_byte_approximation() {
        assert_eq!(super::estimate_text_tokens(""), 0);
        assert_eq!(super::estimate_text_tokens("abcd"), 1);
        assert_eq!(super::estimate_text_tokens("abcde"), 2);
        assert_eq!(super::estimate_text_tokens("資料"), 2);
    }

    #[test]
    fn text_truncation_keeps_both_ends_within_estimated_budget() {
        let source = format!("HEAD-{}-TAIL", "中間content".repeat(1_000));

        let truncated = super::truncate_text_to_estimated_tokens(&source, 80);

        assert!(truncated.starts_with("HEAD-"));
        assert!(truncated.ends_with("-TAIL"));
        assert!(truncated.contains("compaction checkpoint truncated"));
        assert!(super::estimate_text_tokens(&truncated) <= 80);
    }

    #[test]
    fn text_truncation_handles_zero_and_tiny_budgets() {
        assert_eq!(super::truncate_text_to_estimated_tokens("abcdef", 0), "");
        let tiny = super::truncate_text_to_estimated_tokens("abcdef", 1);
        assert!(!tiny.is_empty());
        assert!(super::estimate_text_tokens(&tiny) <= 1);
    }

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
    fn context_window_status_defaults_legacy_diagnostics_to_full_request_estimate() {
        let status: super::ContextWindowTokenStatus = serde_json::from_value(serde_json::json!({
            "active_context_tokens": 100,
            "full_context_window_limit": 1_000,
            "configured_max_output_tokens": 100,
            "overflow_margin_tokens": 10,
            "tokens_until_limit": 790,
            "token_limit_reached": false
        }))
        .expect("legacy context status");

        assert_eq!(
            status.source,
            super::ActiveContextTokenSource::FullPreparedRequestEstimate
        );
    }

    #[test]
    fn prepared_request_estimate_counts_non_ascii_messages_and_tool_payloads() {
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
        let empty_request = request_with(ProviderApiMode::ChatCompletions, Vec::new(), Vec::new());
        assert!(estimated > super::estimate_request_tokens(&empty_request));

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

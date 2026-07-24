use serde_json::Value;

use crate::config::model::ProviderApiMode;
use crate::error::LlmError;
use crate::llm::contract::ChatRequest;
use crate::llm::openai_compat::to_openai_request_with_reasoning;
use crate::llm::responses::{ResponsesRequestOptions, to_responses_request};
use crate::session::RequestWireDiagnostic;

/// Derives redacted diagnostics from the same provider DTO and bounded
/// serialization used by the HTTP transport. The request body itself is never
/// retained in session or harness state.
pub(crate) fn http_request_wire_diagnostic(
    request: &ChatRequest,
) -> Result<RequestWireDiagnostic, LlmError> {
    let (api_mode, input_kind, input_key, body) = match request.provider_target().api_mode() {
        ProviderApiMode::ChatCompletions => (
            "chat_completions",
            "messages",
            "messages",
            to_openai_request_with_reasoning(
                request,
                request.reasoning.as_ref(),
                request.reasoning_capability,
            )?,
        ),
        ProviderApiMode::Responses => (
            "responses",
            "input_items",
            "input",
            to_responses_request(request, ResponsesRequestOptions::from_request(request))?,
        ),
    };
    let input_count = body
        .get(input_key)
        .and_then(Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| {
            LlmError::Message(format!(
                "serialized {api_mode} request is missing its `{input_key}` array"
            ))
        })?;
    let continuation_present = body.get("previous_response_id").is_some();
    let serialized_body_bytes = u64::try_from(request.serialize_wire_body(&body)?.len())
        .map_err(|_| LlmError::Message("serialized request size exceeds u64".to_string()))?;

    Ok(RequestWireDiagnostic {
        transport: "http".to_string(),
        api_mode: api_mode.to_string(),
        input_kind: input_kind.to_string(),
        input_count,
        serialized_body_bytes,
        continuation_present,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::Value;

    use super::*;
    use crate::config::model::{ProviderApiMode, ProviderReasoningCapability};
    use crate::config::{ProviderDeadlines, ProviderMetadataMode, ProviderTarget};
    use crate::llm::{ModelCapabilities, ModelMessage, ModelProfile, ModelToolCall};

    #[test]
    fn chat_completions_diagnostics_measure_the_exact_wire_messages_and_body() {
        let request = request(
            ProviderApiMode::ChatCompletions,
            vec![
                ModelMessage::System {
                    content: "additional policy".to_string(),
                },
                ModelMessage::User {
                    content: "wire-payload-secret".to_string(),
                },
                ModelMessage::Assistant {
                    content: "done".to_string(),
                },
            ],
        );

        let diagnostics = http_request_wire_diagnostic(&request).expect("wire diagnostics");
        let body = to_openai_request_with_reasoning(
            &request,
            request.reasoning.as_ref(),
            request.reasoning_capability,
        )
        .expect("chat request body");
        let expected_bytes = request
            .serialize_wire_body(&body)
            .expect("serialized chat body")
            .len() as u64;

        assert_eq!(diagnostics.api_mode, "chat_completions");
        assert_eq!(diagnostics.input_kind, "messages");
        assert_eq!(diagnostics.input_count, 3);
        assert_eq!(diagnostics.serialized_body_bytes, expected_bytes);
        assert!(!diagnostics.continuation_present);
        assert!(
            !serde_json::to_string(&diagnostics)
                .expect("serialized diagnostics")
                .contains("wire-payload-secret")
        );
    }

    #[test]
    fn responses_diagnostics_count_expanded_input_items_without_a_cursor() {
        let request = request(
            ProviderApiMode::Responses,
            vec![
                ModelMessage::User {
                    content: "inspect the source".to_string(),
                },
                ModelMessage::AssistantToolCalls {
                    content: Some("I will inspect it.".to_string()),
                    tool_calls: vec![ModelToolCall {
                        call_id: "call-1".to_string(),
                        tool_name: "read".to_string(),
                        arguments_json: r#"{"path":"src/main.rs"}"#.to_string(),
                    }],
                },
                ModelMessage::Tool {
                    call_id: "call-1".to_string(),
                    tool_name: "read".to_string(),
                    result: "source".to_string(),
                    metadata: Value::Null,
                },
            ],
        );

        let diagnostics = http_request_wire_diagnostic(&request).expect("wire diagnostics");
        let body = to_responses_request(&request, ResponsesRequestOptions::from_request(&request))
            .expect("Responses request body");
        let expected_bytes = request
            .serialize_wire_body(&body)
            .expect("serialized Responses body")
            .len() as u64;

        assert_eq!(diagnostics.api_mode, "responses");
        assert_eq!(diagnostics.input_kind, "input_items");
        assert_eq!(diagnostics.input_count, 4);
        assert_eq!(diagnostics.serialized_body_bytes, expected_bytes);
        assert!(!diagnostics.continuation_present);
    }

    fn request(api_mode: ProviderApiMode, messages: Vec<ModelMessage>) -> ChatRequest {
        let model = ModelProfile {
            name: "wire-diagnostics-model".to_string(),
            context_window: 131_072,
            max_output_tokens: 8_192,
            provider_metadata_mode: ProviderMetadataMode::OpenAiCompatibleOnly,
            capabilities: ModelCapabilities {
                supports_tools: true,
                supports_reasoning: true,
                supports_images: false,
            },
        };
        let provider = ProviderTarget::new(
            "http://provider.fixture.invalid/v1",
            &model.name,
            model.provider_metadata_mode,
            api_mode,
            ProviderDeadlines {
                response_start_timeout_ms: 30_000,
                stream_idle_timeout_ms: 30_000,
                connect_timeout_ms: 1_000,
                max_connect_retries: 0,
            },
        )
        .expect("provider target");
        let reasoning_capability = match api_mode {
            ProviderApiMode::ChatCompletions => ProviderReasoningCapability::Unsupported,
            ProviderApiMode::Responses => ProviderReasoningCapability::Responses {
                supports_summary: true,
            },
        };

        ChatRequest::new(
            provider,
            model,
            "base instructions".to_string(),
            messages,
            Vec::new(),
            None,
            reasoning_capability,
            BTreeMap::new(),
        )
    }
}

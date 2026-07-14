use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::config::ProviderMetadataMode;
use crate::config::model::{ProviderReasoningCapability, ReasoningEffort, ReasoningSummary};
use crate::error::LlmError;
use crate::session::{FinishReason, TokenUsage};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub supports_tools: bool,
    pub supports_reasoning: bool,
    #[serde(default)]
    pub supports_images: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelProfile {
    pub name: String,
    pub context_window: u32,
    pub max_output_tokens: u32,
    #[serde(default)]
    pub provider_metadata_mode: ProviderMetadataMode,
    pub capabilities: ModelCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelToolCall {
    pub call_id: String,
    pub tool_name: String,
    pub arguments_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelContentPart {
    Text {
        text: String,
    },
    Image {
        mime_type: String,
        data_base64: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum ModelMessage {
    System {
        content: String,
    },
    User {
        content: String,
    },
    UserParts {
        parts: Vec<ModelContentPart>,
    },
    Assistant {
        content: String,
    },
    AssistantToolCalls {
        content: Option<String>,
        tool_calls: Vec<ModelToolCall>,
    },
    Tool {
        call_id: String,
        tool_name: String,
        result: String,
        #[serde(default, skip_serializing, skip_deserializing)]
        metadata: serde_json::Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    #[serde(default)]
    pub strict: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderToolChoice {
    Required,
    Named { name: String },
}

impl ProviderToolChoice {
    pub fn diagnostic_label(&self) -> String {
        match self {
            Self::Required => "required".to_string(),
            Self::Named { name } => format!("named:{name}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: ModelProfile,
    pub base_url: String,
    pub system_prompt: String,
    pub messages: Vec<ModelMessage>,
    pub tools: Vec<ToolSchema>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ProviderToolChoice>,
    #[serde(default)]
    pub parallel_tool_calls: bool,
    pub timeout_ms: u64,
    pub stream_idle_timeout_ms: u64,
    pub stream_max_retries: u8,
    pub extra_headers: BTreeMap<String, String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<u32>,
    pub presence_penalty: Option<f64>,
    pub frequency_penalty: Option<f64>,
    pub seed: Option<u64>,
    pub stop_sequences: Vec<String>,
    pub extra_body: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    #[serde(default)]
    pub summary: ReasoningSummary,
}

impl ReasoningRequest {
    pub fn is_disabled(&self) -> bool {
        self.effort.is_none() && self.summary == ReasoningSummary::None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedChatCompletionsReasoningRequest {
    pub effort: Option<ReasoningEffort>,
    pub summary: Option<ReasoningSummary>,
}

pub(crate) fn validate_chat_completions_reasoning_request(
    request: Option<&ReasoningRequest>,
    capability: ProviderReasoningCapability,
) -> Result<Option<ValidatedChatCompletionsReasoningRequest>, LlmError> {
    let Some(request) = request.filter(|request| !request.is_disabled()) else {
        return Ok(None);
    };

    match capability {
        ProviderReasoningCapability::Unsupported => Err(LlmError::Message(
            "reasoning parameters were requested for a provider that does not advertise a typed reasoning request contract"
                .to_string(),
        )),
        ProviderReasoningCapability::ChatCompletions { parameters } => {
            if request.summary != ReasoningSummary::None && !parameters.supports_summary() {
                return Err(LlmError::Message(
                    "reasoning summary was requested for a Chat Completions provider that supports effort only"
                        .to_string(),
                ));
            }
            Ok(Some(ValidatedChatCompletionsReasoningRequest {
                effort: request.effort.clone(),
                summary: (request.summary != ReasoningSummary::None).then_some(request.summary),
            }))
        }
        ProviderReasoningCapability::ResponsesItemsNotImplemented => Err(LlmError::Message(
            "item-based Responses reasoning state continuity is not implemented by the OpenAI-compatible Chat Completions transport"
                .to_string(),
        )),
    }
}

pub const OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY: &str = "Respond in the user's language. Do not emit hidden reasoning or `<think>` blocks; return only user-facing content and normal tool calls.";

const CURRENT_PROVIDER_MODEL: &str = "qwen/qwen3.6-35b-a3b";
const CURRENT_PROVIDER_BASE_URL: &str = "http://127.0.0.1:1234";
const CURRENT_PROVIDER_CONTEXT_WINDOW: u32 = 131_072;
const CURRENT_PROVIDER_MAX_OUTPUT_TOKENS: u32 = 8_192;

impl ChatRequest {
    pub fn provider_system_prompt(&self) -> String {
        system_prompt_with_provider_policy(
            &self.system_prompt,
            self.model.provider_metadata_mode,
            !self.tools.is_empty(),
        )
    }

    pub fn effective_max_output_tokens(&self) -> u32 {
        effective_max_output_tokens_for_request(self).0
    }

    pub fn output_budget_reason(&self) -> &'static str {
        effective_max_output_tokens_for_request(self).1
    }

    pub fn validate_provider_lifecycle(&self) -> Result<(), LlmError> {
        if !self.model.capabilities.supports_tools && !self.tools.is_empty() {
            return Err(LlmError::Message(
                "ChatRequest provider tools require a tool-capable model profile".to_string(),
            ));
        }
        if self.tool_choice.is_some() && self.tools.is_empty() {
            return Err(LlmError::Message(
                "ChatRequest tool_choice requires a non-empty provider tool surface".to_string(),
            ));
        }
        if let Some(ProviderToolChoice::Named { name }) = &self.tool_choice
            && !self.tools.iter().any(|tool| tool.name == *name)
        {
            return Err(LlmError::Message(format!(
                "ChatRequest named tool_choice `{name}` is not present in provider tool surface"
            )));
        }
        if self.parallel_tool_calls && self.tools.is_empty() {
            return Err(LlmError::Message(
                "ChatRequest parallel_tool_calls requires a non-empty provider tool surface"
                    .to_string(),
            ));
        }
        if !self.model.capabilities.supports_images && self.messages.iter().any(message_has_image) {
            return Err(LlmError::Message(
                "ChatRequest image content requires a vision-capable model profile".to_string(),
            ));
        }
        Ok(())
    }
}

fn message_has_image(message: &ModelMessage) -> bool {
    matches!(
        message,
        ModelMessage::UserParts { parts }
            if parts
                .iter()
                .any(|part| matches!(part, ModelContentPart::Image { .. }))
    )
}

pub fn effective_max_output_tokens_for_request(request: &ChatRequest) -> (u32, &'static str) {
    (request.model.max_output_tokens, "configured_model_limit")
}

pub fn effective_parallel_tool_calls(
    tool_surface_len: usize,
    parallel_tool_calls_enabled: bool,
    max_parallel_predictions: u32,
) -> bool {
    tool_surface_len > 0 && parallel_tool_calls_enabled && max_parallel_predictions > 1
}

pub fn control_plane_parallel_tool_calls_projection(
    tool_surface_len: usize,
    parallel_tool_calls_enabled: bool,
    max_parallel_predictions: u32,
) -> bool {
    effective_parallel_tool_calls(
        tool_surface_len,
        parallel_tool_calls_enabled,
        max_parallel_predictions,
    )
}

pub fn tool_surface_scoped_parallel_tool_calls_projection(
    tool_surface_len: usize,
    effective_parallel_tool_calls: bool,
) -> Option<bool> {
    (tool_surface_len > 0).then_some(effective_parallel_tool_calls)
}

pub fn system_prompt_with_provider_policy(
    system_prompt: &str,
    provider_metadata_mode: ProviderMetadataMode,
    tool_calls_available: bool,
) -> String {
    if provider_metadata_mode != ProviderMetadataMode::OpenAiCompatibleOnly {
        return system_prompt.to_string();
    }

    let provider_policy = openai_compatible_only_provider_policy(tool_calls_available);
    let body = strip_openai_compatible_provider_policy(system_prompt);
    if body.trim().is_empty() {
        provider_policy
    } else {
        format!("{provider_policy}\n\n{body}")
    }
}

fn openai_compatible_only_provider_policy(tool_calls_available: bool) -> String {
    let _ = tool_calls_available;
    OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY.to_string()
}

fn strip_openai_compatible_provider_policy(system_prompt: &str) -> &str {
    if let Some(rest) = system_prompt.strip_prefix(OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY) {
        return rest.strip_prefix("\n\n").unwrap_or(rest);
    }
    system_prompt
}

pub fn openai_compatible_only_tool_policy_fixture_passes() -> bool {
    let no_tool_prompt = system_prompt_with_provider_policy(
        "Base coding prompt",
        ProviderMetadataMode::OpenAiCompatibleOnly,
        false,
    );
    let tool_prompt = system_prompt_with_provider_policy(
        "Base coding prompt",
        ProviderMetadataMode::OpenAiCompatibleOnly,
        true,
    );

    no_tool_prompt.starts_with(OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY)
        && no_tool_prompt.ends_with("\n\nBase coding prompt")
        && tool_prompt.starts_with(OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY)
        && tool_prompt == no_tool_prompt
        && tool_prompt.ends_with("\n\nBase coding prompt")
}

pub fn provider_policy_tool_lifecycle_upgrade_fixture_passes() -> bool {
    let no_tool_prompt = system_prompt_with_provider_policy(
        "Base coding prompt",
        ProviderMetadataMode::OpenAiCompatibleOnly,
        false,
    );
    let upgraded_tool_prompt = system_prompt_with_provider_policy(
        &no_tool_prompt,
        ProviderMetadataMode::OpenAiCompatibleOnly,
        true,
    );

    no_tool_prompt.starts_with(OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY)
        && upgraded_tool_prompt.starts_with(OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY)
        && upgraded_tool_prompt == no_tool_prompt
        && upgraded_tool_prompt.ends_with("\n\nBase coding prompt")
}

pub fn tool_call_turn_uses_configured_output_budget_fixture_passes() -> bool {
    let tool_request = output_budget_fixture_chat_request();
    let no_tool_request = ChatRequest {
        tools: Vec::new(),
        tool_choice: None,
        extra_body: None,
        ..tool_request.clone()
    };

    tool_request.model.max_output_tokens == CURRENT_PROVIDER_MAX_OUTPUT_TOKENS
        && tool_request.effective_max_output_tokens() == CURRENT_PROVIDER_MAX_OUTPUT_TOKENS
        && tool_request.output_budget_reason() == "configured_model_limit"
        && no_tool_request.effective_max_output_tokens() == CURRENT_PROVIDER_MAX_OUTPUT_TOKENS
        && no_tool_request.output_budget_reason() == "configured_model_limit"
}

pub fn llm_contract_current_provider_profile_fixture_passes() -> bool {
    let request = output_budget_fixture_chat_request();

    request.model.name == CURRENT_PROVIDER_MODEL
        && request.base_url == CURRENT_PROVIDER_BASE_URL
        && request.model.context_window == CURRENT_PROVIDER_CONTEXT_WINDOW
        && request.model.max_output_tokens == CURRENT_PROVIDER_MAX_OUTPUT_TOKENS
        && request.model.provider_metadata_mode == ProviderMetadataMode::LmStudioNativeRequired
        && request.model.capabilities.supports_tools
        && !request.model.capabilities.supports_reasoning
        && !request.model.capabilities.supports_images
}

fn output_budget_fixture_chat_request() -> ChatRequest {
    let model = ModelProfile {
        name: CURRENT_PROVIDER_MODEL.to_string(),
        context_window: CURRENT_PROVIDER_CONTEXT_WINDOW,
        max_output_tokens: CURRENT_PROVIDER_MAX_OUTPUT_TOKENS,
        provider_metadata_mode: ProviderMetadataMode::LmStudioNativeRequired,
        capabilities: ModelCapabilities {
            supports_tools: true,
            supports_reasoning: false,
            supports_images: false,
        },
    };
    ChatRequest {
        model: model.clone(),
        base_url: CURRENT_PROVIDER_BASE_URL.to_string(),
        system_prompt: "Use tools for open work.".to_string(),
        messages: vec![ModelMessage::User {
            content:
                "Create src/workflow.rs for workflow-output-budget-contract (llm_contract_fixture_language_neutral)"
                    .to_string(),
        }],
        tools: vec![ToolSchema {
            name: "write".to_string(),
            description: "write a file".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "required": ["path", "content"],
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                }
            }),
            strict: false,
        }],
        tool_choice: Some(ProviderToolChoice::Required),
        parallel_tool_calls: true,
        timeout_ms: 600_000,
        stream_idle_timeout_ms: 300_000,
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
    }
}

pub fn chat_request_tool_choice_is_provider_neutral_typed_fixture_passes() -> bool {
    let named = ProviderToolChoice::Named {
        name: "apply_patch".to_string(),
    };
    let required = ProviderToolChoice::Required;
    let Ok(serialized_named) = serde_json::to_value(&named) else {
        return false;
    };
    named.diagnostic_label() == "named:apply_patch"
        && required.diagnostic_label() == "required"
        && serialized_named
            .get("type")
            .and_then(serde_json::Value::as_str)
            == Some("named")
        && serialized_named
            .get("name")
            .and_then(serde_json::Value::as_str)
            == Some("apply_patch")
        && serialized_named.get("function").is_none()
}

pub fn model_tool_replay_metadata_is_not_serialized_fixture_passes() -> bool {
    let message = ModelMessage::Tool {
        call_id: "call_1".to_string(),
        tool_name: "read".to_string(),
        result: "repository excerpt".to_string(),
        metadata: serde_json::json!({
            "tool_feedback_envelope": {
                "kind": "supporting_context",
                "operation_progress_class": "supporting_context"
            },
            "operation_progress_class": "supporting_context"
        }),
    };

    let serialized = serde_json::to_value(&message).unwrap_or(serde_json::Value::Null);
    serialized.get("metadata").is_none()
        && !serialized.to_string().contains("tool_feedback_envelope")
        && !serialized.to_string().contains("supporting_context")
}

pub fn model_tool_replay_metadata_is_not_deserialized_fixture_passes() -> bool {
    let incoming = serde_json::json!({
        "role": "tool",
        "call_id": "call_1",
        "tool_name": "read",
        "result": "repository excerpt",
        "metadata": {
            "tool_feedback_envelope": {
                "kind": "supporting_context",
                "operation_progress_class": "supporting_context"
            },
            "operation_progress_class": "supporting_context"
        }
    });

    let Ok(ModelMessage::Tool { metadata, .. }) = serde_json::from_value::<ModelMessage>(incoming)
    else {
        return false;
    };
    metadata.is_null()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LlmEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCallStart {
        call_id: String,
        tool_name: String,
    },
    ToolCallArgsDelta {
        call_id: String,
        delta: String,
    },
    Finished {
        finish_reason: FinishReason,
        usage: Option<TokenUsage>,
    },
}

pub trait LlmEventSink {
    fn push(&mut self, event: LlmEvent) -> Result<(), LlmError>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponseSummary {
    pub finish_reason: FinishReason,
    pub usage: Option<TokenUsage>,
}

pub fn validate_toolless_text_response(
    operation: impl Into<String>,
    summary: &LlmResponseSummary,
    saw_tool_call: bool,
) -> Result<(), LlmError> {
    let operation = operation.into();
    if saw_tool_call {
        return Err(LlmError::ToollessTextShape { operation });
    }
    if summary.finish_reason != FinishReason::Stop {
        return Err(LlmError::ToollessTextFinish {
            operation,
            finish_reason: summary.finish_reason,
        });
    }
    Ok(())
}

#[async_trait(?Send)]
pub trait LlmClient: Send + Sync {
    async fn stream_chat(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
        sink: &mut dyn LlmEventSink,
    ) -> Result<LlmResponseSummary, LlmError>;
}

#[cfg(test)]
mod tests {
    use super::{
        LlmResponseSummary, OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY, ReasoningRequest,
        openai_compatible_only_tool_policy_fixture_passes, system_prompt_with_provider_policy,
        tool_call_turn_uses_configured_output_budget_fixture_passes,
        validate_chat_completions_reasoning_request, validate_toolless_text_response,
    };
    use crate::config::ProviderMetadataMode;
    use crate::config::model::{
        ChatCompletionsReasoningParameters, ProviderReasoningCapability, ReasoningEffort,
        ReasoningSummary,
    };
    use crate::error::LlmError;
    use crate::session::FinishReason;

    #[test]
    fn openai_compatible_only_policy_is_prefixed_to_system_prompt() {
        let prompt = system_prompt_with_provider_policy(
            "Base coding prompt",
            ProviderMetadataMode::OpenAiCompatibleOnly,
            false,
        );

        assert!(prompt.starts_with(OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY));
        assert!(prompt.ends_with("\n\nBase coding prompt"));
        assert_eq!(
            prompt
                .matches(OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY)
                .count(),
            1
        );
    }

    #[test]
    fn lm_studio_native_mode_keeps_system_prompt_unchanged() {
        assert_eq!(
            system_prompt_with_provider_policy(
                "Base coding prompt",
                ProviderMetadataMode::LmStudioNativeRequired,
                true,
            ),
            "Base coding prompt"
        );
    }

    #[test]
    fn openai_compatible_only_tool_request_keeps_policy_minimal() {
        let prompt = system_prompt_with_provider_policy(
            "Base coding prompt",
            ProviderMetadataMode::OpenAiCompatibleOnly,
            true,
        );

        assert!(prompt.starts_with(OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY));
        assert_eq!(
            prompt
                .matches(OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY)
                .count(),
            1
        );
        assert!(openai_compatible_only_tool_policy_fixture_passes());
    }

    #[test]
    fn openai_compatible_only_policy_upgrade_is_idempotent() {
        assert!(super::provider_policy_tool_lifecycle_upgrade_fixture_passes());
    }

    #[test]
    fn tool_call_request_uses_configured_output_budget() {
        assert!(tool_call_turn_uses_configured_output_budget_fixture_passes());
    }

    #[test]
    fn toolless_text_terminal_contract_accepts_only_stop_without_tool_calls() {
        for finish_reason in [
            FinishReason::Stop,
            FinishReason::ToolCall,
            FinishReason::Length,
            FinishReason::Cancelled,
            FinishReason::Error,
        ] {
            let summary = LlmResponseSummary {
                finish_reason,
                usage: None,
            };
            let result = validate_toolless_text_response("test operation", &summary, false);
            if finish_reason == FinishReason::Stop {
                assert!(result.is_ok());
            } else {
                assert!(matches!(
                    result,
                    Err(LlmError::ToollessTextFinish {
                        finish_reason: actual,
                        ..
                    }) if actual == finish_reason
                ));
            }
        }

        let summary = LlmResponseSummary {
            finish_reason: FinishReason::Stop,
            usage: None,
        };
        assert!(matches!(
            validate_toolless_text_response("test operation", &summary, true),
            Err(LlmError::ToollessTextShape { .. })
        ));
    }

    #[test]
    fn disabled_reasoning_request_is_omitted_for_every_provider_capability() {
        let disabled = ReasoningRequest::default();
        for capability in [
            ProviderReasoningCapability::Unsupported,
            ProviderReasoningCapability::ChatCompletions {
                parameters: ChatCompletionsReasoningParameters::EffortOnly,
            },
            ProviderReasoningCapability::ResponsesItemsNotImplemented,
        ] {
            assert!(
                validate_chat_completions_reasoning_request(Some(&disabled), capability)
                    .expect("disabled reasoning must not require provider support")
                    .is_none()
            );
        }
    }

    #[test]
    fn reasoning_request_requires_an_explicit_compatible_provider_contract() {
        let effort = ReasoningRequest {
            effort: Some(ReasoningEffort::Medium),
            summary: ReasoningSummary::None,
        };
        assert!(
            validate_chat_completions_reasoning_request(
                Some(&effort),
                ProviderReasoningCapability::Unsupported,
            )
            .is_err()
        );
        assert!(
            validate_chat_completions_reasoning_request(
                Some(&effort),
                ProviderReasoningCapability::ResponsesItemsNotImplemented,
            )
            .is_err()
        );

        let validated = validate_chat_completions_reasoning_request(
            Some(&effort),
            ProviderReasoningCapability::ChatCompletions {
                parameters: ChatCompletionsReasoningParameters::EffortOnly,
            },
        )
        .expect("typed Chat Completions reasoning")
        .expect("enabled request");
        assert_eq!(validated.effort, Some(ReasoningEffort::Medium));
        assert_eq!(validated.summary, None);
    }

    #[test]
    fn effort_only_chat_contract_rejects_reasoning_summary() {
        let request = ReasoningRequest {
            effort: Some(ReasoningEffort::High),
            summary: ReasoningSummary::Concise,
        };
        assert!(
            validate_chat_completions_reasoning_request(
                Some(&request),
                ProviderReasoningCapability::ChatCompletions {
                    parameters: ChatCompletionsReasoningParameters::EffortOnly,
                },
            )
            .is_err()
        );
        assert!(
            validate_chat_completions_reasoning_request(
                Some(&request),
                ProviderReasoningCapability::ChatCompletions {
                    parameters: ChatCompletionsReasoningParameters::EffortAndSummary,
                },
            )
            .is_ok()
        );
    }
}

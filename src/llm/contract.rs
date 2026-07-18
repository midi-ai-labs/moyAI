use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::config::model::ProviderApiMode;
use crate::config::model::{ProviderReasoningCapability, ReasoningEffort, ReasoningSummary};
use crate::config::{ProviderMetadataMode, ProviderTarget};
use crate::error::{LlmError, ProviderRequestLimit};
use crate::session::{FinishReason, TokenUsage};

use super::provider::ProviderPhaseEvent;
use super::validate_image_payload;

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

/// A fully prepared, transport-only generation request.
///
/// This type deliberately has no generic serialization contract. Provider wire
/// DTOs are built explicitly by each transport, and its manual `Debug`
/// projection omits endpoint, header, prompt, message, and extra-body contents.
#[derive(Clone)]
pub struct ChatRequest {
    provider: ProviderTarget,
    pub(crate) model: ModelProfile,
    pub(crate) system_prompt: String,
    pub(crate) messages: Vec<ModelMessage>,
    pub(crate) tools: Vec<ToolSchema>,
    pub(crate) reasoning: Option<ReasoningRequest>,
    pub(crate) reasoning_capability: ProviderReasoningCapability,
    pub(crate) responses_continuation: Option<ResponsesContinuation>,
    pub(crate) tool_choice: Option<ProviderToolChoice>,
    pub(crate) parallel_tool_calls: bool,
    extra_headers: BTreeMap<String, String>,
    pub(crate) temperature: Option<f64>,
    pub(crate) top_p: Option<f64>,
    pub(crate) top_k: Option<u32>,
    pub(crate) presence_penalty: Option<f64>,
    pub(crate) frequency_penalty: Option<f64>,
    pub(crate) seed: Option<u64>,
    pub(crate) stop_sequences: Vec<String>,
    pub(crate) extra_body: Option<serde_json::Value>,
}

impl fmt::Debug for ChatRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChatRequest")
            .field("model", &self.model)
            .field("provider", &self.provider)
            .field("system_prompt_chars", &self.system_prompt.chars().count())
            .field("message_count", &self.messages.len())
            .field("tool_count", &self.tools.len())
            .field("reasoning", &self.reasoning)
            .field("reasoning_capability", &self.reasoning_capability)
            .field(
                "responses_continuation_present",
                &self.responses_continuation.is_some(),
            )
            .field("tool_choice", &self.tool_choice)
            .field("parallel_tool_calls", &self.parallel_tool_calls)
            .field("extra_headers", &"<redacted>")
            .field("temperature", &self.temperature)
            .field("top_p", &self.top_p)
            .field("top_k", &self.top_k)
            .field("presence_penalty", &self.presence_penalty)
            .field("frequency_penalty", &self.frequency_penalty)
            .field("seed", &self.seed)
            .field("stop_sequence_count", &self.stop_sequences.len())
            .field("extra_body_present", &self.extra_body.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponsesContinuation {
    pub previous_response_id: String,
    /// Index into the non-system messages in `ChatRequest.messages`. Messages
    /// before this point are already represented by `previous_response_id`;
    /// system messages always remain part of the current `instructions` projection.
    pub input_start: usize,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedResponsesReasoningRequest {
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
        ProviderReasoningCapability::Responses { .. } => Err(LlmError::Message(
            "Responses reasoning capability cannot be used with the Chat Completions transport"
                .to_string(),
        )),
    }
}

pub(crate) fn validate_responses_reasoning_request(
    request: Option<&ReasoningRequest>,
    capability: ProviderReasoningCapability,
) -> Result<Option<ValidatedResponsesReasoningRequest>, LlmError> {
    let Some(request) = request.filter(|request| !request.is_disabled()) else {
        return Ok(None);
    };

    match capability {
        ProviderReasoningCapability::Unsupported => Err(LlmError::Message(
            "reasoning parameters were requested for a provider that does not advertise a typed reasoning request contract"
                .to_string(),
        )),
        ProviderReasoningCapability::ChatCompletions { .. } => Err(LlmError::Message(
            "Chat Completions reasoning capability cannot be used with the Responses transport"
                .to_string(),
        )),
        ProviderReasoningCapability::Responses {
            supports_summary, ..
        } => {
            if request.summary != ReasoningSummary::None && !supports_summary {
                return Err(LlmError::Message(
                    "reasoning summary was requested for a Responses provider that does not advertise summary support"
                        .to_string(),
                ));
            }
            Ok(Some(ValidatedResponsesReasoningRequest {
                effort: request.effort.clone(),
                summary: (request.summary != ReasoningSummary::None).then_some(request.summary),
            }))
        }
    }
}

impl ChatRequest {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        provider: ProviderTarget,
        model: ModelProfile,
        system_prompt: String,
        messages: Vec<ModelMessage>,
        tools: Vec<ToolSchema>,
        reasoning: Option<ReasoningRequest>,
        reasoning_capability: ProviderReasoningCapability,
        extra_headers: BTreeMap<String, String>,
    ) -> Self {
        Self {
            provider,
            model,
            system_prompt,
            messages,
            tools,
            reasoning,
            reasoning_capability,
            responses_continuation: None,
            tool_choice: None,
            parallel_tool_calls: false,
            extra_headers,
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

    pub(crate) fn provider_target(&self) -> &ProviderTarget {
        &self.provider
    }

    pub(crate) fn extra_headers(&self) -> &BTreeMap<String, String> {
        &self.extra_headers
    }

    #[cfg(test)]
    pub(crate) fn replace_provider_target(&mut self, provider: ProviderTarget) {
        self.provider = provider;
    }

    #[cfg(test)]
    pub(crate) fn replace_extra_headers(&mut self, headers: BTreeMap<String, String>) {
        self.extra_headers = headers;
    }

    pub fn effective_max_output_tokens(&self) -> u32 {
        effective_max_output_tokens_for_request(self).0
    }

    pub fn output_budget_reason(&self) -> &'static str {
        effective_max_output_tokens_for_request(self).1
    }

    pub fn validate_provider_lifecycle(&self) -> Result<(), LlmError> {
        if self.model.name.trim().is_empty() {
            return Err(LlmError::Message(
                "ChatRequest model profile name must not be empty".to_string(),
            ));
        }
        if self.model.name != self.provider.model() {
            return Err(LlmError::Message(format!(
                "ChatRequest model profile `{}` does not match canonical provider target model `{}`",
                self.model.name,
                self.provider.model()
            )));
        }
        if self.model.provider_metadata_mode != self.provider.metadata_mode() {
            return Err(LlmError::Message(format!(
                "ChatRequest model profile metadata mode {:?} does not match canonical provider target mode {:?}",
                self.model.provider_metadata_mode,
                self.provider.metadata_mode()
            )));
        }
        self.validate_request_envelope_shape()?;
        if let Some(capability_api_mode) = self.reasoning_capability.api_mode()
            && capability_api_mode != self.provider.api_mode()
        {
            return Err(LlmError::Message(format!(
                "reasoning capability for {capability_api_mode:?} cannot be used with {:?}",
                self.provider.api_mode()
            )));
        }
        if !self.model.capabilities.supports_tools && !self.tools.is_empty() {
            return Err(LlmError::Message(
                "ChatRequest provider tools require a tool-capable model profile".to_string(),
            ));
        }
        if !self.model.capabilities.supports_reasoning
            && self
                .reasoning
                .as_ref()
                .is_some_and(|reasoning| !reasoning.is_disabled())
        {
            return Err(LlmError::Message(
                "ChatRequest reasoning requires a reasoning-capable model profile".to_string(),
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
        if let Some(continuation) = &self.responses_continuation {
            if self.provider.api_mode() != ProviderApiMode::Responses {
                return Err(LlmError::Message(
                    "previous_response_id continuation requires the Responses API".to_string(),
                ));
            }
            if continuation.previous_response_id.trim().is_empty() {
                return Err(LlmError::Message(
                    "Responses continuation requires a non-empty previous_response_id".to_string(),
                ));
            }
            let non_system_message_count = self
                .messages
                .iter()
                .filter(|message| !matches!(message, ModelMessage::System { .. }))
                .count();
            if continuation.input_start > non_system_message_count {
                return Err(LlmError::Message(format!(
                    "Responses continuation input_start {} exceeds non-system message count {}",
                    continuation.input_start, non_system_message_count
                )));
            }
            if !matches!(
                self.reasoning_capability,
                ProviderReasoningCapability::Responses {
                    supports_previous_response_id: true,
                    ..
                }
            ) {
                return Err(LlmError::Message(
                    "Responses continuation requires advertised previous_response_id support"
                        .to_string(),
                ));
            }
        }
        Ok(())
    }

    fn validate_request_envelope_shape(&self) -> Result<(), LlmError> {
        let limits = self.provider.request_limits();
        let message_count = if self.provider.api_mode() == ProviderApiMode::ChatCompletions {
            self.messages
                .iter()
                .filter(|message| !matches!(message, ModelMessage::System { .. }))
                .count()
                .saturating_add(1)
        } else {
            self.messages.len()
        };
        ensure_request_limit(
            ProviderRequestLimit::MessageCount,
            message_count as u64,
            limits.max_messages as u64,
        )?;
        ensure_request_limit(
            ProviderRequestLimit::ToolCount,
            self.tools.len() as u64,
            limits.max_tools as u64,
        )?;
        ensure_request_limit(
            ProviderRequestLimit::ToolSchemaBytes,
            serialized_json_len(&self.tools)?,
            limits.max_tool_schema_bytes,
        )?;
        if let Some(extra_body) = &self.extra_body {
            ensure_request_limit(
                ProviderRequestLimit::ExtraBodyBytes,
                serialized_json_len(extra_body)?,
                limits.max_extra_body_bytes,
            )?;
        }
        ensure_request_limit(
            ProviderRequestLimit::StopSequenceCount,
            self.stop_sequences.len() as u64,
            limits.max_stop_sequences as u64,
        )?;
        let stop_sequence_bytes = self.stop_sequences.iter().try_fold(0_u64, |total, value| {
            total
                .checked_add(value.len() as u64)
                .ok_or(LlmError::ProviderRequestLimitExceeded {
                    surface: ProviderRequestLimit::StopSequenceBytes,
                    actual: u64::MAX,
                    maximum: limits.max_stop_sequence_bytes,
                })
        })?;
        ensure_request_limit(
            ProviderRequestLimit::StopSequenceBytes,
            stop_sequence_bytes,
            limits.max_stop_sequence_bytes,
        )?;

        let mut image_count = 0_u64;
        let mut total_base64_chars = 0_u64;
        let mut total_decoded_bytes = 0_u64;
        let per_image_base64_chars =
            4_u64.saturating_mul(limits.max_single_image_decoded_bytes.saturating_add(2) / 3);
        for part in self.messages.iter().flat_map(message_content_parts) {
            let ModelContentPart::Image {
                mime_type,
                data_base64,
            } = part
            else {
                continue;
            };
            image_count = image_count.saturating_add(1);
            ensure_request_limit(
                ProviderRequestLimit::ImageCount,
                image_count,
                limits.max_images as u64,
            )?;
            ensure_request_limit(
                ProviderRequestLimit::ImageBase64Chars,
                data_base64.len() as u64,
                per_image_base64_chars,
            )?;
            total_base64_chars = total_base64_chars.saturating_add(data_base64.len() as u64);
            ensure_request_limit(
                ProviderRequestLimit::ImageBase64Chars,
                total_base64_chars,
                limits.max_total_image_base64_chars,
            )?;
            let metadata = validate_image_payload(mime_type, data_base64, limits)?;
            total_decoded_bytes = total_decoded_bytes.saturating_add(metadata.decoded_bytes);
            ensure_request_limit(
                ProviderRequestLimit::ImageDecodedBytes,
                total_decoded_bytes,
                limits.max_total_image_decoded_bytes,
            )?;
        }
        Ok(())
    }

    /// Serializes the exact provider DTO into a bounded buffer before the POST is constructed.
    /// A rejected body therefore cannot reach the provider or allocate beyond the admitted limit.
    pub(crate) fn serialize_wire_body<T: Serialize>(&self, body: &T) -> Result<Vec<u8>, LlmError> {
        let maximum = self.provider.request_limits().max_serialized_body_bytes;
        let mut writer = BoundedJsonBuffer::new(maximum);
        let result = serde_json::to_writer(&mut writer, body);
        if writer.exceeded {
            return Err(LlmError::ProviderRequestLimitExceeded {
                surface: ProviderRequestLimit::SerializedBodyBytes,
                actual: writer.attempted_bytes,
                maximum,
            });
        }
        result?;
        Ok(writer.bytes)
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

fn message_content_parts(message: &ModelMessage) -> &[ModelContentPart] {
    match message {
        ModelMessage::UserParts { parts } => parts,
        _ => &[],
    }
}

fn ensure_request_limit(
    surface: ProviderRequestLimit,
    actual: u64,
    maximum: u64,
) -> Result<(), LlmError> {
    if actual > maximum {
        return Err(LlmError::ProviderRequestLimitExceeded {
            surface,
            actual,
            maximum,
        });
    }
    Ok(())
}

fn serialized_json_len<T: Serialize>(value: &T) -> Result<u64, LlmError> {
    let mut writer = JsonByteCounter::default();
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer.bytes)
}

#[derive(Default)]
struct JsonByteCounter {
    bytes: u64,
}

impl Write for JsonByteCounter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buffer.len() as u64);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct BoundedJsonBuffer {
    bytes: Vec<u8>,
    maximum: u64,
    attempted_bytes: u64,
    exceeded: bool,
}

impl BoundedJsonBuffer {
    fn new(maximum: u64) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
            attempted_bytes: 0,
            exceeded: false,
        }
    }
}

impl Write for BoundedJsonBuffer {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.attempted_bytes = self.attempted_bytes.saturating_add(buffer.len() as u64);
        if self.attempted_bytes > self.maximum {
            self.exceeded = true;
            return Err(std::io::Error::other(
                "provider request exceeded its admitted serialized-body limit",
            ));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
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
            "internal_debug": {"trace": "must_not_replay"}
        }),
    };

    let serialized = serde_json::to_value(&message).unwrap_or(serde_json::Value::Null);
    serialized.get("metadata").is_none() && !serialized.to_string().contains("internal_debug")
}

pub fn model_tool_replay_metadata_is_not_deserialized_fixture_passes() -> bool {
    let incoming = serde_json::json!({
        "role": "tool",
        "call_id": "call_1",
        "tool_name": "read",
        "result": "repository excerpt",
        "metadata": {
            "internal_debug": {"trace": "must_not_replay"}
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
    /// A provider-confirmed summary of its reasoning, never raw chain-of-thought.
    ///
    /// This is a runtime/client projection only. It must not be added to model
    /// messages or canonical conversation history.
    ReasoningSummaryDelta(String),
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

    /// Low-volume typed transport lifecycle channel. It is deliberately
    /// separate from model output deltas so consumers that only collect model
    /// content do not need to reinterpret diagnostics as assistant output.
    fn provider_phase(&mut self, _event: ProviderPhaseEvent) -> Result<(), LlmError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponseSummary {
    /// Terminal transport metadata only. Provider reasoning summaries are
    /// emitted as runtime-only [`LlmEvent::ReasoningSummaryDelta`] values and
    /// are deliberately not duplicated here.
    pub finish_reason: FinishReason,
    pub usage: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
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
        ChatRequest, LlmResponseSummary, ModelCapabilities, ModelMessage, ModelProfile,
        ReasoningRequest, ToolSchema, validate_chat_completions_reasoning_request,
        validate_responses_reasoning_request, validate_toolless_text_response,
    };
    use crate::config::model::{
        ChatCompletionsReasoningParameters, ProviderApiMode, ProviderReasoningCapability,
        ReasoningEffort, ReasoningSummary,
    };
    use crate::config::{
        ProviderDeadlines, ProviderMetadataMode, ProviderRequestLimits, ProviderTarget,
    };
    use crate::error::{LlmError, ProviderRequestLimit};
    use crate::session::FinishReason;

    #[test]
    fn prepared_request_debug_uses_the_typed_target_and_omits_raw_payload_secrets() {
        let provider = ProviderTarget::new(
            "http://lm-studio.local:1234/v1",
            "fixture-model",
            ProviderMetadataMode::OpenAiCompatibleOnly,
            ProviderApiMode::Responses,
            ProviderDeadlines {
                response_start_timeout_ms: 10_000,
                stream_idle_timeout_ms: 10_000,
                connect_timeout_ms: 1_000,
                max_connect_retries: 0,
            },
        )
        .expect("typed provider target");
        let mut request = ChatRequest::new(
            provider,
            ModelProfile {
                name: "fixture-model".to_string(),
                context_window: 16_384,
                max_output_tokens: 1_024,
                provider_metadata_mode: ProviderMetadataMode::OpenAiCompatibleOnly,
                capabilities: ModelCapabilities {
                    supports_tools: true,
                    supports_reasoning: true,
                    supports_images: false,
                },
            },
            "system-super-secret".to_string(),
            vec![ModelMessage::User {
                content: "message-super-secret".to_string(),
            }],
            Vec::new(),
            None,
            ProviderReasoningCapability::Responses {
                supports_summary: true,
                supports_previous_response_id: true,
            },
            std::collections::BTreeMap::from([(
                "Authorization".to_string(),
                "Bearer header-super-secret".to_string(),
            )]),
        );
        request.extra_body = Some(serde_json::json!({"api_key": "body-super-secret"}));

        let debug = format!("{request:?}");

        assert!(debug.contains("ProviderTarget"));
        assert!(debug.contains("http://lm-studio.local:1234/v1"));
        for secret in [
            "system-super-secret",
            "message-super-secret",
            "header-super-secret",
            "body-super-secret",
        ] {
            assert!(!debug.contains(secret));
        }
    }

    #[test]
    fn admitted_envelope_rejects_schema_and_exact_wire_bytes_before_transport() {
        let mut provider = ProviderTarget::new(
            "http://lm-studio.local:1234/v1",
            "fixture-model",
            ProviderMetadataMode::OpenAiCompatibleOnly,
            ProviderApiMode::ChatCompletions,
            ProviderDeadlines {
                response_start_timeout_ms: 10_000,
                stream_idle_timeout_ms: 10_000,
                connect_timeout_ms: 1_000,
                max_connect_retries: 0,
            },
        )
        .expect("typed provider target");
        let mut limits = ProviderRequestLimits::product_default();
        limits.max_tool_schema_bytes = 32;
        limits.max_serialized_body_bytes = 64;
        provider.replace_request_limits(limits);
        let mut request = ChatRequest::new(
            provider,
            ModelProfile {
                name: "fixture-model".to_string(),
                context_window: 16_384,
                max_output_tokens: 1_024,
                provider_metadata_mode: ProviderMetadataMode::OpenAiCompatibleOnly,
                capabilities: ModelCapabilities {
                    supports_tools: true,
                    supports_reasoning: false,
                    supports_images: false,
                },
            },
            "system".to_string(),
            vec![ModelMessage::User {
                content: "request".to_string(),
            }],
            vec![ToolSchema {
                name: "oversized".to_string(),
                description: "x".repeat(64),
                input_schema: serde_json::json!({"type": "object"}),
            }],
            None,
            ProviderReasoningCapability::Unsupported,
            std::collections::BTreeMap::new(),
        );

        assert!(matches!(
            request.validate_provider_lifecycle(),
            Err(LlmError::ProviderRequestLimitExceeded {
                surface: ProviderRequestLimit::ToolSchemaBytes,
                ..
            })
        ));

        request.tools.clear();
        assert!(matches!(
            request.serialize_wire_body(&serde_json::json!({"input": "x".repeat(128)})),
            Err(LlmError::ProviderRequestLimitExceeded {
                surface: ProviderRequestLimit::SerializedBodyBytes,
                maximum: 64,
                ..
            })
        ));
    }

    #[test]
    fn chat_message_limit_counts_the_injected_system_message() {
        let mut provider = ProviderTarget::new(
            "http://lm-studio.local:1234/v1",
            "fixture-model",
            ProviderMetadataMode::OpenAiCompatibleOnly,
            ProviderApiMode::ChatCompletions,
            ProviderDeadlines {
                response_start_timeout_ms: 10_000,
                stream_idle_timeout_ms: 10_000,
                connect_timeout_ms: 1_000,
                max_connect_retries: 0,
            },
        )
        .expect("typed provider target");
        let mut limits = ProviderRequestLimits::product_default();
        limits.max_messages = 2;
        provider.replace_request_limits(limits);
        let mut request = ChatRequest::new(
            provider,
            ModelProfile {
                name: "fixture-model".to_string(),
                context_window: 16_384,
                max_output_tokens: 1_024,
                provider_metadata_mode: ProviderMetadataMode::OpenAiCompatibleOnly,
                capabilities: ModelCapabilities {
                    supports_tools: true,
                    supports_reasoning: false,
                    supports_images: false,
                },
            },
            "injected system".to_string(),
            vec![
                ModelMessage::System {
                    content: "merged system".to_string(),
                },
                ModelMessage::User {
                    content: "first user".to_string(),
                },
            ],
            Vec::new(),
            None,
            ProviderReasoningCapability::Unsupported,
            std::collections::BTreeMap::new(),
        );

        request
            .validate_provider_lifecycle()
            .expect("one wire system plus one user reaches the boundary");
        request.messages.push(ModelMessage::User {
            content: "second user".to_string(),
        });

        assert!(matches!(
            request.validate_provider_lifecycle(),
            Err(LlmError::ProviderRequestLimitExceeded {
                surface: ProviderRequestLimit::MessageCount,
                actual: 3,
                maximum: 2,
            })
        ));
    }

    #[test]
    fn provider_lifecycle_rejects_stale_or_blank_model_profiles() {
        let provider = ProviderTarget::new(
            "http://lm-studio.local:1234/v1",
            "canonical-model",
            ProviderMetadataMode::OpenAiCompatibleOnly,
            ProviderApiMode::Responses,
            ProviderDeadlines {
                response_start_timeout_ms: 10_000,
                stream_idle_timeout_ms: 10_000,
                connect_timeout_ms: 1_000,
                max_connect_retries: 0,
            },
        )
        .expect("typed provider target");
        let mut request = ChatRequest::new(
            provider,
            ModelProfile {
                name: "stale-model".to_string(),
                context_window: 16_384,
                max_output_tokens: 1_024,
                provider_metadata_mode: ProviderMetadataMode::OpenAiCompatibleOnly,
                capabilities: ModelCapabilities {
                    supports_tools: true,
                    supports_reasoning: false,
                    supports_images: false,
                },
            },
            "system".to_string(),
            vec![ModelMessage::User {
                content: "request".to_string(),
            }],
            Vec::new(),
            None,
            ProviderReasoningCapability::Unsupported,
            std::collections::BTreeMap::new(),
        );

        let stale = request
            .validate_provider_lifecycle()
            .expect_err("stale model profile must fail closed");
        assert!(
            stale
                .to_string()
                .contains("canonical provider target model")
        );

        request.model.name = " \t ".to_string();
        let blank = request
            .validate_provider_lifecycle()
            .expect_err("blank model profile must fail closed");
        assert!(blank.to_string().contains("must not be empty"));
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
                response_id: None,
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
            response_id: None,
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
            ProviderReasoningCapability::Responses {
                supports_summary: true,
                supports_previous_response_id: true,
            },
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
                ProviderReasoningCapability::Responses {
                    supports_summary: true,
                    supports_previous_response_id: true,
                },
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

    #[test]
    fn responses_reasoning_contract_validates_summary_support() {
        let request = ReasoningRequest {
            effort: Some(ReasoningEffort::High),
            summary: ReasoningSummary::Concise,
        };
        assert!(
            validate_responses_reasoning_request(
                Some(&request),
                ProviderReasoningCapability::Responses {
                    supports_summary: true,
                    supports_previous_response_id: true,
                },
            )
            .is_ok()
        );
        assert!(
            validate_responses_reasoning_request(
                Some(&request),
                ProviderReasoningCapability::Responses {
                    supports_summary: false,
                    supports_previous_response_id: true,
                },
            )
            .is_err()
        );
        assert!(
            validate_responses_reasoning_request(
                Some(&request),
                ProviderReasoningCapability::Unsupported,
            )
            .is_err()
        );
    }
}

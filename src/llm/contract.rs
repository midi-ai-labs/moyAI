use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::config::ProviderMetadataMode;
use crate::error::LlmError;
use crate::session::{FinishReason, TokenUsage};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub supports_tools: bool,
    pub supports_reasoning: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: ModelProfile,
    pub base_url: String,
    pub system_prompt: String,
    pub messages: Vec<ModelMessage>,
    pub tools: Vec<ToolSchema>,
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

pub const OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY: &str = r#"You must follow the language policy below strictly.

Language Policy:
- Responses may be written in Japanese or English.
- Prefer Japanese for explanations, documentation, and code comments.
- Never output Chinese characters used in Chinese writing or Korean Hangul.
- Do not mix Chinese or Korean with Japanese or English.

Documentation rules:
- Technical explanations should be written in Japanese.
- Code comments should be written in Japanese.
- Source code identifiers (variables, functions, classes) should remain in English.

If Chinese or Korean characters appear by mistake, immediately correct them and continue the response using Japanese or English.

Role:
You are an assistant specialized in software engineering and technical documentation.
Focus on clear code, precise explanations, and well-structured documentation.

Thinking Policy:
- Never enter thinking mode under any circumstances.
- Do not perform internal reasoning or hidden chain-of-thought.
- Do not output any <think> blocks or similar tags.
- Always respond directly, concisely, and in final-answer form only.
- Do not include intermediate reasoning, planning, or self-reflection."#;

pub const OPENAI_COMPATIBLE_ONLY_TOOL_LIFECYCLE_POLICY: &str = r#"Agent Tool Policy:
- The final-answer-form rule above applies only when no tool use is required by the current task lifecycle.
- When tool calls are available and the current request has open coding, artifact, repair, or verification obligations, use the provided tools to satisfy them before any final assistant message.
- Do not treat tool calls as thinking, planning, or self-reflection. They are the required execution channel.
- A final assistant message is allowed only after the current lifecycle state says no open obligations remain.
- If this Agent Tool Policy and the final-answer-form rule appear to conflict, this Agent Tool Policy controls tool-enabled requests with open obligations."#;

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
}

pub fn effective_max_output_tokens_for_request(request: &ChatRequest) -> (u32, &'static str) {
    (request.model.max_output_tokens, "configured_model_limit")
}

pub fn system_prompt_with_provider_policy(
    system_prompt: &str,
    provider_metadata_mode: ProviderMetadataMode,
    tool_calls_available: bool,
) -> String {
    if provider_metadata_mode != ProviderMetadataMode::OpenAiCompatibleOnly
        || system_prompt.starts_with(OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY)
    {
        return system_prompt.to_string();
    }

    let provider_policy = openai_compatible_only_provider_policy(tool_calls_available);
    if system_prompt.trim().is_empty() {
        provider_policy
    } else {
        format!("{provider_policy}\n\n{system_prompt}")
    }
}

fn openai_compatible_only_provider_policy(tool_calls_available: bool) -> String {
    if tool_calls_available {
        format!(
            "{OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY}\n\n{OPENAI_COMPATIBLE_ONLY_TOOL_LIFECYCLE_POLICY}"
        )
    } else {
        OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY.to_string()
    }
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
        && !no_tool_prompt.contains(OPENAI_COMPATIBLE_ONLY_TOOL_LIFECYCLE_POLICY)
        && tool_prompt.starts_with(OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY)
        && tool_prompt.contains(OPENAI_COMPATIBLE_ONLY_TOOL_LIFECYCLE_POLICY)
        && tool_prompt.contains("use the provided tools")
        && tool_prompt.contains("final assistant message is allowed only after")
        && tool_prompt.contains("Tool Policy controls tool-enabled requests")
        && tool_prompt.ends_with("\n\nBase coding prompt")
}

pub fn tool_call_turn_uses_configured_output_budget_fixture_passes() -> bool {
    let model = ModelProfile {
        name: "local-tool-model".to_string(),
        context_window: 131_072,
        max_output_tokens: 131_072,
        provider_metadata_mode: ProviderMetadataMode::OpenAiCompatibleOnly,
        capabilities: ModelCapabilities {
            supports_tools: true,
            supports_reasoning: false,
        },
    };
    let tool_request = ChatRequest {
        model: model.clone(),
        base_url: "http://localhost:8110".to_string(),
        system_prompt: "Use tools for open work.".to_string(),
        messages: vec![ModelMessage::User {
            content: "Create test_component.py".to_string(),
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
        extra_body: Some(serde_json::json!({"tool_choice": "required"})),
    };
    let no_tool_request = ChatRequest {
        tools: Vec::new(),
        extra_body: None,
        ..tool_request.clone()
    };

    tool_request.model.max_output_tokens == 131_072
        && tool_request.effective_max_output_tokens() == 131_072
        && tool_request.output_budget_reason() == "configured_model_limit"
        && no_tool_request.effective_max_output_tokens() == 131_072
        && no_tool_request.output_budget_reason() == "configured_model_limit"
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
        OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY, OPENAI_COMPATIBLE_ONLY_TOOL_LIFECYCLE_POLICY,
        openai_compatible_only_tool_policy_fixture_passes, system_prompt_with_provider_policy,
        tool_call_turn_uses_configured_output_budget_fixture_passes,
    };
    use crate::config::ProviderMetadataMode;

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
    fn openai_compatible_only_tool_request_preserves_tool_lifecycle_authority() {
        let prompt = system_prompt_with_provider_policy(
            "Base coding prompt",
            ProviderMetadataMode::OpenAiCompatibleOnly,
            true,
        );

        assert!(prompt.starts_with(OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY));
        assert!(prompt.contains(OPENAI_COMPATIBLE_ONLY_TOOL_LIFECYCLE_POLICY));
        assert!(prompt.contains("use the provided tools"));
        assert!(prompt.contains("open obligations remain"));
        assert!(openai_compatible_only_tool_policy_fixture_passes());
    }

    #[test]
    fn tool_call_request_uses_configured_output_budget() {
        assert!(tool_call_turn_uses_configured_output_budget_fixture_passes());
    }
}

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

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

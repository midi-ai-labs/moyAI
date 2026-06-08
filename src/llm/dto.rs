use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct OpenAiChatRequest {
    pub model: String,
    pub stream: bool,
    pub messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(rename = "stop", skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<OpenAiToolSchema>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct OpenAiMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<OpenAiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAiMessageToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum OpenAiContent {
    Text(String),
    Parts(Vec<OpenAiContentPart>),
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpenAiContentPart {
    Text { text: String },
    ImageUrl { image_url: OpenAiImageUrl },
}

#[derive(Debug, Serialize)]
pub struct OpenAiImageUrl {
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct OpenAiMessageToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: OpenAiMessageToolCallFunction,
}

#[derive(Debug, Serialize)]
pub struct OpenAiMessageToolCallFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Serialize)]
pub struct OpenAiToolSchema {
    #[serde(rename = "type")]
    pub schema_type: String,
    pub function: OpenAiFunctionSchema,
}

#[derive(Debug, Serialize)]
pub struct OpenAiFunctionSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiChatChunk {
    #[serde(default)]
    pub choices: Vec<OpenAiChoice>,
    pub usage: Option<OpenAiUsage>,
    pub error: Option<OpenAiErrorPayload>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiErrorPayload {
    pub message: Option<String>,
    #[serde(rename = "type")]
    pub error_type: Option<String>,
    pub code: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiChoice {
    pub index: usize,
    pub delta: OpenAiDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct OpenAiDelta {
    pub content: Option<String>,
    #[serde(alias = "reasoning_content")]
    pub reasoning: Option<String>,
    pub tool_calls: Option<Vec<OpenAiToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiToolCallDelta {
    pub index: usize,
    pub id: Option<String>,
    pub function: Option<OpenAiToolCallFunctionDelta>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiToolCallFunctionDelta {
    pub name: Option<String>,
    pub arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub reasoning_tokens: Option<u32>,
}

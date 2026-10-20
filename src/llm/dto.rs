use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct OpenAiChatRequest {
    pub model: String,
    pub stream: bool,
    pub stream_options: OpenAiStreamOptions,
    pub n: u32,
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
pub struct OpenAiStreamOptions {
    pub include_usage: bool,
}

#[derive(Serialize)]
pub struct OpenAiMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<OpenAiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAiMessageToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

impl std::fmt::Debug for OpenAiMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiMessage")
            .field("role", &self.role)
            .field("content", &self.content)
            .field("tool_calls", &self.tool_calls)
            .field("tool_call_id", &self.tool_call_id)
            .field(
                "reasoning_content",
                &self.reasoning_content.as_ref().map(|_| "<private>"),
            )
            .finish()
    }
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

#[derive(Default, Deserialize)]
pub struct OpenAiDelta {
    pub content: Option<String>,
    pub tool_calls: Option<Vec<OpenAiToolCallDelta>>,
    pub reasoning_content: Option<super::continuation::ReasoningFragment>,
    pub reasoning: Option<super::continuation::ReasoningFragment>,
    // Retain extensions so terminal metadata cannot hide additional output.
    #[serde(flatten)]
    pub additional_fields: std::collections::BTreeMap<String, serde_json::Value>,
}

impl std::fmt::Debug for OpenAiDelta {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OpenAiDelta(<private>)")
    }
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
    #[serde(default, deserialize_with = "deserialize_present_value")]
    pub completion_tokens_details: Option<OpenAiCompletionTokensDetails>,
}

#[derive(Debug, Deserialize)]
pub struct OpenAiCompletionTokensDetails {
    pub reasoning_tokens: Option<u32>,
}

fn deserialize_present_value<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_wire_reasoning_is_serialized_but_never_debugged() {
        let secret = "private-dto-canary";
        let outbound = OpenAiMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: Some(secret.into()),
        };
        assert_eq!(
            serde_json::to_value(&outbound).unwrap()["reasoning_content"],
            secret
        );
        assert!(!format!("{outbound:?}").contains(secret));
        let inbound: OpenAiDelta = serde_json::from_value(
            serde_json::json!({"reasoning_content": secret, "reasoning": secret}),
        )
        .unwrap();
        assert!(!format!("{inbound:?}").contains(secret));
    }

    #[test]
    fn unknown_chat_delta_fields_are_retained_without_debugging_their_values() {
        let secret = "private-extension-canary";
        let inbound: OpenAiDelta = serde_json::from_value(serde_json::json!({
            "role": "assistant",
            "reasoning_details": [{"text": secret}]
        }))
        .unwrap();
        assert_eq!(
            inbound.additional_fields["reasoning_details"][0]["text"],
            secret
        );
        assert!(!format!("{inbound:?}").contains(secret));
    }
}

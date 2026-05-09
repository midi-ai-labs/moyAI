use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, RETRY_AFTER};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::error::LlmError;
use crate::llm::dto::{
    OpenAiChatChunk, OpenAiChatRequest, OpenAiContent, OpenAiContentPart, OpenAiErrorPayload,
    OpenAiFunctionSchema, OpenAiImageUrl, OpenAiMessage, OpenAiMessageToolCall,
    OpenAiMessageToolCallFunction, OpenAiToolSchema, OpenAiUsage,
};
use crate::llm::{
    ChatRequest, LlmClient, LlmEvent, LlmEventSink, LlmResponseSummary, ModelMessage,
};
use crate::session::{FinishReason, TokenUsage};
use crate::tool::truncate::clip_text_with_ellipsis;

const RETRY_INITIAL_DELAY_MS: u64 = 2_000;
const RETRY_BACKOFF_FACTOR: u64 = 2;
const RETRY_MAX_DELAY_NO_HEADERS_MS: u64 = 30_000;
const RETRY_MAX_DELAY_MS: u64 = 2_147_483_647;

#[derive(Debug, Clone)]
pub struct OpenAiCompatClient {
    client: reqwest::Client,
    max_retries: u8,
    api_key: Option<String>,
}

impl OpenAiCompatClient {
    pub fn new(
        connect_timeout_ms: u64,
        request_timeout_ms: u64,
        max_retries: u8,
        api_key: Option<String>,
    ) -> Result<Self, LlmError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(connect_timeout_ms))
            .timeout(Duration::from_millis(request_timeout_ms))
            .build()?;
        Ok(Self {
            client,
            max_retries,
            api_key,
        })
    }
}

#[async_trait(?Send)]
impl LlmClient for OpenAiCompatClient {
    async fn stream_chat(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
        sink: &mut dyn LlmEventSink,
    ) -> Result<LlmResponseSummary, LlmError> {
        let response = self.send_request(&request).await?;

        let mut stream = response.bytes_stream().eventsource();
        let mut usage = None;
        let mut finish_reason = None;
        let mut saw_terminal_signal = false;
        let mut ended_by_eof = false;
        let mut tool_calls: HashMap<usize, PartialToolCall> = HashMap::new();

        loop {
            let next_event = if let Some(timeout) =
                stream_idle_timeout(request.stream_idle_timeout_ms)
            {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        return Ok(LlmResponseSummary {
                            finish_reason: FinishReason::Cancelled,
                            usage,
                        });
                    }
                    result = tokio::time::timeout(timeout, stream.next()) => {
                        result.map_err(|_| stream_idle_timeout_error(request.stream_idle_timeout_ms))?
                    }
                }
            } else {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        return Ok(LlmResponseSummary {
                            finish_reason: FinishReason::Cancelled,
                            usage,
                        });
                    }
                    result = stream.next() => result,
                }
            };

            let Some(event) = next_event else {
                ended_by_eof = true;
                break;
            };

            let event =
                event.map_err(|error| LlmError::Message(format!("SSE stream error: {error}")))?;
            if event.data == "[DONE]" {
                saw_terminal_signal = true;
                break;
            }

            let chunk = serde_json::from_str::<OpenAiChatChunk>(&event.data).map_err(|error| {
                LlmError::Message(format!(
                    "failed to parse openai-compatible stream chunk: {}. Raw chunk: {}",
                    error,
                    summarize_stream_chunk(&event.data)
                ))
            })?;
            if let Some(error) = chunk.error.as_ref() {
                return Err(LlmError::Message(format!(
                    "openai-compatible stream error: {}",
                    summarize_stream_error(error)
                )));
            }
            if let Some(value) = chunk.usage.as_ref() {
                usage = Some(to_usage(value));
            }
            if chunk.choices.is_empty() {
                continue;
            }

            for choice in chunk.choices {
                if let Some(value) = choice.delta.content {
                    sink.push(LlmEvent::TextDelta(value))?;
                }
                if let Some(value) = choice.delta.reasoning {
                    sink.push(LlmEvent::ReasoningDelta(value))?;
                }
                if let Some(deltas) = choice.delta.tool_calls {
                    for delta in deltas {
                        let entry = tool_calls.entry(delta.index).or_default();
                        if let Some(id) = delta.id {
                            entry.call_id = Some(id);
                        }
                        if let Some(function) = delta.function {
                            if let Some(name) = function.name {
                                entry.tool_name = Some(name);
                            }
                            if let Some(arguments) = function.arguments {
                                entry.arguments.push_str(&arguments);
                            }
                        }
                        let call_id = entry
                            .call_id
                            .clone()
                            .unwrap_or_else(|| format!("tool_call_{}", choice.index));
                        let tool_name = entry
                            .tool_name
                            .clone()
                            .unwrap_or_else(|| "unknown".to_string());
                        if !entry.started {
                            sink.push(LlmEvent::ToolCallStart {
                                call_id: call_id.clone(),
                                tool_name: tool_name.clone(),
                            })?;
                            entry.started = true;
                        }
                        if !entry.arguments.is_empty() {
                            sink.push(LlmEvent::ToolCallArgsDelta {
                                call_id,
                                delta: entry.arguments_delta(),
                            })?;
                        }
                    }
                }
                if let Some(value) = choice.finish_reason {
                    saw_terminal_signal = true;
                    finish_reason = Some(parse_finish_reason(&value));
                }
            }
        }

        if ended_by_eof && !saw_terminal_signal {
            return Err(stream_missing_terminal_signal_error());
        }

        let finish_reason = finish_reason.unwrap_or(FinishReason::Stop);

        sink.push(LlmEvent::Finished {
            finish_reason,
            usage: usage.clone(),
        })?;
        Ok(LlmResponseSummary {
            finish_reason,
            usage,
        })
    }
}

impl OpenAiCompatClient {
    async fn send_request(&self, request: &ChatRequest) -> Result<reqwest::Response, LlmError> {
        let mut attempt = 0u8;
        loop {
            let mut headers = HeaderMap::new();
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            if let Some(api_key) = &self.api_key {
                let value =
                    HeaderValue::from_str(&format!("Bearer {api_key}")).map_err(|error| {
                        LlmError::Message(format!("invalid API key header: {error}"))
                    })?;
                headers.insert(AUTHORIZATION, value);
            }
            apply_extra_headers(&mut headers, &request.extra_headers)?;

            let result = self
                .client
                .post(format!(
                    "{}/v1/chat/completions",
                    request.base_url.trim_end_matches('/')
                ))
                .timeout(Duration::from_millis(request.timeout_ms))
                .headers(headers)
                .json(&to_openai_request(request)?)
                .send()
                .await;

            match result {
                Ok(response) if response.status().is_success() => return Ok(response),
                Ok(response) => {
                    let failure = parse_response_failure(response).await?;
                    if failure.retryable && attempt < self.max_retries {
                        attempt += 1;
                        tokio::time::sleep(Duration::from_millis(retry_delay_ms(
                            attempt,
                            failure.retry_after_ms,
                        )))
                        .await;
                        continue;
                    }
                    return Err(LlmError::Message(format!(
                        "openai-compatible request failed with status {}: {}",
                        failure.status, failure.message
                    )));
                }
                Err(error)
                    if should_retry_transport_error(&error) && attempt < self.max_retries =>
                {
                    attempt += 1;
                    tokio::time::sleep(Duration::from_millis(retry_delay_ms(attempt, None))).await;
                }
                Err(error) => return Err(LlmError::Http(error)),
            }
        }
    }
}

#[derive(Debug)]
struct ResponseFailure {
    status: StatusCode,
    message: String,
    retryable: bool,
    retry_after_ms: Option<u64>,
}

async fn parse_response_failure(response: reqwest::Response) -> Result<ResponseFailure, LlmError> {
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.text().await.unwrap_or_default();
    Ok(ResponseFailure {
        status,
        message: summarize_failure_body(&body),
        retryable: is_retryable_status(status, &body),
        retry_after_ms: retry_after_ms(&headers),
    })
}

fn stream_idle_timeout(timeout_ms: u64) -> Option<Duration> {
    (timeout_ms > 0).then(|| Duration::from_millis(timeout_ms))
}

fn stream_idle_timeout_error(timeout_ms: u64) -> LlmError {
    LlmError::Message(format!(
        "provider stream idle timeout after {timeout_ms}ms without any SSE event"
    ))
}

fn stream_missing_terminal_signal_error() -> LlmError {
    LlmError::Message(
        "openai-compatible stream ended without terminal [DONE] event or finish_reason".to_string(),
    )
}

fn summarize_failure_body(body: &str) -> String {
    let compact = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        "request failed without a response body".to_string()
    } else if compact.len() > 240 {
        clip_text_with_ellipsis(&compact, 243)
    } else {
        compact
    }
}

fn is_retryable_status(status: StatusCode, body: &str) -> bool {
    if matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS
            | StatusCode::REQUEST_TIMEOUT
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
            | StatusCode::BAD_GATEWAY
    ) {
        return true;
    }

    let lower = body.to_ascii_lowercase();
    lower.contains("overloaded")
        || lower.contains("rate limit")
        || lower.contains("too many requests")
        || lower.contains("temporarily unavailable")
        || lower.contains("timeout")
}

fn should_retry_transport_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || error.is_request() || error.is_body()
}

fn retry_after_ms(headers: &HeaderMap) -> Option<u64> {
    if let Some(value) = headers
        .get("retry-after-ms")
        .and_then(|value| value.to_str().ok())
    {
        if let Some(parsed) = parse_ms_header(value) {
            return Some(parsed.min(RETRY_MAX_DELAY_MS));
        }
    }

    let value = headers.get(RETRY_AFTER)?.to_str().ok()?;
    if let Some(parsed_seconds) = parse_seconds_header(value) {
        return Some((parsed_seconds * 1_000).min(RETRY_MAX_DELAY_MS));
    }
    let retry_at = httpdate::parse_http_date(value).ok()?;
    let delta = retry_at
        .duration_since(std::time::SystemTime::now())
        .ok()?
        .as_millis() as u64;
    Some(delta.min(RETRY_MAX_DELAY_MS))
}

fn parse_ms_header(value: &str) -> Option<u64> {
    let parsed = value.trim().parse::<f64>().ok()?;
    if parsed.is_sign_negative() {
        return None;
    }
    Some(parsed.ceil() as u64)
}

fn parse_seconds_header(value: &str) -> Option<u64> {
    let parsed = value.trim().parse::<f64>().ok()?;
    if parsed.is_sign_negative() {
        return None;
    }
    Some(parsed.ceil() as u64)
}

fn retry_delay_ms(attempt: u8, header_delay_ms: Option<u64>) -> u64 {
    if let Some(delay) = header_delay_ms {
        return delay.min(RETRY_MAX_DELAY_MS);
    }

    let pow = RETRY_BACKOFF_FACTOR.saturating_pow(u32::from(attempt.saturating_sub(1)));
    (RETRY_INITIAL_DELAY_MS.saturating_mul(pow)).min(RETRY_MAX_DELAY_NO_HEADERS_MS)
}

#[derive(Default)]
struct PartialToolCall {
    call_id: Option<String>,
    tool_name: Option<String>,
    arguments: String,
    emitted_len: usize,
    started: bool,
}

impl PartialToolCall {
    fn arguments_delta(&mut self) -> String {
        let delta = self.arguments[self.emitted_len..].to_string();
        self.emitted_len = self.arguments.len();
        delta
    }
}

fn to_openai_request(request: &ChatRequest) -> Result<Value, LlmError> {
    let mut messages = Vec::with_capacity(request.messages.len() + 1);
    messages.push(OpenAiMessage {
        role: "system".to_string(),
        content: Some(OpenAiContent::Text(request.system_prompt.clone())),
        tool_calls: None,
        tool_call_id: None,
    });
    messages.extend(request.messages.iter().map(|message| {
        match message {
            ModelMessage::System { content } => OpenAiMessage {
                role: "system".to_string(),
                content: Some(OpenAiContent::Text(content.clone())),
                tool_calls: None,
                tool_call_id: None,
            },
            ModelMessage::User { content } => OpenAiMessage {
                role: "user".to_string(),
                content: Some(OpenAiContent::Text(content.clone())),
                tool_calls: None,
                tool_call_id: None,
            },
            ModelMessage::UserParts { parts } => OpenAiMessage {
                role: "user".to_string(),
                content: Some(OpenAiContent::Parts(
                    parts
                        .iter()
                        .map(|part| match part {
                            crate::llm::ModelContentPart::Text { text } => {
                                OpenAiContentPart::Text { text: text.clone() }
                            }
                            crate::llm::ModelContentPart::Image {
                                mime_type,
                                data_base64,
                            } => OpenAiContentPart::ImageUrl {
                                image_url: OpenAiImageUrl {
                                    url: format!("data:{mime_type};base64,{data_base64}"),
                                },
                            },
                        })
                        .collect(),
                )),
                tool_calls: None,
                tool_call_id: None,
            },
            ModelMessage::Assistant { content } => OpenAiMessage {
                role: "assistant".to_string(),
                content: Some(OpenAiContent::Text(content.clone())),
                tool_calls: None,
                tool_call_id: None,
            },
            ModelMessage::AssistantToolCalls {
                content,
                tool_calls,
            } => OpenAiMessage {
                role: "assistant".to_string(),
                content: Some(OpenAiContent::Text(content.clone().unwrap_or_default())),
                tool_calls: Some(
                    tool_calls
                        .iter()
                        .map(|tool_call| OpenAiMessageToolCall {
                            id: tool_call.call_id.clone(),
                            call_type: "function".to_string(),
                            function: OpenAiMessageToolCallFunction {
                                name: tool_call.tool_name.clone(),
                                arguments: tool_call.arguments_json.clone(),
                            },
                        })
                        .collect(),
                ),
                tool_call_id: None,
            },
            ModelMessage::Tool {
                call_id, result, ..
            } => OpenAiMessage {
                role: "tool".to_string(),
                content: Some(OpenAiContent::Text(result.clone())),
                tool_calls: None,
                tool_call_id: Some(call_id.clone()),
            },
        }
    }));
    let base = OpenAiChatRequest {
        model: request.model.name.clone(),
        stream: true,
        messages,
        max_tokens: Some(request.model.max_output_tokens),
        temperature: request.temperature,
        top_p: request.top_p,
        top_k: request.top_k,
        presence_penalty: request.presence_penalty,
        frequency_penalty: request.frequency_penalty,
        seed: request.seed,
        stop_sequences: request.stop_sequences.clone(),
        tools: request
            .tools
            .iter()
            .map(|tool| OpenAiToolSchema {
                schema_type: "function".to_string(),
                function: OpenAiFunctionSchema {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.input_schema.clone(),
                    strict: tool.strict.then_some(true),
                },
            })
            .collect(),
    };
    let mut body = serde_json::to_value(base)?;
    if let Some(extra) = &request.extra_body {
        merge_extra_body(&mut body, extra.clone());
    }
    Ok(body)
}

fn apply_extra_headers(
    headers: &mut HeaderMap,
    extra_headers: &std::collections::BTreeMap<String, String>,
) -> Result<(), LlmError> {
    for (name, value) in extra_headers {
        let header_name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| LlmError::Message(format!("invalid header name `{name}`: {error}")))?;
        let header_value = HeaderValue::from_str(value).map_err(|error| {
            LlmError::Message(format!("invalid header value for `{name}`: {error}"))
        })?;
        headers.insert(header_name, header_value);
    }
    Ok(())
}

fn merge_extra_body(base: &mut Value, extra: Value) {
    match (base, extra) {
        (Value::Object(base_map), Value::Object(extra_map)) => {
            for (key, value) in extra_map {
                base_map.insert(key, value);
            }
        }
        (Value::Object(base_map), value) => {
            base_map.insert("extra_body_json".to_string(), value);
        }
        _ => {}
    }
}

fn parse_finish_reason(value: &str) -> FinishReason {
    match value {
        "tool_calls" => FinishReason::ToolCall,
        "length" => FinishReason::Length,
        "cancelled" => FinishReason::Cancelled,
        "error" => FinishReason::Error,
        _ => FinishReason::Stop,
    }
}

fn to_usage(value: &OpenAiUsage) -> TokenUsage {
    TokenUsage {
        prompt_tokens: value.prompt_tokens,
        completion_tokens: value.completion_tokens,
        total_tokens: value.total_tokens,
        reasoning_tokens: value.reasoning_tokens,
    }
}

fn summarize_stream_chunk(chunk: &str) -> String {
    let compact = chunk.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.len() > 240 {
        clip_text_with_ellipsis(&compact, 243)
    } else {
        compact
    }
}

fn summarize_stream_error(error: &OpenAiErrorPayload) -> String {
    let mut parts = Vec::new();
    if let Some(message) = error
        .message
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        parts.push(message.trim().to_string());
    }
    if let Some(error_type) = error
        .error_type
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        parts.push(format!("type={}", error_type.trim()));
    }
    if let Some(code) = error.code.as_ref() {
        parts.push(format!("code={code}"));
    }
    if parts.is_empty() {
        "provider returned an unspecified stream error".to_string()
    } else {
        parts.join(" | ")
    }
}

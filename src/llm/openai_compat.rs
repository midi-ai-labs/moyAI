use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, RETRY_AFTER};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::config::ProviderMetadataMode;
use crate::error::LlmError;
use crate::llm::dto::{
    OpenAiChatChunk, OpenAiChatRequest, OpenAiContent, OpenAiContentPart, OpenAiErrorPayload,
    OpenAiFunctionSchema, OpenAiImageUrl, OpenAiMessage, OpenAiMessageToolCall,
    OpenAiMessageToolCallFunction, OpenAiToolSchema, OpenAiUsage,
};
use crate::llm::{
    ChatRequest, LlmClient, LlmEvent, LlmEventSink, LlmResponseSummary, ModelMessage,
    ProviderToolChoice, ToolSchema, tool_surface_scoped_parallel_tool_calls_projection,
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
        _request_timeout_ms: u64,
        max_retries: u8,
        api_key: Option<String>,
    ) -> Result<Self, LlmError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(connect_timeout_ms))
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
        let mut stream_retry_attempt = 0_u8;

        'stream_attempt: loop {
            let Some(response) = self.send_request(&request, &cancel).await? else {
                return Ok(LlmResponseSummary {
                    finish_reason: FinishReason::Cancelled,
                    usage: None,
                });
            };

            let mut stream = response.bytes_stream().eventsource();
            let mut usage = None;
            let mut finish_reason = None;
            let mut saw_terminal_signal = false;
            let mut ended_by_eof = false;
            let mut emitted_events = 0_usize;
            let mut tool_calls: HashMap<usize, PartialToolCall> = HashMap::new();

            loop {
                let next_event =
                    if let Some(timeout) = stream_idle_timeout(request.stream_idle_timeout_ms) {
                        tokio::select! {
                            _ = cancel.cancelled() => {
                                return Ok(LlmResponseSummary {
                                    finish_reason: FinishReason::Cancelled,
                                    usage,
                                });
                            }
                            result = tokio::time::timeout(timeout, stream.next()) => {
                                match result {
                                    Ok(event) => event,
                                    Err(_) if should_retry_stream_idle_timeout_before_first_event(
                                        emitted_events,
                                        stream_retry_attempt,
                                        request.stream_max_retries,
                                    ) =>
                                    {
                                        stream_retry_attempt += 1;
                                        if !sleep_retry_delay(
                                            retry_delay_ms(stream_retry_attempt, None),
                                            &cancel,
                                        )
                                        .await
                                        {
                                            return Ok(LlmResponseSummary {
                                                finish_reason: FinishReason::Cancelled,
                                                usage,
                                            });
                                        }
                                        continue 'stream_attempt;
                                    }
                                    Err(_) => {
                                        return Err(stream_idle_timeout_error(
                                            request.stream_idle_timeout_ms,
                                            stream_retry_attempt,
                                            request.stream_max_retries,
                                        ));
                                    }
                                }
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

                let event = match event {
                    Ok(event) => event,
                    Err(error)
                        if emitted_events == 0
                            && should_retry_stream_event_error(&error.to_string())
                            && stream_retry_attempt < request.stream_max_retries =>
                    {
                        stream_retry_attempt += 1;
                        if !sleep_retry_delay(retry_delay_ms(stream_retry_attempt, None), &cancel)
                            .await
                        {
                            return Ok(LlmResponseSummary {
                                finish_reason: FinishReason::Cancelled,
                                usage,
                            });
                        }
                        continue 'stream_attempt;
                    }
                    Err(error) => {
                        return Err(LlmError::Message(format!("SSE stream error: {error}")));
                    }
                };
                if event.data == "[DONE]" {
                    saw_terminal_signal = true;
                    break;
                }

                let chunk =
                    serde_json::from_str::<OpenAiChatChunk>(&event.data).map_err(|error| {
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
                        emitted_events += 1;
                    }
                    if let Some(value) = choice.delta.reasoning {
                        sink.push(LlmEvent::ReasoningDelta(value))?;
                        emitted_events += 1;
                    }
                    if let Some(deltas) = choice.delta.tool_calls {
                        for delta in deltas {
                            let delta_index = delta.index;
                            let entry = tool_calls.entry(delta_index).or_default();
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
                            let call_id = entry.stable_call_id(delta_index);
                            if !entry.started {
                                if let Some(tool_name) = entry.typed_tool_name() {
                                    sink.push(LlmEvent::ToolCallStart {
                                        call_id: call_id.clone(),
                                        tool_name,
                                    })?;
                                    emitted_events += 1;
                                    entry.started = true;
                                }
                            }
                            if entry.started && !entry.arguments.is_empty() {
                                sink.push(LlmEvent::ToolCallArgsDelta {
                                    call_id,
                                    delta: entry.arguments_delta(),
                                })?;
                                emitted_events += 1;
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

            for (delta_index, entry) in tool_calls.iter() {
                if !entry.started && !entry.arguments.is_empty() {
                    return Err(stream_missing_tool_name_error(*delta_index));
                }
            }

            let finish_reason = finish_reason.unwrap_or(FinishReason::Stop);

            sink.push(LlmEvent::Finished {
                finish_reason,
                usage: usage.clone(),
            })?;
            return Ok(LlmResponseSummary {
                finish_reason,
                usage,
            });
        }
    }
}

impl OpenAiCompatClient {
    async fn send_request(
        &self,
        request: &ChatRequest,
        cancel: &CancellationToken,
    ) -> Result<Option<reqwest::Response>, LlmError> {
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

            let request_builder = self
                .client
                .post(format!(
                    "{}/v1/chat/completions",
                    request.base_url.trim_end_matches('/')
                ))
                .headers(headers)
                .json(&to_openai_request(request)?);

            let result = if let Some(timeout) = request_header_timeout(request.timeout_ms) {
                match tokio::select! {
                    _ = cancel.cancelled() => return Ok(None),
                    result = tokio::time::timeout(timeout, request_builder.send()) => result,
                } {
                    Ok(result) => result,
                    Err(_) if attempt < self.max_retries => {
                        attempt += 1;
                        if !sleep_retry_delay(retry_delay_ms(attempt, None), cancel).await {
                            return Ok(None);
                        }
                        continue;
                    }
                    Err(_) => {
                        return Err(LlmError::Message(format!(
                            "provider request timeout after {}ms before response headers",
                            request.timeout_ms
                        )));
                    }
                }
            } else {
                tokio::select! {
                    _ = cancel.cancelled() => return Ok(None),
                    result = request_builder.send() => result,
                }
            };

            match result {
                Ok(response) if response.status().is_success() => return Ok(Some(response)),
                Ok(response) => {
                    let failure = parse_response_failure(response).await?;
                    if failure.retryable && attempt < self.max_retries {
                        attempt += 1;
                        if !sleep_retry_delay(
                            retry_delay_ms(attempt, failure.retry_after_ms),
                            cancel,
                        )
                        .await
                        {
                            return Ok(None);
                        }
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
                    if !sleep_retry_delay(retry_delay_ms(attempt, None), cancel).await {
                        return Ok(None);
                    }
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

fn stream_idle_timeout_error(
    timeout_ms: u64,
    completed_retries: u8,
    stream_max_retries: u8,
) -> LlmError {
    let total_attempts = u16::from(completed_retries) + 1;
    LlmError::Message(format!(
        "provider stream idle timeout after {timeout_ms}ms without any SSE event; stream retries exhausted after {total_attempts} attempt(s) with stream_max_retries={stream_max_retries}"
    ))
}

fn stream_missing_terminal_signal_error() -> LlmError {
    LlmError::Message(
        "openai-compatible stream ended without terminal [DONE] event or finish_reason".to_string(),
    )
}

fn stream_missing_tool_name_error(delta_index: usize) -> LlmError {
    LlmError::Message(format!(
        "openai-compatible stream ended with tool-call arguments but no function.name for delta index {delta_index}"
    ))
}

async fn sleep_retry_delay(delay_ms: u64, cancel: &CancellationToken) -> bool {
    tokio::select! {
        _ = cancel.cancelled() => false,
        _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => true,
    }
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

fn should_retry_stream_event_error(error: &str) -> bool {
    let lowered = error.to_ascii_lowercase();
    lowered.contains("transport error")
        || lowered.contains("error decoding response body")
        || lowered.contains("connection")
        || lowered.contains("timed out")
}

fn request_header_timeout(timeout_ms: u64) -> Option<Duration> {
    if timeout_ms == 0 {
        None
    } else {
        Some(Duration::from_millis(timeout_ms))
    }
}

fn should_retry_stream_idle_timeout_before_first_event(
    emitted_events: usize,
    stream_retry_attempt: u8,
    stream_max_retries: u8,
) -> bool {
    emitted_events == 0 && stream_retry_attempt < stream_max_retries
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
    projected_call_id: Option<String>,
    projected_tool_name: Option<String>,
}

impl PartialToolCall {
    fn stable_call_id(&mut self, delta_index: usize) -> String {
        if self.projected_call_id.is_none() {
            self.projected_call_id = Some(
                self.call_id
                    .clone()
                    .unwrap_or_else(|| format!("tool_call_{delta_index}")),
            );
        }
        self.projected_call_id.clone().unwrap_or_default()
    }

    fn typed_tool_name(&mut self) -> Option<String> {
        if self.projected_tool_name.is_none() {
            if let Some(tool_name) = self.tool_name.clone() {
                self.projected_tool_name = Some(tool_name);
            }
        }
        self.projected_tool_name.clone()
    }

    fn arguments_delta(&mut self) -> String {
        let delta = self.arguments[self.emitted_len..].to_string();
        self.emitted_len = self.arguments.len();
        delta
    }
}

pub fn streaming_tool_call_late_name_preserves_typed_tool_identity_fixture_passes() -> bool {
    let mut call = PartialToolCall::default();
    call.arguments.push_str("{\"path\":\"src/main.rs\"}");
    let early_call_id = call.stable_call_id(0);
    let no_typed_name_before_name_delta = call.typed_tool_name().is_none();
    let buffered_without_emission = call.emitted_len == 0;
    call.tool_name = Some("write".to_string());
    let typed_name = call.typed_tool_name();
    let flushed_delta = call.arguments_delta();

    early_call_id == "tool_call_0"
        && no_typed_name_before_name_delta
        && buffered_without_emission
        && typed_name.as_deref() == Some("write")
        && call.stable_call_id(0) == early_call_id
        && flushed_delta == "{\"path\":\"src/main.rs\"}"
        && stream_missing_tool_name_error(0)
            .to_string()
            .contains("tool-call arguments but no function.name")
}

fn to_openai_request(request: &ChatRequest) -> Result<Value, LlmError> {
    request.validate_provider_lifecycle()?;
    let mut messages = Vec::with_capacity(request.messages.len() + 1);
    let mut system_segments = vec![request.provider_system_prompt()];
    let mut non_system_messages = Vec::with_capacity(request.messages.len());
    for message in &request.messages {
        match message {
            ModelMessage::System { content } => {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    system_segments.push(trimmed.to_string());
                }
            }
            other => non_system_messages.push(other),
        }
    }
    messages.push(OpenAiMessage {
        role: "system".to_string(),
        content: Some(OpenAiContent::Text(system_segments.join("\n\n"))),
        tool_calls: None,
        tool_call_id: None,
    });
    messages.extend(non_system_messages.into_iter().map(|message| {
        match message {
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
            ModelMessage::System { .. } => unreachable!("system messages are merged above"),
        }
    }));
    let base = OpenAiChatRequest {
        model: request.model.name.clone(),
        stream: true,
        messages,
        max_tokens: Some(request.effective_max_output_tokens()),
        temperature: request.temperature,
        top_p: request.top_p,
        top_k: request.top_k,
        presence_penalty: request.presence_penalty,
        frequency_penalty: request.frequency_penalty,
        seed: request.seed,
        stop_sequences: request.stop_sequences.clone(),
        tools: request.tools.iter().map(openai_tool_schema).collect(),
        parallel_tool_calls: tool_surface_scoped_parallel_tool_calls_projection(
            request.tools.len(),
            request.parallel_tool_calls,
        ),
    };
    let mut body = serde_json::to_value(base)?;
    if let Some(extra) = &request.extra_body {
        merge_extra_body(&mut body, extra.clone());
    }
    if let Some(tool_choice) = request
        .tool_choice
        .as_ref()
        .map(|choice| provider_tool_choice_json(choice, request.model.provider_metadata_mode))
        && let Value::Object(base_map) = &mut body
    {
        base_map.insert("tool_choice".to_string(), tool_choice);
    }
    Ok(body)
}

pub(crate) fn provider_tool_choice_json(
    tool_choice: &ProviderToolChoice,
    provider_metadata_mode: ProviderMetadataMode,
) -> Value {
    match tool_choice {
        ProviderToolChoice::Required => serde_json::json!("required"),
        ProviderToolChoice::Named { name } => match provider_metadata_mode {
            ProviderMetadataMode::LmStudioNativeRequired => serde_json::json!("required"),
            ProviderMetadataMode::OpenAiCompatibleOnly => serde_json::json!({
                "type": "function",
                "function": {
                    "name": name
                }
            }),
        },
    }
}

fn openai_tool_schema(tool: &ToolSchema) -> OpenAiToolSchema {
    OpenAiToolSchema {
        schema_type: "function".to_string(),
        function: OpenAiFunctionSchema {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.input_schema.clone(),
            strict: tool.strict.then_some(true),
        },
    }
}

pub(crate) fn openai_tool_schema_json(tool: &ToolSchema) -> Result<Value, LlmError> {
    Ok(serde_json::to_value(openai_tool_schema(tool))?)
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
                if is_runtime_owned_openai_request_key(&key) {
                    continue;
                }
                base_map.insert(key, value);
            }
        }
        (Value::Object(base_map), value) => {
            base_map.insert("extra_body_json".to_string(), value);
        }
        _ => {}
    }
}

fn is_runtime_owned_openai_request_key(key: &str) -> bool {
    matches!(
        key,
        "model"
            | "stream"
            | "messages"
            | "max_tokens"
            | "temperature"
            | "top_p"
            | "top_k"
            | "presence_penalty"
            | "frequency_penalty"
            | "seed"
            | "stop"
            | "tools"
            | "tool_choice"
            | "parallel_tool_calls"
    )
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::Value;

    use super::to_openai_request;
    use crate::config::ProviderMetadataMode;
    use crate::llm::contract::OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY;
    use crate::llm::{
        ChatRequest, ModelCapabilities, ModelMessage, ModelProfile, ProviderToolChoice, ToolSchema,
    };

    #[test]
    fn openai_compatible_only_payload_sends_language_policy_as_system_prompt() {
        let request = ChatRequest {
            model: ModelProfile {
                name: "openai-compatible-fixture-model".to_string(),
                context_window: 131_072,
                max_output_tokens: 8_192,
                provider_metadata_mode: ProviderMetadataMode::OpenAiCompatibleOnly,
                capabilities: ModelCapabilities {
                    supports_tools: true,
                    supports_reasoning: false,
                    supports_images: false,
                },
            },
            base_url: "http://openai-compatible.fixture.invalid".to_string(),
            system_prompt: "Base coding prompt".to_string(),
            messages: Vec::new(),
            tools: Vec::new(),
            tool_choice: None,
            parallel_tool_calls: false,
            timeout_ms: 30_000,
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
        };

        let body = to_openai_request(&request).expect("request serialization succeeds");
        let system_prompt = first_system_prompt(&body);

        assert!(system_prompt.starts_with(OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY));
        assert!(system_prompt.ends_with("\n\nBase coding prompt"));
    }

    #[test]
    fn tool_enabled_payload_uses_configured_output_budget() {
        let mut request = ChatRequest {
            model: ModelProfile {
                name: "openai-compatible-fixture-model".to_string(),
                context_window: 131_072,
                max_output_tokens: 131_072,
                provider_metadata_mode: ProviderMetadataMode::OpenAiCompatibleOnly,
                capabilities: ModelCapabilities {
                    supports_tools: true,
                    supports_reasoning: false,
                    supports_images: false,
                },
            },
            base_url: "http://openai-compatible.fixture.invalid".to_string(),
            system_prompt: "Base coding prompt".to_string(),
            messages: vec![ModelMessage::User {
                content: "Create src/workflow.rs".to_string(),
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
            timeout_ms: 30_000,
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
        };

        let tool_body = to_openai_request(&request).expect("request serialization succeeds");
        request.tools.clear();
        request.tool_choice = None;
        request.parallel_tool_calls = false;
        request.extra_body = None;
        let no_tool_body = to_openai_request(&request).expect("request serialization succeeds");

        assert_eq!(tool_body["max_tokens"].as_u64(), Some(131_072));
        assert_eq!(no_tool_body["max_tokens"].as_u64(), Some(131_072));
    }

    fn first_system_prompt(body: &Value) -> &str {
        body["messages"][0]["content"]
            .as_str()
            .expect("first message content is a text system prompt")
    }
}

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
use crate::config::model::{ProviderApiMode, ProviderReasoningCapability};
use crate::error::LlmError;
use crate::llm::contract::{ReasoningRequest, validate_chat_completions_reasoning_request};
use crate::llm::dto::{
    OpenAiChatChunk, OpenAiChatRequest, OpenAiContent, OpenAiContentPart, OpenAiErrorPayload,
    OpenAiFunctionSchema, OpenAiImageUrl, OpenAiMessage, OpenAiMessageToolCall,
    OpenAiMessageToolCallFunction, OpenAiToolSchema, OpenAiUsage,
};
use crate::llm::responses::{
    ResponsesRequestOptions, ResponsesStreamAccumulator, ResponsesTerminal, to_responses_request,
};
use crate::llm::{
    ChatRequest, LlmClient, LlmEvent, LlmEventSink, LlmResponseSummary, ModelMessage,
    ProviderToolChoice, ToolSchema, tool_surface_scoped_parallel_tool_calls_projection,
};
use crate::session::{FinishReason, TokenUsage};
use crate::tool::truncate::clip_text_with_ellipsis;

const RETRY_INITIAL_DELAY_MS: u64 = 2_000;
const RETRY_BACKOFF_FACTOR: u64 = 2;
const RETRY_MAX_DELAY_MS: u64 = 30_000;

#[derive(Debug, Clone)]
pub struct OpenAiCompatClient {
    client: reqwest::Client,
    max_retries: u8,
    api_key: Option<String>,
}

impl OpenAiCompatClient {
    pub fn new(
        connect_timeout_ms: u64,
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
        match request.provider_api_mode {
            ProviderApiMode::Responses => {
                return self.stream_responses(request, cancel, sink).await;
            }
            ProviderApiMode::ChatCompletions => {}
            ProviderApiMode::Auto => {
                return Err(LlmError::Message(
                    "provider_api_mode must be resolved before transport dispatch".to_string(),
                ));
            }
        }
        let body = to_openai_request(&request)?;

        let Some(response) = self
            .send_request(&request, "v1/chat/completions", &body, &cancel)
            .await?
        else {
            return Ok(LlmResponseSummary {
                finish_reason: FinishReason::Cancelled,
                usage: None,
                response_id: None,
            });
        };

        let mut stream = response.bytes_stream().eventsource();
        let mut usage = None;
        let mut finish_reason = None;
        let mut saw_terminal_signal = false;
        let mut ended_by_eof = false;
        let mut tool_calls: HashMap<usize, PartialToolCall> = HashMap::new();

        loop {
            let next_event =
                if let Some(timeout) = stream_idle_timeout(request.stream_idle_timeout_ms) {
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            return Ok(LlmResponseSummary {
                                finish_reason: FinishReason::Cancelled,
                                usage,
                                response_id: None,
                            });
                        }
                        result = tokio::time::timeout(timeout, stream.next()) => {
                            match result {
                                Ok(event) => event,
                                Err(_) => {
                                    return Err(stream_idle_timeout_error(
                                        request.stream_idle_timeout_ms,
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
                                response_id: None,
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
                Err(error) => {
                    return Err(LlmError::Message(format!("SSE stream error: {error}")));
                }
            };
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
                if let Some(deltas) = choice.delta.tool_calls {
                    for delta in deltas {
                        let delta_index = delta.index;
                        let entry = tool_calls.entry(delta_index).or_default();
                        if let Some(id) = delta.id {
                            entry.record_call_id(id, delta_index)?;
                        }
                        if let Some(function) = delta.function {
                            if let Some(name) = function.name {
                                entry.record_tool_name(name, delta_index)?;
                            }
                            if let Some(arguments) = function.arguments {
                                entry.saw_arguments_field = true;
                                entry.arguments.push_str(&arguments);
                            }
                        }
                        if !entry.started {
                            if let Some((call_id, tool_name)) = entry.identity() {
                                sink.push(LlmEvent::ToolCallStart { call_id, tool_name })?;
                                entry.started = true;
                            }
                        }
                        if entry.started && entry.emitted_len < entry.arguments.len() {
                            sink.push(LlmEvent::ToolCallArgsDelta {
                                call_id: entry.call_id.clone().unwrap_or_default(),
                                delta: entry.arguments_delta(),
                            })?;
                        }
                    }
                }
                if let Some(value) = choice.finish_reason {
                    saw_terminal_signal = true;
                    finish_reason = Some(parse_finish_reason(&value)?);
                }
            }
        }

        if ended_by_eof && !saw_terminal_signal {
            return Err(stream_missing_terminal_signal_error());
        }

        let has_complete_tool_calls = validate_streamed_tool_calls(&tool_calls)?;
        let finish_reason = resolve_finish_reason(finish_reason, has_complete_tool_calls)?;

        sink.push(LlmEvent::Finished {
            finish_reason,
            usage: usage.clone(),
        })?;
        Ok(LlmResponseSummary {
            finish_reason,
            usage,
            response_id: None,
        })
    }
}

impl OpenAiCompatClient {
    async fn stream_responses(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
        sink: &mut dyn LlmEventSink,
    ) -> Result<LlmResponseSummary, LlmError> {
        let body = to_responses_request(&request, ResponsesRequestOptions::from_request(&request))?;
        let Some(response) = self
            .send_request(&request, "v1/responses", &body, &cancel)
            .await?
        else {
            return Ok(LlmResponseSummary {
                finish_reason: FinishReason::Cancelled,
                usage: None,
                response_id: None,
            });
        };

        let mut stream = response.bytes_stream().eventsource();
        let mut accumulator = ResponsesStreamAccumulator::default();

        loop {
            let next_event =
                if let Some(timeout) = stream_idle_timeout(request.stream_idle_timeout_ms) {
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            return Ok(LlmResponseSummary {
                                finish_reason: FinishReason::Cancelled,
                                usage: None,
                                response_id: None,
                            });
                        }
                        result = tokio::time::timeout(timeout, stream.next()) => {
                            match result {
                                Ok(event) => event,
                                Err(_) => {
                                    return Err(stream_idle_timeout_error(
                                        request.stream_idle_timeout_ms,
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
                                usage: None,
                                response_id: None,
                            });
                        }
                        result = stream.next() => result,
                    }
                };

            let Some(event) = next_event else {
                return Err(LlmError::Message(
                    "Responses stream closed before response.completed".to_string(),
                ));
            };
            let event = match event {
                Ok(event) => event,
                Err(error) => {
                    return Err(LlmError::Message(format!(
                        "Responses SSE stream error: {error}"
                    )));
                }
            };

            if event.data == "[DONE]" {
                return Err(LlmError::Message(
                    "Responses stream ended with [DONE] before response.completed".to_string(),
                ));
            }

            let update = accumulator.push_json(&event.data).map_err(|error| {
                LlmError::Message(format!(
                    "failed to parse Responses stream event: {error}. Raw event: {}",
                    summarize_stream_chunk(&event.data)
                ))
            })?;
            match update.terminal {
                Some(ResponsesTerminal::Completed {
                    response_id,
                    finish_reason,
                    usage,
                }) => {
                    for event in update.events {
                        sink.push(event)?;
                    }
                    return Ok(LlmResponseSummary {
                        finish_reason,
                        usage,
                        response_id: Some(response_id),
                    });
                }
                Some(ResponsesTerminal::Failed { code, message, .. }) => {
                    return Err(LlmError::ProviderRejected {
                        status: None,
                        code,
                        param: None,
                        message,
                    });
                }
                Some(ResponsesTerminal::Incomplete { reason, usage, .. }) => {
                    return Err(LlmError::IncompleteResponse { reason, usage });
                }
                None => {
                    for event in update.events {
                        sink.push(event)?;
                    }
                }
            }
        }
    }

    async fn send_request(
        &self,
        request: &ChatRequest,
        endpoint_path: &str,
        body: &Value,
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
                    "{}/{}",
                    request.base_url.trim_end_matches('/'),
                    endpoint_path.trim_start_matches('/')
                ))
                .headers(headers)
                .json(body);

            let result = if let Some(timeout) = request_header_timeout(request.timeout_ms) {
                match tokio::select! {
                    _ = cancel.cancelled() => return Ok(None),
                    result = tokio::time::timeout(timeout, request_builder.send()) => result,
                } {
                    Ok(result) => result,
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
                    let Some(failure) = parse_response_failure_until_cancelled(
                        response,
                        cancel,
                        request.timeout_ms,
                    )
                    .await?
                    else {
                        return Ok(None);
                    };
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
                    return Err(LlmError::ProviderRejected {
                        status: Some(failure.status.as_u16()),
                        code: failure.code,
                        param: failure.param,
                        message: failure.message,
                    });
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
    code: Option<String>,
    param: Option<String>,
    message: String,
    retryable: bool,
    retry_after_ms: Option<u64>,
}

async fn parse_response_failure(response: reqwest::Response) -> Result<ResponseFailure, LlmError> {
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.text().await.unwrap_or_default();
    let parsed = serde_json::from_str::<Value>(&body).ok();
    let error = parsed.as_ref().and_then(|value| value.get("error"));
    let code = error
        .and_then(|value| value.get("code").or_else(|| value.get("type")))
        .and_then(Value::as_str)
        .map(str::to_string);
    let param = error
        .and_then(|value| value.get("param"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let message = error
        .and_then(|value| value.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| summarize_failure_body(&body));
    Ok(ResponseFailure {
        status,
        code,
        param,
        message,
        retryable: is_retryable_status(status),
        retry_after_ms: retry_after_ms(&headers),
    })
}

async fn parse_response_failure_until_cancelled(
    response: reqwest::Response,
    cancel: &CancellationToken,
    timeout_ms: u64,
) -> Result<Option<ResponseFailure>, LlmError> {
    let parse = parse_response_failure(response);
    tokio::pin!(parse);
    if let Some(timeout) = request_header_timeout(timeout_ms) {
        tokio::select! {
            _ = cancel.cancelled() => Ok(None),
            result = tokio::time::timeout(timeout, &mut parse) => match result {
                Ok(result) => result.map(Some),
                Err(_) => Err(LlmError::Message(format!(
                    "provider error response body timeout after {timeout_ms}ms"
                ))),
            },
        }
    } else {
        tokio::select! {
            _ = cancel.cancelled() => Ok(None),
            result = &mut parse => result.map(Some),
        }
    }
}

fn stream_idle_timeout(timeout_ms: u64) -> Option<Duration> {
    (timeout_ms > 0).then(|| Duration::from_millis(timeout_ms))
}

fn stream_idle_timeout_error(timeout_ms: u64) -> LlmError {
    LlmError::Message(format!(
        "provider stream idle timeout after {timeout_ms}ms without an SSE event"
    ))
}

fn stream_missing_terminal_signal_error() -> LlmError {
    LlmError::Message(
        "openai-compatible stream ended without terminal [DONE] event or finish_reason".to_string(),
    )
}

fn resolve_finish_reason(
    finish_reason: Option<FinishReason>,
    has_tool_calls: bool,
) -> Result<FinishReason, LlmError> {
    match (finish_reason, has_tool_calls) {
        (None, _) => Err(LlmError::Message(
            "openai-compatible stream ended without a finish_reason".to_string(),
        )),
        (Some(FinishReason::ToolCall), false) => Err(LlmError::Message(
            "openai-compatible stream ended with finish_reason=tool_calls but no complete tool call"
                .to_string(),
        )),
        (Some(FinishReason::ToolCall), true) => Ok(FinishReason::ToolCall),
        (Some(finish_reason), true) => Err(LlmError::Message(format!(
            "openai-compatible stream ended with finish_reason={finish_reason:?} and a tool-call payload"
        ))),
        (Some(finish_reason), _) => Ok(finish_reason),
    }
}

fn validate_streamed_tool_calls(
    tool_calls: &HashMap<usize, PartialToolCall>,
) -> Result<bool, LlmError> {
    if tool_calls.is_empty() {
        return Ok(false);
    }
    for (delta_index, entry) in tool_calls {
        if entry
            .call_id
            .as_deref()
            .is_none_or(|call_id| call_id.trim().is_empty())
        {
            return Err(LlmError::Message(format!(
                "openai-compatible stream ended with a tool call that has no call id at delta index {delta_index}"
            )));
        }
        let Some(tool_name) = entry
            .tool_name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
        else {
            return Err(stream_missing_tool_name_error(*delta_index));
        };
        if !entry.saw_arguments_field {
            return Err(LlmError::Message(format!(
                "openai-compatible stream ended without an arguments field for tool `{tool_name}` at delta index {delta_index}"
            )));
        }
    }
    Ok(true)
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

fn is_retryable_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn should_retry_transport_error(error: &reqwest::Error) -> bool {
    error.is_connect()
}

fn request_header_timeout(timeout_ms: u64) -> Option<Duration> {
    if timeout_ms == 0 {
        None
    } else {
        Some(Duration::from_millis(timeout_ms))
    }
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
        return Some(parsed_seconds.saturating_mul(1_000).min(RETRY_MAX_DELAY_MS));
    }
    let retry_at = httpdate::parse_http_date(value).ok()?;
    let delta = retry_at
        .duration_since(std::time::SystemTime::now())
        .ok()?
        .as_millis()
        .min(u128::from(RETRY_MAX_DELAY_MS)) as u64;
    Some(delta)
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
    (RETRY_INITIAL_DELAY_MS.saturating_mul(pow)).min(RETRY_MAX_DELAY_MS)
}

#[derive(Default)]
struct PartialToolCall {
    call_id: Option<String>,
    tool_name: Option<String>,
    arguments: String,
    saw_arguments_field: bool,
    emitted_len: usize,
    started: bool,
}

impl PartialToolCall {
    fn record_call_id(&mut self, call_id: String, delta_index: usize) -> Result<(), LlmError> {
        if self
            .call_id
            .as_ref()
            .is_some_and(|existing| existing != &call_id)
        {
            return Err(LlmError::Message(format!(
                "openai-compatible stream changed the call id for tool delta index {delta_index}"
            )));
        }
        self.call_id = Some(call_id);
        Ok(())
    }

    fn record_tool_name(&mut self, tool_name: String, delta_index: usize) -> Result<(), LlmError> {
        if self
            .tool_name
            .as_ref()
            .is_some_and(|existing| existing != &tool_name)
        {
            return Err(LlmError::Message(format!(
                "openai-compatible stream changed the tool name for delta index {delta_index}"
            )));
        }
        self.tool_name = Some(tool_name);
        Ok(())
    }

    fn identity(&self) -> Option<(String, String)> {
        let call_id = self.call_id.as_deref()?.trim();
        let tool_name = self.tool_name.as_deref()?.trim();
        if call_id.is_empty() || tool_name.is_empty() {
            return None;
        }
        Some((call_id.to_string(), tool_name.to_string()))
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
    call.saw_arguments_field = true;
    let no_identity_before_provider_id = call.identity().is_none();
    call.record_call_id("provider_call_0".to_string(), 0)
        .expect("first provider call id");
    let no_identity_before_name_delta = call.identity().is_none();
    let buffered_without_emission = call.emitted_len == 0;
    call.record_tool_name("write".to_string(), 0)
        .expect("first provider tool name");
    let identity = call.identity();
    let flushed_delta = call.arguments_delta();

    no_identity_before_provider_id
        && no_identity_before_name_delta
        && buffered_without_emission
        && identity == Some(("provider_call_0".to_string(), "write".to_string()))
        && flushed_delta == "{\"path\":\"src/main.rs\"}"
        && stream_missing_tool_name_error(0)
            .to_string()
            .contains("tool-call arguments but no function.name")
}

fn to_openai_request(request: &ChatRequest) -> Result<Value, LlmError> {
    if request.provider_api_mode != ProviderApiMode::ChatCompletions {
        return Err(LlmError::Message(
            "Chat Completions request serialization requires provider_api_mode=chat_completions"
                .to_string(),
        ));
    }
    to_openai_request_with_reasoning(
        request,
        request.reasoning.as_ref(),
        request.reasoning_capability,
    )
}

pub(crate) fn to_openai_request_with_reasoning(
    request: &ChatRequest,
    reasoning_request: Option<&ReasoningRequest>,
    reasoning_capability: ProviderReasoningCapability,
) -> Result<Value, LlmError> {
    request.validate_provider_lifecycle()?;
    let reasoning =
        validate_chat_completions_reasoning_request(reasoning_request, reasoning_capability)?;
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
    if let Some(reasoning) = reasoning {
        let body_map = body.as_object_mut().ok_or_else(|| {
            LlmError::Message(
                "OpenAI-compatible Chat Completions request must serialize as an object"
                    .to_string(),
            )
        })?;
        if let Some(effort) = reasoning.effort {
            body_map.insert(
                "reasoning_effort".to_string(),
                serde_json::to_value(effort)?,
            );
        }
        if let Some(summary) = reasoning.summary {
            body_map.insert(
                "reasoning_summary".to_string(),
                serde_json::to_value(summary)?,
            );
        }
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
            | "reasoning_effort"
            | "reasoning_summary"
    )
}

fn parse_finish_reason(value: &str) -> Result<FinishReason, LlmError> {
    match value {
        "stop" => Ok(FinishReason::Stop),
        "tool_calls" | "function_call" => Ok(FinishReason::ToolCall),
        "length" => Ok(FinishReason::Length),
        "cancelled" | "canceled" => Ok(FinishReason::Cancelled),
        "error" | "content_filter" => Ok(FinishReason::Error),
        unknown => Err(LlmError::Message(format!(
            "openai-compatible provider returned unknown finish_reason `{unknown}`"
        ))),
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
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use axum::body::{Body, Bytes};
    use axum::extract::State;
    use axum::http::header::CONTENT_TYPE;
    use axum::response::Response;
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::{Value, json};
    use tokio_util::sync::CancellationToken;

    use super::{
        OpenAiCompatClient, PartialToolCall, is_retryable_status, parse_finish_reason,
        resolve_finish_reason, retry_delay_ms, to_openai_request, to_openai_request_with_reasoning,
        validate_streamed_tool_calls,
    };
    use crate::config::ProviderMetadataMode;
    use crate::config::model::{
        ChatCompletionsReasoningParameters, ProviderApiMode, ProviderReasoningCapability,
        ReasoningEffort, ReasoningSummary,
    };
    use crate::llm::contract::{OPENAI_COMPATIBLE_ONLY_SYSTEM_PROMPT_POLICY, ReasoningRequest};
    use crate::llm::{
        ChatRequest, LlmClient, LlmEvent, LlmEventSink, ModelCapabilities, ModelMessage,
        ModelProfile, ModelToolCall, ProviderToolChoice, ResponsesContinuation, ToolSchema,
    };
    use crate::session::{FinishReason, TokenUsage};

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
            provider_api_mode: ProviderApiMode::ChatCompletions,
            reasoning: None,
            reasoning_capability: ProviderReasoningCapability::Unsupported,
            responses_continuation: None,
            tool_choice: None,
            parallel_tool_calls: false,
            timeout_ms: 30_000,
            stream_idle_timeout_ms: 300_000,
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
            provider_api_mode: ProviderApiMode::ChatCompletions,
            reasoning: None,
            reasoning_capability: ProviderReasoningCapability::Unsupported,
            responses_continuation: None,
            tool_choice: Some(ProviderToolChoice::Required),
            parallel_tool_calls: true,
            timeout_ms: 30_000,
            stream_idle_timeout_ms: 300_000,
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

    #[test]
    fn reasoning_fields_are_omitted_without_an_explicit_typed_provider_contract() {
        let request = reasoning_fixture_request();

        let body = to_openai_request(&request).expect("request serialization succeeds");

        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("reasoning_summary").is_none());
    }

    #[test]
    fn typed_chat_completions_reasoning_serializes_verified_wire_fields() {
        let request = reasoning_fixture_request();
        let effort_only = ReasoningRequest {
            effort: Some(ReasoningEffort::Medium),
            summary: ReasoningSummary::None,
        };

        let effort_only_body = to_openai_request_with_reasoning(
            &request,
            Some(&effort_only),
            ProviderReasoningCapability::ChatCompletions {
                parameters: ChatCompletionsReasoningParameters::EffortOnly,
            },
        )
        .expect("provider-verified effort field");
        assert_eq!(effort_only_body["reasoning_effort"], "medium");
        assert!(effort_only_body.get("reasoning_summary").is_none());

        let effort_and_summary = ReasoningRequest {
            effort: Some(ReasoningEffort::High),
            summary: ReasoningSummary::Concise,
        };
        let effort_and_summary_body = to_openai_request_with_reasoning(
            &request,
            Some(&effort_and_summary),
            ProviderReasoningCapability::ChatCompletions {
                parameters: ChatCompletionsReasoningParameters::EffortAndSummary,
            },
        )
        .expect("provider-verified effort and summary fields");
        assert_eq!(effort_and_summary_body["reasoning_effort"], "high");
        assert_eq!(effort_and_summary_body["reasoning_summary"], "concise");
    }

    #[test]
    fn enabled_reasoning_fails_closed_for_unsupported_or_mismatched_transports() {
        let request = reasoning_fixture_request();
        let reasoning = ReasoningRequest {
            effort: Some(ReasoningEffort::Low),
            summary: ReasoningSummary::None,
        };

        assert!(
            to_openai_request_with_reasoning(
                &request,
                Some(&reasoning),
                ProviderReasoningCapability::Unsupported,
            )
            .is_err()
        );
        assert!(
            to_openai_request_with_reasoning(
                &request,
                Some(&reasoning),
                ProviderReasoningCapability::Responses {
                    supports_summary: true,
                    supports_previous_response_id: true,
                },
            )
            .is_err()
        );
    }

    #[test]
    fn extra_body_cannot_own_or_override_reasoning_wire_fields() {
        let mut request = reasoning_fixture_request();
        request.extra_body = Some(serde_json::json!({
            "reasoning_effort": "ultra",
            "reasoning_summary": "detailed",
            "num_ctx": 8192
        }));

        let disabled_body = to_openai_request(&request).expect("disabled reasoning payload");
        assert!(disabled_body.get("reasoning_effort").is_none());
        assert!(disabled_body.get("reasoning_summary").is_none());
        assert_eq!(disabled_body["num_ctx"], 8192);

        let typed_body = to_openai_request_with_reasoning(
            &request,
            Some(&ReasoningRequest {
                effort: Some(ReasoningEffort::Medium),
                summary: ReasoningSummary::None,
            }),
            ProviderReasoningCapability::ChatCompletions {
                parameters: ChatCompletionsReasoningParameters::EffortOnly,
            },
        )
        .expect("typed reasoning owns wire field");
        assert_eq!(typed_body["reasoning_effort"], "medium");
        assert!(typed_body.get("reasoning_summary").is_none());
        assert_eq!(typed_body["num_ctx"], 8192);
    }

    #[test]
    fn finish_reason_parser_is_typed_and_unknown_values_fail_closed() {
        assert_eq!(
            parse_finish_reason("stop").expect("stop"),
            crate::session::FinishReason::Stop
        );
        assert_eq!(
            parse_finish_reason("tool_calls").expect("tools"),
            crate::session::FinishReason::ToolCall
        );
        assert_eq!(
            parse_finish_reason("content_filter").expect("provider error"),
            crate::session::FinishReason::Error
        );
        assert!(parse_finish_reason("provider_specific_success").is_err());
        assert!(resolve_finish_reason(None, true).is_err());
        assert!(resolve_finish_reason(None, false).is_err());
        assert_eq!(
            resolve_finish_reason(Some(FinishReason::ToolCall), true)
                .expect("typed tool-call terminal"),
            crate::session::FinishReason::ToolCall
        );
        assert_eq!(
            resolve_finish_reason(Some(FinishReason::Stop), false).expect("typed stop terminal"),
            crate::session::FinishReason::Stop
        );
        assert!(resolve_finish_reason(Some(FinishReason::ToolCall), false).is_err());
        assert!(resolve_finish_reason(Some(FinishReason::Stop), true).is_err());
        assert!(resolve_finish_reason(Some(FinishReason::Length), true).is_err());
    }

    #[test]
    fn tool_calls_require_provider_identity_and_preserve_raw_arguments() {
        let complete = std::collections::HashMap::from([(
            0,
            PartialToolCall {
                call_id: Some("call_0".to_string()),
                tool_name: Some("read".to_string()),
                arguments: "{\"path\":".to_string(),
                saw_arguments_field: true,
                ..PartialToolCall::default()
            },
        )]);
        assert!(
            validate_streamed_tool_calls(&complete)
                .expect("transport preserves malformed provider arguments for runtime parsing")
        );
        assert_eq!(
            resolve_finish_reason(
                Some(FinishReason::ToolCall),
                validate_streamed_tool_calls(&complete).expect("complete call")
            )
            .expect("typed terminal"),
            crate::session::FinishReason::ToolCall
        );

        for partial in [
            PartialToolCall {
                call_id: Some("id_only".to_string()),
                saw_arguments_field: true,
                ..PartialToolCall::default()
            },
            PartialToolCall {
                tool_name: Some("read".to_string()),
                saw_arguments_field: true,
                ..PartialToolCall::default()
            },
            PartialToolCall {
                call_id: Some("call_without_arguments".to_string()),
                tool_name: Some("read".to_string()),
                ..PartialToolCall::default()
            },
        ] {
            let calls = std::collections::HashMap::from([(0, partial)]);
            assert!(validate_streamed_tool_calls(&calls).is_err());
        }
    }

    #[test]
    fn retry_classification_is_limited_to_rate_limits_and_server_failures() {
        assert!(is_retryable_status(reqwest::StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        ));
        assert!(!is_retryable_status(reqwest::StatusCode::REQUEST_TIMEOUT));
        assert!(!is_retryable_status(reqwest::StatusCode::BAD_REQUEST));
        assert_eq!(retry_delay_ms(1, Some(u64::MAX)), 30_000);
        assert_eq!(retry_delay_ms(u8::MAX, None), 30_000);
    }

    #[tokio::test]
    async fn chat_completions_raw_reasoning_fields_are_not_client_projection() {
        let response = [
            format!(
                "data: {}\n\n",
                json!({
                    "choices": [{
                        "index": 0,
                        "delta": {"reasoning_content": "raw provider trace"},
                        "finish_reason": null
                    }]
                })
            ),
            format!(
                "data: {}\n\n",
                json!({
                    "choices": [{
                        "index": 0,
                        "delta": {"content": "visible answer"},
                        "finish_reason": "stop"
                    }]
                })
            ),
            "data: [DONE]\n\n".to_string(),
        ]
        .concat();
        let (base_url, _requests, server) = start_responses_fixture(vec![response]).await;
        let mut request = reasoning_fixture_request();
        request.base_url = base_url;
        let client = OpenAiCompatClient::new(1_000, 0, None).expect("fixture client");
        let mut sink = RecordingLlmEventSink::default();

        let summary = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect("completed Chat Completions stream");
        server.abort();

        assert_eq!(summary.finish_reason, FinishReason::Stop);
        assert!(matches!(
            sink.events.as_slice(),
            [
                LlmEvent::TextDelta(text),
                LlmEvent::Finished {
                    finish_reason: FinishReason::Stop,
                    usage: None,
                },
            ] if text == "visible answer"
        ));
    }

    #[tokio::test]
    async fn responses_transport_posts_typed_wire_and_projects_completed_text_and_summary() {
        let response = responses_sse([
            json!({
                "type": "response.reasoning_summary_text.delta",
                "delta": "Inspected the repository"
            }),
            json!({
                "type": "response.output_text.delta",
                "item_id": "msg_1",
                "delta": "The change is ready."
            }),
            json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_text_1",
                    "usage": {
                        "input_tokens": 12,
                        "output_tokens": 7,
                        "total_tokens": 19,
                        "output_tokens_details": { "reasoning_tokens": 4 }
                    }
                }
            }),
        ]);
        let (base_url, requests, server) = start_responses_fixture(vec![response]).await;
        let mut request = responses_fixture_request(
            &base_url,
            vec![
                ModelMessage::System {
                    content: "Repository policy".to_string(),
                },
                ModelMessage::User {
                    content: "Inspect the repository".to_string(),
                },
            ],
        );
        request.reasoning = Some(ReasoningRequest {
            effort: Some(ReasoningEffort::High),
            summary: ReasoningSummary::Detailed,
        });
        let client = OpenAiCompatClient::new(1_000, 0, None).expect("fixture client");
        let mut sink = RecordingLlmEventSink::default();

        let summary = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect("completed Responses stream");
        server.abort();

        assert_eq!(summary.finish_reason, FinishReason::Stop);
        assert_eq!(summary.response_id.as_deref(), Some("resp_text_1"));
        assert!(matches!(
            summary.usage.as_ref(),
            Some(TokenUsage {
                prompt_tokens: 12,
                completion_tokens: 7,
                total_tokens: 19,
                reasoning_tokens: Some(4),
            })
        ));
        assert!(matches!(
            sink.events.as_slice(),
            [
                LlmEvent::ReasoningSummaryDelta(reasoning),
                LlmEvent::TextDelta(text),
                LlmEvent::Finished {
                    finish_reason: FinishReason::Stop,
                    usage: Some(TokenUsage {
                        prompt_tokens: 12,
                        completion_tokens: 7,
                        total_tokens: 19,
                        reasoning_tokens: Some(4),
                    }),
                },
            ] if reasoning == "Inspected the repository" && text == "The change is ready."
        ));

        let captured = requests.lock().expect("Responses request capture");
        assert_eq!(captured.len(), 1);
        let wire = &captured[0];
        assert_eq!(wire["model"], json!("responses-fixture-model"));
        assert_eq!(
            wire["instructions"],
            json!("Responses fixture instructions\n\nRepository policy")
        );
        assert_eq!(
            wire["input"],
            json!([{
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "Inspect the repository"
                }]
            }])
        );
        assert_eq!(
            wire["reasoning"],
            json!({ "effort": "high", "summary": "detailed" })
        );
        assert_eq!(wire["max_output_tokens"], json!(4_096));
        assert_eq!(wire["store"], json!(true));
        assert_eq!(wire["stream"], json!(true));
        assert!(wire.get("messages").is_none());
        assert!(wire.get("previous_response_id").is_none());
    }

    #[tokio::test]
    async fn responses_transport_reuses_response_id_and_sends_only_incremental_tool_output() {
        let first_response = responses_sse([
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "{\"path\":\"README.md\"}"
                }
            }),
            json!({
                "type": "response.completed",
                "response": { "id": "resp_tool_1" }
            }),
        ]);
        let second_response = responses_sse([
            json!({
                "type": "response.output_text.delta",
                "item_id": "msg_2",
                "delta": "README inspected."
            }),
            json!({
                "type": "response.completed",
                "response": { "id": "resp_text_2" }
            }),
        ]);
        let (base_url, requests, server) =
            start_responses_fixture(vec![first_response, second_response]).await;
        let mut first_request = responses_fixture_request(
            &base_url,
            vec![ModelMessage::User {
                content: "Inspect README.md".to_string(),
            }],
        );
        first_request.tools = vec![ToolSchema {
            name: "read_file".to_string(),
            description: "Read a file".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
            strict: true,
        }];
        first_request.tool_choice = Some(ProviderToolChoice::Required);
        first_request.extra_body = Some(json!({ "num_ctx": 131_072 }));
        let client = OpenAiCompatClient::new(1_000, 0, None).expect("fixture client");
        let mut first_sink = RecordingLlmEventSink::default();

        let first_summary = client
            .stream_chat(
                first_request.clone(),
                CancellationToken::new(),
                &mut first_sink,
            )
            .await
            .expect("function-call Responses stream");
        assert_eq!(first_summary.finish_reason, FinishReason::ToolCall);
        assert_eq!(first_summary.response_id.as_deref(), Some("resp_tool_1"));
        assert!(matches!(
            first_sink.events.as_slice(),
            [
                LlmEvent::ToolCallStart { call_id, tool_name },
                LlmEvent::ToolCallArgsDelta {
                    call_id: arguments_call_id,
                    delta,
                },
                LlmEvent::Finished {
                    finish_reason: FinishReason::ToolCall,
                    ..
                },
            ] if call_id == "call_1"
                && tool_name == "read_file"
                && arguments_call_id == "call_1"
                && delta == "{\"path\":\"README.md\"}"
        ));

        let mut second_request = first_request;
        second_request.messages = vec![
            ModelMessage::User {
                content: "Inspect README.md".to_string(),
            },
            ModelMessage::AssistantToolCalls {
                content: None,
                tool_calls: vec![ModelToolCall {
                    call_id: "call_1".to_string(),
                    tool_name: "read_file".to_string(),
                    arguments_json: "{\"path\":\"README.md\"}".to_string(),
                }],
            },
            ModelMessage::Tool {
                call_id: "call_1".to_string(),
                tool_name: "read_file".to_string(),
                result: "README contents".to_string(),
                metadata: Value::Null,
            },
        ];
        second_request.responses_continuation = Some(ResponsesContinuation {
            previous_response_id: first_summary
                .response_id
                .expect("first response id for continuation"),
            input_start: 2,
        });
        let mut second_sink = RecordingLlmEventSink::default();

        let second_summary = client
            .stream_chat(second_request, CancellationToken::new(), &mut second_sink)
            .await
            .expect("continued Responses stream");
        server.abort();

        assert_eq!(second_summary.finish_reason, FinishReason::Stop);
        assert_eq!(second_summary.response_id.as_deref(), Some("resp_text_2"));
        assert!(matches!(
            second_sink.events.as_slice(),
            [
                LlmEvent::TextDelta(text),
                LlmEvent::Finished {
                    finish_reason: FinishReason::Stop,
                    ..
                },
            ] if text == "README inspected."
        ));

        let captured = requests.lock().expect("Responses request capture");
        assert_eq!(captured.len(), 2);
        assert!(captured[0].get("previous_response_id").is_none());
        assert_eq!(captured[0]["num_ctx"], json!(131_072));
        assert_eq!(
            captured[0]["input"],
            json!([{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "Inspect README.md" }]
            }])
        );
        assert_eq!(captured[1]["previous_response_id"], json!("resp_tool_1"));
        assert_eq!(captured[1]["num_ctx"], json!(131_072));
        assert_eq!(
            captured[1]["input"],
            json!([{
                "type": "function_call_output",
                "call_id": "call_1",
                "output": "README contents"
            }])
        );
    }

    #[tokio::test]
    async fn responses_transport_does_not_retry_failure_after_response_started() {
        let failed = responses_sse([json!({
            "type": "response.failed",
            "response": {
                "id": "resp_retryable",
                "error": { "code": "server_error", "message": "try again" }
            }
        })]);
        let completed = responses_sse([
            json!({
                "type": "response.output_text.delta",
                "item_id": "msg_after_retry",
                "delta": "Recovered."
            }),
            json!({
                "type": "response.completed",
                "response": { "id": "resp_recovered" }
            }),
        ]);
        let (base_url, requests, server) = start_responses_fixture(vec![failed, completed]).await;
        let request = responses_fixture_request(
            &base_url,
            vec![ModelMessage::User {
                content: "Retry transient failure".to_string(),
            }],
        );
        let client = OpenAiCompatClient::new(1_000, 1, None).expect("fixture client");
        let mut sink = RecordingLlmEventSink::default();

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("a typed stream failure must not re-post the request");
        server.abort();

        assert!(error.to_string().contains("try again"));
        assert_eq!(requests.lock().expect("request capture").len(), 1);
        assert!(sink.events.is_empty());
    }

    #[tokio::test]
    async fn request_header_timeout_is_not_retried() {
        let response = responses_sse([json!({
            "type": "response.completed",
            "response": { "id": "resp_too_late" }
        })]);
        let (base_url, request_count, server) =
            start_delayed_fixture(response, Duration::from_millis(200), true).await;
        let mut request = responses_fixture_request(
            &base_url,
            vec![ModelMessage::User {
                content: "Do not retry a timed out generation".to_string(),
            }],
        );
        request.timeout_ms = 30;
        request.stream_idle_timeout_ms = 1_000;
        let client = OpenAiCompatClient::new(1_000, 2, None).expect("fixture client");
        let mut sink = RecordingLlmEventSink::default();

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("header timeout must terminate the generation request");
        server.abort();

        assert!(error.to_string().contains("before response headers"));
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        assert!(sink.events.is_empty());
    }

    #[tokio::test]
    async fn stream_idle_timeout_is_not_retried() {
        let response = responses_sse([json!({
            "type": "response.completed",
            "response": { "id": "resp_too_late" }
        })]);
        let (base_url, request_count, server) =
            start_delayed_fixture(response, Duration::from_millis(200), false).await;
        let mut request = responses_fixture_request(
            &base_url,
            vec![ModelMessage::User {
                content: "Do not retry an idle generation stream".to_string(),
            }],
        );
        request.timeout_ms = 1_000;
        request.stream_idle_timeout_ms = 30;
        let client = OpenAiCompatClient::new(1_000, 2, None).expect("fixture client");
        let mut sink = RecordingLlmEventSink::default();

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("stream idle timeout must terminate the generation request");
        server.abort();

        assert!(error.to_string().contains("stream idle timeout"));
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        assert!(sink.events.is_empty());
    }

    #[tokio::test]
    async fn responses_transport_rejects_failed_incomplete_and_terminal_less_streams() {
        let failed = responses_sse([json!({
            "type": "response.failed",
            "response": {
                "id": "resp_failed",
                "error": { "code": "server_error", "message": "unavailable" }
            }
        })]);
        let incomplete = responses_sse([json!({
            "type": "response.incomplete",
            "response": {
                "id": "resp_incomplete",
                "incomplete_details": { "reason": "max_output_tokens" },
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 20,
                    "total_tokens": 30
                }
            }
        })]);
        let terminal_less = responses_sse([json!({
            "type": "response.output_text.delta",
            "item_id": "msg_unfinished",
            "delta": "partial output"
        })]);
        let (base_url, _requests, server) =
            start_responses_fixture(vec![failed, incomplete, terminal_less]).await;
        let request = responses_fixture_request(
            &base_url,
            vec![ModelMessage::User {
                content: "Run the failure fixture".to_string(),
            }],
        );
        let client = OpenAiCompatClient::new(1_000, 0, None).expect("fixture client");

        for (index, expected) in [
            "provider rejected the request (server_error): unavailable",
            "provider returned an incomplete response: max_output_tokens",
            "Responses stream closed before response.completed",
        ]
        .into_iter()
        .enumerate()
        {
            let mut sink = RecordingLlmEventSink::default();
            let error = client
                .stream_chat(request.clone(), CancellationToken::new(), &mut sink)
                .await
                .expect_err("non-completed Responses stream must fail");
            assert!(
                error.to_string().contains(expected),
                "unexpected Responses error: {error}"
            );
            if index == 1 {
                assert!(matches!(
                    error.token_usage(),
                    Some(TokenUsage {
                        prompt_tokens: 10,
                        completion_tokens: 20,
                        total_tokens: 30,
                        reasoning_tokens: None,
                    })
                ));
            } else {
                assert!(error.token_usage().is_none());
            }
            assert!(
                !sink
                    .events
                    .iter()
                    .any(|event| matches!(event, LlmEvent::Finished { .. })),
                "failed Responses streams must not emit Finished: {:?}",
                sink.events
            );
        }
        server.abort();
    }

    #[derive(Clone)]
    struct ResponsesFixtureState {
        requests: Arc<Mutex<Vec<Value>>>,
        responses: Arc<Vec<String>>,
        next_response: Arc<AtomicUsize>,
    }

    #[derive(Clone)]
    struct DelayedFixtureState {
        request_count: Arc<AtomicUsize>,
        response: Arc<String>,
        delay: Duration,
        delay_before_headers: bool,
    }

    #[derive(Default)]
    struct RecordingLlmEventSink {
        events: Vec<LlmEvent>,
    }

    impl LlmEventSink for RecordingLlmEventSink {
        fn push(&mut self, event: LlmEvent) -> Result<(), crate::error::LlmError> {
            self.events.push(event);
            Ok(())
        }
    }

    async fn start_responses_fixture(
        responses: Vec<String>,
    ) -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = ResponsesFixtureState {
            requests: requests.clone(),
            responses: Arc::new(responses),
            next_response: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new()
            .route("/v1/responses", post(responses_fixture_handler))
            .route("/v1/chat/completions", post(responses_fixture_handler))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind Responses fixture");
        let address = listener.local_addr().expect("Responses fixture address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve Responses fixture");
        });
        (format!("http://{address}"), requests, server)
    }

    async fn responses_fixture_handler(
        State(state): State<ResponsesFixtureState>,
        Json(request): Json<Value>,
    ) -> Response {
        state
            .requests
            .lock()
            .expect("Responses request capture")
            .push(request);
        let response_index = state.next_response.fetch_add(1, Ordering::SeqCst);
        let Some(response) = state.responses.get(response_index) else {
            return Response::builder()
                .status(500)
                .body(Body::from(format!(
                    "unexpected Responses fixture request {}",
                    response_index + 1
                )))
                .expect("fixture overflow response");
        };

        Response::builder()
            .header(CONTENT_TYPE, "text/event-stream")
            .body(Body::from(response.clone()))
            .expect("Responses fixture SSE response")
    }

    async fn start_delayed_fixture(
        response: String,
        delay: Duration,
        delay_before_headers: bool,
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let request_count = Arc::new(AtomicUsize::new(0));
        let state = DelayedFixtureState {
            request_count: request_count.clone(),
            response: Arc::new(response),
            delay,
            delay_before_headers,
        };
        let app = Router::new()
            .route("/v1/responses", post(delayed_fixture_handler))
            .route("/v1/chat/completions", post(delayed_fixture_handler))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind delayed fixture");
        let address = listener.local_addr().expect("delayed fixture address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve delayed fixture");
        });
        (format!("http://{address}"), request_count, server)
    }

    async fn delayed_fixture_handler(
        State(state): State<DelayedFixtureState>,
        Json(_request): Json<Value>,
    ) -> Response {
        state.request_count.fetch_add(1, Ordering::SeqCst);
        if state.delay_before_headers {
            tokio::time::sleep(state.delay).await;
            return Response::builder()
                .header(CONTENT_TYPE, "text/event-stream")
                .body(Body::from(state.response.as_str().to_string()))
                .expect("delayed-header fixture response");
        }

        let response = state.response.as_str().to_string();
        let delay = state.delay;
        let body = Body::from_stream(futures_util::stream::once(async move {
            tokio::time::sleep(delay).await;
            Ok::<Bytes, Infallible>(Bytes::from(response))
        }));
        Response::builder()
            .header(CONTENT_TYPE, "text/event-stream")
            .body(body)
            .expect("delayed-stream fixture response")
    }

    fn responses_sse(events: impl IntoIterator<Item = Value>) -> String {
        events
            .into_iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect::<Vec<_>>()
            .join("")
    }

    fn responses_fixture_request(base_url: &str, messages: Vec<ModelMessage>) -> ChatRequest {
        ChatRequest {
            model: ModelProfile {
                name: "responses-fixture-model".to_string(),
                context_window: 128_000,
                max_output_tokens: 4_096,
                provider_metadata_mode: ProviderMetadataMode::LmStudioNativeRequired,
                capabilities: ModelCapabilities {
                    supports_tools: true,
                    supports_reasoning: true,
                    supports_images: true,
                },
            },
            base_url: base_url.to_string(),
            system_prompt: "Responses fixture instructions".to_string(),
            messages,
            tools: Vec::new(),
            provider_api_mode: ProviderApiMode::Responses,
            reasoning: None,
            reasoning_capability: ProviderReasoningCapability::Responses {
                supports_summary: true,
                supports_previous_response_id: true,
            },
            responses_continuation: None,
            tool_choice: None,
            parallel_tool_calls: false,
            timeout_ms: 5_000,
            stream_idle_timeout_ms: 5_000,
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

    fn reasoning_fixture_request() -> ChatRequest {
        ChatRequest {
            model: ModelProfile {
                name: "reasoning-chat-completions-fixture-model".to_string(),
                context_window: 131_072,
                max_output_tokens: 8_192,
                provider_metadata_mode: ProviderMetadataMode::OpenAiCompatibleOnly,
                capabilities: ModelCapabilities {
                    supports_tools: true,
                    supports_reasoning: true,
                    supports_images: false,
                },
            },
            base_url: "http://openai-compatible.fixture.invalid".to_string(),
            system_prompt: "Base coding prompt".to_string(),
            messages: vec![ModelMessage::User {
                content: "Plan a repository change".to_string(),
            }],
            tools: Vec::new(),
            provider_api_mode: ProviderApiMode::ChatCompletions,
            reasoning: None,
            reasoning_capability: ProviderReasoningCapability::Unsupported,
            responses_continuation: None,
            tool_choice: None,
            parallel_tool_calls: false,
            timeout_ms: 30_000,
            stream_idle_timeout_ms: 300_000,
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

    fn first_system_prompt(body: &Value) -> &str {
        body["messages"][0]["content"]
            .as_str()
            .expect("first message content is a text system prompt")
    }
}

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use eventsource_stream::{EventStreamError, Eventsource};
use futures_util::{Stream, StreamExt};
use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::config::model::{ProviderApiMode, ProviderReasoningCapability};
use crate::error::{LlmError, ProviderStreamLimit};
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
    ProviderFailure, ProviderFailureKind, ProviderPhase, ProviderPhaseEvent, ProviderRequestId,
    ProviderTerminalStatus, ProviderToolChoice, ToolSchema,
    tool_surface_scoped_parallel_tool_calls_projection,
};
use crate::session::{FinishReason, TokenUsage};
use crate::tool::truncate::clip_text_with_ellipsis;

const RETRY_INITIAL_DELAY_MS: u64 = 2_000;
const RETRY_BACKOFF_FACTOR: u64 = 2;
const RETRY_MAX_DELAY_MS: u64 = 30_000;
const PROVIDER_FAILURE_BODY_LIMIT_BYTES: usize = 64 * 1024;
const PROVIDER_FAILURE_SUMMARY_LIMIT_BYTES: usize = 243;

#[derive(Clone)]
pub struct OpenAiCompatClient {
    api_key: Option<String>,
}

impl OpenAiCompatClient {
    pub fn new(api_key: Option<String>) -> Self {
        Self { api_key }
    }
}

impl fmt::Debug for OpenAiCompatClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiCompatClient")
            .field(
                "api_key",
                &self.api_key.as_ref().map(|_| "<redacted secret>"),
            )
            .finish()
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
        let mut trace = ProviderTraceSink::new(&request, sink);
        let result = self.stream_chat_traced(request, cancel, &mut trace).await;
        trace.finish(result)
    }
}

impl OpenAiCompatClient {
    async fn stream_chat_traced(
        &self,
        request: ChatRequest,
        cancel: CancellationToken,
        sink: &mut ProviderTraceSink<'_>,
    ) -> Result<LlmResponseSummary, LlmError> {
        request.validate_provider_lifecycle()?;
        match request.provider_target().api_mode() {
            ProviderApiMode::Responses => {
                return self.stream_responses(request, cancel, sink).await;
            }
            ProviderApiMode::ChatCompletions => {}
        }
        let body = to_openai_request(&request)?;
        let body = bytes::Bytes::from(request.serialize_wire_body(&body)?);

        let Some(response) = self
            .send_request(&request, "v1/chat/completions", body, &cancel, sink)
            .await?
        else {
            return Ok(LlmResponseSummary {
                finish_reason: FinishReason::Cancelled,
                usage: None,
                response_id: None,
            });
        };

        let stream_limits = request.provider_target().stream_limits();
        let mut stream =
            bounded_response_bytes(response, stream_limits.max_raw_bytes).eventsource();
        let mut stream_budget = ProviderStreamBudget::new(stream_limits);
        let mut usage = None;
        let mut finish_reason = None;
        let mut saw_terminal_signal = false;
        let mut ended_by_eof = false;
        let mut tool_calls: HashMap<usize, PartialToolCall> = HashMap::new();

        loop {
            let idle_timeout_ms = request.provider_target().deadlines().stream_idle_timeout_ms;
            let next_event = if let Some(timeout) = stream_budget.wait_timeout(idle_timeout_ms) {
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
                                return Err(stream_budget.timeout_error(idle_timeout_ms));
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
                Err(EventStreamError::Transport(error)) => return Err(error),
                Err(error) => {
                    return Err(LlmError::Message(format!("SSE stream error: {error}")));
                }
            };
            stream_budget.record_event()?;
            sink.record_progress()?;
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
                        stream_budget.record_tool_call(format!("chat:{delta_index}"))?;
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
                                stream_budget.record_tool_arguments(
                                    &format!("chat:{delta_index}"),
                                    arguments.len() as u64,
                                )?;
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
        sink: &mut ProviderTraceSink<'_>,
    ) -> Result<LlmResponseSummary, LlmError> {
        let body = to_responses_request(&request, ResponsesRequestOptions::from_request(&request))?;
        let body = bytes::Bytes::from(request.serialize_wire_body(&body)?);
        let Some(response) = self
            .send_request(&request, "v1/responses", body, &cancel, sink)
            .await?
        else {
            return Ok(LlmResponseSummary {
                finish_reason: FinishReason::Cancelled,
                usage: None,
                response_id: None,
            });
        };

        let stream_limits = request.provider_target().stream_limits();
        let mut stream =
            bounded_response_bytes(response, stream_limits.max_raw_bytes).eventsource();
        let mut stream_budget = ProviderStreamBudget::new(stream_limits);
        let mut accumulator = ResponsesStreamAccumulator::default();

        loop {
            let idle_timeout_ms = request.provider_target().deadlines().stream_idle_timeout_ms;
            let next_event = if let Some(timeout) = stream_budget.wait_timeout(idle_timeout_ms) {
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
                                return Err(stream_budget.timeout_error(idle_timeout_ms));
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
                Err(EventStreamError::Transport(error)) => return Err(error),
                Err(error) => {
                    return Err(LlmError::Message(format!(
                        "Responses SSE stream error: {error}"
                    )));
                }
            };
            stream_budget.record_event()?;
            sink.record_progress()?;

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
            stream_budget.record_projected_events(&update.events)?;
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
        body: bytes::Bytes,
        cancel: &CancellationToken,
        trace: &mut ProviderTraceSink<'_>,
    ) -> Result<Option<reqwest::Response>, LlmError> {
        let deadlines = request.provider_target().deadlines();
        let deadline = OperationDeadline::new(deadlines.response_start_timeout_ms);
        let client_builder = reqwest::Client::builder();
        let client_builder = if deadlines.connect_timeout_ms > 0 {
            client_builder.connect_timeout(Duration::from_millis(deadlines.connect_timeout_ms))
        } else {
            client_builder
        };
        let client = client_builder.build()?;
        let endpoint_url = request
            .provider_target()
            .endpoint()
            .join_api_path(endpoint_path)
            .map_err(|error| LlmError::Message(error.to_string()))?;
        let mut attempt = 1u16;
        loop {
            trace.begin_attempt(attempt)?;
            let mut headers = HeaderMap::new();
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            if let Some(api_key) = &self.api_key {
                let value =
                    HeaderValue::from_str(&format!("Bearer {api_key}")).map_err(|error| {
                        LlmError::Message(format!("invalid API key header: {error}"))
                    })?;
                headers.insert(AUTHORIZATION, value);
            }
            apply_extra_headers(&mut headers, request.extra_headers())?;

            let request_builder = client
                .post(endpoint_url.clone())
                .headers(headers)
                .body(body.clone());

            trace.phase(ProviderPhase::RequestInFlight)?;

            let result = if let Some(timeout) = deadline.remaining() {
                match tokio::select! {
                    _ = cancel.cancelled() => return Ok(None),
                    result = tokio::time::timeout(timeout, request_builder.send()) => result,
                } {
                    Ok(result) => result,
                    Err(_) => {
                        return Err(LlmError::ProviderResponseStartTimeout {
                            timeout_ms: deadlines.response_start_timeout_ms,
                        });
                    }
                }
            } else {
                tokio::select! {
                    _ = cancel.cancelled() => return Ok(None),
                    result = request_builder.send() => result,
                }
            };

            match result {
                Ok(response) if response.status().is_success() => {
                    trace.phase(ProviderPhase::HeadersReceived)?;
                    return Ok(Some(response));
                }
                Ok(response) => {
                    let Some(failure) =
                        parse_response_failure_until_cancelled(response, cancel, &deadline).await?
                    else {
                        return Ok(None);
                    };
                    return Err(LlmError::ProviderRejected {
                        status: Some(failure.status.as_u16()),
                        code: failure.code,
                        param: failure.param,
                        message: failure.message,
                    });
                }
                Err(error)
                    if should_retry_transport_error(&error)
                        && attempt <= u16::from(deadlines.max_connect_retries) =>
                {
                    let delay = retry_delay_ms(attempt.min(u16::from(u8::MAX)) as u8, None);
                    if !sleep_retry_delay_until_deadline(delay, cancel, &deadline).await {
                        if cancel.is_cancelled() {
                            return Ok(None);
                        }
                        return Err(LlmError::ProviderResponseStartTimeout {
                            timeout_ms: deadlines.response_start_timeout_ms,
                        });
                    }
                    attempt += 1;
                }
                Err(error) => return Err(LlmError::Http(error.without_url())),
            }
        }
    }
}

struct ProviderTraceSink<'a> {
    inner: &'a mut dyn LlmEventSink,
    request_id: ProviderRequestId,
    endpoint: String,
    started_at: Instant,
    attempt: u16,
    current_phase: ProviderPhase,
    first_progress_seen: bool,
    last_progress_elapsed_ms: Option<u64>,
}

impl<'a> ProviderTraceSink<'a> {
    fn new(request: &ChatRequest, inner: &'a mut dyn LlmEventSink) -> Self {
        Self {
            inner,
            request_id: ProviderRequestId::new(),
            endpoint: request.provider_target().sanitized_endpoint().to_string(),
            started_at: Instant::now(),
            attempt: 0,
            current_phase: ProviderPhase::AttemptStarted,
            first_progress_seen: false,
            last_progress_elapsed_ms: None,
        }
    }

    fn elapsed_ms(&self) -> u64 {
        self.started_at
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64
    }

    fn begin_attempt(&mut self, attempt: u16) -> Result<(), LlmError> {
        self.attempt = attempt;
        self.phase(ProviderPhase::AttemptStarted)
    }

    fn phase(&mut self, phase: ProviderPhase) -> Result<(), LlmError> {
        self.current_phase = phase;
        self.inner.provider_phase(ProviderPhaseEvent {
            request_id: self.request_id.clone(),
            endpoint: self.endpoint.clone(),
            phase,
            attempt: self.attempt,
            elapsed_ms: self.elapsed_ms(),
            terminal_status: None,
            failure: None,
        })
    }

    fn record_progress(&mut self) -> Result<(), LlmError> {
        self.last_progress_elapsed_ms = Some(self.elapsed_ms());
        if self.first_progress_seen {
            return Ok(());
        }
        self.first_progress_seen = true;
        self.phase(ProviderPhase::FirstProgress)
    }

    fn finish(
        &mut self,
        result: Result<LlmResponseSummary, LlmError>,
    ) -> Result<LlmResponseSummary, LlmError> {
        if let Some(elapsed_ms) = self.last_progress_elapsed_ms {
            self.current_phase = ProviderPhase::LastProgress;
            self.inner.provider_phase(ProviderPhaseEvent {
                request_id: self.request_id.clone(),
                endpoint: self.endpoint.clone(),
                phase: ProviderPhase::LastProgress,
                attempt: self.attempt,
                elapsed_ms,
                terminal_status: None,
                failure: None,
            })?;
        }

        match result {
            Ok(summary) => {
                let terminal_status = if summary.finish_reason == FinishReason::Cancelled {
                    ProviderTerminalStatus::Cancelled
                } else {
                    ProviderTerminalStatus::Completed
                };
                self.current_phase = ProviderPhase::ProviderTerminal;
                self.inner.provider_phase(ProviderPhaseEvent {
                    request_id: self.request_id.clone(),
                    endpoint: self.endpoint.clone(),
                    phase: ProviderPhase::ProviderTerminal,
                    attempt: self.attempt,
                    elapsed_ms: self.elapsed_ms(),
                    terminal_status: Some(terminal_status),
                    failure: None,
                })?;
                Ok(summary)
            }
            Err(source) => {
                if matches!(
                    &source,
                    LlmError::ProviderRequestLimitExceeded { .. }
                        | LlmError::ProviderRequestImage(_)
                ) {
                    return Err(source);
                }
                if matches!(source, LlmError::ProviderFailure { .. }) {
                    return Err(source);
                }
                let failure = self.failure_for(&source);
                self.inner.provider_phase(ProviderPhaseEvent {
                    request_id: self.request_id.clone(),
                    endpoint: self.endpoint.clone(),
                    phase: ProviderPhase::ProviderTerminal,
                    attempt: self.attempt,
                    elapsed_ms: self.elapsed_ms(),
                    terminal_status: Some(ProviderTerminalStatus::Failed),
                    failure: Some(failure.clone()),
                })?;
                Err(LlmError::ProviderFailure {
                    failure,
                    source: Box::new(source),
                })
            }
        }
    }

    fn failure_for(&self, source: &LlmError) -> ProviderFailure {
        let (kind, status, code) = match source {
            LlmError::Http(error) if error.is_connect() => {
                (ProviderFailureKind::Connect, None, None)
            }
            LlmError::ProviderResponseStartTimeout { .. } => {
                (ProviderFailureKind::ResponseStartTimeout, None, None)
            }
            LlmError::ProviderStreamIdleTimeout { .. } => {
                (ProviderFailureKind::StreamIdleTimeout, None, None)
            }
            LlmError::ProviderStreamLimitExceeded { .. } => {
                (ProviderFailureKind::Protocol, None, None)
            }
            LlmError::ProviderRejected { status, code, .. } => {
                (ProviderFailureKind::HttpStatus, *status, code.clone())
            }
            LlmError::Json(_) => (ProviderFailureKind::Decode, None, None),
            LlmError::Message(_) if self.current_phase == ProviderPhase::FirstProgress => {
                (ProviderFailureKind::Protocol, None, None)
            }
            LlmError::Message(_) if self.current_phase == ProviderPhase::HeadersReceived => {
                (ProviderFailureKind::Protocol, None, None)
            }
            _ => (ProviderFailureKind::Other, None, None),
        };
        ProviderFailure {
            request_id: self.request_id.clone(),
            endpoint: self.endpoint.clone(),
            phase: self.current_phase,
            attempt: self.attempt,
            elapsed_ms: self.elapsed_ms(),
            kind,
            status,
            code,
            message: source.to_string(),
        }
    }
}

impl LlmEventSink for ProviderTraceSink<'_> {
    fn push(&mut self, event: LlmEvent) -> Result<(), LlmError> {
        self.inner.push(event)
    }

    fn provider_phase(&mut self, event: ProviderPhaseEvent) -> Result<(), LlmError> {
        self.inner.provider_phase(event)
    }
}

#[derive(Debug, Clone, Copy)]
struct OperationDeadline {
    deadline: Option<Instant>,
    response_start_timeout_ms: u64,
}

impl OperationDeadline {
    fn new(timeout_ms: u64) -> Self {
        Self {
            deadline: (timeout_ms > 0).then(|| Instant::now() + Duration::from_millis(timeout_ms)),
            response_start_timeout_ms: timeout_ms,
        }
    }

    fn remaining(self) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }
}

#[derive(Debug)]
struct ResponseFailure {
    status: StatusCode,
    code: Option<String>,
    param: Option<String>,
    message: String,
}

async fn parse_response_failure(response: reqwest::Response) -> Result<ResponseFailure, LlmError> {
    let status = response.status();
    let (body, body_was_truncated) = read_provider_failure_body_bounded(response).await?;
    let body = String::from_utf8_lossy(&body);
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
        .unwrap_or_else(|| summarize_failure_body(&body, body_was_truncated));
    Ok(ResponseFailure {
        status,
        code,
        param,
        message,
    })
}

async fn read_provider_failure_body_bounded(
    response: reqwest::Response,
) -> Result<(Vec<u8>, bool), LlmError> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::with_capacity(PROVIDER_FAILURE_BODY_LIMIT_BYTES.min(8 * 1024));
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| LlmError::Http(error.without_url()))?;
        let remaining = PROVIDER_FAILURE_BODY_LIMIT_BYTES.saturating_sub(body.len());
        if remaining == 0 {
            return Ok((body, true));
        }
        let retained = remaining.min(chunk.len());
        body.extend_from_slice(&chunk[..retained]);
        if retained < chunk.len() {
            return Ok((body, true));
        }
    }
    Ok((body, false))
}

async fn parse_response_failure_until_cancelled(
    response: reqwest::Response,
    cancel: &CancellationToken,
    deadline: &OperationDeadline,
) -> Result<Option<ResponseFailure>, LlmError> {
    let parse = parse_response_failure(response);
    tokio::pin!(parse);
    if let Some(timeout) = deadline.remaining() {
        tokio::select! {
            _ = cancel.cancelled() => Ok(None),
            result = tokio::time::timeout(timeout, &mut parse) => match result {
                Ok(result) => result.map(Some),
                Err(_) => Err(LlmError::ProviderResponseStartTimeout {
                    timeout_ms: deadline.response_start_timeout_ms,
                }),
            },
        }
    } else {
        tokio::select! {
            _ = cancel.cancelled() => Ok(None),
            result = &mut parse => result.map(Some),
        }
    }
}

fn stream_idle_timeout_error(timeout_ms: u64) -> LlmError {
    LlmError::ProviderStreamIdleTimeout { timeout_ms }
}

fn bounded_response_bytes(
    response: reqwest::Response,
    maximum: u64,
) -> impl Stream<Item = Result<bytes::Bytes, LlmError>> {
    let mut observed = 0_u64;
    response.bytes_stream().map(move |chunk| {
        let chunk = chunk.map_err(|error| LlmError::Http(error.without_url()))?;
        observed = observed.saturating_add(chunk.len() as u64);
        if observed > maximum {
            return Err(LlmError::ProviderStreamLimitExceeded {
                surface: ProviderStreamLimit::RawBytes,
                actual: observed,
                maximum,
            });
        }
        Ok(chunk)
    })
}

struct ProviderStreamBudget {
    limits: crate::config::ProviderStreamLimits,
    started_at: Instant,
    event_count: u64,
    tool_calls: HashSet<String>,
    tool_argument_bytes: HashMap<String, u64>,
}

impl ProviderStreamBudget {
    fn new(limits: crate::config::ProviderStreamLimits) -> Self {
        Self {
            limits,
            started_at: Instant::now(),
            event_count: 0,
            tool_calls: HashSet::new(),
            tool_argument_bytes: HashMap::new(),
        }
    }

    fn wait_timeout(&self, idle_timeout_ms: u64) -> Option<Duration> {
        let idle = (idle_timeout_ms > 0).then(|| Duration::from_millis(idle_timeout_ms));
        let absolute = (self.limits.max_duration_ms > 0).then(|| {
            Duration::from_millis(self.limits.max_duration_ms)
                .saturating_sub(self.started_at.elapsed())
        });
        match (idle, absolute) {
            (Some(idle), Some(absolute)) => Some(idle.min(absolute)),
            (Some(idle), None) => Some(idle),
            (None, Some(absolute)) => Some(absolute),
            (None, None) => None,
        }
    }

    fn timeout_error(&self, idle_timeout_ms: u64) -> LlmError {
        let elapsed_ms = self
            .started_at
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        if self.limits.max_duration_ms > 0 && elapsed_ms >= self.limits.max_duration_ms {
            LlmError::ProviderStreamLimitExceeded {
                surface: ProviderStreamLimit::DurationMs,
                actual: elapsed_ms,
                maximum: self.limits.max_duration_ms,
            }
        } else {
            stream_idle_timeout_error(idle_timeout_ms)
        }
    }

    fn record_event(&mut self) -> Result<(), LlmError> {
        self.event_count = self.event_count.saturating_add(1);
        ensure_stream_limit(
            ProviderStreamLimit::EventCount,
            self.event_count,
            self.limits.max_events,
        )
    }

    fn record_tool_call(&mut self, key: impl Into<String>) -> Result<(), LlmError> {
        let key = key.into();
        if self.tool_calls.insert(key) {
            ensure_stream_limit(
                ProviderStreamLimit::ToolCallCount,
                self.tool_calls.len() as u64,
                self.limits.max_tool_calls,
            )?;
        }
        Ok(())
    }

    fn record_tool_arguments(&mut self, key: &str, delta_bytes: u64) -> Result<(), LlmError> {
        let total = self.tool_argument_bytes.entry(key.to_string()).or_default();
        *total = total.saturating_add(delta_bytes);
        ensure_stream_limit(
            ProviderStreamLimit::ToolCallArgumentBytes,
            *total,
            self.limits.max_tool_call_argument_bytes,
        )
    }

    fn record_projected_events(&mut self, events: &[LlmEvent]) -> Result<(), LlmError> {
        for event in events {
            match event {
                LlmEvent::ToolCallStart { call_id, .. } => {
                    self.record_tool_call(format!("responses:{call_id}"))?;
                }
                LlmEvent::ToolCallArgsDelta { call_id, delta } => {
                    self.record_tool_arguments(
                        &format!("responses:{call_id}"),
                        delta.len() as u64,
                    )?;
                }
                LlmEvent::TextDelta(_)
                | LlmEvent::ReasoningSummaryDelta(_)
                | LlmEvent::Finished { .. } => {}
            }
        }
        Ok(())
    }
}

fn ensure_stream_limit(
    surface: ProviderStreamLimit,
    actual: u64,
    maximum: u64,
) -> Result<(), LlmError> {
    if actual > maximum {
        return Err(LlmError::ProviderStreamLimitExceeded {
            surface,
            actual,
            maximum,
        });
    }
    Ok(())
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

async fn sleep_retry_delay_until_deadline(
    delay_ms: u64,
    cancel: &CancellationToken,
    deadline: &OperationDeadline,
) -> bool {
    let delay = Duration::from_millis(delay_ms);
    let sleep_for = deadline
        .remaining()
        .map_or(delay, |remaining| remaining.min(delay));
    if sleep_for.is_zero() {
        return false;
    }
    tokio::select! {
        _ = cancel.cancelled() => false,
        _ = tokio::time::sleep(sleep_for) => deadline.deadline.is_none_or(|end| Instant::now() < end),
    }
}

fn summarize_failure_body(body: &str, source_was_truncated: bool) -> String {
    let content_limit = PROVIDER_FAILURE_SUMMARY_LIMIT_BYTES.saturating_sub(3);
    let mut compact = String::with_capacity(PROVIDER_FAILURE_SUMMARY_LIMIT_BYTES);
    let mut was_truncated = source_was_truncated;
    for segment in body.split_whitespace() {
        let separator_len = usize::from(!compact.is_empty());
        let remaining = content_limit.saturating_sub(compact.len());
        if remaining <= separator_len {
            was_truncated = true;
            break;
        }
        if separator_len > 0 {
            compact.push(' ');
        }
        let remaining = content_limit.saturating_sub(compact.len());
        if segment.len() <= remaining {
            compact.push_str(segment);
            continue;
        }
        let mut end = remaining;
        while end > 0 && !segment.is_char_boundary(end) {
            end -= 1;
        }
        compact.push_str(&segment[..end]);
        was_truncated = true;
        break;
    }
    if compact.is_empty() {
        "request failed without a response body".to_string()
    } else {
        if was_truncated {
            compact.push_str("...");
        }
        compact
    }
}

fn should_retry_transport_error(error: &reqwest::Error) -> bool {
    error.is_connect()
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
    if request.provider_target().api_mode() != ProviderApiMode::ChatCompletions {
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
    let mut system_segments = vec![request.system_prompt.clone()];
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
    if let Some(tool_choice) = request.tool_choice.as_ref().map(provider_tool_choice_json)
        && let Value::Object(base_map) = &mut body
    {
        base_map.insert("tool_choice".to_string(), tool_choice);
    }
    Ok(body)
}

pub(crate) fn provider_tool_choice_json(tool_choice: &ProviderToolChoice) -> Value {
    match tool_choice {
        ProviderToolChoice::Required => serde_json::json!("required"),
        ProviderToolChoice::Named { name } => serde_json::json!({
            "type": "function",
            "function": {
                "name": name
            }
        }),
    }
}

fn openai_tool_schema(tool: &ToolSchema) -> OpenAiToolSchema {
    OpenAiToolSchema {
        schema_type: "function".to_string(),
        function: OpenAiFunctionSchema {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.input_schema.clone(),
        },
    }
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
        OpenAiCompatClient, PartialToolCall, parse_finish_reason, resolve_finish_reason,
        retry_delay_ms, to_openai_request, to_openai_request_with_reasoning,
        validate_streamed_tool_calls,
    };
    use crate::config::model::{
        ChatCompletionsReasoningParameters, ProviderApiMode, ProviderReasoningCapability,
        ReasoningEffort, ReasoningSummary,
    };
    use crate::config::{
        ProviderDeadlines, ProviderMetadataMode, ProviderRequestLimits, ProviderStreamLimits,
        ProviderTarget,
    };
    use crate::error::{LlmError, ProviderRequestLimit, ProviderStreamLimit};
    use crate::llm::contract::ReasoningRequest;
    use crate::llm::{
        ChatRequest, LlmClient, LlmEvent, LlmEventSink, ModelCapabilities, ModelMessage,
        ModelProfile, ModelToolCall, ProviderFailureKind, ProviderPhase, ProviderPhaseEvent,
        ProviderTerminalStatus, ProviderToolChoice, ResponsesContinuation, ToolSchema,
    };
    use crate::session::{FinishReason, TokenUsage};

    fn provider_target(
        endpoint: &str,
        model: &str,
        metadata_mode: ProviderMetadataMode,
        api_mode: ProviderApiMode,
        deadlines: ProviderDeadlines,
    ) -> ProviderTarget {
        ProviderTarget::new(endpoint, model, metadata_mode, api_mode, deadlines)
            .expect("provider target")
    }

    fn replace_provider_endpoint(request: &mut ChatRequest, endpoint: &str) {
        let model = request.provider_target().model().to_string();
        let metadata_mode = request.provider_target().metadata_mode();
        let api_mode = request.provider_target().api_mode();
        let deadlines = request.provider_target().deadlines();
        request.replace_provider_target(provider_target(
            endpoint,
            &model,
            metadata_mode,
            api_mode,
            deadlines,
        ));
    }

    fn replace_provider_deadlines(request: &mut ChatRequest, deadlines: ProviderDeadlines) {
        let endpoint = request.provider_target().sanitized_endpoint().to_string();
        let model = request.provider_target().model().to_string();
        let metadata_mode = request.provider_target().metadata_mode();
        let api_mode = request.provider_target().api_mode();
        request.replace_provider_target(provider_target(
            &endpoint,
            &model,
            metadata_mode,
            api_mode,
            deadlines,
        ));
    }

    #[test]
    fn transport_preserves_the_caller_owned_system_prompt() {
        let model = ModelProfile {
            name: "openai-compatible-fixture-model".to_string(),
            context_window: 131_072,
            max_output_tokens: 8_192,
            provider_metadata_mode: ProviderMetadataMode::OpenAiCompatibleOnly,
            capabilities: ModelCapabilities {
                supports_tools: true,
                supports_reasoning: false,
                supports_images: false,
            },
        };
        let provider = provider_target(
            "http://openai-compatible.fixture.invalid",
            &model.name,
            model.provider_metadata_mode,
            ProviderApiMode::ChatCompletions,
            ProviderDeadlines {
                response_start_timeout_ms: 30_000,
                stream_idle_timeout_ms: 300_000,
                connect_timeout_ms: 1_000,
                max_connect_retries: 0,
            },
        );
        let request = ChatRequest::new(
            provider,
            model,
            "Base coding prompt".to_string(),
            Vec::new(),
            Vec::new(),
            None,
            ProviderReasoningCapability::Unsupported,
            BTreeMap::new(),
        );

        let body = to_openai_request(&request).expect("request serialization succeeds");
        let system_prompt = first_system_prompt(&body);

        assert_eq!(system_prompt, "Base coding prompt");
    }

    #[test]
    fn tool_enabled_payload_uses_configured_output_budget() {
        let model = ModelProfile {
            name: "openai-compatible-fixture-model".to_string(),
            context_window: 131_072,
            max_output_tokens: 131_072,
            provider_metadata_mode: ProviderMetadataMode::OpenAiCompatibleOnly,
            capabilities: ModelCapabilities {
                supports_tools: true,
                supports_reasoning: false,
                supports_images: false,
            },
        };
        let provider = provider_target(
            "http://openai-compatible.fixture.invalid",
            &model.name,
            model.provider_metadata_mode,
            ProviderApiMode::ChatCompletions,
            ProviderDeadlines {
                response_start_timeout_ms: 30_000,
                stream_idle_timeout_ms: 300_000,
                connect_timeout_ms: 1_000,
                max_connect_retries: 0,
            },
        );
        let mut request = ChatRequest::new(
            provider,
            model,
            "Base coding prompt".to_string(),
            vec![ModelMessage::User {
                content: "Create src/workflow.rs".to_string(),
            }],
            vec![ToolSchema {
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
            }],
            None,
            ProviderReasoningCapability::Unsupported,
            BTreeMap::new(),
        );
        request.tool_choice = Some(ProviderToolChoice::Required);
        request.parallel_tool_calls = true;

        let tool_body = to_openai_request(&request).expect("request serialization succeeds");
        request.tools.clear();
        request.tool_choice = None;
        request.parallel_tool_calls = false;
        request.extra_body = None;
        let no_tool_body = to_openai_request(&request).expect("request serialization succeeds");

        assert_eq!(tool_body["max_tokens"].as_u64(), Some(131_072));
        assert_eq!(no_tool_body["max_tokens"].as_u64(), Some(131_072));
        assert!(
            tool_body["tools"][0]["function"].get("strict").is_none(),
            "Chat Completions tool schemas must not contain an unsupported strict field"
        );
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
    fn connect_retry_backoff_is_bounded() {
        assert_eq!(retry_delay_ms(1, None), 2_000);
        assert_eq!(retry_delay_ms(2, None), 4_000);
        assert_eq!(retry_delay_ms(u8::MAX, None), 30_000);
    }

    #[test]
    fn client_debug_redacts_the_api_key() {
        let client = OpenAiCompatClient::new(Some("provider-api-key-secret".to_string()));

        let debug = format!("{client:?}");

        assert!(!debug.contains("provider-api-key-secret"));
        assert!(debug.contains("redacted"));
    }

    #[test]
    fn lm_studio_v1_base_url_is_joined_without_a_duplicate_version_segment() {
        for base_url in [
            "http://127.0.0.1:1234",
            "http://127.0.0.1:1234/",
            "http://127.0.0.1:1234/v1",
            "http://127.0.0.1:1234/v1/",
        ] {
            let endpoint = crate::config::ProviderEndpoint::parse(base_url)
                .expect("LM Studio provider endpoint");
            let responses = endpoint
                .join_api_path("v1/responses")
                .expect("LM Studio Responses endpoint");
            let chat_completions = endpoint
                .join_api_path("v1/chat/completions")
                .expect("LM Studio Chat Completions endpoint");

            assert_eq!(responses.path(), "/v1/responses");
            assert_eq!(chat_completions.path(), "/v1/chat/completions");
        }

        let proxied =
            crate::config::ProviderEndpoint::parse("https://provider.example/proxy/openai/v1")
                .expect("proxied provider endpoint")
                .join_api_path("v1/responses")
                .expect("proxied LM Studio endpoint");
        assert_eq!(proxied.path(), "/proxy/openai/v1/responses");
        assert_eq!(proxied.query(), None);
    }

    #[test]
    fn provider_failure_summary_is_bounded_without_materializing_split_segments() {
        let body = format!(
            "provider failure {}",
            "oversized-segment".repeat(super::PROVIDER_FAILURE_BODY_LIMIT_BYTES)
        );

        let summary = super::summarize_failure_body(&body, true);

        assert!(summary.len() <= super::PROVIDER_FAILURE_SUMMARY_LIMIT_BYTES);
        assert!(summary.ends_with("..."));
    }

    #[tokio::test]
    async fn connection_failure_does_not_project_header_or_client_secrets() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("reserve closed fixture address");
        let address = listener.local_addr().expect("fixture address");
        drop(listener);

        let mut request = responses_fixture_request(
            &format!("http://{address}/v1"),
            vec![ModelMessage::User {
                content: "Exercise a redacted connection failure".to_string(),
            }],
        );
        replace_provider_deadlines(
            &mut request,
            ProviderDeadlines {
                response_start_timeout_ms: 500,
                stream_idle_timeout_ms: 5_000,
                connect_timeout_ms: 200,
                max_connect_retries: 0,
            },
        );
        request.replace_extra_headers(BTreeMap::from([(
            "x-provider-key".to_string(),
            "header-secret".to_string(),
        )]));
        let client = OpenAiCompatClient::new(Some("client-api-key-secret".to_string()));
        let mut sink = RecordingLlmEventSink::default();

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("closed endpoint must fail");
        let rendered = format!("{error:?}\n{error}");

        for secret in ["header-secret", "client-api-key-secret"] {
            assert!(!rendered.contains(secret));
        }
    }

    #[tokio::test]
    async fn oversized_exact_wire_body_is_rejected_before_any_provider_post() {
        let (base_url, requests, server) = start_responses_fixture(Vec::new()).await;
        let mut request = responses_fixture_request(
            &base_url,
            vec![ModelMessage::User {
                content: "x".repeat(4_096),
            }],
        );
        let mut provider = request.provider_target().clone();
        let mut limits = ProviderRequestLimits::product_default();
        limits.max_serialized_body_bytes = 256;
        provider.replace_request_limits(limits);
        request.replace_provider_target(provider);
        let client = OpenAiCompatClient::new(None);
        let mut sink = RecordingLlmEventSink::default();

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("oversized wire body must fail before transport");
        server.abort();

        assert!(matches!(
            error,
            LlmError::ProviderRequestLimitExceeded {
                surface: ProviderRequestLimit::SerializedBodyBytes,
                maximum: 256,
                ..
            }
        ));
        assert!(requests.lock().expect("request capture").is_empty());
        assert!(sink.phases.is_empty());
        assert!(sink.events.is_empty());
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
        replace_provider_endpoint(&mut request, &base_url);
        let client = OpenAiCompatClient::new(None);
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
        let client = OpenAiCompatClient::new(None);
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
        assert_eq!(
            sink.phases
                .iter()
                .map(|event| event.phase)
                .collect::<Vec<_>>(),
            vec![
                ProviderPhase::AttemptStarted,
                ProviderPhase::RequestInFlight,
                ProviderPhase::HeadersReceived,
                ProviderPhase::FirstProgress,
                ProviderPhase::LastProgress,
                ProviderPhase::ProviderTerminal,
            ]
        );
        assert!(
            sink.phases
                .windows(2)
                .all(|events| events[0].request_id == events[1].request_id)
        );
        assert_eq!(
            sink.phases.last().and_then(|event| event.terminal_status),
            Some(ProviderTerminalStatus::Completed)
        );

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
        }];
        first_request.tool_choice = Some(ProviderToolChoice::Required);
        first_request.extra_body = Some(json!({ "num_ctx": 131_072 }));
        let client = OpenAiCompatClient::new(None);
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
        let mut request = responses_fixture_request(
            &base_url,
            vec![ModelMessage::User {
                content: "Retry transient failure".to_string(),
            }],
        );
        let mut deadlines = request.provider_target().deadlines();
        deadlines.max_connect_retries = 1;
        replace_provider_deadlines(&mut request, deadlines);
        let client = OpenAiCompatClient::new(None);
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
    async fn http_status_response_is_not_retried() {
        let (base_url, requests, server) = start_responses_fixture(Vec::new()).await;
        let mut request = responses_fixture_request(
            &base_url,
            vec![ModelMessage::User {
                content: "Do not retry an HTTP response".to_string(),
            }],
        );
        let mut deadlines = request.provider_target().deadlines();
        deadlines.max_connect_retries = 3;
        replace_provider_deadlines(&mut request, deadlines);
        let client = OpenAiCompatClient::new(None);
        let mut sink = RecordingLlmEventSink::default();

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("HTTP status must terminate the generation request");
        server.abort();

        let failure = error.provider_failure().expect("typed provider failure");
        assert_eq!(failure.kind, ProviderFailureKind::HttpStatus);
        assert!(failure.message.len() < 1_024);
        assert!(failure.message.ends_with("..."));
        assert_eq!(requests.lock().expect("request capture").len(), 1);
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
        replace_provider_deadlines(
            &mut request,
            ProviderDeadlines {
                response_start_timeout_ms: 30,
                stream_idle_timeout_ms: 1_000,
                connect_timeout_ms: 1_000,
                max_connect_retries: 2,
            },
        );
        let client = OpenAiCompatClient::new(None);
        let mut sink = RecordingLlmEventSink::default();

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("header timeout must terminate the generation request");
        server.abort();

        assert_eq!(
            error.provider_failure().map(|failure| failure.kind),
            Some(ProviderFailureKind::ResponseStartTimeout)
        );
        assert_eq!(
            error.provider_failure().map(|failure| failure.phase),
            Some(ProviderPhase::RequestInFlight)
        );
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        assert!(sink.events.is_empty());
        assert_eq!(
            sink.phases
                .iter()
                .map(|event| event.phase)
                .collect::<Vec<_>>(),
            vec![
                ProviderPhase::AttemptStarted,
                ProviderPhase::RequestInFlight,
                ProviderPhase::ProviderTerminal,
            ]
        );
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
        replace_provider_deadlines(
            &mut request,
            ProviderDeadlines {
                response_start_timeout_ms: 1_000,
                stream_idle_timeout_ms: 30,
                connect_timeout_ms: 1_000,
                max_connect_retries: 2,
            },
        );
        let client = OpenAiCompatClient::new(None);
        let mut sink = RecordingLlmEventSink::default();

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("stream idle timeout must terminate the generation request");
        server.abort();

        assert_eq!(
            error.provider_failure().map(|failure| failure.kind),
            Some(ProviderFailureKind::StreamIdleTimeout)
        );
        assert_eq!(
            error.provider_failure().map(|failure| failure.phase),
            Some(ProviderPhase::HeadersReceived)
        );
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
        assert!(sink.events.is_empty());
        assert_eq!(
            sink.phases
                .iter()
                .map(|event| event.phase)
                .collect::<Vec<_>>(),
            vec![
                ProviderPhase::AttemptStarted,
                ProviderPhase::RequestInFlight,
                ProviderPhase::HeadersReceived,
                ProviderPhase::ProviderTerminal,
            ]
        );
    }

    #[test]
    fn stream_budget_bounds_events_tool_calls_and_per_call_arguments() {
        let mut limits = ProviderStreamLimits::product_default();
        limits.max_events = 1;
        limits.max_tool_calls = 1;
        limits.max_tool_call_argument_bytes = 4;
        let mut budget = super::ProviderStreamBudget::new(limits);

        budget.record_event().expect("first event");
        assert!(matches!(
            budget.record_event(),
            Err(LlmError::ProviderStreamLimitExceeded {
                surface: ProviderStreamLimit::EventCount,
                ..
            })
        ));
        budget.record_tool_call("call_1").expect("first tool");
        assert!(matches!(
            budget.record_tool_call("call_2"),
            Err(LlmError::ProviderStreamLimitExceeded {
                surface: ProviderStreamLimit::ToolCallCount,
                ..
            })
        ));
        budget
            .record_tool_arguments("call_1", 4)
            .expect("argument boundary");
        assert!(matches!(
            budget.record_tool_arguments("call_1", 1),
            Err(LlmError::ProviderStreamLimitExceeded {
                surface: ProviderStreamLimit::ToolCallArgumentBytes,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn raw_stream_byte_limit_terminates_once_without_reposting() {
        let response = responses_sse([json!({
            "type": "response.output_text.delta",
            "item_id": "large_event",
            "delta": "x".repeat(4_096)
        })]);
        let (base_url, requests, server) = start_responses_fixture(vec![response]).await;
        let mut request = responses_fixture_request(
            &base_url,
            vec![ModelMessage::User {
                content: "bound the stream".to_string(),
            }],
        );
        let mut provider = request.provider_target().clone();
        let mut limits = ProviderStreamLimits::product_default();
        limits.max_raw_bytes = 128;
        provider.replace_stream_limits(limits);
        request.replace_provider_target(provider);
        let client = OpenAiCompatClient::new(None);
        let mut sink = RecordingLlmEventSink::default();

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("raw stream must be bounded");
        server.abort();

        assert!(matches!(
            error,
            LlmError::ProviderFailure { source, .. }
                if matches!(
                    *source,
                    LlmError::ProviderStreamLimitExceeded {
                        surface: ProviderStreamLimit::RawBytes,
                        ..
                    }
                )
        ));
        assert_eq!(requests.lock().expect("request capture").len(), 1);
        assert!(sink.events.is_empty());
    }

    #[tokio::test]
    async fn absolute_stream_duration_terminates_active_operation_without_reposting() {
        let response = responses_sse([json!({
            "type": "response.completed",
            "response": { "id": "too_late" }
        })]);
        let (base_url, request_count, server) =
            start_delayed_fixture(response, Duration::from_millis(200), false).await;
        let mut request = responses_fixture_request(
            &base_url,
            vec![ModelMessage::User {
                content: "bound duration".to_string(),
            }],
        );
        replace_provider_deadlines(
            &mut request,
            ProviderDeadlines {
                response_start_timeout_ms: 1_000,
                stream_idle_timeout_ms: 1_000,
                connect_timeout_ms: 1_000,
                max_connect_retries: 2,
            },
        );
        let mut provider = request.provider_target().clone();
        let mut limits = ProviderStreamLimits::product_default();
        limits.max_duration_ms = 30;
        provider.replace_stream_limits(limits);
        request.replace_provider_target(provider);
        let client = OpenAiCompatClient::new(None);
        let mut sink = RecordingLlmEventSink::default();

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("absolute stream duration must be bounded");
        server.abort();

        assert!(matches!(
            error,
            LlmError::ProviderFailure { source, .. }
                if matches!(
                    *source,
                    LlmError::ProviderStreamLimitExceeded {
                        surface: ProviderStreamLimit::DurationMs,
                        ..
                    }
                )
        ));
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
        let client = OpenAiCompatClient::new(None);

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
        phases: Vec<ProviderPhaseEvent>,
    }

    impl LlmEventSink for RecordingLlmEventSink {
        fn push(&mut self, event: LlmEvent) -> Result<(), crate::error::LlmError> {
            self.events.push(event);
            Ok(())
        }

        fn provider_phase(
            &mut self,
            event: ProviderPhaseEvent,
        ) -> Result<(), crate::error::LlmError> {
            self.phases.push(event);
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
            let oversized_body = format!(
                "unexpected Responses fixture request {} {}",
                response_index + 1,
                "x".repeat(super::PROVIDER_FAILURE_BODY_LIMIT_BYTES * 2)
            );
            return Response::builder()
                .status(500)
                .body(Body::from(oversized_body))
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
        let model = ModelProfile {
            name: "responses-fixture-model".to_string(),
            context_window: 128_000,
            max_output_tokens: 4_096,
            provider_metadata_mode: ProviderMetadataMode::LmStudioNativeRequired,
            capabilities: ModelCapabilities {
                supports_tools: true,
                supports_reasoning: true,
                supports_images: true,
            },
        };
        let provider = provider_target(
            base_url,
            &model.name,
            model.provider_metadata_mode,
            ProviderApiMode::Responses,
            ProviderDeadlines {
                response_start_timeout_ms: 5_000,
                stream_idle_timeout_ms: 5_000,
                connect_timeout_ms: 1_000,
                max_connect_retries: 0,
            },
        );
        ChatRequest::new(
            provider,
            model,
            "Responses fixture instructions".to_string(),
            messages,
            Vec::new(),
            None,
            ProviderReasoningCapability::Responses {
                supports_summary: true,
                supports_previous_response_id: true,
            },
            BTreeMap::new(),
        )
    }

    fn reasoning_fixture_request() -> ChatRequest {
        let model = ModelProfile {
            name: "reasoning-chat-completions-fixture-model".to_string(),
            context_window: 131_072,
            max_output_tokens: 8_192,
            provider_metadata_mode: ProviderMetadataMode::OpenAiCompatibleOnly,
            capabilities: ModelCapabilities {
                supports_tools: true,
                supports_reasoning: true,
                supports_images: false,
            },
        };
        let provider = provider_target(
            "http://openai-compatible.fixture.invalid",
            &model.name,
            model.provider_metadata_mode,
            ProviderApiMode::ChatCompletions,
            ProviderDeadlines {
                response_start_timeout_ms: 30_000,
                stream_idle_timeout_ms: 300_000,
                connect_timeout_ms: 1_000,
                max_connect_retries: 0,
            },
        );
        ChatRequest::new(
            provider,
            model,
            "Base coding prompt".to_string(),
            vec![ModelMessage::User {
                content: "Plan a repository change".to_string(),
            }],
            Vec::new(),
            None,
            ProviderReasoningCapability::Unsupported,
            BTreeMap::new(),
        )
    }

    fn first_system_prompt(body: &Value) -> &str {
        body["messages"][0]["content"]
            .as_str()
            .expect("first message content is a text system prompt")
    }
}

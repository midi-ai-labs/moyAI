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
const RETRY_INITIAL_DELAY_MS: u64 = 2_000;
const RETRY_BACKOFF_FACTOR: u64 = 2;
const RETRY_MAX_DELAY_MS: u64 = 30_000;
const PROVIDER_FAILURE_BODY_LIMIT_BYTES: usize = 64 * 1024;
const PROVIDER_FAILURE_SUMMARY_LIMIT_BYTES: usize = 243;
const PROVIDER_STREAM_SUMMARY_SCAN_LIMIT_BYTES: usize = PROVIDER_FAILURE_SUMMARY_LIMIT_BYTES * 4;
const PROVIDER_STREAM_ERROR_MESSAGE_LIMIT_BYTES: usize = 160;
const PROVIDER_STREAM_ERROR_FIELD_LIMIT_BYTES: usize = 48;

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
        let mut saw_terminal_signal = false;
        let mut ended_by_eof = false;
        let mut accumulator = ChatStreamAccumulator::default();

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
            let chunk_usage = chunk.usage.as_ref().map(to_usage).transpose()?;
            if let (Some(existing), Some(candidate)) = (usage.as_ref(), chunk_usage.as_ref())
                && !same_token_usage(existing, candidate)
            {
                return Err(LlmError::Message(
                    "openai-compatible stream returned conflicting usage payloads across chunks"
                        .to_string(),
                ));
            }
            let update = accumulator.apply_chunk(chunk, &mut stream_budget)?;
            if usage.is_none() {
                usage = chunk_usage;
            }
            saw_terminal_signal |= update.saw_terminal_signal;
            for event in update.events {
                sink.push(event)?;
            }
        }

        if ended_by_eof && !saw_terminal_signal {
            return Err(stream_missing_terminal_signal_error());
        }

        let has_complete_tool_calls = validate_streamed_tool_calls(&accumulator.tool_calls)?;
        let finish_reason =
            resolve_finish_reason(accumulator.finish_reason, has_complete_tool_calls)?;

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
    fn request_headers(&self, request: &ChatRequest) -> Result<HeaderMap, LlmError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(api_key) = &self.api_key {
            let value = HeaderValue::from_str(&format!("Bearer {api_key}"))
                .map_err(|error| LlmError::Message(format!("invalid API key header: {error}")))?;
            headers.insert(AUTHORIZATION, value);
        }
        apply_extra_headers(&mut headers, request.extra_headers())?;
        Ok(headers)
    }

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
        let mut accumulator =
            ResponsesStreamAccumulator::new(stream_limits.max_tool_call_argument_bytes);

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

            let update = accumulator
                .push_json(&event.data)
                .map_err(|error| match error {
                    limit @ LlmError::ProviderStreamLimitExceeded { .. } => limit,
                    error => LlmError::Message(format!(
                        "failed to parse Responses stream event: {error}. Raw event: {}",
                        summarize_stream_chunk(&event.data)
                    )),
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
                Some(ResponsesTerminal::Failed {
                    response_id,
                    code,
                    message,
                }) => {
                    return Err(LlmError::ProviderGenerationFailed {
                        response_id,
                        code,
                        message,
                        max_output_tokens: request.effective_max_output_tokens(),
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
        // Header parsing is a local request preflight. Complete it before an
        // attempt exists so invalid configuration cannot project provider lifecycle.
        let headers = self.request_headers(request)?;
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
            let request_builder = client
                .post(endpoint_url.clone())
                .headers(headers.clone())
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
                Ok(response) => {
                    trace.phase(ProviderPhase::HeadersReceived)?;
                    if response.status().is_success() {
                        return Ok(Some(response));
                    }
                    let stream_limits = request.provider_target().stream_limits();
                    let Some(failure) = parse_response_failure_until_cancelled(
                        response,
                        cancel,
                        deadlines.stream_idle_timeout_ms,
                        stream_limits.max_duration_ms,
                    )
                    .await
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
    failure_origin: Option<ProviderTraceFailureOrigin>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderTraceFailureOrigin {
    EventProjection,
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
            failure_origin: None,
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
        self.project_provider_phase(ProviderPhaseEvent {
            request_id: self.request_id.clone(),
            endpoint: self.endpoint.clone(),
            phase,
            attempt: self.attempt,
            elapsed_ms: self.elapsed_ms(),
            terminal_status: None,
            usage: None,
            failure: None,
        })
    }

    fn project_model_event(&mut self, event: LlmEvent) -> Result<(), LlmError> {
        let result = self.inner.push(event);
        self.record_projection_result(result)
    }

    fn project_provider_phase(&mut self, event: ProviderPhaseEvent) -> Result<(), LlmError> {
        let result = self.inner.provider_phase(event);
        self.record_projection_result(result)
    }

    fn record_projection_result(&mut self, result: Result<(), LlmError>) -> Result<(), LlmError> {
        if result.is_err() {
            self.failure_origin = Some(ProviderTraceFailureOrigin::EventProjection);
        }
        result
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
        if self.attempt == 0 {
            return result;
        }
        let result = match result {
            Err(source)
                if self.failure_origin == Some(ProviderTraceFailureOrigin::EventProjection) =>
            {
                return Err(self.normalize_failure(source));
            }
            other => other,
        };
        let (result, provider_failure) = match result {
            Ok(summary) => (Ok(summary), None),
            Err(source)
                if matches!(
                    &source,
                    LlmError::ProviderRequestLimitExceeded { .. }
                        | LlmError::ProviderRequestImage(_)
                        | LlmError::ProviderFailure { .. }
                ) =>
            {
                (Err(source), None)
            }
            Err(source) => {
                let failure = self.failure_for(&source);
                (
                    Err(LlmError::ProviderFailure {
                        failure: failure.clone(),
                        source: Box::new(source),
                    }),
                    Some(failure),
                )
            }
        };
        if let Some(elapsed_ms) = self.last_progress_elapsed_ms {
            self.current_phase = ProviderPhase::LastProgress;
            if let Err(source) = self.project_provider_phase(ProviderPhaseEvent {
                request_id: self.request_id.clone(),
                endpoint: self.endpoint.clone(),
                phase: ProviderPhase::LastProgress,
                attempt: self.attempt,
                elapsed_ms,
                terminal_status: None,
                usage: None,
                failure: None,
            }) {
                return Err(self.normalize_projection_failure(source, result.err()));
            }
        }

        match result {
            Ok(summary) => {
                let terminal_status = if summary.finish_reason == FinishReason::Cancelled {
                    ProviderTerminalStatus::Cancelled
                } else {
                    ProviderTerminalStatus::Completed
                };
                self.current_phase = ProviderPhase::ProviderTerminal;
                if let Err(source) = self.project_provider_phase(ProviderPhaseEvent {
                    request_id: self.request_id.clone(),
                    endpoint: self.endpoint.clone(),
                    phase: ProviderPhase::ProviderTerminal,
                    attempt: self.attempt,
                    elapsed_ms: self.elapsed_ms(),
                    terminal_status: Some(terminal_status),
                    usage: summary.usage.clone(),
                    failure: None,
                }) {
                    return Err(self.normalize_failure(source));
                }
                Ok(summary)
            }
            Err(source) => {
                let Some(failure) = provider_failure else {
                    return Err(source);
                };
                let usage = source.token_usage().cloned();
                self.current_phase = ProviderPhase::ProviderTerminal;
                if let Err(projection_source) = self.project_provider_phase(ProviderPhaseEvent {
                    request_id: self.request_id.clone(),
                    endpoint: self.endpoint.clone(),
                    phase: ProviderPhase::ProviderTerminal,
                    attempt: self.attempt,
                    elapsed_ms: self.elapsed_ms(),
                    terminal_status: Some(ProviderTerminalStatus::Failed),
                    usage,
                    failure: Some(failure),
                }) {
                    return Err(self.normalize_projection_failure(projection_source, Some(source)));
                }
                Err(source)
            }
        }
    }

    fn normalize_failure(&self, source: LlmError) -> LlmError {
        self.normalize_projection_failure(source, None)
    }

    fn normalize_projection_failure(
        &self,
        projection_source: LlmError,
        pending_provider_error: Option<LlmError>,
    ) -> LlmError {
        let failure = self.failure_for(&projection_source);
        LlmError::ProviderFailure {
            failure,
            source: Box::new(pending_provider_error.unwrap_or(projection_source)),
        }
    }

    fn failure_for(&self, source: &LlmError) -> ProviderFailure {
        let (kind, status, code) = if self.failure_origin
            == Some(ProviderTraceFailureOrigin::EventProjection)
        {
            (ProviderFailureKind::EventProjection, None, None)
        } else {
            match source {
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
                LlmError::ProviderGenerationFailed { code, .. } => {
                    (ProviderFailureKind::Generation, None, code.clone())
                }
                LlmError::IncompleteResponse { .. } => {
                    (ProviderFailureKind::Generation, None, None)
                }
                LlmError::Json(_) => (ProviderFailureKind::Decode, None, None),
                LlmError::Message(_) if self.current_phase == ProviderPhase::FirstProgress => {
                    (ProviderFailureKind::Protocol, None, None)
                }
                LlmError::Message(_) if self.current_phase == ProviderPhase::HeadersReceived => {
                    (ProviderFailureKind::Protocol, None, None)
                }
                _ => (ProviderFailureKind::Other, None, None),
            }
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
        self.project_model_event(event)
    }

    fn provider_phase(&mut self, event: ProviderPhaseEvent) -> Result<(), LlmError> {
        self.current_phase = event.phase;
        self.project_provider_phase(event)
    }
}

#[derive(Debug, Clone, Copy)]
struct OperationDeadline {
    deadline: Option<Instant>,
}

impl OperationDeadline {
    fn new(timeout_ms: u64) -> Self {
        Self {
            deadline: (timeout_ms > 0).then(|| Instant::now() + Duration::from_millis(timeout_ms)),
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

fn parse_response_failure_body(
    status: StatusCode,
    body: &[u8],
    body_was_truncated: bool,
) -> ResponseFailure {
    let body = String::from_utf8_lossy(body);
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
    ResponseFailure {
        status,
        code,
        param,
        message,
    }
}

async fn read_provider_failure_body_bounded(
    response: reqwest::Response,
    cancel: &CancellationToken,
    idle_timeout_ms: u64,
    max_duration_ms: u64,
) -> Option<(Vec<u8>, bool)> {
    let declared_length = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok());
    let mut stream = response.bytes_stream();
    let mut body = Vec::with_capacity(PROVIDER_FAILURE_BODY_LIMIT_BYTES.min(8 * 1024));
    let started_at = Instant::now();
    loop {
        let next = if let Some(timeout) =
            bounded_wait_timeout(started_at, idle_timeout_ms, max_duration_ms)
        {
            tokio::select! {
                _ = cancel.cancelled() => return None,
                result = tokio::time::timeout(timeout, stream.next()) => match result {
                    Ok(next) => next,
                    Err(_) => return Some((body, true)),
                },
            }
        } else {
            tokio::select! {
                _ = cancel.cancelled() => return None,
                next = stream.next() => next,
            }
        };
        let Some(chunk) = next else {
            return Some((body, false));
        };
        let Ok(chunk) = chunk else {
            return Some((body, true));
        };
        let remaining = PROVIDER_FAILURE_BODY_LIMIT_BYTES.saturating_sub(body.len());
        if remaining == 0 {
            return Some((body, true));
        }
        let retained = remaining.min(chunk.len());
        body.extend_from_slice(&chunk[..retained]);
        if retained < chunk.len() {
            return Some((body, true));
        }
        if declared_length == Some(body.len()) {
            return Some((body, false));
        }
        if body.len() == PROVIDER_FAILURE_BODY_LIMIT_BYTES {
            return Some((body, true));
        }
    }
}

async fn parse_response_failure_until_cancelled(
    response: reqwest::Response,
    cancel: &CancellationToken,
    idle_timeout_ms: u64,
    max_duration_ms: u64,
) -> Option<ResponseFailure> {
    let status = response.status();
    let (body, body_was_truncated) =
        read_provider_failure_body_bounded(response, cancel, idle_timeout_ms, max_duration_ms)
            .await?;
    Some(parse_response_failure_body(
        status,
        &body,
        body_was_truncated,
    ))
}

fn bounded_wait_timeout(
    started_at: Instant,
    idle_timeout_ms: u64,
    max_duration_ms: u64,
) -> Option<Duration> {
    let idle = (idle_timeout_ms > 0).then(|| Duration::from_millis(idle_timeout_ms));
    let absolute = (max_duration_ms > 0)
        .then(|| Duration::from_millis(max_duration_ms).saturating_sub(started_at.elapsed()));
    match (idle, absolute) {
        (Some(idle), Some(absolute)) => Some(idle.min(absolute)),
        (Some(idle), None) => Some(idle),
        (None, Some(absolute)) => Some(absolute),
        (None, None) => None,
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
        bounded_wait_timeout(
            self.started_at,
            idle_timeout_ms,
            self.limits.max_duration_ms,
        )
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

    fn record_projected_events(&mut self, events: &[LlmEvent]) -> Result<(), LlmError> {
        for event in events {
            match event {
                LlmEvent::ToolCallStart { call_id, .. } => {
                    self.record_tool_call(format!("responses:{call_id}"))?;
                }
                // Responses arguments are admitted by the accumulator before its
                // canonical per-call argument value is mutated. Recounting the
                // projected full value here would create a second budget owner.
                LlmEvent::ToolCallArgsDelta { .. } => {}
                LlmEvent::TextDelta(_)
                | LlmEvent::ReasoningSummaryDelta(_)
                | LlmEvent::Finished { .. } => {}
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct ProviderStreamBudgetStage {
    new_tool_calls: HashSet<String>,
    tool_argument_bytes: HashMap<String, u64>,
}

impl ProviderStreamBudgetStage {
    fn record_tool_call(
        &mut self,
        budget: &ProviderStreamBudget,
        key: String,
    ) -> Result<(), LlmError> {
        if budget.tool_calls.contains(&key) || self.new_tool_calls.contains(&key) {
            return Ok(());
        }
        let actual = (budget.tool_calls.len() as u64)
            .saturating_add(self.new_tool_calls.len() as u64)
            .saturating_add(1);
        ensure_stream_limit(
            ProviderStreamLimit::ToolCallCount,
            actual,
            budget.limits.max_tool_calls,
        )?;
        self.new_tool_calls.insert(key);
        Ok(())
    }

    fn record_tool_arguments(
        &mut self,
        budget: &ProviderStreamBudget,
        key: &str,
        delta_bytes: u64,
    ) -> Result<(), LlmError> {
        let prior = budget
            .tool_argument_bytes
            .get(key)
            .copied()
            .unwrap_or_default();
        let staged = self
            .tool_argument_bytes
            .get(key)
            .copied()
            .unwrap_or_default();
        let actual = prior.saturating_add(staged).saturating_add(delta_bytes);
        ensure_stream_limit(
            ProviderStreamLimit::ToolCallArgumentBytes,
            actual,
            budget.limits.max_tool_call_argument_bytes,
        )?;
        let total = self.tool_argument_bytes.entry(key.to_string()).or_default();
        *total = total.saturating_add(delta_bytes);
        Ok(())
    }

    fn commit(self, budget: &mut ProviderStreamBudget) {
        budget.tool_calls.extend(self.new_tool_calls);
        for (key, delta_bytes) in self.tool_argument_bytes {
            let total = budget.tool_argument_bytes.entry(key).or_default();
            *total = total.saturating_add(delta_bytes);
        }
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
struct ChatStreamAccumulator {
    finish_reason: Option<FinishReason>,
    tool_calls: HashMap<usize, PartialToolCall>,
    call_id_to_delta_index: HashMap<String, usize>,
}

struct ChatChunkUpdate {
    events: Vec<LlmEvent>,
    saw_terminal_signal: bool,
}

impl ChatStreamAccumulator {
    fn apply_chunk(
        &mut self,
        chunk: OpenAiChatChunk,
        stream_budget: &mut ProviderStreamBudget,
    ) -> Result<ChatChunkUpdate, LlmError> {
        let mut journal = ChatChunkJournal::new(self);
        let mut budget_stage = ProviderStreamBudgetStage::default();
        let result =
            self.apply_chunk_transaction(chunk, stream_budget, &mut budget_stage, &mut journal);
        match result {
            Ok(update) => {
                budget_stage.commit(stream_budget);
                Ok(update)
            }
            Err(error) => {
                journal.rollback(self);
                Err(error)
            }
        }
    }

    fn apply_chunk_transaction(
        &mut self,
        chunk: OpenAiChatChunk,
        stream_budget: &ProviderStreamBudget,
        budget_stage: &mut ProviderStreamBudgetStage,
        journal: &mut ChatChunkJournal,
    ) -> Result<ChatChunkUpdate, LlmError> {
        let mut events = Vec::new();
        let mut saw_terminal_signal = false;
        if chunk.choices.len() > 1 {
            return Err(LlmError::Message(format!(
                "openai-compatible stream returned {} choice entries in one chunk; moyAI admits at most one choice entry for index `0`",
                chunk.choices.len()
            )));
        }
        if !chunk.choices.is_empty() && self.finish_reason.is_some() {
            return Err(LlmError::Message(
                "openai-compatible stream returned a non-empty choices chunk after choice index `0` was terminal"
                    .to_string(),
            ));
        }
        for choice in chunk.choices {
            if choice.index != 0 {
                return Err(LlmError::Message(format!(
                    "openai-compatible stream returned unsupported choice index `{}`; moyAI admits exactly choice index `0`",
                    choice.index
                )));
            }
            if let Some(value) = choice.delta.content {
                events.push(LlmEvent::TextDelta(value));
            }
            if let Some(deltas) = choice.delta.tool_calls {
                for delta in deltas {
                    journal.checkpoint_tool_call(self, delta.index);
                    self.apply_tool_call_delta(
                        delta,
                        stream_budget,
                        budget_stage,
                        journal,
                        &mut events,
                    )?;
                }
            }
            if let Some(value) = choice.finish_reason {
                let finish_reason = parse_finish_reason(&value)?;
                self.record_finish_reason(finish_reason)?;
                saw_terminal_signal = true;
            }
        }
        if saw_terminal_signal {
            let has_complete_tool_calls = validate_streamed_tool_calls(&self.tool_calls)?;
            resolve_finish_reason(self.finish_reason, has_complete_tool_calls)?;
        }
        Ok(ChatChunkUpdate {
            events,
            saw_terminal_signal,
        })
    }

    fn apply_tool_call_delta(
        &mut self,
        delta: crate::llm::dto::OpenAiToolCallDelta,
        stream_budget: &ProviderStreamBudget,
        budget_stage: &mut ProviderStreamBudgetStage,
        journal: &mut ChatChunkJournal,
        events: &mut Vec<LlmEvent>,
    ) -> Result<(), LlmError> {
        let delta_index = delta.index;
        let budget_key = format!("chat:{delta_index}");
        budget_stage.record_tool_call(stream_budget, budget_key.clone())?;
        if let Some(call_id) = delta.id {
            if let Some(inserted_alias) = self.bind_call_id(delta_index, call_id)? {
                journal.inserted_call_id_aliases.push(inserted_alias);
            }
        }
        let entry = self.tool_calls.entry(delta_index).or_default();
        if let Some(function) = delta.function {
            if let Some(name) = function.name {
                entry.record_tool_name(name, delta_index)?;
            }
            if let Some(arguments) = function.arguments {
                entry.saw_arguments_field = true;
                budget_stage.record_tool_arguments(
                    stream_budget,
                    &budget_key,
                    arguments.len() as u64,
                )?;
                entry.arguments.push_str(&arguments);
            }
        }
        if !entry.started {
            if let Some((call_id, tool_name)) = entry.identity() {
                events.push(LlmEvent::ToolCallStart { call_id, tool_name });
                entry.started = true;
            }
        }
        if entry.started && entry.emitted_len < entry.arguments.len() {
            events.push(LlmEvent::ToolCallArgsDelta {
                call_id: entry.call_id.clone().unwrap_or_default(),
                delta: entry.arguments_delta(),
            });
        }
        Ok(())
    }

    fn bind_call_id(
        &mut self,
        delta_index: usize,
        call_id: String,
    ) -> Result<Option<String>, LlmError> {
        if let Some(existing_index) = self.call_id_to_delta_index.get(&call_id)
            && *existing_index != delta_index
        {
            return Err(LlmError::Message(format!(
                "openai-compatible stream associated provider call id `{call_id}` with multiple tool delta indices (`{existing_index}` and `{delta_index}`)"
            )));
        }
        let alias_is_new = !self.call_id_to_delta_index.contains_key(&call_id);
        self.tool_calls
            .entry(delta_index)
            .or_default()
            .record_call_id(call_id.clone(), delta_index)?;
        if alias_is_new {
            self.call_id_to_delta_index
                .insert(call_id.clone(), delta_index);
            Ok(Some(call_id))
        } else {
            Ok(None)
        }
    }

    fn record_finish_reason(&mut self, finish_reason: FinishReason) -> Result<(), LlmError> {
        if let Some(existing) = self.finish_reason {
            if existing != finish_reason {
                return Err(LlmError::Message(format!(
                    "openai-compatible stream returned conflicting finish reasons `{existing:?}` and `{finish_reason:?}` for choice index `0`"
                )));
            }
            return Ok(());
        }
        self.finish_reason = Some(finish_reason);
        Ok(())
    }
}

struct ChatChunkJournal {
    original_finish_reason: Option<FinishReason>,
    tool_call_checkpoints: Vec<PartialToolCallCheckpoint>,
    inserted_call_id_aliases: Vec<String>,
}

impl ChatChunkJournal {
    fn new(accumulator: &ChatStreamAccumulator) -> Self {
        Self {
            original_finish_reason: accumulator.finish_reason,
            tool_call_checkpoints: Vec::new(),
            inserted_call_id_aliases: Vec::new(),
        }
    }

    fn checkpoint_tool_call(&mut self, accumulator: &ChatStreamAccumulator, delta_index: usize) {
        self.tool_call_checkpoints
            .push(PartialToolCallCheckpoint::capture(accumulator, delta_index));
    }

    fn rollback(self, accumulator: &mut ChatStreamAccumulator) {
        for alias in self.inserted_call_id_aliases.into_iter().rev() {
            accumulator.call_id_to_delta_index.remove(&alias);
        }
        for checkpoint in self.tool_call_checkpoints.into_iter().rev() {
            checkpoint.rollback(accumulator);
        }
        accumulator.finish_reason = self.original_finish_reason;
    }
}

struct PartialToolCallCheckpoint {
    delta_index: usize,
    existed: bool,
    call_id_was_none: bool,
    tool_name_was_none: bool,
    arguments_len: usize,
    saw_arguments_field: bool,
    emitted_len: usize,
    started: bool,
}

impl PartialToolCallCheckpoint {
    fn capture(accumulator: &ChatStreamAccumulator, delta_index: usize) -> Self {
        let Some(entry) = accumulator.tool_calls.get(&delta_index) else {
            return Self {
                delta_index,
                existed: false,
                call_id_was_none: true,
                tool_name_was_none: true,
                arguments_len: 0,
                saw_arguments_field: false,
                emitted_len: 0,
                started: false,
            };
        };
        Self {
            delta_index,
            existed: true,
            call_id_was_none: entry.call_id.is_none(),
            tool_name_was_none: entry.tool_name.is_none(),
            arguments_len: entry.arguments.len(),
            saw_arguments_field: entry.saw_arguments_field,
            emitted_len: entry.emitted_len,
            started: entry.started,
        }
    }

    fn rollback(self, accumulator: &mut ChatStreamAccumulator) {
        if !self.existed {
            accumulator.tool_calls.remove(&self.delta_index);
            return;
        }
        let Some(entry) = accumulator.tool_calls.get_mut(&self.delta_index) else {
            return;
        };
        if self.call_id_was_none {
            entry.call_id = None;
        }
        if self.tool_name_was_none {
            entry.tool_name = None;
        }
        entry.arguments.truncate(self.arguments_len);
        entry.saw_arguments_field = self.saw_arguments_field;
        entry.emitted_len = self.emitted_len;
        entry.started = self.started;
    }
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
        let call_id = self.call_id.as_ref()?;
        let tool_name = self.tool_name.as_ref()?;
        if call_id.trim().is_empty() || tool_name.trim().is_empty() {
            return None;
        }
        Some((call_id.clone(), tool_name.clone()))
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
            ModelMessage::System { content } | ModelMessage::Developer { content } => {
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
            ModelMessage::Agent { content } => OpenAiMessage {
                role: "user".to_string(),
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
            ModelMessage::System { .. } | ModelMessage::Developer { .. } => {
                unreachable!("instruction messages are merged above")
            }
        }
    }));
    let base = OpenAiChatRequest {
        model: request.provider_target().model().to_string(),
        stream: true,
        n: 1,
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
            | "n"
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

fn to_usage(value: &OpenAiUsage) -> Result<TokenUsage, LlmError> {
    let nested_reasoning_tokens = value
        .completion_tokens_details
        .as_ref()
        .and_then(|details| details.reasoning_tokens);
    let reasoning_tokens = match (value.reasoning_tokens, nested_reasoning_tokens) {
        (Some(legacy), Some(nested)) if legacy != nested => {
            return Err(LlmError::Message(format!(
                "openai-compatible usage contains conflicting reasoning token counts: reasoning_tokens={legacy}, completion_tokens_details.reasoning_tokens={nested}"
            )));
        }
        (Some(value), _) | (_, Some(value)) => Some(value),
        (None, None) => None,
    };
    Ok(TokenUsage {
        prompt_tokens: value.prompt_tokens,
        completion_tokens: value.completion_tokens,
        total_tokens: value.total_tokens,
        reasoning_tokens,
    })
}

fn same_token_usage(left: &TokenUsage, right: &TokenUsage) -> bool {
    left.prompt_tokens == right.prompt_tokens
        && left.completion_tokens == right.completion_tokens
        && left.total_tokens == right.total_tokens
        && left.reasoning_tokens == right.reasoning_tokens
}

fn summarize_stream_chunk(chunk: &str) -> String {
    compact_provider_text_bounded(
        chunk,
        PROVIDER_FAILURE_SUMMARY_LIMIT_BYTES,
        PROVIDER_STREAM_SUMMARY_SCAN_LIMIT_BYTES,
    )
}

fn summarize_stream_error(error: &OpenAiErrorPayload) -> String {
    let mut parts = Vec::with_capacity(3);
    if let Some(message) = error.message.as_ref() {
        let summary = compact_provider_text_bounded(
            message,
            PROVIDER_STREAM_ERROR_MESSAGE_LIMIT_BYTES,
            PROVIDER_STREAM_SUMMARY_SCAN_LIMIT_BYTES,
        );
        if !summary.is_empty() {
            parts.push(summary);
        }
    }
    if let Some(error_type) = error.error_type.as_ref() {
        let summary = compact_provider_text_bounded(
            error_type,
            PROVIDER_STREAM_ERROR_FIELD_LIMIT_BYTES,
            PROVIDER_STREAM_SUMMARY_SCAN_LIMIT_BYTES,
        );
        if !summary.is_empty() {
            parts.push(format!("type={summary}"));
        }
    }
    if let Some(code) = error.code.as_ref() {
        parts.push(summarize_stream_error_code(code));
    }
    if parts.is_empty() {
        "provider returned an unspecified stream error".to_string()
    } else {
        compact_provider_text_bounded(
            &parts.join(" | "),
            PROVIDER_FAILURE_SUMMARY_LIMIT_BYTES,
            PROVIDER_STREAM_SUMMARY_SCAN_LIMIT_BYTES,
        )
    }
}

fn summarize_stream_error_code(code: &Value) -> String {
    match code {
        Value::String(value) => format!(
            "code={}",
            compact_provider_text_bounded(
                value,
                PROVIDER_STREAM_ERROR_FIELD_LIMIT_BYTES,
                PROVIDER_STREAM_SUMMARY_SCAN_LIMIT_BYTES,
            )
        ),
        Value::Null => "code=null".to_string(),
        Value::Bool(value) => format!("code={value}"),
        Value::Number(value) => format!("code={value}"),
        Value::Array(_) => "code=<array>".to_string(),
        Value::Object(_) => "code=<object>".to_string(),
    }
}

fn compact_provider_text_bounded(text: &str, max_bytes: usize, max_scan_bytes: usize) -> String {
    if max_bytes == 0 {
        return String::new();
    }

    const ELLIPSIS: &str = "...";
    let content_limit = max_bytes.saturating_sub(ELLIPSIS.len());
    let mut compact = String::with_capacity(max_bytes);
    let mut pending_space = false;
    let mut scanned_end = 0usize;
    let mut was_truncated = false;

    for (offset, character) in text.char_indices() {
        let character_end = offset.saturating_add(character.len_utf8());
        if character_end > max_scan_bytes {
            was_truncated = true;
            break;
        }
        scanned_end = character_end;
        if character.is_whitespace() {
            pending_space = !compact.is_empty();
            continue;
        }

        let separator_len = usize::from(pending_space && !compact.is_empty());
        let required = separator_len.saturating_add(character.len_utf8());
        if compact.len().saturating_add(required) > content_limit {
            was_truncated = true;
            break;
        }
        if separator_len > 0 {
            compact.push(' ');
        }
        compact.push(character);
        pending_space = false;
    }

    was_truncated |= scanned_end < text.len();
    if was_truncated {
        if max_bytes < ELLIPSIS.len() {
            return ".".repeat(max_bytes);
        }
        compact.push_str(ELLIPSIS);
    }
    compact
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
    use axum::http::{StatusCode, header::CONTENT_TYPE};
    use axum::response::Response;
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::{Value, json};
    use tokio_util::sync::CancellationToken;

    use super::{
        ChatStreamAccumulator, OpenAiCompatClient, PartialToolCall, ProviderStreamBudget,
        ProviderStreamBudgetStage, parse_finish_reason, resolve_finish_reason, retry_delay_ms,
        to_openai_request, to_openai_request_with_reasoning, to_usage,
        validate_streamed_tool_calls,
    };
    use crate::config::model::{
        ChatCompletionsReasoningParameters, ProviderApiMode, ProviderReasoningCapability,
        ReasoningEffort, ReasoningSummary,
    };
    use crate::config::{
        ProviderDeadlines, ProviderMetadataMode, ProviderRequestLimits, ProviderStreamLimits,
        ProviderTarget, ResolvedConfig, ResolvedTurnConfig,
    };
    use crate::error::{LlmError, ProviderRequestLimit, ProviderStreamLimit};
    use crate::llm::contract::ReasoningRequest;
    use crate::llm::{
        ChatRequest, LlmClient, LlmEvent, LlmEventSink, ModelCapabilities, ModelMessage,
        ModelProfile, ModelToolCall, ProviderFailureKind, ProviderPhase, ProviderPhaseEvent,
        ProviderTerminalStatus, ProviderToolChoice, ToolSchema,
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
    fn chat_wire_keeps_the_real_user_anchor_before_the_latest_summary() {
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
        let summary = format!(
            "{}\nThe tests were inspected and the implementation remains pending.",
            include_str!("../../assets/prompts/compaction_summary_prefix.md").trim()
        );
        let request = ChatRequest::new(
            provider,
            model,
            "Base coding prompt".to_string(),
            vec![
                ModelMessage::Developer {
                    content: "<multi_agent_mode>proactive</multi_agent_mode>".to_string(),
                },
                ModelMessage::Developer {
                    content: "<sub_agent>Return verified evidence.</sub_agent>".to_string(),
                },
                ModelMessage::Agent {
                    content: "Message Type: NEW_TASK\nPayload:\nInspect the calculator."
                        .to_string(),
                },
                ModelMessage::User {
                    content: "Continue the calculator task.".to_string(),
                },
                ModelMessage::User {
                    content: summary.clone(),
                },
            ],
            Vec::new(),
            None,
            ProviderReasoningCapability::Unsupported,
            BTreeMap::new(),
        );

        let body = to_openai_request(&request).expect("compacted Chat wire");
        let messages = body["messages"].as_array().expect("messages array");

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0]["role"], json!("system"));
        assert_eq!(
            messages[0]["content"],
            json!(
                "Base coding prompt\n\n<multi_agent_mode>proactive</multi_agent_mode>\n\n<sub_agent>Return verified evidence.</sub_agent>"
            )
        );
        assert_eq!(messages[1]["role"], json!("user"));
        assert_eq!(
            messages[1]["content"],
            json!("Message Type: NEW_TASK\nPayload:\nInspect the calculator.")
        );
        assert_eq!(messages[2]["role"], json!("user"));
        assert_eq!(
            messages[2]["content"],
            json!("Continue the calculator task.")
        );
        assert_eq!(messages[3]["role"], json!("user"));
        assert_eq!(messages[3]["content"], json!(summary));
        assert!(
            !messages
                .iter()
                .any(|message| message["role"] == json!("developer"))
        );
    }

    #[test]
    fn canonical_turn_model_identity_reaches_chat_wire_unchanged() {
        let mut config = ResolvedConfig::default();
        config.model.model = "  canonical-wire-model  ".to_string();
        config.model.provider_api_mode = ProviderApiMode::ChatCompletions;
        let turn = ResolvedTurnConfig::capture(config).expect("canonical turn config");
        let profile = crate::llm::model_policy::ModelPolicy::from_config(turn.runtime_config())
            .transport_profile(turn.provider().metadata_mode());
        let request = ChatRequest::new(
            turn.provider().clone(),
            profile,
            "Canonical model fixture".to_string(),
            Vec::new(),
            Vec::new(),
            None,
            ProviderReasoningCapability::Unsupported,
            BTreeMap::new(),
        );

        let body = to_openai_request(&request).expect("canonical Chat wire");

        assert_eq!(turn.runtime_config().model.model, "canonical-wire-model");
        assert_eq!(turn.provider().model(), "canonical-wire-model");
        assert_eq!(request.model.name, "canonical-wire-model");
        assert_eq!(body["model"], json!("canonical-wire-model"));
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
    fn chat_request_forces_one_choice_and_extra_body_cannot_override_it() {
        let mut request = reasoning_fixture_request();
        request.extra_body = Some(json!({
            "n": 2,
            "num_ctx": 8192
        }));

        let body = to_openai_request(&request).expect("one-choice Chat request");

        assert_eq!(body["n"], 1);
        assert_eq!(body["num_ctx"], 8192);
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
    fn invalid_terminal_chat_chunks_restore_accumulator_and_budget_state() {
        let mut accumulator = ChatStreamAccumulator::default();
        let mut budget = ProviderStreamBudget::new(ProviderStreamLimits::product_default());
        let initial = serde_json::from_value(json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_0",
                        "function": { "name": "read_file", "arguments": "{}" }
                    }]
                },
                "finish_reason": null
            }]
        }))
        .expect("initial Chat tool chunk");
        accumulator
            .apply_chunk(initial, &mut budget)
            .expect("non-terminal tool delta");

        let invalid_stop = serde_json::from_value(json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "content": "must not escape",
                    "tool_calls": [{
                        "index": 0,
                        "function": { "arguments": "x" }
                    }]
                },
                "finish_reason": "stop"
            }]
        }))
        .expect("invalid stop terminal chunk");
        let Err(error) = accumulator.apply_chunk(invalid_stop, &mut budget) else {
            panic!("stop plus a tool payload must fail before commit");
        };
        assert!(error.to_string().contains("tool-call payload"));
        assert_eq!(accumulator.finish_reason, None);
        assert_eq!(accumulator.call_id_to_delta_index.get("call_0"), Some(&0));
        let entry = accumulator.tool_calls.get(&0).expect("prior tool call");
        assert_eq!(entry.call_id.as_deref(), Some("call_0"));
        assert_eq!(entry.tool_name.as_deref(), Some("read_file"));
        assert_eq!(entry.arguments, "{}");
        assert!(entry.saw_arguments_field);
        assert_eq!(entry.emitted_len, 2);
        assert!(entry.started);
        assert_eq!(budget.tool_calls.len(), 1);
        assert_eq!(budget.tool_argument_bytes.get("chat:0"), Some(&2));

        let mut incomplete_accumulator = ChatStreamAccumulator::default();
        let mut incomplete_budget =
            ProviderStreamBudget::new(ProviderStreamLimits::product_default());
        let invalid_tool_terminal = serde_json::from_value(json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "content": "must also stay staged",
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_incomplete",
                        "function": { "name": "read_file" }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }))
        .expect("incomplete tool terminal chunk");
        let Err(error) =
            incomplete_accumulator.apply_chunk(invalid_tool_terminal, &mut incomplete_budget)
        else {
            panic!("incomplete terminal tool call must fail before commit");
        };
        assert!(error.to_string().contains("without an arguments field"));
        assert_eq!(incomplete_accumulator.finish_reason, None);
        assert!(incomplete_accumulator.tool_calls.is_empty());
        assert!(incomplete_accumulator.call_id_to_delta_index.is_empty());
        assert!(incomplete_budget.tool_calls.is_empty());
        assert!(incomplete_budget.tool_argument_bytes.is_empty());
    }

    #[test]
    fn many_small_chat_argument_deltas_preserve_exact_events_arguments_and_budget() {
        const DELTA_COUNT: usize = 1_024;
        let mut limits = ProviderStreamLimits::product_default();
        limits.max_tool_call_argument_bytes = DELTA_COUNT as u64;
        let mut budget = ProviderStreamBudget::new(limits);
        let mut accumulator = ChatStreamAccumulator::default();
        let initial = serde_json::from_value(json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_0",
                        "function": { "name": "write", "arguments": "" }
                    }]
                },
                "finish_reason": null
            }]
        }))
        .expect("initial Chat tool delta");
        let initial_update = accumulator
            .apply_chunk(initial, &mut budget)
            .expect("initial tool identity and empty arguments");
        assert!(matches!(
            initial_update.events.as_slice(),
            [LlmEvent::ToolCallStart { call_id, tool_name }]
                if call_id == "call_0" && tool_name == "write"
        ));

        let mut projected_arguments = String::with_capacity(DELTA_COUNT);
        for _ in 0..DELTA_COUNT {
            let chunk = serde_json::from_value(json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "function": { "arguments": "x" }
                        }]
                    },
                    "finish_reason": null
                }]
            }))
            .expect("small Chat argument delta");
            let update = accumulator
                .apply_chunk(chunk, &mut budget)
                .expect("small argument delta remains admitted");
            assert!(matches!(
                update.events.as_slice(),
                [LlmEvent::ToolCallArgsDelta { call_id, delta }]
                    if call_id == "call_0" && delta == "x"
            ));
            projected_arguments.push('x');
        }

        let terminal = serde_json::from_value(json!({
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "tool_calls"
            }]
        }))
        .expect("terminal Chat tool chunk");
        let terminal_update = accumulator
            .apply_chunk(terminal, &mut budget)
            .expect("complete tool-call terminal");
        assert!(terminal_update.events.is_empty());
        assert!(terminal_update.saw_terminal_signal);

        let entry = accumulator
            .tool_calls
            .get(&0)
            .expect("accumulated tool call");
        assert_eq!(projected_arguments, "x".repeat(DELTA_COUNT));
        assert_eq!(entry.arguments, projected_arguments);
        assert_eq!(entry.emitted_len, DELTA_COUNT);
        assert_eq!(
            budget.tool_argument_bytes.get("chat:0"),
            Some(&(DELTA_COUNT as u64))
        );
        assert_eq!(budget.tool_calls.len(), 1);
    }

    #[test]
    fn chat_usage_reads_nested_reasoning_tokens_and_reconciles_legacy_exactly() {
        for usage_json in [
            json!({
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15,
                "completion_tokens_details": { "reasoning_tokens": 3 }
            }),
            json!({
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15,
                "reasoning_tokens": 3,
                "completion_tokens_details": { "reasoning_tokens": 3 }
            }),
        ] {
            let chunk = serde_json::from_value::<crate::llm::dto::OpenAiChatChunk>(json!({
                "choices": [],
                "usage": usage_json
            }))
            .expect("typed Chat usage");
            let usage = to_usage(chunk.usage.as_ref().expect("usage payload"))
                .expect("matching reasoning usage");
            assert_eq!(usage.reasoning_tokens, Some(3));
        }

        let conflicting = serde_json::from_value::<crate::llm::dto::OpenAiChatChunk>(json!({
            "choices": [],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15,
                "reasoning_tokens": 2,
                "completion_tokens_details": { "reasoning_tokens": 3 }
            }
        }))
        .expect("individually typed conflicting usage values");
        let error = to_usage(conflicting.usage.as_ref().expect("usage payload"))
            .expect_err("legacy and nested reasoning usage must not diverge");
        assert!(error.to_string().contains("conflicting reasoning token"));
    }

    #[test]
    fn chat_usage_rejects_malformed_nested_completion_details() {
        for completion_tokens_details in [
            Value::Null,
            json!([]),
            json!({ "reasoning_tokens": "3" }),
            json!({ "reasoning_tokens": -1 }),
        ] {
            let result = serde_json::from_value::<crate::llm::dto::OpenAiChatChunk>(json!({
                "choices": [],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 5,
                    "total_tokens": 15,
                    "completion_tokens_details": completion_tokens_details
                }
            }));
            assert!(
                result.is_err(),
                "malformed completion_tokens_details must fail closed"
            );
        }
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
    fn chat_tool_identity_preserves_exact_provider_strings_and_only_trims_for_emptiness() {
        let exact = PartialToolCall {
            call_id: Some(" call_raw ".to_string()),
            tool_name: Some("\tread_file\n".to_string()),
            ..PartialToolCall::default()
        };
        assert_eq!(
            exact.identity(),
            Some((" call_raw ".to_string(), "\tread_file\n".to_string()))
        );

        for blank in [
            PartialToolCall {
                call_id: Some(" \t".to_string()),
                tool_name: Some("read_file".to_string()),
                ..PartialToolCall::default()
            },
            PartialToolCall {
                call_id: Some("call_raw".to_string()),
                tool_name: Some("\r\n".to_string()),
                ..PartialToolCall::default()
            },
        ] {
            assert!(blank.identity().is_none());
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

    #[test]
    fn stream_error_summaries_bound_scan_and_allocation() {
        let late_marker = "must-not-be-scanned";
        let whitespace_prefix = " ".repeat(super::PROVIDER_STREAM_SUMMARY_SCAN_LIMIT_BYTES + 32);
        let chunk = format!("{whitespace_prefix}{late_marker}");

        let chunk_summary = super::summarize_stream_chunk(&chunk);

        assert_eq!(chunk_summary, "...");
        assert!(!chunk_summary.contains(late_marker));
        assert!(chunk_summary.len() <= super::PROVIDER_FAILURE_SUMMARY_LIMIT_BYTES);

        let error = crate::llm::dto::OpenAiErrorPayload {
            message: Some(format!(
                "provider message {} {late_marker}",
                "x".repeat(super::PROVIDER_STREAM_SUMMARY_SCAN_LIMIT_BYTES + 32)
            )),
            error_type: Some("y".repeat(super::PROVIDER_STREAM_SUMMARY_SCAN_LIMIT_BYTES + 32)),
            code: Some(json!({ "large": "z".repeat(4_096) })),
        };

        let error_summary = super::summarize_stream_error(&error);

        assert!(error_summary.len() <= super::PROVIDER_FAILURE_SUMMARY_LIMIT_BYTES);
        assert!(error_summary.contains("provider message"));
        assert!(!error_summary.contains(late_marker));
        assert!(error_summary.contains("code=<object>"));
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
    async fn local_preflight_failure_emits_no_provider_lifecycle() {
        let (base_url, requests, server) = start_responses_fixture(Vec::new()).await;
        let mut request = responses_fixture_request(
            &base_url,
            vec![ModelMessage::User {
                content: "Reject the invalid local request".to_string(),
            }],
        );
        request.tool_choice = Some(ProviderToolChoice::Required);
        let client = OpenAiCompatClient::new(None);
        let mut sink = RecordingLlmEventSink::default();

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("tool choice without tools must fail before transport");
        server.abort();

        assert!(error.provider_failure().is_none());
        assert!(requests.lock().expect("request capture").is_empty());
        assert!(sink.phases.is_empty());
        assert!(sink.events.is_empty());
    }

    #[tokio::test]
    async fn invalid_local_headers_emit_no_provider_lifecycle() {
        let (base_url, requests, server) = start_responses_fixture(Vec::new()).await;
        let mut request = responses_fixture_request(
            &base_url,
            vec![ModelMessage::User {
                content: "Reject the invalid local header".to_string(),
            }],
        );
        request.replace_extra_headers(BTreeMap::from([(
            "invalid header name".to_string(),
            "value".to_string(),
        )]));
        let client = OpenAiCompatClient::new(None);
        let mut sink = RecordingLlmEventSink::default();

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("invalid local header must fail before transport");
        server.abort();

        assert!(error.provider_failure().is_none());
        assert!(error.to_string().contains("invalid header name"));
        assert!(requests.lock().expect("request capture").is_empty());
        assert!(sink.phases.is_empty());
        assert!(sink.events.is_empty());
    }

    #[tokio::test]
    async fn stale_model_profile_emits_no_provider_lifecycle_or_post() {
        let (base_url, requests, server) = start_responses_fixture(Vec::new()).await;
        let mut request = responses_fixture_request(
            &base_url,
            vec![ModelMessage::User {
                content: "Reject the stale model profile".to_string(),
            }],
        );
        request.model.name = "stale-profile-model".to_string();
        let client = OpenAiCompatClient::new(None);
        let mut sink = RecordingLlmEventSink::default();

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("stale model profile must fail before transport");
        server.abort();

        assert!(error.provider_failure().is_none());
        assert!(
            error
                .to_string()
                .contains("canonical provider target model")
        );
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
    async fn chat_model_event_sink_failure_is_typed_and_stops_later_projection() {
        let response = [
            format!(
                "data: {}\n\n",
                json!({
                    "choices": [{
                        "index": 0,
                        "delta": { "content": "first" },
                        "finish_reason": null
                    }]
                })
            ),
            format!(
                "data: {}\n\n",
                json!({
                    "choices": [{
                        "index": 0,
                        "delta": { "content": "must not project" },
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
        let mut sink = FailingLlmEventSink::fail_push_on(1);

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("the first model event projection must fail the request");
        server.abort();

        let failure = error.provider_failure().expect("typed projection failure");
        assert_eq!(failure.kind, ProviderFailureKind::EventProjection);
        assert_eq!(failure.phase, ProviderPhase::FirstProgress);
        assert!(!failure.request_id.as_str().is_empty());
        assert!(matches!(
            sink.attempted_events.as_slice(),
            [LlmEvent::TextDelta(text)] if text == "first"
        ));
        assert!(
            !sink.phases.iter().any(|event| matches!(
                event.phase,
                ProviderPhase::LastProgress | ProviderPhase::ProviderTerminal
            )),
            "a failed model-event sink must not receive follow-up lifecycle projection"
        );
    }

    #[tokio::test]
    async fn chat_phase_sink_failure_is_typed_before_model_projection() {
        let response = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({
                "choices": [{
                    "index": 0,
                    "delta": { "content": "must not project" },
                    "finish_reason": "stop"
                }]
            })
        );
        let (base_url, _requests, server) = start_responses_fixture(vec![response]).await;
        let mut request = reasoning_fixture_request();
        replace_provider_endpoint(&mut request, &base_url);
        let client = OpenAiCompatClient::new(None);
        let mut sink = FailingLlmEventSink::fail_phase(ProviderPhase::FirstProgress);

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("first-progress projection must fail the request");
        server.abort();

        let failure = error
            .provider_failure()
            .expect("typed phase projection failure");
        assert_eq!(failure.kind, ProviderFailureKind::EventProjection);
        assert_eq!(failure.phase, ProviderPhase::FirstProgress);
        assert!(!failure.request_id.as_str().is_empty());
        assert!(sink.attempted_events.is_empty());
        assert_eq!(
            sink.phases
                .iter()
                .filter(|event| event.phase == ProviderPhase::FirstProgress)
                .count(),
            1
        );
        assert!(
            !sink.phases.iter().any(|event| matches!(
                event.phase,
                ProviderPhase::LastProgress | ProviderPhase::ProviderTerminal
            )),
            "a failed phase sink must not be used for terminal failure projection"
        );
    }

    #[tokio::test]
    async fn chat_success_terminal_phase_sink_failure_is_event_projection_not_provider_other() {
        let response = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({
                "choices": [{
                    "index": 0,
                    "delta": { "content": "complete" },
                    "finish_reason": "stop"
                }]
            })
        );
        let (base_url, _requests, server) = start_responses_fixture(vec![response]).await;
        let mut request = reasoning_fixture_request();
        replace_provider_endpoint(&mut request, &base_url);
        let client = OpenAiCompatClient::new(None);
        let mut sink = FailingLlmEventSink::fail_phase(ProviderPhase::ProviderTerminal);

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("terminal lifecycle projection must remain a typed local failure");
        server.abort();

        let LlmError::ProviderFailure { failure, source } = error else {
            panic!("terminal projection failure must remain typed");
        };
        assert_eq!(failure.kind, ProviderFailureKind::EventProjection);
        assert_eq!(failure.phase, ProviderPhase::ProviderTerminal);
        assert!(!failure.request_id.as_str().is_empty());
        assert_eq!(failure.message, "fixture provider-phase projection failed");
        assert!(matches!(
            *source,
            LlmError::Message(message)
                if message == "fixture provider-phase projection failed"
        ));
        assert!(matches!(
            sink.attempted_events.as_slice(),
            [
                LlmEvent::TextDelta(text),
                LlmEvent::Finished {
                    finish_reason: FinishReason::Stop,
                    ..
                }
            ] if text == "complete"
        ));
        assert_eq!(
            sink.phases
                .iter()
                .filter(|event| event.phase == ProviderPhase::ProviderTerminal)
                .count(),
            1,
            "the failed terminal projection must not be retried on the same sink"
        );
    }

    #[tokio::test]
    async fn chat_provider_semantic_failure_survives_lifecycle_projection_failure() {
        const SEMANTIC_MESSAGE: &str = "openai-compatible stream returned unsupported choice index `1`; moyAI admits exactly choice index `0`";
        let response = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({
                "choices": [{
                    "index": 1,
                    "delta": { "content": "must not project" },
                    "finish_reason": "stop"
                }]
            })
        );
        let (base_url, _requests, server) =
            start_responses_fixture(vec![response.clone(), response]).await;
        let client = OpenAiCompatClient::new(None);

        for projection_phase in [ProviderPhase::LastProgress, ProviderPhase::ProviderTerminal] {
            let mut request = reasoning_fixture_request();
            replace_provider_endpoint(&mut request, &base_url);
            let mut sink = FailingLlmEventSink::fail_phase(projection_phase);

            let error = client
                .stream_chat(request, CancellationToken::new(), &mut sink)
                .await
                .expect_err("provider and lifecycle projection failures must both be retained");

            let LlmError::ProviderFailure {
                failure: projection_failure,
                source: provider_source,
            } = error
            else {
                panic!("projection failure must be the outer typed failure");
            };
            assert_eq!(
                projection_failure.kind,
                ProviderFailureKind::EventProjection
            );
            assert_eq!(projection_failure.phase, projection_phase);
            assert_eq!(
                projection_failure.message,
                "fixture provider-phase projection failed"
            );
            assert!(!projection_failure.request_id.as_str().is_empty());

            let LlmError::ProviderFailure {
                failure: provider_failure,
                source: semantic_source,
            } = *provider_source
            else {
                panic!("original provider failure must be nested below projection failure");
            };
            assert_eq!(provider_failure.kind, ProviderFailureKind::Protocol);
            assert_eq!(provider_failure.phase, ProviderPhase::FirstProgress);
            assert_eq!(provider_failure.message, SEMANTIC_MESSAGE);
            assert_eq!(
                provider_failure.request_id.as_str(),
                projection_failure.request_id.as_str()
            );
            assert!(matches!(
                *semantic_source,
                LlmError::Message(message) if message == SEMANTIC_MESSAGE
            ));
            assert!(sink.attempted_events.is_empty());
            assert_eq!(
                sink.phases
                    .iter()
                    .filter(|event| event.phase == projection_phase)
                    .count(),
                1,
                "the failed lifecycle projection must not be retried"
            );
            assert_eq!(
                sink.phases.last().map(|event| event.phase),
                Some(projection_phase),
                "a broken sink must not receive later lifecycle projection"
            );
        }
        server.abort();
    }

    #[tokio::test]
    async fn chat_rejects_nonzero_choice_without_projection() {
        let response = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({
                "choices": [{
                    "index": 1,
                    "delta": { "content": "alternate" },
                    "finish_reason": "stop"
                }]
            })
        );
        let (base_url, _requests, server) = start_responses_fixture(vec![response]).await;
        let mut request = reasoning_fixture_request();
        replace_provider_endpoint(&mut request, &base_url);
        let client = OpenAiCompatClient::new(None);
        let mut sink = RecordingLlmEventSink::default();

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("choice index 1 must fail closed");
        server.abort();

        assert!(error.to_string().contains("choice index `1`"));
        assert_eq!(
            error.provider_failure().map(|failure| failure.kind),
            Some(ProviderFailureKind::Protocol),
            "provider semantic errors remain protocol failures when the sink succeeds"
        );
        assert!(sink.events.is_empty());
    }

    #[tokio::test]
    async fn chat_rejects_duplicate_choice_zero_entries_without_projecting_the_first() {
        let response = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({
                "choices": [
                    {
                        "index": 0,
                        "delta": { "content": "must roll back" },
                        "finish_reason": null
                    },
                    {
                        "index": 0,
                        "delta": { "content": "duplicate choice" },
                        "finish_reason": "stop"
                    }
                ]
            })
        );
        let (base_url, _requests, server) = start_responses_fixture(vec![response]).await;
        let mut request = reasoning_fixture_request();
        replace_provider_endpoint(&mut request, &base_url);
        let client = OpenAiCompatClient::new(None);
        let mut sink = RecordingLlmEventSink::default();

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("duplicate choice zero entries must fail closed");
        server.abort();

        assert!(error.to_string().contains("2 choice entries"));
        assert!(sink.events.is_empty());
    }

    #[tokio::test]
    async fn chat_rejects_nonempty_choice_chunk_after_finish_without_projecting_late_content() {
        let response = [
            format!(
                "data: {}\n\n",
                json!({
                    "choices": [{
                        "index": 0,
                        "delta": { "content": "complete" },
                        "finish_reason": "stop"
                    }]
                })
            ),
            format!(
                "data: {}\n\n",
                json!({
                    "choices": [{
                        "index": 0,
                        "delta": { "content": "late" },
                        "finish_reason": null
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

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("choice content after finish_reason must fail closed");
        server.abort();

        assert!(
            error
                .to_string()
                .contains("after choice index `0` was terminal")
        );
        assert!(matches!(
            sink.events.as_slice(),
            [LlmEvent::TextDelta(text)] if text == "complete"
        ));
    }

    #[tokio::test]
    async fn chat_allows_exact_usage_repetition_in_empty_chunk_after_terminal() {
        let usage = json!({
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15,
            "completion_tokens_details": { "reasoning_tokens": 3 }
        });
        let response = [
            format!(
                "data: {}\n\n",
                json!({
                    "choices": [{
                        "index": 0,
                        "delta": { "content": "complete" },
                        "finish_reason": "stop"
                    }],
                    "usage": usage.clone()
                })
            ),
            format!(
                "data: {}\n\n",
                json!({
                    "choices": [],
                    "usage": usage
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
            .expect("an exact usage-only repeat remains valid after finish_reason");
        server.abort();

        assert!(matches!(
            summary.usage,
            Some(TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                reasoning_tokens: Some(3),
            })
        ));
        assert!(matches!(
            sink.events.as_slice(),
            [
                LlmEvent::TextDelta(text),
                LlmEvent::Finished {
                    finish_reason: FinishReason::Stop,
                    ..
                }
            ] if text == "complete"
        ));
    }

    #[tokio::test]
    async fn chat_rejects_conflicting_usage_repeat_without_projecting_finished() {
        let response = [
            format!(
                "data: {}\n\n",
                json!({
                    "choices": [{
                        "index": 0,
                        "delta": { "content": "complete" },
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 5,
                        "total_tokens": 15
                    }
                })
            ),
            format!(
                "data: {}\n\n",
                json!({
                    "choices": [],
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 6,
                        "total_tokens": 16
                    }
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

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("usage changes across chunks must fail closed");
        server.abort();

        assert!(error.to_string().contains("conflicting usage payloads"));
        assert!(matches!(
            sink.events.as_slice(),
            [LlmEvent::TextDelta(text)] if text == "complete"
        ));
    }

    #[tokio::test]
    async fn chat_rejects_call_id_alias_across_delta_indices_atomically() {
        let response = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [
                            {
                                "index": 0,
                                "id": " call_raw ",
                                "function": { "name": " read_file ", "arguments": "{}" }
                            },
                            {
                                "index": 1,
                                "id": " call_raw ",
                                "function": { "name": " write_file ", "arguments": "{}" }
                            }
                        ]
                    },
                    "finish_reason": "tool_calls"
                }]
            })
        );
        let (base_url, _requests, server) = start_responses_fixture(vec![response]).await;
        let mut request = reasoning_fixture_request();
        replace_provider_endpoint(&mut request, &base_url);
        let client = OpenAiCompatClient::new(None);
        let mut sink = RecordingLlmEventSink::default();

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("one provider call id cannot alias two delta indices");
        server.abort();

        assert!(error.to_string().contains("multiple tool delta indices"));
        assert!(sink.events.is_empty());
    }

    #[tokio::test]
    async fn chat_semantic_error_rolls_back_all_events_from_the_same_chunk() {
        let response = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [
                            {
                                "index": 0,
                                "id": "call_first",
                                "function": { "name": "read_file", "arguments": "{}" }
                            },
                            {
                                "index": 0,
                                "id": "call_changed",
                                "function": { "arguments": "" }
                            }
                        ]
                    },
                    "finish_reason": null
                }]
            })
        );
        let (base_url, _requests, server) = start_responses_fixture(vec![response]).await;
        let mut request = reasoning_fixture_request();
        replace_provider_endpoint(&mut request, &base_url);
        let client = OpenAiCompatClient::new(None);
        let mut sink = RecordingLlmEventSink::default();

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("a conflicting delta must reject its whole parsed chunk");
        server.abort();

        assert!(error.to_string().contains("changed the call id"));
        assert!(sink.events.is_empty());
    }

    #[tokio::test]
    async fn chat_rejects_stop_with_same_chunk_tool_payload_without_projection() {
        let response = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "content": "must remain staged",
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_0",
                            "function": { "name": "read_file", "arguments": "{}" }
                        }]
                    },
                    "finish_reason": "stop"
                }]
            })
        );
        let (base_url, _requests, server) = start_responses_fixture(vec![response]).await;
        let mut request = reasoning_fixture_request();
        replace_provider_endpoint(&mut request, &base_url);
        let client = OpenAiCompatClient::new(None);
        let mut sink = RecordingLlmEventSink::default();

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("stop with a tool payload must fail before projection");
        server.abort();

        assert!(error.to_string().contains("tool-call payload"));
        assert!(sink.events.is_empty());
    }

    #[tokio::test]
    async fn chat_rejects_incomplete_terminal_tool_call_without_projection() {
        let response = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "content": "must remain staged",
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_0",
                            "function": { "name": "read_file" }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })
        );
        let (base_url, _requests, server) = start_responses_fixture(vec![response]).await;
        let mut request = reasoning_fixture_request();
        replace_provider_endpoint(&mut request, &base_url);
        let client = OpenAiCompatClient::new(None);
        let mut sink = RecordingLlmEventSink::default();

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("incomplete terminal tool call must fail before projection");
        server.abort();

        assert!(error.to_string().contains("without an arguments field"));
        assert!(sink.events.is_empty());
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
                "output_index": 0,
                "delta": "The change is ready."
            }),
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "The change is ready."
                    }]
                }
            }),
            json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_text_1",
                    "output": [{
                        "type": "message",
                        "id": "msg_1",
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": "The change is ready."
                        }]
                    }],
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
                ModelMessage::Developer {
                    content: "Root delegation plan".to_string(),
                },
                ModelMessage::Developer {
                    content: "Proactive delegation mode".to_string(),
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
        assert_eq!(
            sink.phases.last().and_then(|event| event.usage.as_ref()),
            summary.usage.as_ref()
        );

        let captured = requests.lock().expect("Responses request capture");
        assert_eq!(captured.len(), 1);
        let wire = &captured[0];
        assert_eq!(wire["model"], json!("responses-fixture-model"));
        assert_eq!(
            wire["instructions"],
            json!(
                "Responses fixture instructions\n\nRepository policy\n\nRoot delegation plan\n\nProactive delegation mode"
            )
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
        assert_eq!(wire["store"], json!(false));
        assert_eq!(wire["stream"], json!(true));
        assert!(wire.get("messages").is_none());
        assert!(wire.get("previous_response_id").is_none());
    }

    #[tokio::test]
    async fn responses_http_transport_replays_complete_canonical_input_without_cursor() {
        let first_response = responses_sse([
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
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
                "response": {
                    "id": "resp_tool_1",
                    "output": [{
                        "type": "function_call",
                        "id": "fc_1",
                        "call_id": "call_1",
                        "name": "read_file",
                        "arguments": "{\"path\":\"README.md\"}"
                    }]
                }
            }),
        ]);
        let second_response = responses_sse([
            json!({
                "type": "response.output_text.delta",
                "item_id": "msg_2",
                "output_index": 0,
                "delta": "README inspected."
            }),
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_2",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": "README inspected."
                    }]
                }
            }),
            json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_text_2",
                    "output": [{
                        "type": "message",
                        "id": "msg_2",
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": "README inspected."
                        }]
                    }]
                }
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
        let mut second_sink = RecordingLlmEventSink::default();

        let second_summary = client
            .stream_chat(second_request, CancellationToken::new(), &mut second_sink)
            .await
            .expect("full-history Responses stream");
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
        assert!(captured[1].get("previous_response_id").is_none());
        assert_eq!(captured[1]["num_ctx"], json!(131_072));
        assert_eq!(
            captured[1]["input"],
            json!([{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "Inspect README.md" }]
            }, {
                "type": "function_call",
                "call_id": "call_1",
                "name": "read_file",
                "arguments": "{\"path\":\"README.md\"}"
            }, {
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
                "output_index": 0,
                "delta": "Recovered."
            }),
            json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": "msg_after_retry",
                    "role": "assistant",
                    "content": [{ "type": "output_text", "text": "Recovered." }]
                }
            }),
            json!({
                "type": "response.completed",
                "response": {
                    "id": "resp_recovered",
                    "output": [{
                        "type": "message",
                        "id": "msg_after_retry",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": "Recovered." }]
                    }]
                }
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
        let failure = error.provider_failure().expect("typed generation failure");
        assert_eq!(failure.kind, ProviderFailureKind::Generation);
        assert_eq!(failure.phase, ProviderPhase::FirstProgress);
        assert_eq!(failure.status, None);
        assert_eq!(failure.code.as_deref(), Some("server_error"));
        assert_eq!(requests.lock().expect("request capture").len(), 1);
        assert!(sink.events.is_empty());
    }

    #[tokio::test]
    async fn responses_lm_studio_tool_parse_failure_is_generation_terminal() {
        let failed = responses_sse([
            json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "fc_truncated",
                "delta": "{\"path\":\"docs/calculator-design.md\",\"content\":"
            }),
            json!({
                "type": "response.failed",
                "response": {
                    "id": "resp_tool_parse_failed",
                    "error": {
                        "code": "unknown",
                        "message": "Failed to parse tool call: Unexpected end of content."
                    }
                }
            }),
        ]);
        let (base_url, requests, server) = start_responses_fixture(vec![failed]).await;
        let mut request = responses_fixture_request(
            &base_url,
            vec![ModelMessage::User {
                content: "Rewrite the design document".to_string(),
            }],
        );
        request.model.max_output_tokens = 2_048;
        let expected_max_output_tokens = 2_048;
        let client = OpenAiCompatClient::new(None);
        let mut sink = RecordingLlmEventSink::default();

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("LM Studio generation failure must remain terminal");
        server.abort();

        let failure = error.provider_failure().expect("typed generation failure");
        assert_eq!(failure.kind, ProviderFailureKind::Generation);
        assert_eq!(failure.phase, ProviderPhase::FirstProgress);
        assert_eq!(failure.status, None);
        assert_eq!(failure.code.as_deref(), Some("unknown"));
        assert!(
            error
                .to_string()
                .contains("configured max_output_tokens=2048")
        );
        assert!(matches!(
            &error,
            LlmError::ProviderFailure { source, .. }
                if matches!(
                    source.as_ref(),
                    LlmError::ProviderGenerationFailed {
                        response_id: Some(response_id),
                        code: Some(code),
                        message,
                        max_output_tokens,
                    } if response_id == "resp_tool_parse_failed"
                        && code == "unknown"
                        && message == "Failed to parse tool call: Unexpected end of content."
                        && *max_output_tokens == expected_max_output_tokens
                )
        ));
        let requests = requests.lock().expect("request capture");
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]["max_output_tokens"],
            json!(expected_max_output_tokens)
        );
        assert!(sink.events.is_empty());
        assert!(matches!(
            sink.phases.last(),
            Some(ProviderPhaseEvent {
                phase: ProviderPhase::ProviderTerminal,
                terminal_status: Some(ProviderTerminalStatus::Failed),
                failure: Some(terminal_failure),
                ..
            }) if terminal_failure.kind == ProviderFailureKind::Generation
                && terminal_failure.status.is_none()
                && terminal_failure.code.as_deref() == Some("unknown")
        ));
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
        assert_eq!(failure.phase, ProviderPhase::HeadersReceived);
        assert!(failure.message.len() < 1_024);
        assert!(failure.message.ends_with("..."));
        assert_eq!(requests.lock().expect("request capture").len(), 1);
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

    #[tokio::test]
    async fn stalled_http_failure_body_uses_post_header_budget_not_response_start_timeout() {
        let (base_url, request_count, server) = start_delayed_fixture_with_status(
            "late failure details".to_string(),
            Duration::from_secs(1),
            false,
            StatusCode::INTERNAL_SERVER_ERROR,
        )
        .await;
        let mut request = responses_fixture_request(
            &base_url,
            vec![ModelMessage::User {
                content: "Classify the received HTTP failure".to_string(),
            }],
        );
        replace_provider_deadlines(
            &mut request,
            ProviderDeadlines {
                response_start_timeout_ms: 500,
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
            .expect_err("received HTTP failure must remain a status rejection");
        server.abort();

        let failure = error.provider_failure().expect("typed provider failure");
        assert_eq!(failure.kind, ProviderFailureKind::HttpStatus);
        assert_eq!(failure.phase, ProviderPhase::HeadersReceived);
        assert_eq!(failure.status, Some(500));
        assert!(!error.to_string().contains("response-start deadline"));
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
        let mut stage = ProviderStreamBudgetStage::default();
        stage
            .record_tool_arguments(&budget, "call_1", 4)
            .expect("argument boundary");
        assert!(matches!(
            stage.record_tool_arguments(&budget, "call_1", 1),
            Err(LlmError::ProviderStreamLimitExceeded {
                surface: ProviderStreamLimit::ToolCallArgumentBytes,
                ..
            })
        ));
        stage.commit(&mut budget);
        assert_eq!(budget.tool_argument_bytes.get("call_1"), Some(&4));
    }

    #[tokio::test]
    async fn raw_stream_byte_limit_terminates_once_without_reposting() {
        let response = responses_sse([json!({
            "type": "response.output_text.delta",
            "item_id": "large_event",
            "output_index": 0,
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
    async fn responses_argument_limit_is_typed_before_projection_without_reposting() {
        let response = responses_sse([json!({
            "type": "response.function_call_arguments.delta",
            "item_id": "fc_oversized",
            "delta": "12345"
        })]);
        let (base_url, requests, server) = start_responses_fixture(vec![response]).await;
        let mut request = responses_fixture_request(
            &base_url,
            vec![ModelMessage::User {
                content: "bound tool arguments".to_string(),
            }],
        );
        let mut provider = request.provider_target().clone();
        let mut limits = ProviderStreamLimits::product_default();
        limits.max_tool_call_argument_bytes = 4;
        provider.replace_stream_limits(limits);
        request.replace_provider_target(provider);
        let client = OpenAiCompatClient::new(None);
        let mut sink = RecordingLlmEventSink::default();

        let error = client
            .stream_chat(request, CancellationToken::new(), &mut sink)
            .await
            .expect_err("Responses arguments must be bounded before projection");
        server.abort();

        assert!(matches!(
            error,
            LlmError::ProviderFailure { source, .. }
                if matches!(
                    *source,
                    LlmError::ProviderStreamLimitExceeded {
                        surface: ProviderStreamLimit::ToolCallArgumentBytes,
                        actual: 5,
                        maximum: 4,
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
            "output_index": 0,
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
            "provider generation failed for response resp_failed (server_error)",
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
                let failure = error.provider_failure().expect("typed incomplete failure");
                assert_eq!(failure.kind, ProviderFailureKind::Generation);
                assert_eq!(failure.status, None);
                assert_eq!(failure.code, None);
                assert!(matches!(
                    error.token_usage(),
                    Some(TokenUsage {
                        prompt_tokens: 10,
                        completion_tokens: 20,
                        total_tokens: 30,
                        reasoning_tokens: None,
                    })
                ));
                assert!(matches!(
                    sink.phases.last(),
                    Some(ProviderPhaseEvent {
                        phase: ProviderPhase::ProviderTerminal,
                        terminal_status: Some(ProviderTerminalStatus::Failed),
                        usage: Some(TokenUsage {
                            prompt_tokens: 10,
                            completion_tokens: 20,
                            total_tokens: 30,
                            reasoning_tokens: None,
                        }),
                        failure: Some(terminal_failure),
                        ..
                    }) if terminal_failure.kind == ProviderFailureKind::Generation
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
        status: StatusCode,
    }

    #[derive(Default)]
    struct RecordingLlmEventSink {
        events: Vec<LlmEvent>,
        phases: Vec<ProviderPhaseEvent>,
    }

    #[derive(Default)]
    struct FailingLlmEventSink {
        attempted_events: Vec<LlmEvent>,
        phases: Vec<ProviderPhaseEvent>,
        fail_push_on: Option<usize>,
        fail_phase: Option<ProviderPhase>,
    }

    impl FailingLlmEventSink {
        fn fail_push_on(attempt: usize) -> Self {
            Self {
                fail_push_on: Some(attempt),
                ..Self::default()
            }
        }

        fn fail_phase(phase: ProviderPhase) -> Self {
            Self {
                fail_phase: Some(phase),
                ..Self::default()
            }
        }
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

    impl LlmEventSink for FailingLlmEventSink {
        fn push(&mut self, event: LlmEvent) -> Result<(), crate::error::LlmError> {
            self.attempted_events.push(event);
            if self.fail_push_on == Some(self.attempted_events.len()) {
                return Err(LlmError::Message(
                    "fixture model-event projection failed".to_string(),
                ));
            }
            Ok(())
        }

        fn provider_phase(
            &mut self,
            event: ProviderPhaseEvent,
        ) -> Result<(), crate::error::LlmError> {
            let should_fail = self.fail_phase == Some(event.phase);
            self.phases.push(event);
            if should_fail {
                return Err(LlmError::Message(
                    "fixture provider-phase projection failed".to_string(),
                ));
            }
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
        start_delayed_fixture_with_status(response, delay, delay_before_headers, StatusCode::OK)
            .await
    }

    async fn start_delayed_fixture_with_status(
        response: String,
        delay: Duration,
        delay_before_headers: bool,
        status: StatusCode,
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let request_count = Arc::new(AtomicUsize::new(0));
        let state = DelayedFixtureState {
            request_count: request_count.clone(),
            response: Arc::new(response),
            delay,
            delay_before_headers,
            status,
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
                .status(state.status)
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
            .status(state.status)
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
